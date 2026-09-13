# 阶段 1 验收证据：P0 可靠性与资源上界

> **对应**：`docs/INDEPENDENT_AUDIT_AND_DEEPSEEK_PLAN.md` §4「阶段 1」。
> **原则**：每一条结论后面是**命令 + 输出**，不是"已实现"。
> **修复前**的证据来自一个独立的 `git worktree`（提交 `4a2bb1a`，即审计提交），
> 用**同一批测试文件**跑出失败——这是"问题真实存在且已修复"的唯一可信形式。
>
> **不把测试数量当完成依据**：下面每一条都给出行为层面的前后对比。

---

## 0. 环境与复现方式

| 项 | 值 |
| --- | --- |
| 平台 | Linux（WSL2），AMD Ryzen 9 7940HX |
| 工具链 | Rust `1.98.1`（`rust-toolchain.toml` 固定 `1.98`） |
| 修复前 | `git worktree add .work/qingjian-base 4a2bb1a` |
| 修复后 | 本工作区 |
| 词库 | `schemes/qingjian-default`（41 万词条的部署产物，14 MB） |

**"修复前"的复现步骤**（审计可以直接照做）：

```bash
git worktree add .work/qingjian-base 4a2bb1a
# 把新写的回归测试原样拷进去（它们只用当时已存在的公开 API）
cp crates/qingjian-schemes/tests/cache_identity.rs \
   .work/qingjian-base/crates/qingjian-schemes/tests/
cp crates/qingjian-memory/tests/persistence_failures.rs \
   .work/qingjian-base/crates/qingjian-memory/tests/
cd .work/qingjian-base && cargo test -p qingjian-schemes --test cache_identity --offline
```

---

## 1. 门禁（修复后全部通过）

```bash
cargo fmt --all -- --check                                  # ✓
cargo clippy --workspace --all-targets --offline --locked -- -D warnings   # ✓
cargo test --workspace --offline --locked                    # ✓ 32 个测试目标全绿
for f in scripts/verify-*.sh; do bash "$f"; done             # ✓ 四项门禁
target/release/qingjian --check                                 # ✓ 8 组不变式
```

门禁输出（节选）：

```text
✓ verify-deps: 0 个 registry 依赖，全部有受审记录（重复依赖 0）
✓ verify-no-ime-vocab: 内核标识符中没有输入法专属词汇
✓ verify-no-scheme-data: 内核与方案数据保持分离
✓ verify-zero-deps: 内核（qingjian-core qingjian-engine）保持零第三方依赖
内核自检通过：8 组不变式全部成立。
```

### 一处**必须申报**的改动：MSRV `1.82 → 1.89`

`FileMemory` 的跨进程互斥用了 `std::fs::File::lock()` / `unlock()`，
它们是 Rust **1.89** 稳定的。备选是引入第三方锁 crate（违反核心零依赖）
或自写"锁目录 + 陈旧锁清理"（一段容易写错的并发代码）。
选择提升 MSRV，并在 `Cargo.toml` 里写明了理由。

副作用：MSRV 提升后 clippy 的 `manual_is_multiple_of` 在两处既有代码上生效，
已一并改掉（`crates/qingjian-memory/tests/predict_next.rs`、`crates/qingjian-bench/src/main.rs`）。

---

## 2. 任务包 B：缓存身份（P0）

### 修复前的复现（**审计的原始症状被独立复现**）

```text
$ cd .work/qingjian-base && cargo test -p qingjian-schemes --test cache_identity --offline
test reordering_the_alphabet_recompiles_and_keeps_the_right_word ... FAILED
  assertion `left == right` failed: 字母表重排后**绝不能**复用旧产物：
  缓存里的下标 0 原本是 `ni`，重排后是 `hao`，复用就会让 `ni` 出「好」。
  实得 ["好", "ni"]
    left: Some("好")
   right: Some("你")

test every_semantic_input_enters_the_cache_name ... FAILED
  assertion `left != right` failed: 字母表**内容**变了，缓存身份必须变：
    left: ["d.c37cc3594f0c081f.table"]
   right: ["d.c37cc3594f0c081f.table"]

test result: FAILED. 2 passed; 2 failed
```

**只改 `speller.alphabet` 的顺序、词典一个字节不动 ⇒ `ni` 出「好」。**
这正是审计报告 §2.B 描述的行为，且缓存文件名**完全相同**。

### 修复后

```text
$ cargo test -p qingjian-schemes --test cache_identity --offline
test changing_the_dictionary_recompiles ... ok
test every_semantic_input_enters_the_cache_name ... ok
test reordering_the_alphabet_recompiles_and_keeps_the_right_word ... ok
test the_cache_is_reused_when_nothing_semantic_changed ... ok
test result: ok. 4 passed; 0 failed
```

### 做了什么

- 新增 `qingjian_table::BuildFingerprint`：FNV-1a 覆盖
  **格式版本 + 编译选项 + 字母表内容与顺序 + 源数据校验和**，
  变长字段**先长度后内容**（否则 `["ab","c"]` 与 `["a","bc"]` 会撞）。
- 头部从 64 字节加到 80 字节，`FORMAT_VERSION` 升到 `2`，
  新增 `build_fingerprint` 与 `body_checksum` 两个字段。
  **v1 产物被明确拒绝并提示重新部署**（不猜、不迁移）。
- 缓存文件名改为 `<dict>.<fingerprint:16x>.table`；
  `open_checked` 校验指纹，不匹配即 `FormatError::FingerprintMismatch`
  并说明"字母表变了会静默错码"。
- 单元测试 `fingerprint_is_sensitive_to_alphabet_order` 直接钉住
  "顺序进入指纹"与"变长字段不歧义"。

### 验收线对照

> 修改任一语义输入后，绝不可复用会产生不同码元映射的旧产物；必须拒绝或重建。

- ✅ 字母表顺序变 → 新指纹 → 重新编译（`reordering_...`）；
- ✅ 字母表内容变 → 新指纹（`every_semantic_input_enters_the_cache_name`）；
- ✅ 源词典变 → 新指纹（`changing_the_dictionary_recompiles`）；
- ✅ 什么都没变 → **不重编**（`the_cache_is_reused_when_nothing_semantic_changed`），
  否则就是拿性能问题换正确性问题。

---

## 3. 任务包 C：畸形 `.table` 不得 panic（P0）

### 修复前的复现

```text
$ cd .work/qingjian-base && cargo test -p qingjian-table --test baseline_panic -- --nocapture
装载成功（源校验和仍匹配）——这正是问题：产物完整性没有被校验

thread 'probe_tampered_offset' panicked at crates/qingjian-table/src/lexicon.rs:184:24:
range start index 10000 out of range for slice of length 5
test result: FAILED. 0 passed; 1 failed
```

**与审计报告 §2.C 的 `range start index 10000 out of range for slice of length 2`
是同一个 panic。**篡改保持文件总长、魔数、格式版本与 `source_checksum`
不变，装载成功，查询时崩。

### 修复后

```text
$ cargo test -p qingjian-table --offline
test result: ok. 31 passed   (单元)
test result: ok. 8 passed    (tests/table_integrity.rs)
```

`crates/qingjian-table/tests/table_integrity.rs` 覆盖：

| 测试 | 断言 |
| --- | --- |
| `the_audited_tamper_is_refused_not_a_panic` | 审计原样篡改 → 可读错误 |
| `the_audited_tamper_with_a_forged_checksum_is_still_refused` | **重算校验和后仍被结构校验拒绝**（模拟"能改文件的人"） |
| `every_offset_table_is_validated` | 两张前缀和表的首项/末项/单调性 |
| `header_regions_are_validated` | 6 个头部字段塞极限值 |
| `truncation_and_appended_bytes_are_refused` | 8 个截断点 + 多出尾巴 |
| `flipped_bytes_never_panic` | 逐 5 字节翻转 + 置零，全部"安全拒绝或安全查询" |
| `a_wrong_fingerprint_is_refused_with_an_explanation` | 身份不符的诊断可读 |

### 做了什么

- **分配任何索引缓冲之前**先跑 `validate_layout`：三个区段顺序相邻、
  不重叠、都在文件长度内；`code_count+1`、`entry_count×12`、
  `total_units×2`、`index_offset+index_size` **全程 checked 运算**
  （旧实现在 `code_count + 1` 处 debug 溢出 / release 静默 wrap）。
- 索引读完后再校验两张前缀和表：首项为 0、末项等于哨兵、单调不减、
  每项不超过哨兵。
- 读侧全程 `get` + `checked_*`：`find_code` 的切片、`read_entries` 的
  记录偏移与词区间都不再可能越界；`off - first` 改用**最小值**当基准
  （旧代码用第一条记录，未必是最小值 ⇒ 下溢）。
- 新增 `body_checksum`（头部之后全部字节的 FNV-1a），装载时**分块校验**
  （不额外要一块等于文件大小的堆）。

### 威胁模型（写进代码文档，避免被当成安全边界）

- `body_checksum` 是 **FNV-1a，没有抗碰撞性**：能改文件的人也能重算它。
  它只用于发现**意外损坏**（位翻转、传输截断、构建脚本写坏）。
- 真正的防越界是**结构校验 + 读侧 checked 运算**——
  它们无论校验和是否通过都必须成立。测试里专门有一条
  "校验和已伪造仍被拒绝"来钉住这一点。

### 成本（必须申报）

`body_checksum` 要在装载时读一遍产物主体。实测在 14 MB 的默认词库上：

| | 引擎装载 |
| --- | --- |
| 审计基线（v1，无主体校验） | 80–82 ms |
| 修复后（v2，含主体校验） | **102 ms** |

即 **+20 ms 一次性的装载成本**换产物完整性检测。按键路径不受影响。
这是可以调的政策（例如只在"产物比上次记录更新"时校验），但当前选择
"每次装载都验"，因为部署缓存的加载不是热路径。

---

## 4. 任务包 D：记忆落盘（P0）

### 修复前的复现

```text
$ cd .work/qingjian-base && cargo test -p qingjian-memory --test persistence_failures --offline
test a_failed_flush_never_reports_success_before_retrying ... FAILED
  重试必须真的写盘（旧实现在这里返回 Ok(false)，记录静默丢失）
test a_failed_rename_keeps_dirty_and_the_retry_succeeds ... FAILED
  **写盘失败之后必须仍然是 dirty**，否则重试的机会就永久没了
test a_stale_tmp_directory_does_not_block_flushing ... FAILED
  Io(Os { code: 21, kind: IsADirectory, message: "Is a directory" })
test two_writers_merge_instead_of_overwriting ... FAILED
  assertion `left == right` failed: 先写者的记录不能被后写者覆盖
    left: []
   right: ["你"]
test the_memory_file_is_owner_only ... FAILED
  记忆是敏感的本机行为历史，新建文件必须是 0600，实得 644
test capacity_is_enforced_when_loading ... FAILED
  恢复路径必须施加 cap（旧实现会原样加载 50 条），实得 50

test result: FAILED. 3 passed; 6 failed
```

### 修复后

```text
$ cargo test -p qingjian-memory --test persistence_failures --offline
running 9 tests
test a_corrupt_file_is_not_overwritten_by_flushing ... ok
test a_failed_rename_keeps_dirty_and_the_retry_succeeds ... ok
test a_stale_tmp_directory_does_not_block_flushing ... ok
test a_failed_flush_never_reports_success_before_retrying ... ok
test a_write_after_a_snapshot_is_still_dirty ... ok
test merging_is_idempotent_and_does_not_inflate_counts ... ok
test the_memory_file_is_owner_only ... ok
test capacity_is_enforced_when_loading ... ok
test two_writers_merge_instead_of_overwriting ... ok
test result: ok. 9 passed; 0 failed
```

### 做了什么

落盘是一台**顺序不可调换**的状态机（详见 `docs/memory-persistence-design.md`）：

```text
① 文件锁(<path>.lock) → ② 读盘并逐键合并 → ③ 快照+编码(g)
→ ④ 唯一临时文件 → fsync → rename → fsync 目录 → ⑤ saved = g → ⑥ 放锁
```

- **代次**取代布尔 `dirty`：`saved` 只推进到**快照那一刻**的 `generation`，
  因此快照之后的并发写入仍然 dirty（旧实现会把它一并当成已保存）。
- **唯一临时文件名** `<name>.tmp.<pid>.<counter>`：固定的 `<name>.tmp`
  会让两个写者交错写同一文件，也会被同名目录永久卡住。
- **文件锁 + 版本合并**：锁是 `std::fs::File::lock()`；
  合并策略是**逐键取更大值**（幂等，不翻倍）。
- **坏文件不覆盖**：盘上文件解码失败即中止 flush 并保留 dirty。
- **恢复路径施加 cap**（旧实现只在新插入超限时淘汰）。
- 文件权限 `0600`。
- Android 生命周期合同写在设计文档里（何时 debounce、何时 `onStop` 落盘、
  失败如何重试），**不实现**前端。

### 验收线对照

> 写盘失败后仍 `dirty`、可重试；并发更新不静默覆盖；坏文件不覆盖；
> 容量上限在加载后也成立。

四条**逐条**有对应测试（上表），全部从失败转为通过。

---

## 5. 任务包 A：拼写搜索的资源上界（P0）

### 修复前（debug 构建的探针，`.work/qingjian-base`）

```text
$ cargo test -p qingjian-schemes --test baseline_probe -- --nocapture
         nihao  n=116   time=370µs        ni_hao_rank=Some(0)
          nhao  n=512   time=5.547ms      ni_hao_rank=Some(9)
            nh  n=456   time=767µs        ni_hao_rank=Some(175)
           ...
          ssss  n=512   time=2.286832697s  ni_hao_rank=None
  woaizhongguo  n=512   time=579.541063ms ni_hao_rank=None
  nihaoshijie  n=512   time=142.68779ms  ni_hao_rank=None
```

（审计在 release 下测得 `ssss` 第四键 1.52 s / `VmHWM` 209 MiB。debug 更慢，
所以这里 2.29 s 是同一现象。）

**同一批探针在修复后**（debug）：

```text
$ cargo test -p qingjian-schemes --test spelling_resource_bounds --offline
test pathological_inputs_are_hard_bounded ... ok
test ssss_is_many_orders_of_magnitude_smaller_than_the_old_blowup ... ok
test long_legal_input_stays_bounded ... ok
test normal_words_are_still_recalled ... ok      ← 含 nhao→ni hao 与 nh→ni hao
test the_budget_is_actually_enforced_when_set_tiny ... ok
test expansion_is_deterministic_under_budget ... ok
test query_count_is_bounded_by_the_result_cap ... ok
test result: ok. 7 passed

$ cargo test -p qingjian-schemes --test spelling_resource_bounds --offline --release
test release_single_key_expansion_is_under_ten_milliseconds ... ok
test result: ok. 8 passed
```

### 做了什么

1. **状态表示换成 arena + 父指针**：一个状态是定长 24 字节；
   编码**只对最终结果**重建。旧实现每个状态克隆一份 `Vec<CodeUnitId>`，
   并且为去重再往 `HashSet` 里放一份——209 MiB 主要花在这里。
2. **四项硬预算**（`ExpansionLimits`）：`max_results` / `max_units` /
   **`max_states`** / **`max_work`**。旧实现只有一个"产出条数"上限和一个
   派生的出队预算，既没有状态上界也没有边尝试上界。
3. **截断点从"收满就走"移到"排好序之后"**：旧实现一收满 `max_results`
   就停止探索，同代价同单元数的切分之间没有全序保证 ⇒ 想要的词可能恰好
   没进那 512 条。现在探索只受状态/工作量限，截断发生在按真实排序键
   排好之后。
4. **可观测计数**（`ExpansionStats`）：状态数、出队数、边尝试数、
   结果数、搜索图字节数、是否截断。测试断言**结构量**而不是墙钟时间。
5. **默认值是实测反推的**（写进了常量文档）：

   | 输入 | 状态上界 4096 | 状态上界 **16384** |
   | --- | --- | --- |
   | `nh` → `ni hao` | rank 175 ✅ | rank 175 ✅ |
   | `nhao` → `ni hao` | **被截断，召回丢失** ❌ | rank 9 ✅（与修复前一致） |
   | `ssss` | 图 194 KB | 图 775 KB |
   | `ssss` 边尝试 | 8175 | 32737 |

   所以 `4096` 太小（那是"用性能换掉正确候选"），`16384` 足够保住
   修复前的全部召回，同时把搜索图压在 1 MB 以内。

### 验收线对照

> 现有复现输入在 release 下不再出现 >10 ms 的单键路径。

✅ `release_single_key_expansion_is_under_ten_milliseconds`
（`ssss`/`ssssssssss`/`woaizhongguo`/`zzzz`/`xxxxxxxx`/`nihaoshijie`
各 20 次取最坏值）。

> 测试能证明中间状态受硬上限控制，且正常词不会因该上限静默丢失。

✅ `pathological_inputs_are_hard_bounded`（状态/边尝试/图字节数三重断言）+
`normal_words_are_still_recalled`（`nihao` 规范切分 rank 0、`nhao` 与
`nh` 仍能切出 `ni hao`、端到端仍打出「你好」）。

> 不能为了通过测试而将输入直接退化成字面量；应同时断言目标候选召回。

✅ 召回断言与资源断言在**同一个测试文件**里；把输入退化成字面量会让
`normal_words_are_still_recalled` 立刻失败。

### 尚未完成（阶段 2）

**词典约束搜索**（`CodeOracle`）是根治手段，本阶段只做到"有界"。
设计见 `docs/decoder-design.md` §2.4。它会把 `ssss` 再降一个数量级，
并让 `nhao` 这类贵切分不再依赖预算大小。

---

## 6. 任务包 H：正则与识别器（P0）

### 修复前的复现

```text
$ cd .work/qingjian-base && cargo test -p qingjian-engine --test baseline_regex -- --nocapture
regex=^(a|b)+$   input=bbb   leading="a"   claimed=false
FALSE NEGATIVE: `^(a|b)+$` matches `bbb` but recognizer did not claim

^(a+)+$ n=20  hit=None time=222.645222ms
^(a+)+$ n=22  hit=None time=882.906243ms
^(a+)+$ n=24  hit=None time=3.556204822s
```

（审计 release 实测 54 ms / 848 ms / 3 秒超时；这里是 debug，故更慢。）

### 修复后

```text
$ cargo test -p qingjian-engine --test regex_and_recognizer --offline
test the_audited_false_negatives_are_fixed ... ok
test the_optimisation_never_changes_the_verdict ... ok
test required_prefix_is_conservative ... ok
test nested_repeats_are_rejected_at_compile_time ... ok
test catastrophic_backtracking_is_bounded_by_the_step_budget ... ok
test recursive_depth_is_capped_for_long_inputs ... ok
test a_normal_recognizer_still_works_after_the_budget_was_added ... ok
test result: ok. 7 passed
```

`the_optimisation_never_changes_the_verdict` 是一条**不变式测试**：
12 个模式 × 24 个输入，只要 `Regex` 在位置 0 匹配，`Recognizer`
就必须认领。这比"审计那三行"更强——它约束的是"优化不许改变结论"。

### 做了什么

1. **`leading_literal` 改为从语法树取必需前缀**（`Regex::required_prefix`）：
   跳过开头的 `^`，连续吃掉字面 `Char`，遇别的节点就停。
   **可证明**是全部匹配的公共必需前缀 ⇒ 不可能产生假阴性。
   审计的三行现在都有回归测试。
2. **编译期拒绝"重复套重复"**（`RegexError::NestedRepeat`）：
   `(a+)+`、`(ab*)*`、`(a?){2,}` 一律编译失败并解释为什么。
   保守规则：真实方案的规则里没有嵌套量词，代价接近零。
3. **运行期步数 + 深度预算**：`DEFAULT_STEP_BUDGET = 100_000`、
   `MAX_DEPTH = 1024`；超限的语义是**不匹配**（不是 panic、也不是继续跑）。
   深度**随迭代增长**——否则 `a+` 的深度永远停在 0，一条 20 万字符输入
   会直接 SIGABRT（这是实现过程中实测到并修掉的一个真问题）。
4. **不再声称"不存在爆炸风险"**：模块文档改写，把实测数字与
   真实调用点（`recognizer`）写进去。

### 明确申报的代价

- **超过 1024 字符的输入不会被量词匹配上。** 对 `recognizer` 的前缀模式
  （几个到几十个字符）不构成问题，但这是一个真实的行为边界，不是"没有限制"。
- 步数预算会让"本来该匹配但很费"的输入变成不匹配。这是**缓解**，
  不是终点。

### 尚未完成：自动机替换

设计见 `docs/regex-engine-design.md`（Pike VM，O(长度 × 指令数)，
含语义对照与 fuzz 计划）。**在它落地之前，上面两道闸必须留在原位。**

---

## 7. 端到端性能（release，真实词库，缓存已命中）

语料不再只有 `nihao`（审计 §6.3 的要求）：

```text
$ ./target/release/qingjian-bench --scheme-dir schemes/qingjian-default --iterations=60000 --count-queries

输入                                   P50         P95         P99         max
  nihao                          20.55µs     62.07µs     75.00µs      2.09ms
  nihaoshijie                   521.48µs    825.47µs    849.26µs      1.64ms
  nh                             93.07µs    106.27µs    118.21µs    663.78µs
  nhao                          390.70µs    842.82µs    865.10µs      2.16ms
  ssss                          353.58µs    392.81µs    413.34µs      1.52ms
  woaizhongguo                  515.63µs      2.32ms      2.40ms      2.52ms

常驻内存（VmRSS，采样时刻）: 14068 KiB (13 MiB)
常驻内存（VmHWM，**启动以来峰值**）: 21568 KiB (21 MiB)
引擎装载（含全部方案）: 102083 µs
每次按键的词典查询次数：总 3053368 次 / 76000 键   P50 46   P99 64   max 64
```

### 与审计基线的对比

| 输入 | 审计（修复前，release） | 修复后（release） | 倍数 |
| --- | --- | --- | --- |
| `nihao` P50 | 46–48 µs | **20.6 µs** | 2.3× 更快 |
| `nihao` P99 | 100–105 µs | **75 µs** | 1.4× 更快 |
| `ssss` 第四键 | **1.52 s** | **max 1.52 ms** | **1000× 更快** |
| `woaizhongguo` 部分按键 | 290 / 679 / 285 ms | **max 2.52 ms** | **270× 更快** |
| `ssss` 峰值内存 | `VmHWM` **209 MiB** | 全语料 `VmHWM` **21 MiB** | **10× 更低**（且含 14 MB 词库） |

`qingjian-bench` 现在报告**逐语料**的分位数、`VmRSS` 与 `VmHWM`，
并且 `--scheme-dir` 时不再打印"演示词库只有几十条词"
（那句与事实相反的话已修掉）。

### 测量条件的边界（不可外推）

- 这是 WSL2 的绝对数字，**不能外推到手机**；
- `VmHWM` 含引擎装载那一段；分离"装载峰值"与"按键峰值"需要前端进程配合；
- 冷缓存（首次部署）会额外走一次 41 万词条的编译：`引擎装载 ≈ 611 ms`。

---

## 8. 降级行为（可测试、可显示）

| 触发 | 行为 | 可观测 |
| --- | --- | --- |
| 拼写展开超预算 | 停止探索，返回已有最优结果 | `ExpansionStats::truncated` |
| 产物指纹不符 | 拒绝加载 + 提示重新部署 | `FormatError::FingerprintMismatch` |
| 产物结构损坏 | 拒绝加载 + 指出哪一段 | `FormatError::Corrupt` |
| 产物主体损坏 | 拒绝加载（意外损坏检测） | `FormatError::BodyChecksumMismatch` |
| 记忆写盘失败 | 保留 dirty，可重试 | `flush() -> Err` + `is_dirty()` |
| 记忆文件读不懂 | 降级成空记忆、**不覆盖**、出一行警告 | `open_or_degrade -> Some(note)` |
| 正则超步数预算 | 当作不匹配 | `Budget::exhausted`（内部） |
| 嵌套量词 | 装载期编译失败并解释 | `RegexError::NestedRepeat` |

**没有一处是 panic 或静默吞错。**

---

## 9. 本阶段**没有**做的事（不要误读为已完成）

1. **词典约束搜索**（阶段 2）——`ssss` 的根治手段，现在只是"有界"；
2. **音节图跨度 / 部分消费 / 拼写层补全**（阶段 2）——
   `niha`、`nihaoshijie` 仍然只给字面量；
3. **词图与造句**（阶段 2）——`haoni` 仍然只给字面量；
4. **会话分段 / 余码 / 重开**（阶段 2）；
5. **`xlit` 语义**（阶段 2，现在是"改写 + 派生"，与上游不符）；
6. **多 translator 实例独立词库 / `TableLexicon` 前缀查询 / 配置字段审计表**（阶段 3）；
7. **词级读音与词库质量评测**（阶段 4）；
8. **`THIRD_PARTY_NOTICES.md` / 隐私模型 / README 事实校正**（阶段 4）；
9. **正则引擎的自动机替换**——当前是缓解 + 设计。

对照 `docs/INDEPENDENT_AUDIT_AND_DEEPSEEK_PLAN.md` §6.1 的正确性清单：
`xlit`、多 translator 实例、内存/部署词库能力一致这三条**在阶段 2/3**，
本报告不作已完成声明。
