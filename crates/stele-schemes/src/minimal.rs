//! # Default schemes (embedded)
//!
//! 中文职责：随项目提供的默认方案——**从 YAML 文件装载**，文件在编译期内嵌。
//! English role: the default schemes shipped with the project, loaded from YAML
//! files embedded at compile time.
//! 架构位置：**方案资产**，与内核分属不同 crate（PLAN D24）。
//!
//! # 为什么"内嵌 YAML"而不是"Rust 里写死方案"
//!
//! 早先这里是用 Rust 数据结构硬写的两套方案。改成内嵌 YAML 之后：
//!
//! 1. **单一数据来源**：方案就在 `schemes/stele-default/*.yaml`，
//!    不再有"Rust 里一份、文件里一份"的漂移。
//! 2. **真实路径被测试**：`stele` 一启动走的就是**真正的解析器**，
//!    而不是一条"只有测试才会走"的旁路。**旁路从来不坏，也从来不证明什么。**
//! 3. **换方案不用重编译**：用户可以把自己的方案放进目录，用 `--scheme-dir` 指过来。
//!
//! # 为什么还要内嵌
//!
//! 因为 `stele` 是个**命令行工具**，可能从任何目录被调用。
//! 内嵌一份保证"没有配置文件也能用"——这与 RIME 内置 `data/minimal/` 是同一个考虑。
//!
//! # 两个方案为什么必须都在（D33）
//!
//! | 方案 | 编码单元 | 需要拼写规则吗 | 需要切分图吗 | 翻译器族 |
//! | --- | --- | --- | --- | --- |
//! | `pinyin` | 音节（`ni`、`hao`…） | 需要（缩写） | 需要（`nihao` 有歧义） | 拼写图 |
//! | `shape` | **单个字母** | **不需要** | **不需要** | 精确编码 |
//!
//! 如果引擎被写成"拼音专用"，`shape` 会先跑不起来——
//! **通用性因此被测试保护，而不是靠文档声明。**

use stele_core::SchemaError;
use stele_dict as dict;
use stele_engine::scheme::{SchemeDef, TranslatorKind};

/// 随项目提供的数据文件（路径相对于本 crate 的 `src/`）。
///
/// `include_str!` 让它们在编译期被读进来——**单一数据来源**：
/// 文件是真身，这里只是把它搬进二进制。
pub const EMBEDDED: &[(&str, &str)] = &[
    (
        "pinyin.schema.yaml",
        include_str!("../../../schemes/stele-default/pinyin.schema.yaml"),
    ),
    (
        "pinyin.dict.yaml",
        include_str!("../../../schemes/stele-default/pinyin.dict.yaml"),
    ),
    (
        "cn_dicts/base.dict.yaml",
        include_str!("../../../schemes/stele-default/cn_dicts/base.dict.yaml"),
    ),
    (
        "shape.schema.yaml",
        include_str!("../../../schemes/stele-default/shape.schema.yaml"),
    ),
    (
        "shape.dict.yaml",
        include_str!("../../../schemes/stele-default/shape.dict.yaml"),
    ),
];

/// 要装载哪几份方案（以及装载顺序）。
const EMBEDDED_SCHEMAS: &[&str] = &["pinyin.schema.yaml", "shape.schema.yaml"];

/// 从内嵌数据里读词典。
pub struct EmbeddedSource;

impl dict::Source for EmbeddedSource {
    fn read(&self, rel_path: &str) -> Option<String> {
        // 词典名可以写成 `cn_dicts/base`，也可以写成 `cn_dicts/base.dict.yaml`。
        let with_ext = format!("{rel_path}.dict.yaml");
        EMBEDDED
            .iter()
            .find(|(n, _)| *n == rel_path || *n == with_ext)
            .map(|(_, c)| (*c).to_owned())
    }
}

/// 装载内嵌的默认方案。
///
/// # Errors
///
/// 内嵌数据本身解析失败时返回 [`SchemaError`]。
/// **这在实践中意味着"打包坏了"**——所以它必须在 CI 里被测试抓住
/// （见本模块的测试），而不是留到运行时当可恢复错误处理。
pub fn all() -> Result<Vec<SchemeDef>, SchemaError> {
    let src = EmbeddedSource;
    EMBEDDED_SCHEMAS
        .iter()
        .map(|name| crate::file::load_scheme(embedded(name), name, &src))
        .collect()
}

/// 取一份内嵌文件的原文。
///
/// # Panics
///
/// 名字不在 [`EMBEDDED`] 里时 panic——那是一张编译期常量表，
/// 写错名字属编程错误，不是可恢复的运行时状况。
#[must_use]
pub fn embedded(name: &str) -> &'static str {
    EMBEDDED
        .iter()
        .find(|(n, _)| *n == name)
        .map_or_else(|| panic!("内嵌数据里没有 {name}"), |(_, c)| *c)
}

/// 两族翻译器是否都被覆盖（D33 的通用性保证）。
///
/// **这不是装饰**：如果哪天有人把两族合并成一族，这个判断会失去意义，
/// 而"引擎通用性"也就从"被测试保护"退回成"只是一句声明"。
#[must_use]
pub fn uses_both_translator_families(schemes: &[SchemeDef]) -> bool {
    schemes
        .iter()
        .any(|d| d.translator == TranslatorKind::ExactCode)
        && schemes
            .iter()
            .any(|d| d.translator == TranslatorKind::SpellingGraph)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_schemes_load_and_compile() {
        let defs = all().expect("内嵌方案必须能装载 —— 失败说明打包坏了");
        assert_eq!(defs.len(), 2);
        for d in &defs {
            let s = d
                .compile()
                .unwrap_or_else(|e| panic!("方案 {} 编译失败：{e}", d.info.schema_id));
            assert!(!s.alphabet().is_empty());
            // 词库现在是 `Arc<dyn Lexicon>`（引擎只以 trait 的身份使用它），
            // 所以这里不再断言"条数" —— 那是实现细节，不是契约。
            // 真正证明词库可用的是下面的端到端集成测试。
            let _ = s.lexicon();
        }
    }

    #[test]
    fn both_translator_families_are_covered() {
        let defs = all().unwrap();
        assert!(
            uses_both_translator_families(&defs),
            "默认方案必须同时覆盖两族翻译器（D33）"
        );
        assert!(defs.iter().any(|d| d.info.schema_id == "pinyin"));
        assert!(defs.iter().any(|d| d.info.schema_id == "shape"));
    }

    #[test]
    fn pinyin_has_rules_and_shape_has_none() {
        let defs = all().unwrap();
        let p = defs.iter().find(|d| d.info.schema_id == "pinyin").unwrap();
        assert_eq!(p.translator, TranslatorKind::SpellingGraph);
        assert!(!p.rules.is_empty(), "拼音方案需要缩写规则");

        let s = defs.iter().find(|d| d.info.schema_id == "shape").unwrap();
        assert_eq!(s.translator, TranslatorKind::ExactCode);
        assert!(s.rules.is_empty(), "精确编码方案不需要任何拼写规则");
        assert!(
            s.alphabet.iter().all(|u| u.chars().count() == 1),
            "字形码的编码单元是单字母 —— 这是「编码集合不可枚举」那类输入法的形式特征"
        );
    }

    #[test]
    fn dictionary_imports_are_expanded() {
        let defs = all().unwrap();
        let p = defs.iter().find(|d| d.info.schema_id == "pinyin").unwrap();
        let stele_engine::scheme::DictSource::Inline(entries) = &p.dictionary else {
            panic!("内嵌方案应当用内联词条");
        };
        // 主词典自己 2 条 + 导入的 base 若干条。
        assert!(
            entries.len() > 10,
            "导入应当被展开，实得 {} 条",
            entries.len()
        );
        // 根词典的词条在**前** —— 于是"先出现者优先"这条规则可以直接照做。
        assert_eq!(entries[0].1, "你");
    }

    #[test]
    fn family_is_declared_for_shared_user_dictionary() {
        let defs = all().unwrap();
        let p = defs.iter().find(|d| d.info.schema_id == "pinyin").unwrap();
        assert_eq!(p.info.family.as_deref(), Some("stele-pinyin"));
    }

    #[test]
    fn scheme_data_is_simplified_only() {
        // PLAN D32：随项目提供的方案数据只做简体。
        let traditional = ['這', '國', '學', '體', '經', '門', '個', '們', '繁'];
        for d in all().unwrap() {
            let stele_engine::scheme::DictSource::Inline(entries) = &d.dictionary else {
                continue;
            };
            for (_, word, _) in entries {
                for c in word.chars() {
                    assert!(
                        !traditional.contains(&c),
                        "方案数据里出现了繁体字「{c}」—— 本项目只维护简体形态（D32）"
                    );
                }
            }
        }
    }

    #[test]
    fn traditionalization_switch_exists_but_defaults_off() {
        // D32：能力保留、数据自备。
        let defs = all().unwrap();
        let p = defs.iter().find(|d| d.info.schema_id == "pinyin").unwrap();
        let sw = p
            .switches
            .iter()
            .find(|s| s.name == "traditionalization")
            .expect("繁体转换的开关必须保留（接口不封死）");
        assert!(!sw.on, "但我们不维护繁体数据，所以默认关闭");
    }
}
