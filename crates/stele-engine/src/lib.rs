//! # Stele-IME engine / 石经引擎
//!
//! 中文职责：原生引擎——拼写层、词典、两族翻译器、处理器、过滤器，
//! 以及把方案数据编译成可装载方案的过程。
//! English role: the native engine — spelling layer, lexicon, the two translator
//! families, processors, filters, and the compilation of scheme data.
//!
//! 架构位置：`stele-core` 的**唯一实现**侧。本 crate **零第三方依赖**（PLAN D9），
//! 且**不含任何输入法专属知识**（PLAN D20）——拼音也好、仓颉也好，
//! 都只是 [`scheme::SchemeDef`] 里的数据。
//!
//! 权威设计文档：`docs/engine-design.md`。
//!
//! **本 crate 里没有任何方案数据**——连"内置方案"都是独立的 crate
//! （`stele-schemes-builtin`）。这不是洁癖：CI 门禁会检查内核里
//! 不出现"拼音 / 音节"这类词汇，而方案数据里必然出现它们。
//! 见 PLAN D20 / D24。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod engine;
pub mod filter;
pub mod lexicon;
pub mod pipeline;
pub mod processor;
pub mod regex;
pub mod scheme;
pub mod spelling;
pub mod translator;

pub use engine::{EngineImpl, SessionImpl};
pub use filter::Uniquifier;
pub use lexicon::{Entry, InMemoryLexicon, LexiconError};
pub use pipeline::{PipelineImpl, CANDIDATE_CAP};
pub use processor::{Editor, Selector, Speller};
pub use regex::{Regex, RegexError};
pub use scheme::{LoadedScheme, SchemeDef, TranslatorKind, SCHEME_FORMAT_VERSION};
pub use spelling::{Rule, SpellingTable};
pub use translator::{
    EchoTranslator, ExactCodeTranslator, SpellingGraphTranslator, TaggedTranslator, TRANSLATE_CAP,
};
