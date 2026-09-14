# 执行结果验收与后续整改指示

> **验收时间**：当前工作区 `main`（相对独立审计基线 `4a2bb1a`）
>
> **用途**：这是给下一位执行者的验收结论与明确工作指令。它不替代
> `PLAN.md`、`docs/HANDOFF.md` 和 `docs/INDEPENDENT_AUDIT_AND_DEEPSEEK_PLAN.md`；
> 开工前仍须阅读那些文件及本文件列出的相关设计文档。
>
> **结论先行**：审计整改的阶段 1、2、3–4 的主要实现已落地，常规门禁和测试全部通过；
> 但存在已被刻意标记、且能实际复现的未完成项。因此不得将当前状态表述为“全部完成”。

---

## 1. 本次验收的工作区状态

- 分支：`main`
- 工作区：干净，未发现未提交改动。
- 审计基线：`4a2bb1a`。
- 已包含阶段 1–4 的整改提交，以及 P4a/P4b、P5 第一版等后续工作。
- 当前项目仍是**内部引擎 + CLI + 实验性默认词库**；没有 Windows TSF 或 Android IME 成品。

下面命令已在当前工作区执行并通过：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline --locked -- -D warnings
cargo test --workspace --offline --locked
for f in scripts/verify-*.sh; do bash "$f"; done
cargo run -p qingjian-cli --offline --locked -- --check
```

结果摘要：

| 检查 | 结果 |
| --- | --- |
| `cargo fmt` | 通过 |
| workspace Clippy（warnings 视为错误） | 通过 |
| workspace 常规测试 | 通过；其中有 2 条显式 `#[ignore]` 的已知缺口测试 |
| `verify-deps.sh` | 通过：0 个 registry 依赖，白名单/重复依赖检查通过 |
| `verify-no-ime-vocab.sh` | 通过：内核无输入法专属词汇 |
| `verify-no-scheme-data.sh` | 通过：内核与方案数据分离 |
| `verify-zero-deps.sh` | 通过：`qingjian-core` / `qingjian-engine` 零第三方依赖 |
| `qingjian --check` | 通过：8 组内核不变式全部成立 |

> 常规 `cargo test` 通过不等于已完成全部工作：`#[ignore]` 项必须单独以
> `--include-ignored` 运行，且文档中的“已知限制”仍然有效。

---

## 2. 已验收的整改范围

以下并非只看文档或注册表，而是已具备对应行为测试；证据索引见
`docs/validation/README.md` 与三个阶段报告。

### 2.1 阶段 1：可靠性与资源上界

已完成并有回归测试：

1. **部署词库缓存身份**：缓存 fingerprint 纳入字母表内容和顺序、词典源与编译语义；
   重排 alphabet 时不会复用导致静默错码的旧缓存。
   - 证据：`crates/qingjian-schemes/tests/cache_identity.rs`。
2. **`.table` 畸形产物安全性**：加载期结构验证、主体校验和、读侧 checked 运算；
   篡改 offset、截断、追加字节、翻位不能使 release 查询 panic。
   - 证据：`crates/qingjian-table/tests/table_integrity.rs`。
3. **用户记忆持久化**：flush 失败保留 dirty、可重试；唯一临时文件；文件锁与合并；
   恢复时施加容量；新文件 Unix 权限为 0600。
   - 证据：`crates/qingjian-memory/tests/persistence_failures.rs`；
     设计：`docs/memory-persistence-design.md`。
4. **拼写展开资源硬上限**：arena 父指针状态、`max_results` / `max_units` /
   `max_states` / `max_work`、统计计数器和确定性截断。
   - 证据：`crates/qingjian-schemes/tests/spelling_resource_bounds.rs`。
5. **正则/recognizer 临时防护**：required-prefix 不再产生假阴性；嵌套量词拒绝；
   步数与深度预算防止灾难性回溯。
   - 证据：`crates/qingjian-engine/tests/regex_and_recognizer.rs`。

### 2.2 阶段 2：解码与会话闭环的已交付部分

已完成并有测试：

1. 可解释前缀消费、候选 span 与余码保留；
2. 拼写层补全（默认开启）与词条补全的区分；
3. 有界词图动态规划造句；
4. `xlit` 修正为改写（不再把原拼写作为派生结果保留）；
5. `TableLexicon` 与内存词库的前缀能力对齐；
6. 部分选词后余码继续保留、重开最近一次上屏、标点语义的明确产品决定；
7. 候选与预测通道的数字键选择隔离。

证据：

- `crates/qingjian-schemes/tests/decoder_matrix.rs`；
- `crates/qingjian-schemes/tests/session_state_machine.rs`；
- `crates/qingjian-schemes/tests/punctuation_semantics.rs`；
- `crates/qingjian-schemes/tests/lexicon_capability.rs`；
- `docs/validation/phase-2.md`。

### 2.3 阶段 3–4：配置、资源图、合规与质量工作的已交付部分

已完成并有测试：

1. 多 `script_translator` / `table_translator` 实例可各自装载词库；
2. 声称支持的配置字段已有解析、装配、消费和行为测试（见字段审计表）；
3. 坏方案在启动型目录加载中会被跳过并逐项报告；严格入口仍会失败；
4. 第三方 notices、固定 revision 与 SHA-256 来源锁、许可证文件；
5. 隐私模型明确区分代码已核实的行为和依赖前端/OS 的边界；
6. 审计点名的多音字例已由人工覆盖表改善，26 条小型质量集会报告召回/首选/前五结果。

证据：

- `crates/qingjian-schemes/tests/instance_dictionaries.rs`；
- `crates/qingjian-schemes/tests/config_field_audit.rs`；
- `crates/qingjian-schemes/tests/directory_load_policy.rs`；
- `crates/qingjian-schemes/tests/word_pinyin_quality.rs`；
- `docs/config-field-audit.md`；
- `docs/privacy-model.md`；
- `THIRD_PARTY_NOTICES.md`；
- `docs/validation/phase-3-4.md`。

---

## 3. 本次实际复现的未完成项

### 3.1 P0：生成器仍会把「家」错误编码为 `jie`

此项不是推测，已实际执行被忽略的验收测试：

```bash
cargo test -p qingjian-schemes --test word_pinyin_quality \
  --offline --locked -- --include-ignored
```

结果：**失败**。

```text
the_wrong_reading_of_a_generator_entry_does_not_hijack_a_correct_one ... FAILED
assertion `left != right` failed: 「家」被编成了 `jie`：单字读音取错了
left: Some("家")
right: Some("家")
```

当前人工覆盖表已补 `家 jia`，所以 `jia` 可以正确召回；但生成词库中的
错误条目仍然存在，仍会抢占 `jie` 的结果。此问题的根治不在覆盖表，而在
`tools/wordlist-gen` 的单字读音/编码生成逻辑及重新生成的产物。

相关记录：

- `docs/validation/phase-3-4.md` §5.4.1；
- `crates/qingjian-schemes/tests/word_pinyin_quality.rs`；
- `schemes/qingjian-default/cn_dicts/word_pinyin.override.dict.yaml`。

### 3.2 P1：会话模型仍有结构性边界

已执行：

```bash
cargo test -p qingjian-schemes --test session_state_machine \
  --offline --locked -- --include-ignored
```

结果：17 条均通过。注意其中 `reopen_is_not_a_full_commit_history` 通过的含义是：
它验证**已知边界被正确记录**，不是这些边界已被消除。

仍缺：

- 提交历史栈；
- 已确认段的显式类型/标记；
- 任意 span 的候选覆盖和光标编辑；
- 重开后避免重复学习等完整语义。

详见 `docs/validation/phase-2.md` §8。

### 3.3 P1：正则仍是“预算缓解”，不是自动机实现

现在的正则安全策略足以阻止已审计的回溯爆炸，但仍不是最终的线性/多项式
时间自动机实现。Pike VM 设计及迁移约束在：

- `docs/regex-engine-design.md`；
- `docs/validation/README.md` 的已知缺口。

在自动机替换落地前，嵌套量词拒绝与步数/深度预算不得删除。

### 3.4 P1：词典约束搜索尚未以正确的数据结构融入主搜索

`has_prefix` 与两种词库能力已经实现；但曾尝试的“每个边尝试都重建编码并查询”
在真实词库上净退化，已回滚。正确方向是让拼写搜索 arena 节点携带词典前缀状态，
避免反复重建路径和二分查询。

- 现状和实测否决记录：`docs/decoder-design.md` §2.4；
- 该优化不得通过缩小预算、直接退化字面量或删除召回断言来伪造完成。

### 3.5 P2：词级读音工作仍只是局部人工覆盖

当前 26 条人工质量集的通过，只说明被枚举的高频/审计点名样本已有改善；
不代表 41 万词条已有可靠词级读音。仍缺：

- 经许可审查、可分发或可复现取得的全量词级拼音来源；
- “词级读音优先”的生成管线；
- 格式层面对“同一词多个读音及权重”的一等表达；
- 更大且有错误类型分类的质量评测集。

### 3.6 P2：许可待决项

**✅ 已处置（2026-09-14）**：`reference/wiki-*.md` 的许可状态是 `UNVERIFIED`
——**不得**把该项表述为已完成的许可证闭环。本项目选择了三种处置里的第三项：
**移除逐字副本**。这两份文件已从工作区**与全部 git 历史**中删除，
移除前的核实结果、代价与回溯用的上游 URL 记在
`THIRD_PARTY_NOTICES.md` §1.4 / §5.3。

---

## 4. 发现的文档不同步（应优先修正文档）

`docs/config-field-audit.md` 的“翻译器实例字段”表中，
`dictionary`（`@别名` 实例）仍写为：

> 未装配 / 无消费 / 仅降级诊断

这已与实际代码、阶段 3–4 验收记录及测试相矛盾。下面命令已经通过：

```bash
cargo test -p qingjian-schemes --test instance_dictionaries --offline --locked
```

它覆盖 script/table 两族实例独立词库、无实例词库时回退主词库、不可加载实例词库的
明确报告。因此应更新该表为实际的解析、装配、消费位置与测试引用；不要继续保留
“未装配”的旧状态。

---

## 5. 下一位执行者的任务指令

### 5.1 第一优先级：修正生成器错误读音（必须使忽略测试转绿）

**目标**：从生成器层面消除 `家 → jie` 之类的错误编码，不能只继续堆人工覆盖。

**必做：**

1. 开工前阅读：
   - `docs/validation/phase-3-4.md` §5.4–§5.4.1；
   - `tools/wordlist-gen/` 的实现与 README；
   - `crates/qingjian-schemes/tests/word_pinyin_quality.rs`。
2. 找出单字首读音/同声母微调为何会把「家」生成成 `jie`；修正算法或数据解析。
3. 增加最小单元/集成回归，不能只依赖最终大词库测试。
4. 重新生成默认词库前，必须运行 `bash tools/fetch-sources.sh` 的来源校验；
   重新生成后检查变更的来源、许可头和词条数量是否合理。
5. 将以下测试从 `#[ignore]` 移为正常测试，且不得改弱断言：

   ```bash
   cargo test -p qingjian-schemes --test word_pinyin_quality \
     --offline --locked -- --include-ignored
   ```

6. 保留“一词多读音不必因修正而删除另一读音”的既有测试语义。
7. 更新 `docs/validation/phase-3-4.md`、`docs/validation/README.md` 与相关
   词库文件头，准确说明完成范围和仍然存在的词级读音限制。

**验收线：**

- `jie` 的首选不再是由错误生成条目造成的「家」；
- `jia` 仍可召回「家」；
- 常规质量集和新增回归均通过；
- 不能以删掉测试、仅修改预期或把输入退化为字面量通过。

### 5.2 文档同步：修正 translator 别名词库状态

更新 `docs/config-field-audit.md` 的 `dictionary`（`@别名` 实例）行，写入真实的：

- 解析位置；
- 装配位置；
- 运行消费位置；
- 行为测试：`instance_dictionaries.rs`；
- 仅在实例字典确实无法装载时才出现的诊断语义。

完成后应复核本表所有“已支持”字段是否仍满足“解析 → 装配 → 消费 → 行为测试”的四段链条。

### 5.3 后续任务必须先由所有者排序

以下均是有效工作，但**不要未经所有者明确指示自动开始**：

1. 会话阶段 C：确认段、任意 span、光标与完整提交历史；
2. 正则 Pike VM / 自动机替换；
3. arena 节点携带词典前缀状态的约束搜索；
4. 全量词级读音数据源、许可证审查和质量评测；
5. ~~`reference/wiki-*.md` 的许可处置；~~ **已处置（2026-09-14）**：移除逐字副本（工作区与全部历史），见 `THIRD_PARTY_NOTICES.md` §1.4 / §5.3；
6. recognizer 的三处与 librime 的语义分叉；
7. `select` 切方案与 `send_sequence`；
8. P5 更大、由所有者手写的收益对比集。

其中涉及 recognizer、按键绑定或上游行为时，动手前须阅读：

- `reference/rime-recognizer-and-affix.md`；
- `reference/rime-key-binding-actions.md`；
- 必要时 `tools/oracle/README.md`。

---

## 6. 每次提交前的最低验证清单

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline --locked -- -D warnings
cargo test --workspace --offline --locked
for f in scripts/verify-*.sh; do bash "$f"; done
cargo run -p qingjian-cli --offline --locked -- --check
```

按改动范围追加：

```bash
# 改默认词库、生成器、读音质量：
cargo test -p qingjian-schemes --test word_pinyin_quality --offline --locked -- --include-ignored

# 改装配、engine 清单、Services 或内联零件：
cargo test -p qingjian-schemes --test inline_components --offline --locked
cargo test -p qingjian-schemes --test instance_dictionaries --offline --locked
cargo test -p qingjian-schemes --test config_field_audit --offline --locked

# 改解码、拼写、候选范围或资源预算：
cargo test -p qingjian-schemes --test decoder_matrix --offline --locked
cargo test -p qingjian-schemes --test spelling_resource_bounds --offline --locked

# 改正则或 recognizer：
cargo test -p qingjian-engine --test regex_and_recognizer --offline --locked

# 改记忆持久化：
cargo test -p qingjian-memory --test persistence_failures --offline --locked
cargo test -p qingjian-memory --test no_disk_io_on_keypath --offline --locked
```

涉及真实默认词库的性能结论时，必须注明方案、词库、缓存冷热状态、记忆/预测/向量开关，
并使用 `qingjian-bench` 覆盖短、长、歧义、无效输入；不得只引用 `nihao` 的短输入数据。

---

## 7. 对外表述约束

在上述未完成项消除前，不得宣传或暗示：

- 已可替代成熟日常中文输入法；
- 对 Rime 方案完全或直接兼容；
- 整条真实按键路径零磁盘 I/O（`TableLexicon` 仍会 `read_at`；仅用户记忆热路径有零 I/O 证据）；
- `<30 MB` / `P99 <10 ms` 已覆盖所有输入、所有设备与前端；
- 默认实验词库已有完整可靠的词级读音；
- Rime 因未写隐私承诺而必然不隐私或会联网；
- 第三方内容与许可清理已无待决项。

推荐定位仍是：**受 Rime 思想启发的 Rust 输入法引擎研究/原型，提供实验性方案、
词库和 CLI 验证环境；重点是可审计性、离线可选记忆和轻量化探索。**
