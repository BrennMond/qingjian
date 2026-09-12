//! # stele-bench — 称重台
//!
//! 中文职责：测量并输出内存与延迟数字，让 PLAN §0.2 的硬指标可被验收。
//! English role: measure and report memory and latency, so the hard targets in
//! PLAN §0.2 can actually be checked.
//! 架构位置：P0 交付物（PLAN §9）。**没有它，红线只是一句愿望。**
//!
//! # P0 阶段测的是什么（诚实说明）
//!
//! 引擎本体是 P1 的内容，所以目前**没有真实的按键路径可测**。
//! 本程序测的是**称重台本身**：测量框架的开销、内存读取的可靠性、
//! 以及一个可复现的基线数字。
//!
//! 这样做的价值在于：等 P1 把真实管线接上来时，我们**已经有一把校准过的尺子**，
//! 而不是到时候才发现测量方法本身有问题。

use std::fmt::Write as _;
use std::time::Instant;

/// 一次测量的结果。
struct Report {
    /// 观测次数。
    samples: usize,
    /// 关键分位数。
    p50_us: u64,
    p95_us: u64,
    p99_us: u64,
    max_us: u64,
    /// 常驻内存（KiB）。
    rss_kib: u64,
}

/// 读取常驻内存（KiB）。
///
/// 从 `/proc/self/status` 的 `VmRSS` 读——比 `statm` 好，因为它已经是 KB，
/// 不必假设页大小。非 Linux 平台返回 `None`（我们不猜）。
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

/// 计算分位数（输入的 `durations` 必须是微秒且已排序）。
fn percentile(sorted: &[u64], pct: u32) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    // 最近秩法：取 ceil(pct/100 * n) - 1。
    let n = sorted.len() as u64;
    let rank = (u64::from(pct) * n).div_ceil(100).max(1);
    let idx = usize::try_from(rank - 1).unwrap_or(0).min(sorted.len() - 1);
    sorted[idx]
}

/// 跑一轮测量。
///
/// # Arguments / 参数
/// * `iterations` — 观测次数。
/// * `workload` — 每次迭代要做的事。P1 会把真实的按键处理放进来。
///
/// # Returns / 返回
/// 测量报告；不会失败。
fn measure<F: FnMut()>(iterations: usize, mut workload: F) -> Report {
    let mut samples: Vec<u64> = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let t0 = Instant::now();
        workload();
        samples.push(u64::try_from(t0.elapsed().as_micros()).unwrap_or(u64::MAX));
    }
    samples.sort_unstable();
    let rss_now = rss_kib().unwrap_or(0);
    Report {
        samples: samples.len(),
        p50_us: percentile(&samples, 50),
        p95_us: percentile(&samples, 95),
        p99_us: percentile(&samples, 99),
        max_us: samples.last().copied().unwrap_or(0),
        rss_kib: rss_now,
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
             P0 阶段测量的是称重台自身（引擎管线是 P1 的内容）。"
        );
        return;
    }

    // ── 工作负载 1：空转。给出测量框架自身的开销下限。 ──
    let idle = measure(iterations, || {
        std::hint::black_box(1_u64.wrapping_add(1));
    });

    // ── 工作负载 2：内核的一次典型排序（200 个候选，全部同分）。 ──
    // 这是真实管线里"按键 → 候选排序"那一步的量级。
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
    let sort_iters = iterations.min(20_000);
    let sorted = measure(sort_iters, || {
        stele_core::sort_candidates(&mut buffer);
    });

    let startup_us = u64::try_from(t_start.elapsed().as_micros()).unwrap_or(u64::MAX);

    if as_json {
        let mut out = String::new();
        let _ = write!(
            out,
            "{{\"idle\":{{\"samples\":{},\"p50_us\":{},\"p95_us\":{},\"p99_us\":{},\"max_us\":{}}},\
             \"sort200\":{{\"samples\":{},\"p50_us\":{},\"p95_us\":{},\"p99_us\":{},\"max_us\":{}}},\
             \"rss_kib\":{},\"startup_us\":{}}}",
            idle.samples, idle.p50_us, idle.p95_us, idle.p99_us, idle.max_us,
            sorted.samples, sorted.p50_us, sorted.p95_us, sorted.p99_us, sorted.max_us,
            idle.rss_kib, startup_us
        );
        println!("{out}");
        return;
    }

    println!("Stele-IME 称重台（P0 基线）");
    println!("========================================");
    println!("注意：引擎管线是 P1 的内容；下面测的是测量框架与内核排序。");
    println!();
    println!(
        "{:<24} {:>10} {:>10} {:>10} {:>10}",
        "工作负载", "P50", "P95", "P99", "max"
    );
    println!(
        "{:-<24} {:->10} {:->10} {:->10} {:->10}",
        "", "", "", "", ""
    );
    let row = |name: &str, r: &Report| {
        println!(
            "{:<24} {:>9}µs {:>9}µs {:>9}µs {:>9}µs",
            name, r.p50_us, r.p95_us, r.p99_us, r.max_us
        );
    };
    row(&format!("空转（{} 次）", idle.samples), &idle);
    row(&format!("200 候选排序（{} 次）", sorted.samples), &sorted);
    println!();
    println!(
        "常驻内存（VmRSS） : {} KiB ({} MiB)",
        idle.rss_kib,
        idle.rss_kib / 1024
    );
    println!("启动到测量开始    : {startup_us} µs");
    println!();
    println!("PLAN §0.2 的硬指标（供对照，尚未测到真实管线）：");
    println!("  按键延迟 P50 < 1 ms     常驻内存 < 30 MB     冷启动 < 100 ms");
    println!();
    println!("说明：这里测的空转给出的是**测量框架自身的开销下限**——");
    println!("真实按键处理只要在这个量级之上，差额就是引擎的成本。");
}
