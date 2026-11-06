# Real msiexec install/uninstall smoke test of a built Fauna MSI, run by the
# public Release workflow on a disposable GitHub-hosted runner of the MSI's own
# architecture (windows-latest for x64, windows-11-arm for arm64). Never run it
# on a machine you care about: it installs perMachine services, writes HKLM and
# deletes %ProgramData%\Fauna\.
#
# One script for both arches, so the two smokes assert the same things:
#
#   * install succeeds with every feature (ADDLOCAL=ALL: Nest and Bridge are
#     default-off, and their services are what proves the binaries run);
#   * one Add/Remove Programs entry named "Fauna" exists while installed, and
#     none after uninstall;
#   * every native binary installed is built for -Arch (an x64 binary on an
#     arm64 runner would still run, under emulation, so loading alone proves
#     nothing about the arch);
#   * the terminal app's one machine Path entry: a shell reading the machine
#     Path fresh from the registry finds fauna-tui and runs fauna-cli --help,
#     and the uninstall removes both files and the entry;
#   * the FaunaNest and FaunaBridge services reach Running and are still
#     Running a few seconds later (the binaries load and do not crash at
#     start), and are gone after uninstall;
#   * every shell-extension CLSID is registered against an existing DLL and
#     activates through COM in a native process of the runner's arch;
#   * a plain uninstall PRESERVES %ProgramData%\Fauna\, and REMOVE_USER_DATA=1
#     DELETES it (the util:RemoveFolderEx custom action + the DataDir
#     RegistrySearch, which only a real uninstall exercises).
#
# The feature-tree constraints (Sync non-deselectable, Bridge requires Nest)
# are UI-interactive and verified at the MSI-table level, not here.
#
# Usage: pwsh -File ci-smoke.ps1 -Msi Fauna-Setup-arm64.msi -Arch arm64
# msiexec logs land in the working directory as <arch>-*.log.

param(
  [Parameter(Mandatory)] [string] $Msi,
  [Parameter(Mandatory)] [ValidateSet('x64', 'arm64')] [string] $Arch
)

$ErrorActionPreference = 'Stop'
$Msi = (Resolve-Path -LiteralPath $Msi).Path
$data = Join-Path $env:ProgramData 'Fauna'
$installDir = Join-Path $env:ProgramFiles 'Fauna'
$services = @('FaunaNest', 'FaunaBridge')
# IMAGE_FILE_MACHINE_* of the PE header.
$expectedMachine = @{ x64 = 0x8664; arm64 = 0xAA64 }[$Arch]
# The shell extension's CLSIDs share this prefix (ShellExt.wxs).
$clsidPrefix = '{4a7b8c'

function Invoke-Msi {
  param([string]$Op, [string]$Log, [string]$What, [string[]]$Props = @())
  # Properties go LAST on the msiexec command line, after the switches.
  $argv = @($Op, "`"$Msi`"", '/qn', '/norestart', '/l*v', "$Arch-$Log") + $Props
  Write-Host "==> msiexec $($argv -join ' ')"
  $p = Start-Process -FilePath 'msiexec.exe' -ArgumentList $argv -Wait -PassThru
  # 0 = success; 3010 = success, reboot required (services).
  if ($p.ExitCode -ne 0 -and $p.ExitCode -ne 3010) {
    Write-Host "----- $Arch-$Log (tail) -----"
    if (Test-Path "$Arch-$Log") { Get-Content "$Arch-$Log" -Tail 100 }
    throw "$What FAILED: msiexec exit $($p.ExitCode)"
  }
  Write-Host "$What OK (exit $($p.ExitCode))"
}

function Get-ArpEntries {
  $roots = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*',
           'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*'
  @(Get-ItemProperty -Path $roots -ErrorAction SilentlyContinue |
    Where-Object { $_.PSObject.Properties['DisplayName'] -and $_.DisplayName -eq 'Fauna' })
}

function Get-PeMachine([string]$Path) {
  $bytes = [System.IO.File]::ReadAllBytes($Path)
  $peOffset = [BitConverter]::ToInt32($bytes, 0x3C)
  [BitConverter]::ToUInt16($bytes, $peOffset + 4)
}

# The machine Path the way a NEW shell reads it: from the registry, expanded.
# Never $env:Path, which was fixed when this process started, before the
# install wrote (or the uninstall removed) anything.
function Get-MachinePath { [Environment]::GetEnvironmentVariable('Path', 'Machine') }

function Test-PathHolds([string]$PathValue, [string]$Dir) {
  $want = $Dir.TrimEnd('\')
  [bool]@($PathValue -split ';' | Where-Object { $_ -and $_.TrimEnd('\') -ieq $want }).Count
}

# Run $Command in cmd.exe with PATH set to $PathValue, as a shell opened after
# the install would; returns its output lines, $LASTEXITCODE is its exit code.
function Invoke-FreshShell([string]$PathValue, [string]$Command) {
  $saved = $env:Path
  try {
    $env:Path = $PathValue
    & (Join-Path $env:SystemRoot 'System32\cmd.exe') /d /c "$Command 2>&1"
  } finally { $env:Path = $saved }
}

# The TerminalApp feature (TerminalApp.wxs): its one machine Path entry makes
# fauna-tui and fauna-cli typeable in any new shell. fauna-tui is only found,
# never run (it has no headless invocation); fauna-cli --help needs no host,
# account or store.
function Assert-TerminalAppOnPath {
  $machinePath = Get-MachinePath
  if (-not (Test-PathHolds $machinePath $installDir)) { throw "the machine Path does not hold $installDir after install" }
  $where = @(Invoke-FreshShell $machinePath 'where fauna-tui')
  if ($LASTEXITCODE -ne 0 -or -not $where -or (Split-Path -Parent $where[0]).TrimEnd('\') -ine $installDir.TrimEnd('\')) {
    throw "a fresh shell does not find fauna-tui in ${installDir}: $($where -join ' | ')"
  }
  $help = (Invoke-FreshShell $machinePath 'fauna-cli --help') -join "`n"
  if ($LASTEXITCODE -ne 0 -or $help -notmatch 'commands:') {
    throw "fauna-cli --help in a fresh shell failed (exit $LASTEXITCODE):`n$help"
  }
  Write-Host "Terminal app OK: $installDir on the machine Path; fauna-tui found, fauna-cli --help runs"
}

function Get-ShellClsids {
  @(Get-ChildItem 'HKLM:\SOFTWARE\Classes\CLSID' |
    Where-Object { $_.PSChildName.StartsWith($clsidPrefix, 'OrdinalIgnoreCase') } |
    ForEach-Object { $_.PSChildName })
}

function Assert-Installed {
  # Add/Remove Programs.
  $arp = Get-ArpEntries
  if ($arp.Count -ne 1) { throw "expected exactly one 'Fauna' Add/Remove Programs entry, found $($arp.Count)" }
  Write-Host "ARP entry OK: Fauna $($arp[0].DisplayVersion)"

  # Every native binary is built for the MSI's own arch. App\ also holds the
  # self-contained .NET runtime's IL assemblies (AnyCPU, PE machine i386), so
  # only its apphost is checked there.
  $natives = @(Get-ChildItem -Path (Join-Path $installDir '*') -File -Include '*.exe', '*.dll') +
             @(Get-Item -LiteralPath (Join-Path $installDir 'App\FaunaApp.exe'))
  foreach ($required in 'fauna-sync-agent.exe', 'fauna-tui.exe', 'fauna-cli.exe', 'fauna-nest-svc.exe',
                        'fauna-bridge-svc.exe', 'fauna-mail-bridge.exe') {
    if (-not ($natives | Where-Object Name -eq $required)) { throw "$required is not installed under $installDir" }
  }
  foreach ($f in $natives) {
    $m = Get-PeMachine $f.FullName
    if ($m -ne $expectedMachine) {
      throw ("{0} is PE machine 0x{1:X4}, expected 0x{2:X4} ({3})" -f $f.FullName, $m, $expectedMachine, $Arch)
    }
  }
  Write-Host "Arch OK: $($natives.Count) native binaries are $Arch"

  Assert-TerminalAppOnPath

  # The machine services start and stay up.
  foreach ($name in $services) {
    $svc = Get-Service -Name $name
    try { $svc.WaitForStatus('Running', [TimeSpan]::FromSeconds(60)) }
    catch { throw "service $name did not reach Running within 60 s (status $($svc.Status))" }
  }
  Start-Sleep -Seconds 10
  foreach ($name in $services) {
    $status = (Get-Service -Name $name).Status
    if ($status -ne 'Running') { throw "service $name left Running after start (status $status)" }
  }
  Write-Host "Services OK: $($services -join ', ') Running"

  # The shell extension registers and activates. Activation runs in a CHILD
  # pwsh (the same native arch as this one) that exits afterwards, so the DLL
  # is not held loaded into the uninstall that follows.
  $clsids = Get-ShellClsids
  if ($clsids.Count -ne 8) { throw "expected 8 shell-extension CLSIDs, found $($clsids.Count): $($clsids -join ', ')" }
  foreach ($c in $clsids) {
    $dll = (Get-ItemProperty -LiteralPath "HKLM:\SOFTWARE\Classes\CLSID\$c\InprocServer32").'(default)'
    if (-not $dll -or -not (Test-Path -LiteralPath $dll)) { throw "CLSID $c InprocServer32 names a missing DLL: '$dll'" }
  }
  $activate = {
    param([string[]]$Ids)
    $ErrorActionPreference = 'Stop'
    foreach ($id in $Ids) {
      $obj = [Activator]::CreateInstance([Type]::GetTypeFromCLSID([Guid]$id, $true))
      [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($obj)
      Write-Host "  activated $id"
    }
  }
  & (Join-Path $PSHOME 'pwsh.exe') -NoProfile -NonInteractive -Command $activate -args (, $clsids)
  if ($LASTEXITCODE -ne 0) { throw "shell-extension COM activation FAILED (exit $LASTEXITCODE)" }
  Write-Host "Shell extension OK: $($clsids.Count) CLSIDs activate ($env:PROCESSOR_ARCHITECTURE)"
}

function Assert-Uninstalled {
  $arp = Get-ArpEntries
  if ($arp.Count -ne 0) { throw "uninstall left $($arp.Count) 'Fauna' Add/Remove Programs entries behind" }
  foreach ($name in $services) {
    if (Get-Service -Name $name -ErrorAction SilentlyContinue) { throw "uninstall left service $name behind" }
  }
  $left = Get-ShellClsids
  if ($left.Count -ne 0) { throw "uninstall left shell-extension CLSIDs behind: $($left -join ', ')" }
  foreach ($f in 'fauna-tui.exe', 'fauna-cli.exe') {
    if (Test-Path -LiteralPath (Join-Path $installDir $f)) { throw "uninstall left $f behind in $installDir" }
  }
  if (Test-PathHolds (Get-MachinePath) $installDir) { throw "uninstall left $installDir on the machine Path" }
}

# Without this, the uninstall half's Path check could not tell removal from an
# entry that was there all along.
if (Test-PathHolds (Get-MachinePath) $installDir) { throw "the machine Path holds $installDir BEFORE the install" }

# Case 1 — full install passes every runtime check; a plain uninstall PRESERVES
# %ProgramData%\Fauna\.
Invoke-Msi -Op '/i' -Log 'install-1.log' -What 'install (case 1)' -Props @('ADDLOCAL=ALL')
Assert-Installed
New-Item -ItemType Directory -Force -Path $data | Out-Null
Set-Content -LiteralPath (Join-Path $data 'user-data.sentinel') -Value 'preserve me'
Invoke-Msi -Op '/x' -Log 'uninstall-1.log' -What 'uninstall (case 1, no flag)'
Assert-Uninstalled
if (-not (Test-Path $data)) {
  throw "REGRESSION: a plain uninstall deleted $data; it must be preserved without REMOVE_USER_DATA=1"
}
Write-Host "Case 1 PASS: plain uninstall preserved $data"

# Case 2 — REMOVE_USER_DATA=1 DELETES %ProgramData%\Fauna\.
Invoke-Msi -Op '/i' -Log 'install-2.log' -What 'install (case 2)' -Props @('ADDLOCAL=ALL')
New-Item -ItemType Directory -Force -Path $data | Out-Null
Set-Content -LiteralPath (Join-Path $data 'user-data.sentinel') -Value 'delete me'
Invoke-Msi -Op '/x' -Log 'uninstall-2.log' -What 'uninstall (case 2, REMOVE_USER_DATA=1)' -Props @('REMOVE_USER_DATA=1')
Assert-Uninstalled
if (Test-Path $data) {
  Write-Host "Still present under $data :"
  Get-ChildItem -Recurse -Force $data | ForEach-Object { Write-Host "  $($_.FullName)" }
  throw "REGRESSION: REMOVE_USER_DATA=1 left $data behind; it must be deleted"
}
Write-Host "Case 2 PASS: REMOVE_USER_DATA=1 deleted $data"
Write-Host "Installer install/uninstall smoke test PASSED ($Arch)."
