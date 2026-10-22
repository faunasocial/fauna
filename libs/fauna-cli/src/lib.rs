//! The **cli app's tool catalog** — one transport-agnostic, shell-agnostic
//! catalog every cli shell speaks over the Model Context Protocol (MCP)
//! (`docs/goal/architecture/apps/cli.md` § One catalog, many front-ends).
//!
//! The app is Fauna operated by a large language model: its whole surface is
//! this catalog, and its reach is exactly the grants the user minted from their
//! own app, because it is a **principal** of the user — it holds no identity
//! key and no nest bearer (§ The principal shape). A shell supplies a transport
//! and a [`Host`] (the live principal session, the consent start), nothing
//! more (architectural rule 3).
//!
//! - [`catalog`] — every tool's name, schema, truthful MCP annotations, the
//!   ui.yaml page it serves and the scope arm it needs; a tool that names no
//!   page does not compile (rule 4).
//! - [`tools`] — the handlers: verbs of the page's shared manager, the
//!   manager's error verbatim as an MCP tool error.
//! - [`mcp`] — the JSON-RPC 2.0 method layer (initialize, tools/list,
//!   tools/call, ping), a minimal in-tree layer (§ Test surface).
//! - [`argv`] — the `argv` shell: the command tree derived from the catalog,
//!   its help, the terminal rendering, the exit codes and the
//!   destructive-tool confirmation.
//! - `device` (feature) — the device shell's half: the newline-delimited
//!   stdio transport, the loopback streamable-HTTP transport, the DPoP prover and the typed-code consent start.
//! - `door` (feature `device`) — the conversations tools' client of the
//!   conversations oracle's door on this device.

pub mod argv;
pub mod catalog;
pub mod mcp;
pub mod tools;

#[cfg(feature = "device")]
pub mod device;
#[cfg(feature = "device")]
pub mod door;
#[cfg(feature = "device")]
pub mod http;
#[cfg(feature = "device")]
pub mod stdio;

pub use catalog::{Annotations, CATALOG, Tool, ToolId, consent_scope};
pub use mcp::Server;
pub use tools::{Host, SessionUnavailable, ToolError};
