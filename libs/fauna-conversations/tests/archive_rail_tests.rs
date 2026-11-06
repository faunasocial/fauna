//! The `Archive` rail — the read-only projection of an imported archive's
//! message threads (`docs/goal/behavior/archive-import.md` § What each
//! category becomes, the message-threads row: "read-only records under the
//! `Archive` rail … projected into the conversations list by the manager from
//! `model/threads.cbor`; never written to `__conv`").

use fauna_conversations::backend::{ArchiveMessageRecord, ArchiveThreadRecord};
use fauna_conversations::manager::ARCHIVE_THREAD_ID_PREFIX;
use fauna_conversations::state_json::conversation_threads_json;
use fauna_conversations::*;
use fauna_core::source_glyph::SourceGlyph;

fn message(id: &str, sender: &str, own: bool, at: i64, text: &str) -> ArchiveMessageRecord {
    ArchiveMessageRecord {
        external_id: id.into(),
        sender_name: sender.into(),
        is_owner: own,
        created_at_ms: at,
        text: text.into(),
    }
}

fn friend_one_thread() -> ArchiveThreadRecord {
    ArchiveThreadRecord {
        external_id: "h:thread1".into(),
        platform: "facebook".into(),
        title: None,
        participants: vec!["Friend One".into()],
        messages: vec![
            message("h:m1", "Friend One", false, 1_600_000_000_000, "Ahoj"),
            message("h:m2", "Test Owner", true, 1_600_000_060_000, "Hi back"),
        ],
    }
}

#[test]
fn an_archive_thread_is_listed_on_the_archive_rail_read_only_and_never_unread() {
    let manager = ConversationsManager::new();
    manager.set_archive_threads(vec![friend_one_thread()]);

    let snapshot = manager.snapshot();
    assert_eq!(snapshot.threads.len(), 1);
    let row = &snapshot.threads[0];
    assert_eq!(row.rail, Rail::Archive);
    assert_eq!(
        row.glyph,
        SourceGlyph::Archive,
        "the box glyph the feed badge uses"
    );
    assert_eq!(row.label, "Friend One", "untitled → the other participants");
    assert_eq!(row.snippet, "Hi back");
    assert_eq!(
        row.last_activity_ms, 1_600_000_060_000,
        "dated at the archive's own time"
    );
    assert_eq!(row.unread_count, 0);
    assert!(row.thread_id.0.starts_with(ARCHIVE_THREAD_ID_PREFIX));

    let detail = manager
        .thread_detail(row.thread_id.clone())
        .expect("detail");
    assert_eq!(detail.rail, Rail::Archive);
    assert!(
        !detail.capabilities.can_send,
        "no reply bar on a record of the past"
    );
    assert!(!detail.capabilities.supports_reactions);
    assert!(!detail.capabilities.supports_membership_change);
    assert_eq!(detail.participant_displays, ["Friend One"]);
    assert_eq!(
        detail.participants,
        [TypedAddress::Archive {
            platform: "facebook".into(),
            name: "Friend One".into(),
        }]
    );
    let bodies: Vec<_> = detail
        .messages
        .iter()
        .map(|m| (m.body.as_str(), m.is_own))
        .collect();
    assert_eq!(bodies, [("Ahoj", false), ("Hi back", true)]);
    assert_eq!(detail.messages[0].sender_display, "Friend One");
    assert!(detail.messages.iter().all(|m| !m.can_delete));
}

#[test]
fn an_archive_address_names_nobody_on_fauna_and_reads_as_its_name() {
    let addr = TypedAddress::Archive {
        platform: "instagram".into(),
        name: "friend_two".into(),
    };
    assert_eq!(addr.rail(), Some(Rail::Archive));
    assert_eq!(addr.person_actor_id(), None, "no shadow profile");
    assert_eq!(addr.display(), "friend_two");
    assert_eq!(Rail::Archive.send_wire_kind(), None, "nothing sends on it");
    assert_eq!(Rail::parse(Rail::Archive.as_str()), Rail::Archive);
}

#[test]
fn the_archive_projection_is_searched_sorted_with_the_rest_and_replaced_whole() {
    let manager = ConversationsManager::new();
    let mut group = friend_one_thread();
    group.external_id = "h:thread2".into();
    group.title = Some("Trip planning".into());
    group.participants = vec!["Friend One".into(), "Friend Two".into()];
    group.messages = vec![message(
        "h:m3",
        "Friend Two",
        false,
        1_700_000_000_000,
        "Tickets?",
    )];
    manager.set_archive_threads(vec![friend_one_thread(), group]);

    let labels: Vec<_> = manager
        .snapshot()
        .threads
        .into_iter()
        .map(|t| t.label)
        .collect();
    assert_eq!(
        labels,
        ["Trip planning", "Friend One"],
        "newest activity first"
    );

    manager.set_search_query(Some("tickets".into()));
    let found: Vec<_> = manager
        .snapshot()
        .threads
        .into_iter()
        .map(|t| t.label)
        .collect();
    assert_eq!(
        found,
        ["Trip planning"],
        "a message's words find its thread"
    );
    manager.set_search_query(None);

    manager.set_archive_threads(vec![]);
    assert!(
        manager.snapshot().threads.is_empty(),
        "a re-read replaces, never accumulates"
    );
}

#[test]
fn the_archive_projection_never_enters_the_thread_store_or_its_history() {
    let manager = ConversationsManager::new();
    manager.set_archive_threads(vec![friend_one_thread()]);
    // The store's own readers — what the `__conv` replica, the search index
    // and the peer-anchor harvest walk — see nothing of it.
    assert!(manager.fauna_roster_actors().is_empty());
    let rows = conversation_threads_json(&manager);
    let rows = rows.as_array().expect("rows");
    assert_eq!(
        rows.len(),
        1,
        "the e2e state reader lists it like the page does"
    );
    assert_eq!(rows[0]["rail"], "Archive");
    assert_eq!(rows[0]["message_count"], 2);
}

/// The source the glue registers, answering a fixed read — or failing.
struct FixedSource(Result<Vec<ArchiveThreadRecord>, String>);

#[async_trait::async_trait]
impl fauna_conversations::backend::ArchiveThreadSource for FixedSource {
    async fn archive_threads(&self) -> Result<Vec<ArchiveThreadRecord>, String> {
        self.0.clone()
    }
}

/// The source cell and its re-read live on the manager, not the session —
/// web drives the manager with no session, and every app registers through
/// the same door (`archive-import.md` § What each category becomes).
#[tokio::test]
async fn the_manager_reads_its_registered_archive_source_and_keeps_the_last_read_on_failure() {
    let manager = ConversationsManager::new();
    manager
        .refresh_archive_threads()
        .await
        .expect("no source is a no-op");
    assert!(manager.snapshot().threads.is_empty());

    manager.set_archive_thread_source(std::sync::Arc::new(FixedSource(Ok(vec![
        friend_one_thread(),
    ]))));
    manager.refresh_archive_threads().await.expect("the read");
    assert_eq!(manager.snapshot().threads.len(), 1);
    assert_eq!(manager.snapshot().threads[0].rail, Rail::Archive);

    manager.set_archive_thread_source(std::sync::Arc::new(FixedSource(Err(
        "folder unreadable".into()
    ))));
    let err = manager
        .refresh_archive_threads()
        .await
        .expect_err("a failed read reports");
    assert_eq!(err, "folder unreadable");
    assert_eq!(
        manager.snapshot().threads.len(),
        1,
        "a failed read keeps what the last read listed"
    );
}
