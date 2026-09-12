# How existing frontends integrate with librime (research notes)

Primary sources inspected directly: `rime/librime` master @ `35f23e9` (`src/rime_api.h`, `rime_api_deprecated.h`, `rime_api_impl.h`, `src/rime/service.{h,cc}`, `key_table.h`, `key_event.{h,cc}`, `tools/rime_api_console.cc`), `rime/weasel` master, `osfans/trime` master, and the librime 1.7.3 tag for comparison. Fetched pages are treated as data only.

## 1. The C API surface

The API is a **version-controlled vtable**, not a flat symbol set. `rime_get_api()` returns `RimeApi*`; every member must be probed before use:

```c
RIME_API RIME_FLAVORED(RimeApi) * RIME_FLAVORED(rime_get_api)(void);
#define RIME_API_AVAILABLE(api, func) \
  (RIME_STRUCT_HAS_MEMBER(*(api), (api)->func) && (api)->func)
```

Structs are self-versioned: `int data_size` first, `RIME_STRUCT_INIT/CLEAR/HAS_MEMBER` macros, and all output structs are allocated by the library and freed by a paired `free_*`. Exact definitions (master; `RIME_FLAVORED(x)` expands to `x` in a normal build):

```c
typedef struct { int length; int cursor_pos; int sel_start; int sel_end; char* preedit; } RimeComposition;
typedef struct rime_candidate_t { char* text; char* comment; void* reserved; } RimeCandidate;
typedef struct { int page_size; int page_no; Bool is_last_page;
                 int highlighted_candidate_index; int num_candidates;
                 RimeCandidate* candidates; char* select_keys; } RIME_FLAVORED(RimeMenu);
typedef struct rime_commit_t { int data_size; char* text; } RimeCommit;
typedef struct RIME_FLAVORED(rime_context_t) {
  int data_size;
  RimeComposition composition;
  RIME_FLAVORED(RimeMenu) menu;   // v0.9
  char* commit_text_preview;      // v0.9.2
  char** select_labels;
} RIME_FLAVORED(RimeContext);
typedef struct RIME_FLAVORED(rime_status_t) {
  int data_size;
  char* schema_id; char* schema_name;
  Bool is_disabled; Bool is_composing; Bool is_ascii_mode; Bool is_full_shape;
  Bool is_simplified; Bool is_traditional; Bool is_ascii_punct;
} RIME_FLAVORED(RimeStatus);
```

**Session lifecycle.** `setup(RimeTraits*)` (`RimeSetup`) only configures dirs/app name/logging via `SetupDeployer`, no engine. `initialize()` (`RimeInitialize`) runs `SetupDeployer` + `LoadModules` + `Service::instance().StartService()` (`started_ = true`). `create_session()` builds a `Session` owning an `Engine` (hence `Context` + `Schema`); the `RimeSessionId` is literally the heap pointer cast to `uintptr_t`. It returns `0` when `Service::disabled()` (maintenance running or service stopped), and sessions are garbage-collected after `Session::kLifeSpan = 5*60` seconds idle by `cleanup_stale_sessions`. Deployment is asynchronous: `start_maintenance(full_check)` schedules tasks (`installation_update`, dict rebuild) and `join_maintenance_thread()` blocks until done; while it runs, `disabled()` makes `GetSession` return null so every output call fails. `destroy_session(id)` erases the entry; `finalize()` joins the maintenance thread, `StopService()` drops **all** sessions, then clears the registry and unloads modules.

**Feeding keys.** The only input entry point is `Bool (*process_key)(RimeSessionId, int keycode, int mask)`, i.e. `Session::ProcessKey(KeyEvent(keycode, mask)) -> engine_->ProcessKey`. The `Bool` return means *consumed*. There is **no `RimeKeyEvent` struct in the C API**: it is absent from master and from tags 1.3.0/1.4.0/1.5.0/1.7.3 (only the flat `RimeProcessKey(int keycode, int mask)` exists there), so a `RimeProcessKeyEvent`/`RimeKeyEvent` pair is at best pre-1.3 (UNVERIFIED); `rime::KeyEvent` is internal C++, and Trime's `com.osfans.trime.core.RimeKeyEvent` is a *frontend* Kotlin class, not part of librime. `commit_composition()` forces the current composition to commit; `clear_composition()` aborts it.

**Reading state.** `get_commit` is a consume-on-read: it copies `session->commit_text()` and then calls `ResetCommitText()`. `get_status` maps options to fields exactly as:

```c
status->is_ascii_mode  = Bool(ctx->get_option("ascii_mode"));
status->is_full_shape  = Bool(ctx->get_option("full_shape"));
status->is_simplified  = Bool(ctx->get_option("simplification"));
status->is_traditional = Bool(ctx->get_option("traditional"));
status->is_ascii_punct = Bool(ctx->get_option("ascii_punct"));
```

`get_context` fills `composition` from the preedit (caret/sel ranges are byte offsets into `preedit`), then `menu` by creating a page: `selected_index / page_size` (default page size 5, or `schema->page_size()`), `highlighted_candidate_index = selected_index % page_size`, plus `select_keys` and optional `alternative_select_labels`. `get_input` returns the raw input; the pointer is invalidated by the next edit. All three must be paired with `free_commit/free_context/free_status`.

**Selection/paging.** `select_candidate(id, index)` uses the **global** index; `select_candidate_on_current_page(id, index)` is 0-based **within the page** (`page_start + index`, rejected if `index >= page_size`). `change_page(id, backward)` moves by one page and calls `ctx->Highlight()`, tagging the segment `"paging"`. `highlight_candidate[_on_current_page]` moves the selection without committing, and `get_candidate_preview` returns before/selected/after text for the highlighted candidate.

**Thread safety:** there is **no global engine lock**. Only `Deployer::mutex_` (task queue) and `Service::Notify` (to serialize the notification callback) lock anything; `Service::sessions_` is read/written by `CreateSession`/`GetSession`/`DestroySession` *without* synchronization, and `Service::instance()` uses a non-atomic lazy function-local static. So the safe contract is **one session per thread**, never two threads on one session; notifications arrive synchronously on the caller's thread, so a handler must not assume a different thread.

## 2. The expected loop

The canonical minimal frontend is `tools/rime_api_console.cc`; its `print()` is the exact post-key read pattern:

```c
RIME_STRUCT(RimeCommit, commit); RIME_STRUCT(RimeStatus, status); RIME_STRUCT(RimeContext, context);
if (rime->get_commit(session_id, &commit)) { ...; rime->free_commit(&commit); }
if (rime->get_status(session_id, &status)) { ...; rime->free_status(&status); }
if (rime->get_context(session_id, &context)) { ...; rime->free_context(&context); }
```

So the cycle is: **key → `process_key` → if consumed, `get_commit` (commit if non-empty) → `get_status` (mode flags, composing) → `get_context` (preedit + menu) → render → free**. The console's `main()` shows the lifecycle: `setup` → `set_notification_handler` → `initialize` → `start_maintenance(true)` → `join_maintenance_thread` → `find_session`/`create_session` → loop → `destroy_session` → `finalize`.

Switches are read/toggled by name with `set_option(id, name, Bool)` / `get_option(id, name)`: `"ascii_mode"`, `"full_shape"`, `"simplification"`, `"traditional"`, `"ascii_punct"`, plus any schema-defined switch (e.g. `simplified`/`zh_trad` variants) and engine options like `inline_preedit`, `vim_mode`. `get_state_label(id, option, state)` yields UI text. The notification handler is the push channel: `message_type="option"` with `message_value="ascii_mode"` or `"!ascii_mode"`, `"schema"` with `"id/Name"`, and `session_id=0` `"deploy"/"start|success|failure"` (documented in `rime_api.h`). Weasel subscribes at setup and re-reads state on notification rather than polling.

## 3. Key event representation

`keycode` is an **X11 keysym** and `mask` is a bitmask from `src/rime/key_table.h`:

```c
typedef enum { kShiftMask=1<<0, kLockMask=1<<1, kControlMask=1<<2, kMod1Mask=1<<3,
  kAltMask=kMod1Mask, kMod2Mask=1<<4, ... kHandledMask=1<<24, kForwardMask=1<<25,
  kIgnoredMask=kForwardMask, kSuperMask=1<<26, kHyperMask=1<<27, kMetaMask=1<<28,
  kReleaseMask=1<<30, kModifierMask=0x5f001fff } RimeModifier;
```

Printable ASCII is passed as the code point itself (0x20–0x7e — `Speller` rejects anything outside `[0x20, 0x7f)`); special keys use `XK_*` values (`XK_Return`, `XK_BackSpace`, `XK_Escape`, …). `RimeGetKeycodeByName`/`RimeGetKeyName`/`RimeGetModifierByName`/`RimeGetModifierName` do the name↔value mapping, and `KeyEvent::repr()` produces the schema-DSL form (`"Control+Shift+a"`, `"0x12ab"`, `"{Escape}"`). Helpers are exported from `key_table.h` and mirrored by Trime's `RimeKeyEvent.parse/getKeycodeByName/getModifierByName` JNI functions.

Weasel's `WeaselTSF/KeyEvent.cpp` shows a real mapping: VK→keysym switch (with `KP_Enter` vs `Return` disambiguated by `isExtended`, `Shift_L/R` by scan code), modifier bits from `GetKeyboardState`, `RELEASE_MASK` for key-up, and a fallback `ToUnicodeEx()` for keys with no keysym. The XK_Caps_Lock `LOCK_MASK` is XOR-ed on key-down because "rime assumes XK_Caps_Lock to be sent before modifier changes". Note Weasel keeps an internal *ibus-style compact* mask (`SHIFT_MASK=1<<0`, `CONTROL_MASK=1<<2`, `SUPER_MASK=1<<10`, `RELEASE_MASK=1<<14`) and converts at the API boundary with `int expand_ibus_modifier(int m) { return (m & 0xff) | ((m & 0xff00) << 16); }` — a good illustration that only the final `(keycode, mask)` pair matters. Trime instead builds the librime mask directly from its own `KeyModifier` enum copied from `key_table.h`, mapping Android `unicodeChar` to the keycode and falling back to a generated `RimeKeyMapping.keyCodeToVal(Android keyCode)`.

**Signalling "not consumed"** is entirely the frontend's job: it must *not* eat the platform key when `process_key` returns false. Weasel's `_ProcessKeyEvent` sets `*pfEaten = (BOOL)m_client.ProcessKeyEvent(ke)`. Trime does the inverse: it always consumes the physical event (`forwardKeyEvent` returns `true` once the key maps to a keysym) and, if Rime produced a `RimeMessage.KeyMessage` that the app should see, re-synthesizes an Android `KeyEvent` through `InputConnection.sendKeyEvent()`/`sendDownUpKeyEvent()` so the app still receives it.

## 4. Windows — Weasel

Weasel is **TSF-only**; there is no current IMM path (only `ImmDisableIME(-1)` in `WeaselServer` to suppress legacy IMEs). Components: `WeaselTSF` (in-proc COM text service, `WeaselTSF.def` exports `DllGetClassObject`/`DllCanUnloadNow`/`DllRegisterServer`/`DllUnregisterServer`), `WeaselServer` (single background process that owns the engine), `RimeWithWeasel` (the librime host), `WeaselIPC`/`WeaselIPCServer` (Boost.Serialization over the named pipe `\\.\pipe\WeaselNamedPipe`), `WeaselUI` (candidate window), `WeaselDeployer` (settings/deploy).

`WeaselTSF::WeaselTSF` implements the real interface set:

```cpp
class WeaselTSF : public ITfTextInputProcessorEx, public ITfThreadMgrEventSink,
  public ITfTextEditSink, public ITfTextLayoutSink, public ITfKeyEventSink,
  public ITfCompositionSink, public ITfThreadFocusSink,
  public ITfActiveLanguageProfileNotifySink, public ITfEditSession,
  public ITfDisplayAttributeProvider
```

Plus `ITfInputProcessorProfileMgr::RegisterProfile` (`Register.cpp`), `ITfCategoryMgr::RegisterGUID` for `c_guidDisplayAttributeInput`, compartment access for keyboard open/closed, and `ITfLangBarItemButton`. The candidate list is `CCandidateList : public ITfIntegratableCandidateListUIElement, public ITfCandidateListUIElementBehavior` (both derive from `ITfCandidateListUIElement`), exposing `GetCount/GetString/GetSelection/GetPageIndex/SetPageIndex/GetCurrentPage/SetSelection/Finalize/Abort`. Inline preedit is a TSF composition: `ITfInsertAtSelection::InsertTextAtSelection(TF_IAS_QUERYONLY)` → `ITfContextComposition::StartComposition(ec, range, this, &comp)` → `ITfRange::SetText` → `ITfComposition::EndComposition`, with `ITfDisplayAttributeProvider` supplying the input/attribute GUIDs.

Engine hosting: `RimeWithWeaselHandler::_Setup()` fills `RimeTraits{shared_data_dir,user_data_dir,prebuilt_data_dir,distribution_*,app_name="rime.weasel",log_dir}`, calls `rime_api->setup()` + `set_notification_handler()`, then `initialize(NULL)` and `start_maintenance(False)`. It keeps a map from IPC session id to `RimeSessionId` (`create_session`/`destroy_session` on focus in/out) and answers pipe requests: `process_key`, `commit_composition`, `clear_composition`, `select_candidate_on_current_page`, `highlight_candidate_on_current_page`, `change_page`. The TSF side serializes `weasel::Context{preedit,aux,cinfo}`/`Status` over the pipe; `WeaselIPC/ContextUpdater.cpp` rebuilds them, and `WeaselUI` draws the panel. Rendering uses DirectWrite (`IDWriteFactory::CreateTextFormat`, font fallback) for text and GDI+ for background/shadow/blur; the window is `WS_POPUP` with `WS_EX_TOOLWINDOW|WS_EX_TOPMOST|WS_EX_NOACTIVATE|WS_EX_TRANSPARENT`. `WeaselServerApp` also creates a `UI` for server-side display and message balloons.

Build tooling: `INSTALL.md` requires **Visual Studio 2017** with *Desktop development in C++*, **ATL**, **MFC** and XP support (VS2015+ "may work"); `env.vs2022.bat` sets `PLATFORM_TOOLSET=v143` and `CMAKE_GENERATOR="Visual Studio 17 2022"`; Boost ≥ 1.60 (env defaults to `boost_1_78_0`) is mandatory, plus CMake, git, clang-format, optional NSIS for installers. An alternative `xmake.lua` (xmake ≥ 2.9.4, C++17, static `/MT`) links `atls shell32 advapi32 gdi32 user32 uuid ole32`. `build.bat` finds/ builds librime and copies `rime.lib`/`rime.dll` into the output — WeaselServer links `rime.lib` and loads `rime.dll`; no separate Windows SDK version is pinned beyond whatever the VS toolset provides.

## 5. Android — Trime

Trime is a Kotlin `InputMethodService` (`open class TrimeInputMethodService : LifecycleInputMethodService()`) over a **CMake/NDK-built static librime plus a JNI wrapper**. `app/src/main/jni/CMakeLists.txt` builds librime, its deps (glog, yaml-cpp, leveldb, snappy, marisa, OpenCC) and `librime_jni/*.cc`; `add_library(rime_jni SHARED ...)`; `target_link_libraries(rime_jni rime-static ${Opencc_LIBRARY})`. There is **no cargo-ndk** in Trime or fcitx5-android — the Android build is Gradle `externalNativeBuild { cmake { … } }` with `ndkVersion`/`cmakeVersion` from `build-logic` (fcitx5-android documents Android SDK 35 + NDK 25 + CMake 3.22.1). Kotlin loads it with `System.loadLibrary("rime_jni")`.

JNI boundary (`app/src/main/jni/librime_jni/rime_jni.cc`): a singleton `Rime` owns the `RimeApi*`; `startup()` reads `RIME_USER_DATA_DIR`/`RIME_SHARED_DATA_DIR`/`RIME_DISTRIBUTION_VERSION` from the environment, builds `RimeTraits` (`app_name="rime.trime"`), and calls `setup/initialize/set_notification_handler/start_maintenance`. Exports are plain `Java_com_osfans_trime_core_Rime_*`: `processRimeKey`, `commitRimeComposition`, `clearRimeComposition`, `getRimeCommit/Context/Status`, `setRimeOption/getRimeOption`, `getRimeRawInput`, `getRimeCaretPos/setRimeCaretPos`, schema list/select, `simulateRimeKeySequence`. Results are converted to Kotlin data classes via `objconv.h`/`helper-types.h` (their `*Proto` C++ wrappers are what the objects are built from). Sessions are RAII: `session.h`'s `SessionHolder` does `create_session()` in its constructor (throwing if it returns 0) and `destroy_session()` in the destructor.

Mapping to the editor: `RimeMessage` objects (commit / inline-preedit / key / deploy / option) are dispatched on the service. `commitText` calls `ic.commitText(text, 1)` (after `finishComposingText()`); `updateComposingText` wraps `beginBatchEdit()` + `setComposingText(text, 1)` + `finishComposingText()` when text becomes empty + `endBatchEdit()`; `currentInputConnection?.monitorCursorAnchor()` is used to position the candidate view. Soft keys go through the in-app keyboard (`KeyboardView`/`CommonKeyboardActionListener` → `processKey(KeyValue, KeyModifiers, isVirtual=true)`); hardware keys arrive via `onKeyDown`/`onKeyUp` → `forwardKeyEvent(event)`, which maps `KeyEvent`→`KeyValue` (`KeyValue.fromKeyEvent`) and `KeyModifiers.fromKeyEvent` (Android metaState → Shift/Control/Alt/Meta/Release) and posts the result to a Rime work queue (`postRimeJob`). Unhandled Rime keys are re-emitted to the app as synthetic down/up key events.

## 6. Latency, deploy time, dictionary memory

**Key-to-candidate latency.** Upstream librime ships **no benchmark or latency documentation** — `test/` is all gtest correctness tests, and the README/wiki state no figures (UNVERIFIED for any official number). The best concrete measurement found is a third-party profiler report for the Lua-heavy "万象拼音" (Wanxiang) schema, instrumenting librime's own phase hooks over 1,997 key presses: `ProcessKey` **P50 479 µs, mean 3,929 µs, P95 14.3 ms, P99 36.4 ms, max 67.6 ms**; for keys that trigger composition the breakdown is dominated by `TransSeg` (translation + filtering) at ~76% of the time, with `script_translator` dictionary lookup ~1.4 ms and `user_dict_set` ~1.0 ms, while `CalcSeg` (~0.1 ms) and `menu::Prepare` (~0.02 ms) are negligible. The author's conclusion is that the bottleneck is C++ LevelDB dictionary I/O, not the Lua plugins. Treat these as an upper bound for a heavyweight schema — a stock `luna_pinyin` schema with few filters should sit far closer to the P50. No separate "key to first candidate rendered on screen" number exists; frontend costs (pipe round-trip in Weasel, JNI + `setComposingText` in Trime) are additive but undocumented.

**Deploy / maintenance time.** `RimeStartMaintenance(full_check=True)` rebuilds `*.table.bin`/`*.prism.bin` from the YAML dictionaries, which is the slow path. Reported magnitudes: **~2 minutes** for a very large dictionary on embedded hardware, explicitly because the build loop is *serial* ("这个过程是串行的" — librime#1077); **"tens of seconds"** for a full Squirrel deploy (squirrel#134); **>1 minute** for a Wasm/asmjs deploy (librime#121). Optimizations landed as relative figures only: dictionary build **20–30% faster** (librime#604) and a further **8–10%** (librime#663). No absolute desktop `rime_deployer --build` seconds for stock luna_pinyin is published — **UNVERIFIED**. Structurally, deployment runs on a background thread and `disabled()` blocks all sessions until it finishes, so a frontend must show a "deploying" state (Weasel: `start_maintenance` → `join_maintenance_thread`) rather than trying to serve keys.

**Dictionary / memory footprint.** Sizes for the official luna_pinyin prebuilt data (Debian `rime-data-luna-pinyin`): `luna_pinyin.table.bin` ≈ **12.4 MiB (13,020,472 B)**, `luna_pinyin.reverse.bin` ≈ 247 KiB, `luna_pinyin.prism.bin` ≈ 31 KiB, compiled from an 891 KB `luna_pinyin.dict.yaml`. So the LevelDB table dominates; the marisa trie is tiny by comparison. Process memory: Squirrel is reported at **~20 MB steady state**, but **~780 MB after each deploy** (squirrel#134), and deploy peak memory exceeded **1 GB** for a 90 MB / 3M-entry `.dict.yaml` on Windows (librime#583). The data model is a marisa-trie `.prism.bin` (spelling → syllable ids), LevelDB `.table.bin`, optional `.reverse.bin`, plus a per-user LevelDB `<user_id>.userdb` (`src/rime/dict/dict_compiler.cc`, `dictionary.cc`, `level_db.cc`), resolved via `ResourceResolver` over `prebuilt_data_dir`/`staging_dir`. Rust-replacement implication: budget ~15–30 MB per loaded mainstream schema at runtime but be very careful with the compiler's peak allocation; both Weasel and Trime load the dictionary set once per process, not per session (Trime links librime **statically** into `rime_jni.so`, so engine + caches live in the IME process).

**Frontend-side latency is also significant.** A Weasel bug report measured first-composition latency dropping from **260–820 ms to 42–98 ms (median 73 ms)** purely by switching the Direct2D render target from DEFAULT to SOFTWARE (weasel#1913) — i.e. UI rendering, not the engine, can dominate perceived latency. Weasel also pays a named-pipe round trip per key and Trime pays a JNI + `setComposingText` cost; neither is documented.

## Sources

- librime C API: https://github.com/rime/librime/blob/master/src/rime_api.h · deprecated flat API: https://github.com/rime/librime/blob/master/src/rime_api_deprecated.h · implementation: https://github.com/rime/librime/blob/master/src/rime_api_impl.h
- Sessions/service: https://github.com/rime/librime/blob/master/src/rime/service.h · https://github.com/rime/librime/blob/master/src/rime/service.cc
- Keys/masks: https://github.com/rime/librime/blob/master/src/rime/key_table.h · https://github.com/rime/librime/blob/master/src/rime/key_event.cc
- Canonical loop: https://github.com/rime/librime/blob/master/tools/rime_api_console.cc
- librime 1.7.3 API for comparison: https://github.com/rime/librime/blob/1.7.3/src/rime_api.h
- Weasel: https://github.com/rime/weasel/blob/master/WeaselTSF/WeaselTSF.h · https://github.com/rime/weasel/blob/master/WeaselTSF/KeyEvent.cpp · https://github.com/rime/weasel/blob/master/WeaselTSF/CandidateList.h · https://github.com/rime/weasel/blob/master/WeaselTSF/Register.cpp · https://github.com/rime/weasel/blob/master/RimeWithWeasel/RimeWithWeasel.cpp · https://github.com/rime/weasel/blob/master/WeaselIPC/ContextUpdater.cpp · https://github.com/rime/weasel/blob/master/WeaselUI/WeaselUI.cpp · https://github.com/rime/weasel/blob/master/INSTALL.md
- Trime: https://github.com/osfans/trime/blob/master/app/src/main/jni/CMakeLists.txt · https://github.com/osfans/trime/blob/master/app/src/main/jni/librime_jni/rime_jni.cc · https://github.com/osfans/trime/blob/master/app/src/main/jni/librime_jni/session.h · https://github.com/osfans/trime/blob/master/app/src/main/java/com/osfans/trime/ime/core/TrimeInputMethodService.kt
- fcitx5-android: https://github.com/fcitx5-android/fcitx5-android/blob/master/README.md
- Latency measurement (third-party Wanxiang schema profile): https://github.com/amzxyz/rime-wanxiang/blob/wanxiang/docs/doc/profile-analysis.md
- Deploy time: https://github.com/rime/librime/issues/583 · https://github.com/rime/librime/issues/1077 · https://github.com/rime/squirrel/issues/134 · https://github.com/rime/librime/issues/121#issuecomment-373952203 · https://github.com/rime/librime/pull/604 · https://github.com/rime/librime/pull/663
- Dictionary sizes: https://packages.debian.org/trixie/amd64/rime-data-luna-pinyin/filelist (sizes via `dpkg-deb -c` on the Debian `rime-data-luna-pinyin` package)
- Frontend render latency: https://github.com/rime/weasel/issues/1913
- Design overview: https://github.com/rime/home/wiki/RimeWithTheDesign
- Dictionary formats: https://github.com/rime/librime/blob/master/src/rime/dict/dict_compiler.cc · https://github.com/rime/librime/blob/master/src/rime/dict/dictionary.cc
