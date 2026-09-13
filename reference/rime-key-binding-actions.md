# RIME 按键绑定与编辑器动作 —— 以 librime 源码为准

**取证快照**

| 对象 | 版本 | 说明 |
| --- | --- | --- |
| librime | `master` @ `4e6f83926963633e58da6059ebb129a3a7f0bd42`（2026-09-12） | 所有 C++ 引用均指此提交；行号为该提交下的行号 |
| rime/home wiki | `master` @ `1ea8e1c1284cfb9c1976b2a89ed47c510b0db1f0`（2026-09-01） | 本地副本 `.rime-wiki/` @ `5bfcf14`（2026-07-23） |
| 本机 librime | 1.16.1（`/usr/lib/x86_64-linux-gnu/librime.so.1`） | 用于实测（第 6 节） |

源码 URL 形如 `https://raw.githubusercontent.com/rime/librime/master/src/rime/...`。下文引用写作 `librime@4e6f839 <path>:<行>`。

**术语**（英文词首次出现时给出中文解释）

- **keysym / keycode（键值）**：一个按键的数字标识，来自 X11 的 `keysym.h`。librime 的 `KeyEvent::keycode_` 存的就是 keysym。
- **modifier（修饰键掩码）**：Shift / Control / Alt 等的位掩码，`KeyEvent::modifier_`。
- **binding（绑定）**：`accept`（接受的键）+ `when`（条件）+ 一个动作（`send` / `toggle` / …）。
- **processor（处理器）**：按键流水线的一环，见 `engine/processors` 列表。
- **segment（切分段）**：输入串上的一段区间，带 tag（标签）。

---

## 0. 先纠正一个前提：`kKeyTable` 不在 `key_binder.cc` 里

任务书假设 `key_binder.cc` 里有一张 `kKeyTable`。**没有。** `key_binder.cc` 全文 334 行，不含任何键名表；它只调用 `KeyEvent::Parse()`：

```cpp
// librime@4e6f839 src/rime/gear/key_binder.cc:179-185
    KeyEvent key;
    if (!key.Parse(pattern->str())) {
      LOG(WARNING) << "invalid key binding #" << i
                   << ", with invalid accept pattern: " << pattern->str()
                   << ".";
      continue;
    }
```

真正的键名表在 **`src/rime/key_table.cc`**，由 `key_event.cc` 使用。这一点很重要：**`accept:`、`send:`、`send_sequence:`、以及 `editor`/`selector`/`navigator` 的 `bindings` 键名、`ascii_composer/switch_key` 的键名，全部共用同一个 `KeyEvent::Parse`**。所以第 1 节的结论对整个方案文件都成立，不是 key_binder 专有。

---

## 1. `accept:` 的键名表与组合键解析

### 1.1 解析算法（`KeyEvent::Parse`）

```cpp
// librime@4e6f839 src/rime/key_event.cc:51-82
bool KeyEvent::Parse(const string& repr) {
  keycode_ = modifier_ = 0;
  if (repr.empty()) {
    return false;
  }
  if (repr.size() == 1) {
    keycode_ = static_cast<int>(repr[0]);          // ← 单字符捷径
  } else {
    size_t start = 0;
    size_t found = 0;
    string token;
    int mask = 0;
    while ((found = repr.find('+', start)) != string::npos) {
      token = repr.substr(start, found - start);
      mask = RimeGetModifierByName(token.c_str());
      if (mask) {
        modifier_ |= mask;
      } else {
        LOG(ERROR) << "parse error: unrecognized modifier '" << token << "'";
        return false;
      }
      start = found + 1;
    }
    token = repr.substr(start);
    keycode_ = RimeGetKeycodeByName(token.c_str());
    if (keycode_ == XK_VoidSymbol) {
      LOG(ERROR) << "parse error: unrecognized key '" << token << "'";
      return false;
    }
  }
  return true;
}
```

由这段代码可以推出 `Control+Shift+Return` 的解析规则，**逐条都是源码事实，不是约定**：

1. **`+` 是唯一的分隔符，且最后一个 `+` 之后的 token 是键名，前面的全是修饰键。** 每个修饰键 token 必须命中 `RimeGetModifierByName()`，否则**整个 `accept` 作废**（返回 false，`LoadBindings` 打印一条 WARNING 并 `continue`）。所以 `Control+Shift+Return` = `Control`(掩码 4) | `Shift`(掩码 1)，键名 `Return`。
2. **空字符串非法。**
3. **长度为 1 的字符串走捷径**，直接把该字节当 keysym，**根本不查表**。所以 `accept: a` 合法且等价于 `accept: 0x61`。注意：这意味着 `accept: "+"` 也走捷径（长度 1），得到 keysym 0x2b，而不是"修饰键分隔符"。
4. **长度 > 1 时，表里查不到就是非法**，没有大小写折叠、没有别名归一化——`RimeGetKeycodeByName` 是 `strcmp` 精确比对。
5. **`VoidSymbol` 永远查不到**：`RimeGetKeycodeByName` 的循环条件在比较之前就停在 `XK_VoidSymbol` 那一项上（`key_table.cc:2026-2033`），所以 `accept: VoidSymbol` 必然失败。

### 1.2 修饰键名（完整表）

```cpp
// librime@4e6f839 src/rime/key_table.cc:7-30
static const char* modifier_name[] = {
    "Shift",                            // 0
    "Lock",                             // 1
    "Control",                          // 2
    "Alt",                              // 3
    "Mod2",                             // 4
    "Mod3",                             // 5
    "Mod4",                             // 6
    "Mod5",                             // 7
    "Button1",                          // 8
    "Button2",                          // 9
    "Button3",                          // 10
    "Button4",                          // 11
    "Button5",                          // 12
    NULL,      NULL, NULL, NULL, NULL,  // 13 - 17
    NULL,      NULL, NULL, NULL, NULL,  // 18 - 22
    NULL,      NULL, NULL,              // 23 - 25
    "Super",                            // 26
    "Hyper",                            // 27
    "Meta",                             // 28
    NULL,                               // 29
    "Release",                          // 30
    NULL,                               // 31
};
```

配对的掩码定义在 `src/rime/key_table.h:14-46`（`kShiftMask=1<<0`、`kLockMask=1<<1`、`kControlMask=1<<2`、`kMod1Mask=kAltMask=1<<3`、…、`kSuperMask=1<<26`、`kHyperMask=1<<27`、`kMetaMask=1<<28`、`kReleaseMask=1<<30`）。

**可用的修饰键名一共 20 个**：`Shift` `Lock` `Control` `Alt` `Mod2` `Mod3` `Mod4` `Mod5` `Button1`…`Button5` `Super` `Hyper` `Meta` `Release`。索引 13–25 与 29、31 是 `NULL`，`RimeGetModifierByName` 会跳过它们，所以那些位置没有名字。

两点值得单独记住：

- **没有 `Cmd`、没有 `Command`、没有 `Ctrl` 简写。** macOS 前端把 Command 映射成 `Super` 或 `Meta`，方案里必须写 `Super+...`；写 `Cmd+...` 会让整条绑定被丢弃（只留一条 WARNING 日志）。
- **`Release` 是一个可以用在 `accept` 里的"修饰键"**（掩码 `1<<30`）。`KeyEvent::release()` 就查这一位（`key_event.h:34`）。`ascii_composer/switch_key: {Shift_L: ...}` 的默认语义依赖它——松开 Shift 才切模式。

### 1.3 键名表（完整）

`key_names[]` 是 X11 `keysym.h` 的完整名字表（`key_table.cc:32` 起），与 `keys_by_keyval[]`（`:1345` 起）按偏移配对。**实测统计：1306 个唯一名字。**

- **62 个单字符名字**：`0123456789` + `A-Z` + `a-z`。它们走 1.1 的第 3 条捷径，其实用不到表。
- **1244 个多字符名字**：全部 X11 keysym 名。常用的有 `space` `Return` `BackSpace` `Tab` `Escape` `Delete` `Insert` `Home` `End` `Prior`/`Page_Up` `Next`/`Page_Down` `Left` `Up` `Right` `Down` `comma` `period` `slash` `semicolon` `apostrophe` `grave` `minus` `equal` `bracketleft` `bracketright` `backslash` `Caps_Lock` `Shift_L` `Shift_R` `Control_L` `Control_R` `Alt_L` `Alt_R` `Super_L` `Super_R` `ISO_Left_Tab` `KP_Enter` `F1`–`F35`，以及各语言的 keysym（`Arabic_*`、`Cyrillic_*`、`Greek_*`、`Hangul_*`、`kana_*` 等）。

**完整列表见文末附录 A**（由 `key_table.cc` 的 `key_names[]` 直接抽出的机器生成清单，未做任何人工增删）。

### 1.4 反向：`repr()` 生成的规范写法

`KeyEvent::repr()`（`key_event.cc:19-49`）用于日志和 `KeySequence::repr()`。它的顺序是**先修饰键、后键名**（`modifiers.str() + name`），键名来自反向表 `keys_by_name`（按 keyval 排序，所以同一个 keyval 的多个别名里只有第一个会被打印，例如 0x27 会打印 `apostrophe` 而不是 `quoteright`）。查不到名字时退化为 `0x` + 4 位或 6 位十六进制（`0x12ab` / `0xfffffe`）。

注意 **`keys_by_keyval` 里同一个 keyval 可以出现多次**（别名），例如 `{0x000027, 58}`=`apostrophe` 与 `{0x000027, 69}=quoteright`，`{0x000060, 317}=grave` 与 `{0x000060, 323}=quoteleft`。`RimeGetKeycodeByName` 取**表中第一个**匹配项，所以 `accept: quoteright` 与 `accept: apostrophe` 得到同一个 keysym，是等价的。

---

## 2. 编辑器动作（`editor` / `express_editor` / `fluid_editor`）

### 2.1 一个必须先说清的结构事实

**`confirm`、`revert` 这些动作不属于 `key_binder`。** 它们属于另一个 processor：`editor`。两者的配置格式**完全不同**：

| | `key_binder` | `editor`（以及 `selector` / `navigator`） |
| --- | --- | --- |
| 配置项 | `key_binder/bindings` | `editor/bindings` |
| YAML 类型 | **列表**（`GetList`，`key_binder.cc:305`） | **映射**（`GetMap`，`key_binding_processor_impl.h:85`） |
| 一条的形状 | `- {when: …, accept: …, send: …}` | `键名: 动作名` |
| 实现类 | `KeyBinder` | `KeyBindingProcessor<T>`（模板） |

证据：

```cpp
// librime@4e6f839 src/rime/gear/key_binder.cc:301-307
void KeyBinder::LoadConfig() {
  if (!engine_)
    return;
  Config* config = engine_->schema()->config();
  if (auto bindings = config->GetList("key_binder/bindings"))
    key_bindings_->LoadBindings(bindings);
}
```
```cpp
// librime@4e6f839 src/rime/gear/key_binding_processor_impl.h:80-107
template <class T, int N>
void KeyBindingProcessor<T, N>::LoadConfig(Config* config,
                                           const string& section,
                                           int keymap_selector) {
  auto& keymap = get_keymap(keymap_selector);
  if (auto bindings = config->GetMap(section + "/bindings")) {
    for (auto it = bindings->begin(); it != bindings->end(); ++it) {
      auto value = As<ConfigValue>(it->second);
      if (!value)
        continue;
      auto* p = action_definitions_;
      while (p->action && p->name != value->str()) {
        ++p;
      }
      if (!p->action && p->name != value->str()) {
        LOG(WARNING) << "[" << section << "] invalid action: " << value->str();
        continue;
      }
      KeyEvent ke;
      if (!ke.Parse(it->first)) {
        LOG(WARNING) << "[" << section << "] invalid key: " << it->first;
        continue;
      }
      keymap.Bind(ke, p->action);
    }
  }
}
```

`editor/bindings` **没有 `when` 条件**——它只有一个键→动作的映射。（条件由 `Editor::ProcessKeyEvent` 自己施加，见 2.4。）

### 2.2 完整的编辑器动作表

```cpp
// librime@4e6f839 src/rime/gear/editor.cc:19-32
static Editor::ActionDef editor_action_definitions[] = {
    {"confirm", &Editor::Confirm},
    {"toggle_selection", &Editor::ToggleSelection},
    {"commit_comment", &Editor::CommitComment},
    {"commit_raw_input", &Editor::CommitRawInput},
    {"commit_script_text", &Editor::CommitScriptText},
    {"commit_composition", &Editor::CommitComposition},
    {"revert", &Editor::RevertLastEdit},
    {"back", &Editor::BackToPreviousInput},
    {"back_syllable", &Editor::BackToPreviousSyllable},
    {"delete_candidate", &Editor::DeleteCandidate},
    {"delete", &Editor::DeleteChar},
    {"cancel", &Editor::CancelComposition},
    Editor::kActionNoop};
```

**共 12 个真实动作 + 1 个 `noop`。** 任务书列出的 9 个都存在，另有 3 个任务书没列：`toggle_selection`、`commit_composition`、`back`。

`kActionNoop` 的定义与效果：

```cpp
// librime@4e6f839 src/rime/gear/key_binding_processor_impl.h:8-10
template <class T, int N>
const typename KeyBindingProcessor<T, N>::ActionDef
    KeyBindingProcessor<T, N>::kActionNoop = {"noop", nullptr};
```
```cpp
// librime@4e6f839 src/rime/gear/key_binding_processor_impl.h:70-78
template <class T, int N>
void KeyBindingProcessor<T, N>::Keymap::Bind(KeyEvent key_event,
                                             HandlerPtr action) {
  if (action) {
    (*this)[key_event] = action;
  } else {
    this->erase(key_event);       // ← noop 的真正含义：删掉这个键的默认绑定
  }
}
```

所以 `noop` 是"**解除默认绑定**"，不是"什么都不做的空动作"。

逐个动作（语出一句，尽量引原文）：

| 动作 | 源码 | 行为 |
| --- | --- | --- |
| `confirm` | `editor.cc:87-90` | `ctx->ConfirmCurrentSelection() \|\| ctx->Commit();` —— 有高亮候选就选中它，否则整串上屏。恒返回 `true`。 |
| `toggle_selection` | `editor.cc:92-95` | `ctx->ReopenPreviousSegment() \|\| ctx->ConfirmCurrentSelection();` —— 退回上一个已选段重新选，退不回去就确认当前选择。 |
| `commit_comment` | `editor.cc:97-105` | 把**当前选中候选的 `comment` 字段**直接上屏并 `ctx->Clear()`；候选没有 `comment` 时什么都不做。 |
| `commit_script_text` | `editor.cc:107-111` | `engine_->sink()(ctx->GetScriptText()); ctx->Clear();` —— 上屏 `Context` 里的 script text（经 `preedit_format` 变换后的编码文本），而不是候选中文字。 |
| `commit_raw_input` | `editor.cc:113-117` | `ctx->ClearNonConfirmedComposition(); ctx->Commit();` —— 丢掉未确认的组字、把已确认部分上屏。 |
| `commit_composition` | `editor.cc:119-123` | `if (!ctx->ConfirmCurrentSelection() \|\| !ctx->HasMenu()) ctx->Commit();` —— 确认选择；若确认后没有候选菜单了，就上屏。 |
| `revert` | `editor.cc:125-131` | `ctx->ReopenPreviousSelection() \|\| (ctx->PopInput() && ctx->ReopenPreviousSegment());` —— 撤回上一次选择；没有可撤的选择就删掉最后一个输入字符再退回上一段。 |
| `back` | `editor.cc:133-137` | `ctx->ReopenPreviousSegment() \|\| ctx->ReopenPreviousSelection() \|\| ctx->PopInput();` —— 退段 / 退选择 / 退一个输入字符，三级兜底。 |
| `back_syllable` | `editor.cc:155-160` | `ctx->ReopenPreviousSelection() \|\| ((pop_input_by_syllable(ctx) \|\| ctx->PopInput()) && ctx->ReopenPreviousSegment());` —— 按**音节**退（不是按字符）；`pop_input_by_syllable`（`:139-153`）用 `Phrase::spans().PreviousStop(caret_pos)` 找到上一个音节边界，找不到就退一个字符。 |
| `delete_candidate` | `editor.cc:162-165` | `ctx->DeleteCurrentSelection();` —— 从用户词典里删掉当前候选（学习型删除）。 |
| `delete` | `editor.cc:167-170` | `ctx->DeleteInput();` —— 删掉光标处一个输入字符。 |
| `cancel` | `editor.cc:172-176` | `if (!ctx->ClearPreviousSegment()) ctx->Clear();` —— 有上一段就先清上一段，否则整串清空（取消组字）。 |

补充：所有 Handler 的签名是 `bool Handler(Context*)`，返回值经 `Accept` 决定这次按键算不算被吃掉：

```cpp
// librime@4e6f839 src/rime/gear/key_binding_processor_impl.h:55-68
template <class T, int N>
bool KeyBindingProcessor<T, N>::Accept(const KeyEvent& key_event,
                                       Context* ctx,
                                       Keymap& keymap) {
  auto binding = keymap.find(key_event);
  if (binding != keymap.end()) {
    auto action = binding->second;
    if (RIME_THIS_CALL_AS(T, action)(ctx)) {
      DLOG(INFO) << "action key accepted: " << key_event.repr();
      return true;
    }
  }
  return false;
}
```

上面 12 个动作**全部恒返回 `true`**，所以只要键匹配上就一定被吃掉。（`selector` / `navigator` 的动作会返回 `false` 把按键让给别的组件，那是它们的事。）

### 2.3 默认键位

```cpp
// librime@4e6f839 src/rime/gear/editor.cc:189-203（FluidEditor，别名 fluency_editor）
    keymap.Bind({XK_space, 0}, &Editor::Confirm);
    keymap.Bind({XK_BackSpace, 0}, &Editor::BackToPreviousInput);
    keymap.Bind({XK_BackSpace, kControlMask}, &Editor::BackToPreviousSyllable);
    keymap.Bind({XK_Return, 0}, &Editor::CommitComposition);
    keymap.Bind({XK_Return, kControlMask}, &Editor::CommitRawInput);
    keymap.Bind({XK_Return, kShiftMask}, &Editor::CommitScriptText);
    keymap.Bind({XK_Return, kControlMask | kShiftMask}, &Editor::CommitComment);
    keymap.Bind({XK_Delete, 0}, &Editor::DeleteChar);
    keymap.Bind({XK_Delete, kControlMask}, &Editor::DeleteCandidate);
    keymap.Bind({XK_Escape, 0}, &Editor::CancelComposition);
    char_handler_ = &Editor::AddToInput;
```
```cpp
// librime@4e6f839 src/rime/gear/editor.cc:205-218（ExpressEditor）
    keymap.Bind({XK_space, 0}, &Editor::Confirm);
    keymap.Bind({XK_BackSpace, 0}, &Editor::RevertLastEdit);      // ← 与 fluid 不同
    keymap.Bind({XK_BackSpace, kControlMask}, &Editor::BackToPreviousSyllable);
    keymap.Bind({XK_Return, 0}, &Editor::CommitRawInput);         // ← 与 fluid 不同
    keymap.Bind({XK_Return, kControlMask}, &Editor::CommitScriptText);  // ← 与 fluid 不同
    keymap.Bind({XK_Return, kControlMask | kShiftMask}, &Editor::CommitComment);
    keymap.Bind({XK_Delete, 0}, &Editor::DeleteChar);
    keymap.Bind({XK_Delete, kControlMask}, &Editor::DeleteCandidate);
    keymap.Bind({XK_Escape, 0}, &Editor::CancelComposition);
    char_handler_ = &Editor::DirectCommit;                        // ← 与 fluid 不同
```

**fluent 与 express 的四处差异**（`editor.cc:189-218`）：

| | `fluid_editor` / `fluency_editor` | `express_editor` |
| --- | --- | --- |
| `BackSpace` | `back` | `revert` |
| `Return` | `commit_composition` | `commit_raw_input` |
| `Control+Return` | `commit_raw_input` | `commit_script_text` |
| 普通可打印字符（`char_handler`） | `add_to_input` | `direct_commit` |

**上游实物对照**：rime-ice 的 Lua-free 方案（正是我们这回实现的那套组件）在 `others/no_lua_schema/rime_ice.schema.yaml:183-194` 里显式写了一份 `editor`，用的就是我们上面推出的 **`express_editor`**（它的 `engine/processors` 末位是 `express_editor`）：

```yaml
# editor 用来定制操作键的行为，以下是默认行为 https://github.com/rime/librime/blob/master/src/rime/gear/editor.cc
editor:
  bindings:
    space: confirm                        # 空格键：上屏候选项
    Return: commit_raw_input              # 回车键：上屏原始输入
    Control+Return: commit_script_text    # Ctrl+回车键：上屏变换后输入（经过 preedit_format 转换的）
    Control+Shift+Return: commit_comment  # Ctrl+Shift+回车键：上屏 comment
    BackSpace: revert                     # 退格键：向前删除（撤消上次输入）
    Delete: delete                        # Delete 键：向后删除
    Control+BackSpace: back_syllable      # Ctrl+退格键：删除一个音节
    Control+Delete: delete_candidate      # Ctrl+Delete键：删除或降权候选项
    Escape: cancel                        # Esc 键：取消输入
```

这份 `editor/bindings` 与 `editor.cc:205-218` 的 `ExpressEditor` 默认键位**逐条一致**（这正对应它自己的注释「**以下是默认行为**」），并且它示范了三件我们在实现里要复现的事：**`editor/bindings` 是映射不是列表**、**键名走同一个 `KeyEvent::Parse`**（`Control+Shift+Return`）、**动作名必须精确命中 `editor_action_definitions`**。

`char_handler` 也可配置：`editor/char_handler` ∈ {`direct_commit`, `add_to_input`, `noop`}（`editor.cc:34-39` 的定义表、`:74-84` 的读取）。`auto_commit` 是构造参数（express=true、fluid=false），写进 `_auto_commit` 选项（`editor.cc:41-44`）。

### 2.4 什么时候才会查 `editor/bindings`

```cpp
// librime@4e6f839 src/rime/gear/editor.cc:46-66
ProcessResult Editor::ProcessKeyEvent(const KeyEvent& key_event) {
  if (key_event.release())
    return kRejected;
  int ch = key_event.keycode();
  Context* ctx = engine_->context();
  if (ctx->IsComposing()) {
    auto result = KeyBindingProcessor::ProcessKeyEvent(key_event, ctx, 0,
                                                       FallbackOptions::All);
    if (result != kNoop) {
      return result;
    }
  }
  if (char_handler_ && !key_event.ctrl() && !key_event.alt() &&
      !key_event.super() && ch > 0x20 && ch < 0x7f) {
    return RIME_THIS_CALL(char_handler_)(ctx, ch);
  }
  return kNoop;
}
```

三条硬规则：

1. **`editor/bindings` 只在 `ctx->IsComposing()` 为真时被查询**（`context.cc:48-50`：`return !input_.empty() || !composition_.empty();`）。没在组字时，敲空格、回车都不走编辑器绑定。
2. **按键松开事件（release）直接 `kRejected`**，不会走绑定。
3. **`FallbackOptions::All`**——找不到精确匹配时会依次试"Shift 当 Control"和"忽略 Shift"两种改写：

```cpp
// librime@4e6f839 src/rime/gear/key_binding_processor_impl.h:12-46
  // exact match
  if (Accept(key_event, ctx, keymap)) return kAccepted;
  // try to match the fallback options
  if (key_event.ctrl() || key_event.alt()) return kNoop;
  if (key_event.shift()) {
    if ((fallback_options & ShiftAsControl) != 0) {
      KeyEvent shift_as_control{key_event.keycode(),
                                (key_event.modifier() & ~kShiftMask) | kControlMask};
      if (Accept(shift_as_control, ctx, keymap)) return kAccepted;
    }
    if ((fallback_options & IgnoreShift) != 0) {
      KeyEvent ignore_shift{key_event.keycode(),
                            key_event.modifier() & ~kShiftMask};
      if (Accept(ignore_shift, ctx, keymap)) return kAccepted;
    }
  }
  return kNoop;
```

**这条有反直觉的后果，值得单列**：在 `express_editor` 下按 `Shift+Return`，精确匹配失败后先试 `ShiftAsControl`——于是命中 `Control+Return` = `commit_script_text`，**而不是** `Return` = `commit_raw_input`。要拿到 `commit_script_text` 其实按 `Shift+Return` 就够了，`Control+Shift+Return` 才是 `commit_comment`。

**同一节还隐含一条排序事实**：`LoadConfig` 是**覆盖式**写入（`(*this)[key_event] = action`），所以配置里的绑定直接盖掉上表里的默认绑定；`noop` 则是删除。

### 2.5 其它使用同一套动作机制的组件（对照，不是任务书要求，但同一配置格式）

```cpp
// librime@4e6f839 src/rime/gear/navigator.cc:21-34
static Navigator::ActionDef navigation_actions[] = {
    {"rewind", &Navigator::Rewind},
    {"forward", &Navigator::Forward},
    {"left_by_char", &Navigator::LeftByChar},
    {"right_by_char", &Navigator::RightByChar},
    {"left_by_syllable", &Navigator::LeftBySyllable},
    {"right_by_syllable", &Navigator::RightBySyllable},
    {"left_by_char_no_loop", &Navigator::LeftByCharNoLoop},
    {"right_by_char_no_loop", &Navigator::RightByCharNoLoop},
    {"left_by_syllable_no_loop", &Navigator::LeftBySyllableNoLoop},
    {"right_by_syllable_no_loop", &Navigator::RightBySyllableNoLoop},
    {"home", &Navigator::Home},
    {"end", &Navigator::End},
    Navigator::kActionNoop};
```
```cpp
// librime@4e6f839 src/rime/gear/selector.cc:19-27
static Selector::ActionDef selector_actions[] = {
    {"previous_candidate", &Selector::PreviousCandidate},
    {"next_candidate", &Selector::NextCandidate},
    {"previous_page", &Selector::PreviousPage},
    {"next_page", &Selector::NextPage},
    {"home", &Selector::Home},
    {"end", &Selector::End},
    Selector::kActionNoop};
```

`selector` 的配置有四个命名空间：`selector`、`selector/linear`、`selector/vertical`、`selector/vertical/linear`（`selector.cc:101-105`），按 `_vertical` / `_linear` 选项选一张键表；`navigator` 用 `navigator` 与 `navigator/vertical`（`navigator.cc:66-68`）。

---

## 3. `when:` 谓词

### 3.1 完整取值

```cpp
// librime@4e6f839 src/rime/gear/key_binder.cc:21-38
enum KeyBindingCondition {
  kNever,
  kWhenPredicting,  // showing prediction candidates
  kWhenPaging,      // user has changed page
  kWhenHasMenu,     // at least one candidate
  kWhenComposing,   // input string is not empty
  kAlways,
};

static struct KeyBindingConditionDef {
  KeyBindingCondition condition;
  const char* name;
} condition_definitions[] = {{kWhenPredicting, "predicting"},
                             {kWhenPaging, "paging"},
                             {kWhenHasMenu, "has_menu"},
                             {kWhenComposing, "composing"},
                             {kAlways, "always"},
                             {kNever, NULL}};
```

**是五个，不是四个**：`predicting`、`paging`、`has_menu`、`composing`、`always`。名字**必须精确相等**（`translate_condition` 是 `str == d->name`，`key_binder.cc:40-46`）；不认识的名字得到 `kNever`，`LoadBindings` 随即 `continue` 丢掉该条（`:176-178`）。

### 3.2 每个谓词实际测什么

```cpp
// librime@4e6f839 src/rime/gear/key_binder.cc:248-269
KeyBindingConditions::KeyBindingConditions(Context* ctx) {
  insert(kAlways);

  if (ctx->IsComposing()) {
    insert(kWhenComposing);
  }

  if (ctx->HasMenu() && !ctx->get_option("ascii_mode")) {
    insert(kWhenHasMenu);
  }

  Composition& comp = ctx->composition();
  if (!comp.empty()) {
    const Segment& last_seg = comp.back();
    if (last_seg.HasTag("paging")) {
      insert(kWhenPaging);
    }
    if (last_seg.HasTag("prediction")) {
      insert(kWhenPredicting);
    }
  }
}
```

| 谓词 | 实际测试 | 备注 |
| --- | --- | --- |
| `always` | 无条件 | **永远在集合里**，所以 `when: always` 的绑定在任何时刻都是候选。 |
| `composing` | `ctx->IsComposing()` = `!input_.empty() \|\| !composition_.empty()`（`context.cc:48-50`） | "输入串非空**或**切分非空"。 |
| `has_menu` | `ctx->HasMenu() && !ctx->get_option("ascii_mode")` | `HasMenu()` = 最后一段有 menu 且非空（`context.cc:52-57`）。**额外要求不在西文模式**——这一条容易漏。 |
| `paging` | 最后一段带 tag `"paging"` | 该 tag 由 `selector` 在翻页/移动高亮时打上（`selector.cc:167, 191, 213, 230`），另外 `switcher.cc:101` 与 `rime_api_impl.h:1026` 也会打。语义是"**用户已经翻过页**"，不是"菜单有多页"。 |
| `predicting` | 最后一段带 tag `"prediction"` | **`librime@4e6f839` 全树（`src/` + `plugins/`）搜 `"prediction"` 只找到 `key_binder.cc:265` 这一处读取，没有任何地方写入。** 也就是说：核心 librime 永远不会让这个谓词为真，它只对会给段打 `prediction` 标签的插件（如 librime-predict 一类）有意义。**未能取得**：我们未核实任何具体插件确实打这个标签。 |

### 3.3 多条绑定的优先级（容易踩）

```cpp
// librime@4e6f839 src/rime/gear/key_binder.cc:48-54
struct KeyBinding {
  KeyBindingCondition whence;
  KeySequence target;
  function<void(Engine* engine)> action;

  bool operator<(const KeyBinding& o) const { return whence < o.whence; }
};
```
```cpp
// librime@4e6f839 src/rime/gear/key_binder.cc:228-233
void KeyBindings::Bind(const KeyEvent& key, const KeyBinding& binding) {
  auto& vec = (*this)[key];
  // insert before existing binding of the same condition
  auto lb = std::lower_bound(vec.begin(), vec.end(), binding);
  vec.insert(lb, binding);
}
```
```cpp
// librime@4e6f839 src/rime/gear/key_binder.cc:278-286
  KeyBindingConditions conditions(engine_->context());
  for (const KeyBinding& binding : (*key_bindings_)[key_event]) {
    if (conditions.find(binding.whence) == conditions.end())
      continue;
    PerformKeyBinding(binding);
    return kAccepted;
  }
  // not handled
  return kNoop;
```

`operator<` **只比较 `whence`**，`lower_bound` 因此把新绑定插到同 `whence` 的已有绑定**之前**，向量按 `whence` 升序保持有序。取用时从头到尾取**第一条条件成立的**。合并结论：

1. **跨条件优先级 = 枚举值升序**：`predicting`(1) → `paging`(2) → `has_menu`(3) → `composing`(4) → `always`(5)。越"具体"越先试；`when: always` 永远排在最后，不会抢占更具体的同键绑定。
2. **同条件内，写在列表后面的赢**（因为它被后插入，落在前面）。
3. `import_preset` 的合并顺序让第 2 条变得有意义：预设的 `bindings` 在前，方案自己的 `bindings` 通过 `bindings/+` **追加在后**（见 4.4），所以**方案自己的同条件绑定优先于预设**。

### 3.4 一个不走配置的硬编码行为

`KeyBinder` 里还有一段写死的行为，与 `bindings` 无关：

```cpp
// librime@4e6f839 src/rime/gear/key_binder.cc:309-332
bool KeyBinder::ReinterpretPagingKey(const KeyEvent& key_event) {
  if (key_event.release())
    return false;
  bool ret = false;
  int ch = (key_event.modifier() == 0) ? key_event.keycode() : 0;
  // reinterpret period key followed by alphabetic keys
  // unless period/comma key has been used multiple times
  if (ch == '.' && (last_key_ == '.' || last_key_ == ',')) {
    last_key_ = 0;
    return ret;
  }
  if (last_key_ == '.' && ch >= 'a' && ch <= 'z') {
    Context* ctx = engine_->context();
    const string& input(ctx->input());
    if (!input.empty() && input[input.length() - 1] != '.') {
      LOG(INFO) << "reinterpreted key: '" << last_key_ << "', successor: '"
                << (char)ch << "'";
      ctx->PushInput(last_key_);
      ret = true;
    }
  }
  last_key_ = ch;
  return ret;
}
```

作用：**无修饰地敲 `.` 之后再敲一个 a–z 字母时，那个 `.` 会被"追认"进输入串**（前提是上一次按键不是 `.` 或 `,`，且输入串末尾不是 `.`）。目的是让"打网址/小数"这类输入不被标点处理器提前吃掉。它被放在 `ProcessKeyEvent` 的最前面（`:274-275`），命中时返回 `kNoop` —— 注意不是 `kAccepted`，所以按键会继续往后面的处理器走。

---

## 4. `send:` / `send_sequence:`，以及"发送一个按键"到底意味着什么

### 4.1 解析

```cpp
// librime@4e6f839 src/rime/gear/key_binder.cc:186-201
    if (auto target = map->GetValue("send")) {
      KeyEvent key;
      if (key.Parse(target->str())) {
        binding.target.push_back(std::move(key));
      } else {
        LOG(WARNING) << "invalid key binding #" << i
                     << ", with invalid send pattern: " << target->str() << ".";
        continue;
      }
    } else if (auto target = map->GetValue("send_sequence")) {
      if (!binding.target.Parse(target->str())) {
        LOG(WARNING) << "invalid key sequence #" << i
                     << ", with invalid send_sequence pattern: "
                     << target->str() << ".";
        continue;
      }
    } else if (auto option = map->GetValue("toggle")) {
```

- **两者解析成同一个东西**：`binding.target` 是一个 `KeySequence`（`vector<KeyEvent>`）。`send` 只是"只有一个元素的序列"。所以下游代码对二者完全一视同仁。
- `send` 的合法值 = 1.1 节 `KeyEvent::Parse` 的合法输入（单个字符，或 `修饰键+…+键名`）。
- `send_sequence` 的合法值 = `KeySequence::Parse` 的输入：

```cpp
// librime@4e6f839 src/rime/key_event.cc:111-138
bool KeySequence::Parse(const string& repr) {
  clear();
  size_t n = repr.size();
  size_t start = 0;
  size_t len = 0;
  KeyEvent ke;
  for (size_t i = 0; i < n; ++i) {
    if (repr[i] == '{' && i + 1 < n) {
      start = i + 1;
      size_t j = repr.find('}', start);
      if (j == string::npos) {
        LOG(ERROR) << "parse error: unparalleled brace in '" << repr << "'";
        return false;
      }
      len = j - start;
      i = j;
    } else {
      start = i;
      len = 1;
    }
    if (!ke.Parse(repr.substr(start, len))) {
      LOG(ERROR) << "parse error: unrecognized key sequence";
      return false;
    }
    push_back(ke);
  }
  return true;
}
```

规则：**逐字符扫描；`{键名}` 是一个整体 token；其余每个字符各自成一个 token，各交给 `KeyEvent::Parse`。** 所以：

- `send_sequence: "ni"` = 两个普通按键 `n`、`i`。
- `send_sequence: "{Caps_Lock}"` = 一个特殊键。
- `send_sequence: "{Control+Return}"` = 一个组合键；**组合键必须用花括号包起来**，因为裸写 `Control+Return` 会被拆成 `C`、`o`、`n`、… 逐字符解析。
- 空格、`{`、`}` 需要注意：花括号本身没有转义机制，`{` 一定会被当成 token 起始。`KeySequence::repr()`（`key_event.cc:95-109`）反向生成时会给"非可打印字符、组合键、`{`、`}`"加花括号。
- **`send` 与 `send_sequence` 是 `if / else if`**，同时写两个只会用 `send`。同理 `toggle` / `set_option` / `unset_option` / `select` 都排在后面，写了 `send` 就不会走它们。

### 4.2 "发送一个按键"的真正含义 —— 本节的核心结论

```cpp
// librime@4e6f839 src/rime/gear/key_binder.cc:289-299
void KeyBinder::PerformKeyBinding(const KeyBinding& binding) {
  if (binding.action) {
    binding.action(engine_);
  } else {
    redirecting_ = true;
    for (const KeyEvent& key_event : binding.target) {
      engine_->ProcessKey(key_event);
    }
    redirecting_ = false;
  }
}
```

```cpp
// librime@4e6f839 src/rime/engine.cc:99-122（节选）
bool ConcreteEngine::ProcessKey(const KeyEvent& key_event) {
  DLOG(INFO) << "process key: " << key_event;
  ProcessResult ret = kNoop;
  for (auto& processor : processors_) {
    ret = processor->ProcessKeyEvent(key_event);
    if (ret == kRejected)
      break;
    if (ret == kAccepted)
      return true;
  }
  context_->commit_history().Push(key_event);
  for (auto& processor : post_processors_) {
    ret = processor->ProcessKeyEvent(key_event);
    if (ret == kRejected)
      break;
    if (ret == kAccepted)
      return true;
  }
  context_->unhandled_key_notifier()(context_.get(), key_event);
  return false;
}
```

**结论：`send` 是把键重新送回 `Engine::ProcessKey`，也就是从整条 `processors_` 链的\*\*最开头\*\*重跑。** 不是"从 key_binder 之后往后跑"。

- 被发送的键会**再次经过 key_binder 之前的处理器**（`ascii_composer`、`recognizer`、以及更早的 `switcher_`）。
- 也会经过 key_binder 之后的处理器（`speller`、`punctuator`、`selector`、`navigator`、`editor`）。
- 序列是**一个键一个键顺序派发**的；每发一个键，引擎状态（上下文、选项、切分）都可能改变，进而影响下一个键的处理结果。
- 唯一的防重入是 `KeyBinder` 自己的布尔量：

```cpp
// librime@4e6f839 src/rime/gear/key_binder.h:29-31
  the<KeyBindings> key_bindings_;
  bool redirecting_;
  int last_key_;
```
```cpp
// librime@4e6f839 src/rime/gear/key_binder.cc:271-277
ProcessResult KeyBinder::ProcessKeyEvent(const KeyEvent& key_event) {
  if (redirecting_ || !key_bindings_ || key_bindings_->empty())
    return kNoop;
  if (ReinterpretPagingKey(key_event))
    return kNoop;
```
即：`redirecting_` 为真时 key_binder **只让自己跳过**（返回 `kNoop`，键继续往后走），别的处理器照常处理。**所以 `{accept: space, send: space}` 不会死循环**：`send` 出去的空格回到链头，key_binder 因为 `redirecting_` 直接放行，空格最终到达 `selector`/`express_editor`。

`redirecting_` 只在 `PerformKeyBinding` 里被置位，作用域是**一次** `PerformKeyBinding` 调用（含其中整个 target 序列）。序列内部的键不会触发 key_binder 的再绑定。

### 4.3 实测（真实 librime 1.16.1）

用本仓库自带的 `tools/librime-probe`，在一个隔离的 `/tmp` 副本里跑（**没有读写仓库里的任何文件**）。临时方案 `probe_send` 的处理器链是 `ascii_composer, recognizer, key_binder, speller, punctuator, selector, navigator, express_editor`，另配 `ascii_composer/switch_key: {Caps_Lock: set_ascii_mode}`，绑定：

```yaml
key_binder:
  bindings:
    - { when: always, accept: Control+Shift+K, send: Caps_Lock }
    - { when: always, accept: Control+Shift+J, send_sequence: "ni" }
    - { when: always, accept: Control+Shift+L, send: "Control+Shift+M" }
    - { when: always, accept: Control+Shift+M, send: Caps_Lock }
```

| 编号 | 按键 | 观察到的 `is_ascii_mode` | 说明 |
| --- | --- | --- | --- |
| T1 | `<Control+Shift+K>` | **1** | 发出的 `Caps_Lock` 交给了 **key_binder 之前的** `ascii_composer` ⇒ 确实从链头重跑 |
| T2 | `<Control+Shift+J>` | 0，`preedit="ni"` | 序列里的两个普通键都到达了 key_binder 之后的 `speller` ⇒ 整段序列按顺序派发 |
| T3 | `<Control+Shift+L>`（发 `Control+Shift+M`，而 M 本身是一条 key_binder 绑定） | **0** | 发出的键**没有**再次进入 key_binder ⇒ `redirecting_` 防重入生效 |
| T3b | `<Control+Shift+M>` 直接按 | **1** | 对照组：证明 T3 的 0 是防重入造成的，不是绑定写错 |

T1 是最关键的一条：如果 librime 像"从 key_binder 之后继续"那样实现，`Caps_Lock` 永远不会被 `ascii_composer` 看到，`is_ascii_mode` 会保持 0。

### 4.4 附带：`import_preset` 的合并方式

```cpp
// librime@4e6f839 src/rime/config/legacy_preset_config_plugin.cc:22-41
  if (auto preset = resource->data->Traverse("key_binder/import_preset")) {
    if (!Is<ConfigValue>(preset))
      return false;
    auto preset_config_id = As<ConfigValue>(preset)->str();
    LOG(INFO) << "interpreting key_binder/import_preset: " << preset_config_id;
    auto target = Cow(resource, "key_binder");
    auto map = As<ConfigMap>(**target);
    if (map && map->HasKey("bindings")) {
      // append to included list `key_binder/bindings/+` instead of overwriting
      auto appended = map->Get("bindings");
      *Cow(target, "bindings/+") = appended;
      // `*target` is already referencing a copied map, safe to edit directly
      (*target)["bindings"] = nullptr;
    }
    Reference reference{preset_config_id, "key_binder", false};
    if (!IncludeReference{reference}.TargetedAt(target).Resolve(compiler)) {
      LOG(ERROR) << "failed to include section " << reference;
      return false;
    }
  }
```

`key_binder/import_preset: default` 是**列表追加**（`bindings/+`），语义是「预设的 `bindings` 在前，方案自己写的 `bindings` 追加在后」。这也是 §3.3 第 2/3 条能起作用的原因。（对比：`punctuator/import_preset` 与 `recognizer/import_preset` 是普通 `__include` 式合并，`:48-73`。）

系统预设 `key_bindings.yaml`（`/usr/share/rime-data/key_bindings.yaml`）里的真实条目可以作为写法样例：

```yaml
paging_with_comma_period:
  __append:
    - { when: paging, accept: comma, send: Page_Up }
    - { when: has_menu, accept: period, send: Page_Down }

emacs_editing:
  __append:
    - { when: composing, accept: Control+p, send: Up }
    - { when: composing, accept: Control+k, send: Shift+Delete }
    - { when: composing, accept: Control+h, send: BackSpace }

numbered_mode_switch:
  __append:
    - { when: always, accept: Control+Shift+1, select: .next }
    - { when: always, accept: Control+Shift+2, toggle: ascii_mode }
    - { when: always, accept: Control+Shift+exclam, select: .next }
    - { when: always, accept: Control+Shift+at, toggle: ascii_mode }
```

---

## 5. `toggle:` 及其它 action 形态

### 5.1 判定分支

`LoadBindings` 是严格的 `if / else if` 链，优先级为：`send` → `send_sequence` → `toggle` → `set_option` → `unset_option` → `select`。**一个都不匹配就丢弃该条并打 WARNING**（`key_binder.cc:218-223`）。

`send` / `send_sequence` 走"重发按键"路径（`binding.action` 为空）；其余四个走"直接改引擎状态"路径（`binding.action` 非空）。

### 5.2 `toggle` 的精确语义

```cpp
// librime@4e6f839 src/rime/gear/key_binder.cc:88-117
static void toggle_option(Engine* engine, const string& option) {
  if (!engine)
    return;
  Context* ctx = engine->context();
  Switches switches(engine->schema()->config());
  auto the_option = is_switch_index(option) ? switch_by_index(switches, option)
                                            : switches.OptionByName(option);
  if (the_option.found() && the_option.type == Switches::kRadioGroup) {
    auto selected_option = switches.FindRadioGroupOption(
        the_option.the_switch, [ctx](Switches::SwitchOption option) {
          return ctx->get_option(option.option_name) ? Switches::kFound
                                                     : Switches::kContinue;
        });
    if (!selected_option.found()) {
      // invalid state: none is selected. select the given option.
      radio_select_option(ctx, the_option);
      return;
    }
    // cycle through the ratio group and select the next option.
    auto next_option = Switches::Cycle(selected_option);
    if (next_option.found()) {
      radio_select_option(ctx, next_option);
    }
  } else {  // toggle
    // option can be an index. use the found option name, or an arbitrary
    // option name specified by caller.
    auto option_name = the_option.found() ? the_option.option_name : option;
    ctx->set_option(option_name, !ctx->get_option(option_name));
  }
}
```

拆开说：

1. **`toggle` 的值可以是一个开关名，也可以是 `@数字` 形式的开关序号。** `is_switch_index`（`:74-76`）判断首字符是否为 `@`；`switch_by_index`（`:78-86`）用 `std::stoul` 解析 `@` 之后的十进制数并调 `Switches::ByIndex`。解析失败会走 `catch (...)` 得到"未找到"。
2. **开关在 `switches:` 里存在且是单选组（radio group，即带 `states` 且被声明为互斥组）**：`toggle` 的含义是"**轮到组里的下一个选项**"（`Switches::Cycle`），不是"取反"。典型例子是 `simplification` 这类（若被声明为单选组）。
3. **其它情况（普通布尔开关）**：`ctx->set_option(option_name, !ctx->get_option(option_name))` —— 真正的取反。
4. **开关名没找到时不会报错**：`the_option.found()` 为假，`option_name` 退化为**调用方给的字符串本身**，于是 `toggle: whatever` 会创建一个名为 `whatever` 的临时布尔选项并取反。`engine/schema` 里没声明过它，也可能被后面的组件用 `ctx->get_option("whatever")` 读到。

### 5.3 `set_option` / `unset_option` / `select`

```cpp
// librime@4e6f839 src/rime/gear/key_binder.cc:119-159
static void set_option(Engine* engine, const string& option) {
  ...
  auto the_option = switches.OptionByName(option);
  if (the_option.found() && the_option.type == Switches::kRadioGroup) {
    radio_select_option(ctx, the_option);
  } else {
    ctx->set_option(option, 1);
  }
}

static void unset_option(Engine* engine, const string& option) {
  ...
  auto the_option = switches.OptionByName(option);
  if (the_option.found() && the_option.type == Switches::kRadioGroup) {
    if (ctx->get_option(option)) {
      auto default_option = Switches::Reset(the_option);
      if (default_option.found()) {
        radio_select_option(ctx, default_option);
      }
    }
  } else {
    ctx->set_option(option, 0);
  }
}

static void select_schema(Engine* engine, const string& schema) {
  if (!engine)
    return;
  if (schema == ".next") {
    Switcher switcher(engine);
    switcher.SelectNextSchema();
  } else {
    engine->ApplySchema(new Schema(schema));
  }
}
```

- **`set_option` 只按名字查（不认 `@序号`）**，置为开。单选组则直接选中该选项。
- **`unset_option` 只按名字查**，置为关；单选组则"若当前选的是它，就回到 `Switches::Reset` 给出的默认项"。
- **`select: .next` 是唯一被特殊识别的值**（切换下一个方案）；其它任何值都被当作**方案 id** 传给 `engine->ApplySchema(new Schema(schema))`。方案名不存在时**不会有任何报错**：本仓库 `tools/librime-probe/README.md`（坑 5）已在本机 librime 1.16.1 上实测到 `select_schema()` 对不存在的方案名同样返回 True、`get_current_schema()` 原样回显该名字，会话进入"没有引擎"的假死状态（所有按键 `handled=0`、候选恒空，**退出码仍是 0**）。
- `toggle` / `set_option` 会触发 `option_update_notifier` → `ConcreteEngine::OnOptionUpdate`（`engine.cc:130-142`）：**如果正在组字，会 `RefreshNonConfirmedComposition()`**，也就是直接改变当前候选列表；并向外发一条 `option` 消息。

`set_option` / `unset_option` 不在 wiki 里；`toggle` 与 `select` 只在系统预设 `key_bindings.yaml` 里有例子。**wiki（CustomizationGuide / Configuration / RimeWithSchemata）对 `send_sequence`、`set_option`、`unset_option`、`select`、`when: predicting` 均无任何记载**——本轮检索在这些页面里 0 命中。它们只存在于源码里。

---

## 6. 与我们（qingjian）实现的差异

**这一节是快照，会过期。** 本文件写作期间，工作区里的另一个任务正在改 `crates/qingjian-engine/`（提交 `a8a366d`，之后 `processor.rs` / `spec.rs` 又有未提交改动）。下表核对的是**本文件落笔时磁盘上的实际代码**，已尽量区分「已修」「部分修」「未修」。**只做记录，未修改任何实现文件。**

| # | 主题 | librime 的事实 | qingjian 现状（写作时） | 结论 |
| --- | --- | --- | --- | --- |
| 1 | `send` 的重派发起点 | `engine_->ProcessKey()`，从 `processors_` **链头**重跑，唯一的例外是 key_binder 自己（`redirecting_` 布尔） | **部分修**。`processor.rs:577-583` 已加入 `redirecting` 标志，且 `:548-576` 的文档已正确引用 librime 并把旧做法记为"原先我写错了"。但 `pipeline.rs:333-352` 仍用 `self.dispatch(state, &next, self.binder_index)` 从 key_binder **之后**派发，并仍以 `REBIND_ROUNDS`（`:53`）限轮数 | **还差一半。** 只要 pipeline 那一层不改成"整链重跑、把 `redirecting` 交给 KeyBinder 自己看"，`send` 换来的键在前面那些处理器（`ascii_composer` / `recognizer`）眼里仍等于没发生过——实测 T1 正是这种情形 |
| 2 | `send_sequence` | 与 `send` 同源，都是 `binding.target`（`KeySequence`），**逐键顺序**派发 | **已修**。`spec.rs:296-304` 的 `send_keys: Option<Vec<String>>` 把二者统一成一个序列，注释明确写了"librime 的 `binding.target` 就是一个 `KeySequence`"；`processor.rs:578` 用 `state.sent_keys.extend(keys)` | 一致 |
| 3 | `when: predicting` | 合法谓词之一（`key_binder.cc:33`）；但核心 librime **没有任何地方写这个标签**（§3.2） | **已修**。`spec.rs:326-330` 收录了 `Predicting`，注释写明"目前永远为假…留在这里是为了 RIME 的方案能原样读进来" | 一致，且我们对"它为什么永远为假"的理解比 wiki 更准 |
| 4 | `set_option` / `unset_option` / `select` | 三个独立 action，`select: .next` 特殊（§5.3） | **未实现**。`spec.rs:291-309` 的 `KeyBinding` 只有 `send_keys` 与 `toggle` | 缺功能。系统预设 `key_bindings.yaml` 的 `numbered_mode_switch` 整组依赖 `toggle` + `select`（rime-ice 的 `import_preset: default` 会把它带进来） |
| 5 | `editor` 动作 | 12 个（含 `toggle_selection`、`commit_composition`、`back`），另有 `noop`（§2.2） | **已覆盖**。`spec.rs:133-148` 解析全部 12 个名字；`back_syllable` 另有别名 `back_unit`（`:140`） | 一致。**待核对**：`noop` 是否实现为"**删除**该键的默认绑定"而不是"什么都不做"（`key_binding_processor_impl.h:70-78`） |
| 6 | `editor` 的 `FallbackOptions::All` | `Shift+Return` 在 `express_editor` 下先按 `ShiftAsControl` 命中 `Control+Return` = `commit_script_text`（§2.4） | **未见实现**（在 `crates/qingjian-engine/src/` 下搜 `ShiftAsControl` / `IgnoreShift` 无命中） | 缺功能，且反直觉：我们多半会把 `Shift+Return` 当成"没绑定" |
| 7 | `editor/bindings` 的类型 | **映射**（`键名: 动作`），而 `key_binder/bindings` 是**列表**（§2.1） | 已意识到差别：`spec.rs:114-119` 的注释写明"`editor` 有一张**默认绑定表**…我们是按'整个替换默认表'实现 `editor/bindings` 的" | **语义差异**：librime 是**逐键覆盖**（`(*this)[key_event] = action`，未提到的键保留默认），我们是**整表替换**。只用 rime-ice 那份"完整默认表"时看不出差别，但用户只写一行 `editor/bindings: {Return: commit_comment}` 时两边行为不同 |
| 8 | `when: has_menu` | 额外要求 `!ascii_mode`（§3.2） | 待核对 | 西文模式下 `has_menu` 绑定不应生效 |
| 9 | `VoidSymbol` | `RimeGetKeycodeByName` 永不可达（§1.1 第 5 条） | `keyspec.rs` 已按 librime 的解析规则重写（含"单字符捷径"，`keyspec.rs:82, 135, 189`） | 待核对是否也复刻了 `VoidSymbol` 这一条边界 |
| 10 | `ReinterpretPagingKey` | `.` + 字母的"追认"是**硬编码**行为，与 `bindings` 无关（§3.4） | 待核对 | 影响网址/小数输入 |
| 11 | 多条绑定的优先级 | 跨条件按 `whence` 升序（`always` 最后），同条件内**后写的赢**（§3.3） | 待核对 | 写反了会让 `when: always` 抢掉更具体的绑定 |

---

## 附录 A：`accept:` / `send:` 可用键名完整清单

来源：`librime@4e6f839 src/rime/key_table.cc` 的 `key_names[]`（`key_table.cc:32` 起），逐字抽取、去重、排序，未做人工增删。共 **1306** 个名字。

- **单字符（62 个）**：`0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz`（任何一个单字符都能直接当键名用，见 §1.1 第 3 条）
- **多字符（1244 个）**：见下方清单。`VoidSymbol` 虽然在表里，但**不可用**（§1.1 第 5 条）。

```text
3270_AltCursor  3270_Attn  3270_BackTab  3270_ChangeScreen  3270_Copy  3270_CursorBlink
3270_CursorSelect  3270_DeleteWord  3270_Duplicate  3270_Enter  3270_EraseEOF  3270_EraseInput
3270_ExSelect  3270_FieldMark  3270_Ident  3270_Jump  3270_KeyClick  3270_Left2  3270_PA1  3270_PA2
3270_PA3  3270_Play  3270_PrintScreen  3270_Quit  3270_Record  3270_Reset  3270_Right2  3270_Rule
3270_Setup  3270_Test  AE  Aacute  Abreve  AccessX_Enable  AccessX_Feedback_Enable  Acircumflex
Adiaeresis  Agrave  Alt_L  Alt_R  Amacron  Aogonek  Arabic_ain  Arabic_alef  Arabic_alefmaksura
Arabic_beh  Arabic_comma  Arabic_dad  Arabic_dal  Arabic_damma  Arabic_dammatan  Arabic_fatha
Arabic_fathatan  Arabic_feh  Arabic_ghain  Arabic_ha  Arabic_hah  Arabic_hamza  Arabic_hamzaonalef
Arabic_hamzaonwaw  Arabic_hamzaonyeh  Arabic_hamzaunderalef  Arabic_heh  Arabic_jeem  Arabic_kaf
Arabic_kasra  Arabic_kasratan  Arabic_khah  Arabic_lam  Arabic_maddaonalef  Arabic_meem  Arabic_noon
Arabic_qaf  Arabic_question_mark  Arabic_ra  Arabic_sad  Arabic_seen  Arabic_semicolon
Arabic_shadda  Arabic_sheen  Arabic_sukun  Arabic_switch  Arabic_tah  Arabic_tatweel  Arabic_teh
Arabic_tehmarbuta  Arabic_thal  Arabic_theh  Arabic_waw  Arabic_yeh  Arabic_zah  Arabic_zain  Aring
Atilde  AudibleBell_Enable  BackSpace  Begin  BounceKeys_Enable  Break  Byelorussian_SHORTU
Byelorussian_shortu  Cabovedot  Cacute  Cancel  Caps_Lock  Ccaron  Ccedilla  Ccircumflex  Clear
Codeinput  ColonSign  Control_L  Control_R  CruzeiroSign  Cyrillic_A  Cyrillic_BE  Cyrillic_CHE
Cyrillic_DE  Cyrillic_DZHE  Cyrillic_E  Cyrillic_EF  Cyrillic_EL  Cyrillic_EM  Cyrillic_EN
Cyrillic_ER  Cyrillic_ES  Cyrillic_GHE  Cyrillic_HA  Cyrillic_HARDSIGN  Cyrillic_I  Cyrillic_IE
Cyrillic_IO  Cyrillic_JE  Cyrillic_KA  Cyrillic_LJE  Cyrillic_NJE  Cyrillic_O  Cyrillic_PE
Cyrillic_SHA  Cyrillic_SHCHA  Cyrillic_SHORTI  Cyrillic_SOFTSIGN  Cyrillic_TE  Cyrillic_TSE
Cyrillic_U  Cyrillic_VE  Cyrillic_YA  Cyrillic_YERU  Cyrillic_YU  Cyrillic_ZE  Cyrillic_ZHE
Cyrillic_a  Cyrillic_be  Cyrillic_che  Cyrillic_de  Cyrillic_dzhe  Cyrillic_e  Cyrillic_ef
Cyrillic_el  Cyrillic_em  Cyrillic_en  Cyrillic_er  Cyrillic_es  Cyrillic_ghe  Cyrillic_ha
Cyrillic_hardsign  Cyrillic_i  Cyrillic_ie  Cyrillic_io  Cyrillic_je  Cyrillic_ka  Cyrillic_lje
Cyrillic_nje  Cyrillic_o  Cyrillic_pe  Cyrillic_sha  Cyrillic_shcha  Cyrillic_shorti
Cyrillic_softsign  Cyrillic_te  Cyrillic_tse  Cyrillic_u  Cyrillic_ve  Cyrillic_ya  Cyrillic_yeru
Cyrillic_yu  Cyrillic_ze  Cyrillic_zhe  Dcaron  Delete  DongSign  Down  Dstroke  ENG  ETH  Eabovedot
Eacute  Ecaron  Ecircumflex  EcuSign  Ediaeresis  Egrave  Eisu_Shift  Eisu_toggle  Emacron  End
Eogonek  Escape  Eth  EuroSign  Execute  F1  F10  F11  F12  F13  F14  F15  F16  F17  F18  F19  F2
F20  F21  F22  F23  F24  F25  F26  F27  F28  F29  F3  F30  F31  F32  F33  F34  F35  F4  F5  F6  F7
F8  F9  FFrancSign  Find  First_Virtual_Screen  Gabovedot  Gbreve  Gcedilla  Gcircumflex
Greek_ALPHA  Greek_ALPHAaccent  Greek_BETA  Greek_CHI  Greek_DELTA  Greek_EPSILON
Greek_EPSILONaccent  Greek_ETA  Greek_ETAaccent  Greek_GAMMA  Greek_IOTA  Greek_IOTAaccent
Greek_IOTAdiaeresis  Greek_IOTAdieresis  Greek_KAPPA  Greek_LAMBDA  Greek_LAMDA  Greek_MU  Greek_NU
Greek_OMEGA  Greek_OMEGAaccent  Greek_OMICRON  Greek_OMICRONaccent  Greek_PHI  Greek_PI  Greek_PSI
Greek_RHO  Greek_SIGMA  Greek_TAU  Greek_THETA  Greek_UPSILON  Greek_UPSILONaccent
Greek_UPSILONdieresis  Greek_XI  Greek_ZETA  Greek_accentdieresis  Greek_alpha  Greek_alphaaccent
Greek_beta  Greek_chi  Greek_delta  Greek_epsilon  Greek_epsilonaccent  Greek_eta  Greek_etaaccent
Greek_finalsmallsigma  Greek_gamma  Greek_horizbar  Greek_iota  Greek_iotaaccent
Greek_iotaaccentdieresis  Greek_iotadieresis  Greek_kappa  Greek_lambda  Greek_lamda  Greek_mu
Greek_nu  Greek_omega  Greek_omegaaccent  Greek_omicron  Greek_omicronaccent  Greek_phi  Greek_pi
Greek_psi  Greek_rho  Greek_sigma  Greek_switch  Greek_tau  Greek_theta  Greek_upsilon
Greek_upsilonaccent  Greek_upsilonaccentdieresis  Greek_upsilondieresis  Greek_xi  Greek_zeta
Hangul  Hangul_A  Hangul_AE  Hangul_AraeA  Hangul_AraeAE  Hangul_Banja  Hangul_Cieuc  Hangul_Dikeud
Hangul_E  Hangul_EO  Hangul_EU  Hangul_End  Hangul_Hanja  Hangul_Hieuh  Hangul_I  Hangul_Ieung
Hangul_J_Cieuc  Hangul_J_Dikeud  Hangul_J_Hieuh  Hangul_J_Ieung  Hangul_J_Jieuj  Hangul_J_Khieuq
Hangul_J_Kiyeog  Hangul_J_KiyeogSios  Hangul_J_KkogjiDalrinIeung  Hangul_J_Mieum  Hangul_J_Nieun
Hangul_J_NieunHieuh  Hangul_J_NieunJieuj  Hangul_J_PanSios  Hangul_J_Phieuf  Hangul_J_Pieub
Hangul_J_PieubSios  Hangul_J_Rieul  Hangul_J_RieulHieuh  Hangul_J_RieulKiyeog  Hangul_J_RieulMieum
Hangul_J_RieulPhieuf  Hangul_J_RieulPieub  Hangul_J_RieulSios  Hangul_J_RieulTieut  Hangul_J_Sios
Hangul_J_SsangKiyeog  Hangul_J_SsangSios  Hangul_J_Tieut  Hangul_J_YeorinHieuh  Hangul_Jamo
Hangul_Jeonja  Hangul_Jieuj  Hangul_Khieuq  Hangul_Kiyeog  Hangul_KiyeogSios
Hangul_KkogjiDalrinIeung  Hangul_Mieum  Hangul_Nieun  Hangul_NieunHieuh  Hangul_NieunJieuj  Hangul_O
Hangul_OE  Hangul_PanSios  Hangul_Phieuf  Hangul_Pieub  Hangul_PieubSios  Hangul_PostHanja
Hangul_PreHanja  Hangul_Rieul  Hangul_RieulHieuh  Hangul_RieulKiyeog  Hangul_RieulMieum
Hangul_RieulPhieuf  Hangul_RieulPieub  Hangul_RieulSios  Hangul_RieulTieut  Hangul_RieulYeorinHieuh
Hangul_Romaja  Hangul_Sios  Hangul_Special  Hangul_SsangDikeud  Hangul_SsangJieuj
Hangul_SsangKiyeog  Hangul_SsangPieub  Hangul_SsangSios  Hangul_Start  Hangul_SunkyeongeumMieum
Hangul_SunkyeongeumPhieuf  Hangul_SunkyeongeumPieub  Hangul_Tieut  Hangul_U  Hangul_WA  Hangul_WAE
Hangul_WE  Hangul_WEO  Hangul_WI  Hangul_YA  Hangul_YAE  Hangul_YE  Hangul_YEO  Hangul_YI  Hangul_YO
Hangul_YU  Hangul_YeorinHieuh  Hangul_switch  Hankaku  Hcircumflex  Hebrew_switch  Help  Henkan
Henkan_Mode  Hiragana  Hiragana_Katakana  Home  Hstroke  Hyper_L  Hyper_R  ISO_Center_Object
ISO_Continuous_Underline  ISO_Discontinuous_Underline  ISO_Emphasize  ISO_Enter
ISO_Fast_Cursor_Down  ISO_Fast_Cursor_Left  ISO_Fast_Cursor_Right  ISO_Fast_Cursor_Up
ISO_First_Group  ISO_First_Group_Lock  ISO_Group_Latch  ISO_Group_Lock  ISO_Group_Shift
ISO_Last_Group  ISO_Last_Group_Lock  ISO_Left_Tab  ISO_Level2_Latch  ISO_Level3_Latch
ISO_Level3_Lock  ISO_Level3_Shift  ISO_Lock  ISO_Move_Line_Down  ISO_Move_Line_Up  ISO_Next_Group
ISO_Next_Group_Lock  ISO_Partial_Line_Down  ISO_Partial_Line_Up  ISO_Partial_Space_Left
ISO_Partial_Space_Right  ISO_Prev_Group  ISO_Prev_Group_Lock  ISO_Release_Both_Margins
ISO_Release_Margin_Left  ISO_Release_Margin_Right  ISO_Set_Margin_Left  ISO_Set_Margin_Right
Iabovedot  Iacute  Icircumflex  Idiaeresis  Igrave  Imacron  Insert  Iogonek  Itilde  Jcircumflex
KP_0  KP_1  KP_2  KP_3  KP_4  KP_5  KP_6  KP_7  KP_8  KP_9  KP_Add  KP_Begin  KP_Decimal  KP_Delete
KP_Divide  KP_Down  KP_End  KP_Enter  KP_Equal  KP_F1  KP_F2  KP_F3  KP_F4  KP_Home  KP_Insert
KP_Left  KP_Multiply  KP_Next  KP_Page_Down  KP_Page_Up  KP_Prior  KP_Right  KP_Separator  KP_Space
KP_Subtract  KP_Tab  KP_Up  Kana_Lock  Kana_Shift  Kanji  Katakana  Kcedilla  Korean_Won  Lacute
Last_Virtual_Screen  Lcaron  Lcedilla  Left  Linefeed  LiraSign  Lstroke  Macedonia_DSE
Macedonia_GJE  Macedonia_KJE  Macedonia_dse  Macedonia_gje  Macedonia_kje  Massyo  Menu  Meta_L
Meta_R  MillSign  Mode_switch  MouseKeys_Accel_Enable  MouseKeys_Enable  Muhenkan  Multi_key
MultipleCandidate  Nacute  NairaSign  Ncaron  Ncedilla  NewSheqelSign  Next  Next_Virtual_Screen
Ntilde  Num_Lock  OE  Oacute  Ocircumflex  Odiaeresis  Odoubleacute  Ograve  Omacron  Ooblique
Otilde  Overlay1_Enable  Overlay2_Enable  Page_Down  Page_Up  Pause  PesetaSign  Pointer_Accelerate
Pointer_Button1  Pointer_Button2  Pointer_Button3  Pointer_Button4  Pointer_Button5
Pointer_Button_Dflt  Pointer_DblClick1  Pointer_DblClick2  Pointer_DblClick3  Pointer_DblClick4
Pointer_DblClick5  Pointer_DblClick_Dflt  Pointer_DfltBtnNext  Pointer_DfltBtnPrev  Pointer_Down
Pointer_DownLeft  Pointer_DownRight  Pointer_Drag1  Pointer_Drag2  Pointer_Drag3  Pointer_Drag4
Pointer_Drag5  Pointer_Drag_Dflt  Pointer_EnableKeys  Pointer_Left  Pointer_Right  Pointer_Up
Pointer_UpLeft  Pointer_UpRight  Prev_Virtual_Screen  PreviousCandidate  Print  Prior  Racute
Rcaron  Rcedilla  Redo  RepeatKeys_Enable  Return  Right  Romaji  RupeeSign  Sacute  Scaron
Scedilla  Scircumflex  Scroll_Lock  Select  Serbian_DJE  Serbian_DZE  Serbian_JE  Serbian_LJE
Serbian_NJE  Serbian_TSHE  Serbian_dje  Serbian_dze  Serbian_je  Serbian_lje  Serbian_nje
Serbian_tshe  Shift_L  Shift_Lock  Shift_R  SingleCandidate  SlowKeys_Enable  StickyKeys_Enable
Super_L  Super_R  Sys_Req  THORN  Tab  Tcaron  Tcedilla  Terminate_Server  Thai_baht  Thai_bobaimai
Thai_chochan  Thai_chochang  Thai_choching  Thai_chochoe  Thai_dochada  Thai_dodek  Thai_fofa
Thai_fofan  Thai_hohip  Thai_honokhuk  Thai_khokhai  Thai_khokhon  Thai_khokhuat  Thai_khokhwai
Thai_khorakhang  Thai_kokai  Thai_lakkhangyao  Thai_lekchet  Thai_lekha  Thai_lekhok  Thai_lekkao
Thai_leknung  Thai_lekpaet  Thai_leksam  Thai_leksi  Thai_leksong  Thai_leksun  Thai_lochula
Thai_loling  Thai_lu  Thai_maichattawa  Thai_maiek  Thai_maihanakat  Thai_maihanakat_maitho
Thai_maitaikhu  Thai_maitho  Thai_maitri  Thai_maiyamok  Thai_moma  Thai_ngongu  Thai_nikhahit
Thai_nonen  Thai_nonu  Thai_oang  Thai_paiyannoi  Thai_phinthu  Thai_phophan  Thai_phophung
Thai_phosamphao  Thai_popla  Thai_rorua  Thai_ru  Thai_saraa  Thai_saraaa  Thai_saraae
Thai_saraaimaimalai  Thai_saraaimaimuan  Thai_saraam  Thai_sarae  Thai_sarai  Thai_saraii
Thai_sarao  Thai_sarau  Thai_saraue  Thai_sarauee  Thai_sarauu  Thai_sorusi  Thai_sosala  Thai_soso
Thai_sosua  Thai_thanthakhat  Thai_thonangmontho  Thai_thophuthao  Thai_thothahan  Thai_thothan
Thai_thothong  Thai_thothung  Thai_topatak  Thai_totao  Thai_wowaen  Thai_yoyak  Thai_yoying  Thorn
Touroku  Tslash  Uacute  Ubreve  Ucircumflex  Udiaeresis  Udoubleacute  Ugrave  Ukrainian_I
Ukrainian_IE  Ukrainian_YI  Ukrainian_i  Ukrainian_ie  Ukrainian_yi  Ukranian_I  Ukranian_JE
Ukranian_YI  Ukranian_i  Ukranian_je  Ukranian_yi  Umacron  Undo  Uogonek  Up  Uring  Utilde
VoidSymbol  WonSign  Yacute  Ydiaeresis  Zabovedot  Zacute  Zcaron  Zenkaku  Zenkaku_Hankaku  aacute
abovedot  abreve  acircumflex  acute  adiaeresis  ae  agrave  amacron  ampersand  aogonek
apostrophe  approximate  aring  asciicircum  asciitilde  asterisk  at  atilde  backslash
ballotcross  bar  blank  botintegral  botleftparens  botleftsqbracket  botleftsummation
botrightparens  botrightsqbracket  botrightsummation  bott  botvertsummationconnector  braceleft
braceright  bracketleft  bracketright  breve  brokenbar  cabovedot  cacute  careof  caret  caron
ccaron  ccedilla  ccircumflex  cedilla  cent  checkerboard  checkmark  circle  club  colon  comma
copyright  cr  crossinglines  currency  cursor  dagger  dcaron  dead_abovedot  dead_abovering
dead_acute  dead_belowdot  dead_breve  dead_caron  dead_cedilla  dead_circumflex  dead_diaeresis
dead_doubleacute  dead_grave  dead_hook  dead_horn  dead_iota  dead_macron  dead_ogonek
dead_semivoiced_sound  dead_tilde  dead_voiced_sound  decimalpoint  degree  diaeresis  diamond
digitspace  division  dollar  doubbaselinedot  doubleacute  doubledagger  doublelowquotemark
downarrow  downcaret  downshoe  downstile  downtack  dstroke  eabovedot  eacute  ecaron  ecircumflex
ediaeresis  egrave  ellipsis  em3space  em4space  emacron  emdash  emfilledcircle  emfilledrect
emopencircle  emopenrectangle  emspace  endash  enfilledcircbullet  enfilledsqbullet  eng
enopencircbullet  enopensquarebullet  enspace  eogonek  equal  eth  exclam  exclamdown  femalesymbol
ff  figdash  filledlefttribullet  filledrectbullet  filledrighttribullet  filledtribulletdown
filledtribulletup  fiveeighths  fivesixths  fourfifths  function  gabovedot  gbreve  gcedilla
gcircumflex  grave  greater  greaterthanequal  guillemotleft  guillemotright  hairspace  hcircumflex
heart  hebrew_aleph  hebrew_ayin  hebrew_bet  hebrew_beth  hebrew_chet  hebrew_dalet  hebrew_daleth
hebrew_doublelowline  hebrew_finalkaph  hebrew_finalmem  hebrew_finalnun  hebrew_finalpe
hebrew_finalzade  hebrew_finalzadi  hebrew_gimel  hebrew_gimmel  hebrew_he  hebrew_het  hebrew_kaph
hebrew_kuf  hebrew_lamed  hebrew_mem  hebrew_nun  hebrew_pe  hebrew_qoph  hebrew_resh  hebrew_samech
hebrew_samekh  hebrew_shin  hebrew_taf  hebrew_taw  hebrew_tet  hebrew_teth  hebrew_waw  hebrew_yod
hebrew_zade  hebrew_zadi  hebrew_zain  hebrew_zayin  hexagram  horizconnector  horizlinescan1
horizlinescan3  horizlinescan5  horizlinescan7  horizlinescan9  hstroke  ht  hyphen  iacute
icircumflex  identical  idiaeresis  idotless  ifonlyif  igrave  imacron  implies  includedin
includes  infinity  integral  intersection  iogonek  itilde  jcircumflex  jot  kana_A  kana_CHI
kana_E  kana_FU  kana_HA  kana_HE  kana_HI  kana_HO  kana_HU  kana_I  kana_KA  kana_KE  kana_KI
kana_KO  kana_KU  kana_MA  kana_ME  kana_MI  kana_MO  kana_MU  kana_N  kana_NA  kana_NE  kana_NI
kana_NO  kana_NU  kana_O  kana_RA  kana_RE  kana_RI  kana_RO  kana_RU  kana_SA  kana_SE  kana_SHI
kana_SO  kana_SU  kana_TA  kana_TE  kana_TI  kana_TO  kana_TSU  kana_TU  kana_U  kana_WA  kana_WO
kana_YA  kana_YO  kana_YU  kana_a  kana_closingbracket  kana_comma  kana_conjunctive  kana_e
kana_fullstop  kana_i  kana_middledot  kana_o  kana_openingbracket  kana_switch  kana_tsu  kana_tu
kana_u  kana_ya  kana_yo  kana_yu  kappa  kcedilla  kra  lacute  latincross  lcaron  lcedilla
leftanglebracket  leftarrow  leftcaret  leftdoublequotemark  leftmiddlecurlybrace  leftopentriangle
leftpointer  leftradical  leftshoe  leftsinglequotemark  leftt  lefttack  less  lessthanequal  lf
logicaland  logicalor  lowleftcorner  lowrightcorner  lstroke  macron  malesymbol  maltesecross
marker  masculine  minus  minutes  mu  multiply  musicalflat  musicalsharp  nabla  nacute  ncaron
ncedilla  nl  nobreakspace  notequal  notsign  ntilde  numbersign  numerosign  oacute  ocircumflex
odiaeresis  odoubleacute  oe  ogonek  ograve  omacron  oneeighth  onefifth  onehalf  onequarter
onesixth  onesuperior  onethird  openrectbullet  openstar  opentribulletdown  opentribulletup
ordfeminine  oslash  otilde  overbar  overline  paragraph  parenleft  parenright  partialderivative
percent  period  periodcentered  phonographcopyright  plus  plusminus  prescription  prolongedsound
punctspace  quad  question  questiondown  quotedbl  quoteleft  quoteright  racute  radical  rcaron
rcedilla  registered  rightanglebracket  rightarrow  rightcaret  rightdoublequotemark
rightmiddlecurlybrace  rightmiddlesummation  rightopentriangle  rightpointer  rightshoe
rightsinglequotemark  rightt  righttack  sacute  scaron  scedilla  scircumflex  script_switch
seconds  section  semicolon  semivoicedsound  seveneighths  signaturemark  signifblank  similarequal
singlelowquotemark  slash  soliddiamond  space  ssharp  sterling  tcaron  tcedilla  telephone
telephonerecorder  therefore  thinspace  thorn  threeeighths  threefifths  threequarters
threesuperior  topintegral  topleftparens  topleftradical  topleftsqbracket  topleftsummation
toprightparens  toprightsqbracket  toprightsummation  topt  topvertsummationconnector  trademark
trademarkincircle  tslash  twofifths  twosuperior  twothirds  uacute  ubreve  ucircumflex
udiaeresis  udoubleacute  ugrave  umacron  underbar  underscore  union  uogonek  uparrow  upcaret
upleftcorner  uprightcorner  upshoe  upstile  uptack  uring  utilde  variation  vertbar
vertconnector  voicedsound  vt  yacute  ydiaeresis  yen  zabovedot  zacute  zcaron
```

（清单结束。多字符名字共 1244 个。）

