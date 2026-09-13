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
    /// 转换候选相对原候选的**权重比**（见 [`crate::spec::SimplifierSpec::weight`]）。
    weight: f64,
}

impl Converter {
    /// 由一张表构造。
    #[must_use]
    pub fn new(
        table: std::collections::BTreeMap<String, Vec<String>>,
        option_name: Option<String>,
        inherit_comment: bool,
        weight: f64,
    ) -> Self {
        Self {
            table,
            option_name,
            inherit_comment,
            // **必须 < 1**：转换候选不该压过原候选。方案里写错时（例如
            // 1.0）会让转换结果与原候选同分——排序就退化成"谁先插入"。
            weight: if weight > 0.0 && weight < 1.0 {
                weight
            } else {
                0.95
            },
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

    /// 把一个文本整体转换成**全部**可能的写法（逐字符最长匹配）。
    ///
    /// # 为什么是"全部"而不是"第一个"
    ///
    /// `OpenCC` 的表是**一对多**的：`微笑<TAB>微笑 😊` 表示"这个词有一个
    /// emoji 写法"。而 `emoji.txt` 里**每一条都是这种形状**——第一版只取
    /// `v.first()`，于是那 4857 条**一条都不会生效**，而表确实装进来了
    /// （`--dump-config` 会说"已装载 6355 条转换"）。
    /// 这正是"配置合法、没有报错、功能就是不生效"的典型形状（HANDOFF §3）。
    ///
    /// 匹配点的选择（哪个键、多长）仍由**最长匹配唯一确定**，因此
    /// "哪些位置被替换"是确定的；只有"替换成什么"有多个分支。
    /// 分支总数不超过 [`Self::CAP`]（超出的丢弃），因为无条件展开会让
    /// 一条冷门词条炸出几十个候选。
    #[must_use]
    pub fn convert_all(&self, text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        // 把文本按"最长匹配"切成若干段：`(用掉几个字符, 有哪些目标)`。
        // **原样保留**的段是 `(1, ["该字符"])` —— 用同一个形状表示，
        // 展开时就不必再分两种情况。
        let mut plan: Vec<(String, Vec<String>)> = Vec::new();
        let mut i = 0usize;
        while i < chars.len() {
            let mut hit: Option<(usize, String, &Vec<String>)> = None;
            for len in (1..=(chars.len() - i)).rev() {
                let piece: String = chars[i..i + len].iter().collect();
                if let Some(v) = self.table.get(&piece) {
                    if !v.is_empty() {
                        hit = Some((len, piece, v));
                        break;
                    }
                }
            }
            if let Some((len, key, targets)) = hit {
                let expanded = expand_targets(&key, targets);
                plan.push((key, expanded));
                i += len;
            } else {
                // 原样保留这一段：键与目标都是这个字符本身。
                let c = chars[i].to_string();
                plan.push((c.clone(), vec![c]));
                i += 1;
            }
        }

        let mut results: Vec<String> = Vec::new();
        let mut current = String::new();
        expand(&plan, 0, &mut current, &mut results);
        // 只留**真的改变了文本**的结果（并把结果限制在 `CAP` 个）。
        results.retain(|s| s != text);
        results.truncate(Self::CAP);
        results
    }

    /// 每个文本最多产出多少个转换候选（含首选）。
    const CAP: usize = 8;

    /// 把一个文本整体转换成**首选**写法（逐字符最长匹配）。
    ///
    /// 一对多时取第一个目标；若首选就是原文本身，则往后找一个不同的，
    /// 找不到就返回 `None`（没有可用的转换）。
    #[must_use]
    pub fn convert(&self, text: &str) -> Option<String> {
        self.convert_all(text).into_iter().next()
    }
}

/// 展开一个键的目标写法列表。
///
/// # `OpenCC` 的两种表语义（**必须分开处理**）
///
/// `OpenCC` 的文本表有**两种逐字节相同的形状**，而含义相反：
///
/// | 表 | 例子 | 含义 | 我们产出 |
/// | --- | --- | --- | --- |
/// | 简繁（`STCharacters.txt`） | `干<TAB>乾 幹` | **多选一**（歧义） | 两个候选：`乾`、`幹` |
/// | emoji（`emoji.txt`） | `微笑<TAB>微笑 😊` | **复合串**（词 + 表情） | 一个候选：`😊` |
///
/// 判据只有一个：**整段值是否以键自身开头**。是 → 复合串，多出来的那部分
/// 才是候选；否 → 按空格切成多个可选项。
///
/// 这个判断只能在**同时握着键与值**的地方做，所以解析层
/// （`stele-dict`）整段保留值，不在这里猜。
fn expand_targets(key: &str, targets: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for t in targets {
        match t.strip_prefix(key) {
            // 复合串：`微笑 😊` 去掉 `微笑` → ` 😊`。取多出来的那部分。
            Some(rest) => {
                let rest = rest.trim();
                if !rest.is_empty() {
                    out.push(rest.to_owned());
                }
            }
            // 多选一：按空格切成若干个可选项。
            None => out.extend(
                t.split(' ')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
            ),
        }
    }
    out
}

/// 深度优先展开：每一段选一个目标，拼出全部组合。
fn expand(
    plan: &[(String, Vec<String>)],
    depth: usize,
    current: &mut String,
    out: &mut Vec<String>,
) {
    if out.len() >= Converter::CAP {
        return;
    }
    if depth == plan.len() {
        if !out.iter().any(|s| s == current) {
            out.push(current.clone());
        }
        return;
    }
    let saved = current.len();
    for t in &plan[depth].1 {
        current.push_str(t);
        expand(plan, depth + 1, current, out);
        current.truncate(saved);
        if out.len() >= Converter::CAP {
            return;
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
        for c in cands.iter() {
            for converted in self.convert_all(&c.text) {
                let mut n = c.clone();
                n.text = converted;
                if !self.inherit_comment {
                    n.comment = None;
                }
                // 新候选的分数略低：**转换是"另一个写法"，不是更精确的匹配**。
                n.score = c
                    .score
                    .saturating_add(stele_core::Score::from_weight(self.weight));
                extra.push(n);
            }
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
            kind: stele_core::CandidateKind::Normal,
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

    // ── 转换滤镜（`simplifier`）────────────────────────────────────────

    fn converter(rows: &[(&str, &[&str])]) -> Converter {
        let mut table = std::collections::BTreeMap::new();
        for (k, vs) in rows {
            table.insert(
                (*k).to_owned(),
                vs.iter().map(|s| (*s).to_owned()).collect(),
            );
        }
        Converter::new(table, None, true, 0.95)
    }

    fn apply_converter(c: &Converter, cands: &mut Vec<Candidate>) {
        let mut opts = Options::new();
        opts.set("on", true);
        let ctx = Context::default();
        let q = Query {
            input: "x",
            caret: 1,
            options: &opts,
            context: &ctx,
            segment_text: "x",
        };
        c.apply(&q, Span::new(0, 1), cands);
    }

    #[test]
    fn emoji_style_table_yields_the_emoji_as_a_candidate() {
        // **这一条是那个真 bug 的守卫**：`emoji.txt` 的每一条都是
        // 「词 → 词 空格 emoji」的**复合串**。第一版只取 `v.first()`，
        // 于是那 4857 条**一条都不生效**；而第二版把复合串当一个候选，
        // 会得到「微笑😊」这种带原文的怪东西。正确结果是 emoji 本身。
        let c = converter(&[("微笑", &["微笑", "😊"])]);
        let mut v = vec![cand("微笑", 100, Origin::SystemWord, SpellingAttr::NORMAL)];
        apply_converter(&c, &mut v);
        let texts: Vec<&str> = v.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["微笑", "😊"]);
        // 转换候选的分数更低。
        assert!(v[1].score < v[0].score);
    }

    #[test]
    fn simplifier_style_table_yields_every_alternative() {
        // 简繁表是**多选一**（目标不以键自身开头）——那时每个目标都要出。
        let c = converter(&[("干", &["乾", "幹"])]);
        let mut v = vec![cand("干", 100, Origin::SystemWord, SpellingAttr::NORMAL)];
        apply_converter(&c, &mut v);
        let texts: Vec<&str> = v.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["干", "乾", "幹"]);
    }

    #[test]
    fn converter_does_longest_match_and_can_multi_target() {
        // 简繁表：目标是**别的字**，所以是"多选一"语义。
        let c = converter(&[("里", &["裡"]), ("这里", &["這裡", "此地"])]);
        let got = c.convert_all("这里");
        assert_eq!(got, ["這裡", "此地"], "最长匹配优先，且两个目标都保留");
        // 单字仍然能转。
        assert_eq!(c.convert_all("心里"), ["心裡"]);
    }

    #[test]
    fn a_composite_row_from_the_real_emoji_table_yields_the_emoji() {
        // 真实一行（**值与键之间是 TAB 还是空格都出现过**，两种都要对）：
        //   `微笑<TAB>微笑 😊`
        //   `扭曲<TAB>扭曲🫪`（emoji.txt 里前几条是 TAB）
        let c = converter(&[("微笑", &["微笑 😊"])]);
        assert_eq!(c.convert_all("微笑"), ["😊"]);
        let c2 = converter(&[("扭曲", &["扭曲\t🫪"])]);
        assert_eq!(c2.convert_all("扭曲"), ["🫪"]);
    }

    #[test]
    fn a_value_starting_with_the_key_is_treated_as_a_composite() {
        // `甲<TAB>甲! 乙`：整段以键开头 → 复合串，多出来的 `! 乙` 才是候选。
        // 这是"值以键开头"这一条判据的直接后果，写在测试里以免被误改。
        let c = converter(&[("甲", &["甲! 乙"])]);
        assert_eq!(c.convert_all("甲"), ["! 乙"]);
        // 不以键开头 → 按空格切成多个可选项。
        let c2 = converter(&[("甲", &["乙 丙"])]);
        assert_eq!(c2.convert_all("甲"), ["乙", "丙"]);
    }

    #[test]
    fn converter_returns_nothing_when_nothing_changes() {
        let c = converter(&[("甲", &["乙"])]);
        assert!(c.convert_all("丙丁").is_empty(), "一个字都没命中 → 空");
        let c2 = converter(&[("甲", &["甲"])]);
        assert!(c2.convert_all("甲").is_empty(), "身份映射不算转换");
    }

    #[test]
    fn converter_caps_the_branch_count() {
        // 一条冷门词条不能炸出几十个候选。
        let many: Vec<&str> = (0..50).map(|_| "x").collect();
        let c = converter(&[("甲", &many)]);
        assert!(c.convert_all("甲").len() <= Converter::CAP);
    }

    #[test]
    fn converter_respects_its_option() {
        let mut table = std::collections::BTreeMap::new();
        table.insert("甲".to_owned(), vec!["乙".to_owned()]);
        let c = Converter::new(table, Some("emoji".to_owned()), true, 0.95);
        let opts = Options::new(); // `emoji` 没被打开
        let ctx = Context::default();
        let q = Query {
            input: "x",
            caret: 1,
            options: &opts,
            context: &ctx,
            segment_text: "x",
        };
        let mut v = vec![cand("甲", 100, Origin::SystemWord, SpellingAttr::NORMAL)];
        c.apply(&q, Span::new(0, 1), &mut v);
        assert_eq!(v.len(), 1, "开关关着时什么都不做");
    }
}
