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
    ///
    /// # 服务从参数进来（P4a）
    ///
    /// 签名里出现 [`crate::Services`] 是 `docs/engine-design.md` §4 那条规则的
    /// 落地方式：**服务在组件构造时注入，不穿过 `Query`**。
    /// 曾经这里没有这个参数，于是"用户记忆"无处可挂——那正是 P4a 之前
    /// `Ranker` 一直是空列表的原因。
    fn build_pipeline(&self, services: &crate::service::Services) -> Box<dyn Pipeline + Send>;
}

/// 装配好的组件流水线。
///
/// 它是 `stele-engine` 与 `stele-core` 之间的**唯一接缝**：内核定义了
/// "一次按键 → 一组候选"的形状，具体由哪些组件、按什么顺序完成，
/// 完全由方案数据决定（PLAN D17 / D24）。
pub trait Pipeline: Send {
    /// 送一个按键；可以修改会话状态。
    fn process_key(&mut self, state: &mut SessionState, key: &Key) -> ProcessResult;

    /// 由当前状态产出候选（切分、翻译、重排、**排序**、过滤），
    /// **并且必须按最终顺序排好**。
    ///
    /// 允许修改 `state` 以写回预编辑串与切分信息。
    ///
    /// # 为什么"排序"在这条契约里，而且必须在滤镜之前
    ///
    /// 早先这里分成 `compose` + `finalize` 两步，排序放在 `finalize` 里
    /// ——也就是**滤镜之后**。那让三个"重排型"滤镜（长词优先 / v 模式 /
    /// 置顶候选）**完全不生效**：它们费劲排好的顺序，被紧随其后的那次
    /// 排序原样抹掉（实测：声明 `long_word_filter` 的方案输出仍是纯粹的
    /// 权重序，见 HANDOFF §5 第 38 条）。
    ///
    /// 正确的顺序只有这一种：
    ///
    /// ```text
    /// 翻译 → 重排（只加分，要参与排序）→ 排序 → 滤镜（按位置表达意图）
    /// ```
    ///
    /// 滤镜里的"把长词提到第 4 位"是**按位置**描述意图的，因此它看到的
    /// 必须是最终顺序——排完就定了，**后面不能再排一次**。RIME 也是这个
    /// 语义（滤镜作用在已排好序的候选表上，列表顺序即最终顺序）。
    ///
    /// 因此 `finalize` 这个钩子被删掉了：留着它，下一个人就会想往里放
    /// "收尾排序"，而那正好会重新踩上同一个坑。
    fn compose(&mut self, state: &mut SessionState, out: &mut Vec<Candidate>);

    /// **本流水线实际装配了哪些零件**：`(处理器, 翻译器, 滤镜, 重排器)`。
    ///
    /// # 为什么它必须在 trait 上
    ///
    /// 因为"方案里**声明**了零件"与"流水线里**装进了**零件"是两件事，
    /// 而后者此前**没有任何地方可以问**。后果是真实的：阶段 A 的 10 个
    /// 内联零件有实现、有单元测试、注册表里标着"已实现"，而装配路径里
    /// 一次都没引用过——**没有任何一条测试或工具能发现它**
    /// （HANDOFF §5 第 36 条）。
    ///
    /// 单元测试测的是"零件本身对不对"；这个方法让"装进来了"成为
    /// 可断言的东西。两者缺一，就会留下"接线在、但没被走到"的洞。
    fn component_counts(&self) -> (usize, usize, usize, usize);
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
    /// 它由 [`Pipeline::compose`] 产出，顺序满足"可复现"铁律
    /// （同一状态 + 同一输入 ⇒ 逐字节相同）。
    ///
    /// **排序在滤镜之前完成**，因此滤镜（如"长词优先"）的重排就是最终顺序
    /// ——见 [`Pipeline::compose`] 的说明。
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

    /// **重新打开最近一次上屏的内容**（RIME 的 `Reopen` / 编辑器动作
    /// `reopen_previous_selection`）。
    ///
    /// # 语义（与上游一致的那部分）
    ///
    /// 上屏之后，用户常常想"回到刚才那个词"改一下。RIME 的做法是把
    /// 刚上屏的那一段**放回预编辑串**，于是候选列表重新出现、可以再选一次。
    ///
    /// 实现上只有引擎自己知道"刚才那条是从哪串拼写来的"——`Commit`
    /// 里带着 `input`（原始拼写）与规范编码键，所以重开 = 把那次上屏的
    /// **输入串**恢复进 composition，再重新分析一遍。
    ///
    /// # 返回
    ///
    /// `true` = 真的重开了一段；`false` = 没有可重开的内容
    /// （刚启动、或上一次操作是取消/重置）。
    ///
    /// # 已知边界（**不要当成完整的 RIME 重开**）
    ///
    /// - 只支持**最近一次**上屏（没有提交历史栈）；
    /// - 重开出来的段**没有被标记为"已确认"**，因此预编辑串里分不出
    ///   "这段是我刚放回来的"与"这段是我新敲的"；
    /// - 重开之后再上屏一次会**再学习一次**（同一条被记两次）。
    ///   上游用提交历史避免这件事，我们还没有那份结构。
    ///
    /// 默认实现返回 `false`：不实现重开的会话保持原样。
    fn reopen(&mut self) -> bool {
        false
    }

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
    /// **正在派发"被重绑定换来的"按键**。
    ///
    /// 这一段的历史值得留着：它现在是会话状态里的一个布尔量，因为
    /// **只有 `key_binder` 该看它**，而"哪个处理器是 `key_binder`"是流水线
    /// 装配时才知道的事——把下标记在流水线里（我第一版的做法）会让
    /// "换来的按键跳过某个下标"与"重绑定器知道自己在重入"变成两件事，
    /// 于是只改对一半。
    ///
    /// 与 `librime` 的 `KeyBinder::redirecting_` 一一对应。
    pub redirecting: bool,
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
    /// **预测候选块**在已渲染列表里的起点（P4b，由流水线写回）。
    ///
    /// `Lane::Predict` 的候选在 [`Pipeline::compose`] 的结果里是**连续一段**，
    /// 因此"第 N 个输入候选在哪儿"可以用这对字段算出来。
    ///
    /// # 它为什么是显式的会话状态，而不是"读一下候选列表"
    ///
    /// 因为**处理器看不到候选列表**（那是会话的东西）。而
    /// [`crate::Processor`] 里有两处真的需要它：
    ///
    /// 1. `selector` 的**数字键映射**：预测候选不参与盲选，所以"按 3"
    ///    必须指向**第 3 个输入候选**，而不是已渲染列表的第 3 项。
    ///    少了这对字段，插在中间的预测候选会把后面所有输入候选的编号
    ///    整体推后——用户按 2 却什么都没发生（那正是设计文档
    ///    `docs/engine-design.md` §4.3.1 要防的"肌肉记忆被破坏"）。
    /// 2. `key_binder` 的 `when: predicting` 谓词。
    pub predict_start: usize,
    /// 预测候选的条数（0 = 这次没有预测）。
    pub predict_count: usize,
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
            redirecting: false,
            sent_keys: Vec::new(),
            candidate_count: 0,
            candidate_pages: 0,
            candidate_page: 0,
            predict_start: 0,
            predict_count: 0,
        }
    }

    /// **第 `ordinal` 个可盲选候选在已渲染列表里的下标**（P4b）。
    ///
    /// # 它存在的理由
    ///
    /// `selector` 把数字键 `n` 翻译成"第 n 个候选"。而预测候选
    /// （`Lane::Predict`）**不参与盲选**（`docs/engine-design.md` §4.3.1）：
    /// 它们可能插在输入候选中间（§4.3.2 的默认位置是"第 1 名之后"），
    /// 于是"已渲染列表的第 n 项"与"第 n 个输入候选"在插入点之后**不再相等**。
    ///
    /// 这一条把两个编号空间显式地换算一次：
    ///
    /// ```text
    /// ordinal <  predict_start  →  下标 = ordinal
    /// ordinal >= predict_start  →  下标 = ordinal + predict_count
    /// ```
    ///
    /// 于是**输入候选的编号永远等于它的名次**，插进来多少条预测都不影响
    /// ——这正是"盲选肌肉记忆"要的东西。
    #[must_use]
    pub fn selectable_index(&self, ordinal: usize) -> usize {
        if ordinal < self.predict_start {
            ordinal
        } else {
            ordinal.saturating_add(self.predict_count)
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
    ///
    /// **它借用 `self`，不克隆任何东西**——这条性质是有代价换来的：
    /// `Query` 早先带一个 `composition` 字段，于是每个组件都要
    /// 一份独立的克隆（见 `Query` 的说明，那一次是 5 倍延迟）。
    #[must_use]
    pub fn query(&self) -> Query<'_> {
        Query {
            input: &self.composition.input,
            caret: self.composition.caret,
            options: &self.options,
            context: &self.context,
            segment_text: &self.composition.input,
        }
    }
}
