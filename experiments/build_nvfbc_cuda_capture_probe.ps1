param(
    [string]$Output = "$PSScriptRoot\..\target\release\nvfbc_cuda_capture_probe.exe"
)

$ErrorActionPreference = "Stop"
$nvfbcInclude = Join-Path $env:USERPROFILE "Desktop\nvfbc-relay-reference\inc\NvFBC"
$cudaRoot = $env:CUDA_PATH
$nvcc = Join-Path $cudaRoot "bin\nvcc.exe"
$vcvars = Join-Path ${env:ProgramFiles(x86)} `
    "Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
$buildCmd = Join-Path $PSScriptRoot "build_nvfbc_cuda_capture_probe.cmd"

foreach ($required in @($nvfbcInclude, $cudaRoot, $nvcc, $vcvars, $buildCmd)) {
    if (-not (Test-Path -LiteralPath $required)) {
        throw "Missing build dependency: $required"
    }
}

New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Output) | Out-Null

$env:NVFBC_CUDA_PROBE_OUTPUT = $Output
$env:NVFBC_CUDA_INCLUDE = $nvfbcInclude
try {
    & $buildCmd
} finally {
    Remove-Item Env:NVFBC_CUDA_PROBE_OUTPUT -ErrorAction SilentlyContinue
    Remove-Item Env:NVFBC_CUDA_INCLUDE -ErrorAction SilentlyContinue
}

if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

Write-Output $Output
