//! # qingjian-config — 方案与词典的配置模型
//!
//! 中文职责：把 rime 风格的配置文件解析成**带行号**的值树，
//! 处理跨文件引用与分层补丁，并给出可读诊断。
//! English role: parse rime-style config files into a line-aware value tree,
//! handle cross-file references and layered patches, and report readable diagnostics.
//! 架构位置：`qingjian-core` 与 `qingjian-engine` 之间的配置层；
//! **内核不依赖它**——内核只接受已经编译好的方案数据。
//!
//! # 为什么手写解析器（而不是用 `serde`）
//!
//! 见 [`value`] 的模块文档：RIME 的方案文件有节点级的自定义指令、
//! 我们需要行号来做诊断、而且 map 的书写顺序参与语义。
//! 三条里任何一条单独成立都足够。
//!
//! # 分层用法
//!
//! ```
//! use qingjian_config::{parse, patch::{Registry, merge_into, MergeMode}};
//!
//! // ① 解析多份文件
//! let mut reg = Registry::new();
//! reg.insert("default", parse("punctuator:\n  half: x\n").unwrap());
//! reg.insert("mine", parse("punctuator:\n  $ref: \"default:/punctuator\"\nname: mine\n").unwrap());
//!
//! // ② 展开引用
//! let mut scheme = reg.expand("mine").unwrap();
//!
//! // ③ 叠加用户补丁（后层覆盖前层，未提及的键保留）
//! let user = parse("name: my-scheme\n").unwrap();
//! merge_into(&mut scheme, &user, MergeMode::Layer).unwrap();
//!
//! assert_eq!(scheme.get("name").unwrap().as_str().as_deref(), Some("my-scheme"));
//! assert!(scheme.get("punctuator").is_some(), "未提及的部分保留");
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod parse;
pub mod patch;
pub mod value;

pub use parse::{parse, parse_at, ParseError};
pub use patch::{lookup, merge_into, MergeMode, PatchError, Registry};
pub use value::{Node, Value};
