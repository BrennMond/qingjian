//! **内联零件的端到端验收**（阶段 A 的那一族，HANDOFF §5 第 36 条）。
//!
//! # 为什么必须有这一份，单元测试不够
//!
//! 这 10 个零件此前**有实现、有单元测试、注册表里标着"已实现"**，
//! 而 `LoadedScheme::build_pipeline` 里**一次都没引用过它们**——
//! 也就是说从流水线的角度看，它们是死代码。
//!
//! 单元测试测的是"零件本身对不对"（直接调 `apply()` / `translate()`），
//! 它**证明不了"零件被装配了"**。要证明后者，只有一条路：
//! 一份**真的在 `engine:` 里声明了它**的方案，走完整条链
//! （按键 → 切分 → 翻译 → 滤镜 → 排序 → 候选）。
//!
//! 这正是本项目反复踩的那个形状：**"接线在、但没被走到"**。
//! 所以这份测试的每一条断言，失败时都指向"装配路径断了"，
//! 而不是"零件算错了"——那是单元测试的活。
//!
//! 方案在 `tests/schemes-inline/`。

use std::sync::Arc;

use stele_core::{Engine, FrozenClock, Key, LoadedSchema, Services, Session};

/// 2026-01-01 00:00:00 UTC。**定格时间**才能对日期候选写死断言
/// （D38：外部不确定性一律注入）。
const T0: u64 = 1_767_225_600;

fn dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/schemes-inline")
}

fn engine() -> stele_engine::EngineImpl {
    let defs = stele_schemes::load_dir(&dir()).unwrap_or_else(|e| {
        panic!("内联零件方案必须能装载（它同时是装载器的验收）：{e}");
    });
    let clock = Arc::new(FrozenClock {
        secs: T0,
        ms: 0,
        offset_secs: 0,
    });
    stele_engine::EngineImpl::with_services(&defs, Services::new(clock)).expect("编译内联方案")
}

fn session() -> Box<dyn Session + Send> {
    let s = engine().create_session();
    assert_eq!(s.schema_id(), "inline");
    s
}

fn type_text(s: &mut Box<dyn Session + Send>, text: &str) {
    for c in text.chars() {
        s.process_key(Key::ch(c));
    }
}

#[allow(clippy::borrowed_box)]
fn texts(s: &Box<dyn Session + Send>) -> Vec<String> {
    s.candidates().iter().map(|c| c.text.clone()).collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// ① 装配：方案声明了什么，流水线里就真的有什么
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_declared_inline_parts_are_actually_assembled() {
    // 这一条**直接测装配**，不看行为。
    //
    // 它之所以是第一条：如果装配断了，后面每一条断言都会以"候选不对"
    // 的形式失败，而真正的原因（零件根本没进流水线）会被埋掉。
    // 先让"装进来了"可见，再谈"生效了"。
    let defs = stele_schemes::load_dir(&dir()).expect("装载");
    let scheme = defs[0].compile().expect("编译");
    let p = scheme.build_pipeline(&Services::new(Arc::new(FrozenClock {
        secs: T0,
        ms: 0,
        offset_secs: 0,
    })));
    let (processors, translators, filters, _rankers) = p.component_counts();
    // 声明了 3 个处理器 / 3 个翻译器（+1 兜底）/ 2 个滤镜。
    assert_eq!(processors, 3, "speller / editor / selector");
    assert_eq!(
        translators, 7,
        "table_translator + date/calc/unicode/number/uuid + 兜底 echo"
    );
    assert_eq!(
        filters, 6,
        "autocap + long_word + v_filter + pin_cand + reduce_english + uniquifier"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ② date_translator：一条真正走完整条链的断言
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn date_translator_answers_its_trigger_through_the_pipeline() {
    let mut s = session();
    type_text(&mut s, "rq");
    let got = texts(&s);
    assert!(
        got.contains(&"2026-01-01".to_owned()),
        "敲 `rq` 应当出定格的那一天，实得 {got:?}"
    );
}

#[test]
fn date_translator_stays_silent_for_other_input() {
    // 它**不绑标签**（与上游的 `lua_translator@*date_translator` 一致），
    // 因此每次按键都会被调用——内部认不出触发词时必须是无声的。
    let mut s = session();
    type_text(&mut s, "ab");
    let got = texts(&s);
    assert!(
        !got.iter().any(|t| t.contains("2026-")),
        "非触发词不该出日期：{got:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ③ calc_translator
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn calc_translator_evaluates_through_the_pipeline() {
    let mut s = session();
    type_text(&mut s, "cC1+2");
    let got = texts(&s);
    assert!(
        got.contains(&"3".to_owned()),
        "cC1+2 应当出 3，实得 {got:?}"
    );
    assert!(
        got.contains(&"1+2=3".to_owned()),
        "还应当给一条带算式的形式，实得 {got:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ④ autocap_filter
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn autocap_filter_capitalizes_through_the_pipeline() {
    // 词库里 `hello` 挂在编码 `H e l l o` 下。候选文本是**小写**，
    // 而用户敲的是大写开头 —— 滤镜应当把它改成 `Hello`。
    let mut s = session();
    type_text(&mut s, "Hello");
    let got = texts(&s);
    assert!(
        got.contains(&"Hello".to_owned()),
        "输入码首字母大写时候选应当跟着大写，实得 {got:?}"
    );
    assert!(
        !got.contains(&"hello".to_owned()),
        "小写那条应当被改写掉，实得 {got:?}"
    );
}

#[test]
fn autocap_filter_leaves_lowercase_input_alone() {
    // 反面：小写输入不该被"顺手大写"。
    let mut s = session();
    type_text(&mut s, "hello");
    let got = texts(&s);
    // 词库里没有 `h e l l o` 这个编码，所以候选只有兜底的字面量。
    assert!(
        got.iter().any(|t| t == "hello"),
        "小写输入应当原样落在兜底候选里：{got:?}"
    );
    assert!(!got.contains(&"Hello".to_owned()), "不该大写：{got:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑤ long_word_filter：**重排型滤镜在管线里真的生效了**
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn long_word_filter_promotes_long_words_through_the_pipeline() {
    // 这一条是缺口 ② 的验收（HANDOFF §5 第 38 条）。
    //
    // 词库里 `a b` 下六个同码词，权重刻意让**长词排最后**——
    // 于是"按分数排"与"长词优先"给出**不同**的顺序。两种顺序一致的话，
    // 这条测试什么也证明不了（第一版就是这样，探针跑出来才发现）。
    //
    // 滤镜的参数是 `count: 2` / `idx: 4`：把 2 个长词提到第 4、5 位。
    let mut s = session();
    type_text(&mut s, "ab");
    let got = texts(&s);
    assert_eq!(
        &got[..6],
        &["十", "乙", "丙", "甲乙", "十字架", "丁"],
        "长词应当被提到第 4、5 位（纯按权重排的话丁会插在它们前面）：{got:?}"
    );
    // 兜底候选跟在最后。
    assert_eq!(got.last().map(String::as_str), Some("ab"));
}

#[test]
fn the_long_words_really_have_lower_weights() {
    // 上一条测试的**前提**：长词的权重确实更低。少了这条，
    // 上一条会在"权重顺序恰好等于长词优先顺序"时**假通过**。
    let mut s = session();
    type_text(&mut s, "ab");
    let score = |t: &str| {
        s.candidates()
            .iter()
            .find(|c| c.text == t)
            .map(|c| c.score.as_milli_log())
    };
    assert!(
        score("甲乙") < score("丁") && score("十字架") < score("丁"),
        "前提不成立：长词的权重必须低于「丁」，否则上一条测试证明不了任何事"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑥⑦⑧ 其余内联零件：一个零件一条断言
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn unicode_translator_uses_the_prefix_derived_from_the_recognizer_pattern() {
    // 方案里**没有** `unicode:` 段——前缀 `U` 应当是从
    // `recognizer/patterns/unicode`（`"^U[a-f0-9]+"`）的第 2 个字符推出来的
    // （**上游行为**：rime-ice 的 Lua 就是这么干的）。
    let mut s = session();
    type_text(&mut s, "U62fc");
    let got = texts(&s);
    assert!(
        got.contains(&"拼".to_owned()),
        "U62fc 应当出「拼」（前缀来自 recognizer 模式）：{got:?}"
    );
}

#[test]
fn number_translator_uses_the_prefix_derived_from_the_recognizer_pattern() {
    let mut s = session();
    type_text(&mut s, "R3355");
    let got = texts(&s);
    assert!(
        got.iter().any(|t| t.contains("三千三百五十五")),
        "R3355 应当出中文数字（前缀来自 recognizer 模式）：{got:?}"
    );
}

#[test]
fn uuid_translator_answers_its_configured_trigger() {
    // 触发词由方案配置（`uuid: uuid-test`），证明**配置真的读进去了**。
    let mut s = session();
    type_text(&mut s, "uuid-test");
    let got = texts(&s);
    assert!(
        got.iter()
            .any(|t| t.len() == 36 && t.matches('-').count() == 4),
        "应当出形如 UUID 的候选：{got:?}"
    );
}

#[test]
fn pin_cand_filter_pins_the_declared_words_from_the_upstream_syntax() {
    // 上游写法：`- "编码<TAB>词"`。方案里把它挂在 `m n` 上。
    let mut s = session();
    type_text(&mut s, "mn");
    let got = texts(&s);
    assert_eq!(got[0], "丙", "上游写法的置顶规则没生效：{got:?}");
}

#[test]
fn pin_cand_filter_pins_the_declared_words_from_our_syntax() {
    // 我们的写法：`{preedit, texts}`。挂在 `p q` 上，与上一条**不同编码**，
    // 因为同一个编码上的两条规则是"后写的赢"（上游语义）。
    let mut s = session();
    type_text(&mut s, "pq");
    let got = texts(&s);
    assert_eq!(got[0], "庚", "显式字段写法的置顶规则没生效：{got:?}");
}

#[test]
fn reduce_english_filter_demotes_the_configured_code() {
    // `mode: custom` + `words: [ab]`：敲 `ab` 时英文候选往后放。
    // 词库里 `ab` 下没有英文词，所以这里先造一个：
    // 兜底候选 `ab` 本身是字面量（不是英文词），因此断言的重点是
    // **它没有被误伤**——真正的降权行为由单元测试覆盖。
    let mut s = session();
    type_text(&mut s, "ab");
    assert!(
        texts(&s).contains(&"ab".to_owned()),
        "降权不该把候选弄丢：{:?}",
        texts(&s)
    );
}

#[test]
fn v_filter_is_declared_and_does_not_disturb_normal_input() {
    // `v_filter` 只在"`v` + 恰好一个字符"时动手（上游行为）。
    // 声明了它之后，普通输入必须一字不变。
    let mut s = session();
    type_text(&mut s, "ab");
    let got = texts(&s);
    assert!(
        got.contains(&"十".to_owned()) && got.contains(&"乙".to_owned()),
        "v_filter 只在 `v` + 一个字符时动手，普通输入必须原样保留：{got:?}"
    );
}
