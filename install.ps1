<#
.SYNOPSIS
    poly installer for Windows — downloads the correct prebuilt binaries.

.DESCRIPTION
    Installs poly. Re-run any time to UPDATE to the latest release (it overwrites in place).

        irm https://raw.githubusercontent.com/Goldziher/poly/main/install.ps1 | iex

    Pin a version or change the install dir with environment variables before running:
        $env:POLY_VERSION = "v0.1.5"
        $env:POLY_INSTALL_DIR = "C:\tools\poly"
        $env:POLY_NO_MODIFY_PATH = "1"
#>

$ErrorActionPreference = "Stop"

$Repo = "Goldziher/poly"
$Binaries = @("poly.exe")

$Version = if ($env:POLY_VERSION) { $env:POLY_VERSION } else { "latest" }
$InstallDir = if ($env:POLY_INSTALL_DIR) { $env:POLY_INSTALL_DIR } else { "$env:LOCALAPPDATA\poly\bin" }

function Info($msg) { Write-Host "✓ $msg" -ForegroundColor Green }
function Warn($msg) { Write-Host "⚠ $msg" -ForegroundColor Yellow }
function Die($msg) { Write-Host "✗ $msg" -ForegroundColor Red; exit 1 }

$arch = switch ($env:PROCESSOR_ARCHITECTURE) {
    "AMD64" { "x86_64" }
    "ARM64" { "aarch64" }
    default { Die "Unsupported architecture: $env:PROCESSOR_ARCHITECTURE" }
}
$target = "$arch-pc-windows-msvc"
$ext = "zip"
Info "Platform: $target"

if ($Version -eq "latest") {
    Info "Resolving latest release..."
    $resp = Invoke-WebRequest -Uri "https://github.com/$Repo/releases/latest" `
        -MaximumRedirection 0 -ErrorAction SilentlyContinue
    $loc = $resp.Headers.Location
    if ($loc -and $loc -match "/tag/(?<tag>[^/]+)$") {
        $Version = $Matches.tag
    } else {
        $api = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest"
        $Version = $api.tag_name
    }
    if (-not $Version) { Die "Could not resolve the latest release" }
}

$tag = if ($Version.StartsWith("v")) { $Version } else { "v$Version" }
$ver = $tag.TrimStart("v")
$asset = "poly-$ver-$target.$ext"
$base = "https://github.com/$Repo/releases/download/$tag"

$existing = Join-Path $InstallDir "poly.exe"
if (Test-Path $existing) {
    $prev = (& $existing --version 2>$null) -replace '.*\s', ''
    if ($prev -eq $ver) { Info "poly $ver already installed — re-installing." }
    elseif ($prev) { Info "Updating poly $prev -> $ver" }
} else {
    Info "Installing poly $ver"
}

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("poly-" + [System.Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp -Force | Out-Null
try {
    $archive = Join-Path $tmp $asset
    Info "Downloading $base/$asset"
    Invoke-WebRequest -Uri "$base/$asset" -OutFile $archive

    $sumsPath = Join-Path $tmp "sha256sums.txt"
    Invoke-WebRequest -Uri "$base/sha256sums.txt" -OutFile $sumsPath
    $expected = $null
    foreach ($line in Get-Content $sumsPath) {
        $parts = $line -split '\s+'
        if ($parts.Count -ge 2) {
            $name = $parts[-1] -replace '^\*', '' -replace '^\./', ''
            if ($name -eq $asset) { $expected = $parts[0].ToLower() }
        }
    }
    if (-not $expected) { Die "No checksum entry for $asset — refusing to install unverified binaries" }
    $actual = (Get-FileHash -Algorithm SHA256 -Path $archive).Hash.ToLower()
    if ($actual -ne $expected) { Die "Checksum mismatch for $asset (expected $expected, got $actual)" }
    Info "Checksum verified"

    Expand-Archive -Path $archive -DestinationPath $tmp -Force
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    foreach ($binary in $Binaries) {
        $src = Join-Path $tmp $binary
        if (-not (Test-Path $src)) { Die "Expected binary $binary missing from $asset" }
        Copy-Item -Path $src -Destination (Join-Path $InstallDir $binary) -Force
    }
    Info "Installed poly -> $InstallDir"
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (($userPath -split ';') -notcontains $InstallDir) {
    if ($env:POLY_NO_MODIFY_PATH) {
        Warn "$InstallDir is not on your PATH. Add it via System > Environment Variables."
    } else {
        [Environment]::SetEnvironmentVariable("Path", "$userPath;$InstallDir", "User")
        $env:Path = "$env:Path;$InstallDir"
        Info "Added $InstallDir to your user PATH (restart your terminal to pick it up)."
    }
}

Write-Host "poly $ver is ready. Run 'poly --help' to get started." -ForegroundColor Green
