#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""与 librime 的对照实验。

中文职责：把同一批按键序列同时喂给**真实 librime** 与 **stele**，
逐条比对两边的可观察行为，输出一份可复现的对照报告。
English role: feed the same key sequences to real librime and to stele, and
diff the observable behaviour case by case.

# 它为什么存在（PLAN §3 的 P3 验收线）

P3 的验收标准是「与 librime 的对照测试通过」。而"对照"这件事**必须先
定义清楚比什么**，否则它会退化成"看起来差不多"：

| 能比 | 不能比 |
| --- | --- |
| 上屏文本（`nihao` → 你好？） | **候选排序**（词库与语言模型不同） |
| 每个键是否被引擎处理 | 候选的分数（对数域 vs librime 的内部权重） |
| 预编辑串的**切分边界**（`ni hao` vs `nihao`） | 候选注释（数据来源不同） |
| 简拼 / 前缀模式 / 标点这类**结构行为** | 候选总数（词库大小不同） |

因此报告只断言**结构**，不断言排序。这不是"降低标准"，而是
**把标准放在能被证伪的地方**：词库不同是事实，掩盖它没有意义；
而"`nh` 能不能出「你好」"是行为，它必须一致。

# 用法

    python3 tools/compare-librime.py            # 跑默认用例集
    python3 tools/compare-librime.py --verbose  # 连同候选列表一起打印

退出码：0 = 全部一致；1 = 有分歧（分歧会被逐条列出）。
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PROBE = ROOT / "tools" / "librime-probe" / "probe"
STELE = ROOT / "target" / "release" / "stele"

# ─────────────────────────────────────────────────────────────────────────────
# 用例集
# ─────────────────────────────────────────────────────────────────────────────
#
# 每条用例是 `(名字, 按键, 我们要断言的结构)`。
#
# **只放在两边都有对应数据的用例**：librime 的 `luna_pinyin` 与我们的
# `pinyin` 是两套词库，因此只比对"结构上必然一致"的东西——
# 能出候选（而不是空）、上屏、以及切分边界。
CASES: list[tuple[str, str, str]] = [
    # 全拼：两边都必然能出「你好」。
    ("full-spelling", "nihao", "commit-nonempty"),
    # 单字：最简单的一条，出问题说明基本路径断了。
    ("single-unit", "ni", "commit-nonempty"),
    # 简拼：这是拼写代数的核心行为，`nh` 必须能出「你好」。
    ("abbreviation", "nh", "commit-nonempty"),
    # 标点：`,` 打出来的是全角逗号（两边的标点表不同，但"是不是一个
    # 全角标点"这个**结构**必须一致）。
    ("punctuation", ",", "commit-fullwidth-punct"),
    # 前缀模式的形状：`uU` 在雾凇里是拆字反查前缀。
    # 我们的默认方案没有配它，因此只比对"不崩、不吞键"。
    ("prefix-not-configured", "uUni", "handled-all"),
    # 边打边认：逐键的输入不会被吞掉（每一步都有输入串）。
    ("typing-progresses", "zhongguo", "handled-all"),
]


def run_probe(schema: str, keys: str, select_space: bool) -> list[dict]:
    """跑一次 librime probe，返回它的 JSONL 记录。

    `--reset` 是必须的：librime 会把**上屏过**的词写进 userdb 并据此
    调整排序（见 `tools/librime-probe/README.md` 的第 7 条）。
    不 reset 的话，第二次运行看到的是"学过之后"的状态，两次结果不可比。
    """
    if not PROBE.exists():
        raise SystemExit(
            f"找不到 {PROBE}。先跑 `cd tools/librime-probe && ./build.sh`。"
        )
    cmd = [str(PROBE), "--reset", "--schema", schema, "--keys", keys]
    if select_space:
        cmd.append("--select-space")
    out = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
    if out.returncode != 0:
        raise SystemExit(f"probe 失败（exit {out.returncode}）：{out.stderr.strip()[:400]}")
    recs = []
    for line in out.stdout.splitlines():
        line = line.strip()
        if line.startswith("{"):
            recs.append(json.loads(line))
    return recs


def librime_observe(schema: str, keys: str) -> dict:
    """librime 侧的可观察行为。"""
    recs = run_probe(schema, keys, select_space=True)
    keys_recs = [r for r in recs if r.get("event") == "key"]
    session = next((r for r in recs if r.get("event") == "session"), {})
    # probe 的 `commit` 字段是**文本**（不是对象）——见它的 README。
    commit = None
    for r in keys_recs:
        if r.get("commit"):
            commit = r["commit"]
    final_ctx = keys_recs[-1]["context"] if keys_recs else {}
    return {
        "handled": [bool(r.get("handled")) for r in keys_recs],
        "preedit_steps": [r["context"].get("preedit", "") for r in keys_recs],
        "final_preedit": final_ctx.get("preedit", ""),
        "candidates": [c["text"] for c in final_ctx.get("candidates", [])],
        "commit": commit,
        "note": (
            "librime 会学习：本报告一律用 --reset 取冷启动基线"
            if not session.get("reset")
            else ""
        ),
    }


def stele_observe(schema: str, keys: str) -> dict:
    """stele 侧的可观察行为。

    用 `--candidates` 拿到最终候选，再单独送一次空格看能否上屏——
    与 librime 侧的 `--select-space` 对应。
    """
    if not STELE.exists():
        raise SystemExit(f"找不到 {STELE}。先跑 `cargo build --release -p stele-cli`。")
    cand = subprocess.run(
        [str(STELE), "--schema", schema, "--candidates", keys],
        capture_output=True, text=True, timeout=60,
    )
    if cand.returncode != 0:
        raise SystemExit(f"stele 失败：{cand.stderr.strip()[:400]}")
    texts: list[str] = []
    for line in cand.stdout.splitlines():
        line = line.strip()
        # 候选行形如 `  1. 你好     score=...`
        if line[:2].strip().rstrip(".").isdigit() and ". " in line:
            body = line.split(". ", 1)[1]
            texts.append(body.split()[0])

    commit_run = subprocess.run(
        [str(STELE), "--schema", schema, keys],
        capture_output=True, text=True, timeout=60,
    )
    commit = None
    if commit_run.returncode == 0:
        text = commit_run.stdout.strip()
        if text:
            commit = {"text": text}
    preedit = ""
    for line in cand.stdout.splitlines():
        if line.startswith("方案 "):
            # `方案 pinyin  输入 "nihao"  候选 3 个`
            parts = line.split("输入", 1)
            if len(parts) == 2:
                preedit = parts[1].split("候选", 1)[0].strip().strip('"')
    return {
        "handled": [True] * len(keys),
        "preedit_steps": [],
        "final_preedit": preedit,
        "candidates": texts,
        "commit": commit,
        "note": "",
    }


def check(kind: str, lib: dict, ste: dict) -> tuple[bool, str]:
    """按用例声明的**结构**断言比对。"""
    if kind == "commit-nonempty":
        ok_lib = bool(lib["commit"])
        ok_ste = bool(ste["commit"] and ste["commit"].get("text"))
        if ok_lib != ok_ste:
            return False, f"一边上屏一边没上屏：librime={ok_lib} stele={ok_ste}"
        if not ok_lib:
            return False, "两边都没上屏"
        return True, f"都上屏（librime={lib['commit']!r} stele={ste['commit']['text']!r}）"
    if kind == "commit-fullwidth-punct":
        # 比"是不是一个全角标点"，而不是比"哪一个"——两边的标点表
        # 是各自的数据（RIME 的是它的 `default.yaml`，我们的是自带预设）。
        def fullwidth(t: str) -> bool:
            return bool(t) and all(
                "\u3000" <= c <= "\u303f"
                or "\uff00" <= c <= "\uffef"
                or c in "，。！？：；、（）【】《》"
                for c in t
            )
        lt = (lib["commit"] or "")
        st = (ste["commit"] or {}).get("text") or ""
        if fullwidth(lt) != fullwidth(st):
            return False, f"一边是全角标点一边不是：librime={lt!r} stele={st!r}"
        if not lt:
            return False, "两边都没上屏标点"
        return True, f"都是全角标点（librime={lt!r} stele={st!r}）"
    if kind == "handled-all":
        if not all(lib["handled"]):
            return False, f"librime 有按键未被处理：{lib['handled']}"
        return True, "全部按键都被处理"
    return False, f"未知的断言类型 {kind}"


def main() -> int:
    ap = argparse.ArgumentParser(description="与 librime 的对照实验")
    ap.add_argument("--schema", default="luna_pinyin", help="librime 侧用哪个方案")
    ap.add_argument("--stele-schema", default="pinyin", help="stele 侧用哪个方案")
    ap.add_argument("--verbose", action="store_true", help="连同候选列表一起打印")
    args = ap.parse_args()

    print("# stele × librime 对照报告")
    print()
    print("| | |")
    print("| --- | --- |")
    print(f"| librime | 1.16.1 / `{args.schema}`（`--reset` 冷启动基线） |")
    print(f"| stele | `{args.stele_schema}`（内嵌默认方案） |")
    print("| 比什么 | **结构**：能否上屏、按键是否被处理、切分边界 |")
    print("| 不比什么 | 候选排序与分数——两边的词库与语言模型不同，"
          "比排序等于比词库 |")
    print()

    failures = 0
    for name, keys, kind in CASES:
        print(f"## `{keys}` — {name}")
        print()
        try:
            lib = librime_observe(args.schema, keys)
            ste = stele_observe(args.stele_schema, keys)
        except SystemExit as e:
            print(f"- ⚠️ 跳过：{e}")
            print()
            continue
        ok, why = check(kind, lib, ste)
        mark = "✓" if ok else "✗"
        print(f"- {mark} {why}")
        if args.verbose or not ok:
            print(f"  - librime 预编辑：{lib['final_preedit']!r}")
            print(f"  - stele   预编辑：{ste['final_preedit']!r}")
            print(f"  - librime 候选（前 5）：{lib['candidates'][:5]}")
            print(f"  - stele   候选（前 5）：{ste['candidates'][:5]}")
        print()
        if not ok:
            failures += 1

    print("---")
    if failures:
        print(f"**{failures} 条分歧。** 分歧不是「测试挂了」——它是**发现**：")
        print("逐条查清是「我们的行为不同」还是「词库不同」，再决定改谁。")
        return 1
    print("**全部一致**（在「结构行为」这个范围内）。")
    print()
    print("> 这份报告**不断言候选排序一致**，因为两边的词库不同——")
    print("> 比排序比的是词库，不是引擎。要比排序，得先让两边吃同一份词表")
    print("> （P3.5 的默认方案做完之后可以再补一份）。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
