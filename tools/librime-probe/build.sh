#!/bin/sh
# 编译 librime 行为探针。
#
# 关键点：链接行里 **没有** -lrime —— 本机没装 librime-dev，既没有 .so 符号链接
# 也没有头文件。probe 在运行时用 dlopen("librime.so.1") 装载，所以只需要 -ldl。
set -eu

cd "$(dirname "$0")"

CC="${CC:-cc}"

# -D_GNU_SOURCE 已写在 probe.c 里；这里再给一次也无害，方便 -std=c99 下用
# open_memstream / strcasecmp / nanosleep 等 POSIX 接口。
"$CC" -std=c99 -O2 -Wall -Wextra -D_GNU_SOURCE -o probe probe.c -ldl

echo "built: $(pwd)/probe"
