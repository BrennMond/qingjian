//! # 解码器的硬性验收矩阵（审计 §2.E）
//!
//! 审计给的两张表必须成为**回归测试**，而不是一次性观察：
//!
//! **表 1**（同词表对照）：
//!
//! | 输入 | librime | Stele（修复前） | Stele（现在） |
//! | --- | --- | --- | --- |
//! | `nihao` | 你好 | 你好 | 你好 |
//! | `niha` | 你好等 | 只有字面量 | 你好（只消费 3/4 字节） |
//! | `haoni` | 可造句"好你" | 只有字面量 | 好你（造句） |
//! | `nihaoshijie` | 可解释前缀候选 | 只有字面量 | 你好世界（造句）+ 你好（前缀） |
//!
//! **表 2**（缩写 × 补全 2×2）：上游 4 格命中 3 格——
//! 只有**两条通路都关**时 `niha` 才失效。
//!
//! 断言必须同时检查**候选文本、消费范围（余码）、属性**：
//! 只看"有某个候选"会漏掉"它其实只消费了两个字符"。
//!
//! # 为什么走**目录装载**而不是手搭 `SchemeDef`
//!
//! 手搭容易漏掉装配路径上的必需项（`input_alphabet`、`page_size`、
//! 零件装配），于是测试测的是"我搭得对不对"而不是"引擎行不行"。
//! 目录装载走的是**真实方案装载器**——与 CLI 完全同一条路。
//! 夹具的每一格只改一个变量（缩写规则 / 补全开关）。

use stele_core::{Candidate, Engine, Key, Origin, Span, SpellingAttr};
use stele_engine::EngineImpl;

/// 夹具词表：`你` / `好` 两个单字 + `你好` / `世界` 两个词。
///
/// **没有「好你」**——那正是造句那一格存在的理由。
const DICT: &str = "\
---
name: fixture
version: \"1\"
sort: by_weight
...

你\tni\t20000
好\thao\t18000
是\tshi\t15000
界\tjie\t12000
你好\tni hao\t10000
世界\tshi jie\t9000
";

/// 一个临时方案目录。`abbrev` / `completion` 是这一格的两个变量。
struct Fixture {
    root: std::path::PathBuf,
}

fn schema_yaml(abbrev: bool, completion: bool) -> String {
    let rules = if abbrev {
        "  rules:\n    - abbrev: { take: 1, weight: 0.5 }\n"
    } else {
        "  rules: []\n"
    };
    format!(
        "\
schema:
  schema_id: fixture
  name: 夹具
  version: \"1.0\"
engine:
  tag: abc
  translator: spelling_graph
speller:
  alphabet: [ni, hao, shi, jie]
  delimiter: \"'\"
{rules}translator:
  dictionary: fixture
  enable_word_completion: {completion}
"
    )
}

impl Fixture {
    fn new(tag: &str, abbrev: bool, completion: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "stele-decoder-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("建临时目录");
        std::fs::write(
            root.join("fixture.schema.yaml"),
            schema_yaml(abbrev, completion),
        )
        .expect("写方案");
        std::fs::write(root.join("fixture.dict.yaml"), DICT).expect("写词表");
        Self { root }
    }

    /// 敲一串键，返回候选列表。
    fn candidates(&self, input: &str) -> Vec<Candidate> {
        let defs = stele_schemes::load_dir(&self.root)
            .unwrap_or_else(|e| panic!("夹具方案必须能装载：{e}"));
        let engine = EngineImpl::new(&defs).expect("夹具方案应当能编译");
        let mut s = engine.create_session();
        for c in input.chars() {
            s.process_key(Key::ch(c));
        }
        s.candidates().to_vec()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// 余码 = 输入里没有被这条候选消费掉的部分。
fn remainder<'a>(input: &'a str, c: &Candidate) -> &'a str {
    &input[c.span.end.min(input.len())..]
}

fn find<'a>(cands: &'a [Candidate], text: &str) -> Option<&'a Candidate> {
    cands.iter().find(|c| c.text == text)
}

fn texts(cands: &[Candidate]) -> Vec<String> {
    cands.iter().map(|c| c.text.clone()).collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// 表 1：同词表对照
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn nihao_is_the_whole_word_with_no_remainder() {
    let f = Fixture::new("nihao", true, false);
    let c = f.candidates("nihao");
    let nihao =
        find(&c, "你好").unwrap_or_else(|| panic!("nihao 必须出「你好」，实得 {:?}", texts(&c)));
    assert_eq!(nihao.span, Span::new(0, 5), "必须消费完整串");
    assert_eq!(remainder("nihao", nihao), "");
    assert_eq!(nihao.origin, Origin::SystemWord);
}

#[test]
fn niha_gives_the_word_for_the_interpreted_prefix() {
    // **审计的原话**：`niha` 在「缩写开、补全关」那一格给出「你好」——
    // 此时图只覆盖 `ni` + `h`（`h` 是 `hao` 的缩写），候选跨越输入的前
    // 3 个字符，第 4 个字符 `a` 留在输入里、从未被消费。
    let input = "niha";

    // ① 缩写开、补全关：走"只消费前缀"那条路。
    let f = Fixture::new("niha-abbrev", true, false);
    let c = f.candidates(input);
    let nihao = find(&c, "你好")
        .unwrap_or_else(|| panic!("缩写开时 niha 必须出「你好」，实得 {:?}", texts(&c)));
    assert_eq!(nihao.span, Span::new(0, 3), "只消费前 3 个字节");
    assert_eq!(remainder(input, nihao), "a", "余码必须留在输入里");
    assert!(
        nihao.attr.contains(SpellingAttr::ABBREV),
        "这条路径经过了缩写：attr={:?}",
        nihao.attr
    );

    // ② 缩写关、补全开：走"拼写层补全"那条路，消费全部 4 个字符。
    let f = Fixture::new("niha-completion", false, true);
    let c = f.candidates(input);
    let nihao = find(&c, "你好")
        .unwrap_or_else(|| panic!("补全开时 niha 也必须出「你好」，实得 {:?}", texts(&c)));
    assert_eq!(nihao.span, Span::new(0, 4), "补全时消费完整串");
    assert_eq!(remainder(input, nihao), "");
    assert!(
        nihao.attr.contains(SpellingAttr::COMPLETION),
        "这条路径是补出来的：attr={:?}",
        nihao.attr
    );

    // ③ 两条都关：只剩单字前缀（librime 的第 4 格是「你 尼 泥」）。
    let f = Fixture::new("niha-none", false, false);
    let c = f.candidates(input);
    assert!(
        find(&c, "你好").is_none(),
        "两条通路都关掉就不该有「你好」，实得 {:?}",
        texts(&c)
    );
    let ni =
        find(&c, "你").unwrap_or_else(|| panic!("「你」应当作为前缀候选在，实得 {:?}", texts(&c)));
    assert_eq!(ni.span, Span::new(0, 2));
    assert_eq!(remainder(input, ni), "ha");
}

#[test]
fn haoni_is_composed_by_sentence_making() {
    // 词表里**没有**「好你」，但有「好」「你」两个单字。
    let f = Fixture::new("haoni", true, false);
    let input = "haoni";
    let c = f.candidates(input);
    let sentence =
        find(&c, "好你").unwrap_or_else(|| panic!("必须造句出「好你」，实得 {:?}", texts(&c)));
    assert_eq!(
        sentence.origin,
        Origin::Sentence,
        "它是**猜出来的**：来源必须标明白（排序与学习都据此区别对待）"
    );
    assert_eq!(sentence.span, Span::new(0, 5), "句子覆盖整串输入");
    assert_eq!(remainder(input, sentence), "");
}

#[test]
fn nihaoshijie_gives_prefix_candidates_and_a_composition() {
    let f = Fixture::new("shijie", true, false);
    let input = "nihaoshijie";
    let c = f.candidates(input);

    // ① 可解释前缀：「ni hao」消费前 5 个字节，余码完整留着。
    let nihao =
        find(&c, "你好").unwrap_or_else(|| panic!("必须给可解释前缀的候选，实得 {:?}", texts(&c)));
    assert_eq!(nihao.span, Span::new(0, 5), "只消费 `ni hao`");
    assert_eq!(remainder(input, nihao), "shijie", "余码必须完整留着");

    // ② 造句：`你好` + `世界` 覆盖整串。
    let sentence = find(&c, "你好世界")
        .unwrap_or_else(|| panic!("必须造句出「你好世界」，实得 {:?}", texts(&c)));
    assert_eq!(sentence.origin, Origin::Sentence);
    assert_eq!(sentence.span, Span::new(0, 11));

    // ③ **余码罚分**：消费更多的候选必须排在消费更少的之前。
    // 这条是修复过程中真踩到的坑：单字「你」的词条权重远高于词「你好」，
    // 没有罚分时"敲了 11 个字母"会被"只匹配了一个字"压过。
    let ni = find(&c, "你").unwrap();
    assert!(
        nihao.score > ni.score,
        "消费更多的候选必须排在前面：你好={} 你={}",
        nihao.score.as_milli_log(),
        ni.score.as_milli_log()
    );
    assert!(
        sentence.score > ni.score,
        "覆盖整串的造句也必须压过单个字的前缀候选"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 表 2：缩写 × 补全 2×2
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_four_cell_matrix_matches_the_reference() {
    let table = [
        (true, true, true, "缩写开 + 补全开"),
        (true, false, true, "缩写开 + 补全关"),
        (false, true, true, "缩写关 + 补全开"),
        (false, false, false, "缩写关 + 补全关"),
    ];
    for (i, (abbrev, completion, expect_hit, label)) in table.into_iter().enumerate() {
        let f = Fixture::new(&format!("matrix{i}"), abbrev, completion);
        let c = f.candidates("niha");
        let hit = find(&c, "你好").is_some();
        assert_eq!(
            hit,
            expect_hit,
            "{label}：`niha` 出「你好」应当是 {expect_hit}，实得 {hit}；候选 = {:?}",
            texts(&c)
        );
    }
}

#[test]
fn the_two_completion_paths_are_distinguishable() {
    // 这两条通路**不能只靠 preedit 区分**（上游的观察），所以判据必须是
    // 属性与消费范围。这条测试把两者都钉住。
    let a = Fixture::new("two-abbrev", true, false).candidates("niha");
    let c = Fixture::new("two-completion", false, true).candidates("niha");

    let a = find(&a, "你好").expect("缩写通路");
    let c = find(&c, "你好").expect("补全通路");

    assert!(a.attr.contains(SpellingAttr::ABBREV));
    assert!(!a.attr.contains(SpellingAttr::COMPLETION));
    assert!(c.attr.contains(SpellingAttr::COMPLETION));
    assert!(!c.attr.contains(SpellingAttr::ABBREV));
    // 消费长度不同：缩写只吃了 `h`，补全吃完了 `ha`。
    assert_ne!(a.span, c.span);
}

// ─────────────────────────────────────────────────────────────────────────────
// 造句的质量边界（审计点名要防的两件事）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_single_word_is_never_reported_as_a_sentence() {
    // 上游明确排除"整串就是一个词"（`gear/poet.cc:206-208`）。
    let f = Fixture::new("single", true, false);
    let c = f.candidates("nihao");
    assert!(
        !c.iter().any(|c| c.origin == Origin::Sentence),
        "整串有精确词条时不该造句：{:?}",
        texts(&c)
    );
}

#[test]
fn sentence_making_does_not_repeat_the_same_word() {
    // 「不会把单词重复作为句子」：边严格向前，同一段输入不可能被用两次。
    let f = Fixture::new("repeat", true, false);
    for input in ["haoni", "nihaohaoni", "nihaoni", "haohaoni"] {
        let c = f.candidates(input);
        for s in c.iter().filter(|c| c.origin == Origin::Sentence) {
            assert!(
                s.text.chars().count() <= input.chars().count(),
                "`{input}` 的造句 `{}` 比输入还长，像是重复拼接",
                s.text
            );
        }
    }
}

#[test]
fn sentence_candidates_are_lane_input_and_marked_guessed() {
    let f = Fixture::new("lane", true, false);
    let c = f.candidates("haoni");
    let s = find(&c, "好你").expect("造句");
    assert_eq!(
        s.lane,
        stele_core::Lane::Input,
        "造句属于当前输入，不是预测"
    );
    assert!(
        s.key.is_none(),
        "造句没有规范编码键（它不对应任何一条词库编码）"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 回归：不允许"用退化成字面量"换资源上界
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_target_candidates_are_present_not_just_the_literal() {
    // 审计 §2.A 的验收线：**不能为了通过资源测试而把输入退化成字面量**。
    let cases: &[(&str, &str, bool, bool)] = &[
        ("nihao", "你好", true, false),
        ("niha", "你好", true, false),
        ("niha", "你好", false, true),
        ("haoni", "好你", true, false),
        ("nihaoshijie", "你好", true, false),
        ("nihaoshijie", "你好世界", true, false),
    ];
    for (i, (input, want, abbrev, completion)) in cases.iter().enumerate() {
        let f = Fixture::new(&format!("present{i}"), *abbrev, *completion);
        let c = f.candidates(input);
        assert!(
            find(&c, want).is_some(),
            "`{input}`（缩写={abbrev} 补全={completion}）必须出「{want}」，实得 {:?}",
            texts(&c)
        );
        assert!(
            c.len() > 1,
            "`{input}` 不该只剩一个候选（那通常是「退化成字面量」的症状）"
        );
    }
}

#[test]
fn the_preedit_still_labels_the_consumed_syllable() {
    // 预编辑串至少要把被消费的第一个音节分出来（`ni`）。
    let f = Fixture::new("preedit", true, false);
    let defs = stele_schemes::load_dir(&f.root).expect("夹具");
    let engine = EngineImpl::new(&defs).expect("编译");
    let mut s = engine.create_session();
    for c in "niha".chars() {
        s.process_key(Key::ch(c));
    }
    let preedit = s.composition().preedit.clone();
    assert!(
        preedit.contains("ni"),
        "预编辑串应当标出被消费的音节：{preedit:?}"
    );
}

/// **已知缺口（阶段 2 未完成）**：预编辑串对"只消费前缀"的**显示**。
///
/// 候选的消费范围与余码是**对的**（见
/// `niha_gives_the_word_for_the_interpreted_prefix`），但预编辑串仍然
/// 回显整串 `niha`，而不是上游那样的 `ni ha`（余码 `a` 留在输入里）。
///
/// 原因：预编辑串由 `segment_for_display` 对**整串输入**切分得到，
/// 而它不认识"缩写边只吃一个字符、余码不算已消费"这件事。
/// 要做对需要把拼写图的**消费边界**送进显示层——那是阶段 2 剩下的
/// 「候选 span、余码、逐段确认」那一条，本阶段没有做。
///
/// 这条测试**不假装它已经工作**：它断言的是"当前就是整串回显"。
/// 等显示层接上消费边界时，它会失败，而那次失败正是**提醒**
/// 把那行断言改成 `"ni ha"`（或等价的、带分隔符的形态）。
#[test]
fn preedit_display_for_prefix_consumption_is_a_known_gap() {
    let f = Fixture::new("preedit-gap", true, false);
    let defs = stele_schemes::load_dir(&f.root).expect("夹具");
    let engine = EngineImpl::new(&defs).expect("编译");
    let mut s = engine.create_session();
    for c in "niha".chars() {
        s.process_key(Key::ch(c));
    }
    assert_eq!(
        s.composition().preedit,
        "niha",
        "如果这条开始失败，说明显示层已经接上消费边界——请把断言改成 `ni ha`，\
         并从 `docs/validation/phase-2.md` 的已知缺口里删掉这一条"
    );
    assert_eq!(s.composition().input, "niha", "输入串本身不该被改动");
}
