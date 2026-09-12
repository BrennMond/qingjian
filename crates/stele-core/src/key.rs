//! # Key
//!
//! 中文职责：平台无关的按键表示。前端负责把平台按键翻译成它。
//! English role: platform-independent key representation; frontends translate into it.
//! 架构位置：stele-core 的输入类型，由 Processor 消费。
//!
//! # 为什么不直接用 RIME 的 X11 keysym
//!
//! keysym 是从 Unix 借来的历史产物，Windows 与 Android 前端都要为此写一大坨映射表。
//! 我们内部用自己的类型，只在 FFI 边界提供兼容层。

/// 修饰键的位集合。
///
/// 手写而非用 `bitflags` crate——本 crate 零依赖（PLAN D9）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct Modifiers(u8);

impl Modifiers {
    /// 无修饰键。
    pub const NONE: Self = Self(0);
    /// Shift。
    pub const SHIFT: Self = Self(1 << 0);
    /// Control。
    pub const CTRL: Self = Self(1 << 1);
    /// Alt。
    pub const ALT: Self = Self(1 << 2);
    /// Windows 键 / Command 键。
    pub const SUPER: Self = Self(1 << 3);
    /// `CapsLock` 处于开启状态。
    pub const CAPS: Self = Self(1 << 4);

    /// 由原始位构造。
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// 取原始位。
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// 是否包含某一位。
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// 是否没有任何修饰键。
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl core::ops::BitOr for Modifiers {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

/// 具名功能键。
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum NamedKey {
    /// 退格。
    Backspace,
    /// 删除。
    Delete,
    /// 回车。
    Enter,
    /// 退出。
    Escape,
    /// 空格。
    Space,
    /// 制表。
    Tab,
    /// 左方向键。
    Left,
    /// 右方向键。
    Right,
    /// 上方向键。
    Up,
    /// 下方向键。
    Down,
    /// Home。
    Home,
    /// End。
    End,
    /// 上翻页。
    PageUp,
    /// 下翻页。
    PageDown,
    /// 顶部数字键 0–9，用于选词。
    Digit(u8),
}

/// 按键的"是什么"。
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum KeyCode {
    /// 可打印字符。**已归一化**：`Shift` + `a` 在这里就是 `'A'`。
    Char(char),
    /// 具名功能键。
    Named(NamedKey),
}

/// 一个按键事件。
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Key {
    /// 哪个键。
    pub code: KeyCode,
    /// 有哪些修饰键。
    pub mods: Modifiers,
    /// 是否是"松开"事件。绝大多数输入法逻辑只处理按下。
    pub release: bool,
}

impl Key {
    /// 构造一个"按下"事件。
    #[must_use]
    pub const fn press(code: KeyCode, mods: Modifiers) -> Self {
        Self {
            code,
            mods,
            release: false,
        }
    }

    /// 构造一个"松开"事件。
    #[must_use]
    pub const fn release(code: KeyCode, mods: Modifiers) -> Self {
        Self {
            code,
            mods,
            release: true,
        }
    }

    /// 便捷构造：一个不带修饰键的可打印字符。
    #[must_use]
    pub const fn ch(c: char) -> Self {
        Self::press(KeyCode::Char(c), Modifiers::NONE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_compose() {
        let m = Modifiers::CTRL | Modifiers::SHIFT;
        assert!(m.contains(Modifiers::CTRL));
        assert!(m.contains(Modifiers::SHIFT));
        assert!(!m.contains(Modifiers::ALT));
    }

    #[test]
    fn none_is_empty() {
        assert!(Modifiers::NONE.is_empty());
        assert!(!Modifiers::SHIFT.is_empty());
    }
}
