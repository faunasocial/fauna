//! Per-category goldens over the generated `facebook-json` fixture
//! (`archive-import.md` § Testing, tier_1). IDs pinned here are FROZEN:
//! a change means every previously imported archive would re-import.

mod common;

use fauna_archive::EntityError;
use fauna_archive::facebook::FacebookParser;
use fauna_archive::facebook::json::MAX_IDENTITY_BYTES;
use fauna_archive::model::*;
use fauna_archive::parser::ArchiveParser;
use fauna_archive::reader::ArchiveReader;
use fauna_archive::source::VecSource;

const POSTS_MEMBER: &str =
    "your_facebook_activity/posts/your_posts__check_ins__photos_and_videos_1.json";

fn stream(fixture: &str, category: Category) -> (Vec<Entity>, Vec<EntityError>) {
    let src = common::fixture_source(fixture);
    let mut reader = ArchiveReader::open(&src).unwrap();
    let mut ok = Vec::new();
    let mut errs = Vec::new();
    for item in FacebookParser.stream(&mut reader, category) {
        match item {
            Ok(e) => ok.push(e),
            Err(e) => errs.push(e),
        }
    }
    (ok, errs)
}

fn posts() -> (Vec<ArchivePost>, Vec<EntityError>) {
    let (ok, errs) = stream("facebook-json", Category::Posts);
    let posts = ok
        .into_iter()
        .map(|e| match e {
            Entity::Post(p) => p,
            other => panic!("expected a post, got {other:?}"),
        })
        .collect();
    (posts, errs)
}

fn hex_of(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[test]
fn posts_stream_yields_three_posts_and_one_skip_in_file_order() {
    let (posts, errs) = posts();
    assert_eq!(posts.len(), 3);
    assert_eq!(errs.len(), 1);
    let skip = &errs[0];
    assert_eq!(skip.category, Category::Posts);
    assert_eq!(skip.member, POSTS_MEMBER);
    assert_eq!(skip.position, 3);
    assert!(skip.reason.contains("timestamp"), "{}", skip.reason);
    assert!(skip.to_string().contains(POSTS_MEMBER));
}

#[test]
fn first_post_is_backdated_friends_only_and_mojibake_repaired() {
    let (posts, _) = posts();
    let p = &posts[0];
    assert_eq!(p.text.as_deref(), Some("Dobrý den, přátelé"));
    assert_eq!(p.created_at, Timestamp(1_600_000_000_000_000));
    assert_eq!(p.audience, ArchiveAudience::Friends);
    assert!(p.media.is_empty() && p.links.is_empty() && p.tagged.is_empty());
    assert!(p.place.is_none() && p.album_name.is_none());
    assert!(p.external_id.is_derived());
    assert_eq!(p.external_id.kind, EntityKind::Post);
}

#[test]
fn photo_post_carries_media_place_link_tag_and_public_audience() {
    let (posts, _) = posts();
    let p = &posts[1];
    assert_eq!(p.audience, ArchiveAudience::Public);
    assert_eq!(p.text.as_deref(), Some("At the park"));
    assert_eq!(p.media.len(), 1);
    let m = &p.media[0];
    assert_eq!(
        m.path,
        "your_facebook_activity/posts/media/Timeline_photos/photo_1.jpg"
    );
    assert_eq!(m.size, Some(15));
    assert_eq!(
        m.blake3_hex.as_deref(),
        Some(hex_of(b"fixture-photo-1").as_str())
    );
    assert_eq!(m.mime.as_deref(), Some("image/jpeg"));
    assert_eq!(m.taken_at, Some(Timestamp(1_609_999_000_000_000)));
    assert_eq!(m.caption.as_deref(), Some("Photo caption"));
    assert_eq!(p.album_name.as_deref(), Some("Timeline photos"));
    assert_eq!(p.links, ["https://example.invalid/link"]);
    let place = p.place.as_ref().unwrap();
    assert_eq!(place.name, "Fixture Park");
    assert_eq!(place.address.as_deref(), Some("1 Fixture Way"));
    assert_eq!(place.latitude.as_deref(), Some("59.91"));
    assert_eq!(place.longitude.as_deref(), Some("10.75"));
    assert_eq!(place.url.as_deref(), Some("https://example.invalid/place"));
    assert_eq!(p.tagged.len(), 1);
    assert_eq!(p.tagged[0].display_name, "Friend One");
    assert_eq!(p.tagged[0].name_key, "friend one");
}

#[test]
fn only_me_post_maps_to_only_me_and_minimal_export_to_unknown() {
    let (posts, _) = posts();
    assert_eq!(posts[2].audience, ArchiveAudience::OnlyMe);
    let (ok, errs) = stream("facebook-minimal", Category::Posts);
    assert!(errs.is_empty());
    assert_eq!(ok.len(), 1);
    match &ok[0] {
        Entity::Post(p) => {
            assert_eq!(p.text.as_deref(), Some("Minimal post"));
            assert_eq!(p.audience, ArchiveAudience::Unknown);
        }
        other => panic!("{other:?}"),
    }
}

/// GOLDEN — frozen post IDs (text-only post, and a post whose ID folds in
/// its media creation instant — re-pinned 2026-09-08 with the instants rule,
/// before any real import existed). Never update to make a test pass.
#[test]
fn golden_post_ids_are_frozen() {
    let (posts, _) = posts();
    assert_eq!(
        posts[0].external_id.id,
        "h:7994c384e40458fbb61f4d8791446e63a5a4797785a7ac74a0c119637a87572b"
    );
    assert_eq!(
        posts[1].external_id.id,
        "h:8ef997204c050ffa2641e00e8fded09a299ad247fd1898fe3d72d98a16e9e7ee"
    );
    // Streaming again yields the same IDs (determinism, rule 4).
    let (again, _) = self::posts();
    assert_eq!(again[0].external_id, posts[0].external_id);
    assert_eq!(again[1].external_id, posts[1].external_id);
}

#[test]
fn mojibake_media_path_resolves_and_hashes() {
    // The directory is "Fotky_z_časové_osy" (č = C4 8D, é = C3 A9), escaped
    // byte-wise in the post's `uri` exactly as Facebook writes it.
    let profile = br#"{ "profile_v2": { "name": { "full_name": "Test Owner" } } }"#;
    let posts = br#"[ { "timestamp": 1600000000, "data": [ { "post": "photo" } ],
        "attachments": [ { "data": [ { "media": { "uri": "your_facebook_activity/posts/media/Fotky_z_\u00c4\u008dasov\u00c3\u00a9_osy/p.jpg", "creation_timestamp": 1600000000 } } ] } ] } ]"#;
    let src = common::zip_of(&[
        (
            "personal_information/profile_information/profile_information.json",
            profile,
        ),
        ("your_facebook_activity/posts/your_posts_1.json", posts),
        (
            "your_facebook_activity/posts/media/Fotky_z_časové_osy/p.jpg",
            b"fixture-photo-cz",
        ),
    ]);
    let mut reader = ArchiveReader::open(&src).unwrap();
    let items: Vec<_> = FacebookParser
        .stream(&mut reader, Category::Posts)
        .collect();
    let post = match items.as_slice() {
        [Ok(Entity::Post(p))] => p,
        other => panic!("{other:?}"),
    };
    assert_eq!(post.media.len(), 1);
    assert_eq!(
        post.media[0].path,
        "your_facebook_activity/posts/media/Fotky_z_časové_osy/p.jpg"
    );
    assert_eq!(post.media[0].size, Some(16));
    assert_eq!(
        post.media[0].blake3_hex.as_deref(),
        Some(hex_of(b"fixture-photo-cz").as_str())
    );
}

#[test]
fn album_streams_with_hashed_media_and_cover() {
    let (ok, errs) = stream("facebook-json", Category::Albums);
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(ok.len(), 1);
    let a = match &ok[0] {
        Entity::Album(a) => a,
        other => panic!("{other:?}"),
    };
    assert_eq!(a.name, "Mobile uploads");
    assert_eq!(a.description.as_deref(), Some("Album description"));
    assert_eq!(a.created_at, Timestamp(1_580_000_000_000_000));
    assert_eq!(a.audience, ArchiveAudience::Unknown);
    assert_eq!(a.media.len(), 2);
    assert_eq!(a.media[0].caption.as_deref(), Some("Album photo"));
    assert_eq!(a.media[0].size, Some(15));
    assert_eq!(
        a.media[0].blake3_hex.as_deref(),
        Some(hex_of(b"fixture-photo-2").as_str())
    );
    assert_eq!(
        a.media[1].blake3_hex.as_deref(),
        Some(hex_of(b"fixture-photo-3").as_str())
    );
    let cover = a.cover.as_ref().unwrap();
    assert_eq!(cover.path, a.media[0].path);
    assert_eq!(cover.blake3_hex, a.media[0].blake3_hex);
    assert!(a.external_id.is_derived());
    assert_eq!(a.external_id.kind, EntityKind::Album);
}

fn owner_ref() -> ExternalActorRef {
    ExternalActorRef::new(
        Platform::Facebook,
        Some("test.owner.fixture".to_string()),
        "Test Owner",
    )
}

#[test]
fn comments_stream_carries_authors_targets_and_repair() {
    let (ok, errs) = stream("facebook-json", Category::Comments);
    assert!(errs.is_empty(), "{errs:?}");
    let comments: Vec<ArchiveComment> = ok
        .into_iter()
        .map(|e| match e {
            Entity::Comment(c) => c,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(comments.len(), 2);
    let c0 = &comments[0];
    assert_eq!(c0.text.as_deref(), Some("Děkuji!"));
    assert_eq!(c0.author.display_name, "Test Owner");
    assert_eq!(c0.created_at, Timestamp(1_601_000_000_000_000));
    assert_eq!(c0.target.owner.as_ref().unwrap().display_name, "Friend One");
    assert_eq!(c0.target.kind_hint.as_deref(), Some("photo"));
    assert_eq!(c0.target.url, None);
    assert!(c0.external_id.is_derived());
    assert_eq!(c0.external_id.kind, EntityKind::Comment);
    let c1 = &comments[1];
    assert_eq!(c1.author.display_name, "Friend Two");
    assert_eq!(c1.text.as_deref(), Some("Nice one"));
    // "… on Test Owner's post" resolves to the owner ref itself (with its ID).
    assert_eq!(c1.target.owner, Some(owner_ref()));
    assert_eq!(c1.target.kind_hint.as_deref(), Some("post"));
    assert_eq!(
        c1.target.url.as_deref(),
        Some("https://example.invalid/post/1")
    );
    assert_ne!(c0.external_id, c1.external_id);
}

#[test]
fn reactions_stream_maps_kinds_and_targets() {
    let (ok, errs) = stream("facebook-json", Category::Reactions);
    assert!(errs.is_empty(), "{errs:?}");
    let reactions: Vec<ArchiveReaction> = ok
        .into_iter()
        .map(|e| match e {
            Entity::Reaction(r) => r,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(reactions.len(), 2);
    assert_eq!(reactions[0].kind, ReactionKind::Like);
    assert_eq!(reactions[0].author, owner_ref());
    assert_eq!(reactions[0].created_at, Timestamp(1_603_000_000_000_000));
    assert_eq!(
        reactions[0].target.owner.as_ref().unwrap().display_name,
        "Friend One"
    );
    assert_eq!(reactions[0].target.kind_hint.as_deref(), Some("post"));
    assert_eq!(reactions[1].kind, ReactionKind::Love);
    assert_eq!(
        reactions[1].target.owner.as_ref().unwrap().name_key,
        "friend two"
    );
    assert_eq!(reactions[1].target.kind_hint.as_deref(), Some("photo"));
    assert!(reactions[0].external_id.is_derived() && reactions[1].external_id.is_derived());
    assert_ne!(reactions[0].external_id, reactions[1].external_id);
    assert_eq!(ReactionKind::from_facebook("SUPPORT"), ReactionKind::Care);
    assert_eq!(
        ReactionKind::from_facebook("PRIDE"),
        ReactionKind::Other {
            raw: "PRIDE".into()
        }
    );
}

#[test]
fn same_second_same_kind_reactions_get_distinct_stable_ids() {
    let profile = br#"{ "profile_v2": { "name": { "full_name": "Test Owner" } } }"#;
    let reactions = br#"[
        { "timestamp": 1603000000, "data": [ { "reaction": { "reaction": "LIKE", "actor": "Test Owner" } } ], "title": "Test Owner likes Friend One's post." },
        { "timestamp": 1603000000, "data": [ { "reaction": { "reaction": "LIKE", "actor": "Test Owner" } } ], "title": "Test Owner likes Friend Two's photo." },
        { "timestamp": 1603000000, "data": [ { "reaction": { "reaction": "LOVE", "actor": "Test Owner" } } ], "title": "Test Owner reacted to Friend One's post." }
    ]"#;
    let src = common::zip_of(&[
        (
            "personal_information/profile_information/profile_information.json",
            profile,
        ),
        (
            "your_facebook_activity/comments_and_reactions/likes_and_reactions_1.json",
            reactions,
        ),
    ]);
    let ids = |src: &VecSource| -> Vec<String> {
        let mut reader = ArchiveReader::open(src).unwrap();
        FacebookParser
            .stream(&mut reader, Category::Reactions)
            .map(|r| match r {
                Ok(Entity::Reaction(x)) => x.external_id.id,
                other => panic!("{other:?}"),
            })
            .collect()
    };
    let first = ids(&src);
    assert_eq!(first.len(), 3);
    assert_ne!(
        first[0], first[1],
        "a same-second same-kind tie gets an ordinal"
    );
    assert_ne!(first[0], first[2]);
    assert_ne!(first[1], first[2]);
    // GOLDEN — the tied (n = 1) form, `"<token>#1"`: frozen like every other
    // derived-ID input. Never update to make a test pass.
    assert_eq!(
        first[1],
        "h:ad2ef076104ec0e1c347ce72daf83917794595a4dea536ff827bf18f223ff91b"
    );
    // The first of the tie is the plain (created_at, kind) ID — identical to
    // the fixture's pinned reaction golden, whose inputs are the same.
    let (ok, _) = stream("facebook-json", Category::Reactions);
    let Entity::Reaction(fixture_first) = &ok[0] else {
        panic!("{ok:?}")
    };
    assert_eq!(first[0], fixture_first.external_id.id);
    assert_eq!(ids(&src), first, "deterministic across runs");
}

#[test]
fn friends_stream_normalizes_names() {
    let (ok, errs) = stream("facebook-json", Category::Friends);
    assert!(errs.is_empty(), "{errs:?}");
    let friends: Vec<ArchiveFriendship> = ok
        .into_iter()
        .map(|e| match e {
            Entity::Friendship(f) => f,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(friends.len(), 2);
    assert_eq!(friends[0].actor.display_name, "Friend One");
    assert_eq!(friends[0].since, Some(Timestamp(1_500_000_000_000_000)));
    assert_eq!(friends[0].kind, FriendshipKind::Friend);
    assert_eq!(friends[1].actor.display_name, "Friend Two");
    assert_eq!(friends[1].actor.name_key, "friend two");
    assert_eq!(friends[1].actor.id, None);
}

#[test]
fn groups_stream() {
    let (ok, errs) = stream("facebook-json", Category::Groups);
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(ok.len(), 1);
    match &ok[0] {
        Entity::Group(g) => {
            assert_eq!(g.name, "Fixture Board Gamers");
            assert_eq!(g.joined_at, Some(Timestamp(1_540_000_000_000_000)));
            assert_eq!(g.external_id.kind, EntityKind::Group);
            assert!(g.external_id.is_derived());
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn events_stream_by_rsvp() {
    let (ok, errs) = stream("facebook-json", Category::Events);
    assert!(errs.is_empty(), "{errs:?}");
    let events: Vec<ArchiveEvent> = ok
        .into_iter()
        .map(|e| match e {
            Entity::Event(ev) => ev,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].title, "Fixture Picnic");
    assert_eq!(events[0].rsvp, Rsvp::Joined);
    assert_eq!(events[0].start, Timestamp(1_630_000_000_000_000));
    assert_eq!(events[0].end, Some(Timestamp(1_630_010_000_000_000)));
    assert_eq!(events[0].place.as_ref().unwrap().name, "Fixture Park");
    assert_eq!(events[0].description.as_deref(), Some("Bring food"));
    assert!(events[0].attendees.is_empty());
    assert_eq!(events[1].title, "Board game night");
    assert_eq!(events[1].rsvp, Rsvp::Interested);
    assert_eq!(events[1].end, None);
    assert_eq!(events[2].title, "Marathon");
    assert_eq!(events[2].rsvp, Rsvp::Declined);
    assert!(
        events
            .iter()
            .all(|e| e.external_id.is_derived() && e.external_id.kind == EntityKind::Event)
    );
}

#[test]
fn bad_social_records_are_skips_not_failures() {
    let profile = br#"{ "profile_v2": { "name": { "full_name": "Test Owner" } } }"#;
    let comments = br#"{ "comments_v2": [
        { "timestamp": 1, "data": [] },
        { "timestamp": 2, "data": [ { "comment": { "timestamp": 2, "comment": "ok", "author": "Friend One" } } ], "title": "Friend One commented on Test Owner's post." }
    ] }"#;
    let src = common::zip_of(&[
        (
            "personal_information/profile_information/profile_information.json",
            profile,
        ),
        (
            "your_facebook_activity/comments_and_reactions/comments.json",
            comments,
        ),
    ]);
    let mut reader = ArchiveReader::open(&src).unwrap();
    let items: Vec<_> = FacebookParser
        .stream(&mut reader, Category::Comments)
        .collect();
    assert_eq!(items.len(), 2);
    let err = items[0].as_ref().unwrap_err();
    assert_eq!(err.position, 0);
    assert!(err.reason.contains("comment"), "{}", err.reason);
    let ok = items[1].as_ref().unwrap();
    match ok {
        Entity::Comment(c) => {
            assert_eq!(c.text.as_deref(), Some("ok"));
            // A nameless-ID owner still matches by name_key.
            assert_eq!(c.target.owner.as_ref().unwrap().display_name, "Test Owner");
        }
        other => panic!("{other:?}"),
    }
    let summary = FacebookParser.index(&mut reader).unwrap();
    assert_eq!(
        summary.counts.comments, 2,
        "the index counts bad records too"
    );
}

const THREAD_ID: &str = "friendone_abc123";

fn messages(fixture: &str) -> (Vec<ArchiveMessage>, Vec<EntityError>) {
    let (ok, errs) = stream(fixture, Category::Messages);
    let msgs = ok
        .into_iter()
        .map(|e| match e {
            Entity::Message(m) => m,
            other => panic!("{other:?}"),
        })
        .collect();
    (msgs, errs)
}

#[test]
fn thread_streams_with_participants_and_span() {
    let (ok, errs) = stream("facebook-json", Category::Threads);
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(ok.len(), 1);
    let t = match &ok[0] {
        Entity::Thread(t) => t,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        t.external_id,
        ExternalId::native(Platform::Facebook, EntityKind::Thread, THREAD_ID)
    );
    assert_eq!(t.title.as_deref(), Some("Friend One"));
    assert_eq!(t.participants.len(), 2);
    assert_eq!(t.participants[0].display_name, "Friend One");
    assert_eq!(t.participants[0].id, None);
    assert_eq!(t.participants[1], owner_ref());
    assert_eq!(t.message_count, 3);
    assert_eq!(t.first_at, Some(Timestamp(1_650_000_000_000_000)));
    assert_eq!(t.last_at, Some(Timestamp(1_650_000_002_000_000)));
    assert_eq!(t.path, "inbox/friendone_abc123");
}

#[test]
fn messages_stream_oldest_first_with_repair_media_and_reactions() {
    let (msgs, errs) = messages("facebook-json");
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(msgs.len(), 3);
    let thread = ExternalId::native(Platform::Facebook, EntityKind::Thread, THREAD_ID);
    assert!(msgs.iter().all(|m| m.thread == thread));
    let m0 = &msgs[0];
    assert_eq!(m0.sender.display_name, "Friend One");
    assert_eq!(m0.text.as_deref(), Some("Ahoj! Přijdeš?"));
    assert_eq!(m0.created_at, Timestamp(1_650_000_000_000_000));
    let m1 = &msgs[1];
    assert_eq!(m1.text, None);
    assert_eq!(m1.media.len(), 1);
    assert_eq!(
        m1.media[0].path,
        "your_facebook_activity/messages/inbox/friendone_abc123/photos/img_1.jpg"
    );
    assert_eq!(m1.media[0].size, Some(17));
    assert_eq!(
        m1.media[0].blake3_hex.as_deref(),
        Some(hex_of(b"fixture-msg-photo").as_str())
    );
    assert_eq!(m1.media[0].taken_at, Some(Timestamp(1_650_000_001_000_000)));
    let m2 = &msgs[2];
    assert_eq!(m2.sender, owner_ref());
    assert_eq!(m2.text.as_deref(), Some("See you there"));
    assert_eq!(m2.reactions.len(), 1);
    assert_eq!(m2.reactions[0].actor.display_name, "Friend One");
    assert_eq!(m2.reactions[0].emoji, "❤");
    assert!(
        msgs.iter()
            .all(|m| m.external_id.is_derived() && m.external_id.kind == EntityKind::Message)
    );
    assert_ne!(msgs[0].external_id, msgs[1].external_id);
    assert_ne!(msgs[1].external_id, msgs[2].external_id);
}

/// GOLDEN — one frozen ID per derived-ID category beyond posts/messages.
/// Never update these to make a test pass: a change means every previously
/// imported archive would re-import as duplicates.
#[test]
fn golden_ids_per_category_are_frozen() {
    let (ok, _) = stream("facebook-json", Category::Comments);
    let Entity::Comment(c) = &ok[0] else {
        panic!("{ok:?}")
    };
    assert_eq!(
        c.external_id.id,
        "h:9d3aaefa8a6d23139394a4dbb7b532018535dfa0e9e5687a3fd477139f771da4"
    );
    let (ok, _) = stream("facebook-json", Category::Reactions);
    let Entity::Reaction(r) = &ok[0] else {
        panic!("{ok:?}")
    };
    assert_eq!(
        r.external_id.id,
        "h:6138e722cc2cbf316caed5f2719fed497956f7420ee57a5036ab940369a72cf9"
    );
    let (ok, _) = stream("facebook-json", Category::Events);
    let Entity::Event(e) = &ok[0] else {
        panic!("{ok:?}")
    };
    assert_eq!(
        e.external_id.id,
        "h:be833d12d730b3887b22e2e4149f094448b3031002cd25e928133091319fe470"
    );
    let (ok, _) = stream("facebook-json", Category::Groups);
    let Entity::Group(g) = &ok[0] else {
        panic!("{ok:?}")
    };
    assert_eq!(
        g.external_id.id,
        "h:991ca2b9a8acfb6231eda240b8de75a7cd45c3ad474bb515fe6452d1e632cb0c"
    );
    let (ok, _) = stream("facebook-json", Category::Albums);
    let Entity::Album(a) = &ok[0] else {
        panic!("{ok:?}")
    };
    assert_eq!(
        a.external_id.id,
        "h:7343f45e59da0eef110ffc0cd724fc58f88b9b3099ac96d3a82a32a8bb89c8d3"
    );
}

/// GOLDEN — frozen message ID. Never update to make a test pass.
#[test]
fn golden_message_id_is_frozen() {
    let (msgs, _) = messages("facebook-json");
    assert_eq!(
        msgs[0].external_id.id,
        "h:e3c99ffa99e882ad3dd9892068853361340c57bc2f5e3b96825b338649bf2813"
    );
}

#[test]
fn multi_file_thread_merges_sorts_and_skips_bad_messages() {
    let profile = br#"{ "profile_v2": { "name": { "full_name": "Test Owner" } } }"#;
    let newer = br#"{ "participants": [ { "name": "Friend One" }, { "name": "Test Owner" } ],
        "messages": [
            { "sender_name": "Test Owner", "timestamp_ms": 4000, "content": "four" },
            { "timestamp_ms": 3500, "content": "no sender" },
            { "sender_name": "Friend One", "timestamp_ms": 3000, "content": "three" }
        ],
        "title": "Friend One", "thread_path": "inbox/friendone_abc123" }"#;
    let older = br#"{ "participants": [ { "name": "Friend One" }, { "name": "Test Owner" } ],
        "messages": [
            { "sender_name": "Friend One", "timestamp_ms": 2000, "content": "two" },
            { "sender_name": "Test Owner", "timestamp_ms": 1000, "content": "one" }
        ],
        "title": "Friend One", "thread_path": "inbox/friendone_abc123" }"#;
    let src = common::zip_of(&[
        (
            "personal_information/profile_information/profile_information.json",
            profile,
        ),
        (
            "your_facebook_activity/messages/inbox/friendone_abc123/message_1.json",
            newer,
        ),
        (
            "your_facebook_activity/messages/inbox/friendone_abc123/message_2.json",
            older,
        ),
    ]);
    let mut reader = ArchiveReader::open(&src).unwrap();
    let items: Vec<_> = FacebookParser
        .stream(&mut reader, Category::Messages)
        .collect();
    let texts: Vec<String> = items
        .iter()
        .filter_map(|i| match i {
            Ok(Entity::Message(m)) => m.text.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["one", "two", "three", "four"]);
    let errs: Vec<&EntityError> = items.iter().filter_map(|i| i.as_ref().err()).collect();
    assert_eq!(errs.len(), 1);
    assert_eq!(
        errs[0].position, 1,
        "the bad record's index within its file"
    );
    assert!(errs[0].member.ends_with("message_1.json"));
    assert!(errs[0].reason.contains("sender"), "{}", errs[0].reason);
    let threads: Vec<_> = FacebookParser
        .stream(&mut reader, Category::Threads)
        .collect();
    match threads.as_slice() {
        [Ok(Entity::Thread(t))] => {
            assert_eq!(t.message_count, 4, "good messages across both files");
            assert_eq!(t.first_at, Some(Timestamp(1_000_000)));
            assert_eq!(t.last_at, Some(Timestamp(4_000_000)));
        }
        other => panic!("{other:?}"),
    }
    let summary = FacebookParser.index(&mut reader).unwrap();
    assert_eq!(summary.counts.threads, 1);
    assert_eq!(
        summary.counts.messages, 5,
        "the index counts every record, bad ones included"
    );
}

#[test]
fn summary_golden_over_the_full_fixture() {
    let src = common::fixture_source("facebook-json");
    let mut reader = ArchiveReader::open(&src).unwrap();
    let summary = FacebookParser.index(&mut reader).expect("index");
    assert_eq!(summary.platform, Platform::Facebook);
    assert_eq!(summary.owner, owner_ref());
    assert_eq!(
        summary.counts,
        CategoryCounts {
            posts: 4,
            albums: 1,
            comments: 2,
            reactions: 2,
            events: 3,
            groups: 1,
            friends: 2,
            threads: 1,
            messages: 3,
            profile: 1,
        }
    );
    assert_eq!(summary.media_bytes, 62);
    // Posts: Friends, Public, Only me, and the orphan without a privacy field;
    // the one album carries no privacy either.
    assert_eq!(
        summary.audiences,
        AudienceCounts {
            public: 1,
            friends: 1,
            custom: 0,
            only_me: 1,
            unknown: 2,
        }
    );
    assert_eq!(summary.audiences.known(), 3);
    assert_eq!(
        summary.date_range,
        Some(DateRange {
            first: Timestamp(1_580_000_000_000_000),
            last: Timestamp(1_650_000_002_000_000),
        })
    );
    assert_eq!(summary.parser_version, PARSER_VERSION);
    // Deterministic, and a Fauna record like everything else.
    let again = FacebookParser.index(&mut reader).unwrap();
    assert_eq!(again, summary);
    let bytes = serde_ipld_dagcbor::to_vec(&summary).unwrap();
    let back: ArchiveSummary = serde_ipld_dagcbor::from_slice(&bytes).unwrap();
    assert_eq!(back, summary);
}

#[test]
fn every_streamed_entity_round_trips_and_belongs_to_its_category() {
    for category in Category::ALL {
        let (ok, _) = stream("facebook-json", category.clone());
        for entity in ok {
            assert_eq!(entity.category().as_ref(), Some(&category));
            let bytes = serde_ipld_dagcbor::to_vec(&entity).unwrap();
            let back: Entity = serde_ipld_dagcbor::from_slice(&bytes).unwrap();
            assert_eq!(back, entity);
        }
    }
}

#[test]
fn unreadable_thread_member_surfaces_on_both_streams() {
    let profile = br#"{ "profile_v2": { "name": { "full_name": "Test Owner" } } }"#;
    let src = common::zip_of(&[
        (
            "personal_information/profile_information/profile_information.json",
            profile,
        ),
        (
            "your_facebook_activity/messages/inbox/broken_x1/message_1.json",
            b"this is not json",
        ),
    ]);
    let mut reader = ArchiveReader::open(&src).unwrap();
    let threads: Vec<_> = FacebookParser
        .stream(&mut reader, Category::Threads)
        .collect();
    assert_eq!(
        threads.len(),
        2,
        "one member-level error, then the (empty) thread"
    );
    let err = threads[0].as_ref().unwrap_err();
    assert_eq!(err.category, Category::Threads);
    assert!(err.member.ends_with("broken_x1/message_1.json"));
    assert!(matches!(threads[1], Ok(Entity::Thread(_))));
    let messages: Vec<_> = FacebookParser
        .stream(&mut reader, Category::Messages)
        .collect();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].as_ref().unwrap_err().category,
        Category::Threads
    );
}

/// A thread's ID is copied into every one of its messages, so a thread whose
/// ID outgrows `MAX_IDENTITY_BYTES` is refused whole — one skip on both
/// streams, no thread and no message — while an ID at the ceiling still reads
/// (§ Parser contract rule 9).
#[test]
fn a_thread_whose_id_outgrows_the_identity_ceiling_is_refused_on_both_streams() {
    let thread = |id_len: usize| {
        let id = "x".repeat(id_len);
        format!(
            r#"{{ "participants": [ {{ "name": "Friend One" }} ],
            "messages": [
                {{ "sender_name": "Friend One", "timestamp_ms": 2000, "content": "two" }},
                {{ "sender_name": "Friend One", "timestamp_ms": 1000, "content": "one" }}
            ],
            "title": "Friend One", "thread_path": "inbox/{id}" }}"#
        )
        .into_bytes()
    };
    let profile = br#"{ "profile_v2": { "name": { "full_name": "Test Owner" } } }"#;
    let at = thread(MAX_IDENTITY_BYTES);
    let over = thread(MAX_IDENTITY_BYTES + 1);
    let src = common::zip_of(&[
        (
            "personal_information/profile_information/profile_information.json",
            profile,
        ),
        (
            "your_facebook_activity/messages/inbox/at_1/message_1.json",
            &at,
        ),
        (
            "your_facebook_activity/messages/inbox/over_1/message_1.json",
            &over,
        ),
    ]);
    let mut reader = ArchiveReader::open(&src).unwrap();
    for category in [Category::Threads, Category::Messages] {
        let items: Vec<_> = FacebookParser
            .stream(&mut reader, category.clone())
            .collect();
        let errs: Vec<&EntityError> = items.iter().filter_map(|i| i.as_ref().err()).collect();
        match errs.as_slice() {
            [e] => {
                assert_eq!(e.category, Category::Threads, "{category:?}");
                assert!(e.member.ends_with("over_1/message_1.json"), "{}", e.member);
                assert!(e.reason.contains("thread ID"), "{}", e.reason);
            }
            other => panic!("{category:?}: one refusal for the over-long thread, got {other:?}"),
        }
        let ids: Vec<usize> = items
            .iter()
            .filter_map(|i| match i {
                Ok(Entity::Thread(t)) => Some(t.external_id.id.len()),
                Ok(Entity::Message(m)) => Some(m.thread.id.len()),
                _ => None,
            })
            .collect();
        let read = match category {
            Category::Threads => 1,
            _ => 2,
        };
        assert_eq!(
            ids,
            vec![MAX_IDENTITY_BYTES; read],
            "{category:?}: only the thread at the ceiling is read"
        );
    }
}

/// The owner's ID and display name are copied into every comment, reaction
/// and message the owner wrote or is the target of, so a profile carrying
/// either past `MAX_IDENTITY_BYTES` is refused — a skip — and the export is
/// read with the nameless owner an export without a profile gets.
#[test]
fn a_profile_whose_identity_outgrows_the_ceiling_is_refused_and_the_owner_is_nameless() {
    let long = "x".repeat(MAX_IDENTITY_BYTES + 1);
    let nameless = ExternalActorRef::new(Platform::Facebook, None, "");
    let profiles = [
        format!(
            r#"{{ "profile_v2": {{ "name": {{ "full_name": "Test Owner" }}, "username": "{long}" }} }}"#
        ),
        format!(r#"{{ "profile_v2": {{ "name": {{ "full_name": "{long}" }} }} }}"#),
    ];
    // An author-less comment is the owner's: it carries the owner ref.
    let comments = br#"{ "comments_v2": [ { "timestamp": 1601000000,
        "data": [ { "comment": { "timestamp": 1601000000, "comment": "hi" } } ] } ] }"#;
    for profile in &profiles {
        let src = common::zip_of(&[
            (
                "personal_information/profile_information/profile_information.json",
                profile.as_bytes(),
            ),
            (
                "your_facebook_activity/comments_and_reactions/comments.json",
                comments,
            ),
        ]);
        let mut reader = ArchiveReader::open(&src).unwrap();
        let items: Vec<_> = FacebookParser
            .stream(&mut reader, Category::Profile)
            .collect();
        match items.as_slice() {
            [Err(e)] => assert!(e.reason.contains("bytes, over the"), "{}", e.reason),
            other => panic!("the profile is refused, got {other:?}"),
        }
        assert_eq!(FacebookParser.index(&mut reader).unwrap().owner, nameless);
        let comments: Vec<_> = FacebookParser
            .stream(&mut reader, Category::Comments)
            .collect();
        match comments.as_slice() {
            [Ok(Entity::Comment(c))] => assert_eq!(c.author, nameless),
            other => panic!("{other:?}"),
        }
    }
}

/// The property the 2026-09-08 rule buys: the same post over a re-encoded
/// photo (Facebook re-encodes per export at the requester's chosen quality)
/// keeps its ID, so a re-import dedups it instead of authoring a duplicate.
#[test]
fn a_re_encoded_media_file_keeps_the_post_id() {
    let profile = br#"{ "profile_v2": { "name": { "full_name": "Test Owner" } } }"#;
    let posts = br#"[ { "timestamp": 1600000000, "data": [ { "post": "photo" } ],
        "attachments": [ { "data": [ { "media": { "uri": "your_facebook_activity/posts/media/Timeline_photos/p.jpg", "creation_timestamp": 1599990000 } } ] } ] } ]"#;
    let id_over = |photo: &[u8]| -> (String, Option<String>) {
        let src = common::zip_of(&[
            (
                "personal_information/profile_information/profile_information.json",
                profile,
            ),
            ("your_facebook_activity/posts/your_posts_1.json", posts),
            (
                "your_facebook_activity/posts/media/Timeline_photos/p.jpg",
                photo,
            ),
        ]);
        let mut reader = ArchiveReader::open(&src).unwrap();
        let items: Vec<_> = FacebookParser
            .stream(&mut reader, Category::Posts)
            .collect();
        match items.as_slice() {
            [Ok(Entity::Post(p))] => (p.external_id.id.clone(), p.media[0].blake3_hex.clone()),
            other => panic!("{other:?}"),
        }
    };
    let (high, high_hash) = id_over(b"high-quality-bytes");
    let (low, low_hash) = id_over(b"low-quality-bytes");
    assert_eq!(high, low, "the ID must not depend on the media bytes");
    assert_ne!(
        high_hash, low_hash,
        "the refs still carry the real hashes for the upload side"
    );
}
