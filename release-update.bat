@echo off
setlocal
cd /d "%~dp0"

echo ============================================================
echo   CapShotX - Bump Version (+0.1), Build ^& Publish Release
echo ============================================================
echo.
echo This will:
echo   1. Bump the version in Cargo.toml (+0.1)
echo   2. Build release CapShotX.exe and CapShotX-Setup.exe
echo   3. Commit, push to GitHub, and publish a new GitHub Release
echo.
set "CONFIRM="
set /p "CONFIRM=Are you sure you want to publish a new release? (Y/N): "
if /I not "%CONFIRM%"=="Y" (
    echo.
    echo Cancelled.
    timeout /t 2 >nul
    exit /b 0
)
echo.

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0packaging\build-installer.ps1" -BumpMinor -CommitAndPush -PublishRelease %*
set EXIT_CODE=%ERRORLEVEL%

if %EXIT_CODE% neq 0 (
    echo.
    echo [ERROR] Release build or upload failed (exit code %EXIT_CODE%).
    pause
    exit /b %EXIT_CODE%
)

echo.
echo [SUCCESS] Version bumped, committed, and published to GitHub Releases!
pause
