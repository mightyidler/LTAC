# Hosted CI only: exercise the actual NSIS installer on a machine with no TVs.
[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$Installer)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:GITHUB_ACTIONS -ne 'true') { throw 'Run this installation lifecycle test only on the disposable GitHub Actions runner.' }
if (Get-Service LTACPower -ErrorAction SilentlyContinue) { throw 'Refusing to replace an existing LTAC service.' }
$data = Join-Path $env:ProgramData 'LTAC\Service'
$userData = Join-Path $env:LOCALAPPDATA 'LTAC'
if ((Test-Path (Join-Path $data 'config.json')) -or (Test-Path (Join-Path $data 'devices.json')) -or (Test-Path (Join-Path $userData 'config.json'))) { throw 'Refusing to test on a configured machine.' }
$install = Join-Path $env:ProgramFiles 'LTAC'
$installerPath = (Resolve-Path -LiteralPath $Installer).Path
function Run-Installer([string]$Path, [string]$Arguments) {
    $process = Start-Process -FilePath $Path -ArgumentList $Arguments -WindowStyle Hidden -PassThru
    if (-not $process.WaitForExit(120000)) { throw "Installer timed out: $Path" }
    if ($process.ExitCode -ne 0) { throw "Installer exited with $($process.ExitCode): $Path" }
}
for ($attempt = 1; $attempt -le 2; $attempt++) {
    Run-Installer $installerPath '/S'
    $service = Get-Service LTACPower
    if ($service.Status -ne 'Running') { throw 'Installed service is not running.' }
    $configuration = Start-Process -FilePath (Join-Path $install 'ltac-service.exe') -ArgumentList '--configure-service' -WindowStyle Hidden -Wait -PassThru
    if ($configuration.ExitCode -ne 0) { throw 'Preshutdown API configuration/readback failed.' }
    $log = Get-Content (Join-Path $data 'installation.log') -Raw
    if (($log -split 'Service is running; preshutdown timeout verified').Count - 1 -ne $attempt) { throw 'The installer did not finish its service setup.' }
    if (Test-Path (Join-Path $data 'devices.json')) {
        $devices = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText((Join-Path $data 'devices.json')))
        if ($devices.Count -ne 0) { throw 'The isolated test unexpectedly contains a TV.' }
    }
    Write-Output "NSIS installation ${attempt}: service running and preshutdown readback passed."
    if ($attempt -eq 1) {
        $app = Start-Process -FilePath (Join-Path $install 'ltac-desktop.exe') -WindowStyle Hidden -PassThru
        try {
            $deadline = (Get-Date).AddSeconds(25)
            $discovered = $false
            while ((Get-Date) -lt $deadline) {
                $journal = Join-Path $userData 'events.jsonl'
                if ((Test-Path $journal) -and (Get-Content $journal -Raw) -match 'desktop_discovery_finished') { $discovered = $true; break }
                if ($app.HasExited) { throw 'Desktop exited before completing first-run discovery.' }
                Start-Sleep -Milliseconds 200
            }
            if (-not $discovered) { throw 'First-run desktop discovery did not complete; inspect the startup operation lock.' }
            Write-Output 'Installed desktop: first-run discovery completed without pairing or TV power commands.'
        } finally {
            if (-not $app.HasExited) { Stop-Process -Id $app.Id -Force }
        }
    }
}
Run-Installer (Join-Path $install 'uninstall.exe') '/S'
$deadline = (Get-Date).AddSeconds(10)
while ((Get-Service LTACPower -ErrorAction SilentlyContinue) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 200 }
if (Get-Service LTACPower -ErrorAction SilentlyContinue) { throw 'Uninstall left the service registered.' }
Write-Output 'NSIS uninstall: service removed successfully.'
