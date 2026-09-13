//! # Input scan（内核侧的只读视图）
//!
//! 中文职责：给切分器传"这一轮识别出了什么"的**只读视图**。
//! English role: a read-only view of this round's recognition result,
//! passed to segmentors.
//! 架构位置：`qingjian-core` 的组件协议之一。
//!
//! # 为什么内核需要知道"认领"这件事
//!
//! 识别结果本身（`qingjian-engine::segmentor::InputScan`）是引擎的数据类型，
//! 但**切分器 trait 住在内核里**，而它必须能收到这份结果——
//! 否则切分器只能各自重新识别一遍（那就有了两个识别器，且它们必然漂移）。
//!
//! 因此内核定义这个**最小视图**：起点、终点、标签。引擎的具体类型
//! 与它之间做一次零成本转换（都是 `&[..]` 切片）。

use crate::segment::Tag;

/// 一条"认领"：某段输入有明确的归属。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Claim {
    /// 起点（字节）。
    pub start: usize,
    /// 终点（字节，不含）。
    pub end: usize,
    /// 归属的标签。
    pub tag: Tag,
}

/// 这一轮识别出的全部认领。
#[derive(Clone, Copy, Debug, Default)]
pub struct InputScanView<'a> {
    /// 按起点升序。
    pub claims: &'a [Claim],
}

impl InputScanView<'_> {
    /// 在 `pos` 处开始的认领（取第一个）。
    #[must_use]
    pub fn claim_at(&self, pos: usize) -> Option<&Claim> {
        self.claims.iter().find(|c| c.start == pos)
    }

    /// **严格晚于** `pos` 的下一个认领起点。
    ///
    /// 用于"普通编码段在哪里被迫结束"——这是让 `abc_segmentor`
    /// 不会把 `uUni` 整串吞掉的那一条。
    #[must_use]
    pub fn next_claim_start(&self, pos: usize) -> Option<usize> {
        self.claims.iter().find(|c| c.start > pos).map(|c| c.start)
    }
}
