# 词库管线（P3.5）

这个目录里的东西解决一件事：**把"干净来源"的公开词表编成青简能装载的
`.dict.yaml`**，并让"这份数据是怎么来的"可以被重新跑一遍。

它们**不进内核 crate、不进 CI**（与 `tools/` 里其它工装一样）：
换词库是"使用者给判断"的事，不是单元测试能断言的。

---

## 两步

```bash
# ① 取回源数据（一次网络访问；落进 schemes/qingjian-default/build/，该目录被 gitignore）
#    下载地址固定到 commit SHA，清单是**已跟踪的** tools/sources.lock
bash tools/fetch-sources.sh

# ② 编成词库 + 同步方案的音节表
cargo run --release --manifest-path tools/wordlist-gen/Cargo.toml -- \
    --sources schemes/qingjian-default/build --out schemes/qingjian-default
```

**可复现来源（阶段 4 / 审计 J2.2）**：`tools/sources.lock` 是权威清单，
每条记录含**固定到 commit SHA 的 URL**、revision、SPDX 许可、版权行、
sha256 与说明。旧版用的是浮动 `main` / `master`，且 sha256 只记在
`build/sources.lock`（gitignore 目录里，克隆的人看不到）——两者都已改正。
`fetch-sources.sh` 取回后逐条校验 sha256，不一致就报错停下。

产物落在 **`schemes/qingjian-default/cn_dicts/generated.dict.yaml`**，
由主词典 `pinyin.dict.yaml`（一个**导入清单**）通过 `import_tables` 引用。
这样"手写演示词库"（`base.dict.yaml`）与"生成词库"是两个独立文件，
各自的来源一眼可见。

`--dry-run` 只报告会写什么；`--max-per-source N` 与 `--max-chars N` 用来裁剪。

---

## 数据来源与许可

| 文件 | 来源 | 许可 | 提供了什么 |
| --- | --- | --- | --- |
| `pinyin.txt` | [`mozillazg/pinyin-data`](https://github.com/mozillazg/pinyin-data) | MIT | 4.4 万汉字的拼音（首要读音在前） |
| `jieba_dict.txt` | [`fxsjy/jieba`](https://github.com/fxsjy/jieba) 的 `extra_dict/dict.txt.big` | MIT | **通用**词表 58 万条（`词 词频 词性`） |
| `THUOCL_*.txt` | [`thunlp/THUOCL`](https://github.com/thunlp/THUOCL) | MIT | 8 份分领域词表（IT / 法律 / 医学 / 汽车 / 饮食 / 历史名人 / 成语 / 诗词），带语料词频 |
| `opencc/ST*.txt`、`TSCharacters.txt` | [`BYVoid/OpenCC`](https://github.com/BYVoid/OpenCC) | Apache-2.0 | 简繁转换表；`TSCharacters` 同时用于**滤掉繁体词条** |
| `emoji/*` | [`iDvel/rime-ice`](https://github.com/iDvel/rime-ice) 的 `opencc/` | **GPL-3.0-only** | 中文 → emoji 转换表（**不分发**；旧文档误标 Apache-2.0） |

> 每条来源的固定 revision、版权行与 sha256 见 `tools/sources.lock`；
> 逐项的 artifact / 版权 / 许可文本见根目录 `THIRD_PARTY_NOTICES.md`。

**为什么源数据不进仓库**：其中大部分是可分发的（MIT / Apache-2.0），
但"数据的来源与许可"是使用者要能**自己核对**的东西，所以 `build/`
在 `.gitignore` 里，由 `fetch-sources.sh` 按需取回、并用 sha256 记录在
`tools/sources.lock`（上游改过数据时会报出来）。
**`emoji/*` 是 GPL-3.0-only**：本仓库不分发它们，也不把它们的任何内容
编译进已跟踪的产物；使用者若再分发自己 `build/` 下的副本，
需自行满足 GPL-3.0 的义务。

**生成的 `.dict.yaml` 进仓库**：它是 MIT / Apache-2.0 数据的派生产物，
且"克隆下来就能打字"需要它。它**确实包含第三方派生数据**——
不要再说"本仓库不包含第三方词典数据"（旧 README 的说法与事实不符）。

**雾凇（rime-ice）那 44 MB 词表仍然不进仓库**：仓库许可是 GPL-3.0，
而里面最大的两块（腾讯词向量、`base`）**来源不明或明确限制**——
"来源不明"比 GPL 难处理（PLAN §10）。

---

## 多音字：**只取首选读音**，以及一次真实的编错事故

词级拼音不在任何一份可分发数据里（`pinyin.txt` 只有**单字**读音）。
生成器现在的策略只有一条：**取 `pinyin.txt` 每行列表的首个**，即字典口径的
首选读音（`U+5BB6: jiā,jia,jià,jie,gū  # 家` → `jia`）。

### 曾经不是这样，代价是 838 个多音字

旧版在第 1 条之后还有第 2 条："在**同声母**的候选里，按该读音在单字表里
出现的次数微调"。它的动机是「血」xue/xiě 这类字，但用的统计量讲的是
**音节**有多常见，不是**这个字**读哪个音。实测代价：

```text
$ grep -m1 '^家' schemes/qingjian-default/cn_dicts/generated.dict.yaml
家	jie	41023      ← 「家」读 jia。jie 在单音字里出现 238 次，jia 只有 136 次
```

后果是连锁的：`jie` 的首选变成「家」，`nihaoshijie` 被动态规划拼成
「你好**是家**」。整份词表里有 **838 个多音字**被这条规则改离了首选读音。
这条启发式已经**删除**，不是调参：单字表里没有"这个字在词里读什么"的
信息，任何只靠单字表的"更聪明"算法都还是在猜。

### 守住它的两道门

1. **生成器的单测**（`tools/wordlist-gen/src/main.rs`）：
   `a_more_common_syllable_does_not_hijack_a_characters_primary_reading`
   用「家」的最小复现把"音节频率不能覆盖首选读音"钉住；
2. **落盘前的自检**：`verify_primary_codes` 拿 `pinyin.txt` 的首选读音
   **独立重算**每条词条的编码并逐字对照，不一致就中止、不写文件
   （产物头部也写明这件事）。`家 → jie` 那份产物能解析、能装载、能打字
   ——只是打出来的字不对，所以"能被解析"挡不住它。

集成验收在 `crates/qingjian-schemes/tests/word_pinyin_quality.rs`
（含原先 `#[ignore]` 的那条，现已转正）。

### 剩下的缺口还是那句话

**词级**读音我们仍然没有：「银行」`yinhang` 这类词要靠人工覆盖表
`schemes/qingjian-default/cn_dicts/word_pinyin.override.dict.yaml` 兜，
表外的多音字词仍然只能按单字首选读音拼。要整体修掉，需要一份
**可分发、许可清楚**的词级拼音数据源（审计 §2.I 第 2 条）——
它不在当前的许可清单里。

---

## 自检（"生成器写出来的东西装载器读不懂"）

生成器在落盘前会**用真正的装载器**（`qingjian-dict`）把自己写出来的词库
解析一遍。这条检查抓到的第一个错是：YAML 头部的 `import_tables` 列表项
被续行吃掉了缩进，于是装载器报「同一层里混用了「键: 值」与「- 列表项」」。
**旁路从来不坏，也从来不证明什么**——用真解析器是这里唯一有意义的选择。

### 两条落盘前自检，分别抓不同的错

| 自检 | 抓什么 | 抓不到什么 |
| --- | --- | --- |
| `qingjian_dict::parse_dict` | 产物**读不回来**（YAML 结构错、重复词条、编码单元不在字母表里） | 读得回来但**字不对** |
| `verify_primary_codes` | 每个字的编码 ≠ `pinyin.txt` 的首选读音 | YAML 结构错（它只看 `lines`） |

`家 → jie` 那份产物**能过第一条**——它能解析、能装载、能打字；
第二条才会指出"「家」的首选读音是 `jia`"。所以"能被解析"不能当作
"生成对了"。

### 本目录的两个 workspace 门禁

`tools/wordlist-gen` 是**独立的 workspace**（不在根 `Cargo.toml` 的 members 里），
所以根目录的 `cargo fmt --all --check` / `cargo clippy --workspace` 覆盖不到它。
本轮顺手把它也做干净了，改动生成器后请自己跑一遍：

```bash
cargo fmt   --manifest-path tools/wordlist-gen/Cargo.toml -- --check
cargo clippy --manifest-path tools/wordlist-gen/Cargo.toml --all-targets -- -D warnings
cargo test  --manifest-path tools/wordlist-gen/Cargo.toml
```
