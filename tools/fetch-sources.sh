#!/usr/bin/env bash
# 取回**干净来源**的词表与 OpenCC 数据（P3.5）。
#
# 为什么要有这个脚本（而不是把数据放进仓库）：
#   雾凇那 44 MB 词表里最大的两块授权不明或明确受限（PLAN §10）；
#   我们只取**授权明确、可分发**的那几份，而它们**不进仓库**——
#   用户在自己的机器上取一次，属于个人使用。
#
# 取回来的东西全部落在 `schemes/stele-default/build/`（.gitignore 已排除）。
# 每份文件的 sha256 记在 `build/sources.lock`，下次运行时校验：
#   · 一致  → 跳过（不重复下载）
#   · 不一致 → 报错并停下（**上游改了数据**是一件必须被看见的事：
#     词库内容变了，权重、音节表、乃至候选排序都会跟着变）
#
# 用法：
#   bash tools/fetch-sources.sh              # 取回并校验
#   bash tools/fetch-sources.sh --force      # 忽略已有文件，重新取
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="$ROOT/schemes/stele-default/build"
LOCK="$DEST/sources.lock"
FORCE=0
[ "${1:-}" = "--force" ] && FORCE=1

mkdir -p "$DEST"

# ── 清单：`名字|URL|许可|来源说明` ───────────────────────────────────
#
# **每条都必须有明确的许可**。这三份都是可以随作品分发的
# （pinyin-data / THUOCL 是 MIT，OpenCC 的数据是 Apache-2.0），
# 但我们仍然把"取回"放在用户机器上——理由见文件头。
SOURCES=(
  "pinyin.txt|https://raw.githubusercontent.com/mozillazg/pinyin-data/master/pinyin.txt|MIT|汉字→拼音（41k 字，首要读音在前）"
  "THUOCL_IT.txt|https://raw.githubusercontent.com/thunlp/THUOCL/master/data/THUOCL_IT.txt|MIT|THUOCL 信息技术词表（带词频）"
  "THUOCL_law.txt|https://raw.githubusercontent.com/thunlp/THUOCL/master/data/THUOCL_law.txt|MIT|THUOCL 法律词表"
  "THUOCL_medical.txt|https://raw.githubusercontent.com/thunlp/THUOCL/master/data/THUOCL_medical.txt|MIT|THUOCL 医学词表"
  "THUOCL_car.txt|https://raw.githubusercontent.com/thunlp/THUOCL/master/data/THUOCL_car.txt|MIT|THUOCL 汽车词表"
  "THUOCL_food.txt|https://raw.githubusercontent.com/thunlp/THUOCL/master/data/THUOCL_food.txt|MIT|THUOCL 饮食词表"
  "THUOCL_lishimingren.txt|https://raw.githubusercontent.com/thunlp/THUOCL/master/data/THUOCL_lishimingren.txt|MIT|THUOCL 历史名人词表"
  "THUOCL_chengyu.txt|https://raw.githubusercontent.com/thunlp/THUOCL/master/data/THUOCL_chengyu.txt|MIT|THUOCL 成语词表"
  "THUOCL_poem.txt|https://raw.githubusercontent.com/thunlp/THUOCL/master/data/THUOCL_poem.txt|MIT|THUOCL 诗词词表（含较多冷僻字，按需裁剪）"
  "jieba_dict.txt|https://raw.githubusercontent.com/fxsjy/jieba/master/extra_dict/dict.txt.big|MIT|jieba 通用词表（58 万条，「词 词频 词性」；含繁体条目，生成时按简繁表过滤）"
  "emoji/emoji.json|https://raw.githubusercontent.com/iDvel/rime-ice/main/opencc/emoji.json|Apache-2.0|OpenCC 配置：中文 → emoji"
  "emoji/emoji.txt|https://raw.githubusercontent.com/iDvel/rime-ice/main/opencc/emoji.txt|Apache-2.0|emoji 转换表"
  "emoji/others.txt|https://raw.githubusercontent.com/iDvel/rime-ice/main/opencc/others.txt|Apache-2.0|日期/单位等短语转换表"
  "opencc/STCharacters.txt|https://raw.githubusercontent.com/BYVoid/OpenCC/master/data/dictionary/STCharacters.txt|Apache-2.0|简→繁 单字表"
  "opencc/STPhrases.txt|https://raw.githubusercontent.com/BYVoid/OpenCC/master/data/dictionary/STPhrases.txt|Apache-2.0|简→繁 词组表"
  "opencc/TSCharacters.txt|https://raw.githubusercontent.com/BYVoid/OpenCC/master/data/dictionary/TSCharacters.txt|Apache-2.0|繁→简 单字表"
)

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

: > "$LOCK.tmp"
fail=0
for entry in "${SOURCES[@]}"; do
  IFS='|' read -r name url license desc <<<"$entry"
  out="$DEST/$name"
  mkdir -p "$(dirname "$out")"

  want=""
  if [ -f "$out" ] && [ "$FORCE" -eq 0 ]; then
    want="$(sha256sum "$out" | cut -d' ' -f1)"
  fi

  if [ -z "$want" ]; then
    echo "→ 取回 $name"
    if ! download "$url" "$out.part"; then
      echo "✗ 下载失败：$url" >&2
      fail=1
      continue
    fi
    mv "$out.part" "$out"
  else
    echo "· 已存在 $name（跳过下载）"
  fi

  got="$(sha256sum "$out" | cut -d' ' -f1)"
  printf '%s  %s  %s\n' "$got" "$name" "$license" >> "$LOCK.tmp"

  if [ -f "$LOCK" ] && [ "$FORCE" -eq 0 ]; then
    old="$(grep -F "  $name  " "$LOCK" | cut -d' ' -f1 || true)"
    if [ -n "$old" ] && [ "$old" != "$got" ]; then
      echo "✗ $name 的内容与上次记录不一致（上游改过数据）。" >&2
      echo "  上次 $old" >&2
      echo "  本次 $got" >&2
      echo "  词库内容变了，权重与音节表都会跟着变——请重新生成词库，" >&2
      echo "  或用 --force 明确接受这次更新。" >&2
      fail=1
    fi
  fi
done

if [ "$fail" -ne 0 ]; then
  rm -f "$LOCK.tmp"
  exit 1
fi
mv "$LOCK.tmp" "$LOCK"

# ── 生成 OpenCC 配置（数据已就位，只差把它组装成一份 config）─────────
#
# 这三份 json 是**我们写的**（每个 20 行不到），描述"用哪几张表、按什么顺序
# 转换"。它们与上游 `emoji.json` 的形状一致——但 `emoji.json` 用的是
# 相对文件名，而我们的表在 `build/` 下，所以显式写清楚比让装载器去猜好。
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

echo "· 生成 $DEST/opencc/s2t.json（用 STPhrases + STCharacters，词组优先）"

echo
echo "✓ 数据就位：$DEST"
echo "  sha256 记录在 $LOCK（$(wc -l < "$LOCK") 份）"
echo
echo "下一步："
echo "  cargo run --manifest-path tools/wordlist-gen/Cargo.toml -- \\"
echo "      --sources $DEST --out $ROOT/schemes/stele-default"
