//! # P0-H 回归：`leading_literal` 假阴性 与 正则回溯爆炸
//!
//! 审计实测的两组事实（原始证据在审计快照里）：
//!
//! **① `leading_literal` 优化错杀合法匹配**
//!
//! | 正则 | 输入 | `Regex` 本身 | Recognizer |
//! | --- | --- | --- | --- |
//! | `^(a|b)+$` | `bbb` | 匹配 | **未认领** |
//! | `^https?://.*$` | `http://x` | 匹配 | **未认领** |
//! | `^a?b$` | `b` | 匹配 | **未认领** |
//!
//! 根因：近似前缀提取把"某一分支/可选项的首字符"当成了必需前缀。
//! 性能优化只能产生**假阳性**（多做一次完整匹配），不能产生假阴性。
//!
//! **② 递归回溯可指数爆炸**
//!
//! `^(a+)+$` 对 `a…ab`：20 个 `a` 约 54 ms，24 个约 848 ms，28 个在审计
//! 的 3 秒超时线内没跑完。代码注释当时假定"只处理短音节"，
//! 但**同一份实现也被 `recognizer` 用**，输入长度由用户决定。
//!
//! 这一组测试把两条都钉死：优化不许漏识别，且回溯必须有硬上限。

use std::time::{Duration, Instant};

use stele_engine::regex::Regex;
use stele_engine::segmentor::{leading_literal, Recognizer};
use stele_engine::spec::{At, RecogPattern, RecognizerSpec};
use stele_engine::tag::TagTable;

/// 一份只含一条模式的识别器。
fn recognizer(name: &str, regex: &str) -> Recognizer {
    let spec = RecognizerSpec {
        patterns: vec![RecogPattern {
            name: name.to_owned(),
            leading: leading_literal(regex),
            trailing: None,
            regex: regex.to_owned(),
            at: At::new(1),
        }],
        import_preset: None,
    };
    Recognizer::new(&spec, &mut TagTable::new()).expect("模式应当能编译")
}

// ─────────────────────────────────────────────────────────────────────────────
// ① 假阴性
// ─────────────────────────────────────────────────────────────────────────────

/// 审计的三行表格，逐行断言"正则匹配 ⇒ 识别器必须认领"。
#[test]
fn the_audited_false_negatives_are_fixed() {
    let cases: &[(&str, &str)] = &[
        ("^(a|b)+$", "bbb"),
        ("^https?://.*$", "http://x"),
        ("^a?b$", "b"),
        // 同族的更多形状：可选项、交替、字符类开头。
        ("^(x|y)z$", "yz"),
        ("^a*b$", "b"),
        ("^a{0,2}b$", "b"),
        ("^[ab]+$", "b"),
        ("^\\d?x$", "x"),
    ];
    for (pattern, input) in cases {
        let re = Regex::compile(pattern).unwrap_or_else(|e| panic!("`{pattern}` 应当能编译：{e}"));
        assert!(
            re.is_full_match(input),
            "前提：`{pattern}` 应当匹配 `{input}`"
        );

        let rec = recognizer("t", pattern);
        let scan = rec.scan(input);
        assert!(
            !scan.claims.is_empty(),
            "`{pattern}` 匹配 `{input}`，但识别器**没有认领**——\
             这正是 `leading_literal` 的假阴性形态（自己算出 leading=`{}`）",
            leading_literal(pattern)
        );
    }
}

/// 更一般的不变式：任何输入，只要正则从位置 0 匹配上，识别器就必须认领。
///
/// 这比"审计那三行"更强：它约束的是**优化不许改变结论**。
#[test]
fn the_optimisation_never_changes_the_verdict() {
    let patterns = [
        "^uU[a-z]+$",
        "^https?://.*$",
        "^(a|b)+$",
        "^a?b$",
        "^v([0-9]|10)$",
        "^;.*;$",
        "^\\d+$",
        "^[A-Za-z]+$",
        "^a{2,3}b$",
        "^x*y$",
        "^\\w+$",
        "^.$",
    ];
    let inputs = [
        "",
        "a",
        "b",
        "aa",
        "ab",
        "bbb",
        "c",
        "uUni",
        "uU",
        "http://x",
        "https://x",
        "v1",
        "v10",
        "v11",
        ";abc;",
        ";abc",
        "123",
        "1a",
        "xy",
        "yy",
        "aab",
        "aaab",
        "aaaa",
        "z",
    ];
    for pattern in patterns {
        let Ok(re) = Regex::compile(pattern) else {
            continue;
        };
        let rec = recognizer("p", pattern);
        let to_end = pattern.ends_with('$') && !pattern.ends_with("\\$");
        for input in inputs {
            let re_hit = re.match_prefix_len(input, to_end).is_some();
            let recognised = !rec.scan(input).claims.is_empty();
            assert_eq!(
                re_hit,
                recognised,
                "`{pattern}` 对 `{input}`：正则在位置 0 的结论是 {re_hit}，\
                 而识别器给的是 {recognised}（leading=`{}`）——优化改变了结论",
                leading_literal(pattern)
            );
        }
    }
}

/// 必需前缀的取值必须**保守**：只能是可证明的公共前缀。
#[test]
fn required_prefix_is_conservative() {
    let cases = [
        ("^abc$", "abc"),
        ("^uU[a-z]+$", "uU"),
        ("^v([0-9]|10)$", "v"),
        ("^https?://.*$", "http"),
        // 这三条**不能**取到任何字面前缀。
        ("^(a|b)+$", ""),
        ("^a?b$", ""),
        ("^([nl])ue$", ""),
        ("^\\d+$", ""),
        ("^[ab]+$", ""),
        ("^.*$", ""),
    ];
    for (pattern, want) in cases {
        let re = Regex::compile(pattern).unwrap();
        assert_eq!(
            re.required_prefix(),
            want,
            "`{pattern}` 的必需前缀取错了——多取一个字就是假阴性"
        );
        assert_eq!(leading_literal(pattern), want);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ② 回溯爆炸
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn nested_repeats_are_rejected_at_compile_time() {
    for pattern in ["^(a+)+$", "^(a*)*$", "^(ab*)*$", "^(a?){2,}$", "x(a+)+y"] {
        let e = Regex::compile(pattern)
            .err()
            .unwrap_or_else(|| panic!("`{pattern}` 是重复套重复，必须编译期拒绝"));
        let msg = e.to_string();
        assert!(
            msg.contains("重复套重复") && msg.contains("指数"),
            "诊断必须说清楚为什么：{msg}"
        );
    }
    // **平铺的**量词不能被误伤——真实方案的规则长这样。
    for pattern in ["^([a-z]{2}).+$", "^uU[a-z]+$", "^a{2,3}b$", "^x*y$"] {
        assert!(
            Regex::compile(pattern).is_ok(),
            "`{pattern}` 不含嵌套重复，不该被拒绝"
        );
    }
}

#[test]
fn catastrophic_backtracking_is_bounded_by_the_step_budget() {
    // 不能用 `(a+)+` 来测了（它已经被编译期拒绝），于是换成
    // **非嵌套但组合数极大**的模式：十个 `a*` 对 40 个 a 且末尾不匹配，
    // 组合数 C(49,10) ≈ 8e9——没有预算就是"永远跑不完"。
    let pattern = "^a*a*a*a*a*a*a*a*a*a*b$";
    let re = Regex::compile(pattern).expect("平铺量词应当能编译");
    let input = "a".repeat(40);

    let t0 = Instant::now();
    let hit = re.match_prefix_len(&input, true);
    let dt = t0.elapsed();
    assert!(hit.is_none(), "末尾没有 b，不该匹配");
    assert!(
        dt < Duration::from_millis(500),
        "回溯必须被预算限住，实测 {dt:?}——没有上限就会跑到分钟级"
    );

    // 预算调小仍要能工作（只是更早放弃）。
    let tiny = re.clone().with_step_budget(1_000);
    let t0 = Instant::now();
    let _ = tiny.match_prefix_len(&input, true);
    assert!(t0.elapsed() < Duration::from_millis(100));
}

#[test]
fn recursive_depth_is_capped_for_long_inputs() {
    // 深度上限的意义：不因为一条超长输入把栈打穿。
    let re = Regex::compile("^a+$").unwrap();
    let input = "a".repeat(200_000);
    // 只要求不 panic / 不栈溢出；是否匹配不重要。
    let _ = re.match_prefix_len(&input, true);
    let _ = re.is_full_match(&input);
}

#[test]
fn a_normal_recognizer_still_works_after_the_budget_was_added() {
    // 预算不能把正常识别的结论改掉。
    let rec = recognizer("url", "^https?://.*$");
    assert!(!rec.scan("http://x").claims.is_empty());
    assert!(!rec.scan("https://example.com/a?b=1").claims.is_empty());
    assert!(rec.scan("ftp://x").claims.is_empty());

    let rec = recognizer("unicode", "^uU[a-f0-9]+$");
    assert!(!rec.scan("uUabc").claims.is_empty());
    assert!(!rec.scan("uU1f").claims.is_empty());
    assert!(rec.scan("uUni").claims.is_empty(), "'n' 不在 [a-f0-9] 里");
    assert!(rec.scan("ni").claims.is_empty());
}
