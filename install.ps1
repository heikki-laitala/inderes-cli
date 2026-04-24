# One-liner installer for inderes-cli release binaries on Windows.
#
# Usage (PowerShell):
#   iwr -useb https://raw.githubusercontent.com/heikki-laitala/inderes-cli/main/install.ps1 | iex
#
# Env overrides:
#   $env:INDERES_VERSION      install a specific tag (default: latest)
#   $env:INDERES_INSTALL_DIR  install directory
#                             (default: %LOCALAPPDATA%\Programs\inderes\bin)
#   $env:INDERES_REPO         release source (default: heikki-laitala/inderes-cli)
#   $env:GH_TOKEN             forwarded as `Authorization: Bearer` on GitHub
#                             requests — needed when the repo is private.

$ErrorActionPreference = "Stop"

function Log($msg)  { Write-Host "==> $msg" -ForegroundColor Cyan }
function Warn($msg) { Write-Host "!!  $msg" -ForegroundColor Yellow }
function Die($msg)  { Write-Host "!!  $msg" -ForegroundColor Red; exit 1 }

$repo        = if ($env:INDERES_REPO)        { $env:INDERES_REPO }        else { "heikki-laitala/inderes-cli" }
$version     = if ($env:INDERES_VERSION)     { $env:INDERES_VERSION }     else { "latest" }
$installDir  = if ($env:INDERES_INSTALL_DIR) { $env:INDERES_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "Programs\inderes\bin" }

$arch = $env:PROCESSOR_ARCHITECTURE
switch ($arch) {
    "AMD64" { $target = "x86_64-pc-windows-msvc" }
    default { Die "unsupported architecture: $arch (only AMD64 is published today)" }
}

$headers = @{ "User-Agent" = "inderes-install.ps1" }
if ($env:GH_TOKEN) { $headers["Authorization"] = "Bearer $($env:GH_TOKEN)" }

if ($version -eq "latest") {
    Log "Resolving latest release for $repo"
    try {
        $rel = Invoke-RestMethod -Headers $headers -Uri "https://api.github.com/repos/$repo/releases/latest"
    } catch {
        Die "could not query latest release: $($_.Exception.Message)"
    }
    $version = $rel.tag_name
    if (-not $version) { Die "could not determine latest tag" }
}

$archive  = "inderes-$target.zip"
$url      = "https://github.com/$repo/releases/download/$version/$archive"
$sumUrl   = "$url.sha256"

$tmp = New-Item -ItemType Directory -Force -Path (Join-Path $env:TEMP ("inderes-install-" + [System.Guid]::NewGuid().ToString("N")))
try {
    Log "Downloading $archive ($version)"
    try {
        Invoke-WebRequest -Headers $headers -Uri $url -OutFile (Join-Path $tmp.FullName $archive) -UseBasicParsing
    } catch {
        Die "download failed: $url — $($_.Exception.Message)"
    }

    Log "Verifying checksum"
    try {
        Invoke-WebRequest -Headers $headers -Uri $sumUrl -OutFile (Join-Path $tmp.FullName "$archive.sha256") -UseBasicParsing
        $expected = ((Get-Content (Join-Path $tmp.FullName "$archive.sha256") -Raw).Trim() -split '\s+')[0].ToLower()
        $actual   = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp.FullName $archive)).Hash.ToLower()
        if ($expected -ne $actual) {
            Die "checksum mismatch ($expected vs $actual) — refusing to install"
        }
    } catch {
        Warn "no .sha256 file found at $sumUrl — skipping verification"
    }

    Log "Extracting"
    Expand-Archive -Path (Join-Path $tmp.FullName $archive) -DestinationPath $tmp.FullName -Force

    $srcDir = Join-Path $tmp.FullName "inderes-$target"
    $srcBin = Join-Path $srcDir "inderes.exe"
    if (-not (Test-Path $srcBin)) {
        Die "archive layout unexpected; binary not at $srcBin"
    }

    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    Copy-Item -Force $srcBin (Join-Path $installDir "inderes.exe")

    Log "Installed inderes $version -> $installDir\inderes.exe"

    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if (-not ($userPath -split ';' | Where-Object { $_ -ieq $installDir })) {
        Warn "$installDir is not on your User PATH."
        Warn "Add it persistently:"
        Warn "  [Environment]::SetEnvironmentVariable('Path', `"$installDir;`" + [Environment]::GetEnvironmentVariable('Path', 'User'), 'User')"
        Warn "Then restart your shell, or set `$env:Path = `"$installDir;`$env:Path`" in this session."
    }

    Log "Next steps:"
    Log "  inderes login           # sign in via your Inderes account"
    Log "  inderes install-skill openclaw   # drop SKILL.md into ~\.openclaw\skills\"
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
