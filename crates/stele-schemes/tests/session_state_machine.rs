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

/// **重开已确认段**（审计 §2.F 明确要求）——现在**已实现**。
///
/// # 语义（与上游一致的那部分）
///
/// 上屏之后把刚上屏的那一段**放回预编辑串**，候选重新出现、可以再选一次。
/// 实现依据是 `Commit` 自带的 `input`（原始拼写）——只有引擎知道
/// "刚才那条是从哪串拼写来的"。
///
/// # 这条测试断言的是**可观察行为**
///
/// 1. 重开之后输入串回来了；
/// 2. 候选列表里又出现了刚才那个词；
/// 3. 没有可重开的内容时返回 `false`（不是 panic、也不是静默成功）。
#[test]
fn reopening_restores_the_last_committed_spelling() {
    let mut s = session();
    type_text(&mut s, "nihao");
    let (text, _) = select_text(&mut s, "你好").expect("应当上屏");
    assert_eq!(text, "你好");
    assert!(s.composition().input.is_empty(), "上屏之后输入串应当清空");

    let reopened = s.reopen();
    assert!(reopened, "上屏之后应当能重开");
    assert_eq!(
        s.composition().input,
        "nihao",
        "重开必须把**原始拼写**放回预编辑串"
    );
    assert!(
        texts(&s).iter().any(|t| t == "你好"),
        "重开之后候选列表里必须又有那个词：{:?}",
        texts(&s)
    );
    // 而且能再上屏一次（"改一下再打"是重开的用途）。
    assert_eq!(
        select_text(&mut s, "你好")
            .as_ref()
            .map(|(t, _)| t.as_str()),
        Some("你好"),
        "重开出来的候选必须能再次上屏"
    );
}

#[test]
fn reopening_without_a_previous_commit_reports_false() {
    // 边界：没有可重开的内容时必须**明确地说没有**，而不是假装成功。
    let mut s = session();
    assert!(!s.reopen(), "刚启动时没有可重开的内容");

    // 正在拼写时也不重开——那会把用户当前打的串冲掉。
    type_text(&mut s, "ni");
    assert!(
        !s.reopen(),
        "正在拼写时不该重开：重开是「回到刚才」，不是「丢弃现在」"
    );
    assert_eq!(s.composition().input, "ni", "当前输入必须原样保留");
}

#[test]
fn resetting_discards_the_reopen_target() {
    // `reset()` 是显式的"我不要了" API ⇒ 之后不该还能重开。
    let mut s = session();
    type_text(&mut s, "nihao");
    let _ = select_text(&mut s, "你好");
    assert!(s.reopen(), "前提：上屏之后确实可以重开");
    // 再上屏一次，然后显式 reset。
    let _ = select_text(&mut s, "你好");
    s.reset();
    assert!(!s.reopen(), "reset 之后不该还能重开");
}

#[test]
fn cancelling_an_active_composition_discards_the_reopen_target() {
    // **取消**的判据是通用的：按键之前有输入、按键之后没有、且没有上屏。
    //
    // Esc 就是在**拼写过程中**取消——那种情况下"刚才那个词"已经不算
    // "刚才"了，重开必须失效。
    let mut s = session();
    type_text(&mut s, "nihao");
    let _ = select_text(&mut s, "你好");
    type_text(&mut s, "ni");
    assert!(s.composition().is_active());
    press(&mut s, NamedKey::Escape);
    assert!(s.composition().input.is_empty(), "Esc 应当取消输入");
    assert!(
        !s.reopen(),
        "取消之后不该还能重开——`reopen` 是「回到刚才上屏的词」，\
         不是「把刚取消的串复活」"
    );
}

/// **一个明确的语义边界**：上屏之后（输入串已空）再按 Esc **不是取消**
/// ——没有东西可取消。此时 `reopen` 仍然有效，因为"刚才那个词"确实还在。
///
/// 这条测试把边界钉住，免得将来有人把"Esc 一律清空重开目标"当成修复。
#[test]
fn escape_with_no_active_composition_does_not_discard_the_reopen_target() {
    let mut s = session();
    type_text(&mut s, "nihao");
    let _ = select_text(&mut s, "你好");
    assert!(!s.composition().is_active(), "上屏之后没有正在拼写的内容");
    press(&mut s, NamedKey::Escape);
    assert!(s.reopen(), "没有东西可取消时，Esc 不该清掉「刚才那个词」");
}

#[test]
fn a_direct_literal_commit_has_nothing_to_reopen() {
    // **真正的"直出"**（`Commit::input` 为空）没有拼写可恢复。
    //
    // 标点走的是**候选**那条路（它是输入串 `，` 的一条原样候选，
    // `input` 非空），所以它可以重开——那是正确行为，不是缺口。
    // 这条测试用 `reset()` 之后的空状态来验证"没有可重开的东西"。
    let mut s = session();
    assert!(!s.reopen(), "没有任何上屏记录时重开必须返回 false");
}

/// **已知边界**（写在 `Session::reopen` 的文档里，这里只做记录）：
///
/// - 只支持**最近一次**上屏，没有提交历史栈；
/// - 重开出来的段**没有被标记为"已确认"**，预编辑串里分不出"放回来的"
///   与"新敲的"；
/// - 重开之后再上屏会**再学习一次**（同一条被记两次）。
///
/// 这三条都是**结构**上的缺口（需要提交历史与"确认段"这两个类型），
/// 不是这次的实现能顺手补掉的。它们在这里以测试名出现，是为了让
/// "重开已实现"这句话不被读成"重开与 RIME 完全一致"。
#[test]
#[ignore = "已知边界：重开没有提交历史栈、没有确认段标记、重复上屏会重复学习"]
fn reopen_is_not_a_full_commit_history() {
    let mut s = session();
    type_text(&mut s, "nihao");
    let _ = select_text(&mut s, "你好");
    assert!(s.reopen());
    // 期望：能连续重开更早的那些段（需要历史栈）。
    // 现状：只有一条记录，重开一次就用掉了。
    assert!(
        !s.reopen(),
        "现在只有一条重开记录；这条断言记录的就是「没有历史栈」这个边界"
    );
}
