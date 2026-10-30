<#
.SYNOPSIS
Install Krill for the current Windows user, without administrator rights.

.DESCRIPTION
Without -Zip, download the latest Windows release and verify its SHA-256 before
installing to %LOCALAPPDATA%\Programs\Krill, adding PATH and launching the app.
Running the installer again preserves an existing installation and repairs its
shortcut/PATH entry. Use the in-app updater to update installed binaries.

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
$required = @('Krill.json', 'krill-app.exe', 'krill.exe', 'krilld.exe', 'krill-holder.exe', 'conpty.dll', 'x64\OpenConsole.exe')
$alreadyInstalled = Test-Path -LiteralPath $Dest
if ($alreadyInstalled) {
    foreach ($file in $required) {
        if (-not (Test-Path -LiteralPath (Join-Path $Dest $file) -PathType Leaf)) {
            throw "$Dest already exists but is not a complete Krill installation (missing $file). Its files were preserved; choose another destination or move it aside before retrying."
        }
    }
}

# -----------------------------------------------------------------------------
# Trench Clack Pipeline UI (docs/STYLEGUIDE.md tokens, PS 5.1 & PS 7 compatible)
# Uses 7-bit safe \uXXXX unescaping so Windows PowerShell 5.1 without UTF-8 BOM
# never corrupts box-drawing, arc spinner, or Korean status strings.
# -----------------------------------------------------------------------------
function Convert-KrillUnicode([string]$Text) {
    return [regex]::Unescape($Text)
}

$krillUi = $false
if ($env:KRILL_FORCE_UI -eq '1') {
    $krillUi = $true
} elseif ($env:KRILL_INSTALL_UI -ne '0' -and $env:TERM -ne 'dumb') {
    try {
        if ([Environment]::UserInteractive -and -not [Console]::IsErrorRedirected) {
            $krillUi = $true
        }
    } catch {
        $krillUi = $false
    }
}

$krillColor = $krillUi -and [string]::IsNullOrEmpty($env:NO_COLOR)
$krillAnim = $krillUi -and ($env:KRILL_INSTALL_ANIMATE -ne '0')

$esc = [string][char]27
$cTrenchBg = if ($krillColor) { "$esc[48;2;6;22;32m" } else { '' }
$cCoral    = if ($krillColor) { "$esc[1;38;2;255;82;56m" } else { '' }
$cFg       = if ($krillColor) { "$esc[38;2;223;237;242m" } else { '' }
$cSuccess  = if ($krillColor) { "$esc[38;2;134;239;172m" } else { '' }
$cWarn     = if ($krillColor) { "$esc[38;2;234;179;8m" } else { '' }
$cFail     = if ($krillColor) { "$esc[38;2;255;101;104m" } else { '' }
$cMuted    = if ($krillColor) { "$esc[38;2;154;163;181m" } else { '' }
$cReset    = if ($krillColor) { "$esc[0m" } else { '' }

$gTop   = Convert-KrillUnicode '\u250C'
$gPipe  = Convert-KrillUnicode '\u2502'
$gStep  = Convert-KrillUnicode '\u25C7'
$gBot   = Convert-KrillUnicode '\u2514'
$gCross = Convert-KrillUnicode '\u00D7'
$gDot   = Convert-KrillUnicode '\u00B7'
$gArrow = Convert-KrillUnicode '\u2192'
$gArcs  = @(
    (Convert-KrillUnicode '\u25DC'),
    (Convert-KrillUnicode '\u25E0'),
    (Convert-KrillUnicode '\u25DD'),
    (Convert-KrillUnicode '\u25DF'),
    (Convert-KrillUnicode '\u25E1'),
    (Convert-KrillUnicode '\u25DE')
)

$kWork  = Convert-KrillUnicode '\uC791\uC5C5'
$kFail  = Convert-KrillUnicode '\uC2E4\uD328'
$kAbort = Convert-KrillUnicode '\uC124\uCE58\uB97C \uC911\uB2E8\uD588\uC2B5\uB2C8\uB2E4'
$kDone  = Convert-KrillUnicode '\uC124\uCE58\uD588\uC2B5\uB2C8\uB2E4'
$kStart = Convert-KrillUnicode '\uC2DC\uC791'

$installTimer = [Diagnostics.Stopwatch]::StartNew()
$stepTimer = [Diagnostics.Stopwatch]::New()
$currentStepId = '01'
$currentStepLabel = ''

function Write-KrillErr([string]$Text) {
    if (-not $krillUi) { return }
    try { [Console]::Error.Write($Text) } catch {}
}

function Write-KrillErrLine([string]$Text) {
    if (-not $krillUi) { return }
    try { [Console]::Error.WriteLine($Text) } catch {}
}

function Format-KrillSec([Diagnostics.Stopwatch]$Sw) {
    return ('{0:0.0}s' -f ($Sw.ElapsedMilliseconds / 1000.0))
}

function Show-KrillIntro([string]$ModeDesc) {
    if (-not $krillUi) { return }
    try { [Console]::OutputEncoding = [Text.Encoding]::UTF8 } catch {}
    Write-KrillErrLine ("{0}{1} {2}  Krill{3}  /  install                         {4}" -f $cTrenchBg, $cCoral, $gTop, $cFg, $cReset)
    Write-KrillErrLine ("{0} {1}  {2}{3}" -f $cMuted, $gPipe, $ModeDesc, $cReset)
    Write-KrillErrLine ("{0} {1}{2}" -f $cMuted, $gPipe, $cReset)
}

function Start-KrillStep([string]$StepId, [string]$Label, [int]$ArcIdx = 0) {
    $script:currentStepId = $StepId
    $script:currentStepLabel = $Label
    $stepTimer.Restart()
    if (-not $krillUi) { return }
    $arc = $gArcs[$ArcIdx % $gArcs.Count]
    if ($krillAnim) {
        Write-KrillErr ("`r$esc[2K {0}{1}  [{2}/06 {3}]{4} {5}{6}{4}" -f $cWarn, $arc, $StepId, $kWork, $cReset, $cFg, $Label)
    } else {
        Write-KrillErrLine (" {0}{1}  [{2}/06 {3}]{4} {5}{6}{4}" -f $cWarn, $arc, $StepId, $kWork, $cReset, $cFg, $Label)
    }
}

function Complete-KrillStep([string]$StepId, [string]$Badge, [string]$DoneLabel) {
    $stepTimer.Stop()
    if (-not $krillUi) { return }
    $elapsed = Format-KrillSec $stepTimer
    $prefix = if ($krillAnim) { "`r$esc[2K" } else { '' }
    Write-KrillErrLine ("{0} {1}{2}  [{3}/06 {4}]{5} {6}{7}{5}  {8}{9}{5}" -f $prefix, $cSuccess, $gStep, $StepId, $Badge, $cReset, $cFg, $DoneLabel, $cMuted, $elapsed)
}

function Fail-KrillStep([string]$Reason) {
    $stepTimer.Stop()
    if (-not $krillUi) { return }
    $prefix = if ($krillAnim) { "`r$esc[2K" } else { '' }
    Write-KrillErrLine ("{0} {1}{2}  [{3}/06 {4}] {5}{6}" -f $prefix, $cFail, $gCross, $script:currentStepId, $kFail, $Reason, $cReset)
    Write-KrillErrLine (" {0}{1}  {2}{3}" -f $cFail, $gBot, $kAbort, $cReset)
}

$download = -not $Zip
if ($download -and -not $PSBoundParameters.ContainsKey('AddToPath')) { $AddToPath = $true }
$modeText = if ($download) { "Windows $gDot x64 $gDot release: $Release" } else { "Windows $gDot x64 $gDot local package" }
Show-KrillIntro $modeText

if (-not $alreadyInstalled) {
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("krill-install-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory $tmp | Out-Null
    try {
        if ($download) {

            Start-KrillStep '01' (Convert-KrillUnicode '\uB9B4\uB9AC\uC2A4 \uD655\uC778 \uC911') 0
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
            Complete-KrillStep '01' (Convert-KrillUnicode '\uD655\uC778') ((Convert-KrillUnicode '\uB9B4\uB9AC\uC2A4 \uB2E4\uC6B4\uB85C\uB4DC \uACBD\uB85C \uD655\uC778 (') + $asset.name + ')')


            Start-KrillStep '02' ((Convert-KrillUnicode '\uBC88\uB4E4 \uB2E4\uC6B4\uB85C\uB4DC \uC911 (') + $asset.name + ')') 1
            $Zip = Join-Path $tmp $asset.name
            $checksumFile = Join-Path $tmp 'package.sha256'
            $savedProgress = $ProgressPreference
            try {
                if ($krillUi) { $ProgressPreference = 'SilentlyContinue' }
                foreach ($item in @(@{ Asset = $asset; File = $Zip }, @{ Asset = $checksums[0]; File = $checksumFile })) {
                    $url = [uri]$item.Asset.browser_download_url
                    if ($url.Scheme -ne 'https' -or $url.Host -ne 'github.com' -or
                        -not $url.AbsolutePath.StartsWith('/craftowen/krill/releases/download/')) {
                        throw 'Unexpected release asset URL.'
                    }
                    Invoke-WebRequest -UseBasicParsing -Uri $url.AbsoluteUri -OutFile $item.File
                }
            } finally {
                $ProgressPreference = $savedProgress
            }
            $zipBytes = if (Test-Path -LiteralPath $Zip) { (Get-Item -LiteralPath $Zip).Length } else { 0 }
            $zipMib = ('{0:0.0} MiB' -f ($zipBytes / 1048576.0))
            Complete-KrillStep '02' (Convert-KrillUnicode '\uC218\uC2E0') ((Convert-KrillUnicode '\uBC88\uB4E4 \uB2E4\uC6B4\uB85C\uB4DC \uC644\uB8CC (') + $zipMib + ')')


            Start-KrillStep '03' (Convert-KrillUnicode 'SHA-256 \uBE44\uAD50 \uC911') 2
            $checksum = (Get-Content -LiteralPath $checksumFile -Raw).Trim()
            if ($checksum -notmatch '^([0-9a-fA-F]{64})(?:\s+\*?([^\r\n]+))?$') { throw 'Invalid SHA-256 file.' }
            $expected = $Matches[1]
            if ($Matches[2] -and $Matches[2] -cne $asset.name) { throw 'SHA-256 filename does not match the package.' }
            if ((Get-FileHash -LiteralPath $Zip -Algorithm SHA256).Hash -ne $expected) { throw 'Windows package SHA-256 mismatch; nothing was installed.' }
            if (-not $PSBoundParameters.ContainsKey('AddToPath')) { $AddToPath = $true }
            Complete-KrillStep '03' (Convert-KrillUnicode '\uC77C\uCE58') (Convert-KrillUnicode 'SHA-256 \uCCB4\uD06C\uC12C \uC77C\uCE58')
        } else {
            $leafName = Split-Path -Leaf $Zip
            Start-KrillStep '01' (Convert-KrillUnicode '\uB85C\uCEEC \uD328\uD0A4\uC9C0 \uD655\uC778 \uC911') 0
            Complete-KrillStep '01' (Convert-KrillUnicode '\uD655\uC778') ((Convert-KrillUnicode '\uB85C\uCEEC \uD328\uD0A4\uC9C0 \uC9C0\uC815 (') + $leafName + ')')
            Start-KrillStep '02' (Convert-KrillUnicode '\uB85C\uCEEC \uBC88\uB4E4 \uC900\uBE44 \uC911') 1
            Complete-KrillStep '02' (Convert-KrillUnicode '\uC218\uC2E0') (Convert-KrillUnicode '\uB85C\uCEEC \uBC88\uB4E4 \uC0AC\uC6A9 (\uB2E4\uC6B4\uB85C\uB4DC \uC0DD\uB7B5)')
            Start-KrillStep '03' (Convert-KrillUnicode '\uC624\uD504\uB77C\uC778 \uBAA8\uB4DC \uD655\uC778 \uC911') 2
            Complete-KrillStep '03' (Convert-KrillUnicode '\uC0DD\uB7B5') (Convert-KrillUnicode '\uC624\uD504\uB77C\uC778 \uD328\uD0A4\uC9C0 (\uC678\uBD80 \uCCB4\uD06C\uC12C \uC0DD\uB7B5)')
        }


        Start-KrillStep '04' (Convert-KrillUnicode '\uBC88\uB4E4 \uD574\uC81C\u00B7\uAD6C\uC131 \uD30C\uC77C \uAC80\uC0AC \uC911') 3
        $unpacked = Join-Path $tmp 'unpacked'
        $savedProgress2 = $ProgressPreference
        try {
            if ($krillUi) { $ProgressPreference = 'SilentlyContinue' }
            Expand-Archive -LiteralPath $Zip -DestinationPath $unpacked
        } finally {
            $ProgressPreference = $savedProgress2
        }
        foreach ($file in $required) {
            if (-not (Test-Path -LiteralPath (Join-Path $unpacked "Krill\$file") -PathType Leaf)) {
                throw "Not a Krill package (missing $file)."
            }
        }
        Complete-KrillStep '04' (Convert-KrillUnicode '\uAC80\uC99D') (Convert-KrillUnicode '\uBC88\uB4E4 \uD574\uC81C \u00B7 \uD544\uC218 \uAD6C\uC131 \uD30C\uC77C 5\uC885 \uD655\uC778')


        Start-KrillStep '05' ((Convert-KrillUnicode '\uC571 \uD30C\uC77C \uC124\uCE58 \uC911 (') + $Dest + ')') 4
        New-Item -ItemType Directory -Force (Split-Path -Parent $Dest) | Out-Null
        Move-Item -LiteralPath (Join-Path $unpacked 'Krill') -Destination $Dest
        Complete-KrillStep '05' (Convert-KrillUnicode '\uC124\uCE58') ($Dest + (Convert-KrillUnicode ' \uC124\uCE58 \uC644\uB8CC'))
    } catch {
        Fail-KrillStep $_.Exception.Message
        throw
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}


Start-KrillStep '06' (Convert-KrillUnicode 'CLI PATH \u00B7 \uBC14\uB85C\uAC00\uAE30 \u00B7 \uC571 \uC5F4\uAE30 \uAD6C\uC131 \uC911') 5
try {
    if (-not $NoShortcut) {
        $lnk = Join-Path ([Environment]::GetFolderPath('Programs')) 'Krill.lnk'
        $s = (New-Object -ComObject WScript.Shell).CreateShortcut($lnk)
        $s.TargetPath = Join-Path $Dest 'krill-app.exe'

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
    $openNote = if ($download -and -not $NoOpen) {
        Convert-KrillUnicode '\uC571 \uC5F4\uAE30 \uC694\uCCAD \uC804\uB2EC'
    } else {
        Convert-KrillUnicode '\uC571 \uC5F4\uAE30 \uC0DD\uB7B5'
    }
    Complete-KrillStep '06' (Convert-KrillUnicode '\uC644\uB8CC') ((Convert-KrillUnicode 'CLI \uAD6C\uC131 \u00B7 ') + $openNote)
} catch {
    Fail-KrillStep $_.Exception.Message
    throw
}

$installTimer.Stop()
if ($krillUi) {
    $totalElapsed = Format-KrillSec $installTimer
    $cliPath = Join-Path $Dest 'krill.exe'
    $appPath = Join-Path $Dest 'krill-app.exe'
    Write-KrillErrLine ("{0} {1}{2}" -f $cMuted, $gPipe, $cReset)
    Write-KrillErrLine (" {0}{1}  {2}{3}  {4}/  {5}{3}" -f $cSuccess, $gBot, $kDone, $cReset, $cMuted, $totalElapsed)
    Write-KrillErrLine ("    {0}Krill          {1} {2}{3}" -f $cFg, $gArrow, $Dest, $cReset)
    Write-KrillErrLine ("    {0}CLI            {1} {2}{3}" -f $cMuted, $gArrow, $cliPath, $cReset)
    Write-KrillErrLine ("    {0}{1}           {2} {3}{4}" -f $cMuted, $kStart, $gArrow, $appPath, $cReset)
}

if ($alreadyInstalled) { "already installed: $Dest (files preserved; use the app to check for updates)" }
else { "installed: $Dest" }
if ($download -and -not $NoOpen) { Start-Process -FilePath (Join-Path $Dest 'krill-app.exe') }
