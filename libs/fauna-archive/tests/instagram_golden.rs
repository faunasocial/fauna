//! Detection, layout and per-category goldens over the generated
//! `instagram-json` fixture (`archive-import.md` § Testing, tier_1; § Parser
//! contract rules 1–7). IDs pinned here are FROZEN: a change means every
//! previously imported Instagram archive would re-import.

mod common;

use fauna_archive::instagram::InstagramParser;
use fauna_archive::instagram::layout::locate;
use fauna_archive::model::*;
use fauna_archive::parser::{ArchiveParser, ExportFormat, detect};
use fauna_archive::reader::ArchiveReader;
use fauna_archive::{ArchiveError, EntityError};

const POSTS_MEMBER: &str = "your_instagram_activity/content/posts_1.json";
const THREAD_DIR: &str = "your_instagram_activity/messages/inbox/friendone_17840000000000001";

fn stream(fixture: &str, category: Category) -> (Vec<Entity>, Vec<EntityError>) {
    let src = common::fixture_source(fixture);
    let mut reader = ArchiveReader::open(&src).unwrap();
    let mut ok = Vec::new();
    let mut errs = Vec::new();
    for item in InstagramParser.stream(&mut reader, category) {
        match item {
            Ok(e) => ok.push(e),
            Err(e) => errs.push(e),
        }
    }
    (ok, errs)
}

fn posts() -> (Vec<ArchivePost>, Vec<EntityError>) {
    let (ok, errs) = stream("instagram-json", Category::Posts);
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

fn secs(s: u64) -> Timestamp {
    Timestamp(s * 1_000_000)
}

fn owner_ref() -> ExternalActorRef {
    ExternalActorRef::new(
        Platform::Instagram,
        Some("test.owner.fixture".into()),
        "Test Owner",
    )
}

fn user(name: &str) -> ExternalActorRef {
    ExternalActorRef::new(Platform::Instagram, Some(name.into()), name)
}

// --------------------------------------------------------------- detection

#[test]
fn detects_the_json_export_as_instagram() {
    let src = common::fixture_source("instagram-json");
    let reader = ArchiveReader::open(&src).unwrap();
    let (parser, detected) = detect(reader.directory()).expect("instagram detected");
    assert_eq!(parser.platform(), Platform::Instagram);
    assert_eq!(detected.platform, Platform::Instagram);
    assert_eq!(detected.format, ExportFormat::Json);
}

#[test]
fn detects_then_refuses_the_html_export() {
    let src = common::fixture_source("instagram-html");
    let mut reader = ArchiveReader::open(&src).unwrap();
    let (parser, detected) = detect(reader.directory()).expect("instagram detected");
    assert_eq!(detected.platform, Platform::Instagram);
    assert_eq!(detected.format, ExportFormat::Html);
    match parser.index(&mut reader) {
        Err(ArchiveError::UnsupportedFormat { platform, format }) => {
            assert_eq!(platform, "Instagram");
            assert_eq!(format, "HTML");
        }
        other => panic!("expected UnsupportedFormat, got {other:?}"),
    }
}

/// Neither Meta export is mistaken for the other: each parser's signature
/// matches only its own platform's paths.
#[test]
fn the_two_meta_exports_never_cross_detect() {
    for (fixture, platform) in [
        ("facebook-json", Platform::Facebook),
        ("facebook-html", Platform::Facebook),
        ("facebook-minimal", Platform::Facebook),
        ("instagram-json", Platform::Instagram),
        ("instagram-html", Platform::Instagram),
    ] {
        let src = common::fixture_source(fixture);
        let reader = ArchiveReader::open(&src).unwrap();
        let (_, detected) = detect(reader.directory()).expect(fixture);
        assert_eq!(detected.platform, platform, "{fixture}");
    }
    let src = common::fixture_source("facebook-json");
    let reader = ArchiveReader::open(&src).unwrap();
    assert!(InstagramParser.detect(reader.directory()).is_none());
}

#[test]
fn layout_locates_by_name_and_ignores_stories_and_ads() {
    let src = common::fixture_source("instagram-json");
    let reader = ArchiveReader::open(&src).unwrap();
    let layout = locate(reader.directory());
    assert_eq!(
        layout.profile.as_deref(),
        Some("personal_information/personal_information/personal_information.json")
    );
    assert_eq!(
        layout.posts,
        [
            "your_instagram_activity/content/archived_posts.json",
            POSTS_MEMBER,
        ]
    );
    assert_eq!(
        layout.comments,
        ["your_instagram_activity/comments/post_comments_1.json"]
    );
    assert_eq!(
        layout.reactions,
        ["your_instagram_activity/likes/liked_posts.json"]
    );
    assert_eq!(
        layout.followers,
        ["connections/followers_and_following/followers_1.json"]
    );
    assert_eq!(
        layout.following,
        ["connections/followers_and_following/following.json"]
    );
    assert_eq!(layout.threads.len(), 1);
    assert_eq!(layout.threads[0].dir, THREAD_DIR);
    assert!(layout.json && !layout.html);
    let all: Vec<&String> = layout.all_members().collect();
    assert!(!all.iter().any(|m| m.contains("stories")));
    assert!(!all.iter().any(|m| m.contains("ads_information")));
    // Instagram has no albums, events or groups: read from nothing.
    for category in [Category::Albums, Category::Events, Category::Groups] {
        assert!(layout.members_for(&category).is_empty(), "{category:?}");
    }
}

// ------------------------------------------------------------------- posts

#[test]
fn posts_stream_yields_four_posts_and_one_skip() {
    let (posts, errs) = posts();
    assert_eq!(posts.len(), 4);
    assert_eq!(errs.len(), 1);
    let skip = &errs[0];
    assert_eq!(skip.category, Category::Posts);
    assert_eq!(skip.member, POSTS_MEMBER);
    assert_eq!(skip.position, 3);
    assert!(
        skip.reason.contains("creation_timestamp"),
        "{}",
        skip.reason
    );
}

#[test]
fn a_single_photo_post_takes_its_caption_and_date_from_the_photo_repaired() {
    let (posts, _) = posts();
    // archived_posts.json sorts before posts_1.json; the feed posts follow.
    let post = &posts[1];
    assert_eq!(post.external_id.platform, Platform::Instagram);
    assert_eq!(post.external_id.kind, EntityKind::Post);
    assert_eq!(post.created_at, secs(1_600_000_000));
    assert_eq!(post.text.as_deref(), Some("Café morning"));
    // The export records no audience for a feed post: Unknown, which the
    // machine imports owner-only — never public.
    assert_eq!(post.audience, ArchiveAudience::Unknown);
    assert_eq!(post.media.len(), 1);
    let m = &post.media[0];
    assert_eq!(m.path, "media/posts/202009/photo_1.jpg");
    assert_eq!(m.mime.as_deref(), Some("image/jpeg"));
    assert_eq!(m.taken_at, Some(secs(1_600_000_000)));
    assert_eq!(
        m.blake3_hex.as_deref(),
        Some(hex_of(b"ig-photo-1\n").as_str())
    );
    assert_eq!(m.size, Some(11));
}

#[test]
fn a_multi_photo_post_keeps_every_photo_and_its_own_caption() {
    let (posts, _) = posts();
    let post = &posts[2];
    assert_eq!(post.created_at, secs(1_610_000_000));
    assert_eq!(post.text.as_deref(), Some("At the lake"));
    let paths: Vec<&str> = post.media.iter().map(|m| m.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "media/posts/202101/photo_2.jpg",
            "media/posts/202101/photo_3.jpg"
        ]
    );
    assert!(post.media.iter().all(|m| m.blake3_hex.is_some()));
}

#[test]
fn media_the_export_did_not_include_is_kept_unhashed() {
    let (posts, _) = posts();
    let post = &posts[3];
    assert_eq!(post.text.as_deref(), Some("Referenced but not included"));
    assert_eq!(post.media.len(), 1);
    assert!(post.media[0].blake3_hex.is_none() && post.media[0].size.is_none());
}

#[test]
fn an_archived_post_is_only_me() {
    let (posts, _) = posts();
    let post = &posts[0];
    assert_eq!(post.text.as_deref(), Some("Archived note"));
    assert_eq!(post.created_at, secs(1_620_000_000));
    assert_eq!(post.audience, ArchiveAudience::OnlyMe);
}

/// GOLDEN — frozen Instagram post IDs (pinned 2026-10-28 with slice 5,
/// before any Instagram archive was imported). Never update to make a test
/// pass.
#[test]
fn golden_post_ids_are_frozen() {
    let (posts, _) = posts();
    let ids: Vec<&str> = posts.iter().map(|p| p.external_id.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "h:2f3f4e608e2ccf9c44b2e539ddd1fa076e504c1f75f542ee3e5d85e3ba13f382",
            "h:d1affcf347242af46bba26c88479ad893d1fa172874d6395a104f8c8ae612821",
            "h:acc634a4b0293424c7e717b004608337ce5bc44a7b1090d382a4e3b0868843ac",
            "h:eb9339ea3400f9861831d7110c9a44e6b120ef8be9e0f6c4b3b71e1b763e95ad",
        ]
    );
    let (again, _) = self::posts();
    assert_eq!(
        again, posts,
        "streaming again yields the same posts (rule 4)"
    );
}

// ----------------------------------------------------------------- profile

#[test]
fn profile_names_the_owner_by_username_and_repairs_the_bio() {
    let (ok, errs) = stream("instagram-json", Category::Profile);
    assert!(errs.is_empty(), "{errs:?}");
    let [Entity::Profile(profile)] = ok.as_slice() else {
        panic!("one profile, got {ok:?}");
    };
    assert_eq!(profile.actor, owner_ref());
    assert_eq!(
        profile.external_id,
        ExternalId::native(
            Platform::Instagram,
            EntityKind::Profile,
            "test.owner.fixture"
        )
    );
    assert_eq!(profile.bio.as_deref(), Some("Fixture bio ❤"));
    assert_eq!(profile.links, ["https://example.invalid/owner"]);
    assert_eq!(
        profile.picture.as_ref().map(|p| p.path.as_str()),
        Some("media/profile/202001/profile_1.jpg")
    );
}

// ------------------------------------------------------- comments and likes

#[test]
fn comments_are_the_owners_on_someone_elses_post() {
    let (ok, errs) = stream("instagram-json", Category::Comments);
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].position, 1);
    assert!(errs[0].reason.contains("time"), "{}", errs[0].reason);
    let [Entity::Comment(c)] = ok.as_slice() else {
        panic!("one comment, got {ok:?}");
    };
    assert_eq!(c.text.as_deref(), Some("Děkuji!"));
    assert_eq!(c.created_at, secs(1_601_000_000));
    assert_eq!(c.author, owner_ref());
    assert_eq!(c.target.owner, Some(user("friend.one")));
    assert_eq!(c.target.kind_hint.as_deref(), Some("post"));
}

#[test]
fn likes_in_one_second_get_distinct_stable_ids() {
    let (ok, errs) = stream("instagram-json", Category::Reactions);
    assert!(errs.is_empty(), "{errs:?}");
    let likes: Vec<ArchiveReaction> = ok
        .into_iter()
        .map(|e| match e {
            Entity::Reaction(r) => r,
            other => panic!("expected a reaction, got {other:?}"),
        })
        .collect();
    assert_eq!(likes.len(), 2);
    for like in &likes {
        assert_eq!(like.kind, ReactionKind::Like);
        assert_eq!(like.author, owner_ref());
        assert_eq!(like.created_at, secs(1_603_000_000));
    }
    assert_eq!(likes[0].target.owner, Some(user("friend.one")));
    assert_eq!(
        likes[1].target.url.as_deref(),
        Some("https://www.instagram.com/p/FIXTURE2/")
    );
    assert_ne!(likes[0].external_id, likes[1].external_id);
    // The first of a tie keeps the plain (instant, kind) derivation.
    assert_eq!(
        likes[0].external_id,
        ExternalId::derive(
            Platform::Instagram,
            EntityKind::Reaction,
            secs(1_603_000_000),
            "like",
            &MediaInstants::new(),
        )
    );
}

#[test]
fn followers_and_following_are_kept_with_their_usernames() {
    let (ok, errs) = stream("instagram-json", Category::Friends);
    assert!(errs.is_empty(), "{errs:?}");
    let got: Vec<(String, FriendshipKind, Option<Timestamp>)> = ok
        .into_iter()
        .map(|e| match e {
            Entity::Friendship(f) => (f.actor.id.unwrap(), f.kind, f.since),
            other => panic!("expected a friendship, got {other:?}"),
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                "friend.one".into(),
                FriendshipKind::Follower,
                Some(secs(1_500_000_000))
            ),
            (
                "friend.two".into(),
                FriendshipKind::Follower,
                Some(secs(1_510_000_000))
            ),
            // A following record whose username sits in its title.
            (
                "friend.one".into(),
                FriendshipKind::Following,
                Some(secs(1_505_000_000))
            ),
        ]
    );
}

// ------------------------------------------------------- threads, messages

#[test]
fn direct_messages_stream_as_an_instagram_thread_oldest_first() {
    let (threads, errs) = stream("instagram-json", Category::Threads);
    assert!(errs.is_empty(), "{errs:?}");
    let [Entity::Thread(thread)] = threads.as_slice() else {
        panic!("one thread, got {threads:?}");
    };
    assert_eq!(
        thread.external_id,
        ExternalId::native(
            Platform::Instagram,
            EntityKind::Thread,
            "friendone_17840000000000001"
        )
    );
    assert_eq!(thread.message_count, 2);
    assert!(thread.participants.contains(&owner_ref()));

    let (ok, errs) = stream("instagram-json", Category::Messages);
    assert!(errs.is_empty(), "{errs:?}");
    let messages: Vec<ArchiveMessage> = ok
        .into_iter()
        .map(|e| match e {
            Entity::Message(m) => m,
            other => panic!("expected a message, got {other:?}"),
        })
        .collect();
    assert_eq!(messages.len(), 2);
    let first = &messages[0];
    assert_eq!(first.text.as_deref(), Some("Ahoj! Přijdeš?"));
    assert_eq!(first.external_id.platform, Platform::Instagram);
    assert_eq!(first.sender.platform, Platform::Instagram);
    assert_eq!(first.sender.display_name, "Friend One");
    assert_eq!(first.media.len(), 1);
    assert!(first.media[0].blake3_hex.is_some());
    assert_eq!(first.reactions.len(), 1);
    assert_eq!(first.reactions[0].emoji, "❤");
    // The owner is named by display name in a thread; it resolves to the
    // profile's ref, username included.
    assert_eq!(first.reactions[0].actor, owner_ref());
    assert_eq!(messages[1].sender, owner_ref());
}

/// GOLDEN — frozen comment and message IDs (pinned with slice 5).
#[test]
fn golden_ids_per_category_are_frozen() {
    let (ok, _) = stream("instagram-json", Category::Comments);
    let Entity::Comment(c) = &ok[0] else { panic!() };
    assert_eq!(
        c.external_id.id,
        "h:8fa41741229b518fe226b1805525f60b596a2d33b898b4146fb6b8ce69559f14"
    );
    let (ok, _) = stream("instagram-json", Category::Messages);
    let Entity::Message(m) = &ok[0] else { panic!() };
    assert_eq!(
        m.external_id.id,
        "h:5858e474057aca9ddedc46b8653711cee38867a0aee5e5543a710f8620e3f962"
    );
}

// ----------------------------------------------------------------- summary

#[test]
fn summary_golden_over_the_full_fixture() {
    let src = common::fixture_source("instagram-json");
    let mut reader = ArchiveReader::open(&src).unwrap();
    let summary = InstagramParser.index(&mut reader).expect("index");
    assert_eq!(summary.platform, Platform::Instagram);
    assert_eq!(summary.owner, owner_ref());
    assert_eq!(
        summary.counts,
        CategoryCounts {
            // Four feed records (the unreadable one included, so "N of M"
            // adds up with the skip log) and one archived post.
            posts: 5,
            albums: 0,
            comments: 2,
            reactions: 2,
            events: 0,
            groups: 0,
            friends: 3,
            threads: 1,
            messages: 2,
            profile: 1,
        }
    );
    // Six fixture pictures, sized from the directory, never read.
    assert_eq!(summary.media_bytes, 11 * 4 + 11 + 10);
    assert_eq!(
        summary.audiences,
        AudienceCounts {
            public: 0,
            friends: 0,
            custom: 0,
            only_me: 1,
            unknown: 4,
        }
    );
    assert_eq!(
        summary.date_range,
        Some(DateRange {
            first: secs(1_600_000_000),
            last: Timestamp(1_650_000_002_000_000),
        })
    );
    assert_eq!(summary.parser_version, PARSER_VERSION);
    let again = InstagramParser.index(&mut reader).unwrap();
    assert_eq!(again, summary);
}

/// Every streamed entity encodes and belongs to its category. The decode half
/// of the round trip is `model_roundtrip.rs`'s, over every entity type — not
/// repeated here, so no further raw decode site joins serialization.md's list.
#[test]
fn every_streamed_entity_encodes_and_belongs_to_its_category() {
    for category in Category::ALL {
        let (ok, _) = stream("instagram-json", category.clone());
        for entity in ok {
            assert_eq!(entity.category().as_ref(), Some(&category));
            assert!(!serde_ipld_dagcbor::to_vec(&entity).unwrap().is_empty());
        }
    }
}

/// An export holding only the profile and one posts file: every other
/// category is zero, never a refusal (§ Parser contract rule 2); and with no
/// profile at all the owner is nameless rather than the import refused.
#[test]
fn a_minimal_export_reads_as_zero_of_everything_else() {
    let posts = br#"[{"media":[{"uri":"media/posts/1.jpg","creation_timestamp":1600000000,"title":"Only post"}]}]"#;
    let profile = br#"{"profile_user":[{"string_map_data":{"Username":{"value":"solo.owner"}}}]}"#;
    let src = common::zip_of(&[
        (
            "instagram-solo/personal_information/personal_information/personal_information.json",
            profile,
        ),
        (
            "instagram-solo/your_instagram_activity/content/posts_1.json",
            posts,
        ),
    ]);
    let mut reader = ArchiveReader::open(&src).unwrap();
    let summary = InstagramParser.index(&mut reader).unwrap();
    assert_eq!(summary.counts.posts, 1);
    assert_eq!(summary.counts.profile, 1);
    assert_eq!(
        summary.counts.comments + summary.counts.reactions + summary.counts.friends,
        0
    );
    assert_eq!(summary.counts.threads + summary.counts.messages, 0);
    // A profile with a username but no name shows the username.
    assert_eq!(summary.owner.display_name, "solo.owner");

    let src = common::zip_of(&[(
        "your_instagram_activity/content/posts_1.json",
        posts.as_slice(),
    )]);
    let mut reader = ArchiveReader::open(&src).unwrap();
    let summary = InstagramParser.index(&mut reader).unwrap();
    assert_eq!(summary.platform, Platform::Instagram);
    assert_eq!(summary.owner.display_name, "");
    assert_eq!(summary.counts.posts, 1);
}
