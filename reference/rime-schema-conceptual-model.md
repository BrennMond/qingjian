# The RIME Schema Conceptual Model

Extracted from the primary source `.rime-wiki/RimeWithSchemata.md` by 佛振 (rev. 2013-05-04), cited `[S:line]`. Supporting context: `SpellingAlgebra.md` `[SA]`, `RimeWithTheDesign.md` `[D]`, `Configuration.md` `[C]`, `Introduction.md` `[I]`, `CustomizationGuide.md` `[CG]`. Gaps are marked **NOT FOUND**.

## 0. The author's framing

RIME makes the same claim Qingjian does:

> 「Rime 不是一種輸入法。是從各種常見鍵盤輸入法中提煉出來的抽象的輸入算法框架。因爲 Rime 涵蓋了大多數輸入法的「共性」，所以在不同的設定下，Rime 可化身爲不同的輸入法用來打字。」 `[S:64]`
> ("Rime is not an input method. It is an abstract input-algorithm framework distilled from various common keyboard input methods. Because Rime covers the *commonality* of most input methods, under different settings Rime can become different input methods for typing.")

> 「要讓 Rime 實現某種具體輸入法的功能，就需要一些數據來描述這種輸入法以何種形式工作。即，定義該輸入法的「個性」。」 `[S:68]`
> ("To make Rime implement a specific input method's function, one needs data describing in what form it works — i.e. defining that input method's *personality*.")

The schema is a recipe: 「以本文介紹的規格寫成一套套的配方，就是 Rime 輸入方案。」`[S:70]` The design doc states the split: 「輸入引擎是跨輸入法的通用程序，輸入方案／schema 即是那差異的部份。」`[I:58]` ("The engine is the cross-IME generic program; the schema is precisely the part that differs.") And the kernel-minimality invariant: 「爲了避免知道得太多，這引擎的內部構造必須精巧，他存在的意義在於接合內部的各種組件、並對外提供可靠的接口：Engine所表達的邏輯僅限於此。」`[D:41]` The mechanism that makes this possible is the algebra: 「利用拼寫運算／spelling algebra 機制在輸入碼與字典編碼之間建立一組映射，以此將個別方案中的特殊檢索方式統一到通用的算法。」`[I:66]`

## 1. Anatomy of a schema

The doc defines the following sections, but **explicitly refuses to be exhaustive**: the settings reference is external (「雪齋的文檔…全面而詳細解釋了輸入方案及詞典中各設定項的含義及用法」`[S:462-464]`), and a companion guide 「覆蓋了不少本文未討論的細節」`[S:583]`.

| Section | Conceptual meaning | Kind |
|---|---|---|
| `schema` | identity + metadata. 「`schema/schema_id`、`schema/version` 字段用於在程序中識別輸入方案，而 `schema/name`、`schema/author`、`schema/description` 則主要是展示給用戶的信息。」`[S:231-232]` `schema_id` is internal and part of the filename, so no capitals/CJK/spaces `[S:199-201]`; `version` separates mutually compatible versions `[S:204]`, and an incompatible upgrade **requires a new id** 「若對方案的升級會導致原有的用戶輸入習慣無法在新的方案中繼續使用，則需要換個新的方案標識」`[S:215]` | identity, presentation |
| `switches` | named boolean options with `states` labels and `reset`. Surfaced in the schema menu; 「每選定一次、狀態隨之反轉一次」`[S:1016-1018]` | policy + presentation |
| `engine` | 「輸入引擎設定，即掛接組件的「處方」」`[S:263]` — four ordered lists: `processors`, `segmentors`, `translators`, `filters` | **grammar** (the pipeline) |
| `speller` | `alphabet`, `delimiter`, `algebra` `[S:1371-1374]`. What can be typed, syllable boundaries, the inverse spelling law | **grammar** |
| `translator` | `dictionary`, `prism`, `preedit_format` `[S:1404-1407]`. Binds a translator to a lexicon; `prism` names the compiled spelling map | binding |
| `reverse_lookup` | `dictionary`, `prefix`, `tips`, `preedit_format`, `comment_format` `[S:1441-1448]` | binding |
| `punctuator` | `half_shape`/`full_shape` symbol tables, `import_preset` `[S:859-861]` | policy/presentation |
| `key_binder` | `bindings` of `when`/`accept`/`send` `[S:913-922]` | policy |
| `recognizer` | `patterns` (regex) + `import_preset` `[S:1456-1459]` | policy |
| `menu` | `alternative_select_keys` `[S:1124-1125]` | presentation |
| `patch` | customization overlay keyed by node path, 「以源文件中的設定爲底本」`[S:570-581]` | distribution/versioning |

Top-level `style`/UI, `simplifier`, `uniquifier`, `editor`, `chord_composer` config blocks, and any `formatter` category are **NOT FOUND IN THIS DOCUMENT**.

**Grammar vs policy vs presentation.** The *grammar* is exactly two things: the ordered component list in `engine` (the pipeline) and the `speller` block (alphabet, delimiter, algebra). Everything else configures a component instance rather than the language: `translator`/`reverse_lookup` are *bindings* of grammar to lexicons; `punctuator`/`key_binder`/`recognizer`/`switches` are *policy*; `schema` metadata, `menu`, `switches` labels and the display `*_format` lists are *presentation*. Notably, the kernel rule "no input-method knowledge" is upheld by pushing all method-specific knowledge into (a) the algebra, (b) the dictionary, and (c) *which* components are mounted and in what order — the component set itself is generic vocabulary, not Pinyin/Cangjie vocabulary.

## 2. Extension-point taxonomy

Four categories, each with an explicit contract.

**Processors** — 「第一類功能組件 `processor`s，就是比較籠統地、起着「處理」按鍵消息的作用。」`[S:308]` Contract is a three-way verdict: 「按鍵消息依次送往列表中的 `processor`，由他給出對按鍵的處理意見：或曰「收」、即由 Rime 響應該按鍵；或曰「拒」、回禀操作系統 Rime 不做響應、請對按鍵做默認處理；或曰這個按鍵我不認得、請下一個 `processor` 繼續看。」`[S:310-313]` List order is priority `[S:315]`.

**Segmentors** — 「將用戶連續輸入的文字、數字、符號等不同內容按照需要，識別不同格式的輸入碼，將輸入碼分成若干段分而治之。」`[S:324]` Contract: per round each segmentor proposes the longest match from the current position; the longest wins; the offering segmentor(s) attach *type tags*; multiple tags per segment are possible; scanning resumes at the segment end `[S:325,329]`.

**Translators** — five-point contract quoted verbatim: 「一是翻譯的對象是劃分好的一個代碼段。二是某個 `translator` 組件往往只翻譯具有特定標籤的代碼段。三是翻譯的結果可能有多條，每條結果成爲一個展現給用戶的候選項。四是代碼段可由幾種 `translator` 分別翻譯、翻譯結果按一定規則合併成一列候選。五是候選項所對應的編碼未必是整個代碼段。」`[S:333-338]` The data model is tabulated as `input | tag | translations` and named 「作文」 (an unfinished *essay*); concatenating each segment's first result yields the pending commit `[S:340-350]`.

**Filters** — 「每從結果集選出一條字詞、會經過一組 `filter`s 過濾。多個 `filter` 串行工作，最終產出的結果進入候選序列。」`[S:371]` A filter may 「改裝正在處理的候選項…消除當前候選項…插入新的候選項…修改已有的候選序列」`[S:373-377]`.

**Why these categories?** The author says so directly: 「雖然看起來 `processor` 通過組合可以承擔引擎的全部任務，但爲了將邏輯繼續細分、Rime 又爲引擎設置了另外三類功能組件。這些組件都可以訪問引擎中的數據對象——輸入上下文，並將各自所做處理的階段成果存於其中。」`[S:317]` The causal trigger: 「當「輸入碼」發生變更時，下一組組件 `segmentor`s 開始一輪新的作業。」`[S:319-320]`

**Open or closed?** Closed at the *category* level, open at the *implementation* level. The design doc says 「目前設計中規劃了三類框架級組件」`[D:112]` (only three — `filters` was added later), and implementations are C++ classes registered by name: 「若有多種實現方法，就寫個「Rime類」／`rime::Class`」`[D:54]`, 「還要把每一種實現註冊爲具名的「組件」」`[D:68]`. Extra shipped implementations are listed as an afterthought `[S:289-295]`. There is **no scripting/Lua escape hatch — NOT FOUND IN THIS DOCUMENT**; the stated escape hatch is reading the source: 「再往後，就只有多讀代碼，纔能見識到各種新穎、有趣的玩法。」`[S:1307]` Dispatch internals (lazy paging, `Translation` comparison for ordering, frozen confirmed segments) live in `[D:129-132]`.

## 3. Spelling, codes, syllables, algebra

The vocabulary is formalized in `[SA:16-31]`: 輸入法 = 「以輸入信號的序列到輸出文本序列的轉換方法」; 字符集 = 「構成目標輸出文本的字符的集合」; 編碼 = 「用於檢索目標字符的字母序列」; 字母/字母表 = 「字母的集合，亦稱「編碼字符集」」; 碼表 = 「目標字符集與編碼集合之間的映射表」; 編碼空間 = 「給定的字母表以有限的碼長排列組合所得的可用編碼數目」; 輸入碼 = 「用作輸入的字符序列」.

The pivotal distinction: 拼寫 = 「與單個編碼相對應的輸入碼；拼寫可能不同於編碼」 ("the input code corresponding to a single code; the *spelling* may differ from the *code*"). 拼寫法 = 「一種輸入方案裏，有效拼寫的集合到編碼集合的映射，亦稱「正字法」」 ("the mapping from the set of valid spellings to the set of codes — i.e. the orthography"). 拼寫運算 = 「以拼寫爲運算元的一元運算…並可賦予結果附加的屬性」; 投影 = 「以拼寫法爲運算元的一元運算，獲得一個衍生的拼寫法」.

The syllabary is defined as 「輸入方案中所有編碼的集合；拼音輸入方案中，彼此不同的音節是可窮舉的，其音節表是個固定的集合；多數形碼輸入法按照一定規則在給定的編碼空間內爲新生詞組編碼，故無法給出固定的音節表」`[SA:24]`. This is the deep reason the two translator families exist.

`speller/algebra` therefore operates **on the spelling law over the syllabary, not on dictionary entries**. The projection algorithm is given as a formalism: 「Rime選音節表A上的初始拼寫法(A -> A)爲投影的運算元，逐步推導出映射到音節表A的有效拼寫集合B，即所求的拼寫法(B -> A)」, with the worked derivation `Sa={a->a|a∈A}; Sx=P<x>(Sa)={x(a)->a…}=(B->A); …` `[SA:127-138]`. Runtime effect: 「輸入過程中，這組有效拼寫決定着輸入碼的音節切分方式。」`[SA:172]` The author's own summary: 「概括來說就是將方案中的編碼通過規則映射到一組全新的拼寫形式！也就是說能讓 Rime 輸入方案在不修改碼表的情況下、適應不同的輸入習慣。」`[S:595-598]`

Operators `[SA:74-112]`: `xlit` (transliteration, positional alphabet rewrite), `xform` (regex rewrite, else unchanged), `erase` (full-match removal), `derive` (keep both spellings), `fuzz` (derive + "fuzzy" attribute; needs `translator/strict_spelling: true`), `abbrev` (derive + "abbreviated" attribute, treated specially in syllable splitting). Constraints: separators cannot be escaped `[SA:72]`; `xlit` is the only UTF-32 operator, so alphabets may exceed ASCII `[SA:114-116]`; `xform`/`xlit` double as single-string transforms for display `[SA:118]`.

## 4. What a schema author can express in YAML

- **Key handling & modes**: `ascii_composer`, `recognizer`+`matcher`, `key_binder` (conditional rebinding), `speller`, `punctuator`, `selector`, `navigator`, `express_editor`/`fluid_editor` (aka `fluency_editor`), `chord_composer` `[S:264-272,289-295]`.
- **Segmentation modes**: `ascii_segmentor`, `matcher`, `abc_segmentor`, `punct_segmentor`, `fallback_segmentor` `[S:273-278]`.
- **Translation strategies**: `echo_translator`, `punct_translator`, `script_translator` (`r10n_translator`), `table_translator`, `reverse_lookup_translator` `[S:280-283]`.
- **Post-processing**: `simplifier`, `uniquifier` `[S:285-286]`.
- **The decoding space**: arbitrary `alphabet` ordering (the demo uses a reversed alphabet `[S:1372]`), `delimiter` — where 「第一位的空白用來自動插入到音節邊界處」`[S:1373]` — and the full algebra, including abbreviating an entire 400-syllable double-pinyin table `[S:1374]`.
- **A whole literal keyboard**: `punctuator` redefines space plus 「全部 94 個可打印 ASCII 字符（碼位 0x20 至 0x7e）」, one-to-one or one-to-many, used to build a pure number keyboard IME `[S:983-991]`.
- **Presentation of the code string**: `preedit_format` and `comment_format` reuse algebra operators `[S:1407,1445-1448]`.
- **Lexicons**: `*.dict.yaml` with `name`/`version`/`sort`/`use_preset_vocabulary`, TSV entries, integer or `%` weights, omitted phrase codes with auto-phoneticization gated by a 5% polyphone threshold `[S:401-459]`, and the shipped 230k-entry 【八股文】 word/frequency list `[S:469-480]`.
- **Reuse across schemas**: one dictionary shared by a whole family, so 「不僅復用了碼表數據，也可共享用戶以任一款此系列方案錄入的自造詞（仍以碼表中的形式即全拼編碼記錄）」`[S:384]`.
- **Reverse lookup into another method**: `reverse_lookup` + `recognizer/patterns` (e.g. `` reverse_lookup: "`[a-z]*$" ``) `[S:1441-1459]`.
- **Configuration composition**: `__include`, `__patch`, `__append`/`__merge`, optional `?` paths, list `@index`/`@before`/`@after last` addressing, and auto-applied `<config>.custom:/patch?` `[C:39-355]`.

The author's claim for this surface: 「在Rime輸入方案裏寫一行代碼，頂 Rime 開發者所寫的上百上千行。」`[S:681]`

## 5. Stated design principles

- **Two irreducible generative models.** 「概括起來，這是兩種構造新編碼的方式：羅馬字式輸入方案以一組固定的基本音節碼創造新的組合而構詞，而碼表式輸入方案則以一定碼長爲限創造新的編碼映射而構詞。」`[S:364]` Each cannot emulate the other's features `[S:360-362]`.
- **Text is king.** 「文本爲王。」`[S:89]`
- **DSL complexity is justified.** 「一鍵就搞掂，必然選項少，功能單一。不好玩。」／「輸入法程序一寫兩三年，也許還不夠火候；花兩三個小時來讀入門書，已是輸入法專業速成班。」`[S:74-76]`
- **User files must survive upgrades.** Shipped data is read-only, 「謝絕軟件版本更新以外的任何修改——一旦用戶修改這裏的文件，很可能影響後續的軟件升級或在升級時丟失數據」`[S:137-138]`; hence `patch` files instead of editing `[S:557-581]`.
- **The role of the user / the ambition.** 「Rime 不要定義輸入法應當是哪個樣、而要定義輸入法可以玩出哪些花樣。」／「Rime 不可能通過預設更多的輸入方案來滿足玩家的需求；真正的玩家一定有一般人想不到的高招。」`[S:1500-1502]`
- **Shared vocabulary as maintenance compression.** 【八股文】 exists so 「在不犧牲效果及可維護性的前提下、使詞典文件壓縮到最小的行數」`[S:478]`.
- **Extensibility by component addition.** 「開發者對 Engine 的期望，一是將來可不斷通過添加內部組件的方式增益其所不能，二是有能力動態地調整組件的調度」；「標準化Engine內部的「組件／Component」，明確與關鍵組件之間的對接方式，就可以滿足可擴展、可配置這兩點期望。」`[D:39-44]`

## 6. Easy to miss

- **Tags are the binding layer** between segmentation and translation, and a segment may carry *several* tags `[S:329]`; the companion guide shows the inverse knob, `abc_segmentor/extra_tags: {}`, to stop one method's codes being claimed by another `[CG:669]`.
- **Longest-match, left-to-right, multi-round** segmentation, with a higher-priority segmentor able to abort a round `[S:325, D:125]`.
- **Translators merge into one candidate list** by rule, not by precedence `[S:337]`.
- **Echo/fallback are first-class**: `echo_translator` guarantees the raw code is always selectable `[S:280,713]`.
- **`prism` is a namespace**: 「prism 要以本輸入方案的名稱來命名，以免把朙月拼音的拼寫映射表覆蓋掉」`[S:1406]` — compiled spelling maps are per-schema artifacts that can collide.
- **`recognizer` is a generic escape hatch**: a regex claims a code as email/URL/reverse-lookup before the IME interprets it `[S:266,575-581,819]`.
- **Candidate ordering and laziness** are engine mechanisms, not schema policy: `Translation` comparison and menu paging `[D:129-130]`.
- **Confirmed segments freeze** and are not re-segmented `[D:132]`.
- Mounting one translator type several times with distinct tags/prefixes, and general per-translator tag configuration, are **NOT FOUND IN THIS DOCUMENT**; only `reverse_lookup/prefix` appears `[S:1443]`. Multi-translator *coexistence* is shown only as fixed component lists `[S:1155-1157]`.
