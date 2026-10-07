//! The Contacts page's **Address Book** segment — the CardDAV read surface
//! (`ui/contacts.md` § Layout & flow; the store + wire authority is
//! `behavior/carddav-server.md` § Storage model).
//!
//! A **separate store** from the social contact graph the rest of
//! [`crate::contacts`] renders: full vCards for arbitrary people (the plumber,
//! a school — overwhelmingly *not* Fauna actors), MLS-sealed by the MDA and
//! decrypted here with the actor's own MSEK, so a nest never reads them.
//!
//! **Read-only (slice 4b)** — there is no vCard write UI on any app yet.
//!
//! **Where the logic lives** (priority #2): everything non-trivial is the
//! shared `fauna-client-carddav` crate — the `fauna.bridges.*` transport, the
//! `unseal_card_body` MDA-seal contract, the RFC-6350 `vcard` reader, the two
//! display formatters (`Address::one_line`, `ParsedVCard::org_line`), and the
//! `AddressbookRow`/`VCardRow` display-row projection itself (lifted
//! 2026-08-11 — it was tui's and linux's `views/contacts/carddav_backend.rs`'s
//! last hand-duplicated twin). This module is the thin remainder: transport
//! glue + tui's own paint, and tui is the **2nd direct-Rust consumer** of the
//! crate after linux (no FFI hop) — exactly how [`crate::events`] consumes
//! `fauna-client-caldav` for the CalDAV twin.

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client::NestClient;
pub use fauna_client_carddav::{AddressbookRow, VCardRow, addressbook_row, vcard_row};
use fauna_client_carddav::{
    CardDavClient, DavRecipientKeys, DecodedCardsPage,
    bridge_routing::{ListAddressbooksRequest, QueryCardsRequest},
};
use fauna_client_config::{DavStoreContext, dav_store_context};
use fauna_core::identity::ActorKeypair;
use fauna_i18n::strings::contacts as t;

use crate::contacts::{Action, ContactsState};
use crate::element::{Element, Gesture};

/// Which half of the Contacts page is showing — the `contacts-view-segment`
/// toggle (`contacts.md` § Layout & flow; nav placement user-approved
/// 2026-07-07: a segment *within* Contacts, not a new top-level page).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Segment {
    /// The social contact graph — roster, knocks, Find User.
    #[default]
    People,
    /// The CardDAV address books + vCards.
    AddressBook,
}

/// The picker row's label: the book's name, or the localized "Address Book"
/// when the DAV `displayname` is empty, plus its card count.
fn book_label(b: &AddressbookRow) -> String {
    let name = if b.name.is_empty() {
        t::address_book::TITLE
    } else {
        b.name.as_str()
    };
    format!("{name} ({})", b.card_count)
}

// ── Transport ───────────────────────────────────────────────────────────────

/// The `(actor_id, msek)` pair every CardDAV read needs. Thin wrapper over the
/// shared [`fauna_client_config::dav_store_context`] (also `events::caldav_context`'s
/// twin here, linux's `client::caldav_context`, and the FFI face's
/// `dav_store_context`) — this seam supplies only what tui's address-book page
/// knows: the actor id from the connection secret, and the session's mail
/// custody. `None` when mail was never enabled for this actor (no MSEK to
/// unseal with).
async fn carddav_context(
    mail: &dyn fauna_client_config::MailStore,
    secret: [u8; 32],
) -> Option<DavStoreContext> {
    dav_store_context(mail, ActorKeypair::from_secret(secret).actor_id().0).await
}

/// `fauna.bridges.list_addressbooks` → unseal each book's metadata → picker rows.
///
/// An actor with no MSEK has no CardDAV store at all, so the read degrades to an
/// **empty list rather than an error** — the same degrade the Events page's
/// CalDAV read performs for the identical reason (priority #3).
pub async fn load_addressbooks(
    nest: Arc<NestClient>,
    secret: [u8; 32],
    mail: Arc<dyn fauna_client_config::MailStore>,
) -> Result<Vec<AddressbookRow>, String> {
    let Some(DavStoreContext {
        actor_id,
        msek,
        prior_mseks,
    }) = carddav_context(mail.as_ref(), secret).await
    else {
        return Ok(Vec::new());
    };
    let keys = DavRecipientKeys::from_mseks(&msek, &prior_mseks);
    let books = CardDavClient::new(nest)
        .list_addressbooks_decoded(
            ListAddressbooksRequest {
                actor_id: actor_id.to_vec(),
            },
            &keys,
        )
        .await
        .map_err(|e| format!("list_addressbooks: {e}"))?;
    Ok(books.iter().map(addressbook_row).collect())
}

/// `fauna.bridges.query_cards` → unseal + parse each body → card rows.
///
/// `limit: 0` is the wire's "unbounded": the v1 read surface has no pagination
/// affordance, and asking for one page would silently truncate a real book.
pub async fn load_cards(
    nest: Arc<NestClient>,
    secret: [u8; 32],
    mail: Arc<dyn fauna_client_config::MailStore>,
    book_id: String,
) -> Result<Vec<VCardRow>, String> {
    // Validated (and refused, on a wrong-length id) BEFORE the network round
    // trip below — `hex::decode` alone accepts any length, only
    // `fauna_core::hex32::decode` enforces the 32-byte contract the wire
    // struct's own doc comment states (`bridge_routing::QueryCardsRequest::
    // addressbook_id`), so a malformed id must never reach
    // `fauna.bridges.query_cards`.
    let addressbook_id =
        fauna_core::hex32::decode(&book_id).map_err(|e| format!("address book id: {e}"))?;
    let Some(DavStoreContext {
        actor_id,
        msek,
        prior_mseks,
    }) = carddav_context(mail.as_ref(), secret).await
    else {
        return Ok(Vec::new());
    };
    let keys = DavRecipientKeys::from_mseks(&msek, &prior_mseks);
    let page = CardDavClient::new(nest)
        .query_cards_decoded(
            QueryCardsRequest {
                actor_id: actor_id.to_vec(),
                addressbook_id: addressbook_id.to_vec(),
                since_modseq: None,
                after_card_id: None,
                limit: 0,
            },
            &keys,
        )
        .await
        .map_err(|e| format!("query_cards: {e}"))?;
    match page {
        DecodedCardsPage::Ok { cards, .. } => Ok(cards.iter().map(vcard_row).collect()),
        // The book disappeared between the picker read and this query (another
        // device deleted it). An empty card list is the honest answer; the
        // picker's own refresh is the user's way back.
        DecodedCardsPage::AddressbookNotFound => Ok(Vec::new()),
    }
}

/// What a `SearchNav::Contact` deep link resolved to — the whole Address Book
/// state a jump-to-card needs, from one round of reads.
///
/// `None` in [`Self::open`] means no book holds that `uid_hash` any more; the
/// books still came back, so the page lands on a real picker rather than a
/// blank.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LocatedCard {
    /// Every book, for the picker the deep link lands behind.
    pub books: Vec<AddressbookRow>,
    /// The holding book's hex id, that book's card rows, and the target card's
    /// hex `card_id` — in the order [`crate::contacts::ContactsState`] needs
    /// them (`selected_book`, `cards`, `open_card`).
    pub open: Option<(String, Vec<VCardRow>, String)>,
}

/// Resolve a card's **`uid_hash`** (what a search row carries) to the
/// **`card_id`** the Address Book opens, plus the state around it.
///
/// The id-space join is `fauna_client_carddav::CardDavClient::
/// locate_card_by_uid_hash` — shared, because feeding a `uid_hash` to a
/// `card_id` consumer opens nothing and raises no error, and seven apps must not
/// each re-derive the lookup that avoids it (that function's docs own the
/// reasoning).
///
/// An actor with no MSEK degrades to "nothing found" rather than an error, the
/// same way [`load_addressbooks`] degrades to an empty list.
pub async fn locate_card(
    nest: Arc<NestClient>,
    secret: [u8; 32],
    mail: Arc<dyn fauna_client_config::MailStore>,
    uid_hash_hex: &str,
) -> Result<LocatedCard, String> {
    // Same client-side refusal as `load_cards`: a wrong-length (but validly
    // hex) `uid_hash` must never reach `locate_card_by_uid_hash`.
    let uid_hash =
        fauna_core::hex32::decode(uid_hash_hex).map_err(|e| format!("contact id: {e}"))?;
    let Some(DavStoreContext {
        actor_id,
        msek,
        prior_mseks,
    }) = carddav_context(mail.as_ref(), secret).await
    else {
        return Ok(LocatedCard::default());
    };
    let located = CardDavClient::new(nest)
        .locate_card_by_uid_hash(actor_id.to_vec(), &msek, &prior_mseks, &uid_hash)
        .await
        .map_err(|e| e.to_string())?;
    Ok(LocatedCard {
        books: located.books.iter().map(addressbook_row).collect(),
        open: located.card.map(|c| {
            (
                fauna_core::format::hex_full(&c.addressbook_id),
                c.cards.iter().map(vcard_row).collect(),
                fauna_core::format::hex_full(&c.card_id),
            )
        }),
    })
}

// ── Paint ───────────────────────────────────────────────────────────────────

/// The Address Book half of the Contacts page, appended in registry order.
///
/// The `card_detail` sub-page is a **full-page replacement** of the card list,
/// the shape `events::Mode::EventDetail` and feed's `post_detail` already use on
/// this client — and the reason a terminal needs no "hide" primitive: an
/// unpainted element is absent from the frame registry, so `is_visible` answers
/// false and `wait_for` blocks, which is exactly what a GTK `set_visible(false)`
/// buys the other shells.
pub fn render(st: &ContactsState, out: &mut Vec<Element>) {
    // A stale `open_card` (its book reloaded out from under it) degrades to the
    // list rather than painting an empty pane the user cannot leave.
    let open = st
        .open_card
        .as_ref()
        .and_then(|id| st.cards.iter().find(|c| &c.id == id));

    if let Some(card) = open {
        out.push(Element::label(
            ids::VCARD_DETAIL_FN,
            card.formatted_name.clone(),
        ));
        if !card.org.is_empty() {
            out.push(Element::label(ids::VCARD_DETAIL_ORG, card.org.clone()));
        }
        for e in &card.emails {
            out.push(Element::label(ids::VCARD_DETAIL_EMAIL, e.clone()));
        }
        for tel in &card.tels {
            out.push(Element::label(ids::VCARD_DETAIL_TEL, tel.clone()));
        }
        for a in &card.addresses {
            out.push(Element::label(ids::VCARD_DETAIL_ADR, a.clone()));
        }
        if !card.note.is_empty() {
            out.push(Element::label(ids::VCARD_DETAIL_NOTE, card.note.clone()));
        }
        return;
    }

    for b in &st.addressbooks {
        out.push(Element::gesture_button(
            ids::ADDRESSBOOK_ITEM,
            book_label(b),
            true,
            Gesture::Contacts(Action::SelectAddressbook(b.id.clone())),
        ));
    }

    for c in &st.cards {
        // `vcard-card` and `vcard-card-fn` are painted **1:1 and in the same
        // order**, and that is load-bearing rather than tidy: the shared action
        // reads the `vcard-card-fn` list to find an index, then clicks
        // `vcard-card` AT THAT SAME INDEX
        // (`tests/e2e-unified/actions/contacts.py::open_card_by_name`). A row
        // painting one without the other slides the two index spaces apart and
        // opens a different card than the user picked.
        out.push(Element::gesture_button(
            ids::VCARD_CARD,
            c.formatted_name.clone(),
            true,
            Gesture::Contacts(Action::OpenCard(c.id.clone())),
        ));
        out.push(Element::label(ids::VCARD_CARD_FN, c.formatted_name.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `AddressbookRow`/`VCardRow` and their `addressbook_row`/`vcard_row`
    // projections are shared-crate types now (`fauna_client_carddav`), covered
    // by that crate's own tests. What stays here is tui-specific: `book_label`'s
    // i18n fallback and `render`'s paint over literal row fixtures.

    fn state_with(cards: Vec<VCardRow>, open: Option<&str>) -> ContactsState {
        ContactsState {
            segment: Segment::AddressBook,
            cards,
            open_card: open.map(str::to_string),
            ..ContactsState::default()
        }
    }

    fn ids(out: &[Element]) -> Vec<String> {
        out.iter().map(|e| e.id.clone()).collect()
    }

    #[test]
    fn book_label_falls_back_to_the_localized_title_when_name_is_empty() {
        let empty = AddressbookRow {
            id: "b1".into(),
            name: String::new(),
            card_count: 0,
        };
        assert_eq!(
            book_label(&empty),
            format!("{} (0)", t::address_book::TITLE)
        );

        let named = AddressbookRow {
            id: "b2".into(),
            name: "Work".into(),
            card_count: 7,
        };
        assert_eq!(book_label(&named), "Work (7)");
    }

    /// The index-alignment contract `open_card_by_name` depends on: one
    /// `vcard-card` and one `vcard-card-fn` per card, interleaved so the two
    /// index spaces cannot drift.
    #[test]
    fn card_list_paints_card_and_fn_one_to_one() {
        let st = state_with(
            vec![
                VCardRow {
                    id: "aa".into(),
                    formatted_name: "Ada".into(),
                    ..Default::default()
                },
                VCardRow {
                    id: "bb".into(),
                    formatted_name: "Bob".into(),
                    ..Default::default()
                },
            ],
            None,
        );
        let mut out = Vec::new();
        render(&st, &mut out);
        let painted = ids(&out);
        assert_eq!(
            painted,
            vec!["vcard-card", "vcard-card-fn", "vcard-card", "vcard-card-fn"]
        );
        assert_eq!(painted.iter().filter(|i| *i == "vcard-card").count(), 2);
        assert_eq!(painted.iter().filter(|i| *i == "vcard-card-fn").count(), 2);
    }

    #[test]
    fn detail_replaces_the_card_list_and_paints_only_present_fields() {
        let row = VCardRow {
            id: "card1".into(),
            formatted_name: "Ada Lovelace".into(),
            org: "Analytical Engine · Research".into(),
            note: "first programmer".into(),
            emails: vec!["ada@example.com".into(), "ada@work.example".into()],
            tels: vec!["+15550142".into()],
            addresses: vec!["12 Baker St, London".into()],
            ..Default::default()
        };
        let id = row.id.clone();
        let st = state_with(vec![row], Some(&id));
        let mut out = Vec::new();
        render(&st, &mut out);
        let painted = ids(&out);
        // The list is GONE — a terminal expresses "hidden" by not painting.
        assert!(!painted.contains(&"vcard-card".to_string()));
        assert!(!painted.contains(&"addressbook-item".to_string()));
        assert_eq!(
            painted,
            vec![
                "vcard-detail-fn",
                "vcard-detail-org",
                "vcard-detail-email",
                "vcard-detail-email",
                "vcard-detail-tel",
                "vcard-detail-adr",
                "vcard-detail-note",
            ]
        );
    }

    #[test]
    fn detail_omits_empty_org_and_note() {
        let row = VCardRow {
            id: "bare".into(),
            formatted_name: "Bare".into(),
            ..Default::default()
        };
        let id = row.id.clone();
        let mut out = Vec::new();
        render(&state_with(vec![row], Some(&id)), &mut out);
        assert_eq!(ids(&out), vec!["vcard-detail-fn"]);
    }

    /// A card id that no longer resolves must fall back to the list, never paint
    /// an empty pane the user has no way out of.
    #[test]
    fn stale_open_card_degrades_to_the_list() {
        let st = state_with(
            vec![VCardRow {
                id: "aa".into(),
                formatted_name: "Ada".into(),
                ..Default::default()
            }],
            Some("deadbeef"),
        );
        let mut out = Vec::new();
        render(&st, &mut out);
        assert_eq!(ids(&out), vec!["vcard-card", "vcard-card-fn"]);
    }

    // Never actually dialed — both tests below refuse before either
    // `load_cards`/`locate_card` awaits anything that would touch it.
    fn dummy_nest() -> Arc<NestClient> {
        NestClient::new(
            "http://127.0.0.1:9".to_string(),
            ActorKeypair::from_secret([7u8; 32]),
        )
    }

    /// Mirrors `family.rs`'s `contact_add_and_transfer_refuse_a_wrong_length_
    /// hex_actor_id_client_side`: `hex::decode` alone accepts any length, so a
    /// 31-byte (62-hex-char) value that is still valid hex must be caught here,
    /// not silently forwarded to `fauna.bridges.query_cards`.
    #[tokio::test]
    async fn load_cards_refuses_a_wrong_length_hex_book_id_before_touching_the_nest() {
        let too_short = "07".repeat(31);
        let err = load_cards(
            dummy_nest(),
            [7u8; 32],
            std::sync::Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
            too_short,
        )
        .await
        .unwrap_err();
        assert!(err.contains("address book id"), "{err}");
    }

    #[tokio::test]
    async fn locate_card_refuses_a_wrong_length_hex_uid_hash_before_touching_the_nest() {
        let too_short = "07".repeat(31);
        let err = locate_card(
            dummy_nest(),
            [7u8; 32],
            std::sync::Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
            &too_short,
        )
        .await
        .unwrap_err();
        assert!(err.contains("contact id"), "{err}");
    }
}
