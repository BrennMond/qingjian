# 阶段 3–4 验收证据：配置资源图、兼容性声明、词库质量与合规

> **对应**：`docs/INDEPENDENT_AUDIT_AND_DEEPSEEK_PLAN.md` §4 阶段 3 与阶段 4。
>
> **本页必须与"没做什么"一起读。** 阶段 4 的第 1 项
> （词级读音与词库质量评测）**没有完成**，见 §5。

---

## 0. 门禁

```bash
cargo fmt --all -- --check                                                  # ✓
cargo clippy --workspace --all-targets --offline --locked -- -D warnings     # ✓
cargo test --workspace --offline --locked                                    # ✓ 38 个测试目标全绿
for f in scripts/verify-*.sh; do bash "$f"; done                             # ✓ 四项门禁
target/release/stele --check                                                 # ✓ 8 组不变式
```

---

## 1. 阶段 3 第 1–2 项：`TableLexicon` 的前缀能力（审计 G4）

### 缺口是怎么第二次出现的

阶段 2 加 `Lexicon::has_prefix` 时，`TableLexicon` **声明了**
`supports_prefix() == true` 却**没有实现** `prefix_lookup`
（trait 默认是空实现）。后果与审计 §2.G4 一模一样，只是换个触发点：

```text
# 修复前：同一个 shape 方案、同一个输入 `ab`，两条装载路径给出不同候选
$ stele --schema shape --candidates=all ab            # 内嵌（内存词库）→ 4 条
  1. 木  2. 十  3. 才  4. ab
$ stele --scheme-dir schemes/stele-default --schema shape --candidates=all ab
  1. 十  2. ab                                        # 部署词库 → 2 条
```

`stele --check` 直接报了出来：`shape/ab 应当上屏「十」，得到「木」`。

### 修复后

- `TableLexicon::prefix_lookup`：前缀是**连续区间**，二分下界 + 顺序扫到
  不再匹配为止；与内存实现**逐字段一致**（文本 / 分数 / `attr=COMPLETION` /
  `kind=Completion`）。
- 新增 `crates/stele-schemes/tests/lexicon_capability.rs`：把两台实现的
  `supports_prefix` / `lookup` / `has_prefix` / `prefix_lookup`（两种
  `exclude_exact`）收成同一个 `Probe` 面，8 组编码逐项对照。
  **任何一处分叉都会红**——这是"能力声明必须与实现一致"的可执行版本。
- 顺带修掉一个语义错误：`ExactCodeTranslator` 不给补全候选扣分，
  于是"打全的「十」排在没打全的「木」后面"。两个翻译器现在共用
  `COMPLETION_COST`。

---

## 2. 阶段 3 第 1 项：多实例独立词库（审计 G3）

### 审计的原样复现

```text
> 构造两个 `script_translator`，主词库含「甲」、`script_translator@other`
> 词库含「乙」，输入同码仅得到「甲」。装配路径持续使用 `self.lexicon`。
```

### 修复后

```text
$ cargo test -p stele-schemes --test instance_dictionaries --offline
test two_script_translator_instances_use_their_own_dictionaries ... ok
test two_table_translator_instances_use_their_own_dictionaries ... ok
test an_instance_without_its_own_dictionary_falls_back_to_the_main_one ... ok
test a_declared_instance_dictionary_is_no_longer_reported_as_unsupported ... ok
test an_unloadable_instance_dictionary_is_reported_not_ignored ... ok
test result: ok. 5 passed
```

### 做了什么

- `SchemeDef.extra_lexicons: Vec<(String, DictSource)>`。
  装载器为每个 `@别名` 实例按**与主词库完全相同**的策略造词库
  （内联读表 / 部署编译）——"实例词库"与"主词库"没有任何语义差别。
- `LoadedScheme::lexicon_for(alias)`：按别名选词库；
  别名没有独立词库时**落回主词库**（RIME 的默认行为）。
- 旧的"实例词库不生效"降级诊断改为**只在真的没造出来时**才出声——
  一条与事实相反的警告比没有警告更糟，这条由
  `a_declared_instance_dictionary_is_no_longer_reported_as_unsupported`
  与 `a_parsed_instance_dictionary_is_no_longer_a_degradation` 两条守着。

### 顺带暴露的一处坏数据

`crates/stele-schemes/tests/schemes/p3phrases.dict.yaml` 与 `p3chaizi.dict.yaml`
的条目写成「编码在前、词在后」（`dz\t石经输入法`），与解析器期望的顺序相反。
**这从来没被校验过**，因为实例词库在装配时被丢掉了。现在它会真的装载，
于是装载期**响亮拒绝**。夹具已按正确顺序修好，并给 `p3features` 的字母表
补上 `d/z/y/x` 四个单元。

---

## 3. 阶段 3 第 3–4 项：配置字段审计表（审计 G5）

交付 `docs/config-field-audit.md` + 可执行的
`crates/stele-schemes/tests/config_field_audit.rs`（7 条行为断言）。

### 修掉的两处"解析了没生效"

| 字段 | 修复前 | 修复后 |
| --- | --- | --- |
| `initial_quality` | **解析了、存进 spec、从来没人读**（只在注释里出现过） | 逐实例的权重倍率；两条装配路径都消费；测试断言 `1.1` 的分数增量恰为 `ln(1.1) ≈ 95` 毫对数，且兜底翻译器不受影响 |
| `enable_completion`（短写法路径） | `engine.translator: spelling_graph` 的装配分支**完全不读** `translator:` 段 | 两条装配路径消费同一份 spec；`the_short_form_assembly_consumes_the_translator_section` 守着 |

### 不支持时的诊断

`SchemeDef::check_translator_specs` 逐个实例检查，并把话说清楚：

- 实例 `dictionary` **没造出来** ⇒ 出声（造出来了就不出声）；
- 拼音族的 `enable_sentence` ⇒ "上游同款没有这个开关"；
- 码表族的 `enable_sentence: true` ⇒ "造句器还没有装配点"。

这一检查放在 `is_declared()` 早退**之前**：短写法没有 `engine:` 列表，
但 `translator:` 段照样会被解析——那正是最容易"写了不生效"的组合。

---

## 4. 阶段 3 第 5 项：坏方案的目录加载策略（审计 G1）

| 入口 | 语义 |
| --- | --- |
| `load_dir_reporting` / `load_dir_deployed_reporting` | 跳过坏方案并**逐条报告**（启动路径） |
| `load_dir_layered` / `load_dir_deployed_layered` | **严格**：任何跳过都算失败（CI / 测试 / 打包校验） |

判决表：至少一个装上 ⇒ `Ok` + `skipped`；一个都没装上 ⇒ `Err`；
目录读不了 / 没有方案文件 ⇒ `Err`。

CLI 启动路径改用报告入口，并把被跳过的方案逐条打印。
测试 `directory_load_policy.rs` 覆盖混合目录、全坏、空目录、
以及"读不出文件"（目录冒充方案文件）四种情况。

---

## 5. 阶段 4：**做了什么**与**没做什么**

### 5.1 已交付（J1 许可与可复现来源）

- **`THIRD_PARTY_NOTICES.md`**：逐项列出 artifact / 上游 URL / 固定
  commit / 版权 / 许可 / 是否修改 / 许可文本指针。
- **`tools/sources.lock`**：**已跟踪**的源数据锁（16 条固定 revision +
  SHA-256）。`tools/fetch-sources.sh` 由浮动 `main/master` 改为按锁下载并
  **强校验**；哈希不符即失败。
- **`licenses/`**：MIT（pinyin-data / THUOCL / jieba）与
  BSD-3-Clause（librime）原文。
- **GPL 边界**：从 rime-ice（GPL-3.0-only）复制/改写的三份 Lua
  **已移除**；对照测试改为只依赖保留下来的 `.expected.txt`
  （**输出事实**，不是代码），`cargo test -p stele-engine --test
  number_oracle --test calc_oracle` 仍然全绿。
- `schemes/stele-default/opencc.manifest.yaml` 里 emoji 的许可由
  `Apache-2.0` 更正为 `GPL-3.0-only`（该数据不随仓库分发，但标注必须真实）。

### 5.2 已交付（J1 隐私模型）

`docs/privacy-model.md`：数据分类、磁盘位置与权限（`0600`，见
`docs/memory-persistence-design.md`）、**敏感输入禁学 API 的设计**
（明确标注"尚未实现"）、清除与导出（用真实的
`snapshot()` / `prediction_snapshot()` / `forget()` / `flush()`）、
日志规则、第三方库与模型、Android 权限与备份。
**明确区分"代码已核实"与"依赖前端/OS"**——
不把"没有联网代码"说成完整的隐私证明。

### 5.3 已交付（README / PLAN / HANDOFF 事实校正）

删掉或加上条件的：仓库"不包含第三方词典数据"、"按键路径零磁盘 I/O"
（限定为用户记忆这条路径；`TableLexicon` 仍走 `read_at`）、
"`<30 MB / P99 <10 ms` 普遍达标"（补上测量条件与反例数字）、
对 Rime 隐私/现代性的无证据推论、"完全/直接兼容 Rime"（改为受限子集）。

### 5.4 ❌ **没有完成**：词级读音与词库质量评测（审计 §2.I）

**状态：未做。** 这是阶段 4 的第 1 项，也是审计列出的 P2 里最大的一块：

- 默认词库仍是"单字首读音 + 有限启发式"，因此
  `yinhang` 不召回「银行」、`chongqing` 不召回「重庆」、
  错误读音反而会召回（审计 §2.I 的四行表**没有**被修）；
- 没有词级拼音数据源、没有人工覆盖表；
- 没有独立的质量评测集（常用词 / 地名 / 人名 / 多音字 / 简拼 / 混输的
  召回率、首选率、前五命中、错误类型）。

**为什么没做**：它需要引入一份可分发的词级拼音数据（含许可审查）、
改 `tools/wordlist-gen` 的生成逻辑并**重新生成 41 万条词库**——那是一次
数据工程量的工作，不是一次代码修改。把它压在"顺手做掉"里只会得到
一份没有评测、也没有来源记录的新词库，那恰恰是审计反对的做法。

**交接**：审计 §2.I 的 5 条要求逐条可执行；`docs/config-field-audit.md`
的维护规则与 `THIRD_PARTY_NOTICES.md` 的表格结构已经为"新增一份
数据源 + 评测集"准备好了落点。

### 5.5 其它已知未完成项

| 项 | 状态 |
| --- | --- |
| 阅读 `reference/wiki-*.md` 的许可 | **UNVERIFIED**：rime/home 无通用 LICENSE、wiki 页面无声明；已在 notices §5.3 标为待决（取得许可 / 改自撰摘要 / 移除） |
| `tools/librime-probe/probe.c` 文件头 | 缺 BSD-3-Clause 随附声明（notices §1.4 已指向 licenses/） |
| 已提交的 `generated.dict.yaml` 头部 | 把 jieba 标成 THUOCL；生成器已修，**下次重新生成即消失**（未手改产物） |
| git 历史 | 仍含被移除的 GPL Lua 旧版本（历史改写不在本次范围） |
| 会话分段 / 余码 / 重开（阶段 2 的 F） | 未做，见 `docs/validation/phase-2.md` §8 |
| 正则引擎的自动机替换 | 只有缓解 + 设计，见 `docs/regex-engine-design.md` |
| 词典约束搜索进搜索内部 | 未做（阶段 2 的性能回退的正解） |

---

## 6. 一句话状态

审计 §6.1「正确性」清单的 10 条，现在**全部有对应的行为测试**：

| 检查项 | 证据 |
| --- | --- |
| alphabet 重排不复用错误缓存 | `cache_identity.rs` |
| 随机/畸形 `.table` 永不 panic | `table_integrity.rs`（含伪造校验和那一格） |
| 写盘失败可重试且不清 dirty | `persistence_failures.rs` |
| 两写者不会静默覆盖 | 同上 |
| 读取后遵守记忆容量 | 同上 |
| `xlit` 与 Rime 语义一致 | `spelling.rs::xlit_is_a_rewrite_not_a_derivation` |
| recognizer shortcut 不漏真匹配 | `regex_and_recognizer.rs`（不变式测试） |
| 同一字段有解析/装配/效果/测试 | `config_field_audit.rs` + `config-field-audit.md` |
| 多 translator 实例使用各自资源 | `instance_dictionaries.rs` |
| 内存/部署词库能力不静默差异 | `lexicon_capability.rs` |

**未达成的是"完成"本身**：阶段 4 的词库质量工作（§5.4）与阶段 2 的会话
闭环（任务包 F）仍然是缺口，本页不把它们算作已交付。
