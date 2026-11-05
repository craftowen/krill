<#
.SYNOPSIS
Fail-closed Defender gate for the exact Windows release ZIP. Never remediates scan targets.
.EXAMPLE
powershell -NoProfile -ExecutionPolicy Bypass -File packaging/windows/defender-scan.ps1 -Zip packaging/dist/Krill-0.2.51-windows-x64.zip
#>
param([Parameter(Mandatory)][string]$Zip)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
# PowerShell 7 must leave native exit-code handling to the checks below (2 means detection/error).
$PSNativeCommandUseErrorActionPreference = $false

function Assert-DefenderReady {
    $status = Get-MpComputerStatus
    $status | Select-Object AMServiceEnabled, AntivirusEnabled, AMRunningMode, AMEngineVersion,
        AMProductVersion, AntivirusSignatureVersion, AntivirusSignatureLastUpdated | Format-List | Out-Host
    if (-not $status.AMServiceEnabled -or -not $status.AntivirusEnabled -or $status.AMRunningMode -ne 'Normal') {
        throw 'Defender Antivirus must be running in Normal mode; disabled/passive engines cannot approve a release.'
    }
}
function Get-DefenderCommand {
    $platform = Join-Path $env:ProgramData 'Microsoft\Windows Defender\Platform'
    $candidate = Get-ChildItem -LiteralPath $platform -Filter MpCmdRun.exe -Recurse -ErrorAction SilentlyContinue |
        Sort-Object { [version]('{0}.{1}.{2}.{3}' -f $_.VersionInfo.FileMajorPart, $_.VersionInfo.FileMinorPart, $_.VersionInfo.FileBuildPart, $_.VersionInfo.FilePrivatePart) } -Descending | Select-Object -First 1
    if ($candidate) { return $candidate.FullName }
    $fallback = Join-Path $env:ProgramFiles 'Windows Defender\MpCmdRun.exe'
    if (-not (Test-Path -LiteralPath $fallback -PathType Leaf)) { throw 'MpCmdRun.exe not found' }
    return $fallback
}

$zipPath = (Resolve-Path -LiteralPath $Zip).Path
$zipHash = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash
Write-Host "ZIP SHA256 $zipHash $zipPath"
Assert-DefenderReady
$mp = Get-DefenderCommand
Write-Host "Defender command: $mp"
& $mp -SignatureUpdate | Out-Host
if ($LASTEXITCODE -ne 0) { throw "Defender signature update failed: $LASTEXITCODE" }
Assert-DefenderReady
# A signature update can replace the platform executable.
$mp = Get-DefenderCommand
$root = Join-Path ([IO.Path]::GetTempPath()) ('krill-defender-' + [guid]::NewGuid().ToString('N'))
try {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::OpenRead($zipPath)
    try {
        # Inventory the ZIP, not only the extracted directory: real-time quarantine must never silently skip a file.
        $entries = @($archive.Entries | Where-Object { $_.FullName -match '(?i)\.(exe|dll)$' } |
            Select-Object -ExpandProperty FullName)
    } finally { $archive.Dispose() }
    if ($entries.Count -eq 0) { throw 'Release ZIP contains no EXE/DLL files' }
    Expand-Archive -LiteralPath $zipPath -DestinationPath $root
    $failed = @()
    foreach ($entry in $entries) {
        $file = [IO.Path]::GetFullPath((Join-Path $root $entry))
        if (-not $file.StartsWith(($root + [IO.Path]::DirectorySeparatorChar), [StringComparison]::OrdinalIgnoreCase)) {
            throw "ZIP entry escapes scan directory: $entry"
        }
        if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "ZIP binary missing before scan (possibly quarantined): $entry" }
        $before = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash
        Write-Host "SCAN SHA256 $before $entry"
        & $mp -Scan -ScanType 3 -File $file -DisableRemediation | Out-Host
        $code = $LASTEXITCODE
        Write-Host "SCAN EXIT $code $entry"
        if ($code -ne 0) { $failed += "$entry (exit $code)" }
        if (-not (Test-Path -LiteralPath $file -PathType Leaf) -or
            (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash -ne $before) {
            throw "ZIP binary changed/disappeared during scan: $entry"
        }
    }
    if ((Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash -ne $zipHash) { throw 'ZIP changed during scan' }
    Assert-DefenderReady
    if ($failed.Count -gt 0) { throw "Defender rejected release: $($failed -join ', ')" }
    Write-Host "Defender gate passed: $($entries.Count) EXE/DLL files; ZIP SHA256 $zipHash"
} finally {
    if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
}
