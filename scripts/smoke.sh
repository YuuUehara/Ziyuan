#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
cargo test --workspace
cargo build -p ziyuan-cli
BIN="$ROOT/target/debug/ziyuan"
DEMO="$(mktemp -d)"
trap 'rm -rf "$DEMO"' EXIT
# 读完全部输出再匹配：grep -q 提前退出会让 CLI 写入断开的管道而失败。
has() { local out; out="$(cat)"; grep -q -- "$1" <<< "$out"; }
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
"$BIN" tasks | has "#$A .*待批红（未票拟，旧例预拟：皇帝 → 内阁首辅"
# 内阁票拟（演示后端按旧例拟派），再御笔改票给新工（#7）；B 不票拟直接批红（中旨）
"$BIN" draft "$A" | has "诏令 #$A 票拟：拟派"
if "$BIN" redraft "$A" 3 > /dev/null 2>&1; then echo '不应改派他部官员' >&2; exit 1; fi
"$BIN" redraft "$A" 7 '新工熟悉' | has '御笔改票：新工熟悉'
"$BIN" tasks | has "#$A .*待批红（票拟：.*工部郎中·新工（票拟）"
grep -q '没有未结束的诏令' <<< "$("$BIN" run 2>/dev/null)"
"$BIN" approve all | has "诏令 #$B 已批红"
RUN="$("$BIN" run --jobs 2 --timeout 30)"
grep -q "诏令 #$A：排队一" <<< "$RUN"
grep -q "诏令 #$B：排队二" <<< "$RUN"
if grep -q '撤销我' <<< "$RUN"; then echo '已撤销的诏令不应执行' >&2; exit 1; fi
TASKS="$("$BIN" tasks)"
grep -q "#$C .*已撤销" <<< "$TASKS"
if grep -q '待办\|未结束' <<< "$TASKS"; then echo '仍有未结束诏令' >&2; exit 1; fi
grep -q '没有未结束的诏令' <<< "$("$BIN" run)"
"$BIN" history | has "诏令 #$A 派给 #7"
# 多进程同时入队：每件诏令编号唯一，事件簿保持可回放
for i in $(seq 1 10); do "$BIN" edict --queue --dept works "并发$i" > /dev/null & done
wait
[ "$("$BIN" tasks | grep -c '并发')" -eq 10 ]
"$BIN" run --jobs 4 > /dev/null
# 玉玺：越界或无效的权限在下诏时拒绝；非默认玉玺记入事件簿
mkdir -p src
for bad in "--scope /etc" "--scope ../x" "--scope missing" "--tools run_shell"; do
  if "$BIN" edict --queue $bad '越权' > /dev/null 2>&1; then echo "应拒绝: $bad" >&2; exit 1; fi
done
SEALED="$("$BIN" edict --queue --scope src --tools search_code '只查 src')"
grep -q '玉玺 工具：search_code；范围：src' <<< "$SEALED"
"$BIN" history | has '只查 src〔玉玺'
"$BIN" run > /dev/null
# 吏部：评定、考绩、荐举
"$BIN" review "$A" accept '属实'
if "$BIN" review "$A" reject > /dev/null 2>&1; then echo '不应重复评定' >&2; exit 1; fi
if "$BIN" review "$C" accept > /dev/null 2>&1; then echo '已撤销的诏令不应评定' >&2; exit 1; fi
WORKER="$("$BIN" history | sed -n "s/.*诏令 #$A 由 #\([0-9]*\) 回奏.*/\1/p")"
"$BIN" examine "$WORKER" | has '采纳 1，'
"$BIN" recommend clerk:works | sed -n 2p | has "^#$WORKER "
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
OUT="$("$BIN" abolish)"
grep -q '胡惟庸' <<< "$OUT"
grep -q '株连所及' <<< "$OUT"
if "$BIN" officials | grep -qE '（(正|从)'; then echo '一键朱元璋后不应再有在任官员' >&2; exit 1; fi
"$BIN" officials | has '^#6 .*已革职'
if "$BIN" appoint 6 minister:personnel > /dev/null 2>&1; then echo '革职者不应再任官' >&2; exit 1; fi
"$BIN" court | has '废相亲政'
# 六部为之一空：重新招募工部尚书与郎中
for OFFICE in minister:works clerk:works; do
  NEW="$("$BIN" recruit works | grep -o '#[0-9]*' | head -1 | tr -d '#')"
  "$BIN" appoint "$NEW" "$OFFICE" > /dev/null
done
# 官职配置：指定模型、玉玺上限（诏令范围全在上限外则不予承办）
"$BIN" model clerk:works deepseek-v4-pro | has '工部郎中改用模型 deepseek-v4-pro'
"$BIN" officials | has '工部郎中.*模型 deepseek-v4-pro'
if "$BIN" model clerk:works 'a b' > /dev/null 2>&1; then echo '应拒绝含空白的模型名' >&2; exit 1; fi
"$BIN" ceiling clerk:works --scope src --tools search_code | has '玉玺上限为 工具：search_code；范围：src'
"$BIN" ceiling minister:works --scope src > /dev/null
mkdir -p docs
OUT="$("$BIN" edict --scope docs '越出上限' 2>/dev/null || true)"
grep -q '玉玺上限之外' <<< "$OUT"
"$BIN" edict '受限承办' | has '玉玺受工部.*上限所限：工具：search_code；范围：src'
"$BIN" ceiling clerk:works default | has '取消玉玺上限'
"$BIN" history | has '工部郎中玉玺上限'
# 工部受控写权限：非工部不得授 write_file；演示后端不改文件，无改动可准奏
if "$BIN" edict --dept revenue --tools write_file '越权' > /dev/null 2>&1; then echo '非工部不应授写权' >&2; exit 1; fi
"$BIN" edict --tools read_file,write_file --queue '工部改动' | has '已入队'
"$BIN" run > /dev/null
WID="$("$BIN" tasks | tail -1 | grep -o '^#[0-9]*' | tr -d '#')"
"$BIN" diff "$WID" | has '没有呈报改动'
if "$BIN" apply "$WID" > /dev/null 2>&1; then echo '无改动不应准奏' >&2; exit 1; fi
# 都察院：御史空缺则拒绝；任命后复核（演示回奏无据，按证据例弹劾）；考成簿
if "$BIN" censor "$WID" > /dev/null 2>&1; then echo '御史空缺不应复核' >&2; exit 1; fi
CID="$("$BIN" recruit justice | grep -o '#[0-9]*' | head -1 | tr -d '#')"
"$BIN" appoint "$CID" censor | has '左都御史'
"$BIN" censor "$WID" | has '弹劾（回奏未引用代码位置；未跑测试）'
"$BIN" tasks | has '御史弹劾'
"$BIN" kaocheng | has '天子亲自考成'
"$BIN" examine "$CID" | has '考成：无事可考'
# 多官协作：演示后端无从拆分；御定分办后子诏令先办，尚书汇总
SID="$("$BIN" edict --queue '会办全库' | grep -o '#[0-9]*' | head -1 | tr -d '#')"
if "$BIN" split "$SID" > /dev/null 2>&1; then echo '演示后端不应能拆分' >&2; exit 1; fi
CLERK="$("$BIN" officials | grep '工部郎中' | head -1 | grep -o '^#[0-9]*' | tr -d '#')"
"$BIN" split "$SID" "$CLERK:查甲" "$CLERK:查乙" | has '御定分办'
"$BIN" tasks | has "分办自 #$SID"
"$BIN" tasks | grep "^#$SID " | has '分办中（2 件未结）'
"$BIN" run > /dev/null
"$BIN" tasks | grep "^#$SID " | has '已完成（.*），'
"$BIN" history | has "诏令 #$SID 分办"
# 昏君模式：修道期间不能亲自评定；新诏令执行后司礼监代批照准；回朝
"$BIN" retreat | has '西苑'
"$BIN" court | has '西苑修道中'
"$BIN" edict --queue '修道期间' > /dev/null
"$BIN" run > /dev/null
"$BIN" tasks | tail -1 | has '司礼监代批采纳'
RID="$("$BIN" edict --queue '再办' | grep -o '#[0-9]*' | head -1 | tr -d '#')"
"$BIN" run > /dev/null
if "$BIN" review "$RID" reject > /dev/null 2>&1; then echo '修道期间不应亲自评定' >&2; exit 1; fi
"$BIN" kaocheng | has '御前：怠政'
"$BIN" return | has '治安疏'
# 人格与廷议：官员各有性格文风；演示后端不能廷议，不留纪要
"$BIN" officials | has '·'
DID="$("$BIN" edict --queue '廷议此事' | grep -o '#[0-9]*' | head -1 | tr -d '#')"
if "$BIN" debate "$DID" > /dev/null 2>&1; then echo '演示后端不应能廷议' >&2; exit 1; fi
if "$BIN" debate "$DID" --rounds 9 > /dev/null 2>&1; then echo '轮数越界应拒绝' >&2; exit 1; fi
if "$BIN" history | has '廷议：'; then echo '廷议失败不应留记录' >&2; exit 1; fi
# 户部度支：非正数被拒；设限后 court 显示余额，奏销可读，历史有记录
if "$BIN" budget court 0 > /dev/null 2>&1; then echo '度支须为正数' >&2; exit 1; fi
"$BIN" budget clerk:works 800 | has '工部郎中每件：限 800'
"$BIN" budget court 999999 > /dev/null
"$BIN" court | has '朝廷度支：999999 tokens'
"$BIN" budget | has '度支奏销'
"$BIN" history | has '户部度支'
"$BIN" budget court none > /dev/null
# 刑部与锦衣卫：演示回奏无可疑内容
"$BIN" flags | has '无所奏报'
if "$BIN" flags x > /dev/null 2>&1; then echo '诏令 ID 须为数字' >&2; exit 1; fi
# 礼部：未出题、演示后端不能应试或殿试、未殿试不能钦点，都不留记录；演示回奏合乎体例
{ "$BIN" exam 1 2>&1 || true; } | has '尚未出题'
mkdir -p .ziyuan/exam && printf '何以防争写？\n要点：文件锁\n' > .ziyuan/exam/lock.txt
if "$BIN" exam 1 > /dev/null 2>&1; then echo '演示后端不应能应试' >&2; exit 1; fi
if "$BIN" exam --palace 何以治国 > /dev/null 2>&1; then echo '演示后端不应能殿试' >&2; exit 1; fi
{ "$BIN" crown 1 2>&1 || true; } | has '尚未举行殿试'
if "$BIN" history | has '礼部科举'; then echo '科举失败不应留记录' >&2; exit 1; fi
if "$BIN" history | has '殿试：'; then echo '殿试失败不应留记录' >&2; exit 1; fi
if "$BIN" tasks | has '体例不合'; then echo '演示回奏应合体例' >&2; exit 1; fi
echo 'ZiYuan smoke test PASS'
