param(
    [string]$OutDir = "dist\RustReplay",
    [string]$Target = "x86_64-pc-windows-msvc"
)

$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..")
Push-Location $root
try {
    cargo build --release --target $Target
    if (Test-Path $OutDir) { Remove-Item -LiteralPath $OutDir -Recurse -Force }
    New-Item -ItemType Directory -Force $OutDir | Out-Null
    $exePath = Join-Path "target\$Target\release" "rust_replay.exe"
    Copy-Item -LiteralPath $exePath -Destination (Join-Path $OutDir "rust_replay.exe") -Force
    Copy-Item -LiteralPath "README.md" -Destination (Join-Path $OutDir "README.md") -Force

    $vplCandidates = @(@(
        $env:RUSTREPLAY_VPL_DLL,
        "libvpl-2.dll",
        "libvpl.dll",
        "vpl.dll",
        "onevpl.dll",
        "C:\Program Files (x86)\Intel\oneAPI\vpl\latest\bin\libvpl.dll",
        "C:\Program Files\Intel\oneAPI\vpl\latest\bin\libvpl.dll",
        "C:\msys64\ucrt64\bin\libvpl-2.dll"
    ) | Where-Object { $_ -and (Test-Path $_) })

    $packaged = @()

    if ($vplCandidates.Count -gt 0) {
        $src = (Resolve-Path $vplCandidates[0]).Path
        $name = Split-Path $src -Leaf
        Copy-Item -LiteralPath $src -Destination (Join-Path $OutDir $name) -Force
        $packaged += "Bundled oneVPL user-mode dependency: $name"
        $packaged += "Source: $src"
    } else {
        throw "No libvpl.dll/libvpl-2.dll/vpl.dll/onevpl.dll was found to bundle. Set RUSTREPLAY_VPL_DLL or install oneVPL/Intel Graphics Runtime on the packaging machine."
    }

    $packaged | Set-Content -Encoding UTF8 (Join-Path $OutDir "PACKAGED_DEPENDENCIES.txt")

    Compress-Archive -Path (Join-Path $OutDir "*") -DestinationPath "dist\RustReplay.zip" -Force
    Write-Host "Package generated: dist\RustReplay.zip"
} finally {
    Pop-Location
}
