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
//! 回溯法（continuation-passing），不是自动机。
//!
//! # ⚠️ 复杂度与预算（P0-H 的教训）
//!
//! 旧版本的这一节写着「不存在指数爆炸的风险面（模式是方案作者写死的，
//! 输入是一个音节，几个字符）」。**那句话是错的，而且被实测证伪**：
//!
//! | 模式 | 输入 | 实测 |
//! | --- | --- | --- |
//! | `^(a+)+$` | 20 个 `a` + `b` | 约 54 ms |
//! | 同上 | 24 个 | 约 848 ms |
//! | 同上 | 28 个 | 3 秒超时（审计的中止线） |
//!
//! 问题不在"模式是谁写的"，而在**同一份实现也被 `recognizer` 用**——
//! 那里的输入是用户敲的任意长度字符串，不是音节。注释里的假设
//! 与真实的调用点不一致，这就是缺陷本身。
//!
//! 现在有三道闸：
//!
//! 1. **编译期**拒绝"重复套重复"（[`RegexError::NestedRepeat`]）——
//!    指数爆炸的经典形态。这是保守规则：真实方案的规则里没有这种写法。
//! 2. **运行期**步数与深度预算（[`Regex::with_step_budget`]）：超限时
//!    **当作不匹配**并置位，绝不无声地跑上三秒。
//! 3. 递归深度上限，避免栈溢出。
//!
//! 第 1、2 条是**缓解**，不是终点：最终的替换是"保证多项式时间的
//! 自动机（NFA/DFA）或经审计的正则引擎"，设计见
//! `docs/regex-engine-design.md`。在那一页落地之前，这道闸必须留着。

use std::collections::BTreeMap;

/// 一次匹配默认允许的回溯步数。
///
/// 取值依据：真实方案的规则在短音节上只需个位数到几十步；
/// `recognizer` 的输入最长也就是一条输入行（几十个字符）。
/// `100_000` 比真实需要宽三到四个数量级，同时把最坏耗时锁在毫秒级。
pub const DEFAULT_STEP_BUDGET: u64 = 100_000;

/// 递归深度上限。
///
/// # 它为什么必须存在，以及它的代价
///
/// 回溯匹配的"迭代"是靠递归实现的：`a+` 每多吃一个字符就深一层。
/// 没有上限时，一条 20 万字符的输入会在触发步数预算**之前**先把栈打穿
/// （实测：`^a+$` 对 20 万个 `a` 直接 SIGABRT）。
///
/// 代价是明确的：**超过这个长度的输入不会被量词匹配上**。
/// 对 `recognizer` 的前缀模式（`^uU[a-f0-9]+$` 这类）这不构成问题——
/// 真实输入是几个到几十个字符。等到把回溯引擎换成自动机
/// （见 `docs/regex-engine-design.md`），这条限制会一起消失。
const MAX_DEPTH: u32 = 1024;

/// 编译后的正则。
#[derive(Clone, Debug)]
pub struct Regex {
    root: Node,
    /// 捕获组个数（不含整体匹配的第 0 组）。
    pub groups: usize,
    /// 原模式（诊断用）。
    pub source: String,
    /// 一次匹配的步数预算（见模块文档的复杂度一节）。
    pub step_budget: u64,
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
    /// **重复套重复**：指数回溯的经典形态。
    NestedRepeat,
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
            Self::NestedRepeat => write!(
                f,
                "不支持**重复套重复**（例如 `(a+)+`、`(ab*)*`）：这种写法会让\
                 回溯匹配的耗时随输入长度指数增长（实测 `^(a+)+$` 对 24 个 a \
                 要 848 ms，28 个超过 3 秒）。同一份实现也被 `recognizer` 用，\
                 而那里的输入长度由用户决定。请把它改写成不含嵌套量词的形式，\
                 或拆成多条规则。"
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
        // 编译期闸门：重复套重复会让回溯爆炸（P0-H）。宁可装载期响亮失败，
        // 也不要运行期在某次按键上卡三秒。
        if contains_nested_repeat(&root, false) {
            return Err(RegexError::NestedRepeat);
        }
        Ok(Self {
            root,
            groups: p.groups,
            source: pattern.to_owned(),
            step_budget: DEFAULT_STEP_BUDGET,
        })
    }

    /// 改小/改大步数预算（测试与压力回归用）。
    #[must_use]
    pub fn with_step_budget(mut self, steps: u64) -> Self {
        self.step_budget = steps.max(1);
        self
    }

    /// **全部匹配都必须以这个字面串开头**——保守地取，拿不准就返回空串。
    ///
    /// # 这是 `recognizer` 的"字面前缀"快路径的正确版本
    ///
    /// 旧版本用字符串扫描近似抽取（`^` 之后到第一个元字符之前），
    /// 于是把"某一分支/可选项的首字符"当成了必需前缀，**漏识别**：
    ///
    /// | 正则 | 输入 | 正则自己 | 旧 `leading` | 后果 |
    /// | --- | --- | --- | --- | --- |
    /// | `^(a\|b)+$` | `bbb` | 匹配 | `"a"` | 未认领 |
    /// | `^https?://.*$` | `http://x` | 匹配 | `"https"` | 未认领 |
    /// | `^a?b$` | `b` | 匹配 | `"a"` | 未认领 |
    ///
    /// 正确版本直接从**语法树**上取：跳过开头的 `^`，然后连续吃掉
    /// 字面 `Char` 节点，遇到任何别的节点（分组、量词、选择、字符类、
    /// 锚点…）就停。这**可证明**是全部匹配的公共必需前缀——
    /// `Concat` 要求各元素从左到右依次匹配，而 `Char` 只匹配它自己。
    ///
    /// 只对"从位置 0 开始匹配"成立（[`Regex::match_prefix_len`] 的语义）；
    /// [`Regex::find`] 会从任意位置起匹配，不能用它做剪枝。
    #[must_use]
    pub fn required_prefix(&self) -> String {
        required_prefix_of(&self.root)
    }

    /// 是否**整串**匹配（`erase` 用的是这个语义）。
    #[must_use]
    pub fn is_full_match(&self, text: &str) -> bool {
        let chars: Vec<char> = text.chars().collect();
        let mut caps = vec![None; self.groups + 1];
        let b = Budget::new(self.step_budget);
        if !match_node(
            &self.root,
            &chars,
            0,
            &mut caps,
            &b,
            0,
            &mut |pos, _caps| pos == chars.len(),
        ) {
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
            let b = Budget::new(self.step_budget);
            if match_node(
                &self.root,
                &chars,
                start,
                &mut caps,
                &b,
                0,
                &mut |pos, _caps| {
                    end = Some(pos);
                    true
                },
            ) {
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

    /// 从**开头**匹配；返回匹配结束的**字符**下标。
    ///
    /// `require_to_end = true` 时要求"恰好匹配到末尾"才算命中——
    /// 这就是方案数据里那个 `$` 的意思（`^uU[a-z]+$` 要打到底才算认出）。
    ///
    /// # 为什么要有这个方法（而不是复用 `find` + 判断 `start == 0`）
    ///
    /// 因为两个需求撞在一起了：
    ///
    /// - **前缀模式**（`^uU[a-z]+$`）要"边打边认"：敲到 `uUn` 时就应该
    ///   认出 `uUn` 这一段。用 `find` 做不到——它是整串匹配，
    ///   `uUn` 匹配不上 `^uU[a-z]+$` 的末尾锚。
    /// - **必须以某字符结尾的模式**（`^;.*;$`）要的恰好是整串匹配。
    ///
    /// 一个方法 + 一个布尔量把两者统一了。返回的是**字符**下标，
    /// 调用方负责换算成字节（[`crate::segmentor`] 的认领按字节记）。
    #[must_use]
    pub fn match_prefix_len(&self, text: &str, require_to_end: bool) -> Option<usize> {
        let chars: Vec<char> = text.chars().collect();
        let mut caps = vec![None; self.groups + 1];
        let mut end: Option<usize> = None;
        let b = Budget::new(self.step_budget);
        let hit = match_node(
            &self.root,
            &chars,
            0,
            &mut caps,
            &b,
            0,
            &mut |pos, _caps| {
                if require_to_end && pos != chars.len() {
                    return false;
                }
                end = Some(pos);
                true
            },
        );
        if hit {
            end
        } else {
            None
        }
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
                let b = Budget::new(self.step_budget);
                if match_node(
                    &self.root,
                    &chars,
                    start,
                    &mut caps,
                    &b,
                    0,
                    &mut |p, _caps| {
                        end = Some(p);
                        true
                    },
                ) {
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
    budget: &Budget,
    depth: u32,
    k: &mut dyn FnMut(usize, &mut Caps) -> bool,
) -> bool {
    // 预算/深度闸门。超限 ⇒ 当作**不匹配**（调用方看到的是"没匹配上"），
    // 而不是 panic、也不是继续跑到三秒。见模块文档的复杂度一节。
    if !budget.enter(depth) {
        return false;
    }
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
            let ok = match_node(
                inner,
                text,
                pos,
                caps,
                budget,
                depth + 1,
                &mut |end, caps| {
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
                },
            );
            if !ok {
                if let Some(slot) = caps.get_mut(*idx) {
                    *slot = saved;
                }
            }
            ok
        }
        Node::Concat(items) => match_seq(items, text, pos, caps, budget, depth, k),
        Node::Alt(branches) => {
            for b in branches {
                let saved = caps.clone();
                if match_node(b, text, pos, caps, budget, depth + 1, k) {
                    return true;
                }
                *caps = saved;
            }
            false
        }
        Node::Repeat { node, min, max } => {
            match_repeat(node, text, pos, caps, budget, depth, *min, *max, 0, k)
        }
    }
}

fn match_seq(
    items: &[Node],
    text: &[char],
    pos: usize,
    caps: &mut Caps,
    budget: &Budget,
    depth: u32,
    k: &mut dyn FnMut(usize, &mut Caps) -> bool,
) -> bool {
    match items.split_first() {
        None => k(pos, caps),
        Some((head, rest)) => match_node(head, text, pos, caps, budget, depth, &mut |p, caps| {
            match_seq(rest, text, p, caps, budget, depth, &mut *k)
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn match_repeat(
    node: &Node,
    text: &[char],
    pos: usize,
    caps: &mut Caps,
    budget: &Budget,
    depth: u32,
    min: u32,
    max: Option<u32>,
    done: u32,
    k: &mut dyn FnMut(usize, &mut Caps) -> bool,
) -> bool {
    // 贪婪：**先试"再多匹配一次"**，失败了才退回"到此为止"。
    let can_more = max.is_none_or(|m| done < m);
    if can_more {
        let saved = caps.clone();
        let advanced = match_node(node, text, pos, caps, budget, depth + 1, &mut |p, caps| {
            if p == pos {
                // 零宽重复：不再递归，避免死循环。
                return false;
            }
            // **深度必须随迭代增长**：否则 `a+` 的迭代深度永远停在 0，
            // 深度上限形同虚设，一条长输入就能把栈打穿（实测过）。
            match_repeat(
                node,
                text,
                p,
                caps,
                budget,
                depth + 1,
                min,
                max,
                done + 1,
                &mut *k,
            )
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

/// 回溯预算：步数与递归深度。
///
/// 步数在**每一次 `match_node` 进入**时扣一。这样"最坏耗时"就与模式、
/// 输入长度都无关——超限只是"这次没匹配上"，绝不会卡住按键路径。
/// 调用方在公开入口处各建一个，因此预算是**每次公开调用**的总量，
/// 不是每次 `match_node` 调用各自的。
struct Budget {
    steps: std::cell::Cell<u64>,
    exhausted: std::cell::Cell<bool>,
    max_depth: u32,
}

impl Budget {
    fn new(steps: u64) -> Self {
        Self {
            steps: std::cell::Cell::new(steps),
            exhausted: std::cell::Cell::new(false),
            max_depth: MAX_DEPTH,
        }
    }

    /// 还能继续吗？扣一步并检查深度。
    ///
    /// 用 `Cell` 做内部可变性，是为了让递归调用点**共享**一个预算
    /// （`&Budget`），而不是把 `&mut Budget` 借进闭包——那会让
    /// "续延 + 预算"的组合借不出来（实测的编译错误）。
    fn enter(&self, depth: u32) -> bool {
        if self.steps.get() == 0 || depth > self.max_depth {
            self.exhausted.set(true);
            return false;
        }
        self.steps.set(self.steps.get() - 1);
        true
    }
}

/// 模式里是否存在"重复套重复"（例如 `(a+)+`、`(ab*)*`、`(a?){2,}`）。
///
/// # 为什么是"保守拒绝"而不是精确判定
///
/// 精确判定"哪个模式会指数回溯"需要分析字符集重叠与歧义性，
/// 而那本身是一件容易判错的事（判错的方向是**放过**，代价是卡顿）。
/// 真实方案的拼写规则里没有任何嵌套量词（只有 `^([a-z]{2}).+$`
/// 这种平铺写法），因此"一律拒绝"的代价接近零，收益是**可证明**的：
/// 没有嵌套重复就没有经典的指数回溯路径。
///
/// 这仍然是**缓解**——最终替换是自动机实现（见模块文档）。
fn contains_nested_repeat(node: &Node, inside_repeat: bool) -> bool {
    match node {
        Node::Empty | Node::Char(_) | Node::Any | Node::Class { .. } | Node::Start | Node::End => {
            false
        }
        Node::Group(inner, _) => contains_nested_repeat(inner, inside_repeat),
        Node::Concat(items) | Node::Alt(items) => items
            .iter()
            .any(|n| contains_nested_repeat(n, inside_repeat)),
        Node::Repeat { node, .. } => {
            if inside_repeat {
                return true;
            }
            contains_nested_repeat(node, true)
        }
    }
}

/// 全部匹配的公共字面前缀（从语法树上取，见 `Regex::required_prefix`）。
fn required_prefix_of(node: &Node) -> String {
    let Node::Concat(items) = node else {
        // 单个 `Char`（`a`）或任何别的形状：只有 Char 有必需前缀。
        return match node {
            Node::Char(c) => c.to_string(),
            _ => String::new(),
        };
    };
    let mut out = String::new();
    for (i, it) in items.iter().enumerate() {
        match it {
            // 开头的 `^` 不消费字符，跳过它继续取字面量。
            Node::Start if i == 0 => {}
            Node::Char(c) => out.push(*c),
            // 其余一律停：分组、量词、字符类、选择、`$`……
            _ => break,
        }
    }
    out
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
