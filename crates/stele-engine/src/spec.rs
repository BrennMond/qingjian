//! # Component specs
//!
//! 中文职责：零件在方案里**能写什么**的数据定义。
//! English role: the data model for what a component may declare in scheme data.
//! 架构位置：`stele-schemes`（装载器）与 `stele-engine`（零件实现）之间的契约。
//!
//! # 为什么数据定义与实现分开放
//!
//! RIME 的零件在方案里是这样配的：
//!
//! ```yaml
//! recognizer:
//!   patterns:
//!     punct: "^v([0-9]|10|[A-Za-z]+)$"
//! punctuator:
//!   half_shape: { ",": "，" }
//! key_binder:
//!   bindings:
//!     - { when: paging, accept: minus, send: Page_Up }
//! ```
//!
//! 这些都是**数据**，字段名与取值来自 RIME 的既有约定（我们照抄字段名，
//! 因为"用户的方案文件能直接拿来用"比"我们的字段名更好听"重要得多）。
//! 把它们集中在这里有三个好处：
//!
//! 1. **装载器不需要认识零件**——它只把 YAML 填进这些结构体，
//!    "这个字段是什么意思"由零件自己解释。
//! 2. **诊断能带行号**——每个结构体都记着自己来自第几行，
//!    于是"第 42 行的 `accept` 写错了"这种报错是免费的。
//! 3. **零件可以独立测试**——不需要造一份完整的方案文件。

use stele_core::Tag;

/// 带行号的一次性来源标注。
///
/// 诊断要指得出"第几行"。字段一多就容易忘掉这件事，
/// 所以把它做成一个必须显式填的字段（`line: usize`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct At {
    /// 该配置项在源文件里的行号（1 起）。
    pub line: usize,
}

impl At {
    /// 构造。
    #[must_use]
    pub fn new(line: usize) -> Self {
        Self { line }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 处理器
// ─────────────────────────────────────────────────────────────────────────────

/// 一个按键组合：**已经解析过**（不是字符串）。
///
/// 定义在 [`crate::keyspec`]——解析按键名是引擎的语义，因此那一份实现
/// 由引擎与装载器**共用**（见那个模块的说明）。
pub use crate::keyspec::KeyChord;

/// `editor` 的动作（RIME 的 `editor/bindings` 右侧那一列）。
///
/// **这是一个可枚举的封闭集合**，因此不是字符串——拼错的动作品名
/// 在**装载期**就会报错，而不是静默地什么都不做
/// （RIME 的反面教材：写错的字段名被默默忽略）。
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorAction {
    /// 上屏当前高亮的候选（空格）。
    Confirm,
    /// 上屏**原始输入**（未经拼写变换）。
    CommitRawInput,
    /// 上屏**变换后的输入**（预编辑串，经过 `preedit_format`）。
    CommitScriptText,
    /// 向前删除一个字符（撤销上次输入）。
    Revert,
    /// **按编码单元**回退删除。
    ///
    /// 这是 RIME 那条著名行为：「輸入拼音後按退格鍵，也會以音節爲單位
    /// 回退刪除拼音」。它绑在 `Control+BackSpace` 上时是主动作，
    /// 绑在 `BackSpace` 上就是默认行为。
    BackUnit,
    /// 向后删除一个字符（Delete 键）。
    DeleteForward,
    /// 取消本次输入（Esc）。
    Cancel,
    /// 上屏**当前候选的注释**（`commit_comment`）。
    CommitComment,
    /// 确认当前选择；确认后若没有候选了，就把输入整串上屏
    /// （`commit_composition`）。
    CommitComposition,
    /// 退回上一个已选段重新选；退不回去就确认当前选择
    /// （`toggle_selection`）。
    ReopenOrConfirm,
    /// 退段 / 退选择 / 退一个输入字符，三级兜底（`back`）。
    ///
    /// 与 [`EditorAction::BackUnit`] 的区别：`BackUnit` 是**按编码单元**
    /// 退（RIME 的 `back_syllable`），`Back` 是"能退多少退多少"的兜底链。
    BackStep,
    /// 从用户词典里删掉当前候选（`delete_candidate`，学习型删除）。
    DeleteCandidate,
    /// **解除这个键的默认绑定**。
    ///
    /// librime 里它叫 `noop`，而语义**不是"什么都不做的空动作"**：
    ///
    /// ```cpp
    /// if (action == kActionNoop) {
    ///   this->erase(key_event);   // ← 删掉这个键的默认绑定
    ///   return kAccepted;
    /// }
    /// ```
    ///
    /// 差别很实际：`editor` 有一张**默认绑定表**（空格=确认、退格=回退…），
    /// 方案写 `space: noop` 的意思是"空格别管了，还给系统"。
    /// 若把它实现成"空动作"，方案**删不掉**任何默认绑定——
    /// 于是"我明明把空格解绑了，它还在确认候选"。
    ///
    /// 我们是按"整个替换默认表"实现 `editor/bindings` 的
    /// （见 [`crate::processor::Editor`]），因此解除绑定在这里等价于
    /// **不写这一条**。它的存在意义是：方案从 RIME 那边抄过来时，
    /// `noop` 必须能被解析，而不是报"不认识的动作品名"。
    Noop,
}

impl EditorAction {
    /// 解析 RIME 的动作名。
    ///
    /// **不认识的取值返回 `None`**，由装载器报错——绝不"猜一个"。
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "confirm" => Self::Confirm,
            "commit_raw_input" => Self::CommitRawInput,
            "commit_script_text" => Self::CommitScriptText,
            "revert" => Self::Revert,
            // RIME 里这个动作叫 `back_syllable`。**我们的内核不许出现
            // "syllable" 这个词**（D20 的命名门禁），所以对外保留 RIME 的
            // 拼写、对内叫 `BackUnit`——"一个编码单元"是通用说法。
            "back_syllable" | "back_unit" => Self::BackUnit,
            "delete" => Self::DeleteForward,
            "cancel" => Self::Cancel,
            "commit_comment" => Self::CommitComment,
            "commit_composition" => Self::CommitComposition,
            "toggle_selection" => Self::ReopenOrConfirm,
            "back" => Self::BackStep,
            "delete_candidate" => Self::DeleteCandidate,
            "noop" => Self::Noop,
            _ => return None,
        })
    }

    /// 全部动作名（供诊断信息列出可取的值）。
    #[must_use]
    pub fn all_names() -> &'static [&'static str] {
        &[
            "confirm",
            "commit_raw_input",
            "commit_script_text",
            "revert",
            "back_syllable",
            "delete",
            "cancel",
            "commit_comment",
            "commit_composition",
            "toggle_selection",
            "back",
            "delete_candidate",
            "noop",
        ]
    }
}

/// `editor` 的一条绑定。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorBinding {
    /// 哪个按键。
    pub chord: KeyChord,
    /// 这个按键做什么。
    pub action: EditorAction,
    /// 来源行号。
    pub at: At,
}

// ─────────────────────────────────────────────────────────────────────────────
// 切分器
// ─────────────────────────────────────────────────────────────────────────────

/// 一个识别模式。
///
/// 对应 RIME 的 `recognizer/patterns` 里的一项：
///
/// ```yaml
/// patterns:
///   punct: "^v([0-9]|10|[A-Za-z]+)$"
/// ```
///
/// **`name` 就是标签**（RIME 用 pattern 的名字当 tag），因此
/// `affix_segmentor@radical_lookup` 的 `tag: radical_lookup`
/// 和 `patterns/radical_lookup` 是同一个词。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecogPattern {
    /// 模式名（= 标签）。
    pub name: String,
    /// 正则的**字面前缀**（`^` 之后到第一个元字符之前的那些字符）。
    ///
    /// 提前算出来是为了**快速排除**：绝大多数按键都不以任何一个模式的
    /// 字面前缀开头，于是连正则都不用跑。RIME 也是这么做的（它把
    /// `^abc...` 里的字面部分当作 `leading`）。
    pub leading: String,
    /// 完整正则的源码。
    pub regex: String,
    /// `None` = 匹配到输入末尾（前缀模式）；`Some(s)` = 必须以此结尾。
    ///
    /// RIME 的方案里这两种都有：`^uU[a-z]+$`（前缀模式，边打边匹配）
    /// 与 `^;.*;$`（必须以 `;` 结尾才成立）。
    pub trailing: Option<String>,
    /// 来源行号。
    pub at: At,
}

/// `recognizer` 的配置。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecognizerSpec {
    /// `import_preset: default` —— 是否要叠加预设模式。
    ///
    /// 引擎**不认识** `default` 是什么（那是方案数据的名字），
    /// 因此这里只记"它声明了要导入"，由调用方（部署工具链）决定
    /// 预设从哪来。声明了却给不出预设时，装载器会**出声**。
    pub import_preset: Option<String>,
    /// 方案自己的模式。
    pub patterns: Vec<RecogPattern>,
}

/// `affix_segmentor` 的配置。
///
/// 用途一例：拆字反查。输入 `uUni` 时，前缀 `uU` 被吃掉，
/// 剩下的 `ni` 交给标了 `radical_lookup` 的翻译器去查部件表。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AffixSpec {
    /// 这个切分器产出的标签。
    pub tag: Option<Tag>,
    /// 前缀（例如 `"uU"`；RIME 允许写两个字符表示大小写两种写法）。
    pub prefix: Option<String>,
    /// 后缀（可空）。
    pub suffix: Option<String>,
    /// **额外附加**的标签。
    ///
    /// RIME 的 `extra_tags` 让一个分段同时带多个标签——
    /// 于是"拆字反查"的输入也能被普通翻译器看到（用户可以同时得到
    /// 拼音候选与拆字候选）。这是 G3"一个分段可带多个标签"的用处。
    pub extra_tags: Vec<Tag>,
    /// 显示提示（例如 `"  〔拆字〕"`）。
    pub tips: Option<String>,
    /// 来源行号。
    pub at: At,
}

/// `punctuator` 的配置。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PunctuatorSpec {
    /// 全角映射。
    pub full_shape: Vec<(String, String)>,
    /// 半角映射。
    pub half_shape: Vec<(String, String)>,
    /// 以某个前缀字符进入的符号表（RIME 的 `/` 或雾凇的 `v`）。
    pub symbols: Vec<(String, String)>,
    /// 进入符号表的前缀字符（从 `symbols` 第一个键里取出来，
    /// 但**显式记下来**更清楚，也便于诊断）。
    pub symbol_prefix: Option<char>,
    /// 来源行号。
    pub at: At,
}

/// `key_binder` 的一条绑定。
///
/// # 为什么 `send` 是"一段文本"而不是"一个键"
///
/// RIME 的 `send` 写的是键名（`space`、`Page_Up`），但对我们来说
/// **按键是由前端产生的**：引擎"发送一个 `Page_Up"没有意义`，
/// 它只能"上屏一段文本"或"改一个开关"。
///
/// 因此我们把它落成两种**引擎真能做**的效果：
///
/// - [`KeyBinding::send_text`]：直接上屏这段文本（`send: space` → `" "`）
/// - [`KeyBinding::toggle`]：切换一个开关（`toggle: ascii_mode`）
///
/// 翻页类的 `send: Page_Up` 需要引擎有"页"的概念（P3 尚未有），
/// 这类绑定会被装载器**明确报为不支持**，而不是静默失效。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyBinding {
    /// 什么时候生效（`always` / `composing` / `paging` / `has_menu`）。
    pub when: WhenPredicate,
    /// 接受哪些按键。
    pub accept: Vec<KeyChord>,
    /// **换成哪些按键**（`send` / `send_sequence`）。
    ///
    /// 存的是**按键名的原文**，不是解析结果：派发时要把它变回按键，
    /// 而"名字 → 按键"的解析在 [`crate::keyspec`]（引擎侧）。
    /// 这样数据形状与 RIME 一致，装载器也不需要认识按键语义。
    ///
    /// 一个元素的 `send` 与多个元素的 `send_sequence` 在这里是同一种东西——
    /// librime 的 `binding.target` 就是一个 `KeySequence`。
    pub send_keys: Option<Vec<String>>,
    /// 切换这个开关（`toggle`）。
    pub toggle: Option<String>,
    /// 来源行号。
    pub at: At,
}

/// `key_binder/bindings` 的 `when` 谓词。
///
/// **取值是封闭集合**——写错了在装载期报错。RIME 的 4 个取值全收。
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WhenPredicate {
    /// 总是生效。
    #[default]
    Always,
    /// 正在输入时。
    Composing,
    /// 候选多到需要翻页时。
    Paging,
    /// 有候选时。
    HasMenu,
    /// 上一条候选是预测来的（`Lane::Predict`）。
    ///
    /// **目前永远为假**：核心引擎还没有任何零件给分段打 `prediction` 标签
    /// （下一词预测是 P4b）。留在这里是为了"RIME 的方案能原样读进来"。
    Predicting,
}

impl WhenPredicate {
    /// 解析 RIME 的取值。
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "always" => Self::Always,
            "composing" => Self::Composing,
            "paging" => Self::Paging,
            "has_menu" => Self::HasMenu,
            // librime 的第五个合法谓词。核心引擎里目前没有零件写
            // `prediction` 标签，因此它**永远不会成立**——但**必须能解析**：
            // 报"不认识的谓词"会让一份从 RIME 抄来的方案整份装不进去，
            // 而实际行为并不缺（那个绑定只是不生效）。
            "predicting" => Self::Predicting,
            _ => return None,
        })
    }

    /// 全部取值（供诊断列出）。
    #[must_use]
    pub fn all_names() -> &'static [&'static str] {
        &["always", "composing", "paging", "has_menu", "predicting"]
    }
}

/// `navigator` 的配置。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NavigatorSpec {
    /// 上一页 / 下一页的按键。
    pub page_up: Vec<KeyChord>,
    /// 下一页。
    pub page_down: Vec<KeyChord>,
    /// 上一页 / 下一页的备选（RIME 的 `-` / `=` 之类）。
    pub up: Vec<KeyChord>,
    /// 下一页。
    pub down: Vec<KeyChord>,
    /// 来源行号。
    pub at: At,
}

// ─────────────────────────────────────────────────────────────────────────────
// 翻译器 / 滤镜
// ─────────────────────────────────────────────────────────────────────────────

/// `reverse_lookup_filter` 的配置。
///
/// 用途：给出候选的**编码注音**。雾凇的拆字反查用它显示"这个字怎么拆"。
#[derive(Clone, Debug, Default)]
pub struct ReverseLookupSpec {
    /// 只对这些标签的分段生效。
    pub tags: Vec<Tag>,
    /// 去哪本词典查编码。
    pub dictionary: Option<String>,
    /// 把查到的编码加工成注释（一串拼写运算）。
    pub comment_format: Vec<crate::spelling::Rule>,
    /// 注释已存在时是否覆盖。
    pub overwrite_comment: bool,
    /// 来源行号。
    pub at: At,
}

/// `simplifier` 的配置。
///
/// **不实现 `PartialEq`**：它里面含有已解析的规则（`Rule` 持有编译好的
/// 正则，而正则的比较不是我们需要的语义——两串相同写法的规则
/// 期望上应当相等，但"相等"在这里没有任何用处）。
#[derive(Clone, Debug, Default)]
pub struct SimplifierSpec {
    /// 由哪个开关控制（`option_name`）。
    pub option_name: Option<String>,
    /// 转换表从哪来（RIME 是 `OpenCC` 的 `opencc_config`）。
    pub opencc_config: Option<String>,
    /// 提示方式。
    pub tips: TipsMode,
    /// 是否继承原候选的注释。
    pub inherit_comment: bool,
    /// 只对这些标签生效（空 = 全部）。
    pub tags: Vec<Tag>,
    /// 来源行号。
    pub at: At,
}

/// `tips` 的取值。
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TipsMode {
    /// 都显示。
    #[default]
    All,
    /// 仅单字显示。
    Char,
    /// 不显示。
    None,
}

impl TipsMode {
    /// 解析。
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "all" => Self::All,
            "char" => Self::Char,
            "none" => Self::None,
            _ => return None,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 翻译器（表驱动的那些）
// ─────────────────────────────────────────────────────────────────────────────

/// 翻译器用哪一族实现。
///
/// **这是方案数据**（D33）：方案在配置里选，引擎不预设。
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranslatorKindSpec {
    /// 拼写图族（RIME 的 `script_translator`）：编码集合**可枚举**。
    SpellingGraph,
    /// 精确编码族（RIME 的 `table_translator`）：编码集合**不可枚举**。
    ExactCode,
}

impl TranslatorKindSpec {
    /// 解析 RIME 的零件名。
    #[must_use]
    pub fn parse(component: &str) -> Option<Self> {
        Some(match component {
            "script_translator" => Self::SpellingGraph,
            "table_translator" => Self::ExactCode,
            _ => return None,
        })
    }

    /// RIME 里的零件名。
    #[must_use]
    pub fn component_name(self) -> &'static str {
        match self {
            Self::SpellingGraph => "script_translator",
            Self::ExactCode => "table_translator",
        }
    }
}

/// 一个翻译器实例的配置。
///
/// 字段名**照抄 RIME**（`dictionary` / `enable_completion` / `initial_quality`…），
/// 因为"用户手上的方案文件能直接用"比"我们的字段名更好听"重要得多。
#[derive(Clone, Debug, PartialEq)]
#[derive(Default)]
pub struct TranslatorSpec {
    /// 零件名（`script_translator` / `table_translator`）。
    pub component: String,
    /// 别名实例名（`table_translator@melt_eng` 里的 `melt_eng`）。
    ///
    /// 方案里的配置块叫这个名字，因此它是"这个实例的参数在哪"的钥匙。
    pub alias: Option<String>,
    /// 挂哪本词库。
    pub dictionary: Option<String>,
    /// 词条补全（`enable_word_completion` / `enable_completion`）。
    ///
    /// RIME 的语义：**长词条可以只打前几个编码单元**（`nihao` → 你好世界
    /// 之类的多单元词条可以只敲一部分）就出现。值为 `None` 表示方案没写，
    /// 用引擎默认（[`TranslatorSpec::default_completion`]）。
    pub enable_word_completion: Option<bool>,
    /// 要不要造句。
    pub enable_sentence: Option<bool>,
    /// 初始权重（**线性域**，与词条权重同域）。
    ///
    /// 注意 RIME 的 `initial_quality` 有的方案写成 `1.2` 有的写成整数；
    /// 我们统一按**权重倍率**解释，装载期换算成对数域。
    pub initial_quality: Option<f64>,
    /// 候选注释的格式化规则（`comment_format`）。
    pub comment_format: Vec<String>,
    /// 预编辑串的格式化规则（`preedit_format`）。
    pub preedit_format: Vec<String>,
    /// 前缀（`prefix: "uU"`）——带前缀的翻译器只处理带前缀的输入。
    pub prefix: Option<String>,
    /// 显示提示。
    pub tips: Option<String>,
    /// 来源行号。
    pub at: At,
}


impl TranslatorSpec {
    /// 词条补全的默认值：**关**。
    ///
    /// RIME 的默认也是关——`enable_word_completion` 必须显式打开。
    /// 这条默认值很重要：补全会让**候选变多**，而"候选突然多出一堆
    /// 你没打完的词"是一种打扰，必须由方案决定要不要。
    #[must_use]
    pub fn default_completion() -> bool {
        false
    }

    /// 生效的补全设置。
    #[must_use]
    pub fn completion(&self) -> bool {
        self.enable_word_completion
            .unwrap_or_else(Self::default_completion)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 引擎声明（`engine:` 段）
// ─────────────────────────────────────────────────────────────────────────────

/// `engine:` 段的声明。
///
/// # 两种写法
///
/// RIME 的 `engine:` 是**名字列表**：
///
/// ```yaml
/// engine:
///   processors:  [ ascii_composer, speller, punctuator, selector ]
///   translators: [ script_translator, table_translator@melt_eng ]
/// ```
///
/// 我们**照抄**这个形状（而不是自创一套），但在它为空时退回
/// "按方案声明的翻译器族自动装配"——那是 P1/P2 既有方案的行为，
/// 不能让它们因为 P3 加了注册表就失效。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineSpec {
    /// 处理器名字（按顺序）。
    pub processors: Vec<String>,
    /// 切分器名字（按顺序）。
    pub segmentors: Vec<String>,
    /// 翻译器名字（按顺序）。
    pub translators: Vec<String>,
    /// 滤镜名字（按顺序）。
    pub filters: Vec<String>,
    /// 本方案的主标签（`engine.tag`）。
    pub tag: Option<String>,
}

impl EngineSpec {
    /// 是否声明了任何零件。
    #[must_use]
    pub fn is_declared(&self) -> bool {
        !self.processors.is_empty()
            || !self.segmentors.is_empty()
            || !self.translators.is_empty()
            || !self.filters.is_empty()
    }
}

/// 从 `name@alias` 里拆出零件名与实例名。
///
/// RIME 用 `@` 区分"同一零件的多个实例"（`table_translator@melt_eng`）。
/// **注册表按零件名查**，实例名只是"参数放在哪个配置块"的钥匙——
/// 这个拆分点因此只有一处，别处不许再自己 split。
#[must_use]
pub fn split_alias(name: &str) -> (&str, Option<&str>) {
    match name.split_once('@') {
        Some((c, a)) if !a.is_empty() => (c, Some(a)),
        _ => (name, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stele_core::{Key, KeyCode, Modifiers, NamedKey};

    #[test]
    fn editor_action_names_are_a_closed_set() {
        assert_eq!(
            EditorAction::parse("commit_raw_input"),
            Some(EditorAction::CommitRawInput)
        );
        // RIME 的拼写被接受，但内核里的名字不含那个词（D20）。
        assert_eq!(
            EditorAction::parse("back_syllable"),
            Some(EditorAction::BackUnit)
        );
        // 不认识的取值**不猜**。
        assert_eq!(EditorAction::parse("commit_everything"), None);
        for n in EditorAction::all_names() {
            assert!(
                EditorAction::parse(n).is_some(),
                "列在 all_names 里的 {n} 必须能被解析"
            );
        }
    }

    #[test]
    fn key_chord_only_compares_declared_modifiers() {
        let chord = KeyChord::new(KeyCode::Named(NamedKey::Backspace), Modifiers::CTRL);
        let ctrl_bs = Key::press(KeyCode::Named(NamedKey::Backspace), Modifiers::CTRL);
        let plain_bs = Key::press(KeyCode::Named(NamedKey::Backspace), Modifiers::NONE);
        assert!(chord.matches(&ctrl_bs));
        assert!(!chord.matches(&plain_bs), "没按 Ctrl 不该命中");

        // 声明里没写的修饰键不影响匹配：`Return` 不该要求"恰好没按 Shift"。
        let ret = KeyChord::new(KeyCode::Named(NamedKey::Enter), Modifiers::NONE);
        let shift_ret = Key::press(
            KeyCode::Named(NamedKey::Enter),
            Modifiers::SHIFT,
        );
        assert!(ret.matches(&shift_ret));
    }

    #[test]
    fn when_predicate_is_a_closed_set() {
        assert_eq!(WhenPredicate::parse("paging"), Some(WhenPredicate::Paging));
        assert_eq!(WhenPredicate::parse("whenever"), None);
        for n in WhenPredicate::all_names() {
            assert!(WhenPredicate::parse(n).is_some());
        }
    }

    #[test]
    fn alias_splitting_has_exactly_one_definition() {
        assert_eq!(split_alias("table_translator@melt_eng"), ("table_translator", Some("melt_eng")));
        assert_eq!(split_alias("speller"), ("speller", None));
        // 空别名当作没有别名（`translator@` 这种写法是笔误，不是实例）。
        assert_eq!(split_alias("speller@"), ("speller@", None));
    }

    #[test]
    fn completion_defaults_to_off() {
        // RIME 的默认是关：补全会让候选变多，必须由方案显式决定。
        let s = TranslatorSpec::default();
        assert!(!s.completion());
        let on = TranslatorSpec {
            enable_word_completion: Some(true),
            ..Default::default()
        };
        assert!(on.completion());
    }

    #[test]
    fn translator_kind_parses_the_two_rime_component_names() {
        assert_eq!(
            TranslatorKindSpec::parse("script_translator"),
            Some(TranslatorKindSpec::SpellingGraph)
        );
        assert_eq!(
            TranslatorKindSpec::parse("table_translator"),
            Some(TranslatorKindSpec::ExactCode)
        );
        assert_eq!(TranslatorKindSpec::parse("telepathy_translator"), None);
        // 名字能往返：解析出来的名字必须能被解析回去。
        for k in [
            TranslatorKindSpec::SpellingGraph,
            TranslatorKindSpec::ExactCode,
        ] {
            assert_eq!(TranslatorKindSpec::parse(k.component_name()), Some(k));
        }
    }
}
