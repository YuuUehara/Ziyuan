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
grep -q '四叔来看你了' <<< "$("$BIN" reform cabinet)"
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
# 内阁制：未批红的诏令不执行
"$BIN" tasks | grep -q "#$A .*待批红（票拟：皇帝 → 内阁首辅"
grep -q '没有未结束的诏令' <<< "$("$BIN" run 2>/dev/null)"
"$BIN" approve all | grep -q "诏令 #$B 已批红"
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
# 玉玺：越界或无效的权限在下诏时拒绝；非默认玉玺记入事件簿
mkdir -p src
for bad in "--scope /etc" "--scope ../x" "--scope missing" "--tools write_file"; do
  if "$BIN" edict --queue $bad '越权' > /dev/null 2>&1; then echo "应拒绝: $bad" >&2; exit 1; fi
done
SEALED="$("$BIN" edict --queue --scope src --tools search_code '只查 src')"
grep -q '玉玺 工具：search_code；范围：src' <<< "$SEALED"
"$BIN" history | grep -q '只查 src〔玉玺'
"$BIN" run > /dev/null
# 吏部：评定、考绩、荐举
"$BIN" review "$A" accept '属实'
if "$BIN" review "$A" reject > /dev/null 2>&1; then echo '不应重复评定' >&2; exit 1; fi
if "$BIN" review "$C" accept > /dev/null 2>&1; then echo '已撤销的诏令不应评定' >&2; exit 1; fi
WORKER="$("$BIN" history | sed -n "s/.*诏令 #$A 由 #\([0-9]*\) 回奏.*/\1/p")"
"$BIN" examine "$WORKER" | grep -q '采纳 1，'
"$BIN" recommend clerk:works | sed -n 2p | grep -q "^#$WORKER "
# 官制：不给姓名则随机取名；官员列表显示品级与所用模型
RECRUITED="$("$BIN" recruit works)"
grep -q '招募官员 #[0-9]* ..*（未授官职）' <<< "$RECRUITED"
OFFICIALS="$("$BIN" officials)"
grep -q '内阁首辅（正五品）.*模型 演示' <<< "$OFFICIALS"
grep -q '工部尚书（正二品）' <<< "$OFFICIALS"
# 内阁改回丞相：土木堡；一键朱元璋：革去丞相并废相；被革者不能再任官
# 改制已撤销 #6 的首辅之职
grep -q '大元，杀回来了' <<< "$("$BIN" reform chancellor)"
"$BIN" appoint 6 chancellor
grep -q '胡惟庸' <<< "$("$BIN" abolish)"
"$BIN" officials | grep -q '^#6 .*已革职'
if "$BIN" appoint 6 minister:personnel > /dev/null 2>&1; then echo '革职者不应再任官' >&2; exit 1; fi
"$BIN" court | grep -q '废相亲政'
echo 'ZiYuan smoke test PASS'
