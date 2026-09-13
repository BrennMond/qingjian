//! **与上游实现的逐字节对照**：`calc_translator` 的表达式求值。
//!
//! # 为什么要这样测
//!
//! 上游的 `calc_translator.lua` 把 **Lua 解释器**当计算器用
//! （`pcall(load('return ' .. code, 'calculate', 't', calcPlugin))`）。
//! 我们没有解释器，因此自己写了一个表达式求值器——那意味着
//! **每一个运算符的语义都要被验证**：优先级、结合性、取模的符号、
//! 阶乘的定义域、以及**数字转字符串的格式**。
//!
//! 这份测试拿上游当 oracle，逐条比对结果文本。
//!
//! ```bash
//! # 想重新生成对照数据（需要 luajit）：
//! luajit tools/oracle/calc_translator/calc.lua \
//!   > tools/oracle/calc_translator/calc.expected.txt
//! ```
//!
//! # 已知的、**有意**的不一致
//!
//! `frexp` 上游返回**字符串**（`"m * 2^e"`），我们返回数值。
//! 因此它不在本对照里——把它塞进来只会得到一个"两边不一样"的
//! 假失败，而差别是**我们说清楚了的**。

use stele_engine::calc::CalcTranslator;
use stele_engine::spec::{At, CalcSpec};
use stele_engine::tag::TagTable;

/// 一条对照记录。
struct Case {
    /// 输入表达式。
    input: String,
    /// 上游能否求值成功。
    ok: bool,
    /// 成功时的结果文本。
    value: String,
}

fn load_oracle() -> Vec<Case> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/oracle/calc_translator/calc.expected.txt");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读不到对照数据 {}：{e}", path.display()));
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let mut it = line.split('\t');
            let input = it.next().unwrap_or_default().to_owned();
            let status = it.next().unwrap_or_default().to_owned();
            let value = it.next().unwrap_or_default().to_owned();
            Case {
                input,
                ok: status == "OK",
                value,
            }
        })
        .collect()
}

fn translator() -> CalcTranslator {
    let mut tags = TagTable::new();
    CalcTranslator::new(
        &CalcSpec {
            prefix: "cC".into(),
            show_prefix: false,
            at: At::default(),
        },
        vec![tags.intern("calculator")],
    )
}

#[test]
fn the_oracle_is_present_and_has_enough_cases() {
    // 防"存档被删后测试永远通过"。
    let cases = load_oracle();
    assert!(
        cases.len() >= 60,
        "对照数据太少了（{} 条）——它是不是被截断了？",
        cases.len()
    );
    assert!(
        cases.iter().any(|c| c.ok),
        "对照数据里没有任何成功用例"
    );
    assert!(
        cases.iter().any(|c| !c.ok),
        "对照数据里没有任何失败用例 —— 失败路径也要比对"
    );
}

#[test]
fn evaluation_matches_the_upstream_converter() {
    let t = translator();
    let mut failures = Vec::new();
    for case in load_oracle() {
        let got = t.evaluate(&case.input);
        match (case.ok, got) {
            (true, Ok((value, _))) => {
                if value != case.value {
                    failures.push(format!(
                        "{}：期望 {:?}，实际 {:?}",
                        case.input, case.value, value
                    ));
                }
            }
            (true, Err(e)) => {
                failures.push(format!("{}：上游成功（{:?}），我们失败（{e}）",
                    case.input, case.value));
            }
            (false, Ok((value, _))) => {
                failures.push(format!("{}：上游失败，我们得到 {:?}", case.input, value));
            }
            // 两边都失败：**一致**（错误文案不必相同）。
            (false, Err(_)) => {}
        }
    }
    assert!(
        failures.is_empty(),
        "与上游不一致（{} 条）：\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn the_result_vector_has_two_candidates_in_the_documented_order() {
    use stele_core::{CandidateSink, Context, Options, Query, Span, Translator};
    let t = translator();
    let opts = Options::new();
    let ctx = Context::default();
    let input = "cC1+2";
    let q = Query {
        input,
        caret: input.len(),
        options: &opts,
        context: &ctx,
        segment_text: input,
    };
    let mut buf = Vec::new();
    let mut sink = CandidateSink::new(&mut buf, 8);
    t.translate(&q, Span::new(0, input.len()), &mut sink);
    assert_eq!(buf.len(), 2);
    assert_eq!(buf[0].text, "3", "第一条是结果本身");
    assert_eq!(buf[1].text, "1+2=3", "第二条是「原式=结果」");
    assert_eq!(buf[0].kind, stele_core::CandidateKind::Inline);
}

#[test]
fn a_failing_expression_still_yields_two_labelled_candidates() {
    use stele_core::{CandidateSink, Context, Options, Query, Span, Translator};
    let t = translator();
    let opts = Options::new();
    let ctx = Context::default();
    let input = "cC1+";
    let q = Query {
        input,
        caret: input.len(),
        options: &opts,
        context: &ctx,
        segment_text: input,
    };
    let mut buf = Vec::new();
    let mut sink = CandidateSink::new(&mut buf, 8);
    t.translate(&q, Span::new(0, input.len()), &mut sink);
    assert_eq!(buf.len(), 2, "失败也是两条（照上游）");
    assert_eq!(buf[0].comment.as_deref(), Some("解析失败"));
    assert_eq!(buf[1].comment.as_deref(), Some("入参"));
}

#[test]
fn only_the_configured_prefix_triggers_it() {
    use stele_core::{CandidateSink, Context, Options, Query, Span, Translator};
    let t = translator();
    let opts = Options::new();
    let ctx = Context::default();
    for input in ["1+1", "cc1+1", "C1+1"] {
        let q = Query {
            input,
            caret: input.len(),
            options: &opts,
            context: &ctx,
            segment_text: input,
        };
        let mut buf = Vec::new();
        let mut sink = CandidateSink::new(&mut buf, 8);
        t.translate(&q, Span::new(0, input.len()), &mut sink);
        assert!(buf.is_empty(), "{input} 不该触发");
    }
}
