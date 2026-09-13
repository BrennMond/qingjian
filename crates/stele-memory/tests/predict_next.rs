//! 下一词预测（P4b）的**端到端**验收。
//!
//! # 这份测试在回答什么
//!
//! PLAN §3 的 P4b 行写的是「常见搭配排序改善（**需定义对比集**）」。
//! 没有对比集，这句话无法证伪。对比集在 [`CORPUS`]：
//! `tools/predict/collocations.tsv` 的每一条「常见搭配链」都是一条用例。
//!
//! 每一行都走**完整那条链**才算数：
//!
//! ```text
//! 按键 → 切分 → 翻译 → 候选 → 上屏 → drain_events → apply_events
//!      → MemoryStore::record（按 lane 分流）→ 预测表 → 下次 predict_next
//!      → Lane::Predict 候选 → Session::candidates()
//! ```
//!
//! 任何一环没接上，`the_corpus_of_common_collocations_is_predicted` 都会红。
//! HANDOFF §7.7.4 把"忘了接 `Learned` 事件"列为本阶段最容易漏、且**漏了不报错**
//! 的坑——这一条就是那个坑的守门人。
//!
//! # 为什么用**按对比集生成**的临时方案，而不是 `schemes/stele-default`
//!
//! 与 P4a 的端到端测试同一个理由：这里要断言的是**预测的排序行为**，
//! 而真实词库的权重是数据——改一次词表就可能让断言从"第 1 位"变成"第 3 位"。
//! 按对比集生成的方案让"编码 ↔ 词"由测试自己写死，于是这条断言不依赖任何词表内容。
//!
//! 方案是**拼写图**族（`spelling_graph`）：`wei xin` 敲 `weixin`，
//! 与真实拼音方案的形状一致（而不是把编码当成一串单字母）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use stele_core::{
    Clock, Engine, FrozenClock, Key, Lane, MemoryStore, Outcome, SelectionSource, Services, Session,
};
use stele_memory::{apply_events, FileMemory, MemoryRanker};

/// 对比集的位置。与 `tools/oracle/*.expected.txt` 同样的约定：
/// **测试数据在 `tools/` 下，不在内核 crate 里**（门禁抓到过一次）。
const CORPUS: &str = "../../tools/predict/collocations.tsv";

/// 对比集里的一条「常见搭配链」：交替的 (编码, 词)。
#[derive(Clone, Debug)]
struct Chain {
    pairs: Vec<(String, String)>,
}

impl Chain {
    fn prefix(&self) -> &[(String, String)] {
        &self.pairs[..self.pairs.len() - 1]
    }

    fn expected(&self) -> &str {
        &self.pairs.last().expect("链至少两对").1
    }
}

/// 读对比集。格式见 `tools/predict/collocations.tsv` 的文件头。
fn load_corpus() -> Vec<Chain> {
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
        assert!(
            cols.len() >= 4 && cols.len() % 2 == 0,
            "对比集第 {} 行格式不对（应当是交替的 编码/词 对，至少两对）：{line:?}",
            i + 1
        );
        let pairs = cols
            .chunks(2)
            .map(|c| (c[0].to_owned(), c[1].to_owned()))
            .collect();
        out.push(Chain { pairs });
    }
    assert!(!out.is_empty(), "对比集是空的：{CORPUS}");
    out
}

/// 一个只属于本次运行的临时目录。
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "stele-predict-{}-{tag}-{}",
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

/// 按一组 `(编码, 词)` 对生成一份可装载的方案（音节表与词库都从这里来）。
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
  schema_id: predict
  name: 预测对比集方案
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
  delimiter: \"'\"

translator:
  dictionary: predict
"
    );

    let mut entries: Vec<String> = pairs
        .iter()
        .map(|(code, word)| format!("{word}\t{code}\t100"))
        .collect();
    entries.sort();
    entries.dedup();
    let dict = format!(
        "---\nname: predict\nversion: \"0.1.0\"\nsort: by_weight\n...\n\n{}\n",
        entries.join("\n")
    );

    std::fs::write(dir.join("predict.schema.yaml"), schema).expect("写方案");
    std::fs::write(dir.join("predict.dict.yaml"), dict).expect("写词库");
}

fn clock() -> Arc<dyn Clock> {
    Arc::new(FrozenClock {
        secs: 1_767_225_600,
        ms: 0,
        offset_secs: 0,
    })
}

/// 装配一个**挂了记忆与预测**的引擎。
fn engine_with(dir: &Path, memory: &Arc<FileMemory>) -> stele_engine::EngineImpl {
    let defs = stele_schemes::load_dir(dir).expect("装载测试方案");
    let store = Arc::clone(memory) as Arc<dyn MemoryStore>;
    let services = Services::new(clock())
        .with_ranker(Arc::new(MemoryRanker::new(Arc::clone(&store))))
        // **装上预测服务**——不装就没有 `Lane::Predict` 候选（P4b 默认关）。
        .with_prediction(store);
    stele_engine::EngineImpl::with_services(&defs, services).expect("编译测试方案")
}

fn drain(session: &mut Box<dyn Session + Send>, memory: &FileMemory) {
    let mut events = Vec::new();
    session.drain_events(&mut events);
    apply_events(memory, &events);
}

/// 敲一串键，并在**每个按键之后**把事件喂给记忆——前端必须这么做。
fn type_text(session: &mut Box<dyn Session + Send>, memory: &FileMemory, text: &str) {
    for c in text.chars() {
        session.process_key(Key::ch(c));
        drain(session, memory);
    }
}

/// 敲一个词的编码并上屏**输入通道的第 1 名**，返回上屏文本。
///
/// 用 `select(0, ..)` 而不是 `process_key(space)`：空格也选第 0 个，
/// 但显式下标能把"第 0 个一定是输入候选"这件事写进断言里
/// （预测插在第 1 位之后，所以下标 0 永远是输入候选）。
#[allow(clippy::borrowed_box)]
fn commit_code(session: &mut Box<dyn Session + Send>, memory: &FileMemory, code: &str) -> String {
    type_text(session, memory, code);
    let text = session
        .candidates()
        .first()
        .unwrap_or_else(|| panic!("编码 {code:?} 一个候选都没有"))
        .text
        .clone();
    let outcome = session.select(0, SelectionSource::Keyboard);
    assert!(
        matches!(outcome, Outcome::Committed(_)),
        "上屏 {text:?} 失败：{outcome:?}"
    );
    drain(session, memory);
    text
}

/// 把一串前缀词依次上屏，然后返回**当前预测通道**的 `(词, 分数)`（已按分数降序）。
#[allow(clippy::borrowed_box)]
fn predictions_after(
    engine: &stele_engine::EngineImpl,
    memory: &Arc<FileMemory>,
    prefix: &[(String, String)],
) -> Vec<(String, i32)> {
    let mut s = engine.create_session();
    for (code, _) in prefix {
        commit_code(&mut s, memory, code);
    }
    s.candidates()
        .iter()
        .filter(|c| c.lane == Lane::Predict)
        .map(|c| (c.text.clone(), c.score.as_milli_log()))
        .collect()
}

/// 把整条链上屏 `rounds` 遍（走完整的"按键 → 上屏 → 事件 → 记忆"那条链）。
fn learn_chain(
    engine: &stele_engine::EngineImpl,
    memory: &Arc<FileMemory>,
    chain: &Chain,
    rounds: usize,
) {
    let mut s = engine.create_session();
    for _ in 0..rounds {
        for (code, word) in &chain.pairs {
            let got = commit_code(&mut s, memory, code);
            assert_eq!(&got, word, "编码 {code:?} 应当上屏 {word:?}，实得 {got:?}");
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 验收线：对比集
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_corpus_of_common_collocations_is_predicted() {
    let chains = load_corpus();
    let all: Vec<(String, String)> = chains
        .iter()
        .flat_map(|c| c.pairs.iter().cloned())
        .collect();
    let dir = TempDir::new("corpus");
    write_scheme(dir.path(), &all);

    for chain in &chains {
        let label = chain
            .pairs
            .iter()
            .map(|(_, w)| w.as_str())
            .collect::<Vec<_>>()
            .join(" → ");

        // **每一行一份独立的记忆**：否则"今天 → 天气"会被另一行的
        // "今天 → 微信"顶掉，而那条断言就变成了对处理顺序的隐式依赖。
        let memory = Arc::new(FileMemory::in_memory(clock(), 4_096));
        let engine = engine_with(dir.path(), &memory);

        // ① 基线：还没学过 —— 期望词不该被预测出来。
        //
        // 这一步不是装饰：没有它，"期望词在第 1 位"可能只是"它恰好从
        // 别处冒出来"（例如某个滤镜顺手加的），而那证明不了预测学到了东西。
        let before = predictions_after(&engine, &memory, chain.prefix());
        assert!(
            !before.iter().any(|(t, _)| t == chain.expected()),
            "[{label}] 还没学过就预测出了期望词：{before:?}"
        );

        // ② 学：把整条链上屏 3 遍。
        learn_chain(&engine, &memory, chain, 3);
        assert!(
            memory.prediction_len() > 0,
            "[{label}] 上屏了却没有写进预测表——`Learned` 事件那条接线断了？"
        );

        // ③ 验：换一个**全新会话**、同一份记忆。
        let after = predictions_after(&engine, &memory, chain.prefix());
        assert_eq!(
            after.first().map(|(t, _)| t.as_str()),
            Some(chain.expected()),
            "[{label}] 期望词应当是预测通道的第 1 位，实得 {after:?}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 通道语义：预测可看、不可盲选；点选之后会继续学
// ─────────────────────────────────────────────────────────────────────────────

/// 一组最小搭配，专供通道语义的测试用。
fn mini() -> (TempDir, Vec<(String, String)>) {
    let pairs = vec![
        ("jia".to_owned(), "甲".to_owned()),
        ("yi".to_owned(), "乙".to_owned()),
        ("bing".to_owned(), "丙".to_owned()),
    ];
    let dir = TempDir::new("mini");
    write_scheme(dir.path(), &pairs);
    (dir, pairs)
}

#[test]
fn a_prediction_can_only_be_committed_with_an_explicit_pointer() {
    let (dir, _) = mini();
    let memory = Arc::new(FileMemory::in_memory(clock(), 4_096));
    let engine = engine_with(dir.path(), &memory);

    // 学会「甲 → 乙」。
    let mut s = engine.create_session();
    for _ in 0..2 {
        commit_code(&mut s, &memory, "jia");
        commit_code(&mut s, &memory, "yi");
    }

    // 全新会话：上屏甲，预测里应当有乙。
    let mut s = engine.create_session();
    commit_code(&mut s, &memory, "jia");
    let idx = s
        .candidates()
        .iter()
        .position(|c| c.lane == Lane::Predict && c.text == "乙")
        .expect("甲 之后应当预测出乙");

    // ① **键盘盲选**预测候选 → 不兑现（§4.3.1：预测不参与盲选）。
    let out = s.select(idx, SelectionSource::Keyboard);
    assert!(
        matches!(out, Outcome::Consumed),
        "预测候选不该被键盘盲选，实得 {out:?}"
    );
    // 候选列表原封不动 —— 说明确实什么都没发生。
    assert!(
        s.candidates().iter().any(|c| c.text == "乙"),
        "被拒的盲选不该改变候选列表"
    );

    // ② **明确点选**（鼠标 / 触摸）→ 上屏。
    let out = s.select(idx, SelectionSource::Pointer);
    match out {
        Outcome::Committed(c) => assert_eq!(c.text, "乙"),
        other => panic!("明确点选预测候选应当上屏，实得 {other:?}"),
    }
    drain(&mut s, &memory);
}

#[test]
fn committing_a_prediction_teaches_the_pair_after_it() {
    // 「甲 → 乙 → 丙」：把乙**当作预测**上屏之后，上下文变成 `甲 乙`，
    // 于是应当能预测出丙。这条守的是"预测候选的上屏也要学习"
    // （`Commit.lane == Predict` 走上下文键）。
    let (dir, _) = mini();
    let memory = Arc::new(FileMemory::in_memory(clock(), 4_096));
    let engine = engine_with(dir.path(), &memory);

    // 学：甲 乙 丙 整条链走 3 遍（丙 的上下文是 `甲 乙`）。
    let mut s = engine.create_session();
    for _ in 0..3 {
        commit_code(&mut s, &memory, "jia");
        commit_code(&mut s, &memory, "yi");
        commit_code(&mut s, &memory, "bing");
    }

    // 新会话：上屏甲，然后**用预测**选中乙。
    let mut s = engine.create_session();
    commit_code(&mut s, &memory, "jia");
    let yi = s
        .candidates()
        .iter()
        .position(|c| c.lane == Lane::Predict && c.text == "乙")
        .expect("甲 之后应当预测出乙");
    let out = s.select(yi, SelectionSource::Pointer);
    assert!(matches!(out, Outcome::Committed(_)), "点选乙应当上屏");
    drain(&mut s, &memory);

    // 上屏之后会话会重算：上下文是 `甲 乙`，预测应当是丙。
    let preds: Vec<&str> = s
        .candidates()
        .iter()
        .filter(|c| c.lane == Lane::Predict)
        .map(|c| c.text.as_str())
        .collect();
    assert_eq!(
        preds.first().copied(),
        Some("丙"),
        "预测上屏之后应当接着预测下一词，实得 {preds:?}"
    );
}

#[test]
fn predictions_are_absent_when_no_prediction_service_is_installed() {
    // **默认关**（HANDOFF §7.7.3 第 6 步的产品决定）：没有服务就没有
    // `Lane::Predict` 候选——哪怕记忆里已经学满了搭配。
    let (dir, _) = mini();
    let memory = Arc::new(FileMemory::in_memory(clock(), 4_096));
    let engine = engine_with(dir.path(), &memory);
    let mut s = engine.create_session();
    for _ in 0..3 {
        commit_code(&mut s, &memory, "jia");
        commit_code(&mut s, &memory, "yi");
    }
    assert!(memory.prediction_len() > 0, "记忆里应当有搭配");

    // 换一个**没挂预测服务**的引擎，用同一份记忆。
    let defs = stele_schemes::load_dir(dir.path()).expect("装载测试方案");
    let store = Arc::clone(&memory) as Arc<dyn MemoryStore>;
    let services = Services::new(clock()).with_ranker(Arc::new(MemoryRanker::new(store)));
    let engine = stele_engine::EngineImpl::with_services(&defs, services).expect("编译");
    let mut s = engine.create_session();
    commit_code(&mut s, &memory, "jia");
    assert!(
        s.candidates().iter().all(|c| c.lane == Lane::Input),
        "没有预测服务时不该有预测候选：{:?}",
        s.candidates().iter().map(|c| &c.text).collect::<Vec<_>>()
    );
}

#[test]
fn the_corpus_is_not_empty_and_every_case_is_a_valid_chain() {
    // 前置：对比集本身是好的。一条"跑了个空表"的测试永远是绿的。
    let chains = load_corpus();
    assert!(chains.len() >= 10, "对比集太小：{} 条", chains.len());
    for c in &chains {
        assert!(c.pairs.len() >= 2, "链至少两对：{c:?}");
        for (code, word) in &c.pairs {
            assert!(!code.is_empty() && !word.is_empty(), "空的编码或词：{c:?}");
        }
    }
}
