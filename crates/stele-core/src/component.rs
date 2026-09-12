//! # Component traits
//!
//! 中文职责：五类骨架组件 + 兜底翻译器的 trait，以及传给它们的只读上下文。
//! English role: the five skeleton component traits (plus the fallback translator)
//! and the read-only context passed to them.
//! 架构位置：stele-core 的扩展点定义；具体实现由 `stele-engine` 提供。
//!
//! # 分类是刻意的，但**不是公理**（G8）
//!
//! RIME 2009 年的设计文档**只规划了三类**（处理器 / 切分器 / 翻译器），
//! **Filter 是后来加的**，Formatter 更晚。所以这里的五类是**经验集合**，
//! 长过一次，**可能还会长**。增加第六类需要改内核——这是诚实承认的边界
//! （`docs/engine-design.md` §2.5）。
//!
//! # tag 是绑定层（G3）
//!
//! 切分器给分段打标签，翻译器/滤镜据此声明自己管哪一类。
//! **一个分段可带多个标签。** 这正是 RIME 让一个方案里能同时挂
//! "拼音 / 英文 / 自定义短语 / 拆字反查"四个翻译器的机制——
//! 它们不靠位置区分，靠 tag 区分。

use crate::candidate::{Candidate, CandidateSink, Lane, Origin, Span, SpellingAttr};
use crate::commit::ProcessResult;
use crate::context::Context;
use crate::key::Key;
use crate::option::Options;
use crate::score::Score;
use crate::segment::{Composition, Segmentation, Tag};
use crate::service::{Lexicon, Spelling};
use crate::session::SessionState;

/// 传给组件的只读上下文。
///
/// **只放"每次查询都不同"的数据，不放服务。**
/// 词库、拼写层、记忆、时钟等服务在**组件构造时注入**（见下）。
pub struct Query<'a> {
    /// 当前输入串。
    pub input: &'a str,
    /// 光标位置（字节偏移）。
    pub caret: usize,
    /// 当前开关状态。
    pub options: &'a Options,
    /// 最近已上屏的词（最新的在末尾）。下一词预测完全依赖它。
    pub context: &'a Context,
    /// 当前会话的完整只读状态。
    pub composition: &'a Composition,
}

// ─────────────────────────────────────────────────────────────────────────────
// 服务注入，而不是穿过 Query
// ─────────────────────────────────────────────────────────────────────────────
//
// DSH 的架构文档给了一个比 RIME 更清晰的模型，我们按输入法的需要裁剪后采用：
//
//   「一个 **seam** 是可替换的能力，有三个角色：**服务定义**声明接口、
//     **服务提供者**实现它、**消费者**使用它。……一个角色不成其为 seam；
//     增加一个能力意味着把三个角色都设计出来。」—— dshsrc/docs/architecture.md
//
// 对应到我们：
//   - 服务定义 = 本 crate 里的 trait（`Lexicon` / `Spelling` / `Ranker` / …）
//   - 服务提供者 = 零件包里的实现
//   - 消费者 = 组件（翻译器用 `Lexicon`，重排管线用 `Ranker`）
//
// **规则：服务在组件构造时注入，不穿过 `Query`。**
//
// 为什么必须这样：
//   1. **多词典方案**：rime-ice 一个方案挂了 4 个以上词库。
//      `Query` 里只有一个 `lexicon` 字段的写法**表达不了**。
//   2. **可替换性**：换一个词库实现（内存 → mmap）只需在装配处换一行。
//   3. **显式失败**：装配时找不到方案要求的服务，**立刻报错**。
//   4. **可裁剪**：不装配某个服务，对应组件就不存在（服务内存红线）。
//   5. **注册即"可撤销的效果"**：切换方案时上一个方案注册的组件不会泄漏。

/// 处理器：决定"这一下按键算不算输入"。
pub trait Processor {
    /// 处理一个按键。可以修改会话状态（输入串、光标、开关）。
    fn process(&mut self, state: &mut SessionState, key: &Key) -> ProcessResult;

    /// 本处理器是否启用。方案可以用开关控制（例如"预测"开关）。
    fn enabled(&self, _options: &Options) -> bool {
        true
    }
}

/// 切分器：把输入串切成若干段，并给每段打标签。
pub trait Segmentor {
    /// 从当前位置继续切分。
    ///
    /// 返回 `false` 表示"这一回合到此结束，后面的切分器不必再看"。
    /// 这对应 RIME 的"优先级较高的 Segmentor 可以中止当前回合"。
    fn proceed(&self, q: &Query<'_>, seg: &mut Segmentation) -> bool;

    /// 本切分器产出的标签。
    fn tags(&self) -> &[Tag];
}

/// 翻译器：为一段输入产出候选。
pub trait Translator {
    /// 候选写进 `sink`，由 sink 负责限流。
    fn translate(&self, q: &Query<'_>, span: Span, out: &mut CandidateSink<'_>);

    /// 本翻译器是否负责这个分段。
    ///
    /// **tag 是"切分器 → 翻译器"的绑定层**：切分器给分段打标签
    /// （`abc`、`punct`、`radical_lookup`…），翻译器据此声明自己管哪一类。
    /// 默认全部接收。
    fn accepts(&self, _tags: &[Tag]) -> bool {
        true
    }
}

/// 滤镜：对候选列表做后处理（去重、纠错、简繁、标点、置顶）。
pub trait Filter {
    /// 就地改写候选列表：可以修改、丢弃、插入、重排。
    fn apply(&self, q: &Query<'_>, span: Span, cands: &mut Vec<Candidate>);

    /// 默认对所有段生效；实现可限定只对某些 tag 生效。
    fn applies_to(&self, _tags: &[Tag]) -> bool {
        true
    }
}

/// 格式化器：对**显示字符串**做重写（`preedit` / `comment` 的格式）。
///
/// 与 [`Filter`] 的区别：`Filter` 改的是**候选列表**，`Formatter` 改的是
/// **一段文字怎么显示**。两者作用对象不同，故是两个类别。
///
/// **这是早期设计漏掉的一个类别。** RIME 有它（`formatter.h`，用于
/// `preedit_format` / `comment_format`）。
pub trait Formatter {
    /// 就地把 `text` 改写成要显示的样子。
    fn format(&self, q: &Query<'_>, text: &mut String);
}

/// 兜底翻译器：**保证"你敲的东西永远能上屏"**（G4）。
///
/// RIME 用 `echo_translator` + `fallback_segmentor` 做到这件事。这不是可选功能，
/// 而是输入法的一条**基本安全感**：查不到词时不能什么都不给，否则用户会
/// 以为输入法坏了、或者卡在一个无法退出的输入状态里。
///
/// 实现上它通常是流水线里**最后**一个翻译器，产出一个 [`Origin::Literal`]
/// 的候选（文本就是原始输入），分数取 [`Score::FLOOR`] 之上一个很小的值——
/// **分低，但一定在**。
pub trait Fallback {
    /// 产出兜底候选。
    fn fallback(&self, q: &Query<'_>, span: Span, out: &mut CandidateSink<'_>);

    /// 本兜底器适用于哪些标签。
    fn accepts(&self, _tags: &[Tag]) -> bool {
        true
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 装配期的服务绑定（供 stele-engine 使用）
// ─────────────────────────────────────────────────────────────────────────────

/// 一个翻译器在装配后拿到的服务。
///
/// 注意 `lexicon` 与 `spelling` 是**两个独立可替换的东西**（G2）：
/// 双拼方案的经典做法是"**用全拼的词库，只换拼写规则**"。
pub struct TranslatorDeps {
    /// 词库：编码序列 → 词条。一个方案可以挂多个。
    pub lexicon: std::sync::Arc<dyn Lexicon>,
    /// 拼写层：拼写 → 编码（含简拼/模糊音等变体）。与词库独立替换。
    pub spelling: std::sync::Arc<dyn Spelling>,
}

/// 原样上屏候选的构造助手：`Origin::Literal` + 规范属性 + 最低分。
///
/// 兜底路径（[`Fallback`]）应该用它产出候选——**分低，但一定在**。
#[must_use]
pub fn literal_candidate(text: impl Into<String>, span: Span) -> Candidate {
    Candidate {
        text: text.into(),
        comment: None,
        score: Score::FLOOR,
        origin: Origin::Literal,
        attr: SpellingAttr::NORMAL,
        span,
        lane: Lane::Input,
    }
}
