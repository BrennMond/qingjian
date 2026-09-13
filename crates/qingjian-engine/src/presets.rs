//! # Presets (built-in component sets)
//!
//! 中文职责：随引擎提供的**零件预设**——方案写 `import_preset: <名字>` 就能拿到一整套。
//! English role: built-in component presets; a schema grabs a whole set with
//! `import_preset: <name>`.
//! 架构位置：`qingjian-engine` 的数据表；由方案装载器取用。
//!
//! # 为什么需要它（RIME 有这个，而且方案作者依赖它）
//!
//! RIME 的方案里到处写着：
//!
//! ```yaml
//! recognizer:
//!   import_preset: default     # 从 default.yaml 继承通用的
//! key_binder:
//!   import_preset: default
//! ```
//!
//! `import_preset` 的含义是「**再把那一份配置叠在我写的东西底下**」——
//! 于是方案只需要写自己**特有**的那几条，通用的（翻页键、常用标点）
//! 不必抄一遍。rime-ice 的 `no_lua_schema` 正是这么写的。
//!
//! # 我们为什么不用 RIME 的 `default`
//!
//! RIME 的 `default.yaml` 是**它的资产**（在 `librime` / `rime-prelude` 里），
//! 它的标点表、按键绑定都是 RIME 的选择。照抄它等于把别人的方案数据
//! 搬进我们的引擎——违反 D24（内核与方案分离），也违反"默认方案是自有资产"。
//!
//! 所以：
//!
//! - 我们提供**自己的**预设（名字是 [`PRESET_QINGJIAN`]），内容是"一个中文
//!   输入法本来就该有的东西"：常用标点、翻页键、中英切换的入口键。
//! - 方案若要引用 RIME 的 `default`，装载器会**如实报出"这个名字的预设
//!   我们没有"**，并给出可用的名字。它不会静默忽略——忽略的症状是
//!   "标点打不出来"，而那是很难查的一类问题。
//!
//! # 数据从哪来
//!
//! 这里的表是**引擎自带的默认值**，而方案可以整体覆盖它们
//! （`punctuator.half_shape` 是映射：叠加以方案为准）。
//! 它是"没有方案时也能打字"的保证，不是"引擎内置了某个输入法"——
//! 表里没有任何拼音/仓颉专属的东西，只有"中文标点长什么样"。

use std::collections::BTreeMap;

/// 我们提供的预设名。
pub const PRESET_QINGJIAN: &str = "qingjian";

/// 一份预设。
#[derive(Clone, Debug, Default)]
pub struct Preset {
    /// 半角标点映射（原样 → 上屏）。
    pub half_shape: BTreeMap<String, String>,
    /// 全角标点映射。
    pub full_shape: BTreeMap<String, String>,
    /// 上一页 / 下一页的按键名。
    pub page_up: Vec<String>,
    /// 下一页。
    pub page_down: Vec<String>,
    /// 中英切换的按键名。
    pub toggle_ascii: Vec<String>,
}

/// 取一份预设。
///
/// 返回 `None` 表示这个名字的预设我们没有——装载器据此**报错**，
/// 而不是静默当作"没有预设"。
#[must_use]
pub fn get(name: &str) -> Option<Preset> {
    match name {
        PRESET_QINGJIAN => Some(qingjian()),
        _ => None,
    }
}

/// 我们知道的预设名（供诊断信息列出）。
#[must_use]
pub fn names() -> &'static [&'static str] {
    &[PRESET_QINGJIAN]
}

/// 随引擎提供的默认预设。
///
/// # 它里面为什么是这些、不是别的
///
/// 只放**中文输入法本来就该有的东西**，而且每一条都能说清理由：
///
/// | 项 | 理由 |
/// | --- | --- |
/// | 半角 `,` `.` `!` `?` `:` `;` → 全角 | 中文写作最常用的六个 |
/// | 全角表同形映射 | 开了全角开关时行为不变（全角表本身是方案的事） |
/// | `Page_Up` / `Page_Down` 翻页 | RIME 的默认，用户肌肉记忆 |
/// | `Shift` 切中英 | RIME 的默认 |
///
/// **没有放**：`/` 开头的符号表（那是 RIME 的 `symbols.yaml` 资产）、
/// emoji、简繁表——那些都需要数据文件，属于方案。
#[must_use]
pub fn qingjian() -> Preset {
    let pairs: &[(&str, &str)] = &[
        (",", "，"),
        (".", "。"),
        ("!", "！"),
        ("?", "？"),
        (":", "："),
        (";", "；"),
        ("(", "（"),
        (")", "）"),
        ("[", "【"),
        ("]", "】"),
        ("<", "《"),
        (">", "》"),
    ];
    Preset {
        half_shape: pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        full_shape: pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        page_up: vec!["Page_Up".to_owned()],
        page_down: vec!["Page_Down".to_owned()],
        toggle_ascii: vec!["Shift".to_owned()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_named_preset_exists_and_is_not_empty() {
        let p = get(PRESET_QINGJIAN).expect("默认预设必须在");
        assert!(p.half_shape.contains_key(","));
        assert_eq!(p.half_shape.get(",").map(String::as_str), Some("，"));
        assert!(!p.page_down.is_empty());
    }

    #[test]
    fn an_unknown_preset_is_absent_not_empty() {
        // 这一条是**诊断的依据**：`None` 让装载器能报"这个名字的预设我们没有"，
        // 而"返回空预设"会让它静默失效。
        assert!(get("default").is_none(), "RIME 的 default 不是我们的资产");
        assert_eq!(names(), &[PRESET_QINGJIAN]);
    }
}
