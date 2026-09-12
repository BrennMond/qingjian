//! # Regex (purpose-built)
//!
//! 中文职责：为拼写代数实现的正则子集——**够用、可读、零依赖**。
//! English role: a regex subset for the spelling algebra — sufficient, readable,
//! dependency-free.
//! 架构位置：`stele-engine` 的内部工具，被 [`crate::spelling`] 的代数运算使用。
//!
//! # 为什么要自己写
//!
//! 拼写代数需要正则（RIME 用 `boost::regex`）。我们零依赖，所以只有两条路：
//! 引入 `regex` crate，或者写一个够用的子集。
//!
//! **选后者**，因为需要的语法很窄——真实方案里的规则长这样：
//!
//! ```text
//! xform/^([nl])ue$/$1ve/
//! derive/^([zcs])h/$1/
//! abbrev/^([a-z]).+$/$1/
//! erase/^hm$/
//! ```
//!
//! **用不到的复杂特性一律不支持并报错**，而不是"大概能跑"。
//! 一条被静默误解的规则会变成"某些词就是打不出来"，那是最难查的一类问题。
//!
//! # 支持 / 不支持
//!
//! | 支持 | 说明 |
//! | --- | --- |
//! | 字面字符 | `abc` |
//! | `.` | 任意字符 |
//! | `[...]` `[^...]` `[a-z]` | 字符类 |
//! | `\d` `\w` `\s` 及大写取反 | 预定义类 |
//! | `(...)` | **捕获组**（替换式里用 `$1`…`$9` 引用） |
//! | `\|` | 选择 `a\|b` |
//! | `*` `+` `?` `{n}` `{n,}` `{n,m}` | 量词（贪婪） |
//! | `^` `$` | 锚点 |
//! | `\.` `\*` 等 | 转义 |
//!
//! **不支持**：非贪婪 `*?`、反向引用 `\1`、环视 `(?=)`、命名组、Unicode 属性类。
//! 遇到就报错。
//!
//! # 匹配方式
//!
//! 回溯法（continuation-passing），不是自动机。对拼写规则这种**短模式、
//! 短文本**的场景，回溯法更简单也更容易读；不存在指数爆炸的风险面
//! （模式是方案作者写死的，输入是一个音节，几个字符）。

use std::collections::BTreeMap;

/// 编译后的正则。
#[derive(Clone, Debug)]
pub struct Regex {
    root: Node,
    /// 捕获组个数（不含整体匹配的第 0 组）。
    pub groups: usize,
    /// 原模式（诊断用）。
    pub source: String,
}

/// 编译错误。
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegexError {
    /// 括号不配对。
    UnbalancedParen,
    /// 字符类没有闭合。
    UnclosedClass,
    /// 量词用在了没有可重复对象的位置。
    DanglingQuantifier,
    /// 语法本身不支持。
    Unsupported(String),
    /// 模式为空。
    Empty,
}

impl std::fmt::Display for RegexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnbalancedParen => write!(f, "括号不配对"),
            Self::UnclosedClass => write!(f, "字符类 `[` 没有闭合的 `]`"),
            Self::DanglingQuantifier => write!(f, "量词（* + ? {{}}）前面没有可重复的东西"),
            Self::Unsupported(m) => write!(
                f,
                "不支持的正则语法：{m}。本引擎只实现拼写代数用得上的子集——\
                 遇到不支持的语法一律报错，而不是'大概能跑'"
            ),
            Self::Empty => write!(f, "空模式"),
        }
    }
}

impl std::error::Error for RegexError {}

#[derive(Clone, Debug)]
enum Node {
    Empty,
    Char(char),
    Any,
    Class {
        neg: bool,
        items: Vec<ClassItem>,
    },
    Start,
    End,
    Group(Box<Node>, usize),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat {
        node: Box<Node>,
        min: u32,
        max: Option<u32>,
    },
}

#[derive(Clone, Debug)]
enum ClassItem {
    Char(char),
    Range(char, char),
    Digit(bool),
    Word(bool),
    Space(bool),
}

/// 一次匹配的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// 匹配的起始字符下标。
    pub start: usize,
    /// 匹配的结束字符下标（不含）。
    pub end: usize,
    /// 捕获组：下标 i 对应 `$i`；`None` 表示该组未参与。
    pub groups: Vec<Option<(usize, usize)>>,
}

impl Regex {
    /// 编译一个模式。
    ///
    /// # Errors
    ///
    /// 语法不支持或书写有误时返回 [`RegexError`]。
    pub fn compile(pattern: &str) -> Result<Self, RegexError> {
        if pattern.is_empty() {
            return Err(RegexError::Empty);
        }
        let chars: Vec<char> = pattern.chars().collect();
        let mut p = Parser {
            chars,
            pos: 0,
            groups: 0,
        };
        let root = p.parse_alt()?;
        if p.pos != p.chars.len() {
            // 只剩一个 `)` 会出现这种情况。
            return Err(RegexError::UnbalancedParen);
        }
        Ok(Self {
            root,
            groups: p.groups,
            source: pattern.to_owned(),
        })
    }

    /// 是否**整串**匹配（`erase` 用的是这个语义）。
    #[must_use]
    pub fn is_full_match(&self, text: &str) -> bool {
        let chars: Vec<char> = text.chars().collect();
        let mut caps = vec![None; self.groups + 1];
        if !match_node(&self.root, &chars, 0, &mut caps, &mut |pos, _caps| {
            pos == chars.len()
        }) {
            return false;
        }
        true
    }

    /// 找第一处匹配（**子串**语义）。
    #[must_use]
    pub fn find(&self, text: &str) -> Option<Match> {
        let chars: Vec<char> = text.chars().collect();
        for start in 0..=chars.len() {
            let mut caps = vec![None; self.groups + 1];
            let mut end: Option<usize> = None;
            if match_node(&self.root, &chars, start, &mut caps, &mut |pos, _caps| {
                end = Some(pos);
                true
            }) {
                if let Some(e) = end {
                    return Some(Match {
                        start,
                        end: e,
                        groups: caps,
                    });
                }
            }
        }
        None
    }

    /// 全局替换。**没有匹配时返回原串。**
    #[must_use]
    pub fn replace_all(&self, text: &str, replacement: &str) -> String {
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::with_capacity(text.len());
        let mut pos = 0usize;
        while pos <= chars.len() {
            let mut found: Option<(usize, Match)> = None;
            for start in pos..=chars.len() {
                let mut caps = vec![None; self.groups + 1];
                let mut end: Option<usize> = None;
                if match_node(&self.root, &chars, start, &mut caps, &mut |p, _caps| {
                    end = Some(p);
                    true
                }) {
                    if let Some(e) = end {
                        found = Some((
                            start,
                            Match {
                                start,
                                end: e,
                                groups: caps,
                            },
                        ));
                        break;
                    }
                }
            }
            if let Some((start, m)) = found {
                {
                    for c in &chars[pos..start] {
                        out.push(*c);
                    }
                    out.push_str(&expand(replacement, &chars, &m));
                    if m.end == m.start {
                        // 零宽匹配：推进一步，避免死循环。
                        if m.end < chars.len() {
                            out.push(chars[m.end]);
                        }
                        pos = m.end + 1;
                    } else {
                        pos = m.end;
                    }
                }
            } else {
                for c in &chars[pos..] {
                    out.push(*c);
                }
                break;
            }
        }
        out
    }
}

/// 把替换式里的 `$0`…`$9` 展开。
fn expand(replacement: &str, text: &[char], m: &Match) -> String {
    let mut out = String::with_capacity(replacement.len());
    let chars: Vec<char> = replacement.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // `$$` → 字面 `$`
        if c == '$' && i + 1 < chars.len() {
            if chars[i + 1] == '$' {
                out.push('$');
                i += 2;
                continue;
            }
            if let Some(d) = chars[i + 1].to_digit(10) {
                let idx = d as usize;
                if let Some(Some((a, b))) = m.groups.get(idx) {
                    for ch in &text[*a..*b] {
                        out.push(*ch);
                    }
                    i += 2;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// 解析
// ─────────────────────────────────────────────────────────────────────────────

struct Parser {
    chars: Vec<char>,
    pos: usize,
    groups: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn parse_alt(&mut self) -> Result<Node, RegexError> {
        let mut branches = vec![self.parse_concat()?];
        while self.peek() == Some('|') {
            self.pos += 1;
            branches.push(self.parse_concat()?);
        }
        Ok(if branches.len() == 1 {
            branches.pop().unwrap_or(Node::Empty)
        } else {
            Node::Alt(branches)
        })
    }

    fn parse_concat(&mut self) -> Result<Node, RegexError> {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.parse_atom()?;
            items.push(self.parse_quantifier(atom)?);
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.pop().unwrap_or(Node::Empty),
            _ => Node::Concat(items),
        })
    }

    fn parse_atom(&mut self) -> Result<Node, RegexError> {
        let c = self.bump().ok_or(RegexError::DanglingQuantifier)?;
        match c {
            '(' => {
                // 不支持 `(?...)` 形式。
                if self.peek() == Some('?') {
                    return Err(RegexError::Unsupported(
                        "`(?` 开头的分组（环视、非捕获组、命名组）".into(),
                    ));
                }
                self.groups += 1;
                let idx = self.groups;
                let inner = self.parse_alt()?;
                if self.bump() != Some(')') {
                    return Err(RegexError::UnbalancedParen);
                }
                Ok(Node::Group(Box::new(inner), idx))
            }
            ')' => Err(RegexError::UnbalancedParen),
            '[' => self.parse_class(),
            '.' => Ok(Node::Any),
            '^' => Ok(Node::Start),
            '$' => Ok(Node::End),
            '\\' => self.parse_escape(),
            '*' | '+' | '?' => Err(RegexError::DanglingQuantifier),
            other => Ok(Node::Char(other)),
        }
    }

    fn parse_escape(&mut self) -> Result<Node, RegexError> {
        let c = self
            .bump()
            .ok_or(RegexError::Unsupported("末尾的反斜杠".into()))?;
        Ok(match c {
            'd' => Node::Class {
                neg: false,
                items: vec![ClassItem::Digit(false)],
            },
            'D' => Node::Class {
                neg: false,
                items: vec![ClassItem::Digit(true)],
            },
            'w' => Node::Class {
                neg: false,
                items: vec![ClassItem::Word(false)],
            },
            'W' => Node::Class {
                neg: false,
                items: vec![ClassItem::Word(true)],
            },
            's' => Node::Class {
                neg: false,
                items: vec![ClassItem::Space(false)],
            },
            'S' => Node::Class {
                neg: false,
                items: vec![ClassItem::Space(true)],
            },
            '1'..='9' => {
                return Err(RegexError::Unsupported(
                    "反向引用（`\\1`）。替换式里用 `$1` 引用捕获组".into(),
                ))
            }
            other => Node::Char(other),
        })
    }

    fn parse_class(&mut self) -> Result<Node, RegexError> {
        let neg = if self.peek() == Some('^') {
            self.pos += 1;
            true
        } else {
            false
        };
        let mut items = Vec::new();
        loop {
            // `bump()` **消费**掉这个字符 —— 因此遇到 `]` 时直接返回，
            // 不要再到外面找第二个 `]`（那正是上一版把 `[nl]` 判成"未闭合"的原因）。
            let c = self.bump().ok_or(RegexError::UnclosedClass)?;
            if c == ']' {
                if items.is_empty() {
                    // `[]` 里的第一个 `]` 是字面量。
                    items.push(ClassItem::Char(']'));
                    continue;
                }
                return Ok(Node::Class { neg, items });
            }
            if c == '\\' {
                let e = self.bump().ok_or(RegexError::UnclosedClass)?;
                items.push(match e {
                    'd' => ClassItem::Digit(false),
                    'D' => ClassItem::Digit(true),
                    'w' => ClassItem::Word(false),
                    'W' => ClassItem::Word(true),
                    's' => ClassItem::Space(false),
                    'S' => ClassItem::Space(true),
                    other => ClassItem::Char(other),
                });
                continue;
            }
            // `a-z`
            if self.peek() == Some('-') {
                let save = self.pos;
                self.pos += 1;
                match self.peek() {
                    Some(hi) if hi != ']' => {
                        self.pos += 1;
                        items.push(ClassItem::Range(c, hi));
                        continue;
                    }
                    _ => self.pos = save,
                }
            }
            items.push(ClassItem::Char(c));
        }
    }

    fn parse_quantifier(&mut self, atom: Node) -> Result<Node, RegexError> {
        let (min, max) = match self.peek() {
            Some('*') => {
                self.pos += 1;
                (0, None)
            }
            Some('+') => {
                self.pos += 1;
                (1, None)
            }
            Some('?') => {
                self.pos += 1;
                (0, Some(1))
            }
            // 只有能解析成 `{n}` / `{n,}` / `{n,m}` 时才算量词，
            // 否则当字面量 `{`（拼写里可能出现）。
            Some('{') => {
                let save = self.pos;
                self.pos += 1;
                let mut lo = String::new();
                while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                    lo.push(self.bump().unwrap_or('0'));
                }
                if lo.is_empty() {
                    self.pos = save;
                    return Ok(atom);
                }
                let lo: u32 = lo
                    .parse()
                    .map_err(|_| RegexError::Unsupported("{n} 太大".into()))?;
                match self.peek() {
                    Some('}') => {
                        self.pos += 1;
                        (lo, Some(lo))
                    }
                    Some(',') => {
                        self.pos += 1;
                        let mut hi = String::new();
                        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                            hi.push(self.bump().unwrap_or('0'));
                        }
                        if self.bump() != Some('}') {
                            return Err(RegexError::Unsupported("`{n,m}` 没有闭合".into()));
                        }
                        let hi = if hi.is_empty() {
                            None
                        } else {
                            Some(
                                hi.parse()
                                    .map_err(|_| RegexError::Unsupported("{n,m} 太大".into()))?,
                            )
                        };
                        (lo, hi)
                    }
                    _ => return Err(RegexError::Unsupported("`{` 量词没有闭合".into())),
                }
            }
            _ => return Ok(atom),
        };

        // 非贪婪与占有量词不支持。
        if self.peek() == Some('?') {
            return Err(RegexError::Unsupported("非贪婪量词 `*?`".into()));
        }
        if self.peek() == Some('+') {
            return Err(RegexError::Unsupported("占有量词 `*+`".into()));
        }
        Ok(Node::Repeat {
            node: Box::new(atom),
            min,
            max,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 匹配
// ─────────────────────────────────────────────────────────────────────────────

type Caps = Vec<Option<(usize, usize)>>;

/// 从 `pos` 起尝试匹配，成功则调用 `k` 并返回其结果。
///
/// 用**续延（continuation）**而不是"返回所有可能终点"，是因为量词需要
/// 在"多匹配一点"和"让后面也能匹配上"之间回溯——这正是正则回溯的本质。
fn match_node(
    node: &Node,
    text: &[char],
    pos: usize,
    caps: &mut Caps,
    k: &mut dyn FnMut(usize, &mut Caps) -> bool,
) -> bool {
    match node {
        Node::Empty => k(pos, caps),
        Node::Char(c) => {
            if text.get(pos) == Some(c) {
                k(pos + 1, caps)
            } else {
                false
            }
        }
        Node::Any => {
            if pos < text.len() {
                k(pos + 1, caps)
            } else {
                false
            }
        }
        Node::Class { neg, items } => match text.get(pos) {
            Some(c) if class_matches(*neg, items, *c) => k(pos + 1, caps),
            _ => false,
        },
        Node::Start => {
            if pos == 0 {
                k(pos, caps)
            } else {
                false
            }
        }
        Node::End => {
            if pos == text.len() {
                k(pos, caps)
            } else {
                false
            }
        }
        Node::Group(inner, idx) => {
            let saved = caps.get(*idx).copied().flatten();
            let start = pos;
            let ok = match_node(inner, text, pos, caps, &mut |end, caps| {
                let prev = caps.get(*idx).copied().flatten();
                if let Some(slot) = caps.get_mut(*idx) {
                    *slot = Some((start, end));
                }
                if k(end, caps) {
                    return true;
                }
                if let Some(slot) = caps.get_mut(*idx) {
                    *slot = prev;
                }
                false
            });
            if !ok {
                if let Some(slot) = caps.get_mut(*idx) {
                    *slot = saved;
                }
            }
            ok
        }
        Node::Concat(items) => match_seq(items, text, pos, caps, k),
        Node::Alt(branches) => {
            for b in branches {
                let saved = caps.clone();
                if match_node(b, text, pos, caps, k) {
                    return true;
                }
                *caps = saved;
            }
            false
        }
        Node::Repeat { node, min, max } => match_repeat(node, text, pos, caps, *min, *max, 0, k),
    }
}

fn match_seq(
    items: &[Node],
    text: &[char],
    pos: usize,
    caps: &mut Caps,
    k: &mut dyn FnMut(usize, &mut Caps) -> bool,
) -> bool {
    match items.split_first() {
        None => k(pos, caps),
        Some((head, rest)) => match_node(head, text, pos, caps, &mut |p, caps| {
            match_seq(rest, text, p, caps, &mut *k)
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn match_repeat(
    node: &Node,
    text: &[char],
    pos: usize,
    caps: &mut Caps,
    min: u32,
    max: Option<u32>,
    done: u32,
    k: &mut dyn FnMut(usize, &mut Caps) -> bool,
) -> bool {
    // 贪婪：**先试"再多匹配一次"**，失败了才退回"到此为止"。
    let can_more = max.is_none_or(|m| done < m);
    if can_more {
        let saved = caps.clone();
        let advanced = match_node(node, text, pos, caps, &mut |p, caps| {
            if p == pos {
                // 零宽重复：不再递归，避免死循环。
                return false;
            }
            match_repeat(node, text, p, caps, min, max, done + 1, &mut *k)
        });
        if advanced {
            return true;
        }
        *caps = saved;
    }
    if done >= min {
        return k(pos, caps);
    }
    false
}

fn class_matches(neg: bool, items: &[ClassItem], c: char) -> bool {
    let hit = items.iter().any(|it| match it {
        ClassItem::Char(x) => *x == c,
        ClassItem::Range(a, b) => *a <= c && c <= *b,
        ClassItem::Digit(n) => c.is_ascii_digit() != *n,
        ClassItem::Word(n) => (c.is_alphanumeric() || c == '_') != *n,
        ClassItem::Space(n) => c.is_whitespace() != *n,
    });
    hit != neg
}

/// 供诊断：把编译出来的结构渲染成树（调试用）。
#[must_use]
pub fn describe(re: &Regex) -> String {
    format!("{:?}", re.root)
}

/// 捕获组的引用表（供 `$1` 之外的调试用途）。
#[must_use]
pub fn group_count(re: &Regex) -> usize {
    re.groups
}

/// 便于诊断的辅助：模式 → 组数映射。
#[must_use]
pub fn group_index(patterns: &[&str]) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for p in patterns {
        if let Ok(re) = Regex::compile(p) {
            m.insert((*p).to_owned(), re.groups);
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn re(p: &str) -> Regex {
        Regex::compile(p).unwrap_or_else(|e| panic!("编译 {p} 失败：{e}"))
    }

    #[test]
    fn literals_and_full_match() {
        let r = re("abc");
        assert!(r.is_full_match("abc"));
        assert!(!r.is_full_match("abcd"));
        assert!(!r.is_full_match("xabc"));
    }

    #[test]
    fn anchors() {
        assert!(re("^ab").find("xab").is_none());
        assert_eq!(re("ab$").find("xab").unwrap().end, 3);
        assert!(re("^abc$").is_full_match("abc"));
    }

    #[test]
    fn classes_and_ranges() {
        assert!(re("^[a-z]+$").is_full_match("hello"));
        assert!(!re("^[a-z]+$").is_full_match("Hello"));
        assert!(re("^[^0-9]+$").is_full_match("abc"));
        assert!(re("^[abc]$").is_full_match("b"));
        assert!(re(r"^\d+$").is_full_match("123"));
        assert!(re(r"^\w+$").is_full_match("a_1"));
    }

    #[test]
    fn quantifiers() {
        assert!(re("^a*$").is_full_match(""));
        assert!(re("^a*$").is_full_match("aaa"));
        assert!(re("^a+$").is_full_match("a"));
        assert!(!re("^a+$").is_full_match(""));
        assert!(re("^ab?c$").is_full_match("ac"));
        assert!(re("^ab?c$").is_full_match("abc"));
        assert!(re("^a{2,3}$").is_full_match("aa"));
        assert!(re("^a{2,3}$").is_full_match("aaa"));
        assert!(!re("^a{2,3}$").is_full_match("a"));
        assert!(re("^a{2}$").is_full_match("aa"));
        assert!(!re("^a{2}$").is_full_match("aaa"));
        assert!(re("^a{2,}$").is_full_match("aaaaa"));
    }

    #[test]
    fn greedy_backtracking_lets_the_tail_match() {
        // 贪婪的 `.*` 必须先吃掉全部，再回溯让 `c` 匹配上。
        assert!(re("^.*c$").is_full_match("abc"));
        assert_eq!(re("^(a*)b$").find("aaab").unwrap().groups[1], Some((0, 3)));
    }

    #[test]
    fn alternation() {
        assert!(re("^(cat|dog)$").is_full_match("dog"));
        assert!(re("^(cat|dog)$").is_full_match("cat"));
        assert!(!re("^(cat|dog)$").is_full_match("cow"));
    }

    #[test]
    fn captures_are_recorded() {
        let m = re("^([nl])ue$").find("nue").unwrap();
        assert_eq!(m.groups[1], Some((0, 1)));
        let m2 = re("^([zcs])h$").find("zh").unwrap();
        assert_eq!(m2.groups[1], Some((0, 1)));
    }

    #[test]
    fn replace_all_expands_capture_refs() {
        // 真实方案里的规则。
        assert_eq!(re("^([nl])ue$").replace_all("nue", "$1ve"), "nve");
        assert_eq!(re("^([nl])ue$").replace_all("lue", "$1ve"), "lve");
        assert_eq!(re("^([zcs])h").replace_all("zhang", "$1"), "zang");
        // 未匹配 → 原样返回。
        assert_eq!(re("^([zcs])h").replace_all("abc", "$1"), "abc");
    }

    #[test]
    fn replace_all_handles_the_abbrev_rule() {
        // rime-ice 的缩写规则：每个音节取首字母。
        let r = re("^([a-z]).+$");
        assert_eq!(r.replace_all("ni", "$1"), "n");
        assert_eq!(r.replace_all("hao", "$1"), "h");
        // 单字母音节不匹配（`(.+)` 要求后面至少还有一个字符）。
        assert_eq!(r.replace_all("a", "$1"), "a");
    }

    #[test]
    fn erase_uses_full_match_semantics() {
        // `erase/^hm$/` 只删掉"整个拼写就是 hm"的那一个。
        let r = re("^hm$");
        assert!(r.is_full_match("hm"));
        assert!(!r.is_full_match("hmm"));
        assert!(!r.is_full_match("ahm"));
    }

    #[test]
    fn global_replace_touches_every_match() {
        let r = re("a");
        assert_eq!(r.replace_all("banana", "o"), "bonono");
        assert_eq!(r.replace_all("bbb", "o"), "bbb");
    }

    #[test]
    fn zero_width_matches_terminate() {
        let r = re("x*");
        // 不应当死循环。
        let out = r.replace_all("abc", "-");
        assert!(out.contains('a'));
    }

    #[test]
    fn unsupported_syntax_is_rejected_with_a_reason() {
        let e = Regex::compile("(?=x)").unwrap_err();
        assert!(e.to_string().contains("不支持"), "{e}");

        let e2 = Regex::compile("a*?").unwrap_err();
        assert!(e2.to_string().contains("非贪婪"), "{e2}");

        let e3 = Regex::compile(r"(a)\1").unwrap_err();
        assert!(e3.to_string().contains("反向引用"), "{e3}");

        assert_eq!(
            Regex::compile("[abc").unwrap_err(),
            RegexError::UnclosedClass
        );
        assert_eq!(
            Regex::compile("(ab").unwrap_err(),
            RegexError::UnbalancedParen
        );
        assert_eq!(
            Regex::compile("*a").unwrap_err(),
            RegexError::DanglingQuantifier
        );
        assert_eq!(Regex::compile("").unwrap_err(), RegexError::Empty);
    }

    #[test]
    fn brace_without_digits_is_a_literal() {
        // 拼写本身可能含 `{`，那时它不该被当成量词。
        assert!(re("^a{b$").is_full_match("a{b"));
    }

    #[test]
    fn escape_makes_metacharacters_literal() {
        assert!(re(r"^a\.b$").is_full_match("a.b"));
        assert!(!re(r"^a\.b$").is_full_match("axb"));
        assert!(re(r"^\*$").is_full_match("*"));
    }

    #[test]
    fn group_count_is_reported() {
        assert_eq!(group_count(&re("^(a)(b)$")), 2);
        assert_eq!(group_count(&re("^ab$")), 0);
        assert_eq!(group_index(&["^(a)b$", "^c(d)(e)$"]).len(), 2);
        assert!(!describe(&re("^a$")).is_empty());
    }
}
