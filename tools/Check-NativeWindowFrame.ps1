# Creates only an isolated test HWND. No LTAC service, pairing or TV commands.
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $workspace 'src-tauri/examples/check_native_frame.manifest'
$binary = Join-Path $workspace 'src-tauri/target/debug/examples/check_native_frame.exe'
$kitRoot = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10/bin'
$manifestTool = Get-ChildItem -LiteralPath $kitRoot -Filter mt.exe -Recurse |
    Where-Object { $_.Directory.Name -eq 'x64' } | Sort-Object FullName -Descending |
    Select-Object -First 1 -ExpandProperty FullName
if (-not $manifestTool) { throw 'Windows SDK x64 Manifest Tool (mt.exe) is required.' }
& cargo build --manifest-path (Join-Path $workspace 'src-tauri/Cargo.toml') --example check_native_frame --locked
if ($LASTEXITCODE) { throw 'Native verification build failed.' }
# Cargo examples do not receive Tauri application resources automatically.
& $manifestTool -manifest $manifest "-outputresource:$binary;#1"
if ($LASTEXITCODE) { throw 'Could not embed the test window manifest.' }
$evidence = Join-Path $workspace 'local'
New-Item -ItemType Directory -Path $evidence -Force | Out-Null
$stdout = Join-Path $evidence 'native-frame.stdout.log'
$stderr = Join-Path $evidence 'native-frame.stderr.log'
$process = Start-Process -FilePath $binary -WorkingDirectory $workspace -WindowStyle Hidden -PassThru -Wait -RedirectStandardOutput $stdout -RedirectStandardError $stderr
Get-Content -LiteralPath $stdout
if ($process.ExitCode) {
    Get-Content -LiteralPath $stderr
    throw "Native frame check failed: $($process.ExitCode)"
}
