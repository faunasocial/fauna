//! UniFFI façade for the **encrypted-CardDAV Address Book path** — the Contacts
//! page's "Address Book" segment reads the actor's OWN address books + vCards over
//! the encrypted `bridge_carddav_*` store via the `fauna.bridges.*` CardDAV RPCs +
//! client-side unseal, the SAME store + sealing scheme the mail-bridge MDA serves
//! to Apple Contacts / DAVx5, so a Fauna app and a CardDAV MUA see the same data
//! (`docs/goal/behavior/carddav-server.md` § Independent enablement — the native
//! Address Book view: `list_addressbooks` / `query_cards`, decrypted locally /
//! zero-access; `docs/goal/ui/contacts.md` § Layout & flow).
//!
//! The read/consumption analogue of [`FfiCaldavClient`](crate::FfiCaldavClient):
//! [`FfiCarddavClient`] wraps [`fauna_client_carddav::CardDavClient`] (which the
//! Rust-native Linux app calls directly) so Apple / Windows / Android reach the
//! identical surface over UniFFI. It mirrors the caldav seam 1:1 (priority #2 — one
//! kind-composition + unseal shared by native/UniFFI + web/wasm), but is **read-only**:
//! slice 4b is display-only (`FN` / `EMAIL` / `TEL` / `ADR` / `ORG` / `NOTE`); the
//! vCard *write* path (create/edit a card) is slice 4c and lands later. Construct via
//! [`crate::nest_client::FfiNestClient::carddav`].
//!
//! ## msek sourcing (the encrypted-store gate)
//!
//! The card bodies + collection metadata are unsealed with `cfg.mail.msek`
//! (the MSEK in the `fauna.state.mail` row, minted only by `enable_mail`). This seam sources it
//! **internally** via the shared [`dav_store_context`](crate::caldav_client::dav_store_context)
//! (CalDAV + CardDAV read the SAME msek). `None` (localhost / IP nest with no mail
//! enabled) **degrades gracefully** — reads return an empty list — so the
//! Kotlin/Swift layer never touches key material and an un-provisioned nest never
//! crashes the page. Unlike CalDAV there is **no lazy provisioning**: an address
//! book is minted by the write path (slice 4c) or a CardDAV MUA, never by this
//! read seam, so an actor with no book yet renders an empty Address Book.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_carddav::{
    CardDavClient, DavRecipientKeys, DecodedAddressbook, DecodedCard, DecodedCardsPage,
    bridge_routing::{ListAddressbooksRequest, QueryCardsRequest},
    vcard,
};

use crate::FfiError;
use crate::caldav_client::dav_store_context;
use fauna_client_config::DavStoreContext;

// ── reply mirrors (only the Address-Book-rendered fields) ──────────────────────

/// FFI mirror of an address-book collection row (`DecodedAddressbook`): the hex
/// `addressbook_id` + the unsealed display metadata + the card count the sidebar
/// shows. Mirrors [`FfiCalendarRow`](crate::FfiCalendarRow); an address book carries
/// **no color** (the vCard collection metadata omits it — carddav-server.md
/// § Standard properties).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAddressbookRow {
    /// Hex of the 32-byte `addressbook_id` (the read key for `query_cards`).
    pub id: String,
    /// Unsealed `displayname` (DAV `displayname`).
    pub name: String,
    /// Unsealed CardDAV `addressbook-description`; empty when none was stored.
    pub description: String,
    /// Number of cards in the book (from the `list_addressbooks` row, no decrypt).
    pub card_count: u32,
}

/// A vCard property value paired with its `TYPE` params + preference flag — e.g. a
/// `TEL;TYPE=work,voice;PREF=1:+1-555-0100`. UniFFI mirror of
/// [`vcard::TypedValue`](fauna_client_carddav::vcard::TypedValue).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiVCardValue {
    /// The unescaped property value (the phone number, email, URL, …).
    pub value: String,
    /// Lower-cased `TYPE` values (e.g. `work`, `home`, `cell`), `pref` removed.
    pub types: Vec<String>,
    /// `true` when the property is marked preferred (`PREF=1` / `TYPE=pref`).
    pub pref: bool,
}

/// A structured postal address (mirror
/// [`vcard::Address`](fauna_client_carddav::vcard::Address)) — the components the
/// detail pane renders, plus a shared one-line [`Self::formatted`] rendering so
/// every app shows the same address text (priority #2).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiPostalAddress {
    pub types: Vec<String>,
    pub pref: bool,
    pub po_box: String,
    pub extended: String,
    pub street: String,
    pub locality: String,
    pub region: String,
    pub postal_code: String,
    pub country: String,
    /// The non-empty components joined into a single human line (street, locality,
    /// region, postal code, country) — the shared render the `vcard-detail-adr`
    /// element shows so the format is uniform across clients.
    pub formatted: String,
}

/// FFI mirror of a decoded vCard ([`DecodedCard`]): the hex `card_id` + the parsed
/// fields the Address Book list + detail pane render. Mirrors
/// [`FfiCalEvent`](crate::FfiCalEvent). Read-only in slice 4b (no write ops).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiCardRow {
    /// Hex of the 32-byte `card_id` (the row identity; the write key in slice 4c).
    pub id: String,
    /// The plaintext vCard `UID` (stays inside the sealed body).
    pub uid: String,
    /// `FN` — the display name (the card-list label + detail header).
    pub formatted_name: String,
    pub emails: Vec<FfiVCardValue>,
    pub tels: Vec<FfiVCardValue>,
    pub addresses: Vec<FfiPostalAddress>,
    pub urls: Vec<FfiVCardValue>,
    /// `ORG` components (`organization;unit;subunit`).
    pub org: Vec<String>,
    /// `TITLE`.
    pub title: String,
    /// `NOTE`.
    pub note: String,
    /// `BDAY`, verbatim.
    pub bday: String,
    /// `true` when the row carried a Fauna-only `X-FAUNA-ACTOR-ID` sidecar (the
    /// cross-link to a social contact — surfaced as presence only; the decode +
    /// cross-link is a post-v1 slice).
    pub has_fauna_ext: bool,
}

/// A located card's home: which book, which `card_id`, and that book's
/// decoded cards (so the destination page renders the list it opens into).
/// FFI mirror of [`fauna_client_carddav::FoundCard`].
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiFoundCard {
    /// Hex of the holding book's `addressbook_id`.
    pub addressbook_id: String,
    /// Hex of the card's server-assigned `card_id`.
    pub card_id: String,
    pub cards: Vec<FfiCardRow>,
}

/// What [`FfiCarddavClient::locate_card_by_uid_hash`] found. FFI mirror of
/// [`fauna_client_carddav::LocatedCard`].
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiLocatedCard {
    /// Every address book the actor holds, in wire order.
    pub books: Vec<FfiAddressbookRow>,
    /// The card and where it lives, or `None` when no book holds it.
    pub found: Option<FfiFoundCard>,
}

// ── FfiCarddavClient ─────────────────────────────────────────────────────────────

/// UniFFI handle for the encrypted-CardDAV Address Book surface. Construct via
/// [`crate::nest_client::FfiNestClient::carddav`]; methods are exposed to Swift as
/// `async throws` and Kotlin as `suspend fun`.
#[derive(uniffi::Object)]
pub struct FfiCarddavClient {
    nest: Arc<NestClient>,
}

impl FfiCarddavClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    fn client(&self) -> CardDavClient<Arc<NestClient>> {
        CardDavClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiCarddavClient {
    /// List the actor's address books (`fauna.bridges.list_addressbooks` + unseal
    /// each row's metadata). Empty when mail/CardDAV is off, or when the actor has
    /// no book yet (there is **no** lazy provisioning — the read seam never writes;
    /// a book is minted by the write slice or a CardDAV MUA).
    pub async fn list_addressbooks(&self) -> Result<Vec<FfiAddressbookRow>, FfiError> {
        let Some(DavStoreContext {
            actor_id,
            msek,
            prior_mseks,
        }) = dav_store_context(&self.nest).await
        else {
            return Ok(vec![]);
        };
        let books = self
            .client()
            .list_addressbooks_decoded(
                ListAddressbooksRequest {
                    actor_id: actor_id.to_vec(),
                },
                &DavRecipientKeys::from_mseks(&msek, &prior_mseks),
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(books.iter().map(map_addressbook).collect())
    }

    /// Query + decode every vCard in one address book (hex `addressbook_id`). Empty
    /// when mail/CardDAV is off, or when the book has no row yet
    /// (`AddressbookNotFound` → empty, mirroring caldav's `CalendarNotFound`).
    pub async fn query_cards(
        &self,
        addressbook_id_hex: String,
    ) -> Result<Vec<FfiCardRow>, FfiError> {
        let Some(addressbook_id) = hex32(&addressbook_id_hex) else {
            return Err(FfiError::General {
                msg: "address book id must be 64 hex chars".to_string(),
            });
        };
        let Some(DavStoreContext {
            actor_id,
            msek,
            prior_mseks,
        }) = dav_store_context(&self.nest).await
        else {
            return Ok(vec![]);
        };
        let page = self
            .client()
            .query_cards_decoded(
                QueryCardsRequest {
                    actor_id: actor_id.to_vec(),
                    addressbook_id: addressbook_id.to_vec(),
                    since_modseq: None,
                    after_card_id: None,
                    limit: 0,
                },
                &DavRecipientKeys::from_mseks(&msek, &prior_mseks),
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(match page {
            DecodedCardsPage::Ok { cards, .. } => cards.iter().map(map_card).collect(),
            DecodedCardsPage::AddressbookNotFound => vec![],
        })
    }

    /// Locate a card by its `uid_hash` (hex-encoded `blake3(uid)`) across every
    /// address book the actor holds — the contact-navigation deep-link door a
    /// search hit needs, since the holding book need not be the one currently
    /// open, or loaded at all (`ui/search.md` § Where logic lives → *Result
    /// navigation (deep link)*). ⚠ `uid_hash` and `card_id` are different id
    /// spaces of the same width — never cast one for the other; this method is
    /// the only sanctioned lookup.
    ///
    /// Returns every address book (so the destination page can render its
    /// picker + card list off one round of reads, the same shape
    /// `list_addressbooks`/`query_cards` already cost) plus the found card's
    /// location, or `found: None` when no book holds it (deleted since it was
    /// indexed — the same DROPPED outcome a query-time resolve gives).
    pub async fn locate_card_by_uid_hash(
        &self,
        uid_hash_hex: String,
    ) -> Result<FfiLocatedCard, FfiError> {
        let Some(uid_hash) = hex32(&uid_hash_hex) else {
            return Err(FfiError::General {
                msg: "uid_hash must be 64 hex chars".to_string(),
            });
        };
        let Some(DavStoreContext {
            actor_id,
            msek,
            prior_mseks,
        }) = dav_store_context(&self.nest).await
        else {
            return Ok(FfiLocatedCard {
                books: vec![],
                found: None,
            });
        };
        let located = self
            .client()
            .locate_card_by_uid_hash(actor_id.to_vec(), &msek, &prior_mseks, &uid_hash)
            .await
            .map_err(|e| e.to_string())?;
        Ok(FfiLocatedCard {
            books: located.books.iter().map(map_addressbook).collect(),
            found: located.card.map(|f| FfiFoundCard {
                addressbook_id: hex::encode(&f.addressbook_id),
                card_id: hex::encode(&f.card_id),
                cards: f.cards.iter().map(map_card).collect(),
            }),
        })
    }
}

// ── helpers ────────────────────────────────────────────────────────────────────

/// Map a decoded address book to the UI [`FfiAddressbookRow`].
fn map_addressbook(book: &DecodedAddressbook) -> FfiAddressbookRow {
    FfiAddressbookRow {
        id: hex::encode(&book.addressbook_id),
        name: book.metadata.displayname.clone(),
        description: book.metadata.description.clone(),
        card_count: book.card_count,
    }
}

/// Map a decoded vCard to the UI [`FfiCardRow`] — flatten the parsed fields into
/// the UniFFI Records the Address Book renders.
fn map_card(card: &DecodedCard) -> FfiCardRow {
    let p = &card.parsed;
    FfiCardRow {
        id: hex::encode(&card.card_id),
        uid: p.uid.clone(),
        formatted_name: p.formatted_name.clone(),
        emails: p.emails.iter().map(map_value).collect(),
        tels: p.tels.iter().map(map_value).collect(),
        addresses: p.addresses.iter().map(map_address).collect(),
        urls: p.urls.iter().map(map_value).collect(),
        org: p.org.clone(),
        title: p.title.clone(),
        note: p.note.clone(),
        bday: p.bday.clone(),
        has_fauna_ext: card.has_fauna_ext,
    }
}

fn map_value(v: &vcard::TypedValue) -> FfiVCardValue {
    FfiVCardValue {
        value: v.value.clone(),
        types: v.types.clone(),
        pref: v.pref,
    }
}

fn map_address(a: &vcard::Address) -> FfiPostalAddress {
    FfiPostalAddress {
        types: a.types.clone(),
        pref: a.pref,
        po_box: a.po_box.clone(),
        extended: a.extended.clone(),
        street: a.street.clone(),
        locality: a.locality.clone(),
        region: a.region.clone(),
        postal_code: a.postal_code.clone(),
        country: a.country.clone(),
        // Single-sourced with the web wasm face via the shared crate (priority #2).
        formatted: a.one_line(),
    }
}

/// Decode a 64-char hex string into a 32-byte array (`addressbook_id`); `None` on
/// malformed input. Thin `Option`-returning adapter over the shared
/// [`fauna_core::hex32::decode`].
fn hex32(s: &str) -> Option<[u8; 32]> {
    fauna_core::hex32::decode(s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex32_round_trips_and_rejects_bad_len() {
        let h = hex::encode([0xABu8; 32]);
        assert_eq!(hex32(&h), Some([0xABu8; 32]));
        assert_eq!(hex32("deadbeef"), None);
        assert_eq!(hex32("zz"), None);
    }

    #[test]
    fn map_card_flattens_parsed_fields() {
        let parsed = vcard::parse_vcard(
            "BEGIN:VCARD\r\nFN:Jane Doe\r\nUID:u-1\r\n\
EMAIL;TYPE=work:jane@work.example\r\nTEL;TYPE=cell;PREF=1:+1-555-0100\r\n\
ADR;TYPE=home:;;12 Oak St;Springfield;IL;62704;USA\r\nORG:Globex\r\nEND:VCARD\r\n",
        );
        let card = DecodedCard {
            card_id: vec![0xC1u8; 32],
            uid_hash: vec![0u8; 32],
            etag: "e".into(),
            modseq: 1,
            internal_date: 0,
            vcard: String::new(),
            parsed,
            has_fauna_ext: true,
        };
        let row = map_card(&card);
        assert_eq!(row.id, hex::encode([0xC1u8; 32]));
        assert_eq!(row.uid, "u-1");
        assert_eq!(row.formatted_name, "Jane Doe");
        assert_eq!(row.emails.len(), 1);
        assert_eq!(row.emails[0].value, "jane@work.example");
        assert_eq!(row.tels.len(), 1);
        assert!(row.tels[0].pref);
        assert_eq!(row.addresses.len(), 1);
        assert_eq!(
            row.addresses[0].formatted,
            "12 Oak St, Springfield, IL 62704, USA"
        );
        assert_eq!(row.org, vec!["Globex"]);
        assert!(row.has_fauna_ext);
    }
}
