# librime internals — a reimplementation reference

Verified against `rime/librime@master` (`CMakeLists.txt` declares `rime_version 1.17.0`, C++17). Anything I could not confirm from source is marked **UNVERIFIED**.

## 1. Repo layout and vendored libraries

| Path | Contents |
|---|---|
| `src/rime/` | Core: `engine`, `context`, `composition`, `segmentation`, `menu`, `candidate`, `key_event`, the four component interfaces (`processor.h`, `segmentor.h`, `translator.h`, `filter.h`), plus `ticket.h`, `component.h`, `registry.h`, `module.h`, `service.h`, `deployer.h`, `schema.h`, `config.h`, `translation.h`, `formatter.h`, `switcher.h` |
| `src/rime/algo/` | `algebra` (`Script`, `Projection`), `calculus` (`xlit/xform/erase/derive/fuzz/abbrev/reorder`), `spelling.h`, `syllabifier`, `encoder`, `dynamics.h`, `strings` |
| `src/rime/dict/` | `table`, `prism`, `reverse_lookup_dictionary`, `string_table`, `mapped_file`, `dictionary`, `vocabulary`, `entry_collector`, `dict_settings`, `dict_compiler`, `user_dictionary`, `user_db`, `level_db`, `text_db`, `db_pool`, `corrector`, `preset_vocabulary`, `dict_module.cc` |
| `src/rime/config/` | `config_compiler` (patch), `config_data`, `config_types`, `config_component` |
| `src/rime/gear/` | All concrete components, plus `poet` (sentence DP), `grammar.h`, `translator_commons.h`; `gears_module.cc` registers them |
| `src/rime/lever/` | `deployment_tasks.h` |
| `include/` | In-tree third-party headers: `darts.h` (darts-clone), `utf8.h`/`utf8/`, `X11/keysym.h` |
| `plugins/` | **Build glue only** (`CMakeLists.txt`, `plugins_module.cc`, `plugin.cc`). Real plugins are separate repos cloned into `plugins/<name>` (or listed in `$RIME_PLUGINS`); `BUILD_MERGED_PLUGINS` links them into `librime` |
| `data/minimal/`, `data/test/` | `luna_pinyin.{schema,dict}.yaml`, `cangjie5.*`, `default.yaml`, `essay.txt` |
| `tools/` | `rime_deployer`, `rime_dict_manager`, `rime_patch`, `rime_table_decompiler`, `rime_console`, `rime_api_console` |
| `test/` | googletest suites |
| `deps/` | **Git submodules** (`.gitmodules`): `glog`, `leveldb`, `yaml-cpp`, `googletest`, `marisa-trie`, `opencc` |

There is **no `third_party/` directory** in current master. Boost is not vendored — it is a system `find_package(Boost … COMPONENTS regex)` dep (>=1.74). Uses: **yaml-cpp** config parsing; **leveldb** the `userdb` class; **marisa-trie** `StringTable` (string interning / prefix + predictive match); **opencc** the `simplifier` filter; **glog** `LOG(...)` (optional via `-DENABLE_LOGGING`); **googletest** tests; **darts-clone** the `Prism` double-array trie; **utfcpp** UTF-8.

## 2. Runtime pipeline

Components are registered by string name and instantiated from `Ticket{engine, schema, name_space, klass}`; the schema prescription is `"klass"` or `"klass@alias"`, and the alias overrides `name_space`, which is the config-key prefix the component reads.

Interfaces: `Engine::ProcessKey/ApplySchema/CommitText/Compose`, `sink()`; `Processor::ProcessKeyEvent` → `kRejected` (stop, OS handles), `kAccepted` (consumed), `kNoop` (next); `Segmentor::Proceed(Segmentation*)` (returning **false** ends the current segment); `Translator::Query(input, Segment) → an<Translation>`; `Filter::Apply(an<Translation>, CandidateList*)` + `AppliesToSegment`; `Formatter::Format(string*)`.

**Key flow.** `Session::ProcessKey` → `ConcreteEngine::ProcessKey`: run `processors_` in order, stopping on `kRejected`/`kAccepted`. If all are `kNoop`, push the key into `Context::commit_history()`, run `post_processors_` (`shape_processor`), fire `unhandled_key_notifier`, return false. Accepting processors mutate `Context` (`PushInput`, `PopInput`, `DeleteInput`, `set_caret_pos`), which emits `update_notifier` → `OnContextUpdate` → `Compose(ctx)`. Note `switcher_` is always prepended to `processors_` by `InitializeComponents()`.

`Compose`: `comp.Reset(active_input)` with `active_input = input.substr(0, caret_pos)` (or the full input when the caret is at a confirmed boundary), then `CalculateSegmentation` + `TranslateSegments`.

`CalculateSegmentation`: while `!HasFinishedSegmentation()`, call each `Segmentor::Proceed` until one returns false; `Forward()` unless there was no progress or `start_pos >= caret_pos` (only one segment past the caret); then `Trim()` and another `Forward()` if the last segment is `>= kSelected`.

`TranslateSegments`: for each `Segment` with `status < kGuess`, take `input.substr(segment.start, len)`, build a `Menu`, add every non-`exhausted()` `Translator::Query` result via `AddTranslation`, add each `Filter` whose `AppliesToSegment` is true, then set `status = kGuess`, `selected_index = 0`. `Menu` merges everything through a `MergedTranslation` and lazily builds `Page{page_size, page_no, is_last_page, CandidateList}`.

`Context::Select(i)` → `select_notifier` → `OnSelect`: `seg.Close()`; if `seg.end == input.length()` mark `kConfirmed` and either `Commit()` (option `_auto_commit`) or `composition().Forward()`; else `Forward()` then recompose or move the caret. `Context::Commit()` → `commit_notifier` → `OnCommit`: push a `CommitRecord`, run `Formatter`s, emit `sink_(text)`.

`Candidate`: `type`, `[start,end)` input range, `quality`, virtual `text()/comment()/preedit()`; concrete `SimpleCandidate`, `ShadowCandidate` (retype an existing candidate), `UniquifiedCandidate` (merge duplicates).

Default `luna_pinyin` pipeline: processors `ascii_composer, recognizer, key_binder, speller, punctuator, selector, navigator, express_editor`; segmentors `ascii_segmentor, matcher, abc_segmentor, affix_segmentor@…, punct_segmentor, fallback_segmentor`; translators `punct_translator, reverse_lookup_translator, script_translator, table_translator@cangjie, script_translator@pinyin`; filters `reverse_lookup_filter@cangjie_lookup, simplifier@zh_simp, simplifier@zh_tw, uniquifier`. The smaller sketch (recognizer/ascii_composer/speller → abc_segmentor → script_translator → uniquifier/simplifier) is a valid subset.

## 3. Dictionary subsystem

**Deploy compile** (`DictCompiler`): `Compile(schema_file)` checksums all `.dict.yaml` files (plus `essay.txt` when `use_preset_vocabulary`) with `ChecksumComputer`, then compares against the table's `dict_file_checksum` and the prism's `dict_file_checksum`/`schema_file_checksum` — that *is* the rebuild decision. `BuildTable` runs `EntryCollector::Collect`, maps syllable strings to `SyllableId` (`int32_t`), groups entries via `Vocabulary::LocateEntries(Code)`, converts weight to **`log(w > 0 ? w : DBL_EPSILON)`**, optionally `SortHomophones()`, then `Table::Build`+`Save`. `BuildReverseDb` writes `.reverse.bin` for table 0. `BuildPrism` loads `speller/algebra` into a `Projection`, applies it to a `Script` built from the syllabary, then builds the prism.

**`.dict.yaml`**: YAML header `---` … `...` parsed by `DictSettings::LoadDictHeader` (requires `name`, `version`; also `sort: by_weight|original`, `use_preset_vocabulary`, `vocabulary` — default `essay`, `import_tables`, `columns` default text/code/weight = 0/1/2, `encoder/rules`, `max_phrase_length`, `min_phrase_weight`). Body is TSV `text<TAB>code<TAB>weight`, syllables space-separated; phrases may omit the code and be auto-annotated from single-character codes.

**Binary formats** (each a `MappedFile`: memory-mapped, 32-byte `format` tag, checksums, counts, `OffsetPtr` fields):

- **`.table.bin`** (`Table`) — word dictionary. `Metadata{format, dict_file_checksum, num_syllables, num_entries, OffsetPtr<Syllabary>, OffsetPtr<Index>, reserved_1/2, OffsetPtr<char> string_table, string_table_size}`. `Syllabary = Array<StringType>`, `StringType` being a union of inline `String` or a `StringId` into the marisa `StringTable`. `Index = HeadIndex = Array<HeadIndexNode{List<Entry> entries, OffsetPtr<PhraseIndex> next_level}>`; `PhraseIndex` is a union of `TrunkIndex` (`Array<TrunkIndexNode{SyllableId key, List<Entry>, OffsetPtr<PhraseIndex> next_level}>`) and `TailIndex` (`Array<LongEntry{Code extra_code; Entry entry}>`); `Entry = {StringType text; float weight}`. `TableQuery` walks `lv1_index_…lv4_index_` with `Access/Advance/Backdate`; index codes are the first `Code::kIndexCodeMaxLength = 3` syllables. Accessors accumulate `credibility_` and `quality_len_`.
- **`.prism.bin`** (`Prism`) — spelling dictionary. `Metadata{format, dict_file_checksum, schema_file_checksum, num_syllables, num_spellings, double_array_size, OffsetPtr<char> double_array, OffsetPtr<SpellingMap> spelling_map, char alphabet[256], max_key_length}`. A `Darts::DoubleArray` maps spelling → spelling id; `SpellingMap = Array<List<SpellingDescriptor{SyllableId syllable_id; int32_t type /* bit 30 = is_correction */; Credibility credibility; String tips}>>`. This is the spelling map: one spelling expands to several syllables and vice versa. Queried via `CommonPrefixSearch`, `ExpandSearch`, `GetValue`, `QuerySpelling`.
- **`.reverse.bin`** (`ReverseDb`) — text→code for reverse lookup. `Metadata{format, dict_file_checksum, dict_settings, List<StringId> index, OffsetPtr<char> key_trie/value_trie + sizes}` — two marisa tries, plus `ReverseLookupTable = hash_map<string, set<string>>` "stems".

**Terminology**: *syllable* = one code unit (`SyllableId`); *syllabary* = `set<string>` of canonical code units; *code* = `Code = vector<SyllableId>`; *spelling* = a surface input string resolving to a syllable. Prism is the spellings→syllables relation; table is `Code`→words.

**Splitting "xian"** (`Syllabifier::BuildSyllableGraph`): a Dijkstra-like priority queue visits input positions in increasing `SpellingType` order (`kNormalSpelling < kFuzzySpelling < kAbbreviation < kCompletion < kAmbiguousSpelling < kInvalidSpelling`), discarding already-visited positions. At each vertex it skips `delimiters_` (`" '"`), then `Prism::CommonPrefixSearch` returns every spelling that is a prefix — at position 0 of `xian`: `xi`, `xia`, `xian` (plus algebra-derived spellings). (With a `canonicalizer` configured, prefix monotonicity breaks, so it steps length-by-length calling `GetValue` on the canonicalized syllable.) Each match expands through `QuerySpelling` into one or more `(syllable_id, SpellingProperties)` pairs stored as `graph->edges[start][end][syllable_id]`; `end_pos` absorbs trailing delimiters. The graph therefore holds `0→2 (xi)`, `0→3 (xia)`, `0→4 (xian)`, and from 2, `2→4 (an)` — i.e. both `[xian]` and `[xi][an]`. `CheckOverlappedSpellings` marks position 2 `kAmbiguousSpelling` because a longer spelling spans it (same mechanism flags `niju'ede`). A `Corrector` adds `is_correction` matches with `credibility = kCorrectionCredibility = log(0.01) ≈ -4.605`; completion (`prism.ExpandSearch`, limit 512) adds `kCompletion` edges with `credibility += kCompletionPenalty = log(0.05) ≈ -2.996`. `Transpose()` builds `graph->indices[start][syllable_id] = vector<const EdgeProperties*>`, consumed by `Dictionary::Lookup(syllable_graph, start_pos, blacklist, predict_word)` → `DictEntryCollector = map<size_t /*end*/, DictEntryIterator>`.

**Scoring / sentences** (`ScriptTranslator` → `ScriptTranslation` → `Poet`): `Evaluate` builds the graph, looks up `Dictionary::Lookup(..., predict_word)` and `UserDictionary::Lookup(..., depth 0, predict_word_from_depth 4)`. Sentences are composed only when there are ≥2 syllable edges **and** no reliable exact-matching phrase (system or user, not a correction); otherwise the flat lists are used. `PrepareForMakingSentence` builds `WordGraph = map<start, map<end, DictEntryList>>`, capping homophones at `translator/max_homophones`. `Poet::MakeSentence` runs `DynamicProgramming` (Viterbi over `Line{predecessor, entry, end_pos, weight}`, `weight = predecessor.weight + Grammar::Evaluate(context, entry->text, entry->weight, is_rear, grammar)`, where `context` is the preceding text or the previous two words) or, when a `grammar` plugin is registered, `BeamSearch` (state keyed by last word, `kMaxLineCandidates = 7`). `MakeSentences` (`translator/max_sentences > 1`) uses a beam of width `max_sentences * 3`, dedups by a 31-based text hash, sorts descending by weight, and cuts off by relative weight difference (`sentence_cutoff_threshold` 0.1, decayed per candidate). `CompareWeight` compares weights; `LeftAssociateCompare` prefers fewer words, then lexicographically smaller word lengths. `PrepareCandidate` orders sentences, then user phrases, then system phrases; longer code length wins, user phrase wins ties (unless it is a correction); corrections are capped at `max_corrections_ = 4`. Candidate quality = `exp(entry->weight) + translator/initial_quality + entry->quality_len / full_code_length`, where `quality_len` is the documented total length of characters on the path contributed by full (non-abbreviated) spellings.

## 4. Schema and config language

`.schema.yaml` keys: `schema{schema_id,name,version,author,description}`, `switches`, `engine{processors,segmentors,translators,filters}`, then one namespace per component (`speller`, `translator`, `punctuator`, `key_binder`, `recognizer`, …).

**`speller/algebra`** is a list of `Calculation` formulas parsed by `Calculus::Parse`, syntax `<op><delim><arg1><delim><arg2><delim>` — e.g. `xlit/abc/ABC/`, `xform/^([nl])ue$/$1ve/`, `derive/^([zcs]h).+$/$1/`. Operators (`calculus.h`): `xlit` (UTF-32 transliteration), `xform` (boost::regex replace), `erase` (full-match delete, `addition() == false`), `derive` (adds the transformed spelling, `deletion() == false`), `fuzz` (derive + `kFuzzySpelling`), `abbrev` (derive + `kAbbreviation`), `reorder` (optional `dedup`). `Projection::Apply(Script*)` folds each formula over the script in order, merging via `Script::Merge` and `SpellingProperties::Compose/Update`, with `addition()`/`deletion()` deciding whether original and/or new spelling survives. This runs **once at deploy time** in `BuildPrism`, producing `.prism.bin`. The same `Projection` runs at runtime on plain strings for `translator/preedit_format` and `comment_format` (`ScriptTranslator::FormatPreedit`, `Spell`). The wiki frames it as `P[x,y,z](A→A)` starting from the identity spelling→syllable map over syllabary `A`.

**`.custom.yaml` patch**: a file `<name>.custom.yaml` with one top-level `patch:` map; keys are slash-separated config paths, with `@n`, `@last`, `@before 0`, `@after last`, `@next` for list elements and `+` to merge a list/dict; `__patch:` references reusable fragments. **UNVERIFIED**: I did not read `config_compiler.cc`, so the exact class implementing each operator is unconfirmed.

## 5. Deploy flow, userdb, learning

`tools/rime_deployer.cc`: `--build [user_data_dir] [shared_data_dir] [staging_dir]` loads `kDeployerModules` and runs `WorkspaceUpdate`; `--compile x.schema.yaml …` runs `SchemaUpdate(schema_file)`; `--add-schema` rewrites `patch/schema_list` in `default.custom.yaml`; `--set-active-schema` writes `user.yaml['var']['previously_selected_schema']`. `Deployer` holds `shared_data_dir`, `user_data_dir`, `prebuilt_data_dir`, `staging_dir` (default `build`), `sync_dir`, `user_id`, and runs a task queue (`RunTask`/`ScheduleTask`, `Run()`, `StartWork`/`StartMaintenance`, emitting `message_sink_("deploy", "start"|"success"|"failure")`). Tasks (`lever/deployment_tasks.h`): `DetectModifications`, `InstallationUpdate`, `WorkspaceUpdate`, `SchemaUpdate`, `ConfigFileUpdate`, `PrebuildAllSchemas`, `SymlinkingPrebuiltDictionaries`, `UserDictUpgrade`, `UserDictSync`, `BackupConfigFiles`, `CleanupTrash`, `CleanOldLogFiles`. Outputs land in `staging_dir`: `<schema_id>.schema.yaml`, `<schema>.prism.bin`, `<dict>.table.bin`, `<dict>.reverse.bin`. `Service` owns sessions (`SessionId = uintptr_t`, `Session::kLifeSpan = 5min`) and one `Deployer`; `Service::instance()` is the global entry point.

**User dictionary**: `dict_module.cc` registers `"userdb"` → `UserDbComponent<LevelDb>`, `"plain_userdb"` → `UserDbComponent<TextDb>` (`*.userdb.txt`), plus `"tabledb"`/`"stabledb"` and `"userdb_recovery_task"`. A LevelDB userdb is a directory `*.userdb/`; snapshots use the `.userdb.txt` TSV form. Key format is `code + " " + "\t" + phrase` (`userdb_entry_parser`: `// key ::= code <space> <Tab> phrase`); the value is `UserDbValue::Pack()` = `"c=<commits> d=<dee> t=<tick>"` (`dee` clamped to 10000). Metadata keys: `/tick`, `/user_id`, `/db_type` (`userdb`), `/db_name`, `/rime_version`. Learning: on commit `ScriptTranslator::Memorize` calls `UserDictionary::UpdateEntry(entry, commits=1)` (plus `UpdateElements` for multi-character elements beyond `translator/max_word_length`, governed by `core_word_length`/`ConcatenatePhrases`); `UpdateTickCount` bumps the tick and `algo::formula_d` (`algo/dynamics.h`) decays/accumulates `dee`. Merge/sync uses `UserDbMerger` (tick-aware, max of commits/dee) and `UserDbImporter`. `ContextualWeighted` re-ranks when `translator/contextual_suggestions` is on.

**"Translator for user phrases"**: there is **no separate core translator class**. `ScriptTranslator`/`TableTranslator` each hold a `user_dict_` (`UserDictionaryComponent`, configured under the `user_dictionary` namespace, disablable per input via `translator/user_dict_disabling_patterns`), and user results merge with system results in `ScriptTranslation` as `kUserPhrase` vs `kSysPhrase`. Distinct user-phrase behaviour comes from plugins (e.g. `librime-predict`).

## 6. librime-lua

Repo `hchunhui/librime-lua`, placed at `plugins/lua` (or fetched by `install-plugins.sh`), built as a module or merged via `make merged-plugins`. `src/modules.cc` registers `lua_translator`, `lua_filter`, `lua_segmentor`, `lua_processor`. `src/lua_gears.h` defines `LuaTranslator : Translator`, `LuaFilter : Filter, TagMatching`, `LuaSegmentor : Segmentor`, `LuaProcessor : Processor` — each holding `an<LuaObj> env_/func_/fini_` — plus `LuaTranslation : Translation`, which calls the Lua iterator lazily from `Next()/Peek()`. `LuaComponent<T>::Create` rewrites the `Ticket` so the alias (text after `@`) becomes `name_space_`, which selects the Lua function.

Hooks are the ordinary engine slots:

```yaml
engine:
  translators: [ ..., lua_translator@date_translator ]
  filters:     [ ..., lua_filter@single_char_first_filter ]
  processors:  [ ..., lua_processor@my_processor ]
  segmentors:  [ ..., lua_segmentor@my_segmentor ]
```

Scripts live in `rime.lua` in the Rime user data dir. Translators emit candidates with `yield(Candidate(type, start, end, text, comment))`; filters receive a translation object, iterate it with `input:iter()`, and yield a reordered/filtered stream. `LuaFilter::AppliesToSegment` uses plain `TagsMatch(segment)` unless the Lua object exposes `tags_match`. `src/types.cc` (~62 KB) binds `Candidate`, `Segment`, `Context`, `Translation` etc.; `src/opencc.cc` exposes OpenCC.

## Sources

- https://github.com/rime/librime — README, `CMakeLists.txt`, `src/CMakeLists.txt`, `.gitmodules`, `plugins/CMakeLists.txt`
- https://github.com/rime/librime/tree/master/src/rime — `engine.{h,cc}`, `processor.h`, `segmentor.h`, `translator.h`, `filter.h`, `context.h`, `composition.h`, `menu.h`, `candidate.h`, `key_event.h`, `component.h`, `ticket.h`, `service.h`, `deployer.{h,cc}`
- https://github.com/rime/librime/tree/master/src/rime/gear — `gears_module.cc`, `script_translator.cc`, `poet.cc`, `translator_commons.h`
- https://github.com/rime/librime/tree/master/src/rime/dict — `table.h`, `prism.h`, `dictionary.h`, `dict_compiler.{h,cc}`, `dict_settings.cc`, `entry_collector.h`, `vocabulary.h`, `user_dictionary.h`, `user_db.{h,cc}`, `reverse_lookup_dictionary.h`, `string_table.h`, `dict_module.cc`
- https://github.com/rime/librime/tree/master/src/rime/algo — `syllabifier.{h,cc}`, `algebra.{h,cc}`, `calculus.h`, `spelling.h`
- https://github.com/rime/librime/tree/master/src/rime/lever — `deployment_tasks.h`
- https://github.com/rime/librime/blob/master/tools/rime_deployer.cc
- https://github.com/rime/librime/tree/master/data/minimal — `luna_pinyin.schema.yaml`, `default.yaml`
- https://github.com/rime/librime/tree/master/include — `darts.h`
- https://github.com/rime/home/wiki/RimeWithSchemata
- https://github.com/rime/home/wiki/SpellingAlgebra
- https://github.com/rime/home/wiki/CustomizationGuide
- https://github.com/hchunhui/librime-lua — README, `src/lua_gears.h`, `src/modules.cc`; https://github.com/hchunhui/librime-lua/wiki/Home
