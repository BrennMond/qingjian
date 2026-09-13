# HANDOFF — Stele-IME 项目交接

> **这份文件是为"上下文耗尽"写的。** 它把 P0–P3 的全部决策、实测数字、
> 踩过的坑、以及下一步压缩成一页可读的东西。
> 新会话只要读 **这份文件 + `PLAN.md` + `docs/engine-design.md`**，
> 就能接着干，不必回溯对话。
>
> 最后更新：**P3 完成 + 阶段 A 的内联零件 10/14**（rime-ice 的 Lua 插件重写）。

---

## 0. 一分钟速览

**Stele-IME（石经）**：用 Rust 从原理重写的输入法引擎。
存在的理由：商业输入法占 300–400 MB 且隐私不完善；RIME 依赖重、关键行为寄生在 Lua 上，
而且**RIME 从未承诺过隐私与离线**（其官方文档 NOT FOUND，作者计划里写着"添加網絡功能"）。

**红线**：常驻内存 < 30 MB、部署峰值 < 150 MB、按键 P50 < 1 ms、P99 < 10 ms、
产物/源 < 3×。

**实测现状**（release）：

| 指标 | 实测 | 目标 |
| --- | --- | --- |
| 按键路径 P50（拼音，零件齐全） | **601 ns** | < 1 ms ✅ |
| 按键路径 P99 | **1.56 µs** | < 10 ms ✅ |
| 按键路径 P50（字形码，零件少） | **180 ns** | — |
| 常驻内存（演示词库） | **4 MiB** | < 30 MB ✅ |
| 50 万词条：装载峰值 | **46 MiB**（内存实现是 245 MiB） | — |
| 50 万词条：命中产物 | **23 MiB** | — |

**延迟数字必须注明方案**：`stele-bench --schema=<id>`。零件数差别很大，
180 ns 与 601 ns 是同一套代码。

**进度**：P0 ✅ P1 ✅ P2 ✅ P2.5 ✅ **P3 ✅** → **阶段 A 10/14**（见 §4）
→ 下一步 **P3.5 词库管线** / P4a

| | |
| --- | --- |
| 提交 | 19 个 |
| 测试 | **334 个**（clippy 零警告，三条 CI 门禁全过） |
| crate | 8 个 |
| Rust | 约 25700 行 |
| 零件 | 注册表 40 项：已实现 32、需数据 2、需资源 2、不适用 1、未实现 3 |

---

## 1. 仓库地图

```
stele/
├── PLAN.md                  ← 章程与决策记录（33 条 ADR）。**先读这个**
├── docs/engine-design.md    ← 引擎设计的权威定义（含术语表与"为什么"）
├── docs/HANDOFF.md          ← 本文件
├── reference/               ← 调研资料（RIME 官方文档对比、librime 内部机制…）
├── schemes/stele-default/   ← 默认方案（数据文件）
├── scripts/verify-*.sh      ← 三条 CI 门禁
├── tools/                   ← 验证工装（**不进内核 crate、不进 CI**）
│   ├── librime-probe/       ← 驱动真实 librime 的 C 探针（含抓取样本）
│   ├── compare-librime.py   ← 对照实验：同一批按键喂两边，比对结构行为
│   └── oracle/              ← 上游 Lua 的纯计算副本 + 它的输出存档
│       ├── number_translator/   （42 条对照）
│       └── calc_translator/     （74 条对照）
└── crates/
    ├── stele-core/          抽象层：Engine/Session、组件 trait、数据结构（**零依赖**）
    ├── stele-engine/        引擎：拼写代数（含自写正则）、注册表、翻译器、处理器（**零依赖**）
    ├── stele-config/        YAML 子集解析、$ref、分层补丁、可读诊断
    ├── stele-dict/          .dict.yaml（头部 + TSV + import_tables）
    ├── stele-table/         词库编译产物（紧凑二进制 + 按需分页）
    ├── stele-schemes/       方案装载 + 内嵌默认方案（**与内核分属不同 crate**）
    ├── stele-cli/           命令行调试前端（二进制名 `stele`）
    └── stele-bench/         称重台（延迟 / 内存 / 启动）
```

**为什么 `stele-schemes` 必须与内核分开**：内核被 CI 门禁禁止出现
"拼音/音节/简拼"这类词汇，而方案数据里必然出现。**物理分离是那条约束的执行方式。**

---

## 2. 必须记住的架构决定（改动代价 🔴）

| 决定 | 内容 | 为什么 |
| --- | --- | --- |
| **D12 两级对象** | `Engine`（共享，含方案目录）vs `Session`（私有，可切换方案） | 共享昂贵的词库；Rust 借此在编译期保证"一会话一线程" |
| **D13 定点对数分** | `Score(i32)`，单位毫对数；**`ln` 只在装载期算** | 浮点加法不满足结合律，跨平台（x86-64/ARM）会漂移；整数还省一半内存、无 NaN |
| **D20 通用性** | 内核只认识"编码/拼写/编码单元/字母表" | 仓颉的"音节"只是一个字母；五笔根本不需要切分图 |
| **D23 候选通道** | `Lane::Input`（严格：全序+精确优先）/ `Lane::Predict`（宽松、不参与盲选） | 让"精确优先"与"下一词预测"同时成立 |
| **D24 内核与方案分离** | 雾凇只是**行为参照**；默认方案是**自有资产** | 内核里不留任何具体输入法的痕迹 |
| **D28 产物按内容寻址** | 文件名含校验和；**不匹配即拒绝**，不凑合跑 | 用错版本会静默给错结果 |
| **D33 两族翻译器并跑** | 编码集合**可枚举**（拼音）走拼写图；**不可枚举**（仓颉/五笔）走精确编码 | 通用性从 P1 起被测试保护，而不是声明 |
| **D34 标签只准从 `TagTable` 拿** | `Tag` 是 `&'static str`，方案里的标签名在装载期 intern 一次 | 两处各自 `Box::leak("punct")` 会得到两块内存，`contains` 静默失效（真发生过） |
| **D35 配置值的单位写进字段名** | 边长用 `weight:`（线性比）或 `cost:`（毫对数），不共用一个字段 | `cost: -3000` 被当成"权重为负"→ 掉到下界 → 切分退化成随机（真发生过） |
| **D36 `send` 从链头重派发 + 重入标志** | 照抄 librime：`ProcessKey` 是顶层入口，防重入靠 `redirecting` 布尔 | 我第一版"从 key_binder 之后 + 轮数上限"能跑但语义不同（源码证伪） |
| **D37 候选的第三个轴 `CandidateKind`** | `Origin`（从哪来）/ `SpellingAttr`（编码怎么拼的）/ `kind`（**怎么被找出来的**） | `autocap` 要判"是不是补全"、`reduce_english` 要判"是不是用户词"——用 `Origin` 都表达不了。**判据是"有没有零件真的按它分支"**，不是"RIME 有这字段" |
| **D38 外部不确定性一律注入** | 时钟 `Clock`（含 `utc_offset_secs`）、随机 `RandomSource`、记忆 `MemoryStore` | 零依赖 + 可复现。**默认时区偏移是 0（按 UTC 报时）**——接前端时容易漏 |
| **D39 没有权威标准的行为拿上游当 oracle** | `tools/oracle/`：上游函数的纯计算副本 + `luajit` 跑出的输出存档 + 逐字节比对 | "与上游一致"这句话必须能被**重新跑一遍**，否则它只是又一句没有证据的话 |

**三条铁律**（PLAN §5）：① 内存与延迟是硬指标 ② 可复现 ③ 候选封闭 ④ 精确优先
⑤ 配置错误绝不阻止启动（输入法的失败是**自锁**的）⑥ 简体优先、繁体只留接口。

---

## 3. 工程约定（**新会话必须遵守**）

- **中文注释为主，公开 API 英文摘要**；章节标题用英文规范名（`# Errors` / `# Panics`），
  **因为 clippy 只认英文**。
- **不支持的语法一律报错并解释原因**，绝不"猜一个"。
- **诊断必须带行号**，并**一次报出全部问题**（不是遇到第一个就返回）。
- **内核不得出现输入法专属词汇** —— 由 `scripts/verify-no-ime-vocab.sh` 强制。
- **`stele-core` / `stele-engine` 不得有第三方依赖** —— 由 `verify-zero-deps.sh` 强制。
- **门禁必须反向验证过**（故意违规能被抓住）。**一个不会失败的检查等于没有检查。**
- **数字必须实测**。这一项目里已经有四次"实施者以为对、数字说不对"（见 §5）。
- **不确定的约定不许写成"RIME 约定"**。要么引 librime 源码，要么写"这是我们的选择"。
  我犯过一次：把"`prefix: uU` = 大小写二选一"写进文档注释并称之为 RIME 约定，而它是错的。
- **跨组件的"每轮都要同步"的状态，必须是一条显式的调用**（trait 上要有那个方法），
  不能让两个组件各自以为对方知道。`Segmentor::rescan` 就是这么补上的。
- **症状为"配置看起来正常、功能就是不生效"的 bug，只有端到端测试抓得到**。
  P3 抓到的四个全是这一类。
- **行为没有权威标准时，拿上游当 oracle**（D39）。做法：把上游的**纯计算部分**
  存进 `tools/oracle/`（剥掉它的运行时接口），用 `luajit` 跑出输出存档，
  再与我们的实现逐字节比对。**"与上游一致"这句话必须能被重新跑一遍。**
  写 `number_translator` 时它抓出 6 处、`calc_translator` 时 5 处——
  全都是"我以为理所当然"的错误。见 `tools/oracle/README.md`。
- **"比上游更宽松"也是一种不一致**。`--3`、`1+2)`、`sin(1,2)`（Lua 忽略
  多余实参）我第一版都比 Lua 宽松，而后果是"上游说这个输入错了"
  变成"我们算了个数"。移植时要把**两侧的边界都对齐**，不只是"能跑"。
- **一半的修正比不修更糟**：`send` 的语义我改了 `KeyBinder` 却忘了改 `pipeline`，
  于是那个 `redirecting` 字段永远是 `false`——两半对不上，而测试当时是绿的
  （因为旧的"从中间派发"实现也能让空格到达选择器）。
  **跨两处的语义改动，要有一条只在正确实现下才通过的测试。**

---

## 4. P0–P3 交付与剩余

### 已完成

| 阶段 | 交付 |
| --- | --- |
| **P0** | workspace、CI、三条门禁、称重台、许可证、README |
| **P1** | 拼写层、词库、两族翻译器、处理器、过滤器、流水线、会话；`nihao`→你好、`nh`→你好、`shape ab`→十 |
| **P2** | `stele-config`（YAML 子集 + `$ref` + 分层补丁）、`stele-dict`、目录装载、`--scheme-dir` |
| **P2.5** | `stele-table`：紧凑二进制 + 流式编译器 + 按需分页（500k 词条 245→46 MiB） |
| **P3** | **完整拼写代数**（含自写正则）、**零件集**（24 个名字里 22 个已实现）、**词条补全**、**分层 `--dump-config`**（每个值标来源）、**与 librime 的对照工装** |

### P3 的零件覆盖（`stele --components`）

`no_lua_schema` 引用的 24 个名字：

| 状态 | 个数 | 是哪些 |
| --- | --- | --- |
| **已实现** | 22 | `speller` `selector` `express_editor` `ascii_composer` `navigator` `punctuator` `key_binder` `recognizer` `ascii_segmentor` `matcher` `abc_segmentor` `affix_segmentor` `punct_segmentor` `fallback_segmentor` `script_translator` `table_translator` `punct_translator` `echo_translator` `reverse_lookup_filter` `simplifier` `uniquifier` `select_character` |
| **缺数据**（机制有） | 2 | `simplifier@emoji`、`simplifier@traditionalize`（要 OpenCC 的 `emoji.json` / `s2t.json`） |
| **缺代码** | **0** | — |

**"跑通 `no_lua_schema`"的准确状态**：零件与配置读法都齐了
（`crates/stele-schemes/tests/schemes/p3features.schema.yaml` 是一份
**RIME 原生写法**的等价方案，19+ 条端到端断言守着它），
但我们**没有真的把那份文件跑起来**——拿不到它的词库与 OpenCC 数据。

### 阶段 A：把 rime-ice 的 Lua 插件重写成原生零件（10/14）

**动机**：rime-ice 的默认方案里，品牌输入法"上手快"的那些功能
（日期、计算器、置顶、英文降权…）**全部住在 Lua 插件里**——
核心引擎没有这些能力。要接近那种体验，就得把它们做成原生零件。

| 已实现（10） | 行为 |
| --- | --- |
| `date_translator` | `rq`/`sj`/`xq`/`dt`/`ts`/`rqzh`/`rqen` |
| `unicode_translator` | `U62fc` → 「拼」+ 同区后续码位 |
| `uuid_translator` | 触发词 → UUID(v4) |
| `number_translator` | `R3355` → 四种中文形态（含金额大写） |
| `calc_translator` | `cC1+2` → 3（**自写表达式求值器**） |
| `long_word_filter` | 长词优先 |
| `autocap_filter` | `HEllo` → `HELLO` |
| `v_filter` | v 模式单字优先 |
| `pin_cand_filter` | 置顶 + **简码派生**（`ni hao` 也认 `nih`） |
| `reduce_english_filter` | 英文候选降权（`all`/`custom`/`none`） |

**代码位置**：`stele-engine/src/inline.rs`（8 个）+ `calc.rs`（求值器）。

**余下 4 个——不是"没做完"，是各自缺代码之外的东西**：

| 零件 | 缺什么 | 归在 |
| --- | --- | --- |
| `corrector` | 容错表在**上游的词库里**（数据资产） | `NeedsResource` |
| `lunar` | 1900–2100 的二进制表（上游单独发布） | `NeedsResource` |
| `select_character` | "候选能被当输入用"这条**会话语义** | 阶段 C |
| 拆字辅码 `search` | 同上 + 一个反查索引 | 阶段 C |

`stele --components` 分四档列出（已实现 / 需数据 / 需资源 / 不适用），
`CoverageReport::blocking_reason()` 会说清"缺在哪一步、该谁动手"。

### 与 librime 的对照（P3 的验收线）

```bash
cd tools/librime-probe && ./build.sh        # 一次性
python3 tools/compare-librime.py            # 6 条用例
```

报告在 `tools/librime-probe/samples/compare-report.md`，**6 条结构用例全过**。

**报告只断言结构**（能否上屏、按键是否被处理、标点是不是全角），
**不断言候选排序**——两边的词库与语言模型不同，比排序等于比词库。

它跑第一次就抓到一处真问题：默认拼音方案没声明 `punctuator`，
于是 `,` 什么都打不出来而 librime 出「，」。修法是给默认方案补上
RIME 形状的 `engine:` 清单，并给引擎加**预设**机制（`import_preset`）。

### 下一步

P3 的清单（1–6）**全部做完了**；阶段 A 的 10 个内联零件也做完了。
剩下的是（**按建议顺序**）：

1. **P3.5 词库管线**（最该做的那个）——`schemes/stele-default` 现在只有
   30 条演示词，而**其余一切的验收都卡在它上面**：没有真词库，
   "装上就能打字"是空的，librime 对照也只能比结构、比不了排序。
   两件事一起做：
   - **干净来源的词表**（THUOCL / pinyin-data / Unihan / rime-melt /
     rime-essay-simp——清单见 `reference/rime-ice-research.md` §6）；
   - **OpenCC 数据装载**（`simplifier` 的最后一块，做完"需数据"归零）。

   > 雾凇那 44 MB 词表**不进仓库**：授权状态混合，而最大的两块
   > （腾讯词向量、`base` 里那几项）来源不明或明确限制。
   > 用户部署时自取，我们只提供装载路径与校验。

2. **阶段 C：会话语义改造**（解锁 `select_character` + 拆字辅码）。
   需要让 `Session` 能表达"把第 N 个候选的第 M 个字放进输入串"——
   这是唯一需要动接口的事，风险最大、收益也最大。
3. **recognizer 的三处语义分叉**（记在
   `reference/rime-recognizer-and-affix.md` 的差异表）：
   我们锚死在位置 0、用"正则是否以 `$` 结尾"的启发式、
   取最长认领而非名字典序第一条。前两处会影响真实方案。
4. **`corrector` / `lunar`**：等有了数据来源再说——注册表里已经把
   它们标成"缺表不是缺代码"，不必再查一遍。
5. **`send_sequence` 的用例**与 **`select`**（切方案，需要
   `SchemaCatalog` 进处理器）——数据结构已就位。
6. **P4a 用户记忆**（`MemoryStore`，`Event::ForgetRequested` 已经发出来了）。

## 5. 踩过的坑（**每一条都是"写代码/量数字"才发现的**）

| # | 坑 | 教训 |
| --- | --- | --- |
| 1 | **`Score::saturating_add` 只防 `i32` 溢出，没钳在值域内** —— 两个 `CEIL` 相加得 `42948` | 值域边界要和类型边界一起守 |
| 2 | **"重排器加成有上界 ⇒ 倒置不可能"是错的** —— 上界只限幅度，不消倒置 | 文档里的论断要能被测试推翻 |
| 3 | **`f32` 无法 `Eq`，而记忆衰减值本就不该用浮点** | 影响排序的量一律定点 |
| 4 | **门禁第一次运行就抓住了我自己的架构违规**（方案数据被放进了内核 crate） | 门禁的价值在第一次真正运行时兑现 |
| 5 | **`Rule::Identity` 是设计错误** —— 规范拼写是**基线**，不是规则 | 把"基线"误当"规则"会让最普通的输入都查不到 |
| 6 | **装载器的响亮报错抓住我自己的数据错误**（词库用了 `hua` 但字母表没有） | 静默跳过会变成"有的词永远打不出来" |
| 7 | **`--scheme-dir` 的值被当成按键打了出来** —— 单元测试全绿 | **"能编译+测试绿"不等于"能用"** |
| 8 | **P2.5 接线 bug：部署路径仍走了一遍内联装载再丢掉重编** —— 内存没降 | 换实现要确认旧路径真的没被走到；**只有数字会告诉你** |
| 9 | **`code_count` 永远多 1**（前缀和多推哨兵） | |
| 10 | **`Rule::equivalence` 用字符类+字面替换做不到逐字符映射**（`zhang`→`zzhangh`） | |
| 11 | **我把"编码"与"拼写"的关系搞反了** —— 字典键是**字母表条目**，运算子只改"能敲什么" | 这正是 RIME 的「拼寫 ≠ 編碼」 |
| 12 | **流式序列解析器按字节切片，中文被切坏**（`--dump-config` 打出乱码） | 处理文本一律按 `char`，不按 `u8` |
| 13 | **流水线只产出"整串一段"，按音节退格会一次清空** | 分段要真的按音节切 |
| 14 | **我自己的测试名里用了 "syllable"，被 D20 门禁抓住** | 门禁第二次抓住我了 |
| 15 | **流水线每轮重算识别结果却没告诉切分器** —— 整条"识别→切分→绑定"链静默断掉 | 跨组件的"每轮同步"必须是一条**显式调用**（`Segmentor::rescan`） |
| 16 | **同一个标签名有两处来源**，一处没走 intern 表 —— `contains` 失效，标点整条链断 | `Tag` 只准从 `TagTable` 拿（D34） |
| 17 | **`cost` 一个字段两种单位**：写 `-3000` 被当成"权重为负"→ 掉到下界 → 切分退化成"谁先找到算谁" | 单位写进字段名（D35） |
| 18 | **`Query::composition` 让流水线每键克隆三份会话状态**，而全项目零个使用者 —— P50 301ns→1.55µs | 加字段前先问"谁读它"，实测会告诉你 |
| 19 | **我把猜出来的约定写进文档并称之为"RIME 约定"**（`prefix: uU` / "RIME 也做 leading 缓存"） | 不确定就写"这是我们的选择"，或引源码；猜的约定写进文档比写进代码更危险 |
| 20 | **语义改了 `KeyBinder` 却忘了改 `pipeline`** —— `redirecting` 永远是 false，而测试当时是绿的 | 跨两处的语义改动，要有一条**只在正确实现下**才过的测试 |
| 21 | **目录装载从来没应用过用户补丁** —— 接线写在 `load_scheme_layered` 里，而 CLI 走的是目录那条路；测试也只覆盖了前者 | "机制存在"与"机制被走到"是两件事。**手工跑一遍并对照两份输出**才发现的（P2.5 的接线 bug 是同一形状，这是第二次） |
| 22 | **`char::is_alphanumeric()` 对汉字返回 `true`** —— 长词滤镜把每个中文候选都当成英文候选，一个都不提升（RIME 那边是 Lua 的 `[%a%d]`，**只认 ASCII**） | **跨语言移植时"看着等价的谓词"最危险**：只承认两边行为一致的那部分（ASCII），不要相信名字相同 |
| 23 | **Lua 的 `gsub(p, r)` 默认只替换第一处** —— 我按"全局替换"实现了它，因为那是这个名字给我的印象；上游连写两遍同一个 `gsub` 恰好是在**依赖**这个性质（`R0001` 应为「〇一」） | **名字给的印象不能代替读语义**。对照测试把它从「一」纠正回「〇一」 |
| 24 | **我比 Lua 更宽松**：`--3`（Lua 里 `--` 是注释）、`1+2)`、`sin(1,2)`（Lua 忽略多余实参） | **"更宽松"也是一种不一致**——它会把"上游说这个输入错了"变成"我们算了个数" |
| 25 | **我给对照数据放进了内核 crate**（`crates/stele-engine/tests/oracle/`） | `verify-no-scheme-data.sh` 当场拦下：**内核不许有数据文件**（D24）。"只是测试用"不是理由——门禁第六次抓到我 |

---

## 6. 现在怎么跑

```bash
cargo build --workspace && cargo test --workspace      # 334 个测试
cargo run -p stele-cli -- --check                      # 7 组内核不变式
cargo run -p stele-cli -- nihao                        # → 你好
cargo run -p stele-cli -- nh                           # → 你好（简拼）
cargo run -p stele-cli -- --schema shape ab            # → 十（同一个引擎）
cargo run -p stele-cli -- --dump-config                # 合并后的方案（标来源）
cargo run -p stele-cli -- --components                 # 零件注册表
cargo run -p stele-bench --release -- --schema=shape   # 换方案称重
python3 tools/compare-librime.py                       # 与 librime 对照（结构）
cd tools/librime-probe && ./build.sh && ./probe --help  # 驱动真实 librime
cargo test -p stele-engine --test number_oracle         # 与上游 Lua 逐字节对照
cargo test -p stele-engine --test calc_oracle           #   （42 + 74 条）
# 重新生成对照数据（需要 luajit；见 tools/oracle/README.md）：
luajit tools/oracle/calc_translator/calc.lua > tools/oracle/calc_translator/calc.expected.txt
cargo run -p stele-cli -- --scheme-dir <目录> --list    # 装载自建方案
cargo run -p stele-bench --release -- --iterations=200000
bash scripts/verify-*.sh                               # 三条门禁
```

**环境事实**：WSL2，仓库在 ext4（`/home/brennmond/projects/stele`），
rustup 已装、toolchain 1.98 由 `rust-toolchain.toml` 固定。
`librime-bin 1.16.1` 已装（`rime_deployer` 可用）。

---

## 7. 下一步建议

**优先做 P3.5（默认方案）**，因为其余一切的验收都卡在它上面：

```
P3.5  schemes/stele-default 是自有资产（现在只有 30 条演示词）
      ├─ 装上就能打字 —— "体验对标雾凇"这句话才有内容
      ├─ 有了真词库，对照实验才能比**排序**（现在只能比结构）
      └─ OpenCC 数据装载顺带做完（simplifier 的最后一块）
```

**然后**：

1. **recognizer 的三处语义分叉**——记在
   `reference/rime-recognizer-and-affix.md` 的差异表里。前两处
   （锚死位置 0、`$` 启发式）会影响真实方案，值得对齐。
2. **P4a 用户记忆**：`MemoryStore` 接口早就定好了（`stele-core::service`），
   `Event::ForgetRequested` 也已经发出来。红线是**按键时零磁盘 I/O**。
3. **`select`（切方案）**：需要把 `SchemaCatalog` 送到处理器手里，
   属会话语义；`key_binder` 的其余动作都齐了。
4. 再往后是 P4b（本地下一词预测）、P5（向量重排，需过内存评审）、
   P6/P7（Windows TSF / Android）。

**一件不该忘的事**：`reference/` 下有两份**以 librime 源码为准**的调研
（`rime-key-binding-actions.md` 1084 行、`rime-recognizer-and-affix.md` 573 行），
每份文末都有「与 stele 实现的差异」表。**动手改这些零件之前先读它们**——
P3 里有四个 bug 是"我猜了一个约定"造成的，而它们全都写在里面。

---

## 7.5 许可证边界（**动手前先看这条**）

项目所有者已定（2026-09）：

| | |
| --- | --- |
| **Stele 本体** | MIT / Apache-2.0（**不变**） |
| **插件代码** | **按行为重写**，不复制上游 Lua（阶段 A 的做法） |
| **雾凇词表** | **不进仓库**，用户部署时自取；我们只提供装载路径与校验 |

**一处值得讲清的误解**：把 GPL 插件与 MIT 内核**一起打包分发**，
那一整份分发就落入 GPL——GPL 不允许"把 GPL 部件放进宽松作品、
只给部件标 GPL"。所以"插件用 GPL、内核仍 MIT"在**同一个安装包里**
不成立；成立的做法只有"那个部分是独立项目、独立分发"。

**而且词表的问题比 GPL 更麻烦**：那 44 MB 里最大的两块是
**来源不明或明确限制**的（`tencent` 16.9 MB 无许可声明、
`base` 16.2 MB 是几种来源的混合、`google-10000-english` 作者
自己写"不建议商用"）。"来源不明"比 GPL 难处理——GPL 至少有规则可循。

`tools/oracle/*.lua` 是上游函数的副本，**只用于测试对照、不参与构建**；
想彻底避开 GPL 就删掉它们，保留 `.expected.txt`（那是输出事实，不是代码）。

---

## 8. 给新会话的操作提醒

- 项目所有者**是初学者**，要求：新名词第一次出现就解释；不要高估基础；
  文档别写成座右铭（要能证伪、能写成断言或测试）。
- 他**明确表示过**：对外只维护**简体**形态；繁体留接口不维护（D32）。
- 他**授权过**：装 rustup。**没有授权**其它工作区外的操作
  （sudo 需密码，不能代劳）。
- **每次动手前后都要实测**：这个项目里"以为对"的记录见 §5（现在有 20 条）。
- **门禁与对照工装都在**：改动内核后跑 `bash scripts/verify-*.sh`；
  改动零件行为后跑 `python3 tools/compare-librime.py`；
  改动那 10 个内联零件后跑 `cargo test -p stele-engine --test number_oracle
  --test calc_oracle`（与上游逐字节对照）。
- **内核 crate 不许有数据文件**——包括"只是测试用"的对照数据
  （门禁抓到过我一次，已移到 `tools/oracle/`）。
- **改行为之前先读 `reference/` 里那两份以源码为准的调研**，以及
  `tools/oracle/README.md`——P3 到阶段 A 的 11 个 bug 全都写在里面。
- **不要相信"配置看起来正常"**：P3 抓到的四个 bug 症状完全一样——
  配置合法、没有报错、某个功能就是不生效。只有端到端测试抓得到。
- **不确定的约定，别写成"RIME 约定"**：要么引 librime 源码
  （`reference/` 里有两份现成的），要么写"这是我们的选择"。
