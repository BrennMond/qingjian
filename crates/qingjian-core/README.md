# qingjian-core

Qingjian IME 的抽象层。

**这个 crate 有两条硬性约束**（见 `PLAN.md` 的 D9 与 D20）：

1. **零第三方依赖。** 它是供应链安全的锚点，也是"平台无关代码能纯
   `cargo test`"这条铁律的前提。CI 上有脚本 `scripts/verify-zero-deps.sh` 强制它。
2. **不含任何输入法专属知识。** 只允许出现"编码 / 拼写 / 编码单元 / 字母表"
   这套通用词汇；不允许出现"拼音 / 音节 / 声母 / 韵母 / 简拼 / 模糊音"。
   CI 上有脚本 `scripts/verify-no-ime-vocab.sh` 强制它。

第二条不是洁癖：如果内核里出现"音节"，就等于宣布所有方案都必须说拼音——
而仓颉方案里一个"音节"只是一个字母，五笔方案里根本不需要切分图。
详见 `docs/engine-design.md` §2.4。

## 内容

| 模块 | 内容 |
| --- | --- |
| `score` | `Score` —— 对数域**定点整数**分数，跨平台可复现 |
| `key` | `Key` / `KeyCode` / `Modifiers` —— 平台无关按键 |
| `candidate` | `Candidate` + 两个正交轴：`Origin`（从哪来）、`SpellingAttr`（怎么拼的） |
| `commit` | `Commit` / `Outcome` —— 上屏信息由返回值带出，不可能漏读 |
| `context` | `Context` —— 已上屏内容的滚动窗口（下一词预测的唯一输入） |
| `segment` | `Segment` / `Segmentation` / `Composition` |
| `option` | `Options` —— **引擎不预设任何开关名** |
| `component` | 五类骨架组件 + 兜底翻译器 + `Query` |
| `service` | `Lexicon` / `Spelling` / `Ranker` / `Clock` / `MemoryStore` |
| `session` | `Engine` / `Session` / `SchemaCatalog` 两级拆分 |
| `sort` | 排序的**唯一实现** + "精确优先"守卫 |
| `error` | 加载期错误与**结构化诊断** |

## 权威文档

`docs/engine-design.md`。本 crate 的每个类型在那里都有"为什么这么定"与
"改动代价有多高"的说明。

## 测试

```bash
cargo test -p qingjian-core
cargo clippy -p qingjian-core --all-targets
```

其中 `sort::tests::sorting_is_reproducible_over_many_runs` 直接对应
工程铁律第 2 条（可复现）——**它失败就意味着排序退化成了不确定的**。
