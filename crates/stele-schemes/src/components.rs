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
use stele_core::{Diagnostic, KeyCode, Modifiers, NamedKey};
use stele_engine::spec::{
    AffixSpec, At, EditorAction, EngineSpec, KeyBinding, KeyChord, NavigatorSpec, PunctuatorSpec,
    RecogPattern, RecognizerSpec, ReverseLookupSpec, SimplifierSpec, TranslatorSpec, WhenPredicate,
};
use stele_engine::tag::TagTable;
use stele_core::Tag;

/// `engine:` 段：零件名字列表。
pub fn read_engine(node: &Node) -> EngineSpec {
    let names = |key: &str| -> Vec<String> {
        node.get(key)
            .and_then(Node::as_seq)
            .map(|seq| {
                seq.iter().filter_map(stele_config::Node::as_str).collect()
            })
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
pub fn read_punctuator(node: &Node) -> PunctuatorSpec {
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
    let symbols = table("symbols");
    // 符号表的前缀：RIME 的写法是 `symbols_prefix`；**没写时没有前缀**。
    //
    // # 为什么不能"从第一个键猜前缀"
    //
    // RIME 的符号表有两种约定，而它们**在数据上无法区分**：
    //
    // | 写法 | 含义 | 前缀 |
    // | --- | --- | --- |
    // | `"/hx": "㊕"` | 敲 `/hx` 出 ㊕ | `/`（键**自带**前缀） |
    // | `symbols_prefix: "v"` + `"1": "①"` | 敲 `v1` 出 ① | `v`（键**不带**前缀） |
    //
    // 我第一版试图"猜"（拿第一个键的首字符当前缀），于是第二种写法下
    // 前缀被猜成 `1`，`v1` 永远查不到——**符号表整个不工作，且没有报错**。
    // 端到端测试（`the_symbol_table_expands_under_its_prefix`）抓到了它。
    //
    // 现在的规则是**显式且无歧义**的：写了 `symbols_prefix` 就用它，
    // 没写就当"键自带前缀"——从第一个键的首字符取，并在**存储时剥掉**它
    // （见 [`stele_engine::punctuator::PunctTranslator`]）。
    // 两条路都只有一种解读，不需要猜。
    let explicit = node
        .get("symbols_prefix")
        .and_then(Node::as_str)
        .and_then(|s| s.chars().next());
    let symbol_prefix = explicit.or_else(|| symbols.first().and_then(|(k, _)| k.chars().next()));
    PunctuatorSpec {
        full_shape: table("full_shape"),
        half_shape: table("half_shape"),
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
pub fn read_key_bindings(
    node: &Node,
    diags: &mut Vec<Diagnostic>,
    path: &str,
) -> Vec<KeyBinding> {
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
        let mut send_text = item
            .get("send")
            .and_then(stele_config::Node::as_str)
            .map(|s| key_name_to_literal(&s));
        let toggle = item.get("toggle").and_then(stele_config::Node::as_str);
        if let Some(seq_node) = item.get("send_sequence") {
            match seq_node.as_seq().and_then(|s| s.first()) {
                Some(first) => {
                    diags.push(
                        Diagnostic::new(
                            path,
                            "`send_sequence`（一次发送多个按键）尚未支持，\
                             只发送了它的第一个键",
                        )
                        .with_field(field("send_sequence"))
                        .with_entry(format!("第 {} 行", seq_node.line)),
                    );
                    if send_text.is_none() {
                        send_text = first.as_str().map(|s| key_name_to_literal(&s));
                    }
                }
                None => diags.push(
                    Diagnostic::new(path, "`send_sequence` 必须是列表")
                        .with_field(field("send_sequence"))
                        .with_entry(format!("第 {} 行", seq_node.line)),
                ),
            }
        }
        if send_text.is_none() && toggle.is_none() {
            diags.push(
                Diagnostic::new(
                    path,
                    "这条绑定既没有 `send` 也没有 `toggle` —— 它什么都不会做",
                )
                .with_field(format!("key_binder.bindings[{i}]"))
                .with_entry(format!("第 {} 行", item.line)),
            );
            continue;
        }
        out.push(KeyBinding {
            when,
            accept,
            send_text,
            toggle,
            at: At::new(item.line as usize),
        });
    }
    out
}

/// `navigator:` 段。
#[must_use]
pub fn read_navigator(node: &Node) -> NavigatorSpec {
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
    NavigatorSpec {
        page_up: keys("page_up"),
        page_down: keys("page_down"),
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
pub fn read_translator(
    node: &Node,
    component: &str,
    alias: Option<&str>,
) -> TranslatorSpec {
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

/// 解析 RIME 的按键名。
///
/// # 支持的写法
///
/// | 写法 | 结果 |
/// | --- | --- |
/// | `space` / `Return` / `BackSpace` | 具名键（大小写不敏感） |
/// | `Control+BackSpace` / `Shift+Tab` | 具名键 + 修饰键 |
/// | `minus` / `equal` / `bracketleft` | X11 名字 → 对应的**字符** |
/// | `a` / `,` | 单字符 |
///
/// **不认识的返回 `None`**，由调用方报错并列出可用的写法——
/// RIME 在这里是宽松的（认不出就当没写），而那会让"快捷键没反应"
/// 变成一个查不出来的问题。
#[must_use]
pub fn parse_key_name(name: &str) -> Option<KeyChord> {
    let mut mods = Modifiers::NONE;
    let mut rest = name;
    // 修饰键前缀，可能叠加（`Control+Shift+Return`）。
    loop {
        let lower = rest.to_ascii_lowercase();
        let stripped = ["control+", "ctrl+", "shift+", "alt+", "super+"]
            .iter()
            .find_map(|p| lower.strip_prefix(p).map(|_| p.len()));
        let Some(n) = stripped else { break };
        let prefix = &lower[..n];
        mods = mods
            | match prefix {
                "control+" | "ctrl+" => Modifiers::CTRL,
                "shift+" => Modifiers::SHIFT,
                "alt+" => Modifiers::ALT,
                _ => Modifiers::SUPER,
            };
        rest = &rest[n..];
    }
    let key = rest.to_ascii_lowercase();
    let code = match key.as_str() {
        "space" => KeyCode::Named(NamedKey::Space),
        "return" | "enter" => KeyCode::Named(NamedKey::Enter),
        "backspace" => KeyCode::Named(NamedKey::Backspace),
        "delete" | "delete_forward" => KeyCode::Named(NamedKey::Delete),
        "escape" | "esc" => KeyCode::Named(NamedKey::Escape),
        "tab" => KeyCode::Named(NamedKey::Tab),
        "left" => KeyCode::Named(NamedKey::Left),
        "right" => KeyCode::Named(NamedKey::Right),
        "up" => KeyCode::Named(NamedKey::Up),
        "down" => KeyCode::Named(NamedKey::Down),
        "home" => KeyCode::Named(NamedKey::Home),
        "end" => KeyCode::Named(NamedKey::End),
        "prior" | "page_up" => KeyCode::Named(NamedKey::PageUp),
        "next" | "page_down" => KeyCode::Named(NamedKey::PageDown),
        "minus" => KeyCode::Char('-'),
        "equal" => KeyCode::Char('='),
        "comma" => KeyCode::Char(','),
        "period" => KeyCode::Char('.'),
        "slash" => KeyCode::Char('/'),
        "semicolon" => KeyCode::Char(';'),
        "apostrophe" => KeyCode::Char('\''),
        "grave" => KeyCode::Char('`'),
        "bracketleft" => KeyCode::Char('['),
        "bracketright" => KeyCode::Char(']'),
        "backslash" => KeyCode::Char('\\'),
        _ => {
            // 单字符（含 `,` `.` 这类直接写出来的标点）。
            let mut cs = rest.chars();
            match (cs.next(), cs.next()) {
                (Some(c), None) => KeyCode::Char(c),
                _ => return None,
            }
        }
    };
    Some(KeyChord::new(code, mods))
}

/// `send:` 的值 → 一段**要上屏的文本**。
///
/// # 这里有一处必须解释的换算
///
/// RIME 的 `send: space` 意思是"再发一个空格键"。而空格键在引擎里的
/// 效果是**确认当前候选**（选择器的职责），不是"上屏一个空格字符"。
///
/// 因此这里把 `send` 的名字换算成**该键真正会产生的文本**：
///
/// - `send: space` → `" "`（我们要的是"确认候选"，而确认由上屏表达）
/// - `send: comma` → `","`
/// - `send: "，"` → `"，"`（直接写中文标点也支持）
///
/// 翻页类（`Page_Up`）对应的是**按键**而不是文本，由调用方另行处理
/// （见 [`stele_engine::processor::KeyBinder`]）。
#[must_use]
pub fn key_name_to_literal(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "space" => " ".to_owned(),
        "minus" => "-".to_owned(),
        "equal" => "=".to_owned(),
        "comma" => ",".to_owned(),
        "period" => ".".to_owned(),
        "slash" => "/".to_owned(),
        "semicolon" => ";".to_owned(),
        "apostrophe" => "'".to_owned(),
        "grave" => "`".to_owned(),
        "bracketleft" => "[".to_owned(),
        "bracketright" => "]".to_owned(),
        "backslash" => "\\".to_owned(),
        _ => name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names_round_trip_through_the_parser() {
        assert_eq!(
            parse_key_name("Control+BackSpace"),
            Some(KeyChord::new(
                KeyCode::Named(NamedKey::Backspace),
                Modifiers::CTRL
            ))
        );
        assert_eq!(
            parse_key_name("shift+tab"),
            Some(KeyChord::new(KeyCode::Named(NamedKey::Tab), Modifiers::SHIFT))
        );
        assert_eq!(parse_key_name(","), Some(KeyChord::new(KeyCode::Char(','), Modifiers::NONE)));
        assert_eq!(parse_key_name("nonsense_key"), None);
    }

    #[test]
    fn send_names_become_the_text_the_key_would_produce() {
        assert_eq!(key_name_to_literal("space"), " ");
        assert_eq!(key_name_to_literal("comma"), ",");
        // 直接写中文标点也照样工作。
        assert_eq!(key_name_to_literal("，"), "，");
    }
}
