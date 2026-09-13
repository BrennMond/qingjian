# HANDOFF — Stele-IME 项目交接

> **这份文件是为"上下文耗尽"写的。** 它把 P0–P3 的全部决策、实测数字、
> 踩过的坑、以及下一步压缩成一页可读的东西。
> 新会话只要读 **这份文件 + `PLAN.md` + `docs/engine-design.md`**，
> 就能接着干，不必回溯对话。
>
> 最后更新：**P3.5 完成**——默认词库（41 万条）+ OpenCC 数据装载；
> 顺带修掉四个"静默失效"的引擎/格式 bug（见 §5 的第 26–29 条）。

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
| 按键路径 P50（**真实词库 41 万条**，拼音） | **约 50 µs**（冷页缓存）／**约 15 µs**（热） | < 1 ms ✅ |
| 按键路径 P99（同上） | **约 110 µs** | < 10 ms ✅ |
| 常驻内存（真实词库） | **13.6 MiB** | < 30 MB ✅ |
| 引擎装载（真实词库，产物命中） | **83 ms** | — |
| 首次部署（编译 41 万条产物 + 装载） | **0.62 s**（峰值 **21 MiB**） | < 150 MB ✅ |
| 词库产物 / 源 `.dict.yaml` | 14 MB / 11 MB ≈ **1.3×** | < 3× ✅ |
| 按键路径 P50（字形码演示方案） | **180 ns** | — |

**两套路经**：`stele` 在仓库根目录跑会**自动用 `schemes/stele-default`**
（41 万条，部署路径）；换到别的目录跑则用**内嵌演示词库**（几十条）
兜底——**生成词库不进二进制**（它 11 MB，见 `pinyin.embedded.schema.yaml`）。

> **约 50 µs 与约 15 µs 是同一份产物的两次测量**，差别在**操作系统的
> 页缓存**：词条页留在页缓存里（这正是 `stele-table` 的设计目标——
> 文件大小 ≠ 常驻内存），冷的时候每次查询要走两次 `read_at`。
> 两者都远在 1 ms 红线之内。

**延迟数字必须注明方案与词库**：`stele-bench --schema=<id>`、
量真实词库还要 `--scheme-dir`。演示词库（几十条）与真实词库（41 万条）
是同一套代码，差 80 倍；零件数也不同。

**进度**：P0 ✅ P1 ✅ P2 ✅ P2.5 ✅ P3 ✅ **P3.5 ✅** → 下一步 **P4a 用户记忆**
（或 `recognizer` 的三处语义分叉）

| | |
| --- | --- |
| 测试 | **360 个**（clippy 零警告，三条 CI 门禁全过，`cargo fmt --check` 干净） |
| crate | 8 个 |
| Rust | 约 26500 行 |
| 词条 | **414525 条**（414111 词 + 414 补录单字），399 个编码单元 |
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
│   ├── pinyin.schema.yaml   ← 方案；`speller.alphabet` 由生成器维护
│   ├── pinyin.dict.yaml     ← 主词典（导入清单）
│   ├── z-pinyin-demo.schema.yaml ← 内嵌演示版（`z-` 前缀让它排最后；
│   │                              词库指向 base，避免 11 MB 进二进制）
│   ├── cn_dicts/generated.dict.yaml ← **41 万条**默认词库（生成产物，随仓库分发）
│   ├── opencc.manifest.yaml ← OpenCC 数据清单（声明，不含数据）
│   ├── opencc.patch.yaml    ← 可选叠加层：启用 emoji / 简繁转换
│   └── build/               ← 取回的上游源数据（**不进仓库**）
├── scripts/verify-*.sh      ← 三条 CI 门禁
├── tools/                   ← 验证工装（**不进内核 crate、不进 CI**）
│   ├── fetch-sources.sh     ← 取回干净来源的数据（带 sha256 校验）
│   ├── wordlist-gen/        ← 源数据 → `.dict.yaml`（自写，含自检）
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

**铁律**（完整清单见 PLAN §5，共 12 条；日常最相关的是这几条）：
① 内存与延迟是硬指标 ② 可复现 ③ 候选封闭 ④ 精确优先
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
- **"数据装进来了"与"功能生效了"是两件事**。P3.5 里连着踩了三次同一个形状：
  `--dump-config` 说"已装载 6355 条转换"、而候选里一个 emoji 都没有。
  三次的原因各不相同（裁剪在排序之前 / 表的两种语义 / 展开的名额分配），
  但**症状完全一样**。所以：**配置类功能的验收必须是端到端断言**，
  不能只看"装配报告"。
- **"上限"是静默错误的温床**：`max_expansions`、`candidate_cap`、分页裁剪
  都会**安静地丢东西**。丢的必须是"按正确顺序排在最后"的那些，
  否则丢的是用户真正想要的那条。P3.5 的第 26、28 条坑都是这一条的具体形态。
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

### P3.5 已完成（默认词库 + OpenCC 数据装载）

**验收标准**（`PLAN.md` §3 的 P3.5 行）：**装上就能打字，体验对标雾凇；
内核里没有任何它的痕迹。**

| 交付 | 内容 | 实测 |
| --- | --- | --- |
| **干净来源的词表** | `tools/fetch-sources.sh` 取回 **9 份**上游数据（全部 MIT / Apache-2.0），带 sha256 清单校验 | 取回 3.9 MB 源数据 |
| **词库生成器** | `tools/wordlist-gen/`（自写，零外部依赖；用真正的装载器**自检产物**） | 生成 **414525 条**，产物 11 MB |
| **默认词库** | `schemes/stele-default/cn_dicts/generated.dict.yaml`：jieba 通用词表 + 8 份 THUOCL 分领域词表；简繁过滤（用 OpenCC 的 `TSCharacters` 滤掉繁体条目）。主词典 `pinyin.dict.yaml` 仍是**导入清单** | 音节表 **399 个**编码单元，由生成器反推并同步写回方案 |
| **OpenCC 数据装载** | `stele-dict/src/opencc.rs`（含一个 300 行的极简 JSON 解析器）+ `opencc.manifest.yaml`（数据清单） | emoji 表 6355 条、简繁表 53250 条，**真的装进引擎** |
| **`simplifier` 真的生效** | `stele --option=emoji weixiao` → 候选里有 `😊`；`--option=traditionalization zhongguo` → `中國` | 两条都有端到端断言 |

**为什么源数据不进仓库**：那 9 份里有 8 份是 THUOCL / jieba / OpenCC，
**都是可分发的**（MIT / Apache-2.0）——但我们仍然把它们放在 `build/`
（`.gitignore`）里，因为「数据的来源与许可是使用者要能自己核对的东西」。
**生成的 `.dict.yaml` 进仓库**：它是 MIT 数据的产物，且"克隆下来就能打字"
需要它。两者相加仍然只有 13 MB。

**多音字：说清假设**。`pinyin.txt` 给每个字一个有序读音表（按字典习惯），
而**词级拼音不在任何一份可分发数据里**。生成器的策略是：主读音取首个，
只在**同声母**的候选里按"单字表里出现更多的读音"微调。
**它不会把「银行」读成 `yín háng`**（那是另一份数据的事），
这一条写在词库头部与生成器的文档里，不藏在代码里。

### 与 librime 的对照（P3 的验收线）

```bash
cd tools/librime-probe && ./build.sh        # 一次性
python3 tools/compare-librime.py            # 6 条用例
```
报告在 `tools/librime-probe/samples/compare-report.md`，**6 条结构用例全过**。
**报告只断言结构**（能否上屏、按键是否被处理、标点是不是全角），
**不断言候选排序**——两边的词库与语言模型不同，比排序等于比词库。

### 下一步

按建议顺序：

1. **P4a 用户记忆**（`MemoryStore` 接口与 `Event::ForgetRequested` 早就位）。
   红线是**按键时零磁盘 I/O**。**注意 G10**：落库前必须把编码规范化，
   否则会得到"永远检索不到的无效数据"，症状是"学过的词有时出现有时不出现"。
2. **`recognizer` 的三处语义分叉**（记在
   `reference/rime-recognizer-and-affix.md` 的差异表）：我们锚死在位置 0、
   用"正则是否以 `$` 结尾"的启发式、取最长认领而非名字典序第一条。
   前两处会影响真实方案。
3. **阶段 C：会话语义改造**（解锁 `select_character` + 拆字辅码）。
4. **`corrector` / `lunar`**：等数据来源（注册表已标成"缺表不是缺代码"）。
5. **`select`（切方案）** 与 **`send_sequence` 的用例**——数据结构已就位。
6. **简拼的边界**：`nhao` → 你好，而 `nh` 打不出「你好」（见 §5 第 28 条与
   `pinyin.schema.yaml` 里那段注释）。要支持 `nh` 需要在展开里保住
   `[ni][hao]` 这条**完整**切分，属于拼写代数的下一步。

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
| 26 | **流水线把候选裁成"当前页"是在排序之前** —— 而 `simplifier`（emoji / 简繁）产出的候选**追加在末尾**，于是**永远被裁掉**。症状：`--dump-config` 说"已装载 6355 条转换"、候选里一个 emoji 都没有 | **裁剪/限流必须在排序之后**；"前 N 个"只在"已排序"时才是"前 N 名"。引擎给全量、翻页是前端的事 |
| 27 | **OpenCC 的表有两种逐字节相同的形状**：`干<TAB>乾 幹`（多选一）与 `微笑<TAB>微笑 😊`（复合串）。我按"按空白切分+取第一个"实现，于是 emoji 表 4857 条一条都不生效 | 判据是**值是否以键自身开头**，而它只能在"同时握着键与值"的地方做——解析层整段保留，消费者才判 |
| 28 | **拼写展开用的是深搜 + 硬上限**：`ni hao` 的规范切分**没被生成**，因为名额被 `niu hao` 这类缩写变体占满了。症状是"你好在 41 万词条的词库里打不出来"，而单字 `ni`/`hao` 都正常 | 展开必须**按代价排序**（best-first），而不是"先到先得"；上限截断的是**最差**的那些才安全 |
| 29 | **YAML 子集解析器把裸 `nan` 当浮点 NaN**（Rust 的 `f64::from_str` 认识它）——于是音节表里的 `- nan` 变成 `Float(NaN)`，`as_str()` 返回 `None`，那一项被静默丢掉。装载器随后报"词条引用了字母表里没有的编码单元「nan」"，而**文件里明明写着它** | 语言的"特殊值"写法要按**规范**收（`.nan` / `.inf`）；歧义写法一律当字符串。**查了半天不在装载器上，在解析器的一行** |

---

## 6. 现在怎么跑

```bash
cargo build --workspace && cargo test --workspace      # 360 个测试
cargo run -p stele-cli -- --check                      # 7 组内核不变式
cargo run -p stele-cli -- nihao                        # → 你好（内嵌演示词库）
cargo run -p stele-cli -- --schema shape ab            # → 十（同一个引擎）
cargo run -p stele-cli -- --dump-config                # 合并后的方案（标来源）
cargo run -p stele-cli -- --components                 # 零件注册表

# ── 真实词库（41 万条）── 在仓库根目录跑时会**自动发现** schemes/stele-default
cargo run -p stele-cli --release -- nihao
cargo run -p stele-cli --release -- --scheme-dir schemes/stele-default nihao
cargo run -p stele-cli --release -- --scheme-dir schemes/stele-default nhao
cargo run -p stele-cli --release -- --scheme-dir schemes/stele-default \
    --candidates=all weixiao             # `--candidates=N|all` 看全量（默认只看当前页）

# ── 重新生成默认词库（一次网络访问；源数据落在 .gitignore 的 build/）──
bash tools/fetch-sources.sh              # 取回 9 份干净来源 + sha256 校验
cargo run --release --manifest-path tools/wordlist-gen/Cargo.toml -- \
    --sources schemes/stele-default/build --out schemes/stele-default

# ── OpenCC 转换（emoji / 简繁）：可选叠加层 ─────────────────────────
cp schemes/stele-default/opencc.patch.yaml schemes/stele-default/pinyin.custom.yaml
cargo run -p stele-cli --release -- --scheme-dir schemes/stele-default \
    --option=emoji --candidates=all weixiao      # 候选里有 😊
cargo run -p stele-cli --release -- --scheme-dir schemes/stele-default \
    --option=traditionalization zhongguo          # 候选里有 中國

# ── 称重（真实词库）与门禁 ─────────────────────────────────────────
cargo run -p stele-bench --release -- --scheme-dir schemes/stele-default --schema=pinyin
cargo run -p stele-bench --release -- --schema=shape   # 演示方案（零件少）
bash scripts/verify-*.sh                               # 三条门禁

# ── 与 librime / 上游 Lua 对照 ─────────────────────────────────────
python3 tools/compare-librime.py                       # 结构对照（6 条）
cd tools/librime-probe && ./build.sh && ./probe --help  # 驱动真实 librime
cargo test -p stele-engine --test number_oracle         # 与上游 Lua 逐字节对照
cargo test -p stele-engine --test calc_oracle           #   （42 + 74 条）
# 重新生成对照数据（需要 luajit；见 tools/oracle/README.md）：
luajit tools/oracle/calc_translator/calc.lua > tools/oracle/calc_translator/calc.expected.txt
cargo run -p stele-cli -- --scheme-dir <目录> --list    # 装载自建方案
```

**环境事实**：WSL2，仓库在 ext4（`/home/brennmond/projects/stele`），
rustup 已装、toolchain 1.98 由 `rust-toolchain.toml` 固定。
`librime-bin 1.16.1` 已装（`rime_deployer` 可用）。

---

## 7. 下一步建议

**P3.5 已完成**（见 §4）——默认词库 41 万条、OpenCC 数据装载、
简繁/emoji 两条端到端断言。

**下一步已定稿为 P4a（用户记忆）**，执行书在 **§7.6**——
它把"动手前必须先定的四件事"（量纲 / G10 策略 / 内存上限 / 依赖选型）
写清楚了，**先读它再动手**。

其余候选（**不按此顺序自动开始**，除非所有者指定）：

1. **recognizer 的三处语义分叉**——记在
   `reference/rime-recognizer-and-affix.md` 的差异表里。前两处
   （锚死位置 0、`$` 启发式）会影响真实方案，值得对齐。
2. **阶段 C：会话语义改造**（解锁 `select_character` + 拆字辅码）。
3. **`select`（切方案）**：需要把 `SchemaCatalog` 送到处理器手里。
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

## 7.6 下一步执行计划：**P4a 用户记忆**（已定稿，照此执行）

> 这一节是**给下一个会话的执行书**：它把路线图上的"P4a"拆到可以直接动手的粒度，
> 并把"动手前必须先定的四件事"写清楚。
> **先读完本节，再读 `docs/engine-design.md` §4 与 `reference/dsh-plugin-architecture-lessons.md`。**
>
> **一动代码之前先做一件事**：跑一遍基线并记下来——
> `cargo test --workspace`、`bash scripts/verify-*.sh`、
> `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --check`。
> 当前基线：**360 个测试**全过、clippy 零警告、三条门禁全过。

### 7.6.0 目标与验收（PLAN §3 的 P4a 行）

| | |
| --- | --- |
| **目标** | 频率 + 时间衰减的用户记忆 |
| **交付物** | 新 crate `stele-memory`（`MemoryStore` 的实现）+ 引擎侧接线 + CLI 开关 |
| **验收 1** | **打过的词下次优先**：同一个词连续上屏过 N 次后，它排到同码候选之前 |
| **验收 2** | **按键时零磁盘 I/O**（红线）：按键路径上一次 `read`/`write` 系统调用都不能有 |
| **验收 3** | 进程重启后学到的词还在（持久化可用） |
| **验收 4** | 内存增量可量化，且不超 §0.2 预算（见 7.6.1 ③） |
| **验收 5** | 记忆文件坏了 = **降级成"没有记忆"并打印警告**，绝不阻止启动（D26） |

**"零磁盘 I/O"怎么证伪**（不许只写在文档里）：写一条测试，在按键前后读
`/proc/self/io` 的 `read_bytes` / `write_bytes`，断言按键路径**没有增长**；
落盘只能由显式的 `flush()` 或后台批量触发。
（`/proc/self/io` 是 **Linux 专属**；跨平台版本可以先 `cfg(target_os = "linux")`
只在该平台上断言，其它平台跳过——但**不许因为"不好写"就不写**。）

### 7.6.1 动手前必须定掉的四件事（**不先定，事后就是全量返工**）

#### ① 记忆加成的**量纲**（PLAN §6 那条待决问题）

RIME 的 `initial_quality: 1.2` 是加到**线性计数**上的
（`exp(weight) + initial_quality + …`），而我们是**定点对数域**（D13）。
记忆加成必须落在**对数域**，理由与 D13 相同：候选排序要可复现，
浮点在不同平台会漂移。

**建议定案**：加成 = `Score::from_weight(1 + k × f)` 的**定点值**，在
**更新时**算好、存进 `MemoryEntry::bonus`。`stele-core::service` 已经这么
定义了这个字段——它写明"换算由实现方在**更新时**完成，那一步可以用浮点；
但**存下来的必须是定点**"。`k` 与 `f` 的取值要**先写成一个能证伪的测试**：
给定次数与时间，断言 `bonus` 的整数值。

#### ② G10：**派生拼写到底存不存**（影响面最大，需要所有者点头）

**一句话背景**：词库的键永远是**规范编码**（`ni hao`），而 `nhao` 只是
**到达它的一条路径**——投影是单向的。所以拿 `nhao` 当主键存进去，
那条记录将**永远检索不到**，症状是"学过的词有时出现有时不出现"。

两条路：

| 方案 | 做法 | 代价 |
| --- | --- | --- |
| **A. 反查规范化** | 给拼写层加"`Expansion` → 规范拼写"的反查 | `Spelling` trait 要加方法；反查**可能不唯一**（`nh` 既可能 `ni hao` 也可能 `na hao`），得再查词库确认 |
| **B. 派生候选不学** | `if commit.attr.is_derived() { return; }` | 简拼命中不上学习榜；但**永远不会写出无效数据** |

`docs/engine-design.md` 与 `stele-core::MemoryStore::record` 的文档都写了
"**换算不出来就宁可不存**"。**建议先做 B**（十行、无新接口、可证伪），
把 A 留到"下一词预测"落地时——那时本来就需要更完整的拼写层。

> **需要所有者确认**：接受"简拼命中不参与学习"这个产品行为吗？
> 即：`nhao → 你好` 可以上屏，但不因此给「你好」加分。
> （**建议接受**：简拼是"猜的"，用它去强化记忆会放大误判。）

#### ③ 内存上限与淘汰（红线相关）

用户记忆是**唯一会无界增长**的数据。当前基线：真实词库常驻 **13.6 MiB**，
红线 **30 MB** ⇒ 留给记忆约 **15 MB**。

**建议定案**（写成常量 + 注释 + 测试）：
- 条目上限（建议 `100_000`，按实测单条占用反推）；
- 超限时淘汰 `bonus` 最低的条目，**淘汰规则必须确定性**（不许按容器遍历顺序）；
- 上限可配置，但**默认值要出现在称重台的报告里**。

#### ④ 依赖：SQLite 还是自写格式（**这条可能改变工作量，先定**）

PLAN §2.2 写的是 `stele-memory`（SQLite + 内存缓存）。但**现在有两个新事实**：

1. **CI 里没有 `cargo-deny`**——PLAN §4.8 要求"依赖许可审查必须要有机制，
   否则 D9 只是一句自我声明"，而这条**至今没做**。引入第一个第三方依赖
   之前应当先补上它（那本身就是 P0 的欠账）。
2. 我们的既有手艺是**自写的紧凑二进制 + 按需读取**（`stele-table`，零依赖、无 unsafe）。

| 方案 | 做法 | 影响 |
| --- | --- | --- |
| **B1（推荐）** | **自写排序 KV 文件**：复用 `stele-table` 的思路（索引常驻 + 追加写 + 启动时全量读进内存） | 零依赖、无 unsafe、红线最容易守住；**多端复用同一份格式** |
| **B2** | SQLite（`rusqlite`） | 少写代码、有事务；但要先给 CI 补 `cargo-deny`，并审 SQLite 的许可与体积 |

> **为什么推荐 B1**：我们真正需要的只是一个"按键时零 I/O、启动时读一次"的
> **有序表**——那正是 `stele-table` 已经解决过的问题。SQLite 的价值在并发事务
> 与复杂查询，而用户词库两者都不需要。
>
> **这一条请所有者拍板**（它会显著改变第 2 步的工作量）。

### 7.6.2 执行步骤（建议按此顺序提交，每步单独可验证）

**第 0 步：补依赖门禁**（选 B2 则必做，选 B1 也建议做）
PLAN §4.8 的欠账：给 CI 加依赖许可审查（`cargo-deny` 或等价脚本）。
**门禁必须先能抓住一次故意违规**，否则它等于没有。

**第 1 步：量纲与衰减的纯函数**（不碰引擎）
先写"次数 + 时间 → 定点 `bonus`"的纯函数，配一张**边界表**测试
（0 次 / 1 次 / 1000 次 / 跨年 / 时钟回拨 / `last_used == 0`）。
时钟一律**注入**（D38），不读 `SystemTime`。

**第 2 步：`MemoryStore` 的实现 + 内存缓存**
`record` / `lookup` / `forget` / `predict_next`（后者先返回空，P4b 再填）。
落盘走"追加 + 定期合并"；**读盘只在启动时一次**。
测试：`record` 后 `lookup` 立刻能查到；关掉再打开仍在。

**第 3 步：引擎接线（**唯一需要改接口的地方**）**
- `EngineImpl` 现在**只有 `schemes` 与 `infos`**（`EngineInner`），没有任何服务。
  按 `docs/engine-design.md` §4「组件如何拿到服务」：**服务在装配时注入，
  不穿过 `Query`**。所以给 `EngineImpl` 加一个服务集合，由**装配处**
  （`stele-cli` / `stele-bench` / 测试）构造后传入。
- 新增一个 `Ranker`（放在 `stele-memory`，**不放内核**）：持有
  `Arc<dyn MemoryStore>`，按 `bonus_limit()` 给候选加分。
  **注意 `bonus_limit` 的语义**：上界**不保证**不产生跨类倒置——
  所以 `pipeline` 里那条 `has_cross_class_inversion` 的 debug 断言会真的用上。
- **按键路径上加锁**：`MemoryStore` 的方法是 `&self`，而多个 `Session` 共享它
  ⇒ 内部可变性 + 锁。红线仍是"按键 P50 < 1 ms"，所以：
  先用 `Mutex`/`RwLock` 把功能跑通，**然后必须实测**（第 5 步）；
  超标再换"每会话一份只读快照 + 版本号"之类的无锁读法。

**第 4 步：事件接线（`Learned` / `ForgetRequested`）**
`Session::drain_events` 已经会发 `Learned { input, text, origin, lane, attr }`。
接线在 `stele-cli`（以及将来的前端）：**每个按键之后** `drain_events`，
对 `Learned` 调 `record`，对 `ForgetRequested` 调 `forget`。
**这一步最容易漏**——漏了不报错，只是"学了没记住"。
所以要有**端到端断言**：同一串键跑两次，第二次的候选顺序变了。

**第 5 步：称重与验收**
- 给 `stele-bench` 加**每键词典查询次数**计数器
  （PLAN §6 那条"简拼的收益/成本"至今没实现，顺手做掉）。
- 用 `--scheme-dir schemes/stele-default` 量**加记忆前/后**两组数字：
  P50 / P99 / 常驻内存 / 加载时间。**数字要写回 §0 与 PLAN §9 的表**。
- 逐条对 7.6.0 的五条验收写测试或给出实测数字。

**第 6 步：CLI 开关与文档**
- 加 `--userdb <路径>`（**默认建议关**：刚克隆下来的行为要可复现）。
  默认值是产品决定，请所有者确认。
- 更新 `docs/HANDOFF.md` §0/§4/§5 与 `PLAN.md` §3（P4a 标 ✅）、§9 基线表。

### 7.6.3 已知的坑（提前写下来，省一次调试）

| # | 坑 | 为什么 |
| --- | --- | --- |
| 1 | **G10**：拿派生拼写当主键 | 记录**永远检索不到**；症状是"学过的词有时出现有时不出现"。见 7.6.1 ② |
| 2 | **在按键路径上落盘** | 红线是零磁盘 I/O；RIME 的 P99 36 ms 最大单点瓶颈就是 LevelDB 的磁盘读 |
| 3 | **衰减用浮点** | 它影响排序 ⇒ 必须可复现（D13）。`MemoryEntry::bonus` 已是定点，别半路引入 `f32` |
| 4 | **淘汰规则不确定** | 按容器遍历顺序淘汰 ⇒ 同一份数据两次运行淘汰不同条目，违反"可复现" |
| 5 | **忘了接 `Learned` 事件** | 不报错、只是"学了没记住"。P3 有四个 bug 是这种"接线在、但没被走到"的形状 |
| 6 | **服务穿过 `Query`** | `engine-design` §4 明确禁止：一个方案能挂多个服务，`Query` 里放不下 |
| 7 | **`forget` 对系统词的处理** | RIME 的语义是"**只取消调频效果**"，不是把词从词库删掉。别实现成"让系统词消失" |
| 8 | **重启后主键不一致** | 两次启动对同一个 `input` 算出不同键（例如带不带分隔符）⇒ 记忆"时有时无" |

### 7.6.4 工作量的诚实估计

| 步骤 | 规模 | 风险 |
| --- | --- | --- |
| 0 门禁 | 小（一个脚本 + CI 一段） | 低 |
| 1 量纲纯函数 | 小 | 低（但**决定后面一切**） |
| 2 存储实现 | **中到大**（选 B1 要写格式与合并） | 中 |
| 3 引擎接线 | 中（**唯一改接口的地方**） | **高**（锁 + 倒置守卫） |
| 4 事件接线 | 小 | 中（容易漏、不报错） |
| 5 称重 | 小 | 低 |
| 6 文档 | 小 | 低 |

**一句话**：这一步真正的难点不是"存下来"，而是
**①量纲对 ②主键是规范编码 ③按键路径上没有锁与磁盘**。
三件事都在 7.6.1 定过了，动手时按它走。

### 7.6.5 明确**不做**的事（避免范围膨胀）

- ❌ **不做下一词预测**（`Lane::Predict` 的 `predict_next` 先返回空）——那是 P4b。
- ❌ **不做向量重排**（P5，需先过内存预算评审）。
- ❌ **不改 `recognizer` 的三处语义分叉**——那是有源码依据的独立一项，见 §7 第 2 条。
- ❌ **不顺手修 `Uniquifier` 的去重口径**：它目前只比 `text` 与 `comment`，
  于是同一个词的简繁两种写法**不会被合并**（P3.5 实测：`zhongguo` 出
  「中国」+「中國」）。RIME 的 `uniquifier` 看着也是这样，所以不算 bug，
  但它确实是"候选里出现两条一样的词"的一个真实来源——
  **修它属于产品决定，不是 P4a 的一部分**。

---

## 7.7 给新会话的操作提醒

- 项目所有者**是初学者**，要求：新名词第一次出现就解释；不要高估基础；
  文档别写成座右铭（要能证伪、能写成断言或测试）。
- 他**明确表示过**：对外只维护**简体**形态；繁体留接口不维护（D32）。
- 他**授权过**：装 rustup。**没有授权**其它工作区外的操作
  （sudo 需密码，不能代劳）。
- **每次动手前后都要实测**：这个项目里"以为对"的记录见 §5（现在有 29 条）。
- **门禁与对照工装都在**：改动内核后跑 `bash scripts/verify-*.sh`；
  改动零件行为后跑 `python3 tools/compare-librime.py`；
  改动那 10 个内联零件后跑 `cargo test -p stele-engine --test number_oracle
  --test calc_oracle`（与上游逐字节对照）。
- **P3.5 留下了两套新工装**（`tools/README.md` 有用法）：
  `tools/fetch-sources.sh` 取回干净来源的数据，`tools/wordlist-gen/`
  把源数据编成 `.dict.yaml`。**动了默认方案/词库就跑它们**，
  并在改完后用 `stele --scheme-dir schemes/stele-default` 真的打几个词。
- **"装进来了"不等于"生效了"**：P3.5 里同一个症状（装配报告说装好了、
  功能就是不生效）出现了三次，原因各不相同。**配置类功能的验收必须是
  端到端断言**，不能只看 `--dump-config` 或 `--components` 的报告。
- **内核 crate 不许有数据文件**——包括"只是测试用"的对照数据
  （门禁抓到过我一次，已移到 `tools/oracle/`）。
- **改行为之前先读 `reference/` 里那两份以源码为准的调研**，以及
  `tools/oracle/README.md`——P3 到阶段 A 的 11 个 bug 全都写在里面。
- **不要相信"配置看起来正常"**：P3 抓到的四个 bug 症状完全一样——
  配置合法、没有报错、某个功能就是不生效。只有端到端测试抓得到。
- **不确定的约定，别写成"RIME 约定"**：要么引 librime 源码
  （`reference/` 里有两份现成的），要么写"这是我们的选择"。
