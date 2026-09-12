//! # Built-in schemes
//!
//! 中文职责：随引擎提供的最小方案，用于验证管线并让程序立刻可用。
//! English role: minimal schemes shipped with the engine, so the pipeline can be
//! exercised and the program is usable immediately.
//! 架构位置：**方案资产**，与内核**分属不同的 crate**（PLAN D24）。
//!
//! 这一个 crate 的存在本身就是一条架构约束的可执行表达：
//! 内核（`stele-core` / `stele-engine`）里不允许出现"拼音 / 音节"这类词汇，
//! 而本 crate 里**必然**出现——于是它们在物理上必须分开。
//! CI 门禁 `scripts/verify-no-ime-vocab.sh` 会持续检查这条边界。
//!
//! # ⚠️ 这些不是最终方案
//!
//! 它们只有几百条词，**唯一目的是让 P1 的管线可被端到端验证**。
//! 真正的默认方案（行为对标雾凇、数据自建）是 **P3.5** 的交付物，
//! 届时会从 `.schema.yaml` / `.dict.yaml` 装载（P2 的解析器）。
//!
//! **两个方案共用同一个引擎**，这不是巧合而是 D33 的要求：
//!
//! | 方案 | 编码单元 | 需要拼写规则吗 | 需要切分图吗 | 翻译器族 |
//! | --- | --- | --- | --- | --- |
//! | [`pinyin_scheme`] | 音节（`ni`、`hao`…） | 需要（缩写） | 需要（`nihao` 有歧义） | 拼写图 |
//! | [`shape_scheme`] | **单个字母** | **不需要** | **不需要** | 精确编码 |
//!
//! 如果引擎被写成了"拼音专用"，`shape_scheme` 就跑不起来——
//! 于是通用性从 P1 起就被测试保护，而不是靠文档声明。

use stele_core::{SchemaInfo, Score, SpellingAttr, Switch};

use stele_engine::scheme::{SchemeDef, TranslatorKind, SCHEME_FORMAT_VERSION};

/// 拼音演示方案的 id。
pub const PINYIN_SCHEMA_ID: &str = "pinyin-demo";

/// 精确编码演示方案的 id。
pub const SHAPE_SCHEMA_ID: &str = "shape-demo";

/// 缩写边的代价。
///
/// **对数域里的一个扣分**。它的存在就是"简拼天然排在精确匹配之后"的
/// 全部机制——不需要任何额外规则（`docs/engine-design.md` §5.2）。
fn abbrev_cost() -> Score {
    // ln(0.5) ≈ -0.693 —— 在日志域里就是"打五折"。
    Score::from_weight(0.5)
}

/// 拼音演示方案。
///
/// 覆盖两种真实行为：
/// - **规范拼写**：`nihao` → 「你好」
/// - **缩写（简拼）**：`nh` → 「你好」，带 `ABBREV` 属性且分数更低
///
/// 数据是简体的（PLAN D32）：我们只维护简体形态。
/// `traditionalization` 开关**保留但默认关闭**——接口留着，
/// 需要繁体的人自行配置数据。
#[must_use]
pub fn pinyin_scheme() -> SchemeDef {
    // 一个很小的音节表。真实方案的音节表约 400 项，来自方案数据。
    let alphabet: Vec<String> = [
        "ni", "hao", "wo", "yao", "da", "zi", "shi", "jie", "zhong", "guo", "ren", "min", "de",
        "le", "bu", "ma", "he", "zhang", "san", "li",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();

    // 词条：`(编码单元序列, 词, 权重)`。
    // 权重是**相对词频**，装载期换算成对数域定点分数。
    let entries: Vec<(Vec<&'static str>, &'static str, f64)> = vec![
        (vec!["ni", "hao"], "你好", 10_000.0),
        (vec!["zhong", "guo"], "中国", 9_000.0),
        (vec!["wo", "yao"], "我要", 8_000.0),
        (vec!["da", "zi"], "打字", 6_000.0),
        (vec!["shi", "jie"], "世界", 5_000.0),
        (vec!["ren", "min"], "人民", 4_000.0),
        (vec!["de"], "的", 30_000.0),
        (vec!["shi"], "是", 25_000.0),
        (vec!["bu"], "不", 21_000.0),
        (vec!["le"], "了", 22_000.0),
        (vec!["ni"], "你", 20_000.0),
        (vec!["wo"], "我", 18_000.0),
        (vec!["ren"], "人", 16_000.0),
        (vec!["hao"], "好", 15_000.0),
        (vec!["yao"], "要", 12_000.0),
        (vec!["da"], "大", 11_000.0),
        (vec!["he"], "和", 10_000.0),
        (vec!["ma"], "吗", 9_000.0),
        (vec!["zhong"], "中", 8_000.0),
        (vec!["guo"], "国", 8_000.0),
        (vec!["zi"], "字", 7_000.0),
        (vec!["min"], "民", 5_000.0),
        (vec!["jie"], "界", 3_000.0),
        (vec!["zhang"], "张", 7_000.0),
        (vec!["san"], "三", 9_000.0),
        (vec!["li"], "里", 8_000.0),
    ];

    SchemeDef {
        info: SchemaInfo {
            schema_id: PINYIN_SCHEMA_ID.to_owned(),
            name: "石经・拼音（演示）".to_owned(),
            version: "0.1.0".to_owned(),
            format_version: SCHEME_FORMAT_VERSION,
            // 全拼与双拼将来应共享用户词库，故有"方案族"的概念（D29）。
            family: Some("stele-pinyin".to_owned()),
        },
        switches: vec![
            // 引擎不认识这些名字，它们纯粹是方案数据（PLAN D20）。
            Switch::new("ascii_mode", false),
            Switch::new("ascii_punct", false),
            // 转换能力保留，但项目不维护繁体数据（D32）。
            Switch::new("traditionalization", false),
        ],
        tag: "abc",
        alphabet,
        rules: vec![
            // **规范拼写永远是基线**，这里只额外加一条缩写规则。
            // **引擎不知道它叫"简拼"**，只知道这些边带 ABBREV 属性、有代价。
            stele_engine::spelling::Rule::Abbrev {
                take: 1,
                cost: abbrev_cost(),
            },
        ],
        entries,
        translator: TranslatorKind::SpellingGraph,
        candidate_cap: stele_engine::pipeline::CANDIDATE_CAP,
    }
}

/// 精确编码演示方案（一个极小的"字形码"方案）。
///
/// **它存在的唯一理由就是当反例**：一个没有拼写规则、没有歧义切分、
/// 只需要"编码 → 字"查表的输入法。如果引擎强迫所有方案都走拼写图，
/// 这个方案就跑不起来——那样 D20 的通用性就是假的。
///
/// 编码是编造的（`a`=一、`b`=丨…），**故意不像任何真实输入法**，
/// 以免给人"引擎里内置了某种字形方案"的错觉。
#[must_use]
pub fn shape_scheme() -> SchemeDef {
    // 编码单元就是单个字母——编码集合不可枚举的那类输入法的特征。
    let alphabet: Vec<String> = ["a", "b", "c", "d", "e", "f", "g", "h"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();

    let entries: Vec<(Vec<&'static str>, &'static str, f64)> = vec![
        (vec!["a"], "一", 1_000.0),
        (vec!["b"], "丨", 1_000.0),
        (vec!["c"], "丿", 1_000.0),
        (vec!["d"], "丶", 1_000.0),
        (vec!["e"], "乙", 1_000.0),
        (vec!["a", "b"], "十", 2_000.0),
        (vec!["a", "c"], "厂", 1_500.0),
        (vec!["b", "a"], "上", 2_500.0),
        (vec!["a", "b", "c"], "木", 3_000.0),
        (vec!["a", "b", "d"], "才", 1_800.0),
    ];

    SchemeDef {
        info: SchemaInfo {
            schema_id: SHAPE_SCHEMA_ID.to_owned(),
            name: "石经・字形码（演示）".to_owned(),
            version: "0.1.0".to_owned(),
            format_version: SCHEME_FORMAT_VERSION,
            family: None,
        },
        switches: vec![Switch::new("ascii_mode", false)],
        tag: "code",
        alphabet,
        // 精确编码方案**不需要任何拼写规则**（空列表 = 只有规范拼写）。
        rules: vec![],
        entries,
        translator: TranslatorKind::ExactCode,
        candidate_cap: stele_engine::pipeline::CANDIDATE_CAP,
    }
}

/// 全部内置方案。
#[must_use]
pub fn all() -> Vec<SchemeDef> {
    vec![pinyin_scheme(), shape_scheme()]
}

/// 供测试与调试：确认内置方案用到了两族翻译器。
///
/// 这不是装饰——**它是一个可执行的断言**：如果哪天有人把两族合并成一族，
/// 这个函数会失去意义，而 D33 的通用性保证也就随之消失。
#[must_use]
pub fn uses_both_translator_families() -> bool {
    all()
        .iter()
        .any(|d| d.translator == TranslatorKind::ExactCode)
        && all()
            .iter()
            .any(|d| d.translator == TranslatorKind::SpellingGraph)
}

/// 供测试与调试：缩写规则的属性确实是 `ABBREV`。
#[must_use]
pub fn abbrev_attr() -> SpellingAttr {
    SpellingAttr::ABBREV
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_families_are_present() {
        assert!(uses_both_translator_families());
    }

    #[test]
    fn every_builtin_scheme_compiles() {
        for d in all() {
            let s = d
                .compile()
                .unwrap_or_else(|e| panic!("方案 {} 编译失败：{e}", d.info.schema_id));
            assert!(!s.alphabet().is_empty());
            assert!(!s.lexicon().is_empty());
        }
    }

    #[test]
    fn pinyin_has_rules_and_shape_has_none() {
        let p = pinyin_scheme().compile().unwrap();
        assert_eq!(p.kind(), TranslatorKind::SpellingGraph);
        assert!(p.has_spelling_table());

        let s = shape_scheme().compile().unwrap();
        assert_eq!(s.kind(), TranslatorKind::ExactCode);
        assert!(!s.has_spelling_table(), "精确编码方案不该有拼写表");
    }

    #[test]
    fn shape_alphabet_units_are_single_letters() {
        // 这是"编码集合不可枚举"那类输入法的形式特征。
        let d = shape_scheme();
        assert!(d.alphabet.iter().all(|u| u.chars().count() == 1));
    }

    #[test]
    fn pinyin_units_are_multi_letter_syllables() {
        let d = pinyin_scheme();
        assert!(d.alphabet.iter().any(|u| u.chars().count() > 1));
    }

    #[test]
    fn scheme_data_is_simplified_only() {
        // PLAN D32：随项目提供的方案数据只做简体。
        // 这里做一个粗糙但有效的守门：方案数据里不应出现常见繁体字。
        let traditional = ['這', '國', '學', '體', '經', '門', '個', '們'];
        for d in all() {
            for (_, word, _) in &d.entries {
                for c in word.chars() {
                    assert!(
                        !traditional.contains(&c),
                        "方案数据里出现了繁体字「{c}」—— 本项目只维护简体形态（D32）"
                    );
                }
            }
        }
    }
}
