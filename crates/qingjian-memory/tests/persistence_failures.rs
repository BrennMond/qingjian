//! # P0-D 回归：用户记忆的持久化失败、重试与并发
//!
//! 审计给的复现（原始证据在审计快照里）：
//!
//! ```text
//! flush() -> Err(IsADirectory)
//! dirty == false
//! 移除障碍后再次 flush() -> Ok(false)
//! 目标文件不存在，内存中的记录未被保存
//! ```
//!
//! 根因：`flush` 在**写临时文件之前**就把 `dirty` 清成了 `false`。
//! 于是一次失败会永久关掉之后所有重试的机会：用户学到的东西一次都存不下，
//! 而且没有任何提示。
//!
//! 这一组测试只走**公开 API**（`open` / `record` / `flush` / `is_dirty` /
//! `lookup`），所以它约束的是行为，不是实现细节。

use std::sync::Arc;

use qingjian_core::{Clock, Commit, FrozenClock, Lane, MemoryStore, Origin, SpellingAttr, Trigger};

fn clock(secs: u64) -> Arc<dyn Clock> {
    Arc::new(FrozenClock {
        secs,
        ms: 0,
        offset_secs: 0,
    })
}

fn commit(key: &str, text: &str) -> Commit {
    Commit {
        text: text.into(),
        input: key.into(),
        context: vec![],
        origin: Origin::SystemWord,
        attr: SpellingAttr::NORMAL,
        lane: Lane::Input,
        trigger: Trigger::Space,
        key: Some(key.into()),
    }
}

/// 一个独立的临时数据库路径。
struct Db {
    path: std::path::PathBuf,
}

impl Db {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "qingjian-mem-{tag}-{}-{:?}.mem",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&path);
        Self { path }
    }

    fn open(&self, cap: usize) -> qingjian_memory::FileMemory {
        qingjian_memory::FileMemory::open(&self.path, clock(0), cap).expect("打开记忆")
    }

    fn texts(m: &qingjian_memory::FileMemory, key: &str) -> Vec<String> {
        m.lookup(key).into_iter().map(|e| e.text).collect()
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir_all(&self.path);
        let _ = std::fs::remove_file(self.path.with_extension("mem.lock"));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ① 失败必须保留 dirty，并且**可以重试**
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_failed_rename_keeps_dirty_and_the_retry_succeeds() {
    let db = Db::new("retry");
    let m = db.open(100);
    m.record(&commit("ni", "你"));
    assert!(m.is_dirty(), "刚记过就该是 dirty");

    // 让目标路径变成一个**目录**：临时文件能写，`rename` 必定失败。
    std::fs::create_dir_all(&db.path).expect("把目标路径变成目录");

    let e = m.flush().expect_err("改名到目录上必须失败");
    assert!(!e.to_string().is_empty());
    assert!(
        m.is_dirty(),
        "**写盘失败之后必须仍然是 dirty**，否则重试的机会就永久没了"
    );

    // 排除障碍，再试一次。
    std::fs::remove_dir_all(&db.path).expect("移除障碍");
    assert!(
        m.flush().expect("重试必须成功"),
        "排除障碍之后的 flush 必须真的写盘"
    );
    assert!(!m.is_dirty());

    // 重新打开：记录必须在。
    let again = db.open(100);
    assert_eq!(
        Db::texts(&again, "ni"),
        vec!["你".to_owned()],
        "重试之后落盘的记录必须能被读回来"
    );
}

#[test]
fn a_failed_flush_never_reports_success_before_retrying() {
    // 旧实现的症状：第一次 Err、第二次 Ok(false)，**磁盘上什么都没有**。
    // 这条测试把"第二次返回 false"钉死为失败。
    let db = Db::new("nosilent");
    let m = db.open(100);
    m.record(&commit("hao", "好"));
    std::fs::create_dir_all(&db.path).unwrap();
    let _ = m.flush(); // 必定失败
    std::fs::remove_dir_all(&db.path).unwrap();

    let wrote = m.flush().expect("重试");
    assert!(
        wrote,
        "重试必须真的写盘（旧实现在这里返回 Ok(false)，记录静默丢失）"
    );
    let again = db.open(100);
    assert!(Db::texts(&again, "hao").contains(&"好".to_owned()));
}

#[test]
fn a_stale_tmp_directory_does_not_block_flushing() {
    // 旧实现的临时文件名是固定的 `<name>.tmp`。用户环境里若存在同名目录，
    // `write(tmp)` 会**永远**失败，而且因为 dirty 已经被清掉，连重试都没有。
    // 唯一临时文件名把这一类障碍整类消掉。
    let db = Db::new("staletmp");
    let m = db.open(100);
    m.record(&commit("ni", "你"));

    let stale = db.path.with_file_name(format!(
        "{}.tmp",
        db.path.file_name().unwrap().to_string_lossy()
    ));
    std::fs::create_dir_all(&stale).expect("预置一个同名 .tmp 目录");

    m.flush().expect("唯一临时文件名不该被同名目录挡住");
    let again = db.open(100);
    assert_eq!(Db::texts(&again, "ni"), vec!["你".to_owned()]);

    let _ = std::fs::remove_dir_all(&stale);
}

// ─────────────────────────────────────────────────────────────────────────────
// ② 并发写者不能互相覆盖
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn two_writers_merge_instead_of_overwriting() {
    // 审计复现：两个 `FileMemory` 句柄依次保存，后写者覆盖先写者。
    let db = Db::new("two");
    let a = db.open(100);
    let b = db.open(100);

    a.record(&commit("ni", "你"));
    b.record(&commit("hao", "好"));

    a.flush().expect("a 落盘");
    b.flush().expect("b 落盘");

    let again = db.open(100);
    assert_eq!(
        Db::texts(&again, "ni"),
        vec!["你".to_owned()],
        "先写者的记录不能被后写者覆盖"
    );
    assert_eq!(
        Db::texts(&again, "hao"),
        vec!["好".to_owned()],
        "后写者的记录也要在"
    );
}

#[test]
fn merging_is_idempotent_and_does_not_inflate_counts() {
    // 合并策略必须是**幂等**的：把同一份盘上数据重复并进来，
    // 次数不能翻倍（否则"加载两次"会凭空造出高频词）。
    let db = Db::new("idem");
    let m = db.open(100);
    m.record(&commit("ni", "你"));
    m.record(&commit("ni", "你"));
    let before = m.lookup("ni")[0].count;
    assert_eq!(before, 2);

    m.flush().expect("落盘");
    // 再落盘一次：此时盘上就是自己刚写的内容，合并应当是恒等变换。
    m.record(&commit("hao", "好"));
    m.flush().expect("第二次落盘");

    let again = db.open(100);
    let after = again.lookup("ni")[0].count;
    assert_eq!(after, 2, "合并自己写过的文件不能把次数翻倍");
}

#[test]
fn a_write_after_a_snapshot_is_still_dirty() {
    // 代次语义：落盘只承认"快照那一刻"的状态。
    let db = Db::new("gen");
    let m = db.open(100);
    m.record(&commit("ni", "你"));
    m.flush().expect("落盘");
    assert!(!m.is_dirty());

    m.record(&commit("hao", "好"));
    assert!(m.is_dirty(), "落盘之后的新写入必须重新变 dirty");
    m.flush().expect("再落盘");
    let again = db.open(100);
    assert_eq!(Db::texts(&again, "hao"), vec!["好".to_owned()]);
}

// ─────────────────────────────────────────────────────────────────────────────
// ③ 恢复路径也要遵守容量上限
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn capacity_is_enforced_when_loading() {
    let db = Db::new("cap");
    {
        let big = db.open(1000);
        for i in 0..50 {
            big.record(&commit(&format!("k{i}"), &format!("词{i}")));
        }
        big.flush().expect("落盘 50 条");
    }
    // 用 cap=1 打开一份含 50 条的文件。
    let small = db.open(1);
    assert!(
        small.len() <= 1,
        "恢复路径必须施加 cap（旧实现会原样加载 50 条），实得 {}",
        small.len()
    );
    assert_eq!(small.capacity(), 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// ④ 新建的记忆文件只有所有者可读写
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(unix)]
#[test]
fn the_memory_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let db = Db::new("perm");
    let m = db.open(100);
    m.record(&commit("ni", "你"));
    m.flush().expect("落盘");

    let mode = std::fs::metadata(&db.path)
        .expect("文件应当存在")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o600,
        "记忆是敏感的本机行为历史，新建文件必须是 0600，实得 {mode:o}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑤ 坏文件不覆盖
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_corrupt_file_is_not_overwritten_by_flushing() {
    let db = Db::new("corrupt");
    std::fs::write(&db.path, b"this is not a qingjian memory file").unwrap();

    let (m, note) = qingjian_memory::FileMemory::open_or_degrade(&db.path, clock(0), 100);
    assert!(note.is_some(), "降级必须出声");
    assert!(!m.is_writable(), "读不懂的文件不许被覆盖");
    assert!(
        !m.flush().expect("flush 在不可写时是 Ok(false)"),
        "不可写实例不得写盘"
    );

    let raw = std::fs::read(&db.path).unwrap();
    assert_eq!(
        raw, b"this is not a qingjian memory file",
        "坏文件必须原样保留"
    );
}
