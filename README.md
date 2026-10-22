# Fauna

Fauna is open-source software for social communication — an app for every device and a small server you control — where you own your data, control your experience, and no single actor holds power over the network.

## Principles

**Non-profit, by design.** Fauna is developed by Fauna Social, a self-owned Norwegian association (forening). The bylaws prohibit charging users, selling advertising or user data, and paying members. Funding comes from voluntary donations for direct costs like hosting and domain registration. On dissolution, assets go to an organization with an equivalent mission — or, failing consensus, to the EFF. These rules are protected by a tiered amendment process that requires unanimity to change.

**Open source, no strings.** All code is open source. Contributors retain their copyright. There is no CLA, no relicensing risk, and no corporate parent that can change the terms.

**User owns data.** Your identity is a cryptographic key pair that you hold. Your posts, profile, social graph, and messages belong to you. If you ever change nests, your key goes with you, and your data comes back from your backup; you can also export everything, or run your own nest. Nothing ties you to any particular infrastructure.

**No privileged actors.** Servers — called Nests — store and forward content, but they are not authorities. Every post is cryptographically signed by its author; every client verifies signatures independently. A Nest cannot forge or alter content, and what it withholds of a single post or message it withholds visibly: a notice stands in the content's place, the author of a post can appeal, and the act is recorded. If a Nest disappears, the network continues. Anyone can run one.

**Layered moderation.** Moderation is never centralized. Your own filters shape what you see, and you can sort the posts you see according to labelers you subscribe to, which are transparent and opt-in. Inside a group, its owner and admins can delete a member's message for everyone in it, and the deletion shows in its place. Beyond that, exactly three parties can block a single post or message for others: a guardian for a child's account; a nest's admin, by hand under a legal obligation or on a user's report and by the rules it publishes for its nest; and, once built, a region's authority by published rules. Every block of a post or message is visible and names its rule or reason; a block by hand also leaves a permanent record, and an appeal for a post; nothing is removed silently.

## How it works

Fauna uses a shared-core architecture. A single Rust library (`fauna-core`) implements the protocol, cryptography, storage, and sync logic. This core compiles to WebAssembly for the web app and generates native bindings via UniFFI for mobile and desktop platforms. Each platform gets a native UI — Svelte for web, SwiftUI for Apple, Jetpack Compose for Android, GTK4 for Linux, .NET for Windows, and a Rust terminal UI for the command line — while sharing the same protocol implementation underneath.

**Communication is end-to-end encrypted by default.** Direct messages and group conversations between people on Fauna use the MLS protocol (RFC 9420) for forward-secret, multi-device encryption; a group may instead choose to be a community room, which its home nest can read, to label it for its members and keep a search index; and a conversation with someone on email or another network is protected only as far as that network allows. All content is signed with Ed25519 and hashed with BLAKE3 for integrity and deduplication.

**Bridges connect Fauna to other networks.** Protocol bridges let Fauna users communicate with people on Bluesky (AT Protocol), the Fediverse (ActivityPub), Nostr, and email (SMTP/IMAP) — without requiring those users to switch platforms. The feed bridges for Bluesky, the Fediverse and Nostr run inside the Nest; the mail bridge and the AT Protocol hosting bridge run as separate programs beside it, translating between protocols while preserving Fauna's cryptographic guarantees where possible.

**Offline-first.** Clients compose and sign content locally. Sync happens when connectivity is available. There is no requirement to be online to use Fauna.

## Status

Fauna is under active development and not yet ready for general use. The core protocol, Nest server, and clients for web, iOS, macOS, Android, Windows, Linux, and the terminal are implemented and in closed alpha testing. Bridge support for AT Protocol, ActivityPub, Nostr, and email is functional. There is no public release yet; outside Google Play's invitation-only testing, trying it means building from source.

## Building

The repository is a Rust workspace orchestrated with [`just`](https://github.com/casey/just); the pinned toolchain in `rust-toolchain.toml` installs automatically on first `cargo` run.

```sh
cargo build --workspace        # server, shared libraries, Linux and terminal clients
just web                       # web client (needs Deno + wasm-pack)
just mail-bridge-test          # Go mail bridge (needs Go)
```

The Apple, Android, and Windows clients build with their platforms' native toolchains from `apps/fauna-apple`, `apps/fauna-android`, and `apps/fauna-windows`. See [CONTRIBUTING.md](CONTRIBUTING.md) for the full build and contribution workflow.

## Documentation

Start at the [documentation front door](docs/README.md).

- [User guides](docs/guides/) — [getting started](docs/guides/getting-started.md), [installing the app](docs/guides/install.md), [setting up a nest](docs/guides/nest-internet-setup.md), and the [your-own-cloud](docs/guides/your-own-cloud.md) series
- [`docs/goal/`](docs/goal/) — the project's internal design and architecture specifications, published for transparency; they describe target state and are not user guides
- [CONTRIBUTING.md](CONTRIBUTING.md) — how to build, contribute, and how this published mirror relates to upstream development
- [SECURITY.md](SECURITY.md) — reporting vulnerabilities (please, not via public issues)
- [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)

## License

Dual-licensed under [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option, with no CLA — contributors retain their copyright, with the exception of vendored third-party material, which keeps its upstream licenses — see [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md) for the full list of third-party source, vendored assets, and dependencies (Go modules, npm packages) this repository ships or pulls in.

The licences cover the code, not the name: the Fauna names and artwork are held by the association and governed by the [trademark policy](TRADEMARK.md) — unmodified redistribution, packaging and "works with Fauna" statements are free; a modified version distributed to others takes its own name.
