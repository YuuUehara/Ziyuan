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
# 入队、撤销、并发执行
queue() { "$BIN" edict --queue --dept works "$1" | sed -n 's/^诏令 #\([0-9]*\) 已入队.*/\1/p'; }
A="$(queue '排队一')"; B="$(queue '排队二')"; C="$(queue '撤销我')"
"$BIN" cancel "$C"
if "$BIN" cancel "$C" > /dev/null 2>&1; then echo '不应重复撤销' >&2; exit 1; fi
RUN="$("$BIN" run --jobs 2 --timeout 30)"
grep -q "诏令 #$A：排队一" <<< "$RUN"
grep -q "诏令 #$B：排队二" <<< "$RUN"
if grep -q '撤销我' <<< "$RUN"; then echo '已撤销的诏令不应执行' >&2; exit 1; fi
TASKS="$("$BIN" tasks)"
grep -q "#$C .*已撤销" <<< "$TASKS"
if grep -q '待办\|未结束' <<< "$TASKS"; then echo '仍有未结束诏令' >&2; exit 1; fi
grep -q '没有未结束的诏令' <<< "$("$BIN" run)"
"$BIN" history | grep -q "诏令 #$A 派给"
# 多进程同时入队：每件诏令编号唯一，事件簿保持可回放
for i in $(seq 1 10); do "$BIN" edict --queue --dept works "并发$i" > /dev/null & done
wait
[ "$("$BIN" tasks | grep -c '并发')" -eq 10 ]
"$BIN" run --jobs 4 > /dev/null
echo 'ZiYuan smoke test PASS'
