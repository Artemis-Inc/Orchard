# Orchard installer for Windows.
#
#   irm https://raw.githubusercontent.com/Artemis-Inc/Orchard/main/scripts/install.ps1 | iex
#
# Downloads the `orch` CLI from GitHub Releases and installs it to
# %LOCALAPPDATA%\Orchard\bin, then adds that folder to your user PATH.
# Override the version with:  $env:ORCHARD_VERSION = "3.1.0"

$ErrorActionPreference = "Stop"
$Repo = "Artemis-Inc/Orchard"
$InstallDir = if ($env:ORCHARD_INSTALL_DIR) { $env:ORCHARD_INSTALL_DIR } else { "$env:LOCALAPPDATA\Orchard\bin" }

function Say($m) { Write-Host "orchard: $m" -ForegroundColor Green }
function Die($m) { Write-Host "orchard: $m" -ForegroundColor Red; exit 1 }

# Only 64-bit Windows builds are published today.
$arch = (Get-CimInstance Win32_Processor).Architecture
$target = "x86_64-pc-windows-msvc"

# Resolve version.
$version = $env:ORCHARD_VERSION
if (-not $version) {
  Say "resolving latest release"
  $rel = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest"
  $version = $rel.tag_name -replace '^v', ''
}
if (-not $version) { Die "could not determine the latest release" }

$asset = "orch-$version-$target.zip"
$url = "https://github.com/$Repo/releases/download/v$version/$asset"

$tmp = New-Item -ItemType Directory -Path (Join-Path $env:TEMP ("orchard-" + [guid]::NewGuid()))
try {
  Say "downloading $asset"
  $zip = Join-Path $tmp $asset
  Invoke-WebRequest -Uri $url -OutFile $zip

  # Verify checksum if available.
  try {
    $sums = Invoke-WebRequest "https://github.com/$Repo/releases/download/v$version/SHA256SUMS" -UseBasicParsing
    $line = ($sums.Content -split "`n") | Where-Object { $_ -match [regex]::Escape($asset) } | Select-Object -First 1
    if ($line) {
      $want = ($line -split '\s+')[0]
      $got = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
      if ($want.ToLower() -ne $got) { Die "checksum mismatch for $asset" }
      Say "checksum verified"
    }
  } catch {}

  Say "installing to $InstallDir"
  New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
  Expand-Archive -Path $zip -DestinationPath $tmp -Force
  Copy-Item (Join-Path $tmp "orch.exe") (Join-Path $InstallDir "orch.exe") -Force

  # Add to user PATH if missing.
  $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
  if ($userPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$InstallDir", "User")
    Say "added $InstallDir to your PATH (restart your terminal)"
  }

  Say "installed orch $version"
  & (Join-Path $InstallDir "orch.exe") --version
  Say "done. run 'orch --help' to get started."
}
finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
