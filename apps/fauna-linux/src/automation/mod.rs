//! In-process e2e automation: introspect and (later) actuate GTK widgets by
//! test-id from inside the app, replacing the external AT-SPI bridge.
//!
//! Why this exists: the AT-SPI bridge can't actuate `gtk::Switch` (no AT-SPI
//! action) and contends on the session-global a11y bus when multiple
//! instances run in parallel. Serving the driver's `/element/*` contract in-process — direct
//! GTK widget access on the app's own port — fixes both (design + phases
//! tracked internally).
//!
//! This module is the **read core** (Phase 1). The HTTP server + GTK-main-loop
//! marshalling (Phase 2) and actuation (Phase 3) consume it. Everything here
//! must run on the GTK main thread (widget access is main-thread-affine).
//!
//! The HTTP front-end (routes, parsing, wire shapes, op types) lives in the
//! shared `libs/fauna-e2e-agent` crate since the cli client became the second
//! in-process host (2026-07-10); this module keeps the GTK-specific halves
//! (widget find/actuate, test-agent link) and `server.rs` is thin wiring.

pub mod agent;
pub mod find;
pub mod link;
pub mod observables;
pub mod registry;
pub mod server;
