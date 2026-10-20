<#
.SYNOPSIS
Windows zip 설치 스모크 테스트: 임시 폴더에 install.ps1로 설치하고 데몬 기동·세션 입출력·krilld 강제 종료 생존을 본다.
이 사용자의 실제 Krill 상태(%LOCALAPPDATA%\Krill)와 설치(%LOCALAPPDATA%\Programs\Krill)는 건드리지 않는다.

.EXAMPLE
powershell -ExecutionPolicy Bypass -File packaging\windows\smoke.ps1 -Zip packaging\dist\Krill-0.1.20-windows-x64.zip
#>
param([Parameter(Mandatory)][string]$Zip)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$zipPath = (Resolve-Path $Zip).Path
$base = Join-Path $env:TEMP "krill-smoke-$PID"
$dest = Join-Path $base 'Programs\Krill'
$krill = Join-Path $dest 'krill.exe'
$env:KRILL_STATE_DIR = Join-Path $base 'state'
function Get-Ours([string]$name) { Get-Process $name -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$dest\*" } }
function Invoke-Krill { $out = & $krill @args; if ($LASTEXITCODE -ne 0) { throw "krill $args failed: $LASTEXITCODE" }; $out }

New-Item -ItemType Directory -Force $base | Out-Null
try {
    & (Join-Path $PSScriptRoot 'install.ps1') -Zip $zipPath -Dest $dest -NoShortcut
    $info = Get-Content -Raw (Join-Path $dest 'Krill.json') | ConvertFrom-Json
    foreach ($f in 'krill-app.exe', 'krill.exe', 'krilld.exe', 'krill-holder.exe', 'conpty.dll', 'x64\OpenConsole.exe') {
        if (-not (Test-Path (Join-Path $dest $f))) { throw "missing $f" }
    }

    Invoke-Krill ls | Out-Null
    $id = (Invoke-Krill new --cwd $base | Select-Object -Last 1).Trim()
    Invoke-Krill send $id 'echo ("smoke-" + 6*7)' | Out-Null
    Invoke-Krill send $id --enter | Out-Null
    Invoke-Krill wait $id --for pattern --pattern 'smoke-42' --timeout 20 | Out-Null

    $old = @(Get-Ours krilld)
    if ($old.Count -ne 1) { throw "expected one krilld, found $($old.Count)" }
    Stop-Process -Id $old[0].Id -Force
    Invoke-Krill send $id 'echo ("after-kill-" + 6*7)' | Out-Null
    Invoke-Krill send $id --enter | Out-Null
    Invoke-Krill wait $id --for pattern --pattern 'after-kill-42' --timeout 20 | Out-Null
    $new = @(Get-Ours krilld)
    if ($new.Count -ne 1 -or $new[0].Id -eq $old[0].Id) { throw 'krilld did not restart' }

    # GUI: 헤드리스 세션(SSH)에서는 창을 못 띄우고 곧 끝날 수 있다. 결과만 적고 실패로 보지 않는다.
    $app = Start-Process -PassThru (Join-Path $dest 'krill-app.exe')
    Start-Sleep -Seconds 5
    $appState = if ($app.HasExited) { "exited $($app.ExitCode)" } else { 'running after 5s' }
    "smoke ok: $(Split-Path -Leaf $zipPath) (Krill.json $($info.CFBundleShortVersionString), session $id survived krilld kill; krill-app: $appState)"
} finally {
    foreach ($n in 'krill-app', 'krill', 'krilld', 'krill-holder', 'OpenConsole') { Get-Ours $n | Stop-Process -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Seconds 1
    Remove-Item -Recurse -Force $base -ErrorAction SilentlyContinue
}
