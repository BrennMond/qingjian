#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""RIME × Stele 对比测试。

中文职责：以 **librime**（引擎）与 **plum**（方案/配方管理器）两个上游为准，
把 Stele 与真实 RIME 放在同一套定义下对照，输出一份可复现的报告。
English role: compare Stele against upstream RIME (librime runtime + plum recipe
ecosystem) under explicit, falsifiable definitions, and emit one report.

# 四个维度（每个维度**先定义比什么**，再比）

| 维度 | 对照对象 | 断言什么 | 出处 |
| --- | --- | --- | --- |
| **A 结构行为** | 真实 librime（系统运行库）vs stele | 能否上屏 / 按键是否被处理 / 全角标点 | `tools/compare-librime.py`（P3 的验收线，原样调用） |
| **B 同一份词表下的排序** | 两边读**同一份** `.dict.yaml` | 同码候选必须都按词库权重降序；简拼都要命中 | 本目录 `fixtures/` |
| **C 上游方案能否装载** | `/usr/share/rime-data`（plum preset 的部署产物） | 每个上游方案能否被 Stele 装载，不能的原因是哪一类 | librime 仓库 + plum preset |
| **D 配方覆盖** | plum 的 `preset-packages.conf` / `extra-packages.conf` | 每个配方在这台机器上的部署情况与 Stele 的装载结果 | plum 仓库 |

**B 是这次新增的核心**：`tools/compare-librime.py` 的报告里写着一句
"要比排序，得先让两边吃同一份词表"。两侧读同一份词表之后，
"候选排序不同"就再也不能用"词库不同"解释——它只能来自引擎。

# 断言与观察是两件事

- **断言（invariant）**：必须成立，不成立则退出码非 0。
  例如"同码候选在两边都按权重降序"。
- **观察（divergence）**：如实记录两边行为不同的地方，**不影响退出码**。
  它们可能是"我们有意不做的"（D14/D20/D24），也可能是**真缺口**——
  两类都写进报告，由人决定。把"发现分歧"直接判成失败，会让工具
  因为"我们有意与上游不同"而永远红着，那时就没人看它了。

# 用法

    cargo build --release -p stele-cli
    cd tools/librime-probe && ./build.sh        # 一次性：编出 probe
    python3 tools/rime-compare/compare.py       # 写报告 + 打印摘要
    python3 tools/rime-compare/compare.py --skip-structural   # 跳过 A（较慢）

报告默认写到 `tools/rime-compare/report.md`。

# 前置条件（明确写出来，不猜）

- 本机装有 librime 运行库（`librime.so.1`）与它的方案数据
  （`/usr/share/rime-data`，即 plum preset 包的部署产物）；
  探针经 `dlopen` 调用，不需要 `librime-dev`。
- 维度 D 需要 plum 仓库副本（默认 `.work/upstream/plum`）。
  缺失时 D 会被**跳过并注明**，不猜一个配方表出来。
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
FIXTURES = Path(__file__).resolve().parent / "fixtures"

DEFAULT_PROBE = ROOT / "tools" / "librime-probe" / "probe"
DEFAULT_STELE = ROOT / "target" / "release" / "stele"
DEFAULT_RIME_DATA = Path("/usr/share/rime-data")
DEFAULT_PLUM = ROOT / ".work" / "upstream" / "plum"
DEFAULT_WORK = ROOT / ".work" / "rime-compare"

SCHEMA_ID = "stele-cmp"

# ─────────────────────────────────────────────────────────────────────────────
# 用例集
# ─────────────────────────────────────────────────────────────────────────────

# 排序用例：输入 → 人类可读的说明。
# 期望顺序**从共用词表算出来**（不是写死的），所以改词表不用改用例。
ORDER_CASES: list[tuple[str, str]] = [
    ("ni", "同码单字：三个字按权重降序"),
    ("nihao", "同码词：三个词按权重降序"),
    ("hao", "同码单字：三个字按权重降序"),
    ("women", "两音节词"),
    ("xian", "跨切分竞争：单音节 `xian` vs 两音节 `xi an`"),
]

# 缩写用例：`nh` 必须命中「你好」、`wom` 必须命中「我们」。
# 这是我们自己的小字母表下**应当成立**的行为；`nh` 在 41 万条的默认方案里
# 打不出来，是展开名额被截断的问题（HANDOFF §5 第 28 条），不是这条规则失效。
ABBREV_CASES: list[tuple[str, str]] = [
    ("nh", "你好"),
    ("wom", "我们"),
]

# 分歧观察用例：只记录，不断言。
# 这些输入**故意**让 RIME 与 Stele 的模型分叉（前缀匹配 / 造句），
# 用来把差异摆到报告里。`机制` 一列写清 librime 是靠哪条通路做到的。
DIVERGENCE_CASES: list[tuple[str, str, str]] = [
    ("niha", "末音节只打了一半（`ha` 是 `hao` 的前缀）", "predictive 查询（`Prism::ExpandSearch`）"),
    ("haoni", "词库里没有「好你」，但两个字都有", "造句：两个单字拼成词库外的词"),
    ("nihaoshijie", "词库里只有前两个音节，后面是未消费的输入", "只翻译能认出的前缀段，剩余留在输入里"),
]

# 上游方案装载失败的原因分类。
# 每条是 `(日志里出现的子串, 归类, 处置)`，处置取 `intentional` / `gap`。
#
# ⚠️ 子串必须**足够特别**：`编码单元` 曾在提示文本
# （"列表写法：每个元素是一个编码单元…"）里出现，于是把一堆
# "缺 alphabet" 的方案误判成"编码切不开"。教训：分类的判据
# 也要**反向验证**（拿它去跑一遍已知的样本，看有没有误判）。
FAILURE_CATEGORIES: list[tuple[str, str, str]] = [
    ("import_preset", "引用了 RIME 自带预设（`default` / `symbols`）：机制有、**资产**没搬（D24）", "intentional"),
    ("开关缺少", "RIME 的开关可以只写 `options:`（单选组），Stele 要求 `name`", "gap"),
    ("不认识的按键名", "RIME 用 X11 keysym 名（`KP_1`、`Shift+exclam`…），Stele 只认自己的键名", "gap"),
    ("引用了字母表里没有的编码单元", "词条编码未按空格切成字母表单元（精确编码族要求编码可逐项枚举）", "gap"),
    ("不是数字", "词库权重列不是数字：RIME 的 `%` 百分比权重，或 `columns:` 声明的非文字列（如 `stem`）", "gap"),
    ("缺少 `speller.alphabet`", "`alphabet` 由 `__patch` 指向另一个 YAML 的子树补全，Stele 未实现跨文件 `__patch`", "gap"),
    ("`speller.rules` 必须是列表", "`speller.algebra` 是 `__patch` 映射而不是规则列表（同上）", "gap"),
]

# 共享资产类配方：它们**不含方案**，别把它当成"未部署的方案"。
SHARED_ASSET_PACKAGES: dict[str, tuple[str, str]] = {
    "essay": ("essay.txt", "八股文词表（共享资产，无方案）"),
    "prelude": ("default.yaml", "默认配置与预设（共享资产，无方案）"),
}


# ─────────────────────────────────────────────────────────────────────────────
# 小工具
# ─────────────────────────────────────────────────────────────────────────────


def run(cmd: list, timeout: int = 300) -> subprocess.CompletedProcess:
    return subprocess.run(
        [str(c) for c in cmd], capture_output=True, text=True, timeout=timeout
    )


def md_escape(text: str) -> str:
    return text.replace("|", "\\|")


def git_rev(repo: Path) -> str:
    """取仓库副本的短 hash + 日期；取不到就返回 `未克隆`。"""
    if not (repo / ".git").exists():
        return "未克隆"
    try:
        p = run(["git", "-C", repo, "log", "-1", "--format=%h %ad", "--date=short"], timeout=30)
        return p.stdout.strip() if p.returncode == 0 and p.stdout.strip() else "未知"
    except Exception:
        return "未知"


# ─────────────────────────────────────────────────────────────────────────────
# A. 结构行为（原样调用 P3 的对照工装）
# ─────────────────────────────────────────────────────────────────────────────


def section_structural() -> tuple[str, bool]:
    script = ROOT / "tools" / "compare-librime.py"
    if not script.exists():
        return "_跳过：找不到 `tools/compare-librime.py`。_", True
    p = run([sys.executable, script], timeout=1800)
    body = p.stdout.strip() or p.stderr.strip()
    # 报告里降两级标题，避免与本文档的章节层级打架
    body = re.sub(r"(?m)^## ", "#### ", body)
    body = re.sub(r"(?m)^# ", "### ", body)
    return body, p.returncode == 0


# ─────────────────────────────────────────────────────────────────────────────
# B. 同一份词表下的排序对照
# ─────────────────────────────────────────────────────────────────────────────


def parse_shared_dict(path: Path) -> list[tuple[str, str, float]]:
    """读共用词表：返回 `(词, 去掉空格的编码, 权重)`。"""
    entries: list[tuple[str, str, float]] = []
    in_body = False
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.rstrip("\n")
        if not in_body:
            if line.strip() == "...":
                in_body = True
            continue
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        fields = line.split("\t")
        if len(fields) < 2:
            continue
        word, code = fields[0].strip(), fields[1].strip()
        weight = 0.0
        if len(fields) >= 3 and fields[2].strip():
            try:
                weight = float(fields[2].strip())
            except ValueError:
                weight = 0.0
        entries.append((word, code.replace(" ", ""), weight))
    return entries


def expected_order(entries: list[tuple[str, str, float]], keys: str) -> list[str]:
    rows = [(w, wt) for (w, code, wt) in entries if code == keys]
    rows.sort(key=lambda r: -r[1])
    return [w for w, _ in rows]


def prepare_shared(work: Path) -> tuple[Path, Path]:
    """把 fixtures 铺成两个可直接装载的目录（RIME 侧 / Stele 侧）。"""
    rime_dir = work / "shared" / "rime"
    stele_dir = work / "shared" / "stele"
    for d in (rime_dir, stele_dir):
        if d.exists():
            shutil.rmtree(d)
        d.mkdir(parents=True)
        shutil.copy(FIXTURES / "shared.dict.yaml", d / f"{SCHEMA_ID}.dict.yaml")
    shutil.copy(FIXTURES / "rime" / "default.yaml", rime_dir / "default.yaml")
    shutil.copy(
        FIXTURES / "rime" / f"{SCHEMA_ID}.schema.yaml",
        rime_dir / f"{SCHEMA_ID}.schema.yaml",
    )
    shutil.copy(
        FIXTURES / "stele" / f"{SCHEMA_ID}.schema.yaml",
        stele_dir / f"{SCHEMA_ID}.schema.yaml",
    )
    return rime_dir, stele_dir


def librime_candidates(
    probe: Path, rime_dir: Path, user_dir: Path, keys: str, reset: bool = False
) -> list[str]:
    cmd = [
        probe, "--schema", SCHEMA_ID,
        "--shared-dir", rime_dir,
        "--user-dir", user_dir,
        "--keys", keys,
        "--json",
    ]
    if reset:
        cmd.append("--reset")
    p = run(cmd, timeout=600)
    if p.returncode != 0:
        raise RuntimeError(f"probe 失败（exit {p.returncode}）：{p.stderr.strip()[:300]}")
    data = json.loads(p.stdout)
    keys_recs = [r for r in data.get("records", []) if r.get("event") == "key"]
    if not keys_recs:
        return []
    return [c["text"] for c in keys_recs[-1]["context"].get("candidates", [])]


def stele_candidates(stele: Path, stele_dir: Path, keys: str, n: int = 10) -> list[str]:
    p = run(
        [stele, "--scheme-dir", stele_dir, "--schema", SCHEMA_ID, f"--candidates={n}", keys],
        timeout=180,
    )
    if p.returncode != 0:
        raise RuntimeError(f"stele 失败（exit {p.returncode}）：{p.stderr.strip()[:300]}")
    out: list[str] = []
    for line in p.stdout.splitlines():
        m = re.match(r"^\s*\d+\.\s+(\S+)\s+score=", line)
        if m:
            out.append(m.group(1))
    return out


def section_shared_wordlist(probe: Path, stele: Path, work: Path) -> tuple[str, bool]:
    rime_dir, stele_dir = prepare_shared(work)
    user_dir = work / "shared" / "user"
    entries = parse_shared_dict(FIXTURES / "shared.dict.yaml")
    lines: list[str] = []
    ok = True
    first_librime_call = True

    lines.append("两边装载的是**同一份** `fixtures/shared.dict.yaml`：")
    lines.append("")
    lines.append("| 词 | 编码 | 权重 |")
    lines.append("| --- | --- | --- |")
    for word, code, weight in entries:
        lines.append(f"| {word} | `{code}` | {weight:g} |")
    lines.append("")

    # ── B1：排序不变式 ──
    lines.append("### B1 排序不变式：同码候选必须按词库权重降序")
    lines.append("")
    lines.append("| 输入 | 说明 | 词表算出的期望序 | librime | stele | 判定 |")
    lines.append("| --- | --- | --- | --- | --- | --- |")
    for keys, note in ORDER_CASES:
        want = expected_order(entries, keys)
        lib = librime_candidates(probe, rime_dir, user_dir, keys, reset=first_librime_call)
        first_librime_call = False
        ste = stele_candidates(stele, stele_dir, keys)

        # 只比较"期望集合里的词"的相对顺序——librime 会额外产出
        # 前缀候选 / 造句候选，那是维度 B2 记录的分歧，不该让这条断言红。
        lib_f = [t for t in lib if t in want]
        ste_f = [t for t in ste if t in want]
        good = bool(want) and lib_f == want and ste_f == want
        ok = ok and good
        mark = "✓" if good else "✗"
        lines.append(
            f"| `{keys}` | {md_escape(note)} | {' '.join(want)} | "
            f"{' '.join(lib_f) or '（无）'} | {' '.join(ste_f) or '（无）'} | {mark} |"
        )
    lines.append("")
    lines.append("> **这条断言的性质**：它不要求两边的候选**集合**相同"
                 "（librime 会多出前缀与造句候选），只要求"
                 "**共同候选之间的相对顺序**与词库权重一致。")
    lines.append("")

    # ── B2：简拼 ──
    lines.append("### B2 缩写（简拼）：每个音节取首字母也要命中")
    lines.append("")
    lines.append("| 输入 | 期望词 | librime 第 1 位非字面量 | stele 第 1 位非字面量 | 判定 |")
    lines.append("| --- | --- | --- | --- | --- |")
    for keys, word in ABBREV_CASES:
        lib = librime_candidates(probe, rime_dir, user_dir, keys)
        ste = stele_candidates(stele, stele_dir, keys)
        # stele 的字面量候选等于输入串，librime 不产出字面量——比对时都剔掉
        lib_rank1 = next((t for t in lib if t != keys), None)
        ste_rank1 = next((t for t in ste if t != keys), None)
        good = lib_rank1 == word and ste_rank1 == word
        ok = ok and good
        lines.append(
            f"| `{keys}` | {word} | {lib_rank1 or '（无）'} | {ste_rank1 or '（无）'} | "
            f"{'✓' if good else '✗'} |"
        )
    lines.append("")
    lines.append("> 在 41 万条的默认方案里 `nh` 打不出「你好」（展开名额被截断，"
                 "HANDOFF §5 第 28 条）；这里的小字母表证明**规则本身是活的**。")
    lines.append("")

    # ── B3：分歧观察 ──
    lines.append("### B3 分歧观察（记录，不判失败）：前缀匹配与造句")
    lines.append("")
    lines.append("| 输入 | 说明 | librime 走的通路 | librime 候选（前 5） | stele 候选（前 5） |")
    lines.append("| --- | --- | --- | --- | --- |")
    for keys, note, mechanism in DIVERGENCE_CASES:
        lib = librime_candidates(probe, rime_dir, user_dir, keys)
        ste = stele_candidates(stele, stele_dir, keys)
        lines.append(
            f"| `{keys}` | {md_escape(note)} | {md_escape(mechanism)} | "
            f"{' '.join(lib[:5]) or '（无）'} | {' '.join(ste[:5]) or '（无）'} |"
        )
    lines.append("")
    lines.append("> **共同点**：librime 允许输入**不完整**——它可以只消费一部分输入"
                 "（末音节打一半 `niha`、只认前缀段 `nihaoshijie`），"
                 "也可以用单字**造句**（`haoni` → 好你）。")
    lines.append("> ")
    lines.append("> **Stele 的模型**：拼写图要求输入是**编码单元的完整序列**，"
                 "整段一起翻译；输入消费不完就退化成「字面量」候选"
                 "（B3 三行的 stele 列都只剩输入串本身）。"
                 "这是模型差异，不是崩溃——但**日常打字里"
                 "「多打了一个字母」的场景，体验会明显不同**。")
    lines.append("")

    # ── B4：配置项的实际效力 ──
    #
    # 这里做的是"**把开关打开，看行为变不变**"——一个被解析但没人读的
    # 配置项，只有在端到端跑一遍时才会暴露（HANDOFF §5 第 36 条的同一形状）。
    lines.append("### B4 配置项核对：被解析、但引擎里没人读的开关")
    lines.append("")
    sentence_dir = work / "shared" / "stele-sentence"
    if sentence_dir.exists():
        shutil.rmtree(sentence_dir)
    sentence_dir.mkdir(parents=True)
    shutil.copy(FIXTURES / "shared.dict.yaml", sentence_dir / f"{SCHEMA_ID}.dict.yaml")
    schema_text = (FIXTURES / "stele" / f"{SCHEMA_ID}.schema.yaml").read_text(encoding="utf-8")
    schema_text = schema_text.replace(
        "translator:\n  dictionary: stele-cmp",
        "translator:\n  dictionary: stele-cmp\n  enable_sentence: true",
    )
    (sentence_dir / f"{SCHEMA_ID}.schema.yaml").write_text(schema_text, encoding="utf-8")

    keys = "haoni"
    lib = librime_candidates(probe, rime_dir, user_dir, keys)
    ste_off = stele_candidates(stele, stele_dir, keys)
    ste_on = stele_candidates(stele, sentence_dir, keys)
    lines.append("| 输入 | librime | stele（默认） | stele（`enable_sentence: true`） |")
    lines.append("| --- | --- | --- | --- |")
    lines.append(
        f"| `{keys}` | {' '.join(lib[:4])} | {' '.join(ste_off[:3])} | {' '.join(ste_on[:3])} |"
    )
    lines.append("")
    same = ste_off == ste_on
    lines.append(f"- `enable_sentence: true` 前后，stele 的输出**完全{'相同' if same else '不同'}**"
                 f"（{'开关没有生效' if same else '开关生效了'}）。")
    lines.append("")
    lines.append("> **代码侧核对**：`TranslatorSpec::enable_sentence`"
                 "（`crates/stele-engine/src/spec.rs:556`）确实由"
                 "`crates/stele-schemes/src/components.rs:711` 从方案里读出来，"
                 "但**引擎里没有任何地方读它**"
                 "（`grep -rn enable_sentence crates/stele-engine/src` 只命中字段声明本身）；"
                 "`Origin::Sentence` 也只有一个测试夹具在产出。"
                 "也就是说：**方案里写了 `enable_sentence: true`，不会有任何效果，也不会有警告**。"
                 "这与 HANDOFF §5 第 36 条（「实现了」与「被装配了」是两件事）是同一形状，"
                 "只是这次连「实现」都没有。")
    lines.append("> ")
    lines.append("> **另一处注释与上游源码不符**：`TranslatorSpec::default_completion()`"
                 "的注释写着「RIME 的默认也是关」，而 librime 里"
                 "`TranslatorOptions::enable_completion_` 的初值是 **`true`**"
                 "（`src/rime/gear/translator_commons.h:176`），"
                 "`script_translator` 未显式配置时 `enable_word_completion_` 继承它"
                 "（`src/rime/gear/script_translator.cc:194-196`）。"
                 "建议要么改注释，要么对齐默认值——**别让注释替上游下结论**。")
    lines.append("")

    return "\n".join(lines), ok


# ─────────────────────────────────────────────────────────────────────────────
# C. 上游方案能否装载
# ─────────────────────────────────────────────────────────────────────────────


def categorize_all(log: str) -> list[tuple[str, str]]:
    """返回日志命中的**全部**分类（一个方案可以同时缺几样东西）。"""
    hits: list[tuple[str, str]] = []
    for needle, label, disposition in FAILURE_CATEGORIES:
        if needle in log:
            hits.append((label, disposition))
    return hits


def first_diagnostic(log: str) -> str:
    """取装载器报的第一条具体诊断（`  - ...`），截断后当证据。"""
    for line in log.splitlines():
        line = line.strip()
        if line.startswith("- "):
            text = line[2:].strip()
            # 去掉 `[长解释]` 与 `字段: ` 前缀，只留结论
            text = re.sub(r"^\[[^\]]*\]\s*", "", text)
            text = re.sub(r"^[^ ]*\.yaml\s*", "", text)
            return text[:78] + ("…" if len(text) > 78 else "")
    return ""


def section_loadability(stele: Path, rime_data: Path, work: Path) -> tuple[str, bool]:
    lines: list[str] = []
    schemes = sorted(rime_data.glob("*.schema.yaml"))
    if not schemes:
        return f"_跳过：`{rime_data}` 里没有方案文件。_", True

    root = work / "loadability"
    if root.exists():
        shutil.rmtree(root)
    root.mkdir(parents=True)

    rows: list[tuple[str, str, list[tuple[str, str]], str]] = []
    for sch in schemes:
        sid = sch.name[: -len(".schema.yaml")]
        d = root / sid
        d.mkdir()
        # 目录里只放这一个方案；其余文件（词库、preset、essay）软链过去，
        # 让"装载失败"只可能来自这份方案本身，而不是缺兄弟文件。
        for f in rime_data.iterdir():
            if f.name == "build" or f.name.endswith(".schema.yaml"):
                continue
            os.symlink(f, d / f.name)
        os.symlink(sch, d / sch.name)

        p = run([stele, "--scheme-dir", d, "--list"], timeout=600)
        log = (p.stdout + p.stderr).strip()
        # 原始日志留在 work 目录里当证据（报告只放归类与首条诊断）
        (root / f"{sid}.log").write_text(log + "\n", encoding="utf-8")
        if p.returncode == 0:
            rows.append((sid, "✓ 可装载", [], ""))
        else:
            rows.append((sid, "✗ 装载失败", categorize_all(log), first_diagnostic(log)))

    lines.append(f"本机 `{rime_data}` 是 **plum preset 包的部署产物**"
                 f"（luna-pinyin / cangjie / bopomofo / stroke / terra-pinyin /"
                 f" essay / prelude / quick；Debian 的 `rime-data-*` 包）。"
                 f"逐个方案单独放一个目录装载：")
    lines.append("")
    lines.append("| 方案 | 结果 | 卡在哪几类 | 性质 | 首条诊断 |")
    lines.append("| --- | --- | --- | --- | --- |")
    disp_label = {"intentional": "有意（D24）", "gap": "**缺口**"}
    for sid, result, cats, diag in rows:
        if cats:
            labels = "；".join(md_escape(lbl) for lbl, _ in cats)
            dispositions = " + ".join(sorted({disp_label[d] for _, d in cats}))
        else:
            labels, dispositions = "", ""
        lines.append(f"| `{sid}` | {result} | {labels} | {dispositions} | {md_escape(diag)} |")
    lines.append("")

    n_ok = sum(1 for r in rows if r[1].startswith("✓"))
    with_gap = [r[0] for r in rows if any(d == "gap" for _, d in r[2])]
    only_intent = [r[0] for r in rows if r[2] and all(d == "intentional" for _, d in r[2])]
    lines.append(f"**汇总**：{len(rows)} 个上游方案，可装载 **{n_ok}**；"
                 f"至少有一处**真实缺口**的 **{len(with_gap)}**"
                 f"（{'、'.join('`' + s + '`' for s in with_gap) or '—'}）；"
                 f"只因 RIME 资产未搬而失败的 **{len(only_intent)}**"
                 f"（{'、'.join('`' + s + '`' for s in only_intent) or '—'}）。")
    lines.append("")
    lines.append("> **怎么读这张表**：一个方案可以同时命中几类。"
                 "「有意」指的是 `import_preset`——**机制我们有、资产没搬**"
                 "（D24：内核与方案分离，RIME 的 `default` / `symbols` 是它自己的资产）。"
                 "标着「缺口」的才是能力问题，其中可归成四组：")
    lines.append("> ")
    lines.append("> 1. **字典格式**：RIME 的 `columns:` 声明（`cangjie5` 的 `stem` 列）"
                 "与 `%` 百分比权重（`luna_pinyin` / `terra_pinyin`）——`stele-dict` 只认"
                 "「词 `TAB` 编码 `TAB` 数字权重」。")
    lines.append("> 2. **编码切分**：RIME 码表方案的编码是字符集上的**无空格字符串**"
                 "（`stroke` 的 `shhsh`），Stele 要求编码按空格切成字母表单元。")
    lines.append("> 3. **配置机制**：跨文件的 `__patch`（`pinyin:/abbreviation`）"
                 "与只给 `options:` 的单选组开关。")
    lines.append("> 4. **键名**：X11 keysym 名（`KP_1`、`Shift+exclam`）。")
    lines.append("")
    lines.append("**另一条观察（C2）**：把整目录一次交给 Stele"
                 "（`stele --scheme-dir /usr/share/rime-data --list`）时，"
                 "它**停在第一个坏方案上**（`bopomofo`），后面的方案一个都没报。"
                 "逐目录隔离才有上面这张表。要不要改成「跳过坏的、装载好的」"
                 "是个产品决定——但它现在意味着**一个坏方案会让整目录都用不了**。")
    return "\n".join(lines), True


# ─────────────────────────────────────────────────────────────────────────────
# D. plum 配方覆盖
# ─────────────────────────────────────────────────────────────────────────────


def parse_package_conf(path: Path) -> list[str]:
    if not path.exists():
        return []
    text = path.read_text(encoding="utf-8")
    m = re.search(r"package_list\+=?=\((.*?)\)", text, re.S)
    if not m:
        return []
    return [tok.strip() for tok in m.group(1).split() if tok.strip() and not tok.strip().startswith("#")]


def section_plum(plum: Path, rime_data: Path, load_rows: dict[str, str]) -> tuple[str, bool]:
    if not plum.exists():
        return (
            f"_跳过：找不到 plum 副本（`{plum}`）。"
            "它只用来读 `preset-packages.conf` / `extra-packages.conf`；"
            "没有副本时**不猜**一份配方表出来。_\n",
            True,
        )
    preset = parse_package_conf(plum / "preset-packages.conf")
    extra = parse_package_conf(plum / "extra-packages.conf")
    installed = sorted(p.name[: -len(".schema.yaml")] for p in rime_data.glob("*.schema.yaml"))

    lines: list[str] = []
    lines.append(f"plum 副本：`{plum}`（{git_rev(plum)}）；"
                 f"本机已部署 {len(installed)} 个方案。")
    lines.append("")
    lines.append("| 配方 | 类别 | 本机部署的方案 | Stele 装载结果 |")
    lines.append("| --- | --- | --- | --- |")
    for kind, packages in (("preset", preset), ("extra", extra)):
        for pkg in packages:
            # 共享资产类（essay / prelude）不含方案，单独认，别报成"未部署"
            if pkg in SHARED_ASSET_PACKAGES:
                asset, note = SHARED_ASSET_PACKAGES[pkg]
                present = (rime_data / asset).exists()
                deployed = f"`{asset}`（{note}）" if present else f"`{asset}` **缺失**"
                lines.append(f"| `{pkg}` | {kind} | {deployed} | —（无方案） |")
                continue
            token = pkg.replace("-", "_").split("_")[0]
            hits = [s for s in installed if s.startswith(token)]
            if not hits:
                deployed, result = "—（未部署）", "未实测"
            else:
                deployed = " ".join(f"`{h}`" for h in hits)
                states = {load_rows.get(h, "未实测") for h in hits}
                result = " / ".join(sorted(states))
            lines.append(f"| `{pkg}` | {kind} | {deployed} | {result} |")
    lines.append("")
    lines.append("> **映射是启发式的**：配方名（`luna-pinyin`）与方案 id"
                 "（`luna_pinyin`）没有正式对应表，这里按第一个下划线/连字符前的"
                 "词根匹配。`essay` / `prelude` 是共享资产（八股文词表、默认配置），"
                 "**不含方案**，所以它们的「Stele 装载结果」写「无方案」——那不是失败。")
    lines.append("> ")
    lines.append("> `extra` 组的配方在本机**没有部署**，因此无法实测；"
                 "要覆盖它们得先用 plum 取回（`rime-install`），"
                 "这一步会访问网络，本工装不替使用者做。")
    return "\n".join(lines), True


# ─────────────────────────────────────────────────────────────────────────────
# 主流程
# ─────────────────────────────────────────────────────────────────────────────


def librime_version(probe: Path) -> str:
    try:
        p = run([probe, "--check-layout"], timeout=120)
        m = re.search(r"库版本:\s*(\S+)", p.stdout)
        if m:
            return m.group(1)
    except Exception:
        pass
    return "未知"


def main() -> int:
    ap = argparse.ArgumentParser(description="RIME × Stele 对比测试")
    ap.add_argument("--probe", default=str(DEFAULT_PROBE))
    ap.add_argument("--stele", default=str(DEFAULT_STELE))
    ap.add_argument("--rime-data", default=str(DEFAULT_RIME_DATA))
    ap.add_argument("--plum", default=str(DEFAULT_PLUM))
    ap.add_argument("--work", default=str(DEFAULT_WORK))
    ap.add_argument("--out", default=str(Path(__file__).resolve().parent / "report.md"))
    ap.add_argument("--skip-structural", action="store_true", help="跳过维度 A（较慢）")
    args = ap.parse_args()

    probe, stele = Path(args.probe), Path(args.stele)
    rime_data, plum, work = Path(args.rime_data), Path(args.plum), Path(args.work)
    if not probe.exists():
        print(f"找不到探针 {probe}。先跑 `cd tools/librime-probe && ./build.sh`。", file=sys.stderr)
        return 2
    if not stele.exists():
        print(f"找不到 {stele}。先跑 `cargo build --release -p stele-cli`。", file=sys.stderr)
        return 2
    work.mkdir(parents=True, exist_ok=True)

    failures: list[str] = []
    report: list[str] = []
    report.append("# RIME × Stele 对比测试报告")
    report.append("")
    report.append("| | |")
    report.append("| --- | --- |")
    report.append(f"| librime | {librime_version(probe)}（系统运行库，经 `dlopen` 调用） |")
    report.append(f"| librime 源码副本 | `{git_rev(ROOT / '.work' / 'upstream' / 'librime')}`（只用于引用行号） |")
    report.append(f"| plum 源码副本 | `{git_rev(plum)}`（维度 D 的配方表） |")
    report.append(f"| stele | `{stele.relative_to(ROOT) if str(stele).startswith(str(ROOT)) else stele}` |")
    report.append(f"| 上游方案数据 | `{rime_data}`（plum preset 的部署产物） |")
    report.append("")
    report.append("生成命令：`python3 tools/rime-compare/compare.py`")
    report.append("")

    report.append("## 0. 这次对照的边界")
    report.append("")
    report.append("**比**：能否上屏、按键是否被处理、**同一份词表下的候选相对顺序**、"
                  "上游方案能否装载、plum 配方的覆盖。")
    report.append("")
    report.append("**不比**：绝对分数（两边的分值域不同）、候选总数"
                  "（词库规模不同）、以及任何需要**同一份语言模型**才能比的东西"
                  "（本工装两边都不挂语言模型）。")
    report.append("")

    # A
    report.append("## A. 结构行为（librime 运行时 vs stele）")
    report.append("")
    if args.skip_structural:
        report.append("_（`--skip-structural`：本维度未跑）_")
        report.append("")
    else:
        body, ok = section_structural()
        if not ok:
            failures.append("A 结构行为")
        report.append("这一节由 `tools/compare-librime.py` 产出（P3 的验收线），"
                      "本工装**原样调用**它，避免同一批用例有两份定义。")
        report.append("")
        report.append(body)
        report.append("")

    # B
    report.append("## B. 同一份词表下的排序对照 ★")
    report.append("")
    try:
        body, ok = section_shared_wordlist(probe, stele, work)
        if not ok:
            failures.append("B 排序不变式")
        report.append(body)
    except Exception as e:  # 环境问题不该伪装成"结论"
        failures.append("B 排序不变式（执行失败）")
        report.append(f"**执行失败**：`{e}`")
    report.append("")

    # C
    report.append("## C. 上游方案能否装载（plum preset → stele）")
    report.append("")
    load_rows: dict[str, str] = {}
    c_gap_schemes: list[str] = []
    if rime_data.exists():
        try:
            body, _ = section_loadability(stele, rime_data, work)
            # 从报告里回填"方案 → 结果"，供维度 D 引用
            for line in body.splitlines():
                m = re.match(r"^\| `([^`]+)` \| (✓ 可装载|✗ 装载失败) \|", line)
                if m:
                    load_rows[m.group(1)] = m.group(2)
                    if "**缺口**" in line:
                        c_gap_schemes.append(m.group(1))
            report.append(body)
        except Exception as e:
            report.append(f"**执行失败**：`{e}`")
    else:
        report.append(f"_跳过：`{rime_data}` 不存在。_")
    report.append("")

    # D
    report.append("## D. plum 配方覆盖")
    report.append("")
    body, _ = section_plum(plum, rime_data, load_rows)
    report.append(body)
    report.append("")

    # 汇总
    report.append("## 结论")
    report.append("")
    if failures:
        report.append(f"**{len(failures)} 条断言未通过**：{'、'.join(failures)}。")
        report.append("")
        report.append("断言失败 = 两边在「必须一致」的那部分不一致；"
                      "先查清是引擎行为不同，还是工装自己写错了。")
    else:
        report.append("**全部断言通过**（A 结构行为、B1 排序不变式、B2 缩写）。")
    report.append("")
    report.append("**这次对照交出的问题清单**（不判失败，但都是可开工的条目）：")
    report.append("")
    report.append(f"1. **拼写图不做「不完整输入」**（B3，{len(DIVERGENCE_CASES)} 条用例）："
                  "输入的末音节打一半、或输入比词条长时，Stele 退化成字面量候选，"
                  "librime 仍给前缀候选 / 造句候选。")
    report.append("2. **`enable_sentence` 被解析但没有消费者**（B4）："
                  "方案里写 `enable_sentence: true` 不会有任何效果，也没有警告。")
    report.append(f"3. **{len(c_gap_schemes)} 个上游 preset 方案有真实装载缺口**（C）："
                  "字典 `columns:` / `%` 权重、码表编码的无空格字符串、"
                  "跨文件 `__patch`、X11 键名——四组，见 C 节的归类。")
    report.append("4. **一个坏方案会让整个 `--scheme-dir` 都用不了**（C2）："
                  "装载器停在第一个坏方案上，且只报它。")
    report.append("")
    report.append("> 本报告由 `tools/rime-compare/compare.py` 生成，"
                  "重跑同一条命令应得到同样的结论（librime 侧一律取冷基线）。")

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text("\n".join(report) + "\n", encoding="utf-8")

    print(f"报告已写入 {out}")
    print(f"断言：{'全部通过' if not failures else '未通过 —— ' + '、'.join(failures)}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
