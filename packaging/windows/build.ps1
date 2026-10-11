# Build the Windows portable zip in dist\ (diffcraft.exe = GUI, diffcraft-cli.exe = console).
Set-Location (Join-Path $PSScriptRoot '..\..')
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches.Groups[1].Value
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
cargo build --release -p diffcraft
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
# Native tools write progress to stderr; only fail hard on PowerShell errors from here on.
$ErrorActionPreference = 'Stop'
$stage = "dist\diffcraft-$version-windows-$arch"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $stage | Out-Null
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { 'target' }
Copy-Item "$target\release\diffcraft.exe", "$target\release\diffcraft-cli.exe", README.md, LICENSE-MIT, LICENSE-APACHE $stage
Compress-Archive -Path "$stage\*" -DestinationPath "$stage.zip" -Force
Write-Host "Wrote $stage.zip"
