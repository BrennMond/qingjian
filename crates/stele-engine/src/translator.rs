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
    Candidate, CandidateSink, CodeAlphabet, CodeUnitId, Filter, Lane, Lexicon, Origin, Query,
    Score, Span, Spelling, SpellingAttr, Tag, Translator,
};

/// 一次翻译最多产出多少候选（防止病态输入撑爆内存）。
pub const TRANSLATE_CAP: usize = 200;

/// **精确编码翻译器**：把输入串当作一条完整编码，直接查表。
///
/// 不做拼写运算、不做歧义展开、不做切分图。仓颉 / 五笔 / 英文这一类
/// 方案用它。**它证明引擎不强迫所有方案都走拼音那条路。**
pub struct ExactCodeTranslator {
    lexicon: Arc<dyn Lexicon>,
    /// 字母表。**保留它是为了把编码渲染成记忆的键**（`code_key`）——
    /// 只有它知道"编号 3 写作 `hao`"。
    alphabet: CodeAlphabet,
    /// 单元文本 → 编号。贪心最长匹配时按文本长度降序试。
    by_text: BTreeMap<String, CodeUnitId>,
    /// 单元文本按长度降序（贪心最长匹配）。
    texts_by_len: Vec<String>,
    /// 词条补全（RIME 的 `enable_completion`）。
    completion: bool,
    /// `initial_quality`：这个实例的**权重倍率**（线性域）。
    ///
    /// 换算成对数域在装载期做一次，按键路径上只做加法。
    quality: Score,
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
            alphabet: alphabet.clone(),
            by_text,
            texts_by_len,
            completion: false,
            quality: Score::ZERO,
        }
    }

    /// 打开词条补全。
    #[must_use]
    pub fn with_completion(mut self, on: bool) -> Self {
        self.completion = on;
        self
    }

    /// 设置 `initial_quality`（线性域权重倍率）。
    ///
    /// # 它到底改变什么（审计 §2.G5 点名"解析了没生效"的字段之一）
    ///
    /// RIME 的 `initial_quality` 是**这个翻译器实例**的权重倍率：rime-ice
    /// 用它把英文翻译器（`initial_quality: 1.1`）排在拼音之前。
    /// 语义是"同一个词的分数乘以它"，因此换算到对数域就是**加一个常数**。
    ///
    /// 必须**逐实例**生效：否则"英文 1.1、拼音 1.0"这种最常见的写法
    /// 会变成"所有候选一起 +0.1 倍"，等于没写。
    #[must_use]
    pub fn with_initial_quality(mut self, q: Option<f64>) -> Self {
        self.quality = quality_of(q);
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
        if raw.is_empty() {
            return;
        }
        // **把编码渲染成键，一次即可**——`lookup` 与 `prefix_lookup`
        // 产出的候选都属于这条输入编码。补全候选的"完整编码"更长，
        // 但那不影响记忆的语义：用户敲的就是这条键。
        //
        // 只在**真有候选**时才渲染：渲染要分配一个字符串，而绝大多数
        // 展开边查不到任何词条。实测这一步值得——见 HANDOFF 的 P4a 数字。
        let key = stele_core::code_key(&self.alphabet, &code);
        for mut c in raw {
            // **补全出来的词要扣分**——与拼写图翻译器同一条规则。
            //
            // 少了这一步会出一个**语义错误**：精确编码方案里敲 `ab` 时
            // 「十」（编码就是 `a b`）会被「木」（编码 `a b c`，词条权重更高）
            // 压下去——那意味着"**打全的排在没打全的后面**"。
            // 实测就是这么发现的：`stele --check` 的冒烟测试从「十」变成了「木」。
            //
            // 数值与拼写层共用同一个常量：它就是"补全"这一类边的标准代价
            // （`ln 0.05 ≈ -3.0`）。
            if c.attr.contains(stele_core::SpellingAttr::COMPLETION) {
                c.score = c.score.saturating_add(COMPLETION_COST);
            }
            // `initial_quality`：这个实例的权重倍率（对数域就是加常数）。
            c.score = c.score.saturating_add(self.quality);
            // 覆盖整段输入。
            c.span = span;
            c.key.clone_from(&key);
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
    /// `initial_quality`（见 [`ExactCodeTranslator::with_initial_quality`]）。
    quality: Score,
}

impl SpellingGraphTranslator {
    /// 由拼写层与词库构造。
    #[must_use]
    pub fn new(spelling: Arc<dyn Spelling>, lexicon: Arc<dyn Lexicon>) -> Self {
        Self {
            spelling,
            lexicon,
            completion: false,
            quality: Score::ZERO,
        }
    }

    /// 打开词条补全。
    #[must_use]
    pub fn with_completion(mut self, on: bool) -> Self {
        self.completion = on;
        self
    }

    /// 设置 `initial_quality`（线性域权重倍率）。
    ///
    /// # 它到底改变什么（审计 §2.G5 点名"解析了没生效"的字段之一）
    ///
    /// RIME 的 `initial_quality` 是**这个翻译器实例**的权重倍率：rime-ice
    /// 用它把英文翻译器（`initial_quality: 1.1`）排在拼音之前。
    /// 语义是"同一个词的分数乘以它"，因此换算到对数域就是**加一个常数**。
    ///
    /// 它必须**逐实例**生效，而不是全局——否则"英文 1.1、拼音 1.0"
    /// 这种最常见的写法就变成了"所有候选都 +0.1 倍"，等于没写。
    #[must_use]
    pub fn with_initial_quality(mut self, q: Option<f64>) -> Self {
        self.quality = quality_of(q);
        self
    }
}

/// `initial_quality`（线性域倍率）→ 对数域加分。
///
/// 非正值按"不改变"处理（`0` 或负数在 RIME 里没有意义；静默地**减掉**
/// 全部分数更糟）。
fn quality_of(q: Option<f64>) -> Score {
    q.filter(|v| *v > 0.0)
        .map_or(Score::ZERO, Score::from_weight)
}

impl Translator for SpellingGraphTranslator {
    fn translate(&self, q: &Query<'_>, span: Span, out: &mut CandidateSink<'_>) {
        let input = q.segment_text;

        // ── 通路 ①：可解释前缀 + 拼写层补全 ──
        //
        // 用 `expand_paths` 而不是 `expand`：前者产出**带消费长度**的路径，
        // 于是 `niha` 的 `ni`+`h`（`h` 是 `hao` 的缩写，只消费了 3 个字节）
        // 也是一条合法路径——剩下的 `a` 是**余码**，留在预编辑串里。
        //
        // 这正是 librime 的音节图做法：`interpreted_length` 可以小于
        // `input_length`（`algo/syllabifier.cc:267-274`），
        // `Dictionary::Lookup` 对图上**每一条边**分别查表、
        // 返回 `map<end_pos, entries>`（`gear/script_translator.cc:705-722`）。
        let mut expansions = Vec::new();
        {
            // **补全开关交给拼写层**——这正是上游的做法：
            // `syllabifier_(..., translator->enable_completion(), ...)`
            // （`gear/script_translator.cc:87-90`），补全发生在
            // `Prism::ExpandSearch`，不在词库那一层。
            let limits = stele_core::PathLimits::default().with_completion(self.completion);
            let mut sink = stele_core::PathSink::new(&mut expansions, 512);
            self.spelling.expand_paths(input, limits, &mut sink);
        }

        // **词典约束搜索**（审计 §2.A 的推荐方向）：
        // 不可能命中任何词条的编码根本不必查表。
        //
        // `has_prefix` 只在词库声明支持前缀查询时才有意义——默认实现返回
        // `false`（"不支持"与"不存在"在类型上无法区分），把它当成
        // "不存在"会**静默丢掉全部候选**，所以先问 `supports_prefix()`。
        let constrained = self.lexicon.supports_prefix();
        let completion = self.completion && self.lexicon.supports_prefix();

        // ── 先判断"要不要造句"，**再**铺开前缀 / 补全候选 ──
        //
        // # 顺序为什么重要（一个真踩到的坑）
        //
        // 造句的触发条件之一是"没有覆盖整串的可靠词条"
        // （librime `gear/script_translator.cc:502-514` 的
        // `has_reliable_phrase`），所以它必须在看完展开之后才能决定。
        // 但**候选缓冲是有上限的**（[`TRANSLATE_CAP`]）：先铺开一百多条
        // 前缀 / 补全候选，造句结果就会被挤掉、**静默消失**。
        // 实测：真实词库下 `haoni` 的「好你」、`woaizhongguo` 的
        // 「我爱中国」都因为这一条根本不出现。
        //
        // 所以先做一次**代价极小的预扫**（命中即停）拿到 `covered_whole`，
        // 把至多一条造句候选先放进缓冲，再铺开其余候选。
        let covered_whole = expansions.iter().any(|exp| {
            if exp.consumed < input.len() || exp.code.len() < 2 {
                return false;
            }
            if constrained && !self.lexicon.has_prefix(&exp.code) {
                return false;
            }
            let mut raw: Vec<Candidate> = Vec::new();
            {
                let mut sink = CandidateSink::new(&mut raw, 1);
                self.lexicon.lookup(&exp.code, &mut sink);
            }
            !raw.is_empty()
        });

        // ── 通路 ③：词图造句（没有精确整词匹配时） ──
        //
        // librime：拼音族的造句是**无条件**的（只有"至少两个音节 +
        // 没有可靠整词"两个条件，`gear/script_translator.cc:502-514`），
        // `enable_sentence` 是**码表族**的开关（`table_translator.h:43`）。
        // 我们按同一语义实现：这里不看 `enable_sentence`。
        if !covered_whole {
            self.make_sentence(input, span, out);
        }

        for exp in &expansions {
            let mut raw: Vec<Candidate> = Vec::new();
            {
                let mut sink = CandidateSink::new(&mut raw, TRANSLATE_CAP);
                self.lexicon.lookup(&exp.code, &mut sink);
            }
            // 词条补全的两个门槛，**都照上游**：
            //
            // 1. **输入必须被完整消费**——上游：
            //    `bool predict_word = translator_->enable_word_completion() &&
            //     start_ + consumed == end_of_input_;`
            //    （`gear/script_translator.cc:461-464`）。只消费了前缀的
            //    路径不做词条补全：它连输入都没走完，谈"补出更长的词"
            //    没有意义，而且会把候选数与查表次数乘起来。
            // 2. **只对规范拼写那条边做**：简拼/纠错的边上再补全会让
            //    候选数量再乘一次，收益极小。
            if completion
                && exp.consumed >= input.len()
                && exp.attr == stele_core::SpellingAttr::NORMAL
            {
                let mut sink = CandidateSink::new(&mut raw, TRANSLATE_CAP);
                self.lexicon.prefix_lookup(&exp.code, true, &mut sink);
            }
            // 没有候选的展开边直接跳过——**键要分配字符串**，
            // 而简拼会产出大量查不到词的展开边（拼写展开是按代价排序的，
            // 便宜的边多得多）。
            if raw.is_empty() {
                continue;
            }
            // **每条展开边一把键**：`nhao` 与 `nihao` 可能展开到**同一条**
            // 编码 `[ni, hao]`，于是渲染出**同一把**键 `ni'hao`——
            // 跨拼法共享记忆就是在这里自动成立的，不需要记忆层做反查。
            let key = stele_core::code_key(self.spelling.alphabet(), &exp.code);
            // **候选只覆盖它真正消费掉的那一段输入**，而不是整段：
            // `niha` 的 `[ni][hao]` 只消费 3 个字节（`a` 是余码）。
            // `Span` 的定义本来就是"覆盖输入串的哪一段"（字节），
            // 所以这里不需要新字段——旧实现写死成 `span` 才是把信息丢了。
            let consumed_span = Span::new(span.start, span.start + exp.consumed);
            // **余码越少越好**：一条只消费了 `ni`（余码 `hao`）的候选
            // 不该压过消费整串的 `ni hao`。
            //
            // 为什么不能只靠词条权重：单字「你」在词库里的权重远高于
            // 词「你好」，于是"敲 `nihao` 第一个候选是「你」"——而用户
            // 敲了五个字母。librime 的 `has_reliable_phrase` 走的是
            // 另一条机制（把整串匹配单独挑出来排前面），这里用**可加的
            // 余码罚分**达成同一效果，且不改变跨来源的排序规则。
            //
            // 数值：每字节 4000 毫对数（≈ ln 55）。它必须大于"单字权重
            // 与词权重之差"的量级（实测几千毫对数），又不能让
            // "多覆盖一个字节"压过"这个词根本不存在"。
            let remainder = input.len().saturating_sub(exp.consumed);
            let rest_penalty = i32::try_from(remainder).map_or(i32::MIN, |r| {
                PREFIX_PENALTY_MILLI_PER_BYTE.saturating_mul(r)
            });
            for mut c in raw {
                // 候选的分数 = 词条分数 + 这条边的代价 - 余码罚分。
                // **"简拼天然排在精确匹配之后"靠的是边代价**：
                // 缩写边的代价是负的，不需要任何额外规则。
                c.score = c
                    .score
                    .saturating_add(exp.cost)
                    .saturating_add(Score::from_milli_log(rest_penalty));
                if completion && c.attr.contains(stele_core::SpellingAttr::COMPLETION) {
                    // 补全出来的词**扣一次分**：它是"猜你要打这个"，
                    // 不该与真正打全的词平起平坐。代价数值**属于方案数据**
                    // （RIME 的 `enable_word_completion` 也有对应的权重扣减），
                    // 这里取与拼写代数里 `Completion` 边同量级的值。
                    c.score = c.score.saturating_add(COMPLETION_COST);
                } else {
                    // 属性取并集：只要这条边经过了变形，候选就不是"精确"的。
                    c.attr = c.attr.union(exp.attr);
                }
                // `initial_quality`：这个实例的权重倍率。
                c.score = c.score.saturating_add(self.quality);
                c.span = consumed_span;
                c.key.clone_from(&key);
                out.push(c);
            }
            if out.len() >= TRANSLATE_CAP {
                break;
            }
        }
    }
}

/// 每留下一个**未消费字节**扣多少分（毫对数，**负值**）。
///
/// 「敲了五个字母却只匹配了一个字」应该排在「匹配了两个音节」之后。
/// 见 `translate` 里的使用点。
const PREFIX_PENALTY_MILLI_PER_BYTE: i32 = -4_000;

/// 造句的编码单元奖励（毫对数/单元）。
///
/// 用途：在词图上偏向"用更少、更长的词覆盖同一段输入"。
/// `300` 毫对数 ≈ `ln(1.35)`——足以在"同一个词条分数"时选择更长的那条边，
/// 又不足以压过词条本身的权重差异（词条权重是几千毫对数起步）。
///
/// **这不是语言模型**：第一版只有词频与长度策略。上游没有 grammar 时
/// 也是动态规划（`gear/poet.cc:246-253`），不是神经模型。
const SENTENCE_UNIT_BONUS_MILLI: i32 = 300;

/// 造句里**每多一个词**扣多少分（毫对数）。
///
/// # 它解决的具体问题
///
/// 词库里**单字**的权重往往高于**词**（「是」比「世界」常见得多），
/// 于是纯按词频求和会把 `nihaoshijie` 拼成「你好**是界**」而不是
/// 「你好**世界**」——两者都是 4 个编码单元、都是 3 个/2 个词。
///
/// 上游的做法是给短语（phrase）词条额外的权重加成；我们没有那个数据，
/// 于是等价地在**动态规划的得分里**惩罚词数：覆盖同一段输入时，
/// **词越少越好**（也就是词越长越好）。这是"第一版不接语言模型"的
/// 诚实替代，不是语言模型。
const SENTENCE_WORD_PENALTY_MILLI: i32 = -12_000;

/// 造句候选比精确词条低多少（毫对数）。
///
/// 它保证"猜出来的句子"排在"真有这个词"之后——`Origin::Sentence`
/// 已经在排序时被降级（`sort::origin_rank`），这里再扣一次是**双保险**：
/// 排序里的降级管的是"跨来源"的比较，这个扣分管的是"同一来源内部"。
const SENTENCE_PENALTY_MILLI: i32 = -1_500;

impl SpellingGraphTranslator {
    /// 在**词图**上做有界动态规划，拼出词库里没有的词。
    ///
    /// # 词图是什么
    ///
    /// `map<起点, map<终点, 词条列表>>`：从位置 `i` 到位置 `j` 有哪些词。
    /// 上游就是这个名字与这个形状（`reference/rime-sentence-and-completion.md` §4）。
    ///
    /// # 为什么必须"有界"
    ///
    /// 每个起始位置各展开一次，所以预算是
    /// `起始位置数 × PathLimits::sentence_scan()`；词的条数与去重后的
    /// 路径数都有硬上限。**不这么做的话，"为每个位置各扫一遍词库"
    /// 就是审计点名的那种无界行为。**
    ///
    /// # 不做的事
    ///
    /// - 不扫全词库（只查拼写图给出的那些编码）；
    /// - 不把**整串就是一个词**的情形当成句子（上游同款排除，
    ///   `gear/poet.cc:206-208`）；
    /// - 不无限组合（边严格向前，DP 无环；词数上限 `MAX_SENTENCE_WORDS`）。
    fn make_sentence(&self, input: &str, span: Span, out: &mut CandidateSink<'_>) {
        if input.len() < 2 {
            return;
        }
        let constrained = self.lexicon.supports_prefix();

        // ① 起点集合：从 0 出发能走到的地方（含 0）。
        let mut starts: Vec<usize> = vec![0];
        let mut probe: Vec<stele_core::SpellingPath> = Vec::new();
        {
            let mut sink = stele_core::PathSink::new(&mut probe, 64);
            self.spelling
                .expand_paths(input, stele_core::PathLimits::sentence_scan(), &mut sink);
        }
        for p in &probe {
            if p.consumed < input.len() {
                starts.push(p.consumed);
            }
        }
        starts.sort_unstable();
        starts.dedup();

        // ② 词图边：`(start, end, text, score, units)`。
        let mut edges: Vec<WordEdge> = Vec::new();
        for &start in &starts {
            let mut paths: Vec<stele_core::SpellingPath> = Vec::new();
            {
                let limit = stele_core::PathLimits::sentence_scan();
                let mut sink = stele_core::PathSink::new(&mut paths, limit.max_paths);
                self.spelling
                    .expand_paths(&input[start..], limit, &mut sink);
            }
            for p in &paths {
                if p.consumed == 0 {
                    continue;
                }
                if constrained && !self.lexicon.has_prefix(&p.code) {
                    continue;
                }
                let mut words: Vec<Candidate> = Vec::new();
                {
                    let mut sink = CandidateSink::new(&mut words, 8);
                    self.lexicon.lookup(&p.code, &mut sink);
                }
                for w in words {
                    edges.push(WordEdge {
                        start,
                        end: start + p.consumed,
                        text: w.text,
                        score: w.score.as_milli_log() + p.cost.as_milli_log(),
                        units: p.code.len(),
                    });
                }
            }
        }
        if edges.is_empty() {
            return;
        }

        // ③ 动态规划：`best[j]` = 覆盖输入 `[0, j)` 的最优分数与来源边。
        //
        // 边严格向前（`end > start`），因此这个 DP **无环**，
        // 一次正向扫描即可——不需要迭代到不动点。
        let len = input.len();
        let mut best: Vec<Option<(i32, usize)>> = vec![None; len + 1]; // (分数, 边下标)
        let mut words_used: Vec<usize> = vec![0; len + 1];
        best[0] = Some((0, usize::MAX));
        for j in 1..=len {
            for (ei, e) in edges.iter().enumerate() {
                if e.end != j {
                    continue;
                }
                let Some((prev_score, _)) = best[e.start] else {
                    continue;
                };
                let words = words_used[e.start] + 1;
                if words > MAX_SENTENCE_WORDS {
                    continue;
                }
                // 单元奖励：偏向"更少、更长"的词。见常量文档。
                let bonus = i32::try_from(e.units)
                    .map_or(0, |u| SENTENCE_UNIT_BONUS_MILLI.saturating_mul(u));
                let total = prev_score
                    .saturating_add(e.score)
                    .saturating_add(bonus)
                    .saturating_add(SENTENCE_WORD_PENALTY_MILLI)
                    .saturating_add(SENTENCE_PENALTY_MILLI);
                // 平局时取**词数更少**的那条；词数也相同则保留先到的
                // （边的枚举顺序是确定的：起点升序、路径按排序键、
                //   同码词条按分数降序——所以"先到"是可复现的）。
                let better = match best[j] {
                    None => true,
                    Some((cur, _)) => total > cur || (total == cur && words < words_used[j]),
                };
                if better {
                    best[j] = Some((total, ei));
                    words_used[j] = words;
                }
            }
        }

        // ④ 回溯出词序列，只接受**至少两个词**（整串是一个词不算造句）。
        let Some(()) = best[len].map(|_| ()) else {
            return;
        };
        let mut seq: Vec<usize> = Vec::new();
        let mut pos = len;
        while pos > 0 {
            let Some((_, ei)) = best[pos] else { return };
            if ei == usize::MAX {
                break;
            }
            seq.push(ei);
            pos = edges[ei].start;
        }
        if pos != 0 || seq.len() < 2 {
            return;
        }
        seq.reverse();

        let mut text = String::new();
        for &ei in &seq {
            text.push_str(&edges[ei].text);
        }
        if text.is_empty() {
            return;
        }
        // 与已经产出的候选去重：同一个文本不再重复给。
        // （单字/整词候选已经在上面的通路 ① 里给过了。）
        if out.iter().any(|c| c.text == text) {
            return;
        }
        let score = best[len].map_or(0, |(s, _)| s);
        out.push(Candidate {
            text,
            comment: None,
            score: Score::from_milli_log(score),
            origin: Origin::Sentence,
            // **造句是猜的**：属性上标出来，前端与学习都能区别对待。
            attr: stele_core::SpellingAttr::NORMAL,
            span: Span::new(span.start, span.start + len),
            lane: Lane::Input,
            kind: stele_core::CandidateKind::Normal,
            key: None,
        });
    }
}

/// 一条句子最多几个词。超过它说明这条"句子"已经不是人话，而是一堆单字。
const MAX_SENTENCE_WORDS: usize = 8;

/// 词图里的一条边：从 `start` 到 `end` 有一个词。
///
/// `score` 是**毫对数**（`i32`）：词条分数 + 拼写边的代价，
/// 动态规划在这个域上做整数加法，因此结果可复现。
struct WordEdge {
    start: usize,
    end: usize,
    text: String,
    score: i32,
    units: usize,
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
            key: None,
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
        t.translate(&query("nihao", &opts, &ctx), Span::new(0, 5), &mut sink);
        assert_eq!(buf[0].text, "你好");
        assert_eq!(buf[0].attr, SpellingAttr::NORMAL);
        let canonical_score = buf[0].score;

        // 简拼：同一个词，但带 ABBREV 属性且分数更低。
        let mut buf2 = Vec::new();
        let mut sink2 = CandidateSink::new(&mut buf2, 16);
        t.translate(&query("nh", &opts, &ctx), Span::new(0, 2), &mut sink2);
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
        t.translate(&query("zzz", &opts, &ctx), Span::new(0, 3), &mut sink);
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
