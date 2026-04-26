# Install Wooting Analog SDK + Wooting RGB SDK on Windows x64.
#
# - Downloads the latest releases from the official WootingKb GitHub repos.
# - Installs the Analog SDK silently via its MSI.
# - Unpacks the RGB SDK ZIP and copies wooting-rgb-sdk.dll into the expected
#   location so it can be loaded by libloading at runtime.
#
# Usage:  powershell -ExecutionPolicy Bypass -File scripts\install-wooting-sdks.ps1

[CmdletBinding()]
param(
    [string] $WorkDir = (Join-Path $env:TEMP "xentool-wooting-install")
)

$ErrorActionPreference = 'Stop'

function Write-Step($msg) { Write-Host "[install] $msg" -ForegroundColor Cyan }
function Write-Ok($msg)   { Write-Host "[ok] $msg" -ForegroundColor Green }
function Write-Warn2($msg) { Write-Host "[warn] $msg" -ForegroundColor Yellow }

function Get-LatestReleaseAsset {
    param([string] $Repo, [string] $NameMatch)
    $api = "https://api.github.com/repos/$Repo/releases/latest"
    $rel = Invoke-RestMethod -Uri $api -UseBasicParsing
    $asset = $rel.assets | Where-Object { $_.name -match $NameMatch } | Select-Object -First 1
    if (-not $asset) { throw "No asset matching /$NameMatch/ found in $Repo latest release" }
    [pscustomobject]@{ Tag = $rel.tag_name; Name = $asset.name; Url = $asset.browser_download_url }
}

function Download-File {
    param([string] $Url, [string] $OutPath)
    Write-Step "Downloading $Url"
    Invoke-WebRequest -Uri $Url -OutFile $OutPath -UseBasicParsing
}

New-Item -ItemType Directory -Path $WorkDir -Force | Out-Null
Write-Step "Work dir: $WorkDir"

# --- 1. Wooting Analog SDK (MSI, silent install) ---

$analogInstalled = Test-Path "C:\Program Files\wooting-analog-sdk\wooting_analog_sdk.dll"
if ($analogInstalled) {
    Write-Ok "Wooting Analog SDK already installed (found wooting_analog_sdk.dll)."
} else {
    $analog = Get-LatestReleaseAsset -Repo 'WootingKb/wooting-analog-sdk' -NameMatch '\.msi$'
    $msi = Join-Path $WorkDir $analog.Name
    Download-File -Url $analog.Url -OutPath $msi
    Write-Step "Installing $($analog.Name) silently (requires admin)"
    $p = Start-Process -FilePath "msiexec.exe" -ArgumentList "/i `"$msi`" /qn /norestart" -PassThru -Wait
    if ($p.ExitCode -ne 0) {
        throw "MSI install failed with exit code $($p.ExitCode)."
    }
    Write-Ok "Wooting Analog SDK $($analog.Tag) installed."
}

# --- 2. Wooting RGB SDK (ZIP, DLL copy) ---

$rgbTarget = "C:\Program Files\wooting-rgb-sdk"
$rgbDll    = Join-Path $rgbTarget "wooting-rgb-sdk.dll"

if (Test-Path $rgbDll) {
    Write-Ok "Wooting RGB SDK already installed at $rgbDll"
} else {
    $rgb = Get-LatestReleaseAsset -Repo 'WootingKb/wooting-rgb-sdk' -NameMatch 'win-x64\.zip$'
    $zip = Join-Path $WorkDir $rgb.Name
    Download-File -Url $rgb.Url -OutPath $zip

    $extract = Join-Path $WorkDir 'rgb-extract'
    if (Test-Path $extract) { Remove-Item -Recurse -Force $extract }
    Expand-Archive -Path $zip -DestinationPath $extract -Force

    $dll = Get-ChildItem -Path $extract -Recurse -Filter 'wooting-rgb-sdk.dll' -File | Select-Object -First 1
    if (-not $dll) {
        $dll = Get-ChildItem -Path $extract -Recurse -Filter '*rgb*.dll' -File | Select-Object -First 1
    }
    if (-not $dll) { throw "wooting-rgb-sdk.dll not found inside $zip" }

    try {
        New-Item -ItemType Directory -Path $rgbTarget -Force | Out-Null
        Copy-Item -Path $dll.FullName -Destination $rgbDll -Force
        Write-Ok "Wooting RGB SDK $($rgb.Tag) installed at $rgbDll"
    } catch {
        $fallback = Join-Path $env:LOCALAPPDATA "wooting-rgb-sdk"
        New-Item -ItemType Directory -Path $fallback -Force | Out-Null
        Copy-Item -Path $dll.FullName -Destination (Join-Path $fallback 'wooting-rgb-sdk.dll') -Force
        Write-Warn2 "No admin access to $rgbTarget. Installed to $fallback instead."
        Write-Warn2 "Add $fallback to your PATH or set WOOTING_RGB_SDK_DLL=$fallback\wooting-rgb-sdk.dll"
    }
}

Write-Ok "Done. Restart any shells you had open, then run: xentool list"
