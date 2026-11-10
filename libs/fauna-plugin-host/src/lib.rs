//! The nest-hosted **WASM plugin runner** — the portable execution form of a
//! third-party principal (`docs/goal/architecture/third-party.md` § Execution
//! forms → *The runner contract* / *WASM components*; the sandbox profile is
//! `security.md` § Co-resident process trust boundary → *Hosted third-party
//! code*).
//!
//! A plugin is a WebAssembly **component** against the `fauna:plugin` world
//! (`wit/plugin.wit`). The host links exactly the declared imports — the nest
//! API, a state scope, the holder-key operations, outbound HTTP to named
//! hosts, a clock, a log line — and nothing ambient: no WASI, no randomness,
//! no file, no socket. Every import is served by an embedder-provided
//! [`HostServices`] on the plugin's behalf, under the principal row the
//! install minted, so what a plugin can reach is the row's data under the
//! compiled ceiling, exactly as for a remote principal.
//!
//! **Caps.** Fuel bounds CPU per call ([`PLUGIN_FUEL_PER_CALL`]) and a store
//! limiter bounds the instance's linear memory ([`PLUGIN_MAX_MEMORY_BYTES`],
//! every memory together) and table elements ([`PLUGIN_MAX_TABLE_ELEMENTS`]) — the labeler
//! runtime's two caps (`fauna_labeler`), applied here per instance. An
//! exhausted budget terminates the call as [`PluginFault::OutOfFuel`] and
//! poisons the instance; the host survives, and the embedder decides whether
//! to re-instantiate. A call yields to the async executor every
//! [`PLUGIN_FUEL_YIELD_INTERVAL`] units of fuel, so a runaway call holds no
//! worker thread for its whole budget. A refused memory or table growth is observed by the plugin
//! as a failed `memory.grow` / `table.grow`, never as a host fault. A `log`
//! line is cut at [`PLUGIN_LOG_LINE_MAX_BYTES`]; the embedder bounds the state
//! scope ([`PLUGIN_STATE_MAX_BYTES`]) and each call's wall-clock time, since
//! fuel is not spent while the host awaits an import.
//!
//! **The holder key never enters the sandbox.** [`HolderKey`] is minted by the
//! host at install and kept in the principal's state scope on the host side;
//! the plugin reaches it only through `holder.public-key` and
//! `holder.open-grant`, the second performed by the host with the shared
//! `unseal_capability`. The boundary this buys is stated precisely: neither
//! the plugin's own code nor any remote party can extract the secret; the nest
//! process, which owns the sandbox's memory anyway, can — the same property a
//! container plugin's volume has.
//!
//! The nest's side — the principal rows, the install leg, the start/stop
//! supervision, ingress — is `bins/fauna-nest`'s; this crate is the sandbox
//! and its contract, so a second embedder (a dev-loop CLI, a test) runs the
//! same code.

pub mod fixture;
mod holder;
mod host;
mod services;

pub use holder::HolderKey;
pub use host::{
    CompiledPlugin, IngressRequest, IngressResponse, PluginEngine, PluginFault, PluginInstance,
    bindings,
};
pub use services::{
    BoxFut, HostServices, HttpRequest, HttpResponse, LogLevel, OutboundPolicy, RpcRefusal,
};

/// Ceiling on one instance's linear memory, in bytes, summed over every
/// memory its core instances create — the labeler runtime's host ceiling
/// (`fauna_labeler::LABELER_HOST_MAX_MEMORY_BYTES`), applied to a plugin
/// instance. A constant, never a knob (`principles.md` § One configuration
/// surface).
pub const PLUGIN_MAX_MEMORY_BYTES: usize = 64 * 1024 * 1024;

/// Ceiling on one instance's table elements, summed over every table — far
/// above the function table a compiled plugin carries and a few megabytes of
/// host memory at most, so `table.grow` cannot allocate in the nest past it.
pub const PLUGIN_MAX_TABLE_ELEMENTS: usize = 100_000;

/// Ceiling on one `log` line, in bytes: a longer one is cut and marked.
pub const PLUGIN_LOG_LINE_MAX_BYTES: usize = 4096;

/// Ceiling on one plugin's state scope, in bytes of keys plus values — what
/// the embedder's `state.put` refuses past. Each bound account's settings
/// rest outside it.
pub const PLUGIN_STATE_MAX_BYTES: usize = 16 * 1024 * 1024;

/// Fuel budget for one export call (`start`, `stop`, one ingress request) —
/// roughly one instruction per unit, so about a second of CPU, the labeler
/// runtime's per-item bound. Deterministic termination: the budget, not a wall
/// clock, is what ends a runaway plugin.
pub const PLUGIN_FUEL_PER_CALL: u64 = 1_000_000_000;

/// Fuel spent between two yields to the async executor — about ten
/// milliseconds of CPU. A long call is cooperative: it hands its worker back
/// at this interval, so a runaway plugin spending its whole
/// [`PLUGIN_FUEL_PER_CALL`] never stalls the other tasks sharing that worker.
/// Yielding spends no fuel and moves no budget: the call still ends exactly
/// where the budget runs out.
pub const PLUGIN_FUEL_YIELD_INTERVAL: u64 = 10_000_000;

/// Ceiling on a compiled component's binary, in bytes — what the install
/// leg's fetch is capped at and what [`PluginEngine::compile`] refuses above.
pub const PLUGIN_MODULE_MAX_BYTES: usize = 16 * 1024 * 1024;
