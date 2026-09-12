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

/// 一条拼写规则的种类。
///
/// # 规范拼写永远是基线，规则只做增加
///
/// RIME 的代数从 `Sa = (A → A)`（每个编码对应自身的恒等映射）出发，
/// 规则在它之上**派生**出更多有效拼写。因此**没有"只允许规范拼写"这种规则**——
/// 规则列表为空就等价于只有规范拼写。
///
/// 早先的版本把 `Identity` 做成了一个需要显式声明的规则，这会导致
/// "只写了一条缩写规则、结果连规范拼写都查不到"这种反直觉的行为。
/// 那是把代数里的**基线**误当成了**规则**。
///
/// **每一类都只作用于"拼写"，不作用于词条**——这是 RIME 的分界线：
/// 拼写运算定义的是"有效拼写集合 → 编码集合"的映射（他们称之为拼写法／正字法）。
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rule {
    /// **缩写**：每个编码单元只取前 `take` 个字符，附加 `ABBREV` 属性。
    ///
    /// 这就是"简拼"：拼音方案里 `ni hao` → `n h`，于是敲 `nh` 也能命中。
    /// **引擎不知道它叫简拼**，只知道这些边的属性是 `ABBREV`、代价是多少。
    Abbrev {
        /// 每个编码单元保留前几个字符。
        take: usize,
        /// 这条边的代价（对数域，通常为负）。
        cost: Score,
    },
    /// **等价替换**：把拼写里的字符按映射替换，附加 `FUZZY` 属性。
    ///
    /// 这就是"模糊音"：`zh` ↔ `z` 之类。同样只是数据。
    Equivalence {
        /// 字符替换表：`(原字符, 替换为)`。
        pairs: Vec<(char, char)>,
        /// 这条边的代价。
        cost: Score,
    },
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

/// 拼写表：把方案数据（字母表 + 规则）编译成"片段 → 编码单元"的边集合。
///
/// **编译发生在装载方案时，不在按键路径上**——这正是 RIME 把棱镜做成
/// 部署期产物的原因（见 `docs/engine-design.md` §5.2）。
#[derive(Debug)]
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

impl SpellingTable {
    /// 由字母表与规则表编译出拼写表。
    ///
    /// # Arguments / 参数
    /// * `alphabet` — 方案的编码字母表（拼音方案是音节表；字形方案是字母表）。
    /// * `rules` — 按序施加的规则；**空列表等价于只有规范拼写**（规范拼写永远是基线）。
    ///
    /// # Returns / 返回
    /// 编译好的拼写表。规范拼写永远在内，规则只在其上派生更多有效拼写。
    #[must_use]
    pub fn compile(alphabet: stele_core::CodeAlphabet, rules: &[Rule]) -> Self {
        let mut edges: Vec<UnitEdge> = Vec::new();

        for i in 0..alphabet.len() {
            #[allow(clippy::cast_possible_truncation)]
            let unit = CodeUnitId(i as u32);
            let Some(canonical) = alphabet.text(unit).map(str::to_owned) else {
                continue;
            };

            // 基线：规范拼写永远在内。
            edges.push(UnitEdge {
                text: canonical.clone(),
                unit,
                attr: SpellingAttr::NORMAL,
                cost: Score::ZERO,
            });

            for rule in rules {
                match rule {
                    Rule::Abbrev { take, cost } => {
                        let short: String = canonical.chars().take(*take).collect();
                        // 缩写与规范拼写相同时不重复建边（例如单字母音节）。
                        if short != canonical {
                            edges.push(UnitEdge {
                                text: short,
                                unit,
                                attr: SpellingAttr::ABBREV,
                                cost: *cost,
                            });
                        }
                    }
                    Rule::Equivalence { pairs, cost } => {
                        if let Some(variant) = apply_equivalence(&canonical, pairs) {
                            if variant != canonical {
                                edges.push(UnitEdge {
                                    text: variant,
                                    unit,
                                    attr: SpellingAttr::FUZZY,
                                    cost: *cost,
                                });
                            }
                        }
                    }
                }
            }
        }

        // 确定性排序：先按文本，再按单元编号。**顺序会影响展开顺序，
        // 而展开顺序会影响候选的插入顺序，因此必须确定**（PLAN §5.2）。
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

/// 把等价替换按序施加到一个字符串上；无变化时返回 `None`。
///
/// **只施加一次、从左到右**，不做不动点迭代——这是 RIME 拼写运算的语义
/// （见 `docs/engine-design.md` §13.3 G14）。
fn apply_equivalence(text: &str, pairs: &[(char, char)]) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    for c in text.chars() {
        match pairs.iter().find(|(from, _)| *from == c) {
            Some((_, to)) => {
                out.push(*to);
                changed = true;
            }
            None => out.push(c),
        }
    }
    if changed {
        Some(out)
    } else {
        None
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
            &[Rule::Abbrev {
                take: 1,
                cost: Score::from_weight(0.5),
            }],
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
            &[Rule::Abbrev {
                take: 1,
                cost: Score::from_weight(0.5),
            }],
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
            &[Rule::Equivalence {
                pairs: vec![('z', 'z'), ('h', 'h')],
                cost: Score::ZERO,
            }],
        );
        // 映射到自身不算变化，因此只有规范边。
        assert_eq!(t.edge_count(), 1);

        let t2 = SpellingTable::compile(
            alphabet(&["zhang"]),
            &[Rule::Equivalence {
                pairs: vec![('h', ' ')],
                cost: Score::ZERO,
            }],
        );
        // 'h' → ' ' 会产生一条带 FUZZY 属性的边。
        let got = expand_all(&t2, "z ang");
        assert!(got.iter().any(|e| e.attr.contains(SpellingAttr::FUZZY)));
    }

    #[test]
    fn expansion_is_deterministic() {
        let t = SpellingTable::compile(
            alphabet(&["ni", "na", "hao", "he"]),
            &[Rule::Abbrev {
                take: 1,
                cost: Score::from_weight(0.5),
            }],
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
