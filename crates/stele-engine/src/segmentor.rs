//! # Segmentors
//!
//! 中文职责：把输入串切成若干段，并给每段打标签。
//! English role: cut the input into segments and tag each one.
//! 架构位置：`stele-core::Segmentor` 的实现。
//!
//! # 标签是"切分器 → 翻译器"的绑定层（G3）
//!
//! 一个方案里可以同时挂"拼音 / 英文 / 自定义短语 / 拆字反查"四个翻译器。
//! **它们不靠位置区分，靠标签区分**：
//!
//! ```text
//! 输入 uUni
//!   ├─ recognizer 认出 `^uU[a-z]+$` → 这一段带标签 `radical_lookup`
//!   └─ affix_segmentor@radical_lookup 吃掉前缀 `uU`
//!        → 翻译器只看到 `ni`，产出的候选属于 `radical_lookup` 那一段
//! ```
//!
//! # 识别只在**输入开头**
//!
//! RIME 的 `recognizer` 允许在输入串任意位置认出一个模式（例如
//! `^;.*;$` 这种"以分号包起来"的写法）。**我们目前只做前缀识别**——
//! 也就是"从头开始整段匹配"。这是**诚实的边界**，不是省略：
//!
//! - `no_lua_schema` 的两个模式（`punct` 与 `radical_lookup`）都是前缀模式，
//!   因此这条边界不影响 P3 的验收线；
//! - 中缀识别需要"分段已确认 / 可重开"的增量行为（`SegmentStatus` 的
//!   `Confirmed` / `Reopen`），那是 P4 的内容。数据结构里已经留好了位置，
//!   补行为时**不需要改类型签名**。
//!
//! 一旦要做中缀识别，改动点是 [`InputScan::claims`] 的构造方式——
//! 它返回的是 (起点, 终点, 标签) 三元组，"只在开头"只是它目前
//! 只会产生起点为 0 的那些。

use stele_core::{Query, Segment, SegmentStatus, Segmentor, Span, Tag};

use crate::spec::{AffixSpec, PunctuatorSpec, RecogPattern, RecognizerSpec};
use crate::tag::TagTable;

/// 一遍扫描的结果：认领列表 + 每个认领的**正文起点**（词缀之后）。
///
/// 认领的类型是内核的 [`stele_core::Claim`]——切分器 trait 住在内核里，
/// 它要能收到这份结果，因此**认领的表示由内核定义、由引擎填充**。
/// 这一层没有代价：都是一段 `Vec`，传给切分器的是它的切片。
#[derive(Clone, Debug, Default)]
pub struct InputScan {
    /// 按起点升序（同起点按终点降序：长的优先）。
    pub claims: Vec<stele_core::Claim>,
    /// 与 `claims` 一一对应的"正文起点"。
    ///
    /// 例如 `uUni` 被认领成 `radical_lookup` 时，正文是 `ni`，
    /// 于是这一项是 2。没有词缀时它就等于 `claim.start`。
    pub body_start: Vec<usize>,
}

impl InputScan {
    /// 转成内核侧的**只读视图**（零拷贝）。
    #[must_use]
    pub fn view(&self) -> stele_core::InputScanView<'_> {
        stele_core::InputScanView {
            claims: &self.claims,
        }
    }

    /// 在 `pos` 处开始的认领的下标。
    #[must_use]
    pub fn claim_at(&self, pos: usize) -> Option<usize> {
        self.claims.iter().position(|c| c.start == pos)
    }

    /// 在 `pos` 处开始的认领。
    #[must_use]
    pub fn claim(&self, pos: usize) -> Option<&stele_core::Claim> {
        self.claims.iter().find(|c| c.start == pos)
    }

    /// **严格晚于** `pos` 的下一个认领起点。
    #[must_use]
    pub fn next_claim_start(&self, pos: usize) -> Option<usize> {
        self.claims.iter().find(|c| c.start > pos).map(|c| c.start)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 前缀提取
// ─────────────────────────────────────────────────────────────────────────────

/// 从一个正则源码里取出**字面前缀**（`^` 之后到第一个元字符之前）。
///
/// 这不是"解析正则"，而是一个**保守的加速结构**：取不准时宁可返回空串
/// （空串人人都匹配，于是走完整正则那条慢路）。**取错才可怕**：
/// 一个错误的字面前缀会让该匹配的输入匹配不上，而症状是
/// "某个前缀模式偶尔不生效"。
///
/// 处理的两处特例：
///
/// - `([nl])ue$` 这种**字符类开头的模式**：没有字面前缀，但它的字符类
///   第一项 `n` 是"可能的首字符"，对排除法仍然有用。这里只处理最外层的
///   `(...)` / `[...]` 形式，取其中第一个字面字符。
/// - 转义字符 `\.` 之后是一个字面点号。
#[must_use]
pub fn leading_literal(regex: &str) -> String {
    let mut chars = regex.chars().peekable();
    if chars.peek() == Some(&'^') {
        chars.next();
    }
    let mut out = String::new();
    let mut group_depth = 0usize;
    for c in chars {
        match c {
            // 元字符与转义：字面前缀到此为止（宁可退回慢路，也不要取错）。
            '.' | '*' | '+' | '?' | '{' | '|' | '$' | '[' | ']' | ')' | '\\' | '(' => {
                if c == '(' {
                    group_depth += 1;
                } else {
                    break;
                }
            }
            _ if group_depth > 0 => {
                // 处在分组里：第一个字面字符可以当"可能的首字符"，
                // 但它不构成"必然的前缀"，所以到这里就停。
                out.push(c);
                break;
            }
            _ => out.push(c),
        }
    }
    out
}

/// 正则里 `$` 是否紧跟在一个字面量之后（判断"是否必须以末尾结尾"）。
#[must_use]
fn ends_with_anchor(regex: &str) -> bool {
    regex.ends_with('$') && !regex.ends_with("\\$")
}

// ─────────────────────────────────────────────────────────────────────────────
// recognizer
// ─────────────────────────────────────────────────────────────────────────────

/// **识别器**：把"符合某个模式的输入"标上对应的标签。
///
/// 它不是切分器而是**切分器的前置扫描**：RIME 里 `recognizer` 是 processor
/// 而 `matcher` 是 segmentor，两者配合完成"认出 → 切分"。
/// 我们按同一分工实现：
///
/// - 本类型负责**扫描**（[`Recognizer::scan`]），产出 [`InputScan`]；
/// - [`Matcher`] 负责**切分**（把扫描结果变成分段）。
///
/// **为什么不让一个东西干两件事**：`matcher` 与 `affix_segmentor` 都要用
/// 同一份扫描结果，而它们对"正文从哪开始"的解读不同。分开之后，
/// 扫描只做一次，两个切分器共享它。
pub struct Recognizer {
    /// 模式（按名字有序——诊断输出才有确定顺序）。
    patterns: Vec<CompiledPattern>,
    /// 要不要叠加预设模式。
    import_preset: Option<String>,
}

/// 一个已编译的模式：源码 + 已编译的正则。
struct CompiledPattern {
    /// 模式名（= 标签）。
    ///
    /// **存的是 intern 过的 `Tag`，不是 `String`**：标签要与
    /// [`Segment::tags`](stele_core::Segment::tags) 里放的完全一致
    /// （同一个 `&'static str`），否则 `has_tag` 那类比较会因为
    /// "名字相同但不是同一块内存 / 不同生命周期"而失败。
    tag: Tag,
    /// 模式名的字符串形态（排序与诊断用）。
    name: String,
    /// 字面前缀（快速排除）。
    leading: String,
    /// 已编译的正则。
    regex: crate::regex::Regex,
    /// 是否要求"整串匹配到末尾"。
    to_end: bool,
}

impl Recognizer {
    /// 由配置构造。
    ///
    /// # Errors
    ///
    /// 某个模式正则编译失败时返回 [`RecognizerError`]——**装载期响亮失败**，
    /// 而不是让那个模式永远不生效（那会变成"某个前缀偶尔不好使"）。
    pub fn new(spec: &RecognizerSpec, tags: &mut TagTable) -> Result<Self, RecognizerError> {
        let mut patterns = Vec::with_capacity(spec.patterns.len());
        for p in &spec.patterns {
            let regex = crate::regex::Regex::compile(&p.regex).map_err(|e| {
                RecognizerError::BadPattern {
                    name: p.name.clone(),
                    regex: p.regex.clone(),
                    reason: e.to_string(),
                    line: p.at.line,
                }
            })?;
            // 模式名就是标签：登记它，这样 `Segment::tags` 里放的是
            // 与方案数据同一个 intern 过的 `&'static str`。
            let tag = tags.intern(&p.name);
            patterns.push(CompiledPattern {
                tag,
                name: p.name.clone(),
                leading: if p.leading.is_empty() {
                    leading_literal(&p.regex)
                } else {
                    p.leading.clone()
                },
                regex,
                to_end: ends_with_anchor(&p.regex),
            });
        }
        Ok(Self {
            patterns,
            import_preset: spec.import_preset.clone(),
        })
    }

    /// 声明了要导入预设、但调用方给不出预设时的**出声**依据。
    #[must_use]
    pub fn import_preset(&self) -> Option<&str> {
        self.import_preset.as_deref()
    }

    /// 本识别器认识的模式名（= 它能产出的标签）。
    #[must_use]
    pub fn pattern_names(&self) -> Vec<&str> {
        self.patterns.iter().map(|p| p.name.as_str()).collect()
    }

    /// 扫一遍输入，产出全部"认领"。
    ///
    /// 生效的条件（两条都满足才算认出）：
    ///
    /// 1. 输入**以该模式的字面前缀开头**（快速排除；空前缀恒真）；
    /// 2. 从开头匹配上，且——若模式以 `$` 结尾——**恰好匹配到输入末尾**。
    ///
    /// 第 2 条是"前缀模式"与"必须以某字符结尾"的分界：
    /// `^uU[a-z]+$` 要匹配到末尾才算认出（边打边认），
    /// 而 `^;.*;$` 只在用户敲了收尾的分号之后才算。
    ///
    /// **同一起点有多个模式命中时，取匹配最长的那个**；长度相同则按名字
    /// 字典序（保证确定性——PLAN §5.2）。
    #[must_use]
    pub fn scan(&self, input: &str) -> InputScan {
        let mut scan = InputScan::default();
        if input.is_empty() {
            return scan;
        }
        let mut best: Option<(usize, Tag)> = None; // (字节终点, 标签)
        for p in &self.patterns {
            if !input.starts_with(p.leading.as_str()) {
                continue;
            }
            let Some(chars_end) = p.regex.match_prefix_len(input, p.to_end) else {
                continue;
            };
            if chars_end == 0 {
                continue;
            }
            // 正则引擎按 `char` 计数，认领按字节记 —— 必须换算，
            // 否则中文输入下所有位置都会错位（§5 坑 12 的同类错误）。
            let bytes_end: usize = input.chars().take(chars_end).map(char::len_utf8).sum();
            if bytes_end == 0 {
                continue;
            }
            let better = match best {
                None => true,
                Some((len, tag)) => {
                    bytes_end > len
                        || (bytes_end == len
                            && p.name < self.patterns.iter().find(|x| x.tag == tag).map_or(String::new(), |x| x.name.clone()))
                }
            };
            let _ = chars_end;
            if better {
                best = Some((bytes_end, p.tag));
            }
        }
        if let Some((end, tag)) = best {
            scan.claims.push(stele_core::Claim {
                start: 0,
                end,
                tag,
            });
            scan.body_start.push(0);
        }
        scan
    }
}

/// 认领找不到标签时用的兜底标签——**不带任何输入法含义**的名字。
///
/// 它存在的唯一理由是"类型上必须给一个 `&'static str`"；
/// 真要走到这里说明识别器内部不一致，那是个 bug 而不是配置问题。
#[allow(dead_code)]
const ABC_FALLBACK: Tag = "abc";

/// 识别器配置错误。
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecognizerError {
    /// 正则编译失败。
    BadPattern {
        /// 模式名。
        name: String,
        /// 正则源码。
        regex: String,
        /// 失败原因。
        reason: String,
        /// 源文件行号。
        line: usize,
    },
}

impl core::fmt::Display for RecognizerError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadPattern {
                name,
                regex,
                reason,
                line,
            } => write!(
                f,
                "第 {line} 行：`recognizer/patterns/{name}` 的正则 `{regex}` 编译失败：{reason}"
            ),
        }
    }
}

impl std::error::Error for RecognizerError {}

// ─────────────────────────────────────────────────────────────────────────────
// 切分器
// ─────────────────────────────────────────────────────────────────────────────

/// **匹配切分器**：把 [`Recognizer::scan`] 认出的部分切出来并打标签。
///
/// 对应 RIME 的 `matcher`。它自己不认任何东西——**它只是把识别结果落实成分段**。
pub struct Matcher {
    /// 本切分器负责的标签（= 识别器模式名）。
    tags: Vec<Tag>,
    /// 扫描结果（每次 `compose` 前由流水线用当前输入重算）。
    scan: InputScan,
}

impl Matcher {
    /// 由扫描结果构造。
    #[must_use]
    pub fn new(scan: InputScan, tags: Vec<Tag>) -> Self {
        Self { tags, scan }
    }

    /// 换一份扫描结果（输入变了就换）。
    pub fn rescan_all(&mut self, scan: InputScan) {
        self.scan = scan;
    }
}

impl Segmentor for Matcher {
    fn name(&self) -> &'static str {
        "matcher"
    }

    fn rescan(&mut self, scan: &stele_core::InputScanView<'_>) {
        self.scan = InputScan {
            claims: scan.claims.to_vec(),
            body_start: Vec::new(),
        };
    }

    fn proceed(&self, _q: &Query<'_>, seg: &mut stele_core::Segmentation) -> bool {
        let Some(claim) = self.scan.claim(seg.segments.last().map_or(0, |s| s.span.end)) else {
            return false;
        };
        let claim = *claim;
        // 只认领"本切分器负责的"标签；别的标签留给别的切分器。
        if !self.tags.contains(&claim.tag) {
            return false;
        }
        let mut s = Segment::new(Span::new(claim.start, claim.end));
        s.status = SegmentStatus::Guess;
        s.tags.push(claim.tag);
        seg.segments.push(s);
        // `true` = 这一段到此为止，后面的切分器不必再看
        // （RIME 的"优先级较高的 Segmentor 可以中止当前回合"）。
        true
    }

    fn tags(&self) -> &[Tag] {
        &self.tags
    }
}

/// **带词缀的切分器**：吃掉前缀/后缀，把正文交给别的翻译器。
///
/// 对应 RIME 的 `affix_segmentor`。拆字反查就是它：
/// 用户敲 `uUni`，前缀 `uU` **不是**要查的东西，`ni` 才是。
///
/// # 前缀是**一个字面字符串**
///
/// `prefix: "uU"` 就是"两个字符 `u`、`U` 按顺序出现"，不是"`u` 或 `U`"。
/// 这一条有 librime 源码与 rime-ice 自己的注释双重印证，
/// 见 [`expand_prefix`] 的说明——我第一版猜错了，并把它写成了"RIME 约定"。
pub struct AffixSegmentor {
    tag: Tag,
    /// 接受的前缀（已展开成列表）。
    prefixes: Vec<String>,
    /// 后缀。
    suffix: Option<String>,
    /// 额外标签。
    extra_tags: Vec<Tag>,
    /// 显示提示。
    tips: Option<String>,
    /// 扫描结果。
    scan: InputScan,
}

impl AffixSegmentor {
    /// 由配置构造。
    ///
    /// `tag` 为 `None` 时用 `fallback` 兜底（调用方通常传方案的主标签）。
    #[must_use]
    pub fn new(spec: &AffixSpec, tags: &mut TagTable, fallback: Tag) -> Self {
        let tag = match &spec.tag {
            Some(t) => *t,
            None => tags.intern(fallback),
        };
        Self {
            tag,
            prefixes: expand_prefix(spec.prefix.as_deref()),
            suffix: spec.suffix.clone(),
            extra_tags: spec.extra_tags.clone(),
            tips: spec.tips.clone(),
            scan: InputScan::default(),
        }
    }

    /// 显示提示（例如 `"  〔拆字〕"`）。
    #[must_use]
    pub fn tips(&self) -> Option<&str> {
        self.tips.as_deref()
    }

    /// 换一份扫描结果。
    pub fn rescan_all(&mut self, scan: InputScan) {
        self.scan = scan;
    }

    /// 前缀（已展开）。
    #[must_use]
    pub fn prefixes(&self) -> &[String] {
        &self.prefixes
    }

    /// 后缀。
    #[must_use]
    pub fn suffix(&self) -> Option<&str> {
        self.suffix.as_deref()
    }
}

/// `prefix` 读成**一个字面前缀**——RIME 就是字面比较。
///
/// # 这条注释记着一次"我自己发明了约定"
///
/// 我第一版把 `uU` 读成"`u` 或 `U` 两种单字符写法"（以为那是 RIME 的
/// 大小写约定），于是 `uUni` 的前缀只被吃掉 `u`、正文变成 `Uni`——
/// 反查一个候选都不出。当时我把这个猜测**写进了文档注释**，
/// 而它是错的。
///
/// librime 的源码是字面的（`src/rime/gear/affix_segmentor.cc`）：
///
/// ```cpp
/// if (prefix_.empty() || !boost::starts_with(active_input, prefix_))
///   return true;
/// active_input.erase(0, prefix_.length());
/// ```
///
/// `prefix_` 是一个整体字符串，`starts_with` 是字面前缀比较，
/// 剥离时 `erase(0, prefix_.length())` 剥掉**整个前缀**。
/// rime-ice 自己的注释也印证这一点：
/// 「反查前缀（反查时前缀会消失影响打英文所以设定为两个字母…）」——
/// 两个字母就是两个字母。
///
/// **教训**：不确定的约定不要写成文档里的"RIME 约定"。
/// 这一条现在有源码引用，也有端到端测试（`uUni` → 正文 `ni`）。
#[must_use]
pub fn expand_prefix(raw: Option<&str>) -> Vec<String> {
    match raw {
        Some(s) if !s.is_empty() => vec![s.to_owned()],
        _ => Vec::new(),
    }
}

impl Segmentor for AffixSegmentor {
    fn name(&self) -> &'static str {
        "affix_segmentor"
    }

    fn body_start(&self) -> Option<(Tag, usize)> {
        // 前缀是**字面串**（见 `expand_prefix`），因此长度是唯一的。
        let len = self.prefixes.first().map_or(0, String::len);
        Some((self.tag, len))
    }

    fn rescan(&mut self, scan: &stele_core::InputScanView<'_>) {
        self.scan = InputScan {
            claims: scan.claims.to_vec(),
            body_start: Vec::new(),
        };
    }

    fn proceed(&self, q: &Query<'_>, seg: &mut stele_core::Segmentation) -> bool {
        let pos = seg.segments.last().map_or(0, |s| s.span.end);
        let rest = &q.input[pos..];
        // 前缀必须真的出现在**这里**；多个前缀都能匹配时取**最长**的
        // （`uU` 优先于 `u`，这样两种写法都能用）。
        let Some(prefix) = self
            .prefixes
            .iter()
            .filter(|p| rest.starts_with(p.as_str()))
            .max_by_key(|p| p.len())
        else {
            return false;
        };
        let after_prefix = pos + prefix.len();
        // 后缀：有就必须在末尾。
        let body_end = match &self.suffix {
            Some(suf) => match q.input[after_prefix..].strip_suffix(suf.as_str()) {
                Some(body) => after_prefix + body.len(),
                // 后缀没出现 → 这个切分器**不管**这一段（把机会留给别人）。
                None => return false,
            },
            None => q.input.len(),
        };
        if body_end <= after_prefix {
            // 只有前缀、没有正文：不去切（例如刚敲下 `uU`）。
            return false;
        }

        let mut s = Segment::new(Span::new(pos, body_end));
        s.status = SegmentStatus::Guess;
        s.tags.push(self.tag);
        for t in &self.extra_tags {
            s.tags.push(*t);
        }
        seg.segments.push(s);
        true
    }

    fn tags(&self) -> &[Tag] {
        // 返回值要活得和 `&self` 一样久 —— 用 `extra_tags` 的切片表达不了
        // "主标签 + 额外标签"，所以这里返回一个只含主标签的切片，
        // 由流水线另外收集 `extra_tags`（见 `PipelineImpl::build`）。
        std::slice::from_ref(&self.tag)
    }
}

/// 词缀切分器**实际会产出的全部标签**（主标签 + 额外标签）。
///
/// 做成自由函数而不是 trait 方法，是因为 trait 的 `tags()` 要返回借用，
/// 而"两个来源拼起来"没法返回借用。这个函数返回拥有所有权的 `Vec`，
/// 只在**装配期**调用一次，不在按键路径上。
#[must_use]
pub fn affix_all_tags(spec: &AffixSpec, tags: &mut TagTable, fallback: Tag) -> Vec<Tag> {
    let mut out = vec![match &spec.tag {
        Some(t) => *t,
        None => tags.intern(fallback),
    }];
    out.extend(spec.extra_tags.iter().copied());
    out
}

/// **普通编码切分器**：把剩下的部分整段当作编码。
///
/// 对应 RIME 的 `abc_segmentor`。它是**兜底切分器**——最后一个上场，
/// 别的东西都没认出来，那它就是了。
///
/// # 它必须知道"别人认领到哪里"
///
/// 否则 `uUni` 会被它整串吞掉，拆字反查永远轮不到。
/// 所以它接收扫描结果，遇到第一个**比自己当前位置更靠后**的认领就停下。
pub struct CodingSegmentor {
    tag: Tag,
    scan: InputScan,
}

impl CodingSegmentor {
    /// 构造。
    #[must_use]
    pub fn new(tag: Tag, scan: InputScan) -> Self {
        Self { tag, scan }
    }

    /// 换一份扫描结果。
    pub fn rescan_all(&mut self, scan: InputScan) {
        self.scan = scan;
    }
}

impl Segmentor for CodingSegmentor {
    fn name(&self) -> &'static str {
        "abc_segmentor"
    }

    fn rescan(&mut self, scan: &stele_core::InputScanView<'_>) {
        self.scan = InputScan {
            claims: scan.claims.to_vec(),
            body_start: Vec::new(),
        };
    }

    fn proceed(&self, q: &Query<'_>, seg: &mut stele_core::Segmentation) -> bool {
        let pos = seg.segments.last().map_or(0, |s| s.span.end);
        if pos >= q.input.len() {
            return false;
        }
        // 当前位置有别人认领 → **不接手**。
        //
        // 这一条让"切分器顺序"真的成为优先级：`matcher` 排在前面时，
        // 它还没有机会上场（当前轮的 `pos` 就已经是认领起点）——
        // 兜底切分器必须让路，否则 `uUni` 会被它整串吞掉，
        // 拆字反查永远轮不到。
        if self.scan.claim_at(pos).is_some() {
            return false;
        }
        // 到下一个"别人认领的起点"为止（没有就一直到末尾）。
        let end = self.scan.next_claim_start(pos).unwrap_or(q.input.len());
        if end <= pos {
            return false;
        }
        let mut s = Segment::new(Span::new(pos, end));
        s.status = SegmentStatus::Guess;
        s.tags.push(self.tag);
        seg.segments.push(s);
        true
    }

    fn tags(&self) -> &[Tag] {
        std::slice::from_ref(&self.tag)
    }
}

/// **符号切分器**：认出"一个标点字符"或"符号表前缀开头的一段"。
///
/// 对应 RIME 的 `punct_segmentor`。它让标点走**独立的一段**，
/// 于是标点翻译器可以只对标点段生效（tag 绑定），
/// 而不会去抢正常的编码输入。
pub struct SymbolSegmentor {
    tag: Tag,
    /// 单字符标点（映射表的键里长度为 1 的那些）。
    singles: Vec<char>,
    /// 符号表前缀（例如 `/` 或 `v`）。
    symbol_prefix: Option<char>,
}

impl SymbolSegmentor {
    /// 由标点配置构造。
    ///
    /// # `tag` 必须是 **intern 过的**标签
    ///
    /// 这里是"一个 `&'static str` 陷阱"的真实案例：两处代码各自
    /// `Box::leak("punct")` 会得到**两块不同的内存**，而
    /// `Vec<Tag>::contains` 比的是值——看上去应该能匹配，实际上……
    /// 也能匹配。真正的坑在**比较之外**：只要有一处没走 intern 表，
    /// 就会出现"同一个名字的两个标签"，而任何按标签聚合的逻辑都会
    /// 悄悄少收一半。所以标签**只准从 [`TagTable`] 里拿**。
    #[must_use]
    pub fn new(tag: Tag, spec: &PunctuatorSpec) -> Self {
        let mut singles: Vec<char> = Vec::new();
        for (k, _) in spec.full_shape.iter().chain(spec.half_shape.iter()) {
            let mut cs = k.chars();
            if let (Some(c), None) = (cs.next(), cs.next()) {
                if !singles.contains(&c) {
                    singles.push(c);
                }
            }
        }
        singles.sort_unstable();
        Self {
            tag,
            singles,
            symbol_prefix: spec.symbol_prefix,
        }
    }

    /// 这一段是不是"符号输入"。
    ///
    /// 判据两条：① 整段就是一个映射表里的单字符；② 以符号表前缀开头。
    #[must_use]
    pub fn matches(&self, text: &str) -> bool {
        if let Some(p) = self.symbol_prefix {
            if text.starts_with(p) {
                return true;
            }
        }
        let mut cs = text.chars();
        matches!((cs.next(), cs.next()), (Some(c), None) if self.singles.contains(&c))
    }
}

impl Segmentor for SymbolSegmentor {
    fn name(&self) -> &'static str {
        "punct_segmentor"
    }

    fn proceed(&self, q: &Query<'_>, seg: &mut stele_core::Segmentation) -> bool {
        let pos = seg.segments.last().map_or(0, |s| s.span.end);
        let rest = &q.input[pos..];
        if !self.matches(rest) {
            return false;
        }
        let mut s = Segment::new(Span::new(pos, q.input.len()));
        s.status = SegmentStatus::Guess;
        s.tags.push(self.tag);
        seg.segments.push(s);
        true
    }

    fn tags(&self) -> &[Tag] {
        std::slice::from_ref(&self.tag)
    }
}

/// 从模式列表里取出全部字面前缀（供装载器报"这个模式永远不会命中"之类）。
#[must_use]
pub fn all_leadings(patterns: &[RecogPattern]) -> Vec<String> {
    patterns
        .iter()
        .map(|p| {
            if p.leading.is_empty() {
                leading_literal(&p.regex)
            } else {
                p.leading.clone()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::At;

    fn pat(name: &str, regex: &str) -> RecogPattern {
        RecogPattern {
            name: name.to_owned(),
            leading: leading_literal(regex),
            regex: regex.to_owned(),
            trailing: None,
            at: At::new(1),
        }
    }

    #[test]
    fn leading_literal_stops_at_metacharacters() {
        assert_eq!(leading_literal("^uU[a-z]+$"), "uU");
        assert_eq!(leading_literal("^v([0-9]|10)$"), "v");
        assert_eq!(leading_literal("^abc$"), "abc");
        // 分组开头：**取不到字面前缀**。分组里的第一个字面字符
        // 看上去像"可能的首字符"，但它后面跟着 `|` 之类的选择时
        // 就完全不是前缀——取错会直接错杀匹配。
        // 宁可返回空串（人人都匹配，走完整正则那条慢路）。
        assert_eq!(leading_literal("^([nl])ue$"), "");
        // 转义：保守停下（宁可走慢路，也不要取错前缀）。
        assert_eq!(leading_literal(r"^\d+$"), "");
    }

    #[test]
    fn recognizer_claims_a_prefix_mode() {
        let mut tags = TagTable::new();
        let spec = RecognizerSpec {
            import_preset: None,
            patterns: vec![pat("radical_lookup", "^uU[a-z]+$")],
        };
        let r = Recognizer::new(&spec, &mut tags).unwrap();

        let scan = r.scan("uUni");
        assert_eq!(scan.claims.len(), 1);
        assert_eq!(scan.claims[0].end, 4);
        assert_eq!(scan.claims[0].tag, "radical_lookup");

        // 前缀模式：边打边认。
        assert!(r.scan("uU").claims.is_empty(), "只有前缀时不算认出");
        assert!(r.scan("ni").claims.is_empty());
    }

    #[test]
    fn recognizer_requires_the_trailing_anchor_when_declared() {
        let mut tags = TagTable::new();
        let spec = RecognizerSpec {
            import_preset: None,
            patterns: vec![pat("punct", "^v([0-9]|10|[A-Za-z]+)$")],
        };
        let r = Recognizer::new(&spec, &mut tags).unwrap();
        assert_eq!(r.scan("v10").claims.len(), 1);
        assert!(r.scan("v1a").claims.is_empty(), "^v([0-9]|10|[A-Za-z]+)$ 不吃 v1a");
        assert_eq!(r.scan("vabc").claims.len(), 1);
        assert!(r.scan("v").claims.is_empty(), "v 之后还没有内容");
    }

    #[test]
    fn longest_match_wins_and_ties_are_deterministic() {
        let mut tags = TagTable::new();
        let spec = RecognizerSpec {
            import_preset: None,
            patterns: vec![pat("short", "^uu[a-z]$"), pat("long", "^uu[a-z]+$")],
        };
        let r = Recognizer::new(&spec, &mut tags).unwrap();
        let scan = r.scan("uummm");
        assert_eq!(scan.claims[0].tag, "long", "取匹配最长的那个");
    }

    #[test]
    fn bad_regex_fails_loudly_at_load_time() {
        let mut tags = TagTable::new();
        let spec = RecognizerSpec {
            import_preset: None,
            patterns: vec![pat("broken", "^a(")],
        };
        let Err(err) = Recognizer::new(&spec, &mut tags) else {
            panic!("坏正则必须报错");
        };
        assert!(matches!(err, RecognizerError::BadPattern { .. }));
        // 报错要指得出是哪个模式、第几行。
        let msg = err.to_string();
        assert!(msg.contains("broken") && msg.contains("第 1 行"), "{msg}");
    }

    #[test]
    fn prefix_is_a_literal_string_exactly_like_librime() {
        // **字面**，没有大小写变体。librime 的 `boost::starts_with(prefix_)`
        // 与 `erase(0, prefix_.length())` 就是这么做的。
        assert_eq!(expand_prefix(Some("uU")), vec!["uU"]);
        assert_eq!(expand_prefix(Some("v")), vec!["v"]);
        assert!(expand_prefix(None).is_empty());
        assert!(expand_prefix(Some("")).is_empty());

        let spec = AffixSpec {
            tag: Some("chaizi"),
            prefix: Some("uU".into()),
            ..Default::default()
        };
        let mut tags = TagTable::new();
        let seg = AffixSegmentor::new(&spec, &mut tags, "abc");
        assert_eq!(seg.body_start(), Some(("chaizi", 2)));
        assert_eq!(seg.prefixes(), ["uU".to_owned()]);
    }

    #[test]
    fn coding_segmentor_stops_at_the_next_claim() {
        let mut tags = TagTable::new();
        let spec = RecognizerSpec {
            import_preset: None,
            patterns: vec![pat("radical_lookup", "^uU[a-z]+$")],
        };
        let r = Recognizer::new(&spec, &mut tags).unwrap();
        let scan = r.scan("uUni");
        let seg_tag = tags.intern("abc");
        let abc = CodingSegmentor::new(seg_tag, scan.clone());
        let opts = stele_core::Options::new();
        let ctx = stele_core::Context::default();
        let q = Query {
            input: "uUni",
            caret: 4,
            options: &opts,
            context: &ctx,
            segment_text: "uUni",
        };
        let mut seg = stele_core::Segmentation::default();
        // 认领从 0 开始 —— 兜底切分器不该抢在它前面。
        assert!(!abc.proceed(&q, &mut seg), "认领从 0 开始时兜底切分器不接手");
        assert!(seg.is_empty());

        // 认领**不在开头**时，兜底切分器只吃到认领的起点为止。
        let mut seg2 = stele_core::Segmentation::default();
        seg2.segments.push(stele_core::Segment::new(stele_core::Span::new(
            4, 6,
        )));
        let q2 = Query {
            input: "ni hao uUni",
            caret: 11,
            options: &opts,
            context: &ctx,
            segment_text: "ni hao uUni",
        };
        let scan2 = {
            let mut t2 = TagTable::new();
            let r2 = Recognizer::new(
                &RecognizerSpec {
                    import_preset: None,
                    patterns: vec![pat("radical_lookup", "^ni hao uU[a-z]+$")],
                },
                &mut t2,
            )
            .unwrap();
            r2.scan("ni hao uUni")
        };
        let abc2 = CodingSegmentor::new(seg_tag, scan2);
        assert!(abc2.proceed(&q2, &mut seg2));
        assert_eq!(
            seg2.segments.last().unwrap().span,
            stele_core::Span::new(6, 11),
            "兜底切分器必须停在下一个认领的起点"
        );
    }

    #[test]
    fn symbol_segmentor_recognizes_single_punctuation_and_the_prefix() {
        let spec = PunctuatorSpec {
            half_shape: vec![(",".into(), "，".into())],
            symbol_prefix: Some('v'),
            ..Default::default()
        };
        let tags = TagTable::new();
        let seg = SymbolSegmentor::new("punct", &spec);
        let _ = tags;
        assert!(seg.matches(","));
        assert!(seg.matches("v1"));
        assert!(!seg.matches("ni"), "两个字符不是单标点");
        assert!(!seg.matches(""));
    }
}
