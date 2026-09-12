//! # Segment / Composition
//!
//! 中文职责：输入串的分段状态与整体组成。
//! English role: the segmentation state of the input string and the composition as a whole.
//! 架构位置：stele-core 的会话状态，由 Segmentor 填写、Translator 消费、前端读取
//! （前端只读 [`Composition`]，不直接读 [`Segment::candidates`]）。
//!
//! # "数据模型全要，行为先做一半"
//!
//! P1 阶段所有分段都是 [`SegmentStatus::Guess`] + **每次按键从头重算**；
//! 后续再实现 [`SegmentStatus::Confirmed`] 的"已定段不再重算"行为
//! （RIME 的 `Forward()` / `Trim()` / `Reopen()`）。
//!
//! **为什么这样分**：数据结构现在定完整是便宜的（struct 多几个字段）；
//! 而增量重算的**行为**是 RIME 里最容易出错的部分，用一个几百条的词库时
//! 从头重算只要几十微秒。将来补行为时**不需要改任何类型签名**。

use crate::candidate::{Candidate, Span};

/// 标签：描述一个分段"是什么类型"。
///
/// 用 `&'static str` 而非堆分配的 `String`：标签来自封闭的已知集合
/// （`abc`、`punct`、`radical_lookup` …），方案里自定义的标签在加载期
/// 被 interning 成静态字符串。
///
/// **tag 是"切分器 → 翻译器"的绑定层**（G3）：一个分段可带**多个**标签。
pub type Tag = &'static str;

/// 一段输入的状态。
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum SegmentStatus {
    /// 还没切分。
    #[default]
    Void,
    /// 引擎给了候选，用户正在选。
    Guess,
    /// 用户选了某个候选，但还没上屏。
    Selected,
    /// 已定死，不再重算。
    Confirmed,
}

/// 切分出来的一段。
#[derive(Clone, Debug, Default)]
pub struct Segment {
    /// 覆盖输入串的哪一段。
    pub span: Span,
    /// 这一段的处理状态。
    pub status: SegmentStatus,
    /// 标签（可多个）。
    pub tags: Vec<Tag>,
    /// 这一段**未经跨段合并**的候选。
    ///
    /// **前端不要直接读它**——它没有跨段合并、没有跨通道排序、
    /// 也没有经过最终滤镜。前端读 [`crate::Session::candidates`]。
    pub candidates: Vec<Candidate>,
    /// 用户选中的下标（若已选）。
    pub selected: Option<usize>,
}

impl Segment {
    /// 构造一个空段。
    #[must_use]
    pub fn new(span: Span) -> Self {
        Self {
            span,
            ..Self::default()
        }
    }

    /// 是否带某个标签。
    #[must_use]
    pub fn has_tag(&self, tag: Tag) -> bool {
        self.tags.contains(&tag)
    }

    /// 是否带给定集合中的任意一个标签。
    #[must_use]
    pub fn has_any_tag(&self, tags: &[Tag]) -> bool {
        tags.iter().any(|t| self.has_tag(t))
    }
}

/// 一次切分的全部结果。
#[derive(Clone, Debug, Default)]
pub struct Segmentation {
    /// 按位置从左到右排列的分段。
    pub segments: Vec<Segment>,
}

impl Segmentation {
    /// 清空。
    pub fn clear(&mut self) {
        self.segments.clear();
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// 已确认部分结束的字节位置。
    #[must_use]
    pub fn confirmed_end(&self) -> usize {
        self.segments
            .iter()
            .take_while(|s| s.status == SegmentStatus::Confirmed)
            .map(|s| s.span.end)
            .last()
            .unwrap_or(0)
    }
}

/// 一次输入过程中，输入串的完整状态。
#[derive(Clone, Debug, Default)]
pub struct Composition {
    /// 原始输入，例如 `"nihao"`。
    ///
    /// **当作不透明的 token 串处理**：并击（chord）一类的输入法经由组件
    /// 把和弦变成 token 后放进这里，引擎不需要知道 token 是怎么来的（G13）。
    pub input: String,
    /// 光标位置（字节偏移）。
    pub caret: usize,
    /// 显示用的预编辑串。拼音方案下可能带音节分隔（例如 `"ni'hao"`），
    /// 分隔符由方案数据决定；引擎只负责按方案给的格式串拼接。
    pub preedit: String,
    /// 切分结果。
    pub segments: Segmentation,
}

impl Composition {
    /// 是否正在输入（输入串非空）。
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.input.is_empty()
    }

    /// 清空，准备下一次输入。
    pub fn reset(&mut self) {
        self.input.clear();
        self.caret = 0;
        self.preedit.clear();
        self.segments.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_tags_bind_translators() {
        let mut seg = Segment::new(Span::new(0, 5));
        seg.tags.push("abc");
        seg.tags.push("reverse_lookup");
        assert!(seg.has_tag("abc"));
        assert!(seg.has_any_tag(&["punct", "abc"]));
        assert!(!seg.has_any_tag(&["punct"]));
    }

    #[test]
    fn confirmed_end_counts_only_leading_confirmed() {
        let mut s = Segmentation::default();
        let mut a = Segment::new(Span::new(0, 2));
        a.status = SegmentStatus::Confirmed;
        let mut b = Segment::new(Span::new(2, 4));
        b.status = SegmentStatus::Guess;
        let mut c = Segment::new(Span::new(4, 6));
        c.status = SegmentStatus::Confirmed;
        s.segments = vec![a, b, c];
        // 只有从头连续确认的部分才算数。
        assert_eq!(s.confirmed_end(), 2);
    }

    #[test]
    fn composition_reset_clears_everything() {
        let mut c = Composition {
            input: "nihao".into(),
            caret: 5,
            preedit: "ni'hao".into(),
            ..Default::default()
        };
        assert!(c.is_active());
        c.reset();
        assert!(!c.is_active());
        assert_eq!(c.caret, 0);
        assert!(c.preedit.is_empty());
    }
}
