//! # Minimal JSON parser (for `OpenCC` configs)
//!
//! 中文职责：解析 `OpenCC` 的 `*.json` 配置——**只解析这一种文件真正用到的
//! JSON 子集**，但语法上**完整**（不接受"猜一个"的容错）。
//! English role: parse the JSON subset that `OpenCC` configs use, with strict
//! syntax and line-numbered diagnostics.
//! 架构位置：`stele-dict` 内部工具；唯一的调用者是 [`crate::opencc`]。
//!
//! # 为什么手写而不是引入 `serde_json`
//!
//! 与 `stele-config` 手写 YAML 子集同一条理由（PLAN §8 P2 第 3 条）：
//! 我们要**行号**做诊断、要"不支持的语法一律报错并解释原因"，
//! 而通用库给的是"解析失败"或"静默接受"。这里的文件小（emoji.json 352 B）、
//! 结构固定，手写一遍比引入依赖更短、更可审计。
//!
//! # 支持的语法（就是 JSON 的全部）
//!
//! 对象、数组、字符串（含 `\" \\ \/ \b \f \n \r \t \uXXXX` 转义）、
//! 数字、`true` / `false` / `null`。**不支持**注释、单引号字符串、
//! 尾随逗号——JSON 本来就不允许，而容忍它们会让"上游文件坏了"
//! 变成"我们读出半个配置"。

use std::collections::BTreeMap;

/// JSON 值。
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    /// `null`
    Null,
    /// `true` / `false`
    Bool(bool),
    /// 数字。**统一按 `f64` 存**：`OpenCC` 配置里用不到整数语义，
    /// 而多一个整数变体只会让下游多一个分支。
    Number(f64),
    /// 字符串。
    String(String),
    /// 数组。
    Array(Vec<Json>),
    /// 对象。用 `BTreeMap` 而非 `HashMap`：**遍历顺序必须确定**
    /// （它决定多条链的求值顺序，见 PLAN §5 铁律 2）。
    Object(BTreeMap<String, Json>),
}

impl Json {
    /// 取对象里的一个键。
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(m) => m.get(key),
            _ => None,
        }
    }

    /// 当作字符串。
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    /// 当作数组。
    #[must_use]
    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(v) => Some(v),
            _ => None,
        }
    }
}

/// 解析错误，**带行号**。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonError {
    /// 1-based 行号。
    pub line: u32,
    /// 人话解释（说明"哪里不对、JSON 允许什么"）。
    pub message: String,
}

impl core::fmt::Display for JsonError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "第 {} 行：{}", self.line, self.message)
    }
}

impl std::error::Error for JsonError {}

/// 解析一段 JSON 文本。
///
/// # Errors
///
/// 语法错误时返回带行号的 [`JsonError`]；输入末尾有多余内容也算错。
pub fn parse(text: &str) -> Result<Json, JsonError> {
    let mut p = Parser {
        bytes: text.as_bytes(),
        pos: 0,
    };
    // 行号由一个"前缀换行数"的助手算，因此不必给每个 token 记位置。
    let value = p.parse_value(text)?;
    p.skip_ws();
    if p.pos < p.bytes.len() {
        return Err(JsonError {
            line: line_of(text, p.pos),
            message: format!(
                "JSON 文档结束后还有多余内容（从 `{}` 开始）。\
                 一个文件只能有一个顶层值。",
                snippet(text, p.pos)
            ),
        });
    }
    Ok(value)
}

fn line_of(text: &str, byte_pos: usize) -> u32 {
    #[allow(clippy::cast_possible_truncation)]
    // clippy 建议引入 `bytecount` crate——**不行**：本 crate 零第三方依赖
    // （PLAN D9）。而且这里是诊断路径，不是按键路径，朴素计数足够。
    #[allow(clippy::naive_bytecount)]
    let n = text.as_bytes()[..byte_pos.min(text.len())]
        .iter()
        .filter(|b| **b == b'\n')
        .count() as u32;
    n + 1
}

fn snippet(text: &str, pos: usize) -> String {
    text[pos.min(text.len())..]
        .chars()
        .take(16)
        .collect::<String>()
        .replace('\n', "\\n")
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn skip_ws(&mut self) {
        while let Some(b) = self.bytes.get(self.pos) {
            if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn parse_value(&mut self, text: &str) -> Result<Json, JsonError> {
        self.skip_ws();
        let Some(b) = self.peek() else {
            return Err(JsonError {
                line: line_of(text, self.pos),
                message: "输入在这里就结束了，但还期待一个 JSON 值".to_owned(),
            });
        };
        match b {
            b'{' => self.parse_object(text),
            b'[' => self.parse_array(text),
            b'"' => Ok(Json::String(self.parse_string(text)?)),
            b't' => self.parse_literal(text, "true", Json::Bool(true)),
            b'f' => self.parse_literal(text, "false", Json::Bool(false)),
            b'n' => self.parse_literal(text, "null", Json::Null),
            b'-' | b'0'..=b'9' => self.parse_number(text),
            _ => Err(JsonError {
                line: line_of(text, self.pos),
                message: format!(
                    "这里出现了一个不是 JSON 值的字符 `{}`（从 `{}` 开始）。\
                     JSON 的值只能是对象、数组、字符串、数字、true/false/null。",
                    b as char,
                    snippet(text, self.pos)
                ),
            }),
        }
    }

    fn parse_literal(&mut self, text: &str, lit: &str, value: Json) -> Result<Json, JsonError> {
        if self.bytes[self.pos..].starts_with(lit.as_bytes()) {
            self.pos += lit.len();
            Ok(value)
        } else {
            Err(JsonError {
                line: line_of(text, self.pos),
                message: format!(
                    "期待字面量 `{lit}`，但读到的是 `{}`",
                    snippet(text, self.pos)
                ),
            })
        }
    }

    fn parse_object(&mut self, text: &str) -> Result<Json, JsonError> {
        self.pos += 1; // `{`
        let mut map = BTreeMap::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Object(map));
        }
        loop {
            self.skip_ws();
            let line = line_of(text, self.pos);
            if self.peek() != Some(b'"') {
                return Err(JsonError {
                    line,
                    message: format!(
                        "对象的键必须是双引号字符串，但读到的是 `{}`。\
                         JSON 不允许不带引号的键。",
                        snippet(text, self.pos)
                    ),
                });
            }
            let key = self.parse_string(text)?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(JsonError {
                    line: line_of(text, self.pos),
                    message: format!("键 `{key}` 之后缺少 `:`"),
                });
            }
            self.pos += 1;
            let value = self.parse_value(text)?;
            if map.insert(key.clone(), value).is_some() {
                // 重复键：JSON 规范说行为不确定。**报错而不是取最后一个**，
                // 因为"哪个生效"取决于解析器实现，这属于我们必须消除的不确定性。
                return Err(JsonError {
                    line,
                    message: format!(
                        "对象里出现了重复的键 `{key}`。重复键的行为在 JSON 规范里是\
                         未定义的（不同解析器取第一个或最后一个），所以这里直接拒绝。"
                    ),
                });
            }
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Json::Object(map));
                }
                _ => {
                    return Err(JsonError {
                        line: line_of(text, self.pos),
                        message: format!(
                            "对象里期待 `,` 或 `}}`，但读到的是 `{}`",
                            snippet(text, self.pos)
                        ),
                    })
                }
            }
        }
    }

    fn parse_array(&mut self, text: &str) -> Result<Json, JsonError> {
        self.pos += 1; // `[`
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Array(items));
        }
        loop {
            items.push(self.parse_value(text)?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Json::Array(items));
                }
                _ => {
                    return Err(JsonError {
                        line: line_of(text, self.pos),
                        message: format!(
                            "数组里期待 `,` 或 `]`，但读到的是 `{}`",
                            snippet(text, self.pos)
                        ),
                    })
                }
            }
        }
    }

    fn parse_string(&mut self, text: &str) -> Result<String, JsonError> {
        let start_line = line_of(text, self.pos);
        self.pos += 1; // 开引号
        let mut out = String::new();
        loop {
            let Some(b) = self.peek() else {
                return Err(JsonError {
                    line: start_line,
                    message: "字符串没有闭合（缺少结尾的双引号）".to_owned(),
                });
            };
            match b {
                b'"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.pos += 1;
                    let Some(esc) = self.peek() else {
                        return Err(JsonError {
                            line: start_line,
                            message: "字符串在转义符 `\\` 之后就结束了".to_owned(),
                        });
                    };
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.parse_unicode_escape(text)?),
                        other => {
                            return Err(JsonError {
                                line: line_of(text, self.pos - 1),
                                message: format!(
                                    "不认识的转义 `\\{}`。JSON 允许的转义是 \
                                     \\\" \\\\ \\/ \\b \\f \\n \\r \\t \\uXXXX。",
                                    other as char
                                ),
                            })
                        }
                    }
                }
                _ => {
                    // 多字节字符按 UTF-8 整段收进来；按字节切会把中文切坏
                    // （P3 踩过的坑：处理文本一律按 char，不按 u8）。
                    let ch = text[self.pos..].chars().next().ok_or_else(|| JsonError {
                        line: start_line,
                        message: "字符串在中间结束了".to_owned(),
                    })?;
                    if (ch as u32) < 0x20 {
                        return Err(JsonError {
                            line: line_of(text, self.pos),
                            message: format!(
                                "字符串里出现了未转义的控制字符（U+{:04X}）。\
                                 控制字符必须写成 \\uXXXX。",
                                ch as u32
                            ),
                        });
                    }
                    out.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
    }

    fn parse_unicode_escape(&mut self, text: &str) -> Result<char, JsonError> {
        let line = line_of(text, self.pos);
        let Some(hi) = self.read_hex4() else {
            return Err(JsonError {
                line,
                message: "`\\u` 之后必须跟 4 位十六进制数字".to_owned(),
            });
        };
        // 代理对：`\uD83D\uDE00` 是两个 4 位码元合起来的一个字符。
        // **必须处理**，否则 emoji 表里一半的条目会变成非法字符。
        if (0xD800..0xDC00).contains(&hi) {
            let save = self.pos;
            if self.peek() == Some(b'\\') && self.bytes.get(self.pos + 1) == Some(&b'u') {
                self.pos += 2;
                if let Some(lo) = self.read_hex4() {
                    if (0xDC00..0xE000).contains(&lo) {
                        let c = 0x1_0000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                        return char::from_u32(c).ok_or_else(|| JsonError {
                            line,
                            message: format!("代理对算出的码位 U+{c:04X} 不是合法字符"),
                        });
                    }
                }
                self.pos = save;
            }
            return Err(JsonError {
                line,
                message: format!("U+{hi:04X} 是高代理项，后面必须紧跟一个低代理项"),
            });
        }
        char::from_u32(hi).ok_or_else(|| JsonError {
            line,
            message: format!("U+{hi:04X} 不是合法字符"),
        })
    }

    /// 读 4 位十六进制，成功则前进；失败**不消费**输入。
    fn read_hex4(&mut self) -> Option<u32> {
        let s = std::str::from_utf8(self.bytes.get(self.pos..self.pos + 4)?).ok()?;
        let v = u32::from_str_radix(s, 16).ok()?;
        self.pos += 4;
        Some(v)
    }

    fn parse_number(&mut self, text: &str) -> Result<Json, JsonError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        let raw = &text[start..self.pos];
        raw.parse::<f64>().map(Json::Number).map_err(|_| JsonError {
            line: line_of(text, start),
            message: format!("`{raw}` 不是一个合法的 JSON 数字"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_nested_object() {
        let v = parse(r#"{"a": {"b": [1, 2, {"c": "d"}]}, "e": true}"#).unwrap();
        assert_eq!(v.get("e"), Some(&Json::Bool(true)));
        let b = v.get("a").unwrap().get("b").unwrap().as_array().unwrap();
        assert_eq!(b.len(), 3);
        assert_eq!(b[2].get("c").unwrap().as_str(), Some("d"));
    }

    #[test]
    fn parses_escapes_including_utf16_surrogates() {
        let v = parse(r#""a\tb\u0041\uD83D\uDE00""#).unwrap();
        // \uD83D\uDE00 是 😀（U+1F600）。
        assert_eq!(v.as_str(), Some("a\tbA\u{1F600}"));
    }

    #[test]
    fn keeps_chinese_intact() {
        let v = parse(r#"{"扭曲": "扭曲 🫪"}"#).unwrap();
        assert_eq!(v.get("扭曲").unwrap().as_str(), Some("扭曲 🫪"));
    }

    #[test]
    fn duplicate_keys_are_rejected() {
        let e = parse(r#"{"a": 1, "a": 2}"#).unwrap_err();
        assert!(e.message.contains("重复的键"), "{}", e.message);
    }

    #[test]
    fn unquoted_keys_are_rejected_with_the_reason() {
        let e = parse("{a: 1}").unwrap_err();
        assert!(e.message.contains("双引号"), "{}", e.message);
    }

    #[test]
    fn trailing_content_is_rejected() {
        let e = parse("{} {}").unwrap_err();
        assert!(e.message.contains("多余内容"), "{}", e.message);
    }

    #[test]
    fn trailing_comma_is_rejected() {
        let e = parse("[1, 2,]").unwrap_err();
        assert_eq!(e.line, 1);
        assert!(
            e.message.contains(']') || e.message.contains("值"),
            "{}",
            e.message
        );
    }

    #[test]
    fn line_numbers_are_reported() {
        let e = parse("{\n  \"a\": 1,\n  \"b\": @\n}").unwrap_err();
        assert_eq!(e.line, 3, "{}", e.message);
    }

    #[test]
    fn unterminated_string_is_reported() {
        let e = parse("\"abc").unwrap_err();
        assert!(e.message.contains("没有闭合"), "{}", e.message);
    }

    #[test]
    fn lone_high_surrogate_is_rejected() {
        let e = parse(r#""\uD83D""#).unwrap_err();
        assert!(e.message.contains("高代理项"), "{}", e.message);
    }
}
