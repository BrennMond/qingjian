# RIME Customization & Data Model — Report from the Author's Own Docs

**Sources** (quotes are verbatim from exactly these six files; short codes in brackets): `CustomizationGuide.md` (CG), `Configuration.md` (CFG), `UserData.md` (UD), `SharedData.md` (SD), `Recipes.md` (RCP), `DictionaryPack.md` (DICT). Anything else is marked **NOT FOUND IN THESE DOCUMENTS**.

## 1. The patch mechanism, precisely

Two syntaxes exist at two layers.

**(a) User-facing layer — `<name>.custom.yaml` with a top-level `patch:` map** (CG:92–108). The operator table is CG:96–106:

| Path form | Meaning (author's gloss) |
| --- | --- |
| `"A/B/C": value` | nested map key path, `/`-separated |
| `".../@n"` | nth list element, zero-based |
| `".../@last"` | last list element |
| `".../@before 0"` | insert before first element — "不建議在補靪中使用" (not recommended in patches) |
| `".../@after last"` | insert after last element — likewise discouraged |
| `".../@next"` | insert at end — likewise discouraged |
| `".../+": value` | merge with the list/dict ("必須爲列表/字典") |
| `".../@n"` plus `/=` | **CFG only:** replace the target value explicitly; default op is replace |

Rule (CG:108): 「`patch` 定義了一組「補靪」，以源文件中的設定爲基礎，寫入新的設定項、或以新的設定值取代現有設定項的值。」— "`patch` defines a set of patches that, taking the source file's settings as a base, write new items or replace existing items' values with new values." Only one `patch:` per file (CG:126): 「不可重複 `patch:` 這一行」— the line cannot be repeated (YAML forbids duplicate keys); new content is appended under the existing block.

**(b) Compiler-directive layer — inside any config *source*** (CFG:39: 「在 YAML 語法的基礎上，增設以下編譯指令」— "on top of YAML, the following compile directives are added"). This is where `__include`, `__append`, `__merge`, and the full `__patch` semantics live — they are **not** in the `.custom.yaml` table itself:

- `__include: <node>` / `<file>:/<path>` / `<file>.yaml:/` — CFG:43 「在當前位置包含另一 YAML 節點的內容」. Whole file = `config.yaml:/`. Maps merge recursively; list children default to whole-list replacement (CFG:281–283).
- `__patch:` — CFG:107 「修改某一相對路徑下的配置節點，而非當前節點的整體」— modifies config **at a relative path**, not the whole node. Takes a literal map, a node reference (`__patch: changes`), or a **list of node references** (CFG:188) because YAML has no duplicate keys.
- `__append:` — appends list items under the directive's node (CFG:259). `__merge:` — merges a map under the directive's node (CFG:260).
- `/+` = merge list or map; `/=` = replace the target's value; no operator = replace (CFG:119–121).
- List addressing: `@<n>` (zero-based), `@last` (CFG:220–221), `@before <n>`, `@after <n>`, and `@after last` abbreviated `@next` (CFG:233–234). Paths with spaces must be quoted (`'some_list/@before 0/...'`, CFG:238).
- Optional targets: a path ending in `?` produces **no compile error** when the node (or its external file) is absent — `__patch: default.custom:/patch?` (CFG:245–251).

**Ordering is fixed by the engine, not by the file.** CFG:164–167: 「由於 YAML map 的 key 是無序的，書寫順序並不決定編譯指令的先後。同一節點下，編譯指令的執行順序爲：`__include:` 包含指定節點 → 合併當前節點下的其他 key-value 數據 → `__patch:` 修改子節點。」— map key order is undefined, so write order is irrelevant; the order is include → merge sibling literals → patch children. Referenced nodes are never mutated (CFG:86).

**Auto-applied patch (compat plugin).** CFG:322–328: if a source's root has no `__patch:`, the compiler appends `__patch: <config>.custom:/patch?` after compiling, and a legacy `<config>.custom.yaml` with a plain `patch:` map is loaded as that patch. CFG:347: 「如果源文件的根節點使用了 `__patch:` 指令，則不論其是否加載 `<config>.custom:/patch`，都不再添加自動補靪指令。如果這種情況下仍希望支持補靪文件，須將其列爲 `__patch:` 列表中的一項。」— a root-level `__patch:` **silently disables** automatic loading of the `.custom.yaml`; you must list it yourself.

**Undo / provenance / conflict detection: NOT FOUND IN THESE DOCUMENTS.** There is no `__unpatch`, no "which file set this value" query, no semantic diff or conflict diagnostic. The nearest substitute is manual inspection (CFG:431): 「未出現錯誤信息，配置亦未達到預期效果，請對照 `<用戶文件夾>/build/` 文件夾內的編譯結果文件，檢查配置源文件與補靪。」— when output differs from intent, read the compiled files in `build/` and reason backwards. The only conflict the docs name is YAML's own (CFG:187: 「YAML 語法不允許 map 有重複的 key」). Positional patches otherwise apply in order and the last one silently wins.

## 2. The layered file model

- UD:3 「Rime 從「用戶文件夾」讀取用家自訂的配置。」— the user folder is the customization root and also holds runtime state (user dictionary, install info, option state).
- SD:5–6 「Rime 輸入法在查找一項資源的時候，會優先訪問 [[用戶文件夾|UserData]] 中的文件。用戶文件不存在時，再到共享文件夾中尋找。」— **precedence: user dir first, shared dir as fallback.**
- UD:11 「用戶文件夾的位置應使用絕對路徑。請勿使用相對路徑」— absolute paths only.
- **Hand-written** (UD): `<schema>.schema.yaml`, `<dict>.dict.yaml`, `<name>.txt`, `<config>.custom.yaml` (「應用於配置文件 … 的 *補靪*」), `opencc/*`. **Written by the IME at runtime:** `<ime>.userdb/` (user dictionary), `installation.yaml` (「輸入法程序在首次運行及升級後寫入」), `user.yaml` (selected schema + option states such as 中/西, 簡/繁). **Created by deploy:** `build/*` cache, `trash/*` for files retired by upgrades.
- Shared data may ship a precompiled `build/*` 「從而省去用戶部署時從相同源文件再次編譯的步驟」 (SD:28) — a shared cache skips recompiling identical sources at deploy.
- **Deploy transformation.** CFG:318: the plugins 「將當前輸入方案所需的全部配置內容在部署期間彙總到一份編譯結果文件裏。使輸入法程序不必在運行時打開衆多的配置文件」. CFG:393–394: outputs are **not 1:1 with sources** but 「合併重組爲編譯後的默認配置 `default` 以及各輸入方案的配置」. The runtime reads only compiled results, which no longer contain directives (CFG:390–391). Exceptions read raw at runtime: `installation.yaml`, `user.yaml` (CFG:396–397). Dictionary YAML config is not compiled and supports no directives (CFG:382, 399).

## 3. The dictionary model

- A solid dictionary is `*.table.bin` + `*.prism.bin`; the prism 「綜合了從詞典源文件中提取的音節表和輸入方案定義的拼寫規則」 (DICT:7–8) — the syllable table and the schema's spelling rules are one artifact.
- `import_tables` (legacy): DICT:10–11 「此法相當於將其他源文件中的碼表內容追加到待編譯的詞典文件中，再將合併的碼表編譯成二進制詞典文件。」— **source-level concatenation, then a single compile.** How weights/frequencies merge: **NOT FOUND IN THESE DOCUMENTS.**
- Dictionary packs (librime ≥1.6): extra `.dict.yaml` files compile to their own `.table.bin` with a syllable table matching the main one; at runtime `translator/packs` lists packs and all tables are queried together (DICT:14–15). Stated benefits (DICT:83–84): 「擴展包可以獨立於主詞典及其他擴展包單獨構建，增量添加擴展包不必重複編譯完整的主詞典」 and 「增減擴展包只須重新配置輸入方案」.
- Hard constraint (DICT:87–89): 「查詢時使用主詞典的音節表，這要求擴展包使用相同的音節表構建。目前 librime 並沒有機制保證加載的擴展包與主詞典兼容。用家須充分理解該功能的實現機制，保證數據文件的一致性。這也意味着二進制擴展包不宜脫離於主詞典而製作和分發。」— **no compatibility check; the user must guarantee consistency; binary packs should not be made or distributed apart from the main dictionary.**
- `use_preset_vocabulary` appears only as `false` in DICT examples, unexplained. `vocabulary`, `essay.txt`, `columns`, `encoder`, `rules`: **NOT FOUND IN THESE DOCUMENTS** (the last four appear nowhere in the wiki at all). The only frequency-related statement here is CG:638 「關閉用戶詞典和字頻調整」 / `translator/enable_user_dict: false`.
- Reverse lookup is a separate dictionary **and** prism (`reverse_lookup/dictionary`, `reverse_lookup/prism`, CG:745–759) — e.g. keep luna_pinyin's lexicon but use double_pinyin's spelling rules. Lexicon and spelling are independently swappable.

## 4. What is loaded from where, at what time

- Edits do nothing until redeploy: CG:64 「對設置的修改於重新佈署後生效。編譯新的輸入方案需要一段時間，此間若無法輸出中文，請稍等片刻。」Deploy triggers: Weasel menu/tray, Squirrel language menu, ibus `touch ~/.config/ibus/rime/; ibus restart` (CG:56–62).
- Deploy = compile; runtime reads compiled output (CFG:386–391). Installing recipes is not enough — RCP:37 ends with an explicit reload (`Squirrel --reload`).
- Failure handling: CG:66 「若部署完畢後…輸入方案卻仍無法正常使用，可能是輸入方案未部署成功。請查看日誌文件定位錯誤。」CFG:428–429 says inspect the `INFO` log for lines starting with `E`.
- Incremental rebuild exists only for dictionary packs (DICT:84); otherwise deploy recompiles. `installation.yaml` is written on first run and after upgrade (UD:32).

## 5. The user-facing promise

The promise is *upgrade-safe local override*, motivated by the failure of direct editing (CG:86–87): 「當 Rime 軟件升級時，也會升級各種設定檔、預設輸入方案。用戶編輯過的文檔會被覆寫爲更高版本，所做調整也便丟失了。即使在軟件升級後再手動恢復經過編輯的文件，也會因設定檔的其他部分未得到更新而失去本次升級新增和修復的功能。」— on upgrade, shipped configs are overwritten and edits are lost; hand-restoring an old file forfeits the upgrade's new and fixed functionality.

The recommended method (CG:90) is a same-stem `.custom.yaml` patch, and the benefit is stated at CG:194–196: 「在由 `default` 導入的符號表之上，覆寫對按鍵 `/` 的定義。通過這種方法，既直接繼承了大多數符號的默認定義，又做到了局部的個性化。」— "inherit most defaults directly while achieving local personalization." CFG:331 confirms CLI-era compatibility: 「如果存在與舊版本 librime 兼容的補靪文件，則從中加載補靪。」

CG:79–80 frames the overall goal: 「Rime 輸入方案，將 Rime 輸入法的設定整理成完善的、可分發的形式。但並非一定要創作新的輸入方案，才可以改變 Rime 的行爲。」— schemas are a complete, distributable packaging of settings, but users need not author one to change behaviour.

**Explicit cross-version stability guarantees for config keys: NOT FOUND IN THESE DOCUMENTS.** The only version statements are CFG:437–441 (record `version: '3.14'` as a *quoted string* so YAML does not parse it as a number; `0.10` ≠ `0.1`) and the historical note that librime ≥1.3 moved compiled caches out of the user folder into `trash/`, from which a lost YAML source can be recovered (UD:40). Reproducibility is promised at the recipe level (RCP:9): 「將所需輸入方案及自定義配置項全部以配方形式列出，則可以按照列表自動完成各項配置動作，或在全新的用戶文件夾還原輸入法的自定義配置。（爲最大限度還原使用習慣，還需要另行同步用戶詞典的數據。）」— full restore works **except** that user-dictionary data must be synced separately.

## 6. Pain points stated in the documents

1. **No list element deletion, and only positional addressing.** CG:324: 「對於列表類型，現在無有辦法指定如何添加、消除或單一修改某項，於是要在定製檔中將整個列表替換！」— for lists there is, at the time of writing, no way to add/remove/single-modify an item, so the whole list is replaced. CFG documents no remove operator either; items are addressed by index or last, never by content or key.
2. **Insertion is discouraged:** `@before`/`@after`/`@next` carry 「（不建議在補靪中使用）」 (CG:101–103) with no reason given — **NOT FOUND.**
3. **Path-hostile keys.** Keys containing `/` cannot be path components (CFG:116–117); punctuation keys `/ + =` 「因其在節點路徑中有特殊含義，無法用上面演示的路徑連寫方式」 (CG:828–829), so the whole `half_shape`/`full_shape` node must be redefined.
4. **Directive placement restriction:** `__append:`/`__merge:` 「只能用在 `__include:` 指令所在節點及其（字面值）子節點」 (CFG:262).
5. **You cannot append to a list at an `__include:` node** because 「YAML 語法不允許混合 map 與 list」 (CFG:91); `__append:`/`__patch:` are mandatory.
6. **Root `__patch:` disables the auto `.custom.yaml`** (CFG:347) — a silent foot-gun.
7. **Missing non-optional targets are compile errors:** 「注意：如果指定的配置節點 `<config>:/<component>` 不存在會導致輸入方案編譯錯誤。」 (CFG:378) — hence the `?` suffix.
8. **Dict config is second-class:** 「*韻書* 文件中的 YAML 配置部份目前也不支持配置編譯指令」(CFG:399); 「（尚未實現）導入 `*.dict.yaml` 的 YAML 配置部份」(CFG:382).
9. **One `patch:` per file** (CG:126) and no duplicate YAML keys (CFG:187), forcing the list-of-references form.
10. **Packs have no compatibility validation** and shouldn't be distributed standalone (DICT:87–89).
11. **Debugging is log-and-diff:** scan `INFO` for `E` lines (CFG:428), then diff `build/` (CFG:431). No provenance tooling.
12. **The docs admit direct editing is unsanctioned but sometimes the answer:** CG:82 calls it 「最直接、但不完全正確的做法」— "the most direct, but not entirely correct, approach."
13. **Deploy latency and silence:** compiling can leave no Chinese output for a while (CG:64).

## 7. What a replacement must preserve (minimal checklist)

Path-addressed declarative overrides applied at deploy and stored separately from shipped sources; map-path + list-position addressing with explicit merge/append/replace operators and optional `?` targets; cross-file node inclusion/reuse; user-over-shared resolution with a compiled artifact as the sole runtime input; source-level dict import plus independently built, runtime-loaded dictionary packs; per-schema scoping (`.custom.yaml` beside a global `default.custom.yaml`).

**Where RIME is demonstrably weak, per its own docs** — and a replacement can be strictly better: no patch provenance/undo, no conflict detection, no list deletion or content-based addressing, no pack/main-dict compatibility check, manual bookkeeping to keep `.custom.yaml` loading when a root `__patch:` exists, and build-output diffing as the only debugging story.

**NOT FOUND IN THESE DOCUMENTS:** undo/unpatch; value provenance; conflict detection beyond YAML duplicate keys; `columns`; `encoder`; `rules`; `vocabulary`; `essay.txt`; weight/frequency merge rules for `import_tables`; the meaning of `use_preset_vocabulary`; any explicit semantic-version compatibility contract for config keys.
