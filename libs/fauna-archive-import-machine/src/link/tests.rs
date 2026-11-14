//! tier_1 pins for phase two (`archive-import.md` § Testing, the phase-two
//! bullet), over two in-memory nests — Alice's and Bob's — and a recorded
//! wire between them standing in for the pair's MLS channel.

use std::sync::{Arc, Mutex};

use fauna_archive::model::{
    ArchiveComment, ArchiveFriendship, ArchiveMediaRef, ArchivePost, ArchiveReaction,
    ArchiveTargetRef, FriendshipKind, ReactionKind,
};
use fauna_archive::{
    ArchiveAudience, Category, Entity, EntityKind, ExternalActorRef, ExternalId, Platform,
};
use fauna_core::data::{Reference, Timestamp};
use fauna_core::identity::ActorKeypair;

use super::*;
use crate::nest::fakes::FakeNest;
use crate::state::{
    ArchiveMarker, ImportPhase, ImportScope, ImportState, ImportTarget, ImportedRecord,
    StoredAudienceMode, model_path,
};

const FB: Platform = Platform::Facebook;
const FOLDER: &str = "Facebook archive";
const URL_PUBLIC: &str = "https://www.facebook.com/alice/posts/1";
const URL_FRIENDS: &str = "https://www.facebook.com/alice/posts/2";
const URL_ONLY_ME: &str = "https://www.facebook.com/alice/posts/3";

fn person(name: &str) -> ExternalActorRef {
    ExternalActorRef::new(FB, None, name)
}

fn ext(kind: EntityKind, id: &str) -> ExternalId {
    ExternalId {
        platform: FB,
        kind,
        id: id.to_string(),
    }
}

fn post_id_of(tag: &str) -> String {
    blake3::hash(tag.as_bytes()).to_hex().to_string()
}

/// The recorded wire: every post a side makes, to whom.
#[derive(Default)]
struct Wire {
    sent: Mutex<Vec<(String, Vec<u8>)>>,
    down: Mutex<bool>,
}

#[async_trait::async_trait]
impl ArchiveSignalPoster for Wire {
    async fn post(&self, contact_hex: &str, bytes: Vec<u8>) -> Result<(), ArchiveNestError> {
        if *self.down.lock().unwrap() {
            return Err(ArchiveNestError::Transport("wire down".into()));
        }
        self.sent
            .lock()
            .unwrap()
            .push((contact_hex.to_string(), bytes));
        Ok(())
    }
}

impl Wire {
    fn take(&self) -> Vec<(String, ArchiveSignal)> {
        std::mem::take(&mut *self.sent.lock().unwrap())
            .into_iter()
            .map(|(to, b)| (to, fauna_cbor::decode_strict(&b).unwrap()))
            .collect()
    }
    fn set_down(&self, down: bool) {
        *self.down.lock().unwrap() = down;
    }
}

struct Side {
    hex: String,
    me: ExternalActorRef,
    nest: Arc<FakeNest>,
    wire: Arc<Wire>,
    links: ArchiveLinks,
}

fn marker(owner: &ExternalActorRef, created_at: u64) -> ArchiveMarker {
    ArchiveMarker {
        platform: FB,
        owner: owner.clone(),
        import_id: format!("{created_at:064x}"),
        parser_version: 1,
        raw_file_name: "facebook.zip".into(),
        created_at: Timestamp(created_at),
        raw_len: None,
        raw_blake3: None,
    }
}

fn import_state(imported: Vec<ImportedRecord>) -> ImportState {
    ImportState {
        import_id: "00".repeat(32),
        scope: ImportScope {
            categories: vec![Category::Posts],
            audience_mode: StoredAudienceMode::Original,
            date_from: None,
            date_until_exclusive: None,
            extra: Default::default(),
        },
        phase: ImportPhase::Finished,
        imported,
        skipped: vec![],
        updated_at: Timestamp(1),
        in_flight: None,
        extra: Default::default(),
    }
}

fn write_model(nest: &FakeNest, folder: &str, category: &Category, entities: Vec<Entity>) {
    nest.put_file(
        folder,
        &model_path(category),
        fauna_cbor::encode_canonical(&entities).unwrap(),
    );
}

fn friend(name: &str) -> Entity {
    Entity::Friendship(ArchiveFriendship {
        actor: person(name),
        since: None,
        kind: FriendshipKind::Friend,
    })
}

fn a_post(id: &str, audience: ArchiveAudience, url: &str) -> ArchivePost {
    ArchivePost {
        external_id: ext(EntityKind::Post, id),
        created_at: fauna_archive::Timestamp(1_500_000_000_000_000),
        audience,
        text: Some(format!("post {id}")),
        media: vec![],
        links: vec![],
        tagged: vec![],
        album_name: None,
        url: Some(url.to_string()),
        place: None,
    }
}

fn target(owner: &str, id: Option<ExternalId>, url: Option<&str>) -> ArchiveTargetRef {
    ArchiveTargetRef {
        id,
        url: url.map(str::to_string),
        owner: Some(person(owner)),
        kind_hint: None,
    }
}

fn comment(
    id: &str,
    author: &str,
    target: ArchiveTargetRef,
    text: Option<&str>,
    media: usize,
) -> Entity {
    Entity::Comment(ArchiveComment {
        external_id: ext(EntityKind::Comment, id),
        created_at: fauna_archive::Timestamp(1_600_000_000_000_000),
        author: person(author),
        target,
        text: text.map(str::to_string),
        media: (0..media)
            .map(|i| ArchiveMediaRef {
                path: format!("media/{i}.jpg"),
                size: None,
                blake3_hex: None,
                mime: None,
                taken_at: None,
                caption: None,
            })
            .collect(),
    })
}

fn reaction(id: &str, author: &str, target: ArchiveTargetRef, kind: ReactionKind) -> Entity {
    Entity::Reaction(ArchiveReaction {
        external_id: ext(EntityKind::Reaction, id),
        created_at: fauna_archive::Timestamp(1_600_000_000_000_001),
        author: person(author),
        target,
        kind,
    })
}

fn side(seed: u8, name: &str, friends: &[&str]) -> Side {
    let kp = ActorKeypair::from_secret([seed; 32]);
    let me = person(name);
    let nest = Arc::new(FakeNest::new());
    nest.seed_folder(FOLDER, &marker(&me, 10), &import_state(vec![]));
    write_model(
        &nest,
        FOLDER,
        &Category::Friends,
        friends.iter().map(|f| friend(f)).collect(),
    );
    let wire = Arc::new(Wire::default());
    let links = ArchiveLinks::new(nest.clone(), wire.clone(), &kp);
    Side {
        hex: kp.actor_id().to_hex(),
        me,
        nest,
        wire,
        links,
    }
}

/// Alice imported three posts — public, friends-only, only-me; Bob's own
/// comments and reactions on them are in his archive.
fn alice_and_bob() -> (Side, Side) {
    let alice = side(1, "Alice Archive", &["Bob Builder", "Carol Third"]);
    let bob = side(2, "Bob Builder", &["Alice Archive"]);
    alice.nest.set_contact(&bob.hex, "accepted");
    bob.nest.set_contact(&alice.hex, "confirmed");

    let posts = vec![
        a_post("p-public", ArchiveAudience::Public, URL_PUBLIC),
        a_post("p-friends", ArchiveAudience::Friends, URL_FRIENDS),
        a_post("p-only", ArchiveAudience::OnlyMe, URL_ONLY_ME),
    ];
    let imported = posts
        .iter()
        .map(|p| ImportedRecord {
            external_id: p.external_id.clone(),
            target: ImportTarget::Post {
                post_id: post_id_of(&p.external_id.id),
            },
        })
        .collect();
    alice
        .nest
        .seed_folder(FOLDER, &marker(&alice.me, 10), &import_state(imported));
    write_model(
        &alice.nest,
        FOLDER,
        &Category::Friends,
        vec![friend("Bob Builder"), friend("Carol Third")],
    );
    write_model(
        &alice.nest,
        FOLDER,
        &Category::Posts,
        posts.into_iter().map(Entity::Post).collect(),
    );
    (alice, bob)
}

/// Delivers every signal `from` posted to `to`, as the pair channel would,
/// and lets `to`'s nest serve the posts `from` created (federation).
async fn deliver(from: &Side, to: &Side) -> Vec<ArchiveSignal> {
    for bytes in from.nest.post_bytes() {
        to.nest.plant_post(bytes);
    }
    let mut out = Vec::new();
    for (addressee, signal) in from.wire.take() {
        assert_eq!(addressee, to.hex, "{signal:?}");
        let bytes = fauna_cbor::encode_canonical(&signal).unwrap();
        assert!(
            to.links.ingest(&from.hex, &bytes).await,
            "captured: {signal:?}"
        );
        out.push(signal);
    }
    out
}

fn links_state(side: &Side) -> LinksState {
    fauna_cbor::decode_strict(
        &side
            .nest
            .file(FOLDER, LINKS_PATH)
            .expect("links.cbor written"),
    )
    .unwrap()
}

fn pair_state(side: &Side, contact: &Side) -> Option<PairState> {
    side.nest
        .file(FOLDER, LINKS_PATH)
        .map(|b| fauna_cbor::decode_strict::<LinksState>(&b).unwrap())
        .and_then(|s| s.pair(&contact.hex, &FB).map(|p| p.state.clone()))
}

/// Announce → Bob asked → Bob yes (offer) → Alice asked → Alice yes
/// (confirm): both linked.
async fn link(alice: &Side, bob: &Side) {
    alice.links.announce_round(&FB).await.unwrap();
    deliver(alice, bob).await;
    assert_eq!(pair_state(bob, alice), Some(PairState::Asked));
    bob.links.answer_link(&alice.hex, &FB, true).await.unwrap();
    assert_eq!(pair_state(bob, alice), Some(PairState::Offered));
    deliver(bob, alice).await;
    assert_eq!(pair_state(alice, bob), Some(PairState::Asked));
    alice.links.answer_link(&bob.hex, &FB, true).await.unwrap();
    assert_eq!(pair_state(alice, bob), Some(PairState::Linked));
    deliver(alice, bob).await;
    assert_eq!(pair_state(bob, alice), Some(PairState::Linked));
    // The confirm and Alice's index left Bob work owed: his own index and
    // his claim run, which the wiring's drive does.
    assert!(bob.links.work_due());
    bob.links.drive().await.unwrap();
    assert!(!bob.links.work_due());
}

// ── the fingerprint ────────────────────────────────────────────────────────

#[test]
fn the_fingerprint_is_golden() {
    let by_name = fingerprint(&FB, &person("Bob Builder"));
    assert_eq!(
        hex::encode(by_name),
        "b9935d8c56235c39928dc0bf2ca736bf1d915630ffc9ffc984554c5fda70c84b",
        "the name-keyed fingerprint changed — every older build's Announce stops matching"
    );
    let by_id = fingerprint(
        &FB,
        &ExternalActorRef::new(FB, Some("100001".into()), "Bob Builder"),
    );
    assert_eq!(
        hex::encode(by_id),
        "4ab9fb721ec7378dcdca9328dd325b4c2f710861b3a10737a06e950571eb78a1"
    );
    assert_ne!(
        fingerprint(&Platform::Instagram, &person("Bob Builder")),
        by_name
    );
}

#[test]
fn a_friend_with_an_id_matches_an_announce_keyed_on_either() {
    let friend = ExternalActorRef::new(FB, Some("100001".into()), "Bob Builder");
    assert!(fingerprint_matches(
        &FB,
        &friend,
        &fingerprint(&FB, &friend)
    ));
    assert!(fingerprint_matches(
        &FB,
        &friend,
        &fingerprint(&FB, &person("Bob Builder"))
    ));
    assert!(!fingerprint_matches(
        &FB,
        &friend,
        &fingerprint(&FB, &person("Rob Builder"))
    ));
}

/// A Facebook export carries the owner's `username` but names friends by
/// display name alone, so the common pair is an id-bearing announcer and a
/// name-only friend ref: the Announce must still hit it.
#[tokio::test]
async fn an_owner_with_a_platform_id_is_found_by_a_friend_ref_carrying_only_a_name() {
    let (alice, bob) = alice_and_bob();
    let with_id = ExternalActorRef::new(FB, Some("alice.fixture".into()), "Alice Archive");
    alice
        .nest
        .seed_folder(FOLDER, &marker(&with_id, 10), &import_state(vec![]));
    alice.links.announce_round(&FB).await.unwrap();
    assert_eq!(bob.links.captures(), 0);
    deliver(&alice, &bob).await;
    assert_eq!(pair_state(&bob, &alice), Some(PairState::Asked));
    // The question arriving is a capture the app repaints the row on.
    assert_eq!(bob.links.captures(), 1);
}

// ── the folder union ───────────────────────────────────────────────────────

fn bare_pair(contact: &str, state: PairState, at: u64) -> PairLink {
    new_pair(contact, &FB, state, None, false, Timestamp(at))
}

#[test]
fn the_union_keeps_the_newest_pair_and_honours_a_tombstone() {
    let newest = LinksState {
        pairs: vec![bare_pair("aa", PairState::Linked, 5)],
        ..Default::default()
    };
    let older = LinksState {
        pairs: vec![
            bare_pair("aa", PairState::Unlinked, 9),
            bare_pair("bb", PairState::Declined, 1),
        ],
        ..Default::default()
    };
    let u = LinksState::union(vec![newest, older]);
    assert_eq!(
        u.pair("aa", &FB).unwrap().state,
        PairState::Unlinked,
        "the newer tombstone wins"
    );
    assert_eq!(
        u.pair("bb", &FB).unwrap().state,
        PairState::Declined,
        "an older folder's pair survives"
    );
}

#[test]
fn the_union_keeps_a_claimed_verdict_over_a_newer_held_one() {
    let id = ext(EntityKind::Comment, "c");
    let claimed = ClaimEntry::Claimed {
        post_id: "p".into(),
        target_post_id: "t".into(),
        media_held: 0,
    };
    let mut a = LinksState::default();
    a.record_claim("aa", &FB, &id, ClaimEntry::HeldUnreadable, Timestamp(9));
    let mut b = LinksState::default();
    b.record_claim("aa", &FB, &id, claimed.clone(), Timestamp(1));
    assert_eq!(
        LinksState::union(vec![a, b]).claim("aa", &FB, &id),
        Some(&claimed)
    );
}

#[tokio::test]
async fn links_are_written_into_the_newest_folder_and_read_across_all() {
    let (alice, bob) = alice_and_bob();
    link(&alice, &bob).await;
    // A newer export lands in a second folder: the next write goes there,
    // carrying the union, and the first folder's state still counts.
    alice.nest.seed_folder(
        "Facebook archive (2)",
        &marker(&alice.me, 20),
        &import_state(vec![]),
    );
    alice.links.unlink(&bob.hex, &FB).await.unwrap();
    let newest: LinksState =
        fauna_cbor::decode_strict(&alice.nest.file("Facebook archive (2)", LINKS_PATH).unwrap())
            .unwrap();
    assert_eq!(
        newest.pair(&bob.hex, &FB).unwrap().state,
        PairState::Unlinked
    );
    assert_eq!(
        links_state(&alice).pair(&bob.hex, &FB).unwrap().state,
        PairState::Linked,
        "the old folder is left as it was"
    );
    let views = alice.links.views().await.unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(
        views[0].state,
        ArchiveLinkState::Unlinked,
        "the tombstone wins the union"
    );
}

// ── the transitions ────────────────────────────────────────────────────────

#[tokio::test]
async fn announce_offer_confirm_links_both_sides_and_the_view_names_the_archived_friend() {
    let (alice, bob) = alice_and_bob();
    link(&alice, &bob).await;
    let view = bob.links.views().await.unwrap().remove(0);
    assert_eq!(view.contact, alice.hex);
    assert_eq!(view.state, ArchiveLinkState::Linked);
    assert_eq!(view.platform, "facebook");
    assert_eq!(view.archived_name, "Alice Archive");
    // Nothing is owed any more: the offer was answered.
    assert!(
        links_state(&bob).pending.is_empty(),
        "{:?}",
        links_state(&bob).pending
    );
}

#[tokio::test]
async fn an_announce_matching_no_friend_asks_nothing() {
    let (alice, bob) = alice_and_bob();
    write_model(
        &bob.nest,
        FOLDER,
        &Category::Friends,
        vec![friend("Someone Else")],
    );
    alice.links.announce_round(&FB).await.unwrap();
    deliver(&alice, &bob).await;
    assert_eq!(pair_state(&bob, &alice), None);
}

#[tokio::test]
async fn the_question_is_asked_once_and_a_no_is_final_on_both_sides() {
    let (alice, bob) = alice_and_bob();
    alice.links.announce_round(&FB).await.unwrap();
    deliver(&alice, &bob).await;
    bob.links.answer_link(&alice.hex, &FB, false).await.unwrap();
    assert_eq!(pair_state(&bob, &alice), Some(PairState::Declined));
    deliver(&bob, &alice).await;
    assert_eq!(
        pair_state(&alice, &bob),
        Some(PairState::Declined),
        "a decline to an announce settles the pair on the announcer's side too"
    );

    // A re-import announces again: Bob is not asked a second time.
    alice.links.announce_round(&FB).await.unwrap();
    deliver(&alice, &bob).await;
    assert_eq!(pair_state(&bob, &alice), Some(PairState::Declined));
    assert_eq!(
        bob.links.answer_link(&alice.hex, &FB, true).await,
        Err(LinkError::NotAsked)
    );
}

#[tokio::test]
async fn a_declined_announce_is_never_asked_of_the_announcer_after_a_re_import() {
    let (alice, bob) = alice_and_bob();
    alice.links.announce_round(&FB).await.unwrap();
    deliver(&alice, &bob).await;
    bob.links.answer_link(&alice.hex, &FB, false).await.unwrap();
    deliver(&bob, &alice).await;

    // Bob imports again: his announce reaches Alice, who must not be asked a
    // question Bob already answered no to (outcome 51).
    bob.links.announce_round(&FB).await.unwrap();
    deliver(&bob, &alice).await;
    assert_eq!(pair_state(&alice, &bob), Some(PairState::Declined));
    assert_eq!(
        alice.links.answer_link(&bob.hex, &FB, true).await,
        Err(LinkError::NotAsked)
    );
    alice.links.drive().await.unwrap();
    assert!(
        alice.wire.take().is_empty(),
        "nothing is owed once declined"
    );
}

#[tokio::test]
async fn a_declined_offer_is_never_offered_again_and_a_redriven_offer_is_declined_again() {
    let (alice, bob) = alice_and_bob();
    alice.links.announce_round(&FB).await.unwrap();
    deliver(&alice, &bob).await;
    bob.links.answer_link(&alice.hex, &FB, true).await.unwrap();
    deliver(&bob, &alice).await;
    alice.links.answer_link(&bob.hex, &FB, false).await.unwrap();
    assert_eq!(pair_state(&alice, &bob), Some(PairState::Declined));
    // The decline is lost on the way: Bob's offer stays pending and is
    // re-driven next session; Alice answers it with a decline again.
    alice.wire.take();
    bob.links.drive().await.unwrap();
    let redriven = bob.wire.take();
    assert!(
        matches!(redriven[..], [(_, ArchiveSignal::LinkOffer { .. })]),
        "{redriven:?}"
    );
    let bytes = fauna_cbor::encode_canonical(&redriven[0].1).unwrap();
    assert!(alice.links.ingest(&bob.hex, &bytes).await);
    alice.links.drive().await.unwrap();
    deliver(&alice, &bob).await;
    assert_eq!(pair_state(&bob, &alice), Some(PairState::Declined));
    assert_eq!(pair_state(&alice, &bob), Some(PairState::Declined));
    bob.links.drive().await.unwrap();
    assert!(bob.wire.take().is_empty(), "nothing is owed once declined");
}

#[tokio::test]
async fn a_link_offer_naming_no_friend_is_never_shown() {
    let (alice, bob) = alice_and_bob();
    let offer = ArchiveSignal::LinkOffer {
        platform: FB,
        me: person("Mallory Impostor"),
    };
    assert!(
        alice
            .links
            .ingest(&bob.hex, &fauna_cbor::encode_canonical(&offer).unwrap())
            .await
    );
    assert_eq!(pair_state(&alice, &bob), None);
    assert_eq!(alice.links.tally().offer_not_a_friend, 1);
}

#[tokio::test]
async fn unlink_tombstones_the_pair_and_drops_the_stored_index() {
    let (alice, bob) = alice_and_bob();
    link(&alice, &bob).await;
    deliver(&alice, &bob).await;
    deliver(&bob, &alice).await;
    assert!(
        links_state(&bob)
            .pair(&alice.hex, &FB)
            .unwrap()
            .origin_index
            .is_some()
    );
    alice.links.unlink(&bob.hex, &FB).await.unwrap();
    deliver(&alice, &bob).await;
    let pair = links_state(&bob).pair(&alice.hex, &FB).unwrap().clone();
    assert_eq!(pair.state, PairState::Unlinked);
    assert!(pair.origin_index.is_none());
    assert_eq!(pair_state(&alice, &bob), Some(PairState::Unlinked));
}

#[tokio::test]
async fn a_sender_the_roster_does_not_list_as_accepted_is_dropped_and_tallied() {
    let (alice, bob) = alice_and_bob();
    alice.nest.set_contact(&bob.hex, "pending");
    let announce = ArchiveSignal::Announce {
        platform: FB,
        fingerprint: fingerprint(&FB, &bob.me),
        name_fingerprint: None,
    };
    assert!(
        !alice
            .links
            .ingest(&bob.hex, &fauna_cbor::encode_canonical(&announce).unwrap())
            .await
    );
    assert_eq!(alice.links.tally().not_a_contact, 1);
    assert!(alice.nest.file(FOLDER, LINKS_PATH).is_none());
}

#[tokio::test]
async fn a_session_with_no_archive_of_the_platform_answers_and_stores_nothing() {
    let (alice, bob) = alice_and_bob();
    bob.nest.clear_folders();
    let announce = ArchiveSignal::Announce {
        platform: FB,
        fingerprint: fingerprint(&FB, &alice.me),
        name_fingerprint: None,
    };
    assert!(
        bob.links
            .ingest(
                &alice.hex,
                &fauna_cbor::encode_canonical(&announce).unwrap()
            )
            .await
    );
    assert!(bob.nest.files_in(FOLDER).is_empty());
}

#[tokio::test]
async fn a_newer_builds_signal_is_ignored_and_the_state_untouched() {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "snake_case")]
    enum NewerSignal {
        PhotoTag {
            platform: Platform,
            tagged: Vec<String>,
        },
    }
    let (alice, bob) = alice_and_bob();
    link(&alice, &bob).await;
    let before = alice.nest.file(FOLDER, LINKS_PATH);
    let newer = NewerSignal::PhotoTag {
        platform: FB,
        tagged: vec!["x".into()],
    };
    let bytes = fauna_cbor::encode_canonical(&newer).unwrap();
    assert!(matches!(
        fauna_cbor::decode_strict::<ArchiveSignal>(&bytes).unwrap(),
        ArchiveSignal::Unknown(_)
    ));
    assert!(alice.links.ingest(&bob.hex, &bytes).await);
    assert_eq!(alice.nest.file(FOLDER, LINKS_PATH), before);
    assert_eq!(alice.links.tally().unknown, 1);
}

#[tokio::test]
async fn a_newer_builds_links_state_decodes_is_left_alone_and_is_written_back_as_read() {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "snake_case")]
    enum NewerPairState {
        Merging { pass: u64 },
    }
    #[derive(serde::Serialize)]
    #[serde(rename_all = "snake_case")]
    enum NewerClaim {
        ClaimedTwice { post_ids: Vec<String> },
    }
    #[derive(serde::Serialize)]
    struct NewerPair {
        contact: String,
        platform: Platform,
        state: NewerPairState,
        updated_at: Timestamp,
    }
    #[derive(serde::Serialize)]
    struct NewerRecord {
        contact: String,
        platform: Platform,
        external_id: ExternalId,
        entry: NewerClaim,
        at: Timestamp,
    }
    #[derive(serde::Serialize)]
    struct NewerLinks {
        pairs: Vec<NewerPair>,
        claims: Vec<NewerRecord>,
        updated_at: Timestamp,
        photo_tags: Vec<String>,
    }
    let (alice, bob) = alice_and_bob();
    let newer = NewerLinks {
        pairs: vec![NewerPair {
            contact: bob.hex.clone(),
            platform: FB,
            state: NewerPairState::Merging { pass: 2 },
            updated_at: Timestamp(5),
        }],
        claims: vec![NewerRecord {
            contact: bob.hex.clone(),
            platform: FB,
            external_id: ext(EntityKind::Comment, "c"),
            entry: NewerClaim::ClaimedTwice {
                post_ids: vec!["p".into()],
            },
            at: Timestamp(5),
        }],
        updated_at: Timestamp(5),
        photo_tags: vec!["t".into()],
    };
    alice.nest.put_file(
        FOLDER,
        LINKS_PATH,
        fauna_cbor::encode_canonical(&newer).unwrap(),
    );
    let state = links_state(&alice);
    assert!(matches!(state.pairs[0].state, PairState::Unknown(_)));
    assert!(matches!(state.claims[0].entry, ClaimEntry::Unknown(_)));
    assert!(state.extra.contains_key("photo_tags"));

    // No transition touches the pair it cannot read.
    let unlink = fauna_cbor::encode_canonical(&ArchiveSignal::Unlink { platform: FB }).unwrap();
    assert!(alice.links.ingest(&bob.hex, &unlink).await);
    assert!(matches!(
        links_state(&alice).pairs[0].state,
        PairState::Unknown(_)
    ));
    assert!(
        alice.links.views().await.unwrap().is_empty(),
        "the view omits a pair it cannot name"
    );

    // A write for another reason carries every unread value back.
    alice.links.announce_round(&FB).await.unwrap();
    let after = links_state(&alice);
    assert_eq!(after.pairs[0].state, state.pairs[0].state);
    assert_eq!(after.claims, state.claims);
    assert_eq!(after.extra, state.extra);
}

// ── the re-drive ───────────────────────────────────────────────────────────

#[tokio::test]
async fn the_redrive_resends_exactly_the_pending_signals() {
    let (alice, bob) = alice_and_bob();
    bob.wire.set_down(true);
    alice.links.announce_round(&FB).await.unwrap();
    deliver(&alice, &bob).await;
    bob.links.answer_link(&alice.hex, &FB, true).await.unwrap();
    assert!(bob.wire.take().is_empty());
    let pending = links_state(&bob).pending;
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert!(!pending[0].sent);

    bob.wire.set_down(false);
    bob.links.drive().await.unwrap();
    let sent = bob.wire.take();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1, pending[0].signal);
    // Sent, still awaiting its answer: re-driven next session too.
    assert!(links_state(&bob).pending[0].sent);
    bob.links.drive().await.unwrap();
    assert_eq!(bob.wire.take().len(), 1);
    // Answered: no longer pending.
    let bytes = fauna_cbor::encode_canonical(&sent[0].1).unwrap();
    assert!(alice.links.ingest(&bob.hex, &bytes).await);
    alice.links.answer_link(&bob.hex, &FB, true).await.unwrap();
    deliver(&alice, &bob).await;
    bob.wire.take();
    assert!(
        links_state(&bob).pending.is_empty(),
        "{:?}",
        links_state(&bob).pending
    );
}

// ── the origin index ───────────────────────────────────────────────────────

fn sent_index(side: &Side) -> Vec<OriginEntry> {
    side.wire
        .take()
        .into_iter()
        .find_map(|(_, s)| match s {
            ArchiveSignal::OriginIndex { entries, .. } => Some(entries),
            _ => None,
        })
        .expect("an OriginIndex was sent")
}

#[tokio::test]
async fn the_index_carries_public_posts_and_followers_posts_only_for_a_subscriber() {
    let (alice, bob) = alice_and_bob();
    link(&alice, &bob).await;
    let index = links_state(&bob)
        .pair(&alice.hex, &FB)
        .unwrap()
        .origin_index
        .clone()
        .unwrap();
    let ids: Vec<&str> = index.iter().map(|e| e.external_id.id.as_str()).collect();
    assert_eq!(
        ids,
        ["p-public"],
        "no followers post for a non-subscriber, never an only-me one"
    );
    assert!(index[0].public);
    assert_eq!(index[0].url.as_deref(), Some(URL_PUBLIC));
    assert_eq!(index[0].post_id, post_id_of("p-public"));

    alice.nest.add_followers_subscriber(&bob.hex);
    alice.links.announce_round(&FB).await.unwrap();
    let mut index = sent_index(&alice);
    index.sort_by(|a, b| a.external_id.id.cmp(&b.external_id.id));
    let got: Vec<(&str, bool)> = index
        .iter()
        .map(|e| (e.external_id.id.as_str(), e.public))
        .collect();
    assert_eq!(got, [("p-friends", false), ("p-public", true)]);
}

// ── the claims ─────────────────────────────────────────────────────────────

fn bob_archive(bob: &Side, comments: Vec<Entity>, reactions: Vec<Entity>) {
    write_model(&bob.nest, FOLDER, &Category::Comments, comments);
    write_model(&bob.nest, FOLDER, &Category::Reactions, reactions);
}

fn claim_of<'a>(
    state: &'a LinksState,
    alice: &Side,
    id: &str,
    kind: EntityKind,
) -> Option<&'a ClaimEntry> {
    state.claim(&alice.hex, &FB, &ext(kind, id))
}

#[tokio::test]
async fn each_ladder_rung_places_a_reply_and_no_rung_records_unmatched_without_a_post() {
    let (alice, bob) = alice_and_bob();
    bob_archive(
        &bob,
        vec![
            comment(
                "by-url",
                "Bob Builder",
                target("Alice Archive", None, Some(URL_PUBLIC)),
                Some("lovely"),
                0,
            ),
            comment(
                "by-id",
                "Bob Builder",
                target(
                    "Alice Archive",
                    Some(ext(EntityKind::Post, "p-public")),
                    Some("https://elsewhere"),
                ),
                Some("again"),
                0,
            ),
            comment(
                "nowhere",
                "Bob Builder",
                target("Alice Archive", None, Some("https://unknown")),
                Some("lost"),
                0,
            ),
            comment(
                "no-url",
                "Bob Builder",
                target("Alice Archive", None, None),
                Some("lost too"),
                0,
            ),
            // Not Bob's own words, and not on Alice's posts: never claimed.
            comment(
                "carols",
                "Carol Third",
                target("Alice Archive", None, Some(URL_PUBLIC)),
                Some("hi"),
                0,
            ),
            comment(
                "on-dave",
                "Bob Builder",
                target("Dave Other", None, Some(URL_PUBLIC)),
                Some("hi"),
                0,
            ),
        ],
        vec![],
    );
    link(&alice, &bob).await;
    deliver(&alice, &bob).await;
    let state = links_state(&bob);
    let posts = bob.nest.posts();
    assert_eq!(
        posts.len(),
        2,
        "one reply per placed comment, none for the unmatched"
    );
    for id in ["by-url", "by-id"] {
        let Some(ClaimEntry::Claimed { target_post_id, .. }) =
            claim_of(&state, &alice, id, EntityKind::Comment)
        else {
            panic!("{id} not claimed: {:?}", state.claims);
        };
        assert_eq!(target_post_id, &post_id_of("p-public"));
    }
    for id in ["nowhere", "no-url"] {
        assert_eq!(
            claim_of(&state, &alice, id, EntityKind::Comment),
            Some(&ClaimEntry::Unmatched {
                reason: UNMATCHED_NO_RUNG.into()
            })
        );
    }
    assert!(claim_of(&state, &alice, "carols", EntityKind::Comment).is_none());
    assert!(claim_of(&state, &alice, "on-dave", EntityKind::Comment).is_none());
    let reply = &posts[0];
    assert_eq!(
        reply.created_at.0, 1_600_000_000_000_000,
        "backdated to the comment"
    );
    assert_eq!(reply.origin.as_ref().unwrap().platform, "facebook");
    assert!(reply.gated.is_none(), "public under a public target");
    let want = fauna_core::hex32::decode(&post_id_of("p-public")).unwrap();
    assert!(
        matches!(&reply.references[..], [Reference::Reply { post_id }] if post_id.digest() == want)
    );
    assert_eq!(
        bob.links.counts().await.unwrap(),
        LinkCounts {
            claimed: 2,
            unmatched: 2
        }
    );
}

// ── rung (iii): the shared link ────────────────────────────────────────────

const SHARED: &str = "https://example.invalid/post/1";

fn entry(id: &str, at: u64, url: Option<&str>, links: &[&str]) -> OriginEntry {
    OriginEntry {
        external_id: ext(EntityKind::Post, id),
        url: url.map(str::to_string),
        links: links.iter().map(|l| l.to_string()).collect(),
        created_at: Timestamp(at),
        post_id: post_id_of(id),
        public: true,
    }
}

fn hit_id(r: Resolved<'_>) -> Option<&str> {
    r.hit().map(|e| e.external_id.id.as_str())
}

#[test]
fn the_shared_link_rung_places_on_the_one_older_post_sharing_it() {
    let index = [
        entry("link", 10, None, &[SHARED]),
        entry("other", 10, None, &[]),
    ];
    let t = target("Alice Archive", None, Some(SHARED));
    assert_eq!(hit_id(resolve(&t, Timestamp(20), &index)), Some("link"));
    // Verbatim: a trailing slash is another link.
    let t2 = target(
        "Alice Archive",
        None,
        Some("https://example.invalid/post/1/"),
    );
    assert_eq!(resolve(&t2, Timestamp(20), &index), Resolved::None);
}

#[test]
fn two_posts_sharing_the_link_are_ambiguous_and_never_placed() {
    let index = [
        entry("a", 10, None, &[SHARED]),
        entry("b", 11, None, &[SHARED]),
    ];
    let r = resolve(
        &target("Alice Archive", None, Some(SHARED)),
        Timestamp(20),
        &index,
    );
    assert_eq!(r, Resolved::Ambiguous);
    assert_eq!(r.unmatched_reason(), Some(UNMATCHED_AMBIGUOUS_LINK));
}

#[test]
fn a_post_newer_than_the_item_is_not_its_target() {
    // The newer sharer drops out, so the one older post still places.
    let index = [
        entry("old", 10, None, &[SHARED]),
        entry("new", 30, None, &[SHARED]),
    ];
    let t = target("Alice Archive", None, Some(SHARED));
    assert_eq!(hit_id(resolve(&t, Timestamp(20), &index)), Some("old"));
    // Only a newer sharer: nothing fires.
    assert_eq!(
        resolve(&t, Timestamp(5), &index).unmatched_reason(),
        Some(UNMATCHED_NO_RUNG)
    );
}

#[test]
fn the_permalink_rung_wins_over_the_shared_link() {
    let index = [
        entry("sharer", 10, None, &[SHARED]),
        entry("permalink", 10, Some(SHARED), &[]),
    ];
    let t = target("Alice Archive", None, Some(SHARED));
    assert_eq!(
        hit_id(resolve(&t, Timestamp(20), &index)),
        Some("permalink")
    );
}

#[tokio::test]
async fn a_comment_on_a_link_post_is_claimed_under_it_by_the_shared_link() {
    let (alice, bob) = alice_and_bob();
    // Alice's link post: no permalink (as every Facebook post), one shared link.
    let mut link_post = a_post("p-link", ArchiveAudience::Public, "unused");
    link_post.url = None;
    link_post.links = vec![SHARED.to_string()];
    let mut posts: Vec<Entity> = fauna_cbor::decode_strict(
        &alice
            .nest
            .file(FOLDER, &model_path(&Category::Posts))
            .unwrap(),
    )
    .unwrap();
    posts.push(Entity::Post(link_post.clone()));
    write_model(&alice.nest, FOLDER, &Category::Posts, posts);
    let mut state = alice.nest.read_state(FOLDER);
    state.imported.push(ImportedRecord {
        external_id: link_post.external_id.clone(),
        target: ImportTarget::Post {
            post_id: post_id_of("p-link"),
        },
    });
    alice.nest.put_file(
        FOLDER,
        crate::state::STATE_PATH,
        fauna_cbor::encode_canonical(&state).unwrap(),
    );

    bob_archive(
        &bob,
        vec![comment(
            "on-link",
            "Bob Builder",
            target("Alice Archive", None, Some(SHARED)),
            Some("nice one"),
            0,
        )],
        vec![],
    );
    link(&alice, &bob).await;
    deliver(&alice, &bob).await;
    let Some(ClaimEntry::Claimed { target_post_id, .. }) =
        claim_of(&links_state(&bob), &alice, "on-link", EntityKind::Comment).cloned()
    else {
        panic!("not claimed: {:?}", links_state(&bob).claims);
    };
    assert_eq!(target_post_id, post_id_of("p-link"));
}

#[tokio::test]
async fn a_reply_under_a_followers_post_seals_to_the_claimants_tier_or_is_held() {
    let (alice, bob) = alice_and_bob();
    alice.nest.add_followers_subscriber(&bob.hex);
    bob_archive(
        &bob,
        vec![comment(
            "gated",
            "Bob Builder",
            target("Alice Archive", None, Some(URL_FRIENDS)),
            Some("private words"),
            0,
        )],
        vec![],
    );
    link(&alice, &bob).await;
    deliver(&alice, &bob).await;
    // Alice cannot open Bob's followers tier: held, nothing posted.
    assert_eq!(
        claim_of(&links_state(&bob), &alice, "gated", EntityKind::Comment),
        Some(&ClaimEntry::HeldUnreadable)
    );
    assert!(bob.nest.posts().is_empty());

    // Once she can, the next round seals it to Bob's own followers tier.
    bob.nest.add_followers_subscriber(&alice.hex);
    alice.links.announce_round(&FB).await.unwrap();
    deliver(&alice, &bob).await;
    bob.links.drive().await.unwrap();
    let posts = bob.nest.posts();
    assert_eq!(posts.len(), 1);
    let gated = posts[0].gated.as_ref().expect("sealed");
    assert_eq!(gated.tier, fauna_core::subscription::FOLLOWERS_TIER);
    assert!(
        !posts[0].body_text().contains("private words"),
        "the words are sealed"
    );
    assert!(matches!(
        claim_of(&links_state(&bob), &alice, "gated", EntityKind::Comment),
        Some(ClaimEntry::Claimed { .. })
    ));
    assert!(links_state(&bob).in_flight.is_none());
}

#[tokio::test]
async fn a_media_only_comment_is_held_and_words_with_media_are_claimed_as_words() {
    let (alice, bob) = alice_and_bob();
    bob_archive(
        &bob,
        vec![
            comment(
                "pic",
                "Bob Builder",
                target("Alice Archive", None, Some(URL_PUBLIC)),
                None,
                1,
            ),
            comment(
                "pic-words",
                "Bob Builder",
                target("Alice Archive", None, Some(URL_PUBLIC)),
                Some("look"),
                2,
            ),
        ],
        vec![],
    );
    link(&alice, &bob).await;
    deliver(&alice, &bob).await;
    let state = links_state(&bob);
    assert_eq!(
        claim_of(&state, &alice, "pic", EntityKind::Comment),
        Some(&ClaimEntry::HeldMedia)
    );
    assert!(matches!(
        claim_of(&state, &alice, "pic-words", EntityKind::Comment),
        Some(ClaimEntry::Claimed { media_held: 2, .. })
    ));
    assert_eq!(bob.nest.posts().len(), 1);
}

#[test]
fn the_emoji_map_is_ruling_sevens() {
    for (kind, want) in [
        (ReactionKind::Like, Some(MappedReaction::Upvote)),
        (ReactionKind::Love, Some(MappedReaction::React("❤️"))),
        (ReactionKind::Care, Some(MappedReaction::React("🤗"))),
        (ReactionKind::Haha, Some(MappedReaction::React("😆"))),
        (ReactionKind::Wow, Some(MappedReaction::React("😮"))),
        (ReactionKind::Sad, Some(MappedReaction::React("😢"))),
        (ReactionKind::Angry, Some(MappedReaction::React("😡"))),
        (
            ReactionKind::Other {
                raw: "PRIDE".into(),
            },
            None,
        ),
    ] {
        assert_eq!(reaction_emoji(&kind), want, "{kind:?}");
    }
}

#[tokio::test]
async fn reactions_become_wordless_public_reaction_posts_and_an_unmapped_kind_is_unmatched() {
    let (alice, bob) = alice_and_bob();
    alice.nest.add_followers_subscriber(&bob.hex);
    let on = |url| target("Alice Archive", None, Some(url));
    bob_archive(
        &bob,
        vec![],
        vec![
            reaction("like", "Bob Builder", on(URL_PUBLIC), ReactionKind::Like),
            reaction("love", "Bob Builder", on(URL_FRIENDS), ReactionKind::Love),
            reaction(
                "odd",
                "Bob Builder",
                on(URL_PUBLIC),
                ReactionKind::Other {
                    raw: "PRIDE".into(),
                },
            ),
        ],
    );
    link(&alice, &bob).await;
    deliver(&alice, &bob).await;
    let posts = bob.nest.posts();
    assert_eq!(posts.len(), 2);
    assert!(
        posts
            .iter()
            .all(|p| p.gated.is_none() && p.body_text().is_empty()),
        "wordless, so public"
    );
    assert!(
        posts
            .iter()
            .any(|p| matches!(p.references[..], [Reference::Upvote { .. }]))
    );
    assert!(
        posts.iter().any(
            |p| matches!(&p.references[..], [Reference::React { emoji, .. }] if emoji == "❤️")
        )
    );
    assert_eq!(
        claim_of(&links_state(&bob), &alice, "odd", EntityKind::Reaction),
        Some(&ClaimEntry::Unmatched {
            reason: UNMATCHED_REACTION_KIND.into()
        })
    );
}

#[tokio::test]
async fn claimed_items_are_verified_recorded_and_hide_the_private_copy() {
    let (alice, bob) = alice_and_bob();
    bob_archive(
        &bob,
        vec![comment(
            "c1",
            "Bob Builder",
            target("Alice Archive", None, Some(URL_PUBLIC)),
            Some("lovely"),
            0,
        )],
        vec![],
    );
    link(&alice, &bob).await;
    deliver(&alice, &bob).await;
    let signals = deliver(&bob, &alice).await;
    assert!(
        signals
            .iter()
            .any(|s| matches!(s, ArchiveSignal::Claimed { items, .. } if items.len() == 1)),
        "{signals:?}"
    );
    let hidden = alice.links.hidden_comment_ids(&FB).await.unwrap();
    assert_eq!(
        hidden.into_iter().collect::<Vec<_>>(),
        [ext(EntityKind::Comment, "c1")]
    );
    assert_eq!(links_state(&alice).received.len(), 1);
}

#[tokio::test]
async fn a_claim_whose_post_is_not_the_senders_is_refused() {
    let (alice, bob) = alice_and_bob();
    link(&alice, &bob).await;
    // Mallory's reply to Alice's post, named by Bob as his own.
    let mallory = ActorKeypair::from_secret([9; 32]);
    let target = target_cid(&post_id_of("p-public")).unwrap();
    let bytes = build_referencing_post_at(
        &mallory,
        "not bob",
        &[],
        target,
        ReferencedAudience::Public,
        ReferenceKind::Reply,
        &PostAuthoring::now(),
    )
    .unwrap();
    let post_id = alice.nest.plant_post(bytes);
    let claimed = ArchiveSignal::Claimed {
        platform: FB,
        items: vec![ClaimedItem {
            external_id: ext(EntityKind::Comment, "x"),
            post_id,
            target_post_id: post_id_of("p-public"),
        }],
    };
    assert!(
        alice
            .links
            .ingest(&bob.hex, &fauna_cbor::encode_canonical(&claimed).unwrap())
            .await
    );
    assert!(links_state(&alice).received.is_empty());
    assert_eq!(alice.links.tally().claim_refused, 1);

    // Bob's own post naming a post that is not Alice's is refused too.
    let kp_bob = ActorKeypair::from_secret([2; 32]);
    let elsewhere = post_id_of("not-alices");
    let bytes = build_referencing_post_at(
        &kp_bob,
        "hm",
        &[],
        target_cid(&elsewhere).unwrap(),
        ReferencedAudience::Public,
        ReferenceKind::Reply,
        &PostAuthoring::now(),
    )
    .unwrap();
    let post_id = alice.nest.plant_post(bytes);
    let claimed = ArchiveSignal::Claimed {
        platform: FB,
        items: vec![ClaimedItem {
            external_id: ext(EntityKind::Comment, "y"),
            post_id,
            target_post_id: elsewhere,
        }],
    };
    assert!(
        alice
            .links
            .ingest(&bob.hex, &fauna_cbor::encode_canonical(&claimed).unwrap())
            .await
    );
    assert!(links_state(&alice).received.is_empty());
}

#[test]
fn the_ledger_refuses_a_second_claim_of_one_external_id() {
    let mut state = LinksState::default();
    let id = ext(EntityKind::Comment, "c");
    let first = ClaimEntry::Claimed {
        post_id: "p1".into(),
        target_post_id: "t".into(),
        media_held: 0,
    };
    assert!(state.record_claim(
        "aa",
        &FB,
        &id,
        ClaimEntry::Unmatched { reason: "r".into() },
        Timestamp(1)
    ));
    assert!(
        state.record_claim("aa", &FB, &id, first.clone(), Timestamp(2)),
        "an unmatched record may still be claimed"
    );
    let second = ClaimEntry::Claimed {
        post_id: "p2".into(),
        target_post_id: "t".into(),
        media_held: 0,
    };
    assert!(!state.record_claim("aa", &FB, &id, second, Timestamp(3)));
    assert_eq!(state.claim("aa", &FB, &id), Some(&first));
    assert_eq!(state.claims.len(), 1);
}

#[tokio::test]
async fn a_reimport_adds_only_what_is_new_and_claims_nothing_twice() {
    let (alice, bob) = alice_and_bob();
    let first = comment(
        "c1",
        "Bob Builder",
        target("Alice Archive", None, Some(URL_PUBLIC)),
        Some("one"),
        0,
    );
    bob_archive(&bob, vec![first.clone()], vec![]);
    link(&alice, &bob).await;
    deliver(&alice, &bob).await;
    assert_eq!(bob.nest.posts().len(), 1);

    // Bob's newer export, in a second folder, repeats c1 and adds c2.
    bob.nest.seed_folder(
        "Facebook archive (2)",
        &marker(&bob.me, 20),
        &import_state(vec![]),
    );
    let second = comment(
        "c2",
        "Bob Builder",
        target("Alice Archive", None, Some(URL_PUBLIC)),
        Some("two"),
        0,
    );
    write_model(
        &bob.nest,
        "Facebook archive (2)",
        &Category::Comments,
        vec![first, second],
    );
    bob.links.announce_round(&FB).await.unwrap();
    assert_eq!(bob.nest.posts().len(), 2, "c2 added, c1 not again");
    assert_eq!(bob.links.counts().await.unwrap().claimed, 2);
}

#[tokio::test]
async fn the_done_step_counts_reach_the_snapshot() {
    use crate::nest::fakes::MemoryOpener;
    let machine = crate::ArchiveImportMachine::new(
        Arc::new(FakeNest::new()),
        Arc::new(MemoryOpener::default()),
        ActorKeypair::from_secret([3; 32]),
    );
    machine.set_link_counts(LinkCounts {
        claimed: 4,
        unmatched: 1,
    });
    let snap = machine.snapshot();
    assert_eq!((snap.link_claimed, snap.link_unmatched), (4, 1));
}
