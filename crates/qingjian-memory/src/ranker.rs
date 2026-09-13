//! # Ranker — 把记忆加成加到候选上
//!
//! 中文职责：`Ranker` 的服务提供者；读记忆、给候选加分，并且**不许违反
//! "精确优先"铁律**。
//! English role: the `Ranker` provider that reads memory and adds bonuses while
//! never violating the "precision first" invariant.
//! 架构位置：`qingjian-memory` 的消费者一侧；`qingjian-engine` 在装配流水线时
//! 把它按 `Arc<dyn Ranker>` 注入（服务在构造时注入，不穿过 `Query`）。
//!
//! # 它为什么可以重排，却仍然安全
//!
//! `Ranker` 只允许**加分**，且要声明一个上界（[`Ranker::bonus_limit`]）。
//! 但设计文档已经把话说透了（`docs/engine-design.md` §5.3.2）：
//! **上界只限制移动幅度，不消除倒置**——若某精确匹配是 900、某猜测候选是
//! 100、上界是 2000，那个猜测候选可以合法地升到 2100，照样压过精确匹配。
//!
//! 因此这里**自己再守一道**：猜测候选的加成会被钳到"它前面那个精确匹配
//! 的分"之下（见 [`MemoryRanker::allowed_bonus`]）。铁律于是不只是文档
//! 里的一句话，而是这个重排器**算不出来**的东西。

use std::collections::BTreeMap;
use std::sync::Arc;

use qingjian_core::{
    clamp_bonus, is_exact, Candidate, Lane, MemoryStore, QueryView, Ranker, Score,
};

use crate::decay::MAX_BONUS_ML;
use crate::store::normalize_key;

/// 按用户记忆给候选加分的重排器。
pub struct MemoryRanker {
    /// 记忆服务。`MemoryStore` 的方法是 `&self`，因此多个会话共享它。
    store: Arc<dyn MemoryStore>,
    /// 本重排器单次能加的最大分数。
    limit: Score,
}

impl MemoryRanker {
    /// 用一个记忆实现构造。
    #[must_use]
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self {
            store,
            // 上界就是衰减曲线的饱和值——它必须高于真实词库的最高分，
            // 否则"打过的词下次优先"在算术上不可能成立（见 `decay::MAX_BONUS_ML`）。
            limit: Score::from_milli_log(MAX_BONUS_ML),
        }
    }

    /// 这个候选实际能拿到多少加成。
    ///
    /// # "精确优先"在这里被执行
    ///
    /// - **不是猜的**候选：拿全额（受上界约束）。
    /// - **猜的**候选：不得超过"重排前排在它前面的**最小**精确匹配分 - 1"。
    ///   减 1 是为了不与它**并列**——并列之后由 `Origin` 与插入顺序裁决，
    ///   而插入顺序是翻译器的产出顺序，用它决定"精确优先"就成了碰运气。
    ///
    /// 为什么取"比它大的**最小**精确分"而不是全局最高分：全局最高分会把
    /// 猜测候选压得比必要的更狠。只要不越过它前面最近的那个精确匹配，
    /// 铁律就没有被违反——而它仍然可以在猜测候选内部正常排序。
    #[must_use]
    fn allowed_bonus(cand: &Candidate, wanted_ml: i32, exact_scores: &[Score]) -> i32 {
        if is_exact(cand.origin, cand.attr) {
            return wanted_ml;
        }
        let Some(ceiling) = exact_scores.iter().find(|s| **s > cand.score) else {
            // 它本来就排在所有精确匹配前面 —— 那不是这次重排造成的（§5.3.0）。
            return wanted_ml;
        };
        // 目标：重排后仍然严格低于 `ceiling`。
        let room = ceiling
            .as_milli_log()
            .saturating_sub(1)
            .saturating_sub(cand.score.as_milli_log());
        wanted_ml.min(room.max(0))
    }

    /// 记忆里有没有关于这把键的记录（供调试前端与测试用）。
    ///
    /// 注意它按**键**查，不按拼写——键的形态见 `store::normalize_key`
    /// 与 `Candidate::key` 的文档。
    #[must_use]
    pub fn knows(&self, key: &str) -> bool {
        !self.store.lookup(key).is_empty()
    }
}

impl Ranker for MemoryRanker {
    fn rerank(&self, q: &QueryView<'_>, cands: &mut Vec<Candidate>) {
        // 只作用于 `Lane::Input`：预测候选走另一条通道、有另一套排序规则
        // （D23 / `docs/engine-design.md` §4.3）。
        if q.lane != Lane::Input || q.input.is_empty() || cands.is_empty() {
            return;
        }

        // ── ① 这次查询要查哪些键 ──
        //
        // **候选自带的规范编码键**是第一来源（PLAN D42）：它是翻译器在
        // 产生候选时从编码渲染出来的，因此 `nhao` 与 `nihao` 走到同一条
        // 编码时会得到**同一把键**——跨拼法共享记忆在这里自动成立，
        // 不需要任何反查。去重是必要的：同一条编码下的同音词很多。
        let mut keys: Vec<&str> = Vec::new();
        for c in cands.iter() {
            if let Some(k) = c.key_str() {
                if !keys.contains(&k) {
                    keys.push(k);
                }
            }
        }
        // **拼写键是兜底**，两个理由：
        // ① 没有编码的候选（原样上屏、标点、造句）只能按拼写被找到；
        // ② 早期版本按拼写存过记录，留着它不至于让那些记录变成孤儿。
        // 它与 `FileMemory::record` 的兜底路径用的是同一个 `normalize_key`，
        // 因此"存进去的"与"查出来的"不会错位。
        let spelling = normalize_key(q.input);
        if !spelling.is_empty() && !keys.contains(&spelling.as_str()) {
            keys.push(spelling.as_str());
        }

        // ── ② 查表并按文本合并 ──
        //
        // 同一个词可能同时出现在几把键下（被不同拼法学到过）。
        // 取**最大值**：加成的语义是"用户有多想要它"，取最大才不浪费。
        let mut best: BTreeMap<String, Score> = BTreeMap::new();
        for k in &keys {
            for e in self.store.lookup(k) {
                best.entry(e.text)
                    .and_modify(|s| {
                        if e.bonus > *s {
                            *s = e.bonus;
                        }
                    })
                    .or_insert(e.bonus);
            }
        }
        if best.is_empty() {
            return;
        }

        // 先把"不是猜的"候选的分数排好（升序）。**在加分之前取快照**：
        // 快照偏保守（精确候选自己也可能被加分而往上走），因此不会漏判。
        let mut exact_scores: Vec<Score> = cands
            .iter()
            .filter(|c| is_exact(c.origin, c.attr))
            .map(|c| c.score)
            .collect();
        exact_scores.sort_unstable();

        // ── ③ 加分 ──
        let limit_ml = self.limit.as_milli_log();
        for cand in cands.iter_mut() {
            let Some(bonus) = best.get(&cand.text) else {
                continue;
            };
            let wanted_ml = bonus.as_milli_log().clamp(0, limit_ml);
            let allowed_ml = Self::allowed_bonus(cand, wanted_ml, &exact_scores);
            if allowed_ml > 0 {
                // 用内核的 `clamp_bonus` 落笔：上界这件事**只有一处实现**。
                cand.score = clamp_bonus(cand.score, Score::from_milli_log(allowed_ml), self.limit);
            }
        }
    }

    fn bonus_limit(&self) -> Score {
        self.limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::FileMemory;
    use qingjian_core::{
        Clock, Commit, Context, FrozenClock, MemoryEntry, Origin, Span, SpellingAttr, Trigger,
    };

    const T0: u64 = 1_767_225_600;

    fn clock() -> Arc<dyn Clock> {
        Arc::new(FrozenClock {
            secs: T0,
            ms: 0,
            offset_secs: 0,
        })
    }

    /// 一个**没有编码键**的候选（原样上屏、标点、造句都是这样）。
    fn cand(text: &str, ml: i32, origin: Origin, attr: SpellingAttr) -> Candidate {
        Candidate {
            text: text.into(),
            comment: None,
            score: Score::from_milli_log(ml),
            origin,
            attr,
            span: Span::new(0, 3),
            lane: Lane::Input,
            kind: qingjian_core::CandidateKind::Normal,
            key: None,
        }
    }

    /// 一个**带规范编码键**的候选（翻译器产出的真实形态）。
    fn cand_at(key: &str, text: &str, ml: i32, origin: Origin, attr: SpellingAttr) -> Candidate {
        Candidate {
            key: Some(key.into()),
            ..cand(text, ml, origin, attr)
        }
    }

    fn view(input: &str) -> QueryView<'_> {
        static EMPTY: std::sync::OnceLock<Context> = std::sync::OnceLock::new();
        let ctx = EMPTY.get_or_init(Context::default);
        QueryView {
            input,
            context: ctx,
            lane: Lane::Input,
        }
    }

    /// 按**规范编码键**学一个词（`times` 次）——引擎的真实形态。
    fn learned_at(key: &str, text: &str, times: u32) -> Arc<dyn MemoryStore> {
        let m = Arc::new(FileMemory::in_memory(clock(), 100));
        for _ in 0..times {
            m.record(&Commit {
                text: text.into(),
                input: key.into(),
                context: vec![],
                origin: Origin::SystemWord,
                attr: SpellingAttr::NORMAL,
                lane: Lane::Input,
                trigger: Trigger::Space,
                key: Some(key.into()),
            });
        }
        m
    }

    /// 只按**拼写**学一个词——覆盖"拿不到编码"的兜底路径。
    fn learned_by_spelling(spelling: &str, text: &str, times: u32) -> Arc<dyn MemoryStore> {
        let m = Arc::new(FileMemory::in_memory(clock(), 100));
        for _ in 0..times {
            m.record(&Commit {
                text: text.into(),
                input: spelling.into(),
                context: vec![],
                origin: Origin::SystemWord,
                attr: SpellingAttr::NORMAL,
                lane: Lane::Input,
                trigger: Trigger::Space,
                key: None,
            });
        }
        m
    }

    // ─────────────────────────────────────────────────────────────────────
    // D42 的核心：**一条编码一把键**，于是跨拼法共享是自动的
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn a_word_learned_via_abbreviation_is_found_via_full_pinyin() {
        // 用户用简拼 `nhao` 选中「你好」→ 引擎给的键是**编码** `ni'hao`。
        let store = learned_at("ni'hao", "你好", 3);
        let r = MemoryRanker::new(store);

        // 后来他改敲全拼 `nihao`：候选的键**也是** `ni'hao`（同一条编码），
        // 于是那份加成照样命中。
        let mut cands = vec![
            cand_at(
                "ni'hao",
                "你好",
                9_210,
                Origin::SystemWord,
                SpellingAttr::NORMAL,
            ),
            cand_at(
                "ni'hao",
                "你号",
                9_500,
                Origin::SystemWord,
                SpellingAttr::NORMAL,
            ),
        ];
        r.rerank(&view("nihao"), &mut cands);
        assert!(
            cands[0].score > cands[1].score,
            "简拼学到的词必须在全拼下也优先（这就是 D42 要的东西）"
        );
    }

    #[test]
    fn a_word_learned_via_full_pinyin_is_found_via_abbreviation() {
        // 反过来也要成立：全拼学的，简拼也吃得到。
        let store = learned_at("ni'hao", "你好", 3);
        let r = MemoryRanker::new(store);
        let mut cands = vec![
            // 简拼候选的键**仍然是** `ni'hao`——因为它是同一条编码。
            cand_at(
                "ni'hao",
                "你好",
                9_210,
                Origin::SystemWord,
                SpellingAttr::ABBREV,
            ),
            cand_at(
                "na'hao",
                "那好",
                9_500,
                Origin::SystemWord,
                SpellingAttr::ABBREV,
            ),
        ];
        r.rerank(&view("nhao"), &mut cands);
        assert!(
            cands[0].score > cands[1].score,
            "全拼学到的词在简拼下也要优先"
        );
    }

    #[test]
    fn different_codes_do_not_share_memory() {
        // 反面：**不同编码**不该互相加分。`na'hao` 学的不能帮到 `ni'hao`。
        let store = learned_at("na'hao", "那好", 5);
        let r = MemoryRanker::new(store);
        let mut cands = vec![cand_at(
            "ni'hao",
            "你好",
            9_210,
            Origin::SystemWord,
            SpellingAttr::NORMAL,
        )];
        let before = cands[0].score;
        r.rerank(&view("nihao"), &mut cands);
        assert_eq!(cands[0].score, before, "别的编码的记忆不该越界加分");
    }

    #[test]
    fn spelling_keyed_records_still_work_as_a_fallback() {
        // 拿不到编码的上屏（造句、标点、原样上屏）按拼写存，
        // 查询时那条兜底路径必须还能把它们找回来。
        let store = learned_by_spelling("nihao", "你好世界", 5);
        let r = MemoryRanker::new(store);
        let mut cands = vec![cand(
            "你好世界",
            100,
            Origin::Sentence,
            SpellingAttr::NORMAL,
        )];
        let before = cands[0].score;
        r.rerank(&view("nihao"), &mut cands);
        assert!(cands[0].score > before, "拼写键的兜底路径断了");
    }

    // ─────────────────────────────────────────────────────────────────────
    // 排序与铁律
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn a_learned_word_overtakes_a_higher_scored_rival() {
        let store = learned_at("ni", "貎", 3);
        let r = MemoryRanker::new(store);
        let mut cands = vec![
            cand_at("ni", "你", 12_366, Origin::SystemWord, SpellingAttr::NORMAL),
            cand_at("ni", "貎", 8_517, Origin::SystemWord, SpellingAttr::NORMAL),
        ];
        r.rerank(&view("ni"), &mut cands);
        // 打过 3 次的「貎」拿到 8400 分加成 → 16917 > 12366。
        assert!(cands[1].score > cands[0].score, "学过的词应当反超");
    }

    #[test]
    fn one_commit_is_a_nudge_not_a_takeover() {
        let store = learned_at("ni", "貎", 1);
        let r = MemoryRanker::new(store);
        let mut cands = vec![
            cand_at("ni", "你", 12_366, Origin::SystemWord, SpellingAttr::NORMAL),
            cand_at("ni", "貎", 8_517, Origin::SystemWord, SpellingAttr::NORMAL),
        ];
        r.rerank(&view("ni"), &mut cands);
        assert_eq!(cands[1].score.as_milli_log(), 8_517 + 4_666);
    }

    #[test]
    fn memory_never_lifts_a_guess_above_an_exact_match() {
        // 铁律的**反面测试**：一个学过的、但拼写是"猜的"的候选，
        // 即使加成足够大，也不许越过精确匹配。
        let store = learned_at("ni'hao'shi'jie", "你好世界", 20);
        let r = MemoryRanker::new(store);
        let mut cands = vec![
            cand_at(
                "ni'hao",
                "你好",
                9_210,
                Origin::SystemWord,
                SpellingAttr::NORMAL,
            ),
            cand_at(
                "ni'hao'shi'jie",
                "你好世界",
                100,
                Origin::SystemWord,
                SpellingAttr::COMPLETION,
            ),
        ];
        r.rerank(&view("nihao"), &mut cands);
        assert_eq!(
            cands[1].score.as_milli_log(),
            9_209,
            "猜测候选必须严格低于它前面的精确匹配"
        );
        assert!(cands[1].score < cands[0].score, "铁律被违反了");
    }

    #[test]
    fn a_guess_that_was_already_ahead_is_left_alone() {
        // §5.3.0：猜测候选**本来就**排在前面时，那不关重排器的事。
        let store = learned_at("ni'hao", "句子", 20);
        let r = MemoryRanker::new(store);
        let mut cands = vec![
            cand_at(
                "ni'hao",
                "句子",
                9_500,
                Origin::Sentence,
                SpellingAttr::NORMAL,
            ),
            cand_at(
                "ni'hao",
                "你好",
                9_210,
                Origin::SystemWord,
                SpellingAttr::NORMAL,
            ),
        ];
        let before = cands[0].score;
        r.rerank(&view("nihao"), &mut cands);
        assert!(cands[0].score > before);
    }

    #[test]
    fn derived_candidates_are_boosted_when_no_exact_match_exists() {
        // 敲 `nhao`（简拼）时没有精确匹配，于是没有天花板。
        let store = learned_at("ni'hao", "你好", 3);
        let r = MemoryRanker::new(store);
        let mut cands = vec![
            cand_at(
                "ni'hao",
                "你好",
                9_210,
                Origin::SystemWord,
                SpellingAttr::ABBREV,
            ),
            cand_at(
                "na'hao",
                "那好",
                8_000,
                Origin::SystemWord,
                SpellingAttr::ABBREV,
            ),
        ];
        r.rerank(&view("nhao"), &mut cands);
        assert!(cands[0].score > Score::from_milli_log(9_210));
    }

    #[test]
    fn the_bonus_respects_the_declared_limit() {
        let r = MemoryRanker::new(Arc::new(FakeMemory));
        let mut cands = vec![cand_at(
            "x",
            "词",
            0,
            Origin::SystemWord,
            SpellingAttr::NORMAL,
        )];
        r.rerank(&view("x"), &mut cands);
        assert_eq!(
            cands[0].score.as_milli_log(),
            MAX_BONUS_ML,
            "再大的加成也必须被上界钳住"
        );
        assert_eq!(r.bonus_limit(), Score::from_milli_log(MAX_BONUS_ML));
    }

    #[test]
    fn unknown_input_changes_nothing() {
        let store = learned_at("ni'hao", "你好", 5);
        let r = MemoryRanker::new(store);
        let mut cands = vec![cand_at(
            "ni",
            "你",
            12_366,
            Origin::SystemWord,
            SpellingAttr::NORMAL,
        )];
        let before = cands[0].score;
        r.rerank(&view("ni"), &mut cands);
        assert_eq!(cands[0].score, before);
    }

    #[test]
    fn predict_lane_is_untouched() {
        let store = learned_at("ni'hao", "你好", 5);
        let r = MemoryRanker::new(store);
        let mut cands = vec![cand_at(
            "ni'hao",
            "你好",
            9_210,
            Origin::SystemWord,
            SpellingAttr::NORMAL,
        )];
        let before = cands[0].score;
        let ctx = Context::default();
        r.rerank(
            &QueryView {
                input: "nihao",
                context: &ctx,
                lane: Lane::Predict,
            },
            &mut cands,
        );
        assert_eq!(cands[0].score, before, "预测通道不受记忆重排影响");
    }

    /// 一个永远给出"超大加成"的记忆，用来单独测上界。
    struct FakeMemory;

    impl MemoryStore for FakeMemory {
        fn record(&self, _commit: &Commit) {}
        fn lookup(&self, _key: &str) -> Vec<MemoryEntry> {
            vec![MemoryEntry {
                input: "x".into(),
                text: "词".into(),
                count: 1,
                bonus: Score::from_milli_log(i32::MAX / 2),
                last_used: T0,
            }]
        }
        fn forget(&self, _key: &str, _text: &str) {}
        fn predict_next(&self, _context: &Context) -> Vec<qingjian_core::Prediction> {
            Vec::new()
        }
    }
}
