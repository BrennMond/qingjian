//! # Config value model
//!
//! 中文职责：配置数据的中间表示——一个**保留顺序、带行号**的值树。
//! English role: the intermediate representation of configuration data —
//! an order-preserving value tree that remembers line numbers.
//! 架构位置：`stele-config` 的核心类型，被解析器与补丁层共用。
//!
//! # 为什么不用 `serde` + `serde_yaml`
//!
//! 三条理由，任何一条单独成立都足够：
//!
//! 1. **RIME 的方案文件不是普通 YAML。** 它有 `__include` / `__patch` /
//!    `__append` / `?` 可选目标 / `@n` 列表寻址这些**自定义指令**，
//!    它们作用于"节点"而不是"值"。`serde` 的数据模型里**没有节点身份**，
//!    到 `Value` 那一步这些信息已经丢了。
//! 2. **我们需要行号。** PLAN D17 要求"配置错误必须响亮且可读"——
//!    "`translators/main` 的 `dictionary` 字段引用了不存在的词库"比
//!    "invalid type: string"有用得多，而后者正是 serde 的默认输出。
//! 3. **顺序是语义。** 按键绑定、滤镜顺序都依赖列表顺序；而 map 的**书写顺序**
//!    决定 `__patch` 的求值次序（RIME 明确规定"引用 → 合并同级字面值 → 补丁子节点"）。
//!    用 `HashMap` 语义的映射类型会丢掉它。

use core::fmt;

/// 一个配置节点：值 + 它在源文件里的行号。
///
/// 行号是**诊断质量的全部**：没有它，报错只能说"配置有错"。
#[derive(Clone, Debug)]
pub struct Node {
    /// 节点的值。
    pub value: Value,
    /// 在源文件里的行号（从 1 开始）。由代码构造的节点填 0。
    pub line: u32,
}

impl Node {
    /// 由值构造，行号未知（0）。
    #[must_use]
    pub fn new(value: Value) -> Self {
        Self { value, line: 0 }
    }

    /// 由值 + 行号构造。
    #[must_use]
    pub fn at(value: Value, line: u32) -> Self {
        Self { value, line }
    }

    /// 字符串值。
    #[must_use]
    pub fn str(s: impl Into<String>) -> Self {
        Self::new(Value::Str(s.into()))
    }

    /// 整数。
    #[must_use]
    pub fn int(i: i64) -> Self {
        Self::new(Value::Int(i))
    }

    /// 序列。
    #[must_use]
    pub fn seq(items: Vec<Node>) -> Self {
        Self::new(Value::Seq(items))
    }

    /// 映射。
    #[must_use]
    pub fn map(entries: Vec<(String, Node)>) -> Self {
        Self::new(Value::Map(entries))
    }

    /// 当作字符串读。
    ///
    /// 数字与布尔也会被接受并转成字符串——YAML 里 `version: 1.0` 是浮点数，
    /// 但用户的意思是"版本号 1.0"，为此报错是刁难人。
    #[must_use]
    pub fn as_str(&self) -> Option<String> {
        match &self.value {
            Value::Str(s) => Some(s.clone()),
            Value::Int(i) => Some(i.to_string()),
            Value::Float(f) => Some(format_float(*f)),
            Value::Bool(b) => Some(b.to_string()),
            // 空值、列表、映射都没有"字符串形态"。
            _ => None,
        }
    }

    /// 当作布尔读。**只接受真正的布尔**，不接受 "yes"/"1" 之类的猜测。
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self.value {
            Value::Bool(b) => Some(b),
            _ => None,
        }
    }

    /// 当作整数读。
    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self.value {
            Value::Int(i) => Some(i),
            _ => None,
        }
    }

    /// 当作浮点读（整数会被提升）。
    ///
    /// 词条权重经常写成整数，所以这里必须接受 `Int`。
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        // 权重是相对词频，现实取值远小于 2^53 —— 这里的精度损失不可能发生。
        // 而且这个转换只发生在**装载期**，不进按键路径。
        #[allow(clippy::cast_precision_loss)]
        match self.value {
            Value::Float(f) => Some(f),
            Value::Int(i) => Some(i as f64),
            _ => None,
        }
    }

    /// 当作序列读。
    #[must_use]
    pub fn as_seq(&self) -> Option<&[Node]> {
        match &self.value {
            Value::Seq(v) => Some(v),
            _ => None,
        }
    }

    /// 当作映射读。
    #[must_use]
    pub fn as_map(&self) -> Option<&[(String, Node)]> {
        match &self.value {
            Value::Map(m) => Some(m),
            _ => None,
        }
    }

    /// 按 key 取子节点。
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Node> {
        self.as_map()?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    /// 值的种类名（用于诊断）。
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self.value {
            Value::Null => "空",
            Value::Bool(_) => "布尔",
            Value::Int(_) => "整数",
            Value::Float(_) => "小数",
            Value::Str(_) => "字符串",
            Value::Seq(_) => "列表",
            Value::Map(_) => "映射",
        }
    }

    /// 把值渲染成一行摘要（诊断里用）。
    #[must_use]
    pub fn brief(&self) -> String {
        match &self.value {
            Value::Null => "null".into(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => format_float(*f),
            Value::Str(s) => {
                if s.chars().count() > 24 {
                    format!("\"{}…\"", s.chars().take(24).collect::<String>())
                } else {
                    format!("\"{s}\"")
                }
            }
            Value::Seq(v) => format!("[{} 项]", v.len()),
            Value::Map(m) => format!("{{{} 项}}", m.len()),
        }
    }
}

impl PartialEq for Node {
    /// **比较忽略行号**——行号是诊断元数据，不是值的一部分。
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

/// 把浮点渲染成"像人写的那样"的字符串。
///
/// `1.0` 的默认 `Display` 是 `1`，而 YAML 里写的是 `1.0`——
/// 在版本号这类场景下这个差别会让人困惑。
#[must_use]
fn format_float(f: f64) -> String {
    let s = format!("{f}");
    if s.contains('.') || s.contains('e') || s.contains("inf") || s.contains("NaN") {
        s
    } else {
        format!("{s}.0")
    }
}

/// 一个配置值。
///
/// 有意**不含带标签的节点引用**：RIME 的 `__include` 语义是**复制**
/// （被引用的节点永不被修改），所以引用在解析后就被展开，不留在数据里。
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// 空（`key:` 后面什么都没有）。
    Null,
    /// 布尔。
    Bool(bool),
    /// 整数。
    Int(i64),
    /// 小数。
    Float(f64),
    /// 字符串。
    Str(String),
    /// 序列。
    Seq(Vec<Node>),
    /// 映射。**保留书写顺序**——顺序参与语义（见模块文档）。
    Map(Vec<(String, Node)>),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", Node::new(self.clone()).brief())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nodes_compare_ignoring_line_numbers() {
        let a = Node::at(Value::Int(1), 10);
        let b = Node::at(Value::Int(1), 999);
        assert_eq!(a, b, "行号是诊断元数据，不该参与相等判断");
    }

    #[test]
    fn numbers_read_back_as_strings() {
        // YAML 里 `version: 1.0` 会被解析成浮点，但用户的意思是版本号。
        assert_eq!(
            Node::new(Value::Float(1.0)).as_str().as_deref(),
            Some("1.0")
        );
        assert_eq!(Node::new(Value::Int(2)).as_str().as_deref(), Some("2"));
    }

    #[test]
    fn float_renders_like_a_human_wrote_it() {
        assert_eq!(format_float(1.0), "1.0");
        assert_eq!(format_float(0.5), "0.5");
    }

    #[test]
    fn bool_is_not_guessed_from_strings() {
        // "yes" / "1" 这类猜测是大量配置事故的来源，我们不做。
        assert_eq!(Node::str("yes").as_bool(), None);
        assert_eq!(Node::new(Value::Bool(true)).as_bool(), Some(true));
    }

    #[test]
    fn weights_accept_integers() {
        assert_eq!(Node::int(6170).as_f64(), Some(6170.0));
        assert_eq!(Node::new(Value::Float(1.5)).as_f64(), Some(1.5));
    }

    #[test]
    fn map_lookup_preserves_order() {
        let m = Node::map(vec![("b".into(), Node::int(2)), ("a".into(), Node::int(1))]);
        let keys: Vec<&str> = m
            .as_map()
            .unwrap()
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();
        assert_eq!(keys, ["b", "a"], "书写顺序必须保留");
        assert_eq!(m.get("a").unwrap().as_int(), Some(1));
        assert!(m.get("zzz").is_none());
    }

    #[test]
    fn kind_names_are_for_humans() {
        assert_eq!(Node::str("x").kind(), "字符串");
        assert_eq!(Node::seq(vec![]).kind(), "列表");
        assert_eq!(Node::new(Value::Null).kind(), "空");
    }
}
