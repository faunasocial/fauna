//! `fauna-nest-http` — one home for native Rust talking HTTP to a fauna nest.
//!
//! # What this is
//!
//! The **residual REST surface** between a fauna nest and its native-Rust
//! consumers, consolidated: the [`ApiError`] taxonomy, the [`paths`]
//! constants, the generic content trait (`NestContentApi`) + a `BearerSource`
//! abstraction + the `reqwest`-backed impl (`ReqwestNestContentApi`) with the
//! 401-reactive token-refresh policy (design tracked internally).
//!
//! # Two layers (`error`/`paths` always; `client` feature for the rest)
//!
//! [`error`] (the [`ApiError`] taxonomy) and [`paths`] (the path constants)
//! are std-only and depend on no HTTP stack — the **portable subset**. The
//! `client` feature (on by default) adds the native HTTP machinery: [`bearer`],
//! [`content`], and the `test-helpers`-gated `FakeNestContentApi`. A wasm
//! consumer that only needs the taxonomy + constants depends on this crate
//! with `default-features = false` (so `reqwest`/`tokio`/etc. stay out of its
//! bundle); see `libs/fauna-onboarding-machine`'s `nest_api` below.
//!
//! # Consumers
//!
//! - `apps/fauna-linux` — its `nest_content_api` module is a re-export of
//!   this crate (+ the `LaunchMachineBearer` glue). Feature `launch-machine`.
//! - `libs/fauna-client` — its `AuthClient` mints + caches the bearer over the
//!   `fauna.auth.handshake` WS-RPC kind (its own `WsChallengeBearer`), exposed
//!   as an `Arc<dyn BearerSource>` this crate's content layer accepts.
//! - `libs/fauna-onboarding-machine`'s `nest_api` — shares the [`ApiError`]
//!   taxonomy (its six per-endpoint error enums grow `From<ApiError>`), **but
//!   keeps its own typed-endpoint trait** (the former `paths::onboarding`
//!   constants are gone — that nest surface migrated to pre-identity WS-RPC
//!   kinds): a fixed 7-endpoint shape, each with a rich typed response the
//!   wizard's snapshot builder consumes directly and endpoint-specific status
//!   handling (`403 → Closed`, `404 → NotFound`, …), and it compiles to wasm.
//!   Merging the two traits is a possible future direction (would need the
//!   wasm story + onboarding-area sign-off); not done. Design doc §5.
//!
//! The non-Rust clients (web `api.ts`, Windows `DirectNestClient.cs`, Android
//! `ApiClient.kt`, Swift FaunaKit) re-implement this *shape* in their own
//! language — the crate is their reference, not code they share.
//!
//! # What this is NOT
//!
//! - **Not WS-RPC.** The authenticated UI request/reply + push surface runs
//!   over WS-RPC (`libs/fauna-protocol` + `libs/fauna-client`), unchanged
//!   by this crate. `fauna-nest-http` is the *complement* — the HTTP residue
//!   — not a replacement. New feature surfaces extend WS-RPC, not this.
//! - **Not the web/WASM HTTP layer.** The `client` feature — the
//!   `reqwest`-backed impl with a `LaunchMachine`-driven bearer + 401 retry,
//!   and the bridge daemon's keypair signing — is a native concern; the web
//!   app keeps its TS HTTP layer. (The `error`/`paths` subset *does* build
//!   on wasm, so a wasm Rust consumer can share the taxonomy + constants — but
//!   it implements the requests itself, e.g. the onboarding wizard's
//!   `nest_api`.)
//!
//! # Permanent residue only
//!
//! Everything here is the **permanent HTTP residue**: auth bootstrap, public
//! discovery, blob & chunk transfer, the data export, and the routes shared
//! with federation (CalDAV-backed calendar/event routes; cross-nest inbox
//! delivery). These never become WS-RPC kinds — DAG-CBOR frames are the wrong
//! shape for a multi-MB octet stream. The Layer-1 `/api/v1/*` UI
//! request/reply CRUD this crate once carried has migrated to `fauna.<area>.*`
//! WS-RPC kinds, each path constant removed with its route ([`paths`]' module
//! docs; the authoritative inventory is `docs/goal/architecture/api-layers.md`
//! § HTTP residue). A new feature surface extends WS-RPC, not this crate.
//!
//! # Adding a method
//!
//! When [`NestContentApi`] gains a method, mirror it in [`ReqwestNestContentApi`]'s
//! impl and in `FakeNestContentApi` (the `test-helpers`-gated fake), add a
//! `wiremock` round-trip test (`tests/content_round_trip.rs`), and add the
//! path constant to [`paths`] under the right feature module.

// `error` + `paths` are the portable subset — std-only, no `reqwest`/`tokio`
// — so a wasm consumer (the onboarding wizard's `nest_api`) can share the
// `ApiError` taxonomy and the path constants without dragging the native HTTP
// machinery into its bundle. They depend on this crate with
// `default-features = false`.
pub mod error;
pub mod paths;

pub use error::ApiError;

// The `client` feature gates the bearer-authed REST machinery — native
// concerns (`reqwest` + the `LaunchMachine`/keypair bearer flows). On by
// default; off only for the wasm `paths`/`error` consumers above.
#[cfg(feature = "client")]
pub mod bearer;
#[cfg(feature = "client")]
pub mod capped;
#[cfg(feature = "client")]
pub mod content;

#[cfg(feature = "client")]
mod fake;

#[cfg(feature = "client")]
pub use bearer::{BearerSource, StaticBearer};
#[cfg(feature = "client")]
pub use content::{NestContentApi, ReqwestNestContentApi};

#[cfg(feature = "launch-machine")]
pub use bearer::LaunchMachineBearer;

#[cfg(all(
    feature = "client",
    any(test, debug_assertions, feature = "test-helpers")
))]
pub use fake::{FakeNestContentApi, Verb};
