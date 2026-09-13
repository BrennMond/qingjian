//! # Processors
//!
//! 中文职责：按键处理器——决定"这一下按键算不算输入"。
//! English role: key processors that decide whether a keystroke is input.
//! 架构位置：`stele-core::Processor` 的实现。
//!
//! # 处理器不直接产出 `Commit`
//!
//! 处理器只表达**意图**（"选第 3 个" / "上屏这个标点"），写进
//! [`SessionState::pending_commit`]；由**会话**兑现成完整的 `Commit`。
//! 理由：只有会话才同时知道"已渲染的候选列表"和"当前输入"（见
//! [`stele_core::PendingCommit`] 的说明）。
//!
//! # 处理器之间也要能"转交"
//!
//! `key_binder` 的一条绑定可能是 `accept: Shift+space, send: space`——
//! 意思是"把这一下按键**换成另一下**再走一遍"。因此处理器可以在
//! [`SessionState::sent_keys`] 里塞一个按键，由流水线**在同一次按键内**
//! 继续派发。没有这条通路的话，按键重绑定就只能实现成
//! "引擎自己认下空格"——那等于把选择器的逻辑抄一遍。

use stele_core::{
    Key, KeyCode, Modifiers, NamedKey, PendingCommit, ProcessResult, SessionState, Trigger,
};

/// 输入处理器：把可打印字符追加到输入串。
///
/// 只接受**不含 Ctrl / Alt / Super** 的字符——带这些修饰键的按键属于
/// 系统或应用，必须还给操作系统（这正是 [`ProcessResult::Rejected`]
/// 与 [`ProcessResult::Noop`] 的区别所在）。
///
/// # 字母表是可选的
///
/// 方案声明了 `speller/alphabet` 时，**不在表里的字符一概不收**。
/// 这不是优化，而是正确性：收下一个永远查不到的字符，用户看到的是
/// "候选突然全没了"，而原因是"你敲了一个本方案不认识的字母"。
/// 拒收之后它会被后面的处理器（标点）或系统接走。
pub struct Speller {
    /// 除字母数字外还接受哪些字符（例如方案的编码分隔符）。
    extra_accepted: Vec<char>,
    /// 方案声明的字母表（`None` = 不限制）。
    alphabet: Option<Vec<char>>,
    /// 被哪个开关**抑制**（通常是"英文模式"）。
    ///
    /// 开着的输入法**必须**有一个"现在别管我"的开关，否则用户没法
    /// 在同一个窗口里打代码。名字来自方案数据。
    blocked_by_option: Option<String>,
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
        Self {
            extra_accepted,
            alphabet: None,
            blocked_by_option: None,
        }
    }

    /// 限定字母表。
    #[must_use]
    pub fn with_alphabet(mut self, alphabet: Vec<char>) -> Self {
        self.alphabet = Some(alphabet);
        self
    }

    /// 被某个开关抑制（开着时本处理器不工作）。
    #[must_use]
    pub fn blocked_by(mut self, option: impl Into<String>) -> Self {
        self.blocked_by_option = Some(option.into());
        self
    }

    fn accepts_char(&self, c: char) -> bool {
        if let Some(a) = &self.alphabet {
            if !a.contains(&c) {
                return false;
            }
        } else if !(c.is_ascii_alphanumeric() || self.extra_accepted.contains(&c)) {
            return false;
        }
        true
    }
}

impl stele_core::Processor for Speller {
    fn name(&self) -> &'static str {
        "speller"
    }

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

    fn enabled(&self, options: &stele_core::Options) -> bool {
        // 开关没声明时 `get` 返回 false —— 于是"没配这个开关"等于"不被抑制"，
        // 而装载期会检查方案里是否真的声明了它。
        !self
            .blocked_by_option
            .as_deref()
            .is_some_and(|n| options.get(n))
    }
}

/// 编辑处理器：退格、取消，以及方案声明的**动作绑定**。
///
/// # 退格是**按编码单元**的
///
/// RIME：「輸入拼音後按退格鍵，也會以音節爲單位回退刪除拼音」。
/// 也就是说敲了 `nihao` 按一下退格，应当回到 `ni` 而不是 `niha`。
///
/// 实现靠**上一次切分的结果**（`composition.segments`）：最后一段的起点
/// 就是要截到的位置。切分结果每次 `compose` 都会重算，所以它总是最新的。
///
/// # 默认绑定
///
/// 没有任何方案配置时，用 RIME 的默认那一套（`editor.cc` 的行为）。
/// 方案给了 `editor/bindings` 就**整体替换**它——这是 RIME 的语义，
/// 照抄：否则"我在方案里写了一条绑定"会被默认值悄悄盖住。
pub struct Editor {
    /// 按键 → 动作。
    bindings: Vec<(crate::spec::KeyChord, crate::spec::EditorAction)>,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl Editor {
    /// 由方案的绑定构造（空 = 用默认绑定）。
    #[must_use]
    pub fn new(bindings: Vec<(crate::spec::KeyChord, crate::spec::EditorAction)>) -> Self {
        Self { bindings }
    }

    /// 默认绑定：RIME `editor.cc` 的那一套。
    #[must_use]
    pub fn default_bindings() -> Vec<(crate::spec::KeyChord, crate::spec::EditorAction)> {
        use crate::spec::{EditorAction as A, KeyChord as C};
        let none = Modifiers::NONE;
        let ctrl = Modifiers::CTRL;
        vec![
            (
                C::new(KeyCode::Named(NamedKey::Backspace), none),
                A::BackUnit,
            ),
            (
                C::new(KeyCode::Named(NamedKey::Backspace), ctrl),
                A::BackUnit,
            ),
            (
                C::new(KeyCode::Named(NamedKey::Delete), none),
                A::DeleteForward,
            ),
            (C::new(KeyCode::Named(NamedKey::Escape), none), A::Cancel),
            (
                C::new(KeyCode::Named(NamedKey::Enter), none),
                A::CommitRawInput,
            ),
        ]
    }

    /// 这个按键对应的动作。
    fn action_for(&self, key: &Key) -> Option<crate::spec::EditorAction> {
        let table: Vec<(crate::spec::KeyChord, crate::spec::EditorAction)> =
            if self.bindings.is_empty() {
                Self::default_bindings()
            } else {
                self.bindings.clone()
            };
        // **精确修饰键优先**：`Control+BackSpace` 与 `BackSpace` 都绑了动作时，
        // 按了 Ctrl 的那一下不该命中不要求 Ctrl 的那条。
        // 顺序表里先精确匹配，再退到"修饰键更少"的。
        table
            .iter()
            .filter(|(c, _)| c.matches(key))
            .max_by_key(|(c, _)| c.mods.bits().count_ones())
            .map(|(_, a)| *a)
    }
}

impl stele_core::Processor for Editor {
    fn name(&self) -> &'static str {
        "editor"
    }

    fn process(&mut self, state: &mut SessionState, key: &Key) -> ProcessResult {
        use crate::spec::EditorAction as A;
        if key.release {
            return ProcessResult::Noop;
        }
        let Some(action) = self.action_for(key) else {
            return ProcessResult::Noop;
        };
        match action {
            A::Confirm => {
                // "上屏当前高亮候选" —— 意图与空格完全一致，
                // 因此写成同一种意图，由会话统一兑现。
                if state.composition.is_active() {
                    state.pending_commit = Some(PendingCommit::keyboard(0, Trigger::Space));
                    ProcessResult::Accepted
                } else {
                    ProcessResult::Noop
                }
            }
            A::CommitRawInput => {
                if state.composition.is_active() {
                    let text = state.composition.input.clone();
                    state.pending_commit = Some(PendingCommit::literal(text, Trigger::Enter));
                    ProcessResult::Accepted
                } else {
                    ProcessResult::Noop
                }
            }
            A::CommitScriptText => {
                if state.composition.is_active() {
                    // 预编辑串就是"变换后的输入"（`preedit_format` 已经作用过）。
                    let text = state.composition.preedit.clone();
                    state.pending_commit = Some(PendingCommit::literal(text, Trigger::Enter));
                    ProcessResult::Accepted
                } else {
                    ProcessResult::Noop
                }
            }
            A::Revert => {
                if state.composition.input.is_empty() {
                    return ProcessResult::Noop;
                }
                state.composition.input.pop();
                state.composition.caret = state.composition.input.len();
                state.composition.segments.clear();
                ProcessResult::Accepted
            }
            A::BackUnit => {
                if state.composition.input.is_empty() {
                    // 输入串已空：退格应该还给系统（去删别处的文字）。
                    return ProcessResult::Noop;
                }
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
            A::DeleteForward => {
                // 输入串的光标永远在末尾（P3 不支持插入点移动），
                // 因此"向后删"在没有更多内容时就是"还给系统"。
                if state.composition.caret >= state.composition.input.len() {
                    return ProcessResult::Noop;
                }
                ProcessResult::Accepted
            }
            A::Cancel => {
                if state.composition.is_active() {
                    state.composition.reset();
                    ProcessResult::Accepted
                } else {
                    ProcessResult::Noop
                }
            }
            A::CommitComment => {
                // 注释在**候选**里，而处理器看不到候选列表——因此这里
                // 只表达意图，由会话取当前高亮候选的 `comment` 上屏。
                if state.composition.is_active() {
                    state.pending_commit = Some(PendingCommit::CommitComment { clear: true });
                    ProcessResult::Accepted
                } else {
                    ProcessResult::Noop
                }
            }
            A::CommitComposition => {
                // `ctx->ConfirmCurrentSelection() || ctx->Commit()`：
                // 有候选就选第一个（确认），确认之后没有候选菜单了才整串上屏。
                // 会话在兑现时会处理"候选不存在"的情形。
                if state.composition.is_active() {
                    state.pending_commit = Some(PendingCommit::keyboard(0, Trigger::Enter));
                    ProcessResult::Accepted
                } else {
                    ProcessResult::Noop
                }
            }
            A::ReopenOrConfirm => {
                // 我们没有"已选段"这一层状态（P3 的分段每次重算），
                // 因此"退回上一个已选段"退不回去 —— 按 librime 的语义
                // 落到后半句：确认当前选择。
                if state.composition.is_active() {
                    state.pending_commit = Some(PendingCommit::keyboard(0, Trigger::Space));
                    ProcessResult::Accepted
                } else {
                    ProcessResult::Noop
                }
            }
            A::BackStep => {
                // 三级兜底：退段 / 退选择 / 退一个字符。前两级要的是
                // "已选段"与"选择历史"，我们没有，于是落到第三级——
                // 与 `BackUnit` 的区别在这里体现：`BackUnit` **优先按
                // 编码单元**退，而 `BackStep` 只退一个字符。
                if state.composition.input.is_empty() {
                    return ProcessResult::Noop;
                }
                state.composition.input.pop();
                state.composition.caret = state.composition.input.len();
                state.composition.segments.clear();
                ProcessResult::Accepted
            }
            A::DeleteCandidate => {
                // 学习型删除：真正"从记忆里删掉"是 P4a 的事
                // （`MemoryStore::forget`）。这里把意图交出去。
                if state.composition.is_active() {
                    state.pending_commit = Some(PendingCommit::DeleteCandidate { index: 0 });
                    ProcessResult::Accepted
                } else {
                    ProcessResult::Noop
                }
            }
            A::Noop => {
                // `editor/bindings` 是**整体替换**默认表的（librime 的
                // `LoadConfig` 也是覆盖式写入），因此"解除绑定"在这里
                // 等价于"不写这一条"。走到这里说明方案**显式写了** `noop`：
                // 它的意思是"这个键别管了"，也就是**还给系统**。
                //
                // 与 `ProcessResult::Noop`（"我不管，后面的人可能管"）
                // 的区别很重要：后者会让后面的处理器继续处理这个键。
                ProcessResult::Rejected
            }
        }
    }
}

/// **中英切换处理器**。
///
/// 对应 RIME 的 `ascii_composer`。它只做两件事：
///
/// 1. **切换**那个"英文模式"开关（Shift 键、或方案声明的按键）；
/// 2. 开着时**明确拒绝**可打印字符，让前端把按键原样交给应用。
///
/// # 为什么是"拒绝"而不是"自己上屏"
///
/// 输入法在英文模式下最不该做的事就是"假装打字"：应用可能在做
/// 自动补全、可能有自己的快捷键、可能是密码框。所以那一下按键
/// **必须原样回到应用手里**，而不是由引擎上屏一个看起来一样的字符。
/// 这也正是 [`ProcessResult::Rejected`] 与 `Noop` 的区别所在。
pub struct AsciiComposer {
    /// 哪个开关表示"英文模式"。
    option: Option<String>,
}

impl AsciiComposer {
    /// 由开关名构造。
    #[must_use]
    pub fn new(option: Option<String>) -> Self {
        Self { option }
    }

    fn ascii_on(&self, state: &SessionState) -> bool {
        self.option.as_deref().is_some_and(|n| state.options.get(n))
    }
}

impl stele_core::Processor for AsciiComposer {
    fn name(&self) -> &'static str {
        "ascii_composer"
    }

    fn process(&mut self, state: &mut SessionState, key: &Key) -> ProcessResult {
        let Some(name) = self.option.clone() else {
            return ProcessResult::Noop;
        };
        if key.release {
            return ProcessResult::Noop;
        }

        // ── Shift / CapsLock **单独按下**：切换 ──
        //
        // 两者都是"按一下换一次"，而不是"跟随某个状态位"。
        //
        // # CapsLock 为什么是"按下沿"而不是"跟随 CAPS 位"
        //
        // 我第一版让它跟随 `Modifiers::CAPS`（前端给出的开关状态），
        // 并把"上一次的状态"记在处理器自己的字段里。那有两个问题：
        //
        // 1. **`AsciiComposer` 是每会话一份**，于是"A 会话的 CapsLock 状态"
        //    会影响 B 会话——一个客户端的锁定状态泄漏到另一个客户端。
        // 2. 它让 `{accept: C, send: Caps_Lock}` 这类绑定**在第一次按键时
        //    什么都不做**（记录的状态从 false 变 false，没有"沿"），
        //    而那正是 RIME 方案里"把某个键换成中英切换"的常见写法。
        //
        // 按下沿的判据因此落在**按键本身**：敲了一下 CapsLock = 想切换。
        // 前端若要表达"锁定被外部改成了开"，用 `Session::set_option`。
        //
        // `Shift` + 字母 在 [`KeyCode`] 里已经被归一化成大写字符，
        // 因此"单独按下"只能是具名键那一种形态。
        let bare_toggle = match key.code {
            KeyCode::Named(NamedKey::Shift) => {
                key.mods.contains(Modifiers::SHIFT)
                    && !key.mods.contains(Modifiers::CTRL)
                    && !key.mods.contains(Modifiers::ALT)
            }
            KeyCode::Named(NamedKey::CapsLock) => {
                !key.mods.contains(Modifiers::CTRL) && !key.mods.contains(Modifiers::ALT)
            }
            _ => false,
        };
        if bare_toggle {
            state.toggle_option(&name);
            return ProcessResult::Accepted;
        }

        // ── Esc：先取消输入，再谈退出英文模式 ──
        if matches!(key.code, KeyCode::Named(NamedKey::Escape)) {
            if state.composition.is_active() {
                state.composition.reset();
                return ProcessResult::Accepted;
            }
            if self.ascii_on(state) {
                state.set_option(&name, false);
                return ProcessResult::Accepted;
            }
            return ProcessResult::Noop;
        }

        if !self.ascii_on(state) {
            return ProcessResult::Noop;
        }

        // ── 英文模式：可打印字符与空格**还给系统** ──
        match key.code {
            KeyCode::Char(c) if !c.is_control() => ProcessResult::Rejected,
            KeyCode::Named(NamedKey::Space) => ProcessResult::Rejected,
            _ => ProcessResult::Noop,
        }
    }
}

/// **翻页处理器**。
///
/// # 目前的能力与边界（诚实交代）
///
/// 它做的是**视图翻页**：候选列表已经算出来了，翻页只是把窗口往后挪。
/// 真正的 RIME 是"翻译器按页查询"（每一页去词库要新的一批），
/// 那需要给 `Lexicon` 加"从第 N 条开始"的能力——**P3 不做**。
///
/// 这条边界在实践中的后果：一个音节有 200 个同音字时，第 3 页之后
/// 看不到。而候选上限是 200（[`crate::pipeline::CANDIDATE_CAP`]），
/// 常见输入法一页 5–9 个，也就是前 20–30 页可用。
pub struct Navigator {
    /// 页码（0 起）。
    page: usize,
    /// 上一页按键。
    page_up: Vec<crate::spec::KeyChord>,
    /// 下一页按键。
    page_down: Vec<crate::spec::KeyChord>,
}

impl Navigator {
    /// 构造。
    #[must_use]
    pub fn new(spec: &crate::spec::NavigatorSpec, page_size: usize) -> Self {
        // 每页多少个由**流水线**决定（它才知道候选上限与显示宽度），
        // 这里只记下来备查：`navigator` 的职责是"翻到第几页"，
        // 而"一页有多少个"是渲染的事。两个职责分开，翻页与裁剪
        // 才不会各自用一套页大小算出不同的页数。
        let _ = page_size;
        Self {
            page: 0,
            page_up: spec.page_up.clone(),
            page_down: spec.page_down.clone(),
        }
    }

    /// 当前页（0 起）。
    #[must_use]
    pub fn page(&self) -> usize {
        self.page
    }

    /// 回到第一页（输入变了就必须回第一页）。
    pub fn reset(&mut self) {
        self.page = 0;
    }
}

impl stele_core::Processor for Navigator {
    fn name(&self) -> &'static str {
        "navigator"
    }

    fn process(&mut self, state: &mut SessionState, key: &Key) -> ProcessResult {
        if !state.composition.is_active() {
            return ProcessResult::Noop;
        }
        let up = self.page_up.iter().any(|c| c.matches(key));
        let down = self.page_down.iter().any(|c| c.matches(key));
        if !up && !down {
            return ProcessResult::Noop;
        }
        let pages = state.candidate_pages.max(1);
        if down && self.page + 1 < pages {
            self.page += 1;
            state.candidate_page = self.page;
            return ProcessResult::Accepted;
        }
        if up && self.page > 0 {
            self.page -= 1;
            state.candidate_page = self.page;
            return ProcessResult::Accepted;
        }
        // 到头了：**不吞按键**，让系统或其他处理器去处理
        // （"还有吗？没有了"不该把按键吃掉）。
        ProcessResult::Noop
    }
}

/// **按键重绑定处理器**。
///
/// 对应 RIME 的 `key_binder`。支持与 `librime` 相同的两类效果：
///
/// | 配置 | 效果 |
/// | --- | --- |
/// | `send` / `send_sequence` | 把这一下按键**换成另一串按键**，重新派发 |
/// | `toggle` | 切换一个开关 |
///
/// # 重新派发的两条语义（照抄 `librime`，原先我写错了）
///
/// librime `src/rime/gear/key_binder.cc`：
///
/// ```cpp
/// void KeyBinder::PerformKeyBinding(const KeyBinding& binding) {
///   if (binding.action) { binding.action(engine_); }
///   else {
///     redirecting_ = true;
///     for (const KeyEvent& key_event : binding.target)
///       engine_->ProcessKey(key_event);       // ← 顶层入口
///     redirecting_ = false;
///   }
/// }
/// ProcessResult KeyBinder::ProcessKeyEvent(const KeyEvent& key_event) {
///   if (redirecting_ || ...) return kNoop;    // ← 唯一的防重入
/// ```
///
/// 于是：
///
/// 1. **换来的按键从整条处理器链的最开头重新走**（`engine_->ProcessKey`
///    就是顶层入口）——不是"从 `key_binder` 之后"。
/// 2. 防重入靠**一个布尔标志**，只有 `key_binder` 自己看它。
///    于是 `{accept: space, send: space}` 不会死循环：换成的那一下
///    被 `key_binder` 直接放行，继续往后走到选择器。
///
/// 我第一版写成"从 `key_binder` 之后派发 + 轮数上限"——那个实现能跑，
/// 但**语义不同**：`send` 换来的键在前面那些处理器（中英切换、输入
/// 处理器）眼里等于没发生过。RIME 的方案依赖它们看到。
pub struct KeyBinder {
    bindings: Vec<crate::spec::KeyBinding>,
}

impl KeyBinder {
    /// 构造。
    #[must_use]
    pub fn new(bindings: Vec<crate::spec::KeyBinding>) -> Self {
        Self { bindings }
    }
}

impl stele_core::Processor for KeyBinder {
    fn name(&self) -> &'static str {
        "key_binder"
    }

    fn process(&mut self, state: &mut SessionState, key: &Key) -> ProcessResult {
        // **防重入**：正在派发"换来的按键"时一律放行。
        // 这就是 librime 里那句 `if (redirecting_ || ...) return kNoop;`，
        // 也是 `{accept: space, send: space}` 不会死循环的全部原因。
        if key.release || state.redirecting {
            return ProcessResult::Noop;
        }
        for b in &self.bindings {
            // `when` 谓词。
            let ok = match b.when {
                crate::spec::WhenPredicate::Always => true,
                crate::spec::WhenPredicate::Composing => state.composition.is_active(),
                crate::spec::WhenPredicate::Paging => state.candidate_pages > 1,
                crate::spec::WhenPredicate::HasMenu => state.candidate_count > 0,
                // 下一词预测是 P4b 的内容；现在永远为假（见 `WhenPredicate`）。
                crate::spec::WhenPredicate::Predicting => false,
            };
            if !ok || !b.accept.iter().any(|c| c.matches(key)) {
                continue;
            }
            // 绑定命中。**只有一件事会发生**——librime 的 `if / else if`
            // 链（`send` → `send_sequence` → `toggle` → `set_option` →
            // `unset_option` → `select`），见 [`KeyBinding::effect`]。
            // 装载器已经把"同时写了多个"报成诊断了。
            match b.effect() {
                "send" => {
                    // `send` 与 `send_sequence` 是**同一串按键、按顺序派发**
                    // （librime 的 `binding.target` 是一个 `KeySequence`）。
                    if let Some(seq) = &b.send_keys {
                        let keys = parse_send_sequence(seq);
                        if !keys.is_empty() {
                            state.sent_keys.extend(keys);
                        }
                    }
                }
                "toggle" => {
                    if let Some(name) = &b.toggle {
                        state.toggle_option(name);
                    }
                }
                "set_option" => {
                    if let Some(name) = &b.set_option {
                        state.set_option(name, true);
                    }
                }
                "unset_option" => {
                    if let Some(name) = &b.unset_option {
                        state.set_option(name, false);
                    }
                }
                // `select`（切换方案）需要 SchemaCatalog，属于会话语义，
                // 处理器拿不到——装载期会报"尚未支持"。
                _ => {}
            }
            return ProcessResult::Accepted;
        }
        ProcessResult::Noop
    }
}

/// 把方案的 `send` / `send_sequence` 值解析成一串按键。
///
/// # 两种取值
///
/// - **键名**（`space`、`Page_Up`、`Control+BackSpace`）→ 对应的键。
///   这是 RIME 的正规写法，也是翻页类绑定的唯一表达方式。
/// - **一段文本**（`"，"`、`"test"`）→ 逐个字符当普通字符键。
///   这是我们额外容忍的写法：RIME 的 `send` 只认键名，但"上屏一个中文
///   标点"用键名表达不了，而它在真实方案里很常见。
///
/// 空串返回空序列（调用方据此判断"这条绑定没效果"）。
#[must_use]
pub fn parse_send_sequence(seq: &[String]) -> Vec<Key> {
    let mut out = Vec::new();
    for item in seq {
        match crate::keyspec::parse_key_name(item) {
            Some(chord) => out.push(Key::press(chord.code, chord.mods)),
            None => out.extend(item.chars().map(crate::keyspec::key_for_char)),
        }
    }
    out
}

/// 最后一段的起始字节位置。
fn last_segment_start(c: &stele_core::Composition) -> Option<usize> {
    c.segments.segments.last().map(|s| s.span.start)
}

/// 选词处理器：空格 / 回车 / 数字键选词。
///
/// 它**不检查候选是否存在**——那是会话的事。因此它总是接受按键；
/// 会话在兑现时若发现下标越界，就当作"没选中"处理。
/// 由于兜底翻译器保证"永远至少有一个候选"，实践下标 0 总是有效的。
///
/// # 它为什么排在 `editor` 之后
///
/// `editor` 决定"回车是上屏原始输入还是确认候选"，`selector` 只管
/// "空格 / 数字选第几个"。方案把回车绑成 `commit_raw_input` 时，
/// `editor` 先接住它，`selector` 就不会再把它当成"确认候选"。
/// 这个先后关系**由方案声明**（RIME 的 `engine.processors` 顺序），
/// 不是写死的。
pub struct Selector;

impl stele_core::Processor for Selector {
    fn name(&self) -> &'static str {
        "selector"
    }

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
    fn speller_refuses_letters_outside_the_declared_alphabet() {
        // 一个永远查不到的字符被收进输入串，症状是"候选突然全没了"。
        // 拒收才是对的：它会被标点处理器或系统接走。
        let mut s = state();
        let mut p = Speller::new(vec![]).with_alphabet(vec!['n', 'i']);
        assert_eq!(p.process(&mut s, &Key::ch('n')), ProcessResult::Accepted);
        assert_eq!(p.process(&mut s, &Key::ch('z')), ProcessResult::Noop);
        assert_eq!(s.composition.input, "n");
    }

    #[test]
    fn speller_can_be_suppressed_by_a_switch() {
        let mut s = state();
        s.options
            .declare(stele_core::Switch::new("ascii_mode", true));
        let p = Speller::new(vec![])
            .with_alphabet(vec!['n'])
            .blocked_by("ascii_mode");
        // 抑制由 `enabled()` 表达 —— **流水线在调用之前就问它**，
        // 因此处理器自己不必在 `process` 里再判一次（判两次就会有两个
        // 执行点，改一处漏一处）。
        assert!(!p.enabled(&s.options), "开关开着时输入处理器应当不工作");

        // 关掉开关，它立刻恢复。
        s.options.set("ascii_mode", false);
        assert!(p.enabled(&s.options));
        let mut p2 = p;
        assert_eq!(p2.process(&mut s, &Key::ch('n')), ProcessResult::Accepted);
        assert_eq!(s.composition.input, "n");
    }

    #[test]
    fn backspace_removes_a_whole_unit_when_segmented() {
        // 敲 nihao 后按一下退格：按编码单元回退到 `ni`，而不是 `niha`。
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
        assert_eq!(
            Editor::default().process(&mut s, &bs),
            ProcessResult::Accepted
        );
        assert_eq!(s.composition.input, "ni");
    }

    #[test]
    fn backspace_falls_back_to_one_char_without_segments() {
        let mut s = state();
        s.composition.input = "nihao".into();
        let bs = Key::press(KeyCode::Named(NamedKey::Backspace), Modifiers::NONE);
        assert_eq!(
            Editor::default().process(&mut s, &bs),
            ProcessResult::Accepted
        );
        assert_eq!(s.composition.input, "niha");
    }

    #[test]
    fn editor_backspaces_and_resets() {
        let mut s = state();
        let mut sp = Speller::default();
        let mut ed = Editor::default();
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
        let mut ed = Editor::default();
        let bs = Key::press(KeyCode::Named(NamedKey::Backspace), Modifiers::NONE);
        // 输入串为空时，退格应该去删别处的文字。
        assert_eq!(ed.process(&mut s, &bs), ProcessResult::Noop);
    }

    #[test]
    fn editor_honours_scheme_declared_actions() {
        use crate::spec::{EditorAction as A, KeyChord as C};
        // 方案把回车绑成"上屏变换后的输入"。
        let mut ed = Editor::new(vec![(
            C::new(KeyCode::Named(NamedKey::Enter), Modifiers::NONE),
            A::CommitScriptText,
        )]);
        let mut s = state();
        s.composition.input = "nihao".into();
        s.composition.preedit = "ni'hao".into();
        let ret = Key::press(KeyCode::Named(NamedKey::Enter), Modifiers::NONE);
        assert_eq!(ed.process(&mut s, &ret), ProcessResult::Accepted);
        assert_eq!(
            s.pending_commit,
            Some(PendingCommit::literal("ni'hao", Trigger::Enter))
        );
    }

    #[test]
    fn editor_prefers_the_exact_modifier_binding() {
        use crate::spec::{EditorAction as A, KeyChord as C};
        let mut ed = Editor::new(vec![
            (
                C::new(KeyCode::Named(NamedKey::Backspace), Modifiers::NONE),
                A::Revert,
            ),
            (
                C::new(KeyCode::Named(NamedKey::Backspace), Modifiers::CTRL),
                A::BackUnit,
            ),
        ]);
        let mut s = state();
        s.composition.input = "nihao".into();
        for (a, b) in [(0usize, 2usize), (2, 5)] {
            let mut seg = stele_core::Segment::new(stele_core::Span::new(a, b));
            seg.tags.push("abc");
            s.composition.segments.segments.push(seg);
        }
        // Ctrl+退格 → 按编码单元回退（整段）。
        let ctrl = Key::press(KeyCode::Named(NamedKey::Backspace), Modifiers::CTRL);
        assert_eq!(ed.process(&mut s, &ctrl), ProcessResult::Accepted);
        assert_eq!(s.composition.input, "ni");
        // 普通退格 → 只删一个字符（这里是 Revert 的动作）。
        let plain = Key::press(KeyCode::Named(NamedKey::Backspace), Modifiers::NONE);
        assert_eq!(ed.process(&mut s, &plain), ProcessResult::Accepted);
        assert_eq!(s.composition.input, "n");
    }

    #[test]
    fn the_full_rime_action_vocabulary_is_parsed() {
        // librime 的 12 个动作 + `noop` 一个都不能少：少一个就会让
        // 一份从 RIME 抄来的方案**整份装不进去**（装载期报"不认识的动作"）。
        for name in crate::spec::EditorAction::all_names() {
            assert!(
                crate::spec::EditorAction::parse(name).is_some(),
                "{name} 列在 all_names 里却解析不出来"
            );
        }
        assert_eq!(
            crate::spec::EditorAction::parse("commit_comment"),
            Some(crate::spec::EditorAction::CommitComment)
        );
        assert_eq!(
            crate::spec::EditorAction::parse("noop"),
            Some(crate::spec::EditorAction::Noop)
        );
    }

    #[test]
    fn noop_means_unbind_not_do_nothing() {
        use crate::spec::{EditorAction as A, KeyChord as C};
        // 方案显式把空格解绑 → 空格应当**还给系统**，而不是继续被
        // 后面的处理器（选择器）当成"确认候选"。这就是 librime 里
        // `this->erase(key_event)` 的意思。
        let mut ed = Editor::new(vec![(
            C::new(KeyCode::Named(NamedKey::Space), Modifiers::NONE),
            A::Noop,
        )]);
        let mut s = state();
        s.composition.input = "ni".into();
        let space = Key::press(KeyCode::Named(NamedKey::Space), Modifiers::NONE);
        assert_eq!(ed.process(&mut s, &space), ProcessResult::Rejected);
        assert!(s.pending_commit.is_none(), "解绑之后不该再请求上屏");
    }

    #[test]
    fn commit_comment_asks_the_session_for_the_current_comment() {
        use crate::spec::{EditorAction as A, KeyChord as C};
        let mut ed = Editor::new(vec![(
            C::new(KeyCode::Named(NamedKey::Enter), Modifiers::CTRL),
            A::CommitComment,
        )]);
        let mut s = state();
        s.composition.input = "ni".into();
        let key = Key::press(KeyCode::Named(NamedKey::Enter), Modifiers::CTRL);
        assert_eq!(ed.process(&mut s, &key), ProcessResult::Accepted);
        assert_eq!(s.pending_commit, Some(PendingCommit::commit_comment()));
    }

    #[test]
    fn ascii_composer_toggles_and_then_rejects_printable_keys() {
        let mut s = state();
        s.options
            .declare(stele_core::Switch::new("ascii_mode", false));
        let mut c = AsciiComposer::new(Some("ascii_mode".into()));

        // Shift 单独按下 → 进英文模式，并**记下这次改动**（状态栏要变）。
        let shift = Key::press(KeyCode::Named(NamedKey::Shift), Modifiers::SHIFT);
        assert_eq!(c.process(&mut s, &shift), ProcessResult::Accepted);
        assert!(s.options.get("ascii_mode"));
        assert_eq!(s.option_events, vec![("ascii_mode".to_owned(), true)]);

        // 英文模式下，字母**还给系统**——引擎不"假装打字"。
        assert_eq!(c.process(&mut s, &Key::ch('a')), ProcessResult::Rejected);

        // 再按一次 Shift → 回中文，字母重新被接受（这里只验证不再拒绝）。
        assert_eq!(c.process(&mut s, &shift), ProcessResult::Accepted);
        assert!(!s.options.get("ascii_mode"));
        assert_eq!(c.process(&mut s, &Key::ch('a')), ProcessResult::Noop);
    }

    #[test]
    fn send_text_reverse_maps_special_keys() {
        // `send: space` 是"空格键"，不是"空格字符"。
        assert_eq!(
            crate::keyspec::key_for_char(' ').code,
            KeyCode::Named(NamedKey::Space)
        );
        assert_eq!(crate::keyspec::key_for_char('a').code, KeyCode::Char('a'));
    }

    #[test]
    fn navigator_flips_pages_within_bounds() {
        use crate::spec::{KeyChord as C, NavigatorSpec};
        let spec = NavigatorSpec {
            page_down: vec![C::new(KeyCode::Named(NamedKey::PageDown), Modifiers::NONE)],
            page_up: vec![C::new(KeyCode::Named(NamedKey::PageUp), Modifiers::NONE)],
            ..Default::default()
        };
        let mut n = Navigator::new(&spec, 5);
        let mut s = state();
        s.composition.input = "ni".into();
        s.candidate_pages = 3;

        let down = Key::press(KeyCode::Named(NamedKey::PageDown), Modifiers::NONE);
        assert_eq!(n.process(&mut s, &down), ProcessResult::Accepted);
        assert_eq!(n.page(), 1);
        assert_eq!(s.candidate_page, 1);
        assert_eq!(n.process(&mut s, &down), ProcessResult::Accepted);
        assert_eq!(n.page(), 2);
        // 到头了：**不吞按键**。
        assert_eq!(n.process(&mut s, &down), ProcessResult::Noop);
        assert_eq!(n.page(), 2);
    }

    #[test]
    fn key_binder_sends_another_key_and_toggles() {
        use crate::spec::{KeyBinding, KeyChord as C, WhenPredicate};
        let bindings = vec![
            KeyBinding {
                when: WhenPredicate::Always,
                accept: vec![C::new(KeyCode::Named(NamedKey::Space), Modifiers::SHIFT)],
                send_keys: Some(vec!["space".into()]),
                toggle: None,
                set_option: None,
                unset_option: None,
                at: crate::spec::At::new(1),
            },
            KeyBinding {
                when: WhenPredicate::Always,
                accept: vec![C::new(KeyCode::Char('`'), Modifiers::NONE)],
                send_keys: None,
                toggle: Some("ascii_mode".into()),
                set_option: None,
                unset_option: None,
                at: crate::spec::At::new(2),
            },
        ];
        let mut kb = KeyBinder::new(bindings);
        let mut s = state();
        s.options
            .declare(stele_core::Switch::new("ascii_mode", false));

        // Shift+空格 → 换成普通空格重新派发。
        let shift_space = Key::press(KeyCode::Named(NamedKey::Space), Modifiers::SHIFT);
        assert_eq!(kb.process(&mut s, &shift_space), ProcessResult::Accepted);
        assert_eq!(s.sent_keys.len(), 1);
        // 空格**必须还原成空格键**，而不是 `Char(' ')`——
        // 后者会让选择器与编辑器都不认它（见 `key_for_char`）。
        assert_eq!(s.sent_keys[0].code, KeyCode::Named(NamedKey::Space));
        assert!(s.sent_keys[0].mods.is_empty());

        // 反引号 → 切开关。
        assert_eq!(kb.process(&mut s, &Key::ch('`')), ProcessResult::Accepted);
        assert!(s.options.get("ascii_mode"));
    }
}
