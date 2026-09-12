//! # Spelling layer
//!
//! 中文职责：把**用户敲出来的拼写**映射到**编码**，并给出代价与属性。
//! English role: map a typed spelling to codes, with cost and attributes.
//! 架构位置：`stele-core::Spelling` 的实现；翻译器在它之上工作。
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

use std::collections::BTreeMap;
use stele_core::{CodeUnitId, Expansion, ExpansionSink, Score, SpellingAttr};

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
    alphabet: stele_core::CodeAlphabet,
    edges: Vec<UnitEdge>,
    /// 按首字符分组，避免每个位置都遍历整张表。
    by_first_char: BTreeMap<char, Vec<usize>>,
    /// 一次展开最多产出多少条。
    max_expansions: usize,
    /// 一条编码最多几个单元（防止病态输入导致爆炸）。
    max_units: usize,
}

/// 投影过程中的一个"有效拼写"。
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
    pub fn compile(alphabet: stele_core::CodeAlphabet, rules: &[Rule]) -> Self {
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
            max_expansions: 64,
            max_units: 16,
        }
    }

    /// 设置一次展开的上限（测试与调参用）。
    #[must_use]
    pub fn with_limits(mut self, max_expansions: usize, max_units: usize) -> Self {
        self.max_expansions = max_expansions.max(1);
        self.max_units = max_units.max(1);
        self
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
    pub fn alphabet(&self) -> &stele_core::CodeAlphabet {
        &self.alphabet
    }

    /// 拼写表里的边数（供测试与调试）。
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// 把一段拼写展开成所有可能的编码切分。
    ///
    /// # 算法
    ///
    /// 在"位置"上做**宽度优先的图搜索**：每个位置找出所有能匹配上的边，
    /// 前进到新位置。到达末尾的路径就是一条展开结果。
    ///
    /// **为什么要「所有」而不是「最优的一条」**：一条拼写可能对应多个编码
    /// （`nh` 既能是 `ni hao`，也能是 `na hao`），而**只有词库才知道哪个真有词**。
    /// 所以这里给出一族候选，由词库那一侧决出胜负——这也正是
    /// RIME 的 `script_translator` 在音节图上做查询的方式。
    ///
    /// 输出顺序确定（按"代价降序、编码字典序"），因此可复现（PLAN §5.2）。
    ///
    /// # Errors / 错误
    ///
    /// 无。无法切分时 `out` 保持为空——由兜底翻译器保证"敲的东西总能上屏"。
    pub fn expand_into(&self, spelling: &str, out: &mut ExpansionSink<'_>) {
        if spelling.is_empty() {
            return;
        }

        // (位置, 已选编码, 累计代价, 累计属性)
        let mut frontier: Vec<(usize, Vec<CodeUnitId>, Score, SpellingAttr)> =
            vec![(0, Vec::new(), Score::ZERO, SpellingAttr::NORMAL)];
        let mut done: Vec<Expansion> = Vec::new();
        let mut budget = self.max_expansions * self.max_units * 8;

        while let Some((pos, code, cost, attr)) = frontier.pop() {
            if budget == 0 || done.len() >= self.max_expansions {
                break;
            }
            budget -= 1;

            if pos == spelling.len() {
                done.push(Expansion { code, cost, attr });
                continue;
            }
            if code.len() >= self.max_units {
                continue;
            }

            let Some(c) = spelling[pos..].chars().next() else {
                continue;
            };
            let Some(cands) = self.by_first_char.get(&c) else {
                continue;
            };

            for &idx in cands {
                let edge = &self.edges[idx];
                if !spelling[pos..].starts_with(&edge.text) {
                    continue;
                }
                let mut next_code = code.clone();
                next_code.push(edge.unit);
                frontier.push((
                    pos + edge.text.len(),
                    next_code,
                    cost.saturating_add(edge.cost),
                    attr.union(edge.attr),
                ));
            }
        }

        // 代价高的在前（代价是对数域的扣分，0 最好，负数更差）；
        // 代价相同则按编码字典序，保证确定性。
        done.sort_by(|a, b| {
            b.cost
                .cmp(&a.cost)
                .then_with(|| a.code.cmp(&b.code))
                .then_with(|| a.attr.bits().cmp(&b.attr.bits()))
        });
        done.dedup_by(|a, b| a.code == b.code && a.cost == b.cost && a.attr == b.attr);

        for e in done {
            out.push(e);
        }
    }
}

impl stele_core::Spelling for SpellingTable {
    fn alphabet(&self) -> &stele_core::CodeAlphabet {
        &self.alphabet
    }

    fn expand(&self, spelling: &str, out: &mut ExpansionSink<'_>) {
        self.expand_into(spelling, out);
    }
}

/// 对**一个**有效拼写施加一条运算，把结果追加进 `out`。
///
/// `out` 里同时保留原拼写（除非是 `erase`）——这正是"派生"的语义：
/// **派生前后的拼写都有效**。
fn apply_rule(rule: &Rule, p: &Projected, out: &mut Vec<Projected>) {
    match rule {
        Rule::Xlit { from, to } => {
            let mut changed = false;
            let text: String = p
                .text
                .chars()
                .map(|c| match from.iter().position(|f| *f == c) {
                    Some(i) => {
                        changed = true;
                        to.get(i).copied().unwrap_or(c)
                    }
                    None => c,
                })
                .collect();
            out.push(p.clone());
            if changed && text != p.text {
                out.push(Projected { text, ..p.clone() });
            }
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

    fn alphabet(units: &[&str]) -> stele_core::CodeAlphabet {
        stele_core::CodeAlphabet::new(units.iter().map(|s| (*s).to_owned()).collect())
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

    fn alphabet(units: &[&str]) -> stele_core::CodeAlphabet {
        stele_core::CodeAlphabet::new(units.iter().map(|s| (*s).to_owned()).collect())
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
    fn no_rules_means_canonical_only() {
        let t = SpellingTable::compile(alphabet(&["ni", "hao"]), &[]);
        assert_eq!(t.edge_count(), 2);
        assert!(t.looks_up("ni"));
        assert!(t.looks_up("hao"));
        assert!(!t.looks_up("n"));
    }
}
