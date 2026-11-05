# Read the linked PE resource, rather than matching the .rc source text.
param([Parameter(Mandatory)][string]$TargetDir, [Parameter(Mandatory)][string]$Version)
$ErrorActionPreference = 'Stop'
$descriptions = @{
    'krill-app' = 'Krill Desktop'
    'krill' = 'Krill Command Line'
    'krilld' = 'Krill Session Daemon'
    'krill-holder' = 'Krill Terminal Holder'
    'krill-link' = 'Krill Remote Link'
}
foreach ($name in $descriptions.Keys) {
    $file = Get-Item -LiteralPath (Join-Path $TargetDir "$name.exe")
    $info = $file.VersionInfo
    $expected = @{
        CompanyName = 'Krill contributors'; ProductName = 'Krill';
        FileDescription = $descriptions[$name]; OriginalFilename = "$name.exe";
        InternalName = $name; FileVersion = $Version; ProductVersion = $Version
    }
    foreach ($field in $expected.Keys) {
        if ($info.$field -ne $expected[$field]) { throw "$name $field expected $($expected[$field]); got $($info.$field)" }
    }
    Write-Host "PASS $name.exe VERSIONINFO $($info.FileVersion) $($info.FileDescription)"
}
