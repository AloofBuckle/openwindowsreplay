param(
    [string]$Output = "$PSScriptRoot\..\target\release\nvfbc_nvenc_direct.exe"
)

$ErrorActionPreference = "Stop"
$repo = (Resolve-Path "$PSScriptRoot\..").Path
$nvfbcInclude = Join-Path $env:USERPROFILE "Desktop\nvfbc-relay-reference\inc\NvFBC"
$nvencHeader = Join-Path $env:USERPROFILE "Documents\1\老的Cpp即时回放项目\insreplay\third-party\nv-codec-headers\include\ffnvcodec\nvEncodeAPI.h"
$gxx = "C:\msys64\mingw64\bin\g++.exe"

foreach ($required in @($nvfbcInclude, $nvencHeader, $gxx)) {
    if (-not (Test-Path -LiteralPath $required)) {
        throw "Missing build dependency: $required"
    }
}

$generatedInclude = Join-Path $repo "target\nvfbc_nvenc_include"
New-Item -ItemType Directory -Force -Path $generatedInclude | Out-Null
Copy-Item -LiteralPath $nvencHeader -Destination (Join-Path $generatedInclude "nvEncodeAPI.h") -Force
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Output) | Out-Null

& $gxx `
    -std=c++20 `
    -O2 `
    -Wall `
    -Wextra `
    -Werror `
    "-I$nvfbcInclude" `
    "-I$generatedInclude" `
    (Join-Path $repo "experiments\nvfbc_nvenc_direct.cpp") `
    -o $Output `
    -ld3d9 `
    -ldxgi

if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

Write-Output $Output
