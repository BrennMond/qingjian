#!/usr/bin/env bash
# 门禁：依赖许可审查（PLAN §4.8 / D9）。
#
# # 为什么必须有这条
#
# PLAN D9 允许"周边 crate 使用**受审**依赖"，但"受审"如果只是一个形容词，
# D9 就只是一句自我声明。这条门禁把"受审"变成一个**必须显式推翻**的动作：
# 任何来自 registry 的包，都要在 deps-allowlist.txt 里逐条写下名字、版本、
# 许可与理由——写不下来，就说明还没人真正审过它。
#
# # 它检查什么
#
#   1. Cargo.lock 里每个来自 registry 的包都在白名单里，且版本一致；
#   2. 白名单里没有陈旧条目（已不在 Cargo.lock 里的名字）；
#   3. 同一个 crate 不出现在两个版本上（重复依赖检查）。
#
# # 它不检查什么（诚实交代）
#
# **它不自己去读每个包的 LICENSE 文件**。要做到那一步需要把依赖取回来、
# 或者引入 cargo-deny 这一整套工具链，而两者都与"零依赖 + 离线可跑"相冲。
# 因此本门禁的判据是"**这个包有没有被人显式审过**"，而不是"它的许可对不对"——
# 审的那一步仍然是人的责任，门禁负责让那一步无法被跳过。
#
# # 反向验证过（"一个不会失败的检查等于没有检查"）
#
# 在 Cargo.lock 里临时插入一个假的 registry 包 `evil-crate 9.9.9`，
# 本脚本报出"未受审的依赖"并以 1 退出；删除后恢复通过。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOCK="$ROOT/Cargo.lock"
ALLOW="$ROOT/scripts/deps-allowlist.txt"

[ -f "$LOCK" ] || { echo "✗ 找不到 $LOCK（依赖审查需要锁定版本）"; exit 1; }
[ -f "$ALLOW" ] || { echo "✗ 找不到 $ALLOW"; exit 1; }

# ── 1. 从 Cargo.lock 抽出 (名字, 版本, 是否来自 registry) ────────────────
# workspace 成员没有 `source` 字段；registry 包才有。我们只审后者。
pkgs="$(awk '
  /^\[\[package\]\]/ {
    if (name != "") print name "\t" version "\t" (src ? "registry" : "local")
    name = ""; version = ""; src = 0
    next
  }
  /^name = /    { name = substr($0, 9);  gsub(/"/, "", name);    next }
  /^version = / { version = substr($0, 12); gsub(/"/, "", version); next }
  /^source = /  { src = 1; next }
  END { if (name != "") print name "\t" version "\t" (src ? "registry" : "local") }
' "$LOCK")"

# ── 2. 重复依赖检查：同一个 crate 两个版本 = 失败 ───────────────────────
dups="$(printf '%s\n' "$pkgs" | awk -F'\t' '{ print $1 }' | sort | uniq -d)"
if [ -n "$dups" ]; then
  echo "✗ 同一个 crate 有多个版本（重复依赖）："
  printf '  %s\n' $dups
  echo
  echo "  重复版本会让二进制变大、许可组合变复杂。请先统一版本，"
  echo "  或在 deps-allowlist.txt 里说明为什么无法避免。"
  exit 1
fi

# ── 3. 逐个 registry 包查白名单 ────────────────────────────────────────
# 白名单的一行：crate  version  license  理由（理由占其余全部字段，可含空格）。
missing=""
while IFS=$'\t' read -r name version kind; do
  [ "$kind" = "registry" ] || continue
  found="$(awk -v n="$name" -v v="$version" '
    /^[[:space:]]*#/ { next }
    NF < 3 { next }
    $1 == n {
      if ($2 == v) { print "ok"; exit }
      print "version:" $2; exit
    }
  ' "$ALLOW")"
  case "$found" in
    ok) ;;
    version:*)
      missing="$missing
  $name $version —— 白名单里记的是版本 ${found#version:}，与锁文件不一致"
      ;;
    *)
      missing="$missing
  $name $version —— 完全没有受审记录"
      ;;
  esac
done <<< "$pkgs"

if [ -n "$missing" ]; then
  echo "✗ 发现未受审的第三方依赖："
  printf '%s\n' "$missing"
  echo
  echo "  引入依赖前请先读 PLAN §10 的许可边界（不是\"MIT 就能进\"），"
  echo "  然后在 scripts/deps-allowlist.txt 里写下一行："
  echo "      <crate>  <version>  <SPDX 许可>  <理由>"
  echo "  这一步无法被跳过——这正是 D9 里\"受审\"二字的执行方式。"
  exit 1
fi

# ── 4. 陈旧条目：白名单里写了、但锁文件里已经没有了 ─────────────────────
stale="$(awk '
  /^[[:space:]]*#/ { next }
  NF < 3 { next }
  { print $1 "\t" $2 }
' "$ALLOW" | while IFS=$'\t' read -r n v; do
  if ! printf '%s\n' "$pkgs" | awk -F'\t' -v n="$n" -v v="$v" '$1 == n && $2 == v { hit = 1 } END { exit !hit }'; then
    echo "  $n $v"
  fi
done)"

if [ -n "$stale" ]; then
  echo "✗ 白名单里有已不再使用的条目（陈旧受审记录）："
  printf '%s\n' "$stale"
  echo
  echo "  留着它们会让名单变成历史垃圾堆，让\"审过哪些\"变得不可回答。"
  exit 1
fi

count="$(printf '%s\n' "$pkgs" | awk -F'\t' '$3 == "registry"' | wc -l | tr -d '[:space:]')"
echo "✓ verify-deps: $count 个 registry 依赖，全部有受审记录（重复依赖 0）"
