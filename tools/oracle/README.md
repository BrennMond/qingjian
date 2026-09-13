# Oracle：拿上游实现做逐字节对照

这里放的是 **rime-ice 那套 Lua 函数的纯计算副本**，以及用 `luajit`
跑出来的实际输出。用途只有一个：给 stele 的 Rust 重写一个**可复核**的
判据——"我们与上游一致"这句话必须能被重新跑一遍，而不是靠记忆。

## 为什么需要它

有些零件的行为**没有权威标准可对照**：

| 零件 | 为什么没有标准 |
| --- | --- |
| `number_translator` | 中文数字/金额大写是**会计习惯** |
| `calc_translator` | 表达式语义与**数字显示格式**都是上游的选择 |

这种情况下唯一诚实的判据是"与上游逐字节一致"。而"一致"必须能被验证，
否则它只是又一句没有证据的话。

## 怎么用

```bash
# 重新生成对照数据（需要 luajit）
luajit tools/oracle/number_translator/number_to_chinese.lua \
  > tools/oracle/number_translator/number_to_chinese.expected.txt
luajit tools/oracle/calc_translator/calc.lua \
  > tools/oracle/calc_translator/calc.expected.txt

# 跑对照测试
cargo test -p stele-engine --test number_oracle --test calc_oracle
```

## 三件事必须说清

1. **这些 `.lua` 不参与构建、不进二进制**（没有 `include!`、不在
   `Cargo.toml` 里）。它们是**实验记录**。
2. **它们在 `tools/` 而不是 `crates/`**：内核 crate 不许有数据文件
   （`verify-no-scheme-data.sh` 的门禁，D24）。它抓到过我一次。
3. **想彻底避开 GPL**：删掉 `.lua` 与本文档的生成说明，保留
   `.expected.txt`——那是**输出事实**，不是代码，对照测试照常工作。

## 它抓到了什么

写 `number_translator` 时，对照测试抓出 6 处"我以为理所当然"的错误，
其中一条是：**Lua 的 `gsub` 默认只替换第一处**，而我按"全局替换"
实现了它——因为那是这个名字给我的印象。上游连写两遍同一个 `gsub`
恰好是在**依赖**这个性质。

写 `calc_translator` 时抓到 5 处，其中三处是**我们比 Lua 更宽松**
（`--3`、`1+2)`、`sin(1,2)` 的多余参数）——"更宽松"也是一种不一致。
