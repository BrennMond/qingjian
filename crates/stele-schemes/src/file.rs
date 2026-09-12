//! # Loading a scheme from a file
//!
//! 中文职责：把 `.schema.yaml` + `.dict.yaml` **编译成**引擎能直接用的方案。
//! English role: compile `.schema.yaml` + `.dict.yaml` into a scheme the engine
//! can use directly.
//! 架构位置：`stele-config` / `stele-dict` 与 `stele-engine` 之间的桥。
//!
//! # 这是"内核与方案分离"的落点（PLAN D24）
//!
//! 引擎只认识 [`SchemeDef`] 这一份**编译后的**数据；文件的形状、
//! 字段名、默认值全部由这一层负责。于是：
//!
//! - 换一种配置文件格式，只改这一层；
//! - 引擎里**没有任何**具体输入法的知识（D20 的命名门禁守着这条）。
//!
//! # 错误报告的原则
//!
//! **一次报出全部问题，且每条都带行号与"该怎么改"。**
//! PLAN D17 把 RIME 的"静默忽略"列为反面教材：一个写错的字段名
//! 如果被默默跳过，用户看到的是"行为诡异"，而不是"第 12 行写错了"。
//!
//! 所以本模块**先收集全部诊断再返回**，而不是遇到第一个就 `return`。

use stele_config::{Node, Value};
use stele_core::Score;
use stele_core::{Diagnostic, SchemaError, SchemaInfo, Switch};
use stele_dict as dict;
use stele_engine::pipeline::CANDIDATE_CAP;
use stele_engine::scheme::{entry, SchemeDef, TranslatorKind, SCHEME_FORMAT_VERSION};
use stele_engine::spelling::Rule;

/// `engine.translator` 的取值。
const TRANSLATOR_SPELLING_GRAPH: &str = "spelling_graph";
/// `engine.translator` 的取值。
const TRANSLATOR_EXACT_CODE: &str = "exact_code";

/// 从文本装载一份方案。
///
/// # Arguments / 参数
/// * `text` — `.schema.yaml` 的内容。
/// * `path` — 用于诊断的路径。
/// * `dicts` — 解析 `translator.dictionary` 所指词典的地方。
///
/// # Returns / 返回
/// 编译好的方案声明；尚未 [`SchemeDef::compile`]。
///
/// # Errors
///
/// 任何字段缺失/类型不对/取值不认识/词典装载失败，都会**汇总成一条**
/// [`SchemaError::Invalid`]，其中每条诊断都带行号。
///
/// # Panics
///
/// 不会 panic。上一行之所以显式写出来，是因为 `clippy::missing_panics_doc`
/// 要求任何返回 `Result` 的公开函数说明这件事——**"不会 panic"也是一条契约**。
pub fn load_scheme(
    text: &str,
    path: &str,
    dicts: &dyn dict::Source,
) -> Result<SchemeDef, SchemaError> {
    let root = stele_config::parse(text).map_err(|e| SchemaError::Invalid {
        schema_id: path.to_owned(),
        diagnostics: vec![Diagnostic::new(
            path,
            format!("第 {} 行：{}", e.line, e.message),
        )],
    })?;

    let mut diags: Vec<Diagnostic> = Vec::new();
    let mut schema_id = String::new();

    // ── schema 段 ──
    let mut info = SchemaInfo {
        schema_id: String::new(),
        name: String::new(),
        version: String::new(),
        format_version: SCHEME_FORMAT_VERSION,
        family: None,
    };
    match root.get("schema") {
        None => diags
            .push(Diagnostic::new(path, "缺少 `schema` 段（方案的元数据）").with_field("schema")),
        Some(s) => {
            let line = s.line;
            info.schema_id = req_str(s, "schema_id", path, &mut diags).unwrap_or_default();
            info.name = req_str(s, "name", path, &mut diags).unwrap_or_default();
            info.version = req_str(s, "version", path, &mut diags).unwrap_or_default();
            info.family = s.get("family").and_then(Node::as_str);
            // 版本字段是字符串：YAML 会把 `1.0` 当数字，但那不是我们想要的。
            if info.version.is_empty() {
                let _ = line;
            }
        }
    }
    schema_id.clone_from(&info.schema_id);

    // ── switches ──
    let mut switches: Vec<Switch> = Vec::new();
    if let Some(sw) = root.get("switches") {
        match sw.as_seq() {
            None => diags.push(
                Diagnostic::new(path, "`switches` 必须是列表")
                    .with_field("switches")
                    .with_entry(sw.line.to_string()),
            ),
            Some(items) => {
                for (i, item) in items.iter().enumerate() {
                    match read_switch(item, path, i) {
                        Ok(s) => switches.push(s),
                        Err(d) => diags.push(d),
                    }
                }
            }
        }
    }

    // ── engine 段 ──
    let engine = root.get("engine");
    let tag_text = engine
        .and_then(|e| e.get("tag"))
        .and_then(Node::as_str)
        .unwrap_or_default();
    if tag_text.is_empty() {
        diags.push(
            Diagnostic::new(path, "缺少 `engine.tag`（分段标签，翻译器靠它绑定）")
                .with_field("engine.tag"),
        );
    }
    let translator_text = engine
        .and_then(|e| e.get("translator"))
        .and_then(Node::as_str)
        .unwrap_or_default();
    let translator = match translator_text.as_str() {
        TRANSLATOR_SPELLING_GRAPH => Some(TranslatorKind::SpellingGraph),
        TRANSLATOR_EXACT_CODE => Some(TranslatorKind::ExactCode),
        "" => {
            diags.push(
                Diagnostic::new(path, "缺少 `engine.translator`")
                    .with_field("engine.translator")
                    .with_entry(format!(
                        "取值只能是 `{TRANSLATOR_SPELLING_GRAPH}`（编码集合可枚举：拼音、双拼）\
                         或 `{TRANSLATOR_EXACT_CODE}`（不可枚举：仓颉、五笔）"
                    )),
            );
            None
        }
        other => {
            diags.push(
                Diagnostic::new(
                    path,
                    format!("不认识的 `engine.translator` 取值：`{other}`"),
                )
                .with_field("engine.translator")
                .with_entry(format!(
                    "取值只能是 `{TRANSLATOR_SPELLING_GRAPH}` 或 `{TRANSLATOR_EXACT_CODE}`"
                )),
            );
            None
        }
    };
    let candidate_cap = engine
        .and_then(|e| e.get("candidate_cap"))
        .and_then(Node::as_int)
        .and_then(|c| usize::try_from(c).ok())
        .unwrap_or(CANDIDATE_CAP);

    // ── speller 段 ──
    let speller = root.get("speller");
    let alphabet: Vec<String> = speller
        .and_then(|s| s.get("alphabet"))
        .and_then(Node::as_seq)
        .map(|seq| seq.iter().filter_map(Node::as_str).collect())
        .unwrap_or_default();
    if alphabet.is_empty() {
        diags.push(
            Diagnostic::new(path, "缺少 `speller.alphabet`（编码字母表）")
                .with_field("speller.alphabet")
                .with_entry("拼音方案下它是音节表；字形方案下它是字母表"),
        );
    }

    let mut rules: Vec<Rule> = Vec::new();
    if let Some(rs) = speller.and_then(|s| s.get("rules")) {
        match rs.as_seq() {
            None => diags.push(
                Diagnostic::new(path, "`speller.rules` 必须是列表").with_field("speller.rules"),
            ),
            Some(items) => {
                for (i, item) in items.iter().enumerate() {
                    match read_rule(item, path, i) {
                        Ok(r) => rules.push(r),
                        Err(d) => diags.push(d),
                    }
                }
            }
        }
    }

    // ── translator 段：取词典 ──
    let dict_name = root
        .get("translator")
        .and_then(|t| t.get("dictionary"))
        .and_then(Node::as_str);
    let mut entries: Vec<(Vec<String>, String, f64)> = Vec::new();
    match dict_name {
        None => diags.push(
            Diagnostic::new(path, "缺少 `translator.dictionary`（要挂载哪本词典）")
                .with_field("translator.dictionary"),
        ),
        Some(name) => match dict::load_with_imports(dicts, &name, &name) {
            Ok(loaded) => {
                for e in &loaded.entries {
                    entries.push(entry(&e.units(), &e.word, e.weight));
                }
            }
            Err(e) => diags.push(
                Diagnostic::new(path, format!("词典装载失败：{e}"))
                    .with_field("translator.dictionary"),
            ),
        },
    }

    // ── 汇总 ──
    if !diags.is_empty() {
        return Err(SchemaError::Invalid {
            schema_id,
            diagnostics: diags,
        });
    }

    // 标签必须 `&'static str`（内核的 `Tag` 类型）。
    //
    // **在装载期 interning 一次**是有意为之：标签来自封闭集合
    // （一个方案就那么几个），换来的是热路径上零分配、零比较字符串。
    // 每份方案只会泄漏一个短字符串，进程生命周期内可忽略。
    let tag: stele_core::Tag = Box::leak(tag_text.into_boxed_str());

    Ok(SchemeDef {
        info,
        switches,
        tag,
        alphabet,
        rules,
        entries,
        translator: translator.expect("已在上面校验过"),
        candidate_cap,
    })
}

fn req_str(node: &Node, key: &str, path: &str, diags: &mut Vec<Diagnostic>) -> Option<String> {
    match node.get(key).and_then(Node::as_str) {
        Some(v) if !v.is_empty() => Some(v),
        _ => {
            diags.push(
                Diagnostic::new(path, format!("缺少 `{key}`"))
                    .with_field(format!("schema.{key}"))
                    .with_entry(format!("第 {} 行", node.line)),
            );
            None
        }
    }
}

fn read_switch(item: &Node, path: &str, idx: usize) -> Result<Switch, Diagnostic> {
    let name = item.get("name").and_then(Node::as_str).ok_or_else(|| {
        Diagnostic::new(path, format!("第 {idx} 个开关缺少 `name`"))
            .with_field(format!("switches[{idx}].name"))
            .with_entry(format!("第 {} 行", item.line))
    })?;

    let states = item.get("states").and_then(Node::as_seq).and_then(|seq| {
        let v: Vec<String> = seq.iter().filter_map(Node::as_str).collect();
        if v.len() == 2 {
            Some([v[0].clone(), v[1].clone()])
        } else {
            None
        }
    });

    // `reset` 是 RIME 的写法：0/1 表示默认关/开。
    let on = match item.get("reset").map(|r| &r.value) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Int(0)) => false,
        Some(Value::Int(_)) => true,
        _ => false,
    };

    Ok(Switch {
        name,
        states,
        on,
        abbrev: None,
    })
}

fn read_rule(item: &Node, path: &str, idx: usize) -> Result<Rule, Diagnostic> {
    let Some(map) = item.as_map() else {
        return Err(Diagnostic::new(
            path,
            format!("第 {idx} 条规则必须是 `{{ 规则名: 参数 }}` 的形式"),
        )
        .with_field(format!("speller.rules[{idx}]"))
        .with_entry(format!("第 {} 行", item.line)));
    };
    if map.len() != 1 {
        return Err(Diagnostic::new(
            path,
            format!(
                "第 {idx} 条规则有 {} 个键；一条规则只能有一个规则名",
                map.len()
            ),
        )
        .with_field(format!("speller.rules[{idx}]")));
    }
    let (name, arg) = &map[0];
    let cost = |n: &Node| -> Score {
        n.get("cost")
            .and_then(Node::as_f64)
            .map_or(Score::ZERO, Score::from_weight)
    };

    match name.as_str() {
        "abbrev" => {
            // 允许 `abbrev: 1`（只给长度）或 `abbrev: { take: 1, cost: 0.5 }`。
            let (take, c) = match &arg.value {
                Value::Int(t) => (usize::try_from(*t).unwrap_or(1), Score::ZERO),
                Value::Map(_) => (
                    arg.get("take")
                        .and_then(Node::as_int)
                        .and_then(|t| usize::try_from(t).ok())
                        .unwrap_or(1),
                    cost(arg),
                ),
                _ => {
                    return Err(Diagnostic::new(
                        path,
                        "`abbrev` 的参数应是整数（保留前几个字符）或 `{ take, cost }`",
                    )
                    .with_field(format!("speller.rules[{idx}].abbrev")));
                }
            };
            Ok(Rule::Abbrev { take, cost: c })
        }
        "equivalence" => {
            let pairs_node = arg.get("pairs").ok_or_else(|| {
                Diagnostic::new(path, "`equivalence` 缺少 `pairs`")
                    .with_field(format!("speller.rules[{idx}].equivalence.pairs"))
                    .with_entry("形如 `pairs: [[z, zh], [c, ch]]`")
            })?;
            let pairs = read_pairs(pairs_node, path, idx)?;
            Ok(Rule::Equivalence {
                pairs,
                cost: cost(arg),
            })
        }
        other => Err(Diagnostic::new(path, format!("不认识的拼写规则 `{other}`"))
            .with_field(format!("speller.rules[{idx}]"))
            .with_entry("目前支持 `abbrev`（缩写）与 `equivalence`（等价替换）")),
    }
}

fn read_pairs(node: &Node, path: &str, idx: usize) -> Result<Vec<(char, char)>, Diagnostic> {
    let seq = node.as_seq().ok_or_else(|| {
        Diagnostic::new(path, "`pairs` 必须是列表")
            .with_field(format!("speller.rules[{idx}].equivalence.pairs"))
    })?;
    let mut out = Vec::new();
    for (i, pair) in seq.iter().enumerate() {
        let Some(p) = pair.as_seq() else {
            return Err(Diagnostic::new(
                path,
                format!("`pairs[{i}]` 必须是一对字符，例如 `[z, zh]`"),
            )
            .with_field(format!("speller.rules[{idx}].equivalence.pairs")));
        };
        if p.len() != 2 {
            return Err(
                Diagnostic::new(path, format!("`pairs[{i}]` 需要恰好两个元素"))
                    .with_field(format!("speller.rules[{idx}].equivalence.pairs")),
            );
        }
        let a = p[0].as_str().and_then(|s| s.chars().next());
        let b = p[1].as_str().and_then(|s| s.chars().next());
        match (a, b) {
            (Some(a), Some(b)) => out.push((a, b)),
            _ => {
                return Err(
                    Diagnostic::new(path, format!("`pairs[{i}]` 的元素必须是单个字符"))
                        .with_field(format!("speller.rules[{idx}].equivalence.pairs")),
                );
            }
        }
    }
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────────
// 从目录装载
// ─────────────────────────────────────────────────────────────────────────────

/// 从一个目录里装载**全部** `*.schema.yaml`。
///
/// 这是 P2 "能加载自建词库"的入口：把方案与词库放进一个目录，
/// 用 `--scheme-dir` 指过来即可，**不需要重新编译**。
///
/// 装载顺序按文件名排序，**因此是确定的**（PLAN §5.2）。
///
/// # Errors
///
/// 目录不存在、里面一个方案都没有、或任何一份方案有错时返回 [`SchemaError`]。
/// **一份坏方案不会阻止其它方案**——错误里带上文件名，调用方可以选择跳过
/// （PLAN D26：配置错误绝不阻止启动）。
///
/// # Panics
///
/// 不会 panic。
pub fn load_dir(root: &std::path::Path) -> Result<Vec<SchemeDef>, SchemaError> {
    let entries = std::fs::read_dir(root).map_err(|e| SchemaError::Invalid {
        schema_id: root.display().to_string(),
        diagnostics: vec![Diagnostic::new(
            root.display().to_string(),
            format!("读不了这个目录：{e}"),
        )],
    })?;

    let mut files: Vec<std::path::PathBuf> = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.ends_with(".schema.yaml") {
            files.push(p);
        }
    }
    // 排序 → 装载顺序确定。
    files.sort();

    if files.is_empty() {
        return Err(SchemaError::Invalid {
            schema_id: root.display().to_string(),
            diagnostics: vec![Diagnostic::new(
                root.display().to_string(),
                "这个目录里没有 `*.schema.yaml`",
            )
            .with_entry("方案文件应当以 `.schema.yaml` 结尾")],
        });
    }

    let src = dict::DirSource::new(root);
    let mut out = Vec::with_capacity(files.len());
    for f in files {
        let Some(name) = f.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let text = std::fs::read_to_string(&f).map_err(|e| SchemaError::Invalid {
            schema_id: name.to_owned(),
            diagnostics: vec![Diagnostic::new(name, format!("读不了文件：{e}"))],
        })?;
        out.push(load_scheme(&text, name, &src)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mem(std::collections::BTreeMap<String, String>);

    impl dict::Source for Mem {
        fn read(&self, rel: &str) -> Option<String> {
            self.0.get(rel).cloned()
        }
    }

    fn dicts() -> Mem {
        let mut m = std::collections::BTreeMap::new();
        m.insert(
            "d".into(),
            "---\nname: d\nversion: \"1\"\n...\n你好\tni hao\t100\n".into(),
        );
        Mem(m)
    }

    const GOOD: &str = "\
schema:
  schema_id: t
  name: 测试
  version: \"1.0\"
switches:
  - name: ascii_mode
    states: [中, Ａ]
    reset: 0
engine:
  tag: abc
  translator: spelling_graph
speller:
  alphabet: [ni, hao]
  rules:
    - abbrev: { take: 1, cost: 0.5 }
translator:
  dictionary: d
";

    #[test]
    fn loads_a_well_formed_scheme() {
        let d = load_scheme(GOOD, "t.schema.yaml", &dicts()).unwrap();
        assert_eq!(d.info.schema_id, "t");
        assert_eq!(d.info.version, "1.0");
        assert_eq!(d.tag, "abc");
        assert_eq!(d.translator, TranslatorKind::SpellingGraph);
        assert_eq!(d.alphabet, ["ni", "hao"]);
        assert_eq!(d.entries.len(), 1);
        assert_eq!(d.entries[0].1, "你好");
        assert_eq!(d.switches.len(), 1);
    }

    #[test]
    fn exact_code_scheme_needs_no_rules() {
        let text = GOOD
            .replace("spelling_graph", "exact_code")
            .replace("  rules:\n    - abbrev: { take: 1, cost: 0.5 }\n", "");
        let d = load_scheme(&text, "t", &dicts()).unwrap();
        assert_eq!(d.translator, TranslatorKind::ExactCode);
        assert!(d.rules.is_empty(), "规范拼写是基线，空规则就是只有它");
    }

    #[test]
    fn missing_fields_are_all_reported_at_once_with_lines() {
        let bad = "schema:\n  name: x\nengine:\n  tag: abc\n";
        let e = load_scheme(bad, "bad.yaml", &dicts()).unwrap_err();
        match e {
            SchemaError::Invalid { diagnostics, .. } => {
                assert!(
                    diagnostics.len() >= 4,
                    "应当一次报出全部问题，实得 {}：{diagnostics:?}",
                    diagnostics.len()
                );
                // 每条都要能定位。
                assert!(diagnostics.iter().all(|d| !d.source.is_empty()));
                let joined = diagnostics
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(joined.contains("schema_id"), "{joined}");
                assert!(joined.contains("translator"), "{joined}");
                assert!(joined.contains("alphabet"), "{joined}");
            }
            other => panic!("应当是 Invalid，得到 {other:?}"),
        }
    }

    #[test]
    fn unknown_translator_lists_the_valid_values() {
        let text = GOOD.replace("spelling_graph", "telepathy");
        let e = load_scheme(&text, "t", &dicts()).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("spelling_graph"), "{msg}");
        assert!(msg.contains("exact_code"), "{msg}");
    }

    #[test]
    fn unknown_rule_lists_what_is_supported() {
        let text = GOOD.replace("abbrev: { take: 1, cost: 0.5 }", "magic: 1");
        let e = load_scheme(&text, "t", &dicts()).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("abbrev"), "{msg}");
        assert!(msg.contains("equivalence"), "{msg}");
    }

    #[test]
    fn missing_dictionary_is_reported_with_the_reason() {
        let text = GOOD.replace("dictionary: d", "dictionary: ghost");
        let e = load_scheme(&text, "t", &dicts()).unwrap_err();
        assert!(e.to_string().contains("ghost"), "{e}");
    }

    #[test]
    fn abbrev_accepts_the_shorthand_form() {
        let text = GOOD.replace("abbrev: { take: 1, cost: 0.5 }", "abbrev: 2");
        let d = load_scheme(&text, "t", &dicts()).unwrap();
        assert!(matches!(d.rules[0], Rule::Abbrev { take: 2, .. }));
    }

    #[test]
    fn equivalence_pairs_are_read() {
        let text = GOOD.replace(
            "    - abbrev: { take: 1, cost: 0.5 }\n",
            "    - equivalence: { pairs: [[z, zh]], cost: 1.0 }\n",
        );
        let d = load_scheme(&text, "t", &dicts()).unwrap();
        match &d.rules[0] {
            Rule::Equivalence { pairs, .. } => assert_eq!(pairs, &[('z', 'z')]),
            other => panic!("应当是 Equivalence，得到 {other:?}"),
        }
    }
}
