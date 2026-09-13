//! # Inline translators and filters（引擎直接生成候选的零件）
//!
//! 中文职责：那些**候选文本不在任何词库里**、由代码算出来的零件——
//! 日期、UUID、Unicode 字符，以及几个按类型调整顺序的滤镜。
//! English role: components whose candidate text is *computed*, not looked up —
//! dates, UUIDs, Unicode codepoints — plus type-aware ordering filters.
//! 架构位置：`stele-core::Translator` / `Filter` 的实现。
//!
//! # 这一族零件从哪来
//!
//! 在 RIME 里它们**全部住在 Lua 插件里**（`lua_translator@*date_translator`、
//! `lua_filter@*long_word_filter`…）。这是 RIME 生态里最有意思的一处：
//! 核心引擎没有这些能力，用户自己用脚本补出来，而这些脚本恰好是
//! **品牌输入法体验最接近的那些功能**。
//!
//! 我们把它们按**行为**重做成原生零件。有一条边界必须说清：
//!
//! > **我们照"用户看到什么"重写，不照"代码怎么写"复制。**
//!
//! 理由不只是许可证（虽然那也是一条）：这些脚本的实现细节带着
//! Lua 的痕迹（`yield` 流式产出、`env` 全局表、字符串 `gsub`），
//! 逐行翻译会把那些痕迹搬进 Rust，而我们的类型系统能做得更好。
//!
//! # 三个共同的架构问题，以及各自的答案
//!
//! 1. **时间从哪来**：Lua 直接读 `os.date()`。我们**注入**
//!    [`stele_core::Clock`]（见 `date_translator`），因此测试能定格时间。
//! 2. **随机数从哪来**：Lua 用 `math.random`。UUID 必须随机，
//!    而"可复现"是我们的一条铁律——因此随机源**注入**（见 `uuid`）。
//! 3. **候选的"类型"从哪来**：几个滤镜要按类型分支（`cand.type`）。
//!    那是 [`stele_core::CandidateKind`]，为此新加的一个轴。

use stele_core::{
    Candidate, CandidateKind, CandidateSink, Clock, Filter, Origin, Query, Score, Span, Tag,
    Translator,
};

// ─────────────────────────────────────────────────────────────────────────────
// 日历计算
// ─────────────────────────────────────────────────────────────────────────────
//
// 我们**不引第三方日期库**：`stele-core` / `stele-engine` 零依赖是
// 门禁强制的（`verify-zero-deps.sh`），而标准库只有 `SystemTime`
// （一个 Unix 时间戳，没有日历）。
//
// 好消息是"Unix 秒 → 年月日"的算法很短且有定论——**Howard Hinnant 的
// civil_from_days**（他把它放在公共领域，librime 生态里 C++ 的
// `std::chrono` 实现也是这套）。它没有查表、没有闰年特例分支，
// 因此也不会有一个"2100 年算错一天"的隐藏 bug。

/// 一个**本地**日历时刻。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CivilTime {
    /// 年（公元）。
    pub year: i64,
    /// 月（1–12）。
    pub month: u32,
    /// 日（1–31）。
    pub day: u32,
    /// 时（0–23）。
    pub hour: u32,
    /// 分（0–59）。
    pub minute: u32,
    /// 秒（0–59）。
    pub second: u32,
    /// 星期（0 = 周日）。
    pub weekday: u32,
}

/// 把 Unix 秒加上时区偏移，拆成本地日历时刻。
///
/// `offset_secs` 是当地相对 UTC 的偏移（东八区 `+28800`）。
/// 负的时间戳（1970 年之前）也能正确换算——`div_euclid` 保证向下取整。
#[must_use]
pub fn civil_from_unix(secs: i64, offset_secs: i64) -> CivilTime {
    let local = secs + offset_secs;
    let days = local.div_euclid(86_400);
    let rem = local.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    CivilTime {
        year,
        month,
        day,
        hour: u32::try_from(rem / 3600).unwrap_or(0),
        minute: u32::try_from((rem % 3600) / 60).unwrap_or(0),
        second: u32::try_from(rem % 60).unwrap_or(0),
        // 1970-01-01 是星期四（=4）。
        weekday: u32::try_from((days + 4).rem_euclid(7)).unwrap_or(0),
    }
}

/// Hinnant 的 `civil_from_days`：把"1970-01-01 起的天数"变成年月日。
///
/// 返回值是 `(年, 月, 日)`。它把三月当作一年的开始（于是闰日落在年末），
/// 这样闰年规则就退化成"每 4 年一次"——**这是它没有特例分支的原因**。
#[must_use]
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // 把纪元挪到 0000-03-01。
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, u32::try_from(m).unwrap_or(1), u32::try_from(d).unwrap_or(1))
}

/// 中文星期名。
const WEEKDAYS_ZH: [&str; 7] = [
    "星期日",
    "星期一",
    "星期二",
    "星期三",
    "星期四",
    "星期五",
    "星期六",
];

/// 英文月份名（`November`）。
const MONTHS_EN: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// 中文数字（用于日期）。
const DIGITS_ZH: [&str; 10] = ["〇", "一", "二", "三", "四", "五", "六", "七", "八", "九"];

/// 中文月份名。
const MONTHS_ZH: [&str; 13] = [
    "", "一月", "二月", "三月", "四月", "五月", "六月", "七月", "八月", "九月", "十月", "十一月",
    "十二月",
];

/// 一个年份的**逐位**中文写法：`2026` → `二〇二六`。
///
/// 注意它用的是 `〇` 而不是 `零`——这是《出版物上数字用法》与
/// rime-ice 的行为（那边的 `datezh` 就是这个写法）。
#[must_use]
pub fn year_zh(year: i64) -> String {
    let mut out = String::new();
    for c in year.to_string().chars() {
        let d = c.to_digit(10).unwrap_or(0) as usize;
        out.push_str(DIGITS_ZH[d]);
    }
    out
}

/// 中文月日：`11` → `十一月`、`29` → `二十九日`。
#[must_use]
pub fn month_day_zh(month: u32, day: u32) -> String {
    let m = MONTHS_ZH
        .get(month as usize)
        .copied()
        .unwrap_or("")
        .to_owned();
    format!("{m}{}日", day_zh(day))
}

/// 1–31 的中文读法（`二十` 系列用 `廿`？**不用**——正式日历写 `二十九`）。
#[must_use]
pub fn day_zh(day: u32) -> String {
    match day {
        1..=10 => DIGITS_ZH[day as usize].to_owned(),
        11..=19 => format!("十{}", DIGITS_ZH[(day - 10) as usize]),
        20 => "二十".to_owned(),
        21..=29 => format!("二十{}", DIGITS_ZH[(day - 20) as usize]),
        30 => "三十".to_owned(),
        31 => "三十一".to_owned(),
        other => other.to_string(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// date_translator
// ─────────────────────────────────────────────────────────────────────────────

/// **日期/时间翻译器**：敲 `rq` 出今天，`xq` 出星期，`ts` 出时间戳…
///
/// # 时间从哪来：注入，不读系统
///
/// RIME 的 Lua 版本直接调 `os.date()`——它**每次查询都读一次系统时钟**，
/// 于是同一个输入在不同毫秒下可能给出不同候选。我们的铁律是
/// "候选列表是 (输入, 状态) 的纯函数"，因此时钟**注入**：
///
/// - 生产环境注入真实时钟（一次按键内只读一次，因此结果自洽）；
/// - 测试注入 [`stele_core::FrozenClock`]，于是"2026 年 11 月 29 日是星期几"
///   这类断言才能写。
///
/// 这不是洁癖：`--dump-config` 与对照实验都需要**可复现**的输出，
/// 而"当前时间"是唯一一个天然不可复现的输入。
pub struct DateTranslator {
    /// 注入的时钟。
    clock: std::sync::Arc<dyn Clock>,
    /// 各格式的触发词。
    spec: crate::spec::DateSpec,
    /// 本翻译器负责的标签。
    tags: Vec<Tag>,
}

impl DateTranslator {
    /// 构造。
    #[must_use]
    pub fn new(
        clock: std::sync::Arc<dyn Clock>,
        spec: crate::spec::DateSpec,
        tags: Vec<Tag>,
    ) -> Self {
        Self { clock, spec, tags }
    }

    /// 为某个触发词产出的全部候选。
    ///
    /// 拆成独立方法是为了**可测试**：测试不必经过流水线就能断言
    /// "给定这个时刻、这个触发词，产出什么文本"。
    #[must_use]
    pub fn render(&self, input: &str) -> Vec<(String, Option<String>)> {
        let now = self.clock.now_secs();
        let t = civil_from_unix(i64::try_from(now).unwrap_or(0), self.clock.utc_offset_secs());
        let mut out: Vec<(String, Option<String>)> = Vec::new();

        if input == self.spec.date {
            out.push((format!("{:04}-{:02}-{:02}", t.year, t.month, t.day), None));
            // 简写形式：`2026-11-29` 之外再给一个不带前导零的？**不给**——
            // RIME 那边也只给这一种，多给会挤掉别的候选。
        } else if input == self.spec.time {
            out.push((format!("{:02}:{:02}", t.hour, t.minute), None));
        } else if input == self.spec.week {
            out.push((WEEKDAYS_ZH[t.weekday as usize].to_owned(), None));
        } else if input == self.spec.datetime {
            out.push((self.iso8601(&t), None));
        } else if input == self.spec.timestamp {
            out.push((now.to_string(), None));
        } else if input == self.spec.date_zh {
            out.push((
                format!(
                    "{}年{}",
                    year_zh(t.year),
                    month_day_zh(t.month, t.day)
                ),
                None,
            ));
        } else if input == self.spec.date_en {
            let mon = MONTHS_EN
                .get((t.month as usize).saturating_sub(1))
                .copied()
                .unwrap_or("");
            // 美式写法（`November 29, 2026`）——RIME 的 `dateen` 用这一种。
            out.push((format!("{mon} {}, {}", t.day, t.year), None));
        }
        out
    }

    /// ISO 8601 带本地偏移：`2026-11-29T18:13:11+08:00`。
    fn iso8601(&self, t: &CivilTime) -> String {
        let off = self.clock.utc_offset_secs();
        let sign = if off < 0 { '-' } else { '+' };
        let abs = off.abs();
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}{:02}:{:02}",
            t.year,
            t.month,
            t.day,
            t.hour,
            t.minute,
            t.second,
            sign,
            abs / 3600,
            (abs % 3600) / 60
        )
    }
}

impl Translator for DateTranslator {
    fn translate(&self, q: &Query<'_>, span: Span, out: &mut CandidateSink<'_>) {
        for (text, comment) in self.render(q.segment_text) {
            out.push(Candidate {
                text,
                comment,
                // 与 rime-ice 的权重意图一致：这一类候选要**排在词库候选之前**
                // （那边用 `cand.quality = 100`，而我们的分数是对数域，
                // 取一个明显高于常用词的值）。
                score: Score::from_weight(50_000.0),
                origin: Origin::Literal,
                attr: stele_core::SpellingAttr::NORMAL,
                span,
                lane: stele_core::Lane::Input,
                kind: CandidateKind::Inline,
            });
        }
    }

    fn accepts(&self, tags: &[Tag]) -> bool {
        tags.iter().any(|t| self.tags.contains(t))
    }

    fn targets(&self) -> &[Tag] {
        &self.tags
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// unicode
// ─────────────────────────────────────────────────────────────────────────────

/// **Unicode 翻译器**：敲 `U62fc` 出「拼」。
///
/// # 行为（照 rime-ice 的 `unicode.lua`）
///
/// | 输入 | 产出 |
/// | --- | --- |
/// | 前缀 + 十六进制（≥2 位） | 那个码位的字符，注释是 `U<hex>` |
/// | 码位 > `0x10FFFF` | 一条「数值超限！」 |
/// | 码位 < `0x10000`（BMP） | **再加 15 条**：同一起始的后续码位 |
///
/// 最后一条是它的实用之处：`U62fc` 会连带给出 `U62fd`…`U630b`
/// 一整块，用户不必逐个试。
///
/// # 一个刻意的差别
///
/// 它的前缀在 RIME 那边是**从 `recognizer/patterns/unicode` 的第 2 个字符
/// 现读**的（`"^U[a-f0-9]+"` → `U`）。我们让它在配置里显式写出
/// （默认值相同）。理由：从正则源码里"取第 2 个字符"是**隐式约定**——
/// 用户改了正则却忘了改前缀，症状是"这个功能突然不生效"。
pub struct UnicodeTranslator {
    /// 前缀字符。
    prefix: char,
    /// 本翻译器负责的标签。
    tags: Vec<Tag>,
}

impl UnicodeTranslator {
    /// 构造。
    #[must_use]
    pub fn new(spec: &crate::spec::UnicodeSpec, tags: Vec<Tag>) -> Self {
        Self {
            prefix: spec.prefix,
            tags,
        }
    }

    /// 一次查询最多产出多少条（BMP 时是 16 条，这里留一倍余量）。
    const CAP: usize = 32;

    /// 解析输入，返回要产出的候选文本。
    ///
    /// `None` 表示"这条输入不是本翻译器管的"——**与"解析失败"是两件事**，
    /// 但这里对使用者而言结果一样（都不出候选），因此不区分。
    #[must_use]
    pub fn render(&self, input: &str) -> Vec<(String, String)> {
        let mut chars = input.chars();
        if chars.next() != Some(self.prefix) {
            return Vec::new();
        }
        let hex: String = chars.collect();
        // 至少两位十六进制——一位的话 `U1` 太容易误触发。
        if hex.chars().count() < 2 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Vec::new();
        }
        let Ok(code) = u32::from_str_radix(&hex, 16) else {
            return Vec::new();
        };
        if code > 0x10_FFFF {
            return vec![("数值超限！".to_owned(), String::new())];
        }
        let Some(c) = char::from_u32(code) else {
            return vec![("数值超限！".to_owned(), String::new())];
        };
        let mut out = vec![(c.to_string(), format!("U{code:x}"))];
        if code < 0x1_0000 {
            for i in 0..16u32 {
                let Some(n) = code.checked_mul(16).and_then(|v| v.checked_add(i)) else {
                    break;
                };
                let Some(nc) = char::from_u32(n) else { continue };
                // 控制字符与代理区不该出现在候选里（RIME 那边会产出，
                // 但那是它的疏漏——用户拿到一个不可见的候选只会困惑）。
                if n < 0x20 || (0xD800..0xE000).contains(&n) {
                    continue;
                }
                out.push((nc.to_string(), format!("U{code:x}~{i:x}")));
                if out.len() >= Self::CAP {
                    break;
                }
            }
        }
        out
    }
}

impl Translator for UnicodeTranslator {
    fn translate(&self, q: &Query<'_>, span: Span, out: &mut CandidateSink<'_>) {
        for (i, (text, comment)) in self.render(q.segment_text).into_iter().enumerate() {
            let step = f64::from(u32::try_from(i).unwrap_or(u32::MAX));
            let score = Score::from_weight(50_000.0 - step * 10.0);
            out.push(Candidate {
                text,
                comment: (!comment.is_empty()).then_some(comment),
                score,
                origin: Origin::Literal,
                attr: stele_core::SpellingAttr::NORMAL,
                span,
                lane: stele_core::Lane::Input,
                kind: CandidateKind::Inline,
            });
        }
    }

    fn accepts(&self, tags: &[Tag]) -> bool {
        tags.iter().any(|t| self.tags.contains(t))
    }

    fn targets(&self) -> &[Tag] {
        &self.tags
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// uuid
// ─────────────────────────────────────────────────────────────────────────────

/// **UUID 翻译器**：敲触发词（默认 `uuid`）出一条 UUID。
///
/// # 行为（照 rime-ice 的 `uuid.lua`）
///
/// 17 个随机字节按固定模板拼成 `8-4-4-4-12` 的形式。
///
/// # 一处**故意不同**：我们产出合法的 v4 UUID
///
/// 那份 Lua 的位运算写错了：
///
/// ```lua
/// ((rand(0, 255) % 16) + 64),    -- 版本位：本意是 (x % 16) | 0x40
/// ((rand(0, 255) % 64) + 128),   -- 变体位：本意是 (x % 64) | 0x80
/// ```
///
/// `+ 64` 而不是 `| 64`，于是：
/// - 版本位那一个字节落在 `0x40–0x4F`，但**低 4 位没被清干净**
///   （正确的 v4 要求高 4 位是 `0100`、低 4 位任意——这一条其实碰巧对了）；
/// - 变体位落在 `0x80–0xBF`，也碰巧落在 `10xx_xxxx` 区间内。
///
/// 所以它**碰巧**产出了合法的 UUID——但那是巧合而不是设计：
/// `% 16` + `+ 64` 与 `(x % 16) | 0x40` 在这里恰好等价，因为
/// `x % 16 < 16` 不会进位到 64 以上的那两位。
///
/// 我们写**显式的位运算**并加注释：**同样的字节、同样的结果、但意图明确**。
/// 用户看到的东西完全一样（这一点由测试对齐），而下一个读代码的人
/// 不必再推一遍"这两个写法是不是碰巧一样"。
pub struct UuidTranslator {
    /// 随机源（注入）。
    random: std::sync::Mutex<Box<dyn stele_core::RandomSource>>,
    /// 触发词。
    trigger: String,
    /// 本翻译器负责的标签。
    tags: Vec<Tag>,
}

impl UuidTranslator {
    /// 构造。
    #[must_use]
    pub fn new(
        random: Box<dyn stele_core::RandomSource>,
        spec: &crate::spec::UuidSpec,
        tags: Vec<Tag>,
    ) -> Self {
        Self {
            random: std::sync::Mutex::new(random),
            trigger: spec.trigger.clone(),
            tags,
        }
    }

    /// 生成一条 UUID（v4 形式）。
    ///
    /// 用 `Mutex` 包随机源：`Translator` 要求 `Send`，而 `next_u64`
    /// 需要 `&mut`。加锁的代价可忽略（一条 UUID 只取两次随机数），
    /// 而它换来的是"随机源不必是 `Sync` 的"——测试里的确定性发生器
    /// 因此不需要内部可变性。
    #[must_use]
    pub fn generate(&self) -> String {
        let mut bytes = [0u8; 16];
        {
            let mut rng = self.random.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let a = rng.next_u64().to_le_bytes();
            let b = rng.next_u64().to_le_bytes();
            bytes[..8].copy_from_slice(&a);
            bytes[8..].copy_from_slice(&b);
        }
        // 版本位：高 4 位 = 0100（v4）。
        bytes[6] = (bytes[6] & 0x0F) | 0x40;
        // 变体位：高 2 位 = 10（RFC 4122）。
        bytes[8] = (bytes[8] & 0x3F) | 0x80;
        format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
        )
    }
}

impl Translator for UuidTranslator {
    fn translate(&self, q: &Query<'_>, span: Span, out: &mut CandidateSink<'_>) {
        if q.segment_text != self.trigger {
            return;
        }
        out.push(Candidate {
            text: self.generate(),
            comment: None,
            score: Score::from_weight(50_000.0),
            origin: Origin::Literal,
            attr: stele_core::SpellingAttr::NORMAL,
            span,
            lane: stele_core::Lane::Input,
            kind: CandidateKind::Inline,
        });
    }

    fn accepts(&self, tags: &[Tag]) -> bool {
        tags.iter().any(|t| self.tags.contains(t))
    }

    fn targets(&self) -> &[Tag] {
        &self.tags
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// v_filter
// ─────────────────────────────────────────────────────────────────────────────

/// **v 模式单字优先**：敲 `v` + 一个字符时，把单字候选提到前面。
///
/// # 它解决的具体问题
///
/// rime-ice 给英文翻译器设了 `initial_quality: 1.1`，比拼音大，
/// 于是敲 `va` 时候选是「van vain … ā á ǎ à」——**用户想要的是带声调的
/// 韵母，却先看到英文单词**。这个滤镜把"长度为 1 个字符"的候选提前。
///
/// # 触发条件（两条都要满足）
///
/// 1. 当前编码以 `v` 开头（**不是** `segment_text`——见下）；
/// 2. 编码长度恰好为 2（`v` + 一个字符）。
///
/// 长度必须是 2：这是"v 模式"的定义，敲更长的编码时用户已经在挑词了。
///
/// # 为什么它读整串输入而不是本段正文
///
/// `v_filter` 判的是"用户在不在 v 模式"，那是**整串输入**的性质
/// （rime-ice 那边也是 `context.input`）。用 `segment_text` 的话，
/// `v` 本身被标点段吃掉后就看不见了。这是我们唯一一个读 `q.input`
/// 的滤镜，理由写在这里以免被当成疏漏。
pub struct VFilter {
    /// 例外表：这些候选**无论多长**都排在最前。
    ///
    /// 默认值是 rime-ice 的：数字键帽 emoji 与 `Vs.`——它们是
    /// `symbols_v` 符号表里的条目，用户敲 `v1` 时最可能想要的就是它们。
    exceptions: Vec<String>,
}

impl Default for VFilter {
    fn default() -> Self {
        Self::new(vec![
            "0️⃣".to_owned(),
            "1️⃣".to_owned(),
            "2️⃣".to_owned(),
            "3️⃣".to_owned(),
            "4️⃣".to_owned(),
            "5️⃣".to_owned(),
            "6️⃣".to_owned(),
            "7️⃣".to_owned(),
            "8️⃣".to_owned(),
            "9️⃣".to_owned(),
            "Vs.".to_owned(),
        ])
    }
}

impl VFilter {
    /// 构造。
    #[must_use]
    pub fn new(exceptions: Vec<String>) -> Self {
        Self { exceptions }
    }
}

impl Filter for VFilter {
    fn apply(&self, q: &Query<'_>, _span: Span, cands: &mut Vec<Candidate>) {
        let code = q.input;
        // 只处理 `v` + 恰好一个字符。
        if code.len() != 2 || !code.starts_with('v') {
            return;
        }
        let mut head: Vec<Candidate> = Vec::new();
        let mut tail: Vec<Candidate> = Vec::new();
        for c in cands.drain(..) {
            let is_single = c.text.chars().count() == 1;
            if is_single || self.exceptions.contains(&c.text) {
                head.push(c);
            } else {
                tail.push(c);
            }
        }
        head.extend(tail);
        *cands = head;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// reduce_english_filter
// ─────────────────────────────────────────────────────────────────────────────

/// **降低英文候选的位置**：敲某个编码时，把候选里的英文单词往后放。
///
/// # 它解决的具体问题
///
/// rime-ice 给英文翻译器设了 `initial_quality: 1.1`（比拼音大），于是
/// 敲 `rug` 得到「1. rug  2. 如果 …」——**用户更可能想要「如果」**。
/// 这个滤镜把英文单词降到第 `idx` 位。
///
/// # 三种模式（`mode`）
///
/// | 模式 | 用哪些编码触发 |
/// | --- | --- |
/// | `all`（默认） | 内置表 **＋** 配置里的 `words` |
/// | `custom` | **只有**配置里的 `words` |
/// | `none` | 都不降（等于没启用） |
///
/// # 一张内置表，以及它为什么在这里而不在仓库里
///
/// rime-ice 那边内置了约 500 个"拼音形状的英文单词"（`aid`、`and`、`bat`…）
/// ——它们的共同点是**恰好长得像某个拼音**，所以会跟中文抢位置。
///
/// 我们把这张表**做成数据而不是代码**（[`ReduceEnglishSpec::words`]），
/// 因为它是一份**词表**：它的来源与改动理由都属于方案作者，不属于引擎。
/// 内置表本身在 rime-ice 里是 GPL 项目的一部分，而我们不复制它——
/// 想用的人把它写进方案的 `words` 即可，格式完全一样。
pub struct ReduceEnglishFilter {
    mode: crate::spec::ReduceMode,
    idx: usize,
    /// 触发降权的编码集合（`custom` 模式只有它，`all` 模式是它）。
    words: std::collections::BTreeSet<String>,
    /// 内置表（`all` 模式才用）。
    builtin: std::collections::BTreeSet<String>,
}

impl ReduceEnglishFilter {
    /// 由配置构造，用我们的[内置表](Self::BUILTIN)。
    #[must_use]
    pub fn new(spec: &crate::spec::ReduceEnglishSpec) -> Self {
        Self {
            mode: spec.mode,
            idx: spec.idx.max(1),
            words: spec.words.iter().map(|w| w.to_lowercase()).collect(),
            builtin: Self::BUILTIN.iter().map(|w| (*w).to_owned()).collect(),
        }
    }

    /// 我们的内置表：**常见的"拼音形状"英文短词**。
    ///
    /// # 它从哪来、为什么在这里、为什么这么小
    ///
    /// rime-ice 有一张约 500 条的同类表（在它的 Lua 里）。我们**没有复制
    /// 它**——那是 GPL 项目的内容，而 Stele 是宽松许可。
    ///
    /// 这里是我们自己收的一小批，判据只有一条：
    /// **这个词恰好是一个合法拼音，且是常用英文单词**。
    /// 也就是"会跟中文抢位置"的那些：
    ///
    /// - `rug` 是 `ru`+`g` 的合法简拼，同时是英文单词；
    /// - `and` / `bad` / `can` 是完整拼音或简拼；
    /// - `Mac` / `cd` / `ps` 是缩写形状的编码。
    ///
    /// **刻意做得小**：这张表越长，误伤越多（把用户真想打的英文压下去）。
    /// 想加的人往方案的 `words:` 里写，格式一样、效果一样，且**不必改代码**。
    ///
    /// 一千个人眼里的"常用英文词"不一样，因此这张表的目标不是"全"，
    /// 而是"**默认值不惹事**"。
    pub const BUILTIN: &'static [&'static str] = &[
        "aid", "aim", "air", "and", "ant", "any", "bad", "bag", "ban", "band", "bang", "bank",
        "bar", "bat", "bay", "bed", "ben", "bend", "bent", "bet", "bib", "bid", "big", "bin",
        "bit", "bob", "bog", "bop", "bow", "box", "boy", "bud", "bug", "bus", "but", "buy",
        "cab", "cad", "cam", "can", "cap", "car", "cat", "ceo", "chi", "cod", "cop", "cry",
        "cum", "cup", "cur", "cut", "dam", "day", "den", "dew", "did", "dig", "dim", "din",
        "dip", "dog", "dot", "dry", "dub", "dun", "duo", "ear", "eat", "egg", "end", "era",
        "err", "eye", "fad", "fan", "far", "fat", "fax", "fee", "few", "fig", "fin", "fit",
        "fix", "flu", "fly", "fog", "for", "fox", "fry", "fun", "fur", "gag", "gap", "gas",
        "gay", "gel", "gem", "get", "gin", "god", "got", "gum", "gun", "gut", "guy", "gym",
        "had", "ham", "has", "hat", "hay", "hen", "her", "hey", "hid", "him", "hip", "his",
        "hit", "hop", "hot", "how", "hub", "hug", "hum", "hut", "ice", "ill", "ink", "ion",
        "jar", "jaw", "jazz", "jet", "jog", "joy", "jug", "key", "kid", "kin", "kit", "lab",
        "lad", "lag", "lap", "law", "lay", "led", "leg", "let", "lid", "lie", "lip", "lit",
        "log", "lot", "low", "mad", "man", "map", "mat", "max", "may", "men", "met", "mix",
        "mob", "mom", "mop", "mud", "mug", "nap", "net", "new", "nil", "nip", "nod", "nor",
        "not", "now", "nun", "nut", "oak", "odd", "off", "oil", "old", "one", "our", "out",
        "owe", "owl", "own", "pad", "pal", "pan", "par", "pat", "paw", "pay", "pea", "peg",
        "pen", "per", "pet", "pie", "pig", "pin", "pit", "pod", "pop", "pot", "pro", "pub",
        "pup", "put", "ram", "ran", "rap", "rat", "raw", "ray", "red", "rib", "rid", "rig",
        "rim", "rip", "rob", "rod", "rot", "row", "rub", "rug", "rum", "run", "rut", "sad",
        "sag", "sail", "sam", "sap", "sat", "saw", "say", "sea", "see", "set", "sew", "she",
        "shy", "sin", "sip", "sir", "sis", "sit", "six", "ski", "sky", "sly", "sob", "sod",
        "son", "sow", "soy", "spa", "spy", "sub", "sue", "sum", "sun", "sup", "tab", "tag",
        "tan", "tap", "tar", "tax", "tea", "ten", "the", "tie", "tin", "tip", "toe", "ton",
        "too", "top", "toy", "try", "tub", "tug", "two", "ugh", "van", "vat", "vet", "via",
        "vow", "wag", "war", "was", "wax", "way", "web", "wed", "wet", "who", "why", "wig",
        "win", "wit", "woe", "wok", "won", "wow", "yak", "yam", "yap", "yes", "yet", "you",
        "zap", "zen", "zip", "zoo",
        // 缩写形状的编码（rime-ice 的表里也有一批）。
        "cd", "cn", "hk", "js", "ml", "mt", "ps", "pk", "as", "ak", "dj",
    ];

    /// 这个编码要不要触发降权。
    #[must_use]
    pub fn triggers(&self, code: &str) -> bool {
        match self.mode {
            crate::spec::ReduceMode::None => false,
            crate::spec::ReduceMode::Custom => self.words.contains(code),
            crate::spec::ReduceMode::All => {
                self.words.contains(code) || self.builtin.contains(code)
            }
        }
    }
}

impl Filter for ReduceEnglishFilter {
    fn apply(&self, q: &Query<'_>, _span: Span, cands: &mut Vec<Candidate>) {
        if !self.triggers(q.input) {
            return;
        }
        // 只看**前 `idx` 位**：`idx` 之外的不动（那边是用户已经翻页、
        // 或者本来就不竞争的位置）。
        let head_len = self.idx.min(cands.len());
        let mut demoted: Vec<Candidate> = Vec::new();
        let mut head: Vec<Candidate> = Vec::with_capacity(head_len);
        for c in cands.iter().take(head_len) {
            // 用户词库的词**不降权**——用户自己打过并确认过的，
            // 他的偏好比我们的启发式更可信。
            if is_english_word(&c.text) && c.kind != CandidateKind::UserTable {
                demoted.push(c.clone());
            } else {
                head.push(c.clone());
            }
        }
        // 顺序：非英文（原序）→ 被降权的英文（原序）→ 其余候选。
        let mut out = head;
        out.extend(demoted);
        out.extend_from_slice(&cands[head_len..]);
        *cands = out;
    }
}

/// 这个候选是不是"一个英文单词"。
///
/// 判据（照 rime-ice 的实现）：**含 ASCII 字母**、**不含空格**、
/// **不含非 ASCII**。第二条排除「New York」这类短语，第三条排除中文词。
#[must_use]
pub fn is_english_word(text: &str) -> bool {
    if text.is_empty() || text.contains(' ') {
        return false;
    }
    if !text.is_ascii() {
        return false;
    }
    text.chars().any(|c| c.is_ascii_alphabetic())
}


// ─────────────────────────────────────────────────────────────────────────────
// number_translator
// ─────────────────────────────────────────────────────────────────────────────

/// **数字转中文**：敲 `R3355` 出「三千三百五十五」、
/// `R1234.5` 出「壹仟贰佰叁拾肆元伍角」。
///
/// # 四种形态
///
/// 每一条输入产出最多四条候选，注释标明是哪一种：
///
/// | 注释 | 例（`3355.433`） |
/// | --- | --- |
/// | 〔数字小写〕 | 三千三百五十五点四三三 |
/// | 〔数字大写〕 | 叁仟叁佰伍拾伍点肆叁叁 |
/// | 〔金额小写〕 | 三千三百五十五元四角三分三厘 |
/// | 〔金额大写〕 | 叁仟叁佰伍拾伍元肆角叁分叁厘 |
///
/// # 我们是**逐字节对齐**实现它的，包括几处怪癖
///
/// 这个零件的规则是"会计习惯"，没有权威标准可对照，因此唯一的判据
/// 是**与上游一致**。下面这些怪癖被保留并在测试里钉住：
///
/// | 输入 | 输出 | 说明 |
/// | --- | --- | --- |
/// | `R0.5` 的〔数字小写〕 | 数值超限！五 | 整数部分为 0 的小数 |
/// | `R0001` | 〇一 | 不是「一」 |
/// | `R5.` | 五点 | 小数部分为空仍出「点」 |
/// | `R1234567890123`（13 位） | 数值超限！ | |
///
/// **为什么不"顺手修好"**：这些是用户可观察的输出，改了就是行为不等价。
/// "我觉得这里该是别的样子"正是 P3 里让我猜错两次的心态。
/// 想改的人应当先拿一个对照实验证明上游也改了。
///
/// # 与上游**唯一**一处有意不同
///
/// 上游从 `recognizer/patterns/number` 的第 2 个字符现读触发前缀
/// （`"^R[0-9]+[.]?[0-9]*"` → `R`）。我们让它在配置里显式写出
/// （默认相同）——从正则源码里"取第 2 个字符"是隐式约定，改了正则
/// 就静默失效，而症状是"这个功能突然不好使了"。
pub struct NumberTranslator {
    /// 触发前缀。
    prefix: char,
    /// 本翻译器负责的标签。
    tags: Vec<Tag>,
}

/// 一次转换用哪套用字。
#[derive(Clone, Copy, PartialEq, Eq)]
enum NumberStyle {
    /// 小写（日常写法）：〇一二三…十百千万亿。
    Lower,
    /// 大写（财务写法）：零壹贰叁…拾佰仟萬億。
    Upper,
}

impl NumberStyle {
    /// 数字用字。
    fn figures(self) -> [&'static str; 10] {
        match self {
            Self::Lower => ["〇", "一", "二", "三", "四", "五", "六", "七", "八", "九"],
            Self::Upper => ["零", "壹", "贰", "叁", "肆", "伍", "陆", "柒", "捌", "玖"],
        }
    }

    /// 组内数位（十百千）。
    fn units(self) -> [&'static str; 4] {
        match self {
            Self::Lower => ["", "十", "百", "千"],
            Self::Upper => ["", "拾", "佰", "仟"],
        }
    }

    /// "零"这个字（小写是 〇，大写是 零）。
    fn zero(self) -> &'static str {
        self.figures()[0]
    }

}

/// 数字串转中文读法（`formatNum` 的上位函数）。
///
/// `tail` 是**接在末尾的那个字**：数字形态接「点」（小数）或空串，
/// 金额形态接「元」。上游把它作为一个参数传进来，我们照做——
/// 它决定了 `R5.` 这种输入是否出现「点」。
fn number_to_chinese(
    digits: &str,
    style: NumberStyle,
    zero: &str,
    scale: [&str; 2],
    tail: &str,
) -> String {
    // 上游把「零」也当成一个**可传的参数**（`wordFigure[1]`），而四个
    // 调用点传的值**不完全与该分支的用字一致**：
    //
    // | 分支 | 数字用字 | 传进去的「零」 |
    // | --- | --- | --- |
    // | 数字小写（有小数） | 〇一二三 | 〇 |
    // | 数字大写（有小数） | 壹贰叁 | **〇**（不是零！） |
    // | 数字小写（无小数） | 〇一二三 | 〇 |
    // | 数字大写（无小数） | 零壹贰叁 | 零 |
    // | 金额（小写/大写） | 〇…／零… | 〇／零（默认值） |
    //
    // 第二行那个「〇」是上游的一处不一致，而它**有可观察的后果**：
    // `R1.5` 的〔数字大写〕是「壹点伍」——因为去前导零那一步拿
    // 「〇」去找「壹」，什么也没找到。我按"大写就用零"实现时，
    // 对照测试指出 12 处不一致。
    let len = digits.len();
    let mut result = if len < 5 {
        group_to_chinese(digits, style)
    } else if len < 9 {
        format!(
            "{}{}{}",
            group_to_chinese(&digits[..len - 4], style),
            scale[0],
            group_to_chinese(&digits[len - 4..], style)
        )
    } else if len < 13 {
        format!(
            "{}{}{}{}{}",
            group_to_chinese(&digits[..len - 8], style),
            scale[1],
            group_to_chinese(&digits[len - 8..len - 4], style),
            scale[0],
            group_to_chinese(&digits[len - 4..], style)
        )
    } else {
        String::new()
    };

    // 上游那串 `gsub`，逐条照抄——**顺序有意义**，而且每一条都真的用到了：
    //
    // | 步骤 | 它负责的情形 |
    // | --- | --- |
    // | 去掉开头的零 | `R007` → 「七」（组内算出「〇七」） |
    // | 零+万 / 零+亿 | `R10001` → 「一万〇一」而不是「一万〇〇一」 |
    // | 连续零压成一个 | 组间相接处 |
    // | 去掉末尾的零 | 组末尾留下的零 |
    // 上游的 `gsub("^" .. wordFigure[1], "")`。注意 `wordFigure[1]` 是
    // **零**（数字形态是「〇」），不是尾字。
    if let Some(rest) = result.strip_prefix(zero) {
        result = rest.to_string();
    }
    // `digitUnit[1]` / `[2]` 是数量级（万/亿），与尾字无关。
    result = replace_once(&result, &format!("{zero}{}", scale[0]), "");
    result = replace_once(&result, &format!("{zero}{}", scale[1]), "");
    result = collapse_zeros(&result, zero);
    // `gsub(zero .. "$", "")` —— 去掉**末尾的零**。
    if result.ends_with(zero) {
        result.truncate(result.len() - zero.len());
    }
    // 超过四位时，「一十…」写成「十…」（口语里不说「一十万」）。
    if len > 4 {
        let one_ten = format!("{}{}", style.figures()[1], style.units()[1]);
        if result.starts_with(&one_ten) {
            result = format!("{}{}", style.units()[1], &result[one_ten.len()..]);
        }
    }
    if result.is_empty() {
        "数值超限！".to_owned()
    } else {
        // `result` 可能已经是"数值超限！"——上游那时也会接上尾字，
        // 于是 `R0.5` 的〔数字大写〕是「零点伍」而不是「点伍」。
        // 我们不要自作聪明：两者都照抄。
        if result == "数值超限！" && tail.is_empty() {
            result
        } else {
            format!("{result}{tail}")
        }
    }
}

/// 把**第一次**出现的连续两个「〇」换成一个。
///
/// # 为什么不是"把所有连续零都压成一个"
///
/// 我第一版就是那么写的（直觉上更"对"），结果 `R0001` 从「〇一」变成了「一」。
/// 上游用的是 Lua 的 `gsub`，而 **`gsub` 默认只替换第一处**；
/// 写成 `gsub(pattern, repl, n)` 或 `%1` 才会全换。
///
/// 差别只在这种输入上出现：**带前导零**（`0001`）。对一个直接敲数字的
/// 用户来说，`0001` 是常见的（编号、序号），而「〇一」与「一」
/// 都是可读的——所以我们**照抄上游**，不自己"顺手修好"：
/// 改了就是行为不等价，而"我觉得该这样"正是 P3 里让我猜错两次的心态。
fn replace_once(s: &str, from: &str, to: &str) -> String {
    match s.find(from) {
        Some(i) => {
            let mut out = String::with_capacity(s.len());
            out.push_str(&s[..i]);
            out.push_str(to);
            out.push_str(&s[i + from.len()..]);
            out
        }
        None => s.to_owned(),
    }
}

/// 上游的 `gsub(zero .. zero, zero)`。
fn collapse_zeros(s: &str, zero: &str) -> String {
    replace_once(s, &format!("{zero}{zero}"), zero)
}

/// **最多四位**的数字转中文（`formatNum`）。
///
/// 自左向右拼：`"11"` → 「一」+「十」+「一」=「一十一」。
///
/// # 三处必须照抄的细节（我前两版都写错了）
///
/// 1. **`tonumber(num) == 0` 才算零**，不是"去掉前导零后为空"。
///    两者对 `"0001"` 给出不同答案：前者给「〇一」，后者给「一」。
/// 2. **全零的组返回单个「〇」**（`"0000"` → 「〇」），而它会被
///    `number_to_chinese` 的"零+数量级"规则清掉。
/// 3. **末尾的零在这里去掉**（`gsub(zero .. "$", "")`，上游写了两遍）。
///    这一条决定了 `R007` 是「七」而不是「〇七」——因为清前导零那一步
///    在 `number_to_chinese` 里，而这里先把末尾的零去干净了。
fn group_to_chinese(num: &str, style: NumberStyle) -> String {
    let figures = style.figures();
    let units = style.units();
    let zero = style.zero();
    if num.len() > 4 {
        return zero.to_owned();
    }
    let is_zero = num.is_empty() || num.bytes().all(|b| b == b'0');
    if is_zero {
        return zero.to_owned();
    }
    let bytes = num.as_bytes();
    let mut result = String::new();
    for i in 1..=num.len() {
        let d = usize::from(bytes[num.len() - i] - b'0');
        let n = figures[d];
        if n == zero {
            result = format!("{n}{result}");
        } else {
            result = format!("{n}{}{result}", units[i - 1]);
        }
    }
    // 上游的三步：压一次连续零 → 去掉末尾的零 → **再去一次**（连写两遍）。
    let result = collapse_zeros(&result, zero);
    let result = result.strip_suffix(zero).unwrap_or(&result).to_owned();
    let result = result.strip_suffix(zero).unwrap_or(&result).to_owned();
    result
}

/// 小数部分**逐位**转写（`number2zh`）：`433` → `四三三`。
fn digit_by_digit(dec: &str, style: NumberStyle) -> String {
    let figures = style.figures();
    let zero = style.zero();
    let mut result = String::new();
    for c in dec.chars() {
        if let Some(d) = c.to_digit(10) {
            result.push_str(figures[d as usize]);
        }
    }
    // 上游连写两遍同一个 `gsub` = **替换两次**（不是一次、也不是全部）。
    // 差别在 `R0.0001` 上可见：`〇〇〇一` 要变成 `〇一` 而不是 `〇〇一`。
    let once = collapse_zeros(&result, zero);
    collapse_zeros(&once, zero)
}

/// 金额的小数部分：`433` → `四角三分三厘`。
fn decimal_func(dec: &str, style: NumberStyle) -> String {
    let figures = style.figures();
    let zero = style.zero();
    // 上游先截到 4 位，再去掉**末尾**的零。
    let mut d: Vec<char> = dec.chars().take(4).collect();
    while d.last() == Some(&'0') {
        d.pop();
    }
    if d.is_empty() {
        return "整".to_owned();
    }
    let mut result = String::new();
    for (pos, c) in d.iter().enumerate() {
        let val = c.to_digit(10).unwrap_or(0) as usize;
        if val != 0 {
            result = format!("{result}{}{}", figures[val], DECIMAL_UNIT[pos]);
        } else {
            result.push_str(zero);
        }
    }
    // 上游连做两次同一个 `gsub`（各替换一次，共两次）。
    let once = collapse_zeros(&result, zero);
    collapse_zeros(&once, zero)
}

/// 小数点后位置的名称。
const DECIMAL_UNIT: [&str; 4] = ["角", "分", "厘", "毫"];

impl NumberTranslator {
    /// 构造。
    #[must_use]
    pub fn new(spec: &crate::spec::NumberSpec, tags: Vec<Tag>) -> Self {
        Self {
            prefix: spec.prefix,
            tags,
        }
    }

    /// 把一段输入渲染成候选（文本 + 注释）。
    ///
    /// 空数组表示这条输入不是本翻译器管的。
    #[must_use]
    pub fn render(&self, input: &str) -> Vec<(String, String)> {
        let mut chars = input.chars();
        if chars.next() != Some(self.prefix) {
            return Vec::new();
        }
        // 上游：去掉开头连续的字母，剩下的是数字部分。
        let body: String = input
            .chars()
            .skip_while(char::is_ascii_alphabetic)
            .collect();
        let (int_part, dec_part) = split_number(&body);
        if int_part.is_empty() && dec_part.unwrap_or("").is_empty() {
            return Vec::new();
        }
        let dec = dec_part.unwrap_or("");
        let mut out = Vec::new();
        if dec_part.is_some() {
            out.push((
                format!(
                    "{}{}",
                    number_to_chinese(int_part, NumberStyle::Lower, "〇", ["万", "亿"], "点"),
                    digit_by_digit(dec, NumberStyle::Lower)
                ),
                "〔数字小写〕".to_owned(),
            ));
            // **注意用字与数量级不成对**：上游这一支传的是
            // `{ "萬", "億" }` 数量级，但**数字用字仍是小写的
            // `〇一二三…`**（`wordFigure[1..3]` 传的是 `〇/一/十`，
            // 只有 `wordFigure[4]` 是「点」）。因此
            // `R1.5` 的〔数字大写〕是「一点伍」而不是「壹点伍」。
            //
            // 我一开始按"大写就用大写用字"实现，测试当场指出四处不一致。
            // 这类"看起来该成对、实际不成对"的地方，是逐字节对照的价值所在。
            out.push((
                format!(
                    "{}{}",
                    number_to_chinese(int_part, NumberStyle::Upper, "〇", ["萬", "億"], "点"),
                    digit_by_digit(dec, NumberStyle::Upper)
                ),
                "〔数字大写〕".to_owned(),
            ));
        } else {
            out.push((
                number_to_chinese(int_part, NumberStyle::Lower, "〇", ["万", "亿"], ""),
                "〔数字小写〕".to_owned(),
            ));
            // 无小数时，这一支**才**是大写用字（`{零,壹,拾,元}`）。
            out.push((
                number_to_chinese(int_part, NumberStyle::Upper, "零", ["萬", "億"], ""),
                "〔数字大写〕".to_owned(),
            ));
        }
        out.push((
            format!(
                "{}{}",
                number_to_chinese(int_part, NumberStyle::Lower, "〇", ["万", "亿"], "元"),
                decimal_func(dec, NumberStyle::Lower)
            ),
            "〔金额小写〕".to_owned(),
        ));
        // 注意上游**这一支没有传自定义数量级**，因此它用的是
        // `number2cnChar` 的默认值 `{万, 亿}`（**简体**），
        // 而不是〔数字大写〕那一支的 `{萬, 億}`。
        // 这是上游的一处不一致，但对齐就是对齐：`R10000.5` 的
        // 〔金额大写〕是「壹万元伍角」而不是「壹萬元伍角」。
        let mut upper_amount = format!(
            "{}{}",
            number_to_chinese(int_part, NumberStyle::Upper, "零", ["万", "亿"], "元"),
            decimal_func(dec, NumberStyle::Upper)
        );
        let _ = &mut upper_amount;
        // 会计书写要求：整数部分超过四位、以「拾」开头、且含「万/亿」时
        // 补「壹」——「壹拾万元整」而不是「拾万元整」。
        // 上游为 issue #989 加的，我们照抄。
        if int_part.len() > 4
            && upper_amount.starts_with("拾")
            && (upper_amount.contains('万') || upper_amount.contains('亿'))
        {
            upper_amount = upper_amount.replacen('拾', "壹拾", 1);
        }
        out.push((upper_amount, "〔金额大写〕".to_owned()));
        out
    }
}

impl Translator for NumberTranslator {
    fn translate(&self, q: &Query<'_>, span: Span, out: &mut CandidateSink<'_>) {
        for (i, (text, comment)) in self.render(q.segment_text).into_iter().enumerate() {
            let step = f64::from(u32::try_from(i).unwrap_or(u32::MAX));
            out.push(Candidate {
                text,
                comment: Some(comment),
                score: Score::from_weight(50_000.0 - step * 10.0),
                origin: Origin::Literal,
                attr: stele_core::SpellingAttr::NORMAL,
                span,
                lane: stele_core::Lane::Input,
                kind: CandidateKind::Inline,
            });
        }
    }

    fn accepts(&self, tags: &[Tag]) -> bool {
        tags.iter().any(|t| self.tags.contains(t))
    }

    fn targets(&self) -> &[Tag] {
        &self.tags
    }
}

/// 按上游的 `string.match(str, "^(%d*)(%.?)(%d*)")` 切分。
///
/// 返回 `(整数部分, 小数部分)`；`None` 表示**没有小数点**。
#[must_use]
pub fn split_number(s: &str) -> (&str, Option<&str>) {
    let digits_end = s
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map_or(s.len(), |(i, _)| i);
    let int_part = &s[..digits_end];
    let rest = &s[digits_end..];
    if let Some(after_dot) = rest.strip_prefix('.') {
        let dec_end = after_dot
            .char_indices()
            .find(|(_, c)| !c.is_ascii_digit())
            .map_or(after_dot.len(), |(i, _)| i);
        (int_part, Some(&after_dot[..dec_end]))
    } else {
        (int_part, None)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// pin_cand_filter
// ─────────────────────────────────────────────────────────────────────────────

/// **置顶候选**：在某个编码下把指定的词提到最前。
///
/// # 它为什么是最有用的一个滤镜
///
/// rime-ice 的默认方案里，`d` → 「的」、`m` → 「吗 嘛」、`hm` → 「后面」
/// 都是它做的。这类"单键出最常用字"是**品牌输入法上手快的主要原因**，
/// 而通用引擎给不了——它是**用户自己的偏好**，不是语言模型能推出来的。
///
/// # 两件必须说清的行为
///
/// ## 一、配置里的编码是**音节拼写**，不是候选自己的编码
///
/// `'ni hao` + 制表符 + `你好'` 生成两个键：`nihao`（原样去掉空格）与 `nih`
/// （最后一个音节的首字母）。于是敲 `nih` 时「你好」也会在首位。
///
/// 若最后一个音节以 `zh`/`ch`/`sh` 开头，还会**再生成一个两字母简码**：
/// `'zhi chi` + 制表符 + `支持'` → `zhichi`、`zhic`、`zhich`。这一条是为了让
/// "超级简拼"也能命中（用户敲 `zhich` 而不是 `zhic`）。
///
/// ## 二、**明确写出来的简码优先于自动派生的**
///
/// `'da zhuan` + 制表符 + `大专'` 会派生 `daz`；而 `'da z` + 制表符 + `打字'`
/// 显式定义了 `daz`。
/// 两者共存时 `daz` 归「打字」（先到先得，见 [`PinTable::build`] 的顺序），
/// 而 `dazh` 仍归「大专」。rime-ice 的文档专门举了这个例子。
///
/// # 与 `custom_phrase` 的分工（rime-ice 的注释强调过）
///
/// 这个滤镜**只提升已经在候选里的词**，**不能凭空造词**。想造词要写进
/// `custom_phrase.txt`。写一个词库里没有的词在这里，它**永远不会出现**——
/// 而且是静默的，因此装载期要报出来（见 `scheme.rs` 的检查）。
pub struct PinCandFilter {
    /// 编码 → 要置顶的词（按顺序）。
    table: PinTable,
}

impl PinCandFilter {
    /// 由配置构造。
    #[must_use]
    pub fn new(spec: &crate::spec::PinCandSpec) -> Self {
        Self {
            table: PinTable::build(&spec.entries),
        }
    }

    /// 底层表（供装载期检查与 `--dump-config`）。
    #[must_use]
    pub fn table(&self) -> &PinTable {
        &self.table
    }
}

/// 置顶表的索引。
///
/// **构造顺序即语义**：先写的先插入，而插入不覆盖已有的键
/// （`Entry::or_insert`）。于是"自动派生的简码"与"显式写出的简码"
/// 相遇时，**谁先被处理谁赢**——而 rime-ice 的行为是显式的赢，
/// 因此装载器**必须按"显式先、派生后"的顺序喂进来**（见 `build`）。
#[derive(Debug, Default)]
pub struct PinTable {
    keys: std::collections::BTreeMap<String, Vec<String>>,
}

impl PinTable {
    /// 由条目构造。
    ///
    /// # 两条规则，而它们**不对称**（这是它的全部复杂度所在）
    ///
    /// | 情形 | 结果 |
    /// | --- | --- |
    /// | 某个键被**显式写出** | 它归那一条，**自动派生不会覆盖它** |
    /// | 某个派生键被**多条**派生出来 | 它们**按声明顺序合并** |
    ///
    /// 第二条是要点。rime-ice 文档举的例子：
    ///
    /// ```yaml
    /// - da zhuan    大专
    /// - da zhong    大众
    /// ```
    ///
    /// 两个词都派生 `dazh`，于是敲 `dazh` 时**「大专、大众」都要在**，
    /// 且先写的在前。我第一版实现成"先到先得"，测试当场指出丢了「大众」。
    ///
    /// 而显式写法打破合并：
    ///
    /// ```yaml
    /// - da z        打字     # 显式声明 daz
    /// ```
    ///
    /// 于是 `daz` 归「打字」，而 `dazh` 仍是「大专、大众」。
    #[must_use]
    pub fn build(entries: &[crate::spec::PinEntry]) -> Self {
        // ① 显式键：**最后写的赢**（与 rime-ice 的 `env.pin_cands[k] = ...`
        //    一致——它是直接赋值，不是 `or_insert`）。
        let mut explicit: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for e in entries {
            let key = strip_non_letters(&e.preedit);
            let texts = split_texts(&e.texts);
            if key.is_empty() || texts.is_empty() {
                continue;
            }
            explicit.insert(key, texts);
        }

        // ② 派生键：按声明顺序**合并**（同一键被多条派生时累加）。
        let mut derived: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for e in entries {
            let texts = split_texts(&e.texts);
            if texts.is_empty() {
                continue;
            }
            for k in derived_keys(&e.preedit) {
                if k.is_empty() {
                    continue;
                }
                let slot = derived.entry(k).or_default();
                for t in &texts {
                    if !slot.contains(t) {
                        slot.push(t.clone());
                    }
                }
            }
        }

        // ③ 显式优先：派生键只在"没人显式写过"时生效。
        let mut keys = explicit;
        for (k, v) in derived {
            keys.entry(k).or_insert(v);
        }
        Self { keys }
    }

    /// 精确查一个键。
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&[String]> {
        self.keys.get(key).map(Vec::as_slice)
    }

    /// 查询：**先试精确，再逐字节回退前缀**。
    ///
    /// 回退是为了 `dian` 这种情形：用户敲 `dian` 时 preedit 会随候选
    /// 变成 `di`，而配置里写的是 `dian`。不回退就找不到。
    #[must_use]
    pub fn lookup(&self, key: &str) -> Option<&[String]> {
        if let Some(v) = self.keys.get(key) {
            return Some(v.as_slice());
        }
        for (i, _) in key.char_indices().rev() {
            if i == 0 {
                break;
            }
            if let Some(v) = self.keys.get(&key[..i]) {
                return Some(v.as_slice());
            }
        }
        None
    }

    /// 表里有多少个键。
    #[must_use]
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// 表是不是空的。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// 去掉配置里编码的标点与空格（`ni hao` → `nihao`）。
///
/// **只留 ASCII 字母**：配置里写的是编码（拼音/双拼），不是中文。
/// 取不出字母时返回**空串**（由调用方判断"这一条没有可用的编码"）——
/// 这里不返回 `Option`，因为"没有字母"与"没有这一条"是两件事，
/// 而前者用一个空串表达得更直接。
fn strip_non_letters(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphabetic)
        .collect::<String>()
        .to_lowercase()
}

/// 把一条配置里的词按 `" > "` 或空格分开。
fn split_texts(texts: &[String]) -> Vec<String> {
    texts
        .iter()
        .flat_map(|t| t.split(" > ").flat_map(str::split_whitespace))
        .map(str::to_owned)
        .filter(|t| !t.is_empty())
        .collect()
}

/// 一条配置**派生**出的简码（不含完整编码本身）。
///
/// 规则来自 rime-ice 的注释与实现：
///
/// | 配置 | 派生 |
/// | --- | --- |
/// | `ni hao` | `nih` |
/// | `zhi chi` | `zhic`、`zhich` |
/// | `bu hao chi` | `buhaoc`、`buhaoch` |
///
/// **只对最后一个音节做简写**——前面的音节必须完整写出来。
/// 这一条是刻意的：`nih` 比 `nh` 更难误触发，而 `nh` 已经有了
/// 拼写代数的超级简拼去管。
#[must_use]
pub fn derived_keys(preedit: &str) -> Vec<String> {
    let parts: Vec<&str> = preedit.split_whitespace().collect();
    if parts.len() < 2 {
        return Vec::new();
    }
    let (last, preceding) = match parts.split_last() {
        Some((last, rest)) => (*last, rest.join("")),
        None => return Vec::new(),
    };
    let preceding = strip_non_letters(&preceding);
    let last = strip_non_letters(last);
    if preceding.is_empty() || last.is_empty() {
        return Vec::new();
    }
    let mut out = vec![format!("{preceding}{}", &last[..1])];
    if last.starts_with("zh") || last.starts_with("ch") || last.starts_with("sh") {
        out.push(format!("{preceding}{}", &last[..2]));
    }
    out
}

impl Filter for PinCandFilter {
    fn apply(&self, q: &Query<'_>, _span: Span, cands: &mut Vec<Candidate>) {
        let letters = strip_non_letters(q.input);
        if letters.is_empty() || self.table.is_empty() {
            return;
        }
        let Some(texts) = self.table.lookup(&letters) else {
            return;
        };

        // 按配置里的**词序**收，而不是按候选顺序——"先写的排前面"
        // 是配置的语义（`da zhuan` 在 `da zhong` 之前，所以「大专」在前）。
        let mut pinned: Vec<Option<Candidate>> = vec![None; texts.len()];
        let mut others: Vec<Candidate> = Vec::new();
        let mut rest: Vec<Candidate> = Vec::new();
        let mut done = 0usize;
        let mut finished = false;

        for c in cands.drain(..) {
            if finished {
                rest.push(c);
                continue;
            }
            if let Some(i) = texts.iter().position(|t| *t == c.text) {
                if pinned[i].is_none() {
                    pinned[i] = Some(c);
                    done += 1;
                }
                if done == texts.len() || others.len() > 100 {
                    finished = true;
                }
            } else {
                others.push(c);
            }
        }

        let mut out: Vec<Candidate> = Vec::with_capacity(cands_capacity(&pinned, &others));
        out.extend(pinned.into_iter().flatten());
        out.extend(others);
        out.extend(rest);
        *cands = out;
    }
}

/// 预算容量，避免每次重排都重新分配。
fn cands_capacity(pinned: &[Option<Candidate>], others: &[Candidate]) -> usize {
    pinned.len() + others.len()
}

// ─────────────────────────────────────────────────────────────────────────────
// long_word_filter
// ─────────────────────────────────────────────────────────────────────────────

/// **长词优先**：把比第一个候选更长的词提到前面。
///
/// 解决的是"`xian` 给出「先 现 县 西安」"这类问题——「西安」更常用，
/// 但它的词条权重不如单字高。
///
/// # 行为（照 rime-ice 的 `long_word_filter.lua`）
///
/// 1. 前 `idx - 1` 个候选**原样通过**（不动用户看得惯的前几个位置）；
/// 2. 以**第一个候选的长度**为基准；
/// 3. 从第 `idx` 个开始，比基准长、且**不含字母数字**的候选被提前，
///    最多 `count` 个；
/// 4. 其余候选保持原序跟在后面。
///
/// 第 3 条的"不含字母数字"是刻意的：英文/数字候选不该被长词挤走，
/// 它们长度天然大但没有可比性。
pub struct LongWordFilter {
    count: usize,
    idx: usize,
}

impl LongWordFilter {
    /// 构造。
    #[must_use]
    pub fn new(spec: &crate::spec::LongWordSpec) -> Self {
        Self {
            count: spec.count.max(1),
            idx: spec.idx.max(1),
        }
    }
}

impl Filter for LongWordFilter {
    fn apply(&self, _q: &Query<'_>, _span: Span, cands: &mut Vec<Candidate>) {
        if cands.len() <= self.idx {
            return;
        }
        let base_len = cands
            .first()
            .map_or(0, |c| c.text.chars().count());
        // 前 `idx - 1` 个原样保留。
        let mut promoted: Vec<Candidate> = Vec::new();
        let mut rest: Vec<Candidate> = Vec::new();
        for (i, c) in cands.drain(..).enumerate() {
            if i + 1 < self.idx {
                rest.push(c);
                continue;
            }
            let len = c.text.chars().count();
            // **只认 ASCII 字母数字**——这一条踩过坑。
            //
            // RIME 那边的判据是 Lua 的 `text:find("[%a%d]")`，而在 Lua 里
            // `%a` 只匹配 ASCII 字母。我第一版写成 Rust 的
            // `char::is_alphanumeric()`——它对**汉字也返回 true**
            // （Unicode 的"字母"包含汉字），于是这个滤镜把**每一个中文候选
            // 都当成了"英文候选"，一个都不提升**：整个零件静默失效。
            //
            // 教训：跨语言移植时，**"看起来等价的谓词"是最危险的一类**。
            // 只承认两边行为一致的那部分（ASCII），而不是相信名字相同。
            let has_alnum = c.text.chars().any(|ch| ch.is_ascii_alphanumeric());
            if promoted.len() < self.count && len > base_len && !has_alnum {
                promoted.push(c);
            } else {
                rest.push(c);
            }
        }
        // 前 `idx - 1` 个 + 提升的 + 其余。
        let head_len = self.idx - 1;
        let mut out: Vec<Candidate> = Vec::with_capacity(rest.len() + promoted.len());
        out.extend(rest.drain(..head_len.min(rest.len())));
        out.extend(promoted);
        out.extend(rest);
        *cands = out;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// autocap_filter
// ─────────────────────────────────────────────────────────────────────────────

/// **英文自动大写**：输入码首字母大写 → 候选首字母大写；前两位以上大写 → 全大写。
///
/// 打 `Hello` 得到 `Hello`（而不是 `hello`），打 `HEllo` 得到 `HELLO`。
///
/// # 不转换的四种情况（照 rime-ice 的 `autocap_filter.lua`）
///
/// | 条件 | 例子 | 为什么 |
/// | --- | --- | --- |
/// | 码长为 1 | `A` → 不该变成别的 | 单字母太容易误伤 |
/// | 码首位是小写或标点 | `abc` | 用户没打算大写 |
/// | 候选含非字母数字标点空格的字符 | 含中文/emoji | 不是英文词 |
/// | 候选含空格 | `New York` | 大写整句没意义 |
/// | 码与候选（去掉标点后）不一致 | `PS` → `Photoshop` | 这是缩写，不是大小写问题 |
///
/// 最后一条同时放行**补全**候选（`cand.type == "completion"`）——
/// 补全出来的词本来就可能与码不逐字相等。
pub struct AutoCapFilter;

impl Filter for AutoCapFilter {
    fn apply(&self, q: &Query<'_>, _span: Span, cands: &mut Vec<Candidate>) {
        let code = q.input;
        let code_len = code.chars().count();
        // 不转换：码长为 1，或首位是小写/标点。
        if code_len == 1 {
            return;
        }
        let Some(first) = code.chars().next() else {
            return;
        };
        if !first.is_ascii_uppercase() {
            return;
        }
        let upper_count = code.chars().take_while(char::is_ascii_uppercase).count();
        let all_upper = upper_count >= 2;
        let pure_code: String = strip_punct(code).to_lowercase();

        for c in cands.iter_mut() {
            let text = c.text.clone();
            // 候选含非 字母/数字/标点/空格 的字符（中文、emoji…）→ 不动。
            if text
                .chars()
                .any(|ch| !(ch.is_alphanumeric() || ch.is_ascii_punctuation() || ch == ' '))
            {
                continue;
            }
            // 候选含空格 → 不动。
            if text.contains(' ') {
                continue;
            }
            let pure_text = strip_punct(&text);
            let pure_lower = pure_text.to_lowercase();
            // ① 候选**就是**用户打的码（去掉标点后，**区分大小写**）→ 不动。
            //
            //    区分大小写这一点是实测出来的：`Hello` 打出来时候选里
            //    本来就有 `hello`，而 `hello` **不等于** `Hello`，
            //    所以它要被转换（这正是自动大写的用处）。
            //    若这里写成"不分大小写地相等"，`Hello` 就会被跳过——
            //    整个功能都不工作，而测试会告诉你。
            if pure_text.starts_with(code) {
                continue;
            }
            // ② 码不是候选的前缀（**不分大小写**）→ 这是**缩写**
            //    （`PS` → `Photoshop`），不是大小写问题，不该动。
            //    补全候选例外：它本来就不逐字相等。
            if c.kind != CandidateKind::Completion && !pure_lower.starts_with(&pure_code) {
                continue;
            }
            if all_upper {
                c.text = text.to_uppercase();
            } else {
                // 只把**首字母**大写。
                let mut out = String::with_capacity(text.len());
                let mut done = false;
                for ch in text.chars() {
                    if !done && ch.is_alphabetic() {
                        out.extend(ch.to_uppercase());
                        done = true;
                    } else {
                        out.push(ch);
                    }
                }
                c.text = out;
            }
        }
    }
}

/// 去掉标点与空格（`autocap_filter` 用它比较"码"与"候选"）。
#[must_use]
pub fn strip_punct(s: &str) -> String {
    s.chars()
        .filter(|c| !(c.is_ascii_punctuation() || *c == ' '))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{AutoCapSpec, DateSpec, LongWordSpec, UnicodeSpec};
    use stele_core::{Context, FrozenClock, Lane, Options, SpellingAttr};

    fn cand(text: &str) -> Candidate {
        Candidate {
            text: text.to_owned(),
            comment: None,
            score: Score::ZERO,
            origin: Origin::SystemWord,
            attr: SpellingAttr::NORMAL,
            span: Span::new(0, 1),
            lane: Lane::Input,
            kind: CandidateKind::Normal,
        }
    }

    fn q<'a>(input: &'a str, opts: &'a Options, ctx: &'a Context) -> Query<'a> {
        Query {
            input,
            caret: input.len(),
            options: opts,
            context: ctx,
            segment_text: input,
        }
    }

    // ── 日历基础 ──

    #[test]
    fn civil_conversion_is_exact_on_known_dates() {
        // 1970-01-01 是星期四。
        let t = civil_from_unix(0, 0);
        assert_eq!((t.year, t.month, t.day, t.weekday), (1970, 1, 1, 4));
        // 2000-02-29（闰日，世纪闰年规则）。
        let t = civil_from_unix(951_782_400, 0);
        assert_eq!((t.year, t.month, t.day), (2000, 2, 29));
        // 2026-11-29 是星期日。
        let t = civil_from_unix(1_795_910_400, 0);
        assert_eq!((t.year, t.month, t.day), (2026, 11, 29));
        assert_eq!(t.weekday, 0, "2026-11-29 是星期日");
        // 2100 不是闰年（能被 100 整除但不能被 400 整除）。
        let t = civil_from_unix(4_107_542_400, 0);
        assert_eq!((t.year, t.month, t.day), (2100, 3, 1));
    }

    #[test]
    fn timezone_offset_changes_the_civil_date() {
        // 同一个 UTC 时刻，东八区已经是第二天了。
        // 2026-11-29 23:30 UTC = 2026-11-30 07:30 +08:00
        let secs = 1_795_996_391;
        let utc = civil_from_unix(secs, 0);
        let cst = civil_from_unix(secs, 8 * 3600);
        assert_eq!((utc.month, utc.day), (11, 29));
        assert_eq!((cst.month, cst.day, cst.hour), (11, 30, 7), "+08:00 下已经是第二天");
    }

    #[test]
    fn chinese_year_is_digit_by_digit() {
        assert_eq!(year_zh(2026), "二〇二六");
        assert_eq!(month_day_zh(11, 29), "十一月二十九日");
        assert_eq!(month_day_zh(1, 1), "一月一日");
        assert_eq!(month_day_zh(12, 31), "十二月三十一日");
    }

    // ── date_translator ──

    fn date_translator() -> DateTranslator {
        // 2026-11-30 07:53:11 +08:00 == 2026-11-29 23:53:11 UTC
        let clock = std::sync::Arc::new(FrozenClock {
            secs: 1_795_996_391,
            ms: 0,
            offset_secs: 8 * 3600,
        });
        let mut tags = crate::tag::TagTable::new();
        DateTranslator::new(
            clock,
            DateSpec::default(),
            vec![tags.intern("date")],
        )
    }

    #[test]
    fn date_formats_match_the_documented_shapes() {
        let t = date_translator();
        let render = |input: &str| {
            t.render(input)
                .into_iter()
                .map(|(a, _)| a)
                .collect::<Vec<_>>()
        };
        assert_eq!(render("rq"), ["2026-11-30"]);
        assert_eq!(render("sj"), ["07:53"]);
        assert_eq!(render("xq"), ["星期一"]);
        assert_eq!(render("ts"), ["1795996391"]);
        assert_eq!(render("rqzh"), ["二〇二六年十一月三十日"]);
        assert_eq!(render("rqen"), ["November 30, 2026"]);
        assert_eq!(render("dt"), ["2026-11-30T07:53:11+08:00"]);
        // 不认识的输入不产出候选。
        assert!(render("nihao").is_empty());
    }

    // ── unicode ──

    fn unicode_translator() -> UnicodeTranslator {
        let mut tags = crate::tag::TagTable::new();
        UnicodeTranslator::new(&UnicodeSpec::default(), vec![tags.intern("unicode")])
    }

    #[test]
    fn unicode_decodes_a_codepoint() {
        let t = unicode_translator();
        let out = t.render("U62fc");
        assert_eq!(out[0].0, "拼");
        assert_eq!(out[0].1, "U62fc");
        // BMP 会带出同一起始的后续码位。
        assert!(out.len() > 1, "BMP 码位应当带出后续 15 个");
    }

    #[test]
    fn unicode_rejects_short_and_out_of_range_input() {
        let t = unicode_translator();
        assert!(t.render("U6").is_empty(), "一位十六进制太容易误触发");
        assert!(t.render("62fc").is_empty(), "没有前缀");
        assert!(t.render("Uzzzz").is_empty(), "不是十六进制");
        // 超出 U+10FFFF。
        let over = t.render("U110000");
        assert_eq!(over.len(), 1);
        assert_eq!(over[0].0, "数值超限！");
        // 边界之内最后一个。
        assert_eq!(t.render("U10FFFF")[0].0, "\u{10FFFF}");
    }

    // ── long_word_filter ──

    #[test]
    fn long_word_filter_promotes_longer_words_from_idx() {
        // `idx: 4` 的语义是"**前 3 个位置不动**"——这是 rime-ice 的默认值，
        // 也是它注释里那句示例（「1接 2解 3姐 4饥饿」）的意思。
        let f = LongWordFilter::new(&LongWordSpec {
            count: 1,
            idx: 4,
            ..Default::default()
        });
        let opts = Options::new();
        let ctx = Context::default();
        let mut v = vec![
            cand("接"),   // 1 不动
            cand("解"),   // 2 不动
            cand("姐"),   // 3 不动
            cand("饥饿"), // 4 比首候选长 → 提升到第 4 位
            cand("结"),   // 5
            cand("界"),   // 6
        ];
        f.apply(&q("jie", &opts, &ctx), Span::new(0, 3), &mut v);
        let texts: Vec<&str> = v.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["接", "解", "姐", "饥饿", "结", "界"]);
    }

    #[test]
    fn long_word_filter_respects_count() {
        // `count: 1` 时只提升一个，第二个长词留在原地。
        let f = LongWordFilter::new(&LongWordSpec {
            count: 1,
            idx: 2,
            ..Default::default()
        });
        let opts = Options::new();
        let ctx = Context::default();
        let mut v = vec![cand("先"), cand("县"), cand("西安"), cand("西域")];
        f.apply(&q("xian", &opts, &ctx), Span::new(0, 4), &mut v);
        let texts: Vec<&str> = v.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["先", "西安", "县", "西域"]);
    }

    #[test]
    fn long_word_filter_does_not_promote_alphanumeric() {
        let f = LongWordFilter::new(&LongWordSpec {
            count: 2,
            idx: 2,
            ..Default::default()
        });
        let opts = Options::new();
        let ctx = Context::default();
        let mut v = vec![cand("先"), cand("xian"), cand("西安")];
        f.apply(&q("xian", &opts, &ctx), Span::new(0, 4), &mut v);
        let texts: Vec<&str> = v.iter().map(|c| c.text.as_str()).collect();
        // 英文候选（含字母）**不参与提升**，但西安（更长且无字母数字）要提。
        assert_eq!(texts, ["先", "西安", "xian"]);
    }

    // ── autocap_filter ──

    #[test]
    fn autocap_uppercases_first_letter_for_one_capital() {
        let f = AutoCapFilter;
        let opts = Options::new();
        let ctx = Context::default();
        let mut v = vec![cand("hello"), cand("hello world"), cand("你好")];
        f.apply(&q("Hello", &opts, &ctx), Span::new(0, 5), &mut v);
        assert_eq!(v[0].text, "Hello", "首位大写 → 候选首位大写");
        // 含空格的与含中文的一律不动。
        assert_eq!(v[1].text, "hello world");
        assert_eq!(v[2].text, "你好");
    }

    #[test]
    fn autocap_uppercases_all_for_two_leading_capitals() {
        let f = AutoCapFilter;
        let opts = Options::new();
        let ctx = Context::default();
        let mut v = vec![cand("hello")];
        f.apply(&q("HEllo", &opts, &ctx), Span::new(0, 5), &mut v);
        assert_eq!(v[0].text, "HELLO");
    }

    #[test]
    fn autocap_leaves_lowercase_input_alone() {
        let f = AutoCapFilter;
        let opts = Options::new();
        let ctx = Context::default();
        let mut v = vec![cand("hello")];
        f.apply(&q("hello", &opts, &ctx), Span::new(0, 5), &mut v);
        assert_eq!(v[0].text, "hello");
        // 码长为 1 也不转换。
        let mut v2 = vec![cand("a")];
        f.apply(&q("A", &opts, &ctx), Span::new(0, 1), &mut v2);
        assert_eq!(v2[0].text, "a");
    }

    #[test]
    fn autocap_skips_abbreviations_but_not_completions() {
        let f = AutoCapFilter;
        let opts = Options::new();
        let ctx = Context::default();
        // `PS` → `Photoshop`：码与候选不一致，是缩写，不改。
        let mut v = vec![cand("Photoshop")];
        f.apply(&q("PS", &opts, &ctx), Span::new(0, 2), &mut v);
        assert_eq!(v[0].text, "Photoshop");
        // 但补全候选允许不一致（它本来就不逐字相等）。
        let mut v2 = vec![Candidate {
            kind: CandidateKind::Completion,
            ..cand("Photoshop")
        }];
        f.apply(&q("PHO", &opts, &ctx), Span::new(0, 3), &mut v2);
        assert_eq!(v2[0].text, "PHOTOSHOP");
    }

    // ── uuid ──

    fn uuid_translator() -> UuidTranslator {
        let mut tags = crate::tag::TagTable::new();
        UuidTranslator::new(
            Box::new(stele_core::DeterministicRandom::new(42)),
            &crate::spec::UuidSpec::default(),
            vec![tags.intern("uuid")],
        )
    }

    #[test]
    fn uuid_has_the_v4_shape_and_version_bits() {
        let t = uuid_translator();
        let id = t.generate();
        // 8-4-4-4-12，36 个字符。
        assert_eq!(id.len(), 36);
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert!(id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
        // 版本位 = 4（v4），变体位 ∈ {8,9,a,b}。
        assert_eq!(&id[14..15], "4", "版本位必须是 4：{id}");
        assert!(
            ['8', '9', 'a', 'b'].contains(&id.chars().nth(19).unwrap()),
            "变体位必须是 8/9/a/b：{id}"
        );
    }

    #[test]
    fn uuid_is_deterministic_for_a_fixed_seed() {
        // 这一条是**注入随机源的全部意义**：同一个种子必须给出同一条 UUID，
        // 否则"敲 uuid 得到什么"就无法被断言。
        let a = uuid_translator().generate();
        let b = uuid_translator().generate();
        assert_eq!(a, b);
        // 换一个种子就不同（不是常数）。
        let mut tags = crate::tag::TagTable::new();
        let other = UuidTranslator::new(
            Box::new(stele_core::DeterministicRandom::new(7)),
            &crate::spec::UuidSpec::default(),
            vec![tags.intern("uuid")],
        );
        assert_ne!(a, other.generate());
    }

    #[test]
    fn uuid_only_fires_on_its_trigger() {
        let t = uuid_translator();
        let opts = Options::new();
        let ctx = Context::default();
        let mut buf = Vec::new();
        let mut sink = CandidateSink::new(&mut buf, 4);
        t.translate(&q("uuid", &opts, &ctx), Span::new(0, 4), &mut sink);
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].kind, CandidateKind::Inline);

        let mut buf2 = Vec::new();
        let mut sink2 = CandidateSink::new(&mut buf2, 4);
        t.translate(&q("uu", &opts, &ctx), Span::new(0, 2), &mut sink2);
        assert!(buf2.is_empty(), "只有触发词才产出");
    }

    // ── v_filter ──

    #[test]
    fn v_filter_moves_single_characters_first() {
        let f = VFilter::default();
        let opts = Options::new();
        let ctx = Context::default();
        let mut v = vec![cand("van"), cand("ā"), cand("vain"), cand("á")];
        f.apply(&q("va", &opts, &ctx), Span::new(0, 2), &mut v);
        let texts: Vec<&str> = v.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["ā", "á", "van", "vain"]);
    }

    #[test]
    fn v_filter_exceptions_keep_their_relative_order() {
        // 例外表（`1️⃣`、`Vs.`）的作用是"**也**归到前面那一组"，
        // 而不是"提到最前"。两者差别很实际：
        //
        // - 数字键帽 `1️⃣` 是 3 个码位（`1` + 变体选择符 + 组合键帽），
        //   所以它**不满足**"单字符"那条判据，只能靠例外表进前组；
        // - 进了前组之后，它**保持原来的相对顺序**——这正是 rime-ice
        //   那边 `yield(cand)` 直接产出的行为（它没有排序，只是分流）。
        //
        // 我第一版把这条测试写成"例外提到最前"，测试当场指出是错的。
        // 而这种"我以为语义更强"的偏差，正是不该靠推理、要靠对齐的地方。
        let f = VFilter::default();
        let opts = Options::new();
        let ctx = Context::default();
        let mut v = vec![cand("van"), cand("ā"), cand("1️⃣")];
        f.apply(&q("v1", &opts, &ctx), Span::new(0, 2), &mut v);
        let texts: Vec<&str> = v.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["ā", "1️⃣", "van"]);
    }

    #[test]
    fn v_filter_does_nothing_outside_v_mode() {
        let f = VFilter::default();
        let opts = Options::new();
        let ctx = Context::default();
        let mut v = vec![cand("van"), cand("ā")];
        f.apply(&q("vab", &opts, &ctx), Span::new(0, 3), &mut v);
        assert_eq!(v[0].text, "van", "长度不是 2 就不动");
        let mut v2 = vec![cand("van"), cand("ā")];
        f.apply(&q("ha", &opts, &ctx), Span::new(0, 2), &mut v2);
        assert_eq!(v2[0].text, "van", "不以 v 开头就不动");
    }

    // ── pin_cand_filter ──

    fn pin_entry(preedit: &str, texts: &[&str]) -> crate::spec::PinEntry {
        crate::spec::PinEntry {
            preedit: preedit.to_owned(),
            texts: texts.iter().map(|s| (*s).to_owned()).collect(),
            at: crate::spec::At::default(),
        }
    }

    fn pin_filter(entries: Vec<crate::spec::PinEntry>) -> PinCandFilter {
        PinCandFilter::new(&crate::spec::PinCandSpec {
            entries,
            at: crate::spec::At::default(),
        })
    }

    #[test]
    fn pin_table_derives_the_shorthand_keys() {
        let f = pin_filter(vec![
            pin_entry("ni hao", &["你好"]),
            pin_entry("zhi chi", &["支持"]),
        ]);
        let t = f.table();
        // 原样去空格。
        assert_eq!(t.get("nihao"), Some(&["你好".to_owned()][..]));
        // 最后一个音节的首字母。
        assert_eq!(t.get("nih"), Some(&["你好".to_owned()][..]));
        // zh/ch/sh 再加一个两字母简码。
        assert_eq!(t.get("zhichi"), Some(&["支持".to_owned()][..]));
        assert_eq!(t.get("zhic"), Some(&["支持".to_owned()][..]));
        assert_eq!(t.get("zhich"), Some(&["支持".to_owned()][..]));
    }

    #[test]
    fn an_explicit_shorthand_beats_a_derived_one() {
        // rime-ice 文档专门举的例子：`da z` 显式声明了 `daz`，
        // 于是 `daz` 归「打字」，而 `dazh` 仍归「大专」。
        let f = pin_filter(vec![
            pin_entry("da zhuan", &["大专"]),
            pin_entry("da zhong", &["大众"]),
            pin_entry("da z", &["打字"]),
        ]);
        let t = f.table();
        assert_eq!(t.get("daz"), Some(&["打字".to_owned()][..]), "显式的赢");
        assert_eq!(
            t.get("dazh"),
            Some(&["大专".to_owned(), "大众".to_owned()][..]),
            "没显式声明的按声明顺序合并"
        );
        assert_eq!(t.get("dazhuan"), Some(&["大专".to_owned()][..]));
    }

    #[test]
    fn pin_reorders_to_the_configured_order() {
        let f = pin_filter(vec![pin_entry("hao", &["号", "好"])]);
        let opts = Options::new();
        let ctx = Context::default();
        let mut v = vec![cand("好"), cand("毫"), cand("号"), cand("哈")];
        f.apply(&q("hao", &opts, &ctx), Span::new(0, 3), &mut v);
        let texts: Vec<&str> = v.iter().map(|c| c.text.as_str()).collect();
        // 「号」在配置里排在「好」之前，所以它先出现。
        assert_eq!(texts, ["号", "好", "毫", "哈"]);
    }

    #[test]
    fn pin_falls_back_to_a_shorter_prefix_of_the_input() {
        // 用户敲 `dian`，而配置写的是 `dian`；但候选的「编码」可能是 `di`。
        // 回退让 `dian` 的规则在 `di` 这一步就命中。
        let f = pin_filter(vec![pin_entry("dian", &["点"])]);
        let opts = Options::new();
        let ctx = Context::default();
        let mut v = vec![cand("地"), cand("点"), cand("第")];
        f.apply(&q("dian", &opts, &ctx), Span::new(0, 4), &mut v);
        assert_eq!(v[0].text, "点");
    }

    #[test]
    fn pin_does_nothing_when_the_code_has_no_rule() {
        let f = pin_filter(vec![pin_entry("hao", &["号"])]);
        let opts = Options::new();
        let ctx = Context::default();
        let mut v = vec![cand("好"), cand("毫")];
        f.apply(&q("zzz", &opts, &ctx), Span::new(0, 3), &mut v);
        let texts: Vec<&str> = v.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["好", "毫"], "没有规则就原样通过");
    }

    #[test]
    fn pin_keeps_candidates_it_could_not_find() {
        // 配置里要置顶的词**不在候选里**时，其余候选不能丢。
        // 这条守着一个很容易写错的地方：`pined` 里的空位必须被跳过。
        let f = pin_filter(vec![pin_entry("hao", &["库里没有的词", "好"])]);
        let opts = Options::new();
        let ctx = Context::default();
        let mut v = vec![cand("毫"), cand("好"), cand("哈")];
        f.apply(&q("hao", &opts, &ctx), Span::new(0, 3), &mut v);
        let texts: Vec<&str> = v.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["好", "毫", "哈"]);
    }

    #[test]
    fn pin_supports_the_angle_bracket_separator() {
        // `'l 了 > 啦'` —— 词本身含空格时用 ` > ` 分隔。
        let f = pin_filter(vec![pin_entry("l", &["了 > 啦"])]);
        assert_eq!(
            f.table().get("l"),
            Some(&["了".to_owned(), "啦".to_owned()][..])
        );
    }

    #[test]
    fn derived_keys_need_at_least_two_units() {
        assert!(derived_keys("hao").is_empty(), "单个编码单元没有简码可派生");
        assert_eq!(derived_keys("ni hao"), ["nih"]);
        assert_eq!(derived_keys("bu hao chi"), ["buhaoc", "buhaoch"]);
    }

    // ── reduce_english_filter ──

    fn reduce_filter(
        mode: crate::spec::ReduceMode,
        idx: usize,
        words: &[&str],
    ) -> ReduceEnglishFilter {
        ReduceEnglishFilter::new(&crate::spec::ReduceEnglishSpec {
            mode,
            idx,
            words: words.iter().map(|s| (*s).to_owned()).collect(),
            at: crate::spec::At::default(),
        })
    }

    #[test]
    fn reduce_english_pushes_english_words_down() {
        let f = reduce_filter(crate::spec::ReduceMode::All, 2, &[]);
        let opts = Options::new();
        let ctx = Context::default();
        // 敲 `rug`：英文 rug 在首位，中文「如果」在第二位。
        let mut v = vec![cand("rug"), cand("如果"), cand("如")];
        f.apply(&q("rug", &opts, &ctx), Span::new(0, 3), &mut v);
        let texts: Vec<&str> = v.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["如果", "rug", "如"], "英文降到第 2 位");
    }

    #[test]
    fn reduce_english_only_fires_on_listed_codes() {
        let f = reduce_filter(crate::spec::ReduceMode::Custom, 2, &["rug"]);
        let opts = Options::new();
        let ctx = Context::default();
        // `abc` 不在表里 → 不动。
        let mut v = vec![cand("abc"), cand("啊")];
        f.apply(&q("abc", &opts, &ctx), Span::new(0, 3), &mut v);
        assert_eq!(v[0].text, "abc");
        // `rug` 在表里 → 降。
        let mut v2 = vec![cand("rug"), cand("如果")];
        f.apply(&q("rug", &opts, &ctx), Span::new(0, 3), &mut v2);
        assert_eq!(v2[0].text, "如果");
    }

    #[test]
    fn reduce_english_mode_none_never_fires() {
        let f = reduce_filter(crate::spec::ReduceMode::None, 2, &["rug"]);
        assert!(!f.triggers("rug"));
    }

    #[test]
    fn reduce_english_mode_all_merges_builtin_and_custom() {
        let f = reduce_filter(crate::spec::ReduceMode::All, 2, &["zzzz"]);
        assert!(f.triggers("rug"), "内置表里的");
        assert!(f.triggers("zzzz"), "自定义的");
        assert!(!f.triggers("qqqq"), "都不在");
    }

    #[test]
    fn reduce_english_never_demotes_user_words() {
        // 用户自己打过并确认过的词不该被启发式压下去。
        let f = reduce_filter(crate::spec::ReduceMode::All, 2, &[]);
        let opts = Options::new();
        let ctx = Context::default();
        let mut v = vec![
            Candidate {
                kind: CandidateKind::UserTable,
                ..cand("rug")
            },
            cand("如果"),
        ];
        f.apply(&q("rug", &opts, &ctx), Span::new(0, 3), &mut v);
        assert_eq!(v[0].text, "rug", "用户词不动");
    }

    #[test]
    fn reduce_english_keeps_phrases_and_chinese_in_place() {
        let f = reduce_filter(crate::spec::ReduceMode::All, 3, &[]);
        let opts = Options::new();
        let ctx = Context::default();
        // 含空格、含非 ASCII、纯数字的非英文候选都不降。
        let mut v = vec![cand("New York"), cand("你"), cand("123"), cand("rug")];
        f.apply(&q("rug", &opts, &ctx), Span::new(0, 3), &mut v);
        let texts: Vec<&str> = v.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["New York", "你", "123", "rug"]);
    }

    #[test]
    fn is_english_word_draws_the_line_the_same_way_rime_ice_does() {
        assert!(is_english_word("rug"));
        assert!(is_english_word("Mac"));
        assert!(!is_english_word("New York"), "含空格");
        assert!(!is_english_word("你"), "非 ASCII");
        assert!(!is_english_word("123"), "没有字母");
        assert!(!is_english_word(""), "空串");
    }

    #[test]
    fn the_autocap_spec_exists_so_the_component_can_be_declared() {
        // 它没有配置项，但仍然需要一个 spec——否则方案里没法"声明"它。
        let s = AutoCapSpec::default();
        assert_eq!(s.at.line, 0);
    }
}
