@echo off
setlocal
if exist "%~dp0start-ziyuan.bat" (
  call "%~dp0start-ziyuan.bat" --translation
) else (
  call "%~dp0start-windows.bat" --translation
)
exit /b %errorlevel%
