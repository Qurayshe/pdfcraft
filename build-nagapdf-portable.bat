@echo off
setlocal enabledelayedexpansion

set "VER=0.2.0"
set "PDFCRAFT_REQUIRE_WINRES=1"

echo Building NagaPDF portable v%VER% (nagapdf.exe + nagapdfcli.exe)...
pushd "%~dp0"

cargo build --release -p pdfcraft --bin nagapdf
if errorlevel 1 (
    popd
    exit /b 1
)

cargo build --release -p pdfcraft-cli --bin nagapdfcli
if errorlevel 1 (
    popd
    exit /b 1
)

set "DIST_ROOT=%~dp0dist\release"
set "STAGE=%DIST_ROOT%\nagapdf-%VER%-windows-x64-portable"
set "ZIP=%DIST_ROOT%\nagapdf-%VER%-windows-x64-portable.zip"

if exist "%STAGE%" rmdir /s /q "%STAGE%"
mkdir "%STAGE%"

copy /y "target\release\nagapdf.exe" "%STAGE%\nagapdf.exe" >nul
copy /y "target\release\nagapdfcli.exe" "%STAGE%\nagapdfcli.exe" >nul
copy /y "packaging\windows\portable.txt" "%STAGE%\portable.txt" >nul
if exist "CHANGES-NAGAPDF.md" copy /y "CHANGES-NAGAPDF.md" "%STAGE%\CHANGES-NAGAPDF.md" >nul
if exist "LICENSE-MIT" copy /y "LICENSE-MIT" "%STAGE%\LICENSE-MIT" >nul
if exist "LICENSE-APACHE" copy /y "LICENSE-APACHE" "%STAGE%\LICENSE-APACHE" >nul

if exist "%ZIP%" del /q "%ZIP%"
powershell -NoProfile -Command "Compress-Archive -Path '%STAGE%' -DestinationPath '%ZIP%' -Force"
if errorlevel 1 (
    popd
    exit /b 1
)

echo.
echo Portable folder: %STAGE%
echo Portable zip:    %ZIP%
popd
endlocal
