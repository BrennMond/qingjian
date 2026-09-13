//! P4a 验收 2：**按键路径上零磁盘 I/O**（红线）。
//!
//! # 为什么这条必须是测试，而不是文档里的一句话
//!
//! RIME 的按键延迟 P99 高达 **36 ms**，其中最大的单点瓶颈就是用户词库的
//! `LevelDB` 磁盘读（见 `reference/librime-frontend-research.md`）。
//! 我们的红线是 P99 < 10 ms，所以"记忆全量放内存、改动异步批量落盘"
//! 不是一个优化建议，而是**这条红线成立的前提**。
//!
//! 写在文档里没人能证伪；读 `/proc/self/io` 的计数器可以。
//!
//! # 判据：四个计数器，两个层次
//!
//! | 计数器 | 含义 | 断言 |
//! | --- | --- | --- |
//! | `syscr` / `syscw` | `read`/`write` **系统调用的次数** | 按键那段**一次都没有** |
//! | `read_bytes` / `write_bytes` | 真的从/向**存储层**走的字节 | 一个字节都没有 |
//!
//! **为什么必须两层都测**（这是反向验证逼出来的修正）：
//! 只测 `write_bytes` 时，往 `/tmp` 写一个文件**不会**让计数器变化——
//! 那次写留在页缓存里，块设备层还没见到它。只测字节数的版本因此
//! **抓不住一次故意的违规**，而"一个不会失败的检查等于没有检查"。
//! `syscw` 数的是系统调用本身，页缓存骗不过它。
//!
//! # 两条必须交代的细节
//!
//! 1. **`/proc/self/io` 是 Linux 专属**。别的平台没有它，本测试就跳过——
//!    但**跳过要打出来**，绝不静默通过。HANDOFF §7.6.0 的要求是
//!    "不许因为不好写就不写"，而不是"假装跨平台"。
//! 2. **读探针自己会花掉系统调用**（`read_to_string` 至少一次 `read`）。
//!    所以先连续读两次，量出"一次探针的开销"，再从结果里减掉它。
//!    不减的话，这条断言会因为**测量手段本身**而永远红。
//!
//! 这个文件里**只有一条测试**：`cargo test` 默认多线程跑同一个二进制里的
//! 测试，而别的测试要是恰好写了文件，计数器就会增长，
//! 于是这条断言会以"别人干了坏事"为由变红。一条测试一个进程最干净。

use std::sync::Arc;

use qingjian_core::{
    Clock, Engine, FrozenClock, Key, KeyCode, MemoryStore, Modifiers, NamedKey, Services, Session,
};
use qingjian_memory::{apply_events, FileMemory, MemoryRanker};

/// `/proc/self/io` 里我们关心的四个计数器。
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug)]
struct Io {
    /// `read` 系统调用次数。
    syscr: u64,
    /// `write` 系统调用次数。
    syscw: u64,
    /// 真正从存储层读回的字节。
    read_bytes: u64,
    /// 真正写向存储层的字节。
    write_bytes: u64,
}

/// 读一次计数器。
///
/// 读这个文件本身是一次 `read` 系统调用，因此调用方必须减掉探针开销
/// （见模块文档第 2 条）。
#[cfg(target_os = "linux")]
fn io() -> Option<Io> {
    let text = std::fs::read_to_string("/proc/self/io").ok()?;
    let field = |name: &str| -> Option<u64> {
        text.lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| v.trim().parse::<u64>().ok())
    };
    Some(Io {
        syscr: field("syscr:")?,
        syscw: field("syscw:")?,
        read_bytes: field("read_bytes:")?,
        write_bytes: field("write_bytes:")?,
    })
}

#[cfg(target_os = "linux")]
#[test]
fn typing_does_no_disk_io() {
    if io().is_none() {
        eprintln!("跳过：读不到 /proc/self/io");
        return;
    }

    // ── 装配：内嵌演示方案（全部在内存里）+ 不落盘的用户记忆 ──
    let defs = qingjian_schemes::all().expect("内嵌方案必须能装载");
    let clock: Arc<dyn Clock> = Arc::new(FrozenClock {
        secs: 1_767_225_600,
        ms: 0,
        offset_secs: 0,
    });
    let memory = Arc::new(FileMemory::in_memory(Arc::clone(&clock), 10_000));
    let store = Arc::clone(&memory) as Arc<dyn MemoryStore>;
    let services = Services::new(Arc::clone(&clock))
        .with_ranker(Arc::new(MemoryRanker::new(Arc::clone(&store))))
        // **预测也挂上**（P4b）：这条红线管的是**整条按键路径**，
        // 而预测查询正好落在它上面（`compose` 每次都会问一次预测表）。
        // 只测记忆、不测预测，等于给新加的那段路留了一个缺口。
        .with_prediction(store);
    let engine = qingjian_engine::EngineImpl::with_services(&defs, services).expect("编译方案");
    let mut session = engine.create_session();

    // 一次"打字 + 上屏"，把整条链（含记忆的读与写）都走到。
    let cycle = |session: &mut Box<dyn Session + Send>| {
        for c in "nihao".chars() {
            session.process_key(Key::ch(c));
            let mut events = Vec::new();
            session.drain_events(&mut events);
            apply_events(memory.as_ref(), &events);
        }
        let space = Key::press(KeyCode::Named(NamedKey::Space), Modifiers::NONE);
        session.process_key(space);
        let mut events = Vec::new();
        session.drain_events(&mut events);
        apply_events(memory.as_ref(), &events);
    };

    // ── 预热：让代码页与堆都就位 ──
    //
    // 首次走到某段代码会触发按需分页，那一页来自磁盘 —— 会计进 read_bytes。
    // 不预热的话，测到的是"操作系统在加载这个测试二进制"。
    for _ in 0..200 {
        cycle(&mut session);
    }
    assert!(
        !memory.is_empty(),
        "预热之后记忆里应当有东西——否则这条测试根本没走到记忆"
    );
    assert!(
        memory.prediction_len() > 0,
        "预热之后预测表里应当有东西——否则这条测试没走到预测那条路"
    );

    // ── 量出"一次探针"的开销 ──
    let mut probe_reads = 0;
    let mut probe_writes = 0;
    for _ in 0..5 {
        let a = io().expect("读得到 /proc/self/io");
        let b = io().expect("读得到 /proc/self/io");
        probe_reads = probe_reads.max(b.syscr.saturating_sub(a.syscr));
        probe_writes = probe_writes.max(b.syscw.saturating_sub(a.syscw));
    }

    // ── 测量 ──
    let before = io().expect("读得到 /proc/self/io");
    for _ in 0..1000 {
        cycle(&mut session);
    }
    let after = io().expect("读得到 /proc/self/io");

    let reads = (after.syscr - before.syscr).saturating_sub(probe_reads);
    let writes = (after.syscw - before.syscw).saturating_sub(probe_writes);
    assert_eq!(
        reads, 0,
        "按键路径上发生了 {reads} 次 read 系统调用（探针开销 {probe_reads} 已扣除）——\
         红线是**一次都不能有**"
    );
    assert_eq!(
        writes, 0,
        "按键路径上发生了 {writes} 次 write 系统调用（探针开销 {probe_writes} 已扣除）——\
         红线是**一次都不能有**；落盘只能由显式的 flush() 触发"
    );
    assert_eq!(
        after.read_bytes, before.read_bytes,
        "按键路径真的从存储层读了字节（{} → {}）",
        before.read_bytes, after.read_bytes
    );
    assert_eq!(
        after.write_bytes, before.write_bytes,
        "按键路径真的向存储层写了字节（{} → {}）",
        before.write_bytes, after.write_bytes
    );
}

/// 非 Linux：明确地跳过，并说明这条验收在别的平台上靠什么保证。
#[cfg(not(target_os = "linux"))]
#[test]
fn typing_does_no_disk_io() {
    eprintln!(
        "跳过：本平台没有 /proc/self/io，无法从进程外部证伪这条红线。\
         结构上的保证仍然成立——`record`/`lookup` 的实现里没有任何文件操作，\
         落盘只由显式的 `flush()` 触发（见 crates/qingjian-memory/src/store.rs）。"
    );
}
