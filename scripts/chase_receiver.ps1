# Chase-side telemetry receiver (Windows).
# Plug in the XBee USB adapter (check the COM port in Device Manager), then:
#
#   .\scripts\chase_receiver.ps1
#   $env:XBEE_DEST64 = "0013a20041aeb54e"; .\scripts\chase_receiver.ps1
$ErrorActionPreference = "Stop"

Set-Location (Join-Path $PSScriptRoot "..")

if (-not $env:XBEE_BAUD) { $env:XBEE_BAUD = "115200" }
if (-not $env:KEYS_DIR)  { $env:KEYS_DIR = (Join-Path (Get-Location) "keys") }

cargo build --release -p telemetry_receiver
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

if (-not (Test-Path (Join-Path $env:KEYS_DIR "authorized_sender.pub"))) {
    Write-Warning "$($env:KEYS_DIR)\authorized_sender.pub missing — copy the car's sender_ed25519.pub there (see TELEMETRY.md)."
}

& ".\target\release\telemetry_receiver.exe" @args
