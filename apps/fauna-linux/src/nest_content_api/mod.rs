//! Bearer-authed REST surface to *our* fauna nest — a thin re-export of the
//! shared [`fauna_nest_http`] crate.
//!
//! [`fauna_nest_http`] owns the `NestContentApi` trait, the [`ApiError`]
//! taxonomy, the `ReqwestNestContentApi` impl with the 401-reactive token
//! refresh, the [`BearerSource`] abstraction, and the path constants
//! ([`fauna_nest_http::paths`]). See that crate's docs (design tracked
//! internally). The only thing local to
//! `fauna-linux` is the `LaunchMachine`→`BearerSource` wiring: `FaunaClient`
//! constructs `ReqwestNestContentApi::new(node_url, http, LaunchMachineBearer(machine))`,
//! and the WS-RPC `NestClient` is built (`NestClient::with_auth`) over the
//! same `LaunchMachineBearer` (the WS bearer rides in the `?token=` query
//! string, not an `Authorization` header, so it can't go through the content
//! API).
//!
//! (Want a `FakeNestContentApi` for a `client.rs` logic test? Add
//! `fauna-nest-http = { ..., features = ["test-helpers"] }` to `[dev-dependencies]`
//! and use `fauna_nest_http::FakeNestContentApi` / `fauna_nest_http::Verb`.)
//!
//! Cross-*nest* traffic — a remote nest the user isn't authenticated to
//! (key-package fetches, cross-nest welcome / inbox posts) — does **not**
//! belong here: those calls are unauthed or signed-body and the remote
//! wouldn't accept our bearer; they keep using `reqwest::Client` directly, as
//! do calls to unrelated services (the GitHub releases API in
//! `check_for_updates`).

pub use fauna_nest_http::{ApiError, LaunchMachineBearer, NestContentApi, ReqwestNestContentApi};
