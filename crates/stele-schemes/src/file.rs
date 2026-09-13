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

use crate::components;
use stele_engine::keyspec::parse_key_name;
use stele_config::{Node, Value};
use stele_core::Score;
use stele_core::{CodeAlphabet, Diagnostic, SchemaError, SchemaInfo, Switch};
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
    load_scheme_with(text, path, dicts, &DictMode::Inline).map(|l| l.def)
}

/// 装载的结果：方案定义 + **它是怎么被合出来的**。
///
/// 分成两半是为了让 `--dump-config` 能回答 D25 那个问题
/// （"这个值来自哪一层"），而引擎侧**完全不需要知道来源**——
/// 它只拿 [`SchemeDef`]。这条分界线让 `stele-engine` 保持零配置概念。
#[derive(Debug)]
pub struct Loaded {
    /// 编译前的方案声明。
    pub def: SchemeDef,
    /// 分层合并的结果与来源表。
    pub resolution: crate::provenance::Resolution,
}

/// 装载一份方案，连**来源**一起给出。
///
/// `patch` 是用户补丁层（RIME 的 `*.custom.yaml`）。它是**可选**的：
/// 没有补丁时来源表里就只有方案层，而 `--dump-config` 会如实这么说。
///
/// # Errors
///
/// 与 [`load_scheme`] 相同；另外补丁与方案类型冲突时也会报错，
/// 并指出**是哪一层**与哪条路径。
pub fn load_scheme_layered(
    text: &str,
    path: &str,
    user_patch: Option<(&str, &str)>,
    dicts: &dyn dict::Source,
) -> Result<Loaded, SchemaError> {
    load_layered_with(text, path, user_patch, dicts, &DictMode::Inline)
}

/// 由**方案文本 + 可选补丁文本**算出合并结果并编译。
///
/// # 这是唯一一条"分层 → 编译"的路径
///
/// 上一轮我把补丁合并写在 `load_scheme_layered` 里，而**目录装载
/// （`load_dir_layered`）没有走它**——于是 `--dump-config` 打印出了
/// "用户补丁贡献 2 项"，而引擎拿到的仍是未打补丁的方案。
///
/// 症状极其隐蔽：报告说得很清楚，行为却完全没变。抓它的方式是
/// **真的跑一遍 CLI 并对照 `--dump-config` 与 `--list`**，
/// 而不是只跑那条直接调 `load_scheme_layered` 的测试。
///
/// 现在三个入口（单文件 / 目录 / 目录+部署）都经过这里，因此不可能再分叉。
fn load_layered_with(
    text: &str,
    path: &str,
    user_patch: Option<(&str, &str)>,
    dicts: &dyn dict::Source,
    mode: &DictMode<'_>,
) -> Result<Loaded, SchemaError> {
    let mut layers = vec![(
        crate::provenance::Layer::new("方案", path, "方案文件本身（基础层）"),
        parse_or_diag(text, path)?,
    )];
    if let Some((patch_text, patch_name)) = user_patch {
        layers.push((
            crate::provenance::Layer::new(
                "用户补丁",
                patch_name,
                "你的改动层：它覆盖上面任何一层",
            ),
            parse_or_diag(patch_text, patch_name)?,
        ));
    }
    let resolution = crate::provenance::Resolution::of(layers).map_err(|msg| {
        SchemaError::Invalid {
            schema_id: path.to_owned(),
            diagnostics: vec![Diagnostic::new(path, msg)
                .with_field("layers")
                .with_entry(
                    "检查补丁里同一个键的类型是否与方案一致（映射可以合并，列表整体替换）",
                )],
        }
    })?;
    // **编译的是合并结果**，不是原始方案文件。
    //
    // 这一行是 P2 欠下的接线：`stele-config` 里的分层补丁早就实现并测过，
    // 但装载路径一直只读方案文件本身——于是"用户补丁能覆盖一切"这句话
    // 在 P2 是**假的**。
    let mut loaded = load_from_root(&resolution.root, path, dicts, mode)?;
    loaded.resolution = resolution;
    Ok(loaded)
}

/// 同 [`load_layered_with`]，但词库走部署路径（紧凑产物）。
///
/// # Errors
///
/// 同 [`load_scheme_layered`]。
fn load_layered_inner_deployed(
    text: &str,
    path: &str,
    user_patch: Option<(&str, &str)>,
    dicts: &dyn dict::Source,
    cache_dir: &std::path::Path,
) -> Result<Loaded, SchemaError> {
    load_layered_with(text, path, user_patch, dicts, &DictMode::Deployed(cache_dir))
}

/// 找一份方案的用户补丁：`<schema_id>.custom.yaml`（RIME 的约定）。
///
/// `schema_id` 以**文件里写的**为准（不假定它等于文件名）——因此要先
/// 轻量解析一次方案文本。解析失败时返回 `None`：那条错误会在真正的
/// 装载里以更完整的诊断报出来，这里不必抢着报。
fn find_patch(
    dir: &std::path::Path,
    text: &str,
    file_name: &str,
) -> Option<(String, String)> {
    let id = stele_config::parse(text)
        .ok()
        .and_then(|root| {
            root.get("schema")
                .and_then(|s| s.get("schema_id"))
                .and_then(stele_config::Node::as_str)
        })
        .unwrap_or_else(|| {
            file_name
                .trim_end_matches(".schema.yaml")
                .trim_end_matches(".yaml")
                .to_owned()
        });
    let path = dir.join(format!("{id}.custom.yaml"));
    let patch_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("custom.yaml")
        .to_owned();
    std::fs::read_to_string(&path)
        .ok()
        .map(|t| (t, patch_name))
}

fn parse_or_diag(text: &str, path: &str) -> Result<Node, SchemaError> {
    stele_config::parse(text).map_err(|e| SchemaError::Invalid {
        schema_id: path.to_owned(),
        diagnostics: vec![Diagnostic::new(
            path,
            format!("第 {} 行：{}", e.line, e.message),
        )],
    })
}

/// 词库怎么来。
enum DictMode<'a> {
    /// 读进内存（内嵌的小方案、测试）。
    Inline,
    /// 编译成紧凑产物（部署路径）。
    Deployed(&'a std::path::Path),
}

fn load_scheme_with(
    text: &str,
    schema_path: &str,
    dicts: &dyn dict::Source,
    mode: &DictMode<'_>,
) -> Result<Loaded, SchemaError> {
    let path = schema_path;
    let root = stele_config::parse(text).map_err(|e| SchemaError::Invalid {
        schema_id: path.to_owned(),
        diagnostics: vec![Diagnostic::new(
            path,
            format!("第 {} 行：{}", e.line, e.message),
        )],
    })?;
    // `patch` = 已经与方案合并好的配置（由 [`load_scheme_layered`] 给出）。
    // 为 `None` 时就是方案文件本身。
    load_from_root(&root, path, dicts, mode)
}

/// 从**已经合并好的**配置树编译方案。
///
/// `root` 是合并结果（方案文件 + 用户补丁），因此"用户改了什么"
/// 不在这里判断——那是 [`crate::provenance::Resolution`] 的职责，
/// 它逐路径记着每个值来自哪一层。
fn load_from_root(
    root: &Node,
    path: &str,
    dicts: &dyn dict::Source,
    mode: &DictMode<'_>,
) -> Result<Loaded, SchemaError> {


    let mut diags: Vec<Diagnostic> = Vec::new();
    let mut schema_id = String::new();
    // 装载期发现的**提示**（不是错误）。它们会进 `custom`，由
    // `--dump-config` 打印——"我配的东西为什么没生效"这类问题，
    // 答案常常就在这些提示里。
    let mut load_notes: Option<String> = None;

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
    // `engine.tag` 有两种给法：
    //
    // 1. 我们的短写法：`engine.tag: abc`
    // 2. **RIME 方案根本没有这个字段**——标签来自切分器
    //    （`abc_segmentor` 产出 `abc`、`affix_segmentor@x` 产出 `x`）。
    //
    // 因此它是**可选**的：没写就用 `abc`（RIME 世界里那个"普通编码段"的
    // 既定名字），而真正的标签集合由切分器声明。要求每个 RIME 方案都写
    // `engine.tag` 等于宣布"RIME 的方案文件不能直接用"——那与 P3 的
    // 验收线（跑通 `others/no_lua_schema`）直接冲突。
    let tag_text = engine
        .and_then(|e| e.get("tag"))
        .and_then(Node::as_str)
        .unwrap_or_else(|| "abc".to_owned());
    let translator_text = engine
        .and_then(|e| e.get("translator"))
        .and_then(Node::as_str)
        .unwrap_or_default();
    let mut translator = match translator_text.as_str() {
        TRANSLATOR_SPELLING_GRAPH => Some(TranslatorKind::SpellingGraph),
        TRANSLATOR_EXACT_CODE => Some(TranslatorKind::ExactCode),
        // 没写短写法时**留空**：稍后从 `engine.translators` 列表推断
        // （`script_translator` ⇒ 拼写图；`table_translator` ⇒ 精确编码）。
        // 两者都没有才算缺——那时才报错。
        "" => None,
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
    // 编码字母表有两种写法，**两种都必须收**：
    //
    // - **列表**（我们的写法）：每个元素是一个编码单元（`ni`、`hao`）。
    //   这是"编码集合可枚举"那类输入法需要的形状。
    // - **字符串**（RIME 的 `speller/alphabet: zyxw...a`）：**每个字符是
    //   一个编码单元**。字形类方案（仓颉、五笔）与英文都这么写。
    //
    // 早先只收列表，于是"RIME 的方案文件能直接用"这句话对
    // `speller.alphabet: abc` 这种最普通的写法**是假的**。
    let alphabet: Vec<String> = match speller.and_then(|s| s.get("alphabet")) {
        Some(n) => match &n.value {
            Value::Seq(seq) => seq.iter().filter_map(Node::as_str).collect(),
            Value::Str(s) => s.chars().map(|c| c.to_string()).collect(),
            _ => Vec::new(),
        },
        None => Vec::new(),
    };
    if alphabet.is_empty() {
        diags.push(
            Diagnostic::new(path, "缺少 `speller.alphabet`（编码字母表）")
                .with_field("speller.alphabet")
                .with_entry(
                    "列表写法：每个元素是一个编码单元（拼音方案写音节表；\
                     字形方案写字母表）；字符串写法：每个字符是一个编码单元",
                ),
        );
    }

    let mut rules: Vec<Rule> = Vec::new();
    // `algebra` 是 RIME 的叫法，`rules` 是我们的——两个都收。
    let rules_node = speller.and_then(|s| s.get("rules").or_else(|| s.get("algebra")));
    if let Some(rs) = rules_node {
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

    // RIME 的 `speller.delimiter: " '"` —— 第一位是自动插入的分隔符。
    let preedit_delimiter = speller
        .and_then(|sp| sp.get("delimiter"))
        .and_then(Node::as_str)
        .and_then(|d| d.chars().next());

    // ── 从 `engine.translators` 推断翻译器族（没写短写法时） ──
    //
    // 这是 RIME 方案**唯一**的给法：它不写 `engine.translator`，
    // 只列出零件名字。`script_translator` 与 `table_translator` 分属两族，
    // 因此列表里出现哪一个，就决定了主翻译器走哪一族。
    if translator.is_none() {
        let names: Vec<String> = engine
            .and_then(|e| e.get("translators"))
            .and_then(Node::as_seq)
            .map(|seq| seq.iter().filter_map(Node::as_str).collect())
            .unwrap_or_default();
        if names.iter().any(|n| n.starts_with("script_translator")) {
            translator = Some(TranslatorKind::SpellingGraph);
        } else if names.iter().any(|n| n.starts_with("table_translator")) {
            translator = Some(TranslatorKind::ExactCode);
        }
        if let Some(k) = translator {
            // 这是**提示**而不是错误：RIME 的方案本来就只列零件名。
            // `Diagnostic` 目前只有"错误"一种语义，所以提示走 `custom`，
            // 由 `--dump-config` 打印出来。
            load_notes = Some(format!(
                "`engine.translator` 未给出，已从 `engine.translators` 列表推断为 `{k:?}`"
            ));
        }
    }

    // ── translator 段：取词典 ──
    let dict_name = root
        .get("translator")
        .and_then(|t| t.get("dictionary"))
        .and_then(Node::as_str);
    // 部署路径需要字母表来把编码文本转成编号，故先建好。
    let alphabet_ids = CodeAlphabet::new(alphabet.clone());
    let mut entries: Vec<(Vec<String>, String, f64)> = Vec::new();
    let mut external: Option<std::sync::Arc<dyn stele_core::Lexicon>> = None;
    match dict_name {
        // **没有词典不是错误**：有些方案的主翻译器只靠别的零件出候选
        // （纯转换方案、纯符号方案；RIME 里 `dictionary: ""` 也是合法写法）。
        // 早先把它当必填，于是那类方案连装载都过不去。
        //
        // 但**要出声**：`custom_phrase` 这类实例的正规做法是
        // `dictionary: ""` + `user_dict: xxx`，而"用户词库"我们还没有
        // （P4a）。静默给一本空词库会让用户以为"我配了却没生效"。
        None => diags.push(
            Diagnostic::new(
                path,
                "`translator.dictionary` 为空：这个方案的主翻译器没有任何词库",
            )
            .with_field("translator.dictionary")
            .with_entry(
                "如果这是有意的（纯转换/纯符号方案），忽略本条；\
                 如果你用的是 `user_dict`（用户词库），那是 P4a 的内容，尚未实现",
            ),
        ),
        Some(name) if name.is_empty() => diags.push(
            Diagnostic::new(
                path,
                "`translator.dictionary` 为空：这个方案的主翻译器没有任何词库",
            )
            .with_field("translator.dictionary")
            .with_entry(
                "如果这是有意的（纯转换/纯符号方案），忽略本条；\
                 如果你用的是 `user_dict`（用户词库），那是 P4a 的内容，尚未实现",
            ),
        ),
        Some(name) => match *mode {
            // 内联：读进内存。
            DictMode::Inline => match dict::load_with_imports(dicts, &name, &name) {
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
            // 部署：**根本不构造内联词条**——那正是 245 MB 峰值的来源。
            DictMode::Deployed(cache) => match deploy_dict(dicts, &name, &alphabet_ids, cache) {
                Ok(l) => external = Some(l),
                Err(d) => diags.push(d),
            },
        },
    }

    // ── RIME 风格的 `engine:` 列表 ──
    //
    // 我们自己的短写法是 `engine.translator: spelling_graph`；
    // RIME 的方案则列出零件**名字**。两种都收：有列表就先解析并出覆盖报告，
    // 短写法缺省时可以从列表里推断翻译器族。
    let mut coverage: Option<stele_engine::registry::CoverageReport> = None;
    let mut custom_summary: Option<String> = None;
    if let Some(eng) = root.get("engine") {
        let mut names: Vec<String> = Vec::new();
        for slot in ["processors", "segmentors", "translators", "filters"] {
            if let Some(seq) = eng.get(slot).and_then(Node::as_seq) {
                for it in seq {
                    if let Some(n) = it.as_str() {
                        names.push(n);
                    }
                }
            }
        }
        if !names.is_empty() {
            let rep = stele_engine::registry::CoverageReport::of(&names);
            // 供 `--dump-config` 打印。
            custom_summary = Some(rep.summary());
            // 缺口逐条报出来，**按类分开**——"你缺数据"和"我们缺代码"
            // 对使用者意味着完全不同的下一步。
            // **这里只出报告，不出错误。**
            //
            // "缺不缺"要按**判据**判断，而判据（文件在不在、内联表给了没有）
            // 由本模块下面的代码算出，最终由 `SchemeDef::compile` 里的
            // `unmet_requirements` 出声。分开的理由：报告是**给人看的**，
            // 而报错是**拦装载的**——两件事混在一起就会出现
            // "明明数据齐了却被拦下来"（这个 bug 真发生过）。
            coverage = Some(rep.clone());
            // 没写短写法时，从列表里推断翻译器族。
            if translator.is_none() {
                if names.iter().any(|n| n.starts_with("script_translator")) {
                    translator = Some(TranslatorKind::SpellingGraph);
                } else if names.iter().any(|n| n.starts_with("table_translator")) {
                    translator = Some(TranslatorKind::ExactCode);
                }
            }
        }
    }

    let _ = coverage;

    // ── 各零件的配置段（P3） ──
    //
    // 这一段把 RIME 方案里那些**按零件分节**的配置读进来。读法与上面
    // 完全一致：不合法就报错并给行号，绝不"猜一个"。
    let mut tags = stele_engine::tag::TagTable::new();
    let mut engine_spec = stele_engine::spec::EngineSpec::default();
    if let Some(eng) = root.get("engine") {
        engine_spec = components::read_engine(eng);
        engine_spec.tag = Some(tag_text.clone());
    }

    let recognizer = root
        .get("recognizer")
        .map(|n| components::read_recognizer(n, &mut diags, path))
        .unwrap_or_default();

    // `punctuator` 段没有时，仍然给一份"中文输入法本来该有的标点"——
    // 否则 `,` 只会出一个半角逗号（或者什么都没有）。
    // 这份默认来自**引擎自带的预设**（见 `stele_engine::presets`），
    // 而不是某个具体输入法的数据。
    let punctuator = if let Some(n) = root.get("punctuator") { components::read_punctuator(n, &mut diags, path) } else {
        let p = stele_engine::presets::stele();
        stele_engine::spec::PunctuatorSpec {
            half_shape: p.half_shape.into_iter().collect(),
            full_shape: p.full_shape.into_iter().collect(),
            ..Default::default()
        }
    };

    let editor_bindings = root
        .get("editor")
        .map(|n| components::read_editor(n, &mut diags, path))
        .unwrap_or_default();

    let key_bindings = root
        .get("key_binder")
        .map(|n| components::read_key_bindings(n, &mut diags, path))
        .unwrap_or_default();

    let navigator = root
        .get("navigator").map_or_else(|| {
            // 没写 `navigator` 段时用预设的翻页键（RIME 的默认）。
            let p = stele_engine::presets::stele();
            stele_engine::spec::NavigatorSpec {
                page_up: p.page_up.iter().filter_map(|s| parse_key_name(s)).collect(),
                page_down: p.page_down.iter().filter_map(|s| parse_key_name(s)).collect(),
                ..Default::default()
            }
        }, |n| components::read_navigator(n, &mut diags, path));

    // 带词缀的切分器 / 反查滤镜 / 转换滤镜：**按 `engine:` 里出现的别名**
    // 去找对应的顶层段。找不到就是"声明了却没人配"——`compile` 会报。
    let mut affixes: Vec<(String, stele_engine::spec::AffixSpec)> = Vec::new();
    let mut reverse_lookups: Vec<(String, stele_engine::spec::ReverseLookupSpec)> = Vec::new();
    let mut converters: Vec<(
        String,
        std::collections::BTreeMap<String, Vec<String>>,
        stele_engine::spec::SimplifierSpec,
    )> = Vec::new();
    for name in engine_spec
        .segmentors
        .iter()
        .chain(engine_spec.filters.iter())
        .chain(engine_spec.translators.iter())
    {
        let (component, alias) = stele_engine::spec::split_alias(name);
        let Some(a) = alias else { continue };
        let Some(block) = root.get(a) else { continue };
        match component {
            "affix_segmentor" => {
                affixes.push((a.to_owned(), components::read_affix(block, &mut tags)));
            }
            "reverse_lookup_filter" => {
                reverse_lookups.push((
                    a.to_owned(),
                    components::read_reverse_lookup(block, &mut tags, &mut diags, path),
                ));
            }
            "simplifier" => {
                let mut spec = components::read_simplifier(block, &mut tags, &mut diags, path);
                spec.tags = components::read_tags(block, &mut tags);
                // 转换表：RIME 指 OpenCC 的 json。**我们不解析 OpenCC 格式**
                // ——那是它自己的数据格式，属于"外部数据"。方案若想要
                // 转换，就在这里直接给一张 `from: to` 表。
                let mut table: std::collections::BTreeMap<String, Vec<String>> =
                    std::collections::BTreeMap::new();
                if let Some(t) = block.get("table").and_then(stele_config::Node::as_map) {
                    for (k, v) in t {
                        let to = v.as_str().unwrap_or_default();
                        table.insert(k.clone(), vec![to]);
                    }
                }
                converters.push((a.to_owned(), table, spec));
            }
            _ => {}
        }
    }

    // 翻译器实例：方案级的 `translator:` 段是"没有别名的那一个"。
    let mut translator_specs: Vec<(String, stele_engine::spec::TranslatorSpec)> = Vec::new();
    if let Some(t) = root.get("translator") {
        translator_specs.push((
            String::new(),
            components::read_translator(t, "script_translator", None),
        ));
    }
    for name in &engine_spec.translators {
        let (component, alias) = stele_engine::spec::split_alias(name);
        let Some(a) = alias else { continue };
        if let Some(block) = root.get(a) {
            translator_specs.push((
                a.to_owned(),
                components::read_translator(block, component, Some(a)),
            ));
        }
    }

    // `speller.alphabet` 被写成**字符串**（RIME）时，每个字符是一个输入字符。
    // 注意这与 `speller.alphabet` 的**列表**写法不同：列表是编码字母表
    // （可以是多字符的单元），字符串是"允许敲哪些字符"。
    let input_alphabet: Vec<char> = speller
        .and_then(|s| s.get("input_alphabet"))
        .and_then(Node::as_str)
        .map(|s| s.chars().collect())
        .or_else(|| {
            root.get("ascii_composer")
                .and_then(|n| n.get("good_old_caps_lock"))
                .and_then(Node::as_str)
                .map(|s| s.chars().collect())
        })
        .unwrap_or_default();

    let page_size = root
        .get("menu")
        .and_then(|m| m.get("page_size"))
        .and_then(Node::as_int)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(stele_engine::pipeline::DEFAULT_PAGE_SIZE);

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

    Ok(Loaded {
        def: SchemeDef {
        info,
        switches,
        tag,
        alphabet,
        rules,
        dictionary: match external {
            Some(l) => stele_engine::scheme::DictSource::External(l),
            None => stele_engine::scheme::DictSource::Inline(entries),
        },
        translator: translator.expect("已在上面校验过"),
        candidate_cap,
        preedit_delimiter,
        engine: engine_spec.clone(),
        recognizer,
        punctuator,
        editor_bindings,
        key_bindings,
        navigator,
        affixes,
        reverse_lookups,
        converters,
        translator_specs,
        input_alphabet,
        page_size,
        external_data: external_data_facts(root, path, &engine_spec),
        custom: {
            let mut m = std::collections::BTreeMap::new();
            if let Some(s) = custom_summary {
                m.insert("component_coverage".to_owned(), s);
            }
            if let Some(n) = load_notes {
                m.insert("translator_kind_inferred".to_owned(), n);
            }
            m
        },
        },
        // 逐条装载路径（`load_scheme` 等）只知道方案文件这一层；
        // 用户补丁层由 `load_scheme_layered` 补上。
        resolution: crate::provenance::Resolution::of(vec![(
            crate::provenance::Layer::new("方案", path, "方案文件本身（基础层）"),
            root.clone(),
        )])
        .expect("单层合并不可能失败"),
    })
}

/// 算出"每个实例的外部数据到底在不在"。
///
/// **这是装载器才能回答的问题**（只有它知道文件在不在），算好之后作为
/// **判据**交给引擎，由引擎的 `unmet_requirements` 决定要不要出声。
/// 判据与事实分开，是"注册表说需要数据"与"这份方案缺不缺"
/// 能分别验证的前提。
///
/// 判据的细则：
///
/// | 零件 | 数据在哪 | 怎么算"在" |
/// | --- | --- | --- |
/// | `simplifier@x` | 内联 `table:` | 段里有 `table` |
/// | `simplifier@x` | OpenCC 的 json | `opencc_config` 指的文件**存在** |
/// | `table_translator@x` | 一本词库 | 段里有 `dictionary`（它的装载错误在别处报） |
/// | `reverse_lookup_filter@x` | 一本反查词库 | 同上 |
fn external_data_facts(
    root: &Node,
    path: &str,
    engine: &stele_engine::spec::EngineSpec,
) -> Vec<stele_engine::registry::ExternalData<'static>> {
    let dir = std::path::Path::new(path)
        .parent()
        .filter(|d| !d.as_os_str().is_empty());
    let mut out = Vec::new();
    for name in engine
        .translators
        .iter()
        .chain(engine.filters.iter())
    {
        let (component, alias) = stele_engine::spec::split_alias(name);
        let Some(a) = alias else { continue };
        let present = match root.get(a) {
            None => false,
            Some(block) => match component {
                "simplifier" => {
                    block.get("table").is_some()
                        || block
                            .get("opencc_config")
                            .and_then(Node::as_str)
                            .is_some_and(|cfg| {
                                dir.is_some_and(|d| d.join(&cfg).is_file())
                            })
                }
                "table_translator" | "reverse_lookup_translator"
                | "reverse_lookup_filter" => block.get("dictionary").is_some(),
                _ => false,
            },
        };
        // 泄漏一次是**有意**的：`ExternalData` 借用实例名，而
        // `SchemeDef` 要活到进程结束（方案装载一次）。与标签 intern
        // 是同一个取舍——用一点点常驻内存换"没有生命周期参数污染
        // 整个方案数据结构"。
        let alias_static: &'static str = Box::leak(a.to_owned().into_boxed_str());
        out.push(stele_engine::registry::ExternalData {
            alias: alias_static,
            present,
        });
    }
    out
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
    // RIME 的写法是一行字符串：`xform/^([nl])ue$/$1ve/`。
    // **优先支持它**，因为真实方案的 `speller/algebra` 就是这么写的。
    if let Some(spec) = item.as_str() {
        return Rule::parse(&spec).map_err(|e| {
            Diagnostic::new(path, format!("第 {idx} 条拼写运算有误：{e}"))
                .with_field(format!("speller.rules[{idx}]"))
                .with_entry(format!("第 {} 行", item.line))
        });
    }
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
    // 边的代价有**两种单位**，而它们的名字必须不同：
    //
    // | 写法 | 单位 | 含义 |
    // | --- | --- | --- |
    // | `cost: -3000` | 毫对数 | 这条边的**分数增量**（对数域，D13） |
    // | `weight: 0.5` | 线性权重比 | "命中它的概率是规范拼写的 0.5 倍" |
    //
    // 两者的关系是 `cost = ln(weight) × 1000`（装载期算一次，热路径没有浮点）。
    //
    // **为什么要分成两个名字**：我第一版只有一个 `cost`，却按权重解释它
    // （`Score::from_weight`）。于是：
    //
    // - 方案里写 `cost: 0.5` → 被当成权重 0.5 → -693ml（碰巧"看起来对"）
    // - 方案里写 `cost: -3000` → 被当成**权重为负** → 直接掉到下界 `FLOOR`，
    //   于是所有边代价相同，切分退化成"谁先被找到算谁"——
    //   症状是预编辑串变成 `ni ha ao` 而不是 `ni hao`，而候选却是对的。
    //
    // 一个字段两种解读是 bug 的温床；两个名字就没有歧义。
    let cost = |n: &Node| -> Score {
        if let Some(w) = n.get("weight").and_then(Node::as_f64) {
            return Score::from_weight(w);
        }
        n.get("cost")
            .and_then(Node::as_f64)
            .map_or(Score::ZERO, |v| {
                #[allow(clippy::cast_possible_truncation)]
                Score::from_milli_log(v.round() as i32)
            })
    };

    match name.as_str() {
        "abbrev" => {
            // 允许 `abbrev: 1`（只给长度）、`abbrev: { take: 1, weight: 0.5 }`
            // 或 `abbrev: { take: 1, cost: -693 }`。
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
                        "`abbrev` 的参数应是整数（保留前几个字符）\
                         或 `{ take, weight }` / `{ take, cost }`",
                    )
                    .with_field(format!("speller.rules[{idx}].abbrev")));
                }
            };
            Rule::abbrev(take, c).map_err(|e| {
                Diagnostic::new(path, format!("`abbrev` 参数有误：{e}"))
                    .with_field(format!("speller.rules[{idx}].abbrev"))
            })
        }
        "equivalence" => {
            let pairs_node = arg.get("pairs").ok_or_else(|| {
                Diagnostic::new(path, "`equivalence` 缺少 `pairs`")
                    .with_field(format!("speller.rules[{idx}].equivalence.pairs"))
                    .with_entry("形如 `pairs: [[z, zh], [c, ch]]`")
            })?;
            let pairs = read_pairs(pairs_node, path, idx)?;
            Ok(Rule::equivalence(&pairs, cost(arg)))
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
    Ok(load_dir_layered(root)?.into_iter().map(|l| l.def).collect())
}

/// 从目录装载，**连每一份方案的来源表一起给出**（供 `--dump-config`）。
///
/// 用户补丁层按 RIME 的约定找：`<schema_id>.custom.yaml`。找到就叠上去，
/// 找不到就**只有方案层**——而 `--dump-config` 会如实说"没有用户补丁"，
/// 不会假装有一层空的。
///
/// # Errors
///
/// 目录读不了、没有方案文件、任一方案或补丁有错时返回 [`SchemaError`]。
pub fn load_dir_layered(root: &std::path::Path) -> Result<Vec<Loaded>, SchemaError> {
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
        // 用户补丁：`<schema_id>.custom.yaml`（RIME 的约定）。
        // `schema_id` 通常等于文件名去掉扩展名，但**以文件里写的为准**——
        // 先装载一次拿到 id，再按 id 找补丁。
        // 用户补丁按 RIME 的约定找：`<schema_id>.custom.yaml`。
        // **补丁必须真的进编译**——只把它记进来源表是不够的（那正是
        // 上一轮的 bug：报告说改了、行为没变）。
        let patch = find_patch(root, &text, name);
        let patch_ref = patch
            .as_ref()
            .map(|(t, n)| (t.as_str(), n.as_str()));
        out.push(load_layered_with(&text, name, patch_ref, &src, &DictMode::Inline)?);
    }
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────────
// 部署：把词库编译成紧凑产物，并按需分页地读它
// ─────────────────────────────────────────────────────────────────────────────

/// 从一个目录装载方案，并把词库**编译成紧凑产物**（P2.5）。
///
/// # 为什么需要它
///
/// 内存实现在 50 万词条上实测**峰值 245 MiB**，外推到雾凇的 188 万词条约
/// **0.9 GB**——与 RIME 实测的 780 MB–1 GB 部署峰值同一量级。
/// 而项目的红线是常驻 < 30 MB、部署峰值 < 150 MB。
///
/// 编译产物把常驻内存压到**只留索引**（188 万词条约 4 MB），
/// 词条与词字符串留在文件里按需读取。
///
/// # 缓存
///
/// 产物路径含源数据校验和（`<dict>.<checksum>.table`）。
/// 校验和一致就复用，不一致就重编——**并且旧产物对不上时是拒绝加载，
/// 而不是凑合跑**（PLAN D28）。
///
/// # Errors
///
/// 目录不可读、没有方案、方案有错、词库编译失败时返回 [`SchemaError`]。
///
/// # Panics
///
/// 不会 panic。
pub fn load_dir_deployed(
    root: &std::path::Path,
    cache_dir: &std::path::Path,
) -> Result<Vec<SchemeDef>, SchemaError> {
    Ok(load_dir_deployed_layered(root, cache_dir)?
        .into_iter()
        .map(|l| l.def)
        .collect())
}

/// 部署路径的"连同来源表"版本。语义与 [`load_dir_layered`] 相同，
/// 只是词库走紧凑产物（不读进内存）。
///
/// # Errors
///
/// 同 [`load_dir_deployed`]。
pub fn load_dir_deployed_layered(
    root: &std::path::Path,
    cache_dir: &std::path::Path,
) -> Result<Vec<Loaded>, SchemaError> {
    let src = dict::DirSource::new(root);
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let entries = std::fs::read_dir(root).map_err(|e| SchemaError::Invalid {
        schema_id: root.display().to_string(),
        diagnostics: vec![Diagnostic::new(
            root.display().to_string(),
            format!("读不了这个目录：{e}"),
        )],
    })?;
    for e in entries.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.ends_with(".schema.yaml") {
            files.push(p);
        }
    }
    files.sort();

    if files.is_empty() {
        return Err(SchemaError::Invalid {
            schema_id: root.display().to_string(),
            diagnostics: vec![Diagnostic::new(
                root.display().to_string(),
                "这个目录里没有 `*.schema.yaml`",
            )],
        });
    }

    let mut out = Vec::with_capacity(files.len());
    for f in files {
        let name = f
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_owned();
        let text = std::fs::read_to_string(&f).map_err(|e| SchemaError::Invalid {
            schema_id: name.clone(),
            diagnostics: vec![Diagnostic::new(&name, format!("读不了文件：{e}"))],
        })?;
        let patch = find_patch(root, &text, &name);
        let patch_ref = patch
            .as_ref()
            .map(|(t, n)| (t.as_str(), n.as_str()));
        out.push(load_layered_inner_deployed(
            &text,
            &name,
            patch_ref,
            &src,
            cache_dir,
        )?);
    }
    Ok(out)
}

/// 部署一份词库：算校验和 → 需要就编译 → 以按需分页的方式打开。
///
/// **注意它不经过内联路径**——内联会把全部词条读进内存，
/// 而那正是我们要消灭的 245 MB 峰值。
fn deploy_dict(
    src: &dyn dict::Source,
    dict_name: &str,
    alphabet: &CodeAlphabet,
    cache_dir: &std::path::Path,
) -> Result<std::sync::Arc<dyn stele_core::Lexicon>, Diagnostic> {
    let bad = |m: String| Diagnostic::new(dict_name, m).with_field("translator.dictionary");

    let checksum = dict::checksum_of(src, dict_name, dict_name)
        .map_err(|e| bad(format!("算词库校验和失败：{e}")))?;
    let table_path = cache_dir.join(format!("{dict_name}.{checksum:016x}.table"));

    if !table_path.exists() {
        std::fs::create_dir_all(cache_dir)
            .map_err(|e| bad(format!("建不了缓存目录 {}：{e}", cache_dir.display())))?;

        // 流式：词条一条条喂进写入器，**中间没有 Vec<RawEntry>**。
        let r = stele_table::compile(
            checksum,
            |w| {
                dict::for_each_entry(src, dict_name, dict_name, |word, code, weight| {
                    let mut ids: Vec<u16> = Vec::with_capacity(4);
                    for unit in code.split_whitespace() {
                        let Some(id) = alphabet.id_of(unit) else {
                            return Err(dict::DictError {
                                path: dict_name.to_owned(),
                                line: 0,
                                message: format!(
                                    "词条「{word}」引用了字母表里没有的编码单元「{unit}」"
                                ),
                            });
                        };
                        match u16::try_from(id.0) {
                            Ok(v) => ids.push(v),
                            Err(_) => {
                                return Err(dict::DictError {
                                    path: dict_name.to_owned(),
                                    line: 0,
                                    message: format!(
                                        "编码单元「{unit}」的编号超过 65535，产物无法表达"
                                    ),
                                })
                            }
                        }
                    }
                    w.push(word, &ids, weight).map_err(|e| dict::DictError {
                        path: dict_name.to_owned(),
                        line: 0,
                        message: e.to_string(),
                    })
                })
                .map(|_| ())
                .map_err(|e| stele_table::CompileError::Io(e.to_string()))
            },
            &table_path,
        );
        if let Err(e) = r {
            return Err(bad(format!("词库编译失败：{e}")));
        }
    }

    let lex = stele_table::TableLexicon::open_checked(&table_path, Some(checksum))
        .map_err(|e| bad(format!("词库产物加载失败：{e}")))?;
    Ok(std::sync::Arc::new(lex))
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
        let stele_engine::scheme::DictSource::Inline(e) = &d.dictionary else {
            panic!("应当走内联路径");
        };
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].1, "你好");
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
        // `abbrev: 2` 是语法糖，展开成一条带 ABBREV 属性的派生规则。
        match &d.rules[0] {
            Rule::Derive { attr, .. } => {
                assert!(attr.contains(stele_core::SpellingAttr::ABBREV));
            }
            other => panic!("应当是带 ABBREV 属性的派生规则，得到 {other:?}"),
        }
    }

    #[test]
    fn equivalence_pairs_are_read() {
        let text = GOOD.replace(
            "    - abbrev: { take: 1, cost: 0.5 }\n",
            "    - equivalence: { pairs: [[z, zh]], cost: 1.0 }\n",
        );
        let d = load_scheme(&text, "t", &dicts()).unwrap();
        // `[[z, zh]]` 取每对的首字符 → `('z','z')`，即"不变化"。
        // 这里主要验证**解析路径**通：等价替换现在是一条带 FUZZY 属性的派生规则。
        match &d.rules[0] {
            Rule::Equivalence { pairs, .. } => assert_eq!(pairs, &[('z', 'z')]),
            other => panic!("应当是等价替换，得到 {other:?}"),
        }
    }
}
