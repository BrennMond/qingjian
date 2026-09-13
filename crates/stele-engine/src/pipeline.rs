//! # Pipeline
//!
//! 中文职责：把零件按方案声明的顺序装配起来，并实现"一次按键 → 一组候选"。
//! English role: assemble the components in the schema's declared order and
//! implement "one keystroke → a set of candidates".
//! 架构位置：`stele-core::Pipeline` 的实现；`stele-core` 与 `stele-engine`
//! 之间的唯一接缝。
//!
//! # 顺序是数据，但不是任意的
//!
//! 组件**执行的顺序由方案声明**，而且**顺序本身就是语义**——
//! 滤镜尤其如此：把"置顶候选"放在"插入 Emoji 候选"之后，置顶就会被挤掉。
//!
//! 这与 DSH 的"行序不承载语义"**有意不同**：那边的事件监听器是无序广播，
//! 我们的是有序变换。见 `docs/engine-design.md` §6.1b。
//!
//! # 一次 `compose` 的五个阶段
//!
//! ```text
//! ① 扫描     recognizer 扫一遍输入 → 认领（哪一段属于哪个标签）
//! ② 切分     切分器按顺序上场，每人认一段；兜底切分器吃剩下的
//! ③ 绑定     由分段算出"这次有哪些标签" → 只跑声明负责这些标签的翻译器
//! ④ 后处理   滤镜（顺序即语义）→ 重排器（受上界与守卫约束）
//! ⑤ 写回     页裁剪、候选计数、预编辑串、分段
//! ```
//!
//! **为什么扫描要单独一步**：`matcher` 与 `affix_segmentor` 都要用同一份
//! 扫描结果，而它们对"正文从哪开始"的解读不同（前者整段，后者剥前缀）。
//! 分开之后扫描只做一次。

use std::sync::Arc;
use stele_core::{
    Candidate, CandidateSink, Filter, Lane, Pipeline, ProcessResult, Processor, Query, QueryView,
    Ranker, Segment, SegmentStatus, Segmentation, SessionState, Span, Tag, Translator,
};

use crate::segmentor::{InputScan, Recognizer};

/// 一次组装出来的候选总量上限。
pub const CANDIDATE_CAP: usize = 200;

/// 一页显示多少个候选（`navigator` 用）。
///
/// RIME 的默认是 5，雾凇的方案里是 9。**它是显示参数，不是引擎参数**——
/// 放在这里当默认值，方案可以改。
pub const DEFAULT_PAGE_SIZE: usize = 9;

/// `key_binder` 的 `send` 最多换几轮。
///
/// 换取按键**必须**有轮数上限：一条 `accept: a, send: a` 的绑定会让
/// 按键处理无限循环（用户看到的是输入法卡死）。三轮足够表达
/// "Shift+空格 → 空格 → 确认"这种链，又不至于让配置错误变成死循环。
const REBIND_ROUNDS: usize = 3;

/// 组件流水线的具体实现。
pub struct PipelineImpl {
    /// 本方案的主分段标签（兜底切分器与"没有切分器时的整串"用它）。
    tag: Tag,
    /// 识别器：每轮扫描的入口。
    recognizer: Option<Box<Recognizer>>,
    segmentors: Vec<Box<dyn stele_core::Segmentor>>,
    processors: Vec<Box<dyn Processor>>,
    translators: Vec<Box<dyn Translator>>,
    filters: Vec<Box<dyn Filter>>,
    rankers: Vec<Arc<dyn Ranker>>,
    cap: usize,
    /// 每页候选数。
    page_size: usize,
    /// 预编辑串的编码单元分隔符（`speller.delimiter`）。
    ///
    /// 有它时，预编辑串按**切分结果**渲染成 `ni'hao` 而不是 `nihao`——
    /// 这正是 RIME 的 `speller/delimiter` 的作用，也让用户能看清引擎
    /// 把输入切成了什么。
    delimiter: Option<char>,
    /// 用于渲染预编辑串的拼写层（拼写 → 编码单元 → 规范写法）。
    spelling: Option<Arc<dyn stele_core::Spelling>>,
    /// `key_binder` 在处理器链里的下标（用来让"换来的按键"跳过它）。
    binder_index: Option<usize>,
    /// 上一次 compose 用过的扫描结果（处理器改动输入后由 compose 重算）。
    scan: InputScan,
    /// **本次翻译该看的那一段文本**。
    ///
    /// 它必须是流水线自己的字段（而不是 `compose` 里的局部变量），
    /// 因为 [`Query`] 借用了它，而 `Query` 要活过整段翻译。
    segment_text: String,
}

impl PipelineImpl {
    /// 由各组件的**有序**列表构造。
    ///
    /// 顺序由方案数据决定，这里不重排、不排序——**顺序是语义**。
    #[must_use]
    pub fn new(
        tag: Tag,
        processors: Vec<Box<dyn Processor>>,
        translators: Vec<Box<dyn Translator>>,
        filters: Vec<Box<dyn Filter>>,
        rankers: Vec<Arc<dyn Ranker>>,
        cap: usize,
    ) -> Self {
        Self {
            tag,
            recognizer: None,
            segmentors: Vec::new(),
            processors,
            translators,
            filters,
            rankers,
            cap,
            page_size: DEFAULT_PAGE_SIZE,
            delimiter: None,
            spelling: None,
            binder_index: None,
            scan: InputScan::default(),
            segment_text: String::new(),
        }
    }

    /// 装上识别器与切分器。
    #[must_use]
    pub fn with_segmentors(
        mut self,
        recognizer: Option<Box<Recognizer>>,
        segmentors: Vec<Box<dyn stele_core::Segmentor>>,
    ) -> Self {
        self.recognizer = recognizer;
        self.segmentors = segmentors;
        self
    }

    /// 记下 `key_binder` 在处理器链里的位置（供"换来的按键"跳过它）。
    #[must_use]
    pub fn with_binder_index(mut self, index: Option<usize>) -> Self {
        self.binder_index = index;
        self
    }

    /// 设置预编辑串的编码单元分隔符与渲染所需的拼写层。
    #[must_use]
    pub fn with_preedit(
        mut self,
        delimiter: Option<char>,
        spelling: Option<Arc<dyn stele_core::Spelling>>,
    ) -> Self {
        self.delimiter = delimiter;
        self.spelling = spelling;
        self
    }

    /// 设置每页候选数。
    #[must_use]
    pub fn with_page_size(mut self, page_size: usize) -> Self {
        self.page_size = page_size.max(1);
        self
    }

    /// 本方案的标签。
    #[must_use]
    pub fn tag(&self) -> Tag {
        self.tag
    }

    /// 切分器数量——供调试与测试。
    #[must_use]
    pub fn segmentor_count(&self) -> usize {
        self.segmentors.len()
    }

    /// 是否有识别器。
    #[must_use]
    pub fn has_recognizer(&self) -> bool {
        self.recognizer.is_some()
    }

    /// 零件数量（处理器, 翻译器, 过滤器, 重排器）——供调试与测试。
    #[must_use]
    pub fn component_counts(&self) -> (usize, usize, usize, usize) {
        (
            self.processors.len(),
            self.translators.len(),
            self.filters.len(),
            self.rankers.len(),
        )
    }

    /// 切分器给输入打的全部标签（按声明顺序去重）。
    ///
    /// 这是**装配期**的答案："这个方案能产出哪些标签"。
    /// 它决定翻译器的绑定能不能被满足——方案里写了 `table_translator@x`
    /// 却没有任何切分器产出 `x`，那就是一处**永远不生效的配置**，
    /// 装载期必须报出来。
    #[must_use]
    pub fn declared_tags(&self) -> Vec<Tag> {
        let mut out: Vec<Tag> = Vec::new();
        for s in &self.segmentors {
            for t in s.tags() {
                if !out.contains(t) {
                    out.push(*t);
                }
            }
        }
        if out.is_empty() {
            out.push(self.tag);
        }
        out
    }

    /// 当前输入下**实际出现**的标签（分段的标签并集）。
    ///
    /// 与 [`Self::declared_tags`] 的区别：这个是运行期的。
    /// 翻译器只跑"有人要它"的那些——`punct_translator` 不该在每次敲字母时
    /// 都去查一遍标点表。
    fn active_tags(&self, segs: &Segmentation) -> Vec<Tag> {
        let mut out: Vec<Tag> = Vec::new();
        for s in &segs.segments {
            for t in &s.tags {
                if !out.contains(t) {
                    out.push(*t);
                }
            }
        }
        if out.is_empty() {
            out.push(self.tag);
        }
        out
    }

    /// 用**最优切分**渲染预编辑串，并给出每个编码单元的字节区间。
    ///
    /// 返回 `(预编辑串, 各单元的区间)`。没有拼写层时退回"整串一段"。
    ///
    /// 为什么两件事一起做：预编辑串的分隔（`ni'hao`）与按编码单元退格
    /// **用的是同一份切分结果**。分开算就可能不一致——那种 bug 很难看，
    /// 用户会看到"显示的边界"和"退格的边界"对不上。
    fn segment_for_display(&self, input: &str) -> (String, Vec<Span>) {
        let Some(sp) = self.spelling.as_ref() else {
            return (input.to_owned(), vec![Span::new(0, input.len())]);
        };
        let mut buf = Vec::new();
        {
            let mut sink = stele_core::ExpansionSink::new(&mut buf, 1);
            sp.expand(input, &mut sink);
        }
        let Some(best) = buf.first() else {
            return (input.to_owned(), vec![Span::new(0, input.len())]);
        };

        let alphabet = sp.alphabet();
        let mut parts: Vec<&str> = Vec::with_capacity(best.code.len());
        let mut spans: Vec<Span> = Vec::with_capacity(best.code.len());
        let mut pos = 0usize;
        for u in &best.code {
            let Some(t) = alphabet.text(*u) else { continue };
            parts.push(t);
            spans.push(Span::new(pos, pos + t.len()));
            pos += t.len();
        }
        if parts.is_empty() {
            return (input.to_owned(), vec![Span::new(0, input.len())]);
        }
        let text = match self.delimiter {
            Some(d) => parts.join(&d.to_string()),
            None => input.to_owned(),
        };
        // 拼不回去说明切分与输入串不一致（理论上不该发生）——
        // 这时宁可退回整串一段，也不要给出错位的边界。
        if pos != input.len() {
            return (input.to_owned(), vec![Span::new(0, input.len())]);
        }
        (text, spans)
    }

    /// 跑一遍切分：扫描 → 各切分器按顺序上场 → 兜底。
    fn segment(&mut self, input: &str) -> Segmentation {
        let mut segs = Segmentation::default();
        if input.is_empty() {
            return segs;
        }
        if self.segmentors.is_empty() {
            let mut s = Segment::new(Span::new(0, input.len()));
            s.status = SegmentStatus::Guess;
            s.tags.push(self.tag);
            segs.segments.push(s);
            return segs;
        }
        // 每个切分器按顺序有机会"接着当前进度往下切"，直到没人接。
        while segs.segments.last().map_or(0, |s| s.span.end) < input.len() {
            let before = segs.segments.len();
            let opts = stele_core::Options::new();
            let ctx = stele_core::Context::default();
            let comp = stele_core::Composition::default();
            let q = Query {
                input,
                caret: input.len(),
                options: &opts,
                context: &ctx,
                composition: &comp,
                segment_text: input,
            };
            for s in &self.segmentors {
                if s.proceed(&q, &mut segs) {
                    break;
                }
            }
            if segs.segments.len() == before {
                // **没有任何切分器愿意接手** —— 这不该发生（兜底切分器
                // 的存在就是保证这件事）。真发生了就整段收下，
                // 绝不能死循环：用户会看到输入法卡死。
                let pos = segs.segments.last().map_or(0, |s| s.span.end);
                let mut s = Segment::new(Span::new(pos, input.len()));
                s.status = SegmentStatus::Guess;
                s.tags.push(self.tag);
                segs.segments.push(s);
                break;
            }
        }
        segs
    }

    /// 把"要交给翻译器的那段正文"写进 `self.segment_text`。
    ///
    /// `offset` 是正文在输入串里的字节起点：`affix_segmentor` 之外的分段
    /// 是 0（整串都是正文），带词缀的是前缀长度之后。
    fn set_body(&mut self, input: &str, offset: usize) {
        self.segment_text.clear();
        if offset < input.len() {
            self.segment_text.push_str(&input[offset..]);
        }
    }
}

impl Pipeline for PipelineImpl {
    fn process_key(&mut self, state: &mut SessionState, key: &stele_core::Key) -> ProcessResult {
        let first = self.dispatch(state, key, None);
        // `key_binder` 可能要求"换成另一个按键再走一遍"。
        for _ in 0..REBIND_ROUNDS {
            let Some(next) = state.sent_keys.pop() else {
                break;
            };
            // **换来的按键从 `key_binder` 之后开始派发。**
            //
            // 这不是优化，是正确性：素材里 `{accept: space, send: space}`
            // 这种"把空格换成空格"的绑定在 RIME 方案里很常见（它的意思是
            // "让空格走确认那条路"）。若整链重跑，`key_binder` 会再次接住
            // 这个空格并再发一次——**轮数上限挡住了死循环，却也让按键
            // 永远到不了选择器/编辑器**：症状是"空格没反应"，
            // 而配置看起来完全正常。端到端测试抓到了它。
            let _ = self.dispatch(state, &next, self.binder_index);
        }
        state.sent_keys.clear();
        first
    }

    fn compose(&mut self, state: &mut SessionState, out: &mut Vec<Candidate>) {
        out.clear();

        if state.composition.input.is_empty() {
            state.composition.preedit.clear();
            state.composition.segments.clear();
            state.candidate_count = 0;
            state.candidate_pages = 0;
            state.candidate_page = 0;
            return;
        }

        let input = state.composition.input.clone();
        let span = Span::new(0, input.len());

        // ── ① 扫描 + ② 切分 ──
        if let Some(r) = self.recognizer.as_ref() {
            self.scan = r.scan(&input);
        } else {
            self.scan = InputScan::default();
        }
        // **把新扫描结果发给每一个切分器。**
        //
        // 漏掉这一步的后果见 `Segmentor::rescan` 的文档：整条
        // "识别 → 切分 → 绑定"链静默断掉，没有任何报错。
        for s in &mut self.segmentors {
            s.rescan(&self.scan.view());
        }
        let segs = self.segment(&input);
        let active = self.active_tags(&segs);


        // ── ③ 翻译 ──
        //
        // 只跑"声明负责这些标签"的翻译器，而且**每个标签的翻译器看到的是
        // 自己那一段的正文**（带词缀的分段要去掉前缀——见
        // [`stele_core::Segmentor::body_start`]）。
        //
        // 不声明标签的翻译器一律跑（`targets()` 返回空切片 = 不声明）——
        // 那是兜底翻译器与老方案的行为，改成"不声明就不跑"会让所有
        // 既有方案失效。
        let bodies: Vec<(Tag, usize)> = self
            .segmentors
            .iter()
            .filter_map(|s| s.body_start())
            .collect();

        let opts = state.options.clone();
        let ctx = state.context.clone();
        let comp = state.composition.clone();

        // 先跑"不绑定标签"的翻译器（兜底、老方案的主翻译器）。
        self.set_body(&input, 0);
        {
            let body = std::mem::take(&mut self.segment_text);
            let q = Query {
                input: &input,
                caret: state.composition.caret,
                options: &opts,
                context: &ctx,
                composition: &comp,
                segment_text: &body,
            };
            let mut sink = CandidateSink::new(out, self.cap);
            for t in &self.translators {
                if t.targets().is_empty() && t.accepts(&active) {
                    t.translate(&q, span, &mut sink);
                }
            }
            self.segment_text = body;
        }

        // 再按标签跑绑定了标签的翻译器，各自看到自己那一段的正文。
        for tag in &active {
            let offset = bodies
                .iter()
                .find(|(t, _)| t == tag)
                .map_or(0, |(_, off)| *off);
            self.set_body(&input, offset);
            let body = std::mem::take(&mut self.segment_text);
            let q = Query {
                input: &input,
                caret: state.composition.caret,
                options: &opts,
                context: &ctx,
                composition: &comp,
                segment_text: &body,
            };
            let mut sink = CandidateSink::new(out, self.cap);
            for t in &self.translators {
                let targets = t.targets();
                if !targets.is_empty() && targets.contains(tag) && t.accepts(&active) {
                    t.translate(&q, span, &mut sink);
                }
            }
            self.segment_text = body;
        }

        // ── ④ 滤镜与重排 ──
        {
            let opts = state.options.clone();
            let ctx = state.context.clone();
            let comp = state.composition.clone();
            let q = Query {
                input: &input,
                caret: state.composition.caret,
                options: &opts,
                context: &ctx,
                composition: &comp,
                segment_text: &input,
            };

            // 滤镜：顺序即语义。
            for f in &self.filters {
                if f.applies_to(&active) {
                    f.apply(&q, span, out);
                }
            }

            // 重排器：只允许加分，且受各自的 bonus_limit 约束。
            if !self.rankers.is_empty() {
                // 只在 debug 构建里为前置检查保存一份快照 ——
                // 发布构建不需要它，也就不付这份拷贝的代价。
                #[cfg(debug_assertions)]
                let before: Vec<Candidate> = out.clone();
                let view = QueryView {
                    input: &input,
                    context: &ctx,
                    lane: Lane::Input,
                };
                for r in &self.rankers {
                    r.rerank(&view, out);
                }
                // "精确优先"铁律的开发期检查：重排器不许造成跨类倒置。
                #[cfg(debug_assertions)]
                debug_assert!(
                    !stele_core::has_cross_class_inversion(&before, out, Lane::Input),
                    "重排器把猜测候选顶到了精确匹配之前"
                );
            }
        }

        // ── ⑤ 写回 ──
        //
        // 页信息必须先算：`navigator` 在**下一次按键**时用它决定翻不翻得动。
        state.candidate_count = out.len();
        state.candidate_pages = out.len().div_ceil(self.page_size);
        if state.candidate_page >= state.candidate_pages {
            // 输入变短了（退格）→ 页号可能越界，钳回最后一页。
            state.candidate_page = state.candidate_pages.saturating_sub(1);
        }
        // 视图裁剪：只把当前页交给前端。
        //
        // **这是"视图翻页"而不是"按页查询"**（见 `Navigator` 的说明）：
        // 候选全都算出来了，这里只是选一段给前端看。
        if state.candidate_pages > 1 {
            let start = state.candidate_page * self.page_size;
            let end = (start + self.page_size).min(out.len());
            let page: Vec<Candidate> = out[start..end].to_vec();
            *out = page;
        }

        // 预编辑串与分段**用同一份切分结果**得出。
        //
        // 预编辑串带编码单元分隔不只是好看：它让用户看见引擎把输入切成了什么。
        // 当 `xian` 被切成 `xi'an` 而不是 `xian` 时，用户能立刻明白候选为什么不对。
        let (preedit, spans) = self.segment_for_display(&input);
        state.composition.preedit = preedit;

        let mut out_segs = Segmentation::default();
        for seg in &segs.segments {
            let mut s = seg.clone();
            // 候选只挂在**覆盖整串**的那一段上，且要经过跨段合并与最终排序；
            // 逐单元的分段只用于显示与退格。
            if s.span.start == 0 && s.span.end >= input.len() {
                s.candidates.extend_from_slice(out);
            }
            out_segs.segments.push(s);
        }
        // 把最优切分产生的逐单元边界也放进去（退格按它们走）。
        if out_segs.segments.len() <= 1 && spans.len() > 1 {
            out_segs.segments.clear();
            for sp in spans {
                let mut s = Segment::new(sp);
                s.status = SegmentStatus::Guess;
                s.tags.push(self.tag);
                s.candidates.extend_from_slice(out);
                out_segs.segments.push(s);
            }
        }
        state.composition.segments = out_segs;
    }
}

impl PipelineImpl {
    /// 把一次按键派发给处理器链。
    ///
    /// `skip_before`：从**这个下标**开始派发（`None` = 从头）。
    /// 换来的按键用它跳过"产生它的那个重绑定器"，见 `process_key`。
    fn dispatch(
        &mut self,
        state: &mut SessionState,
        key: &stele_core::Key,
        skip_before: Option<usize>,
    ) -> ProcessResult {
        let start = skip_before.map_or(0, |i| i + 1);
        for p in self.processors.iter_mut().skip(start) {
            if !p.enabled(&state.options) {
                continue;
            }
            let r = p.process(state, key);
            match r {
                // 明确还给系统：**立即停止**，后面的处理器不再有机会。
                ProcessResult::Rejected => return ProcessResult::Rejected,
                // 我不管，但后面的人可能管。
                ProcessResult::Noop => {}
                ProcessResult::Accepted => return ProcessResult::Accepted,
                // `#[non_exhaustive]`：新增变体会在此处编译失败（D30）。
                // 目前把它当作 Noop 处理（继续问后面的处理器）。
                _ => continue_checking(),
            }
        }
        ProcessResult::Noop
    }
}

/// 对 `#[non_exhaustive]` 枚举的未知变体的保守处理：继续问后面的处理器。
///
/// 它被抽成函数，是为了让"未知变体走这条分支"这件事**只有一个定义点**——
/// 将来真的加了变体，改这里一处即可。
fn continue_checking() {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::Uniquifier;
    use crate::lexicon::InMemoryLexicon;
    use crate::processor::{Editor, Selector, Speller};
    use crate::spec::{At, RecogPattern, RecognizerSpec};
    use crate::spelling::{Rule, SpellingTable};
    use crate::translator::{EchoTranslator, SpellingGraphTranslator};
    use stele_core::{CodeAlphabet, Key, ProcessResult};

    fn build() -> PipelineImpl {
        let alphabet = CodeAlphabet::new(vec!["ni".into(), "hao".into()]);
        let table = Arc::new(SpellingTable::compile(
            alphabet.clone(),
            &[Rule::abbrev(1, stele_core::Score::from_weight(0.5)).unwrap()],
        ));
        let lex = Arc::new(
            InMemoryLexicon::from_entries(alphabet, &[(vec!["ni", "hao"], "你好", 100.0)]).unwrap(),
        );

        PipelineImpl::new(
            "abc",
            vec![
                Box::new(Speller::default()),
                Box::new(Editor::default()),
                Box::new(Selector),
            ],
            vec![
                Box::new(SpellingGraphTranslator::new(table, lex)),
                Box::new(EchoTranslator::new()),
            ],
            vec![Box::new(Uniquifier)],
            vec![],
            CANDIDATE_CAP,
        )
    }

    fn type_keys(p: &mut PipelineImpl, keys: &str) -> Vec<Candidate> {
        let mut state = SessionState::default();
        let mut out = Vec::new();
        for c in keys.chars() {
            p.process_key(&mut state, &Key::ch(c));
            p.compose(&mut state, &mut out);
        }
        p.finalize(&mut out);
        out
    }

    #[test]
    fn pipeline_translates_and_ranks() {
        let mut p = build();
        let cands = type_keys(&mut p, "nihao");
        assert_eq!(cands[0].text, "你好");
    }

    #[test]
    fn echo_fallback_is_present_but_last_for_unknown_input() {
        let mut p = build();
        let cands = type_keys(&mut p, "zzz");
        // 一定有东西可以上屏 —— 这是兜底翻译器的承诺。
        assert!(!cands.is_empty());
        let literal = cands
            .iter()
            .find(|c| c.text == "zzz")
            .expect("原样上屏候选必须存在");
        assert_eq!(literal.origin, stele_core::Origin::Literal);
    }

    #[test]
    fn rejected_key_stops_the_chain() {
        let mut p = build();
        let mut state = SessionState::default();
        let ctrl_c = Key::press(stele_core::KeyCode::Char('c'), stele_core::Modifiers::CTRL);
        assert_eq!(
            p.process_key(&mut state, &ctrl_c),
            ProcessResult::Rejected,
            "Ctrl+C 必须还给系统，而不是被吞掉"
        );
        assert!(state.composition.input.is_empty());
    }

    #[test]
    fn empty_input_yields_no_candidates() {
        let mut p = build();
        let mut state = SessionState::default();
        let mut out = vec![];
        p.compose(&mut state, &mut out);
        assert!(out.is_empty());
        assert!(state.composition.preedit.is_empty());
    }

    #[test]
    fn segments_carry_the_scheme_tag() {
        let mut p = build();
        let mut state = SessionState::default();
        p.process_key(&mut state, &Key::ch('n'));
        let mut out = vec![];
        p.compose(&mut state, &mut out);

        assert_eq!(state.composition.segments.segments.len(), 1);
        let seg = &state.composition.segments.segments[0];
        assert_eq!(seg.tags, vec!["abc"]);
        assert_eq!(seg.status, SegmentStatus::Guess);
        assert_eq!(p.tag(), "abc");
    }

    #[test]
    fn a_recognized_prefix_gets_its_own_segment() {
        // 识别器认出 `uUni` 这整段属于 `radical_lookup`，
        // 兜底切分器**不许**把它吞掉。
        let alphabet = CodeAlphabet::new(vec!["ni".into(), "hao".into()]);
        let table = Arc::new(SpellingTable::compile(alphabet.clone(), &[]));
        let lex = Arc::new(
            InMemoryLexicon::from_entries(alphabet, &[(vec!["ni", "hao"], "你好", 100.0)]).unwrap(),
        );
        let mut tag_table = crate::tag::TagTable::new();
        let rec = Recognizer::new(
            &RecognizerSpec {
                import_preset: None,
                patterns: vec![RecogPattern {
                    name: "radical_lookup".into(),
                    leading: "uU".into(),
                    regex: "^uU[a-z]+$".into(),
                    trailing: None,
                    at: At::new(1),
                }],
            },
            &mut tag_table,
        )
        .unwrap();
        // 切分器**故意用空扫描结果构造**：真正的扫描结果由流水线在
        // `compose` 里发下来。这条测试因此同时守着 `Segmentor::rescan`
        // 那条链——漏发时切分器会一直用空扫描，于是整段退化成 `abc`。
        let matcher = crate::segmentor::Matcher::new(
            crate::segmentor::InputScan::default(),
            vec!["radical_lookup"],
        );
        let abc = crate::segmentor::CodingSegmentor::new(
            "abc",
            crate::segmentor::InputScan::default(),
        );

        let mut p = PipelineImpl::new(
            "abc",
            vec![Box::new(Speller::default()), Box::new(Editor::default())],
            vec![Box::new(SpellingGraphTranslator::new(table, lex))],
            vec![],
            vec![],
            CANDIDATE_CAP,
        )
        .with_segmentors(Some(Box::new(rec)), vec![Box::new(matcher), Box::new(abc)]);

        let mut state = SessionState::default();
        for c in "uUni".chars() {
            p.process_key(&mut state, &Key::ch(c));
        }
        let mut out = vec![];
        p.compose(&mut state, &mut out);

        let segs = &state.composition.segments;
        assert_eq!(
            segs.segments.len(),
            1,
            "整段被认领，兜底切分器不该再切"
        );
        assert_eq!(segs.segments[0].tags, vec!["radical_lookup"]);
    }
}
