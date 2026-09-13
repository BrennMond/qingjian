//! # wordlist-gen — 把干净来源的词表编成青简的 `.dict.yaml`
//!
//! 中文职责：读「汉字→拼音」表与几份带词频的公开词表，编出一份
//! `schemes/qingjian-default/` 能直接装载的词库，并**同步更新方案里的音节表**。
//! English role: turn clean public word/pinyin sources into a `.dict.yaml`
//! the default scheme can load, and keep the scheme's alphabet in sync.
//!
//! 架构位置：**部署期工装**，与 `tools/` 里其它东西一样**不进内核、不进 CI**。
//! 它不参与引擎的任何一次按键——产物才是引擎的输入。
//!
//! # 为什么需要它（P3.5）
//!
//! 在这之前 `schemes/qingjian-default` 只有 30 条演示词，于是
//! "装上就能打字"是空的、librime 对照只能比结构比不了排序（HANDOFF §4）。
//! 而雾凇那 44 MB 词表**不进仓库**（授权混合，PLAN §10），因此默认词库必须
//! 由**可分发来源**在部署期生成。这个工具就是那条路径。
//!
//! # 数据来源与它们的许可（都是可随作品分发的）
//!
//! | 文件 | 来源 | 许可 | 提供了什么 |
//! | --- | --- | --- | --- |
//! | `pinyin.txt` | `mozillazg/pinyin-data` | MIT | 41k 汉字的拼音，**首要读音在前** |
//! | `THUOCL_*.txt` | `thunlp/THUOCL` | MIT | 分领域词表，**带语料词频** |
//! | `jieba_dict.txt` | `fxsjy/jieba` | MIT | 通用词表（58 万条，`词 词频 词性`） |
//! | `opencc/TSCharacters.txt` | `BYVoid/OpenCC` | Apache-2.0 | 繁体字形集合，用于**简体过滤** |
//!
//! 取回它们是 [`tools/fetch-sources.sh`](../../fetch-sources.sh) 的事，
//! 这个工具只读本地文件——**部署期一次网络访问都不做**。
//! 每个来源的固定 revision、sha256 与许可证见
//! [`tools/sources.lock`](../../sources.lock) 与 `THIRD_PARTY_NOTICES.md`。
//!
//! # 三条必须说清的取舍
//!
//! 1. **多音字用"语料里哪个读音更常见"来定**（而不是取第一个）。
//!    `pinyin.txt` 给每个字一个有序的读音列表，但它是按字典习惯排的，
//!    不是按语料频率。我们拿整份词表当一个微型语料：一个读音在词表的
//!    音节里出现得越多，它就越可能是常用读法。**这是启发式，不是真理**——
//!    所以它写在参数里（`ReadingPolicy`），并且产物头部如实注明。
//! 2. **不猜拼音**：任何一个字在 `pinyin.txt` 里查不到，**整条词条被丢弃**，
//!    并计数报出来。把汉字原样当拼音塞进去，会造出"永远打不出来"的条目。
//! 3. **音节表由产物反推**：词库里出现的音节必须**全部**在方案的
//!    `speller.alphabet` 里，否则装载期会响亮报错（P2 就靠这条抓过我）。
//!    本工具因此把 `pinyin.schema.yaml` 的 `alphabet:` 整段重写。
//!
//! # 用法
//!
//! ```text
//! bash tools/fetch-sources.sh
//! cargo run --manifest-path tools/wordlist-gen/Cargo.toml -- \
//!     --sources schemes/qingjian-default/build \
//!     --out     schemes/qingjian-default
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// 命令行参数。
struct Args {
    sources: PathBuf,
    out: PathBuf,
    /// 每份 THUOCL 词表最多取多少条（按词频降序）。`None` = 全取。
    max_per_source: Option<usize>,
    /// 单字最多收录多少个（按拼音表的出现顺序，越靠前越常用）。
    max_chars: usize,
    /// 只打印会做什么，不写文件。
    dry_run: bool,
    /// 繁体字集合（OpenCC 的 `TSCharacters.txt`）。给了它就把
    /// **含繁体字的词**滤掉——见 `run()` 里的说明。
    traditional_chars: Option<PathBuf>,
}

const DEFAULT_MAX_CHARS: usize = 21_000;

/// 常用汉字区（U+4E00–U+9FFF）。单字只在这个范围内收录，
/// 且必须**出现在词表里**（见生成逻辑里的说明）。
const CJK_RANGE: std::ops::RangeInclusive<char> = '\u{4E00}'..='\u{9FFF}';

fn usage() -> String {
    format!(
        "用法: wordlist-gen [选项]\n\
         \n\
           --sources <目录>   源数据目录（默认 schemes/qingjian-default/build）\n\
           --out <目录>       写到哪里（默认 schemes/qingjian-default）\n\
           --max-per-source N 每份 THUOCL 词表最多取 N 条（按词频降序）\n\
           --max-chars N      单字最多收录 N 个（默认 {DEFAULT_MAX_CHARS}）\n\
           --traditional-chars <文件>\n\
                               繁体字表（OpenCC 的 TSCharacters.txt）。给了它，\n\
                               含繁体字的词条会被滤掉（默认 build/opencc/TSCharacters.txt，\n\
                               文件不存在则不过滤）\n\
           --dry-run          只报告会写什么，不动文件\n\
           -h, --help         显示这段\n"
    )
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        sources: PathBuf::from("schemes/qingjian-default/build"),
        out: PathBuf::from("schemes/qingjian-default"),
        max_per_source: None,
        max_chars: DEFAULT_MAX_CHARS,
        dry_run: false,
        traditional_chars: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--sources" => args.sources = next_path(&mut it, "--sources")?,
            "--out" => args.out = next_path(&mut it, "--out")?,
            "--max-per-source" => args.max_per_source = Some(next_usize(&mut it, &a)?),
            "--max-chars" => args.max_chars = next_usize(&mut it, &a)?,
            "--traditional-chars" => {
                args.traditional_chars = Some(next_path(&mut it, "--traditional-chars")?);
            }
            "--dry-run" => args.dry_run = true,
            "-h" | "--help" => {
                print!("{}", usage());
                std::process::exit(0);
            }
            other => return Err(format!("不认识的参数 `{other}`\n\n{}", usage())),
        }
    }
    Ok(args)
}

fn next_path(it: &mut impl Iterator<Item = String>, flag: &str) -> Result<PathBuf, String> {
    it.next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("`{flag}` 后面缺少一个值"))
}

fn next_usize(it: &mut impl Iterator<Item = String>, flag: &str) -> Result<usize, String> {
    it.next()
        .ok_or_else(|| format!("`{flag}` 后面缺少一个数"))?
        .parse()
        .map_err(|_| format!("`{flag}` 的值必须是正整数"))
}

// ─────────────────────────────────────────────────────────────────────────────
// 拼音：读表 + 去声调
// ─────────────────────────────────────────────────────────────────────────────

/// 音调符号 → 无调字母。**手写映射而不是用 Unicode 分解**：分解还要
/// 处理组合字符与兼容形式，而这张表小、可读、可测。
///
/// 声调符落在 a/e/i/o/u/ü 上。注意 `ü` 的四种声调（ǖǘǚǜ）
/// **不属于** `ü` 本身，必须单独列。
const TONE_MARKS: &[(char, char)] = &[
    ('ā', 'a'), ('á', 'a'), ('ǎ', 'a'), ('à', 'a'),
    ('ē', 'e'), ('é', 'e'), ('ě', 'e'), ('è', 'e'),
    ('ī', 'i'), ('í', 'i'), ('ǐ', 'i'), ('ì', 'i'),
    ('ō', 'o'), ('ó', 'o'), ('ǒ', 'o'), ('ò', 'o'),
    ('ū', 'u'), ('ú', 'u'), ('ǔ', 'u'), ('ù', 'u'),
    ('ǖ', 'v'), ('ǘ', 'v'), ('ǚ', 'v'), ('ǜ', 'v'),
    ('ü', 'v'), ('ń', 'n'), ('ň', 'n'), ('ǹ', 'n'), ('ḿ', 'm'),
];

/// 有调拼音 → 无调 ASCII 音节。
///
/// **ü 写成 `v`**：这是 RIME 生态的既定写法（`nve` / `lve` / `nv`），
/// 也是我们的方案数据里已经在用的写法。`ǖǘǚǜ` 与 `ü` 都归到 `v`。
#[must_use]
pub fn normalize_syllable(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if let Some((_, base)) = TONE_MARKS.iter().find(|(m, _)| *m == ch) {
            out.push(*base);
        } else {
            out.extend(ch.to_lowercase());
        }
    }
    out
}

/// 一份 `pinyin.txt` 读出来的结果。
#[derive(Debug)]
pub struct PinyinTable {
    /// 汉字 → 有序读音（无调，首要读音在前）。
    pub readings: BTreeMap<char, Vec<String>>,
    /// 有多个读音的字数（诊断用）。
    pub multi_reading: usize,
}

/// 解析 `mozillazg/pinyin-data` 的 `pinyin.txt`。
///
/// 行格式：`U+4E00: yī  # 一`，读音用逗号分隔，`#` 起是注释。
///
/// # Errors
///
/// 行格式不认识时返回一句说明，**并指出行号**（不静默跳过）。
pub fn parse_pinyin(text: &str) -> Result<PinyinTable, String> {
    let mut readings: BTreeMap<char, Vec<String>> = BTreeMap::new();
    let mut multi_reading = 0usize;

    for (i, raw) in text.lines().enumerate() {
        let no = i + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // 去掉行尾注释（拼音本身不含 `#`）。
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((code, rest)) = line.split_once(':') else {
            return Err(format!("第 {no} 行没有 `:`：`{line}`"));
        };
        let code = code.trim();
        let Some(hex) = code.strip_prefix("U+") else {
            return Err(format!("第 {no} 行的码位不是 `U+XXXX` 形式：`{code}`"));
        };
        let Ok(cp) = u32::from_str_radix(hex, 16) else {
            return Err(format!("第 {no} 行的码位 `{hex}` 不是十六进制数"));
        };
        let Some(ch) = char::from_u32(cp) else {
            return Err(format!("第 {no} 行的码位 U+{cp:04X} 不是合法字符"));
        };
        let mut list: Vec<String> = Vec::new();
        for p in rest.trim().split(',') {
            let p = p.trim();
            if p.is_empty() {
                continue;
            }
            let n = normalize_syllable(p);
            // 声调不同、无调形式相同的读音只留一个——否则音节表里会出现
            // 两项一模一样的编码单元。
            if !list.contains(&n) {
                list.push(n);
            }
        }
        if list.is_empty() {
            return Err(format!("第 {no} 行（{ch}）没有任何读音"));
        }
        if list.len() > 1 {
            multi_reading += 1;
        }
        readings.insert(ch, list);
    }

    Ok(PinyinTable {
        readings,
        multi_reading,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// 词表
// ─────────────────────────────────────────────────────────────────────────────

/// 一条候选词条。
#[derive(Clone, Debug)]
pub struct Word {
    /// 词（要上屏的文本）。
    pub text: String,
    /// 语料词频（越大越常用）。
    pub freq: u64,
    /// 来自哪份文件（诊断用）。
    pub source: String,
}

/// 解析一份 THUOCL 词表：`词<TAB>词频`。
///
/// # 上游数据的三个已知瑕疵（**实测**，不是猜测）
///
/// 这些文件是公开数据，不是为我们的解析器写的。实测到的三类：
///
/// | 瑕疵 | 出现在 | 处理 |
/// | --- | --- | --- |
/// | 词频带一个尾随字符（`125472s`） | `THUOCL_food.txt` 第 39 行 | 取前面的数字，**记一条警告** |
/// | 词频为空（`…办法<TAB>`） | `THUOCL_law.txt` | **丢弃该行**并记警告（没有频率的词没法排序） |
/// | 第一列有尾随空格 | `THUOCL_IT.txt` / `THUOCL_chengyu.txt` | 正常 `trim` |
///
/// **为什么容忍而不是报错**：这三处都是上游的录入瑕疵，数值本身没有歧义；
/// 为一行的尾随字符拒掉整份词表，只会让"干净来源"这条路走不通。
/// 但每一步都**记进警告**并打印出来——"我悄悄丢了多少行"是必须能被看见的。
///
/// # Errors
///
/// 没有 TAB、词为空、词频里一个数字都没有时返回带行号的说明。
pub fn parse_wordlist(words: &str, name: &str, warn: &mut Vec<String>) -> Result<Vec<Word>, String> {
    let mut out = Vec::new();
    for (i, raw) in words.lines().enumerate() {
        let no = i + 1;
        let line = raw.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() {
            continue;
        }
        // 分隔符：THUOCL 用 TAB，jieba 用空格。**两种都收**，
        // 因为这两种文件的**第二列都是词频**，而词本身不含空白。
        let Some((w, f)) = line
            .split_once('\t')
            .or_else(|| line.split_once(' '))
        else {
            return Err(format!("{name}:{no}：这一行既没有 TAB 也没有空格：`{line}`"));
        };
        // 第三列（jieba 的词性）忽略——它对排序没有用，而我们不假装用它。
        let f = f.split_whitespace().next().unwrap_or(f);
        let text = w.trim();
        if text.is_empty() {
            return Err(format!("{name}:{no}：词是空的"));
        }
        let raw_freq = f.trim();
        if raw_freq.is_empty() {
            warn.push(format!("{name}:{no}：词频为空，丢弃这一行（词：{text}）"));
            continue;
        }
        // 取前导数字（容忍上游的尾随字符，例如 `125472s`）。
        let digits: String = raw_freq.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            return Err(format!(
                "{name}:{no}：词频 `{raw_freq}` 里一个数字都没有，读不懂这一行"
            ));
        }
        if digits.len() != raw_freq.len() {
            warn.push(format!(
                "{name}:{no}：词频 `{raw_freq}` 有尾随字符，按 `{digits}` 处理"
            ));
        }
        let Ok(freq) = digits.parse::<u64>() else {
            return Err(format!("{name}:{no}：词频 `{digits}` 超出 u64 范围"));
        };
        out.push(Word {
            text: text.to_owned(),
            freq,
            source: name.to_owned(),
        });
    }
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────────
// 组词：把汉字串转成音节序列
// ─────────────────────────────────────────────────────────────────────────────

/// 选取哪个读音的策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadingPolicy {
    /// 取 `pinyin.txt` 里的第一个（字典习惯，最保守）。
    First,
    /// 主读音取列表首个，再在**同声母**的候选里按单字表证据微调。
    /// 平票时退回第一个——**结果因此是确定的**（铁律第 2 条）。
    CorpusFrequent,
}

/// 每个字用哪个读音。
#[must_use]
pub fn choose_readings(
    table: &PinyinTable,
    words: &[Word],
    policy: ReadingPolicy,
) -> BTreeMap<char, String> {
    // `words` 目前**不参与**判定（理由见下面的长注释：词表里没有词级拼音）。
    // 参数保留是因为它是这条策略的"输入契约"——将来接上带拼音的词库时，
    // 它就在手边，不必改所有调用点。
    let _ = words;
    let mut chosen: BTreeMap<char, String> = BTreeMap::new();
    for (ch, list) in &table.readings {
        chosen.insert(*ch, list[0].clone());
    }
    if policy == ReadingPolicy::First {
        return chosen;
    }

    // 多音字的取舍。**先说清我们手里有什么、没有什么**：
    //
    // · THUOCL 只给（词，词频），**不给拼音**——于是"银行 读 yín háng"
    //   这条信息不在数据里。
    // · 只用"音节频率"投票也**不行**：同一个字的每个候选读音在同一条词里
    //   都被记上一次，计数必然相同，等于没有信息。
    //
    // 所以这里用一个**只依赖单字表**、结果可复核的启发式：
    //
    //   1. 主读音 = `pinyin.txt` 里的第一个（字典习惯，与 `First` 一致）；
    //   2. **只在"与主读音同声母"的候选里**看证据——因为多音字的分歧
    //      绝大多数在韵母/声调那一侧（xíng/háng 不同声母，属于另一类分歧，
    //      这里**有意不猜**）；同声母的候选里，挑在"只有单一读音的字"
    //      中出现次数最多的那个。
    //
    // **代价必须说清**：像「银行」这样声母也变了的词，我们会读成 xíng。
    // 想要更准，需要的是**带拼音的词库**（例如 RIME 生态的 `pinyin.txt`
    // 之外的词级读音表），那是另一份数据源，不在当前许可清单里。
    // 这比"猜一个看起来更聪明的算法"诚实——后者会让排名无据可依。
    //
    // 统计口径：只数**单一读音**的字。多音字自己的每个读音都被记一次，
    // 计入就会把噪声放大到与证据同量级。
    let mut unigram: BTreeMap<String, u64> = BTreeMap::new();
    for list in table.readings.values() {
        if list.len() == 1 {
            *unigram.entry(list[0].clone()).or_insert(0) += 1;
        }
    }

    for (ch, list) in &table.readings {
        if list.len() < 2 {
            continue;
        }
        let primary = &list[0];
        let initial = initial_of(primary);
        let mut best: Option<(&String, u64)> = None;
        // **按列表顺序遍历**：只有严格更多才替换，于是平票保留列表靠前者
        // （= `First` 的结果）。顺序确定 ⇒ 结果确定。
        for r in list {
            if initial_of(r) != initial {
                continue;
            }
            let c = unigram.get(r).copied().unwrap_or(0);
            if best.is_none_or(|(_, bc)| c > bc) {
                best = Some((r, c));
            }
        }
        if let Some((r, _)) = best {
            chosen.insert(*ch, r.clone());
        }
    }
    chosen
}

/// 一个无调音节的**声母**（拼音的初始辅音）。
///
/// `zh` / `ch` / `sh` 是双字母声母；`zhuang` 的声母是 `zh` 而不是 `z`——
/// 这一点弄错会让"同声母"这个判据悄悄失效，所以它是单独一个函数，
/// 有自己的测试。
///
/// 零声母（`a` / `e` / `o` / `an` / `yi` / `wu` / `yu` …）统一记作
/// `""`：它们的对立面不是某个辅音，而是"有没有辅音"。
#[must_use]
pub fn initial_of(syllable: &str) -> &str {
    // 双字母声母优先。`get(..2)` 而不是 `&syllable[..2]`：后者在字节边界
    // 切错时会 panic，而这里的输入来自外部文件。
    if let Some(two) = syllable.get(..2) {
        if matches!(two, "zh" | "ch" | "sh") {
            return two;
        }
    }
    // 其余声母是单个辅音字母。元音开头（`an` / `er`）与半元音
    // `y` / `w`（`yi` / `wu` / `yu`）都算零声母——它们不是辅音声母。
    match syllable.chars().next() {
        Some(c) if c.is_ascii_alphabetic() && !"aeiouyw".contains(c) => &syllable[..1],
        _ => "",
    }
}

/// 把一个词转成音节序列；**任何一个字查不到就返回 `None`**。
#[must_use]
pub fn to_code(text: &str, chosen: &BTreeMap<char, String>) -> Option<Vec<String>> {
    let mut out = Vec::with_capacity(text.chars().count());
    for ch in text.chars() {
        out.push(chosen.get(&ch)?.clone());
    }
    Some(out)
}

// ─────────────────────────────────────────────────────────────────────────────
// 简繁过滤
// ─────────────────────────────────────────────────────────────────────────────

/// 从 `OpenCC` 的 `TSCharacters.txt`（繁→简）取出"**只有繁体写法**的汉字"集合。
///
/// 判据很直接：一个繁体字，如果它映射到的简体写法**与它自己不同**，
/// 那它就是一个繁体字形（`個→个`）。映射到自己的（`人→人`）不算。
///
/// **为什么要这个**：jieba 的词表里混着繁体条目（`1號店`）。把它们收进来，
/// 用户敲 `1hao dian` 会得到一个繁体候选——而 D32 明确说默认方案只做简体。
/// 与其在运行期靠 `simplifier` 转回来，不如**在数据层就不收**：
/// 前者要挂一份 s2t 表并常驻内存，后者一次性做完。
///
/// # Errors
///
/// 行格式不是 `繁<TAB>简…` 时返回带行号的说明。
pub fn parse_traditional_chars(text: &str, name: &str) -> Result<BTreeSet<char>, String> {
    let mut out = BTreeSet::new();
    for (i, raw) in text.lines().enumerate() {
        let no = i + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((from, to)) = line.split_once('\t') else {
            return Err(format!("{name}:{no}：这一行没有 TAB：`{line}`"));
        };
        let mut from_chars = from.trim().chars();
        let (Some(f), None) = (from_chars.next(), from_chars.next()) else {
            return Err(format!("{name}:{no}：`繁` 那一列必须是单个字：`{from}`"));
        };
        let Some(first_to) = to.split_whitespace().next() else {
            return Err(format!("{name}:{no}：`简` 那一列是空的"));
        };
        // 只认"一对一且不同"的映射：一对多的（`干 → 干 乾 幹` 的反向）
        // 我们无法判断哪个是简体，宁可不收。
        let mut to_chars = first_to.chars();
        if let (Some(t), None) = (to_chars.next(), to_chars.next()) {
            if t != f {
                out.insert(f);
            }
        }
    }
    Ok(out)
}

/// 词里只要有一个"繁体字形"就丢掉。
#[must_use]
pub fn has_traditional(text: &str, trad: &BTreeSet<char>) -> bool {
    text.chars().any(|c| trad.contains(&c))
}

// ─────────────────────────────────────────────────────────────────────────────
// 方案音节表重写
// ─────────────────────────────────────────────────────────────────────────────

/// 把 `speller.alphabet:` 那一段整段重写成 `syllables`。
///
/// **只动那一段**：方案里其它每一行都是人写的、带注释的设计决定，
/// 工具没有资格重写它们（也不该让它们因为"跑了一次生成器"而漂移）。
///
/// # Errors
///
/// 找不到 `alphabet:` 时返回说明——**不猜**，因为"没找到就当没这回事"
/// 会让词库里的音节永远进不了方案，症状是某个词永远打不出来。
pub fn rewrite_alphabet(schema: &str, syllables: &[String]) -> Result<String, String> {
    let lines: Vec<&str> = schema.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim_end() == "  alphabet:")
        .ok_or_else(|| "在方案里找不到 `  alphabet:` 这一行（speller 段的音节表）".to_owned())?;

    // 紧接着的、缩进更深的 `- ` 列表项行，就是旧表的正文。
    let mut end = start + 1;
    while end < lines.len() {
        let l = lines[end];
        let indent = l.len() - l.trim_start().len();
        if l.trim_start().starts_with("- ") && indent > 2 {
            end += 1;
        } else {
            break;
        }
    }

    let mut out = String::with_capacity(schema.len() + syllables.len() * 8);
    for l in &lines[..=start] {
        out.push_str(l);
        out.push('\n');
    }
    for s in syllables {
        out.push_str("    - ");
        out.push_str(s);
        out.push('\n');
    }
    for l in &lines[end..] {
        out.push_str(l);
        out.push('\n');
    }
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────────
// 主流程
// ─────────────────────────────────────────────────────────────────────────────

/// 词库头部（写明来源、许可，以及本次的两条取舍）。
///
/// **不用 `\` 续行的多行字符串字面量**：Rust 的续行会吃掉下一行的
/// 前导空白，而 YAML 头部**正是靠前导空白表意**（`import_tables:` 下的
/// 列表项必须有缩进）。第一版就是用续行写的，于是生成出来的
/// `- cn_dicts/base` 顶了格，装载器报
/// 「同一层里混用了「键: 值」与「- 列表项」」——
/// **一个只在真正装载产物时才会暴露的错误**。
/// 这里改成显式拼 `\n`，缩进写多少就是多少。
fn dict_header(policy: ReadingPolicy, entries: usize, sources: &[String]) -> String {
    let policy_text = match policy {
        ReadingPolicy::First => "取 `pinyin.txt` 里的首个读音（字典习惯）",
        ReadingPolicy::CorpusFrequent => {
            "主读音取 `pinyin.txt` 首个；只在**同声母**候选里按单字表证据微调（平票取靠前者）"
        }
    };
    let mut out = String::new();
    out.push_str("# 青简・拼音 —— 默认词库（**生成产物，不要手改**）\n");
    out.push_str("#\n");
    out.push_str("# 由 `tools/wordlist-gen` 从下列**授权明确**的来源生成：\n");
    for s in sources {
        let _ = writeln!(out, "#   · {s}");
    }
    out.push_str("#\n");
    out.push_str("# 逐项的许可、版权、固定 revision 与 sha256 见 `THIRD_PARTY_NOTICES.md`、\n");
    out.push_str("# `tools/sources.lock`、`licenses/`；为什么源数据不随仓库分发见 PLAN.md §10。\n");
    out.push_str("#\n");
    let _ = writeln!(out, "# 词条数：{entries}");
    let _ = writeln!(out, "# 多音字策略：{policy_text}");
    out.push_str("#\n");
    out.push_str("# 重新生成：\n");
    out.push_str("#   bash tools/fetch-sources.sh\n");
    out.push_str("#   cargo run --manifest-path tools/wordlist-gen/Cargo.toml -- \\\n");
    out.push_str("#       --sources schemes/qingjian-default/build --out schemes/qingjian-default\n");
    out.push('\n');
    out.push_str("---\n");
    // **词表名与文件名要一致**：主词典用 `import_tables: [cn_dicts/generated]`
    // 引用它，装载器按这个名字找文件。
    out.push_str("name: generated\n");
    out.push_str("version: \"0.2.0\"\n");
    out.push_str("sort: by_weight\n");
    out.push_str("...\n\n");
    out
}

fn main() {
    if let Err(e) = run() {
        eprintln!("✗ {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;

    // ── 读拼音表 ──
    let pinyin_path = args.sources.join("pinyin.txt");
    let pinyin_text = std::fs::read_to_string(&pinyin_path).map_err(|e| {
        format!(
            "读不了 {}：{e}\n先跑 `bash tools/fetch-sources.sh`。",
            pinyin_path.display()
        )
    })?;
    let table = parse_pinyin(&pinyin_text)?;
    eprintln!(
        "· 拼音表：{} 个字（其中 {} 个多音字）",
        table.readings.len(),
        table.multi_reading
    );

    // ── 读全部 THUOCL 词表 ──
    let mut words: Vec<Word> = Vec::new();
    let mut source_notes: Vec<String> = Vec::new();
    // 上游数据瑕疵的清单：**打印出来**，并进产物头部。
    let mut warnings: Vec<String> = Vec::new();
    let mut thuocl: Vec<PathBuf> = Vec::new();
    let mut jieba: Vec<PathBuf> = Vec::new();
    for e in std::fs::read_dir(&args.sources)
        .map_err(|e| format!("读不了源目录 {}：{e}", args.sources.display()))?
        .flatten()
    {
        let p = e.path();
        // 大小写敏感是**有意**的：我们只认 `fetch-sources.sh` 取回的那几个
        // 固定文件名，不打算兼容 `.TXT`。
        #[allow(clippy::case_sensitive_file_extension_comparisons)]
        let is_thuocl = {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            name.starts_with("THUOCL_") && name.ends_with(".txt")
        };
        // jieba 的通用词表：`词 词频 词性`。
        let is_jieba = p.file_name().and_then(|n| n.to_str()) == Some("jieba_dict.txt");
        if is_thuocl {
            thuocl.push(p);
        } else if is_jieba {
            jieba.push(p);
        }
    }
    thuocl.sort();
    if thuocl.is_empty() {
        return Err(format!(
            "{} 里没有 `THUOCL_*.txt`。先跑 `bash tools/fetch-sources.sh`。",
            args.sources.display()
        ));
    }
    for p in thuocl.iter().chain(jieba.iter()) {
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_owned();
        let text =
            std::fs::read_to_string(p).map_err(|e| format!("读不了 {}：{e}", p.display()))?;
        let mut ws = parse_wordlist(&text, &name, &mut warnings)?;
        ws.sort_by(|a, b| b.freq.cmp(&a.freq).then_with(|| a.text.cmp(&b.text)));
        if let Some(n) = args.max_per_source {
            ws.truncate(n);
        }
        eprintln!("· {name}：{} 条", ws.len());
        // **来源名要写对**：THUOCL 与 jieba 都是 MIT，但是两个上游、
        // 两个版权人。旧版把 jieba 也标成「THUOCL」，产物头部因此携带
        // 一句错误的署名——许可信息的价值全在准确。
        let upstream = if name.starts_with("THUOCL_") {
            "THUOCL"
        } else {
            "jieba"
        };
        source_notes.push(format!("{name}（{upstream}，MIT）—— {} 条", ws.len()));
        words.extend(ws);
    }

    // 去重：同一个词在多份词表里出现时取最大词频。
    let mut by_text: BTreeMap<String, Word> = BTreeMap::new();
    for w in words {
        by_text
            .entry(w.text.clone())
            .and_modify(|e| {
                if w.freq > e.freq {
                    e.freq = w.freq;
                }
            })
            .or_insert(w);
    }
    let mut words: Vec<Word> = by_text.into_values().collect();
    words.sort_by(|a, b| b.freq.cmp(&a.freq).then_with(|| a.text.cmp(&b.text)));
    eprintln!("· 去重后：{} 个词", words.len());

    // 简繁过滤（D32：默认方案只做简体）。表从 `--traditional-chars` 或
    // 默认的 `build/opencc/TSCharacters.txt` 来；找不到就**不过滤**并说明。
    let trad_path = args
        .traditional_chars
        .clone()
        .unwrap_or_else(|| args.sources.join("opencc/TSCharacters.txt"));
    match std::fs::read_to_string(&trad_path) {
        Ok(text) => {
            let name = trad_path.display().to_string();
            let trad = parse_traditional_chars(&text, &name)?;
            let before = words.len();
            words.retain(|w| !has_traditional(&w.text, &trad));
            eprintln!(
                "· 简繁过滤：繁体字形 {} 个；滤掉 {} 条词（{before} → {}）",
                trad.len(),
                before - words.len(),
                words.len()
            );
            source_notes.push(format!(
                "{}（`OpenCC`，Apache-2.0）—— 繁体字形 {} 个，用于简体过滤",
                trad_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("TSCharacters.txt"),
                trad.len()
            ));
        }
        Err(_) => {
            warnings.push(format!(
                "读不到繁体字表 `{}`，**未做简繁过滤**（词表里的繁体条目会原样收进默认方案）",
                trad_path.display()
            ));
        }
    }
    if !warnings.is_empty() {
        eprintln!("· 上游数据警告 {} 条（前 5 条）：", warnings.len());
        for w in warnings.iter().take(5) {
            eprintln!("    {w}");
        }
    }

    // ── 定读音 ──
    let chosen = choose_readings(&table, &words, ReadingPolicy::CorpusFrequent);

    // ── 生成 ──
    let mut syllables: BTreeSet<String> = BTreeSet::new();
    let mut lines: Vec<(String, String, u64)> = Vec::new();
    let mut lost_unknown = 0usize;

    // 词条：词频直接用 THUOCL 的计数。**先跑一遍**，因为单字的收录范围
    // 要由"哪些字真的出现在词里"来决定（见下）。
    let mut word_count = 0usize;
    // **(词, 编码) 全局去重**。同一个词可能既是词表里的词条、又是单字表里的
    // 一个字（`一` 两者都有），也可能带两条不同读音（那是合法的两条记录）。
    // 装载器**拒绝重复键**，所以这一步不做的话产物直接读不回去
    // ——这正是"生成器自己校验一遍"抓到的第一个错。
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for w in &words {
        let Some(code) = to_code(&w.text, &chosen) else {
            lost_unknown += 1;
            continue;
        };
        if !seen.insert((w.text.clone(), code.join(" "))) {
            continue;
        }
        for u in &code {
            syllables.insert(u.clone());
        }
        // `+1`：THUOCL 里存在权重 0 的条目，而 0 在我们的格式里 = 最低（等价于
        // "没有权重"）。加一让"来自数据的 0"与"没有权重"区分开。
        lines.push((w.text.clone(), code.join(" "), w.freq + 1));
        word_count += 1;
    }

    // 单字：**只收"常用汉字区（U+4E00–U+9FFF）里、且在词表中出现过"的字**。
    //
    // 为什么不收 `pinyin.txt` 的全部 44k 字：那个文件按码位排序，而码位
    // 靠前的是一大片极少用的扩展区字（`㐀` `㐁` `㐄`…）。收进来会白白撑大
    // 音节表与词库，还要给它们编权重——而它们的权重我们**没有数据**。
    // 以"出现在词里"为准，等于让上游词频替我们做常用字筛选，有据可依。
    let mut in_words: BTreeSet<char> = BTreeSet::new();
    for (text, _, _) in &lines {
        in_words.extend(text.chars());
    }
    let mut char_count = 0usize;
    for ch in table.readings.keys() {
        if char_count >= args.max_chars {
            break;
        }
        if !CJK_RANGE.contains(ch) || !in_words.contains(ch) {
            continue;
        }
        let Some(unit) = chosen.get(ch) else { continue };
        // 单字与词条可能撞（同一个字同一个音）——词条优先，跳过。
        if !seen.insert((ch.to_string(), unit.clone())) {
            continue;
        }
        syllables.insert(unit.clone());
        // 单字权重固定为 5000：**低于 jieba 里最常用的词**（`的` 等 30 万+），
        // 高于冷门词（词频个位数）。不同字之间的相对频率我们**没有数据**
        // （音节表不提供字频），编一个数字出来只会让排序看起来有依据。
        lines.push((ch.to_string(), unit.clone(), 5_000));
        char_count += 1;
    }

    eprintln!(
        "· 词条：{char_count} 单字 + {word_count} 词 = {} 条；丢弃 {lost_unknown} 条（含拼音表里没有的字）",
        lines.len()
    );
    eprintln!("· 音节表：{} 个编码单元", syllables.len());

    if args.dry_run {
        eprintln!("（--dry-run：不写文件）");
        return Ok(());
    }

    // ── 写词库 ──
    let mut sources_note = source_notes;
    sources_note.push("pinyin.txt（pinyin-data，MIT）—— 汉字读音".to_owned());
    sources_note.push(
        "OpenCC 的数据（Apache-2.0）在 build/opencc 与 build/emoji，由方案按 opencc_config 引用"
            .to_owned(),
    );
    if !warnings.is_empty() {
        sources_note.push(format!(
            "上游数据瑕疵 {} 条（已按规则容忍，逐条见生成时的 stderr）",
            warnings.len()
        ));
    }
    let mut out = dict_header(ReadingPolicy::CorpusFrequent, lines.len(), &sources_note);
    for (text, code, weight) in &lines {
        let _ = writeln!(out, "{text}\t{code}\t{weight}");
    }

    // **自己校验一遍再落盘**：生成器与装载器用同一个解析器
    // （`qingjian-dict`），于是"生成器写出来的东西装载器读不懂"这类错误
    // 在生成期就暴露，而不是等到用户 `--scheme-dir` 的时候。
    // 第一版就翻过车：YAML 头部的一个缩进被续行吃掉，装载器报
    // 「同一层里混用了「键: 值」与「- 列表项」」。
    let probe = qingjian_dict::parse_dict(&out, "generated.dict.yaml")
        .map_err(|e| format!("生成器自己写出来的词库读不回去（这是生成器的 bug）：{e}"))?;
    eprintln!(
        "· 自检：产出的词库能被装载器解析（{} 条，头部 name={}）",
        probe.entries.len(),
        probe.header.name
    );

    // 产物落在 `cn_dicts/` 下，与手写的 `base.dict.yaml` 并列：
    // 主词典（`pinyin.dict.yaml`）只做**导入清单**，于是"手写演示词库"
    // 与"生成词库"是两个独立文件，各自的来源一眼可见。
    let dict_path = args.out.join("cn_dicts").join("generated.dict.yaml");
    std::fs::write(&dict_path, &out).map_err(|e| format!("写不了 {}：{e}", dict_path.display()))?;
    eprintln!("✓ 写了 {}", dict_path.display());

    // ── 重写音节表 ──
    let schema_path = args.out.join("pinyin.schema.yaml");
    let schema = std::fs::read_to_string(&schema_path)
        .map_err(|e| format!("读不了 {}：{e}", schema_path.display()))?;
    let list: Vec<String> = syllables.into_iter().collect();
    let new_schema = rewrite_alphabet(&schema, &list)?;
    if new_schema == schema {
        eprintln!("· {} 的音节表没有变化", schema_path.display());
    } else {
        std::fs::write(&schema_path, &new_schema)
            .map_err(|e| format!("写不了 {}：{e}", schema_path.display()))?;
        eprintln!(
            "✓ 更新了 {} 的 `speller.alphabet`（{} 个音节）",
            schema_path.display(),
            list.len()
        );
    }
    Ok(())
}

/// 从目录读一份文件（`--dry-run` 与将来的扩展用）。
///
/// 抽出来是为了让"路径拼接"只有一处实现——`Path::join` 少写一次
/// 就会得到一个相对路径，而相对路径在测试里能过、在用户机器上读不到。
#[allow(dead_code)]
fn read_source(dir: &Path, name: &str) -> Result<String, String> {
    let p = dir.join(name);
    std::fs::read_to_string(&p).map_err(|e| format!("读不了 {}：{e}", p.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_handles_all_tone_marks_and_umlaut() {
        assert_eq!(normalize_syllable("nǚ"), "nv");
        assert_eq!(normalize_syllable("lüe"), "lve");
        assert_eq!(normalize_syllable("lǜ"), "lv");
        assert_eq!(normalize_syllable("hǎo"), "hao");
        assert_eq!(normalize_syllable("zhuāng"), "zhuang");
        assert_eq!(normalize_syllable("ér"), "er");
        assert_eq!(normalize_syllable("ng"), "ng");
    }

    #[test]
    fn parse_pinyin_reads_codepoints_and_readings() {
        let t = "# version: 0.15.0\nU+5973: nǚ,nǜ,rǔ  # 女\nU+4E00: yī\n";
        let p = parse_pinyin(t).unwrap();
        // nǚ 与 nǜ 无调形式相同 → 只留一个。
        assert_eq!(p.readings[&'女'], ["nv", "ru"]);
        assert_eq!(p.readings[&'一'], ["yi"]);
        assert_eq!(p.multi_reading, 1);
    }

    #[test]
    fn parse_pinyin_rejects_a_bad_line_with_its_number() {
        let e = parse_pinyin("U+4E00 yī\n").unwrap_err();
        assert!(e.contains("第 1 行"), "{e}");
    }

    #[test]
    fn parse_wordlist_reads_thuocl_tab_rows() {
        let mut warn = Vec::new();
        let w =
            parse_wordlist("毛泽东\t61678\n胡锦涛\t58429\n", "THUOCL_x.txt", &mut warn).unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].text, "毛泽东");
        assert_eq!(w[0].freq, 61678);
        assert!(warn.is_empty());
    }

    #[test]
    fn parse_wordlist_reads_jieba_space_rows() {
        // jieba 的格式是 `词 词频 词性`（空格分隔），第三列忽略。
        let mut warn = Vec::new();
        let w = parse_wordlist("你好 725 l\n世界 34387 n\n", "jieba_dict.txt", &mut warn).unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].text, "你好");
        assert_eq!(w[0].freq, 725);
        assert!(warn.is_empty(), "jieba 的第三列不是瑕疵，不该报警告");
    }

    #[test]
    fn parse_wordlist_rejects_a_row_with_no_separator() {
        let mut warn = Vec::new();
        let e = parse_wordlist("毛泽东61678\n", "t.txt", &mut warn).unwrap_err();
        assert!(e.contains("TAB"), "{e}");
    }

    #[test]
    fn traditional_chars_are_detected_and_filtered() {
        // `OpenCC` 的 `TSCharacters.txt`：`繁<TAB>简`。
        let text = "# 表头\n個\t个\n號\t号\n人\t人\n";
        let trad = parse_traditional_chars(text, "TSCharacters.txt").unwrap();
        // 映射到自己的（人→人）不算繁体。
        assert!(trad.contains(&'個'));
        assert!(!trad.contains(&'人'));
        assert!(has_traditional("1號店", &trad));
        assert!(!has_traditional("4S店", &trad));
    }

    #[test]
    fn parse_traditional_chars_rejects_malformed_rows() {
        let e = parse_traditional_chars("個 个\n", "t.txt").unwrap_err();
        assert!(e.contains("TAB"), "{e}");
    }

    #[test]
    fn initial_of_handles_digraphs_and_zero_initials() {
        assert_eq!(initial_of("zhuang"), "zh");
        assert_eq!(initial_of("chang"), "ch");
        assert_eq!(initial_of("shui"), "sh");
        assert_eq!(initial_of("zang"), "z");
        assert_eq!(initial_of("cai"), "c");
        assert_eq!(initial_of("sui"), "s");
        assert_eq!(initial_of("ni"), "n");
        assert_eq!(initial_of("hao"), "h");
        // 零声母（含 a/e/o 开头的与 y/w 开头的）。
        assert_eq!(initial_of("an"), "");
        assert_eq!(initial_of("er"), "");
        assert_eq!(initial_of("yi"), "");
        assert_eq!(initial_of("wu"), "");
    }

    #[test]
    fn polyphone_reading_follows_only_same_initial_evidence() {
        // 「血」的候选：xuè（主）/ xiě —— **同声母 x**，因此允许微调。
        // 「行」的候选：xíng（主）/ háng —— **声母不同**，因此不动。
        //
        // 单字表里 `xie` 出现在很多单音字上（些/写/谢…），`xue` 只在「血」上，
        // 于是「血」取 xiě。这正是这条启发式能做的事，也是它的边界：
        // 它**不会**把「银行」读成 yín háng。
        let p = parse_pinyin(
            "U+8840: xuè,xiě\nU+4E9B: xiē\nU+5199: xiě\nU+8C22: xiè\n\
             U+884C: xíng,háng\nU+94F6: yín\n",
        )
        .unwrap();
        let chosen = choose_readings(&p, &[], ReadingPolicy::CorpusFrequent);
        assert_eq!(chosen[&'血'], "xie", "同声母的候选应当按单字表证据胜出");
        assert_eq!(chosen[&'行'], "xing", "声母不同的候选不在调整范围内");

        // `First` 策略一律取列表首个。
        let first = choose_readings(&p, &[], ReadingPolicy::First);
        assert_eq!(first[&'血'], "xue");
        assert_eq!(first[&'行'], "xing");
    }

    #[test]
    fn unknown_characters_drop_the_whole_word() {
        let p = parse_pinyin("U+4E00: yī\n").unwrap();
        let chosen = choose_readings(&p, &[], ReadingPolicy::First);
        assert!(to_code("一", &chosen).is_some());
        // 「丁」不在表里 → 整条丢弃，而不是把汉字当拼音。
        assert!(to_code("一丁", &chosen).is_none());
    }

    #[test]
    fn rewrite_alphabet_replaces_only_that_block() {
        let schema = "speller:\n  delimiter: \"'\"\n  alphabet:\n    - ni\n    - hao\n  rules:\n    - abbrev: { take: 1, weight: 0.5 }\ntranslator:\n  dictionary: pinyin\n";
        let out = rewrite_alphabet(schema, &["ba".into(), "ni".into()]).unwrap();
        assert!(out.contains("    - ba\n    - ni\n"), "{out}");
        assert!(!out.contains("    - hao"), "{out}");
        // 其它行必须原样保留。
        assert!(out.contains("  delimiter: \"'\"\n"));
        assert!(out.contains("  rules:\n    - abbrev: { take: 1, weight: 0.5 }\n"));
        assert!(out.contains("translator:\n  dictionary: pinyin\n"));
    }

    #[test]
    fn rewrite_alphabet_fails_loudly_when_the_block_is_missing() {
        let e = rewrite_alphabet("speller:\n  rules: []\n", &[]).unwrap_err();
        assert!(e.contains("alphabet"), "{e}");
    }

    #[test]
    fn generated_header_is_parseable_by_the_real_loader() {
        // 这条测试是"缩进被续行吃掉"那个 bug 的守卫：它拿**真正的装载器**
        // 去解析生成的头部，而不是拿眼睛看。
        let header = dict_header(ReadingPolicy::First, 1, &["测试来源".to_owned()]);
        let text = format!("{header}甲\tyi\t1\n");
        let parsed = qingjian_dict::parse_dict(&text, "t.dict.yaml").unwrap();
        assert_eq!(parsed.header.name, "generated");
        assert!(parsed.header.import_tables.is_empty());
        assert_eq!(parsed.entries.len(), 1);
    }

    #[test]
    fn dict_header_names_every_source() {
        let h = dict_header(
            ReadingPolicy::CorpusFrequent,
            3,
            &["pinyin.txt（pinyin-data，MIT）".to_owned()],
        );
        assert!(h.contains("pinyin-data"));
        assert!(h.contains("词条数：3"));
        assert!(h.contains("name: generated"));
    }
}
