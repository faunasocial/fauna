//! User-facing WS-RPC payload types for the nest's Nostr kinds: the NIP-46
//! bunker control plane (`fauna.nostr.bunker.*`), the zap-signer roster
//! (`fauna.nostr.zap_signers.*`) and the protocol-native content kinds
//! (`nostr.<area>.<verb>`). Each family's section below owns its naming and
//! semantics.
//!
//! Direct messages have no types here: a Nostr DM is a bridged room, and its
//! wire is [`crate::bridged_conversations`] (`docs/goal/ui/nostr.md`
//! § Implementation status today → DMs).

use crate::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ── fauna.nostr.bunker.* — the NIP-46 bunker control plane ─────────
//
// `docs/goal/ui/nostr.md` § The nest as the user's NIP-46 signer (ratified
// 2026-07-19): a nest-enforced roster with mint/revoke verbs — deliberately
// NOT `fauna.bridges.set_settings` (this is not a settings blob) and NOT the
// capability-grant plane (the app receives no key material; the nest is the
// enforcement point). All four kinds are **User-class, caller-scoped**: every
// query keys on the authenticated connection's actor. Consumed by the Nostr
// page's *Connected apps* section on all 7 apps (`nostr-bunker-*` IDs).

/// Request for `fauna.nostr.bunker.create_invite` — mint a pending connection
/// with a one-time secret. Empty: the subject is the caller's account.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateBunkerInviteRequest {
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.nostr.bunker.create_invite`. The secret appears here
/// once — only its hash rests on the nest (one-time reveal, the
/// mail-credentials precedent).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateBunkerInviteReply {
    /// Roster row id of the pending connection.
    pub connection_id: i64,
    /// The full `bunker://<signer-pubkey>?relay=wss://<domain>/nostr&secret=…`
    /// string the client renders as text + copy + QR
    /// (`nostr-bunker-connect-string`). Composed nest-side — single source of
    /// truth for the relay URL.
    pub connect_string: String,
    /// The account's dedicated bunker signer pubkey (hex) — NOT the user's
    /// pubkey (§ Signer identity).
    pub signer_pubkey: String,
    /// Epoch-seconds when the unredeemed invite lapses (hard-constant TTL).
    pub expires_at: u64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.nostr.bunker.bind` — a third-party **principal** binds
/// the NIP-46 client key it will speak to the account's signer with (TP11,
/// `docs/goal/ui/nostr.md` § The nest as the user's NIP-46 signer → *A
/// principal as a bunker client*). Principal-only: no actor class holds the
/// kind. A re-bind re-points the principal's one client row.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BindBunkerClientRequest {
    /// The principal's NIP-46 client pubkey — 64 lowercase hex.
    pub client_pubkey: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.nostr.bunker.bind` — the
/// [`CreateBunkerInviteReply`] shape minus the secret and the expiry: a bound
/// client needs no one-time secret (its key is already pinned) and the
/// binding lasts as long as the principal and its grant do.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BindBunkerClientReply {
    /// `bunker://<signer-pubkey>?relay=wss://<domain>/nostr` — composed
    /// nest-side, the same relay resolution `create_invite` uses.
    pub connect_string: String,
    /// The account's dedicated bunker signer pubkey (hex) — NOT the user's
    /// pubkey (§ Signer identity).
    pub signer_pubkey: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.nostr.bunker.list` — the caller's connection roster.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListBunkerAppsRequest {
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One bunker connection row (`nostr-bunker-app-item`): pending (invite
/// outstanding) or active. Revoked tombstones are not served.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BunkerAppEntry {
    /// Roster row id (the revoke/set_label handle).
    pub id: i64,
    /// The connected app's client pubkey (hex); absent while pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_pubkey: Option<String>,
    /// User-editable label (the bunker flow transports no app name; empty
    /// until the user names it).
    pub label: String,
    /// `"pending"` or `"active"`.
    pub status: String,
    /// Epoch-seconds the row was created.
    pub created_at: u64,
    /// Epoch-seconds of the most recent served request; absent before first
    /// use. With `use_count`, the roster's audit surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<u64>,
    /// Requests served over this connection.
    pub use_count: u64,
    /// Epoch-seconds when the row lapses (invite TTL while pending; sliding
    /// idle expiry once active).
    pub expires_at: u64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.nostr.bunker.list`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListBunkerAppsReply {
    pub apps: Vec<BunkerAppEntry>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.nostr.bunker.revoke` — immediate disconnect of one
/// connection (authorization is evaluated per request; no cached authority
/// survives).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RevokeBunkerAppRequest {
    /// Roster row id to revoke.
    pub connection_id: i64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.nostr.bunker.revoke`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RevokeBunkerAppReply {
    /// `false` when no live caller-owned row matched (already revoked, or
    /// not the caller's).
    pub revoked: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.nostr.bunker.set_label` — name a connection row.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetBunkerAppLabelRequest {
    /// Roster row id to label.
    pub connection_id: i64,
    /// The new label.
    pub label: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.nostr.bunker.set_label`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetBunkerAppLabelReply {
    /// `false` when no live caller-owned row matched.
    pub updated: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.nostr.zap_signers.* — the NIP-57 trust root ──────────────
//
// The payee-designated signer list that decides which kind-9735 zap receipts
// this nest may believe (`docs/goal/behavior/monetization.md` § Zap receipts
// — the trust model). Which wallet/LNURL provider a payee trusts is a
// genuine user choice, so it is app UI + nest state, never a config file
// (§ Product invariants). All three kinds are User-class and caller-scoped,
// mirroring the `fauna.nostr.bunker.*` roster above.
//
// Gated on the `zaps` registry feature (`dynamic-features.md` § Charter
// members — a subset member of `payments`). The gate is per-TYPE rather than
// on the module, because these sit between the bunker roster above and the
// badge/publish surfaces below, neither of which is a registry member.

/// One designated zap signer (`nostr-zap-signer-item`).
#[cfg(feature = "zaps")]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ZapSignerEntry {
    /// Roster row id.
    pub id: i64,
    /// The designated signer's pubkey (64 lowercase hex) — in practice the
    /// payee's LNURL/wallet provider's `nostrPubkey`.
    pub signer_pubkey: String,
    /// User-editable label naming the provider ("Alby", "my node").
    pub label: String,
    /// Epoch-seconds the designation was made.
    pub created_at: u64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.nostr.zap_signers.list` — the caller's trust root.
#[cfg(feature = "zaps")]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListZapSignersRequest {
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.nostr.zap_signers.list`.
///
/// An empty list is the meaningful out-of-the-box state, not a missing
/// answer: it means this payee believes no zap receipt at all.
#[cfg(feature = "zaps")]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListZapSignersReply {
    pub signers: Vec<ZapSignerEntry>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.nostr.zap_signers.add` — designate a signer.
#[cfg(feature = "zaps")]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AddZapSignerRequest {
    /// The signer pubkey to trust (64 hex; normalized lowercase nest-side).
    pub signer_pubkey: String,
    /// Optional label naming the provider.
    #[serde(default)]
    pub label: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.nostr.zap_signers.add` — the stored row, so the client
/// renders the normalized pubkey rather than whatever case it sent.
#[cfg(feature = "zaps")]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AddZapSignerReply {
    pub signer: ZapSignerEntry,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.nostr.zap_signers.remove` — undesignate a signer.
///
/// Keyed by the pubkey rather than a row id: the pubkey is the natural
/// identity of a designation, so removal is idempotent and a client that
/// only knows "stop trusting this key" needs no roster round-trip first.
#[cfg(feature = "zaps")]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoveZapSignerRequest {
    /// The signer pubkey to stop trusting.
    pub signer_pubkey: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.nostr.zap_signers.remove`.
#[cfg(feature = "zaps")]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoveZapSignerReply {
    /// `false` when the caller had not designated that signer.
    pub removed: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── nostr.* protocol-native content kinds ──────────────────────────
//
// The prefix-less `nostr.<area>.<verb>` family this module's header reserves
// for the genuinely protocol-specific content surfaces — the WS-RPC
// successors to the deleted `/api/v1/nostr/{zaps,badges,publish-signed}`
// HTTP routes (the native-content HTTP→WS-RPC rip, user-directed 2026-07-22;
// `docs/goal/ui/nostr.md` § WS-RPC migration contract). Naming follows the
// `bluesky.feed.thread` precedent (`bridges.md` § Bluesky-native thread
// view): a bridge's protocol-unique consume-side surface keeps a
// `<bridge>.*` kind, while nest-backed feeds stay `fauna.nostr.*`.
//
// All three are **User-class**; `zaps.total` / `badges.list` are pure local
// reads over the sync worker's ingested NIP-57/NIP-58 rows, and
// `events.publish_signed` is the NIP-07 leg — the one surface where the
// client, not the nest, holds the signature (a browser extension signed the
// event), so the nest verifies + relays without ever seeing a key.

/// Request for `nostr.zaps.total` — aggregate zap receipts (NIP-57) ingested
/// for one Nostr event.
///
/// `zaps`-gated: the display read is a zap surface, while its two siblings
/// (`badges.list`, `events.publish_signed`) are not registry members and stay
/// unconditional.
#[cfg(feature = "zaps")]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct NostrZapTotalRequest {
    /// The zapped Nostr event id (hex).
    pub event_id: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `nostr.zaps.total`. An unknown event id is zeros, not an error
/// (mirrors the HTTP twin's contract).
#[cfg(feature = "zaps")]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct NostrZapTotalReply {
    /// Sum of parsed bolt11 amounts across the event's zap receipts, msats.
    pub total_msats: i64,
    /// Number of zap receipts counted.
    pub zap_count: i64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `nostr.badges.list` — badge awards (NIP-58) ingested for one
/// pubkey.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListNostrBadgesRequest {
    /// The awardee's Nostr public key (32-byte hex).
    pub pubkey: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One badge award row. Mirrors `bins/fauna-nest/src/nostr/db.rs::Badge`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct NostrBadgeItem {
    /// The badge definition's identifier (the NIP-58 `a`-tag `d` value).
    pub badge_id: String,
    /// Human badge name, when the definition carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge_name: Option<String>,
    /// Badge image URL, when the definition carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge_image: Option<String>,
    /// Epoch-seconds award time.
    pub created_at: i64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `nostr.badges.list` (newest first).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListNostrBadgesReply {
    pub badges: Vec<NostrBadgeItem>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `nostr.events.publish_signed` — relay-enqueue an event the
/// **client** signed (the NIP-07 browser-extension leg: the extension holds
/// the key; the nest verifies the signature, checks the pubkey is the
/// caller's linked account, and enqueues to the account's relay list).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublishSignedNostrEventRequest {
    /// The signed event as NIP-01 wire JSON (the exact object the extension
    /// returned). Carried as a JSON string — the event is an
    /// external-protocol object the nest re-parses and signature-verifies;
    /// mirroring it field-by-field on our wire would just be a second parser
    /// to keep honest.
    pub event_json: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `nostr.events.publish_signed` — success is the (empty) reply
/// itself; rejection rides `RpcError` (bad signature / pubkey not linked),
/// mirroring the HTTP twin's 202-no-body vs 400/403 split.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublishSignedNostrEventReply {
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}
