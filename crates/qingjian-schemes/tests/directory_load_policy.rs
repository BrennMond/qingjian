//! # 目录装载策略：一个坏方案不该拖垮整个目录（审计 §2.G1）
//!
//! 审计实测：本机 `/usr/share/rime-data` 的 13 个上游方案逐个隔离装载，
//! 0 个成功；而且**只要目录里有一个坏方案，整个目录都装不上**。
//!
//! 这与项目自己的目标（D26：配置错误不阻止启动）直接冲突：用户加了一个
//! 写错的方案文件，代价却是"整个输入法用不了"——而"打字去改配置"
//! 恰好是输入法唯一的自救手段。
//!
//! 策略（`qingjian_schemes::DirLoad`）：
//!
//! | 情况 | 行为 |
//! | --- | --- |
//! | 至少一个方案装上了 | `Ok`，坏的那些进 `skipped`，**调用方必须打印** |
//! | 一个都没装上 | `Err`（没有任何可用方案，启动没有意义） |
//! | 目录读不了 / 没有方案文件 | `Err` |

use qingjian_core::Engine;
use qingjian_engine::EngineImpl;

const GOOD: &str = "\
schema:
  schema_id: good
  name: 好方案
  version: \"1.0\"
engine:
  tag: abc
  translator: spelling_graph
speller:
  alphabet: [ni, hao]
translator:
  dictionary: good
";

/// 一个语法上就不成立的方案（缺少必需的 `schema` / `speller` 段）。
const BAD: &str = "\
schema:
  schema_id: bad
  name: 坏方案
this is not a scheme at all: [
";

fn dict() -> &'static str {
    "---\nname: good\nversion: \"1\"\nsort: by_weight\n...\n\n你\tni\t100\n"
}

struct Dir(std::path::PathBuf);

impl Dir {
    fn new(tag: &str, files: &[(&str, &str)]) -> Self {
        let root = std::env::temp_dir().join(format!(
            "qingjian-dirload-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("建临时目录");
        for (name, text) in files {
            std::fs::write(root.join(name), text).expect("写文件");
        }
        Self(root)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_broken_schema_is_skipped_not_fatal() {
    // 目录里**同时**有好方案和坏方案。
    let dir = Dir::new(
        "mixed",
        &[
            ("a-good.schema.yaml", GOOD),
            ("b-broken.schema.yaml", BAD),
            ("good.dict.yaml", dict()),
        ],
    );

    // ① 旧的严格入口仍然会失败（向后兼容：调用方要"全有或全无"时用它）。
    assert!(
        qingjian_schemes::load_dir(&dir.0).is_err(),
        "严格入口遇到坏方案必须报错"
    );

    // ② 新的报告入口：好的装上，坏的进 skipped，**且诊断可读**。
    let got = qingjian_schemes::load_dir_reporting(&dir.0).expect("至少有一个方案能装上");
    assert_eq!(got.loaded.len(), 1, "好方案必须装上");
    assert_eq!(got.loaded[0].def.info.schema_id, "good");
    assert_eq!(got.skipped.len(), 1, "坏方案必须被记下来，而不是静默丢掉");
    let (name, diag) = &got.skipped[0];
    assert_eq!(name, "b-broken.schema.yaml");
    assert!(!diag.message.is_empty(), "诊断不能是空的");

    // ③ 警告必须是给人看的一行行文本（调用方负责打印）。
    let warnings = got.warnings();
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0].contains("b-broken.schema.yaml") && warnings[0].contains("跳过"),
        "警告要说清楚是哪个方案被跳过了：{warnings:?}"
    );

    // ④ 装上来的那一个**真的能用**——不是"装载成功但引擎编译不了"。
    let engine = EngineImpl::new(&got.into_defs()).expect("好方案必须能编译");
    let mut s = engine.create_session();
    for c in "ni".chars() {
        s.process_key(qingjian_core::Key::ch(c));
    }
    assert!(
        s.candidates().iter().any(|c| c.text == "你"),
        "装上的方案必须真的能出候选"
    );
}

#[test]
fn a_directory_with_only_broken_schemas_is_an_error() {
    // 一个都没装上 ⇒ 明确失败，而不是"启动了一个什么都不能用的引擎"。
    let dir = Dir::new("allbad", &[("a.schema.yaml", BAD), ("b.schema.yaml", BAD)]);
    let e = qingjian_schemes::load_dir_reporting(&dir.0).expect_err("全坏必须报错");
    let msg = e.to_string();
    assert!(!msg.is_empty());
}

#[test]
fn an_empty_directory_is_an_error() {
    let dir = Dir::new("empty", &[("readme.txt", "not a scheme")]);
    assert!(qingjian_schemes::load_dir_reporting(&dir.0).is_err());
}

#[test]
fn a_missing_scheme_file_is_reported_not_silently_dropped() {
    // 坏在"读不出文件"这一层（这里是: 一个目录冒充方案文件）。
    let dir = Dir::new(
        "unreadable",
        &[("a-good.schema.yaml", GOOD), ("good.dict.yaml", dict())],
    );
    std::fs::create_dir_all(dir.0.join("b-dir.schema.yaml")).expect("目录冒充方案文件");

    let got = qingjian_schemes::load_dir_reporting(&dir.0).expect("好方案仍应装上");
    assert_eq!(got.loaded.len(), 1);
    assert_eq!(got.skipped.len(), 1, "读不了的方案也要出现在 skipped 里");
}
