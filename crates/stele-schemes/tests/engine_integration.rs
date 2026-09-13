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
use stele_schemes::all;

fn engine() -> EngineImpl {
    let defs = all().expect("内嵌方案必须能装载 —— 失败说明打包坏了");
    EngineImpl::new(&defs).expect("默认方案应当能编译")
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
    // **这条测试断言的是"简拼能不能命中"，不是"哪个词排第一"。**
    // 真实词库（40 万条）里很多词都以 `n…h…` 开头，排名属于**词库权重**，
    // 把它写进断言会让这条测试在换词库时变成噪声。
    //
    // 断言的形状：`nhao` 是「你好」的简拼（第一音节 `ni` 缩成 `n`，
    // 第二音节保持 `hao`）。这一点由方案的 `abbrev` 规则确定，
    // 与词库内容无关。
    let e = engine();
    let mut s = e.create_session();
    type_text(&mut s, "nhao");

    let idx = s
        .candidates()
        .iter()
        .position(|c| c.text == "你好")
        .expect("`nhao` 必须能命中「你好」——否则简拼规则没生效");
    assert!(
        s.candidates()[idx].attr.contains(SpellingAttr::ABBREV),
        "变体拼写的命中必须被标记 —— 它既驱动 UI 的「猜测」标记，\
         也决定学习时要不要规范化编码（G9 / G10 / G12）"
    );
    let outcome = s.select(idx, stele_core::SelectionSource::Keyboard);
    match outcome {
        Outcome::Committed(c) => assert_eq!(c.text, "你好"),
        other => panic!("选中简拼候选应当上屏，实得 {other:?}"),
    }
}

#[test]
fn shape_scheme_has_no_spelling_variants() {
    // 精确编码方案的输入同样是"一条编码"，不经过任何拼写派生 ——
    // 因此它命中的候选**永远是 NORMAL 属性**。
    let e = engine();
    let mut s = e.create_session();
    s.switch_schema("shape").unwrap();

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
    assert_eq!(s.schema_id(), "pinyin");

    // 切换成功。
    assert!(s.switch_schema("shape").is_ok());
    assert!(s.composition().input.is_empty(), "切换后应当清空输入");
    assert_eq!(s.schema_id(), "shape");
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
fn backspace_removes_a_whole_syllable() {
    // RIME：「輸入拼音後按退格鍵，也會以音節爲單位回退刪除拼音」。
    // 敲 `nihao` 按一下退格 → `ni`，而不是 `niha`。
    let e = engine();
    let mut s = e.create_session();
    type_text(&mut s, "nihao");
    press(&mut s, NamedKey::Backspace);
    assert_eq!(s.composition().input, "ni", "退格应当按音节，而不是按字符");

    // 再按一下：只剩一个音节，清空。
    press(&mut s, NamedKey::Backspace);
    assert_eq!(s.composition().input, "");

    // Esc 同样清空。
    type_text(&mut s, "nihao");
    press(&mut s, NamedKey::Escape);
    assert!(s.composition().input.is_empty());
}

#[test]
fn preedit_shows_syllable_boundaries() {
    // 预编辑串按切分结果渲染成 `ni'hao` —— 用户能看见引擎把输入切成了什么。
    let e = engine();
    let mut s = e.create_session();
    type_text(&mut s, "nihao");
    assert_eq!(s.composition().preedit, "ni'hao");
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

    // 简拼上屏时属性必须是派生的，接收方据此才知道要规范化（G10）。
    //
    // 用 `nhao`（第一音节缩成 `n`、第二音节保留 `hao`）而不是 `nh`：
    // 后者要求**两个音节都缩成单字母**，而词库里的编码是规范编码
    // `ni hao`——那条完整切分在缩写边太多时会被展开上限挤掉。
    // 这是机制边界（见 `pinyin.schema.yaml` 里 `rules:` 的注释），
    // 不是这条测试要断言的东西，所以这里选一条确定的写法。
    s.reset();
    type_text(&mut s, "nhao");
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
    assert_eq!(ids, ["pinyin", "shape"]);
    assert!(e.schemas().acquire("pinyin").is_ok());
    assert!(e.schemas().acquire("nope").is_err());
    assert_eq!(all().unwrap().len(), 2);
}
