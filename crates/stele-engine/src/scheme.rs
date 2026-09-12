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
    /// 词条：`(编码单元文本序列, 词, 权重)`。
    pub entries: Vec<(Vec<&'static str>, &'static str, f64)>,
    /// 用哪族翻译器。
    pub translator: TranslatorKind,
    /// 候选总量上限。
    pub candidate_cap: usize,
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
        for (code, word, _) in &self.entries {
            for unit in code {
                if alphabet.id_of(unit).is_none() {
                    diagnostics.push(
                        stele_core::Diagnostic::new(
                            format!("scheme:{}", self.info.schema_id),
                            format!("词条引用了字母表里没有的编码单元「{unit}」"),
                        )
                        .with_entry((*word).to_owned()),
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

        let lexicon = InMemoryLexicon::from_entries(alphabet.clone(), &self.entries).map_err(
            |e: LexiconError| SchemaError::Invalid {
                schema_id: self.info.schema_id.clone(),
                diagnostics: vec![stele_core::Diagnostic::new(
                    format!("scheme:{}", self.info.schema_id),
                    e.to_string(),
                )],
            },
        )?;

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
            lexicon: Arc::new(lexicon),
            kind: self.translator,
            candidate_cap: self.candidate_cap,
        })
    }
}

/// 编译好的、可装载的方案。
#[derive(Debug)]
pub struct LoadedScheme {
    info: SchemaInfo,
    options: Options,
    tag: Tag,
    alphabet: CodeAlphabet,
    spelling: Option<Arc<SpellingTable>>,
    lexicon: Arc<InMemoryLexicon>,
    kind: TranslatorKind,
    candidate_cap: usize,
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

    /// 词库。
    #[must_use]
    pub fn lexicon(&self) -> Arc<InMemoryLexicon> {
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
                    Arc::clone(&self.lexicon) as Arc<dyn stele_core::Lexicon>,
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
                        Arc::clone(&self.lexicon) as Arc<dyn stele_core::Lexicon>,
                    )),
                    Box::new(EchoTranslator::new()),
                ]
            }
        };

        let filters: Vec<Box<dyn Filter>> = vec![Box::new(Uniquifier)];

        Box::new(PipelineImpl::new(
            self.tag,
            processors,
            translators,
            filters,
            Vec::new(),
            self.candidate_cap,
        ))
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
            entries: vec![(vec!["a", "b"], "十", 10.0)],
            translator,
            candidate_cap: CANDIDATE_CAP,
        }
    }

    #[test]
    fn compiles_both_translator_families_from_the_same_data() {
        for kind in [TranslatorKind::ExactCode, TranslatorKind::SpellingGraph] {
            let scheme = def(kind).compile().unwrap();
            assert_eq!(scheme.kind(), kind);
            assert_eq!(scheme.alphabet().len(), 2);
            assert_eq!(scheme.lexicon().len(), 1);
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
        d.entries.push((vec!["a", "z"], "坏词", 1.0)); // z 不在字母表里
        let err = d.compile().unwrap_err();
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
        d.entries.clear();
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
