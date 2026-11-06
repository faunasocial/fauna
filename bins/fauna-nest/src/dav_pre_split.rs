//! The pre-split DAV tombstoning — schema 158's boot pass
//! (`docs/goal/architecture/owner-key-material.md` § Path B-sibling-5 →
//! *Migration*).
//!
//! Before the calendar and card key split, CalDAV and CardDAV records were
//! sealed as `mail-record` envelopes. The apps' calendar and card rings refuse
//! every other kind, so such a record is a row no app can open. Under the 0.1.x
//! compat-free window it is not re-sealed: this pass removes it, once per nest,
//! through the production write paths, so the placement journal, the expunge
//! tables and the WebDAV-Sync tokens stay coherent:
//!
//! - an **event or card** whose body (in the `__calendar` / `__card` segment
//!   store) or sidecar (`encrypted_fauna_ext`) is a `mail-record` is deleted by
//!   UID exactly as a DAV `DELETE` is, and its `DeleteEvent` / `DeleteCard`
//!   placement record journaled — a sync from a pre-boot token lists it gone;
//! - a **calendar or address book** whose metadata is a `mail-record` has that
//!   metadata blanked through the PROPPATCH path (an empty blob reads as the
//!   default metadata, `unseal_dav_body_typed_or_default`). The collection
//!   itself stays: a calendar app may have filed post-split events into it,
//!   and those are readable data the pass must not take with it.
//!
//! The envelope kind is plaintext, so no key is needed; bytes that do not
//! decode as an envelope are left untouched. The pass runs only while
//! `dav_pre_split_tombstone_pending` holds its row (set by
//! `db::migrations::mark_pre_split_dav_tombstoning` on a database stamped below
//! 158) and clears it on completion, so a fresh nest never scans and a crash
//! mid-pass re-runs it — every step is idempotent, as a removed record is gone.

use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use fauna_mls::wrapped_blob::{MailRecordEnvelope, RecordKind};

use crate::bridge_caldav_handlers::{journal_event_delete, update_calendar_metadata};
use crate::bridge_carddav_handlers::{journal_card_delete, update_addressbook_metadata};
use crate::db::bridge_caldav::DeleteCaldavEventOutcome;
use crate::db::bridge_carddav::DeleteCarddavCardOutcome;
use crate::db::now_epoch_secs;
use crate::routes::AppState;

/// What one run of the pass removed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tombstoned {
    pub events: usize,
    pub cards: usize,
    pub calendars_blanked: usize,
    pub addressbooks_blanked: usize,
}

/// Whether `bytes` are a sealed record of the pre-split `mail-record` kind.
fn is_pre_split(bytes: &[u8]) -> bool {
    MailRecordEnvelope::from_canonical_bytes(bytes)
        .ok()
        .and_then(|env| env.record_kind())
        == Some(RecordKind::Mail)
}

/// Run the pass if it is owed, before DAV serving. A failure is logged and
/// leaves the pending mark, so the next boot runs it again.
pub async fn run_at_boot(state: &Arc<AppState>) {
    match run(state).await {
        Ok(Some(done)) => tracing::info!(
            events = done.events,
            cards = done.cards,
            calendars_blanked = done.calendars_blanked,
            addressbooks_blanked = done.addressbooks_blanked,
            "pre-split DAV records tombstoned"
        ),
        Ok(None) => {}
        Err(e) => {
            tracing::error!("pre-split DAV tombstoning failed; the next boot retries it: {e:#}")
        }
    }
}

/// The pass itself: `None` when it is not owed (a fresh nest, or any boot
/// after the one that completed it).
pub(crate) async fn run(state: &Arc<AppState>) -> Result<Option<Tombstoned>> {
    if !state.db.dav_pre_split_tombstone_pending().await? {
        return Ok(None);
    }
    let mut done = Tombstoned::default();
    tombstone_calendars(state, &mut done).await?;
    tombstone_addressbooks(state, &mut done).await?;
    state.db.clear_dav_pre_split_tombstone_pending().await?;
    Ok(Some(done))
}

fn rpc(e: fauna_protocol::RpcError) -> anyhow::Error {
    anyhow!("{e:?}")
}

async fn tombstone_calendars(state: &Arc<AppState>, done: &mut Tombstoned) -> Result<()> {
    for actor in state.db.caldav_actor_ids().await? {
        for cal in state.db.list_bridge_caldav_calendars(&actor).await? {
            let page = state
                .db
                .query_caldav_events(&actor, &cal.calendar_id, None, None, 0)
                .await?;
            for row in page.events {
                let body = crate::segments::cal::load_event_body(
                    &state.cal_segments,
                    &state.db,
                    &actor,
                    &row,
                )
                .await?;
                let pre_split = body.as_deref().is_some_and(is_pre_split)
                    || row.encrypted_fauna_ext.as_deref().is_some_and(is_pre_split);
                if !pre_split {
                    continue;
                }
                let uid_hash: [u8; 32] = row
                    .uid_hash
                    .as_slice()
                    .try_into()
                    .context("bridge_caldav_events.uid_hash is not 32 bytes")?;
                let now = now_epoch_secs();
                // I1: 0.1.x compat-free window (`version-compatibility.md`
                // § Dimension 2, the fifth exception) — a pre-split event no
                // app can open; its owner re-creates it.
                let outcome = state
                    .db
                    .delete_caldav_event_by_uid(&actor, &cal.calendar_id, &uid_hash, None, now)
                    .await?;
                if let DeleteCaldavEventOutcome::Deleted { event_id, modseq } = outcome {
                    journal_event_delete(
                        state,
                        actor,
                        cal.calendar_id,
                        uid_hash,
                        event_id,
                        modseq,
                        now,
                    )
                    .await
                    .map_err(rpc)?;
                    done.events += 1;
                }
            }
            if is_pre_split(&cal.encrypted_metadata) {
                // I1: 0.1.x compat-free window — metadata no app can open,
                // blanked to the default; the calendar and its readable events
                // stay.
                update_calendar_metadata(state, actor, cal.calendar_id, &[])
                    .await
                    .map_err(rpc)?;
                done.calendars_blanked += 1;
            }
        }
    }
    Ok(())
}

async fn tombstone_addressbooks(state: &Arc<AppState>, done: &mut Tombstoned) -> Result<()> {
    for actor in state.db.carddav_actor_ids().await? {
        for book in state.db.list_bridge_carddav_addressbooks(&actor).await? {
            let page = state
                .db
                .query_carddav_cards(&actor, &book.addressbook_id, None, None, 0)
                .await?;
            for row in page.cards {
                let body = crate::segments::card::load_card_body(
                    &state.card_segments,
                    &state.db,
                    &actor,
                    &row,
                )
                .await?;
                let pre_split = body.as_deref().is_some_and(is_pre_split)
                    || row.encrypted_fauna_ext.as_deref().is_some_and(is_pre_split);
                if !pre_split {
                    continue;
                }
                let uid_hash: [u8; 32] = row
                    .uid_hash
                    .as_slice()
                    .try_into()
                    .context("bridge_carddav_cards.uid_hash is not 32 bytes")?;
                let now = now_epoch_secs();
                // I1: 0.1.x compat-free window (`version-compatibility.md`
                // § Dimension 2, the fifth exception) — a pre-split card no
                // app can open; its owner re-creates it.
                let outcome = state
                    .db
                    .delete_carddav_card_by_uid(&actor, &book.addressbook_id, &uid_hash, None, now)
                    .await?;
                if let DeleteCarddavCardOutcome::Deleted { card_id, modseq } = outcome {
                    journal_card_delete(
                        state,
                        actor,
                        book.addressbook_id,
                        uid_hash,
                        card_id,
                        modseq,
                        now,
                    )
                    .await
                    .map_err(rpc)?;
                    done.cards += 1;
                }
            }
            if is_pre_split(&book.encrypted_metadata) {
                // I1: 0.1.x compat-free window — metadata no app can open,
                // blanked to the default; the address book and its readable
                // cards stay.
                update_addressbook_metadata(state, actor, book.addressbook_id, &[])
                    .await
                    .map_err(rpc)?;
                done.addressbooks_blanked += 1;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fixture_record;
    use fauna_contacts::segments::placement::CardPlacementRecord;
    use fauna_mls::wrapped_blob::SealedRecordBytes;
    use fauna_protocol::bridge_routing::{PutCardCiphertextRequest, PutEventCiphertextRequest};

    const ACTOR: [u8; 32] = [0x61; 32];
    const CAL: [u8; 32] = [0x62; 32];
    const BOOK: [u8; 32] = [0x63; 32];

    fn rec(kind: RecordKind, label: &[u8]) -> Vec<u8> {
        fixture_record(kind, label)
    }

    /// File an event through the production storage path, below the put door
    /// (which now refuses a `mail-record` body or sidecar).
    async fn seed_event(
        state: &Arc<AppState>,
        uid: [u8; 32],
        body: Vec<u8>,
        sidecar: Option<Vec<u8>>,
    ) {
        let req = PutEventCiphertextRequest {
            actor_id: ACTOR.to_vec(),
            calendar_id: CAL.to_vec(),
            uid_hash: uid.to_vec(),
            encrypted_body: body.clone(),
            encrypted_index_hint: rec(RecordKind::Mail, b"hint"),
            timestamp: 1_700_000_000,
            ciphertext_size: body.len() as u32,
            encrypted_fauna_ext: sidecar,
            ..Default::default()
        };
        crate::bridge_caldav_handlers::store_sealed_event(
            state,
            ACTOR,
            CAL,
            &req,
            SealedRecordBytes::verify(body).unwrap(),
            SealedRecordBytes::verify(req.encrypted_index_hint.clone()).unwrap(),
        )
        .await
        .expect("seed event");
    }

    async fn seed_card(
        state: &Arc<AppState>,
        uid: [u8; 32],
        body: Vec<u8>,
        sidecar: Option<Vec<u8>>,
    ) {
        let req = PutCardCiphertextRequest {
            actor_id: ACTOR.to_vec(),
            addressbook_id: BOOK.to_vec(),
            uid_hash: uid.to_vec(),
            encrypted_body: body.clone(),
            encrypted_index_hint: rec(RecordKind::Mail, b"hint"),
            timestamp: 1_700_000_000,
            ciphertext_size: body.len() as u32,
            encrypted_fauna_ext: sidecar,
            ..Default::default()
        };
        crate::bridge_carddav_handlers::store_sealed_card(
            state,
            ACTOR,
            BOOK,
            &req,
            SealedRecordBytes::verify(body).unwrap(),
            SealedRecordBytes::verify(req.encrypted_index_hint.clone()).unwrap(),
        )
        .await
        .expect("seed card");
    }

    async fn seed_collections(state: &Arc<AppState>, cal_meta: Vec<u8>, book_meta: Vec<u8>) {
        crate::bridge_caldav_handlers::insert_calendar(state, ACTOR, CAL, &cal_meta)
            .await
            .expect("seed calendar");
        state
            .db
            .insert_bridge_carddav_addressbook(&ACTOR, &BOOK, &book_meta, 1_700_000_000)
            .await
            .expect("seed address book");
        state
            .card_placement
            .append_event(
                &ACTOR,
                &CardPlacementRecord::ProvisionAddressbook {
                    addressbook_id: BOOK,
                    encrypted_metadata: book_meta,
                },
            )
            .await
            .expect("journal address book");
    }

    async fn event_uids(state: &Arc<AppState>) -> Vec<Vec<u8>> {
        let page = state
            .db
            .query_caldav_events(&ACTOR, &CAL, None, None, 0)
            .await
            .unwrap();
        page.events.into_iter().map(|r| r.uid_hash).collect()
    }

    async fn card_uids(state: &Arc<AppState>) -> Vec<Vec<u8>> {
        let page = state
            .db
            .query_carddav_cards(&ACTOR, &BOOK, None, None, 0)
            .await
            .unwrap();
        page.cards.into_iter().map(|r| r.uid_hash).collect()
    }

    #[tokio::test]
    async fn pre_split_records_are_tombstoned_and_journaled_post_split_twins_survive() {
        let state = crate::test_support::fixture_state();
        seed_collections(
            &state,
            rec(RecordKind::Mail, b"cal-meta"),
            rec(RecordKind::Mail, b"book-meta"),
        )
        .await;
        // Pre-split by body, pre-split by sidecar only, and a post-split twin.
        seed_event(&state, [1; 32], rec(RecordKind::Mail, b"ev1"), None).await;
        seed_event(
            &state,
            [2; 32],
            rec(RecordKind::Calendar, b"ev2"),
            Some(rec(RecordKind::Mail, b"ev2-ext")),
        )
        .await;
        seed_event(
            &state,
            [3; 32],
            rec(RecordKind::Calendar, b"ev3"),
            Some(rec(RecordKind::Calendar, b"ev3-ext")),
        )
        .await;
        seed_card(&state, [4; 32], rec(RecordKind::Mail, b"c4"), None).await;
        seed_card(
            &state,
            [5; 32],
            rec(RecordKind::Card, b"c5"),
            Some(rec(RecordKind::Mail, b"c5-ext")),
        )
        .await;
        seed_card(&state, [6; 32], rec(RecordKind::Card, b"c6"), None).await;
        let cal_hms_before = state
            .db
            .caldav_calendar_highestmodseq(&ACTOR, &CAL)
            .await
            .unwrap()
            .unwrap();
        let book_hms_before = state
            .db
            .carddav_addressbook_highestmodseq(&ACTOR, &BOOK)
            .await
            .unwrap()
            .unwrap();

        state
            .db
            .set_dav_pre_split_tombstone_pending()
            .await
            .unwrap();
        let done = run(&state).await.unwrap().expect("the pass was owed");
        assert_eq!(
            done,
            Tombstoned {
                events: 2,
                cards: 2,
                calendars_blanked: 1,
                addressbooks_blanked: 1,
            }
        );

        // Only the post-split twins remain.
        assert_eq!(event_uids(&state).await, vec![vec![3u8; 32]]);
        assert_eq!(card_uids(&state).await, vec![vec![6u8; 32]]);

        // The removals are WebDAV-Sync-visible from a pre-boot token.
        let expunged = state
            .db
            .query_caldav_expunged_since(&ACTOR, &CAL, cal_hms_before)
            .await
            .unwrap();
        let mut gone: Vec<_> = expunged.into_iter().map(|r| r.uid_hash).collect();
        gone.sort();
        assert_eq!(gone, vec![vec![1u8; 32], vec![2u8; 32]]);
        let expunged_cards = state
            .db
            .query_carddav_expunged_since(&ACTOR, &BOOK, book_hms_before)
            .await
            .unwrap();
        let mut gone_cards: Vec<_> = expunged_cards.into_iter().map(|r| r.uid_hash).collect();
        gone_cards.sort();
        assert_eq!(gone_cards, vec![vec![4u8; 32], vec![5u8; 32]]);

        // The placement journal agrees: one live event and card each, two
        // tombstones each, and the blanked metadata.
        let cal_m = state.cal_placement.current_manifest(&ACTOR).await.unwrap();
        assert_eq!(cal_m.events.len(), 1);
        assert_eq!(cal_m.tombstones.len(), 2);
        assert!(cal_m.calendars[0].encrypted_metadata.is_empty());
        let card_m = state.card_placement.current_manifest(&ACTOR).await.unwrap();
        assert_eq!(card_m.cards.len(), 1);
        assert_eq!(card_m.tombstones.len(), 2);
        assert!(card_m.addressbooks[0].encrypted_metadata.is_empty());

        // The collections stay, their metadata blank.
        let cals = state.db.list_bridge_caldav_calendars(&ACTOR).await.unwrap();
        assert_eq!(cals.len(), 1);
        assert!(cals[0].encrypted_metadata.is_empty());
        let books = state
            .db
            .list_bridge_carddav_addressbooks(&ACTOR)
            .await
            .unwrap();
        assert_eq!(books.len(), 1);
        assert!(books[0].encrypted_metadata.is_empty());

        // Done once: the mark is cleared, so a second boot scans nothing.
        assert!(!state.db.dav_pre_split_tombstone_pending().await.unwrap());
        assert_eq!(run(&state).await.unwrap(), None);
    }

    #[tokio::test]
    async fn post_split_collections_keep_their_metadata() {
        let state = crate::test_support::fixture_state();
        let cal_meta = rec(RecordKind::Calendar, b"cal-meta");
        let book_meta = rec(RecordKind::Card, b"book-meta");
        seed_collections(&state, cal_meta.clone(), book_meta.clone()).await;
        state
            .db
            .set_dav_pre_split_tombstone_pending()
            .await
            .unwrap();
        let done = run(&state).await.unwrap().expect("the pass was owed");
        assert_eq!(done, Tombstoned::default());
        let cals = state.db.list_bridge_caldav_calendars(&ACTOR).await.unwrap();
        assert_eq!(cals[0].encrypted_metadata, cal_meta);
        let books = state
            .db
            .list_bridge_carddav_addressbooks(&ACTOR)
            .await
            .unwrap();
        assert_eq!(books[0].encrypted_metadata, book_meta);
    }

    #[tokio::test]
    async fn a_fresh_database_never_runs_the_pass() {
        let state = crate::test_support::fixture_state();
        seed_collections(
            &state,
            rec(RecordKind::Mail, b"cal-meta"),
            rec(RecordKind::Mail, b"book-meta"),
        )
        .await;
        seed_event(&state, [1; 32], rec(RecordKind::Mail, b"ev1"), None).await;
        // No mark: the genesis of a fresh database never sets it.
        assert!(!state.db.dav_pre_split_tombstone_pending().await.unwrap());
        assert_eq!(run(&state).await.unwrap(), None);
        assert_eq!(event_uids(&state).await, vec![vec![1u8; 32]]);
    }

    #[test]
    fn only_a_mail_record_envelope_reads_as_pre_split() {
        assert!(is_pre_split(&rec(RecordKind::Mail, b"x")));
        assert!(!is_pre_split(&rec(RecordKind::Calendar, b"x")));
        assert!(!is_pre_split(&rec(RecordKind::Card, b"x")));
        assert!(!is_pre_split(b"BEGIN:VCALENDAR"));
        assert!(!is_pre_split(&[]));
    }
}
