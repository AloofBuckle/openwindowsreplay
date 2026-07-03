param(
    [Parameter(Mandatory=$true)]
    [string]$Candidate,
    [string]$MediaInfo = "MediaInfo.exe"
)

$ErrorActionPreference = "Stop"
$text = & $MediaInfo $Candidate | Out-String -Width 240

$required = @(
    "Format                                   : MPEG-4",
    "Format profile                           : Base Media / Version 2",
    "Codec ID                                 : mp42",
    "Format                                   : HEVC",
    "Codec ID                                 : hvc1",
    "Codec ID/Info                            : High Efficiency Video Coding",
    "Frame rate mode                          : Variable",
    "Color space                              : YUV",
    "Chroma subsampling                       : 4:2:0",
    "Bit depth                                : 10 bits",
    "Color range                              : Full",
    "Color primaries                          : BT.2020",
    "Transfer characteristics                 : PQ",
    "Matrix coefficients                      : BT.2020 non-constant",
    "Codec configuration box                  : hvcC",
    "Format                                   : AAC LC",
    "Codec ID                                 : mp4a-40-2",
    "Bit rate mode                            : Constant",
    "Bit rate                                 : 192 kb/s",
    "Channel(s)                               : 2 channels",
    "Channel layout                           : L R",
    "Sampling rate                            : 48.0 kHz",
    "Frame rate                               : 46.875 FPS (1024 SPF)",
    "Compression mode                         : Lossy",
    "Title                                    : SoundHandle / System sounds",
    "Encoded date                             : 2026-07-03 18:04:34 UTC",
    "Tagged date                              : 2026-07-03 18:04:34 UTC"
)

$missing = @()
foreach ($item in $required) {
    if (-not $text.Contains($item)) {
        $missing += $item
    }
}
if (-not ($text -match "mdhd_Duration\s*:")) {
    $missing += "mdhd_Duration                            : <present>"
}

if ($missing.Count -eq 0) {
    Write-Host "MediaInfo sample-required fields: OK"
    exit 0
}

Write-Host "MediaInfo missing fields:"
$missing | ForEach-Object { Write-Host "  $_" }
exit 1
