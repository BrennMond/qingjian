//! # Punctuation
//!
//! 中文职责：标点的直出、全半角、以及"符号表"（`/` 或 `v` 开头的那张表）。
//! English role: punctuation output, full/half shape, and the symbol table.
//! 架构位置：`stele-core` 的 `Processor` / `Translator` 的实现。
//!
//! # 标点为什么不是"一个翻译器"就够
//!
//! 真实输入法里标点要同时满足四件事，而它们是**四个不同环节**：
//!
//! | 需求 | 谁做 | 为什么不能合并 |
//! | --- | --- | --- |
//! | 标点键**进得来**（不被输入处理器吞掉） | [`Punctuator`]（处理器） | 输入处理器只管"是不是编码字符" |
//! | 标点独占**一段**（不抢正常编码） | `SymbolSegmentor`（切分器） | 切分发生在翻译之前 |
//! | 标点**变成文本**（`,` → `，`） | [`PunctTranslator`]（翻译器） | 映射是方案数据，翻译器才拿得到 |
//! | **全角/半角**随时切换 | 处理器读开关 | 开关是会话状态，翻译器只读 |
//!
//! 把它塞进一个零件，就会变成 RIME 那种"零件之间靠隐式约定协作"的样子；
//! 拆成四个，每一块的契约都能单独测。
//!
//! # 与 RIME 的一处有意不同
//!
//! RIME 的 `punctuator` 是**处理器**：它直接上屏标点，**不经过候选列表**。
//! 我们让标点走候选列表，理由是候选列表才是"用户可以改主意"的地方——
//! 而且**状态栏与候选窗口的行为于是统一了**：任何上屏都从列表来。
//!
//! 代价是标点上屏多了一次翻译。按键 P50 是 301 ns，这条开销在噪声里。

use std::collections::BTreeMap;
use stele_core::{
    Candidate, CandidateSink, Key, KeyCode, Lane, Modifiers, Origin, PendingCommit, ProcessResult,
    Query, Score, SessionState, Span, SpellingAttr, Translator, Trigger,
};

use crate::spec::PunctuatorSpec;

/// **标点处理器**：把标点键放进输入串，并处理符号表前缀。
///
/// # 它为什么必须"看情况"
///
/// 标点键与编码键是**冲突**的（`.` 既可能是标点，也可能是方案字母表里的
/// 一个编码字符）。规则是：
///
/// - 输入串**为空**时：标点直接进入输入串（自成一段，由切分器标成标点段）。
/// - 输入串**已经是一段标点**时：继续追加（支持 `……`、`——` 这类多字符标点）。
/// - 输入串是**编码**时：**不管**，把机会留给输入处理器——
///   否则 `ni.` 里的 `.` 会被标点吃掉，而用户想要的可能是别的。
pub struct Punctuator {
    /// 半角映射（原样 → 上屏文本）。
    half: BTreeMap<char, String>,
    /// 全角映射。
    full: BTreeMap<char, String>,
    /// 谁是"标点字符"（两张表的键的并集）。
    punct_chars: Vec<char>,
    /// 符号表前缀（`/` 或 `v`）。
    symbol_prefix: Option<char>,
    /// 全角开关名。
    full_shape_option: Option<String>,
}

impl Punctuator {
    /// 由配置构造。
    ///
    /// `full_shape_option` 是方案里那个"全角/半角"开关的名字。
    /// 引擎不知道它叫什么——**名字是方案数据**（D20）。
    #[must_use]
    pub fn new(spec: &PunctuatorSpec, full_shape_option: Option<String>) -> Self {
        let mut half = BTreeMap::new();
        let mut full = BTreeMap::new();
        for (k, v) in &spec.half_shape {
            collect(&mut half, k, v);
        }
        for (k, v) in &spec.full_shape {
            collect(&mut full, k, v);
        }
        let mut punct_chars: Vec<char> =
            half.keys().chain(full.keys()).copied().collect::<Vec<_>>();
        punct_chars.sort_unstable();
        punct_chars.dedup();
        Self {
            half,
            full,
            punct_chars,
            symbol_prefix: spec.symbol_prefix,
            full_shape_option,
        }
    }

    /// 这个字符是不是标点键。
    #[must_use]
    pub fn is_punct(&self, c: char) -> bool {
        self.punct_chars.contains(&c) || self.symbol_prefix == Some(c)
    }

    /// 当前应当使用的映射表（受全角开关控制）。
    fn table(&self, state: &SessionState) -> &BTreeMap<char, String> {
        let full_on = self
            .full_shape_option
            .as_deref()
            .is_some_and(|n| state.options.get(n));
        if full_on {
            &self.full
        } else {
            &self.half
        }
    }
}

/// 把一条映射放进表里。键取**首字符**（RIME 的映射表键就是单个字符）。
fn collect(map: &mut BTreeMap<char, String>, key: &str, value: &str) {
    if let Some(c) = key.chars().next() {
        map.insert(c, value.to_owned());
    }
}

impl stele_core::Processor for Punctuator {
    fn name(&self) -> &'static str {
        "punctuator"
    }

    fn process(&mut self, state: &mut SessionState, key: &Key) -> ProcessResult {
        if key.release {
            return ProcessResult::Noop;
        }
        // 带 Ctrl / Alt / Super 的按键属于系统或别的处理器。
        let blocked = Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER;
        if !Modifiers::from_bits(key.mods.bits() & blocked.bits()).is_empty() {
            return ProcessResult::Noop;
        }
        let KeyCode::Char(c) = key.code else {
            return ProcessResult::Noop;
        };

        // ── 符号表：前缀字符开启它，后面的键继续追加 ──
        if let Some(p) = self.symbol_prefix {
            if c == p {
                // 输入串为空，或本来就以这个前缀开头时才接管。
                if state.composition.input.is_empty() || state.composition.input.starts_with(p) {
                    state.composition.input.push(c);
                    state.composition.caret = state.composition.input.len();
                    return ProcessResult::Accepted;
                }
                return ProcessResult::Noop;
            }
            // 已经在前缀模式里：把后续的字母数字收进来（`v1`、`vabc`）。
            if state.composition.input.starts_with(p)
                && (c.is_ascii_alphanumeric() || !self.is_punct(c))
            {
                state.composition.input.push(c);
                state.composition.caret = state.composition.input.len();
                return ProcessResult::Accepted;
            }
        }

        // ── 普通标点 ──
        if !self.is_punct(c) {
            return ProcessResult::Noop;
        }
        // 输入串为空，或已是一段纯标点 → 接管。
        let input = &state.composition.input;
        let take_over = input.is_empty() || input.chars().all(|x| self.is_punct(x));
        if !take_over {
            return ProcessResult::Noop;
        }
        // 映射：表里没有的键就原样放进去（半角表通常覆盖全部常用标点）。
        let mapped = self
            .table(state)
            .get(&c)
            .cloned()
            .unwrap_or_else(|| c.to_string());
        state.composition.input.push_str(&mapped);
        state.composition.caret = state.composition.input.len();
        ProcessResult::Accepted
    }
}

/// **标点翻译器**：把输入串里的标点变成候选。
///
/// 它**只对标点段生效**（tag 绑定）。有一个候选与多个候选的情形都覆盖：
/// 符号表里 `/hx` 给出"㊕㊙…"一串，就是多候选。
pub struct PunctTranslator {
    /// 半角映射。
    half: BTreeMap<String, String>,
    /// 全角映射。
    full: BTreeMap<String, String>,
    /// 符号表。**键已剥掉前缀**（`v1` → `1`），因为用户敲的是前缀之后的
    /// 那一串。
    symbols: BTreeMap<String, String>,
    /// 符号表前缀。
    symbol_prefix: Option<char>,
    /// 全角开关名。
    full_shape_option: Option<String>,
    /// 符号表的键（**已剥前缀**），有序（保证可复现——PLAN §5.2）。
    symbol_keys: Vec<String>,
}

impl PunctTranslator {
    /// 由配置构造。
    #[must_use]
    pub fn new(spec: &PunctuatorSpec, full_shape_option: Option<String>) -> Self {
        let half = spec
            .half_shape
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let full = spec
            .full_shape
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        // 键剥掉前缀：RIME 的方案写 `"v1": "①"`，而用户敲 `v1` 时
        // 前缀 `v` 已经在输入串里被认出来了，要查的是 `1`。
        let symbols: BTreeMap<String, String> = spec
            .symbols
            .iter()
            .map(|(k, v)| {
                let key = match spec.symbol_prefix {
                    Some(p) if k.starts_with(p) => k[p.len_utf8()..].to_owned(),
                    _ => k.clone(),
                };
                (key, v.clone())
            })
            .collect();
        let symbol_keys = symbols.keys().cloned().collect();
        Self {
            half,
            full,
            symbols,
            symbol_prefix: spec.symbol_prefix,
            full_shape_option,
            symbol_keys,
        }
    }

    /// 一次符号表查询最多产出多少候选（`v` 后面可能匹配几百条）。
    const SYMBOL_CAP: usize = 50;

    fn push(text: &str, span: Span, out: &mut CandidateSink<'_>, score_milli: i32) {
        out.push(Candidate {
            text: text.to_owned(),
            comment: None,
            score: Score::from_milli_log(score_milli),
            origin: Origin::Literal,
            attr: SpellingAttr::NORMAL,
            span,
            lane: Lane::Input,
            kind: stele_core::CandidateKind::Punct,
            key: None,
        });
    }
}

impl Translator for PunctTranslator {
    fn translate(&self, q: &Query<'_>, span: Span, out: &mut CandidateSink<'_>) {
        let text = q.segment_text;
        if text.is_empty() {
            return;
        }

        // ── 符号表：以前缀字符开头的一段 ──
        if let Some(p) = self.symbol_prefix {
            if text.starts_with(p) {
                let query = &text[p.len_utf8()..];
                let mut hits: Vec<&str> = Vec::new();
                // 精确命中排第一（`v1` → 「①」）。
                if !query.is_empty() {
                    if let Some(v) = self.symbols.get(query) {
                        hits.push(v.as_str());
                    }
                }
                // 再前缀命中（`vh` → 所有以 `h` 开头的符号条目）。
                for k in &self.symbol_keys {
                    if k == query || query.is_empty() {
                        continue;
                    }
                    if k.starts_with(query) {
                        if let Some(v) = self.symbols.get(k) {
                            hits.push(v.as_str());
                        }
                    }
                    if hits.len() >= Self::SYMBOL_CAP {
                        break;
                    }
                }
                for (i, h) in hits.iter().enumerate() {
                    // 精确命中排第一，其余按表序。
                    let step = i32::try_from(i).unwrap_or(i32::MAX / 20);
                    let score = Score::from_milli_log(6_000 - step * 10);
                    out.push(Candidate {
                        text: (*h).to_owned(),
                        comment: None,
                        score,
                        origin: Origin::Literal,
                        attr: SpellingAttr::NORMAL,
                        span,
                        lane: Lane::Input,
                        kind: stele_core::CandidateKind::Punct,
                        key: None,
                    });
                }
                return;
            }
        }

        // ── 普通标点：整段在映射表里查一次 ──
        let full_on = self
            .full_shape_option
            .as_deref()
            .is_some_and(|n| q.options.get(n));
        let table = if full_on { &self.full } else { &self.half };
        if let Some(v) = table.get(text) {
            Self::push(v, span, out, 7_000);
            return;
        }
        // 表里没有：原样上屏（半角原字符），保证"敲什么都有东西"。
        // 这一条很重要——`digit_separators` 之类的映射表可能不全。
        if text.chars().count() == 1 {
            Self::push(text, span, out, 6_900);
        }
    }

    fn accepts(&self, tags: &[stele_core::Tag]) -> bool {
        tags.contains(&"punct")
    }

    fn targets(&self) -> &[stele_core::Tag] {
        // `punct` 是引擎内置的标签名（RIME 也用同一个词）。
        const PUNCT: &[stele_core::Tag] = &["punct"];
        PUNCT
    }
}

/// 一段文本直接上屏的意图（供别处复用）。
#[must_use]
pub fn literal_pending(text: impl Into<String>) -> PendingCommit {
    PendingCommit::literal(text, Trigger::Punctuation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stele_core::{Key, Options, Processor};

    fn spec() -> PunctuatorSpec {
        PunctuatorSpec {
            half_shape: vec![
                (",".into(), "，".into()),
                (".".into(), "。".into()),
                ("!".into(), "！".into()),
            ],
            full_shape: vec![",".into()]
                .into_iter()
                .map(|k| (k, "，".into()))
                .collect(),
            symbols: vec![
                ("1".into(), "①".into()),
                ("2".into(), "②".into()),
                ("hx".into(), "㊕".into()),
            ],
            symbol_prefix: Some('v'),
            at: crate::spec::At::new(1),
        }
    }

    fn state_with(option: Option<(&str, bool)>) -> SessionState {
        let mut s = SessionState::default();
        if let Some((n, on)) = option {
            s.options.declare(stele_core::Switch::new(n, on));
        }
        s
    }

    #[test]
    fn punctuation_enters_the_input_when_nothing_is_composing() {
        let mut p = Punctuator::new(&spec(), None);
        let mut s = state_with(None);
        assert_eq!(p.process(&mut s, &Key::ch(',')), ProcessResult::Accepted);
        assert_eq!(s.composition.input, "，");
    }

    #[test]
    fn punctuation_does_not_steal_a_key_while_composing() {
        // `ni` 正在输入时敲 `.` —— 标点处理器**必须让路**，
        // 否则用户想敲的编码字符会被换成标点。
        let mut p = Punctuator::new(&spec(), None);
        let mut s = state_with(None);
        s.composition.input = "ni".into();
        assert_eq!(p.process(&mut s, &Key::ch('.')), ProcessResult::Noop);
        assert_eq!(s.composition.input, "ni");
    }

    #[test]
    fn full_shape_switch_selects_the_table() {
        let mut p = Punctuator::new(&spec(), Some("full_shape".into()));
        let mut off = state_with(Some(("full_shape", false)));
        p.process(&mut off, &Key::ch(','));
        assert_eq!(off.composition.input, "，", "半角表的映射");

        let mut on = state_with(Some(("full_shape", true)));
        p.process(&mut on, &Key::ch(','));
        assert_eq!(on.composition.input, "，");
    }

    #[test]
    fn symbol_prefix_opens_the_table_and_keeps_appending() {
        let mut p = Punctuator::new(&spec(), None);
        let mut s = state_with(None);
        assert_eq!(p.process(&mut s, &Key::ch('v')), ProcessResult::Accepted);
        assert_eq!(p.process(&mut s, &Key::ch('1')), ProcessResult::Accepted);
        assert_eq!(s.composition.input, "v1");
    }

    #[test]
    fn translator_maps_a_single_symbol() {
        let t = PunctTranslator::new(&spec(), None);
        let opts = Options::new();
        let ctx = stele_core::Context::default();
        let q = Query {
            input: "，",
            caret: 3,
            options: &opts,
            context: &ctx,
            segment_text: "，",
        };
        let mut buf = Vec::new();
        let mut sink = CandidateSink::new(&mut buf, 16);
        t.translate(&q, Span::new(0, 3), &mut sink);
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].text, "，");
        assert_eq!(buf[0].origin, Origin::Literal);
        assert!(
            stele_core::is_exact(buf[0].origin, buf[0].attr),
            "标点是原样上屏，不是猜的"
        );
    }

    #[test]
    fn translator_expands_the_symbol_table() {
        let t = PunctTranslator::new(&spec(), None);
        let opts = Options::new();
        let ctx = stele_core::Context::default();
        let q = Query {
            input: "v1",
            caret: 2,
            options: &opts,
            context: &ctx,
            segment_text: "v1",
        };
        let mut buf = Vec::new();
        let mut sink = CandidateSink::new(&mut buf, 16);
        t.translate(&q, Span::new(0, 2), &mut sink);
        assert_eq!(buf[0].text, "①", "精确命中排第一");
    }
}
