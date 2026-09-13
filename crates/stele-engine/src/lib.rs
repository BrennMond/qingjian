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

pub use calc::{CalcError, CalcTranslator, lua_number_to_string, replace_factorial, replace_percent};
pub use engine::{EngineImpl, SessionImpl};
pub use filter::{Converter, ReverseLexicon, ReverseLookupFilter, Uniquifier};
pub use inline::{
    AutoCapFilter, CivilTime, DateTranslator, LongWordFilter, PinCandFilter, PinTable,
    NumberTranslator, ReduceEnglishFilter, UnicodeTranslator, UuidTranslator, VFilter,
    civil_from_days, civil_from_unix, derived_keys, is_english_word, month_day_zh, split_number,
    strip_punct, year_zh,
};
pub use keyspec::{KeyChord, key_code_name, key_for_char, parse_key_name};
pub use lexicon::{Entry, InMemoryLexicon, LexiconError, TextIndex};
pub use pipeline::{PipelineImpl, CANDIDATE_CAP, DEFAULT_PAGE_SIZE};
pub use processor::{AsciiComposer, Editor, KeyBinder, Navigator, Selector, Speller};
pub use presets::{PRESET_STELE, Preset};
pub use punctuator::{PunctTranslator, Punctuator, literal_pending};
pub use regex::{Regex, RegexError};
pub use registry::{Availability, CoverageReport, ExternalData, Slot, unmet_requirements};
pub use scheme::{LoadedScheme, SchemeDef, TranslatorKind, SCHEME_FORMAT_VERSION};
pub use segmentor::{
    AffixSegmentor, CodingSegmentor, InputScan, Matcher, Recognizer, RecognizerError,
    SymbolSegmentor,
};
pub use spec::{
    AffixSpec, At, EditorAction, EditorBinding, EngineSpec, KeyBinding, NavigatorSpec,
    AutoCapSpec, CalcSpec, DateSpec, LongWordSpec, NotApplicableSpec, NumberSpec, PinCandSpec,
    PinEntry,
    ReduceEnglishSpec,
    ReduceMode, RecogPattern, RecognizerSpec, ReverseLookupSpec, SimplifierSpec, TipsMode,
    TranslatorKindSpec, TranslatorSpec, UnicodeSpec, UuidSpec, WhenPredicate, split_alias,
};
pub use spelling::{FormatRule, Rule, SpellingFormat, SpellingTable};
pub use tag::TagTable;
pub use translator::{
    COMPLETION_COST, EchoTranslator, ExactCodeTranslator, SpellingGraphTranslator, TaggedFilter,
    TaggedTranslator, TRANSLATE_CAP,
};
