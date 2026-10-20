<#
.SYNOPSIS
Windows 릴리스 빌드: +crt-static cargo 빌드 → 고정 ConPTY nupkg 확인 → bundle.ps1 → zip·.sha256. 서명·업로드는 하지 않는다.

.DESCRIPTION
소스 루트에서 실행한다. ConPTY nupkg는 -CacheDir에 한 번 받아 두고 해시를 확인해 재사용한다(README.md 표).
결과: <OutDir>\Krill-<Version>-windows-x64.zip 과 .zip.sha256.

.EXAMPLE
powershell -ExecutionPolicy Bypass -File packaging\windows\build.ps1 -Version 0.1.20 -PublicKey <64hex> `
  -UpdateUrl https://github.com/OWNER/REPO/releases/latest/download/latest-windows.json
#>
param(
    [Parameter(Mandatory)][string]$Version,
    [string]$PublicKey = '',
    [string]$UpdateUrl = '',
    [string]$CacheDir = (Join-Path $env:LOCALAPPDATA 'krill-release-cache'),
    [string]$OutDir = 'packaging\dist'
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$cargoVer = (Select-String -Path Cargo.toml -Pattern '^version = "([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
if ($cargoVer -ne $Version) { throw "version ($Version) does not match Cargo.toml ($cargoVer)" }

# +crt-static: VCRUNTIME140.dll 의존 제거(docs/research/windows-spike-OWE-837.md §4).
$env:RUSTFLAGS = '-C target-feature=+crt-static'
cargo build --release --locked -p krill-app -p krill -p krilld -p krill-holder -p krill-link
if ($LASTEXITCODE -ne 0) { throw "cargo build failed: $LASTEXITCODE" }
$targetDir = if ($env:CARGO_TARGET_DIR) { Join-Path $env:CARGO_TARGET_DIR 'release' } else { 'target\release' }

# ConPTY B안 파일(저장소에 넣지 않음). nupkg 해시를 확인하고, 안의 두 파일 해시는 bundle.ps1이 다시 확인한다.
$conptyVer = '1.25.260930003'
$conptyDir = Join-Path $CacheDir "conpty-$conptyVer"
if (-not (Test-Path $conptyDir)) {
    New-Item -ItemType Directory -Force $CacheDir | Out-Null
    $nupkg = Join-Path $CacheDir "conpty-$conptyVer.nupkg"
    & "$env:SystemRoot\System32\curl.exe" -fsSL -o $nupkg "https://www.nuget.org/api/v2/package/Microsoft.Windows.Console.ConPTY/$conptyVer"
    if ($LASTEXITCODE -ne 0) { throw "conpty download failed: $LASTEXITCODE" }
    $got = (Get-FileHash -Algorithm SHA256 $nupkg).Hash.ToLowerInvariant()
    if ($got -ne '02b07b349af66d801159bdf9e440d4a1ce78bb951f37fc8609731665afdae7ee') { throw "conpty.nupkg sha256 mismatch: $got" }
    $tmp = "$conptyDir.tmp"
    New-Item -ItemType Directory -Force $tmp | Out-Null
    & "$env:SystemRoot\System32\tar.exe" -xf $nupkg -C $tmp
    if ($LASTEXITCODE -ne 0) { throw "tar failed: $LASTEXITCODE" }
    Move-Item $tmp $conptyDir
}

$a = @{ Version = $Version; TargetDir = $targetDir; ConptyDir = $conptyDir; OutDir = $OutDir }
if ($PublicKey) { $a.PublicKey = $PublicKey }
if ($UpdateUrl) { $a.UpdateUrl = $UpdateUrl }
$line = & (Join-Path $PSScriptRoot 'bundle.ps1') @a
Set-Content -Encoding ascii (Join-Path $OutDir "Krill-$Version-windows-x64.zip.sha256") $line
$line
