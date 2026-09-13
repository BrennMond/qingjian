#!/usr/bin/env bash
# 取回**干净来源**的词表与 OpenCC 数据（P3.5；阶段 4 / 审计 J2 改造为固定 revision）。
#
# 为什么要有这个脚本（而不是把数据放进仓库）：
#   雾凇那 44 MB 词表里最大的两块授权不明或明确受限（PLAN §10）；
#   我们只取**授权明确或已知边界**的那几份，而它们**不进仓库**——
#   用户在自己的机器上取一次，属于个人使用。
#
# 取回来的东西全部落在 `schemes/stele-default/build/`（.gitignore 已排除）。
#
# ── 与旧版的区别（阶段 4 / 审计 J2.2）─────────────────────────────────
#
#   1. **不再用浮动 `main` / `master`**：每条 URL 都固定到 commit SHA，
#      清单位于**已跟踪的** `tools/sources.lock`——克隆仓库就能看到
#      "这份数据取自哪个 revision、期望的 sha256 是多少"。
#   2. 旧版把 sha256 记在 `build/sources.lock`，而 `build/` 是 gitignore 的：
#      那份记录**只有跑过一次的人**才看得到，无法作为可复现输入。
#      现在权威清单是 `tools/sources.lock`；本脚本另写一份
#      `build/fetched.lock` 作为**本地实取记录**（仍是输出，不是输入）。
#   3. 每条记录的许可是逐条写死的（`MIT` / `Apache-2.0` / `GPL-3.0-only`），
#      不再用一句"都是 MIT / Apache"概括——emoji/* 来自 rime-ice，
#      是 **GPL-3.0-only**，旧版标成 Apache-2.0 是错的。
#
# 用法：
#   bash tools/fetch-sources.sh                    # 按锁文件取回并校验
#   bash tools/fetch-sources.sh --force            # 忽略已有文件，重新取
#   bash tools/fetch-sources.sh --allow-unverified # 允许清单里的 TODO-UNVERIFIED
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="$ROOT/schemes/stele-default/build"
LOCK="$ROOT/tools/sources.lock"
FETCHED="$DEST/fetched.lock"
FORCE=0
ALLOW_UNVERIFIED=0

while [ $# -gt 0 ]; do
  case "$1" in
    --force) FORCE=1 ;;
    --allow-unverified) ALLOW_UNVERIFIED=1 ;;
    -h|--help)
      sed -n '1,40p' "${BASH_SOURCE[0]}"
      exit 0
      ;;
    *)
      echo "✗ 不认识的参数：$1（见 --help）" >&2
      exit 2
      ;;
  esac
  shift
done

[ -f "$LOCK" ] || { echo "✗ 找不到锁文件 $LOCK（它是仓库里的可复现输入）" >&2; exit 1; }
command -v sha256sum >/dev/null 2>&1 || { echo "✗ 没有 sha256sum，无法校验（本脚本依赖 coreutils）" >&2; exit 1; }

mkdir -p "$DEST"

download() {  # url dest
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 3 -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$2" "$1"
  else
    echo "✗ 既没有 curl 也没有 wget，无法取回数据" >&2
    return 1
  fi
}

hash_of() { sha256sum "$1" | cut -d' ' -f1; }

: > "$FETCHED.tmp"
fail=0
unverified=0
count=0

# 字段：目标路径|URL|revision|许可|版权|sha256|说明
while IFS='|' read -r path url rev spdx copyright want desc; do
  case "$path" in
    ''|'#'*) continue ;;
  esac
  count=$((count + 1))
  out="$DEST/$path"
  mkdir -p "$(dirname "$out")"

  fresh=0
  if [ -f "$out" ] && [ "$FORCE" -eq 0 ] && [ "$want" != "TODO-UNVERIFIED" ]; then
    if [ "$(hash_of "$out")" = "$want" ]; then
      fresh=1
      echo "· 已存在且校验通过 $path（跳过下载）"
    fi
  fi

  if [ "$fresh" -eq 0 ]; then
    echo "→ 取回 $path  @ ${rev:0:12}…"
    if ! download "$url" "$out.part.$$"; then
      echo "✗ 下载失败：$url" >&2
      rm -f "$out.part.$$"
      fail=1
      continue
    fi
    mv "$out.part.$$" "$out"
  fi

  got="$(hash_of "$out")"
  if [ "$want" = "TODO-UNVERIFIED" ]; then
    echo "⚠ $path 的 sha256 尚未核对（清单里是 TODO-UNVERIFIED）。" >&2
    echo "   本次实测：$got" >&2
    echo "   请用实测值更新 $LOCK 的字段 6（这一步必须由人确认）。" >&2
    unverified=$((unverified + 1))
  elif [ "$got" != "$want" ]; then
    echo "✗ $path 的内容与锁文件不一致。" >&2
    echo "  期望 $want" >&2
    echo "  实得 $got" >&2
    echo "  这两个值对应的 URL 是固定的（$url）。不一致意味着：" >&2
    echo "  · 上游改写了历史，或" >&2
    echo "  · 我们的链接 / revision 写错了。" >&2
    echo "  词库内容变了，权重、音节表、候选排序都会跟着变——" >&2
    echo "  必须有人看过这次差异，不能静默采用。" >&2
    fail=1
  fi

  # 本地实取记录：输出，不是输入。字段与 tools/sources.lock 对齐，便于 diff。
  printf '%s  %s  %s  %s  %s\n' "$got" "$path" "$spdx" "$rev" "$url" >> "$FETCHED.tmp"
done < "$LOCK"

if [ "$fail" -ne 0 ]; then
  rm -f "$FETCHED.tmp"
  echo "✗ 有源数据未通过校验，未写本地记录。已下载的文件保留在 $DEST（可人工检查）。" >&2
  exit 1
fi

mv "$FETCHED.tmp" "$FETCHED"

# ── 生成 OpenCC 配置（数据已就位，只差把它组装成一份 config）─────────
#
# 这份 s2t.json 是**我们写的**（20 行不到），描述"用哪几张表、按什么顺序
# 转换"。它与上游 OpenCC 的 config 形状一致——但上游用的是相对文件名，
# 而我们的表在 `build/opencc/` 下，所以显式写清楚比让装载器去猜好。
# 上游 rime-ice 的 emoji.json 由它所依赖的 emoji.txt / others.txt 直接使用，
# 本脚本不改写它。
mkdir -p "$DEST/emoji" "$DEST/opencc"

cat > "$DEST/opencc/s2t.json" <<'JSON'
{
	"name": "Simplified to Traditional",
	"segmentation": {
		"type": "mmseg",
		"dict": { "type": "text", "file": "STPhrases.txt" }
	},
	"conversion_chain": [
		{
			"dict": {
				"type": "group",
				"dicts": [
					{ "type": "text", "file": "STPhrases.txt" },
					{ "type": "text", "file": "STCharacters.txt" }
				]
			}
		}
	]
}
JSON

echo "· 生成 $DEST/opencc/s2t.json（用 STPhrases + STCharacters，词组优先；本项目自撰）"

echo
echo "✓ 数据就位：$DEST（$count 条，全部按固定 revision 校验）"
echo "  权威清单：$LOCK"
echo "  本地实取记录：$FETCHED"
if [ "$unverified" -ne 0 ]; then
  echo "⚠ 其中 $unverified 条的 sha256 仍是 TODO-UNVERIFIED（见上面的提示）。"
  if [ "$ALLOW_UNVERIFIED" -ne 1 ]; then
    echo "✗ 清单未核对完；如确要继续，请显式加 --allow-unverified。" >&2
    exit 1
  fi
fi
echo
echo "下一步："
echo "  cargo run --manifest-path tools/wordlist-gen/Cargo.toml -- \\"
echo "      --sources $DEST --out $ROOT/schemes/stele-default"
