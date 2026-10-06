//! `data.conversation_threads[]` — the shared e2e state-serialization contract
//! (`state_json::conversation_threads_json`).
//!
//! The row this file exists for is **`participant_actor_ids`**, added 2026-08-10
//! so a driver can witness an identity succession's participant re-point
//! (`identity-succession.md` § Propagation → *MLS groups*: the row is re-pointed
//! **in place**, same position, **handle kept** — the nest moved the handle to
//! the successor inside the succession transaction). Everything a driver could
//! previously read is invariant under exactly that operation: `label`, `snippet`
//! and `participant_count` do not move, and `thread-member-chip[i]`'s text is the
//! kept handle. So the succeeding member's continuity render had **no observable
//! at all** — a test could only assert the right outcome through a gate that
//! cannot see it.

use fauna_conversations::backends::mock::MockRailBackend;
use fauna_conversations::state_json::conversation_threads_json;
use fauna_conversations::*;
use fauna_core::identity::{ActorId, ActorKeypair};
use std::sync::Arc;

fn inbound(rail: Rail, sender: TypedAddress, recipients: Vec<TypedAddress>) -> RailInboundMessage {
    RailInboundMessage {
        rail,
        sender,
        recipients,
        subject: None,
        body: "hello".into(),
        body_format: BodyFormat::PlainText,
        timestamp_ms: 1,
        message_id: MessageId("m1".into()),
        in_reply_to: None,
        attachments: vec![],
        badges: MessageBadges::default(),
        legal_takedown_ref: None,
        plane_ref: None,
    }
}

/// The one row of the serialized shape this file is about, for the single
/// thread the manager holds.
fn only_row(manager: &ConversationsManager) -> serde_json::Value {
    let rows = conversation_threads_json(manager);
    let arr = rows.as_array().expect("the contract is an array");
    assert_eq!(arr.len(), 1, "fixture builds exactly one thread: {rows:?}");
    arr[0].clone()
}

fn str_list(row: &serde_json::Value, key: &str) -> Vec<Option<String>> {
    row[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} must serialize as an array; row={row:?}"))
        .iter()
        .map(|v| v.as_str().map(str::to_string))
        .collect()
}

/// A Fauna participant's actor id is readable, and it is **index-parallel with
/// `participant_displays`** — which is what makes it assertable against the
/// `thread-member-chip[i]` a test can see.
///
/// Alignment is the whole point rather than a nicety: the re-point keeps the
/// list position (`ThreadStore::repoint_participant`), so a driver asserting
/// "chip 1's person is now the successor" needs the id at *that* index. A bare
/// unordered set of ids would answer "the successor is in this thread", which is
/// also true of a stranger honestly added a moment later.
#[test]
fn a_fauna_participants_actor_id_is_readable_and_aligned_with_its_display() {
    let alice = ActorKeypair::from_secret([11u8; 32]).actor_id();
    let manager = ConversationsManager::new();
    manager.register_backend(Arc::new(MockRailBackend::new(Rail::FaunaMls)));
    manager
        .ingest_inbound(inbound(
            Rail::FaunaMls,
            TypedAddress::Fauna {
                handle: "alice@nest.test".into(),
                actor_id: alice,
            },
            vec![],
        ))
        .expect("ingest");

    let row = only_row(&manager);
    let ids = str_list(&row, "participant_actor_ids");
    let displays: Vec<Option<String>> = manager
        .thread_detail(ThreadId(row["thread_id"].as_str().unwrap().to_string()))
        .expect("detail")
        .participant_displays
        .into_iter()
        .map(Some)
        .collect();

    assert_eq!(
        ids.len(),
        displays.len(),
        "participant_actor_ids is index-parallel with participant_displays, so a \
         driver can pair an id with the chip that renders it; ids={ids:?} \
         displays={displays:?}"
    );
    let alice_at = displays
        .iter()
        .position(|d| d.as_deref() == Some("alice@nest.test"))
        .expect("alice renders a chip");
    assert_eq!(
        ids[alice_at].as_deref(),
        Some(alice.to_hex().as_str()),
        "the id at alice's own index is alice's actor id, lowercase hex \
         (the spelling `session.actor_id` and `succession.lookup` both use, so a \
         test compares them without re-casing); row={row:?}"
    );
}

/// **The chips' own text is on the row**, index-parallel with the ids beside
/// it, so a driver reads what the app paints instead of inferring it.
///
/// The property this exists for is the one no other field can show: a member
/// seated off an MLS roster carries no handle, and until `TypedAddress::display`
/// gained its elided-id fallback (`value-formatting.md` § Account display label)
/// such a member rendered as a **blank chip**. `participant_actor_ids` shows
/// *that* someone is seated and *who*; only this column shows whether the row
/// says anything at all.
#[test]
fn the_row_carries_the_chip_text_and_a_nameless_member_is_not_blank() {
    let manager = ConversationsManager::new();
    manager.register_backend(Arc::new(MockRailBackend::new(Rail::FaunaMls)));
    let nameless = ActorId([0x5au8; 32]);
    manager
        .ingest_inbound(inbound(
            Rail::FaunaMls,
            TypedAddress::Fauna {
                handle: String::new(),
                actor_id: nameless,
            },
            vec![],
        ))
        .expect("ingest");

    let row = only_row(&manager);
    let ids = str_list(&row, "participant_actor_ids");
    let displays = str_list(&row, "participant_displays");

    assert_eq!(
        ids.len(),
        displays.len(),
        "the two columns are index-parallel; ids={ids:?} displays={displays:?}"
    );
    let at = ids
        .iter()
        .position(|id| id.as_deref() == Some(nameless.to_hex().as_str()))
        .expect("the nameless member holds a slot");
    assert_eq!(
        displays[at].as_deref(),
        Some(fauna_core::format::short_id(&nameless.to_hex()).as_str()),
        "a member with no handle renders as its elided actor id — the blank \
         chip is the defect this column exists to catch; row={row:?}"
    );
}

/// A participant with no actor id serializes as `null` rather than being
/// skipped. Skipping would silently shorten the list and slide every later id
/// onto the wrong chip — an off-by-one that renders as a *wrong person*, not as
/// a missing field, on any mixed-rail thread.
#[test]
fn a_non_fauna_participant_holds_its_slot_as_null() {
    let manager = ConversationsManager::new();
    manager.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    manager
        .ingest_inbound(inbound(
            Rail::Smtp,
            TypedAddress::Email {
                email_address: "someone@host.test".into(),
            },
            vec![],
        ))
        .expect("ingest");

    let row = only_row(&manager);
    let ids = str_list(&row, "participant_actor_ids");
    let detail = manager
        .thread_detail(ThreadId(row["thread_id"].as_str().unwrap().to_string()))
        .expect("detail");

    assert_eq!(
        ids.len(),
        detail.participant_displays.len(),
        "the slot is held, not dropped: ids={ids:?} displays={:?}",
        detail.participant_displays
    );
    assert!(
        ids.iter().all(Option::is_none),
        "an email participant has no actor id — the slot must be null, never an \
         empty string (which a driver would compare equal to a real id it failed \
         to read); ids={ids:?}"
    );
}

/// `data.conversation_sort` names the list's active order in the serde spelling
/// `setSort` already speaks. The rendered rows alone cannot say which order is
/// active: with no thread unread, the unread order and latest-activity are the
/// same rows in the same order, so a driver asserting "this tap produced that
/// order" has to be told which order the tap produced.
#[test]
fn the_active_sort_order_is_published_in_the_set_sort_spelling() {
    use fauna_conversations::state_json::conversation_sort_json;

    let manager = ConversationsManager::new();
    assert_eq!(conversation_sort_json(&manager), "LatestActivity");
    manager.set_sort(SortOrder::OldestFirst);
    assert_eq!(conversation_sort_json(&manager), "OldestFirst");
    manager.set_sort(SortOrder::Unread);
    assert_eq!(conversation_sort_json(&manager), "Unread");
    // The manager-level face the FFI apps publish is the same value, JSON-encoded.
    assert_eq!(manager.conversation_sort_json(), "\"Unread\"");
}
