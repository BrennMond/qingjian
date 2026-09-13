# librime-probe —— 真实 librime 的按键行为基线探针

## 这是什么

一个独立的小 C 程序，用来驱动**真实的 librime**（不是 Rust 侧的模拟实现），把逐键的
候选、preedit、上屏结果记录成机器可读的 JSON。用途是给 Qingjian IME 提供一份
「上游 librime 到底怎么反应」的对照基线：同一个按键序列，librime 出什么候选、
什么 preedit、什么时候上屏，都可以拿这里的输出逐字段比对。

这个目录**完全独立**于 Rust 代码：不引用任何 crate、不改任何 Rust 文件、不参与
Cargo 构建。删掉整个目录对仓库其它部分没有任何影响。

## 为什么要手抄 ABI（本机没有 librime-dev）

本机装的是运行时包 `librime1t64` 1.16.1：

- 有 `/usr/lib/x86_64-linux-gnu/librime.so.1`（指向 `librime.so.1.16.1`）
- **没有** `/usr/include/rime_api.h`
- **没有** `librime.so` 这个开发用符号链接
- `librime-dev` 装不上（sudo 需要密码，本任务不允许提权）

所以 `probe.c` 的做法是：`dlopen("librime.so.1")` + `dlsym`，并在文件里按
`.scratch/rime_api.h`（取自 librime tag 1.16.1）逐字段重抄所需的结构体。
链接行里**没有** `-lrime`，因为没有 `librime.so` 可链；只需要 `-ldl`。

### 抄的时候实测出来的四个坑

这些不是推测，都是在本机跑出来的，写在这里免得下次再踩。

**1. 这个构建根本没有导出 C 名字的 `RimeSetup` / `RimeProcessKey`。**

`nm -D` 显示它们只以 C++ mangled 名字存在：

```
$ nm -D --defined-only /usr/lib/x86_64-linux-gnu/librime.so.1 | grep -c '^.* T RimeProcessKey$'
0
$ nm -D --defined-only /usr/lib/x86_64-linux-gnu/librime.so.1 | grep RimeProcessKey
0000000000091d40 T _Z14RimeProcessKeymii
$ ./dlsymtest
RimeProcessKey                   -> absent
rime_get_api                     -> FOUND
```

未修饰的 C 导出只有三个：`rime_get_api`、`rime_get_api_stdbool`、
`RimeRegisterModule`、`RimeFindModule`。

因此 probe **只用 `rime_get_api()`**，拿到那张版本化的函数指针表 `RimeApi`，
之后所有调用都走 `api->process_key(...)` / `api->get_context(...)`。
这条路也正是 `rime_api.h` 自己推荐的入口，比 dlsym 一堆 mangled 名字稳得多。

**2. `RIME_STRUCT_INIT` 在 1.16.1 里不是 `sizeof(Type)`。**

头文件里写的是：

```c
#define RIME_STRUCT_INIT(Type, var) \
  ((var).data_size = sizeof(Type) - sizeof((var).data_size))
```

即 `data_size = sizeof(结构体) - sizeof(int)`，**不是**早期版本的 `sizeof(结构体)`。
配套的 `RIME_STRUCT_HAS_MEMBER` 判断是
`sizeof(data_size) + data_size > (char*)&member - (char*)&var`。
probe 照抄了这一对宏，没有自己发挥。

> 顺带修掉一个常见的记忆错误：网上（以及不少旧代码里）流传的
> 「flavored struct 的 data_size 要填 `sizeof(struct)`」是**旧约定**。
> 在 1.16.1 上按旧约定填也能跑（因为库只会认为字段"都在"），
> 但和头文件不一致，属于靠巧合工作。这里按头文件来。

`--check-layout` 会把这个自检打出来：

```
$ ./probe --check-layout
sizeof(RimeApi)     = 792
offsetof(RimeContext, select_labels)      = 80
RimeContext.data_size after RIME_STRUCT_INIT = 84
HAS_MEMBER(ctx, select_labels)               = 1
库返回的 RimeApi.data_size = 788（本文件期望 788）
库版本: 1.16.1
布局自检: 通过
```

其中 `788 == sizeof(RimeApi) - 4` 是**最硬的一条外部校验**：`data_size` 是
librime 自己填的，它等于我们手抄结构体的大小，说明 `RimeApi` 那张函数指针表的
字段个数和顺序与库里的完全一致。错一个字段这个数就对不上。

**3. 真正的 struct 布局错误会被 `select_labels` 抓住。**

`RimeContext.select_labels`（offset 80）是 1.16.1 里最靠后的字段，只要
`data_size` 约定错了，库就会认为该字段不存在、把它留成 NULL。
本机 `bopomofo` 方案在 `schema.yaml` 里定义了
`alternative_select_labels: ['⇧1'…'⇧0']`，正好可以验证：

```
$ ./probe --schema bopomofo --keys "su3" | head -2 | grep -o '"select_labels":\[[^]]*\]'
"select_labels":["⇧1","⇧2","⇧3","⇧4","⇧5"]
```

拿到 5 个真实标签 ⇒ offset 80 和 `data_size` 约定都是对的。
（`luna_pinyin` / `cangjie5` 没定义这个字段，所以它们正常输出 `[]`，
不是 bug。）

**4. `dlclose()` 之后进程退出必崩。**

清理时如果调用 `dlclose(handle)` 再从 `main` 返回，进程会在退出阶段收到
SIGSEGV（exit 139）。`dmesg` 里的特征是「跳转到未映射页」：

```
probe[21822]: segfault at 7d82b09ac4e1 ip 00007d82b09ac4e1 ...
```

故障地址和 RIP 完全相同 ⇒ 不是读写越界，而是跳到了一个已经 unmap 的地址。
原因是 librime 里的 glog 注册了 atexit / 静态析构回调，`dlclose` 把库卸掉之后
这些回调仍然会在进程退出时被调用。

probe 的处理：**有意不调用 `dlclose`**。进程马上就退出了，把库留在映射里是
最省事也最安全的做法。`RimeDestroySession` / `RimeFinalize` 正常调用，
所以会话和引擎状态本身是干净释放的。

**5. `select_schema()` 对不存在的方案名也返回 True。**

这是做错误处理时发现的：传 `--schema nosuch_schema`，
`select_schema()` 返回 True，`get_current_schema()` 还把 `nosuch_schema`
原样回显。会话进入「没有引擎」的假死状态：所有按键 `handled=0`、候选恒为空，
**而退出码是 0**。

probe 的处理：先拿 `get_schema_list()` 把请求的方案名和已部署列表核对一遍，
不在列表里就直接报错退出（exit 1）并列出可用方案。

**6. glog 的日志文件名会撞名。**

glog 的文件名是 `<程序名>.<主机>.<用户>.log.<级别>.<时间戳>.<pid>`。
在容器 / PID namespace 下 pid 会被快速复用，同一秒内的两次运行可能拿到同一个
pid，于是 `open(O_EXCL)` 撞名，stderr 上多出一行
`Could not create logging file: File exists`。

probe 的处理：每次运行用 `mkdtemp` 开一个专属日志目录，成功时删掉，
失败或 `--verbose` 时保留便于排查。日志目录始终在 `run/` 里，不碰 `/tmp`。

**7. librime 会学习：user_data_dir 是有状态的，输出因此会漂移。**

这条对「基线」这件事影响最大。librime 会把**上屏过**的词写进
`run/user/<schema>.userdb`，并据此调整后续候选排序。实测（`luna_pinyin`，
按键 `nihao`，看第 3 步 `ni h` 的候选）：

| 状态 | `ni h` 的候选 |
|---|---|
| 冷启动（空 user dir） | 你會 **你好** 你還 妳好 你和 |
| 某次运行提交过「你好」之后 | **你好** 你會 你還 妳好 你和 |

「你好」从第 2 位升到第 1 位。进一步隔离实验：

```
A (fresh, no commit):   ['你會', '你好', '你還', '妳好', '你和']
B (2nd run, no commit): ['你會', '你好', '你還', '妳好', '你和']   ← 和 A 相同
-- 中间跑一次带 --select-space 的 --
C (after commit):       ['你好', '你會', '你還', '妳好', '你和']   ← 变了
D (next run):           ['你好', '你會', '你還', '妳好', '你和']   ← 和 C 相同
```

结论：**触发学习的是「上屏」，不是「打字」**；只打字不选词的话每次运行结果完全一致。

因此：

- 想要**可复现的冷基线**，加 `--reset`（清空 `run/user` 重新部署，约 4.8 s）。
  实测两次 `--reset` 运行的 stdout 逐字节相同。
- 不加 `--reset` 时会复用 `run/user`，快得多（约 26 ms），但如果上一次运行提交过
  内容，候选顺序就已经被学习改变了。
- 每条 `session` 记录里有 `"reset"` 字段标明本次是不是干净基线。
  `samples/` 里的文件都是 `--reset` 抓的冷基线。

## 编译

```sh
cd tools/librime-probe
./build.sh
```

等价于：

```sh
cc -std=c99 -O2 -Wall -Wextra -D_GNU_SOURCE -o probe probe.c -ldl
```

注意链接行里没有 `-lrime`。

## 用法

```
probe --schema <id> --keys "<keys>" [选项]

  --schema ID        方案 id，例如 luna_pinyin / cangjie5（必填）
  --keys KEYS        按键序列。普通字符 = 一次按键；
                     也支持 <space> <Return> <BackSpace> <Escape>
                     <Page_Down> <Page_Up> <Tab> <Delete> 等记号。
  --select-space     按键序列跑完后补一个空格键（选中高亮候选）
  --page N           按键序列之后按 N 次 Page_Down（翻页）
  --json             汇总成单个 JSON 对象（默认是每键一行的 JSONL）
  --text             人类可读的纯文本输出
  --reset            开跑前清空 run/user（强制重新部署）
  --full-deploy      维护时做全量检查（默认增量，用系统预编译 .bin）
  --user-dir DIR     覆盖一次性用户目录（默认 <exe目录>/run/user）
  --shared-dir DIR   覆盖共享数据目录（默认 /usr/share/rime-data）
  --deploy-timeout S 部署等待上限秒数（默认 600）
  --verbose          打开 librime INFO 日志，并保留本次日志目录
  --check-layout     打印手抄结构体的布局自检后退出
  -h, --help         显示帮助
```

### 一次性用户目录

`shared_data_dir` 指向系统的 `/usr/share/rime-data`（只读），
`user_data_dir` 指向 `tools/librime-probe/run/user`，
`prebuilt_data_dir` 指向 `/usr/share/rime-data/build`（系统自带的预编译
`.prism.bin` / `.table.bin`），`staging_dir` 指向 `run/user/build`。

**用户真实的 RIME 配置完全不会被读写。** 所有写操作（部署产物、日志、
userdb）都落在 `run/` 里，且 `run/` 已在 `.gitignore` 中忽略。

实测耗时：冷启动（首次部署）约 **4.8 s**，之后每次约 **26 ms**。

## 输出格式

默认输出是 **JSONL**：每按一个键打印一行 JSON 对象，第一行是 `session`，
最后一行是 `end`。stdout 是干净的（librime 的日志走 stderr 和日志文件），
每一行都能单独 `json.loads`。

```json
{"event":"session","probe_version":"1","librime":"1.16.1","schema_requested":"luna_pinyin",
 "schema_active":"luna_pinyin","keys":"nihao","select_space":1,"page_down":0,
 "shared_data_dir":"...","user_data_dir":"...","prebuilt_data_dir":"...","staging_dir":"..."}
{"event":"key","step":4,"key":"o","keycode":111,"mask":0,"handled":1,
 "context":{"preedit":"ni hao","cursor_pos":6,"sel_start":0,"sel_end":6,
   "page_no":0,"page_size":5,"is_last_page":0,"highlighted":0,"num_candidates":5,
   "select_labels":[],"select_keys":null,
   "candidates":[{"index":0,"text":"你好","comment":null}, ...],
   "commit_text_preview":"你好","schema":"luna_pinyin",
   "is_composing":1,"is_ascii_mode":0},
 "commit":null}
{"event":"end","steps":6,"commit":"你好"}
```

`--json` 则把同样的记录汇总成一个 `{"event":"run","records":[...]}` 对象。

## 实测结果

以下都是本机真实跑出来的输出，原件在 `samples/` 里。

### luna_pinyin + `nihao` + 空格

```sh
./probe --reset --schema luna_pinyin --keys "nihao" --select-space
```

逐步候选（`candidates[].text`）：

| step | key | preedit | 候选 |
|---|---|---|---|
| 0 | `n` | `n` | 你 那 呢 能 年 |
| 1 | `i` | `ni` | 你 擬 尼 泥 呢 |
| 2 | `h` | `ni h` | 你會 你好 你還 妳好 你和 |
| 3 | `a` | `ni ha` | 你哈 你 擬 尼 泥 |
| 4 | `o` | `ni hao` | **你好** 妳好 逆號 擬好 你 |
| 5 | `<space>` | `` | （上屏） |

```json
{"event":"end","steps":6,"commit":"你好"}
```

**上屏文本是「你好」**，符合预期。

### cangjie5 + `ab`（证明不是拼音专用）

```sh
./probe --schema cangjie5 --keys "ab"
./probe --schema cangjie5 --keys "ab" --select-space
```

| step | key | preedit | 候选 |
|---|---|---|---|
| 0 | `a` | `a（日）` | 日 曰 啊 阿 吖 |
| 1 | `b` | `ab（日月）` | **明** 冐 日月 阿布 阿寶 |

```json
{"event":"end","steps":3,"commit":"明"}
```

`a`→日、`ab`→明 是仓颉最经典的例子，说明这套 harness 不依赖拼音。
cangjie5 是码表方案（走 `.table.bin` + 反查），和 luna_pinyin 的
`.prism.bin` 路径不同，两条路径都通。

### 翻页

```sh
./probe --schema luna_pinyin --keys "ni" --page 1 --text
```

```
[2] key=<Page_Down> handled=1 preedit="ni"
      0. 妳
      1. 妮
      2. 膩
      3. 逆
      4. 倪
      preview: 妳
```

### 错误处理

```sh
$ ./probe --schema nosuch_schema --keys "a"; echo "exit=$?"
probe: 方案 "nosuch_schema" 不在已部署的方案列表里。可用方案：
        luna_pinyin          朙月拼音
        luna_pinyin_simp     朙月拼音·简化字
        luna_pinyin_fluency  朙月拼音·語句流
        bopomofo             注音
        bopomofo_tw          注音·臺灣正體
        cangjie5             倉頡五代
        stroke               五筆畫
        terra_pinyin         地球拼音
exit=1
```

坏方案名 / 坏按键记号 / 缺参数都是 exit 1，且 stdout 为空。

## samples/ 里有什么

| 文件 | 内容 | 用途 |
|---|---|---|
| `luna_pinyin-nihao.txt` | `--reset --schema luna_pinyin --keys "nihao" --select-space` 的原始 stdout | 主验收证据，commit = 你好 |
| `cangjie5-ab.txt` | `--reset --schema cangjie5 --keys "ab"` 的原始 stdout | 证明非拼音方案同样可用 |
| `bopomofo-su3-selectlabels.txt` | `--schema bopomofo --keys "su3"` 的原始 stdout | 证明 `select_labels`（offset 80）抄对了 |
| `layout-check.txt` | `--check-layout` 的输出 | 结构体布局与库自报 `data_size` 的对照 |

这些文件是**逐字节的原始 stdout**，没有加注释头，可以直接当 JSONL 解析；
每条记录里已经带了 schema / keys / reset / 目录等上下文，本身是自描述的证据。
前两个是 `--reset` 冷基线，重跑同一条命令应当逐字节一致。

## 已知限制

- 只支持单个会话、单次按键序列；不做交互式输入、不做方案切换。
- `--keys` 里的非 ASCII 字节会被拒绝，特殊键要用 `<name>` 记号。
- `select_schema` 之后没有再校验引擎是否真的加载成功；目前靠
  `get_schema_list()` 的事前核对兜底。
- 默认复用 `run/user`，因此**输出会受上一次运行的学习结果影响**；
  要可复现的冷基线请加 `--reset`（见坑 7）。
- 依赖 `/proc/self/exe` 定位自己的目录（Linux only），可以用 `--user-dir` 覆盖。
- `dlclose` 被有意跳过，见上文坑 4。
