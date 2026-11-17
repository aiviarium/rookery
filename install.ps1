# Installs rookery for this user on Windows: the released program, checked
# against its SHA-256, into %LOCALAPPDATA%\Programs\rookery (or
# ROOKERY_INSTALL_DIR). On Windows rookery uses the hosts of a rookery serve elsewhere.
#
#   irm https://raw.githubusercontent.com/aiviarium/rookery/main/install.ps1 | iex
#
# ROOKERY_VERSION picks a release tag such as v0.5.0; the default is the latest.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$repo = 'aiviarium/rookery'
$asset = 'rookery-x86_64-windows.zip'
$dir = if ($env:ROOKERY_INSTALL_DIR) { $env:ROOKERY_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\rookery' }
$version = if ($env:ROOKERY_VERSION) { $env:ROOKERY_VERSION } else { 'latest' }

function Fail([string] $message) {
    Write-Host "install: $message" -ForegroundColor Red
    # Not exit: the script runs inside the caller's PowerShell, which exit would close.
    throw "install: $message"
}

if (-not [Environment]::Is64BitOperatingSystem -or $env:PROCESSOR_ARCHITECTURE -eq 'ARM64') {
    Fail "no released program for this processor: build it with cargo install --locked --git https://github.com/$repo rookery"
}

$url = if ($version -eq 'latest') {
    "https://github.com/$repo/releases/latest/download"
} else {
    "https://github.com/$repo/releases/download/$version"
}

$work = Join-Path ([IO.Path]::GetTempPath()) ("rookery-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $work | Out-Null
try {
    $zip = Join-Path $work $asset
    try { Invoke-WebRequest -UseBasicParsing -Uri "$url/$asset" -OutFile $zip } catch { Fail "could not download $url/$asset" }
    try { Invoke-WebRequest -UseBasicParsing -Uri "$url/$asset.sha256" -OutFile "$zip.sha256" } catch { Fail 'could not download the checksum' }
    $want = ((Get-Content -Raw "$zip.sha256").Trim() -split '\s+')[0]
    if ((Get-FileHash -Algorithm SHA256 $zip).Hash -ne $want) { Fail 'the download does not match its checksum' }
    Expand-Archive -Path $zip -DestinationPath (Join-Path $work 'unpacked')
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $program = Join-Path $dir 'rookery.exe'
    Copy-Item -Force (Join-Path $work 'unpacked\rookery.exe') $program
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

$said = try { & $program --version } catch { 'rookery' }
Write-Host "Installed $said to $program"
$path = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not (($path -split ';') -contains $dir)) {
    Write-Host "Add $dir to your Path to run it as rookery:"
    Write-Host "  [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path', 'User') + ';$dir', 'User')"
}
Write-Host "Next: rookery login URL, with the setup key of your rookery server."
