# HANDOFF — Stele-IME 项目交接

> **这份文件是为"上下文耗尽"写的。** 它把 P0–P4b 与阶段 A 的全部决策、
> 实测数字、踩过的坑、以及下一步压缩成一页可读的东西。
> 新会话只要读 **这份文件 + `PLAN.md` + `docs/engine-design.md`**，
> 就能接着干，不必回溯对话。
>
> 最后更新：**P4b（本地下一词预测）完成**、**P5 的本地向量偏好记忆第一版落地（默认关）**——
> P4b：`Lane::Predict` 通道打通、对比集 15 条端到端全过、**用户记忆的**按键路径仍零磁盘 I/O（词库查询仍走 `read_at`，见 §0 的读法）；
> P5：`stele-embed`（零依赖、无模型、无网络），把本地历史的共现计数投影成
> `i16` 向量，在 `Lane::Input` 上加有界偏好分；**默认关闭**（D46），
> 向量表实测 **2.44 MiB @ 4 万词**、延迟无可测变化。
> 两阶段合计牵出并修掉**七处静默缺陷**（事件丢了上下文 / 学习按 lane 二选一 /
> `Context::default()` 容量 0 直接 panic / 小 N 的 RSS 增量严重低估 /
> 设计文档两处编号规则的冲突 / 验收夹具commit错了候选 / 训练瞬态把 RSS 顶高），
> 见 §5 的第 41–47 条。
>
> **P5 的可行性评审报告**在 `docs/p5-vector-feasibility.md`；
> **执行书与实测**在 `docs/embed-design.md`。
> **下一步的候选**（**不按顺序自动开始**，除非所有者指定）：`recognizer`
> 的三处语义分叉 → 阶段 C 会话语义改造 → `select`（切方案）→
> **P5 的收益对比集（需要所有者手写）**。详见 §7。

---

## 0. 一分钟速览

**Stele-IME（石经）**：用 Rust 从原理重写的输入法引擎。
存在的理由：商业输入法占 300–400 MB；RIME（librime）成熟但依赖重、
**部分**方案把关键行为放进 Lua 插件（基础 `script_translator` 的造句/补全**不**依赖 Lua）。
上游文档没有把隐私写成承诺、早期计划里出现"添加網絡功能"——这只能说明
**"上游没把它当承诺"**，**不能**推断"librime 会联网或不保护隐私"（审计 §3.1）；
Stele 要做的，是把自己的离线与隐私做成一条**可核对**的承诺（`docs/privacy-model.md`）。

> **合规与隐私入口**：第三方来源 / 固定 revision / 许可见
> `THIRD_PARTY_NOTICES.md`；隐私边界见 `docs/privacy-model.md`。

**红线**：常驻内存 < 30 MB、部署峰值 < 150 MB、按键 P50 < 1 ms、P99 < 10 ms、
产物/源 < 3×。

**实测现状**（release）：

| 指标 | 实测 | 目标 |
| --- | --- | --- |
| 按键路径 P50（**真实词库 41 万条**，拼音） | **约 50 µs**（冷页缓存）／**约 15 µs**（热） | < 1 ms ✅ |
| 按键路径 P99（同上） | **约 110 µs** | < 10 ms ✅ |
| 常驻内存（真实词库） | **13.6 MiB** | < 30 MB ✅ |
| 常驻内存（真实词库 **+ 3 万条用户记忆**） | **17.8 MiB**（一次运行里新学 3 万条则 19.4 MiB） | < 30 MB ✅ |
| 按键 P50/P99（真实词库 + 3 万条记忆） | **约 50–52 / 110 µs**（与无记忆的差在 2 µs 内） | ✅ |
| 每键词典查询次数（拼音 / 真实词库） | **P50 19 / P99 64 / max 64**（平均 33.2） | — |
| 用户记忆落盘体积 | 3 万条 = **1.21 MiB**（单条约 42 字节） | — |
| 引擎装载（真实词库，产物命中） | **83–91 ms** | — |
| 首次部署（编译 41 万条产物 + 装载） | **0.62 s**（峰值 **21 MiB**） | < 150 MB ✅ |
| 词库产物 / 源 `.dict.yaml` | 14 MB / 11 MB ≈ **1.3×** | < 3× ✅ |
| 按键路径 P50（字形码演示方案） | **180 ns** | — |
| **预测表单条内存**（P4b，斜率法实测） | **≈188–211 字节/条** | — |
| **预测内存增量**（P4b，默认上限 2 万条） | **≈4.0 MiB** | < 5 MB ✅ |
| 按键路径 P50/P99（真实词库 + 记忆 + 预测） | **48.0 / 104.3 µs**（与无预测差在噪声内） | ✅ |

**两套路经**：`stele` 在仓库根目录跑会**自动用 `schemes/stele-default`**
（41 万条，部署路径）；换到别的目录跑则用**内嵌演示词库**（几十条）
兜底——**生成词库不进二进制**（它 11 MB，见 `pinyin.embedded.schema.yaml`）。

> **约 50 µs 与约 15 µs 是同一份产物的两次测量**，差别在**操作系统的
> 页缓存**：词条页留在页缓存里（这正是 `stele-table` 的设计目标——
> 文件大小 ≠ 常驻内存），冷的时候每次查询要走两次 `read_at`。
> 两者都远在 1 ms 红线之内。

**用户记忆默认关闭**：不给 `--userdb <路径>` 就没有记忆。
理由：刚克隆下来的行为必须**逐字节可复现**（HANDOFF §7.6.2 第 6 步的产品决定）。

**延迟数字必须注明方案与词库**：`stele-bench --schema=<id>`、
量真实词库还要 `--scheme-dir`。演示词库（几十条）与真实词库（41 万条）
是同一套代码，差 80 倍；零件数也不同。

**进度**：P0 ✅ P1 ✅ P2 ✅ P2.5 ✅ P3 ✅ P3.5 ✅ P4a ✅ · **阶段 A 装配补齐 ✅**（10/10）
→ **P4b 本地下一词预测 ✅** → 下一步见 §7

| | |
| --- | --- |
| 测试 | **484 个**（clippy 零警告，四条 CI 门禁全过，`cargo fmt --check` 干净） |
| crate | **10 个** |
| Rust | 约 33500 行 |
| 词条 | **414525 条**（414111 词 + 414 补录单字），399 个编码单元 |
| 零件 | 注册表 40 项：已实现 32、需数据 2、需资源 2、不适用 1、未实现 3 |
| 阶段 A 的 10 个内联零件 | **10/10 已装配**（此前只有实现、没有装配）——见 §4 与 §5 第 36 条 |
| P4b 的对比集 | **15 条**常见搭配（`tools/predict/collocations.tsv`），端到端全过 |
| P5 的对比集 | **3 条**上下文消歧用例（`tools/embed/context-cases.tsv`）——**证据还很小**，见 `docs/embed-design.md` §3 |
| 本地向量表（P5，默认关） | **2.44 MiB @ 4 万词**（`dim=32, i16`）；装载峰值约 +9 MiB；延迟无可测变化 |

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
├── crates/stele-schemes/tests/schemes-inline/  ← **内联零件的端到端测试方案**
│                                               （10 个零件全部声明；改装配路径必跑）
├── scripts/verify-*.sh      ← 四条 CI 门禁（含依赖许可审查）
├── scripts/deps-allowlist.txt ← 受审依赖白名单（现在是空的：零第三方依赖）
├── tools/                   ← 验证工装（**不进内核 crate、不进 CI**）
│   ├── fetch-sources.sh     ← 取回干净来源的数据（带 sha256 校验）
│   ├── wordlist-gen/        ← 源数据 → `.dict.yaml`（自写，含自检）
│   ├── predict/             ← **下一词预测的对比集**（P4b 的验收线）
│   ├── embed/               ← **本地向量偏好记忆的对比集**（P5/D46 的验收线）
│   ├── embed-probe/         ← **P5 的内存预算探针**（合成向量表，不载模型）
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
    ├── stele-memory/        用户记忆：频率 + 时间衰减（**自写 KV，零依赖**）
    ├── stele-embed/        本地向量偏好记忆：本地历史 → 整数向量 → 有界重排（**零依赖、无模型**）
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
| **D43 管线的排序在滤镜之前**（阶段 A） | 顺序固定为 **翻译 → 重排 → 排序 → 滤镜**；`Pipeline::finalize` **已删除** | 滤镜用**位置**表达意图（`long_word_filter` 的 `idx: 4`），所以它看到的必须是最终顺序，**后面不能再排一次**。原先排序在滤镜之后，把 4 个重排型滤镜的效果**原样抹掉**（实测）。删掉 `finalize` 而不是留成空操作，是为了不让人往里放"收尾排序" |
| **D42 用户记忆的键是"规范编码"，随候选带出**（P4a） | `Candidate::key`（如 `ni'hao`）由**翻译器**从编码渲染；上屏时随候选进入 `Commit`，落库与查询都用它 | 编码本来就在翻译器手里，是到候选列表这层才丢的。带上它之后：**同一条编码只有一把键**，于是 `nhao` 与 `nihao` 走到 `[ni,hao]` 时共享同一份记忆——**与 rime-ice 功能等价**（RIME 的用户词典同样以 `code` 为键）。没有任何一层需要反查 |

> **一次自我纠正（D41 → D42）**：第一版把"重排器拿不到编码"当成了架构约束，
> 于是键取"规范化拼写"，代价是**跨拼法不共享**。所有者追问
> "为什么不能用编码"之后查明：那只是"当时没把编码传下来"，不是分层限制——
> `Candidate` 加一个字段就解决了。教训见 §5 第 37 条。

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
- **"实现了"与"被装配了"是两件事**。注册表里的 `Implemented` 说的是
  "我们写了实现"，**不是**"装配路径会用到它"——两者差过一次很远：
  阶段 A 的 10 个零件全都有实现、全都有单元测试、注册表全标着"已实现"，
  而 `build_pipeline` 里**一次都没引用过**（§5 第 36 条）。
  **加零件时要同时加装配分支，或者让"没有装配分支"可见**
  （现在有 `scheme::assembles()` 与一条守着它不漂移的测试）。
- **排序的位置是语义，不是实现细节**：管线只有一种顺序对——
  **翻译 → 重排 → 排序 → 滤镜**。滤镜用**位置**描述意图（"提到第 4 位"），
  所以它看到的就是最终顺序，**后面不许再排一次**（§5 第 38 条、`engine-design` §5.4.1）。
- **不要把自己实现的现状说成架构约束**。"重排器拿不到编码"听着像分层定律，
  其实只是"我当时没把编码传下来"——**"做不到"与"我没做"是两句不同的话，
  前者需要证据**（§5 第 37 条）。
- **"值看起来差不多对"的解析错误要靠新语法的解析测试来抓**：
  流式集合里的 `["abc"]` 会多出一个引号、`["a\tb"]` 的转义不会被解开
  （§5 第 40 条）。写测试方案时每用到一个**没测过的写法**，就补一条解析层测试。
- **一半的修正比不修更糟**：`send` 的语义我改了 `KeyBinder` 却忘了改 `pipeline`，
  于是那个 `redirecting` 字段永远是 `false`——两半对不上，而测试当时是绿的
  （因为旧的"从中间派发"实现也能让空格到达选择器）。
  **跨两处的语义改动，要有一条只在正确实现下才通过的测试。**

---

## 4. P0–P4b 与阶段 A 的交付

### 已完成

| 阶段 | 交付 |
| --- | --- |
| **P0** | workspace、CI、三条门禁（P4a 时增至四条）、称重台、许可证、README |
| **P1** | 拼写层、词库、两族翻译器、处理器、过滤器、流水线、会话；`nihao`→你好、`nh`→你好、`shape ab`→十 |
| **P2** | `stele-config`（YAML 子集 + `$ref` + 分层补丁）、`stele-dict`、目录装载、`--scheme-dir` |
| **P2.5** | `stele-table`：紧凑二进制 + 流式编译器 + 按需分页（500k 词条 245→46 MiB） |
| **P3** | **完整拼写代数**（含自写正则）、**零件集**（24 个名字里 22 个已实现）、**词条补全**、**分层 `--dump-config`**（每个值标来源）、**与 librime 的对照工装** |
| **P3.5** | `schemes/stele-default`：自有方案 + **41 万条**词库 + OpenCC 数据装载 —— 见下文 |
| **P4a** | `stele-memory`：频率 + 时间衰减的用户记忆、`Services` 注入、CLI `--userdb` —— 见下文 |
| **P4b** | `stele-memory` 的**预测表** + `Lane::Predict` 通道 + 对比集 `tools/predict/` + CLI `--predict` —— 见下文 |

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

### 阶段 A：把 rime-ice 的 Lua 插件重写成原生零件（10/10 ✅ 已装配）

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

> **阶段 A 的 10 个零件：10/10 已装配**（2026-09 补齐）
>
> **此前的状态**：代码是真的、单元测试是绿的、`--components` 标着"已实现"，
> 而 `LoadedScheme::build_pipeline` 里**一次都没引用过它们**（`grep` 数出 0 次）
> ——`engine:` 里写了名字也会被 `_ => {}` 静默跳过。
> **"已实现"在注册表里指的是"有实现"，不是"会生效"。**
> 详见 §5 第 36 条。
>
> **补齐时又发现三件事**（这就是为什么"先打通一条模板链"值回票价）：
> ① 其中 4 个**重排型滤镜装配了也不生效**（排序在它们之后，把结果抹掉，
> §5 第 38 条）→ 已按**所有者决定**把排序移到滤镜之前，并删掉
> `Pipeline::finalize`（D43）；② 非字母数字输入被静默丢掉（§5 第 39 条）；
> ③ 流式集合的引号解析错值（§5 第 40 条）。
>
> **验收**：`crates/stele-schemes/tests/inline_components.rs`，15 条端到端断言，
> 方案在 `tests/schemes-inline/`。**每个零件一条**：
>
> | 零件 | 验收方式 |
> | --- | --- |
> | `date_translator` | `rq` → 定格的那一天 |
> | `calc_translator` | `cC1+2` → `3` 与算式两条候选 |
> | `unicode_translator` | `U62fc` → 「拼」——**前缀是从 `recognizer` 模式推的**（上游行为） |
> | `number_translator` | `R3355` → 「三千三百五十五」（同上） |
> | `uuid_translator` | 方案配的 `uuid-test` → 真 UUID（CLI 里两次不同 ⇒ 真随机） |
> | `autocap_filter` | `Hello`（码）→ `Hello`（候选） |
> | `long_word_filter` | `count: 2 / idx: 4` → 长词真的被提到第 4、5 位 |
> | `pin_cand_filter` | 两种写法各占一组编码：上游的 `编码<TAB>词`、我们的 `{preedit, texts}` |
> | `v_filter` | 声明它之后普通输入一字不变 |
> | `reduce_english_filter` | `mode: custom` + `words` 可读、方案可跑 |
>
> **可检查性（这才是缺口真正的教训）**：判断"某个零件名有没有装配分支"
> 现在有一个**可执行**的答案 —— `stele_engine::scheme::assembles(name)`，
> 且有一条测试（`assembles_agrees_with_what_build_pipeline_actually_builds`）
> 守着它不会与 `build_pipeline` 漂移：**说 true 就必须真的多一个组件，
> 说 false 就必须有一条降级说明**。声明了没有装配分支的零件会得到降级说明。

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

### P4a 已完成（用户记忆：频率 + 时间衰减）

**验收标准**（`PLAN.md` §3 的 P4a 行 + §7.6.0 的五条）：**打过的词下次优先；
用户记忆的按键路径零磁盘 I/O**（不是"整条按键路径零 I/O"：词库查询仍按需 `read_at`，见 §0）。

| 交付 | 内容 | 证据 |
| --- | --- | --- |
| **新 crate `stele-memory`** | 零依赖、无 `unsafe`。`decay`（量纲纯函数）+ `store`（自写 KV）+ `ranker`（接到排序上）+ `events`（事件接线的唯一映射点） | 51 个测试 |
| **量纲** | `bonus = MAX × f / (f + H)`，`f` 是**整数次减半**的衰减退频（半衰期 30 天）。全程整数 ⇒ 跨平台逐位一致（D13） | `boundary_table_is_exact` 逐值表 |
| **存储** | 自写紧凑 KV：魔数 + 版本 + 记录表 + FNV-1a 校验和；落盘 = 临时文件 + 原子 `rename` | 8 个格式测试（截断 / 翻位 / 版本 / 尾部垃圾） |
| **上限与淘汰** | 默认 **30 000 条**（实测反推，见下）；淘汰**衰减频次最低**的条目，平局按 `(输入, 词)` 字典序 ⇒ 确定 | `eviction_is_deterministic_and_drops_the_coldest` |
| **服务注入** | `stele_core::Services`（时钟 + 重排器链）；`LoadedSchema::build_pipeline(&Services)` —— 服务在**装配时注入**，不穿过 `Query` | 引擎只多了一个字段 |
| **事件接线** | `stele_memory::apply_events`：`Learned → record`、`ForgetRequested → forget`。**三个前端共用同一段代码** | `learned_and_forget_events_become_the_right_calls` |
| **CLI** | `--userdb <路径>`（**默认关**）、`--dump-memory`、`--select=<n>`（不选中一个"不是第一个"的候选，就永远观察不到学习） | 见 §6 的命令 |

**五条验收逐条对账**

| # | 验收 | 怎么证的 |
| --- | --- | --- |
| 1 | 打过的词下次优先 | 端到端：自建方案（甲 10000 / 乙 1，同码），把「乙」选 4 次 → 它升到第 1；**打 1 次时断言它还没升**（分界点两侧都钉住）。另有**跨拼法**两条：简拼学的全拼吃得到、全拼学的简拼吃得到；以及"落库的键不是拼写而是编码" |
| 2 | **用户记忆的按键路径零磁盘 I/O**（红线） | `crates/stele-memory/tests/no_disk_io_on_keypath.rs` 读 `/proc/self/io`：`syscr`/`syscw` 与 `read_bytes`/`write_bytes` 在 1000 次按键后**一个都没涨**。范围是记忆/预测/向量这条路径；**词典查询（`TableLexicon`）仍走 `read_at`** |
| 3 | 重启后学到的词还在 | 落盘 → 新引擎 + 新 store → 顺序仍然是学过的那个 |
| 4 | 内存增量可量化且不超预算 | `stele-bench --seed-memory=N`：3 万条 + 真实词库 = **17.8 MiB**（一次运行里新学则 19.4 MiB） |
| 5 | 坏文件 = 降级 + 警告 | 写入垃圾 → 警告一行、引擎照常打字、**且不覆盖那个文件** |

**一处必须讲清的偏离**

**上限取 30 000，不是计划里建议的 100 000**。实测（`--seed-memory`）：
10 万条要 **+18.2 MiB**，叠上真实词库会**越过 30 MB 红线**。
3 万条 → 总常驻 17.8–19.4 MiB，给 P4b/P5 留出约 10 MB。
**这是"数字必须实测"的又一次兑现**：估算会得到"几十字节/条"，真相是三到四倍。

**键是规范编码（D42）——与 rime-ice 功能等价**

`Candidate::key` 由**翻译器**从编码渲染（`ni'hao`），上屏时随候选进入
`Commit`，落库与查询都用它。于是：

- **`nhao` 学的词，敲 `nihao` 时照样优先**（实测 9210 → 18543）；
- 反方向也成立（全拼学的，简拼下 8517 → 16917）；
- 落库的键**就是编码**：`shijie` → `shi'jie`（CLI `--dump-memory` 可见）。

CLI 复现：

```bash
stele --scheme-dir schemes/stele-default --candidates=1 nihao      # 基线 9210
for i in 1 2 3 4; do stele --scheme-dir schemes/stele-default --userdb /tmp/u.mem nhao; done
stele --userdb /tmp/u.mem --dump-memory                            # 键是 ni'hao
stele --scheme-dir schemes/stele-default --userdb /tmp/u.mem --candidates=1 nihao   # 18543
```

**红线仍然成立**：真实词库 + 3 万条记忆下，P50 **约 50–52 µs**、P99 **约 110 µs**
（与无记忆相比差在 2 µs 内）、常驻 **17.8 MiB**。
带上编码键本身要**分配字符串**，因此它在 4 万次按键上换来约 **+2 µs**——
渲染只在"这条展开边真的产出了候选"时才做（实测：不做这个优化要多花 3–4 µs）。

### P4b 已完成（本地下一词预测）

**验收标准**（`PLAN.md` §3 的 P4b 行）：**常见搭配排序改善（需定义对比集）；
内存增量 < 5 MB；神经模型撞红线，暂缓。**

| 交付 | 内容 | 证据 |
| --- | --- | --- |
| **对比集**（先于代码） | `tools/predict/collocations.tsv`：**15 条**常见搭配链（交替的 `编码/词` 对），两对的是 bigram 用例、三对的是 trigram 用例 | `tools/predict/README.md` 写了格式与"加一行 = 加一条用例" |
| **预测表** | `stele-memory` 的第二张表：`(ctx1, ctx2, 词) → 记录`；`ctx1` 为空串表示 bigram。**与输入表分开两段**（键空间不同），但**同一个文件**（一起原子替换） | 文件格式升到 **v2**；两段各有 round-trip / 截断 / 翻位 / 版本门禁测试 |
| **上下文粒度** | **trigram 优先 + bigram 回退**（所有者选定的粒度 + 稀疏数据的安全网）：一次上屏写两条，查询时长的有记录就只答它 | `trigram_wins_over_bigram_when_both_are_known` 等 6 条 |
| **纯函数评分** | 复用 P4a 的整数衰减曲线（`bonus_now`）——**没有引入任何浮点** | `prediction_records_decay_with_time` |
| **上限与淘汰** | 默认 **20 000 条**（实测反推，见 §0 的单条成本）；淘汰**衰减频次最低**的，平局由 `BTreeMap` 键序决定 ⇒ 确定 | `prediction_eviction_is_deterministic_and_drops_the_coldest` |
| **引擎接线** | `Services.prediction`（装配时注入）+ `PipelineImpl::append_predictions`：跑在**滤镜之后**（插入位置即最终位置），`Lane::Predict` 内部按分数降序 | `predictions_are_inserted_right_after_the_first_input_candidate` 等 6 条 |
| **空输入也有预测** | 输入为空时 `compose` **不再直接返回空**——"刚打完一个词"正是预测的主场景；上屏后会话立刻重算一次 | `predictions_are_visible_with_no_input_at_all` |
| **盲选编号不漂移** | 数字键数的是"第 N 个**输入**候选"（`SessionState::selectable_index`），因此预测插在中间也不会把编号推后 | `selector_counts_only_input_candidates` + `the_keyboard_ordinal_skips_over_the_prediction_block` |
| **事件接线** | `Event::Learned` 新增 **`context`** 字段（`Lane::Predict` 的学习键就是它）；`FileMemory::record` **按 lane 分流但不互斥**（Input 上屏同时写两张表） | `a_prediction_event_carries_its_context_to_the_store` + 对比集端到端 |
| **CLI** | `--predict`（**默认关**，需要 `--userdb`）、`--commit-seq=甲,乙`（连续上屏，手工复现多上下文功能）、`--dump-memory` 多打一张预测表 | 见 §6 的命令 |
| **称重** | `stele-bench --predict / --seed-predict=N / --predict-cap=N` | 数字在 §0 |

**对比集的验收口径（所有者拍板）**：自建搭配语料 + 存档表，
**断言"学过之后期望词是预测通道的第 1 位"**。每一行用**各自独立的一份记忆**跑，
因此行与行之间不会互相污染（否则断言会变成对处理顺序的隐式依赖）。
基线那一步（"还没学过时期望词不该被预测出来"）不是装饰：
没有它，"第 1 位"可能只是它恰好从别处冒出来。

**两条所有者决定，与 §7.7.2 的建议不同或需要说明的**：

| 待定 | 结论 | 说明 |
| --- | --- | --- |
| ② context 粒度 | **trigram**（不是建议的 bigram） | 落地时补了 **bigram 回退**：只用 trigram 时新用户前几周几乎预测不出东西。回退而不是插值——后者要为两段上下文定权重（PLAN D45） |
| ④ 通用搭配表 | **只留接口 + 本地自建** | 与 §7.7.2 的建议一致。`PredictionOrigin::General` 这条通路在类型上留着，但今天唯一的来源只能是用户自己的历史（`Personal`） |
| 预测默认开关 | **默认关**，且与 `--userdb` 是两个开关 | 照 §7.7.3 第 6 步"倾向于默认关"：给了 `--userdb` 只代表"记下来"，还要再加 `--predict`。执行点是**装配期的服务注入**：没有服务就没有预测候选 |
| ③ 内存预算 | **20 000 条 ≈ 4.0 MiB**（实测反推） | 单条 ≈188–211 字节（斜率法）。直接读小 N 的 RSS 增量会**严重低估**——见 §5 第 44 条 |

**一处刻意没加的东西**：`Candidate` **没有**加"这条预测来自个人还是通用"的字段。
§7.7.4 第 6 条提到 G12 要能区分两者，但今天 `PredictionOrigin::General`
**没有任何生产者**（通用表不随项目分发）。按本项目"加字段前先问谁读它"的规矩
（§5 第 18 条），留到真有通用表时再加——那时它才有消费者。

### P5 第一版已落地（本地向量偏好记忆，**默认关**）

**执行书与实测**：`docs/embed-design.md`；**可行性评审**：`docs/p5-vector-feasibility.md`。

| 交付 | 内容 |
| --- | --- |
| **新 crate `stele-embed`** | **零依赖、无模型、无网络**。把用户本地历史里的 `(上下文词 → 下一个词)` 计数**投影**成 `i16` 整数向量（`tools/embed-probe` 量的那种），在 `Lane::Input` 上加有界偏好分 |
| **确定性** | 投影由词的哈希决定（无随机）、累加是整数、量化是整数除法——**没有一处浮点**（D13）。同一份历史 ⇒ 逐位相同的向量 |
| **保守的重排** | 只给"不是猜的"候选加分（铁律在算术上不可能被违反）；**按名次**而非分值（没有可调常数）；默认前 3 名、≤4000 毫对数 |
| **装配** | `Services::rankers` 里接在 `MemoryRanker` **之后**；CLI `--embed`（**默认关**，需 `--userdb`）；称重台 `--embed` |
| **对比集** | `tools/embed/context-cases.tsv`：3 条"同码词、上下文说话"的用例 |
| **实测** | 对比集 **3/3**（基线 1/3，**无回归**）；向量表 **2.44 MiB @ 4 万词**；装载峰值约 +9 MiB；按键 P50/P99 无可测变化 |

**它填的空**（这是它存在的理由）：词库权重、P4a、P4b 都没有回答
"**同一个编码下、按上下文该选哪个词**"——P4a 不看上下文，P4b 只喂
`Lane::Predict`。实例：`tianqi` 下 `天气`/`田七` 谁在前。

**必须讲清的局限**（否则下一个人会高估它）：
- 它现在学的是"**这段上文之后出现过什么**"，**不是词义**。一点点哈希平滑
  不等于"理解了语义"。
- **没有两步共现、没有子词回退**：没见过的上下文词**没有向量**（`docs/embed-design.md` §5）。
- **收益证据还很小**：3 条用例、还是实施者自己写的，只证明**机制成立**。
  要回答"值不值得默认开"，需要**所有者手写**的更大对比集（§7）。

### 下一步

按建议顺序：

1. **`recognizer` 的三处语义分叉**（记在
   `reference/rime-recognizer-and-affix.md` 的差异表）：我们锚死在位置 0、
   用"正则是否以 `$` 结尾"的启发式、取最长认领而非名字典序第一条。
   前两处会影响真实方案。
2. **阶段 C：会话语义改造**（解锁 `select_character` + 拆字辅码）。
3. **`select`（切方案）** 与 **`send_sequence` 的用例**——数据结构已就位。
4. **简拼的边界**：`nhao` → 你好，而 `nh` 打不出「你好」（见 §5 第 28、32 条与
   `pinyin.schema.yaml` 里那段注释）。要支持 `nh` 需要在展开里保住
   `[ni][hao]` 这条**完整**切分，属于拼写代数的下一步。
5. **P4b 留下的两条可选改进**（都不是缺口，是下一层的收益）：
   · 预测的**通用搭配表**（要有人先提供一份可分发、许可清楚的数据）；
   · 预测候选的 **UI 来源标记**（`PredictionOrigin` 要真有第二个生产者才值得加字段）。

**一件不该忘的事**：`reference/` 下有两份**以 librime 源码为准**的调研
（`rime-key-binding-actions.md` 1084 行、`rime-recognizer-and-affix.md` 573 行），
每份文末都有「与 stele 实现的差异」表。**动手改这些零件之前先读它们**——
P3 里有四个 bug 是"我猜了一个约定"造成的，而它们全都写在里面。

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
| 30 | **`stele --check` 的自检写死了方案 id**：P3.5 把内嵌演示方案从 `pinyin` 改名成 `pinyin-demo`，**漏改了自检里的 `run("pinyin", …)`**。于是自检从那时起一直在报"方案不存在：pinyin" | 自检引用**名字**就会随重命名腐坏。修法不是把字符串改对，而是**从实际装载到的方案里取**（按翻译器族挑）。**在干净 HEAD 上复现过** |
| 31 | **同一个自检的第②步还写着一个已被证伪的期望**（`nh` → 你好）：§5 的第 28 条早就写明 `nh` 打不出来，而自检里的期望没跟着改 | "已知行为"要有**一处权威**；改行为时要搜一遍谁依赖它。自检要守的是"变体拼写能上屏"这条不变式，不是某个具体缩写串（改用 `nhao`，并在代码里写清为什么不是 `nh`） |
| 32 | **这两处失效存在了很久，而没有任何东西提醒**：CI 里跑了 `--check`，但它不在本地必跑清单里；用户与贡献者都不会看到 CI 红 | 自检要么**进 `scripts/verify-*.sh`**（本地与 CI 同一条命令），要么等于没有。**一个没人跑的检查就是没有检查** |
| 33 | **"零磁盘 I/O"的测试第一版抓不住违规**：只读 `write_bytes` 时，往 `/tmp` 写文件**不会**让它变化（写在页缓存里，块设备层还没见到）。我加上一次故意的写，**测试照样绿** | **反向验证不只验证"检查会不会失败"，还验证"尺子准不准"。** 改用 `syscr`/`syscw`（系统调用次数）后，故意的写立刻报出 1000 次 |
| 34 | **HANDOFF §7.6.1 ③ 建议的条目上限（100 000）实测越线**：10 万条 +18.2 MiB，叠上 13.6 MiB 的真实词库会**越过 30 MB 红线** | **建议值也是待验证的数字。** 改成 30 000（总常驻 17.8–19.4 MiB）。单条实测 150–190 字节——"几十字节"的估算差三到四倍 |
| 35 | **称重台的 `引擎装载` 把造测试数据的时间算了进去**：`--seed-memory` 插在装载与计时取点之间，于是打出了"装载 106 ms"（其中 17 ms 是 seeding） | 计时区间的边界要**写在代码的顺序里并加注释**，别指望读者按顺序推断 |
| 36 | **阶段 A 的 10 个内联零件在装配路径里根本不存在**：`date_translator` / `calc_translator` / `long_word_filter` / `autocap_filter` / `v_filter` / `pin_cand_filter` / `reduce_english_filter` / `unicode_translator` / `uuid_translator` / `number_translator` 有实现、有单元测试、注册表里标着"已实现"，而 `LoadedScheme::build_pipeline` 里**一处引用都没有**（用 `grep` 逐个数过：0 次） | **"实现了"与"被装配了"是两件事**。"已实现"在注册表里指的是"有实现"，不是"会生效"——**同一个词在两处含义不同，而没有任何东西会发现**。修法不只是补装配分支：还要让"有没有装配分支"本身**可被检查**（`assembles()` + 一条守着它不漂移的测试），否则下次加零件还会重演。**✅ 已修（10/10），但它牵出了下面两条** |
| 37 | **我把"我的实现没把数据传下来"说成了"架构不允许"**：第一版的用户记忆以**拼写**为键，代价是简拼学的词帮不到全拼——而我给出的理由是"重排器拿不到编码，这是分层决定的"。**所有者追问"为什么不能用编码"，一查就发现那只是我当时没把编码从翻译器传到候选上**（`Candidate` 加一个字段而已）。更糟的是，权威设计文档 §8 与 §9 ⑥ 早就写好了正确的形状（记忆以编码为键、由会话负责记录），我却照着 HANDOFF 的一段执行建议走偏了 | **"做不到"与"我没做"是两句不同的话，前者需要证据。** 把实现现状包装成架构约束，会让一个本可以修的设计缺陷看起来像一条定律。**遇到"为什么不能 X"时，先去找"X 需要哪一块数据、那块数据现在在哪"**——答案往往只是一个字段的距离 |
| 38 | **4 个"重排型"滤镜在管线里根本不生效**：`long_word_filter` / `v_filter` / `pin_cand_filter` / `reduce_english_filter` 都是**移动 `Vec` 里的元素**来起作用，而引擎在滤镜之后还会按分数排序一次（`Pipeline::finalize`）——**那次排序把它们辛苦排好的顺序全丢了**。实测（声明 `long_word_filter` 的方案，`ab` 的六个同码词）：滤镜把两个长词提到了第 4、5 位，最终输出仍是**纯粹的权重序**。它此前完全没被发现，因为**默认方案与 `p3features` 都没声明这四个**，而单元测试直接调 `apply()`、绕过了整条管线 | **"滤镜改了顺序"与"最后还会排序"是两件不能同时成立的事。** 只有一种顺序对：**翻译 → 重排 → 排序 → 滤镜**（RIME 的语义：滤镜看到的就是排好序的列表，它的顺序就是最终顺序）。**✅ 已修**，并且 **`Pipeline::finalize` 被删掉**——留着一个"不再排序的 finalize"就是给下一个人挖坑。**只装配它们是不行的**：那会造出"装配报告一切正常、功能就是不生效"的最坏形状（这条也是**先装配、后用探针实测才发现**的） |
| 39 | **`speller.input_alphabet` 是一个 RIME 没有的键**：装载器只从它读"允许敲哪些字符"，于是任何**原生 RIME 方案**的这个设置都被忽略，退回"ASCII 字母数字 + 分隔符"的兜底。后果是**非字母数字输入被静默丢掉**：雾凇的辅码引导符 `` ` ``、`v` 模式的符号、计算器要用的 `+ - * /` 全部打不进去，症状是"敲了没反应"而没有任何报错 | RIME 的 `speller/alphabet` **就是**"允许敲哪些字符"（`initials` 是其中只能作始码的子集）。已在 B 阶段修正：**字符串写法**的 `speller.alphabet` 现在同时是输入字符集（列表写法仍只当编码字母表，因为它反推不出字符集）。抓它的是"计算器要打 `+`"这条端到端断言——**又一个"只有真的跑一遍才会发现"的缺口** |
| 40 | **流式集合里的引号解析错了两处，而且都是静默错值**：`raw_token` 把**闭引号**也塞进了 token（`["abc"]` → `abc"`），而且**转义从不被解开**（`["a\tb"]` → 字面反斜杠 + t）。块式写法（`k: "abc"`）一直是对的，所以它躲过了所有既有测试 | 抓到它的是**写测试方案**：`texts: ["十字架"]` 读出来多一个引号，于是置顶规则匹配不上，而**没有任何报错**。修法是让 `raw_token` 只负责"哪里是边界"（引号照收），解析仍只有 `parse_scalar` 一处。**"值看起来差不多对"是最难查的一类错**——它不会崩，只会让功能莫名其妙地不匹配 |
| 41 | **`Event::Learned` 里没有 `context`**，而 `Lane::Predict` 的学习键**就是**上下文。少了它，`apply_events` 组出来的 `Commit` 上下文永远是空的，于是预测学习**每次都静默地什么都不记**：事件发了、函数调了、预测表永远是空的 | **一个字段的缺失就能让整条链"接上了但学不会"**，而它不报错。抓到它的办法是"按 owner 的对比集写端到端断言"——单元测试若只测 `record`（直接构造带上下文的 `Commit`），那条路永远是绿的。**"接线在"与"数据到得了"是两件事** |
| 42 | **`record` 按 `lane` 二选一，于是主路径永远学不到预测**：我第一版写成 `Input → 输入表 / Predict → 预测表`，而用户一个词一个词打字时**全部**是 `Lane::Input`——预测表因此永远是空的。对比集测试第一次运行就红了 | 两张表**不是互斥的**：`Lane::Input` 的上屏既该记编码键，**也该**记"上下文 → 这个词"。修法是把分流写成"总是写预测表，输入表只在 Input 通道写"。**"分流"不等于"二选一"**——写下 `match lane` 时要想清楚每个分支是不是真的排他 |
| 43 | **`Context::default()` 的容量是 0**（`derive(Default)` 给的），而 `push` 在 `cap == 0` 时会对**空的 `Vec`** 调 `remove(0)`——**直接 panic**。这条路径此前没被走到：真实会话用 `with_capacity(8)`，而用 `default()` 的地方恰好从不 `push` | 写预测测试时按了第一次它就炸了。**"永远不会被走到"的分支不是安全的，它只是还没被走到。** 修法是手写 `Default` 走 `with_capacity(1)`，并补一条"默认值也能 push"的测试 |
| 44 | **小 N 上直接读 RSS 增量会严重低估预测表的内存**：`--seed-predict` 说"20 000 条 → 增量 64 KiB"，而斜率法（10 万 / 20 万条）给出的是 **≈200 字节/条**（20 000 条 ≈ 4 MiB）。原因是**引擎装载那一大段会留下一片已驻留的空闲堆**，前两万条记录直接把它填满，RSS 一动不动 | **RSS 增量只在"新申请的内存"上可信**；复用已驻留的空闲页时它对你是隐形的。修法不是改测量代码，而是**换一个量法**：突破默认上限、量两点之间的斜率（`--predict-cap=300000 --seed-predict=100000/200000`）。这与 §5 第 33 条（尺子不准）是同一类教训——**先确认尺子量的是你要的东西** |
| 45 | **设计文档 §4.3.2 与 §4.3.3 的交点没人检查过**：前者要"预测插在第 1 名之后"，后者要"`index` 指向已渲染列表"——两条同时成立时，插进来的预测会把后面**所有输入候选的编号推后**，用户按 `2` 命中一条预测，而预测不参与盲选（表现为"按了没反应"）。而 §4.3.1 整节的理由恰恰是"保护盲选肌肉记忆" | **两条都对、合起来错**——这类冲突只有在实现时才暴露。修法不是在两条里选一条，而是**把两个编号空间显式换算一次**（`SessionState::selectable_index`），于是插入位置与编号稳定性都保住了（PLAN D44）。**写下一条规则时，顺手检查它与上一条的交点** |
| 46 | **验收夹具自己 commit 错了候选**：P5 的对比集第一次跑出来"基线对 2 条、向量没修好任何一条"——一个**看起来很合理**的结果。真相是夹具用 `select(0, …)` 上屏，而 `tian qi` 的第一个候选并不总是该行写的那一个词，于是 6 行"农田→田七"实际全都在打 `天气`，向量自然无从学起 | **对照实验的夹具本身也要被验证**。症状是"结果不显著"，而它太容易被解释成"这个方法没用"。修法：按**文本**找下标再上屏（`position(|c| c.text == word)`），找不到就 panic。**一个喂错数据的实验，比不做实验更糟**——它会给出一个可信的否定结论 |
| 47 | **训练瞬态把 RSS 顶高，与第 44 条方向相反**：`--embed` 实测常驻 32 MiB，而向量表只有 2.44 MiB——差额是 `V×dim×4` 的 `i32` 累加缓冲（5.1 MiB）与调用方的样本快照（约 3 MiB）。它们在训练后释放，但**分配器留住了页面** | 第 44 条是"复用空闲堆 ⇒ 低估"；这里是"制造了瞬态 ⇒ 高估"。**同一把尺子，两个方向的误差**。报数字时必须分开说"稳态常驻"与"装载峰值"，并把两者都写进文档（D46 第②条要求的就是这个） |

---

## 6. 现在怎么跑

```bash
cargo build --workspace && cargo test --workspace      # 441 个测试
cargo run -p stele-cli -- --check                      # 8 组内核不变式
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

# ── 内联零件（阶段 A）── 一份把 10 个零件全声明了的测试方案 ──────────
cargo test -p stele-schemes --test inline_components     # 15 条端到端断言
cargo run -p stele-cli --release -- --scheme-dir crates/stele-schemes/tests/schemes-inline rq
cargo run -p stele-cli --release -- --scheme-dir crates/stele-schemes/tests/schemes-inline cC1+2
cargo run -p stele-cli --release -- --scheme-dir crates/stele-schemes/tests/schemes-inline U62fc
cargo run -p stele-cli --release -- --scheme-dir crates/stele-schemes/tests/schemes-inline uuid-test
# 改了装配路径/配置读取之后，**先跑这一条**：它是"零件真的被装配了"的唯一证据

# ── 用户记忆（P4a）── **默认关闭**，给了 `--userdb` 才学、才记 ────────
cargo run -p stele-cli --release -- --scheme-dir schemes/stele-default \
    --userdb /tmp/u.mem --select=3 shi        # 把第 3 个候选上屏并记住它
cargo run -p stele-cli --release -- --userdb /tmp/u.mem --dump-memory   # 看记住了什么
cargo run -p stele-cli --release -- --scheme-dir schemes/stele-default \
    --userdb /tmp/u.mem --candidates=4 shi    # **顺序变了**：学过的那个上来了
# 不加 `--userdb` 时行为逐字节可复现（这是产品决定，不是省事）

# ── 下一词预测（P4b）── **默认关**，要 `--userdb` + `--predict` 两个一起给 ──
cargo test -p stele-memory --test predict_next       # 对比集 15 条端到端（验收线）
cargo test -p stele-memory --test no_disk_io_on_keypath   # 用户记忆按键路径零磁盘 I/O（含预测；词典查询仍有 read_at）
cargo run -p stele-cli --release -- --scheme-dir schemes/stele-default \
    --userdb /tmp/u.mem --predict --commit-seq=jintian,tianqi,jintian
    # 连续上屏「今天 天气 今天」→ 最后一行 `[预测] 接下来可能打：天气`
    # **为什么必须 `--commit-seq`**：上下文是会话状态，一次调用只上屏一次，
    # 因此"今天 → 天气"这种跨两次上屏的搭配在单次调用里根本产生不了。
cargo run -p stele-cli --release -- --userdb /tmp/u.mem --dump-memory
    # 两张表：上面是编码键的输入表，下面是**上下文键的预测表**
# 量预测的内存成本（斜率法；直接读小 N 的 RSS 增量会低估，见 §5 第 44 条）：
cargo run -p stele-bench --release -- --scheme-dir schemes/stele-default \
    --schema=pinyin --userdb /tmp/m.mem --predict-cap=300000 --seed-predict=100000

# ── 本地向量偏好记忆（P5 · D46）── **默认关**，要 `--userdb` + `--embed` ──
cargo test -p stele-embed                       # 单元测试（15 条）
cargo test -p stele-embed --test context_cases -- --nocapture
    # ↑ A/B 对照：会打印每条用例在「基线」与「加向量」下的名次
cargo run -p stele-bench --release -- --scheme-dir schemes/stele-default \
    --schema=pinyin --userdb /tmp/e.mem --seed-memory=30000    # 先灌输入记忆
cargo run -p stele-bench --release -- --scheme-dir schemes/stele-default \
    --schema=pinyin --userdb /tmp/e.mem --seed-predict=20000   # 再灌上下文历史（会落盘）
cargo run -p stele-bench --release -- --scheme-dir schemes/stele-default \
    --schema=pinyin --userdb /tmp/e.mem --embed                # 报告里打出向量表大小与加成上限
# 设计、内存账与实测：docs/embed-design.md

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
cargo run -p stele-bench --release -- --scheme-dir schemes/stele-default \
    --schema=pinyin --count-queries                    # 每键词典查询次数（PLAN §6 的欠账）
cargo run -p stele-bench --release -- --iterations=2000 \
    --userdb /tmp/m.mem --seed-memory=30000            # 记忆的内存成本（验收 4）
bash scripts/verify-*.sh                               # 四条门禁（含依赖许可审查）

# ── 与 librime / 上游 Lua 对照 ─────────────────────────────────────
python3 tools/compare-librime.py                       # 结构对照（6 条）
cd tools/librime-probe && ./build.sh && ./probe --help  # 驱动真实 librime
cargo test -p stele-engine --test number_oracle         # 与上游输出记录逐字节对照
cargo test -p stele-engine --test calc_oracle           #   （42 + 74 条）
# 对照数据是上游程序跑出来的 .expected.txt 存档；
# **上游 Lua 源码已不随仓库分发**（GPL-3.0-only，审计 J2.3）——
# 需要重新生成时的取回配方见 tools/oracle/README.md
cargo run -p stele-cli -- --scheme-dir <目录> --list    # 装载自建方案
```

**环境事实**：WSL2，仓库在 ext4（`/home/brennmond/projects/stele`），
rustup 已装、toolchain 1.98 由 `rust-toolchain.toml` 固定。
`librime-bin 1.16.1` 已装（`rime_deployer` 可用）。

---

## 7. 下一步建议

**P4a / P4b 均已完成**（见 §4）——用户记忆（频率 + 时间衰减）与本地下一词预测
（`Lane::Predict`）都已落地，**用户记忆这条路径**仍零磁盘 I/O、延迟无可测变化
（词库查询仍走 `read_at`）；实测数字在 §0，对比集在 `tools/predict/`。

**阶段 A 的装配缺口已经关掉**（§5 第 36、38 条）：10 个内联零件**全部装配**、
配置可读、15 条端到端断言守着；"零件名有没有装配分支"现在有可执行的答案
（`scheme::assembles()`）。过程中牵出的另外三处静默缺陷也一并修了（§5 第 38–40 条）。

**P4b 的执行计划在 §7.7**——它列的四件事都已由所有者拍板，结论与差异
记在 **§4 的「P4b 已完成」**（② 选了 trigram 并补了 bigram 回退；
④ 与建议一致；默认关；内存 20 000 条 ≈ 4.0 MiB）。

**P5 的第一版已落地（默认关）**：评审在 `docs/p5-vector-feasibility.md`，
**执行书与实测在 `docs/embed-design.md`**。当前形态是**本地向量偏好记忆**：
`stele-embed`（零依赖、无模型、无网络）把本地历史里的 `(上下文 → 下一个词)`
计数投影成 `i16` 向量，接在 `Lane::Input` 的重排链上；**默认关闭**（D46）。

- 实测：对比集 **3/3**（基线 1/3，**无回归**）；向量表 **2.44 MiB @ 4 万词**；
  装载峰值约 +9 MiB；按键延迟**无可测变化**。
- **收益证据还很小**（3 条自造用例）——只证明机制成立，**不证明**值得默认开。
- **最大的下一步是所有者手写一份更大的对比集**（`tools/embed/context-cases.tsv`
  的形状），因为这条纪律是整个项目的核心：**没有对比集，"更好"就是一句无法证伪的话**。
- 观察到的两个技术缺口：**没有两步共现**（没见过的搭配不泛化）、
  **没有子词回退**（没见过的上下文词没有向量）。见 `docs/embed-design.md` §5。

其余候选（**不按此顺序自动开始**，除非所有者指定）：

1. **recognizer 的三处语义分叉**——记在
   `reference/rime-recognizer-and-affix.md` 的差异表里。前两处
   （锚死位置 0、`$` 启发式）会影响真实方案，值得对齐。
2. **阶段 C：会话语义改造**（解锁 `select_character` + 拆字辅码）。
3. **`select`（切方案）**：需要把 `SchemaCatalog` 送到处理器手里。
4. **P5 的收益对比集**（需要所有者手写；见上）。
5. 再往后是 P6/P7（Windows TSF / Android）。

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

`tools/oracle/*.lua`（从 rime-ice 复制/改写的纯计算副本）**已在阶段 4 移除**
（审计 J2.3）：把它们与 MIT/Apache 作品一起分发会把整份分发拖入 GPL。
保留的是 `.expected.txt` 输出记录（事实，不是代码），对照测试照常工作；
需要重新生成时的上游 URL + 固定 revision 留在 `tools/oracle/README.md`。
逐项许可与版权见 `THIRD_PARTY_NOTICES.md`。

---

## 7.6 下一步执行计划：**P4a 用户记忆**（已定稿，照此执行）

> ## ✅ 本节已执行完毕（2026-09）——交付与验收见 §4 的「P4a 已完成」
>
> **四件必须先定的事，结论如下**（过程留在这里，因为"当初为什么这么定"
> 比结论本身更容易被忘记）：
>
> | 待定 | 结论 | 与本节建议的差异 |
> | --- | --- | --- |
> | ① 加成量纲 | `bonus = MAX × f / (f + H)`，`f` 是**整数次减半**的衰减退频；半衰期 30 天；上界 14 000 毫对数 | 采纳，但**把浮点也拿掉了**：连"更新时用浮点"都不需要，于是"跨平台逐位一致"是类型层面的事实 |
> | ② G10 策略 | 第一版按"规范化拼写"作键（D41）；**所有者认为跨拼法共享很重要，于是改成了规范编码作键（D42）** | 最终结论与上游一致：编码作键 ⇒ `nhao` 学的帮到 `nihao`。实现方式不同（键随候选带出，而不是在记忆层反查） |
> | ③ 内存上限 | **30 000 条**（实测反推） | **否掉了本节建议的 100 000**：实测 +18.2 MiB，叠上词库越过 30 MB 红线 |
> | ④ 依赖选型 | **B1 自写排序 KV**（零依赖、无 unsafe） | 采纳。第 0 步的依赖许可门禁也补上了（`scripts/verify-deps.sh`，已反向验证三种违规） |
>
> 另外**顺手修掉两个一直在那儿的自检失效**（§5 的第 30、31 条）——
> 它们是执行"第 5 步：称重与验收"时撞出来的，不是计划内的活。
>
> 下面保留原文，作为"当时是怎么想的"的记录。

> 这一节是**给下一个会话的执行书**：它把路线图上的"P4a"拆到可以直接动手的粒度，
> 并把"动手前必须先定的四件事"写清楚。
> **先读完本节，再读 `docs/engine-design.md` §4 与 `reference/dsh-plugin-architecture-lessons.md`。**
>
> **一动代码之前先做一件事**：跑一遍基线并记下来——
> `cargo test --workspace`、`bash scripts/verify-*.sh`、
> `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --check`。
> **动手前**的基线是：360 个测试全过、clippy 零警告、三条门禁全过。
> **P4a 完成后**是 441 个测试、四条门禁（新增依赖许可审查）。

### 7.6.0 目标与验收（PLAN §3 的 P4a 行）

| | |
| --- | --- |
| **目标** | 频率 + 时间衰减的用户记忆 |
| **交付物** | 新 crate `stele-memory`（`MemoryStore` 的实现）+ 引擎侧接线 + CLI 开关 |
| **验收 1** | **打过的词下次优先**：同一个词连续上屏过 N 次后，它排到同码候选之前 |
| **验收 2** | **用户记忆的按键路径零磁盘 I/O**（红线）：这条路径上一次 `read`/`write` 系统调用都不能有（词典查询 `TableLexicon` 不在范围内，它按需 `read_at`） |
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
| 2 | **在按键路径上落盘** | 红线是**用户记忆这条路径**零磁盘 I/O（词库查询仍 `read_at`）；RIME 的 P99 36 ms 最大单点瓶颈据称是 LevelDB 的磁盘读 |
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

## 7.7 执行计划（**已执行**）：**P4b 本地下一词预测**

> ## ✅ 本节已执行完毕——交付与验收见 §4 的「P4b 已完成」
>
> **四件必须先定的事，结论如下**（过程留在这里，因为"当初为什么这么定"
> 比结论本身更容易被忘记）：
>
> | 待定 | 结论 | 与本节建议的差异 |
> | --- | --- | --- |
> | ① 对比集 | **自建搭配语料 + 存档表**（`tools/predict/collocations.tsv`，15 条），断言"学过后期望词进入预测**前 1 位**" | 采纳。基线那一步（没学过时不该预测出来）也做了 |
> | ② context 粒度 | **trigram**，并补 **bigram 回退** | **与建议不同**：所有者选了 trigram。回退是落地时补的——只用 trigram 时新用户前几周几乎预测不出东西（PLAN D45） |
> | ③ 内存预算 | **20 000 条 ≈ 4.0 MiB**（斜率法实测 ≈200 字节/条） | 采纳。<5 MB 成立，但**测法**要小心：直接读小 N 的 RSS 增量会低估到 1/60（§5 第 44 条） |
> | ④ 通用搭配表 | **只留接口 + 本地自建** | 采纳。`PredictionOrigin::General` 留着但没有生产者，因此**没有**给 `Candidate` 加来源字段（没有消费者） |
>
> 落地时**又发现两处静默缺陷**（事件丢上下文、`record` 按 lane 二选一）
> 和一处陈年 panic（`Context::default()` 容量 0），见 §5 第 41–43 条。
>
> 下面保留原文，作为"当时是怎么想的"的记录。

> 这一节是给下一个会话的执行书。与 §7.6 一样，它把路线图上的 "P4b"
> 拆到可以动手的粒度，并**先列出必须由所有者拍板的四件事**。
>
> **一动代码之前先跑一遍基线并记下来**（**执行前**的基线：`cargo test --workspace`
> **441 个**全过、clippy 零警告、**四条**门禁全过、`cargo fmt --check` 干净、
> `stele --check` 8 组不变式通过。**执行后**：**467 个**测试，其余同）。

### 7.7.0 目标与验收（PLAN §3 的 P4b 行，原文）

| | |
| --- | --- |
| **目标** | **本地下一词预测**（`Lane::Predict`） |
| **交付物** | 个人 n-gram 优先，通用预测表次之 |
| **验收** | 常见搭配排序改善（**需定义对比集**）；内存增量 **< 5 MB**；神经模型**撞红线，暂缓** |

### 7.7.1 P4a 与阶段 A 已经替它铺好的（**不用重做**）

- `MemoryStore::predict_next(&Context)` **已在 trait 上**，现在明确返回空；
- `Commit.lane` 的**分流规则**已定：`Lane::Input` 按编码键、`Lane::Predict` 按上下文；
- `Lane::Predict` 的**排序规则**（宽松、可混排）、**不参与盲选**、
  `SelectionSource::Pointer` 这条执行点，都定在 `docs/engine-design.md` §4.3；
- `Query::context` / `QueryView.context`（最近已上屏词的滚动窗口）已就位；
- **装配路径刚刚理顺**：`Services` 能注入任意服务，`assembles()` 保证
  "声明了就真的装配"，`Pipeline::component_counts()` 让"装进来了"可断言；
- **排序位置已定**（D43）：预测候选的插入发生在滤镜之前还是之后，
  要看它是否参与"按位置"的语义——**动手前先想清楚这一条**；
- 记忆层的**成本纪律**可复用：实测单条占用 → 反推上限（见
  `stele_memory::DEFAULT_CAPACITY` 的文档与 `stele-bench --seed-memory`）。

### 7.7.2 动手前必须定掉的四件事（**不先定，事后就是全量返工**）

#### ① 对比集（**验收原文就要求它，也是这一阶段最容易糊弄过去的一条**）

"常见搭配排序改善"没有对比集就是一句无法证伪的话。**建议**：先在 CLI 上
跑一批真实上下文，把 `(上一个词 → 期望的下一个词)` 写成**存档表**
（做法照抄 `tools/oracle/*.expected.txt`：输出是事实、可重新跑一遍），
断言"期望的词出现在预测候选的前 N 位"。**没有这张表就先不写代码。**

#### ② context 的粒度与长度

上一个词？上两个词？`Context` 现在保留 8 个词。**建议先做 bigram
（只看上一个词）**：数据稠密、收益明确、实现最小；trigram 留给之后。
`engine-design` §4.3.1 只说"按 context 记录"，没说几个词——**这是要定的**。

#### ③ 内存预算怎么分（验收写 < 5 MB）

P4a 的记忆已经占了约 4–6 MiB（3 万条上限），**红线剩下的余量要一起算总账**。
**建议**：上限由实测反推（`stele-bench --seed-memory=N` 的同款做法），
把默认值写进称重台的报告里，照 §7.6.1 ③ 的格式。

#### ④ 通用搭配表的来源与许可（PLAN §10）

**建议只留接口 + 本地自建**，通用表走"用户在部署时自取"那条既有路径——
P3.5 已经证明过：许可不明的数据（雾凇那两块）比 GPL 更难处理。

> **上面四条都需要所有者确认**。① 与 ④ 尤其：一个是验收线，
> 一个是数据边界，两者都不该由实施者单方面决定。

### 7.7.3 执行步骤（建议按此顺序，每步单独可验证）

0. **先补对比集**（它是验收线，不是可选项）。
1. **纯函数**：`(上下文, 下一个词) → 定点分数`（含时间衰减）。配边界表测试，
   时钟一律注入（D38）。**别引入浮点**——它影响排序（D13）。
2. **存储**：**不要在 D42 的那张表里混两套键空间**（那边是**编码**，
   这边是**上下文文本**）。要么另开一份文件，要么在同一份文件里分段并写明。
3. **引擎接线**：`predict_next` 在 `compose` 的哪一步跑、预测候选怎么进
   `Session::candidates()` 的已渲染列表（§4.3.2 的插入位置）。
   **注意 D43**：排序在滤镜之前——预测候选的插入点要么在排序之前（参与排序），
   要么在滤镜之后（位置即最终）。**这一条要在动手前想清楚**。
4. **事件接线**：`apply_events` 现在是"事件 → 记忆"的**唯一映射点**，
   `Learned { lane: Predict }` 要走 context 键（P4a 已经按 lane 分流）。
5. **称重与对账**：内存增量、按键延迟、对比集的前后对比。
6. **CLI 开关与文档**：预测默认开还是关？**默认值是产品决定**（照 P4a 的先例，
   倾向于"默认关"）。

### 7.7.4 已知的坑（提前写下来）

| # | 坑 | 为什么 |
| --- | --- | --- |
| 1 | **把预测候选混进 `Lane::Input`** | 会同时破坏"盲选肌肉记忆"与"精确优先"两条（§4.3 已定，别绕开） |
| 2 | **两套键空间混进一张表** | 编码键（D42）与上下文键会互相污染，症状是"有时查到奇怪的东西" |
| 3 | **淘汰规则不确定** | 按容器遍历顺序淘汰 ⇒ 同一份数据两次运行结果不同（与 §5 第 38 条同类的教训） |
| 4 | **忘了接 `Learned` 事件** | 不报错、只是"学了没记住"——P4a 的头号坑，这里会重演 |
| 5 | **在按键路径上落盘** | 红线是**用户记忆路径**零磁盘 I/O（P4a 已有一条 `/proc/self/io` 的证伪测试，**扩展它**；词库查询仍 `read_at`） |
| 6 | **预测候选没进"猜测"标记** | G12：UI 要能区分"你的习惯"与"通用搭配"（`PredictionOrigin` 已预留） |

### 7.7.5 明确**不做**的事

- ❌ 向量重排（P5，**需先过内存预算评审**）。
- ❌ 神经网络模型（PLAN §3 P4b 行明写"**撞红线，暂缓**"）。
- ❌ 顺手改 `recognizer` 的三处语义分叉（那是有源码依据的独立一项，见 §7）。


---

## 7.8 给新会话的操作提醒

- 项目所有者**是初学者**，要求：新名词第一次出现就解释；不要高估基础；
  文档别写成座右铭（要能证伪、能写成断言或测试）。
- 他**明确表示过**：对外只维护**简体**形态；繁体留接口不维护（D32）。
- 他**授权过**：装 rustup。**没有授权**其它工作区外的操作
  （sudo 需密码，不能代劳）。
- **每次动手前后都要实测**：这个项目里"以为对"的记录见 §5（现在有 **45 条**）。
  最近这 16 条（30–45）里有 **9 条是"只有真的跑一遍才会发现"**——
  写一个声明新语法的**测试方案**本身就是一种探针。
- **改动 `Lane::Predict` / 预测表 / `Services.prediction` 之后先跑
  `cargo test -p stele-memory --test predict_next`**：那 15 条对比集是这条能力
  **唯一的端到端证据**（HANDOFF §7.7 的验收线）。
  它的形状值得照抄——**每一行一份独立记忆**，基线那一步不省。
- **量内存成本之前先问"尺子量的是我要的东西吗"**：RSS **增量**在"复用已驻留
  空闲堆"时对你是隐形的（§5 第 44 条：小 N 低估到 1/60）。权威做法是
  突破上限量**斜率**，或与"0 条"那一次的总常驻相减。
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
- **改动装配路径之后先跑 `cargo test -p stele-schemes --test inline_components`**：
  它是"零件真的被装配了"的唯一证据（15 条断言，10 个零件各一条）。
  涉及 `engine:` 清单、`build_pipeline`、`Services`、配置读取时必跑。
- **"说 true 就必须真的有"**：`scheme::assembles()` 是一张**手写的**清单，
  靠 `assembles_agrees_with_what_build_pipeline_actually_builds` 一条测试
  与 `build_pipeline` 对齐。**加零件却忘了改清单，那条测试会红**。
- **不要相信"配置看起来正常"**：P3 抓到的四个 bug 症状完全一样——
  配置合法、没有报错、某个功能就是不生效。只有端到端测试抓得到。
- **不确定的约定，别写成"RIME 约定"**：要么引 librime 源码
  （`reference/` 里有两份现成的），要么写"这是我们的选择"。
