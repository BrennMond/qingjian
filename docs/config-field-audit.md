# 配置字段审计表

> **用途**：审计 §2.G5 要求建立一张表，逐字段回答"**解析了，然后呢？**"
> 判据不是"字段有没有被读进来"，而是**有没有运行期的效果**；
> 不支持时必须有一条**看得见的诊断**。
>
> **可执行版本**：`crates/qingjian-schemes/tests/config_field_audit.rs`
> ——本页每一个"已支持"都对应那里的一条行为断言。
>
> **本表只列"我们公开声称支持"的字段。** 不在这张表里的 Rime 字段，
> 默认就是**不支持**（见 §4）。

---

## 1. 判据

一个字段"真的支持"需要**四处都有**：

| # | 环节 | 在哪 |
| --- | --- | --- |
| 1 | **解析** | `crates/qingjian-schemes/src/components.rs`、`file.rs` |
| 2 | **装配** | `crates/qingjian-engine/src/scheme.rs`（`compile` / `make_translator`） |
| 3 | **消费** | `crates/qingjian-engine/src/translator.rs`、`processor.rs`、`filter.rs`… |
| 4 | **端到端测试** | `crates/*/tests/` |

少任何一处，用户看到的就是"我配了却没生效"——而那**不报错**。
`rust-analyzer` 的 references/call hierarchy 能发现"字段无人读取"，
但**只有行为测试能证明它真的被消费**（静态分析看不到"读了但算错了"）。

---

## 2. 翻译器实例字段

| 字段 | 解析 | 装配 | 消费 | 端到端测试 | 不支持时 |
| --- | --- | --- | --- | --- | --- |
| `dictionary`（主实例） | `components.rs:715`（`read_translator` 读 `dictionary`） | `file.rs:511`（内联 / 部署）→ `SchemeDef.dictionary` | `scheme.rs:295` 造词库；`translator.rs` 查询 | 全部集成测试 | — |
| `dictionary`（`@别名` 实例） | `components.rs:715`（`read_translator` 读实例块的 `dictionary`） | `file.rs:793-847`：按**与主词库完全相同**的策略造词库——内联 `load_with_imports`、部署 `deploy_dict`——存进 `SchemeDef.extra_lexicons`；`scheme.rs:311-328` 编译成 `LoadedScheme.extra_lexicons` | `scheme.rs:712` `lexicon_for(alias)` 按别名选词库；装配点在 `scheme.rs:1044`。别名没有独立词库时**落回主词库**（RIME 的默认行为） | `crates/qingjian-schemes/tests/instance_dictionaries.rs`（5 条：script / table 两族各用各的词库、无独立词库时回退主词库、声明了不再报"不支持"、装载失败被逐条报告） | ✅ 仅在**实例词库真的没造出来**时才出声：装载期 `file.rs:841-844` 收集诊断；`scheme.rs:403-411` 的 `check_translator_specs` 只在 `extra_lexicons` 里找不到该别名时报"会退回主词库" |
| `enable_completion` / `enable_word_completion` | `components.rs:719-720` | `scheme.rs` 两条装配路径都读 | `translator.rs` 的 `completion` 分支 | `config_field_audit.rs::completion_reaches_both_translator_families` | — |
| `enable_sentence` | `components.rs:721` | 存进 spec | **拼音族不看**（上游同款）；**码表族未实现** | `config_field_audit.rs::a_missing_feature_is_reported_not_silently_ignored` | ✅ **降级诊断** |
| `initial_quality` | `components.rs:722` | `scheme.rs` 两条路径都 `.with_initial_quality(...)` | `translator.rs` 加对数域常数 | `initial_quality_actually_changes_the_scores`、`..._is_per_translator_instance_not_global` | — |
| `comment_format` | `components.rs` | `SchemeDef.translator_specs` | `pipeline.rs` 渲染注释 | `p3_pipeline.rs` | — |
| `preedit_format` | 同上 | 同上 | `pipeline.rs` 渲染预编辑 | `engine_integration.rs` | — |
| `prefix`（`uU` 这类） | `components.rs` | `file.rs`（词缀切分器） | `segmentor.rs` 剥前缀 | `p3_pipeline.rs` | — |
| `tips` | `components.rs` | 存进 spec | **未消费** | — | ⚠️ 待补诊断 |

### 本次修正：`@别名` 实例词库那一行曾经与实现相反

本表此前把 `dictionary`（`@别名` 实例）写成「**未装配** / **无**消费 /
仅降级诊断」。这与实际代码、阶段 3–4 的验收记录和
`crates/qingjian-schemes/tests/instance_dictionaries.rs`（5 条全绿）**全部矛盾**：
实例词库早已被装载、编译、按别名选用。错误的那一行已按实际实现改写——
"一条与事实相反的记录，比没有记录更糟"。

顺带把这一节所有**行号**核对了一遍（解析点漂移会让"解析位置"这条链名存实亡）：

| 字段 | 旧行号 | 实际 |
| --- | --- | --- |
| `read_translator` 的 `dictionary` | `705` | `715` |
| `enable_word_completion` / `enable_completion` | `709-710` | `719-720` |
| `enable_sentence` | `711` | `721` |
| `initial_quality` | `712` | `722` |
| `punctuator` | `583` | `154` |
| `reverse_lookup_filter` | `578` | `584` |
| 主词库物化点 | `scheme.rs:285` | `scheme.rs:295` |

### 这四条里有两处是本次修复的

**① `enable_completion` 的短写法路径**：`engine.translator: spelling_graph`
（不带 `engine.translators` 列表）的装配分支**完全不读 `translator:` 段**，
于是 `enable_completion` 被解析、存进 spec、然后丢掉。
修法是让两条装配路径消费同一份 spec。
**发现方式**是"改一个默认值、看行为有没有变"——不是静态检查。

**② `initial_quality` 从来没被消费过**（只在注释里出现过）。
现已实现：语义是**这个实例**的权重倍率，换算到对数域就是加一个常数；
非正值按"不改变"处理。测试用 `1.1` 断言分数增量恰为 `ln(1.1) ≈ 95` 毫对数。

---

## 3. 方案级字段

| 字段 | 解析 | 装配 | 消费 | 测试 | 备注 |
| --- | --- | --- | --- | --- | --- |
| `schema.{schema_id,name,version,family}` | `file.rs` | `SchemeInfo` | `session.rs`（族共享）、CLI | `engine_integration.rs` | — |
| `switches` | `file.rs` | `Options::declare` | `Options::set`（**未声明不静默创建**） | `p3_pipeline.rs` | — |
| `speller.alphabet` | `file.rs`（列表与字符串两种写法） | `CodeAlphabet` | 拼写表、词库编译 | `cache_identity.rs`、`engine_integration.rs` | — |
| `speller.rules` / `algebra` | `file.rs:408` | `SpellingTable::compile` | `spelling.rs` | `spelling.rs` 单元测试 | — |
| `speller.delimiter` | `file.rs` | `preedit_delimiter` | `pipeline.rs` 分段显示 | `engine_integration.rs` | — |
| `translator.candidate_cap` | `components.rs` | `SchemeDef.candidate_cap` | `pipeline.rs` | `engine_integration.rs` | — |
| `punctuator` | `components.rs:154` | `Punctuator` | `punctuator.rs` | `p3_pipeline.rs` | 缺段时给默认 |
| `recognizer.patterns` | `components.rs` | `Recognizer` | `segmentor.rs::scan` | `regex_and_recognizer.rs`（含假阴性回归） | turn |
| `key_binder` | `components.rs` | `KeyBinder` | `processor.rs` | `p3_pipeline.rs` | — |
| `editor` 绑定 | `components.rs` | `Editor` | `processor.rs` | `p3_pipeline.rs` | — |
| `navigator`（翻页） | `components.rs` | `Navigator` | `processor.rs` | `p3_pipeline.rs` | — |
| `engine.{processors,segmentors,translators,filters}` | `file.rs` | `build_processors` / `build_segmentors` / `make_translator` | 各自 | `p3_pipeline.rs` | 声明了有实现却没装配的**逐条降级**（HANDOFF §5 第 36 条） |
| `reverse_lookup_filter@别名` | `components.rs:584` | `ReverseLookupFilter` + `ReverseLexicon` | `filter.rs` | `p3_pipeline.rs` | 数据源固定为主词库的反查索引 |
| `simplifier@别名`（如 emoji） | `components.rs` | `Converter` | `filter.rs` | `p3_pipeline.rs` | **数据缺失 ⇒ 降级，不是错误**（D26） |
| `opencc_config` | `file.rs` | `Converter` 的数据 | `filter.rs` | `p3_pipeline.rs` | 同上 |

---

## 4. 明确**不支持**的 Rime 字段（不在这张表里的默认结论）

审计 §2.G1 的实测是：本机 `/usr/share/rime-data` 的 **13 个上游 preset 方案
逐个隔离装载，0 个成功**。主要缺口：

1. 字典 `columns:`、`%` 百分比权重与非数值列；
2. 码表方案的无空格字符串编码（部分已支持：`speller.alphabet` 的字符串写法）；
3. 跨文件 `__patch` / `__include` 语义；
4. Rime 的单选组 `options:`；
5. 部分 X11 keysym 名称；
6. Rime preset 资产（`default` / `symbols`）未提供。

**因此对外只能说"Rime 风格的受限子集"。** 这条约束写在 README 与定位段里，
不在本表重复；本表的职责是**我们声称支持的那些**必须四处齐全。

---

## 5. 目录装载策略（与字段审计相邻的一条）

审计 §2.G1 还指出："一个坏方案目前还会中止整个目录加载；这与'配置错误不阻止启动'
的目标也有冲突。"

现已实现**跳过并报告**：

| 入口 | 语义 |
| --- | --- |
| `load_dir_reporting` / `load_dir_deployed_reporting` | 跳过坏方案并**逐条报告**（启动路径用） |
| `load_dir_layered` / `load_dir_deployed_layered` | **严格**：任何跳过都算失败（CI / 测试 / 打包校验） |

判决表：至少一个装上 ⇒ `Ok` + `skipped`；一个都没装上 ⇒ `Err`；
目录读不了 / 没有方案文件 ⇒ `Err`。
被跳过的方案必须由调用方**打印出来**——静默跳过就是"我配了却没生效"。
测试：`crates/qingjian-schemes/tests/directory_load_policy.rs`。

---

## 6. 维护规则

1. **新增一个被解析的字段** ⇒ 同时补上"装配 + 消费 + 一条行为测试"，
   或者补上一条降级诊断。两者都没有的字段**不许合入**。
2. 删掉一个消费点时，`config_field_audit.rs::the_audited_field_list_is_covered_by_this_file`
   会提醒你回来改这张表。
3. 这张表的"消费"列必须指向**真实文件**，不是"应该在某处"。
