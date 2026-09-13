# RIME 的造句、补全与「不完整输入」 —— 以 librime 源码为准

**取证快照**

| 对象 | 版本 | 说明 |
| --- | --- | --- |
| librime 源码 | `master` @ `2479df58cb51480299f94afe53d7b1790ecf0eb1`（2026-09-13） | 所有 C++ 引用均指此提交；行号为该提交下的行号 |
| 本机 librime | 1.16.1（`/usr/lib/x86_64-linux-gnu/librime.so.1`） | 用于第 7 节的实测 |
| 对照工装 | `tools/rime-compare/` | 第 7 节的矩阵可由 `compare.py` 重跑（B3.1） |
| 语义导航 | clangd 21.1.8（`~/.local/bin/clangd`） | 只用于导航；**结论以源码阅读 + 运行库实测为准** |

源码 URL 形如 `https://raw.githubusercontent.com/rime/librime/master/src/rime/...`。
下文引用写作 `librime@2479df5 <path>:<行>`。

**术语**

- **拼写（spelling）**：方案里"能敲什么"的字符串（`ni`、`hao`、简拼的 `h`）。
- **编码单元（syllable）**：韵书里编码的最小单位，运行期编成整数 id。
- **音节图（`SyllableGraph`）**：输入串上"这一段是一个编码单元"的所有边。
- **词图（`WordGraph`）**：`map<int, map<int, DictEntryList>>`，即"从位置 i 到位置 j 有哪些词"。
- **造句（sentence composition）**：在词图上求一条覆盖输入的最优词序列，拼出一个**词库里没有的词**。
- **补全（completion）**：输入只打了一半时，补出更长拼写对应的词。
- **缩写（abbreviation）**：拼写代数派生的短拼写（`hao` → `h`）。

---

## 0. 一句话结论

librime 的拼音翻译器**不是**"把整串输入展开成编码、再逐条查表"，而是
**"在音节图上收集所有位置的匹配（词图），没有精确匹配就造句"**。

因此"输入不完整也能出候选"在 librime 里有**三条独立通路**：

| 通路 | 机制 | 出处 |
| --- | --- | --- |
| ① 只消费前缀 | 音节图只覆盖能解释的前缀，查表在该子图上做 | `algo/syllabifier.cc:268` |
| ② 拼写层补全 | 剩下的尾巴用 `ExpandSearch` 补成更长的拼写 | `algo/syllabifier.cc:224-228` |
| ③ 造句 | 没有精确匹配的词时，在词图上组合 | `gear/script_translator.cc:503` |

**Stele 三条都没有。** 第 8 节逐条列出差异。

---

## 1. 一次查询的完整路径

从按键到候选，拼音族走的是这条路（`librime@2479df5`）：

```
ScriptTranslator::Query(input, segment)              gear/script_translator.cc:214
  └─ New<ScriptTranslation>(...)                     :230
     └─ ScriptTranslation::Evaluate(dict, user_dict) gear/script_translator.cc:460
        ├─ ScriptSyllabifier::BuildSyllableGraph     :461  → 音节图
        ├─ Dictionary::Lookup(syllable_graph, ...)   :466  → 位置 0 的匹配
        ├─ 有精确匹配？没有 → MakeSentence(s)         :503  → 词图 + 造句
        └─ ...
  └─ DistinctTranslation 去重                         :237
  └─ Poet::ContextualWeighted（挂语言模型时）          :239
```

`Evaluate` 是理解一切分歧的那一段（`:460-517`）：

```cpp
// librime@2479df5 src/rime/gear/script_translator.cc:461-464
size_t consumed = syllabifier_->BuildSyllableGraph(*dict->prism());
bool predict_word = translator_->enable_word_completion() &&
                    start_ + consumed == end_of_input_;
```

```cpp
// librime@2479df5 src/rime/gear/script_translator.cc:502-514
// make sentences when there is no exact-matching phrase candidate
if (has_at_least_two_syllables && !has_reliable_phrase &&
    !has_reliable_user_phrase) {
  if (max_sentences_ > 1)
    sentences_ = MakeSentences(dict, user_dict);
  else if (max_sentences_) { ... MakeSentence(dict, user_dict); ... }
}
```

**注意这里没有 `enable_sentence` 的判断**——拼音族造句是**无条件**的，
唯一的条件是"至少两个音节、且没有精确匹配的词"。`enable_sentence` 是**码表族**的开关（第 6 节）。

---

## 2. 音节图：`interpreted_length` 可以小于输入长度

```cpp
// librime@2479df5 src/rime/algo/syllabifier.cc:267-274
graph->input_length = input.length();
graph->interpreted_length = farthest;
...
return farthest;
```

`input_length` 与 `interpreted_length` 是**两个不同的数**。`farthest` 在遍历中
取到过的最远位置（`:55-56`），补全成功时还会被推到最后（`:262`）。

这就是"只消费前缀"的全部机制：**图可以只覆盖输入的前缀，查表在该前缀上做，
剩下的字符留在输入里**。

实测（第 7 节）：`niha` 在「缩写开、补全关」那一格给出「你好」——此时图只覆盖
`ni` + `h`（`h` 是 `hao` 的缩写），候选跨越的是输入的前 **3** 个字符，
第 4 个字符 `a` 留在输入里、从未被消费。同一格的 preedit 渲染成 `ni ha`，
两种通路（缩写 / 补全）渲染出来**看起来一样**——所以判断靠的是第 7 节那种
只改一个变量的矩阵，不是看 preedit。

### 2.1 图上一条边是怎么来的

在 `farthest` 之前的每个位置，切分器做两件事（`algo/syllabifier.cc:66-88`）：

```cpp
// 有 canonicalizer（拼写代数里有重排类规则）时：逐长度精确查
if (canonicalizer_) { ... prism.GetValue(syllable, &value) ... }   // :82
else { prism.CommonPrefixSearch(current_input, &matches); }        // :87
```

`CommonPrefixSearch` 找的是"**树里存的键是当前剩余输入的前缀**"——
简拼 `h` 是 `ha` 的前缀，于是它在位置 2 就能被认领。这一条与补全**无关**，
是切分器本身就有的能力。

---

## 3. 补全：发生在拼写层，且**默认是开**

补全的开关不在翻译器手上，而是被交给切分器：

```cpp
// librime@2479df5 src/rime/gear/script_translator.cc:87-90
syllabifier_(translator->delimiters(),
             translator->enable_completion(),   // ← 补全开关传给切分器
             translator->strict_spelling(),
             translator->canonicalizer())
```

切分器在"图没能覆盖到输入末尾"时启动它：

```cpp
// librime@2479df5 src/rime/algo/syllabifier.cc:224-228
if (enable_completion_ && farthest < input.length()) {
  DLOG(INFO) << "completion enabled";
  const size_t kExpandSearchLimit = 512;
  vector<Prism::Match> keys;
  prism.ExpandSearch(input.substr(farthest), &keys, kExpandSearchLimit);
```

`ExpandSearch` 找出"**以这段尾巴开头、但更长**"的拼写（`dict/prism.cc:288`），
把它们作为一条边接进图里，属性标成 `kCompletion` 并**扣一次可信度**
（`:245-248`），最后把 `farthest` 推到输入末尾（`:262`）。

**默认值**（这一点 Stele 的注释写反了）：

```cpp
// librime@2479df5 src/rime/gear/translator_commons.h:176
bool enable_completion_ = true;
```

读取处是 `gear/translator_commons.cc:123`（键为 `<命名空间>/enable_completion`）。
`script_translator` 另外还有一个 `enable_word_completion`，
**没写时继承 `enable_completion_`**：

```cpp
// librime@2479df5 src/rime/gear/script_translator.cc:194-196
if (!config->GetBool(name_space_ + "/enable_word_completion",
                     &enable_word_completion_)) {
  enable_word_completion_ = enable_completion_;
}
```

它只用来决定"要不要在整串都被消费时做预测式查词"（`:463-464`），
**不影响**上面那条拼写层补全。

---

## 4. 词图：所有位置的匹配

造句的原料是词图。它由"对音节图**每一条边**分别查表"得到：

```cpp
// librime@2479df5 src/rime/gear/script_translator.cc:705-722（节选）
WordGraph ScriptTranslation::PrepareForMakingSentence(Dictionary* dict, UserDictionary* user_dict) {
  WordGraph graph;
  for (const auto& x : syllable_graph.edges) {
    auto& same_start_pos = graph[x.first];
    if (user_dict) EnrollEntries(same_start_pos, user_dict->Lookup(syllable_graph, x.first, ...));
    EnrollEntries(same_start_pos, dict->Lookup(syllable_graph, x.first, &translator_->blacklist()));
  }
  return graph;
}
```

`Dictionary::Lookup`（`dict/dictionary.cc:271`）返回的是一个 `DictEntryCollector`：
**该起点上所有长度的所有匹配**，不是一个"最优匹配"。

这与"展开成编码再逐条精确查表"是两种数据模型：前者是**图**，后者是**若干条路径**。

---

## 5. 造句：不需要语言模型

```cpp
// librime@2479df5 src/rime/gear/poet.cc:246-253
an<Sentence> Poet::MakeSentence(const WordGraph& graph, size_t total_length,
                                const string& preceding_text) {
  return grammar_ ? MakeSentenceWithStrategy<BeamSearch>(graph, total_length, preceding_text)
                  : MakeSentenceWithStrategy<DynamicProgramming>(graph, total_length, preceding_text);
}
```

**有语言模型走束搜索，没有就走动态规划。** 两种策略共用同一个实现
（`poet.cc:192` 的 `MakeSentenceWithStrategy`）：以位置为状态，
逐条边扩展、按 `compare_` 保留更优的行（`:221-227`），最后取终点状态的最优行。

一个细节值得记下来——**整串就是一个词时不算造句**：

```cpp
// librime@2479df5 src/rime/gear/poet.cc:206-208
size_t end_pos = ev.first;
if (start_pos == 0 && end_pos == total_length)
  continue;  // exclude single word from the result
```

`poet.h` 的抬头注释自称 "simplistic sentence-making"——它不是语言模型级的造句，
就是一个词图上的最短路。

---

## 6. `enable_sentence` 到底在哪一边

| 翻译器 | 有没有这个开关 | 默认值 | 消费点 |
| --- | --- | --- | --- |
| `script_translator`（拼音族） | **没有** | — | 无条件造句（`script_translator.cc:503`） |
| `table_translator`（码表族） | 有 | **`true`**（`table_translator.h:43`） | `:218` 读 → `:226` 建 `Poet` → `:293` 触发 |

```cpp
// librime@2479df5 src/rime/gear/table_translator.cc:226-228
if (enable_sentence_ || sentence_over_completion_ ||
    contextual_suggestions_) {
  poet_.reset(new Poet(language(), config, Poet::LeftAssociateCompare));
}
```

```cpp
// librime@2479df5 src/rime/gear/table_translator.cc:293-294
if (enable_sentence_ && !translation) {
  translation = MakeSentence(input, segment.start, /* include_prefix_phrases = */ true);
}
```

结论：**这个开关属于码表族，而且默认是开的**；拼音族压根没有它。

---

## 7. 实测：缩写 × 补全 2×2 矩阵

第 2、3 节说的两条通路都能让 `niha` 出「你好」。要分开它们，只有**只改一个变量**地跑。
工装 `tools/rime-compare/compare.py` 的 B3.1 就是这张表（可重跑）：

同一份词表（`fixtures/shared.dict.yaml`），只改「缩写规则」与「补全」两个开关：

| 缩写 | 补全 | librime 候选（前 3） | stele 候选（前 3） |
| --- | --- | --- | --- |
| 开 | 开 | 你好 拟好 尼号 | `niha`（字面量） |
| 开 | 关 | 你好 拟好 尼号 | `niha` |
| 关 | 开 | 你好 拟好 尼号 | `niha` |
| 关 | 关 | 你 尼 泥 | `niha` |

**对照 `nih`**（缩写开、补全关）：librime `你好 拟好 尼号`；stele `你好 拟好 尼号`。

**怎么读这张表**

- librime 4 格里 **3 格**命中 → 它有**不止一条**通路：
  - 「缩写开」的两格走 ①：`h` 被认领，只消费 3 个字符，`a` 留在输入里；
  - 「缩写关、补全开」那一格走 ②：`ExpandSearch("ha")` → `hao`，消费全部 4 个字符。
  - 只有两条都关掉才失效（第 4 格）。
- stele 4 格**全不命中**——但对照 `nih` 两边都命中，说明**它的缩写是活的**。
  它缺的是**两处**：
  1. **没有"只消费前缀"**：`SpellingTable::expand_into` 只把"到达末尾的路径"
     当作展开结果（`crates/stele-engine/src/spelling.rs:638-639`），
     所以 `nih`（能整串消费）行、`niha`（不能）不行。
  2. **补全在编码单元层**：`crates/stele-engine/src/translator.rs:192` 是
     `lexicon.prefix_lookup(&exp.code, ...)`，而尾巴 `ha` 产不出 `exp.code`，
     补全永远轮不到；librime 的补全在**拼写层**。

**顺带证实了默认值**：夹具从没写过 `enable_completion`，
而「缩写关、补全开」那一格仍然命中 → 它只能是默认开的
（`translator_commons.h:176`）。

> **一条方法论记录**：本节第一版结论是错的——当时写的是"stele 只有缩写这一条通路，
> 差在补全"。真正跑出这张表后才发现 stele **四格全空**，缺的是两处。
> "能解释现象"和"是那个原因"是两句话；**只改一个变量的实验**才是分界线。

---

## 8. 与 stele 实现的差异

**这一节是快照，会过期。** 核对对象是本文落笔时磁盘上的代码。
**只做记录，未修改任何实现文件。**

| # | 主题 | librime 的事实 | stele 现状（写作时） | 结论 |
| --- | --- | --- | --- | --- |
| 1 | 切分图覆盖范围 | `interpreted_length` **可以小于**输入长度，查表在该前缀上做（`algo/syllabifier.cc:268`） | `SpellingTable::expand_into` 只把"到达末尾的路径"当展开结果（`spelling.rs:638-639`） | **语义差异。** stele 没有"只消费前缀"这回事 |
| 2 | 补全的层级 | **拼写层**：`ExpandSearch(尾巴)`（`algo/syllabifier.cc:224-228`） | **编码单元层**：`lexicon.prefix_lookup(&exp.code, ...)`（`translator.rs:192`） | 尾巴不是合法单元时，stele 的补全永不触发 |
| 3 | 补全的默认值 | `enable_completion_ = true`（`translator_commons.h:176`） | `default_completion() -> false`（`spec.rs:581`），且注释称「RIME 的默认也是关」（`spec.rs:577`） | **注释与上游不符**，默认值也相反；两者建议一起改 |
| 4 | 造句 | 拼音族**无条件**（`script_translator.cc:503`）；码表族有开关、默认 `true` | **没有造句器** | 缺能力，且不需要语言模型（`poet.cc:249-252`） |
| 5 | `enable_sentence` 的归属 | 只在码表族（`table_translator.h:43`） | 放在两族共用的 `TranslatorSpec`（`spec.rs:556`） | **建模错位**；且该字段零读取（rust-analyzer `references` 只返回声明 + `components.rs:711` 的赋值） |
| 6 | 词图 | `WordGraph`（`poet.h:20`）+ `PrepareForMakingSentence`（`script_translator.cc:705`） | 无对应结构 | 造句的前置 |
| 7 | 音节图查询的用法 | 图查询（`CommonPrefixSearch`）只是**前半段**，后面还有造句 | `spelling.rs:643-644` 的注释写「这也正是 RIME 的 `script_translator` 在音节图上做查询的方式」 | 这句话**本身不假**，但容易让读者以为两边的模型一致：RIME 在图查询之后还会用这张图造句 |

---

## 9. 明确的取证缺口

- **没有真正的 `compile_commands.json`**。本机没有 `cmake`，也没有 boost / leveldb /
  yaml-cpp / opencc 的开发包，且不允许提权，因此 librime **没有被编译过**。
  clangd 用的是重建的 `compile_flags.txt` + 本地解包的头文件（见附录）。
  所以：**凡本文给出的语义结论，依据都是源码阅读 + 运行库实测，不是 clangd 的推断。**
- **`max_sentences > 1` 未实测**：`MakeSentences`（`script_translator.cc:725`）走的是
  另一段束搜索代码（`poet.cc:258`），夹具没调它。
- **带语言模型的 `BeamSearch` 分支未实测**：本机没有可自由分发的语言模型数据，
  实测全部落在 `DynamicProgramming` 分支。第 5 节的"不需要语言模型"正是因此才成立。
- **`table_translator` 的 `enable_sentence` 未端到端实测**：需要一份码表方案 + 词库；
  第 6 节只有代码侧核对。
- **`sentence_over_completion`**（`table_translator.cc:226`、`:297`）只读到了存在，
  语义未展开。

---

## 附录：语义导航环境（clangd）

本机没有 clangd（Ubuntu 把它拆成单独的包），librime 的依赖头文件也不全。两者都可以
**不需要 root** 地装好，做法是 `apt-get download` + `dpkg-deb -x`：

```bash
# 1) clangd 本体 + 它缺的运行库（protobuf / grpc / absl / re2）
#    解到 ~/.local/opt/clangd-21，用 ~/.local/bin/clangd 包一层 LD_LIBRARY_PATH
# 2) librime 需要的头文件 → ~/.local/opt/cpp-headers/include
#    boost / marisa / leveldb / yaml-cpp / opencc / darts / X11(keysym)
# 3) 让 clangd 知道怎么解析 librime：
#    - .work/upstream/librime/compile_flags.txt  → -Isrc -I<头文件库> -std=c++17
#    - .work/upstream/librime/src/rime/build_config.h
#      （由 build_config.h.in 展开；**有意不定义 RIME_ENABLE_LOGGING**，
#        这样 common.h 走自带的 no_logging.h，省掉 glog）
```

验证方式（不是"能启动"，而是"能解析"）：

```bash
cd .work/upstream/librime
clangd --check=src/rime/dict/prism.cc   # 只数真实诊断，忽略 tweak 日志
```

**注意**：这套脚手架放在 `.work/`（gitignore）里，重新克隆 librime 后需要重做。
它只是**取证工具**，不参与项目构建，也不是结论的来源。
