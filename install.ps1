# Cadenza installer for Windows (x86_64).
#
#   irm https://github.com/Pep3M/cadenza/releases/latest/download/install.ps1 | iex
#
# Environment variables:
#   CADENZA_VERSION      release to install, e.g. v0.2.0 (default: latest)
#   CADENZA_INSTALL_DIR  install folder (default: %LOCALAPPDATA%\Programs\Cadenza)
#   CADENZA_UNINSTALL=1  remove what this script installed
#
# Installs for the current user (no administrator rights): the files, a Start menu
# shortcut and the folder on the user PATH. The download is checked against SHA256SUMS.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Repo = 'Pep3M/cadenza'
$Version = if ($env:CADENZA_VERSION) { $env:CADENZA_VERSION } else { 'latest' }
$InstallDir = if ($env:CADENZA_INSTALL_DIR) { $env:CADENZA_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\Cadenza' }
$Shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'Cadenza.lnk'

function Say($msg) { Write-Host "cadenza: $msg" }

function Set-UserPath([string]$dir, [bool]$add) {
	$current = [Environment]::GetEnvironmentVariable('Path', 'User')
	$parts = @(if ($current) { $current -split ';' | Where-Object { $_ -and $_ -ne $dir } })
	if ($add) { $parts += $dir }
	[Environment]::SetEnvironmentVariable('Path', ($parts -join ';'), 'User')
}

function Assert-OwnDir {
	if ((Test-Path $InstallDir) -and (Get-ChildItem $InstallDir -Force | Select-Object -First 1) -and
		-not (Test-Path (Join-Path $InstallDir 'cadenza-app.exe'))) {
		throw "$InstallDir is not empty and holds no Cadenza install; set CADENZA_INSTALL_DIR to another folder"
	}
}

if ($env:CADENZA_UNINSTALL -eq '1') {
	Assert-OwnDir
	if (Test-Path $InstallDir) { Remove-Item -Recurse -Force $InstallDir }
	if (Test-Path $Shortcut) { Remove-Item -Force $Shortcut }
	Set-UserPath $InstallDir $false
	Say 'uninstalled (projects, settings and the style library in your user folders were kept)'
	return
}

if (-not [Environment]::Is64BitOperatingSystem -or $env:PROCESSOR_ARCHITECTURE -eq 'ARM64') {
	throw 'Prebuilt Windows binaries are x86_64 only; build from source (see README).'
}

if ($Version -eq 'latest') {
	$Tag = (Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest" -UseBasicParsing).tag_name
	if (-not $Tag) { throw "Could not find the latest release of $Repo (GitHub API rate limit? set CADENZA_VERSION)." }
} elseif ($Version.StartsWith('v')) {
	$Tag = $Version
} else {
	$Tag = "v$Version"
}

$Name = "cadenza-$($Tag.TrimStart('v'))-windows-x86_64"
$Asset = "$Name.zip"
$Base = "https://github.com/$Repo/releases/download/$Tag"
$Tmp = Join-Path ([IO.Path]::GetTempPath()) ("cadenza-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Force $Tmp | Out-Null

try {
	Say "downloading $Asset ($Tag)"
	Invoke-WebRequest "$Base/$Asset" -OutFile (Join-Path $Tmp $Asset) -UseBasicParsing
	Invoke-WebRequest "$Base/SHA256SUMS" -OutFile (Join-Path $Tmp 'SHA256SUMS') -UseBasicParsing

	$line = Get-Content (Join-Path $Tmp 'SHA256SUMS') | Where-Object { ($_ -split '\s+\*?')[1] -eq $Asset } | Select-Object -First 1
	if (-not $line) { throw "$Asset is not listed in SHA256SUMS" }
	$expected = ($line -split '\s+')[0].ToLower()
	$actual = (Get-FileHash (Join-Path $Tmp $Asset) -Algorithm SHA256).Hash.ToLower()
	if ($actual -ne $expected) { throw "Checksum mismatch for $Asset; aborting." }

	Expand-Archive (Join-Path $Tmp $Asset) -DestinationPath $Tmp -Force
	$src = Join-Path $Tmp $Name
	if (-not (Test-Path (Join-Path $src 'cadenza-app.exe'))) { throw 'Unexpected archive layout.' }

	Assert-OwnDir
	Get-Process cadenza, cadenza-app -ErrorAction SilentlyContinue |
		Where-Object { $_.Path -and $_.Path.StartsWith($InstallDir) } |
		ForEach-Object { throw "Close Cadenza (pid $($_.Id)) before updating it." }
	if (Test-Path $InstallDir) { Remove-Item -Recurse -Force $InstallDir }
	New-Item -ItemType Directory -Force $InstallDir | Out-Null
	Copy-Item (Join-Path $src '*') $InstallDir -Recurse -Force
	Get-ChildItem $InstallDir -Recurse | Unblock-File

	$shell = New-Object -ComObject WScript.Shell
	$lnk = $shell.CreateShortcut($Shortcut)
	$lnk.TargetPath = Join-Path $InstallDir 'cadenza-app.exe'
	$lnk.WorkingDirectory = $InstallDir
	$lnk.Description = 'Cadenza DAW'
	$lnk.Save()

	Set-UserPath $InstallDir $true
	Say "installed $Tag in $InstallDir"
	Say 'Start menu: Cadenza. Open a new terminal to use the `cadenza` command.'
	Say 'The binaries are not code-signed yet: if SmartScreen warns, choose "More info" > "Run anyway".'
} finally {
	Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}
