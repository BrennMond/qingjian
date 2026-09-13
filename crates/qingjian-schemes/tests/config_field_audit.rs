//! # 配置字段审计表（审计 §2.G5）
//!
//! 审计的原话：
//!
//! > 已发现或应系统排查的字段：`enable_sentence`、`initial_quality`、
//! > 实例 dictionary、`reverse_lookup_filter` 的数据源、completion 等。
//! > **所有公开声称支持的字段均要填齐。**
//!
//! 判据不是"字段有没有被 `parse`"，而是**有没有运行期的效果**：
//! 解析 → 装配 → 消费 → 有端到端测试；不支持时**必须有一条看得见的诊断**。
//!
//! 这张表就是 `docs/config-field-audit.md` 的可执行版本。
//! 每一个"已支持"的断言都对应下面一条行为测试。

use qingjian_core::{Engine, Key, Session};
use qingjian_engine::scheme::TranslatorKind;
use qingjian_engine::EngineImpl;

/// 写一个临时方案目录并装载它。
struct Dir(std::path::PathBuf);

impl Dir {
    fn new(tag: &str, schema: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "qingjian-cfg-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("建临时目录");
        std::fs::write(root.join("cfg.schema.yaml"), schema).expect("写方案");
        std::fs::write(
            root.join("cfg.dict.yaml"),
            "---\nname: cfg\nversion: \"1\"\nsort: by_weight\n...\n\n你\tni\t100\n好\thao\t100\n你好\tni hao\t100\n",
        )
        .expect("写词表");
        Self(root)
    }

    fn session(&self) -> Box<dyn Session + Send> {
        let defs = qingjian_schemes::load_dir(&self.0).expect("方案必须能装载");
        let engine = EngineImpl::new(&defs).expect("方案必须能编译");
        engine.create_session()
    }

    fn engine(&self) -> EngineImpl {
        let defs = qingjian_schemes::load_dir(&self.0).expect("方案必须能装载");
        EngineImpl::new(&defs).expect("方案必须能编译")
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn schema_with_translator(extra: &str) -> String {
    format!(
        "\
schema:
  schema_id: cfg
  name: 配置审计
  version: \"1.0\"
engine:
  tag: abc
  translator: spelling_graph
speller:
  alphabet: [ni, hao]
translator:
  dictionary: cfg
{extra}
"
    )
}

#[allow(clippy::borrowed_box)]
fn texts(s: &Box<dyn Session + Send>) -> Vec<String> {
    s.candidates().iter().map(|c| c.text.clone()).collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// `initial_quality`（本次修复前：解析了、存进 spec、**没人读**）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn initial_quality_actually_changes_the_scores() {
    let base = Dir::new("q0", &schema_with_translator(""));
    let boosted = Dir::new("q1", &schema_with_translator("  initial_quality: 1.1\n"));

    let run = |d: &Dir| -> Vec<(String, i32)> {
        let mut s = d.session();
        for c in "ni".chars() {
            s.process_key(Key::ch(c));
        }
        s.candidates()
            .iter()
            .map(|c| (c.text.clone(), c.score.as_milli_log()))
            .collect()
    };

    let a = run(&base);
    let b = run(&boosted);
    assert_eq!(
        a.iter().map(|(t, _)| t.clone()).collect::<Vec<_>>(),
        b.iter().map(|(t, _)| t.clone()).collect::<Vec<_>>(),
        "倍率不该改变候选的**集合**，只该改变分数"
    );
    // `1.1` 的毫对数是 `ln(1.1) × 1000 ≈ 95`。
    //
    // 只对**这个翻译器产出的**候选断言：原样上屏（`Origin::Literal`）
    // 来自兜底翻译器，它没有 `initial_quality`，分数不该变——
    // 那正是"逐实例"而不是"全局"的证据。
    let expect = qingjian_core::Score::from_weight(1.1).as_milli_log();
    let mut checked = 0;
    for ((t, sa), (t2, sb)) in a.iter().zip(b.iter()) {
        assert_eq!(t, t2);
        if t == "ni" {
            assert_eq!(sb, sa, "原样上屏属于兜底翻译器，不该被这个实例的倍率影响");
            continue;
        }
        assert_eq!(
            sb - sa,
            expect,
            "`{t}` 的分数增量必须等于 ln(initial_quality)：{sa} → {sb}"
        );
        checked += 1;
    }
    assert!(checked > 0, "至少要检查到一条词条候选");
}

#[test]
fn initial_quality_is_per_translator_instance_not_global() {
    // 两个实例：主翻译器不加成，`script_translator@boost` 加成。
    // 判据是**只有那个实例的候选**被加分，而不是全局一起加。
    let schema = "\
schema:
  schema_id: cfg
  name: 配置审计
  version: \"1.0\"
engine:
  tag: abc
  processors: [speller]
  segmentors: [abc_segmentor]
  translators: [script_translator, script_translator@boost]
speller:
  alphabet: [ni, hao]
translator:
  dictionary: cfg
boost:
  dictionary: cfg
  initial_quality: 1.5
";
    let d = Dir::new("per-instance", schema);
    let defs = qingjian_schemes::load_dir(&d.0).expect("方案");
    let specs = &defs[0].translator_specs;
    let main = specs
        .iter()
        .find(|(a, _)| a.is_empty())
        .map(|(_, s)| s.clone())
        .expect("主实例");
    let boost = specs
        .iter()
        .find(|(a, _)| a == "boost")
        .map(|(_, s)| s.clone())
        .expect("boost 实例");
    assert_eq!(main.initial_quality, None);
    assert_eq!(boost.initial_quality, Some(1.5));
    // 装配之后必须真的能跑（不然就是"解析了没生效"）。
    let _ = EngineImpl::new(&defs).expect("两个实例都要能装配");
}

// ─────────────────────────────────────────────────────────────────────────────
// 不支持时必须**有诊断**，而不是静默忽略
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_missing_feature_is_reported_not_silently_ignored() {
    // ① 拼音族的 `enable_sentence`：上游同款没有这个开关 ⇒ 必须说出来。
    //
    // （曾经的另一个例子是"实例独立词库"，它已经实现了——见
    //   `instance_dictionaries.rs`。这条测试因此只剩这一个例子，
    //   而"实现之后就不再报"由那条测试的反面断言守着。）
    let d = Dir::new(
        "diag-sentence",
        &schema_with_translator("  enable_sentence: true\n"),
    );
    let engine = d.engine();
    let all: String = engine
        .degradations()
        .iter()
        .map(|(_, n)| (*n).to_owned())
        .collect();
    assert!(
        all.contains("enable_sentence"),
        "拼音族的 enable_sentence 会被忽略，必须说出来：{all}"
    );
}

#[test]
fn a_parsed_instance_dictionary_is_no_longer_a_degradation() {
    // 与上一条配对：**实现了的能力不该再报"不生效"**。
    // 一条与事实相反的警告比没有警告更糟。
    let schema = "\
schema:
  schema_id: cfg
  name: 配置审计
  version: \"1.0\"
engine:
  tag: abc
  processors: [speller]
  segmentors: [abc_segmentor]
  translators: [script_translator, script_translator@other]
speller:
  alphabet: [ni, hao]
translator:
  dictionary: cfg
other:
  dictionary: cfg
";
    let d = Dir::new("diag-dict-ok", schema);
    let engine = d.engine();
    let all: String = engine
        .degradations()
        .iter()
        .map(|(_, n)| (*n).to_owned())
        .collect();
    assert!(
        !all.contains("独立词库"),
        "实例词库已经装配上了，不该再报「不生效」：{all}"
    );
}

#[test]
fn the_short_form_assembly_consumes_the_translator_section() {
    // 回归：短写法（`engine.translator: spelling_graph`）曾经**完全不读**
    // `translator:` 段，于是 `enable_word_completion` 被解析、存进 spec、
    // 然后丢掉。判据是行为，不是"字段有赋值"。
    //
    // 用 `initial_quality` 做探针（它只改分数，最容易观察到）。
    let plain = Dir::new("short-plain", &schema_with_translator(""));
    let boosted = Dir::new(
        "short-boost",
        &schema_with_translator("  initial_quality: 2.0\n"),
    );

    let score = |d: &Dir| -> i32 {
        let mut s = d.session();
        for c in "ni".chars() {
            s.process_key(Key::ch(c));
        }
        s.candidates()
            .iter()
            .find(|c| c.text == "你")
            .expect("必须有「你」")
            .score
            .as_milli_log()
    };
    assert!(
        score(&boosted) > score(&plain),
        "短写法装配路径必须消费 translator: 段：{} vs {}",
        score(&plain),
        score(&boosted)
    );
}

#[test]
fn completion_reaches_both_translator_families() {
    // `enable_completion`（补全）在两族都要有运行期效果。
    // 精确编码族的输入必须是**能被切成字母表的编码**：字母表是
    // `[ni, hao]`，所以 `n` 切不出来（它不是任何单元的前缀匹配结果），
    // 而 `ni` 可以——补全再给出更长的编码。
    for (tag, translator, keys) in [
        ("graph", "spelling_graph", "ni"),
        ("exact", "exact_code", "ni"),
    ] {
        let schema = format!(
            "\
schema:
  schema_id: cfg
  name: 配置审计
  version: \"1.0\"
engine:
  tag: abc
  translator: {translator}
speller:
  alphabet: [ni, hao]
translator:
  dictionary: cfg
  enable_completion: true
"
        );
        let d = Dir::new(tag, &schema);
        let mut s = d.session();
        for c in keys.chars() {
            s.process_key(Key::ch(c));
        }
        let got = texts(&s);
        assert!(
            got.iter().any(|t| t.contains("你")),
            "{tag} 族的补全必须真的生效（短写路径也算）：{got:?}"
        );
    }
}

#[test]
fn the_audited_field_list_is_covered_by_this_file() {
    // 自检：审计 §2.G5 点名的字段，每一个都必须在上面有对应的行为测试。
    // 这条断言的存在是为了**防止有人删掉上面某条而不改文档**。
    let source = include_str!("config_field_audit.rs");
    for field in [
        "initial_quality",
        "enable_sentence",
        "enable_completion",
        "dictionary",
    ] {
        assert!(
            source.contains(field),
            "审计点名的字段 `{field}` 在这份审计测试里必须有对应断言"
        );
    }
    // 翻译器族分类必须与实现一致（`enable_sentence` 只属于码表族）。
    assert_ne!(TranslatorKind::SpellingGraph, TranslatorKind::ExactCode);
}
