//! # Service traits
//!
//! 中文职责：外部资源与可替换能力的接口定义。
//! English role: interfaces for external resources and swappable capabilities.
//! 架构位置：qingjian-core 的"服务定义"一侧；由 `qingjian-engine` / `qingjian-memory` /
//! `qingjian-dict` 等提供实现，在**组件构造时注入**（不穿过 `Query`）。
//!
//! PLAN §2.5：外部资源（存储、模型、时钟、随机数）一律通过 trait 注入，
//! 保证引擎不直接读文件、时钟或网络，从而可纯 `cargo test`。

use crate::candidate::{Candidate, CandidateSink};
use crate::commit::Commit;
use crate::context::Context;
use crate::score::Score;
use std::sync::Arc;

/// 编码单元的编号。
///
/// **通用类型**：拼音方案下每个编号对应一个音节（`ni`、`hao`）；
/// 仓颉方案下每个编号对应一个字母（一个字根）。引擎不关心是哪种。
///
/// （RIME 源码里这个类型叫 `SyllableId`——那是历史遗留用词，
/// 因为 RIME 诞生于拼音方案。见 `docs/engine-design.md` §1 的术语对照。）
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct CodeUnitId(pub u32);

/// 某方案允许出现的全部编码单元。
///
/// **它的内容是方案数据**（拼音方案的音节表 / 仓颉方案的字母表），
/// 引擎里不内置任何一份。
#[derive(Clone, Debug, Default)]
pub struct CodeAlphabet {
    /// 编码单元的字面写法，下标即其 [`CodeUnitId`]。
    units: Vec<String>,
}

impl CodeAlphabet {
    /// 由字面写法列表构造。
    #[must_use]
    pub fn new(units: Vec<String>) -> Self {
        Self { units }
    }

    /// 编码单元的数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.units.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    /// 取某个编码单元的字面写法。
    #[must_use]
    pub fn text(&self, id: CodeUnitId) -> Option<&str> {
        self.units.get(id.0 as usize).map(String::as_str)
    }

    /// 按字面写法查编号（时间复杂度 `O(n)`；加载期用，运行期应预先解析成编号）。
    #[must_use]
    pub fn id_of(&self, text: &str) -> Option<CodeUnitId> {
        self.units
            .iter()
            .position(|u| u == text)
            .map(|i| CodeUnitId(u32::try_from(i).unwrap_or(u32::MAX)))
    }
}

/// 词库：把**编码**映射到词条。
///
/// **精确匹配**：变体拼写（简拼 / 模糊音 / 补全 / 纠错）已经在拼写层
/// 被展开成不同的编码，到这里就已经是确定的一条编码了。
///
/// **一个方案可以装载多个词库**（rime-ice 就挂了 4 个以上），
/// 每个词库注入到需要它的那个翻译器。"引擎里有几个词库"是方案的自由。
pub trait Lexicon: Send + Sync {
    /// 查一个编码序列对应的词条。
    fn lookup(&self, code: &[CodeUnitId], out: &mut CandidateSink<'_>);

    /// 查**以 `prefix` 开头**的全部编码（词条补全）。
    ///
    /// # 为什么它是 trait 上的一个方法，而不是"翻译器自己扫"
    ///
    /// 因为"前缀是一段连续区间"这件事**只有词库实现自己知道**：
    /// 内存表靠有序 map 的 `range`，紧凑表靠编码段的偏移数组，
    /// 未来的 mmap 实现靠页索引。翻译器如果自己去遍历，就等于
    /// 假定"词库能被顺序扫描"——而那是**存储格式的细节**，
    /// 正是 `Lexicon` 这个 trait 要挡住的东西（P2.5 的兑现点）。
    ///
    /// # 默认实现：**什么都不返回**
    ///
    /// 这条默认值是刻意的：补全是**可选能力**（RIME 的
    /// `enable_word_completion` 默认就是关的）。不支持前缀查询的
    /// 词库退回"查不到"是正确行为——**比"假装支持"好得多**：
    /// 后者会让用户看到"有的词能补全、有的不能"，而那无法排查。
    ///
    /// # Arguments / 参数
    /// * `prefix` — 编码前缀。
    /// * `exclude_exact` — 是否跳过**恰好等于**前缀的那些词条。
    ///   翻译器已经精确查过一次了，补全只该给出"更长"的那些。
    /// * `out` — 候选写这里。属性的 `COMPLETION` 位由**实现方**负责打上
    ///   （它知道这些候选是补出来的）。
    fn prefix_lookup(
        &self,
        prefix: &[CodeUnitId],
        exclude_exact: bool,
        out: &mut CandidateSink<'_>,
    ) {
        let _ = (prefix, exclude_exact, out);
    }

    /// 本词库是否支持[前缀查询](Lexicon::prefix_lookup)。
    ///
    /// 翻译器用它决定要不要尝试补全——省掉一次必然落空的调用，
    /// 也让 `--dump-config` 能如实报告"这个方案开了补全，但词库不支持"。
    fn supports_prefix(&self) -> bool {
        false
    }

    /// 是否存在**以 `code` 为前缀**的词库编码（含恰好相等）？
    ///
    /// # 它回答的是"要不要继续往下搜"，不是"有哪些词"
    ///
    /// 拼写展开的绝大部分分支**从一开始就不可能命中任何词条**。
    /// 先用这个问题剪掉它们，搜索空间就从"所有切分"塌缩成
    /// "真的有词的切分"——这是审计 §2.A 要求的**词典约束搜索**。
    ///
    /// # 调用方必须先问 `supports_prefix()`
    ///
    /// 默认实现返回 `false`（"不支持"和"不存在"在类型上无法区分）。
    /// **把"不支持"误读成"不存在"会静默丢掉全部候选**，所以调用方
    /// 只在 `supports_prefix()` 为真时才用它剪枝。
    fn has_prefix(&self, code: &[CodeUnitId]) -> bool {
        let _ = code;
        false
    }
}

/// 一条展开出来的编码切分：编码 + 代价 + 属性。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expansion {
    /// 展开出的编码。
    pub code: Vec<CodeUnitId>,
    /// 相对规范拼写的**代价**（对数域，通常 ≤ 0）。
    ///
    /// 简拼 / 模糊音 / 补全 / 纠错的"罚分"都在这里，由**方案数据**给出。
    /// 引擎不认识"简拼"这个词，它只知道"这条边有一个代价"。
    pub cost: Score,
    /// 这条边经过了哪些变形（可叠加）。
    pub attr: crate::candidate::SpellingAttr,
}

/// 展开结果的写入缓冲，同样限流。
pub struct ExpansionSink<'a> {
    buf: &'a mut Vec<Expansion>,
    cap: usize,
}

impl<'a> ExpansionSink<'a> {
    /// 构造。
    pub fn new(buf: &'a mut Vec<Expansion>, cap: usize) -> Self {
        Self { buf, cap }
    }

    /// 推入一条展开；超出上限则丢弃。
    pub fn push(&mut self, e: Expansion) {
        if self.buf.len() < self.cap {
            self.buf.push(e);
        }
    }

    /// 已写入数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// 剩余配额。
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.cap.saturating_sub(self.buf.len())
    }
}

/// 拼写层（棱镜）：把**用户敲出来的拼写**映射到**编码**，并给出代价与属性。
///
/// # 为什么它与 [`Lexicon`] 必须分开（G2）
///
/// RIME 允许两者独立替换——双拼方案的经典做法是「**用全拼的词库，
/// 只换拼写规则**」（`reverse_lookup` 就有独立的 `dictionary` **和** `prism`）。
/// 旧设计把 `alphabet()` 放在 `Lexicon` 上，等于宣称"换拼写就必须换词库"，
/// **把两个可以自由组合的东西焊死了**。
///
/// 另外 RIME 的 `speller/algebra` **只作用于拼写法**（有效拼写集合 →
/// 编码集合的映射），从不作用于词条本身——所以它属于这一层。
pub trait Spelling: Send + Sync {
    /// 编码字母表。
    fn alphabet(&self) -> &CodeAlphabet;

    /// 把一段拼写展开成所有可能的编码切分（带代价与属性）。
    ///
    /// 这是**通用最短路算法**的输入：简拼 / 模糊音 / 补全 / 纠错在这里变成
    /// "带代价的边"，而引擎并不认识这些名字。
    fn expand(&self, spelling: &str, out: &mut ExpansionSink<'_>);

    /// 把拼写展开成**带消费长度的路径**——包括"只解释了前缀"的那些。
    ///
    /// # 为什么需要它（审计 §2.E 的第 ① 条通路）
    ///
    /// librime 的音节图有 `input_length` 与 `interpreted_length` **两个数**：
    /// 图可以只覆盖输入的前缀，查表在那个前缀上做，剩下的字符留在输入里
    /// （`algo/syllabifier.cc:267-274`）。这正是 `niha` 能给出「你好」的机制：
    /// 图只覆盖 `ni` + `h`（`h` 是 `hao` 的缩写），第 4 个字符 `a`
    /// 从未被消费。
    ///
    /// [`Spelling::expand`] 只产出"恰好消费完整个输入"的路径，
    /// 所以它**表达不了**这件事。
    ///
    /// # 默认实现
    ///
    /// 退化成 [`Spelling::expand`] 并把 `consumed` 填成输入长度。
    /// 语义正确（那些路径确实消费了整串），只是没有前缀路径。
    fn expand_paths(&self, spelling: &str, _limits: PathLimits, out: &mut PathSink<'_>) {
        let mut exps = Vec::new();
        {
            let mut sink = ExpansionSink::new(&mut exps, out.remaining().max(1));
            self.expand(spelling, &mut sink);
        }
        for e in exps {
            out.push(SpellingPath {
                code: e.code,
                consumed: spelling.len(),
                cost: e.cost,
                attr: e.attr,
            });
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 带消费长度的拼写路径
// ─────────────────────────────────────────────────────────────────────────────

/// 一条拼写路径：**编码 + 消费了多少输入** + 代价 + 属性。
///
/// `consumed < spelling.len()` 表示这条路径只解释了输入的**前缀**——
/// 剩下的字符是**余码**，前端应当让它继续留在预编辑串里。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpellingPath {
    /// 展开出的编码。
    pub code: Vec<CodeUnitId>,
    /// 消费掉的输入**字节**数（≤ 输入长度）。
    pub consumed: usize,
    /// 相对规范拼写的代价（对数域，通常 ≤ 0）。
    pub cost: Score,
    /// 这条路径经过了哪些变形。
    pub attr: crate::candidate::SpellingAttr,
}

impl SpellingPath {
    /// 余码长度（给定输入总长）。
    #[must_use]
    pub fn remainder_len(&self, input_len: usize) -> usize {
        input_len.saturating_sub(self.consumed)
    }
}

/// 一次前缀展开的**硬预算**。
///
/// 与 [`ExpansionSink`] 的容量是两件事：那个限制**写出多少条**，
/// 这个限制**搜索花多少**。造句要为每个起始位置各展开一次，
/// 所以那里必须用一个比"整串展开"小得多的预算。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathLimits {
    /// 一次最多产出多少条路径。
    pub max_paths: usize,
    /// 搜索图最多几个状态。
    pub max_states: usize,
    /// 最多尝试多少次边。
    pub max_work: usize,
    /// **拼写层补全**：允许最后一条边"吃掉"没敲完的尾巴吗？
    ///
    /// 打开时，若剩余输入 `ha` 是某个拼写 `hao` 的**前缀**，那条边也可以走，
    /// 并把它标成 [`crate::SpellingAttr::COMPLETION`]。这正是 librime 的
    /// `Prism::ExpandSearch`（`algo/syllabifier.cc:224-228`）：
    /// 补全发生在**拼写层**，所以 `ha`（不是一个合法音节）也能被补成 `hao`。
    ///
    /// 关掉时只有"输入是边的完整匹配"才成立——那是补全关闭时该有的行为。
    pub completion: bool,
}

impl Default for PathLimits {
    fn default() -> Self {
        Self {
            max_paths: 512,
            max_states: 16_384,
            max_work: 131_072,
            completion: false,
        }
    }
}

impl PathLimits {
    /// 三项预算显式给出；补全默认关闭。
    #[must_use]
    pub const fn new(max_paths: usize, max_states: usize, max_work: usize) -> Self {
        Self {
            max_paths,
            max_states,
            max_work,
            completion: false,
        }
    }

    /// 打开/关闭拼写层补全。
    #[must_use]
    pub const fn with_completion(mut self, on: bool) -> Self {
        self.completion = on;
        self
    }

    /// 用于"每个起始位置各展开一次"的小预算（造句）。
    ///
    /// 取值理由：造句只有在**没有精确整词匹配**时才会跑，而那时输入通常
    /// 只有两三个音节。`256` 个状态足够覆盖"几个音节的少量切分"，
    /// 同时把 `起始位置数 × 状态上界` 压在几千以内。
    #[must_use]
    pub const fn sentence_scan() -> Self {
        Self {
            max_paths: 48,
            max_states: 256,
            max_work: 2_048,
            completion: false,
        }
    }
}

/// 路径写入缓冲，限流。
pub struct PathSink<'a> {
    buf: &'a mut Vec<SpellingPath>,
    cap: usize,
}

impl<'a> PathSink<'a> {
    /// 构造。
    pub fn new(buf: &'a mut Vec<SpellingPath>, cap: usize) -> Self {
        Self { buf, cap }
    }

    /// 推入一条；超出上限则丢弃。
    pub fn push(&mut self, p: SpellingPath) {
        if self.buf.len() < self.cap {
            self.buf.push(p);
        }
    }

    /// 已写入数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// 剩余配额。
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.cap.saturating_sub(self.buf.len())
    }
}

/// 重排器：记忆、上下文、向量都实现它。
pub trait Ranker: Send + Sync {
    /// 允许改分、允许重排；**不允许凭空生成候选**，
    /// 也不允许把非猜测候选排到猜测候选之后。
    ///
    /// 实现者应当只**加**一个有界的分数（见 [`Ranker::bonus_limit`]）。
    fn rerank(&self, q: &QueryView<'_>, cands: &mut Vec<Candidate>);

    /// 本重排器单次能给某个候选加的最大分数。
    ///
    /// # 它能保证什么，不能保证什么
    ///
    /// 上界 `L` 保证的是"猜测候选最多升 `L`"。**它本身不足以消除倒置**：
    /// 只有基础分差距大于 `L` 时倒置才不可能；差距更小时仍可能发生。
    ///
    /// 想靠算术完全消除倒置，只有"给非猜测候选一个类别偏置"一途——
    /// 而那正是被否决的分区方案（它会损害排序质量）。
    ///
    /// **所以上界只是第一道防线，[`crate::sort::has_cross_class_inversion`]
    /// 那样的守卫不是可选项。** 见 `docs/engine-design.md` §5.3.2。
    fn bonus_limit(&self) -> Score;
}

/// 重排器看到的只读视图。
///
/// 与 [`crate::component::Query`] 分开，因为重排发生在**翻译与过滤之后**，
/// 此时关心的是"输入 + 上下文"，而不是切分细节。
pub struct QueryView<'a> {
    /// 当前输入串。
    pub input: &'a str,
    /// 最近已上屏的词。
    pub context: &'a Context,
    /// 本次查询所属的通道。
    pub lane: crate::candidate::Lane,
}

/// 时钟。引擎不直接读系统时间——注入它才能在测试里"定格时间"，
/// 从而稳定测试时间衰减逻辑。
pub trait Clock: Send + Sync {
    /// 当前 Unix 时间戳（秒）。
    fn now_secs(&self) -> u64;
    /// 单调毫秒计时（用于 `Session::tick` 的时序判定）。
    fn now_ms(&self) -> u64;

    /// 本地时区相对 UTC 的偏移（**秒**，东八区是 `+28800`）。
    ///
    /// # 为什么它在 trait 上（一个"时区是平台知识"的边界）
    ///
    /// "日期/时间/星期"这类候选需要**本地日历日**，而本地日历日 = UTC 时刻
    /// ＋时区偏移。时区规则是平台与系统的知识（`TZ` 环境变量、注册表、
    /// `zoneinfo`），而**引擎不许读平台**——它只认识 trait。
    ///
    /// 默认实现返回 `0`（即"按 UTC 报时"）：这是一个**诚实的缺省**，
    /// 而不是伪装成"本地时间"。实现了它的时钟（真实前端）给出正确偏移；
    /// 没实现的（测试）得到确定性的结果——**这正是可复现所需的**。
    ///
    /// 注意它**不含夏令时**：偏移是"此刻"的，不是"任意时刻"的。
    /// 对"现在几点"够用；要算历史日期的时区得引入真正的时区库，
    /// 而那会撞内存红线（`tzdata` 是几 MB 的表），故不做。
    fn utc_offset_secs(&self) -> i64 {
        0
    }
}

/// 随机数源。
///
/// # 为什么它要注入（而不是直接调 `rand`）
///
/// 两条理由，各自独立成立：
///
/// 1. **零依赖**：`qingjian-core` / `qingjian-engine` 不许有第三方依赖
///    （`verify-zero-deps.sh` 强制），`rand` 进不来。
/// 2. **可复现**：RIME 的 UUID 插件用 `math.random`，而"候选列表是
///    (输入, 状态) 的纯函数"是我们的铁律——**随机候选天然破坏它**。
///    注入之后，测试能给一个确定性的发生器，于是"敲 uuid 得到什么"
///    可以被断言；生产环境注入真随机的那个。
///
/// 这是本项目的第 N 次同一个模式：**外部不确定性一律注入**。
/// 时钟（[`Clock`]）、内存（[`MemoryStore`]）、随机数，三者一视同仁。
pub trait RandomSource: Send + Sync {
    /// 产生一个 64 位随机数。
    ///
    /// **只给这一个方法**：`next_u64` 足够派生出任何需要的东西
    /// （UUID 的 16 字节、洗牌、抽样），而接口越小，实现越容易正确。
    fn next_u64(&mut self) -> u64;
}

/// 一个**真随机**的随机源（生产装配处用）。
///
/// # 它凭什么不需要第三方依赖
///
/// `std` 的 [`std::collections::hash_map::RandomState`] 在**每个进程**
/// 启动时从操作系统取一次随机种子（这是 `HashMap` 抗哈希碰撞攻击的机制）。
/// 拿它当键、拿一个自增计数器当消息做一次哈希，就得到一条**不可预测**
/// （跨进程）且互不重复（进程内）的流。
///
/// # 它不是什么（诚实交代）
///
/// **它不是密码学安全的**：计数器是公开的，安全性完全落在 `RandomState`
/// 那份每进程密钥上。用它生成 UUID 够用——UUID 在这里是**用户要打出来的
/// 一段文本**，不是安全令牌。真需要密码学随机时，前端应当注入自己的实现
/// （`RandomSource` 是个 trait，这正是它的用途）。
#[derive(Debug, Clone)]
pub struct SystemRandom {
    /// 每进程随机的哈希键。
    state: std::collections::hash_map::RandomState,
    /// 进程内单调递增的计数器。
    counter: u64,
}

impl SystemRandom {
    /// 构造。
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: std::collections::hash_map::RandomState::new(),
            counter: 0,
        }
    }
}

impl Default for SystemRandom {
    fn default() -> Self {
        Self::new()
    }
}

impl RandomSource for SystemRandom {
    fn next_u64(&mut self) -> u64 {
        use std::hash::{BuildHasher, Hasher};
        self.counter = self.counter.wrapping_add(1);
        let mut h = self.state.build_hasher();
        h.write_u64(self.counter);
        h.finish()
    }
}

/// 一个**确定性**的随机源（测试与 `--dump-config` 用）。
///
/// 它是 splitmix64——一个短小、无依赖、分布够好的发生器。
/// **它的输出可预测，因此绝不该用于生产**：名字里的 `Deterministic`
/// 就是给使用者看的警告。
#[derive(Debug, Clone, Copy)]
pub struct DeterministicRandom {
    state: u64,
}

impl DeterministicRandom {
    /// 由种子构造。**同一个种子永远给出同一串数**。
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }
}

impl RandomSource for DeterministicRandom {
    fn next_u64(&mut self) -> u64 {
        // splitmix64：作者是 Sebastiano Vigna，公共领域。
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// 一条已学记录。`(输入, 词)` 是主键。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryEntry {
    /// **规范编码**（见 [`MemoryStore::record`] 的警告）。
    pub input: String,
    /// 上屏的文本。
    pub text: String,
    /// 被上屏过几次。
    pub count: u32,
    /// 由"次数 + 时间衰减"算出的**对数域加成**，已可直接加到候选分数上。
    ///
    /// **为什么不是 `f32`**：它会影响候选顺序，因此**必须可复现**——
    /// 浮点在不同平台 / 不同 libm 上可能差 1 ULP（见 [`Score`] 的说明）。
    /// "次数与时间戳 → 本字段"的换算由实现方在**更新时**完成，
    /// 那一步可以用浮点；但**存下来的必须是定点**。
    pub bonus: Score,
    /// 最后一次使用时间（秒）。
    pub last_used: u64,
}

/// 一条预测结果（下一词预测，`Lane::Predict`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prediction {
    /// 预测的文本。
    pub text: String,
    /// 对数域分数，**已与 `Lane::Input` 的分数可比**
    /// （换算责任在这里，不在排序处）。
    pub score: Score,
    /// 来源说明，用于 UI 区分标记。
    pub origin: PredictionOrigin,
}

/// 预测的来源。
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum PredictionOrigin {
    /// 来自用户自己的打字历史。
    Personal,
    /// 来自随项目分发的通用搭配表（是数据，不是网络调用）。
    General,
}

/// 用户记忆。
pub trait MemoryStore: Send + Sync {
    /// 记录一次上屏。**必须是异步 / 非阻塞路径。**
    ///
    /// 按 `commit.lane` 分流：`Lane::Input` 用**编码键**作主键，
    /// `Lane::Predict` 用 `context` 作主键。
    ///
    /// # ⚠️ 主键必须是**规范编码**（G10 / PLAN D42）
    ///
    /// RIME 明确警告过一颗地雷：
    ///
    /// > 凡是編碼爲源碼表中未出現過的形式，如通過「拼寫運算」實現的
    /// > **簡拼、異拼**，又如編碼中的**拼寫錯誤**，都將導致該條記錄成爲
    /// > **用戶詞典中的無效數據**，因爲無法通過正常的輸入檢索到。
    ///
    /// 原因在于**投影是单向的**：词库的键永远是**规范编码**（`ni hao`），
    /// 简拼（`nh`）只是**到达该编码的一条路径**。
    ///
    /// **主键就是 `commit.key`**——它在**产生候选的地方**（翻译器）
    /// 由编码渲染而来，因此"规范"这件事不需要任何一层去反查。
    /// `commit.key` 为 `None` 时（原样上屏、标点、造句）退回按
    /// [`Commit::input`] 规范化：那条路自洽，但不跨拼法共享。
    ///
    /// **实现错了的症状是"学过的词有时出现有时不出现"，极难排查。**
    fn record(&self, commit: &Commit);

    /// 按**键**查询已学词条，供 `Lane::Input` 的重排使用。
    /// **必须走内存缓存，零磁盘 I/O。**
    ///
    /// # `key` 是**已经定好的键**，实现不得再加工
    ///
    /// 它要么是 [`Commit::key`]（规范编码，如 `ni'hao`），要么是
    /// 实现自己在 `record` 的兜底路径里用过的拼写键。**不要再规范化一次**
    /// ——规范化的规则（去分隔符）会把编码键里的 `'` 吃掉，
    /// 于是存进去的与查出来的变成两把不同的键。
    fn lookup(&self, key: &str) -> Vec<MemoryEntry>;

    /// **取消一次学习**（G11）。
    ///
    /// RIME 的规则是：「只能夠從用戶詞典中刪除詞組。用於碼表中原有的詞組時，
    /// **只會取消其調頻效果**」——也就是说，"删除"对系统词来说不是删掉它，
    /// 而是**撤销用户对它的加权**。用户需要能反悔。
    ///
    /// `key` 与 [`MemoryStore::lookup`] 同一条约定：**已经是定好的键**。
    fn forget(&self, key: &str, text: &str);

    /// 查询"上一个词之后可能接什么"，供 `Lane::Predict` 使用（P4b）。
    ///
    /// # 上下文有多长、看谁
    ///
    /// **由实现决定策略，但必须是确定性的**。本项目的默认实现
    /// （`qingjian-memory::FileMemory`）走**最长上下文优先 + 回退**：
    /// 先看最近**两个**词（trigram），有记录就只答它；没有才退到最近**一个**
    /// 词（bigram）。这样"今天 微信 → 朋友圈"这种更具体的搭配优先于
    /// "微信 → 朋友圈"这种更泛的搭配，而数据稀疏时又不会什么都不给。
    ///
    /// # 返回什么顺序
    ///
    /// **分数降序**，同分时按确定顺序（实现内部用有序容器）。
    /// 流水线会把它们排进 `Lane::Predict`，因此这里**不需要**考虑
    /// `Lane::Input` 的排序规则，也不需要满足"精确优先"。
    ///
    /// 上下文为空（还没上屏过任何词）时返回空。
    fn predict_next(&self, context: &Context) -> Vec<Prediction>;
}

/// 一个什么都不做的记忆实现，供 P1 使用。
///
/// 它存在的意义是：**接口先接线，实现后补**——这样 P4a 换上真实现时
/// 引擎一行都不用改。
#[derive(Debug, Default)]
pub struct NoMemory;
impl MemoryStore for NoMemory {
    fn record(&self, _commit: &Commit) {}

    fn lookup(&self, _input: &str) -> Vec<MemoryEntry> {
        Vec::new()
    }

    fn forget(&self, _key: &str, _text: &str) {}

    fn predict_next(&self, _context: &Context) -> Vec<Prediction> {
        Vec::new()
    }
}

/// 一个固定在某个时刻的时钟，供测试使用。
#[derive(Debug, Clone, Copy)]
pub struct FrozenClock {
    /// 固定的秒。
    pub secs: u64,
    /// 固定的毫秒。
    pub ms: u64,
    /// 固定的时区偏移（秒）。默认 0 = 按 UTC 报时。
    pub offset_secs: i64,
}

impl Clock for FrozenClock {
    fn now_secs(&self) -> u64 {
        self.secs
    }

    fn now_ms(&self) -> u64 {
        self.ms
    }

    fn utc_offset_secs(&self) -> i64 {
        self.offset_secs
    }
}

/// 便于测试：一个从不产生候选的词库。
#[derive(Debug, Default)]
pub struct EmptyLexicon;

impl Lexicon for EmptyLexicon {
    fn lookup(&self, _code: &[CodeUnitId], _out: &mut CandidateSink<'_>) {}
}

/// 便于测试：一个把整段拼写当作单个编码单元的拼写层。
#[derive(Debug, Default)]
pub struct LiteralSpelling;

impl Spelling for LiteralSpelling {
    fn alphabet(&self) -> &CodeAlphabet {
        static EMPTY: CodeAlphabet = CodeAlphabet { units: Vec::new() };
        &EMPTY
    }

    fn expand(&self, spelling: &str, out: &mut ExpansionSink<'_>) {
        out.push(Expansion {
            code: vec![CodeUnitId(0)],
            cost: Score::ZERO,
            attr: crate::candidate::SpellingAttr::NORMAL,
        });
        let _ = spelling;
    }
}

/// 装配处注入引擎的**服务集合**。
///
/// # 为什么是一个结构体，而不是"给 `Engine` 加几个参数"
///
/// 服务会变多（P4a 的重排器、P4b 的预测表、P5 的向量），而每加一个就改一次
/// `Engine` 的构造函数，意味着**每一处装配点都要跟着改**。一个集合把这件事
/// 收敛成一处：装配点构造它，引擎消费它。
///
/// # 谁构造它（PLAN §5.12 的三个角色）
///
/// - **接口是什么**：本结构体 + 它持有的那些 trait。
/// - **谁实现**：周边 crate（`qingjian-memory` 提供 `Ranker` 与 `Clock`）。
/// - **谁消费**：[`crate::LoadedSchema::build_pipeline`] 在**装配期**把它们
///   注入组件——**不穿过 `Query`**（`docs/engine-design.md` §4）。
///
/// # 它为什么在 `qingjian-core` 而不是 `qingjian-engine`
///
/// 因为 `LoadedSchema` 是内核 trait，它的签名里出现的东西必须也属于内核。
/// 这个结构体只装 `qingjian-core` 自己定义的 trait 对象，因此不引入任何依赖。
#[derive(Clone)]
pub struct Services {
    /// 需要"现在几点"的零件用它（`date_translator` 一族）。
    ///
    /// **不是 `Option`**：时钟缺席时该做的不是"零件不装"，而是"装配处要说清
    /// 用哪个时钟"——一个静默冻结在 1970 的时钟会让日期候选全部出错，
    /// 而那是这个项目最怕的一类 bug（配置看着正常、功能就是不对）。
    pub clock: Arc<dyn Clock>,
    /// **每个会话装配时都会挂上的重排器**（按顺序执行）。
    ///
    /// 记忆（P4a）、上下文（P4b）、向量（P5）都从这里进来。为空即"不重排"，
    /// 这正是 `--userdb` 默认关闭时的行为。
    pub rankers: Vec<Arc<dyn Ranker>>,
    /// **下一词预测的数据源**（P4b）。`None` = 这个进程**不预测**。
    ///
    /// # 它为什么与 `rankers` 分开，而不是从重排器里取
    ///
    /// 两者消费的是**同一个服务实现**（`qingjian-memory::FileMemory`）的两组
    /// 不同数据，但它们是**两条独立的开关**：用户可以只要"打过的词下次优先"
    /// 而不要"猜我下一句想打什么"（HANDOFF §7.7.3 第 6 步：预测默认关）。
    /// 用一个 `Option` 表达"预测有没有被装上"，是最直接的可检查判据。
    ///
    /// # 它为什么是 `Option` 而 `clock` 不是
    ///
    /// 时钟缺席会让日期类零件**静默给出 1970 年**——那必须由装配处说清。
    /// 而预测缺席是**有意义的产品状态**（默认关），不是配置遗漏。
    /// 两者的区别是"静默错误"与"显式关闭"的区别。
    pub prediction: Option<Arc<dyn MemoryStore>>,
    /// **随机源的工厂**（`uuid_translator` 用它）。
    ///
    /// # 为什么是工厂而不是一个 `RandomSource`
    ///
    /// 因为 [`RandomSource::next_u64`] 要 `&mut self`：一条**流**不能被
    /// 多个会话共享（共享就得给每次取数上锁，而"随机"本来就不需要跨会话
    /// 一致）。每个会话装配时向工厂要一个新的流，各自独立推进。
    ///
    /// 用 `Box<dyn Fn() -> ...>` 而不是泛型：服务集合要被 `Clone`
    /// 并在所有会话之间共享（D38）。
    pub random: Arc<dyn Fn() -> Box<dyn RandomSource> + Send + Sync>,
}

impl Services {
    /// 只有时钟、没有任何重排器。
    ///
    /// 随机源默认是**确定性**的（`DeterministicRandom`）：这样测试与
    /// `--check` 的输出可复现。**生产装配处应当用
    /// [`Services::with_random`] 注入真随机**——否则同一个进程每次生成的
    /// UUID 会一模一样。
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            rankers: Vec::new(),
            prediction: None,
            random: Arc::new(|| Box::new(DeterministicRandom::new(0))),
        }
    }

    /// 换一个随机源工厂（链式）。
    #[must_use]
    pub fn with_random(
        mut self,
        random: Arc<dyn Fn() -> Box<dyn RandomSource> + Send + Sync>,
    ) -> Self {
        self.random = random;
        self
    }

    /// 加上一个重排器（链式）。
    #[must_use]
    pub fn with_ranker(mut self, ranker: Arc<dyn Ranker>) -> Self {
        self.rankers.push(ranker);
        self
    }

    /// 装上**下一词预测的数据源**（链式）——装上才会预测（P4b）。
    ///
    /// 不调用它 = 不预测。这是"预测默认关"在装配层的表达，
    /// 也是"一键关闭"的执行点：**没有服务，就没有预测候选**。
    #[must_use]
    pub fn with_prediction(mut self, store: Arc<dyn MemoryStore>) -> Self {
        self.prediction = Some(store);
        self
    }

    /// 有没有预测数据源——装配期用它决定要不要走预测那条路。
    #[must_use]
    pub fn has_prediction(&self) -> bool {
        self.prediction.is_some()
    }

    /// 有没有重排器——装配期用它决定要不要走重排那条路。
    #[must_use]
    pub fn has_rankers(&self) -> bool {
        !self.rankers.is_empty()
    }

    /// **无服务的便捷构造**：没有重排器，时钟固定在 Unix 纪元。
    ///
    /// # 它只该出现在测试与自检里
    ///
    /// 冻结在 0 的时钟意味着任何依赖"现在几点"的零件都会报 1970 年。
    /// 生产装配处（CLI / 未来前端）**必须**用 [`Services::new`] 传一个真时钟，
    /// 否则那个功能会静默地给出错误的日期。
    #[must_use]
    pub fn none() -> Self {
        Self::new(Arc::new(FrozenClock {
            secs: 0,
            ms: 0,
            offset_secs: 0,
        }))
    }
}

impl Default for Services {
    fn default() -> Self {
        Self::none()
    }
}
