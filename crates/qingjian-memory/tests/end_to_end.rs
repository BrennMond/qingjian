//! 用户记忆的**端到端**验收（P4a 的验收 1 与 3）。
//!
//! # 这份测试在回答什么
//!
//! HANDOFF §7.6.0 的两条验收是行为断言，不是单元性质：
//!
//! 1. **打过的词下次优先**：同一个词连续上屏过 N 次后，它排到同码候选之前。
//! 2. **进程重启后学到的词还在**（持久化可用）。
//!
//! 它们都必须**穿过整条链**才算数：按键 → 切分 → 翻译 → 重排 →
//! `drain_events` → `apply_events` → `MemoryStore::record` → 落盘 → 重装 →
//! 重排。任何一环没接上，这两条都会红——而这正是它们的价值：
//! HANDOFF 把"忘了接 `Learned` 事件"列为这一步最容易漏、且**漏了不报错**的坑。
//!
//! # 为什么用自建的临时方案，而不是 `schemes/qingjian-default`
//!
//! 因为这里要断言的是**算术**（学到多少次才能反超），而不是词库内容。
//! 自建的方案让权重由测试自己写死：甲 10000、乙 1。真实词库里
//! 候选的权重是数据，改一次词表就可能让"打 4 次反超"变成"打 5 次"，
//! 于是一条行为断言变成了对词表的**隐式依赖**——那不是好的测试。
//!
//! 方案是**精确编码族**（`exact_code`）：`ab` → 编码 `[a, b]`。
//! 挑它是因为它最短：不需要拼写规则、不需要切分图，
//! 于是这份测试里**只有记忆**这一件事在起作用。

use std::sync::Arc;

use qingjian_core::{
    Clock, Engine, FrozenClock, Key, MemoryStore, Outcome, SelectionSource, Services, Session,
};
use qingjian_memory::{apply_events, FileMemory, MemoryRanker};

/// 方案里的两个词：`甲` 权重 10000（分 9210），`乙` 权重 1（分 0）。
const SCHEMA: &str = r#"
schema:
  schema_id: mem
  name: 记忆测试方案
  version: "0.1.0"

switches:
  - name: ascii_mode
    states: [中, Ａ]
    reset: 0

engine:
  tag: code
  translator: exact_code
  candidate_cap: 200

speller:
  alphabet: [a, b]

translator:
  dictionary: mem
"#;

const DICT: &str = "\
---
name: mem
version: \"0.1.0\"
sort: by_weight
...

甲\ta b\t10000
乙\ta b\t1
";

/// 一个只属于本次运行的临时目录。
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "qingjian-memory-e2e-{}-{tag}-{}",
            std::process::id(),
            // 同一进程内多个测试并行时会撞名，用纳秒时间戳区分。
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        Self(dir)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 写出方案文件，返回目录。
fn scheme_dir(tag: &str) -> TempDir {
    let dir = TempDir::new(tag);
    std::fs::write(dir.path().join("mem.schema.yaml"), SCHEMA).expect("写方案");
    std::fs::write(dir.path().join("mem.dict.yaml"), DICT).expect("写词库");
    dir
}

fn clock() -> Arc<dyn Clock> {
    Arc::new(FrozenClock {
        secs: 1_767_225_600,
        ms: 0,
        offset_secs: 0,
    })
}

/// 装配一个挂了记忆的引擎。
fn engine_with(dir: &std::path::Path, memory: &Arc<FileMemory>) -> qingjian_engine::EngineImpl {
    let defs = qingjian_schemes::load_dir(dir).expect("装载测试方案");
    let services = Services::new(clock()).with_ranker(Arc::new(MemoryRanker::new(
        Arc::clone(memory) as Arc<dyn MemoryStore>,
    )));
    qingjian_engine::EngineImpl::with_services(&defs, services).expect("编译测试方案")
}

/// 敲一串键，并在**每个按键之后**把事件喂给记忆——前端必须这么做。
fn type_text(session: &mut Box<dyn Session + Send>, memory: &FileMemory, text: &str) {
    for c in text.chars() {
        session.process_key(Key::ch(c));
        drain(session, memory);
    }
}

fn drain(session: &mut Box<dyn Session + Send>, memory: &FileMemory) {
    let mut events = Vec::new();
    session.drain_events(&mut events);
    apply_events(memory, &events);
}

/// 选中第 `index` 个候选上屏，并把学习事件喂给记忆。
fn commit(session: &mut Box<dyn Session + Send>, memory: &FileMemory, index: usize) -> String {
    let text = session.candidates()[index].text.clone();
    let outcome = session.select(index, SelectionSource::Keyboard);
    assert!(
        matches!(outcome, Outcome::Committed(_)),
        "选中第 {index} 个候选应当上屏，实得 {outcome:?}"
    );
    drain(session, memory);
    text
}

// 与 `p3_pipeline.rs` 同样的写法：会话是 `Box<dyn Session + Send>`，
// 传 `&Box<..>` 是为了避免在每一处调用点写 `&**s`。
#[allow(clippy::borrowed_box)]
/// 装配一个挂在**内嵌演示方案**上的引擎（跨拼法共享要用拼音方案测）。
fn demo_engine(memory: &Arc<FileMemory>) -> qingjian_engine::EngineImpl {
    let defs = qingjian_schemes::all().expect("内嵌方案必须能装载");
    let services = Services::new(clock()).with_ranker(Arc::new(MemoryRanker::new(
        Arc::clone(memory) as Arc<dyn MemoryStore>,
    )));
    qingjian_engine::EngineImpl::with_services(&defs, services).expect("编译内嵌方案")
}

/// 某个候选文本当前的分数；找不到返回 `None`。
#[allow(clippy::borrowed_box)]
fn score_of(session: &Box<dyn Session + Send>, text: &str) -> Option<i32> {
    session
        .candidates()
        .iter()
        .find(|c| c.text == text)
        .map(|c| c.score.as_milli_log())
}

#[allow(clippy::borrowed_box)]
fn texts(session: &Box<dyn Session + Send>) -> Vec<String> {
    session
        .candidates()
        .iter()
        .map(|c| c.text.clone())
        .collect()
}

#[test]
fn the_scheme_has_two_words_for_one_code() {
    // 前置：这条测试成立的前提是"同一个编码有两个候选"。
    // 没有它就等于在测一个不存在的场景——所以先把它钉住。
    let dir = scheme_dir("precondition");
    let memory = Arc::new(FileMemory::in_memory(clock(), 1_000));
    let engine = engine_with(dir.path(), &memory);
    let mut s = engine.create_session();
    type_text(&mut s, &memory, "ab");
    let got = texts(&s);
    assert!(got.len() >= 3, "应当是 甲 / 乙 / 原样上屏，实得 {got:?}");
    assert_eq!(got[0], "甲");
    assert_eq!(got[1], "乙");
}

#[test]
fn a_word_committed_enough_times_comes_first() {
    let dir = scheme_dir("overtake");
    let memory = Arc::new(FileMemory::in_memory(clock(), 1_000));
    let engine = engine_with(dir.path(), &memory);
    let mut s = engine.create_session();

    // 乙 的基础分是 0，甲 是 9210。加成曲线：1 次 ≈ 4666、3 次 ≈ 8400、
    // 4 次 ≈ 9333 —— 所以"第 4 次"是分界点，而分界点两侧都要断言。
    for round in 1..=4 {
        type_text(&mut s, &memory, "ab");
        if round == 1 {
            let got = texts(&s);
            assert_eq!(got[0], "甲", "打 1 次还不够反超（{got:?}）");
        }
        let picked = commit(&mut s, &memory, 1);
        assert_eq!(picked, "乙");
    }

    type_text(&mut s, &memory, "ab");
    let got = texts(&s);
    assert_eq!(
        got[0], "乙",
        "同一个词连续上屏 4 次之后应当排到同码候选之前，实得 {got:?}"
    );
    // 精确优先没有被破坏：被顶下去的仍然在列表里。
    assert!(got.contains(&"甲".to_owned()));
}

#[test]
fn one_commit_does_not_reorder_anything_else() {
    // 反面：学过 乙 之后，**别的输入**的候选顺序一点都不许变。
    //
    // 判据是"与没有记忆时逐项相同"，而不是写死一个列表：写死列表会把
    // **与记忆无关**的行为（例如补全的默认值）也钉进这条测试里，
    // 于是改默认值会让一条关于记忆的测试变红——那是最误导人的一种红。
    let dir = scheme_dir("isolation");

    // ① 无记忆的基线。
    let cold = {
        let memory = Arc::new(FileMemory::in_memory(clock(), 1_000));
        let engine = engine_with(dir.path(), &memory);
        let mut s = engine.create_session();
        type_text(&mut s, &memory, "a");
        texts(&s)
    };

    // ② 有记忆、且刚学过一个**别的编码**的词。
    let memory = Arc::new(FileMemory::in_memory(clock(), 1_000));
    let engine = engine_with(dir.path(), &memory);
    let mut s = engine.create_session();
    type_text(&mut s, &memory, "ab");
    commit(&mut s, &memory, 1);
    type_text(&mut s, &memory, "a");
    let warm = texts(&s);

    assert_eq!(
        warm, cold,
        "输入 `a` 没有学过任何东西，候选顺序必须与无记忆时逐项相同"
    );
    assert!(!warm.is_empty(), "至少要有一个候选（原样上屏兜底）");
}

#[test]
fn what_was_learned_survives_a_restart() {
    // 验收 3：落盘 → 重新装载 → 学到的词仍然优先。
    let dir = scheme_dir("restart");
    let db = dir.path().join("user.mem");

    let learned = {
        let memory = Arc::new(FileMemory::open(&db, clock(), 1_000).expect("空表"));
        let engine = engine_with(dir.path(), &memory);
        let mut s = engine.create_session();
        for _ in 0..4 {
            type_text(&mut s, &memory, "ab");
            commit(&mut s, &memory, 1);
        }
        assert!(memory.flush().expect("落盘"), "有改动时应当真的写盘");
        memory.snapshot().len()
    };
    assert_eq!(learned, 1, "只学了一个词");

    // 模拟"进程重启"：全新的 store、全新的引擎，只有文件是旧的。
    let memory = Arc::new(FileMemory::open(&db, clock(), 1_000).expect("读回"));
    assert_eq!(memory.snapshot().len(), 1, "重启后记录必须还在");
    let engine = engine_with(dir.path(), &memory);
    let mut s = engine.create_session();
    type_text(&mut s, &memory, "ab");
    let got = texts(&s);
    assert_eq!(got[0], "乙", "重启之后学到的词仍然优先，实得 {got:?}");
}

#[test]
fn a_corrupt_database_degrades_without_blocking_startup() {
    // 验收 5：坏文件 = 降级成"没有记忆" + 警告，绝不阻止启动（D26）。
    let dir = scheme_dir("corrupt");
    let db = dir.path().join("user.mem");
    std::fs::write(&db, b"this is not a memory database").unwrap();

    let (memory, warning) = FileMemory::open_or_degrade(&db, clock(), 1_000);
    let warning = warning.expect("坏文件必须给出警告");
    assert!(warning.contains("不可用"), "警告要能读懂：{warning}");

    let memory = Arc::new(memory);
    let engine = engine_with(dir.path(), &memory);
    let mut s = engine.create_session();
    type_text(&mut s, &memory, "ab");
    // 引擎照常工作 —— 这才是"绝不阻止启动"。
    let got = texts(&s);
    assert_eq!(got[0], "甲", "坏记忆文件不该影响打字：{got:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// D42：**一条编码一把键** ⇒ 跨拼法共享（这是本次改动的全部理由）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_word_learned_by_abbreviation_helps_the_full_spelling() {
    // 用**真实的拼音方案**（内嵌演示）走完整条链：
    // 翻译器产出候选并附带编码键 → 上屏 → 事件 → 记忆 → 下次重排。
    //
    // 这一条与 rime-ice 的功能表现对齐：RIME 的用户词典以编码为键，
    // 所以简拼学到的词在全拼下也优先。
    let memory = Arc::new(FileMemory::in_memory(clock(), 1_000));
    let engine = demo_engine(&memory);
    let mut s = engine.create_session();
    s.switch_schema("pinyin-demo").expect("切到内嵌拼音方案");

    // ① 基线：全拼 `nihao` 下「你好」的分数。
    type_text(&mut s, &memory, "nihao");
    let before = score_of(&s, "你好").expect("demo 方案里 nihao 应当出「你好」");
    s.reset();

    // ② 用**简拼** `nhao` 选中它四次。
    for _ in 0..4 {
        type_text(&mut s, &memory, "nhao");
        let picked = commit(&mut s, &memory, 0);
        assert_eq!(picked, "你好", "nhao 的第一个候选应当是「你好」");
    }

    // ③ 回到全拼：分数必须涨。
    type_text(&mut s, &memory, "nihao");
    let after = score_of(&s, "你好").expect("全拼下仍然要出「你好」");
    assert!(
        after > before,
        "简拼学到的词必须帮到全拼（D42）：{before} → {after}"
    );
}

#[test]
fn the_learned_key_is_the_canonical_code_not_the_spelling() {
    // 直接检查**落库的键**：简拼上屏之后，记忆里存的必须是编码键
    // （`ni'hao`），不是用户敲的那串（`nhao`）。
    let memory = Arc::new(FileMemory::in_memory(clock(), 1_000));
    let engine = demo_engine(&memory);
    let mut s = engine.create_session();
    s.switch_schema("pinyin-demo").expect("切到内嵌拼音方案");

    type_text(&mut s, &memory, "nhao");
    commit(&mut s, &memory, 0);

    let keys: Vec<String> = memory.snapshot().into_iter().map(|e| e.input).collect();
    assert_eq!(
        keys,
        vec!["ni'hao".to_owned()],
        "落库的键应当是规范编码；实得 {keys:?}"
    );
    // 而且**拿编码键查得到**、拿拼写查不到（拼写只是到达它的一条路径）。
    assert_eq!(memory.lookup("ni'hao").len(), 1);
    assert!(memory.lookup("nhao").is_empty());
}
