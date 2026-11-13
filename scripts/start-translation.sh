#!/usr/bin/env bash
set -euo pipefail
base="$(cd "$(dirname "$0")" && pwd)"
if [ -f "$base/start-ziyuan.sh" ]; then
  exec bash "$base/start-ziyuan.sh" --translation
else
  exec bash "$base/start-unix.sh" --translation
fi
