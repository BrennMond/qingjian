//! # Binary layout
//!
//! 中文职责：词库编译产物的字节布局、头部读写、校验和。
//! English role: the byte layout, header I/O and checksum of a compiled lexicon.
//! 架构位置：`qingjian-table` 的最底层；`compile` 写它、`lexicon` 读它。
//!
//! # 布局（全部**小端**）
//!
//! ```text
//! 偏移  长度  字段
//! 0     8     magic = "QJIANLEX"
//! 8     4     format_version
//! 12    4     header_size (= 80)
//! 16    8     source_checksum   ← 源数据的 FNV-1a；用于"要不要重新编译"（D28）
//! 24    4     entry_count
//! 28    4     code_count        ← 不同的编码个数
//! 32    4     word_bytes
//! 36    4     total_units       ← 所有编码的编码单元总数
//! 40    8     words_offset
//! 48    8     entries_offset
//! 56    8     index_offset
//! 64    8     build_fingerprint ← **产物身份**：格式版本 + 编译选项 +
//!                                 字母表内容与顺序 + 源数据校验和
//! 72    8     body_checksum     ← 头部之后全部字节的 FNV-1a（意外损坏检测）
//! ── 80 ──────────────────────────────────────────────────────────────
//!             词字符串表      word_bytes 字节，按词条顺序拼接
//!             词条数组        entry_count × 12 字节
//!             索引            见下
//! ```
//!
//! # `build_fingerprint` 为什么必须存在（P0 修复）
//!
//! 早先的缓存名字只用 `source_checksum`（源词典的字节），而产物里存的
//! 是**编码单元的下标**。只把方案的 `alphabet` 顺序换一下、词典一个字节
//! 不改，旧产物就会被复用，于是同一个下标指向另一个音节——**静默错码**：
//! 敲 `ni` 出「好」。产物身份必须包含**一切影响下标语义的输入**，
//! 这就是 [`BuildFingerprint`] 的职责。
//!
//! # `body_checksum` 的威胁模型（**不是安全边界**）
//!
//! 它覆盖头部之后的全部字节，用来发现**意外损坏**（磁盘位翻转、
//! 传输截断、构建脚本写坏）。它是 FNV-1a，**没有抗碰撞性**：
//! 能改文件的人也能重算它。因此真正的防越界靠的是
//! [`validate_layout`] 与读侧全程 checked 运算——那是无论校验和
//! 是否通过都必须成立的**结构性**保证。文档与诊断里不得把
//! `body_checksum` 说成"防篡改"。
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

/// 文件魔数：`QJIANLEX`，**恰好 8 字节**（字段宽度固定为 8，改长短就是换格式）。
/// 改它等于换了一种文件格式——旧魔数的文件会被[`FormatError::BadMagic`] 明确拒绝。
pub const MAGIC: &[u8; 8] = b"QJIANLEX";

/// 格式版本。**不认识的版本一律拒绝，绝不猜**（PLAN D27 / D28）。
///
/// 版本演进时的规矩：**一次只加一步**（`vN → vN+1`），
/// 且旧版本必须能被明确识别并给出"请重新部署"的提示。
///
/// - `1`：`source_checksum` + 64 字节头部。**缓存身份不含字母表**
///   （会静默错码），且没有产物完整性校验。已废弃。
/// - `2`：80 字节头部，加入 [`BuildFingerprint`] 与 `body_checksum`。
pub const FORMAT_VERSION: u32 = 2;

/// 头部固定长度。
pub const HEADER_SIZE: u32 = 80;

/// 一条词条记录的字节数。
pub const ENTRY_SIZE: usize = 12;

/// 编译选项串：进入 [`BuildFingerprint`] 的一部分。
///
/// 任何会改变产物字节含义的编译期选择都必须写在这里，改了就要改版本。
/// 现在只有一项：权重到定点分数的换算方式。
pub const COMPILER_OPTIONS: &str = "qingjian-table;weights=milli-log;entry=12";

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

/// FNV-1a 64 位，可增量喂入。
///
/// 为什么不用 `std::hash::Hasher`：标准库**不承诺** `DefaultHasher`
/// 的算法与跨版本稳定性，而产物身份必须跨进程、跨版本可复现
/// （PLAN §5.2：同一输入逐字节相同）。
#[derive(Clone, Copy, Debug)]
struct Fnv(u64);

impl Fnv {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    const fn new() -> Self {
        Self(Self::OFFSET)
    }

    fn byte(&mut self, b: u8) {
        self.0 ^= u64::from(b);
        self.0 = self.0.wrapping_mul(Self::PRIME);
    }

    fn bytes(&mut self, bs: &[u8]) {
        for b in bs {
            self.byte(*b);
        }
    }

    /// 喂一个 `u64`（小端）。
    fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }

    /// 喂一个字符串：**先长度后内容**。
    ///
    /// 变长字段必须带长度，否则 `["ab","c"]` 与 `["a","bc"]` 会算出
    /// 同一个指纹——那正是"身份碰撞导致复用错产物"的成因。
    fn str(&mut self, s: &str) {
        self.u64(s.len() as u64);
        self.bytes(s.as_bytes());
    }
}

/// 一份编译产物的**身份**。
///
/// # 它回答的问题
///
/// "这份 `.table` 是不是由**当前这份方案数据**编译出来的？"
///
/// 而不是"源词典文件变了没有"——后者是 `source_checksum`，它**不够**：
/// 产物的语义还取决于字母表的内容与顺序（下标是编译期决定的）。
///
/// 进入指纹的每一项都是**语义输入**：
///
/// | 项 | 变了会怎样 |
/// | --- | --- |
/// | 格式版本 | 布局变了，旧产物不可读 |
/// | 编译选项 | 权重换算/记录布局变了 |
/// | 字母表内容与顺序 | **下标含义变了** → 静默错码（P0 复现） |
/// | 源数据校验和 | 词条内容变了 |
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BuildFingerprint(u64);

impl BuildFingerprint {
    /// 从原始值构造（测试与诊断用）。
    #[must_use]
    pub const fn from_u64(v: u64) -> Self {
        Self(v)
    }

    /// 原始值。
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// 16 位小写十六进制——缓存文件名里用的形态。
    #[must_use]
    pub fn hex(self) -> String {
        format!("{:016x}", self.0)
    }

    /// 由全部语义输入算出指纹。
    #[must_use]
    pub fn of(
        format_version: u32,
        compiler_options: &str,
        alphabet: &[String],
        source_checksum: u64,
    ) -> Self {
        let mut h = Fnv::new();
        h.u64(u64::from(format_version));
        h.str(compiler_options);
        // 字母表**条数 + 每条内容**，顺序敏感。
        h.u64(alphabet.len() as u64);
        for unit in alphabet {
            h.str(unit);
        }
        h.u64(source_checksum);
        Self(h.0)
    }
}

impl std::fmt::Display for BuildFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.hex())
    }
}

/// 增量计算**产物主体**（头部之后全部字节）的校验和。
///
/// 写侧与读侧都必须**按同一顺序**喂入——顺序就是文件里的字节顺序：
/// 词字符串表 → 词条数组 → 索引。
#[derive(Clone, Copy, Debug)]
pub struct BodyChecksum(Fnv);

impl Default for BodyChecksum {
    fn default() -> Self {
        Self::new()
    }
}

impl BodyChecksum {
    /// 空校验和。
    #[must_use]
    pub fn new() -> Self {
        Self(Fnv::new())
    }

    /// 喂入一段产物字节。
    pub fn update(&mut self, bytes: &[u8]) {
        self.0.bytes(bytes);
    }

    /// 结束并取值。
    #[must_use]
    pub fn finish(self) -> u64 {
        self.0 .0
    }
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
    /// 产物身份（见 [`BuildFingerprint`]）。
    pub build_fingerprint: u64,
    /// 产物主体的 FNV-1a（见模块文档的威胁模型）。
    pub body_checksum: u64,
}

impl TableHeader {
    /// 索引三段的字节数。**溢出时返回 `None`**，绝不 wrap。
    #[must_use]
    pub fn checked_index_size(&self) -> Option<u64> {
        let offsets = u64::from(self.code_count)
            .checked_add(1)?
            .checked_mul(4)?
            .checked_mul(2)?;
        let units = u64::from(self.total_units).checked_mul(2)?;
        offsets.checked_add(units)
    }

    /// 索引三段的字节数（诊断/展示用；溢出时饱和）。
    #[must_use]
    pub fn index_size(&self) -> u64 {
        self.checked_index_size().unwrap_or(u64::MAX)
    }

    /// 整个文件的字节数。**溢出时返回 `None`**。
    #[must_use]
    pub fn checked_file_size(&self) -> Option<u64> {
        self.index_offset.checked_add(self.checked_index_size()?)
    }

    /// 整个文件的字节数（诊断/展示用；溢出时饱和）。
    #[must_use]
    pub fn file_size(&self) -> u64 {
        self.checked_file_size().unwrap_or(u64::MAX)
    }

    /// 序列化成 [`HEADER_SIZE`] 字节。
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
        b[64..72].copy_from_slice(&self.build_fingerprint.to_le_bytes());
        b[72..80].copy_from_slice(&self.body_checksum.to_le_bytes());
        b
    }

    /// 从 [`HEADER_SIZE`] 字节解析。
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
            build_fingerprint: u64::from_le_bytes(b[64..72].try_into().unwrap_or([0; 8])),
            body_checksum: u64::from_le_bytes(b[72..80].try_into().unwrap_or([0; 8])),
        })
    }
}

/// 在**分配任何索引缓冲之前**把头部声明的布局校验一遍。
///
/// # 为什么必须在这里做（P0 复现的根因）
///
/// `code_count` / `total_units` / 各 offset 都是**文件里的数字**。
/// 旧实现直接拿它们去 `Vec::with_capacity` 和切片：篡改一个
/// `unit_offsets` 值就能让 release 进程在切片范围处 panic。
/// 结构性校验必须在任何"用文件里的数字索引内存"之前完成，
/// 并且**全程 checked 运算**——溢出本身也是一种畸形输入。
///
/// # 校验的内容
///
/// - 三个区段**顺序相邻、不重叠、都在文件长度内**；
/// - `entry_count × 12`、`code_count + 1`、`total_units × 2` 均不溢出；
/// - 索引大小与 `index_offset` 相加不溢出，且**恰好等于**文件长度
///   （多出来的尾巴是损坏，不是"将来扩展"——扩展要走版本号）。
///
/// # Errors
///
/// 任一条不成立时返回 [`FormatError::Corrupt`] 或 [`FormatError::Truncated`]。
pub fn validate_layout(header: &TableHeader, actual_len: u64) -> Result<(), FormatError> {
    let index_size = header.checked_index_size().ok_or_else(|| {
        FormatError::Corrupt("索引大小溢出（code_count/total_units 是畸形值）".into())
    })?;
    let file_size = header
        .checked_file_size()
        .ok_or_else(|| FormatError::Corrupt("文件长度溢出（index_offset 是畸形值）".into()))?;

    if actual_len < file_size {
        return Err(FormatError::Truncated {
            expected: file_size,
            actual: actual_len,
        });
    }
    if actual_len > file_size {
        return Err(FormatError::Corrupt(format!(
            "文件尾部多出 {} 字节（头部声明的总长是 {file_size}）——产物已损坏",
            actual_len - file_size
        )));
    }

    // 词字符串表。
    if header.words_offset < u64::from(HEADER_SIZE) {
        return Err(FormatError::Corrupt(format!(
            "词表偏移 {} 落在头部之内（头部 {HEADER_SIZE} 字节）",
            header.words_offset
        )));
    }
    let words_end = header
        .words_offset
        .checked_add(u64::from(header.word_bytes))
        .ok_or_else(|| FormatError::Corrupt("词表区间溢出".into()))?;
    if words_end != header.entries_offset {
        return Err(FormatError::Corrupt(format!(
            "词表区间 [{}, {words_end}) 与词条区偏移 {} 不衔接",
            header.words_offset, header.entries_offset
        )));
    }

    // 词条数组。
    let entry_bytes = u64::from(header.entry_count)
        .checked_mul(ENTRY_SIZE as u64)
        .ok_or_else(|| FormatError::Corrupt("词条区大小溢出".into()))?;
    let entries_end = header
        .entries_offset
        .checked_add(entry_bytes)
        .ok_or_else(|| FormatError::Corrupt("词条区区间溢出".into()))?;
    if entries_end != header.index_offset {
        return Err(FormatError::Corrupt(format!(
            "词条区区间 [{}, {entries_end}) 与索引偏移 {} 不衔接",
            header.entries_offset, header.index_offset
        )));
    }

    // 索引自身。
    let index_end = header
        .index_offset
        .checked_add(index_size)
        .ok_or_else(|| FormatError::Corrupt("索引区间溢出".into()))?;
    if index_end != file_size {
        return Err(FormatError::Corrupt("索引区间与文件长度不一致".into()));
    }

    Ok(())
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
    /// 产物身份与当前方案数据不符。
    ///
    /// 出现它意味着**产物是用另一套语义输入编译的**（字母表不同、
    /// 格式版本不同、源数据不同）——复用会静默错码，所以必须重建。
    FingerprintMismatch {
        /// 当前算出来的指纹。
        expected: u64,
        /// 产物头部写的指纹。
        found: u64,
    },
    /// 产物主体校验和不匹配（**意外损坏检测**，不是防篡改）。
    BodyChecksumMismatch {
        /// 头部写的校验和。
        expected: u64,
        /// 实际算出来的。
        found: u64,
    },
    /// I/O 错误。
    Io(String),
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadMagic => write!(
                f,
                "这不是 qingjian 的词库编译产物（魔数不对）。\
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
            Self::FingerprintMismatch { expected, found } => write!(
                f,
                "词库产物的身份指纹不匹配（产物 {found:016x}，当前方案 {expected:016x}）。\
                 这通常意味着**字母表、格式版本或源词典变了**——产物里的编码是下标，\
                 换一套字母表就会静默指向别的音节。**请重新部署**。"
            ),
            Self::BodyChecksumMismatch { expected, found } => write!(
                f,
                "词库产物的内容校验和不匹配（头部 {expected:016x}，实测 {found:016x}）——\
                 文件已损坏。**请重新部署**。\
                 （这是意外损坏检测，不是防篡改机制。）"
            ),
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
            words_offset: 80,
            entries_offset: 758,
            index_offset: 2234,
            build_fingerprint: 0x1234_5678_9abc_def0,
            body_checksum: 0x0fed_cba9_8765_4321,
        };
        let b = h.to_bytes();
        assert_eq!(TableHeader::from_bytes(&b).unwrap(), h);
    }

    #[test]
    fn bad_magic_is_rejected_with_an_explanation() {
        let mut b = [0u8; HEADER_SIZE as usize];
        b[0..8].copy_from_slice(b"NOTQJIAN");
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
            words_offset: 80,
            entries_offset: 80,
            index_offset: 80,
            build_fingerprint: 0,
            body_checksum: 0,
        };
        h.format_version = 99;
        let mut b = h.to_bytes();
        b[8..12].copy_from_slice(&99u32.to_le_bytes());
        let e = TableHeader::from_bytes(&b).unwrap_err();
        assert!(matches!(e, FormatError::UnsupportedVersion { .. }));
        assert!(e.to_string().contains("静默给出错误的结果"), "{e}");
    }

    #[test]
    fn version_1_artifacts_are_refused_not_reinterpreted() {
        // v1 的头部是 64 字节。即便有人把版本号改回 1，也要被拒绝——
        // 因为 v1 的产物**没有字母表身份**，复用它会静默错码。
        let mut b = [0u8; HEADER_SIZE as usize];
        b[0..8].copy_from_slice(MAGIC);
        b[8..12].copy_from_slice(&1u32.to_le_bytes());
        b[12..16].copy_from_slice(&64u32.to_le_bytes());
        let e = TableHeader::from_bytes(&b).unwrap_err();
        assert!(
            matches!(e, FormatError::UnsupportedVersion { found: 1, .. }),
            "{e:?}"
        );
    }

    #[test]
    fn checksum_is_stable_and_sensitive() {
        let a = source_checksum(b"hello");
        assert_eq!(a, source_checksum(b"hello"));
        assert_ne!(a, source_checksum(b"hellp"));
        assert_ne!(combine_checksums(a, 1), combine_checksums(a, 2));
    }

    #[test]
    fn fingerprint_is_sensitive_to_alphabet_order() {
        let a = vec!["ni".to_owned(), "hao".to_owned()];
        let b = vec!["hao".to_owned(), "ni".to_owned()];
        let fa = BuildFingerprint::of(FORMAT_VERSION, COMPILER_OPTIONS, &a, 7);
        let fb = BuildFingerprint::of(FORMAT_VERSION, COMPILER_OPTIONS, &b, 7);
        assert_ne!(fa, fb, "**字母表顺序必须进入指纹**——这就是静默错码的根因");
        // 同一份输入必须稳定。
        assert_eq!(
            fa,
            BuildFingerprint::of(FORMAT_VERSION, COMPILER_OPTIONS, &a, 7)
        );
        // 字符串边界不能含糊：["ab","c"] 与 ["a","bc"] 必须不同。
        let c = vec!["ab".to_owned(), "c".to_owned()];
        let d = vec!["a".to_owned(), "bc".to_owned()];
        assert_ne!(
            BuildFingerprint::of(FORMAT_VERSION, COMPILER_OPTIONS, &c, 7),
            BuildFingerprint::of(FORMAT_VERSION, COMPILER_OPTIONS, &d, 7),
        );
        // 源数据变了，指纹也要变。
        assert_ne!(
            fa,
            BuildFingerprint::of(FORMAT_VERSION, COMPILER_OPTIONS, &a, 8)
        );
    }

    #[test]
    fn body_checksum_is_order_sensitive() {
        let mut a = BodyChecksum::new();
        a.update(b"ab");
        a.update(b"c");
        let mut b = BodyChecksum::new();
        b.update(b"a");
        b.update(b"bc");
        assert_eq!(a.finish(), b.finish(), "同一串字节，切法不该影响结果");
        let mut c = BodyChecksum::new();
        c.update(b"acb");
        assert_ne!(a.finish(), c.finish(), "顺序变了必须不同");
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
            words_offset: 80,
            entries_offset: 180,
            index_offset: 300,
            build_fingerprint: 0,
            body_checksum: 0,
        };
        // (4+1)×4×2 + 9×2 = 40 + 18 = 58
        assert_eq!(h.index_size(), 58);
        assert_eq!(h.file_size(), 300 + 58);
    }

    #[test]
    fn absurd_counts_do_not_overflow() {
        let h = TableHeader {
            format_version: FORMAT_VERSION,
            source_checksum: 0,
            entry_count: u32::MAX,
            code_count: u32::MAX,
            word_bytes: u32::MAX,
            total_units: u32::MAX,
            words_offset: 80,
            entries_offset: u64::MAX,
            index_offset: u64::MAX,
            build_fingerprint: 0,
            body_checksum: 0,
        };
        // 旧实现会在 `code_count + 1` 处 debug 溢出 panic / release 静默 wrap。
        assert!(h.checked_index_size().is_some());
        assert!(
            h.checked_file_size().is_none(),
            "index_offset=u64::MAX 必须报溢出"
        );
        let e = validate_layout(&h, 0).unwrap_err();
        assert!(matches!(e, FormatError::Corrupt(_)), "{e:?}");
    }

    #[test]
    fn layout_validation_rejects_trailing_and_missing_bytes() {
        let mut h = TableHeader {
            format_version: FORMAT_VERSION,
            source_checksum: 0,
            entry_count: 1,
            code_count: 1,
            word_bytes: 4,
            total_units: 1,
            words_offset: 80,
            entries_offset: 84,
            index_offset: 96,
            build_fingerprint: 0,
            body_checksum: 0,
        };
        // 索引 = 2×4×2 + 1×2 = 18；文件总长 = 96 + 18 = 114。
        assert_eq!(h.file_size(), 114);
        assert!(validate_layout(&h, 114).is_ok());
        let e = validate_layout(&h, 110).unwrap_err();
        assert!(matches!(e, FormatError::Truncated { .. }), "{e:?}");
        let e = validate_layout(&h, 115).unwrap_err();
        assert!(matches!(e, FormatError::Corrupt(_)), "{e:?}");

        // 区段不衔接。
        h.entries_offset = 85;
        let e = validate_layout(&h, 114).unwrap_err();
        assert!(matches!(e, FormatError::Corrupt(_)), "{e:?}");
    }
}
