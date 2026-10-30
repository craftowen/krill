# Real PowerShell packaging contract; supply the already verified pinned NuGet directory.
param([Parameter(Mandatory)][string]$ConptyDir)
$ErrorActionPreference = 'Stop'
$bundle = Join-Path $PSScriptRoot 'bundle.ps1'
$root = Join-Path ([IO.Path]::GetTempPath()) ('krill-bundle-test-' + [guid]::NewGuid().ToString('N'))
function Assert($Condition, $Message) { if (-not $Condition) { throw $Message } }
function Reject($Name, $Source) {
    $out = Join-Path $root $Name
    $caught = $false
    try { & $bundle -Version '0.0.0' -TargetDir $target -ConptyDir $Source -OutDir $out | Out-Null }
    catch { $caught = $true }
    Assert $caught "$Name unexpectedly accepted a missing or corrupt ConPTY backend"
    Assert (-not (Test-Path -LiteralPath $out)) "$Name created partial package output"
}
try {
    $target = Join-Path $root 'binaries'
    New-Item -ItemType Directory -Force $target | Out-Null
    foreach ($name in @('krill-app', 'krill', 'krilld', 'krill-holder')) {
        Set-Content -LiteralPath (Join-Path $target "$name.exe") -Value 'fixture' -Encoding ascii
    }
    Reject 'missing-argument' ''
    Reject 'missing-files' (Join-Path $root 'absent')
    $broken = Join-Path $root 'broken'
    $dll = Join-Path $broken 'runtimes\win-x64\native\conpty.dll'
    New-Item -ItemType Directory -Force (Split-Path $dll) | Out-Null
    Copy-Item -LiteralPath (Join-Path $ConptyDir 'runtimes\win-x64\native\conpty.dll') -Destination $dll
    Reject 'missing-console' $broken
    Set-Content -LiteralPath $dll -Value 'corrupt' -Encoding ascii
    Reject 'corrupt-backend' $broken
    Copy-Item -LiteralPath (Join-Path $ConptyDir 'runtimes\win-x64\native\conpty.dll') -Destination $dll
    $console = Join-Path $broken 'build\native\runtimes\x64\OpenConsole.exe'
    New-Item -ItemType Directory -Force (Split-Path $console) | Out-Null
    Set-Content -LiteralPath $console -Value 'corrupt' -Encoding ascii
    Reject 'corrupt-console' $broken

    $out = Join-Path $root 'valid'
    & $bundle -Version '0.0.0' -TargetDir $target -ConptyDir $ConptyDir -OutDir $out | Out-Null
    $zip = Join-Path $out 'Krill-0.0.0-windows-x64.zip'
    Assert (Test-Path -LiteralPath $zip) 'Valid pinned backend did not produce a ZIP'
    $unpacked = Join-Path $root 'unpacked'
    Expand-Archive -LiteralPath $zip -DestinationPath $unpacked
    foreach ($name in @('conpty.dll', 'x64\OpenConsole.exe', 'Krill.json', 'krill-holder.exe')) {
        Assert (Test-Path -LiteralPath (Join-Path $unpacked "Krill\$name")) "ZIP missing $name"
    }
    $info = Get-Content -LiteralPath (Join-Path $unpacked 'Krill\Krill.json') -Raw | ConvertFrom-Json
    Assert ($info.CFBundleShortVersionString -eq '0.0.0') 'ZIP version mismatch'
    'Windows bundle: missing/corrupt backend rejected before output; pinned backend ZIP passed.'
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
