//! # Candidate
//!
//! 中文职责：候选及其两个正交属性轴（来源 / 拼写变形），以及限流的写入缓冲。
//! English role: candidates plus their two orthogonal attribute axes, and a rate-limited sink.
//! 架构位置：qingjian-core 的产出类型，由 Translator 生成、Filter 改写、前端渲染。
//!
//! # 两个轴，不是一个枚举（G9）
//!
//! 早期设计把"候选从哪来"和"编码怎么拼出来的"挤进一个 `Source` 枚举，
//! 于是**一条同时由模糊音和简拼派生的编码无法表达**（只能任选其一）。
//! RIME 的拼写运算给派生拼写附加**可叠加**的属性，因此必须拆成：
//!
//! - [`Origin`]：候选**从哪来**（单值）
//! - [`SpellingAttr`]：编码**怎么拼出来的**（位集，可叠加）

use crate::score::Score;
use std::sync::Arc;

/// 输入串中的一段，左闭右开 `[start, end)`，单位是**字节**。
///
/// 用字节而非字符：一个汉字在 UTF-8 里占 3 个字节，而引擎内部对输入串的
/// 一切切片都以字节计（Rust 的 `String` 天然如此）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct Span {
    /// 起始字节偏移。
    pub start: usize,
    /// 结束字节偏移（不含）。
    pub end: usize,
}

impl Span {
    /// 构造。
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// 一个位于 `at` 的空区间——`Lane::Predict` 的候选用它。
    #[must_use]
    pub const fn empty_at(at: usize) -> Self {
        Self { start: at, end: at }
    }

    /// 长度（字节）。
    #[must_use]
    pub const fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// 是否为空区间。
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.start >= self.end
    }
}

/// 这条编码**是怎么被拼出来的**——位集，**可以叠加**。
///
/// 来自 RIME 的拼写运算：`fuzz` 给派生拼写附加「模糊」属性，
/// `abbrev` 附加「缩略」属性，**两者可以同时成立**。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct SpellingAttr(u8);

impl SpellingAttr {
    /// 规范拼写，未经过任何变形。
    pub const NORMAL: Self = Self(0);
    /// 模糊音。
    pub const FUZZY: Self = Self(1 << 0);
    /// 简拼（缩略）。
    pub const ABBREV: Self = Self(1 << 1);
    /// 补全。
    pub const COMPLETION: Self = Self(1 << 2);
    /// 纠错。
    pub const CORRECTION: Self = Self(1 << 3);

    /// 由原始位构造。
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// 取原始位。
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// 是否经过了任何变形——即这条编码**不是规范拼写**。
    ///
    /// 两个用途：UI 的"猜测"标记（G12），以及学习主键的规范化判据（G10）。
    #[must_use]
    pub const fn is_derived(self) -> bool {
        self.0 != 0
    }

    /// 交集：合并两个来路时用。
    ///
    /// 只有当**所有**来路都是规范拼写时，合并结果才算规范拼写。
    #[must_use]
    pub const fn intersect(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// 并集。
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// 是否包含某一位。
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl core::ops::BitOr for SpellingAttr {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

/// 候选**从哪来**——单值。
///
/// **声明顺序即平局时的优先级**（`#[derive(Ord)]`）。
/// 调整顺序等于调整排序行为，见 `sort` 模块。
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord, Hash)]
pub enum Origin {
    // ── 不是猜的（NotGuessed）──
    /// 用户词库里的词（用户打过并确认过）。
    UserWord,
    /// 词库精确匹配。
    SystemWord,
    /// 原样上屏（英文、标点、数字）。输入本身就是答案，不是猜测。
    Literal,
    // ── 猜出来的（Guessed）──
    /// 造句（动态规划拼出来的组合）。
    Sentence,
    /// **下一词预测**（P4b）：文本来自预测表（个人 n-gram），不是当前输入。
    ///
    /// 它一定是 [`Lane::Predict`] 的候选——`Lane::Input` 的候选永远不是
    /// 预测出来的。把它与 `Sentence` 分开，是因为两者是**不同的猜测**：
    /// 一个在回答"你敲的这串想要哪个词"，一个在回答"你接下来想打什么"，
    /// 而 UI 与诊断（`--candidates`）都该把它们分开显示。
    Prediction,
}

/// 候选属于哪条通道。
///
/// 这是让"精确优先"与"下一词预测"同时成立的关键：**两者的规则完全不同**，
/// 混在一个列表里用一套规则管，必然二选一地牺牲掉一方。
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum Lane {
    /// 对应当前输入的候选：`nihao` → 你好。
    ///
    /// 严格规则：全序、精确优先、参与"第 N 个就是我要的"盲选、计入学习。
    #[default]
    Input,
    /// 对"你接下来可能想打的词"的猜测：微信 → 朋友圈。
    ///
    /// 宽松规则：可混排、数量受限、**不参与盲选**、学习单独计、可一键关闭。
    Predict,
}

/// 候选的**生成方式**——第三个轴。
///
/// # 为什么需要它（这个轴是后补的，不是设计出来的）
///
/// 早先只有两个轴：[`Origin`]（从哪来）与 [`SpellingAttr`]（编码怎么拼的）。
/// 在做 rime-ice 的等价零件时发现不够——有两个滤镜明确按"候选类型"分支：
///
/// | 零件 | 它要区分什么 |
/// | --- | --- |
/// | `autocap_filter` | 自动大写的候选**不能**是补全来的（`cand.type ~= "completion"`） |
/// | `reduce_english_filter` | 用户词库的词**不降权**（`cand.type == "user_table"`） |
///
/// 这两条用 `Origin` 表达不了：`Origin` 说的是"词条从哪个库来"，
/// 而这里要的是"这一条是**怎么被找出来的**"——同一个系统词条，
/// 精确匹配出来与补全出来是两种 `kind`。
///
/// **判据不是"RIME 有这么个字段"，而是"有没有零件真的按它分支"。**
/// 现在有，所以它进内核；将来再多一个分支理由，就往这里加一个变体。
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum CandidateKind {
    /// 词库的常规命中。
    #[default]
    Normal,
    /// **补全**：只打了编码的一部分，词条比输入更长。
    Completion,
    /// **用户词典**里的词。
    ///
    /// 与 [`Origin::UserWord`] 的关系：那个是"来源"，这个是"生成方式"。
    /// 目前两者总是一致（用户词只从用户词典出），但**不是同一个概念**——
    /// 排序看 `Origin`，滤镜看 `kind`。
    UserTable,
    /// 造句拼出来的。
    Sentence,
    /// 标点 / 符号表。
    Punct,
    /// 引擎**直接生成**的（日期、UUID、Unicode、计算结果……）。
    ///
    /// 这些候选的文本不在任何词库里，是代码算出来的——RIME 里
    /// 这类东西全部躺在 Lua 插件里。
    Inline,
}

/// 一个候选。
#[derive(Clone, Debug)]
pub struct Candidate {
    /// 上屏的文本。
    pub text: String,
    /// 候选窗右侧的备注（拼音、来源说明等）。
    pub comment: Option<String>,
    /// **对数域**分数（定点整数）：越大越靠前。
    ///
    /// 天然全序、**不存在 NaN**，因此排序永远可复现。
    pub score: Score,
    /// 候选从哪来——决定平局优先级与学习行为。
    pub origin: Origin,
    /// 这条编码是怎么拼出来的——位集，**可叠加**。
    pub attr: SpellingAttr,
    /// 覆盖输入串的哪一段。
    ///
    /// **`Lane::Predict` 的候选没有对应的输入**，此时必须填
    /// [`Span::empty_at`]`(caret)`（caret 处的空区间），
    /// 不得复用别的候选的 span——否则"按 span 切分/上屏"的逻辑会误伤输入串。
    pub span: Span,
    /// 所属通道。
    pub lane: Lane,
    /// 这一条是**怎么被找出来的**——决定滤镜怎么对待它。
    pub kind: CandidateKind,
    /// **产生这条候选的规范编码键**（如 `ni'hao`）；没有编码时为 `None`。
    ///
    /// # 它为什么进内核（判据与 [`CandidateKind`] 相同）
    ///
    /// 不是"RIME 有这么个字段"，而是"**有没有零件真的按它分支**"。
    /// 有，而且是两个：**用户记忆按它查表、上屏时按它落库**。
    ///
    /// 这条路只有翻译器走得通：编码在翻译器手里（`Expansion::code`），
    /// 到了候选列表这一层本来就被丢掉了。丢掉之后，记忆就只能按
    /// "用户敲的那串拼写"作键——于是 `nhao` 学到的词**永远不会**帮到
    /// `nihao`，而 RIME 的用户词典（以编码为键）是共享的（PLAN D42）。
    ///
    /// **"规范"的意思是**：同一条编码只有一把键，无论它是被哪条拼写
    /// （全拼 / 简拼 / 模糊音 / 补全）走到的。跨拼法共享因此是**自动**的，
    /// 不需要在记忆层做任何反查。
    ///
    /// # 为什么是 `Arc<str>` 而不是 `String`
    ///
    /// 同一条编码下的全部同音词**共享一份**；而候选在流水线里会被克隆
    /// （每个分段一份、debug 守卫一份）。克隆 `String` 会把热路径变成
    /// 分配热点，克隆 `Arc` 只是一次原子自增。
    pub key: Option<Arc<str>>,
}

impl Candidate {
    /// 规范编码键的字符串视图。
    #[must_use]
    pub fn key_str(&self) -> Option<&str> {
        self.key.as_deref()
    }
}

/// 把一条编码渲染成**记忆的键**：编码单元的规范写法用 `'` 连接。
///
/// # 为什么要有分隔符
///
/// 直接拼接会撞车：编码 `["n", "i"]` 与 `["ni"]` 拼出来都是 `ni`，
/// 而它们是两条**不同的编码**。`'` 是拼音方案里惯用的音节分隔符，
/// 因此这个键既可读又无歧义。
///
/// 返回 `None` 表示有编码单元不在字母表里——那是**装载期的错误**
/// （`SchemeDef::compile` 会响亮报出来），这里只能选择不产出键。
#[must_use]
pub fn code_key(
    alphabet: &crate::service::CodeAlphabet,
    code: &[crate::service::CodeUnitId],
) -> Option<Arc<str>> {
    if code.is_empty() {
        return None;
    }
    let mut out = String::new();
    for (i, unit) in code.iter().enumerate() {
        let text = alphabet.text(*unit)?;
        if i > 0 {
            out.push('\'');
        }
        out.push_str(text);
    }
    Some(Arc::from(out))
}

/// 这个候选是不是"输入本身就是答案"，而不是引擎猜的。
///
/// **判据由两个轴共同决定**：来源不是猜测，**且**编码没有被变形。
/// 一条模糊音派生出来的词，即使词库里真有这个词，用户看到的也是"猜的"。
///
/// **全项目只有这一处定义**——排序、断言、重排守卫都复用它。
#[must_use]
pub fn is_exact(origin: Origin, attr: SpellingAttr) -> bool {
    matches!(
        origin,
        Origin::UserWord | Origin::SystemWord | Origin::Literal
    ) && !attr.is_derived()
}

/// 候选的写入缓冲，带**容量上限**。
///
/// 超限的候选被静默丢弃——这保护引擎不被"某个查询命中几万条词条"拖垮。
///
/// **注意一个次序陷阱**：容量是在**去重之前**扣的。若某个翻译器产出大量重复候选
/// （词库有重复词条时很常见），配额会被重复项吃掉。因此去重应尽早做，
/// 且 `cap` 要留足余量。
pub struct CandidateSink<'a> {
    buf: &'a mut Vec<Candidate>,
    cap: usize,
}

impl<'a> CandidateSink<'a> {
    /// 由一个外部缓冲区构造。
    pub fn new(buf: &'a mut Vec<Candidate>, cap: usize) -> Self {
        Self { buf, cap }
    }

    /// 推入一个候选；超出上限则丢弃。
    pub fn push(&mut self, c: Candidate) {
        if self.buf.len() < self.cap {
            self.buf.push(c);
        }
    }

    /// 当前已写入的数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// 是否还没有写入任何候选。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// 还剩多少配额。
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.cap.saturating_sub(self.buf.len())
    }

    /// 已写入的候选（只读迭代）。
    ///
    /// 翻译器用它做**去重**（例如"造句的结果与已有的整词候选同名"），
    /// 而不是自己再维护一份影子列表。
    pub fn iter(&self) -> impl Iterator<Item = &Candidate> {
        self.buf.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(text: &str, origin: Origin, attr: SpellingAttr) -> Candidate {
        Candidate {
            text: text.to_owned(),
            comment: None,
            score: Score::ZERO,
            origin,
            attr,
            span: Span::new(0, 1),
            lane: Lane::Input,
            kind: CandidateKind::Normal,
            key: None,
        }
    }

    #[test]
    fn sink_respects_cap() {
        let mut buf = Vec::new();
        let mut sink = CandidateSink::new(&mut buf, 2);
        for _ in 0..5 {
            sink.push(cand("x", Origin::SystemWord, SpellingAttr::NORMAL));
        }
        assert_eq!(sink.len(), 2);
        assert_eq!(sink.remaining(), 0);
    }

    #[test]
    fn derived_attr_is_not_exact() {
        assert!(is_exact(Origin::SystemWord, SpellingAttr::NORMAL));
        // 词库里真有这个词，但编码是简拼派生的 —— 用户看到的是"猜的"。
        assert!(!is_exact(Origin::SystemWord, SpellingAttr::ABBREV));
        assert!(!is_exact(Origin::Sentence, SpellingAttr::NORMAL));
    }

    #[test]
    fn attr_is_a_bitset_and_can_overlap() {
        // 一条边可以同时是模糊音和简拼 —— 这正是单选题表达不了的（G9）。
        let both = SpellingAttr::FUZZY | SpellingAttr::ABBREV;
        assert!(both.contains(SpellingAttr::FUZZY));
        assert!(both.contains(SpellingAttr::ABBREV));
        assert!(both.is_derived());
        // 合并两条来路时取交集：只要有一条经过变形，结果就不算规范。
        assert_eq!(both.intersect(SpellingAttr::NORMAL), SpellingAttr::NORMAL);
        assert_eq!(
            SpellingAttr::FUZZY.intersect(SpellingAttr::FUZZY),
            SpellingAttr::FUZZY
        );
    }

    #[test]
    fn predict_span_is_empty() {
        let s = Span::empty_at(3);
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn code_keys_separate_units_so_different_codes_never_collide() {
        use crate::service::{CodeAlphabet, CodeUnitId};
        let alphabet = CodeAlphabet::new(vec!["n".into(), "i".into(), "ni".into()]);
        // `["n","i"]` 与 `["ni"]` 是**两条不同的编码**。直接拼接会得到同一个
        // `ni`，于是两条编码共享一份记忆——一个安静而难查的错误。
        let split = code_key(&alphabet, &[CodeUnitId(0), CodeUnitId(1)]).unwrap();
        let whole = code_key(&alphabet, &[CodeUnitId(2)]).unwrap();
        assert_ne!(split, whole);
        assert_eq!(&*split, "n'i");
        assert_eq!(&*whole, "ni");
    }

    #[test]
    fn code_key_rejects_unknown_units() {
        use crate::service::{CodeAlphabet, CodeUnitId};
        let alphabet = CodeAlphabet::new(vec!["ni".into()]);
        // 空编码没有键（它不是"一条编码"）。
        assert!(code_key(&alphabet, &[]).is_none());
        // 越界的编号不是我们的错误——装载期已经报过，这里只能不产键。
        assert!(code_key(&alphabet, &[CodeUnitId(7)]).is_none());
    }
}
