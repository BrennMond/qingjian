//! # 默认词库的**词级读音质量评测**（审计 §2.I 第 4 条）
//!
//! 审计要求：「建立质量评测集，独立于生成器的训练/来源数据：常用词、
//! 地名、人名、多音字、简拼、混输。报告召回率、首选率、前五命中和错误类型。」
//!
//! # 这个评测集**独立于生成器**
//!
//! 用例是**手写**的（来源：审计 §2.I 点名的四例 + 常用多音字/地名/姓氏），
//! 不是从 `tools/wordlist-gen` 的输入里抽的。这一点很重要：从训练数据里
//! 抽评测集，测的是"生成器会不会背"，不是"词库对不对"。
//!
//! # 指标
//!
//! - **召回率**：正确编码能否查到目标词；
//! - **首选率**：目标词是否排在第一位；
//! - **前五命中**：目标词是否在前 5 个候选里。
//!
//! 失败时把三类计数一起打出来——只报"某一条失败"会让人以为是个案，
//! 而这是一张**质量表**。
//!
//! # 它依赖真实词库（`schemes/stele-default`）
//!
//! 演示词库只有几十条，量不出读音质量。首次运行会编译一次部署产物
//! （缓存在 `.stele-cache`），之后复用。

use stele_core::{Engine, Key};
use stele_engine::EngineImpl;

/// 一条评测用例：`(说明, 输入, 目标词, 类别)`。
struct Case {
    input: &'static str,
    want: &'static str,
    kind: &'static str,
}

const CASES: &[Case] = &[
    // ── 审计 §2.I 点名的四例（**这是本轮修复的验收线**） ──
    Case {
        input: "yinhang",
        want: "银行",
        kind: "多音字",
    },
    Case {
        input: "chongqing",
        want: "重庆",
        kind: "地名·多音字",
    },
    Case {
        input: "yinyue",
        want: "音乐",
        kind: "多音字",
    },
    Case {
        input: "chongxin",
        want: "重新",
        kind: "多音字",
    },
    // ── 常用词 ──
    Case {
        input: "nihao",
        want: "你好",
        kind: "常用词",
    },
    Case {
        input: "zhongguo",
        want: "中国",
        kind: "常用词",
    },
    Case {
        input: "shijie",
        want: "世界",
        kind: "常用词",
    },
    Case {
        input: "weixin",
        want: "微信",
        kind: "常用词",
    },
    // ── 高频多音字词 ──
    Case {
        input: "hangye",
        want: "行业",
        kind: "多音字",
    },
    Case {
        input: "zhongyao",
        want: "重要",
        kind: "多音字",
    },
    Case {
        input: "haishi",
        want: "还是",
        kind: "多音字",
    },
    Case {
        input: "juede",
        want: "觉得",
        kind: "多音字",
    },
    Case {
        input: "chuli",
        want: "处理",
        kind: "多音字",
    },
    Case {
        input: "changjiang",
        want: "长江",
        kind: "多音字",
    },
    Case {
        input: "kuaile",
        want: "快乐",
        kind: "多音字",
    },
    Case {
        input: "yueqi",
        want: "乐器",
        kind: "多音字",
    },
    Case {
        input: "pianyi",
        want: "便宜",
        kind: "多音字",
    },
    Case {
        input: "daifu",
        want: "大夫",
        kind: "多音字",
    },
    // ── 地名 ──
    Case {
        input: "xiamen",
        want: "厦门",
        kind: "地名",
    },
    Case {
        input: "bengbu",
        want: "蚌埠",
        kind: "地名",
    },
    Case {
        input: "bozhou",
        want: "亳州",
        kind: "地名",
    },
    Case {
        input: "panyu",
        want: "番禺",
        kind: "地名",
    },
    // ── 人名/姓氏 ──
    Case {
        input: "chanyu",
        want: "单于",
        kind: "人名",
    },
    Case {
        input: "yuchi",
        want: "尉迟",
        kind: "人名",
    },
    // ── 简拼（缩写） ──
    Case {
        input: "nh",
        want: "你好",
        kind: "简拼",
    },
    Case {
        input: "nhao",
        want: "你好",
        kind: "简拼",
    },
];

fn real_engine() -> EngineImpl {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemes/stele-default");
    assert!(root.is_dir(), "找不到默认方案目录 {}", root.display());
    let defs = stele_schemes::load_dir_deployed(&root, &root.join(".stele-cache"))
        .unwrap_or_else(|e| panic!("默认方案必须能装载：{e}"));
    EngineImpl::new(&defs).expect("默认方案必须能编译")
}

#[test]
fn the_default_lexicon_recalls_the_word_for_its_correct_reading() {
    let engine = real_engine();
    let mut total = 0usize;
    let mut recalled = 0usize;
    let mut top1 = 0usize;
    let mut top5 = 0usize;
    let mut misses: Vec<String> = Vec::new();

    for case in CASES {
        total += 1;
        let mut s = engine.create_session();
        for c in case.input.chars() {
            s.process_key(Key::ch(c));
        }
        let got: Vec<&str> = s.candidates().iter().map(|c| c.text.as_str()).collect();
        let pos = got.iter().position(|t| *t == case.want);
        match pos {
            None => misses.push(format!(
                "召回失败 [{}] `{}` 找不到「{}」；前 5 = {:?}",
                case.kind,
                case.input,
                case.want,
                &got[..got.len().min(5)]
            )),
            Some(0) => {
                recalled += 1;
                top1 += 1;
                top5 += 1;
            }
            Some(i) if i < 5 => {
                recalled += 1;
                top5 += 1;
                misses.push(format!(
                    "非首选（排第 {}）[{}] `{}` →「{}」；前 3 = {:?}",
                    i + 1,
                    case.kind,
                    case.input,
                    case.want,
                    &got[..got.len().min(3)]
                ));
            }
            Some(i) => {
                recalled += 1;
                misses.push(format!(
                    "排在 {i}（不在前 5）[{}] `{}` →「{}」；前 5 = {:?}",
                    case.kind,
                    case.input,
                    case.want,
                    &got[..got.len().min(5)]
                ));
            }
        }
    }

    // **报告三类指标**，而不是只报"某一条失败"。
    let report = format!(
        "词级读音质量（{} 条）：召回 {recalled}/{total}（{}%），\
         首选 {top1}/{total}（{}%），前五 {top5}/{total}（{}%）\n失败明细：\n  {}",
        total,
        recalled * 100 / total,
        top1 * 100 / total,
        top5 * 100 / total,
        misses.join("\n  ")
    );
    println!("{report}");

    // 门槛：先守住"审计点名的四例必须首选"，再要求整体不退化。
    assert_eq!(
        top1, total,
        "首选率必须 100%——审计要求的是「正确读音召回目标词」，\
         排在后面等于用户仍要多按一次。\n{report}"
    );
}

#[test]
fn the_wrong_reading_does_not_have_to_disappear() {
    // 审计 §2.I 第 5 条：**一个词可以有多个读音**。
    // 「银行」的 `yinxing` 是一个合法的读音组合（"银"+"行(xíng)"），
    // 覆盖表不该把它删掉——多音字本来就有两读。
    // 这条测试防的是"为了修 A 而把 B 删掉"这种过度修正。
    let engine = real_engine();
    let mut s = engine.create_session();
    for c in "yinxing".chars() {
        s.process_key(Key::ch(c));
    }
    let got: Vec<&str> = s.candidates().iter().map(|c| c.text.as_str()).collect();
    assert!(
        got.contains(&"银行"),
        "多音字的另一读不该被删掉（审计 §2.I 第 5 条）：{got:?}"
    );
}

/// **已知缺陷（生成器，不是覆盖表）**：单字编码错误会连锁污染词级结果。
///
/// 本轮实测发现的第一个实例：
///
/// ```text
/// $ grep -m1 "^家" schemes/stele-default/cn_dicts/generated.dict.yaml
/// 家    jie    41023          ← 「家」读 jia，不读 jie
/// ```
///
/// 后果是**连锁**的：
///
/// 1. `jie` 的首选变成「家」；
/// 2. `nihaoshijie` 被动态规划拼成「你好**是家**」而不是「你好**世界**」
///    ——因为「家」的权重（41023）把两词组合顶掉了。
///
/// 覆盖表已经补上 `家 jia`（于是 `jia` 能正确召回「家」），
/// 但**错误的那条仍在生成词库里**，因此这两条断言现在还红。
///
/// 根本修法在 `tools/wordlist-gen`：单字读音要按 pinyin-data 的
/// **首选读音**取，并在生成后做一次"码与读音一致性"自检。
/// 这条测试是那个修法的验收点——它绿了，就说明生成器修好了。
#[test]
#[ignore = "已知缺陷：生成器把「家」编成 jie；修法在 tools/wordlist-gen，不在覆盖表"]
fn the_wrong_reading_of_a_generator_entry_does_not_hijack_a_correct_one() {
    let engine = real_engine();

    // ① `jie` 的首选不该是「家」——「家」没有 jie 这个读音。
    let mut s = engine.create_session();
    for c in "jie".chars() {
        s.process_key(Key::ch(c));
    }
    assert_ne!(
        s.candidates().first().map(|c| c.text.as_str()),
        Some("家"),
        "「家」被编成了 `jie`：单字读音取错了"
    );

    // ② 连锁后果：`nihaoshijie` 不该被拼成「你好是家」。
    let mut s = engine.create_session();
    for c in "nihaoshijie".chars() {
        s.process_key(Key::ch(c));
    }
    let top = s.candidates().first().map(|c| c.text.clone());
    assert_ne!(
        top.as_deref(),
        Some("你好是家"),
        "错误读音把动态规划带偏了（应当能拼出「你好世界」）"
    );
}

#[test]
fn the_curated_override_table_is_documented_and_sourced() {
    // 覆盖表必须有**来源与测试**（审计 §2.I 第 3 条）。
    // 这条断言守的是"文件头必须写清楚来源与收录标准"——
    // 一张没有来源的人工表，比没有表更糟。
    let text =
        include_str!("../../../schemes/stele-default/cn_dicts/word_pinyin.override.dict.yaml");
    for needle in ["来源", "审计", "自撰", "现代汉语词典"] {
        assert!(
            text.contains(needle),
            "覆盖表的文件头必须写明 `{needle}`：它是人工数据，来源必须可审计"
        );
    }
    // 而且它必须**真的被导入**（写了文件却没人 import = 解析不等于生效）。
    let main = include_str!("../../../schemes/stele-default/pinyin.dict.yaml");
    assert!(
        main.contains("cn_dicts/word_pinyin.override"),
        "覆盖表必须被 `pinyin.dict.yaml` 导入，否则它一点作用都没有"
    );
    // 顺序也是语义的一部分：覆盖表必须排在 generated **之前**
    // （"先出现者优先"），否则同码时它的权重不生效。
    // 只看 `import_tables:` **之后**的部分：注释里也会出现这些路径名，
    // 从全文找会拿到注释里的位置（第一次写这条断言时就踩了这个坑）。
    let imports = &main[main.find("import_tables:").expect("必须有 import_tables")..];
    let ov = imports.find("word_pinyin.override").unwrap();
    let gen = imports.find("cn_dicts/generated").unwrap();
    assert!(
        ov < gen,
        "覆盖表必须排在 generated 之前，否则同码时覆盖不生效"
    );
}
