<#
.SYNOPSIS
OWE-839 Windows 메모리 회귀 게이트(MK-066 Windows판). 예산을 넘으면 1로 끝난다.

.DESCRIPTION
격리한 상태 디렉터리에 release krill-holder + krilld를 띄우고, 헤드리스 세션 N개(기본 10)에 한글·색·120열을 넘는 줄
2만 개씩 흘린 뒤 10초 쉬고 PrivateUsage(커밋) 합을 잰다. 지표는 Windows 프로세스 메모리 API를 기준으로 한다:
PrivateUsage가 게이트, Private Working Set은 보조 기록. 성능 카운터 이름이 지역화되므로 Get-Counter를 쓰지 않는다.

  기본(데몬 게이트): holder + krilld <= 60MiB. SSH 같은 비대화형 세션에서 돈다.
  -App(사람이 로그온한 GUI 세션에서만): krill-app도 띄워 앱 <= 120MiB, 합계 <= 200MiB.
  세션 프로세스(생성기)와 ConPTY 호스트(conhost/OpenConsole)는 예산 밖이다. 호스트 합은 기록만 한다.

종료 코드: 0 통과, 1 예산 초과, 2 준비 실패(빌드·데몬 기동·세션 생성).
이 스크립트의 도움말과 실행 결과를 기준으로 측정 조건을 확인한다.

.PARAMETER Inject
krilld가 그만큼(MiB) 더 쥐게 해(KRILL_TEST_BALLAST_MB) 게이트가 회귀를 잡는지 확인한다. 이때는 1이 정상이다.

.PARAMETER BinDir
이미 빌드한 바이너리 폴더. 주면 빌드를 건너뛴다. 없으면 release.yml과 같은 +crt-static release로 빌드한다.

.EXAMPLE
powershell -ExecutionPolicy Bypass -File scripts\memory-gate-windows.ps1
powershell -ExecutionPolicy Bypass -File scripts\memory-gate-windows.ps1 -Inject 80   # 1이어야 한다
#>
param(
    [int]$Sessions = 10,
    [int]$Inject = 0,
    [string]$BinDir = '',
    [switch]$App
)
# 문자열은 ASCII만 쓴다(BOM 없는 파일을 Windows PowerShell 5.1이 ANSI로 읽는다).
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot)
$DaemonLimit = 60MB
$AppLimit = 120MB
$TotalLimit = 200MB
$GenLines = 20000
$Idle = 10

function Stop-Gate([int]$code, [string]$msg) {
    [Console]::Error.WriteLine("memory-gate-windows: $msg")
    exit $code
}

if (-not $BinDir) {
    $target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { 'target' }
    $BinDir = Join-Path $target 'release'
    $pkgs = @('-p', 'krill', '-p', 'krilld', '-p', 'krill-holder')
    if ($App) { $pkgs += @('-p', 'krill-app') }
    $saved = $env:RUSTFLAGS
    $env:RUSTFLAGS = '-C target-feature=+crt-static'
    cargo build --release --locked @pkgs
    $built = $LASTEXITCODE
    $env:RUSTFLAGS = $saved
    if ($built -ne 0) { Stop-Gate 2 "cargo build failed ($built)" }
}
$BinDir = (Resolve-Path $BinDir).Path
foreach ($n in 'krill', 'krilld', 'krill-holder') {
    if (-not (Test-Path (Join-Path $BinDir "$n.exe"))) { Stop-Gate 2 "missing $n.exe in $BinDir" }
}

# 소켓 경로가 sun_path(108B)에 들어가도록 짧게 잡는다.
$dir = Join-Path $env:TEMP "mkw-$PID"
New-Item -ItemType Directory $dir | Out-Null
$socket = Join-Path $dir 'run\krilld.sock'
$env:KRILL_STATE_DIR = $dir
if ($Inject -gt 0) { $env:KRILL_TEST_BALLAST_MB = "$Inject" }

# 생성기: go 파일(앱이 붙은 뒤)을 기다렸다가 줄을 내고 done-<i>를 남긴 채 산다. 한글은 코드 포인트로 넣는다.
$gen = Join-Path $dir 'gen.ps1'
Set-Content -Encoding ascii $gen @"
param(`$Dir, `$I)
[Console]::OutputEncoding = [Text.Encoding]::UTF8
`$ko = [string][char]0xD55C + [char]0xAE00
`$color = [string][char]0xC0C9
`$e = [char]27
`$x = 'x' * 110
while (-not (Test-Path "`$Dir\go")) { Start-Sleep -Milliseconds 20 }
`$sb = New-Object Text.StringBuilder
foreach (`$n in 1..$GenLines) {
    [void]`$sb.AppendFormat("{0:D7} {1} {2}[3{3}m{4}{2}[0m {5}``r``n", `$n, `$ko, `$e, (`$n % 8), `$color, `$x)
    if (`$n % 1000 -eq 0) { [Console]::Write(`$sb.ToString()); [void]`$sb.Clear() }
}
[Console]::Write(`$sb.ToString())
New-Item -ItemType File "`$Dir\done-`$I" | Out-Null
while (`$true) { Start-Sleep 1 }
"@

$holder = $null
$appProc = $null
try {
    # macOS 게이트(start_daemon)처럼 holder를 직접 띄운다. 이 트리만 정리하므로 사용자의 Krill 데몬은 건드리지 않는다.
    # -NoNewWindow: holder 자신의 conhost가 생기지 않아 holder 자식 conhost는 ConPTY 호스트만 남는다.
    $holder = Start-Process (Join-Path $BinDir 'krill-holder.exe') -PassThru -NoNewWindow `
        -ArgumentList @('--detach', "`"$(Join-Path $BinDir 'krilld.exe')`"", "`"$socket`"")
    $deadline = (Get-Date).AddSeconds(30)
    while (-not (Test-Path $socket)) {
        if ($holder.HasExited) { Stop-Gate 2 "krill-holder exited ($($holder.ExitCode)); see $dir\logs" }
        if ((Get-Date) -gt $deadline) { Stop-Gate 2 "daemon socket did not appear in 30s: $socket" }
        Start-Sleep -Milliseconds 100
    }

    $krill = Join-Path $BinDir 'krill.exe'
    $ps = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    for ($i = 0; $i -lt $Sessions; $i++) {
        $out = & $krill new --cwd $dir --cols 120 --rows 40 -- $ps -NoProfile -ExecutionPolicy Bypass -File $gen $dir $i 2>&1
        if ($LASTEXITCODE -ne 0) { Stop-Gate 2 "krill new #$i failed ($LASTEXITCODE): $out" }
    }

    if ($App) {
        $appProc = Start-Process (Join-Path $BinDir 'krill-app.exe') -PassThru
        Read-Host "Open all $Sessions sessions as tabs in Krill, then press Enter"
        if ($appProc.HasExited) { Stop-Gate 2 "krill-app exited ($($appProc.ExitCode))" }
    }
    New-Item -ItemType File (Join-Path $dir 'go') | Out-Null
    $deadline = (Get-Date).AddSeconds(600)
    while (@(0..($Sessions - 1) | Where-Object { -not (Test-Path (Join-Path $dir "done-$_")) }).Count) {
        if ((Get-Date) -gt $deadline) { Stop-Gate 2 'generator output did not finish in 600s' }
        Start-Sleep -Milliseconds 200
    }
    Start-Sleep $Idle

    $kids = @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$($holder.Id)")
    $krilld = @($kids | Where-Object Name -eq 'krilld.exe')
    if (-not $krilld) { Stop-Gate 2 'krilld is not running under krill-holder' }
    $hosts = @($kids | Where-Object { $_.Name -in 'conhost.exe', 'OpenConsole.exe' })

    # PrivateMemorySize64 = PROCESS_MEMORY_COUNTERS_EX.PrivateUsage. Private WS는 CIM(지역화되지 않는 클래스 이름).
    function Get-Mem([int[]]$ids) {
        $priv = 0; $ws = 0
        foreach ($id in $ids) {
            $priv += (Get-Process -Id $id).PrivateMemorySize64
            $ws += [int64](Get-CimInstance Win32_PerfFormattedData_PerfProc_Process -Filter "IDProcess=$id").WorkingSetPrivate
        }
        [pscustomobject]@{ Private = $priv; WS = $ws }
    }
    $rows = [ordered]@{
        holder = Get-Mem @($holder.Id)
        krilld = Get-Mem @($krilld.ProcessId)
    }
    $daemon = $rows.holder.Private + $rows.krilld.Private
    $rows.daemon = [pscustomobject]@{ Private = $daemon; WS = $rows.holder.WS + $rows.krilld.WS }
    if ($appProc) {
        $rows.app = Get-Mem @($appProc.Id)
        $rows.total = [pscustomobject]@{ Private = $daemon + $rows.app.Private; WS = $rows.daemon.WS + $rows.app.WS }
    }
    $hostMem = Get-Mem @($hosts | ForEach-Object ProcessId)
    $fmt = { param($b) '{0:N1}' -f ($b / 1MB) }
    $line = ($rows.Keys | ForEach-Object { "$_ $(& $fmt $rows[$_].Private)" }) -join ' '
    $wsLine = ($rows.Keys | ForEach-Object { "$_ $(& $fmt $rows[$_].WS)" }) -join ' '
    "MKW $Sessions sessions: $line MiB PrivateUsage (limits: daemon 60 / app 120 / total 200 MiB)"
    "MKW private WS: $wsLine MiB; pty hosts ($($hosts.Count)) private $(& $fmt $hostMem.Private) / WS $(& $fmt $hostMem.WS) MiB (not gated)"

    $over = @()
    if ($daemon -gt $DaemonLimit) { $over += 'daemon > 60 MiB' }
    if ($appProc -and $rows.app.Private -gt $AppLimit) { $over += 'app > 120 MiB' }
    if ($appProc -and $rows.total.Private -gt $TotalLimit) { $over += 'total > 200 MiB' }
    if ($over) { Stop-Gate 1 "memory gate failed: $($over -join ', ')" }
    'memory-gate-windows: ok'
} finally {
    if ($appProc -and -not $appProc.HasExited) { Stop-Process -Id $appProc.Id -Force -ErrorAction SilentlyContinue }
    # holder 트리(krilld·세션·ConPTY 호스트)를 함께 끝낸다.
    # 이미 끝난 자식이 있으면 taskkill이 stderr에 쓴다. Stop 설정에서 그걸 실패로 바꾸지 않게 cmd로 삼킨다.
    if ($holder) { cmd.exe /c "taskkill /T /F /PID $($holder.Id) >nul 2>&1" }
    Remove-Item Env:KRILL_STATE_DIR, Env:KRILL_TEST_BALLAST_MB -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
}
