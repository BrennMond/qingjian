//! # stele-table — 词库编译产物
//!
//! 中文职责：把词库编译成紧凑二进制，并以**低常驻内存**的方式查询它。
//! English role: compile a dictionary into a compact binary and query it with a
//! small resident footprint.
//! 架构位置：`stele-core::Lexicon` 的第二个实现（P2.5）。
//!
//! # 它在解决什么问题（实测数字）
//!
//! | 词条数 | 内存实现峰值 | 说明 |
//! | --- | --- | --- |
//! | 30 | 3 MiB | 内嵌默认方案 |
//! | 500,000 | **245 MiB** | 内存实现（`BTreeMap` + 每键一个 `Vec` + 每词条两个 `String`） |
//! | 1,880,000（雾凇规模，**外推**） | **约 0.9 GB** | 与 RIME 实测的 780 MB–1 GB 部署峰值同一量级 |
//!
//! 而项目的红线是**常驻 < 30 MB**、**部署峰值 < 150 MB**。
//! **也就是说：不换实现，我们恰好复现了 RIME 最糟糕的那个问题——
//! 而"内存比 RIME 小"正是这个项目存在的理由。**
//!
//! # 为什么不是 mmap
//!
//! `std` 里没有 mmap。用它要么引入 `memmap2`（本项目第一个第三方依赖），
//! 要么写 `unsafe`。而下面这个方案**零依赖、无 unsafe，内存效果相同**：
//!
//! ```text
//! 常驻内存（约 4 MB）              磁盘（约 36 MB）
//! ┌──────────────────────┐        ┌────────────────────┐
//! │ 编码索引             │        │ 词条数组            │
//! │  unit_offsets  [u32] │───────►│  (词偏移,长度,分数)  │
//! │  entry_offsets [u32] │        ├────────────────────┤
//! │  units         [u16] │        │ 词字符串表          │
//! └──────────────────────┘        └────────────────────┘
//!        ▲                                  ▲
//!        └── 二分查找，纯内存 ────────────────┘
//!                       按需 read_at（每次查询 2 次系统调用）
//! ```
//!
//! **关键**：索引小（几十万编码 × 几个字节）→ 常驻；词条与字符串大
//! （十几 MB）→ 留在文件里，只读查到的那些。
//!
//! 这带来一个与 mmap 相同的性质：**文件大小 ≠ 常驻内存**。
//! 文件页留在操作系统的页缓存里，**不计入本进程的 RSS**。
//!
//! **换回 mmap 是一次 crate 内部的改动**——因为 `Lexicon` 是 trait
//! （`docs/engine-design.md` §5）。这正是当初把它做成 trait 的兑现时刻。
//!
//! # 格式
//!
//! 全部小端。头部 80 字节，随后是三段：词字符串表、词条数组、索引。
//! **内容寻址**：文件名里带 [`BuildFingerprint`]（格式版本 + 编译选项 +
//! 字母表内容与顺序 + 源数据校验和），加载时校验（PLAN D28）。
//! 头部还带一份**主体校验和**用于发现意外损坏——它不是防篡改机制，
//! 真正的防越界靠 [`format::validate_layout`] 与读侧全程 checked 运算。
//!
//! # 三代缓存身份（为什么 v1 必须作废）
//!
//! | 版本 | 缓存身份含什么 | 后果 |
//! | --- | --- | --- |
//! | v1 | 只有源词典校验和 | 只改 `alphabet` 顺序就复用旧产物 → **静默错码** |
//! | v2 | + 格式版本、编译选项、字母表内容与顺序 | 语义输入变了必定重建 |
//!
//! v1 产物会被 [`FormatError::UnsupportedVersion`] 明确拒绝并提示重新部署。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod compile;
pub mod format;
pub mod lexicon;

pub use compile::{compile, CompileError, CompiledTable, TableWriter};
pub use format::{
    source_checksum, BuildFingerprint, FormatError, TableHeader, COMPILER_OPTIONS, FORMAT_VERSION,
    HEADER_SIZE, MAGIC,
};
pub use lexicon::TableLexicon;
