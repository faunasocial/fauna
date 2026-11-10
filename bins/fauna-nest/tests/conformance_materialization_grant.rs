//! **The materialization grant, nest half** (tier_3) — real wire, real nest
//! (`docs/goal/architecture/encryption-at-rest.md` § Capability tiering → *The
//! materialization grant*, parts (a)–(d); class 4 of § Readable classes).
//!
//! Production data flow asserted on the real `fauna-nest` router over a real
//! socket: the owner's app mints the two-tuple grant with the shared
//! `fauna_client_capabilities::mint_materialization_grant` to the materializer
//! the roster lists → `fauna.capabilities.mint` stores it and arms the
//! materializer → a sealed mail lands through `segments::mail::append_record`
//! → the append seam's observer wakes the worker, which opens the record under
//! the held key and indexes it under `__materialized/<owner>/` → the owner's
//! `fauna.search.query` finds it by a body phrase (content id the RFC
//! `Message-ID`, the nest id in `extra`) → another member's query over the
//! same phrase finds nothing → `fauna.capabilities.revoke` deletes the area
//! before it replies, and search finds nothing → a restart (a fresh
//! materializer over the same data dir) keeps it gone.
//!
//! Beside the journey: a standing grant at boot materializes the sealed
//! history already held (retroactive), a lapsed window deletes the area at
//! the next reconcile, and the mint door refuses a `materialize` tuple sealed
//! to any other holder and a bounded (per-epoch) mail read.

mod common;
use common::connected_client;

use std::sync::Arc;
use std::time::Duration;

use fauna_client_capabilities::rpc::CapabilitiesClient;
use fauna_client_search::SearchClient;
use fauna_core::data::MailConfig;
use fauna_core::identity::ActorKeypair;
use fauna_mail::segments::{MAIL_FLOOR_FORMAT_VERSION, MailFloorMetadata};
use fauna_mls::wrapped_blob::{
    GrantWindow, ScopeTuple, SealedRecordBytes, build_grant_blob_with_epochs,
    derive_recipient_xwing_keypair, generate_x25519_keypair, seal_to_recipient_xwing,
};
use fauna_nest::db::CacheDb;
use fauna_nest::materializer::{ContentStores, MatKind, Materializer, area};
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::state::AuthState;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::search::{SearchQueryRequest, SearchResult};
use fauna_protocol::wrapped_blob::{ListServiceUsersReply, ListServiceUsersRequest};

const OWNER_SEED: [u8; 32] = [0x31; 32];
const OTHER_SEED: [u8; 32] = [0x32; 32];
const OWNER_MSEK: [u8; 32] = [0x5A; 32];

fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs() as u64
}

fn floor(received_at_ms: i64) -> MailFloorMetadata {
    MailFloorMetadata {
        format_version: MAIL_FLOOR_FORMAT_VERSION,
        received_at: received_at_ms,
        timestamp: received_at_ms / 1000,
        sender_domain: "example.org".into(),
        spam_disposition: "accept".into(),
        continuation_role: fauna_mail::segments::CONTINUATION_ROLE_NORMAL,
        ..Default::default()
    }
}

/// Seal `raw` exactly as the MTA does for this owner (X-Wing to the
/// MSEK-derived recipient key) and append it to their `__mail` segment.
async fn deliver(state: &AppState, owner: &[u8; 32], raw: &[u8]) {
    let kp = derive_recipient_xwing_keypair(&OWNER_MSEK);
    let body = seal_to_recipient_xwing(raw, &kp.public)
        .unwrap()
        .to_canonical_bytes()
        .unwrap();
    let hint = seal_to_recipient_xwing(b"hint", &kp.public)
        .unwrap()
        .to_canonical_bytes()
        .unwrap();
    fauna_nest::segments::mail::append_record(
        &state.mail_segments,
        &state.db,
        owner,
        &SealedRecordBytes::verify(body).unwrap(),
        &SealedRecordBytes::verify(hint).unwrap(),
        floor(fauna_core::data::Timestamp::now_millis() as i64),
    )
    .await
    .unwrap();
}

fn message(id: &str, body: &str) -> Vec<u8> {
    format!(
        "From: sender@example.org\r\nTo: owner@example.org\r\nSubject: Field notes\r\n\
         Message-ID: {id}\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}

fn stores(state: &AppState) -> ContentStores {
    ContentStores {
        mail: state.mail_segments.clone(),
        calendar: state.cal_segments.clone(),
        card: state.card_segments.clone(),
    }
}

/// The owner's app mint: the shared client function, sealed to the
/// materializer's published keys.
fn mint_blob(
    m: &Materializer,
    owner: &[u8; 32],
    grant_id: [u8; 16],
    window: GrantWindow,
) -> Vec<u8> {
    mint_kind_blob(m, "mail", owner, grant_id, window)
}

fn mint_kind_blob(
    m: &Materializer,
    kind: &str,
    owner: &[u8; 32],
    grant_id: [u8; 16],
    window: GrantWindow,
) -> Vec<u8> {
    let mail = MailConfig {
        msek: Some(OWNER_MSEK.into()),
        ..MailConfig::default()
    };
    fauna_client_capabilities::mint_materialization_grant(
        kind,
        &mail,
        owner,
        &grant_id,
        m.holder_x25519(),
        Some(m.holder_mlkem_ek()),
        window,
    )
    .unwrap()
    .to_canonical_bytes()
    .unwrap()
}

struct Nest {
    base: String,
    state: Arc<AppState>,
    materializer: Arc<Materializer>,
}

async fn start_nest(db: Arc<CacheDb>) -> Nest {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let base_state = AppState::for_test(db.clone());
    let data_dir = base_state.mail_segments.data_dir().to_path_buf();
    let materializer = Materializer::init(&data_dir, db.clone(), stores(&base_state))
        .await
        .unwrap()
        .expect("the materializer comes up");
    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::bridge_blob_handlers::register_bridge_blob_handlers(&mut b);
            fauna_nest::bridge_blob_handlers::register_capability_handlers(&mut b);
            fauna_nest::search_handlers::register_search_handlers(&mut b);
            b.build()
        }),
        auth: AuthState {
            token_store: Arc::new(TokenStore::new()),
            registration: RegistrationConfig {
                handle_domain: Some(authority.clone()),
                ..Default::default()
            },
            ..Default::default()
        },
        materializer: Some(materializer.clone()),
        ..base_state
    });
    tokio::spawn(materializer.clone().run());
    let app = fauna_nest::build_router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    Nest {
        base: format!("http://{authority}"),
        state,
        materializer,
    }
}

fn query(q: &str) -> SearchQueryRequest {
    SearchQueryRequest {
        query: q.into(),
        content_type: None,
        before: None,
        after: None,
        limit: Some(20),
        offset: None,
        extra: Default::default(),
    }
}

fn mail_hits(results: &[SearchResult]) -> Vec<&SearchResult> {
    results
        .iter()
        .filter(|r| r.content_type == "mail")
        .collect()
}

#[tokio::test]
async fn the_owner_finds_their_sealed_mail_and_revoke_deletes_the_view_for_good() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let owner = ActorKeypair::from_secret(OWNER_SEED).actor_id().0;
    let other = ActorKeypair::from_secret(OTHER_SEED).actor_id().0;
    db.create_user(&owner, "free", "owner").await.unwrap();
    db.create_user(&other, "free", "other").await.unwrap();
    let nest = start_nest(db.clone()).await;
    let owner_nest = connected_client(&nest.base, ActorKeypair::from_secret(OWNER_SEED)).await;
    let other_nest = connected_client(&nest.base, ActorKeypair::from_secret(OTHER_SEED)).await;

    // Every member's mint picker discovers the materializer on the roster.
    let roster: ListServiceUsersReply = owner_nest
        .request(
            "fauna.bridges.list_service_users",
            ListServiceUsersRequest {
                role: None,
                status: None,
                extra: Default::default(),
            },
        )
        .await
        .unwrap();
    assert!(
        roster
            .service_users
            .iter()
            .any(|u| u.bridge_id == "materializer" && u.role == "content-processor"),
        "the user-visible roster lists the in-process materializer"
    );

    let grant_id = [0x11u8; 16];
    let blob = mint_blob(
        &nest.materializer,
        &owner,
        grant_id,
        GrantWindow(0, u64::MAX),
    );
    CapabilitiesClient::new(owner_nest.clone())
        .mint(blob)
        .await
        .expect("the materializer accepts its own grant");
    let dir = area::actor_dir(nest.materializer.data_dir(), &owner);
    assert!(dir.exists(), "the mint arms the area");

    deliver(
        &nest.state,
        &owner,
        &message(
            "<harvest-1@example.org>",
            "the quarterly harvest ledger is ready",
        ),
    )
    .await;

    let search = SearchClient::new(owner_nest.clone());
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let hit = loop {
        let reply = search.query(query("harvest")).await.unwrap();
        if let Some(h) = mail_hits(&reply.results).first() {
            break (*h).clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the trickle never indexed the mail"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(hit.content_id, "<harvest-1@example.org>");
    assert!(
        hit.snippet.contains("harvest"),
        "snippet rendered from the sealed record: {:?}",
        hit.snippet
    );
    assert!(
        matches!(hit.extra.get("secondary_id"), Some(fauna_cbor::Value::Bytes(b)) if b.len() == 32),
        "the nest message id rides `extra`"
    );

    let theirs = SearchClient::new(other_nest.clone())
        .query(query("harvest"))
        .await
        .unwrap();
    assert!(
        mail_hits(&theirs.results).is_empty(),
        "another member never sees the owner's view"
    );

    CapabilitiesClient::new(owner_nest.clone())
        .revoke(grant_id)
        .await
        .unwrap();
    assert!(
        !dir.exists(),
        "the revoke deleted the area before it replied"
    );
    let after = search.query(query("harvest")).await.unwrap();
    assert!(
        mail_hits(&after.results).is_empty(),
        "nothing answers after revoke"
    );

    // A restart: the boot reconcile keeps it gone.
    let rebooted = Materializer::init(
        nest.materializer.data_dir(),
        db.clone(),
        stores(&nest.state),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!dir.exists());
    assert!(
        rebooted
            .search(&owner, &[MatKind::Mail], "harvest", None, 20)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn a_standing_grant_at_boot_materializes_the_history_already_held() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let owner = ActorKeypair::from_secret(OWNER_SEED).actor_id().0;
    db.create_user(&owner, "free", "owner").await.unwrap();
    let nest = start_nest(db.clone()).await;

    // History first, with no grant: nothing is kept.
    deliver(
        &nest.state,
        &owner,
        &message("<old-1@example.org>", "an archived lighthouse survey"),
    )
    .await;
    deliver(
        &nest.state,
        &owner,
        &message("<old-2@example.org>", "lighthouse keeper rota"),
    )
    .await;
    assert!(!area::actor_dir(nest.materializer.data_dir(), &owner).exists());

    // The grant row stands when the nest boots (written as the mint would).
    let blob = mint_blob(
        &nest.materializer,
        &owner,
        [0x22; 16],
        GrantWindow(0, u64::MAX),
    );
    db.put_capability_grant(
        &owner,
        &[0x22; 16],
        nest.materializer.holder_x25519(),
        i64::MAX,
        &blob,
    )
    .await
    .unwrap();
    let booted = Materializer::init(
        nest.materializer.data_dir(),
        db.clone(),
        stores(&nest.state),
    )
    .await
    .unwrap()
    .unwrap();
    booted.process(&owner).await.unwrap();
    let hits = booted
        .search(&owner, &[MatKind::Mail], "lighthouse", None, 20)
        .await
        .expect("armed at boot");
    let mut ids: Vec<_> = hits.iter().map(|h| h.content_id.clone()).collect();
    ids.sort();
    assert_eq!(ids, vec!["<old-1@example.org>", "<old-2@example.org>"]);
}

#[tokio::test]
async fn a_lapsed_window_deletes_the_area_at_the_next_reconcile() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let owner = ActorKeypair::from_secret(OWNER_SEED).actor_id().0;
    db.create_user(&owner, "free", "owner").await.unwrap();
    let nest = start_nest(db.clone()).await;
    let owner_nest = connected_client(&nest.base, ActorKeypair::from_secret(OWNER_SEED)).await;

    let end = now_secs() + 2;
    let blob = mint_blob(&nest.materializer, &owner, [0x33; 16], GrantWindow(0, end));
    CapabilitiesClient::new(owner_nest.clone())
        .mint(blob)
        .await
        .unwrap();
    let dir = area::actor_dir(nest.materializer.data_dir(), &owner);
    assert!(dir.exists());
    while now_secs() <= end {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    nest.materializer.reconcile().await;
    assert!(!dir.exists(), "a lapsed one-off window ends the view");
}

#[tokio::test]
async fn the_mint_door_refuses_another_holder_and_a_bounded_mail_read() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let owner = ActorKeypair::from_secret(OWNER_SEED).actor_id().0;
    db.create_user(&owner, "free", "owner").await.unwrap();
    let nest = start_nest(db.clone()).await;
    let caps = CapabilitiesClient::new(
        connected_client(&nest.base, ActorKeypair::from_secret(OWNER_SEED)).await,
    );

    // A well-formed materialization grant sealed to some other key.
    let (_sk, stranger) = generate_x25519_keypair();
    let mail = MailConfig {
        msek: Some(OWNER_MSEK.into()),
        ..MailConfig::default()
    };
    let foreign = fauna_client_capabilities::mint_materialization_grant(
        "mail",
        &mail,
        &owner,
        &[0x44; 16],
        &stranger,
        None,
        GrantWindow(0, u64::MAX),
    )
    .unwrap()
    .to_canonical_bytes()
    .unwrap();
    let err = caps
        .mint(foreign)
        .await
        .expect_err("only this nest's materializer");
    assert!(format!("{err:?}").contains("permission"), "{err:?}");

    // The bounded (per-epoch) regime, to the right holder.
    let bounded = build_grant_blob_with_epochs(
        &owner,
        &[0x55; 16],
        nest.materializer.holder_x25519(),
        Some(nest.materializer.holder_mlkem_ek()),
        GrantWindow(0, u64::MAX),
        &[
            (
                ScopeTuple::delegable_kind_read("mail"),
                vec![(Some(7), vec![1u8; 32])],
            ),
            (ScopeTuple::materialize("mail"), vec![]),
        ],
    )
    .unwrap()
    .to_canonical_bytes()
    .unwrap();
    let err = caps
        .mint(bounded)
        .await
        .expect_err("the master-key regime only");
    assert!(format!("{err:?}").contains("permission"), "{err:?}");
    assert!(
        db.fetch_capability_grants_for_holder(nest.materializer.holder_x25519(), 0)
            .await
            .unwrap()
            .is_empty(),
        "nothing refused was stored"
    );
}

// ── The follow-on kinds: calendar and card ────────────────────────────────

const CALENDAR: [u8; 32] = [0xCA; 32];
const BOOK: [u8; 32] = [0xB0; 32];
const EVENT_UID: [u8; 32] = [0xE1; 32];
const CARD_UID: [u8; 32] = [0xC1; 32];

fn seal_dav(kind: fauna_mls::wrapped_blob::DavKind, plaintext: &[u8]) -> Vec<u8> {
    fauna_mls::wrapped_blob::dav_body::seal_dav_body_xwing(kind, plaintext, &OWNER_MSEK).unwrap()
}

/// Store one event the way a CalDAV PUT does: the sealed record into the
/// owner's `__calendar` segment and the `bridge_caldav_events` row naming it
/// (the row derives the same `record_cid` from the same body + hint).
async fn put_event(state: &AppState, owner: &[u8; 32], ical: &str) {
    use fauna_mls::wrapped_blob::DavKind;
    let now = now_secs() as i64;
    state
        .db
        .insert_bridge_caldav_calendar(owner, &CALENDAR, b"meta", now)
        .await
        .unwrap();
    let body = seal_dav(DavKind::Calendar, ical.as_bytes());
    let hint = seal_dav(DavKind::Calendar, b"hint");
    let placed = state
        .db
        .place_caldav_event(
            owner,
            &CALENDAR,
            &EVENT_UID,
            &body,
            &hint,
            now,
            body.len() as u32,
            now,
        )
        .await
        .unwrap();
    assert!(
        format!("{placed:?}").starts_with("Created"),
        "the event row is placed: {placed:?}"
    );
    fauna_nest::segments::cal::append_record(
        &state.cal_segments,
        &state.db,
        owner,
        &SealedRecordBytes::verify(body).unwrap(),
        &SealedRecordBytes::verify(hint).unwrap(),
        &fauna_calendar::segments::CalFloorMetadata {
            calendar_id: CALENDAR,
            uid_hash: EVENT_UID.to_vec(),
            created_at: now,
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

/// Store one card the way a CardDAV PUT does (twin of [`put_event`]).
async fn put_card(state: &AppState, owner: &[u8; 32], vcard: &str) {
    use fauna_mls::wrapped_blob::DavKind;
    let now = now_secs() as i64;
    state
        .db
        .insert_bridge_carddav_addressbook(owner, &BOOK, b"meta", now)
        .await
        .unwrap();
    let body = seal_dav(DavKind::Card, vcard.as_bytes());
    let hint = seal_dav(DavKind::Card, b"hint");
    let placed = state
        .db
        .place_carddav_card(
            owner,
            &BOOK,
            &CARD_UID,
            &body,
            &hint,
            now,
            body.len() as u32,
            now,
        )
        .await
        .unwrap();
    assert!(
        format!("{placed:?}").starts_with("Created"),
        "the card row is placed: {placed:?}"
    );
    fauna_nest::segments::card::append_record(
        &state.card_segments,
        &state.db,
        owner,
        &SealedRecordBytes::verify(body).unwrap(),
        &SealedRecordBytes::verify(hint).unwrap(),
        &fauna_contacts::segments::CardFloorMetadata {
            addressbook_id: BOOK,
            uid_hash: CARD_UID.to_vec(),
            created_at: now,
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

fn typed(q: &str, content_type: Option<&str>) -> SearchQueryRequest {
    SearchQueryRequest {
        content_type: content_type.map(str::to_string),
        ..query(q)
    }
}

async fn wait_hit(
    search: &SearchClient<Arc<fauna_client::NestClient>>,
    q: &str,
    content_type: &str,
) -> SearchResult {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let reply = search.query(query(q)).await.unwrap();
        if let Some(h) = reply
            .results
            .iter()
            .find(|r| r.content_type == content_type)
        {
            return h.clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the trickle never indexed the {content_type}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Production flow: the owner's app mints a calendar and a card
/// materialization grant (each its own `materialize{kind}` grant under its
/// own key) → each arms its own `__materialized/<owner>/<kind>/` view → an
/// event and a card stored as a DAV PUT stores them trickle in → the owner's
/// `fauna.search.query` finds each (content type `calendar` / `contact`,
/// content id the hex `uid_hash`, snippet from the sealed record), a type
/// filter answers that kind alone, another member finds nothing → a card
/// delete retires its doc → revoking the calendar grant deletes the calendar
/// view alone, the card view standing.
#[tokio::test]
async fn calendar_and_card_views_are_searchable_and_each_revokes_alone() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let owner = ActorKeypair::from_secret(OWNER_SEED).actor_id().0;
    let other = ActorKeypair::from_secret(OTHER_SEED).actor_id().0;
    db.create_user(&owner, "free", "owner").await.unwrap();
    db.create_user(&other, "free", "other").await.unwrap();
    let nest = start_nest(db.clone()).await;
    let owner_nest = connected_client(&nest.base, ActorKeypair::from_secret(OWNER_SEED)).await;
    let other_nest = connected_client(&nest.base, ActorKeypair::from_secret(OTHER_SEED)).await;
    let caps = CapabilitiesClient::new(owner_nest.clone());

    let (cal_grant, card_grant) = ([0x66u8; 16], [0x77u8; 16]);
    for (kind, id) in [("calendar", cal_grant), ("card", card_grant)] {
        caps.mint(mint_kind_blob(
            &nest.materializer,
            kind,
            &owner,
            id,
            GrantWindow(0, u64::MAX),
        ))
        .await
        .unwrap_or_else(|e| panic!("the materializer accepts a {kind} grant: {e:?}"));
    }
    let data = nest.materializer.data_dir();
    let cal_dir = area::kind_dir(data, &owner, MatKind::Calendar);
    let card_dir = area::kind_dir(data, &owner, MatKind::Card);
    assert!(
        cal_dir.exists() && card_dir.exists(),
        "each mint arms its own view"
    );
    assert!(
        !area::kind_dir(data, &owner, MatKind::Mail).exists(),
        "no mail grant, no mail view"
    );

    put_event(
        &nest.state,
        &owner,
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:festival-1\r\n\
         DTSTART:20261201T180000Z\r\nDTEND:20261201T200000Z\r\nSUMMARY:Harvest festival\r\n\
         DESCRIPTION:bring the paper lanterns\r\nLOCATION:Old mill\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
    )
    .await;
    put_card(
        &nest.state,
        &owner,
        "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:ada-1\r\nFN:Ada Lanternmaker\r\n\
         NOTE:met at the mill\r\nEND:VCARD\r\n",
    )
    .await;

    let search = SearchClient::new(owner_nest.clone());
    let event = wait_hit(&search, "lanterns", "calendar").await;
    assert_eq!(event.content_id, hex::encode(EVENT_UID));
    assert!(event.snippet.contains("lanterns"), "{:?}", event.snippet);
    assert!(
        event.extra.is_empty(),
        "a DAV hit carries no nest message id"
    );
    let card = wait_hit(&search, "Lanternmaker", "contact").await;
    assert_eq!(card.content_id, hex::encode(CARD_UID));
    assert!(
        search
            .query(query("Lanternmaker"))
            .await
            .unwrap()
            .results
            .iter()
            .all(|r| r.content_type != "card"),
        "a card hit is a `contact`, never a `card`"
    );

    let only_cal = search
        .query(typed("lanterns mill", Some("calendar")))
        .await
        .unwrap();
    assert!(!only_cal.results.is_empty());
    assert!(
        only_cal
            .results
            .iter()
            .all(|r| r.content_type == "calendar"),
        "a type filter answers that kind alone: {:?}",
        only_cal.results
    );

    let theirs = SearchClient::new(other_nest.clone())
        .query(query("lanterns"))
        .await
        .unwrap();
    assert!(
        theirs.results.is_empty(),
        "another member never sees the owner's views"
    );

    // A delete retires the doc at the next walk.
    db.delete_carddav_card_by_uid(&owner, &BOOK, &CARD_UID, None, now_secs() as i64)
        .await
        .unwrap();
    nest.materializer.process(&owner).await.unwrap();
    let after = search.query(query("Lanternmaker")).await.unwrap();
    assert!(
        after.results.iter().all(|r| r.content_type != "contact"),
        "the deleted card is retired"
    );

    caps.revoke(cal_grant).await.unwrap();
    assert!(
        !cal_dir.exists(),
        "the calendar revoke deleted its view before replying"
    );
    assert!(
        card_dir.exists(),
        "the card view rides its own grant and stands"
    );
    let after = search.query(query("lanterns")).await.unwrap();
    assert!(
        after.results.iter().all(|r| r.content_type != "calendar"),
        "nothing answers for the revoked kind"
    );
}

/// A calendar grant opens no card: with the calendar grant alone, a stored
/// card is never materialized, and the nest keeps no card view at all.
#[tokio::test]
async fn a_calendar_grant_keeps_no_card_view() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let owner = ActorKeypair::from_secret(OWNER_SEED).actor_id().0;
    db.create_user(&owner, "free", "owner").await.unwrap();
    let nest = start_nest(db.clone()).await;
    let owner_nest = connected_client(&nest.base, ActorKeypair::from_secret(OWNER_SEED)).await;
    CapabilitiesClient::new(owner_nest.clone())
        .mint(mint_kind_blob(
            &nest.materializer,
            "calendar",
            &owner,
            [0x88; 16],
            GrantWindow(0, u64::MAX),
        ))
        .await
        .unwrap();
    put_card(
        &nest.state,
        &owner,
        "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:z\r\nFN:Zed Quillwright\r\nEND:VCARD\r\n",
    )
    .await;
    nest.materializer.process(&owner).await.unwrap();
    let hits = nest
        .materializer
        .search(&owner, &[MatKind::Card], "Quillwright", None, 20)
        .await;
    assert!(hits.is_none(), "no card grant, no card view: {hits:?}");
    assert!(!area::kind_dir(nest.materializer.data_dir(), &owner, MatKind::Card).exists());
}

/// The version-1 layout (one mail view at the actor's root) is derived
/// state: the boot reconcile deletes it whole.
#[tokio::test]
async fn a_version_one_area_is_deleted_whole_at_boot() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let owner = ActorKeypair::from_secret(OWNER_SEED).actor_id().0;
    db.create_user(&owner, "free", "owner").await.unwrap();
    let nest = start_nest(db.clone()).await;
    let actor_dir = area::actor_dir(nest.materializer.data_dir(), &owner);
    std::fs::create_dir_all(actor_dir.join("mail")).unwrap();
    std::fs::write(actor_dir.join("state.db"), b"v1").unwrap();
    let _booted = Materializer::init(
        nest.materializer.data_dir(),
        db.clone(),
        stores(&nest.state),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        !actor_dir.exists(),
        "the v1 area is deleted whole, never migrated"
    );
}
