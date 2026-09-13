//! # 输入中敲标点的语义：一个**有意**与 Rime 不同的决定（审计 §2.F）
//!
//! 审计的观察：
//!
//! > librime 输入 `ni,` 会提交「你，」；当前默认 Stele 路径中逗号返回
//! > `Rejected`，`ni` 仍留在预编辑中。这未必是"代码 bug"——当前标点处理器
//! > 文档已明确选择了另一种交互——但它是与 Rime 不同、且会影响前端设计的
//! > **产品语义差异，需要明确决定并测试**。
//!
//! ## 决定（Stele 的有意选择，不是未实现）
//!
//! **正在拼写时，标点键交还给系统**（`Outcome::Rejected`），预编辑串保持不动。
//!
//! - Rime 的做法是"标点打断编码并一起上屏"（`ni,` → 你，）。
//! - Stele 选择不打断：标点由**前端**决定怎么处理（多数桌面/移动前端会
//!   把它直接送进应用）。理由是内核不该替前端决定"标点要不要吃掉正在输入的编码"，
//!   而这个决定一旦做错，用户会看到"打了一半的词被一个逗号吞了"。
//! - **代价**：前端必须自己实现"标点打断并上屏"这个交互（如果它想要）。
//!   这是明确的接口契约，不是缺口。
//!
//! 本文件把这条语义**钉死**，这样它就不会在将来的重构里被悄悄改掉——
//! 也不会被误读成"还没做"。

use stele_core::{Engine, Key, Outcome, Session};
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

#[test]
fn punctuation_during_composition_is_returned_to_the_system() {
    let mut s = session();
    type_text(&mut s, "ni");
    assert_eq!(s.composition().input, "ni");

    let outcome = s.process_key(Key::ch(','));
    assert!(
        matches!(outcome, Outcome::Rejected),
        "标点必须**交还给系统**（这是 Stele 的有意选择，见本文件文档），实得 {outcome:?}"
    );
    assert_eq!(
        s.composition().input,
        "ni",
        "交还之后预编辑串必须**原样保留**——半截的词不能被一个逗号吞掉"
    );
    assert!(!s.candidates().is_empty(), "候选列表也要保持可用");
}

#[test]
fn the_composition_survives_and_can_still_be_committed() {
    // 标点交还之后，用户仍然可以把这个词打完/选出来。
    let mut s = session();
    type_text(&mut s, "ni");
    let _ = s.process_key(Key::ch(','));
    let outcome = s.commit();
    assert!(outcome.is_some(), "标点之后仍然应当能上屏：{outcome:?}");
}

#[test]
fn with_no_composition_a_keystroke_becomes_a_literal_candidate() {
    // 另一半语义：**没有**正在拼写时，标点进入输入串成为一条原样上屏候选
    // （而不是被拒绝，也不是立刻上屏）。
    //
    // 两半合起来才是完整的设计：
    //
    // | 敲标点时 | 行为 | 理由 |
    // | --- | --- | --- |
    // | 正在拼写（`ni`） | `Rejected` + 输入保持 | 别让一个逗号吞掉半截的词 |
    // | 没有拼写 | `Consumed` + 出现原样候选 | 标点是用户要打的内容 |
    let mut s = session();
    let outcome = s.process_key(Key::ch(','));
    assert!(
        !matches!(outcome, Outcome::Rejected),
        "没有拼写时，标点不该被拒绝：{outcome:?}"
    );
    let cands: Vec<String> = s.candidates().iter().map(|c| c.text.clone()).collect();
    assert!(
        cands.iter().any(|t| t == "，"),
        "标点应当作为原样候选进入输入串：{cands:?}"
    );
    assert_eq!(s.composition().input, "，", "输入串里应当能看到它");
}
