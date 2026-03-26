$ProjectRoot = Join-Path $PSScriptRoot "Source_Code"

if (-not (Test-Path $ProjectRoot)) {
    Write-Error "Project folder not found: $ProjectRoot"
    exit 1
}

Write-Host "Starting satellite and GCS from: $ProjectRoot"

Start-Process powershell -ArgumentList @(
    '-NoExit',
    '-Command',
    "Set-Location '$ProjectRoot'; `$Host.UI.RawUI.WindowTitle = 'Student A - Satellite'; cargo run --release --bin satellite"
)

Start-Sleep -Seconds 1

Start-Process powershell -ArgumentList @(
    '-NoExit',
    '-Command',
    "Set-Location '$ProjectRoot'; `$Host.UI.RawUI.WindowTitle = 'Student B - GCS'; cargo run --release --bin gcs"
)

Write-Host "Both windows launched."
Write-Host "Stop them with Ctrl+C in each window."