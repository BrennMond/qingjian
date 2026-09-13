//! # qingjian-memory — 用户记忆（P4a）
//!
//! 中文职责：**频率 + 时间衰减**的用户记忆。它是 `qingjian_core::MemoryStore`
//! 的服务提供者，以及把它接到候选排序上的 `Ranker`。
//! English role: the frequency-plus-decay user memory: a `MemoryStore` provider
//! plus the `Ranker` that applies it.
//! 架构位置：PLAN §2.1 的"侧挂服务"——**注入引擎**，不是引擎的上层。
//! 它只依赖 `qingjian-core` 的 trait，**不依赖 `qingjian-engine`**。
//!
//! # 这个 crate 兑现的四件事
//!
//! | 验收（HANDOFF §7.6.0） | 落在哪 |
//! | --- | --- |
//! | 打过的词下次优先 | [`ranker::MemoryRanker`] + [`decay`] 的量纲 |
//! | **按键时零磁盘 I/O**（红线） | [`store::FileMemory`]：按键只碰内存，落盘只由 [`store::FileMemory::flush`] 触发 |
//! | 进程重启后学到的词还在 | [`file`] 的自校验紧凑格式 |
//! | 记忆文件坏了 = 降级 + 警告 | [`store::FileMemory::open_or_degrade`]（D26） |
//!
//! # seam 的三个角色（PLAN §5.12 要求"顺手回答"）
//!
//! - **接口是什么**：`qingjian_core::MemoryStore`（定义在核心），
//!   以及 `qingjian_core::Ranker`（记忆的读取端）。
//! - **谁实现**：本 crate（[`store::FileMemory`] + [`ranker::MemoryRanker`]）。
//! - **谁消费**：引擎流水线在**装配时**注入 `Arc<dyn Ranker>`；
//!   前端（CLI / TSF / Android）消费 `Event::Learned` 与 `Event::ForgetRequested`
//!   两条事件，把它们转成 `record` / `forget`。
//!
//! # 两条刻意的设计选择（都能被测试证伪）
//!
//! 1. **键是"规范编码"**（如 `ni'hao`），由翻译器渲染成 `Candidate::key`
//!    随候选带出（PLAN D42）。同一条编码只有一把键，于是 `nhao` 与 `nihao`
//!    共享同一份记忆——G10 那颗地雷（"永远检索不到的无效数据"）
//!    在**算术上**不可能被踩到，而跨拼法共享是它的直接结果。
//! 2. **量纲全部是整数**（D13）。衰减是整数次减半、加成是整数除法，
//!    因此"同一份记忆 + 同一时刻 ⇒ 逐字节相同的候选序列"不是承诺而是事实。
//!
//! # 明确不做的事（HANDOFF §7.6.5）
//!
//! 下一词预测（`predict_next` 现在返回空，那是 P4b）、向量重排（P5）。

pub mod clock;
pub mod decay;
pub mod events;
pub mod ranker;
pub mod store;

mod file;

pub use clock::SystemClock;
pub use decay::{
    bonus_ml, bonus_now, decay_milli, decayed_after_commit, decayed_now, halvings, HALF_LIFE_SECS,
    HALF_SATURATION_MILLI, MAX_BONUS_ML, MAX_DECAYED_MILLI,
};
pub use events::apply_events;
pub use ranker::MemoryRanker;
pub use store::{
    normalize_key, FileMemory, PredictionEntry, DEFAULT_CAPACITY, DEFAULT_PREDICT_CAPACITY,
    MAX_PREDICTIONS,
};

/// 用户记忆层的错误。
///
/// # 为什么调用方很少看到它
///
/// 按 D26（"配置错误绝不阻止启动"），**正常路径不该因为记忆文件坏掉而失败**：
/// [`FileMemory::open_or_degrade`] 把任何错误变成"空记忆 + 一行警告"。
/// 这个类型存在的意义是让 [`FileMemory::open`] 与 [`FileMemory::flush`]
/// 能说清**为什么**失败——诊断要能读，而不是"出了点问题"。
#[derive(Debug)]
#[non_exhaustive]
pub enum MemoryError {
    /// 读写文件失败。
    Io(std::io::Error),
    /// 文件存在但内容不是我们能理解的东西（魔数 / 版本 / 校验和 / 结构）。
    ///
    /// 保留原文而不是只留一个布尔：**"坏在哪一步"是用户唯一能自行处置的信息**。
    Corrupt(String),
}

impl core::fmt::Display for MemoryError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "读写失败：{e}"),
            Self::Corrupt(why) => write!(f, "文件内容不可用：{why}"),
        }
    }
}

impl std::error::Error for MemoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Corrupt(_) => None,
        }
    }
}

impl From<std::io::Error> for MemoryError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
