//! Detection, format refusal, unknown-file tolerance and missing-category
//! tolerance (`archive-import.md` § Parser contract rules 2 and 5).

mod common;

use fauna_archive::ArchiveError;
use fauna_archive::facebook::FacebookParser;
use fauna_archive::facebook::layout::locate;
use fauna_archive::model::{Category, Platform, Timestamp};
use fauna_archive::parser::{ArchiveParser, ExportFormat, detect};
use fauna_archive::reader::ArchiveReader;
use fauna_archive::source::VecSource;

#[test]
fn detects_the_json_export() {
    let src = common::fixture_source("facebook-json");
    let reader = ArchiveReader::open(&src).unwrap();
    let (parser, detected) = detect(reader.directory()).expect("facebook detected");
    assert_eq!(parser.platform(), Platform::Facebook);
    assert_eq!(detected.platform, Platform::Facebook);
    assert_eq!(detected.format, ExportFormat::Json);
}

#[test]
fn detects_then_refuses_the_html_export() {
    let src = common::fixture_source("facebook-html");
    let mut reader = ArchiveReader::open(&src).unwrap();
    let (parser, detected) = detect(reader.directory()).expect("facebook detected");
    assert_eq!(detected.format, ExportFormat::Html);
    match parser.index(&mut reader) {
        Err(ArchiveError::UnsupportedFormat { platform, format }) => {
            assert_eq!(platform, "Facebook");
            assert_eq!(format, "HTML");
        }
        other => panic!("expected UnsupportedFormat, got {other:?}"),
    }
}

#[test]
fn an_unrelated_zip_is_not_detected() {
    use std::io::{Cursor, Write};
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    w.start_file("notes/todo.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    w.write_all(b"[]").unwrap();
    let src = VecSource(w.finish().unwrap().into_inner());
    let reader = ArchiveReader::open(&src).unwrap();
    assert!(detect(reader.directory()).is_none());
    assert!(FacebookParser.detect(reader.directory()).is_none());
}

#[test]
fn layout_locates_by_suffix_and_ignores_unknown_files() {
    let src = common::fixture_source("facebook-json");
    let reader = ArchiveReader::open(&src).unwrap();
    let layout = locate(reader.directory());
    assert_eq!(
        layout.posts,
        ["your_facebook_activity/posts/your_posts__check_ins__photos_and_videos_1.json"]
    );
    assert_eq!(layout.albums, ["your_facebook_activity/posts/album/0.json"]);
    assert_eq!(
        layout.comments,
        ["your_facebook_activity/comments_and_reactions/comments.json"]
    );
    assert_eq!(
        layout.reactions,
        ["your_facebook_activity/comments_and_reactions/likes_and_reactions_1.json"]
    );
    assert_eq!(layout.friends, ["connections/friends/your_friends.json"]);
    assert_eq!(
        layout.events,
        ["your_facebook_activity/events/your_event_responses.json"]
    );
    assert_eq!(
        layout.groups,
        ["your_facebook_activity/groups/your_group_membership_activity.json"]
    );
    assert_eq!(
        layout.profile.as_deref(),
        Some("personal_information/profile_information/profile_information.json")
    );
    assert_eq!(layout.threads.len(), 1);
    assert_eq!(
        layout.threads[0].dir,
        "your_facebook_activity/messages/inbox/friendone_abc123"
    );
    assert_eq!(
        layout.threads[0].files,
        ["your_facebook_activity/messages/inbox/friendone_abc123/message_1.json"]
    );
    assert!(layout.json && !layout.html);
    // The advertisers file is in no category.
    let all: Vec<&String> = layout.all_members().collect();
    assert!(!all.iter().any(|m| m.contains("ads_information")));
    assert_eq!(layout.media_bytes(reader.directory()), 62);
}

#[test]
fn numbered_members_sort_naturally() {
    use std::io::{Cursor, Write};
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for n in [10, 2, 1] {
        w.start_file(
            format!("your_facebook_activity/posts/your_posts_{n}.json"),
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        w.write_all(b"[]").unwrap();
    }
    let src = VecSource(w.finish().unwrap().into_inner());
    let reader = ArchiveReader::open(&src).unwrap();
    let layout = locate(reader.directory());
    assert_eq!(
        layout.posts,
        [
            "your_facebook_activity/posts/your_posts_1.json",
            "your_facebook_activity/posts/your_posts_2.json",
            "your_facebook_activity/posts/your_posts_10.json",
        ]
    );
}

#[test]
fn minimal_export_indexes_with_zero_for_missing_categories() {
    let src = common::fixture_source("facebook-minimal");
    let mut reader = ArchiveReader::open(&src).unwrap();
    let summary = FacebookParser.index(&mut reader).expect("index");
    assert_eq!(summary.platform, Platform::Facebook);
    assert_eq!(summary.owner.display_name, "Test Owner");
    assert_eq!(summary.owner.id.as_deref(), Some("100099999999999"));
    assert_eq!(summary.counts.profile, 1);
    assert_eq!(summary.counts.comments, 0);
    assert_eq!(summary.counts.threads, 0);
    assert_eq!(summary.media_bytes, 0);
    for category in [
        Category::Comments,
        Category::Threads,
        Category::Messages,
        Category::Events,
    ] {
        assert_eq!(
            FacebookParser.stream(&mut reader, category.clone()).count(),
            0,
            "{category:?}"
        );
    }
}

#[test]
fn profile_streams_the_owner() {
    let src = common::fixture_source("facebook-json");
    let mut reader = ArchiveReader::open(&src).unwrap();
    let items: Vec<_> = FacebookParser
        .stream(&mut reader, Category::Profile)
        .collect();
    assert_eq!(items.len(), 1);
    let profile = match items.into_iter().next().unwrap().unwrap() {
        fauna_archive::model::Entity::Profile(p) => p,
        other => panic!("expected a profile, got {other:?}"),
    };
    assert_eq!(profile.actor.display_name, "Test Owner");
    assert_eq!(profile.actor.id.as_deref(), Some("test.owner.fixture"));
    assert_eq!(profile.actor.name_key, "test owner");
    assert_eq!(profile.external_id.id, "test.owner.fixture");
    assert_eq!(profile.bio.as_deref(), Some("Fixture bio ❤"));
    assert_eq!(profile.links, ["https://example.invalid/owner"]);
    assert_eq!(
        profile.registered_at,
        Some(Timestamp(1_262_304_000_000_000))
    );
}

/// `locate` groups a thread's split `message_N.json` files under one directory,
/// and keeps directories in first-seen order.
///
/// This is the correctness pin for the grouping's move from a linear scan of
/// `threads` (O(entries × threads) by construction, re-derived on every
/// `detect`/`index`/`stream`) to a map. Deliberately a behavioural assertion and
/// **not** a timing one — a wall-clock threshold would be exactly the shape
/// convention 14 forbids, and would tell us nothing on a loaded box.
#[test]
fn locate_groups_split_thread_files_under_one_directory() {
    let members: Vec<(&str, &[u8])> = vec![
        ("messages/inbox/alice_1/message_1.json", b"[]"),
        ("messages/inbox/bob_2/message_1.json", b"[]"),
        ("messages/inbox/alice_1/message_2.json", b"[]"),
        ("messages/inbox/alice_1/message_10.json", b"[]"),
        ("messages/inbox/bob_2/message_2.json", b"[]"),
    ];
    let src = fauna_archive::testing::zip_of(&members);
    let reader = ArchiveReader::open(&src).unwrap();
    let layout = locate(reader.directory());

    assert_eq!(layout.threads.len(), 2, "two directories, two threads");
    assert_eq!(
        layout.threads[0].dir, "messages/inbox/alice_1",
        "first-seen order is preserved"
    );
    assert_eq!(layout.threads[1].dir, "messages/inbox/bob_2");
    assert_eq!(
        layout.threads[0].files.len(),
        3,
        "every split file of one thread groups together, interleaving included"
    );
    assert_eq!(layout.threads[1].files.len(), 2);
    // Natural order, so message_10 sorts after message_2 rather than beside _1.
    assert!(
        layout.threads[0].files[2].ends_with("message_10.json"),
        "split files stay naturally sorted: {:?}",
        layout.threads[0].files
    );
}
