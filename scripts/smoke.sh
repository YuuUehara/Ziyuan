#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
cargo test --workspace
cargo build -p ziyuan-cli
BIN="$ROOT/target/debug/ziyuan"
DEMO="$(mktemp -d)"
trap 'rm -rf "$DEMO"' EXIT
cd "$DEMO"
"$BIN" init
"$BIN" recruit '新工' works
"$BIN" appoint 7 clerk:works
ROUTE="$("$BIN" edict --dept works '调查 Rust unsafe')"
grep -q '丞相' <<< "$ROUTE"
grep -q 'DEMO ONLY' <<< "$ROUTE"
# 第二次派工应选当前记录任务数较少的官员 #7。
ROUTE="$("$BIN" edict --dept works '第二次调查')"
grep -q '新工' <<< "$ROUTE"
"$BIN" reform direct
ROUTE="$("$BIN" edict --dept works '直接承办')"
if grep -q '丞相' <<< "$ROUTE"; then exit 1; fi
"$BIN" reform cabinet
if "$BIN" edict --dept works '尚未设首辅' > /dev/null 2>&1; then
  echo '首辅空缺时应拒绝派工' >&2
  exit 1
fi
"$BIN" dismiss 6
"$BIN" appoint 6 first-secretary
ROUTE="$("$BIN" edict --dept works '内阁承办')"
grep -q '首辅' <<< "$ROUTE"
"$BIN" history > /dev/null
echo 'ZiYuan smoke test PASS'
