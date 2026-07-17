[CmdletBinding()]
param(
    [string]$ProcessName = "cs2",
    [ValidateRange(1, 20)]
    [int]$StableChecks = 5,
    [ValidateRange(10, 2000)]
    [int]$CheckDelayMs = 200
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if (-not ("RustReplayFocusNative" -as [type])) {
    Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

public static class RustReplayFocusNative
{
    [DllImport("user32.dll")]
    public static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr window);

    [DllImport("user32.dll")]
    public static extern bool ShowWindowAsync(IntPtr window, int command);

    [DllImport("user32.dll")]
    public static extern bool BringWindowToTop(IntPtr window);

    [DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);

    [DllImport("user32.dll")]
    public static extern bool AttachThreadInput(uint attachThread, uint attachToThread, bool attach);

    [DllImport("user32.dll")]
    public static extern IntPtr SetFocus(IntPtr window);

    [DllImport("kernel32.dll")]
    public static extern uint GetCurrentThreadId();
}
"@
}

function Get-WindowOwner {
    param([IntPtr]$Window)

    [uint32]$ownerPid = 0
    [void][RustReplayFocusNative]::GetWindowThreadProcessId($Window, [ref]$ownerPid)
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
$before = [RustReplayFocusNative]::GetForegroundWindow()
$showResult = [RustReplayFocusNative]::ShowWindowAsync($targetWindow, 9)
$bringResult = [RustReplayFocusNative]::BringWindowToTop($targetWindow)
$focusResult = [RustReplayFocusNative]::SetForegroundWindow($targetWindow)
Start-Sleep -Milliseconds 300
$after = [RustReplayFocusNative]::GetForegroundWindow()
$fallbackUsed = $false

if ($after -ne $targetWindow) {
    $fallbackUsed = $true
    [uint32]$foregroundPid = 0
    [uint32]$targetPid = 0
    $foregroundThread = [RustReplayFocusNative]::GetWindowThreadProcessId(
        $after,
        [ref]$foregroundPid
    )
    $targetThread = [RustReplayFocusNative]::GetWindowThreadProcessId(
        $targetWindow,
        [ref]$targetPid
    )
    $currentThread = [RustReplayFocusNative]::GetCurrentThreadId()
    $attachedForeground = $false
    $attachedTarget = $false

    try {
        if ($foregroundThread -ne 0 -and $foregroundThread -ne $currentThread) {
            $attachedForeground = [RustReplayFocusNative]::AttachThreadInput(
                $currentThread,
                $foregroundThread,
                $true
            )
        }
        if ($targetThread -ne 0 -and $targetThread -ne $currentThread) {
            $attachedTarget = [RustReplayFocusNative]::AttachThreadInput(
                $currentThread,
                $targetThread,
                $true
            )
        }
        [void][RustReplayFocusNative]::ShowWindowAsync($targetWindow, 9)
        [void][RustReplayFocusNative]::BringWindowToTop($targetWindow)
        $focusResult = [RustReplayFocusNative]::SetForegroundWindow($targetWindow)
        [void][RustReplayFocusNative]::SetFocus($targetWindow)
    }
    finally {
        if ($attachedTarget) {
            [void][RustReplayFocusNative]::AttachThreadInput(
                $currentThread,
                $targetThread,
                $false
            )
        }
        if ($attachedForeground) {
            [void][RustReplayFocusNative]::AttachThreadInput(
                $currentThread,
                $foregroundThread,
                $false
            )
        }
    }

    Start-Sleep -Milliseconds 300
    $after = [RustReplayFocusNative]::GetForegroundWindow()
}

$stable = 0
for ($index = 0; $index -lt $StableChecks; $index++) {
    if ([RustReplayFocusNative]::GetForegroundWindow() -eq $targetWindow) {
        $stable++
    }
    Start-Sleep -Milliseconds $CheckDelayMs
}

$focused = $stable -eq $StableChecks
[pscustomobject]@{
    TargetPid                 = $targetProcess.Id
    TargetHandle              = "0x{0:X}" -f $targetWindow.ToInt64()
    Before                    = Get-WindowOwner $before
    ShowWindowAsync           = $showResult
    BringWindowToTop          = $bringResult
    SetForegroundWindow       = $focusResult
    FallbackAttachThreadInput = $fallbackUsed
    After                     = Get-WindowOwner $after
    StableChecks              = "$stable/$StableChecks"
    Focused                   = $focused
}

if (-not $focused) {
    exit 2
}
