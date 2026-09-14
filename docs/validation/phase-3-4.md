# 阶段 3–4 验收证据：配置资源图、兼容性声明、词库质量与合规

> **对应**：`docs/INDEPENDENT_AUDIT_AND_DEEPSEEK_PLAN.md` §4 阶段 3 与阶段 4。
>
> **本页必须与"没做什么"一起读。** 阶段 4 的第 1 项
> （词级读音与词库质量评测）**是部分完成**，见 §5：单字读音这一块
> 本轮修好了（§5.4.1），**词级读音**仍然没有可分发的数据源（§5.4.2）。

---

## 0. 门禁

```bash
cargo fmt --all -- --check                                                  # ✓
cargo clippy --workspace --all-targets --offline --locked -- -D warnings     # ✓
cargo test --workspace --offline --locked                                    # ✓ 全绿
cargo test --workspace --offline --locked -- --include-ignored               # ✓ 含唯一一条 #[ignore]（会话边界，见 phase-2.md §8）
for f in scripts/verify-*.sh; do bash "$f"; done                             # ✓ 四项门禁
target/release/qingjian --check                                                 # ✓ 8 组不变式
```

> **常规 `cargo test` 通过 ≠ 全部完成**。本页 §5.4 那条
> 词级读音的验收点原先是一条 `#[ignore]`——现在它**已转正**，
> 因为生成器的单字编码错误已经修掉（§5.4.1）。
> 工作区里剩下的唯一一条 `#[ignore]` 是
> `session_state_machine.rs::reopen_is_not_a_full_commit_history`，
> 它记录的是**仍然存在**的会话边界。

---

## 1. 阶段 3 第 1–2 项：`TableLexicon` 的前缀能力（审计 G4）

### 缺口是怎么第二次出现的

阶段 2 加 `Lexicon::has_prefix` 时，`TableLexicon` **声明了**
`supports_prefix() == true` 却**没有实现** `prefix_lookup`
（trait 默认是空实现）。后果与审计 §2.G4 一模一样，只是换个触发点：

```text
# 修复前：同一个 shape 方案、同一个输入 `ab`，两条装载路径给出不同候选
$ qingjian --schema shape --candidates=all ab            # 内嵌（内存词库）→ 4 条
  1. 木  2. 十  3. 才  4. ab
$ qingjian --scheme-dir schemes/qingjian-default --schema shape --candidates=all ab
  1. 十  2. ab                                        # 部署词库 → 2 条
```

`qingjian --check` 直接报了出来：`shape/ab 应当上屏「十」，得到「木」`。

### 修复后

- `TableLexicon::prefix_lookup`：前缀是**连续区间**，二分下界 + 顺序扫到
  不再匹配为止；与内存实现**逐字段一致**（文本 / 分数 / `attr=COMPLETION` /
  `kind=Completion`）。
- 新增 `crates/qingjian-schemes/tests/lexicon_capability.rs`：把两台实现的
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
$ cargo test -p qingjian-schemes --test instance_dictionaries --offline
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

`crates/qingjian-schemes/tests/schemes/p3phrases.dict.yaml` 与 `p3chaizi.dict.yaml`
的条目写成「编码在前、词在后」（`dz\t青简输入法`），与解析器期望的顺序相反。
**这从来没被校验过**，因为实例词库在装配时被丢掉了。现在它会真的装载，
于是装载期**响亮拒绝**。夹具已按正确顺序修好，并给 `p3features` 的字母表
补上 `d/z/y/x` 四个单元。

---

## 3. 阶段 3 第 3–4 项：配置字段审计表（审计 G5）

交付 `docs/config-field-audit.md` + 可执行的
`crates/qingjian-schemes/tests/config_field_audit.rs`（7 条行为断言）。

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
  （**输出事实**，不是代码），`cargo test -p qingjian-engine --test
  number_oracle --test calc_oracle` 仍然全绿。
- `schemes/qingjian-default/opencc.manifest.yaml` 里 emoji 的许可由
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

### 5.4 阶段 4 第 1 项：词级读音与词库质量评测（审计 §2.I）——**部分完成**

这一项由**两块**拼起来，各自的完成度不同，必须分开看：

| 块 | 做什么 | 状态 |
| --- | --- | --- |
| **单字读音（生成器）** | `tools/wordlist-gen` 取 `pinyin.txt` 的**首选读音**，落盘前自检「码 = 首选读音」 | ✅ **本轮修好**（§5.4.1 第 0 条） |
| **词级读音（人工覆盖表）** | 常见多音字词的正确读音，手工枚举 | ⚠️ **只覆盖枚举到的词**（§5.4.2 第 1–4 条） |

审计点名的四例**已修**，并且有可执行的评测集：

```text
$ cargo test -p qingjian-schemes --test word_pinyin_quality --offline -- --nocapture
词级读音质量（26 条）：召回 26/26（100%），首选 26/26（100%），前五 26/26（100%）
test result: ok. 4 passed
```

| 审计的点名 | 修复前 | 现在 |
| --- | --- | --- |
| `yinhang` → 银行 | 召回不到 | **首选** |
| `chongqing` → 重庆 | 召回不到 | **首选** |
| `yinyue` → 音乐 | 召回不到 | **首选** |
| `chongxin` → 重新 | 召回不到 | **首选** |

**做法**：新增**自撰的**人工覆盖表
`schemes/qingjian-default/cn_dicts/word_pinyin.override.dict.yaml`
（文件头写明来源、收录标准与许可；不是从任何第三方词表复制），
覆盖审计四例 + 高频多音字词 + 常见地名 + 姓氏人名 + 评测集暴露的常用词。
`pinyin.dict.yaml` 把它排在 `generated` **之前**（先出现者优先）。

**允许多个读音**（审计第 5 条）：生成器给的编码**保留不删**——
「银行」的 `yinxing` 是合法读音组合，多音字本来就有两读。
有一条测试专门守着"不许为了修 A 而删掉 B"。

**评测集**（`crates/qingjian-schemes/tests/word_pinyin_quality.rs`）：
26 条**手写**用例（独立于生成器的输入），分常用词 / 多音字 / 地名 /
人名 / 简拼；报告**召回率、首选率、前五命中**与逐条失败明细。
另有三条断言守着：覆盖表必须有来源、必须被导入、必须排在 `generated` 之前。

### 5.4.1 ✅ 本轮修掉的：生成器的单字编码错误

**`家 → jie`：根因在生成器，已修，验收测试已转正。**

旧版 `tools/wordlist-gen` 有一条"同声母微调"：主读音取 `pinyin.txt`
首个之后，再在**声母相同**的候选里挑"该音节在单字表里出现得更多"的
那个。它把**音节**的语料频率当成了**这个字**的读音证据——
`pinyin.txt` 里「家」是 `jiā,jia,jià,jie,gū`，`jie` 在单音字里出现
238 次、`jia` 只有 136 次，于是产物写成 `家 → jie`（权重 41023）。
实测代价：**838 个多音字**被改离首选读音；连锁后果是 `jie` 的首选
变成「家」、`nihaoshijie` 被拼成「你好**是家**」。

**修法（两处，缺一不可）**：

1. `tools/wordlist-gen`：删除那条启发式，改为 `primary_readings()`
   —— 只取 `pinyin.txt` 每行列表的首个；
2. 落盘前的**独立自检** `verify_primary_codes()`：拿 `pinyin.txt` 的
   首选读音重算每条词条的编码并逐字对照，不一致就中止、不写文件
   （放在 `--dry-run` 之前，所以干跑也会跑这条检查）。

为什么"能被装载器解析"挡不住它：`家 → jie` 那份产物能解析、能装载、
能打字，**只是打出来的字不对**。两条自检是两件事。

**回归**（验收 §5.1"不能只依赖最终大词库测试"）：

- 生成器单测 `a_more_common_syllable_does_not_hijack_a_characters_primary_reading`
  （用「家」的最小复现钉住"音节频率不许覆盖首选读音"）；
- 生成器单测 `the_consistency_check_catches_a_code_that_is_not_the_primary_reading`
  （自检能抓、且不误伤）；
- `word_pinyin_quality.rs::the_wrong_reading_of_a_generator_entry_does_not_hijack_a_correct_one`
  ——**原先 `#[ignore]`，现在转正**，并补了"`jia` 仍召回「家」"与
  "`nihaoshijie` 首选 = 你好世界"两条断言；
- 覆盖表里那条 `家 jia 41023` 兜底条目**已删除**（生成器自己对了，
  留着只会让人以为它还没修好）。

**重新生成与可复现**：先跑 `bash tools/fetch-sources.sh`（16 条全部
sha256 校验通过），再重新生成；词库正文 414,525 条与 YAML 头部
**逐字节可复现**，音节表 399 → **405** 个编码单元（`z-pinyin-demo`
内嵌孪生体同步更新）。命令与输出见 `THIRD_PARTY_NOTICES.md` §6.4。

### 5.4.2 ❌ 仍未做的部分

1. **没有引入可分发的"词级拼音"数据集**（审计 §2.I 第 2 条）。
   覆盖表是**人工枚举**的，因此它只修了表里那些词；
   表外的多音字词（`行`/`重`/`长`/`乐` 的其它组合）**只能按单字首选读音拼**。
   要把这一类整体修掉，需要一份覆盖全词表的词级读音数据 + 许可审查，
   并按审计要求记录许可证、版权、版本、hash、转换脚本与合并规则。
2. **生成器本身没有改成"词级读音优先"**（第 1 条的后半）。
   现在的顺序是"按单字首选读音拼 + 人工覆盖"，而不是"有词级读音就用它"。
3. **词库格式没有表达"一个词有多个读音与权重"**（第 5 条的下半）。
   现在的做法是"两条独立的词条"（覆盖表一条、生成表一条），
   效果等价，但格式上不是"一个词多读音"的结构。
4. **评测集是小样本**（26 条），不是统计意义的基准。
   它守的是审计点名的类别与四例，不声称覆盖整个 41 万词条。
   没有做**错误类型分类统计**（审计要求报告"错误类型"）——
   现在的报告只有召回/首选/前五三个比例。

### 5.5 其它已知未完成项

| 项 | 状态 |
| --- | --- |
| 阅读 `reference/wiki-*.md` 的许可 | **UNVERIFIED**：rime/home 无通用 LICENSE、wiki 页面无声明；已在 notices §5.3 标为待决（取得许可 / 改自撰摘要 / 移除） |
| `tools/librime-probe/probe.c` 文件头 | 缺 BSD-3-Clause 随附声明（notices §1.4 已指向 licenses/） |
| 已提交的 `generated.dict.yaml` 头部 | ~~把 jieba 标成 THUOCL~~ **已消除**：本轮重新生成后头部写的是 `jieba_dict.txt（jieba，MIT）` |
| `opencc.manifest.yaml` 的 emoji 许可 | ~~标成 Apache-2.0~~ **已消除**：现为 `GPL-3.0-only`（notices §5.2 已同步） |
| `scheme::entry` 的文档示例 | ~~` ```ignore ` 且不是合法 Rust~~ **已消除**：改成会编译会跑的例子，`--include-ignored` 下也过。**代价记一笔**：这次改动让 `docs/config-field-audit.md` 引用的 `scheme.rs` 行号整体 +8（已更正，并由 `config_field_audit.rs` 的守卫测试看住——见 `validation/README.md` 反馈 ②） |
| 字母表/规则孪生体测试的承诺 | ~~`sort()`+`dedup()` 只验集合、`rules` 只验非空~~ **已消除**：改为有序逐项比较 + 无重复项断言 + `rules` 正文比较 + 解析结果对照，四条都用破坏测试验证过（`validation/README.md` 反馈 ①） |
| git 历史 | 仍含被移除的 GPL Lua 旧版本（历史改写不在本次范围） |
| 会话分段 / 重开（阶段 2 的 F） | **部分完成**：余码保留与标点语义已交付并有测试；逐段确认、重开、任意 span 未做。见 `docs/validation/phase-2.md` §8 |
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

**未达成的是"完成"本身**：阶段 4 的**词级**读音工作（§5.4.2）与阶段 2 的
会话闭环（任务包 F）仍然是缺口，本页不把它们算作已交付。
本轮消除的是其中**具体、可验收**的一项——生成器的单字编码错误
（§5.4.1，含原先 `#[ignore]` 的验收点转正）。
