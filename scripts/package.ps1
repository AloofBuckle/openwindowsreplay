param(
    [string]$OutDir = "dist\RustReplay"
)

$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..")
Push-Location $root
try {
    cargo build --release
    if (Test-Path $OutDir) { Remove-Item -LiteralPath $OutDir -Recurse -Force }
    New-Item -ItemType Directory -Force $OutDir | Out-Null
    Copy-Item -LiteralPath "target\release\rust_replay.exe" -Destination (Join-Path $OutDir "rust_replay.exe") -Force
    Copy-Item -LiteralPath "README.md" -Destination (Join-Path $OutDir "README.md") -Force

    $vplCandidates = @(@(
        $env:RUSTREPLAY_VPL_DLL,
        "libvpl.dll",
        "libvpl-2.dll",
        "C:\Program Files (x86)\Intel\oneAPI\vpl\latest\bin\libvpl.dll",
        "C:\Program Files\Intel\oneAPI\vpl\latest\bin\libvpl.dll",
        "C:\msys64\ucrt64\bin\libvpl-2.dll"
    ) | Where-Object { $_ -and (Test-Path $_) })

    if ($vplCandidates.Count -gt 0) {
        $src = $vplCandidates[0]
        $name = Split-Path $src -Leaf
        Copy-Item -LiteralPath $src -Destination (Join-Path $OutDir $name) -Force
        "Bundled oneVPL user-mode dependency: $name" | Set-Content -Encoding UTF8 (Join-Path $OutDir "PACKAGED_DEPENDENCIES.txt")
    } else {
        "No libvpl.dll/libvpl-2.dll was found to bundle. The target machine must provide oneVPL/Intel Graphics Runtime." | Set-Content -Encoding UTF8 (Join-Path $OutDir "PACKAGED_DEPENDENCIES.txt")
    }

    Compress-Archive -Path (Join-Path $OutDir "*") -DestinationPath "dist\RustReplay.zip" -Force
    Write-Host "Package generated: dist\RustReplay.zip"
} finally {
    Pop-Location
}
