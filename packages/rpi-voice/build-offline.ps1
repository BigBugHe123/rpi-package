# Build rpi-voice with the official sherpa-onnx Rust bindings and offline STT.
# The crate downloads matching static native libraries automatically, or uses
# SHERPA_ONNX_LIB_DIR when provided. No libclang/bindgen setup is needed.
#
# Usage (from anywhere): pwsh packages/rpi-voice/build-offline.ps1

$ErrorActionPreference = "Stop"

$voiceWorkspaceRoot = git -C $PSScriptRoot rev-parse --show-toplevel 2>$null
if ($LASTEXITCODE -ne 0 -or -not $voiceWorkspaceRoot) {
    $voiceWorkspaceRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
}
Push-Location $voiceWorkspaceRoot
try {
    cargo build -p rpi-voice --release --features local-stt
    if ($LASTEXITCODE -ne 0) {
        throw "rpi-voice build failed with exit code $LASTEXITCODE"
    }
} finally {
    Pop-Location
}
