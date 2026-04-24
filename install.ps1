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
#   $env:GH_TOKEN             optional — when set, forwarded as
#                             `Authorization: Bearer` so GitHub's API calls
#                             use the 5000/hr authenticated rate limit
#                             (vs 60/hr anonymous) and private-repo mirrors
#                             work.

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

# Choose asset URLs. On private repos the user-facing github.com/.../download/
# URL 404s even with a Bearer token — the only reliable authenticated path is
# via the API with the asset's numeric id. Public repos work either way; we
# switch on whether GH_TOKEN is set.
$archiveHeaders = @{ "User-Agent" = "inderes-install.ps1" }
if ($env:GH_TOKEN) {
    $archiveHeaders["Authorization"] = "Bearer $($env:GH_TOKEN)"

    Log "Resolving asset IDs for $version"
    try {
        $rel = Invoke-RestMethod -Headers $headers -Uri "https://api.github.com/repos/$repo/releases/tags/$version"
    } catch {
        Die "could not read release metadata for ${version}: $($_.Exception.Message)"
    }
    $archiveAsset = $rel.assets | Where-Object { $_.name -eq $archive } | Select-Object -First 1
    $sumsAsset    = $rel.assets | Where-Object { $_.name -eq "SHA256SUMS" } | Select-Object -First 1
    if (-not $archiveAsset) { Die "asset $archive not found in release $version" }
    $url    = "https://api.github.com/repos/$repo/releases/assets/$($archiveAsset.id)"
    $sumUrl = if ($sumsAsset) { "https://api.github.com/repos/$repo/releases/assets/$($sumsAsset.id)" } else { $null }
    $archiveHeaders["Accept"] = "application/octet-stream"
} else {
    $url    = "https://github.com/$repo/releases/download/$version/$archive"
    $sumUrl = "https://github.com/$repo/releases/download/$version/SHA256SUMS"
}

$tmp = New-Item -ItemType Directory -Force -Path (Join-Path $env:TEMP ("inderes-install-" + [System.Guid]::NewGuid().ToString("N")))
try {
    Log "Downloading $archive ($version)"
    try {
        Invoke-WebRequest -Headers $archiveHeaders -Uri $url -OutFile (Join-Path $tmp.FullName $archive) -UseBasicParsing
    } catch {
        Die "download failed: $url — $($_.Exception.Message)"
    }

    Log "Verifying checksum"
    $verified = $false
    if ($sumUrl) {
        try {
            Invoke-WebRequest -Headers $archiveHeaders -Uri $sumUrl -OutFile (Join-Path $tmp.FullName "SHA256SUMS") -UseBasicParsing
            $sumLine = Select-String -Path (Join-Path $tmp.FullName "SHA256SUMS") -Pattern "\s$([Regex]::Escape($archive))$" | Select-Object -First 1
            if ($sumLine) {
                $expected = ($sumLine.Line.Trim() -split '\s+')[0].ToLower()
                $actual   = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp.FullName $archive)).Hash.ToLower()
                if ($expected -ne $actual) {
                    Die "checksum mismatch ($expected vs $actual) — refusing to install"
                }
                $verified = $true
            }
        } catch {
            # fall through to warning
        }
    }
    if (-not $verified) {
        Warn "SHA256SUMS not available or $archive not listed — skipping verification"
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
