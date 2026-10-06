//! Internationalized strings for fauna.
//!
//! All strings in [`strings`] are auto-generated from `i18n/strings/en.yaml`.
//! Do not edit `strings.rs` by hand — run `just i18n-generate` instead. It is
//! the **only** Rust emission: tui links this crate directly and linux
//! re-exports it as its own `crate::i18n::strings`.
//!
//! [`time`] is hand-written glue over that table — index-to-name lookups both
//! Rust apps need. Keep this crate **dependency-free**: much of the workspace
//! links it (`fauna-protocol`, `fauna-client`, `fauna-conversations`, …), so
//! anything needing `fauna-core` belongs above it, not here.

pub mod strings;
pub mod time;
