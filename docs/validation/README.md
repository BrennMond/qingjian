# 验收索引：审计 §6 的检查单 → 证据

> **用途**：审计 agent 的检查单（`docs/INDEPENDENT_AUDIT_AND_DEEPSEEK_PLAN.md` §6）
> 有四组共 25 条。这一页把**每一条**映射到它的可执行证据（测试文件 / 测试名 /
> 命令），并**如实标注哪几条还没达成**。
>
> **判据**：一条检查项只有在"有一条**行为测试**会因为它被破坏而变红"时才算达成。
> 注释、文档、字段被解析——都不算。
>
> **前两列不是承诺，是位置。** 没有对应测试的项在"状态"列里写 ❌ 或 ⚠️。

---

## 一次跑完全部证据

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline --locked -- -D warnings
cargo test --workspace --offline --locked          # 40 个测试目标
cargo test --workspace --offline --locked -- --include-ignored   # 含唯一一条 #[ignore]（会话边界）
for f in scripts/verify-*.sh; do bash "$f"; done   # 四项门禁
target/release/qingjian --check                       # 8 组不变式
bash tools/fetch-sources.sh                        # 源数据固定 revision + sha256 校验
```

阶段报告：`phase-1.md`（P0 可靠性与资源上界）、`phase-2.md`（解码与会话）、
`phase-3-4.md`（配置资源图、兼容性声明、词库质量与合规）。

---

## 6.1 正确性

| # | 检查项 | 证据 | 状态 |
| --- | --- | --- | --- |
| 1 | alphabet 重排不复用错误缓存 | `crates/qingjian-schemes/tests/cache_identity.rs`（4 条，含"确实重新编译"与"没变就不重编"） | ✅ |
| 2 | 随机/畸形 `.table` 永不 panic | `crates/qingjian-table/tests/table_integrity.rs`（8 条，含**伪造校验和后仍被结构校验拒绝**、逐字节翻转/置零） | ✅ |
| 3 | 写盘失败可重试且不会清 dirty | `crates/qingjian-memory/tests/persistence_failures.rs::a_failed_rename_keeps_dirty_and_the_retry_succeeds` | ✅ |
| 4 | 两写者不会静默覆盖 | 同上 `::two_writers_merge_instead_of_overwriting` + `::merging_is_idempotent_and_does_not_inflate_counts` | ✅ |
| 5 | 读取后遵守记忆容量 | 同上 `::capacity_is_enforced_when_loading` | ✅ |
| 6 | `xlit` 与 Rime 语义一致 | `crates/qingjian-engine/src/spelling.rs::tests::xlit_is_a_rewrite_not_a_derivation`（`aa` 必须**失效**、`bb` 有效、只留一条边） | ✅ |
| 7 | recognizer shortcut 不会漏掉 regex 真匹配 | `crates/qingjian-engine/tests/regex_and_recognizer.rs::the_optimisation_never_changes_the_verdict`（12 模式 × 24 输入的不变式） | ✅ |
| 8 | 同一配置字段有解析、装配、实际效果和测试 | `docs/config-field-audit.md` + `crates/qingjian-schemes/tests/config_field_audit.rs`（7 条）；不支持的字段有降级诊断 | ✅ |
| 9 | 多 translator 实例使用各自资源 | `crates/qingjian-schemes/tests/instance_dictionaries.rs`（5 条：script/table 两族、继承、缺失诊断） | ✅ |
| 10 | 内存/部署词库能力不发生静默差异 | `crates/qingjian-schemes/tests/lexicon_capability.rs`（两台实现 × 4 项能力 × 8 组编码逐项对照） | ✅ |

---

## 6.2 解码体验

| # | 检查项 | 证据 | 状态 |
| --- | --- | --- | --- |
| 1 | `nihao`/`niha`/`nih`/`nh`/`haoni`/`nihaoshijie` 都有定义明确的预期 | `crates/qingjian-schemes/tests/decoder_matrix.rs`（15 条）。`nih` → 你好、`nh` → 你好、`nhao` → 你好 也都在 `word_pinyin_quality.rs` 的简拼用例里 | ✅ |
| 2 | 候选消费范围和余码有测试 | `decoder_matrix.rs::niha_gives_the_word_for_the_interpreted_prefix`（`span=0..3`、余码 `a`、attr=ABBREV）+ `::committing_a_prefix_candidate_keeps_the_remainder_in_the_input`（上屏后余码仍在输入里） | ✅ |
| 3 | 逐段选择、重开、删除、标点、中英/数字混输有状态机测试 | `crates/qingjian-schemes/tests/session_state_machine.rs`（**17 条通过 + 1 条 `#[ignore]`**）：部分选词后继续输入、选第二段、Backspace/Delete/Esc、**重开已上屏的段**、数字选词、中英开关、预测与数字选择的隔离、取消与重开的交互；标点在 `punctuation_semantics.rs`（3 条）。`#[ignore]` 的那条记录的是**已知边界**：重开只有一条记录（没有提交历史栈）、没有"确认段"标记、重复上屏会重复学习 | ✅ |
| 4 | 动态造句不会把单词重复作为"句子"，不会无限展开 | `decoder_matrix.rs::sentence_making_does_not_repeat_the_same_word`、`::a_single_word_is_never_reported_as_a_sentence`；上限 `MAX_SENTENCE_WORDS`、边严格向前 | ✅ |
| 5 | 正确候选召回和资源预算同时通过 | `decoder_matrix.rs::the_target_candidates_are_present_not_just_the_literal` 与 `spelling_resource_bounds.rs`（同一批语料上同时断言召回与状态/工作量/图字节上界） | ✅ |

---

## 6.3 性能与内存

| # | 检查项 | 证据 | 状态 |
| --- | --- | --- | --- |
| 1 | 短、长、歧义、无效输入均入基准 | `qingjian-bench --keys` 默认六条：`nihao`（短）/`nihaoshijie`（长）/`nh`、`nhao`（歧义）/`ssss`、`woaizhongguo`（病态） | ✅ |
| 2 | 报告 P50/P95/P99/max、状态数、查询数、VmRSS/VmHWM | `qingjian-bench` 逐语料分位数 + `VmRSS`/`VmHWM` + `--count-queries`；状态数在图/工作量上界测试里（`ExpansionStats`），并断言为**硬上限** | ✅ |
| 3 | 区分进程 RSS、内核页缓存、系统总内存、冷/热缓存 | `qingjian-bench` 现在同时报 `VmRSS`/`VmHWM`（进程）、`MemTotal`/`MemAvailable`/`Cached`（系统 + **内核页缓存**），并明确写出"`TableLexicon` 读过的产物页在页缓存里、**不计入 VmRSS**，只看 RSS 会低估真实占用"。冷/热由"缓存是否命中"区分（报告里注明） | ✅ |
| 4 | 证明复杂度上界，不仅报告某次机器上的快数字 | `docs/decoder-design.md` §5 的复杂度表（`L`/`E`/`W` 记法）+ `spelling_resource_bounds.rs` 对**状态数/边尝试数/图字节数**的硬上限断言（与机器无关） | ✅ |
| 5 | 可选记忆/预测/向量的内存另计，默认与显式开启分开报告 | `qingjian-bench --seed-memory/--seed-predict/--embed` 各自单独报；默认关闭（不给 `--userdb` 就没有记忆） | ✅ |

---

## 6.4 文档与合规

| # | 检查项 | 证据 | 状态 |
| --- | --- | --- | --- |
| 1 | README 状态、测试数、功能边界更新 | `README.md` 已按阶段 1–4 的事实校正（删掉"不含第三方词典数据"、"完全兼容 Rime"等） | ✅ |
| 2 | 删除或改正对 Rime 隐私/现代性的无证据推论 | `README.md` / `PLAN.md` / `docs/HANDOFF.md` 已删；改为审计 §3.1 的口径 | ✅ |
| 3 | 默认词库明示实验性质和已知词级读音限制 | `schemes/qingjian-default/pinyin.dict.yaml` 与 `cn_dicts/word_pinyin.override.dict.yaml` 文件头，以及生成词库头部（写明"单字取首选读音、不做覆盖、落盘前自检"）都明示了；**覆盖表只修了枚举到的词**，表外的多音字词只能按单字首选读音拼——没有可分发的词级读音数据源（`phase-3-4.md` §5.4.2） | ⚠️ |
| 4 | 第三方 notices、固定版本、哈希、许可证齐全 | `THIRD_PARTY_NOTICES.md`（400 行）、`tools/sources.lock`（16 条固定 revision + SHA-256）、`licenses/`；**`reference/wiki-*.md` 的许可 UNVERIFIED**（rime/home 无通用 LICENSE） | ⚠️ |
| 5 | 隐私模型不把"没有联网代码"简化成完整隐私证明 | `docs/privacy-model.md` 专门分节区分"代码已核实"与"依赖前端/OS"；禁学 API 明确标注**尚未实现** | ✅ |

---

## 未达成项汇总（**不要从这张表的 ✅ 推断全部完成**）

| 项 | 位置 | 缺口 |
| --- | --- | --- |
| 6.2 #3 会话状态机的**边界** | 阶段 2 任务包 F | 重开只有一条记录（无提交历史栈）、无"确认段"标记、候选只能覆盖"从头消费到 consumed"的 span |

| 6.4 #3 | 词库质量 | 没有可分发的**词级拼音数据集**；覆盖表是人工枚举 |
| 6.4 #4 | 许可 | `reference/wiki-*.md` 许可未定（三种处置待决） |

**本轮已消除的两项**（此前记在这里的"生成器单字编码错误"与
"`--include-ignored` 下有一条编不过的 doctest"）：

- `家 → jie`：生成器改为只取 `pinyin.txt` 的首选读音，并加落盘前
  「码 = 首选读音」自检；原先 `#[ignore]` 的集成验收测试已转正，
  另加两条生成器单测。证据：`tools/wordlist-gen/src/main.rs`、
  `crates/qingjian-schemes/tests/word_pinyin_quality.rs`，
  过程与复现命令见 `phase-3-4.md` §5.4.1 与 `THIRD_PARTY_NOTICES.md` §6.4。
- `scheme::entry` 的 ` ```ignore ` 文档示例不是合法 Rust，`--include-ignored`
  会在它上面失败；现已改成会编译会跑的例子。

另外三处**功能**层面的已知缺口（不在 §6 检查单里，但同样是审计点名的）：

1. **正则引擎的自动机替换**——现在是"编译期拒绝嵌套量词 + 运行期步数/深度预算"
   的缓解，设计见 `docs/regex-engine-design.md`；
2. **词典约束搜索的落地方式**——`has_prefix` 已就位并被使用，但把它接在
   每次边尝试上实测净亏（`ssss` +155%），已回滚并记录在
   `docs/decoder-design.md` §2.4；正解需要 arena 节点自带前缀状态；
3. **`nihao` 的按键 P50 相对阶段 1 有 5.1× 回退**（20.6 µs → 105.9 µs，
   仍在 1 ms 红线内），代价来自"支持不完整输入"，正解同上。
