//! # Reading component configuration
//!
//! 中文职责：把方案文件里**各零件的配置段**读成 `stele-engine` 的数据类型。
//! English role: read each component's config block into engine data types.
//! 架构位置：`stele-schemes/src/file.rs` 的延续——把"读 YAML"与"编译方案"
//! 分开，否则 `file.rs` 会变成一千多行的一团。
//!
//! # 一条贯穿全文件的原则
//!
//! **取值不合法就报错，并说清该怎么改。** 这里的每一个 `parse_*` 都返回
//! `Option`，由调用方（[`crate::file`]）拼成带**行号**的诊断。
//! "猜一个"是被明确禁止的：RIME 把写错的字段名静默忽略，于是用户看到的是
//! "行为诡异"而不是"第 42 行写错了"（PLAN D17）。

use stele_config::Node;
use stele_core::Diagnostic;
use stele_core::Tag;
use stele_engine::keyspec::{parse_key_name, KeyChord};
use stele_engine::spec::{
    AffixSpec, At, EditorAction, EngineSpec, KeyBinding, NavigatorSpec, PunctuatorSpec,
    RecogPattern, RecognizerSpec, ReverseLookupSpec, SimplifierSpec, TranslatorSpec, WhenPredicate,
};
use stele_engine::tag::TagTable;

/// `engine:` 段：零件名字列表。
pub fn read_engine(node: &Node) -> EngineSpec {
    let names = |key: &str| -> Vec<String> {
        node.get(key)
            .and_then(Node::as_seq)
            .map(|seq| seq.iter().filter_map(stele_config::Node::as_str).collect())
            .unwrap_or_default()
    };
    EngineSpec {
        processors: names("processors"),
        segmentors: names("segmentors"),
        translators: names("translators"),
        filters: names("filters"),
        tag: node.get("tag").and_then(Node::as_str),
    }
}

/// `recognizer:` 段。
///
/// # `patterns` 的两种写法
///
/// RIME 允许一个模式名对应**多个**正则：
///
/// ```yaml
/// patterns:
///   punct: "^/([0-9]|10)$"
///   email: ["^[A-Za-z]+@[A-Za-z]+$", "^[A-Za-z]+\\.com$"]
/// ```
///
/// 我们把它展开成多条 [`RecogPattern`]（同名）——匹配时"取最长"
/// 已经能正确处理它们，不需要一个"模式组"的概念。
pub fn read_recognizer(node: &Node, diags: &mut Vec<Diagnostic>, path: &str) -> RecognizerSpec {
    let mut patterns: Vec<RecogPattern> = Vec::new();
    let Some(map) = node.get("patterns") else {
        return RecognizerSpec {
            import_preset: node.get("import_preset").and_then(Node::as_str),
            patterns,
        };
    };
    let Some(entries) = map.as_map() else {
        diags.push(
            Diagnostic::new(path, "`recognizer.patterns` 必须是 `名字: 正则` 的映射")
                .with_field("recognizer.patterns")
                .with_entry(format!("第 {} 行", map.line)),
        );
        return RecognizerSpec::default();
    };
    for (name, value) in entries {
        // 一个模式名可以给**一串**正则；展开成多条同名模式。
        let mut regexes: Vec<(String, u32)> = Vec::new();
        match &value.value {
            stele_config::Value::Str(s) => regexes.push((s.clone(), value.line)),
            stele_config::Value::Seq(items) => {
                for it in items {
                    match it.as_str() {
                        Some(s) => regexes.push((s, it.line)),
                        None => diags.push(
                            Diagnostic::new(path, format!("`{name}` 的正则必须是字符串"))
                                .with_field(format!("recognizer.patterns.{name}"))
                                .with_entry(format!("第 {} 行", it.line)),
                        ),
                    }
                }
            }
            _ => diags.push(
                Diagnostic::new(
                    path,
                    format!("`recognizer.patterns/{name}` 必须是字符串或字符串列表"),
                )
                .with_field(format!("recognizer.patterns.{name}"))
                .with_entry(format!("第 {} 行", value.line)),
            ),
        }
        for (regex, line) in regexes {
            match stele_engine::regex::Regex::compile(&regex) {
                Ok(_) => patterns.push(RecogPattern {
                    name: name.clone(),
                    leading: stele_engine::segmentor::leading_literal(&regex),
                    trailing: trailing_literal(&regex),
                    regex,
                    at: At::new(line as usize),
                }),
                Err(e) => diags.push(
                    Diagnostic::new(
                        path,
                        format!("`recognizer.patterns/{name}` 的正则编译失败：{e}"),
                    )
                    .with_field(format!("recognizer.patterns.{name}"))
                    .with_entry(format!("第 {line} 行：{regex}")),
                ),
            }
        }
    }
    RecognizerSpec {
        import_preset: node.get("import_preset").and_then(Node::as_str),
        patterns,
    }
}

/// 正则是否要求"必须以某个字面量结尾"。
///
/// 只认 `...;$` 这种最直白的形式；取不准时返回 `None`，
/// 由 [`RecogPattern`] 的语义（`None` = 匹配到输入末尾）兜住。
fn trailing_literal(regex: &str) -> Option<String> {
    let body = regex.strip_suffix('$')?;
    let tail: String = body
        .chars()
        .rev()
        .take_while(char::is_ascii_alphanumeric)
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    if tail.is_empty() || body.ends_with(&tail) && tail.len() == body.len() {
        // 整个模式都是字面量（`^abc$`）：那是"整串等于 abc"，
        // 由前缀匹配 + 末尾锚已经能表达，不需要 trailing。
        None
    } else {
        Some(tail)
    }
}

/// `punctuator:` 段。
pub fn read_punctuator(node: &Node, diags: &mut Vec<Diagnostic>, path: &str) -> PunctuatorSpec {
    let table = |key: &str| -> Vec<(String, String)> {
        node.get(key)
            .and_then(Node::as_map)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.clone())))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut half_shape = table("half_shape");
    let mut full_shape = table("full_shape");
    // `import_preset`：**把预设叠在我写的东西底下**（RIME 的语义）。
    // 方案写了的键以方案为准，没写的由预设补上——于是方案只需要写
    // 自己**特有**的那几条。
    if let Some(name) = node
        .get("import_preset")
        .and_then(stele_config::Node::as_str)
    {
        match stele_engine::presets::get(&name) {
            Some(p) => {
                for (k, v) in &p.half_shape {
                    if !half_shape.iter().any(|(hk, _)| hk == k) {
                        half_shape.push((k.clone(), v.clone()));
                    }
                }
                for (k, v) in &p.full_shape {
                    if !full_shape.iter().any(|(fk, _)| fk == k) {
                        full_shape.push((k.clone(), v.clone()));
                    }
                }
                half_shape.sort();
                full_shape.sort();
            }
            None => diags.push(
                Diagnostic::new(
                    path,
                    format!("不认识的预设名 `{name}`（`punctuator.import_preset`）"),
                )
                .with_field("punctuator.import_preset")
                .with_entry(format!(
                    "我们提供的预设：{}。RIME 的 `default` / `symbols_v` 是它自己的\
                     资产，我们没有搬过来——请把需要的表直接写在这里",
                    stele_engine::presets::names().join("、")
                )),
            ),
        }
    }

    let symbols = table("symbols");
    // 符号表的前缀有两种约定，而它们用 `symbols_prefix` 区分：
    //
    // | 写法 | 含义 | 前缀 |
    // | --- | --- | --- |
    // | `symbols_prefix: "v"` + `"1": "①"` | 敲 `v1` 出 ① | `v`（键**不含**前缀） |
    // | `"/hx": "㊕"`（不写 `symbols_prefix`） | 敲 `/hx` 出 ㊕ | `/`（键**自带**前缀） |
    //
    // 第二行的判据是"**所有的键都以同一个字符开头**"——那是键自带前缀的
    // 形式特征。**不能只看第一个键**：`"1": "①"` 的第一个键以 `1` 开头，
    // 只看它就会把前缀猜成 `1`，于是 `v1` 永远查不到，
    // **符号表整个不工作且没有报错**（这个 bug 真发生过）。
    let symbol_prefix = node
        .get("symbols_prefix")
        .and_then(Node::as_str)
        .and_then(|s| s.chars().next())
        .or_else(|| {
            let first = symbols.first().and_then(|(k, _)| k.chars().next())?;
            let all_share_it = symbols
                .iter()
                .all(|(k, _)| k.starts_with(first) && k.chars().count() > 1);
            all_share_it.then_some(first)
        });
    PunctuatorSpec {
        full_shape,
        half_shape,
        symbols,
        symbol_prefix,
        at: At::new(node.line as usize),
    }
}

/// `editor:` 段。
pub fn read_editor(
    node: &Node,
    diags: &mut Vec<Diagnostic>,
    path: &str,
) -> Vec<(KeyChord, EditorAction)> {
    let mut out = Vec::new();
    let Some(map) = node.get("bindings") else {
        return out;
    };
    let Some(entries) = map.as_map() else {
        diags.push(
            Diagnostic::new(path, "`editor.bindings` 必须是 `按键: 动作` 的映射")
                .with_field("editor.bindings")
                .with_entry(format!("第 {} 行", map.line)),
        );
        return out;
    };
    for (key, value) in entries {
        let Some(action_name) = value.as_str() else {
            diags.push(
                Diagnostic::new(path, format!("`editor.bindings/{key}` 的值必须是动作名"))
                    .with_field(format!("editor.bindings.{key}"))
                    .with_entry(format!("第 {} 行", value.line)),
            );
            continue;
        };
        let Some(action) = EditorAction::parse(&action_name) else {
            diags.push(
                Diagnostic::new(
                    path,
                    format!("不认识的编辑器动作 `{action_name}`（绑在 `{key}` 上）"),
                )
                .with_field(format!("editor.bindings.{key}"))
                .with_entry(format!(
                    "可用的动作：{}",
                    EditorAction::all_names().join("、")
                )),
            );
            continue;
        };
        match parse_key_name(key) {
            Some(chord) => out.push((chord, action)),
            None => diags.push(
                Diagnostic::new(path, format!("不认识的按键名 `{key}`"))
                    .with_field(format!("editor.bindings.{key}"))
                    .with_entry(format!("第 {} 行", value.line)),
            ),
        }
    }
    out
}

/// `key_binder:` 段。
pub fn read_key_bindings(node: &Node, diags: &mut Vec<Diagnostic>, path: &str) -> Vec<KeyBinding> {
    let mut out = Vec::new();
    let Some(seq) = node.get("bindings").and_then(Node::as_seq) else {
        return out;
    };
    for (i, item) in seq.iter().enumerate() {
        let field = |k: &str| format!("key_binder.bindings[{i}].{k}");
        // `when`
        let when = item
            .get("when")
            .and_then(Node::as_str)
            .map_or(WhenPredicate::Always, |s| {
                WhenPredicate::parse(&s).unwrap_or_default()
            });
        // `accept`：一个键名或一个键名列表。
        let mut accept: Vec<KeyChord> = Vec::new();
        let accept_node = item.get("accept");
        let accept_names: Vec<(String, usize)> = match accept_node {
            Some(n) => match &n.value {
                stele_config::Value::Str(s) => vec![(s.clone(), n.line as usize)],
                stele_config::Value::Seq(items) => items
                    .iter()
                    .filter_map(|x| x.as_str().map(|s| (s, x.line as usize)))
                    .collect(),
                _ => Vec::new(),
            },
            None => Vec::new(),
        };
        for (name, line) in accept_names {
            match parse_key_name(&name) {
                Some(c) => accept.push(c),
                None => diags.push(
                    Diagnostic::new(path, format!("不认识的按键名 `{name}`"))
                        .with_field(field("accept"))
                        .with_entry(format!("第 {line} 行")),
                ),
            }
        }
        // `send` / `send_sequence` / `toggle`
        //
        // **存键名原文，不解析成键**：`send` 换成的是"另一串按键"，
        // 而按键的语义（`space` = 空格键 = 确认候选）是**引擎**的事。
        // 装载器只搬运名字，`key_binder` 自己解析——一份解析，
        // 两个使用者（见 `stele_engine::keyspec`）。
        //
        // 一个元素的 `send` 与多个元素的 `send_sequence` 在这里是同一种
        // 东西：librime 的 `binding.target` 就是一个 `KeySequence`。
        let mut send_keys: Option<Vec<String>> = None;
        if let Some(send_node) = item.get("send") {
            match send_node.as_str() {
                Some(v) => send_keys = Some(vec![v]),
                None => diags.push(
                    Diagnostic::new(path, "`send` 的值必须是按键名或一段文本")
                        .with_field(field("send"))
                        .with_entry(format!("第 {} 行", send_node.line)),
                ),
            }
        }
        if let Some(seq_node) = item.get("send_sequence") {
            match seq_node.as_seq() {
                Some(items) => {
                    let names: Vec<String> = items
                        .iter()
                        .filter_map(stele_config::Node::as_str)
                        .collect();
                    if names.is_empty() {
                        diags.push(
                            Diagnostic::new(path, "`send_sequence` 是空的，它什么都不会做")
                                .with_field(field("send_sequence"))
                                .with_entry(format!("第 {} 行", seq_node.line)),
                        );
                    } else {
                        if send_keys.is_some() {
                            diags.push(
                                Diagnostic::new(
                                    path,
                                    "同一条绑定同时写了 `send` 与 `send_sequence`，\
                                     只有 `send_sequence` 会生效",
                                )
                                .with_field(format!("key_binder.bindings[{i}]"))
                                .with_entry(format!("第 {} 行", item.line)),
                            );
                        }
                        send_keys = Some(names);
                    }
                }
                None => diags.push(
                    Diagnostic::new(path, "`send_sequence` 必须是列表")
                        .with_field(field("send_sequence"))
                        .with_entry(format!("第 {} 行", seq_node.line)),
                ),
            }
        }
        // librime 的绑定是一条**严格的选择链**：
        //
        // ```text
        // send → send_sequence → toggle → set_option → unset_option → select
        // ```
        //
        // **只有第一个写了的会生效**；一个都没写的整条被丢弃
        // （librime 在那里打 WARNING，我们报一条诊断）。
        //
        // 这里必须显式实现这条链，而不是"每个动作各自生效"：
        // 后者在"同时写了 `send` 与 `toggle`"时会把两件事都做掉，
        // 而方案作者的预期（照 RIME 的文档）只有一件。
        let mut toggle = item.get("toggle").and_then(stele_config::Node::as_str);
        let mut set_option = item.get("set_option").and_then(stele_config::Node::as_str);
        let mut unset_option = item
            .get("unset_option")
            .and_then(stele_config::Node::as_str);

        if let Some(name) = item.get("select").and_then(stele_config::Node::as_str) {
            diags.push(
                Diagnostic::new(
                    path,
                    format!("`select`（切换到方案 `{name}`）尚未支持，这条绑定不会生效"),
                )
                .with_field(field("select"))
                .with_entry(format!("第 {} 行", item.line)),
            );
            continue;
        }

        let effect = if send_keys.is_some() {
            "send"
        } else if toggle.is_some() {
            "toggle"
        } else if set_option.is_some() {
            "set_option"
        } else if unset_option.is_some() {
            "unset_option"
        } else {
            ""
        };
        if effect.is_empty() {
            diags.push(
                Diagnostic::new(
                    path,
                    "这条绑定没有 `send` / `toggle` / `set_option` / `unset_option`\
                     —— 它什么都不会做",
                )
                .with_field(format!("key_binder.bindings[{i}]"))
                .with_entry(format!("第 {} 行", item.line)),
            );
            continue;
        }
        // 链上第一个之后的动作**必须报出来**：静默只做一半的效果，
        // 症状是"我配了两件事、只发生了一件"，而那查起来毫无线索。
        let mut dropped: Vec<&str> = Vec::new();
        if effect != "send" && send_keys.is_some() {
            dropped.push("send");
        }
        if effect != "toggle" && toggle.is_some() {
            dropped.push("toggle");
        }
        if effect != "set_option" && set_option.is_some() {
            dropped.push("set_option");
        }
        if effect != "unset_option" && unset_option.is_some() {
            dropped.push("unset_option");
        }
        if !dropped.is_empty() {
            diags.push(
                Diagnostic::new(
                    path,
                    format!(
                        "这条绑定写了多个动作，只有 `{effect}` 会生效（被忽略：{}）",
                        dropped.join("、")
                    ),
                )
                .with_field(format!("key_binder.bindings[{i}]"))
                .with_entry(
                    "librime 的绑定是一条严格的选择链：\
                     send → send_sequence → toggle → set_option → unset_option → select"
                        .to_owned(),
                ),
            );
        }
        // 落库时只留生效的那一个，避免"数据里有两个动作、行为只有一个"
        // 这种自相矛盾的状态流到引擎里。
        if effect != "toggle" {
            toggle = None;
        }
        if effect != "set_option" {
            set_option = None;
        }
        if effect != "unset_option" {
            unset_option = None;
        }
        out.push(KeyBinding {
            when,
            accept,
            send_keys,
            toggle,
            set_option,
            unset_option,
            at: At::new(item.line as usize),
        });
    }
    out
}

/// `navigator:` 段。
#[must_use]
pub fn read_navigator(node: &Node, diags: &mut Vec<Diagnostic>, path: &str) -> NavigatorSpec {
    let keys = |key: &str| -> Vec<KeyChord> {
        node.get(key)
            .and_then(Node::as_seq)
            .map(|seq| {
                seq.iter()
                    .filter_map(stele_config::Node::as_str)
                    .filter_map(|s| parse_key_name(&s))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut page_up = keys("page_up");
    let mut page_down = keys("page_down");
    if let Some(name) = node
        .get("import_preset")
        .and_then(stele_config::Node::as_str)
    {
        match stele_engine::presets::get(&name) {
            Some(p) => {
                if page_up.is_empty() {
                    page_up = p.page_up.iter().filter_map(|s| parse_key_name(s)).collect();
                }
                if page_down.is_empty() {
                    page_down = p
                        .page_down
                        .iter()
                        .filter_map(|s| parse_key_name(s))
                        .collect();
                }
            }
            None => diags.push(
                Diagnostic::new(
                    path,
                    format!("不认识的预设名 `{name}`（`navigator.import_preset`）"),
                )
                .with_field("navigator.import_preset")
                .with_entry(format!(
                    "我们提供的预设：{}",
                    stele_engine::presets::names().join("、")
                )),
            ),
        }
    }
    NavigatorSpec {
        page_up,
        page_down,
        up: keys("up"),
        down: keys("down"),
        at: At::new(node.line as usize),
    }
}

/// `affix_segmentor` 的实例配置（例如 `radical_lookup:` 段）。
pub fn read_affix(node: &Node, tags: &mut TagTable) -> AffixSpec {
    AffixSpec {
        tag: node
            .get("tag")
            .and_then(Node::as_str)
            .map(|t| tags.intern(&t)),
        prefix: node.get("prefix").and_then(Node::as_str),
        suffix: node.get("suffix").and_then(Node::as_str),
        extra_tags: node
            .get("extra_tags")
            .and_then(Node::as_seq)
            .map(|seq| {
                seq.iter()
                    .filter_map(stele_config::Node::as_str)
                    .map(|t| tags.intern(&t))
                    .collect()
            })
            .unwrap_or_default(),
        tips: node.get("tips").and_then(Node::as_str),
        at: At::new(node.line as usize),
    }
}

/// `reverse_lookup_filter` 的实例配置。
///
/// # `comment_format` 里的 `erase` 要翻译成 `xform`
///
/// 雾凇写的是 `erase/^.*$//`，而 `erase` 是**拼写代数**的运算子
/// （"整串匹配就消除这条拼写"），不是文本改写。在注释上它的意图
/// 显然是"清空注释"，等价于 `xform/^.*$//`。
///
/// 我们**替它翻译**并在诊断里说一声——而不是静默接受一个语义不对的
/// 运算子，也不是直接报错让用户的方案跑不起来。这是**唯一**一处
/// 语义翻译，且有据可依（两边的效果逐字节相同）。
pub fn read_reverse_lookup(
    node: &Node,
    tags: &mut TagTable,
    diags: &mut Vec<Diagnostic>,
    path: &str,
) -> ReverseLookupSpec {
    let mut rules = Vec::new();
    if let Some(seq) = node.get("comment_format").and_then(Node::as_seq) {
        for (i, item) in seq.iter().enumerate() {
            let Some(spec) = item.as_str() else { continue };
            let translated = if let Some(rest) = spec.strip_prefix("erase/") {
                diags.push(
                    Diagnostic::new(
                        path,
                        format!(
                            "`comment_format` 里的 `erase` 已按文本改写处理：\
                             `{spec}` → `xform/{rest}`"
                        ),
                    )
                    .with_field("comment_format")
                    .with_entry(format!("第 {} 行", item.line)),
                );
                format!("xform/{rest}")
            } else {
                spec.clone()
            };
            match Rule::parse(&translated) {
                Ok(r) => rules.push(r),
                Err(e) => diags.push(
                    Diagnostic::new(path, format!("第 {i} 条注释格式有误：{e}"))
                        .with_field(format!("comment_format[{i}]"))
                        .with_entry(format!("第 {} 行", item.line)),
                ),
            }
        }
    }
    ReverseLookupSpec {
        tags: node
            .get("tags")
            .and_then(Node::as_seq)
            .map(|seq| {
                seq.iter()
                    .filter_map(stele_config::Node::as_str)
                    .map(|t| tags.intern(&t))
                    .collect()
            })
            .unwrap_or_default(),
        dictionary: node.get("dictionary").and_then(Node::as_str),
        comment_format: rules,
        overwrite_comment: node
            .get("overwrite_comment")
            .is_some_and(|n| matches!(n.value, stele_config::Value::Bool(true))),
        at: At::new(node.line as usize),
    }
}

use stele_engine::spelling::Rule;

/// `simplifier` 的实例配置。
pub fn read_simplifier(
    node: &Node,
    tags: &mut TagTable,
    diags: &mut Vec<Diagnostic>,
    path: &str,
) -> SimplifierSpec {
    let tips = node
        .get("tips")
        .and_then(Node::as_str)
        .map_or(stele_engine::spec::TipsMode::All, |s| {
            stele_engine::spec::TipsMode::parse(&s).unwrap_or_default()
        });
    if node.get("tags").is_none() {
        let _ = &tags;
    }
    let _ = diags;
    let _ = path;
    SimplifierSpec {
        option_name: node.get("option_name").and_then(Node::as_str),
        opencc_config: node.get("opencc_config").and_then(Node::as_str),
        tips,
        inherit_comment: node
            .get("inherit_comment")
            .is_none_or(|n| matches!(n.value, stele_config::Value::Bool(true))),
        // 方案里写 `weight: 0.7`（线性比）。缺省 0.95 —— 见 `SimplifierSpec::weight`
        // 为什么"越小越看不见"。
        weight: node.get("weight").map_or(0.95, |n| match &n.value {
            stele_config::Value::Float(f) => *f,
            #[allow(clippy::cast_precision_loss)]
            stele_config::Value::Int(i) => *i as f64,
            _ => 0.95,
        }),
        tags: Vec::new(),
        at: At::new(node.line as usize),
    }
}

/// `simplifier` 实例的 `tags:`（单独读，因为它需要 `&mut TagTable`）。
pub fn read_tags(node: &Node, tags: &mut TagTable) -> Vec<Tag> {
    node.get("tags")
        .and_then(Node::as_seq)
        .map(|seq| {
            seq.iter()
                .filter_map(stele_config::Node::as_str)
                .map(|t| tags.intern(&t))
                .collect()
        })
        .unwrap_or_default()
}

/// 一个翻译器实例的配置。
pub fn read_translator(node: &Node, component: &str, alias: Option<&str>) -> TranslatorSpec {
    let bool_of = |key: &str| -> Option<bool> {
        node.get(key).map(|n| match &n.value {
            stele_config::Value::Bool(b) => *b,
            stele_config::Value::Int(i) => *i != 0,
            _ => false,
        })
    };
    let strings = |key: &str| -> Vec<String> {
        node.get(key)
            .and_then(Node::as_seq)
            .map(|seq| seq.iter().filter_map(stele_config::Node::as_str).collect())
            .unwrap_or_default()
    };
    TranslatorSpec {
        component: component.to_owned(),
        alias: alias.map(str::to_owned),
        dictionary: node.get("dictionary").and_then(Node::as_str),
        // RIME 的两套写法都收：`table_translator` 用 `enable_word_completion`，
        // `script_translator` 用 `enable_completion`——而真实方案里两者
        // 常常混用，所以**不看零件名**，两个键都认。
        enable_word_completion: bool_of("enable_word_completion")
            .or_else(|| bool_of("enable_completion")),
        enable_sentence: bool_of("enable_sentence"),
        initial_quality: node.get("initial_quality").and_then(Node::as_f64),
        comment_format: strings("comment_format"),
        preedit_format: strings("preedit_format"),
        prefix: node.get("prefix").and_then(Node::as_str),
        tips: node.get("tips").and_then(Node::as_str),
        at: At::new(node.line as usize),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 按键名
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use stele_core::{KeyCode, Modifiers, NamedKey};

    fn key_binder_of(text: &str) -> (Vec<KeyBinding>, Vec<Diagnostic>) {
        let root = stele_config::parse(text).expect("测试用的 YAML 必须能解析");
        let node = root.get("key_binder").expect("要有 key_binder 段");
        let mut diags = Vec::new();
        let out = read_key_bindings(node, &mut diags, "t.yaml");
        (out, diags)
    }

    #[test]
    fn one_binding_produces_exactly_one_effect() {
        // librime 的选择链：`send` 在前，`toggle` 在后，**只有第一个生效**。
        let (bindings, diags) =
            key_binder_of("key_binder:\n  bindings:\n    - {accept: a, send: space, toggle: x}\n");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].effect(), "send");
        assert!(bindings[0].toggle.is_none(), "被忽略的动作不该留在数据里");
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("只有 `send` 会生效")),
            "丢了一个动作必须报出来：{diags:?}"
        );
    }

    #[test]
    fn set_option_and_unset_option_are_their_own_actions() {
        let (b, d) = key_binder_of(
            "key_binder:\n  bindings:\n    - {accept: b, set_option: ascii_mode}\n    - {accept: c, unset_option: ascii_mode}\n",
        );
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(b[0].effect(), "set_option");
        assert_eq!(b[0].set_option.as_deref(), Some("ascii_mode"));
        assert_eq!(b[1].effect(), "unset_option");
        assert_eq!(b[1].unset_option.as_deref(), Some("ascii_mode"));
    }

    #[test]
    fn select_is_reported_as_unsupported_not_silently_dropped() {
        let (b, d) =
            key_binder_of("key_binder:\n  bindings:\n    - {accept: d, select: luna_pinyin}\n");
        assert!(b.is_empty());
        assert!(
            d.iter().any(|x| x.message.contains("尚未支持")),
            "不支持的动作品名要报出来：{d:?}"
        );
    }

    #[test]
    fn a_binding_without_any_action_is_dropped_with_a_reason() {
        let (b, d) = key_binder_of("key_binder:\n  bindings:\n    - {accept: e}\n");
        assert!(b.is_empty());
        assert!(
            d.iter().any(|x| x.message.contains("什么都不会做")),
            "{d:?}"
        );
    }

    #[test]
    fn key_names_round_trip_through_the_parser() {
        assert_eq!(
            parse_key_name("Control+BackSpace"),
            Some(KeyChord::new(
                stele_core::KeyCode::Named(stele_core::NamedKey::Backspace),
                Modifiers::CTRL
            ))
        );
        assert_eq!(
            parse_key_name("shift+tab"),
            Some(KeyChord::new(
                KeyCode::Named(NamedKey::Tab),
                Modifiers::SHIFT
            ))
        );
        assert_eq!(
            parse_key_name(","),
            Some(KeyChord::new(KeyCode::Char(','), Modifiers::NONE))
        );
        assert_eq!(parse_key_name("nonsense_key"), None);
    }

    #[test]
    fn send_names_stay_verbatim_for_the_engine_to_parse() {
        // 装载器**不解释**按键名：它只搬运。`space` 是键名，
        // 由引擎解析成空格键（而不是"一个空格字符"）。
        assert_eq!(
            parse_key_name("space"),
            Some(KeyChord::new(
                stele_core::KeyCode::Named(stele_core::NamedKey::Space),
                stele_core::Modifiers::NONE
            ))
        );
        // 单个字符**总是**被当成字符键——包括中文标点。
        // （`send: "，"` 在 RIME 里并不是合法写法，但我们接受它：
        // "上屏一个中文标点"用键名表达不了，而它在真实方案里很常见。）
        assert_eq!(
            parse_key_name("，"),
            Some(KeyChord::new(
                stele_core::KeyCode::Char('，'),
                stele_core::Modifiers::NONE
            ))
        );
        // 多字符且不是键名 → `None`，由 `key_binder` 当作"一段文本"逐字派发。
        assert_eq!(parse_key_name("dian"), None);
    }
}
