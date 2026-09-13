//! # `OpenCC` data loading
//!
//! 中文职责：把 `OpenCC` 的 `*.json` 配置读成一张**已装载好的转换表**
//! （`from → to` 列表），供 `simplifier` 使用。
//! English role: load `OpenCC` conversion configs into a ready conversion table.
//! 架构位置：`qingjian-dict`（数据格式层）；消费者是 `qingjian-schemes` 的方案装载器，
//! 最终变成 `qingjian-engine` 的 `Converter` 滤镜。
//!
//! # 为什么要单独做这件事
//!
//! `simplifier` 的机制早就实现了（`qingjian-engine` 的 `Converter`），
//! 但**它的数据从来没有人读过**：方案里写 `opencc_config: emoji.json`，
//! 装载器只记下这个名字、把表留空——于是"配置看起来正常、emoji 就是不生效"。
//! 这正是 HANDOFF §3 里点名的那一类 bug。这个模块把那条链补上。
//!
//! # `OpenCC` 的配置长什么样（照真文件写，不凭印象）
//!
//! `emoji.json`：
//!
//! ```json
//! {
//!   "name": "Chinese to Emoji",
//!   "segmentation": { "type": "mmseg", "dict": { "type": "text", "file": "emoji.txt" } },
//!   "conversion_chain": [
//!     { "dict": { "type": "group", "dicts": [
//!       { "type": "text", "file": "emoji.txt" },
//!       { "type": "text", "file": "others.txt" }
//!     ] } }
//!   ]
//! }
//! ```
//!
//! 词典文件（`emoji.txt` / `others.txt` / `OpenCC` 的 `STCharacters.txt` 同格式）：
//!
//! ```text
//! # 以 # 开头的行是注释
//! 扭曲<TAB>扭曲 🫪        ← 键<TAB>候选1 候选2 …（空格分隔）
//! ```
//!
//! # 三条刻意的严格
//!
//! 1. **不认识的 `type` 一律报错**（`ocd2` 是 `OpenCC` 的二进制格式，我们不解析）。
//!    静默跳过会让"简繁不生效"变成一个没有症状的 bug。
//! 2. **文件缺失一律报错**，并指出是配置里的哪一条。
//! 3. **重复键取"后出现者"**——这是 `OpenCC` 自己的规则（链上后面的词典覆盖前面的），
//!    而**同一个文件里**的重复行会被报出来：上游明确说"不能有重复的行"，
//!    那属于上游数据坏了，不该由我们悄悄消化。

use crate::json::Json;
use crate::Source;

/// 一个"从哪读词典"的描述（`OpenCC` 配置里的 `dict` 节点）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DictRef {
    /// `{"type": "text", "file": "emoji.txt"}`——一张 TAB 分隔的文本表。
    Text(String),
    /// `{"type": "group", "dicts": [...]}`——按顺序求值的词典组。
    Group(Vec<DictRef>),
}

impl DictRef {
    /// 把 `file` 那一层的文件名收集出来（诊断用）。
    pub fn files(&self, out: &mut Vec<String>) {
        match self {
            DictRef::Text(f) => out.push(f.clone()),
            DictRef::Group(v) => {
                for d in v {
                    d.files(out);
                }
            }
        }
    }
}

/// 一份解析好的 `OpenCC` 配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenCcConfig {
    /// `name` 字段（可缺省）。
    pub name: Option<String>,
    /// 转换链：**按顺序求值**，后面的覆盖前面的。
    pub conversion_chain: Vec<DictRef>,
}

impl OpenCcConfig {
    /// 全部词典文件（按求值顺序，去重前的原样）。
    #[must_use]
    pub fn files(&self) -> Vec<String> {
        let mut out = Vec::new();
        for d in &self.conversion_chain {
            d.files(&mut out);
        }
        out
    }
}

/// 装载错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenCcError {
    /// 出错的文件（配置或某张表）。
    pub path: String,
    /// 行号（0 表示不在具体某行）。
    pub line: u32,
    /// 人话解释。
    pub message: String,
}

impl OpenCcError {
    fn new(path: &str, line: u32, message: impl Into<String>) -> Self {
        Self {
            path: path.to_owned(),
            line,
            message: message.into(),
        }
    }
}

impl core::fmt::Display for OpenCcError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.line > 0 {
            write!(f, "{}:{}：{}", self.path, self.line, self.message)
        } else {
            write!(f, "{}：{}", self.path, self.message)
        }
    }
}

impl std::error::Error for OpenCcError {}

/// 一张装载好的转换表：键 → 候选列表（**顺序有意义**，第一个是首选）。
pub type ConvertTable = std::collections::BTreeMap<String, Vec<String>>;

/// 解析 `OpenCC` 配置文本（不读词典文件）。
///
/// # Errors
///
/// JSON 语法错误、缺少 `conversion_chain`、词典 `type` 不认识时返回
/// 带行号的 [`OpenCcError`]。
pub fn parse_config(text: &str, path: &str) -> Result<OpenCcConfig, OpenCcError> {
    let json = crate::json::parse(text)
        .map_err(|e| OpenCcError::new(path, e.line, format!("JSON 语法错误：{}", e.message)))?;

    let name = json.get("name").and_then(Json::as_str).map(str::to_owned);

    let chain = json
        .get("conversion_chain")
        .and_then(Json::as_array)
        .ok_or_else(|| {
            OpenCcError::new(
                path,
                1,
                "`OpenCC` 配置缺少 `conversion_chain`（数组）。\
                 它是「用哪些词典、按什么顺序转换」的声明——没有它就无从装载。",
            )
        })?;

    let mut conversion_chain = Vec::with_capacity(chain.len());
    for (i, item) in chain.iter().enumerate() {
        let dict = item.get("dict").ok_or_else(|| {
            OpenCcError::new(path, 1, format!("`conversion_chain[{i}]` 缺少 `dict` 节点"))
        })?;
        conversion_chain.push(parse_dict_ref(
            dict,
            path,
            &format!("conversion_chain[{i}].dict"),
        )?);
    }

    Ok(OpenCcConfig {
        name,
        conversion_chain,
    })
}

fn parse_dict_ref(node: &Json, path: &str, where_: &str) -> Result<DictRef, OpenCcError> {
    let ty = node
        .get("type")
        .and_then(Json::as_str)
        .ok_or_else(|| OpenCcError::new(path, 1, format!("`{where_}` 缺少 `type` 字段")))?;
    match ty {
        "text" => {
            let file = node.get("file").and_then(Json::as_str).ok_or_else(|| {
                OpenCcError::new(
                    path,
                    1,
                    format!("`{where_}` 是 `text` 类型，但缺少 `file` 字段"),
                )
            })?;
            Ok(DictRef::Text(file.to_owned()))
        }
        "group" => {
            let dicts = node.get("dicts").and_then(Json::as_array).ok_or_else(|| {
                OpenCcError::new(
                    path,
                    1,
                    format!("`{where_}` 是 `group` 类型，但缺少 `dicts` 数组"),
                )
            })?;
            let mut out = Vec::with_capacity(dicts.len());
            for (i, d) in dicts.iter().enumerate() {
                out.push(parse_dict_ref(d, path, &format!("{where_}.dicts[{i}]"))?);
            }
            Ok(DictRef::Group(out))
        }
        other => Err(OpenCcError::new(
            path,
            1,
            format!(
                "不认识的词典类型 `{other}`（在 `{where_}`）。\
                 我们只解析 `text`（TAB 分隔的文本表）与 `group`（词典组）。\
                 `OpenCC` 的 `ocd2` 是它自己的二进制格式——**不解析而不是猜**，\
                 因为猜错的结果是「转换表少了一半而没有任何报错」。"
            ),
        )),
    }
}

/// 解析一张 `OpenCC` 文本表。
///
/// 格式：`键<TAB>值1 值2 …`；`#` 开头的行与空行跳过。
///
/// **值内部可以含 TAB**（emoji 表的 `微笑<TAB>微笑<TAB>😊` 就是一个值
/// 里放了两半），因此切分只按空格——见下面那句注释。
///
/// 返回 `(表, 警告)`。**警告而不是错误**：上游注释里明确说"不能有重复行"
/// （`others.txt` 第一行就是这句话），但真有重复时，取后者比拒绝服务更安全——
/// 而我们要让这件事**可见**。
///
/// # Errors
///
/// 某一行没有 TAB、或键为空时返回带行号的 [`OpenCcError`]。
pub fn parse_table(text: &str, path: &str) -> Result<(ConvertTable, Vec<String>), OpenCcError> {
    let mut table: ConvertTable = std::collections::BTreeMap::new();
    let mut warnings = Vec::new();

    for (i, raw) in text.lines().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let no = (i + 1) as u32;
        let line = raw.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, values)) = line.split_once('\t') else {
            return Err(OpenCcError::new(
                path,
                no,
                format!(
                    "这一行没有 TAB：`{}`。`OpenCC` 的词典是「键<TAB>候选…」，\
                     用空格分隔会让「哪个是键」无从判断。",
                    crate::truncate_for_diag(line, 40)
                ),
            ));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(OpenCcError::new(path, no, "这一行的键（TAB 之前）是空的"));
        }
        // **值不在此处切分**，整段保留。
        //
        // `OpenCC` 的表有**两种逐字节相同的形状**，而含义相反：
        //
        // | 表 | 行 | 含义 |
        // | --- | --- | --- |
        // | 简繁 | `干<TAB>乾 幹` | 两个**可选**写法 |
        // | emoji | `微笑<TAB>微笑 😊` | 一个**复合**串（词 + 表情） |
        //
        // 单看这一行分不出是哪种——判据在**键**上：后者以键自身开头。
        // 因此切分推迟到消费者（`qingjian-engine` 的 `Converter`），
        // 它同时握着键与值，能做出正确判断。
        //
        // 这个坑真的踩过：早期版本在这里按空白切，于是 emoji 表的
        // 每一条都被拆成 `["微笑", "😊"]`——而消费者把第一个当"词本身"、
        // 第二个当"候选"，**结果一个 emoji 都出不来**。
        let values: Vec<String> = vec![values.trim().to_owned()];
        if values.is_empty() {
            return Err(OpenCcError::new(
                path,
                no,
                format!("键 `{key}` 没有任何候选（TAB 之后是空的）"),
            ));
        }
        if table.insert(key.to_owned(), values).is_some() {
            warnings.push(format!(
                "{path}:{no}：键 `{key}` 在本文件里重复了，按 `OpenCC` 的规则取**后出现者**"
            ));
        }
    }
    Ok((table, warnings))
}

/// 装载一份 `OpenCC` 配置**以及它引用的全部词典**。
///
/// `rel_path` 是配置文件的相对路径（诊断与 `src.read` 用）；
/// 词典文件按**与配置同目录**解析（`OpenCC` 就是这么约定相对路径的）。
///
/// # Errors
///
/// 配置不存在/语法错、词典文件不存在/格式错、`type` 不认识时返回 [`OpenCcError`]。
pub fn load(
    src: &dyn Source,
    rel_path: &str,
    display_name: &str,
) -> Result<(ConvertTable, Vec<String>), OpenCcError> {
    let config_text = src.read(rel_path).ok_or_else(|| {
        OpenCcError::new(
            display_name,
            0,
            format!(
                "找不到 `OpenCC` 配置 `{rel_path}`。`opencc_config` 指的是\
                 相对于方案文件的路径（`OpenCC` 的约定）。"
            ),
        )
    })?;
    let config = parse_config(&config_text, rel_path)?;

    let dir = match rel_path.rfind('/') {
        Some(i) => &rel_path[..=i],
        None => "",
    };

    let mut table: ConvertTable = std::collections::BTreeMap::new();
    let mut warnings = Vec::new();
    // 链**按顺序**求值：后面的词典覆盖前面的。
    for dict in &config.conversion_chain {
        load_dict(src, dict, dir, display_name, &mut table, &mut warnings)?;
    }
    Ok((table, warnings))
}

fn load_dict(
    src: &dyn Source,
    dict: &DictRef,
    dir: &str,
    display_name: &str,
    table: &mut ConvertTable,
    warnings: &mut Vec<String>,
) -> Result<(), OpenCcError> {
    match dict {
        DictRef::Text(file) => {
            let rel = format!("{dir}{file}");
            let text = src.read(&rel).ok_or_else(|| {
                OpenCcError::new(
                    display_name,
                    0,
                    format!(
                        "`OpenCC` 配置引用的词典 `{file}`（解析为 `{rel}`）读不到。\
                         它应当与配置放在同一个目录里。"
                    ),
                )
            })?;
            let (t, w) = parse_table(&text, &rel)?;
            warnings.extend(w);
            // 后面出现的覆盖前面的——**逐键覆盖，而不是整表替换**。
            for (k, v) in t {
                table.insert(k, v);
            }
            Ok(())
        }
        DictRef::Group(v) => {
            for d in v {
                load_dict(src, d, dir, display_name, table, warnings)?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMOJI_JSON: &str = r#"{
	"name": "Chinese to Emoji",
	"segmentation": {
		"type": "mmseg",
		"dict": { "type": "text", "file": "emoji.txt" }
	},
	"conversion_chain": [
		{
			"dict": {
				"type": "group",
				"dicts": [
					{ "type": "text", "file": "emoji.txt" },
					{ "type": "text", "file": "others.txt" }
				]
			}
		}
	]
}"#;

    #[test]
    fn parses_the_real_emoji_config_shape() {
        let c = parse_config(EMOJI_JSON, "emoji.json").unwrap();
        assert_eq!(c.name.as_deref(), Some("Chinese to Emoji"));
        assert_eq!(c.conversion_chain.len(), 1);
        assert_eq!(c.files(), ["emoji.txt", "others.txt"]);
    }

    #[test]
    fn unknown_dict_type_is_rejected_with_the_reason() {
        let text = r#"{"conversion_chain": [{"dict": {"type": "ocd2", "file": "x.ocd2"}}]}"#;
        let e = parse_config(text, "c.json").unwrap_err();
        assert!(e.message.contains("ocd2"), "{}", e.message);
        assert!(e.message.contains("不解析"), "{}", e.message);
    }

    #[test]
    fn missing_conversion_chain_is_rejected() {
        let e = parse_config("{}", "c.json").unwrap_err();
        assert!(e.message.contains("conversion_chain"), "{}", e.message);
    }

    #[test]
    fn table_parsing_preserves_the_whole_value() {
        // **值与键一起才可解读**：`微笑<TAB>微笑 😊` 是复合串，
        // 而 `干<TAB>乾 幹` 是两个可选项。解析层不猜，整段保留。
        let t = "# 注释\n微笑\t微笑 😊\n扭曲\t扭曲\t🫪\n干\t乾 幹\n";
        let (table, w) = parse_table(t, "t.txt").unwrap();
        assert!(w.is_empty());
        assert_eq!(table["微笑"], ["微笑 😊"]);
        assert_eq!(table["扭曲"], ["扭曲\t🫪"]);
        assert_eq!(table["干"], ["乾 幹"]);
    }

    #[test]
    fn duplicate_rows_warn_but_take_the_last() {
        let t = "甲\t一\n甲\t二\n";
        let (table, w) = parse_table(t, "t.txt").unwrap();
        assert_eq!(w.len(), 1, "重复行必须被报出来");
        assert!(w[0].contains("取**后出现者**"), "{}", w[0]);
        assert_eq!(table["甲"], ["二"]);
    }

    #[test]
    fn a_row_without_tab_is_rejected_with_the_reason() {
        let e = parse_table("甲 一\n", "t.txt").unwrap_err();
        assert_eq!(e.line, 1);
        assert!(e.message.contains("TAB"), "{}", e.message);
    }

    struct Mem(std::collections::BTreeMap<String, String>);

    impl Source for Mem {
        fn read(&self, rel: &str) -> Option<String> {
            self.0.get(rel).cloned()
        }
    }

    fn mem() -> Mem {
        let mut m = std::collections::BTreeMap::new();
        m.insert("opencc/emoji.json".into(), EMOJI_JSON.into());
        // 两个文件**有重叠键**：`扭曲` 必须由后面的 others.txt 赢。
        m.insert(
            "opencc/emoji.txt".into(),
            "扭曲\t扭曲 🫪\n打斗\t打斗 🫯\n".into(),
        );
        m.insert(
            "opencc/others.txt".into(),
            "扭曲\t扭曲 后端赢\n一月\t一月 Jan.\n".into(),
        );
        Mem(m)
    }

    #[test]
    fn loads_the_chain_relative_to_the_config() {
        let (table, w) = load(&mem(), "opencc/emoji.json", "s.schema.yaml").unwrap();
        assert!(w.is_empty());
        // 值整段保留（见 `parse_table` 的说明）：覆盖是**整条覆盖**。
        assert_eq!(table["扭曲"], ["扭曲 后端赢"], "后面的词典必须覆盖前面的");
        assert_eq!(table["打斗"], ["打斗 🫯"]);
        assert_eq!(table["一月"], ["一月 Jan."]);
    }

    #[test]
    fn missing_dictionary_file_is_reported_with_the_path() {
        let mut m = std::collections::BTreeMap::new();
        m.insert("c.json".into(), EMOJI_JSON.into());
        m.insert("emoji.txt".into(), "甲\t一\n".into());
        // others.txt 缺失
        let e = load(&Mem(m), "c.json", "s").unwrap_err();
        assert!(e.message.contains("others.txt"), "{}", e.message);
    }

    #[test]
    fn missing_config_is_reported() {
        let e = load(&mem(), "nope.json", "s").unwrap_err();
        assert!(e.message.contains("opencc_config"), "{}", e.message);
    }
}
