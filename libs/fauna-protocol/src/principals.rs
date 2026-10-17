//! `fauna.principals.*` — the third-party principal roster
//! (`docs/goal/architecture/third-party.md` § The principal model).
//!
//! A **principal** is the nest-side identity of one approved Client ID Metadata
//! Document for one account: minted by the account's own consent, never by a
//! registrar or an admin (rule 1). These three USER-class kinds are its whole
//! app-facing surface today: the roster read the connected-apps page composes
//! from, the oracle's per-operation record for one principal, and **the one
//! verb** that ends a principal (rule 4).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

use crate::Value;

/// One roster row as `fauna.principals.list` answers it.
// `Default` so fixtures can grow this type with `..Default::default()` — the
// standing prevention for two branches independently adding a field to one wire
// struct and colliding on every hand-listed literal.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PrincipalInfo {
    /// What `fauna.principals.revoke` takes. Opaque, nest-minted.
    #[serde(with = "serde_bytes")]
    pub principal_id: Vec<u8>,
    /// The metadata document's URL — the principal's self-authenticating
    /// identity, rendered verbatim beside `label`.
    pub client_id: String,
    /// The X25519 public key the client attested at consent — what a
    /// capability grant to this principal is sealed to. Absent for a client
    /// that presented none (a standard OAuth client), which no grant can be
    /// minted to.
    #[serde(default)]
    pub holder_x25519: Option<ByteBuf>,
    /// `remote` | `device` today; `wasm` | `container` once hosted code runs.
    /// An app that meets a value it does not know renders the row without it.
    pub execution_form: String,
    /// The `ext.*` kinds the document's verified manifest declares
    /// (`third-party-kinds.md` § The manifest). Empty for a document without
    /// one.
    #[serde(default)]
    pub declared_kinds: Vec<String>,
    /// Space-delimited — the scope set the latest consent approved, in the one
    /// encoding every scope surface uses.
    pub granted_scopes: String,
    pub created_at: i64,
    /// Stamped at every consent that finds this row. Advisory, as the grant
    /// row's own `last_used_at` is.
    pub last_used_at: Option<i64>,
    /// The display name: the RESOLVED `client_name` at first consent — what
    /// the card showed — never the client's self-asserted string as-is.
    pub label: Option<String>,
    /// The manifest's Ed25519 publisher key, raw 32 bytes, as the latest
    /// consented document's verified manifest named it. Absent for a document
    /// without one.
    #[serde(default)]
    pub publisher_key: Option<ByteBuf>,
    /// The Ed25519 key the client attested to sign its own `ext.*` rows with
    /// (`fauna_writer_ed25519`, `third-party-kinds.md` § Principal write
    /// authority) — the writer a `content.write` grant to this principal
    /// names. Absent for a client that presented none, which is read-only over
    /// its kinds.
    #[serde(default)]
    pub writer_ed25519: Option<ByteBuf>,
    /// How many of this principal's OAuth grant families are live right now
    /// (the `list_grants` liveness predicate). Zero is a principal with no
    /// working connection — a forced session-secret rotation, a per-grant
    /// revoke, or an expiry left it — which the roster shows rather than hides.
    #[serde(default)]
    pub live_grants: u32,
    /// The consented document's validated `bridge` block
    /// (`third-party.md` § The manifest → *The `bridge` block*) — absent for a
    /// principal that is not a conversation bridge. Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge: Option<crate::kind_manifest::BridgeBlock>,
    /// The consented document's `service_auth` entries (`third-party.md`
    /// § The manifest) — the set the oracle's `atproto.service_auth` class is
    /// bounded by. Empty (and absent on the wire) for a document that declares
    /// none. Additive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service_auth: Vec<crate::kind_manifest::ServiceAuthEntry>,
    /// The consented document's `activitypub_peers` hosts (`third-party.md`
    /// § The manifest) — the set the oracle's `activitypub.http_sig` class
    /// signs to. Empty (and absent on the wire) for a document that declares
    /// none. Additive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activitypub_peers: Vec<String>,
    /// What the oracle's custodians did for this principal
    /// (`key-material-hierarchy.md` § Audience: deployment infrastructure →
    /// *The oracle*, the audit split) — absent for a principal no custodian
    /// has served. Merged across custodians, so a second custodian's record
    /// joins this one field. Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oracle: Option<OracleSummary>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The class an [`OracleOp`] carries for one write an external app made at
/// the ATProto PDS write funnel (`key-material-hierarchy.md` § *The oracle* →
/// *The ATProto classes ride the protocol's own door*). Carried by the
/// ATProto family, never an `identity.op` class: no
/// `fauna_core::identity_op::IdentityOpClass` parses it.
pub const ATPROTO_COMMIT_CLASS: &str = "atproto.commit";

/// One key operation an oracle custodian performed for a principal — a row of
/// the custodian-held per-operation record the audit split names.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OracleOp {
    /// Unix seconds.
    pub at: i64,
    /// The `identity.op` class the operation ran under (`nostr.sign_event`,
    /// …). The class names its custodian, so rows of several custodians
    /// interleave without a custodian field.
    pub class: String,
    /// The class's own detail: the event kind for `nostr.sign_event`, the
    /// method for the NIP-44 pair, `<method> <host>` for
    /// `activitypub.http_sig`. An app renders it verbatim.
    pub detail: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The oracle's per-principal summary a roster row carries, summed across the
/// custodians that served the principal.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OracleSummary {
    /// Operations admitted in the rate ceiling's current window — the
    /// custodian's own window position, so it reads what the ceiling reads.
    #[serde(default)]
    pub ops_in_window: u32,
    /// Every request a custodian served for this principal, the key-less ones
    /// (connect, ping, the public-key read) included — the same count the
    /// bunker roster's `use_count` keeps.
    #[serde(default)]
    pub requests_served: u64,
    /// The newest operation, absent before the first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_op: Option<OracleOp>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.principals.list` — the roster read (USER class, self-scoped).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListPrincipalsRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListPrincipalsReply {
    /// Oldest first.
    pub principals: Vec<PrincipalInfo>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// How many operation rows one `fauna.principals.ops` reply carries at most —
/// a hard constant: the record is the owner's recent-activity view, not an
/// archive.
pub const PRINCIPAL_OPS_PAGE: u32 = 256;

/// `fauna.principals.ops` — one principal's oracle record (USER class,
/// self-scoped): the per-operation audit the custodians keep.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListPrincipalOpsRequest {
    #[serde(with = "serde_bytes")]
    pub principal_id: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListPrincipalOpsReply {
    /// Newest first, at most [`PRINCIPAL_OPS_PAGE`]. Empty for a principal no
    /// custodian has served — and for one that is not the caller's, which
    /// reads exactly like it.
    pub ops: Vec<OracleOp>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.principals.revoke` — the one verb (USER class, self-scoped).
///
/// Deletes the roster row and, in the same act, ends every OAuth grant family
/// the principal accrued (one per ceremony) and every capability grant whose
/// holder is the principal's key.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RevokePrincipalRequest {
    #[serde(with = "serde_bytes")]
    pub principal_id: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RevokePrincipalReply {
    /// `false` when no such principal exists for the caller — already
    /// revoked, or never theirs. Not an error: the end state the caller asked
    /// for holds either way.
    pub revoked: bool,
    /// Grant families ended by this call.
    #[serde(default)]
    pub grants_ended: u32,
    /// Capability grants deleted by this call.
    #[serde(default)]
    pub capability_grants_ended: u32,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict, encode_canonical};

    #[test]
    fn an_absent_holder_key_round_trips_as_absent() {
        let info = PrincipalInfo {
            principal_id: vec![1; 16],
            client_id: "https://app.example/client.json".into(),
            execution_form: "remote".into(),
            granted_scopes: "atproto".into(),
            ..Default::default()
        };
        let back: PrincipalInfo = decode_strict(&encode_canonical(&info).unwrap()).unwrap();
        assert_eq!(back, info);
        assert_eq!(back.holder_x25519, None);
    }

    #[test]
    fn an_oracle_summary_round_trips_and_an_absent_one_stays_absent() {
        let op = OracleOp {
            at: 1_700_000_000,
            class: "nostr.sign_event".into(),
            detail: "1".into(),
            extra: Default::default(),
        };
        let info = PrincipalInfo {
            oracle: Some(OracleSummary {
                ops_in_window: 3,
                requests_served: 5,
                last_op: Some(op.clone()),
                extra: Default::default(),
            }),
            ..Default::default()
        };
        let back: PrincipalInfo = decode_strict(&encode_canonical(&info).unwrap()).unwrap();
        assert_eq!(back, info);
        let none: PrincipalInfo =
            decode_strict(&encode_canonical(&PrincipalInfo::default()).unwrap()).unwrap();
        assert_eq!(none.oracle, None);
        let reply = ListPrincipalOpsReply {
            ops: vec![op],
            extra: Default::default(),
        };
        let back: ListPrincipalOpsReply =
            decode_strict(&encode_canonical(&reply).unwrap()).unwrap();
        assert_eq!(back, reply);
    }

    #[test]
    fn a_holder_key_rides_as_bytes() {
        let info = PrincipalInfo {
            holder_x25519: Some(ByteBuf::from(vec![7; 32])),
            ..Default::default()
        };
        let back: PrincipalInfo = decode_strict(&encode_canonical(&info).unwrap()).unwrap();
        assert_eq!(back.holder_x25519, Some(ByteBuf::from(vec![7u8; 32])));
    }

    #[test]
    fn a_bridge_block_rides_dag_cbor_with_its_flags_and_mode_intact() {
        use crate::kind_manifest::{BridgeBlock, BridgeCapabilityValue};
        let bridge = BridgeBlock {
            id: "matrix".into(),
            glyph: "bridge".into(),
            address_grammar: "^@[^:]+:.+$".into(),
            capabilities: [
                (
                    "supports_reactions".to_string(),
                    BridgeCapabilityValue::Flag(true),
                ),
                (
                    "delivery_mode".to_string(),
                    BridgeCapabilityValue::Mode("Async".into()),
                ),
            ]
            .into_iter()
            .collect(),
            extra: Default::default(),
        };
        let info = PrincipalInfo {
            bridge: Some(bridge.clone()),
            ..Default::default()
        };
        let back: PrincipalInfo = decode_strict(&encode_canonical(&info).unwrap()).unwrap();
        assert_eq!(back.bridge, Some(bridge));
        // Absent stays absent — an older reader's bytes are unchanged.
        let none: PrincipalInfo =
            decode_strict(&encode_canonical(&PrincipalInfo::default()).unwrap()).unwrap();
        assert_eq!(none.bridge, None);
    }

    #[test]
    fn a_service_auth_set_rides_dag_cbor_and_an_empty_one_is_absent() {
        use crate::kind_manifest::ServiceAuthEntry;
        let entry = ServiceAuthEntry {
            aud: "did:web:api.bsky.app#bsky_appview".into(),
            lxm: vec!["app.bsky.feed.getFeedSkeleton".into()],
            extra: Default::default(),
        };
        let info = PrincipalInfo {
            service_auth: vec![entry.clone()],
            ..Default::default()
        };
        let back: PrincipalInfo = decode_strict(&encode_canonical(&info).unwrap()).unwrap();
        assert_eq!(back.service_auth, vec![entry]);
        // Empty stays off the wire — an older reader's bytes are unchanged.
        assert_eq!(
            encode_canonical(&PrincipalInfo::default()).unwrap(),
            encode_canonical(&PrincipalInfo {
                service_auth: vec![],
                ..Default::default()
            })
            .unwrap()
        );
        let none: PrincipalInfo =
            decode_strict(&encode_canonical(&PrincipalInfo::default()).unwrap()).unwrap();
        assert!(none.service_auth.is_empty());
    }

    /// A block a newer build wrote — a member this build does not name, and a
    /// capability whose value is neither a flag nor a string — decodes, reads
    /// as not offered, and is re-emitted unchanged on the wire and in the
    /// roster row's JSON (`transport.md` § Schema and forward-compat
    /// discipline, rules 3 and 4).
    #[test]
    fn a_newer_builds_bridge_block_is_carried_whole() {
        use crate::kind_manifest::{BridgeBlock, BridgeCapabilityValue};
        let json = serde_json::json!({
            "id": "matrix",
            "glyph": "bridge",
            "address_grammar": "^@[^:]+:.+$",
            "capabilities": {
                "supports_reactions": true,
                "delivery_mode": "Async",
                "max_attachment_count": 4,
                "reaction_sets": ["emoji", "custom"],
            },
            "relay_hint": { "tier": 2 },
        });
        let block: BridgeBlock = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(
            block.capabilities.get("supports_reactions"),
            Some(&BridgeCapabilityValue::Flag(true))
        );
        assert_eq!(
            block.capabilities.get("delivery_mode"),
            Some(&BridgeCapabilityValue::Mode("Async".into()))
        );
        assert!(matches!(
            block.capabilities.get("max_attachment_count"),
            Some(BridgeCapabilityValue::Other(_))
        ));
        assert!(matches!(
            block.capabilities.get("reaction_sets"),
            Some(BridgeCapabilityValue::Other(_))
        ));
        assert!(block.extra.contains_key("relay_hint"));
        // The stored form: JSON out equals JSON in.
        assert_eq!(serde_json::to_value(&block).unwrap(), json);
        // The wire form: a round trip keeps every carried value.
        let info = PrincipalInfo {
            bridge: Some(block.clone()),
            ..Default::default()
        };
        let bytes = encode_canonical(&info).unwrap();
        let back: PrincipalInfo = decode_strict(&bytes).unwrap();
        assert_eq!(back.bridge, Some(block));
        assert_eq!(encode_canonical(&back).unwrap(), bytes);
    }
}
