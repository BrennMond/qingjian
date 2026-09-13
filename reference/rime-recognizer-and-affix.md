# RIME `recognizer` 与 `affix_segmentor` —— 以 librime 源码为准

**取证快照**

| 对象 | 版本 | 说明 |
| --- | --- | --- |
| librime | `master` @ `4e6f83926963633e58da6059ebb129a3a7f0bd42`（2026-09-12） | 所有 C++ 引用均指此提交；行号为该提交下的行号 |
| 本机 librime | 1.16.1（`/usr/lib/x86_64-linux-gnu/librime.so.1`） | 用于第 2.4 节实测 |
| rime-ice | `main` @ 抓取时 | 仅用于引其自带注释，不引其行为 |

源码 URL 形如 `https://raw.githubusercontent.com/rime/librime/master/src/rime/...`。下文引用写作 `librime@4e6f839 <path>:<行>`。

**术语**

- **segmentor（切分器）**：把输入串切成若干 `Segment` 的组件，实现 `Proceed(Segmentation*)`。返回 `false` 表示"本轮到此为止"。
- **segment（切分段）**：输入串上的 `[start, end)` 区间，带一组 `tags`（标签，`std::set<string>`）。
- **tag（标签）**：字符串。翻译器/过滤器靠它决定"这一段归不归我管"。
- **translator（翻译器）**：把一段输入码变成候选。
- **pattern（模式）**：`recognizer/patterns` 里的一条正则。
- **prefix（前缀）**：`affix_segmentor` 要从段首剥掉的字符串。

---

## 1. `recognizer/patterns`

### 1.1 精确的 YAML 形态：**`名称: 正则字符串`，值必须是标量**

```cpp
// librime@4e6f839 src/rime/gear/recognizer.cc:18-37
static void load_patterns(RecognizerPatterns* patterns, an<ConfigMap> map) {
  if (!patterns || !map)
    return;
  for (auto it = map->begin(); it != map->end(); ++it) {
    auto value = As<ConfigValue>(it->second);
    if (!value)
      continue;
    try {
      boost::regex pattern(value->str());
      (*patterns)[it->first] = pattern;
    } catch (boost::regex_error& e) {
      LOG(ERROR) << "error parsing pattern /" << value->str()
                 << "/: " << e.what();
    }
  }
}

void RecognizerPatterns::LoadConfig(Config* config, const string& name_space) {
  load_patterns(this, config->GetMap(name_space + "/patterns"));
}
```

结论，逐条：

1. **容器是 `ConfigMap`**（`config->GetMap(...)`），所以 `patterns` 下是 **`名称: 值` 的映射**。
2. **值必须能 `As<ConfigValue>` 成标量字符串。** 写成 YAML 列表 `name: [regex1, regex2]` 时，`As<ConfigValue>` 返回空，`if (!value) continue;` —— **该条被静默丢弃，连 WARNING 都没有**。也不存在"多条正则取并集"的写法。（要并集只能写进一条正则里，用 `|`，例如系统 `default.yaml:61` 的 `url: "^(www[.]|https?:|ftp[.:]|mailto:|file:).*$|^[a-z]+[.].+$"`。）
3. 编译用 `boost::regex pattern(value->str())`，**没有传 `boost::regex::optimize` 或任何标志**；编译失败只打一条 `ERROR` 日志并跳过该条（其它条目不受影响）。
4. **实测核对**：本机 `/usr/share/rime-data/` 下 8 个方案（`default` / `luna_pinyin` / `bopomofo` / `cangjie5` / `stroke` / `terra_pinyin` / `luna_pinyin_fluency`）的 `recognizer/patterns` 全部是标量字符串，**没有任何一处用列表形式**。

### 1.2 `recognizer/import_preset`

```cpp
// librime@4e6f839 src/rime/config/legacy_preset_config_plugin.cc:61-73
  if (auto preset = resource->data->Traverse("recognizer/import_preset")) {
    if (!Is<ConfigValue>(preset))
      return false;
    auto preset_config_id = As<ConfigValue>(preset)->str();
    LOG(INFO) << "interpreting recognizer/import_preset: " << preset_config_id;
    Reference reference{preset_config_id, "recognizer", false};
    if (!IncludeReference{reference}
             .TargetedAt(Cow(resource, "recognizer"))
             .Resolve(compiler)) {
      LOG(ERROR) << "failed to include section " << reference;
      return false;
    }
  }
```

这是**普通 `__include` 式合并**（不是 key_binder 那种列表追加，见 `rime-key-binding-actions.md` §4.4）：`import_preset: default` 等价于把 `default:/recognizer` 包含进当前方案的 `recognizer` 节点，**同名 `patterns` 键由方案自己覆盖，其余保留**。`Is<ConfigValue>(preset)` 为假（例如写成列表）会让整个 link 阶段失败。

### 1.3 模式名就是 tag —— 而且有两个消费方

`RecognizerPatterns` 是 `map<string, boost::regex>`（`recognizer.h:30`），`GetMatch` 返回：

```cpp
// librime@4e6f839 src/rime/gear/recognizer.h:19-28
struct RecognizerMatch {
  string tag;
  size_t start = 0, end = 0;
  ...
  bool found() const { return start < end; }
};
```

**`RecognizerMatch::tag` 就是 `patterns` 的键名。** 它被两个完全不同的组件使用，两者读的是**同一份配置**：

**(a) `recognizer` —— processor（按键层）。** 它在 `engine/processors` 里：

```cpp
// librime@4e6f839 src/rime/gear/recognizer.cc:72-102
Recognizer::Recognizer(const Ticket& ticket) : Processor(ticket) {
  if (!ticket.schema)
    return;
  if (name_space_ == "processor") {
    name_space_ = "recognizer";
  }
  if (Config* config = ticket.schema->config()) {
    patterns_.LoadConfig(config, name_space_);
    config->GetBool(name_space_ + "/use_space", &use_space_);
  }
}

ProcessResult Recognizer::ProcessKeyEvent(const KeyEvent& key_event) {
  if (patterns_.empty() || key_event.ctrl() || key_event.alt() ||
      key_event.super() || key_event.release()) {
    return kNoop;
  }
  int ch = key_event.keycode();
  if ((use_space_ && ch == ' ') || (ch > 0x20 && ch < 0x80)) {
    // pattern matching against the input string plus the incoming character
    Context* ctx = engine_->context();
    string input = ctx->input();
    input += ch;
    auto match = patterns_.GetMatch(input, ctx->composition());
    if (match.found()) {
      ctx->PushInput(ch);
      return kAccepted;
    }
  }
  return kNoop;
}
```

作用：**把"本来会被别的处理器拒掉"的按键直接塞进输入串并吃掉这次按键**（返回 `kAccepted` 就不再往下走）。典型用途是让 `@` `:` `/` 这类不在 `speller/alphabet` 里的字符能进输入串（邮箱/网址），或者让大写字母进入（系统 `default.yaml:60` 的 `uppercase: "[A-Z][-_+.'0-9A-Za-z]*$"`）。它**不创建切分段**。

注意判定时机：它拿的是 **`已有输入串 + 新按键`**，也就是"算上这个新字符之后是否整串符合某个模式"。所以 `@` 是敲下去的那一刻被接受，而第一个字符（模式还不成立时）仍走普通处理器。

**(b) `matcher` —— segmentor（切分层）。** 它在 `engine/segmentors` 里，**负责把 tag 贴到段上**：

```cpp
// librime@4e6f839 src/rime/gear/matcher.cc:15-42
Matcher::Matcher(const Ticket& ticket) : Segmentor(ticket) {
  // read schema settings
  if (!ticket.schema)
    return;
  if (name_space_ == "segmentor") {
    name_space_ = "recognizer";
  }
  Config* config = ticket.schema->config();
  patterns_.LoadConfig(config, name_space_);
}

bool Matcher::Proceed(Segmentation* segmentation) {
  if (patterns_.empty())
    return true;
  auto match = patterns_.GetMatch(segmentation->input(), *segmentation);
  if (match.found()) {
    while (segmentation->GetCurrentStartPosition() > match.start)
      segmentation->pop_back();
    Segment segment(match.start, match.end);
    segment.tags.insert(match.tag);
    segmentation->AddSegment(segment);
    // terminate this round?
    // return false;
  }
  return true;
}
```

> **这是理解 `affix_segmentor` 的关键，也是我们最容易漏掉的一环**：裸写 `affix_segmentor@foo` 是**不会自己触发**的。它的第一件事是检查"当前最后一段是否已经带 `foo` 标签"（§2.1 第 1 步）。给段贴 `foo` 标签的正是 `matcher` + `recognizer/patterns/foo`。**`recognizer/patterns/<tag>` 与 `affix_segmentor@<tag>` 是成对出现的。** rime-ice 自己在 `prefix` 那一行也写了这句话：「与 recognizer/patterns/radical_lookup 匹配」。

### 1.4 模式如何套用到输入上：**是 `regex_search`，但被两条约束夹住**

```cpp
// librime@4e6f839 src/rime/gear/recognizer.cc:39-70
RecognizerMatch RecognizerPatterns::GetMatch(
    const string& input,
    const Segmentation& segmentation) const {
  size_t j = segmentation.GetCurrentEndPosition();
  size_t k = segmentation.GetConfirmedPosition();
  string active_input = input.substr(k);
  DLOG(INFO) << "matching active input '" << active_input << "' at pos " << k;
  for (const auto& v : *this) {
    boost::smatch m;
    if (boost::regex_search(active_input, m, v.second)) {
      size_t start = k + m.position();
      size_t end = start + m.length();
      if (end != input.length())
        continue;
      if (start == j) {
        DLOG(INFO) << "input [" << start << ", " << end << ") '" << m.str()
                   << "' matches pattern: " << v.first;
        return {v.first, start, end};
      }
      for (const Segment& seg : segmentation) {
        if (start < seg.start)
          break;
        if (start == seg.start) {
          DLOG(INFO) << "input [" << start << ", " << end << ") '" << m.str()
                     << "' matches pattern: " << v.first;
          return {v.first, start, end};
        }
      }
    }
  }
  return RecognizerMatch();
}
```

逐条回答任务书的问题：

**（1）不是全匹配（full match），不是前缀匹配（prefix match），是"带约束的搜索"。** `boost::regex_search` 本身在整串里找**最左**的匹配，但随后：

- `if (end != input.length()) continue;` —— **匹配必须一直延伸到整个输入串的末尾**。这一条等价于给每条模式强制加了一个 `$` 锚点（但对 `^` 没有强制）。
- **匹配的起点必须落在"有意义的位置"**：要么恰好等于 `j = segmentation.GetCurrentEndPosition()`（本轮切分的起点，也是上一段的终点），要么恰好等于某个已有段的 `start`。

**（2）识别并不限于输入串开头。** 它限于**未确认部分**：`active_input = input.substr(k)`，其中 `k = segmentation.GetConfirmedPosition()`（`segmentation.cc:147-154`：所有 `status >= kSelected` 的段的最大 `end`）。`k` 之后的文本才参与匹配；匹配结果再平移回 `+k`。所以已经确认（上屏/选中）的部分不会重新参与识别。

**（3）"怎么在中间找到匹配"**：靠 `regex_search` 扫描，再用上面两条约束筛。这带来两个必须知道的行为：

- **模式应当自己写 `^` 和/或 `$`**。`GetMatch` 只补了"结束于串尾"这一半；不写 `^` 时，匹配可以从任意段边界开始。系统 `default.yaml:60` 的 `uppercase: "[A-Z][-_+.'0-9A-Za-z]*$"` 就是故意不写 `^` 的例子。
- **`regex_search` 只返回最左的那个匹配，不会为了满足 `end == input.length()` 去试别的匹配长度。** 如果模式在更靠左的位置先匹配上、但没到串尾，`continue` 会直接跳过这条模式，即使换个长度就能成立。实践中的写法（带 `$` 或 `.*$`）避开了这个坑，但自己写模式时要留意。

**（4）尝试顺序 = 模式名的字典序。** `RecognizerPatterns` 继承 `map<string, boost::regex>`（`recognizer.h:30`），`for (const auto& v : *this)` 按 key 升序遍历。**多条模式同时满足时，名字字典序最小的那条赢**（例如 `email` 先于 `punct` 先于 `reverse_lookup` 先于 `uppercase` 先于 `url`）。模式名不是随便起的标签，它参与优先级判定。

### 1.5 有没有"首个字面字符"缓存（leading literal）用于快速排除？

**没有。** 这一点是明确否定的，不是"未能取得"：

- `recognizer.cc` 全文 104 行，没有提取、保存或比较任何字面前缀的代码。
- 编译处只有 `boost::regex pattern(value->str());`（`recognizer.cc:26`），**没有 `boost::regex::optimize`**。
- 在 `librime@4e6f839` 全树 `src/` 下搜 `literal`（不分大小写）只有 8 处命中，**全部在 `src/rime/config/` 的配置编译器里**（`config_data.cc:294` 的 YAML emitter、`config_compiler*.{h,cc}` 的 `PatchLiteral`），与 recognizer 无关。

所以每一次按键，`Recognizer::ProcessKeyEvent` 都会对**当前所有模式**各做一次 `boost::regex_search`；`matcher` 在每次切分时也一样。模式数量在真实方案里是个位数（rime-ice 6 条，luna_pinyin 2 条 + 预设 3 条），这就是全部成本。**缓存 leading literal 是我们自己的优化空间，不是 librime 的既有行为**——不能拿"librime 也这么做"来为它背书。

---

## 2. `affix_segmentor` 的 `prefix`：**字面字符串，不是字符集**

### 2.1 源码

```cpp
// librime@4e6f839 src/rime/gear/affix_segmentor.h:20-27
 protected:
  string tag_;
  string prefix_;
  string suffix_;
  string tips_;
  string closing_tips_;
  set<string> extra_tags_;
```
```cpp
// librime@4e6f839 src/rime/gear/affix_segmentor.cc:15-33
AffixSegmentor::AffixSegmentor(const Ticket& ticket)
    : Segmentor(ticket), tag_("abc") {
  if (!ticket.schema)
    return;
  if (Config* config = ticket.schema->config()) {
    config->GetString(name_space_ + "/tag", &tag_);
    config->GetString(name_space_ + "/prefix", &prefix_);
    config->GetString(name_space_ + "/suffix", &suffix_);
    config->GetString(name_space_ + "/tips", &tips_);
    config->GetString(name_space_ + "/closing_tips", &closing_tips_);
    if (auto extra_tags = config->GetList(name_space_ + "/extra_tags")) {
      for (size_t i = 0; i < extra_tags->size(); ++i) {
        if (auto value = extra_tags->GetValueAt(i)) {
          extra_tags_.insert(value->str());
        }
      }
    }
  }
}
```

**第一步结论**：`prefix` 是 `config->GetString(...)` 读出来的**一个字符串**，不是列表。写成 YAML 列表会读不到（`GetString` 失败，`prefix_` 保持空串，整个组件变成"没有前缀"而静默失效）。

**第二步结论 —— 匹配**：

```cpp
// librime@4e6f839 src/rime/gear/affix_segmentor.cc:52-71
  size_t j = segmentation->GetCurrentStartPosition();
  size_t k = segmentation->GetCurrentEndPosition();
  string active_input(segmentation->input().substr(j, k - j));
  if (prefix_.empty() || !boost::starts_with(active_input, prefix_)) {
    return true;
  }
  DLOG(INFO) << "affix_segmentor: " << active_input;
  DLOG(INFO) << "segmentation: " << *segmentation;
  // just prefix
  if (active_input.length() == prefix_.length()) {
    Segment& prefix_segment(segmentation->back());
    prefix_segment.tags.erase(tag_);
    prefix_segment.prompt = tips_;
    prefix_segment.tags.insert(tag_ + "_prefix");
    DLOG(INFO) << "prefix: " << *segmentation;
    // continue this round
    return true;
  }
  // prefix + code
  active_input.erase(0, prefix_.length());
```

`boost::starts_with(range, prefix)`（Boost.StringAlgorithms）是**逐字符字面前缀比较**：`starts_with("uUni", "uU") == true`，`starts_with("uni", "uU") == false`，`starts_with("Uni", "uU") == false`。这里没有任何字符集、大小写折叠、或"逐个候选前缀"的逻辑。

**所以 `prefix: "uU"` 的含义是：输入串必须以字符 `u` 紧跟字符 `U` 开头。** 它**不是**"`u` 或 `U` 都行"，也**不是**两个候选的单字符前缀。

### 2.2 `uU` 这个具体写法的证据链

| 证据 | 内容 |
| --- | --- |
| librime 源码 | `boost::starts_with(active_input, prefix_)` + `active_input.erase(0, prefix_.length())`（`affix_segmentor.cc:55, 61, 71`）——匹配与剥离都以 `prefix_` 的**完整长度**为单位 |
| rime-ice 自己的注释 | `others/no_lua_schema/rime_ice.schema.yaml:154`：`prefix: "uU"  # 反查前缀（反查时前缀会消失影响打英文所以设定为两个字母，或可改成一个非字母符号），与 recognizer/patterns/radical_lookup 匹配` —— 「设定为**两个字母**」，理由正是"要避开打英文时的干扰"，如果它等价于单个 `u` 就完全达不到这个目的 |
| 配套的正则 | `recognizer/patterns/radical_lookup: "^uU[a-z]+$"` —— 正则里也是字面的 `u` 后跟 `U`；若前缀是字符集，这条正则必须写成 `^[uU][a-z]+$` 才自洽 |
| 配套的字母表 | rime-ice `speller/alphabet` 含大写：`zyxwvutsrqponmlkjihgfedcbaZYXWVUTSRQPONMLKJIHGFEDCBA` —— 大写 `U` 能被输入，正是为了能打出 `uU` |

四条互相独立、互相印证。**我们的读法是错的。**

### 2.3 实测（真实 librime 1.16.1）

用本仓库自带的 `tools/librime-probe`，在隔离的 `/tmp` 副本里跑（**没有读写仓库里的任何文件**）。临时方案 `probe_affix` 的切分器链为 `ascii_segmentor, matcher, abc_segmentor, affix_segmentor@wq, affix_segmentor@uq, punct_segmentor, fallback_segmentor`，`translator` 为 `luna_pinyin`，另配：

```yaml
recognizer:
  patterns:
    wq: "^zz[a-z]+$"
    uq: "^uU[a-z]+$"
wq: { tag: wq, prefix: "zz", dictionary: luna_pinyin, enable_user_dict: false, enable_sentence: false, tips: "〔Z〕" }
uq: { tag: uq, prefix: "uU", dictionary: luna_pinyin, enable_user_dict: false, enable_sentence: false, tips: "〔U〕" }
```

按键 `zzni` 的逐键结果：

| 按键 | preedit | 候选 |
| --- | --- | --- |
| `z` | `z` | 在 中 這 做 再 |
| `z` | `z z` | 這種 最終 真正 作者 組織 |
| `n` | `n` | 那 你 呢 拿 哪 |
| `i` | `ni` | 你 擬 尼 泥 呢 |

解读：

- 前两键没有任何 affix 行为（`^zz[a-z]+$` 还不成立，走普通 abc 段）。
- 第 3 键起，`^zz[a-z]+$` 成立 → `matcher` 建段 `[0,3)` 并贴 `wq` 标签 → `affix_segmentor@wq` 把**两个字符** `zz` 整体剥掉 → 代码段 `[2,3)` = `"n"`。
- **候选 `那 你 呢 拿 哪` 正是 `luna_pinyin` 对编码 `n` 的结果**；第 4 键代码段 `"ni"` 得到 `你 擬 尼 泥 呢`，与直接跑 `luna_pinyin --keys "ni"` 的结果一致（`你 擬 尼 泥 呢`）。
- **preedit 里 `zz` 完全消失**（只剩 `n` / `ni`）——前缀段是 `phony` 的，且被单独切出去。

⇒ **`prefix` 是字面字符串这一条，实测确认：一个两字符前缀被整体匹配、整体剥离，翻译器只看到前缀之后的正文。**

**关于 `uU` 本体的限制（必须说明）**：`librime-probe` 把 `--keys` 里的大写 ASCII 字母按 Linux/X11 惯例翻译成 **小写 keycode + ShiftMask**：

```c
// tools/librime-probe/probe.c（keyseq_parse 内）
    if (c >= 'A' && c <= 'Z') {
      /* librime 期望小写 keycode + ShiftMask */
      keyseq_push(s, (int)(c - 'A' + 'a'), MASK_SHIFT, label);
    }
```

因此探针送进 librime 的 `KeyEvent::keycode()` 是小写 `u`，输入串里出现的也是 `u` 而不是 `U`，`^uU[a-z]+$` 与 `prefix: "uU"` 都不会成立（实测 `--keys "uUni"` 在该方案下确实零候选、零 affix 行为）。**这是探针/前端约定造成的，不是 librime 的行为**：macOS 的 Squirrel 与 Windows 的 Weasel 直接送字符码，Shift+u 得到的就是大写 `U`（这也正是系统 `default.yaml:60` 要写 `uppercase: "[A-Z][-_+.'0-9A-Za-z]*$"`、rime-ice 要把大写字母放进 `speller/alphabet` 的原因）。

**所以：`uU` 的字面性由源码 + rime-ice 注释 + 配套正则/字母表四项证据确认；"两字符前缀被整体匹配与剥离"由 `zz` 实测确认。我们没有在探针里跑通 `uU` 这一具体字符串**——记为一条明确的取证限制。

---

## 3. 前缀如何被剥离，以及翻译器到底看到什么

```cpp
// librime@4e6f839 src/rime/gear/affix_segmentor.cc:59-88
  // just prefix
  if (active_input.length() == prefix_.length()) {
    Segment& prefix_segment(segmentation->back());
    prefix_segment.tags.erase(tag_);
    prefix_segment.prompt = tips_;
    prefix_segment.tags.insert(tag_ + "_prefix");
    DLOG(INFO) << "prefix: " << *segmentation;
    // continue this round
    return true;
  }
  // prefix + code
  active_input.erase(0, prefix_.length());
  Segment prefix_segment(j, j + prefix_.length());
  prefix_segment.status = Segment::kGuess;
  prefix_segment.prompt = tips_;
  prefix_segment.tags.insert(tag_ + "_prefix");
  prefix_segment.tags.insert("phony");  // do not commit raw input
  segmentation->pop_back();
  segmentation->Forward();
  segmentation->AddSegment(prefix_segment);
  j += prefix_.length();
  Segment code_segment(j, k);
  code_segment.tags.insert(tag_);
  for (const string& tag : extra_tags_) {
    code_segment.tags.insert(tag);
  }
  segmentation->Forward();
  segmentation->AddSegment(code_segment);
  DLOG(INFO) << "prefix+code: " << *segmentation;
```

### 3.1 三种情形

`j`/`k` 是**当前段的起点/终点**（`segmentation.cc:135-141`），`active_input` 是这一段的内容。

**情形 A —— 输入里只有前缀，还没有正文**（`active_input.length() == prefix_.length()`，例如刚打完 `uU`）：

- 不新建段。把**当前这一段本身**改造成前缀段：删掉 `tag_`，插入 `tag_ + "_prefix"`，`prompt = tips_`。
- `prompt`（提示串，例如 `〔拆字〕`）会随 preedit 显示。
- **返回 `true`（继续本轮）**，不结束切分。所以后面还可能再被别的切分器处理。

**情形 B —— 前缀 + 正文**（正常情形）：

1. `active_input.erase(0, prefix_.length())` —— 只影响局部变量（它只用于下面的 `ends_with(suffix_)` 判断），**并不是在原地裁剪输入串**。真正的裁剪体现在段的划分上。
2. `segmentation->pop_back()` 丢掉原来那个整段（`[j, k)`）。
3. `Forward()` + `AddSegment(prefix_segment)` 建**前缀段** `[j, j + prefix_.length())`：`status = kGuess`、`prompt = tips_`、标签 `tag_ + "_prefix"` 和 `"phony"`（注释写明 `// do not commit raw input`，即不要把这个前缀原样上屏）。
4. `Forward()` + `AddSegment(code_segment)` 建**正文段** `[j + prefix_.length(), k)`：标签 `tag_`，外加**全部 `extra_tags_`**。
5. **返回 `false`（exclusive，本轮结束）**——`affix_segmentor.cc:106-107`。

**情形 C —— 带 `suffix_`**（`affix_segmentor.cc:89-105`）：在正文段之后再切出一个后缀段 `[k, k + suffix_.length())`，标签 `tag_ + "_suffix"` 与 `"phony"`，`prompt` 用 `closing_tips_`（为空则退回 `tips_`）。若正文被压成空段，则 `pop_back()` 丢掉正文段。

### 3.2 因此翻译器看到的文本

**翻译器看到的是「前缀之后的正文」，前缀不在它的段里。**

理由：

```cpp
// librime@4e6f839 src/rime/engine.cc:203-215
void ConcreteEngine::TranslateSegments(Segmentation* segments) {
  DLOG(INFO) << "TranslateSegments: " << *segments;
  for (Segment& segment : *segments) {
    DLOG(INFO) << "segment [" << segment.start << ", " << segment.end
               << "), status: " << segment.status;
    if (segment.status >= Segment::kGuess)
      continue;
    size_t len = segment.end - segment.start;
    string input = segments->input().substr(segment.start, len);
    DLOG(INFO) << "translating segment: [" << input << "]";
    auto menu = New<Menu>();
    for (auto& translator : translators_) {
      auto translation = translator->Query(input, segment);
```

引擎给每个段的查询串是 **`input.substr(segment.start, segment.end - segment.start)`**。正文段的 `start` 已经 `+= prefix_.length()`（`affix_segmentor.cc:80`），所以前缀字符**不在**这个区间内。前缀段自己也被 `status = kGuess` 预置了（`:73`），会被 `if (segment.status >= Segment::kGuess) continue;` 跳过翻译。

实测印证（§2.3）：`prefix: "zz"` + 输入 `zzni` → `table_translator@wq` 收到的查询串就是 `"ni"`，候选与直接查 `ni` 完全一致；preedit 也不含 `zz`。

**两个容易踩的细节**：

- **段的 `start/end` 不含前缀**，但**整条输入串仍然含前缀**。凡是按"输入串下标"而不是"段下标"工作的代码（例如我们自己的实现）都必须显式跳掉前缀长度，否则会错位。
- **`Segment::length` 是个几乎没人读的字段，不要信它。** 它在构造时算一次（`segmentation.h:36-37`：`length(end_pos - start_pos)`），而 `affix_segmentor` 情形 C 里 `segmentation->back().end = k;`（`:95`）只改 `end`、不同步 `length`。我们核对了 `librime@4e6f839` 的 `src/`：**没有任何一处读取 `Segment::length`**；`TranslateSegments` 是现算 `end - start` 的。所以这个不一致在 librime 里目前无害，但移植时若照着 `length` 实现就会在带 `suffix` 的边界情形下错位。

### 3.3 还有一个前置分支：`partial` 续段

```cpp
// librime@4e6f839 src/rime/gear/affix_segmentor.cc:35-51
bool AffixSegmentor::Proceed(Segmentation* segmentation) {
  if (segmentation->empty())
    return true;
  if (!segmentation->back().HasTag(tag_)) {
    if (segmentation->size() >= 2) {
      Segment& previous_segment(*(segmentation->rbegin() + 1));
      if (previous_segment.HasTag("partial") && previous_segment.HasTag(tag_)) {
        // the remaining part of a partial selection should inherit the tag
        segmentation->back().tags.insert(tag_);
        // without adding new tag "abc"
        if (!previous_segment.HasTag("abc")) {
          segmentation->back().tags.erase("abc");
        }
      }
    }
    return true;
  }
  ...
```

这是 `partial` 选择（`Segment::Close()` 在候选只匹配了段的一部分时打的标签，`segmentation.cc:17-24`）之后的续段继承逻辑。**翻译器那个 `tag` 参数**见 §4。

---

## 4. `tag` 与 `extra_tags`

### 4.1 `affix_segmentor` 自己的 `tag`

- **默认值 `"abc"`**（`affix_segmentor.cc:16` 的构造初始化列表 `tag_("abc")`），可被 `<命名空间>/tag` 覆盖（`:20`）。
- 三个用途：
  1. **触发条件**：`segmentation->back().HasTag(tag_)`（`:38`）——最后一段必须已经带这个标签，组件才继续往下做。这正是 §1.3 里"必须先由 `matcher` 贴标签"的那一步。
  2. **命名两个附属段**：前缀段标签 `tag_ + "_prefix"`（`:65, :75`），后缀段 `tag_ + "_suffix"`（`:100`）。
  3. **给正文段贴标签**：`code_segment.tags.insert(tag_)`（`:82`）。
- **`name_space_` 的映射**：`recognizer`（processor）和 `matcher`（segmentor）在裸写时会把 `name_space_` 从槽位名改写成 `"recognizer"`（`recognizer.cc:75-77`、`matcher.cc:19-21`）；**`affix_segmentor` 没有这段映射**。所以裸写 `- affix_segmentor` 读的是 `segmentor/prefix`、`segmentor/tag`；要读 `radical_lookup/prefix` 必须写 `- affix_segmentor@radical_lookup`。

### 4.2 `affix_segmentor` 的 `extra_tags`：一个**列表**

- 读取方式 `config->GetList(name_space_ + "/extra_tags")`（`:25`），元素取 `GetValueAt(i)->str()`，收进 `set<string> extra_tags_`。
- **只加到正文段上**（`:83-85`），不加到前缀段/后缀段。
- 与 `tag` 的区别：`tag` 是**这个组件自己的**身份标签（默认 `abc`）；`extra_tags` 是**额外的、别人的**标签，用来让别的翻译器/过滤器也认领这一段。典型用法是让一个 `table_translator@xxx` 或某个 filter 同时作用于该段。

### 4.3 不要和另外两个同名概念混淆

| 写法 | 属于谁 | 语义 |
| --- | --- | --- |
| `affix_segmentor` 的 `tag` / `extra_tags` | 切分器 | 决定段上贴哪些标签 |
| `abc_segmentor/extra_tags` | `abc_segmentor` | 给 abc 段额外贴标签（`abc_segmentor.cc:26-32` 读取，`:62-64` 贴）。CustomizationGuide 里 `abc_segmentor/extra_tags: {}` 用于关掉仓颉+拼音混打 |
| 翻译器的 `tag` / `tags` | 翻译器 | 翻译器**认领哪些标签的段** |
| 过滤器的 `tags` | 过滤器（`TagMatching`） | 过滤器作用于哪些标签的段 |

翻译器一侧：

```cpp
// librime@4e6f839 src/rime/gear/translator_commons.cc:140-153（节选）
    string tag;
    if (config->GetString(ticket.name_space + "/tag", &tag)) {
      // replace the first tag, and understand /tags as extra tags
      tags_[0] = tag;
    } else {
      // replace all of the default tags
      tags_.clear();
    }
    if (auto list = config->GetList(ticket.name_space + "/tags"))
      for (size_t i = 0; i < list->size(); ++i)
        if (auto value = As<ConfigValue>(list->GetAt(i)))
          tags_.push_back(value->str());
    if (tags_.empty())
      tags_.push_back("abc");
```
（默认值见 `translator_commons.h:174`：`vector<string> tags_{"abc"};  // invariant: non-empty`；消费处见 `table_translator.cc:246-247` / `script_translator.cc:218-219` 的 `if (!segment.HasAnyTagIn(tags_)) return nullptr;`。）

**注意 `tag`（单数）与 `tags`（复数）的差别**：写 `tag: x` 会把默认标签 `abc` **替换**成 `x`；写 `tags: [x, y]` 则是在（清空后的）集合上**追加** `x`、`y`。两者都不写时是 `["abc"]`。

过滤器一侧：

```cpp
// librime@4e6f839 src/rime/gear/filter_commons.cc:27-37
bool TagMatching::TagsMatch(Segment* segment) {
  if (!segment)
    return false;
  if (tags_.empty())  // match any
    return true;
  for (const string& tag : tags_) {
    if (segment->HasTag(tag))
      return true;
  }
  return false;
}
```

**过滤器不写 `tags` 时匹配一切段**；翻译器不写时默认只匹配 `abc`。这个不对称性很容易记反。

---

## 5. 与我们（stele）实现的差异

**这一节是快照，会过期。** 本文件写作期间，工作区里的另一个任务正在改 `crates/stele-engine/`（提交 `a8a366d`，之后 `processor.rs` / `spec.rs` 又有未提交改动）。下表核对的是**本文件落笔时磁盘上的实际代码**。**只做记录，未修改任何实现文件。**

| # | 主题 | librime 的事实 | stele 现状（写作时） | 结论 |
| --- | --- | --- | --- | --- |
| 1 | `prefix` 的含义 | **字面字符串**（§2）。`"uU"` = 两个字面字符 | **代码已修**：`segmentor.rs:494-499` 的 `expand_prefix` 现在只返回 `vec![s.to_owned()]`，`body_start`（`:506-510`）也改成"前缀长度唯一"。**但注释还是旧的**：`segmentor.rs:402-409` 仍写着「RIME 约定：`prefix: "uU"` 表示大小写两种写法都接受…所以它不是一个两字符的前缀，而是两个候选前缀」，`segmentor.rs:412` 仍写「接受的前缀（已展开成列表）」，`spec.rs:243` 仍写「RIME 允许写两个字符表示大小写两种写法」 | 行为已对齐。**剩下的是三处会误导下一个人的注释**，应当一并改掉——它们现在是"文档说 A、代码做 B" |
| 2 | `recognizer/patterns` 的值类型 | 必须是标量；列表被**静默跳过**（§1.1） | `spec.rs:232` 的 `patterns: Vec<RecogPattern>`；装载器如何对待列表形式待核对 | 若我们接受列表并"取并集"，会比 librime 宽松 |
| 3 | **匹配从哪个位置开始** | `regex_search(input.substr(confirmed_pos))`，匹配起点必须等于 `j = GetCurrentEndPosition()` **或某个已有段的 `start`**（§1.4）——也就是**允许从串中间开始** | **锚死在位置 0**：`regex.rs:216-232` 的 `match_prefix_len` 调 `match_node(&self.root, &chars, 0, …)`；`segmentor.rs:251` 还有 `if !input.starts_with(p.leading.as_str()) { continue; }` | **语义差异。** 真实反例：`/usr/share/rime-data/default.yaml:60` 的 `uppercase: "[A-Z][-_+.'0-9A-Za-z]*$"` 没有 `^`，librime 允许它在段边界处起匹配；我们要求它必须从 0 起 |
| 4 | **匹配到哪里结束** | **无条件**要求 `end == input.length()`（`recognizer.cc:51-52`），与正则里有没有 `$` **无关** | 用**正则源码文本的启发式**：`segmentor.rs:133-135` 的 `ends_with_anchor()` 判断"正则是否以 `$` 结尾"，结果存进 `RecogPattern::to_end`（`:209`），`match_prefix_len(input, to_end)` 据此决定要不要吃掉整串（`regex.rs:220-226`） | **两条都不完全等价。** 例：rime-ice（带 Lua 版）的 `unicode: "^U[a-f0-9]+"` **没有 `$`** ——librime 仍要求它匹配到串尾（所以 `U62fcxyz` 不成立），我们会接受前缀匹配（`U62fc` 就成立），**我们更宽松** |
| 5 | **多条模式谁赢** | 按**模式名字典序**遍历，**第一条满足约束的**赢（§1.4 第 4 条） | **最长认领赢**：`segmentor.rs:266-269` 的 `bytes_end > len` 取最长的那个 | **语义差异。** 例：同名长度下 `punct` 与 `radical_lookup` 谁赢，两边可能给出不同答案 |
| 6 | leading literal 缓存 | librime **完全没有**（§1.5，全树核对过） | 我们**有**：`segmentor.rs:102-129` 的 `leading_literal()` 抽出 `^` 之后的字面前缀，`:251` 用它做快速排除 | **代码本身没错，但注释是编的**：`spec.rs:205-209` 写着「提前算出来是为了快速排除…**RIME 也是这么做的**（它把 `^abc...` 里的字面部分当作 `leading`）」。**librime 没有这个机制**，这句话必须删掉或改成"这是我们的优化"。此外它是**近似**的（`leading_literal` 对 `([nl])ue$` 取 `n`、对 `^\d+$` 取空串），必须验证它与完整匹配在语义上等价 |
| 7 | `affix_segmentor` 的触发前提 | 必须由 `matcher` + `recognizer/patterns/<tag>` **先贴标签**（§1.3、§4.1）；裸写 `affix_segmentor` 读的是 `segmentor/*` 命名空间 | `segmentor.rs:244-269` 的 `scan()` 直接把"前缀匹配"做成了认领（claim），`AffixSegmentor` 据此切分 | **结构性差异。** 我们少了一层"recognizer 贴标签 → affix 才动"的耦合：在**只有 `affix_segmentor@foo` 而没写 `recognizer/patterns/foo`** 的方案上，librime 是彻底不工作，我们会照常工作。宽松不一定是坏事，但要**明确知道这是分叉**，并且不能声称"与 librime 一致" |
| 8 | `extra_tags` 贴在哪个段 | 只贴**正文段**（§4.2） | 待核对（`segmentor.rs:571` 有 `affix_all_tags`） | 贴错段会让过滤器作用到前缀段上 |
| 9 | 翻译器 `tag` vs 过滤器 `tags` 的默认值 | 翻译器默认 `["abc"]`；过滤器不写则**匹配一切**（§4.3） | 待核对 | 容易记反 |
| 10 | `Segment.length` | librime 自己在 `affix_segmentor.cc:95` 制造了 `end` 与 `length` 不同步；引擎**不读 `length`**，现算 `end - start`（§3.2） | 待核对 | 对照实现时按 `end - start` 对齐 |
| 11 | 模式名的优先级 | 模式名不是纯标签，它参与优先级（§1.4） | 待核对（我们按最长匹配，与名字无关） | 若将来要完全对齐，得引入"按名字排序" |

---

## 6. 明确的取证缺口

- **`prefix: "uU"` 未在探针里跑通**：`tools/librime-probe` 按 Linux/X11 惯例把大写字母转成"小写 keycode + ShiftMask"，`U` 进不了输入串。结论由源码 + rime-ice 注释 + 配套正则 + 配套字母表四项证据支撑，另有 `zz` 的实测确认"字面多字符前缀被整体匹配与整体剥离"。**未能取得**：`uU` 这一具体字符串在真实 librime 上的端到端按键记录。
- **`when: predicting` 的实际来源**：`librime@4e6f839` 全树只有 `key_binder.cc:265` 读取 `"prediction"` 标签，没有任何地方写入。我们**未能取得**任何确实会给段打该标签的插件的源码，因此无法说明它在实践中何时为真。
- **wiki 覆盖**：`CustomizationGuide` / `Configuration` / `RimeWithSchemata` 三个页面里，`send_sequence`、`set_option`、`unset_option`、`select`、`when: predicting`、`editor` 小节**全部 0 命中**。这些特性只存在于源码与发行版自带的 `key_bindings.yaml` 里，wiki 没有记载——这本身就是一条结论。
