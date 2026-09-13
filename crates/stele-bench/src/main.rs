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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use stele_core::{
    CandidateSink, CodeUnitId, Engine, Key, KeyCode, Lexicon, MemoryStore, Modifiers, NamedKey,
    Outcome, Services,
};

/// 一个**会计数**的词库包装。
///
/// # 它回答的是 PLAN §6 里那条一直没答的问题
///
/// > 简拼的收益/成本：需要 P1 的实测数据（每次按键的词典查询次数）。
///
/// 数"每次按键查了几次词典"只有一个地方能数准：`Lexicon` 的实现边界。
/// 让引擎自己计数会把一个纯诊断的量塞进热路径，而**包装一层**只在
/// 称重台里生效（`--count-queries`），生产路径上一行都不多。
struct CountingLexicon {
    inner: Arc<dyn Lexicon>,
    /// `lookup`（精确查表）的调用次数。
    lookups: Arc<AtomicU64>,
    /// `prefix_lookup`（补全查表）的调用次数。
    prefixes: Arc<AtomicU64>,
}

impl Lexicon for CountingLexicon {
    fn lookup(&self, code: &[CodeUnitId], out: &mut CandidateSink<'_>) {
        self.lookups.fetch_add(1, Ordering::Relaxed);
        self.inner.lookup(code, out);
    }

    fn prefix_lookup(
        &self,
        prefix: &[CodeUnitId],
        exclude_exact: bool,
        out: &mut CandidateSink<'_>,
    ) {
        self.prefixes.fetch_add(1, Ordering::Relaxed);
        self.inner.prefix_lookup(prefix, exclude_exact, out);
    }

    fn supports_prefix(&self) -> bool {
        self.inner.supports_prefix()
    }
}

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

/// 每次按键的**词典查询次数**（PLAN §6 那条欠账的答案）。
///
/// 它回答的是"简拼这类变体拼写的收益/成本"：收益是少敲键，
/// 成本是每多一条拼写展开就多一次查表。
struct QueryReport {
    /// 观测到的按键数。
    samples: usize,
    /// 总查询次数。
    total: u64,
    /// 单键查询次数的分位数。
    per_key_p50: u64,
    per_key_p99: u64,
    per_key_max: u64,
    /// 单键平均查询次数 × 1000（整数，避免浮点）。
    mean_milli: u64,
}

/// 用户记忆的**内存成本**（`--seed-memory=N`）。
struct MemoryCost {
    /// 合成之后的条目数。
    entries: usize,
    /// 常驻内存增量（KiB）。
    rss_delta_kib: u64,
    /// 落盘之后的文件字节数（没落盘则为 `None`）。
    file_bytes: Option<u64>,
    /// 插入 N 条花了多少微秒。
    insert_us: u64,
}

/// **预测表**的内存成本（`--seed-predict=N`，P4b）。
///
/// 与 [`MemoryCost`] 分开量，因为它们是两张表、两个上限、两个开关
/// （HANDOFF §7.7.2 ③ 要求"红线剩下的余量要一起算总账"）。
struct PredictionCost {
    /// 合成之后的预测条目数。
    entries: usize,
    /// 常驻内存增量（KiB）。
    rss_delta_kib: u64,
    /// 插入 N 条花了多少微秒。
    insert_us: u64,
}

/// 单条记录占多少字节（**只用于显示**，因此浮点精度损失无关紧要）。
#[allow(clippy::cast_precision_loss)]
fn per_entry(bytes: u64, entries: usize) -> f64 {
    if entries == 0 {
        0.0
    } else {
        bytes as f64 / entries as f64
    }
}

/// 读取常驻内存（KiB）。
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
             用法：stele-bench [--iterations=N] [--json] [--schema=<id>] [--scheme-dir <目录>]\n\
             \x20                [--userdb <文件>] [--predict] [--count-queries]\n\
             \x20                [--seed-memory=N] [--seed-predict=N] [--predict-cap=N]\n\
             \x20                [--embed]\n\n\
             测量：空转（框架开销下限）/ 内核排序 / 真实按键路径。\n\
             `--scheme-dir` 用真实词库量（内嵌演示词库的量不出真实成本）。\n\
             `--userdb`    挂上用户记忆（不给就量「没有记忆」的那一组数字）。\n\
             `--predict`   挂上下一词预测（P4b，默认关；需要 `--userdb`）。\n\
             `--embed`     挂上本地向量偏好记忆（P5/D46，默认关；需要 `--userdb`）。\n\
             `--count-queries` 数**每次按键查了几次词典**（PLAN §6 那条欠账）。\n\
             `--seed-memory=N` 合成 N 条用户记忆，量出单条占用与落盘体积。\n\
             `--seed-predict=N` 合成 N 条预测记录，量出预测表的内存增量（P4b 验收 < 5 MB）。\n\
             `--predict-cap=N` 改预测表上限（默认 20000），用来量成本曲线。"
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
            kind: stele_core::CandidateKind::Normal,
            key: None,
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
    // 方案来源：`--scheme-dir` 指定的目录（部署路径，词库走紧凑产物），
    // 否则是内嵌的演示方案。
    //
    // **为什么称重台必须支持目录**：`schemes/stele-default` 的真实词库有
    // 40 万条，而内嵌演示只有几十条——**用演示词库量出来的延迟不是延迟**
    // （P1 的 241 ns 就是这么来的，见 PLAN §8）。真实数字只能在真实词库上量。
    let scheme_dir = args
        .iter()
        .position(|a| a == "--scheme-dir")
        .and_then(|i| args.get(i + 1))
        .map(std::path::PathBuf::from);

    // `--userdb <文件>` 或 `--userdb=<文件>`：挂上用户记忆。
    //
    // **不给就是"没有记忆"那一组**——这正是我们要比较的两组数字
    // （HANDOFF §7.6.2 第 5 步）。
    let userdb: Option<std::path::PathBuf> = args
        .iter()
        .find_map(|a| a.strip_prefix("--userdb="))
        .map(std::path::PathBuf::from)
        .or_else(|| {
            args.iter()
                .position(|a| a == "--userdb")
                .and_then(|i| args.get(i + 1))
                .map(std::path::PathBuf::from)
        });
    let count_queries = args.iter().any(|a| a == "--count-queries");
    let seed_memory: Option<usize> = args
        .iter()
        .find_map(|a| a.strip_prefix("--seed-memory="))
        .and_then(|v| v.parse().ok());
    // ── 下一词预测（P4b）──
    let predict = args.iter().any(|a| a == "--predict");
    let seed_predict: Option<usize> = args
        .iter()
        .find_map(|a| a.strip_prefix("--seed-predict="))
        .and_then(|v| v.parse().ok());
    // ── 本地向量偏好记忆（P5 · D46）：**默认关** ──
    let embed = args.iter().any(|a| a == "--embed");

    let t_load = Instant::now();
    let mut defs: Vec<stele_engine::scheme::SchemeDef> = match &scheme_dir {
        Some(dir) => stele_schemes::load_dir_deployed(dir, &dir.join(".stele-cache"))
            .expect("按 --scheme-dir 装载方案失败"),
        None => stele_schemes::all().expect("内嵌方案必须能装载 —— 失败说明打包坏了"),
    };

    // 需要数查询次数时，把每个方案的外部词库包一层计数器。
    let mut lookups = Vec::new();
    let mut prefixes = Vec::new();
    if count_queries {
        for def in &mut defs {
            if let stele_engine::scheme::DictSource::External(inner) = &def.dictionary {
                let l = Arc::new(AtomicU64::new(0));
                let p = Arc::new(AtomicU64::new(0));
                def.dictionary =
                    stele_engine::scheme::DictSource::External(Arc::new(CountingLexicon {
                        inner: Arc::clone(inner),
                        lookups: Arc::clone(&l),
                        prefixes: Arc::clone(&p),
                    }));
                lookups.push(l);
                prefixes.push(p);
            }
        }
    }
    let total_queries = || -> u64 {
        lookups
            .iter()
            .chain(prefixes.iter())
            .map(|c| c.load(Ordering::Relaxed))
            .sum()
    };

    // 用户记忆：与 CLI 一样，**默认关闭**。
    //
    // `--predict-cap=N` 可以改预测表的上限——量成本曲线时要能突破默认值，
    // 否则"默认值该取多少"就只能靠外推（P4a 的教训：外推会差三到四倍）。
    let predict_cap: usize = args
        .iter()
        .find_map(|a| a.strip_prefix("--predict-cap="))
        .and_then(|v| v.parse().ok())
        .unwrap_or(stele_memory::DEFAULT_PREDICT_CAPACITY);
    let clock: Arc<dyn stele_core::Clock> = Arc::new(stele_memory::SystemClock::new());
    let memory: Option<Arc<stele_memory::FileMemory>> = userdb.as_ref().map(|p| {
        let m = if predict_cap == stele_memory::DEFAULT_PREDICT_CAPACITY {
            let (m, warn) = stele_memory::FileMemory::open_or_degrade(
                p,
                Arc::clone(&clock),
                stele_memory::DEFAULT_CAPACITY,
            );
            if let Some(w) = warn {
                eprintln!("⚠ {w}");
            }
            m
        } else {
            // 显式改了上限：用内存表（不落盘），让"成本曲线"只反映表本身。
            stele_memory::FileMemory::in_memory_with(
                Arc::clone(&clock),
                stele_memory::DEFAULT_CAPACITY,
                predict_cap,
            )
        };
        Arc::new(m)
    });
    // **本地向量偏好记忆（P5 · D46）**：训练用的材料是**文件里已有的**历史
    // （`--seed-memory` / `--seed-predict` 先把它灌好，再跑这一趟）。
    // 这样它既能量到真实的内存代价，又不会把"造测试数据"的时间算进装载。
    let mut embed_info: Option<(usize, usize)> = None;
    let services = {
        let base = Services::new(Arc::clone(&clock))
            .with_random(Arc::new(|| Box::new(stele_core::SystemRandom::new())));
        let base = match &memory {
            Some(m) => base.with_ranker(Arc::new(stele_memory::MemoryRanker::new(
                Arc::clone(m) as Arc<dyn stele_core::MemoryStore>
            ))),
            None => base,
        };
        // 预测：**默认关**，与 CLI 同一条产品决定。量"预测的代价"时才挂。
        let base = match (predict || seed_predict.is_some(), &memory) {
            (true, Some(m)) => {
                base.with_prediction(Arc::clone(m) as Arc<dyn stele_core::MemoryStore>)
            }
            (true, None) => {
                eprintln!(
                    "⚠ --predict / --seed-predict 需要 --userdb <文件>：预测数据来自用户记忆"
                );
                base
            }
            (false, _) => base,
        };
        // 向量记忆：**默认关**（D46 第①条）。
        match (embed, &memory) {
            (true, Some(m)) => {
                let samples = m
                    .prediction_snapshot()
                    .into_iter()
                    .map(|e| (e.context, e.text, e.count));
                if let Some(model) =
                    stele_embed::VectorMemory::train(samples, stele_embed::VectorConfig::default())
                {
                    embed_info = Some((model.len(), model.bytes()));
                    base.with_ranker(Arc::new(stele_embed::EmbedRanker::new(Arc::new(model))))
                } else {
                    eprintln!("⚠ --embed：用户记忆里还没有历史，学不出向量");
                    base
                }
            }
            (true, None) => {
                eprintln!("⚠ --embed 需要 --userdb <文件>：向量由本地历史学出来");
                base
            }
            (false, _) => base,
        }
    };

    // ── `--seed-memory=N`：合成 N 条记录，量出**单条占用**（HANDOFF §7.6.1 ③）──
    //
    // 条目上限不能拍脑袋：红线是常驻内存 < 30 MB，而真实词库已经占 13.6 MiB。
    // 这里量的是"再多 N 条要花多少 RSS"，上限由它反推。
    //
    // **放在引擎装载之后**：否则 `load_us` 会把合成的时间算进去，
    // 而"引擎装载"与"造测试数据"是两件事（我第一次就量错了）。
    let engine =
        stele_engine::EngineImpl::with_services(&defs, services).expect("默认方案应当能编译");
    let load_us = u64::try_from(t_load.elapsed().as_micros()).unwrap_or(u64::MAX);

    let memory_cost = match (&memory, seed_memory) {
        (Some(m), Some(n)) if n > 0 => {
            let rss_before = rss_kib().unwrap_or(0);
            let t = Instant::now();
            for i in 0..n {
                m.record(&stele_core::Commit {
                    text: format!("词{i}"),
                    input: format!("in{i}"),
                    context: Vec::new(),
                    origin: stele_core::Origin::SystemWord,
                    attr: stele_core::SpellingAttr::NORMAL,
                    lane: stele_core::Lane::Input,
                    trigger: stele_core::Trigger::Space,
                    // 合成记录直接给**规范键**：这样 `--seed-memory` 量的是
                    // 记忆的真实形态，而不是退化成拼写的那条兜底路径。
                    key: Some(format!("in{i}").into()),
                });
            }
            let insert_us = u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX);
            let rss_after = rss_kib().unwrap_or(0);
            let file_bytes = m.flush().ok().and_then(|_| {
                m.path()
                    .and_then(|p| std::fs::metadata(p).ok())
                    .map(|md| md.len())
            });
            Some(MemoryCost {
                entries: m.len(),
                rss_delta_kib: rss_after.saturating_sub(rss_before),
                file_bytes,
                insert_us,
            })
        }
        _ => None,
    };

    // ── `--seed-predict=N`：合成 N 条预测记录（P4b 的内存验收）──
    //
    // 与 `--seed-memory` 同样的道理：一次预测学习会写 **bigram + trigram**
    // 两条，因此"条目数"与"学了多少个搭配"不是一回事。这里用**只有一个词的
    // 上下文**，于是一次 `record` 恰好写一条，数字读起来才不歧义。
    let prediction_cost = match (&memory, seed_predict) {
        (Some(m), Some(n)) if n > 0 => {
            let rss_before = rss_kib().unwrap_or(0);
            let t = Instant::now();
            for i in 0..n {
                m.record(&stele_core::Commit {
                    text: format!("接{i}"),
                    input: String::new(),
                    context: vec![format!("词{i}")],
                    origin: stele_core::Origin::Prediction,
                    attr: stele_core::SpellingAttr::NORMAL,
                    lane: stele_core::Lane::Predict,
                    trigger: stele_core::Trigger::Explicit,
                    key: None,
                });
            }
            let insert_us = u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX);
            let rss_after = rss_kib().unwrap_or(0);
            // **落盘**：让下一趟（`--embed`）能拿这份历史去学向量。
            // 放在计时区间之后 —— 落盘不是"插入成本"的一部分。
            let _ = m.flush();
            Some(PredictionCost {
                entries: m.prediction_len(),
                rss_delta_kib: rss_after.saturating_sub(rss_before),
                insert_us,
            })
        }
        _ => None,
    };

    // 方案可选：不同方案的零件数差别很大（拼音有切分器与标点，
    // 字形码没有），因此**延迟数字必须注明测的是哪个方案**。
    let schema: Option<String> = args
        .iter()
        .find_map(|a| a.strip_prefix("--schema="))
        .map(str::to_owned);
    let mut session = engine.create_session();
    if let Some(id) = &schema {
        if let Err(e) = session.switch_schema(id) {
            eprintln!("切换方案 {id} 失败：{e}");
            std::process::exit(2);
        }
    }
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

    // ── 工作负载 4：每次按键的词典查询次数（PLAN §6 的欠账）──
    //
    // 只数调用次数，不计时——计时已经在上面那条按键路径里了。
    // 用**同一个**循环结构（每四位 reset）以便与延迟数字对照。
    let query_report = if count_queries {
        let n = iterations.min(50_000);
        let mut per_key: Vec<u64> = Vec::with_capacity(n);
        let mut last = total_queries();
        for key_index in 0..n {
            if key_index > 0 && key_index % cycle.len() == 0 {
                session.reset();
            }
            let c = cycle[key_index % cycle.len()];
            session.process_key(Key::ch(c));
            let now = total_queries();
            per_key.push(now.saturating_sub(last));
            last = now;
        }
        per_key.sort_unstable();
        let total: u64 = per_key.iter().sum();
        let samples = per_key.len() as u64;
        Some(QueryReport {
            samples: per_key.len(),
            total,
            per_key_p50: percentile(&per_key, 50),
            per_key_p99: percentile(&per_key, 99),
            per_key_max: per_key.last().copied().unwrap_or(0),
            mean_milli: total.saturating_mul(1000).checked_div(samples).unwrap_or(0),
        })
    } else {
        None
    };

    let startup_us = u64::try_from(t_start.elapsed().as_micros()).unwrap_or(u64::MAX);

    if as_json {
        let mut out = String::new();
        let _ = write!(
            out,
            "{{\"schema\":\"{}\",\"idle\":{{\"samples\":{},\"p50_ns\":{},\"p95_ns\":{},\"p99_ns\":{},\"max_ns\":{}}},\
             \"sort200\":{{\"samples\":{},\"p50_ns\":{},\"p95_ns\":{},\"p99_ns\":{},\"max_ns\":{}}},\
             \"keypath\":{{\"samples\":{},\"p50_ns\":{},\"p95_ns\":{},\"p99_ns\":{},\"max_ns\":{}}},\
             \"rss_kib\":{},\"engine_load_us\":{},\"process_us\":{},\"probe_commit\":\"{}\",\
             \"memory_entries\":{},\"queries\":{}}}",
            session.schema_id(),
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
            committed,
            memory.as_ref().map_or(0, |m| m.len()),
            query_report.as_ref().map_or("null".to_owned(), |q| format!(
                "{{\"samples\":{},\"total\":{},\"per_key_p50\":{},\"per_key_p99\":{},\"per_key_max\":{},\"mean_milli\":{}}}",
                q.samples, q.total, q.per_key_p50, q.per_key_p99, q.per_key_max, q.mean_milli
            ))
        );
        println!("{out}");
        return;
    }

    println!("Stele-IME 称重台");
    println!("========================================");
    println!("被测方案：{}", session.schema_id());
    println!("自检：敲 nihao 后上屏 「{committed}」");
    match &memory {
        Some(m) => println!(
            "用户记忆：{} 条{}",
            m.len(),
            m.path()
                .map_or_else(String::new, |p| format!("（{}）", p.display()))
        ),
        None => println!("用户记忆：**未挂载**（没有给 `--userdb`）"),
    }
    match &memory {
        Some(m) if predict => println!("下一词预测：已挂载（预测表 {} 条）", m.prediction_len()),
        Some(_) => println!("下一词预测：**未挂载**（没给 `--predict`；默认关）"),
        None => println!("下一词预测：**未挂载**（没有 `--userdb`，无处取数据）"),
    }
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
    println!("引擎装载（含全部方案）: {load_us} µs");
    if let Some(c) = &memory_cost {
        // 精度损失在这里无害：这是**给人看的显示**，不是参与排序的分数
        // （真正要求精度的分数用的是定点整数 `Score`）。
        #[allow(clippy::cast_precision_loss)]
        let delta_mib = c.rss_delta_kib as f64 / 1024.0;
        println!();
        println!("用户记忆的内存成本（--seed-memory）：");
        println!(
            "  {} 条 → 这段合成的 RSS 增量 {} KiB（{:.2} MiB）  {} µs",
            c.entries, c.rss_delta_kib, delta_mib, c.insert_us
        );
        println!("  ⚠ 增量可能被**分配器复用**低估；权威数字是上面那一行总常驻内存：");
        println!("    与「0 条记忆」那一次的总 RSS 相减，才是它真实的占用。");
        if let Some(b) = c.file_bytes {
            println!(
                "  落盘文件 {} 字节（单条约 {:.1} 字节）",
                b,
                per_entry(b, c.entries)
            );
        }
        println!(
            "  默认上限 {} 条（`stele_memory::DEFAULT_CAPACITY`）",
            stele_memory::DEFAULT_CAPACITY
        );
    }
    if let Some(c) = &prediction_cost {
        // 精度损失在这里无害：这是给人看的显示（分数本身是定点 `Score`）。
        #[allow(clippy::cast_precision_loss)]
        let delta_mib = c.rss_delta_kib as f64 / 1024.0;
        println!();
        println!("预测表的内存成本（--seed-predict，P4b）：");
        println!(
            "  {} 条 → 这段合成的 RSS 增量 {} KiB（{:.2} MiB）  {} µs",
            c.entries, c.rss_delta_kib, delta_mib, c.insert_us
        );
        println!("  ⚠ 同样可能被分配器复用低估；权威数字是与「0 条」那次的总 RSS 相减。");
        println!(
            "  默认上限 {} 条（`stele_memory::DEFAULT_PREDICT_CAPACITY`）；\
             一次学习写 bigram+trigram 两条",
            stele_memory::DEFAULT_PREDICT_CAPACITY
        );
    }
    if let Some((vocab, bytes)) = embed_info {
        // D46 第②条：**上限要进称重台的报告**，不能只写在文档里。
        #[allow(clippy::cast_precision_loss)]
        let mib = bytes as f64 / (1024.0 * 1024.0);
        println!();
        println!("本地向量偏好记忆（--embed，P5/D46）：");
        println!(
            "  {} 个词 × {} 维 → 向量表 {} KiB（{:.2} MiB）",
            vocab,
            stele_embed::VectorConfig::default().dim,
            bytes / 1024,
            mib
        );
        println!(
            "  加成上限：前 {} 名、每次最多 {} 毫对数（`stele_embed::EmbedRanker`）",
            stele_embed::EmbedRanker::DEFAULT_MAX_BOOSTED,
            stele_embed::EmbedRanker::DEFAULT_LIMIT_ML
        );
        println!("  **默认关闭**（D46 第①条）：不给 `--embed` 时这一项为 0，红线按默认配置算。");
    }
    if let Some(q) = &query_report {
        println!();
        println!("每次按键的词典查询次数（PLAN §6 的欠账）：");
        println!(
            "  总查询 {} 次 / {} 键   P50 {}   P99 {}   max {}   平均 {}.{:03}",
            q.total,
            q.samples,
            q.per_key_p50,
            q.per_key_p99,
            q.per_key_max,
            q.mean_milli / 1000,
            q.mean_milli % 1000
        );
        println!("  读法：这个数字是**变体拼写的成本**——每多一条拼写展开就多一次查表。");
    }
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
