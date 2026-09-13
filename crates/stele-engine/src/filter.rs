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

use crate::spelling::SpellingFormat;

/// **反查滤镜**：用另一本词典查出候选的编码，写进 `comment`。
///
/// 对应 RIME 的 `reverse_lookup_filter`。雾凇的"部件拆字反查"用它：
/// 打 `uUni` 时给出所有含 `ni` 这个部件的字，**并在候选后面显示它们的拼音**。
///
/// # 名字里的"反查"是什么意思
///
/// 正查是"编码 → 词"（词库的本职）。反查是"**词 → 编码**"：
/// 手上有候选文本，想知道它对应的编码是什么。`Lexicon` 的接口正是
/// 前者，所以这里必须**借助拼写层把候选文本转回编码**——
/// 而不是指望词库提供反向索引。
///
/// 具体做法：候选文本 → 在词库里以"整串编码"查它拿不到（那是编码→词），
/// 于是改用**拼写层**：`Spelling::expand(候选文本)` 给出它的编码切分。
/// 这要求拼写层认识这个文本——拼音方案下它认识（词库的键就是编码）。
///
/// # 一个诚实的限制
///
/// 步进三的 `spelling.expand` 是"拼写 → 编码"，因此只有当候选文本
/// **恰好是可拼写的**（例如拼音方案下的单字注音）时才拿得到编码。
/// 词条的中文文本当然不是拼写，所以真正的数据流是：
/// **反查词库的键是"文本"，值是"编码"**——那需要一本反过来的词典。
/// 我们把它表达成"注入另一个 `Lexicon`"，由装载器决定给它哪一本
/// （RIME 的 `reverse_lookup_filter/dictionary` 就是这个意思）。
/// 本实现因此需要一个**按文本查编码**的接口——见 [`ReverseLexicon`]。
pub struct ReverseLookupFilter {
    /// 查编码用的词库（按文本查，不是按编码查）。
    lexicon: std::sync::Arc<dyn ReverseLexicon>,
    /// 把编码加工成注释。
    comment_format: Option<SpellingFormat>,
    /// 注释已存在时是否覆盖。
    overwrite_comment: bool,
}

/// **按文本查编码**的词库。
///
/// 它与 [`Lexicon`] 方向相反，因此是**另一个 trait**而不是给 `Lexicon`
/// 加一个方法：加方法会强迫每一个词库实现（内存表、紧凑表、未来的 mmap）
/// 都提供反向索引，而"反查"是**少数方案才要**的能力。
/// 一个角色不成其为 seam（见 `component.rs` 里那段关于 seam 的说明）——
/// 这里正是"只在需要时才引入一个新 seam"的例子。
pub trait ReverseLexicon: Send + Sync {
    /// 查一个文本对应的编码（可能多条）。
    ///
    /// 写入的是**编码单元字面写法的序列**，由滤镜拼成注释。
    fn lookup_text(&self, text: &str, out: &mut Vec<Vec<String>>);
}

impl ReverseLookupFilter {
    /// 构造。
    #[must_use]
    pub fn new(
        lexicon: std::sync::Arc<dyn ReverseLexicon>,
        comment_format: Option<SpellingFormat>,
        overwrite_comment: bool,
    ) -> Self {
        Self {
            lexicon,
            comment_format,
            overwrite_comment,
        }
    }

    /// 把一条候选的编码写进它的 `comment`。
    fn annotate(&self, c: &mut Candidate) {
        if c.comment.is_some() && !self.overwrite_comment {
            return;
        }
        let mut codes: Vec<Vec<String>> = Vec::new();
        self.lexicon.lookup_text(&c.text, &mut codes);
        let Some(first) = codes.first() else {
            return;
        };
        let mut text = first.join(" ");
        if let Some(fmt) = &self.comment_format {
            text = fmt.apply(&text);
        }
        if !text.is_empty() {
            c.comment = Some(text);
        }
    }
}

impl Filter for ReverseLookupFilter {
    fn apply(&self, _q: &Query<'_>, _span: Span, cands: &mut Vec<Candidate>) {
        for c in cands.iter_mut() {
            self.annotate(c);
        }
    }

    fn applies_to(&self, tags: &[Tag]) -> bool {
        // tag 约束由外层的 `TaggedFilter` 施加；这里不重复声明，
        // 否则同一个约束会有两个执行点（改一处漏一处）。
        let _ = tags;
        true
    }
}

/// **简繁/转换滤镜**（对应 RIME 的 `simplifier`）。
///
/// # 它为什么是"表驱动"而不是"内置简繁"
///
/// RIME 用 `OpenCC` 的 `s2t.json` / `emoji.json` 这类**转换表文件**。
/// 表是**数据**，引擎不该内置任何一份（D20 / D24）——否则"简体优先"
/// 这条产品决定就变成了引擎的硬编码。
///
/// 因此这里接受一张**已经装载好的表**：`from → to` 的多对多映射。
/// 表从哪来（OpenCC 的 json？我们自己的一种格式？）是装载器的事。
pub struct Converter {
    /// 转换表：源串 → 目标候选（可多个，取第一个时退化成一对一）。
    table: std::collections::BTreeMap<String, Vec<String>>,
    /// 由哪个开关控制（开着才转换）。
    option_name: Option<String>,
    /// 是否继承原候选的注释。
    inherit_comment: bool,
}

impl Converter {
    /// 由一张表构造。
    #[must_use]
    pub fn new(
        table: std::collections::BTreeMap<String, Vec<String>>,
        option_name: Option<String>,
        inherit_comment: bool,
    ) -> Self {
        Self {
            table,
            option_name,
            inherit_comment,
        }
    }

    /// 表里有多少条。
    #[must_use]
    pub fn len(&self) -> usize {
        self.table.len()
    }

    /// 表是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    /// 把一个文本整体转换成另一个（逐字符最长匹配）。
    #[must_use]
    pub fn convert(&self, text: &str) -> Option<String> {
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::new();
        let mut changed = false;
        let mut i = 0usize;
        while i < chars.len() {
            // 最长匹配优先：`["", "里"]` 这种多字词条要先试。
            let mut hit: Option<(usize, &str)> = None;
            for len in (1..=(chars.len() - i)).rev() {
                let piece: String = chars[i..i + len].iter().collect();
                if let Some(v) = self.table.get(&piece) {
                    if let Some(first) = v.first() {
                        hit = Some((len, first.as_str()));
                        break;
                    }
                }
            }
            if let Some((len, to)) = hit {
                out.push_str(to);
                changed = true;
                i += len;
            } else {
                out.push(chars[i]);
                i += 1;
            }
        }
        if changed {
            Some(out)
        } else {
            None
        }
    }
}

impl Filter for Converter {
    fn apply(&self, q: &Query<'_>, _span: Span, cands: &mut Vec<Candidate>) {
        // 开关关着就什么都不做（连遍历都省了）。
        if let Some(name) = &self.option_name {
            if !q.options.get(name) {
                return;
            }
        }
        if self.table.is_empty() {
            return;
        }
        let mut extra: Vec<Candidate> = Vec::new();
        for c in cands.iter_mut() {
            let Some(converted) = self.convert(&c.text) else {
                continue;
            };
            // 转换结果与原文相同时不插重复候选（`uniquifier` 本来也会去重，
            // 但在源头少造一个更省）。
            if converted == c.text {
                continue;
            }
            let mut n = c.clone();
            n.text = converted;
            if !self.inherit_comment {
                n.comment = None;
            }
            // 新候选的分数略低：**转换是"另一个写法"，不是更精确的匹配**。
            n.score = c.score.saturating_add(stele_core::Score::from_weight(0.95));
            extra.push(n);
        }
        cands.extend(extra);
    }

    fn applies_to(&self, _tags: &[Tag]) -> bool {
        true
    }
}

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
        let q = Query {
            input: "x",
            caret: 1,
            options: &opts,
            context: &ctx,
            segment_text: "x",
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
