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

use crate::format::{read_exact_at, FormatError, TableHeader, ENTRY_SIZE, HEADER_SIZE};

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

    /// 打开并**校验源数据校验和**（PLAN D28）。
    ///
    /// # Errors
    ///
    /// 除 [`TableLexicon::open`] 的错误外，校验和不匹配时返回
    /// [`FormatError::Corrupt`]——**拒绝加载，而不是凑合跑**。
    /// 用错版本的产物会静默给出错误结果，那比加载失败糟糕得多。
    pub fn open_checked(path: &Path, expected: Option<u64>) -> Result<Self, FormatError> {
        let file = File::open(path)?;
        let actual = file.metadata()?.len();

        // 先查长度：比 64 字节还短的文件连头部都放不下，
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

        if actual < header.file_size() {
            return Err(FormatError::Truncated {
                expected: header.file_size(),
                actual,
            });
        }
        if let Some(exp) = expected {
            if exp != header.source_checksum {
                return Err(FormatError::Corrupt(format!(
                    "源数据校验和不匹配（产物 {}，源数据 {exp}）——\
                     请重新部署。用旧产物凑合跑会静默给出错误结果。",
                    header.source_checksum
                )));
            }
        }

        // ── 把索引读进内存（这是唯一常驻的部分）──
        let code_count = header.code_count as usize;
        #[allow(clippy::cast_possible_truncation)]
        let index_size = header.index_size() as usize;
        let mut raw = vec![0u8; index_size];
        read_exact_at(&file, &mut raw, header.index_offset)?;

        let mut unit_offsets = Vec::with_capacity(code_count + 1);
        let mut entry_offsets = Vec::with_capacity(code_count + 1);
        let mut p = 0usize;
        for _ in 0..=code_count {
            unit_offsets.push(u32::from_le_bytes(
                raw[p..p + 4].try_into().unwrap_or([0; 4]),
            ));
            p += 4;
        }
        for _ in 0..=code_count {
            entry_offsets.push(u32::from_le_bytes(
                raw[p..p + 4].try_into().unwrap_or([0; 4]),
            ));
            p += 4;
        }
        let mut units = Vec::with_capacity(header.total_units as usize);
        for _ in 0..header.total_units {
            units.push(u16::from_le_bytes(
                raw[p..p + 2].try_into().unwrap_or([0; 2]),
            ));
            p += 2;
        }

        if unit_offsets.last().copied().unwrap_or(0) as usize != units.len() {
            return Err(FormatError::Corrupt("编码索引与单元表长度不符".into()));
        }
        if entry_offsets.last().copied().unwrap_or(0) != header.entry_count {
            return Err(FormatError::Corrupt("词条索引与词条数不符".into()));
        }

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
    fn find_code(&self, code: &[u16]) -> Option<usize> {
        let n = self.entry_offsets.len().checked_sub(1)?;
        let code_at = |i: usize| -> &[u16] {
            let (a, b) = (
                self.unit_offsets[i] as usize,
                self.unit_offsets[i + 1] as usize,
            );
            &self.units[a..b]
        };
        // 手写二分：`units` 是拼接的，无法直接用 `binary_search` 的切片接口。
        let (mut lo, mut hi) = (0usize, n);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match code_at(mid).cmp(code) {
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
    fn read_entries(&self, idx: usize, limit: usize) -> Result<Vec<(String, i32)>, FormatError> {
        let start = self.entry_offsets[idx] as usize;
        let end = self.entry_offsets[idx + 1] as usize;
        if end < start {
            return Err(FormatError::Corrupt("词条索引非单调".into()));
        }
        let count = (end - start).min(limit);
        if count == 0 {
            return Ok(Vec::new());
        }

        let mut buf = vec![0u8; count * ENTRY_SIZE];
        read_exact_at(
            &self.file,
            &mut buf,
            self.header.entries_offset + (start as u64) * ENTRY_SIZE as u64,
        )?;

        let mut recs: Vec<(u32, u16, i32)> = Vec::with_capacity(count);
        for i in 0..count {
            let r = &buf[i * ENTRY_SIZE..(i + 1) * ENTRY_SIZE];
            recs.push((
                u32::from_le_bytes(r[0..4].try_into().unwrap_or([0; 4])),
                u16::from_le_bytes(r[4..6].try_into().unwrap_or([0; 2])),
                i32::from_le_bytes(r[8..12].try_into().unwrap_or([0; 4])),
            ));
        }

        // 词字节：因为写产物时按排序后的顺序重排过词表，
        // 所以这几条词条的词在文件里是**连续**的 —— 一次 read_at 拿全。
        let first = recs.first().map_or(0, |r| r.0);
        let last_end = recs
            .iter()
            .map(|(off, len, _)| off + u32::from(*len))
            .max()
            .unwrap_or(0);
        let span = (last_end - first) as usize;
        let mut words = vec![0u8; span];
        read_exact_at(
            &self.file,
            &mut words,
            self.header.words_offset + u64::from(first),
        )?;

        let mut out = Vec::with_capacity(count);
        for (off, len, score) in recs {
            let a = (off - first) as usize;
            let b = a + usize::from(len);
            let s = String::from_utf8_lossy(&words[a..b]).into_owned();
            out.push((s, score));
        }
        Ok(out)
    }
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
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::compile;

    fn build(name: &str, entries: &[(&str, &[u16], f64)]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("stele-lex-{name}.table"));
        let _ = std::fs::remove_file(&p);
        compile(
            7,
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
    fn checksum_mismatch_is_refused_not_tolerated() {
        let p = build("ck", &[("甲", &[1], 1.0)]);
        // 正确的校验和可以打开。
        assert!(TableLexicon::open_checked(&p, Some(7)).is_ok());
        // 不匹配 → **拒绝加载**，而不是凑合跑。
        let e = TableLexicon::open_checked(&p, Some(999)).unwrap_err();
        assert!(e.to_string().contains("校验和不匹配"), "{e}");
        assert!(e.to_string().contains("重新部署"), "{e}");
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
        compile(0, |_w| Ok(()), &p).unwrap();
        let l = TableLexicon::open(&p).unwrap();
        assert!(lookup(&l, &[1]).is_empty());
    }
}
