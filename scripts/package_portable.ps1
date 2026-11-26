$ErrorActionPreference = "Stop"

# Build a self-contained portable Windows release ZIP for Spotirust.
# Optional maintainer environment variables:
#   $env:SPOTIRUST_CLIENT_ID     - Spotify OAuth PKCE public Client ID (baked at compile time)
#   $env:SPOTIRUST_REDIRECT_PORT - Optional fixed loopback port (0 = dynamic port)

$RootDir = Resolve-Path (Join-Path $PSScriptRoot "..")
Push-Location $RootDir
try {
    Write-Host "==> Building Spotirust release binary..."
    cargo build --release

    $StageDir = Join-Path $RootDir "target\portable\spotirust"
    $Archive = Join-Path $RootDir "target\portable\spotirust-windows-x86_64-portable.zip"

    if (Test-Path $StageDir) { Remove-Item -Recurse -Force $StageDir }
    if (Test-Path $Archive) { Remove-Item -Force $Archive }
    New-Item -ItemType Directory -Force -Path $StageDir | Out-Null

    Copy-Item "target\release\spotirust.exe" $StageDir
    Copy-Item "README.md", "LICENSE" $StageDir
    foreach ($doc in @("PRODUCTION_READINESS.md", "PERFORMANCE_REPORT.md")) {
        if (Test-Path $doc) { Copy-Item $doc $StageDir }
    }
    Copy-Item -Recurse "assets", "docs" $StageDir
    New-Item -ItemType File -Force -Path (Join-Path $StageDir "portable.flag") | Out-Null

    Compress-Archive -Path (Join-Path $StageDir "*") -DestinationPath $Archive
    Write-Host "==> Portable bundle created: $Archive"
} finally {
    Pop-Location
}
