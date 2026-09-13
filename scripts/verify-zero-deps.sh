#!/usr/bin/env bash
# 门禁：内核 crate 必须保持**零第三方依赖**（PLAN D9）。
#
# 范围是 **`stele-core` 与 `stele-engine`**——PLAN §2.1 把这两个合称"内核"，
# HANDOFF §3 也这么写。本脚本原先只检查 `stele-core`：
# **文档说的范围比脚本宽**，那是一个静默的缺口（"stele-engine 里塞进依赖"
# 不会被任何门禁拦下）。现在两个都查。
#
# "零第三方"的准确含义是：**允许依赖同一个 workspace 里的其他 crate**
# （`stele-engine` 依赖 `stele-core` 是设计的一部分），
# **不允许任何走 registry 的依赖**。
# 判据因此落在"这一条依赖有没有 `path`"上——有 path 且指向仓库内部的
# 目录才算内部依赖；写成版本号的（`foo = "1.0"`）一律算第三方。
#
# 为什么用脚本而不是靠自觉：零依赖是供应链安全的锚点，也是"平台无关代码能纯
# `cargo test`"的前提。它很容易被一次图省事的 `cargo add` 破坏。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# 内核 crate。加进来之前请先读 PLAN §2.1 对"内核"的定义。
KERNEL_CRATES=(stele-core stele-engine)

failed=0

for crate in "${KERNEL_CRATES[@]}"; do
  MANIFEST="$ROOT/crates/$crate/Cargo.toml"
  [ -f "$MANIFEST" ] || { echo "✗ 找不到 $MANIFEST"; exit 1; }

  # 提取 [dependencies] 段（到下一个 [ 开头的行或文件末尾），去掉注释与空行。
  deps="$(awk '
    /^\[dependencies\]/ { in_deps = 1; next }
    /^\[/ { in_deps = 0 }
    in_deps { print }
  ' "$MANIFEST" | sed 's/#.*//' | sed '/^[[:space:]]*$/d')"

  bad=""
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    dep_name="${line%%=*}"
    dep_name="$(printf '%s' "$dep_name" | tr -d '[:space:]')"

    # 内部依赖：必须显式写 `path`，且目标目录真的在仓库里。
    if printf '%s' "$line" | grep -q 'path[[:space:]]*='; then
      dep_path="$(printf '%s' "$line" | sed -n 's/.*path[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p')"
      target="$(cd "$(dirname "$MANIFEST")" && cd "$dep_path" 2>/dev/null && pwd || true)"
      case "$target" in
        "$ROOT"/crates/*) continue ;;
        *)
          bad="$bad
  $dep_name —— path 指向仓库之外（$dep_path）"
          continue
          ;;
      esac
    fi

    bad="$bad
  $dep_name —— 没有 path，即来自 registry（第三方依赖）"
  done <<< "$deps"

  if [ -n "$bad" ]; then
    echo "✗ $crate 不再零第三方依赖："
    printf '%s\n' "$bad"
    echo
    echo "  若确实需要新依赖，请先在 PLAN.md 的决策记录里说明理由，"
    echo "  并把它加进 scripts/deps-allowlist.txt——"
    echo "  这条约束有意设计成需要显式推翻，而不是可以顺手绕过。"
    failed=1
    continue
  fi

  echo "✓ $crate 零第三方依赖"
done

if [ "$failed" -ne 0 ]; then
  exit 1
fi

echo "✓ verify-zero-deps: 内核（${KERNEL_CRATES[*]}）保持零第三方依赖"
