//! # embed-probe — P5（向量偏好重排）的内存预算探针
//!
//! 中文职责：在没有模型的前提下，量出"一张向量表要占多少内存、一次重排要花多少
//! 时间"。它回答的是 P5 的**门槛问题**，不是 P5 本身。
//! English role: measure the memory and latency envelope of a hypothetical
//! embedding table — the gate question for P5, not P5 itself.
//!
//! # 它为什么存在
//!
//! PLAN D19 给 P5 写了一道门槛：**先过内存预算评审**。而"评审"若只是
//! 一句"轻量模型应该够小"，那和没有评审一样。这个探针把两件事变成数字：
//!
//! 1. **表本身的内存**：`V × D × bytes` 是名义值，实际 RSS 还含页对齐与
//!    分配器开销——而"名义值 vs 实测"的差在这个项目里被咬过（P4a 估的是
//!    几十字节，实测 150–190）。
//! 2. **点积的成本**：200 个候选 × D 维。它决定"能不能放进按键路径"。
//!
//! # 它**不做**什么（诚实交代）
//!
//! - 不训练、不加载、不量化任何模型：那需要数据与许可（PLAN §10），
//!   而那是评审要**先回答**的问题，不是探针能替它回答的。
//! - 不测"向量重排有没有用"——那是收益问题，需要真实模型与对比集。
//!
//! 用法：
//!
//! ```text
//! cargo run --release --manifest-path tools/embed-probe/Cargo.toml -- table 414525 64 1
//! cargo run --release --manifest-path tools/embed-probe/Cargo.toml -- dot 200 64 100000
//! cargo run --release --manifest-path tools/embed-probe/Cargo.toml -- pq 414525 8
//! ```

// 这是一个**测量工装**：`V`/`D`/`N` 是这一行的通用记法，短名字更好读；
// "名义值 vs 实测"的比值是给人看的显示，浮点精度损失无关紧要。
// 项目里"参与排序的数一律定点"那条纪律约束的是引擎，不是探针的打印。
#![allow(clippy::many_single_char_names, clippy::cast_precision_loss)]

use std::time::Instant;

/// 读常驻内存（KiB）。非 Linux 返回 `None`（**不猜**）。
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

/// `table V D bytes`：分配一张 `V × D` 的表，每分量 `bytes` 字节，报告 RSS 增量。
fn table(v: usize, d: usize, bytes: usize) {
    let nominal = v * d * bytes;
    let before = rss_kib();
    // 用 `Vec<u8>` + 手动索引，避免引入任何依赖；写入时按页步进，
    // 保证**每一页都真的被触碰**（否则 RSS 只反映被访问的那几页）。
    let mut buf = vec![0u8; nominal];
    let page = 4096usize;
    let mut i = 0usize;
    while i < nominal {
        buf[i] = u8::try_from(i % 251).unwrap_or(0);
        i += page;
    }
    // 再读一遍，防止编译器把上面那次写优化掉（release 下它会）。
    let mut acc = 0u64;
    for &b in buf.iter().step_by(page) {
        acc = acc.wrapping_add(u64::from(b));
    }
    let after = rss_kib();

    println!("表 V={v}  D={d}  每分量 {bytes} 字节");
    println!(
        "  名义大小        : {} 字节（{:.2} MiB）",
        nominal,
        mib(nominal as u64)
    );
    match (before, after) {
        (Some(b), Some(a)) => {
            let delta = a.saturating_sub(b);
            println!(
                "  RSS 增量        : {delta} KiB（{:.2} MiB）",
                mib(delta * 1024)
            );
            println!(
                "  实测/名义       : {:.3}",
                if nominal == 0 {
                    0.0
                } else {
                    (delta * 1024) as f64 / nominal as f64
                }
            );
        }
        _ => println!("  RSS 增量        : 本平台读不到 /proc/self/status，跳过"),
    }
    // 单条向量的成本——评审里真正要看的数。
    println!("  单条向量        : {} 字节", d * bytes);
    println!("  （校验和 {acc}）");
}

/// `dot N D iters`：测"一次按键要对 N 个候选做 D 维点积"要多少时间。
///
/// 用 `i32` 累加（对标 `Score` 的定点取向）与 `i8` 分量（对标 int8 量化）。
fn dot(n: usize, d: usize, iters: usize) {
    // 造一批确定性的假向量：不引入随机数依赖，也保证可重复。
    let mut table = vec![0i8; n * d];
    for (i, x) in table.iter_mut().enumerate() {
        *x = i8::try_from((i * 31 + 7) % 255)
            .unwrap_or(0)
            .wrapping_sub(127);
    }
    let query: Vec<i8> = (0..d)
        .map(|j| i8::try_from((j * 17) % 255).unwrap_or(0).wrapping_sub(127))
        .collect();

    let mut sink = 0i64;
    // 预热：让代码页与栈都就位。
    for _ in 0..1000 {
        sink += one_round(&table, &query, n, d);
    }

    let t = Instant::now();
    for _ in 0..iters {
        sink += one_round(&table, &query, n, d);
    }
    let ns = t.elapsed().as_nanos();
    let per_round = ns as f64 / iters as f64;
    #[allow(clippy::cast_precision_loss)]
    let per_cand = per_round / n as f64;

    println!("一次重排：{n} 个候选 × {d} 维 int8 点积 × {iters} 轮");
    println!(
        "  每轮      : {per_round:.1} ns（{:.3} µs）",
        per_round / 1000.0
    );
    println!("  每个候选  : {per_cand:.1} ns");
    println!(
        "  按键红线  : P50 < 1 ms → 占 {:.3}%",
        per_round / 1_000_000.0 * 100.0
    );
    println!("  （校验和 {sink}）");
}

/// 一轮：对每个候选算一次点积，返回累加和（防止被优化掉）。
fn one_round(table: &[i8], query: &[i8], n: usize, d: usize) -> i64 {
    let mut total = 0i64;
    for i in 0..n {
        let row = &table[i * d..(i + 1) * d];
        let mut acc = 0i32;
        for (a, b) in row.iter().zip(query.iter()) {
            acc += i32::from(*a) * i32::from(*b);
        }
        total += i64::from(acc);
    }
    total
}

/// `pq V M`：乘积量化的码本大小（只算数，不实测）。
fn pq(v: usize, m: usize) {
    // PQ：把 D 维切成 M 段，每段用一个 8 位码，于是每条向量 M 字节。
    // 码本本身：M 段 × 256 个质心 × 子维度 × 4 字节（float32）。
    println!("乘积量化（PQ）：V={v}，M={m} 段（每段 1 字节码）");
    println!(
        "  码表大小 : {} 字节（{:.2} MiB）",
        v * m,
        mib((v * m) as u64)
    );
    let codebook_64 = m * 256 * 64 / m * 4;
    println!(
        "  码本（每段子维度 64/{m}）：约 {} 字节（{:.2} MiB）——比码表小两个数量级",
        codebook_64,
        mib(codebook_64 as u64)
    );
    println!("  单条向量 : {m} 字节（对标 int8 的 D 字节、float32 的 4D 字节）");
}

#[allow(clippy::cast_precision_loss)]
fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("table") => {
            let v = parse(&args, 1, 414_525);
            let d = parse(&args, 2, 64);
            let bytes = parse(&args, 3, 1);
            table(v, d, bytes);
        }
        Some("dot") => {
            let n = parse(&args, 1, 200);
            let d = parse(&args, 2, 64);
            let iters = parse(&args, 3, 100_000);
            dot(n, d, iters);
        }
        Some("pq") => {
            let v = parse(&args, 1, 414_525);
            let m = parse(&args, 2, 8);
            pq(v, m);
        }
        _ => {
            println!(
                "embed-probe — P5 内存预算探针\n\n\
                 用法：\n\
                 \x20 embed-probe table <V> <D> <bytes>   一张 V×D、每分量 bytes 字节的表\n\
                 \x20 embed-probe dot <N> <D> <iters>     一次按键对 N 个候选做 D 维点积\n\
                 \x20 embed-probe pq <V> <M>              乘积量化的码表大小（每段 1 字节）"
            );
        }
    }
}

/// 取第 `i` 个参数，缺省用 `default`。
fn parse(args: &[String], i: usize, default: usize) -> usize {
    args.get(i).and_then(|v| v.parse().ok()).unwrap_or(default)
}
