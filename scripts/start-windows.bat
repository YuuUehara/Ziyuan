@echo off
chcp 65001 >nul
setlocal
cd /d "%~dp0"
if not exist "ziyuan.exe" (
  echo 未找到 ziyuan.exe。请把本脚本放在解压后的目录里。
  pause
  exit /b 1
)
if not exist ".ziyuan\court.log" (
  echo 首次启动，正在初始化...
  ziyuan.exe init
  if errorlevel 1 (
    echo 初始化失败，请查看上面的提示。
    pause
    exit /b 1
  )
)
start "紫垣服务" cmd /k "ziyuan.exe serve --port 18080"
set "TOKEN_FILE=.ziyuan\web-token"
for /l %%i in (1,1,30) do (
  if exist "%TOKEN_FILE%" goto ready
  timeout /t 1 /nobreak >nul
)
echo 服务没有在 30 秒内就绪，请查看“紫垣服务”窗口。
pause
exit /b 1
:ready
type "%TOKEN_FILE%" | clip
start "" "http://127.0.0.1:18080/"
echo.
echo 已把登录令牌复制到剪贴板：在浏览器登录框按 Ctrl+V 粘贴。
echo “紫垣服务”窗口请勿关闭；关闭窗口即停止服务。
pause
