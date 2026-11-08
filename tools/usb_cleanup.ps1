<#
.SYNOPSIS
    Remove the usb-rat payload from the USB and optionally clean Windows persistence.
.DESCRIPTION
    Run on the operator Windows box with the USB mounted. By default it removes
    the payload files in this folder (Sənədlər.exe, _sys, _tools, _docs) and
    then tries to remove the folder itself. -CleanWindows additionally stops
    running agents, removes the HKCU Run value, the %APPDATA% install folder,
    scheduled tasks and %LOCALAPPDATA%\WMProcess.
.PARAMETER UsbPath
    Payload folder (default: the folder containing this script, e.g. E:\usb-rat).
.PARAMETER CleanWindows
    Also remove local persistence and running agents on this machine.
.PARAMETER DryRun
    Print actions without deleting anything.
.PARAMETER Force
    Do not prompt for confirmation.
#>
param(
    [string]$UsbPath = "",
    [switch]$CleanWindows,
    [switch]$DryRun,
    [switch]$Force
)
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
if ($UsbPath -eq "") { $UsbPath = $here }
if (-not (Test-Path $UsbPath)) { throw "path not found: $UsbPath" }

$targets = @(
    (Join-Path $UsbPath "Sənədlər.exe"),
    (Join-Path $UsbPath "_sys"),
    (Join-Path $UsbPath "_tools"),
    (Join-Path $UsbPath "_docs"),
    (Join-Path $UsbPath "cleanup.ps1")
)

Write-Host "usb-rat cleanup"
Write-Host "  target : $UsbPath"
Write-Host "  windows: $([bool]$CleanWindows)"
if (-not $Force -and -not $DryRun) {
    $answer = Read-Host "Remove the payload from this folder? [y/N]"
    if ($answer -notmatch '^(y|yes)$') { Write-Host "aborted"; return }
}

foreach ($t in $targets) {
    if (-not (Test-Path $t)) { continue }
    if ($DryRun) { Write-Host "[dry-run] remove $t"; continue }
    Remove-Item $t -Recurse -Force -ErrorAction SilentlyContinue
    if (Test-Path $t) { Write-Warning "could not remove $t" } else { Write-Host "[+] removed $t" }
}

if ($CleanWindows) {
    $actions = @(
        "stop processes winhost/svcupdate/wmpsync",
        "remove HKCU Run value WindowsRuntimeHost",
        "remove $env:APPDATA\Microsoft\WindowsRuntime",
        "unregister scheduled tasks WindowsMediaSync*",
        "remove $env:LOCALAPPDATA\WMProcess"
    )
    foreach ($a in $actions) { Write-Host "  [windows] $a" }
    if (-not $DryRun) {
        Stop-Process -Name winhost,svcupdate,wmpsync -Force -ErrorAction SilentlyContinue
        Remove-ItemProperty "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name "WindowsRuntimeHost" -ErrorAction SilentlyContinue
        Remove-Item "$env:APPDATA\Microsoft\WindowsRuntime" -Recurse -Force -ErrorAction SilentlyContinue
        Get-ScheduledTask -ErrorAction SilentlyContinue |
            Where-Object { $_.TaskName -like "WindowsMediaSync*" } |
            Unregister-ScheduledTask -Confirm:$false -ErrorAction SilentlyContinue
        Remove-Item "$env:LOCALAPPDATA\WMProcess" -Recurse -Force -ErrorAction SilentlyContinue
        Write-Host "[+] windows persistence cleaned"
    }
}

if (-not $DryRun) {
    # Remove the payload folder if it is now empty (the running script may keep it).
    $left = @(Get-ChildItem -Force $UsbPath -ErrorAction SilentlyContinue)
    if ($left.Count -eq 0) {
        Remove-Item $UsbPath -Force -ErrorAction SilentlyContinue
        Write-Host "[+] removed folder $UsbPath"
    } else {
        Write-Host "[i] folder not empty; remove manually if desired: $UsbPath"
    }
}
