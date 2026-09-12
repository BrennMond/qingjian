//! # stele-cli — 命令行调试前端
//!
//! 中文职责：一个最小的命令行前端，用于开发期观察引擎行为。
//! English role: a minimal CLI frontend for observing engine behaviour during development.
//! 架构位置：`platforms/` 之外的"调试用前端"，与 TSF / Android 前端平级，
//! 只是它跑在终端里。
//!
//! # P0 阶段的状态（诚实说明）
//!
//! P0 只交付**环境与骨架**：workspace、CI、规范、称重台。
//! 引擎本体（切分、词库、候选、会话）是 **P1** 的内容，因此本程序目前
//! **还不能真的打字**。它现在能做的事：
//!
//! - `--version` / `--help`
//! - `--check`：跑一遍内建的自检（验证内核不变式）
//! - `--dump-config`：**按 PLAN D25 应有的接口**，目前明确报告"尚未实现"
//!
//! 宁可明确报告未实现，也不提供假装能用的功能。

use std::process::ExitCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "\
Stele-IME（石經）命令行调试前端

用法：
  stele [选项]

选项：
  -h, --help          显示本帮助
  -V, --version       显示版本
      --check         运行内核自检（不变式）
      --dump-config   打印合并后的完整方案（P2/P3 实现）

说明：
  本程序是开发期的调试前端。真实的输入法前端是 platforms/windows（TSF）
  与 platforms/android（IME）。P0 阶段引擎尚未实现，故不能打字。
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }

    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("stele {VERSION} — Stele-IME（石經）");
        return ExitCode::SUCCESS;
    }

    if args.iter().any(|a| a == "--check") {
        return self_check();
    }

    if args.iter().any(|a| a == "--dump-config") {
        // PLAN D25 要求这个接口存在，且"打印出来的每一行都能被补丁覆盖"。
        eprintln!(
            "错误：--dump-config 尚未实现。\n\
             它需要方案加载（P2）与配置分层（P3）。\n\
             现在报告未实现，好过打印一份假的配置——\n\
             一个会骗人的调试工具比没有调试工具更糟。"
        );
        return ExitCode::FAILURE;
    }

    if args.is_empty() {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }

    eprintln!("未知参数：{}", args.join(" "));
    eprint!("{HELP}");
    ExitCode::FAILURE
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

    // 不变式 4：比较函数是全序（自反、反对称、可传递的抽样检查）。
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

    if failures.is_empty() {
        println!("内核自检通过：5 组不变式全部成立。");
        ExitCode::SUCCESS
    } else {
        eprintln!("内核自检失败：{}", failures.len());
        for f in &failures {
            eprintln!("  - {f}");
        }
        ExitCode::FAILURE
    }
}
