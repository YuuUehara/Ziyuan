#!/usr/bin/env bash
set -euo pipefail
START_URL="http://127.0.0.1:18080/"
if [ "${1:-}" = --translation ]; then START_URL="http://127.0.0.1:18080/#translation"; fi
cd "$(dirname "$0")"
if [ ! -x ./ziyuan ]; then
  echo "未找到可执行的 ./ziyuan。请把本脚本放在解压后的目录里。"
  read -r -p "按回车退出..." _
  exit 1
fi
if [ ! -f .ziyuan/court.log ]; then
  echo "首次启动，正在初始化..."
  ./ziyuan init
fi
nohup ./ziyuan serve --port 18080 >/tmp/ziyuan-serve.log 2>&1 &
for _ in $(seq 1 30); do
  [ -f .ziyuan/web-token ] && break
  sleep 1
done
if [ ! -f .ziyuan/web-token ]; then
  echo "服务没有在 30 秒内就绪，请查看 /tmp/ziyuan-serve.log"
  read -r -p "按回车退出..." _
  exit 1
fi
TOKEN="$(cat .ziyuan/web-token)"
if command -v pbcopy >/dev/null 2>&1; then printf '%s' "$TOKEN" | pbcopy; COPIED=1
elif command -v xclip >/dev/null 2>&1; then printf '%s' "$TOKEN" | xclip -selection clipboard; COPIED=1
elif command -v wl-copy >/dev/null 2>&1; then printf '%s' "$TOKEN" | wl-copy; COPIED=1
else COPIED=0; fi
if command -v open >/dev/null 2>&1; then open "$START_URL"
elif command -v xdg-open >/dev/null 2>&1; then xdg-open "$START_URL"
fi
echo
if [ "$COPIED" = 1 ]; then
  echo "已把登录令牌复制到剪贴板：在浏览器登录框粘贴即可。"
else
  echo "登录令牌：$TOKEN"
fi
echo "服务在后台运行；日志：/tmp/ziyuan-serve.log"
