//! # Model — 从本地共现里学出的低维向量
//!
//! 中文职责：把"本地历史上出现过的 `(上下文词 → 下一个词)` 计数"压成
//! 一组定长整数向量，并给出"某个候选与当前上下文有多搭"的分数。
//! English role: compress locally-counted `(context word → next word)` pairs into
//! fixed-size integer vectors and score a candidate against a context.
//! 架构位置：`stele-embed` 的核心；[`crate::EmbedRanker`] 消费它。
//!
//! # 它算什么（一句话 + 一条推导）
//!
//! 记 `M[c][w]` = "`c` 之后出现过 `w`"的计数。我们不存 `M`（那是 V×V），
//! 而是给每个词一条确定的 ±1 投影 `r_x`（[`crate::projection::signs`]），
//! 只存**词侧**的向量：
//!
//! ```text
//! v_w = Σ_c  M[c][w] · r_c
//! ```
//!
//! 查询时把上下文投影成一条向量 `s = Σ_{c∈上下文} r_c`，然后
//!
//! ```text
//! dot(s, v_w) = Σ_{c∈上下文} Σ_{c'} M[c'][w] · (r_c · r_{c'})
//!             ≈ dim · Σ_{c∈上下文} M[c][w]        （因为 r_c · r_{c'} ≈ dim·δ）
//! ```
//!
//! ——**它估的就是"这个词跟着这段上下文出现过多少次"**。
//!
//! # 为什么这值得做（而不是 P4b 的重复品）
//!
//! 那个信号**今天是拿不到的**，因为三条已有的路各缺一块：
//!
//! | 已有 | 它回答 | 缺什么 |
//! | --- | --- | --- |
//! | P4a `MemoryRanker` | 这个**编码**打过哪个词 | **不看上下文** |
//! | P4b 预测表 | 这段**上下文**之后常接什么 | 只喂 `Lane::Predict`，**不碰 `Lane::Input`** |
//! | 词库权重 | 谁全局更常用 | 同上 |
//!
//! 于是"同一个编码下、按上下文该选哪个词"这件事**没有任何一档在管**：
//! 敲 `tianqi` 时，`天气` 与 `田七` 谁在前，P4a 只会看历史次数、看不出
//! 上文是「今天」还是「农田」。把这个信号投影成向量、接进 `Lane::Input`
//! 的重排链，就是本 crate 的作用。
//!
//! # 投影的另一半作用（诚实交代它的分量）
//!
//! 把这个计数表压成 `V × dim` 的向量，代价是**哈希碰撞**：两个词的投影
//! 会有约 `±√dim` 的相关，于是彼此的计数会互相"漏"一点。这带来一点点
//! **跨词的平滑**（这是它算"向量"而不是"查表"的地方），但它**不是**
//! 语义相似——真正让"共同邻居"生效的是**两步共现**，而那一版需要的
//! 量纲配平（直接项 vs 两步项）还没做，记在 `docs/embed-design.md`。
//! **不要把这一点点平滑说成"理解了语义"。**
//!
//! # 为什么全部是整数（D13）
//!
//! 向量影响候选顺序，因此必须"同一份历史 ⇒ 逐字节相同的候选序列"。
//! 浮点的加法不满足结合律、投影初始化在不同平台可能差 1 ULP——
//! 所以**没有浮点**：投影是 ±1，累加是整数，量化是整数除法与舍入。
//! 唯一的"除法"发生在训练期（按全局最大绝对值缩放到 `i16`），
//! 而那一步的输入输出都是整数，因此也是确定的。

use std::collections::BTreeMap;

use crate::projection::signs;

/// 向量记忆的配置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VectorConfig {
    /// 每条向量的维数。
    ///
    /// 维数越高，哈希碰撞越少、越接近"直接查表"；代价是内存
    /// （`V × dim × 2 字节`）。默认 32 是实测后的折中，
    /// 见 `docs/embed-design.md` 的内存账与 D46。
    pub dim: usize,
    /// 查询时看上下文的最后几个词。
    ///
    /// 与 P4b 的 trigram 对齐（默认 2）：预测表里写的正是
    /// "最近两个词"与"最近一个词"两种粒度。
    pub window: usize,
}

impl Default for VectorConfig {
    fn default() -> Self {
        Self { dim: 32, window: 2 }
    }
}

/// 一张"词侧"向量表：`v_w` 是"`w` 之前常跟哪些词的投影之和"。
pub struct VectorMemory {
    /// 词 → 行号。用 `BTreeMap`：编号顺序与量化都要求确定（PLAN §5.2）。
    index: BTreeMap<String, u32>,
    /// 维数。
    dim: usize,
    /// 词侧向量表：`v[w]`。
    v: Vec<i16>,
    /// 训练时用的窗口（查询时也要用同一个）。
    window: usize,
}

impl VectorMemory {
    /// 由一批 `(上下文词, 下一个词, 次数)` 学出向量表。
    ///
    /// 返回 `None` 表示**样本为空**（没有历史就没有向量——这不是错误，
    /// 而是"新用户"的正常状态；调用方据此不加任何分数）。
    ///
    /// # 确定性
    ///
    /// 输出的每一个字节都是 `samples`（与 `config`）的纯函数：
    /// 词表按字典序编号、投影由词哈希决定、累加是整数、量化按全局最大值。
    #[must_use]
    pub fn train<I>(samples: I, config: VectorConfig) -> Option<Self>
    where
        I: IntoIterator<Item = (Vec<String>, String, u32)>,
    {
        let dim = config.dim.clamp(1, MAX_DIM);
        // ① 收词表。必须先收完再编号，否则编号依赖输入顺序（那就不确定了）。
        let mut index: BTreeMap<String, u32> = BTreeMap::new();
        let mut collected: Vec<(Vec<String>, String, u32)> = Vec::new();
        for (context, word, count) in samples {
            if word.is_empty() || count == 0 {
                continue;
            }
            for c in &context {
                if !c.is_empty() {
                    index.entry(c.clone()).or_insert(0);
                }
            }
            index.entry(word.clone()).or_insert(0);
            collected.push((context, word, count));
        }
        if collected.is_empty() {
            return None;
        }
        for (i, slot) in index.values_mut().enumerate() {
            // 词表可能超过 u32——那是病态输入，钳住而不是 panic（D26 的精神）。
            *slot = u32::try_from(i).unwrap_or(u32::MAX);
        }

        let n = index.len();
        // ② 投影累加。用 i64 是防御性的：`count` 来自衰减计数（上限 1e6），
        //    累加再多次也不会溢出 i64。
        let mut acc = vec![0i32; n * dim];
        let mut r = vec![0i32; dim];
        for (context, word, count) in &collected {
            let wi = index[word] as usize;
            let weight = i32::try_from(*count).unwrap_or(i32::MAX);
            let row = &mut acc[wi * dim..(wi + 1) * dim];
            for c in context {
                // 上下文词不在词表里是不可能的（上面刚收过），但**不 panic**：
                // 数据是从文件读回来的，宁可少加一项也不要让输入法崩（D26）。
                if !index.contains_key(c) {
                    continue;
                }
                signs(c, &mut r);
                for (slot, sign) in row.iter_mut().zip(r.iter()) {
                    *slot = slot.saturating_add(weight.saturating_mul(*sign));
                }
            }
        }

        // ③ 量化到 i16（全局缩放 + 整数舍入 ⇒ 确定）。
        let v = quantize(&acc);

        Some(Self {
            index,
            dim,
            v,
            window: config.window.max(1),
        })
    }

    /// 向量维数。
    #[must_use]
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// 词表大小。
    #[must_use]
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// 词表是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// 向量表的常驻字节数——**D46 的第②条要求把上限写进文档与称重台**，
    /// 这个函数就是那个数字的来源。
    ///
    /// # 它**不**包含装载峰值
    ///
    /// 训练时还要一块 `词表 × 维数 × 4 字节` 的 `i32` 累加缓冲
    /// （4 万词 × 32 维 = **5.1 MiB**），以及调用方给的一份样本快照。
    /// 两者在训练结束就释放，但**分配器往往会留住页面**（RSS 只涨不落），
    /// 因此称重台上看到的增量会大于本函数的返回值。
    /// 真实上限见 `docs/embed-design.md`：**常驻 2.4 MiB @ 4 万词，
    /// 装载峰值约 +10 MiB**。
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.v.len() * std::mem::size_of::<i16>()
    }

    /// 某个词在不在词表里（诊断用）。
    #[must_use]
    pub fn knows(&self, word: &str) -> bool {
        self.index.contains_key(word)
    }

    /// 由上下文算出查询向量（`Σ_c r_c`，只看最后 `window` 个词）。
    ///
    /// 出参复用调用方的缓冲：重排器每次按键都会问一次，
    /// 而"每键一次分配"正是这个项目反复避开的形状。
    pub fn context_vector(&self, context: &[String], out: &mut Vec<i64>) {
        out.clear();
        out.resize(self.dim, 0);
        let start = context.len().saturating_sub(self.window);
        // 投影只有 `dim` 个分量，栈上开一个固定上限的缓冲即可——
        // `train` 已经把 dim 钳在 `MAX_DIM` 之内，因此这里不会越界。
        let mut r = [0i32; MAX_DIM];
        for word in &context[start..] {
            if !self.index.contains_key(word) {
                // 词表外的上下文词**没有向量**：直接跳过。
                // 不给它编一个（例如按字拆）是刻意的——那是另一套机制
                // （子词回退），而"看起来像做了、其实在猜"正是本项目最恨的形状。
                continue;
            }
            signs(word, &mut r[..self.dim]);
            for (slot, sign) in out.iter_mut().zip(r[..self.dim].iter()) {
                *slot += i64::from(*sign);
            }
        }
    }

    /// 用**已经算好的**上下文向量给一个词打分。
    ///
    /// 这就是 `dot(Σ_c r_c, v_w)`；量纲不重要（重排器用的是**相对**名次），
    /// 重要的是它是整数、且同一份向量永远给同一个数。
    #[must_use]
    pub fn score_with(&self, context_vector: &[i64], word: &str) -> i64 {
        let Some(&wi) = self.index.get(word) else {
            return 0;
        };
        let row = &self.v[wi as usize * self.dim..(wi as usize + 1) * self.dim];
        context_vector
            .iter()
            .zip(row.iter())
            .map(|(a, b)| a * i64::from(*b))
            .sum()
    }

    /// 便捷入口：给上下文与候选词直接打分（内部会分配一次上下文向量）。
    #[must_use]
    pub fn score(&self, context: &[String], word: &str) -> i64 {
        let mut cv = Vec::new();
        self.context_vector(context, &mut cv);
        self.score_with(&cv, word)
    }
}

/// `context_vector` 在栈上预留的最大维数。
///
/// 超过它的维数会在 `context_vector` 里被**截断**（并因此给出偏小的分数），
/// 而不是 panic：`VectorConfig::dim` 是配置项，配置错误不该锁死输入法（D26）。
const MAX_DIM: usize = 256;

/// 把 i32 累加量按**全局最大绝对值**缩放到 `i16`。
///
/// 全局（而不是逐行）缩放是刻意的：逐行缩放要为每一行再存一个 scale，
/// 那既费内存又给"分数比较"引入额外的非线性；全局缩放只丢动态范围，
/// 而重排器只用相对名次——**够用**。
fn quantize(acc: &[i32]) -> Vec<i16> {
    let max_abs = acc.iter().map(|x| x.unsigned_abs()).max().unwrap_or(0);
    if max_abs == 0 {
        return vec![0i16; acc.len()];
    }
    let max_abs = i128::from(max_abs);
    acc.iter()
        .map(|x| {
            // 先乘后除，避免整数除法把小数全部抹掉；i128 防溢出。
            let scaled = (i128::from(*x) * 32_767) / max_abs;
            i16::try_from(scaled.clamp(-32_767, 32_767)).unwrap_or(0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples() -> Vec<(Vec<String>, String, u32)> {
        vec![
            (vec!["今天".into()], "天气".into(), 5),
            (vec!["明天".into()], "天气".into(), 4),
            (vec!["农田".into()], "田七".into(), 6),
            (vec!["电脑".into()], "屏幕".into(), 5),
        ]
    }

    #[test]
    fn training_is_deterministic() {
        let a = VectorMemory::train(samples(), VectorConfig::default()).unwrap();
        let b = VectorMemory::train(samples(), VectorConfig::default()).unwrap();
        assert_eq!(a.v, b.v, "同一批历史必须给出逐位相同的向量");
        // 今天 / 天气 / 明天 / 农田 / 田七 / 电脑 / 屏幕
        assert_eq!(a.len(), 7);
    }

    #[test]
    fn empty_history_yields_no_model() {
        assert!(VectorMemory::train(Vec::new(), VectorConfig::default()).is_none());
        // 次数为 0 的样本不算历史。
        let zero = vec![(vec!["今天".into()], "天气".into(), 0)];
        assert!(VectorMemory::train(zero, VectorConfig::default()).is_none());
    }

    #[test]
    fn the_context_word_that_actually_preceded_the_word_scores_highest() {
        // **这个 crate 存在的理由**：`天气` 与 `田七` 是同一个编码下的两个词，
        // P4a 只看次数（田七 6 > 天气 5），看不出上文是「今天」还是「农田」。
        // 这里断言向量看得懂：
        let m = VectorMemory::train(samples(), VectorConfig::default()).unwrap();
        let tianqi = vec!["今天".to_string()];
        let nongtian = vec!["农田".to_string()];
        assert!(
            m.score(&tianqi, "天气") > m.score(&tianqi, "田七"),
            "上文是「今天」时，`天气` 该高于 `田七`"
        );
        assert!(
            m.score(&nongtian, "田七") > m.score(&nongtian, "天气"),
            "上文是「农田」时，`田七` 该高于 `天气`"
        );
    }

    #[test]
    fn an_unknown_word_scores_zero() {
        let m = VectorMemory::train(samples(), VectorConfig::default()).unwrap();
        assert_eq!(m.score(&["今天".to_string()], "没见过的词"), 0);
        // 词表外的上下文词不贡献任何分量 ⇒ 分数为 0。
        assert_eq!(m.score(&["没见过的上下文".to_string()], "天气"), 0);
    }

    #[test]
    fn context_window_is_bounded() {
        let m = VectorMemory::train(samples(), VectorConfig::default()).unwrap();
        // window = 2：更早的词不该影响分数。
        let long = vec!["很旧".to_string(), "今天".to_string()];
        let short = vec!["今天".to_string()];
        assert_eq!(
            m.score(&long, "天气"),
            m.score(&short, "天气"),
            "超出窗口的上下文词不该参与打分"
        );
    }

    #[test]
    fn memory_is_bounded_by_vocab_times_dim() {
        let cfg = VectorConfig { dim: 16, window: 2 };
        let m = VectorMemory::train(samples(), cfg).unwrap();
        // 一张表 × 词表 × 维数 × 2 字节。
        assert_eq!(m.bytes(), m.len() * 16 * 2);
        assert_eq!(m.dim(), 16);
    }

    #[test]
    fn a_huge_configured_dim_is_clamped_not_fatal() {
        // 配置错误不该锁死输入法（D26）：维数被钳到上限，而不是 panic。
        let m = VectorMemory::train(
            samples(),
            VectorConfig {
                dim: 4096,
                window: 2,
            },
        )
        .unwrap();
        assert_eq!(m.dim(), MAX_DIM);
        assert!(m.bytes() <= m.len() * MAX_DIM * 2);
    }
}
