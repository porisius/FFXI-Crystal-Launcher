# Build first. The signing key must stay outside source and distributed packages.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$PackageFolder,
    [Parameter(Mandatory)][string]$PrivateKeyPath,
    [Parameter(Mandatory)][string]$Version,
    [Parameter(Mandatory)][string]$OutputFolder,
    # The GitHub repository releases are published to; must match the one built into the launcher
    [string]$Repository = $(if ($env:CRYSTAL_RELEASE_REPO) { $env:CRYSTAL_RELEASE_REPO } else { 'porisius/FFXI-Crystal-Launcher' }),
    [switch]$Offline
)
$ErrorActionPreference = 'Stop'
$package = (Resolve-Path -LiteralPath $PackageFolder).Path
$privateKey = (Resolve-Path -LiteralPath $PrivateKeyPath).Path
$output = [IO.Path]::GetFullPath($OutputFolder)
$launcher = Join-Path $package 'Crystal-Launcher.exe'
if (-not (Test-Path -LiteralPath $launcher -PathType Leaf)) { throw "No packaged launcher in $package" }
$manifest = Join-Path (Split-Path -Parent $PSScriptRoot) 'Cargo.toml'
[string[]]$offlineArgs = @()
if ($Offline) { $offlineArgs = @('--offline') }
$url = "https://github.com/$Repository/releases/download/v$Version/Crystal-Launcher-$Version.zip"
# The delivered launcher asks for administrator rights. Signing needs none;
# use the ordinary source build's early, headless command instead.
cargo run --locked --manifest-path $manifest @offlineArgs -- --sign-launcher-release $privateKey $Version $package $url $output
if ($LASTEXITCODE -ne 0) { throw "Release signing failed ($LASTEXITCODE)." }
if (-not (Test-Path -LiteralPath (Join-Path $output 'launcher-update.json') -PathType Leaf)) {
    throw "Signing did not produce launcher-update.json."
}
Write-Host "Signed release files are in $output. This script does not upload them."
