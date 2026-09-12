//! 集成测试：从**公开 API** 出发验证四条工程铁律。
//!
//! 与单元测试的区别：这些测试只使用 `stele_core` 对外导出的东西，
//! 因此它们同时也在检验**公开接口是否够用**——如果某个不变式在公开 API 上
//! 无法表达或无法验证，那本身就是接口设计的缺陷。

use stele_core::{
    clamp_bonus, has_cross_class_inversion, is_exact, literal_candidate, sort_candidates,
    Candidate, Lane, Origin, Score, Span, SpellingAttr,
};

fn cand(text: &str, milli_log: i32, origin: Origin, lane: Lane) -> Candidate {
    Candidate {
        text: text.to_owned(),
        comment: None,
        score: Score::from_milli_log(milli_log),
        origin,
        attr: SpellingAttr::NORMAL,
        span: Span::new(0, 1),
        lane,
    }
}

/// 铁律 1（可复现）：候选列表是 (输入, 状态) 的纯函数。
///
/// 这条测试的意义：词库里同权重的词极多，若排序依赖 `HashMap` 的遍历顺序，
/// 同一个输入每次运行的结果就可能不同——而用户会立刻察觉"候选顺序会跳"。
#[test]
fn ordering_is_a_pure_function_of_the_candidate_set() {
    let build = || {
        let mut v: Vec<Candidate> = (0..200)
            .map(|i| {
                // 故意让大量候选同分，逼出平局规则。
                let origin = match i % 4 {
                    0 => Origin::UserWord,
                    1 => Origin::SystemWord,
                    2 => Origin::Sentence,
                    _ => Origin::Literal,
                };
                cand(&format!("c{i:03}"), 1000, origin, Lane::Input)
            })
            .collect();
        // 打乱一次插入顺序，确认"插入顺序"确实是规则的一部分。
        v.swap(0, 199);
        v
    };

    let mut reference: Option<Vec<(String, i32, u8)>> = None;
    for run in 0..100 {
        let mut v = build();
        sort_candidates(&mut v);
        let snapshot: Vec<(String, i32, u8)> = v
            .iter()
            .map(|c| {
                (
                    c.text.clone(),
                    c.score.as_milli_log(),
                    // Origin 不是 Copy 语义上的稳定排序键，这里用序号代替。
                    match c.origin {
                        Origin::UserWord => 0,
                        Origin::SystemWord => 1,
                        Origin::Literal => 2,
                        Origin::Sentence => 3,
                        _ => 9,
                    },
                )
            })
            .collect();
        match &reference {
            None => reference = Some(snapshot),
            Some(r) => assert_eq!(r, &snapshot, "第 {run} 次运行的排序结果与首次不同"),
        }
    }
}

/// 铁律 1 的推论：分数必须在**值域内**饱和，否则会静默压过一切。
#[test]
fn scores_stay_within_their_domain() {
    let mut s = Score::ZERO;
    for _ in 0..100 {
        s = s.saturating_add(Score::from_milli_log(10_000));
    }
    assert_eq!(s, Score::CEIL, "加法必须在值域上界饱和");

    let mut f = Score::ZERO;
    for _ in 0..100 {
        f = f.saturating_add(Score::from_milli_log(-10_000));
    }
    assert_eq!(f, Score::FLOOR, "加法必须在值域下界饱和");

    // NaN / 非正权重一律落到下界——绝不让 NaN 进入排序。
    assert_eq!(Score::from_weight(f64::NAN), Score::FLOOR);
    assert_eq!(Score::from_weight(0.0), Score::FLOOR);
    assert_eq!(Score::from_weight(-1.0), Score::FLOOR);
}

/// 铁律 4（精确优先）：它约束的是**重排器造成的改变**，
/// 而不是要求最终列表按来源分区。
#[test]
fn precision_rule_constrains_the_ranker_not_the_base_order() {
    // 情形 A：高分的造句候选本来就该排在一个低分精确匹配词之前。
    //        —— 要求分区会损害质量，因此这**不算**倒置。
    let base = vec![
        cand("rare-exact", 100, Origin::SystemWord, Lane::Input),
        cand("common-sentence", 900, Origin::Sentence, Lane::Input),
    ];
    assert!(!has_cross_class_inversion(&base, &base, Lane::Input));

    // 情形 B：重排器把猜测候选顶到了**本来在前**的精确匹配之前 —— 这是倒置。
    let before = vec![
        cand("exact", 900, Origin::SystemWord, Lane::Input),
        cand("guessed", 100, Origin::Sentence, Lane::Input),
    ];
    let after = vec![
        cand("guessed", 90_000, Origin::Sentence, Lane::Input),
        cand("exact", 900, Origin::SystemWord, Lane::Input),
    ];
    assert!(has_cross_class_inversion(&before, &after, Lane::Input));
}

/// 铁律 4 的**能力边界**：加成上界只能"限制移动幅度"，**不能单独保证不倒置**。
///
/// 这一条是写测试时发现的设计文档错误。结论是：
///
/// - 上界 `L` 保证：猜测候选最多升 `L`。
/// - 因此**只有当基础分差距 > L 时**，倒置才在算术上不可能。
/// - 差距小于 `L` 时，倒置仍然可能发生 —— 这时要靠**回退式单调守卫**兜底。
///
/// 想靠算术完全消除倒置，只有"给非猜测候选一个类别偏置"一途，
/// 而那正是我们否决掉的分区方案（它会损害排序质量）。**所以守卫不是可选项。**
#[test]
fn bonus_limit_bounds_movement_but_is_not_by_itself_a_guarantee() {
    let limit = Score::from_milli_log(2_000); // 重排器最多加 2.0 个 ln 单位
    let guessed = Score::from_milli_log(100);

    let ranked = clamp_bonus(guessed, Score::from_milli_log(99_999), limit);
    assert_eq!(ranked, Score::from_milli_log(2_100), "加成被钳在上界内");

    // 差距大于上界 → 倒置不可能。
    let far_exact = Score::from_milli_log(5_000);
    assert!(ranked < far_exact, "基础分差距大于上界时，倒置不可能");

    // 差距小于上界 → 倒置仍然可能，端赖守卫。
    let near_exact = Score::from_milli_log(1_500);
    assert!(
        ranked > near_exact,
        "基础分差距小于上界时，上界挡不住倒置 —— 这正是需要单调守卫的原因"
    );
}

/// 铁律 2（候选封闭）+ G4：原样上屏的候选一定存在，且一定是"非猜测"。
#[test]
fn literal_fallback_is_always_available_and_never_guessed() {
    let c = literal_candidate("nihao", Span::new(0, 5));
    assert_eq!(c.text, "nihao");
    assert_eq!(c.origin, Origin::Literal);
    assert_eq!(c.attr, SpellingAttr::NORMAL);
    assert!(
        is_exact(c.origin, c.attr),
        "原样上屏是「输入本身就是答案」，绝不能被当成猜测"
    );
    // 分低，但一定在。
    assert_eq!(c.score, Score::FLOOR);
}

/// G9：拼写属性是**可叠加的位集**，而来源是单值——两个轴互不干扰。
#[test]
fn spelling_attrs_stack_while_origin_stays_single() {
    // 一条边同时是模糊音和简拼：单选题表达不了，位集可以。
    let both = SpellingAttr::FUZZY | SpellingAttr::ABBREV;
    assert!(both.contains(SpellingAttr::FUZZY));
    assert!(both.contains(SpellingAttr::ABBREV));

    // 词库里真有这个词（SystemWord），但编码是派生的 —— 用户看到的是"猜的"。
    assert!(!is_exact(Origin::SystemWord, both));
    // 同一个来源，规范拼写则是精确的。
    assert!(is_exact(Origin::SystemWord, SpellingAttr::NORMAL));
}

/// D23：两条通道的规则不同——预测候选不参与"精确优先"，也不参与盲选排序。
#[test]
fn lanes_are_ordered_but_governed_by_different_rules() {
    let mut v = vec![
        cand("prediction", 99_999, Origin::Sentence, Lane::Predict),
        cand("input-low", -99_999, Origin::SystemWord, Lane::Input),
    ];
    sort_candidates(&mut v);
    assert_eq!(v[0].lane, Lane::Input, "输入通道永远排在预测通道之前");

    // 预测通道内部不看 Origin 优先级。
    let mut p = vec![
        cand("a", 500, Origin::UserWord, Lane::Predict),
        cand("b", 500, Origin::Sentence, Lane::Predict),
    ];
    sort_candidates(&mut p);
    assert_eq!(p[0].text, "a", "预测通道平局时按插入顺序");
}
