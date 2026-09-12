//! # stele-schemes — 方案资产与装载
//!
//! 中文职责：把 `.schema.yaml` + `.dict.yaml` 装载成引擎能用的方案；
//! 并内嵌一份默认方案，使 `stele` 在没有配置文件时也能用。
//! English role: load `.schema.yaml` + `.dict.yaml` into engine-ready schemes,
//! and embed a default set so `stele` works with no config files present.
//!
//! 架构位置：**方案资产**，与内核分属不同 crate（PLAN D24）。
//! 这一个 crate 的存在本身就是一条架构约束的可执行表达：
//! 内核（`stele-core` / `stele-engine`）里不允许出现"拼音 / 音节"这类词汇，
//! 而本 crate 里**必然**出现——于是它们在物理上必须分开。
//! CI 门禁 `scripts/verify-no-ime-vocab.sh` 持续检查这条边界。
//!
//! # 用法
//!
//! ```
//! use stele_core::{Engine, Key, KeyCode, Modifiers, NamedKey, Outcome};
//!
//! let engine = stele_engine::EngineImpl::new(&stele_schemes::minimal::all().unwrap()).unwrap();
//! let mut session = engine.create_session();
//! for c in "nihao".chars() {
//!     session.process_key(Key::ch(c));
//! }
//! assert_eq!(session.candidates()[0].text, "你好");
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod file;
pub mod minimal;

pub use file::{load_dir, load_scheme};
pub use minimal::{all, embedded, uses_both_translator_families, EmbeddedSource, EMBEDDED};
