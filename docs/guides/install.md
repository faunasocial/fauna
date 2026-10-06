# Installing the Fauna app

> **The honest version first:** Fauna is in closed alpha. There are **no app
> store listings, no installers to download, and no packaged releases yet** —
> apart from the web app (which needs no install at all), running a Fauna
> app today means **building it from source**. Every app below builds
> and runs; that is what "available" means on this page.

## The no-install option: the web app

Every Fauna server (nest) serves the full web app itself at:

```
https://<the-nest's-domain-or-ip>/app/
```

If you have a nest to connect to — yours or a friend's — open that address in
any modern browser and follow [Getting started](getting-started.md). Nothing
to install, and it's the same app as the native apps.

One thing to know before you use it for a nest you don't run yourself: the
app at that address comes from the nest, so you are trusting whoever runs the
nest with the code you run, not only with your data. For your own nest that
is exactly right. For someone else's nest, the safer choice is a native app
(below), which you build from the project's source rather than load from the
nest. The Fauna project will also host the web app at its own address,
`https://app.fauna.social`, for exactly this reason — it is not live yet;
when it is, bookmark it and open your nest from there.

## Building the native apps

All apps share one repository and one Rust core. Clone it, and the pinned
Rust toolchain installs itself on the first `cargo` run. Builds are
orchestrated with [`just`](https://github.com/casey/just). Each app directory
has a `README.md` with the details; the short version:

### Linux (GTK4)

Native desktop app. On Debian/Ubuntu:

```sh
sudo apt install build-essential pkg-config libssl-dev cmake clang \
                 libgtk-4-dev libadwaita-1-dev libdbus-1-dev
just linux-run          # build + launch
just linux-install      # install to ~/.local
```

Details: [`apps/fauna-linux/README.md`](../../apps/fauna-linux/README.md).

### macOS (SwiftUI)

Needs a Mac with Xcode command-line tools.

```sh
just mac-debug          # builds the FaunaMacOS product via SwiftPM
```

Details: [`apps/fauna-apple/README.md`](../../apps/fauna-apple/README.md).

### iOS (SwiftUI)

Needs a Mac with Xcode; runs in the simulator or on your own device with your
developer signing.

```sh
just apple-ffi          # the shared-core XCFramework
xcodebuild -scheme FaunaiOS   # then build/run from Xcode
```

Details: [`apps/fauna-apple/README.md`](../../apps/fauna-apple/README.md).

### Android (Jetpack Compose)

Needs JDK 17 + the Android SDK — setup walkthrough in
[`apps/fauna-android/BUILD-SETUP.md`](../../apps/fauna-android/BUILD-SETUP.md).

```sh
just android-debug      # builds the APK (shared core via UniFFI)
```

Details: [`apps/fauna-android/README.md`](../../apps/fauna-android/README.md).

### Windows (WinUI 3)

Needs Visual Studio's MSBuild and the .NET SDK; the XAML app must be built
with MSBuild (not `dotnet build`).

Details: [`apps/fauna-windows/README.md`](../../apps/fauna-windows/README.md).

### Terminal (TUI)

A seventh app, `fauna-tui` (a Rust terminal UI built on ratatui +
crossterm), runs on Linux, macOS and Windows alike — full-screen, keyboard
and mouse driven, no display server needed. Handy over SSH or on a headless
box.

**Installing a release.** Each release publishes one archive per operating
system and processor (`fauna-tui-<arch>-linux.tar.gz`,
`fauna-tui-<arch>-darwin.tar.gz`, `fauna-tui-<arch>-windows.zip`), each
carrying the terminal app together with the sync agent it drives. Verify the
download the way the release page describes (see *Checking you have the
official app* below), unpack it, and on Linux or macOS run the `install.sh`
inside it — it copies both programs into `~/.local/bin` (or `/usr/local/bin`
when run with `sudo`), and `./install.sh --uninstall` removes them again. On
Windows, unpack the zip into a folder on your `PATH`. The macOS package and
the Windows installer also install the terminal app alongside the desktop
app (the "Fauna Terminal App" choice, on by default), so if you installed one
of those you already have it.

**Building from source** instead:

```sh
just tui-debug          # or: cargo build -p fauna-tui
```

The app tells you when a newer release is out: press **Check for Updates**
on the Settings page and, if there is one, it names the version and the
release page to get it from. It also looks once by itself each time you sign
in, and shows the same note on the Settings page if a newer release is out.
It never checks again behind your back, and nothing is downloaded or
installed for you.

## Checking you have the official app

Fauna is free software, so anyone may build it and share it. That is welcome
and it also means the name alone does not tell you who published what you
are about to install. One page does:

```
https://fauna.social/official-apps
```

It lists, for every platform, the package or bundle identifier the official
app carries, the publisher name and identity a store shows for it, the
address of the official listing, and the signing identities behind the
downloads and updates. Every row is generated from the project's own source,
so the page cannot promise one thing while a release does another. A row
marked "not yet" is an identity that does not exist yet, with the reason;
the page never shows a placeholder.

To check an app, compare what you can see with the page:

- **On a phone**, open the app's details in the system settings, or look at
  the store address bar: the package name must be exactly the one listed
  (`social.fauna.fauna`), and the store's publisher line must be the one
  listed too.
- **On a computer**, the installer or app bundle names its publisher and
  identifier; the page tells you which command shows them on each system and
  what they must read.
- **On the web**, the app comes from the address you opened it at. From your
  own nest that is your nest's address; from the project it will be the
  address the page lists, once that is live.

Today, during the closed alpha, no official app is on any store and no
installer has been released. So right now the rule is short: **a store entry
or a download calling itself Fauna is not the project's.** If you find one,
write to `hello@fauna.social` with where you saw it; we ask the store to
take it down.

## The server

The nest server is its own guide: [on the internet](nest-internet-setup.md)
or [at home](nest-home-setup.md).
For development, `just docker-run` builds and runs a local nest container with
the web app at `http://localhost:3000/app/`.

## What "alpha" means for you

- Expect rough edges, and expect to update often (the apps and server evolve
  together; within a major version they stay compatible in both directions).
- Your data is designed to survive: updates never require wiping the nest, and
  everything you put in stays exportable from your own app.
- Packaged releases (app stores, installers, distro packages) arrive as the
  project approaches its first public release.
