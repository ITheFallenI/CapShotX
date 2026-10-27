param(
    [switch]$Offline
)

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $RepoRoot

try {
    $cargoFlags = @()
    if ($Offline -or $env:CARGO_NET_OFFLINE -eq "true") {
        $cargoFlags += "--offline"
    }

    $defaultExe = Join-Path $RepoRoot "target\release\CapShotX.exe"
    $useStageDir = $false

    if (Test-Path $defaultExe) {
        try {
            $stream = [System.IO.File]::Open(
                $defaultExe,
                [System.IO.FileMode]::Open,
                [System.IO.FileAccess]::ReadWrite,
                [System.IO.FileShare]::None
            )
            $stream.Close()
        } catch {
            $useStageDir = $true
        }
    }

    if ($useStageDir) {
        $stageTarget = Join-Path $RepoRoot "target\installer-stage"
        Write-Host "[1/3] target\release\CapShotX.exe is currently running; building fresh payload in target\installer-stage..." -ForegroundColor Cyan
        & cargo build --release -p capshotx --target-dir $stageTarget @cargoFlags
        if ($LASTEXITCODE -ne 0) { throw "Failed to build CapShotX payload." }
        $payloadExe = Join-Path $stageTarget "release\CapShotX.exe"
    } else {
        Write-Host "[1/3] Building release CapShotX.exe payload..." -ForegroundColor Cyan
        & cargo build --release -p capshotx @cargoFlags
        if ($LASTEXITCODE -ne 0) { throw "Failed to build CapShotX payload." }
        $payloadExe = $defaultExe
    }

    if (-not (Test-Path $payloadExe)) {
        throw "Expected payload not found at $payloadExe"
    }

    Write-Host "[2/3] Embedding compressed payload and building CapShotX-Setup.exe..." -ForegroundColor Cyan
    $env:CAPSHOTX_PAYLOAD_EXE = $payloadExe
    & cargo build --release -p capshotx-installer @cargoFlags
    if ($LASTEXITCODE -ne 0) { throw "Failed to build CapShotX-Setup.exe." }

    $builtSetup = Join-Path $RepoRoot "target\release\CapShotX-Setup.exe"
    $distDir = Join-Path $RepoRoot "dist"
    New-Item -ItemType Directory -Force -Path $distDir | Out-Null

    $distSetup = Join-Path $distDir "CapShotX-Setup.exe"
    $distPortable = Join-Path $distDir "CapShotX.exe"
    Copy-Item -Force $builtSetup $distSetup
    Copy-Item -Force $payloadExe $distPortable

    $setupMb = [math]::Round((Get-Item $distSetup).Length / 1MB, 2)
    $appMb = [math]::Round((Get-Item $distPortable).Length / 1MB, 2)

    Write-Host "[3/3] Packaging complete!" -ForegroundColor Green
    Write-Host "  Installer : $distSetup ($setupMb MB)"
    Write-Host "  Portable  : $distPortable ($appMb MB)"
} finally {
    Remove-Item Env:\CAPSHOTX_PAYLOAD_EXE -ErrorAction SilentlyContinue
    Pop-Location
}
