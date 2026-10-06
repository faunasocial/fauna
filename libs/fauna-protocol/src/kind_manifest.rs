//! The **kind manifest** — the metadata document's `fauna` member, a compact
//! JWS whose payload is the extension object — verified and parsed.
//!
//! Owner: `docs/goal/architecture/third-party-kinds.md` § The manifest (the
//! bytes and the signature) and § The kinds vocabulary (each `kinds` entry);
//! the object's members are `third-party.md` § The manifest's. One function,
//! [`verify_manifest`], is the only door: every consumer — the nest's
//! document resolver, the consenting device, every replica re-verifying the
//! `fauna.state.kind-manifest` plane row — runs the same checks, in shared
//! Rust, over the same bytes.
//!
//! The rulings it enforces:
//!
//! - **Compact JWS (RFC 7515 § 7.1), payload = the UTF-8 JSON object.** The
//!   signature covers the JWS signing input as transmitted, so there is no
//!   canonicalization: what is verified is what is then parsed.
//! - **Header exactly `{"alg":"EdDSA","kid":"<did:key>","typ":"fauna-manifest+json"}`**
//!   — any other `alg` (`none` included), a missing `typ`, a `crit` or any
//!   other member refuses.
//! - **`did:key` over the Ed25519 multicodec only**, and `publisher.key`
//!   equals the header's `kid`.
//! - **Same-origin anchoring:** `publisher.domain` equals the document's
//!   host, and every declared kind's parsed publisher equals it.
//! - **Unknown members refuse** — at the top level, in `publisher`, and in
//!   every `kinds` entry.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, VerifyingKey};
use serde_json::{Map, Value};

use crate::ext_kind::{ExtKind, is_valid_publisher};
use crate::merge_policy::{AdmitError, AdmittedKinds, MergePolicy};

/// The one admitted JWS `alg`.
pub const MANIFEST_ALG: &str = "EdDSA";
/// The one admitted JWS `typ`.
pub const MANIFEST_TYP: &str = "fauna-manifest+json";
/// The one payload `version` this build reads.
pub const MANIFEST_VERSION: u64 = 1;
/// The most kinds one manifest may declare (§ The kinds vocabulary; refutable).
pub const MAX_MANIFEST_KINDS: usize = 64;
/// The one admitted `class` value — class-2 account state.
pub const MANIFEST_CLASS_STATE: &str = "state";
/// The one admitted `floor` value.
pub const MANIFEST_FLOOR_NONE: &str = "none";

/// The `did:key` prefix and the Ed25519 public-key multicodec varint
/// (`0xed 0x01`).
const DID_KEY_PREFIX: &str = "did:key:z";
const ED25519_MULTICODEC: [u8; 2] = [0xED, 0x01];

/// Every top-level member of the extension object (`third-party.md` § The
/// manifest). This module reads `version`, `publisher`, `kinds`, `bridge` and
/// `service_auth`; the rest are known members other slices interpret, kept
/// verbatim.
const PAYLOAD_MEMBERS: &[&str] = &[
    "version",
    "publisher",
    "execution",
    "kinds",
    "settings_schema",
    "bridge",
    "ingress",
    "events_uri",
    "service_auth",
];

/// The most `service_auth` entries (audiences) one manifest may declare
/// (`third-party.md` § The manifest; refutable).
pub const MAX_SERVICE_AUTH_AUDIENCES: usize = 16;
/// The most `lxm` methods one `service_auth` entry may name.
pub const MAX_SERVICE_AUTH_METHODS: usize = 64;

/// Why a manifest was refused. Each refusal happens at resolution — a
/// document carrying one never yields a principal or a kind.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    /// Not three dot-separated base64url segments.
    #[error("the fauna member is not a compact JWS")]
    NotCompactJws,
    /// A segment is not unpadded base64url, or not JSON where JSON is due.
    #[error("malformed JWS segment: {0}")]
    BadSegment(&'static str),
    /// The protected header is not exactly the pinned three members.
    #[error("JWS header refused: {0}")]
    BadHeader(String),
    /// The key is not a `did:key` over Ed25519.
    #[error("publisher key is not an Ed25519 did:key: {0:?}")]
    BadKey(String),
    /// The signature does not verify under the header's key.
    #[error("manifest signature does not verify")]
    BadSignature,
    /// The key is valid but is not the one this row pinned (key continuity).
    #[error("manifest key differs from the pinned publisher key — re-consent required")]
    KeyChanged,
    /// The payload is not a JSON object, or a member is malformed.
    #[error("manifest payload refused: {0}")]
    BadPayload(String),
    /// A member this build does not know.
    #[error("unknown manifest member {0:?}")]
    UnknownMember(String),
    /// `publisher.domain` is not the document's host.
    #[error("publisher.domain {domain:?} is not the document host {host:?}")]
    WrongDomain {
        /// The manifest's claim.
        domain: String,
        /// The host the document was served from.
        host: String,
    },
    /// A declared kind is malformed or outside the publisher's own domain.
    #[error("manifest kind refused: {0}")]
    BadKind(String),
    /// A kind entry's `class`, `merge` or `floor` is outside the admitted set.
    #[error("manifest kind {kind} refused: {why}")]
    NotAdmitted {
        /// The kind.
        kind: String,
        /// Which member and why.
        why: String,
    },
    /// `events_uri` is not an `https` URL on the publisher's own domain
    /// (`transport.md` § Push events → *Third-party event doors*, the
    /// webhook: the nest is never aimed at a URL the publisher does not
    /// serve).
    #[error("events_uri refused: {0}")]
    BadEventsUri(String),
}

/// One admitted `kinds` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestKind {
    /// The kind, its publisher equal to the manifest's.
    pub kind: ExtKind,
    /// Its merge policy — `LatestWins`, the one a manifest may admit.
    pub merge: MergePolicy,
}

/// A manifest that verified and parsed. Holding one is proof that every
/// check above passed over these bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedManifest {
    /// The compact JWS verbatim — what the plane row carries and every
    /// replica re-verifies.
    pub jws: String,
    /// The publisher — the document's host.
    pub publisher_domain: String,
    /// The publisher's Ed25519 key, raw (the row's `publisher_key`).
    pub publisher_key: [u8; 32],
    /// The declared kinds, in manifest order.
    pub kinds: Vec<ManifestKind>,
    /// The `bridge` block, validated — `None` for a document that is not a
    /// conversation bridge.
    pub bridge: Option<BridgeBlock>,
    /// The `service_auth` member, structurally validated, in manifest order —
    /// empty for a document that declares none. Which `lxm` a custodian may
    /// mint for at all is the atproto bridge's deny half
    /// (`fauna_bridge_atproto::authz::service_auth_lxm_admitted`), which the
    /// nest's resolver applies after this parse.
    pub service_auth: Vec<ServiceAuthEntry>,
    /// The `events_uri` member, validated by [`validate_events_uri`] —
    /// `None` for a document that declares none (absent or `null`): the
    /// webhook the events doors POST to (`transport.md` § Push events →
    /// *Third-party event doors*).
    pub events_uri: Option<String>,
    /// The whole payload object, for the members other slices interpret.
    pub payload: Map<String, Value>,
}

/// One `service_auth` entry (`third-party.md` § The manifest): the service an
/// `atproto.service_auth` mint may name as `aud`, and the exact methods it may
/// name as `lxm` — no wildcard, no prefix. The set the oracle's class is
/// bounded by, kept on the roster row as `declared_service_auth` and served by
/// `fauna.principals.list`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServiceAuthEntry {
    /// A `did:plc:` / `did:web:` DID, optionally with a `#fragment`.
    pub aud: String,
    /// Syntactically valid NSIDs, non-empty, each at most once.
    pub lxm: Vec<String>,
    /// Members a newer build added, kept for the re-serve (rule 4). The
    /// manifest parse refuses an unknown member, so only a stored or served
    /// entry ever fills this.
    #[serde(flatten, default)]
    pub extra: std::collections::BTreeMap<String, fauna_cbor::Value>,
}

/// `Eq` for the reason [`BridgeBlock`]'s holds.
impl Eq for ServiceAuthEntry {}

impl ServiceAuthEntry {
    /// Does this entry admit the pair? Exact strings — the audience compared
    /// whole, fragment included, as declared.
    #[must_use]
    pub fn admits(&self, aud: &str, lxm: &str) -> bool {
        self.aud == aud && self.lxm.iter().any(|m| m == lxm)
    }
}

/// Is `s` a syntactically valid NSID (the atproto Lexicon grammar): at least
/// three dot-separated segments, at most 317 bytes; each domain-authority
/// segment 1–63 bytes of ASCII letters, digits and inner hyphens, the first
/// not starting with a digit; the name segment 1–63 bytes, a letter then
/// letters and digits. A `*` or a trailing `.` — a wildcard or a prefix —
/// is no NSID.
#[must_use]
pub fn is_valid_nsid(s: &str) -> bool {
    if s.len() > 317 {
        return false;
    }
    let segments: Vec<&str> = s.split('.').collect();
    let Some((name, authority)) = segments.split_last() else {
        return false;
    };
    if authority.len() < 2 {
        return false;
    }
    let authority_ok = authority.iter().enumerate().all(|(i, seg)| {
        !seg.is_empty()
            && seg.len() <= 63
            && seg.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !seg.starts_with('-')
            && !seg.ends_with('-')
            && !(i == 0 && seg.starts_with(|c: char| c.is_ascii_digit()))
    });
    let name_ok = !name.is_empty()
        && name.len() <= 63
        && name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name.bytes().all(|b| b.is_ascii_alphanumeric());
    authority_ok && name_ok
}

/// Is `s` a service DID a `service_auth` entry may name: `did:plc:` (24
/// lowercase base32 characters) or `did:web:` (a lowercase host, a `%3A`
/// port allowed), optionally followed by one `#fragment` of unreserved
/// characters.
fn is_valid_service_did(s: &str) -> bool {
    let (did, fragment) = match s.split_once('#') {
        Some((did, f)) => (did, Some(f)),
        None => (s, None),
    };
    if let Some(f) = fragment
        && (f.is_empty()
            || !f
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')))
    {
        return false;
    }
    if let Some(id) = did.strip_prefix("did:plc:") {
        return id.len() == 24 && id.bytes().all(|b| matches!(b, b'a'..=b'z' | b'2'..=b'7'));
    }
    if let Some(rest) = did.strip_prefix("did:web:") {
        let (host, port) = match rest.split_once("%3A") {
            Some((h, p)) => (h, Some(p)),
            None => (rest, None),
        };
        return is_valid_publisher(host)
            && port.is_none_or(|p| {
                !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit())
            });
    }
    false
}

/// Parse and validate the `service_auth` member — structure only; the deny
/// half is the atproto bridge's, applied by the resolver.
fn parse_service_auth(value: &Value) -> Result<Vec<ServiceAuthEntry>, ManifestError> {
    let bad = |why: String| ManifestError::BadPayload(format!("service_auth: {why}"));
    let entries = value
        .as_array()
        .ok_or_else(|| bad("must be an array".into()))?;
    if entries.len() > MAX_SERVICE_AUTH_AUDIENCES {
        return Err(bad(format!("at most {MAX_SERVICE_AUTH_AUDIENCES} entries")));
    }
    let mut out: Vec<ServiceAuthEntry> = Vec::with_capacity(entries.len());
    for entry in entries {
        let obj = entry
            .as_object()
            .ok_or_else(|| bad("an entry must be an object".into()))?;
        only_members(obj, &["aud", "lxm"])?;
        let aud = obj
            .get("aud")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("aud must be a string".into()))?;
        if !is_valid_service_did(aud) {
            return Err(bad(format!(
                "aud {aud:?} is not a did:plc or did:web service DID"
            )));
        }
        if out.iter().any(|e| e.aud == aud) {
            return Err(bad(format!("aud {aud:?} is declared twice")));
        }
        let methods = obj
            .get("lxm")
            .and_then(Value::as_array)
            .ok_or_else(|| bad("lxm must be an array".into()))?;
        if methods.is_empty() || methods.len() > MAX_SERVICE_AUTH_METHODS {
            return Err(bad(format!(
                "lxm must name 1 to {MAX_SERVICE_AUTH_METHODS} methods"
            )));
        }
        let mut lxm: Vec<String> = Vec::with_capacity(methods.len());
        for m in methods {
            let m = m
                .as_str()
                .ok_or_else(|| bad("an lxm must be a string".into()))?;
            if !is_valid_nsid(m) {
                return Err(bad(format!(
                    "lxm {m:?} is not an NSID (no wildcard, no prefix)"
                )));
            }
            if lxm.iter().any(|x| x == m) {
                return Err(bad(format!("lxm {m:?} is declared twice")));
            }
            lxm.push(m.to_string());
        }
        out.push(ServiceAuthEntry {
            aud: aud.to_string(),
            lxm,
            extra: Default::default(),
        });
    }
    Ok(out)
}

/// The longest `bridge.address_grammar` a manifest may declare, in bytes
/// (`third-party.md` § The manifest → *The `bridge` block*).
pub const MAX_BRIDGE_ADDRESS_GRAMMAR_BYTES: usize = 256;

/// The first-party bridge ids no manifest may claim — the in-process legs'
/// own identities (`third-party.md` § The manifest → *The `bridge` block*).
pub const RESERVED_BRIDGE_IDS: &[&str] = &["nostr", "bluesky", "activitypub", "email", "fauna"];

/// The members of `bridge.capabilities` a manifest MUST declare: the
/// `ThreadCapabilities` record's always-present fields
/// (`libs/fauna-conversations/src/capabilities.rs`) minus `encryption`, which
/// the room's class derives. `fauna-conversations` never depends on this
/// crate, so the names are listed here and pinned against the record by the
/// nest's `bridge_capability_members_are_thread_capabilities_minus_encryption`.
pub const BRIDGE_CAPABILITY_REQUIRED: &[&str] = &[
    "supports_attachments",
    "supports_markdown",
    "supports_reactions",
    "supports_message_delete",
    "supports_per_message_reply",
    "supports_membership_change",
    "supports_recipient_selection",
    "supports_rename",
    "supports_subject",
    "delivery_mode",
];

/// The record's role-gated fields, each `#[serde(default)]` there and so
/// optional here.
pub const BRIDGE_CAPABILITY_OPTIONAL: &[&str] = &[
    "can_invite",
    "can_remove_members",
    "can_set_policy",
    "can_appoint_admins",
    "can_transfer_ownership",
    "can_leave_room",
];

/// The `delivery_mode` spellings — `DeliveryMode`'s serde form.
pub const BRIDGE_DELIVERY_MODES: &[&str] = &["Realtime", "Async"];

/// One value of the declared capability vector: a flag, or the
/// `delivery_mode` spelling. Untagged, so the map serializes as the
/// `ThreadCapabilities` record's own JSON shape and a reader deserializes it
/// into that record directly.
///
/// Open, carrying (`transport.md` § Schema and forward-compat discipline →
/// *Rule 3 in full*): the vector grows by new members, and a later member may
/// hold a value that is neither a flag nor a string. A nest re-serves the
/// stored block and an older build may be the one reading it, so the value is
/// kept whole and re-emitted unchanged.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum BridgeCapabilityValue {
    /// A `supports_*` / `can_*` member.
    Flag(bool),
    /// `delivery_mode`.
    Mode(String),
    /// A value shape a newer build added. It reads as no flag and no mode —
    /// the capability is not offered — and is never authored here:
    /// `parse_bridge` refuses a member it does not name.
    #[serde(untagged)]
    Other(fauna_core::carried::CarriedValue),
}

/// A manifest's `bridge` block, validated at resolution
/// (`third-party.md` § The manifest → *The `bridge` block*). Holding one is
/// proof every refusal below was passed; the roster row stores it as JSON
/// and `fauna.principals.list` / `fauna.bridges.list` carry it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BridgeBlock {
    /// One lowercase label under the kind grammar's name rule; never a
    /// reserved first-party id.
    pub id: String,
    /// The lowercase id of one `SourceGlyph::ALL` member.
    pub glyph: String,
    /// A regex over the far network's own address spelling, at most
    /// [`MAX_BRIDGE_ADDRESS_GRAMMAR_BYTES`], known to compile. Matched
    /// nest-side only, at `conversation.rooms.open`.
    pub address_grammar: String,
    /// The `ThreadCapabilities` record minus `encryption`.
    pub capabilities: std::collections::BTreeMap<String, BridgeCapabilityValue>,
    /// Members a newer build added, kept for the re-serve (rule 4). The
    /// manifest parse itself refuses an unknown member, so only a stored or
    /// served block ever fills this.
    #[serde(flatten, default)]
    pub extra: std::collections::BTreeMap<String, fauna_cbor::Value>,
}

/// `Eq` holds for the reason it holds for `CarriedValue`: the only value
/// `PartialEq` is not reflexive over is a NaN float, which neither dag-cbor
/// nor the roster row's JSON can carry into `extra`.
impl Eq for BridgeBlock {}

impl BridgeBlock {
    /// Does `address` match the declared grammar? The one place it is
    /// matched. A grammar that no longer compiles (it compiled at
    /// resolution, so only a hand-edited row) matches nothing.
    #[must_use]
    pub fn address_matches(&self, address: &str) -> bool {
        compile_address_grammar(&self.address_grammar).is_ok_and(|re| re.is_match(address))
    }

    /// The declared glyph's concept.
    #[must_use]
    pub fn source_glyph(&self) -> fauna_core::source_glyph::SourceGlyph {
        fauna_core::source_glyph::SourceGlyph::from_id(&self.glyph)
    }
}

/// Compile a declared grammar under the bound — byte length first, then the
/// `regex` crate's own size limit, so a short pattern that expands into a
/// huge automaton refuses too.
fn compile_address_grammar(grammar: &str) -> Result<regex::Regex, String> {
    if grammar.len() > MAX_BRIDGE_ADDRESS_GRAMMAR_BYTES {
        return Err(format!(
            "address_grammar is {} bytes (at most {MAX_BRIDGE_ADDRESS_GRAMMAR_BYTES})",
            grammar.len()
        ));
    }
    regex::RegexBuilder::new(grammar)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| format!("address_grammar does not compile: {e}"))
}

/// Parse and validate the `bridge` member.
fn parse_bridge(value: &Value) -> Result<BridgeBlock, ManifestError> {
    let bad = |why: String| ManifestError::BadPayload(format!("bridge: {why}"));
    let obj = value
        .as_object()
        .ok_or_else(|| bad("must be an object".into()))?;
    only_members(obj, &["id", "glyph", "address_grammar", "capabilities"])?;
    let id = str_member(obj, "id")?;
    if !fauna_core::ext_kind::is_valid_kind_name(id) {
        return Err(bad(format!("id {id:?} is not one lowercase label")));
    }
    if RESERVED_BRIDGE_IDS.contains(&id) {
        return Err(bad(format!(
            "id {id:?} is reserved for a first-party bridge"
        )));
    }
    let glyph = str_member(obj, "glyph")?;
    if !fauna_core::source_glyph::SourceGlyph::ALL
        .iter()
        .any(|g| g.id() == glyph)
    {
        return Err(bad(format!("glyph {glyph:?} is not a known glyph id")));
    }
    let address_grammar = str_member(obj, "address_grammar")?;
    compile_address_grammar(address_grammar).map_err(bad)?;
    let caps = obj
        .get("capabilities")
        .and_then(Value::as_object)
        .ok_or_else(|| bad("capabilities must be an object".into()))?;
    if caps.contains_key("encryption") {
        return Err(bad(
            "capabilities may not declare encryption — the room's class derives it".into(),
        ));
    }
    let mut capabilities = std::collections::BTreeMap::new();
    for (name, v) in caps {
        let value = if name == "delivery_mode" {
            match v.as_str() {
                Some(m) if BRIDGE_DELIVERY_MODES.contains(&m) => {
                    BridgeCapabilityValue::Mode(m.to_string())
                }
                _ => {
                    return Err(bad(format!(
                        "delivery_mode {v} is not one of {BRIDGE_DELIVERY_MODES:?}"
                    )));
                }
            }
        } else if BRIDGE_CAPABILITY_REQUIRED.contains(&name.as_str())
            || BRIDGE_CAPABILITY_OPTIONAL.contains(&name.as_str())
        {
            BridgeCapabilityValue::Flag(
                v.as_bool()
                    .ok_or_else(|| bad(format!("capabilities.{name} must be a boolean")))?,
            )
        } else {
            return Err(ManifestError::UnknownMember(format!(
                "bridge.capabilities.{name}"
            )));
        };
        capabilities.insert(name.clone(), value);
    }
    if let Some(missing) = BRIDGE_CAPABILITY_REQUIRED
        .iter()
        .find(|m| !capabilities.contains_key(**m))
    {
        return Err(bad(format!("capabilities.{missing} is required")));
    }
    Ok(BridgeBlock {
        id: id.to_string(),
        glyph: glyph.to_string(),
        address_grammar: address_grammar.to_string(),
        capabilities,
        extra: std::collections::BTreeMap::new(),
    })
}

impl VerifiedManifest {
    /// Admit every declared kind into `overlay` (§ The kinds vocabulary →
    /// *The registry overlay*).
    ///
    /// # Errors
    /// A kind already admitted under a different policy.
    pub fn admit_into(&self, overlay: &mut AdmittedKinds) -> Result<(), AdmitError> {
        for k in &self.kinds {
            overlay.admit(k.kind.clone(), k.merge)?;
        }
        Ok(())
    }

    /// The `execution` member's form — `None` when the manifest declares no
    /// `execution` (a remote or device client's document).
    ///
    /// # Errors
    /// `execution` present but not an object with a string `form`.
    pub fn execution_form(&self) -> Result<Option<&str>, ManifestError> {
        match self.payload.get("execution") {
            None => Ok(None),
            Some(Value::Object(obj)) => str_member(obj, "form").map(Some),
            Some(_) => Err(ManifestError::BadPayload(
                "execution must be an object".into(),
            )),
        }
    }

    /// The `wasm` form's `execution` member (`third-party.md` § The manifest),
    /// parsed — what the install leg fetches, verifies and confines the
    /// plugin to. `None` when the manifest declares no `execution` or another
    /// form.
    ///
    /// # Errors
    /// A `wasm` member with an unknown member, a `module` that is not an
    /// `https` URL, a `digest` that is not `sha256:` + 64 lowercase hex, or a
    /// `hosts` entry that is not a lowercase DNS name.
    pub fn wasm_execution(&self) -> Result<Option<WasmExecution>, ManifestError> {
        if self.execution_form()? != Some(EXECUTION_FORM_WASM) {
            return Ok(None);
        }
        let Some(Value::Object(obj)) = self.payload.get("execution") else {
            return Ok(None);
        };
        only_members(obj, &["form", "module", "digest", "hosts"])?;
        let module = str_member(obj, "module")?;
        if client_id_host(module).is_none() {
            return Err(ManifestError::BadPayload(
                "execution.module must be an https URL".into(),
            ));
        }
        let digest = str_member(obj, "digest")?;
        let hex = digest.strip_prefix("sha256:").unwrap_or_default();
        if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(ManifestError::BadPayload(
                "execution.digest must be sha256: and 64 lowercase hex digits".into(),
            ));
        }
        let hosts = match obj.get("hosts") {
            None => Vec::new(),
            Some(Value::Array(entries)) => entries
                .iter()
                .map(|h| {
                    h.as_str()
                        // The publisher grammar's labels admit digits, so an
                        // IPv4 literal would pass it; the member never does.
                        .filter(|h| {
                            is_valid_publisher(h) && h.parse::<std::net::Ipv4Addr>().is_err()
                        })
                        .map(str::to_string)
                        .ok_or_else(|| {
                            ManifestError::BadPayload(format!(
                                "execution.hosts entry {h} is not a lowercase DNS name"
                            ))
                        })
                })
                .collect::<Result<_, _>>()?,
            Some(_) => {
                return Err(ManifestError::BadPayload(
                    "execution.hosts must be an array".into(),
                ));
            }
        };
        Ok(Some(WasmExecution {
            module: module.to_string(),
            digest: digest.to_string(),
            hosts,
        }))
    }
}

/// The `execution.form` value of a nest-hosted WASM component.
pub const EXECUTION_FORM_WASM: &str = "wasm";

/// A `wasm` manifest's `execution` member (`third-party.md` § The manifest):
/// the component's URL, the SHA-256 the publisher's signature pins its bytes
/// to, and the plugin's whole outbound surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmExecution {
    /// The `https` URL the component is fetched from at install.
    pub module: String,
    /// `sha256:<64 lowercase hex>` — the fetched bytes must hash to it.
    pub digest: String,
    /// The hosts the plugin's `http.fetch` may reach; empty reaches nothing.
    pub hosts: Vec<String>,
}

impl WasmExecution {
    /// The pinned digest's raw 32 bytes.
    #[must_use]
    pub fn digest_bytes(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        // `wasm_execution` admitted exactly 64 lowercase hex digits.
        let hex = self.digest.trim_start_matches("sha256:");
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap_or(0);
        }
        out
    }
}

/// The value of a `fauna.state.kind-manifest` plane row
/// ([`crate::merge_policy::KIND_KIND_MANIFEST`], logical key = the document's
/// `client_id`): the compact JWS **verbatim** — never a parsed policy, so every
/// reader re-runs [`verify_manifest`] over the bytes it holds — and when the
/// admitting device admitted it (unix ms, advisory). Canonical dag-cbor.
///
/// Tolerant decode (no `deny_unknown_fields`): the row is whole-record LWW,
/// which adopts a newer writer's bytes verbatim, so a field a later build
/// adds survives an older replica's re-seal and is dropped only from its view.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KindManifestRecord {
    /// The metadata document's `fauna` member, as the consent verified it.
    pub jws: String,
    /// When the consenting device admitted it, unix ms.
    pub admitted_at_ms: i64,
    /// Forward-compat catch-all (transport.md § Schema and forward-compat
    /// discipline rule 4): a field a later build adds survives this build's
    /// re-seal. Empty ⇒ omitted on the wire, so existing bytes are unchanged.
    #[serde(
        flatten,
        default,
        skip_serializing_if = "std::collections::BTreeMap::is_empty"
    )]
    pub extra: std::collections::BTreeMap<String, crate::Value>,
}

/// The host a `client_id` (the metadata document's URL) names — what
/// [`verify_manifest`] anchors the publisher to. `None` for anything but an
/// `https` URL with a host: a row whose key names no publisher admits
/// nothing.
#[must_use]
pub fn client_id_host(client_id: &str) -> Option<String> {
    let url = url::Url::parse(client_id).ok()?;
    if url.scheme() != "https" {
        return None;
    }
    url.host_str().map(str::to_ascii_lowercase)
}

/// The `did:key` of an Ed25519 public key (`did:key:z6Mk…`).
pub fn ed25519_did_key(key: &[u8; 32]) -> String {
    let mut bytes = Vec::with_capacity(34);
    bytes.extend_from_slice(&ED25519_MULTICODEC);
    bytes.extend_from_slice(key);
    format!("{DID_KEY_PREFIX}{}", bs58::encode(bytes).into_string())
}

/// Decode an Ed25519 `did:key` to its raw 32 bytes; any other method, codec
/// or length refuses.
pub fn decode_ed25519_did_key(s: &str) -> Result<[u8; 32], ManifestError> {
    let bad = || ManifestError::BadKey(s.to_string());
    let b58 = s.strip_prefix(DID_KEY_PREFIX).ok_or_else(bad)?;
    let bytes = bs58::decode(b58).into_vec().map_err(|_| bad())?;
    let [0xED, 0x01, rest @ ..] = bytes.as_slice() else {
        return Err(bad());
    };
    rest.try_into().map_err(|_| bad())
}

fn b64(segment: &str, what: &'static str) -> Result<Vec<u8>, ManifestError> {
    URL_SAFE_NO_PAD
        .decode(segment)
        .map_err(|_| ManifestError::BadSegment(what))
}

fn str_member<'a>(obj: &'a Map<String, Value>, key: &str) -> Result<&'a str, ManifestError> {
    obj.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ManifestError::BadPayload(format!("{key} must be a string")))
}

fn only_members(obj: &Map<String, Value>, allowed: &[&str]) -> Result<(), ManifestError> {
    match obj.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(ManifestError::UnknownMember(k.clone())),
        None => Ok(()),
    }
}

/// Verify `jws` — the document's `fauna` member — as served from
/// `document_host` (the `client_id` URL's host, lowercase), and parse it.
///
/// # Errors
/// Every refusal of § The manifest and § The kinds vocabulary.
pub fn verify_manifest(jws: &str, document_host: &str) -> Result<VerifiedManifest, ManifestError> {
    let mut parts = jws.split('.');
    let (Some(header_b64), Some(payload_b64), Some(sig_b64), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(ManifestError::NotCompactJws);
    };

    // The header: exactly the three pinned members.
    let header: Map<String, Value> = serde_json::from_slice(&b64(header_b64, "header")?)
        .map_err(|_| ManifestError::BadSegment("header is not a JSON object"))?;
    if header.len() != 3 {
        let extra: Vec<&String> = header
            .keys()
            .filter(|k| !matches!(k.as_str(), "alg" | "kid" | "typ"))
            .collect();
        return Err(ManifestError::BadHeader(format!(
            "exactly alg, kid and typ are allowed; found {extra:?} beside {} member(s)",
            header.len()
        )));
    }
    if header.get("alg").and_then(Value::as_str) != Some(MANIFEST_ALG) {
        return Err(ManifestError::BadHeader(format!(
            "alg must be {MANIFEST_ALG}"
        )));
    }
    if header.get("typ").and_then(Value::as_str) != Some(MANIFEST_TYP) {
        return Err(ManifestError::BadHeader(format!(
            "typ must be {MANIFEST_TYP}"
        )));
    }
    let kid = header
        .get("kid")
        .and_then(Value::as_str)
        .ok_or_else(|| ManifestError::BadHeader("kid must be a did:key string".into()))?;
    let key_bytes = decode_ed25519_did_key(kid)?;

    // The signature, over the signing input as transmitted.
    let key =
        VerifyingKey::from_bytes(&key_bytes).map_err(|_| ManifestError::BadKey(kid.into()))?;
    let sig: [u8; 64] = b64(sig_b64, "signature")?
        .try_into()
        .map_err(|_| ManifestError::BadSegment("signature is not 64 bytes"))?;
    let signing_input = &jws[..header_b64.len() + 1 + payload_b64.len()];
    key.verify_strict(signing_input.as_bytes(), &Signature::from_bytes(&sig))
        .map_err(|_| ManifestError::BadSignature)?;

    // The payload: the extension object, verified bytes parsed.
    let payload: Map<String, Value> = serde_json::from_slice(&b64(payload_b64, "payload")?)
        .map_err(|_| ManifestError::BadPayload("payload is not a JSON object".into()))?;
    only_members(&payload, PAYLOAD_MEMBERS)?;
    if payload.get("version").and_then(Value::as_u64) != Some(MANIFEST_VERSION) {
        return Err(ManifestError::BadPayload(format!(
            "version must be {MANIFEST_VERSION}"
        )));
    }

    let publisher = payload
        .get("publisher")
        .and_then(Value::as_object)
        .ok_or_else(|| ManifestError::BadPayload("publisher must be an object".into()))?;
    only_members(publisher, &["domain", "key"])?;
    let domain = str_member(publisher, "domain")?;
    if domain != document_host || !is_valid_publisher(domain) {
        return Err(ManifestError::WrongDomain {
            domain: domain.to_string(),
            host: document_host.to_string(),
        });
    }
    if str_member(publisher, "key")? != kid {
        return Err(ManifestError::BadPayload(
            "publisher.key must equal the JWS header kid".into(),
        ));
    }

    let kinds = match payload.get("kinds") {
        None => Vec::new(),
        Some(Value::Array(entries)) => parse_kinds(entries, domain)?,
        Some(_) => return Err(ManifestError::BadPayload("kinds must be an array".into())),
    };
    let bridge = payload.get("bridge").map(parse_bridge).transpose()?;
    let service_auth = payload
        .get("service_auth")
        .map(parse_service_auth)
        .transpose()?
        .unwrap_or_default();
    let events_uri = match payload.get("events_uri") {
        None | Some(Value::Null) => None,
        Some(Value::String(uri)) => {
            validate_events_uri(uri, domain)?;
            Some(uri.clone())
        }
        Some(_) => {
            return Err(ManifestError::BadEventsUri(
                "must be a string or null".into(),
            ));
        }
    };

    Ok(VerifiedManifest {
        jws: jws.to_string(),
        publisher_domain: domain.to_string(),
        publisher_key: key_bytes,
        kinds,
        bridge,
        service_auth,
        events_uri,
        payload,
    })
}

/// The one rule for a manifest's `events_uri` (`transport.md` § Push events
/// → *Third-party event doors*, the webhook): an `https` URL whose host is
/// exactly `publisher_domain` — the domain the manifest's signature already
/// proves control of — on the default port, with no credentials and no
/// fragment. So a consented document can only ever aim the nest's signed
/// notifications at the publisher's own server, never at a third party's;
/// the dial's SSRF guard still runs on every POST, because a name's answer
/// can change after consent.
///
/// # Errors
/// [`ManifestError::BadEventsUri`] naming the rule broken.
pub fn validate_events_uri(uri: &str, publisher_domain: &str) -> Result<(), ManifestError> {
    let parsed =
        url::Url::parse(uri).map_err(|_| ManifestError::BadEventsUri("did not parse".into()))?;
    if parsed.scheme() != "https" {
        return Err(ManifestError::BadEventsUri("must be https".into()));
    }
    if parsed.host_str() != Some(publisher_domain) {
        return Err(ManifestError::BadEventsUri(format!(
            "host must be the publisher's domain {publisher_domain:?}"
        )));
    }
    if parsed.port().is_some() {
        return Err(ManifestError::BadEventsUri(
            "must use the default port".into(),
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ManifestError::BadEventsUri(
            "must carry no credentials".into(),
        ));
    }
    if parsed.fragment().is_some() {
        return Err(ManifestError::BadEventsUri("must carry no fragment".into()));
    }
    Ok(())
}

/// [`verify_manifest`], then **key continuity**: the verified key must be the
/// one the row (or the plane row's admitting device) pinned — a different
/// key is a re-consent, never a silent swap (§ The manifest).
///
/// # Errors
/// As [`verify_manifest`], plus [`ManifestError::KeyChanged`].
pub fn verify_manifest_pinned(
    jws: &str,
    document_host: &str,
    pinned_key: &[u8; 32],
) -> Result<VerifiedManifest, ManifestError> {
    let verified = verify_manifest(jws, document_host)?;
    if &verified.publisher_key != pinned_key {
        return Err(ManifestError::KeyChanged);
    }
    Ok(verified)
}

fn parse_kinds(entries: &[Value], publisher: &str) -> Result<Vec<ManifestKind>, ManifestError> {
    if entries.len() > MAX_MANIFEST_KINDS {
        return Err(ManifestError::BadPayload(format!(
            "at most {MAX_MANIFEST_KINDS} kinds per manifest"
        )));
    }
    let mut out: Vec<ManifestKind> = Vec::with_capacity(entries.len());
    for entry in entries {
        let obj = entry
            .as_object()
            .ok_or_else(|| ManifestError::BadPayload("a kinds entry must be an object".into()))?;
        only_members(obj, &["kind", "class", "merge", "floor"])?;
        let raw = str_member(obj, "kind")?;
        let kind: ExtKind = raw
            .parse()
            .map_err(|e| ManifestError::BadKind(format!("{raw:?}: {e}")))?;
        if kind.publisher() != publisher {
            return Err(ManifestError::BadKind(format!(
                "{kind} is outside the publisher's own domain {publisher:?}"
            )));
        }
        if out.iter().any(|k| k.kind == kind) {
            return Err(ManifestError::BadKind(format!("{kind} is declared twice")));
        }
        let refuse = |why: String| ManifestError::NotAdmitted {
            kind: kind.to_string(),
            why,
        };
        let class = str_member(obj, "class")?;
        if class != MANIFEST_CLASS_STATE {
            return Err(refuse(format!(
                "class {class:?} (only \"state\" is admitted)"
            )));
        }
        let spelling = str_member(obj, "merge")?;
        let merge = MergePolicy::from_manifest_spelling(spelling)
            .ok_or_else(|| refuse(format!("merge {spelling:?} is not one of the closed five")))?;
        if !merge.manifest_admitted() {
            return Err(refuse(format!(
                "merge {spelling:?} (only \"latest-wins\" is admitted)"
            )));
        }
        let floor = str_member(obj, "floor")?;
        if floor != MANIFEST_FLOOR_NONE {
            return Err(refuse(format!(
                "floor {floor:?} (only \"none\" is admitted)"
            )));
        }
        out.push(ManifestKind { kind, merge });
    }
    Ok(out)
}

/// Sign `payload` (any JSON value) as a manifest
/// under `signing_key`, with `header` overriding the pinned header when
/// given (a test reaching a refusal). What publisher tooling and the fixtures call.
pub fn sign_manifest(
    signing_key: &ed25519_dalek::SigningKey,
    payload: &Value,
    header: Option<&Value>,
) -> String {
    use ed25519_dalek::Signer as _;
    let kid = ed25519_did_key(&signing_key.verifying_key().to_bytes());
    let default_header =
        serde_json::json!({ "alg": MANIFEST_ALG, "kid": kid, "typ": MANIFEST_TYP });
    let header = header.unwrap_or(&default_header);
    let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(header).expect("json"));
    let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload).expect("json"));
    let input = format!("{header_b64}.{payload_b64}");
    let sig = signing_key.sign(input.as_bytes());
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[0x42; 32])
    }

    fn did() -> String {
        ed25519_did_key(&key().verifying_key().to_bytes())
    }

    fn payload(kinds: Value) -> Value {
        json!({
            "version": 1,
            "publisher": { "domain": "example.com", "key": did() },
            "kinds": kinds,
        })
    }

    fn notes() -> Value {
        json!({ "kind": "ext.example.com.notes", "class": "state", "merge": "latest-wins", "floor": "none" })
    }

    fn sign(p: &Value) -> String {
        sign_manifest(&key(), p, None)
    }

    #[test]
    fn a_valid_manifest_yields_its_kinds_and_admits_them() {
        let jws = sign(&payload(json!([
            notes(),
            { "kind": "ext.example.com.tags", "class": "state", "merge": "latest-wins", "floor": "none" },
        ])));
        let m = verify_manifest(&jws, "example.com").unwrap();
        assert_eq!(m.publisher_domain, "example.com");
        assert_eq!(m.publisher_key, key().verifying_key().to_bytes());
        assert_eq!(m.jws, jws);
        let kinds: Vec<String> = m.kinds.iter().map(|k| k.kind.to_string()).collect();
        assert_eq!(kinds, ["ext.example.com.notes", "ext.example.com.tags"]);
        let mut overlay = AdmittedKinds::new();
        m.admit_into(&mut overlay).unwrap();
        assert_eq!(
            overlay.merge_policy("ext.example.com.tags"),
            Some(MergePolicy::LatestWins)
        );
    }

    #[test]
    fn a_did_key_round_trips_and_is_the_ed25519_multikey_form() {
        let did = did();
        assert!(did.starts_with("did:key:z6Mk"), "{did}");
        assert_eq!(
            decode_ed25519_did_key(&did).unwrap(),
            key().verifying_key().to_bytes()
        );
        // A P-256 did:key (the issuer's own curve) is a different codec — refused.
        let mut p256 = vec![0x80, 0x24, 0x02];
        p256.extend_from_slice(&[0x11; 32]);
        let p256 = format!("did:key:z{}", bs58::encode(p256).into_string());
        assert!(decode_ed25519_did_key(&p256).is_err());
        // Right codec, wrong length.
        let short = format!(
            "did:key:z{}",
            bs58::encode([0xED, 0x01, 0x22]).into_string()
        );
        assert!(decode_ed25519_did_key(&short).is_err());
        assert!(decode_ed25519_did_key("did:web:example.com").is_err());
    }

    /// The row's red test: a manifest naming another publisher's kind refuses
    /// at parse.
    #[test]
    fn a_kind_outside_the_publishers_domain_refuses() {
        let jws = sign(&payload(json!([
            { "kind": "ext.other.org.thing", "class": "state", "merge": "latest-wins", "floor": "none" }
        ])));
        assert!(matches!(
            verify_manifest(&jws, "example.com"),
            Err(ManifestError::BadKind(_))
        ));
        // A subdomain is its own publisher — host equality, not registrable domain.
        let jws = sign(&payload(json!([
            { "kind": "ext.api.example.com.thing", "class": "state", "merge": "latest-wins", "floor": "none" }
        ])));
        assert!(matches!(
            verify_manifest(&jws, "example.com"),
            Err(ManifestError::BadKind(_))
        ));
    }

    #[test]
    fn a_document_from_another_host_refuses() {
        let jws = sign(&payload(json!([notes()])));
        assert!(matches!(
            verify_manifest(&jws, "evil.example"),
            Err(ManifestError::WrongDomain { .. })
        ));
    }

    #[test]
    fn a_bare_object_or_a_non_jws_refuses() {
        let bare = serde_json::to_string(&payload(json!([notes()]))).unwrap();
        assert_eq!(
            verify_manifest(&bare, "example.com"),
            Err(ManifestError::NotCompactJws)
        );
        assert!(verify_manifest("a.b", "example.com").is_err());
        assert!(verify_manifest("a.b.c.d", "example.com").is_err());
    }

    #[test]
    fn the_header_is_pinned_exactly() {
        let p = payload(json!([notes()]));
        for header in [
            json!({ "alg": "ES256", "kid": did(), "typ": MANIFEST_TYP }),
            json!({ "alg": "none", "kid": did(), "typ": MANIFEST_TYP }),
            json!({ "alg": "EdDSA", "kid": did() }),
            json!({ "alg": "EdDSA", "kid": did(), "typ": "JWT" }),
            json!({ "alg": "EdDSA", "kid": did(), "typ": MANIFEST_TYP, "crit": ["exp"] }),
            json!({ "alg": "EdDSA", "jwk": {}, "typ": MANIFEST_TYP }),
        ] {
            let jws = sign_manifest(&key(), &p, Some(&header));
            assert!(
                matches!(
                    verify_manifest(&jws, "example.com"),
                    Err(ManifestError::BadHeader(_))
                ),
                "must refuse header {header}"
            );
        }
    }

    #[test]
    fn a_signature_by_another_key_or_over_other_bytes_refuses() {
        let p = payload(json!([notes()]));
        // Signed by key B, but claiming key A's kid.
        let other = ed25519_dalek::SigningKey::from_bytes(&[0x43; 32]);
        let header = json!({ "alg": MANIFEST_ALG, "kid": did(), "typ": MANIFEST_TYP });
        let jws = sign_manifest(&other, &p, Some(&header));
        assert_eq!(
            verify_manifest(&jws, "example.com"),
            Err(ManifestError::BadSignature)
        );
        // A payload swapped after signing.
        let good = sign(&p);
        let swapped_payload = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&payload(json!([notes(), {
                "kind": "ext.example.com.x", "class": "state", "merge": "latest-wins", "floor": "none"
            }])))
            .unwrap(),
        );
        let mut segs: Vec<&str> = good.split('.').collect();
        segs[1] = &swapped_payload;
        assert_eq!(
            verify_manifest(&segs.join("."), "example.com"),
            Err(ManifestError::BadSignature)
        );
    }

    #[test]
    fn publisher_key_must_equal_the_kid() {
        let mut p = payload(json!([notes()]));
        p["publisher"]["key"] = json!(ed25519_did_key(&[0x01; 32]));
        assert!(matches!(
            verify_manifest(&sign(&p), "example.com"),
            Err(ManifestError::BadPayload(_))
        ));
    }

    #[test]
    fn the_kinds_vocabulary_is_closed() {
        for (entry, why) in [
            (
                json!({ "kind": "ext.example.com.notes", "class": "state", "merge": "crdt-per-field", "floor": "none" }),
                "crdt-per-field",
            ),
            (
                json!({ "kind": "ext.example.com.notes", "class": "state", "merge": "three-way", "floor": "none" }),
                "three-way",
            ),
            (
                json!({ "kind": "ext.example.com.notes", "class": "state", "merge": "lww", "floor": "none" }),
                "unknown merge",
            ),
            (
                json!({ "kind": "ext.example.com.notes", "class": "record", "merge": "latest-wins", "floor": "none" }),
                "class record",
            ),
            (
                json!({ "kind": "ext.example.com.notes", "class": "state", "merge": "latest-wins", "floor": "scope" }),
                "floor",
            ),
        ] {
            assert!(
                matches!(
                    verify_manifest(&sign(&payload(json!([entry]))), "example.com"),
                    Err(ManifestError::NotAdmitted { .. })
                ),
                "must refuse {why}"
            );
        }
        let rung = json!({ "kind": "ext.example.com.notes", "class": "state", "merge": "latest-wins", "floor": "none", "rung": "delegable" });
        assert!(matches!(
            verify_manifest(&sign(&payload(json!([rung]))), "example.com"),
            Err(ManifestError::UnknownMember(m)) if m == "rung"
        ));
        assert!(matches!(
            verify_manifest(&sign(&payload(json!([notes(), notes()]))), "example.com"),
            Err(ManifestError::BadKind(_))
        ));
        let many: Vec<Value> = (0..=MAX_MANIFEST_KINDS)
            .map(|i| json!({ "kind": format!("ext.example.com.k{i}"), "class": "state", "merge": "latest-wins", "floor": "none" }))
            .collect();
        assert!(verify_manifest(&sign(&payload(Value::Array(many))), "example.com").is_err());
    }

    #[test]
    fn unknown_payload_members_and_versions_refuse() {
        let mut p = payload(json!([notes()]));
        p["audience"] = json!("everyone");
        assert!(matches!(
            verify_manifest(&sign(&p), "example.com"),
            Err(ManifestError::UnknownMember(m)) if m == "audience"
        ));
        let mut p = payload(json!([notes()]));
        p["version"] = json!(2);
        assert!(matches!(
            verify_manifest(&sign(&p), "example.com"),
            Err(ManifestError::BadPayload(_))
        ));
        // Known members other slices interpret are accepted verbatim.
        let mut p = payload(json!([notes()]));
        p["execution"] = json!({ "form": "device" });
        p["events_uri"] = Value::Null;
        assert!(verify_manifest(&sign(&p), "example.com").is_ok());
    }

    fn with_events_uri(events_uri: Value) -> Result<VerifiedManifest, ManifestError> {
        let mut p = payload(json!([notes()]));
        p["events_uri"] = events_uri;
        verify_manifest(&sign(&p), "example.com")
    }

    /// `transport.md` § Push events → *Third-party event doors*, the
    /// webhook: `events_uri` is an `https` URL on the publisher's own domain
    /// — the nest is never aimed at a third party's URL — and a document
    /// declaring none (absent or `null`) has no webhook.
    #[test]
    fn events_uri_must_be_https_on_the_publishers_own_domain() {
        assert_eq!(
            with_events_uri(json!("https://example.com/fauna/events"))
                .unwrap()
                .events_uri
                .as_deref(),
            Some("https://example.com/fauna/events")
        );
        assert_eq!(with_events_uri(Value::Null).unwrap().events_uri, None);
        assert_eq!(
            verify_manifest(&sign(&payload(json!([notes()]))), "example.com")
                .unwrap()
                .events_uri,
            None
        );
        for refused in [
            json!("http://example.com/events"),
            json!("https://other.example/events"),
            json!("https://api.example.com/events"),
            json!("https://example.com:8443/events"),
            json!("https://user:pw@example.com/events"),
            json!("https://example.com/events#frag"),
            json!("not a url"),
            json!(7),
        ] {
            assert!(
                matches!(
                    with_events_uri(refused.clone()),
                    Err(ManifestError::BadEventsUri(_))
                ),
                "{refused} must refuse"
            );
        }
    }

    #[test]
    fn key_continuity_refuses_a_rotated_key_until_re_consent() {
        let jws = sign(&payload(json!([notes()])));
        assert!(
            verify_manifest_pinned(&jws, "example.com", &key().verifying_key().to_bytes()).is_ok()
        );
        assert_eq!(
            verify_manifest_pinned(&jws, "example.com", &[0x09; 32]),
            Err(ManifestError::KeyChanged)
        );
    }

    fn with_service_auth(service_auth: Value) -> Result<VerifiedManifest, ManifestError> {
        let mut p = payload(json!([]));
        p["service_auth"] = service_auth;
        verify_manifest(&sign(&p), "example.com")
    }

    /// `third-party.md` § The manifest: the `service_auth` member parses into
    /// typed entries in manifest order, and a document without it declares
    /// none.
    #[test]
    fn a_service_auth_member_parses_into_typed_entries() {
        let m = with_service_auth(json!([
            { "aud": "did:web:api.bsky.app#bsky_appview", "lxm": ["app.bsky.feed.getFeedSkeleton"] },
            { "aud": "did:plc:abcdefghijklmnopqrstuvwx", "lxm": ["com.example.blog.getPost", "com.example.blog.listPosts"] },
            { "aud": "did:web:localhost%3A2583", "lxm": ["com.example.x.y"] },
        ]))
        .unwrap();
        assert_eq!(m.service_auth.len(), 3);
        assert_eq!(m.service_auth[0].aud, "did:web:api.bsky.app#bsky_appview");
        assert!(m.service_auth[1].admits(
            "did:plc:abcdefghijklmnopqrstuvwx",
            "com.example.blog.listPosts"
        ));
        assert!(!m.service_auth[1].admits(
            "did:plc:abcdefghijklmnopqrstuvwx",
            "com.example.blog.deletePost"
        ));
        // The audience is compared whole, fragment included.
        assert!(!m.service_auth[0].admits("did:web:api.bsky.app", "app.bsky.feed.getFeedSkeleton"));
        let none = verify_manifest(&sign(&payload(json!([]))), "example.com").unwrap();
        assert!(none.service_auth.is_empty());
    }

    /// Every structural refusal of the `service_auth` member refuses the whole
    /// document.
    #[test]
    fn each_service_auth_refusal_refuses_the_whole_document() {
        let ok_lxm = json!(["app.bsky.feed.getFeedSkeleton"]);
        for (member, why) in [
            (json!({ "aud": "did:web:api.bsky.app" }), "not an array"),
            (json!(["did:web:api.bsky.app"]), "entry not an object"),
            (json!([{ "lxm": ok_lxm }]), "no aud"),
            (
                json!([{ "aud": "did:key:z6Mkabc", "lxm": ok_lxm }]),
                "did:key audience",
            ),
            (
                json!([{ "aud": "https://api.bsky.app", "lxm": ok_lxm }]),
                "URL audience",
            ),
            (
                json!([{ "aud": "did:plc:short", "lxm": ok_lxm }]),
                "short did:plc",
            ),
            (
                json!([{ "aud": "did:web:API.bsky.app", "lxm": ok_lxm }]),
                "uppercase host",
            ),
            (
                json!([{ "aud": "did:web:api.bsky.app#", "lxm": ok_lxm }]),
                "empty fragment",
            ),
            (json!([{ "aud": "did:web:api.bsky.app" }]), "no lxm"),
            (
                json!([{ "aud": "did:web:api.bsky.app", "lxm": [] }]),
                "empty lxm",
            ),
            (
                json!([{ "aud": "did:web:api.bsky.app", "lxm": ["app.bsky.feed.*"] }]),
                "wildcard",
            ),
            (
                json!([{ "aud": "did:web:api.bsky.app", "lxm": ["app.bsky.feed."] }]),
                "prefix",
            ),
            (
                json!([{ "aud": "did:web:api.bsky.app", "lxm": ["getFeed"] }]),
                "no authority",
            ),
            (
                json!([{ "aud": "did:web:api.bsky.app", "lxm": [7] }]),
                "non-string lxm",
            ),
            (
                json!([{ "aud": "did:web:api.bsky.app", "lxm": ["app.bsky.a.b", "app.bsky.a.b"] }]),
                "duplicate lxm",
            ),
            (
                json!([
                    { "aud": "did:web:api.bsky.app", "lxm": ok_lxm },
                    { "aud": "did:web:api.bsky.app", "lxm": ok_lxm },
                ]),
                "duplicate aud",
            ),
        ] {
            assert!(
                matches!(with_service_auth(member), Err(ManifestError::BadPayload(_))),
                "must refuse: {why}"
            );
        }
        // An entry's unknown member refuses like any other.
        assert!(matches!(
            with_service_auth(json!([{ "aud": "did:web:api.bsky.app", "lxm": ok_lxm, "exp": 60 }])),
            Err(ManifestError::UnknownMember(m)) if m == "exp"
        ));
        // The two caps.
        let audiences: Vec<Value> = (0..=MAX_SERVICE_AUTH_AUDIENCES)
            .map(|i| json!({ "aud": format!("did:web:s{i}.example.com"), "lxm": ok_lxm }))
            .collect();
        assert!(with_service_auth(Value::Array(audiences)).is_err());
        let methods: Vec<String> = (0..=MAX_SERVICE_AUTH_METHODS)
            .map(|i| format!("com.example.api.m{i}"))
            .collect();
        assert!(
            with_service_auth(json!([{ "aud": "did:web:api.bsky.app", "lxm": methods }])).is_err()
        );
        let at_cap: Vec<String> = (0..MAX_SERVICE_AUTH_METHODS)
            .map(|i| format!("com.example.api.m{i}"))
            .collect();
        assert!(
            with_service_auth(json!([{ "aud": "did:web:api.bsky.app", "lxm": at_cap }])).is_ok()
        );
    }

    #[test]
    fn the_nsid_grammar_is_the_lexicon_one() {
        for ok in [
            "app.bsky.feed.getFeedSkeleton",
            "com.atproto.server.getServiceAuth",
            "com.example-co.x.y2",
        ] {
            assert!(is_valid_nsid(ok), "{ok}");
        }
        for bad in [
            "",
            "a.b",
            "app.bsky.feed.",
            "app.bsky.*",
            "1app.bsky.x",
            "app.-bsky.x",
            "app.bsky.2x",
            "app.bsky.get-feed",
            "app..bsky.x",
        ] {
            assert!(!is_valid_nsid(bad), "{bad}");
        }
    }

    /// The `wasm` form's `execution` member (`third-party.md` § The manifest):
    /// read when the form is `wasm`, absent otherwise, refused when any part
    /// of it is malformed.
    #[test]
    fn the_wasm_execution_member_parses_and_refuses() {
        let digest = format!("sha256:{}", "ab".repeat(32));
        let with = |execution: Value| {
            let mut p = payload(json!([]));
            p["execution"] = execution;
            verify_manifest(&sign(&p), "example.com").unwrap()
        };
        let m = with(json!({
            "form": "wasm", "module": "https://example.com/p.wasm",
            "digest": digest, "hosts": ["api.example.com"],
        }));
        let exec = m.wasm_execution().unwrap().unwrap();
        assert_eq!(exec.module, "https://example.com/p.wasm");
        assert_eq!(exec.hosts, ["api.example.com"]);
        assert_eq!(exec.digest_bytes(), [0xAB; 32]);
        assert_eq!(m.execution_form().unwrap(), Some("wasm"));

        // No `hosts` reaches nothing; another form, or none, is not wasm.
        let m = with(
            json!({ "form": "wasm", "module": "https://example.com/p.wasm", "digest": digest }),
        );
        assert!(m.wasm_execution().unwrap().unwrap().hosts.is_empty());
        assert_eq!(
            with(json!({ "form": "container" })).wasm_execution(),
            Ok(None)
        );
        let none = verify_manifest(&sign(&payload(json!([]))), "example.com").unwrap();
        assert_eq!(none.wasm_execution(), Ok(None));
        assert_eq!(none.execution_form(), Ok(None));

        for bad in [
            json!({ "form": "wasm", "module": "http://example.com/p.wasm", "digest": digest }),
            json!({ "form": "wasm", "module": "https://example.com/p.wasm", "digest": "sha256:AB" }),
            json!({ "form": "wasm", "module": "https://example.com/p.wasm", "digest": digest.to_uppercase() }),
            json!({ "form": "wasm", "module": "https://example.com/p.wasm", "digest": digest, "hosts": ["10.0.0.1"] }),
            json!({ "form": "wasm", "module": "https://example.com/p.wasm", "digest": digest, "hosts": ["API.example.com"] }),
            json!({ "form": "wasm", "module": "https://example.com/p.wasm", "digest": digest, "hosts": "api.example.com" }),
            json!({ "form": "wasm", "module": "https://example.com/p.wasm", "digest": digest, "image": "x" }),
        ] {
            assert!(with(bad.clone()).wasm_execution().is_err(), "{bad}");
        }
    }

    /// A conforming `bridge` block — what `third-party.md`'s example declares,
    /// in the record's own field names.
    pub(crate) fn matrix_bridge() -> Value {
        json!({
            "id": "matrix",
            "glyph": "bridge",
            "address_grammar": "^@[^:]+:.+$",
            "capabilities": {
                "supports_attachments": true,
                "supports_markdown": false,
                "supports_reactions": true,
                "supports_message_delete": true,
                "supports_per_message_reply": true,
                "supports_membership_change": true,
                "supports_recipient_selection": false,
                "supports_rename": true,
                "supports_subject": false,
                "delivery_mode": "Async",
            },
        })
    }

    fn with_bridge(bridge: Value) -> Result<VerifiedManifest, ManifestError> {
        let mut p = payload(json!([]));
        p["bridge"] = bridge;
        verify_manifest(&sign(&p), "example.com")
    }

    fn bridge_with(edit: impl FnOnce(&mut Value)) -> Result<VerifiedManifest, ManifestError> {
        let mut b = matrix_bridge();
        edit(&mut b);
        with_bridge(b)
    }

    #[test]
    fn a_valid_bridge_block_is_carried_typed_and_its_grammar_matches_nest_side() {
        let m = with_bridge(matrix_bridge()).unwrap();
        let b = m.bridge.expect("bridge block");
        assert_eq!(b.id, "matrix");
        assert_eq!(
            b.source_glyph(),
            fauna_core::source_glyph::SourceGlyph::Bridge
        );
        assert_eq!(
            b.capabilities.get("delivery_mode"),
            Some(&BridgeCapabilityValue::Mode("Async".into()))
        );
        assert!(b.address_matches("@alice:matrix.org"));
        assert!(!b.address_matches("alice"));
        // A manifest without the member is no bridge.
        assert_eq!(
            verify_manifest(&sign(&payload(json!([]))), "example.com")
                .unwrap()
                .bridge,
            None
        );
    }

    #[test]
    fn each_bridge_block_refusal_refuses_the_whole_document() {
        let refused = |r: Result<VerifiedManifest, ManifestError>| r.expect_err("refused");
        // id: the label rule, and the reserved first-party ids.
        refused(bridge_with(|b| b["id"] = json!("Matrix")));
        refused(bridge_with(|b| b["id"] = json!("matrix.org")));
        for reserved in RESERVED_BRIDGE_IDS {
            refused(bridge_with(|b| b["id"] = json!(reserved)));
        }
        // glyph: one of the fixed set's ids, never an image.
        refused(bridge_with(|b| {
            b["glyph"] = json!("https://bridge.example/icon.png")
        }));
        // address_grammar: bounded, and must compile.
        refused(bridge_with(|b| {
            b["address_grammar"] = json!("a".repeat(257))
        }));
        refused(bridge_with(|b| b["address_grammar"] = json!("^(unclosed")));
        refused(bridge_with(|b| {
            b["address_grammar"] = json!("a{10000}{10000}")
        }));
        // capabilities: never encryption, nothing unknown, nothing missing,
        // the right types.
        refused(bridge_with(|b| {
            b["capabilities"]["encryption"] = json!("TransportOnly")
        }));
        assert_eq!(
            bridge_with(|b| b["capabilities"]["supports_teleport"] = json!(true)),
            Err(ManifestError::UnknownMember(
                "bridge.capabilities.supports_teleport".into()
            ))
        );
        refused(bridge_with(|b| {
            b["capabilities"]
                .as_object_mut()
                .unwrap()
                .remove("supports_rename");
        }));
        refused(bridge_with(|b| {
            b["capabilities"]["delivery_mode"] = json!("Instant")
        }));
        refused(bridge_with(|b| {
            b["capabilities"]["supports_markdown"] = json!("yes")
        }));
        // An unknown block member refuses like any other.
        assert_eq!(
            bridge_with(|b| b["label"] = json!("Matrix")),
            Err(ManifestError::UnknownMember("label".into()))
        );
        // The role-gated fields are optional but typed.
        assert!(bridge_with(|b| b["capabilities"]["can_invite"] = json!(false)).is_ok());
        refused(bridge_with(|b| b["capabilities"]["can_invite"] = json!(1)));
    }
}
