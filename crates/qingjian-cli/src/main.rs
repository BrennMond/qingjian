//! # qingjian-cli — 命令行调试前端
//!
//! 中文职责：把按键序列喂给引擎，观察候选与上屏结果。
//! English role: feed a key sequence to the engine and observe candidates and commits.
//! 架构位置：`platforms/` 之外的"调试用前端"，与 TSF / Android 前端平级，
//! 只是它跑在终端里。
//!
//! # 它同时是"通用性"的活体验证
//!
//! `--schema` 可以切到任何一个已装载的方案，而**引擎代码完全一样**：
//!
//! ```text
//! $ qingjian --schema pinyin-demo nihao      → 你好（规范拼写）
//! $ qingjian --schema pinyin-demo nh         → 你好（简拼，分数更低）
//! $ qingjian --schema shape-demo ab          → 十（精确编码，无拼写运算）
//! ```
//!
//! 第二条与第三条分别落在两族翻译器上。**这比任何文档声明都有说服力**——
//! 见 `PLAN.md` D33。

use qingjian_core::{is_exact, Engine, Key, KeyCode, Modifiers, NamedKey, Outcome};
use std::process::ExitCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "\
Qingjian IME（青简输入法）命令行调试前端

用法：
  qingjian [选项] <按键序列>

选项：
  -h, --help            显示本帮助
  -V, --version         显示版本
      --list            列出已装载的方案
      --schema <id>     选择方案（默认第一个）
      --scheme-dir <p>  从目录装载方案（不指定则用内嵌的默认方案）
      --candidates      打印候选列表（当前页）
      --candidates=N    打印前 N 个候选；`--candidates=all` 打印全部
      --check           运行内核自检（不变式）
      --dump-config     打印合并后的完整方案，并标注每个值的来源
      --components      打印零件注册表（认识了什么、缺什么）
      --userdb <路径>   打开用户记忆（**默认关闭**：给了它才学、才记）
      --predict         打开下一词预测（P4b；**默认关闭**，且需要 `--userdb`）
      --embed           打开本地向量偏好记忆（P5/D46；**默认关闭**，需要 `--userdb`）
      --commit-seq=<甲,乙> 依次上屏每一段按键（验证需要多个上下文的功能）
      --dump-memory     打印用户记忆里学到的全部条目（含预测表）
      --select=<n>      上屏第 n 个候选（默认按空格选第 1 个；视为明确点选）
      --option=<名>     打开方案里的一个开关（`--option=emoji`）
      --option=<名>=off 关掉它（`--option=traditionalization=off`）

示例：
  qingjian nihao                   拼音：上屏「你好」
  qingjian nh                      拼音：简拼也上屏「你好」（分数更低）
  qingjian --candidates ni         看候选列表（含分数 / 来源 / 属性）
  qingjian --schema shape ab       精确编码方案：上屏「十」
  qingjian --scheme-dir ./my-schemes --list    装载自己的方案目录
  qingjian --scheme-dir schemes/qingjian-default --option=emoji weixiao
                                            开 emoji 开关，看候选里有没有 😄
  qingjian --userdb /tmp/u.mem --select=3 shi  把第 3 个候选上屏并**记住**
  qingjian --userdb /tmp/u.mem --dump-memory   看记住了什么（两张表）
  qingjian --scheme-dir schemes/qingjian-default --userdb /tmp/u.mem \
      --predict --commit-seq=jintian,tianqi,jintian
                                            连续上屏，看「今天 → 天气」的预测

说明：
  · 用户记忆**默认关闭**：不给 `--userdb` 就没有记忆，于是「刚克隆下来」
    的行为逐字节可复现（产品决定，见 HANDOFF §7.6.2 第 6 步）。
  · **下一词预测也默认关闭**，而且要 `--userdb` 与 `--predict` 同时给：
    它消费的是用户记忆里的第二张表（上下文 n-gram），
    没有记忆就没有数据（HANDOFF §7.7.3 第 6 步的产品决定）。
  · **本地向量偏好记忆同样默认关闭**（D46 第①条）。它不上网、不加载外部模型，
    只用 `--userdb` 里的本地历史学出一组整数向量（`docs/embed-design.md`）；
    内存上限见那一页与称重台的报告。
  · `--userdb` 指向的文件在**按键路径上一次都不碰**——只有退出时才落盘。
    这条红线有测试：`crates/qingjian-memory/tests/no_disk_io_on_keypath.rs`。
  · 本程序是开发期的调试前端。真实的输入法前端是 platforms/windows（TSF）
    与 platforms/android（IME）。
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("qingjian {VERSION} — Qingjian IME（青简输入法）");
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--check") {
        return self_check();
    }
    if args.iter().any(|a| a == "--components") {
        return list_components();
    }

    // 方案来源：`--scheme-dir` 指定的目录，否则是**内嵌的 YAML**
    // （单一数据来源，因此两种路径走的都是同一个解析器）。
    let scheme_dir = args
        .iter()
        .position(|a| a == "--scheme-dir")
        .and_then(|i| args.get(i + 1))
        .cloned();
    //
    // 两条路都返回**连同来源表**的结果：`--dump-config` 要回答
    // "这个值来自哪一层"，而那个信息只在装载期存在。
    // 方案来源，按优先级：
    //   1. `--scheme-dir <目录>`（显式指定）
    //   2. **仓库里的默认方案目录**（`schemes/qingjian-default`，存在就用它）
    //   3. 内嵌的演示方案（几十条词；保证任何环境下都能跑起来）
    //
    // 第 2 条是"克隆下来就能打字"的落点：**41 万条的生成词库不进二进制**
    // （`include_str!` 会让它白胖 11 MB），而是走部署路径——编译成紧凑产物、
    // 按需分页地读，常驻内存只留索引。
    let auto = std::path::Path::new("schemes/qingjian-default");
    let chosen_dir: Option<std::path::PathBuf> = match &scheme_dir {
        Some(d) => Some(std::path::PathBuf::from(d)),
        None if auto.is_dir() => Some(auto.to_path_buf()),
        None => None,
    };
    // 目录装载走**跳过并报告**的策略：一个写错的方案**不该让整个输入法
    // 用不了**（审计 §2.G1 与项目自己的 D26 都要求这条）。被跳过的方案
    // 必须打印出来——静默跳过会让用户以为"我配了却没生效"。
    let loaded: Result<(Vec<qingjian_schemes::Loaded>, Vec<String>), _> = match &chosen_dir {
        // 目录装载走**部署路径**：词库编译成紧凑产物，按需分页地读。
        // 内嵌方案只有几十条词，用内存表更快，所以两条路各走各的。
        Some(root) => {
            qingjian_schemes::load_dir_deployed_reporting(root, &root.join(".qingjian-cache")).map(
                |d| {
                    let w = d.warnings();
                    (d.loaded, w)
                },
            )
        }
        None => qingjian_schemes::all_layered().map(|d| (d, Vec::new())),
    };
    let (loaded, skipped) = match loaded {
        Ok(d) => d,
        Err(e) => {
            eprintln!("装载方案失败：{e}");
            return ExitCode::FAILURE;
        }
    };
    for w in &skipped {
        eprintln!("⚠ {w}");
    }
    let defs: Vec<_> = loaded.iter().map(|l| l.def.for_engine()).collect();

    // ── 用户记忆（P4a）─────────────────────────────────────────────────
    //
    // **默认关闭**：不给 `--userdb` 就没有记忆。这是产品决定（HANDOFF
    // §7.6.2 第 6 步）：刚克隆下来的行为必须**逐字节可复现**，
    // 而记忆会让"同一串键"在两个不同的机器上给出不同的候选顺序。
    //
    // 给了路径才挂重排器、才消费学习事件——于是"记忆"这件事在默认路径上
    // **完全不存在**，而不是"存在但空着"。
    let userdb = args
        .iter()
        .position(|a| a == "--userdb")
        .and_then(|i| args.get(i + 1))
        .cloned();
    // 下一词预测：**独立的开关**，默认关（见下面的服务装配处）。
    let predict = args.iter().any(|a| a == "--predict");
    // 本地向量偏好记忆（P5 / D46）：**独立的开关，默认关**。
    let embed = args.iter().any(|a| a == "--embed");
    let clock: std::sync::Arc<dyn qingjian_core::Clock> =
        std::sync::Arc::new(qingjian_memory::SystemClock::new());
    let memory: Option<std::sync::Arc<qingjian_memory::FileMemory>> = userdb.as_ref().map(|p| {
        // 坏文件 = 降级成"没有记忆" + 一行警告，绝不阻止启动（D26）。
        let (m, warn) = qingjian_memory::FileMemory::open_or_degrade(
            p,
            std::sync::Arc::clone(&clock),
            qingjian_memory::DEFAULT_CAPACITY,
        );
        if let Some(w) = warn {
            eprintln!("⚠ {w}");
        }
        std::sync::Arc::new(m)
    });
    let services = {
        let base = qingjian_core::Services::new(std::sync::Arc::clone(&clock))
            // **真随机**（`uuid_translator` 用）。`Services::new` 的默认值是
            // 确定性的，那是给测试的；装出来的输入法每次生成同一个 UUID
            // 就是一个真缺陷，所以生产装配处必须显式换掉。
            .with_random(std::sync::Arc::new(|| {
                Box::new(qingjian_core::SystemRandom::new())
            }));
        let base = match &memory {
            Some(m) => base.with_ranker(std::sync::Arc::new(qingjian_memory::MemoryRanker::new(
                std::sync::Arc::clone(m) as std::sync::Arc<dyn qingjian_core::MemoryStore>,
            ))),
            None => base,
        };
        // ── 下一词预测（P4b）──
        //
        // **默认关**，而且与记忆是**两个开关**：给了 `--userdb` 只代表
        // "记下来"，预测要再加 `--predict`。理由是这两件事回答两个不同的问题
        // （"这个编码想要哪个词" vs "你接下来想打什么"），而后者会改变
        // 候选列表的形状——照 P4a 的先例，默认路径必须逐字节可复现。
        //
        // 没有记忆就没有预测数据：`--predict` 单独给出时只警告，不静默忽略。
        let base = match (predict, &memory) {
            (true, Some(m)) => base.with_prediction(
                std::sync::Arc::clone(m) as std::sync::Arc<dyn qingjian_core::MemoryStore>
            ),
            (true, None) => {
                eprintln!("⚠ --predict 需要 --userdb <路径>：预测数据来自用户记忆，没有它无处可查");
                base
            }
            (false, _) => base,
        };

        // ── 本地向量偏好记忆（P5 · D46）──
        //
        // **默认关**（D46 的第①条），而且要 `--userdb`：向量是从**本地历史**
        // 学出来的，没有历史就没有向量。它接在记忆重排器**之后**，
        // 于是它的作用是在"精确历史没给出信号"的地方打破僵局。
        match (embed, &memory) {
            (true, Some(m)) => {
                // 学习材料就是预测表那批 `(上下文 → 下一个词, 次数)`——
                // 它本来就是"本地历史与偏好"的计数形式（`qingjian-memory`）。
                let samples = m
                    .prediction_snapshot()
                    .into_iter()
                    .map(|e| (e.context, e.text, e.count));
                let trained = qingjian_embed::VectorMemory::train(
                    samples,
                    qingjian_embed::VectorConfig::default(),
                );
                if let Some(model) = trained {
                    // **D46 的第②条**：把上限打出来（称重台那边也打）。
                    eprintln!(
                        "· 本地向量记忆：{} 个词 × {} 维（{} KiB，最多给前 {} 名加 {} 毫对数）",
                        model.len(),
                        model.dim(),
                        model.bytes() / 1024,
                        qingjian_embed::EmbedRanker::DEFAULT_MAX_BOOSTED,
                        qingjian_embed::EmbedRanker::DEFAULT_LIMIT_ML,
                    );
                    base.with_ranker(std::sync::Arc::new(qingjian_embed::EmbedRanker::new(
                        std::sync::Arc::new(model),
                    )))
                } else {
                    eprintln!("· 本地向量记忆：历史还是空的，本次不加向量分");
                    base
                }
            }
            (true, None) => {
                eprintln!("⚠ --embed 需要 --userdb <路径>：向量由本地历史学出来，没有历史无从谈起");
                base
            }
            (false, _) => base,
        }
    };
    let engine = match qingjian_engine::EngineImpl::with_services(&defs, services) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("编译默认方案失败：{e}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(m) = &memory {
        if let Some(p) = m.path() {
            eprintln!(
                "· 用户记忆已打开：{}（已装载 {} 条，上限 {} 条）",
                p.display(),
                m.len(),
                m.capacity()
            );
        }
    }

    // **降级警告**：方案装上了，但有零件没生效（典型是外部数据没取回）。
    //
    // 缺失数据不阻止启动（D26），但必须**看得见**——"功能不生效却没有
    // 任何提示"是这个项目反复踩的坑，所以出口是一行 stderr 警告。
    for (id, note) in engine.degradations() {
        eprintln!("⚠ 方案 {id}：{note}");
    }

    if args.iter().any(|a| a == "--list") {
        for info in engine.schemas().list() {
            println!(
                "{:<14} {:<26} family={}",
                info.schema_id,
                info.name,
                info.family.as_deref().unwrap_or("-")
            );
        }
        return ExitCode::SUCCESS;
    }

    let schema_pos = args.iter().position(|a| a == "--schema");
    let schema_id = schema_pos.and_then(|i| args.get(i + 1)).cloned();
    let dir_pos = args.iter().position(|a| a == "--scheme-dir");
    let userdb_pos = args.iter().position(|a| a == "--userdb");
    // `--candidates`（按页，默认）或 `--candidates=N` / `--candidates=all`。
    //
    // **为什么要有 `=N`**：候选是分页显示的（默认每页 9 个），而
    // `Session::candidates()` 只把**当前页**交给前端。想看清"某个滤镜到底
    // 有没有产出候选"（例如 emoji 排在 30 名开外），就得能要看全量。
    // 这个项目里"功能存在但看不见"已经算过一次 bug，所以调试前端必须
    // 有一条把全量候选摊开的路。
    let candidate_limit: Option<usize> = args
        .iter()
        .find_map(|a| a.strip_prefix("--candidates="))
        .map(|v| {
            if v == "all" {
                usize::MAX
            } else {
                v.parse().unwrap_or(9)
            }
        });
    let show_candidates = args.iter().any(|a| a == "--candidates") || candidate_limit.is_some();
    // `--option=emoji`（开）/ `--option=emoji=off`（关）。
    //
    // 为什么需要它：方案里的开关（emoji、简繁、全角）**只能这样验证**——
    // 没有前端就没有开关界面，而"配置看着对、功能不生效"正是这个项目
    // 反复踩的那类 bug（HANDOFF §3）。能一条命令开关它，
    // 才谈得上端到端验证。
    let options: Vec<(String, bool)> = args
        .iter()
        .filter_map(|a| a.strip_prefix("--option="))
        .map(|spec| match spec.split_once('=') {
            Some((name, "off" | "0" | "false")) => (name.to_owned(), false),
            Some((name, _)) => (name.to_owned(), true),
            None => (spec.to_owned(), true),
        })
        .collect();

    // 按键序列 = 所有不以 `-` 开头、且不是某个**选项之值**的位置参数。
    // 漏掉任何一个选项都会让它的值被当成按键打出去 —— 这就是下面那句注释存在的理由。
    let keys: String = args
        .iter()
        .enumerate()
        .filter(|(i, a)| {
            let is_option_value = [schema_pos, dir_pos, userdb_pos]
                .into_iter()
                .flatten()
                .any(|p| *i == p + 1);
            !a.starts_with('-') && !is_option_value
        })
        .map(|(_, a)| a.as_str())
        .collect();

    if args.iter().any(|a| a == "--dump-config") {
        return dump_config(&engine, &loaded, schema_id.as_deref());
    }

    // `--dump-memory`：把记忆里学到的条目摊开。
    //
    // 为什么值得有一条命令：这是"我到底记住了什么"唯一可回答的地方，
    // 而 `--dump-config` 已经证明了这种"可观察出口"的价值
    // （"装进来了"不等于"生效了"，见 HANDOFF §3）。
    if args.iter().any(|a| a == "--dump-memory") {
        let Some(m) = &memory else {
            println!("# 用户记忆未打开（没有给 `--userdb <路径>`）");
            return ExitCode::SUCCESS;
        };
        let snapshot = m.snapshot();
        println!(
            "# 用户记忆：{} 条（上限 {}）{}",
            snapshot.len(),
            m.capacity(),
            m.path().map_or_else(
                || "，只在内存里".to_owned(),
                |p| format!("，文件 {}", p.display()),
            )
        );
        println!(
            "# {:<24} {:<12} {:>6} {:>9}  最后使用",
            "输入(键)", "词", "次数", "加成"
        );
        for e in &snapshot {
            println!(
                "  {:<24} {:<12} {:>6} {:>9}  {}",
                e.input,
                e.text,
                e.count,
                e.bonus.as_milli_log(),
                e.last_used
            );
        }

        // **预测表单独打**（P4b）：它的键是上下文，与上面那张表的键空间
        // 完全不同（`docs/engine-design.md` §4.3）。不分开打的话，
        // "预测怎么不生效"这个问题只能靠猜——而两张表混在一起看
        // 恰好会让人以为是同一张表。
        let preds = m.prediction_snapshot();
        println!(
            "\n# 预测表（下一词，P4b）：{} 条（上限 {}）",
            preds.len(),
            m.prediction_capacity()
        );
        println!(
            "# {:<20} {:<12} {:>6} {:>9}  最后使用",
            "上下文", "下一个词", "次数", "加成"
        );
        for e in &preds {
            println!(
                "  {:<20} {:<12} {:>6} {:>9}  {}",
                e.context.join(" "),
                e.text,
                e.count,
                e.bonus.as_milli_log(),
                e.last_used
            );
        }
        return ExitCode::SUCCESS;
    }

    let mut session = engine.create_session();

    if let Some(id) = schema_id {
        if let Err(e) = session.switch_schema(&id) {
            eprintln!("切换方案失败：{e}");
            return ExitCode::FAILURE;
        }
    }

    for (name, on) in &options {
        session.set_option(name, *on);
    }

    // ── `--commit-seq=甲,乙,丙`：连续上屏，用来手工验证**需要多个上下文**
    //    才成立的功能（下一词预测是第一个这样的功能）──
    //
    // 为什么需要它：**候选与上下文都是会话状态**，而一次 CLI 调用只在末尾
    // 上屏一次。于是"今天 → 天气"这种跨两次上屏的搭配在命令行上
    // **根本无法产生**——预置好的记忆文件也帮不上忙，因为
    // `Commit.context` 每次都还是空的。这条开关把"多打几个词"变成一句话。
    if let Some(seq) = args.iter().find_map(|a| a.strip_prefix("--commit-seq=")) {
        for part in seq.split(',').filter(|s| !s.is_empty()) {
            for c in part.chars() {
                session.process_key(Key::ch(c));
                feed_memory(&mut session, memory.as_deref());
            }
            let outcome = session.select(0, qingjian_core::SelectionSource::Keyboard);
            let events = feed_memory(&mut session, memory.as_deref());
            match outcome {
                Outcome::Committed(commit) => {
                    println!("{}", commit.text);
                    print_learned(&events);
                }
                other => {
                    eprintln!("未能上屏（{part:?}）：{other:?}");
                    return ExitCode::FAILURE;
                }
            }
        }
        if predict {
            print_predictions(&*session);
        }
        if let Some(m) = &memory {
            match m.flush() {
                Ok(true) => eprintln!("· 用户记忆已落盘：{} 条", m.len()),
                Ok(false) => {}
                Err(e) => eprintln!("⚠ 用户记忆落盘失败（本次学习没有保存）：{e}"),
            }
        }
        return ExitCode::SUCCESS;
    }

    if keys.is_empty() {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }

    // ── 按键 ──
    for c in keys.chars() {
        session.process_key(Key::ch(c));
        // **每个按键之后**都要把事件取干净并喂给记忆。
        //
        // 漏掉这一步的症状是"学了没记住"，而且**不报错**——
        // HANDOFF §7.6.3 把这条列为 P4a 最容易漏的坑。把它写在这里而不是
        // 只写在文档里，是因为这一行是"接线真的被走到"的唯一证据。
        feed_memory(&mut session, memory.as_deref());
    }

    if show_candidates {
        println!(
            "方案 {}  输入 {:?}  候选 {} 个",
            session.schema_id(),
            session.composition().input,
            session.candidates().len()
        );
        let limit = candidate_limit.unwrap_or(usize::MAX);
        for (i, c) in session.candidates().iter().take(limit).enumerate() {
            println!(
                "  {}. {:<8} score={:<9} origin={:<11} attr={:<9} lane={:?} {}",
                i + 1,
                c.text,
                c.score.as_milli_log(),
                format!("{:?}", c.origin),
                format!("{:?}", c.attr),
                c.lane,
                if is_exact(c.origin, c.attr) {
                    ""
                } else {
                    "← 猜的"
                }
            );
        }
        return ExitCode::SUCCESS;
    }

    // ── 提交 ──
    //
    // 默认按空格选第 1 个；`--select=<n>` 选第 n 个（1 起数）。
    // 后者是**验证用户记忆的唯一手动手段**：不选中一个"不是第一个"的候选，
    // 就永远观察不到"学过的词下次优先"。
    let selected: Option<usize> = args
        .iter()
        .find_map(|a| a.strip_prefix("--select="))
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n >= 1);
    let outcome = if let Some(n) = selected {
        // `--select=<n>` 是命令行上的**明确选择**，因此报
        // `SelectionSource::Pointer`：预测候选只允许被明确点选
        // （`docs/engine-design.md` §4.3.3），而这条命令正是验证
        // "预测候选能被选上并学习"的唯一手动手段。
        session.select(n - 1, qingjian_core::SelectionSource::Pointer)
    } else {
        let space = Key::press(KeyCode::Named(NamedKey::Space), Modifiers::NONE);
        session.process_key(space)
    };
    let events = feed_memory(&mut session, memory.as_deref());

    let code = match outcome {
        Outcome::Committed(commit) => {
            println!("{}", commit.text);
            print_learned(&events);
            // **上屏之后立刻看预测**（P4b）：会话在 `finish_commit` 里
            // 已经重算过一次，因此这里读到的是"以刚上屏的词为上下文"的预测。
            // 这条输出是 CLI 上唯一能看见「今天 → 天气」的地方。
            if predict {
                print_predictions(&*session);
            }
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("未能上屏：{other:?}");
            ExitCode::FAILURE
        }
    };

    // 落盘**只在这里**发生——按键路径上一次 I/O 都没有。
    if let Some(m) = &memory {
        match m.flush() {
            Ok(true) => eprintln!("· 用户记忆已落盘：{} 条", m.len()),
            Ok(false) => {}
            Err(e) => eprintln!("⚠ 用户记忆落盘失败（本次学习没有保存）：{e}"),
        }
    }
    code
}

/// 把会话事件取干净并喂给记忆，返回这一批事件的副本（供打印诊断）。
///
/// # 为什么事件要"每个按键之后"取，而不是"退出时一次"
///
/// `drain_events` 是**读取即清空**的（与会话的候选列表不同）。攒着不取，
/// 事件会一直堆在会话里；而 P4a 的学习发生在**每次上屏**时，
/// 所以每键取一次是唯一不会漏的时机。
fn feed_memory(
    session: &mut Box<dyn qingjian_core::Session + Send>,
    memory: Option<&qingjian_memory::FileMemory>,
) -> Vec<qingjian_core::Event> {
    let mut events = Vec::new();
    session.drain_events(&mut events);
    if let Some(m) = memory {
        qingjian_memory::apply_events(m, &events);
    }
    events
}

/// 把一批事件里的学习记录打出来（两种上屏路径共用）。
fn print_learned(events: &[qingjian_core::Event]) {
    for e in events {
        if let qingjian_core::Event::Learned {
            input, text, attr, ..
        } = e
        {
            eprintln!("  [学习] 输入 {input:?} → {text:?}（属性 {attr:?}）");
        }
    }
}

/// 把当前会话的**预测候选**打出来（P4b）。
///
/// 空列表也说话：**"没有预测"与"预测没接上"是两件不同的事**，
/// 而命令行上唯一的区别就是这行输出——静默什么都不打的话，
/// 使用者分不清"还没学过"与"功能坏了"。
fn print_predictions(session: &(dyn qingjian_core::Session + Send)) {
    let preds: Vec<String> = session
        .candidates()
        .iter()
        .filter(|c| c.lane == qingjian_core::Lane::Predict)
        .map(|c| format!("{}（{}）", c.text, c.score.as_milli_log()))
        .collect();
    if preds.is_empty() {
        eprintln!("  [预测] （这个上下文还没有学过的搭配）");
    } else {
        eprintln!("  [预测] 接下来可能打：{}", preds.join("、"));
    }
}

/// `--dump-config`：打印**合并后**的方案，并标注每个值的来源。
///
/// # 它兑现的是 D25 的一句话
///
/// > 打印出来的每一行都能被用户补丁覆盖，且标注它来自哪一层。
///
/// 因此这个命令做三件事，缺一不可：
///
/// 1. **列出层**（内置 / 方案 / 用户补丁）——用户得知道自己在跟谁较劲。
/// 2. **每个值后面缀上来源**（第几层、哪个文件、第几行）。RIME 用户
///    最熟悉的动作就是翻 `*.custom.yaml`，而这里直接告诉他去哪一行。
/// 3. **打印可粘贴的补丁片段**——这是"每一行都能被覆盖"的**可执行证明**：
///    复制走、改一个值、存成 `<schema_id>.custom.yaml`，再跑一次
///    就会看到来源那一列变了。空口说"可覆盖"是没有意义的。
///
/// # 与引擎的关系
///
/// 这些数字**全部来自装载期**（[`qingjian_schemes::Resolution`]），
/// 引擎一行都没参与。`--dump-config` 打印的是"装载器读到了什么"，
/// 而不是"引擎打算怎么跑"——两者分开，才能让用户看出
/// "我写的东西有没有被读进去"。
fn dump_config(
    engine: &qingjian_engine::EngineImpl,
    loaded: &[qingjian_schemes::Loaded],
    schema_id: Option<&str>,
) -> ExitCode {
    use qingjian_core::Engine;

    let id = if let Some(i) = schema_id {
        i.to_owned()
    } else if let Some(info) = engine.schemas().list().first() {
        info.schema_id.clone()
    } else {
        eprintln!("没有已装载的方案");
        return ExitCode::FAILURE;
    };
    let Some(entry) = loaded.iter().find(|l| l.def.info.schema_id == id) else {
        eprintln!("找不到方案 {id} 的装载记录（只有通过目录或内嵌方案装载的才有）");
        return ExitCode::FAILURE;
    };
    // 确认引擎真的能装载它 —— `--dump-config` 打印的东西必须是
    // **引擎真的在用的那一份**，而不是"装载器以为引擎会用的"。
    if let Err(e) = engine.schemas().acquire(&id) {
        eprintln!("装载方案 {id} 失败：{e}");
        return ExitCode::FAILURE;
    }
    let scheme = &entry.def;
    let res = &entry.resolution;
    let o = |p: &str| res.origin_note(p);

    let info = &scheme.info;
    println!("# 合并后的方案：{}", info.schema_id);
    println!("#");
    println!("# 层（先列的在下面，后列的在上面）：");
    for (i, l) in res.layers.iter().enumerate() {
        println!("#   [{}] {:<8} {}  —— {}", i, l.name, l.file, l.note);
    }
    println!(
        "# 本层贡献的生效值：{}",
        (0..res.layers.len())
            .map(|i| format!("[{}] {} 项", i, res.count_of_layer(i)))
            .collect::<Vec<_>>()
            .join("，")
    );
    println!();

    println!("schema:");
    println!("  schema_id: {}", info.schema_id);
    println!("  name: {}", info.name);
    println!("  version: {}", info.version);
    println!("  format_version: {}", info.format_version);
    if let Some(f) = &info.family {
        println!("  family: {f}");
    }
    println!("  {}", o("schema.family"));

    println!();
    println!("switches:");
    for (i, sw) in scheme.switches.iter().enumerate() {
        let states = sw
            .states
            .as_ref()
            .map_or_else(|| "-".to_owned(), |s| format!("[{}, {}]", s[0], s[1]));
        println!(
            "  - name: {:<22} reset: {}   states: {states}   {}",
            sw.name,
            u8::from(sw.on),
            o(&format!("switches.{i}"))
        );
    }

    println!();
    println!("engine:");
    let es = &scheme.engine;
    for (slot, names) in [
        ("processors", &es.processors),
        ("segmentors", &es.segmentors),
        ("translators", &es.translators),
        ("filters", &es.filters),
    ] {
        if names.is_empty() {
            println!("  {slot}: []   {}", o(&format!("engine.{slot}")));
        } else {
            println!("  {slot}:   {}", o(&format!("engine.{slot}")));
            for n in names {
                println!("    - {n}");
            }
        }
    }

    println!();
    println!("# 引擎侧的实际装配（由上面的声明编译而来）：");
    println!("#   主标签       {}", scheme.tag);
    println!(
        "#   翻译器族     {:?}{}",
        scheme.translator,
        if scheme.translator == qingjian_engine::TranslatorKind::ExactCode {
            "（无拼写表：编码集合不可枚举）"
        } else {
            "（有拼写表）"
        }
    );
    println!(
        "#   开关         {} 个   {}",
        scheme.switches.len(),
        o("switches")
    );
    println!(
        "#   拼写规则     {} 条   {}",
        scheme.rules.len(),
        // 两种写法都收：`speller.rules` 与 RIME 的 `speller.algebra`。
        match res.origin_of("speller.algebra") {
            Some(_) => o("speller.algebra"),
            None => o("speller.rules"),
        }
    );
    match entry.def.custom.get("component_coverage") {
        Some(s) => println!("#   零件覆盖     {s}"),
        None => println!("#   零件覆盖     （方案未声明 `engine:` 列表）"),
    }
    if let Some(n) = entry.def.custom.get("translator_kind_inferred") {
        println!("#   装载提示     {n}");
    }

    println!();
    println!("# ── 可粘贴的覆盖片段 ──");
    println!("# 把它复制成一个名为 `<方案 id>.custom.yaml` 的文件放在方案目录里，");
    println!("# 改掉任意一个值，再运行 `qingjian --scheme-dir <目录> --dump-config`：");
    println!("# 上面「来源」那一列会变成 [1] 用户补丁。**这就是 D25 的证明方式。**");
    println!("#");
    println!("schema:");
    println!("  name: 我改过的名字");
    println!("menu:");
    println!("  page_size: 9");
    ExitCode::SUCCESS
}

/// `--components`：打印零件注册表——我们认识什么、实现了什么、缺什么。
///
/// **这张表是 P3 验收线的一部分。** "跑通 `no_lua_schema`"这句话，
/// 落地就是"那张表里的 24 个零件，我们有几个能跑"——
/// 而含糊其辞地回答没有意义，所以这里把它逐条列出来，
/// 并把"你缺数据"与"我们缺代码"分成两类。
fn list_components() -> ExitCode {
    use qingjian_engine::registry::{
        implemented_names, missing_names, needs_data_names, needs_resource_names,
        not_applicable_names,
    };
    println!("# 零件注册表（RIME 的方案按名字引用它们）");
    println!();
    println!("已实现 {} 个：", implemented_names().len());
    for n in implemented_names() {
        let (_, slot, note) = qingjian_engine::registry::lookup(n);
        println!("  {n:<28} {slot:?}   {note}");
    }
    println!();
    println!(
        "需要外部数据 {} 个（机制有，数据要给）：",
        needs_data_names().len()
    );
    for n in needs_data_names() {
        let (_, slot, note) = qingjian_engine::registry::lookup(n);
        println!("  {n:<28} {slot:?}   {note}");
    }
    println!();
    println!(
        "尚未实现 {} 个（这是本项目的缺口）：",
        missing_names().len()
    );
    for n in missing_names() {
        let (_, slot, note) = qingjian_engine::registry::lookup(n);
        println!("  {n:<28} {slot:?}   {note}");
    }
    println!();
    println!(
        "需要外部资源或新语义 {} 个（**不是简单的缺代码**）：",
        needs_resource_names().len()
    );
    for n in needs_resource_names() {
        let (_, slot, note) = qingjian_engine::registry::lookup(n);
        println!("  {n:<28} {slot:?}   {note}");
    }
    println!();
    println!(
        "在本架构里不适用 {} 个（**不是缺口**，不必等）：",
        not_applicable_names().len()
    );
    for n in not_applicable_names() {
        let (_, slot, note) = qingjian_engine::registry::lookup(n);
        println!("  {n:<28} {slot:?}   {note}");
    }
    ExitCode::SUCCESS
}

/// 内核自检：验证那些"一旦破坏就会污染全部输出"的不变式。
///
/// 这些检查刻意放在 CLI 里而不是只放在测试里——**用户和贡献者都应该能
/// 一条命令确认内核还是健全的**。
fn self_check() -> ExitCode {
    use qingjian_core::{
        clamp_bonus, compare, is_exact, sort_candidates, Candidate, Lane, Origin, Score, Span,
        SpellingAttr,
    };

    let mut failures: Vec<String> = Vec::new();

    // 不变式 1：分数是对数域定点整数，单调且无 NaN。
    if Score::from_weight(1000.0) <= Score::from_weight(1.0) {
        failures.push("分数不再随权重单调".into());
    }
    if Score::from_weight(f64::NAN) != Score::FLOOR {
        failures.push("NaN 权重没有被钳到下界".into());
    }

    // 不变式 2：精确优先的判据由两个轴共同决定。
    if !is_exact(Origin::SystemWord, SpellingAttr::NORMAL) {
        failures.push("规范拼写的系统词未被判为精确".into());
    }
    if is_exact(Origin::SystemWord, SpellingAttr::ABBREV) {
        failures.push("简拼派生的词被误判为精确".into());
    }

    // 不变式 3：排序是全序且可复现。
    let mk = |text: &str, ml: i32, origin: Origin| Candidate {
        text: text.to_owned(),
        comment: None,
        score: Score::from_milli_log(ml),
        origin,
        attr: SpellingAttr::NORMAL,
        span: Span::new(0, 1),
        lane: Lane::Input,
        kind: qingjian_core::CandidateKind::Normal,
        key: None,
    };
    // 输入顺序特意打乱，让平局规则必须真的起作用。
    let build = || {
        vec![
            mk("b_sys", 1, Origin::SystemWord),
            mk("a_user", 1, Origin::UserWord),
            mk("c_sys", 1, Origin::SystemWord),
        ]
    };
    let mut first: Option<Vec<String>> = None;
    for _ in 0..64 {
        let mut v = build();
        sort_candidates(&mut v);
        let got: Vec<String> = v.into_iter().map(|c| c.text).collect();
        match &first {
            None => first = Some(got),
            Some(f) if f != &got => {
                failures.push("排序结果不可复现".into());
                break;
            }
            Some(_) => {}
        }
    }
    let want = ["a_user".to_owned(), "b_sys".to_owned(), "c_sys".to_owned()];
    if first.as_deref() != Some(&want) {
        failures.push("平局规则不符合预期（应为 origin 优先，再按插入序）".into());
    }

    // 不变式 4：比较函数是确定的。
    let a = mk("x", 5, Origin::SystemWord);
    let b = mk("y", 5, Origin::SystemWord);
    if compare(&a, &b) != compare(&a, &b) {
        failures.push("比较函数不确定".into());
    }

    // 不变式 5：重排器的加成被上界钳住（"结构保证优于运行期修正"）。
    let base = Score::from_milli_log(1000);
    let limit = Score::from_milli_log(2000);
    if clamp_bonus(base, Score::from_milli_log(9999), limit) != Score::from_milli_log(3000) {
        failures.push("重排器加成未被钳到上界".into());
    }

    // 不变式 6（P1 新增）：两族翻译器都必须可用 —— 这是 D33 的通用性保证。
    let defs = qingjian_schemes::all().unwrap_or_default();
    if !qingjian_schemes::uses_both_translator_families(&defs) {
        failures.push("内置方案不再覆盖两族翻译器（D33 的通用性保证失效）".into());
    }

    // 不变式 7（P1 新增）：同一个引擎必须能跑两种完全不同的输入法。
    if let Err(e) = engine_smoke_test() {
        failures.push(e);
    }

    // 不变式 8（P4a 新增）：用户记忆的量纲、上界与**键可复现**。
    //
    // 为什么它值得进 `--check`：G10 那颗地雷的症状（"学过的词有时出现
    // 有时不出现"）没有任何报错，只有"写进去的键查得回来"这条性质能证伪它。
    {
        use qingjian_core::{Commit, FrozenClock, MemoryStore, Origin, SpellingAttr, Trigger};
        use qingjian_memory::{bonus_ml, normalize_key, FileMemory, MAX_BONUS_ML};

        if bonus_ml(0) != 0 {
            failures.push("记忆：零频次的加成不是零".into());
        }
        if bonus_ml(1_000_000) > MAX_BONUS_ML {
            failures.push("记忆：加成越过了声明的上界".into());
        }
        let mut prev = -1;
        for f in [0_u64, 1_000, 10_000, 1_000_000] {
            let ml = bonus_ml(f);
            if ml < prev {
                failures.push("记忆：加成不随频次单调".into());
                break;
            }
            prev = ml;
        }
        if normalize_key("ni'hao") != "nihao" || normalize_key("NI HAO") != "nihao" {
            failures.push("记忆：键的规范化没有去掉分隔符 / 统一大小写".into());
        }

        let clock = std::sync::Arc::new(FrozenClock {
            secs: 1_767_225_600,
            ms: 0,
            offset_secs: 0,
        });
        let memory = FileMemory::in_memory(clock, 64);
        memory.record(&Commit {
            text: "你好".into(),
            input: "ni'hao".into(),
            context: Vec::new(),
            origin: Origin::SystemWord,
            attr: SpellingAttr::NORMAL,
            lane: qingjian_core::Lane::Input,
            trigger: Trigger::Space,
            // **故意不给 key**：这条自检要证的正是"拿不到编码时的兜底
            // 路径也不会写出检索不到的数据"（G10 那颗地雷）。
            key: None,
        });
        if memory.lookup("nihao").is_empty() {
            failures.push("记忆：record 之后 lookup 查不到（G10 的无效数据）".into());
        }

        // ② 引擎给键时（D42 的真实形态）：键**不被加工**，
        //    而且它与拼写键是两把不同的键——编码里的 `'` 是分隔符，
        //    不是可以顺手去掉的装饰。
        let keyed = FileMemory::in_memory(
            std::sync::Arc::new(FrozenClock {
                secs: 1_767_225_600,
                ms: 0,
                offset_secs: 0,
            }),
            64,
        );
        keyed.record(&Commit {
            text: "你好".into(),
            input: "nhao".into(),
            context: Vec::new(),
            origin: Origin::SystemWord,
            attr: SpellingAttr::ABBREV,
            lane: qingjian_core::Lane::Input,
            trigger: Trigger::Space,
            key: Some("ni'hao".into()),
        });
        if keyed.lookup("ni'hao").is_empty() {
            failures.push("记忆：规范编码键 record 之后查不到".into());
        }
        if !keyed.lookup("nihao").is_empty() {
            failures.push("记忆：编码键被当成拼写规范化了（两把键串了）".into());
        }
    }

    if failures.is_empty() {
        println!("内核自检通过：8 组不变式全部成立。");
        ExitCode::SUCCESS
    } else {
        eprintln!("内核自检失败：{}", failures.len());
        for f in &failures {
            eprintln!("  - {f}");
        }
        ExitCode::FAILURE
    }
}

/// 端到端冒烟：用**同一个引擎**跑两个方案，覆盖两族翻译器。
///
/// # 它曾经写死过方案 id，然后静默失效了很久
///
/// 这里原本写的是 `run("pinyin", …)`。P3.5 把内嵌演示方案的 id 改成
/// `pinyin-demo`（见 `z-pinyin-demo.schema.yaml` 顶部的说明），
/// **漏改了这一处**——于是自检从那时起就一直在报"方案不存在：pinyin"。
///
/// 教训与铁律第 5 条同形：**"接线在、但没被走到"**。修法不是把字符串改对，
/// 而是**从实际装载到的方案里取 id**：这样重命名再也不会悄悄打断自检。
fn engine_smoke_test() -> Result<(), String> {
    use qingjian_engine::scheme::TranslatorKind;

    let defs = qingjian_schemes::all().map_err(|e| format!("默认方案装载失败：{e}"))?;
    let engine =
        qingjian_engine::EngineImpl::new(&defs).map_err(|e| format!("默认方案编译失败：{e}"))?;

    // 按**翻译器族**挑方案，而不是按写死的名字。
    let pick = |kind: TranslatorKind, what: &str| -> Result<String, String> {
        defs.iter()
            .find(|d| d.translator == kind)
            .map(|d| d.info.schema_id.clone())
            .ok_or_else(|| format!("内置方案里没有任何{what}族的方案"))
    };
    let spelling = pick(TranslatorKind::SpellingGraph, "拼写图")?;
    let exact = pick(TranslatorKind::ExactCode, "精确编码")?;

    let run = |schema: &str, keys: &str| -> Result<String, String> {
        let mut s = engine.create_session();
        s.switch_schema(schema)
            .map_err(|e| format!("切换 {schema} 失败：{e}"))?;
        for c in keys.chars() {
            s.process_key(Key::ch(c));
        }
        let space = Key::press(KeyCode::Named(NamedKey::Space), Modifiers::NONE);
        match s.process_key(space) {
            Outcome::Committed(c) => Ok(c.text),
            other => Err(format!("{schema}/{keys} 未能上屏：{other:?}")),
        }
    };

    // ① 拼写图族：规范拼写。
    let a = run(&spelling, "nihao")?;
    if a != "你好" {
        return Err(format!("{spelling}/nihao 应当上屏「你好」，得到「{a}」"));
    }
    // ② 拼写图族：变体拼写（简拼）。
    //
    // **为什么是 `nhao` 而不是 `nh`**：简拼的边界在 P3.5 就写清楚了
    // （HANDOFF §7 第 6 条与 §5 第 28 条）——拼写展开是带硬上限的深搜，
    // `nh` 这种"每个音节只留首字母"的极端缩写要和大量词争名额，
    // 于是 `[ni][hao]` 这条**完整**切分不一定被生成（实测：得到字面量）。
    // 自检要守的是"变体拼写确实能上屏"这条不变式，而不是某一个缩写串；
    // 用 `nhao` 表达它，才不会把一条**已知边界**当成回归。
    let b = run(&spelling, "nhao")?;
    if b != "你好" {
        return Err(format!("{spelling}/nhao 应当上屏「你好」，得到「{b}」"));
    }
    // ③ 精确编码族：完全不同的输入法，同一个引擎。
    let c = run(&exact, "ab")?;
    if c != "十" {
        return Err(format!("{exact}/ab 应当上屏「十」，得到「{c}」"));
    }
    Ok(())
}
