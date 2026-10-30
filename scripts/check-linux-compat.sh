#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

if [ "$#" -ne 1 ]; then
  echo "Usage: $0 <Linux GNU binary>" >&2
  exit 1
fi
binary="$1"
# 带 / 的相对路径直接执行，不经 PATH 查找。
if [[ "$binary" != /* ]]; then binary="./$binary"; fi

# version-info 包含强、弱版本需求；不能仅凭启动成功或过滤掉 WEAK。
version_info="$(readelf --wide --version-info "$binary")"
versions="$(grep -oE 'GLIBC_[0-9]+(\.[0-9]+)*' <<< "$version_info" | sort -Vu)"
required="${versions##*$'\n'}"
required="${required#GLIBC_}"
echo "GLIBC versions (strong and weak):"
echo "$versions"
if [ "$(printf '%s\n' 2.35 "$required" | sort -V | tail -1)" != 2.35 ]; then
  echo "FAIL: $binary references GLIBC_$required above the GLIBC_2.35 baseline" >&2
  exit 1
fi

echo "Starting $binary --version"
"$binary" --version
echo "PASS: GLIBC versions <= 2.35 and binary starts successfully"
