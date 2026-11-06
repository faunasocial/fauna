//! Unread tracking — `docs/goal/ui/conversations.md` § State & data shape →
//! *When a thread is read*. One definition in shared Rust: every app renders
//! `ThreadSummary::unread_count` and none of them decides it.

use fauna_conversations::backends::mock::MockRailBackend;
use fauna_conversations::*;
use std::sync::Arc;

/// Where this run's news starts. Everything stamped below it is history the
/// launch catch-up replayed; everything at or above it arrived this run.
const FLOOR_MS: i64 = 1_000;

fn manager() -> Arc<ConversationsManager> {
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    m.set_launch_floor_for_test(FLOOR_MS);
    m
}

fn mail(sender: &str, subject: &str, body: &str, timestamp_ms: i64) -> RailInboundMessage {
    RailInboundMessage {
        rail: Rail::Smtp,
        sender: TypedAddress::Email {
            email_address: sender.to_string(),
        },
        recipients: vec![TypedAddress::Email {
            email_address: "me@host.test".into(),
        }],
        subject: Some(subject.to_string()),
        body: body.to_string(),
        body_format: BodyFormat::PlainText,
        timestamp_ms,
        message_id: MessageId(format!("msg-{body}")),
        in_reply_to: None,
        attachments: vec![],
        badges: Default::default(),
        legal_takedown_ref: None,
        admin_block_reason: None,
        plane_ref: None,
    }
}

fn unread_of(m: &ConversationsManager, label: &str) -> u32 {
    m.snapshot()
        .threads
        .iter()
        .find(|t| t.label == label)
        .unwrap_or_else(|| panic!("no thread labelled {label:?}"))
        .unread_count
}

fn id_of(m: &ConversationsManager, label: &str) -> ThreadId {
    m.snapshot()
        .threads
        .iter()
        .find(|t| t.label == label)
        .unwrap_or_else(|| panic!("no thread labelled {label:?}"))
        .thread_id
        .clone()
}

#[test]
fn an_inbound_message_in_a_thread_nobody_is_looking_at_is_unread() {
    let m = manager();
    m.ingest_inbound(mail("a@host.test", "alpha", "one", 2_000))
        .unwrap();
    assert_eq!(unread_of(&m, "alpha"), 1);
    m.ingest_inbound(mail("a@host.test", "alpha", "two", 3_000))
        .unwrap();
    assert_eq!(unread_of(&m, "alpha"), 2);
}

#[test]
fn before_positions_arrive_no_native_message_is_unread_and_the_delivery_fills_the_set() {
    // The store is refilled from the nest at every launch; until the seam has
    // delivered positions it cannot tell replayed history from news, so it
    // reports nothing (`conversation-read-state.md` § How the carriers meet
    // the in-memory set, position unknown) — and the first delivery recomputes
    // the whole set from the nest-assigned `seq`, never a clock.
    let m = manager();
    let id = m.restore_channel_slice(&slice(vec![native(1, false), native(2, false)]));
    assert_eq!(unread_in(&m, &id), 0, "position unknown: nothing native");
    let generation = m.set_read_positions(Arc::new(RecordingSeam::default()));
    m.apply_read_positions(generation, vec![(CHANNEL.into(), 1)]);
    assert_eq!(unread_in(&m, &id), 1, "seq 2 — above the position");
}

#[test]
fn the_users_own_message_is_never_unread() {
    let m = manager();
    m.ingest_inbound(mail("a@host.test", "alpha", "theirs", 2_000))
        .unwrap();
    m.inject_own_for_test(mail("a@host.test", "alpha", "mine", 3_000))
        .unwrap();
    assert_eq!(unread_of(&m, "alpha"), 1, "theirs counts, mine never does");
}

#[test]
fn opening_a_thread_reads_it_and_only_it() {
    let m = manager();
    m.ingest_inbound(mail("a@host.test", "alpha", "one", 2_000))
        .unwrap();
    m.ingest_inbound(mail("b@host.test", "beta", "two", 2_500))
        .unwrap();
    m.select_thread(id_of(&m, "alpha"));
    assert_eq!(unread_of(&m, "alpha"), 0);
    assert_eq!(
        unread_of(&m, "beta"),
        1,
        "opening one thread reads no other"
    );
}

#[test]
fn a_message_arriving_in_the_open_thread_is_read_on_arrival() {
    let m = manager();
    m.ingest_inbound(mail("a@host.test", "alpha", "one", 2_000))
        .unwrap();
    assert_eq!(unread_of(&m, "alpha"), 1);
    m.select_thread(id_of(&m, "alpha"));
    m.ingest_inbound(mail("a@host.test", "alpha", "two", 3_000))
        .unwrap();
    assert_eq!(unread_of(&m, "alpha"), 0);
}

#[test]
fn a_thread_the_user_left_collects_unread_again() {
    let m = manager();
    m.ingest_inbound(mail("a@host.test", "alpha", "one", 2_000))
        .unwrap();
    m.select_thread(id_of(&m, "alpha"));
    m.clear_selection();
    m.ingest_inbound(mail("a@host.test", "alpha", "two", 3_000))
        .unwrap();
    assert_eq!(unread_of(&m, "alpha"), 1);
}

#[test]
fn the_new_thread_composer_covers_the_selected_thread_so_it_is_not_being_read() {
    // `start_new_conversation` keeps `selected` (the draft switch is
    // reversible) but the detail pane shows the composer, not the thread.
    let m = manager();
    m.ingest_inbound(mail("a@host.test", "alpha", "one", 2_000))
        .unwrap();
    m.select_thread(id_of(&m, "alpha"));
    m.start_new_conversation();
    m.ingest_inbound(mail("a@host.test", "alpha", "two", 3_000))
        .unwrap();
    assert_eq!(unread_of(&m, "alpha"), 1);
}

#[test]
fn mark_read_reads_a_thread_without_opening_it() {
    let m = manager();
    m.ingest_inbound(mail("a@host.test", "alpha", "one", 2_000))
        .unwrap();
    let id = id_of(&m, "alpha");
    assert_eq!(unread_of(&m, "alpha"), 1);
    m.mark_read(id);
    assert_eq!(unread_of(&m, "alpha"), 0);
    assert_eq!(m.snapshot().selected_thread_id, None);
}

#[test]
fn the_unread_order_puts_unread_threads_first_newest_first_within_each_group() {
    let m = manager();
    m.ingest_inbound(mail("a@host.test", "oldest", "one", 2_000))
        .unwrap();
    m.ingest_inbound(mail("b@host.test", "middle", "two", 3_000))
        .unwrap();
    m.ingest_inbound(mail("c@host.test", "newest", "three", 4_000))
        .unwrap();
    m.mark_read(id_of(&m, "middle"));
    m.mark_read(id_of(&m, "newest"));
    m.set_sort(SortOrder::Unread);
    let labels: Vec<String> = m
        .snapshot()
        .threads
        .iter()
        .map(|t| t.label.clone())
        .collect();
    assert_eq!(labels, ["oldest", "newest", "middle"]);
}

#[test]
fn an_identity_change_carries_no_unread_state_across() {
    let m = manager();
    let id = m.restore_channel_slice(&slice(vec![native(1, false)]));
    let generation = m.set_read_positions(Arc::new(RecordingSeam::default()));
    m.apply_read_positions(generation, vec![]);
    assert_eq!(unread_in(&m, &id), 1);
    m.clear_for_identity_change();
    let id = m.restore_channel_slice(&slice(vec![native(1, false)]));
    assert_eq!(
        unread_in(&m, &id),
        0,
        "the next identity starts position-unknown, with nothing unread"
    );
}

#[test]
fn the_published_state_row_carries_the_count() {
    let m = manager();
    m.ingest_inbound(mail("a@host.test", "alpha", "one", 2_000))
        .unwrap();
    let state = fauna_conversations::state_json::conversation_threads_json(&m);
    let row = state
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["label"] == "alpha"))
        .expect("alpha's state row");
    assert_eq!(row["unread_count"], 1);
}

// ── The fauna-native rail's synced read positions ────────────────────────────
// `docs/goal/behavior/conversation-read-state.md` § How the carriers meet the
// in-memory set. Every native message below carries a timestamp BEFORE the
// launch floor: were the floor consulted, none of them could ever be unread,
// so each assertion that one is proves the channel position decided it — no
// clock anywhere in the rule.

use fauna_conversations::backend::ReadPositions;
use fauna_conversations::store::history::ChannelHistorySlice;
use std::sync::Mutex;

const CHANNEL: &str = "c4a1c4a1c4a1c4a1c4a1c4a1c4a1c4a1c4a1c4a1c4a1c4a1c4a1c4a1c4a1c4a1";

fn native(seq: u64, own: bool) -> MessageSnapshot {
    MessageSnapshot {
        message_id: MessageId(format!("conv:{CHANNEL}:{seq}")),
        sender: TypedAddress::Email {
            email_address: if own {
                "me@host.test"
            } else {
                "peer@host.test"
            }
            .into(),
        },
        sender_display: String::new(),
        body: format!("message {seq}"),
        document: fauna_core::render::RenderDocument::default(),
        timestamp_ms: FLOOR_MS - 500 + seq as i64,
        subject_line: None,
        badges: MessageBadges::default(),
        reply_to: None,
        reactions: vec![],
        deleted: false,
        is_own: own,
        legal_takedown_ref: None,
        admin_block_reason: None,
        labels: vec![],
        plane_ref: None,
        can_delete: false,
        undelivered: false,
        warmup_hold: None,
        scan_result_lines: Vec::new(),
        delivered_to: None,
        withheld: None,
    }
}

/// A channel slice as the launch restore hands it over — the relaunch shape.
fn slice(messages: Vec<MessageSnapshot>) -> ChannelHistorySlice {
    ChannelHistorySlice {
        channel_id_hex: CHANNEL.to_string(),
        label: "peer".to_string(),
        flavor: ThreadFlavor::OneToOne,
        participants: vec![],
        messages,
        ..Default::default()
    }
}

fn unread_in(m: &ConversationsManager, id: &ThreadId) -> u32 {
    m.snapshot()
        .threads
        .iter()
        .find(|t| &t.thread_id == id)
        .expect("the channel's thread")
        .unread_count
}

/// What a seam was asked to raise, in order.
#[derive(Default)]
struct RecordingSeam(Mutex<Vec<(String, u64)>>);

impl ReadPositions for RecordingSeam {
    fn raise(&self, channel_id_hex: &str, through: u64) {
        self.0
            .lock()
            .unwrap()
            .push((channel_id_hex.to_string(), through));
    }
}

#[test]
fn a_relaunch_given_a_position_below_the_newest_message_reports_exactly_those_above_it() {
    let m = manager();
    let id = m.restore_channel_slice(&slice((1..=5).map(|s| native(s, false)).collect()));
    assert_eq!(
        unread_in(&m, &id),
        0,
        "position unknown: nothing native is unread"
    );
    let seam = Arc::new(RecordingSeam::default());
    let generation = m.set_read_positions(seam);
    assert!(m.apply_read_positions(generation, vec![(CHANNEL.into(), 3)]));
    assert_eq!(unread_in(&m, &id), 2, "seqs 4 and 5 — above the position");
}

#[test]
fn a_channel_with_no_marker_is_unread_from_its_first_message_once_positions_are_known() {
    // The declared one-time transition: an absent entry is position 0.
    let m = manager();
    let id = m.restore_channel_slice(&slice(vec![native(1, false), native(2, true)]));
    let generation = m.set_read_positions(Arc::new(RecordingSeam::default()));
    m.apply_read_positions(generation, vec![]);
    assert_eq!(
        unread_in(&m, &id),
        1,
        "the peer's message; never the own one"
    );
}

#[test]
fn a_later_arrival_is_unread_iff_its_seq_is_above_the_position() {
    let m = manager();
    let id = m.restore_channel_slice(&slice(vec![native(1, false), native(2, false)]));
    let generation = m.set_read_positions(Arc::new(RecordingSeam::default()));
    m.apply_read_positions(generation, vec![(CHANNEL.into(), 4)]);
    assert_eq!(unread_in(&m, &id), 0);
    // A late copy at or below the position is read; one above it is news —
    // whatever its (pre-floor) timestamp says.
    m.restore_channel_slice(&slice(vec![native(3, false), native(5, false)]));
    assert_eq!(unread_in(&m, &id), 1, "only seq 5");
}

#[test]
fn a_raise_from_another_device_removes_what_it_covers_and_never_lowers() {
    let m = manager();
    let id = m.restore_channel_slice(&slice((1..=4).map(|s| native(s, false)).collect()));
    let generation = m.set_read_positions(Arc::new(RecordingSeam::default()));
    m.apply_read_positions(generation, vec![(CHANNEL.into(), 1)]);
    assert_eq!(unread_in(&m, &id), 3);
    m.apply_read_positions(generation, vec![(CHANNEL.to_uppercase(), 3)]);
    assert_eq!(unread_in(&m, &id), 1, "a raise to 3 leaves seq 4");
    m.apply_read_positions(generation, vec![(CHANNEL.into(), 2)]);
    assert_eq!(
        unread_in(&m, &id),
        1,
        "a stale delivery cannot move it back"
    );
}

#[test]
fn reading_a_native_thread_raises_its_marker_to_the_highest_seq_it_holds() {
    let m = manager();
    let id = m.restore_channel_slice(&slice(vec![
        native(1, false),
        native(2, true),
        native(3, false),
    ]));
    let seam = Arc::new(RecordingSeam::default());
    let generation = m.set_read_positions(seam.clone());
    m.apply_read_positions(generation, vec![]);
    m.select_thread(id.clone());
    assert_eq!(unread_in(&m, &id), 0);
    assert_eq!(*seam.0.lock().unwrap(), vec![(CHANNEL.to_string(), 3)]);
    // Re-selecting a thread with nothing to read raises nothing.
    m.select_thread(id.clone());
    assert_eq!(seam.0.lock().unwrap().len(), 1);
    // A delivery older than the read (the store has not caught up) does not
    // re-present the thread as unread: the local read joined the position.
    m.clear_selection();
    m.apply_read_positions(generation, vec![(CHANNEL.into(), 0)]);
    assert_eq!(unread_in(&m, &id), 0);
}

#[test]
fn a_mail_thread_raises_no_marker_and_positions_leave_it_alone() {
    let m = manager();
    let seam = Arc::new(RecordingSeam::default());
    let generation = m.set_read_positions(seam.clone());
    m.ingest_inbound(mail("a@host.test", "alpha", "one", 2_000))
        .unwrap();
    m.apply_read_positions(generation, vec![]);
    assert_eq!(unread_of(&m, "alpha"), 1, "mail keeps its own rule");
    m.select_thread(id_of(&m, "alpha"));
    assert!(seam.0.lock().unwrap().is_empty());
}

#[test]
fn an_identity_change_retires_the_seam_and_refuses_its_deliveries() {
    let m = manager();
    let seam = Arc::new(RecordingSeam::default());
    let generation = m.set_read_positions(seam.clone());
    m.clear_for_identity_change();
    m.set_launch_floor_for_test(FLOOR_MS);
    assert!(
        !m.apply_read_positions(generation, vec![]),
        "the outgoing account's positions never land on the incoming one"
    );
    // Back in the position-unknown arm: nothing native is unread.
    let id = m.restore_channel_slice(&slice(vec![native(1, false)]));
    assert_eq!(unread_in(&m, &id), 0, "the replay is history again");
    m.restore_channel_slice(&slice(vec![native(2, false)]));
    assert_eq!(
        unread_in(&m, &id),
        0,
        "and so is a live arrival, until positions"
    );
    m.select_thread(id);
    assert!(
        seam.0.lock().unwrap().is_empty(),
        "a retired seam hears nothing"
    );
}

/// The new-message banner keys on the count this file pins, not on the
/// sender's stamp (`docs/goal/ui/conversations.md` § Where logic lives, rule
/// 2): the real store's snapshot, projected the way every app projects it
/// (`ThreadActivity::from_summary`) and fed to the shared tracker, banners a
/// live arrival its sender stamped below the thread's newest — the arrival
/// the unread count takes — exactly once.
#[test]
fn a_live_arrival_stamped_below_its_threads_newest_banners_as_it_counts() {
    let m = manager();
    let generation = m.set_read_positions(Arc::new(RecordingSeam::default()));
    m.apply_read_positions(generation, vec![]);
    let tracker = MessageNotificationTracker::new();
    let tick = || {
        let snap = m.snapshot();
        let threads = snap
            .threads
            .iter()
            .map(ThreadActivity::from_summary)
            .collect();
        tracker.diff(
            threads,
            snap.selected_thread_id.clone(),
            snap.launch_floor_ms,
        )
    };
    let stamped = |seq: u64, ts: i64| {
        let mut msg = native(seq, false);
        msg.timestamp_ms = ts;
        msg
    };
    let id = m.restore_channel_slice(&slice(vec![stamped(1, FLOOR_MS + 5_000)]));
    assert!(tick().is_empty(), "the first non-empty snapshot seeds");
    m.restore_channel_slice(&slice(vec![stamped(2, FLOOR_MS + 4_000)]));
    assert_eq!(unread_in(&m, &id), 2, "the unread count takes the arrival");
    assert_eq!(tick().len(), 1, "and the banner fires for it");
    assert!(tick().is_empty(), "exactly once");
}

/// The e2e inject seam's arrivals are news on every rail. A FaunaMls inject
/// carries no channel position, so the carrier rule alone would never count
/// it (`conversation-read-state.md` § How the carriers meet the in-memory
/// set) — and a journey that watches unread rise, the banner's among them,
/// would watch nothing. The seam says so itself; no clock decides it.
#[test]
fn an_injected_inbound_message_is_unread_on_the_native_rail_too() {
    let m = manager();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::FaunaMls)));
    let inject = |subject: &str, extra: serde_json::Value| {
        let mut p = serde_json::json!({
            "rail": "FaunaMls",
            "sender": "peer",
            "subject": subject,
            "body": format!("{subject} arrived"),
        });
        p.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        m.inject_inbound_from_test_payload(p.as_object().unwrap())
            .unwrap();
    };
    inject("plain", serde_json::json!({}));
    inject(
        "relabelled",
        serde_json::json!({ "force_subject_change": "renamed" }),
    );
    let threads: Vec<(ThreadId, u32)> = m
        .snapshot()
        .threads
        .iter()
        .map(|t| (t.thread_id.clone(), t.unread_count))
        .collect();
    assert_eq!(threads.len(), 2, "two threads: {threads:?}");
    assert!(
        threads.iter().all(|(_, n)| *n == 1),
        "every inject is unread: {threads:?}"
    );

    // The open thread still reads what arrives in it, and an own inject never counts.
    let open = threads[0].0.clone();
    let subject = m
        .snapshot()
        .threads
        .iter()
        .find(|t| t.thread_id == open)
        .unwrap()
        .label
        .clone();
    m.select_thread(open.clone());
    inject(&subject, serde_json::json!({}));
    inject(&subject, serde_json::json!({ "is_own": true }));
    assert_eq!(
        unread_in(&m, &open),
        0,
        "the open thread is read on arrival"
    );
}
