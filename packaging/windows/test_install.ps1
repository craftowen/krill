# Exercise the release installer with local ZIP fixtures; no network or user settings.
$ErrorActionPreference = 'Stop'
$installer = Join-Path $PSScriptRoot 'install.ps1'
$root = Join-Path ([IO.Path]::GetTempPath()) ('krill-installer-test-' + [guid]::NewGuid().ToString('N'))
$savedEnv = @{}
foreach ($key in @('OS', 'PROCESSOR_ARCHITECTURE', 'PROCESSOR_ARCHITEW6432', 'LOCALAPPDATA')) {
    $savedEnv[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
}
$state = @{ Requests = 0; Assets = @(); Checksum = ""; Fixture = "" }
Set-Item Function:Invoke-RestMethod -Value { param($Uri, $Headers) $state.Requests++; @{ assets = $state.Assets } }.GetNewClosure()
Set-Item Function:Invoke-WebRequest -Value {
    param($Uri, $OutFile, [switch]$UseBasicParsing)
    if ($Uri.EndsWith('.sha256')) { Set-Content -LiteralPath $OutFile -Value $state.Checksum -Encoding ascii }
    else { Copy-Item -LiteralPath $state.Fixture -Destination $OutFile }
}.GetNewClosure()
function Assert($Condition, $Message) { if (-not $Condition) { throw $Message } }
function Install($Dest, $Extra = @{}) {
    $options = @{ Dest = $Dest; NoShortcut = $true; NoOpen = $true; AddToPath = $false }
    foreach ($key in $Extra.Keys) { $options[$key] = $Extra[$key] }
    & $installer @options | Out-Null
}
function Reject($Name, $Pattern, $Extra = @{}) {
    $dest = Join-Path $root $Name
    $caught = $false
    try { Install $dest $Extra }
    catch { Assert ($_.Exception.Message -match $Pattern) "Unexpected error for ${Name}: $_"; $caught = $true }
    Assert $caught "$Name should fail"
    Assert (-not (Test-Path $dest)) "$Name changed the destination"
}
try {
    New-Item -ItemType Directory -Path (Join-Path $root 'source/Krill') -Force | Out-Null
    $env:OS = 'Windows_NT'
    $env:PROCESSOR_ARCHITECTURE = 'AMD64'
    $env:PROCESSOR_ARCHITEW6432 = ''
    $env:LOCALAPPDATA = $root
    foreach ($name in @('Krill.json', 'krill-app.exe', 'krill.exe', 'krilld.exe', 'krill-holder.exe')) {
        Set-Content -LiteralPath (Join-Path $root "source/Krill/$name") -Value 'fixture'
    }
    $state.Fixture = Join-Path $root 'fixture.zip'
    Compress-Archive -Path (Join-Path $root 'source/Krill') -DestinationPath $state.Fixture
    $name = 'Krill-0.1.99-windows-x64.zip'
    $url = "https://github.com/craftowen/krill/releases/download/v0.1.99/$name"
    $package = @{ name = $name; browser_download_url = $url }
    $shaAsset = @{ name = "$name.sha256"; browser_download_url = "$url.sha256" }
    $state.Assets = @($package, $shaAsset)
    $goodChecksum = (Get-FileHash -LiteralPath $state.Fixture).Hash + "  $name"
    $state.Checksum = $goodChecksum
    Install (Join-Path $root 'downloaded')
    Assert (Test-Path (Join-Path $root 'downloaded/krill.exe')) 'Release installation failed'
    Install (Join-Path $root 'offline') @{ Zip = $state.Fixture }
    Assert (Test-Path (Join-Path $root 'offline/krill-app.exe')) 'Offline installation failed'

    $before = $state.Requests
    Set-Content -LiteralPath (Join-Path $root 'downloaded/user-data.txt') -Value 'preserve me'
    Install (Join-Path $root 'downloaded')
    Install (Join-Path $root 'downloaded') @{ Zip = (Join-Path $root 'does-not-exist.zip') }
    Assert ((Get-Content (Join-Path $root 'downloaded/user-data.txt') -Raw).Trim() -eq 'preserve me') 'User files were modified'
    Assert ($state.Requests -eq $before) 'Existing install triggered network access'
    Assert ((Get-Content (Join-Path $root 'downloaded/krill.exe') -Raw).Trim() -eq 'fixture') 'Existing install was modified'

    $partial = Join-Path $root 'partial'
    New-Item -ItemType Directory $partial | Out-Null
    Set-Content -LiteralPath (Join-Path $partial 'keep.txt') -Value 'preserve me'
    $before = $state.Requests
    $caught = $false
    try { Install $partial }
    catch { Assert ($_.Exception.Message -match 'not a complete Krill installation') 'Wrong partial-install error'; $caught = $true }
    Assert $caught 'Partial install should require choosing another destination'
    Assert ($state.Requests -eq $before) 'Partial install triggered network access'
    Assert ((Get-Content (Join-Path $partial 'keep.txt') -Raw).Trim() -eq 'preserve me') 'Partial install was modified'

    $state.Checksum = ('0' * 64) + "  $name"
    Reject 'bad-hash' 'SHA-256 mismatch'
    $state.Checksum = (Get-FileHash -LiteralPath $state.Fixture).Hash + '  other.zip'
    Reject 'wrong-name' 'filename does not match'
    $state.Checksum = 'not a checksum'
    Reject 'malformed-hash' 'Invalid SHA-256'
    $state.Checksum = $goodChecksum
    $state.Assets = @($package)
    Reject 'missing-sha' 'SHA-256 file is missing'
    $state.Assets = @()
    Reject 'unreleased' 'no Windows x64 package yet'
    $state.Assets = @($package, $package, $shaAsset)
    Reject 'ambiguous' 'no Windows x64 package yet'
    $state.Assets = @(@{ name = $name; browser_download_url = 'https://example.com/package.zip' }, $shaAsset)
    Reject 'untrusted-url' 'Unexpected release asset URL'
    $state.Assets = @($package, $shaAsset)

    $env:PROCESSOR_ARCHITECTURE = 'ARM64'
    Reject 'wrong-arch' 'require an x64 PC'
    $env:PROCESSOR_ARCHITECTURE = 'AMD64'
    $env:OS = 'Other'
    Reject 'wrong-os' 'requires Windows'
    $env:OS = 'Windows_NT'

    Remove-Item -LiteralPath (Join-Path $root 'source/Krill/krilld.exe')
    $incomplete = Join-Path $root 'incomplete.zip'
    Compress-Archive -Path (Join-Path $root 'source/Krill') -DestinationPath $incomplete
    Reject 'incomplete' 'missing krilld.exe' @{ Zip = $incomplete }
    'Windows installer: download, offline install, collision preservation and rejection cases passed.'
} finally {
    foreach ($key in $savedEnv.Keys) { [Environment]::SetEnvironmentVariable($key, $savedEnv[$key], 'Process') }
    Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
}
