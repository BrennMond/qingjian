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

use std::sync::Arc;
use stele_core::{
    Candidate, CandidateSink, Filter, Lane, Pipeline, ProcessResult, Processor, QueryView, Ranker,
    Segment, SegmentStatus, Segmentation, SessionState, Span, Tag, Translator,
};

/// 一次组装出来的候选总量上限。
pub const CANDIDATE_CAP: usize = 200;

/// 组件流水线的具体实现。
pub struct PipelineImpl {
    /// 本方案的分段标签。切分器给分段打它，翻译器据此绑定（G3）。
    tag: Tag,
    processors: Vec<Box<dyn Processor>>,
    translators: Vec<Box<dyn Translator>>,
    filters: Vec<Box<dyn Filter>>,
    rankers: Vec<Arc<dyn Ranker>>,
    cap: usize,
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
            processors,
            translators,
            filters,
            rankers,
            cap,
        }
    }

    /// 本方案的标签。
    #[must_use]
    pub fn tag(&self) -> Tag {
        self.tag
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
}

/// 对 `#[non_exhaustive]` 枚举的未知变体的保守处理：继续问后面的处理器。
///
/// 它被抽成函数，是为了让"未知变体走这条分支"这件事**只有一个定义点**——
/// 将来真的加了变体，改这里一处即可。
fn continue_checking() {}

impl Pipeline for PipelineImpl {
    fn process_key(&mut self, state: &mut SessionState, key: &stele_core::Key) -> ProcessResult {
        for p in &mut self.processors {
            if !p.enabled(&state.options) {
                continue;
            }
            match p.process(state, key) {
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

    fn compose(&mut self, state: &mut SessionState, out: &mut Vec<Candidate>) {
        out.clear();

        if state.composition.input.is_empty() {
            state.composition.preedit.clear();
            state.composition.segments.clear();
            return;
        }

        let span = Span::new(0, state.composition.input.len());
        let tags: Vec<Tag> = vec![self.tag];

        // ── 只读阶段：查询 → 翻译 → 过滤 → 重排 ──
        {
            let q = state.query();

            // 翻译器：tag 绑定（G3）——不靠位置区分，靠标签。
            {
                let mut sink = CandidateSink::new(out, self.cap);
                for t in &self.translators {
                    if t.accepts(&tags) {
                        t.translate(&q, span, &mut sink);
                    }
                }
            }

            // 滤镜：顺序即语义。
            for f in &self.filters {
                if f.applies_to(&tags) {
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
                    input: q.input,
                    context: q.context,
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

        // ── 写回显示信息 ──
        //
        // 预编辑串暂时就是输入串本身。带音节分隔的显示（`ni'hao`）属于
        // `Formatter` 的职责，是 P2 的内容。
        state.composition.preedit = state.composition.input.clone();

        let mut seg = Segment::new(span);
        seg.status = SegmentStatus::Guess;
        seg.tags.push(self.tag);
        seg.candidates.extend_from_slice(out);
        let mut segs = Segmentation::default();
        segs.segments.push(seg);
        state.composition.segments = segs;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::Uniquifier;
    use crate::lexicon::InMemoryLexicon;
    use crate::processor::{Editor, Selector, Speller};
    use crate::spelling::{Rule, SpellingTable};
    use crate::translator::{EchoTranslator, SpellingGraphTranslator};
    use stele_core::{CodeAlphabet, Key, ProcessResult};

    fn build() -> PipelineImpl {
        let alphabet = CodeAlphabet::new(vec!["ni".into(), "hao".into()]);
        let table = Arc::new(SpellingTable::compile(
            alphabet.clone(),
            &[Rule::Abbrev {
                take: 1,
                cost: stele_core::Score::from_weight(0.5),
            }],
        ));
        let lex = Arc::new(
            InMemoryLexicon::from_entries(alphabet, &[(vec!["ni", "hao"], "你好", 100.0)]).unwrap(),
        );

        PipelineImpl::new(
            "abc",
            vec![
                Box::new(Speller::default()),
                Box::new(Editor),
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
}
