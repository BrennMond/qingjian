# 阶段 2 验收证据：解码与会话闭环

> **对应**：`docs/INDEPENDENT_AUDIT_AND_DEEPSEEK_PLAN.md` §4「阶段 2」。
> **修复前的证据**来自 `git worktree add .work/stele-base 4a2bb1a`，
> 用**同一批测试文件**跑出失败。复现方式见 `docs/validation/phase-1.md` §0。
>
> **本页同时列出本阶段没做的事**——阶段 2 的任务包里有几项**尚未完成**，
> 它们没有被算进"已交付"。

---

## 0. 门禁

```bash
cargo fmt --all -- --check                                                  # ✓
cargo clippy --workspace --all-targets --offline --locked -- -D warnings     # ✓
cargo test --workspace --offline --locked                                    # ✓ 34 个测试目标全绿
for f in scripts/verify-*.sh; do bash "$f"; done                             # ✓ 四项门禁
target/release/stele --check                                                 # ✓ 8 组不变式
```

---

## 1. 任务包 E①：可解释前缀（不完整输入）

### 修复前

```text
$ cd .work/stele-base && cargo test -p stele-schemes --test decoder_matrix --offline
test niha_gives_the_word_for_the_interpreted_prefix ... FAILED
缩写开时 niha 必须出「你好」，实得 ["niha"]
test nihaoshijie_gives_prefix_candidates_and_a_composition ... FAILED
必须给可解释前缀的候选，实得 ["nihaoshijie"]
test result: FAILED. 5 passed; 7 failed
```

**与审计 §2.E 的表完全一致**：`niha`、`nihaoshijie` 只给字面量。

### 修复后

```text
$ cargo test -p stele-schemes --test decoder_matrix --offline
test niha_gives_the_word_for_the_interpreted_prefix ... ok
test nihaoshijie_gives_prefix_candidates_and_a_composition ... ok
test result: ok. 12 passed
```

### 做了什么

- `stele_core::Spelling` 新增 [`expand_paths`]：产出**带消费字节数**的路径。
  与 `expand` 的唯一差别是"**每一个到达过的位置都算一条路径**"，
  而不是只收"恰好消费完整串"的那些。
  这正是 librime 的 `interpreted_length < input_length`
  （`algo/syllabifier.cc:267-274`）。
- `Candidate.span` 按它**本来的定义**填成"消费掉的那一段输入"：
  `niha` 的「你好」是 `0..3`，余码 `a` 留在输入里。
  旧实现把它写死成整段 span，信息在那一刻就丢了。
- 新增**余码罚分**（每未消费字节 −4000 毫对数）。
  没有它会出现一个真踩到的坑：单字「你」的词条权重远高于词「你好」，
  于是"敲了 5 个字母"的第一个候选变成了「你」。

### 断言的不只是"有候选"

`decoder_matrix.rs` 对每条候选同时检查**文本、消费范围、余码、属性**：

```rust
let nihao = find(&c, "你好")?;
assert_eq!(nihao.span, Span::new(0, 3));       // 只消费前 3 个字节
assert_eq!(remainder(input, nihao), "a");      // 余码留在输入里
assert!(nihao.attr.contains(SpellingAttr::ABBREV));
```

---

## 2. 任务包 E②：拼写层补全

### 修复前的 2×2 矩阵（审计 §7 的四格）

```text
test the_four_cell_matrix_matches_the_reference ... FAILED
  缩写开 + 补全开：`niha` 出「你好」应当是 true，实得 false；候选 = ["niha"]
test the_target_candidates_are_present_not_just_the_literal ... FAILED
  `niha`（缩写=true 补全=false）必须出「你好」，实得 ["niha"]
```

审计记录的是"stele 四格全不命中"。实测确认。

### 修复后

```text
test the_four_cell_matrix_matches_the_reference ... ok
test the_two_completion_paths_are_distinguishable ... ok
```

| 缩写 | 补全 | 上游 | 修复前 | 修复后 |
| --- | --- | --- | --- | --- |
| 开 | 开 | 你好 | 字面量 | ✅ 你好（消费 3/4，余码 `a`） |
| 开 | 关 | 你好 | 字面量 | ✅ 你好（同上） |
| 关 | 开 | 你好 | 字面量 | ✅ 你好（消费 4/4，attr=COMPLETION） |
| 关 | 关 | 你 尼 泥 | 字面量 | ✅ 你（消费 2/4，余码 `ha`） |

两条通路**用属性与消费范围区分**（不是靠 preedit——上游专门提醒过这一点），
`the_two_completion_paths_are_distinguishable` 把两者都钉住。

### 做了什么

- 补全移到**拼写层**：`PathLimits::completion` 允许最后一条边
  "吃掉没敲完的尾巴"（`ha` 是 `hao` 的前缀），消费到输入末尾并标 COMPLETION。
  上游是 `Prism::ExpandSearch`（`algo/syllabifier.cc:224-228`）。
- **`default_completion()` 由 `false` 改成 `true`**，并把那段与上游不符的
  注释（"RIME 的默认也是关"）改成带源码行号的引用。
  审计 §2.E 第 7 条与 §G5 都点到了这一条。
- 词条补全（`prefix_lookup`）加上上游的第二个门槛
  `consumed == end_of_input`（`gear/script_translator.cc:461-464`）。

---

## 3. 任务包 E③：词图造句

### 修复前

```text
test haoni_is_composed_by_sentence_making ... FAILED
  必须造句出「好你」，实得 ["haoni"]
```

### 修复后（真实默认词库，41 万词条）

```text
$ ./target/release/stele --scheme-dir schemes/stele-default --candidates=all haoni
  14. 好你       score=-2599     origin=Sentence

$ ./target/release/stele --scheme-dir schemes/stele-default --candidates=all woaizhongguo
   1. 我爱中国     score=-5218     origin=Sentence
```

### 做了什么（第一版，**不接语言模型**）

- 对每个可达起点各展开一次（小预算 `PathLimits::sentence_scan()`），
  得到词图边 `(start, end, text, score, units)`；
- 在词图上做**有界动态规划**：边严格向前 ⇒ 无环 ⇒ 一次正向扫描；
- 得分 = Σ词条分数 + 单元奖励 − **词数罚分** − 句子罚分；
- **整串就是一个词时不算句子**（上游 `gear/poet.cc:206-208` 同款排除，
  有测试钉住）；
- 词数上限 `MAX_SENTENCE_WORDS = 8`。

**词数罚分**是实测加上的：没有它时 `nihaoshijie` 会拼成「你好**是界**」
而不是「你好**世界**」——因为单字「是」的词条权重远高于词「世界」。
上游用"短语词条的额外权重"解决，我们没有那份数据，于是等价地在 DP 里
惩罚词数（覆盖同一段输入时**词越少越好**）。

### 一个真踩到的顺序坑

造句的触发条件是"没有覆盖整串的可靠词条"，所以它只能在看完展开之后决定；
但候选缓冲有上限（`TRANSLATE_CAP = 200`）。先铺开一百多条前缀/补全候选，
**造句结果会被挤掉、静默消失**：真实词库下「好你」与「我爱中国」
一开始根本不出现。修法是先做一次"命中即停"的预扫拿到 `covered_whole`，
把至多一条造句候选先放进缓冲。

---

## 4. 任务包 E⑥ + G2：`xlit` 语义修正

### 修复前

`spelling.rs` 的 `apply_rule` 对 `Rule::Xlit` **同时**推进原拼写与转写结果
——等价于把 `xlit` 当成了派生。而上游 wiki 与 librime
`Transliteration::Apply` 的语义是**改写**（原拼写失效）。

**这同时是一处"实现与自己文档矛盾"**：同文件里 `Rule::Equivalence` 的
文档一直写着「`xlit` 是改写（原拼写失效）」。

### 修复后

```rust
#[test]
fn xlit_is_a_rewrite_not_a_derivation() {
    let t = SpellingTable::compile(alphabet(&["aa"]), &[Rule::parse("xlit/a/b/").unwrap()]);
    assert!(t.looks_up("bb"), "转写结果必须有效");
    assert!(!t.looks_up("aa"), "`xlit` 是**改写**：原拼写不再有效");
    assert_eq!(t.edge_count(), 1);
}
```

---

## 5. 任务包 G4：两台词库实现的能力一致

### 缺口是怎么第二次出现的

阶段 2 加 `Lexicon::has_prefix` 时，`TableLexicon` **声明了**
`supports_prefix() == true` 却**没有实现** `prefix_lookup`
（trait 的默认实现是空的）。后果与审计 §2.G4 描述的**一模一样**，
只是换了个触发点：

```text
# 修复前：同一个 shape 方案、同一个输入 `ab`，两条装载路径给出不同候选
$ stele --schema shape --candidates=all ab            # 走内嵌（内存词库）
方案 shape  输入 "ab"  候选 4 个
  1. 木  2. 十  3. 才  4. ab
$ stele --scheme-dir schemes/stele-default --schema shape --candidates=all ab
方案 shape  输入 "ab"  候选 2 个
  1. 十  2. ab
```

它还是 `stele --check` 报出来的：

```text
内核自检失败：1
  - shape/ab 应当上屏「十」，得到「木」
```

### 修复后

- 实现 `TableLexicon::prefix_lookup`：**前缀是连续区间**，
  二分下界 + 顺序扫到不再匹配为止；与内存实现**逐字段一致**
  （文本、分数、`attr=COMPLETION`、`kind=Completion`）。
- 新增 `crates/stele-schemes/tests/lexicon_capability.rs`：
  对两台实现的 **`supports_prefix` / `lookup` / `has_prefix` /
  `prefix_lookup`（两种 `exclude_exact`）**逐项对照，
  8 组编码 × 4 项能力，任何一处分叉都会红。
- 顺带修掉一个**语义错误**：`ExactCodeTranslator` 没有给补全候选扣分，
  于是精确编码方案里"打全的「十」排在没打全的「木」后面"。
  现在两个翻译器共用同一个 `COMPLETION_COST`。

```text
$ target/release/stele --check
内核自检通过：8 组不变式全部成立。
```

---

## 6. 任务包 G5 的一个实例：解析 ≠ 生效

修 `enable_word_completion` 时发现：**短写法装配路径根本没读 `translator:` 段**。

```rust
// scheme.rs：`engine.translators` 为空时（即 `engine.translator: spelling_graph`）
TranslatorKind::SpellingGraph => Box::new(SpellingGraphTranslator::new(...)),  // ← 没有 with_completion
```

于是 `translator: { enable_word_completion: true }` 被**解析、存进 spec、
然后丢掉**。这正是审计 §2.G5「解析不等于生效」的形态。
已改为两条装配路径都消费同一份 spec。

> 这一条也说明 **阶段 3 的"配置字段审计表"必须做**：
> 这次是靠"改一个默认值、看行为有没有变"才发现的，不是靠静态检查。

---

## 7. 性能（release，真实词库，缓存已命中）

### 阶段 3–4 之后的复测（含词级读音覆盖表）

```text
$ ./target/release/stele-bench --scheme-dir schemes/stele-default --iterations=30000
输入                                   P50         P95         P99         max
  nihao                         105.86µs    138.40µs    153.02µs    188.63µs
  nihaoshijie                   618.53µs    932.16µs    960.76µs      1.51ms
  nh                            234.65µs    262.06µs    274.82µs    944.74µs
  nhao                          444.66µs    702.50µs    722.61µs      1.29ms
  ssss                          494.04µs    613.53µs    637.09µs    825.50µs
  woaizhongguo                  560.71µs      1.83ms      1.96ms      2.55ms

常驻内存（VmRSS）: 14148 KiB (13 MiB)
常驻内存（VmHWM，进程峰值）: 21424 KiB (20 MiB)
```

**所有红线仍然成立**：P50 红线 1 ms（实测最坏 618 µs），
P99 红线 10 ms（实测最坏 1.96 ms），常驻红线 30 MB（实测 13 MiB）。
词级读音覆盖表（几十条高权重条目）**没有**改变性能量级。

### 与阶段 1 的对比（**必须申报的一次回退**）

| 输入 | 阶段 1 P50 | 现在 P50 | 变化 |
| --- | --- | --- | --- |
| `nihao` | 20.6 µs | **105.9 µs** | **5.1× 慢** |

原因明确：`expand_paths` 会为**每一个到达过的位置**产出一条路径
（`nihao` 从 116 条变成上限 512 条），每条都要查一次词库——
这是"支持不完整输入"的直接代价，不是实现失误。

**下一步的正当优化**（不是"把上限调小"）：把词典约束**放进搜索内部**
（不可能命中词条的分支根本不展开），而不是像现在这样"先展开、再查表"。
`has_prefix` 已经就位并且两台词库都实现了，接进 `walk` 需要让 arena 节点
能增量回答"我的编码还是不是某个词条的前缀"——那是**下一阶段的第一项**。

## 8. 本阶段**没有**做完的事

阶段 2 的任务包共 6 项，交付了 4 项（E①②③、E⑥/G2、G4、G5 的一个实例），
下面两项**未完成**：

### 8.1 任务包 F：会话分段、部分选词、余码保留、重开

**部分完成。** 已交付的是**余码保留**这一半：

```text
$ cargo test -p stele-schemes --test decoder_matrix --offline
test committing_a_prefix_candidate_keeps_the_remainder_in_the_input ... ok
test committing_a_whole_input_candidate_clears_the_input ... ok
test the_remainder_is_reanalysed_after_a_partial_commit ... ok
```

`finish_commit` 现在接收"这次上屏消费了多少字节"：候选的 `span` 小于输入
长度时，`[consumed, len)` 留在输入里继续打，并**立刻按余码重算候选**。
旧实现无条件 `composition.reset()`，等于把用户敲的余码丢掉。

**仍然没有做的**：

- **逐段确认**（一次上屏确认一段、其余仍在预编辑里）；
- **重新打开已确认段**（RIME 的 `Reopen`）；
- 候选覆盖**任意 span**（现在只支持"从头消费到 consumed"）——
  这与"任意位置的分段"是两个不同的能力；
- 光标位置参与编辑。

预编辑串的**显示**也仍是整串回显（见下面那条"已知缺口"守门员测试）：
`segment_for_display` 对整串输入切分，不认识"缩写边只吃一个字符、
余码不算已消费"。**候选的消费范围是对的**（有测试），错的只有显示。

### 8.2 任务包 F 的状态机测试：标点、编辑器、中英/数字混输

**标点已完成（决定 + 测试）**，见 `crates/stele-schemes/tests/punctuation_semantics.rs`：

| 敲标点时 | 行为 | 理由 |
| --- | --- | --- |
| 正在拼写（`ni`） | `Rejected` + 预编辑串原样保留 | 别让一个逗号吞掉半截的词 |
| 没有拼写 | `Consumed` + 原样候选进入输入串 | 标点是用户要打的内容 |

这是**有意与 Rime 不同**的产品语义（Rime 是 `ni,` → 你，），
理由与前端契约写在那份测试的文档注释里——审计要求的是"明确决定并测试"，
而不是"含糊过去"。

**端到端状态转换表现已建立**：`crates/stele-schemes/tests/session_state_machine.rs`
覆盖 10 条交互（部分选词后继续输入、选第二段、Backspace/Delete/Esc、
数字选词、中英开关、预测与数字选择的隔离），跑法含被忽略项：

```bash
cargo test -p stele-schemes --test session_state_machine -- --include-ignored
# 11 passed; 0 failed
```

**仍然未做**：

- **重开已确认段**——审计明确要求的能力。内核一旦把文本交给前端就不再
  持有它，而 `Session` 上没有"把这段放回来继续编辑"的入口。
  有一条 `#[ignore]` 的测试把这条缺口**显式记录**为将来的验收点；
- URL / 数字**识别器**与候选选择的交叉矩阵（识别器本身在
  `regex_and_recognizer.rs` 里有覆盖）。

### 8.3 造句子系统的其它边界

- 没有语言模型，只有词频 + 长度策略（审计允许，但要知道这是上限）；
- `MAX_SENTENCE_WORDS`、词数罚分、单元奖励三个常数是**实测反推的第一版**，
  没有质量评测集（阶段 4 的第 4 项）来校准。

### 8.4 `enable_sentence`

按要求"按清晰的方案语义实现或明确拒绝"：**拼音族不看它**（与上游一致，
`script_translator` 没有这个开关），字段文档已改写并写明归属。
码表族的消费点**尚未实现**（`ExactCodeTranslator` 不造句）——
这是**明确的未支持**，不是"解析了没生效"。

---

## 9. 阶段 3 的交接点

1. **词典约束搜索进搜索内部**（§7 的性能回退的正解）；
2. 多 translator 实例独立词库（审计 §2.G3，本阶段未动）；
3. **配置字段审计表**（§6 是它的一个实例，说明它必须做）；
4. 会话状态机与余码（§8.1）；
5. 一个坏 schema 的目录加载策略（§2.G1）。
