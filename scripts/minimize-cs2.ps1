[CmdletBinding()]
param(
    [string]$ProcessName = "cs2",
    [ValidateRange(100, 10000)]
    [int]$TimeoutMs = 2000
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if (-not ("RustReplayMinimizeNative" -as [type])) {
    Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

public static class RustReplayMinimizeNative
{
    [DllImport("user32.dll")]
    public static extern bool ShowWindowAsync(IntPtr window, int command);

    [DllImport("user32.dll")]
    public static extern bool IsIconic(IntPtr window);

    [DllImport("user32.dll")]
    public static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
}
"@
}

function Get-WindowOwner {
    param([IntPtr]$Window)

    [uint32]$ownerPid = 0
    [void][RustReplayMinimizeNative]::GetWindowThreadProcessId($Window, [ref]$ownerPid)
    $owner = Get-Process -Id $ownerPid -ErrorAction SilentlyContinue
    [pscustomobject]@{
        Handle  = "0x{0:X}" -f $Window.ToInt64()
        Pid     = $ownerPid
        Process = if ($owner) { $owner.ProcessName } else { $null }
    }
}

$targetProcess = Get-Process -Name $ProcessName -ErrorAction Stop |
    Where-Object { $_.MainWindowHandle -ne 0 } |
    Select-Object -First 1
if (-not $targetProcess) {
    throw "Process '$ProcessName' has no main window."
}

$targetWindow = [IntPtr]$targetProcess.MainWindowHandle
$before = [RustReplayMinimizeNative]::GetForegroundWindow()
$showResult = [RustReplayMinimizeNative]::ShowWindowAsync($targetWindow, 6)
$deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMs)
do {
    Start-Sleep -Milliseconds 50
    $minimized = [RustReplayMinimizeNative]::IsIconic($targetWindow)
} while (-not $minimized -and [DateTime]::UtcNow -lt $deadline)
$after = [RustReplayMinimizeNative]::GetForegroundWindow()

[pscustomobject]@{
    TargetPid       = $targetProcess.Id
    TargetHandle    = "0x{0:X}" -f $targetWindow.ToInt64()
    Before          = Get-WindowOwner $before
    ShowWindowAsync = $showResult
    IsMinimized     = $minimized
    After           = Get-WindowOwner $after
    LeftForeground  = $after -ne $targetWindow
}

if (-not $minimized) {
    exit 2
}
