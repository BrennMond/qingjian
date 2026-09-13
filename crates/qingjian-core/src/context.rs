//! # Context
//!
//! 中文职责：已上屏内容的滚动窗口。下一词预测的唯一输入来源。
//! English role: a rolling window of recently committed text — the only input
//! source for next-word prediction.
//! 架构位置：qingjian-core 的会话状态之一，随 `Query` 传给组件。
//!
//! **这是一个早期设计的遗漏**：初稿的 `Query` 里没有任何地方能拿到
//! "刚才打了什么"，因此下一词预测在数据模型上根本无法实现。
//! RIME 对应的是 `commit_history.cc`。

/// 已上屏内容的滚动窗口。**最新的在末尾**。
#[derive(Clone, Debug)]
pub struct Context {
    /// 最近上屏的词。容量由方案配置（例如只保留最近 8 个），避免无界增长。
    recent: Vec<String>,
    /// 容量上限。
    cap: usize,
}

impl Default for Context {
    /// 默认容量 1。
    ///
    /// # 为什么不能是 `derive(Default)`
    ///
    /// 派生出来的 `cap` 是 **0**，而 [`Context::push`] 在 `cap == 0` 时会
    /// 对一个**空的 `Vec`** 调 `remove(0)` —— **直接 panic**。
    /// 这条路径此前没被走到：真实会话用 `with_capacity(8)`，
    /// 而用到 `default()` 的地方（`Query` 的常量视图、测试）
    /// 恰好从不 `push`。P4b 写预测测试时按了第一次，它就炸了。
    ///
    /// **教训与 P3 那批一样**：一个"永远不会被走到"的分支不是安全的，
    /// 它只是**还没被走到**。让默认值自洽，比依赖调用方记得传容量便宜。
    fn default() -> Self {
        Self::with_capacity(1)
    }
}

impl Context {
    /// 以给定容量构造（容量 0 会被提升为 1，避免退化成"永远为空"）。
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            recent: Vec::new(),
            cap: cap.max(1),
        }
    }

    /// 记录一次上屏。
    ///
    /// **注意**：预测候选（`Lane::Predict`）上屏后**同样要记入**——
    /// 否则"连续预测"就断了。
    pub fn push(&mut self, text: impl Into<String>) {
        let text = text.into();
        if text.is_empty() {
            return;
        }
        if self.recent.len() >= self.cap {
            self.recent.remove(0);
        }
        self.recent.push(text);
    }

    /// 最近的词，最新的在末尾。
    #[must_use]
    pub fn recent(&self) -> &[String] {
        &self.recent
    }

    /// 最后一个词（即"上一个词"）。预测最常用。
    #[must_use]
    pub fn last(&self) -> Option<&str> {
        self.recent.last().map(String::as_str)
    }

    /// 清空。
    pub fn clear(&mut self) {
        self.recent.clear();
    }

    /// 容量上限。
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.cap
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_the_last_n() {
        let mut c = Context::with_capacity(3);
        for w in ["我", "要", "打", "字"] {
            c.push(w);
        }
        assert_eq!(c.recent(), &["要", "打", "字"]);
        assert_eq!(c.last(), Some("字"));
    }

    #[test]
    fn ignores_empty_commits() {
        let mut c = Context::with_capacity(2);
        c.push("");
        assert!(c.recent().is_empty());
    }

    #[test]
    fn capacity_zero_is_promoted_to_one() {
        let c = Context::with_capacity(0);
        assert_eq!(c.capacity(), 1);
    }

    #[test]
    fn default_context_can_accept_a_push() {
        // 这条测试是从一个真实的 panic 里长出来的：`derive(Default)` 给出
        // `cap = 0`，而 `push` 在 cap 为 0 时会对空 `Vec` 调 `remove(0)`。
        // 症状是"写预测测试时莫名其妙地炸在 Context 里"。
        let mut c = Context::default();
        c.push("甲");
        assert_eq!(c.last(), Some("甲"));
        assert_eq!(c.recent(), &["甲"]);
    }
}
