@echo off
setlocal
cd /d "%~dp0"

echo ============================================================
echo   CapShotX - Bump Version (+0.1), Build ^& Publish Release
echo ============================================================
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
