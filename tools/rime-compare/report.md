# RIME × Stele 对比测试报告

| | |
| --- | --- |
| librime | 1.16.1（系统运行库，经 `dlopen` 调用） |
| librime 源码副本 | `2479df5 2026-09-13`（只用于引用行号） |
| plum 源码副本 | `b1be196 2026-05-08`（维度 D 的配方表） |
| stele | `target/release/stele` |
| 上游方案数据 | `/usr/share/rime-data`（plum preset 的部署产物） |

生成命令：`python3 tools/rime-compare/compare.py`

## 0. 这次对照的边界

**比**：能否上屏、按键是否被处理、**同一份词表下的候选相对顺序**、上游方案能否装载、plum 配方的覆盖。

**不比**：绝对分数（两边的分值域不同）、候选总数（词库规模不同）、以及任何需要**同一份语言模型**才能比的东西（本工装两边都不挂语言模型）。

## A. 结构行为（librime 运行时 vs stele）

这一节由 `tools/compare-librime.py` 产出（P3 的验收线），本工装**原样调用**它，避免同一批用例有两份定义。

### stele × librime 对照报告

| | |
| --- | --- |
| librime | 1.16.1 / `luna_pinyin`（`--reset` 冷启动基线） |
| stele | `pinyin`（内嵌默认方案） |
| 比什么 | **结构**：能否上屏、按键是否被处理、切分边界 |
| 不比什么 | 候选排序与分数——两边的词库与语言模型不同，比排序等于比词库 |

#### `nihao` — full-spelling

- ✓ 都上屏（librime='你好' stele='你好'）

#### `ni` — single-unit

- ✓ 都上屏（librime='你' stele='你'）

#### `nh` — abbreviation

- ✓ 都上屏（librime='你會' stele='南海'）

#### `,` — punctuation

- ✓ 都是全角标点（librime='，' stele='，'）

#### `uUni` — prefix-not-configured

- ✓ 全部按键都被处理

#### `zhongguo` — typing-progresses

- ✓ 全部按键都被处理

---
**全部一致**（在「结构行为」这个范围内）。

> 这份报告**不断言候选排序一致**，因为两边的词库不同——
> 比排序比的是词库，不是引擎。要比排序，得先让两边吃同一份词表
> （P3.5 的默认方案做完之后可以再补一份）。

## B. 同一份词表下的排序对照 ★

两边装载的是**同一份** `fixtures/shared.dict.yaml`：

| 词 | 编码 | 权重 |
| --- | --- | --- |
| 你 | `ni` | 900 |
| 尼 | `ni` | 500 |
| 泥 | `ni` | 100 |
| 你好 | `nihao` | 800 |
| 拟好 | `nihao` | 20 |
| 尼号 | `nihao` | 10 |
| 好 | `hao` | 900 |
| 号 | `hao` | 300 |
| 耗 | `hao` | 100 |
| 我 | `wo` | 900 |
| 门 | `men` | 500 |
| 我们 | `women` | 1000 |
| 先 | `xian` | 900 |
| 西安 | `xian` | 850 |

### B1 排序不变式：同码候选必须按词库权重降序

| 输入 | 说明 | 词表算出的期望序 | librime | stele | 判定 |
| --- | --- | --- | --- | --- | --- |
| `ni` | 同码单字：三个字按权重降序 | 你 尼 泥 | 你 尼 泥 | 你 尼 泥 | ✓ |
| `nihao` | 同码词：三个词按权重降序 | 你好 拟好 尼号 | 你好 拟好 尼号 | 你好 拟好 尼号 | ✓ |
| `hao` | 同码单字：三个字按权重降序 | 好 号 耗 | 好 号 耗 | 好 号 耗 | ✓ |
| `women` | 两音节词 | 我们 | 我们 | 我们 | ✓ |
| `xian` | 跨切分竞争：单音节 `xian` vs 两音节 `xi an` | 先 西安 | 先 西安 | 先 西安 | ✓ |

> **这条断言的性质**：它不要求两边的候选**集合**相同（librime 会多出前缀与造句候选），只要求**共同候选之间的相对顺序**与词库权重一致。

### B2 缩写（简拼）：每个音节取首字母也要命中

| 输入 | 期望词 | librime 第 1 位非字面量 | stele 第 1 位非字面量 | 判定 |
| --- | --- | --- | --- | --- |
| `nh` | 你好 | 你好 | 你好 | ✓ |
| `wom` | 我们 | 我们 | 我们 | ✓ |

> 在 41 万条的默认方案里 `nh` 打不出「你好」（展开名额被截断，HANDOFF §5 第 28 条）；这里的小字母表证明**规则本身是活的**。

### B3 分歧观察（记录，不判失败）：前缀匹配与造句

| 输入 | 说明 | librime 走的通路 | librime 候选（前 5） | stele 候选（前 5） |
| --- | --- | --- | --- | --- |
| `niha` | 末音节只打了一半（`ha` 是 `hao` 的前缀） | predictive 查询（`Prism::ExpandSearch`） | 你好 拟好 尼号 你 尼 | niha |
| `haoni` | 词库里没有「好你」，但两个字都有 | 造句：两个单字拼成词库外的词 | 好你 好 号 耗 | haoni |
| `nihaoshijie` | 词库里只有前两个音节，后面是未消费的输入 | 只翻译能认出的前缀段，剩余留在输入里 | 你好 拟好 尼号 你 尼 | nihaoshijie |

> **共同点**：librime 允许输入**不完整**——它可以只消费一部分输入（末音节打一半 `niha`、只认前缀段 `nihaoshijie`），也可以用单字**造句**（`haoni` → 好你）。
> 
> **Stele 的模型**：拼写图要求输入是**编码单元的完整序列**，整段一起翻译；输入消费不完就退化成「字面量」候选（B3 三行的 stele 列都只剩输入串本身）。这是模型差异，不是崩溃——但**日常打字里「多打了一个字母」的场景，体验会明显不同**。

### B4 配置项核对：被解析、但引擎里没人读的开关

| 输入 | librime | stele（默认） | stele（`enable_sentence: true`） |
| --- | --- | --- | --- |
| `haoni` | 好你 好 号 耗 | haoni | haoni |

- `enable_sentence: true` 前后，stele 的输出**完全相同**（开关没有生效）。

> **代码侧核对**：`TranslatorSpec::enable_sentence`（`crates/stele-engine/src/spec.rs:556`）确实由`crates/stele-schemes/src/components.rs:711` 从方案里读出来，但**引擎里没有任何地方读它**（`grep -rn enable_sentence crates/stele-engine/src` 只命中字段声明本身）；`Origin::Sentence` 也只有一个测试夹具在产出。也就是说：**方案里写了 `enable_sentence: true`，不会有任何效果，也不会有警告**。这与 HANDOFF §5 第 36 条（「实现了」与「被装配了」是两件事）是同一形状，只是这次连「实现」都没有。
> 
> **另一处注释与上游源码不符**：`TranslatorSpec::default_completion()`的注释写着「RIME 的默认也是关」，而 librime 里`TranslatorOptions::enable_completion_` 的初值是 **`true`**（`src/rime/gear/translator_commons.h:176`），`script_translator` 未显式配置时 `enable_word_completion_` 继承它（`src/rime/gear/script_translator.cc:194-196`）。建议要么改注释，要么对齐默认值——**别让注释替上游下结论**。


## C. 上游方案能否装载（plum preset → stele）

本机 `/usr/share/rime-data` 是 **plum preset 包的部署产物**（luna-pinyin / cangjie / bopomofo / stroke / terra-pinyin / essay / prelude / quick；Debian 的 `rime-data-*` 包）。逐个方案单独放一个目录装载：

| 方案 | 结果 | 卡在哪几类 | 性质 | 首条诊断 |
| --- | --- | --- | --- | --- |
| `bopomofo` | ✗ 装载失败 | RIME 的开关可以只写 `options:`（单选组），Stele 要求 `name`；RIME 用 X11 keysym 名（`KP_1`、`Shift+exclam`…），Stele 只认自己的键名；词库权重列不是数字：RIME 的 `%` 百分比权重，或 `columns:` 声明的非文字列（如 `stem`）；`speller.algebra` 是 `__patch` 映射而不是规则列表（同上） | **缺口** | [第 31 行] .switches[2].name: 第 2 个开关缺少 `name` |
| `bopomofo_express` | ✗ 装载失败 | `alphabet` 由 `__patch` 指向另一个 YAML 的子树补全，Stele 未实现跨文件 `__patch`；`speller.algebra` 是 `__patch` 映射而不是规则列表（同上） | **缺口** | [列表写法：每个元素是一个编码单元（拼音方案写音节表；字形方案写字母表）；字符串写法：每个字符是一个编码单元] .speller.alphabet: 缺少 … |
| `bopomofo_tw` | ✗ 装载失败 | `alphabet` 由 `__patch` 指向另一个 YAML 的子树补全，Stele 未实现跨文件 `__patch`；`speller.algebra` 是 `__patch` 映射而不是规则列表（同上） | **缺口** | [列表写法：每个元素是一个编码单元（拼音方案写音节表；字形方案写字母表）；字符串写法：每个字符是一个编码单元] .speller.alphabet: 缺少 … |
| `cangjie5` | ✗ 装载失败 | 引用了 RIME 自带预设（`default` / `symbols`）：机制有、**资产**没搬（D24）；词库权重列不是数字：RIME 的 `%` 百分比权重，或 `columns:` 声明的非文字列（如 `stem`） | **缺口** + 有意（D24） | cangjie5 .translator.dictionary: 算词库校验和失败：cangjie5:73：词条「晭」的权重 `ab'gr` 不是数字。权重… |
| `cangjie5_express` | ✗ 装载失败 | 引用了 RIME 自带预设（`default` / `symbols`）：机制有、**资产**没搬（D24）；词库权重列不是数字：RIME 的 `%` 百分比权重，或 `columns:` 声明的非文字列（如 `stem`） | **缺口** + 有意（D24） | cangjie5 .translator.dictionary: 算词库校验和失败：cangjie5:73：词条「晭」的权重 `ab'gr` 不是数字。权重… |
| `detenele` | ✗ 装载失败 | 引用了 RIME 自带预设（`default` / `symbols`）：机制有、**资产**没搬（D24）；词库权重列不是数字：RIME 的 `%` 百分比权重，或 `columns:` 声明的非文字列（如 `stem`） | **缺口** + 有意（D24） | terra_pinyin .translator.dictionary: 算词库校验和失败：terra_pinyin:7066：词条「一」的权重 `98%`… |
| `luna_pinyin` | ✗ 装载失败 | 引用了 RIME 自带预设（`default` / `symbols`）：机制有、**资产**没搬（D24）；词库权重列不是数字：RIME 的 `%` 百分比权重，或 `columns:` 声明的非文字列（如 `stem`）；`speller.algebra` 是 `__patch` 映射而不是规则列表（同上） | **缺口** + 有意（D24） | .speller.rules: `speller.rules` 必须是列表 |
| `luna_pinyin_fluency` | ✗ 装载失败 | `alphabet` 由 `__patch` 指向另一个 YAML 的子树补全，Stele 未实现跨文件 `__patch` | **缺口** | [列表写法：每个元素是一个编码单元（拼音方案写音节表；字形方案写字母表）；字符串写法：每个字符是一个编码单元] .speller.alphabet: 缺少 … |
| `luna_pinyin_simp` | ✗ 装载失败 | `alphabet` 由 `__patch` 指向另一个 YAML 的子树补全，Stele 未实现跨文件 `__patch` | **缺口** | [列表写法：每个元素是一个编码单元（拼音方案写音节表；字形方案写字母表）；字符串写法：每个字符是一个编码单元] .speller.alphabet: 缺少 … |
| `luna_pinyin_tw` | ✗ 装载失败 | `alphabet` 由 `__patch` 指向另一个 YAML 的子树补全，Stele 未实现跨文件 `__patch` | **缺口** | [列表写法：每个元素是一个编码单元（拼音方案写音节表；字形方案写字母表）；字符串写法：每个字符是一个编码单元] .speller.alphabet: 缺少 … |
| `luna_quanpin` | ✗ 装载失败 | `alphabet` 由 `__patch` 指向另一个 YAML 的子树补全，Stele 未实现跨文件 `__patch`；`speller.algebra` 是 `__patch` 映射而不是规则列表（同上） | **缺口** | [列表写法：每个元素是一个编码单元（拼音方案写音节表；字形方案写字母表）；字符串写法：每个字符是一个编码单元] .speller.alphabet: 缺少 … |
| `stroke` | ✗ 装载失败 | 引用了 RIME 自带预设（`default` / `symbols`）：机制有、**资产**没搬（D24）；RIME 用 X11 keysym 名（`KP_1`、`Shift+exclam`…），Stele 只认自己的键名；词条编码未按空格切成字母表单元（精确编码族要求编码可逐项枚举） | **缺口** + 有意（D24） | stroke .translator.dictionary: 词库编译失败：写产物失败：stroke：词条「㐀」引用了字母表里没有的编码单元「shhsh」 |
| `terra_pinyin` | ✗ 装载失败 | 引用了 RIME 自带预设（`default` / `symbols`）：机制有、**资产**没搬（D24）；词库权重列不是数字：RIME 的 `%` 百分比权重，或 `columns:` 声明的非文字列（如 `stem`） | **缺口** + 有意（D24） | terra_pinyin .translator.dictionary: 算词库校验和失败：terra_pinyin:7066：词条「一」的权重 `98%`… |

**汇总**：13 个上游方案，可装载 **0**；至少有一处**真实缺口**的 **13**（`bopomofo`、`bopomofo_express`、`bopomofo_tw`、`cangjie5`、`cangjie5_express`、`detenele`、`luna_pinyin`、`luna_pinyin_fluency`、`luna_pinyin_simp`、`luna_pinyin_tw`、`luna_quanpin`、`stroke`、`terra_pinyin`）；只因 RIME 资产未搬而失败的 **0**（—）。

> **怎么读这张表**：一个方案可以同时命中几类。「有意」指的是 `import_preset`——**机制我们有、资产没搬**（D24：内核与方案分离，RIME 的 `default` / `symbols` 是它自己的资产）。标着「缺口」的才是能力问题，其中可归成四组：
> 
> 1. **字典格式**：RIME 的 `columns:` 声明（`cangjie5` 的 `stem` 列）与 `%` 百分比权重（`luna_pinyin` / `terra_pinyin`）——`stele-dict` 只认「词 `TAB` 编码 `TAB` 数字权重」。
> 2. **编码切分**：RIME 码表方案的编码是字符集上的**无空格字符串**（`stroke` 的 `shhsh`），Stele 要求编码按空格切成字母表单元。
> 3. **配置机制**：跨文件的 `__patch`（`pinyin:/abbreviation`）与只给 `options:` 的单选组开关。
> 4. **键名**：X11 keysym 名（`KP_1`、`Shift+exclam`）。

**另一条观察（C2）**：把整目录一次交给 Stele（`stele --scheme-dir /usr/share/rime-data --list`）时，它**停在第一个坏方案上**（`bopomofo`），后面的方案一个都没报。逐目录隔离才有上面这张表。要不要改成「跳过坏的、装载好的」是个产品决定——但它现在意味着**一个坏方案会让整目录都用不了**。

## D. plum 配方覆盖

plum 副本：`/home/brennmond/projects/stele/.work/upstream/plum`（b1be196 2026-05-08）；本机已部署 13 个方案。

| 配方 | 类别 | 本机部署的方案 | Stele 装载结果 |
| --- | --- | --- | --- |
| `bopomofo` | preset | `bopomofo` `bopomofo_express` `bopomofo_tw` | ✗ 装载失败 |
| `cangjie` | preset | `cangjie5` `cangjie5_express` | ✗ 装载失败 |
| `essay` | preset | `essay.txt`（八股文词表（共享资产，无方案）） | —（无方案） |
| `luna-pinyin` | preset | `luna_pinyin` `luna_pinyin_fluency` `luna_pinyin_simp` `luna_pinyin_tw` `luna_quanpin` | ✗ 装载失败 |
| `prelude` | preset | `default.yaml`（默认配置与预设（共享资产，无方案）） | —（无方案） |
| `quick` | preset | —（未部署） | 未实测 |
| `stroke` | preset | `stroke` | ✗ 装载失败 |
| `terra-pinyin` | preset | `terra_pinyin` | ✗ 装载失败 |
| `array` | extra | —（未部署） | 未实测 |
| `cantonese` | extra | —（未部署） | 未实测 |
| `combo-pinyin` | extra | —（未部署） | 未实测 |
| `double-pinyin` | extra | —（未部署） | 未实测 |
| `emoji` | extra | —（未部署） | 未实测 |
| `ipa` | extra | —（未部署） | 未实测 |
| `jyutping` | extra | —（未部署） | 未实测 |
| `middle-chinese` | extra | —（未部署） | 未实测 |
| `pinyin-simp` | extra | —（未部署） | 未实测 |
| `scj` | extra | —（未部署） | 未实测 |
| `soutzoe` | extra | —（未部署） | 未实测 |
| `stenotype` | extra | —（未部署） | 未实测 |
| `wubi` | extra | —（未部署） | 未实测 |
| `wugniu` | extra | —（未部署） | 未实测 |

> **映射是启发式的**：配方名（`luna-pinyin`）与方案 id（`luna_pinyin`）没有正式对应表，这里按第一个下划线/连字符前的词根匹配。`essay` / `prelude` 是共享资产（八股文词表、默认配置），**不含方案**，所以它们的「Stele 装载结果」写「无方案」——那不是失败。
> 
> `extra` 组的配方在本机**没有部署**，因此无法实测；要覆盖它们得先用 plum 取回（`rime-install`），这一步会访问网络，本工装不替使用者做。

## 结论

**全部断言通过**（A 结构行为、B1 排序不变式、B2 缩写）。

**这次对照交出的问题清单**（不判失败，但都是可开工的条目）：

1. **拼写图不做「不完整输入」**（B3，3 条用例）：输入的末音节打一半、或输入比词条长时，Stele 退化成字面量候选，librime 仍给前缀候选 / 造句候选。
2. **`enable_sentence` 被解析但没有消费者**（B4）：方案里写 `enable_sentence: true` 不会有任何效果，也没有警告。
3. **13 个上游 preset 方案有真实装载缺口**（C）：字典 `columns:` / `%` 权重、码表编码的无空格字符串、跨文件 `__patch`、X11 键名——四组，见 C 节的归类。
4. **一个坏方案会让整个 `--scheme-dir` 都用不了**（C2）：装载器停在第一个坏方案上，且只报它。

> 本报告由 `tools/rime-compare/compare.py` 生成，重跑同一条命令应得到同样的结论（librime 侧一律取冷基线）。
