//! # 同一个 `Lexicon` trait，两种实现必须**语义相同**（审计 §2.G4）
//!
//! 审计的观察：
//!
//! > 显式 `enable_completion: true` 时，小方案使用内存词库可从 `ni` 得到
//! > 「你、你好」；走实际二进制部署词库只得到「你」。`TableLexicon`
//! > 没有相应 prefix 查询实现。
//!
//! 后来这个缺口以另一种形态又出现了一次：`TableLexicon` **声明了**
//! `supports_prefix() == true` 却没有实现 `prefix_lookup`，于是
//! `shape` 方案敲 `ab` 时，内存实现给 4 条候选（含补全）、部署实现给 2 条。
//! **声明能力与实现能力不一致**，而且没有任何诊断。
//!
//! 这一组测试把两台实现的**每一次能力**逐项对照：能力声明、精确查询、
//! 前缀存在性、前缀查询的文本与属性。任何一处分叉都会变红。
//!
//! 这是审计 §2.G5 要求的"配置字段审计表"在 **trait 层**的对应物：
//! 不是"字段有没有人读"，而是"两个实现是不是同一件事"。

use stele_core::{
    Candidate, CandidateSink, CodeAlphabet, CodeUnitId, Lexicon, Score, SpellingAttr,
};
use stele_engine::lexicon::InMemoryLexicon;

/// 夹具：覆盖"精确命中 / 同码多词 / 前缀补全 / 完全不命中"四种情形。
fn entries() -> Vec<(Vec<String>, String, f64)> {
    vec![
        (vec!["ni".into()], "你".into(), 20_000.0),
        (vec!["ni".into(), "hao".into()], "你好".into(), 10_000.0),
        (vec!["ni".into(), "hao".into()], "拟好".into(), 9_000.0),
        (
            vec!["ni".into(), "hao".into(), "ma".into()],
            "你好吗".into(),
            8_000.0,
        ),
        (vec!["hao".into()], "好".into(), 18_000.0),
        (vec!["shi".into()], "是".into(), 15_000.0),
    ]
}

fn alphabet() -> CodeAlphabet {
    CodeAlphabet::new(vec!["ni".into(), "hao".into(), "shi".into(), "ma".into()])
}

/// 一个"被测词库"的抽象：把两种实现的同名操作收成同一个调用面。
trait Probe {
    fn supports_prefix(&self) -> bool;
    fn lookup(&self, code: &[CodeUnitId]) -> Vec<(String, i32)>;
    fn prefix_lookup(&self, code: &[CodeUnitId], exclude_exact: bool) -> Vec<(String, i32, u8)>;
    fn has_prefix(&self, code: &[CodeUnitId]) -> bool;
}

fn code(texts: &[&str]) -> Vec<CodeUnitId> {
    let a = alphabet();
    texts
        .iter()
        .map(|t| a.id_of(t).unwrap_or_else(|| panic!("字母表里没有 {t}")))
        .collect()
}

impl Probe for InMemoryLexicon {
    fn supports_prefix(&self) -> bool {
        Lexicon::supports_prefix(self)
    }
    fn lookup(&self, c: &[CodeUnitId]) -> Vec<(String, i32)> {
        let mut buf: Vec<Candidate> = Vec::new();
        let mut sink = CandidateSink::new(&mut buf, 64);
        Lexicon::lookup(self, c, &mut sink);
        buf.into_iter()
            .map(|x| (x.text, x.score.as_milli_log()))
            .collect()
    }
    fn prefix_lookup(&self, c: &[CodeUnitId], exclude_exact: bool) -> Vec<(String, i32, u8)> {
        let mut buf: Vec<Candidate> = Vec::new();
        let mut sink = CandidateSink::new(&mut buf, 64);
        Lexicon::prefix_lookup(self, c, exclude_exact, &mut sink);
        buf.into_iter()
            .map(|x| (x.text, x.score.as_milli_log(), x.attr.bits()))
            .collect()
    }
    fn has_prefix(&self, c: &[CodeUnitId]) -> bool {
        Lexicon::has_prefix(self, c)
    }
}

impl Probe for stele_table::TableLexicon {
    fn supports_prefix(&self) -> bool {
        Lexicon::supports_prefix(self)
    }
    fn lookup(&self, c: &[CodeUnitId]) -> Vec<(String, i32)> {
        let mut buf: Vec<Candidate> = Vec::new();
        let mut sink = CandidateSink::new(&mut buf, 64);
        Lexicon::lookup(self, c, &mut sink);
        buf.into_iter()
            .map(|x| (x.text, x.score.as_milli_log()))
            .collect()
    }
    fn prefix_lookup(&self, c: &[CodeUnitId], exclude_exact: bool) -> Vec<(String, i32, u8)> {
        let mut buf: Vec<Candidate> = Vec::new();
        let mut sink = CandidateSink::new(&mut buf, 64);
        Lexicon::prefix_lookup(self, c, exclude_exact, &mut sink);
        buf.into_iter()
            .map(|x| (x.text, x.score.as_milli_log(), x.attr.bits()))
            .collect()
    }
    fn has_prefix(&self, c: &[CodeUnitId]) -> bool {
        Lexicon::has_prefix(self, c)
    }
}

/// 造出两台实现，跑同一组探针，逐项比较。
fn compare<P: Probe, Q: Probe>(what: &str, mem: &P, table: &Q) {
    assert_eq!(
        mem.supports_prefix(),
        table.supports_prefix(),
        "{what}：`supports_prefix()` 两台实现必须一致"
    );
    let probes: Vec<Vec<CodeUnitId>> = vec![
        code(&["ni"]),
        code(&["ni", "hao"]),
        code(&["ni", "hao", "ma"]),
        code(&["hao"]),
        code(&["shi"]),
        code(&["ma"]),
        code(&["hao", "shi"]),
        vec![CodeUnitId(999)],
    ];
    for c in &probes {
        let alpha = alphabet();
        let label: Vec<&str> = c.iter().map(|u| alpha.text(*u).unwrap_or("?")).collect();
        assert_eq!(
            mem.lookup(c),
            table.lookup(c),
            "{what}：`lookup({label:?})` 两台实现必须一致"
        );
        assert_eq!(
            mem.has_prefix(c),
            table.has_prefix(c),
            "{what}：`has_prefix({label:?})` 两台实现必须一致"
        );
        for exclude_exact in [false, true] {
            assert_eq!(
                mem.prefix_lookup(c, exclude_exact),
                table.prefix_lookup(c, exclude_exact),
                "{what}：`prefix_lookup({label:?}, exclude_exact={exclude_exact})` \
                 两台实现必须一致（文本、分数、属性）"
            );
        }
    }
}

#[test]
fn the_two_lexicon_implementations_agree_on_every_capability() {
    let p = std::env::temp_dir().join(format!("stele-lexcap-{}.table", std::process::id()));
    let _ = std::fs::remove_file(&p);
    let fp = stele_table::BuildFingerprint::of(
        stele_table::FORMAT_VERSION,
        stele_table::COMPILER_OPTIONS,
        &[],
        0,
    );
    stele_table::compile(
        0,
        fp,
        |w| {
            for (code_texts, word, weight) in entries() {
                let ids: Vec<u16> = code_texts
                    .iter()
                    .map(|t| {
                        u16::try_from(alphabet().id_of(t).unwrap().0)
                            .expect("夹具的编码单元编号很小")
                    })
                    .collect();
                w.push(&word, &ids, weight)?;
            }
            Ok(())
        },
        &p,
    )
    .expect("编译夹具产物");

    let mem = InMemoryLexicon::from_entries(alphabet(), &entries()).expect("内存词库");
    let table = stele_table::TableLexicon::open_checked(&p, Some(fp)).expect("部署词库");

    compare("夹具", &mem, &table);

    // 不变量：能力声明为真时，前缀查询必须**真的给出东西**。
    // 这正是上一版的缺陷形态：声明 true、实现是空的默认体。
    if Lexicon::supports_prefix(&table) {
        let hits = Probe::prefix_lookup(&table, &code(&["ni"]), true);
        assert!(
            !hits.is_empty(),
            "`supports_prefix()` 为真时 `prefix_lookup` 不能是空的默认实现"
        );
        assert!(
            hits.iter().all(|(_, _, attr)| {
                SpellingAttr::from_bits(*attr).contains(SpellingAttr::COMPLETION)
            }),
            "补全候选必须打上 COMPLETION 位：{hits:?}"
        );
    }

    let _ = std::fs::remove_file(&p);
}

#[test]
fn prefix_lookup_marks_completions_and_excludes_the_exact_code() {
    let mem = InMemoryLexicon::from_entries(alphabet(), &entries()).expect("内存词库");
    let exact = Probe::lookup(&mem, &code(&["ni"]));
    assert_eq!(
        exact,
        vec![("你".to_owned(), Score::from_weight(20_000.0).as_milli_log())]
    );

    let with_exact = Probe::prefix_lookup(&mem, &code(&["ni"]), false);
    assert!(
        with_exact.iter().any(|(t, _, _)| t == "你"),
        "`exclude_exact=false` 时应当含恰好相等的那条：{with_exact:?}"
    );
    let without_exact = Probe::prefix_lookup(&mem, &code(&["ni"]), true);
    assert!(
        !without_exact.iter().any(|(t, _, _)| t == "你"),
        "`exclude_exact=true` 时应当排除恰好相等的那条：{without_exact:?}"
    );
    // 补全出来的是更长的编码。
    assert!(without_exact.iter().any(|(t, _, _)| t == "你好"));
    assert!(without_exact.iter().any(|(t, _, _)| t == "你好吗"));
}
