//! # Processors
//!
//! 中文职责：按键处理器——决定"这一下按键算不算输入"。
//! English role: key processors that decide whether a keystroke is input.
//! 架构位置：`stele-core::Processor` 的实现。
//!
//! # 处理器不直接产出 `Commit`
//!
//! 处理器只表达**意图**（"选第 3 个"），写进
//! [`SessionState::pending_commit`]；由**会话**兑现成完整的 `Commit`。
//! 理由：只有会话才同时知道"已渲染的候选列表"和"当前输入"（见
//! [`stele_core::PendingCommit`] 的说明）。

use stele_core::{
    Key, KeyCode, Modifiers, NamedKey, PendingCommit, ProcessResult, SessionState, Trigger,
};

/// 输入处理器：把可打印字符追加到输入串。
///
/// 只接受**不含 Ctrl / Alt / Super** 的字符——带这些修饰键的按键属于
/// 系统或应用，必须还给操作系统（这正是 [`ProcessResult::Rejected`]
/// 与 [`ProcessResult::Noop`] 的区别所在）。
pub struct Speller {
    /// 除字母数字外还接受哪些字符（例如拼音的音节分隔符）。
    extra_accepted: Vec<char>,
}

impl Default for Speller {
    fn default() -> Self {
        Self::new(vec!['\''])
    }
}

impl Speller {
    /// 构造，并指定额外接受的字符。
    #[must_use]
    pub fn new(extra_accepted: Vec<char>) -> Self {
        Self { extra_accepted }
    }

    fn accepts_char(&self, c: char) -> bool {
        c.is_ascii_alphanumeric() || self.extra_accepted.contains(&c)
    }
}

impl stele_core::Processor for Speller {
    fn process(&mut self, state: &mut SessionState, key: &Key) -> ProcessResult {
        if key.release {
            return ProcessResult::Noop;
        }

        // 带 Ctrl / Alt / Super 的按键不属于输入——**明确地还给系统**。
        let blocked = Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER;
        let mods = Modifiers::from_bits(key.mods.bits() & blocked.bits());
        if !mods.is_empty() {
            return ProcessResult::Rejected;
        }

        let KeyCode::Char(c) = key.code else {
            return ProcessResult::Noop;
        };
        if !self.accepts_char(c) {
            return ProcessResult::Noop;
        }

        state.composition.input.push(c);
        state.composition.caret = state.composition.input.len();
        ProcessResult::Accepted
    }
}

/// 编辑处理器：退格与取消。
///
/// # 退格是**按音节**的
///
/// RIME：「輸入拼音後按退格鍵，也會以音節爲單位回退刪除拼音」。
/// 也就是说敲了 `nihao` 按一下退格，应当回到 `ni` 而不是 `niha`。
///
/// 实现靠**上一次切分的结果**（`composition.segments`）：最后一段的起点
/// 就是要截到的位置。切分结果每次 `compose` 都会重算，所以它总是最新的。
///
/// 若没有切分结果（例如输入还没被处理过），退回按一个字符删。
pub struct Editor;

impl stele_core::Processor for Editor {
    fn process(&mut self, state: &mut SessionState, key: &Key) -> ProcessResult {
        if key.release {
            return ProcessResult::Noop;
        }
        match key.code {
            KeyCode::Named(NamedKey::Backspace) => {
                if state.composition.input.is_empty() {
                    // 输入串已空：退格应该还给系统（去删别处的文字）。
                    return ProcessResult::Noop;
                }
                // 优先按音节回退。
                let cut = last_segment_start(&state.composition)
                    .filter(|c| *c < state.composition.input.len());
                match cut {
                    Some(pos) => state.composition.input.truncate(pos),
                    None => {
                        state.composition.input.pop();
                    }
                }
                state.composition.caret = state.composition.input.len();
                // 截断之后旧的分段不再成立，清掉以免下一次退格用错边界。
                state.composition.segments.clear();
                ProcessResult::Accepted
            }
            KeyCode::Named(NamedKey::Escape) => {
                if state.composition.is_active() {
                    state.composition.reset();
                    ProcessResult::Accepted
                } else {
                    ProcessResult::Noop
                }
            }
            _ => ProcessResult::Noop,
        }
    }
}

/// 选词处理器：空格 / 回车 / 数字键选词。
///
/// 它**不检查候选是否存在**——那是会话的事。因此它总是接受按键；
/// 会话在兑现时若发现下标越界，就当作"没选中"处理。
/// 由于兜底翻译器保证"永远至少有一个候选"，实践下标 0 总是有效的。
pub struct Selector;

impl stele_core::Processor for Selector {
    fn process(&mut self, state: &mut SessionState, key: &Key) -> ProcessResult {
        if key.release || !state.composition.is_active() {
            return ProcessResult::Noop;
        }
        // 带修饰键的选词不算选词（例如 Ctrl+1 是切标签页）。
        if !Modifiers::from_bits(key.mods.bits() & Modifiers::CTRL.bits()).is_empty() {
            return ProcessResult::Noop;
        }

        let pending = match key.code {
            KeyCode::Named(NamedKey::Space) => Some(PendingCommit::keyboard(0, Trigger::Space)),
            KeyCode::Named(NamedKey::Enter) => Some(PendingCommit::keyboard(0, Trigger::Enter)),
            KeyCode::Named(NamedKey::Digit(d)) if (1..=9).contains(&d) => Some(
                PendingCommit::keyboard(usize::from(d - 1), Trigger::Explicit),
            ),
            _ => None,
        };

        match pending {
            Some(p) => {
                state.pending_commit = Some(p);
                ProcessResult::Accepted
            }
            None => ProcessResult::Noop,
        }
    }
}

/// 最后一段的起始字节位置。
fn last_segment_start(c: &stele_core::Composition) -> Option<usize> {
    c.segments.segments.last().map(|s| s.span.start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stele_core::Processor;

    fn state() -> SessionState {
        SessionState::default()
    }

    #[test]
    fn speller_appends_printable_characters() {
        let mut s = state();
        let mut p = Speller::default();
        assert_eq!(p.process(&mut s, &Key::ch('n')), ProcessResult::Accepted);
        assert_eq!(s.composition.input, "n");
        assert_eq!(s.composition.caret, 1);
    }

    #[test]
    fn speller_rejects_control_modified_keys() {
        // Ctrl+C 属于系统 —— 必须**明确还给系统**，而不是"我不管"。
        let mut s = state();
        let mut p = Speller::default();
        let key = Key::press(KeyCode::Char('c'), Modifiers::CTRL);
        assert_eq!(p.process(&mut s, &key), ProcessResult::Rejected);
        assert!(s.composition.input.is_empty());
    }

    #[test]
    fn speller_accepts_the_apostrophe_delimiter() {
        let mut s = state();
        let mut p = Speller::default();
        assert_eq!(p.process(&mut s, &Key::ch('\'')), ProcessResult::Accepted);
        assert_eq!(s.composition.input, "'");
    }

    #[test]
    fn backspace_removes_a_whole_unit_when_segmented() {
        // 敲 nihao 后按一下退格：按音节回退到 `ni`，而不是 `niha`。
        let mut s = state();
        s.composition.input = "nihao".into();
        s.composition.caret = 5;
        // `nihao` 切成 [ni][hao] 两段 —— 退格应当回到最后一段的起点（2）。
        for (a, b) in [(0usize, 2usize), (2, 5)] {
            let mut seg = stele_core::Segment::new(stele_core::Span::new(a, b));
            seg.tags.push("abc");
            s.composition.segments.segments.push(seg);
        }

        let bs = Key::press(KeyCode::Named(NamedKey::Backspace), Modifiers::NONE);
        assert_eq!(Editor.process(&mut s, &bs), ProcessResult::Accepted);
        assert_eq!(s.composition.input, "ni");
    }

    #[test]
    fn backspace_falls_back_to_one_char_without_segments() {
        let mut s = state();
        s.composition.input = "nihao".into();
        let bs = Key::press(KeyCode::Named(NamedKey::Backspace), Modifiers::NONE);
        assert_eq!(Editor.process(&mut s, &bs), ProcessResult::Accepted);
        assert_eq!(s.composition.input, "niha");
    }

    #[test]
    fn editor_backspaces_and_resets() {
        let mut s = state();
        let mut sp = Speller::default();
        let mut ed = Editor;
        for c in "nihao".chars() {
            sp.process(&mut s, &Key::ch(c));
        }
        let bs = Key::press(KeyCode::Named(NamedKey::Backspace), Modifiers::NONE);
        assert_eq!(ed.process(&mut s, &bs), ProcessResult::Accepted);
        assert_eq!(s.composition.input, "niha");

        let esc = Key::press(KeyCode::Named(NamedKey::Escape), Modifiers::NONE);
        assert_eq!(ed.process(&mut s, &esc), ProcessResult::Accepted);
        assert!(s.composition.input.is_empty());
    }

    #[test]
    fn editor_gives_backspace_back_when_nothing_to_delete() {
        let mut s = state();
        let mut ed = Editor;
        let bs = Key::press(KeyCode::Named(NamedKey::Backspace), Modifiers::NONE);
        // 输入串为空时，退格应该去删别处的文字。
        assert_eq!(ed.process(&mut s, &bs), ProcessResult::Noop);
    }

    #[test]
    fn selector_requests_a_commit_only_when_composing() {
        let mut s = state();
        let mut sel = Selector;
        let space = Key::press(KeyCode::Named(NamedKey::Space), Modifiers::NONE);

        // 没有输入时，空格还给系统。
        assert_eq!(sel.process(&mut s, &space), ProcessResult::Noop);

        s.composition.input.push('n');
        assert_eq!(sel.process(&mut s, &space), ProcessResult::Accepted);
        assert_eq!(
            s.pending_commit,
            Some(PendingCommit::keyboard(0, Trigger::Space))
        );
    }

    #[test]
    fn selector_maps_digit_keys_to_indexes() {
        let mut s = state();
        s.composition.input.push('n');
        let mut sel = Selector;
        let d3 = Key::press(KeyCode::Named(NamedKey::Digit(3)), Modifiers::NONE);
        assert_eq!(sel.process(&mut s, &d3), ProcessResult::Accepted);
        assert_eq!(
            s.pending_commit,
            Some(PendingCommit::keyboard(2, Trigger::Explicit))
        );
    }
}
