# fauna-windows — the Windows app

Native Windows app: **WinUI 3** shell over a C# core, consuming the shared
Rust core via UniFFI C# bindings (`libs/fauna-ffi` + the vendored
`libs/uniffi-bindgen-cs`). Beyond the app itself, this directory holds the
Windows service pieces: a local nest service, sync service, bridge service,
a control CLI, and the Explorer shell extension.

## Build

Needs the .NET 10 SDK and Visual Studio Build Tools (MSBuild). Two steps from
the repository root:

```sh
just windows-ffi      # Rust FFI cdylib + generated C# bindings
```

Then build the app with **MSBuild** (`-restore` so the regenerated bindings
are picked up), e.g.:

```sh
MSBuild.exe apps/fauna-windows/FaunaApp/FaunaApp/FaunaApp.csproj \
    -restore -p:Platform=ARM64 -p:Configuration=Debug
```

Set `Platform` to `ARM64` or `x64` to match your machine. On ARM64 hosts the
WinUI XAML compiler crashes under `dotnet build` — use MSBuild for the app
project (plain class libraries and console projects build fine with
`dotnet build`). The `just windows-debug` / `windows-release` / `windows-run`
recipes wrap this invocation.

## Test

```sh
dotnet test apps/fauna-windows/FaunaApp/FaunaApp.Tests    # C# unit tests
pytest tests/e2e-unified/tests/ --client windows          # cross-app e2e (see tests/e2e-unified/README.md)
```

## Layout

- `FaunaApp/FaunaApp/` — the WinUI 3 app (views, XAML)
- `FaunaApp/FaunaApp.Core/` — view-models + FFI glue (platform-independent C#)
- `FaunaApp/FaunaApp.Tests/` — unit tests
- `fauna-nest-service/`, `fauna-bridge-service/` — Windows services (local nest, bridges); the per-user file-sync agent is the cross-platform `bins/fauna-sync-agent`
- `shell-ext/`, `installer/` — Explorer integration + MSI installer

UI element IDs come from `tests/e2e-unified/ui.yaml`
(`AutomationProperties.AutomationId`) — the same IDs as every other app.
