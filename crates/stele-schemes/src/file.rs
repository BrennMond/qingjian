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
use stele_config::{Node, Value};
use stele_core::Score;
use stele_core::{CodeAlphabet, CodeUnitId, Diagnostic, SchemaError, SchemaInfo, Switch};
use stele_dict as dict;
use stele_engine::keyspec::parse_key_name;
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
    load_layered_with(text, path, user_patch, dicts, &DictMode::Inline, None)
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
    base_dir: Option<&std::path::Path>,
) -> Result<Loaded, SchemaError> {
    let mut layers = vec![(
        crate::provenance::Layer::new("方案", path, "方案文件本身（基础层）"),
        parse_or_diag(text, path)?,
    )];
    if let Some((patch_text, patch_name)) = user_patch {
        layers.push((
            crate::provenance::Layer::new("用户补丁", patch_name, "你的改动层：它覆盖上面任何一层"),
            parse_or_diag(patch_text, patch_name)?,
        ));
    }
    let resolution =
        crate::provenance::Resolution::of(layers).map_err(|msg| SchemaError::Invalid {
            schema_id: path.to_owned(),
            diagnostics: vec![Diagnostic::new(path, msg).with_field("layers").with_entry(
                "检查补丁里同一个键的类型是否与方案一致（映射可以合并，列表整体替换）",
            )],
        })?;
    // **编译的是合并结果**，不是原始方案文件。
    //
    // 这一行是 P2 欠下的接线：`stele-config` 里的分层补丁早就实现并测过，
    // 但装载路径一直只读方案文件本身——于是"用户补丁能覆盖一切"这句话
    // 在 P2 是**假的**。
    let mut loaded = load_from_root(&resolution.root, path, dicts, mode, base_dir)?;
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
    base_dir: Option<&std::path::Path>,
) -> Result<Loaded, SchemaError> {
    load_layered_with(
        text,
        path,
        user_patch,
        dicts,
        &DictMode::Deployed(cache_dir),
        base_dir,
    )
}

/// 找一份方案的用户补丁：`<schema_id>.custom.yaml`（RIME 的约定）。
///
/// `schema_id` 以**文件里写的**为准（不假定它等于文件名）——因此要先
/// 轻量解析一次方案文本。解析失败时返回 `None`：那条错误会在真正的
/// 装载里以更完整的诊断报出来，这里不必抢着报。
fn find_patch(dir: &std::path::Path, text: &str, file_name: &str) -> Option<(String, String)> {
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
    std::fs::read_to_string(&path).ok().map(|t| (t, patch_name))
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
    load_from_root(&root, path, dicts, mode, None)
}

/// 找到方案的**辅助数据目录**——`opencc_config` 这类相对路径的解析基准。
///
/// 两个来源，**都要**：
///
/// 1. 方案文件本身所在目录（`path` 是真实路径时）。这是 RIME 的约定：
///    `opencc_config: emoji.json` 相对于方案文件。
/// 2. 装载器给出的根目录。目录装载时 `path` 只是**文件名**（诊断里用），
///    真正的基准目录是装载器的 `root`。
///
/// 两条都试是为了让"单文件装载 + 目录装载"行为一致——只认其中一条，
/// 就会出现"测试里能读到、真跑起来读不到"这类接线 bug（P2.5 与 P3
/// 各踩过一次，见 HANDOFF §5 第 8、21 条）。
fn schema_aux_dir<'a>(
    schema_path: &'a str,
    base_dir: Option<&'a std::path::Path>,
) -> Option<&'a std::path::Path> {
    // **装载器给的目录优先**。它在目录装载时是方案的根目录，而
    // `opencc_config` 是相对**根**写的（RIME 的约定，`emoji.json` 与
    // `.schema.yaml` 同级）。反过来优先"文件名的父目录"，会在
    // `name = "cn_dicts/x.schema.yaml"` 时解析成 `cn_dicts/emoji.json`——错。
    base_dir.or_else(|| {
        std::path::Path::new(schema_path)
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
    })
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
    base_dir: Option<&std::path::Path>,
) -> Result<Loaded, SchemaError> {
    let mut diags: Vec<Diagnostic> = Vec::new();
    let mut schema_id = String::new();
    // 装载期发现的**提示**（不是错误）。它们会进 `custom`，由
    // `--dump-config` 打印——"我配的东西为什么没生效"这类问题，
    // 答案常常就在这些提示里。
    //
    // 用 `Vec<(键, 值)>` 而不是一个字符串：**一条提示一个键**。
    // 上一版是一个 `Option<String>`，于是第二个想写提示的人要么覆盖
    // 第一个（`OpenCC` 的重复行警告会吃掉"翻译器族是推断出来的"），
    // 要么把两件不相干的事拼进同一个键里。两种都不对。
    let mut load_notes: Vec<(&'static str, String)> = Vec::new();

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
            load_notes.push((
                "translator_kind_inferred",
                format!("`engine.translator` 未给出，已从 `engine.translators` 列表推断为 `{k:?}`"),
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
    let punctuator = if let Some(n) = root.get("punctuator") {
        components::read_punctuator(n, &mut diags, path)
    } else {
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

    let navigator = root.get("navigator").map_or_else(
        || {
            // 没写 `navigator` 段时用预设的翻页键（RIME 的默认）。
            let p = stele_engine::presets::stele();
            stele_engine::spec::NavigatorSpec {
                page_up: p.page_up.iter().filter_map(|s| parse_key_name(s)).collect(),
                page_down: p
                    .page_down
                    .iter()
                    .filter_map(|s| parse_key_name(s))
                    .collect(),
                ..Default::default()
            }
        },
        |n| components::read_navigator(n, &mut diags, path),
    );

    // ── 内联零件（阶段 A）：顶层段，默认值与上游一致 ──
    //
    // **只有写了段才读**：没写就用默认值。这与 `punctuator` 的处理不同
    // （那个有"中文输入法本来该有的标点"的预设），因为这些零件的默认值
    // 本身就是"上游的默认"——写 `date_translator` 不写 `date_translator:`
    // 段，得到的应当是与 rime-ice 一样的行为。
    let mut inline = stele_engine::scheme::InlineConfigs::default();
    if let Some(n) = root.get("date_translator") {
        inline.date = components::read_date(n, &mut diags, path);
    }
    if let Some(n) = root.get("calculator") {
        inline.calc = components::read_calc(n, &mut diags, path);
    }
    if let Some(n) = root.get("long_word_filter") {
        inline.long_word = components::read_long_word(n, &mut diags, path);
    }
    if let Some(n) = root.get("autocap_filter") {
        inline.autocap = components::read_autocap(n);
    }
    if let Some(n) = root.get("unicode") {
        inline.unicode = components::read_unicode(n, &mut diags, path);
    }
    if let Some(n) = root.get("number_translator") {
        inline.number = components::read_number(n, &mut diags, path);
    }
    if let Some(n) = root.get("uuid") {
        inline.uuid = components::read_uuid(n);
    }
    if let Some(n) = root.get("v_filter") {
        inline.v_filter = components::read_v_filter(n);
    }
    if let Some(n) = root.get("pin_cand_filter") {
        inline.pin_cand = components::read_pin_cand(n, &mut diags, path);
    }
    if let Some(n) = root.get("reduce_english_filter") {
        inline.reduce_english = components::read_reduce_english(n, &mut diags, path);
    }

    // **上游从识别模式里推前缀**（`unicode` 与 `number_translator` 的 Lua
    // "自动获取 `recognizer/patterns/<name>` 的第 2 个字符"）。
    //
    // 只有**没写显式段**时才推：显式配置永远优先，因为"前缀"与"分段模式"
    // 是两件事（见 `read_calc` 的说明）。这条推导让一份从 rime-ice 抄来的
    // 方案**不改一个字**也能得到正确的触发前缀。
    if root.get("unicode").is_none() {
        if let Some(c) = leading_char_of_pattern(&recognizer, "unicode") {
            inline.unicode.prefix = c;
        }
    }
    if root.get("number_translator").is_none() {
        if let Some(c) = leading_char_of_pattern(&recognizer, "number") {
            inline.number.prefix = c;
        }
    }

    // 带词缀的切分器 / 反查滤镜 / 转换滤镜：**按 `engine:` 里出现的别名**
    // 去找对应的顶层段。找不到就是"声明了却没人配"——`compile` 会报。
    let mut affixes: Vec<(String, stele_engine::spec::AffixSpec)> = Vec::new();
    let mut reverse_lookups: Vec<(String, stele_engine::spec::ReverseLookupSpec)> = Vec::new();
    let mut converters: Vec<(
        String,
        std::collections::BTreeMap<String, Vec<String>>,
        stele_engine::spec::SimplifierSpec,
    )> = Vec::new();
    // 每个 `simplifier@别名` 的**外部数据事实**：它的转换表到底装到了没有、
    // 从哪个文件装的。由装载器填（只有它知道文件在不在），最后交给
    // `stele_engine::registry::unmet_requirements` 决定要不要出声。
    let mut convert_facts: std::collections::BTreeMap<String, ConvertFact> =
        std::collections::BTreeMap::new();
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
                let aux = schema_aux_dir(path, base_dir);

                // 转换表的来源，**按顺序**：
                //
                // 1. 方案声明的 `opencc_config:`（RIME 的真实写法）。
                // 2. `opencc.manifest.yaml` 里按**别名**声明的 `config:`
                //    ——方案里只写 `option_name: emoji` 时靠它找到数据。
                // 3. 内联 `table:` —— 我们的扩展，给"只想改几个词"的方案用。
                //
                // 上一版的接线状态是：`opencc_config` **只被记下来、从未被读**，
                // 于是"配置合法、没有报错、emoji 就是不生效"（HANDOFF §3
                // 点名的那类 bug）。所以这一处必须真的走装载。
                let manifest = opencc_manifest(aux);
                let cfg_from_manifest = manifest
                    .iter()
                    .find(|(alias, _)| alias == a)
                    .map(|(_, entry)| entry.config.clone());
                let cfg = spec.opencc_config.clone().or(cfg_from_manifest);
                let cfg_path = cfg
                    .as_ref()
                    .map(|c| aux.map_or_else(|| std::path::PathBuf::from(c), |d| d.join(c)));
                // 文件不存在时**不把它当错误**：数据本来就不在仓库里
                // （`build/` 是 .gitignore 的）。这一条事实由
                // `external_data_facts` 转成"这份方案缺不缺数据"。
                let file_present = cfg_path.as_ref().is_some_and(|p| p.is_file());

                let mut table: std::collections::BTreeMap<String, Vec<String>> =
                    std::collections::BTreeMap::new();
                let mut loaded_from: Option<String> = None;
                if file_present {
                    let cfg = cfg.clone().unwrap_or_default();
                    match load_opencc_table(aux, &cfg) {
                        Ok((t, warns)) => {
                            if !t.is_empty() {
                                loaded_from = Some(cfg);
                            }
                            merge_convert_table(&mut table, t);
                            for w in warns {
                                load_notes.push(("opencc_notes", w));
                            }
                        }
                        Err(msg) => diags.push(
                            Diagnostic::new(path, msg)
                                .with_field(format!("{a}.opencc_config"))
                                .with_entry(format!("第 {} 行", block.line)),
                        ),
                    }
                }
                if let Some(t) = block.get("table").and_then(stele_config::Node::as_map) {
                    for (k, v) in t {
                        let to = v.as_str().unwrap_or_default();
                        // 内联表**覆盖** `OpenCC` 表：用户写的比上游的优先，
                        // 这样"只改几个词"才可能（否则内联表永远被压掉）。
                        table.insert(k.clone(), vec![to]);
                    }
                }
                let entries = table.len();
                convert_facts.insert(
                    a.to_owned(),
                    ConvertFact {
                        present: loaded_from.is_some() || block.get("table").is_some(),
                        source: loaded_from,
                        declared: cfg,
                        file_present,
                        entries,
                    },
                );
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

    // **允许敲哪些字符**。
    //
    // # 修过的一个静默缺口：这里以前只看 `speller.input_alphabet`
    //
    // 而 RIME **没有**这个键。RIME 的 `speller/alphabet` 就是"这个方案允许
    // 敲哪些字符"（`initials` 是其中"只能作始码"的子集），所以真正该读的是
    // **`speller.alphabet` 的字符串写法**。
    //
    // 于是此前任何 RIME 原生方案里的非字母数字输入都被**静默丢掉**：
    // 雾凇的辅码引导符 `` ` ``、`v` 模式的符号、计算器要用的 `+ - * /`
    // 全部打不进去——而症状是"敲了没反应"，没有任何报错。
    //
    // 两种写法的区别是刻意的：
    // - **字符串**（RIME）：每个字符既是一个编码单元，也是允许输入的字符。
    // - **列表**（我们的扩展）：每个元素是一个编码单元，可能是多字符
    //   （拼音方案的 `ni`/`hao`），**反推不出输入字符集**，因此留空
    //   ——空 = 不额外限制（[`crate::file`] 的调用方把它交给 `Speller`，
    //   由那里的兜底规则"ASCII 字母数字 + 分隔符"处理）。
    //
    // `speller.input_alphabet` 仍然接受，作为**显式覆盖**。
    let alphabet_string_input_chars: Vec<char> = speller
        .and_then(|s| s.get("alphabet"))
        .and_then(|n| match &n.value {
            Value::Str(t) => Some(t.chars().collect()),
            _ => None,
        })
        .unwrap_or_default();
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
        .unwrap_or(alphabet_string_input_chars);

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
            inline,
            external_data: external_data_facts(root, &engine_spec, &convert_facts),
            custom: {
                let mut m = std::collections::BTreeMap::new();
                if let Some(s) = custom_summary {
                    m.insert("component_coverage".to_owned(), s);
                }
                // 每个 `simplifier@别名` 的数据状态——"我配了 emoji，到底有没有
                // 生效"这类问题的答案就在这里（与 `--dump-config` 一起看）。
                for (alias, f) in &convert_facts {
                    m.insert(format!("simplifier_data.{alias}"), f.describe());
                }
                for (k, v) in load_notes {
                    m.insert(k.to_owned(), v);
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

/// 从 `recognizer/patterns/<name>` 里推出**触发前缀**。
///
/// 上游的 Lua 这么干：模式 `"^U[a-f0-9]+"` 的第 2 个字符（跳过 `^`）就是
/// `unicode` 的前缀 `U`。**这是上游的行为**（rime-ice 的方案里因此没有
/// `unicode:` 段），所以我们要复现它，否则抄过来的方案前缀会不对。
///
/// 推不出来（没有这个模式、或模式不以 `^` + 一个字符开头）就返回 `None`
/// ——**不猜**，退回 spec 的默认值。
fn leading_char_of_pattern(
    recognizer: &stele_engine::spec::RecognizerSpec,
    name: &str,
) -> Option<char> {
    let regex = recognizer
        .patterns
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.regex.as_str())?;
    let rest = regex.strip_prefix('^').unwrap_or(regex);
    let mut it = rest.chars();
    match (it.next(), it.next()) {
        // 前缀必须是一个**字面**字符，后面还得有东西（`^U[...]` 而不是 `^U`）。
        (Some(c), Some(_)) if c.is_ascii_alphanumeric() => Some(c),
        _ => None,
    }
}

/// 一个 `simplifier@别名` 的外部数据事实。
#[derive(Clone, Debug, Default)]
struct ConvertFact {
    /// 数据是否**真的装到了引擎里**（有 `OpenCC` 表，或有内联 `table:`）。
    present: bool,
    /// 实际装载成功的 `OpenCC` 配置（相对路径）。
    source: Option<String>,
    /// 方案或清单声明的配置路径（无论是否装载成功）。
    declared: Option<String>,
    /// 声明指向的文件在不在。
    file_present: bool,
    /// 装进来的表有多少条。
    entries: usize,
}

impl ConvertFact {
    /// 给 `--dump-config` 看的一句人话。
    fn describe(&self) -> String {
        match (&self.source, self.file_present) {
            (Some(s), _) => format!("已装载 `{s}`（{} 条转换）", self.entries),
            (None, false) => match &self.declared {
                Some(c) => {
                    format!("**数据缺失**：`{c}` 不存在。先跑 `bash tools/fetch-sources.sh`。")
                }
                None => "**数据缺失**：既没有 `opencc_config`，也没有内联 `table:`".to_owned(),
            },
            (None, true) => match &self.declared {
                Some(c) => format!("`{c}` 存在但一条转换也没读出来（检查它的 `conversion_chain`）"),
                None => "**数据缺失**".to_owned(),
            },
        }
    }
}

/// 读 `opencc.manifest.yaml`，返回 `别名 → 清单条目`。
///
/// **清单是可选的**：没有它（或格式不对）就返回空，装载照旧——
/// 因为"数据不在"是正常状态（`build/` 不进仓库），而**配置文件坏了
/// 不该阻止启动**（D26：输入法的失败是自锁的）。
fn opencc_manifest(dir: Option<&std::path::Path>) -> Vec<(String, ManifestEntry)> {
    #[derive(Clone, Debug, Default)]
    struct _Unused;
    let Some(dir) = dir else { return Vec::new() };
    let Some(text) = std::fs::read_to_string(dir.join("opencc.manifest.yaml")).ok() else {
        return Vec::new();
    };
    let Ok(root) = stele_config::parse(&text) else {
        return Vec::new();
    };
    let Some(items) = root.get("converters").and_then(Node::as_seq) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for it in items {
        let (Some(name), Some(config)) = (
            it.get("name").and_then(Node::as_str),
            it.get("config").and_then(Node::as_str),
        ) else {
            continue;
        };
        out.push((
            name,
            ManifestEntry {
                config,
                source: it.get("source").and_then(Node::as_str),
                license: it.get("license").and_then(Node::as_str),
                purpose: it.get("purpose").and_then(Node::as_str),
            },
        ));
    }
    out
}

/// 清单的一条。
#[derive(Clone, Debug, Default)]
pub struct ManifestEntry {
    /// `OpenCC` 配置的相对路径。
    pub config: String,
    /// 上游来源（人话）。
    pub source: Option<String>,
    /// 上游许可。
    pub license: Option<String>,
    /// 这个转换是干什么的。
    pub purpose: Option<String>,
}

/// 装载 `simplifier` 的 `opencc_config`，把错误翻成一句人话。
///
/// 真正的解析在 `stele-dict`（它是纯函数、可脱离文件系统测试）；
/// 这里只负责**把"相对路径"变成"能读到的字节"**——也就是装载器
/// 唯一有资格做的那件事：知道文件在哪。
///
/// `aux` 是方案文件所在目录（`None` = 方案不是从文件装载的，
/// 例如内嵌方案）。此时不报错而是返回一句解释：内嵌方案里写
/// `opencc_config` 本来就无处可读，说出来比静默空表好。
fn load_opencc_table(
    aux: Option<&std::path::Path>,
    cfg: &str,
) -> Result<(dict::opencc::ConvertTable, Vec<String>), String> {
    let Some(dir) = aux else {
        return Err(format!(
            "`opencc_config: {cfg}` 需要一个方案文件所在目录来解析相对路径，\
             但这份方案不是从文件装载的（内嵌方案）。\
             请用 `--scheme-dir` 从目录装载，或改用内联 `table:`。"
        ));
    };
    // `opencc_config` 可能写成 `emoji.json`（OpenCC 的约定，相对方案文件），
    // 也可能写成 `opencc/emoji.json`。两种情况都按"相对方案目录"解析。
    let abs = dir.join(cfg);
    let display = abs.display().to_string();
    let store = FsSource;
    dict::opencc::load(&store, &display, &display).map_err(|e| e.to_string())
}

/// 把 `OpenCC` 的配置与它引用的词典**直接从文件系统读**。
///
/// 为什么不复用方案的 `dict::Source`：那一套的路径解析约定是"相对词典目录"
/// （`DirSource` 还会自动补 `.dict.yaml` 后缀），而 `OpenCC` 的文件名是**写死的**
/// （`emoji.txt`）。两个约定混用会让"文件明明在、就是读不到"再次发生。
/// 所以这里给出一份**只要绝对路径**的最小实现。
struct FsSource;

impl dict::Source for FsSource {
    fn read(&self, rel_path: &str) -> Option<String> {
        std::fs::read_to_string(rel_path).ok()
    }
}

/// 把一张转换表并进另一张：**逐键覆盖**。
///
/// 用于"`OpenCC` 的链"（后面的词典赢）——`stele-dict` 内部已经按链的
/// 顺序覆盖过一次，这里是把**多段配置**（一个方案里可以有好几个
/// `simplifier@x`）分开保存，所以不复用。
fn merge_convert_table(
    into: &mut std::collections::BTreeMap<String, Vec<String>>,
    from: std::collections::BTreeMap<String, Vec<String>>,
) {
    for (k, v) in from {
        into.insert(k, v);
    }
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
    engine: &stele_engine::spec::EngineSpec,
    convert_facts: &std::collections::BTreeMap<String, ConvertFact>,
) -> Vec<stele_engine::registry::ExternalData<'static>> {
    let mut out = Vec::new();
    for name in engine.translators.iter().chain(engine.filters.iter()) {
        let (component, alias) = stele_engine::spec::split_alias(name);
        let Some(a) = alias else { continue };
        let present = match root.get(a) {
            None => false,
            Some(block) => match component {
                // **以"表真的装到了"为准**，而不是"文件在不在"。
                // 前者才是引擎能不能工作的判据。
                "simplifier" => convert_facts.get(a).is_some_and(|f| f.present),
                "table_translator" | "reverse_lookup_translator" | "reverse_lookup_filter" => {
                    block.get("dictionary").is_some()
                }
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
        let patch_ref = patch.as_ref().map(|(t, n)| (t.as_str(), n.as_str()));
        out.push(load_layered_with(
            &text,
            name,
            patch_ref,
            &src,
            &DictMode::Inline,
            // 目录装载：辅助数据（`opencc_config`）相对于**这个目录**解析，
            // 而不是相对于 `name`（那只是文件名，诊断里用）。
            Some(root),
        )?);
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
        let patch_ref = patch.as_ref().map(|(t, n)| (t.as_str(), n.as_str()));
        out.push(load_layered_inner_deployed(
            &text,
            &name,
            patch_ref,
            &src,
            cache_dir,
            Some(root),
        )?);
    }
    Ok(out)
}

/// 部署一份词库：算身份指纹 → 需要就编译 → 以按需分页的方式打开。
///
/// **注意它不经过内联路径**——内联会把全部词条读进内存，
/// 而那正是我们要消灭的 245 MB 峰值。
///
/// # 缓存身份（P0-B 修复）
///
/// 产物文件名里放的是 [`BuildFingerprint`]，**不是**源数据校验和。
/// 差别是致命的：产物里存的是**编码单元的下标**，而下标由 `alphabet`
/// 的顺序决定。只用源校验和当文件名时，"只把 `alphabet` 从 `[ni, hao]`
/// 改成 `[hao, ni]`"会命中旧产物——输入 `ni` 得到「好」。指纹把
/// 格式版本、编译选项、字母表内容与顺序、源数据校验和一起算进去，
/// 于是**任何影响下标语义的改动都会强制重建**。
///
/// # 并发
///
/// 两个进程同时部署同一份词库时，它们算出的指纹相同、目标路径相同；
/// 编译走"同目录唯一临时文件 + 原子改名"（见 `stele_table::compile`），
/// 因此谁先到谁发布，后到者覆盖成逐字节相同的内容。**读者永远看不到
/// 半个产物**。
fn deploy_dict(
    src: &dyn dict::Source,
    dict_name: &str,
    alphabet: &CodeAlphabet,
    cache_dir: &std::path::Path,
) -> Result<std::sync::Arc<dyn stele_core::Lexicon>, Diagnostic> {
    let bad = |m: String| Diagnostic::new(dict_name, m).with_field("translator.dictionary");

    // 源数据校验和（含 import 链的原始字节）。
    let checksum = dict::checksum_of(src, dict_name, dict_name)
        .map_err(|e| bad(format!("算词库校验和失败：{e}")))?;

    // 身份指纹：格式版本 + 编译选项 + **字母表内容与顺序** + 源校验和。
    let units: Vec<String> = (0..alphabet.len())
        .filter_map(|i| alphabet.text(CodeUnitId(u32::try_from(i).ok()?)))
        .map(str::to_owned)
        .collect();
    let fingerprint = stele_table::BuildFingerprint::of(
        stele_table::FORMAT_VERSION,
        stele_table::COMPILER_OPTIONS,
        &units,
        checksum,
    );
    let table_path = cache_dir.join(format!("{dict_name}.{}.table", fingerprint.hex()));

    if !table_path.exists() {
        std::fs::create_dir_all(cache_dir)
            .map_err(|e| bad(format!("建不了缓存目录 {}：{e}", cache_dir.display())))?;

        // 流式：词条一条条喂进写入器，**中间没有 Vec<RawEntry>**。
        let r = stele_table::compile(
            checksum,
            fingerprint,
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

    let lex = stele_table::TableLexicon::open_checked(&table_path, Some(fingerprint))
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
