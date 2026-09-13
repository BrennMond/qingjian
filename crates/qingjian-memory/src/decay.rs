//! # Decay — 次数与时间到定点加成的纯函数
//!
//! 中文职责：把"上屏过几次 + 最后一次是什么时候"换算成一个**定点**的
//! 候选加成。它是用户记忆的**全部数学**，因此刻意做成纯函数、可逐值断言。
//! English role: the pure function turning (commit count, last use) into a
//! fixed-point candidate bonus.
//! 架构位置：`qingjian-memory` 的最底层；`store` 在**更新时**调用它，
//! `ranker` 读它算出的结果。它不认识词库、文件或引擎。
//!
//! # 为什么全部用整数（D13）
//!
//! 加成**直接影响候选顺序**，因此必须"同一份数据、同一时刻 ⇒ 逐字节相同的
//! 候选序列"（PLAN §5.2）。而浮点加法不满足结合律，不同平台的 `libm`
//! 还可能差 1 ULP——这个引擎同时要跑 Windows x86-64 与 Android ARM。
//!
//! 所以这里的衰减是**整数右移**、加成是**整数除法**：没有任何浮点，
//! 因此"跨平台一致"不是一句承诺，而是类型层面的事实。
//!
//! # 模型：RIME 的 `dee`，但换成定点
//!
//! RIME 的用户词典存一个**已衰减的累计量**（`dee`）和一个计数值，
//! 每次上屏先让累计量随时间衰减、再 `+1`。我们照这个形状做，只是把
//! `dee` 换成"千分之一单位"的整数（[`DECAY_UNIT_MILLI`] = 一次上屏）：
//!
//! ```text
//! 每次上屏：decayed ← decay(decayed, 距上次的时长) + 1.0
//! 任何时刻：bonus   ← MAX × f / (f + H)      （f = 此刻的 decayed）
//! ```
//!
//! **为什么要存累计量而不是"次数"**：只存总次数时，"三年前打过 1000 次"
//! 与"昨天打过 1000 次"会算出同一个加成——而那正是时间衰减要区分的东西。
//!
//! # 三个常数各自的作用（都能被测试证伪）
//!
//! | 常数 | 值 | 作用 |
//! | --- | --- | --- |
//! | [`HALF_LIFE_SECS`] | 30 天 | 多久减半。**离散**成整数次减半，因此没有浮点 |
//! | [`MAX_BONUS_ML`] | 14000 毫对数 | 加成上界。它必须**高于任何真实词库权重**（实测最高约 12366），否则"打过的词永远上不了第一位" |
//! | [`HALF_SATURATION_MILLI`] | 2.0 次 | 曲线的半饱和点：f=2 时给一半加成 |

use qingjian_core::Score;

/// 时间衰减的半衰期：**30 天**。
///
/// 选它而不是 RIME 的连续公式，是为了让衰减变成**整数次减半**——
/// 于是"昨天打的"与"今天打的"在同一个半衰期内给出**逐位相同**的结果，
/// 而"30 天前打的"恰好减半。这是一条能被测试钉死的语义。
pub const HALF_LIFE_SECS: u64 = 30 * 24 * 60 * 60;

/// 一次上屏积累的"衰减退频"，单位是千分之一。
///
/// 取 1000 是为了让"一次上屏 = 1.0"在整数里读得出来，
/// 同时给未来的"一次上屏算几次"留出分数空间（例如选词 +2.0）。
pub const DECAY_UNIT_MILLI: u64 = 1_000;

/// 衰减退频的上限（等价于连续上屏 1000 次）。
///
/// 它防的是 `u64` 溢出，而不是行为：`MAX_BONUS_ML` 早就饱和了，
/// 再大的 `f` 也只会让 `f/(f+H)` 更接近 1。
pub const MAX_DECAYED_MILLI: u64 = 1_000_000;

/// 加成的上界（毫对数）。
///
/// # 为什么是 14000
///
/// 实测默认词库里的候选分数最高约 **12366**（输入 `ni` 的「你」，权重约 2.3e5）。
/// 记忆的加成**必须能超过它**，否则"连着打过很多次的词"永远排不到第一位——
/// 而那正是 P4a 的验收 1。
///
/// 14000 毫对数 ≈ 权重 1.2e6，比任何真实词条权重都高一到两个数量级，
/// 同时仍在 [`Score`] 的值域内（`CEIL` = 21474）。
pub const MAX_BONUS_ML: i32 = 14_000;

/// 半饱和点：衰减退频 `f` 等于它时，加成是上界的一半。
pub const HALF_SATURATION_MILLI: u64 = 2_000;

/// 经过 `elapsed_secs` 后，累计量要右移几位（即除以 2 的几次方）。
///
/// 向下取整：不满一个半衰期就不衰减。**这是刻意的**——它让"同一秒内连打
/// 十次"精确地等于 `+10`，而不是每次都被浮点误差啃掉一点。
#[must_use]
pub const fn halvings(elapsed_secs: u64) -> u32 {
    // u64 / u64 在 const fn 里可用；结果是"跨越了几个半衰期"。
    let steps = elapsed_secs / HALF_LIFE_SECS;
    if steps > u32::MAX as u64 {
        return u32::MAX;
    }
    // 上面的判断已经保证 `steps <= u32::MAX`。用 `as` 而不是 `u32::try_from`：
    // 后者在 const fn 里还不是稳定能力。
    #[allow(clippy::cast_possible_truncation)]
    let out = steps as u32;
    out
}

/// 让累计量衰减 `steps` 个半衰期。
///
/// `steps >= 64` 时直接归零：`u64` 右移 64 位在 Rust 里是**未定义行为式**的
/// 恐慌/回绕（取决于写法），而这里的语义本来就该是 0。
#[must_use]
pub const fn decay_milli(decayed_milli: u64, steps: u32) -> u64 {
    if steps >= 64 {
        0
    } else {
        decayed_milli >> steps
    }
}

/// 由"此刻的衰减退频"算出加成（毫对数）。
///
/// 曲线是 `MAX × f / (f + H)`：`f = 0` 时是 0，`f → ∞` 时趋近 `MAX`。
/// 整数除法向下取整，因此**单调不减**且完全确定。
#[must_use]
pub const fn bonus_ml(decayed_milli: u64) -> i32 {
    if decayed_milli == 0 {
        return 0;
    }
    // 用 u128 做中间量：`decayed_milli` 是公开函数的入参，可以是任意 u64
    // （例如 `u64::MAX`），而 `MAX_BONUS_ML as u64 * u64::MAX` 会溢出。
    // 换成 u128 之后**任何**输入都算出 ≤ `MAX_BONUS_ML` 的结果。
    // （用 `as` 而不是 `u128::from`：后者在 const fn 里还不是稳定能力。）
    let num = (MAX_BONUS_ML as u128) * (decayed_milli as u128);
    let den = (decayed_milli as u128) + (HALF_SATURATION_MILLI as u128);
    // 结果必然 ≤ MAX_BONUS_ML < i32::MAX（见上面的上界测试），因此这次收窄是安全的。
    #[allow(clippy::cast_possible_truncation)]
    let out = (num / den) as i32;
    out
}

/// "此刻"的衰减退频。
///
/// # 两处刻意的宽容
///
/// - `last_used == 0`：看成"没有经过时间"（刚建立、或时钟从 0 开始）。
///   若把它当成 1970 年，会让所有记录一装上就归零。
/// - `now < last_used`（**时钟回拨**）：`saturating_sub` 得 0，即"不衰减"。
///   时钟回拨不是我们造成的，但**不能让它变成负数**——那会算出比新建
///   记录还高的加成，或者干脆恐慌。
#[must_use]
pub const fn decayed_now(decayed_milli: u64, last_used: u64, now: u64) -> u64 {
    if last_used == 0 {
        return decayed_milli;
    }
    decay_milli(decayed_milli, halvings(now.saturating_sub(last_used)))
}

/// 一次上屏之后的衰减退频（先衰减、再 `+1`）。
#[must_use]
pub const fn decayed_after_commit(decayed_milli: u64, last_used: u64, now: u64) -> u64 {
    let d = decayed_now(decayed_milli, last_used, now) + DECAY_UNIT_MILLI;
    if d > MAX_DECAYED_MILLI {
        MAX_DECAYED_MILLI
    } else {
        d
    }
}

/// "此刻"的加成——**查询路径唯一该调用的函数**。
///
/// 记忆里的 `bonus` 是**更新时**算出来的；但时间在流逝，因此查询时
/// 必须用存下来的累计量重新算一次。否则"三年前打过的词"会一直霸占第一位——
/// 那正是设计文档点名要避免的事。
#[must_use]
pub const fn bonus_now(decayed_milli: u64, last_used: u64, now: u64) -> Score {
    Score::from_milli_log(bonus_ml(decayed_now(decayed_milli, last_used, now)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一天有多少秒（测试里反复用）。
    const DAY: u64 = 24 * 60 * 60;
    /// 一个便于阅读的"当前时刻"：2026-01-01 00:00:00 UTC。
    const T0: u64 = 1_767_225_600;

    #[test]
    fn boundary_table_is_exact() {
        // 一张**逐值**的边界表。它的作用不是"证明公式对"，而是把公式
        // **钉死**：任何人改动常数或曲线，这张表都会红。
        // 期望值全部由整数除法的定义直接得出，不是从实现里抄的。
        let table: &[(u64, i32)] = &[
            (0, 0),           // 没有记录 → 不加分
            (1_000, 4_666),   // 上屏 1 次
            (2_000, 7_000),   // 2 次
            (3_000, 8_400),   // 3 次
            (5_000, 10_000),  // 5 次
            (10_000, 11_666), // 10 次
            (100_000, 13_725),
            (1_000_000, 13_972), // 饱和附近
        ];
        for (f, want) in table {
            assert_eq!(bonus_ml(*f), *want, "f={f} 的加成应当是 {want}");
        }
    }

    #[test]
    fn max_bonus_can_beat_the_highest_real_dictionary_score() {
        // P4a 验收 1 的算术前提：加成上界必须高于真实的词库权重上限。
        // 实测默认词库最高分约 12366（输入 ni 的「你」）。
        let highest_real_score_ml = 12_366;
        assert!(
            MAX_BONUS_ML > highest_real_score_ml,
            "加成上界 {MAX_BONUS_ML} 必须能压过真实词库的最高分"
        );
        // 而且它仍在 Score 的值域内。
        assert!(Score::from_milli_log(MAX_BONUS_ML) < Score::CEIL);
    }

    #[test]
    fn bonus_is_monotone_in_frequency() {
        let mut prev = -1;
        for step in 0..2000 {
            let ml = bonus_ml(step * 500);
            assert!(ml >= prev, "f={} 时加成反而变小了", step * 500);
            prev = ml;
        }
    }

    #[test]
    fn bonus_never_exceeds_its_bound() {
        // 入参是任意 u64（它是公开函数）：极端值也必须落在上界内，且不溢出。
        for f in [0, 1, 999, 1_000_000, u64::MAX / 2, u64::MAX] {
            let ml = bonus_ml(f);
            assert!((0..=MAX_BONUS_ML).contains(&ml), "f={f} 得到 {ml}");
        }
    }

    #[test]
    fn decay_halves_every_half_life() {
        let f = 64_000;
        assert_eq!(decay_milli(f, 0), f);
        assert_eq!(decay_milli(f, 1), f / 2);
        assert_eq!(decay_milli(f, 2), f / 4);
        // 超过 64 个半衰期直接归零，而不是回绕。
        assert_eq!(decay_milli(f, 64), 0);
        assert_eq!(decay_milli(f, u32::MAX), 0);
    }

    #[test]
    fn halvings_count_whole_half_lives() {
        assert_eq!(halvings(0), 0);
        assert_eq!(halvings(HALF_LIFE_SECS - 1), 0);
        assert_eq!(halvings(HALF_LIFE_SECS), 1);
        assert_eq!(halvings(HALF_LIFE_SECS * 3 + 5), 3);
        // 极端输入不 panic。5e9 秒 ≈ 158 年 ⇒ 约 1929 个半衰期。
        assert_eq!(halvings(5_000_000_000), 1_929);
    }

    #[test]
    fn a_word_used_today_beats_the_same_word_used_a_year_ago() {
        // 时间衰减的意义：同样的"打过 5 次"，昨天打的与一年前打的不是一回事。
        let fresh = bonus_now(5_000, T0 - DAY, T0);
        let stale = bonus_now(5_000, T0 - 365 * DAY, T0);
        assert!(
            fresh > stale,
            "刚打过的应当高于一年前打过的（{fresh:?} vs {stale:?}）"
        );
        // 一年 ≈ 12 个半衰期 ⇒ 5.0 次只剩 1/4096，几乎衰减干净。
        assert!(
            stale.as_milli_log() < 100,
            "一年前打过的词应当几乎归零，实得 {stale:?}"
        );
    }

    #[test]
    fn clock_rollback_does_not_panic_or_amplify() {
        // 时钟回拨：`now` 早于 `last_used`。不许恐慌，也不许算出比"刚打过"更高的值。
        let rolled_back = bonus_now(5_000, T0, T0 - 10 * DAY);
        let fresh = bonus_now(5_000, T0, T0);
        assert_eq!(rolled_back, fresh, "时钟回拨应当等价于'没有时间流逝'");
    }

    #[test]
    fn last_used_zero_means_no_elapsed_time() {
        // `last_used == 0` 是一个真实会出现的边界（时钟从 0 开始的测试、
        // 或外部导入的记录）。把它当成 1970 年会让所有记录一上来就归零。
        assert_eq!(decayed_now(5_000, 0, T0), 5_000);
        assert_eq!(bonus_now(5_000, 0, T0), bonus_now(5_000, T0, T0));
    }

    #[test]
    fn repeated_commits_accumulate_within_one_half_life() {
        // 同一秒内连打 5 次：精确地是 5.0，而不是被浮点啃掉一点。
        let mut f = 0;
        for _ in 0..5 {
            f = decayed_after_commit(f, T0, T0);
        }
        assert_eq!(f, 5 * DECAY_UNIT_MILLI);
        assert_eq!(bonus_now(f, T0, T0), Score::from_milli_log(10_000));
    }

    #[test]
    fn accumulate_then_decay_then_accumulate() {
        // 打过 4 次 → 隔一个半衰期 → 再打 1 次：应当是 2.0 + 1.0 = 3.0。
        let mut f = 4 * DECAY_UNIT_MILLI;
        let later = T0 + HALF_LIFE_SECS;
        f = decayed_after_commit(f, T0, later);
        assert_eq!(f, 3 * DECAY_UNIT_MILLI);
    }

    #[test]
    fn decayed_is_capped() {
        let f = decayed_after_commit(MAX_DECAYED_MILLI, T0, T0);
        assert_eq!(f, MAX_DECAYED_MILLI);
    }
}
