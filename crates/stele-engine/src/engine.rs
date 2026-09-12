//! # Engine and Session
//!
//! 中文职责：引擎与会话的具体实现。
//! English role: the concrete engine and session.
//! 架构位置：`stele-core::Engine` / `Session` / `SchemaCatalog` 的实现。
//!
//! # 会话如何兑现"上屏"
//!
//! 处理器只表达**意图**（写进 `SessionState::pending_commit`）；
//! **会话**把它兑现成完整的 [`Commit`]——因为只有会话同时知道
//! "已渲染的候选列表"和"当前输入"。
//!
//! 兑现时顺带执行两条规则：
//!
//! 1. 下标越界 → 当作"没选中"（不改状态、不产出上屏）。
//! 2. 预测通道的候选**默认不允许键盘盲选**（`docs/engine-design.md` §4.3.3）。

use std::sync::Arc;
use stele_core::{
    Candidate, Commit, Context, Engine, Event, Lane, LoadedSchema, Options, Outcome, PendingCommit,
    Pipeline, ProcessResult, SchemaCatalog, SchemaError, SchemaInfo, SelectionSource, Session,
    SessionState, Trigger,
};

use crate::scheme::LoadedScheme;

/// 引擎内部：已装载的方案集合。
///
/// 用 `Vec` 而不是 `BTreeMap`：方案数量是个位数，线性查找更快，
/// 而且 `list()` 要返回 `&[SchemaInfo]`——顺序确定（PLAN §5.2）。
struct EngineInner {
    schemes: Vec<Arc<LoadedScheme>>,
    infos: Vec<SchemaInfo>,
}

impl EngineInner {
    fn find(&self, schema_id: &str) -> Option<&Arc<LoadedScheme>> {
        self.schemes
            .iter()
            .find(|s| s.info().schema_id == schema_id)
    }
}

/// 引擎实现。
///
/// # 关于"惰性装载"
///
/// PLAN D29 要求"多方案按内存预算惰性装载与淘汰"。P1 的实现是
/// **全部在构造时编译好**——因为 P1 的方案只有几百条词，静态编译的开销
/// 可以忽略，而惰性装载 + LRU 淘汰需要真正的内存计量（那是 P2.5 的内容，
/// 等词库大到几 MB 才有意义）。
///
/// **接口形状已经是对的形状**：`acquire()` 返回 `Arc`，丢弃即释放；
/// 将来把 `schemes` 换成"未装载则编译 + 超预算先淘汰"即可，
/// **不需要改任何调用方**。
#[derive(Clone)]
pub struct EngineImpl {
    inner: Arc<EngineInner>,
}

impl EngineImpl {
    /// 由一组方案声明构造。
    ///
    /// # Errors
    ///
    /// 任一方案编译失败即返回错误——**但错误里带上方案 id**，
    /// 便于调用方做"跳过坏的、加载好的"的降级（PLAN D26）。
    pub fn new(defs: &[crate::scheme::SchemeDef]) -> Result<Self, SchemaError> {
        let mut schemes = Vec::with_capacity(defs.len());
        let mut infos = Vec::with_capacity(defs.len());

        for d in defs {
            let s = d.compile()?;
            infos.push(s.info().clone());
            schemes.push(Arc::new(s));
        }

        Ok(Self {
            inner: Arc::new(EngineInner { schemes, infos }),
        })
    }

    /// 已装载的方案数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.schemes.len()
    }

    /// 是否没有任何方案。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.schemes.is_empty()
    }
}

impl SchemaCatalog for EngineImpl {
    fn list(&self) -> &[SchemaInfo] {
        &self.inner.infos
    }

    fn acquire(&self, schema_id: &str) -> Result<Arc<dyn LoadedSchema>, SchemaError> {
        self.inner
            .find(schema_id)
            .map(|s| Arc::clone(s) as Arc<dyn LoadedSchema>)
            .ok_or_else(|| SchemaError::NotFound {
                schema_id: schema_id.to_owned(),
            })
    }
}

impl Engine for EngineImpl {
    fn create_session(&self) -> Box<dyn Session + Send> {
        let scheme = Arc::clone(&self.inner.schemes[0]);
        Box::new(SessionImpl::new(scheme, Arc::clone(&self.inner)))
    }

    fn schemas(&self) -> &dyn SchemaCatalog {
        self
    }
}

/// 会话实现。
pub struct SessionImpl {
    inner: Arc<EngineInner>,
    scheme: Arc<LoadedScheme>,
    pipeline: Box<dyn Pipeline + Send>,
    state: SessionState,
    candidates: Vec<Candidate>,
    events: Vec<Event>,
}

impl SessionImpl {
    fn new(scheme: Arc<LoadedScheme>, inner: Arc<EngineInner>) -> Self {
        let pipeline = scheme.build_pipeline();
        let options = scheme.options().clone();
        let context = Context::with_capacity(8);
        Self {
            inner,
            scheme,
            pipeline,
            state: SessionState::new(stele_core::Composition::default(), options, context),
            candidates: Vec::new(),
            events: Vec::new(),
        }
    }

    /// 重新计算候选。
    fn recompose(&mut self) {
        let mut out = std::mem::take(&mut self.candidates);
        self.pipeline.compose(&mut self.state, &mut out);
        self.pipeline.finalize(&mut out);
        self.candidates = out;
    }

    /// 把处理器的意图兑现成一次上屏。
    ///
    /// 返回 `None` 表示"下标无效，什么都没发生"。
    fn realize_commit(
        &mut self,
        pending: PendingCommit,
        source: SelectionSource,
    ) -> Option<Commit> {
        let c = self.candidates.get(pending.index)?;

        // 规则 2：预测通道的候选默认不允许**键盘盲选**。
        //
        // 理由：用户会对输入通道形成肌肉记忆（"敲 nihao 然后按 1"）。
        // 若预测候选也能被数字键选中、位置还会变，这套肌肉记忆就崩了。
        if c.lane == Lane::Predict && source == SelectionSource::Keyboard {
            return None;
        }

        let commit = Commit {
            text: c.text.clone(),
            input: self.state.composition.input.clone(),
            context: self.state.context.recent().to_vec(),
            origin: c.origin,
            attr: c.attr,
            lane: c.lane,
            trigger: pending.trigger,
        };

        // 学习事件（P4a 的实现会消费它）。
        //
        // **注意传的是原始输入与属性**——接收方必须按 `attr` 把 `input`
        // 规范化成规范编码再落库，否则会产生"永远检索不到的无效数据"（G10）。
        self.events.push(Event::Learned {
            input: commit.input.clone(),
            text: commit.text.clone(),
            origin: commit.origin,
            attr: commit.attr,
            lane: commit.lane,
        });

        self.state.context.push(commit.text.clone());
        self.state.composition.reset();
        self.state.pending_commit = None;
        self.candidates.clear();

        Some(commit)
    }
}

impl Session for SessionImpl {
    fn process_key(&mut self, key: stele_core::Key) -> Outcome {
        self.state.pending_commit = None;
        let result = self.pipeline.process_key(&mut self.state, &key);

        match result {
            // 两种情况**对外结果相同**（都把按键还给系统），但语义不同：
            // `Rejected` 是"明确拒绝"，`Noop` 是"无人认领"。
            // P1 里没有"提交历史"之类的后处理，所以两者合并；
            // 将来若要区分，就在这里分开——这是**唯一的分支点**。
            ProcessResult::Rejected | ProcessResult::Noop => Outcome::Rejected,
            // `ProcessResult` 是 `#[non_exhaustive]`（D30）：将来加了变体，
            // 这里会**编译失败**而不是静默走错分支——这正是它的目的。
            _ => {
                self.recompose();
                match self.state.pending_commit.take() {
                    Some(p) => match self.realize_commit(p, p.source) {
                        Some(c) => Outcome::Committed(c),
                        // 下标无效 → 按键仍被消费（否则空格会漏给系统）。
                        None => Outcome::Consumed,
                    },
                    // 没有请求上屏：按键被消费，只需重绘。
                    None => Outcome::Consumed,
                }
            }
        }
    }

    fn composition(&self) -> &stele_core::Composition {
        &self.state.composition
    }

    fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }

    fn select(&mut self, index: usize, source: SelectionSource) -> Outcome {
        match self.realize_commit(PendingCommit::keyboard(index, Trigger::Explicit), source) {
            Some(c) => Outcome::Committed(c),
            None => Outcome::Consumed,
        }
    }

    fn commit(&mut self) -> Option<Commit> {
        self.realize_commit(
            PendingCommit::keyboard(0, Trigger::Space),
            SelectionSource::Keyboard,
        )
    }

    fn reset(&mut self) {
        self.state.composition.reset();
        self.state.pending_commit = None;
        self.candidates.clear();
    }

    fn option(&self, name: &str) -> bool {
        self.state.options.get(name)
    }

    fn set_option(&mut self, name: &str, on: bool) -> bool {
        let ok = self.state.options.set(name, on);
        if ok {
            self.events.push(Event::OptionChanged {
                name: name.to_owned(),
                on,
            });
        }
        ok
    }

    fn schema_id(&self) -> &str {
        &self.scheme.info().schema_id
    }

    fn switch_schema(&mut self, schema_id: &str) -> Result<(), SchemaError> {
        // 失败时**保持原方案可用**（PLAN D26）——先找到目标，再动手切换。
        let target = self
            .inner
            .find(schema_id)
            .cloned()
            .ok_or_else(|| SchemaError::NotFound {
                schema_id: schema_id.to_owned(),
            })?;

        let pipeline = target.build_pipeline();
        let options = target.options().clone();

        self.scheme = target;
        self.pipeline = pipeline;
        self.state.composition.reset();
        self.state.options = options;
        self.candidates.clear();
        Ok(())
    }

    fn drain_events(&mut self, out: &mut Vec<Event>) {
        out.append(&mut self.events);
    }
}

impl SessionImpl {
    /// 当前方案的元数据（供 CLI / 前端显示）。
    #[must_use]
    pub fn current_schema(&self) -> &SchemaInfo {
        self.scheme.info()
    }

    /// 当前方案的开关集合。
    #[must_use]
    pub fn options(&self) -> &Options {
        &self.state.options
    }
}
