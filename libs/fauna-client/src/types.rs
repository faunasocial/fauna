// The HTTP response DTOs that once lived here — `NodeInfo` /
// `RegistrationInfo` / `HandleAvailability` (the discovery reads) and the
// `/auth/token` `AuthToken*` pair — were deleted along with their HTTP routes
// in the WS-RPC-everywhere rip. Every former HTTP read is now a WS-RPC kind
// whose wire types live in `fauna_protocol` (e.g.
// `fauna_protocol::discovery::{NestInfoReply, HandleAvailableReply}`),
// consumed through the per-feature client crates. This module now only
// re-exports the connection-lifecycle state.

/// The supervised-connection lifecycle state now lives in the shared substrate
/// (it is driven by `fauna_ws_substrate::run_supervisor`); re-exported here so
/// existing `fauna_client::types::ConnectionState` / `fauna_client::ConnectionState`
/// paths keep resolving.
pub use fauna_ws_substrate::ConnectionState;
