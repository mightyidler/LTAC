# Invoked by the elevated NSIS installer. Never reads the user's pairing key.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][ValidateSet('Stop', 'Install', 'Remove')][string]$Action,
    [Parameter(Mandatory = $true)][string]$InstallDirectory
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$script:installationLog = $null
trap {
    $failure = "$(Get-Date -Format o) $Action failed: $($_.Exception.Message)`r`n$($_.ScriptStackTrace)"
    if ($script:installationLog) { [IO.File]::AppendAllText($script:installationLog, $failure + "`r`n", [Text.UTF8Encoding]::new($false)) }
    Write-Error $failure -ErrorAction Continue
    exit 1
}
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Administrator installation is required.' }
$install = [IO.Path]::GetFullPath($InstallDirectory)
$service = Get-Service -Name LTACPower -ErrorAction SilentlyContinue
if ($service -and $service.Status -ne 'Stopped') {
    Stop-Service -Name LTACPower -ErrorAction Stop
    $service.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(10))
}
if ($Action -eq 'Stop') { exit 0 }
$sc = Join-Path $env:SystemRoot 'System32\sc.exe'
if ($Action -eq 'Remove') {
    if ($service) { & $sc delete LTACPower; if ($LASTEXITCODE -ne 0) { throw 'Cannot remove LTACPower.' } }
    # Retain protected pairing data for reinstalls. No TV power command is sent.
    exit 0
}
$nativeProgramFiles = if ($env:ProgramW6432) { $env:ProgramW6432 } else { $env:ProgramFiles }
$programRoot = [IO.Path]::GetFullPath($nativeProgramFiles).TrimEnd('\')
if (-not $install.StartsWith($programRoot + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Install LTAC inside Program Files.' }
$binary = Join-Path $install 'ltac-service.exe'
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) { throw 'The service binary is missing.' }

$data = Join-Path $env:ProgramData 'LTAC\Service'
$settingsPath = Join-Path $data 'service.json'
$ownerSid = $identity.User.Value
if (Test-Path -LiteralPath $settingsPath) {
    $previous = Get-Content -LiteralPath $settingsPath -Raw | ConvertFrom-Json
    $ownerSid = $previous.owner_sid
}
# Validate before putting a SID in the pipe's security descriptor.
[Security.Principal.SecurityIdentifier]::new($ownerSid) | Out-Null
foreach ($folder in @((Join-Path $env:ProgramData 'LTAC'), $data)) {
    if ((Test-Path -LiteralPath $folder) -and ((Get-Item -LiteralPath $folder -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Linked service data directory refused.' }
    [IO.Directory]::CreateDirectory($folder) | Out-Null
    $acl = [Security.AccessControl.DirectorySecurity]::new()
    $acl.SetAccessRuleProtection($true, $false)
    $acl.SetOwner([Security.Principal.SecurityIdentifier]::new('S-1-5-32-544'))
    foreach ($entry in @(@('S-1-5-18','FullControl'), @('S-1-5-32-544','FullControl'), @('S-1-5-19','Modify'), @($ownerSid,'ReadAndExecute'))) {
        $sid = [Security.Principal.SecurityIdentifier]::new($entry[0])
        $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid, [Security.AccessControl.FileSystemRights]$entry[1], [Security.AccessControl.InheritanceFlags]'ContainerInherit, ObjectInherit', [Security.AccessControl.PropagationFlags]::None, [Security.AccessControl.AccessControlType]::Allow))
    }
    Set-Acl -LiteralPath $folder -AclObject $acl
}
$script:installationLog = Join-Path $data 'installation.log'
if ((Test-Path -LiteralPath $script:installationLog) -and ((Get-Item -LiteralPath $script:installationLog -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) { $script:installationLog = $null; throw 'Linked installation log refused.' }
[IO.File]::AppendAllText($script:installationLog, "$(Get-Date -Format o) Installing service from $install`r`n", [Text.UTF8Encoding]::new($false))
if (-not (Test-Path -LiteralPath $settingsPath)) {
    $settings = @{ owner_sid = $ownerSid; automatic = $true } | ConvertTo-Json
    [IO.File]::WriteAllText($settingsPath, $settings, [Text.UTF8Encoding]::new($false))
}
$arguments = @{
    DisplayName = 'LTAC';
    PathName = ('"' + $binary + '"'); StartMode = 'Automatic';
    StartName = 'NT AUTHORITY\LocalService'; StartPassword = '';
    ServiceDependencies = [string[]]@('Tcpip')
}
if ($service) {
    $instance = Get-CimInstance -ClassName Win32_Service -Filter "Name='LTACPower'"
    $result = Invoke-CimMethod -InputObject $instance -MethodName Change -Arguments $arguments
} else {
    $arguments.Name = 'LTACPower'
    $arguments.ServiceType = [byte]16; $arguments.ErrorControl = [byte]1; $arguments.DesktopInteract = $false
    $result = Invoke-CimMethod -ClassName Win32_Service -MethodName Create -Arguments $arguments
}
if ($result.ReturnValue -ne 0) { throw "Service registration failed: $($result.ReturnValue)" }
$configuration = Start-Process -FilePath $binary -ArgumentList '--configure-service' -WindowStyle Hidden -Wait -PassThru
if ($configuration.ExitCode -ne 0) { throw 'Native preshutdown timeout configuration or readback failed.' }
foreach ($command in @(
    @('failure','LTACPower','reset=','86400','actions=','restart/1000/restart/5000/restart/15000'),
    @('failureflag','LTACPower','1')
)) {
    & $sc @command
    if ($LASTEXITCODE -ne 0) { throw 'Service recovery configuration failed.' }
}
# Installing/updating is not a PC boot and must not wake an unattended TV.
[IO.File]::WriteAllText((Join-Path $data 'skip-boot-wake'), '', [Text.UTF8Encoding]::new($false))
Start-Service -Name LTACPower
(Get-Service -Name LTACPower).WaitForStatus('Running', [TimeSpan]::FromSeconds(10))
[IO.File]::AppendAllText($script:installationLog, "$(Get-Date -Format o) Service is running; preshutdown timeout verified at 6000 ms.`r`n", [Text.UTF8Encoding]::new($false))
exit 0
