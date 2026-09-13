# 第三方声明（Third-Party Notices）

> **这份文件回答一个问题**：本仓库里，哪些内容不是我们写的、它从哪来、
> 固定在哪一个 revision、谁拥有版权、适用什么许可、有没有被改过。
>
> **核对日期**：2026-09-13。**核对方法**：不靠记忆，用 `git ls-files` 枚举
> 已跟踪文件，逐条下载上游文件比对 sha256（命令与结果见 §6）。
>
> **结论先行**：
>
> 1. 本仓库**分发**一份派生的默认词库
>    （`schemes/qingjian-default/cn_dicts/generated.dict.yaml`，41 万条），
>    它派生自 pinyin-data / THUOCL / jieba（MIT）与 OpenCC（Apache-2.0）。
> 2. 本仓库**分发** `tools/librime-probe/probe.c`，其中逐字段抄写了
>    librime `rime_api.h`（BSD-3-Clause）的结构体与宏声明。
> 3. 本仓库**分发**两份 Rime 官方 wiki 页面的**逐字副本**
>    （`reference/wiki-*.md`）。**这两份的许可状态未确定**（§5.3）——
>    这是本文件当前唯一没有结论的项，已明确标出，不做粉饰。
> 4. 本仓库**不分发** rime-ice 的词典（GPL-3.0-only），
>    也不分发任何 GPL 代码：旧版 `tools/oracle/*.lua`（从 rime-ice 复制/改写）
>    已在本次整改中**移除**（§5.1）。
> 5. **运行时源数据**（词表、OpenCC 表、emoji 表）由使用者在本地取回，
>    落在 `.gitignore` 的 `schemes/qingjian-default/build/`，**不随仓库分发**；
>    其固定 revision 与 sha256 记录在已跟踪的 [`tools/sources.lock`](tools/sources.lock)。
> 6. 本项目自身许可是 **MIT OR Apache-2.0**（见 `LICENSE-MIT` / `LICENSE-APACHE`）。
>    分发上述第三方派生内容**不会**改变那些内容各自的许可。
>
> **本文不是法律意见。** 它只做事实记录：来源、revision、版权行、许可、
> 是否修改、许可文本在哪。有疑义时以上游原始许可为准。

---

## 0. 已跟踪 / 未跟踪的边界

| 类别 | 是否随仓库分发 | 位置 |
| --- | --- | --- |
| 本项目代码（`crates/`、`tools/` 下的 Rust） | 是 | 仓库 |
| 本项目自撰的方案与演示词库 | 是 | `schemes/qingjian-default/*.dict.yaml`（`base` / `shape`） |
| **由第三方数据派生的生成词库** | **是** | `schemes/qingjian-default/cn_dicts/generated.dict.yaml`（§1.1） |
| 从上游复制的 ABI 声明 / wiki 页面 | **是** | `tools/librime-probe/probe.c`（§1.3）、`reference/wiki-*.md`（§1.4） |
| 原始第三方数据文件（词表 / OpenCC / emoji） | **否**（`.gitignore`） | `schemes/qingjian-default/build/`（§2） |
| 上游源码副本（librime、rime-ice、Rime wiki） | 否 | `.rime-wiki/`、`.rime-research/`（研究用，不入库） |
| registry 第三方依赖 | **无** | `Cargo.lock` 只含 workspace 成员；见 §4 |

`git ls-files` 在整改前（提交 `ad936c2`）共 181 个已跟踪文件；
本文件逐项覆盖其中所有第三方来源或派生内容。整改新增的文件
（`licenses/`、`tools/sources.lock`、本文等）与移除的文件在 §5 / §6 说明。

---

## 1. 随仓库分发的第三方内容

### 1.1 `schemes/qingjian-default/cn_dicts/generated.dict.yaml`（**派生产物**）

**是什么**：414,525 条（414 单字 + 414,111 词）的默认拼音词库，
头部注释写明"生成产物，不要手改"。

**怎么来的**：`tools/wordlist-gen`（本项目自写）读取 §2 的锁定的源数据，
做以下**修改性转换**：去声调、多音字单字表启发式选音、按 `TSCharacters.txt`
过滤繁体词条、去重、生成 `speller.alphabet`。转换脚本与参数在
`tools/wordlist-gen/src/main.rs`（`ReadingPolicy::CorpusFrequent`）。

**是否修改**：**是**——是转换与再编排的产物，不是上游文件的副本。

**上游（四个）**：

| 上游 | URL | 固定 revision | 版权 | 许可 | 许可文本 |
| --- | --- | --- | --- | --- | --- |
| mozillazg/pinyin-data | <https://github.com/mozillazg/pinyin-data> | `923b108dc5d45dee061324c011b478fb649f8b73` | Copyright (c) 2016 mozillazg | MIT | [`licenses/MIT-pinyin-data.txt`](licenses/MIT-pinyin-data.txt) |
| thunlp/THUOCL | <https://github.com/thunlp/THUOCL> | `a30ce79d895d01ab5132a5c74c29703ff7efb4cc` | Copyright (c) 2018 THUNLP | MIT | [`licenses/MIT-THUOCL.txt`](licenses/MIT-THUOCL.txt) |
| fxsjy/jieba（`extra_dict/dict.txt.big`） | <https://github.com/fxsjy/jieba> | `67fa2e36e72f69d9134b8a1037b83fbb070b9775` | Copyright (c) 2013 Sun Junyi | MIT | [`licenses/MIT-jieba.txt`](licenses/MIT-jieba.txt) |
| BYVoid/OpenCC（`TSCharacters.txt`） | <https://github.com/BYVoid/OpenCC> | `c363a7ba51d487950982bd8a589211ffbfd95ba1` | 未署名（文件头声明 `License: Apache-2.0`） | Apache-2.0 | [`LICENSE-APACHE`](LICENSE-APACHE) |

**可复现性（实测，不是声明）**：用上述四个 revision 的输入重新运行生成器，
产出的词库**正文 414,525 条逐字节一致**，`pinyin.schema.yaml` 的音节表
**零差异**（只有生成器头部注释 3 行因本次整改的文字订正而不同）。
命令与输出见 §6.4。

> **已知缺陷（不影响许可，影响准确性）**：当前**已提交**的
> `generated.dict.yaml` 头部把 `jieba_dict.txt` 标成「THUOCL，MIT」。
> 生成器已修正（`tools/wordlist-gen/src/main.rs`），下一次重新生成即消失。

### 1.2 `schemes/qingjian-default/pinyin.schema.yaml` 的 `speller.alphabet:` 段（**派生子段**）

整个文件是本项目自撰的方案（字母表、规则、翻译器配置）；
其中 `speller.alphabet:` 这一段由 `tools/wordlist-gen` **整段重写**，
内容是生成词库中出现过的音节集合（399 个编码单元），因此它是 §1.1 的
派生物，随词库一起适用上表的许可。文件的其余每一行都是人写的方案决策。
`base.dict.yaml` / `shape.dict.yaml` / `z-pinyin-demo.schema.yaml` 是自造演示数据
（`base.dict.yaml` 头部明确写"几百条规模的演示词库"），**不含**第三方内容。

> **已知缺陷**：`schemes/qingjian-default/opencc.manifest.yaml` 把 `emoji`
> 转换器的 `license` 写成了 `Apache-2.0`。按 §2 的核实，rime-ice 的
> `opencc/emoji.*` 是 **GPL-3.0-only**。该文件需要改（见 §5.2）。

### 1.3 `tools/librime-probe/probe.c`（**部分抄自 librime**）

**是什么**：一个用 `dlopen` 驱动真实 librime 的 C 探针。

**第三方部分**：`probe.c` 里"逐字段抄自 `.scratch/rime_api.h`（librime tag
1.16.1）"的结构体、宏与函数指针表声明——即 librime 的公开 C ABI 声明。
其余（会话驱动、JSON 输出、按键序列）是本项目自写。

**是否修改**：**是**——只抄了所需子集，并加了探针逻辑。

| 上游 | URL | 固定 revision | 版权 | 许可 | 许可文本 |
| --- | --- | --- | --- | --- | --- |
| rime/librime | <https://github.com/rime/librime> | `2479df58cb51480299f94afe53d7b1790ecf0eb1`（源码引用基线） | Copyright (c) 2014, RIME Developers | BSD-3-Clause | [`licenses/BSD-3-Clause-librime.txt`](licenses/BSD-3-Clause-librime.txt) |

BSD-3-Clause 第 1 条要求源码再分发保留版权声明、条件列表与免责声明；
本仓库通过 `licenses/BSD-3-Clause-librime.txt` 与本表保留它们。
**该要求当前只在本文件中被满足，`probe.c` 文件头尚未带这段声明**——
建议在文件头补一行指向本文件（见 §5.4）。

### 1.4 `reference/wiki-RimeWithTheDesign.md`、`reference/wiki-SharedData.md`（**逐字副本**）

**是什么**：Rime 官方 wiki 两页的**逐字节副本**（繁体中文）。

**核实**：与 `rime/home.wiki` 上对应页面逐字节相同（`diff` 无输出，见 §6.3）。
wiki 仓库当时的 HEAD 是 `5bfcf14a7ae127635dff9da1f133cae9a5319607`，
与 `reference/rime-key-binding-actions.md` 记录的本地存档 revision 一致。

| 上游 | URL | 固定 revision | 版权 | 许可 | 是否修改 |
| --- | --- | --- | --- | --- | --- |
| rime/home wiki（`RimeWithTheDesign`、`SharedData`） | <https://github.com/rime/home/wiki> | `5bfcf14a7ae127635dff9da1f133cae9a5319607` | **未在页面或仓库中声明** | **未找到明确许可（UNVERIFIED，见 §5.3）** | 否（逐字） |

### 1.5 `reference/` 下其余文档（**本项目自撰，含上游引文**）

`reference/` 里其余 10 份（`rime-wiki-algebra-and-philosophy.md`、
`librime-internals.md`、`rime-ice-research.md`、`rime-frontend-research.md`、
`rime-key-binding-actions.md`、`rime-recognizer-and-affix.md`、
`rime-sentence-and-completion.md`、`rime-schema-conceptual-model.md`、
`rime-customization-data-model-report.md`、`dsh-plugin-architecture-lessons.md`）
是本项目撰写的调研报告。它们**逐字引用了** librime 源码片段
（BSD-3-Clause，`2479df58…`）与 Rime 官方文档（wiki `5bfcf14a…`），
并为每处引用标注了 `路径:行号`。引用片段的版权仍属上游；
报告本身是本项目作品。**上游引文按"引用/取证"处理，不在此重新授权为 MIT。**

### 1.6 `tools/oracle/*/*.expected.txt`（上游程序的**输出记录**）

`calc.expected.txt`（1.7 KB）与 `number_to_chinese.expected.txt`（3.5 KB）
是从 rime-ice 的 `lua/calc_translator.lua`、`lua/number_translator.lua`
（GPL-3.0-only，revision `859e3b53…`）跑出来的**输入→输出记录**，
供 `crates/qingjian-engine/tests/{calc_oracle,number_oracle}.rs` 逐条对照。
它们是数据/事实，不含上游源代码；上游 Lua 源码**已从仓库移除**（§5.1）。
生成配方（上游 URL + revision）保留在 `tools/oracle/README.md`。

### 1.7 `tools/librime-probe/samples/*.txt`、`tools/rime-compare/report.md`（**运行记录**）

探针与对照工装跑真实 librime 1.16.1（BSD-3-Clause）得到的 JSON / Markdown
记录，含候选文本与 preedit。它们是**行为事实记录**，不是上游代码；
样本里出现的词条来自本机 `/usr/share/rime-data` 的方案与词典
（各方案数据另有自己的许可，本仓库**不分发**那些数据文件——
`tools/librime-probe/run/` 也被 `.gitignore` 排除）。

---

## 2. 部署期取回、**不随仓库分发**的源数据

以下 16 份文件由 [`tools/fetch-sources.sh`](tools/fetch-sources.sh) 按
[`tools/sources.lock`](tools/sources.lock) 取回到
`schemes/qingjian-default/build/`（`.gitignore` 排除）。**它们不在 git 里。**
每条 URL 都固定到 commit SHA，每条 sha256 都与该 revision 实测一致（§6.2）。

| 目标 | 上游 | 固定 revision | 版权 | 许可 | sha256 |
| --- | --- | --- | --- | --- | --- |
| `pinyin.txt` | [mozillazg/pinyin-data](https://github.com/mozillazg/pinyin-data) | `923b108d…` | Copyright (c) 2016 mozillazg | MIT | `621f8ca9…` |
| `THUOCL_IT.txt` | [thunlp/THUOCL](https://github.com/thunlp/THUOCL) | `a30ce79d…` | Copyright (c) 2018 THUNLP | MIT | `e8cd42c9…` |
| `THUOCL_law.txt` | 同上 | `a30ce79d…` | 同上 | MIT | `8ef1a5f4…` |
| `THUOCL_medical.txt` | 同上 | `a30ce79d…` | 同上 | MIT | `a01f39f1…` |
| `THUOCL_car.txt` | 同上 | `a30ce79d…` | 同上 | MIT | `58ffd756…` |
| `THUOCL_food.txt` | 同上 | `a30ce79d…` | 同上 | MIT | `084caa96…` |
| `THUOCL_lishimingren.txt` | 同上 | `a30ce79d…` | 同上 | MIT | `d0e50940…` |
| `THUOCL_chengyu.txt` | 同上 | `a30ce79d…` | 同上 | MIT | `c339d5d6…` |
| `THUOCL_poem.txt` | 同上 | `a30ce79d…` | 同上 | MIT | `f7045571…` |
| `jieba_dict.txt` | [fxsjy/jieba](https://github.com/fxsjy/jieba)（`extra_dict/dict.txt.big`） | `67fa2e36…` | Copyright (c) 2013 Sun Junyi | MIT | `b1601127…` |
| `opencc/STCharacters.txt` | [BYVoid/OpenCC](https://github.com/BYVoid/OpenCC) | `c363a7ba…` | 未署名 | Apache-2.0 | `a0ca1601…` |
| `opencc/STPhrases.txt` | 同上 | `c363a7ba…` | 未署名 | Apache-2.0 | `f6eab5e5…` |
| `opencc/TSCharacters.txt` | 同上 | `c363a7ba…` | 未署名 | Apache-2.0 | `737c21c6…` |
| `emoji/emoji.json` | [iDvel/rime-ice](https://github.com/iDvel/rime-ice)（`opencc/`） | `859e3b53…` | 未署名（仓库声明 GPL-3.0 only） | **GPL-3.0-only** | `79fe3b87…` |
| `emoji/emoji.txt` | 同上 | `859e3b53…` | 同上 | **GPL-3.0-only** | `09e29b83…` |
| `emoji/others.txt` | 同上 | `859e3b53…` | 同上 | **GPL-3.0-only** | `9595273a…` |

（sha256 的完整 64 位十六进制值、完整 URL、逐条说明见 `tools/sources.lock`；
本表为可读性截断。）

**关于 emoji 三件套的边界（重要）**：
- 旧版 `fetch-sources.sh` 把它们标成 `Apache-2.0`，**这是错的**：
  `iDvel/rime-ice` 的 LICENSE 是 **GNU GPL-3.0**，其 README 写
  `GPL-3.0 (only) License.`，`opencc/emoji.txt`、`others.txt` 是该仓库内容
  （README 自述"纯手搓的 Emoji"），仓库里**没有**为这三个文件另行声明的许可。
- 因此按 **GPL-3.0-only** 对待。本仓库**不分发**它们，也不把它们的任何内容
  编译进已跟踪的产物（`generated.dict.yaml` 不含 emoji）。
- 使用者若把自己 `build/` 下的副本**再分发**，需要自行满足 GPL-3.0 的义务
  （附完整许可文本与源码获取方式）。完整 GPL 文本在上游仓库
  <https://github.com/iDvel/rime-ice/blob/859e3b5300e0ea01334a627b15db101e94312a75/LICENSE>。

---

## 3. 仅引用 / 取证、未分发

| 对象 | URL | 固定 revision | 许可 | 在本仓库里的角色 |
| --- | --- | --- | --- | --- |
| librime 源码 | <https://github.com/rime/librime> | `2479df58cb51480299f94afe53d7b1790ecf0eb1` | BSD-3-Clause | `reference/*.md` 逐行引用；`tools/rime-compare/` 通过 `dlopen` 调用**系统已安装**的 librime 1.16.1；**源码未入库** |
| Rime 官方 wiki | <https://github.com/rime/home/wiki> | `5bfcf14a7ae127635dff9da1f133cae9a5319607` | 未声明（UNVERIFIED） | `reference/*.md` 引用；两份逐字存档见 §1.4 |
| rime-ice 仓库与词典 | <https://github.com/iDvel/rime-ice> | `859e3b5300e0ea01334a627b15db101e94312a75` | GPL-3.0-only | 只作**行为对照**；词典（含来源不明的 tencent/base）**未取回、未分发**；`opencc/emoji.*` 见 §2 |
| `/usr/share/rime-data` 的上游 preset 方案 | 本机安装 | 不适用 | 各方案不同 | `tools/librime-probe/samples/` 记录了它们的运行结果；**数据文件未分发** |
| rime-prelude（`default.yaml` / `symbols.yaml`） | <https://github.com/rime/rime-prelude> | 未在本仓库固定 | 各文件不同（多为 BSD-3-Clause） | `tools/rime-compare/fixtures/rime/default.yaml` 是**本项目自撰的最小版本**，明确不用 `/usr/share/rime-data/default.yaml` |

**关于"未取回 rime-ice 词典"**：`fetch-sources.sh` 只取 rime-ice 的
`opencc/` 下三个文件，**不取** `rime_ice.dict.yaml` 或 `cn_dicts/`。
`PLAN.md` §10 记录的理由不只是 GPL：那 44 MB 里最大的两块
（`tencent` 16.9 MB、`base` 16.2 MB）**来源不明或明确受限**。

---

## 4. 许可证文本位置与本项目依赖

| 许可 | 适用对象 | 文本位置 |
| --- | --- | --- |
| MIT | pinyin-data | [`licenses/MIT-pinyin-data.txt`](licenses/MIT-pinyin-data.txt) |
| MIT | THUOCL | [`licenses/MIT-THUOCL.txt`](licenses/MIT-THUOCL.txt) |
| MIT | jieba | [`licenses/MIT-jieba.txt`](licenses/MIT-jieba.txt) |
| Apache-2.0 | OpenCC（数据与项目） | [`LICENSE-APACHE`](LICENSE-APACHE)（本仓库已有的规范全文） |
| BSD-3-Clause | librime | [`licenses/BSD-3-Clause-librime.txt`](licenses/BSD-3-Clause-librime.txt) |
| GPL-3.0-only | rime-ice（仅 `build/` 下本地取回的 emoji 数据） | **不随本仓库分发**，故未收录全文；见 §2 的上游链接 |
| 未确定（UNVERIFIED） | Rime wiki 两页副本 | 无（见 §5.3） |
| MIT OR Apache-2.0 | 本项目自身 | [`LICENSE-MIT`](LICENSE-MIT) / [`LICENSE-APACHE`](LICENSE-APACHE) |

`licenses/` 下的 MIT / BSD-3-Clause 文本是**上游原文照录**（未改写一字）；
Apache-2.0 用仓库里已有的规范全文，不重复一份以免两处漂移。

**第三方代码依赖**：`Cargo.lock` 里**没有任何来自 registry 的包**
（10 个条目全部是 workspace 成员）。`scripts/verify-zero-deps.sh` 守
`qingjian-core` / `qingjian-engine` 零第三方依赖，`scripts/verify-deps.sh` 的白名单
当前为空。因此本项目不引入任何需要额外许可声明的 Rust 依赖。

**模型**：没有。本项目不使用、不分发任何机器学习模型
（`qingjian-embed` 是零依赖、无模型的本地计数投影）。

---

## 5. 已知问题与待决事项（不粉饰）

### 5.1 已处理：rime-ice 的 Lua 副本（GPL-3.0-only）

**整改前**：`tools/oracle/calc_translator/calc.lua`、
`tools/oracle/number_translator/number_to_chinese.lua`、
`tools/oracle/calc_probe.lua` 是从 rime-ice（GPL-3.0-only）复制/改写的
纯计算副本，**被跟踪、被分发**。把 GPL 部件与 MIT/Apache 作品**一起分发**
会使整份分发落入 GPL（`PLAN.md` §10 已写明这条规则），而项目所有者定下的
边界是 `MIT / Apache-2.0`。

**处理**（本次整改）：**移除这三个 `.lua`**，保留测试真正读取的
`.expected.txt` 输出记录。`tools/oracle/README.md` 保留上游 URL 与固定
revision（`859e3b53…`），需要时可在本地取回同一条源码做复核，
但**不再提交回仓库**。`crates/qingjian-engine/tests/*_oracle.rs` 只读
`.expected.txt`，测试不受影响（`cargo test -p qingjian-engine --test
calc_oracle --test number_oracle` 验证）。

**残留**：git **历史**里仍有这三个文件的旧版本。历史清理（`filter-repo` 等）
不在本次范围；如需彻底移除需另做一次带备份的历史改写。

### 5.2 待修：`opencc.manifest.yaml` 把 emoji 标成 Apache-2.0

位置：`schemes/qingjian-default/opencc.manifest.yaml`，
`converters[0]`（`name: emoji`）的 `license: Apache-2.0`。
应改为 `GPL-3.0-only`，并把 `source` 补上固定 revision。
本次整改未改 `schemes/`（超出本任务的修改范围），在此登记。

### 5.3 未解决：Rime wiki 两份逐字副本的许可

`reference/wiki-RimeWithTheDesign.md` 与 `reference/wiki-SharedData.md` 是
`rime/home.wiki` 的逐字副本（§1.4）。核实结果：

- `rime/home` 仓库根目录**没有**通用 `LICENSE` 文件（`LICENSE.txt` /
  `LICENSE.md` / `COPYING` 均为 404）；
- 只有一个 `LICENSE-freewill`（MIT 文本，`Copyright (c) 2013 Joseph Pan`），
  其适用范围未在任何地方说明，**不能据此断定覆盖 wiki 内容**；
- 两个 wiki 页面本身**没有署名、没有许可声明**。

**因此：许可状态 UNVERIFIED。** 在取得明确许可之前，这两份文件的分发
状态是**有疑义的**。可选处置（需项目所有者决定）：
① 向 Rime 项目取得书面许可；② 改为本项目自行撰写的摘要/转述，
只保留必要的短引用；③ 从仓库移除这两份文件。

### 5.4 待补：`probe.c` 文件头缺少 BSD-3-Clause 声明

BSD-3-Clause 的源码再分发要求"保留版权声明、条件列表与免责声明"。
当前这些内容只在 `THIRD_PARTY_NOTICES.md` §1.3 与
`licenses/BSD-3-Clause-librime.txt` 中，建议在
`tools/librime-probe/probe.c` 文件头加一行指向它们，使声明随文件自身可见。

### 5.5 关于 revision 的选法（方法说明，不是问题）

本文件里的 revision 是**核对当日各上游默认分支 HEAD 的 commit SHA**
（`git ls-remote <repo> HEAD`，日期 2026-09-13），不是"某个 release tag"。
commit SHA 本身不可变，因此这个引用是稳定的、可复现的。
它**不表示**我们跟进了上游的最新 release——恰恰相反：固定 SHA 意味着
在上游前进时我们**不动**，直到有人显式更新 `tools/sources.lock`。

### 5.6 上游数据的两处已知瑕疵（数据质量，不影响许可）

重新生成时生成器如实报告（不静默跳过）：

- `THUOCL_food.txt:39` 词频带尾随字符 `125472s`，按 `125472` 处理；
- `THUOCL_law.txt:7339` 词频为空，该行丢弃（词：浙江省地质灾害防治管理办法）。

---

## 6. 复现核对（命令 + 观察到的结果）

> 这些命令在 2026-09-13、本仓库 `ad936c2` 之后的工作树上执行。
> 结论是**当时的实测**；上游前进后需重跑。

### 6.1 枚举已跟踪内容

```bash
git ls-files | wc -l          # → 181（整改前的基线；整改后含新增/删除，见 §5）
git ls-files tools/ schemes/  # 逐条核对本文的路径
```

### 6.2 逐条把本地源码数据与固定 revision 比对

对 `tools/sources.lock` 的每条 URL 下载到临时目录，
`sha256sum` 与 `schemes/qingjian-default/build/` 下已有副本比对：

```text
pinyin.txt                 SAME   621f8ca9eff8519f47e2b17b564fd318161e13bca07eea8c8e04993cd5d3b52e
THUOCL_IT.txt              SAME   e8cd42c9f5559735fa3bfb91515cbe051f991efd4a803038a1d5c0da85af3526
...（8 份 THUOCL 全部 SAME）...
jieba_dict.txt             SAME   b16011275c42955ccd81fc1adecc93a59dbb7926af69d93fc95d4943d40f6aad
opencc/STCharacters.txt    SAME   a0ca1601c70648cf48b33c3c6210ccbecc5c7eead4b4c3daf76587ba2c03582b
opencc/STPhrases.txt       SAME   f6eab5e5c6dd7640597878d3dfc6599ee1279d2bc91561eadd8e114194e2925a
opencc/TSCharacters.txt    SAME   737c21c66f55a419dd6956cb3089476cdefc5a36877452631617696df1e5d925
emoji/emoji.json           SAME   79fe3b878625d8f96d70aeb6c4d1929ab2a582f8b935d309125b6ccffd799602
emoji/emoji.txt            SAME   09e29b83ad367ea273e9ab438e572a7621649d93b36924ead28852762d2898b1
emoji/others.txt           SAME   9595273a49139e1184bca0f1923660793b3873d7b4996d0a5dd665e777d4e55c
```

**16/16 SAME**：本地副本 == 固定 revision 上的文件。

`fetch-sources.sh` 的校验也反向验证过：把 `pinyin.txt` 的期望 sha256 改成
一个假值后运行，脚本以 **exit 1** 报"内容与锁文件不一致"并列出期望/实得值；
恢复后 `bash tools/fetch-sources.sh` 以 **exit 0** 通过。

### 6.3 核对两份 wiki 副本

```bash
diff <(curl -fsSL https://raw.githubusercontent.com/wiki/rime/home/RimeWithTheDesign.md) \
     reference/wiki-RimeWithTheDesign.md   # → 无输出（逐字节相同）
diff <(curl -fsSL https://raw.githubusercontent.com/wiki/rime/home/SharedData.md) \
     reference/wiki-SharedData.md          # → 无输出
git ls-remote https://github.com/rime/home.wiki.git HEAD
# → 5bfcf14a7ae127635dff9da1f133cae9a5319607
```

### 6.4 核对生成词库可由固定输入复现

```bash
rm -rf .work/regen && mkdir -p .work/regen/cn_dicts
cp schemes/qingjian-default/pinyin.schema.yaml .work/regen/
cargo run --offline --manifest-path tools/wordlist-gen/Cargo.toml -- \
    --sources schemes/qingjian-default/build --out "$PWD/.work/regen"
diff <(grep -v '^#' schemes/qingjian-default/cn_dicts/generated.dict.yaml) \
     <(grep -v '^#' .work/regen/cn_dicts/generated.dict.yaml)
# → 无输出（414,525 条正文与 YAML 头逐字节一致）
diff schemes/qingjian-default/pinyin.schema.yaml .work/regen/pinyin.schema.yaml
# → 无输出（音节表零差异）
```

生成器 stderr 摘要：拼音表 44,435 字（6,992 多音字）；去重后 653,447 词；
简繁过滤滤掉 236,127 条；最终 **414,525 条**（414 单字 + 414,111 词），
399 个编码单元——与已跟踪产物的头部数字一致。

### 6.5 确认仓库里没有 registry 依赖

```bash
bash scripts/verify-zero-deps.sh   # → 内核（qingjian-core qingjian-engine）保持零第三方依赖
bash scripts/verify-deps.sh        # → 0 个 registry 依赖，全部有受审记录（重复依赖 0）
```

---

## 7. 上游 URL 一览（便于复核）

| 名字 | URL | 本仓库用到的 revision |
| --- | --- | --- |
| pinyin-data | <https://github.com/mozillazg/pinyin-data> | `923b108dc5d45dee061324c011b478fb649f8b73` |
| THUOCL | <https://github.com/thunlp/THUOCL> | `a30ce79d895d01ab5132a5c74c29703ff7efb4cc` |
| jieba | <https://github.com/fxsjy/jieba> | `67fa2e36e72f69d9134b8a1037b83fbb070b9775` |
| OpenCC | <https://github.com/BYVoid/OpenCC> | `c363a7ba51d487950982bd8a589211ffbfd95ba1` |
| rime-ice | <https://github.com/iDvel/rime-ice> | `859e3b5300e0ea01334a627b15db101e94312a75` |
| librime | <https://github.com/rime/librime> | `2479df58cb51480299f94afe53d7b1790ecf0eb1` |
| Rime 官方 wiki | <https://github.com/rime/home/wiki> | `5bfcf14a7ae127635dff9da1f133cae9a5319607` |

---

*本文与 `tools/sources.lock`、`licenses/` 一起构成第三方的来源与许可记录。
更新任何一方的固定 revision 时，三处必须同步。*
