# 青简输入法 / Qingjian IME

一个**从原理出发、用 Rust 重写**的跨平台输入法引擎。

> **名称**：中文正式名 **青简输入法**；英文正式名 **Qingjian IME**；简称 **青简** / **Qingjian**。
> 「青简」本身即完整、统一的专有名词，**不意译**为 Bamboo Slips 一类的英文词——
> 保留原语言名称既点明中文根源，也避免意译把文化意象压扁成普通英文词。
> 拼写连写、首字母大写（不写 `Qing Jian`，也不写 `QingJian`）。
>
> **项目沿革**：青简前身为 **Stele-IME**（*Qingjian was formerly developed under the name Stele*）。
>
> **文字形态：简体优先。** 本项目只维护简体形态的数据与体验；
> **繁体不在我们适配的责任范围内**，但相关接口与配置项一律保留——
> 需要繁体的人可以自行配置（见 `PLAN.md` D32）。

**当前状态：原型内核（研究阶段）。** 已有分层引擎、两族翻译器、方案装载、
紧凑词库、可选本地记忆，以及一批可重跑的对照工装与回归测试。
测试数量请以 `cargo test --workspace` 的**实际输出**为准——
写死在 README 里的数字会过期（审计 §1.1 已指出旧的"208 tests"过时）。

**它不是可以替代日常中文输入法的前端产品，也不是完整的 Rime 实现**：
上游 preset 方案目前**不能直接装载**（13 个样本全部失败，审计 §2.G1），
已实现的是 **"Rime 风格的受限子集"**。对外定位见审计 §3.3。

> 新会话请先读 [`docs/HANDOFF.md`](docs/HANDOFF.md)（一页交接）；
> 第三方数据与许可见 [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md)，
> 隐私边界见 [`docs/privacy-model.md`](docs/privacy-model.md)。

```bash
$ qingjian nihao                    # 拼音方案：规范拼写
你好
$ qingjian nh                       # 拼音方案：缩写（简拼），分数更低
你好
$ qingjian --schema shape ab        # 精确编码方案：同一个引擎，完全不同的输入法
十

# 换一套自己的方案与词库 —— 不需要重新编译
$ qingjian --scheme-dir ./my-schemes --list
$ qingjian --scheme-dir ./my-schemes mami
猫咪
```

**实测**（release）。**每个数字都有适用条件，不是"普遍达标"**：

| 场景 | 词库 | P50 | P99 | 常驻内存 | 出处 |
| --- | --- | --- | --- | --- | --- |
| 演示词库（几十条） | 内嵌演示 | 301 ns | 431 ns | 4 MiB | PLAN §9 基线表 |
| 真实词库 41 万条 | `cn_dicts/generated` | 约 50 µs（冷页缓存）／约 15 µs（热） | 约 110 µs | 13.6 MiB | `docs/HANDOFF.md` §0 |

> 这两行只对**该机器、该词库、`nihao` 一类的短正常输入**成立。
> 审计 §1.3 在同一构建、同一机器上复现过：`ssss` 的第 4 键约 1.52 s、
> 进程峰值约 209 MiB；`woaizhongguo` 输入期间出现 290/679/285 ms 的按键。
> **短输入的漂亮数字不能代表全部打字性能**；相关修复与验收状态见
> `docs/validation/`。

---

## 为什么做这个

1. 商业闭源输入法**占用巨大**（实测峰值 300–400 MB）、功能冗余。
2. RIME（librime）是成熟的可替代实现，但依赖重（Boost + LevelDB + marisa +
   OpenCC + yaml-cpp + glog）、构建复杂；**部分方案**把关键行为放进 Lua 插件。
   **注意**：基础 `script_translator` 的造句与补全**不需要** Lua 或神经模型，
   重度 Lua 方案的性能不能代表 librime 内核本身（审计 §3.1）。
3. **我们想把"离线 + 隐私"做成一条明确、可核对、写进文档与测试的承诺。**
   这是要补的一课，不是对 Rime 的贬低：librime 本身是 BSD-3-Clause，
   而"Rime 是否联网、是否保护隐私"取决于**具体前端、部署、插件与用户配置**，
   **不能**由"官方文档里没写隐私承诺"推断出来（审计 §3.1 明确反对这种推论）。

## 硬指标（**目标**，不是已验收的现状）

右列的"RIME 实测基线"来自 `reference/rime-frontend-research.md` 引用的公开实测；
左列是本项目**尚未全部验收**的目标（审计 §1.3 / §6.3）。

| 指标 | 目标 | RIME 实测基线 |
| --- | --- | --- |
| 打字时常驻内存（单方案） | < 30 MB | 约 20 MB（Squirrel） |
| 部署峰值内存 | < 150 MB | 780 MB – 1 GB |
| 按键延迟 P50 | < 1 ms | 0.48 ms（重度 Lua 方案） |
| 按键延迟 P99 | < 10 ms | 36.4 ms（重度 Lua 方案） |
| 冷启动到可打字 | < 100 ms | 未公开 |
| 编译产物 / 源 YAML 体积比 | < 3× | 约 14× |

---

## 仓库结构

```
qingjian/
├── crates/
│   ├── qingjian-core/     # 抽象层：Engine/Session、组件 trait、数据结构（零依赖）
│   ├── qingjian-engine/   # 原生引擎：拼写代数（含自写正则）、词库、两族翻译器、处理器、过滤器（零依赖）
│   ├── qingjian-config/   # YAML 子集解析、$ref 跨文件引用、分层补丁、可读诊断
│   ├── qingjian-dict/     # .dict.yaml（头部 + TSV 正文 + import_tables）
│   ├── qingjian-table/    # 词库编译产物：紧凑二进制 + 按需分页（零依赖、无 unsafe）
│   ├── qingjian-schemes/  # 方案装载 + 内嵌默认方案（**与内核分属不同 crate**）
│   ├── qingjian-cli/      # 命令行调试前端（可执行文件名为 `qingjian`）
│   └── qingjian-bench/    # 称重台：内存与延迟测量
├── schemes/            # 方案资产（与内核解耦）
│   └── qingjian-default/  # 默认方案：pinyin（拼写图族）/ shape（精确编码族）+ 词库
│       └── cn_dicts/generated.dict.yaml  # **生成的默认词库**（第三方 MIT/Apache 数据的派生物）
├── tools/              # 部署期工装（词库生成、上游对照、探针）
│   ├── sources.lock    # 源数据的固定 revision + sha256（已跟踪的可复现输入）
│   └── fetch-sources.sh
├── licenses/           # 第三方许可证文本（MIT × 3、BSD-3-Clause）
├── docs/
│   ├── engine-design.md   # 引擎设计的权威定义
│   └── privacy-model.md   # 隐私边界（数据分类、权限、禁学、清除）
├── reference/          # 调研资料（RIME 官方文档对比、librime 内部机制、前端对接）
├── THIRD_PARTY_NOTICES.md  # 第三方来源 / 版权 / 许可逐项清单
└── PLAN.md             # 项目章程与决策记录（ADR）
```

---

## 快速开始

```bash
# 构建与测试
cargo build --workspace
cargo test --workspace

# 打字
cargo run -p qingjian-cli -- nihao                 # 规范拼写 → 你好
cargo run -p qingjian-cli -- nh                    # 缩写拼写 → 你好
cargo run -p qingjian-cli -- --candidates ni       # 看候选（分数 / 来源 / 属性）
cargo run -p qingjian-cli -- --schema shape ab      # 精确编码方案 → 十
cargo run -p qingjian-cli -- --list                 # 列出方案

# 装载自己的方案目录（方案与词库都是数据文件，改完不必重编译）
cargo run -p qingjian-cli -- --scheme-dir ./my-schemes --list
cargo run -p qingjian-cli -- --scheme-dir ./my-schemes mami

# 内核自检（验证"可复现 / 精确优先 / 通用性"等 7 组不变式）
cargo run -p qingjian-cli -- --check

# 称重台（用 --release，否则测的是未优化代码）
cargo run -p qingjian-bench --release -- --iterations=200000
cargo run -p qingjian-bench --release -- --json    # 便于 CI 记录历史
```

**注意**：`--dump-config` 目前明确报告未实现（配置分层的机制已在 `qingjian-config` 里
实现并有测试，但还没接到 CLI 上）。**宁可报未实现，也不打印一份假的配置**——
一个会骗人的调试工具比没有更糟。

## 写自己的方案

方案与词库都是数据文件，放在一个目录里用 `--scheme-dir` 指过去即可：

```
my-schemes/
├── demo.schema.yaml     # 方案：字母表、规则、用哪族翻译器
└── demo.dict.yaml       # 词库：词 <TAB> 编码 <TAB> 权重
```

配置写错时会**一次报出全部问题，每条带行号**——而不是静默忽略其中一个字段。
完整格式见 `schemes/qingjian-default/` 里的两份示例，以及 `docs/engine-design.md` §6。

---

## 设计原则（详见 `PLAN.md` 与 `docs/engine-design.md`）

1. **内核与方案分离**：内核只提供机制（编码 / 拼写 / 编码单元 / 字母表），
   一切输入法的个性都属于**方案数据**。内核里不允许出现"拼音 / 音节 / 简拼"。
2. **可复现**：候选列表是 (输入, 状态) 的纯函数。分数用**定点整数**表示，
   因为浮点加法不满足结合律，跨平台（Windows x86-64 / Android ARM）会漂移。
3. **候选封闭**：候选文本必须来自可枚举来源，禁止自由文本生成
   （输入法是特权组件，自由生成等于开一条数据外泄通道）。
4. **简体优先，繁体只留接口**：转换能力（`simplifier` 一类的组件与开关）
   保留并可配置，但**项目不维护繁体数据、不做繁体适配**。
5. **精确优先**：重排器不得把"猜出来的"候选顶到"不是猜的"候选之前。
6. **配置错误绝不阻止启动**：输入法的失败是**自锁**的——拒绝启动会让用户
   连"打字去改配置"都做不到。
7. **两族翻译器**：编码集合**可枚举**的方案（拼音）走拼写图；
   **不可枚举**的方案（仓颉、五笔）走精确编码。**引擎不预设谁用哪族**，
   且从 P1 起就有测试同时覆盖两者——通用性不是声明，是被测试保护的。

---

## 参与开发前请读

- `PLAN.md` — 决策记录、路线图、工程铁律
- `docs/engine-design.md` — 引擎抽象与数据结构的权威定义
- `reference/` — RIME 官方设计文档的逐条对比（含"我们漏掉了什么"的诚实清单）
- `docs/privacy-model.md` — 隐私模型（哪些是代码保证的、哪些依赖前端/OS）
- `THIRD_PARTY_NOTICES.md` — 第三方来源、固定 revision、版权与许可

## 许可证

本项目自身：**MIT OR Apache-2.0**（见 `LICENSE-MIT` / `LICENSE-APACHE`）。

**本仓库分发第三方内容，不是"没有"**（旧版本此处写"不包含第三方词典数据"
与事实不符）：

- `schemes/qingjian-default/cn_dicts/generated.dict.yaml` 是 pinyin-data /
  THUOCL / jieba（MIT）与 OpenCC（Apache-2.0）的**派生产物**；
- `tools/librime-probe/probe.c` 含逐字段抄自 librime（BSD-3-Clause）的 ABI 声明；
- `reference/wiki-*.md` 是 Rime 官方 wiki 两页的**逐字副本**
  （许可状态未确定，见 `THIRD_PARTY_NOTICES.md` §5.3）。

**本仓库不分发** rime-ice 的词典（GPL-3.0-only，且内部词源含限制性许可）、
librime 与 Rime wiki 的源码副本。运行时源数据由使用者用
`tools/fetch-sources.sh` 在**自己机器上**取回，落在 `.gitignore` 的 `build/`。

**逐项的 artifact / 上游 URL / 固定 revision / 版权 / 许可 / 是否修改**，
以及 MIT / Apache-2.0 / BSD-3-Clause 的许可文本，见
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md) 与 `licenses/`。
理由见 `PLAN.md` §10。
