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

    #[test]
    fn the_autocap_spec_exists_so_the_component_can_be_declared() {
        // 它没有配置项，但仍然需要一个 spec——否则方案里没法"声明"它。
        let s = AutoCapSpec::default();
        assert_eq!(s.at.line, 0);
    }
}
