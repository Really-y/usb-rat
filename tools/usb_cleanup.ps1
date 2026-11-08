<#
.SYNOPSIS
    Remove the usb-rat payload from the USB, clean Windows persistence, and show
    a verification report afterwards.
.DESCRIPTION
    Run on the operator Windows box with the USB mounted. Removes the payload
    files in this folder (Sənədlər.exe, _sys, _tools, _docs, cleanup.ps1) and
    then prints, for every item, whether it is gone. -CleanWindows additionally
    stops running agents, removes the HKCU Run value, the %APPDATA% install
    folder, scheduled tasks and %LOCALAPPDATA%\WMProcess, then verifies each.
.PARAMETER UsbPath
    Payload folder (default: the folder containing this script, e.g. E:\usb-rat).
.PARAMETER CleanWindows
    Also remove local persistence and running agents on this machine.
.PARAMETER DryRun
    Print planned actions and the report without deleting anything.
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

$results = New-Object System.Collections.Generic.List[object]
function Add-Result([string]$item, [bool]$ok, [string]$note) {
    $results.Add([pscustomobject]@{ Item = $item; Ok = $ok; Note = $note })
}

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
    $name = Split-Path -Leaf $t
    if (-not (Test-Path $t)) { Add-Result $name $true "not present"; continue }
    if ($DryRun) { Add-Result $name $true "would remove"; continue }
    Remove-Item $t -Recurse -Force -ErrorAction SilentlyContinue
    if (Test-Path $t) { Add-Result $name $false "still present (locked?)" }
    else { Add-Result $name $true "removed" }
}

if ($CleanWindows) {
    if ($DryRun) {
        Add-Result "processes winhost/svcupdate/wmpsync" $true "would stop"
        Add-Result "HKCU Run WindowsRuntimeHost" $true "would remove"
        Add-Result "%APPDATA%\Microsoft\WindowsRuntime" $true "would remove"
        Add-Result "scheduled tasks WindowsMediaSync*" $true "would unregister"
        Add-Result "%LOCALAPPDATA%\WMProcess" $true "would remove"
    } else {
        Stop-Process -Name winhost,svcupdate,wmpsync -Force -ErrorAction SilentlyContinue
        Remove-ItemProperty "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name "WindowsRuntimeHost" -ErrorAction SilentlyContinue
        Remove-Item "$env:APPDATA\Microsoft\WindowsRuntime" -Recurse -Force -ErrorAction SilentlyContinue
        if (Get-Command Get-ScheduledTask -ErrorAction SilentlyContinue) {
            Get-ScheduledTask -ErrorAction SilentlyContinue |
                Where-Object { $_.TaskName -like "WindowsMediaSync*" } |
                Unregister-ScheduledTask -Confirm:$false -ErrorAction SilentlyContinue
        }
        Remove-Item "$env:LOCALAPPDATA\WMProcess" -Recurse -Force -ErrorAction SilentlyContinue

        # Verify the Windows side too.
        $procs = @(Get-Process -Name winhost,svcupdate,wmpsync -ErrorAction SilentlyContinue)
        Add-Result "processes winhost/svcupdate/wmpsync" ($procs.Count -eq 0) "count=$($procs.Count)"
        $run = Get-ItemProperty "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name "WindowsRuntimeHost" -ErrorAction SilentlyContinue
        Add-Result "HKCU Run WindowsRuntimeHost" ($null -eq $run) $(if ($null -eq $run) { "absent" } else { "present" })
        Add-Result "%APPDATA%\Microsoft\WindowsRuntime" (-not (Test-Path "$env:APPDATA\Microsoft\WindowsRuntime")) $(if (Test-Path "$env:APPDATA\Microsoft\WindowsRuntime") { "present" } else { "absent" })
        $tasks = @()
        if (Get-Command Get-ScheduledTask -ErrorAction SilentlyContinue) {
            $tasks = @(Get-ScheduledTask -ErrorAction SilentlyContinue | Where-Object { $_.TaskName -like "WindowsMediaSync*" })
        }
        Add-Result "scheduled tasks WindowsMediaSync*" ($tasks.Count -eq 0) "count=$($tasks.Count)"
        Add-Result "%LOCALAPPDATA%\WMProcess" (-not (Test-Path "$env:LOCALAPPDATA\WMProcess")) $(if (Test-Path "$env:LOCALAPPDATA\WMProcess") { "present" } else { "absent" })
    }
}

if (-not $DryRun -and (Test-Path $UsbPath)) {
    $left = @(Get-ChildItem -Force $UsbPath -ErrorAction SilentlyContinue)
    if ($left.Count -eq 0) {
        Remove-Item $UsbPath -Force -ErrorAction SilentlyContinue
        if (-not (Test-Path $UsbPath)) { Add-Result "payload folder" $true "removed" }
        else { Add-Result "payload folder" $false "still present" }
    } else {
        Add-Result "payload folder" $false ("not empty: " + $left.Count + " item(s)")
        foreach ($l in $left) { Write-Host "  [i] remaining: $($l.Name)" }
    }
}

Write-Host ""
Write-Host "=== verification ==="
foreach ($r in $results) {
    $mark = if ($r.Ok) { "OK  " } else { "FAIL" }
    Write-Host ("  {0}  {1,-42} {2}" -f $mark, $r.Item, $r.Note)
}
$bad = @($results | Where-Object { -not $_.Ok })
Write-Host ""
if ($DryRun) {
    Write-Host "RESULT: DRY-RUN (nothing deleted)"
} elseif ($bad.Count -eq 0) {
    Write-Host "RESULT: CLEAN"
} else {
    Write-Host "RESULT: $($bad.Count) item(s) remain"
    foreach ($b in $bad) { Write-Host "  - $($b.Item): $($b.Note)" }
}
