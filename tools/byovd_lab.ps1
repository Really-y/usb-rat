<#
.SYNOPSIS
    BYOVD lab workflow: run an operator-supplied tool/driver and capture Defender state.
.DESCRIPTION
    Requires an elevated PowerShell. This script does NOT download or bundle a
    vulnerable driver; supply your own tool (Terminator, EDRSandblast, KDU, ...).
    Revert the VM snapshot after every run. HVCI, the Microsoft driver blocklist
    and PatchGuard can prevent or destabilize BYOVD. Lab VMs only.
.PARAMETER Tool
    Path to the operator-supplied BYOVD tool/executable.
.PARAMETER ToolArgs
    Arguments passed to the tool.
.PARAMETER OutDir
    Directory for the JSON report (default current).
.PARAMETER DryRun
    Print the planned command and exit without executing.
#>
param(
    [Parameter(Mandatory = $true)][string]$Tool,
    [string]$ToolArgs = "",
    [string]$OutDir = ".",
    [switch]$DryRun
)
$ErrorActionPreference = "Stop"

$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "byovd_lab.ps1 requires an elevated PowerShell (Run as Administrator)."
}
if (-not (Test-Path $Tool)) { throw "tool not found: $Tool" }

Write-Host "==============================================================" -ForegroundColor Yellow
Write-Host " BYOVD LAB ONLY - revert the VM snapshot after this run." -ForegroundColor Yellow
Write-Host " HVCI / driver blocklist / PatchGuard may block or crash." -ForegroundColor Yellow
Write-Host "==============================================================" -ForegroundColor Yellow

function Get-DefenderState {
    $state = [ordered]@{}
    try { $state.status = Get-MpComputerStatus | Select-Object RealTimeProtectionEnabled, IsTamperProtected, AntivirusEnabled } catch { $state.status = $null }
    try { $state.msmpeng = @(Get-Process MsMpEng -ErrorAction Stop | Select-Object Id, ProcessName, StartTime) } catch { $state.msmpeng = @() }
    return $state
}

$start = Get-Date
$pre = Get-DefenderState
$commandLine = if ($ToolArgs -ne "") { "$Tool $ToolArgs" } else { $Tool }
if ($DryRun) {
    Write-Host "[dry-run] would execute: $commandLine"
    return
}

$proc = $null
if ($ToolArgs -ne "") {
    $proc = Start-Process -FilePath $Tool -ArgumentList $ToolArgs -Wait -PassThru
} else {
    $proc = Start-Process -FilePath $Tool -Wait -PassThru
}
$post = Get-DefenderState
$dets = @()
try { $dets = Get-MpThreatDetection -ErrorAction Stop | Sort-Object InitialDetectionTime -Descending | Select-Object -First 10 } catch {}
$threats = @()
try { $threats = Get-MpThreat -ErrorAction Stop | Select-Object -First 10 } catch {}

$report = [pscustomobject]@{
    timestamp  = $start.ToString("o")
    tool       = $Tool
    arguments  = $ToolArgs
    exit_code  = $(if ($proc) { $proc.ExitCode } else { $null })
    pre        = $pre
    post       = $post
    detections = $dets
    threats    = $threats
}
$out = Join-Path $OutDir ("byovd-" + $start.ToString("yyyyMMdd-HHmmss") + ".json")
$report | ConvertTo-Json -Depth 8 | Set-Content -Encoding UTF8 $out
Write-Host "[+] report: $out"
Write-Host "[+] pre MsMpEng: $($pre.msmpeng.Count)  post MsMpEng: $($post.msmpeng.Count)"
Write-Host "[!] Revert the VM snapshot now."
