//! The DAV **collection / resource identity** contract — the one Rust owner of
//! every value that must be derived identically by a Fauna app and by the Go
//! mail-bridge MDA, so the two land on the *same* `bridge_caldav_*` /
//! `bridge_carddav_*` row.
//!
//! # Why this module exists
//!
//! A CalDAV/CardDAV collection has two independent writers: a Fauna app (over
//! the encrypted `fauna.bridges.*` RPCs) and a stock DAV client (Apple
//! Calendar / Contacts, DAVx5, Thunderbird) talking to the Go MDA. Both address
//! rows by *derived* identifiers — never by a server-assigned key — so the
//! derivations have to agree **byte for byte** or the same logical calendar,
//! address book, or event silently becomes two rows.
//!
//! These values used to live as hand-copied constants on each side, under doc
//! comments asking a future human to keep them equal. They now have one Rust
//! owner (this module) and one mechanism holding the Go side to it:
//! `tests/dav_identity_cross_language.rs`, which reads the MDA's production Go
//! source and fails if either side drifts.
//!
//! # Placement
//!
//! `fauna-protocol` owns the wire fields these values fill
//! (`bridge_routing::{ProvisionCalendarRequest::calendar_id,
//! PutEventCiphertextRequest::uid_hash, …}`), and is reachable from both native
//! (`fauna-client-core`) and wasm (`fauna-rpc-wasm`) consumers, so all 7 apps
//! can use the same definition. `blake3` is `features = ["pure"]` in the
//! workspace, so this adds no build cost and excludes no target.
//!
//! Authority for the *values* is the goal docs, not this file:
//! `docs/goal/behavior/caldav-server.md` § Lazy "Personal" calendar and
//! `docs/goal/behavior/carddav-server.md` § Write surface.

/// The slug the lazy `Personal` calendar's id is derived from.
///
/// `caldav-server.md` § Lazy "Personal" calendar: a freshly-AUTH'd actor with
/// zero provisioned calendars gets `calendar_id = blake3("personal")[:32]`.
pub const PERSONAL_CALENDAR_SLUG: &str = "personal";

/// The slug the lazy `Contacts` address book's id is derived from.
///
/// `carddav-server.md` § Write surface: "the lazy "Contacts" book always reuses
/// `blake3("contacts")[:32]`" — which is why a collection DELETE clears its
/// tombstones (a re-provisioned book collides with the original id by design).
pub const CONTACTS_ADDRESSBOOK_SLUG: &str = "contacts";

/// The lazy `Personal` calendar's default display name.
pub const DEFAULT_CALENDAR_DISPLAYNAME: &str = "Personal";

/// The lazy `Personal` calendar's default colour.
pub const DEFAULT_CALENDAR_COLOR: &str = "#3273dc";

/// The lazy `Contacts` address book's default display name. Address books
/// carry no colour analog (`carddav-server.md` § Write surface).
pub const DEFAULT_ADDRESSBOOK_DISPLAYNAME: &str = "Contacts";

/// A calendar collection's metadata — the plaintext sealed into
/// `bridge_caldav_calendars.encrypted_metadata`.
///
/// **Cross-language interop contract.** The Rust side of the MDA's Go
/// `EncryptedCollectionMetadata`
/// (`bins/fauna-bridges/internal/mda/caldav/encrypted_metadata.go`): identical
/// CBOR field names (`displayname` / `color` / `description`, the last omitted
/// when empty), and **both sides MUST encode canonical dag-CBOR** (length-first
/// key sort) — the Fauna apps decode it strictly, so a struct-order encoding
/// renders as zero calendars. With both canonical, a calendar a Fauna app
/// provisions, one a calendar app's MKCOL/PROPFIND creates, and the Personal
/// calendar the nest creates for an emailed invitation all render the same
/// name and colour everywhere.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CalendarMetadata {
    /// Human-readable calendar name (DAV `displayname`).
    pub displayname: String,
    /// 7-char hex `#RRGGBB` rendered as CalDAV `calendar-color` (opaque hex in
    /// v1 — caldav-server.md § Out of scope: no RGB-A alpha).
    pub color: String,
    /// Optional CalDAV `calendar-description`; empty ⇒ omitted on the wire
    /// (matches the Go `cbor:"description,omitempty"`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Forward-compat catch-all (transport.md § Schema and forward-compat
    /// discipline rule 4): a field a newer writer adds survives this build's
    /// re-seal. Empty ⇒ omitted on the wire, so existing bytes are unchanged.
    #[serde(
        flatten,
        default,
        skip_serializing_if = "std::collections::BTreeMap::is_empty"
    )]
    pub extra: std::collections::BTreeMap<String, crate::Value>,
}

impl CalendarMetadata {
    /// The lazy `Personal` calendar's metadata (`caldav-server.md` § Lazy
    /// "Personal" calendar).
    #[must_use]
    pub fn personal() -> Self {
        Self {
            displayname: DEFAULT_CALENDAR_DISPLAYNAME.to_string(),
            color: DEFAULT_CALENDAR_COLOR.to_string(),
            description: String::new(),
            ..Default::default()
        }
    }
}

/// Derive a DAV collection id from a URL path segment or slug:
/// `blake3(segment)` (256-bit, so the goal doc's `[:32]` is the whole hash).
///
/// This is the single rule behind every collection id in the DAV surface — the
/// lazy `Personal` calendar and `Contacts` book pass their fixed slug, and the
/// MDA passes the opaque slug a stock client picked for a `MKCALENDAR`/`MKCOL`
/// (`caldav-server.md` § Collections: "any **other** non-empty segment … maps
/// deterministically to `calendar_id = blake3(segment)[:32]`. The mapping is
/// one source of truth, shared by every path parser").
///
/// ⚠ A **64-hex** segment is decoded directly rather than hashed — that branch
/// is the caller's (it is a URL-parsing concern, not an identity derivation),
/// and today lives only in the MDA's `resolveCalendarSegment` /
/// `resolveAddressbookSegment`.
#[must_use]
pub fn collection_id(segment: &str) -> [u8; 32] {
    *blake3::hash(segment.as_bytes()).as_bytes()
}

/// The deterministic id of the lazy `Personal` calendar.
#[must_use]
pub fn personal_calendar_id() -> [u8; 32] {
    collection_id(PERSONAL_CALENDAR_SLUG)
}

/// The deterministic id of the lazy `Contacts` address book.
#[must_use]
pub fn contacts_addressbook_id() -> [u8; 32] {
    collection_id(CONTACTS_ADDRESSBOOK_SLUG)
}

/// The `uid_hash` of a DAV resource's plaintext `UID`: `blake3(uid)` (256-bit).
///
/// One rule for both rails — an iCalendar `VEVENT`'s `UID` and a vCard's `UID`
/// hash identically, so a row a Fauna app writes and the row a DAV client PUTs
/// for the same `UID` collide on the same dedup key
/// (`bridge_caldav_events` / `bridge_carddav_cards`). The plaintext `UID` stays
/// inside the sealed body; only this hash is exposed as a plaintext index.
#[must_use]
pub fn uid_hash(uid: &str) -> [u8; 32] {
    *blake3::hash(uid.as_bytes()).as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lazy_collection_ids_are_blake3_of_their_slugs() {
        assert_eq!(
            personal_calendar_id(),
            *blake3::hash(b"personal").as_bytes()
        );
        assert_eq!(
            contacts_addressbook_id(),
            *blake3::hash(b"contacts").as_bytes()
        );
    }

    #[test]
    fn the_two_lazy_collections_do_not_collide() {
        assert_ne!(personal_calendar_id(), contacts_addressbook_id());
    }

    #[test]
    fn derivations_are_deterministic() {
        assert_eq!(personal_calendar_id(), personal_calendar_id());
        assert_eq!(contacts_addressbook_id(), contacts_addressbook_id());
        assert_eq!(uid_hash("evt-1@fauna.test"), uid_hash("evt-1@fauna.test"));
    }

    /// The lazy collections are just `collection_id` applied to a fixed slug —
    /// a stock client that happens to pick the slug `personal` for a
    /// `MKCALENDAR` must land on the very same row, not a second one.
    #[test]
    fn a_client_chosen_slug_equal_to_the_lazy_slug_resolves_to_the_lazy_id() {
        assert_eq!(collection_id("personal"), personal_calendar_id());
        assert_eq!(collection_id("contacts"), contacts_addressbook_id());
    }

    /// `uid_hash` and `collection_id` are the same primitive over different
    /// inputs; what matters is that neither is salted or domain-separated, because
    /// the Go side applies a bare `blake3.Sum256` to both.
    #[test]
    fn uid_hash_and_collection_id_are_the_same_unsalted_primitive() {
        assert_eq!(uid_hash("personal"), personal_calendar_id());
    }

    #[test]
    fn distinct_uids_hash_distinctly() {
        assert_ne!(uid_hash("evt-1@fauna.test"), uid_hash("evt-2@fauna.test"));
    }
}
