# Run manually as administrator after Stop-Service LTACPower.
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Run PowerShell as administrator.' }
$service = Get-Service -Name LTACPower -ErrorAction SilentlyContinue
if (-not $service) { Write-Output 'LTACPower is not installed.'; return }
if ($service.Status -ne 'Stopped') { throw 'Stop LTACPower first. An ordinary service stop leaves the TV in its current state.' }
& (Join-Path $env:SystemRoot 'System32\sc.exe') delete LTACPower
if ($LASTEXITCODE -ne 0) { throw 'Service removal failed.' }
Write-Output 'Service removed. Protected configuration and binaries retained for recovery. Reopen LTAC before using desktop synchronization.'
