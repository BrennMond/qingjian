//! # Compiler
//!
//! 中文职责：把词条流**流式**编译成紧凑的二进制产物。
//! English role: stream entries into the compact binary artifact.
//! 架构位置：`qingjian-table` 的写侧；产物由 [`crate::TableLexicon`] 读。
//!
//! # 为什么要"流式"
//!
//! PLAN §3 给 P2.5 的验收标准里有两条直接约束这里：
//!
//! - **部署峰值 < 150 MB**（RIME 实测是 780 MB – 1 GB）
//! - **产物/源 < 3×**
//!
//! 旧的装载路径在一份 500k 词条的词库上实测**峰值 245 MiB**，
//! 外推到雾凇的 188 万条约 **0.9 GB**。原因很直白：同一批数据被搬了三遍——
//! `Vec<RawEntry>`（两个 `String`）→ `SchemeDef.entries`（`Vec<String>` + `String`）
//! → `InMemoryLexicon`（`BTreeMap` + 每键一个 `Vec`）。
//!
//! 这里改成**一次搬进紧凑的竞技场（arena）**：
//!
//! ```text
//! words:      Vec<u8>           11 MB   ← 所有词，顺序拼接
//! word_spans: Vec<(u32,u16)>    15 MB
//! code_units: Vec<u16>           9 MB   ← 所有编码单元，顺序拼接
//! code_spans: Vec<(u32,u8)>     15 MB
//! scores:     Vec<i32>         7.5 MB
//! order:      Vec<u32>         7.5 MB   ← 排序用
//! ────────────────────────────────────
//! 约 65 MB（188 万词条）—— 在 150 MB 预算之内
//! ```
//!
//! 排序仍在内存里做（外部归并排序是另一个量级的工作）。
//! **这是一个有意的取舍**：65 MB 换掉一个外部排序器，
//! 而预算还有一倍余量。真到了不够的那天再说——那时也知道确切数字。

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::format::{
    BodyChecksum, BuildFingerprint, TableHeader, ENTRY_SIZE, FORMAT_VERSION, HEADER_SIZE,
};

/// 临时文件名的计数器（同一进程内并发编译也不会撞名）。
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 编译错误。
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompileError {
    /// 写文件失败。
    Io(String),
    /// 词条引用了字母表之外的编码单元。
    ///
    /// **这是加载期的响亮失败**：字母表与词库不一致说明方案数据有错，
    /// 而不是"这个词查不到"。
    UnknownUnit {
        /// 出错的编码单元文本。
        unit: String,
        /// 属于哪个词。
        word: String,
    },
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(m) => write!(f, "写产物失败：{m}"),
            Self::UnknownUnit { unit, word } => write!(
                f,
                "词条「{word}」引用了字母表里没有的编码单元「{unit}」——\
                 方案数据不一致（字母表与词库必须来自同一个方案）"
            ),
        }
    }
}

impl std::error::Error for CompileError {}

impl From<std::io::Error> for CompileError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

/// 词条接收器：调用方往里"喂"词条，它负责紧凑地攒起来。
///
/// **它就是"流式"的接口**：调用方不需要先构造一个 `Vec<RawEntry>`，
/// 因此那份中间表示的内存开销被整段省掉。
pub struct TableWriter {
    words: Vec<u8>,
    word_spans: Vec<(u32, u16)>,
    code_units: Vec<u16>,
    code_spans: Vec<(u32, u8)>,
    scores: Vec<i32>,
    checksum: u64,
    fingerprint: BuildFingerprint,
}

impl TableWriter {
    /// 以给定的源数据校验和与产物指纹开始。
    ///
    /// - `source_checksum`：源词典 + import 链原始字节的 FNV-1a
    ///   （多份源文件用 [`crate::format::combine_checksums`] 合并）。
    /// - `fingerprint`：**产物身份**，必须包含格式版本、编译选项、
    ///   字母表内容与顺序（见 [`BuildFingerprint`]）。缓存命中判定用它，
    ///   而不是只用 `source_checksum`——后者不含字母表，会静默错码。
    #[must_use]
    pub fn new(source_checksum: u64, fingerprint: BuildFingerprint) -> Self {
        Self {
            words: Vec::new(),
            word_spans: Vec::new(),
            code_units: Vec::new(),
            code_spans: Vec::new(),
            scores: Vec::new(),
            checksum: source_checksum,
            fingerprint,
        }
    }

    /// 喂入一条词条。
    ///
    /// # Errors
    ///
    /// 编码长度超过 255 个单元、或词长超过 65535 字节时返回 [`CompileError::Io`]
    /// （这类输入本身就不合理，且会让定长字段溢出）。
    pub fn push(&mut self, word: &str, code: &[u16], weight: f64) -> Result<(), CompileError> {
        let wb = word.as_bytes();
        if wb.len() > u16::MAX as usize {
            return Err(CompileError::Io(format!("词「{word}」超过 65535 字节")));
        }
        if code.len() > u8::MAX as usize {
            return Err(CompileError::Io(format!(
                "词「{word}」的编码超过 255 个单元"
            )));
        }
        if self.words.len() > u32::MAX as usize {
            return Err(CompileError::Io("词字符串表超过 4 GB".into()));
        }
        if self.code_units.len() > u32::MAX as usize {
            return Err(CompileError::Io("编码单元表超过 4 GB".into()));
        }

        #[allow(clippy::cast_possible_truncation)]
        let word_off = self.words.len() as u32;
        self.words.extend_from_slice(wb);
        // 上面的长度检查保证了这里不会截断。
        #[allow(clippy::cast_possible_truncation)]
        let word_len = wb.len() as u16;
        self.word_spans.push((word_off, word_len));

        #[allow(clippy::cast_possible_truncation)]
        let code_off = self.code_units.len() as u32;
        self.code_units.extend_from_slice(code);
        #[allow(clippy::cast_possible_truncation)]
        let code_len = code.len() as u8;
        self.code_spans.push((code_off, code_len));

        self.scores.push(score_of(weight));
        // **校验和由调用方从源文件的原始字节算出，写入器不再改动它。**
        //
        // 为什么不让写入器把词条内容混进去：那样"产物是否对应这份源"就再也
        // 无法从源文件**独立重算**——而重新部署时正是要拿源文件的校验和来比对。
        Ok(())
    }

    /// 已经喂入多少条。
    #[must_use]
    pub fn len(&self) -> usize {
        self.scores.len()
    }

    /// 是否还没有任何词条。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.scores.is_empty()
    }

    /// 排序并写出产物。
    ///
    /// # Errors
    ///
    /// 写文件失败时返回 [`CompileError::Io`]。
    pub fn finish(self, out_path: &Path) -> Result<CompiledTable, CompileError> {
        let n = self.scores.len();

        // ── 排序 ──
        //
        // 按 (编码, -分数) 排。**编码升序**是为了二分查找；
        // **同码内分数降序**是为了"高权重的词先被读到"——
        // 于是限流读前 K 条时就自然拿到了分数最高的 K 条。
        let mut order: Vec<u32> = (0..u32::try_from(n).unwrap_or(u32::MAX)).collect();
        {
            let (units, spans, scores) = (&self.code_units, &self.code_spans, &self.scores);
            let code_of = |i: u32| -> &[u16] {
                let (off, len) = spans[i as usize];
                &units[off as usize..off as usize + len as usize]
            };
            order.sort_by(|&a, &b| {
                code_of(a)
                    .cmp(code_of(b))
                    .then_with(|| scores[b as usize].cmp(&scores[a as usize]))
                    .then_with(|| a.cmp(&b)) // 最后的平局键：保证确定性
            });
        }

        // ── 建索引（前缀和）──
        let mut unit_offsets: Vec<u32> = Vec::with_capacity(n + 1);
        let mut entry_offsets: Vec<u32> = Vec::with_capacity(n + 1);
        let mut codes: Vec<u16> = Vec::with_capacity(self.code_units.len());
        unit_offsets.push(0);
        entry_offsets.push(0);

        let mut prev: Option<&[u16]> = None;
        for (pos, &idx) in order.iter().enumerate() {
            let (coff, clen) = self.code_spans[idx as usize];
            let code = &self.code_units[coff as usize..coff as usize + clen as usize];
            if prev != Some(code) {
                // 新编码的开始位置。**第一个编码不推**——`entry_offsets` 已经
                // 以 0 开头，那个 0 就是它的起点；再推一次会多出一个哨兵，
                // 于是 `code_count` 永远多 1。
                if prev.is_some() {
                    #[allow(clippy::cast_possible_truncation)]
                    entry_offsets.push(pos as u32);
                }
                codes.extend_from_slice(code);
                #[allow(clippy::cast_possible_truncation)]
                unit_offsets.push(codes.len() as u32);
                prev = Some(code);
            }
        }
        #[allow(clippy::cast_possible_truncation)]
        let total = n as u32;
        if n > 0 {
            // 收尾哨兵：最后一个编码的区间右端。
            entry_offsets.push(total);
        }
        // 空词库时 `entry_offsets` 只留那个 0 —— 于是 code_count = 0。

        let code_count = u32::try_from(entry_offsets.len() - 1).unwrap_or(0);

        // ── 布局 ──
        #[allow(clippy::cast_possible_truncation)]
        let word_bytes = self.words.len() as u32;
        #[allow(clippy::cast_possible_truncation)]
        let total_units = codes.len() as u32;

        let words_offset = u64::from(HEADER_SIZE);
        let entries_offset = words_offset + u64::from(word_bytes);
        let index_offset = entries_offset + (n as u64) * ENTRY_SIZE as u64;

        // ── 词表：按**排序后的顺序**重排 ──
        //
        // 于是同一编码的词条在文件里是连续的，读侧一次 `read_at` 就能拿到
        // 某个编码的全部词——这是"两次系统调用完成一次查询"这个承诺的关键。
        let mut sorted_word_offsets: Vec<u32> = Vec::with_capacity(n);
        let mut new_words: Vec<u8> = Vec::with_capacity(self.words.len());
        for &idx in &order {
            let (off, len) = self.word_spans[idx as usize];
            #[allow(clippy::cast_possible_truncation)]
            sorted_word_offsets.push(new_words.len() as u32);
            new_words.extend_from_slice(&self.words[off as usize..off as usize + len as usize]);
        }

        // ── 词条数组 ──
        let mut entry_buf = Vec::with_capacity(n * ENTRY_SIZE);
        for (pos, &idx) in order.iter().enumerate() {
            let (_, len) = self.word_spans[idx as usize];
            entry_buf.extend_from_slice(&sorted_word_offsets[pos].to_le_bytes());
            entry_buf.extend_from_slice(&len.to_le_bytes());
            entry_buf.extend_from_slice(&0u16.to_le_bytes()); // flags（留给将来）
            entry_buf.extend_from_slice(&self.scores[idx as usize].to_le_bytes());
        }

        let header = TableHeader {
            format_version: FORMAT_VERSION,
            source_checksum: self.checksum,
            entry_count: total,
            code_count,
            word_bytes,
            total_units,
            words_offset,
            entries_offset,
            index_offset,
            build_fingerprint: self.fingerprint.as_u64(),
            // 先占位，等主体字节都算完再回填（就在下面几行）。
            body_checksum: 0,
        };

        // ── 索引三段（先在内存里成形：既要写文件，也要算校验和）──
        let mut index_buf: Vec<u8> = Vec::with_capacity(unit_offsets.len() * 8 + codes.len() * 2);
        for v in &unit_offsets {
            index_buf.extend_from_slice(&v.to_le_bytes());
        }
        for v in &entry_offsets {
            index_buf.extend_from_slice(&v.to_le_bytes());
        }
        for v in &codes {
            index_buf.extend_from_slice(&v.to_le_bytes());
        }
        debug_assert_eq!(index_buf.len() as u64, header.index_size());

        // ── 主体校验和：**严格按文件里的字节顺序**喂 ──
        let mut body = BodyChecksum::new();
        body.update(&new_words);
        body.update(&entry_buf);
        body.update(&index_buf);
        let mut header = header;
        header.body_checksum = body.finish();

        // ── 写文件（临时文件 + 原子改名）──
        //
        // 为什么要原子发布：缓存目录里**绝不能出现"半个产物"**。
        // 直接 `File::create(out_path)` 会在写第一行之前就把旧产物截断，
        // 中途失败就留下一个头部声明 0 字节、主体缺失的文件。先写同目录的
        // 唯一临时文件、`sync_all` 之后再 `rename`，读者要么看到旧产物、
        // 要么看到完整的新产物。这是"缓存不可见半成品"的实现方式。
        if let Some(dir) = out_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = tmp_path(out_path);
        {
            let file = std::fs::File::create(&tmp)?;
            let mut w = BufWriter::with_capacity(1 << 20, file);
            w.write_all(&header.to_bytes())?;
            w.write_all(&new_words)?;
            w.write_all(&entry_buf)?;
            w.write_all(&index_buf)?;
            w.flush()?;
            let file = w
                .into_inner()
                .map_err(|e| CompileError::Io(e.to_string()))?;
            // 数据先落盘，再让它以最终名字出现。
            file.sync_all()?;
        }
        if let Err(e) = std::fs::rename(&tmp, out_path) {
            // 改名失败时**不留垃圾**：清掉临时文件再报错。
            let _ = std::fs::remove_file(&tmp);
            return Err(CompileError::Io(e.to_string()));
        }

        Ok(CompiledTable {
            header,
            path: out_path.to_path_buf(),
        })
    }
}

/// 与目标同目录的唯一临时文件名。
///
/// **必须同目录**：跨文件系统的 `rename` 会退化成"复制 + 删除"，
/// 那就不再是原子的。名字里带 pid 与计数器，避免并发编译互踩
/// （固定的 `.tmp` 后缀会让两个进程写同一个文件）。
fn tmp_path(out_path: &Path) -> PathBuf {
    let mut name = out_path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(
        ".tmp.{}.{}",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    out_path.with_file_name(name)
}

/// 权重 → 对数域定点分数。
///
/// **换算只发生在这里（装载期）**，运行期只做整数加法与比较——
/// 这正是"可复现"能跨平台成立的原因（PLAN D13）。
fn score_of(weight: f64) -> i32 {
    qingjian_core::Score::from_weight(weight).as_milli_log()
}

/// 一份编译好的产物。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledTable {
    /// 文件头。
    pub header: TableHeader,
    /// 产物路径。
    pub path: PathBuf,
}

impl CompiledTable {
    /// 产物的字节数。
    #[must_use]
    pub fn size_bytes(&self) -> u64 {
        self.header.file_size()
    }

    /// 源数据校验和。
    #[must_use]
    pub fn checksum(&self) -> u64 {
        self.header.source_checksum
    }
}

/// 便捷入口：一次性编译。
///
/// `feed` 会被调用一次，往 [`TableWriter`] 里喂词条。**流式**——
/// 调用方不必先攒一个 `Vec`。
///
/// # Errors
///
/// 喂入时出错或写文件失败时返回 [`CompileError`]。
pub fn compile<F>(
    source_checksum: u64,
    fingerprint: BuildFingerprint,
    feed: F,
    out_path: &Path,
) -> Result<CompiledTable, CompileError>
where
    F: FnOnce(&mut TableWriter) -> Result<(), CompileError>,
{
    let mut w = TableWriter::new(source_checksum, fingerprint);
    feed(&mut w)?;
    w.finish(out_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("qingjian-table-test-{name}"));
        let _ = std::fs::remove_file(&p);
        p
    }

    /// 测试用的固定指纹（内容不重要，稳定性重要）。
    fn fp() -> BuildFingerprint {
        BuildFingerprint::of(FORMAT_VERSION, "test", &[], 0)
    }

    #[test]
    fn compiles_and_reports_a_sane_header() {
        let path = tmp("basic");
        let t = compile(
            42,
            fp(),
            |w| {
                w.push("你好", &[0, 1], 100.0)?;
                w.push("世界", &[2, 3], 50.0)?;
                w.push("你", &[0], 200.0)?;
                Ok(())
            },
            &path,
        )
        .unwrap();

        assert_eq!(t.header.entry_count, 3);
        assert_eq!(t.header.code_count, 3);
        assert_eq!(t.checksum(), 42);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), t.size_bytes());
        assert_eq!(t.header.build_fingerprint, fp().as_u64());
        // 主体的校验和必须真的算过（非 0 且能被读侧独立复算）。
        assert!(crate::format::validate_layout(&t.header, t.size_bytes()).is_ok());
    }

    #[test]
    fn writing_leaves_no_temp_file_behind() {
        let path = tmp("notmp");
        compile(0, fp(), |_w| Ok(()), &path).unwrap();
        let dir = path.parent().unwrap();
        let leftovers: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("qingjian-table-test-notmp") && n.contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "留下了临时文件：{leftovers:?}");
    }

    #[test]
    fn same_code_entries_include_every_one() {
        let path = tmp("samecode");
        let t = compile(
            0,
            fp(),
            |w| {
                w.push("低", &[7], 1.0)?;
                w.push("高", &[7], 100.0)?;
                w.push("中", &[7], 50.0)?;
                Ok(())
            },
            &path,
        )
        .unwrap();
        // 三条同码 → 一个编码、三条词条。
        assert_eq!(t.header.code_count, 1);
        assert_eq!(t.header.entry_count, 3);
    }

    #[test]
    fn empty_dictionary_compiles() {
        let path = tmp("empty");
        let t = compile(0, fp(), |_w| Ok(()), &path).unwrap();
        assert_eq!(t.header.entry_count, 0);
        assert_eq!(t.header.code_count, 0);
        assert!(t.size_bytes() > 0);
    }

    #[test]
    fn oversized_input_is_rejected_with_a_reason() {
        let path = tmp("oversize");
        let long_code: Vec<u16> = (0..300).collect();
        let e = compile(0, fp(), |w| w.push("x", &long_code, 1.0), &path).unwrap_err();
        assert!(e.to_string().contains("255"), "{e}");
    }

    #[test]
    fn product_is_smaller_than_a_naive_estimate() {
        // 1000 条 3 字词：源（词+编码+权重文本）约 1000×20 = 20 KB；
        // 产物应当远小于 3× 源。
        let path = tmp("ratio");
        let t = compile(
            0,
            fp(),
            |w| {
                for i in 0..1000u32 {
                    w.push(&format!("词{i}"), &[1, 2, 3], 100.0)?;
                }
                Ok(())
            },
            &path,
        )
        .unwrap();
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size < 30_000, "产物应当紧凑，实得 {size} 字节");
        assert_eq!(t.header.entry_count, 1000);
    }
}
