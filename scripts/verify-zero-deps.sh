#!/usr/bin/env bash
# 门禁：stele-core 必须保持**零第三方依赖**（PLAN D9）。
#
# 为什么用脚本而不是靠自觉：零依赖是供应链安全的锚点，也是"平台无关代码能纯
# `cargo test`"的前提。它很容易被一次图省事的 `cargo add` 破坏。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/crates/stele-core/Cargo.toml"

[ -f "$MANIFEST" ] || { echo "✗ 找不到 $MANIFEST"; exit 1; }

# 提取 [dependencies] 段（到下一个 [ 开头的行或文件末尾），去掉空行与注释。
deps=$(awk '
  /^\[dependencies\]/ { in_deps = 1; next }
  /^\[/ { in_deps = 0 }
  in_deps { print }
' "$MANIFEST" | sed 's/#.*//' | tr -d '[:space:]')

if [ -n "$deps" ]; then
  echo "✗ stele-core 不再零依赖，发现："
  awk '/^\[dependencies\]/ { in_deps = 1; next } /^\[/ { in_deps = 0 } in_deps { print }' "$MANIFEST"
  echo
  echo "  若确实需要新依赖，请先在 PLAN.md 的决策记录里说明理由——"
  echo "  这条约束有意设计成需要显式推翻，而不是可以顺手绕过。"
  exit 1
fi

echo "✓ verify-zero-deps: stele-core 保持零依赖"
