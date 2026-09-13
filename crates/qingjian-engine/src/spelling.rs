//! # Spelling layer
//!
//! 中文职责：把**用户敲出来的拼写**映射到**编码**，并给出代价与属性。
//! English role: map a typed spelling to codes, with cost and attributes.
//! 架构位置：`qingjian-core::Spelling` 的实现；翻译器在它之上工作。
//!
//! # 这一层为什么存在（并且为什么不在词库里）
//!
//! RIME 的作者在 2009 年就把这件事讲清楚了：
//!
//! > 爲了足夠靈活而能支持廣泛的輸入法類型，在輸入方案中，利用**拼寫運算**
//! > 機制在輸入碼與字典編碼之間建立一組映射，**以此將個別方案中的特殊檢索
//! > 方式統一到通用的算法**。
//!
//! 也就是说：**引擎不认识"简拼"，也不知道"模糊音"是什么**；它只知道
//! "某个拼写片段可以走某条边，代价是 X、属性是 Y"。方案的个性全部是数据。
//!
//! # 与 RIME 的差别（P1 范围）
//!
//! RIME 的运算子是正则表达式驱动的（`xform` / `derive` / `abbrev` / `fuzz` /
//! `erase` / `xlit`），需要正则引擎。本 crate 零依赖，因此 P1 只实现三类
//! 不依赖正则的规则（[`Rule`]）；**完整的代数与正则语义是 P2 的内容**。
//!
//! 但**结构是对的**：规则列表、按序各施加一次、产生带代价与属性的边——
//! 将来把 [`Rule`] 换成完整的运算子集合，不需要改这一层之外的东西。

use qingjian_core::{
    CodeUnitId, Expansion, ExpansionSink, PathLimits, PathSink, Score, SpellingAttr, SpellingPath,
};
use std::collections::BTreeMap;
use std::collections::BinaryHeap;

use crate::regex::{Regex, RegexError};

/// 一条拼写运算。
///
/// # 这是 RIME 的那套运算子，不是自创的简化规则
///
/// | 运算子 | 语义（RIME 原文档） |
/// | --- | --- |
/// | `xlit` | 「依次將拼寫中見於<左字母表>的字符替換爲<右字母表>對應位置的字符」 |
/// | `xform` | 「若拼寫（或其子串）與<模式>匹配，則將所匹配的部份改寫爲<替換式>」 |
/// | `erase` | 「若拼寫與<模式>**完全**匹配，則將該拼寫從有效拼寫集合中消除」 |
/// | `derive` | 「若對拼寫做正則匹配、替換而獲得了新的拼寫，則有效拼寫集合同時包含派生前後的拼寫」 |
/// | `fuzz` | 執行派生運算；派生出的拼寫將獲得「模糊」屬性 |
/// | `abbrev` | 執行派生運算；派生出的拼寫將獲得「縮略」屬性 |
///
/// **`erase` 是全匹配，其余是全局替换** —— 这是两种不同的正则操作，
/// 不是同一个操作加标志位。
///
/// **规范拼写永远是基线**：RIME 的代数从 `Sa = (A → A)` 出发，
/// 规则只在其上派生。所以没有"只保留规范拼写"这种规则——空规则列表就是它。
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum Rule {
    /// 字符转写。**RIME 中唯一按 UTF-32 处理的运算**。
    Xlit {
        /// 左字母表。
        from: Vec<char>,
        /// 右字母表（长度必须与左一致）。
        to: Vec<char>,
    },
    /// 正则改写（全局替换）。
    Xform {
        /// 模式。
        pattern: Regex,
        /// 替换式（可用 `$1`）。
        repl: String,
    },
    /// 消除（**全匹配**）。
    Erase {
        /// 模式。
        pattern: Regex,
    },
    /// 逐字符等价替换（模糊音）。
    ///
    /// 与 [`Rule::Xlit`] 的区别：`xlit` 是**改写**（原拼写失效），
    /// 这里是**派生**（原拼写仍有效）——因为模糊音要的是
    /// "`zhao` 和 `zao` 都能命中同一个编码"，而不是"`zhao` 变成 `zao`"。
    ///
    /// 为什么不能用 `Derive` + 字符类表达：`replace_all` 对每一处匹配
    /// 用**同一个**替换串，做不到"z→z、h→h"这种逐字符映射
    /// （那会把 `zhang` 变成 `zzhangh`）。这是实现时被测试抓到的一个真错误。
    Equivalence {
        /// 字符映射表。
        pairs: Vec<(char, char)>,
        /// 这条边的代价。
        cost: Score,
    },
    /// 派生：原拼写与新拼写**都留在**有效拼写集合里。
    Derive {
        /// 模式。
        pattern: Regex,
        /// 替换式。
        repl: String,
        /// 这条边的代价（对数域，通常为负）。
        cost: Score,
        /// 给派生拼写附加的属性。
        attr: SpellingAttr,
    },
}

/// 规则解析/编译错误。
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleError {
    /// 规则串为空。
    Empty,
    /// 认不出的运算子。
    UnknownOp(String),
    /// 参数个数不对。
    WrongArity {
        /// 运算子。
        op: String,
        /// 期望的参数个数。
        expected: usize,
        /// 实际拿到几个。
        got: usize,
    },
    /// `xlit` 两侧字母表长度不一致。
    XlitLengthMismatch {
        /// 左边长度。
        left: usize,
        /// 右边长度。
        right: usize,
    },
    /// 正则编译失败。
    BadRegex {
        /// 运算子。
        op: String,
        /// 底层错误。
        error: RegexError,
    },
    /// 这条规则不能用在格式化里。
    ///
    /// `comment_format` / `preedit_format` 只接受 `xform` 与 `xlit`——
    /// 其余运算子会产生**多个**结果，而一段注释只能显示成一种样子。
    NotAFormatRule,
}

impl std::fmt::Display for RuleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "拼写运算规则是空的"),
            Self::UnknownOp(op) => write!(
                f,
                "不认识的拼写运算子 `{op}`。可用的是 xlit / xform / erase / derive / fuzz / abbrev"
            ),
            Self::WrongArity { op, expected, got } => write!(
                f,
                "运算子 `{op}` 需要 {expected} 个参数，实际给了 {got} 个。\
                 写法是 `{op}<分隔符>参数1<分隔符>参数2<分隔符>`，例如 `xform/^([nl])ue$/$1ve/`"
            ),
            Self::XlitLengthMismatch { left, right } => write!(
                f,
                "`xlit` 两侧字母表长度必须相同（左 {left} 个、右 {right} 个字符）"
            ),
            Self::BadRegex { op, error } => write!(f, "运算子 `{op}` 的正则有问题：{error}"),
            Self::NotAFormatRule => write!(
                f,
                "这个运算子不能用在 comment_format / preedit_format 里：\
                 格式化只接受 `xform`（改写）与 `xlit`（逐字符转写）。\
                 `derive` / `fuzz` / `abbrev` 会产生**多个**结果，\
                 而一段注释只能显示成一种样子；`erase` 是「整串消除」的语义，\
                 在注释上应当写成 `xform/^.*$//`"
            ),
        }
    }
}

impl std::error::Error for RuleError {}

impl Rule {
    /// 按 RIME 的写法解析一条规则。
    ///
    /// 形如 `<运算子><分隔符><参数1><分隔符><参数2><分隔符>`，分隔符是单个 ASCII 字符
    /// （通常是 `/`；仓颉方案的 26 字母表用 `|`）。
    ///
    /// **参数里不能出现分隔符**，也不支持转义——这是 RIME 的约束，我们照办
    /// （见 `docs/engine-design.md` §13.3 G14）。
    ///
    /// # Errors
    ///
    /// 运算子不认识、参数个数不对、正则编译失败时返回 [`RuleError`]。
    pub fn parse(spec: &str) -> Result<Self, RuleError> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Err(RuleError::Empty);
        }
        let sep = spec
            .chars()
            .find(|c| c.is_ascii() && !c.is_ascii_alphanumeric())
            .unwrap_or('/');
        let parts: Vec<&str> = spec.split(sep).collect();
        let op = parts[0].trim();
        let args: Vec<&str> = parts[1..].to_vec();

        match op {
            "xlit" => {
                if args.len() < 2 {
                    return Err(RuleError::WrongArity {
                        op: op.into(),
                        expected: 2,
                        got: args.len(),
                    });
                }
                let from: Vec<char> = args[0].chars().collect();
                let to: Vec<char> = args[1].chars().collect();
                if from.len() != to.len() {
                    return Err(RuleError::XlitLengthMismatch {
                        left: from.len(),
                        right: to.len(),
                    });
                }
                Ok(Self::Xlit { from, to })
            }
            "xform" | "derive" | "fuzz" | "abbrev" => {
                if args.len() < 2 {
                    return Err(RuleError::WrongArity {
                        op: op.into(),
                        expected: 2,
                        got: args.len(),
                    });
                }
                let pattern = Regex::compile(args[0]).map_err(|e| RuleError::BadRegex {
                    op: op.into(),
                    error: e,
                })?;
                let repl = args[1].to_owned();
                match op {
                    "xform" => Ok(Self::Xform { pattern, repl }),
                    "derive" => Ok(Self::Derive {
                        pattern,
                        repl,
                        cost: Score::ZERO,
                        attr: SpellingAttr::NORMAL,
                    }),
                    "fuzz" => Ok(Self::Derive {
                        pattern,
                        repl,
                        cost: Score::ZERO,
                        attr: SpellingAttr::FUZZY,
                    }),
                    _ => Ok(Self::Derive {
                        pattern,
                        repl,
                        cost: Score::ZERO,
                        attr: SpellingAttr::ABBREV,
                    }),
                }
            }
            "erase" => {
                if args.is_empty() {
                    return Err(RuleError::WrongArity {
                        op: op.into(),
                        expected: 1,
                        got: 0,
                    });
                }
                let pattern = Regex::compile(args[0]).map_err(|e| RuleError::BadRegex {
                    op: op.into(),
                    error: e,
                })?;
                Ok(Self::Erase { pattern })
            }
            other => Err(RuleError::UnknownOp(other.to_owned())),
        }
    }

    /// 便捷构造：缩写规则（每个编码单元取前 `take` 个字符）。
    ///
    /// 等价于 RIME 的 `abbrev/^([a-z]{take}).+$/$1/`，只是写起来短。
    ///
    /// `cost` 是**毫对数**的代价（`Score` 本身就是那个域），
    /// 因此调用方若要写"打五折"，应当传 `Score::from_weight(0.5)`。
    ///
    /// # Errors
    ///
    /// 生成的正则若编译失败（`take` 为 0）时返回 [`RuleError`]。
    pub fn abbrev(take: usize, cost: Score) -> Result<Self, RuleError> {
        if take == 0 {
            return Err(RuleError::WrongArity {
                op: "abbrev".into(),
                expected: 1,
                got: 0,
            });
        }
        // 用 `{n}` 量词而不是重复写字符类——`take` 是配置里给的。
        let pattern =
            Regex::compile(&format!("^([a-z]{{{take}}}).+$")).map_err(|e| RuleError::BadRegex {
                op: "abbrev".into(),
                error: e,
            })?;
        Ok(Self::Derive {
            pattern,
            repl: "$1".into(),
            cost,
            attr: SpellingAttr::ABBREV,
        })
    }

    /// 便捷构造：等价替换（模糊音）。
    ///
    /// 与 `xlit` 的区别：它**派生**而不是**改写**——原拼写仍然有效。
    /// 这正是"模糊音"要的行为：`zhao` 和 `zao` 都能命中同一个编码。
    ///
    #[must_use]
    pub fn equivalence(pairs: &[(char, char)], cost: Score) -> Self {
        Self::Equivalence {
            pairs: pairs.to_vec(),
            cost,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 格式化规则（comment_format / preedit_format）
// ─────────────────────────────────────────────────────────────────────────────

/// 一条**纯文本**改写规则。
///
/// # 它为什么与 [`Rule`] 是两个东西
///
/// 两者写法一样（`xform/^([jqxy])v/$1u/`），但**语义方向相反**：
///
/// | | [`Rule`]（拼写代数） | [`FormatRule`]（格式化） |
/// | --- | --- | --- |
/// | 作用对象 | 拼写（"用户能敲什么"） | 一段要显示的文本 |
/// | 结果 | **一族**可能的编码 | **一个**字符串 |
/// | 可用的运算子 | xlit/xform/erase/derive/fuzz/abbrev | 只有 xlit/xform |
///
/// 把 `derive` 用在 `comment_format` 上是配置错误：注释只能显示成一种样子，
/// "派生两种写法"没有意义。**所以装载期要报错**，而不是随便挑一种。
#[derive(Clone, Debug)]
pub struct FormatRule {
    /// 模式。
    pattern: Regex,
    /// 替换式（可用 `$1`）。`None` = 纯删字符的 `xlit`。
    repl: String,
    /// `xlit` 的逐字符映射（`Some` 时按字符替换，不用正则）。
    xlit: Option<(Vec<char>, Vec<char>)>,
}

impl FormatRule {
    /// 从一条拼写运算规则里取出它的**文本变换**形态。
    ///
    /// # Errors
    ///
    /// 传入的规则不是 `xform` / `xlit` 时返回 [`RuleError::NotAFormatRule`]。
    /// 这条错误是**有意的**：静默挑一种语义会让 `comment_format` 出问题时
    /// 表现为"注释偶尔不对"，那是最难查的一类 bug。
    pub fn from_rule(rule: &Rule) -> Result<Self, RuleError> {
        match rule {
            Rule::Xform { pattern, repl } => Ok(Self {
                pattern: pattern.clone(),
                repl: repl.clone(),
                xlit: None,
            }),
            Rule::Xlit { from, to } => Ok(Self {
                pattern: Regex::compile(".").map_err(|e| RuleError::BadRegex {
                    op: "xlit".into(),
                    error: e,
                })?,
                repl: String::new(),
                xlit: Some((from.clone(), to.clone())),
            }),
            _ => Err(RuleError::NotAFormatRule),
        }
    }

    /// 解析 RIME 的写法。
    ///
    /// # Errors
    ///
    /// 写法不合法、或用了不可用于格式化的运算子时返回 [`RuleError`]。
    pub fn parse(spec: &str) -> Result<Self, RuleError> {
        Self::from_rule(&Rule::parse(spec)?)
    }

    /// 对一段文本施加这条改写。
    #[must_use]
    pub fn apply_text(&self, text: &str) -> String {
        if let Some((from, to)) = &self.xlit {
            let mut out = String::with_capacity(text.len());
            for c in text.chars() {
                match from.iter().position(|f| *f == c) {
                    Some(i) => out.push(*to.get(i).unwrap_or(&c)),
                    None => out.push(c),
                }
            }
            return out;
        }
        self.pattern.replace_all(text, &self.repl)
    }
}

/// 一串格式化规则。
///
/// 与 [`SpellingTable`] 分开的理由见 [`FormatRule`]：**它们的方向相反**。
/// 放在一起会让人以为"`comment_format` 里写了 `derive` 也能用"。
#[derive(Clone, Debug, Default)]
pub struct SpellingFormat {
    rules: Vec<FormatRule>,
}

impl SpellingFormat {
    /// 由一串已解析的规则构造。
    #[must_use]
    pub fn new(rules: Vec<FormatRule>) -> Self {
        Self { rules }
    }

    /// 解析 RIME 的写法列表（`comment_format:` 下面那几行）。
    ///
    /// # Errors
    ///
    /// 任一条不合法时返回 [`RuleError`]——**一次只报第一条**，
    /// 因为这里没有行号可用；装载器会把它包成带行号的诊断。
    pub fn parse_all(specs: &[String]) -> Result<Self, RuleError> {
        let mut rules = Vec::with_capacity(specs.len());
        for s in specs {
            rules.push(FormatRule::parse(s)?);
        }
        Ok(Self { rules })
    }

    /// 依次施加全部规则。
    #[must_use]
    pub fn apply(&self, text: &str) -> String {
        let mut cur = text.to_owned();
        for r in &self.rules {
            cur = r.apply_text(&cur);
        }
        cur
    }

    /// 规则条数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// 是否没有规则。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// 一条"拼写片段 → 编码单元"的边。
///
/// 与 RIME 的棱镜（prism）里的 `SpellingDescriptor` 对应：它记录
/// "这个片段对应哪个编码单元、带什么属性、代价多少"。
#[derive(Clone, Debug, PartialEq, Eq)]
struct UnitEdge {
    text: String,
    unit: CodeUnitId,
    attr: SpellingAttr,
    cost: Score,
}

/// 拼写表：把方案数据（字母表 + 运算规则）编译成"片段 → 编码单元"的边集合。
///
/// **编译发生在装载方案时，不在按键路径上**——这正是 RIME 把棱镜做成
/// 部署期产物的原因（见 `docs/engine-design.md` §5.2）。
pub struct SpellingTable {
    alphabet: qingjian_core::CodeAlphabet,
    edges: Vec<UnitEdge>,
    /// 按首字符分组，避免每个位置都遍历整张表。
    by_first_char: BTreeMap<char, Vec<usize>>,
    /// 一次展开的**硬预算**（见 [`ExpansionLimits`]）。
    limits: ExpansionLimits,
}

/// 一次拼写展开的**硬预算**。
///
/// # 为什么需要四项而不是一个 `max_expansions`
///
/// 审计实测：`ssss` 的第四次按键约 **1.52 s**、进程峰值约 **209 MiB**，
/// 而最终只得到字面量 `ssss`；`woaizhongguo` 输入过程中有 290/679/285 ms
/// 的按键，峰值约 146 MiB。输入法在**每个按键的同步路径**上，秒级卡顿
/// 与数百 MiB 瞬时分配都不可接受。
///
/// 旧实现只有一个 `max_expansions`（限制**产出**条数）和一个派生的
/// `budget = max_expansions × max_units × 8`。它既没有限制**中间状态数**，
/// 也没有限制**边尝试次数**，而内存恰恰花在"每条状态 clone 一个
/// `Vec<CodeUnitId>` 再塞进 `HashSet`/`BinaryHeap`"上。
///
/// 这四项合起来构成真正的资源合同：
///
/// | 字段 | 限制什么 | 谁在花 |
/// | --- | --- | --- |
/// | `max_results` | 最终候选切分条数 | 词库查询次数 |
/// | `max_units` | 一条切分的编码单元数 | 单条路径长度 |
/// | `max_states` | **搜索图节点数**（arena 长度） | 内存主项 |
/// | `max_work` | **边尝试次数**（CPU） | 单键耗时 |
///
/// 超限的行为是**确定的降级**：停止探索、置 `truncated`、返回已经找到的
/// 最优结果（best-first ⇒ 最像用户本意的那些先被找到）。
/// **绝不 panic，也绝不停不下来。**
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpansionLimits {
    /// 最终产出多少条切分。
    pub max_results: usize,
    /// 一条切分最多几个编码单元。
    pub max_units: usize,
    /// 搜索图最多几个状态（arena 节点）。
    pub max_states: usize,
    /// 最多尝试多少次边。
    pub max_work: usize,
}

/// 默认的搜索状态上界。
///
/// # 取值是**实测反推**的，不是拍脑袋
///
/// 在真实默认拼音方案（399 个音节 + `abbrev(take=1)`）的拼写表上，
/// 用 arena + 父指针的实现测（同一台机器，debug 构建）：
///
/// | 输入 | 状态上界 | 需要的边尝试 | 搜索图字节 | 召回 |
/// | --- | --- | --- | --- | --- |
/// | `nihao` | 4096 | 448 | 6 KB | `ni hao` rank 0 |
/// | `nh` | 4096 | 959 | 12 KB | `ni hao` rank 175 |
/// | `nhao` | 4096 | 6724 | 153 KB | **被截断，召回丢失** |
/// | `nhao` | **16384** | 9671 | 134 KB | `ni hao` rank 9（与修复前一致） |
/// | `ssss` | 16384 | 32737 | 775 KB | 无（旧实现 2.29 s / 209 MiB） |
/// | `woaizhongguo` | 16384 | 31511 | 731 KB | 无（旧实现 580 ms） |
///
/// 也就是说：**4096 太小**——`nhao`（方案注释里承诺的简拼写法）
/// 会在探索到它之前被截断，而那是"用性能换掉了正确的候选"，不可接受。
/// `16384` 足够保住全部修复前的召回，同时把搜索图压在 1 MB 以内。
///
/// 注意这里的对照是**修复前**的实现：它在 `nh` 上排到 rank 175、
/// 在 `nhao` 上排到 rank 9，两者都必须继续工作。
pub const DEFAULT_MAX_STATES: usize = 16_384;

/// 默认的边尝试上界。
///
/// 一次边尝试是"切片比较 + 一次 push"，实测 ~30–60 ns。上表里最坏
/// 的一次是 `ssss` 的 32737 次——`131072` 留了四倍余量，同时把
/// 单键 CPU 压死在 10 ms 红线之下（debug 实测 2.4 ms，release 更低）。
pub const DEFAULT_MAX_WORK: usize = 131_072;

impl Default for ExpansionLimits {
    fn default() -> Self {
        Self {
            // 512 不是随手填的：它决定"哪些切分能进入词库查询"。
            // 实测（默认方案 399 个音节 + 一条缩写规则）：`nihao` 要出
            // `ni hao`（rank 0），`nh` 要到 rank 4 才出现 `n hao`——
            // 而 64 的上限**根本走不到那里**，结果是「你好」这个 40 万
            // 词条词库里存在的词**打不出来**。
            max_results: 512,
            max_units: 16,
            max_states: DEFAULT_MAX_STATES,
            max_work: DEFAULT_MAX_WORK,
        }
    }
}

impl ExpansionLimits {
    /// 四项都显式给出。
    #[must_use]
    pub const fn new(
        max_results: usize,
        max_units: usize,
        max_states: usize,
        max_work: usize,
    ) -> Self {
        Self {
            max_results,
            max_units,
            max_states,
            max_work,
        }
    }

    /// 每一项至少为 1，否则搜索连一步都走不了。
    #[must_use]
    fn sane(self) -> Self {
        Self {
            max_results: self.max_results.max(1),
            max_units: self.max_units.max(1),
            max_states: self.max_states.max(1),
            max_work: self.max_work.max(1),
        }
    }
}

/// 一次展开的**可观测计数**。
///
/// 它是"资源上界是否成立"的唯一证据来源：测试断言的是这些数字的**上界**，
/// 而不是某台机器上某次运行的墙钟时间（后者不可复现，CI 上尤其不可靠）。
/// 时间只在受控基准里报告。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExpansionStats {
    /// 推进搜索图的节点数（arena 长度）。
    pub states_pushed: usize,
    /// 出队的节点数。
    pub states_popped: usize,
    /// 尝试过的边数（CPU 主项）。
    pub edge_attempts: usize,
    /// 到达输入末尾、被收下的切分数（截断前）。
    pub results: usize,
    /// 搜索图常驻字节数（arena + 前沿）。
    pub graph_bytes: usize,
    /// 是否触碰过任一预算。**true 表示结果可能不完整**。
    pub truncated: bool,
}

impl ExpansionStats {
    /// 是否在预算内完成。
    #[must_use]
    pub fn within_budget(&self) -> bool {
        !self.truncated
    }
}

/// 搜索图里的一个节点：**父指针 + 一步边**，而不是一整条路径的副本。
///
/// 旧实现每个状态持有一个 `Vec<CodeUnitId>`（24 字节栈 + 堆分配 + 内容），
/// 并且为了去重再往 `HashSet` 里放一份。状态数上万时，光是这些副本
/// 就是百 MiB 量级——而它们**全都是中间状态**，一条都不会上屏。
///
/// 改成 arena 之后，一个状态是定长的 24 字节；编码靠回溯父指针重建，
/// **只对最终结果重建**。
#[derive(Clone, Copy, Debug)]
struct SearchNode {
    /// 父节点下标；[`NO_PARENT`] 表示根。
    parent: u32,
    /// 从父节点走过来的那条边对应哪个编码单元。
    unit: CodeUnitId,
    /// 根到这里的累计代价。
    cost: Score,
    /// 根到这里的累计属性。
    attr: SpellingAttr,
    /// 已经消费掉的**字节**位置。
    pos: u32,
    /// 已经走了几个编码单元。
    depth: u32,
}

/// 根节点的 `parent` 值。
const NO_PARENT: u32 = u32::MAX;

/// 前沿（frontier）里的一个待扩展状态。
///
/// 排序键是 `(代价, 单元数, 入队序号)`。与最终排序键的前两项一致，
/// 因此"名额"总是先给最像用户本意的切分；第三项只是**确定性**的兜底
/// （同一份输入永远得到同一批结果）。
///
/// 完整的字典序比较需要把整条编码重建出来——那是旧实现每步都在付的
/// 代价。这里换来的是：**中间状态的内存与一条路径的长度无关**。
#[derive(PartialEq, Eq)]
struct Frontier {
    cost: Score,
    depth: u32,
    /// arena 下标。
    node: u32,
    /// 入队序号（确定性兜底）。
    seq: u64,
}

impl Ord for Frontier {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.cost
            .cmp(&other.cost)
            .then_with(|| other.depth.cmp(&self.depth))
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

impl PartialOrd for Frontier {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// 投影过程中的一个"有效拼写"（装载期用，与按键路径无关）。
#[derive(Clone, Debug)]
struct Projected {
    text: String,
    unit: CodeUnitId,
    cost: Score,
    attr: SpellingAttr,
}

impl SpellingTable {
    /// 由字母表与运算规则编译出拼写表。
    ///
    /// # 算法：这就是 RIME 的"投影"
    ///
    /// 记音节表为 `A`。初始拼写法是恒等映射 `Sa = (A → A)`。
    /// 每一条规则是一个**投影**：对当前有效拼写集合里的每一个拼写施加一次
    /// 拼写运算，得到新的有效拼写集合，并重新建立它与 `A` 的映射。
    ///
    /// **每条规则只施加一次，从左到右，不做不动点迭代。**
    /// 规则顺序**就是语义**——RIME 作者的原话：
    /// 「模糊音定義先於簡拼定義，可令簡拼支持以上模糊音」。
    ///
    /// # Arguments / 参数
    /// * `alphabet` — 方案的编码字母表。
    /// * `rules` — 按序施加的运算规则；**空列表等价于只有规范拼写**。
    #[must_use]
    pub fn compile(alphabet: qingjian_core::CodeAlphabet, rules: &[Rule]) -> Self {
        // ① 恒等映射 Sa = (A → A)。
        let mut current: Vec<Projected> = Vec::with_capacity(alphabet.len() * 2);
        for i in 0..alphabet.len() {
            #[allow(clippy::cast_possible_truncation)]
            let unit = CodeUnitId(i as u32);
            if let Some(t) = alphabet.text(unit) {
                current.push(Projected {
                    text: t.to_owned(),
                    unit,
                    cost: Score::ZERO,
                    attr: SpellingAttr::NORMAL,
                });
            }
        }

        // ② 逐条投影。
        for rule in rules {
            let mut next: Vec<Projected> = Vec::with_capacity(current.len() * 2);
            for p in &current {
                apply_rule(rule, p, &mut next);
            }
            current = next;
        }

        // ③ 收成边，并**确定性排序**——顺序会影响展开顺序，
        //    而展开顺序会影响候选的插入顺序（PLAN §5.2 可复现）。
        let mut edges: Vec<UnitEdge> = current
            .into_iter()
            .map(|p| UnitEdge {
                text: p.text,
                unit: p.unit,
                attr: p.attr,
                cost: p.cost,
            })
            .collect();
        edges.sort_by(|a, b| {
            a.text
                .cmp(&b.text)
                .then_with(|| a.unit.cmp(&b.unit))
                .then_with(|| a.attr.bits().cmp(&b.attr.bits()))
        });
        edges.dedup_by(|a, b| a.text == b.text && a.unit == b.unit && a.attr == b.attr);

        let mut by_first_char: BTreeMap<char, Vec<usize>> = BTreeMap::new();
        for (idx, e) in edges.iter().enumerate() {
            if let Some(c) = e.text.chars().next() {
                by_first_char.entry(c).or_default().push(idx);
            }
        }

        Self {
            alphabet,
            edges,
            by_first_char,
            limits: ExpansionLimits::default(),
        }
    }

    /// 设置展开的硬预算（测试与调参用）。
    #[must_use]
    pub fn with_limits(mut self, max_expansions: usize, max_units: usize) -> Self {
        self.limits.max_results = max_expansions.max(1);
        self.limits.max_units = max_units.max(1);
        self
    }

    /// 设置**全部**预算（测试与基准用）。
    #[must_use]
    pub fn with_budget(mut self, limits: ExpansionLimits) -> Self {
        self.limits = limits.sane();
        self
    }

    /// 当前的硬预算。
    #[must_use]
    pub fn limits(&self) -> ExpansionLimits {
        self.limits
    }

    /// 是否存在某条拼写边（测试与调试用）。
    ///
    /// 它只回答"这个拼写能不能被识别"，不回答"对应哪个编码"——
    /// 后者由 [`Self::expand_into`] 给出。
    #[must_use]
    pub fn looks_up(&self, spelling: &str) -> bool {
        let mut buf = Vec::new();
        let mut sink = ExpansionSink::new(&mut buf, 8);
        self.expand_into(spelling, &mut sink);
        !buf.is_empty()
    }

    /// 字母表。
    #[must_use]
    pub fn alphabet(&self) -> &qingjian_core::CodeAlphabet {
        &self.alphabet
    }

    /// 拼写表里的边数（供测试与调试）。
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// 走一遍按代价排序的搜索图，对每个出队节点调用 `visit`。
    ///
    /// `visit(pos, code, cost, attr)` 返回 `false` 表示"够了，停下"。
    ///
    /// # 为什么把搜索抽成这一层
    ///
    /// 两个调用方要的是**同一个搜索的不同切片**：
    ///
    /// - [`SpellingTable::expand_into`]：只要 `pos == 输入长度` 的节点
    ///   （"恰好消费完整串"）；
    /// - [`qingjian_core::Spelling::expand_paths`]：**每个** `pos > 0` 的节点
    ///   （"消费了多远"是候选的一部分）。
    ///
    /// 抽出来之后，"资源上界"这件事只有一处实现——两条路都受同一组
    /// 硬预算约束，不会出现"新加的那条路忘了限流"。
    fn walk<F>(
        &self,
        spelling: &str,
        limits: PathLimits,
        max_units: usize,
        mut visit: F,
    ) -> ExpansionStats
    where
        F: FnMut(usize, &[CodeUnitId], Score, SpellingAttr) -> bool,
    {
        let mut stats = ExpansionStats::default();
        if spelling.is_empty() {
            return stats;
        }

        // 搜索图：节点是 (位置, 走过的边序列)，父指针重建序列。
        //
        // **不再用 `HashSet<(pos, Vec<CodeUnitId>)>` 去重**：那份去重表
        // 每插入一个状态就要 clone 一整条编码，而它换来的只是"重复状态
        // 少扩展一次"。在 `max_states` 硬限之下，重复状态只花预算、
        // 不破坏正确性（最终 `dedup_by` 会去掉重复结果），而内存从
        // "每条路径一份堆分配"降到"每状态 24 字节"。
        let mut arena: Vec<SearchNode> = Vec::with_capacity(limits.max_states.min(1024));
        arena.push(SearchNode {
            parent: NO_PARENT,
            unit: CodeUnitId(0),
            cost: Score::ZERO,
            attr: SpellingAttr::NORMAL,
            pos: 0,
            depth: 0,
        });
        stats.states_pushed = 1;

        let mut heap: BinaryHeap<Frontier> = BinaryHeap::new();
        heap.push(Frontier {
            cost: Score::ZERO,
            depth: 0,
            node: 0,
            seq: 0,
        });
        let mut seq: u64 = 1;

        while let Some(f) = heap.pop() {
            if stats.edge_attempts >= limits.max_work || stats.states_pushed >= limits.max_states {
                stats.truncated = true;
                break;
            }
            stats.states_popped += 1;
            let node = arena[f.node as usize];

            if node.pos > 0 {
                let code = rebuild_code(&arena, f.node);
                stats.results += 1;
                if !visit(node.pos as usize, &code, node.cost, node.attr) {
                    break;
                }
            }

            if node.pos as usize == spelling.len() || node.depth as usize >= max_units {
                continue;
            }
            let Some(c) = spelling[node.pos as usize..].chars().next() else {
                continue;
            };
            let Some(cands) = self.by_first_char.get(&c) else {
                continue;
            };

            let rest = &spelling[node.pos as usize..];
            for &idx in cands {
                if stats.edge_attempts >= limits.max_work
                    || stats.states_pushed >= limits.max_states
                {
                    stats.truncated = true;
                    break;
                }
                stats.edge_attempts += 1;
                let edge = &self.edges[idx];
                let (next_pos, extra_attr) = if rest.starts_with(&edge.text) {
                    // 正常：边文本被输入完整覆盖。
                    (node.pos as usize + edge.text.len(), SpellingAttr::NORMAL)
                } else if limits.completion
                    && edge.text.len() > rest.len()
                    && edge.text.starts_with(rest)
                {
                    // **拼写层补全**：剩余输入是这条边的**前缀**。
                    // 把它当作"用户还没敲完"，消费到输入末尾，
                    // 并标上 COMPLETION（于是候选会扣一次可信度）。
                    (spelling.len(), SpellingAttr::COMPLETION)
                } else {
                    continue;
                };
                // 位置按字节推进；边文本是 UTF-8，因此不会切在多字节中间。
                #[allow(clippy::cast_possible_truncation)]
                let next_pos = next_pos as u32;
                #[allow(clippy::cast_possible_truncation)]
                let child = arena.len() as u32;
                arena.push(SearchNode {
                    parent: f.node,
                    unit: edge.unit,
                    cost: node.cost.saturating_add(edge.cost),
                    attr: node.attr.union(edge.attr).union(extra_attr),
                    pos: next_pos,
                    depth: node.depth + 1,
                });
                stats.states_pushed += 1;
                heap.push(Frontier {
                    cost: node.cost.saturating_add(edge.cost),
                    depth: node.depth + 1,
                    node: child,
                    seq,
                });
                seq += 1;
            }
        }

        // **不再往 heap / arena 之外分配**：图字节数就是 arena + 前沿。
        stats.graph_bytes = arena.len() * core::mem::size_of::<SearchNode>()
            + heap.len() * core::mem::size_of::<Frontier>();
        stats
    }

    /// 把一段拼写展开成所有可能的编码切分。
    ///
    /// # 算法
    ///
    /// 在"位置"上做**按代价排序的图搜索（best-first）**：每个位置找出所有
    /// 能匹配上的边，前进到新位置。到达末尾的路径就是一条展开结果。
    ///
    /// **为什么要「所有」而不是「最优的一条」**：一条拼写可能对应多个编码
    /// （`nh` 既能是 `ni hao`，也能是 `na hao`），而**只有词库才知道哪个真有词**。
    /// 所以这里给出一族候选，由词库那一侧决出胜负——这也正是
    /// RIME 的 `script_translator` 在音节图上做查询的方式。
    ///
    /// 输出顺序确定（按"代价降序、编码单元数升序、编码字典序、属性"），
    /// 因此可复现（PLAN §5.2）。
    ///
    /// # Errors / 错误
    ///
    /// 无。无法切分时 `out` 保持为空——由兜底翻译器保证"敲的东西总能上屏"。
    ///
    /// # 资源上界（P0-A）
    ///
    /// 见 [`ExpansionLimits`]。搜索图是 arena + 父指针，**中间状态的内存
    /// 与路径长度无关**；探索被 `max_states` / `max_work` 双向硬限。
    /// 需要观测这些数字时用 [`Self::expand_into_with_stats`]。
    pub fn expand_into(&self, spelling: &str, out: &mut ExpansionSink<'_>) {
        let _ = self.expand_into_with_stats(spelling, out);
    }

    /// 同 [`Self::expand_into`]，但把这一趟的**可观测计数**交出来。
    ///
    /// 测试与基准用它断言资源上界（状态数、边尝试数、图字节数），
    /// 而不是断言墙钟时间——后者在 CI 上不可复现。
    pub fn expand_into_with_stats(
        &self,
        spelling: &str,
        out: &mut ExpansionSink<'_>,
    ) -> ExpansionStats {
        let limits = self.limits;
        if spelling.is_empty() {
            return ExpansionStats::default();
        }

        // 收下**全部到达末尾**的结果；最后统一排序再按 `max_results` 截断。
        //
        // # 为什么不在收满 `max_results` 时就停
        //
        // 旧实现是"一堆满就走"。而堆是按 (代价, 单元数) 出队的，同代价同
        // 单元数的切分之间没有全序保证——一旦收满的时机落在某个并列组中间，
        // **想要的词可能恰好没进那 512 条**，症状是"某些词偶尔打不出来"。
        // 现在探索只受 `max_states`/`max_work` 限，截断发生在**按真实排序键
        // 排好之后**，于是"该留哪 512 条"由语义决定，不由探索顺序决定。
        let mut done: Vec<Expansion> = Vec::new();
        let stats = self.walk(
            spelling,
            PathLimits::new(usize::MAX, limits.max_states, limits.max_work),
            limits.max_units,
            |pos, code, cost, attr| {
                if pos == spelling.len() {
                    done.push(Expansion {
                        code: code.to_vec(),
                        cost,
                        attr,
                    });
                }
                true
            },
        );

        // 排序：**代价降序 → 编码单元数升序 → 编码字典序 → 属性**。
        //
        // # "编码单元数升序"这一条是实测加上的
        //
        // 它对应"**最长匹配优先**"这条输入法常识：同一个拼写能被切成
        // `[ni][ha][ao]` 与 `[ni][hao]` 时，后者才是人想要的。
        // 少了这一条时，两者代价相同（同一批边的组合），于是退化成
        // 按编码编号排序——**预编辑串会显示 `ni ha ao`**，
        // 而候选中却有正确的词。端到端测试抓到了它
        // （`the_input_is_cut_into_labelled_segments`）。
        //
        // 注意它只是**并列时的次序**：真正决定优劣的仍然是代价，
        // 因为代价是方案数据说了算的东西（简拼该罚多少是方案的判断）。
        done.sort_by(|a, b| {
            b.cost
                .cmp(&a.cost)
                .then_with(|| a.code.len().cmp(&b.code.len()))
                .then_with(|| a.code.cmp(&b.code))
                .then_with(|| a.attr.bits().cmp(&b.attr.bits()))
        });
        done.dedup_by(|a, b| a.code == b.code && a.cost == b.cost && a.attr == b.attr);

        for e in done.into_iter().take(limits.max_results) {
            out.push(e);
        }
        stats
    }
}

/// 由父指针重建一条编码（从根到 `node`）。
///
/// 只在**到达输入末尾**（即真的产出一条结果）时调用，因此调用次数
/// 上界是结果数，而不是状态数——这正是把路径副本换成父指针的收益。
fn rebuild_code(arena: &[SearchNode], node: u32) -> Vec<CodeUnitId> {
    let mut out = Vec::new();
    let mut cur = node;
    while cur != NO_PARENT {
        let n = arena[cur as usize];
        // 根是哨兵：它没有"进来的边"，`unit` 字段无意义。
        if n.parent == NO_PARENT {
            break;
        }
        out.push(n.unit);
        cur = n.parent;
    }
    out.reverse();
    out
}

impl qingjian_core::Spelling for SpellingTable {
    fn alphabet(&self) -> &qingjian_core::CodeAlphabet {
        &self.alphabet
    }

    fn expand(&self, spelling: &str, out: &mut ExpansionSink<'_>) {
        self.expand_into(spelling, out);
    }

    /// 带消费长度的展开：**每一个到达过的位置都算一条路径**。
    ///
    /// # 与 [`SpellingTable::expand_into`] 的唯一差别
    ///
    /// `expand_into` 只在 `pos == len` 时收结果（"恰好消费完整串"）。
    /// 这里在**每个非根节点**都收——节点带着"已经消费了多少字节"，
    /// 于是"只解释了前缀"的路径（`niha` 的 `ni`+`h`，消费 3 字节）
    /// 自然出现在结果里。
    ///
    /// librime 的 `Dictionary::Lookup` 正是对音节图**每一条边**分别查表、
    /// 返回 `map<end_pos, entries>`——"消费了多远"是候选的一部分，
    /// 而不是被丢掉的信息（`gear/script_translator.cc:705-722`）。
    ///
    /// # 排序
    ///
    /// **消费得多**的优先，其次代价高、单元数少、编码字典序。
    /// 前端于是可以"先给覆盖整串的候选，再给前缀候选"。
    fn expand_paths(&self, spelling: &str, limits: PathLimits, out: &mut PathSink<'_>) {
        let mut found: Vec<SpellingPath> = Vec::new();
        // `walk` 只在 `pos > 0` 的节点上回调，所以根不会进来。
        // 回调返回 `false` 即"收够了"——这样就不必为每个状态都重建一次编码。
        let _ = self.walk(
            spelling,
            limits,
            self.limits.max_units,
            |pos, code, cost, attr| {
                found.push(SpellingPath {
                    code: code.to_vec(),
                    consumed: pos,
                    cost,
                    attr,
                });
                found.len() < limits.max_paths
            },
        );
        found.sort_by(|a, b| {
            b.consumed
                .cmp(&a.consumed)
                .then_with(|| b.cost.cmp(&a.cost))
                .then_with(|| a.code.len().cmp(&b.code.len()))
                .then_with(|| a.code.cmp(&b.code))
                .then_with(|| a.attr.bits().cmp(&b.attr.bits()))
        });
        found.dedup_by(|a, b| {
            a.code == b.code && a.consumed == b.consumed && a.cost == b.cost && a.attr == b.attr
        });
        for p in found.into_iter().take(limits.max_paths) {
            out.push(p);
        }
    }
}

/// 对**一个**有效拼写施加一条运算，把结果追加进 `out`。
///
/// `out` 里同时保留原拼写（除非是 `erase`）——这正是"派生"的语义：
/// **派生前后的拼写都有效**。
fn apply_rule(rule: &Rule, p: &Projected, out: &mut Vec<Projected>) {
    match rule {
        Rule::Xlit { from, to } => {
            // **`xlit` 是改写，不是派生**（上游 wiki + librime
            // `Transliteration::Apply`）。这里**不再**把原拼写推进 `out`：
            // 推进去就等于宣称 `xlit/a/b/` 之后 `aa` 与 `bb` 都有效，
            // 而 librime 里只有 `bb` 有效。
            //
            // 代价与属性随原拼写保留（`..p.clone()`）：转写不改变
            // "这条拼写有多可信"。
            let text: String = p
                .text
                .chars()
                .map(|c| match from.iter().position(|f| *f == c) {
                    Some(i) => to.get(i).copied().unwrap_or(c),
                    None => c,
                })
                .collect();
            out.push(Projected { text, ..p.clone() });
        }
        Rule::Xform { pattern, repl } => {
            let text = pattern.replace_all(&p.text, repl);
            if text == p.text {
                // 没变化：原拼写仍有效。
                out.push(p.clone());
            } else {
                // `xform` 是**改写**：原拼写在新的拼写法里不再有效。
                // （RIME 的 `xform/^([nl])ue$/$1ve/` 会使 `nue` 不可用——这是文档明说的。）
                out.push(Projected { text, ..p.clone() });
            }
        }
        Rule::Equivalence { pairs, cost } => {
            out.push(p.clone());
            let mut changed = false;
            let text: String = p
                .text
                .chars()
                .map(|c| match pairs.iter().find(|(a, _)| *a == c) {
                    Some((_, b)) => {
                        changed = true;
                        *b
                    }
                    None => c,
                })
                .collect();
            if changed && text != p.text {
                out.push(Projected {
                    text,
                    cost: p.cost.saturating_add(*cost),
                    attr: p.attr.union(SpellingAttr::FUZZY),
                    unit: p.unit,
                });
            }
        }
        Rule::Erase { pattern } => {
            // 全匹配才消除；否则原样保留。
            if !pattern.is_full_match(&p.text) {
                out.push(p.clone());
            }
        }
        Rule::Derive {
            pattern,
            repl,
            cost,
            attr,
        } => {
            out.push(p.clone());
            let text = pattern.replace_all(&p.text, repl);
            if text != p.text {
                out.push(Projected {
                    text,
                    cost: p.cost.saturating_add(*cost),
                    attr: p.attr.union(*attr),
                    unit: p.unit,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alphabet(units: &[&str]) -> qingjian_core::CodeAlphabet {
        qingjian_core::CodeAlphabet::new(units.iter().map(|s| (*s).to_owned()).collect())
    }

    fn expand_all(table: &SpellingTable, spelling: &str) -> Vec<Expansion> {
        let mut buf = Vec::new();
        let mut sink = ExpansionSink::new(&mut buf, 64);
        table.expand_into(spelling, &mut sink);
        buf
    }

    fn code_texts<'a>(table: &'a SpellingTable, e: &Expansion) -> Vec<&'a str> {
        e.code
            .iter()
            .map(|u| table.alphabet().text(*u).unwrap_or("?"))
            .collect()
    }

    #[test]
    fn identity_segments_a_canonical_spelling() {
        let t = SpellingTable::compile(alphabet(&["ni", "hao"]), &[]);
        let got = expand_all(&t, "nihao");
        assert_eq!(got.len(), 1);
        assert_eq!(code_texts(&t, &got[0]), ["ni", "hao"]);
        assert_eq!(got[0].cost, Score::ZERO);
        assert_eq!(got[0].attr, SpellingAttr::NORMAL);
    }

    #[test]
    fn identity_directly_resolves_ambiguity_by_edges() {
        // 「xian」既能是 [xian] 也能是 [xi][an] —— 两条边都要给出来，
        // 由词库决定谁真有词。
        let t = SpellingTable::compile(alphabet(&["xi", "an", "xian"]), &[]);
        let got = expand_all(&t, "xian");
        let segs: Vec<Vec<&str>> = got.iter().map(|e| code_texts(&t, e)).collect();
        assert!(segs.contains(&vec!["xian"]), "{segs:?}");
        assert!(segs.contains(&vec!["xi", "an"]), "{segs:?}");
    }

    #[test]
    fn abbrev_rule_lets_nh_reach_ni_hao() {
        let t = SpellingTable::compile(
            alphabet(&["ni", "na", "hao"]),
            &[Rule::abbrev(1, Score::from_weight(0.5)).unwrap()],
        );
        let got = expand_all(&t, "nh");
        let segs: Vec<Vec<&str>> = got.iter().map(|e| code_texts(&t, e)).collect();
        // 缩写的歧义：n 既可能是 ni 也可能是 na —— 两条都要给出。
        assert!(segs.contains(&vec!["ni", "hao"]), "{segs:?}");
        assert!(segs.contains(&vec!["na", "hao"]), "{segs:?}");
        // 属性必须是 ABBREV，分数必须是负的（有代价）。
        for e in &got {
            assert!(e.attr.contains(SpellingAttr::ABBREV));
            assert!(e.cost < Score::ZERO);
        }
    }

    #[test]
    fn canonical_spelling_beats_abbreviation_on_cost() {
        let t = SpellingTable::compile(
            alphabet(&["ni", "hao"]),
            &[Rule::abbrev(1, Score::from_weight(0.5)).unwrap()],
        );
        let got = expand_all(&t, "nihao");
        // 规范拼写代价 0，缩写代价为负 —— 规范拼写必须排在最前。
        assert_eq!(code_texts(&t, &got[0]), ["ni", "hao"]);
        assert_eq!(got[0].cost, Score::ZERO);
        assert_eq!(got[0].attr, SpellingAttr::NORMAL);
    }

    #[test]
    fn equivalence_rule_creates_a_fuzzy_edge() {
        let t = SpellingTable::compile(
            alphabet(&["zhao"]),
            &[Rule::equivalence(&[('z', 'z'), ('h', 'h')], Score::ZERO)],
        );
        // 映射到自身不算变化，因此只有规范边。
        assert_eq!(t.edge_count(), 1);

        let t2 = SpellingTable::compile(
            alphabet(&["zhang"]),
            &[Rule::equivalence(&[('h', ' ')], Score::ZERO)],
        );
        // 'h' → ' ' 会产生一条带 FUZZY 属性的边。
        let got = expand_all(&t2, "z ang");
        assert!(got.iter().any(|e| e.attr.contains(SpellingAttr::FUZZY)));
    }

    #[test]
    fn expansion_is_deterministic() {
        let t = SpellingTable::compile(
            alphabet(&["ni", "na", "hao", "he"]),
            &[Rule::abbrev(1, Score::from_weight(0.5)).unwrap()],
        );
        let first = expand_all(&t, "nh");
        for _ in 0..50 {
            let again = expand_all(&t, "nh");
            assert_eq!(
                first.iter().map(|e| e.code.clone()).collect::<Vec<_>>(),
                again.iter().map(|e| e.code.clone()).collect::<Vec<_>>(),
            );
        }
    }

    #[test]
    fn unresolvable_spelling_yields_nothing() {
        let t = SpellingTable::compile(alphabet(&["ni", "hao"]), &[]);
        assert!(expand_all(&t, "zzz").is_empty());
        assert!(expand_all(&t, "").is_empty());
    }
}

#[cfg(test)]
mod algebra_tests {
    use super::*;

    fn alphabet(units: &[&str]) -> qingjian_core::CodeAlphabet {
        qingjian_core::CodeAlphabet::new(units.iter().map(|s| (*s).to_owned()).collect())
    }

    #[test]
    fn parses_real_rime_rule_syntax() {
        // 这几条直接抄自 rime-ice 的 speller/algebra。
        assert!(matches!(
            Rule::parse("xform/^([nl])ue$/$1ve/").unwrap(),
            Rule::Xform { .. }
        ));
        assert!(matches!(
            Rule::parse("derive/^([zcs])h/$1/").unwrap(),
            Rule::Derive { .. }
        ));
        assert!(matches!(
            Rule::parse("abbrev/^([a-z]).+$/$1/").unwrap(),
            Rule::Derive { attr, .. } if attr == SpellingAttr::ABBREV
        ));
        assert!(matches!(
            Rule::parse("fuzz/^([zcs])h/$1/").unwrap(),
            Rule::Derive { attr, .. } if attr == SpellingAttr::FUZZY
        ));
        assert!(matches!(
            Rule::parse("erase/^hm$/").unwrap(),
            Rule::Erase { .. }
        ));
        assert!(matches!(
            Rule::parse("xlit/abc/ABC/").unwrap(),
            Rule::Xlit { .. }
        ));
    }

    #[test]
    fn rule_errors_explain_themselves() {
        let e = Rule::parse("telepathy/a/b/").unwrap_err();
        assert!(e.to_string().contains("xlit"), "{e}");
        assert!(e.to_string().contains("xform"), "{e}");

        // `xform/^a$` 只给了一个参数（替换式缺失）。
        let e2 = Rule::parse("xform/^a$").unwrap_err();
        assert!(e2.to_string().contains("2 个参数"), "{e2}");

        let e3 = Rule::parse("xlit/ab/CDE/").unwrap_err();
        assert!(e3.to_string().contains("长度"), "{e3}");

        assert_eq!(Rule::parse("").unwrap_err(), RuleError::Empty);
    }

    #[test]
    fn xform_removes_the_original_spelling() {
        // RIME 文档：「`xform/^([nl])ue$/$1ve/` 使 `nue` 不再可用」。
        let t = SpellingTable::compile(
            alphabet(&["nue"]),
            &[Rule::parse("xform/^([nl])ue$/$1ve/").unwrap()],
        );
        // 只有 `nve` 有效，`nue` 被改写掉了。
        assert_eq!(t.edge_count(), 1);
        assert!(t.looks_up("nve"));
        assert!(!t.looks_up("nue"), "xform 是改写，原拼写应当失效");
    }

    #[test]
    fn derive_keeps_both_spellings() {
        // `derive/^([zcs])h/$1/`：`zhang` 与 `zang` **都**有效。
        let t = SpellingTable::compile(
            alphabet(&["zhang"]),
            &[Rule::parse("derive/^([zcs])h/$1/").unwrap()],
        );
        assert_eq!(t.edge_count(), 2);
        assert!(t.looks_up("zhang"));
        assert!(t.looks_up("zang"));
    }

    #[test]
    fn erase_removes_only_an_exact_match() {
        let t = SpellingTable::compile(
            alphabet(&["hm", "hmm"]),
            &[Rule::parse("erase/^hm$/").unwrap()],
        );
        assert!(!t.looks_up("hm"));
        assert!(t.looks_up("hmm"), "erase 是全匹配，hmm 不该被删");
    }

    #[test]
    fn rule_order_is_semantics() {
        // 这是 RIME 的"投影"里最容易忽略的一点：**每条规则只施加一次，
        // 从左到右，不做不动点迭代**，因此顺序就是语义。
        //
        // 例：`ni` 先缩写得到 `n`，再被 `xform/^n$/m/` 改写成 `m`；
        // 反过来先改写时 `^n$` 匹配不上 `ni`，于是缩写仍得到 `n`。
        let abbrev = Rule::abbrev(1, Score::ZERO).unwrap();
        let rewrite = Rule::parse("xform/^n$/m/").unwrap();

        let a = SpellingTable::compile(alphabet(&["ni"]), &[abbrev.clone(), rewrite.clone()]);
        let b = SpellingTable::compile(alphabet(&["ni"]), &[rewrite, abbrev]);

        assert!(a.looks_up("ni"));
        assert!(a.looks_up("m"), "先缩写后改写：`ni`→`n`→`m`");
        assert!(!a.looks_up("n"), "`n` 已被改写掉");

        assert!(b.looks_up("ni"));
        assert!(
            b.looks_up("n"),
            "先改写后缩写：`^n$` 匹配不上 `ni`，缩写仍得 `n`"
        );
        assert!(!b.looks_up("m"), "`m` 不会出现");

        // 同样的规则、同样的数据，只因为顺序不同，**有效拼写集合就不同**。
        // （注意：边数可能恰好相同——这里两条边 vs 两条边——所以判据是
        //  "哪些拼写有效"，不是"有几条"。这正是上面四个 looks_up 断言在做的。）
        assert_eq!(a.edge_count(), 2, "先缩写后改写：{{ni, m}}");
        assert_eq!(b.edge_count(), 2, "先改写后缩写：{{ni, n}}");
    }

    #[test]
    fn xlit_is_a_rewrite_not_a_derivation() {
        // 上游语义：`xlit/a/b/` 之后 `bb` 有效、`aa` **无效**。
        // 旧实现把 `xlit` 当派生，两个都有效——与 librime 不一致。
        let t = SpellingTable::compile(alphabet(&["aa"]), &[Rule::parse("xlit/a/b/").unwrap()]);
        assert!(t.looks_up("bb"), "转写结果必须有效");
        assert!(
            !t.looks_up("aa"),
            "`xlit` 是**改写**：原拼写不再有效（这是与 librime 的对照点）"
        );
        assert_eq!(t.edge_count(), 1, "只应当留下转写后的那一条边");
    }

    #[test]
    fn xlit_unchanged_spelling_stays() {
        // 字母表里没有要转写的字符时，拼写原样保留（不是"消失"）。
        let t = SpellingTable::compile(alphabet(&["ni"]), &[Rule::parse("xlit/a/b/").unwrap()]);
        assert!(t.looks_up("ni"));
        assert_eq!(t.edge_count(), 1);
    }

    #[test]
    fn no_rules_means_canonical_only() {
        let t = SpellingTable::compile(alphabet(&["ni", "hao"]), &[]);
        assert_eq!(t.edge_count(), 2);
        assert!(t.looks_up("ni"));
        assert!(t.looks_up("hao"));
        assert!(!t.looks_up("n"));
    }
}
