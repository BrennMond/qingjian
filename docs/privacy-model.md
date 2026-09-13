# 隐私模型（Privacy Model）

> **状态**：§1–§3、§5–§7 是**对现有代码的核实与记录**（附命令）。
> §4（敏感输入禁学）是**接口设计**，**尚未实现**——本文不把它说成已有能力。
> §8 是前端/操作系统的责任边界，**不在引擎代码的控制内**。
>
> **这份文件为什么必须在做前端之前完成**：输入法的隐私边界一半在内核
> （记什么、存哪、默认开不开），一半在前端与操作系统（焦点应用、备份、
> 系统日志）。前一半现在能定，后一半现在只能定**合同**；等前端开工再想，
> 就会变成"每个前端各写各的"。
>
> **一条必须先说清的界限（审计 §4 / 6.4 明确要求）**：
> **"引擎里没有联网代码"不等于完整的隐私证明。** 它只证明
> *这台引擎自己不会联网*；文件位置、父目录权限、系统备份、崩溃上报、
> 输入法框架本身、以及前端将来加的任何东西，都在这个证明之外。
> §9 把"代码保证的"与"依赖前端/OS 的"分开列。

---

## 1. 不收集 / 不联网承诺（已核实）

### 1.1 承诺

**Qingjian 引擎（`crates/`）不发起任何网络请求，不发送任何遥测，
不采集任何使用数据到本机之外的任何地方。**

### 1.2 核实方式与结果

对 `crates/` 下的全部 Rust 源码与清单做符号级搜索：

```bash
grep -rn -E "TcpStream|UdpSocket|reqwest|hyper|std::net|ureq|isahc|socket2|openssl|native_tls" \
     crates/ --include=*.rs
```

**观察到的全部命中都只是字符串/测试数据**，不是网络调用：正则识别器的
测试用例里出现了 `http://x`、`https://x`（`crates/qingjian-engine/src/regex.rs`、
`segmentor.rs`、`tests/regex_and_recognizer.rs`）。**没有任何 socket、
HTTP 客户端或 TLS 依赖的使用点。**

依赖侧同时成立：`Cargo.lock` 只有 **10 个 workspace 成员，0 个 registry 包**。
`scripts/verify-zero-deps.sh` 守 `qingjian-core` / `qingjian-engine` 的零第三方依赖，
`scripts/verify-deps.sh` 的受审白名单当前为空。没有依赖 ⇒ 没有"某个依赖偷偷
联网"的供应链路径。

`std::process::Command` 在 `crates/` 里**没有出现**（仅 `std::process::id()`
与 `ExitCode` 被使用），因此也不存在"调起外部程序"的旁路。

### 1.3 例外：`tools/` 下的**开发期**脚本会联网

`tools/fetch-sources.sh` 会下载 §7 列出的公开数据。
这是**使用者显式执行的一次部署动作**（`bash tools/fetch-sources.sh`），
不是引擎行为：

- 它只在**被调用时**运行，不在任何按键路径、不在 CI；
- 下载目标固定到 commit SHA，清单在 `tools/sources.lock`，可先读后跑；
- `tools/wordlist-gen` 只读本地文件，**一次网络访问都不做**。

区分这两者是本模型的一部分：**引擎离线，是"运行时没有网络代码"；
数据取回需要一次网络，是"部署时由人决定"。** 不要把它们混为一谈。

---

## 2. 数据分类：用户记忆里到底有什么

用户记忆由两个实现承载：`qingjian_core::MemoryStore`（trait）与
`qingjian_memory::FileMemory`（默认实现）。它包含**两张表**：

| 表 | 键 | 值（每条记录） | 上限 |
| --- | --- | --- | --- |
| 输入表 | `(规范编码, 词)` | `count`（上屏次数）、`decayed_milli`（时间衰减后的累计频次）、`last_used`（Unix 秒） | `DEFAULT_CAPACITY = 30_000` 条 |
| 预测表 | `(上文1, 上文2, 词)` | 同上 | `DEFAULT_PREDICT_CAPACITY = 20_000` 条 |

**代码位置**：`crates/qingjian-core/src/service.rs`（`MemoryEntry`、`MemoryStore`）、
`crates/qingjian-memory/src/store.rs`（`Entry`、`Inner`、`snapshot`、
`prediction_snapshot`）、`crates/qingjian-memory/src/file.rs`（落盘格式）。

**逐项说明**：

- **上屏文本（`text`）**：用户最终**提交**的词句。这既包括你打出来的内容，
  也包括被候选/联想（`Lane::Predict`）送上去的内容。
- **编码（`input`，规范编码键）**：如 `ni'hao`。它由翻译器给出，
  与具体拼法（`nihao` / `nh`）无关（PLAN D42）。
- **上下文（预测表的 `context`，最多两个词）**：**最近提交过的词**。
  这是**跨应用**的：输入法服务在系统里是共享的，切到另一个应用后，
  上下文仍可能带着上一个应用里的词。这正是 Android 端要处理
  "跨应用上下文"的原因。
- **计数与时间**：`count`、`decayed_milli`、`last_used`（Unix 秒）。
  `last_used` 让文件带有**行为时间线**性质——它能反映"什么时候在用什么词"。

**它不是什么（同样重要）**：

- **不是逐键日志**：按键本身不落盘，被删除/取消的输入不记录，光标、
  选区、周围屏幕文本、剪贴板、密码框内容都不会进入记忆。
- **不是完整文档**：只收"提交过的词"，不是输入框全文。
- **不是网络数据**：没有账号、设备标识、地理位置、IP。

**但必须承认的推断风险**：即使不是逐键日志，"上屏文本 + 时间 + 上下文"
已足以还原相当一部分输入行为，包括你以为没打出去的内容
（提交后再删除的词仍留在记忆里）。**因此它属于敏感的本机行为历史，
不是"无害的缓存"。**

**默认关闭**：CLI 不给 `--userdb <路径>` 就没有任何记忆，行为逐字节可复现
（`crates/qingjian-cli/src/main.rs`）；预测（`--predict`）与本地向量（`--embed`）
还要再各自显式打开。

---

## 3. 磁盘位置与权限

### 3.1 位置由前端决定，不由引擎决定

`FileMemory` 从不自己选路径：路径来自调用方
（`FileMemory::open(path, …)` / `open_with` / `open_or_degrade`），
CLI 通过 `--userdb <路径>` 传入。**这是有意的**：位置策略属于前端/平台，
引擎只保证"写到给定的那个文件"。

| 环境 | 位置要求 |
| --- | --- |
| 桌面 CLI / 开发 | 使用者给的任意路径（示例用 `/tmp/u.mem`，**仅为演示**） |
| Windows TSF（未实现） | 应用数据目录（`%LOCALAPPDATA%` 下的私有目录） |
| Android（未实现） | **app-private storage**：`getFilesDir()` 或 `noBackupFilesDir()`，**不得**用外部存储或共享目录 |

### 3.2 权限（已实现，Unix）

新建/替换记忆文件时以 `0600`（仅所有者读写）创建：

```rust
// crates/qingjian-memory/src/store.rs: write_private()
opts.mode(0o600);   // #[cfg(unix)]
// 写临时文件 → fsync → rename → fsync 目录；目标文件继承 0600
```

`crates/qingjian-memory/tests/persistence_failures.rs::the_memory_file_is_owner_only`
钉住这条行为。

**权限**不是**完整的隐私方案**，本文不把它当结论：

- 它挡的是**同机其他用户**，挡不住以**同一用户**运行的其他进程，
  也不挡 root、恶意软件或取证工具；
- 父目录若本身对他人可读，文件仍不可读（0600 生效），但**文件名与存在性**可见；
- 备份 / 云同步 / 磁盘镜像会**连文件内容一起**带走，权限对此无效（§8）；
- 文件**没有加密**（§3.3）。

### 3.3 文件格式：自校验，但不加密、不防篡改

`crates/qingjian-memory/src/file.rs`：

- 魔数 `QJIANMEM` + `VERSION = 2`；
- 一个覆盖**前面全部字节**的 **FNV-1a** 校验和。

含义要精确：

- FNV 是**完整性**检查（防写坏、防写了一半），**不是密码学完整性**，
  **不是防篡改**，**不是加密**。能读文件的人能读内容、也能改内容
  （改完重算校验和即可）。
- 文件损坏时 `open_or_degrade` 降级为"没有记忆 + 一行警告"，
  且实例 `writable == false`——**坏文件不会被自动覆盖**（D26）。

### 3.4 落盘时机：按键路径一次都不碰

**用户记忆这条路径**的按键**零磁盘 I/O** 是红线，由
`crates/qingjian-memory/tests/no_disk_io_on_keypath.rs` 用 `/proc/self/io` 的
`syscr`/`syscw` 守着。写盘只发生在 `flush()`——由前端在
"空闲 debounce / `onStop` / `onTrimMemory` / 正常退出"时调用
（合同见 `docs/memory-persistence-design.md` §5）。
多实例并发由 `<path>.lock` 咨询锁 + 逐键取更大值合并处理
（**咨询锁**：不遵守约定的写者挡不住）。

---

## 4. 敏感输入禁学（**接口设计，尚未实现**）

> **实现不在本次范围内。本文不声称它已经存在。**
> 现状：**没有任何代码路径会因为焦点是密码框而停止学习**——
> 只要前端调了 `qingjian_memory::apply_events`，`Event::Learned` 就会进记忆。

### 4.1 为什么必须有它

记忆里存的是上屏文本与上下文。在密码框、私密应用、搜索框的敏感查询里，
即使输入法不"联网"，**把内容写进一个默认长期保留的本地文件**本身就是风险
（取证、备份、同机其他进程、误导出）。另外 `Lane::Predict` 会把
**其他应用**的上下文带进来，可能把不该出现的历史泄漏到当前输入框的候选里。

### 4.2 设计：策略门放在"事件 → 记忆"的唯一映射点

现有代码已经把映射收敛到一处：`crates/qingjian-memory/src/events.rs` 的
`apply_events(memory, events)`。**门就加在这里**，前端不需要各自 `match`：

```rust
// 设计草案（尚未实现）
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LearningPolicy {
    /// 正常学习：`Event::Learned` 进记忆。
    Learn,
    /// 敏感输入：**丢弃 `Event::Learned`**，但仍处理 `Event::ForgetRequested`。
    /// 丢弃 = 不调用 `MemoryStore::record`，不是"记了再删"。
    Suspend,
}

/// 与 `apply_events` 同签名，多一个策略参数。
/// 返回处理了几条（`Suspend` 时被丢弃的 `Learned` 不计入）。
pub fn apply_events_with_policy(
    memory: &dyn MemoryStore,
    events: &[Event],
    policy: LearningPolicy,
) -> usize;
```

要点：

1. **丢弃而不是"记了再删"**：先 `record` 再 `forget` 会在坏文件/崩溃窗口
   把敏感内容留在盘上，并且 `forget` 目前够不着预测表（§5.2）。
2. **`ForgetRequested` 在 `Suspend` 下仍要执行**：用户要求取消学习是
   删除动作，敏感策略不应阻止它。
3. **读取侧要不要也停？** 建议**一并停**：`Suspend` 时前端同时不要调用
   `MemoryStore::lookup` / `predict_next`，避免把其他应用学到的东西
   作为候选送进密码框。这是产品决定，但默认应当是"不显示"。
4. **策略由前端按焦点窗口决定**，不由引擎猜：

   | 前端 | 判据（Android） | 判据（Windows TSF，未实现） |
   | --- | --- | --- |
   | 敏感 | `EditorInfo.inputType` 含 `TYPE_TEXT_VARIATION_PASSWORD` / `TYPE_TEXT_VARIATION_WEB_PASSWORD` / `TYPE_TEXT_VARIATION_VISIBLE_PASSWORD`；或 `IME_FLAG_NO_PERSONALIZED_LEARNING` / `IME_FLAG_NO_SUGGESTIONS`；或焦点应用在用户的"私密应用"清单里 | 密码字段标志 / UIA 的 `IsPassword` / 用户清单 |

5. **可观察**：策略切换应当有日志/计数（"本次输入未学习"），
   否则会变成"静默不生效"——这是本项目反复踩的坑。
6. **可测试**：至少要有
   `suspend_drops_learned_but_still_forgets`、
   `learn_after_suspend_resumes`、
   `suspend_does_not_change_the_memory_file` 三条。

### 4.3 前端合同（现在就要遵守）

即使 API 还没实现，前端**现在就应当**：

- 在拿到焦点时判定敏感输入，并把结论存在会话里（不要每键重新判）；
- 敏感会话里**不调用** `apply_events`（临时规避；API 落地后换成 `Suspend`）；
- 敏感会话里**不显示**个人化候选（`Lane::Input` 的记忆重排、`Lane::Predict`、
  向量重排）；
- **不把敏感输入写进任何日志或崩溃上报**。

---

## 5. 清除与导出

### 5.1 已有 API（真实名字，代码位置）

| 能力 | API | 位置 | 语义 |
| --- | --- | --- | --- |
| 导出输入表 | `FileMemory::snapshot() -> Vec<MemoryEntry>` | `store.rs:430` | 只读视图，按 `(输入, 词)` 升序；字段：`input`、`text`、`count`、`bonus`、`last_used` |
| 导出预测表 | `FileMemory::prediction_snapshot() -> Vec<PredictionEntry>` | `store.rs:452` | 只读视图；字段：`context`、`text`、`count`、`bonus`、`last_used` |
| 取消一条学习 | `MemoryStore::forget(key, text)` | trait：`qingjian-core/src/service.rs:637`；实现：`store.rs:910` | 按**与记录相同的键**删除输入表的一条记录 |
| 落盘 | `FileMemory::flush() -> Result<bool, MemoryError>` | `store.rs:521` | 全量重写 + 原子替换；任何失败保留 dirty，可重试 |

可观察出口已接到 CLI：`qingjian --userdb <路径> --dump-memory`
把两张表逐条打印（`--dump-memory` 会打印上屏文本，属于**用户显式要求**的导出）。

### 5.2 明确的缺口（不粉饰）

- **`forget` 只删输入表，删不掉预测表**。原因：`Event::ForgetRequested`
  不带上下文，而预测表的主键是上下文（`store.rs:910` 的注释记为"已知的接口缺口"）。
  删除语义要等 `ForgetRequested` 带上完整键后再设计。
- **没有"全部清除"API**。当前唯一的全量清除是：**在引擎未运行时删除记忆
  文件（及 `<path>.lock`）**。应用内"一键清除"需要新 API（建议
  `MemoryStore::clear_all()`，并同时清两张表、落盘、返回是否成功）。
- **`snapshot()` / `prediction_snapshot()` 不脱敏**：导出的是原文，
  这是有意的（它回答"到底学到了什么"），但**导出文件与记忆文件同等敏感**，
  前端在实现"导出"时必须按本文件的分类处理（不放到共享目录、不自动上传）。

### 5.3 删除的真实含义

- 删除文件是**删除**；`forget` 是**取消一条记录**（`count` 归零后不再影响排序）；
- 文件是**明文**，删除后**可能从磁盘镜像/备份中恢复**；
- 若记忆被同步/备份带走（§8），本机删除**不会**删除远端副本。

---

## 6. 日志规则

**引擎现状**：`crates/qingjian-memory/src/*.rs` 的运行时路径**没有**
`println!` / `eprintln!`；错误通过 `MemoryError` 返回
（`Io(std::io::Error)` / `Corrupt(String)`），`Corrupt` 只带**结构原因**
（魔数、版本、校验和），**不带记忆内容**。CLI 的 `⚠` 提示同样只报
"文件坏了 / 未打开"这类结构性事实。

**规则（对前端有约束力）**：

1. **禁止**在日志、崩溃上报、诊断包中出现：上屏文本、编码/规范编码键、
   上下文、记忆文件字节、`MemoryEntry` / `PredictionEntry` 的逐条内容。
2. **允许**记录：条数、容量、耗时、文件路径、错误种类（`Io` / `Corrupt`）、
   策略状态（"本次未学习"）。
3. **路径也算敏感**：日志里的 `--userdb` 路径可能含用户名。
   对外报错时应只在本地显示，不上传。
4. **不要以"verbose/debug 模式"为由绕过**：调试开关不是数据外泄的许可证。
   需要看内容时用 `--dump-memory`（显式、本机、用户自己看）。
5. **无遥测**：崩溃上报若将来引入，必须**默认关闭、显式同意、内容零上传**；
   在它落地前，本文件不承诺"有崩溃上报"这种能力。

---

## 7. 第三方库 / 模型

- **registry 依赖：0**。`Cargo.lock` 只有 workspace 成员；
  没有第三方运行时库，因此没有"某个库把数据发出去"的路径。
  门禁：`scripts/verify-zero-deps.sh`、`scripts/verify-deps.sh`。
- **模型：没有**。`qingjian-embed`（P5）是**零依赖、无模型**的本地计数投影：
  把本地历史里的 `(上下文 → 下一个词)` 计数投影成 `i16` 向量，
  不加载任何神经网络、不下载权重。
- **随仓库分发的第三方数据**：默认词库（派生自 MIT / Apache-2.0 数据）
  与两份 wiki 副本等。逐项来源 / revision / 版权 / 许可见
  `THIRD_PARTY_NOTICES.md`；**其中不含任何用户数据**。
- **取回但不分发的数据**（`build/`，`.gitignore`）：
  含 rime-ice 的 emoji 三件套（**GPL-3.0-only**）。它们是**上游公开数据**，
  不是用户数据；但它们会**留在本机**，因此：
  - 不随仓库分发，也不进引擎二进制；
  - 使用者若再分发自己的 `build/` 副本，需自行满足 GPL-3.0。

---

## 8. Android 权限与备份（前端合同，**未实现**）

### 8.1 权限

| 权限 | 是否需要 | 理由 |
| --- | --- | --- |
| `INTERNET` | **不需要，不申请** | 引擎无网络代码（§1）。输入法请求网络权限本身就是一个需要解释的信号 |
| `ACCESS_NETWORK_STATE` | 不需要 | 同上 |
| `READ/WRITE_EXTERNAL_STORAGE` | **不需要** | 记忆写在 app-private storage |
| `VIBRATE`、`POST_NOTIFICATIONS` 等 | 按功能另议 | 与隐私模型无关，但应在设置页可见 |

### 8.2 存储

- 记忆文件放 `getFilesDir()`（或 `noBackupFilesDir()`，见下），
  **不放**外部存储、`getExternalFilesDir()`、共享目录；
- Android 上 `0600` 语义由 app 沙箱提供（每个 app 一个 UID）；
  **不要**为了"方便导出"把文件拷到共享目录——导出必须是显式动作（§5）。

### 8.3 备份与设备迁移

记忆是**本机行为历史**，默认**不应**进入 Android 自动备份 / 云迁移：

- `AndroidManifest.xml` 用 `android:fullBackupContent`（API ≤ 30）
  与 `android:dataExtractionRules`（API 31+）**排除记忆文件**；
- 或把记忆放进 `noBackupFilesDir()`；
- 若产品决定允许备份，必须在设置页**显式说明**并让用户选择进/出，
  不能默认开启。

### 8.4 生命周期

`onStop` / `onTrimMemory(TRIM_MEMORY_UI_HIDDEN)` 落盘、
空闲 **≥ 30 s** debounce、按键路径**不落盘**、失败**保留 dirty** 并重试、
连续失败在设置页可见——完整合同见 `docs/memory-persistence-design.md` §5。
**不要只依赖正常进程退出**（系统可能在后台直接杀进程）。

### 8.5 其他前端面

- 剪贴板、截图、无障碍（Accessibility）能力都会绕过"引擎无网络"这一层；
  需要它们时单独评估，不纳入本模型；
- 系统级输入法可能被第三方主题/皮肤/插件扩展——**只安装本项目的签名包**；
- 用户可能把 `--userdb` 指到一个云同步目录（Dropbox / 网盘）。
  引擎无法阻止，前端在设置页给出警告。

---

## 9. 结论：什么是代码保证的，什么不是

### 9.1 代码层面**已核实**的保证

| 保证 | 证据 |
| --- | --- |
| 引擎不做网络请求、不发遥测 | §1.2 的符号搜索；`Cargo.lock` 0 个 registry 依赖 |
| 用户记忆**默认关闭** | `crates/qingjian-cli/src/main.rs`（不给 `--userdb` 即无记忆） |
| 用户记忆的按键路径零磁盘 I/O（词库查询仍 `read_at`） | `tests/no_disk_io_on_keypath.rs`（`/proc/self/io`） |
| 记忆文件 Unix `0600` | `store.rs: write_private()`；`tests/persistence_failures.rs::the_memory_file_is_owner_only` |
| 坏文件不自动覆盖 | `open_or_degrade` + `writable == false` |
| 不记按键日志 | §2：只存**提交过的词**、编码、上下文与计数/时间 |
| 无模型、无第三方运行时库 | §7；`verify-zero-deps.sh` |

### 9.2 依赖前端 / 操作系统的部分（**本模型只给合同，不给保证**）

- 记忆文件放在哪、父目录权限如何；
- 系统备份、云同步、设备迁移是否带走文件；
- 操作系统/第三方崩溃上报、调试日志是否会记录输入内容；
- 焦点应用是否敏感（密码框、私密应用）——**§4 的 API 尚未实现**；
- 同一用户下的其他进程、root、恶意软件、取证；
- 磁盘是否加密；
- 应用商店/系统对输入法的额外采集；
- 用户把记忆路径指向同步目录。

### 9.3 一句话总结

**引擎当前可以诚实地声称：不联网、不遥测、默认不记、记了也只在本地、
只记提交结果且按键路径不落盘。**
它**不能**声称："因此你的输入是私密的"——
在敏感输入禁学 API、Android 私有存储与备份排除、日志规则落地之前，
隐私的完整性取决于前端与操作系统。本文件把这两类话分开写，
就是为了不让第一类事实被当成第二类结论。

---

*相关文件：`docs/memory-persistence-design.md`（落盘状态机与 Android 生命
周期合同）、`THIRD_PARTY_NOTICES.md`（第三方数据与许可）、
`crates/qingjian-core/src/service.rs`（`MemoryStore`）、
`crates/qingjian-memory/src/{store,file,events}.rs`。*
