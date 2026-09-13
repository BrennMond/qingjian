//! # Store — 内存表 + 自写紧凑文件的 `MemoryStore` 实现
//!
//! 中文职责：用户记忆的实现。按键路径**只碰内存**，落盘是一条显式的调用。
//! English role: the user-memory implementation; keystrokes touch memory only.
//! 架构位置：`qingjian-memory` 的核心，实现 `qingjian_core::MemoryStore`。
//!
//! # 三条硬约束（P4a 的验收）
//!
//! 1. **按键时零磁盘 I/O**（红线）。`record` / `lookup` / `forget` 只改内存，
//!    落盘只由显式的 [`FileMemory::flush`] 触发。这条有测试：
//!    `crates/qingjian-memory/tests/no_disk_io_on_keypath.rs` 读 `/proc/self/io`。
//! 2. **坏文件 = 降级成"没有记忆" + 警告**，绝不阻止启动（D26）。
//!    见 [`FileMemory::open_or_degrade`]。
//! 3. **淘汰必须确定**。按"衰减后的频次"升序淘汰，平局按 `(input, text)`
//!    字典序——**不许依赖容器遍历顺序**，否则同一份数据两次运行会淘汰不同条目。
//!
//! # 锁的取舍（先用最笨的，再用实测决定要不要换）
//!
//! `MemoryStore` 的方法是 `&self`，而多个 `Session` 共享同一个实现，
//! 所以内部可变性 + 锁是必须的。这里先用 `RwLock`：
//! **查（每键一次）走读锁、写（每次上屏一次）走写锁**。
//! HANDOFF §7.6.2 第 3 步要求"先跑通再实测；超标再换无锁读法"——
//! 实测数字在 HANDOFF §0 与 PLAN §9 的基线表里。
//!
//! 锁中毒一律 `into_inner` 继续：**输入法的失败是自锁的**（D26），
//! 一个曾经 panic 过的线程不该让之后每一次按键都跟着死。

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use qingjian_core::{
    Clock, Commit, Context, Lane, MemoryEntry, MemoryStore, Origin, Prediction, PredictionOrigin,
    Score,
};

use crate::decay::{bonus_now, decayed_after_commit};
use crate::file::{self, PredRecord, Record};
use crate::MemoryError;

/// 默认的条目上限。
///
/// # 它怎么来的（**实测反推**，不是拍脑袋）
///
/// 红线是常驻内存 < 30 MB，而真实词库（41 万条）的常驻已经占 **13.6 MiB**。
/// 实测（`qingjian-bench --scheme-dir schemes/qingjian-default --userdb <文件>
/// --seed-memory=N`，同一台机器上读 `VmRSS`）。**两种形态都要看**：
/// `--seed-memory` 是"一次运行里新学这么多"（分配器要给新页，最悲观），
/// 装载已有文件是"重启后读回来"（分配更紧凑）：
///
/// | 记忆条目 | 常驻内存（含真实词库） | 相对 0 条（13.6 MiB） |
/// | --- | --- | --- |
/// | 0 | 13 944 KiB（13.6 MiB） | — |
/// | **30 000（新学）** | **19 884 KiB（19.4 MiB）** | +5.8 MiB |
/// | **30 000（装载）** | **18 204 KiB（17.8 MiB）** | +4.3 MiB |
/// | 50 000（新学） | 27 108 KiB（26.5 MiB） | +12.9 MiB |
/// | 100 000（隔离测量） | +18.2 MiB —— 叠上词库会**越过 30 MB 红线** | — |
///
/// **HANDOFF §7.6.1 ③ 曾建议 100 000。实测说不行**：那个值会把总常驻
/// 推过红线。取 `30_000` 之后总常驻约 18–20 MiB，给前端与将来的功能
/// （P4b 的 n-gram、P5 的向量）留出约 10 MB 余量——
/// 而"30 000 条 (输入, 词) 对"对个人使用是**很宽裕**的量级。
///
/// 单条 150–190 字节里，一半是 `String` 的两份堆分配（输入与词），
/// 另一半是 `BTreeMap` 的节点开销。**这正是"按实测反推"的意义**：
/// 估算会得到"几十字节"，而真相是三到四倍。
///
/// 超限时淘汰**衰减频次最低**的条目（见 [`FileMemory::evict`]）。
/// 这个值是常量，但 [`FileMemory::in_memory`] / [`FileMemory::open`]
/// 都接受上限参数——前端要调，改一处即可。
pub const DEFAULT_CAPACITY: usize = 30_000;

/// 预测表（上下文 n-gram）的默认条目上限。
///
/// # 它为什么是**另一个**上限，而不是与 [`DEFAULT_CAPACITY`] 共用一个
///
/// 两张表回答两个不同的问题、各自独立增长，也**各自独立关闭**：
/// 用户可能只要"打过的词下次优先"而不要预测。共用一个上限会让
/// "关掉预测"这件事省不下内存（预测表还在长），那就不是真的关掉了。
///
/// # 这个值怎么来的（**实测反推**，照 P4a 的做法）
///
/// 红线仍是常驻 < 30 MB，而 P4b 的验收写的是**预测增量 < 5 MB**
/// （PLAN §3）。实测用的是"突破默认上限量斜率"的办法——
/// 在小 N 上直接读 RSS 增量**会严重低估**：引擎装载那一段会留下一大片
/// 已驻留的空闲堆，前两万条记录直接把它填满，RSS 一动不动。
/// `qingjian-bench --scheme-dir schemes/qingjian-default --schema=pinyin
/// --userdb /tmp/m.mem --predict-cap=300000 --seed-predict=N`：
///
/// | 预测条目 | 常驻内存（含真实词库 13.6 MiB） | 单条 |
/// | --- | --- | --- |
/// | 100 000 | 32 488 KiB（31.7 MiB） | ≈ 188 字节 |
/// | 200 000 | 55 320 KiB（54.0 MiB） | ≈ 211 字节 |
///
/// 取上界 **≈ 210 字节/条** 反推：`20 000 × 210 B ≈ 4.0 MiB`，
/// 落在 5 MB 之内并留出一点余量（真实上下文词比测试串长，单条会更大）。
///
/// # 一次上屏最多产生两条
///
/// 每次预测学习会同时写一条 **trigram**（最近两个词）与一条 **bigram**
/// （最近一个词）——这样"更具体的搭配优先、数据不够时回退到更泛的"。
/// 因此上表的"条目"数要按**两次**增长来理解：`20_000` 条约等于
/// `10_000` 个不同的"上下文 → 下一词"对。
pub const DEFAULT_PREDICT_CAPACITY: usize = 20_000;

/// 一次 `predict_next` 最多返回几条（在流水线的限额之前）。
///
/// 引擎侧还有 `DEFAULT_PREDICTION_LIMIT`（默认 2）决定**插进候选几条**；
/// 这里多留一些，是为了让"重复项合并"与将来的排序调整有回旋余地。
pub const MAX_PREDICTIONS: usize = 5;

/// 淘汰的批大小比例：满了就一次降到 `cap × 7/8`。
///
/// 为什么要批量：如果每插入一条就扫一遍全表找最小值，那个 O(n) 就落在
/// **按键路径**上了。批量之后扫描被摊薄到"每 `cap/8` 次插入一次"，
/// 而每次扫描仍然是确定的（同一份数据、同一时刻 ⇒ 淘汰同一批）。
const EVICT_NUMERATOR: usize = 7;
const EVICT_DENOMINATOR: usize = 8;

/// 把一段**拼写**规范化成记忆的键。
///
/// # 它只在"拿不到编码"时才用得上
///
/// 记忆的主键是**规范编码**（如 `ni'hao`），由翻译器渲染成
/// [`qingjian_core::Candidate::key`] 随候选带出（PLAN D42）。本函数是
/// **兜底路径**：原样上屏、标点、造句这些候选没有编码，只能按拼写记。
///
/// # 这一步是在修一颗地雷（HANDOFF §7.6.3 第 8 条）
///
/// "重启后主键不一致"的症状是"记忆时有时无"：用户敲 `ni'hao` 学到的东西，
/// 敲 `nihao` 时找不到。规范化把分隔符与空白去掉、大小写统一，
/// 让**同一个意图只有一把键**。
///
/// # ⚠️ 它**不能**用在编码键上
///
/// 编码键长这样：`ni'hao`——那个 `'` 是**编码单元的分隔符**，不是可选的
/// 花饰（`["n","i","hao"]` 与 `["ni","hao"]` 拼起来都是 `nihao`）。
/// 把编码键喂进本函数会得到另一把键，于是"存进去的"与"查出来的"错位。
/// 因此 [`FileMemory::record`] 只在 `commit.key` 缺失时才调用它，
/// 而 [`FileMemory::lookup`] / `forget` **一次都不调用**——那里的 `key`
/// 已经是最终键。
#[must_use]
pub fn normalize_key(spelling: &str) -> String {
    spelling
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\'')
        .flat_map(char::to_lowercase)
        .collect()
}

/// 预测表的一条记录（`--dump-memory` 与测试用的只读视图）。
///
/// # 它为什么不是 `qingjian_core::Prediction`
///
/// `Prediction` 是**查询结果**（"接下来可能是什么"），只有文本与分数。
/// 这个类型是**表的行**：它还要有"学过几次、什么时候学的"——
/// 那是"预测怎么不生效"这个问题唯一能查的东西。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PredictionEntry {
    /// 上下文（最多两个词，最近的在后）。长度为 1 时这是一条 bigram。
    pub context: Vec<String>,
    /// 预测出来的词。
    pub text: String,
    /// 由这段上下文上屏过几次。
    pub count: u32,
    /// 由"次数 + 时间衰减"算出的定点加成。
    pub bonus: Score,
    /// 最后一次使用时间（Unix 秒）。
    pub last_used: u64,
}

/// 内存里的一条记录。
#[derive(Clone, Copy, Debug, Default)]
struct Entry {
    /// 累计上屏次数。
    count: u32,
    /// 已衰减的累计频次（千分之一单位）。
    decayed_milli: u64,
    /// 最后一次使用时间（Unix 秒）。
    last_used: u64,
}

/// 可变的内部状态。锁保护的就是它。
#[derive(Default)]
struct Inner {
    /// `(规范化输入, 词) → 记录`。用 `BTreeMap` 是**刻意的**：
    /// 落盘顺序与淘汰平局都由它决定，而 `HashMap` 会让两者都变得不确定。
    ///
    /// 这张表的键是**编码**（PLAN D42，如 `ni'hao`）。
    entries: BTreeMap<(String, String), Entry>,
    /// `(ctx1, ctx2, 词) → 记录`：预测表（P4b）。
    ///
    /// # 为什么它与 `entries` 分开，而不是共用一张表
    ///
    /// 两张表的**键空间完全不同**：这边是词文本（`微信`），那边是编码
    /// （`ni'hao`）。混在一张表里，一次查询就会在两套键之间乱撞——
    /// 症状是"有时查到奇怪的东西"，而它不会报错（HANDOFF §7.7.4 第 2 条）。
    ///
    /// # 为什么 `ctx1` 用空串表示"这是一条 bigram"
    ///
    /// 因为 `BTreeMap` 的**连续区间**是这个实现的性能来源：
    /// - trigram 查询 = `("今天", "微信", _)` 这一段；
    /// - bigram 查询 = `("", "微信", _)` 这一段。
    ///
    /// 上下文里的词不可能是空串（`Context::push` 丢弃空串），
    /// 因此空串是安全的保留值。
    predictions: BTreeMap<(String, String, String), Entry>,
    /// **内存状态的代次**：任何一次修改都 +1。
    ///
    /// 为什么要代次而不是一个布尔 `dirty`：落盘要先把快照编码，再做
    /// 一段可能失败的 I/O。用布尔量时，"快照之后、清 dirty 之前"发生的
    /// 并发写入会被**一并当成已保存**而丢掉（P0-D 的复现之一）。
    /// 有了代次，落盘完成时只把 `saved` 推进到**快照那一刻**的代次：
    /// 之后的新写入让 `generation > saved`，`dirty` 自然仍为真。
    generation: u64,
    /// 最后一次**确认写成功**的代次。
    saved: u64,
}

impl Inner {
    /// 有任何未落盘的改动吗？
    fn is_dirty(&self) -> bool {
        self.generation != self.saved
    }

    /// 标记一次修改。
    fn touch(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }
}

/// 用户记忆：全量放内存，落盘是一条显式调用。
pub struct FileMemory {
    /// 落盘路径。`None` = 只在内存里（测试与"本次不持久化"）。
    path: Option<PathBuf>,
    /// 注入的时钟（D38）。**引擎不读系统时间**，记忆也一样。
    clock: Arc<dyn Clock>,
    /// 条目上限。
    cap: usize,
    /// **预测表**的条目上限（P4b）。与 `cap` 分开，见 [`DEFAULT_PREDICT_CAPACITY`]。
    predict_cap: usize,
    /// 加载失败时为 `false`：**不许覆盖一个我们读不懂的文件**。
    ///
    /// 理由：坏文件里的数据可能是用户唯一的记录，而"降级成没有记忆"
    /// 已经是可用性上的让步；再把人家仅有的文件覆盖掉就说不过去了。
    writable: bool,
    inner: RwLock<Inner>,
}

impl FileMemory {
    /// 只为内存的实现（不落盘）。测试与"本次运行不要持久化"用它。
    #[must_use]
    pub fn in_memory(clock: Arc<dyn Clock>, cap: usize) -> Self {
        Self::in_memory_with(clock, cap, DEFAULT_PREDICT_CAPACITY)
    }

    /// 只为内存的实现，两张表的上限都可指定。
    #[must_use]
    pub fn in_memory_with(clock: Arc<dyn Clock>, cap: usize, predict_cap: usize) -> Self {
        Self {
            path: None,
            clock,
            cap: cap.max(1),
            predict_cap: predict_cap.max(1),
            writable: true,
            inner: RwLock::new(Inner::default()),
        }
    }

    /// 从一个文件装载。路径不存在按"空表"处理（第一次运行就是这种情形）。
    ///
    /// # Errors
    ///
    /// 文件存在但读不动、或格式不对时返回 [`MemoryError`]。
    /// **调用方通常应该用 [`FileMemory::open_or_degrade`]**：
    /// 那条路把错误变成"没有记忆 + 一行警告"，符合 D26。
    pub fn open(
        path: impl AsRef<Path>,
        clock: Arc<dyn Clock>,
        cap: usize,
    ) -> Result<Self, MemoryError> {
        Self::open_with(path, clock, cap, DEFAULT_PREDICT_CAPACITY)
    }

    /// 从一个文件装载，两张表的上限都可指定。
    ///
    /// # Errors
    ///
    /// 同 [`FileMemory::open`]。
    pub fn open_with(
        path: impl AsRef<Path>,
        clock: Arc<dyn Clock>,
        cap: usize,
        predict_cap: usize,
    ) -> Result<Self, MemoryError> {
        let path = path.as_ref().to_path_buf();
        let cap = cap.max(1);
        let predict_cap = predict_cap.max(1);
        let mut inner = Inner::default();
        if path.exists() {
            let bytes = std::fs::read(&path).map_err(MemoryError::Io)?;
            let (records, preds) =
                file::decode(&bytes).map_err(|e| MemoryError::Corrupt(e.to_string()))?;
            for r in records {
                inner.entries.insert(
                    (r.input, r.text),
                    Entry {
                        count: r.count,
                        decayed_milli: r.decayed_milli,
                        last_used: r.last_used,
                    },
                );
            }
            for p in preds {
                inner.predictions.insert(
                    (p.ctx1, p.ctx2, p.text),
                    Entry {
                        count: p.count,
                        decayed_milli: p.decayed_milli,
                        last_used: p.last_used,
                    },
                );
            }
        }
        // **加载之后也要遵守上限**（P0-D 第 5 条）。
        //
        // 旧实现只在"新记录导致超限"时淘汰，于是 `cap=1` 打开一份含 10 条
        // 的文件会加载 10 条——上限在恢复路径上是**不成立**的，而恢复
        // 正好是内存最敏感的时刻（前端在启动时打开记忆）。
        if inner.entries.len() > cap {
            Self::evict(&mut inner, cap);
        }
        if inner.predictions.len() > predict_cap {
            Self::evict_predictions(&mut inner, predict_cap);
        }
        Ok(Self {
            path: Some(path),
            clock,
            cap,
            predict_cap,
            writable: true,
            inner: RwLock::new(inner),
        })
    }

    /// 打开，**失败就降级**（PLAN D26）。
    ///
    /// 返回 `(记忆, 警告)`。警告是 `Some` 时调用方**必须打印出来**——
    /// "功能静默不生效"是这个项目反复踩的坑，降级必须看得见。
    ///
    /// 降级得到的实例**不可写**：我们不会覆盖一个读不懂的文件。
    #[must_use]
    pub fn open_or_degrade(
        path: impl AsRef<Path>,
        clock: Arc<dyn Clock>,
        cap: usize,
    ) -> (Self, Option<String>) {
        let path = path.as_ref();
        match Self::open(path, clock, cap) {
            Ok(m) => (m, None),
            Err(e) => {
                let mut degraded = Self::in_memory(degraded_clock(), cap);
                degraded.writable = false;
                let note = format!(
                    "用户记忆文件 {} 不可用（{e}）——本次运行将不使用记忆，且**不会覆盖它**。\
                     若不需要里面的内容，请自行删除或改名后重试。",
                    path.display()
                );
                (degraded, Some(note))
            }
        }
    }

    /// 条目上限。
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// 当前条目数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.read().entries.len()
    }

    /// 是否为空（**两张表都空**才算空）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        let inner = self.read();
        inner.entries.is_empty() && inner.predictions.is_empty()
    }

    /// 预测表的条目上限（P4b）。
    #[must_use]
    pub fn prediction_capacity(&self) -> usize {
        self.predict_cap
    }

    /// 预测表当前的条目数。
    #[must_use]
    pub fn prediction_len(&self) -> usize {
        self.read().predictions.len()
    }

    /// 是否有尚未落盘的改动。
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.read().is_dirty()
    }

    /// 是否允许落盘（加载失败时为 `false`）。
    #[must_use]
    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// 落盘路径（若有）。
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// 全部记录，按 `(输入, 词)` 升序——**内存表顺序的只读视图**。
    ///
    /// 用途有二：调试前端可以把它打出来（"到底学到了什么"是这个项目反复
    /// 需要回答的问题），测试可以用它断言淘汰结果。
    #[must_use]
    pub fn snapshot(&self) -> Vec<MemoryEntry> {
        let now = self.clock.now_secs();
        let inner = self.read();
        inner
            .entries
            .iter()
            .map(|((input, text), e)| MemoryEntry {
                input: input.clone(),
                text: text.clone(),
                count: e.count,
                bonus: bonus_now(e.decayed_milli, e.last_used, now),
                last_used: e.last_used,
            })
            .collect()
    }

    /// 预测表的全部记录，按 `(较前的词, 最近的词, 词)` 升序。
    ///
    /// 与 [`FileMemory::snapshot`] 同一个用途：让"到底学到了什么"有一个
    /// 可观察的出口。**预测表必须能单独看见**——否则"预测怎么不生效"
    /// 这个问题只能靠猜，而那正是这个项目反复踩的坑。
    #[must_use]
    pub fn prediction_snapshot(&self) -> Vec<PredictionEntry> {
        let now = self.clock.now_secs();
        let inner = self.read();
        inner
            .predictions
            .iter()
            .map(|((ctx1, ctx2, text), e)| {
                // `ctx1` 为空串 = bigram：只显示最近那个词。
                let mut context = Vec::with_capacity(2);
                if !ctx1.is_empty() {
                    context.push(ctx1.clone());
                }
                context.push(ctx2.clone());
                PredictionEntry {
                    context,
                    text: text.clone(),
                    count: e.count,
                    bonus: bonus_now(e.decayed_milli, e.last_used, now),
                    last_used: e.last_used,
                }
            })
            .collect()
    }

    ///
    /// **显式落盘**。返回是否真的写了（没有改动 / 不可写 / 无路径时为 `false`）。
    ///
    /// # 为什么是"全量重写 + 原子替换"而不是"追加 + 合并"
    ///
    /// HANDOFF §7.6.2 建议的是"追加 + 定期合并"。这里选了更笨的那条，
    /// 理由是它与本项目的两条价值观对得上：
    ///
    /// - **零 I/O 是红线，不是"少 I/O"**：只有落盘这一处写文件，
    ///   就没有"追加日志写了一半、合并写到一半"的中间态要处理。
    /// - **落盘是状态的纯函数**：`BTreeMap` 顺序 + 自校验格式 ⇒
    ///   同一份内存状态永远产出逐字节相同的文件。追加日志做不到这一点。
    ///
    /// 代价是每次落盘 O(n) 字节。n 的上限是 [`DEFAULT_CAPACITY`]，
    /// 实测文件大小与耗时记在 HANDOFF 的 P4a 一节；调用点在**退出时**，
    /// 不在按键路径上。
    ///
    /// 写盘走"临时文件 + rename"：`rename` 在同一文件系统上是原子的，
    /// 因此**断电也不会留下半个文件**（那正是自校验格式要防的另一半）。
    ///
    /// # Errors
    ///
    /// 建目录、写临时文件或改名失败时返回 [`MemoryError::Io`]。
    /// **任何失败都保留 dirty**，因此调用方可以排除障碍后显式重试。
    ///
    /// # 原子性的确切含义（P0-D）
    ///
    /// 一次 `flush` 是一台状态机，顺序**不能**调换：
    ///
    /// ```text
    /// ① 拿文件锁（<path>.lock，跨进程互斥）
    /// ② 读盘上的当前文件并**合并**进内存（多写者不互相覆盖）
    /// ③ 取快照：编码成字节，记下快照代次 g
    /// ④ 写同目录唯一临时文件 → fsync → rename → fsync 目录
    /// ⑤ 成功之后才 saved = g（只推进到快照那一刻）
    /// ⑥ 放锁
    /// ```
    ///
    /// 旧实现在 ③ 之前就把 `dirty` 清成 `false`，于是：
    /// `write(tmp)` 失败 → 返回 `Err` → 但 `dirty == false` →
    /// 再调 `flush()` 直接返回 `Ok(false)`，**磁盘上什么都没有**。
    /// 用户学到的东西就这样一次次丢掉，而且没有任何提示。
    ///
    /// `saved = g`（而不是 `saved = generation`）是另一半：快照之后
    /// 新学的词不会因为"某次落盘成功了"而被当成已保存。
    pub fn flush(&self) -> Result<bool, MemoryError> {
        let Some(path) = self.path.as_deref() else {
            return Ok(false);
        };
        if !self.writable {
            return Ok(false);
        }
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).map_err(MemoryError::Io)?;
            }
        }

        // ① 文件锁。同目录的 `<name>.lock` 充当互斥量。
        //
        // 为什么需要它：两个 `FileMemory` 句柄（或两个进程）依次落盘时，
        // 后写者会用**自己**的全量快照覆盖先写者的记录——先写者刚学到的
        // 东西静默消失。加锁 + ② 的合并把"最后一次写赢"变成"并集"。
        let lock = Self::lock_file(path)?;
        // 提前放锁的路径都在下面显式 `unlock`；用守卫保证异常路径也放。
        let _guard = LockGuard { file: Some(&lock) };

        // ② 读盘上的当前内容并合并。
        //
        // 合并策略是**逐键取更大值**（次数、衰减频次、最后使用时间），
        // 而不是相加：相加会让"同一份数据加载两次"凭空翻倍，而取更大值
        // 是**幂等**的——合并自己刚写过的文件不改变任何数字。
        // 读不懂的文件**不动它**（坏文件不覆盖），报错并保留 dirty。
        if path.exists() {
            let bytes = std::fs::read(path).map_err(MemoryError::Io)?;
            let (records, preds) =
                file::decode(&bytes).map_err(|e| MemoryError::Corrupt(e.to_string()))?;
            let mut inner = self.write();
            for r in records {
                // 先取出标量，再把两个 `String` 移进键（否则是部分移动）。
                let entry = Entry {
                    count: r.count,
                    decayed_milli: r.decayed_milli,
                    last_used: r.last_used,
                };
                merge_entry(&mut inner.entries, (r.input, r.text), entry);
            }
            for p in preds {
                let entry = Entry {
                    count: p.count,
                    decayed_milli: p.decayed_milli,
                    last_used: p.last_used,
                };
                merge_entry(&mut inner.predictions, (p.ctx1, p.ctx2, p.text), entry);
            }
            inner.touch();
            // 合并可能把表推过上限（磁盘上有别的进程写的条目）。
            if inner.entries.len() > self.cap {
                Self::evict(&mut inner, self.cap);
            }
            if inner.predictions.len() > self.predict_cap {
                Self::evict_predictions(&mut inner, self.predict_cap);
            }
        }

        // ③ 快照 + 编码。**锁只在这一小段里持有**，文件 I/O 在锁外。
        let (bytes, snapshot) = {
            let inner = self.write();
            if !inner.is_dirty() {
                return Ok(false);
            }
            let records: Vec<Record> = inner
                .entries
                .iter()
                .map(|((input, text), e)| Record {
                    input: input.clone(),
                    text: text.clone(),
                    count: e.count,
                    decayed_milli: e.decayed_milli,
                    last_used: e.last_used,
                })
                .collect();
            let preds: Vec<PredRecord> = inner
                .predictions
                .iter()
                .map(|((ctx1, ctx2, text), e)| PredRecord {
                    ctx1: ctx1.clone(),
                    ctx2: ctx2.clone(),
                    text: text.clone(),
                    count: e.count,
                    decayed_milli: e.decayed_milli,
                    last_used: e.last_used,
                })
                .collect();
            // **两张表一起写、一起替换**：分成两个文件时，
            // "输入表写成功、预测表写失败"会留下一个没有任何自校验
            // 能发现的自相矛盾状态（见 `file` 的模块文档）。
            (file::encode(&records, &preds), inner.generation)
        };

        // ④ 唯一临时文件 → fsync → rename。
        let tmp = tmp_path(path);
        let write_result = (|| -> Result<(), MemoryError> {
            write_private(&tmp, &bytes)?;
            std::fs::rename(&tmp, path).map_err(MemoryError::Io)?;
            sync_dir(path);
            Ok(())
        })();
        if let Err(e) = write_result {
            // 失败：清掉临时文件，**保留 dirty**，把错误原样报出去。
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }

        // ⑤ 只有到这里才承认"保存过了"，而且只推进到快照代次。
        {
            let mut inner = self.write();
            inner.saved = snapshot;
        }
        Ok(true)
    }

    /// 打开（或创建）互斥用的锁文件。
    ///
    /// 锁文件独立于数据文件：数据文件的 `rename` 会换 inode，
    /// 而锁必须挂在**一个稳定的名字**上，否则两个写者会各锁各的。
    fn lock_file(path: &Path) -> Result<std::fs::File, MemoryError> {
        let lock_path = lock_path(path);
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(MemoryError::Io)?;
        f.lock().map_err(MemoryError::Io)?;
        Ok(f)
    }

    /// 读锁。中毒也继续（见模块文档）。
    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 写锁。中毒也继续。
    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 超限时淘汰一批最低频的条目。
    ///
    /// # 为什么按"衰减后的频次"而不是"次数"
    ///
    /// 只按次数淘汰，会让"三年前打过 1000 次、之后再没用过"挤掉
    /// "昨天打过 3 次"——与时间衰减的整个目的相反。
    ///
    /// # 确定性
    ///
    /// 排序键是 `(decayed_milli, 主键)`，而主键在表内唯一，
    /// 因此**全序、无平局**。同一份数据在同一时刻淘汰的必定是同一批条目。
    /// 两张表（输入 / 预测）各淘汰各的，用的是同一段代码。
    fn evict(inner: &mut Inner, cap: usize) {
        Self::evict_from(&mut inner.entries, cap);
    }

    /// 预测表的淘汰，与输入表同一段逻辑。
    fn evict_predictions(inner: &mut Inner, cap: usize) {
        Self::evict_from(&mut inner.predictions, cap);
    }

    /// 通用淘汰：把 `map` 降到约 `cap × 7/8`，丢弃衰减频次最低的那些。
    ///
    /// 泛型参数只有"键"——两张表的**值是同一个 `Entry`**，
    /// 因此"谁更冷"的判据只有一处实现。两处各写一遍的话，
    /// 改了一处漏一处会让其中一张表的淘汰行为悄悄变样。
    fn evict_from<K: Ord + Clone>(map: &mut BTreeMap<K, Entry>, cap: usize) {
        let target = cap - cap * (EVICT_DENOMINATOR - EVICT_NUMERATOR) / EVICT_DENOMINATOR;
        let target = target.max(1);
        let excess = map.len().saturating_sub(target);
        if excess == 0 {
            return;
        }

        let mut victims: Vec<(u64, &K)> =
            map.iter().map(|(key, e)| (e.decayed_milli, key)).collect();
        // O(n) 选出最低的 `excess` 个，而不是整表排序：
        // 这一步落在按键路径上，因此常数是重要的。
        if excess < victims.len() {
            victims.select_nth_unstable(excess - 1);
        }
        let doomed: Vec<K> = victims[..excess]
            .iter()
            .map(|(_, k)| (*k).clone())
            .collect();
        for key in doomed {
            map.remove(&key);
        }
    }
}

/// 临时文件路径：与目标同目录（**必须同文件系统**，否则 `rename` 不是原子的）。
///
/// 名字里带 pid 与进程内计数器。**不能固定为 `<name>.tmp`**：
/// 那样两个写者会往同一个临时文件里写，交错出一份谁的内容都不对的
/// 产物；而且 `<name>.tmp` 若恰好已存在（比如是个目录），
/// `write` 会稳定失败——旧实现正是被这一点卡住且**无法重试**。
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(
        ".tmp.{}.{}",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    path.with_file_name(name)
}

/// 进程内的临时文件计数器。
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 与数据文件同目录、名字稳定的锁文件。
fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    path.with_file_name(name)
}

/// 持有文件锁的守卫：`Drop` 时解锁。
///
/// 显式写它而不是靠 `File` 的析构，是因为"锁有没有被放掉"必须一眼可见：
/// 漏放的后果是**之后每一次落盘都永久卡住**，而输入法会安静地停止保存。
struct LockGuard<'a> {
    file: Option<&'a std::fs::File>,
}

impl Drop for LockGuard<'_> {
    fn drop(&mut self) {
        if let Some(f) = self.file.take() {
            let _ = f.unlock();
        }
    }
}

/// 写一个**只有所有者可读写**（Unix `0600`）的文件，并 `fsync` 它。
///
/// 记忆里有上屏文本、编码、上下文与时间——是敏感的本机行为历史。
/// 权限不是完整的隐私方案（父目录、备份、日志都在范围外），
/// 但"默认给同机其他用户可读"没有任何理由（P0-D 第 7 条）。
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), MemoryError> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).map_err(MemoryError::Io)?;
    f.write_all(bytes).map_err(MemoryError::Io)?;
    f.sync_all().map_err(MemoryError::Io)?;
    Ok(())
}

/// `rename` 之后把目录项也刷下去。
///
/// 只 `fsync` 文件是不够的：断电时目录项可能还没落盘，于是"改名成功"
/// 这个事实本身会丢。目录 fsync 在 Windows 上没有对应语义，跳过。
fn sync_dir(path: &Path) {
    #[cfg(unix)]
    {
        if let Some(dir) = path.parent() {
            if let Ok(d) = std::fs::File::open(dir) {
                let _ = d.sync_all();
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// 把盘上读到的一条记录**并**进内存表：逐字段取更大值。
///
/// 取更大值而不是相加：相加会让"同一份数据加载两次"翻倍，而取更大值是
/// **幂等**的——合并自己刚写的文件不改变任何数字，因此正常单写者场景下
/// 合并是恒等变换。多写者场景下它是并集，谁都不丢。
fn merge_entry<K: Ord>(map: &mut BTreeMap<K, Entry>, key: K, incoming: Entry) {
    match map.get_mut(&key) {
        Some(e) => {
            e.count = e.count.max(incoming.count);
            e.decayed_milli = e.decayed_milli.max(incoming.decayed_milli);
            e.last_used = e.last_used.max(incoming.last_used);
        }
        None => {
            map.insert(key, incoming);
        }
    }
}

impl From<Record> for Entry {
    fn from(r: Record) -> Self {
        Self {
            count: r.count,
            decayed_milli: r.decayed_milli,
            last_used: r.last_used,
        }
    }
}

impl From<PredRecord> for Entry {
    fn from(p: PredRecord) -> Self {
        Self {
            count: p.count,
            decayed_milli: p.decayed_milli,
            last_used: p.last_used,
        }
    }
}

/// 一个把"降级"路径也写清楚的占位时钟。
///
/// 降级得到的实例不会被查询（前端拿到的是一份空记忆），
/// 但 `FileMemory` 仍然需要一个 `Clock`。用一个**固定的 0** 而不是系统时钟，
/// 是为了让"降级"这条路径也完全可复现。
fn degraded_clock() -> Arc<dyn Clock> {
    Arc::new(qingjian_core::FrozenClock {
        secs: 0,
        ms: 0,
        offset_secs: 0,
    })
}

impl MemoryStore for FileMemory {
    fn record(&self, commit: &Commit) {
        // **按通道分流**（PLAN D42 / `docs/engine-design.md` §4.3.1），
        // 但两张表**不是互斥的**：
        //
        // | 上屏的通道 | 写输入表（键 = 编码） | 写预测表（键 = 上下文） |
        // | --- | --- | --- |
        // | `Lane::Input`  | ✅ | ✅ |
        // | `Lane::Predict` | ❌（它没有输入串） | ✅ |
        //
        // 只按 lane 二选一是**错的**，而且错得很安静：用户一个词一个词
        // 打出来的句子（那是绝大多数情况）全是 `Lane::Input`，
        // 于是预测表永远是空的——功能"接上了"却学不到任何东西。
        // 这是 P4b 最容易踩的一脚，`predict_next.rs` 的对比集守着它。
        match commit.lane {
            Lane::Input => {
                self.record_input(commit);
                self.record_prediction(commit);
            }
            Lane::Predict => self.record_prediction(commit),
            // `Lane` 是 `#[non_exhaustive]`（D30）：将来加通道时这里会**编译
            // 提醒**我们"新通道的记录该往哪张表写"。在决定之前**宁可不记**：
            // 记错了表会让一条记录永远检索不到（G10 那颗地雷的形态）。
            _ => {}
        }
    }

    fn lookup(&self, key: &str) -> Vec<MemoryEntry> {
        // **不要再规范化一次**：规范化的规则（去分隔符）会把编码键里的
        // `'` 吃掉，于是"存进去的"与"查出来的"变成两把不同的键。
        // 传入的键要么来自 `Commit::key`，要么来自调用方自己的
        // `normalize_key`（见 `MemoryRanker`）。
        if key.is_empty() {
            return Vec::new();
        }
        let key = key.to_owned();
        let now = self.clock.now_secs();
        let inner = self.read();
        let mut out = Vec::new();
        // 主键是 `(input, text)`，因此"某个输入的全部记录"是一段**连续区间**。
        // 上界用 `(key, String::new())`：任何 `text` 都 ≥ 空串。
        let lo = (key.clone(), String::new());
        for ((i, text), e) in inner.entries.range(lo..) {
            if i != &key {
                break;
            }
            out.push(MemoryEntry {
                input: i.clone(),
                text: text.clone(),
                count: e.count,
                // **查询时重算**：存下来的加成会随时间变旧，
                // 而"三年前打过的词"不该一直霸占第一位。
                bonus: bonus_now(e.decayed_milli, e.last_used, now),
                last_used: e.last_used,
            });
        }
        out
    }

    fn forget(&self, key: &str, text: &str) {
        // 与 `lookup` 同一条约定：`key` 已经是最终键。
        //
        // **只删输入表**：预测表的键是上下文，而 `ForgetRequested` 事件
        // 目前不带上下文（它带的是 `input`，预测候选的 `input` 是空串）。
        // 于是"删掉一条预测学习"暂时够不着——这是一处**已知的接口缺口**，
        // 不是"实现了但不生效"：`DeleteCandidate` 走的是键盘盲选，
        // 而预测候选根本不参与盲选（§4.3.1），所以这条路径不可达。
        let mut inner = self.write();
        if inner
            .entries
            .remove(&(key.to_owned(), text.to_owned()))
            .is_some()
        {
            inner.touch();
        }
    }

    fn predict_next(&self, context: &Context) -> Vec<Prediction> {
        let words = context.recent();
        let Some(last) = words.last() else {
            // 没上屏过任何词 ⇒ 没有任何上下文可依据。
            return Vec::new();
        };
        let now = self.clock.now_secs();
        let inner = self.read();

        // **最长上下文优先**（trigram → bigram 回退）。
        //
        // 为什么是"回退"而不是"插值"：插值需要为两段上下文各定一套权重，
        // 而那正是"配置数字的量纲"这类问题的温床。回退的语义只有一条
        // ——**更具体的搭配优先，没有就退到更泛的**——可以被一句话说清，
        // 也可以被一条测试钉死。
        if words.len() >= 2 {
            let prev = &words[words.len() - 2];
            let out = Self::collect_predictions(&inner.predictions, prev, last, now);
            if !out.is_empty() {
                return out;
            }
        }
        Self::collect_predictions(&inner.predictions, "", last, now)
    }
}

impl FileMemory {
    /// `Lane::Input` 的学习：键是**规范编码**（PLAN D42）。
    fn record_input(&self, commit: &Commit) {
        // **原样上屏不是"词"**：它是用户敲进去的那串字符本身（兜底候选、
        // 标点直出）。把它当词记下来，只会用"输入 = 词"的垃圾条目占内存。
        if commit.origin == Origin::Literal {
            return;
        }
        // **键优先取引擎算好的规范编码**（PLAN D42）：它已经"规范化"过了，
        // 再走一遍 `normalize_key` 只会把编码里的 `'` 吃掉，变成另一把键。
        // 只有拿不到编码时才退回按拼写规范化（原样上屏、标点、造句）。
        let key = match commit.key.as_deref() {
            Some(k) if !k.is_empty() => k.to_owned(),
            _ => normalize_key(&commit.input),
        };
        if key.is_empty() || commit.text.is_empty() {
            return;
        }

        let now = self.clock.now_secs();
        let mut inner = self.write();
        Self::bump(&mut inner.entries, (key, commit.text.clone()), now);
        inner.touch();

        if inner.entries.len() > self.cap {
            Self::evict(&mut inner, self.cap);
        }
    }

    /// `Lane::Predict` 的学习：键是**上下文**（P4b）。
    ///
    /// # 一次上屏写两条：bigram 与 trigram
    ///
    /// 于是"今天 微信" 与 "微信" 两条路都能查到，查询时**长的优先**。
    /// 代价是条目数按两次增长（见 [`DEFAULT_PREDICT_CAPACITY`]）。
    ///
    /// # 为什么键取 `commit.context` 而不是"刚才那条预测"
    ///
    /// `Commit::context` 是**上屏之前**的窗口（会话在 `finish_commit` 里
    /// 才把本次文本推进去），因此它正好是"产生这次上屏时的上下文"。
    /// 用预测结果本身当键会得到一条自我循环的记录（预测 A → 预测 A）。
    fn record_prediction(&self, commit: &Commit) {
        if commit.text.is_empty() || commit.origin == Origin::Literal {
            return;
        }
        let ctx = &commit.context;
        let n = ctx.len();
        if n == 0 {
            // 没有上下文就没有"下一词"可言（`Context::push` 丢弃空串，
            // 因此这里的空窗口是真实状态，不是数据错误）。
            return;
        }
        let now = self.clock.now_secs();
        let mut inner = self.write();
        // bigram：`ctx1` 用空串表示"只用了最近一个词"。
        Self::bump(
            &mut inner.predictions,
            (String::new(), ctx[n - 1].clone(), commit.text.clone()),
            now,
        );
        if n >= 2 {
            Self::bump(
                &mut inner.predictions,
                (ctx[n - 2].clone(), ctx[n - 1].clone(), commit.text.clone()),
                now,
            );
        }
        inner.touch();

        if inner.predictions.len() > self.predict_cap {
            Self::evict_predictions(&mut inner, self.predict_cap);
        }
    }

    /// 记一次使用：**先让旧账随时间衰减，再 `+1`**（RIME 的 `dee` 同一次序）。
    ///
    /// 两张表共用它——"一条记录怎么积累"只有一处定义。
    fn bump<K: Ord>(map: &mut BTreeMap<K, Entry>, key: K, now: u64) {
        let entry = map.entry(key).or_default();
        entry.decayed_milli = decayed_after_commit(entry.decayed_milli, entry.last_used, now);
        entry.count = entry.count.saturating_add(1);
        entry.last_used = now;
    }

    /// 查一段**确切**的上下文（`ctx1` 为空串 = bigram），返回分数降序的预测。
    ///
    /// # 先收集全部、再排序、再截断
    ///
    /// 这个次序是刻意的：**在排序之前截断会安静地丢掉分最高的那条**
    /// （HANDOFF §5 第 26 条的形态）。同一段上下文的后继词数量有限，
    /// 全收下来的代价可以接受。
    fn collect_predictions(
        map: &BTreeMap<(String, String, String), Entry>,
        ctx1: &str,
        ctx2: &str,
        now: u64,
    ) -> Vec<Prediction> {
        let lo = (ctx1.to_owned(), ctx2.to_owned(), String::new());
        let mut best: Vec<(Score, String)> = Vec::new();
        for ((c1, c2, text), e) in map.range(lo..) {
            if c1 != ctx1 || c2 != ctx2 {
                // `BTreeMap` 的区间是连续的：前缀一变，后面的都不属于这段上下文。
                break;
            }
            best.push((bonus_now(e.decayed_milli, e.last_used, now), text.clone()));
        }
        // 分数降序。同分时 `sort_by_key` 稳定 ⇒ 保留 range 给出的文本升序，
        // 因此结果**永远逐字节一致**（PLAN §5.2）。
        best.sort_by_key(|p| core::cmp::Reverse(p.0));
        best.truncate(MAX_PREDICTIONS);
        best.into_iter()
            .map(|(score, text)| Prediction {
                text,
                score,
                // 通用搭配表**不随项目分发**（HANDOFF §7.7.2 ④ 的所有者决定）：
                // 接口留着，但今天唯一的来源只能是用户自己的打字历史。
                origin: PredictionOrigin::Personal,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qingjian_core::{FrozenClock, SpellingAttr};

    const T0: u64 = 1_767_225_600;
    const DAY: u64 = 24 * 60 * 60;

    fn clock(secs: u64) -> Arc<dyn Clock> {
        Arc::new(FrozenClock {
            secs,
            ms: 0,
            offset_secs: 0,
        })
    }

    /// 一条**没有编码键**的上屏（走兜底路径：按拼写规范化）。
    fn commit(input: &str, text: &str) -> Commit {
        Commit {
            text: text.into(),
            input: input.into(),
            context: vec![],
            origin: Origin::SystemWord,
            attr: SpellingAttr::NORMAL,
            lane: Lane::Input,
            trigger: qingjian_core::Trigger::Space,
            key: None,
        }
    }

    /// 一条**带规范编码键**的上屏（引擎的真实形态）。
    fn commit_at(key: &str, text: &str) -> Commit {
        Commit {
            key: Some(key.into()),
            ..commit(key, text)
        }
    }

    /// 一条**预测候选的上屏**（P4b）：键是上下文，输入串恒为空。
    fn predict_commit(context: &[&str], text: &str) -> Commit {
        Commit {
            text: text.into(),
            input: String::new(),
            context: context.iter().map(|w| (*w).to_owned()).collect(),
            origin: Origin::Prediction,
            attr: SpellingAttr::NORMAL,
            lane: Lane::Predict,
            trigger: qingjian_core::Trigger::Explicit,
            key: None,
        }
    }

    /// 只用来喂 `predict_next` 的上下文窗口。
    fn ctx(words: &[&str]) -> Context {
        let mut c = Context::with_capacity(8);
        for w in words {
            c.push(*w);
        }
        c
    }

    fn store_at(secs: u64) -> FileMemory {
        FileMemory::in_memory(clock(secs), 100)
    }

    #[test]
    fn record_then_lookup_finds_it() {
        let m = store_at(T0);
        m.record(&commit("nihao", "你好"));
        let got = m.lookup("nihao");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "你好");
        assert_eq!(got[0].count, 1);
        assert!(got[0].bonus > qingjian_core::Score::ZERO);
    }

    #[test]
    fn a_recorded_key_is_always_findable() {
        // 不变式（G10 的全部内容）：**写进去的键一定查得回来**。
        //
        // 引擎给键时（`commit.key`），`record` 与 `lookup` 用的是**同一个
        // 字符串**，中间没有任何加工——所以这条是恒等式。
        let m = store_at(T0);
        m.record(&commit_at("ni'hao", "你好"));
        assert_eq!(m.lookup("ni'hao").len(), 1);
        // 而它**不会**被规范化成另一把键：`ni'hao` 与 `nihao` 是两把键。
        // （规范化只作用于"拿不到编码时的兜底路径"，见下一条测试。）
        assert!(m.lookup("nihao").is_empty());
    }

    #[test]
    fn the_fallback_spelling_key_is_normalized_and_findable() {
        // 拿不到编码时（造句、标点、原样上屏），`record` 按拼写规范化。
        // 那条路的不变式是：`lookup(normalize_key(原始拼写))` 必定命中。
        let m = store_at(T0);
        for raw in ["ni'hao", "NIHAO", "ni hao", " nihao "] {
            m.record(&commit(raw, "你好世界"));
        }
        // 四种写法是**同一把键**：一条记录、次数 4。
        let got = m.lookup(&normalize_key("nihao"));
        assert_eq!(got.len(), 1, "规范化之后应当只有一条记录，得到 {got:?}");
        assert_eq!(got[0].count, 4);
        for raw in ["ni'hao", "NIHAO", "ni hao", " nihao "] {
            assert_eq!(
                m.lookup(&normalize_key(raw)).len(),
                1,
                "lookup(normalize({raw:?})) 应当命中"
            );
        }
    }

    #[test]
    fn derived_spellings_are_learned_too() {
        // B′：所有上屏都学（HANDOFF §7.6.1 ② 的所有者决定）。
        let m = store_at(T0);
        let mut c = commit("nhao", "你好");
        c.attr = SpellingAttr::ABBREV;
        m.record(&c);
        assert_eq!(m.lookup("nhao").len(), 1);
    }

    #[test]
    fn literal_commits_are_not_words() {
        let m = store_at(T0);
        let mut c = commit("zzz", "zzz");
        c.origin = Origin::Literal;
        m.record(&c);
        assert!(m.is_empty(), "原样上屏不该占记忆条目");
    }

    #[test]
    fn commits_without_a_usable_key_are_ignored() {
        // 标点直出：没有当前输入，也无从规范化出键。
        let m = store_at(T0);
        m.record(&commit("", "，"));
        assert!(m.is_empty());
        // 预测候选：键是**上下文**，而这里上下文为空 —— 没有"下一词"可言。
        let m2 = store_at(T0);
        m2.record(&predict_commit(&[], "世界"));
        assert!(m2.is_empty(), "没有上下文的预测不该写成记录");
    }

    #[test]
    fn forgetting_removes_the_record() {
        let m = store_at(T0);
        m.record(&commit("nihao", "你好"));
        m.forget("nihao", "你好");
        assert!(m.lookup("nihao").is_empty());
        // RIME 的语义是"取消调频效果"，不是把系统词删掉——
        // 我们删的只是**这一条学习记录**，词库一行没动。
    }

    #[test]
    fn forgetting_uses_the_same_normalization() {
        let m = store_at(T0);
        m.record(&commit("ni'hao", "你好"));
        m.forget("nihao", "你好");
        assert!(m.lookup("nihao").is_empty());
    }

    #[test]
    fn bonus_grows_with_commits() {
        let single = store_at(T0);
        single.record(&commit("nihao", "你好"));
        let after_one = single.lookup("nihao")[0].bonus;

        let many = store_at(T0);
        for _ in 0..5 {
            many.record(&commit("nihao", "你好"));
        }
        let after_five = many.lookup("nihao")[0].bonus;
        assert!(after_five > after_one, "打过 5 次应当比 1 次加成更高");
    }

    #[test]
    fn time_decays_the_bonus_across_reloads() {
        // 时间衰减的端到端形态：**同一份落盘数据**，用不同的"现在"去读，
        // 加成不同。这也顺带测了持久化——数据是真的从文件回来的。
        let dir = std::env::temp_dir().join(format!("qingjian-mem-decay-{}", std::process::id()));
        let path = dir.join("user.mem");
        let _ = std::fs::remove_file(&path);

        {
            let m = FileMemory::open(&path, clock(T0), 100).unwrap();
            for _ in 0..3 {
                m.record(&commit("nihao", "你好"));
            }
            assert!(m.flush().unwrap());
        }

        let fresh = FileMemory::open(&path, clock(T0 + DAY), 100).unwrap();
        let stale = FileMemory::open(&path, clock(T0 + 365 * DAY), 100).unwrap();
        let f = fresh.lookup("nihao")[0].bonus;
        let s = stale.lookup("nihao")[0].bonus;
        assert!(f > s, "一年前的记录不该和昨天的一样重（{f:?} vs {s:?}）");
        assert_eq!(
            s,
            qingjian_core::Score::ZERO,
            "一年 ≈ 12 个半衰期，应当衰减干净"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn eviction_is_deterministic_and_drops_the_coldest() {
        // 上限 8、批量目标 7。先打 4 条"热"的（各 3 次），再打 6 条"冷"的。
        let build = || {
            let m = FileMemory::in_memory(clock(T0), 8);
            for i in 0..4 {
                for _ in 0..3 {
                    m.record(&commit(&format!("in{i}"), &format!("词{i}")));
                }
            }
            for i in 4..10 {
                m.record(&commit(&format!("in{i}"), &format!("词{i}")));
            }
            m.snapshot().into_iter().map(|e| e.text).collect::<Vec<_>>()
        };
        let a = build();
        let b = build();
        assert_eq!(a, b, "淘汰必须确定：同一份数据两次运行结果一致");
        assert!(a.len() <= 8, "上限没守住：{a:?}");
        // 冷条目（各只打过 1 次）先被淘汰，热条目必须留下。
        for hot in ["词0", "词1", "词2", "词3"] {
            assert!(
                a.contains(&hot.to_owned()),
                "热条目 {hot} 不该被淘汰：{a:?}"
            );
        }
        assert!(
            a.len() < 10,
            "冷条目必须开始被淘汰（否则上限形同虚设）：{a:?}"
        );
    }

    #[test]
    fn capacity_is_respected() {
        let m = FileMemory::in_memory(clock(T0), 32);
        for i in 0..200 {
            m.record(&commit(&format!("in{i}"), &format!("词{i}")));
        }
        assert!(m.len() <= 32, "条目数 {} 超过上限", m.len());
    }

    // ─────────────────────────────────────────────────────────────────────
    // 预测表（P4b）
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn an_empty_context_yields_no_predictions() {
        let m = store_at(T0);
        m.record(&predict_commit(&["微信"], "朋友圈"));
        // 学到了东西，但查询时上下文为空 —— 没有"上一个词"就没有下一词。
        assert!(m.predict_next(&Context::default()).is_empty());
    }

    #[test]
    fn a_learned_pair_predicts_the_next_word() {
        let m = store_at(T0);
        m.record(&predict_commit(&["微信"], "朋友圈"));
        let got = m.predict_next(&ctx(&["微信"]));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "朋友圈");
        assert_eq!(got[0].origin, PredictionOrigin::Personal);
        assert!(got[0].score > Score::ZERO, "学过一次就该有正分");
    }

    #[test]
    fn trigram_wins_over_bigram_when_both_are_known() {
        // "微信 → 朋友圈"（学过 3 次）与 "今天 微信 → 文件传输助手"（1 次）。
        //
        // 注意第二条记录**同时**写进了 bigram（`微信 → 文件传输助手`），
        // 因为一次上屏写两条（更具体的与更泛的）。
        let m = store_at(T0);
        for _ in 0..3 {
            m.record(&predict_commit(&["微信"], "朋友圈"));
        }
        m.record(&predict_commit(&["今天", "微信"], "文件传输助手"));

        // 上下文是 `今天 微信` ⇒ 更具体的那条胜出，**与它的分数无关**。
        let after_today = m.predict_next(&ctx(&["今天", "微信"]));
        assert_eq!(after_today[0].text, "文件传输助手");
        assert_eq!(after_today.len(), 1, "trigram 有记录时不该再混入 bigram");

        // 上下文只有一个词 ⇒ 退到 bigram，此时按分数（3 次 > 1 次）排序。
        let only_wechat = m.predict_next(&ctx(&["微信"]));
        assert_eq!(only_wechat[0].text, "朋友圈");
    }

    #[test]
    fn backoff_only_kicks_in_when_the_longer_context_is_empty() {
        // 反面：trigram **没有**记录时，不该凭空造一条，而应退回 bigram。
        let m = store_at(T0);
        m.record(&predict_commit(&["微信"], "朋友圈"));
        let got = m.predict_next(&ctx(&["今天", "微信"]));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "朋友圈");
    }

    #[test]
    fn prediction_and_input_tables_do_not_mix() {
        // 两套键空间隔离：编码键查不到预测记录，反之亦然。
        let m = store_at(T0);
        m.record(&commit_at("wei'xin", "微信"));
        m.record(&predict_commit(&["微信"], "朋友圈"));

        // 输入表里没有 `微信` 这条键（它是上下文，不是编码）。
        assert!(m.lookup("微信").is_empty());
        // 预测表里也没有 `wei'xin` 这段上下文。
        assert!(m.predict_next(&ctx(&["wei'xin"])).is_empty());
        // 两张表各自都真的学到了东西。
        assert_eq!(m.lookup("wei'xin").len(), 1);
        assert_eq!(m.predict_next(&ctx(&["微信"])).len(), 1);
    }

    #[test]
    fn stronger_pairs_rank_first() {
        // 同一段上下文下的多个后继词，按分数（次数 + 衰减）降序。
        let m = store_at(T0);
        for _ in 0..3 {
            m.record(&predict_commit(&["今天"], "天气"));
        }
        m.record(&predict_commit(&["今天"], "很热"));
        let got = m.predict_next(&ctx(&["今天"]));
        assert_eq!(got[0].text, "天气", "学过 3 次的该排在 1 次的前面");
        // 顺序**确定**：连查 100 次逐字节一致（PLAN §5.2）。
        let first: Vec<String> = got.into_iter().map(|p| p.text).collect();
        for _ in 0..100 {
            let again: Vec<String> = m
                .predict_next(&ctx(&["今天"]))
                .into_iter()
                .map(|p| p.text)
                .collect();
            assert_eq!(again, first, "预测顺序不可复现");
        }
    }

    #[test]
    fn predictions_are_capped() {
        let m = store_at(T0);
        for i in 0..MAX_PREDICTIONS * 3 {
            m.record(&predict_commit(&["上"], &format!("词{i}")));
        }
        assert_eq!(
            m.predict_next(&ctx(&["上"])).len(),
            MAX_PREDICTIONS,
            "返回条数必须受限"
        );
    }

    #[test]
    fn prediction_records_decay_with_time() {
        // 同一份"学过 1 次"的记录，最后使用时间不同 ⇒ 分数不同。
        let with_last_used = |last_used: u64| {
            let m = FileMemory::in_memory(clock(T0), 100);
            {
                let mut inner = m.write();
                inner.predictions.insert(
                    (String::new(), "微信".into(), "朋友圈".into()),
                    Entry {
                        count: 1,
                        decayed_milli: crate::decay::DECAY_UNIT_MILLI,
                        last_used,
                    },
                );
            }
            m.predict_next(&ctx(&["微信"]))
        };
        let fresh = with_last_used(T0 - DAY);
        let stale = with_last_used(T0 - 365 * DAY);
        assert!(fresh[0].score > Score::ZERO, "昨天学过的该有正分");
        // 一年 ≈ 12 个半衰期 ⇒ 1.0 次只剩 1/4096，整数衰减到 0。
        assert_eq!(stale[0].score, Score::ZERO, "一年前学过的该衰减干净");
        assert!(fresh[0].score > stale[0].score);
    }

    #[test]
    fn prediction_eviction_is_deterministic_and_drops_the_coldest() {
        // 预测上限 8、批量目标 7：先学 4 条热的（各 3 次），再学 6 条冷的。
        let build = || {
            let m = FileMemory::in_memory_with(clock(T0), 100, 8);
            for i in 0..4 {
                for _ in 0..3 {
                    m.record(&predict_commit(&["上"], &format!("热{i}")));
                }
            }
            for i in 4..10 {
                m.record(&predict_commit(&["上"], &format!("冷{i}")));
            }
            m.prediction_snapshot()
                .into_iter()
                .map(|e| e.text)
                .collect::<Vec<_>>()
        };
        let a = build();
        let b = build();
        assert_eq!(a, b, "淘汰必须确定：同一份数据两次运行结果一致");
        assert!(a.len() <= 8, "上限没守住：{a:?}");
        for hot in ["热0", "热1", "热2", "热3"] {
            assert!(
                a.contains(&hot.to_owned()),
                "热条目 {hot} 不该被淘汰：{a:?}"
            );
        }
    }

    #[test]
    fn predictions_survive_flush_and_reload() {
        let dir = std::env::temp_dir().join(format!("qingjian-pred-{}", std::process::id()));
        let path = dir.join("user.mem");
        let _ = std::fs::remove_file(&path);

        {
            let m = FileMemory::open(&path, clock(T0), 100).unwrap();
            m.record(&commit_at("wei'xin", "微信"));
            m.record(&predict_commit(&["微信"], "朋友圈"));
            assert!(m.flush().unwrap());
        }

        let reloaded = FileMemory::open(&path, clock(T0), 100).unwrap();
        let got = reloaded.predict_next(&ctx(&["微信"]));
        assert_eq!(got.len(), 1, "预测表没有从文件里读回来");
        assert_eq!(got[0].text, "朋友圈");
        // 输入表也还在（两张表在同一个文件里分段，一起写一起读）。
        assert_eq!(reloaded.lookup("wei'xin").len(), 1);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn flush_writes_and_reload_restores() {
        let dir = std::env::temp_dir().join(format!("qingjian-mem-test-{}", std::process::id()));
        let path = dir.join("user.mem");
        let _ = std::fs::remove_file(&path);

        let m = FileMemory::open(&path, clock(T0), 100).expect("首次打开应当是空表");
        assert!(!m.is_dirty());
        assert!(!m.flush().expect("空表落盘应当成功"), "没有改动时不该写盘");

        m.record(&commit("nihao", "你好"));
        assert!(m.is_dirty());
        assert!(m.flush().expect("落盘"));

        let reloaded = FileMemory::open(&path, clock(T0 + DAY), 100).expect("应当能读回");
        let got = reloaded.lookup("nihao");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "你好");
        assert_eq!(got[0].count, 1);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn a_corrupt_file_degrades_instead_of_failing() {
        let dir = std::env::temp_dir().join(format!("qingjian-mem-corrupt-{}", std::process::id()));
        let path = dir.join("user.mem");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, b"\x00\x01\x02 not a memory file").unwrap();

        let (m, warn) = FileMemory::open_or_degrade(&path, clock(T0), 100);
        let warn = warn.expect("坏文件必须给出警告（D26：降级要看得见）");
        assert!(warn.contains("不可用"), "警告应当说清发生了什么：{warn}");
        assert!(m.is_empty());
        // 降级后**不可写**：绝不覆盖一个读不懂的文件。
        assert!(!m.is_writable());
        m.record(&commit("nihao", "你好"));
        assert!(!m.flush().unwrap(), "降级实例不该落盘");
        // 原文件原封不动。
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"\x00\x01\x02 not a memory file"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn normalization_keeps_chinese_and_drops_separators() {
        assert_eq!(normalize_key("ni'hao"), "nihao");
        assert_eq!(normalize_key("Ni Hao"), "nihao");
        assert_eq!(normalize_key("你好"), "你好");
        assert_eq!(normalize_key(""), "");
    }
}
