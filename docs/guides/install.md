# Installing the Fauna app

> **The honest version first:** Fauna is in closed alpha. There is **no public release yet** — outside Google Play's invitation-only testing, and apart from the web app (which needs no install at all), running a Fauna app today means **building it from source**. Every app below builds and runs; that is what "available" means on this page.

## The no-install option: the web app

Every Fauna server (nest) serves the full web app itself at the address below, unless its admin has chosen to send that address to the project's own web address for the app instead (a nest without a domain yet keeps serving it itself). That address is not live yet; once it is, you land there on the same app, already pointed at that nest.

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
nest. The Fauna project will also serve the web app from its own address, `https://app.fauna.social`, for exactly this reason: that address serves the app's code and holds nothing of yours. It is not live yet; when it is, bookmark it and open your nest from there.

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

Needs JDK 21 + the Android SDK — setup walkthrough in
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
carrying the terminal app together with the sync agent it drives. Verify the download the way the release page describes (see *Checking you have the official app* below), unpack it, and on Linux or macOS run the `install.sh` inside it — it copies the programs into `~/.local/bin` (or `/usr/local/bin` when run with `sudo`), and `./install.sh --uninstall` removes them again. On macOS, an install from the archive alone does not yet keep the sync agent running in the background. On
Windows, unpack the zip into a folder on your `PATH`. The macOS package and the Windows installer also install the terminal app alongside the desktop app (the "Fauna Terminal App" choice, on by default), so with either of those you have it already.

**Building from source** instead:

```sh
just tui-debug          # or: cargo build -p fauna-tui
```

The app tells you when a newer release is out: press **Check for Updates**
on the Settings page and, if there is one, it names the version and the
release page to get it from. It also looks once by itself each time you sign
in, and shows the same note on the Settings page if a newer release is out.
That look asks only your own server, which keeps track of new releases, so
signing in contacts no one else.
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

Today, during the closed alpha, there is no public release yet: no installer has been released, and the only official app on any store is the Android app in Google Play's invitation-only testing, which you can reach only from an invitation the project sent you. So right now the rule is short: **any other store entry, and any app download, calling itself Fauna is not the project's.** If you find one,
write to `hello@fauna.social` with where you saw it; we ask the store to take it down. The server is published too, as the project's container image `ghcr.io/faunasocial/nest`: the **Dev** update channel's development builds are published, a numbered release is published on the **Test** channel, and until the first Stable release is out there is nothing on the **Stable** channel.

## The server

The nest server is its own guide: [on the internet](nest-internet-setup.md)
or [at home](nest-home-setup.md).
For development, `just docker-run` builds and runs a local nest container with the web app at `https://localhost:3443/app/`.

## What "alpha" means for you

- Expect rough edges, and expect to update often (the apps and server evolve together; during the 0.1 versions an update may need a fresh install of the app or the nest, and from 0.2.0 on, apps and servers within a major version stay compatible in both directions).
- During the 0.1 versions an update may need a fresh start, so export anything you want to keep first; from 0.2.0 on, updates never require wiping the nest. Your data stays exportable from your own app: **Settings → Account → Export My Data** downloads what your nest holds for you, your mail included, and **Export mailbox**, among the mail pages in Settings, takes your mailbox out in standard formats to move it to another mail service.
- Packaged releases (app stores, installers, distro packages) arrive as the
  project approaches its first public release.
