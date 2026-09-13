# 词库管线（P3.5）

这个目录里的东西解决一件事：**把"干净来源"的公开词表编成石经能装载的
`.dict.yaml`**，并让"这份数据是怎么来的"可以被重新跑一遍。

它们**不进内核 crate、不进 CI**（与 `tools/` 里其它工装一样）：
换词库是"使用者给判断"的事，不是单元测试能断言的。

---

## 两步

```bash
# ① 取回源数据（一次网络访问；落进 schemes/stele-default/build/，该目录被 gitignore）
bash tools/fetch-sources.sh

# ② 编成词库 + 同步方案的音节表
cargo run --release --manifest-path tools/wordlist-gen/Cargo.toml -- \
    --sources schemes/stele-default/build --out schemes/stele-default
```

产物落在 **`schemes/stele-default/cn_dicts/generated.dict.yaml`**，
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
| `emoji/*` | [`iDvel/rime-ice`](https://github.com/iDvel/rime-ice) 的 `opencc/` | Apache-2.0 | 中文 → emoji 转换表 |

**为什么源数据不进仓库**：这九份都是可分发的（MIT / Apache-2.0），
但"数据的来源与许可"是使用者要能**自己核对**的东西，所以 `build/`
在 `.gitignore` 里，由 `fetch-sources.sh` 按需取回、并用 sha256 记录在
`build/sources.lock`（上游改过数据时会报出来）。

**生成的 `.dict.yaml` 进仓库**：它是 MIT 数据的产物，且"克隆下来就能打字"
需要它。

**雾凇（rime-ice）那 44 MB 词表仍然不进仓库**：仓库许可是 GPL-3.0，
而里面最大的两块（腾讯词向量、`base`）**来源不明或明确限制**——
"来源不明"比 GPL 难处理（PLAN §10）。

---

## 多音字：这里有一条**假设**，写在这里而不是藏在代码里

词级拼音不在任何一份可分发数据里（`pinyin.txt` 只有**单字**读音）。
生成器的策略是：

1. 主读音取 `pinyin.txt` 里的**第一个**（字典习惯）；
2. 再在**同声母**的候选里，按"该读音在单字表里出现的次数"微调。

**它不会把「银行」读成 `yín háng`**——那需要一份带拼音的词库。
想改策略，见 `wordlist-gen` 里的 `ReadingPolicy`；产物头部会如实写明
本次用的是哪一种。

---

## 自检（"生成器写出来的东西装载器读不懂"）

生成器在落盘前会**用真正的装载器**（`stele-dict`）把自己写出来的词库
解析一遍。这条检查抓到的第一个错是：YAML 头部的 `import_tables` 列表项
被续行吃掉了缩进，于是装载器报「同一层里混用了「键: 值」与「- 列表项」」。
**旁路从来不坏，也从来不证明什么**——用真解析器是这里唯一有意义的选择。
