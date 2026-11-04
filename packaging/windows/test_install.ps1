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
    New-Item -ItemType Directory -Path (Join-Path $root 'source/Krill/x64') -Force | Out-Null
    foreach ($name in @('Krill.json', 'krill-app.exe', 'krill.exe', 'krilld.exe', 'krill-holder.exe', 'conpty.dll', 'x64/OpenConsole.exe')) {
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

    # Dot-source the ASCII-only installer just as irm | iex does, then create
    # both the shortcut and its target under a Unicode path with spaces.
    $unicode = [regex]::Unescape('\uD64D \uAE38\uB3D9')
    $unicodeDest = Join-Path (Join-Path $root $unicode) 'Programs/Krill'
    . $installer -Zip $state.Fixture -Dest $unicodeDest -NoShortcut -NoOpen | Out-Null
    $link = Join-Path (Join-Path $root $unicode) 'Krill link.lnk'
    $target = Join-Path $unicodeDest 'krill-app.exe'
    New-KrillShortcut $link $target (Join-Path $root $unicode)
    # Read via IShellLinkW as well: WScript's reader uses the ANSI interface.
    if (-not ('KrillTest.LinkReader' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
namespace KrillTest {
    [ComImport, Guid("00021401-0000-0000-C000-000000000046")]
    class LinkObject { }
    [ComImport, InterfaceType(ComInterfaceType.InterfaceIsIUnknown), Guid("000214F9-0000-0000-C000-000000000046")]
    interface ILink {
        void GetPath([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder path, int count, IntPtr data, uint flags);
        void GetIDList(out IntPtr id);
        void SetIDList(IntPtr id);
        void GetDescription(IntPtr description, int count);
        void SetDescription(IntPtr description);
        void GetWorkingDirectory([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder path, int count);
    }
    public static class LinkReader {
        public static string[] Read(string file) {
            object obj = new LinkObject();
            try {
                ((IPersistFile)obj).Load(file, 0);
                var link = (ILink)obj;
                var target = new StringBuilder(32768);
                var working = new StringBuilder(32768);
                link.GetPath(target, target.Capacity, IntPtr.Zero, 4);
                link.GetWorkingDirectory(working, working.Capacity);
                return new string[] {target.ToString(), working.ToString()};
            } finally { Marshal.FinalReleaseComObject(obj); }
        }
    }
}
'@
    }
    $readLink = [KrillTest.LinkReader]::Read($link)
    Assert ($readLink[0] -eq $target) 'Unicode shortcut target did not round-trip'
    Assert ($readLink[1] -eq (Join-Path $root $unicode)) 'Unicode shortcut working directory did not round-trip'
    New-KrillShortcut $link $target (Join-Path $root $unicode)
    Assert (Test-Path -LiteralPath $link) 'Unicode shortcut repair failed'

    # Run the complete installer with an isolated Start Menu, including retry.
    $script:testPrograms = Join-Path $root $unicode
    $source = [IO.File]::ReadAllText($installer)
    $source = $source.Replace("[Environment]::GetFolderPath('Programs')", '$script:testPrograms')
    $isolatedInstaller = [scriptblock]::Create($source)
    & $isolatedInstaller -Dest $unicodeDest -NoOpen | Out-Null
    $menuLink = Join-Path $script:testPrograms 'Krill.lnk'
    Assert ([KrillTest.LinkReader]::Read($menuLink)[0] -eq $target) 'Installer shortcut target is wrong'
    & $isolatedInstaller -Dest $unicodeDest -NoOpen | Out-Null
    Assert ([KrillTest.LinkReader]::Read($menuLink)[0] -eq $target) 'Installer shortcut retry failed'

    # Make creation fail without touching the real Start Menu or user PATH.
    $script:testPrograms = Join-Path $root 'missing-programs'
    $state.PathWritten = $false
    $script:opened = $false
    $savedPath = $env:Path
    function Start-Process { param($FilePath) $script:opened = ($FilePath -eq $target) }
    $source = $source.Replace("[Environment]::GetEnvironmentVariable('Path', 'User')", '$null')
    $source = $source.Replace("[Environment]::SetEnvironmentVariable('Path', ((@(`$path, `$Dest) | Where-Object { `$_ }) -join ';'), 'User')", '$state.PathWritten = $true')
    try {
        $warning = @(& ([scriptblock]::Create($source)) -Dest $unicodeDest -AddToPath 3>&1)
        Assert (@($warning | Where-Object { $_ -is [System.Management.Automation.WarningRecord] }).Count -gt 0) 'Shortcut failure did not warn'
        Assert $state.PathWritten 'Shortcut failure prevented user PATH registration'
        Assert (($env:Path -split ';') -contains $unicodeDest) 'Shortcut failure prevented process PATH registration'
        Assert $script:opened 'Shortcut failure prevented app launch'
    } finally {
        $env:Path = $savedPath
        Remove-Item Function:Start-Process
    }

    # Exercise the documented irm | iex shape with a Unicode LOCALAPPDATA.
    $savedLocalAppData = $env:LOCALAPPDATA
    $savedPath = $env:Path
    $env:LOCALAPPDATA = Join-Path $root $unicode
    $script:testPrograms = $env:LOCALAPPDATA
    $state.Launched = ''
    function Start-Process { param($FilePath) $state.Launched = $FilePath }
    try {
        Assert (-not $source.Contains("[Environment]::SetEnvironmentVariable('Path'")) 'User PATH mock was not applied'
        & { $source | Invoke-Expression } | Out-Null
        $iexTarget = Join-Path $env:LOCALAPPDATA 'Programs/Krill/krill-app.exe'
        Assert (Test-Path -LiteralPath $iexTarget) 'irm | iex did not install to Unicode LOCALAPPDATA'
        Assert ([KrillTest.LinkReader]::Read($menuLink)[0] -eq $iexTarget) 'irm | iex shortcut target is wrong'
        Assert ($state.Launched -eq $iexTarget) 'irm | iex did not launch the app'
    } finally {
        $env:LOCALAPPDATA = $savedLocalAppData
        $env:Path = $savedPath
        Remove-Item Function:Start-Process
    }

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

    foreach ($backend in @('conpty.dll', 'x64\OpenConsole.exe')) {
        $source = Join-Path $root "source/Krill/$backend"
        $bytes = [IO.File]::ReadAllBytes($source)
        Remove-Item -LiteralPath $source
        $invalid = Join-Path $root ('missing-backend-' + [guid]::NewGuid().ToString('N') + '.zip')
        Compress-Archive -Path (Join-Path $root 'source/Krill') -DestinationPath $invalid
        Reject ('missing-backend-' + [guid]::NewGuid().ToString('N')) ('missing ' + [regex]::Escape($backend)) @{ Zip = $invalid }
        [IO.File]::WriteAllBytes($source, $bytes)
    }

    Remove-Item -LiteralPath (Join-Path $root 'source/Krill/krilld.exe')
    $incomplete = Join-Path $root 'incomplete.zip'
    Compress-Archive -Path (Join-Path $root 'source/Krill') -DestinationPath $incomplete
    Reject 'incomplete' 'missing krilld.exe' @{ Zip = $incomplete }
    'Windows installer: download, offline install, collision preservation and rejection cases passed.'
} finally {
    foreach ($key in $savedEnv.Keys) { [Environment]::SetEnvironmentVariable($key, $savedEnv[$key], 'Process') }
    Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
}
