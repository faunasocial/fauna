//! Every archive entity is a Fauna record: it must encode as canonical
//! DAG-CBOR and decode back to the same value
//! (`docs/goal/behavior/archive-import.md` § The archive model). The codec
//! is the IPLD one Fauna's own canonical encoder wraps.

use fauna_archive::model::*;
use serde_ipld_dagcbor::to_vec as encode_canonical;

fn actor(name: &str) -> ExternalActorRef {
    ExternalActorRef::new(Platform::Facebook, Some("100001".to_string()), name)
}

fn xid(kind: EntityKind, id: &str) -> ExternalId {
    ExternalId {
        platform: Platform::Facebook,
        kind,
        id: id.to_string(),
    }
}

fn media(path: &str) -> ArchiveMediaRef {
    ArchiveMediaRef {
        path: path.to_string(),
        size: Some(12),
        blake3_hex: Some("ab".repeat(32)),
        mime: Some("image/jpeg".to_string()),
        taken_at: Some(Timestamp(1_600_000_000_000_000)),
        caption: Some("a caption".to_string()),
    }
}

fn round_trip<T>(v: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let bytes = encode_canonical(v).expect("canonical encode");
    // Raw decode of bytes this very test encoded — the self-encoded case
    // `serialization.md` § How decode-strictness is actually enforced argues
    // for this crate's tests; the library itself never decodes.
    let back: T = serde_ipld_dagcbor::from_slice(&bytes).expect("decode");
    assert_eq!(&back, v);
}

#[test]
fn platform_tokens_are_lowercase_and_stable() {
    assert_eq!(Platform::Facebook.token(), "facebook");
    assert_eq!(Platform::Instagram.token(), "instagram");
    assert_eq!(Platform::Facebook.label(), "Facebook");
    assert_eq!(
        Platform::parse_token("Instagram"),
        Some(Platform::Instagram)
    );
    assert_eq!(Platform::parse_token("x"), None);
    // The serde form IS the token: the marker file and the origin field agree.
    let json = serde_json::to_string(&Platform::Facebook).unwrap();
    assert_eq!(json, "\"facebook\"");
}

#[test]
fn actor_ref_computes_name_key() {
    let a = ExternalActorRef::new(Platform::Facebook, None, "  Friend   One ");
    assert_eq!(a.display_name, "Friend One");
    assert_eq!(a.name_key, "friend one");
    assert_eq!(a.id, None);
}

#[test]
fn every_entity_round_trips_through_canonical_cbor() {
    let owner = actor("Test Owner");
    round_trip(&ArchiveProfile {
        external_id: xid(EntityKind::Profile, "100001"),
        actor: owner.clone(),
        bio: Some("hello".to_string()),
        links: vec!["https://example.invalid".to_string()],
        picture: Some(media("p.jpg")),
        registered_at: Some(Timestamp(1_500_000_000_000_000)),
    });
    round_trip(&ArchivePost {
        external_id: xid(EntityKind::Post, "h:00"),
        created_at: Timestamp(1_600_000_000_000_000),
        audience: ArchiveAudience::Friends,
        text: Some("text".to_string()),
        media: vec![media("a.jpg")],
        links: vec!["https://example.invalid/x".to_string()],
        tagged: vec![actor("Friend One")],
        album_name: Some("Timeline photos".to_string()),
        url: None,
        place: Some(ArchivePlace {
            name: "Somewhere".to_string(),
            address: None,
            latitude: Some("59.9".to_string()),
            longitude: Some("10.7".to_string()),
            url: None,
        }),
    });
    round_trip(&ArchiveAlbum {
        external_id: xid(EntityKind::Album, "h:01"),
        name: "Mobile uploads".to_string(),
        description: None,
        created_at: Timestamp(1),
        audience: ArchiveAudience::Unknown,
        media: vec![media("b.jpg")],
        cover: None,
    });
    round_trip(&ArchiveComment {
        external_id: xid(EntityKind::Comment, "h:02"),
        created_at: Timestamp(2),
        author: actor("Friend One"),
        target: ArchiveTargetRef {
            id: None,
            url: Some("https://example.invalid/post".to_string()),
            owner: Some(owner.clone()),
            kind_hint: Some("post".to_string()),
        },
        text: Some("nice".to_string()),
        media: vec![],
    });
    round_trip(&ArchiveReaction {
        external_id: xid(EntityKind::Reaction, "h:03"),
        created_at: Timestamp(3),
        author: owner.clone(),
        target: ArchiveTargetRef {
            id: None,
            url: None,
            owner: Some(actor("Friend Two")),
            kind_hint: Some("photo".to_string()),
        },
        kind: ReactionKind::Other {
            raw: "PRIDE".to_string(),
        },
    });
    round_trip(&ArchiveEvent {
        external_id: xid(EntityKind::Event, "h:04"),
        title: "Picnic".to_string(),
        description: None,
        start: Timestamp(4),
        end: Some(Timestamp(5)),
        place: None,
        rsvp: Rsvp::Joined,
        attendees: vec![actor("Friend One")],
        url: None,
    });
    round_trip(&ArchiveGroup {
        external_id: xid(EntityKind::Group, "h:05"),
        name: "Board games".to_string(),
        joined_at: Some(Timestamp(6)),
        url: None,
    });
    round_trip(&ArchiveFriendship {
        actor: actor("Friend Two"),
        since: Some(Timestamp(7)),
        kind: FriendshipKind::Friend,
    });
    round_trip(&ArchiveThread {
        external_id: xid(EntityKind::Thread, "friendone_abc123"),
        title: Some("Friend One".to_string()),
        participants: vec![owner.clone(), actor("Friend One")],
        message_count: 3,
        first_at: Some(Timestamp(8)),
        last_at: Some(Timestamp(9)),
        path: "inbox/friendone_abc123".to_string(),
    });
    round_trip(&ArchiveMessage {
        external_id: xid(EntityKind::Message, "h:06"),
        thread: xid(EntityKind::Thread, "friendone_abc123"),
        sender: actor("Friend One"),
        created_at: Timestamp(10),
        text: Some("hi".to_string()),
        media: vec![media("m.jpg")],
        reactions: vec![MessageReaction {
            actor: owner.clone(),
            emoji: "❤".to_string(),
        }],
    });
    round_trip(&ArchiveSummary {
        platform: Platform::Facebook,
        owner,
        date_range: Some(DateRange {
            first: Timestamp(1),
            last: Timestamp(10),
        }),
        counts: CategoryCounts {
            posts: 3,
            albums: 1,
            comments: 2,
            reactions: 2,
            events: 3,
            groups: 1,
            friends: 2,
            threads: 1,
            messages: 3,
            profile: 1,
        },
        media_bytes: 1234,
        parser_version: PARSER_VERSION,
        audiences: AudienceCounts::default(),
    });
    // The Entity envelope round-trips too (it is what a stream yields and
    // what `model/<category>.cbor` will hold as a list).
    round_trip(&Entity::Group(ArchiveGroup {
        external_id: xid(EntityKind::Group, "h:05"),
        name: "Board games".to_string(),
        joined_at: None,
        url: None,
    }));
}

#[test]
fn unknown_unit_variants_degrade_to_unknown() {
    // An older reader meeting a variant a newer writer minted must not fail
    // the whole file: the unit enums carry a `#[serde(other)]` catch-all.
    assert_eq!(
        serde_json::from_str::<ArchiveAudience>("\"friends_of_friends\"").unwrap(),
        ArchiveAudience::Unknown
    );
    assert_eq!(
        serde_json::from_str::<Rsvp>("\"maybe\"").unwrap(),
        Rsvp::Unknown
    );
    assert_eq!(
        serde_json::from_str::<FriendshipKind>("\"blocked\"").unwrap(),
        FriendshipKind::Unknown
    );
    assert_eq!(
        serde_json::from_str::<Rsvp>("\"joined\"").unwrap(),
        Rsvp::Joined
    );
}

#[test]
fn category_counts_answer_by_category() {
    let counts = CategoryCounts {
        posts: 1,
        albums: 2,
        comments: 3,
        reactions: 4,
        events: 5,
        groups: 6,
        friends: 7,
        threads: 8,
        messages: 9,
        profile: 1,
    };
    for (category, expected) in Category::ALL.iter().zip([1u64, 2, 3, 4, 5, 6, 7, 8, 9, 1]) {
        assert_eq!(counts.get(category), expected, "{category:?}");
    }
    assert_eq!(Category::Posts.token(), "posts");
    assert_eq!(Category::ALL.len(), 10);
}
