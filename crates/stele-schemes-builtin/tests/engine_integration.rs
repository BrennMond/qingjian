//! 端到端集成测试：用**同一个引擎**驱动两个完全不同的方案。
//!
//! 这些测试放在方案 crate 而不是引擎 crate 里，是有意的：
//! **引擎不该知道任何具体输入法**（PLAN D20）。测试里出现"拼音"无所谓，
//! 引擎源码里出现就不行——CI 门禁会拦住后者。
//!
//! 这一组测试同时是 **D33 的验收**：同一个引擎必须能跑
//!
//! - 一个**拼写图**方案（拼音：有字母表、有规则、`nihao` 有歧义需要切分）
//! - 一个**精确编码**方案（字形码：单字母单位、无规则、无歧义）
//!
//! 如果哪天有人把引擎写回"拼音专用"，这里的第二个方案会先失败。

use stele_core::{
    Commit, Engine, Event, Key, KeyCode, Lane, Modifiers, NamedKey, Outcome, Session, SpellingAttr,
    Trigger,
};
use stele_engine::EngineImpl;
use stele_schemes_builtin::{all, pinyin_scheme, shape_scheme};

fn engine() -> EngineImpl {
    EngineImpl::new(&[pinyin_scheme(), shape_scheme()]).expect("内置方案应当能编译")
}

/// 敲一串字符（不提交）。
fn type_text(s: &mut Box<dyn Session + Send>, text: &str) {
    for c in text.chars() {
        s.process_key(Key::ch(c));
    }
}

fn press(s: &mut Box<dyn Session + Send>, k: NamedKey) -> Outcome {
    s.process_key(Key::press(KeyCode::Named(k), Modifiers::NONE))
}

fn commit_now(s: &mut Box<dyn Session + Send>) -> Option<Commit> {
    match press(s, NamedKey::Space) {
        Outcome::Committed(c) => Some(c),
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ① 拼写图族：拼音
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn pinyin_types_the_canonical_spelling() {
    let e = engine();
    let mut s = e.create_session(); // 默认第一个方案 = 拼音
    type_text(&mut s, "nihao");

    assert_eq!(s.candidates()[0].text, "你好");
    assert_eq!(s.candidates()[0].origin, stele_core::Origin::SystemWord);
    assert_eq!(s.candidates()[0].attr, SpellingAttr::NORMAL);

    let c = commit_now(&mut s).expect("空格应当上屏");
    assert_eq!(c.text, "你好");
    assert_eq!(c.input, "nihao");
    assert_eq!(c.lane, Lane::Input);
    assert_eq!(c.trigger, Trigger::Space);
    assert!(s.composition().input.is_empty(), "上屏后输入应当清空");
}

#[test]
fn pinyin_types_by_abbreviation() {
    let e = engine();
    let mut s = e.create_session();
    type_text(&mut s, "nh");

    assert_eq!(s.candidates()[0].text, "你好");
    assert!(
        s.candidates()[0].attr.contains(SpellingAttr::ABBREV),
        "变体拼写的命中必须被标记 —— 它既驱动 UI 的「猜测」标记，\
         也决定学习时要不要规范化编码（G9 / G10 / G12）"
    );
    assert_eq!(commit_now(&mut s).map(|c| c.text), Some("你好".to_owned()));
}

#[test]
fn canonical_spelling_outranks_abbreviation_for_the_same_word() {
    let e = engine();

    let mut s1 = e.create_session();
    type_text(&mut s1, "nihao");
    let canonical = s1.candidates()[0].score;

    let mut s2 = e.create_session();
    type_text(&mut s2, "nh");
    let abbrev = s2.candidates()[0].score;

    assert!(
        abbrev < canonical,
        "同一个词经变体拼写命中时分数必须更低（{abbrev} vs {canonical}）——\
         这就是「精确匹配天然排在前面」的全部机制"
    );
}

#[test]
fn every_candidate_is_priceable_and_something_is_always_committable() {
    let e = engine();
    let mut s = e.create_session();
    type_text(&mut s, "zzzz");

    assert!(!s.candidates().is_empty(), "候选永远不该是空的");
    let c = commit_now(&mut s).expect("兜底候选应当能上屏");
    assert_eq!(c.text, "zzzz", "查不到词时，敲什么就上屏什么");
    assert_eq!(c.origin, stele_core::Origin::Literal);
}

// ─────────────────────────────────────────────────────────────────────────────
// ② 精确编码族：字形码（同一个引擎，完全不同的输入法）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn shape_scheme_runs_on_the_same_engine() {
    let e = engine();
    let mut s = e.create_session();
    s.switch_schema("shape-demo").unwrap();

    type_text(&mut s, "ab");
    assert_eq!(s.candidates()[0].text, "十");
    assert_eq!(s.candidates()[0].attr, SpellingAttr::NORMAL);
    assert_eq!(commit_now(&mut s).map(|c| c.text), Some("十".to_owned()));
}

#[test]
fn shape_scheme_has_no_spelling_variants() {
    // 精确编码方案的输入同样是"一条编码"，不经过任何拼写派生 ——
    // 因此它命中的候选**永远是 NORMAL 属性**。
    let e = engine();
    let mut s = e.create_session();
    s.switch_schema("shape-demo").unwrap();

    for keys in ["a", "ab", "abc"] {
        s.reset();
        type_text(&mut s, keys);
        for c in s.candidates() {
            assert_eq!(
                c.attr,
                SpellingAttr::NORMAL,
                "{keys} 的候选 {:?} 不该带变体属性",
                c.text
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ③ 会话行为
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn switching_schema_keeps_the_session_usable() {
    let e = engine();
    let mut s = e.create_session();
    type_text(&mut s, "ni");

    // 切换失败必须保持原方案可用（PLAN D26）。
    assert!(s.switch_schema("no-such-scheme").is_err());
    type_text(&mut s, "hao");
    assert_eq!(s.candidates()[0].text, "你好");
    assert_eq!(s.schema_id(), "pinyin-demo");

    // 切换成功。
    assert!(s.switch_schema("shape-demo").is_ok());
    assert!(s.composition().input.is_empty(), "切换后应当清空输入");
    assert_eq!(s.schema_id(), "shape-demo");
}

#[test]
fn control_keys_are_returned_to_the_os() {
    let e = engine();
    let mut s = e.create_session();
    type_text(&mut s, "ni");
    let ctrl_a = Key::press(KeyCode::Char('a'), Modifiers::CTRL);
    assert!(matches!(s.process_key(ctrl_a), Outcome::Rejected));
    assert_eq!(s.composition().input, "ni", "Ctrl+A 不应改变输入");
}

#[test]
fn backspace_and_escape_edit_the_composition() {
    let e = engine();
    let mut s = e.create_session();
    type_text(&mut s, "nihao");
    press(&mut s, NamedKey::Backspace);
    assert_eq!(s.composition().input, "niha");
    press(&mut s, NamedKey::Escape);
    assert!(s.composition().input.is_empty());
}

#[test]
fn digit_keys_select_by_index() {
    let e = engine();
    let mut s = e.create_session();
    type_text(&mut s, "ni");
    assert!(s.candidates().len() >= 2, "「ni」应当有多个候选");
    let second = s.candidates()[1].text.clone();
    match s.process_key(Key::press(
        KeyCode::Named(NamedKey::Digit(2)),
        Modifiers::NONE,
    )) {
        Outcome::Committed(c) => assert_eq!(c.text, second),
        other => panic!("应当上屏，得到 {other:?}"),
    }
}

#[test]
fn context_accumulates_and_learning_events_carry_the_attr() {
    let e = engine();
    let mut s = e.create_session();

    for word in ["nihao", "zhongguo"] {
        type_text(&mut s, word);
        commit_now(&mut s);
    }

    let mut events = Vec::new();
    s.drain_events(&mut events);
    let learned: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            Event::Learned { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(learned, ["你好", "中国"]);

    // 学习事件必须带上 `attr` —— 少了它，G10（落库前规范化编码）无法实现。
    match &events[0] {
        Event::Learned { attr, .. } => assert_eq!(*attr, SpellingAttr::NORMAL),
        other => panic!("应当是 Learned，得到 {other:?}"),
    }

    // 简拼上屏时属性必须是派生的，接收方据此才知道要规范化。
    s.reset();
    type_text(&mut s, "nh");
    commit_now(&mut s);
    let mut ev = Vec::new();
    s.drain_events(&mut ev);
    match ev.last() {
        Some(Event::Learned { attr, .. }) => {
            assert!(attr.is_derived(), "简拼命中的学习事件必须标记为派生");
        }
        other => panic!("应当有 Learned 事件，得到 {other:?}"),
    }
}

#[test]
fn options_come_from_scheme_data() {
    let e = engine();
    let mut s = e.create_session();
    // `ascii_mode` 由拼音方案声明，所以可设置；引擎不认识它的含义。
    assert!(!s.option("ascii_mode"));
    assert!(s.set_option("ascii_mode", true));
    assert!(s.option("ascii_mode"));
    // 未声明的开关**不会被静默创建** —— 拼错名字要能被发现。
    assert!(!s.set_option("no_such_switch", true));
    assert!(!s.option("no_such_switch"));
}

#[test]
fn ordering_is_reproducible_across_sessions() {
    // 铁律：同一输入连续跑多次，候选序列必须逐字节一致。
    let e = engine();
    let mut reference: Option<Vec<(String, i32)>> = None;
    for _ in 0..50 {
        let mut s = e.create_session();
        type_text(&mut s, "nihao");
        let got: Vec<(String, i32)> = s
            .candidates()
            .iter()
            .map(|c| (c.text.clone(), c.score.as_milli_log()))
            .collect();
        match &reference {
            None => reference = Some(got),
            Some(r) => assert_eq!(r, &got, "候选序列不可复现"),
        }
    }
}

#[test]
fn scheme_catalog_lists_what_it_ships() {
    let e = engine();
    let ids: Vec<&str> = e
        .schemas()
        .list()
        .iter()
        .map(|i| i.schema_id.as_str())
        .collect();
    assert_eq!(ids, ["pinyin-demo", "shape-demo"]);
    assert!(e.schemas().acquire("pinyin-demo").is_ok());
    assert!(e.schemas().acquire("nope").is_err());
    assert_eq!(all().len(), 2);
}
