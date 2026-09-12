//! # stele-bench — 称重台
//!
//! 中文职责：测量并输出内存与延迟数字，让 PLAN §0.2 的硬指标可被验收。
//! English role: measure and report memory and latency, so the hard targets in
//! PLAN §0.2 can actually be checked.
//! 架构位置：P0 交付物；P1 起开始测**真实管线**（PLAN §9）。
//!
//! # 没有它，红线只是一句愿望
//!
//! "内存 ≤ RIME、速度 ≥ RIME"是项目的存在理由之一，而**不可测的目标等于没有目标**。
//!
//! 输出三组数字：
//!
//! 1. **测量框架自身的开销下限**——它给出"引擎成本"的参照。
//! 2. **内核排序**（200 个同分候选）——候选量很大时的那一步。
//! 3. **真实按键路径**——`process_key` 全流程（处理器 → 解析 → 翻译 → 过滤 → 排序）。

use std::fmt::Write as _;
use std::time::Instant;
use stele_core::{Engine, Key, KeyCode, Modifiers, NamedKey, Outcome};

/// 一次测量的结果。
struct Report {
    /// 观测次数。
    samples: usize,
    /// 关键分位数（**纳秒**）。
    ///
    /// 为什么用纳秒：release 构建下按键路径在**亚微秒**量级，
    /// 微秒分辨率会把所有数字显示成 0 —— 一把太粗的尺子等于没有尺子。
    p50_ns: u64,
    p95_ns: u64,
    p99_ns: u64,
    max_ns: u64,
    /// 常驻内存（KiB）。
    rss_kib: u64,
}

/// 读取常驻内存（KiB）。
///
/// 从 `/proc/self/status` 的 `VmRSS` 读——比 `statm` 好，因为它已经是 KB，
/// 不必假设页大小。非 Linux 平台返回 `None`（**我们不猜**）。
fn rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let num: String = rest.chars().filter(char::is_ascii_digit).collect();
            return num.parse().ok();
        }
    }
    None
}

/// 计算分位数（`sorted` 必须是微秒且已排序）。
fn percentile(sorted: &[u64], pct: u32) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let n = sorted.len() as u64;
    let rank = (u64::from(pct) * n).div_ceil(100).max(1);
    let idx = usize::try_from(rank - 1).unwrap_or(0).min(sorted.len() - 1);
    sorted[idx]
}

/// 跑一轮测量。
fn measure<F: FnMut()>(iterations: usize, mut workload: F) -> Report {
    let mut samples: Vec<u64> = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let t0 = Instant::now();
        workload();
        samples.push(u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX));
    }
    samples.sort_unstable();
    Report {
        samples: samples.len(),
        p50_ns: percentile(&samples, 50),
        p95_ns: percentile(&samples, 95),
        p99_ns: percentile(&samples, 99),
        max_ns: samples.last().copied().unwrap_or(0),
        rss_kib: rss_kib().unwrap_or(0),
    }
}

/// 把纳秒格式化成人类可读的串。
///
/// **不四舍五入到整数微秒**——那样会把亚微秒的数字全变成 `0µs`，
/// 读起来像是没测到东西。
#[must_use]
fn human_ns(ns: u64) -> String {
    // 精度损失在这里无害：这是**给人看的显示**，不是参与排序的分数。
    // （真正要求精度的分数用的是定点整数 `Score`，不是浮点。）
    #[allow(clippy::cast_precision_loss)]
    fn as_f64(v: u64) -> f64 {
        v as f64
    }
    if ns < 1_000 {
        format!("{ns}ns")
    } else if ns < 1_000_000 {
        format!("{:.2}µs", as_f64(ns) / 1_000.0)
    } else {
        format!("{:.2}ms", as_f64(ns) / 1_000_000.0)
    }
}

fn main() {
    let t_start = Instant::now();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let as_json = args.iter().any(|a| a == "--json");
    let iterations: usize = args
        .iter()
        .find_map(|a| a.strip_prefix("--iterations="))
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000);

    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "stele-bench — 称重台\n\n\
             用法：stele-bench [--iterations=N] [--json]\n\n\
             测量：空转（框架开销下限）/ 内核排序 / 真实按键路径。"
        );
        return;
    }

    // ── 工作负载 1：空转。给出测量框架自身的开销下限。 ──
    let idle = measure(iterations, || {
        std::hint::black_box(1_u64.wrapping_add(1));
    });

    // ── 工作负载 2：内核排序（200 个同分候选）──
    let mut buffer: Vec<stele_core::Candidate> = (0..200)
        .map(|i| stele_core::Candidate {
            text: format!("候选{i}"),
            comment: None,
            score: stele_core::Score::from_milli_log(1000),
            origin: stele_core::Origin::SystemWord,
            attr: stele_core::SpellingAttr::NORMAL,
            span: stele_core::Span::new(0, 1),
            lane: stele_core::Lane::Input,
        })
        .collect();
    let sort_report = measure(iterations.min(20_000), || {
        stele_core::sort_candidates(&mut buffer);
    });

    // ── 工作负载 3：真实按键路径（P1 起）──
    //
    // 装载一次引擎（这是"冷启动"的主要成本），然后反复敲键。
    // 每敲满一轮就 reset，使输入长度有界——否则测到的是"输入越来越长"的曲线，
    // 而不是单键成本。
    let t_load = Instant::now();
    let defs = stele_schemes::all().expect("内嵌方案必须能装载 —— 失败说明打包坏了");
    let engine = stele_engine::EngineImpl::new(&defs).expect("默认方案应当能编译");
    let load_us = u64::try_from(t_load.elapsed().as_micros()).unwrap_or(u64::MAX);

    let mut session = engine.create_session();
    let cycle: Vec<char> = "nihao".chars().collect();
    let mut i: usize = 0;
    let pipeline = measure(iterations.min(50_000), || {
        if i > 0 && i % cycle.len() == 0 {
            session.reset();
        }
        let c = cycle[i % cycle.len()];
        i += 1;
        session.process_key(Key::ch(c));
    });

    // 顺带验证引擎确实在工作（而不是在测一个空壳）。
    let mut probe = engine.create_session();
    for c in "nihao".chars() {
        probe.process_key(Key::ch(c));
    }
    let space = Key::press(KeyCode::Named(NamedKey::Space), Modifiers::NONE);
    let committed = match probe.process_key(space) {
        Outcome::Committed(c) => c.text,
        other => format!("<未上屏: {other:?}>"),
    };

    let startup_us = u64::try_from(t_start.elapsed().as_micros()).unwrap_or(u64::MAX);

    if as_json {
        let mut out = String::new();
        let _ = write!(
            out,
            "{{\"idle\":{{\"samples\":{},\"p50_ns\":{},\"p95_ns\":{},\"p99_ns\":{},\"max_ns\":{}}},\
             \"sort200\":{{\"samples\":{},\"p50_ns\":{},\"p95_ns\":{},\"p99_ns\":{},\"max_ns\":{}}},\
             \"keypath\":{{\"samples\":{},\"p50_ns\":{},\"p95_ns\":{},\"p99_ns\":{},\"max_ns\":{}}},\
             \"rss_kib\":{},\"engine_load_us\":{},\"process_us\":{},\"probe_commit\":\"{}\"}}",
            idle.samples,
            idle.p50_ns,
            idle.p95_ns,
            idle.p99_ns,
            idle.max_ns,
            sort_report.samples,
            sort_report.p50_ns,
            sort_report.p95_ns,
            sort_report.p99_ns,
            sort_report.max_ns,
            pipeline.samples,
            pipeline.p50_ns,
            pipeline.p95_ns,
            pipeline.p99_ns,
            pipeline.max_ns,
            pipeline.rss_kib,
            load_us,
            startup_us,
            committed
        );
        println!("{out}");
        return;
    }

    println!("Stele-IME 称重台");
    println!("========================================");
    println!("自检：敲 nihao 后上屏 「{committed}」");
    println!();
    println!(
        "{:<28} {:>11} {:>11} {:>11} {:>11}",
        "工作负载", "P50", "P95", "P99", "max"
    );
    println!(
        "{:-<28} {:->11} {:->11} {:->11} {:->11}",
        "", "", "", "", ""
    );
    let row = |name: &str, r: &Report| {
        println!(
            "{:<28} {:>11} {:>11} {:>11} {:>11}",
            name,
            human_ns(r.p50_ns),
            human_ns(r.p95_ns),
            human_ns(r.p99_ns),
            human_ns(r.max_ns)
        );
    };
    row(&format!("空转（框架下限, {}）", idle.samples), &idle);
    row(
        &format!("内核排序 200 候选（{}）", sort_report.samples),
        &sort_report,
    );
    row(&format!("真实按键路径（{}）", pipeline.samples), &pipeline);
    println!();
    println!(
        "常驻内存（VmRSS）  : {} KiB ({} MiB)",
        pipeline.rss_kib,
        pipeline.rss_kib / 1024
    );
    println!("引擎装载（含两方案）: {load_us} µs");
    println!("进程启动到测量开始 : {startup_us} µs");
    println!();
    println!("PLAN §0.2 的硬指标：");
    println!(
        "  按键延迟 P50 < 1 ms   → 实测 {}  {}",
        human_ns(pipeline.p50_ns),
        if pipeline.p50_ns < 1_000_000 {
            "✓"
        } else {
            "✗"
        }
    );
    println!(
        "  按键延迟 P99 < 10 ms  → 实测 {}  {}",
        human_ns(pipeline.p99_ns),
        if pipeline.p99_ns < 10_000_000 {
            "✓"
        } else {
            "✗"
        }
    );
    println!(
        "  常驻内存 < 30 MB      → 实测 {} MiB  {}",
        pipeline.rss_kib / 1024,
        if pipeline.rss_kib / 1024 < 30 {
            "✓"
        } else {
            "✗"
        }
    );
    println!();
    println!("说明：");
    println!("  · 「空转」给出**测量框架自身的开销下限**——按键路径减去它就是引擎的成本。");
    println!("  · 用 `--release` 运行，否则看到的是未优化代码的耗时。");
    println!("  · 冷启动的端到端数字需要真实前端（TSF / Android），属 P6/P7。");
    println!();
    println!("⚠️ 这些数字**只反映管线本身的开销**：演示词库只有几十条词，");
    println!("   词库查找几乎不花时间。真实词库（几十万条）的成本要到 P2.5 才测得到。");
}
