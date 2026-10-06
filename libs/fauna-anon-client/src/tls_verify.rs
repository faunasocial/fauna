//! The capturing TLS certificate verifier — re-exported from
//! [`fauna_ws_substrate::tls_verify`], its shared home.
//!
//! Lifted into `fauna-ws-substrate` (Spec Y2 slice 4 §5, priority #2/#4) so the
//! bearer client channel and the nest↔nest federation channel share one
//! implementation. This module preserves the existing
//! `fauna_anon_client::tls_verify::{…}` paths; see the substrate crate for the
//! verifier itself and the security-model documentation
//! (`docs/goal/architecture/security.md` § Transport trust).

pub use fauna_ws_substrate::tls_verify::{
    CaptureHandle, CapturedCert, NoPinPolicy, PinResolver, capturing_client_config,
    dynamic_pinned_client_config, spki_pinned_client_config,
};
