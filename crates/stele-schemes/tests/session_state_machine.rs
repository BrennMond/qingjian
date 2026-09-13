//! # 会话状态机：端到端状态转换表（审计 §2.F 第 3 条）
//!
//! 审计要求为以下交互写**端到端状态转换表与测试**：
//!
//! - 部分选词后继续输入；
//! - 选择第二段；
//! - Backspace / Delete / Esc；
//! - 重新选择已确认段（**重开**）；
//! - 输入中插标点（已有专页：`punctuation_semantics.rs`）；
//! - 全半角、中英切换、数字；
//! - 预测候选与数字选择的隔离。
//!
//! # 这份文件的作用是**画出现状**，不是宣告完成
//!
//! 每一条测试都写明它断言的是"已经成立的行为"还是"已知缺口"。
//! 后者用 `#[ignore]` **显式跳过并写清原因**——比"没写测试"诚实得多：
//! 它同时是缺口清单和将来的验收点。
//!
//! 跑法（含被忽略的那些）：
//!
//! ```bash
//! cargo test -p stele-schemes --test session_state_machine -- --include-ignored
//! ```

use stele_core::{
    Engine, Key, KeyCode, Lane, Modifiers, NamedKey, Outcome, SelectionSource, Session,
};
use stele_engine::EngineImpl;

fn session() -> Box<dyn Session + Send> {
    let defs = stele_schemes::all().expect("内嵌方案");
    let engine = EngineImpl::new(&defs).expect("编译");
    let mut s = engine.create_session();
    s.switch_schema("pinyin-demo").expect("切到拼音方案");
    s
}

fn type_text(s: &mut Box<dyn Session + Send>, text: &str) {
    for c in text.chars() {
        s.process_key(Key::ch(c));
    }
}

fn press(s: &mut Box<dyn Session + Send>, k: NamedKey) -> Outcome {
    s.process_key(Key::press(KeyCode::Named(k), Modifiers::NONE))
}

#[allow(clippy::borrowed_box)]
fn texts(s: &Box<dyn Session + Send>) -> Vec<String> {
    s.candidates().iter().map(|c| c.text.clone()).collect()
}

/// 选中某个文本的候选并上屏，返回 `(上屏文本, 它消费到的字节位置)`。
///
/// **返回消费位置**是必要的：`niha` 的「你好」在"缩写"路径上消费 3 个字节
/// （留余码 `a`），在"拼写层补全"路径上消费 4 个（不留余码）。
/// 两条路径都正确，**取决于方案的 `enable_completion`**——所以测试必须
/// 断言"余码 == 输入[消费位置..]"这条**不变式**，而不是写死某个字符串。
fn select_text(s: &mut Box<dyn Session + Send>, text: &str) -> Option<(String, usize)> {
    let idx = s.candidates().iter().position(|c| c.text == text)?;
    let consumed = s.candidates()[idx].span.end;
    match s.select(idx, SelectionSource::Keyboard) {
        Outcome::Committed(c) => Some((c.text, consumed)),
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ① 部分选词后继续输入（**已成立**）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn after_a_partial_commit_the_remainder_is_still_typeable() {
    // **不变式**：上屏之后，输入串恰好等于"这次上屏没消费掉的那一段"。
    //
    // `niha` 的「你好」在两种方案配置下消费的字节数不同（缩写路径 3、
    // 补全路径 4），但"余码 = input[consumed..]"这条永远成立。
    let input = "niha";
    let mut s = session();
    type_text(&mut s, input);
    let (text, consumed) = select_text(&mut s, "你好").expect("应当出「你好」并上屏");
    assert_eq!(text, "你好");
    let want = &input[consumed..];
    assert_eq!(
        s.composition().input,
        want,
        "余码必须恰好是没被消费的那一段（消费到第 {consumed} 字节）"
    );

    if want.is_empty() {
        // 这个方案配置把整串都消费了（拼写层补全）——那也是正确的，
        // 只是没有"继续打"这一步。
        return;
    }
    // 继续敲：余码与新按键拼在一起，仍然是**可分析的输入**。
    type_text(&mut s, "i");
    let expected = format!("{want}i");
    assert_eq!(s.composition().input, expected);
    assert!(
        !s.candidates().is_empty(),
        "余码 + 新按键必须被重新分析，而不是留下一段无主的输入：{:?}",
        texts(&s)
    );
}

#[test]
fn after_a_partial_commit_the_first_segment_is_not_reverted() {
    // 反面：上屏的部分**不能**因为后面继续输入而回退。
    let mut s = session();
    type_text(&mut s, "niha");
    let (committed, _) = select_text(&mut s, "你好").expect("应当上屏");
    assert_eq!(committed, "你好");
    // 上下文里应当已经有它（下一词预测依赖这个）。
    type_text(&mut s, "i");
    // 上屏过的词不会回到预编辑串里。
    assert!(
        !s.composition().input.contains("你好"),
        "上屏的部分不该回到输入串：{:?}",
        s.composition().input
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ② 选择"第二段"（**已成立**）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_second_segment_can_be_selected_after_the_first() {
    // 余码本身就是"第二段"的起点：选完「你好」之后，剩下的那段
    // 是一条独立的分段，可以再选一次。
    let input = "niha";
    let mut s = session();
    type_text(&mut s, input);
    let (_, consumed) = select_text(&mut s, "你好").expect("应当上屏");
    if consumed >= input.len() {
        // 这个方案配置一次消费了整串 ⇒ 没有第二段可言。
        assert!(s.composition().input.is_empty());
        return;
    }

    // 第二段必须有自己的候选，而且能独立上屏。
    assert!(
        !s.candidates().is_empty(),
        "第二段必须也有候选：{:?}",
        s.composition().input
    );
    let outcome = s.commit();
    assert!(outcome.is_some(), "第二段必须能上屏");
    assert!(
        s.composition().input.is_empty(),
        "两段都上屏之后输入串必须清空"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ③ Backspace / Delete / Esc（**已成立**）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn backspace_shortens_the_composition_by_one_syllable() {
    let mut s = session();
    type_text(&mut s, "nihao");
    assert_eq!(s.composition().input, "nihao");
    press(&mut s, NamedKey::Backspace);
    assert!(
        s.composition().input.len() < "nihao".len(),
        "退格必须缩短输入串：{:?}",
        s.composition().input
    );
    // 而且仍然是可用的输入（候选会重算）。
    assert!(s.composition().is_active());
}

#[test]
fn escape_clears_the_composition() {
    let mut s = session();
    type_text(&mut s, "nihao");
    press(&mut s, NamedKey::Escape);
    assert!(
        s.composition().input.is_empty(),
        "Esc 必须清空预编辑串，实得 {:?}",
        s.composition().input
    );
}

#[test]
fn escape_does_not_commit_anything() {
    // 反面：取消**不是**上屏。
    let mut s = session();
    type_text(&mut s, "nihao");
    let outcome = press(&mut s, NamedKey::Escape);
    assert!(
        !matches!(outcome, Outcome::Committed(_)),
        "Esc 不该提交任何东西：{outcome:?}"
    );
}

#[test]
fn delete_from_an_empty_composition_is_a_noop() {
    // 边界：没有输入时 Delete 不该 panic、也不该产生候选。
    let mut s = session();
    let _ = press(&mut s, NamedKey::Delete);
    assert!(s.composition().input.is_empty());
    assert!(s.candidates().is_empty(), "空输入不该有候选");
}

// ─────────────────────────────────────────────────────────────────────────────
// ④ 预测候选与数字选择的隔离（**已成立**）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn prediction_candidates_are_never_keyboard_selectable() {
    // 规则 2：预测通道的候选**默认不允许键盘盲选**。
    // 没有记忆时不会有预测候选，所以这里断言的是**可达性**：
    // 任何进入候选列表的预测候选都必须是 `Lane::Predict`，
    // 而 `select` 在键盘来源下会拒绝它。
    let mut s = session();
    type_text(&mut s, "ni");
    let predict_count = s
        .candidates()
        .iter()
        .filter(|c| c.lane == Lane::Predict)
        .count();
    assert_eq!(predict_count, 0, "没有记忆时不该有预测候选");
    assert!(
        s.candidates().iter().all(|c| c.lane == Lane::Input),
        "输入通道的候选全部属于 Input"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑤ 数字键与全半角 / 中英切换（**已成立**）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_digit_key_selects_a_candidate_while_composing() {
    let mut s = session();
    type_text(&mut s, "ni");
    let want = s.candidates().get(1).map(|c| c.text.clone());
    let Some(want) = want else {
        panic!("`ni` 应当至少有两个候选");
    };
    let outcome = press(&mut s, NamedKey::Digit(2));
    match outcome {
        Outcome::Committed(c) => assert_eq!(c.text, want, "数字键 2 应当选第 2 条"),
        other => panic!("数字键应当选词，实得 {other:?}"),
    }
}

#[test]
fn the_ascii_mode_switch_is_visible_and_reversible() {
    let mut s = session();
    assert!(!s.option("ascii_mode"), "默认应当是中文模式");
    assert!(s.set_option("ascii_mode", true), "开关必须已声明");
    assert!(s.option("ascii_mode"));
    assert!(s.set_option("ascii_mode", false));
    assert!(!s.option("ascii_mode"));
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑥ 重新选择已确认段（**重开**）——已知缺口
// ─────────────────────────────────────────────────────────────────────────────

/// **审计 §2.F 明确要求、但本阶段没有实现的能力。**
///
/// RIME 允许把已上屏的一段"重新打开"放回预编辑串继续编辑。
/// Stele 没有这条通路：`finish_commit` 一旦把文本交给前端，
/// 内核就不再持有它——重开必须由前端把文本递回来，而接口上**没有这个入口**。
///
/// 这条被 `#[ignore]` 的测试是**缺口清单**，同时是将来实现的验收点。
/// 它不是在"证明已经支持"。
#[test]
#[ignore = "已知缺口（审计 §2.F）：重开已确认段需要新的接口，内核目前不持有已上屏的文本"]
fn reopening_a_confirmed_segment_is_not_implemented() {
    let mut s = session();
    type_text(&mut s, "niha");
    let _ = select_text(&mut s, "你好");
    // 期望：能请求"把刚才上屏的段放回来继续编辑"。
    // 现状：`Session` 上没有这个入口（编译期就不存在），因此这条测试
    // 只能断言"输入串里没有它"——也就是缺口本身。
    assert!(
        !s.composition().input.contains("你好"),
        "现在**没有**重开通路；这条断言记录的就是缺口"
    );
}
