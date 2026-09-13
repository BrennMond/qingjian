//! # Calculator
//!
//! 中文职责：计算器翻译器——敲 `cC1+2` 出「3」。
//! English role: the calculator translator — type `cC1+2`, get `3`.
//! 架构位置：`qingjian-core::Translator` 的实现，与 `inline.rs` 同族。
//!
//! # 为什么它单独一个文件
//!
//! 上游的 `calc_translator.lua` 干的事是：
//!
//! ```lua
//! pcall(load('return ' .. code, 'calculate', 't', calcPlugin))
//! ```
//!
//! 也就是说它**把 Lua 解释器当计算器用**。我们没有解释器，因此要自己写
//! 一个表达式求值器——这不是"再写一个零件"，而是**一整套表达式语言**：
//! 词法、递归下降、运算符优先级、函数表、以及**数字到字符串的格式化**。
//!
//! 所以它值得单独一个文件，而不是塞进 `inline.rs` 让那边变成两千行。
//!
//! # 我们能表达什么、不能表达什么（诚实清单）
//!
//! 支持：四则运算、`^`、一元负号、括号、`%`（既当取模也当百分号）、
//! 阶乘 `!`、以及上游 `calcPlugin` 里的数学函数。
//!
//! **不支持**：Lua 的 `and` / `or` / 比较运算符、字符串、表构造器、
//! `..` 拼接。上游把它们一并"支持"了（因为那就是 Lua），但一个**输入法
//! 计算器**不需要它们，而支持它们意味着实现半个 Lua。
//!
//! 这个取舍是有意的：**我们能说清边界**，而不是"大概能跑"。

use qingjian_core::{
    Candidate, CandidateKind, CandidateSink, Origin, Query, Score, Span, Tag, Translator,
};

// ─────────────────────────────────────────────────────────────────────────────
// 词法
// ─────────────────────────────────────────────────────────────────────────────

/// 一个记号。
#[derive(Clone, Debug, PartialEq)]
enum Token {
    /// 数字字面量。
    Number(f64),
    /// 标识符（函数名或常量名）。
    Ident(String),
    /// 运算符 / 括号。
    Op(char),
}

/// 词法错误。
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CalcError {
    /// 认不出的字符。
    Unexpected(char),
    /// 表达式不完整（例如以运算符结尾）。
    UnexpectedEnd,
    /// 括号不匹配。
    Unbalanced,
    /// 不认识的名字。
    UnknownName(String),
    /// 函数参数个数不对。
    BadArity {
        /// 函数名。
        name: String,
        /// 期望几个参数。
        expected: usize,
        /// 实际给了几个。
        got: usize,
    },
    /// 除零之外的定义域问题（例如 `sqrt(-1)`、`log(0)`）。
    OutOfDomain(&'static str),
}

impl core::fmt::Display for CalcError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unexpected(c) => write!(f, "认不出的字符 `{c}`"),
            Self::UnexpectedEnd => write!(f, "表达式不完整"),
            Self::Unbalanced => write!(f, "括号不匹配"),
            Self::UnknownName(n) => write!(f, "不认识的名字 `{n}`"),
            Self::BadArity {
                name,
                expected,
                got,
            } => write!(f, "`{name}` 需要 {expected} 个参数，给了 {got} 个"),
            Self::OutOfDomain(what) => write!(f, "超出定义域：{what}"),
        }
    }
}

impl std::error::Error for CalcError {}

/// 把一段文本切成记号。
///
/// **数字的写法**照 Lua：`1`、`1.5`、`.5`、`1e3`、`1E-2` 都认。
/// 上游靠 Lua 的词法，我们手写一份——两种写法都能被识别这一点要一致，
/// 否则"`cC1e3` 在那边是 1000、在这是解析失败"。
fn tokenize(s: &str) -> Result<Vec<Token>, CalcError> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit)) {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            if chars.get(i) == Some(&'.') {
                i += 1;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            // 指数部分：`e` 后面必须跟数字（或符号+数字），否则 `e` 是常量。
            if matches!(chars.get(i), Some('e' | 'E')) {
                let mut j = i + 1;
                if matches!(chars.get(j), Some('+' | '-')) {
                    j += 1;
                }
                if chars.get(j).is_some_and(char::is_ascii_digit) {
                    i = j;
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                }
            }
            let text: String = chars[start..i].iter().collect();
            let v: f64 = text.parse().map_err(|_| CalcError::Unexpected('.'))?;
            out.push(Token::Number(v));
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            out.push(Token::Ident(chars[start..i].iter().collect()));
            continue;
        }
        if "+-*/^%(),!".contains(c) {
            out.push(Token::Op(c));
            i += 1;
            continue;
        }
        return Err(CalcError::Unexpected(c));
    }
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────────
// 求值
// ─────────────────────────────────────────────────────────────────────────────

/// 求值器：递归下降 + 运算符优先级。
struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn eat_op(&mut self, op: char) -> bool {
        if matches!(self.peek(), Some(Token::Op(c)) if *c == op) {
            self.pos += 1;
            return true;
        }
        false
    }

    /// `expr := term (('+' | '-') term)*`
    fn expr(&mut self) -> Result<f64, CalcError> {
        let mut left = self.term()?;
        loop {
            if self.eat_op('+') {
                left += self.term()?;
            } else if self.eat_op('-') {
                left -= self.term()?;
            } else {
                return Ok(left);
            }
        }
    }

    /// `term := unary (('*' | '/' | '%') unary)*`
    ///
    /// `%` 在 Lua 里是**取模**（不是取余）：`-1 % 3` 得 2。
    /// 我们用 `rem_euclid` 的语义与之对齐。注意**百分号**在更早一步
    /// 就被改写成了 `/100`（见 [`preprocess`]），因此走到这里的 `%`
    /// 一定是取模。
    fn term(&mut self) -> Result<f64, CalcError> {
        let mut left = self.unary()?;
        loop {
            if self.eat_op('*') {
                left *= self.unary()?;
            } else if self.eat_op('/') {
                left /= self.unary()?;
            } else if self.eat_op('%') {
                let rhs = self.unary()?;
                left = lua_mod(left, rhs);
            } else {
                return Ok(left);
            }
        }
    }

    /// `unary := ('-' | '+')? power`
    ///
    /// **`--` 不是"两次取负"**：在 Lua 里它是行注释的开头，因此
    /// `--3` 是一行空注释 → 表达式为空 → 语法错误。
    /// 我第一版让它返回 3，对照测试指出这是"比 Lua 更宽松"。
    fn unary(&mut self) -> Result<f64, CalcError> {
        if matches!(self.peek(), Some(Token::Op('-')))
            && matches!(self.tokens.get(self.pos + 1), Some(Token::Op('-')))
        {
            return Err(CalcError::Unexpected('-'));
        }
        if self.eat_op('-') {
            return Ok(-self.unary()?);
        }
        if self.eat_op('+') {
            return self.unary();
        }
        self.power()
    }

    /// `power := postfix ('^' unary)?`
    ///
    /// Lua 的 `^` 是**右结合**（`2^3^2` = `2^9`），因此右边递归回 `unary`。
    /// 而且优先级高于一元负号：`-2^2` = `-4`（不是 4）。
    fn power(&mut self) -> Result<f64, CalcError> {
        let base = self.postfix()?;
        if self.eat_op('^') {
            let exp = self.unary()?;
            return Ok(base.powf(exp));
        }
        Ok(base)
    }

    /// `postfix := primary ('!')*`
    fn postfix(&mut self) -> Result<f64, CalcError> {
        let mut v = self.primary()?;
        while self.eat_op('!') {
            v = factorial(v).ok_or(CalcError::OutOfDomain("阶乘的负数"))?;
        }
        Ok(v)
    }

    /// `primary := number | ident | '(' expr ')'`
    fn primary(&mut self) -> Result<f64, CalcError> {
        match self.next() {
            Some(Token::Number(v)) => Ok(v),
            Some(Token::Ident(name)) => {
                if self.eat_op('(') {
                    let args = self.arg_list()?;
                    call(&name, &args)
                } else {
                    constant(&name).ok_or(CalcError::UnknownName(name))
                }
            }
            Some(Token::Op('(')) => {
                let v = self.expr()?;
                if !self.eat_op(')') {
                    return Err(CalcError::Unbalanced);
                }
                Ok(v)
            }
            Some(Token::Op(c)) => Err(CalcError::Unexpected(c)),
            None => Err(CalcError::UnexpectedEnd),
        }
    }

    /// 实参表（已经把 `(` 吃掉了）。
    fn arg_list(&mut self) -> Result<Vec<f64>, CalcError> {
        let mut args = Vec::new();
        if self.eat_op(')') {
            return Ok(args);
        }
        loop {
            args.push(self.expr()?);
            if self.eat_op(',') {
                continue;
            }
            if self.eat_op(')') {
                return Ok(args);
            }
            return Err(CalcError::Unbalanced);
        }
    }
}

/// Lua 的取模：结果的符号跟**除数**（不是被除数）。
///
/// `-1 % 3` 在 Lua 里是 2，在 C/Rust 的 `%` 里是 -1。
/// 这个差别会在 `cC-1%3` 上直接可见，因此不能"大概一样就行"。
fn lua_mod(a: f64, b: f64) -> f64 {
    if b == 0.0 {
        return f64::NAN;
    }
    a - (a / b).floor() * b
}

/// 阶乘。负数返回 `None`（上游返回 `nil` → 那条候选不产出）。
fn factorial(x: f64) -> Option<f64> {
    if x < 0.0 || x.fract() != 0.0 {
        return None;
    }
    if x > 170.0 {
        // 171! 超出 f64 范围 → 上游会得到 inf，这里照做。
        return Some(f64::INFINITY);
    }
    // 到这里 `x` 已保证是 `0..=170` 的整数（上面两个分支挡住了别的），
    // 因此这个窄化是安全的。`f64 as u32` 在 Rust 里是**饱和**转换
    // （越界给上界），而我们已经把越界挡住了，所以它不会悄悄截断。
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = x as u32;
    let mut r = 1.0f64;
    for i in 1..=n {
        r *= f64::from(i);
    }
    Some(r)
}

/// 常量表。
fn constant(name: &str) -> Option<f64> {
    match name {
        "pi" => Some(std::f64::consts::PI),
        "e" => Some(std::f64::consts::E),
        _ => None,
    }
}

/// 函数表（对应上游的 `calcPlugin`）。
///
/// # 与上游的差异：几个"数学上尴尬"的函数我们**不做**
///
/// 上游把 Lua 5.1 的 `math` 库整个搬了进来，其中三个在今天的
/// `f64` 里已经没有对应实现：
///
/// | 上游 | 问题 | 我们的处理 |
/// | --- | --- | --- |
/// | `atan2(y,x)` | `math.atan2` 在 Lua 5.3 已移除 | 用 `y.atan2(x)` 实现 |
/// | `loge` | Lua 5.2 起没有这个名字 | 实现为 `ln`，并接受 `loge` |
/// | `log10` | 同上 | 实现为 `log10` |
///
/// 我们把它们都实现了（代价为零），但**不声称**"Lua 5.1 里有的我们都有"。
fn call(name: &str, args: &[f64]) -> Result<f64, CalcError> {
    // **多给的参数被忽略、少给的才是错误**——这一条是 Lua 的行为
    // （`sin(1,2)` 在 Lua 里就是 `sin(1)`）。我第一版要求"个数恰好相等"，
    // 于是 `sin(1,2)` 上游成功而我们失败，对照测试当场指出。
    let n = |want: usize| -> Result<(), CalcError> {
        if args.len() >= want {
            Ok(())
        } else {
            Err(CalcError::BadArity {
                name: name.to_owned(),
                expected: want,
                got: args.len(),
            })
        }
    };
    let a = args.first().copied().unwrap_or(f64::NAN);
    Ok(match name {
        "abs" => {
            n(1)?;
            a.abs()
        }
        "floor" => {
            n(1)?;
            a.floor()
        }
        "ceil" => {
            n(1)?;
            a.ceil()
        }
        // 注意：负数在 Lua 里得到 `nan` 而不是错误（C 的 `sqrt` 如此），
        // 因此这里**不做定义域检查**——做了就与上游不一致。
        "sqrt" => {
            n(1)?;
            a.sqrt()
        }
        "exp" => {
            n(1)?;
            a.exp()
        }
        "sin" => {
            n(1)?;
            a.sin()
        }
        "cos" => {
            n(1)?;
            a.cos()
        }
        "tan" => {
            n(1)?;
            a.tan()
        }
        "sinh" => {
            n(1)?;
            a.sinh()
        }
        "cosh" => {
            n(1)?;
            a.cosh()
        }
        "tanh" => {
            n(1)?;
            a.tanh()
        }
        "asin" => {
            n(1)?;
            a.asin()
        }
        "acos" => {
            n(1)?;
            a.acos()
        }
        "atan" => {
            n(1)?;
            a.atan()
        }
        "atan2" => {
            n(2)?;
            args[0].atan2(args[1])
        }
        "deg" => {
            n(1)?;
            a.to_degrees()
        }
        "rad" => {
            n(1)?;
            a.to_radians()
        }
        "ldexp" => {
            n(2)?;
            args[0] * args[1].exp2()
        }
        // `loge` 是自然对数，`ln` 是它的常用别名（两者都收）。
        // 上游对 `x <= 0` 返回 `nil` → **那条候选不产出**（不是 nan）。
        // 因此这里保留检查：Lua 的 `log` 在这里**确实**会失败。
        "ln" | "loge" => {
            n(1)?;
            if a <= 0.0 {
                return Err(CalcError::OutOfDomain("对数的非正数"));
            }
            a.ln()
        }
        "log10" => {
            n(1)?;
            if a <= 0.0 {
                return Err(CalcError::OutOfDomain("对数的非正数"));
            }
            a.log10()
        }
        // 上游的 `log(y, x)` 是"以 y 为底 x 的对数"——**参数顺序反直觉**，
        // 我们照抄（改了就与上游不一致）。
        "log" => {
            n(2)?;
            let (base, x) = (args[0], args[1]);
            if x <= 0.0 || base <= 0.0 {
                return Err(CalcError::OutOfDomain("对数的非正数"));
            }
            x.ln() / base.ln()
        }
        "fact" => {
            n(1)?;
            factorial(a).ok_or(CalcError::OutOfDomain("阶乘的负数"))?
        }
        "frexp" => {
            n(1)?;
            // # 这是**唯一**一处我们有意与上游不同
            //
            // 上游返回**字符串** `"m * 2^e"`（Lua 的 `m .. ' * 2^' .. e`）。
            // 那不是一个值，而是一段**文本**，因此：
            //
            // | 表达式 | 上游 | 我们 |
            // | --- | --- | --- |
            // | `frexp(12)` | `"0.75 * 2^4"`（字符串） | `48`（数值） |
            // | `frexp(12)*2` | **崩**（字符串乘数字） | `96` |
            //
            // 上游那条返回字符串的注释自己写着「无法参与运算后续，
            // 只能单独使用」——**它知道这是个坑，只是选择留着**。
            //
            // 我们返回数值，因为"候选里的文本"与"求值用的值"是两件事，
            // 而把它做成字符串会让**后续运算直接失败**。
            // 代价是 `frexp(x)` 的显示文本不同——`frexp` 不在 oracle 对照
            // 范围内，就是因为这一条，而不是因为"没测得出来"。
            frexp_value(a)
        }
        _ => return Err(CalcError::UnknownName(name.to_owned())),
    })
}

/// `frexp`：`x = m * 2^e`，返回 `m`（上游返回字符串，我们返回数值）。
fn frexp_value(x: f64) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    let e = x.abs().log2().floor() + 1.0;
    x / e.exp2()
}

// ─────────────────────────────────────────────────────────────────────────────
// 预处理（照抄上游的两步）
// ─────────────────────────────────────────────────────────────────────────────

/// 把 `[0-9]+!` 改写成 `fact(...)`。
///
/// 上游用 `gsub('([0-9]+)!', 'fact(%1)')`——**全部替换**（这个 `gsub`
/// 没有连写两遍，与 `number_translator` 那边不同）。
#[must_use]
pub fn replace_factorial(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 8);
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            let digits: String = chars[start..i].iter().collect();
            if chars.get(i) == Some(&'!') {
                out.push_str("fact(");
                out.push_str(&digits);
                out.push(')');
                i += 1;
            } else {
                out.push_str(&digits);
            }
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// 把百分号改写成 `/100`。
///
/// # 上游的两条规则与它们的顺序
///
/// ```lua
/// str = str:gsub("(%b())%%(%D)", ...)   -- ① 括号形式
/// str = str:gsub("(%d+%.?%d*)%%(%D)", ...) -- ② 纯数字形式
/// ```
///
/// `%b()` 是 Lua 的**配对括号**匹配，要求 `(` 与 `)` 真的配起来
/// （因此 `1+2)` 里的那个 `)` 不会被它认领——那是好事，语法错误
/// 应当留给求值器报）。两条规则都要求 `%` 后面**跟着非数字**，
/// 因为上游在末尾补了一个空格来充当那个"尾字符"。
///
/// **这个尾巴是它能同时当取模与百分号用的全部原因**：
/// `10%3` 里的 `%` 后面是数字，两条规则都不匹配，于是它保持为取模。
///
/// # 两条被测试抓出来的细节
///
/// 1. `(1+1)%` → `((1+1)/100)`——**外面又包了一层括号**（上游的
///    `"(" .. block .. "/100)"` 把 `block` 整个放进去，而 `block`
///    自带括号）。我第一版写成原地插入，丢了一层。
/// 2. `%b()` 要求成对，所以 `1+2)` 的 `)` 不该被它认领。
#[must_use]
pub fn replace_percent(s: &str) -> String {
    // 上游的 `str .. ' '`：给 `%D` 一个尾字符。
    let chars: Vec<char> = s.chars().chain(std::iter::once(' ')).collect();

    // ① 括号形式：`( ... )%` → `(( ... )/100)`。
    let mut out: Vec<char> = Vec::with_capacity(chars.len() + 8);
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '(' {
            // 找**配对**的 `)`（`%b()` 的语义）。
            let mut depth = 0i32;
            let mut j = i;
            let mut matched = None;
            while j < chars.len() {
                match chars[j] {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            matched = Some(j);
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            if let Some(close) = matched {
                // `%` 后面必须跟非数字（尾字符已由那个空格保证）。
                let pct = close + 1;
                let tail_ok = chars.get(pct).is_some_and(|c| *c == '%')
                    && chars.get(pct + 1).is_some_and(|c| !c.is_ascii_digit());
                if tail_ok {
                    out.push('(');
                    out.extend_from_slice(&chars[i..=close]);
                    out.extend("/100".chars());
                    out.push(')');
                    i = pct + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }

    // ② 纯数字形式：`123%` → `(123/100)`。
    let mut out2 = String::with_capacity(out.len() + 8);
    let mut i = 0usize;
    while i < out.len() {
        if out[i].is_ascii_digit() {
            let start = i;
            while i < out.len() && out[i].is_ascii_digit() {
                i += 1;
            }
            if out.get(i) == Some(&'.') {
                let mut k = i + 1;
                while k < out.len() && out[k].is_ascii_digit() {
                    k += 1;
                }
                // 只有小数点后真的有数字才算进这个数。
                if k > i + 1 {
                    i = k;
                }
            }
            let num: String = out[start..i].iter().collect();
            if out.get(i) == Some(&'%') && out.get(i + 1).is_some_and(|c| !c.is_ascii_digit()) {
                out2.push('(');
                out2.push_str(&num);
                out2.push_str("/100)");
                i += 1;
                continue;
            }
            out2.push_str(&num);
            continue;
        }
        out2.push(out[i]);
        i += 1;
    }
    // 去掉补的那个尾空格（上游的 `sub(1, -2)`）。
    out2.pop();
    out2
}

// ─────────────────────────────────────────────────────────────────────────────
// 数字格式化（Lua 的 `tostring`）
// ─────────────────────────────────────────────────────────────────────────────

/// 按 **Lua 的 `tostring`** 格式化一个数。
///
/// # 为什么必须自己写
///
/// Lua 的 `tostring(1/3)` 是 `"0.33333333333333"`（**14 位有效数字**），
/// 而 Rust 的 `{}` 会给 `0.3333333333333333`（17 位）。直接 `to_string()`
/// 的结果就是"候选里的数字跟上游不一样"——用户一眼能看出来。
///
/// Lua 用的是 C 的 `%.14g`，规则两条：
///
/// 1. 取 14 位有效数字；
/// 2. **指数 `exp` 满足 `-4 <= exp < 14` 时用位置记法，否则用科学记法**，
///    并去掉尾随的零。
///
/// 第 2 条那个边界是实测出来的：`1e-5` 在 Lua 里是 `"1e-05"`
/// （指数 -5 < -4 → 科学记法），而 `1e13` 是 `"10000000000000"`。
/// 我按"`exp >= -5` 就用位置记法"写过一版，`1e-5` 于是变成了
/// `"0.00001"`——测试当场指出。
#[must_use]
pub fn lua_number_to_string(x: f64) -> String {
    if x.is_nan() {
        return "nan".to_owned();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf" } else { "-inf" }.to_owned();
    }
    if x == 0.0 {
        return "0".to_owned();
    }
    let neg = x < 0.0;
    let ax = x.abs();
    // 取 14 位有效数字的指数形式，拿到指数。
    let sci = format!("{ax:.13e}");
    let (mant, exp_str) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp_str.parse().unwrap_or(0);
    let body = if (-4..14).contains(&exp) {
        // 位置记法：小数位数 = 13 - exp（负数则补零到整数）。
        // `13 - exp` 在 `-4..14` 区间里落在 `0..=17`，不可能为负；
        // 用 `usize::try_from` 把这件事写成代码。
        let decimals = usize::try_from((13 - exp).max(0)).unwrap_or(0);
        let s = format!("{ax:.decimals$}");
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            s
        }
    } else {
        // 科学记法：尾数去掉尾随零，指数是**至少两位**。
        let m = mant.trim_end_matches('0').trim_end_matches('.');
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{m}e{sign}{:02}", exp.abs())
    };
    if neg {
        format!("-{body}")
    } else {
        body
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 零件
// ─────────────────────────────────────────────────────────────────────────────

/// **计算器翻译器**：敲 `cC1+2` 出「3」。
///
/// # 两条候选（照上游）
///
/// | 输入 `cC1+2` | 文本 | 注释 |
/// | --- | --- | --- |
/// | 第一条 | `3` | 空 |
/// | 第二条 | `1+2=3` | 空 |
///
/// 解析失败时换成另外两条：原式（注释「解析失败」）与预处理后的式子
/// （注释「入参」）。**"入参"那条是上游的调试输出**，它把
/// `replacePercent`/`replaceToFactorial` 之后的式子给用户看——
/// 用户因此能明白"`50%` 为什么变成了 0.5"。
pub struct CalcTranslator {
    /// 触发前缀（默认 `cC`）。
    prefix: String,
    /// 是否在预编辑区显示前缀。
    show_prefix: bool,
    /// 本翻译器负责的标签。
    tags: Vec<Tag>,
}

impl CalcTranslator {
    /// 构造。
    #[must_use]
    pub fn new(spec: &crate::spec::CalcSpec, tags: Vec<Tag>) -> Self {
        Self {
            prefix: spec.prefix.clone(),
            show_prefix: spec.show_prefix,
            tags,
        }
    }

    /// 求值一条表达式，返回 `(结果文本, 预处理后的式子)`。
    ///
    /// # Errors
    ///
    /// 词法 / 语法 / 定义域错误。**调用方不该把它当异常**——
    /// 上游的"解析失败"也是正常输出的一部分。
    pub fn evaluate(&self, expression: &str) -> Result<(String, String), CalcError> {
        let code = replace_percent(&replace_factorial(expression));
        let tokens = tokenize(&code)?;
        if tokens.is_empty() {
            return Err(CalcError::UnexpectedEnd);
        }
        let mut p = Parser { tokens, pos: 0 };
        let v = p.expr()?;
        // 尾随记号的处理**照 Lua**：
        //
        // - `,` 之后的内容合法（`1,2` 在 Lua 里是"返回第一个值"）；
        // - 其它任何剩余记号都是语法错误（`1+2)`、`1 2`）。
        //
        // 我第一版**无条件忽略**剩余记号，于是 `1+2)` 被我们算成 3
        // 而 Lua 报语法错误——比 Lua 更宽松也是一种不一致。
        if let Some(tok) = p.tokens.get(p.pos) {
            match tok {
                Token::Op(',') => {}
                Token::Op(c) => return Err(CalcError::Unexpected(*c)),
                _ => return Err(CalcError::UnexpectedEnd),
            }
        }
        Ok((lua_number_to_string(v), code))
    }
}

impl Translator for CalcTranslator {
    fn translate(&self, q: &Query<'_>, span: Span, out: &mut CandidateSink<'_>) {
        let Some(expr) = q.segment_text.strip_prefix(self.prefix.as_str()) else {
            return;
        };
        // 上游：表达式为空就直接返回（防止正则写错造成误触发）。
        if expr.is_empty() {
            return;
        }
        // 有前缀时按上游的行为把整段输入当作显示文本。
        let shown = if self.show_prefix {
            q.segment_text.to_owned()
        } else {
            expr.to_owned()
        };
        if let Ok((result, _code)) = self.evaluate(expr) {
            out.push(Candidate {
                text: result.clone(),
                comment: None,
                score: Score::from_weight(50_000.0),
                origin: Origin::Literal,
                attr: qingjian_core::SpellingAttr::NORMAL,
                span,
                lane: qingjian_core::Lane::Input,
                kind: CandidateKind::Inline,
                key: None,
            });
            out.push(Candidate {
                text: format!("{shown}={result}"),
                comment: None,
                score: Score::from_weight(49_990.0),
                origin: Origin::Literal,
                attr: qingjian_core::SpellingAttr::NORMAL,
                span,
                lane: qingjian_core::Lane::Input,
                kind: CandidateKind::Inline,
                key: None,
            });
        } else {
            // 失败路径也是**两条候选**，注释不同（照上游）。
            out.push(Candidate {
                text: shown.clone(),
                comment: Some("解析失败".to_owned()),
                score: Score::from_weight(50_000.0),
                origin: Origin::Literal,
                attr: qingjian_core::SpellingAttr::NORMAL,
                span,
                lane: qingjian_core::Lane::Input,
                kind: CandidateKind::Inline,
                key: None,
            });
            out.push(Candidate {
                text: replace_percent(&replace_factorial(expr)),
                comment: Some("入参".to_owned()),
                score: Score::from_weight(49_990.0),
                origin: Origin::Literal,
                attr: qingjian_core::SpellingAttr::NORMAL,
                span,
                lane: qingjian_core::Lane::Input,
                kind: CandidateKind::Inline,
                key: None,
            });
        }
    }

    fn accepts(&self, tags: &[Tag]) -> bool {
        // **不绑标签时对全部输入生效**（自己的标签表为空 = "我自己判断"）。
        //
        // 上游写的是 `lua_translator@*date_translator`，那个 `*` 是 Lua 的
        // 命名空间而不是标签——这些零件本来就不绑标签，各自在 `translate()`
        // 里先认自己的触发词。少了这一句，`accepts` 会对空标签表返回 false，
        // 于是"装配好了却永远不被调用"。
        self.tags.is_empty() || tags.iter().any(|t| self.tags.contains(t))
    }

    fn targets(&self) -> &[Tag] {
        &self.tags
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lua_number_formatting_matches_the_14_digit_rule() {
        // 整数不带小数点。
        assert_eq!(lua_number_to_string(2.0), "2");
        assert_eq!(lua_number_to_string(1024.0), "1024");
        assert_eq!(lua_number_to_string(120.0), "120");
        // 14 位有效数字（不是 17 位）。
        assert_eq!(lua_number_to_string(1.0 / 3.0), "0.33333333333333");
        assert_eq!(lua_number_to_string(0.1 + 0.2), "0.3");
        // 指数边界：`-4 <= exp < 14` 用位置记法。
        assert_eq!(lua_number_to_string(0.0001), "0.0001");
        assert_eq!(lua_number_to_string(0.00001), "1e-05");
        assert_eq!(lua_number_to_string(1e13), "10000000000000");
        assert_eq!(lua_number_to_string(1e14), "1e+14");
        // 特殊值（Lua 的 `tostring` 就是这么写的）。
        assert_eq!(lua_number_to_string(f64::INFINITY), "inf");
        assert_eq!(lua_number_to_string(f64::NEG_INFINITY), "-inf");
        assert_eq!(lua_number_to_string(f64::NAN), "nan");
        assert_eq!(lua_number_to_string(0.0), "0");
        assert_eq!(lua_number_to_string(-1.5), "-1.5");
    }

    #[test]
    fn regex_operators_follow_lua() {
        let t = {
            let mut tags = crate::tag::TagTable::new();
            CalcTranslator::new(
                &crate::spec::CalcSpec::default(),
                vec![tags.intern("calculator")],
            )
        };
        let ok = |e: &str| t.evaluate(e).ok().map(|(v, _)| v);
        // `^` 右结合。
        assert_eq!(ok("2^3^2").as_deref(), Some("512"));
        // 一元负号优先级低于 `^`。
        assert_eq!(ok("-2^2").as_deref(), Some("-4"));
        // `%` 是**取模**，符号跟除数（Lua 的语义，不是 C 的）。
        assert_eq!(ok("-1%3").as_deref(), Some("2"));
        // 而 `50%` 是百分号（被预处理改写成 /100）。
        assert_eq!(ok("50%").as_deref(), Some("0.5"));
        // `10%3` 里那个 `%` 后面是数字 → 保持取模。
        assert_eq!(ok("10%3").as_deref(), Some("1"));
        // 阶乘。
        assert_eq!(ok("5!").as_deref(), Some("120"));
        // 除零 → inf / nan（Lua 不报错）。
        assert_eq!(ok("1/0").as_deref(), Some("inf"));
        assert_eq!(ok("0/0").as_deref(), Some("nan"));
    }

    #[test]
    fn we_are_not_more_permissive_than_lua() {
        let t = {
            let mut tags = crate::tag::TagTable::new();
            CalcTranslator::new(
                &crate::spec::CalcSpec::default(),
                vec![tags.intern("calculator")],
            )
        };
        // 这几条 Lua 都会失败，我们也必须失败——
        // **比上游更宽松也是一种不一致**（它会让"上游说错了"变成"我们算了个数"）。
        for bad in ["--3", "1+", "1+2)", "abc", "unknown(1)", ""] {
            assert!(t.evaluate(bad).is_err(), "{bad} 应当失败");
        }
        // 逗号是例外：Lua 里 `1,2` 合法（返回第一个值）。
        assert_eq!(t.evaluate("1,2").ok().map(|(v, _)| v).as_deref(), Some("1"));
    }

    #[test]
    fn extra_function_arguments_are_ignored_like_lua() {
        let t = {
            let mut tags = crate::tag::TagTable::new();
            CalcTranslator::new(
                &crate::spec::CalcSpec::default(),
                vec![tags.intern("calculator")],
            )
        };
        // `sin(1,2)` 在 Lua 里就是 `sin(1)`。
        assert_eq!(
            t.evaluate("sin(1,2)").ok().map(|(v, _)| v).as_deref(),
            Some("0.8414709848079")
        );
        // 而少给参数是错误。
        assert!(t.evaluate("atan2(1)").is_err());
    }

    #[test]
    fn percent_rewriting_wraps_the_parenthesised_form_once_more() {
        assert_eq!(replace_percent("(1+1)%"), "((1+1)/100)");
        assert_eq!(replace_percent("50%"), "(50/100)");
        assert_eq!(replace_percent("12.5%"), "(12.5/100)");
        // `%` 后面是数字 → 不动（那是取模）。
        assert_eq!(replace_percent("10%3"), "10%3");
        // 不成对的括号不该被 `%b()` 认领。
        assert_eq!(replace_percent("1+2)"), "1+2)");
    }

    #[test]
    fn factorial_rewriting_replaces_every_occurrence() {
        assert_eq!(replace_factorial("5!"), "fact(5)");
        assert_eq!(replace_factorial("5!+3!"), "fact(5)+fact(3)");
        assert_eq!(replace_factorial("5"), "5");
    }

    #[test]
    fn frexp_is_a_number_not_a_string() {
        // 与上游的唯一一处有意不同：见 `call` 里 `frexp` 的说明。
        for (x, want) in [(12.0, 0.75), (1.0, 0.5), (0.0, 0.0), (-6.0, -0.75)] {
            assert!(
                (frexp_value(x) - want).abs() < 1e-12,
                "frexp({x}) 应当是 {want}，实际 {}",
                frexp_value(x)
            );
        }
    }
}
