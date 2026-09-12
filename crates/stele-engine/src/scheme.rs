//! # Scheme
//!
//! 中文职责：把**方案数据**编译成可装载的方案。
//! English role: compile scheme data into a loadable scheme.
//! 架构位置：`stele-core::LoadedSchema` 的实现；`stele-engine` 的入口。
//!
//! # 这是"内核与方案分离"的落点（PLAN D20 / D24）
//!
//! 一个方案的**全部个性**都在 [`SchemeDef`] 里：字母表、规则、词条、
//! 用哪族翻译器。引擎只负责把这些数据编译成机制。
//!
//! **P1 的方案定义是 Rust 数据**；`.schema.yaml` / `.dict.yaml` 的解析是
//! **P2** 的内容（届时 [`SchemeDef`] 从一个解析结果构造，这一层不变）。

use std::sync::Arc;
use stele_core::{
    CodeAlphabet, Filter, LoadedSchema, Options, Pipeline, Processor, SchemaError, SchemaInfo,
    Switch, Tag, Translator,
};

use crate::filter::Uniquifier;
use crate::lexicon::{InMemoryLexicon, LexiconError};
use crate::pipeline::PipelineImpl;
use crate::processor::{Editor, Selector, Speller};
use crate::spelling::{Rule, SpellingTable};
use crate::translator::{EchoTranslator, ExactCodeTranslator, SpellingGraphTranslator};

/// 当前方案格式版本（PLAN D27 的两级版本门禁之一）。
pub const SCHEME_FORMAT_VERSION: u32 = 1;

/// 词库从哪来。
#[non_exhaustive]
pub enum DictSource {
    /// 直接给出词条。
    ///
    /// 适合内嵌的小方案与测试——几十条词，怎么做都快。
    /// 用 [`entry`] 可以少写一堆 `.to_owned()`。
    Inline(Vec<(Vec<String>, String, f64)>),
    /// 一个**已经编译好**的词库实现。
    ///
    /// `stele-engine` 不知道它是内存表、紧凑二进制还是别的什么——
    /// 它只调用 `Lexicon::lookup`。
    External(Arc<dyn stele_core::Lexicon>),
}

impl core::fmt::Debug for DictSource {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Inline(v) => write!(f, "DictSource::Inline({} 条)", v.len()),
            Self::External(_) => write!(f, "DictSource::External(<编译好的词库>)"),
        }
    }
}

/// 方案用哪一族翻译器。
///
/// 选择权在**方案**，不在引擎——这是 D33 要验证的通用性。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TranslatorKind {
    /// **精确编码**：输入串当作一条完整编码直接查表。
    ///
    /// 适用于编码集合**不可枚举**的输入法（仓颉、五笔、英文…）。
    /// 不需要字母表以外的任何东西。
    ExactCode,
    /// **拼写图**：拼写展开成编码（含变体），再查词库。
    ///
    /// 适用于编码集合**可枚举**的输入法（拼音、双拼、注音…）。
    /// 需要字母表 + 规则。
    SpellingGraph,
}

/// 一份方案的声明（编译前的数据）。
#[derive(Debug)]
pub struct SchemeDef {
    /// 元数据。
    pub info: SchemaInfo,
    /// 方案声明的开关。
    pub switches: Vec<Switch>,
    /// 分段标签——翻译器据此绑定（G3）。
    pub tag: Tag,
    /// 编码字母表：拼音方案是音节表，字形方案是字母表。
    pub alphabet: Vec<String>,
    /// 拼写规则（[`TranslatorKind::ExactCode`] 时会忽略）。
    pub rules: Vec<Rule>,
    /// 词库从哪来。
    ///
    /// **引擎只认识这个枚举，不认识任何存储格式**——
    /// 紧凑二进制表、mmap、远程词库，都通过 [`DictSource::External`] 注入。
    /// 这正是 `Lexicon` 做成 trait 的兑现点（`docs/engine-design.md` §5）。
    pub dictionary: DictSource,
    /// 用哪族翻译器。
    pub translator: TranslatorKind,
    /// 候选总量上限。
    pub candidate_cap: usize,
    /// 预编辑串的音节分隔符（RIME 的 `speller.delimiter` 第一位）。
    pub preedit_delimiter: Option<char>,
    /// 方案装载器附带的自由信息（例如 RIME 风格 `engine:` 列表的覆盖报告）。
    ///
    /// **引擎不解释它的内容**——这是"装载器 → 工具链"的一条旁路，
    /// 用来让 `--dump-config` 之类的东西能报告装载细节，而不必让引擎认识它们。
    pub custom: std::collections::BTreeMap<String, String>,
}

/// 构造一个词条的便捷函数。
///
/// ```ignore
/// entries: vec![
///     entry(&["ni", "hao"], "你好", 10_000.0),
/// ]
/// ```
#[must_use]
pub fn entry(code: &[&str], word: &str, weight: f64) -> (Vec<String>, String, f64) {
    (
        code.iter().map(|s| (*s).to_owned()).collect(),
        word.to_owned(),
        weight,
    )
}

impl SchemeDef {
    /// 编译成可装载的方案。
    ///
    /// # Errors
    ///
    /// 词条引用了字母表里没有的编码单元、或字母表为空时返回
    /// [`SchemaError`]。**一次报出全部问题**，不是遇到第一个就返回。
    pub fn compile(&self) -> Result<LoadedScheme, SchemaError> {
        let mut diagnostics = Vec::new();

        if self.alphabet.is_empty() {
            diagnostics.push(stele_core::Diagnostic::new(
                format!("scheme:{}", self.info.schema_id),
                "字母表为空：没有任何编码单元可用",
            ));
        }

        let alphabet = CodeAlphabet::new(self.alphabet.clone());

        // 逐条检查词条引用的单元是否存在——一次报完所有问题。
        // 只有内联词条需要检查；外部词库在它自己的装载期已经校验过。
        let inline_entries: &[(Vec<String>, String, f64)] = match &self.dictionary {
            DictSource::Inline(v) => v,
            DictSource::External(_) => &[],
        };
        for (code, word, _) in inline_entries {
            for unit in code {
                if alphabet.id_of(unit).is_none() {
                    diagnostics.push(
                        stele_core::Diagnostic::new(
                            format!("scheme:{}", self.info.schema_id),
                            format!("词条引用了字母表里没有的编码单元「{unit}」"),
                        )
                        .with_entry(word.clone()),
                    );
                }
            }
        }

        if !diagnostics.is_empty() {
            return Err(SchemaError::Invalid {
                schema_id: self.info.schema_id.clone(),
                diagnostics,
            });
        }

        let lexicon: Arc<dyn stele_core::Lexicon> = match &self.dictionary {
            DictSource::Inline(v) => {
                Arc::new(InMemoryLexicon::from_entries(alphabet.clone(), v).map_err(
                    |e: LexiconError| SchemaError::Invalid {
                        schema_id: self.info.schema_id.clone(),
                        diagnostics: vec![stele_core::Diagnostic::new(
                            format!("scheme:{}", self.info.schema_id),
                            e.to_string(),
                        )],
                    },
                )?)
            }
            DictSource::External(l) => Arc::clone(l),
        };

        let spelling = match self.translator {
            TranslatorKind::ExactCode => None,
            TranslatorKind::SpellingGraph => Some(Arc::new(SpellingTable::compile(
                alphabet.clone(),
                &self.rules,
            ))),
        };

        let mut options = Options::new();
        for s in &self.switches {
            options.declare(s.clone());
        }

        Ok(LoadedScheme {
            info: self.info.clone(),
            options,
            tag: self.tag,
            alphabet,
            spelling,
            lexicon,
            kind: self.translator,
            candidate_cap: self.candidate_cap,
            preedit_delimiter: self.preedit_delimiter,
        })
    }
}

/// 编译好的、可装载的方案。
///
/// **不实现 `Debug`**：它持有 `Arc<dyn Lexicon>`，而词库实现没有
/// （也不该有）`Debug`。想调试就打印 [`LoadedScheme::info`]。
pub struct LoadedScheme {
    info: SchemaInfo,
    options: Options,
    tag: Tag,
    alphabet: CodeAlphabet,
    spelling: Option<Arc<SpellingTable>>,
    lexicon: Arc<dyn stele_core::Lexicon>,
    kind: TranslatorKind,
    candidate_cap: usize,
    preedit_delimiter: Option<char>,
}

impl LoadedScheme {
    /// 信息。
    #[must_use]
    pub fn info(&self) -> &SchemaInfo {
        &self.info
    }

    /// 开关。
    #[must_use]
    pub fn options(&self) -> &Options {
        &self.options
    }

    /// 字母表。
    #[must_use]
    pub fn alphabet(&self) -> &CodeAlphabet {
        &self.alphabet
    }

    /// 词库（**引擎只以 `Lexicon` 的身份使用它**）。
    #[must_use]
    pub fn lexicon(&self) -> Arc<dyn stele_core::Lexicon> {
        Arc::clone(&self.lexicon)
    }

    /// 用哪族翻译器。
    #[must_use]
    pub fn kind(&self) -> TranslatorKind {
        self.kind
    }

    /// 是否有拼写表。
    ///
    /// 精确编码方案**没有**它——这正是两族翻译器的形式差别。
    #[must_use]
    pub fn has_spelling_table(&self) -> bool {
        self.spelling.is_some()
    }

    /// 标签。
    #[must_use]
    pub fn tag(&self) -> Tag {
        self.tag
    }
}

impl LoadedSchema for LoadedScheme {
    fn info(&self) -> &SchemaInfo {
        &self.info
    }

    fn options(&self) -> &Options {
        &self.options
    }

    /// 装配一条流水线。
    ///
    /// **处理器是每会话一份**（它们可以有内部状态）；翻译器与过滤器共享
    /// 昂贵的资源（词库、拼写表都是 `Arc`），因此**装配本身很便宜**。
    fn build_pipeline(&self) -> Box<dyn Pipeline + Send> {
        let processors: Vec<Box<dyn Processor>> = vec![
            Box::new(Speller::default()),
            Box::new(Editor),
            Box::new(Selector),
        ];

        let translators: Vec<Box<dyn Translator>> = match self.kind {
            TranslatorKind::ExactCode => vec![
                Box::new(ExactCodeTranslator::new(
                    &self.alphabet,
                    Arc::clone(&self.lexicon),
                )),
                // 兜底永远在最后：查不到也要能上屏（G4）。
                Box::new(EchoTranslator::new()),
            ],
            TranslatorKind::SpellingGraph => {
                let spelling: Arc<dyn stele_core::Spelling> = self
                    .spelling
                    .clone()
                    .expect("SpellingGraph 方案必定有拼写表")
                    as Arc<dyn stele_core::Spelling>;
                vec![
                    Box::new(SpellingGraphTranslator::new(
                        spelling,
                        Arc::clone(&self.lexicon),
                    )),
                    Box::new(EchoTranslator::new()),
                ]
            }
        };

        let filters: Vec<Box<dyn Filter>> = vec![Box::new(Uniquifier)];

        let spelling: Option<Arc<dyn stele_core::Spelling>> = self
            .spelling
            .clone()
            .map(|s| s as Arc<dyn stele_core::Spelling>);

        Box::new(
            PipelineImpl::new(
                self.tag,
                processors,
                translators,
                filters,
                Vec::new(),
                self.candidate_cap,
            )
            .with_preedit(self.preedit_delimiter, spelling),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::CANDIDATE_CAP;
    use stele_core::{Key, LoadedSchema, Origin, ProcessResult, Span};

    fn def(translator: TranslatorKind) -> SchemeDef {
        SchemeDef {
            info: SchemaInfo {
                schema_id: "t".into(),
                name: "测试".into(),
                version: "0".into(),
                format_version: SCHEME_FORMAT_VERSION,
                family: None,
            },
            switches: vec![Switch::new("ascii_mode", false)],
            tag: "abc",
            alphabet: vec!["a".into(), "b".into()],
            rules: vec![],
            dictionary: DictSource::Inline(vec![entry(&["a", "b"], "十", 10.0)]),
            translator,
            candidate_cap: CANDIDATE_CAP,
            preedit_delimiter: None,
            custom: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn compiles_both_translator_families_from_the_same_data() {
        for kind in [TranslatorKind::ExactCode, TranslatorKind::SpellingGraph] {
            let scheme = def(kind).compile().unwrap();
            assert_eq!(scheme.kind(), kind);
            assert_eq!(scheme.alphabet().len(), 2);
        }
    }

    #[test]
    fn exact_code_scheme_needs_no_spelling_table() {
        let scheme = def(TranslatorKind::ExactCode).compile().unwrap();
        assert!(scheme.spelling.is_none());
        let mut p = scheme.build_pipeline();
        let mut state = stele_core::SessionState::default();

        assert_eq!(
            p.process_key(&mut state, &Key::ch('a')),
            ProcessResult::Accepted
        );
        p.process_key(&mut state, &Key::ch('b'));

        let mut out = vec![];
        p.compose(&mut state, &mut out);
        p.finalize(&mut out);
        assert_eq!(out[0].text, "十");
        assert_eq!(out[0].origin, Origin::SystemWord);
        assert_eq!(out[0].span, Span::new(0, 2));
    }

    #[test]
    fn inconsistent_scheme_data_fails_loudly_at_load() {
        let mut d = def(TranslatorKind::ExactCode);
        if let DictSource::Inline(v) = &mut d.dictionary {
            v.push(entry(&["a", "z"], "坏词", 1.0)); // z 不在字母表里
        }
        // `LoadedScheme` 持有 `Arc<dyn Lexicon>`，没有 `Debug`，
        // 所以这里 match 而不是 `unwrap_err()`。
        let Err(err) = d.compile() else {
            panic!("坏方案不该编译成功");
        };
        match err {
            SchemaError::Invalid { diagnostics, .. } => {
                assert_eq!(diagnostics.len(), 1);
                assert!(diagnostics[0].message.contains('z'));
            }
            other => panic!("应当是 Invalid，得到 {other:?}"),
        }
    }

    #[test]
    fn empty_alphabet_is_rejected() {
        let mut d = def(TranslatorKind::ExactCode);
        d.alphabet.clear();
        d.dictionary = DictSource::Inline(vec![]);
        assert!(d.compile().is_err());
    }

    #[test]
    fn switches_come_from_scheme_data_not_the_engine() {
        let scheme = def(TranslatorKind::ExactCode).compile().unwrap();
        assert!(!scheme.options().get("ascii_mode"));
        // 引擎不认识这个开关，但只要方案声明了它就存在。
        assert!(scheme.options().missing(&["ascii_mode"]).is_empty());
        assert_eq!(
            scheme.options().missing(&["emoji"]),
            vec!["emoji".to_owned()]
        );
    }
}
