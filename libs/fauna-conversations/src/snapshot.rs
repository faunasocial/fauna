use crate::address::{Rail, TypedAddress};
use crate::capabilities::ThreadCapabilities;
use crate::compose::{AddParticipantState, ComposeState};
use crate::message::{MessageId, MessageSnapshot};
use crate::thread::{ThreadFlavor, ThreadId};
use fauna_core::localized::LocalizedText;
pub use fauna_core::source_glyph::BridgeIdentitySnapshot;
use fauna_core::source_glyph::SourceGlyph;
use serde::{Deserialize, Serialize};

/// The family gate's marker on a supervised account's bridged conversation —
/// what `conversation-guardian-state` paints on the row and in the detail
/// (`behavior/family-safety.md` § The bridge-DM gate). **Computed by the nest
/// at every read and only painted here**: the user-side `rooms.list` row
/// carries it, the bridged rail keeps the newest answer, and the manager
/// projects it onto the thread. Absent for every unsupervised account. A read
/// is never gated on it — a held thread stays fully readable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum GuardianState {
    /// The guardian reviews cold peers and has not decided this one.
    Held,
    /// The guardian decided against this peer: its new messages never land
    /// and a send to it is refused.
    Blocked,
}

impl GuardianState {
    /// Parse the wire word. A word only a newer nest could write reads as
    /// [`Self::Held`] — the nest's own rule for a verdict it cannot name
    /// (`supervised_dm_verdict`): the marker that claims no guardian decision
    /// and hides nothing.
    pub fn from_wire(word: &str) -> Self {
        match word {
            "blocked" => Self::Blocked,
            _ => Self::Held,
        }
    }

    /// The stable token the automation surface reports as the element's
    /// value — the wire word.
    pub fn attr_token(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Blocked => "blocked",
        }
    }

    /// The marker's localized text, the same on every app.
    pub fn label(self) -> &'static str {
        use fauna_i18n::strings::conversations::unified;
        match self {
            Self::Held => unified::GUARDIAN_STATE_HELD,
            Self::Blocked => unified::GUARDIAN_STATE_BLOCKED,
        }
    }
}

/// FFI-exported twin of [`GuardianState::label`] — what
/// `conversation-guardian-state` states, on the row and in the detail. Behind
/// `client-display`, like `typed_address_display`, so the Go mail-bridge's
/// FFI build carries no paint export.
#[cfg(feature = "client-display")]
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn guardian_state_label(state: GuardianState) -> String {
    state.label().to_string()
}

/// FFI-exported twin of [`GuardianState::attr_token`] — the marker's `state`
/// attribute.
#[cfg(feature = "client-display")]
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn guardian_state_attr_token(state: GuardianState) -> String {
    state.attr_token().to_string()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum SortOrder {
    #[default]
    LatestActivity,
    OldestFirst,
    Unread,
}

/// The `conversation-sort` button's canonical cycle: latest activity → oldest
/// first → unread → back to latest. One tap advances one step; three taps from
/// any order return home, so every order stays reachable on every app.
///
/// This owns the *cycle*; [`crate::manager::ConversationsManager::set_sort`]
/// owns the resulting reorder (`sort_summaries`). Clients pass the order the
/// snapshot handed them and feed the result straight back to `set_sort` — they
/// never enumerate the orders themselves. That is what keeps the arity uniform:
/// each app used to hand-roll it, and they disagreed — android cycled all
/// three while web and apple reached only two, leaving [`SortOrder::Unread`]
/// (fully implemented in `sort_summaries`) unreachable for those users.
///
/// The 3-way cycle is user-ratified (2026-07-16); see
/// `docs/goal/ui/conversations.md` § Where logic lives + the `conversation-sort`
/// element-table row.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn next_sort_order(current: SortOrder) -> SortOrder {
    match current {
        SortOrder::LatestActivity => SortOrder::OldestFirst,
        SortOrder::OldestFirst => SortOrder::Unread,
        SortOrder::Unread => SortOrder::LatestActivity,
    }
}

/// [`next_sort_order`] over the serde variant name — the wasm/JS face, the same
/// take-the-serde-name shape as
/// [`crate::compose::recipient_resolve_status_from_variant`]. `name` is exactly
/// what [`ConversationsSnapshot::sort`] serializes (`"LatestActivity"`,
/// `"OldestFirst"`, `"Unread"`).
///
/// An unknown or absent name restarts the cycle at the [`Default`] order rather
/// than erroring: a tap must always move the sort somewhere, so a stale or
/// garbled order can never freeze the button. (Contrast the wasm `setSort`,
/// which *does* reject an unknown value — there the caller is asserting a
/// specific order, and a silent fallback would hide the bug.)
pub fn next_sort_order_from_variant(name: &str) -> SortOrder {
    match name {
        "LatestActivity" => next_sort_order(SortOrder::LatestActivity),
        "OldestFirst" => next_sort_order(SortOrder::OldestFirst),
        "Unread" => next_sort_order(SortOrder::Unread),
        _ => SortOrder::default(),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ConversationsSnapshot {
    pub threads: Vec<ThreadSummary>,
    pub sort: SortOrder,
    pub search_query: Option<String>,
    pub selected_thread_id: Option<ThreadId>,
    pub new_thread_compose: Option<ComposeState>,
    pub add_participant: Option<AddParticipantState>,
    /// Page-level error → `error-message` (`conversations.md` § Errors & edge
    /// cases), the same shape and role as `FeedSnapshot::error`.
    ///
    /// Carries the failures of the **membership/label wire ops** —
    /// `confirm_add_participant`, `remove_participant`, `rename_thread` — each
    /// of which mutates the snapshot optimistically and then fires a wire op
    /// that can fail. Before this field they only `tracing::warn!`d, so a failed
    /// add closed the overlay and did nothing visible: a dropped command by
    /// `../architecture/testing.md` point 11's definition, and the inverse of
    /// the "rendered truthfully as not-yet-shared" property
    /// `mls-group-key-material.md` § M2 requires of a half-added member.
    ///
    /// **Not** the compose send path: a send failure is compose-scoped and
    /// already surfaced through `ComposeState::send_state`
    /// (`SendState::Failed { reason }`, whose `reason` is a `LocalizedText` too —
    /// one element, one carrier), which clients render from the *active* compose.
    /// Two truths for one gesture would be the drift; each gesture has exactly
    /// one. A new gesture supersedes the previous one's error — every
    /// producer clears this on entry, `send`/`send_new_thread` included.
    #[serde(default)]
    pub error: Option<LocalizedText>,
    /// The **launch floor** — where this run's news starts, in message-stamp
    /// terms: the moment the thread store was created, or wiped for an identity
    /// change (`conversations.md` § State & data shape → *When a thread is
    /// read*). A message stamped before it is history, whichever snapshot
    /// delivers it — it never counts as unread, and the new-message banner
    /// decision (`MessageNotificationTracker::diff`, § Where logic lives) reads
    /// this same value so a thread a slower rail delivers after the seed never
    /// banners for old mail. Published rather than re-read by each app so the
    /// two decisions can never disagree about where news starts.
    #[serde(default)]
    pub launch_floor_ms: i64,
    /// Every community-room invitation standing for this account, verified —
    /// `room-invitation[i]` atop the conversation list. Refreshed on the
    /// receive loop's sweep; an accepted or declined one leaves the list in the
    /// same act (`conversation-rooms.md` § Join rules and invites).
    #[serde(default)]
    pub room_invitations: Vec<crate::room::RoomInvitationSnapshot>,
    /// The bridges serving this account, by declared identity and ordered by
    /// label — what the recipient picker names, so the user can see which far
    /// networks a typed address may reach, and what labels a resolved bridged
    /// address (`conversations.md` § Where logic lives → *The `Bridged`
    /// adapter*, ruling 2 (d): the grammar is the nest's, so the picker lists
    /// bridges by label and never matches an address itself). Empty with no
    /// bridge consented.
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = []))]
    pub bridges: Vec<BridgeIdentitySnapshot>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ThreadSummary {
    pub thread_id: ThreadId,
    pub rail: Rail,
    /// The canonical icon concept for `rail` (`rail.glyph()`), precomputed so
    /// every app maps one `SourceGlyph → native asset` instead of switching
    /// on `rail` itself (D5; `render-model.md` § Deltas). Web reads the
    /// lowercase serde string off the snapshot; native apps match the enum.
    pub glyph: SourceGlyph,
    pub flavor: ThreadFlavor,
    pub label: String,
    pub snippet: String,
    pub last_activity_ms: i64,
    pub unread_count: u32,
    pub participant_count: u32,
    /// Which bridge carries this thread — `Some` only on a [`Rail::Bridged`]
    /// thread whose bridge the rail's registry knows, `None` on every other
    /// rail (`ui/conversations.md` § Where logic lives → *The `Bridged`
    /// adapter*, ruling 2 (a)). When `Some`, [`Self::glyph`] is the bridge's
    /// declared glyph and the label names the bridge; a bridged thread read
    /// with no identity paints the generic `SourceGlyph::Bridge`. A
    /// **read-time projection** filled by `ConversationsManager::snapshot`,
    /// like [`ThreadDetail::room`].
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub bridge: Option<BridgeIdentitySnapshot>,
    /// The family gate's marker for this thread — see [`GuardianState`].
    /// `Some` only on a [`Rail::Bridged`] thread of a supervised account whose
    /// peer the nest reports held or blocked; a read-time projection from the
    /// rail, like [`Self::bridge`].
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub guardian_state: Option<GuardianState>,
}

/// Total unread across every thread — **the home-screen widget's number**
/// (`apps/common.md` § Home-screen widget): the per-thread `unread_count` the
/// conversations list renders, summed, so a widget can never show a count the
/// app would not. Shared by every app's outside-the-app surface (linux's
/// launcher badge and tray tooltip, windows' taskbar badge, android's widget)
/// so no app keeps a per-app tally or runs a second count query. Pure over the
/// threads so it unit-tests without a live `ConversationsManager`; the manager
/// exposes it over the FFI as
/// [`crate::manager::ConversationsManager::unread_total`]. Lifted from the
/// linux app 2026-09-26 (priority #2), where it fed the tray badge since the
/// legacy inbox drain was deleted.
pub fn sum_unread(threads: &[ThreadSummary]) -> u32 {
    threads.iter().map(|t| t.unread_count).sum()
}

/// The number of secure channels this identity holds open — **the Status
/// page's `status-mls-channels` count** (`ui/status.md` § State & data shape,
/// the MLS leg of the shared snapshot). One per thread on the [`Rail::FaunaMls`]
/// rail, each of which is exactly one MLS group; a thread on any other rail
/// (SMTP, a bridge) is not an MLS group and never counts. Pure over
/// the threads, like [`sum_unread`], so every app's Status surface reads the
/// same number and none keeps a rail filter of its own; the manager exposes it
/// as [`crate::manager::ConversationsManager::secure_channel_count`].
pub fn secure_channel_count(threads: &[ThreadSummary]) -> u64 {
    threads.iter().filter(|t| t.rail == Rail::FaunaMls).count() as u64
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ThreadDetail {
    pub thread_id: ThreadId,
    pub rail: Rail,
    /// The canonical icon concept for `rail` (`rail.glyph()`); see
    /// [`ThreadSummary::glyph`]. Lets the thread header reuse the same single
    /// per-app `SourceGlyph → native asset` map as the conversation list.
    pub glyph: SourceGlyph,
    pub flavor: ThreadFlavor,
    pub label: String,
    pub participants: Vec<TypedAddress>,
    pub participant_displays: Vec<String>,
    pub capabilities: ThreadCapabilities,
    pub messages: Vec<MessageSnapshot>,
    pub compose: ComposeState,
    /// The one message in [`messages`](Self::messages) the view should
    /// distinguish, or `None` — the second half of `SearchNav::Mail`'s contract,
    /// *"open the thread **and** select this message in it"*
    /// (`docs/goal/ui/search.md` § State & data shape).
    ///
    /// A **read-time resolve**, not stored state: `ConversationsManager::
    /// thread_detail` re-derives it on every emit and yields `None` unless the
    /// id is present in `messages`, so an app can never be handed a selection it
    /// cannot place. Apps paint from this field alone and never re-derive which
    /// message is selected (priority #2); the marker's *shape* is per-platform
    /// (a background tint where there is fill to vary, a text marker in a
    /// terminal), its automation observable is the shared `selected` attribute
    /// on `dm-message-timestamp` (ui.yaml `dm-message-bubble`).
    ///
    /// Lives on the detail rather than on each [`MessageSnapshot`] because it is
    /// *view* state, not a fact about the message — the same split that puts
    /// `selected_thread_id` on [`ConversationsSnapshot`] and not on
    /// [`ThreadSummary`]. It also keeps the concept out of `MessageSnapshot`,
    /// which the Go mail bridge consumes over FFI and has no view to select in.
    #[serde(default)]
    pub selected_message_id: Option<MessageId>,
    /// The room this thread is — class, per-member roles (index-parallel
    /// with `participants`), the policy, the viewer's role
    /// (`conversation-rooms.md` § The room; [`crate::room`]). A **read-time
    /// projection** like `selected_message_id`: `ConversationsManager::
    /// thread_detail` asks the thread's rail on every emit, and the same
    /// projection overlays the role gating onto `capabilities`. `None` for a
    /// rail that models no room yet (every non-native rail until the
    /// `Bridged` adapter's derivation lands, `conversations.md`
    /// § Implementation status today).
    #[serde(default)]
    pub room: Option<crate::room::RoomSnapshot>,
    /// Which bridge carries this thread — see [`ThreadSummary::bridge`]; the
    /// same read-time projection, made by `ConversationsManager::thread_detail`
    /// beside [`Self::room`].
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub bridge: Option<BridgeIdentitySnapshot>,
    /// The family gate's marker — see [`ThreadSummary::guardian_state`].
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub guardian_state: Option<GuardianState>,
}

/// One thread as a state reader sees it — `ConversationsManager::
/// thread_state_facts`: the detail with its message list EMPTY, and the two
/// facts taken from the messages instead. Never crosses FFI: the readers are
/// the shared state serializer and its tests.
#[derive(Clone, Debug)]
pub struct ThreadStateFacts {
    /// Everything but `messages` (empty) and `compose` (default), with the room
    /// and room-gated capabilities projected as `thread_detail` projects them.
    pub detail: ThreadDetail,
    pub message_count: usize,
    /// Each message's subject line (`""` where it has none), in message order.
    pub subject_lines: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(id: &str, unread: u32) -> ThreadSummary {
        ThreadSummary {
            thread_id: ThreadId(id.to_string()),
            rail: Rail::FaunaMls,
            glyph: Rail::FaunaMls.glyph(),
            flavor: ThreadFlavor::OneToOne,
            label: id.to_string(),
            snippet: String::new(),
            last_activity_ms: 0,
            unread_count: unread,
            participant_count: 1,
            bridge: None,
            guardian_state: None,
        }
    }

    #[test]
    fn sum_unread_adds_per_thread_counts() {
        let threads = [thread("a", 3), thread("b", 0), thread("c", 5)];
        assert_eq!(sum_unread(&threads), 8);
    }

    #[test]
    fn sum_unread_empty_is_zero() {
        assert_eq!(sum_unread(&[]), 0);
    }

    /// The Status page's channel count is the MLS-rail threads only — a
    /// bridged thread is not a secure channel, whatever its flavor.
    #[test]
    fn secure_channel_count_counts_only_the_mls_rail() {
        let mut bridged = thread("smtp", 0);
        bridged.rail = Rail::Smtp;
        bridged.glyph = Rail::Smtp.glyph();
        let mut group = thread("room", 0);
        group.flavor = ThreadFlavor::MlsGroup;
        let threads = [thread("dm", 2), bridged, group];
        assert_eq!(secure_channel_count(&threads), 2);
        assert_eq!(secure_channel_count(&[]), 0);
    }

    #[test]
    fn next_sort_order_cycles_all_three_orders_and_returns_to_the_default() {
        assert_eq!(
            next_sort_order(SortOrder::LatestActivity),
            SortOrder::OldestFirst
        );
        assert_eq!(next_sort_order(SortOrder::OldestFirst), SortOrder::Unread);
        assert_eq!(
            next_sort_order(SortOrder::Unread),
            SortOrder::LatestActivity
        );

        // Every order is reachable from every other: three taps from anywhere
        // return you home, so no client can strand a user in a sub-cycle (the
        // web/apple 2-way drift this lift resolves).
        for start in [
            SortOrder::LatestActivity,
            SortOrder::OldestFirst,
            SortOrder::Unread,
        ] {
            let looped = next_sort_order(next_sort_order(next_sort_order(start)));
            assert_eq!(looped, start, "three taps from {start:?} must return home");
        }
    }

    #[test]
    fn next_sort_order_from_variant_takes_the_serde_name_and_restarts_on_unknown() {
        assert_eq!(
            next_sort_order_from_variant("LatestActivity"),
            SortOrder::OldestFirst
        );
        assert_eq!(
            next_sort_order_from_variant("OldestFirst"),
            SortOrder::Unread
        );
        assert_eq!(
            next_sort_order_from_variant("Unread"),
            SortOrder::LatestActivity
        );

        // Unknown/absent restarts the cycle at the default rather than erroring
        // — a tap always moves, so a stale/garbled order can never freeze the
        // button.
        assert_eq!(next_sort_order_from_variant(""), SortOrder::LatestActivity);
        assert_eq!(
            next_sort_order_from_variant("Nonsense"),
            SortOrder::LatestActivity
        );

        // The names are exactly what the snapshot's `sort` field serializes,
        // which is what the wasm face receives from the SPA.
        for order in [
            SortOrder::LatestActivity,
            SortOrder::OldestFirst,
            SortOrder::Unread,
        ] {
            let name = serde_json::to_value(order).unwrap();
            let name = name.as_str().unwrap();
            assert_eq!(next_sort_order_from_variant(name), next_sort_order(order));
        }
    }
}
