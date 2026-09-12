//! # Sorting and guards
//!
//! 中文职责：候选排序的**唯一实现**，以及"精确优先"铁律的守卫。
//! English role: the single implementation of candidate ordering, plus the guard
//! enforcing the "precision first" invariant.
//! 架构位置：stele-core 的排序与不变式，被引擎管线在最后一步调用。
//!
//! # 为什么排序必须是全序（PLAN §5.2 可复现）
//!
//! 词库里同权重的词极多（rime-ice 里大量词条权重都是 `1`）。若用 `HashMap`
//! 的遍历顺序决定先后，**同一个输入每次运行的结果可能不同**——直接违反铁律。
//!
//! 排序规则（必须显式实现，不得依赖容器遍历顺序）：
//!
//! **`Lane::Input` 内**：① `score` 降序 → ② [`Origin`] 优先级（平局时）→
//! ③ 插入顺序（靠 `sort_by` 的**稳定性**保证）。
//!
//! **`Lane::Predict` 内**：① `score` 降序 → ② 插入顺序。
//! **不受 `Origin` 优先级与"精确优先"约束**（它们在这里没有意义）。
//!
//! # 关于插入顺序本身的不确定性
//!
//! "按插入顺序"这条平局规则，要求**插入顺序本身是确定的**。这常被忽略：
//! 若某个翻译器遍历 `HashMap` 来产出候选，不确定性的源头只是从排序阶段
//! **前移**到了产出阶段，铁律照样被破坏。
//! **因此：组件内部凡需遍历映射，必须用有序容器。**

use crate::candidate::{is_exact, Candidate, Lane, Origin};
use crate::score::Score;
use core::cmp::Ordering;

/// 通道的排序位次：`Lane::Input` 在前。
///
/// 用函数而非 `as u8`，以免枚举增加变体时静默改变位次。
#[must_use]
fn lane_rank(lane: Lane) -> u8 {
    match lane {
        Lane::Input => 0,
        Lane::Predict => 1,
    }
}

/// 对候选列表排序：**通道间** Input 在前，**通道内**按各自规则。
///
/// 这是唯一的排序入口——不要在各处各写一遍 `sort_by`。
pub fn sort_candidates(cands: &mut [Candidate]) {
    // `sort_by` 是**稳定排序**，因此"插入顺序"自动成为最后的平局键。
    // 切勿改成 `sort_unstable_by`。
    cands.sort_by(compare);
}

/// 排序比较函数（暴露出来便于测试与复用）。
#[must_use]
pub fn compare(a: &Candidate, b: &Candidate) -> Ordering {
    lane_rank(a.lane)
        .cmp(&lane_rank(b.lane))
        .then_with(|| b.score.cmp(&a.score))
        .then_with(|| match (a.lane, b.lane) {
            // `Lane::Input`：平局时看来源优先级。
            (Lane::Input, Lane::Input) => a.origin.cmp(&b.origin),
            // `Lane::Predict`：不看来源优先级。
            _ => Ordering::Equal,
        })
}

/// 只取某个通道的候选数量。
#[must_use]
pub fn count_lane(cands: &[Candidate], lane: Lane) -> usize {
    cands.iter().filter(|c| c.lane == lane).count()
}

/// 检测**重排器造成的跨类倒置**（铁律"精确优先"的精确判据）。
///
/// # 这条铁律真正在说什么
///
/// v1 的原话是「AI 只做重排……**绝不压过精确匹配**」。**主语是重排器。**
/// 它约束的是重排器**造成的改变**，不是基础排序的天然结果。
///
/// 因此这里校验的是"**重排前后**跨类的相对次序有没有被翻转"，
/// **而不是**"最终列表里两类必须分区"。
///
/// **为什么不能要求分区**：如果一个造句候选的分数**本来就高于**一个冷门
/// 精确匹配词（真实词库里很常见——权重 `1` 的生僻字 vs 高概率的常用组合），
/// 那么按分数它就该排在前面。要求分区等于要求"生僻字永远压过高概率组句"，
/// **会明显损害输入质量**，而且对 RIME 的真实行为也不准确。
///
/// # 复杂度
///
/// `O(n²)`（需要对每一对比较先后）。因此本函数**只用于 debug 构建与测试**；
/// 发布构建里靠"重排器加成有整数上界"做**结构保证**——那才是更快、
/// 更强的机制（见 [`crate::service::Ranker::bonus_limit`]）。
///
/// 返回 `true` 表示**发生了倒置**（即铁律被违反）。
#[must_use]
pub fn has_cross_class_inversion(before: &[Candidate], after: &[Candidate], lane: Lane) -> bool {
    let before_lane: Vec<&Candidate> = before.iter().filter(|c| c.lane == lane).collect();
    let after_lane: Vec<&Candidate> = after.iter().filter(|c| c.lane == lane).collect();

    // 不是同一批候选就不判（例如过滤阶段丢弃了一些）。
    if before_lane.len() != after_lane.len() {
        return false;
    }

    let pos_in_before = |text: &str| before_lane.iter().position(|c| c.text == text);

    for a in &after_lane {
        if is_exact(a.origin, a.attr) {
            continue;
        }
        let Some(p_b) = pos_in_before(&a.text) else {
            continue;
        };
        for b in &after_lane {
            if !is_exact(b.origin, b.attr) {
                continue;
            }
            let Some(p_a) = pos_in_before(&b.text) else {
                continue;
            };
            // 重排前 b 在 a 前面（p_a < p_b）；重排后若 a 跑到 b 前面 —— 倒置。
            if p_a < p_b {
                let ia = after_lane.iter().position(|c| c.text == a.text);
                let ib = after_lane.iter().position(|c| c.text == b.text);
                if let (Some(ia), Some(ib)) = (ia, ib) {
                    if ia < ib {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// 把重排器的加成钳到声明的上界内——**发布构建里的守卫**。
///
/// 这是"结构保证优于运行期修正"的落地：只要每个重排器都通过它加分数，
/// 跨类倒置在算术上就不可能发生，无需事后检查。
#[must_use]
pub fn clamp_bonus(base: Score, bonus: Score, limit: Score) -> Score {
    let limited = if bonus > limit { limit } else { bonus };
    base.saturating_add(limited)
}

/// 生成一个稳定的平局键：来源优先级。
#[must_use]
pub fn origin_rank(origin: Origin) -> u8 {
    match origin {
        Origin::UserWord => 0,
        Origin::SystemWord => 1,
        Origin::Literal => 2,
        Origin::Sentence => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::{Span, SpellingAttr};
    use crate::score::Score;

    fn c(text: &str, score_milli: i32, origin: Origin, lane: Lane) -> Candidate {
        Candidate {
            text: text.to_owned(),
            comment: None,
            score: Score::from_milli_log(score_milli),
            origin,
            attr: SpellingAttr::NORMAL,
            span: Span::new(0, 1),
            lane,
        }
    }

    #[test]
    fn score_dominates_origin() {
        // 分数高的猜测候选，本来就该排在分数低的精确匹配前面。
        // （要求分区会损害质量 —— 见 has_cross_class_inversion 的说明。）
        let mut v = vec![
            c("rare", 100, Origin::SystemWord, Lane::Input),
            c("common sentence", 900, Origin::Sentence, Lane::Input),
        ];
        sort_candidates(&mut v);
        assert_eq!(v[0].text, "common sentence");
    }

    #[test]
    fn origin_breaks_ties() {
        let mut v = vec![
            c("sentence", 500, Origin::Sentence, Lane::Input),
            c("user", 500, Origin::UserWord, Lane::Input),
            c("system", 500, Origin::SystemWord, Lane::Input),
        ];
        sort_candidates(&mut v);
        assert_eq!(v[0].text, "user");
        assert_eq!(v[1].text, "system");
        assert_eq!(v[2].text, "sentence");
    }

    #[test]
    fn insertion_order_breaks_remaining_ties() {
        let mut v = vec![
            c("first", 500, Origin::SystemWord, Lane::Input),
            c("second", 500, Origin::SystemWord, Lane::Input),
            c("third", 500, Origin::SystemWord, Lane::Input),
        ];
        sort_candidates(&mut v);
        assert_eq!(
            v.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(),
            ["first", "second", "third"]
        );
    }

    #[test]
    fn input_lane_precedes_predict_lane() {
        let mut v = vec![
            c("prediction", 9999, Origin::Sentence, Lane::Predict),
            c("input", -9999, Origin::SystemWord, Lane::Input),
        ];
        sort_candidates(&mut v);
        assert_eq!(v[0].lane, Lane::Input);
    }

    #[test]
    fn prediction_lane_ignores_origin_priority() {
        let mut v = vec![
            c("a", 500, Origin::UserWord, Lane::Predict),
            c("b", 500, Origin::Sentence, Lane::Predict),
        ];
        sort_candidates(&mut v);
        // 平局时保持插入顺序，而不是按 origin 重排。
        assert_eq!(v[0].text, "a");
        assert_eq!(v[1].text, "b");
    }

    #[test]
    fn sorting_is_reproducible_over_many_runs() {
        // 铁律：同一输入连续跑 100 次，候选序列必须逐字节一致。
        //
        // 这里让**四个候选分数完全相同**，以便单独检验平局规则。
        // 输入顺序特意打乱（b_sys 在 a_user 之前），从而证明平局规则
        // 真的在起作用，而不只是"恰好没动"。
        let build = || {
            vec![
                c("b_sys", 1, Origin::SystemWord, Lane::Input),
                c("a_user", 1, Origin::UserWord, Lane::Input),
                c("c_sys", 1, Origin::SystemWord, Lane::Input),
                c("d_sentence", 1, Origin::Sentence, Lane::Input),
            ]
        };
        let mut first: Option<Vec<String>> = None;
        for _ in 0..100 {
            let mut v = build();
            sort_candidates(&mut v);
            let got: Vec<String> = v.into_iter().map(|x| x.text).collect();
            match &first {
                None => first = Some(got),
                Some(f) => assert_eq!(f, &got, "排序结果不可复现"),
            }
        }
        assert_eq!(
            first.unwrap(),
            ["a_user", "b_sys", "c_sys", "d_sentence"],
            "平局时应当：origin 优先，再按插入顺序（b_sys 在 c_sys 之前）"
        );
    }

    #[test]
    fn higher_score_beats_higher_origin_priority() {
        // 分数是主键，来源只是平局键 —— 这条边界常被搞混。
        let mut v = vec![
            c("user", 1, Origin::UserWord, Lane::Input),
            c("sentence", 999, Origin::Sentence, Lane::Input),
        ];
        sort_candidates(&mut v);
        assert_eq!(v[0].text, "sentence");
    }

    #[test]
    fn detects_inversion_caused_by_reranking() {
        let before = vec![
            c("exact", 100, Origin::SystemWord, Lane::Input),
            c("guessed", 200, Origin::Sentence, Lane::Input),
        ];
        // 重排前 exact 在 guessed 之后（因为分数低）——这不算倒置。
        // 构造一个"重排前 exact 在前"的情形：
        let before2 = vec![
            c("exact", 900, Origin::SystemWord, Lane::Input),
            c("guessed", 100, Origin::Sentence, Lane::Input),
        ];
        assert!(!has_cross_class_inversion(&before2, &before2, Lane::Input));

        // 重排后 guessed 被顶到 exact 之前 —— 倒置。
        let after = vec![
            c("guessed", 9000, Origin::Sentence, Lane::Input),
            c("exact", 900, Origin::SystemWord, Lane::Input),
        ];
        assert!(has_cross_class_inversion(&before2, &after, Lane::Input));
        assert!(!has_cross_class_inversion(&before, &before, Lane::Input));
    }

    #[test]
    fn clamp_bonus_bounds_the_reranker() {
        let base = Score::from_milli_log(1000);
        let limit = Score::from_milli_log(2000);
        // 重排器想加 9999，被钳到 2000。
        let got = clamp_bonus(base, Score::from_milli_log(9999), limit);
        assert_eq!(got, Score::from_milli_log(3000));
    }
}
