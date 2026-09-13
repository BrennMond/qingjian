//! # Stele-IME core / 石经内核抽象层
//!
//! 中文职责：定义引擎的一切抽象——`Engine` / `Session` 两级对象、五类骨架组件的
//! trait、外部服务的 trait，以及贯穿全局的数据结构。
//! English role: the abstraction layer of the Stele input-method engine —
//! the `Engine`/`Session` split, the component traits, the service traits,
//! and the shared data structures.
//!
//! 架构位置：本 crate **零第三方依赖**（PLAN D9），且**不含任何输入法专属知识**
//! （PLAN D20）——不允许出现"拼音 / 音节 / 简拼 / 模糊音"之类的概念。
//! 一切输入法的个性都属于**方案数据**（PLAN D24）。
//!
//! 权威设计文档：`docs/engine-design.md`。
//!
//! # 四条不可动摇的东西
//!
//! 1. **可复现**：候选列表是 (输入, 状态) 的纯函数（PLAN §5.2）。
//!    这靠 [`Score`] 的定点整数表示在类型层面保证——整数加法精确，
//!    且**不存在 NaN**，因此跨平台逐位一致。
//! 2. **候选封闭**：候选文本只能来自可枚举的来源，禁止自由文本生成（PLAN §5.3）。
//! 3. **精确优先**：重排器不得把"猜出来的"候选顶到"不是猜的"候选之前（PLAN §5.4）。
//! 4. **通用性**：内核只认识"编码 / 拼写 / 编码单元 / 字母表"，
//!    不认识任何具体输入法（PLAN D20）。
//!
//! ```
//! use stele_core::{is_exact, Origin, Score, SpellingAttr};
//!
//! // 对数域 + 定点整数：权重 1000 → ln(1000) ≈ 6.908
//! assert_eq!(Score::from_weight(1000.0).as_milli_log(), 6908);
//!
//! // 精确优先的判据由两个轴共同决定
//! assert!(is_exact(Origin::SystemWord, SpellingAttr::NORMAL));
//! assert!(!is_exact(Origin::SystemWord, SpellingAttr::ABBREV));
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod candidate;
pub mod commit;
pub mod component;
pub mod context;
pub mod error;
pub mod key;
pub mod option;
pub mod score;
pub mod segment;
pub mod segmentor;
pub mod service;
pub mod session;
pub mod sort;

pub use candidate::{
    is_exact, Candidate, CandidateKind, CandidateSink, Lane, Origin, Span, SpellingAttr,
};
pub use commit::{Commit, Event, Outcome, PendingCommit, ProcessResult, SelectionSource, Trigger};
pub use component::{
    literal_candidate, Fallback, Filter, Formatter, Processor, Query, Segmentor, Translator,
    TranslatorDeps,
};
pub use context::Context;
pub use error::{Diagnostic, Result, SchemaError, SteleError};
pub use key::{Key, KeyCode, Modifiers, NamedKey};
pub use option::{Options, Switch};
pub use score::Score;
pub use segment::{Composition, Segment, SegmentStatus, Segmentation, Tag};
pub use segmentor::{Claim, InputScanView};
pub use service::{
    Clock, CodeAlphabet, CodeUnitId, DeterministicRandom, EmptyLexicon, Expansion, ExpansionSink,
    FrozenClock, Lexicon, LiteralSpelling, MemoryEntry, MemoryStore, NoMemory, Prediction,
    PredictionOrigin, QueryView, RandomSource, Ranker, Spelling,
};
pub use session::{
    Engine, LoadedSchema, Pipeline, SchemaCatalog, SchemaInfo, Session, SessionState,
};
pub use sort::{
    clamp_bonus, compare, count_lane, has_cross_class_inversion, origin_rank, sort_candidates,
};
