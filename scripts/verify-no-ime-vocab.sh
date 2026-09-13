#!/usr/bin/env bash
# 门禁：内核里不得出现输入法专属词汇（PLAN D20 / docs/engine-design.md §2.4.5）。
#
# 检查的是**标识符**（类型名、函数名、trait 名），不是注释——注释里举例说明
# "拼音方案下一个编码单元就是一个音节"是被明确允许的。
#
# 为什么是标识符：如果一个类型叫 `SyllableId`，那就等于宣布所有方案都必须说拼音。
# 而仓颉方案里一个"音节"只是一个字母，五笔方案里根本不需要切分图。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGETS=("$ROOT/crates/qingjian-core/src" "$ROOT/crates/qingjian-engine/src")

# 只匹配"声明位置"的标识符：struct / enum / trait / type / fn / 常量。
PATTERN='(struct|enum|trait|type|fn|const|static)[[:space:]]+[A-Za-z_]*([Ss]yllab|[Pp]inyin|[Bb]opomofo|[Cc]angjie|[Ww]ubi|[Jj]ianpin|[Ff]uzzy[Ss]ound)'

status=0
for dir in "${TARGETS[@]}"; do
  [ -d "$dir" ] || continue
  if hits=$(grep -rInE "$PATTERN" "$dir" 2>/dev/null); then
    echo "✗ 内核里出现了输入法专属标识符："
    echo "$hits"
    status=1
  fi
done

if [ "$status" -eq 0 ]; then
  echo "✓ verify-no-ime-vocab: 内核标识符中没有输入法专属词汇"
fi
exit "$status"
