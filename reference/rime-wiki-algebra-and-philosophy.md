# RIME author's own documentation: spelling algebra, ComboPinyin, design contract

Sources read in full: `.rime-wiki/SpellingAlgebra.md`, `ComboPinyin.md`, `UserGuide.md`,
`Introduction.md`, `RimeWithTheCode.md`. Quotes are verbatim (Traditional Chinese); bracketed
English is my translation. `NOT FOUND IN THESE DOCUMENTS` marks a requested item that is absent.

---

## 1. Spelling algebra — the full formalism

### 1.1 What the algebra operates on

From the terminology section of `SpellingAlgebra.md`:

- 碼表 — "目標字符集與編碼集合之間的映射表" [mapping table between target character set and code set].
- 音節表 — "輸入方案中所有編碼的集合" [the set of all codes in a schema].
- 拼寫 — "與單個編碼相對應的輸入碼；拼寫可能不同於編碼" [the input code corresponding to a single code; a spelling may differ from its code].
- 拼寫法 — "一種輸入方案裏，有效拼寫的集合到編碼集合的映射，亦稱「正字法」" [within one schema, the mapping from the set of valid spellings to the set of codes; also called the orthography].
- 拼寫運算 — "以拼寫爲運算元的一元運算，通過字符串匹配及替換操作對拼寫實施文字變換，獲得一個新的拼寫，並可賦予結果附加的屬性" [a unary operation whose operand is a spelling; it transforms the spelling by string match and replace, obtaining a new spelling, and may attach additional attributes to the result].
- 投影 — "以拼寫法爲運算元的一元運算，獲得一個衍生的拼寫法；實際運用時，通常對拼寫法連續執行一組投影操作；每一輪操作中，對拼寫法裏的每個有效拼寫做一次拼寫運算，從而獲得新的有效拼寫集合，並重新建立其與編碼集合的映射" [a unary operation whose operand is an orthography, yielding a derived orthography; in practice one runs a sequence of projections on the orthography; each round applies one spelling operation to every valid spelling, producing a new valid-spelling set and re-establishing its mapping to the code set].

The object is **not a string and not a code table**: it is a many-to-one relation `B → A` with `A`
(the syllable table) fixed and `B` (valid spellings) grown and pruned. A spelling operator is a unary
map on *spellings*; a projection **lifts** it to the relation, outputting a new orthography `B' → A`.

### 1.2 Expression syntax

> "格式爲：`<運算子><分隔符><參數1><分隔符><參數2><分隔符>...`
> 分隔符爲單個ASCII字符，通常用符號或空白字符。
> … 注意：作爲分隔符的字符不能在參數中出現；不同於Perl的 `s/\//\\/` 語法：拼寫運算式不支持在參數中將用作分隔符的字符用“\\”轉義表示。"

[Format: `<operator><delimiter><arg1><delimiter><arg2><delimiter>...`. The delimiter is a single
ASCII character, usually punctuation or whitespace. … The delimiter may not occur inside an
argument; unlike Perl's `s/\//\\/`, an algebra expression does **not** support escaping a delimiter
inside an argument.] If no argument contains a space, `xlit abc ABC` is legal. Patterns follow Perl
regex syntax.

### 1.3 Operators (author's definitions, verbatim)

1. **轉寫 / Transliteration — `xlit/<左字母表>/<右字母表>/`**
   "依次將拼寫中見於<左字母表>的字符替換爲<右字母表>對應位置的字符。左、右字母表應包含相同數目的Unicode字符。" [Replace, in order, each character of the spelling found in the left alphabet with the character at the corresponding position in the right alphabet. The alphabets must contain the same number of Unicode characters.] `xlit/abc/ABC/` on `abracadabra` → `ABrACAdABrA`.

2. **變形 / Transformation — `xform/<模式>/<替換式>/`**
   "若拼寫（或其子串）與<模式>匹配，則將所匹配的部份改寫爲<替換式>；否則拼寫保持不變。" [If the spelling or a substring matches, rewrite the matched part; otherwise unchanged.] `xform/^([nl])ue$/$1ve/` on `nue` → `nve`. Effect: "輸入nve(lve)可以獲得源碼表中與編碼nue(lue)對應的候選；輸入nue(lue)無候選" [nve retrieves candidates coded nue; nue itself no longer works].

3. **消除 / Erasion — `erase/<模式>/`**
   "若拼寫與<模式> **完 全** 匹配，則將該拼寫從有效拼寫集合中消除。" [If the spelling **fully** matches, remove it from the valid-spelling set.] `erase/^.*\d$/` on `dang1` — "帶聲調的拼音不再可用" [toned pinyin no longer usable].

4. **派生 / Derivation — `derive/<模式>/<替換式>/`**
   "若對拼寫做正則模式匹配、替換而獲得了新的拼寫，則有效拼寫集合同時包含派生前後的拼寫；否則僅保留原拼寫。" [If match/replace yields a new spelling, the valid set contains both before and after; otherwise only the original is kept.] Thus `derive/^([nl])ue$/$1ve/` on `nue` gives `nve` with both usable; the workhorse for fuzzy sounds and alternates.

5. **模糊 / Fuzzing — `fuzz/<模式>/<替換式>/`**
   "執行派生運算；派生出的拼寫將獲得「模糊」屬性，可設定將其用作構成詞組的簡碼、但不用於輸入單字。" [Performs derivation; the derived spelling receives the fuzzy attribute, configurable as usable for phrase abbreviation but not for single-character input.] Note: "需配合 script_translator 的選項 `translator/strict_spelling: true` 方可限定該拼寫不用於輸入單字。" [Requires `translator/strict_spelling: true` to actually enforce that.]

6. **縮略 / Abbreviation — `abbrev/<模式>/<替換式>/`**
   "執行派生運算；派生出的拼寫將獲得「縮略」屬性，會在音節切分時與通常的拼寫做區分處理。" [Performs derivation; the derived spelling receives the abbreviated attribute, treated differently during syllable segmentation.]

**No other operators are defined here.** `reorder` is **NOT FOUND IN THESE DOCUMENTS** (a
case-insensitive search of the entire local `.rime-wiki/` snapshot finds no occurrence).

### 1.4 Two matching modes, one encoding exception

> "「轉寫」是拼寫運算中目前唯一一則將運算元和參數作UTF-32編碼、而非UTF-8編碼處理的運算。意味着，字母表可以採用ASCII範圍以外的字符、字母表的長度按照Unicode字符數來計算。"

[xlit is the only operation treating operand and arguments as UTF-32 rather than UTF-8: alphabets
may use non-ASCII characters and length counts Unicode characters.] This lets the same machinery
drive cangjie letters and bopomofo.

> "「轉寫」和「變形」兩則運算，除在拼寫法投影操作中起重要作用，還可用於對單個字符串進行變換。「消除」、「派生」和「縮略」，用於定義拼寫法投影中非一一映射的情況。"

[xlit and xform also transform a single string; erase, derive and abbrev exist to define the
non-one-to-one cases of a projection.]

> "「消除」就給定的模式，對運算元做完全匹配，即regex match操作；「變形」、「派生」和「縮略」則可做部份匹配，相當於regex search/global replace操作。"

[erase does a full match (regex *match*); xform, derive and abbrev may match partially — regex
*search/global replace*.] Hence they rewrite **all** matches unless anchored.

### 1.5 Composition and order

Projection algorithm, verbatim:

> "記音節表爲A，拼寫運算爲序列[x,y,z]，該投影的結果記爲 P[x,y,z](A -> A)
> Sa = { a -> a | for a in A } = (A -> A)
> Sx = P&lt;x&gt;(Sa) = { x(a) -> a | for (a -> a) in (A -> A) } = (B -> A)
> Sy = P&lt;y&gt;(Sx) = { y(b) -> a | for (b -> a) in (B -> A) } = (C -> A)
> Sz = P&lt;z&gt;(Sy) = { z(c) -> a | for (c -> a) in (C -> A) } = (D -> A)
> P[x,y,z](Sa) = Sz"

[Let A be the syllable table and [x,y,z] the operator sequence; the projection is
`P[x,y,z](A -> A)`. Start from the identity orthography `Sa = {a→a | a ∈ A}`; each step lifts one
operator over the spelling side, leaving the code side `A` untouched; result `Sz`.]

Why seed with `A → A`:

> "將拼寫法投影用於構建拼寫－編碼映射時，用戶的輸入是隨意的；而碼表中，音節表是固定的集合A。所以Rime選音節表A上的初始拼寫法(A -> A)爲投影的運算元，逐步推導出映射到音節表A的有效拼寫集合B，即所求的拼寫法(B -> A)。"

[User input is arbitrary, but the table's syllable table is the fixed set A; so Rime starts from the
identity orthography over A and derives B mapping onto A.]

**Order is the semantics.** Each operator runs **exactly once**, in written order — not iterated to a
fixpoint. The author states the consequence explicitly: "模糊音定義先於簡拼定義，可令簡拼支持以上模糊音"
[the fuzzy-sound rules precede the abbreviation rules, which makes abbreviations support those fuzzy
sounds]. The algebra is a pipeline over sets, not a rewrite system with a normal form.

## 2. ComboPinyin

**Problem.** Pinyin typing is serial; order carries information. The author rejects copying QWERTY
spelling onto chords ("照搬爲串擊設計的 QWERTY 鍵位，強行改作並擊是行不通的"), because letters repeat
(`nan`, `gong`), letter layouts yield unplayable combos, and `liang` makes the hand jump. Chording
maps phonological components instead: "並擊可以直接將語音的各部分同時映射爲擊鍵的動作。這是更純粹地拼「音」，而不是拼字母。"
[Chording maps each part of the syllable directly onto simultaneous keys — a purer spelling of sound,
not of letters.]

**Mechanism.** One chord = one syllable; 20 keys (letters + space), 1–6 keys per chord, seven fingers.
Chord press order is irrelevant, but there is a canonical *writing* order `SCZHLFGDBKTP-IUÜANREO`.
The mapping is attributed to an engine component plus the algebra: "Rime 輸入法的 `chord_composer`
組件支持對並擊鍵位的自定義，通過拼寫運算技術將並擊組合鍵映射爲拼音音節。" [The `chord_composer`
component supports custom chord layouts and, through spelling algebra, maps chord combinations to
pinyin syllables.] Borrowing (通借) reuses phonotactically impossible chords — "將不構成有效音節的並擊組合借作他用，簡化並擊的指法"
[e.g. `[URO]` = ueng/ong, exploiting that `ou` cannot follow medial `u`].

**Does it need engine support?** Yes. `UserGuide.md` confirms "仍是基於【朙月拼音】" [it is still based
on luna_pinyin] — the dictionary is reused, so the schema-visible part is data. But the engine must
supply: (a) a chord front-end emitting a spelling token; (b) single-key-vs-chord disambiguation —
`[A]` alone is **space**, needing release/idle timing policy; (c) auto separator and syllable-wise
backspace: "軟件將在每個並擊所得的音節後自動加入隔音符號；輸入拼音後按退格鍵，也會以音節爲單位回退刪除拼音"; (d) hardware
precondition "6 鍵並擊無衝突（6KRO）". Required generality: **the spelling alphabet must be arbitrary
and opaque**, and the algebra must run over chord tokens exactly as over pinyin letters.

## 3. User-facing behavior contract

**First run.** "下載、安裝完成後，試試：切換到 Rime 輸入法，按 `F4` 鍵或組合鍵 `Ctrl+\`` 喚出輸入方案選單"
[after installing, switch to Rime and press F4 / Ctrl+` for the schema menu]. `朙月拼音` is the
default — "默認安裝後將使用此方案。其詞典包含 Rime 內置的【八股文】繁體詞庫". Only Weasel has a panel: "目前僅【小狼毫】配有一組簡單的設定面板".

**Candidate ordering.** Weight is data: "權重（決定重碼的次序、可選，權重高則選項在前）" [weight decides
order for duplicate codes; optional; higher first]. Use also reorders, and deleting a table word only
cancels that: "只能夠從用戶詞典中刪除詞組。用於碼表中原有的詞組時，只會取消其調頻效果。" [Only user-dictionary
phrases can be deleted; for table phrases, deletion merely cancels their frequency adjustment.]
Reverse lookup tie-break: "倉頡碼與拼音重碼時，倉頡碼查到的候選字優先".

**How user words enter.** Three routes: the user dictionary grown by selection and merged with
snapshots ("詞頻更新爲二者的較大值，其他參數亦會按照合理的算法疊加" [frequencies take the max; other
parameters accumulate by a reasonable algorithm]); `custom_phrase.txt`, a tab-separated text/code/weight
file; and generated entries the user is warned about: "該候選詞組並不存在於碼表中，而是通過「混元編碼器」產生、或由已知字碼自動組合而成的結果。您需要確認一下這是否恰是你想要的詞。"
[This candidate is not in the table; it was produced by the Hundun encoder or assembled from known
codes — check that it is what you wanted.] Those carry the ☯ mark.

**Binding invariant.** "凡是編碼爲源碼表中未出現過的形式，如通過「拼寫運算」實現的簡拼、異拼，又如編碼中的拼寫錯誤，都將導致該條記錄成爲用戶詞典中的無效數據，因爲無法通過正常的輸入檢索到。"
[Any code not in canonical form — algebra-derived abbreviations/alternates, or typos — becomes invalid
user-dictionary data, unretrievable by normal input.]

**Many-to-one mapping:** "注意詞典與輸入方案可能是一對多的關係。"

**Privacy / offline.** `NOT FOUND IN THESE DOCUMENTS`. None of the five states a privacy, offline, or
data-locality guarantee. The nearest statement is future work in `Introduction.md`: "第三期，添加網絡功能，持續優化輸入效果；建立輸入法創作平臺。"
[Phase 3: add network functionality; build a schema authoring platform.] Sync is user-driven via
removable media or Dropbox, with `*.userdb.txt` snapshots and a one-way backup of user YAML/txt.

## 4. Design-philosophy quotes (verbatim + translation)

- "爲了足夠靈活而能支持廣泛的輸入法類型，在輸入方案中，利用 *拼寫運算／spelling algebra* 機制在輸入碼與字典編碼之間建立一組映射，以此將個別方案中的特殊檢索方式統一到通用的算法。" — [To support a wide range of input methods, the schema uses spelling algebra to map input codes to dictionary codes, unifying each scheme's special retrieval into one generic algorithm.]
- "若要講，輸入引擎是跨輸入法的通用程序，*輸入方案／schema* 即是那差異的部份。" — [The engine is the cross-IME generic program; the schema is precisely the part that differs.]
- "咱假定，從不同種類的輸入法中，可歸納出幾種類型的實現機制，即通用於一類輸入法的算法和數據結構。" — [We assume a few mechanism types — algorithms and data structures common to a class of IMEs — can be induced from different input methods.]
- "其中不包括：實現編碼到文字轉換的字典數據…經過操作系統與設備和輸入目的程序交互的組件…展現輸入法信息的介面…配置工具" — [The engine excludes dictionary data, OS/device interaction components, the UI, and configuration tools.]
- "輸入方案按一定的規格撰寫，用戶可於需要時導入到軟件，這便是本項目軟件開發者與輸入方案創作者分工、協作的方式。" — [Schemas are written to a spec and imported as needed; that is how developers and schema authors divide and share the work.]
- "拼寫運算，提供了描述產生式規則的能力！" / "拼寫運算，提供爲輸入方案重構拼寫法的能力！" — [Spelling algebra provides the ability to describe production rules! / to reconstruct a schema's orthography!]
- "拼寫運算，藉助正則表達式實現其字符串處理能力。進一步，利用數學知識，構造出建立在輸入法編碼集合上的代數系統。" — [It gets string processing from regex; further, it uses mathematics to build an algebraic system on the code set.]
- "故，產生簡碼、容錯碼的規則是與輸入方案相關的，一款通用的輸入法軟件要以與編碼方案相適應的方式生成這些衍生編碼。" — [The rules producing short and error-tolerant codes are schema-relative; a general IME must generate derived codes adapted to the encoding scheme.]
- "創造應用價值是一方面，更要堅持對好技術的追求，希望能寫出靈動而易於擴展的代碼" — [Creating value is one thing; one must also pursue good technology, writing nimble, extensible code.]
- ComboPinyin: "所謂「肌肉記憶」，說得不準確。人的肢體和肌肉沒有記憶力；手指能否靈活地運動，本質是大腦會不會高效地指揮。學習新指法的過程，表面是練手，實際是在練腦。" — [So-called "muscle memory" is inaccurate; learning a fingering is ostensibly hand practice but actually brain practice.] Also "輸入法，我主張，用合適的。" [For input methods, I advocate using the one that suits you.]

## 5. What an engine designer is likely to miss

1. **The unit is the orthography, not a string.** `xlit`/`xform` double as single-string transforms (`preedit_format`), so the operator layer must be standalone. Luna's preedit rule `xform/([nl])v/$1ü/` deliberately **omits anchors** to rewrite every match in a multi-syllable string; the same operator text has two scopes.
2. **Non-injective operators carry attributes.** `fuzz`/`abbrev` differ from `derive` only by an attribute consumed downstream; spillover entries need at least a bitset.
3. **`erase` is full-match; the others are global replace.** Different regex operations, not flags.
4. **One pass per operator, in order, no fixpoint.** Termination is trivial only because each runs once; do not "apply until stable".
5. **Deployment-time compilation is part of the design.** "輸入方案部署工具：將投影所得的拼寫法製成Prism文件，供Rime輸入法於工作時快速訪問" [the deployment tool turns the projected orthography into a Prism file for fast runtime access].
6. **Projection is one-way.** Canonical codes, not spellings, are dictionary keys; persisting a non-canonical spelling hits the "無效數據" trap. Decide deliberately whether to allow it.
7. **Delimiters are a hard constraint.** One ASCII char, unescapable inside arguments (cangjie's 26-letter alphabet uses `|`). A parser assuming `/` fails real schemas.
8. **Arbitrary alphabets and input sources.** `xlit` is UTF-32; ComboPinyin feeds chord tokens through the same algebra; spellings and codes must be opaque token strings.
9. **Schema/dictionary is one-to-many**, and reverse lookup crosses schemas with explicit collision priority.
10. **Front-end policy leaks into schemas.** Chording needs single-key/chord disambiguation, auto separator insertion, and syllable-granular backspace. Budget a generic "input source emits spelling tokens" abstraction with policy hooks.

`RimeWithTheCode.md` contains only repository links and issue-filing guidance — no architecture or philosophy.
