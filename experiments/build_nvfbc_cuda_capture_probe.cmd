@echo off
setlocal
set "PATH=%PATH:"=%"
call C:\PROGRA~2\MICROS~2\2022\BUILDT~1\VC\AUXILI~1\Build\vcvars64.bat >nul
if errorlevel 1 exit /b %errorlevel%

"%CUDA_PATH%\bin\nvcc.exe" ^
  -std=c++20 ^
  -O2 ^
  -Xcompiler "/W4,/WX,/EHsc,/utf-8" ^
  -I"%NVFBC_CUDA_INCLUDE%" ^
  "%~dp0nvfbc_cuda_capture_probe.cpp" ^
  -o "%NVFBC_CUDA_PROBE_OUTPUT%" ^
  -lcuda ^
  -ld3d9 ^
  -luser32

exit /b %errorlevel%
