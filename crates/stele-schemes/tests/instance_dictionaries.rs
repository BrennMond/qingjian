//! # 多 translator 实例的**独立词库**（审计 §2.G3）
//!
//! 审计的复现：
//!
//! > 审计构造两个 `script_translator`，主词库含「甲」、
//! > `script_translator@other` 词库含「乙」，输入同码仅得到「甲」。
//! > 装配路径持续使用 `self.lexicon`，并未按实例
//! > `TranslatorSpec.dictionary` 选择词库。
//!
//! 这一组测试直接照那个复现写：**同码、两个实例、两本词库**，
//! 两边的词都必须出现，而且**各自带上自己那本词库的候选**。
//!
//! `table_translator@alias`（精确编码族）走同一套装配，另外测一遍。

use stele_core::{Engine, Key};
use stele_engine::EngineImpl;

/// 主词库只有「甲」（编码 `ni`），`other` 词库只有「乙」（同码 `ni`）。
///
/// **同码**是关键：两个实例都处理同一个输入段，只有词库不同。
/// 如果装配路径用同一本词库，就会只看到一边的词。
const MAIN_DICT: &str = "\
---
name: main
version: \"1\"
sort: by_weight
...

甲\tni\t1000
";

const OTHER_DICT: &str = "\
---
name: other
version: \"1\"
sort: by_weight
...

乙\tni\t1000
";

struct Dir(std::path::PathBuf);

impl Dir {
    fn new(tag: &str, schema: &str, dicts: &[(&str, &str)]) -> Self {
        let root = std::env::temp_dir().join(format!(
            "stele-g3-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("建临时目录");
        std::fs::write(root.join("g3.schema.yaml"), schema).expect("写方案");
        for (name, text) in dicts {
            std::fs::write(root.join(name), text).expect("写词库");
        }
        Self(root)
    }

    fn candidates(&self, keys: &str) -> Vec<String> {
        let defs =
            stele_schemes::load_dir(&self.0).unwrap_or_else(|e| panic!("方案必须能装载：{e}"));
        let engine = EngineImpl::new(&defs).expect("方案必须能编译");
        let mut s = engine.create_session();
        for c in keys.chars() {
            s.process_key(Key::ch(c));
        }
        s.candidates().iter().map(|c| c.text.clone()).collect()
    }

    fn degradations(&self) -> String {
        let defs = stele_schemes::load_dir(&self.0).expect("装载");
        let engine = EngineImpl::new(&defs).expect("编译");
        engine
            .degradations()
            .iter()
            .map(|(_, n)| (*n).to_owned())
            .collect()
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn script_schema() -> String {
    "\
schema:
  schema_id: g3
  name: 实例词库
  version: \"1.0\"
engine:
  tag: abc
  processors: [speller]
  segmentors: [abc_segmentor]
  translators: [script_translator, script_translator@other]
speller:
  alphabet: [ni, hao]
translator:
  dictionary: main
other:
  dictionary: other
"
    .to_owned()
}

fn table_schema() -> String {
    "\
schema:
  schema_id: g3
  name: 实例词库（精确编码）
  version: \"1.0\"
engine:
  tag: abc
  processors: [speller]
  segmentors: [abc_segmentor]
  translators: [table_translator, table_translator@other]
speller:
  alphabet: [ni, hao]
translator:
  dictionary: main
other:
  dictionary: other
"
    .to_owned()
}

#[test]
fn two_script_translator_instances_use_their_own_dictionaries() {
    // 审计的原样复现：同码，两个实例，两本词库。
    let dir = Dir::new(
        "script",
        &script_schema(),
        &[
            ("main.dict.yaml", MAIN_DICT),
            ("other.dict.yaml", OTHER_DICT),
        ],
    );
    let got = dir.candidates("ni");
    assert!(
        got.iter().any(|t| t == "甲"),
        "主词库的「甲」必须在：{got:?}"
    );
    assert!(
        got.iter().any(|t| t == "乙"),
        "`script_translator@other` 的「乙」也必须在——这正是审计点名的缺口：{got:?}"
    );
}

#[test]
fn two_table_translator_instances_use_their_own_dictionaries() {
    let dir = Dir::new(
        "table",
        &table_schema(),
        &[
            ("main.dict.yaml", MAIN_DICT),
            ("other.dict.yaml", OTHER_DICT),
        ],
    );
    let got = dir.candidates("ni");
    assert!(
        got.iter().any(|t| t == "甲"),
        "主词库的「甲」必须在：{got:?}"
    );
    assert!(
        got.iter().any(|t| t == "乙"),
        "`table_translator@other` 的「乙」也必须在：{got:?}"
    );
}

#[test]
fn an_instance_without_its_own_dictionary_falls_back_to_the_main_one() {
    // RIME 的默认行为：实例不写 `dictionary` 就继承方案级的那本。
    let schema = "\
schema:
  schema_id: g3
  name: 实例词库
  version: \"1.0\"
engine:
  tag: abc
  processors: [speller]
  segmentors: [abc_segmentor]
  translators: [script_translator, script_translator@other]
speller:
  alphabet: [ni, hao]
translator:
  dictionary: main
other:
  enable_completion: false
";
    let dir = Dir::new(
        "fallback",
        schema,
        &[
            ("main.dict.yaml", MAIN_DICT),
            ("other.dict.yaml", OTHER_DICT),
        ],
    );
    let got = dir.candidates("ni");
    assert!(got.iter().any(|t| t == "甲"), "{got:?}");
    assert!(
        !got.iter().any(|t| t == "乙"),
        "`@other` 没有声明自己的词库 ⇒ 应当落到主词库，不该出现「乙」：{got:?}"
    );
    // 而且这**不是**降级：没有"独立词库不生效"的警告。
    let notes = dir.degradations();
    assert!(
        !notes.contains("独立词库"),
        "没写 dictionary 的实例不该报「独立词库不生效」：{notes}"
    );
}

#[test]
fn a_declared_instance_dictionary_is_no_longer_reported_as_unsupported() {
    // 反面：**写了**独立词库的实例，现在不该再有那条降级诊断。
    let dir = Dir::new(
        "nodiag",
        &script_schema(),
        &[
            ("main.dict.yaml", MAIN_DICT),
            ("other.dict.yaml", OTHER_DICT),
        ],
    );
    let notes = dir.degradations();
    assert!(
        !notes.contains("独立词库"),
        "实例词库已经装配上了，不该再报「不生效」：{notes}"
    );
}

#[test]
fn an_unloadable_instance_dictionary_is_reported_not_ignored() {
    // 实例词库写错名字 ⇒ **必须出声**，而不是悄悄退回主词库。
    let schema = "\
schema:
  schema_id: g3
  name: 实例词库
  version: \"1.0\"
engine:
  tag: abc
  processors: [speller]
  segmentors: [abc_segmentor]
  translators: [script_translator, script_translator@other]
speller:
  alphabet: [ni, hao]
translator:
  dictionary: main
other:
  dictionary: does-not-exist
";
    let dir = Dir::new("missing", schema, &[("main.dict.yaml", MAIN_DICT)]);
    let defs = stele_schemes::load_dir(&dir.0);
    // 目录装载是**严格**入口：这里必须报错（或至少不是"静默忽略"）。
    match defs {
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("does-not-exist") || msg.contains("other"),
                "诊断必须指出是哪个实例的哪本词库：{msg}"
            );
        }
        Ok(d) => {
            // 宽松入口（报告式）下：坏方案被跳过，但**必须**进 skipped。
            let got = stele_schemes::load_dir_reporting(&dir.0).expect("至少主词库能装上");
            let warnings = got.warnings().join(" ");
            assert!(
                warnings.contains("does-not-exist") || warnings.contains("other"),
                "被跳过的实例词库必须出现在警告里：{warnings}"
            );
            let _ = d;
        }
    }
}
