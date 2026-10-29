@echo off
setlocal
cd /d "%~dp0"

echo ============================================================
echo   CapShotX - Build ^& Update Local Instance (Testing Only)
echo ============================================================
echo.
echo This will close your running CapShotX instance, rebuild it,
echo overwrite your local installation, and relaunch it.
echo.
set "CONFIRM="
set /p "CONFIRM=Continue with local test build? (Y/N): "
if /I not "%CONFIRM%"=="Y" (
    echo.
    echo Cancelled.
    timeout /t 2 >nul
    exit /b 0
)
echo.

echo [1/3] Closing running CapShotX / CapShotX_Editor instances (if any)...
taskkill /IM CapShotX.exe /T /F >nul 2>&1
taskkill /IM CapShotX_Editor.exe /T /F >nul 2>&1
timeout /t 1 /nobreak >nul

echo [2/3] Building release CapShotX.exe and CapShotX_Editor.exe...
cargo build --release -p capshotx
if %ERRORLEVEL% neq 0 (
    echo.
    echo [ERROR] Build failed.
    pause
    exit /b %ERRORLEVEL%
)

set "BUILT_EXE=%~dp0target\release\CapShotX.exe"
set "BUILT_EDITOR=%~dp0target\release\CapShotX_Editor.exe"
set "INSTALL_DIR=%LOCALAPPDATA%\Programs\CapShotX"
set "INSTALLED_EXE=%INSTALL_DIR%\CapShotX.exe"
set "INSTALLED_EDITOR=%INSTALL_DIR%\CapShotX_Editor.exe"

if exist "%~dp0dist" (
    copy /Y "%BUILT_EXE%" "%~dp0dist\CapShotX.exe" >nul
    copy /Y "%BUILT_EDITOR%" "%~dp0dist\CapShotX_Editor.exe" >nul
)

echo [3/3] Updating local installation and launching...
if exist "%INSTALL_DIR%" (
    copy /Y "%BUILT_EXE%" "%INSTALLED_EXE%" >nul
    if %ERRORLEVEL% neq 0 (
        echo [ERROR] Could not overwrite %INSTALLED_EXE%
        pause
        exit /b 1
    )
    copy /Y "%BUILT_EDITOR%" "%INSTALLED_EDITOR%" >nul
    start "" "%INSTALLED_EXE%"
    echo.
    echo [SUCCESS] Updated and launched: %INSTALLED_EXE% (+ %INSTALLED_EDITOR%)
) else (
    start "" "%BUILT_EXE%"
    echo.
    echo [SUCCESS] Launched: %BUILT_EXE% (+ %BUILT_EDITOR%)
)
