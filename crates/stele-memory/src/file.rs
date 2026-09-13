//! # File format — 用户记忆的紧凑二进制
//!
//! 中文职责：把两张表（`(输入, 词)` 与 `(上下文, 词)`）编码成一个**自校验**的文件。
//! English role: encode the two memory tables into a single self-checking binary file.
//! 架构位置：`stele-memory` 的存储层，只有 `store` 用它。
//!
//! # 为什么自写格式（D9 的欠账与 P4a 的选型）
//!
//! PLAN §2.2 原本写的是 SQLite。但引入第一个第三方依赖之前，
//! PLAN §4.8 要求的"依赖许可审查机制"**必须先存在**（那条欠账在 P4a 的第 0 步补上了），
//! 而我们真正需要的只是一个"**按键时零 I/O、启动时读一次**的有序表"——
//! 那正是 `stele-table` 已经解决过的形状。所以格式自写：零依赖、无 `unsafe`、
//! 跨端复用同一份实现。
//!
//! # 布局（全部小端）
//!
//! ```text
//! magic   8 字节  "STELEMEM"
//! version u32     = 2
//! count   u32     输入表记录条数
//! 记录 × count：
//!     input_len  u32   input  UTF-8 字节数
//!     input      ...
//!     text_len   u32
//!     text       ...
//!     count      u32   累计上屏次数
//!     decayed    u64   已衰减的累计频次（千分之一单位）
//!     last_used  u64   Unix 秒
//! pcount  u32     预测表记录条数
//! 预测记录 × pcount：
//!     ctx1_len   u32   ctx1（trigram 的较前那个词；bigram 时为空串）
//!     ctx1       ...
//!     ctx2_len   u32   ctx2（最近的那个词）
//!     ctx2       ...
//!     text_len   u32   预测出来的词
//!     text       ...
//!     count      u32
//!     decayed    u64
//!     last_used  u64
//! checksum u64    前面**全部字节**的 FNV-1a
//! ```
//!
//! # 为什么两张表在**同一个文件里分段**，而不是各写一个文件
//!
//! HANDOFF §7.7.3 第 2 步给的两条路是"另开一份文件"或"在同一份文件里分段"。
//! 选后者，理由是一致性：**两份表必须一起原子替换**。分两个文件时，
//! "输入表写成功、预测表写失败"会留下一个自相矛盾的状态，
//! 而它没有任何自校验能发现（两个文件各自都是好的）。
//! 分段 + 一个覆盖全篇的校验和，让"半个状态"在结构上不可能存在。
//!
//! **两段之间没有任何共享的键空间**：输入表的第一列是**编码**
//! （PLAN D42，如 `ni'hao`），预测表的第一/二列是**已上屏的词文本**
//! （如 `微信`）。混在一张表里会让两类记录互相污染（HANDOFF §7.7.4 第 2 条）。
//!
//! # 三条刻意写下来的约定
//!
//! 1. **两段都按各自的主键升序写出**。调用方（`store`）用 `BTreeMap`，
//!    因此同一份数据两次落盘**逐字节相同**——这让"文件是状态的纯函数"
//!    成为可断言的性质，而不是巧合。
//! 2. **校验和覆盖头部**。只校验正文的话，改了 `version` 或 `count`
//!    反而不会被发现（那正是最需要被发现的两处）。
//! 3. **长度字段防的是"读到一半"**。按长度逐段推进并检查越界，
//!    因此截断的文件报错而不是读出垃圾。坏文件的处置见 `store::open_or_degrade`。

/// 文件魔数。
pub(crate) const MAGIC: &[u8; 8] = b"STELEMEM";

/// 格式版本。**不匹配即拒绝**（PLAN D28 的同一条精神：用错版本会静默给错结果）。
///
/// # 为什么是 2
///
/// P4a 的版本 1 只有输入表一段。P4b 加入预测表时**必须**升版本：
/// 用旧结构去读新文件会读到一段"看起来是记录、其实是预测表"的字节，
/// 而校验和是好的（它不知道我们怎么解释这些字节）。**版本号是这里唯一
/// 能拦住"结构错了但字节没坏"的东西。**
pub(crate) const VERSION: u32 = 2;

/// 磁盘上的一条**输入表**记录（内存里按 `(input, text)` 索引）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    /// 规范化之后的输入串。
    pub input: String,
    /// 上屏文本。
    pub text: String,
    /// 累计上屏次数。
    pub count: u32,
    /// 已衰减的累计频次（千分之一单位）。
    pub decayed_milli: u64,
    /// 最后一次使用时间（Unix 秒）。
    pub last_used: u64,
}

/// 磁盘上的一条**预测表**记录（内存里按 `(ctx1, ctx2, text)` 索引）。
///
/// `ctx1` 为空串表示这是一条 **bigram** 记录（只用了最近一个词）。
/// 上下文里的词不可能是空串（`Context::push` 明确丢弃空串），
/// 因此空串在这里是**保留值**，不会与真实上下文撞车。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PredRecord {
    /// 较前的那一个上下文词（bigram 时为空串）。
    pub ctx1: String,
    /// 最近的那一个上下文词。
    pub ctx2: String,
    /// 预测出来的词。
    pub text: String,
    /// 累计上屏次数。
    pub count: u32,
    /// 已衰减的累计频次（千分之一单位）。
    pub decayed_milli: u64,
    /// 最后一次使用时间（Unix 秒）。
    pub last_used: u64,
}

/// 解析失败的原因。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FormatError {
    /// 文件太短，连头部都不完整。
    Truncated,
    /// 魔数不对。
    NotMemoryFile,
    /// 版本不支持。
    UnsupportedVersion(u32),
    /// 校验和不符（文件被改坏或写了一半）。
    ChecksumMismatch { expected: u64, found: u64 },
    /// 文本不是合法 UTF-8。
    BadText,
}

impl core::fmt::Display for FormatError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Truncated => write!(f, "文件被截断"),
            Self::NotMemoryFile => write!(f, "不是用户记忆文件（魔数不符）"),
            Self::UnsupportedVersion(v) => {
                write!(f, "格式版本 {v} 不受支持（本程序只认 {VERSION}）")
            }
            Self::ChecksumMismatch { expected, found } => {
                write!(f, "校验和不符（记录 {expected:#x}，实得 {found:#x}）")
            }
            Self::BadText => write!(f, "文本不是合法 UTF-8"),
        }
    }
}

/// FNV-1a 64 位。**它不是密码学哈希**，用途只有一个：发现文件被截断或改坏。
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    // 记录长度是 u32：单条 `input`/`text` 不可能接近 4 GB。
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(bytes);
}

fn put_entry_tail(out: &mut Vec<u8>, count: u32, decayed_milli: u64, last_used: u64) {
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&decayed_milli.to_le_bytes());
    out.extend_from_slice(&last_used.to_le_bytes());
}

/// 把两张表编码成字节。
///
/// `records` **必须已按 `(input, text)` 升序**，
/// `predictions` **必须已按 `(ctx1, ctx2, text)` 升序**——
/// 落盘的确定性靠这条前提，而它由调用方的 `BTreeMap` 保证。
pub(crate) fn encode(records: &[Record], predictions: &[PredRecord]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity((records.len() + predictions.len()) * 40 + 32);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    let count = u32::try_from(records.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&count.to_le_bytes());
    for r in records {
        put_bytes(&mut out, r.input.as_bytes());
        put_bytes(&mut out, r.text.as_bytes());
        put_entry_tail(&mut out, r.count, r.decayed_milli, r.last_used);
    }
    let pcount = u32::try_from(predictions.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&pcount.to_le_bytes());
    for p in predictions {
        put_bytes(&mut out, p.ctx1.as_bytes());
        put_bytes(&mut out, p.ctx2.as_bytes());
        put_bytes(&mut out, p.text.as_bytes());
        put_entry_tail(&mut out, p.count, p.decayed_milli, p.last_used);
    }
    let sum = fnv1a(&out);
    out.extend_from_slice(&sum.to_le_bytes());
    out
}

/// 一个只向前走的字节游标。所有越界都变成 [`FormatError::Truncated`]。
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], FormatError> {
        let end = self.pos.checked_add(n).ok_or(FormatError::Truncated)?;
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or(FormatError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    fn u32(&mut self) -> Result<u32, FormatError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, FormatError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn len_prefixed_str(&mut self) -> Result<String, FormatError> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        core::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| FormatError::BadText)
    }
}

/// 解析字节。**任何不一致都报错**，绝不"尽量读出来"——
/// 一个读了一半的记忆表会产生"有的词有时出现有时不出现"，比没有记忆更难查。
///
/// 返回 `(输入表, 预测表)`。
pub(crate) fn decode(bytes: &[u8]) -> Result<(Vec<Record>, Vec<PredRecord>), FormatError> {
    // ① 先看魔数。它对"这根本不是我们的文件"给出的诊断最有用，
    //    而校验和对那种文件只会说"校验和不符"（读的人无从下手）。
    if bytes.len() < MAGIC.len() {
        return Err(FormatError::Truncated);
    }
    if &bytes[..MAGIC.len()] != MAGIC.as_slice() {
        return Err(FormatError::NotMemoryFile);
    }

    // ② 再校验和：它覆盖**除自己以外的全部字节**（含头部）。
    //    只校验正文的话，改了 `version` 或 `count` 反而不会被发现。
    let sum_at = bytes.len().checked_sub(8).ok_or(FormatError::Truncated)?;
    let (body, tail) = bytes.split_at(sum_at);
    let found = u64::from_le_bytes([
        tail[0], tail[1], tail[2], tail[3], tail[4], tail[5], tail[6], tail[7],
    ]);
    let expected = fnv1a(body);
    if found != expected {
        return Err(FormatError::ChecksumMismatch { expected, found });
    }

    // ③ 结构：逐段按长度推进，越界即报错。
    let mut c = Cursor::new(body);
    let _ = c.take(8)?;
    let version = c.u32()?;
    if version != VERSION {
        return Err(FormatError::UnsupportedVersion(version));
    }

    let count = c.u32()? as usize;
    // 上限只是防御"count 被改成一个天文数字"时的巨额预分配；
    // 真正的越界由游标在读到一半时报出来。
    let mut out = Vec::with_capacity(count.min(1 << 20));
    for _ in 0..count {
        let input = c.len_prefixed_str()?;
        let text = c.len_prefixed_str()?;
        let count = c.u32()?;
        let decayed_milli = c.u64()?;
        let last_used = c.u64()?;
        out.push(Record {
            input,
            text,
            count,
            decayed_milli,
            last_used,
        });
    }

    let pcount = c.u32()? as usize;
    let mut preds = Vec::with_capacity(pcount.min(1 << 20));
    for _ in 0..pcount {
        let ctx1 = c.len_prefixed_str()?;
        let ctx2 = c.len_prefixed_str()?;
        let text = c.len_prefixed_str()?;
        let count = c.u32()?;
        let decayed_milli = c.u64()?;
        let last_used = c.u64()?;
        preds.push(PredRecord {
            ctx1,
            ctx2,
            text,
            count,
            decayed_milli,
            last_used,
        });
    }

    if c.pos != body.len() {
        // 尾部有多余字节：说明写它的人和我们理解的结构不一致。
        return Err(FormatError::Truncated);
    }
    Ok((out, preds))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Record> {
        vec![
            Record {
                input: "nihao".into(),
                text: "你好".into(),
                count: 3,
                decayed_milli: 2_500,
                last_used: 1_767_225_600,
            },
            Record {
                input: "shijie".into(),
                text: "世界".into(),
                count: 1,
                decayed_milli: 1_000,
                last_used: 1_767_225_000,
            },
        ]
    }

    fn sample_preds() -> Vec<PredRecord> {
        vec![
            // bigram：ctx1 为空串。
            PredRecord {
                ctx1: String::new(),
                ctx2: "微信".into(),
                text: "朋友圈".into(),
                count: 4,
                decayed_milli: 3_200,
                last_used: 1_767_225_600,
            },
            // trigram。
            PredRecord {
                ctx1: "今天".into(),
                ctx2: "微信".into(),
                text: "朋友圈".into(),
                count: 2,
                decayed_milli: 1_800,
                last_used: 1_767_225_500,
            },
        ]
    }

    #[test]
    fn round_trip_is_exact() {
        let recs = sample();
        let preds = sample_preds();
        let bytes = encode(&recs, &preds);
        assert_eq!(decode(&bytes).unwrap(), (recs, preds));
    }

    #[test]
    fn encoding_is_a_pure_function_of_the_records() {
        // 同一份数据两次编码逐字节相同 —— 落盘因此是确定性的。
        assert_eq!(
            encode(&sample(), &sample_preds()),
            encode(&sample(), &sample_preds())
        );
    }

    #[test]
    fn empty_tables_round_trip() {
        let bytes = encode(&[], &[]);
        assert_eq!(decode(&bytes).unwrap(), (Vec::new(), Vec::new()));
    }

    #[test]
    fn the_two_sections_do_not_share_a_key_space() {
        // 输入表的一段与预测表的一段长度相同，但**内容分属两处**：
        // 同一条文本在两段里各出现一次，各自独立。
        // 这条测试守的是"分段真的分开了"——若实现把它们写进同一段，
        // 这里读出来的条数会不对。
        let recs = sample();
        let preds = sample_preds();
        let (got_recs, got_preds) = decode(&encode(&recs, &preds)).unwrap();
        assert_eq!(got_recs.len(), 2);
        assert_eq!(got_preds.len(), 2);
        // 预测表里那条 bigram 的 ctx1 必须**原样**是空串，而不是被写成别的。
        assert_eq!(got_preds[0].ctx1, "");
    }

    #[test]
    fn rejects_a_truncated_file() {
        let bytes = encode(&sample(), &sample_preds());
        for cut in [0, 1, 7, 8, 12, bytes.len() - 1] {
            assert!(decode(&bytes[..cut]).is_err(), "截断到 {cut} 字节应当报错");
        }
    }

    #[test]
    fn rejects_a_flipped_byte() {
        let mut bytes = encode(&sample(), &sample_preds());
        // 翻正文里的一个字节（不是校验和本身）。
        bytes[20] ^= 0x01;
        assert!(matches!(
            decode(&bytes),
            Err(FormatError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn rejects_a_wrong_magic() {
        let mut bytes = encode(&[], &[]);
        bytes[0] = b'X';
        assert!(decode(&bytes).is_err());
    }

    #[test]
    fn rejects_the_p4a_version_one_layout() {
        // **版本门禁的存在理由**：P4a 的 v1 文件没有预测段，
        // 用 v2 的结构去读它会把校验和之后的字节当成 `pcount`。
        // 校验和是好的（它不知道我们怎么解释这些字节），
        // 因此**只有版本号能拦住"结构错了但字节没坏"**。
        let mut body = Vec::new();
        body.extend_from_slice(MAGIC);
        body.extend_from_slice(&1_u32.to_le_bytes());
        body.extend_from_slice(&0_u32.to_le_bytes());
        let sum = fnv1a(&body);
        body.extend_from_slice(&sum.to_le_bytes());
        assert_eq!(decode(&body), Err(FormatError::UnsupportedVersion(1)));
    }

    #[test]
    fn rejects_an_unsupported_version() {
        // 改版本号必须让校验和也失配，因此这里同时重算校验和，
        // 以便单独测"版本门禁"这条（否则测的是校验和）。
        let mut body = Vec::new();
        body.extend_from_slice(MAGIC);
        body.extend_from_slice(&99_u32.to_le_bytes());
        body.extend_from_slice(&0_u32.to_le_bytes());
        let sum = fnv1a(&body);
        body.extend_from_slice(&sum.to_le_bytes());
        assert_eq!(decode(&body), Err(FormatError::UnsupportedVersion(99)));
    }

    #[test]
    fn rejects_trailing_garbage() {
        let mut body = encode(&[], &[]);
        // 去掉校验和，加一个多余字节，再补一个正确的校验和。
        body.truncate(body.len() - 8);
        body.push(0xAB);
        let sum = fnv1a(&body);
        body.extend_from_slice(&sum.to_le_bytes());
        assert!(decode(&body).is_err(), "尾部多余字节必须被报出来");
    }
}
