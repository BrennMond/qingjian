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
        availability: Availability::NeedsData,
        note: "以词定字",
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
        availability: Availability::NotYet,
        note: "中英切换、Shift/Caps 处理",
    },
    Entry {
        name: "navigator",
        slot: Slot::Processor,
        availability: Availability::NotYet,
        note: "翻页键",
    },
    Entry {
        name: "punctuator",
        slot: Slot::Processor,
        availability: Availability::NotYet,
        note: "标点直出",
    },
    Entry {
        name: "key_binder",
        slot: Slot::Processor,
        availability: Availability::NotYet,
        note: "按键重绑定",
    },
    Entry {
        name: "recognizer",
        slot: Slot::Processor,
        availability: Availability::NotYet,
        note: "前缀模式（uU / R / N / cC / v）",
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
        availability: Availability::NotYet,
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
        availability: Availability::NotYet,
        note: "按 recognizer 的模式切分",
    },
    Entry {
        name: "punct_segmentor",
        slot: Slot::Segmentor,
        availability: Availability::NotYet,
        note: "标点段",
    },
    Entry {
        name: "fallback_segmentor",
        slot: Slot::Segmentor,
        availability: Availability::Implemented,
        note: "兜底，保证输入总能上屏",
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
        availability: Availability::NotYet,
        note: "标点候选",
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
        availability: Availability::NeedsData,
        note: "反查提示（需反查词典）",
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
    // ── 别的方案里可能出现的 ──
    Entry {
        name: "abc_segmentor",
        slot: Slot::Segmentor,
        availability: Availability::Implemented,
        note: "",
    },
    Entry {
        name: "punct_translator",
        slot: Slot::Translator,
        availability: Availability::NotYet,
        note: "",
    },
];

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

/// 已实现的零件名（有序、去重）。
#[must_use]
pub fn implemented_names() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = ENTRIES
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
                Availability::Unknown => r.unknown.push(n),
            }
        }
        r
    }

    /// 能否**完整**跑通（缺一个都不算）。
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.needs_data.is_empty() && self.not_yet.is_empty() && self.unknown.is_empty()
    }

    /// 一行摘要。
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "需要 {} 个零件：已实现 {}，需外部数据 {}，尚未实现 {}，不认识 {}",
            self.required.len(),
            self.implemented.len(),
            self.needs_data.len(),
            self.not_yet.len(),
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
        assert_eq!(
            lookup("reverse_lookup_filter@radical_reverse_lookup").0,
            Availability::NeedsData
        );
    }

    #[test]
    fn needs_data_and_not_yet_are_distinguished() {
        // 这两者的区别对使用者是有意义的：一个是"你缺数据"，一个是"我们缺代码"。
        assert_eq!(lookup("simplifier").0, Availability::NeedsData);
        assert_eq!(lookup("ascii_composer").0, Availability::NotYet);
        assert_ne!(Availability::NeedsData.note(), Availability::NotYet.note());
    }

    #[test]
    fn unknown_names_are_reported_as_unknown_not_as_missing() {
        assert_eq!(lookup("telepathy").0, Availability::Unknown);
    }

    #[test]
    fn coverage_report_on_no_lua_schema_names() {
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
        assert!(!r.is_complete());
        // 我们确实有一批现成的。
        assert!(r.implemented.contains(&"speller".to_owned()));
        assert!(r.implemented.contains(&"script_translator".to_owned()));
        assert!(r.implemented.contains(&"uniquifier".to_owned()));
        // 缺口被分类，而不是笼统一句"不支持"。
        assert!(r.not_yet.contains(&"ascii_composer".to_owned()));
        assert!(r.needs_data.contains(&"simplifier@emoji".to_owned()));
        assert!(
            r.unknown.is_empty(),
            "这 24 个名字我们都认识：{:?}",
            r.unknown
        );
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
