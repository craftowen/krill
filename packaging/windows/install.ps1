<#
.SYNOPSIS
Install Krill for the current Windows user, without administrator rights.

.DESCRIPTION
Without -Zip, download the latest Windows release and verify its SHA-256 before
installing to %LOCALAPPDATA%\Programs\Krill, adding PATH and launching the app.
Existing installations are preserved; use the in-app updater to update them.

.EXAMPLE
irm https://krill.page/install.ps1 | iex

.EXAMPLE
powershell -ExecutionPolicy Bypass -File install.ps1 -Zip .\Krill-0.1.5-windows-x64.zip -AddToPath
#>
param(
    [string]$Zip,
    [string]$Dest = (Join-Path $env:LOCALAPPDATA 'Programs\Krill'),
    [string]$Release = 'latest',
    [switch]$AddToPath,
    [switch]$NoShortcut,
    [switch]$NoOpen
)
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'This installer requires Windows x64.' }
$arch = $env:PROCESSOR_ARCHITECTURE
if ($env:PROCESSOR_ARCHITEW6432) { $arch = $env:PROCESSOR_ARCHITEW6432 }
if ($arch -ne 'AMD64') { throw 'Windows release packages require an x64 PC.' }
if (Test-Path $Dest) { throw "$Dest already exists; update from the app or run: krill-app.exe --update-cli" }

$download = -not $Zip
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("krill-install-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $tmp | Out-Null
try {
    if ($download) {
        # Windows PowerShell 5.1 may otherwise negotiate an obsolete TLS version.
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
        $api = 'https://api.github.com/repos/craftowen/krill/releases/'
        if ($Release -eq 'latest') { $api += 'latest' }
        else { $api += 'tags/' + [uri]::EscapeDataString($Release) }
        $releaseInfo = Invoke-RestMethod -Uri $api -Headers @{ 'User-Agent' = 'Krill-Installer' }
        $packages = @($releaseInfo.assets | Where-Object { $_.name -cmatch '^Krill-[0-9][0-9A-Za-z.+-]*-windows-x64\.zip$' })
        if ($packages.Count -ne 1) { throw 'This release has no Windows x64 package yet. Please check https://krill.page for availability.' }
        $asset = $packages[0]
        $checksums = @($releaseInfo.assets | Where-Object { $_.name -ceq ($asset.name + '.sha256') })
        if ($checksums.Count -ne 1) { throw 'The Windows package SHA-256 file is missing.' }
        $Zip = Join-Path $tmp $asset.name
        $checksumFile = Join-Path $tmp 'package.sha256'
        foreach ($item in @(@{ Asset = $asset; File = $Zip }, @{ Asset = $checksums[0]; File = $checksumFile })) {
            $url = [uri]$item.Asset.browser_download_url
            if ($url.Scheme -ne 'https' -or $url.Host -ne 'github.com' -or
                -not $url.AbsolutePath.StartsWith('/craftowen/krill/releases/download/')) {
                throw 'Unexpected release asset URL.'
            }
            Invoke-WebRequest -UseBasicParsing -Uri $url.AbsoluteUri -OutFile $item.File
        }
        $checksum = (Get-Content -LiteralPath $checksumFile -Raw).Trim()
        if ($checksum -notmatch '^([0-9a-fA-F]{64})(?:\s+\*?([^\r\n]+))?$') { throw 'Invalid SHA-256 file.' }
        $expected = $Matches[1]
        if ($Matches[2] -and $Matches[2] -cne $asset.name) { throw 'SHA-256 filename does not match the package.' }
        if ((Get-FileHash -LiteralPath $Zip -Algorithm SHA256).Hash -ne $expected) { throw 'Windows package SHA-256 mismatch; nothing was installed.' }
        if (-not $PSBoundParameters.ContainsKey('AddToPath')) { $AddToPath = $true }
    }
    $unpacked = Join-Path $tmp 'unpacked'
    Expand-Archive -LiteralPath $Zip -DestinationPath $unpacked
    foreach ($file in @('Krill.json', 'krill-app.exe', 'krill.exe', 'krilld.exe', 'krill-holder.exe')) {
        if (-not (Test-Path -LiteralPath (Join-Path $unpacked "Krill\$file") -PathType Leaf)) {
            throw "Not a Krill package (missing $file)."
        }
    }
    New-Item -ItemType Directory -Force (Split-Path -Parent $Dest) | Out-Null
    Move-Item -LiteralPath (Join-Path $unpacked 'Krill') -Destination $Dest
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
    if (($env:Path -split ';') -notcontains $Dest) { $env:Path += ";$Dest" }
}
"installed: $Dest"
if ($download -and -not $NoOpen) { Start-Process -FilePath (Join-Path $Dest 'krill-app.exe') }
