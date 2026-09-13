//! # Engine / Session
//!
//! 中文职责：引擎与会话的两级拆分。
//! English role: the two-level split between the engine and a session.
//! 架构位置：stele-core 的顶层对象模型，被 `stele-ffi` 与各前端使用。
//!
//! # 为什么必须分两层
//!
//! 引擎里有两类性质完全相反的东西：
//!
//! | | 共享的 | 私有的 |
//! | --- | --- | --- |
//! | 例子 | 词库、方案配置、记忆服务 | 正在敲的这串字母、当前候选、光标位置 |
//! | 构造代价 | 昂贵（加载词库） | 廉价（几个 String） |
//! | 数量 | 进程内一份 | 每个客户端一份 |
//! | 可变性 | 只读 | 可变 |
//!
//! 把它们塞进同一个 `&mut` 对象，会导致**无法同时开两个输入会话**，
//! 也无法安全地多线程共享词库。

use crate::candidate::Candidate;
use crate::commit::{Commit, Event, Outcome, ProcessResult, SelectionSource};
use crate::component::Query;
use crate::context::Context;
use crate::error::SchemaError;
use crate::key::Key;
use crate::option::Options;
use crate::segment::Composition;
use std::sync::Arc;

/// 一个方案的元数据。**列出方案不应触发装载**（D29）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SchemaInfo {
    /// 方案 id，例如 `stele-default`。
    pub schema_id: String,
    /// 显示名，例如「石经・全拼」。
    pub name: String,
    /// 版本串。
    pub version: String,
    /// 方案格式版本（PLAN D27 的两级版本门禁之一）。
    pub format_version: u32,
    /// **方案族**：同一族的方案共享用户词典。
    ///
    /// RIME 的好设计——全拼与双拼共享用户词库，且**以全拼形式记录**，
    /// 于是用户换双拼不丢学过的词。照抄这条。
    pub family: Option<String>,
}

/// 一个已装载的方案。
pub trait LoadedSchema: Send + Sync {
    /// 元数据。
    fn info(&self) -> &SchemaInfo;

    /// 该方案的开关集合（含方案声明的全部开关）。
    fn options(&self) -> &Options;

    /// **为一个会话装配组件流水线。**
    ///
    /// # 为什么是"每会话一份"而不是共享
    ///
    /// 组件可以有内部状态（缓存、光标、上次处理结果），因此需要 `&mut self`。
    /// 若把流水线共享给所有会话，就必须给每个组件加锁——那是自找的麻烦。
    ///
    /// **代价是可控的**：组件本身很轻，**昂贵的资源（词库、拼写表）通过
    /// `Arc` 共享**，每个会话只是重新装配一遍引用。RIME 也是这个模型
    /// （每个 Session 拥有一份 Engine）。
    fn build_pipeline(&self) -> Box<dyn Pipeline + Send>;
}

/// 装配好的组件流水线。
///
/// 它是 `stele-engine` 与 `stele-core` 之间的**唯一接缝**：内核定义了
/// "一次按键 → 一组候选"的形状，具体由哪些组件、按什么顺序完成，
/// 完全由方案数据决定（PLAN D17 / D24）。
pub trait Pipeline: Send {
    /// 送一个按键；可以修改会话状态。
    fn process_key(&mut self, state: &mut SessionState, key: &Key) -> ProcessResult;

    /// 由当前状态产出候选（含切分、翻译、过滤、重排）。
    ///
    /// 允许修改 `state` 以写回预编辑串与切分信息。
    fn compose(&mut self, state: &mut SessionState, out: &mut Vec<Candidate>);

    /// 收尾：排序与"精确优先"守卫。
    ///
    /// 默认实现已经正确，**实现者通常不需要覆盖它**。
    fn finalize(&self, cands: &mut Vec<Candidate>) {
        crate::sort::sort_candidates(cands);
    }
}

/// 方案目录：应用级，持有已装载的方案，按内存预算惰性装载与淘汰（D29）。
///
/// **四条约束**
///
/// 1. **懒装载**：只装载用到的方案，不预载全部。
/// 2. **内存预算 + LRU 淘汰**：预算是配置项；**淘汰必须真的释放**
///    （靠"注册是可撤销的效果"，`docs/engine-design.md` §6.3）。
/// 3. **切换失败要回退**：目标方案坏掉时，留在原方案并报诊断（D26）。
/// 4. **用户词库跨方案共享**（按 [`SchemaInfo::family`] 分族）。
pub trait SchemaCatalog: Send + Sync {
    /// 列出可用方案（只读元数据，**不触发装载**）。
    fn list(&self) -> &[SchemaInfo];

    /// 取用一个方案；未装载则装载；超预算则先淘汰最久未用的。
    ///
    /// 返回的 `Arc` 被丢弃时，若无人再持有，该方案的注册被撤销、内存随之释放。
    ///
    /// # Errors
    ///
    /// 方案不存在、配置有错、缺少必需零件、格式版本不支持时返回
    /// [`SchemaError`]。**调用方（CLI / 前端）的默认行为是降级 + 提示，
    /// 不是退出**（PLAN D26）。
    fn acquire(&self, schema_id: &str) -> Result<Arc<dyn LoadedSchema>, SchemaError>;
}

/// 引擎：**方案目录 + 共享服务**。构造昂贵，进程内共享一份。
///
/// `Send + Sync` 是对编译器的承诺："这个东西可以安全地被多个线程共享。"
///
/// **注意**：早期设计让 `Engine` 等同于"一个已加载的方案"，同时又给了它
/// `schema_id()`——这与 D29（会话内可切换方案）直接冲突。现在明确分两级：
///
/// ```text
/// Engine（应用级，一份）
///   ├── SchemaCatalog：已装载方案的集合（惰性装载 + LRU 淘汰）
///   ├── 共享服务：MemoryStore / Clock / …
///   └── create_session() → Session（每个客户端一份）
/// Session（客户端级）
///   └── 当前方案句柄 + 输入状态
/// ```
pub trait Engine: Send + Sync {
    /// 为一个客户端创建独立会话。多次调用互不影响。
    ///
    /// 返回 `Box<dyn Session + Send>` 而非 `Box<dyn Session>`：
    /// **不加 `Send` 的话，会话连"搬到另一个线程"都不允许**，
    /// 而 Windows TSF 组件（跑在别人的进程里）与前端的工作线程都需要搬。
    fn create_session(&self) -> Box<dyn Session + Send>;

    /// 方案目录：列出 / 取用 / 淘汰。
    fn schemas(&self) -> &dyn SchemaCatalog;
}

/// 会话：一个客户端的私有状态。
///
/// 这里**故意不写 `Send + Sync`**：Rust 会因此在编译期保证
/// "同一个会话不可能被两个线程同时修改"——这是 RIME 靠口头约定
/// （"一个 session 一个线程"）才能换来的东西，我们免费拿到。
pub trait Session {
    /// 送入一个按键，推进输入状态机。
    fn process_key(&mut self, key: Key) -> Outcome;

    /// 当前输入串与切分结果（只读）。
    fn composition(&self) -> &Composition;

    /// 当前候选列表，**已跨段合并、跨通道排好序**——前端照着画就行。
    ///
    /// 它由 [`Pipeline::compose`] + [`Pipeline::finalize`] 产出，
    /// 顺序满足"可复现"铁律（同一状态 + 同一输入 ⇒ 逐字节相同）。
    ///
    /// 前端**不要**去读 `Segment::candidates`：它没有跨段合并、
    /// 没有跨通道排序、也没有经过最终滤镜。
    fn candidates(&self) -> &[Candidate];

    /// 选中第 `index` 个候选。
    ///
    /// `index` 指向 [`Session::candidates`] 返回的那个"已渲染列表"的位置
    /// （两条通道统一编号）；`source` 区分键盘盲选与明确的点选——
    /// **预测候选默认只允许后者**。
    fn select(&mut self, index: usize, source: SelectionSource) -> Outcome;

    /// 显式上屏当前高亮候选（空格 / 回车）。
    fn commit(&mut self) -> Option<Commit>;

    /// 放弃当前输入。
    fn reset(&mut self);

    /// 按**名字**读取开关。
    ///
    /// 引擎不预设任何开关名（D20）——名字全部来自方案声明，
    /// 因此这里的 `name` 是纯粹的字符串，引擎只当它是键。
    fn option(&self, name: &str) -> bool;

    /// 按名字设置开关。返回 `false` 表示该开关未声明。
    fn set_option(&mut self, name: &str, on: bool) -> bool;

    /// 当前方案的 id。
    ///
    /// 前端需要它（状态栏、方案菜单、以及"我是谁"的诊断）。
    fn schema_id(&self) -> &str;

    /// 切换方案（D29）。**失败时必须保持原方案可用**（PLAN D26）。
    ///
    /// # Errors
    ///
    /// 目标方案不可用时返回 [`SchemaError`]；**此时会话仍留在原方案上**，
    /// 用户不会因为目标方案坏掉而打不了字。
    fn switch_schema(&mut self, schema_id: &str) -> Result<(), SchemaError>;

    /// 引擎侧异步发生的事件（学习通知等）。前端可以忽略。
    ///
    /// **用出参而不是返回 `Vec`**：这个方法在每次按键后都会被调用，
    /// 返回 `Vec` 意味着每次按键都分配（哪怕为空）。
    fn drain_events(&mut self, out: &mut Vec<Event>);

    /// **时钟推进（G13 的预留）**：当前时间到了 `now_ms`。
    ///
    /// 目前**没有任何组件需要它**——`process_key` 是唯一入口。
    /// 但并击输入（RIME 的 `chord_composer`）、长按、以及"单键按下超过
    /// N 毫秒就当作空格"这类判定**必须**有它。
    ///
    /// **现在就留这个方法**，因为事后往 `Session` 上加方法虽然是加法，
    /// 但**所有前端都要重新实现一遍事件循环**（它们得开始定时唤醒引擎）。
    /// 前端可以永远不调用它——默认实现是空操作。
    fn tick(&mut self, _now_ms: u64) {}
}

/// 处理器的可变工作区。
///
/// 它是会话内部状态的**受控视图**：处理器只能通过它改输入串、光标、开关，
/// 不能碰词库或别的东西。
#[derive(Clone, Debug, Default)]
pub struct SessionState {
    /// 输入串与切分状态。
    pub composition: Composition,
    /// 开关。
    pub options: Options,
    /// 已上屏内容的滚动窗口（供预测与上下文重排使用）。
    pub context: Context,
    /// 处理器请求的上屏；由**会话**在按键处理结束后兑现成
    /// [`crate::Commit`]（见 [`crate::PendingCommit`]）。
    pub pending_commit: Option<crate::commit::PendingCommit>,
    /// 处理器**改动了开关**时记在这里，由会话转成 [`crate::Event::OptionChanged`]。
    ///
    /// 为什么不让处理器直接发事件：处理器拿不到事件队列（那是会话的东西），
    /// 而"中英切换"这类动作**必须**让前端知道（状态栏要变）。
    /// 用 `Vec` 而不是单个值：一次按键可能切好几个开关，而**丢弃**其中
    /// 任何一个都会让状态栏与实际状态不一致。
    pub option_events: Vec<(String, bool)>,
    /// 处理器要求"换成这些按键**再派发一遍**"（`key_binder` 的 `send`）。
    ///
    /// 由流水线在**同一次按键内**取走并重新派发。用 `Vec` 而不是单个值：
    /// 一条链上可能连续换两次键（`Shift+space` → `space` → …）。
    ///
    /// **不会无限循环**：流水线只重派发有限轮（见 `REBIND_ROUNDS`），
    /// 且被重派发的按键不再经过绑定它的那个处理器。
    pub sent_keys: Vec<Key>,
    /// 候选总数（由流水线在 `compose` 后写回，供 `navigator` 与 `when: has_menu` 判断）。
    pub candidate_count: usize,
    /// 候选可以翻几页（由流水线写回）。
    pub candidate_pages: usize,
    /// 当前页（由 `navigator` 写回，流水线据此裁剪可见候选）。
    pub candidate_page: usize,
}

impl SessionState {
    /// 由这三样构造。
    #[must_use]
    pub fn new(composition: Composition, options: Options, context: Context) -> Self {
        Self {
            composition,
            options,
            context,
            pending_commit: None,
            option_events: Vec::new(),
            sent_keys: Vec::new(),
            candidate_count: 0,
            candidate_pages: 0,
            candidate_page: 0,
        }
    }

    /// 切换一个开关，并**记下这次改动**。
    ///
    /// 返回 `false` 表示该开关未声明（拼错名字不会被静默创建）。
    pub fn toggle_option(&mut self, name: &str) -> bool {
        if !self.options.toggle(name) {
            return false;
        }
        let on = self.options.get(name);
        self.option_events.push((name.to_owned(), on));
        true
    }

    /// 设置一个开关，并**记下这次改动**。
    pub fn set_option(&mut self, name: &str, on: bool) -> bool {
        if !self.options.set(name, on) {
            return false;
        }
        self.option_events.push((name.to_owned(), on));
        true
    }

    /// 构造一个只读查询视图。
    #[must_use]
    pub fn query(&self) -> Query<'_> {
        Query {
            input: &self.composition.input,
            caret: self.composition.caret,
            options: &self.options,
            context: &self.context,
            composition: &self.composition,
            segment_text: &self.composition.input,
        }
    }
}
