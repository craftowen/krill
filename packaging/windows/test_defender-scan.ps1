# Contract tests use a fake native scanner; actual engine evidence requires defender-scan.ps1 on Windows.
$ErrorActionPreference = 'Stop'
$root = Join-Path ([IO.Path]::GetTempPath()) ('krill-defender-test-' + [guid]::NewGuid().ToString('N'))
$scan = Join-Path $PSScriptRoot 'defender-scan.ps1'
function Assert($Condition, $Message) { if (-not $Condition) { throw $Message } }
function Get-MpComputerStatus {
    [pscustomobject]@{ AMServiceEnabled = ($env:KRILL_DEFENDER_TEST_MODE -ne 'disabled');
        AntivirusEnabled = $true; AMRunningMode = $(if ($env:KRILL_DEFENDER_TEST_MODE -eq 'passive') { 'Passive' } else { 'Normal' }) }
}
function Get-ChildItem {
    [CmdletBinding()]
    param([string]$LiteralPath, [string]$Filter, [switch]$Recurse)
    if ($LiteralPath -eq (Join-Path $env:ProgramData 'Microsoft\Windows Defender\Platform')) {
        [pscustomobject]@{ FullName = (Join-Path $root 'MpCmdRun.exe');
            VersionInfo = [pscustomobject]@{ FileMajorPart = 4; FileMinorPart = 18; FileBuildPart = 1; FilePrivatePart = 0 } }
    } else { Microsoft.PowerShell.Management\Get-ChildItem @PSBoundParameters }
}
function Expand-Archive {
    [CmdletBinding()]
    param([string]$LiteralPath, [string]$DestinationPath)
    Microsoft.PowerShell.Archive\Expand-Archive @PSBoundParameters
    if ($env:KRILL_DEFENDER_TEST_MODE -eq 'quarantined') {
        Remove-Item -LiteralPath (Join-Path $DestinationPath 'Krill\krill.EXE')
    }
}
function Case($Mode, $Zip, $ExpectedError, $ExpectedCalls) {
    $env:KRILL_DEFENDER_TEST_MODE = $Mode
    [IO.File]::WriteAllText($env:KRILL_DEFENDER_TEST_CALLS, '')
    $message = ''
    try { & $scan -Zip $Zip } catch { $message = $_.Exception.Message }
    if ($ExpectedError) { Assert ($message -like "*$ExpectedError*") "$Mode expected $ExpectedError; got $message" }
    else { Assert (-not $message) "$Mode unexpectedly failed: $message" }
    $calls = @(Get-Content -LiteralPath $env:KRILL_DEFENDER_TEST_CALLS)
    Assert ($calls.Count -eq $ExpectedCalls) "$Mode expected $ExpectedCalls calls; got $($calls.Count)"
    if ($Mode -eq 'clean' -or $Mode -eq 'detect') {
        Assert ($calls[0] -eq '-SignatureUpdate') 'Signatures must update before scans'
        Assert (@($calls | Where-Object { $_ -match '-Scan -ScanType 3 -File .* -DisableRemediation' }).Count -eq 2) 'Every nested EXE/DLL must use non-remediating custom scans'
    }
    Write-Host "PASS $Mode"
}
$savedMode = $env:KRILL_DEFENDER_TEST_MODE
$savedCalls = $env:KRILL_DEFENDER_TEST_CALLS
try {
    $bin = Join-Path $root 'fixture\Krill'
    New-Item -ItemType Directory -Force (Join-Path $bin 'nested') | Out-Null
    Set-Content -LiteralPath (Join-Path $bin 'krill.EXE') -Value 'exe fixture' -Encoding ascii
    Set-Content -LiteralPath (Join-Path $bin 'nested\conpty.dll') -Value 'dll fixture' -Encoding ascii
    $zip = Join-Path $root 'release.zip'
    Compress-Archive -LiteralPath $bin -DestinationPath $zip
    Set-Content -LiteralPath (Join-Path $root 'readme.txt') -Value 'no binaries'
    $empty = Join-Path $root 'empty.zip'
    Compress-Archive -LiteralPath (Join-Path $root 'readme.txt') -DestinationPath $empty
    $env:KRILL_DEFENDER_TEST_CALLS = Join-Path $root 'calls.txt'
    Add-Type -OutputAssembly (Join-Path $root 'MpCmdRun.exe') -OutputType ConsoleApplication -TypeDefinition @'
using System;
using System.IO;
public class FakeDefender {
    public static int Main(string[] args) {
        File.AppendAllText(Environment.GetEnvironmentVariable("KRILL_DEFENDER_TEST_CALLS"), String.Join(" ", args) + "\n");
        string mode = Environment.GetEnvironmentVariable("KRILL_DEFENDER_TEST_MODE");
        if (args[0] == "-SignatureUpdate") return mode == "update-error" ? 5 : 0;
        return mode == "detect" ? 2 : 0;
    }
}
'@
    Case 'clean' $zip '' 3
    Case 'detect' $zip 'Defender rejected release' 3
    Case 'update-error' $zip 'signature update failed' 1
    Case 'disabled' $zip 'must be running in Normal mode' 0
    Case 'passive' $zip 'must be running in Normal mode' 0
    Case 'quarantined' $zip 'ZIP binary missing before scan' 1
    Case 'empty' $empty 'contains no EXE/DLL' 1
    'Defender gate contracts passed (fake scanner; no system configuration changes).'
} finally {
    $env:KRILL_DEFENDER_TEST_MODE = $savedMode
    $env:KRILL_DEFENDER_TEST_CALLS = $savedCalls
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
