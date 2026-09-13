//! # Component registry
//!
//! 中文职责：按**名字**查找零件——方案文件里写的是名字，注册表把它们翻译成实现。
//! English role: look components up by NAME — schemes name them, the registry maps
//! them to implementations.
//! 架构位置：`stele-engine` 的零件目录；被方案装载器查询。
//!
//! # 为什么需要它（PLAN D17）
//!
//! RIME 的方案这样写：
//!
//! ```yaml
//! engine:
//!   processors:  [ ascii_composer, speller, punctuator, selector, navigator ]
//!   translators: [ script_translator, table_translator@melt_eng ]
//!   filters:     [ simplifier@emoji, uniquifier ]
//! ```
//!
//! **这些是名字，不是代码。** 引擎按名字去注册表里找零件；找不到就报错——
//! 而"哪些名字我们实现了、哪些没有"必须能被**明确地回答**，
//! 否则"跑通某个方案"就成了一句无法验证的话。
//!
//! # 三种"没有"
//!
//! 表里区分三件事，因为它们对使用者的含义完全不同：
//!
//! | 状态 | 含义 | 使用者该做什么 |
//! | --- | --- | --- |
//! | [`Availability::Implemented`] | 我们有 | 无 |
//! | [`Availability::NeedsData`] | 机制有，但**数据是外部的** | 自行提供数据 |
//! | [`Availability::NotYet`] | 还没实现 | 等，或自己写 |
//!
//! **把 `NeedsData` 和 `NotYet` 混为一谈是危险的**：前者是"你缺数据"，
//! 后者是"我们缺代码"。含糊其辞会让使用者白费力气。

/// 零件类别。
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    /// 处理器。
    Processor,
    /// 切分器。
    Segmentor,
    /// 翻译器。
    Translator,
    /// 滤镜。
    Filter,
}

/// 这个零件我们能提供到什么程度。
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Availability {
    /// 已实现，可直接用。
    Implemented,
    /// 机制已实现，但需要**外部数据**才能生效。
    NeedsData,
    /// 还没实现。
    NotYet,
    /// **在我们的架构里不适用**——这是"我们考虑过并决定不做"。
    ///
    /// 与 [`Availability::NotYet`] 的区别很实际：
    ///
    /// - `NotYet` = "我们缺代码，以后可能做"（用户应当等或自己写）
    /// - `NotApplicable` = "**这件事在 Rust 里不存在**"（用户不必等）
    ///
    /// 典型例子是 `force_gc`：Lua 插件需要手动推 GC（解释器堆），
    /// 而 Rust 的候选生命周期由作用域决定。**把它实现成空操作才是最糟的
    /// 选择**——注册表会报"已实现"，而它永远不会有效果。
    NotApplicable,
    /// 名字不认识。
    Unknown,
}

impl Availability {
    /// 一句话说明。
    #[must_use]
    pub fn note(self) -> &'static str {
        match self {
            Self::Implemented => "已实现",
            Self::NeedsData => "机制已实现，但需要外部数据",
            Self::NotYet => "尚未实现",
            Self::NotApplicable => "在我们这个架构里不适用（不是缺口）",
            Self::Unknown => "不认识这个名字",
        }
    }
}

/// 注册表里的一条。
struct Entry {
    name: &'static str,
    slot: Slot,
    availability: Availability,
    note: &'static str,
}

/// 零件表。
///
/// **刻意写成静态表而不是代码里的 `match`**：它同时是一份**清单**——
/// 「我们覆盖了 RIME 的哪些零件」这个问题，应当能一眼看出一半的答案。
const ENTRIES: &[Entry] = &[
    // ── 处理器 ──
    Entry {
        name: "speller",
        slot: Slot::Processor,
        availability: Availability::Implemented,
        note: "把可打印字符收进输入串",
    },
    Entry {
        name: "select_character",
        slot: Slot::Processor,
        availability: Availability::Implemented,
        note: "以词定字（用标点/数字从候选里定字）",
    },
    Entry {
        name: "selector",
        slot: Slot::Processor,
        availability: Availability::Implemented,
        note: "空格/回车/数字选词",
    },
    Entry {
        name: "express_editor",
        slot: Slot::Processor,
        availability: Availability::Implemented,
        note: "退格与取消（含按音节退格）",
    },
    Entry {
        name: "editor",
        slot: Slot::Processor,
        availability: Availability::Implemented,
        note: "退格与取消",
    },
    Entry {
        name: "ascii_composer",
        slot: Slot::Processor,
        availability: Availability::Implemented,
        note: "中英切换、Shift/Caps 处理",
    },
    Entry {
        name: "navigator",
        slot: Slot::Processor,
        availability: Availability::Implemented,
        note: "翻页键",
    },
    Entry {
        name: "punctuator",
        slot: Slot::Processor,
        availability: Availability::Implemented,
        note: "标点直出与符号表",
    },
    Entry {
        name: "key_binder",
        slot: Slot::Processor,
        availability: Availability::Implemented,
        note: "按键重绑定",
    },
    Entry {
        name: "recognizer",
        slot: Slot::Processor,
        availability: Availability::Implemented,
        note: "前缀模式扫描（认出 → 打标签）",
    },
    Entry {
        name: "chord_composer",
        slot: Slot::Processor,
        availability: Availability::NotYet,
        note: "并击输入（需要时序能力，见 G13）",
    },
    Entry {
        name: "affix_segmentor",
        slot: Slot::Segmentor,
        availability: Availability::Implemented,
        note: "带前缀/后缀的切分（拆字辅码用）",
    },
    // ── 切分器 ──
    Entry {
        name: "abc_segmentor",
        slot: Slot::Segmentor,
        availability: Availability::Implemented,
        note: "标记拼音段",
    },
    Entry {
        name: "ascii_segmentor",
        slot: Slot::Segmentor,
        availability: Availability::Implemented,
        note: "标记非编码段",
    },
    Entry {
        name: "matcher",
        slot: Slot::Segmentor,
        availability: Availability::Implemented,
        note: "按 recognizer 的模式切分",
    },
    Entry {
        name: "punct_segmentor",
        slot: Slot::Segmentor,
        availability: Availability::Implemented,
        note: "标点段",
    },
    Entry {
        name: "fallback_segmentor",
        slot: Slot::Segmentor,
        availability: Availability::Implemented,
        note: "兜底，保证输入总能上屏",
    },
    // ── 内联零件（引擎直接生成候选的那些）──
    //
    // RIME 里这一族全部住在 Lua 插件里；我们按**行为**重做成原生零件。
    // 见 `crates/stele-engine/src/inline.rs` 的模块文档。
    Entry {
        name: "date_translator",
        slot: Slot::Translator,
        availability: Availability::Implemented,
        note: "日期/时间/星期/时间戳/中英日期",
    },
    Entry {
        name: "unicode_translator",
        slot: Slot::Translator,
        availability: Availability::Implemented,
        note: "U<hex> → Unicode 字符（含同区后续码位）",
    },
    Entry {
        name: "uuid_translator",
        slot: Slot::Translator,
        availability: Availability::Implemented,
        note: "触发词 → UUID",
    },
    Entry {
        name: "reduce_english_filter",
        slot: Slot::Filter,
        availability: Availability::Implemented,
        note: "降低英文候选位置（all / custom / none 三种模式）",
    },
    Entry {
        name: "pin_cand_filter",
        slot: Slot::Filter,
        availability: Availability::Implemented,
        note: "置顶候选（含最后一个音节的简码派生）",
    },
    Entry {
        name: "v_filter",
        slot: Slot::Filter,
        availability: Availability::Implemented,
        note: "v 模式单字优先（敲 v+一个字符时）",
    },
    Entry {
        name: "long_word_filter",
        slot: Slot::Filter,
        availability: Availability::Implemented,
        note: "长词优先（从第 idx 位提升 count 个更长的词）",
    },
    Entry {
        name: "autocap_filter",
        slot: Slot::Filter,
        availability: Availability::Implemented,
        note: "英文自动大写（输入码首字母/前两位大写）",
    },
    Entry {
        name: "force_gc",
        slot: Slot::Filter,
        availability: Availability::NotApplicable,
        note: "Lua 的手动 GC 在 Rust 里不存在（见 NotApplicable 的说明）",
    },
    // ── 翻译器 ──
    Entry {
        name: "script_translator",
        slot: Slot::Translator,
        availability: Availability::Implemented,
        note: "拼写图族（拼音、双拼）",
    },
    Entry {
        name: "table_translator",
        slot: Slot::Translator,
        availability: Availability::Implemented,
        note: "精确编码族（仓颉、五笔、英文、短语）",
    },
    Entry {
        name: "punct_translator",
        slot: Slot::Translator,
        availability: Availability::Implemented,
        note: "标点候选（含符号表）",
    },
    Entry {
        name: "echo_translator",
        slot: Slot::Translator,
        availability: Availability::Implemented,
        note: "原样上屏兜底",
    },
    Entry {
        name: "reverse_lookup_translator",
        slot: Slot::Translator,
        availability: Availability::NeedsData,
        note: "反查（需反查词典）",
    },
    // ── 滤镜 ──
    Entry {
        name: "uniquifier",
        slot: Slot::Filter,
        availability: Availability::Implemented,
        note: "去重",
    },
    Entry {
        name: "simplifier",
        slot: Slot::Filter,
        availability: Availability::NeedsData,
        note: "简繁/Emoji 转换（需 OpenCC 数据）",
    },
    Entry {
        name: "reverse_lookup_filter",
        slot: Slot::Filter,
        availability: Availability::Implemented,
        note: "反查提示（数据由装载器提供）",
    },
    Entry {
        name: "charset_filter",
        slot: Slot::Filter,
        availability: Availability::NotYet,
        note: "字符集过滤",
    },
    Entry {
        name: "single_char_filter",
        slot: Slot::Filter,
        availability: Availability::NotYet,
        note: "只留单字",
    },
];

/// 一个零件实例的外部数据**是否已经就位**。
///
/// # 为什么装载器必须回答这个问题，而不是注册表
///
/// 注册表能回答的只有"**我们**能不能提供这个零件"。而方案作者要问的是
/// 另一个问题："**我这份方案**缺不缺东西"。两者不是同一个问题：
///
/// - `simplifier@fanti` 机制齐全，而方案里带了一张内联转换表 → **不缺**
/// - `simplifier@emoji` 机制齐全，而 `emoji.json` 找不到 → **缺**
///
/// 把它放在 `stele-engine` 而不是 `stele-schemes` 的理由：这是
/// **判据**，而判据属于引擎；"去哪读文件、读不读得到"才是装载器的事。
/// 装载器把答案算好（它才知道文件在不在）填进这个表。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExternalData<'a> {
    /// 实例名（`table_translator@melt_eng` 里的 `melt_eng`）。
    pub alias: &'a str,
    /// 数据是否就位。
    pub present: bool,
}

/// 给出"这些实例的数据已经就位"的判据，据此检查一组零件名。
///
/// # Errors / 返回
///
/// 返回 `(Availability, 说明)` 的列表，只含**真正有问题的**那些：
/// 尚未实现的、以及**数据没到位**的。已经就位的不出现在结果里。
#[must_use]
pub fn unmet_requirements(
    names: &[String],
    external: &[ExternalData<'_>],
) -> Vec<(String, Availability, &'static str)> {
    let mut out = Vec::new();
    for n in names {
        let (availability, _slot, note) = lookup(n);
        match availability {
            // "不适用"与"已实现"在这里的处理一样（都不是缺口）：
            // 那个零件在我们这个架构里没有意义，方案声明了它也不会有效果，
            // 但这不是错误——真实方案的零件清单是从 RIME 那边抄来的。
            Availability::NotApplicable | Availability::Implemented => {}
            Availability::NotYet | Availability::Unknown => {
                out.push((n.clone(), availability, note));
            }
            Availability::NeedsData => {
                // 数据到位了就不算缺 —— 判据只看"这个实例有没有数据"。
                let (_, alias) = crate::spec::split_alias(n);
                let present = alias.is_some_and(|a| {
                    external.iter().any(|e| e.alias == a && e.present)
                });
                if !present {
                    out.push((n.clone(), availability, note));
                }
            }
        }
    }
    out
}

/// 查一个零件名（**不含 `@alias`**）。
///
/// `simplifier@emoji` 这样的引用，调用方应先剥掉 `@` 之后的部分再查。
#[must_use]
pub fn lookup(name: &str) -> (Availability, Slot, &'static str) {
    let base = name.split('@').next().unwrap_or(name);
    match ENTRIES.iter().find(|e| e.name == base) {
        Some(e) => (e.availability, e.slot, e.note),
        None => (Availability::Unknown, Slot::Filter, ""),
    }
}

/// 这个名字认不认识（不管实没实现）。
///
/// 与 `lookup(..).0 != Unknown` 是同一件事，但**意图更清楚**：
/// 调用方常常只是想问"这是不是一个认识的零件名"。
#[must_use]
pub fn is_known(name: &str) -> bool {
    let base = name.split('@').next().unwrap_or(name);
    ENTRIES.iter().any(|e| e.name == base)
}

/// 已实现的零件名（有序、去重）。
#[must_use]
pub fn implemented_names() -> Vec<&'static str> {    let mut v: Vec<&'static str> = ENTRIES
        .iter()
        .filter(|e| e.availability == Availability::Implemented)
        .map(|e| e.name)
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// 尚未实现的零件名（有序、去重）。
#[must_use]
pub fn missing_names() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = ENTRIES
        .iter()
        .filter(|e| e.availability == Availability::NotYet)
        .map(|e| e.name)
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// 在我们这个架构里不适用的零件名（有序、去重）。
///
/// 它与 `missing_names()` 分开，因为使用者该做的事完全不同：
/// 缺代码要等，而不适用**不必等**。
#[must_use]
pub fn not_applicable_names() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = ENTRIES
        .iter()
        .filter(|e| e.availability == Availability::NotApplicable)
        .map(|e| e.name)
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// 需要外部数据的零件名（有序、去重）。
#[must_use]
pub fn needs_data_names() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = ENTRIES
        .iter()
        .filter(|e| e.availability == Availability::NeedsData)
        .map(|e| e.name)
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// 一份"这个方案需要哪些零件、我们缺哪些"的报告。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoverageReport {
    /// 方案里出现的零件名（去重、有序）。
    pub required: Vec<String>,
    /// 已实现的。
    pub implemented: Vec<String>,
    /// 需要外部数据的。
    pub needs_data: Vec<String>,
    /// 尚未实现的。
    pub not_yet: Vec<String>,
    /// **在我们这个架构里不适用**的（不是缺口）。
    pub not_applicable: Vec<String>,
    /// 不认识的。
    pub unknown: Vec<String>,
}

impl CoverageReport {
    /// 分析一组零件名。
    #[must_use]
    pub fn of(names: &[String]) -> Self {
        let mut r = Self::default();
        let mut sorted: Vec<String> = names.to_vec();
        sorted.sort();
        sorted.dedup();
        for n in sorted {
            r.required.push(n.clone());
            match lookup(&n).0 {
                Availability::Implemented => r.implemented.push(n),
                Availability::NeedsData => r.needs_data.push(n),
                Availability::NotYet => r.not_yet.push(n),
                Availability::NotApplicable => r.not_applicable.push(n),
                Availability::Unknown => r.unknown.push(n),
            }
        }
        r
    }

    /// 这份方案能不能**完整**跑通。
    ///
    /// 三条都要空。注意 `not_applicable` **不算缺口**——那些零件在我们
    /// 这个架构里没有意义（`force_gc`），方案里留着它行为上与"没有它"一致。
    /// 把它算成缺口会让**每一份从 RIME 抄来的方案都报"缺零件"**，
    /// 而那正是"含糊其辞让使用者白费力气"。
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.needs_data.is_empty() && self.not_yet.is_empty() && self.unknown.is_empty()
    }

    /// 一行摘要。
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "需要 {} 个零件：已实现 {}，需外部数据 {}，尚未实现 {}，不适用 {}，不认识 {}",
            self.required.len(),
            self.implemented.len(),
            self.needs_data.len(),
            self.not_yet.len(),
            self.not_applicable.len(),
            self.unknown.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implemented_components_are_found() {
        for n in ["speller", "selector", "script_translator", "uniquifier"] {
            assert_eq!(lookup(n).0, Availability::Implemented, "{n}");
        }
    }

    #[test]
    fn alias_suffix_is_stripped() {
        // `table_translator@melt_eng` 查的是 `table_translator`。
        assert_eq!(
            lookup("table_translator@melt_eng").0,
            Availability::Implemented
        );
        assert_eq!(lookup("simplifier@emoji").0, Availability::NeedsData);
        // P3 之后反查滤镜**机制已实现**：它需要的是"反查数据"，
        // 而数据由装载器注入——所以它不再是"我们缺代码"。
        assert_eq!(
            lookup("reverse_lookup_filter@radical_reverse_lookup").0,
            Availability::Implemented
        );
    }

    #[test]
    fn needs_data_and_not_yet_are_distinguished() {
        // 这两者的区别对使用者是有意义的：一个是"你缺数据"，一个是"我们缺代码"。
        assert_eq!(lookup("simplifier").0, Availability::NeedsData);
        assert_eq!(lookup("chord_composer").0, Availability::NotYet);
        assert_ne!(Availability::NeedsData.note(), Availability::NotYet.note());
    }

    #[test]
    fn unknown_names_are_reported_as_unknown_not_as_missing() {
        assert_eq!(lookup("telepathy").0, Availability::Unknown);
    }

    #[test]
    fn coverage_report_on_no_lua_schema_names() {
        // P3 收尾时更新过：`no_lua_schema` 的 24 个名字里，剩下的缺口
        // **只有"需要外部数据"的那些**（Emoji / 简繁的 OpenCC 表、
        // 拆字词典、自定义短语表）。"尚未实现"已经清零——
        // 这条断言就是那句话的可执行版本。
        // rime-ice 的 `others/no_lua_schema` 用到的全部名字。
        let names: Vec<String> = [
            "ascii_composer",
            "recognizer",
            "key_binder",
            "speller",
            "punctuator",
            "selector",
            "navigator",
            "express_editor",
            "ascii_segmentor",
            "matcher",
            "abc_segmentor",
            "affix_segmentor@radical_lookup",
            "punct_segmentor",
            "fallback_segmentor",
            "punct_translator",
            "script_translator",
            "table_translator@custom_phrase",
            "table_translator@melt_eng",
            "table_translator@cn_en",
            "table_translator@radical_lookup",
            "reverse_lookup_filter@radical_reverse_lookup",
            "simplifier@emoji",
            "simplifier@traditionalize",
            "uniquifier",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();

        let r = CoverageReport::of(&names);
        assert_eq!(r.required.len(), 24);
        // 还不完整——但**缺的是数据，不是代码**。
        assert!(!r.is_complete());
        assert!(
            r.not_yet.is_empty(),
            "P3 收尾后不该还有「我们缺代码」的零件：{:?}",
            r.not_yet
        );
        assert!(
            r.unknown.is_empty(),
            "这 24 个名字我们都认识：{:?}",
            r.unknown
        );
        // 绝大多数零件已经能用了。
        assert!(r.implemented.contains(&"speller".to_owned()));
        assert!(r.implemented.contains(&"script_translator".to_owned()));
        assert!(r.implemented.contains(&"uniquifier".to_owned()));
        assert!(r.implemented.contains(&"ascii_composer".to_owned()));
        assert!(r.implemented.contains(&"punctuator".to_owned()));
        assert!(r.implemented.contains(&"key_binder".to_owned()));
        assert!(r.implemented.contains(&"recognizer".to_owned()));
        assert!(r.implemented.contains(&"matcher".to_owned()));
        assert!(
            r.implemented
                .contains(&"affix_segmentor@radical_lookup".to_owned())
        );
        // 缺口**逐个列出**（不是笼统一句"不支持"），且只剩"要数据"这一类。
        for n in ["simplifier@emoji", "simplifier@traditionalize"] {
            assert!(r.needs_data.contains(&n.to_owned()), "{n} 应当缺数据");
        }
        // 表驱动翻译器：**机制已实现**——它们要的"英文词库 / 自定义短语表 /
        // 拆字词典"由装载器注入，注入不了才算缺数据。
        for n in [
            "table_translator@melt_eng",
            "table_translator@cn_en",
            "table_translator@radical_lookup",
        ] {
            assert!(r.implemented.contains(&n.to_owned()), "{n} 机制已实现");
        }
        assert!(r.summary().contains("24"), "{}", r.summary());
    }

    #[test]
    fn lists_are_sorted_and_deduplicated() {
        for v in [implemented_names(), missing_names(), needs_data_names()] {
            let mut sorted = v.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(v, sorted);
        }
        assert!(implemented_names().len() >= 8);
    }
}
