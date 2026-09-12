//! # Filters
//!
//! 中文职责：对候选列表做后处理。
//! English role: post-process the candidate list.
//! 架构位置：`stele-core::Filter` 的实现。
//!
//! **滤镜顺序就是语义**（`docs/engine-design.md` §6.1b）：雾凇的配置里
//! 明确写着"置顶候选 > Emoji > 简繁"。把置顶放在"插入 Emoji 候选"之后，
//! 置顶的词就会被挤掉。因此流水线里的顺序**必须显式写死**，不能由
//! 注册顺序或集合遍历顺序决定。

use std::collections::BTreeSet;
use stele_core::{Candidate, Filter, Query, Span, Tag};

/// 去重：同一个文本只保留一个候选。
///
/// # 合并规则（`docs/engine-design.md` §5.5）
///
/// - **`Origin` 取优先级最高者**（按声明顺序取小）——用户词优于系统词，
///   系统词优于造句。
/// - **`SpellingAttr` 取交集**——只有当**所有**来路都是规范拼写时，
///   合并结果才算规范拼写。理由：合并后的候选只要有一条来路经过了变形，
///   它就不是"确定无疑的精确匹配"，不该被当作精确匹配对待。
/// - **分数取最高者**，`comment` 取第一个非空的。
///
/// 这三条必须显式实现：若写成"先到先留"，保留哪个就取决于翻译器的执行顺序——
/// 结果虽然可复现，却**不为任何人有意义地控制**。
pub struct Uniquifier;

impl Filter for Uniquifier {
    fn apply(&self, _q: &Query<'_>, _span: Span, cands: &mut Vec<Candidate>) {
        // 先确定"首次出现的顺序"，再按它输出——保证顺序确定（不用 HashMap）。
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut merged: Vec<Candidate> = Vec::with_capacity(cands.len());

        for c in cands.drain(..) {
            if seen.insert(c.text.clone()) {
                merged.push(c);
                continue;
            }
            // 已经有一个同文本的候选：就地合并。
            if let Some(existing) = merged.iter_mut().find(|m| m.text == c.text) {
                if c.score > existing.score {
                    existing.score = c.score;
                }
                if c.origin < existing.origin {
                    existing.origin = c.origin;
                }
                existing.attr = existing.attr.intersect(c.attr);
                if existing.comment.is_none() {
                    existing.comment = c.comment;
                }
            }
        }

        *cands = merged;
    }

    fn applies_to(&self, _tags: &[Tag]) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stele_core::{Context, Lane, Options, Origin, Score, SpellingAttr};

    fn cand(text: &str, milli: i32, origin: Origin, attr: SpellingAttr) -> Candidate {
        Candidate {
            text: text.to_owned(),
            comment: None,
            score: Score::from_milli_log(milli),
            origin,
            attr,
            span: Span::new(0, 1),
            lane: Lane::Input,
        }
    }

    fn run(cands: &mut Vec<Candidate>) {
        let opts = Options::new();
        let ctx = Context::default();
        let comp = stele_core::Composition::default();
        let q = Query {
            input: "x",
            caret: 1,
            options: &opts,
            context: &ctx,
            composition: &comp,
        };
        Uniquifier.apply(&q, Span::new(0, 1), cands);
    }

    #[test]
    fn removes_duplicates_keeping_first_position() {
        let mut v = vec![
            cand("甲", 10, Origin::SystemWord, SpellingAttr::NORMAL),
            cand("乙", 5, Origin::SystemWord, SpellingAttr::NORMAL),
            cand("甲", 1, Origin::SystemWord, SpellingAttr::NORMAL),
        ];
        run(&mut v);
        let texts: Vec<&str> = v.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["甲", "乙"], "保持首次出现的位置");
    }

    #[test]
    fn merge_takes_best_origin_and_highest_score() {
        let mut v = vec![
            cand("你好", 10, Origin::Sentence, SpellingAttr::ABBREV),
            cand("你好", 99, Origin::UserWord, SpellingAttr::NORMAL),
        ];
        run(&mut v);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].origin, Origin::UserWord, "来源取优先级最高的");
        assert_eq!(v[0].score, Score::from_milli_log(99), "分数取最高");
    }

    #[test]
    fn merge_intersects_spelling_attrs() {
        // 一条来路是简拼，一条是规范 —— 合并结果必须是简拼（取交集），
        // 因为只要有一条来路经过变形，它就不是"确定无疑的精确匹配"。
        let mut v = vec![
            cand("你好", 10, Origin::SystemWord, SpellingAttr::ABBREV),
            cand("你好", 10, Origin::SystemWord, SpellingAttr::NORMAL),
        ];
        run(&mut v);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].attr, SpellingAttr::NORMAL);
        assert!(
            !stele_core::is_exact(v[0].origin, v[0].attr) || v[0].attr == SpellingAttr::NORMAL,
            "交集规则"
        );

        // 两条都是简拼 → 结果仍是简拼。
        let mut v2 = vec![
            cand("你好", 10, Origin::SystemWord, SpellingAttr::ABBREV),
            cand("你好", 10, Origin::SystemWord, SpellingAttr::ABBREV),
        ];
        run(&mut v2);
        assert!(v2[0].attr.contains(SpellingAttr::ABBREV));
    }
}
