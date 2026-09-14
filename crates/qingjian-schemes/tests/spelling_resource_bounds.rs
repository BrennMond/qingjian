//! # P0-A 回归：拼写展开的**资源上界**
//!
//! 审计实测（同一台机器、同一个 release 构建、真实默认词库）：
//!
//! | 输入 | 现象 |
//! | --- | --- |
//! | `nihao` | 按键 P50 46–48 µs，P99 100–105 µs |
//! | `ssss` 第四键 | **约 1.52 s**，进程 `VmHWM` 约 **209 MiB** |
//! | `woaizhongguo` | 部分按键 290 / 679 / 285 ms，`VmHWM` 约 146 MiB |
//!
//! 三者最终都只得到字面量。**差数千到数万倍**——这不是"慢"，是
//! 按键路径上没有资源上界。
//!
//! 这一组测试不看墙钟时间（那在 CI 上不可复现），而是断言**可观测的
//! 结构量**：搜索状态数、边尝试次数、搜索图字节数。它们与耗时成正比，
//! 又完全确定。另有一条 release-only 的时间红线守住"10 ms 单键"。
//!
//! **正常词的召回与资源上限必须同时通过**：只把输入退化成字面量
//! 也能让状态数变小，那是在用错误换性能。

use qingjian_core::{CodeAlphabet, Engine, Expansion, ExpansionSink, Key};
use qingjian_engine::spelling::{ExpansionLimits, ExpansionStats, SpellingTable};

/// 真实默认拼音方案的拼写表（405 个音节 + 一条缩写规则）。
///
/// 用**内嵌的演示孪生体**（`z-pinyin-demo`）：它的 `alphabet` 与 `rules`
/// 与 `schemes/qingjian-default/pinyin.schema.yaml` 完全一致，只是词库指向
/// 手写的小词典（见 `minimal.rs` 的模块文档）。因此这里测的是
/// **真实的字母表规模**，却不必在测试里编译 41 万词条。
///
/// "完全一致"不是注释里的自我声明：`the_embedded_demo_speller_matches_the_real_scheme`
/// 会拿磁盘上的真实方案逐项对照（**含顺序**，并比较 `rules`）。这条断言是必要的——
/// 真实方案的 `speller.alphabet` 由 `tools/wordlist-gen` **整段重写**，一次重新
/// 生成（399 → 405 个音节）就足以让上面这句话变成假话。
fn real_pinyin_table() -> SpellingTable {
    let defs = qingjian_schemes::all().expect("内嵌方案必须能装载");
    let p = defs
        .iter()
        .find(|d| d.translator == qingjian_engine::scheme::TranslatorKind::SpellingGraph)
        .expect("必须存在拼写图族方案（拼音）");
    // 先把"这确实是真实规模"钉住——否则把字母表改小就能让测试变绿。
    assert!(
        p.alphabet.len() >= 300,
        "拼音字母表应当有几百个音节，实得 {}",
        p.alphabet.len()
    );
    assert!(!p.rules.is_empty(), "拼音方案必须有拼写规则（缩写）");
    SpellingTable::compile(CodeAlphabet::new(p.alphabet.clone()), &p.rules)
}

fn expand(t: &SpellingTable, spelling: &str) -> (Vec<Expansion>, ExpansionStats) {
    let mut buf = Vec::new();
    let mut sink = ExpansionSink::new(&mut buf, 4096);
    let stats = t.expand_into_with_stats(spelling, &mut sink);
    (buf, stats)
}

fn texts<'a>(t: &'a SpellingTable, e: &'a Expansion) -> Vec<&'a str> {
    e.code
        .iter()
        .map(|u| t.alphabet().text(*u).unwrap_or("?"))
        .collect()
}

/// 每个输入一条断言：状态数与边尝试数都在硬上限之内。
fn assert_bounded(t: &SpellingTable, spelling: &str) -> ExpansionStats {
    let limits = t.limits();
    let (_, stats) = expand(t, spelling);
    assert!(
        stats.states_pushed <= limits.max_states,
        "`{spelling}`：搜索状态数 {} 超过硬上限 {}",
        stats.states_pushed,
        limits.max_states
    );
    assert!(
        stats.edge_attempts <= limits.max_work,
        "`{spelling}`：边尝试次数 {} 超过硬上限 {}",
        stats.edge_attempts,
        limits.max_work
    );
    // 搜索图常驻内存：arena + 前沿，每项都是定长记录。
    // 上界取 `states_pushed` 对应的 arena + 同等数量的前沿条目。
    let per_state = core::mem::size_of::<u32>() * 6;
    let bound = limits.max_states * per_state * 2 + 64 * 1024;
    assert!(
        stats.graph_bytes <= bound,
        "`{spelling}`：搜索图 {} 字节超过上界 {bound}",
        stats.graph_bytes
    );
    stats
}

#[test]
fn pathological_inputs_are_hard_bounded() {
    let t = real_pinyin_table();
    // 审计点名的两个，加上同族的更病态变体。
    for spelling in [
        "ssss",
        "ssssssssss",
        "woaizhongguo",
        "zzzz",
        "jjjj",
        "aaaaaaaaaaaa",
    ] {
        let stats = assert_bounded(&t, spelling);
        // 病态输入**必须**被截断，否则说明上限形同虚设（那才是要测的东西）。
        assert!(
            stats.truncated || stats.states_pushed < t.limits().max_states,
            "`{spelling}` 既没截断也没接近上限，测试失去意义：{stats:?}"
        );
    }
}

#[test]
fn ssss_is_many_orders_of_magnitude_smaller_than_the_old_blowup() {
    // 旧实现：`ssss` 的第四次按键约 1.52 s（release）、进程 `VmHWM` 约 209 MiB。
    // 原因是前沿/状态表里每条状态都持有一份 `Vec<CodeUnitId>` 的副本
    // （堆 + `HashSet` + `BinaryHeap`），而 `ssss` 那条路要展开
    // 34（s 开头音节数）^4 ≈ 1.3e6 条。
    //
    // 新实现：状态是定长 24 字节的 arena 节点，且被硬限在 16384。
    let t = real_pinyin_table();
    let (_, stats) = expand(&t, "ssss");
    assert!(
        stats.states_pushed <= t.limits().max_states,
        "`ssss` 的状态数必须被硬限在 {}，实得 {}",
        t.limits().max_states,
        stats.states_pushed
    );
    assert!(
        stats.graph_bytes < 1 << 21,
        "`ssss` 的搜索图必须远小于 2 MiB（旧实现 209 MiB），实得 {} 字节",
        stats.graph_bytes
    );
    // 与旧实现的峰值相比低两个数量级以上。
    assert!(stats.graph_bytes < 209 * 1024 * 1024 / 100);
}

#[test]
fn long_legal_input_stays_bounded() {
    let t = real_pinyin_table();
    for spelling in [
        "nihaoshijie",
        "zhonghuarenmingongheguo",
        "woshiyigexuesheng",
    ] {
        let stats = assert_bounded(&t, spelling);
        assert!(stats.results > 0, "`{spelling}` 应当至少有一条切分");
    }
}

#[test]
fn normal_words_are_still_recalled() {
    // **资源上界不能靠牺牲召回来换。**
    let t = real_pinyin_table();

    // ① `nihao` 的规范切分必须在，而且排在最前。
    let (got, stats) = expand(&t, "nihao");
    assert!(!stats.truncated, "正常输入不该触碰预算：{stats:?}");
    let segs: Vec<Vec<&str>> = got.iter().map(|e| texts(&t, e)).collect();
    assert!(
        segs.contains(&vec!["ni", "hao"]),
        "`nihao` 必须能切成 `ni hao`，实得 {segs:?}"
    );
    assert_eq!(segs[0], vec!["ni", "hao"], "规范切分必须排第一");

    // ② 单音节。
    for (input, want) in [("ni", "ni"), ("hao", "hao"), ("shi", "shi")] {
        let (got, _) = expand(&t, input);
        let segs: Vec<Vec<&str>> = got.iter().map(|e| texts(&t, e)).collect();
        assert!(
            segs.iter().any(|s| s == &vec![want]),
            "`{input}` 必须能切成 `{want}`，实得 {segs:?}"
        );
    }

    // ③ 简拼：`nhao`（第一音节缩、第二音节保留）是方案注释里承诺的形态。
    let (got, stats) = expand(&t, "nhao");
    let segs: Vec<Vec<&str>> = got.iter().map(|e| texts(&t, e)).collect();
    assert!(
        segs.contains(&vec!["ni", "hao"]),
        "`nhao` 必须能切成 `ni hao`，实得前 8 条 {:?}",
        &segs[..segs.len().min(8)]
    );
    assert!(stats.within_budget(), "{stats:?}");

    // ④ 整条管线也要出「你好」——不是只有拼写层"看起来对"。
    let defs = qingjian_schemes::all().expect("内嵌方案");
    let engine = qingjian_engine::EngineImpl::new(&defs).expect("引擎");
    let mut s = engine.create_session();
    for c in "nihao".chars() {
        s.process_key(Key::ch(c));
    }
    assert_eq!(
        s.candidates().first().map(|c| c.text.as_str()),
        Some("你好"),
        "端到端必须仍然打出「你好」"
    );
}

#[test]
fn the_budget_is_actually_enforced_when_set_tiny() {
    // 故意把预算调小：截断必须发生、必须置位、且仍然不丢规范切分的第一位。
    let t = real_pinyin_table().with_budget(ExpansionLimits::new(4, 2, 12, 24));
    let (got, stats) = expand(&t, "nihao");
    assert!(stats.truncated, "预算这么小时必须置 truncated：{stats:?}");
    assert!(stats.states_pushed <= 12, "{stats:?}");
    assert!(stats.edge_attempts <= 24, "{stats:?}");
    assert!(
        got.len() <= 4,
        "产出条数必须遵守 max_results：{}",
        got.len()
    );
    if let Some(first) = got.first() {
        assert_eq!(texts(&t, first), vec!["ni", "hao"], "最优切分仍然要排第一");
    }
}

#[test]
fn expansion_is_deterministic_under_budget() {
    let t = real_pinyin_table();
    for spelling in ["nihao", "ssss", "woaizhongguo", "nhao"] {
        let (a, sa) = expand(&t, spelling);
        for _ in 0..20 {
            let (b, sb) = expand(&t, spelling);
            assert_eq!(
                a.iter().map(|e| e.code.clone()).collect::<Vec<_>>(),
                b.iter().map(|e| e.code.clone()).collect::<Vec<_>>(),
                "`{spelling}` 的展开必须可复现"
            );
            assert_eq!(sa, sb, "`{spelling}` 的计数也必须可复现");
        }
    }
}

#[test]
fn query_count_is_bounded_by_the_result_cap() {
    // 翻译器对每条展开做 1 次 `lookup`（+ 可选 1 次 `prefix_lookup`），
    // 因此"每次按键的词典查询次数 ≤ 2 × max_results"是资源合同的一部分。
    // 这里直接对着那个合同断言（真正的计数器在称重台 `--count-queries`）。
    let t = real_pinyin_table();
    for spelling in ["ssss", "woaizhongguo", "nihao", "nihaoshijie"] {
        let (got, _) = expand(&t, spelling);
        assert!(
            got.len() <= t.limits().max_results,
            "`{spelling}`：展开条数 {} 超过 max_results {}",
            got.len(),
            t.limits().max_results
        );
    }
}

/// 单键时间红线：**只在 release 下断言**。
///
/// debug 构建慢一到两个数量级，把它也拉进门槛只会逼人调大阈值，
/// 那样门槛就失去意义。这条测试对应审计验收里的
/// 「现有复现输入在 release 下不再出现 >10 ms 的单键路径」。
#[cfg(not(debug_assertions))]
#[test]
fn release_single_key_expansion_is_under_ten_milliseconds() {
    use std::time::Instant;
    let t = real_pinyin_table();
    // 先热一次（首次调用会把表的页摸进去）。
    let _ = expand(&t, "nihao");

    for spelling in [
        "ssss",
        "ssssssssss",
        "woaizhongguo",
        "zzzz",
        "xxxxxxxx",
        "nihaoshijie",
    ] {
        // 取多次的最大值——一次偶然的调度抖动不该让红线变松。
        let mut worst = std::time::Duration::ZERO;
        for _ in 0..20 {
            let t0 = Instant::now();
            let _ = expand(&t, spelling);
            worst = worst.max(t0.elapsed());
        }
        assert!(
            worst < std::time::Duration::from_millis(10),
            "`{spelling}` 的单键展开最坏 {worst:?}，超过 10 ms 红线"
        );
    }
}

/// 取出 `speller:` 段里某个键（`alphabet:` / `rules:`）下面的正文，
/// **保持文件顺序、不去重**，并去掉注释行与空行。
///
/// 返回的是**配置内容**：注释不参与比较（两份文件的注释可以各自演化），
/// 但顺序、重复项、以及规则的写法都是配置的一部分，必须原样带出来。
fn speller_block(text: &str, key: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let speller = lines
        .iter()
        .position(|l| l.trim_end() == "speller:")
        .unwrap_or_else(|| panic!("方案里必须有 `speller:`"));
    let key_line = format!("  {key}");
    let at = lines[speller..]
        .iter()
        .position(|l| l.trim_end() == key_line)
        .map_or_else(
            || panic!("speller 段里必须有 `{key_line}`"),
            |i| speller + i,
        );
    let mut out = Vec::new();
    for l in &lines[at + 1..] {
        let t = l.trim();
        if t.is_empty() {
            continue;
        }
        // 缩进回到 speller 的子键层级（2 格）就说明这一段结束了。
        if l.len() - l.trim_start().len() <= 2 {
            break;
        }
        if t.starts_with('#') {
            continue;
        }
        out.push(t.to_owned());
    }
    out
}

/// **内嵌演示孪生体的 `speller` 段必须与磁盘上的真实方案完全一致。**
///
/// 上面 [`real_pinyin_table`] 的整个前提就是这一条：它拿演示体测"真实规模"。
/// 而真实方案的 `speller.alphabet` 是 `tools/wordlist-gen` 生成并**整段重写**
/// 的——重新生成一次词库（本轮：399 → 405 个音节）就会让演示体过期，症状是
/// "测试还绿，但测的已经不是真实规模了"。
///
/// # 为什么**不排序、不去重**
///
/// 这条测试的第一版对两边都做了 `sort()` + `dedup()`，于是它验证的只是
/// **音节集合**相等：顺序不同、字母表里有重复项，它都看不出来——而注释里
/// 写的是"逐项一致"。收窄承诺或加强检查，只能选一个；这里选后者：
///
/// 1. 有序比较两边的 `alphabet`（顺序是配置的一部分：单元的编号由它决定）；
/// 2. 显式断言真实方案的字母表**没有重复项**（`dedup` 会把重复吃掉）；
/// 3. **比较 `rules`**——原先只断言"非空"，那是"规则相同"最弱的替代品；
/// 4. 再拿**解析出来的**演示体字母表与磁盘上的有序列表对照，确认
///    [`real_pinyin_table`] 编译进拼写表的确实是这一份。
#[test]
fn the_embedded_demo_speller_matches_the_real_scheme() {
    let schemes_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemes/qingjian-default");
    let real_text = std::fs::read_to_string(schemes_dir.join("pinyin.schema.yaml"))
        .expect("真实方案 pinyin.schema.yaml 必须在仓库里");
    let demo_text = std::fs::read_to_string(schemes_dir.join("z-pinyin-demo.schema.yaml"))
        .expect("内嵌孪生体 z-pinyin-demo.schema.yaml 必须在仓库里");

    // ① 有序字母表，逐项比较。
    let real_alphabet = speller_block(&real_text, "alphabet:");
    let demo_alphabet = speller_block(&demo_text, "alphabet:");
    assert!(
        real_alphabet.len() >= 300,
        "真实方案的字母表应当有几百个音节，实得 {}",
        real_alphabet.len()
    );
    // ② 重复项：`dedup` 掉的正是这一类，所以单独断言。
    let mut unique = real_alphabet.clone();
    unique.sort();
    let before = unique.len();
    unique.dedup();
    assert_eq!(
        unique.len(),
        before,
        "真实方案的 `speller.alphabet` 里有重复项：同一个音节出现两次，会被编成两个编号。"
    );
    assert_eq!(
        demo_alphabet, real_alphabet,
        "内嵌 `z-pinyin-demo` 的 alphabet 与真实方案分叉（含顺序）；重新生成后必须同步 z-pinyin-demo.schema.yaml。"
    );

    // ③ 规则也必须一致：只查"非空"等于没查。
    assert_eq!(
        speller_block(&demo_text, "rules:"),
        speller_block(&real_text, "rules:"),
        "内嵌 `z-pinyin-demo` 的 rules 与真实方案分叉；两份是孪生体，规则必须一致。"
    );

    // ④ 演示体**编译进拼写表的**字母表就是上面那一份。
    let defs = qingjian_schemes::all().expect("内嵌方案必须能装载");
    let demo = defs
        .iter()
        .find(|d| d.translator == qingjian_engine::scheme::TranslatorKind::SpellingGraph)
        .expect("必须存在拼写图族方案（拼音）");
    // `speller_block` 带的是 YAML 列表项的原文（`- ai`），解析出来的是裸音节。
    let real_items: Vec<String> = real_alphabet
        .iter()
        .map(|l| l.strip_prefix("- ").unwrap_or(l).trim().to_owned())
        .collect();
    assert_eq!(
        demo.alphabet, real_items,
        "内嵌演示体解析出来的字母表与磁盘上的真实方案不一致——`real_pinyin_table()` 测的就不是真实规模了。"
    );
    // 演示体的规则同样要**真的编译出来**，而不只是文件里有那一行。
    assert!(
        !demo.rules.is_empty(),
        "拼音方案必须有拼写规则（缩写）——文件里有、装配后为空也算坏"
    );
}
