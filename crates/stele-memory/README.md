# stele-memory — 用户记忆（P4a）

**职责**：`stele_core::MemoryStore` 的服务提供者——频率 + 时间衰减的用户记忆，
以及把它接到候选排序上的 `Ranker`。

**零第三方依赖**（与内核同级）。这不是巧合：我们需要的只是一个
"按键时零 I/O、启动时读一次"的**有序表**，而那正是 `stele-table` 已经
解决过的形状。选型过程见 `docs/HANDOFF.md` §7.6.1 ④。

## 四条硬约束（都有测试守着）

| 约束 | 怎么证的 |
| --- | --- |
| **按键时零磁盘 I/O** | `tests/no_disk_io_on_keypath.rs` 读 `/proc/self/io`，断言 1000 次按键后 `syscr`/`syscw`/`read_bytes`/`write_bytes` **一个都没涨**；落盘只有显式的 `FileMemory::flush()` |
| **可复现** | 量纲全程整数（D13）；淘汰按 `(衰减频次, 输入, 词)` 全序；落盘顺序由 `BTreeMap` 决定 ⇒ 同一状态两次运行逐字节相同 |
| **坏文件不阻止启动** | `FileMemory::open_or_degrade`：警告一行、降级成空记忆、**且不覆盖那个读不懂的文件**（D26） |
| **内存有界且确定** | 默认 `DEFAULT_CAPACITY`（实测反推）；超限淘汰衰减频次最低的条目 |

## 用法（前端要做的三件事）

```rust
use std::sync::Arc;
use stele_core::{Services, Session};
use stele_memory::{apply_events, FileMemory, MemoryRanker, SystemClock};

let clock = Arc::new(SystemClock::new());
// ① 打开记忆（坏文件会降级 + 给出一行警告，务必打出来）
let (memory, warning) = FileMemory::open_or_degrade("user.mem", clock.clone(), 30_000);
if let Some(w) = warning { eprintln!("⚠ {w}"); }
let memory = Arc::new(memory);

// ② 装配时注入重排器（**不穿过 Query**，见 engine-design §4）
let services = Services::new(clock)
    .with_ranker(Arc::new(MemoryRanker::new(memory.clone())));
let engine = stele_engine::EngineImpl::with_services(&defs, services)?;
let mut session = engine.create_session();

// ③ 每个按键之后把事件取干净并喂给记忆
let mut events = Vec::new();
session.drain_events(&mut events);
apply_events(memory.as_ref(), &events);

// …退出时落盘（**绝不在按键路径上**）
memory.flush()?;
```

## 文件格式（v1，小端）

前端若要自己读它，只需这一段：

```text
magic    8 字节  "STELEMEM"
version  u32     = 1
count    u32
记录 × count：
    input_len  u32 + input  UTF-8（**规范化之后的键**）
    text_len   u32 + text   UTF-8
    count      u32   累计上屏次数
    decayed    u64   已衰减的累计频次（千分之一单位，一次上屏 = 1000）
    last_used  u64   Unix 秒
checksum u64     前面**全部字节**的 FNV-1a
```

记录按 `(input, text)` 升序。落盘 = 写临时文件 + `rename`（同目录，因此原子）。

## 量纲

```text
每次上屏：decayed ← decay(decayed, 距上次) + 1.0     （整数次减半，半衰期 30 天）
任何时刻：bonus   ← 14000 × f / (f + 2000)          （f = 此刻的 decayed，毫单位）
```

上界 14 000 毫对数**必须高于真实词库的最高分**（实测约 12 366），
否则"打过的词下次优先"在算术上不成立。

**键是规范编码**（如 `ni'hao`），由翻译器渲染成 `Candidate::key`
随候选带出（PLAN D42）。于是 `nhao` 学到的词在 `nihao` 下也优先——
与 RIME 的用户词典（以 `code` 为键）功能等价。
拿不到编码时（造句、标点、原样上屏）退回按 `store::normalize_key` 规范化。

## 明确不做的事

- **下一词预测**（`predict_next` 现在返回空）——P4b。
- **向量重排**——P5，且要先过内存预算评审。
