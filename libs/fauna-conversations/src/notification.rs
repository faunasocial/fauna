//! New-message OS-toast diff decision — the shared *when / for-whom* half of
//! DM notifications (`docs/goal/ui/conversations.md` § Where logic lives).
//!
//! OS notifications split into two halves: the **decision** — which threads warrant
//! a new-message toast — and the **firing** — handing a label to the platform's
//! native toast API. The *firing* is genuinely client glue (a per-platform native
//! call); the *decision* is pure, deterministic, and identical on every app, so
//! it lives once here (priority #2/#4) instead of being re-derived per client. This
//! mirrors the same refinement the doc already applies to the markdown toolbar-wrap
//! *rule*, the decoration-map *computation*, and [`crate::address::TypedAddress::display`]:
//! the pure decision is shared Rust; the platform splice/apply/fire is client glue.
//!
//! Before this lift the decision was duplicated bit-for-bit in C#
//! (`FaunaApp.Core/Notifications/MessageNotificationTracker.cs`) and Rust
//! (`apps/fauna-linux/src/conversations/notification_tracker.rs`), and the two had
//! already drifted (the seed condition — see below). Consumed natively over UniFFI
//! (windows/macos/ios/android) and directly as a Rust dep (linux).
//!
//! **The decision** (stateful across snapshot ticks — feed it every
//! [`crate::ConversationsManager`] snapshot): seed silently (no toast for the threads
//! already present at login), then fire when a thread's `unread_count` **rises**
//! while its newest activity (`last_activity_ms`) is **at or past the run's launch
//! floor** — whether the thread was seen before (its count increased) or is first
//! seen now with unread messages (a brand-new thread) — and suppress the
//! selected/focused thread (the user is already looking at it). An **empty**
//! snapshot neither seeds nor notifies — an app-lifetime observer attached before
//! login sees empty ticks first, and seeding on those would make the whole post-login
//! thread load look "new" (a toast storm). Seeding on the first *non-empty* snapshot
//! is a strict superset of seeding on the first *call* (identical when the first
//! snapshot already has threads).
//!
//! **Why the floor (ratified 2026-09-22).** The rails load asynchronously, and the
//! mail rail re-drains the whole mailbox from UID 0 on every launch, record by record
//! (`mail-app-surface.md` § Inbound client receive). Seeding once on the first
//! non-empty snapshot therefore seeds on whichever rail lands first, and every mail
//! thread the drain reaches afterwards used to read as brand-new — a sign-in raised a
//! banner for days-old mail whenever mail was the slower rail. The floor is the
//! **same** one the unread count reads (`ConversationsSnapshot::launch_floor_ms`,
//! `store::threads`; `conversations.md` § State & data shape → *When a thread is
//! read*): a message stamped before this run began is history, so the banner and
//! `dm-unread-indicator` can never disagree about where news starts. It is a stamp
//! comparison against the device clock, with the declared under-reports that
//! section owns — a message that arrived while the app was closed raises no banner
//! (by design: the banner is for what arrives while the app runs; the indicator
//! carries the rest), a device clock running ahead misses banners for that long
//! after launch, and a thread whose every stamp is sender-backdated below the
//! floor, on a rail that carries the sender's own time, raises none. The seed rule
//! stands on its own: the first non-empty snapshot is silent whatever its stamps
//! say.
//!
//! **Why the unread count is the key (2026-09-27).** News used to be a rise of
//! `last_activity_ms` — the stamp of the thread's last-appended message, which on
//! every rail but mail is the sender's own claim — so a live arrival stamped below
//! its thread's newest (backdated, or from a sender whose clock runs behind the
//! previous one's) raised no banner although the unread count took it. The count
//! follows carriers the sender does not choose (the nest-assigned `seq`, mail's
//! `\Seen`; `conversation-read-state.md` § How the carriers meet the in-memory
//! set), never counts the user's own messages, and is the very number
//! `dm-unread-indicator` renders — so past the floor the banner and the indicator
//! agree message for message. Its two declared edges (`conversations.md` § Where
//! logic lives, rule 2): a mail client marking an old message unread on a thread
//! whose newest is past the floor raises the count, and so a banner; and a read
//! from another device landing in the same tick as an arrival can leave the
//! count where it stood, and so no banner.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::snapshot::ThreadSummary;
use crate::thread::ThreadId;

/// One thread's notification-facing activity, projected from a snapshot
/// [`ThreadSummary`] — the `label` (+ `snippet`) the toast renders, the
/// `unread_count` the diff keys on and the `last_activity_ms` it gates by the
/// launch floor. Kept minimal (free of the rest of `ThreadSummary`) so the
/// decision is deterministically unit-testable. Every app projects it through
/// [`ThreadActivity::from_summary`] (over UniFFI,
/// [`thread_activity_from_summary`]) rather than naming fields itself, so a
/// field the decision grows reaches all 7 apps with no per-app edit.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    all(feature = "client-display", feature = "uniffi"),
    derive(uniffi::Record)
)]
pub struct ThreadActivity {
    pub thread_id: ThreadId,
    pub label: String,
    pub snippet: String,
    pub last_activity_ms: i64,
    pub unread_count: u32,
}

impl ThreadActivity {
    /// Project a live snapshot [`ThreadSummary`] into the notification-facing view.
    pub fn from_summary(t: &ThreadSummary) -> Self {
        Self {
            thread_id: t.thread_id.clone(),
            label: t.label.clone(),
            snippet: t.snippet.clone(),
            last_activity_ms: t.last_activity_ms,
            unread_count: t.unread_count,
        }
    }
}

/// [`ThreadActivity::from_summary`], exported over UniFFI — the one projection
/// the Swift, Kotlin and C# observers call (a UniFFI record carries no methods,
/// so the shared constructor is a free function there; linux, tui and wasm call
/// the method directly).
#[cfg(all(feature = "client-display", feature = "uniffi"))]
#[uniffi::export]
pub fn thread_activity_from_summary(summary: ThreadSummary) -> ThreadActivity {
    ThreadActivity::from_summary(&summary)
}

#[derive(Default)]
struct TrackerState {
    unread_count: HashMap<ThreadId, u32>,
    /// Flips on the first **non-empty** snapshot (see module docs); empty
    /// pre-login snapshots neither seed nor notify.
    seeded: bool,
}

/// Decides which threads warrant a new-message OS toast by diffing successive
/// conversations snapshots. Stateful across calls — feed it every snapshot tick.
///
/// Interior-mutable (`Mutex`) so the single shared shape serves both a UniFFI
/// Object (whose exported methods take `&self`) and a direct Rust caller, and so
/// the native observer — which the manager may drive from arbitrary threads — needs
/// no external lock. (The cost is a single uncontended lock per tick.)
#[cfg_attr(
    all(feature = "client-display", feature = "uniffi"),
    derive(uniffi::Object)
)]
pub struct MessageNotificationTracker {
    state: Mutex<TrackerState>,
}

impl Default for MessageNotificationTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg_attr(all(feature = "client-display", feature = "uniffi"), uniffi::export)]
impl MessageNotificationTracker {
    /// A fresh, unseeded tracker.
    #[cfg_attr(
        all(feature = "client-display", feature = "uniffi"),
        uniffi::constructor
    )]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(TrackerState::default()),
        }
    }

    /// Feed the latest snapshot's `threads`, the currently-selected thread id and
    /// the snapshot's `launch_floor_ms`; returns the activities of threads with a
    /// NEW inbound message since the previous call — a rise of the thread's
    /// `unread_count`, never of the sender-stamped `last_activity_ms` (module
    /// docs). Empty on the seeding (first
    /// non-empty) snapshot — pre-existing threads at login are not "new" — empty
    /// for unchanged ticks (selection / compose changes), empty for a thread whose
    /// newest activity predates the floor however it reached this tick (a slower
    /// rail's launch re-drain is history, not news — module docs), and the
    /// selected/focused thread is suppressed (the user is already looking at it).
    ///
    /// Pass the floor straight off the same snapshot the `threads` came from
    /// (`ConversationsSnapshot::launch_floor_ms`): it is the store's, not the
    /// tracker's, so the unread count and this decision read one value.
    pub fn diff(
        &self,
        threads: Vec<ThreadActivity>,
        selected: Option<ThreadId>,
        launch_floor_ms: i64,
    ) -> Vec<ThreadActivity> {
        // An empty snapshot (e.g. the pre-login ticks an app-lifetime observer sees)
        // neither seeds nor notifies — see module docs.
        if threads.is_empty() {
            return Vec::new();
        }
        let mut state = self.state.lock().expect("tracker mutex poisoned");
        let was_seeded = state.seeded;
        let mut to_notify = Vec::new();
        for t in threads {
            // A first-seen thread "rises" from zero: a brand-new thread with
            // an unread message is news, one the user started is not.
            let prev = state.unread_count.get(&t.thread_id).copied().unwrap_or(0);
            let rose = t.unread_count > prev;
            // News is a rise of the unread count while the thread's newest
            // activity is at or past the floor. A rise below it is the launch
            // re-drain paging an old thread's unread history in (record by
            // record, so the same thread rises across several ticks); a thread
            // first seen with old activity is that same history landing on a
            // slower rail. Neither is a new message.
            let is_new = rose && t.last_activity_ms >= launch_floor_ms;
            state
                .unread_count
                .insert(t.thread_id.clone(), t.unread_count);

            if !was_seeded {
                continue; // first non-empty call seeds, never notifies
            }
            if !is_new {
                continue; // no new activity this tick
            }
            if selected.as_ref() == Some(&t.thread_id) {
                continue; // focus suppression — the user is viewing this thread
            }
            to_notify.push(t);
        }
        state.seeded = true;
        to_notify
    }
}

// ---------------------------------------------------------------------------
// The fired-banner log — the e2e witness half of `conversations` outcome 11.
// ---------------------------------------------------------------------------

/// The banners this **process** actually handed to the platform's toast API,
/// plus the diff-tick counters that make a *negative* read of that list sound.
/// The shared derivation behind `fauna_e2e_agent::MESSAGE_BANNERS_KEY`, so every
/// app's leg is three calls plus one getter read (priority #2) instead of its own
/// bookkeeping in seven languages.
///
/// **Why the record lives beside the tracker rather than inside it.** The tracker
/// *decides*; the app *fires*, and the two can disagree — glue that swallowed the
/// decision (a stale handle, a platform permission never granted, a suppression
/// rule of its own) would leave [`MessageNotificationTracker::diff`]'s return
/// value claiming a banner went out while the user saw nothing. So the log is
/// appended at the **firing site**, after every suppression the app applies, and
/// means "a banner was raised for this thread", never "a banner was decided".
///
/// **Why there are two counters.** `completed > started_at_plant_time` is the
/// only sound barrier for the negative assertion — *this message raises NO
/// banner* — for the reason `fauna_e2e_agent::ALERT_SWEEP_PASSES_KEY` states in
/// full: a tick that merely *finished* after the plant may have read its snapshot
/// before it, and an app is free to drive its observer from more than one place.
/// Bump [`banner_pass_started`] before reading the snapshot and
/// [`banner_pass_completed`] after the tick's last fire, and the pigeonhole holds
/// without anyone counting the app's observers.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
mod banner_log {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicU64;

    use crate::thread::ThreadId;

    pub(super) static STARTED: AtomicU64 = AtomicU64::new(0);
    pub(super) static COMPLETED: AtomicU64 = AtomicU64::new(0);
    pub(super) static FIRED: Mutex<Vec<(ThreadId, String)>> = Mutex::new(Vec::new());
}

// The UniFFI face of the four calls below — the apple leg's whole obligation
// (`docs/goal/ui/conversations.md` § Where logic lives). linux consumes this
// crate as a Rust dep and web goes through wasm, so neither needed an export;
// macos/ios reach the log only over UniFFI, and their firing site is Swift.
//
// Two gates, deliberately different, per `docs/goal/architecture/
// e2e-automation-surface-gating.md` § The convention — the same pair
// `ConversationsManager`'s e2e injection seams carry, and for the same reasons:
// **visibility** keeps the `debug_assertions` arm (so an in-process Rust
// consumer's plain debug build reaches these without naming the feature), while
// **the `uniffi::export` cfg keys on the FEATURE ONLY, never the profile**, so
// the generated Kotlin/Swift/C#/Go faces stay a pure function of the feature set
// and a profile change owes no binding regen. Do not merge the two gates.
/// Bump the diff-tick **start** counter — call immediately before reading the
/// snapshot the [`MessageNotificationTracker::diff`] tick will feed on.
///
/// A no-op in a release build: convention 15 gates the seam's visibility, while
/// the app's call site is plumbing that compiles unconditionally, so the seam
/// ships a same-signature release twin rather than pushing a `cfg` into glue.
///
/// **That holds for Rust callers only** (linux, tui). The release twin is not
/// exported over UniFFI — "no surface" — so a Swift/Kotlin/C# call site finds no
/// such symbol in the production FFI flavor and must sit behind its platform's
/// debug gate itself (apple: `#if DEBUG`, as `MessageBannerObserver` does); a
/// call left ungated compiles under a debug build and breaks only the release one.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[cfg_attr(all(feature = "uniffi", feature = "test-helpers"), uniffi::export)]
pub fn banner_pass_started() {
    banner_log::STARTED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// Release twin of [`banner_pass_started`] — same signature, no surface.
#[cfg(not(any(test, debug_assertions, feature = "test-helpers")))]
#[inline]
pub fn banner_pass_started() {}

/// Bump the diff-tick **completion** counter — call after the tick's last banner
/// has been handed to the platform (and on a tick that fired none).
///
/// A no-op in a release build; see [`banner_pass_started`], whose comment also
/// owns the two cfg gates repeated here.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[cfg_attr(all(feature = "uniffi", feature = "test-helpers"), uniffi::export)]
pub fn banner_pass_completed() {
    banner_log::COMPLETED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// Release twin of [`banner_pass_completed`] — same signature, no surface.
#[cfg(not(any(test, debug_assertions, feature = "test-helpers")))]
#[inline]
pub fn banner_pass_completed() {}

/// Record that a banner for `thread_id` was **fired** — call at the firing site,
/// after every suppression the app applies, never at the decision site.
///
/// A no-op in a release build; see [`banner_pass_started`], whose comment also
/// owns the two cfg gates repeated here.
///
/// Takes both arguments **by value** so the one signature serves the Rust
/// callers and the UniFFI face alike: `ThreadId` is a `custom_type!` lowering to
/// `String`, which UniFFI carries by value, never behind a reference.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[cfg_attr(all(feature = "uniffi", feature = "test-helpers"), uniffi::export)]
pub fn record_fired_banner(thread_id: ThreadId, label: String) {
    banner_log::FIRED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((thread_id, label));
}

/// Release twin of [`record_fired_banner`] — same signature, no surface.
#[cfg(not(any(test, debug_assertions, feature = "test-helpers")))]
#[inline]
pub fn record_fired_banner(_thread_id: ThreadId, _label: String) {}

/// Build `fauna_e2e_agent::MESSAGE_BANNERS_KEY`'s value —
/// `{"started": N, "completed": M, "fired": [{"thread_id", "label"}, …]}`.
///
/// `fired` is append-only for the process lifetime and ordered by firing, so the
/// reader's assertions are latency-independent (convention 14): a positive test
/// waits for its thread to appear, a negative one waits `completed` past the
/// `started` it read at plant time and then asserts the list did not grow.
///
/// Only an app's own already-gated state builder calls this, so — unlike the
/// three recorders above — it has no release twin: there, it does not exist.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub fn message_banners_json() -> serde_json::Value {
    use std::sync::atomic::Ordering;

    let fired: Vec<serde_json::Value> = banner_log::FIRED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(thread_id, label)| serde_json::json!({ "thread_id": thread_id.0, "label": label }))
        .collect();
    serde_json::json!({
        "started": banner_log::STARTED.load(Ordering::SeqCst),
        "completed": banner_log::COMPLETED.load(Ordering::SeqCst),
        "fired": fired,
    })
}

/// [`message_banners_json`] as JSON **text** — the UniFFI face of that one
/// derivation, for the apps whose state builder is not Rust.
///
/// A separate name rather than a changed return type because UniFFI has no JSON
/// value type: the Rust consumers (linux, tui) splice the `Value` straight into
/// the state object they are already building, while macos/ios re-parse this
/// string — the identical passthrough idiom their sibling observables already use
/// (`ConversationsSession::conv_receive_cycles_json`, `feed_reloads_json`).
/// Neither side re-derives the shape, which is the whole point.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[cfg_attr(all(feature = "uniffi", feature = "test-helpers"), uniffi::export)]
pub fn message_banners_json_text() -> String {
    message_banners_json().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `ThreadActivity` with the given id, newest activity and unread
    /// count (label defaults to `label-<id>`; snippet is irrelevant to the diff
    /// decision).
    fn t(id: &str, activity_ms: i64, unread: u32) -> ThreadActivity {
        t_lbl(id, activity_ms, unread, &format!("label-{id}"))
    }

    fn t_lbl(id: &str, activity_ms: i64, unread: u32, label: &str) -> ThreadActivity {
        ThreadActivity {
            thread_id: ThreadId(id.to_string()),
            label: label.to_string(),
            snippet: String::new(),
            last_activity_ms: activity_ms,
            unread_count: unread,
        }
    }

    fn labels(v: &[ThreadActivity]) -> Vec<&str> {
        v.iter().map(|a| a.label.as_str()).collect()
    }

    #[test]
    fn diff_seeds_silently_on_first_call() {
        let tracker = MessageNotificationTracker::new();
        // First snapshot at login carries pre-existing threads — none are "new",
        // unread or not.
        let r = tracker.diff(vec![t("a", 100, 2), t("b", 200, 0)], None, 0);
        assert!(r.is_empty());
    }

    #[test]
    fn diff_notifies_when_thread_unread_count_rises() {
        let tracker = MessageNotificationTracker::new();
        tracker.diff(vec![t("a", 100, 0)], None, 0); // seed
        let r = tracker.diff(vec![t_lbl("a", 150, 1, "Alice")], None, 0);
        assert_eq!(labels(&r), vec!["Alice"]);
    }

    #[test]
    fn diff_suppresses_selected_thread() {
        let tracker = MessageNotificationTracker::new();
        let a = ThreadId("a".to_string());
        tracker.diff(vec![t("a", 100, 0)], Some(a.clone()), 0); // seed
        // A new unread message on the thread the user is viewing → no toast.
        let r = tracker.diff(vec![t("a", 150, 1)], Some(a), 0);
        assert!(r.is_empty());
    }

    #[test]
    fn diff_notifies_for_brand_new_thread_after_seed() {
        let tracker = MessageNotificationTracker::new();
        tracker.diff(vec![t("a", 100, 0)], None, 0); // seed
        let r = tracker.diff(vec![t("a", 100, 0), t_lbl("b", 50, 1, "Bob")], None, 0);
        assert_eq!(labels(&r), vec!["Bob"]);
    }

    #[test]
    fn diff_does_not_notify_on_unchanged_tick() {
        let tracker = MessageNotificationTracker::new();
        let a = ThreadId("a".to_string());
        tracker.diff(vec![t("a", 100, 1), t("b", 200, 3)], None, 0); // seed
        // An observer tick from a selection/compose change (no new message) must
        // not fire a spurious toast.
        let r = tracker.diff(vec![t("a", 100, 1), t("b", 200, 3)], Some(a), 0);
        assert!(r.is_empty());
    }

    /// The app-lifetime observer attaches before login, so the first ticks are
    /// empty. They must neither seed nor notify — the first **non-empty** snapshot
    /// seeds (post-login load is not a toast storm).
    #[test]
    fn diff_empty_pre_login_ticks_do_not_seed() {
        let tracker = MessageNotificationTracker::new();
        assert!(tracker.diff(vec![], None, 0).is_empty()); // pre-login empty tick
        assert!(tracker.diff(vec![], None, 0).is_empty()); // still empty — still unseeded
        // First non-empty snapshot (threads loaded post-login) seeds silently.
        assert!(tracker.diff(vec![t("a", 100, 1)], None, 0).is_empty());
        // A genuinely new message after the seed now fires.
        let r = tracker.diff(vec![t_lbl("a", 150, 2, "Al")], None, 0);
        assert_eq!(labels(&r), vec!["Al"]);
    }

    /// A thread a slower rail delivers AFTER the seed — a mail thread the
    /// launch re-drain reaches once the native rail has already seeded — is
    /// history when its newest activity predates the launch floor: it seeds
    /// silently, whichever snapshot carries it, however much of it is unread
    /// (mail's `\Seen` consults no floor). Only a first-seen thread whose
    /// activity is at or past the floor is news.
    #[test]
    fn diff_thread_first_seen_after_seed_with_activity_below_floor_is_history() {
        const FLOOR: i64 = 1_000;
        let tracker = MessageNotificationTracker::new();
        tracker.diff(vec![t("a", 2_000, 0)], None, FLOOR); // seed
        // Old unread mail, drained late: stamped before this run began.
        let r = tracker.diff(
            vec![t("a", 2_000, 0), t_lbl("old", 500, 1, "Old mail")],
            None,
            FLOOR,
        );
        assert!(r.is_empty(), "old mail bannered: {:?}", labels(&r));
        // A brand-new thread stamped at/after the floor still fires.
        let r = tracker.diff(
            vec![
                t("a", 2_000, 0),
                t("old", 500, 1),
                t_lbl("new", 1_000, 1, "New"),
            ],
            None,
            FLOOR,
        );
        assert_eq!(labels(&r), vec!["New"]);
    }

    /// The re-drain pages a mailbox from UID 0 and ingests record by record, so
    /// an old thread's unread count RISES across several ticks while every
    /// stamp is still history. A rise while the thread's newest stays below
    /// the floor is not news; the rise that brings a message past it is.
    #[test]
    fn diff_unread_rising_below_the_floor_is_history_crossing_it_is_news() {
        const FLOOR: i64 = 1_000;
        let tracker = MessageNotificationTracker::new();
        tracker.diff(vec![t("a", 2_000, 0)], None, FLOOR); // seed
        tracker.diff(vec![t("a", 2_000, 0), t("m", 100, 1)], None, FLOOR); // page 1: silent seed
        let r = tracker.diff(vec![t("a", 2_000, 0), t("m", 200, 2)], None, FLOOR); // page 2
        assert!(
            r.is_empty(),
            "a rise below the floor bannered: {:?}",
            labels(&r)
        );
        let r = tracker.diff(
            vec![t("a", 2_000, 0), t_lbl("m", 1_500, 3, "Live")],
            None,
            FLOOR,
        );
        assert_eq!(labels(&r), vec!["Live"]);
    }

    /// Rule 2 keys news on the unread count, which follows the nest-assigned
    /// carriers (`seq`, `\Seen`) rather than the sender's stamp
    /// (`conversations.md` § Where logic lives): a live message stamped below
    /// its thread's newest — backdated by its sender, or from a sender whose
    /// clock runs behind the previous one's — LOWERS `last_activity_ms` (the
    /// store's newest is its last-appended message), yet raises exactly one
    /// banner, as it raises the unread count.
    #[test]
    fn diff_a_live_arrival_stamped_below_its_threads_newest_raises_a_banner() {
        const FLOOR: i64 = 1_000;
        let tracker = MessageNotificationTracker::new();
        tracker.diff(vec![t("a", 5_000, 0)], None, FLOOR); // seed
        // Arrives now, stamped after the floor but before the thread's newest:
        // the activity falls to 4_000, the unread count takes it.
        let r = tracker.diff(vec![t_lbl("a", 4_000, 1, "Backdated")], None, FLOOR);
        assert_eq!(labels(&r), vec!["Backdated"]);
        // Exactly one: the next tick carrying the same state is silent.
        let r = tracker.diff(vec![t("a", 4_000, 1)], None, FLOOR);
        assert!(r.is_empty(), "bannered twice: {:?}", labels(&r));
    }

    /// The user's own message — sent here, or from another of their devices —
    /// raises the thread's activity but never its unread count, so it is not
    /// news; nor is a brand-new thread the user started.
    #[test]
    fn diff_own_message_raising_activity_but_not_unread_raises_no_banner() {
        let tracker = MessageNotificationTracker::new();
        tracker.diff(vec![t("a", 100, 0)], None, 0); // seed
        let r = tracker.diff(vec![t("a", 150, 0)], None, 0);
        assert!(r.is_empty(), "own message bannered: {:?}", labels(&r));
        let r = tracker.diff(vec![t("a", 150, 0), t("b", 200, 0)], None, 0);
        assert!(r.is_empty(), "own new thread bannered: {:?}", labels(&r));
    }

    /// Reading a thread lowers its count; the next arrival raises it again and
    /// banners, although the count ends no higher than it stood before the read.
    #[test]
    fn diff_a_rise_after_a_read_is_news() {
        let tracker = MessageNotificationTracker::new();
        tracker.diff(vec![t("a", 100, 2)], None, 0); // seed
        assert!(tracker.diff(vec![t("a", 100, 0)], None, 0).is_empty()); // read
        let r = tracker.diff(vec![t_lbl("a", 150, 1, "Again")], None, 0);
        assert_eq!(labels(&r), vec!["Again"]);
    }

    /// The floor gates news, never the seed: the first non-empty snapshot is
    /// silent whatever its stamps say (rule 1 stands on its own).
    #[test]
    fn diff_seed_is_silent_even_for_activity_past_the_floor() {
        let tracker = MessageNotificationTracker::new();
        let r = tracker.diff(vec![t("a", 5_000, 1), t("b", 6_000, 2)], None, 1_000);
        assert!(r.is_empty());
    }
}
