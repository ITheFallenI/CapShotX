param(
    [switch]$Offline,
    [switch]$BumpMinor,
    [switch]$CommitAndPush,
    [switch]$PublishRelease,
    [string]$ReleaseNotes = ""
)

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $RepoRoot

$cargoTomlPath = Join-Path $RepoRoot "Cargo.toml"
$originalCargoToml = $null
$buildSucceeded = $false

try {
    if ($BumpMinor) {
        $originalCargoToml = Get-Content $cargoTomlPath -Raw
        if ($originalCargoToml -match '(?m)^version\s*=\s*"(\d+)\.(\d+)\.(\d+)"') {
            $major = [int]$Matches[1]
            $minor = [int]$Matches[2] + 1
            $patch = 0
            $oldVer = "$($Matches[1]).$($Matches[2]).$($Matches[3])"
            $newVer = "$major.$minor.$patch"
            $updatedToml = [regex]::Replace(
                $originalCargoToml,
                '(?m)^version\s*=\s*"\d+\.\d+\.\d+"',
                "version = `"$newVer`"",
                1
            )
            [System.IO.File]::WriteAllText($cargoTomlPath, $updatedToml, [System.Text.UTF8Encoding]::new($false))
            Write-Host "[0/3] Bumped version in Cargo.toml: $oldVer -> $newVer" -ForegroundColor Yellow
        } else {
            throw "Could not find 'version = `"X.Y.Z`"' in Cargo.toml"
        }
    }

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
    $buildSucceeded = $true

    $cargoToml = Get-Content $cargoTomlPath -Raw
    if ($cargoToml -match '(?m)^version\s*=\s*"([^"]+)"') {
        $ver = $Matches[1]
    } else {
        throw "Could not parse version from Cargo.toml"
    }
    $tag = "v$ver"

    if ($CommitAndPush) {
        Write-Host "Committing and pushing $tag to GitHub..." -ForegroundColor Cyan
        $prevEap = $ErrorActionPreference
        $ErrorActionPreference = "Continue"
        & git add -A
        if ($LASTEXITCODE -ne 0) { $ErrorActionPreference = $prevEap; throw "git add failed" }
        $status = & git status --porcelain
        if (-not [string]::IsNullOrWhiteSpace(($status -join ""))) {
            & git commit -m "Release $tag"
            if ($LASTEXITCODE -ne 0) { $ErrorActionPreference = $prevEap; throw "git commit failed" }
        }
        & git push origin main
        if ($LASTEXITCODE -ne 0) { $ErrorActionPreference = $prevEap; throw "git push failed" }
        $ErrorActionPreference = $prevEap
    }

    if ($PublishRelease) {
        $setupAsset = "$distSetup#CapShotX-Setup.exe (Windows Installer)"
        $portableAsset = "$distPortable#CapShotX.exe (Portable)"

        Write-Host "Publishing $tag to GitHub Releases..." -ForegroundColor Cyan
        $prevEap = $ErrorActionPreference
        $ErrorActionPreference = "Continue"
        & gh release view $tag *> $null
        $releaseExists = ($LASTEXITCODE -eq 0)

        if ($releaseExists) {
            & gh release upload $tag $setupAsset $portableAsset --clobber
            if ($LASTEXITCODE -ne 0) { $ErrorActionPreference = $prevEap; throw "Failed to upload release assets to $tag" }
            Write-Host "Updated assets on existing release $tag!" -ForegroundColor Green
        } else {
            if ([string]::IsNullOrWhiteSpace($ReleaseNotes)) {
                & gh release create $tag $setupAsset $portableAsset --title "CapShotX $tag" --generate-notes
            } else {
                & gh release create $tag $setupAsset $portableAsset --title "CapShotX $tag" --notes $ReleaseNotes
            }
            if ($LASTEXITCODE -ne 0) { $ErrorActionPreference = $prevEap; throw "Failed to create GitHub release $tag" }
            Write-Host "Published new GitHub release $tag!" -ForegroundColor Green
        }
        $ErrorActionPreference = $prevEap
    }
} finally {
    if ($BumpMinor -and -not $buildSucceeded -and $null -ne $originalCargoToml) {
        [System.IO.File]::WriteAllText($cargoTomlPath, $originalCargoToml, [System.Text.UTF8Encoding]::new($false))
        Write-Host "Build failed; reverted version bump in Cargo.toml." -ForegroundColor Yellow
    }
    Remove-Item Env:\CAPSHOTX_PAYLOAD_EXE -ErrorAction SilentlyContinue
    Pop-Location
}
