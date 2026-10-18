<#
.SYNOPSIS
Krill Windows zip 묶음(OWE-837). 서명·업로드·원격 게시는 하지 않는다(Authenticode는 OWE-838).

.DESCRIPTION
release 빌드 산출물을 <OutDir>\Krill\ 에 모으고 Krill-<Version>-windows-x64.zip 과 SHA-256을 만든다.
zip 안 레이아웃(업데이터 updater.rs·rename.rs와 holder sys/windows.rs가 기대하는 위치):

  Krill\Krill.json               CFBundleShortVersionString·KrillUpdatePublicKey·KrillUpdateURL (Info.plist와 같은 키)
  Krill\krill-app.exe, krill.exe, krilld.exe, krill-holder.exe, [krill-link.exe]
  Krill\conpty.dll               (B안, -ConptyDir를 줄 때) holder가 자기 exe 옆에서 로드한다
  Krill\x64\OpenConsole.exe      (B안) conpty.dll이 <dll 폴더>\x64\OpenConsole.exe를 띄운다
  Krill\THIRD_PARTY_NOTICES.txt  ConPTY MIT 고지

OpenConsole 바이너리는 저장소에 넣지 않는다. NuGet Microsoft.Windows.Console.ConPTY 1.25.260930003을 내려받아
압축을 푼 폴더를 -ConptyDir로 준다. 고정 해시와 다르면 거부한다(README.md).

.EXAMPLE
$env:RUSTFLAGS = '-C target-feature=+crt-static'
cargo build --release --locked -p krill-app -p krill -p krilld -p krill-holder -p krill-link
powershell -ExecutionPolicy Bypass -File packaging\windows\bundle.ps1 -Version 0.1.5 -ConptyDir C:\deps\conpty-1.25.260930003 -PublicKey <64hex>
#>
param(
    [Parameter(Mandatory)][string]$Version,
    [string]$TargetDir = 'target\release',
    [string]$ConptyDir = '',
    [string]$PublicKey = '',
    [string]$UpdateUrl = '',
    [string]$OutDir = 'packaging\dist'
)
$ErrorActionPreference = 'Stop'

# NuGet Microsoft.Windows.Console.ConPTY 1.25.260930003 (MIT) x64 파일과 SHA-256(OWE-826에서 받은 패키지 실측).
$ConptyPins = [ordered]@{
    'runtimes\win-x64\native\conpty.dll'        = @('conpty.dll', 'feeef341d891643c62d30b6b07800bc70f0bc148f44cb8c3bee84aa557ae805a')
    'build\native\runtimes\x64\OpenConsole.exe' = @('x64\OpenConsole.exe', '3d66b23d0a71bb8eed2b77edc8b9df9bf54ce6c8fb74c863a30e760997f80586')
}

$root = Join-Path $OutDir 'Krill'
$zip = Join-Path $OutDir "Krill-$Version-windows-x64.zip"
# 기존 산출물을 덮어쓰지 않는다(macOS release.sh와 같다).
foreach ($p in $root, $zip) { if (Test-Path $p) { throw "$p already exists" } }
New-Item -ItemType Directory -Force $root | Out-Null

foreach ($name in 'krill-app', 'krill', 'krilld', 'krill-holder') {
    Copy-Item (Join-Path $TargetDir "$name.exe") $root
}
$link = Join-Path $TargetDir 'krill-link.exe'
if (Test-Path $link) { Copy-Item $link $root }

if ($ConptyDir) {
    foreach ($src in $ConptyPins.Keys) {
        $dst, $want = $ConptyPins[$src]
        $from = Join-Path $ConptyDir $src
        $got = (Get-FileHash -Algorithm SHA256 $from).Hash.ToLowerInvariant()
        if ($got -ne $want) { throw "sha256 mismatch for ${from}: $got" }
        $to = Join-Path $root $dst
        New-Item -ItemType Directory -Force (Split-Path $to) | Out-Null
        Copy-Item $from $to
    }
}
Copy-Item (Join-Path $PSScriptRoot 'THIRD_PARTY_NOTICES.txt') $root

$info = [ordered]@{ CFBundleShortVersionString = $Version }
if ($PublicKey) { $info.KrillUpdatePublicKey = $PublicKey }
if ($UpdateUrl) { $info.KrillUpdateURL = $UpdateUrl }
# serde_json은 BOM을 거부한다. 내용이 ASCII라 -Encoding ascii로 BOM 없이 쓴다(PowerShell 5.1의 utf8은 BOM을 붙인다).
$info | ConvertTo-Json | Set-Content -Encoding ascii (Join-Path $root 'Krill.json')

& "$env:SystemRoot\System32\tar.exe" -a -c -f $zip -C $OutDir Krill
if ($LASTEXITCODE -ne 0) { throw "tar failed: $LASTEXITCODE" }
"$((Get-FileHash -Algorithm SHA256 $zip).Hash.ToLowerInvariant())  $(Split-Path -Leaf $zip)"
