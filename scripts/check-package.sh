#!/usr/bin/env bash
# Offline smoke for an extracted release binary; also shared by native-platform CI.
set -euo pipefail
if [ "$#" -ne 2 ]; then
  echo "Usage: $0 <binary> <expected-version>" >&2
  exit 1
fi
binary="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
version="$2"
for name in ${!ZIYUAN_@}; do unset "$name"; done
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"

"$binary" --version > version.txt 2> version.stderr
test ! -s version.stderr
test "$(tr -d '\r' < version.txt)" = "ziyuan $version"
"$binary" --help > help.txt
grep -q ZIYUAN_CONTEXT_TOKENS help.txt
grep -q ZIYUAN_DELTA help.txt
"$binary" init > init.txt
"$binary" edict --queue --dept works --tools read_file 'offline package smoke' > queued.txt
"$binary" court --json > pending.json
jq -e '(.tasks | length == 1) and (.tasks[0].status == "pending")' pending.json > /dev/null
cp .ziyuan/court.log before.log
if ZIYUAN_LLM_MODEL=package-check ZIYUAN_LLM_BASE_URL=http://127.0.0.1:9/v1 \
  ZIYUAN_CONTEXT_TOKENS=0 "$binary" run --timeout 5 > invalid.txt 2> invalid.stderr; then
  echo 'FAIL: invalid context threshold was accepted' >&2
  exit 1
fi
grep -q ZIYUAN_CONTEXT_TOKENS invalid.stderr
cmp .ziyuan/court.log before.log
"$binary" run --timeout 10 > run.txt
"$binary" court --json > completed.json
jq -e '(.tasks | length == 1) and (.tasks[0].status == "completed")' completed.json > /dev/null
"$binary" migrate --json > migrate.json
jq -e '.readable == true' migrate.json > /dev/null
echo "PASS: $version offline package smoke (version, help, queue/run, context rejection, replay)"
