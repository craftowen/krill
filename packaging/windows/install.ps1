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

# 7-bit safe \uXXXX unescaping so Windows PowerShell 5.1 without UTF-8 BOM
# never corrupts box-drawing, arc spinner, or Korean status strings.
function Convert-KrillUnicode([string]$Text) {
    return [regex]::Unescape($Text)
}

$krillLang = if ($null -ne $env:KRILL_LANG) { $env:KRILL_LANG.Trim() } else { '' }
if ($krillLang -ne '') {
    $uiKo = ($krillLang -like 'ko*')
} else {
    $uiKo = $false
    try {
        $uiKo = ([Globalization.CultureInfo]::CurrentUICulture.TwoLetterISOLanguageName -eq 'ko')
    } catch {
        $uiKo = $false
    }
}

function Get-KrillMsg([string]$Ko, [string]$En) {
    if ($uiKo) { return (Convert-KrillUnicode $Ko) }
    return $En
}

$krillSiteUrl = if ($uiKo) { 'https://krill.page' } else { 'https://krill.page/en' }

if ($env:OS -ne 'Windows_NT') {
    throw (Get-KrillMsg '\uC774 \uC124\uCE58\uAE30\uB294 Windows x64 \uB610\uB294 Windows 11 ARM64\uAC00 \uD544\uC694\uD569\uB2C8\uB2E4.' 'This installer requires Windows x64 or Windows 11 on ARM64.')
}
$arch = $env:PROCESSOR_ARCHITECTURE
if ($env:PROCESSOR_ARCHITEW6432) { $arch = $env:PROCESSOR_ARCHITEW6432 }
# ARM64 PCs run the x64 package through Windows 11's built-in x64 emulation (build 22000+).
# Windows 10 on ARM only emulates 32-bit x86, so the x64 binaries cannot start there.
if ($arch -eq 'ARM64') {
    if ([Environment]::OSVersion.Version.Build -lt 22000) {
        throw (Get-KrillMsg 'ARM64 PC\uC5D0\uC11C Krill\uC744 \uC2E4\uD589\uD558\uB824\uBA74 Windows 11(x64 \uC5D0\uBBAC\uB808\uC774\uC158)\uC774 \uD544\uC694\uD569\uB2C8\uB2E4. Windows 10 on ARM\uC740 \uC9C0\uC6D0\uD558\uC9C0 \uC54A\uC2B5\uB2C8\uB2E4.' 'Krill on ARM64 PCs requires Windows 11 (x64 emulation). Windows 10 on ARM is not supported.')
    }
} elseif ($arch -ne 'AMD64') {
    throw (Get-KrillMsg 'Windows \uB9B4\uB9AC\uC2A4 \uD328\uD0A4\uC9C0\uB294 x64 PC \uB610\uB294 Windows 11 ARM64\uAC00 \uD544\uC694\uD569\uB2C8\uB2E4.' 'Windows release packages require an x64 PC or Windows 11 on ARM64.')
}
$Dest = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Dest)
$required = @('Krill.json', 'krill-app.exe', 'krill.exe', 'krilld.exe', 'krill-holder.exe', 'conpty.dll', 'x64\OpenConsole.exe')
$alreadyInstalled = Test-Path -LiteralPath $Dest
if ($alreadyInstalled) {
    foreach ($file in $required) {
        if (-not (Test-Path -LiteralPath (Join-Path $Dest $file) -PathType Leaf)) {
            throw ((Get-KrillMsg '{0} \uACBD\uB85C\uAC00 \uC774\uBBF8 \uC788\uC9C0\uB9CC \uC644\uC804\uD55C Krill \uC124\uCE58\uAC00 \uC544\uB2D9\uB2C8\uB2E4({1} \uB204\uB77D). \uAE30\uC874 \uD30C\uC77C\uC740 \uBCF4\uC874\uD588\uC2B5\uB2C8\uB2E4. \uB2E4\uB978 \uC124\uCE58 \uACBD\uB85C\uB97C \uC120\uD0DD\uD558\uAC70\uB098 \uAE30\uC874 \uD3F4\uB354\uB97C \uC62E\uAE34 \uB4A4 \uB2E4\uC2DC \uC2DC\uB3C4\uD558\uC138\uC694.' '{0} already exists but is not a complete Krill installation (missing {1}). Its files were preserved; choose another destination or move it aside before retrying.') -f $Dest, $file)
        }
    }
}

# -----------------------------------------------------------------------------
# Trench Clack Pipeline UI (docs/STYLEGUIDE.md tokens, PS 5.1 & PS 7 compatible)
# -----------------------------------------------------------------------------

# IShellLinkW avoids WScript.Shell's ANSI path conversion on non-UTF-8 systems.
function New-KrillShortcut([string]$Link, [string]$Target, [string]$WorkingDirectory) {
    if (-not ('KrillInstaller.ShellLink' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;

namespace KrillInstaller {
    [ComImport, Guid("00021401-0000-0000-C000-000000000046")]
    class ShellLinkObject { }

    [ComImport, InterfaceType(ComInterfaceType.InterfaceIsIUnknown), Guid("000214F9-0000-0000-C000-000000000046")]
    interface IShellLinkW {
        void GetPath(IntPtr file, int count, IntPtr data, uint flags);
        void GetIDList(out IntPtr idList);
        void SetIDList(IntPtr idList);
        void GetDescription(IntPtr name, int count);
        void SetDescription([MarshalAs(UnmanagedType.LPWStr)] string name);
        void GetWorkingDirectory(IntPtr directory, int count);
        void SetWorkingDirectory([MarshalAs(UnmanagedType.LPWStr)] string directory);
        void GetArguments(IntPtr arguments, int count);
        void SetArguments([MarshalAs(UnmanagedType.LPWStr)] string arguments);
        void GetHotkey(out short hotkey);
        void SetHotkey(short hotkey);
        void GetShowCmd(out int show);
        void SetShowCmd(int show);
        void GetIconLocation(IntPtr path, int count, out int icon);
        void SetIconLocation([MarshalAs(UnmanagedType.LPWStr)] string path, int icon);
        void SetRelativePath([MarshalAs(UnmanagedType.LPWStr)] string path, uint reserved);
        void Resolve(IntPtr window, uint flags);
        void SetPath([MarshalAs(UnmanagedType.LPWStr)] string path);
    }

    public static class ShellLink {
        public static void Create(string link, string target, string workingDirectory) {
            object instance = new ShellLinkObject();
            try {
                IShellLinkW shellLink = (IShellLinkW)instance;
                shellLink.SetPath(target);
                shellLink.SetWorkingDirectory(workingDirectory);
                ((IPersistFile)instance).Save(link, true);
            } finally {
                Marshal.FinalReleaseComObject(instance);
            }
        }
    }
}
'@
    }
    [KrillInstaller.ShellLink]::Create($Link, $Target, $WorkingDirectory)
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

$kWork  = Get-KrillMsg '\uC791\uC5C5' 'working'
$kFail  = Get-KrillMsg '\uC2E4\uD328' 'failed'
$kAbort = Get-KrillMsg '\uC124\uCE58\uB97C \uC911\uB2E8\uD588\uC2B5\uB2C8\uB2E4' 'Installation stopped'
$kDone  = Get-KrillMsg '\uC124\uCE58\uD588\uC2B5\uB2C8\uB2E4' 'Installed'
$kStartLabel = Get-KrillMsg '\uC2DC\uC791           ' 'Start          '

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
$archText = if ($arch -eq 'ARM64') { Get-KrillMsg 'ARM64 (x64 \uC5D0\uBBAC\uB808\uC774\uC158)' 'ARM64 (x64 emulation)' } else { 'x64' }
$modeText = if ($download) {
    (Get-KrillMsg 'Windows {0} {1} {0} \uB9B4\uB9AC\uC2A4: {2}' 'Windows {0} {1} {0} release: {2}') -f $gDot, $archText, $Release
} else {
    (Get-KrillMsg 'Windows {0} {1} {0} \uB85C\uCEEC \uD328\uD0A4\uC9C0' 'Windows {0} {1} {0} local package') -f $gDot, $archText
}
Show-KrillIntro $modeText

if (-not $alreadyInstalled) {
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("krill-install-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        if ($download) {

            Start-KrillStep '01' (Get-KrillMsg '\uB9B4\uB9AC\uC2A4 \uD655\uC778 \uC911' 'Checking the release') 0
            # Windows PowerShell 5.1 may otherwise negotiate an obsolete TLS version.
            [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
            $api = 'https://api.github.com/repos/craftowen/krill/releases/'
            if ($Release -eq 'latest') { $api += 'latest' }
            else { $api += 'tags/' + [uri]::EscapeDataString($Release) }
            $releaseInfo = Invoke-RestMethod -Uri $api -Headers @{ 'User-Agent' = 'Krill-Installer' }
            $packages = @($releaseInfo.assets | Where-Object { $_.name -cmatch '^Krill-[0-9][0-9A-Za-z.+-]*-windows-x64\.zip$' })
            if ($packages.Count -ne 1) {
                throw ((Get-KrillMsg '\uC774 \uB9B4\uB9AC\uC2A4\uC5D0\uB294 \uC544\uC9C1 Windows x64 \uD328\uD0A4\uC9C0\uAC00 \uC5C6\uC2B5\uB2C8\uB2E4. {0}\uC5D0\uC11C \uC9C0\uC6D0 \uC5EC\uBD80\uB97C \uD655\uC778\uD558\uC138\uC694.' 'This release has no Windows x64 package yet. Please check {0} for availability.') -f $krillSiteUrl)
            }
            $asset = $packages[0]
            $checksums = @($releaseInfo.assets | Where-Object { $_.name -ceq ($asset.name + '.sha256') })
            if ($checksums.Count -ne 1) {
                throw (Get-KrillMsg 'Windows \uD328\uD0A4\uC9C0 SHA-256 \uD30C\uC77C\uC774 \uC5C6\uC2B5\uB2C8\uB2E4.' 'The Windows package SHA-256 file is missing.')
            }
            Complete-KrillStep '01' (Get-KrillMsg '\uD655\uC778' 'Checked') ((Get-KrillMsg '\uB9B4\uB9AC\uC2A4 \uB2E4\uC6B4\uB85C\uB4DC \uACBD\uB85C \uD655\uC778 ({0})' 'Release download path found ({0})') -f $asset.name)


            Start-KrillStep '02' ((Get-KrillMsg '\uBC88\uB4E4 \uB2E4\uC6B4\uB85C\uB4DC \uC911 ({0})' 'Downloading the bundle ({0})') -f $asset.name) 1
            $Zip = Join-Path $tmp $asset.name
            $checksumFile = Join-Path $tmp 'package.sha256'
            $savedProgress = $ProgressPreference
            try {
                if ($krillUi) { $ProgressPreference = 'SilentlyContinue' }
                foreach ($item in @(@{ Asset = $asset; File = $Zip }, @{ Asset = $checksums[0]; File = $checksumFile })) {
                    $url = [uri]$item.Asset.browser_download_url
                    if ($url.Scheme -ne 'https' -or $url.Host -ne 'github.com' -or
                        -not $url.AbsolutePath.StartsWith('/craftowen/krill/releases/download/')) {
                        throw (Get-KrillMsg '\uC608\uC0C1\uCE58 \uBABB\uD55C \uB9B4\uB9AC\uC2A4 \uC790\uC0B0 URL\uC785\uB2C8\uB2E4.' 'Unexpected release asset URL.')
                    }
                    Invoke-WebRequest -UseBasicParsing -Uri $url.AbsoluteUri -OutFile $item.File
                }
            } finally {
                $ProgressPreference = $savedProgress
            }
            $zipBytes = if (Test-Path -LiteralPath $Zip) { (Get-Item -LiteralPath $Zip).Length } else { 0 }
            $zipMib = ('{0:0.0} MiB' -f ($zipBytes / 1048576.0))
            Complete-KrillStep '02' (Get-KrillMsg '\uC218\uC2E0' 'Received') ((Get-KrillMsg '\uBC88\uB4E4 \uB2E4\uC6B4\uB85C\uB4DC \uC644\uB8CC ({0})' 'Bundle downloaded ({0})') -f $zipMib)


            Start-KrillStep '03' (Get-KrillMsg 'SHA-256 \uBE44\uAD50 \uC911' 'Comparing SHA-256') 2
            $checksum = (Get-Content -LiteralPath $checksumFile -Raw).Trim()
            if ($checksum -notmatch '^([0-9a-fA-F]{64})(?:\s+\*?([^\r\n]+))?$') {
                throw (Get-KrillMsg '\uC62C\uBC14\uB978 SHA-256 \uD30C\uC77C\uC774 \uC544\uB2D9\uB2C8\uB2E4.' 'Invalid SHA-256 file.')
            }
            $expected = $Matches[1]
            if ($Matches[2] -and $Matches[2] -cne $asset.name) {
                throw (Get-KrillMsg 'SHA-256 \uD30C\uC77C \uC774\uB984\uC774 \uD328\uD0A4\uC9C0\uC640 \uC77C\uCE58\uD558\uC9C0 \uC54A\uC2B5\uB2C8\uB2E4.' 'SHA-256 filename does not match the package.')
            }
            if ((Get-FileHash -LiteralPath $Zip -Algorithm SHA256).Hash -ne $expected) {
                throw (Get-KrillMsg 'Windows \uD328\uD0A4\uC9C0 SHA-256\uC774 \uC77C\uCE58\uD558\uC9C0 \uC54A\uC544 \uC124\uCE58\uD558\uC9C0 \uC54A\uC558\uC2B5\uB2C8\uB2E4.' 'Windows package SHA-256 mismatch; nothing was installed.')
            }
            if (-not $PSBoundParameters.ContainsKey('AddToPath')) { $AddToPath = $true }
            Complete-KrillStep '03' (Get-KrillMsg '\uC77C\uCE58' 'Matched') (Get-KrillMsg 'SHA-256 \uCCB4\uD06C\uC12C \uC77C\uCE58' 'SHA-256 checksum matched')
        } else {
            $leafName = Split-Path -Leaf $Zip
            Start-KrillStep '01' (Get-KrillMsg '\uB85C\uCEEC \uD328\uD0A4\uC9C0 \uD655\uC778 \uC911' 'Checking the local package') 0
            Complete-KrillStep '01' (Get-KrillMsg '\uD655\uC778' 'Checked') ((Get-KrillMsg '\uB85C\uCEEC \uD328\uD0A4\uC9C0 \uC9C0\uC815 ({0})' 'Local package selected ({0})') -f $leafName)
            Start-KrillStep '02' (Get-KrillMsg '\uB85C\uCEEC \uBC88\uB4E4 \uC900\uBE44 \uC911' 'Preparing the local bundle') 1
            Complete-KrillStep '02' (Get-KrillMsg '\uC218\uC2E0' 'Received') (Get-KrillMsg '\uB85C\uCEEC \uBC88\uB4E4 \uC0AC\uC6A9 (\uB2E4\uC6B4\uB85C\uB4DC \uC0DD\uB7B5)' 'Using local bundle (download skipped)')
            Start-KrillStep '03' (Get-KrillMsg '\uC624\uD504\uB77C\uC778 \uBAA8\uB4DC \uD655\uC778 \uC911' 'Checking offline mode') 2
            Complete-KrillStep '03' (Get-KrillMsg '\uC0DD\uB7B5' 'Skipped') (Get-KrillMsg '\uC624\uD504\uB77C\uC778 \uD328\uD0A4\uC9C0 (\uC678\uBD80 \uCCB4\uD06C\uC12C \uC0DD\uB7B5)' 'Offline package (external checksum skipped)')
        }


        Start-KrillStep '04' (Get-KrillMsg '\uBC88\uB4E4 \uD574\uC81C\u00B7\uAD6C\uC131 \uD30C\uC77C \uAC80\uC0AC \uC911' 'Unpacking the bundle and verifying files') 3
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
                throw ((Get-KrillMsg 'Krill \uD328\uD0A4\uC9C0\uAC00 \uC544\uB2D9\uB2C8\uB2E4({0} \uB204\uB77D).' 'Not a Krill package (missing {0}).') -f $file)
            }
        }
        # conpty.dll starts <dll folder>\<native OS architecture>\OpenConsole.exe, so an ARM64 PC needs
        # arm64\OpenConsole.exe even though Krill itself runs as x64; without it sessions fail.
        if ($arch -eq 'ARM64' -and -not (Test-Path -LiteralPath (Join-Path $unpacked 'Krill\arm64\OpenConsole.exe') -PathType Leaf)) {
            throw ((Get-KrillMsg '\uC774 Krill \uB9B4\uB9AC\uC2A4\uC5D0\uB294 ARM64 \uCF58\uC194 \uD638\uC2A4\uD2B8({0})\uAC00 \uC5C6\uC2B5\uB2C8\uB2E4. ARM64 PC\uC5D0\uB294 \uCD5C\uC2E0 \uB9B4\uB9AC\uC2A4\uAC00 \uD544\uC694\uD569\uB2C8\uB2E4.' 'This Krill release has no ARM64 console host ({0}); ARM64 PCs need a newer release.') -f 'arm64\OpenConsole.exe')
        }
        Complete-KrillStep '04' (Get-KrillMsg '\uAC80\uC99D' 'Verified') ((Get-KrillMsg '\uBC88\uB4E4 \uD574\uC81C \u00B7 \uD544\uC218 \uAD6C\uC131 \uD30C\uC77C {0}\uC885 \uD655\uC778' 'Bundle unpacked, verified {0} required files') -f $required.Count)


        Start-KrillStep '05' ((Get-KrillMsg '\uC571 \uD30C\uC77C \uC124\uCE58 \uC911 ({0})' 'Installing app files ({0})') -f $Dest) 4
        $destFull = $Dest
        $destParent = Split-Path -Parent $destFull
        if ($destParent) {
            New-Item -ItemType Directory -Force -Path $destParent | Out-Null
        }
        $srcDir = Join-Path $unpacked 'Krill'
        $srcFull = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($srcDir)
        $srcRoot = [IO.Path]::GetPathRoot($srcFull)
        $destRoot = [IO.Path]::GetPathRoot($destFull)
        if ($srcRoot -and $destRoot -and ($srcRoot -ieq $destRoot)) {
            Move-Item -LiteralPath $srcDir -Destination $Dest
        } else {
            $stageBase = if ($destParent) { $destParent } else { $destRoot }
            $stageDest = Join-Path $stageBase ('.krill-install-stage-' + [guid]::NewGuid().ToString('N'))
            try {
                Copy-Item -LiteralPath $srcDir -Destination $stageDest -Recurse -Force
                Move-Item -LiteralPath $stageDest -Destination $Dest
            } catch {
                Remove-Item -LiteralPath $stageDest -Recurse -Force -ErrorAction SilentlyContinue
                throw
            }
        }
        Complete-KrillStep '05' (Get-KrillMsg '\uC124\uCE58' 'Installed') ((Get-KrillMsg '{0} \uC124\uCE58 \uC644\uB8CC' 'Installed to {0}') -f $Dest)
    } catch {
        Fail-KrillStep $_.Exception.Message
        throw
    } finally {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}


Start-KrillStep '06' (Get-KrillMsg 'CLI PATH \u00B7 \uBC14\uB85C\uAC00\uAE30 \u00B7 \uC571 \uC5F4\uAE30 \uAD6C\uC131 \uC911' 'Configuring CLI PATH, shortcut, and app launch') 5
try {
    if (-not $NoShortcut) {
        try {
            $lnk = Join-Path ([Environment]::GetFolderPath('Programs')) 'Krill.lnk'
            New-KrillShortcut $lnk (Join-Path $Dest 'krill-app.exe') $env:USERPROFILE
        } catch {
            Write-Warning ((Get-KrillMsg '\uC2DC\uC791 \uBA54\uB274 \uBC14\uB85C\uAC00\uAE30\uB97C \uB9CC\uB4E4\uC9C0 \uBABB\uD588\uC2B5\uB2C8\uB2E4: {0}' 'Could not create the Start Menu shortcut: {0}') -f $_.Exception.Message)
        }
    }
    if ($AddToPath) {
        # Read and write the raw registry value: [Environment]::SetEnvironmentVariable would store
        # REG_SZ and expand entries such as %USERPROFILE%\...\WindowsApps for good.
        $envKey = Get-Item 'HKCU:\Environment'
        $path = [string]$envKey.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
        if (($path -split ';') -notcontains $Dest) {
            New-ItemProperty -Path 'HKCU:\Environment' -Name Path -PropertyType ExpandString -Force `
                -Value ((@($path.TrimEnd(';'), $Dest) | Where-Object { $_ }) -join ';') | Out-Null
            # An empty set broadcasts WM_SETTINGCHANGE so new Explorer windows see the new PATH.
            [Environment]::SetEnvironmentVariable('KRILL_PATH_REFRESH', $null, 'User')
        }
        if (($env:Path -split ';') -notcontains $Dest) { $env:Path += ";$Dest" }
    }
    $openNote = if ($download -and -not $NoOpen) {
        Get-KrillMsg '\uC571 \uC5F4\uAE30 \uC694\uCCAD \uC804\uB2EC' 'app launch requested'
    } else {
        Get-KrillMsg '\uC571 \uC5F4\uAE30 \uC0DD\uB7B5' 'app launch skipped'
    }
    Complete-KrillStep '06' (Get-KrillMsg '\uC644\uB8CC' 'Done') ((Get-KrillMsg 'CLI \uAD6C\uC131 \u00B7 {0}' 'CLI configured, {0}') -f $openNote)
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
    Write-KrillErrLine ("    {0}{1}{2} {3}{4}" -f $cMuted, $kStartLabel, $gArrow, $appPath, $cReset)
}

if ($alreadyInstalled) { "already installed: $Dest (files preserved; use the app to check for updates)" }
else { "installed: $Dest" }
if ($download -and -not $NoOpen) { Start-Process -FilePath (Join-Path $Dest 'krill-app.exe') }
elseif ($Dest -eq (Join-Path $env:LOCALAPPDATA 'Programs\Krill')) {
    $cliExe = Join-Path $Dest 'krill.exe'
    if (Test-Path -LiteralPath $cliExe -PathType Leaf) {
        try {
            $stream = [IO.File]::OpenRead($cliExe)
            try {
                $b0 = $stream.ReadByte()
                $b1 = $stream.ReadByte()
            } finally {
                $stream.Dispose()
            }
            if ($b0 -eq 0x4D -and $b1 -eq 0x5A) {
                & $cliExe ls *> $null
            }
        } catch {}
    }
}
