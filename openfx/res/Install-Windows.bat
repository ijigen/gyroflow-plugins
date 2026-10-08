@echo off
chcp 65001 >nul
rem Installs fpSupGyroflow.ofx.bundle for DaVinci Resolve. Right-click > Run as administrator.
net session >nul 2>&1
if errorlevel 1 (
  echo Right-click Install-Windows.bat and choose "Run as administrator".
  echo 請對 Install-Windows.bat 按右鍵，選「以系統管理員身分執行」。
  pause
  exit /b 1
)
set "DEST=%CommonProgramFiles%\OFX\Plugins"
if not exist "%DEST%" mkdir "%DEST%"
if exist "%DEST%\fpSupGyroflow.ofx.bundle" rmdir /s /q "%DEST%\fpSupGyroflow.ofx.bundle"
xcopy /e /i /q /y "%~dp0fpSupGyroflow.ofx.bundle" "%DEST%\fpSupGyroflow.ofx.bundle" >nul
if errorlevel 1 (
  echo Copy failed. Is DaVinci Resolve still open? Close it and try again.
  pause
  exit /b 1
)
echo Installed to %DEST%\fpSupGyroflow.ofx.bundle. Restart DaVinci Resolve.
echo 已安裝。請重開 DaVinci Resolve。
pause
