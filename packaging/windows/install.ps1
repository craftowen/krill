<#
.SYNOPSIS
Krill zip을 사용자 단위로 설치한다(OWE-837). 관리자 권한이 필요 없다.

.DESCRIPTION
zip을 <Dest>(기본 %LOCALAPPDATA%\Programs\Krill)에 풀고 시작 메뉴 바로 가기를 만든다.
이미 설치돼 있으면 거부한다. 업데이트는 앱 안 업데이터나 `krill-app.exe --update-cli`가 한다.
상태·세그먼트는 %LOCALAPPDATA%\Krill\ 에 생기며 설치 폴더에는 쓰지 않는다(CONVENTIONS A5).

.EXAMPLE
powershell -ExecutionPolicy Bypass -File install.ps1 -Zip .\Krill-0.1.5-windows-x64.zip -AddToPath
#>
param(
    [Parameter(Mandatory)][string]$Zip,
    [string]$Dest = (Join-Path $env:LOCALAPPDATA 'Programs\Krill'),
    [switch]$AddToPath,
    [switch]$NoShortcut
)
$ErrorActionPreference = 'Stop'
if (Test-Path $Dest) { throw "$Dest already exists; update from the app or run: krill-app.exe --update-cli" }

$tmp = "$Dest.install-$PID"
New-Item -ItemType Directory -Force $tmp | Out-Null
try {
    & "$env:SystemRoot\System32\tar.exe" -xf $Zip -C $tmp
    if ($LASTEXITCODE -ne 0) { throw "tar failed: $LASTEXITCODE" }
    if (-not (Test-Path (Join-Path $tmp 'Krill\Krill.json'))) { throw 'not a Krill package (missing Krill\Krill.json)' }
    Move-Item (Join-Path $tmp 'Krill') $Dest
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

if (-not $NoShortcut) {
    $lnk = Join-Path ([Environment]::GetFolderPath('Programs')) 'Krill.lnk'
    $s = (New-Object -ComObject WScript.Shell).CreateShortcut($lnk)
    $s.TargetPath = Join-Path $Dest 'krill-app.exe'
    # 작업 디렉터리를 설치 폴더로 두지 않는다(실행 중 프로세스의 cwd가 설치 폴더면 교체가 어려워진다).
    $s.WorkingDirectory = $env:USERPROFILE
    $s.Save()
}
if ($AddToPath) {
    $path = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (($path -split ';') -notcontains $Dest) {
        [Environment]::SetEnvironmentVariable('Path', ((@($path, $Dest) | Where-Object { $_ }) -join ';'), 'User')
    }
}
"installed: $Dest"
