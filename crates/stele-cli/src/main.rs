//! # stele-cli — 命令行调试前端
//!
//! 中文职责：把按键序列喂给引擎，观察候选与上屏结果。
//! English role: feed a key sequence to the engine and observe candidates and commits.
//! 架构位置：`platforms/` 之外的"调试用前端"，与 TSF / Android 前端平级，
//! 只是它跑在终端里。
//!
//! # 它同时是"通用性"的活体验证
//!
//! `--schema` 可以切到任何一个已装载的方案，而**引擎代码完全一样**：
//!
//! ```text
//! $ stele --schema pinyin-demo nihao      → 你好（规范拼写）
//! $ stele --schema pinyin-demo nh         → 你好（简拼，分数更低）
//! $ stele --schema shape-demo ab          → 十（精确编码，无拼写运算）
//! ```
//!
//! 第二条与第三条分别落在两族翻译器上。**这比任何文档声明都有说服力**——
//! 见 `PLAN.md` D33。

use std::process::ExitCode;
use stele_core::{is_exact, Engine, Key, KeyCode, Modifiers, NamedKey, Outcome};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "\
Stele-IME（石经）命令行调试前端

用法：
  stele [选项] <按键序列>

选项：
  -h, --help            显示本帮助
  -V, --version         显示版本
      --list            列出已装载的方案
      --schema <id>     选择方案（默认第一个）
      --scheme-dir <p>  从目录装载方案（不指定则用内嵌的默认方案）
      --candidates      打印候选列表，而不只是上屏结果
      --check           运行内核自检（不变式）
      --dump-config     打印合并后的完整方案（P2/P3 实现）

示例：
  stele nihao                   拼音：上屏「你好」
  stele nh                      拼音：简拼也上屏「你好」（分数更低）
  stele --candidates ni         看候选列表（含分数 / 来源 / 属性）
  stele --schema shape ab       精确编码方案：上屏「十」
  stele --scheme-dir ./my-schemes --list    装载自己的方案目录

说明：
  本程序是开发期的调试前端。真实的输入法前端是 platforms/windows（TSF）
  与 platforms/android（IME）。
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("stele {VERSION} — Stele-IME（石经）");
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--check") {
        return self_check();
    }
    if args.iter().any(|a| a == "--dump-config") {
        eprintln!(
            "错误：--dump-config 尚未实现。\n\
             它需要方案加载（P2）与配置分层（P3）。\n\
             现在报告未实现，好过打印一份假的配置——\n\
             一个会骗人的调试工具比没有调试工具更糟。"
        );
        return ExitCode::FAILURE;
    }

    // 方案来源：`--scheme-dir` 指定的目录，否则是**内嵌的 YAML**
    // （单一数据来源，因此两种路径走的都是同一个解析器）。
    let scheme_dir = args
        .iter()
        .position(|a| a == "--scheme-dir")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let defs = match &scheme_dir {
        // 目录装载走**部署路径**：词库编译成紧凑产物，按需分页地读。
        // 内嵌方案只有几十条词，用内存表更快，所以两条路各走各的。
        Some(dir) => {
            let root = std::path::Path::new(dir);
            stele_schemes::load_dir_deployed(root, &root.join(".stele-cache"))
        }
        None => stele_schemes::all(),
    };
    let defs = match defs {
        Ok(d) => d,
        Err(e) => {
            eprintln!("装载方案失败：{e}");
            return ExitCode::FAILURE;
        }
    };
    let engine = match stele_engine::EngineImpl::new(&defs) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("编译默认方案失败：{e}");
            return ExitCode::FAILURE;
        }
    };

    if args.iter().any(|a| a == "--list") {
        for info in engine.schemas().list() {
            println!(
                "{:<14} {:<26} family={}",
                info.schema_id,
                info.name,
                info.family.as_deref().unwrap_or("-")
            );
        }
        return ExitCode::SUCCESS;
    }

    let schema_pos = args.iter().position(|a| a == "--schema");
    let schema_id = schema_pos.and_then(|i| args.get(i + 1)).cloned();
    let dir_pos = args.iter().position(|a| a == "--scheme-dir");
    let show_candidates = args.iter().any(|a| a == "--candidates");

    // 按键序列 = 所有不以 `-` 开头、且不是某个**选项之值**的位置参数。
    // 漏掉任何一个选项都会让它的值被当成按键打出去 —— 这就是下面那句注释存在的理由。
    let keys: String = args
        .iter()
        .enumerate()
        .filter(|(i, a)| {
            let is_option_value = [schema_pos, dir_pos]
                .into_iter()
                .flatten()
                .any(|p| *i == p + 1);
            !a.starts_with('-') && !is_option_value
        })
        .map(|(_, a)| a.as_str())
        .collect();

    let mut session = engine.create_session();

    if let Some(id) = schema_id {
        if let Err(e) = session.switch_schema(&id) {
            eprintln!("切换方案失败：{e}");
            return ExitCode::FAILURE;
        }
    }

    if keys.is_empty() {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }

    // ── 按键 ──
    for c in keys.chars() {
        session.process_key(Key::ch(c));
    }

    if show_candidates {
        println!(
            "方案 {}  输入 {:?}  候选 {} 个",
            session.schema_id(),
            session.composition().input,
            session.candidates().len()
        );
        for (i, c) in session.candidates().iter().enumerate() {
            println!(
                "  {}. {:<8} score={:<9} origin={:<11} attr={:<9} lane={:?} {}",
                i + 1,
                c.text,
                c.score.as_milli_log(),
                format!("{:?}", c.origin),
                format!("{:?}", c.attr),
                c.lane,
                if is_exact(c.origin, c.attr) {
                    ""
                } else {
                    "← 猜的"
                }
            );
        }
        return ExitCode::SUCCESS;
    }

    // ── 提交 ──
    let space = Key::press(KeyCode::Named(NamedKey::Space), Modifiers::NONE);
    match session.process_key(space) {
        Outcome::Committed(commit) => {
            println!("{}", commit.text);

            let mut events = Vec::new();
            session.drain_events(&mut events);
            for e in &events {
                if let stele_core::Event::Learned {
                    input, text, attr, ..
                } = e
                {
                    eprintln!(
                        "  [学习] 输入 {:?} → {:?}（属性 {:?}{}）",
                        input,
                        text,
                        attr,
                        if attr.is_derived() {
                            "，落库前必须先规范化成规范编码"
                        } else {
                            ""
                        }
                    );
                }
            }
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("未能上屏：{other:?}");
            ExitCode::FAILURE
        }
    }
}

/// 内核自检：验证那些"一旦破坏就会污染全部输出"的不变式。
///
/// 这些检查刻意放在 CLI 里而不是只放在测试里——**用户和贡献者都应该能
/// 一条命令确认内核还是健全的**。
fn self_check() -> ExitCode {
    use stele_core::{
        clamp_bonus, compare, is_exact, sort_candidates, Candidate, Lane, Origin, Score, Span,
        SpellingAttr,
    };

    let mut failures: Vec<String> = Vec::new();

    // 不变式 1：分数是对数域定点整数，单调且无 NaN。
    if Score::from_weight(1000.0) <= Score::from_weight(1.0) {
        failures.push("分数不再随权重单调".into());
    }
    if Score::from_weight(f64::NAN) != Score::FLOOR {
        failures.push("NaN 权重没有被钳到下界".into());
    }

    // 不变式 2：精确优先的判据由两个轴共同决定。
    if !is_exact(Origin::SystemWord, SpellingAttr::NORMAL) {
        failures.push("规范拼写的系统词未被判为精确".into());
    }
    if is_exact(Origin::SystemWord, SpellingAttr::ABBREV) {
        failures.push("简拼派生的词被误判为精确".into());
    }

    // 不变式 3：排序是全序且可复现。
    let mk = |text: &str, ml: i32, origin: Origin| Candidate {
        text: text.to_owned(),
        comment: None,
        score: Score::from_milli_log(ml),
        origin,
        attr: SpellingAttr::NORMAL,
        span: Span::new(0, 1),
        lane: Lane::Input,
    };
    // 输入顺序特意打乱，让平局规则必须真的起作用。
    let build = || {
        vec![
            mk("b_sys", 1, Origin::SystemWord),
            mk("a_user", 1, Origin::UserWord),
            mk("c_sys", 1, Origin::SystemWord),
        ]
    };
    let mut first: Option<Vec<String>> = None;
    for _ in 0..64 {
        let mut v = build();
        sort_candidates(&mut v);
        let got: Vec<String> = v.into_iter().map(|c| c.text).collect();
        match &first {
            None => first = Some(got),
            Some(f) if f != &got => {
                failures.push("排序结果不可复现".into());
                break;
            }
            Some(_) => {}
        }
    }
    let want = ["a_user".to_owned(), "b_sys".to_owned(), "c_sys".to_owned()];
    if first.as_deref() != Some(&want) {
        failures.push("平局规则不符合预期（应为 origin 优先，再按插入序）".into());
    }

    // 不变式 4：比较函数是确定的。
    let a = mk("x", 5, Origin::SystemWord);
    let b = mk("y", 5, Origin::SystemWord);
    if compare(&a, &b) != compare(&a, &b) {
        failures.push("比较函数不确定".into());
    }

    // 不变式 5：重排器的加成被上界钳住（"结构保证优于运行期修正"）。
    let base = Score::from_milli_log(1000);
    let limit = Score::from_milli_log(2000);
    if clamp_bonus(base, Score::from_milli_log(9999), limit) != Score::from_milli_log(3000) {
        failures.push("重排器加成未被钳到上界".into());
    }

    // 不变式 6（P1 新增）：两族翻译器都必须可用 —— 这是 D33 的通用性保证。
    let defs = stele_schemes::all().unwrap_or_default();
    if !stele_schemes::uses_both_translator_families(&defs) {
        failures.push("内置方案不再覆盖两族翻译器（D33 的通用性保证失效）".into());
    }

    // 不变式 7（P1 新增）：同一个引擎必须能跑两种完全不同的输入法。
    if let Err(e) = engine_smoke_test() {
        failures.push(e);
    }

    if failures.is_empty() {
        println!("内核自检通过：7 组不变式全部成立。");
        ExitCode::SUCCESS
    } else {
        eprintln!("内核自检失败：{}", failures.len());
        for f in &failures {
            eprintln!("  - {f}");
        }
        ExitCode::FAILURE
    }
}

/// 端到端冒烟：用**同一个引擎**跑两个方案，覆盖两族翻译器。
fn engine_smoke_test() -> Result<(), String> {
    let defs = stele_schemes::all().map_err(|e| format!("默认方案装载失败：{e}"))?;
    let engine =
        stele_engine::EngineImpl::new(&defs).map_err(|e| format!("默认方案编译失败：{e}"))?;

    let run = |schema: &str, keys: &str| -> Result<String, String> {
        let mut s = engine.create_session();
        s.switch_schema(schema)
            .map_err(|e| format!("切换 {schema} 失败：{e}"))?;
        for c in keys.chars() {
            s.process_key(Key::ch(c));
        }
        let space = Key::press(KeyCode::Named(NamedKey::Space), Modifiers::NONE);
        match s.process_key(space) {
            Outcome::Committed(c) => Ok(c.text),
            other => Err(format!("{schema}/{keys} 未能上屏：{other:?}")),
        }
    };

    // ① 拼写图族：规范拼写。
    let a = run("pinyin", "nihao")?;
    if a != "你好" {
        return Err(format!("pinyin-demo/nihao 应当上屏「你好」，得到「{a}」"));
    }
    // ② 拼写图族：变体拼写（简拼）。
    let b = run("pinyin", "nh")?;
    if b != "你好" {
        return Err(format!("pinyin-demo/nh 应当上屏「你好」，得到「{b}」"));
    }
    // ③ 精确编码族：完全不同的输入法，同一个引擎。
    let c = run("shape", "ab")?;
    if c != "十" {
        return Err(format!("shape-demo/ab 应当上屏「十」，得到「{c}」"));
    }
    Ok(())
}
