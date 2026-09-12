//! # Service traits
//!
//! 中文职责：外部资源与可替换能力的接口定义。
//! English role: interfaces for external resources and swappable capabilities.
//! 架构位置：stele-core 的"服务定义"一侧；由 `stele-engine` / `stele-memory` /
//! `stele-dict` 等提供实现，在**组件构造时注入**（不穿过 `Query`）。
//!
//! PLAN §2.5：外部资源（存储、模型、时钟、随机数）一律通过 trait 注入，
//! 保证引擎不直接读文件、时钟或网络，从而可纯 `cargo test`。

use crate::candidate::{Candidate, CandidateSink};
use crate::commit::Commit;
use crate::context::Context;
use crate::score::Score;

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
    /// 按 `commit.lane` 分流：`Lane::Input` 用 `input` 作主键，
    /// `Lane::Predict` 用 `context` 作主键。
    ///
    /// # ⚠️ 主键必须是**规范编码**（G10）
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
    /// 所以：如果 `commit.attr.is_derived()`，实现**必须**先把 `commit.input`
    /// 经拼写层反查回规范编码再存；**换算不出来就宁可不存**。
    ///
    /// **实现错了的症状是"学过的词有时出现有时不出现"，极难排查。**
    fn record(&self, commit: &Commit);

    /// 查询某个输入的已学词条，供 `Lane::Input` 的重排使用。
    /// **必须走内存缓存，零磁盘 I/O。**
    fn lookup(&self, input: &str) -> Vec<MemoryEntry>;

    /// **取消一次学习**（G11）。
    ///
    /// RIME 的规则是：「只能夠從用戶詞典中刪除詞組。用於碼表中原有的詞組時，
    /// **只會取消其調頻效果**」——也就是说，"删除"对系统词来说不是删掉它，
    /// 而是**撤销用户对它的加权**。用户需要能反悔。
    fn forget(&self, key: &str, text: &str);

    /// 查询"上一个词之后可能接什么"，供 `Lane::Predict` 使用（P4b）。
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
}

impl Clock for FrozenClock {
    fn now_secs(&self) -> u64 {
        self.secs
    }

    fn now_ms(&self) -> u64 {
        self.ms
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
