//! # Binary layout
//!
//! 中文职责：词库编译产物的字节布局、头部读写、校验和。
//! English role: the byte layout, header I/O and checksum of a compiled lexicon.
//! 架构位置：`stele-table` 的最底层；`compile` 写它、`lexicon` 读它。
//!
//! # 布局（全部**小端**）
//!
//! ```text
//! 偏移  长度  字段
//! 0     8     magic = "STELELEX"
//! 8     4     format_version
//! 12    4     header_size (= 64)
//! 16    8     source_checksum   ← 源数据的 FNV-1a；用于"要不要重新编译"（D28）
//! 24    4     entry_count
//! 28    4     code_count        ← 不同的编码个数
//! 32    4     word_bytes
//! 36    4     total_units       ← 所有编码的编码单元总数
//! 40    8     words_offset
//! 48    8     entries_offset
//! 56    8     index_offset
//! ── 64 ──────────────────────────────────────────────────────────────
//!             词字符串表      word_bytes 字节，按词条顺序拼接
//!             词条数组        entry_count × 12 字节
//!             索引            见下
//! ```
//!
//! 词条记录 12 字节：
//!
//! ```text
//! word_off u32 | word_len u16 | flags u16 | score i32
//! ```
//!
//! 索引三段（顺序排列在 `index_offset` 处）：
//!
//! ```text
//! unit_offsets  (code_count+1) × u32   ← 前缀和，指向 units
//! entry_offsets (code_count+1) × u32   ← 前缀和，指向词条数组
//! units         total_units    × u16   ← 所有编码的编码单元，顺序拼接
//! ```
//!
//! **为什么索引用前缀和而不是"每个编码一个偏移"**：前缀和让第 i 个编码的
//! 区间是 `[off[i], off[i+1])`，省掉一个长度字段，也让二分查找只需读这两个数组。

use std::io;

/// 文件魔数。
pub const MAGIC: &[u8; 8] = b"STELELEX";

/// 格式版本。**不认识的版本一律拒绝，绝不猜**（PLAN D27 / D28）。
///
/// 版本演进时的规矩：**一次只加一步**（`vN → vN+1`），
/// 且旧版本必须能被明确识别并给出"请重新部署"的提示。
pub const FORMAT_VERSION: u32 = 1;

/// 头部固定长度。
pub const HEADER_SIZE: u32 = 64;

/// 一条词条记录的字节数。
pub const ENTRY_SIZE: usize = 12;

/// 源数据校验和：FNV-1a 64 位。
///
/// 用它而不是加密散列：**这不是安全边界**，只是"源数据变没变"的快速判断。
/// 一个几十行的实现胜过为此引入一个依赖。
#[must_use]
pub fn source_checksum(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// 把两个校验和合起来（用于多份源文件）。
#[must_use]
pub fn combine_checksums(a: u64, b: u64) -> u64 {
    // 简单但确定的混合。同样不是密码学用途。
    a.rotate_left(7) ^ b.wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

/// 文件头。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableHeader {
    /// 格式版本。
    pub format_version: u32,
    /// 源数据校验和。
    pub source_checksum: u64,
    /// 词条数。
    pub entry_count: u32,
    /// 不同的编码个数。
    pub code_count: u32,
    /// 词字符串表的字节数。
    pub word_bytes: u32,
    /// 所有编码的编码单元总数。
    pub total_units: u32,
    /// 词字符串表在文件里的偏移。
    pub words_offset: u64,
    /// 词条数组在文件里的偏移。
    pub entries_offset: u64,
    /// 索引在文件里的偏移。
    pub index_offset: u64,
}

impl TableHeader {
    /// 索引三段的字节数。
    #[must_use]
    pub fn index_size(&self) -> u64 {
        let offsets = u64::from(self.code_count + 1) * 4 * 2;
        let units = u64::from(self.total_units) * 2;
        offsets + units
    }

    /// 整个文件的字节数。
    #[must_use]
    pub fn file_size(&self) -> u64 {
        self.index_offset + self.index_size()
    }

    /// 序列化成 64 字节。
    #[must_use]
    pub fn to_bytes(self) -> [u8; HEADER_SIZE as usize] {
        let mut b = [0u8; HEADER_SIZE as usize];
        b[0..8].copy_from_slice(MAGIC);
        b[8..12].copy_from_slice(&self.format_version.to_le_bytes());
        b[12..16].copy_from_slice(&HEADER_SIZE.to_le_bytes());
        b[16..24].copy_from_slice(&self.source_checksum.to_le_bytes());
        b[24..28].copy_from_slice(&self.entry_count.to_le_bytes());
        b[28..32].copy_from_slice(&self.code_count.to_le_bytes());
        b[32..36].copy_from_slice(&self.word_bytes.to_le_bytes());
        b[36..40].copy_from_slice(&self.total_units.to_le_bytes());
        b[40..48].copy_from_slice(&self.words_offset.to_le_bytes());
        b[48..56].copy_from_slice(&self.entries_offset.to_le_bytes());
        b[56..64].copy_from_slice(&self.index_offset.to_le_bytes());
        b
    }

    /// 从 64 字节解析。
    ///
    /// # Errors
    ///
    /// 魔数不对、版本不认识、头部长度不对时返回 [`FormatError`]。
    pub fn from_bytes(b: &[u8; HEADER_SIZE as usize]) -> Result<Self, FormatError> {
        if &b[0..8] != MAGIC {
            return Err(FormatError::BadMagic);
        }
        let format_version = u32::from_le_bytes([b[8], b[9], b[10], b[11]]);
        if format_version != FORMAT_VERSION {
            return Err(FormatError::UnsupportedVersion {
                found: format_version,
                expected: FORMAT_VERSION,
            });
        }
        let header_size = u32::from_le_bytes([b[12], b[13], b[14], b[15]]);
        if header_size != HEADER_SIZE {
            return Err(FormatError::BadHeaderSize(header_size));
        }
        Ok(Self {
            format_version,
            source_checksum: u64::from_le_bytes(b[16..24].try_into().unwrap_or([0; 8])),
            entry_count: u32::from_le_bytes(b[24..28].try_into().unwrap_or([0; 4])),
            code_count: u32::from_le_bytes(b[28..32].try_into().unwrap_or([0; 4])),
            word_bytes: u32::from_le_bytes(b[32..36].try_into().unwrap_or([0; 4])),
            total_units: u32::from_le_bytes(b[36..40].try_into().unwrap_or([0; 4])),
            words_offset: u64::from_le_bytes(b[40..48].try_into().unwrap_or([0; 8])),
            entries_offset: u64::from_le_bytes(b[48..56].try_into().unwrap_or([0; 8])),
            index_offset: u64::from_le_bytes(b[56..64].try_into().unwrap_or([0; 8])),
        })
    }
}

/// 格式错误。
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormatError {
    /// 文件不是本格式。
    BadMagic,
    /// 格式版本不认识。
    UnsupportedVersion {
        /// 读到的版本。
        found: u32,
        /// 本程序支持的版本。
        expected: u32,
    },
    /// 头部长度不对（说明文件被截断或来自别的工具）。
    BadHeaderSize(u32),
    /// 文件长度与头部声明的不一致。
    Truncated {
        /// 头部声明的长度。
        expected: u64,
        /// 实际长度。
        actual: u64,
    },
    /// 索引里出现越界引用（文件损坏）。
    Corrupt(String),
    /// I/O 错误。
    Io(String),
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadMagic => write!(
                f,
                "这不是 stele 的词库编译产物（魔数不对）。\
                 若文件名是 `.table.bin`，它可能是别的工具生成的——请重新部署。"
            ),
            Self::UnsupportedVersion { found, expected } => write!(
                f,
                "词库产物的格式版本是 {found}，本程序支持 {expected}。\
                 **请重新部署**——不要试图用旧产物凑合，那会静默给出错误的结果。"
            ),
            Self::BadHeaderSize(n) => write!(f, "头部长度异常（{n} 字节），文件可能已损坏"),
            Self::Truncated { expected, actual } => write!(
                f,
                "文件被截断：头部声明 {expected} 字节，实际 {actual} 字节"
            ),
            Self::Corrupt(m) => write!(f, "产物内容损坏：{m}"),
            Self::Io(m) => write!(f, "读取失败：{m}"),
        }
    }
}

impl std::error::Error for FormatError {}

impl From<io::Error> for FormatError {
    fn from(e: io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

/// 按平台读文件的一段。
///
/// `read_at` / `seek_read` 取 `&File`（不是 `&mut`），因此 `TableLexicon`
/// 可以是 `Sync` —— 这正是"一个引擎被多个会话共享"需要的。
pub(crate) fn read_exact_at(file: &std::fs::File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_exact_at(buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0usize;
        while done < buf.len() {
            let n = file.seek_read(&mut buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "读到文件末尾"));
            }
            done += n;
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (file, buf, offset);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "本平台没有实现按偏移读取",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trips() {
        let h = TableHeader {
            format_version: FORMAT_VERSION,
            source_checksum: 0xdead_beef,
            entry_count: 123,
            code_count: 45,
            word_bytes: 678,
            total_units: 90,
            words_offset: 64,
            entries_offset: 742,
            index_offset: 2218,
        };
        let b = h.to_bytes();
        assert_eq!(TableHeader::from_bytes(&b).unwrap(), h);
    }

    #[test]
    fn bad_magic_is_rejected_with_an_explanation() {
        let mut b = [0u8; 64];
        b[0..8].copy_from_slice(b"NOTSTELE");
        let e = TableHeader::from_bytes(&b).unwrap_err();
        assert!(matches!(e, FormatError::BadMagic));
        assert!(e.to_string().contains("重新部署"), "{e}");
    }

    #[test]
    fn future_version_is_rejected_not_guessed() {
        let mut h = TableHeader {
            format_version: FORMAT_VERSION,
            source_checksum: 0,
            entry_count: 0,
            code_count: 0,
            word_bytes: 0,
            total_units: 0,
            words_offset: 64,
            entries_offset: 64,
            index_offset: 64,
        };
        h.format_version = 99;
        let mut b = h.to_bytes();
        b[8..12].copy_from_slice(&99u32.to_le_bytes());
        let e = TableHeader::from_bytes(&b).unwrap_err();
        assert!(matches!(e, FormatError::UnsupportedVersion { .. }));
        assert!(e.to_string().contains("静默给出错误的结果"), "{e}");
    }

    #[test]
    fn checksum_is_stable_and_sensitive() {
        let a = source_checksum(b"hello");
        assert_eq!(a, source_checksum(b"hello"));
        assert_ne!(a, source_checksum(b"hellp"));
        assert_ne!(combine_checksums(a, 1), combine_checksums(a, 2));
    }

    #[test]
    fn index_size_matches_the_layout() {
        let h = TableHeader {
            format_version: FORMAT_VERSION,
            source_checksum: 0,
            entry_count: 10,
            code_count: 4,
            word_bytes: 100,
            total_units: 9,
            words_offset: 64,
            entries_offset: 164,
            index_offset: 284,
        };
        // (4+1)×4×2 + 9×2 = 40 + 18 = 58
        assert_eq!(h.index_size(), 58);
        assert_eq!(h.file_size(), 284 + 58);
    }
}
