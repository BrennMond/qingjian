//! # Projection — 确定的 ±1 投影向量
//!
//! 中文职责：给一个词算出一条**确定**的 ±1 向量（Random Projection 的基底）。
//! English role: a deterministic ±1 projection vector per word.
//! 架构位置：`qingjian-embed` 的最底层；[`crate::model`] 用它把共现矩阵投影到低维。
//!
//! # 为什么不用随机数（D38 的直接结果）
//!
//! 外部不确定性一律注入，而这里的随机**不需要是真的**：投影矩阵只需要
//! "看起来不相关"，而这一点可以**由词的哈希**确定地给出。于是：
//!
//! - **同一份历史 ⇒ 同一批向量**（PLAN §5.2 可复现），跨进程、跨平台都成立；
//! - 不需要 `RandomSource`，也就不需要在装配处多传一个服务。
//!
//! 取 splitmix64 做扩散（与 `qingjian_core::DeterministicRandom` 同一个算法，
//! 那一份是给"需要流式随机"的场景用的；这里需要的是"按词的哈希取值"，
//! 因此**不能**直接复用它——复用会要求把发生器的状态带到查询路径上）。

/// FNV-1a 64 位。**只用于把词映射到一个确定的起点**，不是密码学哈希。
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 把 `word` 的 ±1 投影向量写进 `out`。
///
/// # 确定性
///
/// 完全由 `word` 的字节决定：同样的词在任何进程、任何平台给出同一个向量。
/// **这是"同一份历史 ⇒ 同一套候选顺序"的前提之一。**
///
/// # 为什么是 ±1 而不是高斯
///
/// 因为整数。±1 的投影向量让"累加"变成纯整数加法（没有乘法、没有浮点），
/// 于是投影本身不引入任何跨平台漂移——符合 D13 对"影响排序的量"的要求。
pub fn signs(word: &str, out: &mut [i32]) {
    let mut state = fnv1a(word.as_bytes());
    for slot in out.iter_mut() {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        // 取最低位定符号：splitmix64 的最低位分布已经足够好，
        // 而我们只需要"不相关"，不需要"密码学随机"。
        *slot = if z & 1 == 0 { 1 } else { -1 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_word_always_gives_the_same_vector() {
        let mut a = [0i32; 32];
        let mut b = [0i32; 32];
        signs("微信", &mut a);
        signs("微信", &mut b);
        assert_eq!(a, b, "同一个词必须给出逐位相同的投影");
        // 而且只有 ±1。
        assert!(a.iter().all(|x| *x == 1 || *x == -1));
    }

    #[test]
    fn different_words_get_different_vectors() {
        let mut a = [0i32; 32];
        let mut b = [0i32; 32];
        signs("微信", &mut a);
        signs("朋友圈", &mut b);
        assert_ne!(a, b);
    }

    #[test]
    fn projections_are_roughly_uncorrelated() {
        // 随机投影之所以能当低维嵌入用，前提是"两个不相关的词的点积约为 0"。
        // 这条不做统计断言（那是概率命题），只钉住**量级**：
        // 32 维上随机 ±1 向量的点积落在 ±12 之内的概率很高，
        // 而"完全同向"（=32）不该出现。
        let words = ["今天", "明天", "天气", "微信", "电脑", "学习", "工作"];
        let mut a = [0i32; 32];
        let mut b = [0i32; 32];
        for w1 in words {
            signs(w1, &mut a);
            for w2 in words {
                if w1 == w2 {
                    continue;
                }
                signs(w2, &mut b);
                let dot: i32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
                assert!(dot.abs() < 32, "{w1} 与 {w2} 的投影不该完全同向");
            }
        }
    }
}
