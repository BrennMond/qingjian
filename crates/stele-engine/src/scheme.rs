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
use crate::processor::{AsciiComposer, Editor, KeyBinder, Navigator, Selector, Speller};
use crate::punctuator::{PunctTranslator, Punctuator};
use crate::segmentor::Recognizer;
use crate::spelling::{Rule, SpellingTable};
use crate::translator::{
    EchoTranslator, ExactCodeTranslator, SpellingGraphTranslator, TaggedFilter, TaggedTranslator,
};

/// 当前方案格式版本（PLAN D27 的两级版本门禁之一）。
pub const SCHEME_FORMAT_VERSION: u32 = 1;

/// 词库从哪来。
///
/// `Default` 是"空的内联词条"——一个方案可以没有任何词（纯转换方案、
/// 纯符号方案）。它让 [`SchemeDef::default`] 能存在，而后者让
/// "只关心某几个字段"的构造与测试不必写二十行样板。
#[non_exhaustive]
#[derive(Clone)]
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

impl Default for DictSource {
    /// 默认是**空的内联词条**：一个方案可以没有任何词
    /// （纯符号方案、纯转换方案）。
    fn default() -> Self {
        Self::Inline(Vec::new())
    }
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
///
/// 默认是 [`TranslatorKind::ExactCode`]：它是"什么都不要"的那一族
/// （不需要字母表以外的任何东西），因此是最保守的默认。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TranslatorKind {
    /// **精确编码**：输入串当作一条完整编码直接查表。
    ///
    /// 适用于编码集合**不可枚举**的输入法（仓颉、五笔、英文…）。
    /// 不需要字母表以外的任何东西。
    #[default]
    ExactCode,
    /// **拼写图**：拼写展开成编码（含变体），再查词库。
    ///
    /// 适用于编码集合**可枚举**的输入法（拼音、双拼、注音…）。
    /// 需要字母表 + 规则。
    SpellingGraph,
}

/// 一份方案的声明（编译前的数据）。
///
/// 字段很多，但**分成两类**：一半是"引擎的机制需要什么"
/// （字母表、规则、词库、翻译器族），另一半是"方案声明了哪些零件"
/// （`engine:` 段与各零件的配置）。后者全部有默认值——
/// 一个最小的方案只需要前一半就能跑起来，这正是 P1/P2 既有方案的情形。
#[derive(Clone, Debug, Default)]
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
    /// 预编辑串的编码单元分隔符（RIME 的 `speller.delimiter` 第一位）。
    pub preedit_delimiter: Option<char>,
    /// `engine:` 段声明的零件（为空则按既有的"按翻译器族自动装配"）。
    pub engine: crate::spec::EngineSpec,
    /// 识别器配置。
    pub recognizer: crate::spec::RecognizerSpec,
    /// 标点配置。
    pub punctuator: crate::spec::PunctuatorSpec,
    /// 编辑器的按键绑定。
    pub editor_bindings: Vec<(crate::spec::KeyChord, crate::spec::EditorAction)>,
    /// 按键重绑定。
    pub key_bindings: Vec<crate::spec::KeyBinding>,
    /// 翻页键。
    pub navigator: crate::spec::NavigatorSpec,
    /// 带词缀的切分器（按别名索引）。
    pub affixes: Vec<(String, crate::spec::AffixSpec)>,
    /// 反查滤镜（按别名索引）。
    pub reverse_lookups: Vec<(String, crate::spec::ReverseLookupSpec)>,
    /// 转换滤镜（按别名索引）：`(别名, 转换表, 配置)`。
    pub converters: Vec<(
        String,
        std::collections::BTreeMap<String, Vec<String>>,
        crate::spec::SimplifierSpec,
    )>,
    /// 表驱动翻译器（按别名索引）。
    pub translator_specs: Vec<(String, crate::spec::TranslatorSpec)>,
    /// `speller.alphabet` 里声明的字符（用于限制输入）。
    pub input_alphabet: Vec<char>,
    /// 每页候选数（显示参数）。
    pub page_size: usize,
    /// **外部数据就位判据**（由装载器给出）。
    ///
    /// 每个元素的含义是"`alias` 这个实例要的数据，装载器已经找到了"。
    /// 例如 `simplifier@fanti` 带了内联 `table`、或 `emoji.json` 读到了，
    /// 装载器就填 `ExternalData { alias: "fanti", present: true }`。
    ///
    /// **为什么必须由装载器填**：只有它知道文件在不在。引擎能做的只是
    /// "按这个判据决定要不要出声"——判据与事实分开，才能让
    /// "注册表说需要数据"与"这份方案缺不缺"是两个可分别验证的命题。
    pub external_data: Vec<crate::registry::ExternalData<'static>>,
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
    /// 复制一份交给引擎。
    ///
    /// **存在的理由很具体**：装载器给出的 [`SchemeDef`] 还带着"它是怎么被
    /// 合出来的"（来源表），而引擎只需要方案本身。让引擎侧持有装载器的
    /// 类型等于把"配置分层"这个概念漏进内核——那正是 D24 要挡住的东西。
    /// 因此边界上**显式复制一次**，而这个方法的签名就是那条边界。
    ///
    /// 复制成本只在装载期付一次（几十条词的 `Vec` 或一个 `Arc` 的克隆）。
    #[must_use]
    pub fn for_engine(&self) -> Self {
        self.clone()
    }

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

        // 方案声明了配置、却没有任何东西会用到它 —— 这是一处
        // **永远不生效的配置**，必须在装载期报出来（PLAN D17 的反面教材
        // 正是"写错的字段被默默忽略"）。
        diagnostics.extend(self.check_declared_components());

        if !diagnostics.is_empty() {
            return Err(SchemaError::Invalid {
                schema_id: self.info.schema_id.clone(),
                diagnostics,
            });
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
            engine: self.engine.clone(),
            recognizer: self.recognizer.clone(),
            punctuator: self.punctuator.clone(),
            editor_bindings: self.editor_bindings.clone(),
            key_bindings: self.key_bindings.clone(),
            navigator: self.navigator.clone(),
            affixes: self.affixes.clone(),
            reverse_lookups: self.reverse_lookups.clone(),
            converters: self.converters.clone(),
            translator_specs: self.translator_specs.clone(),
            input_alphabet: self.input_alphabet.clone(),
            page_size: self.page_size,
            external_data: self.external_data.clone(),
        })
    }

    /// 检查"声明了零件、却没有任何东西会用到它"这一类问题。
    ///
    /// 具体查三件事，都是**会在运行期表现为"某个功能莫名其妙不生效"**
    /// 而不会报错的配置错误：
    ///
    /// 1. **零件名不认识 / 尚未实现**。注册表有完整清单，报得准。
    /// 2. **别名没有对应的配置块**：`table_translator@melt_eng` 要求方案里
    ///    有一个 `melt_eng:` 段。缺了它，那个翻译器会用空配置跑起来——
    ///    用户看到的是"英文输入不出候选"，而不是一条报错。
    /// 3. **开关没声明**：`punctuator` 要 `full_shape`、`simplifier` 要
    ///    `emoji`。开关没声明时 `Options::set` 返回 `false`（**不会被静默
    ///    创建**），于是那个功能永远关着。
    fn check_declared_components(&self) -> Vec<stele_core::Diagnostic> {
        use crate::registry::Availability;
        let path = format!("scheme:{}", self.info.schema_id);
        let mut out = Vec::new();
        if !self.engine.is_declared() {
            return out;
        }

        let names: Vec<String> = self
            .engine
            .translators
            .iter()
            .chain(self.engine.filters.iter())
            .chain(self.engine.processors.iter())
            .chain(self.engine.segmentors.iter())
            .cloned()
            .collect();

        // **判据由装载器给**（只有它知道文件在不在），引擎按判据出声。
        for (name, availability, note) in
            crate::registry::unmet_requirements(&names, &self.external_data)
        {
            let (component, _) = crate::spec::split_alias(&name);
            let (msg, entry) = match availability {
                Availability::Unknown => (
                    format!("不认识的零件名 `{component}`（来自 `{name}`）"),
                    "方案里的 `engine:` 只能写已实现的零件。\
                     运行 `stele --components` 可以打印完整清单"
                        .to_owned(),
                ),
                Availability::NotYet => (
                    format!("零件 `{component}` 尚未实现，声明了它也用不上"),
                    note.to_owned(),
                ),
                Availability::NeedsData => (
                    format!("零件 `{component}` 需要外部数据，而这份方案没有提供它"),
                    format!(
                        "{note}（实例 `{name}` 的数据没找到：内联表、\
                         或它指向的文件）"
                    ),
                ),
                Availability::Implemented => continue,
            };
            out.push(
                stele_core::Diagnostic::new(&path, msg)
                    .with_field(component)
                    .with_entry(entry),
            );
        }

        // 别名必须有配置块，否则那个实例拿到的是空配置。
        for name in &names {
            let (_, alias) = crate::spec::split_alias(name);
            let Some(a) = alias else { continue };
            let known = self.translator_specs.iter().any(|(k, _)| k == a)
                || self.affixes.iter().any(|(k, _)| k == a)
                || self.reverse_lookups.iter().any(|(k, _)| k == a)
                || self.converters.iter().any(|(k, _, _)| k == a);
            if !known {
                out.push(stele_core::Diagnostic::new(
                    &path,
                    format!("`{name}` 需要方案里有一个 `{a}:` 配置段，但没有找到"),
                ));
            }
        }

        // 零件要用的开关必须已经声明。
        let mut required: Vec<&str> = Vec::new();
        if self.engine.processors.iter().any(|n| n == "ascii_composer") {
            required.push("ascii_mode");
        }
        if self.engine.processors.iter().any(|n| n == "punctuator")
            || self.engine.segmentors.iter().any(|n| n.starts_with("punct"))
        {
            required.push("full_shape");
        }
        for (_, _, spec) in &self.converters {
            if let Some(n) = spec.option_name.as_deref() {
                required.push(n);
            }
        }
        for name in &required {
            if !self.switches.iter().any(|s| s.name == *name) {
                out.push(
                    stele_core::Diagnostic::new(
                        &path,
                        format!("零件需要开关 `{name}`，但 `switches:` 里没有声明它"),
                    )
                    .with_field("switches")
                    .with_entry(
                        "未声明的开关不会被静默创建，`set` 会返回 false —— \
                         于是那个功能永远关着"
                            .to_owned(),
                    ),
                );
            }
        }

        out
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
    /// `engine:` 段声明的零件。
    engine: crate::spec::EngineSpec,
    recognizer: crate::spec::RecognizerSpec,
    punctuator: crate::spec::PunctuatorSpec,
    editor_bindings: Vec<(crate::spec::KeyChord, crate::spec::EditorAction)>,
    key_bindings: Vec<crate::spec::KeyBinding>,
    navigator: crate::spec::NavigatorSpec,
    affixes: Vec<(String, crate::spec::AffixSpec)>,
    reverse_lookups: Vec<(String, crate::spec::ReverseLookupSpec)>,
    converters: Vec<(
        String,
        std::collections::BTreeMap<String, Vec<String>>,
        crate::spec::SimplifierSpec,
    )>,
    translator_specs: Vec<(String, crate::spec::TranslatorSpec)>,
    input_alphabet: Vec<char>,
    page_size: usize,
    /// 外部数据就位判据（见 [`SchemeDef::external_data`]）。
    ///
    /// 它只在 `compile` 期用来"出声"，装完之后留着供 `--dump-config`
    /// 回答"这个实例的数据到底找到了没有"——那正是使用者最想问的问题。
    external_data: Vec<crate::registry::ExternalData<'static>>,
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

    /// `engine:` 段的声明（供 `--dump-config` 打印"每个零件来自哪一行"）。
    #[must_use]
    pub fn engine_spec(&self) -> &crate::spec::EngineSpec {
        &self.engine
    }

    /// 某个实例的外部数据找到了没有。
    ///
    /// 返回 `None` 表示**这个实例不需要外部数据**（或它没被声明）——
    /// 与"需要但没找到"是两件事，`--dump-config` 靠这个区分。
    #[must_use]
    pub fn external_data_present(&self, alias: &str) -> Option<bool> {
        self.external_data
            .iter()
            .find(|e| e.alias == alias)
            .map(|e| e.present)
    }

    /// 某个翻译器实例的配置（`alias` 为空 = 方案级的默认那份）。
    #[must_use]
    pub fn translator_spec(&self, alias: &str) -> crate::spec::TranslatorSpec {
        self.translator_specs
            .iter()
            .find(|(k, _)| k == alias)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }

    /// 装配**切分器**（含识别器）。
    ///
    /// 返回 `(识别器, 切分器列表, 各切分器产出的标签)`。
    ///
    /// # 这里的顺序规则
    ///
    /// 方案写的是**零件名列表**（RIME 的 `engine.segmentors`），
    /// 而一个"名字"展开成什么样，由**该零件的配置**决定：
    ///
    /// - `matcher` → 认领识别器模式的那一段（未配 `affix_segmentor` 的那些）
    /// - `affix_segmentor@x` → 由 `x:` 段里的 `prefix` 决定怎么剥
    /// - `abc_segmentor` → 兜底（吃剩下的）
    ///
    /// 因此"名字 → 实例"是**数据驱动**的，不是写死的匹配表。
    fn build_segmentors(&self) -> SegmentorBuild {
        use crate::segmentor as seg;
        let mut tags = crate::tag::TagTable::new();
        // **先把内置标签登记好**：`punct` 被切分器与翻译器两处引用，
        // 而两处各自 `intern` 会拿到**同一块内存**（这正是不出问题的原因）；
        // 但若有一处忘了 intern、直接写字符串字面量，就会变成两块内存——
        // 于是 `tags.contains()` 失效、标点整条链断掉，且没有任何报错。
        // 这个 bug 真发生过（符号表整条不工作），所以这里显式登记。
        let punct_tag = tags.intern("punct");
        let abc_tag = tags.intern(if self.tag.is_empty() { "abc" } else { self.tag });
        let mut out: Vec<Box<dyn stele_core::Segmentor>> = Vec::new();
        let mut produced: Vec<Tag> = Vec::new();

        let recognizer = if self.recognizer.patterns.is_empty() {
            None
        } else {
            match Recognizer::new(&self.recognizer, &mut tags) {
                Ok(r) => Some(Box::new(r)),
                // 正则编译失败在装载期已经报过（`compile` 走的是同一条路），
                // 到这里说明是"编译过了又改坏了"——不 panic，退回无识别器。
                Err(_) => None,
            }
        };
        let scan = recognizer
            .as_ref()
            .map_or_else(seg::InputScan::default, |r| r.scan(""));

        // 哪些识别器模式会被 affix_segmentor 接管（于是不该再给 matcher）。
        let affix_tags: Vec<Tag> = self.affixes.iter().filter_map(|(_, a)| a.tag).collect();
        let matcher_tags: Vec<Tag> = recognizer
            .as_ref()
            .map(|r| {
                let names = r.pattern_names();
                names
                    .into_iter()
                    .filter(|n| !affix_tags.iter().any(|t| t == n))
                    .map(|n| tags.intern(n))
                    .collect()
            })
            .unwrap_or_default();

        for name in &self.engine.segmentors {
            let (component, alias) = crate::spec::split_alias(name);
            match component {
                // RIME 的 `ascii_segmentor` 标"非编码段"，`fallback_segmentor`
                // 是兜底。这两个零件在 P3 里**没有独立的实现**：它们的功能
                // 已经由别的零件覆盖（`abc_segmentor` 本来就是"没人认领就归我"）。
                //
                // 静默略过（而不是报"尚未实现"）是有意的：真实方案
                // （rime-ice 的 `no_lua_schema`）里这两个名字都在，
                // 报错会让整份方案装不进去，而实际行为并不缺。
                //
                "ascii_segmentor" | "fallback_segmentor" => {}
                "matcher" => {
                    if !matcher_tags.is_empty() {
                        produced.extend(matcher_tags.iter().copied());
                        out.push(Box::new(seg::Matcher::new(
                            scan.clone(),
                            matcher_tags.clone(),
                        )));
                    }
                }
                "abc_segmentor" => {
                    produced.push(abc_tag);
                    out.push(Box::new(seg::CodingSegmentor::new(abc_tag, scan.clone())));
                }
                "affix_segmentor" => {
                    if let Some(a) = alias {
                        if let Some((_, spec)) = self.affixes.iter().find(|(k, _)| k == a) {
                            let all = seg::affix_all_tags(spec, &mut tags, self.tag);
                            produced.extend(all.iter().copied());
                            out.push(Box::new(seg::AffixSegmentor::new(
                                spec, &mut tags, abc_tag,
                            )));
                        }
                    }
                }
                "punct_segmentor" => {
                    produced.push(punct_tag);
                    out.push(Box::new(seg::SymbolSegmentor::new(
                        punct_tag,
                        &self.punctuator,
                    )));
                }
                // 别的切分器（`chord_composer` 等）由注册表在装载期报
                // "尚未实现"；这里不重复处理。
                #[allow(clippy::match_same_arms)]
                _ => {}
            }
        }

        // 方案没写 `engine.segmentors` 时退回老行为：整串一段。
        if out.is_empty() {
            produced.push(abc_tag);
        }
        // 去重但保序（标签的顺序会影响诊断输出的可读性）。
        let mut seen: Vec<Tag> = Vec::new();
        for t in produced {
            if !seen.contains(&t) {
                seen.push(t);
            }
        }
        SegmentorBuild {
            recognizer,
            segmentors: out,
            tags: seen,
        }
    }
}

/// [`LoadedScheme::build_segmentors`] 的返回：三样东西一起给出。
///
/// 用结构体而不是元组：三个字段都是 `Vec`/`Option`，元组在三处调用点上
/// 靠位置区分，读起来是 `let (a, b, c) = ...`——**位置不承载语义**，
/// 而这个项目里已经有过一次"按固定下标读调试输出读错了"的教训。
struct SegmentorBuild {
    /// 识别器（没有配置模式时为 `None`）。
    recognizer: Option<Box<Recognizer>>,
    /// 切分器（按方案声明的顺序）。
    segmentors: Vec<Box<dyn stele_core::Segmentor>>,
    /// 它们能产出的全部标签。
    tags: Vec<Tag>,
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
    ///
    /// 装配分两步，缺一不可：
    ///
    /// 1. **按方案声明的顺序**放进零件（`engine:` 段）。顺序即语义。
    /// 2. **检查每个翻译器的标签有切分器产出它**——否则那个翻译器
    ///    永远不会被调用，而配置看起来完全正常。
    fn build_pipeline(&self) -> Box<dyn Pipeline + Send> {
        let mut processors: Vec<Box<dyn Processor>> = Vec::new();
        // `key_binder` 的位置要记下来：换来的按键必须从它之后开始派发，
        // 否则 `{accept: space, send: space}` 这类绑定会把自己再触发一遍，
        // 于是空格永远到不了选择器（见 `PipelineImpl::process_key`）。
        let mut binder_index: Option<usize> = None;
        // 没有被 `engine:` 声明时用 RIME 的默认顺序。
        let declared: Vec<String> = if self.engine.processors.is_empty() {
            vec![
                "speller".to_owned(),
                "editor".to_owned(),
                "selector".to_owned(),
            ]
        } else {
            self.engine.processors.clone()
        };
        for name in &declared {
            let (component, _alias) = crate::spec::split_alias(name);
            match component {
                "speller" => {
                    let mut s = Speller::new(vec!['\'']);
                    if !self.input_alphabet.is_empty() {
                        s = s.with_alphabet(self.input_alphabet.clone());
                    }
                    if self.engine.processors.iter().any(|n| n == "ascii_composer") {
                        s = s.blocked_by("ascii_mode");
                    }
                    processors.push(Box::new(s));
                }
                "editor" | "express_editor" => {
                    processors.push(Box::new(Editor::new(self.editor_bindings.clone())));
                }
                "selector" => processors.push(Box::new(Selector)),
                "ascii_composer" => processors.push(Box::new(AsciiComposer::new(Some(
                    "ascii_mode".to_owned(),
                )))),
                "punctuator" => processors.push(Box::new(Punctuator::new(
                    &self.punctuator,
                    Some("full_shape".to_owned()),
                ))),
                "key_binder" => {
                    binder_index = Some(processors.len());
                    processors.push(Box::new(KeyBinder::new(self.key_bindings.clone())));
                }
                "navigator" => processors.push(Box::new(Navigator::new(
                    &self.navigator,
                    self.page_size,
                ))),
                // `recognizer` 是"扫描"而不是处理器（见 `segmentor.rs`），
                // `select_character` 需要"以词定字"的数据，P3 未做。
                _ => {}
            }
        }

        let SegmentorBuild {
            recognizer,
            segmentors,
            tags: segmentor_tags,
        } = self.build_segmentors();

        let transl = |alias: &str| self.translator_spec(alias);
        let translators: Vec<Box<dyn Translator>> = if self.engine.translators.is_empty() {
            // 老行为：按翻译器族自动装配。
            match self.kind {
                TranslatorKind::ExactCode => vec![
                    Box::new(ExactCodeTranslator::new(
                        &self.alphabet,
                        Arc::clone(&self.lexicon),
                    )),
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
            }
        } else {
            let mut v: Vec<Box<dyn Translator>> = Vec::new();
            for name in &self.engine.translators {
                let (component, alias) = crate::spec::split_alias(name);
                let Some(kind) = crate::spec::TranslatorKindSpec::parse(component) else {
                    continue;
                };
                let spec = transl(alias.unwrap_or(""));
                // 带前缀的实例交给词缀切分器剥前缀，翻译器只看正文。
                let Some(prefix_tag) = self
                    .affixes
                    .iter()
                    .find(|(k, _)| Some(k.as_str()) == alias)
                    .and_then(|(_, a)| a.tag)
                else {
                    // 既没有词缀切分器、又写了 prefix 的实例：把它当作
                    // 普通翻译器（前缀在输入里原样存在）。
                    v.push(Self::make_translator(
                        kind,
                        &spec,
                        &self.alphabet,
                        &self.lexicon,
                        self.spelling.as_ref(),
                    ));
                    continue;
                };
                // 有前缀 → 只处理那个标签，且看到的是剥掉前缀的正文。
                v.push(Box::new(
                    TaggedTranslator::new(
                        Self::make_translator(
                            kind,
                            &spec,
                            &self.alphabet,
                            &self.lexicon,
                            self.spelling.as_ref(),
                        ),
                        vec![prefix_tag],
                    )
                    .without_affix_stripping(),
                ));
            }
            // 兜底永远在最后：查不到也要能上屏（G4）。
            v.push(Box::new(EchoTranslator::new()));
            v
        };

        // 标点翻译器：方案声明了 `punctuator`（处理器）或 `punct_segmentor`
        // 时自动挂上——它只对标点段生效，因此不会有副作用。
        let has_punct = self
            .engine
            .processors
            .iter()
            .any(|n| n == "punctuator")
            || self
                .engine
                .segmentors
                .iter()
                .any(|n| n == "punct_segmentor");
        let mut translators = translators;
        if has_punct {
            // 插在兜底之前：标点候选的分数高于兜底。
            let at = translators.len().saturating_sub(1);
            translators.insert(
                at,
                Box::new(PunctTranslator::new(
                    &self.punctuator,
                    Some("full_shape".to_owned()),
                )),
            );
        }

        let mut filters: Vec<Box<dyn Filter>> = Vec::new();
        for name in &self.engine.filters {
            let (component, alias) = crate::spec::split_alias(name);
            match component {
                "uniquifier" => filters.push(Box::new(Uniquifier)),
                "reverse_lookup_filter" => {
                    if let Some(a) = alias {
                        if let Some((_, spec)) =
                            self.reverse_lookups.iter().find(|(k, _)| k == a)
                        {
                            let fmt = crate::spelling::SpellingFormat::parse_all(
                                &spec
                                    .comment_format
                                    .iter()
                                    .map(rule_to_spec)
                                    .collect::<Vec<_>>(),
                            )
                            .ok()
                            .filter(|f| !f.is_empty());
                            // 反查词典：目前复用方案自己的词库（雾凇的
                            // `radical_reverse_lookup/dictionary` 指的是
                            // 本方案词库）——见 `ReverseLexicon` 的说明。
                            let rev: Arc<dyn crate::filter::ReverseLexicon> =
                                Arc::new(crate::lexicon::TextIndex::default());
                            filters.push(Box::new(TaggedFilter::new(
                                Box::new(crate::filter::ReverseLookupFilter::new(
                                    rev,
                                    fmt,
                                    spec.overwrite_comment,
                                )),
                                spec.tags.clone(),
                            )));
                        }
                    }
                }
                "simplifier" => {
                    if let Some(a) = alias {
                        if let Some((_, table, spec)) =
                            self.converters.iter().find(|(k, _, _)| k == a)
                        {
                            let inner = Box::new(crate::filter::Converter::new(
                                table.clone(),
                                spec.option_name.clone(),
                                spec.inherit_comment,
                            ));
                            if spec.tags.is_empty() {
                                filters.push(inner);
                            } else {
                                filters.push(Box::new(TaggedFilter::new(
                                    inner,
                                    spec.tags.clone(),
                                )));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // 方案没声明滤镜时保留去重（它是"同一文本只出现一次"的保证）。
        if filters.is_empty() {
            filters.push(Box::new(Uniquifier));
        }

        let spelling: Option<Arc<dyn stele_core::Spelling>> = self
            .spelling
            .clone()
            .map(|s| s as Arc<dyn stele_core::Spelling>);

        // 标签绑定的一致性检查放在**装配期**：这是唯一能同时看到
        // "切分器产出什么"与"翻译器要什么"的地方。
        debug_assert!(
            translators.iter().all(|t| {
                let targets = t.targets();
                targets.is_empty() || targets.iter().any(|x| segmentor_tags.contains(x))
            }),
            "有翻译器声明了没有任何切分器会产出的标签，它永远不会被调用"
        );

        Box::new(
            PipelineImpl::new(
                self.tag,
                processors,
                translators,
                filters,
                Vec::new(),
                self.candidate_cap,
            )
            .with_segmentors(recognizer, segmentors)
            .with_binder_index(binder_index)
            .with_page_size(self.page_size)
            .with_preedit(self.preedit_delimiter, spelling),
        )
    }
}

impl LoadedScheme {
    /// 造一个翻译器（两族共用一条装配路径）。
    fn make_translator(
        kind: crate::spec::TranslatorKindSpec,
        spec: &crate::spec::TranslatorSpec,
        alphabet: &CodeAlphabet,
        lexicon: &Arc<dyn stele_core::Lexicon>,
        spelling: Option<&Arc<SpellingTable>>,
    ) -> Box<dyn Translator> {
        use crate::spec::TranslatorKindSpec as K;
        let graph = match kind {
            K::SpellingGraph => spelling
                .cloned()
                .map(|s| s as Arc<dyn stele_core::Spelling>)
                .map(|sp| {
                    SpellingGraphTranslator::new(sp, Arc::clone(lexicon))
                        .with_completion(spec.completion())
                }),
            K::ExactCode => None,
        };
        match kind {
            K::SpellingGraph => match graph {
                Some(t) => Box::new(t),
                // 方案要拼写图族却没给拼写表（精确编码方案）——
                // 退回精确编码，至少能查表。
                None => Box::new(ExactCodeTranslator::new(alphabet, Arc::clone(lexicon))),
            },
            K::ExactCode => Box::new(
                ExactCodeTranslator::new(alphabet, Arc::clone(lexicon))
                    .with_completion(spec.completion()),
            ),
        }
    }
}

/// 把一条已解析的规则还原成 RIME 的写法串。
///
/// `comment_format` / `preedit_format` 只需要 `xform` / `xlit`，
/// 而这两者都能无损还原。其它运算子在这里**不该出现**
/// （装载器已经报过错），真出现了就还原成空串——宁可少一条规则，
/// 也不要在格式化里悄悄用错语义。
fn rule_to_spec(r: &crate::spelling::Rule) -> String {
    use crate::spelling::Rule as R;
    match r {
        R::Xform { pattern, repl } => {
            format!("xform/{}/{}/", crate::regex::describe(pattern), repl)
        }
        R::Xlit { from, to } => {
            let f: String = from.iter().collect();
            let t: String = to.iter().collect();
            format!("xlit/{f}/{t}/")
        }
        _ => String::new(),
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
            ..Default::default()
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
