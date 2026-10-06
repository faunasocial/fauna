//! **iroh-QUIC implementation of the P2P-transport seam** — the droppable
//! *second* impl behind the shared [`fauna_transport`] trait, beside the bespoke
//! WireGuard stack (`fauna-peer`/`fauna-wireguard`, the first impl).
//!
//! Design authority tracked internally (the seam + iroh-as-second-impl +
//! the nest relay). Goal-doc authorities:
//! `docs/goal/behavior/p2p.md` (§ Transport seam), `docs/goal/architecture/transport.md`
//! (§ Future directions). The prototype results that cleared this impl's
//! load-bearing risk (PQ handshake over iroh, DAG-CBOR Y.1 over a bidi stream,
//! SPKI-neutral provider swap, self-hosted relay sovereignty) are tracked
//! internally.
//!
//! ## Reversible, feature-gated, unconsumed (additive landing, 2026-06-28)
//!
//! The user adopted iroh as a **reversible bridge** (2026-06-28) and chose
//! **additive-first**: this crate lands *beside* the WireGuard impl, **gated
//! behind the non-default `quic` feature** and **not yet wired to any consumer**
//! — the listener-side data-plane reshape + the per-client consumer rewire onto
//! [`fauna_transport::PeerConn::open_stream`] are the separate, deferred Y.1
//! migration (`transport.md` § Future directions). So enabling iroh changes no
//! production behavior today; it makes the second substrate *exist* and *compile*
//! behind the seam, ready for the capability-negotiated wiring when Y.1 is
//! scheduled. The shipping `--features bluesky` nest image leaves `quic` off, so
//! iroh never enters the production binary.
//!
//! ## What lands here vs. what is deferred
//!
//! - **Here:** [`IrohTransport`] (`PeerTransport` over an iroh [`Endpoint`] — QUIC
//!   bidi streams as [`fauna_transport::ByteStream`]s, the authenticated iroh
//!   `remote_id` as [`fauna_transport::PeerConn::peer_identity`], iroh's observed
//!   connection type as [`fauna_transport::PeerConn::path`]). Unlike the WireGuard
//!   impl, `listen()` is **supported** natively (iroh accepts inbound QUIC
//!   connections directly).
//! - **Deferred (sidecar track):** the nest-side self-hosted `iroh-relay` +
//!   `iroh-dns-server`, s6-supervised and advertised under the existing `relay`
//!   capability. This crate is relay-agnostic for now (direct / discovered paths
//!   only); relay wiring lands with that sidecar.
//! - **Deferred (§7.3 migration):** PQ (`X25519MLKEM768`). The crypto provider is
//!   an injection point on [`IrohTransport::builder`]; the default is ring (tree
//!   convention), and the PQ swap to aws-lc-rs + `prefer-post-quantum` is the
//!   separate, SPKI-neutral `ring → aws-lc-rs` migration.

#[cfg(feature = "quic")]
mod transport;

#[cfg(feature = "quic")]
pub use transport::{
    DEFAULT_ALPN, IrohConn, IrohTransport, IrohTransportBuilder, ceremony_transport,
    peer_leg_transport,
};

/// Re-exported because it is [`IrohTransportBuilder::relay_url`]'s own
/// parameter type: consumers (the peer-leg factories in tui and the sync
/// agent) parse the nest-advertised URL string into it without taking a
/// direct `iroh` dependency.
#[cfg(feature = "quic")]
pub use iroh::RelayUrl;
