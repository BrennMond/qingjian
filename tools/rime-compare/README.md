# rime-compare —— 与上游 RIME 的对比测试

## 这是什么

以 **librime**（引擎）与 **plum**（方案/配方管理器）两个上游为准，
把 Qingjian 与**真实 RIME** 放在同一套定义下对照，产出一份可复现的报告：

```
python3 tools/rime-compare/compare.py     # → tools/rime-compare/report.md
```

它与 `tools/compare-librime.py` 的关系：**那份是 P3 的验收线（6 条结构用例），
这份把它当作维度 A 原样调用**，再补上三个新维度。同一批结构用例只有一份定义。

## 为什么要有它

P3 的对照报告结尾写着一句话：

> 这份报告**不断言候选排序一致**，因为两边的词库不同——
> 比排序比的是词库，不是引擎。要比排序，得先让两边吃同一份词表。

**这份工装就是来兑现那句话的。** 维度 B 把**同一份** `.dict.yaml` 同时喂给
librime 与 Qingjian（`fixtures/shared.dict.yaml`），于是"排序不同"再也不能
用"词库不同"解释——它只能来自引擎。

## 四个维度

| 维度 | 对照对象 | 断言什么 | 出处 |
| --- | --- | --- | --- |
| **A 结构行为** | 真实 librime（系统运行库）vs qingjian | 能否上屏 / 按键是否被处理 / 全角标点 | `tools/compare-librime.py`（原样调用） |
| **B 同一份词表下的排序** | 两边读**同一份**词表 | 同码候选必须都按词库权重降序；简拼都要命中 | `fixtures/` |
| **C 上游方案能否装载** | `/usr/share/rime-data`（plum preset 的部署产物） | 每个上游方案能否装载；不能的原因归到哪一类 | librime 仓库 + plum preset |
| **D 配方覆盖** | plum 的 `preset-packages.conf` / `extra-packages.conf` | 每个配方在本机的部署情况与 Qingjian 的装载结果 | plum 仓库 |

### B3.1：把"解释"变成"实验"

维度 B 里有一节值得单独说：`B3.1` 是**缩写 × 补全 的 2×2 矩阵**——
同一份词表、同一个输入，只改两个开关，四个格子分别看谁还能出候选。

它存在的理由不是为了多一张表，而是**它已经推翻过一次结论**：
"输入不完整也能出候选"的第一版解释是"Qingjian 只有缩写这一条通路"，
跑完矩阵才发现 Qingjian **四格全空**——缺的是两处（没有"只消费前缀"、
补全在编码单元层），而不是一处。

根因分析（含源码行号与上游对照）见
[`reference/rime-sentence-and-completion.md`](../../reference/rime-sentence-and-completion.md)。

### 断言与观察是两件事

- **断言**必须成立，不成立则退出码非 0：A 的结构用例、B1 的排序不变式、
  B2 的缩写命中。
- **观察**如实记录、**不影响退出码**：B3 / B3.1（前缀、造句、补全）、
  B4（配置项效力）、C（装载缺口）、D（配方覆盖）。

理由：把"发现分歧"直接判成失败，工具会因为"我们有意与上游不同"（D14/D20/D24）
而永远红着——那时就没人看它了。**分歧是待办，不是测试红。**

## 前置条件

```bash
cargo build --release -p qingjian-cli          # target/release/qingjian
cd tools/librime-probe && ./build.sh        # tools/librime-probe/probe
```

- 本机需要有 librime 运行库（`librime.so.1`）与它的方案数据
  （`/usr/share/rime-data`，即 plum preset 包的部署产物）。
  探针经 `dlopen` 调用，**不需要** `librime-dev`。
- 维度 D 需要 plum 仓库副本（默认 `.work/upstream/plum`）。
  **缺失时 D 会被跳过并注明**——不猜一份配方表出来。

取回上游副本（只为读配方表与引用源码行号；`.work/` 已被 gitignore）：

```bash
mkdir -p .work/upstream && cd .work/upstream
git clone --depth 1 https://github.com/rime/librime.git
git clone --depth 1 https://github.com/rime/plum.git
```

librime 的源码副本**不参与构建**：实际的按键行为来自系统装的运行库
（`librime.so.1`，本机实测 1.16.1），副本只用来看行号；plum 副本只用来读
`preset-packages.conf` / `extra-packages.conf`。

常用开关：

```
--probe PATH        探针路径（默认 tools/librime-probe/probe）
--qingjian PATH        qingjian 可执行文件（默认 target/release/qingjian）
--rime-data DIR     上游方案目录（默认 /usr/share/rime-data）
--plum DIR          plum 副本（默认 .work/upstream/plum）
--work DIR          中间产物目录（默认 .work/rime-compare，已被 gitignore）
--out FILE          报告路径（默认 tools/rime-compare/report.md）
--skip-structural   跳过维度 A（它每条用例都要重新部署，较慢）
```

## fixtures/：两边必须真的等价

```
fixtures/
├── shared.dict.yaml          # 共用的词表（RIME 的 .dict.yaml 格式）
├── rime/
│   ├── default.yaml          # 最小 librime 共享目录配置（自包含，不引用 rime-prelude）
│   └── qingjian-cmp.schema.yaml # librime 侧方案（RIME 原生写法）
└── qingjian/
    └── qingjian-cmp.schema.yaml # Qingjian 侧方案（同一份词表、同一批零件）
```

`compare.py` 运行时会把它们铺成两个可直接装载的目录（在 `--work` 下）。
两边的方案刻意都**不挂语言模型**（八股文 `grammar`）、不挂简繁转换与反查——
变量控制：对照要回答的是"同一个词库、同一个输入，两个引擎的排序是否一致"，
多挂一个语言模型，差异就再也说不清是谁造成的。

**加一条排序用例**：在 `shared.dict.yaml` 里加一条带唯一权重的词条，
再往 `compare.py` 的 `ORDER_CASES` 里加一行 `(输入, 说明)`。
期望顺序是**从词表算出来的**，不是写死的。

**加一条分歧观察**：往 `DIVERGENCE_CASES` 加 `(输入, 说明, librime 走的通路)`。

## 已知限制（写在这里，免得下一个人高估它）

- **不比绝对分数**：两边的分值域不同（librime 是对数域的内部权重，
  Qingjian 是定点毫对数），只比**相对顺序**。
- **不比语言模型**：两边都不挂；要比得先有同一份模型。
- **不比性能**：延迟/内存有各自的工装（`qingjian-bench` 与
  `tools/librime-probe` 的实测数字），混在一份报告里会互相稀释。
  需要的是一条**同机、同输入、同预热条件**的专门对照，那是另一件事。
- **D 的配方名 → 方案 id 映射是启发式的**（按词根匹配），
  报告里已注明；没有正式对应表可用。
- **C 只覆盖本机已部署的 preset 方案**；`extra` 组的配方需要先用 plum 取回
  （`rime-install`，要联网），本工装不替使用者做。
- **装载器只报第一个坏方案就会停**（观察 C2），所以"整目录一次装载"得不到
  完整清单；C 节的表是逐方案隔离跑出来的。

## 中间产物

全部落在 `--work`（默认 `.work/rime-compare/`，已被 `.gitignore` 忽略）：
每个上游方案的装载日志（`loadability/<方案>.log`）与铺好的方案目录。
报告里只放归类与首条诊断，原始日志留在那里当证据。
