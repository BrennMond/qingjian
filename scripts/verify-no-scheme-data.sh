#!/usr/bin/env bash
# 门禁：内核 crate 里不得内置任何词表 / 音节表 / 方案数据（PLAN D20、D24）。
#
# 内核只提供机制；一切输入法的"个性"（有哪些编码单元、词条、权重）
# 都是方案资产，放在 schemes/ 下，与内核解耦。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
status=0

CORE_DIRS=("$ROOT/crates/qingjian-core" "$ROOT/crates/qingjian-engine")

# ── 1) 内核 crate 里只允许"源码 + 它的元数据"，其它一律拒绝 ──────────
#
# **为什么用白名单而不是黑名单**（2026-09-14 修正）：第一版列的是
# yaml/yml/dict/txt/json 五种扩展名，于是
# `crates/qingjian-engine/tests/oracle/*.lua`——上游 GPL-3.0 代码的副本，
# 正是本门禁要拦的那类东西——从扩展名缝里漏了过去，一直存活到仓库公开
# 之后才在别处被发现。**黑名单的失效方式是"漏"，白名单的失效方式是"吵"。**
# 对一个"不许出错"的检查，只有后者是可接受的方向。
ALLOWED_RE='(\.rs|/Cargo\.toml|/README\.md)$'
for dir in "${CORE_DIRS[@]}"; do
  [ -d "$dir" ] || continue
  hits="$(find "$dir" -type f -not -path '*/target/*' 2>/dev/null \
    | grep -Ev "$ALLOWED_RE" || true)"
  if [ -n "$hits" ]; then
    echo "✗ 内核 crate 里出现了源码与元数据之外的文件（数据 / 脚本 / 上游副本）："
    echo "$hits"
    echo "  处理：测试对照数据移到 tools/oracle/；方案数据放进 schemes/。"
    status=1
  fi
done

# ── 2) 内核 crate 不得**依赖** schemes/ ───────────────────────────────
#
# 只看两处真正构成依赖的地方：
#   (a) Cargo.toml 里的引用
#   (b) Rust 源码里的 include! / include_str! / include_bytes!
# 刻意**不**扫文档注释——注释里举例说明路径是允许的（而且确实有用）。
for dir in "${CORE_DIRS[@]}"; do
  [ -d "$dir" ] || continue

  hits="$(grep -rIn 'schemes' "$dir" --include='Cargo.toml' 2>/dev/null || true)"
  if [ -n "$hits" ]; then
    echo "✗ 内核 crate 的 Cargo.toml 引用了 schemes/："
    echo "$hits"
    status=1
  fi

  hits="$(grep -rInE 'include(_str|_bytes)?![^)]*schemes' "$dir" --include='*.rs' 2>/dev/null || true)"
  if [ -n "$hits" ]; then
    echo "✗ 内核 crate 用 include! 嵌入了方案数据："
    echo "$hits"
    status=1
  fi
done

# ── 3) 方案之间不得互相依赖（每个方案都必须能独立装配）────────────────
if [ -d "$ROOT/schemes" ]; then
  hits="$(grep -rInE '\$ref:[[:space:]]*"?schemes/' "$ROOT/schemes" 2>/dev/null || true)"
  if [ -n "$hits" ]; then
    echo "✗ 方案之间出现了互相引用："
    echo "$hits"
    status=1
  fi
fi

if [ "$status" -eq 0 ]; then
  echo "✓ verify-no-scheme-data: 内核与方案数据保持分离"
fi
exit "$status"
