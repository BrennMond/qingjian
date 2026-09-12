//! # Dictionary file format
//!
//! 中文职责：解析 Rime 的 `.dict.yaml`——**YAML 头部 + TSV 正文**。
//! English role: parse Rime's `.dict.yaml` — a YAML header plus a TSV body.
//! 架构位置：`stele-engine` 的上游；产物是**原始词条**，由方案装载器编译成词库。
//!
//! # 文件形状
//!
//! ```text
//! # 注释可以出现在任何地方
//! ---
//! name: stele
//! version: "1"
//! sort: by_weight
//! import_tables:
//!   - cn_dicts/base
//! ...
//! 你好<TAB>ni hao<TAB>10000
//! 世界<TAB>shi jie<TAB>5000
//! ```
//!
//! 正文是 **TAB 分隔**的三列：`词 / 编码 / 权重`（下面用 `<TAB>` 表示制表符）。
//! 权重可省略（省略即 0，等价于"最低"）。
//!
//! # 两条刻意的严格
//!
//! 1. **正文用 TAB 而不是空格分隔。** 编码里本来就含空格（`ni hao`），
//!    所以分隔符必须是 TAB——用空格会让"编码"与"权重"分不开。
//!    遇到空格分隔的正文**直接报错**，而不是猜。
//! 2. **重复词条要报错。** YAML 里重复的键是未定义行为；词库里重复的**词**
//!    同样会让"为什么这个词的权重是 3 而不是 5000"变成一场考古。
//!    （同词不同音是合法的，那算两条不同的记录。）

use std::collections::BTreeSet;
use stele_config::{parse_at, Node};

/// 词典文件里的一个原始词条。
#[derive(Clone, Debug, PartialEq)]
pub struct RawEntry {
    /// 词（上屏文本）。
    pub word: String,
    /// 编码：空格分隔的编码单元文本序列（例如 `ni hao`）。
    pub code: String,
    /// 权重。缺省为 0（等价于最低权重）。
    pub weight: f64,
    /// 在源文件里的行号。
    pub line: u32,
}

impl RawEntry {
    /// 把编码切成编码单元。
    #[must_use]
    pub fn units(&self) -> Vec<&str> {
        self.code.split_whitespace().collect()
    }
}

/// 词典头部。
#[derive(Clone, Debug, PartialEq)]
pub struct DictHeader {
    /// 词典名。
    pub name: String,
    /// 版本串。
    pub version: String,
    /// 排序方式（`by_weight` 等）。**目前只记录，不影响行为**——
    /// 分数的排序在引擎里做，不依赖文件里的顺序。
    pub sort: Option<String>,
    /// 要合并进来的其它词典（相对于本文件的路径）。
    pub import_tables: Vec<String>,
}

/// 一份解析好的词典文件（**不含** import 展开）。
#[derive(Clone, Debug, PartialEq)]
pub struct DictFile {
    /// 文件路径（用于诊断）。
    pub path: String,
    /// 头部。
    pub header: DictHeader,
    /// 正文词条。
    pub entries: Vec<RawEntry>,
}

/// 词典解析错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DictError {
    /// 出错的文件。
    pub path: String,
    /// 行号（0 表示不在具体某行）。
    pub line: u32,
    /// 人话解释。
    pub message: String,
}

impl DictError {
    fn new(path: &str, line: u32, message: impl Into<String>) -> Self {
        Self {
            path: path.to_owned(),
            line,
            message: message.into(),
        }
    }
}

impl core::fmt::Display for DictError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.line > 0 {
            write!(f, "{}:{}：{}", self.path, self.line, self.message)
        } else {
            write!(f, "{}：{}", self.path, self.message)
        }
    }
}

impl std::error::Error for DictError {}

/// 解析一份 `.dict.yaml`。
///
/// # Errors
///
/// 缺少 `---` / `...` 分隔符、头部字段缺失、正文不是 TAB 分隔、
/// 词条重复时返回带行号的 [`DictError`]。
pub fn parse_dict(text: &str, path: &str) -> Result<DictFile, DictError> {
    let (header_text, header_start, body_start) = split_sections(text, path)?;

    let header_node = parse_at(&header_text, header_start)
        .map_err(|e| DictError::new(path, e.line, format!("词典头部解析失败：{}", e.message)))?;

    let header = read_header(&header_node, path)?;

    let mut entries = Vec::new();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for_each_body_line(text, body_start, path, |e| {
        if !seen.insert((e.word.clone(), e.code.clone())) {
            return Err(DictError::new(
                path,
                e.line,
                format!(
                    "词条「{} / {}」重复了。\
                     重复不会报错但会让「为什么这个词的权重是 3 而不是 5000」\
                     变成一场考古，所以这里直接拒绝。\
                     （同一个词的不同读音是合法的，那算两条不同的记录。）",
                    e.word, e.code
                ),
            ));
        }
        entries.push(e);
        Ok(())
    })?;

    Ok(DictFile {
        path: path.to_owned(),
        header,
        entries,
    })
}

/// 把文件切成"头部 YAML"与"正文"两段。
///
/// 返回 `(头部文本, 头部起始行号, 正文起始行号)`。
fn split_sections(text: &str, path: &str) -> Result<(String, u32, usize), DictError> {
    let mut start: Option<usize> = None;
    let mut end: Option<usize> = None;

    for (i, raw) in text.lines().enumerate() {
        let t = raw.trim();
        if t == "---" && start.is_none() {
            start = Some(i);
        } else if t == "..." && start.is_some() && end.is_none() {
            end = Some(i);
        }
    }

    let start = start.ok_or_else(|| {
        DictError::new(
            path,
            0,
            "找不到头部起始标记 `---`。词典文件的第一段是 YAML 头部，\
             用 `---` 开始、`...` 结束。",
        )
    })?;
    let end = end.ok_or_else(|| {
        DictError::new(
            path,
            0,
            "找不到头部结束标记 `...`。没有它就无法区分「头部」与「词条正文」。",
        )
    })?;
    if end < start {
        return Err(DictError::new(path, 0, "`...` 出现在 `---` 之前"));
    }

    let lines: Vec<&str> = text.lines().collect();
    let header_text = lines[start + 1..end].join("\n");
    // 头部的第一行是 `---` 之后的第 start+2 行（1-based）。
    #[allow(clippy::cast_possible_truncation)]
    let header_start = (start + 2) as u32;
    Ok((header_text, header_start, end + 2))
}

fn read_header(node: &Node, path: &str) -> Result<DictHeader, DictError> {
    let name = node
        .get("name")
        .and_then(Node::as_str)
        .ok_or_else(|| DictError::new(path, node.line, "头部缺少 `name` 字段（词典名）"))?;
    let version = node
        .get("version")
        .and_then(Node::as_str)
        .ok_or_else(|| DictError::new(path, node.line, "头部缺少 `version` 字段"))?;
    let sort = node.get("sort").and_then(Node::as_str);
    let import_tables = node
        .get("import_tables")
        .and_then(|n| n.as_seq())
        .map(|seq| seq.iter().filter_map(Node::as_str).collect())
        .unwrap_or_default();

    Ok(DictHeader {
        name,
        version,
        sort,
        import_tables,
    })
}

/// 去掉行尾注释（`#` 前有空白才算）。
fn strip_trailing_comment(line: &str) -> &str {
    let b = line.as_bytes();
    for i in 0..b.len() {
        if b[i] == b'#' && i > 0 && (b[i - 1] as char).is_whitespace() {
            return &line[..i];
        }
    }
    line
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

/// 逐行解析正文，对每一条合法词条调用 `f`。
///
/// **抽出来是为了让"流式编译"不必先攒一个 `Vec<RawEntry>`**——
/// 雾凇规模的词库有 188 万条，光是那个中间向量就要几百 MB。
fn for_each_body_line<F>(
    text: &str,
    body_start: usize,
    path: &str,
    mut f: F,
) -> Result<(), DictError>
where
    F: FnMut(RawEntry) -> Result<(), DictError>,
{
    for (i, raw) in text.lines().enumerate().skip(body_start.saturating_sub(1)) {
        #[allow(clippy::cast_possible_truncation)]
        let no = (i + 1) as u32;
        let line = raw.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if !line.contains('\t') {
            return Err(DictError::new(
                path,
                no,
                format!(
                    "词条必须用 TAB 分隔（词 <TAB> 编码 <TAB> 权重），但这一行没有 TAB：`{}`。\
                     编码本身含空格（`ni hao`），所以分隔符只能是 TAB——\
                     用空格的话「编码」与「权重」就分不开了。",
                    truncate(line, 40)
                ),
            ));
        }

        let content = strip_trailing_comment(line);
        let mut cols = content.split('\t');
        let word = cols.next().unwrap_or("").trim().to_owned();
        let code = cols.next().unwrap_or("").trim().to_owned();
        let weight_text = cols.next().unwrap_or("").trim();
        let extra = cols.next();

        if word.is_empty() {
            return Err(DictError::new(path, no, "词条的第一列（词）是空的"));
        }
        if code.is_empty() {
            return Err(DictError::new(
                path,
                no,
                format!("词条「{word}」缺少第二列（编码）"),
            ));
        }
        if extra.is_some() {
            return Err(DictError::new(
                path,
                no,
                format!("词条「{word}」超过了三列——本格式只有「词 / 编码 / 权重」三列"),
            ));
        }

        let weight = if weight_text.is_empty() {
            0.0
        } else {
            weight_text.parse::<f64>().map_err(|_| {
                DictError::new(
                    path,
                    no,
                    format!(
                        "词条「{word}」的权重 `{weight_text}` 不是数字。\
                         权重是相对词频（整数或小数），省略即视为最低。"
                    ),
                )
            })?
        };

        f(RawEntry {
            word,
            code,
            weight,
            line: no,
        })?;
    }
    Ok(())
}

/// 流式遍历一份词典（含 `import_tables`），**不构造中间向量**。
///
/// 返回所有源文件的字节数（供调用方算校验和，见 PLAN D28）。
///
/// # Errors
///
/// 与 [`load_with_imports`] 相同。
pub fn for_each_entry<F>(
    src: &dyn Source,
    rel_path: &str,
    display_name: &str,
    mut f: F,
) -> Result<u64, DictError>
where
    F: FnMut(&str, &str, f64) -> Result<(), DictError>,
{
    let mut checksum = 0u64;
    let mut stack: Vec<String> = Vec::new();
    stream_collect(
        src,
        rel_path,
        display_name,
        &mut f,
        &mut stack,
        &mut checksum,
    )?;
    Ok(checksum)
}

/// 只算校验和（不取词条）。
///
/// 部署期用它判断"产物还能不能用"——**读一遍源文件是不可避免的代价**，
/// 但比"重新编译一遍"便宜得多。
///
/// # Errors
///
/// 与 [`for_each_entry`] 相同。
pub fn checksum_of(src: &dyn Source, rel_path: &str, display_name: &str) -> Result<u64, DictError> {
    for_each_entry(src, rel_path, display_name, |_, _, _| Ok(()))
}

fn stream_collect<F>(
    src: &dyn Source,
    rel_path: &str,
    display_name: &str,
    f: &mut F,
    stack: &mut Vec<String>,
    checksum: &mut u64,
) -> Result<(), DictError>
where
    F: FnMut(&str, &str, f64) -> Result<(), DictError>,
{
    if stack.iter().any(|p| p == rel_path) {
        return Err(DictError::new(
            display_name,
            0,
            format!(
                "词典循环导入：`{rel_path}` 已在导入链里（{}）。",
                stack.join(" → ")
            ),
        ));
    }
    let text = src.read(rel_path).ok_or_else(|| {
        DictError::new(
            display_name,
            0,
            format!("找不到词典文件 `{rel_path}`（已尝试原路径、加 `.dict.yaml`、加 `.yaml`）"),
        )
    })?;
    // FNV-1a 逐字节地累积——多份文件（含 import）都会并进来，
    // 因此任何一份源文件变了，校验和都会变。
    *checksum = combine(*checksum, text.as_bytes());

    let (header_text, header_start, body_start) = split_sections(&text, rel_path)?;
    let header_node = parse_at(&header_text, header_start).map_err(|e| {
        DictError::new(rel_path, e.line, format!("词典头部解析失败：{}", e.message))
    })?;
    let header = read_header(&header_node, rel_path)?;

    for_each_body_line(&text, body_start, rel_path, |e| {
        f(&e.word, &e.code, e.weight)
    })?;

    stack.push(rel_path.to_owned());
    for import in &header.import_tables {
        stream_collect(src, import, display_name, f, stack, checksum)?;
    }
    stack.pop();
    Ok(())
}

/// FNV-1a 64 位，把一段字节并进已有校验和。
fn combine(h: u64, bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = if h == 0 { OFFSET } else { h };
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// 只读头部（流式路径用不到整个 [`DictFile`] 时）。
///
/// # Errors
///
/// 找不到文件或头部有错时返回 [`DictError`]。
pub fn read_header_only(src: &dyn Source, rel_path: &str) -> Result<DictHeader, DictError> {
    let text = src
        .read(rel_path)
        .ok_or_else(|| DictError::new(rel_path, 0, "找不到词典文件"))?;
    let (header_text, header_start, _) = split_sections(&text, rel_path)?;
    let node = parse_at(&header_text, header_start)
        .map_err(|e| DictError::new(rel_path, e.line, e.message))?;
    read_header(&node, rel_path)
}

// ─────────────────────────────────────────────────────────────────────────────
// import_tables
// ─────────────────────────────────────────────────────────────────────────────

/// 读取词典内容的地方。**抽成 trait 是为了能在没有文件系统的测试里跑。**
pub trait Source {
    /// 按相对于词典目录的路径读取；不存在返回 `None`。
    fn read(&self, rel_path: &str) -> Option<String>;
}

/// 从真实目录读取。
pub struct DirSource {
    root: std::path::PathBuf,
}

impl DirSource {
    /// 以某个目录为根。
    #[must_use]
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl Source for DirSource {
    fn read(&self, rel_path: &str) -> Option<String> {
        // 词典名可能写成 `cn_dicts/base`，也可能写成带扩展名的完整文件名。
        let candidates = [
            self.root.join(rel_path),
            self.root.join(format!("{rel_path}.dict.yaml")),
            self.root.join(format!("{rel_path}.yaml")),
        ];
        for c in candidates {
            if let Ok(s) = std::fs::read_to_string(&c) {
                return Some(s);
            }
        }
        None
    }
}

/// 一份已展开 import 的词典。
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedDict {
    /// 根词典名。
    pub name: String,
    /// 全部词条（根在前，被导入的在后）。
    ///
    /// **顺序有意如此**：RIME 的规则是"有重复词条时，最上面的权重生效"。
    /// 我们把它显式化为"**先出现者优先**"，由调用方按顺序处理。
    pub entries: Vec<RawEntry>,
    /// 展开过程中读过的文件（用于诊断与"部署"决策）。
    pub files: Vec<String>,
}

/// 递归展开 `import_tables`。
///
/// # Errors
///
/// 被导入的词典不存在、循环导入、或某份词典本身解析失败时返回 [`DictError`]。
pub fn load_with_imports(
    src: &dyn Source,
    rel_path: &str,
    display_name: &str,
) -> Result<LoadedDict, DictError> {
    let mut entries = Vec::new();
    let mut files = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut root_name = display_name.to_owned();

    collect(
        src,
        rel_path,
        display_name,
        &mut entries,
        &mut files,
        &mut stack,
        &mut root_name,
    )?;

    Ok(LoadedDict {
        name: root_name,
        entries,
        files,
    })
}

#[allow(clippy::too_many_arguments)]
fn collect(
    src: &dyn Source,
    rel_path: &str,
    display_name: &str,
    entries: &mut Vec<RawEntry>,
    files: &mut Vec<String>,
    stack: &mut Vec<String>,
    root_name: &mut String,
) -> Result<(), DictError> {
    if stack.iter().any(|p| p == rel_path) {
        return Err(DictError::new(
            display_name,
            0,
            format!(
                "词典循环导入：`{rel_path}` 已在导入链里（{}）。",
                stack.join(" → ")
            ),
        ));
    }
    let text = src.read(rel_path).ok_or_else(|| {
        DictError::new(
            display_name,
            0,
            format!("找不到词典文件 `{rel_path}`（已尝试原路径、加 `.dict.yaml`、加 `.yaml`）"),
        )
    })?;

    let dict = parse_dict(&text, rel_path)?;
    files.push(rel_path.to_owned());
    if stack.is_empty() {
        dict.header.name.clone_into(root_name);
    }

    // 根词典自己的词条在前 —— 于是"先出现者优先"这条规则可以直接照做。
    entries.extend(dict.entries.iter().cloned());

    stack.push(rel_path.to_owned());
    for import in &dict.header.import_tables {
        collect(src, import, display_name, entries, files, stack, root_name)?;
    }
    stack.pop();
    Ok(())
}

#[cfg(test)]
// 测试里比较的是同一个字面量解析出来的浮点，逐位相等是确定的，
// 这里用精确比较反而更能发现"解析悄悄变了"。
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const SIMPLE: &str = "\
# 一份最小词典
---
name: stele
version: \"1\"
sort: by_weight
...

你好\tni hao\t10000
世界\tshi jie\t5000
最低\tzui di
";

    #[test]
    fn parses_header_and_entries() {
        let d = parse_dict(SIMPLE, "stele.dict.yaml").unwrap();
        assert_eq!(d.header.name, "stele");
        assert_eq!(d.header.version, "1");
        assert_eq!(d.header.sort.as_deref(), Some("by_weight"));
        assert_eq!(d.entries.len(), 3);
        assert_eq!(d.entries[0].word, "你好");
        assert_eq!(d.entries[0].units(), ["ni", "hao"]);
        assert_eq!(d.entries[0].weight, 10000.0);
        // 省略权重 → 0（最低）。
        assert_eq!(d.entries[2].weight, 0.0);
    }

    #[test]
    fn line_numbers_point_into_the_original_file() {
        let d = parse_dict(SIMPLE, "x").unwrap();
        // 正文第一行（你好）在源文件的第 8 行。
        assert_eq!(d.entries[0].line, 8);
    }

    #[test]
    fn comments_and_blank_lines_in_the_body_are_skipped() {
        let t = "---\nname: a\nversion: \"1\"\n...\n\n# 注释\n甲\ta\t1\n\n乙\tb\t2\n";
        let d = parse_dict(t, "x").unwrap();
        assert_eq!(d.entries.len(), 2);
    }

    #[test]
    fn trailing_comments_on_entries_work() {
        let t = "---\nname: a\nversion: \"1\"\n...\n甲\ta\t1  # 说明\n";
        let d = parse_dict(t, "x").unwrap();
        assert_eq!(d.entries[0].weight, 1.0);
    }

    #[test]
    fn space_separated_body_is_rejected_with_the_reason() {
        let t = "---\nname: a\nversion: \"1\"\n...\n甲 a 1\n";
        let e = parse_dict(t, "x").unwrap_err();
        assert!(e.message.contains("TAB"), "{}", e.message);
        assert!(e.message.contains("空格"), "错误信息应当解释为什么");
    }

    #[test]
    fn duplicate_entries_are_rejected_with_the_reason() {
        let t = "---\nname: a\nversion: \"1\"\n...\n甲\ta\t1\n甲\ta\t9999\n";
        let e = parse_dict(t, "x").unwrap_err();
        assert!(e.message.contains("重复"), "{}", e.message);
    }

    #[test]
    fn same_word_different_reading_is_allowed() {
        // 「掉色」既可以是 diao se 也可以是 diao shai —— 这是两条合法记录。
        let t = "---\nname: a\nversion: \"1\"\n...\n掉色\tdiao se\t4780\n掉色\tdiao shai\t4780\n";
        let d = parse_dict(t, "x").unwrap();
        assert_eq!(d.entries.len(), 2);
    }

    #[test]
    fn missing_markers_are_explained() {
        let e = parse_dict("name: a\n甲\ta\t1\n", "x").unwrap_err();
        assert!(e.message.contains("---"), "{}", e.message);
        let e2 = parse_dict("---\nname: a\nversion: \"1\"\n甲\ta\t1\n", "x").unwrap_err();
        assert!(e2.message.contains("..."), "{}", e2.message);
    }

    #[test]
    fn missing_header_fields_are_reported_with_a_line() {
        let e = parse_dict("---\nversion: \"1\"\n...\n", "x").unwrap_err();
        assert!(e.message.contains("name"), "{}", e.message);
        assert!(e.line > 0, "诊断必须带行号");
    }

    #[test]
    fn bad_weight_is_reported() {
        let t = "---\nname: a\nversion: \"1\"\n...\n甲\ta\t重\n";
        let e = parse_dict(t, "x").unwrap_err();
        assert!(e.message.contains("权重"), "{}", e.message);
    }

    #[test]
    fn four_columns_are_rejected() {
        let t = "---\nname: a\nversion: \"1\"\n...\n甲\ta\t1\t多余\n";
        let e = parse_dict(t, "x").unwrap_err();
        assert!(e.message.contains("三列"), "{}", e.message);
    }

    // ── import_tables ──

    struct Mem(BTreeMap<String, String>);

    impl Source for Mem {
        fn read(&self, rel: &str) -> Option<String> {
            self.0.get(rel).cloned()
        }
    }

    fn mem() -> Mem {
        let mut m = BTreeMap::new();
        m.insert(
            "main".into(),
            "---\nname: main\nversion: \"1\"\nimport_tables:\n  - base\n  - extra\n...\n甲\ta\t1\n"
                .into(),
        );
        m.insert(
            "base".into(),
            "---\nname: base\nversion: \"1\"\n...\n乙\tb\t2\n".into(),
        );
        m.insert(
            "extra".into(),
            "---\nname: extra\nversion: \"1\"\n...\n丙\tc\t3\n".into(),
        );
        Mem(m)
    }

    #[test]
    fn imports_are_expanded_root_first() {
        let d = load_with_imports(&mem(), "main", "main").unwrap();
        let words: Vec<&str> = d.entries.iter().map(|e| e.word.as_str()).collect();
        // 根在前、被导入的在后 —— 于是"先出现者优先"这条规则可以直接照做。
        assert_eq!(words, ["甲", "乙", "丙"]);
        assert_eq!(d.files, ["main", "base", "extra"]);
        assert_eq!(d.name, "main");
    }

    #[test]
    fn missing_import_is_an_error() {
        let mut m = BTreeMap::new();
        m.insert(
            "a".into(),
            "---\nname: a\nversion: \"1\"\nimport_tables:\n  - nope\n...\n".into(),
        );
        let e = load_with_imports(&Mem(m), "a", "a").unwrap_err();
        assert!(e.message.contains("nope"), "{}", e.message);
    }

    #[test]
    fn circular_imports_are_detected() {
        let mut m = BTreeMap::new();
        m.insert(
            "a".into(),
            "---\nname: a\nversion: \"1\"\nimport_tables:\n  - b\n...\n".into(),
        );
        m.insert(
            "b".into(),
            "---\nname: b\nversion: \"1\"\nimport_tables:\n  - a\n...\n".into(),
        );
        let e = load_with_imports(&Mem(m), "a", "a").unwrap_err();
        assert!(e.message.contains("循环导入"), "{}", e.message);
    }
}
