<#
.SYNOPSIS
    Run one variant inside the Windows lab VM and record a JSON report.
.DESCRIPTION
    Revert the VM snapshot BEFORE each variant, then run this script inside the
    guest. It records SHA-256, process survival, Defender status and any new
    detections. It does not revert the snapshot itself.
.PARAMETER Path
    Variant to run (e.g. E:\_sys\wmpsvc.dll).
.PARAMETER Arguments
    Optional command line (e.g. '--no-install' or ',Run' for rundll32 is passed
    via -Path to rundll32.exe instead).
.PARAMETER WaitSeconds
    Observation window (default 90 s).
.PARAMETER OutDir
    Report directory (default current).
#>
param(
    [Parameter(Mandatory = $true)][string]$Path,
    [string]$Arguments = "",
    [int]$WaitSeconds = 90,
    [string]$OutDir = "."
)
$ErrorActionPreference = "Continue"
$start = Get-Date
$exe = (Resolve-Path $Path).Path

if ($Arguments -ne "") {
    $proc = Start-Process -FilePath $exe -ArgumentList $Arguments -PassThru -WindowStyle Hidden
} else {
    $proc = Start-Process -FilePath $exe -PassThru -WindowStyle Hidden
}
Start-Sleep -Seconds $WaitSeconds

$alive = $false
if ($proc) { $alive = -not $proc.HasExited }
$dets = @()
try {
    $dets = Get-MpThreatDetection -ErrorAction Stop |
        Where-Object { $_.InitialDetectionTime -ge $start } |
        Select-Object ThreatID, Threat, Resources, ActionSuccess, InitialDetectionTime
} catch {}
$status = $null
try { $status = Get-MpComputerStatus | Select-Object RealTimeProtectionEnabled, IsTamperProtected, AntivirusEnabled } catch {}

$report = [pscustomobject]@{
    timestamp       = $start.ToString("o")
    variant         = (Split-Path $exe -Leaf)
    sha256          = (Get-FileHash $exe -Algorithm SHA256).Hash
    arguments       = $Arguments
    process_alive   = $alive
    pid             = $(if ($proc) { $proc.Id } else { $null })
    defender        = $status
    detections      = $dets
}
$out = Join-Path $OutDir ("result-" + (Split-Path $exe -Leaf) + "-" + $start.ToString("yyyyMMdd-HHmmss") + ".json")
$report | ConvertTo-Json -Depth 6 | Set-Content -Encoding UTF8 $out
Write-Host "[+] report: $out"
if ($dets) { $dets | Format-List }
