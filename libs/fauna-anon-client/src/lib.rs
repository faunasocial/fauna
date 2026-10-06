//! Native client of the **pre-identity (anonymous) WS-RPC connection** to
//! `fauna-nest` (`GET /api/v1/ws`, `Sec-WebSocket-Protocol: fauna.v1`, no
//! bearer; transport.md § Pre-identity).
//!
//! Extracted out of `fauna-client` so the pre-identity state machines
//! (`fauna-onboarding-machine`, `fauna-launch-machine`) can consume the
//! anonymous connector **without** transitively depending on `fauna-nest-http`
//! (the legacy HTTP layer). `fauna-client → fauna-nest-http →
//! (`launch-machine` feature) → fauna-launch-machine` is a real Cargo edge; a
//! launch- or onboarding-machine that reached `AnonymousNestClient` through
//! `fauna-client` would close that into a cycle. Reaching it through this leaf
//! crate (which has no `fauna-nest-http` dep) breaks the cycle.
//!
//! Native-only. The wasm twin is `fauna-rpc-wasm::AnonymousWsRpcClient`.

mod bearer;
pub mod cert_binding;
pub mod client;
mod dispatch;
pub mod error;
pub mod tls_dial;
pub mod tls_verify;
mod token_client;
pub mod trust;
mod ws;

pub use bearer::{
    MintedBearer, mint_bearer_over_custody_handshake, mint_bearer_over_device_handshake,
    mint_bearer_over_handshake, mint_bearer_over_silent_challenge,
};
pub use cert_binding::{
    BindingError, DiskPinStore, IdentityError, IdentityOutcome, IdentityRoot, MemoryPinStore,
    NestIdentityPinStore, check_identity_root, verify_cert_binding,
};
pub use client::{AnonymousNestClient, FirstContactOutcome};
pub use error::AnonClientError;
pub use tls_verify::{CaptureHandle, CapturedCert};
pub use token_client::TokenNestClient;
pub use trust::{
    Graduated, IdentityChangedVerdict, PinMinting, TrustError, authority_of,
    classify_identity_changed, forget_identity_pin, fresh_nonce, graduate_handshake,
    graduate_handshake_with_root, graduate_handshake_with_root_minting, graduate_verify_path,
    install_pin_store, pinned_identity, pinned_spki, read_login_binding, resolve_dns_self_root,
    store_pinned_reqwest_tls,
};
