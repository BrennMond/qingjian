//! # Score
//!
//! 中文职责：候选分数的定点表示。对数域，单位是**毫对数**（milli-log）。
//! English role: fixed-point log-domain score, in milli-log units.
//! 架构位置：stele-core 的基础数值类型，被 candidate / sort / ranker / memory 使用。
//!
//! # 为什么是定点整数而不是 `f64`（PLAN D13）
//!
//! 铁律第 2 条要求"同一输入永远逐字节相同"。但**浮点加法不满足结合律**，
//! 以下情况都会破坏它：并行归约、遍历 `HashMap` 累加、以及**不同平台 / 不同 libm
//! 的 `ln`/`exp` 可能差 1 ULP**——而本引擎同时要跑 Windows x86-64 与 Android ARM。
//!
//! 定点整数的加法与比较是**精确**的，跨平台逐位一致。关键在于：
//! **`ln` 只在编译期需要算**——把词频转成分数时用 `f64` 算完、取整、存表；
//! **运行时只做整数加法与比较**，永远不需要 `ln`。
//!
//! 附带的三个好处：内存 4 字节（vs `f64` 的 8）、整数运算更快、
//! **NaN 在类型层面不存在**（因此排序天然全序、天然可复现）。

use core::fmt;
use core::ops::Add;

/// 下界：约 `ln(1e-9)`。替代 `-∞`，保证永不产生 `-inf`。
const MIN_MILLI_LOG: i32 = -20_723;

/// 上界：约 `ln(2e9)`。覆盖一切现实权重。
const MAX_MILLI_LOG: i32 = 21_474;

/// 候选分数。**越大越靠前**，单位是毫对数（1 单位 = 0.001 的 `ln` 值）。
///
/// ```
/// # use stele_core::Score;
/// // 权重 1000 的词 → ln(1000) ≈ 6.908
/// let s = Score::from_weight(1000.0);
/// assert_eq!(s.as_milli_log(), 6908);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash, Default)]
pub struct Score(i32);

impl Score {
    /// 零分（对应权重 1）。
    pub const ZERO: Self = Self(0);

    /// 下界，替代 `ln(0) = -∞`。所有权重为 0 / 概率为 0 的情形都钳到这里。
    pub const FLOOR: Self = Self(MIN_MILLI_LOG);

    /// 上界。
    pub const CEIL: Self = Self(MAX_MILLI_LOG);

    /// 直接由毫对数构造。
    #[must_use]
    pub const fn from_milli_log(milli_log: i32) -> Self {
        Self(milli_log)
    }

    /// 取毫对数原值（供调试工具、序列化、测试使用）。
    #[must_use]
    pub const fn as_milli_log(self) -> i32 {
        self.0
    }

    /// 由权重（正数）构造分数。
    ///
    /// 内部使用浮点 `ln`，因此**只允许在编译期 / 加载期调用**；
    /// **按键路径上禁止调用**（见 `docs/engine-design.md` §5.7）。
    ///
    /// 非正数（含 0、负数、`NaN`）一律返回 [`Score::FLOOR`]——这是"概率为 0"
    /// 的定点表示，同时天然挡住了 `NaN` 传播。
    #[must_use]
    // 故意用 `!(weight > 0.0)` 而不是 `weight <= 0.0`：前者对 NaN 返回 true，
    // 正是我们要的（NaN 权重必须落到 FLOOR）。换成后者会让 NaN 漏过去。
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    pub fn from_weight(weight: f64) -> Self {
        if !(weight > 0.0) {
            return Self::FLOOR;
        }
        let milli = (weight.ln() * 1000.0).round();
        // `clamp` 之前要挡住 NaN：`!(weight > 0.0)` 已经挡掉了 NaN 输入，
        // 但 `weight.ln()` 对 +inf 会返回 +inf，故仍需钳位。
        if milli.is_nan() {
            return Self::FLOOR;
        }
        let milli = milli.clamp(f64::from(MIN_MILLI_LOG), f64::from(MAX_MILLI_LOG));
        // SAFETY(逻辑上): milli 已被钳进 i32 的可表示区间。
        #[allow(clippy::cast_possible_truncation)]
        Self(milli as i32)
    }

    /// 相加：**饱和**在 Score 的**值域**内，而不只是防 i32 溢出。
    ///
    /// 两件事不同：`i32::saturating_add` 只保证不溢出（上界约 21 亿），
    /// 而 `Score` 的值域是 `[FLOOR, CEIL]`（约 ±21）。若只防溢出，
    /// 两个 `CEIL` 相加会得到 `42948` —— 一个**超出定义域**的分数，
    /// 它会静默地压过一切，破坏"分数可比"的前提。
    #[must_use]
    pub const fn saturating_add(self, rhs: Self) -> Self {
        let sum = self.0.saturating_add(rhs.0);
        if sum > MAX_MILLI_LOG {
            Self(MAX_MILLI_LOG)
        } else if sum < MIN_MILLI_LOG {
            Self(MIN_MILLI_LOG)
        } else {
            Self(sum)
        }
    }

    /// 转回权重（浮点）。**仅供调试工具与测试**——运行期不参与排序。
    #[must_use]
    pub fn to_weight_f64(self) -> f64 {
        (f64::from(self.0) / 1000.0).exp()
    }
}

impl Add for Score {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        self.saturating_add(rhs)
    }
}

impl fmt::Display for Score {
    /// 显示为毫对数原值——**故意不显示权重**，因为排序发生在对数域。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}ml", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weight_one_is_zero() {
        assert_eq!(Score::from_weight(1.0), Score::ZERO);
    }

    #[test]
    fn larger_weight_scores_higher() {
        assert!(Score::from_weight(6170.0) > Score::from_weight(1.0));
    }

    #[test]
    fn non_positive_weight_floors() {
        assert_eq!(Score::from_weight(0.0), Score::FLOOR);
        assert_eq!(Score::from_weight(-1.0), Score::FLOOR);
        assert_eq!(Score::from_weight(f64::NAN), Score::FLOOR);
    }

    #[test]
    fn huge_weight_saturates_instead_of_overflowing() {
        assert_eq!(Score::from_weight(f64::INFINITY), Score::CEIL);
    }

    #[test]
    fn addition_saturates() {
        assert_eq!(Score::CEIL.saturating_add(Score::CEIL), Score::CEIL);
        assert_eq!(Score::FLOOR.saturating_add(Score::FLOOR), Score::FLOOR);
    }

    #[test]
    fn ordering_is_total_and_exact() {
        // 整数排序天然全序：不存在 NaN，故不存在"分不出先后"的情形。
        let mut v = [
            Score::from_weight(1.0),
            Score::from_weight(1000.0),
            Score::FLOOR,
            Score::from_weight(1000.0),
        ];
        v.sort();
        assert_eq!(v[0], Score::FLOOR);
        assert_eq!(v[3], Score::from_weight(1000.0));
    }

    #[test]
    fn round_trip_is_accurate_enough() {
        let w = 12345.0;
        let back = Score::from_weight(w).to_weight_f64();
        assert!((back / w - 1.0).abs() < 1e-3, "round trip {w} -> {back}");
    }
}
