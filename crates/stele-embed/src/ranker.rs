//! # Ranker — 把向量分数接到候选排序上
//!
//! 中文职责：`Ranker` 的服务提供者；用 [`VectorMemory`] 给候选加一个**有界**的
//! 向量偏好分，且**不许违反"精确优先"铁律**。
//! English role: the `Ranker` provider that adds a bounded vector-preference bonus.
//! 架构位置：与 `stele_memory::MemoryRanker` 同一个位置（装配时注入的
//! `Services::rankers`），只是它的信号来自向量而不是精确查表。
//!
//! # 三条刻意的保守选择（都能被测试证伪）
//!
//! 1. **只给"不是猜的"候选加分**。于是"重排器把猜测候选顶到精确匹配之前"
//!    在算术上不可能发生——铁律不是靠检查守住，而是靠**不做那件事**。
//! 2. **按名次加分，不按分值**。分值的绝对量纲取决于词频与维数，
//!    按分值缩放需要一个"信号够不够强"的阈值，而那个阈值没有数据可依据。
//!    名次法没有可调常数：向量最看好的那条拿满额，第二、第三条递减。
//!    **代价是强度信息被丢掉**——这是第一版的取舍，收益要靠对比集量。
//! 3. **只在有上下文时工作**。上下文为空（刚启动、或还没上屏过）时
//!    向量无从谈起，直接不加分。

use std::sync::Arc;

use stele_core::{clamp_bonus, is_exact, Candidate, Lane, QueryView, Ranker, Score};

use crate::model::VectorMemory;

/// 按向量偏好给候选加分的重排器。
pub struct EmbedRanker {
    /// 学出来的向量表。
    model: Arc<VectorMemory>,
    /// 单次能加的最大分数（毫对数）。
    limit: Score,
    /// 最多给前几名加分（名次越靠后，加分越少）。
    max_boosted: usize,
}

impl EmbedRanker {
    /// 默认上限：**刻意很小**（4000 毫对数 ≈ 权重 55 倍）。
    ///
    /// 为什么小：向量记忆是"弱信号"，它的价值在于**打破旗鼓相当的僵局**，
    /// 而不是压过词库权重与 P4a 的精确历史。给大了会让"向量猜错"的代价
    /// 直接变成"用户想选的那个被挤下去"。
    ///
    /// **它是 D46 要求写进文档与称重台的那个上限的一部分。**
    pub const DEFAULT_LIMIT_ML: i32 = 4_000;

    /// 默认最多加分的名次数量。
    pub const DEFAULT_MAX_BOOSTED: usize = 3;

    /// 由一个向量表构造。
    #[must_use]
    pub fn new(model: Arc<VectorMemory>) -> Self {
        Self {
            model,
            limit: Score::from_milli_log(Self::DEFAULT_LIMIT_ML),
            max_boosted: Self::DEFAULT_MAX_BOOSTED,
        }
    }

    /// 换一个加成上限（链式）。
    #[must_use]
    pub fn with_limit(mut self, limit: Score) -> Self {
        self.limit = limit;
        self
    }

    /// 换"最多给前几名加分"（链式）。
    #[must_use]
    pub fn with_max_boosted(mut self, n: usize) -> Self {
        self.max_boosted = n;
        self
    }

    /// 底层的向量表（诊断与测试用）。
    #[must_use]
    pub fn model(&self) -> &VectorMemory {
        &self.model
    }
}

impl Ranker for EmbedRanker {
    fn rerank(&self, q: &QueryView<'_>, cands: &mut Vec<Candidate>) {
        // 只作用于 `Lane::Input`：预测候选由 P4b 的通道单独排序
        // （`docs/engine-design.md` §4.3）。
        if q.lane != Lane::Input || cands.is_empty() || q.context.recent().is_empty() {
            return;
        }

        // 上下文向量每键只算一次（缓冲区复用是在 `context_vector` 内部
        // 由调用方持有；这里每键一次分配一个 `Vec<i64>`，与 P4b 的
        // 每次按键分配几个 `String` 同量级）。
        let mut context_vector = Vec::new();
        self.model
            .context_vector(q.context.recent(), &mut context_vector);
        if context_vector.iter().all(|x| *x == 0) {
            // 上下文一个词都不在词表里：向量无从谈起。
            return;
        }

        // 只给**不是猜的**候选打分——选择 1（见模块文档）。
        let mut scored: Vec<(usize, i64)> = cands
            .iter()
            .enumerate()
            .filter(|(_, c)| is_exact(c.origin, c.attr))
            .map(|(i, c)| (i, self.model.score_with(&context_vector, &c.text)))
            .filter(|(_, score)| *score > 0)
            .collect();
        if scored.is_empty() {
            // 没有任何"正相关"的候选：与其按噪声加分，不如不加。
            return;
        }

        // 分数降序；`sort_by_key` 是稳定排序，因此同分时保留插入顺序
        // ——与内核的平局规则一致（PLAN §5.4）。
        scored.sort_by_key(|x| core::cmp::Reverse(x.1));

        let limit_ml = self.limit.as_milli_log();
        for (rank, (idx, _)) in scored.iter().take(self.max_boosted).enumerate() {
            // `rank` 是 usize，最多 3：移位数不会越界（这里显式钳一下，
            // 免得将来有人把 `max_boosted` 配到 64 以上时静默回绕）。
            let shift = u32::try_from(rank).unwrap_or(u32::MAX).min(31);
            let ml = limit_ml >> shift;
            if ml <= 0 {
                break;
            }
            let bonus = Score::from_milli_log(ml);
            let cand = &mut cands[*idx];
            // 用内核的 `clamp_bonus` 落笔：**上界这件事只有一处实现**。
            cand.score = clamp_bonus(cand.score, bonus, self.limit);
        }
    }

    fn bonus_limit(&self) -> Score {
        self.limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stele_core::{Candidate, CandidateKind, Context, Origin, QueryView, Span, SpellingAttr};

    fn model() -> Arc<VectorMemory> {
        // 「今天 → 天气」学过很多次，「今天 → 心情」只学过一次。
        let samples = vec![
            (vec!["今天".to_string()], "天气".to_string(), 9),
            (vec!["今天".to_string()], "心情".to_string(), 1),
        ];
        Arc::new(VectorMemory::train(samples, crate::model::VectorConfig::default()).unwrap())
    }

    fn cand(text: &str, ml: i32, origin: Origin, attr: SpellingAttr) -> Candidate {
        Candidate {
            text: text.to_owned(),
            comment: None,
            score: Score::from_milli_log(ml),
            origin,
            attr,
            span: Span::new(0, 2),
            lane: Lane::Input,
            kind: CandidateKind::Normal,
            key: None,
        }
    }

    fn view(context: &Context) -> QueryView<'_> {
        QueryView {
            input: "tianqi",
            context,
            lane: Lane::Input,
        }
    }

    fn ctx_with(words: &[&str]) -> Context {
        let mut c = Context::with_capacity(8);
        for w in words {
            c.push(*w);
        }
        c
    }

    #[test]
    fn the_vector_favoured_candidate_gets_a_bonus() {
        let r = EmbedRanker::new(model());
        let context = ctx_with(&["今天"]);
        // 天气 与 心情 基础分相同（旗鼓相当），向量应当把 天气 抬起来。
        let mut cands = vec![
            cand("心情", 100, Origin::SystemWord, SpellingAttr::NORMAL),
            cand("天气", 100, Origin::SystemWord, SpellingAttr::NORMAL),
        ];
        r.rerank(&view(&context), &mut cands);
        let tianqi = cands.iter().find(|c| c.text == "天气").unwrap();
        let xinqing = cands.iter().find(|c| c.text == "心情").unwrap();
        assert!(
            tianqi.score > xinqing.score,
            "向量最看好的那条应当被抬起来：{} vs {}",
            tianqi.score.as_milli_log(),
            xinqing.score.as_milli_log()
        );
    }

    #[test]
    fn guessed_candidates_are_never_boosted() {
        // 铁律的**结构保证**：我们不给猜测候选加分，于是"把猜测顶到精确之前"
        // 在算术上不可能发生——不需要事后检查。
        let r = EmbedRanker::new(model());
        let context = ctx_with(&["今天"]);
        let mut cands = vec![
            cand("天气", 100, Origin::Sentence, SpellingAttr::NORMAL),
            cand("心情", 100, Origin::SystemWord, SpellingAttr::NORMAL),
        ];
        let before = cands[0].score;
        r.rerank(&view(&context), &mut cands);
        assert_eq!(cands[0].score, before, "猜测候选的分数不该被向量改动");
    }

    #[test]
    fn an_empty_context_changes_nothing() {
        let r = EmbedRanker::new(model());
        let mut cands = vec![cand("天气", 100, Origin::SystemWord, SpellingAttr::NORMAL)];
        let before = cands[0].score;
        r.rerank(&view(&Context::default()), &mut cands);
        assert_eq!(cands[0].score, before);
    }

    #[test]
    fn predict_lane_is_untouched() {
        let r = EmbedRanker::new(model());
        let context = ctx_with(&["今天"]);
        let mut cands = vec![cand("天气", 100, Origin::SystemWord, SpellingAttr::NORMAL)];
        let before = cands[0].score;
        let q = QueryView {
            input: "",
            context: &context,
            lane: Lane::Predict,
        };
        r.rerank(&q, &mut cands);
        assert_eq!(cands[0].score, before);
    }

    #[test]
    fn the_bonus_never_exceeds_the_declared_limit() {
        let r = EmbedRanker::new(model()).with_limit(Score::from_milli_log(500));
        let context = ctx_with(&["今天"]);
        let mut cands = vec![cand("天气", 0, Origin::SystemWord, SpellingAttr::NORMAL)];
        r.rerank(&view(&context), &mut cands);
        assert!(cands[0].score.as_milli_log() <= 500);
        assert_eq!(r.bonus_limit(), Score::from_milli_log(500));
    }
}
