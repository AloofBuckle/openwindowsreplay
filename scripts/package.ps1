param(
    [string]$OutDir = "dist\RustReplay",
    [string]$Target = "x86_64-pc-windows-msvc"
)

$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..")
$previousVplDll = $env:RUSTREPLAY_VPL_DLL
Push-Location $root
try {
    $vplCandidates = @(@(
        $env:RUSTREPLAY_VPL_DLL,
        "libvpl-2.dll",
        "libvpl.dll",
        "C:\Program Files (x86)\Intel\oneAPI\vpl\latest\bin\libvpl.dll",
        "C:\Program Files\Intel\oneAPI\vpl\latest\bin\libvpl.dll",
        "C:\msys64\ucrt64\bin\libvpl-2.dll"
    ) | Where-Object { $_ -and (Test-Path -LiteralPath $_) })
    if ($vplCandidates.Count -eq 0) {
        throw "No libvpl.dll/libvpl-2.dll was found to embed. Set RUSTREPLAY_VPL_DLL or install oneVPL/Intel Graphics Runtime on the packaging machine."
    }
    $env:RUSTREPLAY_VPL_DLL = (Resolve-Path -LiteralPath $vplCandidates[0]).Path
    cargo build --release --target $Target
    if (Test-Path $OutDir) { Remove-Item -LiteralPath $OutDir -Recurse -Force }
    New-Item -ItemType Directory -Force $OutDir | Out-Null
    $exePath = Join-Path "target\$Target\release" "rust_replay.exe"
    Copy-Item -LiteralPath $exePath -Destination (Join-Path $OutDir "rust_replay.exe") -Force
    Copy-Item -LiteralPath "README.md" -Destination (Join-Path $OutDir "README.md") -Force

    Compress-Archive -Path (Join-Path $OutDir "*") -DestinationPath "dist\RustReplay.zip" -Force
    Write-Host "Package generated: dist\RustReplay.zip"
    Write-Host "Embedded oneVPL runtime source: $env:RUSTREPLAY_VPL_DLL"
    Write-Host "Runtime extraction directory: %ProgramData%\OneVPL Replay"
} finally {
    Pop-Location
    if ($null -eq $previousVplDll) {
        Remove-Item Env:RUSTREPLAY_VPL_DLL -ErrorAction SilentlyContinue
    } else {
        $env:RUSTREPLAY_VPL_DLL = $previousVplDll
    }
}
