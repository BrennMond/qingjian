//! # `TableLexicon` — 按需分页的词库
//!
//! 中文职责：`stele_core::Lexicon` 的第二个实现——**索引常驻、词条按需读**。
//! English role: the second `Lexicon` implementation — resident index,
//! on-demand entry reads.
//! 架构位置：与 `stele_engine::InMemoryLexicon` **平级**。
//!
//! # 这个文件是"抽象是否成立"的验收
//!
//! `docs/engine-design.md` §5 说过：「`Lexicon` 的内存实现 → mmap 实现，
//! **引擎代码一行不用改**」。
//!
//! **这句话在本文件出现之前，一直只是承诺。** 现在它是可验证的：
//! 引擎只持有 `Arc<dyn Lexicon>`，换实现不碰引擎一行。
//! 如果当初把"怎么存词条"漏进了引擎，这里就会被迫改引擎——
//! 那样的信号比任何架构评审都可靠。
//!
//! # 内存账
//!
//! 常驻的是**索引**：`unit_offsets` + `entry_offsets` + `units`。
//! 对 188 万词条约 **4 MB**。词字符串表与词条数组留在文件里。
//!
//! # 一次查询两次系统调用
//!
//! 1. 二分查找索引（**纯内存**，无 I/O）
//! 2. `read_at` 读该编码的词条区间（≤ 上限条）
//! 3. `read_at` 读这些词条的词字节（它们连续，所以一次就够）
//!
//! 每次 `read_at` 约 1 µs，两次约 2 µs——相对 1 ms 的按键预算可以忽略。

use std::fs::File;
use std::path::Path;

use stele_core::{
    Candidate, CandidateSink, CodeUnitId, Lane, Lexicon, Origin, Score, Span, SpellingAttr,
};

use crate::format::{
    read_exact_at, validate_layout, BodyChecksum, BuildFingerprint, FormatError, TableHeader,
    ENTRY_SIZE, HEADER_SIZE,
};

/// 单个编码最多读多少条词条。
///
/// 一个高频音节（例如 `shi`）可能有几百个同码词。上限既是内存保护，
/// 也是延迟保护——而且**因为产物里同码词条按分数降序存放**，
/// 截断丢掉的一定是分数最低的那些，**这正是我们可以接受截断的原因**。
pub const MAX_ENTRIES_PER_CODE: usize = 256;

/// 从编译产物读词库。
#[derive(Debug)]
pub struct TableLexicon {
    file: File,
    header: TableHeader,
    /// 每个编码在 `units` 里的起始下标（前缀和，长度 = `code_count + 1`）。
    unit_offsets: Vec<u32>,
    /// 每个编码在词条数组里的起始下标（前缀和，长度 = `code_count + 1`）。
    entry_offsets: Vec<u32>,
    /// 所有编码的编码单元，顺序拼接。
    units: Vec<u16>,
}

impl TableLexicon {
    /// 打开一份产物。
    ///
    /// # Errors
    ///
    /// 文件不存在、魔数不对、版本不认识、长度与头部不符时返回 [`FormatError`]。
    pub fn open(path: &Path) -> Result<Self, FormatError> {
        Self::open_checked(path, None)
    }

    /// 打开并**校验产物身份**（PLAN D28 / P0-B）。
    ///
    /// # Errors
    ///
    /// 除 [`TableLexicon::open`] 的错误外：
    ///
    /// - 指纹不匹配时返回 [`FormatError::FingerprintMismatch`]——**拒绝加载**。
    ///   产物的编码是**字母表下标**，复用一份用别的字母表编译的产物会
    ///   静默给出错误的字（P0-B 的复现）。用错版本的产物会静默给出错误结果，
    ///   那比加载失败糟糕得多。
    /// - 结构损坏时返回 [`FormatError::Corrupt`]，主体校验和不符时返回
    ///   [`FormatError::BodyChecksumMismatch`]。**任何畸形输入都只能是
    ///   可读的拒绝，绝不能 panic。**
    pub fn open_checked(
        path: &Path,
        expected: Option<BuildFingerprint>,
    ) -> Result<Self, FormatError> {
        let file = File::open(path)?;
        let actual = file.metadata()?.len();

        // 先查长度：比头部还短的文件连头部都放不下，
        // 直接说"这不是本格式"比报"读文件失败"有用得多。
        if actual < u64::from(HEADER_SIZE) {
            return Err(FormatError::Truncated {
                expected: u64::from(HEADER_SIZE),
                actual,
            });
        }

        let mut hb = [0u8; HEADER_SIZE as usize];
        read_exact_at(&file, &mut hb, 0)?;
        let header = TableHeader::from_bytes(&hb)?;

        // ① 身份：最便宜的一步，而且它决定"要不要重建"。
        if let Some(exp) = expected {
            if exp.as_u64() != header.build_fingerprint {
                return Err(FormatError::FingerprintMismatch {
                    expected: exp.as_u64(),
                    found: header.build_fingerprint,
                });
            }
        }

        // ② 布局：**必须在任何"用文件里的数字索引内存"之前**。
        //    这一步同时挡住溢出、越界与区段不衔接。
        validate_layout(&header, actual)?;

        // ③ 内容完整性：意外损坏检测（不是防篡改，见模块文档）。
        let found = body_checksum_of(&file, &header)?;
        if found != header.body_checksum {
            return Err(FormatError::BodyChecksumMismatch {
                expected: header.body_checksum,
                found,
            });
        }

        // ── 把索引读进内存（这是唯一常驻的部分）──
        let code_count = header.code_count as usize;
        #[allow(clippy::cast_possible_truncation)]
        let index_size = header.checked_index_size().unwrap_or(0) as usize;
        let mut raw = vec![0u8; index_size];
        read_exact_at(&file, &mut raw, header.index_offset)?;

        let mut unit_offsets = Vec::with_capacity(code_count + 1);
        let mut entry_offsets = Vec::with_capacity(code_count + 1);
        let mut units = Vec::with_capacity(header.total_units as usize);
        let mut p = 0usize;
        for _ in 0..=code_count {
            unit_offsets.push(u32_at(&raw, p)?);
            p += 4;
        }
        for _ in 0..=code_count {
            entry_offsets.push(u32_at(&raw, p)?);
            p += 4;
        }
        for _ in 0..header.total_units {
            units.push(u16_at(&raw, p)?);
            p += 2;
        }

        // ── 前缀和数组的**结构性**校验 ──
        //
        // 这些值是文件里的数字，读侧拿它们做切片下标。少验一条，
        // 篡改一个值就能让 release 进程在切片处 panic（P0-C 的复现）。
        check_offset_table(&unit_offsets, header.total_units as usize, "编码单元")?;
        check_offset_table(&entry_offsets, header.entry_count as usize, "词条")?;

        Ok(Self {
            file,
            header,
            unit_offsets,
            entry_offsets,
            units,
        })
    }

    /// 文件头。
    #[must_use]
    pub fn header(&self) -> &TableHeader {
        &self.header
    }

    /// **常驻内存的字节数**（索引部分）。
    ///
    /// 称重台用它回答"换个实现到底省了多少"——而不是靠估算。
    #[must_use]
    pub fn resident_index_bytes(&self) -> usize {
        (self.unit_offsets.len() + self.entry_offsets.len()) * 4 + self.units.len() * 2
    }

    /// 文件的字节数。
    #[must_use]
    pub fn file_bytes(&self) -> u64 {
        self.header.file_size()
    }

    /// 编码在索引里的位置。
    ///
    /// **全程 `get`**：`open_checked` 已经保证了下标合法，但这条路径是
    /// 按键路径，一个越界就是一个进程级 panic。多一次边界检查换"永不崩"，
    /// 在输入法里是划算的。
    fn find_code(&self, code: &[u16]) -> Option<usize> {
        let n = self.entry_offsets.len().checked_sub(1)?;
        let code_at = |i: usize| -> Option<&[u16]> {
            let a = *self.unit_offsets.get(i)? as usize;
            let b = *self.unit_offsets.get(i + 1)? as usize;
            self.units.get(a..b)
        };
        // 手写二分：`units` 是拼接的，无法直接用 `binary_search` 的切片接口。
        let (mut lo, mut hi) = (0usize, n);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match code_at(mid)?.cmp(code) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }

    /// 读某个编码的词条（含词字节），一次到位。
    ///
    /// 返回 `(词, 分数)` 列表。
    ///
    /// # 为什么这里每一步都要 checked
    ///
    /// 词条记录里的 `word_off` / `word_len` 也是**文件里的数字**。
    /// `off - first` 这种减法在畸形产物上会下溢，随后的切片就会 panic。
    /// 因此：记录先逐条验证落在 `word_bytes` 之内，再取**最小值**当基准
    /// （不是第一条——记录未必按偏移有序），最后一律用 `get` 切片。
    fn read_entries(&self, idx: usize, limit: usize) -> Result<Vec<(String, i32)>, FormatError> {
        let start = u64::from(
            *self
                .entry_offsets
                .get(idx)
                .ok_or_else(|| FormatError::Corrupt("词条索引下标越界".into()))?,
        );
        let end = u64::from(
            *self
                .entry_offsets
                .get(idx + 1)
                .ok_or_else(|| FormatError::Corrupt("词条索引下标越界".into()))?,
        );
        if end < start {
            return Err(FormatError::Corrupt("词条索引非单调".into()));
        }
        if end > u64::from(self.header.entry_count) {
            return Err(FormatError::Corrupt("词条索引超过词条总数".into()));
        }
        #[allow(clippy::cast_possible_truncation)]
        let count = usize::try_from(end - start)
            .map_err(|_| FormatError::Corrupt("词条数超过本平台可寻址范围".into()))?
            .min(limit);
        if count == 0 {
            return Ok(Vec::new());
        }

        let entry_base = self
            .header
            .entries_offset
            .checked_add(
                start
                    .checked_mul(ENTRY_SIZE as u64)
                    .ok_or_else(|| FormatError::Corrupt("词条读取偏移溢出".into()))?,
            )
            .ok_or_else(|| FormatError::Corrupt("词条读取偏移溢出".into()))?;

        let mut buf = vec![0u8; count * ENTRY_SIZE];
        read_exact_at(&self.file, &mut buf, entry_base)?;

        let word_bytes = u64::from(self.header.word_bytes);
        let mut recs: Vec<(u32, u16, i32)> = Vec::with_capacity(count);
        for i in 0..count {
            let r = buf
                .get(i * ENTRY_SIZE..(i + 1) * ENTRY_SIZE)
                .ok_or_else(|| FormatError::Corrupt("词条区被截断".into()))?;
            let off = u32::from_le_bytes(r[0..4].try_into().unwrap_or([0; 4]));
            let len = u16::from_le_bytes(r[4..6].try_into().unwrap_or([0; 2]));
            let score = i32::from_le_bytes(r[8..12].try_into().unwrap_or([0; 4]));
            // **记录自校验**：词必须落在词字符串表之内。
            if u64::from(off) + u64::from(len) > word_bytes {
                return Err(FormatError::Corrupt(format!(
                    "词条记录指向词表之外（off={off}, len={len}, 词表 {word_bytes} 字节）"
                )));
            }
            recs.push((off, len, score));
        }

        // 词字节：因为写产物时按排序后的顺序重排过词表，
        // 所以这几条词条的词在文件里是**连续**的 —— 一次 read_at 拿全。
        let first = recs.iter().map(|r| r.0).min().unwrap_or(0);
        let last_end = recs
            .iter()
            .map(|(off, len, _)| u64::from(*off) + u64::from(*len))
            .max()
            .unwrap_or(0);
        let span = usize::try_from(last_end - u64::from(first))
            .map_err(|_| FormatError::Corrupt("词表区间超过本平台可寻址范围".into()))?;
        let mut words = vec![0u8; span];
        read_exact_at(
            &self.file,
            &mut words,
            self.header
                .words_offset
                .checked_add(u64::from(first))
                .ok_or_else(|| FormatError::Corrupt("词表读取偏移溢出".into()))?,
        )?;

        let mut out = Vec::with_capacity(count);
        for (off, len, score) in recs {
            // `first` 是最小值 ⇒ 这两个减法都不会下溢。
            let a = (off - first) as usize;
            let b = a + usize::from(len);
            let s = words
                .get(a..b)
                .map(|w| String::from_utf8_lossy(w).into_owned())
                .ok_or_else(|| FormatError::Corrupt("词条越出读入的词表区间".into()))?;
            out.push((s, score));
        }
        Ok(out)
    }
}

/// 从前缀和数组里取一个 `u32`，越界就报损坏。
fn u32_at(raw: &[u8], p: usize) -> Result<u32, FormatError> {
    raw.get(p..p + 4)
        .and_then(|s| <[u8; 4]>::try_from(s).ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| FormatError::Corrupt("索引区被截断".into()))
}

/// 从前缀和数组里取一个 `u16`，越界就报损坏。
fn u16_at(raw: &[u8], p: usize) -> Result<u16, FormatError> {
    raw.get(p..p + 2)
        .and_then(|s| <[u8; 2]>::try_from(s).ok())
        .map(u16::from_le_bytes)
        .ok_or_else(|| FormatError::Corrupt("索引区被截断".into()))
}

/// 校验一张**前缀和偏移表**：以 0 开头、以 `sentinel` 结尾、单调不减、
/// 且每一项都不超过 `sentinel`。
///
/// 这四条合起来等价于"每一项都能安全地当切片下标用"——少一条就有
/// panic 的口子。
fn check_offset_table(offsets: &[u32], sentinel: usize, what: &str) -> Result<(), FormatError> {
    if offsets.first().copied() != Some(0) {
        return Err(FormatError::Corrupt(format!("{what}索引没有以 0 开头")));
    }
    if offsets.last().copied().map(|v| v as usize) != Some(sentinel) {
        return Err(FormatError::Corrupt(format!(
            "{what}索引末项与总数不符（末项 {:?}，总数 {sentinel}）",
            offsets.last()
        )));
    }
    for w in offsets.windows(2) {
        if w[1] < w[0] {
            return Err(FormatError::Corrupt(format!(
                "{what}索引非单调（{} → {}）",
                w[0], w[1]
            )));
        }
    }
    if offsets.iter().any(|v| *v as usize > sentinel) {
        return Err(FormatError::Corrupt(format!("{what}索引里有超过总数的值")));
    }
    Ok(())
}

/// 分块读产物主体并算 FNV-1a。
///
/// **分块**是为了不给"读一个 150 MB 的产物"再要一块 150 MB 的堆：
/// 缓存加载不该把常驻内存推过红线。
fn body_checksum_of(file: &File, header: &TableHeader) -> Result<u64, FormatError> {
    let Some(end) = header.checked_file_size() else {
        return Err(FormatError::Corrupt("文件长度溢出".into()));
    };
    let mut ck = BodyChecksum::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut pos = u64::from(HEADER_SIZE);
    while pos < end {
        // 每块最多 `buf.len()`；`min` 保证不会超过它，因此 `try_from` 必定成功。
        let n = usize::try_from((end - pos).min(buf.len() as u64)).unwrap_or(buf.len());
        read_exact_at(file, &mut buf[..n], pos)?;
        ck.update(&buf[..n]);
        pos += n as u64;
    }
    Ok(ck.finish())
}

impl Lexicon for TableLexicon {
    fn lookup(&self, code: &[CodeUnitId], out: &mut CandidateSink<'_>) {
        // 编码单元在产物里是 u16（`CodeUnitId` 是 u32，但方案规模远达不到 65536 个单元）。
        let mut narrow: Vec<u16> = Vec::with_capacity(code.len());
        for u in code {
            match u16::try_from(u.0) {
                Ok(v) => narrow.push(v),
                Err(_) => return, // 超出产物能表达的编码单元范围 → 查不到
            }
        }

        let Some(idx) = self.find_code(&narrow) else {
            return;
        };
        let limit = out.remaining().clamp(1, MAX_ENTRIES_PER_CODE);
        let Ok(entries) = self.read_entries(idx, limit) else {
            // 运行期**不可失败**：读失败就当查不到。
            // （文件被删/被截断属于部署问题，会在装载期被 D28 的校验挡住。）
            return;
        };

        let span = Span::new(0, narrow.len());
        for (text, score) in entries {
            out.push(Candidate {
                text,
                comment: None,
                score: Score::from_milli_log(score),
                origin: Origin::SystemWord,
                attr: SpellingAttr::NORMAL,
                span,
                lane: Lane::Input,
                kind: stele_core::CandidateKind::Normal,
                key: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::compile;
    use crate::format::{BuildFingerprint, COMPILER_OPTIONS, FORMAT_VERSION, HEADER_SIZE};

    fn fp() -> BuildFingerprint {
        BuildFingerprint::of(FORMAT_VERSION, COMPILER_OPTIONS, &[], 7)
    }

    fn build(name: &str, entries: &[(&str, &[u16], f64)]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("stele-lex-{name}.table"));
        let _ = std::fs::remove_file(&p);
        compile(
            7,
            fp(),
            |w| {
                for (word, code, weight) in entries {
                    w.push(word, code, *weight)?;
                }
                Ok(())
            },
            &p,
        )
        .unwrap();
        p
    }

    fn lookup(l: &TableLexicon, code: &[u16]) -> Vec<String> {
        let ids: Vec<CodeUnitId> = code.iter().map(|c| CodeUnitId(u32::from(*c))).collect();
        let mut buf = Vec::new();
        let mut sink = CandidateSink::new(&mut buf, 64);
        l.lookup(&ids, &mut sink);
        buf.into_iter().map(|c| c.text).collect()
    }

    #[test]
    fn reads_back_what_was_written() {
        let p = build(
            "rt",
            &[
                ("你好", &[0, 1], 100.0),
                ("世界", &[2, 3], 50.0),
                ("你", &[0], 200.0),
            ],
        );
        let l = TableLexicon::open(&p).unwrap();
        assert_eq!(lookup(&l, &[0, 1]), ["你好"]);
        assert_eq!(lookup(&l, &[2, 3]), ["世界"]);
        assert_eq!(lookup(&l, &[0]), ["你"]);
        assert!(lookup(&l, &[9, 9]).is_empty());
    }

    #[test]
    fn same_code_returns_highest_score_first() {
        let p = build(
            "order",
            &[("低", &[7], 1.0), ("高", &[7], 100.0), ("中", &[7], 50.0)],
        );
        let l = TableLexicon::open(&p).unwrap();
        assert_eq!(lookup(&l, &[7]), ["高", "中", "低"]);
    }

    #[test]
    fn truncation_keeps_the_best_ones() {
        let p = std::env::temp_dir().join("stele-lex-trunc.table");
        let _ = std::fs::remove_file(&p);
        compile(
            0,
            fp(),
            |w| {
                for i in 0..1000u32 {
                    w.push(&format!("w{i}"), &[1], f64::from(i) + 1.0)?;
                }
                Ok(())
            },
            &p,
        )
        .unwrap();
        let l = TableLexicon::open(&p).unwrap();
        let ids = [CodeUnitId(1)];
        let mut buf = Vec::new();
        let mut sink = CandidateSink::new(&mut buf, 8);
        l.lookup(&ids, &mut sink);
        assert_eq!(buf.len(), 8);
        // 因为产物里同码按分数降序存放，截断丢掉的必是最低分的。
        assert_eq!(buf[0].text, "w999");
    }

    #[test]
    fn resident_index_is_small_compared_to_the_file() {
        let p = std::env::temp_dir().join("stele-lex-size.table");
        let _ = std::fs::remove_file(&p);
        let words: Vec<String> = (0..20_000u32).map(|i| format!("词汇{i}")).collect();
        compile(
            0,
            fp(),
            |w| {
                for (i, word) in words.iter().enumerate() {
                    #[allow(clippy::cast_possible_truncation)]
                    let code = [((i % 300) as u16), 9];
                    w.push(word, &code, 100.0)?;
                }
                Ok(())
            },
            &p,
        )
        .unwrap();
        let l = TableLexicon::open(&p).unwrap();
        #[allow(clippy::cast_possible_truncation)]
        let file_bytes = l.file_bytes() as usize;
        assert!(
            l.resident_index_bytes() < file_bytes,
            "常驻索引（{} 字节）应当远小于文件（{} 字节）",
            l.resident_index_bytes(),
            file_bytes
        );
        assert_eq!(l.header().entry_count, 20_000);
    }

    #[test]
    fn fingerprint_mismatch_is_refused_not_tolerated() {
        let p = build("ck", &[("甲", &[1], 1.0)]);
        // 正确的指纹可以打开。
        assert!(TableLexicon::open_checked(&p, Some(fp())).is_ok());
        // 不匹配 → **拒绝加载**，而不是凑合跑。
        let other = BuildFingerprint::of(FORMAT_VERSION, COMPILER_OPTIONS, &[], 999);
        let e = TableLexicon::open_checked(&p, Some(other)).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("身份指纹不匹配"), "{msg}");
        assert!(msg.contains("重新部署"), "{msg}");
        assert!(
            msg.contains("字母表"),
            "诊断必须说清为什么会静默错码：{msg}"
        );
    }

    #[test]
    fn a_non_table_file_is_refused_with_an_explanation() {
        let p = std::env::temp_dir().join("stele-lex-junk.table");
        // 长度必须够放下头部，否则会先被"截断"挡下来 —— 那样测的就不是魔数了。
        std::fs::write(&p, vec![b'x'; 128]).unwrap();
        let e = TableLexicon::open(&p).unwrap_err();
        assert!(e.to_string().contains("魔数"), "{e}");
    }

    #[test]
    fn empty_table_looks_up_nothing() {
        let p = std::env::temp_dir().join("stele-lex-empty.table");
        let _ = std::fs::remove_file(&p);
        compile(0, fp(), |_w| Ok(()), &p).unwrap();
        let l = TableLexicon::open(&p).unwrap();
        assert!(lookup(&l, &[1]).is_empty());
    }

    // ─────────────────────────────────────────────────────────────────
    // P0-C：畸形产物
    //
    // 审计的复现是：只改一个内部 `unit_offsets`，保持文件长度、魔数、
    // 格式版本不变，装载成功，查询时在切片范围处 panic。
    // 下面这一组是**翻转 / 越界 / 溢出**回归——每一条都必须得到
    // 一个可读的 `FormatError`，绝不允许 panic。
    // ─────────────────────────────────────────────────────────────────

    /// 造一份内容足够丰富的产物，供畸形化使用。
    fn canary(name: &str) -> Vec<u8> {
        let p = build(
            name,
            &[
                ("你好", &[0, 1], 100.0),
                ("世界", &[2, 3], 50.0),
                ("你", &[0], 200.0),
                ("好", &[1], 300.0),
                ("好你", &[1, 0], 10.0),
            ],
        );
        std::fs::read(&p).unwrap()
    }

    fn write_bytes(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("stele-lex-mal-{name}.table"));
        let _ = std::fs::remove_file(&p);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    /// 打开一份畸形产物：只允许返回错误，**不许 panic**。
    fn refuse(bytes: &[u8], name: &str) -> FormatError {
        let p = write_bytes(name, bytes);
        let e = TableLexicon::open_checked(&p, Some(fp()))
            .err()
            .unwrap_or_else(|| panic!("畸形产物 `{name}` 竟然装载成功了"));
        let _ = std::fs::remove_file(&p);
        e
    }

    /// 装载后必须还能安全查询（不能 panic）。
    fn refuse_or_query_safely(bytes: &[u8], name: &str) {
        let p = write_bytes(name, bytes);
        if let Ok(l) = TableLexicon::open_checked(&p, Some(fp())) {
            // 装载成功也无妨——但查询必须安全。
            let probes: [&[u16]; 6] = [&[0, 1], &[2, 3], &[0], &[1], &[9, 9], &[0, 1, 2, 3]];
            for code in probes {
                let _ = lookup(&l, code);
            }
        }
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn tampered_unit_offset_is_refused() {
        let mut b = canary("tamper-unit");
        let header =
            TableHeader::from_bytes(&b[..HEADER_SIZE as usize].try_into().unwrap()).unwrap();
        let at = usize::try_from(header.index_offset).unwrap() + 4;
        b[at..at + 4].copy_from_slice(&10_000u32.to_le_bytes());
        let e = refuse(&b, "tamper-unit");
        let msg = e.to_string();
        assert!(
            msg.contains("损坏") || msg.contains("校验和") || msg.contains("非单调"),
            "{msg}"
        );
    }

    #[test]
    fn tampered_unit_offset_with_recomputed_checksum_is_refused_structurally() {
        // **绕过校验和**：改完结构再把 body_checksum 重算正确。
        // 这模拟"能改文件的人"——威胁模型说校验和挡不住他，
        // 挡他的是结构校验。
        let mut b = canary("tamper-unit-bypass");
        let header =
            TableHeader::from_bytes(&b[..HEADER_SIZE as usize].try_into().unwrap()).unwrap();
        let at = usize::try_from(header.index_offset).unwrap() + 4;
        b[at..at + 4].copy_from_slice(&10_000u32.to_le_bytes());
        recompute_body_checksum(&mut b);
        let e = refuse(&b, "tamper-unit-bypass");
        assert!(matches!(e, FormatError::Corrupt(_)), "{e:?}");
        let msg = e.to_string();
        assert!(
            msg.contains("非单调") || msg.contains("超过总数"),
            "必须是结构校验挡下来，而不是 panic：{msg}"
        );
    }

    #[test]
    fn non_monotonic_offsets_are_refused() {
        let mut b = canary("nonmono");
        let header =
            TableHeader::from_bytes(&b[..HEADER_SIZE as usize].try_into().unwrap()).unwrap();
        // 第一个 entry_offset（下标 0）改成 5 —— 但首项必须是 0。
        let at =
            usize::try_from(header.index_offset).unwrap() + (header.code_count as usize + 1) * 4;
        b[at..at + 4].copy_from_slice(&5u32.to_le_bytes());
        recompute_body_checksum(&mut b);
        let e = refuse(&b, "nonmono");
        assert!(matches!(e, FormatError::Corrupt(_)), "{e:?}");
    }

    #[test]
    fn bad_sentinel_is_refused() {
        let mut b = canary("sentinel");
        let header =
            TableHeader::from_bytes(&b[..HEADER_SIZE as usize].try_into().unwrap()).unwrap();
        let n = header.code_count as usize;
        // 最后一个 unit_offset 改成错的（哨兵必须是 total_units）。
        let at = usize::try_from(header.index_offset).unwrap() + n * 4;
        b[at..at + 4].copy_from_slice(&1u32.to_le_bytes());
        recompute_body_checksum(&mut b);
        let e = refuse(&b, "sentinel");
        assert!(matches!(e, FormatError::Corrupt(_)), "{e:?}");
    }

    #[test]
    fn entry_pointing_outside_the_word_table_is_refused() {
        // 改一条词条记录里的 word_off，让它指向词表之外。
        let mut b = canary("badword");
        let header =
            TableHeader::from_bytes(&b[..HEADER_SIZE as usize].try_into().unwrap()).unwrap();
        let at = usize::try_from(header.entries_offset).unwrap();
        b[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        recompute_body_checksum(&mut b);
        // 结构校验看不到单条记录，因此装载会成功；**查询必须安全**。
        refuse_or_query_safely(&b, "badword");
    }

    #[test]
    fn truncated_and_overlong_files_are_refused() {
        let b = canary("trunc");
        for cut in [0usize, 1, 40, 79, 80, 81, 120] {
            let short = &b[..cut.min(b.len())];
            let p = write_bytes(&format!("trunc-{cut}"), short);
            let r = TableLexicon::open_checked(&p, Some(fp()));
            assert!(r.is_err(), "截断到 {cut} 字节的文件不该装载成功");
            let _ = std::fs::remove_file(&p);
        }
        let mut long = b.clone();
        long.push(0);
        let e = refuse(&long, "overlong");
        assert!(matches!(e, FormatError::Corrupt(_)), "{e:?}");
    }

    #[test]
    fn flipped_bytes_never_panic() {
        // 逐字节翻转：每一个位置都必须"安全拒绝或安全查询"。
        // 这是无依赖版本的畸形输入测试（cargo-fuzz 在本项目不可用）。
        let base = canary("flip");
        for i in (0..base.len()).step_by(7) {
            let mut b = base.clone();
            b[i] ^= 0xff;
            refuse_or_query_safely(&b, &format!("flip-{i}"));
        }
    }

    #[test]
    fn absurd_header_counts_never_panic() {
        // 直接往头部字段里塞极限值——旧实现在 `code_count + 1` 与
        // `Vec::with_capacity` 处会溢出或申请天量内存。
        for (off, val) in [
            (24usize, u32::MAX), // entry_count
            (28, u32::MAX),      // code_count
            (32, u32::MAX),      // word_bytes
            (36, u32::MAX),      // total_units
        ] {
            let mut b = canary("absurd");
            b[off..off + 4].copy_from_slice(&val.to_le_bytes());
            recompute_body_checksum(&mut b);
            let p = write_bytes(&format!("absurd-{off}"), &b);
            let _ = TableLexicon::open_checked(&p, Some(fp()));
            let _ = std::fs::remove_file(&p);
        }
        for (off, val) in [
            (40usize, u64::MAX), // words_offset
            (48, u64::MAX),      // entries_offset
            (56, u64::MAX),      // index_offset
        ] {
            let mut b = canary("absurd2");
            b[off..off + 8].copy_from_slice(&val.to_le_bytes());
            recompute_body_checksum(&mut b);
            let p = write_bytes(&format!("absurd2-{off}"), &b);
            let _ = TableLexicon::open_checked(&p, Some(fp()));
            let _ = std::fs::remove_file(&p);
        }
    }

    /// 头部之后全部字节的 FNV-1a，写回头部字段——模拟"会改文件的人"。
    fn recompute_body_checksum(bytes: &mut [u8]) {
        let mut ck = BodyChecksum::new();
        ck.update(&bytes[HEADER_SIZE as usize..]);
        bytes[72..80].copy_from_slice(&ck.finish().to_le_bytes());
    }
}
