<# :
@echo off
chcp 65001 >nul
setlocal
set "ZIYUAN_START_URL=http://127.0.0.1:18080/"
if /I "%~1"=="--translation" set "ZIYUAN_START_URL=http://127.0.0.1:18080/#translation"
cd /d "%~dp0"
set "ZIYUAN_BAT_SELF=%~f0"
set "PS_EXE=%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe"
if not exist "%PS_EXE%" set "PS_EXE=powershell.exe"
"%PS_EXE%" -NoProfile -ExecutionPolicy Bypass -Command "Invoke-Expression ([System.IO.File]::ReadAllText($env:ZIYUAN_BAT_SELF, [System.Text.Encoding]::UTF8))"
exit /b %errorlevel%
#>

$ErrorActionPreference = 'Stop'
[Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
try { $Host.UI.RawUI.WindowTitle = '紫垣 · 奉天门（关闭本窗口即停止服务）' } catch {}

$url = if ($env:ZIYUAN_START_URL) { $env:ZIYUAN_START_URL } else { 'http://127.0.0.1:18080/' }

function Wait-KeyToClose {
  if ($env:ZIYUAN_LAUNCHER_NO_WAIT) { return }
  Write-Host ''
  Write-Host '按任意键关闭窗口...'
  try {
    [void][Console]::ReadKey($true)
  } catch {
    [void][Console]::In.ReadLine()
  }
}

function Copy-Token {
  if (Test-Path '.ziyuan\web-token') {
    $token = (Get-Content -Raw -Encoding UTF8 '.ziyuan\web-token').Trim()
    if ($token) {
      try {
        Set-Clipboard -Value $token
        return $true
      } catch {}
    }
  }
  return $false
}

function Test-PortOpen {
  param([int]$TimeoutMs = 250)
  $client = [System.Net.Sockets.TcpClient]::new()
  try {
    $ar = $client.BeginConnect('127.0.0.1', 18080, $null, $null)
    if (-not $ar.AsyncWaitHandle.WaitOne($TimeoutMs)) { return $false }
    try {
      $client.EndConnect($ar)
      return $true
    } catch {
      return $false
    }
  } finally {
    $client.Close()
  }
}

if (-not (Test-Path '.\ziyuan.exe')) {
  Write-Host '[错误] 未找到 ziyuan.exe。请把本脚本放在解压后的同一目录里。' -ForegroundColor Red
  Wait-KeyToClose
  exit 1
}

if (Test-PortOpen -TimeoutMs 250) {
  $isZiyuan = $false
  try {
    $req = [System.Net.HttpWebRequest]::Create('http://127.0.0.1:18080/api/state')
    $req.Timeout = 1500
    $resp = $req.GetResponse()
    $resp.Close()
    $isZiyuan = $true
  } catch [System.Net.WebException] {
    if ($_.Exception.Response) {
      $code = [int]$_.Exception.Response.StatusCode
      if ($code -eq 401 -or $code -eq 200) { $isZiyuan = $true }
    }
  }
  if ($isZiyuan) {
    $copied = Copy-Token
    if (-not $env:ZIYUAN_LAUNCHER_NO_OPEN) { Start-Process $url }
    Write-Host '紫垣服务已在运行（http://127.0.0.1:18080/），已为您重新打开浏览器。'
    if ($copied) { Write-Host '登录令牌已复制到剪贴板：在登录框按 Ctrl+V 粘贴即可。' }
    if (-not $env:ZIYUAN_LAUNCHER_NO_WAIT) { Start-Sleep -Seconds 2 }
    exit 0
  }
  Write-Host '[错误] 18080 端口已被其他程序占用，无法启动紫垣服务。' -ForegroundColor Red
  Write-Host '请先关闭占用该端口的程序，或在命令行改用：ziyuan.exe serve --port 18081'
  Wait-KeyToClose
  exit 1
}

if (-not (Test-Path '.\.ziyuan\court.log')) {
  Write-Host '首次启动，正在初始化紫垣...'
  & '.\ziyuan.exe' init
  if ($LASTEXITCODE -ne 0) {
    Write-Host '[错误] 初始化失败，请查看上面的提示。' -ForegroundColor Red
    Wait-KeyToClose
    exit 1
  }
}

Write-Host '=================================================='
Write-Host '  紫垣 · 奉天门（关闭本窗口即停止服务）'
Write-Host '=================================================='
Write-Host "  访问地址：$url"
Write-Host '  正在启动本地服务，就绪后会自动复制令牌并打开浏览器…'
Write-Host ''

$exePath = (Resolve-Path '.\ziyuan.exe').Path
$proc = Start-Process -FilePath $exePath -ArgumentList 'serve', '--port', '18080' -NoNewWindow -PassThru
try {
  $ready = $false
  for ($i = 0; $i -lt 150; $i++) {
    if ($proc.HasExited) { break }
    if ((Test-PortOpen -TimeoutMs 150) -and (Test-Path '.\.ziyuan\web-token')) {
      $t = (Get-Content -Raw -Encoding UTF8 '.\.ziyuan\web-token').Trim()
      if ($t) {
        $ready = $true
        break
      }
    }
    Start-Sleep -Milliseconds 200
  }

  if ($proc.HasExited) {
    Write-Host ''
    Write-Host ("[错误] 紫垣服务启动失败（退出码 " + $proc.ExitCode + '），请查看上面的提示。') -ForegroundColor Red
    Wait-KeyToClose
    exit 1
  }

  if (-not $ready) {
    Write-Host ''
    Write-Host '[错误] 服务没有在 30 秒内就绪。' -ForegroundColor Red
    Wait-KeyToClose
    exit 1
  }

  Start-Sleep -Milliseconds 150
  $copied = Copy-Token
  if (-not $env:ZIYUAN_LAUNCHER_NO_OPEN) { Start-Process $url }
  Write-Host ''
  if ($copied) {
    Write-Host '已把登录令牌复制到剪贴板：在浏览器登录框按 Ctrl+V 粘贴即可。'
  }
  Write-Host '提示：请保持本窗口开启；关闭本窗口或按 Ctrl+C 即停止服务。'

  $proc.WaitForExit()
  if ($proc.ExitCode -ne 0) {
    Write-Host ''
    Write-Host ("[错误] 紫垣服务异常退出（退出码 " + $proc.ExitCode + '）。') -ForegroundColor Red
    Wait-KeyToClose
    exit 1
  }
} finally {
  if ($proc -and -not $proc.HasExited) {
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
  }
}
