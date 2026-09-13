# rime-ice (雾凇拼音) — data/configuration research report

Source: `github.com/iDvel/rime-ice`, branch `main`, fetched 2026 (schema `version: "2026-03-08"`). Sizes from the GitHub Contents API; entry counts and line counts measured locally on the actual downloaded files.

## 1. Repository layout and size budget

| Path | Purpose | Size |
|---|---|---|
| `rime_ice.schema.yaml` | **The schema**: engine pipeline, all option/translator/filter config | 19,484 B / 448 lines |
| `rime_ice.dict.yaml` | Import manifest + ~90 inline entries | 1,407 B / 77 lines |
| `default.yaml` | Global preset (schema_list, menu, switcher, shared punctuator/recognizer/key_binder) | 14,842 B / 408 lines |
| `t9.schema.yaml` | 9-key, `__include: rime_ice.schema.yaml:/` | 3,969 B |
| `double_pinyin*.schema.yaml` | 8 double-pinyin layouts | ~16 KB each |
| `melt_eng.schema.yaml` / `.dict.yaml` | Standalone English schema / manifest | 9,189 B / 288 B |
| `radical_pinyin.schema.yaml` / `.dict.yaml` | Radical-decomposition lookup schema / data | 9,248 B / **2,167,717 B** |
| `cn_dicts/` (6), `en_dicts/` (10), `opencc/` (3) | CN data, EN data, emoji configs | ~46.5 MB / ~522 KB / ~178 KB |
| `lua/` | 27 `.lua` files + `lunar.db` (722,428 B) | ~937 KB |
| `others/` | `docs/`, `recipes/`, `script/` (Go generator), `no_lua_schema/`, `patch_examples/`, themes | ~407 KB |
| `symbols_v.yaml`, `symbols_caps_v.yaml` | `v`-prefixed symbol tables | 31,123 B, 29,731 B |
| `weasel.yaml`, `squirrel.yaml`, `custom_phrase.txt` | Frontend skins; user phrase table | 26 KB, 19 KB, 1,708 B |
| `recipe.yaml`, `build/`, `LICENSE`, `README.md`, `AGENTS.md`, `.github/` | plum packaging, build dir, GPL-3.0, docs | — |

**Dictionary totals.** Measured non-comment entry counts:

| Dict | Entries | Bytes |
|---|---|---|
| `cn_dicts/8105` | 8,757 | 115,728 |
| `cn_dicts/base` | 542,887 | 16,603,110 |
| `cn_dicts/ext` | 339,179 | 11,922,724 |
| `cn_dicts/tencent` | 981,032 | 17,333,341 |
| `cn_dicts/others` | 905 | 17,454 |
| **enabled CN total** | **1,872,760** | **~46.0 MB** |
| `cn_dicts/41448` (disabled) | 46,019 | 387,353 |
| `en` + `en_ext` | 23,890 | 422,059 |
| `radical_pinyin` | 132,367 | 2,167,717 |

The prism/syllable tables imply roughly 1,872,760 + 23,890 + 132,367 ≈ **2.03 M dictionary entries** loaded by default. For a Rust port: ~46 MB of UTF-8 YAML source, dominated by `tencent` (981 k entries) and `base` (543 k). `8105` is the char table (8,757 chars), `41448` a second char table that is **commented out** in `import_tables`.

## 2. `rime_ice.schema.yaml` walkthrough

Top-level keys, in order: `schema`, `switches`, `engine`, `date_translator`, `lunar`/`lunar_template`, `uuid`, `long_word_filter`, `reduce_english_filter`, `pin_cand_filter`, `translator`, `melt_eng`, `cn_en`, `custom_phrase`, `emoji`, `traditionalize`, `punctuator`, `radical_lookup`, `radical_reverse_lookup`, `recognizer`, `key_binder`, `editor`, `speller`.

**schema**: `schema_id: rime_ice`, `name: 雾凇拼音`, `version: "2026-03-08"`, `dependencies: [melt_eng, radical_pinyin]`.

**switches**: `ascii_mode`, `ascii_punct`, `traditionalization`, `emoji` (`states: [ 💀, 😄 ]`, `reset: 1` → **ON by default**), `full_shape`, `search_single_char` (`abbrev: [词, 单]`, consumed by `search.lua`).

**engine** — verbatim:
```yaml
processors:
  - lua_processor@*select_character  # 以词定字
  - ascii_composer
  - recognizer
  - key_binder
  - speller
  - punctuator
  - selector
  - navigator
  - express_editor
segmentors:
  - ascii_segmentor
  - matcher
  - abc_segmentor
  - affix_segmentor@radical_lookup
  - punct_segmentor
  - fallback_segmentor
translators:
  - punct_translator
  - script_translator
  - lua_translator@*date_translator
  - lua_translator@*lunar
  - lua_translator@*uuid
  - table_translator@custom_phrase
  - table_translator@melt_eng
  - table_translator@cn_en
  - table_translator@radical_lookup
  - lua_translator@*unicode
  - lua_translator@*number_translator
  - lua_translator@*calc_translator
  - lua_translator@*force_gc
filters:
  - lua_filter@*corrector
  - reverse_lookup_filter@radical_reverse_lookup
  - lua_filter@*autocap_filter
  - lua_filter@*v_filter
  - lua_filter@*pin_cand_filter          # order: pin > Emoji > 简繁
  - lua_filter@*long_word_filter         # order: long-word > Emoji
  - lua_filter@*reduce_english_filter
  - simplifier@emoji
  - simplifier@traditionalize
  - lua_filter@*search@radical_pinyin
  - uniquifier
```
**Filter order is semantically load-bearing** (the file says so explicitly) — a Rust port must preserve it.

**speller**:
```yaml
alphabet: zyxwvutsrqponm...CBA`
initials: zyxwvutsrqponm...CBA       # no backtick → bare ` commits
delimiter: " '"                      # space separates syllables; ' splits manually
```
`algebra` is large. Real rules and their meaning:
- `- erase/^hm$/`, `- erase/^m$/`, `/^n$/`, `/^ng$/` — free up `hm m n ng` so super-abbreviation can use them.
- `- abbrev/^([a-z]).+$/$1/` — super abbreviation: any full spelling also gets a 1-letter code.
- `- abbrev/^([zcs]h).+$/$1/` — treat `zh ch sh` as one unit (`ch'sh`→城市, not `c'h's'h`).
- `- derive/^([nl])ve$/$1ue/` and `- derive/^([jqxy])u/$1v/` — accept wrong `qv`/`nue` spellings; the reverse pair (`$1ve`, `$1u`) tolerates other dicts.
- `- derive/([zcs])h(a|e|i|u|ai|...)$/h$1$2/` — typo correction (`hzi`→`zhi`), plus ~40 transposition rules (`wia`→`wai`, `ang`→`nag`/`agn`, `ao`→`oa`, `uan`→`aun`, …).
- The entire 模糊音 (fuzzy-sound) block (`derive/^([zcs])h/$1/`, `ang$→an`, `in$→ing`, …) is **commented out** — no fuzzy pinyin by default.

**translator** (main, pinyin):
```yaml
dictionary: rime_ice
enable_word_completion: true   # words > 4 syllables
spelling_hints: 8
always_show_comments: true
initial_quality: 1.2
comment_format: [ xform/^/［/, xform/$/］/ ]
preedit_format:
  - xform/([jqxy])v/$1u/     # ju qu xu yu
  - xform/([nl])v/$1v/       # nv lv
  - xform/([nl])ue/$1ve/     # nve lve
  - xform/(?<=[A-Z])\s(?=[A-Z])//   # strip spaces between capitals
```
**Correction to the brief:** there is **no `prism:` key** in `rime_ice.schema.yaml` (grep returns nothing); librime auto-derives `rime_ice.prism.bin`. `prism: t9` appears only in `t9.schema.yaml`. `commented` alternatives for ü display are left as comments.

**reverse_lookup**: also a nuance — there is **no top-level `reverse_lookup:` key**. Reverse lookup is `radical_lookup` (a `table_translator`, `prefix: "uU"`, `dictionary: radical_pinyin`, `enable_user_dict: false`, `tips: "  〔拆字〕"`, comment erased) plus the filter:
```yaml
radical_reverse_lookup:
  tags: [ radical_lookup ]
  dictionary: rime_ice        # annotates with this schema's readings
```

**punctuator**: inherits by reference — `__include: default:/punctuator` for `digit_separators`, `full_shape`, `half_shape`, and `__include: symbols_v:/symbols` for the `v`-mode symbol tables (`symbols_v.yaml` rebinds Rime's default `/` prefix to `v`).

**recognizer**:
```yaml
import_preset: default
patterns:
  punct: "^v([0-9]|10|[A-Za-z]+)$"
  radical_lookup: "^uU[a-z]+$"
  unicode: "^U[a-f0-9]+"
  number: "^R[0-9]+[.]?[0-9]*"
  calculator: "^cC.+"
  gregorian_to_lunar: "^N[0-9]{1,8}"
```
**key_binder**: `import_preset: default` + `search: "\`"` (the auxiliary-code guide, also added to `speller/alphabet`). **editor**: explicit `space/Return/Control+Return/BackSpace/Delete/Control+BackSpace/Control+Delete/Escape` bindings.

## 3. Dictionary format

Header is YAML up to a `...` line; entries follow. `rime_ice.dict.yaml`:
```yaml
name: rime_ice
version: "2026-01-26"
import_tables:
  - cn_dicts/8105     # 字表
  - cn_dicts/base
  - cn_dicts/ext
  - cn_dicts/tencent
  - cn_dicts/others
...
```
It is **not imports-only**: after `...` it also defines ~90 real entries (`A A` … `Z Z`, `0 ling`, `5G`, `3D`, `3D打印`, `M1`). `cn_dicts/41448` is present but commented out. Entries are `word<TAB>pinyin syllables<TAB>weight`, e.g. from `base`:
```
掉色	diao se	4780
掉色	diao shai	4780
密钥	mi yao	55160
密钥	mi yue	55160
啊啊	a a	27365
版权	ban quan	13204281
```
Weights are integers where larger = stronger; `8105` uses real corpus frequencies (`这 zhei 17648803`) but weight `1` for rare chars; missing weight means default. `others.dict.yaml` is a pure 容错/多音 correction list (multiple lines per word, **no weight**).

**Tone marks: none.** Verified programmatically — 0 lines in `8105`, `base`, `ext`, `tencent`, `others` have a digit tone suffix or a diacritic (`āáǎà…`). Rime-ice pinyin is **toneless**; `nüe/lüe` are written `nve/lve` and `ju/qu/xu/yu` use `u`. Tone digits appear only in the *commented-out* source metadata at the top of `radical_pinyin.dict.yaml` (`# '倉': [['cang1']]`). Add tones at runtime via the optional `others/recipes/reverse_tone` recipe, which downloads `build/kMandarin.reverse.bin`.

`radical_pinyin.dict.yaml` uses `'` as an intra-code delimiter: `巢	shun'guo	15`, `履	shi'ren'fu	15`, alphabet `a-z;` and `prefix: uU`.

## 4. English (`melt_eng`), OpenCC, emoji

`melt_eng.dict.yaml` is imports-only: `en_dicts/en_ext` then `en_dicts/en (en_ext first so its weights win)`. `en.dict.yaml` entries are identity-mapped (`abort<TAB>abort`), derived from google-10000-english + rime-melt; `en_ext` holds abbreviations/`README.md`/`&nbsp;`-style literals. `melt_eng.schema.yaml` is a self-contained schema (`table_translator`, `enable_sentence: false`, `enable_user_dict: false`, `spelling_hints: 9`), mounted into rime_ice as `table_translator@melt_eng` with `initial_quality: 1.1` and comment erased. Its `algebra_common` derives number words (`derive/1([4-7|9])/$1teen/`), symbol names (`derive/\+/plus/`), and case variants (`derive/^.+$/\U$0/`); layout-specific `algebra_*` blocks select the right number transliterations. `cn_en` is a `stabledb` user dict over `en_dicts/cn_en.txt` (`X光	Xguang`) with `initial_quality: 0.5`.

**OpenCC:** the repo ships only `opencc/emoji.json` (352 B), `emoji.txt` (132,394 B / 4,857 lines), `others.txt` (45,660 B / 1,498 lines). `emoji.json` is a standard OpenCC config: `mmseg` segmentation over `emoji.txt`, conversion chain = group dict `[emoji.txt, others.txt]`. It is a **simplifier** (`simplifier@emoji`, `option_name: emoji`, `inherit_comment: false`). 简繁 conversion uses **Rime's built-in** `s2t.json` — no `s2t.json`/`s2tw.json`/`s2hk.json` is shipped in this repo; `traditionalize` just references the name with `tags: [abc, number, gregorian_to_lunar]` and `tips: none`.

## 5. `lua/` — enabled vs merely available

**Enabled by default in `rime_ice.schema.yaml` (15):**
| Script | Behavior |
|---|---|
| `select_character.lua` | processor: `[`/`]` commit first/last char of the selected candidate |
| `date_translator.lua` | `rq sj xq dt ts rqzh rqen` → date/time/week/ISO-8601/timestamp/Chinese/English date |
| `lunar.lua` (+`lunar.db`) | `nl` / `N19700101` → lunar date, 干支, 生肖, solar terms |
| `uuid.lua` | `uuid` → random UUID v4 |
| `unicode.lua` | `U62fc` → the character at that code point |
| `number_translator.lua` | `R1234.56` → Chinese numerals / financial capitalization |
| `calc_translator.lua` | `cC…` / `=` → arithmetic expression evaluation |
| `force_gc.lua` | calls `collectgarbage("step")` each keystroke to cap memory |
| `corrector.lua` | wrong-reading/wrong-character hints in `comment` (reads `others.dict.yaml`) |
| `autocap_filter.lua` | auto-capitalize English candidates from code casing |
| `v_filter.lua` | in `v`-mode, promote single-character candidates |
| `pin_cand_filter.lua` | pin configured candidates to top positions per code |
| `long_word_filter.lua` | promote 2 long words to position 4 (`count`/`idx`) |
| `reduce_english_filter.lua` | demote built-in/custom short English words (`mode: custom`, `idx: 2`) |
| `search.lua` | radical auxiliary-code search (`lua_filter@*search@radical_pinyin`, switch `search_single_char`) |

**Available but NOT wired into `rime_ice.schema.yaml` (disconnected / commented):** `cn_en_spacer.lua` (insert spaces into mixed CN/EN candidates), `en_spacer.lua` (space after an English commit), `is_in_user_dict.lua` (mark user-dict candidates with `*`), `t9_preedit.lua` (render T9 digits as pinyin; also absent from `t9.schema.yaml`'s filters in this revision), `debuger.lua` (dumps input/preedit into comment), and the whole `lua/cold_word_drop/` plugin (8 files: `processor/filter/metatable/logger/string/drop_words/hide_words/reduce_freq_words.lua` — cold-word demotion, no reference anywhere in the schema). `convert_ar_num_to_zh.lua` is a **library**, not a plugin: `date_translator.lua` does `require("convert_ar_num_to_zh")`. `others/no_lua_schema/rime_ice.schema.yaml` (12,876 B) is a maintained Lua-free variant — the most useful reference for a Lua-less Rust engine.

## 6. Licensing

- Repository `LICENSE` is the full **GNU GPL v3** (35,149 B); README §许可证: **"GPL-3.0 (only) License."** → the configuration, Lua, and compilation are GPL-3.0-only.
- **No per-file or per-dictionary license header exists** in any `cn_dicts/*.dict.yaml` or `en_dicts/*` — only prose "数据来源/来源" comments. Upstream licenses I verified directly:
  - `radical_pinyin.dict.yaml` ← `mirtlecn/rime-radical-pinyin`: **GPL-3.0**.
  - `base` lists **THUOCL** (`thunlp/THUOCL`): **MIT**.
  - `base`/`en` list **腾讯词向量** (Tencent AI Lab embeddings) and 华宇野风系统词库 (a BBS post) and 现代汉语常用词表 (a gist): **UNCLEAR** — no license text found in-repo; redistribution terms are not stated.
  - `41448` ← Unihan (`kMandarin`, Unicode license) + `mozillazg/pinyin-data`: **MIT**.
  - `ext` references `rime/rime-essay-simp`: **LGPL-3.0**.
  - `8105` ← Wiktionary 汉语拼音索引 (Wiktionary is **CC BY-SA 4.0 / GFDL** — share-alike) + BLCU 25亿字语料字频表: **UNCLEAR**.
  - `en`/`en_ext` ← `first20hours/google-10000-english`: its `LICENSE.md` says *"Educational and personal/research use … permitted under the LDC license … I do not recommend using this data for commercial purposes without licensing it from the Linguistic Data Consortium."* — **restrictive / UNCLEAR for commercial use**.
  - `melt_eng.schema.yaml` / melt spelling rules ← `tumuyan/rime-melt`: **Apache-2.0**.

**Practical caveat for the Rust port:** the *code/config* is cleanly GPL-3.0-only, but the *data* is a heterogeneous mix. Before shipping the dictionaries commercially, the THUOCL (MIT), pinyin-data (MIT) and rime-melt (Apache-2.0) parts are safe with attribution; `google-10000-english`, 腾讯词向量, 华宇野风, Wiktionary-derived `8105`, and the BLCU frequency list are **UNCLEAR or explicitly restricted** and should be treated as blockers unless replaced.

## Downloads kept for reuse
Live dictionary/schema copies are in `/home/brennmond/projects/qingjian/.rime-research/` (`cn_dicts_*.dict.yaml`, `en_dicts_*.dict.yaml`, `radical_pinyin.dict.yaml`, `rime_ice.schema.yaml`, `README.md`, `tree.json`).

## URLs used
- https://github.com/iDvel/rime-ice
- https://raw.githubusercontent.com/iDvel/rime-ice/main/rime_ice.schema.yaml
- https://raw.githubusercontent.com/iDvel/rime-ice/main/rime_ice.dict.yaml
- https://raw.githubusercontent.com/iDvel/rime-ice/main/default.yaml
- https://raw.githubusercontent.com/iDvel/rime-ice/main/melt_eng.schema.yaml
- https://raw.githubusercontent.com/iDvel/rime-ice/main/melt_eng.dict.yaml
- https://raw.githubusercontent.com/iDvel/rime-ice/main/t9.schema.yaml
- https://raw.githubusercontent.com/iDvel/rime-ice/main/radical_pinyin.schema.yaml
- https://raw.githubusercontent.com/iDvel/rime-ice/main/radical_pinyin.dict.yaml
- https://raw.githubusercontent.com/iDvel/rime-ice/main/double_pinyin.schema.yaml
- https://raw.githubusercontent.com/iDvel/rime-ice/main/symbols_v.yaml
- https://raw.githubusercontent.com/iDvel/rime-ice/main/opencc/emoji.json
- https://raw.githubusercontent.com/iDvel/rime-ice/main/opencc/emoji.txt
- https://raw.githubusercontent.com/iDvel/rime-ice/main/opencc/others.txt
- https://raw.githubusercontent.com/iDvel/rime-ice/main/recipe.yaml
- https://raw.githubusercontent.com/iDvel/rime-ice/main/LICENSE
- https://raw.githubusercontent.com/iDvel/rime-ice/main/README.md
- https://raw.githubusercontent.com/iDvel/rime-ice/main/others/recipes/full.recipe.yaml
- https://raw.githubusercontent.com/iDvel/rime-ice/main/others/recipes/no_lua_schema.recipe.yaml
- https://raw.githubusercontent.com/iDvel/rime-ice/main/others/recipes/reverse_tone.recipe.yaml
- https://raw.githubusercontent.com/iDvel/rime-ice/main/others/no_lua_schema/rime_ice.schema.yaml
- https://api.github.com/repos/iDvel/rime-ice/contents/ (and `/cn_dicts`, `/en_dicts`, `/lua`, `/opencc`, `/others`, `/others/recipes`)
- https://data.jsdelivr.com/v1/packages/gh/iDvel/rime-ice@main?structure=flat
- https://raw.githubusercontent.com/mirtlecn/rime-radical-pinyin/master/LICENSE
- https://raw.githubusercontent.com/thunlp/THUOCL/master/LICENSE
- https://raw.githubusercontent.com/tumuyan/rime-melt/master/LICENSE
- https://raw.githubusercontent.com/rime/rime-essay-simp/master/LICENSE
- https://raw.githubusercontent.com/first20hours/google-10000-english/master/LICENSE.md
- https://raw.githubusercontent.com/mozillazg/pinyin-data/master/LICENSE
