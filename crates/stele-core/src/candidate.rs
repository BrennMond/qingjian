//! # Candidate
//!
//! 中文职责：候选及其两个正交属性轴（来源 / 拼写变形），以及限流的写入缓冲。
//! English role: candidates plus their two orthogonal attribute axes, and a rate-limited sink.
//! 架构位置：stele-core 的产出类型，由 Translator 生成、Filter 改写、前端渲染。
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
}
