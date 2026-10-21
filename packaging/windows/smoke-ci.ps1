<#
Run the packaged Windows smoke outside the Actions runner's restrictive Job.
Win32_Process.Create starts a separate process tree; the smoke still exercises
normal CLI bootstrap, installation, PTY input and daemon-kill survival.
https://learn.microsoft.com/windows/win32/procthread/job-objects
#>
param([Parameter(Mandatory)][string]$Zip)
$ErrorActionPreference = 'Stop'
$smoke = Join-Path $PSScriptRoot 'smoke.ps1'
if ($env:GITHUB_ACTIONS -ne 'true') { & $smoke -Zip $Zip; return }
$zipPath = (Resolve-Path $Zip).Path
$root = Join-Path $env:RUNNER_TEMP ("krill-smoke-ci-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $root | Out-Null
$wrapper = Join-Path $root 'run.ps1'
$resultPath = Join-Path $root 'result.json'
$logPath = Join-Path $root 'smoke.log'
$body = @'
$ErrorActionPreference = 'Stop'
$env:TEMP = '__ROOT__'
$env:TMP = '__ROOT__'
$env:KRILL_LOG_DIR = Join-Path $env:TEMP 'logs'
function Complete-Smoke($value) {
    $value | ConvertTo-Json | Set-Content -Encoding UTF8 '__RESULT__.tmp'
    Move-Item -LiteralPath '__RESULT__.tmp' -Destination '__RESULT__'
}
try {
    Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class KrillSmokeJob { [DllImport("kernel32.dll", SetLastError=true)] public static extern bool IsProcessInJob(IntPtr process, IntPtr job, out bool result); }'
    $inJob = $false
    if (-not [KrillSmokeJob]::IsProcessInJob([Diagnostics.Process]::GetCurrentProcess().Handle, [IntPtr]::Zero, [ref]$inJob)) { throw 'IsProcessInJob failed' }
    if ($inJob) { throw 'Smoke process still belongs to a restrictive runner Job' }
    & '__SMOKE__' -Zip '__ZIP__' *> '__LOG__'
    Complete-Smoke @{ exitCode = 0; outsideRunnerJob = $true }
} catch {
    $failure = $_.Exception.Message
    $_ | Out-String | Add-Content '__LOG__'
    Get-ChildItem $env:KRILL_LOG_DIR -File -ErrorAction SilentlyContinue | ForEach-Object {
        "== $($_.Name)" | Add-Content '__LOG__'
        Get-Content $_.FullName -Tail 40 | Add-Content '__LOG__'
    }
    Complete-Smoke @{ exitCode = 1; error = $failure }
}
'@
foreach ($pair in @(@('__ROOT__', $root), @('__SMOKE__', $smoke), @('__ZIP__', $zipPath), @('__LOG__', $logPath), @('__RESULT__', $resultPath))) {
    $body = $body.Replace($pair[0], $pair[1].Replace("'", "''"))
}
Set-Content -Encoding UTF8 -LiteralPath $wrapper -Value $body
$powershell = Join-Path $PSHOME 'powershell.exe'
$child = $null
try {
    $child = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{
        CommandLine = ('"{0}" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "{1}"' -f $powershell, $wrapper)
    }
    if ($child.ReturnValue -ne 0) { throw "WMI smoke launch failed: $($child.ReturnValue)" }
    $deadline = (Get-Date).AddSeconds(120)
    while (-not (Test-Path -LiteralPath $resultPath)) {
        if ((Get-Date) -gt $deadline) { throw 'Packaged Windows smoke timed out' }
        Start-Sleep -Milliseconds 200
    }
    if (Test-Path -LiteralPath $logPath) { Get-Content -LiteralPath $logPath }
    $result = Get-Content -Raw -LiteralPath $resultPath | ConvertFrom-Json
    "CI smoke result: $($result | ConvertTo-Json -Compress)"
    if ($result.exitCode -ne 0) { throw "Packaged Windows smoke failed: $($result.error)" }
} finally {
    if ($child -and $child.ProcessId) {
        $process = Get-CimInstance Win32_Process -Filter "ProcessId=$($child.ProcessId)" -ErrorAction SilentlyContinue
        if ($process -and $process.CommandLine -like "*$wrapper*") { Stop-Process -Id $child.ProcessId -Force -ErrorAction SilentlyContinue }
    }
    foreach ($name in 'krill-app', 'krill', 'krilld', 'krill-holder', 'OpenConsole') {
        Get-Process $name -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$root\*" } | Stop-Process -Force -ErrorAction SilentlyContinue
    }
    Remove-Item -Recurse -Force -LiteralPath $root -ErrorAction SilentlyContinue
}
