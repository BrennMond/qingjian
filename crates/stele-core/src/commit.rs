//! # Commit / Outcome
//!
//! 中文职责：上屏记录与按键处理结果。
//! English role: the commit record and the per-key outcome.
//! 架构位置：stele-core 的对外结果类型，`Session::process_key` 返回它。
//!
//! # 为什么不用 RIME 的写法
//!
//! RIME 是 `bool`（消费了没）+ 单独调用 `get_commit()`（**读取即清空**）。
//! 两个危害：**漏读一次 = 文字永久丢失**；且布尔值不说"这一下上屏了什么"。
//! 我们两端都是自己的代码，没有理由继承它——**上屏信息由返回值带出**。

use crate::candidate::{Lane, Origin, SpellingAttr};

/// 上屏的触发方式。
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Trigger {
    /// 空格确认。
    Space,
    /// 回车确认。
    Enter,
    /// 标点自动上屏。
    Punctuation,
    /// 引擎自动上屏（如候选唯一）。
    AutoCommit,
    /// 显式选词。
    Explicit,
    /// 输入被放弃时部分上屏（如按 Esc 后把原始输入打出去）。
    Fallback,
}

/// 一次上屏的完整记录。
///
/// `input` 与 `origin`/`attr` 是用户记忆能否实现的前提：
/// 学习需要的三元组是 **(原始输入, 上屏文本, 来源)**。
/// 旧设计的 `commit() -> Option<String>` 丢掉了其中两项。
#[derive(Clone, Debug)]
pub struct Commit {
    /// 上屏的文本。
    pub text: String,
    /// 触发它的原始输入。
    ///
    /// **`Lane::Predict` 的候选上屏时这里是空字符串**（预测没有对应的当前输入），
    /// 学习时必须改用 `context` 作为键（G10 / §4.3.1）。
    pub input: String,
    /// 上屏时的上下文（最近已上屏的词，最新的在末尾）。
    pub context: Vec<String>,
    /// 候选从哪来。
    pub origin: Origin,
    /// **学习时必须用它把 `input` 规范化成规范编码**（G10）。
    pub attr: SpellingAttr,
    /// 所属通道。
    pub lane: Lane,
    /// 触发方式。
    pub trigger: Trigger,
}

/// 选中的来源——用于区分"键盘盲选"与"明确点选"。
///
/// 预测候选（`Lane::Predict`）**默认只允许后者**：用户会对
/// `Lane::Input` 形成肌肉记忆（"敲 nihao 然后按 1"），
/// 若预测候选也能被数字键选中且位置会变，这套肌肉记忆就崩了。
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum SelectionSource {
    /// 键盘数字键 / 空格 —— 盲选，受肌肉记忆约束。
    Keyboard,
    /// 鼠标点击 / 触摸 —— 明确意图，可以选中预测候选。
    Pointer,
}

/// `process_key` / `select` 的返回值。
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum Outcome {
    /// 这个键我不处理，请前端还给操作系统。
    Rejected,
    /// 已处理，前端应重新读取状态并重绘。
    Consumed,
    /// 已处理并且上屏了。**上屏信息是返回值带出来的，前端不可能漏读。**
    Committed(Commit),
}

/// 处理器**请求上屏**时填写的东西。
///
/// # 为什么处理器不直接产出 `Commit`
///
/// 只有**会话**才同时知道"已渲染的候选列表"和"当前输入"，而 `Commit`
/// 需要这两者（`text` 来自候选，`input`/`attr`/`origin` 来自候选，
/// `context` 来自会话历史）。处理器只表达**意图**（"选第 3 个"），
/// 由会话兑现成完整的 [`Commit`]。
///
/// 这也让"[`crate::SelectionSource`] 约束预测候选不被盲选"这条规则
/// 有一个统一的执行点。
///
/// # 两种意图（G15）
///
/// 上屏有两种来源，而它们**不是同一件事**：
///
/// | 变体 | 文本从哪来 | 谁需要它 |
/// | --- | --- | --- |
/// | [`Self::Select`] | 已渲染候选列表的第 `index` 项 | 选词键（空格 / 数字） |
/// | [`Self::Literal`] | 处理器**自己带的**文本 | 标点直出、按键重绑定的"发送" |
///
/// 标点处理器不可能通过"选中第几个候选"来表达自己——它的文本**不在候选里**，
/// 而且它上屏时**输入串根本还没被翻译**（RIME 的行为是标点立刻上屏、
/// 不打断正在输入的编码）。把两者塞进一个结构体只会让"候选下标"这个字段
/// 在两个变体里有不同含义。
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingCommit {
    /// 选中已渲染候选列表中的某一项。
    Select {
        /// 全局序号（从 0 开始）。
        index: usize,
        /// 触发方式。
        trigger: Trigger,
        /// 选择的来源（键盘盲选 / 明确点选）。
        source: SelectionSource,
    },
    /// **直接上屏一段文本**，不看候选列表。
    Literal {
        /// 要上屏的文本。
        text: String,
        /// 触发方式。
        trigger: Trigger,
    },
    /// **上屏当前候选的注释**（RIME 的 `commit_comment`）。
    ///
    /// 这是第三种意图，因为它的文本既不在候选列表的 `text` 里、
    /// 也不是处理器自己带的——它在**当前候选的 `comment` 字段**里。
    /// 用前两种都表达不了：`Select` 上屏 `text`，`Literal` 上屏固定文本。
    CommitComment {
        /// 上屏后是否清空输入（librime 的 `commit_comment` 会 `Clear()`）。
        clear: bool,
    },
    /// **从记忆里删掉当前候选**（RIME 的 `delete_candidate`，学习型删除）。
    ///
    /// 引擎在这里只能**表达意图**：真正"从用户词典里删掉"是 P4a
    /// （`MemoryStore::forget`）的事。这条意图让那个能力有一条通路，
    /// 而不必等到 P4a 再改接口。
    DeleteCandidate {
        /// 被删候选在**已渲染列表**里的下标。
        index: usize,
    },
}

impl PendingCommit {
    /// 键盘盲选第 `index` 个。
    #[must_use]
    pub const fn keyboard(index: usize, trigger: Trigger) -> Self {
        Self::Select {
            index,
            trigger,
            source: SelectionSource::Keyboard,
        }
    }

    /// 直接上屏一段文本（标点、按键重绑定的"发送"）。
    ///
    /// 这样上屏的候选**不是猜的**：它的来源是 [`Origin::Literal`]——
    /// "输入本身就是答案"。因此它不该被"精确优先"守卫当成猜测候选。
    #[must_use]
    pub fn literal(text: impl Into<String>, trigger: Trigger) -> Self {
        Self::Literal {
            text: text.into(),
            trigger,
        }
    }

    /// 上屏当前候选的注释。
    #[must_use]
    pub const fn commit_comment() -> Self {
        Self::CommitComment { clear: true }
    }

    /// 删除第 `index` 个候选的学习记录。
    #[must_use]
    pub const fn delete_candidate(index: usize) -> Self {
        Self::DeleteCandidate { index }
    }

    /// 触发方式。
    #[must_use]
    pub const fn trigger(&self) -> Trigger {
        match self {
            Self::Select { trigger, .. } | Self::Literal { trigger, .. } => *trigger,
            // 注释上屏与"删候选"都不产生上屏记录，给一个最接近的触发方式。
            Self::CommitComment { .. } | Self::DeleteCandidate { .. } => Trigger::Explicit,
        }
    }
}

/// 处理器之间的三态结果。
///
/// 与 [`Outcome`] 的区别：`Outcome` 是**会话对外**的结果（两态 + 上屏信息），
/// 而 `ProcessResult` 是**处理器之间**的协商结果。三态在内部是有意义的：
/// "还给系统"和"我不管、后面有人管"是不同的事。
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum ProcessResult {
    /// 我不管，请走系统默认处理（例如 `Ctrl+C`）。
    Rejected,
    /// 我不管，但后面的处理器可能管。
    Noop,
    /// 我处理了。
    Accepted,
}

/// 引擎侧异步事件。前端可以忽略。
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum Event {
    /// 记录一次学习（用户记忆使用）。
    Learned {
        /// 原始输入（**未规范化**；接收方须按 `attr` 换算，见 G10）。
        input: String,
        /// 上屏文本。
        text: String,
        /// 候选来源。
        origin: Origin,
        /// **这条编码是怎么拼出来的。**
        ///
        /// 接收方靠它决定要不要把 `input` 规范化成规范编码——
        /// 少了这个字段，G10 那条规则**无法实现**。
        attr: SpellingAttr,
        /// 上屏时的通道。
        lane: Lane,
    },
    /// **用户要求删除这个候选的学习记录**（RIME 的 `delete_candidate`）。
    ///
    /// 前端可以忽略它；P4a 的 `MemoryStore::forget` 会消费它。
    /// 现在就发出这条事件的理由与 `Session::tick` 相同：**事后加事件
    /// 意味着所有前端都要改一遍**，而现在加只是多一个 `match` 分支。
    ForgetRequested {
        /// 当时的输入（**未规范化**，与 `Learned` 同一条约定）。
        input: String,
        /// 要求删除的候选文本。
        text: String,
    },
    /// 开关被引擎改动（例如自动切换到英文模式）。
    OptionChanged {
        /// 开关名。
        name: String,
        /// 新状态。
        on: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_commit() -> Commit {
        Commit {
            text: "你好".into(),
            input: "nihao".into(),
            context: vec![],
            origin: Origin::SystemWord,
            attr: SpellingAttr::NORMAL,
            lane: Lane::Input,
            trigger: Trigger::Space,
        }
    }

    #[test]
    fn commit_carries_everything_learning_needs() {
        let c = sample_commit();
        // 学习需要的三元组：原始输入、上屏文本、来源。
        assert!(!c.input.is_empty());
        assert!(!c.text.is_empty());
        assert_eq!(c.origin, Origin::SystemWord);
        assert_eq!(c.attr, SpellingAttr::NORMAL);
    }

    #[test]
    fn outcome_is_not_a_bool() {
        // 上屏信息是返回值带出来的：前端不可能像"读取即清空"那样漏读。
        let o = Outcome::Committed(sample_commit());
        match o {
            Outcome::Committed(c) => assert_eq!(c.text, "你好"),
            _ => panic!("应当带上屏信息"),
        }
    }

    #[test]
    fn pending_commit_expresses_two_different_intents() {
        // 选词：文本来自候选列表，因此必须带**来源**（预测候选不许盲选）。
        let sel = PendingCommit::keyboard(2, Trigger::Explicit);
        assert!(matches!(
            sel,
            PendingCommit::Select {
                index: 2,
                source: SelectionSource::Keyboard,
                ..
            }
        ));

        // 直出：文本不在候选里，因此**没有**下标，也就无所谓来源。
        let lit = PendingCommit::literal("，", Trigger::Punctuation);
        match &lit {
            PendingCommit::Literal { text, .. } => assert_eq!(text, "，"),
            other => panic!("应当是直出意图，实际 {other:?}"),
        }
        assert_eq!(lit.trigger(), Trigger::Punctuation);

        // 另外两种意图：上屏注释、删候选。它们的文本来源与前两种都不同，
        // 因此必须是独立的变体（见各变体的说明）。
        assert!(matches!(
            PendingCommit::commit_comment(),
            PendingCommit::CommitComment { clear: true }
        ));
        assert!(matches!(
            PendingCommit::delete_candidate(3),
            PendingCommit::DeleteCandidate { index: 3 }
        ));
    }
}
