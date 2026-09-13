//! # Options
//!
//! 中文职责：会话开关的**按名字**读写机制。
//! English role: named on/off switches, accessed by name.
//! 架构位置：qingjian-core 的会话状态，方案通过它声明自己的开关。
//!
//! # 引擎不预设任何开关名（PLAN D20）
//!
//! 早期设计把 `ascii_mode` / `emoji` / `traditionalization` / `full_shape` /
//! `ascii_punct` 写成了 `qingjian-core` 里的**结构体字段**。其中 `emoji` 和
//! `traditionalization` 是**某个方案专有的**（换个方案就没有这两个概念），
//! 把它们写进内核等于在引擎里内置了输入法专属知识——**直接违反 D20**。
//!
//! 现在：开关**全部**是方案声明的数据；引擎的组件**按名字**申请它需要的开关，
//! 加载期校验存在性。于是"全角/半角""简繁""emoji"在引擎眼里**都是同一种东西**：
//! 一个有名字的布尔量。
//!
//! **对比 RIME**：它的 `RimeStatus` 把 `is_ascii_mode` / `is_full_shape` /
//! `is_simplified` / `is_traditional` / `is_ascii_punct` 做成了 C 结构体固定字段，
//! 所以它的 API 里**焊死了这五个概念**。

use std::collections::BTreeMap;

/// 一个开关的声明。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Switch {
    /// 开关名（引擎按它访问）。
    pub name: String,
    /// 两个状态在界面上的显示名，例如 `["中", "Ａ"]`。
    pub states: Option<[String; 2]>,
    /// 当前状态。
    pub on: bool,
    /// 显示用的缩写字符。
    pub abbrev: Option<char>,
}

impl Switch {
    /// 构造一个开关。
    #[must_use]
    pub fn new(name: impl Into<String>, on: bool) -> Self {
        Self {
            name: name.into(),
            states: None,
            on,
            abbrev: None,
        }
    }

    /// 当前状态在界面上的显示名。
    #[must_use]
    pub fn state_label(&self) -> Option<&str> {
        self.states.as_ref().map(|s| {
            if self.on {
                s[1].as_str()
            } else {
                s[0].as_str()
            }
        })
    }
}

/// 会话的开关集合。
///
/// 内部用 `BTreeMap` 而非 `HashMap`：**凡是顺序可能影响输出的集合一律用有序容器**
/// （PLAN §5.2 可复现铁律）。遍历开关的顺序若不确定，任何依赖它的行为都会漂移。
#[derive(Clone, Debug, Default)]
pub struct Options {
    switches: BTreeMap<String, Switch>,
}

impl Options {
    /// 空集合。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 声明一个开关（加载期由方案调用）。已存在则覆盖。
    pub fn declare(&mut self, switch: Switch) {
        self.switches.insert(switch.name.clone(), switch);
    }

    /// 按名字读取。**未声明的开关一律返回 `false`**，不 panic——
    /// 因为运行期不可失败（`docs/engine-design.md` §5.6）。
    ///
    /// 但方案**要求的**开关若不存在，必须在**加载期**报错；
    /// 那由 [`Options::missing`] 在装载方案时检查。
    #[must_use]
    pub fn get(&self, name: &str) -> bool {
        self.switches.get(name).is_some_and(|s| s.on)
    }

    /// 按名字设置。未声明的开关**不会被创建**（返回 `false`）——
    /// 这防止拼错名字时静默产生一个永远不生效的开关。
    pub fn set(&mut self, name: &str, on: bool) -> bool {
        match self.switches.get_mut(name) {
            Some(s) => {
                s.on = on;
                true
            }
            None => false,
        }
    }

    /// 切换。
    pub fn toggle(&mut self, name: &str) -> bool {
        match self.switches.get_mut(name) {
            Some(s) => {
                s.on = !s.on;
                true
            }
            None => false,
        }
    }

    /// 返回 `required` 里那些**尚未声明**的开关名。
    ///
    /// 供装载方案时做"缺零件/缺开关"的显式失败检查（PLAN D17）。
    #[must_use]
    pub fn missing(&self, required: &[&str]) -> Vec<String> {
        required
            .iter()
            .filter(|n| !self.switches.contains_key(**n))
            .map(|n| (*n).to_owned())
            .collect()
    }

    /// 按名字（有序）遍历。
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Switch)> {
        self.switches.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// 已声明的开关数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.switches.len()
    }

    /// 是否没有任何开关。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.switches.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_knows_no_switch_names() {
        // 引擎只按名字读写；`emoji` 与 `ascii_mode` 对它是同一种东西。
        let mut o = Options::new();
        o.declare(Switch::new("emoji", true));
        o.declare(Switch::new("ascii_mode", false));

        assert!(o.get("emoji"));
        assert!(!o.get("ascii_mode"));
        assert!(!o.get("nonexistent"));
    }

    #[test]
    fn setting_an_undeclared_switch_fails_loudly() {
        let mut o = Options::new();
        // 拼错名字不会被静默创建 —— 返回 false 让调用方能发现。
        assert!(!o.set("emojii", true));
        assert!(o.is_empty());
    }

    #[test]
    fn missing_reports_undeclared_requirements() {
        let mut o = Options::new();
        o.declare(Switch::new("ascii_mode", false));
        assert_eq!(
            o.missing(&["ascii_mode", "full_shape"]),
            vec!["full_shape".to_owned()]
        );
    }

    #[test]
    fn toggle_flips() {
        let mut o = Options::new();
        o.declare(Switch::new("x", false));
        assert!(o.toggle("x"));
        assert!(o.get("x"));
    }
}
