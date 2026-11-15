# Installing the Fauna app

> **The honest version first:** Fauna is in closed alpha. Released versions, the first of them 0.1.6, are published on the project's [releases page](https://github.com/faunasocial/fauna/releases), with downloads of the macOS, Windows, Linux and terminal apps; the Android app is in Google Play's invitation-only testing, and the web app needs no install at all. For the iOS app, and for an app on a system those downloads do not cover, running it today means **building it from source**. Every app below builds and runs; that is what "available" means on this page.

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
is exactly right. For someone else's nest, the safer choice is a native app (below), which you download from the project's releases page or build from its source rather than load from the nest. The Fauna project will also serve the web app from its own address, `https://app.fauna.social`, for exactly this reason: that address serves the app's code and holds nothing of yours. It is not live yet; when it is, bookmark it and open your nest from there.

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

**Installing from Google Play.** If the project invited you to test the Android app, you don't need to build it: open the link in your invitation on your Android phone, signed in to Google with the account the invitation was sent to, accept the invitation, and install Fauna from Google Play through the link on that page. Only the Google accounts the project invited can install it this way.

**Building from source** instead needs JDK 21 + the Android SDK — setup walkthrough in
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

**Installing a release.** Each pre-release and release publishes one archive per processor for Linux and for Windows, and since 0.1.6 for macOS too (`fauna-tui-<arch>-linux.tar.gz`, `fauna-tui-<arch>-darwin.tar.gz`, `fauna-tui-<arch>-windows.zip`), each carrying the terminal app together with the sync agent it drives and `fauna-cli`, the program an AI assistant runs to work in Fauna. Verify the download as the project's [code signing policy](https://fauna.social/code-signing-policy#how-to-check-a-signature) describes (see also *Checking you have the official app* below), unpack it, and on Linux or macOS run the `install.sh` inside it — it copies the programs into `~/.local/bin` (or `/usr/local/bin` when run with `sudo`), and `./install.sh --uninstall` removes them again. On Windows, unpack the zip into a folder on your `PATH`. The Windows installer also installs the terminal app, with `fauna-cli`, alongside the desktop app (the "Fauna Terminal App" choice, on by default), so with it you have both already; the macOS package, once published, does the same.

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

Fauna is free software, so anyone may build it and share it. Official app
downloads are published on the project's [releases
page](https://github.com/faunasocial/fauna/releases). To check a download,
compare its SHA-256 hash with the one the releases page lists; the page at
`https://fauna.social/apps` gives the command for Windows, macOS and Linux.

Today, during the closed alpha, the only official app on any store is the Android app in Google Play's invitation-only testing, which you can reach only from an invitation the project sent you, and the only official app downloads are the releases and pre-releases on the project's [releases page](https://github.com/faunasocial/fauna/releases), and the project's [code signing policy](https://fauna.social/code-signing-policy#how-to-check-a-signature) says how to check that a file from them is ours. So right now the rule is short: **any other store entry, and any app download from anywhere else, presenting itself as Fauna Social's app is not ours.** If you find one, write to `hello@fauna.social` with where you saw it. The server is published too, as the
project's container image `ghcr.io/faunasocial/nest`: the **Dev** update channel's development builds are published, a numbered release is published on the **Test** channel, and released versions are published on the **Stable** channel.

## The server

The nest server is its own guide: [on the internet](nest-internet-setup.md) or [at home](nest-home-setup.md). Both run the nest from the project's container image, the recommended way to run a nest on a server. For development, `just docker-run` builds and runs a local nest container with the web app at `https://localhost:3443/app/`.

### From the Windows installer

The Windows installer can also set up a nest on the computer it installs on: tick its "Nest Service" choice, which is off by default. The macOS package, once published, does the same. The nest then runs as a service that starts with the computer and keeps running with nobody signed in, and your other devices reach it at the computer's address on your network. On its first start it writes a one-time claim code to the file `C:\ProgramData\Fauna\nest\claim-code` on that computer. To claim the nest, set up the Fauna app on one of your devices with a handle at that address — `you@192.168.1.50`, with the computer's real address after the `@`, as [Claim it from the app](nest-home-setup.md#step-7--claim-it-from-the-app) explains — and enter the claim code when asked.

## What "alpha" means for you

- Expect rough edges, and expect to update often (the apps and server evolve together; during the 0.1 versions an update may need a fresh install of the app or the nest, and from 0.2.0 on, apps and servers within a major version stay compatible in both directions).
- During the 0.1 versions an update may need a fresh start, so export anything you want to keep first; from 0.2.0 on, updates never require wiping the nest. Your data stays exportable from your own app: **Settings → Account → Export My Data** downloads what your nest holds for you, your mail included, and **Export mailbox**, among the mail pages in Settings, takes your mailbox out in standard formats to move it to another mail service.
- Public releases arrive once a version passes acceptance, the first of them 0.1.6; until then a version's downloads are pre-releases.
