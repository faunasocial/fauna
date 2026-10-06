//! User-facing WS-RPC payload types for the Layer-3 Bridge Management
//! surface — what end-user clients call to list bridges, link/unlink,
//! adjust settings, manage follows.
//!
//! Distinct from `wrapped_blob.rs` + `bridge_routing.rs`, which are the
//! daemon-internal bridge↔nest plane (provision, fetch, IMAP, CalDAV).
//! Both planes share the `fauna.bridges.*` kind namespace and are gated
//! apart by `bridge_method_allowlist::is_permitted`'s `CallerClass`.
//!
//! Kind registry entries live in `kind.rs::register_bridges_ui_kinds`.
//!
//! Dynamic per-bridge values (a setting's current value, a setting
//! option's value, a follow's `extra` blob) are typed as `Value` so
//! the wire stays self-describing without forcing every provider to
//! pre-flatten to strings. The existing HTTP routes keep using
//! `serde_json::Value` until their twins are retired (tracked
//! internally); conversion lives
//! at the handler boundary (nest-side), not in this crate.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use fauna_cbor::Value;

// ── fauna.bridges.list ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListBridgesRequest {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListBridgesReply {
    pub bridges: Vec<BridgeStatus>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BridgeStatus {
    pub id: String,
    pub name: String,
    pub available: bool,
    pub linked: bool,
    pub identity: Option<BridgeIdentity>,
    pub mode: Option<String>,
    pub settings: Vec<BridgeSetting>,
    pub supports_follows: bool,
    /// The bridge's network lets an account decide who follows it, so its
    /// card shows the requests that are waiting
    /// (`fauna.bridges.list_follow_requests` /
    /// `fauna.bridges.resolve_follow_request`; `bridges.md` § Follow
    /// requests). `false` — the app renders no section.
    pub supports_follow_requests: bool,
    pub link_modes: Option<Vec<BridgeLinkMode>>,
    /// The bridge's declared glyph — the lowercase id of one `SourceGlyph`
    /// (`third-party.md` § The manifest → *The `bridge` block*) — set on a
    /// consented third-party conversation bridge's row; `None` on a
    /// first-party provider's, whose glyph every app already knows by its id.
    /// What `fauna_feed::classify_sources`' bridge roster reads. Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glyph: Option<String>,
    /// Set when `provider.status(...)` errored. `None` on success; on
    /// error all other fields fall back to the "available=true,
    /// linked=false, settings=[], link_modes=None" shape the existing
    /// HTTP route emits.
    pub error: Option<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BridgeIdentity {
    pub label: String,
    pub value: String,
    pub display: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BridgeSetting {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub setting_type: String,
    pub value: Value,
    pub options: Option<Vec<BridgeSettingOption>>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BridgeSettingOption {
    pub value: Value,
    pub label: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BridgeLinkMode {
    pub mode: String,
    pub label: String,
    pub client_action: Option<String>,
    pub platform: Option<String>,
    pub fields: Vec<BridgeLinkField>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BridgeLinkField {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub field_type: String,
    pub placeholder: Option<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.bridges.set_settings ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetSettingsRequest {
    pub bridge_id: String,
    /// Free-form per-bridge settings object. Wire-typed as `Value`
    /// so providers (Bluesky / ActivityPub / mail / …) don't have to
    /// pre-flatten to strings; the handler converts back to
    /// `serde_json::Value` at the `BridgeProvider::update_settings`
    /// boundary until the HTTP twin retires (T10).
    pub settings: Value,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetSettingsReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.bridges.link ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LinkRequest {
    pub bridge_id: String,
    pub mode: String,
    /// Per-mode parameters (OAuth handle, credential fields, …). Typed
    /// as `Value` so each provider's `BridgeProvider::link` can
    /// pick out the keys it cares about without forcing the wire to a
    /// pre-flattened string shape. Converted back to
    /// `serde_json::Value` at the handler boundary until the HTTP twin
    /// retires (T10).
    pub params: Value,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LinkReply {
    pub linked: bool,
    pub identity: Option<BridgeIdentity>,
    pub redirect_url: Option<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.bridges.link_challenge ────────────────────────────────────

/// Ask the provider for a proof-of-possession challenge the app must have an
/// external signer sign before `fauna.bridges.link` in `mode` will write a
/// row. Only modes whose identity is held outside the nest issue one (Nostr
/// `nip07` — the browser extension signs; `docs/goal/ui/nostr.md` § Errors &
/// edge cases → *Proof of possession*); any other (bridge, mode) refuses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LinkChallengeRequest {
    pub bridge_id: String,
    pub mode: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LinkChallengeReply {
    /// The nest-minted nonce. One outstanding challenge per (actor, bridge);
    /// a new one supersedes the last.
    pub challenge: String,
    /// Unix seconds after which the challenge is dead and `link` refuses it.
    pub expires_at: i64,
    /// What the external signer must sign, in the provider's own shape —
    /// `Value` for the same reason `LinkRequest::params` is: each provider
    /// defines it (Nostr: the unsigned kind-22242 event, `{kind, created_at,
    /// tags, content}`, exactly what `window.nostr.signEvent` takes).
    pub payload: Value,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.bridges.unlink ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UnlinkRequest {
    pub bridge_id: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UnlinkReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.bridges.list_follows ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListFollowsRequest {
    pub bridge_id: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListFollowsReply {
    pub follows: Vec<BridgeFollow>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BridgeFollow {
    pub id: String,
    pub petname: Option<String>,
    pub created_at: Option<i64>,
    /// Provider-specific extra metadata. Unlike the HTTP twin's
    /// `#[serde(skip_serializing_if = "Option::is_none")]` shape, the
    /// typed wire always carries the slot as `null` when absent — the
    /// CBOR map's key set stays stable across bridges.
    pub extra: Option<Value>,
    /// Rule 4's forward-compat catch-all (transport.md § Schema and
    /// forward-compat discipline).
    ///
    /// Spelled `unknown_keys`, not the house `extra`, because this struct's
    /// `extra` above is a *domain* field — provider-specific follow metadata —
    /// that owns the name. The collision is Rust-identifier-only, never wire:
    /// `#[serde(flatten)]` splats its map into the parent and never emits under
    /// its own field name, so on decode the key `"extra"` still binds to the
    /// domain field above and only the *other* unknown keys land here. Nothing
    /// about the encoding changes.
    #[serde(flatten, default)]
    pub unknown_keys: BTreeMap<String, Value>,
}

// ── fauna.bridges.add_follow ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AddFollowRequest {
    pub bridge_id: String,
    pub id: String,
    pub petname: Option<String>,
    /// Provider-specific extra metadata attached to the follow entry.
    /// Typed as `Value` so the wire stays self-describing — converted
    /// back to `serde_json::Value` at the `BridgeProvider::add_follow`
    /// boundary until the HTTP twin retires (T10).
    pub extra: Option<Value>,
    /// Rule 4's forward-compat catch-all, spelled `unknown_keys` for the same
    /// reason as [`BridgeFollow::unknown_keys`]: this struct's `extra` is a
    /// domain field, and a flattened field never emits under its own name, so
    /// the two coexist with no wire change.
    #[serde(flatten, default)]
    pub unknown_keys: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AddFollowReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.bridges.remove_follow ─────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RemoveFollowRequest {
    pub bridge_id: String,
    pub follow_id: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RemoveFollowReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.bridges.list_follow_requests ──────────────────────────────

/// List the follow requests waiting on the calling actor's account on the
/// named bridge — only a bridge whose `BridgeStatus.supports_follow_requests`
/// is true answers it (`bridges.md` § Follow requests).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListFollowRequestsRequest {
    pub bridge_id: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListFollowRequestsReply {
    pub requests: Vec<BridgeFollowRequest>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One waiting follow request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BridgeFollowRequest {
    /// The requester, in the form [`BridgeFollow::id`] uses on this bridge —
    /// what `fauna.bridges.resolve_follow_request` takes back.
    pub id: String,
    /// The requester's display name, when the nest knows it.
    pub name: Option<String>,
    /// Unix seconds the request arrived.
    pub requested_at: Option<i64>,
    /// Provider-specific extra metadata, the slot always present as on
    /// [`BridgeFollow::extra`]. One key is shared across providers: `handle`,
    /// the requester's address on that network in the spelling a person
    /// types (`@user@host`), when the nest knows it — a row shows it in place
    /// of `id`.
    pub extra: Option<Value>,
    /// Rule 4's forward-compat catch-all, spelled `unknown_keys` for the same
    /// reason as [`BridgeFollow::unknown_keys`].
    #[serde(flatten, default)]
    pub unknown_keys: BTreeMap<String, Value>,
}

// ── fauna.bridges.resolve_follow_request ────────────────────────────

/// Approve or refuse one waiting follow request. Idempotent: answering a
/// request that is already gone (withdrawn, or answered from another device)
/// succeeds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolveFollowRequestRequest {
    pub bridge_id: String,
    /// [`BridgeFollowRequest::id`] of the request being answered.
    pub id: String,
    /// `true` approves, `false` refuses.
    pub approve: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolveFollowRequestReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.bridges.feeds.* — cross-bridge feed-subscription surface ──
//
// Distinct from `list_follows` / `add_follow` / `remove_follow` above:
// those manage per-bridge follow lists (one external account at a
// time). The `feeds.*` kinds manage cross-bridge feed subscriptions
// (e.g. a Bluesky custom feed `at://…/app.bsky.feed.generator/whats-hot`)
// whose posts flow into the unified timeline. Backed by the
// `bridge_feed_subscriptions` table, not the `BridgeProvider` trait.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedSubscription {
    pub id: i64,
    pub bridge: String,
    pub feed_uri: String,
    pub name: String,
    /// Creation epoch in milliseconds (matches the DB's `created_at`
    /// column and the HTTP twin's JSON shape).
    pub created_at: i64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListFeedsRequest {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListFeedsReply {
    pub subscriptions: Vec<FeedSubscription>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateFeedRequest {
    pub bridge: String,
    pub feed_uri: String,
    pub name: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateFeedReply {
    pub id: i64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeleteFeedRequest {
    pub id: i64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeleteFeedReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};
    use std::collections::BTreeMap;

    fn sample_reply() -> ListBridgesReply {
        ListBridgesReply {
            bridges: vec![
                BridgeStatus {
                    id: "bluesky".into(),
                    name: "Bluesky".into(),
                    available: true,
                    linked: true,
                    identity: Some(BridgeIdentity {
                        label: "Handle".into(),
                        value: "did:plc:abc123".into(),
                        display: "alice.bsky.social".into(),
                        extra: Default::default(),
                    }),
                    mode: Some("personal".into()),
                    settings: vec![BridgeSetting {
                        key: "write_through".into(),
                        label: "Crosspost to Bluesky".into(),
                        setting_type: "enum".into(),
                        value: Value::Integer(1),
                        options: Some(vec![
                            BridgeSettingOption {
                                value: Value::Integer(0),
                                label: "Off".into(),
                                extra: Default::default(),
                            },
                            BridgeSettingOption {
                                value: Value::Integer(1),
                                label: "On".into(),
                                extra: Default::default(),
                            },
                        ]),
                        extra: Default::default(),
                    }],
                    supports_follows: true,
                    supports_follow_requests: false,
                    link_modes: None,
                    glyph: None,
                    error: None,
                    extra: Default::default(),
                },
                BridgeStatus {
                    id: "activitypub".into(),
                    name: "ActivityPub".into(),
                    available: true,
                    linked: false,
                    identity: None,
                    mode: None,
                    settings: vec![],
                    supports_follows: true,
                    supports_follow_requests: true,
                    link_modes: Some(vec![BridgeLinkMode {
                        mode: "oauth".into(),
                        label: "Connect via Mastodon".into(),
                        client_action: Some("oauth_redirect".into()),
                        platform: None,
                        fields: vec![BridgeLinkField {
                            key: "instance".into(),
                            label: "Instance".into(),
                            field_type: "text".into(),
                            placeholder: Some("mastodon.social".into()),
                            extra: Default::default(),
                        }],
                        extra: Default::default(),
                    }]),
                    glyph: None,
                    error: None,
                    extra: Default::default(),
                },
            ],
            extra: Default::default(),
        }
    }

    #[test]
    fn list_bridges_request_round_trips() {
        let req = ListBridgesRequest {};
        let bytes = encode_canonical(&req).unwrap();
        let decoded: ListBridgesRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn list_bridges_reply_round_trips() {
        let reply = sample_reply();
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: ListBridgesReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn list_bridges_reply_canonical_re_encodes_identically() {
        let reply = sample_reply();
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: ListBridgesReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2, "canonical re-encode must be byte-identical");
    }

    fn sample_set_settings_request() -> SetSettingsRequest {
        SetSettingsRequest {
            bridge_id: "bluesky".into(),
            settings: Value::Map(BTreeMap::from([
                ("write_through".to_string(), Value::Bool(true)),
                ("poll_interval_s".to_string(), Value::Integer(60)),
            ])),
            extra: Default::default(),
        }
    }

    #[test]
    fn set_settings_request_round_trips() {
        let req = sample_set_settings_request();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: SetSettingsRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn set_settings_reply_round_trips() {
        let reply = SetSettingsReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: SetSettingsReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn set_settings_request_canonical_re_encodes_identically() {
        let req = sample_set_settings_request();
        let bytes1 = encode_canonical(&req).unwrap();
        let decoded: SetSettingsRequest = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    fn sample_list_follows_reply() -> ListFollowsReply {
        ListFollowsReply {
            follows: vec![
                BridgeFollow {
                    id: "did:plc:abc".into(),
                    petname: Some("Alice".into()),
                    created_at: Some(1_700_000_000),
                    extra: Some(Value::Map(BTreeMap::from([(
                        "handle".to_string(),
                        Value::String("alice.bsky.social".into()),
                    )]))),
                    unknown_keys: Default::default(),
                },
                BridgeFollow {
                    id: "did:plc:def".into(),
                    petname: None,
                    created_at: None,
                    extra: None,
                    unknown_keys: Default::default(),
                },
            ],
            extra: Default::default(),
        }
    }

    #[test]
    fn list_follows_request_round_trips() {
        let req = ListFollowsRequest {
            bridge_id: "bluesky".into(),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: ListFollowsRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn list_follows_reply_round_trips() {
        let reply = sample_list_follows_reply();
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: ListFollowsReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn list_follows_reply_canonical_re_encodes_identically() {
        let reply = sample_list_follows_reply();
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: ListFollowsReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    fn sample_link_request() -> LinkRequest {
        LinkRequest {
            bridge_id: "bluesky".into(),
            mode: "oauth".into(),
            params: Value::Map(BTreeMap::from([(
                "handle".to_string(),
                Value::String("alice.bsky.social".into()),
            )])),
            extra: Default::default(),
        }
    }

    #[test]
    fn link_request_round_trips() {
        let req = sample_link_request();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: LinkRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn link_request_canonical_re_encodes_identically() {
        let req = sample_link_request();
        let bytes1 = encode_canonical(&req).unwrap();
        let decoded: LinkRequest = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn link_reply_with_redirect_round_trips() {
        let reply = LinkReply {
            linked: false,
            identity: None,
            redirect_url: Some("https://bsky.social/oauth/authorize?…".into()),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: LinkReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn link_reply_with_identity_round_trips() {
        let reply = LinkReply {
            linked: true,
            identity: Some(BridgeIdentity {
                label: "Handle".into(),
                value: "did:plc:abc".into(),
                display: "alice.bsky.social".into(),
                extra: Default::default(),
            }),
            redirect_url: None,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: LinkReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn link_reply_canonical_re_encodes_identically() {
        let reply = LinkReply {
            linked: true,
            identity: Some(BridgeIdentity {
                label: "Handle".into(),
                value: "did:plc:abc".into(),
                display: "alice.bsky.social".into(),
                extra: Default::default(),
            }),
            redirect_url: None,
            extra: Default::default(),
        };
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: LinkReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    fn sample_link_challenge_reply() -> LinkChallengeReply {
        // The Nostr provider's payload: an unsigned kind-22242 AUTH event.
        let tag = |name: &str, value: &str| {
            Value::List(vec![
                Value::String(name.into()),
                Value::String(value.into()),
            ])
        };
        LinkChallengeReply {
            challenge: "9f1e2d3c4b5a69788796a5b4c3d2e1f0".into(),
            expires_at: 1_700_000_300,
            payload: Value::Map(BTreeMap::from([
                ("kind".to_string(), Value::Integer(22242)),
                ("created_at".to_string(), Value::Integer(1_700_000_000)),
                (
                    "tags".to_string(),
                    Value::List(vec![
                        tag("relay", "wss://nest.example/nostr"),
                        tag("challenge", "9f1e2d3c4b5a69788796a5b4c3d2e1f0"),
                    ]),
                ),
                ("content".to_string(), Value::String(String::new())),
            ])),
            extra: Default::default(),
        }
    }

    #[test]
    fn link_challenge_request_round_trips() {
        let req = LinkChallengeRequest {
            bridge_id: "nostr".into(),
            mode: "nip07".into(),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: LinkChallengeRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes, bytes2);
    }

    #[test]
    fn link_challenge_reply_round_trips_and_re_encodes_identically() {
        let reply = sample_link_challenge_reply();
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: LinkChallengeReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes, bytes2);
    }

    #[test]
    fn unlink_request_round_trips() {
        let req = UnlinkRequest {
            bridge_id: "bluesky".into(),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: UnlinkRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn unlink_reply_round_trips() {
        let reply = UnlinkReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: UnlinkReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn unlink_request_canonical_re_encodes_identically() {
        let req = UnlinkRequest {
            bridge_id: "bluesky".into(),
            extra: Default::default(),
        };
        let bytes1 = encode_canonical(&req).unwrap();
        let decoded: UnlinkRequest = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    fn sample_add_follow_request() -> AddFollowRequest {
        AddFollowRequest {
            bridge_id: "bluesky".into(),
            id: "did:plc:abc".into(),
            petname: Some("Alice".into()),
            extra: Some(Value::Map(BTreeMap::from([(
                "handle".to_string(),
                Value::String("alice.bsky.social".into()),
            )]))),
            unknown_keys: Default::default(),
        }
    }

    #[test]
    fn add_follow_request_round_trips() {
        let req = sample_add_follow_request();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: AddFollowRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn add_follow_request_without_optional_fields_round_trips() {
        let req = AddFollowRequest {
            bridge_id: "nostr".into(),
            id: "npub1xyz".into(),
            petname: None,
            extra: None,
            unknown_keys: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: AddFollowRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn add_follow_request_canonical_re_encodes_identically() {
        let req = sample_add_follow_request();
        let bytes1 = encode_canonical(&req).unwrap();
        let decoded: AddFollowRequest = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn add_follow_reply_round_trips() {
        let reply = AddFollowReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: AddFollowReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn remove_follow_request_round_trips() {
        let req = RemoveFollowRequest {
            bridge_id: "bluesky".into(),
            follow_id: "did:plc:abc".into(),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: RemoveFollowRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn remove_follow_request_canonical_re_encodes_identically() {
        let req = RemoveFollowRequest {
            bridge_id: "bluesky".into(),
            follow_id: "did:plc:abc".into(),
            extra: Default::default(),
        };
        let bytes1 = encode_canonical(&req).unwrap();
        let decoded: RemoveFollowRequest = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn remove_follow_reply_round_trips() {
        let reply = RemoveFollowReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: RemoveFollowReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    /// `supports_follow_requests` is required: every nest states it for every
    /// bridge, so a `BridgeStatus` without the key is refused at decode
    /// (`bridges.md` § Follow requests).
    #[test]
    fn a_bridge_status_without_the_follow_requests_flag_is_refused() {
        let reply = sample_reply();
        let mut bare = serde_json::to_value(&reply.bridges[1]).unwrap();
        assert_eq!(bare["supports_follow_requests"], true);
        bare.as_object_mut()
            .unwrap()
            .remove("supports_follow_requests");
        assert!(serde_json::from_value::<BridgeStatus>(bare).is_err());
    }

    #[test]
    fn list_follow_requests_round_trips() {
        let req = ListFollowRequestsRequest {
            bridge_id: "activitypub".into(),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: ListFollowRequestsRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);

        let reply = ListFollowRequestsReply {
            requests: vec![
                BridgeFollowRequest {
                    id: "https://remote.example/users/bob".into(),
                    name: Some("Bob".into()),
                    requested_at: Some(1_700_000_000),
                    extra: Some(Value::Map(BTreeMap::from([(
                        "handle".to_string(),
                        Value::String("@bob@remote.example".into()),
                    )]))),
                    unknown_keys: Default::default(),
                },
                BridgeFollowRequest {
                    id: "https://remote.example/users/carol".into(),
                    name: None,
                    requested_at: None,
                    extra: None,
                    unknown_keys: Default::default(),
                },
            ],
            extra: Default::default(),
        };
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: ListFollowRequestsReply = decode(&bytes1).unwrap();
        assert_eq!(reply, decoded);
        assert_eq!(bytes1, encode_canonical(&decoded).unwrap());
    }

    #[test]
    fn resolve_follow_request_round_trips() {
        for approve in [true, false] {
            let req = ResolveFollowRequestRequest {
                bridge_id: "activitypub".into(),
                id: "https://remote.example/users/bob".into(),
                approve,
                extra: Default::default(),
            };
            let bytes1 = encode_canonical(&req).unwrap();
            let decoded: ResolveFollowRequestRequest = decode(&bytes1).unwrap();
            assert_eq!(req, decoded);
            assert_eq!(bytes1, encode_canonical(&decoded).unwrap());
        }
        let reply = ResolveFollowRequestReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: ResolveFollowRequestReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    fn sample_list_feeds_reply() -> ListFeedsReply {
        ListFeedsReply {
            subscriptions: vec![
                FeedSubscription {
                    id: 17,
                    bridge: "bluesky".into(),
                    feed_uri: "at://did:plc:abc/app.bsky.feed.generator/whats-hot".into(),
                    name: "What's Hot".into(),
                    created_at: 1_700_000_000_000,
                    extra: Default::default(),
                },
                FeedSubscription {
                    id: 42,
                    bridge: "bluesky".into(),
                    feed_uri: "at://did:plc:xyz/app.bsky.feed.generator/discover".into(),
                    name: "Discover".into(),
                    created_at: 1_700_000_500_000,
                    extra: Default::default(),
                },
            ],
            extra: Default::default(),
        }
    }

    #[test]
    fn list_feeds_request_round_trips() {
        let req = ListFeedsRequest {};
        let bytes = encode_canonical(&req).unwrap();
        let decoded: ListFeedsRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn list_feeds_reply_round_trips() {
        let reply = sample_list_feeds_reply();
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: ListFeedsReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn list_feeds_reply_canonical_re_encodes_identically() {
        let reply = sample_list_feeds_reply();
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: ListFeedsReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    fn sample_create_feed_request() -> CreateFeedRequest {
        CreateFeedRequest {
            bridge: "bluesky".into(),
            feed_uri: "at://did:plc:abc/app.bsky.feed.generator/whats-hot".into(),
            name: "What's Hot".into(),
            extra: Default::default(),
        }
    }

    #[test]
    fn create_feed_request_round_trips() {
        let req = sample_create_feed_request();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: CreateFeedRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn create_feed_request_canonical_re_encodes_identically() {
        let req = sample_create_feed_request();
        let bytes1 = encode_canonical(&req).unwrap();
        let decoded: CreateFeedRequest = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn create_feed_reply_round_trips() {
        let reply = CreateFeedReply {
            id: 17,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: CreateFeedReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn delete_feed_request_round_trips() {
        let req = DeleteFeedRequest {
            id: 17,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: DeleteFeedRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn delete_feed_reply_round_trips() {
        let reply = DeleteFeedReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: DeleteFeedReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    /// `BridgeFollow` is the one shape in the crate where a **domain** field is
    /// named `extra` and rule 4's catch-all sits beside it under another Rust
    /// name. The two must not collide: `"extra"` has to keep binding to the
    /// domain field, and only the *other* unknown keys may land in the
    /// catch-all. Decoding a map a newer peer wrote is the only way to see it —
    /// a round-trip of our own value never populates the catch-all at all.
    #[test]
    fn a_domain_extra_and_the_catch_all_do_not_collide() {
        let from_a_newer_peer = BTreeMap::from([
            ("id".to_string(), Value::String("did:plc:abc".into())),
            ("petname".to_string(), Value::String("alice".into())),
            ("created_at".to_string(), Value::Integer(1_700_000_000)),
            // The domain field: provider metadata the bridge attached.
            ("extra".to_string(), Value::String("provider-blob".into())),
            // A key this binary has never heard of.
            ("follow_kind".to_string(), Value::String("mutual".into())),
        ]);
        let bytes = encode_canonical(&Value::Map(from_a_newer_peer)).unwrap();

        let decoded: BridgeFollow = decode(&bytes).unwrap();
        assert_eq!(
            decoded.extra,
            Some(Value::String("provider-blob".into())),
            "`extra` must still bind to the domain field, not the catch-all"
        );
        assert_eq!(
            decoded.unknown_keys.get("follow_kind"),
            Some(&Value::String("mutual".into())),
            "the unknown key must land in the catch-all"
        );
        assert!(
            !decoded.unknown_keys.contains_key("extra"),
            "the domain field must not be swept up as an unknown key"
        );

        // And re-encoding preserves both — the relay property rule 4 is for.
        let round_tripped: BridgeFollow = decode(&encode_canonical(&decoded).unwrap()).unwrap();
        assert_eq!(round_tripped, decoded);
    }
}
