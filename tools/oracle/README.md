# Oracle：拿上游实现做逐字节对照

这里放的是**上游函数实际跑出来的输出存档**（`*.expected.txt`），
以及用 `luajit` 重新生成它们的**配方**（上游 URL + 固定 revision）。
用途只有一个：给 qingjian 的 Rust 重写一个**可复核**的判据——
"我们与上游一致"这句话必须能被重新跑一遍，而不是靠记忆。

## 为什么需要它

有些零件的行为**没有权威标准可对照**：

| 零件 | 为什么没有标准 |
| --- | --- |
| `number_translator` | 中文数字/金额大写是**会计习惯** |
| `calc_translator` | 表达式语义与**数字显示格式**都是上游的选择 |

这种情况下唯一诚实的判据是"与上游逐字节一致"。而"一致"必须能被验证，
否则它只是又一句没有证据的话。

## 这里有什么、没有什么（阶段 4 / 审计 J2.3 之后）

| 文件 | 是什么 | 许可 |
| --- | --- | --- |
| `calc_translator/calc.expected.txt` | 上游 `calc_translator.lua` 的**输出记录** | 事实/数据，随本仓库分发 |
| `number_translator/number_to_chinese.expected.txt` | 上游 `number_translator.lua` 的**输出记录** | 同上 |

**这里不再放 `.lua` 源码。** 旧的 `calc.lua` / `number_to_chinese.lua` /
`calc_probe.lua` 是从 rime-ice（**GPL-3.0-only**）复制/改写的纯计算副本。
把一个 GPL 部件放进 MIT / Apache-2.0 的仓库并一起分发，会把整份分发拖入
GPL（PLAN §10 已经把这条规则写清楚了）——**所以源码被移除了**，
只留下测试真正依赖的输出存档。

保留 `.expected.txt` 是安全的：它是"跑出来的结果"，不是代码；
`crates/qingjian-engine/tests/{number_oracle,calc_oracle}.rs` 读的也只有它。

## 怎么用

```bash
# 跑对照测试（只需要 .expected.txt，不需要任何 .lua）
cargo test -p qingjian-engine --test number_oracle --test calc_oracle

# 想重新生成对照数据（需要 luajit；**在你自己的机器上做，不要提交**）：
#   ① 取回上游那两份 Lua（固定 revision，落在 gitignore 的 .work/ 下）
#      rime-ice @ 859e3b5300e0ea01334a627b15db101e94312a75
RI=.work/rime-ice-oracle
mkdir -p "$RI"
curl -fsSL -o "$RI/number_translator.lua" \
  https://raw.githubusercontent.com/iDvel/rime-ice/859e3b5300e0ea01334a627b15db101e94312a75/lua/number_translator.lua
curl -fsSL -o "$RI/calc_translator.lua" \
  https://raw.githubusercontent.com/iDvel/rime-ice/859e3b5300e0ea01334a627b15db101e94312a75/lua/calc_translator.lua
#   ② 按 `tools/oracle/*/*.expected.txt` 的表头格式，用 luajit 生成对照行
#      （上游文件带 RIME 的 env/yield 接口，需要自己剥掉——这一步是
#       一次性的、本地的工作，不属于仓库内容）
```

**不要把生成用的 `.lua` 或中间文件提交回仓库**：那正是本节要避免的事。
上游 URL 与 revision 已经写死，需要时随时能取回同一份。

## 三件事必须说清

1. **这些 `.expected.txt` 不参与构建、不进二进制**（没有 `include!`、
   不在 `Cargo.toml` 里）。它们是**实验记录**，由测试读取。
2. **它们在 `tools/` 而不是 `crates/`**：内核 crate 不许有数据文件
   （`verify-no-scheme-data.sh` 的门禁，D24）。它抓到过我一次。
3. **上游那两份 Lua 是 GPL-3.0-only，本项目不分发它们**；
   完整许可边界见 `THIRD_PARTY_NOTICES.md`。

## 它抓到了什么

写 `number_translator` 时，对照测试抓出 6 处"我以为理所当然"的错误，
其中一条是：**Lua 的 `gsub` 默认只替换第一处**，而我按"全局替换"
实现了它——因为那是这个名字给我的印象。上游连写两遍同一个 `gsub`
恰好是在**依赖**这个性质。

写 `calc_translator` 时抓到 5 处，其中三处是**我们比 Lua 更宽松**
（`--3`、`1+2)`、`sin(1,2)` 的多余参数）——"更宽松"也是一种不一致。
