//! Shared-Rust CardDAV **consumption** crate — a Fauna app reads its OWN
//! address books + vCards over the *encrypted* `fauna.bridges.*` CardDAV RPCs
//! against the `bridge_carddav_*` store, the SAME store + sealing scheme the
//! mail-bridge MDA serves to Apple Contacts / DAVx5, so a Fauna app and a
//! CardDAV MUA see the same data
//! (`docs/goal/behavior/carddav-server.md` § Independent enablement — the native
//! "Address Book" view: `list_addressbooks` / `query_cards`, decrypted locally /
//! zero-access).
//!
//! The read/consumption analogue of [`fauna-client-caldav`] (calendars/events);
//! it mirrors that crate's transport + seal surface 1:1 (priority #2 — one
//! kind-composition + unseal shared by native/UniFFI + web/wasm):
//! - the typed CardDAV call surface ([`CardDavClient`]), generic over the WS-RPC
//!   transport (`R: RpcRequester`);
//! - **sealing** ([`seal_card_body`] / [`unseal_card_body`]) — the EXACT HPKE
//!   scheme + single-[`MailRecordEnvelope`](fauna_mls::wrapped_blob::MailRecordEnvelope) wire shape the MDA's CardDAV PUT uses
//!   (`mailfauna.EncryptToRecipientHybrid`), so MDA-written and client-written
//!   card bodies are mutually readable;
//! - the collection-metadata mirror ([`AddressbookMetadata`] +
//!   [`unseal_addressbook_metadata`]) — the cross-language CBOR twin of the Go
//!   MDA's `EncryptedCollectionMetadata`;
//! - the WASM-safe [`vcard`] reader + the decoded read path
//!   ([`CardDavClient::query_cards_decoded`] / [`decode_card_entry`]).
//!
//! **Fully WASM-safe.** Unlike CalDAV (whose iCalendar parser pulls the
//! native-only `icalendar`/`rrule` crates, gating its read path behind
//! `cfg(not(wasm))`), the vCard grammar (RFC 6350) is simple line-based text, so
//! the [`vcard`] parser is hand-rolled and portable — the whole read path
//! (transport → unseal → parse → typed [`DecodedCard`]) links on web with no
//! native-only slice.

use fauna_protocol::RpcRequester;
use fauna_protocol::bridge_routing::{
    AddressbookEntry, CardEntry, ListAddressbooksReply, ListAddressbooksRequest, QueryCardsReply,
    QueryCardsRequest, SyncAddressbookSinceReply, SyncAddressbookSinceRequest,
};
use serde::{Deserialize, Serialize};

// Re-exported so a consumer reaches the CardDAV wire types (request/reply/entry
// shapes) through this one crate rather than depending on `fauna-protocol`
// directly (priority #2).
pub use fauna_protocol::bridge_routing;

/// The deterministic `uid_hash` of a vCard's plaintext `UID`: `blake3(uid)`
/// (256-bit), so a card a Fauna app writes and one a CardDAV MUA's PUT stores
/// dedup onto the same row. The plaintext UID stays inside the sealed body; only
/// this hash is a plaintext index.
///
/// Re-exported from [`fauna_protocol::dav_identity`] — the SAME function the
/// CalDAV crate re-exports, because both rails hash a resource `UID` by one
/// rule. Agreement with the MDA's Go `blake3.Sum256([]byte(uid))` is pinned by
/// `libs/fauna-protocol/tests/dav_identity_cross_language.rs`.
pub use fauna_protocol::dav_identity::uid_hash;

/// The lazy `Contacts` address book's id and default display name
/// (`blake3("contacts")` / `"Contacts"`) — carddav-server.md § Write surface.
/// The MDA lazily provisions this book on first access; a Fauna app addressing
/// the same book must derive the identical id. Re-exported from the shared
/// owner and pinned against the MDA's Go source by the same conformance test.
pub use fauna_protocol::dav_identity::{
    CONTACTS_ADDRESSBOOK_SLUG, DEFAULT_ADDRESSBOOK_DISPLAYNAME, contacts_addressbook_id,
};

// ── Sealing (carddav-server.md § Card resources — the same HPKE scheme as the MDA) ──

/// Seal/unseal failure for the card-body crypto layer — the shared
/// [`fauna_mls::wrapped_blob::dav_body::SealError`], which CalDAV's event bodies
/// use too. Was a byte-identical twin of CalDAV's until 2026-08-23.
pub use fauna_mls::wrapped_blob::dav_body::SealError;

/// Seal `plaintext` (the canonical vCard bytes, or the sealed metadata bytes) to
/// the actor's own `credential_id="default"` recipient pubkey derived from `msek`
/// (the `fauna.state.mail` MSEK), producing the `encrypted_body` wire bytes.
///
/// This is the EXACT scheme the MDA's CardDAV PUT uses
/// (`internal/mda/carddav/put.go` → `mailfauna.EncryptToRecipientHybrid`): a
/// single [`MailRecordEnvelope`](fauna_mls::wrapped_blob::MailRecordEnvelope) (HPKE `DHKEM(X25519,HKDF-SHA256)`, `HKDF-SHA256`,
/// `ChaCha20-Poly1305`) serialized canonical. Same `msek` yields the same
/// recipient keypair, so the bytes are interchangeable with MDA-written ones.
pub use fauna_mls::wrapped_blob::dav_body::seal_dav_body as seal_card_body;

/// Post-quantum (X-Wing, ML-KEM-768 ∥ X25519) sibling of [`seal_card_body`].
/// Derives the actor's **own** X-Wing keypair from `msek` and seals the body to
/// it; the produced [`MailRecordEnvelope`](fauna_mls::wrapped_blob::MailRecordEnvelope) self-describes its suite, so the MDA
/// and every Fauna app open it through the same suite-dispatching
/// [`unseal_card_body`]. Selected by the write path iff the actor has published
/// an ML-KEM ek (`post-quantum.md` § Capability negotiation); otherwise the
/// classical [`seal_card_body`] runs.
pub use fauna_mls::wrapped_blob::dav_body::seal_dav_body_xwing as seal_card_body_xwing;

/// Unseal an `encrypted_body` produced by [`seal_card_body`] **or by the MDA** —
/// both derive the same recipient keypair from `msek`, so a Fauna app reads
/// MDA-written cards and vice versa. Returns the plaintext vCard bytes. Opens
/// either suite (classical X25519 or hybrid X-Wing), both derived from the same
/// MSEK, so cards ride the same post-quantum migration as mail records.
pub use fauna_mls::wrapped_blob::dav_body::unseal_dav_body as unseal_card_body;

/// The actor's own recipient keypair for this crate's sealed bodies (address
/// book metadata, card bodies), derived once from `msek` and reused across a
/// whole batch — [`unseal_addressbook_metadata`] and [`decode_card_entry`]
/// take one of these instead of a bare `msek`, so `query_cards` renders N rows
/// with a single keygen instead of N .
pub use fauna_mls::wrapped_blob::dav_body::DavRecipientKeys;

// ── Address-book collection metadata (carddav-server.md § Standard properties) ─

/// The decrypted shape of an address book's `encrypted_metadata` blob
/// (`bridge_carddav_addressbooks.encrypted_metadata`) — the MUA-visible DAV
/// properties a Fauna app and the MDA both read.
///
/// **Cross-language interop contract.** This is the Rust mirror of the MDA's Go
/// `EncryptedCollectionMetadata`
/// (`bins/fauna-bridges/internal/mda/carddav/encrypted_metadata.go`):
/// identical CBOR field names (`displayname` / `description`, the latter omitted
/// when empty), sealed with the SAME single-[`MailRecordEnvelope`](fauna_mls::wrapped_blob::MailRecordEnvelope) scheme
/// ([`seal_card_body`] ≡ Go `SealCollectionMetadata`). **Both sides MUST encode
/// canonical dag-CBOR (length-first key sort)** — [`unseal_addressbook_metadata`]
/// decodes with `fauna_cbor::decode_strict`, which rejects non-canonical key order, so the
/// MDA seals via `internal/dagcbor.Marshal`, not bare `cbor.Marshal` (a
/// non-canonical seal renders as 0 address books on every native app even
/// though CardDAV MUAs read it fine). Unlike a calendar, an address book carries
/// **no color** (the Go twin omits that field too).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressbookMetadata {
    /// Human-readable address-book name (DAV `displayname`).
    pub displayname: String,
    /// Optional CardDAV `addressbook-description`; empty ⇒ omitted on the wire
    /// (matches the Go `cbor:"description,omitempty"`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// Seal an [`AddressbookMetadata`] to the actor's own recipient key (the SAME
/// scheme as [`seal_card_body`] / the MDA's `SealCollectionMetadata`), producing
/// the `encrypted_metadata` wire bytes for `provision_addressbook`.
pub fn seal_addressbook_metadata(
    meta: &AddressbookMetadata,
    msek: &[u8; 32],
) -> Result<Vec<u8>, SealError> {
    fauna_mls::wrapped_blob::dav_body::seal_dav_body_typed(meta, msek)
}

/// Unseal an address book's `encrypted_metadata` (written by a Fauna app or
/// the MDA) back into a typed [`AddressbookMetadata`]. An empty blob yields the
/// default (treated as "no metadata stored yet", mirroring the Go
/// `UnsealCollectionMetadata` empty-ciphertext path). Takes a pre-derived
/// [`DavRecipientKeys`] rather than a bare `msek` — a caller listing N address
/// books derives one keypair up front instead of N .
pub fn unseal_addressbook_metadata(
    sealed: &[u8],
    keys: &DavRecipientKeys,
) -> Result<AddressbookMetadata, SealError> {
    fauna_mls::wrapped_blob::dav_body::unseal_dav_body_typed_or_default(sealed, keys)
}

// ── vCard reader (RFC 6350 — WASM-safe, hand-rolled) ──────────────────────────

/// A small, WASM-safe vCard (RFC 6350 / 2426) reader for the Address Book view.
///
/// Deliberately lenient: unknown properties are ignored and malformed lines
/// skipped — the goal is a best-effort render of the fields the detail pane shows
/// (`FN` / `N` / `EMAIL` / `TEL` / `ADR` / `ORG` / `TITLE` / `NOTE` / `BDAY`),
/// not RFC validation (the MDA + MUA already validate on PUT). Handles §3.2 line
/// unfolding, `;`-separated params with `TYPE`/`PREF`, `;`-separated structured
/// values (`N` / `ADR` / `ORG`), and §3.4 text escaping (`\n \, \; \\`).
pub mod vcard {
    /// A property value paired with its `TYPE` parameters and preference flag —
    /// e.g. a `TEL;TYPE=work,voice;PREF=1:+1-555-0100`.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct TypedValue {
        /// The unescaped property value (the phone number, email, URL, …).
        pub value: String,
        /// Lower-cased `TYPE` parameter values (e.g. `work`, `home`, `cell`), with
        /// the `pref` pseudo-type removed (surfaced via [`Self::pref`] instead).
        pub types: Vec<String>,
        /// `true` when the property is marked preferred (vCard 4.0 `PREF=1` or the
        /// 3.0 `TYPE=pref` idiom).
        pub pref: bool,
    }

    /// The structured `N` property: `family;given;additional;prefixes;suffixes`.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct StructuredName {
        pub family: String,
        pub given: String,
        pub additional: String,
        pub prefixes: String,
        pub suffixes: String,
    }

    /// The structured `ADR` property: a mailing address plus its `TYPE`s.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct Address {
        pub types: Vec<String>,
        pub pref: bool,
        pub po_box: String,
        pub extended: String,
        pub street: String,
        pub locality: String,
        pub region: String,
        pub postal_code: String,
        pub country: String,
    }

    impl Address {
        /// The non-empty components joined into a single human line — `po_box,
        /// extended, street, locality, region postal_code, country` (region + postal
        /// code read as one token, e.g. `IL 62704`), skipping empties. The single
        /// source of truth for the `vcard-detail-adr` render, so every app (native
        /// via UniFFI, web via wasm) shows byte-identical address text (priority #2).
        #[must_use]
        pub fn one_line(&self) -> String {
            let region_zip = [self.region.as_str(), self.postal_code.as_str()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            [
                self.po_box.as_str(),
                self.extended.as_str(),
                self.street.as_str(),
                self.locality.as_str(),
                region_zip.as_str(),
                self.country.as_str(),
            ]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ")
        }
    }

    /// The fields the Address Book renders, parsed from a decrypted vCard body.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct ParsedVCard {
        /// `UID` (plaintext; the sealed body's own identifier).
        pub uid: String,
        /// `FN` — the display name (RFC 6350 requires exactly one; first wins).
        pub formatted_name: String,
        /// `N` — the structured name, when present.
        pub name: Option<StructuredName>,
        pub emails: Vec<TypedValue>,
        pub tels: Vec<TypedValue>,
        pub addresses: Vec<Address>,
        pub urls: Vec<TypedValue>,
        /// `ORG` components (`organization;unit;subunit`).
        pub org: Vec<String>,
        /// `TITLE`.
        pub title: String,
        /// `NOTE`.
        pub note: String,
        /// `BDAY`, verbatim (a date or date-time as written).
        pub bday: String,
        /// `X-FAUNA-ACTOR-ID` — the optional link back to a social contact
        /// (carddav-server.md § What a CardDAV address book is). Parsed here so
        /// the post-v1 cross-link slice has it; unused by the v1 UI.
        pub fauna_actor_id: Option<String>,
    }

    impl ParsedVCard {
        /// The non-empty `ORG` components joined with ` · `, skipping empties —
        /// the single source of truth for the `vcard-detail-org` render, the twin
        /// of [`Address::one_line`] and here for the same reason (priority #2): so
        /// every app shows byte-identical organization text. The separator is
        /// the one web and linux already render, so adopting this changes no
        /// client's output; an all-empty `ORG` yields `""`, which every app
        /// treats as "hide the row".
        #[must_use]
        pub fn org_line(&self) -> String {
            self.org
                .iter()
                .map(String::as_str)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · ")
        }
    }

    /// Parse a decrypted vCard body into the fields the Address Book renders.
    /// Never fails — an unrecognized or malformed line is skipped.
    #[must_use]
    pub fn parse_vcard(text: &str) -> ParsedVCard {
        let mut out = ParsedVCard::default();
        for logical in unfold_lines(text).lines() {
            let Some(line) = ContentLine::parse(logical) else {
                continue;
            };
            match line.name.as_str() {
                "FN" if out.formatted_name.is_empty() => {
                    out.formatted_name = unescape_text(&line.value);
                }
                "N" if out.name.is_none() => {
                    let c = split_components(&line.value);
                    out.name = Some(StructuredName {
                        family: comp(&c, 0),
                        given: comp(&c, 1),
                        additional: comp(&c, 2),
                        prefixes: comp(&c, 3),
                        suffixes: comp(&c, 4),
                    });
                }
                "EMAIL" => out.emails.push(line.typed_value()),
                "TEL" => out.tels.push(line.typed_value()),
                "URL" => out.urls.push(line.typed_value()),
                "ADR" => {
                    let c = split_components(&line.value);
                    out.addresses.push(Address {
                        types: line.type_params(),
                        pref: line.is_pref(),
                        po_box: comp(&c, 0),
                        extended: comp(&c, 1),
                        street: comp(&c, 2),
                        locality: comp(&c, 3),
                        region: comp(&c, 4),
                        postal_code: comp(&c, 5),
                        country: comp(&c, 6),
                    });
                }
                "ORG" => {
                    out.org = split_components(&line.value)
                        .iter()
                        .map(|s| unescape_text(s))
                        .collect();
                }
                "TITLE" if out.title.is_empty() => out.title = unescape_text(&line.value),
                "NOTE" if out.note.is_empty() => out.note = unescape_text(&line.value),
                "BDAY" if out.bday.is_empty() => out.bday = unescape_text(&line.value),
                "UID" if out.uid.is_empty() => out.uid = unescape_text(&line.value),
                "X-FAUNA-ACTOR-ID" if out.fauna_actor_id.is_none() => {
                    out.fauna_actor_id = Some(unescape_text(&line.value));
                }
                _ => {}
            }
        }
        out
    }

    /// One parsed content line: `[group "."] NAME *(";" param) ":" value`. The
    /// value is kept raw (unescaping is applied by the caller per property).
    struct ContentLine {
        /// Upper-cased property name (group prefix stripped).
        name: String,
        /// `(UPPER-name, values)` params — values already comma-split + dequoted.
        params: Vec<(String, Vec<String>)>,
        /// Raw property value (post-colon, pre-unescape).
        value: String,
    }

    impl ContentLine {
        fn parse(logical: &str) -> Option<ContentLine> {
            if logical.trim().is_empty() {
                return None;
            }
            let colon = find_value_colon(logical)?;
            let head = &logical[..colon];
            let value = &logical[colon + 1..];
            let mut parts = split_unquoted(head, ';');
            if parts.is_empty() {
                return None;
            }
            let name_with_group = parts.remove(0);
            let name = name_with_group
                .rsplit('.')
                .next()
                .unwrap_or(&name_with_group)
                .to_ascii_uppercase();
            if name.is_empty() {
                return None;
            }
            let mut params = Vec::new();
            for p in parts {
                if p.is_empty() {
                    continue;
                }
                let (pn, pv) = match p.split_once('=') {
                    Some((n, v)) => (n.to_ascii_uppercase(), parse_param_values(v)),
                    // vCard 2.1 bare type param (e.g. `;WORK`): treat as `TYPE=work`.
                    None => ("TYPE".to_string(), vec![p]),
                };
                params.push((pn, pv));
            }
            Some(ContentLine {
                name,
                params,
                value: value.to_string(),
            })
        }

        /// Lower-cased `TYPE` values with the `pref` pseudo-type removed.
        fn type_params(&self) -> Vec<String> {
            self.params
                .iter()
                .filter(|(n, _)| n == "TYPE")
                .flat_map(|(_, v)| v.iter())
                .map(|s| s.to_ascii_lowercase())
                .filter(|s| s != "pref")
                .collect()
        }

        /// vCard 4.0 `PREF=…` param or 3.0 `TYPE=pref` idiom.
        fn is_pref(&self) -> bool {
            self.params.iter().any(|(n, v)| {
                n == "PREF" || (n == "TYPE" && v.iter().any(|x| x.eq_ignore_ascii_case("pref")))
            })
        }

        fn typed_value(&self) -> TypedValue {
            TypedValue {
                value: unescape_text(&self.value),
                types: self.type_params(),
                pref: self.is_pref(),
            }
        }
    }

    /// Comma-split a param value string (respecting DQUOTE), trimming and
    /// stripping surrounding double-quotes from each value.
    fn parse_param_values(v: &str) -> Vec<String> {
        split_unquoted(v, ',')
            .into_iter()
            .map(|s| {
                let t = s.trim();
                t.strip_prefix('"')
                    .and_then(|x| x.strip_suffix('"'))
                    .unwrap_or(t)
                    .to_string()
            })
            .collect()
    }

    // `find_value_colon` / `split_unquoted` / `unfold_lines` — the content
    // line's DQUOTE quoting rule and its line-folding rule — are
    // `fauna_core::content_line`'s, shared with the iCalendar parser: RFC 6350
    // § 3.2/3.3 and RFC 5545 § 3.1 define the same grammar, so a private copy
    // here is a second place vCard and iCalendar can come to disagree about
    // one syntax. Pure `&str` work, no deps — the "fully WASM-safe, no
    // native-only slice" property above is untouched.
    use fauna_core::content_line::{find_value_colon, split_unquoted, unfold_lines};

    /// Split a structured value on unescaped `;` component separators, keeping
    /// backslash-escape pairs intact for [`unescape_text`] to resolve.
    fn split_components(value: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut chars = value.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\' {
                cur.push(c);
                if let Some(&next) = chars.peek() {
                    cur.push(next);
                    chars.next();
                }
            } else if c == ';' {
                out.push(std::mem::take(&mut cur));
            } else {
                cur.push(c);
            }
        }
        out.push(cur);
        out
    }

    /// The `i`-th structured component, unescaped, or empty if absent.
    fn comp(components: &[String], i: usize) -> String {
        components
            .get(i)
            .map(|s| unescape_text(s))
            .unwrap_or_default()
    }

    /// RFC 6350 §3.4 text unescaping: `\n`/`\N` → newline, `\,` → `,`,
    /// `\;` → `;`, `\\` → `\`; any other `\x` yields `x`.
    fn unescape_text(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n' | 'N') => out.push('\n'),
                    Some(',') => out.push(','),
                    Some(';') => out.push(';'),
                    Some('\\') => out.push('\\'),
                    Some(other) => out.push(other),
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        out
    }
}

// ── Decoded read types (carddav-server.md § The native Address Book view) ─────

/// A single address book decoded from a [`bridge_routing::AddressbookEntry`]: the
/// `list_addressbooks` row metadata with its `encrypted_metadata` unsealed into a
/// typed [`AddressbookMetadata`]. What the Address Book sidebar renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedAddressbook {
    /// Opaque 32-byte address-book identifier.
    pub addressbook_id: Vec<u8>,
    /// The unsealed collection metadata (name + optional description).
    pub metadata: AddressbookMetadata,
    /// Current ctag (bumped on any write to the book).
    pub ctag: i64,
    /// Highest modseq seen in this book (delta-sync reference).
    pub highestmodseq: i64,
    /// Total number of cards in the book.
    pub card_count: u32,
    /// Unix epoch seconds when the book was provisioned.
    pub created_at: i64,
}

/// Unseal one [`bridge_routing::AddressbookEntry`]'s metadata into a
/// [`DecodedAddressbook`]. Takes a pre-derived [`DavRecipientKeys`] rather than
/// a bare `msek` — [`CardDavClient::list_addressbooks_decoded`] derives one
/// per page and reuses it across every entry .
pub fn decode_addressbook_entry(
    entry: &AddressbookEntry,
    keys: &DavRecipientKeys,
) -> Result<DecodedAddressbook, SealError> {
    Ok(DecodedAddressbook {
        addressbook_id: entry.addressbook_id.clone(),
        metadata: unseal_addressbook_metadata(&entry.encrypted_metadata, keys)?,
        ctag: entry.ctag,
        highestmodseq: entry.highestmodseq,
        card_count: entry.card_count,
        created_at: entry.created_at,
    })
}

/// A single vCard decoded from a [`bridge_routing::CardEntry`]: the `query_cards`
/// / `sync_addressbook_since` row metadata paired with its unsealed body (both
/// the raw vCard text — for verbatim re-PUT / export — and the parsed
/// [`vcard::ParsedVCard`] the detail pane renders).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedCard {
    /// Server-assigned 32-byte card id (deterministic blake3 hash).
    pub card_id: Vec<u8>,
    /// Blake3 hash of the plaintext vCard UID (the dedup key).
    pub uid_hash: Vec<u8>,
    /// ETag for conditional requests (`If-Match` / `If-None-Match`).
    pub etag: String,
    /// Modseq at which this card was last written.
    pub modseq: i64,
    /// Epoch seconds (vCard `REV` surrogate).
    pub internal_date: i64,
    /// The unsealed canonical vCard body as UTF-8 text — the exact text the MDA
    /// serves to a CardDAV MUA. Kept alongside [`Self::parsed`] so a consumer can
    /// re-PUT verbatim or export `.vcf`.
    pub vcard: String,
    /// The parsed fields (`FN` / `EMAIL` / `TEL` / `ADR` / …).
    pub parsed: vcard::ParsedVCard,
    /// `true` when the row carried an `encrypted_fauna_ext` sidecar (the
    /// Fauna-only refinement, e.g. the `X-FAUNA-ACTOR-ID` link). Its decode is a
    /// post-v1 slice (the cross-link to social Contacts); v1 only surfaces
    /// presence.
    pub has_fauna_ext: bool,
}

/// Failure decoding one [`bridge_routing::CardEntry`] into a [`DecodedCard`] —
/// the seal layer rejected the body, or it was not valid UTF-8. (The vCard
/// *parse* itself never fails; it is lenient by design.) WASM-safe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeCardError {
    /// HPKE-open failed (wrong `msek` / tampered body) or the bytes were not a
    /// valid [`MailRecordEnvelope`](fauna_mls::wrapped_blob::MailRecordEnvelope).
    Unseal(SealError),
    /// The unsealed body was not UTF-8.
    Parse(String),
}

impl core::fmt::Display for DecodeCardError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DecodeCardError::Unseal(e) => write!(f, "{e}"),
            DecodeCardError::Parse(m) => write!(f, "parse card body: {m}"),
        }
    }
}

impl std::error::Error for DecodeCardError {}

/// Unseal + parse one [`bridge_routing::CardEntry`] (from a `query_cards` or
/// `sync_addressbook_since` reply) into a typed [`DecodedCard`]. The
/// `encrypted_body` is opened with a pre-derived [`DavRecipientKeys`] rather
/// than a bare `msek` — [`CardDavClient::query_cards_decoded`] derives one per
/// page and reuses it across every card instead of paying a fresh X-Wing
/// keygen per card  — then parsed via the WASM-safe
/// [`vcard::parse_vcard`].
pub fn decode_card_entry(
    entry: &CardEntry,
    keys: &DavRecipientKeys,
) -> Result<DecodedCard, DecodeCardError> {
    let plaintext = keys
        .unseal(&entry.encrypted_body)
        .map_err(DecodeCardError::Unseal)?;
    let vcard = String::from_utf8(plaintext)
        .map_err(|e| DecodeCardError::Parse(format!("vCard body not UTF-8: {e}")))?;
    let parsed = vcard::parse_vcard(&vcard);
    Ok(DecodedCard {
        card_id: entry.card_id.clone(),
        uid_hash: entry.uid_hash.clone(),
        etag: entry.etag.clone(),
        modseq: entry.modseq,
        internal_date: entry.internal_date,
        vcard,
        parsed,
        has_fauna_ext: entry.encrypted_fauna_ext.is_some(),
    })
}

// ── Display-row projection (the Address Book UI's flat render model) ─────────

/// One address book in the Address Book picker — the row tui's `addressbook-item`
/// and linux's picker both render (the CardDAV analogue of the CalDAV
/// `CalendarRow` sidebar entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressbookRow {
    /// Hex-encoded `addressbook_id` — the `query_cards` key, carried on the
    /// row as its click/gesture target.
    pub id: String,
    /// DAV `displayname`; may be empty, in which case each app substitutes its
    /// own localized "Address Book" fallback title when painting.
    pub name: String,
    /// Cards in the book — rendered as a count beside the name.
    pub card_count: u32,
}

/// One decoded vCard flattened for the card list + detail pane.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VCardRow {
    /// Hex-encoded `card_id` — stable per card, carried on the row's gesture.
    pub id: String,
    /// `FN` — the card list label and the detail header.
    pub formatted_name: String,
    /// `ORG` components via [`vcard::ParsedVCard::org_line`]; empty ⇒ hidden.
    pub org: String,
    /// `TITLE`; empty ⇒ hidden. tui's Address Book has no `vcard-detail-title`
    /// id and does not paint this field — it is still carried here because
    /// linux's detail pane does render it.
    pub title: String,
    /// `NOTE`; empty ⇒ hidden.
    pub note: String,
    /// `EMAIL` values — one detail row each.
    pub emails: Vec<String>,
    /// `TEL` values — one detail row each.
    pub tels: Vec<String>,
    /// `ADR` values, each already one-lined by [`vcard::Address::one_line`].
    pub addresses: Vec<String>,
}

/// Map a decoded address book onto its picker row.
#[must_use]
pub fn addressbook_row(d: &DecodedAddressbook) -> AddressbookRow {
    AddressbookRow {
        id: hex::encode(&d.addressbook_id),
        name: d.metadata.displayname.clone(),
        card_count: d.card_count,
    }
}

/// Map a decoded vCard onto its display row. Every formatting decision here
/// defers to this crate's own formatters (`org_line`, `one_line`), so every
/// app's rendered text is byte-identical.
#[must_use]
pub fn vcard_row(d: &DecodedCard) -> VCardRow {
    VCardRow {
        id: hex::encode(&d.card_id),
        formatted_name: d.parsed.formatted_name.clone(),
        org: d.parsed.org_line(),
        title: d.parsed.title.clone(),
        note: d.parsed.note.clone(),
        emails: d.parsed.emails.iter().map(|v| v.value.clone()).collect(),
        tels: d.parsed.tels.iter().map(|v| v.value.clone()).collect(),
        addresses: d.parsed.addresses.iter().map(|a| a.one_line()).collect(),
    }
}

// ── Transport: the typed `fauna.bridges.*` CardDAV call surface ──

/// Typed `fauna.bridges.*` CardDAV call surface, generic over the WS-RPC
/// transport (`R: RpcRequester`): native call sites pass `Arc<NestClient>`, the
/// UniFFI wrapper the same, the wasm SPA its `WsRpcClient`. A Fauna app calls
/// these against its OWN actor (nest caller-scopes every non-MDA caller). Errors
/// propagate as the transport's `R::Error`.
pub struct CardDavClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> CardDavClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.bridges.list_addressbooks` — the actor's address books (sealed
    /// metadata + ctag / highestmodseq / card_count per entry). Pure read.
    pub async fn list_addressbooks(
        &self,
        req: ListAddressbooksRequest,
    ) -> Result<ListAddressbooksReply, R::Error> {
        self.nest
            .request("fauna.bridges.list_addressbooks", req)
            .await
    }

    /// `fauna.bridges.query_cards` — paginated REPORT (addressbook-query /
    /// multiget) over one collection. Returns sealed card bodies; the caller
    /// unseals via [`unseal_card_body`] (or uses [`Self::query_cards_decoded`]).
    /// Pure read.
    pub async fn query_cards(&self, req: QueryCardsRequest) -> Result<QueryCardsReply, R::Error> {
        self.nest.request("fauna.bridges.query_cards", req).await
    }

    /// `fauna.bridges.sync_addressbook_since` — RFC 6578 sync-collection: changed
    /// cards + tombstones since a `sync_token`. Pure read.
    ///
    /// A caller that applies the reply must read [`SyncAddressbookSinceReply::Unknown`]
    /// as "re-read the whole book" and apply none of its deletions (no app calls
    /// this kind yet; the unknown arm is pinned by a decode test).
    pub async fn sync_addressbook_since(
        &self,
        req: SyncAddressbookSinceRequest,
    ) -> Result<SyncAddressbookSinceReply, R::Error> {
        self.nest
            .request("fauna.bridges.sync_addressbook_since", req)
            .await
    }

    /// Headline read op for the Address Book sidebar: `list_addressbooks` (RPC) →
    /// unseal each entry's metadata → typed [`DecodedAddressbook`]s.
    /// Takes a pre-derived [`DavRecipientKeys`] rather than a bare `msek` — a
    /// caller gathering across several address books
    /// ([`Self::locate_card_by_uid_hash`]) derives ONE and passes it to every
    /// page, so N books cost one keygen, not N .
    pub async fn list_addressbooks_decoded(
        &self,
        req: ListAddressbooksRequest,
        keys: &DavRecipientKeys,
    ) -> Result<Vec<DecodedAddressbook>, ReadAddressbooksError<R::Error>> {
        let reply = self
            .list_addressbooks(req)
            .await
            .map_err(ReadAddressbooksError::Transport)?;
        reply
            .addressbooks
            .iter()
            .map(|a| decode_addressbook_entry(a, keys))
            .collect::<Result<Vec<_>, _>>()
            .map_err(ReadAddressbooksError::Decode)
    }

    /// Headline read op for the Address Book card list: `query_cards` (RPC) →
    /// unseal + parse each returned body → typed [`DecodedCard`]s. Takes a
    /// pre-derived [`DavRecipientKeys`] rather than a bare `msek`, same reason
    /// as [`Self::list_addressbooks_decoded`] .
    pub async fn query_cards_decoded(
        &self,
        req: QueryCardsRequest,
        keys: &DavRecipientKeys,
    ) -> Result<DecodedCardsPage, ReadCardsError<R::Error>> {
        match self
            .query_cards(req)
            .await
            .map_err(ReadCardsError::Transport)?
        {
            QueryCardsReply::Ok {
                cards,
                highestmodseq,
                more,
            } => {
                let decoded = cards
                    .iter()
                    .map(|c| decode_card_entry(c, keys))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(ReadCardsError::Decode)?;
                Ok(DecodedCardsPage::Ok {
                    cards: decoded,
                    highestmodseq,
                    more,
                })
            }
            QueryCardsReply::AddressbookNotFound => Ok(DecodedCardsPage::AddressbookNotFound),
            QueryCardsReply::Unknown => Err(ReadCardsError::UnknownOutcome),
        }
    }

    /// Find the card a **`uid_hash`** names, across every address book the actor
    /// holds — the deep-link read behind `SearchNav::Contact`
    /// (`ui/search.md` § State & data shape; `behavior/content-index.md`
    /// § Where queries run — the contacts ruling's *doc identity* sub-bullet).
    ///
    /// **Why this exists at all.** A search row carries the card's `uid_hash`,
    /// because that is the identity which survives an in-place vCard edit and is
    /// stable across devices; every Address Book UI keys its open card on the
    /// server-assigned **`card_id`**, and nothing on the wire maps one to the
    /// other. The two are the same width and both hex, so feeding one where the
    /// other is expected opens nothing and raises no error — a silent failure,
    /// which is why the join lives here once rather than in seven apps
    /// (priority #2), and why it is a *lookup* rather than a spelling match.
    ///
    /// **Why it reads rather than scanning loaded state.** The book holding the
    /// card need not be the book the user last opened — it need not be loaded at
    /// all, since a deep link can arrive on a page the user has never visited.
    /// Resolving against whatever happens to be in memory would render nothing
    /// for exactly the case the feature exists for (the class the feed's
    /// `find_post` deep-link slot closes for posts: *the loaded page is not the
    /// addressable universe*).
    ///
    /// One `list_addressbooks` plus one `query_cards` per book until the card is
    /// found — the same reads [`Self::list_addressbooks_decoded`] and
    /// [`Self::query_cards_decoded`] already take for a book the user picks, and
    /// bounded by the number of books rather than the number of cards. Books are
    /// visited in wire order and the walk **stops at the first hit**: a
    /// `uid_hash` is the CardDAV dedup key, so a second copy in a later book
    /// would be the same person's card either way.
    ///
    /// A book that vanishes between the listing and its query is skipped, not an
    /// error (another device deleted it mid-walk — the same posture the card-list
    /// read takes).
    pub async fn locate_card_by_uid_hash(
        &self,
        actor_id: Vec<u8>,
        msek: &[u8; 32],
        prior_mseks: &[[u8; 32]],
        uid_hash: &[u8],
    ) -> Result<LocatedCard, LocateCardError<R::Error>> {
        // Derived once for the whole walk — the book listing AND every book's
        // card page below reuse it instead of each paying its own keygen
        //  — and as the whole ring, so a card written before a
        // mail-key rotation is still found.
        let keys = DavRecipientKeys::from_mseks(msek, prior_mseks);
        let books = self
            .list_addressbooks_decoded(
                ListAddressbooksRequest {
                    actor_id: actor_id.clone(),
                },
                &keys,
            )
            .await
            .map_err(LocateCardError::Addressbooks)?;

        let mut card = None;
        for book in &books {
            let page = self
                .query_cards_decoded(
                    QueryCardsRequest {
                        actor_id: actor_id.clone(),
                        addressbook_id: book.addressbook_id.clone(),
                        since_modseq: None,
                        after_card_id: None,
                        // The wire's "unbounded" — asking for one page would
                        // silently truncate a real book and drop the very card
                        // the caller named (`Self::query_cards_decoded`'s own
                        // callers use the same 0).
                        limit: 0,
                    },
                    &keys,
                )
                .await
                .map_err(LocateCardError::Cards)?;
            let DecodedCardsPage::Ok { cards, .. } = page else {
                continue;
            };
            if let Some(hit) = cards.iter().find(|c| c.uid_hash == uid_hash) {
                card = Some(FoundCard {
                    addressbook_id: book.addressbook_id.clone(),
                    card_id: hit.card_id.clone(),
                    cards,
                });
                break;
            }
        }
        Ok(LocatedCard { books, card })
    }
}

/// What [`CardDavClient::locate_card_by_uid_hash`] found.
///
/// `books` rides along because the locate had to list them anyway: a deep link
/// lands on an Address Book the user may never have opened, and returning the
/// picker's rows here is what lets that page come up whole — book list, card
/// list and open card — off **one** round of reads instead of three.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocatedCard {
    /// Every address book the actor holds, in wire order.
    pub books: Vec<DecodedAddressbook>,
    /// The card and where it lives, or `None` when no book holds it — deleted
    /// since it was indexed, which is the same **DROPPED** outcome a query-time
    /// resolve gives (`ui/search.md` § State & data shape → *An unresolvable
    /// local hit is DROPPED*), arriving one click later.
    pub card: Option<FoundCard>,
}

/// The located card's home: which book, which `card_id`, and that book's
/// decoded cards (so the destination page renders the list it opens into).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundCard {
    /// The holding book's id — what an Address Book UI selects.
    pub addressbook_id: Vec<u8>,
    /// The card's server-assigned id — what an Address Book UI opens.
    pub card_id: Vec<u8>,
    /// The holding book's decoded cards, in wire order.
    pub cards: Vec<DecodedCard>,
}

/// Error from [`CardDavClient::locate_card_by_uid_hash`] — one of the two reads
/// it composes failed.
#[derive(Debug)]
pub enum LocateCardError<E> {
    /// Listing the actor's address books failed.
    Addressbooks(ReadAddressbooksError<E>),
    /// Reading one book's cards failed.
    Cards(ReadCardsError<E>),
}

impl<E: core::fmt::Display> core::fmt::Display for LocateCardError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            LocateCardError::Addressbooks(e) => write!(f, "{e}"),
            LocateCardError::Cards(e) => write!(f, "{e}"),
        }
    }
}

/// Outcome of [`CardDavClient::query_cards_decoded`]: a page of [`DecodedCard`]s
/// plus the book's `highestmodseq` reference and a `more` pagination flag, or the
/// `AddressbookNotFound` signal a fresh client gets before it provisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodedCardsPage {
    /// Cards decoded successfully.
    Ok {
        /// Decoded cards, in the wire order (`card_id ASC`).
        cards: Vec<DecodedCard>,
        /// Current address-book `highestmodseq` (a stable reference for the next
        /// incremental sync), echoed from `QueryCardsReply::Ok`.
        highestmodseq: i64,
        /// `true` iff more pages remain (resume with `after_card_id`).
        more: bool,
    },
    /// No address-book row exists for `(actor_id, addressbook_id)`.
    AddressbookNotFound,
}

/// Error from [`CardDavClient::query_cards_decoded`] — the transport failed, or a
/// returned card could not be unsealed/decoded.
#[derive(Debug)]
pub enum ReadCardsError<E> {
    /// The `query_cards` RPC failed.
    Transport(E),
    /// One of the returned cards could not be decoded (unseal / non-UTF-8).
    Decode(DecodeCardError),
    /// The nest answered with an outcome this build does not know
    /// ([`QueryCardsReply::Unknown`]). An error, never `AddressbookNotFound`:
    /// that would read as an empty book and forget the sync token.
    UnknownOutcome,
}

impl<E: core::fmt::Display> core::fmt::Display for ReadCardsError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ReadCardsError::Transport(e) => write!(f, "query_cards transport: {e}"),
            ReadCardsError::Decode(e) => write!(f, "{e}"),
            ReadCardsError::UnknownOutcome => f.write_str(
                "query_cards: the nest answered with an outcome this version does not know",
            ),
        }
    }
}

/// Error from [`CardDavClient::list_addressbooks_decoded`] — the transport
/// failed, or an address book's metadata could not be unsealed.
#[derive(Debug)]
pub enum ReadAddressbooksError<E> {
    /// The `list_addressbooks` RPC failed.
    Transport(E),
    /// One of the returned books' metadata could not be unsealed.
    Decode(SealError),
}

impl<E: core::fmt::Display> core::fmt::Display for ReadAddressbooksError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ReadAddressbooksError::Transport(e) => write!(f, "list_addressbooks transport: {e}"),
            ReadAddressbooksError::Decode(e) => write!(f, "{e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::block_on;
    use fauna_protocol::bridge_routing::AddressbookEntry;

    const MSEK: [u8; 32] = [7u8; 32];

    fn id(b: u8) -> Vec<u8> {
        vec![b; 32]
    }

    fn enc<T: serde::Serialize>(v: &T) -> Vec<u8> {
        fauna_protocol::encode_canonical(v)
            .expect("encode")
            .to_vec()
    }

    /// A requester that asserts the composed `kind` and returns a fixed,
    /// pre-encoded reply — enough for the transport + decoded read paths (which
    /// call exactly one kind).
    struct FixedRequester {
        expect_kind: &'static str,
        reply: Vec<u8>,
    }

    impl RpcRequester for FixedRequester {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            assert_eq!(kind, self.expect_kind, "unexpected kind");
            Ok(fauna_protocol::decode_strict(&self.reply).expect("decode reply"))
        }
    }

    const SAMPLE_VCARD: &str = "BEGIN:VCARD\r\n\
VERSION:4.0\r\n\
UID:urn:uuid:1234-5678\r\n\
FN:Jane Q. Doe\r\n\
N:Doe;Jane;Q.;Dr.;Jr.\r\n\
EMAIL;TYPE=work:jane@work.example\r\n\
EMAIL;TYPE=home,pref:jane@home.example\r\n\
TEL;TYPE=cell;PREF=1:+1-555-0100\r\n\
ADR;TYPE=home:;;12 Oak St;Springfield;IL;62704;USA\r\n\
ORG:Globex;Engineering\r\n\
TITLE:Principal Engineer\r\n\
NOTE:Met at the conference.\r\n\
BDAY:19850514\r\n\
X-FAUNA-ACTOR-ID:actor-abc123\r\n\
END:VCARD\r\n";

    // ── vCard parser ──────────────────────────────────────────────────────────

    #[test]
    fn parse_vcard_extracts_all_rendered_fields() {
        let c = vcard::parse_vcard(SAMPLE_VCARD);
        assert_eq!(c.uid, "urn:uuid:1234-5678");
        assert_eq!(c.formatted_name, "Jane Q. Doe");
        let n = c.name.expect("N present");
        assert_eq!(n.family, "Doe");
        assert_eq!(n.given, "Jane");
        assert_eq!(n.additional, "Q.");
        assert_eq!(n.prefixes, "Dr.");
        assert_eq!(n.suffixes, "Jr.");

        assert_eq!(c.emails.len(), 2);
        assert_eq!(c.emails[0].value, "jane@work.example");
        assert_eq!(c.emails[0].types, vec!["work"]);
        assert!(!c.emails[0].pref);
        assert_eq!(c.emails[1].value, "jane@home.example");
        assert_eq!(c.emails[1].types, vec!["home"]); // "pref" filtered out of types
        assert!(c.emails[1].pref); // TYPE=pref idiom

        assert_eq!(c.tels.len(), 1);
        assert_eq!(c.tels[0].value, "+1-555-0100");
        assert_eq!(c.tels[0].types, vec!["cell"]);
        assert!(c.tels[0].pref); // PREF=1 param

        assert_eq!(c.addresses.len(), 1);
        let a = &c.addresses[0];
        assert_eq!(a.types, vec!["home"]);
        assert_eq!(a.po_box, "");
        assert_eq!(a.extended, "");
        assert_eq!(a.street, "12 Oak St");
        assert_eq!(a.locality, "Springfield");
        assert_eq!(a.region, "IL");
        assert_eq!(a.postal_code, "62704");
        assert_eq!(a.country, "USA");

        assert_eq!(c.org, vec!["Globex", "Engineering"]);
        assert_eq!(c.title, "Principal Engineer");
        assert_eq!(c.note, "Met at the conference.");
        assert_eq!(c.bday, "19850514");
        assert_eq!(c.fauna_actor_id.as_deref(), Some("actor-abc123"));
    }

    #[test]
    fn org_line_joins_non_empty_components() {
        let c = vcard::parse_vcard(SAMPLE_VCARD);
        assert_eq!(c.org_line(), "Globex · Engineering");
    }

    #[test]
    fn org_line_skips_empty_components_and_is_empty_when_unset() {
        // An `ORG:Globex;;Research` line parses to a middle empty component; the
        // join must not leave a dangling separator (what a naive `join` gives).
        let c = vcard::parse_vcard(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:X\r\nORG:Globex;;Research\r\nEND:VCARD\r\n",
        );
        assert_eq!(c.org_line(), "Globex · Research");

        let none = vcard::parse_vcard("BEGIN:VCARD\r\nVERSION:4.0\r\nFN:X\r\nEND:VCARD\r\n");
        assert_eq!(none.org_line(), "");
    }

    #[test]
    fn address_one_line_joins_non_empty_and_merges_region_zip() {
        let c = vcard::parse_vcard(SAMPLE_VCARD);
        assert_eq!(
            c.addresses[0].one_line(),
            "12 Oak St, Springfield, IL 62704, USA"
        );
        let sparse = vcard::Address {
            locality: "Berlin".into(),
            country: "Germany".into(),
            ..Default::default()
        };
        assert_eq!(sparse.one_line(), "Berlin, Germany");
    }

    #[test]
    fn parse_vcard_unfolds_continuation_lines() {
        // RFC 6350 §3.2: a continuation line begins with one whitespace byte,
        // which is stripped. Two leading spaces here ⇒ one survives as the join.
        let text = "BEGIN:VCARD\nNOTE:hello\n  world\nEND:VCARD\n";
        let c = vcard::parse_vcard(text);
        assert_eq!(c.note, "hello world");
    }

    #[test]
    fn parse_vcard_unescapes_text() {
        // Source `\\` = one literal backslash in the vCard body, so the body reads
        // `NOTE:a\,b\;c\nd` → unescape → "a,b;c\nd" (last is a real newline).
        let text = "BEGIN:VCARD\nNOTE:a\\,b\\;c\\nd\nEND:VCARD\n";
        let c = vcard::parse_vcard(text);
        assert_eq!(c.note, "a,b;c\nd");
    }

    #[test]
    fn parse_vcard_ignores_unknown_and_malformed_lines() {
        let text = "BEGIN:VCARD\nFN:Solo\nX-WEIRD;TYPE=x:whatever\nnot a content line\nEND:VCARD\n";
        let c = vcard::parse_vcard(text);
        assert_eq!(c.formatted_name, "Solo");
        assert!(c.name.is_none());
        assert!(c.emails.is_empty());
    }

    #[test]
    fn parse_vcard_handles_quoted_param_with_colon() {
        // A DQUOTE-quoted param value may contain `:` — the value-colon scan must
        // skip it and split at the real value colon.
        let text = "BEGIN:VCARD\nTEL;TYPE=\"a:b\":+1-555-0199\nEND:VCARD\n";
        let c = vcard::parse_vcard(text);
        assert_eq!(c.tels.len(), 1);
        assert_eq!(c.tels[0].value, "+1-555-0199");
        assert_eq!(c.tels[0].types, vec!["a:b"]);
    }

    // ── seal / unseal round-trips ─────────────────────────────────────────────

    #[test]
    fn seal_unseal_card_body_round_trips_classical() {
        let body = SAMPLE_VCARD.as_bytes();
        let sealed = seal_card_body(body, &MSEK).expect("seal");
        assert_eq!(unseal_card_body(&sealed, &MSEK).expect("unseal"), body);
    }

    #[test]
    fn seal_unseal_card_body_round_trips_xwing() {
        let body = SAMPLE_VCARD.as_bytes();
        let sealed = seal_card_body_xwing(body, &MSEK).expect("xwing seal");
        // unseal opens either suite from the same MSEK
        assert_eq!(unseal_card_body(&sealed, &MSEK).expect("unseal"), body);
    }

    #[test]
    fn unseal_card_body_wrong_msek_fails() {
        let sealed = seal_card_body(b"secret", &MSEK).expect("seal");
        let wrong = [9u8; 32];
        assert!(matches!(
            unseal_card_body(&sealed, &wrong),
            Err(SealError::Unseal(_))
        ));
    }

    #[test]
    fn unseal_card_body_bad_envelope_fails() {
        assert!(matches!(
            unseal_card_body(b"not-a-valid-envelope", &MSEK),
            Err(SealError::Unseal(_))
        ));
    }

    #[test]
    fn addressbook_metadata_round_trips() {
        let meta = AddressbookMetadata {
            displayname: "Colleagues".into(),
            description: "Work contacts".into(),
        };
        let sealed = seal_addressbook_metadata(&meta, &MSEK).expect("seal");
        assert_eq!(
            unseal_addressbook_metadata(&sealed, &DavRecipientKeys::derive(&MSEK)).expect("unseal"),
            meta
        );
    }

    #[test]
    fn addressbook_metadata_empty_blob_is_default() {
        assert_eq!(
            unseal_addressbook_metadata(&[], &DavRecipientKeys::derive(&MSEK)).expect("empty"),
            AddressbookMetadata::default()
        );
    }

    // ── decode helpers ────────────────────────────────────────────────────────

    fn sealed_card(vcard: &str) -> CardEntry {
        CardEntry {
            card_id: id(0xC1),
            uid_hash: uid_hash("urn:uuid:1234-5678").to_vec(),
            encrypted_body: seal_card_body(vcard.as_bytes(), &MSEK).expect("seal body"),
            etag: "etag-1".into(),
            modseq: 5,
            internal_date: 1_700_000_000,
            ..Default::default()
        }
    }

    #[test]
    fn decode_card_entry_unseals_and_parses() {
        let entry = sealed_card(SAMPLE_VCARD);
        let card = decode_card_entry(&entry, &DavRecipientKeys::derive(&MSEK)).expect("decode");
        assert_eq!(card.etag, "etag-1");
        assert_eq!(card.modseq, 5);
        assert_eq!(card.vcard, SAMPLE_VCARD);
        assert_eq!(card.parsed.formatted_name, "Jane Q. Doe");
        assert_eq!(card.parsed.emails.len(), 2);
        assert!(!card.has_fauna_ext);
    }

    #[test]
    fn decode_card_entry_wrong_msek_is_unseal_error() {
        let entry = sealed_card(SAMPLE_VCARD);
        let wrong = [3u8; 32];
        assert!(matches!(
            decode_card_entry(&entry, &DavRecipientKeys::derive(&wrong)),
            Err(DecodeCardError::Unseal(_))
        ));
    }

    #[test]
    fn decode_card_entry_flags_sidecar_presence() {
        let mut entry = sealed_card(SAMPLE_VCARD);
        entry.encrypted_fauna_ext = Some(vec![0u8; 4]);
        let card = decode_card_entry(&entry, &DavRecipientKeys::derive(&MSEK)).expect("decode");
        assert!(card.has_fauna_ext);
    }

    #[test]
    fn decode_addressbook_entry_unseals_metadata() {
        let meta = AddressbookMetadata {
            displayname: "Personal".into(),
            description: String::new(),
        };
        let entry = AddressbookEntry {
            addressbook_id: id(0xAB),
            encrypted_metadata: seal_addressbook_metadata(&meta, &MSEK).expect("seal"),
            ctag: 3,
            highestmodseq: 7,
            card_count: 12,
            created_at: 1_699_000_000,
        };
        let decoded =
            decode_addressbook_entry(&entry, &DavRecipientKeys::derive(&MSEK)).expect("decode");
        assert_eq!(decoded.metadata.displayname, "Personal");
        assert_eq!(decoded.card_count, 12);
        assert_eq!(decoded.ctag, 3);
    }

    // ── Display-row projection ───────────────────────────────────────────────

    fn decoded_book(name: &str, count: u32) -> DecodedAddressbook {
        DecodedAddressbook {
            addressbook_id: id(0xAB),
            metadata: AddressbookMetadata {
                displayname: name.to_string(),
                description: String::new(),
            },
            ctag: 1,
            highestmodseq: 1,
            card_count: count,
            created_at: 0,
        }
    }

    #[test]
    fn addressbook_row_maps_id_name_and_count() {
        let row = addressbook_row(&decoded_book("Contacts", 3));
        assert_eq!(row.id, hex::encode(id(0xAB)));
        assert_eq!(row.name, "Contacts");
        assert_eq!(row.card_count, 3);
    }

    #[test]
    fn addressbook_row_empty_name_passes_through_for_the_view_fallback() {
        // The projection stays i18n-free; each app substitutes its own
        // localized "Address Book" fallback title when the DAV name is empty.
        assert_eq!(addressbook_row(&decoded_book("", 0)).name, "");
    }

    #[test]
    fn vcard_row_projects_every_display_field_through_the_shared_formatters() {
        let card = decode_card_entry(&sealed_card(SAMPLE_VCARD), &DavRecipientKeys::derive(&MSEK))
            .expect("decode");
        let row = vcard_row(&card);
        assert_eq!(row.id, hex::encode(id(0xC1)));
        assert_eq!(row.formatted_name, "Jane Q. Doe");
        assert_eq!(row.org, "Globex · Engineering"); // via org_line, not a local join
        assert_eq!(row.title, "Principal Engineer");
        assert_eq!(row.note, "Met at the conference.");
        assert_eq!(row.emails, vec!["jane@work.example", "jane@home.example"]);
        assert_eq!(row.tels, vec!["+1-555-0100"]);
        assert_eq!(row.addresses.len(), 1);
        assert!(row.addresses[0].contains("12 Oak St")); // via one_line, not a local join
    }

    #[test]
    fn vcard_row_bare_card_leaves_every_optional_empty() {
        let entry = sealed_card("BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Bare\r\nEND:VCARD\r\n");
        let card = decode_card_entry(&entry, &DavRecipientKeys::derive(&MSEK)).expect("decode");
        let row = vcard_row(&card);
        assert_eq!(row.formatted_name, "Bare");
        assert!(row.org.is_empty() && row.title.is_empty() && row.note.is_empty());
        assert!(row.emails.is_empty() && row.tels.is_empty() && row.addresses.is_empty());
    }

    // ── transport wrappers ────────────────────────────────────────────────────

    #[test]
    fn list_addressbooks_composes_kind() {
        let reply = ListAddressbooksReply {
            addressbooks: vec![],
        };
        let client = CardDavClient::new(FixedRequester {
            expect_kind: "fauna.bridges.list_addressbooks",
            reply: enc(&reply),
        });
        let got =
            block_on(client.list_addressbooks(ListAddressbooksRequest { actor_id: id(0xAA) }))
                .expect("ok");
        assert!(got.addressbooks.is_empty());
    }

    #[test]
    fn query_cards_composes_kind() {
        let reply = QueryCardsReply::Ok {
            cards: vec![],
            highestmodseq: 0,
            more: false,
        };
        let client = CardDavClient::new(FixedRequester {
            expect_kind: "fauna.bridges.query_cards",
            reply: enc(&reply),
        });
        let got = block_on(client.query_cards(QueryCardsRequest {
            actor_id: id(0xAA),
            addressbook_id: id(0xBB),
            since_modseq: None,
            after_card_id: None,
            limit: 0,
        }))
        .expect("ok");
        assert!(matches!(got, QueryCardsReply::Ok { .. }));
    }

    #[test]
    fn sync_addressbook_since_composes_kind() {
        let reply = SyncAddressbookSinceReply::Ok {
            changed: vec![],
            expunged: vec![],
            new_sync_token: "0".into(),
            more: false,
            stale: false,
        };
        let client = CardDavClient::new(FixedRequester {
            expect_kind: "fauna.bridges.sync_addressbook_since",
            reply: enc(&reply),
        });
        let got = block_on(client.sync_addressbook_since(SyncAddressbookSinceRequest {
            actor_id: id(0xAA),
            addressbook_id: id(0xBB),
            sync_token: "0".into(),
            limit: 0,
            mua_id: None,
        }))
        .expect("ok");
        assert!(matches!(got, SyncAddressbookSinceReply::Ok { .. }));
    }

    // ── decoded read paths ────────────────────────────────────────────────────

    #[test]
    fn query_cards_decoded_returns_decoded_page() {
        let reply = QueryCardsReply::Ok {
            cards: vec![sealed_card(SAMPLE_VCARD)],
            highestmodseq: 9,
            more: false,
        };
        let client = CardDavClient::new(FixedRequester {
            expect_kind: "fauna.bridges.query_cards",
            reply: enc(&reply),
        });
        let page = block_on(client.query_cards_decoded(
            QueryCardsRequest {
                actor_id: id(0xAA),
                addressbook_id: id(0xBB),
                since_modseq: None,
                after_card_id: None,
                limit: 0,
            },
            &DavRecipientKeys::derive(&MSEK),
        ))
        .expect("decoded page");
        match page {
            DecodedCardsPage::Ok {
                cards,
                highestmodseq,
                more,
            } => {
                assert_eq!(cards.len(), 1);
                assert_eq!(highestmodseq, 9);
                assert!(!more);
                assert_eq!(cards[0].parsed.formatted_name, "Jane Q. Doe");
            }
            other => panic!("expected Ok page, got {other:?}"),
        }
    }

    #[test]
    fn query_cards_decoded_addressbook_not_found_passthrough() {
        let client = CardDavClient::new(FixedRequester {
            expect_kind: "fauna.bridges.query_cards",
            reply: enc(&QueryCardsReply::AddressbookNotFound),
        });
        let page = block_on(client.query_cards_decoded(
            QueryCardsRequest {
                actor_id: id(0xAA),
                addressbook_id: id(0xBB),
                since_modseq: None,
                after_card_id: None,
                limit: 0,
            },
            &DavRecipientKeys::derive(&MSEK),
        ))
        .expect("ok result");
        assert_eq!(page, DecodedCardsPage::AddressbookNotFound);
    }

    /// What a newer nest sends for an outcome this build cannot name: a twin
    /// tagged enum with one extra outcome (the real enums' `Unknown` arm refuses
    /// to serialize, so it cannot stand in).
    fn newer_outcome() -> Vec<u8> {
        #[derive(serde::Serialize)]
        #[serde(tag = "outcome", rename_all = "snake_case")]
        enum NewerOutcome {
            FromTheFuture,
        }
        enc(&NewerOutcome::FromTheFuture)
    }

    /// A read that meets an outcome it cannot name is an error — never
    /// `AddressbookNotFound`, which would empty the book and forget the token.
    #[test]
    fn query_cards_decoded_newer_outcome_is_an_error_never_not_found() {
        let client = CardDavClient::new(FixedRequester {
            expect_kind: "fauna.bridges.query_cards",
            reply: newer_outcome(),
        });
        let err = block_on(client.query_cards_decoded(
            QueryCardsRequest {
                actor_id: id(0xAA),
                addressbook_id: id(0xBB),
                since_modseq: None,
                after_card_id: None,
                limit: 0,
            },
            &DavRecipientKeys::derive(&MSEK),
        ))
        .expect_err("an unknown outcome must not read as a page");
        assert!(matches!(err, ReadCardsError::UnknownOutcome), "{err:?}");
    }

    /// The sync reply hands the unknown outcome back as `Unknown`, which a
    /// caller that applies it reads as "re-read the whole book".
    #[test]
    fn sync_addressbook_since_newer_outcome_reaches_the_caller_as_unknown() {
        let client = CardDavClient::new(FixedRequester {
            expect_kind: "fauna.bridges.sync_addressbook_since",
            reply: newer_outcome(),
        });
        let got = block_on(client.sync_addressbook_since(SyncAddressbookSinceRequest {
            actor_id: id(0xAA),
            addressbook_id: id(0xBB),
            sync_token: "41".into(),
            limit: 0,
            mua_id: None,
        }))
        .expect("ok");
        assert_eq!(got, SyncAddressbookSinceReply::Unknown);
    }

    #[test]
    fn list_addressbooks_decoded_unseals_each() {
        let meta = AddressbookMetadata {
            displayname: "Family".into(),
            description: String::new(),
        };
        let reply = ListAddressbooksReply {
            addressbooks: vec![AddressbookEntry {
                addressbook_id: id(0xAB),
                encrypted_metadata: seal_addressbook_metadata(&meta, &MSEK).expect("seal"),
                ctag: 1,
                highestmodseq: 1,
                card_count: 2,
                created_at: 100,
            }],
        };
        let client = CardDavClient::new(FixedRequester {
            expect_kind: "fauna.bridges.list_addressbooks",
            reply: enc(&reply),
        });
        let books = block_on(client.list_addressbooks_decoded(
            ListAddressbooksRequest { actor_id: id(0xAA) },
            &DavRecipientKeys::derive(&MSEK),
        ))
        .expect("decoded");
        assert_eq!(books.len(), 1);
        assert_eq!(books[0].metadata.displayname, "Family");
        assert_eq!(books[0].card_count, 2);
    }

    #[test]
    fn query_cards_decoded_wrong_msek_is_decode_error() {
        // Sealed with `MSEK`; unsealed with a different key inside the
        // wrapper's decode-and-collect loop — the `ReadCardsError::Decode`
        // arm, never exercised via `query_cards_decoded` itself before.
        let reply = QueryCardsReply::Ok {
            cards: vec![sealed_card(SAMPLE_VCARD)],
            highestmodseq: 9,
            more: false,
        };
        let client = CardDavClient::new(FixedRequester {
            expect_kind: "fauna.bridges.query_cards",
            reply: enc(&reply),
        });
        let wrong = [3u8; 32];
        let err = block_on(client.query_cards_decoded(
            QueryCardsRequest {
                actor_id: id(0xAA),
                addressbook_id: id(0xBB),
                since_modseq: None,
                after_card_id: None,
                limit: 0,
            },
            &DavRecipientKeys::derive(&wrong),
        ))
        .expect_err("wrong msek must fail to decode");
        assert!(matches!(
            err,
            ReadCardsError::Decode(DecodeCardError::Unseal(_))
        ));
    }

    #[test]
    fn list_addressbooks_decoded_wrong_msek_is_decode_error() {
        let meta = AddressbookMetadata {
            displayname: "Family".into(),
            description: String::new(),
        };
        let reply = ListAddressbooksReply {
            addressbooks: vec![AddressbookEntry {
                addressbook_id: id(0xAB),
                encrypted_metadata: seal_addressbook_metadata(&meta, &MSEK).expect("seal"),
                ctag: 1,
                highestmodseq: 1,
                card_count: 2,
                created_at: 100,
            }],
        };
        let client = CardDavClient::new(FixedRequester {
            expect_kind: "fauna.bridges.list_addressbooks",
            reply: enc(&reply),
        });
        let wrong = [3u8; 32];
        let err = block_on(client.list_addressbooks_decoded(
            ListAddressbooksRequest { actor_id: id(0xAA) },
            &DavRecipientKeys::derive(&wrong),
        ))
        .expect_err("wrong msek must fail to decode");
        assert!(matches!(err, ReadAddressbooksError::Decode(_)));
    }

    // ── locate by uid_hash (the SearchNav::Contact deep link) ─────────────────

    /// Replies handed out in call order, recording the kinds actually asked for
    /// — the locate walk's *shape* (how many books it queries, and that it stops)
    /// is half of what these tests assert, and a fixed single-reply mock cannot
    /// see it.
    struct ScriptedRequester {
        replies: std::cell::RefCell<std::collections::VecDeque<(&'static str, Vec<u8>)>>,
        seen: std::cell::RefCell<Vec<&'static str>>,
    }

    impl ScriptedRequester {
        fn new(replies: Vec<(&'static str, Vec<u8>)>) -> Self {
            Self {
                replies: std::cell::RefCell::new(replies.into()),
                seen: std::cell::RefCell::new(Vec::new()),
            }
        }
        fn calls(&self) -> usize {
            self.seen.borrow().len()
        }
    }

    impl RpcRequester for ScriptedRequester {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let (expect, bytes) = self
                .replies
                .borrow_mut()
                .pop_front()
                .expect("an unscripted request was made");
            assert_eq!(kind, expect, "requests must come in the scripted order");
            self.seen.borrow_mut().push(kind);
            Ok(fauna_protocol::decode_strict(&bytes).expect("decode reply"))
        }
    }

    fn book_entry(book_id: u8, name: &str) -> AddressbookEntry {
        let meta = AddressbookMetadata {
            displayname: name.into(),
            description: String::new(),
        };
        AddressbookEntry {
            addressbook_id: id(book_id),
            encrypted_metadata: seal_addressbook_metadata(&meta, &MSEK).expect("seal"),
            ctag: 1,
            highestmodseq: 1,
            card_count: 1,
            created_at: 100,
        }
    }

    fn sealed_card_at(card_id: u8, uid: &str) -> CardEntry {
        CardEntry {
            card_id: id(card_id),
            uid_hash: uid_hash(uid).to_vec(),
            encrypted_body: seal_card_body(SAMPLE_VCARD.as_bytes(), &MSEK).expect("seal body"),
            etag: "etag-1".into(),
            modseq: 5,
            internal_date: 1_700_000_000,
            ..Default::default()
        }
    }

    fn cards_reply(cards: Vec<CardEntry>) -> Vec<u8> {
        enc(&QueryCardsReply::Ok {
            cards,
            highestmodseq: 9,
            more: false,
        })
    }

    fn books_reply(books: Vec<AddressbookEntry>) -> Vec<u8> {
        enc(&ListAddressbooksReply {
            addressbooks: books,
        })
    }

    /// The whole point of the join: the `uid_hash` a search row carries resolves
    /// to the **`card_id`** an Address Book opens, in whichever book holds it —
    /// including a book the caller never selected.
    #[test]
    fn locate_card_by_uid_hash_finds_the_card_in_a_book_the_caller_never_opened() {
        let client = CardDavClient::new(ScriptedRequester::new(vec![
            (
                "fauna.bridges.list_addressbooks",
                books_reply(vec![book_entry(0xB1, "Work"), book_entry(0xB2, "Family")]),
            ),
            (
                "fauna.bridges.query_cards",
                cards_reply(vec![sealed_card_at(0xC1, "urn:uuid:someone-else")]),
            ),
            (
                "fauna.bridges.query_cards",
                cards_reply(vec![sealed_card_at(0xC2, "urn:uuid:1234-5678")]),
            ),
        ]));
        let located = block_on(client.locate_card_by_uid_hash(
            id(0xAA),
            &MSEK,
            &[],
            &uid_hash("urn:uuid:1234-5678"),
        ))
        .expect("locate");

        assert_eq!(located.books.len(), 2, "the picker's rows ride along");
        let found = located.card.expect("the card is in the second book");
        assert_eq!(found.addressbook_id, id(0xB2));
        assert_eq!(
            found.card_id,
            id(0xC2),
            "the CARD_ID is the join's output — never the uid_hash the row carried"
        );
        assert_eq!(
            found.cards.len(),
            1,
            "the holding book's cards come back with it, so the page renders off one read round"
        );
    }

    /// Books are visited in wire order and the walk stops at the first hit — a
    /// `uid_hash` is the CardDAV dedup key, so there is nothing to gain from
    /// paying for the books behind it.
    #[test]
    fn locate_card_by_uid_hash_stops_at_the_first_hit() {
        let client = CardDavClient::new(ScriptedRequester::new(vec![
            (
                "fauna.bridges.list_addressbooks",
                books_reply(vec![book_entry(0xB1, "Work"), book_entry(0xB2, "Family")]),
            ),
            (
                "fauna.bridges.query_cards",
                cards_reply(vec![sealed_card_at(0xC1, "urn:uuid:1234-5678")]),
            ),
            // A third scripted reply that must never be asked for; the mock
            // asserts nothing beyond the script, so the call count is the pin.
            (
                "fauna.bridges.query_cards",
                cards_reply(vec![sealed_card_at(0xC2, "urn:uuid:1234-5678")]),
            ),
        ]));
        let located = block_on(client.locate_card_by_uid_hash(
            id(0xAA),
            &MSEK,
            &[],
            &uid_hash("urn:uuid:1234-5678"),
        ))
        .expect("locate");
        assert_eq!(
            located.card.expect("found").addressbook_id,
            id(0xB1),
            "the first book wins"
        );
        assert_eq!(
            client.nest.calls(),
            2,
            "one listing + one query — the second book is never read"
        );
    }

    /// No book holds it: the card was deleted between being indexed and being
    /// clicked. `None`, not an error — the DROPPED rule arriving one click late.
    #[test]
    fn locate_card_by_uid_hash_reports_no_card_rather_than_an_error() {
        let client = CardDavClient::new(ScriptedRequester::new(vec![
            (
                "fauna.bridges.list_addressbooks",
                books_reply(vec![book_entry(0xB1, "Work")]),
            ),
            (
                "fauna.bridges.query_cards",
                cards_reply(vec![sealed_card_at(0xC1, "urn:uuid:someone-else")]),
            ),
        ]));
        let located = block_on(client.locate_card_by_uid_hash(
            id(0xAA),
            &MSEK,
            &[],
            &uid_hash("urn:uuid:gone"),
        ))
        .expect("a missing card is not a failure");
        assert!(located.card.is_none());
        assert_eq!(
            located.books.len(),
            1,
            "the books still come back, so the page it lands on is not empty"
        );
    }

    /// A book deleted by another device between the listing and its query is
    /// skipped, and the walk carries on to the book that does hold the card.
    #[test]
    fn locate_card_by_uid_hash_skips_a_book_that_vanished_mid_walk() {
        let client = CardDavClient::new(ScriptedRequester::new(vec![
            (
                "fauna.bridges.list_addressbooks",
                books_reply(vec![book_entry(0xB1, "Gone"), book_entry(0xB2, "Family")]),
            ),
            (
                "fauna.bridges.query_cards",
                enc(&QueryCardsReply::AddressbookNotFound),
            ),
            (
                "fauna.bridges.query_cards",
                cards_reply(vec![sealed_card_at(0xC2, "urn:uuid:1234-5678")]),
            ),
        ]));
        let located = block_on(client.locate_card_by_uid_hash(
            id(0xAA),
            &MSEK,
            &[],
            &uid_hash("urn:uuid:1234-5678"),
        ))
        .expect("locate");
        assert_eq!(located.card.expect("found").addressbook_id, id(0xB2));
    }

    /// An unopenable card body fails the locate loudly rather than reporting the
    /// card missing — "we cannot read this book" and "this card is gone" are
    /// different answers, and only the second may silently close the deep link.
    #[test]
    fn locate_card_by_uid_hash_surfaces_a_decode_failure() {
        let client = CardDavClient::new(ScriptedRequester::new(vec![
            (
                "fauna.bridges.list_addressbooks",
                books_reply(vec![book_entry(0xB1, "Work")]),
            ),
            (
                "fauna.bridges.query_cards",
                cards_reply(vec![sealed_card_at(0xC1, "urn:uuid:1234-5678")]),
            ),
        ]));
        let wrong = [3u8; 32];
        let err = block_on(client.locate_card_by_uid_hash(
            id(0xAA),
            &wrong,
            &[],
            &uid_hash("urn:uuid:1234-5678"),
        ))
        .expect_err("a book we cannot open is an error");
        // The books themselves unseal under the msek too, so the wrong key trips
        // the listing first — either arm is a loud failure, which is the point.
        assert!(matches!(err, LocateCardError::Addressbooks(_)));
    }

    // ── error Display ─────────────────────────────────────────────────────────

    /// The prefix names the *operation*, not the resource: the seal is one
    /// scheme shared with CalDAV event bodies
    /// (`fauna_mls::wrapped_blob::dav_body`), so one spelling serves both. It
    /// read "seal card body" until 2026-08-23, when the two byte-identical
    /// copies of this error and its three functions became one — the domain
    /// noun still reaches a reader through the calling fn's name and through
    /// [`DecodeCardError`]'s own arms below.
    #[test]
    fn seal_error_display_prefixes() {
        assert_eq!(SealError::Seal("x".into()).to_string(), "seal DAV body: x");
        assert_eq!(
            SealError::Unseal("y".into()).to_string(),
            "unseal DAV body: y"
        );
    }

    #[test]
    fn decode_card_error_display_delegates_or_prefixes() {
        assert_eq!(
            DecodeCardError::Unseal(SealError::Unseal("bad key".into())).to_string(),
            "unseal DAV body: bad key",
            "Unseal delegates to the inner SealError's own Display"
        );
        assert_eq!(
            DecodeCardError::Parse("not utf8".into()).to_string(),
            "parse card body: not utf8"
        );
    }

    #[test]
    fn read_cards_error_display_prefixes_transport_and_delegates_decode() {
        let transport: ReadCardsError<String> = ReadCardsError::Transport("disconnected".into());
        assert_eq!(transport.to_string(), "query_cards transport: disconnected");
        let decode: ReadCardsError<String> =
            ReadCardsError::Decode(DecodeCardError::Parse("not utf8".into()));
        assert_eq!(decode.to_string(), "parse card body: not utf8");
    }

    #[test]
    fn read_addressbooks_error_display_prefixes_transport_and_delegates_decode() {
        let transport: ReadAddressbooksError<String> =
            ReadAddressbooksError::Transport("disconnected".into());
        assert_eq!(
            transport.to_string(),
            "list_addressbooks transport: disconnected"
        );
        let decode: ReadAddressbooksError<String> =
            ReadAddressbooksError::Decode(SealError::Unseal("bad key".into()));
        assert_eq!(decode.to_string(), "unseal DAV body: bad key");
    }
}
