# Fauna Windows Installer

WiX v6 MSI installer for Fauna for Windows.

## Prerequisites

On the Windows build machine:

```powershell
# Install WiX (v6) .NET tool
dotnet tool install --global wix

# Install the WiX extensions used by the .wxs source:
#   Util     — RemoveFolderEx + ServiceConfig (failure recovery)
#   UI       — WixUI_FeatureTree (feature-selection dialog)
#   Firewall — FirewallException (inbound :443 allow rule for the nest)
wix extension add --global WixToolset.Util.wixext
wix extension add --global WixToolset.UI.wixext
wix extension add --global WixToolset.Firewall.wixext
```

## Building from a clean checkout — required steps

A clean checkout hits several gotchas (all surfaced by the first real end-to-end build,
2026-06-15 — prior "exit 0" claims were `cargo-win.cmd` false successes):

1. ~~**Seed the workspace lock.**~~ **No longer needed (2026-07-22).** The service
   crates were a separate `apps/fauna-windows` Cargo workspace whose `Cargo.lock` was
   gitignored, so a fresh resolve rejected the **yanked `core2 0.4.0`** (via
   `fauna-cbor`) unless you first seeded it from the root lock. That workspace is now
   unified into the root one — the crates build from the repo root against the single
   tracked `Cargo.lock`, and `--locked` works. Nothing to seed.
2. **Generate the UniFFI bindings + native dll first.** The WinUI app P/Invokes
   `fauna_ffi.dll` and `FaunaApp.Core` consumes the generated `Generated/uniffi/*.cs`
   (both gitignored). Run `just windows-ffi` **before** the app build or `FaunaApp.Core`
   won't compile. (NB: that recipe is hardcoded to `aarch64` + `cargo-win.cmd`; a CI x64
   leg needs it arch-parameterized.)
3. **On win-arm64, publish the app with MSBuild, not `dotnet`** — `dotnet build/publish`
   crashes the XAML compiler under x86 emulation. Use
   `MSBuild.exe …/FaunaApp.csproj -restore -t:Publish -p:Configuration=Release
   -p:RuntimeIdentifier=win-arm64`.
4. **Publish self-contained** so the MSI runs on a bare VM with no .NET 10 / Windows App
   SDK runtime install: add `-p:SelfContained=true -p:WindowsAppSDKSelfContained=true`.
5. **`wix build` `-d` paths must be ABSOLUTE.** WiX resolves `App.wxs`'s
   `<Files Include="$(var.AppDir)**">` harvest relative to the **.wxs file's directory**,
   not the working dir — relative paths silently harvest **zero** app files (the MSI
   builds "successfully" but ships no app). Pass absolute paths for every `-d` var.
   (From **Git Bash** on Windows, `$PWD` is the MSYS form `/c/...` which the native
   `wix.exe` rejects — use `$(pwd -W)` for the `C:/...` Windows-form absolute paths.)
6. **Build the Go mail-bridge MDA — the Bridge feature now requires it.** Since the
   Track-2 mail-bridge wiring (`886866383`), `ServiceBridge.wxs` references
   `$(var.MailBridgeBin)` / `$(var.MailBridgeFfiDll)` / `$(var.MailBridgeLibunwindDll)`,
   so `wix build` fails without all three. Run `just windows-mail-bridge-build` (defaults
   arm64; `… x86_64-pc-windows-gnullvm amd64` for x64) → stages
   `target/fauna-mail-bridge.exe` + a **separate gnullvm** `target/fauna_ffi.dll` +
   `target/libunwind.dll`. Needs Go + `llvm-mingw`. The Go MDA's gnullvm `fauna_ffi.dll` ships beside
   `fauna-bridge-svc.exe` in `INSTALLFOLDER`; the C# app's own MSVC `fauna_ffi.dll` lands
   under `app/` — same name, **different MSI dir**, never collide.
7. **Build shipped native binaries with the `dist` cargo profile (size).** Pass
   `--profile dist` in place of `--release` for everything that ships: `just windows-ffi
   dist`, `cargo build --locked --profile dist -p …`
   for the 4 shipped service crates, and `just windows-mail-bridge-build <rust_target> <go_arch>
   dist` (which also links the Go MDA with `-ldflags="-s -w"`). `dist` (`strip` + thin-LTO
   + `opt-level="s"`) trims the MSI ~21% — per-binary table in
   `docs/goal/architecture/installers/windows.md` § *Size & build profile*. Then **stage
   from the `…/dist/` output dir**, not `…/release/`. Dev/e2e builds keep `--release` (the
   recipes default `profile=release`, so a bare `just windows-ffi` is unchanged). ⚠ `just`
   takes recipe args **positionally** — it is `just windows-ffi dist`, **not** `just
   windows-ffi profile=dist` (the `name=value` form makes `profile` the literal string
   `profile=dist`, which `cargo --profile` then rejects).

**Static CRT (automatic, do not remove).** The repo-root `.cargo/config.toml` sets
`-C target-feature=+crt-static` for `*-pc-windows-msvc`, so the service exes,
`fauna_shell.dll`, and `fauna_ffi.dll` statically link the MSVC runtime and carry **no
`VCRUNTIME140.dll` dependency**. This is required: the installer's target is a bare VM with
no Visual C++ Redistributable, where a dynamic-CRT service fails to start (MSI error 1920).
Verify with `dumpbin /dependents fauna-sync-agent.exe` — there must be no `VCRUNTIME140.dll`.

## Build

### 1. Stage binaries

Build each component and copy to a staging directory:

All commands below run from the **repo root**. The Windows service crates under
`apps/fauna-windows/` are **root-workspace members** (the nested cargo workspace was
unified away 2026-07-22), so they build into the top-level `target/` and `--locked`
resolves against the one committed `Cargo.lock`. The sync agent is the cross-platform
`fauna-sync-agent` package (`bins/fauna-sync-agent`) itself. Keep each `-p` in its **own**
cargo invocation, as below: naming several packages in one invocation unifies their
features, and the shipped agent must carry only the feature set it asks for.

```powershell
# Rust binaries — the sync agent + the Windows service crates
cargo build --locked --release -p fauna-sync-agent     --target x86_64-pc-windows-msvc
cargo build --locked --release -p fauna-nest-service   --target x86_64-pc-windows-msvc
cargo build --locked --release -p fauna-bridge-service --target x86_64-pc-windows-msvc
cargo build --locked --release -p fauna-shell-ext      --target x86_64-pc-windows-msvc

# .NET app
dotnet publish apps/fauna-windows/FaunaApp/FaunaApp/FaunaApp.csproj -c Release -r win-x64

# Go mail-bridge MDA (cgo, gnullvm) — see clean-checkout step 6. amd64 leg:
just windows-mail-bridge-build x86_64-pc-windows-gnullvm amd64

# Stage. NOTE: bin names are set by each crate's [[bin]] (fauna-sync / fauna-nest-svc /
# fauna-bridge-svc); the shell-ext crate overrides [lib] name = "fauna_shell",
# so the cdylib emits fauna_shell.dll DIRECTLY — stage it under the bare name (no rename).
# The MSI installs it under a VERSION-STAMPED @Name (fauna_shell_<ver>.dll) set in
# ShellExt.wxs ($(var.ShellDllName)); -d ShellDll= still points at the bare staged source,
# so staging is unchanged. (Path 1 side-by-side upgrade — installers/windows.md
# § Shell Extension § Upgrade handling. Bump $(var.ShellDllName) + Package/@Version
# together when the shell DLL genuinely changes.)
mkdir -p stage/x64/icons stage/x64/app
$rel = "target/x86_64-pc-windows-msvc/release"
cp "$rel/fauna-sync-agent.exe"        stage/x64/
cp "$rel/fauna-nest-svc.exe"    stage/x64/
cp "$rel/fauna-bridge-svc.exe"  stage/x64/
cp "$rel/fauna_shell.dll"       stage/x64/fauna_shell.dll
# Go MDA + its gnullvm runtime DLLs. These land in the target/ ROOT (no --target
# triple subdir), NOT in $rel. Ship beside fauna-bridge-svc.exe; the app's MSVC
# fauna_ffi.dll lands under app/ (different MSI dir) — same name, never collide.
cp target/fauna-mail-bridge.exe stage/x64/
cp target/fauna_ffi.dll         stage/x64/
cp target/libunwind.dll         stage/x64/
cp apps/fauna-windows/shell-ext/src/icons/*.ico stage/x64/icons/
cp apps/fauna-windows/FaunaApp/FaunaApp/Assets/AppIcon.ico stage/x64/icons/
cp -r apps/fauna-windows/FaunaApp/FaunaApp/bin/Release/net10.0-windows*/win-x64/publish/* stage/x64/app/
```

For ARM64, replace `x86_64-pc-windows-msvc` with `aarch64-pc-windows-msvc`
and `win-x64` with `win-arm64`.

**GUI-subsystem (no console window at logon).** The shipped `fauna-sync-agent.exe` is
launched per-user at logon via an HKLM `…\Run` key (OneDrive model), so it MUST be a
GUI-subsystem (PE subsystem **2**) binary — a console-subsystem (3) build flashes a
terminal at logon (the 2026-06-23 stale-binary reship). A `dist`/`release` build is
subsystem 2 (the `#![windows_subsystem = "windows"]` flip in
`bins/fauna-sync-agent/src/main.rs`, active when `debug_assertions` is off); a stale
pre-flip artifact re-staged here is subsystem 3. Verify the **staged** binary before
building the MSI (and the **installed** one after install) — the file-count / service
/ firewall checks all pass on a console build, so this is the only thing that catches
a stale re-stage:

```powershell
$p = "$PWD\stage\arm64\fauna-sync-agent.exe"   # and 'C:\Program Files\Fauna\fauna-sync-agent.exe' after install
$b = [IO.File]::ReadAllBytes($p); $o = [BitConverter]::ToInt32($b, 0x3C)
[BitConverter]::ToUInt16($b, $o + 92)    # MUST be 2 (GUI). 3 = stale console build → terminal at logon.
```

The tier_3 `test_installer_structure.py` (`TestSyncAgentSubsystem`) gates the staged
binary on this automatically.

### 2. Build the sparse package

The **ShellExt** feature ships `Fauna-Sparse.msix` as a component (`ShellExt.wxs`'s
`$(var.SparsePackage)`), built once and shared by every arch (it is
`ProcessorArchitecture="neutral"` — § *Menu placement — the sparse package* in
`docs/goal/architecture/installers/windows.md`; there is nothing to substitute per arch,
only the version-stamped DLL name, which the manifest template already carries):

This is built by a dedicated dev-fleet packaging script (internal tooling, not
included in this public tree) into `build/installer/Fauna-Sparse.msix` (dev
cert; a self-sign-and-trust option is also available).

### 3. Build MSI

The `-ext` flags load the globally-installed WiX extensions (`wix extension add --global
WixToolset.Util.wixext WixToolset.UI.wixext WixToolset.Firewall.wixext` once, beforehand).
They are required when building from the repo root (where `wix.json` is not auto-discovered).

The `-d` paths MUST be absolute (clean-checkout gotcha 5): WiX harvests
`App.wxs`'s `<Files Include="$(var.AppDir)**">` relative to the **.wxs file's**
directory, so a relative `AppDir` silently harvests zero app files and ships an
app-less MSI. Running from the repo root, `$PWD` anchors them absolutely (the CI
equivalent is `${{ github.workspace }}`). `SparsePackage` points at the ONE package
step 2 built — same path for both arches.

```powershell
# x64
wix build -arch x64 -ext WixToolset.Util.wixext -ext WixToolset.UI.wixext `
  -ext WixToolset.Firewall.wixext `
  -o Fauna-Setup-x64.msi apps/fauna-windows/installer/*.wxs `
  -d SyncBin=$PWD/stage/x64/fauna-sync-agent.exe `
  -d TuiBin=$PWD/stage/x64/fauna-tui.exe `
  -d NestBin=$PWD/stage/x64/fauna-nest-svc.exe `
  -d BridgeBin=$PWD/stage/x64/fauna-bridge-svc.exe `
  -d MailBridgeBin=$PWD/stage/x64/fauna-mail-bridge.exe `
  -d MailBridgeFfiDll=$PWD/stage/x64/fauna_ffi.dll `
  -d MailBridgeLibunwindDll=$PWD/stage/x64/libunwind.dll `
  -d ShellDll=$PWD/stage/x64/fauna_shell.dll `
  -d IconDir=$PWD/stage/x64/icons/ `
  -d AppDir=$PWD/stage/x64/app/ `
  -d SparsePackage=$PWD/build/installer/Fauna-Sparse.msix

# ARM64 (from Git Bash on Windows, replace $PWD with $(pwd -W) — clean-checkout step 5)
wix build -arch arm64 -ext WixToolset.Util.wixext -ext WixToolset.UI.wixext `
  -ext WixToolset.Firewall.wixext `
  -o Fauna-Setup-arm64.msi apps/fauna-windows/installer/*.wxs `
  -d SyncBin=$PWD/stage/arm64/fauna-sync-agent.exe `
  -d TuiBin=$PWD/stage/arm64/fauna-tui.exe `
  -d NestBin=$PWD/stage/arm64/fauna-nest-svc.exe `
  -d BridgeBin=$PWD/stage/arm64/fauna-bridge-svc.exe `
  -d MailBridgeBin=$PWD/stage/arm64/fauna-mail-bridge.exe `
  -d MailBridgeFfiDll=$PWD/stage/arm64/fauna_ffi.dll `
  -d MailBridgeLibunwindDll=$PWD/stage/arm64/libunwind.dll `
  -d ShellDll=$PWD/stage/arm64/fauna_shell.dll `
  -d IconDir=$PWD/stage/arm64/icons/ `
  -d AppDir=$PWD/stage/arm64/app/ `
  -d SparsePackage=$PWD/build/installer/Fauna-Sparse.msix
```

### 4. Test

```powershell
# Install (full app, auto-start services)
msiexec /i Fauna-Setup-x64.msi /l*v install.log

# Verify the two machine services (FaunaNest, FaunaBridge) — Sync is a per-user
# logon agent, so it appears in Task Manager → Startup, not services.msc:
sc query FaunaNest
sc query FaunaBridge

# Uninstall (preserve data)
msiexec /x Fauna-Setup-x64.msi

# Uninstall (remove data)
msiexec /x Fauna-Setup-x64.msi REMOVE_USER_DATA=1
```

## Features

| Feature | Description | Default | Requires |
|---------|-------------|---------|----------|
| Sync Service | `fauna-sync-agent.exe` (per-user logon agent) | Selected | -- |
| Explorer Integration | Shell overlay icons + context menus | Selected | Sync |
| Nest Service | `fauna-nest-svc.exe` (+ inbound :443 firewall rule) | Unselected | -- |
| Bridge Service | `fauna-bridge-svc.exe` IMAP/SMTP bridge | Unselected | Nest |
| Desktop App | WinUI 3 app + protocol handler | Selected | -- |

All features are independently installable and deselectable, so a headless
nest-only install (Nest [+ Bridge], no Sync, no Desktop App) is possible.
There is no admin CLI (`fauna-ctl` left the MSI 2026-06-20 and was deleted
2026-10-02): a headless box is configured entirely from a client at `test@<ip>`.

**Sync is a per-user logon agent, not a machine service** — `fauna-sync-agent.exe`
runs in each user's session under their standard token, launched by an HKLM
`…\Run` entry. The two **machine services** are FaunaNest and FaunaBridge: each
runs under its own virtual service account (`NT SERVICE\FaunaNest`,
`NT SERVICE\FaunaBridge`) with automatic failure recovery (restart with a flat
5 s delay for all three attempts; failure count resets after 24 h). The Nest
service additionally adds a Windows Firewall inbound allow rule (TCP :443,
scoped to `fauna-nest-svc.exe`) so the nest is reachable from another machine.

## Architecture

Two separate MSIs: `Fauna-Setup-x64.msi` and `Fauna-Setup-arm64.msi`.
Same WiX source files, different binary staging directories.

### Data directories

Each service stores data under `%PROGRAMDATA%\Fauna\`:

```
%PROGRAMDATA%\Fauna\
+-- sync\
+-- nest\
+-- bridge\
```
