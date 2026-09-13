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
use crate::segment::{Segmentation, Tag};
use crate::service::{Lexicon, Spelling};
use crate::session::SessionState;

/// 传给组件的只读上下文。
///
/// **只放"每次查询都不同"的数据，不放服务。**
/// 词库、拼写层、记忆、时钟等服务在**组件构造时注入**（见下）。
///
/// # 为什么这里**没有** `composition`
///
/// 早先它有一个 `composition: &Composition` 字段，而流水线为了构造它
/// **每次按键都克隆一遍 Options / Context / Composition**（借用检查器
/// 不允许同时可变借用 `SessionState` 与它的字段）。
///
/// 实测代价：按键路径 P50 从 **301 ns 涨到 1.55 µs**——
/// 五次克隆换来的却是**零个使用者**（全项目没有任何组件读它）。
/// 删掉之后三个克隆全部消失。
///
/// 组件若确实需要更多会话状态，正确的做法是**显式加一个字段**
/// （像 `segment_text` 那样），而不是把整个 `Composition` 搬过来——
/// 后者会让"谁读了什么"变成不可回答的问题。
pub struct Query<'a> {
    /// 当前输入串。
    pub input: &'a str,
    /// 光标位置（字节偏移）。
    pub caret: usize,
    /// 当前开关状态。
    pub options: &'a Options,
    /// 最近已上屏的词（最新的在末尾）。下一词预测完全依赖它。
    pub context: &'a Context,
    /// **本次翻译该看的那一段文本**。
    ///
    /// 绝大多数时候它就是 [`Query::input`] 本身。但**带词缀的分段**
    /// （RIME 的 `affix_segmentor`）会让它变短：用户敲 `uUni` 时，
    /// 前缀 `uU` 只是"告诉引擎接下来是什么"，真正要翻译的是 `ni`。
    ///
    /// 为什么不直接改 `input`：`span` / `caret` / 预编辑串都以
    /// **原始输入**为坐标。改 input 会让这三样全部错位——
    /// 而错位的症状是"候选对但高亮位置不对"，极难排查。
    pub segment_text: &'a str,
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
///
/// # 为什么要求 `Send`
///
/// 流水线整体是 `Send` 的（会话要能被搬到别的线程——Windows TSF 组件跑在
/// 别人的进程里，前端也有工作线程）。组件被流水线**按值拥有**，故必须 `Send`。
///
/// **但不要求 `Sync`**：组件是每会话一份的（它们可以有内部状态，
/// 例如缓存与光标），因此不需要被多个线程共享。昂贵的资源
/// （词库、拼写表）另以 `Arc` 共享——见 [`crate::session::Pipeline`]。
pub trait Processor: Send {
    /// 处理一个按键。可以修改会话状态（输入串、光标、开关）。
    fn process(&mut self, state: &mut SessionState, key: &Key) -> ProcessResult;

    /// 这个处理器叫什么（方案里的零件名）。
    ///
    /// 与 [`Segmentor::name`] 同一个理由：装配完之后流水线里是一串
    /// `Box<dyn Processor>`，而"这一下按键到底被谁吃掉了"是**唯一**
    /// 能解释"某个键没反应"的信息。没有它时只能靠 `eprintln!` 加下标猜，
    /// 而**下标是会随方案变的**——我按固定下标读调试输出，读错了两次。
    fn name(&self) -> &'static str {
        "processor"
    }

    /// 本处理器是否启用。方案可以用开关控制（例如"预测"开关）。
    fn enabled(&self, _options: &Options) -> bool {
        true
    }
}

/// 切分器：把输入串切成若干段，并给每段打标签。
pub trait Segmentor: Send {
    /// 从当前位置继续切分。
    ///
    /// 返回 `false` 表示"这一回合到此结束，后面的切分器不必再看"。
    /// 这对应 RIME 的"优先级较高的 Segmentor 可以中止当前回合"。
    fn proceed(&self, q: &Query<'_>, seg: &mut Segmentation) -> bool;

    /// 本切分器产出的标签。
    fn tags(&self) -> &[Tag];

    /// **告诉切分器：输入变了，这是一份新的扫描结果。**
    ///
    /// # 为什么它必须在 trait 上（这是一个真 bug 的教训）
    ///
    /// 识别（`recognizer` 扫出"哪一段属于哪个标签"）与切分（切分器据此
    /// 分段）是两步。流水线**每次 `compose` 都会重算扫描结果**——
    /// 但它一开始没有把这个结果告诉切分器，于是切分器一直拿着
    /// **构造时那份针对空输入的扫描结果**，`matcher` 永远找不到认领，
    /// 整条"识别 → 切分 → 绑定"的链静默断掉：
    ///
    /// - 没有报错，
    /// - 单元测试全绿（它们各自只测自己那一半），
    /// - 症状只是"前缀模式不生效"。
    ///
    /// 端到端测试（`the_symbol_table_expands_under_its_prefix`）抓到了它。
    /// **教训：跨组件的"每次都要同步"的状态，必须是一条显式的调用，
    /// 而不是两个组件各自以为对方知道。**
    ///
    /// 默认实现是空操作：不需要扫描结果的切分器（`abc_segmentor`）
    /// 不必实现它。
    fn rescan(&mut self, scan: &crate::segmentor::InputScanView<'_>) {
        let _ = scan;
    }

    /// **这一段的正文从哪开始**（带词缀的切分器才需要实现）。
    ///
    /// 返回 `Some((标签, 正文起点的字节偏移))` 表示"我切出来的这一段，
    /// 标签 `T` 对应的翻译器不该看到词缀"。例如 `uUni` 被切成
    /// `(标签 chaizi, 正文从 2 开始)`，于是 `chaizi` 的翻译器只看到 `ni`。
    ///
    /// # 为什么必须由切分器说，而不是翻译器自己猜
    ///
    /// "前缀有几个字符"这件事**只有切分器知道**（它读的是方案里的
    /// `prefix`）。翻译器若自己去剥，就得把 `affix_segmentor` 的配置
    /// 再读一遍——于是同一个配置有两个解释点，改一处漏一处。
    ///
    /// 这条链断过一次：切分器算出了正文起点却没人拿，
    /// 于是反查翻译器拿到的是 `uUni` 而不是 `ni`——**候选一个都不出，
    /// 而预编辑串看起来完全正常**。端到端测试抓到了它。
    ///
    /// 默认实现返回 `None`：不涉及词缀的切分器不必实现它。
    fn body_start(&self) -> Option<(Tag, usize)> {
        None
    }

    /// 这个切分器叫什么（方案里的零件名）。
    ///
    /// **它只用于诊断与 `--dump-config`**：装配完之后，流水线里是一串
    /// `Box<dyn Segmentor>`，而"到底装进去了哪几个、按什么顺序"
    /// 是一个使用者有权知道、且出错时唯一能查的东西。
    /// 没有这个方法时，那个问题只能靠 `eprintln!` 调试（我真这么干过）。
    fn name(&self) -> &'static str {
        "segmentor"
    }
}

/// 翻译器：为一段输入产出候选。
pub trait Translator: Send {
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

    /// 本翻译器**声明负责**的标签。
    ///
    /// 与 [`Translator::accepts`] 的分工：`accepts` 回答"这一段归我吗"
    /// （运行期、每个分段问一次），本方法回答"你管哪些标签"（装配期、问一次）。
    ///
    /// **为什么两个都要**：流水线在**装配期**就要知道
    /// "输入里出现了哪些标签"，才能决定这次要跑哪些翻译器；而运行期
    /// 还要再按分段确认一遍。返回空切片 = 不声明（一律当作"全部接收"）。
    fn targets(&self) -> &[Tag] {
        &[]
    }
}

/// 滤镜：对候选列表做后处理（去重、纠错、简繁、标点、置顶）。
pub trait Filter: Send {
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
pub trait Formatter: Send {
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
pub trait Fallback: Send {
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
