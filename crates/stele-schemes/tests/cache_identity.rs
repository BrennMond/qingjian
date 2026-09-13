//! # P0-B 回归：部署词库缓存必须绑定**字母表映射**
//!
//! 审计给的最小复现（原始证据在审计快照里）：
//!
//! ```yaml
//! # 初始
//! speller:
//!   alphabet: [ni, hao]
//! # 词典：你 ni；好 hao
//! ```
//!
//! 首次部署后输入 `ni` 得到「你」。只把 `alphabet` 改成 `[hao, ni]`、
//! **词典一个字节不改**，缓存仍被复用——输入 `ni` 得到「好」。删掉
//! `.stele-cache` 重新编译才恢复。**全程没有任何诊断。**
//!
//! 根因：二进制表里存的是**编码单元的下标**，而缓存文件名只依赖源词典
//! 的校验和。下标到音节的映射没有进入缓存身份。
//!
//! 这一组测试是端到端的：真的写文件、真的走目录部署装载、真的驱动
//! 一个会话敲键，而不是只断言"指纹函数变了"。
//!
//! **反例必须失败**：如果哪天有人把缓存身份改回"只看源校验和"，
//! `reordering_the_alphabet_recompiles_and_keeps_the_right_word` 会红。

use stele_core::{Engine, Key};
use stele_engine::EngineImpl;

/// 一份最小方案：`alphabet` 可参数化，词典固定为「你 ni / 好 hao」。
fn schema_text(alphabet: &str) -> String {
    format!(
        "\
schema:
  schema_id: cacheid
  name: 缓存身份
  version: \"1.0\"
engine:
  tag: abc
  translator: spelling_graph
speller:
  alphabet: [{alphabet}]
translator:
  dictionary: d
"
    )
}

const DICT: &str = "\
---
name: d
version: \"1\"
sort: by_weight
...

你\tni\t100
好\thao\t90
";

/// 一个独立的临时工作目录（方案 + 缓存）。
struct Work {
    root: std::path::PathBuf,
}

impl Work {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "stele-cacheid-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("建临时目录");
        Self { root }
    }

    fn write_schema(&self, alphabet: &str) {
        std::fs::write(self.root.join("cacheid.schema.yaml"), schema_text(alphabet))
            .expect("写方案");
        std::fs::write(self.root.join("d.dict.yaml"), DICT).expect("写词典");
    }

    fn cache_dir(&self) -> std::path::PathBuf {
        self.root.join(".stele-cache")
    }

    /// 缓存目录里现有的 `.table` 文件（排序后）。
    fn cached_tables(&self) -> Vec<String> {
        let dir = self.cache_dir();
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut v: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| {
                std::path::Path::new(n)
                    .extension()
                    .is_some_and(|e| e == "table")
            })
            .collect();
        v.sort();
        v
    }

    /// 按**部署路径**装载（真实二进制词库）并敲 `keys`，返回候选文本。
    fn type_keys(&self, keys: &str) -> Vec<String> {
        let loaded = stele_schemes::load_dir_deployed_layered(&self.root, &self.cache_dir())
            .unwrap_or_else(|e| panic!("部署装载必须成功：{e}"));
        let defs: Vec<_> = loaded.into_iter().map(|l| l.def).collect();
        let engine = EngineImpl::new(&defs).expect("引擎应当能编译");
        let mut s = engine.create_session();
        for c in keys.chars() {
            s.process_key(Key::ch(c));
        }
        s.candidates().iter().map(|c| c.text.clone()).collect()
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn reordering_the_alphabet_recompiles_and_keeps_the_right_word() {
    let w = Work::new("reorder");
    w.write_schema("ni, hao");

    // ① 首次部署：`ni` 必须是「你」。
    let first = w.type_keys("ni");
    assert_eq!(
        first.first().map(String::as_str),
        Some("你"),
        "初始部署应当给「你」，实得 {first:?}"
    );
    let after_first = w.cached_tables();
    assert_eq!(
        after_first.len(),
        1,
        "应当只编译出一份产物：{after_first:?}"
    );

    // ② **只改字母表顺序，词典与缓存目录都不动。**
    w.write_schema("hao, ni");

    let second = w.type_keys("ni");
    assert_eq!(
        second.first().map(String::as_str),
        Some("你"),
        "字母表重排后**绝不能**复用旧产物：缓存里的下标 0 原本是 `ni`，\
         重排后是 `hao`，复用就会让 `ni` 出「好」。实得 {second:?}"
    );
    assert!(
        !second.contains(&"好".to_owned()) || second.first().map(String::as_str) == Some("你"),
        "「好」不该出现在 `ni` 的首位：{second:?}"
    );

    // ③ 必须**真的重新编译**（旧产物仍在，但多出一份新指纹的产物）。
    let after_second = w.cached_tables();
    assert_eq!(
        after_second.len(),
        2,
        "字母表变了必须产生一份新身份的产物，实得 {after_second:?}"
    );
    assert_ne!(after_first, after_second);
}

#[test]
fn the_cache_is_reused_when_nothing_semantic_changed() {
    // 另一半：**没变就不该重编**。否则"每次启动都重新部署十几万词条"
    // 会被当成"修好了"，而那是把性能问题换成了正确性问题。
    let w = Work::new("reuse");
    w.write_schema("ni, hao");
    let _ = w.type_keys("ni");
    let first = w.cached_tables();
    let _ = w.type_keys("ni");
    assert_eq!(first, w.cached_tables(), "语义输入没变时不得产生新产物");
}

#[test]
fn changing_the_dictionary_recompiles() {
    let w = Work::new("dict");
    w.write_schema("ni, hao");
    let _ = w.type_keys("ni");
    let first = w.cached_tables();

    // 改词典内容（同一个编码指向另一个词）。
    std::fs::write(
        w.root.join("d.dict.yaml"),
        "---\nname: d\nversion: \"1\"\nsort: by_weight\n...\n\n甲\tni\t100\n",
    )
    .unwrap();
    let got = w.type_keys("ni");
    assert_eq!(got.first().map(String::as_str), Some("甲"), "实得 {got:?}");
    assert_ne!(first, w.cached_tables(), "源词典变了必须重编");
}

#[test]
fn every_semantic_input_enters_the_cache_name() {
    // 直接对着"缓存文件名"断言：它必须随字母表内容/顺序变化。
    // 这条比上面三条更靠近根因，失败时的诊断也更短。
    let w = Work::new("name");
    w.write_schema("ni, hao");
    let _ = w.type_keys("ni");
    let a = w.cached_tables();

    w.write_schema("ni, hao, ta");
    let _ = w.type_keys("ni");
    let b = w.cached_tables();
    assert_ne!(a, b, "字母表**内容**变了，缓存身份必须变：{a:?} vs {b:?}");

    w.write_schema("hao, ni, ta");
    let _ = w.type_keys("ni");
    let c = w.cached_tables();
    assert_ne!(b, c, "字母表**顺序**变了，缓存身份必须变：{b:?} vs {c:?}");
}
