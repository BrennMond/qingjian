//! # Qingjian IME engine / 青简输入法引擎
//!
//! 中文职责：原生引擎——拼写层、词典、两族翻译器、处理器、过滤器，
//! 以及把方案数据编译成可装载方案的过程。
//! English role: the native engine — spelling layer, lexicon, the two translator
//! families, processors, filters, and the compilation of scheme data.
//!
//! 架构位置：`qingjian-core` 的**唯一实现**侧。本 crate **零第三方依赖**（PLAN D9），
//! 且**不含任何输入法专属知识**（PLAN D20）——拼音也好、仓颉也好，
//! 都只是 [`scheme::SchemeDef`] 里的数据。
//!
//! 权威设计文档：`docs/engine-design.md`。
//!
//! **本 crate 里没有任何方案数据**——连"内置方案"都是独立的 crate
//! （`qingjian-schemes-builtin`）。这不是洁癖：CI 门禁会检查内核里
//! 不出现"拼音 / 音节"这类词汇，而方案数据里必然出现它们。
//! 见 PLAN D20 / D24。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod calc;
pub mod engine;
pub mod filter;
pub mod inline;
pub mod keyspec;
pub mod lexicon;
pub mod pipeline;
pub mod presets;
pub mod processor;
pub mod punctuator;
pub mod regex;
pub mod registry;
pub mod scheme;
pub mod segmentor;
pub mod spec;
pub mod spelling;
pub mod tag;
pub mod translator;

pub use calc::{
    lua_number_to_string, replace_factorial, replace_percent, CalcError, CalcTranslator,
};
pub use engine::{EngineImpl, SessionImpl};
pub use filter::{Converter, ReverseLexicon, ReverseLookupFilter, Uniquifier};
pub use inline::{
    civil_from_days, civil_from_unix, derived_keys, is_english_word, month_day_zh, split_number,
    strip_punct, year_zh, AutoCapFilter, CivilTime, DateTranslator, LongWordFilter,
    NumberTranslator, PinCandFilter, PinTable, ReduceEnglishFilter, UnicodeTranslator,
    UuidTranslator, VFilter,
};
pub use keyspec::{key_code_name, key_for_char, parse_key_name, KeyChord};
pub use lexicon::{Entry, InMemoryLexicon, LexiconError, TextIndex};
pub use pipeline::{PipelineImpl, CANDIDATE_CAP, DEFAULT_PAGE_SIZE};
pub use presets::{Preset, PRESET_QINGJIAN};
pub use processor::{AsciiComposer, Editor, KeyBinder, Navigator, Selector, Speller};
pub use punctuator::{literal_pending, PunctTranslator, Punctuator};
pub use regex::{Regex, RegexError};
pub use registry::{unmet_requirements, Availability, CoverageReport, ExternalData, Slot};
pub use scheme::{LoadedScheme, SchemeDef, TranslatorKind, SCHEME_FORMAT_VERSION};
pub use segmentor::{
    AffixSegmentor, CodingSegmentor, InputScan, Matcher, Recognizer, RecognizerError,
    SymbolSegmentor,
};
pub use spec::{
    split_alias, AffixSpec, At, AutoCapSpec, CalcSpec, DateSpec, EditorAction, EditorBinding,
    EngineSpec, KeyBinding, LongWordSpec, NavigatorSpec, NotApplicableSpec, NumberSpec,
    PinCandSpec, PinEntry, RecogPattern, RecognizerSpec, ReduceEnglishSpec, ReduceMode,
    ReverseLookupSpec, SimplifierSpec, TipsMode, TranslatorKindSpec, TranslatorSpec, UnicodeSpec,
    UuidSpec, WhenPredicate,
};
pub use spelling::{FormatRule, Rule, SpellingFormat, SpellingTable};
pub use tag::TagTable;
pub use translator::{
    EchoTranslator, ExactCodeTranslator, SpellingGraphTranslator, TaggedFilter, TaggedTranslator,
    COMPLETION_COST, TRANSLATE_CAP,
};
