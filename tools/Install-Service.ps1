# Run manually in an elevated PowerShell. This script does not start the service.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$PackageDirectory,
    [string]$ConfigPath = (Join-Path $env:LOCALAPPDATA 'LTAC\config.json')
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run PowerShell as administrator using the account that paired the TV.'
}
$ownerSid = $identity.User.Value
$package = [IO.Path]::GetFullPath($PackageDirectory)
$config = [IO.Path]::GetFullPath($ConfigPath)
$existing = Get-Service -Name LTACPower -ErrorAction SilentlyContinue
if ($existing -and $existing.Status -ne 'Stopped') {
    throw 'Stop LTACPower before installing/updating. No running service has been changed.'
}
foreach ($name in @('LTAC.exe', 'ltac-cli.exe', 'ltac-service.exe')) {
    if (-not (Test-Path -LiteralPath (Join-Path $package $name) -PathType Leaf)) { throw "Missing package file: $name" }
}

function Protect-Directory([string]$Path, [string]$TrustedRoot, [bool]$WritableByService) {
    $full = [IO.Path]::GetFullPath($Path)
    $root = [IO.Path]::GetFullPath($TrustedRoot).TrimEnd('\')
    if (-not $full.StartsWith($root + '\', [StringComparison]::OrdinalIgnoreCase)) { throw "Path outside trusted root: $full" }
    $cursor = $full
    while ($cursor.Length -gt $root.Length) {
        if (Test-Path -LiteralPath $cursor) {
            if ((Get-Item -LiteralPath $cursor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Reparse point refused: $cursor" }
        }
        $cursor = [IO.Path]::GetDirectoryName($cursor)
    }
    [IO.Directory]::CreateDirectory($full) | Out-Null
    $acl = [Security.AccessControl.DirectorySecurity]::new()
    $acl.SetAccessRuleProtection($true, $false)
    $admins = [Security.Principal.SecurityIdentifier]::new('S-1-5-32-544')
    $acl.SetOwner($admins)
    $inherit = [Security.AccessControl.InheritanceFlags]'ContainerInherit, ObjectInherit'
    $propagation = [Security.AccessControl.PropagationFlags]::None
    $allow = [Security.AccessControl.AccessControlType]::Allow
    foreach ($entry in @(
        @('S-1-5-18', 'FullControl'),
        @('S-1-5-32-544', 'FullControl'),
        @('S-1-5-19', $(if ($WritableByService) { 'Modify' } else { 'ReadAndExecute' })),
        @($ownerSid, 'ReadAndExecute')
    )) {
        $sid = [Security.Principal.SecurityIdentifier]::new($entry[0])
        $rights = [Security.AccessControl.FileSystemRights]$entry[1]
        $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid, $rights, $inherit, $propagation, $allow))
    }
    Set-Acl -LiteralPath $full -AclObject $acl
    return $full
}

$install = Protect-Directory (Join-Path $env:ProgramFiles 'LTAC') $env:ProgramFiles $false
$dataRoot = Protect-Directory (Join-Path $env:ProgramData 'LTAC') $env:ProgramData $true
$data = Protect-Directory (Join-Path $dataRoot 'Service') $env:ProgramData $true

# Refuse unexpected pre-existing objects instead of following file links.
foreach ($target in @((Join-Path $data 'config.json'), (Join-Path $data 'service.json'), (Join-Path $install 'LTAC.exe'), (Join-Path $install 'ltac-cli.exe'), (Join-Path $install 'ltac-service.exe'))) {
    if ((Test-Path -LiteralPath $target) -and ((Get-Item -LiteralPath $target -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw "Linked destination refused: $target" }
}

# Export is offline: validates prior physical verification and re-protects the
# key with machine DPAPI. Failure (including a running desktop lock) aborts.
& (Join-Path $package 'ltac-cli.exe') --config $config service-export --output (Join-Path $data 'config.json')
if ($LASTEXITCODE -ne 0) { throw 'Credential export failed. Close LTAC and use the original pairing account.' }
$settings = @{ owner_sid = $ownerSid; automatic = $true } | ConvertTo-Json
$settingsTemporary = Join-Path $data ([guid]::NewGuid().ToString('N') + '.tmp')
[IO.File]::WriteAllText($settingsTemporary, $settings, [Text.UTF8Encoding]::new($false))
$settingsTarget = Join-Path $data 'service.json'
if (Test-Path -LiteralPath $settingsTarget) { Remove-Item -LiteralPath $settingsTarget -Force }
Move-Item -LiteralPath $settingsTemporary -Destination $settingsTarget
foreach ($name in @('LTAC.exe', 'ltac-cli.exe', 'ltac-service.exe')) {
    $destination = Join-Path $install $name
    $temporary = Join-Path $install ([guid]::NewGuid().ToString('N') + '.tmp')
    Copy-Item -LiteralPath (Join-Path $package $name) -Destination $temporary
    if (Test-Path -LiteralPath $destination) { Remove-Item -LiteralPath $destination -Force }
    Move-Item -LiteralPath $temporary -Destination $destination
}

$sc = Join-Path $env:SystemRoot 'System32\sc.exe'
$binary = '"' + (Join-Path $install 'ltac-service.exe') + '"'
# Structured CIM arguments preserve quotes in paths under Program Files across
# Windows PowerShell 5.1 and newer PowerShell native-argument modes.
$serviceArguments = @{
    PathName = $binary
    StartMode = 'Manual'
    StartName = 'NT AUTHORITY\LocalService'
    StartPassword = ''
    ServiceDependencies = [string[]]@('Tcpip')
}
if ($existing) {
    $instance = Get-CimInstance -ClassName Win32_Service -Filter "Name='LTACPower'"
    $registration = Invoke-CimMethod -InputObject $instance -MethodName Change -Arguments $serviceArguments
} else {
    $serviceArguments.Name = 'LTACPower'
    $serviceArguments.DisplayName = 'LTAC Power'
    $serviceArguments.ServiceType = [byte]16
    $serviceArguments.ErrorControl = [byte]1
    $serviceArguments.DesktopInteract = $false
    $registration = Invoke-CimMethod -ClassName Win32_Service -MethodName Create -Arguments $serviceArguments
}
if ($registration.ReturnValue -ne 0) { throw "Service registration failed: $($registration.ReturnValue)" }
& $sc description LTACPower 'LG TV power control before logon, with bounded preshutdown OFF.'
if ($LASTEXITCODE -ne 0) { throw 'Service description failed.' }
& $sc preshutdown LTACPower 6000
if ($LASTEXITCODE -ne 0) { throw 'Preshutdown configuration failed.' }
& $sc failure LTACPower reset= 86400 actions= restart/1000/restart/5000/restart/15000
if ($LASTEXITCODE -ne 0) { throw 'Failure recovery configuration failed.' }
& $sc failureflag LTACPower 1
if ($LASTEXITCODE -ne 0) { throw 'Failure recovery flag failed.' }
& $sc config LTACPower start= auto
if ($LASTEXITCODE -ne 0) { throw 'Automatic startup configuration failed.' }

# Preserve existing tray autostart intent while replacing legacy executable
# paths. An old controller must not launch beside the new boot service.
foreach ($runKey in @('HKCU:\Software\Microsoft\Windows\CurrentVersion\Run', 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Run')) {
    if (Test-Path -LiteralPath $runKey) {
        foreach ($name in @('LTAC', 'ltac-desktop')) {
            $value = Get-ItemPropertyValue -LiteralPath $runKey -Name $name -ErrorAction SilentlyContinue
            if ($null -ne $value) {
                Set-ItemProperty -LiteralPath $runKey -Name $name -Value ('"' + (Join-Path $install 'LTAC.exe') + '" --background')
            }
        }
    }
}
Write-Output "Installed, not started: LTACPower (LocalService, automatic without delayed start)."
Write-Output "When ready to test: Start-Service LTACPower"
Write-Output "Desktop UI: $(Join-Path $install 'LTAC.exe')"
