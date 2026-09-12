# Stele-IME（石经）

一个**从原理出发、用 Rust 重写**的跨平台输入法引擎。

> **名称**：全名 **Stele-IME**，简称 **Stele**；中文名 **石经**。
> 「Stele」意为石碑，「石经」取"刻经于石"之典——把经典刻在石碑上。
>
> **文字形态：简体优先。** 本项目只维护简体形态的数据与体验；
> **繁体不在我们适配的责任范围内**，但相关接口与配置项一律保留——
> 需要繁体的人可以自行配置（见 `PLAN.md` D32）。

**当前状态：P2.5 完成。词库编译成紧凑产物、按需分页读取——176 个测试通过。**

```bash
$ stele nihao                    # 拼音方案：规范拼写
你好
$ stele nh                       # 拼音方案：缩写（简拼），分数更低
你好
$ stele --schema shape ab        # 精确编码方案：同一个引擎，完全不同的输入法
十

# 换一套自己的方案与词库 —— 不需要重新编译
$ stele --scheme-dir ./my-schemes --list
$ stele --scheme-dir ./my-schemes mami
猫咪
```

**实测**（release）。50 万词条的词库：

| | 内存实现 | 编译产物（首次） | 编译产物（命中） |
| --- | --- | --- | --- |
| 峰值内存 | 245 MiB | **47 MiB** | **23 MiB** |
| 装载耗时 | 1.27 s | 0.43 s | 0.09 s |

按键路径 P50 **301 ns** / P99 **431 ns**（演示词库，只反映管线开销）。

---

## 为什么做这个

1. 商业闭源输入法**占用巨大**（实测峰值 300–400 MB）、功能冗余、隐私保护不完善。
2. RIME 是唯一可用的替代，但依赖重（Boost + LevelDB + marisa + OpenCC + yaml-cpp + glog），
   构建复杂，关键行为大量寄生在 Lua 插件上。
3. **RIME 从未承诺隐私与离线**——它的设计文档里找不到这样的承诺，
   而作者的开发计划写着「第三期，添加網絡功能」。**这是我们可以立起来的旗帜。**

## 硬指标（不可退让的红线）

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
stele/
├── crates/
│   ├── stele-core/     # 抽象层：Engine/Session、组件 trait、数据结构（零依赖）
│   ├── stele-engine/   # 原生引擎：拼写层、词库、两族翻译器、处理器、过滤器（零依赖）
│   ├── stele-config/   # YAML 子集解析、$ref 跨文件引用、分层补丁、可读诊断
│   ├── stele-dict/     # .dict.yaml（头部 + TSV 正文 + import_tables）
│   ├── stele-table/    # 词库编译产物：紧凑二进制 + 按需分页（零依赖、无 unsafe）
│   ├── stele-schemes/  # 方案装载 + 内嵌默认方案（**与内核分属不同 crate**）
│   ├── stele-cli/      # 命令行调试前端（可执行文件名为 `stele`）
│   └── stele-bench/    # 称重台：内存与延迟测量
├── schemes/            # 方案资产（与内核解耦）
│   └── stele-default/  # 默认方案：pinyin（拼写图族）/ shape（精确编码族）+ 词库
├── docs/
│   └── engine-design.md   # 引擎设计的权威定义
├── reference/          # 调研资料（RIME 官方文档对比、librime 内部机制、前端对接）
└── PLAN.md             # 项目章程与决策记录（ADR）
```

---

## 快速开始

```bash
# 构建与测试
cargo build --workspace
cargo test --workspace

# 打字
cargo run -p stele-cli -- nihao                 # 规范拼写 → 你好
cargo run -p stele-cli -- nh                    # 缩写拼写 → 你好
cargo run -p stele-cli -- --candidates ni       # 看候选（分数 / 来源 / 属性）
cargo run -p stele-cli -- --schema shape ab      # 精确编码方案 → 十
cargo run -p stele-cli -- --list                 # 列出方案

# 装载自己的方案目录（方案与词库都是数据文件，改完不必重编译）
cargo run -p stele-cli -- --scheme-dir ./my-schemes --list
cargo run -p stele-cli -- --scheme-dir ./my-schemes mami

# 内核自检（验证"可复现 / 精确优先 / 通用性"等 7 组不变式）
cargo run -p stele-cli -- --check

# 称重台（用 --release，否则测的是未优化代码）
cargo run -p stele-bench --release -- --iterations=200000
cargo run -p stele-bench --release -- --json    # 便于 CI 记录历史
```

**注意**：`--dump-config` 目前明确报告未实现（配置分层的机制已在 `stele-config` 里
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
完整格式见 `schemes/stele-default/` 里的两份示例，以及 `docs/engine-design.md` §6。

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

- `PLAN.md` — 决策记录（D1–D31）、路线图、工程铁律
- `docs/engine-design.md` — 引擎抽象与数据结构的权威定义
- `reference/` — RIME 官方设计文档的逐条对比（含"我们漏掉了什么"的诚实清单）

## 许可证

MIT OR Apache-2.0（见 `LICENSE-MIT` / `LICENSE-APACHE`）。

**本仓库不包含第三方词典数据或模型**：rime-ice 为 GPL-3.0，
且其内部词源混合了多种限制性许可。方案与数据由使用者自行获取并在本地编译。
理由与逐项许可清单见 `PLAN.md` §10。
