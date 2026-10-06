// ActivityPub protocol types.

use serde::{Deserialize, Deserializer, Serialize};

/// The ActivityPub public addressing constant.
pub const AP_PUBLIC: &str = "https://www.w3.org/ns/activitystreams#Public";

/// Deserialize an AS2 multi-valued property that may arrive as a *single* value.
///
/// JSON-LD compaction drops the array wrapper when a property holds exactly one
/// value, so `"tag": {…}` and `"tag": [{…}]` are the same document, as are
/// `"to": "…#Public"` and `"to": ["…#Public"]`. Every AS2 property below is
/// multi-valued in the vocabulary, so a peer may legally send either form and a
/// bare `Vec<T>` field rejects half of them.
///
/// **This is not hypothetical strictness.** Observed against real GoToSocial
/// 0.22.1 (2026-07-22): a reply mentioning one account compacts to a bare `tag`
/// object, and because serde fails the *whole* struct on one field, every
/// inbound reply from that peer was rejected with `400` and the opaque
/// `invalid type: map, expected a sequence` — while Mastodon, which always
/// emits arrays, was unaffected. One lenient peer is a weak oracle.
pub fn one_or_many<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany<T> {
        // `Many` first: a JSON array must not be offered to `One` (for a `T`
        // that could itself deserialize from a sequence, it would win).
        Many(Vec<T>),
        One(T),
    }

    Ok(match Option::<OneOrMany<T>>::deserialize(deserializer)? {
        None => Vec::new(),
        Some(OneOrMany::Many(items)) => items,
        Some(OneOrMany::One(item)) => vec![item],
    })
}

/// Deserialize an AS2 single-value *reference* that may be spelled three ways.
///
/// A property whose value is one object (`attributedTo`, `actor`, an object's
/// `id`) may arrive as a bare IRI string (`"https://x/users/a"`), as an object
/// carrying an `id` (`{"type":"Person","id":"https://x/users/a"}`), or as an
/// array of either (some servers wrap a single reference). All three name the
/// same actor, so a plain `String` field rejects two of the three — the exact
/// class of break `one_or_many` fixes for multi-valued properties, here for a
/// single-valued reference.
///
/// Kept *required* by erroring when no non-empty id can be extracted: the one
/// field this guards (`ApNote.attributed_to`) mints the stored post's author,
/// and a Note with no author identity has nothing to ingest. **This governs
/// parsing, not trust** — the relationship/reaction gates key on the top-level
/// signature-verified `actor`, never this self-claimed field, so widening the
/// accepted *shape* cannot let a Note claim an author the signature never
/// covered (the author it mints is a display attribution, already unverified in
/// the string form this replaces).
fn id_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;

    fn extract(v: &serde_json::Value) -> Option<String> {
        match v {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Object(m) => m.get("id").and_then(|i| i.as_str()).map(String::from),
            serde_json::Value::Array(a) => a.first().and_then(extract),
            _ => None,
        }
    }

    let v = serde_json::Value::deserialize(deserializer)?;
    extract(&v).filter(|s| !s.is_empty()).ok_or_else(|| {
        D::Error::custom(
            "reference has no usable id (expected an IRI string, an object with `id`, \
             or an array of either)",
        )
    })
}

/// Returns the standard JSON-LD context array used in ActivityPub objects.
pub fn default_context() -> serde_json::Value {
    serde_json::json!([
        "https://www.w3.org/ns/activitystreams",
        "https://w3id.org/security/v1"
    ])
}

// ---------------------------------------------------------------------------
// Actor types
// ---------------------------------------------------------------------------

/// RSA public key embedded in an Actor document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApPublicKey {
    pub id: String,
    pub owner: String,
    #[serde(rename = "publicKeyPem")]
    pub public_key_pem: String,
}

/// Shared inbox / other endpoint URLs for an Actor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApEndpoints {
    #[serde(rename = "sharedInbox", skip_serializing_if = "Option::is_none")]
    pub shared_inbox: Option<String>,
}

/// Mastodon-style profile metadata field (name/value pair).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApPropertyValue {
    #[serde(rename = "type")]
    pub r#type: String,
    pub name: String,
    pub value: String,
}

/// ActivityPub Person actor.
///
/// **Serialize-only in practice (verified 2026-07-22): this crate never
/// deserializes a remote actor through `ApPerson`.** Inbound actor documents are
/// read field-by-field from a raw `serde_json::Value` (`fetch_remote_actor` /
/// `parse_remote_actor`, and the `Update{Person}` arm); we only ever *build* an
/// `ApPerson` to serve our own actor. So the required fields below (`name`,
/// `preferredUsername`, `publicKey`, …) cannot cause an inbound parse drop — the
/// audit's "a name-less actor document fails to parse → empty-key cache" concern
/// does not apply to this type. They are kept required because we always supply
/// them when serving; do NOT relax them chasing an inbound break that lives on
/// the raw-`Value` path, not here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApPerson {
    #[serde(rename = "@context")]
    pub context: serde_json::Value,
    #[serde(rename = "type")]
    pub r#type: String,
    pub id: String,
    #[serde(rename = "preferredUsername")]
    pub preferred_username: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub inbox: String,
    pub outbox: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub followers: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub following: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(rename = "publicKey")]
    pub public_key: ApPublicKey,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<serde_json::Value>,
    #[serde(rename = "manuallyApprovesFollowers", default)]
    pub manually_approves_followers: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoints: Option<ApEndpoints>,
    #[serde(
        default,
        deserialize_with = "one_or_many",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub attachment: Vec<ApPropertyValue>,
}

// ---------------------------------------------------------------------------
// Object types
// ---------------------------------------------------------------------------

/// Media attachment (image, video, etc.) on a Note. Deserialized as part of an
/// inbound `ApNote`, so every field is relaxed: one mis-shaped attachment must
/// not fail the whole Note (audit 2026-07-22).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApMediaAttachment {
    /// Unconsumed on ingest (we always emit `"Document"` outbound); default
    /// rather than let a type-less attachment drop the Note.
    #[serde(rename = "type", default)]
    pub r#type: String,
    /// AS2-optional; default to empty rather than drop the Note.
    #[serde(rename = "mediaType", default)]
    pub media_type: String,
    /// Optional so a url-less attachment does not fail the Note — the ingest
    /// path (`ap_note_to_fauna_post`) skips attachments with no url instead.
    /// (A `url` spelled as a Link object / array — Pleroma-style multi-
    /// resolution — is a documented residual: still only a string is accepted.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Hashtag or mention tag on a Note. **Entirely unconsumed on ingest** — parsed
/// only so its presence cannot fail the Note — so every field is optional
/// (audit 2026-07-22). A non-object array element (a bare-string tag) is a
/// documented residual: `Vec<ApTag>` still rejects it, but no target peer emits
/// that shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApTag {
    #[serde(rename = "type", default)]
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Set only on the FEP-e232 object `Link` a quote carries.
    #[serde(
        rename = "mediaType",
        skip_deserializing,
        skip_serializing_if = "Option::is_none"
    )]
    pub media_type: Option<String>,
}

/// ActivityPub Note object — the ONE AS2 type this crate deserializes from
/// remote input (`handle_create` / `handle_update`; every other struct here is
/// serialize-only — see `ApPerson` / `ApActivity`). So the strict-serde class
/// audited 2026-07-22 lives here and in its nested `ApMediaAttachment` / `ApTag`:
/// serde fails the WHOLE struct on one field, so any single required-but-AS2-
/// optional field silently drops an entire legal Note shape from some peer, the
/// way a bare `Vec` dropped every compacted-`tag` reply from GoToSocial. Only
/// `id` and `type` stay required; every other field below is relaxed with a
/// one-line reason, and each relaxation is pinned in `tests`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApNote {
    #[serde(rename = "@context", skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
    /// REQUIRED (kept): dispatch already guaranteed `"Note"` before this parse.
    #[serde(rename = "type")]
    pub r#type: String,
    /// REQUIRED (kept): the `ap_post_map` identity key. An id-less Note cannot
    /// be mapped, deduped, updated, or deleted — there is nothing to ingest.
    pub id: String,
    /// Accepts the IRI-string, `{id}`-object, and array shapes AS2 permits; kept
    /// non-empty-required (it mints the author). Trust is unaffected — see
    /// [`id_string`].
    #[serde(rename = "attributedTo", deserialize_with = "id_string")]
    pub attributed_to: String,
    /// AS2-optional: a Note may be attachment-only or summary/CW-only. Empty is
    /// a safe input to `strip_html`, so default rather than drop the Note.
    #[serde(default)]
    pub content: String,
    /// AS2-optional: absent ⇒ the ingesting caller supplies its own timestamp
    /// (`ap_note_to_fauna_post`'s fallback). A *malformed* value still errors —
    /// that is non-conformant, not a legal shape variant.
    #[serde(default)]
    pub published: String,
    #[serde(
        default,
        deserialize_with = "one_or_many",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub to: Vec<String>,
    #[serde(
        default,
        deserialize_with = "one_or_many",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub cc: Vec<String>,
    #[serde(rename = "inReplyTo", skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    /// The quoted object's id, in the three spellings mainstream servers read
    /// (`activitypub.md` § Reply and quote): FEP-044f `quote` (Mastodon ≥ 4.4)
    /// and its legacy twins `quoteUri` and `_misskey_quote` (the Misskey
    /// family). Emitted together on an outbound quote. Serialize-only: never read
    /// on ingest, so a peer spelling one as an object cannot fail the Note.
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
    #[serde(
        rename = "quoteUri",
        skip_deserializing,
        skip_serializing_if = "Option::is_none"
    )]
    pub quote_uri: Option<String>,
    #[serde(
        rename = "_misskey_quote",
        skip_deserializing,
        skip_serializing_if = "Option::is_none"
    )]
    pub misskey_quote: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sensitive: Option<bool>,
    #[serde(rename = "summary", skip_serializing_if = "Option::is_none")]
    pub content_warning: Option<String>,
    #[serde(
        default,
        deserialize_with = "one_or_many",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub attachment: Vec<ApMediaAttachment>,
    #[serde(
        default,
        deserialize_with = "one_or_many",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tag: Vec<ApTag>,
}

// ---------------------------------------------------------------------------
// Activity types
// ---------------------------------------------------------------------------

/// Generic ActivityPub activity (Create, Follow, Like, Announce, Delete, …).
/// The `object` field is left as raw JSON to accommodate any wrapped type.
///
/// **Serialize-only in practice (verified 2026-07-22): the inbox parses the
/// top-level activity as a raw `serde_json::Value`, never through `ApActivity`.**
/// This type is only *built* for outbound delivery (`translate::build_*`). So its
/// required `id`/`actor`/`object` cannot cause an inbound parse drop — the
/// audit's `ApActivity.id` suspect does not reach a receive path. Kept required
/// because every builder supplies them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApActivity {
    #[serde(rename = "@context", skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
    #[serde(rename = "type")]
    pub r#type: String,
    pub id: String,
    pub actor: String,
    pub object: serde_json::Value,
    #[serde(
        default,
        deserialize_with = "one_or_many",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub to: Vec<String>,
    #[serde(
        default,
        deserialize_with = "one_or_many",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub cc: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
}

// ---------------------------------------------------------------------------
// Collection types
// ---------------------------------------------------------------------------

/// An ordered collection (e.g. outbox, followers list).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApOrderedCollection {
    #[serde(rename = "@context", skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
    #[serde(rename = "type")]
    pub r#type: String,
    pub id: String,
    #[serde(rename = "totalItems")]
    pub total_items: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last: Option<String>,
}

/// A page of an ordered collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApOrderedCollectionPage {
    #[serde(rename = "@context", skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
    #[serde(rename = "type")]
    pub r#type: String,
    pub id: String,
    #[serde(rename = "partOf")]
    pub part_of: String,
    #[serde(rename = "orderedItems")]
    pub ordered_items: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev: Option<String>,
}

// ---------------------------------------------------------------------------
// WebFinger types
// ---------------------------------------------------------------------------

/// A single link in a WebFinger response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebFingerLink {
    pub rel: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

/// Top-level WebFinger JRD response (`/.well-known/webfinger`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebFingerResponse {
    pub subject: String,
    #[serde(
        default,
        deserialize_with = "one_or_many",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub aliases: Vec<String>,
    #[serde(
        default,
        deserialize_with = "one_or_many",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub links: Vec<WebFingerLink>,
}

// ---------------------------------------------------------------------------
// NodeInfo types
// ---------------------------------------------------------------------------

/// A single link returned by the NodeInfo well-known endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfoLink {
    pub rel: String,
    pub href: String,
}

/// Response from `/.well-known/nodeinfo`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfoWellKnown {
    pub links: Vec<NodeInfoLink>,
}

/// The NodeInfo schema version this nest serves, and the path its discovery
/// link points at (`/nodeinfo/<NODEINFO_SCHEMA_VERSION>`). Single source, so the
/// `rel` we advertise, the route we mount, and the `version` field inside the
/// document can never drift apart — the drift that left the discovery link
/// dangling at the deleted `/api/v1/node-info` twin from 2026-06-05 to
/// 2026-07-16.
pub const NODEINFO_SCHEMA_VERSION: &str = "2.1";

/// The `rel` of the NodeInfo discovery link (NodeInfo spec § Discovery).
pub const NODEINFO_SCHEMA_REL: &str = "http://nodeinfo.diaspora.software/ns/schema/2.1";

/// `software` block of a NodeInfo document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfoSoftware {
    pub name: String,
    /// Coarsened to the `major.minor` line — never the patch level. NodeInfo is
    /// world-readable and crawled by fediverse observatories, so an exact
    /// version here is a targeted-CVE fingerprint. Same posture as the
    /// anonymous `fauna.nest.info` reply (`discovery_core::nest_info_core`).
    pub version: String,
}

/// `services` block — third-party services this node can relay to/from. Fauna
/// bridges nothing through NodeInfo's service vocabulary, so both are empty
/// (the fields are schema-required, not optional).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfoServices {
    pub inbound: Vec<String>,
    pub outbound: Vec<String>,
}

/// `usage.users` block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfoUsers {
    pub total: u64,
}

/// `usage` block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfoUsage {
    pub users: NodeInfoUsers,
}

/// The NodeInfo 2.1 document — served at `/nodeinfo/2.1`, the target of the
/// `/.well-known/nodeinfo` discovery link.
///
/// `protocols` carries **only** NodeInfo's own fixed vocabulary (`activitypub`,
/// `diaspora`, `ostatus`, …) — deliberately NOT the nest's internal protocol
/// list (`fauna`/`nostr`/`bluesky`), whose tokens are absent from the NodeInfo
/// schema enum and would make this document fail a strict validator on a real
/// peer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfoDocument {
    pub version: String,
    pub software: NodeInfoSoftware,
    pub protocols: Vec<String>,
    pub services: NodeInfoServices,
    #[serde(rename = "openRegistrations")]
    pub open_registrations: bool,
    pub usage: NodeInfoUsage,
    /// Schema-required free-form object; empty by default — anything added here
    /// is a public fingerprinting surface.
    pub metadata: serde_json::Value,
}

impl NodeInfoDocument {
    /// Build the document this nest serves. `software_version` must already be
    /// coarsened by the caller (see [`NodeInfoSoftware::version`]).
    pub fn new(
        software_name: &str,
        software_version: &str,
        open_registrations: bool,
        users_total: u64,
    ) -> Self {
        Self {
            version: NODEINFO_SCHEMA_VERSION.to_string(),
            software: NodeInfoSoftware {
                name: software_name.to_string(),
                version: software_version.to_string(),
            },
            protocols: vec!["activitypub".to_string()],
            services: NodeInfoServices {
                inbound: Vec::new(),
                outbound: Vec::new(),
            },
            open_registrations,
            usage: NodeInfoUsage {
                users: NodeInfoUsers { total: users_total },
            },
            metadata: serde_json::json!({}),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The document carries every NodeInfo-2.1-required key, under the exact
    /// wire names a peer parses (`openRegistrations` is camelCase; `services`
    /// and `metadata` are required even when empty).
    #[test]
    fn nodeinfo_document_serializes_to_the_2_1_schema() {
        let doc = NodeInfoDocument::new("fauna", "0.1", false, 0);
        let v = serde_json::to_value(&doc).expect("serializes");

        assert_eq!(v["version"], "2.1");
        assert_eq!(v["software"]["name"], "fauna");
        assert_eq!(v["software"]["version"], "0.1");
        assert_eq!(v["openRegistrations"], false);
        assert_eq!(v["usage"]["users"]["total"], 0);
        assert_eq!(v["services"]["inbound"], serde_json::json!([]));
        assert_eq!(v["services"]["outbound"], serde_json::json!([]));
        assert_eq!(v["metadata"], serde_json::json!({}));
    }

    /// `protocols` is NodeInfo's own fixed vocabulary — never the nest's
    /// internal `fauna`/`nostr`/`bluesky` tokens, which are not in the schema
    /// enum and would fail a strict peer validator.
    #[test]
    fn nodeinfo_document_advertises_only_activitypub_protocol() {
        let doc = NodeInfoDocument::new("fauna", "0.1", true, 3);
        assert_eq!(doc.protocols, vec!["activitypub".to_string()]);
        assert_eq!(doc.usage.users.total, 3);
        assert!(doc.open_registrations);
    }

    /// The discovery `rel` and the served path derive from one constant, so the
    /// link can never advertise a schema version the route does not serve.
    #[test]
    fn nodeinfo_rel_and_path_agree_on_schema_version() {
        assert!(NODEINFO_SCHEMA_REL.ends_with(NODEINFO_SCHEMA_VERSION));
        assert_eq!(
            NodeInfoDocument::new("fauna", "0.1", false, 0).version,
            NODEINFO_SCHEMA_VERSION
        );
    }

    #[test]
    fn note_serializes_with_context() {
        let note = ApNote {
            context: Some(default_context()),
            r#type: "Note".to_string(),
            id: "https://fauna.social/users/alice/notes/1".to_string(),
            attributed_to: "https://fauna.social/users/alice".to_string(),
            content: "<p>Hello, Fediverse!</p>".to_string(),
            published: "2026-03-20T00:00:00Z".to_string(),
            to: vec![AP_PUBLIC.to_string()],
            cc: vec![],
            in_reply_to: None,
            quote: None,
            quote_uri: None,
            misskey_quote: None,
            url: None,
            sensitive: None,
            content_warning: None,
            attachment: vec![],
            tag: vec![],
        };

        let json = serde_json::to_value(&note).expect("serialize ApNote");

        assert_eq!(json["type"], "Note");
        assert_eq!(json["attributedTo"], "https://fauna.social/users/alice");
        assert!(json["@context"].is_array(), "@context should be an array");
    }

    #[test]
    fn person_serializes_with_public_key() {
        let person = ApPerson {
            context: default_context(),
            r#type: "Person".to_string(),
            id: "https://fauna.social/users/alice".to_string(),
            preferred_username: "alice".to_string(),
            name: "Alice".to_string(),
            summary: None,
            inbox: "https://fauna.social/users/alice/inbox".to_string(),
            outbox: "https://fauna.social/users/alice/outbox".to_string(),
            followers: None,
            following: None,
            url: None,
            public_key: ApPublicKey {
                id: "https://fauna.social/users/alice#main-key".to_string(),
                owner: "https://fauna.social/users/alice".to_string(),
                public_key_pem: "-----BEGIN PUBLIC KEY-----\nMIIB...\n-----END PUBLIC KEY-----\n"
                    .to_string(),
            },
            icon: None,
            image: None,
            manually_approves_followers: false,
            endpoints: None,
            attachment: vec![],
        };

        let json = serde_json::to_value(&person).expect("serialize ApPerson");

        assert_eq!(json["preferredUsername"], "alice");
        assert_eq!(
            json["publicKey"]["publicKeyPem"],
            "-----BEGIN PUBLIC KEY-----\nMIIB...\n-----END PUBLIC KEY-----\n"
        );
    }

    #[test]
    fn create_activity_wraps_note() {
        let note = ApNote {
            context: None,
            r#type: "Note".to_string(),
            id: "https://fauna.social/users/alice/notes/42".to_string(),
            attributed_to: "https://fauna.social/users/alice".to_string(),
            content: "<p>Test</p>".to_string(),
            published: "2026-03-20T00:00:00Z".to_string(),
            to: vec![AP_PUBLIC.to_string()],
            cc: vec![],
            in_reply_to: None,
            quote: None,
            quote_uri: None,
            misskey_quote: None,
            url: None,
            sensitive: None,
            content_warning: None,
            attachment: vec![],
            tag: vec![],
        };

        let activity = ApActivity {
            context: Some(default_context()),
            r#type: "Create".to_string(),
            id: "https://fauna.social/users/alice/activities/42".to_string(),
            actor: "https://fauna.social/users/alice".to_string(),
            object: serde_json::to_value(&note).expect("serialize note"),
            to: vec![AP_PUBLIC.to_string()],
            cc: vec![],
            published: Some("2026-03-20T00:00:00Z".to_string()),
        };

        let json = serde_json::to_value(&activity).expect("serialize ApActivity");

        assert_eq!(json["type"], "Create");
        assert_eq!(json["object"]["type"], "Note");
    }

    /// AS2 multi-valued properties may arrive **compacted to a single value**,
    /// and a `Vec<T>` field alone rejects that form — failing the *whole* Note,
    /// not one field.
    ///
    /// This is the shape real GoToSocial 0.22.1 sends for a reply that mentions
    /// exactly one account (2026-07-22): `tag` compacts to a bare object. Under
    /// a plain `Vec<ApTag>` it produced `invalid type: map, expected a sequence`
    /// and a `400` on every inbound reply from that peer, while Mastodon — which
    /// always emits arrays — was unaffected. Both spellings are the same
    /// document, so both must parse to the same value.
    #[test]
    fn a_note_parses_with_single_valued_and_array_properties_alike() {
        let compacted = serde_json::json!({
            "type": "Note",
            "id": "https://gts.test/users/alice/statuses/1",
            "attributedTo": "https://gts.test/users/alice",
            "content": "<p>hi</p>",
            "published": "2026-07-22T12:00:00Z",
            "to": "https://www.w3.org/ns/activitystreams#Public",
            "cc": "https://nest.test/ap/users/bob",
            "tag": {"type": "Mention", "href": "https://nest.test/ap/users/bob", "name": "@bob@nest.test"},
            "attachment": {"type": "Document", "mediaType": "image/png", "url": "https://gts.test/a.png"},
        });
        let expanded = serde_json::json!({
            "type": "Note",
            "id": "https://gts.test/users/alice/statuses/1",
            "attributedTo": "https://gts.test/users/alice",
            "content": "<p>hi</p>",
            "published": "2026-07-22T12:00:00Z",
            "to": ["https://www.w3.org/ns/activitystreams#Public"],
            "cc": ["https://nest.test/ap/users/bob"],
            "tag": [{"type": "Mention", "href": "https://nest.test/ap/users/bob", "name": "@bob@nest.test"}],
            "attachment": [{"type": "Document", "mediaType": "image/png", "url": "https://gts.test/a.png"}],
        });

        let from_compacted: ApNote =
            serde_json::from_value(compacted).expect("compacted Note must parse");
        let from_expanded: ApNote =
            serde_json::from_value(expanded).expect("array Note must parse");

        for note in [&from_compacted, &from_expanded] {
            assert_eq!(note.to, vec![AP_PUBLIC.to_string()]);
            assert_eq!(note.cc, vec!["https://nest.test/ap/users/bob".to_string()]);
            assert_eq!(note.tag.len(), 1, "the mention must survive");
            assert_eq!(note.tag[0].name.as_deref(), Some("@bob@nest.test"));
            assert_eq!(note.attachment.len(), 1);
        }
        // The compacted form is not merely accepted — it yields the SAME value.
        assert_eq!(
            serde_json::to_value(&from_compacted).unwrap(),
            serde_json::to_value(&from_expanded).unwrap(),
        );
    }

    /// The audience gate reads `to`/`cc`, so a compacted `to` must not silently
    /// become an EMPTY audience — that would turn a public Note into a
    /// non-public one and drop it, which is the same observable as a parse
    /// failure but for a completely different reason.
    #[test]
    fn a_compacted_audience_is_not_silently_empty() {
        let note: ApNote = serde_json::from_value(serde_json::json!({
            "type": "Note",
            "id": "https://gts.test/users/alice/statuses/2",
            "attributedTo": "https://gts.test/users/alice",
            "content": "hi",
            "published": "2026-07-22T12:00:00Z",
            "to": "https://www.w3.org/ns/activitystreams#Public",
        }))
        .expect("Note with a compacted `to` must parse");
        assert!(
            crate::translate::is_publicly_addressed(&note.to, &note.cc),
            "a compacted public `to` must still read as publicly addressed"
        );
    }

    /// The quote spellings and a tag's `mediaType` are serialize-only: a peer
    /// spelling `quote` as an object (or `mediaType` as anything) must not fail
    /// the Note, and nothing of them is read back in.
    #[test]
    fn inbound_quote_spellings_of_any_shape_do_not_fail_the_note() {
        let note: ApNote = serde_json::from_value(serde_json::json!({
            "type": "Note",
            "id": "https://misskey.test/notes/4",
            "attributedTo": "https://misskey.test/users/carol",
            "quote": {"id": "https://gts.test/users/alice/statuses/2"},
            "quoteUri": 7,
            "_misskey_quote": ["x"],
            "tag": [{"type": "Link", "href": "https://gts.test/s/2", "mediaType": {"odd": true}}],
        }))
        .expect("a Note with odd quote spellings must parse");
        assert!(note.quote.is_none() && note.quote_uri.is_none() && note.misskey_quote.is_none());
        assert!(note.tag[0].media_type.is_none());
    }

    /// An absent or null multi-valued property is an empty list, never an error
    /// — `one_or_many` replaces `#[serde(default)]`, so it must keep that.
    #[test]
    fn absent_and_null_multi_valued_properties_stay_empty() {
        let note: ApNote = serde_json::from_value(serde_json::json!({
            "type": "Note",
            "id": "https://gts.test/users/alice/statuses/3",
            "attributedTo": "https://gts.test/users/alice",
            "content": "hi",
            "published": "2026-07-22T12:00:00Z",
            "tag": serde_json::Value::Null,
        }))
        .expect("absent/null multi-valued properties must parse");
        assert!(note.to.is_empty() && note.cc.is_empty());
        assert!(note.tag.is_empty() && note.attachment.is_empty());
    }

    /// Minimal valid Note JSON (only the two fields the audit keeps required),
    /// so a test can add exactly the one adversarial shape it exercises.
    fn minimal_note() -> serde_json::Value {
        serde_json::json!({
            "type": "Note",
            "id": "https://gts.test/users/alice/statuses/1",
            "attributedTo": "https://gts.test/users/alice",
        })
    }

    /// `content` is AS2-optional (an attachment-only or CW-only Note). Required,
    /// it would drop the whole Note; relaxed, it defaults to empty.
    #[test]
    fn a_content_less_note_parses_to_empty_content() {
        let note: ApNote =
            serde_json::from_value(minimal_note()).expect("a content-less Note must parse");
        assert_eq!(note.content, "");
    }

    /// `published` is AS2-optional. Required, it would drop the whole Note;
    /// relaxed, it defaults to empty and the ingest caller supplies a fallback.
    #[test]
    fn a_published_less_note_parses_to_empty_published() {
        let note: ApNote =
            serde_json::from_value(minimal_note()).expect("a published-less Note must parse");
        assert_eq!(note.published, "");
    }

    /// `attributedTo` may be an IRI string, an `{id}` object, or an array of
    /// either — all three name the same actor and MUST parse to an equal value.
    #[test]
    fn attributed_to_accepts_string_object_and_array_alike() {
        let actor = "https://gts.test/users/alice";
        let shapes = [
            serde_json::json!(actor),
            serde_json::json!({ "type": "Person", "id": actor }),
            serde_json::json!([actor]),
            serde_json::json!([{ "type": "Person", "id": actor }]),
        ];
        for shape in shapes {
            let mut j = minimal_note();
            j["attributedTo"] = shape.clone();
            let note: ApNote = serde_json::from_value(j)
                .unwrap_or_else(|e| panic!("attributedTo shape {shape} must parse: {e}"));
            assert_eq!(note.attributed_to, actor, "shape {shape} must yield the id");
        }
    }

    /// A reference with no extractable id is still an error — kept required,
    /// because a Note with no author identity has nothing to ingest.
    #[test]
    fn attributed_to_with_no_usable_id_errors() {
        for bad in [
            serde_json::json!(""),
            serde_json::json!({ "type": "Person" }),
            serde_json::json!([]),
            serde_json::json!(42),
        ] {
            let mut j = minimal_note();
            j["attributedTo"] = bad.clone();
            assert!(
                serde_json::from_value::<ApNote>(j).is_err(),
                "attributedTo {bad} must be rejected"
            );
        }
    }

    /// One attachment missing `mediaType` and/or `url` must not fail the Note —
    /// it parses (media_type defaults empty, url stays None) and the ingest path
    /// drops just that attachment.
    #[test]
    fn an_attachment_missing_media_type_or_url_does_not_fail_the_note() {
        let mut j = minimal_note();
        j["attachment"] = serde_json::json!([
            { "type": "Document", "mediaType": "image/png", "url": "https://gts.test/a.png" },
            { "type": "Document" }, // no mediaType, no url — the adversarial one
        ]);
        let note: ApNote =
            serde_json::from_value(j).expect("a url-less attachment must not fail the Note");
        assert_eq!(note.attachment.len(), 2);
        assert_eq!(note.attachment[1].media_type, "");
        assert_eq!(note.attachment[1].url, None);
    }

    /// A tag missing `type` and/or `name` must not fail the Note — tags are
    /// unconsumed on ingest, so their shape is never worth dropping a post over.
    #[test]
    fn a_type_or_name_less_tag_does_not_fail_the_note() {
        let mut j = minimal_note();
        j["tag"] = serde_json::json!([
            { "href": "https://gts.test/tags/x" }, // no type, no name
        ]);
        let note: ApNote =
            serde_json::from_value(j).expect("a bare tag object must not fail the Note");
        assert_eq!(note.tag.len(), 1);
        assert_eq!(note.tag[0].r#type, "");
        assert_eq!(note.tag[0].name, None);
    }
}
