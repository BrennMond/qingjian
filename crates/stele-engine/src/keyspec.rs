//! # Key names
//!
//! 中文职责：解析方案里写的**按键名**（`space`、`Control+BackSpace`、`Page_Up`）。
//! English role: parse the key names that schemes write.
//! 架构位置：`stele-engine` 的装载期工具；被引擎（`key_binder`、`editor`）
//! 与方案装载器共用。
//!
//! # 为什么它住在引擎里，而不是装载器里
//!
//! 按键名的含义是**引擎的语义**：`space` 是"空格键"，而空格键在这个引擎里
//! 是"确认候选"。装载器只是把字符串搬过来；把它解释成键的是引擎。
//!
//! 两者共用**同一个**解析函数的理由很实际：`key_binder` 的 `send` 存的是
//! 键名（照抄 RIME 的数据形状），而它要把它变回按键才能重新派发。
//! 若装载器与引擎各写一份解析，就会出现"装载时说这个名字合法、
//! 派发时解析不出来"的分裂——那正是最难查的一类配置 bug。

use stele_core::{Key, KeyCode, Modifiers, NamedKey};

/// 一个按键组合：键 + 修饰键。
///
/// 与 [`stele_core::Key`] 的区别：`Key` 是**一次按键事件**（带"是否松开"），
/// 这里是**绑定表里的一个模式**（没有"松开"这回事）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct KeyChord {
    /// 键本身。
    pub code: KeyCode,
    /// 修饰键。
    pub mods: Modifiers,
}

impl KeyChord {
    /// 构造。
    #[must_use]
    pub fn new(code: KeyCode, mods: Modifiers) -> Self {
        Self { code, mods }
    }

    /// 这个按键是否命中本组合。
    ///
    /// **只比较被声明的修饰键**：方案写 `Return` 时不该要求用户
    /// 恰好没按 Shift——RIME 的行为也是"声明的修饰键都要在，
    /// 没声明的不管"。这条差异会直接体现在"回车上屏"能不能用上。
    #[must_use]
    pub fn matches(&self, key: &Key) -> bool {
        if key.release {
            return false;
        }
        self.code == key.code && key.mods.contains(self.mods)
    }

    /// 还原成 RIME 的写法（供 `--dump-config` 打印）。
    #[must_use]
    pub fn to_name(self) -> String {
        let mut out = String::new();
        if self.mods.contains(Modifiers::CTRL) {
            out.push_str("Control+");
        }
        if self.mods.contains(Modifiers::SHIFT) {
            out.push_str("Shift+");
        }
        if self.mods.contains(Modifiers::ALT) {
            out.push_str("Alt+");
        }
        if self.mods.contains(Modifiers::SUPER) {
            out.push_str("Super+");
        }
        out.push_str(&key_code_name(self.code));
        out
    }
}

/// 解析 RIME 的按键名。
///
/// # 支持的写法
///
/// | 写法 | 结果 |
/// | --- | --- |
/// | `space` / `Return` / `BackSpace` | 具名键（大小写不敏感） |
/// | `Control+BackSpace` / `Shift+Tab` | 具名键 + 修饰键（可叠加） |
/// | `minus` / `equal` / `bracketleft` | X11 名字 → 对应的**字符** |
/// | `a` / `,` | 单字符 |
///
/// **不认识的返回 `None`**，由调用方报错并列出可用的写法——
/// RIME 在这里是宽松的（认不出就当没写），而那会让"快捷键没反应"
/// 变成一个查不出来的问题。
#[must_use]
pub fn parse_key_name(name: &str) -> Option<KeyChord> {
    let mut mods = Modifiers::NONE;
    let mut rest = name;
    // 修饰键前缀，可能叠加（`Control+Shift+Return`）。
    loop {
        let lower = rest.to_ascii_lowercase();
        let stripped = ["control+", "ctrl+", "shift+", "alt+", "super+"]
            .iter()
            .find(|p| lower.starts_with(**p));
        let Some(prefix) = stripped else { break };
        mods = mods
            | match *prefix {
                "control+" | "ctrl+" => Modifiers::CTRL,
                "shift+" => Modifiers::SHIFT,
                "alt+" => Modifiers::ALT,
                _ => Modifiers::SUPER,
            };
        rest = &rest[prefix.len()..];
    }
    let key = rest.to_ascii_lowercase();
    let code = match key.as_str() {
        "space" => KeyCode::Named(NamedKey::Space),
        "return" | "enter" => KeyCode::Named(NamedKey::Enter),
        "backspace" => KeyCode::Named(NamedKey::Backspace),
        "delete" | "delete_forward" => KeyCode::Named(NamedKey::Delete),
        "escape" | "esc" => KeyCode::Named(NamedKey::Escape),
        "tab" => KeyCode::Named(NamedKey::Tab),
        "left" => KeyCode::Named(NamedKey::Left),
        "right" => KeyCode::Named(NamedKey::Right),
        "up" => KeyCode::Named(NamedKey::Up),
        "down" => KeyCode::Named(NamedKey::Down),
        "home" => KeyCode::Named(NamedKey::Home),
        "end" => KeyCode::Named(NamedKey::End),
        "prior" | "page_up" => KeyCode::Named(NamedKey::PageUp),
        "next" | "page_down" => KeyCode::Named(NamedKey::PageDown),
        "minus" => KeyCode::Char('-'),
        "equal" => KeyCode::Char('='),
        "comma" => KeyCode::Char(','),
        "period" => KeyCode::Char('.'),
        "slash" => KeyCode::Char('/'),
        "semicolon" => KeyCode::Char(';'),
        "apostrophe" => KeyCode::Char('\''),
        "grave" => KeyCode::Char('`'),
        "bracketleft" => KeyCode::Char('['),
        "bracketright" => KeyCode::Char(']'),
        "backslash" => KeyCode::Char('\\'),
        _ => {
            // 单字符（含 `,` `.` 这类直接写出来的标点）。
            let mut cs = rest.chars();
            match (cs.next(), cs.next()) {
                (Some(c), None) => KeyCode::Char(c),
                _ => return None,
            }
        }
    };
    Some(KeyChord::new(code, mods))
}

/// 一个键的显示名（[`KeyChord::to_name`] 用）。
#[must_use]
pub fn key_code_name(code: KeyCode) -> String {
    match code {
        KeyCode::Char(c) => c.to_string(),
        // `KeyCode` / `NamedKey` 都是 `#[non_exhaustive]`：新增变体会在
        // 这里被看见，而"打不出来"比"打错"好——未知键名显示成 `?`，
        // 而不是让 `--dump-config` 崩掉。
        _ => match code {
            KeyCode::Named(n) => match n {
            NamedKey::Space => "space".into(),
            NamedKey::Enter => "Return".into(),
            NamedKey::Backspace => "BackSpace".into(),
            NamedKey::Delete => "Delete".into(),
            NamedKey::Escape => "Escape".into(),
            NamedKey::Tab => "Tab".into(),
            NamedKey::Left => "Left".into(),
            NamedKey::Right => "Right".into(),
            NamedKey::Up => "Up".into(),
            NamedKey::Down => "Down".into(),
            NamedKey::Home => "Home".into(),
            NamedKey::End => "End".into(),
            NamedKey::PageUp => "Page_Up".into(),
            NamedKey::PageDown => "Page_Down".into(),
                NamedKey::Digit(d) => d.to_string(),
                NamedKey::Shift => "Shift".into(),
                NamedKey::CapsLock => "Caps_Lock".into(),
                _ => "?".into(),
            },
            _ => "?".into(),
        },
    }
}

/// 把一个字符还原成**它最可能是的那个按键**。
///
/// 用于 `key_binder` 的 `send` 里那种"直接写文本"的写法
/// （`send: space` 是键名，而 `send: "，"` 是文本）。要重新派发就必须
/// 换回按键，而"这个字符是哪一种键"只能靠约定：
///
/// | 字符 | 按键 |
/// | --- | --- |
/// | 空格 / 制表 / 换行 | 对应的具名键 |
/// | 其它单字符 | 字符键 |
///
/// **为什么不能统一按字符键处理**：空格是输入法里最特殊的一个键
/// （它是"确认候选"），而"上屏一个空格字符"是另一件事。
/// 这条区别在 RIME 的方案里到处都是（`send: space`）。
#[must_use]
pub fn key_for_char(c: char) -> Key {
    match c {
        ' ' => Key::press(KeyCode::Named(NamedKey::Space), Modifiers::NONE),
        '\t' => Key::press(KeyCode::Named(NamedKey::Tab), Modifiers::NONE),
        '\n' | '\r' => Key::press(KeyCode::Named(NamedKey::Enter), Modifiers::NONE),
        other => Key::ch(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names_round_trip_through_the_parser() {
        assert_eq!(
            parse_key_name("Control+BackSpace"),
            Some(KeyChord::new(
                KeyCode::Named(NamedKey::Backspace),
                Modifiers::CTRL
            ))
        );
        assert_eq!(
            parse_key_name("shift+tab"),
            Some(KeyChord::new(KeyCode::Named(NamedKey::Tab), Modifiers::SHIFT))
        );
        assert_eq!(
            parse_key_name(","),
            Some(KeyChord::new(KeyCode::Char(','), Modifiers::NONE))
        );
        assert_eq!(parse_key_name("nonsense_key"), None);
    }

    #[test]
    fn names_render_back_the_way_rime_writes_them() {
        let chord = parse_key_name("Control+Shift+Return").unwrap();
        assert_eq!(chord.to_name(), "Control+Shift+Return");
        assert_eq!(parse_key_name("Page_Up").unwrap().to_name(), "Page_Up");
    }

    #[test]
    fn only_declared_modifiers_are_compared() {
        let chord = KeyChord::new(KeyCode::Named(NamedKey::Enter), Modifiers::NONE);
        let shift_enter = Key::press(KeyCode::Named(NamedKey::Enter), Modifiers::SHIFT);
        assert!(chord.matches(&shift_enter), "声明里没写 Shift 就不该要求它");
        assert!(!chord.matches(&Key::ch('a')));
    }
}
