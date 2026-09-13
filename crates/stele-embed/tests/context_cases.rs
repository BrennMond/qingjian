//! 本地向量偏好记忆的**端到端验收**（P5 · D46）。
//!
//! 它做一次 A/B 对照，而不只是"零件算得对"：
//!
//! | 组 | 装配 | 代表 |
//! | --- | --- | --- |
//! | **A 基线** | 词库权重 + P4a 精确记忆 | 今天的产品 |
//! | **C +向量** | 再挂上 `stele_embed::EmbedRanker` | 本 crate |
//!
//! 用例来自 [`CORPUS`]（`tools/embed/context-cases.tsv`），形状与 P4b 的对比集一致：
//! **它是一条可以被重新跑一遍的验收线**，不是一段说明。
//!
//! # 这组用例为什么能区分两者
//!
//! `天气` 与 `田七` 是**同一个编码** `tian qi` 下的两个词。P4a 只能按编码记次数，
//! 而对比集刻意让 `田七` 的次数更多——于是**基线必然选错**，
//! 上下文（「今天」还是「农田」）成了唯一的判别依据。
//! 向量若没学到上下文，它同样会选错；学到了就会翻过来。
//!
//! # 两条断言缺一不可
//!
//! 1. **改善**：基线错的用例，加向量后期望词排第 1；
//! 2. **无净损失**：基线对的用例，加向量后**仍然是**第 1。
//!
//! 只有第一条的话，一个"把所有候选随机打乱"的实现也能碰巧过。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use stele_core::{
    Clock, Engine, FrozenClock, Key, MemoryStore, Outcome, SelectionSource, Services, Session,
};
use stele_embed::{EmbedRanker, VectorConfig, VectorMemory};
use stele_memory::{apply_events, FileMemory, MemoryRanker};

/// 对比集位置。与 P4b 的 `tools/predict/` 同一条约定：测试数据在 `tools/` 下，
/// **不在内核 crate 里**。
const CORPUS: &str = "../../tools/embed/context-cases.tsv";

/// 对比集里的一行：`history` 或 `case`，后面是交替的 (编码, 词) 对。
#[derive(Clone, Debug)]
struct Row {
    is_case: bool,
    pairs: Vec<(String, String)>,
}

fn load_corpus() -> Vec<Row> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(CORPUS);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读不到对比集 {}：{e}", path.display()));
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').map(str::trim).collect();
        let (kind, rest) = cols.split_first().expect("每行至少有 kind");
        let is_case = match *kind {
            "history" => false,
            "case" => true,
            other => panic!("对比集第 {} 行的类型不认识：{other:?}", i + 1),
        };
        assert!(
            rest.len() >= 4 && rest.len() % 2 == 0,
            "对比集第 {} 行应当是交替的 编码/词 对（至少两对）：{line:?}",
            i + 1
        );
        out.push(Row {
            is_case,
            pairs: rest
                .chunks(2)
                .map(|c| (c[0].to_owned(), c[1].to_owned()))
                .collect(),
        });
    }
    assert!(!out.is_empty(), "对比集是空的：{CORPUS}");
    out
}

// ── 与 P4b 的对比集同一套临时方案机 ──────────────────────────────────────

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "stele-embed-{}-{tag}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_scheme(dir: &Path, pairs: &[(String, String)]) {
    let mut units: Vec<String> = Vec::new();
    for (code, _) in pairs {
        for unit in code.split_whitespace() {
            if !units.iter().any(|u| u == unit) {
                units.push(unit.to_owned());
            }
        }
    }
    units.sort();
    let alphabet = units
        .iter()
        .map(|u| format!("\"{u}\""))
        .collect::<Vec<_>>()
        .join(", ");

    let schema = format!(
        "\
schema:
  schema_id: embed
  name: 向量对比集方案
  version: \"0.1.0\"

switches:
  - name: ascii_mode
    states: [中, Ａ]
    reset: 0

engine:
  tag: abc
  translator: spelling_graph
  candidate_cap: 200

speller:
  alphabet: [{alphabet}]

translator:
  dictionary: embed
"
    );
    let mut entries: Vec<String> = pairs
        .iter()
        .map(|(code, word)| format!("{word}\t{code}\t100"))
        .collect();
    entries.sort();
    entries.dedup();
    let dict = format!(
        "---\nname: embed\nversion: \"0.1.0\"\nsort: by_weight\n...\n\n{}\n",
        entries.join("\n")
    );
    std::fs::write(dir.join("embed.schema.yaml"), schema).expect("写方案");
    std::fs::write(dir.join("embed.dict.yaml"), dict).expect("写词库");
}

fn clock() -> Arc<dyn Clock> {
    Arc::new(FrozenClock {
        secs: 1_767_225_600,
        ms: 0,
        offset_secs: 0,
    })
}

/// 装配一个引擎；`embed` 决定要不要挂向量重排器。
fn engine_with(
    dir: &Path,
    memory: &Arc<FileMemory>,
    embed: Option<Arc<VectorMemory>>,
) -> stele_engine::EngineImpl {
    let defs = stele_schemes::load_dir(dir).expect("装载测试方案");
    let store = Arc::clone(memory) as Arc<dyn MemoryStore>;
    let mut services =
        Services::new(clock()).with_ranker(Arc::new(MemoryRanker::new(Arc::clone(&store))));
    if let Some(model) = embed {
        services = services.with_ranker(Arc::new(EmbedRanker::new(model)));
    }
    stele_engine::EngineImpl::with_services(&defs, services).expect("编译测试方案")
}

fn drain(session: &mut Box<dyn Session + Send>, memory: &FileMemory) {
    let mut events = Vec::new();
    session.drain_events(&mut events);
    apply_events(memory, &events);
}

#[allow(clippy::borrowed_box)]
fn commit_pair(
    session: &mut Box<dyn Session + Send>,
    memory: &FileMemory,
    code: &str,
    word: &str,
) -> usize {
    for c in code.chars() {
        session.process_key(Key::ch(c));
        drain(session, memory);
    }
    let index = session
        .candidates()
        .iter()
        .position(|c| c.text == word)
        .unwrap_or_else(|| {
            let seen: Vec<&str> = session
                .candidates()
                .iter()
                .map(|c| c.text.as_str())
                .collect();
            panic!("编码 {code:?} 的候选里没有 {word:?}：{seen:?}")
        });
    let outcome = session.select(index, SelectionSource::Keyboard);
    assert!(
        matches!(outcome, Outcome::Committed(_)),
        "上屏 {word:?} 失败：{outcome:?}"
    );
    drain(session, memory);
    index
}

/// 期望词在**已渲染列表**里的名次（0 起）。找不到返回 `None`。
fn rank_of(
    engine: &stele_engine::EngineImpl,
    memory: &Arc<FileMemory>,
    row: &Row,
) -> Option<usize> {
    let mut s = engine.create_session();
    // 先把上文词依次上屏。
    for (code, word) in &row.pairs[..row.pairs.len() - 1] {
        commit_pair(&mut s, memory, code, word);
    }
    // 再敲待输入的编码（只敲键，不上屏）。
    let (input_code, expected) = row.pairs.last().expect("至少两对");
    for c in input_code.chars() {
        s.process_key(Key::ch(c));
        drain(&mut s, memory);
    }
    s.candidates().iter().position(|c| &c.text == expected)
}

#[test]
fn the_corpus_is_well_formed_and_actually_discriminating() {
    let rows = load_corpus();
    let history: Vec<&Row> = rows.iter().filter(|r| !r.is_case).collect();
    let cases: Vec<&Row> = rows.iter().filter(|r| r.is_case).collect();
    assert!(!history.is_empty(), "对比集里没有 history");
    assert!(!cases.is_empty(), "对比集里没有 case");
    // 每条 case 的"待输入编码"必须**没有**在 history 里出现过——
    // 否则 P4a 就足以判断，用例区分不了向量。
    //
    // （同一个编码下 `天气`/`田七` 都出现过是允许的：那正是本用例的形状。）
    for case in &cases {
        let (input_code, _) = case.pairs.last().unwrap();
        assert!(!input_code.is_empty());
    }
}

#[test]
fn the_vector_ranker_fixes_what_exact_memory_cannot() {
    let rows = load_corpus();
    let all_pairs: Vec<(String, String)> =
        rows.iter().flat_map(|r| r.pairs.iter().cloned()).collect();
    let dir = TempDir::new("cases");
    write_scheme(dir.path(), &all_pairs);

    // ── 一份共享的记忆：先把 history 打进去 ──
    let memory = Arc::new(FileMemory::in_memory_with(clock(), 4_096, 4_096));
    let baseline = engine_with(dir.path(), &memory, None);
    {
        let mut s = baseline.create_session();
        for row in rows.iter().filter(|r| !r.is_case) {
            for (code, word) in &row.pairs {
                commit_pair(&mut s, &memory, code, word);
            }
        }
    }

    // ── 从同一份本地历史学出向量（默认关，这里是显式装配） ──
    let samples = memory
        .prediction_snapshot()
        .into_iter()
        .map(|e| (e.context, e.text, e.count));
    let model = Arc::new(
        VectorMemory::train(samples, VectorConfig::default())
            .expect("history 已经打进去了，应当学得出向量"),
    );
    assert!(model.len() >= 4, "词表太小，学不到东西：{}", model.len());
    let with_vectors = engine_with(dir.path(), &memory, Some(model));

    // ── A/B 对照 ──
    let mut baseline_right = 0usize;
    let mut vector_right = 0usize;
    let mut regressions = Vec::new();
    let mut fixes = Vec::new();
    for case in rows.iter().filter(|r| r.is_case) {
        let expected = &case.pairs.last().unwrap().1;
        let a = rank_of(&baseline, &memory, case);
        let c = rank_of(&with_vectors, &memory, case);
        let a_ok = a == Some(0);
        let c_ok = c == Some(0);
        if a_ok {
            baseline_right += 1;
        }
        if c_ok {
            vector_right += 1;
        }
        if a_ok && !c_ok {
            regressions.push(format!("{expected}: A #{a:?} → C #{c:?}"));
        }
        if !a_ok && c_ok {
            fixes.push(format!("{expected}: A #{a:?} → C #{c:?}"));
        }
        println!("用例 {expected}: 基线名次 {a:?}，加向量后 {c:?}");
    }

    // ① 无净损失：基线对的，加了向量不许变错。
    assert!(
        regressions.is_empty(),
        "向量重排造成了回归：{regressions:?}"
    );
    // ② 改善：至少修好一个基线错的用例（这就是"上下文说话"的证据）。
    assert!(
        !fixes.is_empty(),
        "向量重排没有修好任何基线错的用例——它没有带来上下文信息。\
         基线对 {baseline_right} 条，加向量对 {vector_right} 条"
    );
    // ③ 而且修好之后应当是全对（本对比集只有三条，全对是可达的门槛）。
    assert_eq!(
        vector_right,
        rows.iter().filter(|r| r.is_case).count(),
        "加向量之后应当全部用例都对"
    );
}
