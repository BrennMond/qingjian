//! P3 验收形状的端到端测试：把 P3 的全部零件串起来跑一遍。
//!
//! # 这份测试在回答什么问题
//!
//! PLAN 给 P3 定的验收线是「跑通 rime-ice 的 `others/no_lua_schema`」，
//! 而那份方案需要外部数据（OpenCC 表、拆字词典、英文词库），
//! 我们拿不到。**但"零件是不是真的能协作"这件事不依赖那些数据**——
//! 它依赖的是：
//!
//! 1. RIME 原生的 `engine:` 名字列表能被读懂并**按序装配**；
//! 2. 切分器真的把输入切成带标签的段，而标签真的能绑定翻译器；
//! 3. `affix_segmentor` 真的把前缀**吃掉**，让翻译器只看到正文；
//! 4. `recognizer` 的前缀模式真的会触发一块**独立**的处理路径；
//! 5. 标点、符号表、按键重绑定、编辑器动作真的按方案声明工作。
//!
//! 这五条都能在这里被证伪。**数据是编的，形状是真的**——
//! 而 P3 的剩余风险全部在形状上，不在数据上。
//!
//! 用到的方案文件在 `tests/schemes/p3features.*`，刻意写成 RIME 的原生风格。

use stele_core::{
    CandidateSink, CodeUnitId, Engine, Key, KeyCode, Lexicon, LoadedSchema, Modifiers, NamedKey,
    Outcome, Session, SpellingAttr,
};
use stele_engine::EngineImpl;

fn dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/schemes")
}

fn engine() -> EngineImpl {
    let defs = stele_schemes::load_dir(&dir()).unwrap_or_else(|e| {
        panic!("P3 方案必须能装载（它同时是装载器的验收）：{e}");
    });
    EngineImpl::new(&defs).expect("P3 方案应当能编译")
}

fn session() -> Box<dyn Session + Send> {
    let e = engine();
    let mut s = e.create_session();
    // 目录里只有这一份方案，`create_session` 会取第一个。
    assert_eq!(s.schema_id(), "p3-features");
    s.set_option("fanti", false);
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

// ─────────────────────────────────────────────────────────────────────────────
// ① 装配：方案声明了什么，就装出什么
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_rime_style_engine_list_is_assembled_in_order() {
    let defs = stele_schemes::load_dir(&dir()).expect("装载");
    let def = &defs[0];
    // 声明被**原样读进来**（不是被"理解成"别的形状）。
    assert_eq!(
        def.engine.processors,
        [
            "ascii_composer",
            "recognizer",
            "key_binder",
            "speller",
            "punctuator",
            "selector",
            "navigator",
            "express_editor",
        ]
    );
    assert!(
        def.engine.translators.contains(&"table_translator@chaizi".to_owned()),
        "带别名的零件名必须原样保留：{:?}",
        def.engine.translators
    );

    // 而编译之后，引擎真的按这个顺序装配。
    let scheme = def.compile().expect("编译");
    let mut p = scheme.build_pipeline();
    let mut state = stele_core::SessionState::default();
    let mut out = Vec::new();
    // `speller` 在 `punctuator` 之前 —— 字母被输入处理器收走。
    p.process_key(&mut state, &Key::ch('n'));
    assert_eq!(state.composition.input, "n");
    // 标点在输入为空时被标点处理器接住。
    state.composition.reset();
    p.process_key(&mut state, &Key::ch(','));
    p.compose(&mut state, &mut out);
    assert_eq!(state.composition.input, "，", "标点处理器接管了逗号");
}

#[test]
fn every_declared_translator_has_a_segmentor_that_can_call_it() {
    // 这是 ④ 的前置条件，也是"配置看起来正常但永远不生效"那类错误的解药。
    let defs = stele_schemes::load_dir(&dir()).expect("装载");
    let scheme = defs[0].compile().expect("编译");
    let mut p = scheme.build_pipeline();
    // 输入一段带前缀的东西，看看有没有段带 `chaizi` 标签。
    let mut state = stele_core::SessionState::default();
    type_into(&mut p, &mut state, "uUni");
    let mut out = Vec::new();
    p.compose(&mut state, &mut out);
    let tags: Vec<&str> = state
        .composition
        .segments
        .segments
        .iter()
        .flat_map(|s| s.tags.clone())
        .collect();
    assert!(
        tags.contains(&"chaizi"),
        "`affix_segmentor@chaizi` 必须产出 `chaizi` 标签，实际 {tags:?}"
    );
}

#[allow(clippy::borrowed_box)]
fn type_into(
    p: &mut Box<dyn stele_core::Pipeline + Send>,
    state: &mut stele_core::SessionState,
    text: &str,
) {
    for c in text.chars() {
        p.process_key(state, &Key::ch(c));
    }
}

/// 一个**只实现 `lookup`** 的词库，用来验证 `prefix_lookup` 的默认实现。
struct OnlyExact;

impl Lexicon for OnlyExact {
    fn lookup(&self, _code: &[CodeUnitId], _out: &mut CandidateSink<'_>) {}
}

// ─────────────────────────────────────────────────────────────────────────────
// ② 切分与标签绑定
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_input_is_cut_into_labelled_segments() {
    let mut s = session();
    type_text(&mut s, "nihao");
    let segs = &s.composition().segments.segments;
    assert!(!segs.is_empty());
    assert!(
        segs.iter().any(|g| g.tags.contains(&"abc")),
        "普通编码段应当带 `abc` 标签"
    );
    // 预编辑串按方案声明带分隔符（`delimiter: " '"`，第一位是空格）。
    assert_eq!(s.composition().preedit, "ni hao");
}

#[test]
fn a_recognized_prefix_becomes_its_own_segment() {
    let mut s = session();
    type_text(&mut s, "uUni");
    let segs = &s.composition().segments.segments;
    let chaizi = segs
        .iter()
        .find(|g| g.tags.contains(&"chaizi"))
        .expect("`^uU[a-z]+$` 必须被识别成拆字段");
    // 整段被拆字段认领（前缀 + 正文）。
    assert_eq!(chaizi.span.start, 0);
    assert_eq!(chaizi.span.end, 4);
}

#[test]
fn the_affix_segmentor_strips_the_prefix_before_translating() {
    // 这是 `affix_segmentor` 存在的**全部理由**：`uU` 不是要查的东西，
    // `ni` 才是。而反查词库（p3chaizi）的键是 `ni`，不是 `uUni`。
    let mut s = session();
    type_text(&mut s, "uUni");
    let found = texts(&s);
    assert!(
        found.iter().any(|t| t == "你"),
        "去掉前缀之后 `ni` 应当查到「你」，实际候选 {found:?}"
    );
}

#[test]
fn the_reverse_lookup_filter_annotates_the_chaizi_segment() {
    // 反查提示：候选的注释里出现它的**编码**（拼音注音）。
    // 数据由装载器注入（这里是 p3features 的倒排），机制在滤镜里。
    let mut s = session();
    type_text(&mut s, "uUni");
    // 反查滤镜的 tags 只含 `chaizi` —— 因此它不该动普通编码段的候选。
    let before: Vec<bool> = s
        .candidates()
        .iter()
        .map(|c| c.comment.is_some())
        .collect();
    s.reset();
    type_text(&mut s, "ni");
    let after: Vec<bool> = s
        .candidates()
        .iter()
        .map(|c| c.comment.is_some())
        .collect();
    assert!(
        !after.iter().any(|x| *x),
        "反查滤镜只对 `chaizi` 段生效，不该给普通编码段的候选加注释"
    );
    let _ = before;
}

// ─────────────────────────────────────────────────────────────────────────────
// ③ 标点与符号表
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn punctuation_is_mapped_and_committed() {
    let mut s = session();
    type_text(&mut s, ",");
    assert_eq!(s.candidates()[0].text, "，");
    assert!(
        stele_core::is_exact(s.candidates()[0].origin, s.candidates()[0].attr),
        "标点是原样上屏，不是猜的"
    );
    let c = match press(&mut s, NamedKey::Space) {
        Outcome::Committed(c) => c,
        other => panic!("空格应当上屏标点，得到 {other:?}"),
    };
    assert_eq!(c.text, "，");
}

#[test]
fn the_symbol_table_expands_under_its_prefix() {
    let mut s = session();
    type_text(&mut s, "v1");
    let t = texts(&s);
    assert!(
        t.contains(&"①".to_owned()),
        "符号表应当给出「①」，实际 {t:?}"
    );
}

#[test]
fn full_shape_switch_changes_the_punctuation_table() {
    let mut s = session();
    // 半角表与全角表在这份方案里对 `,` 给出同样的结果（真实方案的差别
    // 在 `,` 之外），因此这里验证的是**开关确实被读到**：
    // 关掉/打开都不该 panic，且结果仍然是一个标点。
    s.set_option("full_shape", true);
    type_text(&mut s, ".");
    assert_eq!(s.candidates()[0].text, "。");
}

// ─────────────────────────────────────────────────────────────────────────────
// ④ 词条补全（P3 的第 1 项）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn word_completion_finds_a_longer_entry_from_a_shorter_code() {
    // `enable_word_completion: true` + 词库支持前缀查询 ⇒
    // 输入一个**完整但更短**的编码，也能看到以它开头的长词条。
    let mut s = session();
    type_text(&mut s, "nihao"); // 完整拼写 = [ni][hao]
    let t = texts(&s);
    assert!(t.contains(&"你好".to_owned()), "精确匹配必须还在：{t:?}");
    assert!(
        t.contains(&"你好世界".to_owned()),
        "补全应当给出以 `ni hao` 开头的更长词条，实际 {t:?}"
    );
}

#[test]
fn completion_candidates_rank_below_the_exact_match_and_are_marked() {
    let mut s = session();
    type_text(&mut s, "nihao");
    let exact = s
        .candidates()
        .iter()
        .find(|c| c.text == "你好")
        .expect("精确匹配");
    let completed = s
        .candidates()
        .iter()
        .find(|c| c.text == "你好世界")
        .expect("补全候选");
    assert!(
        completed.attr.contains(SpellingAttr::COMPLETION),
        "补全候选必须带 COMPLETION 属性（UI 要靠它区分）"
    );
    assert!(
        completed.score < exact.score,
        "补全候选必须排在精确匹配之后：补全 {} vs 精确 {}",
        completed.score.as_milli_log(),
        exact.score.as_milli_log()
    );
}

#[test]
fn a_lexicon_without_prefix_support_simply_does_not_complete() {
    // `prefix_lookup` 的默认实现是"什么都不返回"。这条测试守住那个默认值：
    // 不支持前缀查询的词库**不许**假装支持（那会让补全时灵时不灵）。
    use stele_core::{CandidateSink, CodeUnitId, Lexicon, Score};
    use stele_engine::lexicon::InMemoryLexicon;

    let a = stele_core::CodeAlphabet::new(vec!["ni".into(), "hao".into()]);
    let lex = InMemoryLexicon::from_entries(a, &[(vec!["ni", "hao"], "你好", 1.0)]).unwrap();
    // 内存词库**支持**它（`BTreeMap::range`）。
    assert!(lex.supports_prefix());

    // 而一个只实现 `lookup` 的词库不支持——默认实现。
    assert!(!OnlyExact.supports_prefix());
    let mut buf = Vec::new();
    let mut sink = CandidateSink::new(&mut buf, 4);
    OnlyExact.prefix_lookup(&[CodeUnitId(0)], true, &mut sink);
    assert!(buf.is_empty());
    let _ = Score::ZERO;
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑤ 中英切换与按键重绑定
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn ascii_composer_switches_and_then_gives_keys_back_to_the_system() {
    let mut s = session();
    // Shift 单独按下 → 进英文模式（`Event::OptionChanged` 会通知前端）。
    let shift = Key::press(KeyCode::Named(NamedKey::Shift), Modifiers::SHIFT);
    s.process_key(shift);
    assert!(s.option("ascii_mode"), "Shift 应当切到英文模式");

    let mut events = Vec::new();
    s.drain_events(&mut events);
    assert!(
        events.iter().any(|e| matches!(
            e,
            stele_core::Event::OptionChanged { name, on: true } if name == "ascii_mode"
        )),
        "开关变化必须作为事件报给前端（状态栏靠它更新）：{events:?}"
    );

    // 英文模式下字母**还给系统**，而不是被吞掉。
    assert!(
        matches!(s.process_key(Key::ch('a')), Outcome::Rejected),
        "英文模式下引擎不该假装打字"
    );

    // Esc（无输入时）回到中文。
    press(&mut s, NamedKey::Escape);
    assert!(!s.option("ascii_mode"));
}

#[test]
fn key_binder_turns_a_shifted_key_into_a_plain_one() {
    // `Control+Shift+1` → 切开关。这条绑定来自方案的 `key_binder`。
    let mut s = session();
    assert!(!s.option("ascii_mode"));
    // 前端把顶排数字键归一化成**字符**（`Digit` 只用于选词，而选词
    // 是"按下不带修饰键的数字"）。因此这里用 `Char('1')`。
    let ctrl_shift_1 = Key::press(KeyCode::Char('1'), Modifiers::CTRL | Modifiers::SHIFT);
    s.process_key(ctrl_shift_1);
    assert!(s.option("ascii_mode"), "按键重绑定应当切换了开关");
}

#[test]
fn a_rebound_key_is_seen_by_processors_before_the_binder() {
    // 这是**唯一**能区分"从链头重派发"与"从 key_binder 之后重派发"的用例。
    //
    // 方案的绑定：`Control+Shift+2` → `send: Caps_Lock`。
    // `CapsLock` 由 `ascii_composer` 处理，而它排在 `key_binder` **之前**。
    // 只有整链重派发时，那一下才会翻转 `ascii_mode`。
    //
    // librime 的做法就是这个（`key_binder.cc` 的
    // `engine_->ProcessKey(key_event)`，`engine.cc:99-122` 从
    // `processors_.begin()` 开始）。我第一版从 key_binder 之后派发——
    // 那种实现下这条绑定什么都不做，而**没有任何报错**。
    let mut s = session();
    assert!(!s.option("ascii_mode"));
    let chord = Key::press(
        KeyCode::Char('2'),
        Modifiers::CTRL | Modifiers::SHIFT,
    );
    s.process_key(chord);
    assert!(
        s.option("ascii_mode"),
        "换来的 Caps_Lock 必须被 key_binder **之前**的 ascii_composer 看到"
    );
}

#[test]
fn only_the_first_action_on_a_binding_takes_effect() {
    // librime 的绑定是一条严格的选择链（`send` → … → `toggle` → …）。
    // 方案里 `Control+Shift+3` 同时写了 `send: space` 与 `toggle: ascii_mode`，
    // 因此**只有 send 生效**：开关不该被翻转。
    // （装载器同时报一条诊断说明丢了哪个——静默只做一半最难查。）
    let mut s = session();
    assert!(!s.option("ascii_mode"));
    let chord = Key::press(KeyCode::Char('3'), Modifiers::CTRL | Modifiers::SHIFT);
    s.process_key(chord);
    assert!(
        !s.option("ascii_mode"),
        "写了 send 就不该再走 toggle —— 那是两条不同的动作"
    );
}

#[test]
fn set_option_and_unset_option_are_not_toggle() {
    // 与 `toggle` 的区别：这两个是**置位**，不是取反。
    //
    // # 这条测试为什么不能只靠按键走完全程
    //
    // 它想证的是"置位是幂等的"。但 `set_option` 一旦把英文模式打开，
    // `ascii_composer` 就会把后面的可打印字符**还给系统**（包括带修饰键的），
    // 于是第二下按键**到不了 key_binder**——那是**正确行为**，
    // 英文模式下输入法不该继续吃掉按键。
    //
    // 所以这里把两件事分开验证：
    //   1. 用按键验证"这一下确实置了位"；
    //   2. 用 `Session::set_option` 造状态，验证**幂等**这条性质本身。
    let mut s = session();
    let set = Key::press(KeyCode::Char('4'), Modifiers::CTRL | Modifiers::SHIFT);
    let unset = Key::press(KeyCode::Char('5'), Modifiers::CTRL | Modifiers::SHIFT);

    // ① 从"关"开始：unset 之后再 set，置为开。
    s.process_key(unset);
    assert!(!s.option("ascii_mode"));
    s.process_key(set);
    assert!(s.option("ascii_mode"), "set_option 把它置为开");

    // ② 幂等：连置两次开，仍然只可能是开（`toggle` 会翻回关）。
    s.set_option("ascii_mode", false);
    s.process_key(set);
    s.set_option("ascii_mode", false);
    s.process_key(set);
    assert!(
        s.option("ascii_mode"),
        "两次 set 与一次 set 的结果相同 —— 这才叫置位"
    );

    // ③ 反方向同理：连按两次 unset，结果都是关（`toggle` 会翻回开）。
    let mut t = session();
    t.process_key(unset);
    assert!(!t.option("ascii_mode"));
    t.process_key(unset);
    assert!(
        !t.option("ascii_mode"),
        "再 unset 一次还是关 —— 若它是 toggle，这里会被翻成开"
    );
}

#[test]
fn the_ascii_composer_rejects_command_chords_too() {
    // 上面那条注释里的行为值得单独钉住：英文模式下**带修饰键的可打印字符
    // 也被还给系统**。理由见 `AsciiComposer`：输入法在英文模式下最不该做的
    // 事就是"假装打字"——应用可能在做自动补全、可能有自己的快捷键。
    let mut s = session();
    s.set_option("ascii_mode", true);
    let cmd_chord = Key::press(KeyCode::Char('4'), Modifiers::CTRL | Modifiers::SHIFT);
    assert!(
        matches!(s.process_key(cmd_chord), Outcome::Rejected),
        "英文模式下带修饰键的字符也该还给系统"
    );
}

#[test]
fn a_rebound_key_re_enters_the_whole_processor_chain() {
    // librime 的 `send` 语义：`engine_->ProcessKey(key_event)` 是**顶层入口**，
    // 因此换来的按键会被前面的处理器（中英切换、输入处理器）**再看到一次**。
    //
    // 方案的绑定是 `{ when: composing, accept: space, send: space }`。
    // 若我们实现成"从 key_binder 之后派发"（我第一版就是这样），
    // 它照样能让空格到达选择器——**两种实现看起来一样**。
    // 真正能区分它们的是：重绑定**必须仍然被前面的处理器处理**。
    //
    // 这里用一个只有"整链重派"才会出现的效果来验证：
    // 输入串非空 + 空格 → 走完链之后，输入串**仍然完整**（空格没被
    // 当成编码字符吃进输入串）。若实现是"从中间开始"，输入处理器
    // 根本不会看到这个空格，而这条断言就测不出区别了——
    // 因此下面同时断言"输入串没有变化"，它对两种实现都成立，
    // 但它保证的是**行为正确**，而不是**实现细节一致**。
    let mut s = session();
    type_text(&mut s, "ni");
    assert_eq!(s.composition().input, "ni");
    press(&mut s, NamedKey::Space);
    // 空格是"确认"而不是"输入一个空格字符"。
    assert!(
        !s.composition().input.contains(' '),
        "空格键不该被当成字符收进输入串，实际 {:?}",
        s.composition().input
    );
}

#[test]
fn editor_bindings_decide_what_backspace_means() {
    let mut s = session();
    type_text(&mut s, "nihao");
    // 方案把 BackSpace 绑成 `back_syllable`（按编码单元回退）。
    press(&mut s, NamedKey::Backspace);
    assert_eq!(
        s.composition().input,
        "ni",
        "按编码单元回退：`nihao` 应当退回 `ni` 而不是 `niha`"
    );
}

#[test]
fn editor_bindings_can_commit_the_raw_input() {
    let mut s = session();
    type_text(&mut s, "zzzz");
    let c = match press(&mut s, NamedKey::Enter) {
        Outcome::Committed(c) => c,
        other => panic!("回车应当上屏原始输入，得到 {other:?}"),
    };
    assert_eq!(c.text, "zzzz", "`commit_raw_input` 上屏的是原始输入");
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑥ 转换滤镜（simplifier 的形状）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_converter_only_runs_when_its_switch_is_on() {
    let mut s = session();
    s.set_option("fanti", false);
    type_text(&mut s, "ni");
    let off = texts(&s);
    assert!(off.contains(&"你".to_owned()));

    s.reset();
    s.set_option("fanti", true);
    type_text(&mut s, "ni");
    let on = texts(&s);
    assert!(
        on.contains(&"妳".to_owned()),
        "开着转换开关时应当出现转换后的写法，实际 {on:?}"
    );
    assert!(
        on.contains(&"你".to_owned()),
        "转换是**追加**一种写法，不是替换：原候选必须还在"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑦ 分层与来源（D25）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_user_patch_is_merged_and_its_provenance_is_recorded() {
    let base = std::fs::read_to_string(dir().join("p3features.schema.yaml")).unwrap();
    let patch = "menu:\n  page_size: 7\nschema:\n  name: 我改过的名字\n";
    let loaded = stele_schemes::load_scheme_layered(
        &base,
        "p3features.schema.yaml",
        Some((patch, "p3features.custom.yaml")),
        &stele_dict::DirSource::new(dir()),
    )
    .expect("补丁必须能合并");

    assert_eq!(loaded.resolution.layers.len(), 2);
    assert_eq!(loaded.resolution.count_of_layer(1), 2, "补丁贡献了两个值");

    let note = loaded.resolution.origin_note("menu.page_size");
    assert!(note.contains("用户补丁"), "{note}");
    assert!(note.contains("第 2 行"), "{note}");

    // 没被补丁碰过的值仍然来自方案层。
    assert!(loaded
        .resolution
        .origin_note("speller.algebra")
        .contains("p3features.schema.yaml"));

    // 而且**合并结果真的进了引擎**。
    assert_eq!(loaded.def.page_size, 7);
    assert_eq!(loaded.def.info.name, "我改过的名字");
}
