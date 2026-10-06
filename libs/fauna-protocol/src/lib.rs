//! Spec Y L3 transport protocol — wire envelope, DAG-CBOR codec,
//! RpcDispatcher, forward-compat infrastructure.
//!
//! Design tracked internally.

pub mod account;
pub mod account_state;
pub mod admin;
pub mod age;
pub mod atproto;
pub mod atproto_pds;
pub mod auth;
pub mod backup;
pub mod bluesky;
pub mod bridge_atproto;
pub mod bridge_routing;
pub mod bridge_search_policy;
pub mod bridged_conversations;
pub mod bridges_ui;
pub mod claim;
pub mod client_clock;
pub mod codec;
pub mod contacts;
pub mod content_index;
pub mod conversations;
pub mod custody;
pub mod dav_identity;
pub mod delegation;
pub mod discovery;
pub mod dispatcher;
pub mod dns;
pub mod domain_expiry;
pub mod drafts;
pub mod email;
pub mod envelope;
pub mod error;
/// The `ext.*` kind grammar — one parser, shared with the crates that cannot
/// depend on this one (`fauna_bridge_atproto::fauna_scope`'s `records` arm).
pub use fauna_core::ext_kind;
pub mod family;
pub mod features;
pub mod feed;
pub mod files;
pub mod filesync;
pub mod folder_envelope_sig;
pub mod folders;
pub mod generation_escrow;
pub mod group_state;
pub mod handle;
pub mod inbox;
pub mod invite;
pub mod kind;
pub mod kind_manifest;
pub mod labelers;
pub mod labels;
pub mod linkpreview;
pub mod log_plane;
pub mod media;
pub mod media_ticket;
/// The class-2 merge-policy seam — which of the charter's five closed policies
/// a class-2 kind merges under, and the reader-side application of one incoming
/// entry (`account-data-plane.md` § Merge-policy seam).
pub mod merge_policy;
pub mod mls_replica;
pub mod moderation;
pub mod nat_mode;
/// The deployment-seed rotation statement + chain verification
/// (`nest/box-recovery.md` § Deployment-seed rotation) — shared by the nest that
/// mints the chain and every app that walks it before re-pinning.
pub mod nest_rotation;
pub mod node_policy;
pub mod nostr;
pub mod nostr_relay;
pub mod notifications;
/// The `fauna.oauth.consent.*` user surface of the consent starts — typed-code
/// lookup and the per-client block (`authorization-server.md` § Consent).
/// Ungated for the same reason as the issuer's admin kinds below.
pub mod oauth_consent;
/// The `fauna.oauth.*` admin surface over the nest-held OAuth issuer key set —
/// status and rotation (`authorization-server.md` § The issuer). Ungated: the
/// issuer is up whenever the nest is up, so its admin kinds compile in every
/// flavor, bridge features or none.
pub mod oauth_issuer;
/// Per-kind offline classification — which mutations a disconnected client may
/// apply locally, queue as an intent, or must refuse
/// (`account-data-plane.md` § The offline-mutation contract).
pub mod offline_class;
pub mod pair;
/// The `fauna.payments.*` wire plane — a controversial-class feature, so it
/// compiles away with the `payments` feature (`dynamic-features.md` § The cargo
/// feature spine). [`subscriptions::TierAskingPrice`] deliberately stays
/// ungated: an excised build must keep round-tripping a priced tier it cannot
/// buy.
#[cfg(feature = "payments")]
pub mod payments;
pub mod peer;
/// The `fauna.peer.share.*` wire plane — the cross-user share leg, a
/// controversial-class registry feature (`p2p-share`), so it compiles away
/// with the `p2p-share` feature (`dynamic-features.md` § The cargo feature
/// spine; the module's own docs carry the family rules).
#[cfg(feature = "p2p-share")]
pub mod peer_share;
pub mod peer_sync;
pub mod pending_actions;
pub mod personalization;
pub mod plugins;
pub mod posts;
pub mod principals;
pub mod profile;
pub mod protocol_kinds;
pub mod push;
pub mod push_events;
pub mod reconnect;
pub mod recovery;
pub mod region;
pub mod relay;
pub mod requester;
/// Scope strings — the canonical name of an account-plane scope
/// (`account-sync-plane.md` § Feeds and cursors → *The scope string*).
pub mod scope;
pub mod search;
/// The delegable-rung Secret-free conformance check
/// (`account-data-plane.md` § The audience ladder).
pub mod secret_free;
pub mod segments;
pub mod sessions;
pub mod share;
pub mod sidecar;
pub mod sig_domain;
pub mod spam;
pub mod stats;
pub mod subscriptions;
pub mod sync;
pub mod sync_row_verify;
pub mod sync_writer_sig;
/// A `Stream`/`Sink` in-memory transport pair over a bounded mpsc channel, for
/// driving [`dispatcher::RpcDispatcher`]/RPC-channel unit tests without a real
/// socket. Reached only through `cfg(test)` (this crate's own tests) or the
/// `test-helpers` feature (an external crate's own `#[cfg(test)]` code, via a
/// `[dev-dependencies]` entry) — never a plain dependency, so it cannot ride a
/// release build.
#[cfg(any(test, feature = "test-helpers"))]
pub mod test_transport;
/// The `fauna.tips.*` wire plane — the post-addressed tip attribution surface,
/// part of the same `payments` registry feature (`dynamic-features.md` §
/// Charter members: tips are a buy-side gate surface).
#[cfg(feature = "payments")]
pub mod tips;
pub mod tls;
#[cfg(feature = "tls-spki")]
pub mod tls_spki;
pub mod transport;
pub mod unknown;
pub mod web;
pub mod web_app_origin;
pub mod wrapped_blob;

pub use codec::{decode_strict, encode_canonical};
pub use dispatcher::{
    DISCONNECTED_CODE, DispatchError, OUTBOUND_CAPACITY, RpcCall, RpcDispatcher, RpcResult,
    TypedRequestError, encode_payload,
};
pub use envelope::{Cancel, Frame, FrameError, Push, Reply, Request, decode_frame, encode_frame};
pub use error::{LocalizedText, RpcError, RpcErrorAction};
pub use fauna_cbor::Value;
pub use kind::{KindRegistry, RpcKindMeta};
pub use protocol_kinds::{EchoReply, EchoRequest};
pub use push_events::{PushEvent, StaleSurfaces};
pub use requester::{
    KeyedRpcRequester, MaybeSendSync, NestSeamError, RpcErrorClass, RpcRequester,
    classify_rpc_error, nest_seam_error,
};
/// Re-exported so consumers (client wrappers, nest handlers) can build
/// the byte-buffer fields of the wire types without a direct
/// `serde_bytes` dependency.
pub use serde_bytes::ByteBuf;
pub use unknown::Unknown;
