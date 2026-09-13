# qingjian × librime 对照报告

| | |
| --- | --- |
| librime | 1.16.1 / `luna_pinyin`（`--reset` 冷启动基线） |
| qingjian | `pinyin`（内嵌默认方案） |
| 比什么 | **结构**：能否上屏、按键是否被处理、切分边界 |
| 不比什么 | 候选排序与分数——两边的词库与语言模型不同，比排序等于比词库 |

## `nihao` — full-spelling

- ✓ 都上屏（librime='你好' qingjian='你好'）

## `ni` — single-unit

- ✓ 都上屏（librime='你' qingjian='你'）

## `nh` — abbreviation

- ✓ 都上屏（librime='你會' qingjian='你好'）

## `,` — punctuation

- ✓ 都是全角标点（librime='，' qingjian='，'）

## `uUni` — prefix-not-configured

- ✓ 全部按键都被处理

## `zhongguo` — typing-progresses

- ✓ 全部按键都被处理

---
**全部一致**（在「结构行为」这个范围内）。

> 这份报告**不断言候选排序一致**，因为两边的词库不同——
> 比排序比的是词库，不是引擎。要比排序，得先让两边吃同一份词表
> （P3.5 的默认方案做完之后可以再补一份）。
