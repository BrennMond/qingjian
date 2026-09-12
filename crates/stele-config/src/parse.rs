//! # YAML subset parser
//!
//! 中文职责：解析 rime 风格配置文件真正用到的那部分 YAML。
//! English role: parse the subset of YAML that rime-style config files actually use.
//! 架构位置：`stele-config` 的前端；输出 [`Node`] 值树。
//!
//! # 支持的子集（**明确列出，不含糊**）
//!
//! | 语法 | 支持 | 说明 |
//! | --- | --- | --- |
//! | 注释 `#` | ✅ | 引号内的 `#` 不算注释 |
//! | 块映射（缩进） | ✅ | |
//! | 块序列（`- `） | ✅ | 含 `- key: value` 形式的映射项 |
//! | 纯量：空/布尔/整数/小数/字符串 | ✅ | |
//! | 单引号、双引号字符串 | ✅ | 双引号支持 `\n` `\t` `\\` `\"` 转义 |
//! | 块标量 `\|` 与 `>` | ✅ | `description` 那一类多行文本 |
//! | 流序列 `[a, b]`、流映射 `{a: b}` | ✅ | 按键绑定里大量使用 |
//! | 文档标记 `---` / `...` | ✅ | 词典文件靠它分隔头部与正文 |
//! | **锚点与别名 `&` / `*`** | ❌ | 明确报错，不静默当成字符串 |
//! | **标签 `!!`、多行纯量折叠的完整规则** | ❌ | 明确报错 |
//! | **制表符缩进** | ❌ | YAML 本身就禁止；报错并说明原因 |
//!
//! **不支持的东西一律报错，而不是"猜一个"**——PLAN D17 要求配置错误响亮。
//! 一个被静默当成字符串的 `*alias` 会变成极难排查的行为异常。
//!
//! # 与 RIME 的关系
//!
//! RIME 的 `__include` / `__patch` 等**编译指令不是 YAML 语法**，
//! 它们是解析之后的节点级操作，所以不在这里处理——见 [`crate::patch`]。

use crate::value::{Node, Value};

/// 解析错误：**一定带行号**。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// 出错的物理行号（从 1 开始）。
    pub line: u32,
    /// 人话解释。
    pub message: String,
}

impl core::fmt::Display for ParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "第 {} 行：{}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

/// 解析一段 YAML 文本。
///
/// # Errors
///
/// 语法错误、不支持的语法、制表符缩进等都会返回带行号的 [`ParseError`]。
pub fn parse(text: &str) -> Result<Node, ParseError> {
    parse_at(text, 1)
}

/// 解析一段 YAML 文本，并把行号偏移 `base_line - 1`。
///
/// 用于"从一个大文件里切出一段来解析"的场景（词典文件的头部）——
/// 这样报错给出的仍是**原文件的行号**。
///
/// # Errors
///
/// 同 [`parse`]。
pub fn parse_at(text: &str, base_line: u32) -> Result<Node, ParseError> {
    let lines = scan_lines(text, base_line)?;
    let mut p = Parser { lines, pos: 0 };
    if p.lines.is_empty() {
        return Ok(Node::new(Value::Null));
    }
    let indent = p.lines[0].indent;
    p.parse_block(indent)
}

/// 一行有效内容。
#[derive(Clone, Debug)]
struct Line {
    /// 缩进宽度（空格数）。
    indent: usize,
    /// 去掉缩进与注释之后的内容。
    content: String,
    /// 物理行号。
    no: u32,
}

/// 把原始文本扫描成有效行：去空行、去注释、算缩进、查制表符。
fn scan_lines(text: &str, base_line: u32) -> Result<Vec<Line>, ParseError> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        // 行号从 1 开始；调用方可能要求从原文件的某一行开始计数。
        #[allow(clippy::cast_possible_truncation)]
        let no = base_line + i as u32;

        // 制表符缩进：YAML 明确禁止。**解释原因**，别只说"语法错误"。
        let leading = raw.len() - raw.trim_start_matches([' ', '\t']).len();
        if raw[..leading].contains('\t') {
            return Err(ParseError {
                line: no,
                message: "缩进里出现了制表符。YAML 只允许空格缩进——\
                          制表符与空格的宽度在不同编辑器里不一致，会让同一份文件\
                          在不同机器上解析出不同结构。"
                    .into(),
            });
        }

        let stripped = strip_comment(raw);
        let trimmed = stripped.trim_end();
        if trimmed.trim().is_empty() {
            continue;
        }

        // 文档标记单独成行，调用方（词典解析）需要它们，故保留。
        let content = trimmed.trim_start();
        if content == "---" || content == "..." {
            out.push(Line {
                indent: 0,
                content: content.to_owned(),
                no,
            });
            continue;
        }

        out.push(Line {
            indent: leading,
            content: content.to_owned(),
            no,
        });
    }
    Ok(out)
}

/// 去掉行尾注释。**引号内的 `#` 不算注释。**
fn strip_comment(raw: &str) -> &str {
    let bytes = raw.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '\\' if in_double => {
                i += 1; // 跳过被转义的字符
            }
            // 只有"前面是空白或行首"才是注释开始。
            // `a#b` 是合法的纯量（例如颜色值、URL 片段）。
            '#' if !in_single
                && !in_double
                && (i == 0 || (bytes[i - 1] as char).is_whitespace()) =>
            {
                return &raw[..i];
            }
            _ => {}
        }
        i += 1;
    }
    raw
}

struct Parser {
    lines: Vec<Line>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Line> {
        self.lines.get(self.pos)
    }

    /// 解析给定缩进处的一个块（映射或序列）。
    fn parse_block(&mut self, indent: usize) -> Result<Node, ParseError> {
        let Some(line) = self.peek() else {
            return Ok(Node::new(Value::Null));
        };
        if line.indent < indent {
            return Ok(Node::new(Value::Null));
        }
        if line.indent > indent {
            return Err(ParseError {
                line: line.no,
                message: format!(
                    "缩进不一致：期望 {} 个空格，实际 {} 个。\
                     YAML 靠缩进表达层级，多一个或少一个空格都会改变结构。",
                    indent, line.indent
                ),
            });
        }
        if line.content == "---" || line.content == "..." {
            self.pos += 1;
            return self.parse_block(indent);
        }
        if is_seq_item(&line.content) {
            self.parse_sequence(indent)
        } else {
            self.parse_mapping(indent, None)
        }
    }

    /// 解析序列。
    fn parse_sequence(&mut self, indent: usize) -> Result<Node, ParseError> {
        let start_line = self.peek().map_or(0, |l| l.no);
        let mut items = Vec::new();

        while let Some(line) = self.peek() {
            if line.indent != indent || !is_seq_item(&line.content) {
                break;
            }
            let content = line.content.clone();
            let no = line.no;
            let extra = &content[1..]; // 去掉 '-'
            let rest_owned = extra.trim_start().to_owned();
            let rest: &str = &rest_owned;
            let indent_delta = extra.len() - rest.len() + 1; // '- ' 之后的列
            let item_indent = indent + indent_delta;

            if rest.is_empty() {
                self.pos += 1;
                let next_indent = self.peek().map(|l| l.indent);
                let v = match next_indent {
                    Some(ni) if ni > indent => self.parse_block(ni)?,
                    _ => Node::at(Value::Null, no),
                };
                items.push(v);
                continue;
            }

            // 流式集合要先判断：`- { when: paging }` 里的冒号是**流映射内部**的冒号，
            // 若先按 `key: value` 拆，键会变成 `{ when`——一个很难看出来的解析错误。
            let starts_flow = rest.starts_with('{') || rest.starts_with('[');

            // `- key: value` —— 这是一个映射项的第一行，后续同层键在 item_indent。
            if !starts_flow {
                if let Some((k, v)) = split_key_value(rest) {
                    self.pos += 1;
                    let first = self.value_after_key(v, no, item_indent)?;
                    items.push(self.parse_mapping(item_indent, Some((k, first, no)))?);
                    continue;
                }
            }

            // `- scalar` / `- [flow]` / `- {flow}`
            self.pos += 1;
            items.push(self.parse_inline(rest, no)?);
        }

        Ok(Node::at(Value::Seq(items), start_line))
    }

    /// 解析映射。`first` 用于 `- key: value` 这种"已经在序列里开了一个头"的情形。
    fn parse_mapping(
        &mut self,
        indent: usize,
        first: Option<(String, Node, u32)>,
    ) -> Result<Node, ParseError> {
        let mut entries: Vec<(String, Node)> = Vec::new();
        let start_line = first
            .as_ref()
            .map_or_else(|| self.peek().map_or(0, |l| l.no), |(_, _, no)| *no);

        if let Some((k, v, _)) = first {
            entries.push((k, v));
        }

        while let Some(line) = self.peek().cloned() {
            if line.content == "---" || line.content == "..." {
                break;
            }
            if line.indent < indent {
                break;
            }
            if line.indent > indent {
                return Err(ParseError {
                    line: line.no,
                    message: format!(
                        "缩进不一致：上一层的键在第 {} 列，这里却缩进到第 {} 列。\
                         同一个映射里的键必须左对齐。",
                        indent + 1,
                        line.indent + 1
                    ),
                });
            }
            if is_seq_item(&line.content) {
                // 同一层既有映射又有序列 —— 结构上不可能。
                return Err(ParseError {
                    line: line.no,
                    message: "同一层里混用了「键: 值」与「- 列表项」。\
                              列表项必须整体属于某一个键。"
                        .into(),
                });
            }

            let Some((key, raw_value)) = split_key_value(&line.content) else {
                return Err(ParseError {
                    line: line.no,
                    message: format!(
                        "这一行不是「键: 值」的形式：`{}`。\
                         若想写多行文本，请用块标量（`键: |`）。",
                        line.content
                    ),
                });
            };

            if entries.iter().any(|(k, _)| *k == key) {
                return Err(ParseError {
                    line: line.no,
                    message: format!(
                        "键 `{key}` 重复了。YAML 里重复的键是未定义行为\
                         （不同实现取的可能是第一个也可能是最后一个），因此我们直接报错。"
                    ),
                });
            }

            self.pos += 1;
            let v = self.value_after_key(raw_value, line.no, indent)?;
            entries.push((key, v));
        }

        Ok(Node::at(Value::Map(entries), start_line))
    }

    /// 处理 `key:` 之后的部分：块标量、嵌套块、或行内值。
    fn value_after_key(
        &mut self,
        raw: &str,
        no: u32,
        parent_indent: usize,
    ) -> Result<Node, ParseError> {
        let raw = raw.trim();

        // 块标量：`|` / `|-` / `>` / `>-`
        if let Some(style) = block_scalar_style(raw) {
            return self.parse_block_scalar(style, no, parent_indent);
        }

        if raw.is_empty() {
            // 值在下一行（缩进更深）；若没有更深的行，就是空值。
            let next = self.peek().map(|l| l.indent);
            return match next {
                Some(ni) if ni > parent_indent => self.parse_block(ni),
                _ => Ok(Node::at(Value::Null, no)),
            };
        }

        self.parse_inline(raw, no)
    }

    /// 块标量：把后续更深缩进的行原样收集起来。
    // 返回 `Result` 是为与同一层的其它解析函数保持**统一的调用形状**
    // （调用方一律 `?`）。块标量本身不会出错，但混用两种形状更难读。
    #[allow(clippy::unnecessary_wraps)]
    fn parse_block_scalar(
        &mut self,
        style: BlockStyle,
        no: u32,
        parent_indent: usize,
    ) -> Result<Node, ParseError> {
        let mut raw_lines: Vec<(usize, String)> = Vec::new();
        while let Some(line) = self.peek() {
            if line.indent <= parent_indent {
                break;
            }
            raw_lines.push((line.indent, line.content.clone()));
            self.pos += 1;
        }
        if raw_lines.is_empty() {
            return Ok(Node::at(Value::Str(String::new()), no));
        }

        let min_indent = raw_lines.iter().map(|(i, _)| *i).min().unwrap_or(0);
        let text_lines: Vec<&str> = raw_lines
            .iter()
            .map(|(i, c)| if *i >= min_indent { c.as_str() } else { "" })
            .collect();

        let mut text = match style {
            // 字面量：换行就是换行。
            BlockStyle::Literal | BlockStyle::LiteralNoTrailing => text_lines.join("\n"),
            // 折叠：单个换行变空格，空行仍是换行。
            BlockStyle::Folded | BlockStyle::FoldedNoTrailing => {
                let mut s = String::new();
                for (i, l) in text_lines.iter().enumerate() {
                    if i > 0 {
                        if l.is_empty() {
                            s.push('\n');
                        } else {
                            s.push(' ');
                        }
                    }
                    s.push_str(l);
                }
                s
            }
        };
        if style.keep_trailing_newline() {
            text.push('\n');
        }
        Ok(Node::at(Value::Str(text), no))
    }

    /// 解析行内值：流序列、流映射、或纯量。
    // 同上：形状统一，故保留 `&mut self` 与 `Result`。
    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    fn parse_inline(&mut self, raw: &str, no: u32) -> Result<Node, ParseError> {
        let s = raw.trim();
        if s.starts_with('[') {
            return parse_flow(s, no).map(|n| Node::at(n.value, no));
        }
        if s.starts_with('{') {
            return parse_flow(s, no).map(|n| Node::at(n.value, no));
        }
        if s.starts_with('&') || s.starts_with('*') {
            return Err(ParseError {
                line: no,
                message: format!(
                    "不支持 YAML 的锚点与别名（`{s}`）。\
                     方案文件里没有用到它们，而静默当成字符串会变成极难排查的行为异常。\
                     需要复用时请用 `$ref` 引用另一个节点。"
                ),
            });
        }
        if s.starts_with("!!") || s.starts_with('!') {
            return Err(ParseError {
                line: no,
                message: format!("不支持 YAML 标签（`{s}`）。"),
            });
        }
        Ok(Node::at(parse_scalar(s), no))
    }
}

/// 块标量的风格。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockStyle {
    /// `|` 字面量。
    Literal,
    /// `>` 折叠。
    Folded,
    /// `|-` / `>-`：不要末尾换行。
    LiteralNoTrailing,
    /// `>-`。
    FoldedNoTrailing,
}

impl BlockStyle {
    fn keep_trailing_newline(self) -> bool {
        matches!(self, Self::Literal | Self::Folded)
    }
}

fn block_scalar_style(s: &str) -> Option<BlockStyle> {
    match s {
        "|" => Some(BlockStyle::Literal),
        "|-" => Some(BlockStyle::LiteralNoTrailing),
        ">" => Some(BlockStyle::Folded),
        ">-" => Some(BlockStyle::FoldedNoTrailing),
        _ => None,
    }
}

/// 是不是一个序列项（`-` 后面跟空白，或就是单独的 `-`）。
fn is_seq_item(content: &str) -> bool {
    content == "-" || content.starts_with("- ")
}

/// 把 `key: value` 拆开。**只认第一个"冒号+空白"或行尾冒号。**
///
/// 这一点很重要：`- when: paging` 里的 `when` 是键，而
/// `{ accept: comma }` 里的冒号也在流映射里——所以拆分必须尊重引号。
fn split_key_value(s: &str) -> Option<(String, &str)> {
    let bytes = s.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '\\' if in_double => i += 1,
            ':' if !in_single && !in_double => {
                let at_end = i + 1 == bytes.len();
                let followed_by_space = !at_end && (bytes[i + 1] as char).is_whitespace();
                if at_end || followed_by_space {
                    let key = s[..i].trim();
                    let value = if at_end { "" } else { s[i + 1..].trim() };
                    if key.is_empty() {
                        return None;
                    }
                    return Some((unquote_key(key), value));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// 键可以是带引号的（例如 `"engine/translators/@before 0"`）。
fn unquote_key(k: &str) -> String {
    let t = k.trim();
    if t.len() >= 2
        && ((t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')))
    {
        let inner = &t[1..t.len() - 1];
        return if t.starts_with('"') {
            unescape_double(inner)
        } else {
            inner.replace("''", "'")
        };
    }
    t.to_owned()
}

/// 解析纯量。
fn parse_scalar(s: &str) -> Value {
    let t = s.trim();
    if t.is_empty() {
        return Value::Null;
    }
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        return Value::Str(unescape_double(&t[1..t.len() - 1]));
    }
    if t.len() >= 2 && t.starts_with('\'') && t.ends_with('\'') {
        return Value::Str(t[1..t.len() - 1].replace("''", "'"));
    }
    match t {
        "~" | "null" | "Null" | "NULL" => return Value::Null,
        "true" | "True" | "TRUE" => return Value::Bool(true),
        "false" | "False" | "FALSE" => return Value::Bool(false),
        _ => {}
    }
    // 整数（允许下划线分隔，YAML 1.1 的写法）。
    if let Ok(i) = t.replace('_', "").parse::<i64>() {
        return Value::Int(i);
    }
    if let Ok(f) = t.replace('_', "").parse::<f64>() {
        return Value::Float(f);
    }
    Value::Str(t.to_owned())
}

/// 双引号里的转义。
fn unescape_double(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            // 反斜杠与"未知转义"的兜底都产生一个反斜杠，合并写。
            Some('\\') | None => out.push('\\'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            Some(other) => {
                // 未知转义：保留反斜杠，不静默吞掉——吞掉会让路径类字符串出错。
                out.push('\\');
                out.push(other);
            }
        }
    }
    out
}

/// 解析流式集合（`[...]` / `{...}`），支持嵌套。
fn parse_flow(s: &str, no: u32) -> Result<Node, ParseError> {
    let mut p = Flow {
        s: s.as_bytes(),
        pos: 0,
        no,
    };
    let v = p.value()?;
    p.skip_ws();
    if p.pos < p.s.len() {
        return Err(ParseError {
            line: no,
            message: format!("流式集合后面还有多余内容：`{}`", &s[p.pos..]),
        });
    }
    Ok(Node::at(v, no))
}

struct Flow<'a> {
    s: &'a [u8],
    pos: usize,
    no: u32,
}

impl Flow<'_> {
    fn skip_ws(&mut self) {
        while self.pos < self.s.len() && (self.s[self.pos] as char).is_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.s.get(self.pos).map(|b| *b as char)
    }

    fn value(&mut self) -> Result<Value, ParseError> {
        self.skip_ws();
        match self.peek() {
            Some('[') => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    if self.peek() == Some(']') {
                        self.pos += 1;
                        break;
                    }
                    items.push(Node::at(self.value()?, self.no));
                    self.skip_ws();
                    match self.peek() {
                        Some(',') => self.pos += 1,
                        Some(']') => {
                            self.pos += 1;
                            break;
                        }
                        other => {
                            return Err(
                                self.err(format!("流序列里期望 `,` 或 `]`，遇到 {other:?}"))
                            );
                        }
                    }
                }
                Ok(Value::Seq(items))
            }
            Some('{') => {
                self.pos += 1;
                let mut entries: Vec<(String, Node)> = Vec::new();
                loop {
                    self.skip_ws();
                    if self.peek() == Some('}') {
                        self.pos += 1;
                        break;
                    }
                    let key = self.raw_token(&[',', ':', '}'])?;
                    self.skip_ws();
                    if self.peek() != Some(':') {
                        return Err(self.err(format!("流映射里的键 `{key}` 后面缺少 `:`")));
                    }
                    self.pos += 1;
                    let v = self.value()?;
                    if entries.iter().any(|(k, _)| *k == key) {
                        return Err(self.err(format!("流映射里的键 `{key}` 重复了")));
                    }
                    entries.push((key, Node::at(v, self.no)));
                    self.skip_ws();
                    match self.peek() {
                        Some(',') => self.pos += 1,
                        Some('}') => {
                            self.pos += 1;
                            break;
                        }
                        other => {
                            return Err(
                                self.err(format!("流映射里期望 `,` 或 `}}`，遇到 {other:?}"))
                            );
                        }
                    }
                }
                Ok(Value::Map(entries))
            }
            _ => {
                let tok = self.raw_token(&[',', ']', '}', ':'])?;
                Ok(parse_scalar(&tok))
            }
        }
    }

    /// 读一段原始文本（到任一终止字符为止），处理引号。
    // 形状统一（见上），故保留 `Result`。
    #[allow(clippy::unnecessary_wraps)]
    fn raw_token(&mut self, stops: &[char]) -> Result<String, ParseError> {
        self.skip_ws();
        let mut out = String::new();
        let mut quoted: Option<char> = None;
        while let Some(c) = self.peek() {
            if let Some(q) = quoted {
                self.pos += 1;
                if c == q {
                    quoted = None;
                } else if c == '\\' && q == '"' {
                    if let Some(e) = self.peek() {
                        out.push('\\');
                        out.push(e);
                        self.pos += 1;
                        continue;
                    }
                }
                out.push(c);
                continue;
            }
            if stops.contains(&c) {
                break;
            }
            if c == '"' || c == '\'' {
                quoted = Some(c);
                self.pos += 1;
                continue;
            }
            out.push(c);
            self.pos += 1;
        }
        Ok(out.trim().to_owned())
    }

    fn err(&self, message: String) -> ParseError {
        ParseError {
            line: self.no,
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Node {
        parse(s).unwrap_or_else(|e| panic!("解析失败：{e}\n源：\n{s}"))
    }

    #[test]
    fn parses_flat_mapping() {
        let n = p("name: rime_ice\nversion: \"2026-01-26\"\nsort: by_weight\n");
        assert_eq!(n.get("name").unwrap().as_str().as_deref(), Some("rime_ice"));
        assert_eq!(
            n.get("version").unwrap().as_str().as_deref(),
            Some("2026-01-26")
        );
    }

    #[test]
    fn parses_nested_mapping() {
        let n = p("engine:\n  processors:\n    - speller\n    - selector\n");
        let procs = n.get("engine").unwrap().get("processors").unwrap();
        let items: Vec<&str> = procs
            .as_seq()
            .unwrap()
            .iter()
            .map(|i| i.as_str().unwrap())
            .map(|s| Box::leak(s.into_boxed_str()) as &str)
            .collect();
        assert_eq!(items, ["speller", "selector"]);
    }

    #[test]
    fn parses_sequence_of_mappings() {
        // 这正是按键绑定的写法。
        let n = p("bindings:\n  - { when: paging, accept: comma }\n  - when: has_menu\n    accept: period\n");
        let b = n.get("bindings").unwrap().as_seq().unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(
            b[0].get("when").unwrap().as_str().as_deref(),
            Some("paging")
        );
        assert_eq!(
            b[0].get("accept").unwrap().as_str().as_deref(),
            Some("comma")
        );
        assert_eq!(
            b[1].get("accept").unwrap().as_str().as_deref(),
            Some("period")
        );
    }

    #[test]
    fn parses_block_scalar() {
        let n = p("description: |\n  第一行\n  第二行\nname: x\n");
        let d = n.get("description").unwrap().as_str().unwrap();
        assert!(d.contains("第一行"), "{d:?}");
        assert!(d.contains("第二行"), "{d:?}");
        assert_eq!(n.get("name").unwrap().as_str().as_deref(), Some("x"));
    }

    #[test]
    fn block_scalar_dash_drops_trailing_newline() {
        let n = p("x: |-\n  a\n");
        assert_eq!(n.get("x").unwrap().as_str().as_deref(), Some("a"));
    }

    #[test]
    fn folded_scalar_joins_lines() {
        let n = p("x: >\n  a\n  b\n");
        assert_eq!(n.get("x").unwrap().as_str().unwrap().trim(), "a b");
    }

    #[test]
    fn comments_are_stripped_but_not_inside_quotes() {
        let n = p("a: 1 # 注释\nb: \"x # 不是注释\"\nc: 'y # 也不是'\nd: url#fragment\n");
        assert_eq!(n.get("a").unwrap().as_int(), Some(1));
        assert_eq!(
            n.get("b").unwrap().as_str().as_deref(),
            Some("x # 不是注释")
        );
        assert_eq!(n.get("c").unwrap().as_str().as_deref(), Some("y # 也不是"));
        // `#` 前面没有空白 → 不是注释。
        assert_eq!(
            n.get("d").unwrap().as_str().as_deref(),
            Some("url#fragment")
        );
    }

    #[test]
    fn scalars_are_classified() {
        let n = p("s: hello\ni: 42\nf: 0.5\nt: true\nz: ~\nq: \"42\"\n");
        assert!(matches!(n.get("s").unwrap().value, Value::Str(_)));
        assert_eq!(n.get("i").unwrap().as_int(), Some(42));
        assert_eq!(n.get("f").unwrap().as_f64(), Some(0.5));
        assert_eq!(n.get("t").unwrap().as_bool(), Some(true));
        assert!(matches!(n.get("z").unwrap().value, Value::Null));
        // 引号里的 42 是字符串，不是数字。
        assert!(matches!(n.get("q").unwrap().value, Value::Str(_)));
    }

    #[test]
    fn dict_header_shape_parses() {
        // 真实词典文件的头部形状。
        let n = p("---\nname: rime_ice\nversion: \"2026-01-26\"\nimport_tables:\n  - cn_dicts/8105\n  - cn_dicts/base\n...\n");
        assert_eq!(n.get("name").unwrap().as_str().as_deref(), Some("rime_ice"));
        let imports = n.get("import_tables").unwrap().as_seq().unwrap();
        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].as_str().as_deref(), Some("cn_dicts/8105"));
    }

    #[test]
    fn flow_sequences_nest() {
        let n = p("a: [1, [2, 3], {k: v}]\n");
        let seq = n.get("a").unwrap().as_seq().unwrap();
        assert_eq!(seq[0].as_int(), Some(1));
        assert_eq!(seq[1].as_seq().unwrap().len(), 2);
        assert_eq!(seq[2].get("k").unwrap().as_str().as_deref(), Some("v"));
    }

    #[test]
    fn keys_may_be_quoted_and_contain_colons() {
        let n = p("\"engine/translators/@before 0\": predict_translator\n");
        assert!(n.get("engine/translators/@before 0").is_some());
    }

    #[test]
    fn empty_value_is_null() {
        let n = p("a:\nb: x\n");
        assert!(matches!(n.get("a").unwrap().value, Value::Null));
    }

    // ── 错误路径：每一条都要求"报错且说人话" ──

    #[test]
    fn tab_indentation_is_rejected_with_a_reason() {
        let e = parse("a:\n\tb: 1\n").unwrap_err();
        assert!(e.message.contains("制表符"), "{}", e.message);
        assert!(e.message.contains("YAML"), "错误信息应当解释原因");
    }

    #[test]
    fn duplicate_keys_are_rejected() {
        let e = parse("a: 1\na: 2\n").unwrap_err();
        assert!(e.message.contains("重复"), "{}", e.message);
    }

    #[test]
    fn anchors_are_rejected_loudly() {
        let e = parse("a: &anchor 1\nb: *anchor\n").unwrap_err();
        assert!(e.message.contains("锚点"), "{}", e.message);
        assert!(e.message.contains("$ref"), "应当指出替代方案");
    }

    #[test]
    fn inconsistent_indent_is_rejected() {
        let e = parse("a:\n    b: 1\n  c: 2\n").unwrap_err();
        assert!(e.message.contains("缩进"), "{}", e.message);
    }

    #[test]
    fn errors_carry_real_line_numbers_even_with_offset() {
        let e = parse_at("a: 1\n\tb: 2\n", 100).unwrap_err();
        assert_eq!(e.line, 101, "偏移后的行号必须是原文件的行号");
    }

    #[test]
    fn mixed_map_and_seq_at_one_level_is_rejected() {
        let e = parse("a: 1\n- b\n").unwrap_err();
        assert!(e.message.contains("混用"), "{}", e.message);
    }
}
