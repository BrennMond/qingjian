//! # Translators
//!
//! 中文职责：两族翻译器——**精确编码**与**拼写图**。
//! English role: the two translator families — exact-code and spelling-graph.
//! 架构位置：`stele-core::Translator` 的实现。
//!
//! # 为什么是两族（而不是一族）
//!
//! 这不是"实现细节"，而是**编码集合能否被枚举**决定的：
//!
//! - **拼音 / 双拼 / 注音**：编码单元的集合是**可枚举的小集合**（约 400 个音节），
//!   因此可以构造"拼写 → 编码"的映射，走[拼写图翻译器](SpellingGraphTranslator)。
//! - **仓颉 / 五笔 / 英文**：编码单元的集合**不可枚举**（字根组合是幂集），
//!   构造不出那个映射，只能把输入串当作**一条完整编码**直接查表，
//!   走[精确编码翻译器](ExactCodeTranslator)。
//!
//! **引擎不预设谁用哪一族——方案在配置里选。** 这也正是
//! `docs/engine-design.md` §2.4.3 与 D33 说的那件事。

use std::collections::BTreeMap;
use std::sync::Arc;
use stele_core::{
    Candidate, CandidateSink, CodeAlphabet, CodeUnitId, Filter, Lane, Lexicon, Origin, Query, Score,
    Span, Spelling, SpellingAttr, Tag, Translator,
};

/// 一次翻译最多产出多少候选（防止病态输入撑爆内存）。
pub const TRANSLATE_CAP: usize = 200;

/// **精确编码翻译器**：把输入串当作一条完整编码，直接查表。
///
/// 不做拼写运算、不做歧义展开、不做切分图。仓颉 / 五笔 / 英文这一类
/// 方案用它。**它证明引擎不强迫所有方案都走拼音那条路。**
pub struct ExactCodeTranslator {
    lexicon: Arc<dyn Lexicon>,
    /// 单元文本 → 编号。贪心最长匹配时按文本长度降序试。
    by_text: BTreeMap<String, CodeUnitId>,
    /// 单元文本按长度降序（贪心最长匹配）。
    texts_by_len: Vec<String>,
    /// 词条补全（RIME 的 `enable_completion`）。
    completion: bool,
}

impl ExactCodeTranslator {
    /// 由字母表与词库构造。
    #[must_use]
    pub fn new(alphabet: &CodeAlphabet, lexicon: Arc<dyn Lexicon>) -> Self {
        let mut by_text = BTreeMap::new();
        for i in 0..alphabet.len() {
            #[allow(clippy::cast_possible_truncation)]
            let id = CodeUnitId(i as u32);
            if let Some(t) = alphabet.text(id) {
                by_text.insert(t.to_owned(), id);
            }
        }
        let mut texts_by_len: Vec<String> = by_text.keys().cloned().collect();
        // 长的先试 → 贪心最长匹配；同长度按字典序，保证确定性。
        texts_by_len.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

        Self {
            lexicon,
            by_text,
            texts_by_len,
            completion: false,
        }
    }

    /// 打开词条补全。
    #[must_use]
    pub fn with_completion(mut self, on: bool) -> Self {
        self.completion = on;
        self
    }

    /// 把输入串切成编码单元（贪心最长匹配）。
    ///
    /// 返回 `None` 表示有字符不属于任何编码单元——**这不是错误**，
    /// 只是"这条输入不是本方案的编码"。
    #[must_use]
    pub fn split(&self, input: &str) -> Option<Vec<CodeUnitId>> {
        let mut code = Vec::new();
        let mut pos = 0usize;
        while pos < input.len() {
            let matched = self
                .texts_by_len
                .iter()
                .find(|t| input[pos..].starts_with(t.as_str()))?;
            code.push(*self.by_text.get(matched)?);
            pos += matched.len();
        }
        Some(code)
    }
}

impl Translator for ExactCodeTranslator {
    fn translate(&self, q: &Query<'_>, span: Span, out: &mut CandidateSink<'_>) {
        let Some(code) = self.split(q.segment_text) else {
            return;
        };
        let mut raw: Vec<Candidate> = Vec::new();
        {
            let mut sink = CandidateSink::new(&mut raw, TRANSLATE_CAP);
            self.lexicon.lookup(&code, &mut sink);
            // 补全：编码集合不可枚举的方案（仓颉 / 英文）里，
            // "打一半就出候选"同样是常见的期待——英文尤其如此。
            if self.completion && self.lexicon.supports_prefix() {
                self.lexicon.prefix_lookup(&code, true, &mut sink);
            }
        }
        for mut c in raw {
            // 覆盖整段输入。
            c.span = span;
            out.push(c);
        }
    }
}

/// **拼写图翻译器**：把拼写展开成编码（含变体），再逐个查词库。
///
/// 拼音 / 双拼 / 注音这一类方案用它。
pub struct SpellingGraphTranslator {
    spelling: Arc<dyn Spelling>,
    lexicon: Arc<dyn Lexicon>,
    /// 词条补全（RIME 的 `enable_word_completion`）。
    completion: bool,
}

impl SpellingGraphTranslator {
    /// 由拼写层与词库构造。
    #[must_use]
    pub fn new(spelling: Arc<dyn Spelling>, lexicon: Arc<dyn Lexicon>) -> Self {
        Self {
            spelling,
            lexicon,
            completion: false,
        }
    }

    /// 打开词条补全。
    #[must_use]
    pub fn with_completion(mut self, on: bool) -> Self {
        self.completion = on;
        self
    }
}

impl Translator for SpellingGraphTranslator {
    fn translate(&self, q: &Query<'_>, span: Span, out: &mut CandidateSink<'_>) {
        let mut expansions = Vec::new();
        {
            let mut sink = stele_core::ExpansionSink::new(&mut expansions, 64);
            self.spelling.expand(q.segment_text, &mut sink);
        }

        // 这一次查询要不要补全：**只有当这段拼写是一条完整的切分时才做**。
        //
        // 为什么：`niha` 展开出的编码是 `[ni, ha]`，而 `ha` 不是一个合法的
        // 编码单元——[`Spelling::expand`] 根本不会产出这条展开（短路的边
        // 必须在表里）。**"补全"所以不是"前缀扫描一条编码"，而是
        // "把最后那个编码单元之后可能接什么补出来"**——那需要知道
        // 字母表里有哪些单元以它开头，而那正是前缀扫描能回答的问题。
        let completion = self.completion && self.lexicon.supports_prefix();

        for exp in expansions {
            let mut raw: Vec<Candidate> = Vec::new();
            {
                let mut sink = CandidateSink::new(&mut raw, TRANSLATE_CAP);
                self.lexicon.lookup(&exp.code, &mut sink);
                if completion {
                    // 补全只对**规范拼写**那条边做：简拼/纠错的边上再补全
                    // 会让候选数量乘起来，而收益极小（用户敲简拼时本来就
                    // 不指望看到完整词的补全）。
                    if exp.attr == stele_core::SpellingAttr::NORMAL {
                        self.lexicon.prefix_lookup(&exp.code, true, &mut sink);
                    }
                }
            }
            for mut c in raw {
                // 候选的分数 = 词条分数 + 这条边的代价。
                // **这就是"简拼天然排在精确匹配之后"的全部机制**：
                // 缩写边的代价是负的，不需要任何额外规则。
                c.score = c.score.saturating_add(exp.cost);
                if completion && c.attr.contains(stele_core::SpellingAttr::COMPLETION) {
                    // 补全出来的词**扣一次分**：它是"猜你要打这个",
                    // 不该与真正打全的词平起平坐。
                    //
                    // 代价数值**属于方案数据**（RIME 的 `enable_word_completion`
                    // 也有对应的权重扣减）。这里用一个与拼写代数里
                    // `Completion` 边同量级的值（ln 0.05 ≈ -3.0，
                    // 见 `docs/engine-design.md` §5.2 的表）。
                    c.score = c.score.saturating_add(COMPLETION_COST);
                } else {
                    // 属性取并集：只要这条边经过了变形，候选就不是"精确"的。
                    c.attr = c.attr.union(exp.attr);
                }
                c.span = span;
                out.push(c);
            }
            if out.len() >= TRANSLATE_CAP {
                break;
            }
        }
    }
}

/// 补全候选的代价（对数域，`ln(0.05) ≈ -3.0`）。
///
/// 取这个数的理由见 `docs/engine-design.md` §5.2 的边代价表：
/// 它就是"补全"这一类边的标准代价。**放在这里而不是散在代码里**，
/// 因为它是"补全排在精确匹配之后"的**唯一**机制——
/// 没有它，补全出来的长词会凭词条权重压过用户真正打全的词。
pub const COMPLETION_COST: Score = Score::from_milli_log(-2_996);

/// **兜底翻译器**：保证"你敲的东西永远能上屏"（G4）。
///
/// 它是流水线里**最后**一个翻译器，产出一个 `Origin::Literal` 的候选，
/// 分数取 [`Score::FLOOR`] 之上一点点——**分低，但一定在**。
///
/// 没有它，用户会遇到"敲了东西但候选框空的"，那会让人以为输入法坏了，
/// 或者卡在一个退不出去的输入状态里。
pub struct EchoTranslator {
    /// 相对下界抬高多少，使兜底候选高于"完全不可能"。
    lift: Score,
}

impl Default for EchoTranslator {
    fn default() -> Self {
        Self::new()
    }
}

impl EchoTranslator {
    /// 构造。
    #[must_use]
    pub fn new() -> Self {
        Self {
            lift: Score::from_weight(1e-6),
        }
    }
}

impl Translator for EchoTranslator {
    fn translate(&self, q: &Query<'_>, span: Span, out: &mut CandidateSink<'_>) {
        if q.input.is_empty() {
            return;
        }
        out.push(Candidate {
            text: q.input.to_owned(),
            comment: None,
            score: Score::FLOOR.saturating_add(self.lift),
            origin: Origin::Literal,
            attr: SpellingAttr::NORMAL,
            span,
            lane: Lane::Input,
            kind: stele_core::CandidateKind::Normal,
        });
    }
}

/// 一个只对特定 tag 生效的翻译器包装——演示并验证 tag 绑定（G3）。
///
/// **tag 是"切分器 → 翻译器"的绑定层**：一个方案可以挂多个翻译器，
/// 它们不靠位置区分，靠 tag 区分。
///
/// # 包装层还要负责"换掉 `Query` 里那一段文本"（G15）
///
/// 带词缀的分段（`affix_segmentor`）里，要翻译的不是整串 `uUni`，
/// 而是去掉前缀之后的 `ni`。这个"换文本"的动作**由包装层做**：
/// 被包装的翻译器根本不知道词缀的存在——它只看到"一段要翻译的文本"。
///
/// 为什么不让每个翻译器自己处理前缀：那样"前缀"这个概念会渗透进
/// 每一个翻译器，包括仓颉 / 英文这些**根本没有前缀概念**的方案。
/// 包装层把它挡在外面，这正是它存在的意义。
pub struct TaggedTranslator {
    inner: Box<dyn Translator>,
    tags: Vec<Tag>,
    /// 被包装的翻译器是否要看到**去掉词缀之后**的正文。
    ///
    /// `false` 时它看到整串输入（例如"标点翻译器"要看到那个标点本身）。
    strip_affix: bool,
}

impl TaggedTranslator {
    /// 包装一个翻译器，限定它只处理带这些 tag 的分段。
    #[must_use]
    pub fn new(inner: Box<dyn Translator>, tags: Vec<Tag>) -> Self {
        Self {
            inner,
            tags,
            strip_affix: true,
        }
    }

    /// 让被包装的翻译器看到**整串输入**（不剥词缀）。
    #[must_use]
    pub fn without_affix_stripping(mut self) -> Self {
        self.strip_affix = false;
        self
    }
}

impl Translator for TaggedTranslator {
    /// # 谁负责"剥掉词缀"
    ///
    /// **流水线负责**，不在这里。词缀的长度是**切分器的知识**
    /// （它读方案里的 `prefix`），而这里只声明"我要的是正文"。
    /// 早先这里自己想剥，但它拿不到前缀长度——于是反查翻译器
    /// 收到的是 `uUni` 而不是 `ni`，**候选一个都不出**。
    /// 现在流水线按 [`stele_core::Segmentor::body_start`] 填好正文，
    /// 这里只把 `Query::segment_text` 原样交给被包装者。
    fn translate(&self, q: &Query<'_>, span: Span, out: &mut CandidateSink<'_>) {
        self.inner.translate(q, span, out);
    }

    fn accepts(&self, tags: &[Tag]) -> bool {
        tags.iter().any(|t| self.tags.contains(t))
    }

    fn targets(&self) -> &[Tag] {
        &self.tags
    }
}

/// 一个只对特定 tag 生效的**滤镜**包装。
///
/// 与 [`TaggedTranslator`] 对称：`simplifier` 的 `tags: [abc]`、
/// `reverse_lookup_filter` 的 `tags: [radical_lookup]` 都是这条约束。
///
/// **为什么它必须存在**：雾凇的简繁转换写着 `tags: [ abc ]`，
/// 注释是「限制在对应 tag，不对其他如反查的内容做简繁转换」——
/// 少了这层约束，拆字反查出来的部件字会被"顺手"转成繁体，
/// 用户看到的反查结果与他敲的东西对不上。
pub struct TaggedFilter {
    inner: Box<dyn Filter>,
    tags: Vec<Tag>,
}

impl TaggedFilter {
    /// 包装一个滤镜，限定它只对带这些 tag 的分段生效。
    #[must_use]
    pub fn new(inner: Box<dyn Filter>, tags: Vec<Tag>) -> Self {
        Self { inner, tags }
    }
}

impl Filter for TaggedFilter {
    fn apply(&self, q: &Query<'_>, span: Span, cands: &mut Vec<Candidate>) {
        self.inner.apply(q, span, cands);
    }

    fn applies_to(&self, tags: &[Tag]) -> bool {
        tags.iter().any(|t| self.tags.contains(t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexicon::InMemoryLexicon;
    use crate::spelling::{Rule, SpellingTable};
    use stele_core::{Context, Options};

    fn alphabet(units: &[&str]) -> CodeAlphabet {
        CodeAlphabet::new(units.iter().map(|s| (*s).to_owned()).collect())
    }

    fn query<'a>(input: &'a str, options: &'a Options, context: &'a Context) -> Query<'a> {
        Query {
            input,
            caret: input.len(),
            options,
            context,
            segment_text: input,
        }
    }

    #[test]
    fn exact_translator_splits_and_looks_up() {
        let a = alphabet(&["a", "b"]);
        let lex = Arc::new(
            InMemoryLexicon::from_entries(a.clone(), &[(vec!["a", "b"], "十", 5.0)]).unwrap(),
        );
        let t = ExactCodeTranslator::new(&a, lex);

        assert_eq!(t.split("ab"), Some(vec![CodeUnitId(0), CodeUnitId(1)]));
        assert_eq!(t.split("az"), None, "z 不属于该方案的编码单元");

        let opts = Options::new();
        let ctx = Context::default();
                let mut buf = Vec::new();
        let mut sink = CandidateSink::new(&mut buf, 16);
        t.translate(&query("ab", &opts, &ctx), Span::new(0, 2), &mut sink);
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].text, "十");
        assert_eq!(buf[0].span, Span::new(0, 2));
    }

    #[test]
    fn spelling_graph_translator_applies_edge_cost_and_attr() {
        let a = alphabet(&["ni", "hao"]);
        let table = Arc::new(SpellingTable::compile(
            a.clone(),
            &[Rule::abbrev(1, Score::from_weight(0.5)).unwrap()],
        ));
        let lex = Arc::new(
            InMemoryLexicon::from_entries(a, &[(vec!["ni", "hao"], "你好", 100.0)]).unwrap(),
        );
        let t = SpellingGraphTranslator::new(table, lex);

        let opts = Options::new();
        let ctx = Context::default();
        
        // 规范拼写：属性为 NORMAL，分数就是词条分数。
        let mut buf = Vec::new();
        let mut sink = CandidateSink::new(&mut buf, 16);
        t.translate(
            &query("nihao", &opts, &ctx),
            Span::new(0, 5),
            &mut sink,
        );
        assert_eq!(buf[0].text, "你好");
        assert_eq!(buf[0].attr, SpellingAttr::NORMAL);
        let canonical_score = buf[0].score;

        // 简拼：同一个词，但带 ABBREV 属性且分数更低。
        let mut buf2 = Vec::new();
        let mut sink2 = CandidateSink::new(&mut buf2, 16);
        t.translate(
            &query("nh", &opts, &ctx),
            Span::new(0, 2),
            &mut sink2,
        );
        assert_eq!(buf2[0].text, "你好");
        assert!(buf2[0].attr.contains(SpellingAttr::ABBREV));
        assert!(
            buf2[0].score < canonical_score,
            "简拼候选的分数必须低于同词的规范拼写 —— 这就是它天然排在后面的原因"
        );
    }

    #[test]
    fn echo_translator_always_yields_something() {
        let t = EchoTranslator::new();
        let opts = Options::new();
        let ctx = Context::default();
        
        let mut buf = Vec::new();
        let mut sink = CandidateSink::new(&mut buf, 16);
        t.translate(
            &query("zzz", &opts, &ctx),
            Span::new(0, 3),
            &mut sink,
        );
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].text, "zzz");
        assert_eq!(buf[0].origin, Origin::Literal);
        assert!(
            stele_core::is_exact(buf[0].origin, buf[0].attr),
            "原样上屏不是猜测"
        );

        // 空输入不产出候选。
        let mut buf2 = Vec::new();
        let mut sink2 = CandidateSink::new(&mut buf2, 16);
        t.translate(&query("", &opts, &ctx), Span::new(0, 0), &mut sink2);
        assert!(buf2.is_empty());
    }

    #[test]
    fn tagged_translator_binds_by_tag() {
        let inner = EchoTranslator::new();
        let tagged = TaggedTranslator::new(Box::new(inner), vec!["abc"]);
        assert!(tagged.accepts(&["abc"]));
        assert!(tagged.accepts(&["punct", "abc"]));
        assert!(!tagged.accepts(&["punct"]));
    }
}
