//! The conversations page — the unified DM surface (`conversations.md`).
//!
//! **Where the logic lives** (`conversations.md` § Where logic lives + Architectural
//! rules 1–2): the snapshot and every mutation belong to the shared
//! [`fauna_conversations::ConversationsManager`], which tui consumes **directly**
//! — no FFI hop, the same way linux does (priority #2: the two Rust-native
//! apps share the manager) and the same way the feed page consumes
//! [`fauna_feed::FeedManager`]. This module is a paint shell: it reads a
//! snapshot, emits an [`Element`] list, and forwards gestures to manager methods.
//! It holds **no** view-model state the manager could own (Architectural rule 6:
//! "No per-app ViewModel owning state").
//!
//! One structural difference from feed: [`fauna_conversations::ConversationsManager`]
//! is **not generic** (it holds `Arc<dyn RailBackend>` backends internally), so the
//! tui type is a plain `Arc<ConversationsManager>` — no `<Arc<NestClient>>` param.
//!
//! **Slice status (M4):** list page, new-thread compose, the
//! `conversation_detail` sub-page (read view + membership/rename overlays + the
//! reply compose bar), the `data.conversation_threads` state serializer, the
//! mock-backend inject commands, and the real `ConversationsSession` (MLS
//! engine + receive loop — [`conv_backend`]) are all built. Reactions/delete
//! (`dm-message-actions`) is the open remainder — the milestone chain is
//! tracked internally.
//!
//! **The element rule this page inherits from the walker** (`document.rs`): a
//! block that ui.yaml gives its own element ID is painted by the **page**, as that
//! element; everything else is body text painted by the walker.

pub mod conv_backend;
pub mod drafts;

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client_mail_settings::MailSettingsMachine;
#[cfg(test)]
use fauna_conversations::SortOrder;
use fauna_conversations::{
    ConversationsManager, ConversationsSnapshot, MessageId, Rail, RecipientPickerState,
    ResolveState, SendState, SnapshotObserver, ThreadId, TypedAddress, next_sort_order,
};
use fauna_core::identity::ActorId;
use fauna_core::obligation::RenderVerdict;
use fauna_core::render::{RenderBlock, RenderDocument};
use fauna_i18n::strings::{common, conversations, family, markdown};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{App, DataMessage, UiMessage};
use crate::element::{Element, Field, Gesture, SelectTarget};

/// A gesture on the conversations page. Each maps onto a manager method or a
/// local mode switch — never a navigation decision of its own.
#[derive(Debug, Clone)]
pub enum Action {
    /// `conversation-item` click → ui.yaml `conversations` transition
    /// `click conversation-item → conversation_detail`.
    SelectThread(String),
    /// Open a thread **and** select one message inside it — the whole of
    /// `SearchNav::Mail`'s contract (`ui/search.md` § State & data shape). Not
    /// reachable by a gesture on this page: the Search page's result row is its
    /// only producer, which is why `SelectThread` above stays the plain
    /// thread-open every `conversation-item` click uses.
    SelectThreadAndMessage {
        thread_id: String,
        message_id: String,
    },
    /// `new-conversation-button` → the in-pane `compose` sub-page.
    StartNewConversation,
    /// `new-conversation-cancel` → discard the new-thread draft, back to the list.
    CancelNewConversation,
    /// `conversation-sort` — toggle the thread sort order.
    Sort,

    // --- the compose sub-page ---
    /// `recipient-picker-suggestion` click / the `conversations_accept_recipient`
    /// command — commit the picker's current text (or a picked suggestion) as a
    /// chip. The manager decides which picker is active and whether the text
    /// parses (`accept_current_recipient_chip`).
    AcceptRecipientChip,
    /// `topic-toggle-button` — reveal/hide the `subject-input` on the active
    /// compose.
    ToggleTopic,
    /// A markdown-toolbar button — splice `prefix`…`suffix` markers into the
    /// compose body (the wrap *rule* is shared `fauna_core::markdown::wrap_selection`).
    /// A terminal input carries no selection, so this inserts the shared "text"
    /// placeholder at the cursor (the empty-selection case the GUI toolbars hit too:
    /// bold → `**text**`).
    MarkdownWrap {
        prefix: &'static str,
        suffix: &'static str,
    },
    /// `dm-send-button` on the new-thread composer — send. Async (the one network
    /// gesture on this sub-page), so it comes back as an [`Op`].
    ///
    /// `rail` is the rail the first committed recipient chip resolves to, read
    /// off the same paint that produced the button and carried **for the
    /// offline gate only** — the manager remains the dispatch authority
    /// (`send_new_thread` re-derives it, and additionally flushes a
    /// typed-but-uncommitted recipient this cannot see). That asymmetry is
    /// deliberate and safe by construction: a carried rail that disagreed with
    /// the manager's could only mis-*gate*, never mis-*route*, and `None` (no
    /// chip committed yet) leaves the control live.
    ///
    /// `founds_room` is the same paint's answer to "does this send found a
    /// community room" — the home-nest toggle on and the prospective class
    /// community — carried for the gate on the same terms: a founding is the
    /// `room.create` ceremony, never a queued channel append, so it needs a
    /// nest, and the manager decides it again at send.
    SendNewThread {
        rail: Option<Rail>,
        founds_room: bool,
    },
    /// `recipient-picker-home-nest-toggle` — seat (or unseat) the user's home
    /// nest in the room about to be created, which makes the first send found
    /// a **community** room (`manager.set_new_thread_home_nest`). Local: the
    /// choice lives on the new-thread draft until Send.
    ToggleHomeNest,

    // --- the conversation_detail overlays (membership + rename) ---
    /// `thread-add-participant-button` — open the add-participant overlay for the
    /// selected thread (`manager.open_add_participant`). Local (manager state),
    /// so it lands synchronously.
    OpenAddParticipant,
    /// `add-participant-confirm` — apply the membership change (fork a new group
    /// on a FaunaMls 1:1, add in place otherwise). Async
    /// (`manager.confirm_add_participant`), so it comes back as an [`Op`].
    ///
    /// `in_place_mls_group` is read straight off
    /// [`fauna_conversations::AddParticipantState::in_place_mls_group`], the
    /// discriminant the manager stamps when it opens the overlay, carried
    /// **for the offline gate only** (the manager re-derives the authority for
    /// the wire op): it is exactly the condition under which this gesture
    /// reaches the wire at all. A 1:1 fork, or any other rail, is a
    /// snapshot-only edit — the new thread's first send is what bootstraps —
    /// so the two arms are "needs a nest" and "issues nothing", which is a
    /// class difference no undiscriminated variant could state.
    ///
    /// ⚠ tui used to re-derive the `(rail, flavor)` test at paint. It doesn't
    /// any more, and no app should: the six UniFFI apps read the same field
    /// off the snapshot, which is what let apple declare this kind at all
    /// (a blanket gate would have greyed the fork).
    ConfirmAddParticipant { in_place_mls_group: bool },
    /// A `thread-member-chip` tap on a membership-change-capable thread —
    /// remove that participant (`manager.remove_participant`, the same op the
    /// linux chip drives: posts the MLS Commit, no Welcome). Async →
    /// [`Op::RemoveMember`]. Capability-gated at paint, linux's exact rule
    /// (`thread_header.rs::render` — capability-gated, never rail-branched):
    /// the chip is a gesture only under `supports_membership_change`; mail
    /// chips stay informational labels, so this action is unreachable on a
    /// thread whose rail cannot change membership. NOT routed through the
    /// `conv_backend::e2e_remove` seam — that exists for the agent
    /// (an agent-only path makes a feature look present
    /// while no user can reach it).
    RemoveMember { addr: TypedAddress },
    /// `thread-member-keep-button[i]` — the owner recognises this person, so
    /// their open review items close (`identity-succession.md` § Propagation →
    /// *MLS groups*). Async → [`Op::KeepMember`], because the verdict is a
    /// succession-ledger write.
    ///
    /// The *Remove* half of the pair has no action of its own: it is
    /// [`Self::RemoveMember`] on the chip this button sits inside. Keep joins
    /// the affordance that exists rather than minting a second eviction path,
    /// exactly as the grant plane's Keep joins `nest-trust-grant-revoke`.
    ///
    /// ⚠ Keep closes **an item, never the person forever**: a later succession
    /// is a different raising event and legitimately asks again.
    KeepMember { person: ActorId },
    /// `thread-rename-button` — open the rename overlay, seeding the field with
    /// the thread's current label. Local (the draft is client state), synchronous.
    OpenRename,
    /// `thread-rename-confirm` — commit the renamed label (`manager.rename_thread`).
    /// Async, so it comes back as an [`Op`].
    ConfirmRename,

    // --- the room policy editor (`conversation-rooms.md` § Roles and authorization) ---
    /// `thread-room-settings-button` — open the editor, seeding the draft from
    /// `ThreadDetail::room` (join rule, history policy, the admin flag per
    /// participant). Local (the draft is client state, like the rename's).
    /// Greyed at paint unless `capabilities.can_set_policy`.
    OpenRoomSettings,
    /// `room-join-rule-select` — stage a join rule by picker token
    /// (`invite`/`member-invite`). Local: nothing leaves the device until Save.
    SetRoomJoinRule(String),
    /// `room-history-policy-select` — stage a history policy by token
    /// (`none`/`full`). Local.
    SetRoomHistoryPolicy(String),
    /// `room-admin-toggle[i]` — flip the staged admin flag of participant `index`.
    /// Local; greyed at paint unless `capabilities.can_appoint_admins`.
    ToggleRoomAdmin { index: usize },
    /// `room-owner-transfer-button[i]` — stage participant `index` as the
    /// room's new owner, or un-stage it when it already is (at most one row
    /// staged). Local; greyed at paint unless
    /// `capabilities.can_transfer_ownership`. Save posts the countersigned
    /// offer LAST, after every other staged change — once it lands this seat
    /// can no longer edit the policy.
    ToggleRoomOwnerTransfer { index: usize },
    /// `room-nest-read-toggle` — flip the staged home-nest read on a community
    /// room (`RoomSettingsDraft::toggle_nest_read`). Local; Save commits it as
    /// a rotation with the nest in or out, before any hand-over. Greyed at
    /// paint unless `capabilities.can_set_policy`.
    ToggleRoomNestRead,
    /// `room-labeler-toggle[i]` — name or un-name one published labeler in the
    /// staged set (`RoomSettingsDraft::toggle_labeler`). Carries the labeler's
    /// id, read off the catalog row at paint, rather than a row index: the
    /// catalog can be re-read under the open editor, and an id cannot then
    /// aim at a different labeler. Local; greyed at paint unless
    /// `capabilities.can_set_policy` and `labeler_toggle_live`.
    ToggleRoomLabeler { labeler: String },
    /// `room-labeler-inspect-button[i]` — open that labeler's catalog inspect
    /// view in place (`LabelerCatalogMachine::inspect`). Carries the catalog
    /// SNAPSHOT index the machine addresses rows by, like the catalog page's
    /// own inspect. Async → [`Op::RoomLabelerCatalog`].
    InspectRoomLabeler { index: u32 },
    /// `labeler-inspect-close-button` painted inside the room editor — close
    /// the view through the machine (`close_inspect`), back to the editor.
    CloseRoomLabelerInspect,
    /// `room-settings-save-button` — commit every staged change, one policy
    /// commit each (`set_room_join_rule` / `set_room_history_policy` /
    /// `appoint_admin` / `demote_admin`). Async → [`Op::SaveRoomSettings`]; the
    /// editor closes on [`Outcome::RoomSettingsSaved`] only when every commit
    /// landed, and stays open with the page's `error-message` otherwise.
    SaveRoomSettings,
    /// `room-leave-button` — open the departure confirm. Local; greyed at
    /// paint unless `capabilities.can_leave_room`, which the roles table
    /// closes for the owner (hand the room over first).
    StartRoomLeave,
    /// `room-leave-confirm` — walk out (`manager.leave_room`). Async →
    /// [`Op::LeaveRoom`]. Deliberately NOT staged through
    /// `room-settings-save-button`: leaving is an immediate act, not a policy
    /// edit, and the room's class picks the door beneath it in shared Rust.
    ConfirmRoomLeave,
    /// `room-pending-invite-withdraw-button[i]` — withdraw the invitation
    /// pending on the open room for `invitee_actor_hex` (the row's own
    /// `RoomPendingInviteSnapshot::invitee_actor_hex`;
    /// `manager.withdraw_room_invite`). Async → [`Op::WithdrawRoomInvite`].
    /// Acts at once and needs no confirm — inviting again undoes it — and
    /// stays outside `room-settings-save-button`'s staged set like the
    /// walk-out: a withdrawal is not a policy edit. No capability gates it:
    /// the nest served the row *because* this viewer may withdraw it
    /// (`conversation-rooms.md` § Join rules and invites → *Pending
    /// invitations are visible to whoever may withdraw them*).
    WithdrawRoomInvite { invitee_actor_hex: String },

    // --- the standing community-room invitations, atop the list ---
    /// `room-invitation-accept-button[i]` — accept the invitation `id` names
    /// (`manager.accept_room_invitation`): the account is seated on the
    /// room's floor and the room opens as a thread. Async →
    /// [`Op::AcceptRoomInvitation`]; the page follows the manager onto the
    /// thread it selected.
    AcceptRoomInvitation { id: i64 },
    /// `room-invitation-decline-button[i]` — the invitation stops standing
    /// for this account; the room is not told
    /// (`manager.decline_room_invitation`). Async → [`Op::DeclineRoomInvitation`].
    DeclineRoomInvitation { id: i64 },
    // NB: both overlays are *cancelled* by Esc (the human-only keymap affordance
    // in `app.rs`, via `cancel_detail_overlay`), not by a painted gesture — ui.yaml
    // gives neither overlay a dismiss id — so there is no `Cancel*` action here.

    // --- the detail reply compose bar (`conversations.md` § Participants vs reply recipients) ---
    /// `dm-reply-button[i]` (sender-only) / `dm-reply-all-button[i]` (every
    /// participant but self) — seed the reply for message `msg_id`
    /// (`manager.start_reply`). Local (sync), so it lands in place.
    ReplyToMessage { msg_id: MessageId, reply_all: bool },
    /// `dm-reply-cancel` — clear `compose.reply_to` (`manager.set_reply_to(id,
    /// None)`), dropping back to a plain (non-reply) send on this thread. The
    /// reply-recipient chips seeded by [`Self::ReplyToMessage`] are cleared as
    /// part of the same manager call, not a separate step. Local (sync).
    CancelReply,
    /// `dm-reply-recipient-remove[i]` — drop `addr` from **this reply's** To/Cc
    /// (`manager.remove_reply_recipient`); thread history is untouched.
    RemoveReplyRecipient { addr: TypedAddress },
    /// Enter on `dm-reply-recipient-add` — parse the To-line input buffer and
    /// commit it as a reply recipient (`manager.add_reply_recipient`), then clear
    /// the buffer. A malformed address is a no-op (the buffer stays for a fix).
    CommitReplyRecipientAdd,
    /// `dm-send-button` in Detail mode — send the current draft on the open thread
    /// (`manager.send(thread_id)`). Async, so it comes back as an [`Op`]
    /// (distinct from `SendNewThread`, which sends the new-thread composer).
    ///
    /// `rail` is the open thread's own rail, carried for the offline gate on
    /// the same terms as [`Self::SendNewThread`]'s: the manager routes by
    /// `ThreadDetail::rail` regardless, so this can only ever mis-gate.
    SendThread { rail: Rail },
    /// Enter on `attachment-button` — read the typed path and stage its bytes on
    /// the active composer (`manager.add_attachment` in Detail,
    /// `add_new_thread_attachment` in Compose), then clear the buffer.
    ///
    /// **Never silently drops.** An unreadable path writes the io error to
    /// `App::errors` so it reaches `error-message` (e2e convention 11; the same
    /// failure mode as the `folder-save-paths` pending-path bug, 2026-07-30) and
    /// keeps the buffer so the user can fix the path. Sync — the read is local
    /// file io and the manager hashes + caches in-memory, so nothing goes to the
    /// nest until send.
    CommitAttachment,
    /// `dm-compose-attachment-remove[i]` — unstage the attachment at `index` from
    /// the active composer (`remove_attachment`/`remove_new_thread_attachment`).
    /// Sync.
    RemoveAttachment { index: u32 },

    // --- the per-bubble ⋯ actions menu (`conversations.md` § Reactions & message delete) ---
    /// `dm-message-actions-button[i]` — open the actions overlay for message
    /// `msg_id` (a local overlay, like the rename draft). Sync.
    OpenMessageActions { msg_id: MessageId },
    /// `dm-reaction-option[i]` in the open menu, or a `dm-reaction-pill[i]` tap —
    /// toggle `emoji` on `msg_id` (`manager.toggle_reaction`). Async → [`Op`];
    /// closes the overlay (a pill tap has none open — closing is a no-op).
    ToggleReaction { msg_id: MessageId, emoji: String },
    /// `dm-reaction-more-button` — a terminal has no OS emoji picker (the one
    /// per-platform divergence ui.yaml's menu component notes), so "more" flips
    /// the menu into a free-entry prompt: type any emoji, Enter commits. Sync.
    StartReactionEntry,
    /// Enter on the reaction free-entry prompt — commit the typed emoji as a
    /// toggle on the overlay's message (empty/whitespace input is a no-op).
    CommitReactionEntry,
    /// `dm-message-delete-button` — reveal the destructive-action confirm step
    /// (linux's two-step inside the same flyout). Sync.
    StartDeleteMessage,
    /// `dm-message-delete-confirm-button` — post the cooperative tombstone
    /// (`manager.delete_message`). Async → [`Op`]; closes the overlay.
    ConfirmDeleteMessage,
    /// `dm-message-mark-as-spam-button` — train the sealed tier-1 spam model over
    /// this received (`!is_own`) message's body (`mail-spam.md` § Training signal
    /// sources 1). Async → [`Op`]; closes the overlay (one flyout hop, no confirm
    /// step). A **silent no-op** when mail isn't enabled (no machine) — a
    /// conversation message is client-only content the nest can't read, so there
    /// is no server-train fallback.
    MarkMessageSpam { msg_id: MessageId },
    /// `dm-message-muted-reveal-button[i]` — un-collapse ONE muted-keyword match
    /// for the rest of the session (`content-moderation-and-ranking.md` § Q3;
    /// `moderation.md` § Muted keywords — the mute itself is untouched, which is
    /// why this writes a session-local set and never the sealed list). Sync.
    RevealMuted { msg_id: MessageId },
    /// Un-collapse ONE content-policy `collapse` for the rest of the session
    /// (`family-safety.md` § Content policy). Session-local like
    /// [`Self::RevealMuted`], and deliberately a separate verb: the floor itself
    /// persists, so revealing one message never relaxes the guardian's policy or
    /// the viewer's own threshold. It can never reveal a `block`.
    RevealContent { msg_id: MessageId },
    /// `load-remote-content-button[i]` — opt ONE message into loading its blocked
    /// remote content: body `RemoteImage`s **and** a resolved link preview's
    /// og:image, together (`render-model.md` § D3; § D4 *og:image reveal gate*).
    /// Sync, and deliberately a *manager* dispatch rather than app state: the
    /// reveal set is manager-owned so the privacy-sensitive "when does untrusted
    /// inbound content phone home" decision lives in one audited place across all
    /// 7 apps — unlike [`Self::RevealMuted`], whose collapse is a pure render
    /// decision and so stays local.
    RevealRemoteImages { msg_id: MessageId },
}

impl Action {
    /// The wire kind this gesture issues — the offline gate's input
    /// (`crate::element::Gesture::wire_kind`), exhaustive with no fallback arm
    /// so a new variant cannot skip the question.
    ///
    /// **The page's headline is that almost nothing here desensitizes, and
    /// that is the design working rather than the sweep missing.** A
    /// conversation is the archetypal offline surface: every reachable send
    /// resolves to an `OfflineQueued` kind — `fauna.conversations.channel.send`
    /// on the MLS rail, `fauna.email.send` on the mail rail — so composing,
    /// sending, reacting, renaming, deleting and evicting all stay live with no
    /// nest, which is exactly what the outbox exists to carry. The one gesture
    /// that greys is `dm-message-mark-as-spam-button`, and it greys for a
    /// reason that has nothing to do with messaging: it writes the sealed spam
    /// model through `fauna.bridges.put_spam_model`, a *bridge* call.
    ///
    /// ⚠ **The "three classes behind one send" reading is wrong for this app,
    /// and the correction is worth keeping.** `fauna.conversations.group.send_message`
    /// (since retired with the group plane) — the `OfflineSafe` kind that reading
    /// names — was issued by **no** rail backend: tui's MLS rail sends through
    /// `post_app_message` →
    /// `send_on_channel` → `channel.send`, and tui registers exactly two
    /// send-capable rails (`FaunaMlsBackend` in `ConversationsSession::from_parts`,
    /// `SmtpBackend` via `register_smtp`). So the rail selects the
    /// *kind*, never the *class*.
    ///
    /// **Two residual under-claims, both deliberate, both in the safe
    /// direction** (a control stays live rather than greying on a guess —
    /// caution 1's rule):
    /// 1. **The MLS bootstrap.** A *first* send on a FaunaMls thread with no
    ///    bound channel runs `bootstrap_group` — `fauna.conversations.keypackage.fetch`
    ///    plus `fauna.conversations.welcome.deliver`, both `OnlineOnly` —
    ///    before the append. Whether a thread is bound is `FaunaMlsBackend`'s
    ///    private `channels` map, not snapshot state, so no paint can see it.
    /// 2. **Foreign-homed channels.** `send_on_channel` picks
    ///    `channel.send_remote` over `channel.send` when the channel's home is
    ///    another nest, and the home map is likewise backend-private. The two
    ///    are declared class-equivalent by
    ///    `the_channel_send_pair_is_class_equivalent`, so this page's gate
    ///    answer is identical either way — and the day that stops being true,
    ///    that test reds and names this method.
    pub fn wire_kind(&self) -> Option<&'static str> {
        match self {
            // ── The MLS channel's application-message family ────────────────
            // A reaction, a delete tombstone, a rename and a member eviction
            // are all sealed application messages posted through
            // `post_app_message`, so they ride the same append as a chat send.
            // (The eviction's payload is an MLS Commit rather than a chat body,
            // but `evict_leaf_locked` posts it through the same seam.)
            Action::ToggleReaction { .. }
            | Action::CommitReactionEntry
            | Action::ConfirmDeleteMessage
            | Action::ConfirmRename
            | Action::RemoveMember { .. }
            // A policy change is a group-context commit posted through the
            // same seam as the eviction's.
            | Action::SaveRoomSettings => Some("fauna.conversations.channel.send"),
            // The departure is the self-scoped leave door on every class, and
            // both kinds are
            // OnlineOnly, so the
            // gate's answer is the same either way.
            Action::ConfirmRoomLeave => Some("fauna.conversations.room.leave"),
            // The withdrawal is the room plane's revoke door on the home
            // nest — the invitation's envelope is consumed there, so no nest
            // means nothing to withdraw.
            Action::WithdrawRoomInvite { .. } => Some("fauna.conversations.room.revoke_invite"),
            // The catalog page's own inspect gesture, from the room editor.
            Action::InspectRoomLabeler { .. } => Some("fauna.labelers.inspect"),

            // ── The two sends, keyed by the rail the paint saw ──────────────
            Action::SendThread { rail } => rail.send_wire_kind(),
            // A founding is the room ceremony, not a queued append: the room
            // must exist on its home nest before anything is sealed to it.
            Action::SendNewThread {
                founds_room: true, ..
            } => Some("fauna.conversations.room.create"),
            Action::SendNewThread {
                rail,
                founds_room: false,
            } => rail.and_then(|r| r.send_wire_kind()),

            // ── The standing invitations ────────────────────────────────────
            // Accepting seats this account on the room's floor — the nest's
            // act, so it needs one. Declining only settles the invitation's
            // inbox envelope, which queues like any other ack.
            Action::AcceptRoomInvitation { .. } => Some("fauna.conversations.room.accept_invite"),
            Action::DeclineRoomInvitation { .. } => Some("fauna.inbox.ack"),

            // ── Membership: the in-place add is the only wire arm ───────────
            // The add commit opens by fetching the newcomer's key package, and
            // that is what cannot happen without a nest; the Welcome delivery
            // that ends it is `OnlineOnly` too. A 1:1 fork issues nothing.
            Action::ConfirmAddParticipant {
                in_place_mls_group: true,
            } => Some("fauna.conversations.keypackage.fetch"),
            Action::ConfirmAddParticipant {
                in_place_mls_group: false,
            } => None,

            // ── Two gestures that leave the conversations plane entirely ────
            // Keep is a local succession-ledger write (`decide_member_review`
            // — a door put on the account store the pump publishes later), so
            // it declares nothing and stays live offline; the succession
            // review it answers is not a messaging operation at all.
            Action::KeepMember { .. } => None,
            // ⚠ The page's ONE desensitizing gesture. Training the sealed
            // tier-1 model reseals it and PUTs it to the mail bridge; there is
            // deliberately no server-train fallback for conversation content
            // (the nest cannot read it), so with no nest there is nothing this
            // gesture can do.
            Action::MarkMessageSpam { .. } => Some("fauna.bridges.put_spam_model"),

            // ── Network-shaped with no kind at all ──────────────────────────
            // Revealing remote content fetches third-party hosts directly over
            // HTTP — the only fetch on this page that is not this nest — so
            // there is no wire kind to classify. The media page's finding, from
            // the same direction.
            Action::RevealRemoteImages { .. } => None,

            // ── Local by construction ──────────────────────────────────────
            // Navigation, view state, every compose buffer, and the overlay
            // openers whose commit gesture carries the call. The two
            // session-local reveals belong here for a sharper reason than
            // "no call": they write a session set precisely so the mute and the
            // content floor themselves are never touched.
            Action::SelectThread(_)
            | Action::SelectThreadAndMessage { .. }
            | Action::StartNewConversation
            | Action::CancelNewConversation
            | Action::Sort
            | Action::AcceptRecipientChip
            | Action::ToggleTopic
            | Action::MarkdownWrap { .. }
            | Action::OpenAddParticipant
            | Action::OpenRename
            | Action::OpenRoomSettings
            // The opener; its commit gesture (ConfirmRoomLeave) carries the call.
            | Action::StartRoomLeave
            | Action::SetRoomJoinRule(_)
            | Action::SetRoomHistoryPolicy(_)
            | Action::ToggleRoomAdmin { .. }
            | Action::ToggleRoomOwnerTransfer { .. }
            // Both choices are staged: the founding's commit is Send, the
            // read's is Save, and each of those carries the call.
            | Action::ToggleHomeNest
            | Action::ToggleRoomNestRead
            | Action::ToggleRoomLabeler { .. }
            | Action::CloseRoomLabelerInspect
            | Action::ReplyToMessage { .. }
            | Action::CancelReply
            | Action::RemoveReplyRecipient { .. }
            | Action::CommitReplyRecipientAdd
            // Staging an attachment is local file io plus an in-memory hash;
            // the bytes reach the nest at send, not here.
            | Action::CommitAttachment
            | Action::RemoveAttachment { .. }
            | Action::OpenMessageActions { .. }
            | Action::StartReactionEntry
            | Action::StartDeleteMessage
            | Action::RevealMuted { .. }
            | Action::RevealContent { .. } => None,
        }
    }
}

/// Which surface of the conversations page is showing.
///
/// The new-thread compose and the thread detail are ui.yaml **sub_pages** of
/// `conversations`, not modals (`conversations.md` § Layout & flow: "New-thread
/// compose lives in the detail pane, not a modal"). The tui models them as page
/// modes exactly as the feed page models `create_feed` / `post_detail`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    List,
    /// ui.yaml `conversations.sub_pages.compose`.
    Compose,
    /// ui.yaml `conversations.sub_pages.conversation_detail` — the thread shown.
    Detail(ThreadId),
}

/// The conversations page's state, hung off [`App`] rather than a process-wide
/// singleton (linux's `OnceLock` shape): the manager is built on auth and dropped
/// on sign-out, so its lifetime is the session's — which is what [`App`] already
/// scopes. Mirrors [`crate::feed::FeedState`].
#[derive(Default)]
pub struct ConversationsState {
    /// `None` before the first sign-in — the e2e state serializer runs pre-auth,
    /// so every reader degrades gracefully rather than panicking.
    pub manager: Option<Arc<ConversationsManager>>,
    /// Decides which threads warrant a new-message banner, by diffing successive
    /// snapshots — the shared, unit-tested when/for-whom rule
    /// (`conversations.md` § Where logic lives). Fed by
    /// [`fire_message_banners`] on every `ConversationsChanged` tick.
    ///
    /// **Living on this struct is what keeps it fresh per identity**, and that is
    /// load-bearing rather than incidental. A tracker carried across an account
    /// switch would treat the incoming identity's *restored* threads as new and
    /// fire one banner apiece — a toast storm on every switch. Because the whole
    /// `ConversationsState` is replaced at both ends of an identity's life —
    /// `session::establish` on login, `App`'s identity teardown on sign-out —
    /// tui gets that for free, structurally. linux, whose tracker is a local
    /// bound beside a process-lifetime observer, has to re-make one by hand each
    /// time it re-attaches (`fauna-linux/src/main.rs`); here there is no
    /// re-attach path to forget, because there is no surviving state to forget
    /// it in.
    pub notif_tracker: fauna_conversations::MessageNotificationTracker,
    /// The rail's `DraftsSync`, so `main.rs`'s leave-door flush
    /// (`drafts_autosave::flush_now`) can force a save of `manager`'s current
    /// snapshot at quit time without a debounce wait. `None` before the first
    /// sign-in, or if `drafts::start`'s malformed-secret arm disabled
    /// persistence for this session.
    pub drafts_sync: Option<Arc<drafts::ConvDraftsSync>>,
    pub mode: Mode,
    /// The in-progress new-label buffer of the rename overlay (`Some` while it is
    /// open). Rename is the one detail-overlay the manager holds **no** state for
    /// — it takes the final label on `rename_thread`, so the draft lives here,
    /// exactly as linux keeps it in the transient `rename_overlay` widget rather
    /// than the snapshot. (The add-participant overlay, by contrast, is manager
    /// state — `snapshot.add_participant` — so it needs no local field.)
    pub rename_draft: Option<String>,
    /// The room policy editor's staged values (`Some` while the `room_settings`
    /// sub-page is open) — local like [`Self::rename_draft`]: the manager takes
    /// each change on Save and holds no draft.
    pub room_settings: Option<RoomSettingsDraft>,
    /// Whether `room-leave-button` has opened its confirm — the
    /// `ActionsStep::ConfirmDelete` shape, kept beside the draft because the
    /// departure is not one of the draft's staged edits.
    pub room_leave_confirm: bool,
    /// The `dm-reply-recipient-add` To-line input buffer (the typed-but-uncommitted
    /// new reply recipient). Local like [`Self::rename_draft`] — the manager holds
    /// only committed `compose.reply_recipients`; Enter parses + commits this via
    /// `add_reply_recipient`, then clears it. Cleared on thread switch.
    pub reply_recipient_draft: String,
    /// The live [`fauna_conversations::ConversationsSession`] (MLS engine +
    /// `NestConversationsRpc` + the unified receive loop), built by
    /// [`conv_backend::start_conversations_session`] at login. Holding the `Arc`
    /// here keeps the receive loop's liveness `Weak` upgradeable; a re-login
    /// replaces the whole state, dropping it so the old loop exits. `None`
    /// pre-auth or when the MLS engine failed to init (the page then reads the
    /// manager's honest degraded snapshot).
    pub real_session: Option<Arc<fauna_conversations::ConversationsSession>>,
    /// Another instance of this app holds the conversations-engine role over
    /// this account's `mls_state.db` (`MlsError::ServedElsewhere` at engine
    /// init — `account-data-plane.md` § Multi-instance concurrency). A
    /// standing condition for this session, surfaced with top precedence by
    /// [`sync_page_error`] ("served in another instance", the honest refusal
    /// the ruled design demands — never a silent unwired page); everything
    /// non-conversations proceeds normally. Cleared only by the re-login that
    /// rebuilds this whole state.
    pub served_elsewhere: bool,
    /// The in-group succession witness this login registered, kept as its
    /// concrete type so its convention-6 report
    /// ([`fauna_client_recovery::witness::WitnessObservation`]) stays readable.
    /// The session holds the same object behind `dyn SuccessionWitness`, which
    /// deliberately carries no reporting method — a conversations-crate trait
    /// must not name recovery-plane types (priority #2's seam rule) — so the
    /// second handle is how every app reaches the report. Lifetime mirrors
    /// [`Self::real_session`]: built at login, dropped with this state.
    pub succession_witness: Option<Arc<conv_backend::TuiChainWitness>>,
    /// What the peer-anchor harvest sweep did, per peer — the producer half of
    /// the member-path report the witness above gives the consumer half of.
    /// Shared type, so every app inherits the report with the sweep; a session
    /// with no sweep simply has an empty one.
    pub peer_anchor_harvest: Arc<fauna_client_recovery::harvest::HarvestLog>,
    /// The content-index launcher this login wired — the one holder of the MSEK
    /// and the `__index` rail, so it is also the only thing that can open the
    /// sealed slice the Search page's local arm queries
    /// (`crate::search::attach_local_index`). Kept here rather than on
    /// `SearchState` because its lifetime is the conversations session's: it is
    /// built at login and dropped with this state, which ends its flush driver.
    pub index_launcher: Option<Arc<fauna_client_conversations::NestMailIndexLauncher>>,
    /// The per-bubble ⋯ actions overlay (`Some` while open) — a LOCAL overlay
    /// like [`Self::rename_draft`]: the manager holds no menu state (the GUI
    /// apps' flyout is a transient popover too). Esc cancels overlay-first.
    pub actions_overlay: Option<ActionsOverlay>,
    /// The `doc-remote-image` art cache, keyed by **url** — the bubble's twin of
    /// [`crate::feed::FeedState::remote_images`], and this page's only fetch of a
    /// host that is not this nest. Populated solely from urls the reader revealed
    /// (`crate::remote_image`); session-scoped like the reveal set itself, which
    /// render-model.md § D3 keeps in memory and never persists.
    pub remote_images: crate::image_cache::ImageCache,
    /// The `attachment-button` typed-path buffer (the path typed but not yet
    /// staged). Local like [`Self::reply_recipient_draft`] — the manager holds only
    /// *staged* `ComposeState.attachments`, and Enter reads the file and commits it
    /// through `add_attachment`/`add_new_thread_attachment`, then clears this.
    ///
    /// A terminal has no OS file chooser, so path entry **is** the production
    /// affordance here (`apps/tui.md` § Declared platform absences 4: "Drag-and-drop
    /// and OS file pickers — replaced by path entry with completion and a
    /// file-browser widget"); the same shape feed's `compose-file` and profile's
    /// `profile-edit-avatar`/`-banner` already ship.
    pub attach_path_draft: String,
    /// Messages whose muted-keyword collapse the user opened this session
    /// (`dm-message-muted-reveal-button`) — linux's `REVEALED_MUTED_MESSAGES`
    /// thread-local, hung off `App` instead for the same reason the manager is.
    ///
    /// **Session-local by design.** The mute is a *hide/collapse verb, not a
    /// queue flag* (`moderation.md` § Muted keywords), and revealing one instance
    /// must not un-mute the term — so this never touches the sealed list, and it
    /// dies with the session (a re-login re-collapses, which is the honest
    /// behaviour every other app ships).
    pub revealed_muted: std::collections::HashSet<MessageId>,
    /// Messages whose **content-policy `collapse`** the user opened this session
    /// (`family-safety.md` § Content policy) — linux's `REVEALED_CONTENT`. A
    /// separate set from [`Self::revealed_muted`] on purpose: different verbs,
    /// different sources (the user's own muted terms vs. a guardian floor or a
    /// spam threshold), so revealing one must not reveal the other on the same
    /// message. Session-local for the same reason, and it can never reveal a
    /// `block` — that arm returns before any reveal is offered.
    pub revealed_content: std::collections::HashSet<MessageId>,
}

/// The open `dm-message-actions-menu` — which message it targets, which step it
/// is showing, and the free-entry emoji buffer ("more reactions" — the
/// terminal's picker is typing the emoji directly).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionsOverlay {
    pub msg_id: MessageId,
    pub step: ActionsStep,
    /// The `dm-reaction-more-button` free-entry buffer (only read in
    /// [`ActionsStep::EmojiEntry`]).
    pub emoji_draft: String,
}

/// Which face of the actions overlay is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionsStep {
    /// The quick-set row + more/delete entries.
    Menu,
    /// The destructive-action confirm step revealed by `dm-message-delete-button`.
    ConfirmDelete,
    /// The free-entry emoji prompt revealed by `dm-reaction-more-button`.
    EmojiEntry,
}

impl ConversationsState {
    pub fn snapshot(&self) -> Option<fauna_conversations::ConversationsSnapshot> {
        self.manager.as_ref().map(|m| m.snapshot())
    }
}

/// Forward manager notifications into the render loop's `UiMessage` channel.
///
/// `on_changed` fires **synchronously on whatever thread mutated** (a tokio worker
/// for the async methods), so it must not touch `App`. Sending a bare tick +
/// re-reading a fresh snapshot per tick makes coalescing safe: the receiver always
/// reads current state, so a dropped duplicate loses nothing. Direct analogue of
/// [`crate::feed`]'s `TuiFeedObserver` and linux's `GtkConversationsObserver`.
struct TuiConvObserver {
    tx: UnboundedSender<UiMessage>,
}

impl SnapshotObserver for TuiConvObserver {
    fn on_changed(&self) {
        // A closed channel means the app is shutting down — nothing to notify.
        let _ = self
            .tx
            .send(UiMessage::Data(DataMessage::ConversationsChanged));
    }
}

/// Build the conversations manager, attach the observer, and — in e2e mode —
/// install the mock rail backends the cross-app tier_2 suites inject through.
///
/// Called from the one post-auth hook (`session::establish`), so every path that
/// produces a session gets a conversations manager without each remembering to
/// wire one. `establish` then immediately calls
/// [`conv_backend::start_conversations_session`], which registers the **real**
/// FaunaMls + SMTP backends over this manager (overwriting those two mock rail
/// entries — `from_manager` is documented idempotent for exactly this) and
/// starts the unified receive loop, the same ordering linux's AuthSuccess
/// wiring lands in.
///
/// **Mock backends are e2e-only** (`FAUNA_E2E_AGENT_PORT` set), mirroring linux's
/// `host::manager()`: production installs no mocks. After the real session
/// lands, the mocks still back the bridged rail in e2e, and the
/// inject seams (`inject_inbound_for_test` & co.)
/// bypass backends entirely, so the tier_2/tier_3 snapshot suites keep working
/// unchanged.
pub fn init(tx: &UnboundedSender<UiMessage>) -> ConversationsState {
    let manager = ConversationsManager::new();
    manager.add_observer(Arc::new(TuiConvObserver { tx: tx.clone() }));
    // Two gates, both required, exactly as linux's `conversations::host::manager()`
    // does it: `crate::e2e_mode_enabled()` is the runtime switch that picks agent-on
    // vs agent-off *within* a test-capable build, and the `cfg` is the outer boundary
    // keeping the seam out of the release binary (`docs/goal/architecture/testing.md`
    // convention 15 — "a runtime env-var gate alone is not enough").
    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    if crate::e2e_mode_enabled() {
        manager.install_mock_backends_for_test();
    }
    ConversationsState {
        manager: Some(manager),
        // Fresh with the state it is built into — this call site IS the "new
        // identity" edge, so the incoming identity's restored threads seed
        // silently instead of raising a banner apiece.
        notif_tracker: fauna_conversations::MessageNotificationTracker::new(),
        drafts_sync: None,
        mode: Mode::default(),
        rename_draft: None,
        room_settings: None,
        room_leave_confirm: false,
        reply_recipient_draft: String::new(),
        attach_path_draft: String::new(),
        real_session: None,
        served_elsewhere: false,
        succession_witness: None,
        peer_anchor_harvest: Default::default(),
        index_launcher: None,
        actions_overlay: None,
        remote_images: crate::image_cache::ImageCache::new(),
        revealed_muted: std::collections::HashSet::new(),
        revealed_content: std::collections::HashSet::new(),
    }
}

// ── Field access ──────────────────────────────────────────────────────────────

/// A conversations-page editable field (`crate::conversations`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ConversationsField {
    /// `conversation-search-box` — a **local** filter over the loaded threads
    /// (`conversations.md` § Where logic lives), not a nest re-query, so writing
    /// it lands synchronously in the manager and returns no pending work. Reads
    /// back off the manager's snapshot, like the compose fields.
    Search,
    /// `recipient-picker-input` — the new-thread recipient text. Reads/writes the
    /// manager's `new_thread_compose.recipient_picker.raw_input`, and a write
    /// triggers the manager's async backend probe
    /// ([`crate::conversations::PendingResolve`]) exactly as the GUI apps do —
    /// so the write path can return work, like the feed's re-query.
    RecipientInput,
    /// `dm-text-field` — the compose body. Routes to the new-thread draft or the
    /// selected thread's draft by `ConversationsState.mode`.
    ComposeBody,
    /// `subject-input` — the compose subject/topic draft. Routes by mode like
    /// [`Self::ComposeBody`].
    Subject,
    /// `thread-rename-field` — the new-label draft in the rename overlay. A
    /// **local** buffer on [`crate::conversations::ConversationsState`] (the
    /// manager has no rename-draft state; it takes the final label on confirm),
    /// so writing it lands synchronously and returns no pending work.
    ThreadRename,
    /// `dm-reply-recipient-add` — the "type a new recipient" input on the reply
    /// To-line (mail threads). A **local** buffer; Enter commits it as a chip
    /// (`add_reply_recipient` after `try_parse_typed_address`), like the manager
    /// has no draft for this either.
    ReplyRecipientAdd,
    /// The ⋯ actions menu's free-entry emoji prompt (`dm-reaction-more-button`
    /// in entry mode) — a terminal's "fuller emoji picker" is typing the emoji.
    /// A **local** buffer on the open overlay; Enter commits it as a reaction
    /// toggle, like [`Self::ReplyRecipientAdd`]'s submit.
    ReactionEmoji,
    /// `attachment-button` — the typed attachment path. A **local** buffer
    /// ([`crate::conversations::ConversationsState::attach_path_draft`]); Enter
    /// reads the file and stages it on the active composer, like
    /// [`Self::ReplyRecipientAdd`]'s submit. Path entry is tui's declared
    /// replacement for an OS file picker (`apps/tui.md` § Declared platform
    /// absences 4).
    AttachPath,
}

/// Read one of the page's editable fields.
///
/// The search query reads back off the **snapshot**, not a local buffer: the
/// manager owns it (the `conversation-search-box` filter is a manager
/// `set_search_query`), so a bridge-driven write shows up in the input exactly as
/// a keystroke would (the discipline the feed's `FeedField::Search` follows).
pub fn field(state: &ConversationsState, field: &ConversationsField) -> String {
    let snap = state.snapshot();
    match field {
        ConversationsField::Search => snap
            .as_ref()
            .and_then(|s| s.search_query.clone())
            .unwrap_or_default(),
        // Reads whichever recipient picker is active — the add-participant
        // overlay's takes priority over the new-thread composer's, mirroring the
        // manager's `resolve_recipient` / `accept_current_recipient_chip`
        // precedence, so `get_text("recipient-picker-input")` reflects the picker
        // the driver is actually typing into.
        ConversationsField::RecipientInput => snap
            .as_ref()
            .and_then(active_recipient_picker)
            .map(|p| p.raw_input.clone())
            .unwrap_or_default(),
        ConversationsField::ComposeBody => compose_body(state),
        ConversationsField::Subject => compose_subject(state),
        ConversationsField::ThreadRename => state.rename_draft.clone().unwrap_or_default(),
        ConversationsField::ReplyRecipientAdd => state.reply_recipient_draft.clone(),
        ConversationsField::ReactionEmoji => state
            .actions_overlay
            .as_ref()
            .map(|o| o.emoji_draft.clone())
            .unwrap_or_default(),
        ConversationsField::AttachPath => state.attach_path_draft.clone(),
    }
}

/// The recipient picker the client is currently typing into — the
/// add-participant overlay's if it is open, else the new-thread composer's. The
/// read-side twin of the manager's `active_picker_*` precedence (which the
/// add-participant overlay's picker also wins).
fn active_recipient_picker(snap: &ConversationsSnapshot) -> Option<&RecipientPickerState> {
    if let Some(ap) = snap.add_participant.as_ref() {
        return Some(&ap.picker);
    }
    snap.new_thread_compose
        .as_ref()
        .and_then(|c| c.recipient_picker.as_ref())
}

/// The active compose subject draft — the new-thread draft's, or the selected
/// thread's. Empty when the topic input is not shown (`subject_draft: None`).
fn compose_subject(state: &ConversationsState) -> String {
    let Some(manager) = state.manager.as_ref() else {
        return String::new();
    };
    let compose = match &state.mode {
        Mode::Detail(thread_id) => manager.thread_detail(thread_id.clone()).map(|d| d.compose),
        _ => manager.snapshot().new_thread_compose,
    };
    compose.and_then(|c| c.subject_draft).unwrap_or_default()
}

/// Write one of the page's editable fields, handing back the async work the
/// write implies (today only the recipient-resolve).
///
/// The conversation-list filter is a **local** filter over the already-loaded
/// threads (`conversations.md` § Where logic lives: "Deliberately a local
/// filter, NOT a nest re-query"), so `set_search_query` lands synchronously and
/// returns nothing. The recipient-picker input is the one field that implies
/// network work: writing it triggers the manager's backend probe
/// ([`ConversationsManager::resolve_recipient`]) — the same shape the feed's
/// `FeedSearch` returns, unified under [`crate::app::PendingWrite`].
///
/// The manager `?` sits inside the arms that need it, not at the top of the fn —
/// the rename draft, the reply-recipient draft and the overlay's emoji draft are
/// plain local buffers. Gating them on the manager silently dropped the write
/// when none was installed (the "no error, no effect" shape the exhaustive
/// `Field` nesting retires — `apps/tui.md` § Target state); the manager-backed
/// arms below are unchanged.
pub fn set_field(
    state: &mut ConversationsState,
    field: ConversationsField,
    value: String,
) -> Option<PendingResolve> {
    match field {
        ConversationsField::Search => {
            state
                .manager
                .clone()?
                .set_search_query((!value.is_empty()).then_some(value));
            None
        }
        ConversationsField::RecipientInput => {
            let manager = state.manager.clone()?;
            // Two-step, exactly as linux's `recipient_picker` `on_input` does
            // (`set_*_recipient_input` → `resolve_recipient().await`): the
            // synchronous write parks the picker on `resolving` (typing owes a
            // probe — `conversations.md` § Errors & edge cases → *The picker
            // tells the truth*), and the async backend probe — which promotes an
            // email-shaped fauna handle to a Fauna chip and settles the terminal
            // state — is the returned [`PendingResolve`] the caller drives. Overlay-aware: the
            // add-participant overlay's picker takes priority over the new-thread
            // composer's, matching `resolve_recipient`'s own active-picker
            // precedence (so the returned probe settles the same picker this write
            // targeted).
            if manager.snapshot().add_participant.is_some() {
                manager.set_add_participant_recipient_input(value);
            } else {
                manager.set_new_thread_recipient_input(value);
            }
            Some(PendingResolve { manager })
        }
        ConversationsField::ThreadRename => {
            // A local buffer, not a manager mutator — the manager takes the final
            // label only on confirm (`rename_thread`). No network work.
            state.rename_draft = Some(value);
            None
        }
        ConversationsField::ReplyRecipientAdd => {
            // A local buffer too — Enter commits it via `add_reply_recipient`.
            state.reply_recipient_draft = value;
            None
        }
        ConversationsField::AttachPath => {
            // A local buffer as well — Enter (`Action::CommitAttachment`) reads the
            // path and stages the bytes on the active composer.
            state.attach_path_draft = value;
            None
        }
        ConversationsField::ReactionEmoji => {
            // The ⋯ menu's free-entry emoji prompt — a local buffer on the open
            // overlay; Enter commits it via `toggle_reaction`.
            if let Some(overlay) = state.actions_overlay.as_mut() {
                overlay.emoji_draft = value;
            }
            None
        }
        ConversationsField::ComposeBody => {
            let manager = state.manager.clone()?;
            match &state.mode {
                Mode::Detail(thread_id) => manager.set_compose_body(thread_id.clone(), value),
                _ => manager.set_new_thread_body(value),
            }
            None
        }
        ConversationsField::Subject => {
            let manager = state.manager.clone()?;
            match &state.mode {
                Mode::Detail(thread_id) => manager.set_compose_subject(thread_id.clone(), value),
                _ => manager.set_new_thread_subject(Some(value)),
            }
            None
        }
    }
}

/// The async recipient-resolve a `RecipientInput` write implies — the manager's
/// backend probe ([`ConversationsManager::resolve_recipient`]), which promotes a
/// typed address to its real rail/identity (an email-shaped fauna handle → a
/// Fauna chip) and settles the picker's terminal resolve state. Awaited on the
/// agent's type path (element reads are single-shot, so the terminal `state`
/// must be set before `/element/type` replies), spawned on the keyboard's (a
/// slow probe must never freeze the render loop) — the same split feed's
/// [`crate::feed::PendingSearch`] uses, unified under [`crate::app::PendingWrite`].
pub struct PendingResolve {
    manager: Arc<ConversationsManager>,
}

impl PendingResolve {
    pub async fn run(self) {
        self.manager.resolve_recipient().await;
    }
}

// ── Gesture dispatch ────────────────────────────────────────────────────────

/// Whether a modal overlay (add-participant, rename, or the ⋯ actions menu) is
/// open over the detail view. The app keymap consults this so Esc cancels the
/// overlay **first** and only a second Esc leaves the thread (`app.rs`), matching
/// the GUI apps whose dialog Cancel closes the modal without leaving the thread.
pub fn detail_overlay_open(state: &ConversationsState) -> bool {
    state.rename_draft.is_some()
        || state.room_settings.is_some()
        || state.actions_overlay.is_some()
        || state
            .manager
            .as_ref()
            .is_some_and(|m| m.snapshot().add_participant.is_some())
}

/// Cancel whichever detail overlay is open (the Esc affordance). Add-participant
/// is manager state (`cancel_add_participant` clears `snapshot.add_participant`);
/// rename and the ⋯ actions menu are local. Cancelling all is safe — at most one
/// is ever open.
pub fn cancel_detail_overlay(state: &mut ConversationsState) {
    if let Some(manager) = state.manager.as_ref()
        && manager.snapshot().add_participant.is_some()
    {
        manager.cancel_add_participant();
    }
    state.rename_draft = None;
    state.room_settings = None;
    state.room_leave_confirm = false;
    state.actions_overlay = None;
}

/// Apply a conversations gesture's **local** half and hand back its network half,
/// if any — the same split the feed page uses, and for the same reason: the agent's
/// click path must **await** the network op (element reads are single-shot, so a
/// send must have landed before `/element/click` replies), while the keyboard path
/// must **spawn** it (the render loop can never block on a nest). A single
/// `async fn(&mut App)` could serve only the first — `&mut App` cannot cross a
/// `tokio::spawn` — so local state changes land here synchronously and the network
/// half comes back as an [`Op`] owning only `Arc`s.
///
/// The manager's mutators are all `&self` on an `Arc`, so the sync ones run in
/// place; only `send_new_thread` is `async`.
/// Back to the thread list — the one door out of a sub-page, for the
/// `conversations-tab` and for Esc alike.
///
/// tui shows the list **or** a thread, never both, so leaving a thread for the
/// list is the moment it stops being open. The manager has to hear that: it
/// reads whatever arrives in the open thread (`conversations.md` § State & data
/// shape → *When a thread is read*), and a thread left selected behind the list
/// would swallow every later arrival unflagged. The compose sub-page needs no
/// such call — the manager already counts a thread the composer covers as not
/// open, and leaving compose keeps its draft (§ Persistence).
pub fn show_list(state: &mut ConversationsState) {
    if matches!(state.mode, Mode::Detail(_))
        && let Some(manager) = state.manager.as_ref()
    {
        manager.clear_selection();
    }
    state.mode = Mode::List;
}

/// Show `thread_id`'s detail sub-page — the one door onto a thread, for a row
/// the user picked and for the thread a new-thread send just created alike.
/// The transient per-thread drafts (a half-typed rename / reply recipient / an
/// open ⋯ menu) don't follow the user to a different thread. Telling the
/// manager which thread is selected is the caller's: a pick selects it here,
/// while a send has already selected it.
fn show_detail(state: &mut ConversationsState, thread_id: ThreadId) {
    state.mode = Mode::Detail(thread_id);
    state.rename_draft = None;
    state.room_settings = None;
    state.room_leave_confirm = false;
    state.reply_recipient_draft.clear();
    state.actions_overlay = None;
}

pub fn apply_local(app: &mut App, action: Action) -> Option<Op> {
    let manager = app.conversations.manager.clone()?;
    match action {
        Action::SelectThread(id) => {
            let thread_id = ThreadId(id);
            manager.select_thread(thread_id.clone());
            show_detail(&mut app.conversations, thread_id.clone());
            // A thread addressed to one of the account's own mailing lists shows
            // the list-send warning on its compose (`mail-mass-mailing.md`
            // § Composing a list message); the manager answers None for the rest.
            Some(Op::RefreshListSend {
                manager,
                thread_id: Some(thread_id),
            })
        }
        Action::SelectThreadAndMessage {
            thread_id,
            message_id,
        } => {
            let thread_id = ThreadId(thread_id);
            manager.select_thread_and_message(
                thread_id.clone(),
                fauna_conversations::message::MessageId(message_id),
            );
            show_detail(&mut app.conversations, thread_id);
            // Land the focus ring on the selected message. In tui the viewport
            // follows the focus ring (`crate::ui::scroll_offset`), so the focus
            // IS the scroll — the same reason `events::open_time_axis_at_
            // working_start` exists. Deferred to the caller (`crate::search`)
            // because the ring indexes the page's *painted* elements, which do
            // not exist until this mode switch has been rendered.
            None
        }
        Action::StartNewConversation => {
            manager.start_new_conversation();
            app.conversations.mode = Mode::Compose;
            None
        }
        Action::CancelNewConversation => {
            manager.cancel_new_conversation();
            app.conversations.mode = Mode::List;
            None
        }
        Action::Sort => {
            manager.set_sort(next_sort_order(manager.snapshot().sort));
            None
        }
        // Enter on the picker: probe first, then commit what the probe confirmed
        // — the same order web's Enter, linux's `on_accept` and the agent's
        // `accept_recipient` drive. A chip is only ever a probed address
        // (`accept_current_recipient_chip`), so a bare sync accept here would
        // silently no-op whenever Enter beat the keystroke-spawned resolve.
        Action::AcceptRecipientChip => Some(Op::AcceptRecipientChip { manager }),
        // Purely local — the body is already decrypted and in the snapshot; the
        // collapse was only a render decision, so revealing is one set insert.
        Action::RevealMuted { msg_id } => {
            app.conversations.revealed_muted.insert(msg_id);
            None
        }
        // Same reasoning for the content-policy collapse: a render decision over
        // the shared verdict, so revealing is one set insert, never a write.
        Action::RevealContent { msg_id } => {
            app.conversations.revealed_content.insert(msg_id);
            None
        }
        // Not local, and not a fetch either: the manager owns the per-message
        // reveal set (`render-model.md` § D3), so this is one manager call whose
        // `notify()` re-emits a `thread_detail` with `RemoteImage.revealed` /
        // the og:image's `revealed` flipped for this message. The feed's
        // `Action::RevealRemoteImages` arm is the same one-liner.
        Action::RevealRemoteImages { msg_id } => {
            manager.reveal_remote_images(msg_id);
            None
        }
        Action::ToggleTopic => {
            // The topic toggle applies to whichever compose is active: the
            // new-thread draft has no ThreadId, so it rides `set_new_thread_subject`
            // (Some("") reveals the input, None hides it); a selected thread rides
            // `toggle_topic(thread_id)`.
            match &app.conversations.mode {
                Mode::Detail(thread_id) => manager.toggle_topic(thread_id.clone()),
                _ => {
                    let showing = manager
                        .snapshot()
                        .new_thread_compose
                        .and_then(|c| c.subject_draft)
                        .is_some();
                    manager.set_new_thread_subject((!showing).then(String::new));
                }
            }
            None
        }
        Action::MarkdownWrap { prefix, suffix } => {
            wrap_compose_body(&app.conversations, &manager, prefix, suffix);
            None
        }
        // `rail` and `founds_room` are the gate's discriminants only — the
        // manager re-derives both (and flushes an uncommitted recipient this
        // cannot see), so the dispatch deliberately ignores them.
        Action::SendNewThread { .. } => {
            // No pre-send address re-bind needed: the SMTP rail reads the
            // session's live self-address cell at send time (seeded at login,
            // pushed by the `SelfAddressRefreshed` handler — `conversations.md`
            // § State & data shape → *Self-address: live, never baked*).
            Some(Op::SendNewThread { manager })
        }
        Action::ToggleHomeNest => {
            let included = manager
                .snapshot()
                .new_thread_compose
                .and_then(|c| c.recipient_picker)
                .is_some_and(|p| p.include_home_nest);
            manager.set_new_thread_home_nest(!included);
            None
        }
        Action::AcceptRoomInvitation { id } => Some(Op::AcceptRoomInvitation { manager, id }),
        Action::DeclineRoomInvitation { id } => Some(Op::DeclineRoomInvitation { manager, id }),

        // ── the conversation_detail overlays ──
        Action::OpenAddParticipant => {
            if let Mode::Detail(thread_id) = &app.conversations.mode {
                manager.open_add_participant(thread_id.clone());
            }
            None
        }
        // The membership change is async (the FaunaMls 1:1 fork or the in-place
        // group add), so it rides an [`Op`] — awaited on the agent's click
        // path (the overlay must be gone before `/element/click` replies, the
        // driver's dialog-closed proxy), spawned on the keyboard's. The manager
        // clears `snapshot.add_participant` itself, closing the overlay via the
        // observer tick.
        // Same posture: `in_place_mls_group` is the paint's copy of a test
        // `confirm_add_participant` makes for itself.
        Action::ConfirmAddParticipant {
            in_place_mls_group: _,
        } => Some(Op::ConfirmAddParticipant { manager }),
        // The eviction is async (posts the MLS Commit), so it rides an [`Op`]
        // like the add — awaited on the agent's click path, spawned on the
        // keyboard's. The chip only paints as a gesture in Detail mode, so the
        // mode guard is a belt, not a branch.
        Action::RemoveMember { addr } => {
            let Mode::Detail(thread_id) = &app.conversations.mode else {
                return None;
            };
            Some(Op::RemoveMember {
                manager,
                thread_id: thread_id.clone(),
                addr,
            })
        }
        // Keep needs no thread and no manager: the verdict is about the person,
        // not about this group, and it closes every open item for them (a second
        // group's chip for the same person un-marks in the same frame). No
        // confirm — the ratified shape for both verdicts.
        Action::KeepMember { person } => app
            .ledger_store
            .clone()
            .map(|store| Op::KeepMember { store, person }),
        Action::OpenRename => {
            // Seed the field with the thread's current label (linux reads it off
            // `thread_detail` too). No-op outside a detail view.
            if let Mode::Detail(thread_id) = &app.conversations.mode {
                let current = manager
                    .thread_detail(thread_id.clone())
                    .map(|d| d.label)
                    .unwrap_or_default();
                app.conversations.rename_draft = Some(current);
            }
            None
        }
        Action::ConfirmRename => {
            let Mode::Detail(thread_id) = &app.conversations.mode else {
                return None;
            };
            let thread_id = thread_id.clone();
            // Take the draft (exits edit mode optimistically); the relabel lands
            // via the manager's own notify.
            let label = app.conversations.rename_draft.take().unwrap_or_default();
            // Ignore an all-whitespace rename — linux's overlay gates Save on a
            // non-empty trimmed field, so a blank confirm is a no-op, not a wipe.
            let label = label.trim().to_string();
            if label.is_empty() {
                return None;
            }
            Some(Op::RenameThread {
                manager,
                thread_id,
                label,
            })
        }

        // ── the room policy editor ──
        Action::OpenRoomSettings => {
            if let Mode::Detail(thread_id) = &app.conversations.mode
                && let Some(detail) = manager.thread_detail(thread_id.clone())
            {
                app.conversations.room_settings = RoomSettingsDraft::seed(&detail);
            }
            // A room with a labeler set to stage paints the catalog's
            // publishable rows, so the editor re-reads the catalog as it
            // opens: it is nest-global, and the post-auth read predates any
            // labeler published since. A view left open on the Community
            // labelers page must not greet the editor either, so it closes
            // first — through the machine, or the re-read would restore it.
            let stages_labelers = app
                .conversations
                .room_settings
                .as_ref()
                .is_some_and(|draft| draft.labelers.is_some());
            let machine = app.settings.labeler_catalog.machine.clone()?;
            if !stages_labelers {
                return None;
            }
            machine.close_inspect();
            app.settings.labeler_catalog.snapshot = Some(machine.snapshot());
            Some(Op::RoomLabelerCatalog {
                machine,
                inspect: None,
            })
        }
        Action::ToggleRoomLabeler { labeler } => {
            let Mode::Detail(thread_id) = &app.conversations.mode else {
                return None;
            };
            // A greyed control must not act, whoever raced it: the draft's own
            // bound covers a full set, the capability is this app's term.
            let may_set = manager
                .thread_detail(thread_id.clone())
                .is_some_and(|d| d.capabilities.can_set_policy);
            if may_set && let Some(draft) = app.conversations.room_settings.as_mut() {
                draft.toggle_labeler(&labeler);
            }
            None
        }
        Action::InspectRoomLabeler { index } => {
            app.conversations.room_settings.as_ref()?;
            let machine = app.settings.labeler_catalog.machine.clone()?;
            Some(Op::RoomLabelerCatalog {
                machine,
                inspect: Some(index),
            })
        }
        // Synchronous on the machine, like the catalog page's own close — and
        // through it for the same reason: clearing only the local copy would
        // let the next re-read pop the view back open.
        Action::CloseRoomLabelerInspect => {
            if let Some(machine) = app.settings.labeler_catalog.machine.as_ref() {
                machine.close_inspect();
                app.settings.labeler_catalog.snapshot = Some(machine.snapshot());
            }
            None
        }
        Action::SetRoomJoinRule(token) => {
            if let Some(draft) = app.conversations.room_settings.as_mut() {
                draft.set_join_rule_token(&token);
            }
            None
        }
        Action::SetRoomHistoryPolicy(token) => {
            if let Some(draft) = app.conversations.room_settings.as_mut() {
                draft.set_history_policy_token(&token);
            }
            None
        }
        // Both toggles carry the PAINT index, which names a row in the LIVE
        // participant list — so the live list is what resolves it to a person
        // (`RoomSettingsDraft::slot_of`). Reading the draft's own vectors at
        // that index instead was the defect: a roster that moved under the
        // open overlay aimed the gesture at the wrong slot.
        Action::ToggleRoomAdmin { index } => {
            let Mode::Detail(thread_id) = &app.conversations.mode else {
                return None;
            };
            let participants = manager.thread_detail(thread_id.clone())?.participants;
            if let Some(draft) = app.conversations.room_settings.as_mut() {
                draft.toggle_admin(index, &participants);
            }
            None
        }
        Action::ToggleRoomOwnerTransfer { index } => {
            let Mode::Detail(thread_id) = &app.conversations.mode else {
                return None;
            };
            let participants = manager.thread_detail(thread_id.clone())?.participants;
            if let Some(draft) = app.conversations.room_settings.as_mut() {
                draft.toggle_transfer(index, &participants);
            }
            None
        }
        Action::ToggleRoomNestRead => {
            if let Some(draft) = app.conversations.room_settings.as_mut() {
                draft.toggle_nest_read();
            }
            None
        }
        Action::StartRoomLeave => {
            // Local, and only while the editor is open — the control is painted
            // inside it. Gated on the capability as well as on the paint: the
            // button is greyed for the owner, and a greyed control must not be
            // openable by a driver or by a gesture raced against a snapshot
            // that has since named this seat the owner.
            let Mode::Detail(thread_id) = &app.conversations.mode else {
                return None;
            };
            let may_leave = manager
                .thread_detail(thread_id.clone())
                .is_some_and(|d| d.capabilities.can_leave_room);
            if app.conversations.room_settings.is_some() && may_leave {
                app.conversations.room_leave_confirm = true;
            }
            None
        }
        Action::ConfirmRoomLeave => {
            let Mode::Detail(thread_id) = &app.conversations.mode else {
                return None;
            };
            let thread_id = thread_id.clone();
            // The editor closes with the gesture: whatever was staged in it
            // belongs to a room this account is walking out of.
            app.conversations.room_settings = None;
            app.conversations.room_leave_confirm = false;
            Some(Op::LeaveRoom { manager, thread_id })
        }
        Action::WithdrawRoomInvite { invitee_actor_hex } => {
            // Only from inside the editor, where the row is painted; the
            // editor stays open — the list is read again on success and the
            // row leaves it, and a refusal shows on `error-message` beside
            // whatever was staged.
            let Mode::Detail(thread_id) = &app.conversations.mode else {
                return None;
            };
            app.conversations.room_settings.as_ref()?;
            Some(Op::WithdrawRoomInvite {
                manager,
                thread_id: thread_id.clone(),
                invitee_actor_hex,
            })
        }
        Action::SaveRoomSettings => {
            let Mode::Detail(thread_id) = &app.conversations.mode else {
                return None;
            };
            let thread_id = thread_id.clone();
            let draft = app.conversations.room_settings.as_ref()?;
            let detail = manager.thread_detail(thread_id.clone())?;
            let edits = draft.edits(&detail.participants);
            if edits.is_empty() {
                // Nothing staged: Save is a close, not a commit.
                app.conversations.room_settings = None;
                app.conversations.room_leave_confirm = false;
                return None;
            }
            Some(Op::SaveRoomSettings {
                manager,
                thread_id,
                edits,
            })
        }

        // ── the reply compose bar ──
        Action::ReplyToMessage { msg_id, reply_all } => {
            if let Mode::Detail(thread_id) = &app.conversations.mode {
                // Seeds `compose.reply_to` + `reply_recipients` (sender-only, or
                // every participant-but-self for reply-all); the To-line then
                // renders the seeded chips off the snapshot.
                manager.start_reply(thread_id.clone(), msg_id, reply_all);
            }
            None
        }
        Action::CancelReply => {
            if let Mode::Detail(thread_id) = &app.conversations.mode {
                manager.set_reply_to(thread_id.clone(), None);
            }
            None
        }
        Action::RemoveReplyRecipient { addr } => {
            if let Mode::Detail(thread_id) = &app.conversations.mode {
                manager.remove_reply_recipient(thread_id.clone(), addr);
            }
            None
        }
        Action::CommitReplyRecipientAdd => {
            if let Mode::Detail(thread_id) = &app.conversations.mode {
                let thread_id = thread_id.clone();
                // Parse the buffer; commit + clear only on a valid address (a
                // malformed one stays so the user can fix it, matching linux's
                // Entry which rejects an unparseable add).
                if let Some(addr) = fauna_conversations::try_parse_typed_address(
                    &app.conversations.reply_recipient_draft,
                ) {
                    manager.add_reply_recipient(thread_id, addr);
                    app.conversations.reply_recipient_draft.clear();
                }
            }
            None
        }
        Action::SendThread { rail: _ } => {
            let Mode::Detail(thread_id) = &app.conversations.mode else {
                return None;
            };
            let thread_id = thread_id.clone();
            // Same live-cell stance as the new-thread send path above.
            Some(Op::SendThread { manager, thread_id })
        }
        Action::CommitAttachment => {
            let path = app.conversations.attach_path_draft.trim().to_string();
            if path.is_empty() {
                return None;
            }
            // Read now, not at send: the composer must show the chip immediately
            // (`dm-compose-attachment-chip`), which is also what lets the user
            // unstage before sending. Feed's `compose-file` defers its read
            // because a post has no staged-attachment surface to feed.
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(e) => {
                    // Loud, not silent — and the buffer survives for a retry. A
                    // client-glue error goes straight to `App::errors`, per the
                    // page-module contract (`tui.md`: the manager's own error takes
                    // precedence on the next tick).
                    app.errors
                        .insert(crate::pages::Page::Conversations, format!("{path}: {e}"));
                    return None;
                }
            };
            let filename = std::path::Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone());
            // The mime the OS dialog would have handed a GUI app, from the
            // shared guesser every app uses (linux's `.on_attach` calls the same).
            let mime = fauna_conversations::compose::guess_mime_type(&filename).to_string();
            match &app.conversations.mode {
                Mode::Detail(thread_id) => {
                    manager.add_attachment(thread_id.clone(), filename, mime, bytes);
                }
                _ => {
                    // `None` = no new-thread compose is open, so the bytes would go
                    // nowhere. Report it rather than dropping the user's file
                    // silently (the manager's one fallible staging path).
                    if manager
                        .add_new_thread_attachment(filename, mime, bytes)
                        .is_none()
                    {
                        app.errors.insert(
                            crate::pages::Page::Conversations,
                            conversations::unified::ERROR_ATTACHMENT_NO_COMPOSER.to_string(),
                        );
                        return None;
                    }
                }
            }
            app.conversations.attach_path_draft.clear();
            None
        }
        Action::RemoveAttachment { index } => {
            match &app.conversations.mode {
                Mode::Detail(thread_id) => manager.remove_attachment(thread_id.clone(), index),
                _ => manager.remove_new_thread_attachment(index),
            }
            None
        }

        // ── the per-bubble ⋯ actions menu ──
        Action::OpenMessageActions { msg_id } => {
            app.conversations.actions_overlay = Some(ActionsOverlay {
                msg_id,
                step: ActionsStep::Menu,
                emoji_draft: String::new(),
            });
            None
        }
        Action::ToggleReaction { msg_id, emoji } => {
            let Mode::Detail(thread_id) = &app.conversations.mode else {
                return None;
            };
            let thread_id = thread_id.clone();
            // A quick-set pick closes the menu (linux's popdown-on-click); a pill
            // tap has no overlay open, so this is a harmless no-op there.
            app.conversations.actions_overlay = None;
            Some(Op::ToggleReaction {
                manager,
                thread_id,
                msg_id,
                emoji,
            })
        }
        Action::StartReactionEntry => {
            if let Some(overlay) = app.conversations.actions_overlay.as_mut() {
                overlay.step = ActionsStep::EmojiEntry;
            }
            None
        }
        Action::CommitReactionEntry => {
            let overlay = app.conversations.actions_overlay.take()?;
            let emoji = overlay.emoji_draft.trim().to_string();
            if emoji.is_empty() {
                // Nothing typed: keep the prompt open for a fix, matching the
                // reply To-line's reject-don't-clear on a malformed address.
                app.conversations.actions_overlay = Some(overlay);
                return None;
            }
            let Mode::Detail(thread_id) = &app.conversations.mode else {
                return None;
            };
            Some(Op::ToggleReaction {
                manager,
                thread_id: thread_id.clone(),
                msg_id: overlay.msg_id,
                emoji,
            })
        }
        Action::StartDeleteMessage => {
            if let Some(overlay) = app.conversations.actions_overlay.as_mut() {
                overlay.step = ActionsStep::ConfirmDelete;
            }
            None
        }
        Action::ConfirmDeleteMessage => {
            let overlay = app.conversations.actions_overlay.take()?;
            let Mode::Detail(thread_id) = &app.conversations.mode else {
                return None;
            };
            Some(Op::DeleteMessage {
                manager,
                thread_id: thread_id.clone(),
                msg_id: overlay.msg_id,
            })
        }
        Action::MarkMessageSpam { msg_id } => {
            // Close the menu regardless (fire-and-forget, no confirm step —
            // android/linux parity). Then, if every precondition holds, hand back
            // the sealed-train op; any missing one is a **silent no-op** (mail not
            // enabled → no machine / v-older nest) — a conversation message is
            // client-only content the nest can't read, so there is no server-train
            // fallback (`mail-spam.md` § Training signal sources 1).
            app.conversations.actions_overlay = None;
            let machine = app.settings.mail_machine()?;
            let Mode::Detail(thread_id) = &app.conversations.mode else {
                return None;
            };
            let detail = manager.thread_detail(thread_id.clone())?;
            let msg = detail.messages.iter().find(|m| m.message_id == msg_id)?;
            // The button only paints on a received message, but re-check here as
            // defence-in-depth; an empty body is untrainable.
            if msg.is_own || msg.body.trim().is_empty() {
                return None;
            }
            Some(Op::MarkMessageSpam {
                machine,
                // Stored **opaque** as the sealed row's reference (never decoded
                // nest-side) — its UTF-8 bytes, exactly as linux/android pass them.
                message_id: msg.message_id.0.clone().into_bytes(),
                body: msg.body.clone(),
                // A received conversation message usually carries no subject; the
                // shared façade derives a body snippet when this is empty (its
                // single home — priorities #2/#4), so pass the raw value.
                subject: msg.subject_line.clone().unwrap_or_default(),
            })
        }
    }
}

/// The network half of a conversations gesture — owns only an `Arc`, so it can be
/// awaited on the agent's path or spawned on the keyboard's. Resolves to an
/// [`Outcome`]: an error to surface on `error-message`, or `Done` on success.
pub enum Op {
    SendNewThread {
        manager: Arc<ConversationsManager>,
    },
    /// Enter on the recipient picker — resolve, then commit the resolved chip
    /// (`resolve_recipient` → `accept_current_recipient_chip`). Nothing to
    /// surface either way: the picker's own `recipient-resolve-status` carries
    /// the verdict (`conversations.md` § Errors & edge cases → *The picker
    /// tells the truth*), and a refused commit leaves the typed text in place.
    AcceptRecipientChip {
        manager: Arc<ConversationsManager>,
    },
    /// Re-derive a compose's list-send view (`refresh_list_send`) — the opened
    /// thread's, or the new-thread compose's for `None`. The snapshot notify
    /// repaints; nothing to surface.
    RefreshListSend {
        manager: Arc<ConversationsManager>,
        thread_id: Option<ThreadId>,
    },
    /// `add-participant-confirm` — apply the membership change (`confirm_add_participant`).
    ConfirmAddParticipant {
        manager: Arc<ConversationsManager>,
    },
    /// A `thread-member-chip` tap — evict that participant from the group
    /// (`remove_participant`; posts the MLS Commit, no Welcome).
    RemoveMember {
        manager: Arc<ConversationsManager>,
        thread_id: ThreadId,
        addr: TypedAddress,
    },
    /// `thread-member-keep-button[i]` — record the owner's *Keep* on every open
    /// review item for that person (`identity-succession.md` § Propagation →
    /// *MLS groups*).
    ///
    /// Carries the shared succession-ledger seam rather than a nest handle +
    /// secret: the whole write is [`fauna_client_config::decide_member_review`],
    /// so this layer names who was pressed and nothing else.
    KeepMember {
        store: Arc<dyn fauna_client_config::SuccessionLedgerStore>,
        person: ActorId,
    },
    /// `thread-rename-confirm` — commit the renamed label (`rename_thread`).
    RenameThread {
        manager: Arc<ConversationsManager>,
        thread_id: ThreadId,
        label: String,
    },
    /// `room-settings-save-button` — the staged policy changes, committed one
    /// by one (`RoomSettingsEdit`); stops at the first refusal, which the
    /// manager has already painted on `error-message`.
    SaveRoomSettings {
        manager: Arc<ConversationsManager>,
        thread_id: ThreadId,
        edits: Vec<RoomSettingsEdit>,
    },
    /// The room editor's read of the labeler catalog — the Community labelers
    /// page's own machine: `inspect: None` re-reads it as the editor opens,
    /// `Some(i)` opens `room-labeler-inspect-button[i]`'s view (the catalog
    /// snapshot index).
    RoomLabelerCatalog {
        machine: Arc<fauna_labeler_catalog_machine::LabelerCatalogMachine>,
        inspect: Option<u32>,
    },
    /// `room-leave-confirm` — walk out of the room (`leave_room`). One verb;
    /// the rail picks the door by the room's class, so this app never branches
    /// on it (`conversation-rooms.md` § Roles and authorization → *Leaving —
    /// the mechanism*). A refusal is already on `error-message`.
    LeaveRoom {
        manager: Arc<ConversationsManager>,
        thread_id: ThreadId,
    },
    /// `room-pending-invite-withdraw-button[i]` — withdraw the invitation
    /// pending for `invitee_actor_hex` on the open room
    /// (`withdraw_room_invite`). Acts at once; the manager re-lists on
    /// success and puts a refusal on `error-message`.
    WithdrawRoomInvite {
        manager: Arc<ConversationsManager>,
        thread_id: ThreadId,
        invitee_actor_hex: String,
    },
    /// `room-invitation-accept-button[i]` — accept the standing invitation
    /// (`accept_room_invitation`), which selects the room's thread on success.
    AcceptRoomInvitation {
        manager: Arc<ConversationsManager>,
        id: i64,
    },
    /// `room-invitation-decline-button[i]` — decline it
    /// (`decline_room_invitation`).
    DeclineRoomInvitation {
        manager: Arc<ConversationsManager>,
        id: i64,
    },
    /// `dm-send-button` in Detail — send the open thread's draft (`send`).
    SendThread {
        manager: Arc<ConversationsManager>,
        thread_id: ThreadId,
    },
    /// `dm-reaction-option` / a `dm-reaction-pill` tap / the free-entry commit —
    /// toggle `emoji` on a message (`toggle_reaction`).
    ToggleReaction {
        manager: Arc<ConversationsManager>,
        thread_id: ThreadId,
        msg_id: MessageId,
        emoji: String,
    },
    /// `dm-message-delete-confirm-button` — post the cooperative tombstone
    /// (`delete_message`).
    DeleteMessage {
        manager: Arc<ConversationsManager>,
        thread_id: ThreadId,
        msg_id: MessageId,
    },
    /// `dm-message-mark-as-spam-button` — train the sealed tier-1 spam model over
    /// a received message's body via the shared façade
    /// [`MailSettingsMachine::train_spam_model_client_mail`] (the live `Insert`
    /// consumer; `mail-spam.md` § Wire shapes). Fire-and-forget + **silent**: a
    /// degraded outcome (mail not enabled / no `spam-model-sealed-at-rest`) is a
    /// no-op with no server-train fallback, and even a transport error never
    /// surfaces on the conversation page (android parity — the training gesture
    /// must not blank the thread). Resolves to `Done` unconditionally.
    MarkMessageSpam {
        machine: Arc<MailSettingsMachine>,
        message_id: Vec<u8>,
        body: String,
        subject: String,
    },
    /// Fetch + rasterize the **revealed** `doc-remote-image` bodies for `urls` —
    /// the bubble's twin of `feed::Op::FetchRemoteImages`, and the only op here
    /// that touches no manager and no nest. Built solely from
    /// [`crate::document::revealed_remote_image_urls`], so a blocked image's url
    /// cannot reach it.
    FetchRemoteImages {
        urls: Vec<String>,
    },
}

/// What an [`Op`] resolved to; folded back into the page by [`apply_outcome`].
///
/// A single variant: the manager's own snapshot tick drives the redraw, and a
/// send failure is already stamped on the manager's `send_state` before this
/// arrives (`Manager::send`/`send_new_thread` call `set_send_state(Failed)` on
/// error, before returning), so there is no separate error payload to carry —
/// [`apply_outcome`] resyncs `error-message` from that manager-owned truth via
/// [`sync_page_error`] regardless of which way the op resolved.
#[derive(Debug)]
pub enum Outcome {
    Done,
    /// A new-thread send returned — sent, refused, or not attempted at all (no
    /// recipient). Whichever: the page follows the manager, which is what
    /// decides whether a thread now holds the draft ([`apply_outcome`]).
    NewThreadSent,
    /// The room policy editor's Save ran: `true` when every staged commit
    /// landed (the editor closes), `false` when one was refused (it stays
    /// open, the refusal already on the page's `error-message`).
    RoomSettingsSaved(bool),
    /// An invitation was accepted: the room's thread, which the manager has
    /// selected, or `None` when it was refused (on `error-message` already).
    RoomJoined(Option<ThreadId>),
    /// The labeler catalog after the room editor's read of it. `inspected`
    /// says whether the user asked for it (an inspect), which is the only
    /// case whose failure is the page's to report: the opening re-read is
    /// best-effort, and a failed one keeps the rows it had.
    RoomLabelerCatalog {
        snapshot: Box<fauna_labeler_catalog_machine::LabelerCatalogSnapshot>,
        inspected: bool,
    },
    /// Rasterized `doc-remote-image` art (or `None` for a fetch/decode failure)
    /// per url — a pure cache write into [`ConversationsState::remote_images`],
    /// never a banner: one unreachable image in an inbound message is a
    /// placeholder, not an error the reader must dismiss.
    RemoteImages(Vec<(String, Option<crate::thumbnail::Thumbnail>)>),
    /// The open review roster, re-read after a *Keep* landed
    /// (`identity-succession.md` § Propagation → *MLS groups*).
    ///
    /// The re-read is carried back rather than the pressed person, and that is
    /// the point: the store's merge is what decides what is still open (a peer
    /// device may have answered someone else in the same window), so dropping
    /// the pressed row locally would be this app guessing at a merge it does not
    /// run. `None` when the write or the re-read failed — the marks then stay as
    /// they were, which over-asks rather than hiding a flagged person.
    MemberReviews(Option<Vec<fauna_core::data::MemberReview>>),
}

impl Op {
    pub async fn run(self) -> Outcome {
        match self {
            // `send_new_thread` first flushes a typed-but-uncommitted recipient
            // (the manager's own contract), so Send never silently drops a pending
            // recipient. On failure it has already stamped `send_state = Failed`
            // before returning, so the Err is dropped here — `apply_outcome`
            // lands the page on the thread and resyncs `error-message` from
            // that stamp via `sync_page_error`.
            Op::SendNewThread { manager } => {
                let _ = manager.send_new_thread().await;
                Outcome::NewThreadSent
            }
            Op::AcceptRecipientChip { manager } => {
                manager.resolve_recipient().await;
                manager.accept_current_recipient_chip();
                manager.refresh_list_send(None).await;
                Outcome::Done
            }
            Op::RefreshListSend { manager, thread_id } => {
                manager.refresh_list_send(thread_id).await;
                Outcome::Done
            }
            // The overlay closes and the roster updates via the manager's own
            // snapshot notify — nothing to surface on success. A refused wire op
            // (a person not reachable yet, `conversations.md` § Participants vs.
            // reply recipients) rolls the optimistic add back and lands on the
            // manager's page-error slot, resynced onto `error-message` by
            // `apply_outcome` → `sync_page_error`, exactly as `RemoveMember` below.
            Op::ConfirmAddParticipant { manager } => {
                manager.confirm_add_participant().await;
                Outcome::Done
            }
            // The roster shrinks via the manager's own snapshot notify; a
            // failed wire op lands on the manager's page-error slot (the same
            // truth `conv_backend::page_error` reads), resynced onto
            // `error-message` by `apply_outcome` → `sync_page_error`.
            Op::RemoveMember {
                manager,
                thread_id,
                addr,
            } => {
                manager.remove_participant(thread_id, addr).await;
                Outcome::Done
            }
            // The verdict is a succession-ledger write, so unlike every other op here
            // it has no manager snapshot to repaint from — the roster is re-read
            // and carried back instead. A failed write and a failed re-read are
            // deliberately the same outcome: both leave the mark standing, which
            // is the over-ask direction § Propagation blesses.
            Op::KeepMember { store, person } => {
                let refreshed = match fauna_client_config::decide_member_review(
                    store.as_ref(),
                    &person,
                    fauna_core::data::UnattestedVerdict::Kept,
                )
                .await
                {
                    Ok(_) => fauna_client_config::load_member_reviews(store.as_ref())
                        .await
                        .inspect_err(|e| {
                            tracing::warn!(error = %e, "re-reading the review roster after Keep failed")
                        })
                        .ok(),
                    Err(e) => {
                        tracing::warn!(error = %e, "recording Keep on a member review failed");
                        None
                    }
                };
                Outcome::MemberReviews(refreshed)
            }
            // The relabel is applied to the snapshot immediately inside
            // `rename_thread` (before the wire op), so there is nothing to report
            // back — the observer tick repaints the new label.
            Op::RenameThread {
                manager,
                thread_id,
                label,
            } => {
                manager.rename_thread(thread_id, label).await;
                Outcome::Done
            }
            // One manager call per staged change, in order; the first refusal
            // ends the run — the manager paints it, and the editor stays open
            // so the user sees which value was refused still staged. The loop
            // itself is shared (`ConversationsManager::apply_room_settings`),
            // so all seven apps agree on the order and on what "landed" means.
            Op::SaveRoomSettings {
                manager,
                thread_id,
                edits,
            } => Outcome::RoomSettingsSaved(manager.apply_room_settings(thread_id, edits).await),
            // Every machine gesture returns `()`; a failure lands on the
            // snapshot's `error`, which the fold decides whether to show.
            Op::RoomLabelerCatalog { machine, inspect } => {
                match inspect {
                    Some(index) => machine.inspect(index).await,
                    None => machine.refresh().await,
                }
                Outcome::RoomLabelerCatalog {
                    snapshot: Box::new(machine.snapshot()),
                    inspected: inspect.is_some(),
                }
            }
            // The departure. One manager call — the room's class picks the
            // door inside shared Rust, so this app never asks which. A refusal
            // (the owner's, or a report the floor did not take) is already on
            // `error-message` when this returns; the thread itself stays in
            // the list, because the user keeps their own copy of it.
            Op::LeaveRoom { manager, thread_id } => {
                manager.leave_room(thread_id).await;
                Outcome::Done
            }
            // The withdrawal. One manager call; the list is read again inside
            // it on success (the row leaves the editor on the observer tick),
            // and a refusal is already on `error-message` when this returns.
            // The invitee is told nothing — the envelope simply stops standing.
            Op::WithdrawRoomInvite {
                manager,
                thread_id,
                invitee_actor_hex,
            } => {
                manager
                    .withdraw_room_invite(thread_id, invitee_actor_hex)
                    .await;
                Outcome::Done
            }
            Op::AcceptRoomInvitation { manager, id } => {
                Outcome::RoomJoined(manager.accept_room_invitation(id).await)
            }
            // Declining tells the room nothing, so there is nothing to follow;
            // a refusal is on the manager's page-error slot.
            Op::DeclineRoomInvitation { manager, id } => {
                manager.decline_room_invitation(id).await;
                Outcome::Done
            }
            // A per-thread send. Like `SendNewThread`, a failed send stamps
            // `send_state = Failed` before returning (surfaced via
            // `sync_page_error`); on success the compose bar clears via the
            // observer tick (`send_state` → Idle).
            Op::SendThread { manager, thread_id } => {
                let _ = manager.send(thread_id).await;
                Outcome::Done
            }
            // The reaction lands in the snapshot optimistically inside
            // `toggle_reaction` (the pill repaints via the observer tick); a
            // failed wire op is logged by the manager, not a user error.
            Op::ToggleReaction {
                manager,
                thread_id,
                msg_id,
                emoji,
            } => {
                manager.toggle_reaction(thread_id, msg_id, emoji).await;
                Outcome::Done
            }
            // The tombstone lands in the snapshot inside `delete_message` (the
            // bubble repaints as `dm-message-deleted` via the observer tick).
            Op::DeleteMessage {
                manager,
                thread_id,
                msg_id,
            } => {
                manager.delete_message(thread_id, msg_id).await;
                Outcome::Done
            }
            // Fire-and-forget + silent (see the variant doc): the façade seals +
            // writes the model client-side when mail is enabled and the nest
            // advertises `spam-model-sealed-at-rest`, and degrades to a no-op
            // otherwise. The result is dropped exactly as android/linux drop it —
            // a training gesture must never blank the conversation page. The
            // mailbox is the honest display default `"INBOX"` (a conversation
            // message has no IMAP mailbox); `is_spam=true`.
            Op::MarkMessageSpam {
                machine,
                message_id,
                body,
                subject,
            } => {
                let _ = machine
                    .train_spam_model_client_mail(
                        body,
                        true,
                        message_id,
                        "INBOX".to_string(),
                        subject,
                    )
                    .await;
                Outcome::Done
            }
            Op::FetchRemoteImages { urls } => Outcome::RemoteImages(
                crate::remote_image::fetch_all(urls, crate::thumbnail::POST_IMAGE_COLS).await,
            ),
        }
    }
}

/// Mirror the active compose's `send_state` onto the page's `error-message`
/// (`tui.md` § The page-module contract: "Sync on both edges the manager can
/// change it — the fold of any manager-driven `Outcome`, and the observer
/// tick"; Media's `sync_page_error` is the reference). **The manager owns the
/// page error**, not this page: a failure stamped on the active compose by
/// ANY path — a direct `send`/`send_new_thread` dispatch, or an out-of-band
/// mutation the observer tick alone witnesses (e.g. `inject_send_failure_for_test`)
/// — must surface here, or it reads exactly like a dropped command
/// (`../testing.md` point 10: no error, no effect).
///
/// The active compose is the new-thread compose when present, else the
/// selected thread's — the same precedence linux's `detail.rs` `render()`
/// uses for its `active_send_state` read. `Failed { reason }` shows the
/// reason — a `LocalizedText`, resolved here exactly as the page error is; any
/// other state (including no active compose at all) clears a stale error.
///
/// Two manager-owned truths feed one element, and they do not overlap: the
/// snapshot's `error` carries the **membership/label** wire ops
/// (`confirm_add_participant`, `remove_participant`, `rename_thread`), the
/// active compose's `send_state` carries **sends**. It reads the page error
/// first only because it is the more recent of the two by construction — every
/// producer clears the page error on entry, `send` included, so a stale
/// membership failure can never mask a fresh send failure.
pub fn sync_page_error(app: &mut App) {
    // The standing conversations-engine-role refusal outranks both manager
    // truths: with no engine there is no session to stamp them, and the honest
    // "served in another instance" must never be masked or cleared by an
    // unrelated success fold (`ConversationsState::served_elsewhere`).
    if app.conversations.served_elsewhere {
        app.errors.insert(
            crate::pages::Page::Conversations,
            crate::wizard::key("conversations.errors.served_elsewhere"),
        );
        return;
    }
    // The dead receive rail is the second standing truth: the loop died by
    // panic and is deliberately not re-armed
    // (`fauna_conversations::session::ReceiveLoopExit::Panicked`), so nothing
    // arrives until the user restarts the app — and, like the refusal above,
    // no gesture's outcome may mask or clear it
    // (`ConversationsManager::receive_stopped`).
    if app
        .conversations
        .manager
        .as_ref()
        .is_some_and(|manager| manager.receive_stopped())
    {
        app.errors.insert(
            crate::pages::Page::Conversations,
            crate::wizard::key("conversations.errors.receive_stopped"),
        );
        return;
    }
    let error = app.conversations.manager.as_ref().and_then(|manager| {
        let snap = manager.snapshot();
        if let Some(page_error) = &snap.error {
            return Some(crate::wizard::localized(page_error));
        }
        let active = snap
            .new_thread_compose
            .as_ref()
            .map(|c| c.send_state.clone())
            .or_else(|| {
                snap.selected_thread_id
                    .as_ref()
                    .and_then(|id| manager.thread_detail(id.clone()))
                    .map(|d| d.compose.send_state)
            });
        match active {
            // Both truths carry a `LocalizedText`, so both take the same
            // `wizard::localized` route — the send reason is a key + `{message}`,
            // never a pre-rendered English string.
            Some(SendState::Failed { reason }) => Some(crate::wizard::localized(&reason)),
            _ => None,
        }
    });
    // The lowest-ranked standing truth: received mail this run skipped because
    // it would not open under the account's key set
    // (`ConversationsManager::unopenable_mail_count`). Everything above it is
    // either more urgent or actionable, so it shows only when nothing else
    // does; like the two standing truths above, no gesture clears it — only
    // the records opening on a later re-drain does.
    let error = error.or_else(|| {
        let count = app
            .conversations
            .manager
            .as_ref()
            .map_or(0, |manager| manager.unopenable_mail_count());
        (count > 0).then(|| {
            crate::wizard::key("conversations.errors.mail_unopenable")
                .replace("{count}", &count.to_string())
        })
    });
    match error {
        Some(reason) => {
            app.errors.insert(crate::pages::Page::Conversations, reason);
        }
        None => {
            app.errors.remove(&crate::pages::Page::Conversations);
        }
    }
}

/// Fold an [`Outcome`] back into the page. The manager already stamped
/// `send_state` (Idle on success, `Failed { reason }` on error) before this
/// arrives, so the fold just resyncs from that truth via [`sync_page_error`]
/// — unified with the six other `Outcome`-shaped pages, each of which clears
/// on its own success variant (`settings::Outcome::FilterCreated`,
/// `events::Outcome::EventMutated`, …): every conversations mutation
/// collapses into this one `Done` variant, so success on ANY gesture clears a
/// stale error from an unrelated prior failure, matching `app.errors` being
/// page-scoped rather than gesture-scoped everywhere else.
pub fn apply_outcome(app: &mut App, outcome: Outcome) {
    match outcome {
        Outcome::Done => sync_page_error(app),
        // A send materializes its thread, moves the draft onto that thread's
        // compose and selects it BEFORE attempting delivery, so success and
        // refusal leave the manager in the same place (`conversations.md`
        // § Persistence, the `new_thread_compose` bullet). The page follows
        // it: the new-thread pane it was on is now an empty slot, and a
        // refused author's words and files wait on the thread, where the
        // failure also shows. Only when the page is still on that pane and the
        // slot really moved — a send that never started (no recipient) leaves
        // the draft where it is, and a user who navigated away mid-send is not
        // yanked back.
        Outcome::NewThreadSent => {
            if app.conversations.mode == Mode::Compose
                && let Some(snapshot) = app.conversations.snapshot()
                && snapshot.new_thread_compose.is_none()
                && let Some(thread_id) = snapshot.selected_thread_id
            {
                show_detail(&mut app.conversations, thread_id);
            }
            sync_page_error(app);
        }
        // The accepted room opens where the user is: the manager selected its
        // thread, so the page follows it into the detail view — the room reads
        // empty until a member with key authority keys this account in.
        Outcome::RoomJoined(joined) => {
            if let Some(thread_id) = joined {
                show_detail(&mut app.conversations, thread_id);
            }
            sync_page_error(app);
        }
        Outcome::RoomSettingsSaved(all_landed) => {
            if all_landed {
                app.conversations.room_settings = None;
                app.conversations.room_leave_confirm = false;
            }
            sync_page_error(app);
        }
        // The catalog is the Community labelers page's state too — one machine,
        // one snapshot — so it lands where that page reads it. Only an inspect
        // the user asked for reports its failure here (the error bridge the
        // catalog page's fold keeps, `settings::apply_labeler_catalog_snapshot`:
        // a machine gesture's failure rides the snapshot, never a return value).
        Outcome::RoomLabelerCatalog {
            snapshot,
            inspected,
        } => {
            let error = snapshot.error.clone().filter(|_| inspected);
            app.settings.labeler_catalog.snapshot = Some(*snapshot);
            match error {
                Some(err) => {
                    app.errors.insert(
                        crate::pages::Page::Conversations,
                        err.resolve(fauna_i18n::strings::lookup),
                    );
                }
                None => sync_page_error(app),
            }
        }
        // A pure cache write, and deliberately NOT a `sync_page_error` call: an
        // image that failed to load must not clear (or raise) the manager-owned
        // page error, exactly as feed's `Outcome::Images` leaves its banner alone.
        Outcome::RemoteImages(arts) => {
            for (url, art) in arts {
                app.conversations.remote_images.set(url, art);
            }
        }
        // The roster is app-wide state, not the page's — the contacts badge
        // reads the same field — so it lands on `App` and both surfaces drop
        // the person in the same frame. `None` leaves it untouched: a failed
        // Keep must not blank a mark that is still genuinely open.
        Outcome::MemberReviews(refreshed) => {
            if let Some(roster) = refreshed {
                app.member_reviews = roster;
            }
        }
    }
}

/// Fire a new-message banner for every thread the shared tracker says warrants
/// one — tui's leg of `conversations` outcome 11 (*a new message raises a system
/// notification while the app is running, except in the conversation you already
/// have open*). The linux twin is the observer loop in `fauna-linux/src/main.rs`.
///
/// **This function decides nothing.** The three when/for-whom rules — seed
/// silently on the first non-empty snapshot, fire on increased activity, suppress
/// the selected thread — are the whole decision and live once in
/// [`fauna_conversations::MessageNotificationTracker`] (`conversations.md`
/// § Where logic lives). Adding a fourth rule here would be exactly the
/// divergence that was removed from linux on 2026-09-20, where an app-glue
/// `!WINDOW_FOCUSED` gate silently falsified the outcome in its most ordinary
/// case. A terminal's own "is this window focused" instinct is the same trap:
/// the terminal emulator already decides whether to surface the escape, and that
/// is its business, not this app's.
///
/// Driven off the `ConversationsChanged` tick, the same edge every other
/// conversations projection rides, so a message reaching the manager by ANY
/// route — the real MLS receive loop, the mail rail, an e2e injection — raises a
/// banner without each route remembering to.
pub fn fire_message_banners(state: &ConversationsState) {
    // The diff-tick start barrier, bumped BEFORE the snapshot read so a negative
    // e2e assertion ("the open thread raises no banner") can prove by pigeonhole
    // that a tick which began after its plant has since finished
    // (`fauna_e2e_agent::MESSAGE_BANNERS_KEY`). A no-op in a release build.
    fauna_conversations::banner_pass_started();
    let Some(snapshot) = state.snapshot() else {
        // Pre-auth: no manager, so no snapshot and nothing to diff. The pass is
        // still *completed* — a barrier that stopped counting before login would
        // strand any reader waiting on it.
        fauna_conversations::banner_pass_completed();
        return;
    };
    let activities: Vec<_> = snapshot
        .threads
        .iter()
        .map(fauna_conversations::ThreadActivity::from_summary)
        .collect();
    for act in state.notif_tracker.diff(
        activities,
        snapshot.selected_thread_id.clone(),
        snapshot.launch_floor_ms,
    ) {
        crate::os_notify::notify_message(&act.label, &act.snippet);
        // Recorded at the FIRING site, immediately after the fire and after every
        // suppression above it, so the log means "the user was shown this" rather
        // than "the tracker returned this" — glue that swallowed a decision has to
        // fail the witness, not pass it.
        fauna_conversations::record_fired_banner(act.thread_id, act.label);
    }
    fauna_conversations::banner_pass_completed();
}

/// Kick a `doc-remote-image` byte fetch for every **revealed** remote image in
/// the open thread — the bubble's twin of `feed::kick_remote_image_fetches`.
///
/// Scoped to the open thread on purpose: a bubble only paints in Detail, so
/// fetching the rest of the mailbox's revealed images would be work no one can
/// see. Driven off the `ConversationsChanged` tick, which is what re-emits after
/// `reveal_remote_images` flips the message's blocks.
pub fn kick_remote_image_fetches(app: &mut App) -> Option<Op> {
    let Mode::Detail(thread_id) = &app.conversations.mode else {
        return None;
    };
    let detail = app
        .conversations
        .manager
        .as_ref()?
        .thread_detail(thread_id.clone())?;
    let urls: Vec<String> = detail
        .messages
        .iter()
        .flat_map(|msg| crate::document::revealed_remote_image_urls(&msg.document))
        .collect();
    let urls = crate::remote_image::kick(&mut app.conversations.remote_images, urls)?;
    Some(Op::FetchRemoteImages { urls })
}

/// Splice `prefix`…`suffix` markers into the active compose body using the shared
/// wrap rule. A terminal input has no selection, so this is the empty-selection
/// case: `wrap_selection` returns the wrapped "text" placeholder, inserted at the
/// cursor (= end of the body) — `world` + bold → `world**text**`, `` + bold →
/// `**text**` (the exact shape `test_compose_richeditbox_value_and_toolbar` pins).
fn wrap_compose_body(
    state: &ConversationsState,
    manager: &Arc<ConversationsManager>,
    prefix: &str,
    suffix: &str,
) {
    let body = compose_body(state);
    let wrap = fauna_core::markdown::wrap_selection("", prefix, suffix, "text");
    let new_body = format!("{body}{}", wrap.replacement);
    match &state.mode {
        Mode::Detail(thread_id) => manager.set_compose_body(thread_id.clone(), new_body),
        _ => manager.set_new_thread_body(new_body),
    }
}

/// The active compose body — the new-thread draft's, or the selected thread's.
fn compose_body(state: &ConversationsState) -> String {
    let Some(manager) = state.manager.as_ref() else {
        return String::new();
    };
    match &state.mode {
        Mode::Detail(thread_id) => manager
            .thread_detail(thread_id.clone())
            .map(|d| d.compose.body_draft)
            .unwrap_or_default(),
        _ => manager
            .snapshot()
            .new_thread_compose
            .map(|c| c.body_draft)
            .unwrap_or_default(),
    }
}

// ── Elements ────────────────────────────────────────────────────────────────

/// The `(actor_id_hex, newest_activity_ms)` key that feeds the backup audit
/// loop, or `None` when there is nothing to observe yet. Pure, so a test can
/// pin the exact resolution with no side effect to trip over.
///
/// Keyed by the SESSION's own account (`app.session.actor_id`) — the account
/// whose conversations are actually on screen — never a fresh
/// `crate::session::registry(app).active()` read, which can name a
/// *different* account on a bound (secondary) launch
/// (`account-scoping.md` § Concurrent instances → "every session-path
/// identity read resolves through the registry for that account", :818-822).
fn backup_audit_key(app: &App, snapshot: &ConversationsSnapshot) -> Option<(String, i64)> {
    let newest = snapshot.threads.iter().map(|t| t.last_activity_ms).max()?;
    let actor_id_hex = app.session.as_ref()?.actor_id.clone();
    Some((actor_id_hex, newest))
}

/// Feeds `elements`'s render loop's freshest-activity observation to the
/// backup audit loop. Makes the `backup_audit::observe_thread_activity` call
/// itself, immediately after computing [`backup_audit_key`] — the render
/// loop passes no key of its own , so there is no
/// call site left for a regression to revert to a bare `active()` read: the
/// only place the key is chosen is this function, and it acts on its own
/// choice in the same breath, with no other logic between the two.
///
/// Returns `observe_thread_activity`'s own return **directly** — never a
/// separately-held local — so a future call site forwarding a *different*
/// freshly-resolved actor there is reflected in what comes back, not only in
/// what got written .
/// `backup_audit_observation_keys_by_the_session_account_not_active` pins
/// this against `backup_audit_key`'s independent resolution; the render loop
/// itself (`elements`, below) still discards the return: `OBSERVED`
/// (`backup_audit.rs`) remains a process-wide static with no production
/// reader.
fn backup_audit_observation(app: &App, snapshot: &ConversationsSnapshot) -> Option<(String, i64)> {
    let (actor_id_hex, newest) = backup_audit_key(app, snapshot)?;
    crate::backup_audit::observe_thread_activity(&actor_id_hex, newest)
}

/// The conversations page's ordered element list.
///
/// **Every thread in the snapshot gets a row, however few fit the terminal** — the
/// list *is* the automation registry (`crate::element`), so clipping it to the
/// viewport would cap `count("conversation-item")` at terminal height. The
/// viewport clips paint only.
pub fn elements(app: &App) -> Vec<Element> {
    let mut out = vec![
        Element::label(ids::PAGE_HEADING, conversations::list::TITLE),
        Element::label(ids::CONVERSATIONS_VIEW, ""),
    ];
    let Some(snapshot) = app.conversations.snapshot() else {
        return out;
    };

    // Both sub-pages replace the list in the detail pane, exactly as the feed
    // page's `create_feed` / `post_detail` do (ui.yaml
    // `conversations.sub_pages.{compose,conversation_detail}`).
    match &app.conversations.mode {
        Mode::Compose => {
            out.extend(compose_elements(&snapshot));
            return out;
        }
        Mode::Detail(thread_id) => {
            out.extend(detail_elements(app, thread_id));
            return out;
        }
        Mode::List => {}
    }

    // List pane: search box + sort + new-conversation, then one row per thread.
    out.push(
        Element::input(
            ids::CONVERSATION_SEARCH_BOX,
            snapshot.search_query.clone().unwrap_or_default(),
            Field::Conversations(ConversationsField::Search),
        )
        .labelled(conversations::list::SEARCH_PLACEHOLDER),
    );
    out.push(Element::gesture_button(
        ids::CONVERSATION_SORT,
        conversations::list::SORT,
        true,
        Gesture::Conversations(Action::Sort),
    ));
    out.push(Element::gesture_button(
        ids::NEW_CONVERSATION_BUTTON,
        conversations::list::NEW_CONVERSATION,
        true,
        Gesture::Conversations(Action::StartNewConversation),
    ));
    out.extend(room_invitation_elements(&snapshot));

    // Feed the backup audit loop's freshness comparison: the newest activity
    // this client has actually *displayed* is its own, source-untrusted evidence
    // that data this recent exists, and the audit compares that against what each
    // backup destination is holding — never against a live read of the source,
    // which is the party being audited (`ui/backups.md` § Audit-alert surface:
    // "a shell that renders the elements but feeds no observation ships a
    // permanently-passing audit"). Monotonic, and a cached no-op on the repeat
    // renders this paint does constantly — `crate::backup_audit` owns both
    // contracts. linux feeds the identical value from its own list render.
    backup_audit_observation(app, &snapshot);

    // Empty state: real UI, but ui.yaml gives it no element ID — so it paints as
    // chrome and never registers (the feed page's discipline; minting a
    // `conversations-empty` id would be the invented-app-specific-ID
    // anti-pattern the rules forbid).
    if snapshot.threads.is_empty() {
        out.push(Element::chrome(conversations::list::NO_CONVERSATIONS));
    }
    for (i, thread) in snapshot.threads.iter().enumerate() {
        // The row's display label is the shared `thread_label_display` — a blank
        // label carries the canonical `(no subject)` placeholder rather than an
        // empty heading (priority #1: linux resolves it the same way). Its text is
        // what the harness's find-a-thread-by-its-subject helpers match on, so
        // clicking opens `conversation_detail` (the ui.yaml transition).
        let label =
            crate::wizard::localized(&fauna_core::format::thread_label_display(&thread.label));
        out.push(Element::gesture_button(
            ids::CONVERSATION_ITEM,
            label,
            true,
            Gesture::Conversations(Action::SelectThread(thread.thread_id.0.clone())),
        ));
        // `dm-subject` on the list row is the message *snippet* (the preview),
        // exactly as linux renders it (`views/conversations/list.rs`) — the ui.yaml
        // "subject display in list" label is a misnomer the reference resolves this
        // way. (`dm-sender` is a *bubble* element, not a list-row one.)
        out.push(
            Element::label(ids::DM_SUBJECT, thread.snippet.clone())
                .within(ids::CONVERSATION_ITEM, i),
        );
        // The glyph is the snapshot's — on a bridged room the one its bridge
        // declared, with the bridge's declared label beside it so two bridges
        // sharing a glyph still read apart (`conversations.md` § Where logic
        // lives → *The `Bridged` adapter*, ruling 2 (a)). No app-side mapping.
        let icon = match &thread.bridge {
            Some(bridge) => Element::label(
                ids::PROTOCOL_ICON,
                format!("{} {}", thread.glyph.emoji(), bridge.label),
            )
            .attr("bridge", bridge.id.clone()),
            None => Element::label(ids::PROTOCOL_ICON, thread.glyph.emoji()),
        };
        out.push(icon.within(ids::CONVERSATION_ITEM, i));
        // The family gate's marker, only while the nest reports one for this
        // room's peer (`family-safety.md` § The bridge-DM gate → *App
        // affordance*): computed there, painted here, and the row still opens.
        if let Some(state) = thread.guardian_state {
            out.push(
                Element::label(ids::CONVERSATION_GUARDIAN_STATE, state.label())
                    .attr("state", state.attr_token())
                    .within(ids::CONVERSATION_ITEM, i),
            );
        }
        // `conversation-item-timestamp`: when the thread last moved, through the
        // same shared buckets as a bubble's time — absent for a thread with no
        // activity time, as on linux.
        if thread.last_activity_ms > 0 {
            out.push(
                Element::label(
                    ids::CONVERSATION_ITEM_TIMESTAMP,
                    message_timestamp_text(thread.last_activity_ms),
                )
                .within(ids::CONVERSATION_ITEM, i),
            );
        }
        // `dm-unread-indicator` (the `conversation-list-item` component) paints
        // only when the thread has unread messages — the unread count, not a
        // permanent element.
        if thread.unread_count > 0 {
            out.push(
                Element::label(ids::DM_UNREAD_INDICATOR, thread.unread_count.to_string())
                    .within(ids::CONVERSATION_ITEM, i),
            );
        }
    }
    out
}

/// The community-room invitations standing for this account, atop the list
/// (`room-invitation[i]` with its accept/decline pair, flat and indexed alike,
/// the `room-admin-toggle` convention). The rows are already verified against
/// their signers in shared Rust, and the sentence is shared too
/// (`RoomInvitationSnapshot::text`), so this only lays them out. Absent while
/// none stands.
fn room_invitation_elements(snapshot: &ConversationsSnapshot) -> Vec<Element> {
    let mut out = Vec::new();
    for invitation in &snapshot.room_invitations {
        out.push(
            Element::label(ids::ROOM_INVITATION, invitation.text())
                .attr("role", invitation.role.attr_token()),
        );
        out.push(Element::gesture_button(
            ids::ROOM_INVITATION_ACCEPT_BUTTON,
            common::ACCEPT,
            true,
            Gesture::Conversations(Action::AcceptRoomInvitation { id: invitation.id }),
        ));
        out.push(Element::gesture_button(
            ids::ROOM_INVITATION_DECLINE_BUTTON,
            common::DECLINE,
            true,
            Gesture::Conversations(Action::DeclineRoomInvitation { id: invitation.id }),
        ));
    }
    out
}

/// The recipient picker (`recipient-picker` component) — `recipient-picker-input`,
/// the committed `recipient-picker-chip`s, the `recipient-picker-suggestion`s, and
/// the `recipient-resolve-status` line. Shared by the new-thread composer and the
/// add-participant overlay so the two can never drift on ids or the resolve
/// `state` attribute (priority #2, in-crate). `picker` is `None` before the
/// composer has a draft; the input always registers (so a bridge write has a
/// target), the rest only when a picker exists.
fn recipient_picker_elements(
    picker: Option<&RecipientPickerState>,
    bridges: &[fauna_conversations::snapshot::BridgeIdentitySnapshot],
) -> Vec<Element> {
    let mut out = vec![
        Element::input(
            ids::RECIPIENT_PICKER_INPUT,
            picker.map(|p| p.raw_input.clone()).unwrap_or_default(),
            Field::Conversations(ConversationsField::RecipientInput),
        )
        .labelled(conversations::unified::RECIPIENT_PICKER_PLACEHOLDER),
    ];
    // The bridges serving the account, each by the label it declared — which
    // far networks a typed address may reach. Only a list: the nest matches an
    // address to its bridge (`conversations.md` § Where logic lives → *The
    // `Bridged` adapter*, ruling 2 (d)). Chrome, like the empty-list line:
    // ui.yaml gives it no id.
    if !bridges.is_empty() {
        let labels: Vec<&str> = bridges.iter().map(|b| b.label.as_str()).collect();
        out.push(Element::chrome(
            conversations::unified::recipient_picker_bridges(&labels.join(", ")),
        ));
    }
    // A bridged address reads with its bridge's declared label beside it, so
    // the user sees which network the nest resolved it to.
    let display = |address: &TypedAddress| address.display_with_bridges(bridges);
    if let Some(p) = picker {
        for chip in &p.chips {
            out.push(Element::label(ids::RECIPIENT_PICKER_CHIP, display(chip)));
        }
        for sug in &p.suggestions {
            out.push(Element::gesture_button(
                ids::RECIPIENT_PICKER_SUGGESTION,
                display(sug),
                true,
                Gesture::Conversations(Action::AcceptRecipientChip),
            ));
        }
        // The status label carries its terminal-state name as a `state`
        // automation attribute (`get_attr("recipient-resolve-status", "state")`),
        // exactly as linux carries it on the AT-SPI Description
        // (`recipient_picker.rs:191`) — the value the recipient-picker suite polls
        // to know the async resolve reached a terminal state. Same five names on
        // every app (priority #1).
        out.push(
            Element::label(
                ids::RECIPIENT_RESOLVE_STATUS,
                resolve_status_text(p.resolve_state),
            )
            .attr("state", resolve_state_name(p.resolve_state)),
        );
    }
    out
}

/// ui.yaml `conversations.sub_pages.compose` — the in-pane new-thread composer
/// (`conversations.md` § Layout & flow: "New-thread compose lives in the detail
/// pane, not a modal").
///
/// The `recipient-picker-chip` / `-suggestion` repeats are **flat repeated ids**
/// addressed by index (`click("recipient-picker-suggestion", index=0)`), not a
/// positional scope — the same convention feed's `post-card` uses.
///
/// **Not yet in this slice** (tracked as buildout in `ui-actual-tui.yaml`):
/// `attachment-button` (a per-rail attachment sub-feature) and
/// `markdown-marker-toggle-button`. The async rail-resolve typing trigger and
/// the `recipient-resolve-status` `state` attribute landed with slice C (a
/// keystroke now drives `resolve_recipient` and the status carries its terminal
/// state, matching the GUI apps).
fn compose_elements(snapshot: &ConversationsSnapshot) -> Vec<Element> {
    let compose = snapshot.new_thread_compose.as_ref();
    let picker = compose.and_then(|c| c.recipient_picker.as_ref());

    // The recipient picker (input + chips + suggestions + resolve-status) is the
    // same widget the add-participant overlay reuses — factored to one place so
    // the two never drift on ids or the `state` attribute (priority #2, in-crate).
    let mut out = recipient_picker_elements(picker, &snapshot.bridges);
    // The group-conversation hint appears once a second compatible recipient is
    // committed (>= 2 chips) — the "this starts a group conversation" signal.
    if picker.map(|p| p.chips.len() >= 2).unwrap_or(false) {
        out.push(Element::label(
            ids::GROUP_CONVERSATION_HINT,
            conversations::unified::GROUP_CONVERSATION_HINT,
        ));
    }
    // The class of the room about to be created, stated once a chip is
    // committed (`conversation-rooms.md` § The three classes — the picker's
    // statement before the first message). Derived in shared Rust from the
    // committed chips and the home-nest choice; this only paints it.
    let class = picker
        .and_then(|p| fauna_conversations::prospective_room_class(&p.chips, p.include_home_nest));
    if let Some(class) = class {
        out.push(
            Element::label(ids::RECIPIENT_PICKER_CLASS, class.label())
                .attr("class", class.attr_token()),
        );
    }
    // The home-nest choice, painted with the composer whether or not a chip is
    // committed yet: it is a property of the room, chosen before its first
    // message, and the class statement above follows it.
    let include_home_nest = picker.is_some_and(|p| p.include_home_nest);
    out.push(
        Element::gesture_button(
            ids::RECIPIENT_PICKER_HOME_NEST_TOGGLE,
            if include_home_nest {
                conversations::unified::ROOM_HOME_NEST_YES
            } else {
                conversations::unified::ROOM_HOME_NEST_NO
            },
            true,
            Gesture::Conversations(Action::ToggleHomeNest),
        )
        .attr("checked", if include_home_nest { "true" } else { "false" }),
    );
    let founds_room = include_home_nest && class == Some(fauna_conversations::RoomClass::Community);
    // subject-input paints only when the topic is expanded (`subject_draft: Some`),
    // gated by topic-toggle-button — the same rule every app follows.
    if let Some(subject) = compose.and_then(|c| c.subject_draft.clone()) {
        out.push(
            Element::input(
                ids::SUBJECT_INPUT,
                subject,
                Field::Conversations(ConversationsField::Subject),
            )
            .labelled(conversations::unified::TOPIC_INPUT_PLACEHOLDER),
        );
    }
    // The compose bar: body + markdown toolbar + topic toggle + send + cancel.
    out.push(
        Element::input(
            ids::DM_TEXT_FIELD,
            compose.map(|c| c.body_draft.clone()).unwrap_or_default(),
            Field::Conversations(ConversationsField::ComposeBody),
        )
        .labelled(conversations::compose::WRITE_MESSAGE),
    );
    out.extend(markdown_toolbar());
    // `attachment-button` + one chip pair per staged attachment
    // (`add_new_thread_attachment` routes here — `Mode` is Compose).
    // No rail is resolved until a recipient is, so the new-thread composer paints
    // the control enabled (the same "enabled by default until caps resolve" rule
    // the markdown toolbar follows here).
    out.extend(attachment_elements(
        compose.map(|c| c.attachments.as_slice()).unwrap_or(&[]),
        true,
    ));
    out.push(Element::gesture_button(
        ids::TOPIC_TOGGLE_BUTTON,
        conversations::unified::TOPIC_TOGGLE_ADD,
        true,
        Gesture::Conversations(Action::ToggleTopic),
    ));
    out.extend(list_send_elements(
        compose.and_then(|c| c.list_send.as_ref()),
    ));
    let sending = matches!(compose.map(|c| &c.send_state), Some(SendState::Sending));
    // The rail the first committed chip resolves to — the offline gate's
    // discriminant, and nothing else (the manager re-derives it at send, and
    // also flushes a typed-but-uncommitted recipient this cannot see).
    let rail = picker
        .and_then(|p| p.chips.first())
        .and_then(TypedAddress::rail);
    out.push(Element::gesture_button(
        ids::DM_SEND_BUTTON,
        common::SEND,
        !sending,
        Gesture::Conversations(Action::SendNewThread { rail, founds_room }),
    ));
    out.push(Element::gesture_button(
        ids::NEW_CONVERSATION_CANCEL,
        common::CANCEL,
        true,
        Gesture::Conversations(Action::CancelNewConversation),
    ));
    out
}

// ── The conversation_detail sub-page ──────────────────────────────────────────

/// The post-succession review pair for one member chip, scoped inside it —
/// present only while that person carries an open review item
/// (`identity-succession.md` § Propagation → *MLS groups*).
///
/// **This is the load-bearing rendering of the flag**, for the reason the ruling
/// gives: a group member frequently is *not* a contact — the entry that matters
/// most, an identity a thief seated in a group, is precisely the one that never
/// was — so a contacts-only badge would hide exactly the population the surface
/// exists for. Removal already lives on this row, which is why the pair joins it
/// instead of minting a second eviction path.
///
/// ⚠ **Remove is deliberately NOT re-rendered.** The chip itself already *is*
/// it (a tap posts the remove Commit), exactly as `nest-trust-grant-revoke` is
/// the grant plane's Remove half. Two adjacent affordances is the ratified
/// shape and this row has them: Keep, and the chip.
///
/// ⚠ **Scoped `.within(ids::THREAD_MEMBER_CHIP, i)` rather than pushed flat.** The
/// pair renders only on flagged members, so a flat push would index the marks
/// `0..n` over a chip list indexed `0..m`: a driver reading
/// `thread-member-unattested-mark[0]` would get the first *flagged* member while
/// `thread-member-chip[0]` is the first member, and the two would silently
/// disagree about who is being asked about.
fn member_review_elements(
    app: &App,
    addr: &fauna_conversations::TypedAddress,
    i: usize,
) -> Vec<Element> {
    let Some(person) = addr.person_actor_id() else {
        return Vec::new();
    };
    if !fauna_core::data::is_under_review(&app.member_reviews, &person) {
        return Vec::new();
    }
    vec![
        Element::label(
            ids::THREAD_MEMBER_UNATTESTED_MARK,
            conversations::detail::MEMBER_UNATTESTED_MARK,
        )
        .within(ids::THREAD_MEMBER_CHIP, i),
        Element::gesture_button(
            ids::THREAD_MEMBER_KEEP_BUTTON,
            conversations::detail::MEMBER_KEEP,
            true,
            Gesture::Conversations(Action::KeepMember { person }),
        )
        .within(ids::THREAD_MEMBER_CHIP, i),
    ]
}

/// The chip's `role` attribute — a state of an existing element, ui.yaml's
/// ratified shape for one (`recipient-resolve-status`'s `state`), so a driver
/// reads the role off the chip it already addresses.
fn role_attr(role: fauna_conversations::RoomRole) -> &'static str {
    role.attr_token()
}

/// The room policy editor's staged values and the commits Save issues both
/// live in shared Rust (`fauna_conversations::room_settings`) — every app's
/// editor stages through the same draft and diffs it the same way, so the
/// seed, the at-most-one-staged hand-over rule and the edit order are decided
/// once for all seven (priority #2).
pub use fauna_conversations::{RoomSettingsDraft, RoomSettingsEdit};
use fauna_conversations::{history_policy_label, join_rule_label, member_chip_text};

/// ui.yaml `conversations.sub_pages.room_settings` — the policy editor,
/// replacing the read view like the rename overlay. Two token pickers
/// (localized labels ride `display_value`), one admin switch and one
/// hand-over control per participant (each greyed unless the viewer holds
/// the owner-only capability behind it, and never live on the owner's own
/// row), a community room's labeler set over the labeler catalog (with that
/// catalog's inspect view painted in place), Save, and the walk-out.
///
/// **Any member of a room opens this** (user-approved 2026-09-20) — the
/// walk-out is a plain member's verb, so gating the door on `can_set_policy`
/// hid the one control a member needs behind the one capability a member
/// never has. A member therefore reads this surface with every policy
/// control greyed and only `room-leave-button` live. Esc cancels
/// (`cancel_detail_overlay`).
fn room_settings_elements(
    detail: &fauna_conversations::ThreadDetail,
    draft: &RoomSettingsDraft,
    leave_confirm: bool,
    catalog: &crate::settings::labeler_catalog::LabelerCatalogState,
) -> Vec<Element> {
    use fauna_conversations::{HistoryPolicy, JoinRule};
    let mut out = vec![
        Element::chrome(conversations::unified::ROOM_JOIN_RULE_LABEL),
        Element::select(
            ids::ROOM_JOIN_RULE_SELECT,
            draft.join_rule.token(),
            SelectTarget::RoomJoinRule,
            JoinRule::EDITOR_CHOICES
                .iter()
                .map(|rule| rule.token().to_string())
                .collect(),
        )
        .display_value(join_rule_label(draft.join_rule))
        .enabled(detail.capabilities.can_set_policy),
        Element::chrome(conversations::unified::ROOM_HISTORY_POLICY_LABEL),
        Element::select(
            ids::ROOM_HISTORY_POLICY_SELECT,
            draft.history_policy.token(),
            SelectTarget::RoomHistoryPolicy,
            HistoryPolicy::EDITOR_CHOICES
                .iter()
                .map(|policy| policy.token().to_string())
                .collect(),
        )
        .display_value(history_policy_label(draft.history_policy))
        .enabled(detail.capabilities.can_set_policy),
    ];
    for (i, display) in detail.participant_displays.iter().enumerate() {
        // Resolved through the draft's identity column, never indexed into
        // its vectors: `i` is a live-list index and those vectors are seeded
        // to the list as it stood when the overlay opened. Indexing them here
        // put the checkmark on the wrong row **in the same frame** as the
        // mis-aimed gesture, so the owner could not see what they were about
        // to sign.
        let staged = draft.admin_at(i, &detail.participants);
        // Which rows either control may act on at all is the shared draft's
        // call (`RoomSettingsDraft::eligible`: a Fauna member who is not the
        // owner); each app adds only its own capability term.
        let eligible = draft.is_eligible(i, &detail.participants);
        let mark = if staged {
            conversations::unified::ROOM_ADMIN_YES
        } else {
            conversations::unified::ROOM_ADMIN_NO
        };
        out.push(
            Element::gesture_button(
                ids::ROOM_ADMIN_TOGGLE,
                format!("{display} · {mark}"),
                detail.capabilities.can_appoint_admins && eligible,
                Gesture::Conversations(Action::ToggleRoomAdmin { index: i }),
            )
            .attr("checked", if staged { "true" } else { "false" }),
        );
        // The hand-over control beside the same name: owner only, never on
        // the owner's own row, at most one row staged.
        let staged_owner = draft.transfer_staged_at(i, &detail.participants);
        let transfer_mark = if staged_owner {
            conversations::unified::ROOM_TRANSFER_STAGED
        } else {
            conversations::unified::ROOM_TRANSFER_MARK
        };
        out.push(
            Element::gesture_button(
                ids::ROOM_OWNER_TRANSFER_BUTTON,
                format!("{display} · {transfer_mark}"),
                detail.capabilities.can_transfer_ownership && eligible,
                Gesture::Conversations(Action::ToggleRoomOwnerTransfer { index: i }),
            )
            .attr("checked", if staged_owner { "true" } else { "false" }),
        );
    }
    // The home nest's read, only where there is one to stage: a community
    // room whose answer this device has read (`RoomSettingsDraft::nest_read`).
    if let Some(reads) = draft.nest_read {
        out.push(
            Element::gesture_button(
                ids::ROOM_NEST_READ_TOGGLE,
                if reads {
                    conversations::unified::ROOM_NEST_READ_YES
                } else {
                    conversations::unified::ROOM_NEST_READ_NO
                },
                detail.capabilities.can_set_policy,
                Gesture::Conversations(Action::ToggleRoomNestRead),
            )
            .attr("checked", if reads { "true" } else { "false" }),
        );
    }
    // What stands pending on the room, only once the home nest has served
    // this viewer a list and only while it is not empty: the section is
    // absent for `None` (not served — an end-to-end room, or a floor read
    // that has not landed) and for an empty answer alike. Every row is one
    // the nest served *because* this viewer may withdraw it, so the button is
    // live on every row and no capability is consulted here; the sentence is
    // shared (`RoomPendingInviteSnapshot::text`), and `lapsed` is the
    // driver's attribute (the `room-admin-toggle` `checked` shape).
    if let Some(pending) = detail
        .room
        .as_ref()
        .and_then(|room| room.pending_invites.as_deref())
        .filter(|pending| !pending.is_empty())
    {
        out.push(Element::chrome(
            conversations::unified::ROOM_PENDING_INVITES_LABEL,
        ));
        for invite in pending {
            out.push(
                Element::label(ids::ROOM_PENDING_INVITE, invite.text())
                    .attr("lapsed", if invite.lapsed { "true" } else { "false" }),
            );
            out.push(Element::gesture_button(
                ids::ROOM_PENDING_INVITE_WITHDRAW_BUTTON,
                conversations::unified::ROOM_PENDING_INVITE_WITHDRAW,
                true,
                Gesture::Conversations(Action::WithdrawRoomInvite {
                    invitee_actor_hex: invite.invitee_actor_hex.clone(),
                }),
            ));
        }
    }
    out.extend(room_labeler_elements(detail, draft, catalog));
    // Greyed for a member, who opens this surface to read it and to leave:
    // every staged edit behind it is owner-or-admin work.
    out.push(Element::gesture_button(
        ids::ROOM_SETTINGS_SAVE_BUTTON,
        common::SAVE,
        detail.capabilities.can_set_policy,
        Gesture::Conversations(Action::SaveRoomSettings),
    ));
    // The walk-out, below Save and outside the staged set: leaving is an
    // immediate act, not a policy edit. Greyed — never hidden — for the owner,
    // who hands the room over first (§ Architectural rules 5).
    out.push(Element::gesture_button(
        ids::ROOM_LEAVE_BUTTON,
        conversations::unified::ROOM_LEAVE,
        detail.capabilities.can_leave_room,
        Gesture::Conversations(Action::StartRoomLeave),
    ));
    if leave_confirm {
        out.push(Element::gesture_button(
            ids::ROOM_LEAVE_CONFIRM,
            common::LEAVE,
            detail.capabilities.can_leave_room,
            Gesture::Conversations(Action::ConfirmRoomLeave),
        ));
    }
    out
}

/// The room's labeler set — `room-labeler-toggle[i]` + `room-labeler-inspect-button[i]`
/// per catalog labeler a room may name (`ui/conversations.md` § Element IDs;
/// behavior owner `community-rooms.md` § The three classes, purpose 2).
///
/// **Every decision is shared Rust**: which kinds a room may name
/// (`room_may_name_labeler_kind`, the list the home nest admits by), the
/// `checked` mark (`labeler_staged`) and whether a row may act
/// (`labeler_toggle_live`: a full set greys only the rows it does not name).
/// This paints them and adds the one capability term.
///
/// Absent where the draft has no set to stage (an end-to-end room, or a set
/// that did not verify). The catalog's inspect view, when open, paints at the
/// section's foot with the Community labelers page's own builder — the same
/// view, not a second one — its close wired back to this editor.
fn room_labeler_elements(
    detail: &fauna_conversations::ThreadDetail,
    draft: &RoomSettingsDraft,
    catalog: &crate::settings::labeler_catalog::LabelerCatalogState,
) -> Vec<Element> {
    let mut out = Vec::new();
    if draft.labelers.is_none() {
        return out;
    }
    out.push(Element::chrome(conversations::unified::ROOM_LABELERS_LABEL));
    for (index, entry) in catalog.entries().iter().enumerate() {
        if !fauna_conversations::room_may_name_labeler_kind(entry.artifact_kind.clone()) {
            continue;
        }
        let staged = draft.labeler_staged(&entry.labeler_id);
        let mark = if staged {
            conversations::unified::ROOM_LABELER_ON
        } else {
            conversations::unified::ROOM_LABELER_OFF
        };
        // The kind and a prefix of the id: the catalog row carries no name,
        // and the full id sits one gesture away in the inspect view.
        let short_id: String = entry.labeler_id.chars().take(16).collect();
        out.push(
            Element::gesture_button(
                ids::ROOM_LABELER_TOGGLE,
                format!("{} · {short_id}… · {mark}", entry.artifact_kind),
                detail.capabilities.can_set_policy && draft.labeler_toggle_live(&entry.labeler_id),
                Gesture::Conversations(Action::ToggleRoomLabeler {
                    labeler: entry.labeler_id.clone(),
                }),
            )
            .attr("checked", if staged { "true" } else { "false" }),
        );
        // Inspecting is a read any member may make.
        out.push(Element::gesture_button(
            ids::ROOM_LABELER_INSPECT_BUTTON,
            fauna_i18n::strings::labeler_catalog::INSPECT,
            true,
            Gesture::Conversations(Action::InspectRoomLabeler {
                index: index as u32,
            }),
        ));
    }
    if let Some(view) = catalog.inspecting() {
        crate::settings::labeler_catalog::push_inspect_panel(
            &mut out,
            view,
            Gesture::Conversations(Action::CloseRoomLabelerInspect),
        );
    }
    out
}

/// ui.yaml `conversations.sub_pages.conversation_detail` — the opened thread's
/// detail view (`conversations.md` § Layout & flow). Mirrors feed's
/// `post_detail_elements`: reads the shared `ThreadDetail` and emits a header +
/// (capability-gated) membership/rename affordances + one bubble per message.
///
/// **The two modal overlays replace the read view while open**, exactly as the
/// GUI apps present them (linux `add_participant_overlay` / `rename_overlay`
/// are `adw::MessageDialog`s over the detail pane):
/// - the **add-participant** overlay is driven by manager state
///   (`snapshot.add_participant`) — no local field, so it opens/closes purely
///   through the observer tick, and its `recipient-picker-input` leaving the tree
///   on confirm is the driver's "membership applied" proxy;
/// - the **rename** overlay is driven by the local `rename_draft` (the manager
///   holds no rename-draft state).
///
/// **The reply compose bar** follows the messages: per-message `dm-reply-button`
/// (sender-only) + `dm-reply-all-button` (mail only — gated
/// `supports_recipient_selection`) seed the reply via `start_reply`; the editable
/// "To" line (`dm-reply-recipient-chip`/`-remove`/`-add`, also mail-only) renders
/// the seeded `compose.reply_recipients`; then the `dm-text-field` body +
/// `dm-send-button` (`manager.send`). Still deferred to slice F (they need real
/// crypto to be non-default): the encrypt/sign/verify message badges.
///
/// **The quote is extracted here, not walked.** `document.rs` leaves
/// `RenderBlock::QuotedMessage` inert (the walker's rule), so the page paints
/// `dm-message-quote` off the leading block the manager prepends
/// (`fold_reply_quotes`) — exactly as feed paints `quoted-post` off `quoted_post()`.
/// How many bubbles of the open thread the composed engine REGION-blocks — the
/// verdict side of the convention-17 "a region Block never renders silent"
/// invariant (`crate::region::block_render_json`). Walks the same messages the
/// paint reaches the verdict for (a deleted or legally-tombstoned message never
/// does), through the same `crate::region::verdict_for`, but never through the
/// paint's arms — which is what lets it catch an arm that drops the placeholder.
pub(crate) fn region_blocked_count(app: &App) -> usize {
    let Mode::Detail(thread_id) = &app.conversations.mode else {
        return 0;
    };
    let Some(detail) = app
        .conversations
        .manager
        .as_ref()
        .and_then(|m| m.thread_detail(thread_id.clone()))
    else {
        return 0;
    };
    detail
        .messages
        .iter()
        .filter(|msg| !msg.deleted && msg.legal_takedown_ref.is_none())
        .filter(|msg| {
            let composed =
                crate::region::verdict_for(app, &msg.message_id.0, &msg.labels, None, || {
                    crate::region::message_input(&msg.document.to_plaintext())
                });
            crate::region::region_verdict(&composed)
                .is_some_and(|p| p.verb == fauna_client_region::RegionVerb::Block)
        })
        .count()
}

fn detail_elements(app: &App, thread_id: &ThreadId) -> Vec<Element> {
    // `thread-header` must register even in the degenerate cases (pre-auth, or a
    // thread that vanished under a race) so the driver's `_wait_thread_open`
    // (which polls `is_visible("thread-header")`) resolves instead of burning its
    // timeout, and the page never blanks.
    let Some(manager) = app.conversations.manager.as_ref() else {
        return vec![Element::label(ids::THREAD_HEADER, "")];
    };
    let Some(detail) = manager.thread_detail(thread_id.clone()) else {
        return vec![Element::label(ids::THREAD_HEADER, "")];
    };

    let mut out = vec![
        Element::label(
            ids::THREAD_HEADER,
            crate::wizard::localized(&fauna_core::format::thread_label_display(&detail.label)),
        ),
        Element::label(ids::PROTOCOL_ICON, detail.glyph.emoji()),
    ];

    // ── Modal overlays replace the read view (add-participant wins if both set) ──
    let snapshot = manager.snapshot();
    if let Some(ap) = snapshot
        .add_participant
        .as_ref()
        .filter(|ap| &ap.target_thread_id == thread_id)
    {
        // Reuse the shared recipient picker, then the Add button. Cancel is Esc
        // (human-only, no ui.yaml id — linux's dialog Cancel isn't test-driven).
        out.extend(recipient_picker_elements(
            Some(&ap.picker),
            &snapshot.bridges,
        ));
        out.push(Element::gesture_button(
            ids::ADD_PARTICIPANT_CONFIRM,
            common::ADD,
            true,
            Gesture::Conversations(Action::ConfirmAddParticipant {
                // Read off the overlay state, never re-derived here: the
                // manager stamps the discriminant when it opens the overlay
                // (`AddParticipantState::in_place_mls_group`) so tui and the
                // six UniFFI apps gate on ONE expression instead of each
                // keeping a copy of the `(rail, flavor)` test (priority #2).
                in_place_mls_group: ap.in_place_mls_group,
            }),
        ));
        return out;
    }
    if let Some(draft) = app.conversations.rename_draft.clone() {
        out.push(
            Element::input(
                ids::THREAD_RENAME_FIELD,
                draft,
                Field::Conversations(ConversationsField::ThreadRename),
            )
            .labelled(conversations::unified::THREAD_RENAME_PLACEHOLDER),
        );
        out.push(Element::gesture_button(
            ids::THREAD_RENAME_CONFIRM,
            common::SAVE,
            true,
            Gesture::Conversations(Action::ConfirmRename),
        ));
        return out;
    }
    if let Some(draft) = app.conversations.room_settings.as_ref() {
        out.extend(room_settings_elements(
            &detail,
            draft,
            app.conversations.room_leave_confirm,
            &app.settings.labeler_catalog,
        ));
        return out;
    }
    if let Some(overlay) = app.conversations.actions_overlay.clone() {
        out.extend(actions_menu_elements(&detail, &overlay));
        return out;
    }

    // ── Read view: capability-gated header actions, then chips + bubbles ──
    // The gates are the shared `derive_capabilities` matrix, so tui shows exactly
    // what the other six apps show: add-participant iff the thread's membership
    // is mutable (FaunaMls forks a group on a 1:1; SMTP/ActivityPub add in place),
    // rename iff it is an MLS group (`conversations.md` § Element IDs).
    // The room's class on the header (`conversation-rooms.md` § The three
    // classes: "the class is on the thread header"), read off the projected
    // room, never computed here; the driver reads the `class` attribute
    // (`ui/conversations.md` § Element IDs). Beside it the editor's door,
    // live for **any member of a room** (user-approved 2026-09-20): the
    // surface carries the walk-out, which is a plain member's verb, so a door
    // gated on `can_set_policy` put the one control a member needs behind the
    // one capability a member never has. The greying moves INSIDE, per
    // control, which is what § Architectural rules 5 asks for anyway.
    if let Some(room) = detail.room.as_ref() {
        out.push(
            Element::label(ids::THREAD_ROOM_CLASS, room.class.label())
                .attr("class", room.class.attr_token()),
        );
        // The family gate's marker in the detail, the row's twin — the thread
        // below it stays fully readable (`family-safety.md` § The bridge-DM
        // gate).
        if let Some(state) = detail.guardian_state {
            out.push(
                Element::label(ids::CONVERSATION_GUARDIAN_STATE, state.label())
                    .attr("state", state.attr_token()),
            );
        }
        // The room's standing notice, only while one holds — which one speaks
        // (awaiting-key over unverified moderation) is the shared
        // `RoomSnapshot::notice`, never decided here (`ui/conversations.md`
        // § Element IDs → `thread-room-notice`). A room-level sentence: never
        // a mark on the message a parked record targets.
        if let Some(notice) = room.notice() {
            out.push(
                Element::label(ids::THREAD_ROOM_NOTICE, notice.label())
                    .attr("state", notice.attr_token()),
            );
        }
        out.push(Element::gesture_button(
            ids::THREAD_ROOM_SETTINGS_BUTTON,
            conversations::unified::THREAD_ROOM_SETTINGS,
            true,
            Gesture::Conversations(Action::OpenRoomSettings),
        ));
    }
    // The "+ add participant" affordance is painted whenever the rail's
    // membership is mutable and GREYED when the viewer's role in a governed
    // room may not invite (`capabilities.can_invite`, the roles table applied
    // in shared Rust — `conversations.md` § Architectural rules 5: never a
    // role branch here).
    if detail.capabilities.supports_membership_change {
        out.push(Element::gesture_button(
            ids::THREAD_ADD_PARTICIPANT_BUTTON,
            conversations::unified::THREAD_ADD_PARTICIPANT,
            detail.capabilities.can_invite,
            Gesture::Conversations(Action::OpenAddParticipant),
        ));
    }
    if detail.capabilities.supports_rename {
        out.push(Element::gesture_button(
            ids::THREAD_RENAME_BUTTON,
            conversations::unified::THREAD_RENAME,
            true,
            Gesture::Conversations(Action::OpenRename),
        ));
    }
    // Member chips: interactive — a tap removes that participant — iff the
    // rail supports membership change, linux's exact capability rule
    // (`thread_header.rs::render`: capability-gated, never rail-branched;
    // `conversations.md` § User actions). Mail chips stay informational
    // labels (`conversations.md` § Participants vs reply recipients). The
    // display/address zip is the linux shape: the gesture carries the row's
    // TypedAddress, never its index — the roster re-orders under a fresh
    // snapshot, and an index would then evict the wrong member.
    //
    // A governed room marks the owner and admins on their chips and carries
    // the role as the chip's `role` attribute (`conversation-rooms.md`
    // § Roles and authorization — "roles on member chips"); the tap is
    // greyed when the viewer may not remove (`capabilities.can_remove_members`).
    // `room.members` is index-parallel with `participants`, exactly as
    // `participant_displays` is.
    if detail.capabilities.supports_membership_change {
        for (i, (display, addr)) in detail
            .participant_displays
            .iter()
            .zip(detail.participants.iter())
            .enumerate()
        {
            let role = detail
                .room
                .as_ref()
                .and_then(|room| room.members.get(i))
                .and_then(|member| member.role);
            let mut chip = Element::gesture_button(
                ids::THREAD_MEMBER_CHIP,
                member_chip_text(display, role),
                detail.capabilities.can_remove_members,
                Gesture::Conversations(Action::RemoveMember { addr: addr.clone() }),
            );
            if let Some(role) = role {
                chip = chip.attr("role", role_attr(role));
            }
            out.push(chip);
            out.extend(member_review_elements(app, addr, i));
        }
    } else {
        for member in &detail.participant_displays {
            out.push(Element::label(ids::THREAD_MEMBER_CHIP, member.clone()));
        }
    }

    // One bubble per message, in thread order (flat indexed ids — the driver reads
    // `dm-message-text[i]` etc. by index, like feed's post-card children).
    for msg in &detail.messages {
        // The selected message — `SearchNav::Mail`'s second half (`ui/search.md`
        // § State & data shape). The manager resolved it against this thread's
        // fetched window, so this is a pure paint: `Some` here always names a
        // message in the list below.
        //
        // The MARK is `Element::chrome` — presentation, no id, the same call the
        // legal-takedown tombstone makes below. A GUI app tints the bubble's
        // background; a terminal has no fill to vary (the reaction pill's own
        // `reacted_by_me` marker settles this the same way), so the mark is a
        // line of its own above the message.
        //
        // The OBSERVABLE is a `selected` attribute on `dm-message-timestamp`,
        // not a new id — ui.yaml's ratified shape for a state of an existing
        // element (`recipient-resolve-status`: "State exposed via attribute,
        // not separate elements"). The timestamp carries it because it is the
        // one child painted in EVERY arm below — deleted, legal-takedown,
        // content-blocked, muted, content-collapsed and normal alike — so a hit
        // on a since-deleted or muted message still has somewhere to land.
        let selected = detail.selected_message_id.as_ref() == Some(&msg.message_id);
        if selected {
            out.push(
                Element::chrome(format!(
                    "\u{258c} {}",
                    conversations::detail::SELECTED_MESSAGE
                ))
                // In-process anchor for `search::focus_selected_message`, which
                // lands the focus ring — and therefore the viewport — on this
                // message. An attribute rather than a text match: the mark's
                // wording is localized and its glyph is cosmetic, so matching
                // either would make a translation silently stop the scroll.
                // Invisible to the agent (an id-less element never registers),
                // which is why the *observable* is the timestamp's attribute.
                .attr(SELECTED_MESSAGE_MARKER_ATTR, "true"),
            );
        }
        // `subject-divider` paints before a message that opens a new subject line
        // (the manager sets `subject_line: Some` only on an actual change, so no
        // extra dividers — `test_subject_divider`).
        if let Some(subject) = &msg.subject_line {
            out.push(Element::label(ids::SUBJECT_DIVIDER, subject.clone()));
        }
        // A deleted message renders the localized tombstone placeholder and its
        // timestamp ONLY — body/attachments/actions/reactions hidden (ui.yaml
        // `dm-message-deleted`; conversations.md § Reactions & message delete).
        if msg.deleted {
            out.push(Element::label(
                ids::DM_MESSAGE_DELETED,
                conversations::detail::MESSAGE_DELETED,
            ));
            out.push(message_timestamp_element(msg.timestamp_ms, selected));
            continue;
        }
        // Legal-takedown tombstone (`moderation.md` § Categories & enforcement
        // item 1): the nest withheld the sealed envelope under a legal
        // obligation, so the bubble collapses exactly like `deleted` — the
        // shared localized tombstone plus the timestamp, never a blank or
        // failed-decrypt bubble. Ordered AFTER `deleted` (linux's order) so a
        // tombstoned-then-deleted message stays a plain deletion. No dedicated
        // test id: it is presentation, like the quoted-post tombstone, and a new
        // e2e id would need ui.yaml approval first (§ UI Consistency A).
        if let Some(reference) = &msg.legal_takedown_ref {
            out.push(Element::chrome(
                fauna_i18n::strings::moderation::legal_takedown::tombstone(reference),
            ));
            out.push(message_timestamp_element(msg.timestamp_ms, selected));
            continue;
        }
        // Content-policy render enforcement (`family-safety.md` § Content
        // policy): the composed verdict over this message's **post-decrypt**
        // labels (the manager's `observe_local_detection` classifies inbound
        // bodies — the nest never sees an MLS-sealed body, so this is the only
        // place the floor can bind on a DM).
        // …with the region policies on the declared chain as the third source,
        // their scorers reading the post-decrypt body (`region-blocking.md`
        // § Render scope: private correspondence renders compose it too).
        // …and the viewer's own reports (`moderation.md` § Corollary): a report
        // names a message by its record digest (`crate::report::message_target`)
        // and hides everything an account the viewer reported authored.
        let sender = msg.sender.person_actor_id().map(|a| a.to_hex());
        let composed = crate::region::verdict_for(
            app,
            &msg.message_id.0,
            &msg.labels,
            Some(crate::region::ReportKey {
                item_id: msg
                    .plane_ref
                    .as_ref()
                    .map_or(msg.message_id.0.as_str(), |p| p.record_digest.as_str()),
                author_id: sender.as_deref(),
            }),
            || crate::region::message_input(&msg.document.to_plaintext()),
        );
        let content_verdict = composed.verdict;
        // Guardian Notify (§ Guardian Notify): count this message if the guardian
        // floor enforces on it — a no-op unless `content_notify` is on. Deduped
        // per message per local day; the one-minute tick reports the batch.
        app.content_policy
            .note_enforcement(&msg.message_id.0, &msg.labels);
        // A `block` floor is absolute — no reveal — so it is checked FIRST,
        // ahead of the muted-keyword collapse, so a message that is both muted
        // (revealable) and blocked can never be revealed past the guardian's
        // block. Painted like the `deleted` arm: placeholder + timestamp only, so
        // `count("dm-message-text")` excludes it. Mirrors the feed
        // (`crate::feed`) — both surfaces share `content_policy::verdict_for`.
        // A REGION verdict paints the region's own placeholder (the region, its
        // authority, the authority's reason verbatim) — same verb as the family
        // arm below, better attributed. A revealed region `collapse` falls
        // through to the ordinary arms.
        if let Some(withheld) = crate::region::region_verdict(&composed)
            && (withheld.verb == fauna_client_region::RegionVerb::Block
                || !app.conversations.revealed_content.contains(&msg.message_id))
        {
            out.extend(crate::region::placeholder(
                &withheld,
                Gesture::Conversations(Action::RevealContent {
                    msg_id: msg.message_id.clone(),
                }),
                None,
            ));
            out.push(message_timestamp_element(msg.timestamp_ms, selected));
            continue;
        }
        // The viewer's own report: the same placeholder slot, the reporter's
        // own words ("You reported this"), `source="reported"` beside it.
        if composed.reported() {
            out.push(
                Element::label(
                    ids::CONTENT_POLICY_BLOCKED_NOTICE,
                    fauna_i18n::strings::moderation::report::HIDDEN_PLACEHOLDER,
                )
                .attr("source", "reported"),
            );
            out.push(message_timestamp_element(msg.timestamp_ms, selected));
            continue;
        }
        if content_verdict == RenderVerdict::Block {
            out.push(Element::label(
                ids::CONTENT_POLICY_BLOCKED_NOTICE,
                family::CONTENT_BLOCKED_NOTICE,
            ));
            out.push(message_timestamp_element(msg.timestamp_ms, selected));
            continue;
        }
        // The muted-keyword collapse (`content-moderation-and-ranking.md` § Q3;
        // `moderation.md` § Muted keywords). Client-side, **post-decrypt**, over
        // the user's sealed list — the nest holds neither the plaintext nor the
        // list, so this is the only place the match can run. Computed at RENDER
        // (never stored on the message and never routed through the
        // `LocalDetectionStore`): a mute is a hide verb, not a queue flag.
        //
        // Painted like the `deleted` arm above — placeholder + timestamp only, no
        // body, no actions — which is exactly what makes `count("dm-message-text")`
        // exclude a collapsed bubble, the contract `test_muted_words.py` asserts.
        // Ordered AFTER `deleted` so a deleted message stays a tombstone rather
        // than advertising that it matched a muted word (linux's order).
        if !app.conversations.revealed_muted.contains(&msg.message_id)
            && fauna_core::scoring::muted_keywords_collapse(
                app.settings.muted_words.keywords(),
                &msg.document.to_plaintext(),
            )
        {
            out.push(Element::label(
                ids::DM_MESSAGE_MUTED,
                conversations::detail::MUTED_WORD,
            ));
            out.push(Element::gesture_button(
                ids::DM_MESSAGE_MUTED_REVEAL_BUTTON,
                conversations::detail::MUTED_REVEAL,
                true,
                Gesture::Conversations(Action::RevealMuted {
                    msg_id: msg.message_id.clone(),
                }),
            ));
            out.push(message_timestamp_element(msg.timestamp_ms, selected));
            continue;
        }
        // Content-policy `collapse` floor: a one-tap, session-local reveal — the
        // same shape as the muted collapse above but its own reveal set. `Block`
        // already returned, so only `Collapse` reaches here; the floor itself
        // persists (the guardian relaxing it is what stops future collapse).
        // Untagged, like linux's and web's twins — ui.yaml scopes no id to this
        // arm, so the reveal is a `chrome_button`: reachable by the focus ring,
        // invisible to the registry.
        if content_verdict == RenderVerdict::Collapse
            && !app.conversations.revealed_content.contains(&msg.message_id)
        {
            out.push(Element::chrome(family::CONTENT_COLLAPSED_NOTICE));
            out.push(Element::chrome_button(
                family::CONTENT_REVEAL_BUTTON,
                Gesture::Conversations(Action::RevealContent {
                    msg_id: msg.message_id.clone(),
                }),
            ));
            out.push(message_timestamp_element(msg.timestamp_ms, selected));
            continue;
        }
        // `dm-sender` (flat indexed, one per bubble) — `sender_display` is empty
        // until contact-name resolution lands (conversations.md § Where logic
        // lives), so fall back to the shared `TypedAddress::display`, same as
        // linux/apple/web/windows.
        let sender_text = if msg.sender_display.is_empty() {
            msg.sender.display()
        } else {
            msg.sender_display.clone()
        };
        out.push(Element::label(ids::DM_SENDER, sender_text));
        // `dm-message-quote` paints above the body when the message replies to a
        // parent loaded in the same thread — the leading `QuotedMessage` block the
        // manager prepends. Absent (no block) when the parent isn't loaded, so a
        // reply to an un-ingested parent shows no quote (`test_conversations_reply_quote`).
        if let Some(snippet) = leading_quote_snippet(&msg.document) {
            out.push(Element::label(ids::DM_MESSAGE_QUOTE, snippet.to_string()));
        }
        // Crypto badges (flat indexed like the other bubble children), gated
        // per-flag exactly as linux's top row paints them. All-false on every
        // mock-injected message (`MessageBadges::default()`), so these surface
        // only on the real-session path — real crypto is what sets them
        // (`conversations.md` § Element IDs). A terminal has no tooltip, so the
        // glyph carries the localized text linux puts in its tooltip.
        if msg.badges.encrypted {
            out.push(Element::label(
                ids::ENCRYPTED_BADGE,
                format!("\u{1f512} {}", conversations::detail::BADGE_ENCRYPTED),
            ));
        }
        if msg.badges.signed {
            out.push(Element::label(
                ids::SIGNED_BADGE,
                format!("\u{270d} {}", conversations::detail::BADGE_SIGNED),
            ));
        }
        if msg.badges.verified {
            out.push(Element::label(
                ids::VERIFIED_BADGE,
                format!("\u{2713} {}", conversations::detail::BADGE_VERIFIED),
            ));
        }
        // `content-label-badge` — the highest-confidence classifier verdict on
        // this message, via the SAME shared pair the moderation queue and the
        // feed post-card use, so a message carries an identical badge wherever
        // it renders (`moderation.md` § Per-row badge data path). The data path
        // is `MessageSnapshot.labels`, populated by the manager's
        // `observe_local_detection` at post-decrypt — NOT `msg.badges
        // .content_warning`, a different wire field nothing in
        // `libs/fauna-conversations` ever sets (the dead-field bug linux,
        // android and apple each had to repoint).
        if let Some(badge) = crate::moderation::content_label_badge(&msg.labels) {
            out.push(badge);
        }
        // The body walks the shared `RenderDocument` (`Element::document` pins the
        // registry `text` to its plaintext, the read every app's body answers with).
        //
        // This is also where the T1 observation hangs (`crate::observation`).
        // It rides the BODY element and no other, so every arm above — deleted,
        // legally withheld, muted, content-collapsed, content-blocked — excludes
        // itself by having returned before reaching this line, rather than by a
        // second list of conditions that could drift from these. Carrying it is
        // not yet reporting it: the shell reports only once the element proves
        // it landed inside the viewport.
        let mut body = Element::document(ids::DM_MESSAGE_TEXT, msg.document.clone());
        if let Some(record) = msg.plane_ref.clone() {
            body = body.observes(record);
        }
        out.push(body);
        // A body `![](https://…)` — the walker leaves `RenderBlock::RemoteImage`
        // inert like every other embed, so the page registers each one as
        // `doc-remote-image`: the blocked placeholder, and the real half-block
        // picture once the reader's `load-remote-content-button` below let it
        // load (`crate::document`; render-model.md § D3).
        out.extend(crate::document::remote_image_elements(
            &msg.document,
            &app.conversations.remote_images,
        ));
        // `[body] → [attachments]`, the order all seven apps render (render-model.md
        // § D2: an attachment is a *block* in the document, not a sibling snapshot
        // field). The walker leaves `RenderBlock::Attachment` inert, so the page
        // reads it here — feed's rule for `QuotedPost`/`Attachment`.
        out.extend(attachment_bubble_elements(&msg.document, manager));
        // `[body] → [attachments] → [link previews] → [reveal]`, the order tui's
        // feed post-card already builds (`feed/mod.rs`) — the walker leaves
        // `RenderBlock::LinkPreview` inert, so the page extracts it, feed's rule
        // for every embed. Both blocks below read the message's *folded*
        // document: `thread_detail` projects the manager-owned preview cache and
        // reveal set onto it before the page ever sees it, so there is no
        // per-app preview map and no per-app reveal dictionary
        // (`render-model.md` § D3 / § D4).
        out.extend(link_preview_bubble_elements(&msg.document));
        if msg.document.has_blocked_remote_images() {
            out.push(Element::gesture_button(
                ids::LOAD_REMOTE_CONTENT_BUTTON,
                conversations::detail::LOAD_REMOTE_CONTENT,
                true,
                Gesture::Conversations(Action::RevealRemoteImages {
                    msg_id: msg.message_id.clone(),
                }),
            ));
        }
        out.push(message_timestamp_element(msg.timestamp_ms, selected));
        // Per-message reply seeds (flat indexed ids, one per message in order —
        // the driver reads `dm-reply-button[i]` like the other bubble children).
        // `dm-reply-button` (sender-only) is unconditional; `dm-reply-all-button`
        // is mail-only (on FaunaMls the recipients ARE the group membership, so
        // there is nothing per-reply to pick — `supports_recipient_selection`).
        out.push(Element::gesture_button(
            ids::DM_REPLY_BUTTON,
            common::REPLY,
            true,
            Gesture::Conversations(Action::ReplyToMessage {
                msg_id: msg.message_id.clone(),
                reply_all: false,
            }),
        ));
        if detail.capabilities.supports_recipient_selection {
            out.push(Element::gesture_button(
                ids::DM_REPLY_ALL_BUTTON,
                conversations::unified::REPLY_ALL,
                true,
                Gesture::Conversations(Action::ReplyToMessage {
                    msg_id: msg.message_id.clone(),
                    reply_all: true,
                }),
            ));
        }
        // The per-bubble ⋯ overflow — shown iff ≥1 action is available: react OR
        // deletable (`MessageSnapshot::can_delete` — own, or the viewer governs
        // the room; the snapshot's answer, never a role branch here) OR
        // mark-as-spam. Mark-as-spam is the third, deliberately
        // rail-**independent** action (`conversations.md` § Reactions & message
        // delete → behavior owner `mail-spam.md` § Training signal sources 1):
        // offered on any *received* (`!is_own`) message, so a received bubble
        // carries the ⋯ even on a rail with no reactions/delete (android/windows
        // parity — `canFlagSpam = !is_own`). The train itself degrades to a silent
        // no-op when mail isn't enabled; the affordance is unconditional.
        if detail.capabilities.supports_reactions || msg.can_delete || !msg.is_own {
            out.push(Element::gesture_button(
                ids::DM_MESSAGE_ACTIONS_BUTTON,
                "\u{2026}",
                true,
                Gesture::Conversations(Action::OpenMessageActions {
                    msg_id: msg.message_id.clone(),
                }),
            ));
        }
        // Aggregated reactions under the bubble — one tap-to-toggle pill per
        // emoji (`dm-reaction-pill`; the manager folds the reaction log onto
        // `msg.reactions`). The own-reaction highlight is a trailing marker —
        // a terminal has no fill color to vary.
        for group in &msg.reactions {
            let mine = if group.reacted_by_me { " \u{2713}" } else { "" };
            out.push(Element::gesture_button(
                ids::DM_REACTION_PILL,
                format!("{} {}{mine}", group.emoji, group.count),
                true,
                Gesture::Conversations(Action::ToggleReaction {
                    msg_id: msg.message_id.clone(),
                    emoji: group.emoji.clone(),
                }),
            ));
        }
    }

    // The reply compose bar (`conversations.md` § Participants vs reply recipients).
    out.extend(reply_compose_bar(
        &detail.compose,
        &detail.capabilities,
        manager.reply_preview(thread_id.clone()),
        thread_id,
        detail.rail,
    ));
    out
}

/// The open `dm-message-actions-menu` — the tui face of the GUI apps' ⋯
/// popover, painted as a modal overlay exactly like the rename/add-participant
/// overlays (Esc cancels; no ui.yaml dismiss id). Step 1 is the quick-set row
/// (`dm-reaction-option`, the shared fixed order) + `dm-reaction-more-button` +
/// `dm-message-delete-button` (on a message the snapshot says this viewer may
/// delete); the delete two-step reveals
/// `dm-message-delete-confirm-button`; "more" flips to a free-entry emoji
/// prompt (a terminal's picker is typing the emoji — Enter commits, the same
/// special-cased submit as the reply To-line input). Gates mirror linux:
/// `supports_reactions` / `MessageSnapshot::can_delete`; the
/// received-only `dm-message-mark-as-spam-button` (`!is_own`) rounds out the
/// three ⋯ actions.
fn actions_menu_elements(
    detail: &fauna_conversations::ThreadDetail,
    overlay: &ActionsOverlay,
) -> Vec<Element> {
    // The menu container id doubles as the step's heading — the GUI popover has
    // no visible title, but a terminal overlay needs a text line, and the
    // destructive step must say what it is about to do.
    let heading = match overlay.step {
        ActionsStep::ConfirmDelete => conversations::detail::DELETE_MESSAGE_CONFIRM_TITLE,
        _ => conversations::detail::ADD_REACTION,
    };
    let mut out = vec![Element::label(ids::DM_MESSAGE_ACTIONS_MENU, heading)];
    let target = detail
        .messages
        .iter()
        .find(|m| m.message_id == overlay.msg_id);
    let is_own = target.is_some_and(|m| m.is_own);
    let can_delete = target.is_some_and(|m| m.can_delete);
    match overlay.step {
        ActionsStep::Menu => {
            if detail.capabilities.supports_reactions {
                for emoji in fauna_conversations::QUICKSET_EMOJIS {
                    out.push(Element::gesture_button(
                        ids::DM_REACTION_OPTION,
                        emoji,
                        true,
                        Gesture::Conversations(Action::ToggleReaction {
                            msg_id: overlay.msg_id.clone(),
                            emoji: emoji.to_string(),
                        }),
                    ));
                }
                out.push(Element::gesture_button(
                    ids::DM_REACTION_MORE_BUTTON,
                    conversations::detail::MORE_REACTIONS,
                    true,
                    Gesture::Conversations(Action::StartReactionEntry),
                ));
            }
            if can_delete {
                out.push(Element::gesture_button(
                    ids::DM_MESSAGE_DELETE_BUTTON,
                    conversations::detail::DELETE_MESSAGE,
                    true,
                    Gesture::Conversations(Action::StartDeleteMessage),
                ));
            }
            // Mark-as-spam — the third ⋯ action, rail-independent, offered on any
            // received (`!is_own`) message (`mail-spam.md` § Training signal
            // sources 1; ui.yaml `dm-message-mark-as-spam-button`). One flyout hop,
            // no confirm step (unlike delete): the click fires the sealed
            // client-side train and closes the menu.
            if !is_own {
                out.push(Element::gesture_button(
                    ids::DM_MESSAGE_MARK_AS_SPAM_BUTTON,
                    conversations::detail::MARK_AS_SPAM,
                    true,
                    Gesture::Conversations(Action::MarkMessageSpam {
                        msg_id: overlay.msg_id.clone(),
                    }),
                ));
                // Report — beside mark-as-spam, received messages only
                // (`moderation.md` § User-initiated reporting → *App surface*).
                // Only a message with a plane identity has anything on the nest
                // to report it against, so a mail or bridged message paints no
                // report verb rather than a dead one.
                if let Some(target) = target.and_then(crate::report::message_target) {
                    out.push(Element::gesture_button(
                        ids::DM_MESSAGE_REPORT_BUTTON,
                        conversations::detail::REPORT_MESSAGE,
                        true,
                        Gesture::Report(crate::report::Action::Open(Box::new(target))),
                    ));
                }
            }
        }
        ActionsStep::ConfirmDelete => {
            out.push(Element::gesture_button(
                ids::DM_MESSAGE_DELETE_CONFIRM_BUTTON,
                conversations::detail::DELETE_MESSAGE_CONFIRM,
                true,
                Gesture::Conversations(Action::ConfirmDeleteMessage),
            ));
        }
        ActionsStep::EmojiEntry => {
            // A **committing** input, not a plain one: the prompt's only commit
            // affordance is Enter, and a plain `Role::Input` leaves that
            // reachable from the keyboard alone — the automation surface's
            // `press_key` is a no-op, so nothing could ever witness the fuller
            // picker on this app (`Element::input_commit`'s own doc: "Without
            // this variant a tui input can only be committed by some OTHER
            // element"). `InputCommit` keeps Enter working through the generic
            // `actuate_focused` arm and adds the type-then-click idiom the
            // shared action layer drives a commit-on-Enter entry with.
            out.push(
                Element::input_commit(
                    ids::DM_REACTION_MORE_BUTTON,
                    overlay.emoji_draft.clone(),
                    Field::Conversations(ConversationsField::ReactionEmoji),
                    Gesture::Conversations(Action::CommitReactionEntry),
                )
                .labelled(conversations::detail::MORE_REACTIONS),
            );
        }
    }
    out
}

/// The reply compose bar shown under the message list — the editable "To" line
/// (mail only) + the body field + send. Mirrors linux's `compose_bar` render off
/// the same shared `ComposeState`.
fn reply_compose_bar(
    compose: &fauna_conversations::ComposeState,
    caps: &fauna_conversations::ThreadCapabilities,
    reply_preview: Option<fauna_conversations::ReplyPreview>,
    _thread_id: &ThreadId,
    // The open thread's rail — carried onto `dm-send-button` for the offline
    // gate; the manager routes by the live `ThreadDetail::rail` regardless.
    rail: Rail,
) -> Vec<Element> {
    let mut out = Vec::new();
    // `dm-reply-preview` / `dm-reply-cancel` — shown only while a reply target
    // is armed (`compose.reply_to`, seeded by `dm-reply-button`/
    // `dm-reply-all-button`). What the preview says is the shared
    // `ConversationsManager::reply_preview` — the answered message's sender and
    // plain-text excerpt — so no app derives its own; an answered message
    // outside the fetched window previews empty rather than stale or wrong.
    if compose.reply_to.is_some() {
        let preview = reply_preview
            .map(|p| format!("{}: {}", p.sender_display, p.excerpt))
            .unwrap_or_default();
        out.push(Element::label(ids::DM_REPLY_PREVIEW, preview));
        out.push(Element::gesture_button(
            ids::DM_REPLY_CANCEL,
            common::CANCEL,
            true,
            Gesture::Conversations(Action::CancelReply),
        ));
    }
    // The editable reply-recipient "To" line is mail-only: each reply picks its
    // To/Cc (`supports_recipient_selection`). On FaunaMls the recipients are the
    // group membership, so the whole line (chips + remove + add) is hidden.
    if caps.supports_recipient_selection {
        for addr in &compose.reply_recipients {
            out.push(Element::label(ids::DM_REPLY_RECIPIENT_CHIP, addr.display()));
            out.push(Element::gesture_button(
                ids::DM_REPLY_RECIPIENT_REMOVE,
                common::REMOVE,
                true,
                Gesture::Conversations(Action::RemoveReplyRecipient { addr: addr.clone() }),
            ));
        }
        // The "type a new recipient" input; Enter commits it (`app.rs` keymap →
        // `CommitReplyRecipientAdd`). Reads the local buffer, not the manager.
        out.push(
            Element::input(
                ids::DM_REPLY_RECIPIENT_ADD,
                String::new(),
                Field::Conversations(ConversationsField::ReplyRecipientAdd),
            )
            .labelled(conversations::unified::REPLY_RECIPIENT_ADD_PLACEHOLDER),
        );
    }
    // The body + send. `dm-text-field` is already `Mode::Detail`-routed
    // (`set_field` → `set_compose_body(thread_id, …)`), and `dm-send-button` runs
    // the per-thread `manager.send` (a distinct gesture from the composer's).
    out.push(
        Element::input(
            ids::DM_TEXT_FIELD,
            compose.body_draft.clone(),
            Field::Conversations(ConversationsField::ComposeBody),
        )
        .labelled(conversations::compose::WRITE_MESSAGE),
    );
    // The reply bar carries the same attach control + chip row as the new-thread
    // composer (ui.yaml declares the ids on both); `Mode::Detail` routes the commit
    // to `add_attachment`/`remove_attachment` instead. Here the thread's caps ARE
    // resolved, so the control gates on them.
    out.extend(attachment_elements(
        &compose.attachments,
        caps.supports_attachments,
    ));
    // `topic-toggle-button` (+ the `subject-input` it reveals) is declared on
    // `dm-compose-bar` too, not just the new-thread form — ui.yaml
    // `conversations.sub_pages.conversation_detail`. tui had never painted it here,
    // which made `test_capability_gating.py`'s Detail-mode assertions pass
    // **vacuously** (`get_attr` on an absent id answers `None`, and `None` satisfies
    // `in ("false", None)`). Gated on `supports_subject` the way the attach control
    // gates on `supports_attachments`.
    out.push(
        Element::gesture_button(
            ids::TOPIC_TOGGLE_BUTTON,
            conversations::unified::TOPIC_TOGGLE_ADD,
            caps.supports_subject,
            Gesture::Conversations(Action::ToggleTopic),
        )
        .enabled(caps.supports_subject),
    );
    if let Some(subject) = compose.subject_draft.clone() {
        out.push(
            Element::input(
                ids::SUBJECT_INPUT,
                subject,
                Field::Conversations(ConversationsField::Subject),
            )
            .labelled(conversations::unified::TOPIC_INPUT_PLACEHOLDER),
        );
    }
    out.extend(list_send_elements(compose.list_send.as_ref()));
    let sending = matches!(compose.send_state, SendState::Sending);
    out.push(Element::gesture_button(
        ids::DM_SEND_BUTTON,
        common::SEND,
        !sending,
        Gesture::Conversations(Action::SendThread { rail }),
    ));
    out
}

/// The compose form's list-send elements, present only while the compose's one
/// mail recipient is one of the account's own mailing lists
/// (`mail-mass-mailing.md` § Composing a list message; ui.yaml `dm-compose-form`
/// `optional_elements`). Shared Rust derives every text
/// (`fauna_conversations::list_send`); this only paints them, beside Send.
fn list_send_elements(view: Option<&fauna_conversations::ListSendView>) -> Vec<Element> {
    let Some(view) = view else {
        return Vec::new();
    };
    let mut out = vec![Element::label(
        ids::DM_COMPOSE_LIST_SEND_WARNING,
        crate::wizard::localized(&view.send_warning),
    )];
    if let Some(warning) = &view.quota_warning {
        out.push(Element::label(
            ids::DM_COMPOSE_LIST_QUOTA_WARNING,
            crate::wizard::localized(warning),
        ));
    }
    if let Some(progress) = &view.progress {
        out.push(Element::label(
            ids::DM_COMPOSE_LIST_SEND_PROGRESS,
            crate::wizard::localized(progress),
        ));
    }
    out
}

/// The leading reply-quote snippet of a message document, if any — the
/// `RenderBlock::QuotedMessage` the manager prepends as `blocks[0]`. The tui
/// walker leaves that block inert, so the page reads it here (feed's rule for
/// `QuotedPost`/`Attachment`). Borrows the snippet; the caller clones on paint.
fn leading_quote_snippet(doc: &RenderDocument) -> Option<&str> {
    match doc.blocks.first() {
        Some(RenderBlock::QuotedMessage { snippet, .. }) => Some(snippet.as_str()),
        _ => None,
    }
}

/// A message timestamp's display text, via the **shared** contextual bucketer
/// (`fauna_core::format::conversation_timestamp_display`, value-formatting.md
/// § Conversation timestamp): today → a local 24h `HH:MM`, else a localized
/// relative label or a local short date. Only the local-offset SOURCE is
/// per-app (`chrono::Local` here, `glib::DateTime::now_local()` on linux) — the
/// buckets are shared, so tui shows the same value every app does.
fn message_timestamp_text(then_ms: i64) -> String {
    let display = fauna_core::format::conversation_timestamp_display(
        now_ms(),
        then_ms,
        crate::settings::logs::local_offset_secs(),
    );
    if let Some(clock) = display.clock {
        clock
    } else if let Some(localized) = display.localized {
        crate::wizard::localized(&localized)
    } else if let Some(epoch_ms) = display.absolute_epoch_ms {
        local_short_date(epoch_ms)
    } else {
        String::new()
    }
}

/// Attribute key marking the `Element::chrome` line painted above the selected
/// message. Shared by the paint (`thread_detail_elements`) and the focus landing
/// (`crate::search::focus_selected_message`) so the two cannot drift apart; it
/// never reaches a driver, because an id-less element does not register.
pub(crate) const SELECTED_MESSAGE_MARKER_ATTR: &str = "selected-message-marker";

/// A message's `dm-message-timestamp`, carrying the `selected` automation
/// attribute — the observable for `SearchNav::Mail`'s second half (`ui/search.md`
/// § State & data shape).
///
/// Every arm of the bubble loop builds its timestamp here, which is the point:
/// the timestamp is the one child painted for a deleted, legal-takedown,
/// content-blocked, muted, content-collapsed AND normal message alike, so
/// routing all six through one constructor is what stops a future arm from
/// silently shipping an unmarkable message. The attribute is always present
/// (`"true"`/`"false"`) rather than set only when selected: a test that reads it
/// can then distinguish "this message is not selected" from "this app never
/// surfaced the attribute", which an absent-when-false attribute cannot
/// (`testing.md` point 6 — failures must diagnose themselves).
fn message_timestamp_element(then_ms: i64, selected: bool) -> Element {
    Element::label(ids::DM_MESSAGE_TIMESTAMP, message_timestamp_text(then_ms))
        .attr("selected", if selected { "true" } else { "false" })
}

/// A local `YYYY-MM-DD` short date for a message older than a week (the
/// `absolute_epoch_ms` bucket), via the shared
/// [`fauna_core::format::format_unix_local_date_ms`]. Locale-neutral for now;
/// an i18n date pass can refine it without touching the shared bucketer.
///
/// Named `local_short_date` after linux's twin, but note the two are NOT the
/// same render and deliberately so: linux's `i18n::local_short_date` reaches for
/// glib's locale-aware `%b %-d`, which a terminal has no equivalent for, so tui
/// resolves to linux's *other* helper (`i18n::local_date`) — the same shared fn
/// this now calls.
fn local_short_date(epoch_ms: i64) -> String {
    fauna_core::format::format_unix_local_date_ms(epoch_ms)
}

/// The shared markdown toolbar (`markdown-toolbar` component) — the four universal
/// wrap buttons plus the two `optional_elements` heading/list ones, all six over
/// the same shared `wrap_selection` rule.
///
/// Heading and list are *prefix-only* wraps (`"## "`/`"- "` with an empty suffix),
/// exactly as linux spells them (`compose_toolbar.rs:193`/`:200`) and web's
/// `insert('- ')` — so the full six-button variant needs no rule of its own, just
/// two more marker pairs. ui.yaml scopes them `optional_elements`; tui carries them
/// because the lead app should paint the richest toolbar (priority #4), and the
/// wrap semantics are platform-independent shared Rust.
///
/// `markdown-marker-toggle-button` is a **declared absence** here, not a follow-on:
/// it flips this editor between hidden-marker and dimmed-marker *decoration* modes,
/// and `ui/conversations.md` § Compose-field inline markdown styling ratifies that
/// "the tui's terminal composer shows the markers literally … the decoration engine
/// is GUI-only". With only one mode there is nothing for a toggle to flip.
fn markdown_toolbar() -> Vec<Element> {
    vec![
        Element::label(ids::MARKDOWN_TOOLBAR, ""),
        wrap_button("markdown-bold-button", markdown::BOLD, "**", "**"),
        wrap_button("markdown-italic-button", markdown::ITALIC, "*", "*"),
        wrap_button("markdown-code-button", markdown::CODE, "`", "`"),
        wrap_button("markdown-link-button", markdown::LINK, "[", "](url)"),
        wrap_button("markdown-heading-button", markdown::HEADING, "## ", ""),
        wrap_button("markdown-list-button", markdown::LIST, "- ", ""),
    ]
}

/// One received message's attachment leaves — `dm-attachment-image[i]` for an
/// image, `dm-attachment-file[i]` for anything else, in document order.
///
/// The bytes come from the **shared** loader (`ConversationsManager::attachment_bytes`,
/// keyed by the block's `blob_hash`) — the same resolution path every app uses; only
/// the paint is per-app. An image rasterizes to cell art + protocol pixels through
/// the same `thumbnail::rasterize` feed's `post-image` and Media's thumbnails use, and
/// **degrades to its declared name-and-size placeholder under the same id** when the
/// bytes are absent (not yet fetched, evicted with nowhere to refill from) or
/// undecodable — never a blank row and never a page banner, the shared degrade
/// `ui/media.md` § Thumbnails pins. A file leaf carries the same name and size, which is
/// the text the cross-app test reads.
fn attachment_bubble_elements(
    doc: &RenderDocument,
    manager: &Arc<ConversationsManager>,
) -> Vec<Element> {
    let mut out = Vec::new();
    for block in &doc.blocks {
        let RenderBlock::Attachment {
            blob_hash,
            filename,
            size_bytes,
            is_image,
            c2pa,
            ..
        } = block
        else {
            continue;
        };
        let bytes = manager.attachment_bytes(blob_hash.clone());
        // The declared shape — name and size — is what a file always shows and
        // what a picture without its bytes falls back to (`conversations.md`
        // § Attachments → *Retention*: "filename and size, no bytes").
        let declared = format!(
            "{filename} ({})",
            crate::wizard::localized(&fauna_core::format::byte_size(*size_bytes))
        );
        if *is_image {
            let art = bytes
                .as_deref()
                .and_then(|b| crate::thumbnail::rasterize(b, crate::thumbnail::POST_IMAGE_COLS));
            out.push(match art {
                Some(art) => Element::thumbnail(ids::DM_ATTACHMENT_IMAGE, art),
                None => Element::label(ids::DM_ATTACHMENT_IMAGE, declared),
            });
        } else {
            out.push(Element::label(ids::DM_ATTACHMENT_FILE, declared));
        }
        // The receiver's own per-attachment verdict — shared Rust probed the
        // decrypted bytes (`AttachmentSnapshot.c2pa`), so the badge sits on the
        // attachment it vouches for, never on the whole message
        // (`conversations.md` § Attachments "C2PA on-device").
        if *c2pa {
            out.push(Element::label(
                ids::C2PA_BADGE,
                fauna_i18n::strings::c2pa::BADGE_LABEL,
            ));
        }
    }
    out
}

/// One message's link-preview cards — `link-preview-card[i]` plus its
/// title/description/domain children, `render-model.md` § D4.
///
/// **Only `Resolved` blocks paint.** `Resolving` and `Failed` paint *no card* —
/// the inline body link is still in the paragraph and already shows the url, so a
/// skeleton would be a perpetual-loading state if a resolve never completes
/// (`render-model.md`:222-227, which supersedes the original skeleton shape). The
/// shared `resolved_link_previews()` projection encodes exactly that filter and
/// recurses into list items / block quotes, so a nested bare url is not silently
/// missed — the reason no client re-walks the blocks itself.
///
/// The og:image is **reveal-gated** (`render-model.md` § D4 *og:image reveal
/// gate*, user-ratified 2026-06-27): even though it is a blob on the user's *own*
/// nest — so painting it never phones home — it obeys the message's D3
/// remote-content reveal, and it counts toward `has_blocked_remote_images()`, so a
/// message whose only remote content is an og:image still surfaces the one
/// `load-remote-content-button`.
///
/// The identical extraction lives in `feed/mod.rs`'s post-card builder; the two
/// are separate because the ancestor scope differs (`post-card[i]` there, flat
/// at the top level here — the bubble's convention), not because the shape does.
///
/// **`link-preview-card` is `indexed: true`** (ui.yaml § `link_preview_card`, ruled
/// 2026-08-13): a body with several standalone bare urls paints one card per
/// preview under the one bare id, and each card's four children are scoped
/// `.within(ids::LINK_PREVIEW_CARD, n)` so a test reading the second card's title
/// cannot get the first card's. tui was the ONE app that had to conform — the
/// other six nest the children inside the card widget for free; here the element
/// list is flat, so the containment has to be stated. Scope matching is
/// descendant-based (`automation.rs::matches`, e2e-conventions.md § convention
/// 1), so an unscoped or ancestor-scoped query still resolves those children.
fn link_preview_bubble_elements(doc: &RenderDocument) -> Vec<Element> {
    let mut out = Vec::new();
    for (n, preview) in doc.resolved_link_previews().into_iter().enumerate() {
        out.push(Element::label(
            ids::LINK_PREVIEW_CARD,
            preview.title.to_string(),
        ));
        out.push(
            Element::label(ids::LINK_PREVIEW_TITLE, preview.title.to_string())
                .within(ids::LINK_PREVIEW_CARD, n),
        );
        out.push(
            Element::label(
                ids::LINK_PREVIEW_DESCRIPTION,
                preview.description.to_string(),
            )
            .within(ids::LINK_PREVIEW_CARD, n),
        );
        // The host alone, through the shared formatter — never the scheme, path
        // or port (`render-model.md`:221).
        out.push(
            Element::label(
                ids::LINK_PREVIEW_DOMAIN,
                fauna_core::format::url_host(preview.url),
            )
            .within(ids::LINK_PREVIEW_CARD, n),
        );
        if preview.revealed
            && let Some(hash) = preview.image_hash
        {
            out.push(
                Element::label(ids::LINK_PREVIEW_IMAGE, hash.to_string())
                    .within(ids::LINK_PREVIEW_CARD, n),
            );
        }
    }
    out
}

/// The attach control + the staged-attachment chip row — one builder for both
/// composers (`dm-compose-bar`'s inline reply and `dm-compose-form`'s new thread),
/// because ui.yaml declares the identical id set on each and the only difference is
/// which manager mutator `Action::CommitAttachment` routes to (by `Mode`).
///
/// `attachment-button` is an **input**, not a gesture button: a terminal has no OS
/// file chooser, so the ratified replacement is path entry (`apps/tui.md`
/// § Declared platform absences 4) — the same shape feed's `compose-file` and
/// `profile-edit-avatar` ship. It is an `input_commit` so the cross-app
/// type-then-click idiom works: the driver's `set_input_files` state patch and a
/// human's Enter land on one code path.
///
/// Each staged attachment gets a chip naming the file and its size through the
/// **shared** `fauna_core::format::byte_size` (`value-formatting.md` § Byte sizes —
/// never a per-app hand-roll), plus the sibling × that unstages it.
fn attachment_elements(
    attachments: &[fauna_conversations::AttachmentDraft],
    supported: bool,
) -> Vec<Element> {
    let mut out = vec![
        Element::input_commit(
            ids::ATTACHMENT_BUTTON,
            String::new(),
            Field::Conversations(ConversationsField::AttachPath),
            Gesture::Conversations(Action::CommitAttachment),
        )
        .labelled(conversations::unified::ATTACHMENT_BUTTON)
        // Capability-gated exactly as linux gates its button
        // (`compose_bar.rs:418` — `set_sensitive(caps.supports_attachments)`): a
        // rail that drops attachments paints the control disabled rather than
        // hiding it, so `get_attr("attachment-button", "disabled")` answers the
        // cross-app contract `test_capability_gating.py` pins. Clients gate on
        // `capabilities.*`, never on the rail.
        .enabled(supported),
    ];
    for (i, draft) in attachments.iter().enumerate() {
        out.push(Element::label(
            ids::DM_COMPOSE_ATTACHMENT_CHIP,
            format!(
                "{} ({})",
                draft.filename,
                crate::wizard::localized(&fauna_core::format::byte_size(draft.size_bytes))
            ),
        ));
        out.push(Element::gesture_button(
            ids::DM_COMPOSE_ATTACHMENT_REMOVE,
            conversations::unified::ATTACHMENT_REMOVE,
            true,
            Gesture::Conversations(Action::RemoveAttachment { index: i as u32 }),
        ));
    }
    out
}

fn wrap_button(id: &str, label: &str, prefix: &'static str, suffix: &'static str) -> Element {
    Element::gesture_button(
        id,
        label,
        true,
        Gesture::Conversations(Action::MarkdownWrap { prefix, suffix }),
    )
}

/// The `recipient-resolve-status` display text for a `ResolveState` (`Idle` paints
/// nothing).
fn resolve_status_text(state: ResolveState) -> &'static str {
    match state {
        ResolveState::Idle => "",
        ResolveState::Resolving => conversations::unified::RECIPIENT_RESOLVE_RESOLVING,
        ResolveState::Resolved => conversations::unified::RECIPIENT_RESOLVE_RESOLVED,
        ResolveState::NotFound => conversations::unified::RECIPIENT_RESOLVE_NOT_FOUND,
        ResolveState::Error => conversations::unified::RECIPIENT_RESOLVE_ERROR,
    }
}

/// The `state` automation-attribute value for a `ResolveState` — the
/// cross-app contract the recipient-picker suite reads via
/// `get_attr("recipient-resolve-status", "state")`. The five names match linux
/// verbatim (`recipient_picker.rs:160-178`); they are an automation surface, not
/// user-facing text, so they stay hard-coded rather than i18n'd.
fn resolve_state_name(state: ResolveState) -> &'static str {
    match state {
        ResolveState::Idle => "idle",
        ResolveState::Resolving => "resolving",
        ResolveState::Resolved => "resolved",
        ResolveState::NotFound => "not-found",
        ResolveState::Error => "error",
    }
}

// ── e2e state serializer ──────────────────────────────────────────────────────

/// The `data.conversation_threads` half of `GET /app/state` — ui.yaml's
/// `conversations.state_fields`, exactly.
///
/// This is **not** optional decoration: the cross-app action layer's
/// `list_threads()` reads the thread set from here (`conversations.py`), and the
/// shared row shape is the contract every app publishes. The row-building
/// itself lives in `fauna_conversations::state_json` (priority #2, 2026-07-20 —
/// lifted out of an identical hand-rolled copy here and in linux's
/// `conversations::state::build_conversation_threads_state`, both now this one
/// function); windows carries its own C# twin,
/// `AppDataSnapshot.GetConversationsThreadsForState`.
pub fn state_json(state: &ConversationsState) -> Value {
    match state.manager.as_ref() {
        Some(manager) => fauna_conversations::state_json::conversation_threads_json(manager),
        None => Value::Array(Vec::new()),
    }
}

/// `data.conversation_sort` — the list's active order, which the rows alone
/// cannot name (shared `fauna_conversations::state_json::conversation_sort_json`).
/// `null` before a manager exists.
pub fn sort_state_json(state: &ConversationsState) -> Value {
    state.manager.as_ref().map_or(Value::Null, |manager| {
        fauna_conversations::state_json::conversation_sort_json(manager)
    })
}

/// `data.selected_thread_id` — linux's twin, off the same shared serializer.
pub fn selected_state_json(state: &ConversationsState) -> Value {
    state.manager.as_ref().map_or(Value::Null, |manager| {
        fauna_conversations::state_json::selected_thread_id_json(manager)
    })
}

// ── Mock-backend inject commands (e2e `/app/commands`) ────────────────────────
//
// The tui twins of linux's `handle_conversations_*` (`apps/fauna-linux/src/main.rs`).
// They route through the `test-helpers` seams (`install_mock_backends_for_test`
// is applied at `init` in e2e mode), so a message/thread lands in the manager with
// **no nest and no MLS engine** — the same shape the cross-app tier_2
// conversations suites drive. Each returns whether it was recognized (a bad
// payload logs and returns `false`, never a silent green).

/// `conversations_inject_inbound` — inject a rendered inbound (or, with
/// `is_own`, outbound) message into the mock backend. Flat payload shape (the
/// action layer does `body.update(payload)`): `{rail, sender, recipient?,
/// subject?, body, message_id?, in_reply_to?, force_subject_change?, is_own?,
/// attachments?}`. Mirrors linux's `handle_conversations_inject_inbound`.
/// Compiled out of release artifacts (`docs/goal/architecture/testing.md`
/// convention 15): it drives a `fauna-conversations` `test-helpers` seam that is
/// itself absent from a release build. Its only caller is the gated-real
/// `automation::apply_command`, so no no-op twin is needed.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub fn inject_inbound(state: &ConversationsState, payload: &Value) -> bool {
    let Some(manager) = state.manager.as_ref() else {
        tracing::debug!("[agent] conversations_inject_inbound: no manager (pre-auth)");
        return false;
    };
    let Some(p) = payload.as_object() else {
        tracing::debug!("[agent] conversations_inject_inbound: missing payload");
        return false;
    };
    match manager.inject_inbound_from_test_payload(p) {
        Ok(()) => true,
        Err(e) => {
            tracing::debug!("[agent] conversations_inject_inbound: {e}");
            false
        }
    }
}

/// `conversations_create_mls_group` — bootstrap a FaunaMls group thread with the
/// given handles (test seam). Flat payload: `{participants: [handle, …]}`.
/// Compiled out of release artifacts (`docs/goal/architecture/testing.md`
/// convention 15): it drives a `fauna-conversations` `test-helpers` seam that is
/// itself absent from a release build. Its only caller is the gated-real
/// `automation::apply_command`, so no no-op twin is needed.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub fn create_mls_group(state: &ConversationsState, payload: &Value) -> bool {
    use fauna_conversations::TypedAddress;

    let Some(manager) = state.manager.as_ref() else {
        return false;
    };
    let participants: Vec<TypedAddress> = payload
        .get("participants")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| p.as_str())
                .map(|s| TypedAddress::Fauna {
                    handle: s.to_string(),
                    // ⚠ NOT `ActorId([0u8; 32])`, which is what this seam minted
                    // until 2026-08-09: that gave every member of a fixture group
                    // ONE identity, so any per-person assertion (a review flag, a
                    // per-member badge) either lit up on all of them or on none,
                    // and could not fail for the right reason. The shared helper
                    // is deterministic per handle, so a driver can address a
                    // specific member.
                    actor_id: fauna_conversations::manager::test_actor_id_for_handle(s),
                })
                .collect()
        })
        .unwrap_or_default();
    manager.create_mls_group(participants);
    true
}

/// `conversations_inject_send_failure` — force a thread's compose into
/// `SendState::Failed` so `error-message` surfaces (test seam). Flat payload:
/// `{thread_id, reason?}`.
/// Compiled out of release artifacts (`docs/goal/architecture/testing.md`
/// convention 15): it drives a `fauna-conversations` `test-helpers` seam that is
/// itself absent from a release build. Its only caller is the gated-real
/// `automation::apply_command`, so no no-op twin is needed.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub fn inject_send_failure(state: &ConversationsState, payload: &Value) -> bool {
    let Some(manager) = state.manager.as_ref() else {
        return false;
    };
    let thread_id = payload
        .get("thread_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if thread_id.is_empty() {
        tracing::debug!("[agent] conversations_inject_send_failure: missing thread_id");
        return false;
    }
    let reason = payload
        .get("reason")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("nest rejected fauna.email.send")
        .to_string();
    manager.inject_send_failure_for_test(&ThreadId(thread_id.to_string()), reason);
    true
}

/// `conversations_inject_page_error` — stamp the snapshot's page-level error so
/// `error-message` surfaces a failed **membership/label** wire op (test seam;
/// the twin of [`inject_send_failure`], which covers sends). Flat payload:
/// `{key?, message?}` — `key` is the i18n key the app resolves, `message` its
/// `{message}` substitution, so a test can assert the *resolved* text rather
/// than a raw key.
///
/// Honours the command or fails loudly by returning `false`
/// (`../../../docs/goal/architecture/testing.md` point 11) — never a silent
/// drop, which downstream reads as a real product bug.
/// Compiled out of release artifacts (`docs/goal/architecture/testing.md`
/// convention 15): it drives a `fauna-conversations` `test-helpers` seam that is
/// itself absent from a release build. Its only caller is the gated-real
/// `automation::apply_command`, so no no-op twin is needed.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub fn inject_page_error(state: &ConversationsState, payload: &Value) -> bool {
    let Some(manager) = state.manager.as_ref() else {
        return false;
    };
    let key = payload
        .get("key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("conversations.unified.error_add_participant");
    let message = payload
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("no key package published")
        .to_string();
    manager.inject_page_error_for_test(fauna_core::localized::LocalizedText::key_arg(
        key, "message", message,
    ));
    true
}

/// `conversations_seed_resolved_link_preview` — record a **pre-resolved** D4
/// preview for `url`, so a bubble whose body is the standalone `[url](url)`
/// paragraph folds its `LinkPreview` block `Resolved` and paints the card.
/// Flat payload: `{url, title?, description?, image_hash?}`.
///
/// Why a seam and not the real resolve: `fauna.linkpreview.resolve` needs the nest
/// to fetch a live OpenGraph page (SSRF-guarded — `render-model.md` § D4
/// *Producer location*), so the cross-app e2e seeds the terminal state
/// deterministically. The conversations twin of the feed's `feed_inject_posts`
/// `link_preview` spec, and the tui twin of linux's
/// `handle_conversations_seed_resolved_link_preview`.
///
/// `revealed: false` at seed (the manager's own posture), so the card's og:image
/// stays blocked until the message's `load-remote-content-button` is tapped —
/// which is precisely the transition the e2e asserts.
///
/// Honours the command or fails loudly by returning `false`
/// (`../../../docs/goal/architecture/testing.md` point 11): a missing manager
/// (pre-auth) or a missing `url` is refused, never silently dropped. An empty
/// `image_hash` string means *no og:image*, not a zero-length hash — the same
/// normalisation linux applies, so `image_hash: null` and `""` agree.
/// Compiled out of release artifacts (convention 15) for the same reason as its
/// siblings above: the manager seam it drives is itself absent from a release build.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub fn seed_resolved_link_preview(state: &ConversationsState, payload: &Value) -> bool {
    let Some(manager) = state.manager.as_ref() else {
        tracing::debug!("[agent] conversations_seed_resolved_link_preview: no manager (pre-auth)");
        return false;
    };
    let Some(url) = payload
        .get("url")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    else {
        return false;
    };
    let text = |field: &str| {
        payload
            .get(field)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    manager.seed_resolved_link_preview_for_test(
        url.to_string(),
        text("title"),
        text("description"),
        payload
            .get("image_hash")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    );
    true
}

/// `conversations_accept_recipient` — commit the active picker's current text as a
/// chip (the manager picks which picker is active and whether the text parses).
///
/// `Err` is convention 11's **declining-arm** clause: an arm that is dispatched
/// to and then quietly does nothing is indistinguishable from a working one at
/// the driver, so the caller stamps the reason on the app's own `error-message`
/// (`report_real_outcome`). Until 2026-08-28 this discarded
/// `accept_current_recipient_chip`'s boolean and returned `true` unconditionally,
/// so an accept against an untouched picker acked green and surfaced ~5 s later
/// as the action layer's generic "chip not added" — naming neither the command
/// nor the reason. web hit the identical gap and fixed it the same way.
pub async fn accept_recipient(state: &ConversationsState) -> Result<(), String> {
    let Some(manager) = state.manager.as_ref() else {
        return Err("no conversations manager on this seat (not signed in yet)".to_string());
    };
    // **Probe first, then commit** — the same order the keyboard accept path
    // drives (`PendingResolve` → `accept_current_recipient_chip`). Committing
    // without the probe can only ever use the format-only parse, and
    // `try_parse_typed_address` cannot produce `TypedAddress::Fauna` by design,
    // so a typed Fauna handle or 64-hex actor id committed no chip at all over
    // the agent while working for a real user. See the linux twin
    // (`conv_backend::e2e_accept_recipient`) for the full account.
    manager.resolve_recipient().await;
    if !manager.accept_current_recipient_chip() {
        return Err(fauna_conversations::manager::ACCEPT_RECIPIENT_NO_CHIP_REASON.to_string());
    }
    // The same follow-up the keyboard accept runs (`Op::AcceptRecipientChip`).
    manager.refresh_list_send(None).await;
    Ok(())
}

/// Milliseconds since the Unix epoch, saturating to 0 before it.
fn now_ms() -> i64 {
    fauna_core::data::Timestamp::now_millis_or_zero() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_conversations::backends::mock::MockRailBackend;
    use serde_json::json;
    use std::collections::BTreeSet;

    use crate::app::tests::authed_app;
    use crate::pages::Page;

    /// An authenticated app on the conversations page with a manager + mock
    /// backends, and `threads` injected through the inbound seam (no nest, no
    /// session, no MLS engine) — the analogue of the feed page's `feed_app`.
    /// Each tuple is `(rail, sender, subject)`.
    fn conv_app(threads: &[(&str, &str, &str)]) -> App {
        let mut app = authed_app();
        let manager = ConversationsManager::new();
        manager.install_mock_backends_for_test();
        // Struct-update, not a hand-listed field set: a branch that grows
        // `ConversationsState` then merges cleanly instead of colliding on the
        // grown axis — the project-wide fixture-shape convention.
        let state = ConversationsState {
            manager: Some(manager),
            ..Default::default()
        };
        for (rail, sender, subject) in threads {
            assert!(
                inject_inbound(
                    &state,
                    &json!({ "rail": rail, "sender": sender, "subject": subject, "body": "hi" }),
                ),
                "inject_inbound should route through the mock backend"
            );
        }
        app.conversations = state;
        app.page = Page::Conversations;
        app
    }

    fn ids(app: &App) -> BTreeSet<String> {
        elements(app)
            .into_iter()
            .map(|e| e.id)
            .filter(|id| !id.is_empty()) // chrome
            .collect()
    }

    /// Every id the list emits is one ui.yaml scopes to the `conversations` page
    /// (its `elements` + the `conversation-list-item` component) — the "no
    /// invisible shim elements" rule. `dm-sender` is deliberately absent (it is a
    /// *bubble* element, not a list-row one — `conversations.md` § Layout).
    #[test]
    fn the_conversations_list_registers_only_ui_yaml_ids() {
        let app = conv_app(&[("Smtp", "alice@host.test", "Q4 budget")]);
        let allowed: BTreeSet<String> = [
            "page-heading",
            "conversations-view",
            "conversation-search-box",
            "conversation-sort",
            "new-conversation-button",
            "conversation-item",
            "dm-subject",
            "protocol-icon",
            "dm-unread-indicator",
            "conversation-item-timestamp",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let got = ids(&app);
        let invented: Vec<&String> = got.difference(&allowed).collect();
        assert!(invented.is_empty(), "invented ids: {invented:?}");
        for required in [
            "page-heading",
            "conversation-search-box",
            "conversation-sort",
            "new-conversation-button",
            "conversation-item",
        ] {
            assert!(got.contains(required), "the list must register {required}");
        }
    }

    /// The empty list still registers the chrome (search / sort / new) so a
    /// freshly-signed-in user can start a conversation — and never invents a
    /// `conversations-empty` id for the empty-state line (it paints as chrome).
    #[test]
    fn the_empty_list_registers_the_chrome_but_no_rows() {
        let app = conv_app(&[]);
        let got = ids(&app);
        assert!(got.contains("new-conversation-button"));
        assert!(
            !got.contains("conversation-item"),
            "no rows for an empty thread list"
        );
        assert!(
            !got.iter().any(|id| id.contains("empty")),
            "the empty-state line paints as chrome, registering no id"
        );
    }

    /// **The element list is the registry; the viewport clips paint only.** Every
    /// thread registers a row however small the terminal (the e2e pty is 40×120).
    #[test]
    fn every_thread_registers_a_row_however_small_the_terminal() {
        let threads: Vec<(&str, String, String)> = (0..30)
            .map(|i| {
                (
                    "Smtp",
                    format!("sender{i}@host.test"),
                    format!("subject {i}"),
                )
            })
            .collect();
        let refs: Vec<(&str, &str, &str)> = threads
            .iter()
            .map(|(r, s, sub)| (*r, s.as_str(), sub.as_str()))
            .collect();
        let app = conv_app(&refs);
        let rows = elements(&app)
            .iter()
            .filter(|e| e.id == "conversation-item")
            .count();
        assert_eq!(rows, 30, "the registry must not be clipped to the viewport");
    }

    /// The backup-audit feed's account resolution must key by the
    /// SESSION's own account, never a fresh `registry(app).active()` read —
    /// on a bound (secondary) launch the two can genuinely diverge, and the
    /// rendered threads are always the session's own (`account-scoping.md`
    /// § Concurrent instances, :818-822).
    ///
    /// Pins `backup_audit_key`'s pure resolution — the sibling test
    /// `backup_audit_observation_keys_by_the_session_account_not_active` pins
    /// `backup_audit_observation`'s own forward of this key
    /// , closing the gap this comment used to
    /// excuse: pinning `backup_audit_key` alone never proved the render
    /// loop's actual call site forwarded what it computed.
    #[test]
    fn backup_audit_key_resolves_by_the_session_account_not_active() {
        let app = conv_app(&[("Smtp", "alice@host.test", "Q4 budget")]);
        let bound_actor = app.session.as_ref().expect("authed").actor_id.clone();

        // Add and activate a SECOND account — the session stays bound to the
        // first even though `registry(app).active()` now names the other one.
        const OTHER_SECRET: [u8; 32] = [9u8; 32];
        let other_actor = crate::session::registry(&app)
            .add_account(&hex::encode(OTHER_SECRET), None, None)
            .expect("seed a second account");
        crate::session::registry(&app)
            .set_active(&other_actor)
            .expect("pin the other account active");
        assert_ne!(
            bound_actor, other_actor,
            "the fixture must seed two genuinely distinct accounts"
        );
        assert_eq!(
            crate::session::registry(&app).active().as_deref(),
            Some(other_actor.as_str()),
            "active() must now name the OTHER account — the two reads \
             genuinely diverge, which is exactly why keying off active() \
             was unsafe"
        );

        let snapshot = app
            .conversations
            .snapshot()
            .expect("conv_app installs a manager");
        let (observed_actor, _) = backup_audit_key(&app, &snapshot).expect("a thread is present");
        assert_eq!(
            observed_actor, bound_actor,
            "the audit observation key must resolve by the session's own \
             account, never a fresh registry(app).active() read"
        );
    }

    /// Sibling of `backup_audit_key_resolves_by_the_session_account_not_active`,
    /// but pins `backup_audit_observation` — the render loop's actual call
    /// site — instead of the pure `backup_audit_key` helper it forwards. The
    /// account-keying wiring is what was unwitnessed here, not the wiring's
    /// existence: the tui e2e leg
    /// (`test_backup_audit_observation.py::…_tui`) already reads this
    /// function's on-disk effect back, it just runs against one signed-in
    /// account, so `active()` and the session account can never diverge
    /// there (`account-scoping.md` § Concurrent instances, :818-822).
    ///
    /// **Residual limit closed :** this test alone
    /// only proves the return matches `backup_audit_key`'s resolution — it
    /// cannot prove the forwarded call used that same binding rather than a
    /// different value routed there separately. The sibling test
    /// `backup_audit_observation_returns_the_sinks_own_pair_not_a_separate_local`
    /// closes that: `backup_audit_observation` now returns
    /// `observe_thread_activity`'s own return directly, so a two-line
    /// mutation (a new local forwarded to `observe_thread_activity`, the
    /// original binding left in the return) shows up there instead.
    #[test]
    fn backup_audit_observation_keys_by_the_session_account_not_active() {
        let app = conv_app(&[("Smtp", "alice@host.test", "Q4 budget")]);
        let bound_actor = app.session.as_ref().expect("authed").actor_id.clone();

        const OTHER_SECRET: [u8; 32] = [8u8; 32];
        let other_actor = crate::session::registry(&app)
            .add_account(&hex::encode(OTHER_SECRET), None, None)
            .expect("seed a second account");
        crate::session::registry(&app)
            .set_active(&other_actor)
            .expect("pin the other account active");
        assert_ne!(
            bound_actor, other_actor,
            "the fixture must seed two genuinely distinct accounts"
        );

        let snapshot = app
            .conversations
            .snapshot()
            .expect("conv_app installs a manager");
        let expected = backup_audit_key(&app, &snapshot).expect("a thread is present");
        let observed = backup_audit_observation(&app, &snapshot).expect("a thread is present");
        assert_eq!(
            observed, expected,
            "backup_audit_observation must return the exact key it forwarded \
             to observe_thread_activity, matching backup_audit_key's own \
             session-account resolution"
        );
    }

    /// Closes 's residual limit on the sibling test
    /// above: pins that `backup_audit_observation`'s return IS
    /// `observe_thread_activity`'s own return for the same call, not merely
    /// a value that happens to match `backup_audit_key`'s resolution. The
    /// expected actor is derived independently, straight from
    /// `backup_audit_key` — never re-derived from `backup_audit_observation`'s
    /// own return, which would make this tautological. Needs the same
    /// two-account divergence as the sibling test above: a mutation that
    /// forwards a freshly-resolved `active()` local to `observe_thread_activity`
    /// instead of the session actor is invisible on a single-account fixture,
    /// where the two happen to be equal.
    #[test]
    fn backup_audit_observation_returns_the_sinks_own_pair_not_a_separate_local() {
        let app = conv_app(&[("Smtp", "alice@host.test", "Q4 budget")]);
        let bound_actor = app.session.as_ref().expect("authed").actor_id.clone();

        const OTHER_SECRET: [u8; 32] = [6u8; 32];
        let other_actor = crate::session::registry(&app)
            .add_account(&hex::encode(OTHER_SECRET), None, None)
            .expect("seed a second account");
        crate::session::registry(&app)
            .set_active(&other_actor)
            .expect("pin the other account active");
        assert_ne!(
            bound_actor, other_actor,
            "the fixture must seed two genuinely distinct accounts"
        );

        let snapshot = app
            .conversations
            .snapshot()
            .expect("conv_app installs a manager");
        let (expected_actor, expected_ms) =
            backup_audit_key(&app, &snapshot).expect("a thread is present");

        let direct = crate::backup_audit::observe_thread_activity(&expected_actor, expected_ms)
            .expect("the lock is never poisoned in this test");
        let via_render_path =
            backup_audit_observation(&app, &snapshot).expect("a thread is present");

        assert_eq!(
            via_render_path, direct,
            "backup_audit_observation must return observe_thread_activity's \
             own return for the same call, not a separately-held local"
        );
    }

    /// `data.conversation_threads[]` is ui.yaml's declared `conversations.state_fields`,
    /// and the action layer's `list_threads()` reads the thread set from here — so a
    /// missing/misshaped row is a silent test failure, not a missing feature.
    #[test]
    fn state_json_carries_the_declared_thread_rows() {
        let app = conv_app(&[("Smtp", "alice@host.test", "Q4 budget")]);
        let rows = state_json(&app.conversations);
        let row = &rows[0];
        assert_eq!(row["rail"], "Smtp");
        assert_eq!(row["flavor"], "SubjectKeyed");
        assert!(
            row["thread_id"].as_str().is_some_and(|s| !s.is_empty()),
            "a thread carries a stable id"
        );
        assert_eq!(row["message_count"], 1);
        // Every declared state_field is present (the cross-app row contract).
        for key in [
            "thread_id",
            "label",
            "snippet",
            "rail",
            "flavor",
            "unread_count",
            "participant_count",
            "message_count",
            "message_subject_lines",
        ] {
            assert!(row.get(key).is_some(), "state row missing `{key}`");
        }
    }

    /// The sort toggle cycles the three `SortOrder`s and the manager owns the
    /// choice (no client-side sort state — Architectural rule 6).
    #[test]
    fn sort_toggles_through_the_three_orders() {
        let mut app = conv_app(&[("Smtp", "a@host.test", "s")]);
        let order = |app: &App| app.conversations.snapshot().unwrap().sort;
        assert_eq!(order(&app), SortOrder::LatestActivity);
        apply_local(&mut app, Action::Sort);
        assert_eq!(order(&app), SortOrder::OldestFirst);
        apply_local(&mut app, Action::Sort);
        assert_eq!(order(&app), SortOrder::Unread);
        apply_local(&mut app, Action::Sort);
        assert_eq!(order(&app), SortOrder::LatestActivity);
    }

    /// `new-conversation-button` opens the in-pane compose sub-page (a mode
    /// switch + the manager's `start_new_conversation`), never a modal
    /// (`conversations.md` § Layout & flow). Cancel returns to the list.
    #[test]
    fn new_conversation_opens_and_cancels_the_compose_sub_page() {
        let mut app = conv_app(&[]);
        assert_eq!(app.conversations.mode, Mode::List);
        apply_local(&mut app, Action::StartNewConversation);
        assert_eq!(app.conversations.mode, Mode::Compose);
        assert!(
            app.conversations
                .snapshot()
                .unwrap()
                .new_thread_compose
                .is_some(),
            "the manager holds the new-thread compose draft"
        );
        apply_local(&mut app, Action::CancelNewConversation);
        assert_eq!(app.conversations.mode, Mode::List);
    }

    /// Selecting a thread opens the detail sub-page and tells the manager which
    /// thread is selected.
    #[test]
    fn selecting_a_thread_opens_detail_and_selects_it_in_the_manager() {
        let mut app = conv_app(&[("Smtp", "alice@host.test", "Q4 budget")]);
        let thread_id = app.conversations.snapshot().unwrap().threads[0]
            .thread_id
            .clone();
        apply_local(&mut app, Action::SelectThread(thread_id.0.clone()));
        assert_eq!(app.conversations.mode, Mode::Detail(thread_id.clone()));
        assert_eq!(
            app.conversations.snapshot().unwrap().selected_thread_id,
            Some(thread_id)
        );
    }

    /// A new-thread send lands on the thread it created — sent or refused. The
    /// manager materializes the thread, moves the draft onto its compose and
    /// selects it before the send is even attempted (`conversations.md`
    /// § Persistence, the `new_thread_compose` bullet), and tui shows a list OR
    /// a thread, so the page has to follow: left on the new-thread pane, whose
    /// slot the send just emptied, the author of a refused send faced a blank
    /// composer while their words and their file waited on a thread they were
    /// never shown.
    #[tokio::test]
    async fn a_new_thread_send_lands_on_the_thread_it_created_even_when_refused() {
        // The restart the refusal exists for: the device before the relaunch
        // staged the file, and only the draft's handle for it reaches this one.
        let before = ConversationsManager::new();
        before.install_mock_backends_for_test();
        before.start_new_conversation();
        before.set_new_thread_body("words that wait for their file".into());
        before
            .add_new_thread_attachment("photo.png".into(), "image/png".into(), vec![1, 2, 3])
            .expect("an open composer stages the file");
        let rested = before.drafts_snapshot_bytes();

        let mut app = conv_app(&[]);
        let manager = app.conversations.manager.clone().unwrap();
        manager.restore_drafts(rested).await;
        apply_local(&mut app, Action::StartNewConversation);
        manager.set_new_thread_recipient_input("someone@host.test".into());
        manager.resolve_recipient().await;
        assert!(manager.accept_current_recipient_chip());

        let op = apply_local(
            &mut app,
            Action::SendNewThread {
                rail: None,
                founds_room: false,
            },
        )
        .expect("an op");
        apply_outcome(&mut app, op.run().await);

        let thread = app
            .conversations
            .snapshot()
            .unwrap()
            .selected_thread_id
            .expect("the send selected the thread it created");
        assert_eq!(app.conversations.mode, Mode::Detail(thread));
        assert_eq!(
            texts_of(&app, ids::DM_TEXT_FIELD),
            vec!["words that wait for their file".to_string()],
            "the refused draft's words are on the composer the page now shows"
        );
        assert_eq!(
            count_id(&app, ids::DM_COMPOSE_ATTACHMENT_CHIP),
            1,
            "and so is its file, for the author to drop or attach again"
        );

        // And a send that goes lands the same way.
        let mut app = conv_app(&[]);
        let manager = app.conversations.manager.clone().unwrap();
        apply_local(&mut app, Action::StartNewConversation);
        manager.set_new_thread_body("hello".into());
        manager.set_new_thread_recipient_input("someone-else@host.test".into());
        manager.resolve_recipient().await;
        assert!(manager.accept_current_recipient_chip());
        let op = apply_local(
            &mut app,
            Action::SendNewThread {
                rail: None,
                founds_room: false,
            },
        )
        .expect("an op");
        apply_outcome(&mut app, op.run().await);
        let thread = app.conversations.snapshot().unwrap().selected_thread_id;
        assert!(thread.is_some(), "the send selected the thread it created");
        assert_eq!(app.conversations.mode, Mode::Detail(thread.unwrap()));
    }

    /// Each list row paints its thread's last-activity time as
    /// `conversation-item-timestamp`, scoped within that row's own
    /// `conversation-item`, through the shared `conversation_timestamp_display`
    /// buckets — the same text a message bubble's time takes, never a
    /// hand-rolled clock (`conversations.md` § Layout & flow; `value-formatting.md`
    /// § Conversation timestamp).
    #[test]
    fn a_list_row_paints_its_last_activity_time_through_the_shared_formatter() {
        let app = conv_app(&[
            ("Smtp", "alice@host.test", "Q4 budget"),
            ("Smtp", "bob@host.test", "Offsite"),
        ]);
        let threads = app.conversations.snapshot().unwrap().threads;
        let stamps: Vec<Element> = elements(&app)
            .into_iter()
            .filter(|e| e.id == ids::CONVERSATION_ITEM_TIMESTAMP)
            .collect();
        assert_eq!(stamps.len(), threads.len(), "one time per row");
        for (i, (stamp, thread)) in stamps.iter().zip(&threads).enumerate() {
            assert_eq!(
                stamp.path,
                vec![(ids::CONVERSATION_ITEM.to_string(), i)],
                "row {i}'s time is scoped within its own conversation-item"
            );
            assert!(thread.last_activity_ms > 0, "a seeded thread has activity");
            assert_eq!(stamp.text, message_timestamp_text(thread.last_activity_ms));
        }
    }

    fn unread_indicators(app: &App) -> usize {
        elements(app)
            .iter()
            .filter(|e| e.id == ids::DM_UNREAD_INDICATOR)
            .count()
    }

    /// A row paints `dm-unread-indicator` while its thread holds a message
    /// nobody opened it over, and loses it once the thread is opened — the
    /// count is shared Rust's, the row only renders it (`conversations.md`
    /// § State & data shape → *When a thread is read*).
    #[test]
    fn a_list_row_paints_unread_until_its_thread_is_opened() {
        let mut app = conv_app(&[
            ("Smtp", "alice@host.test", "Q4 budget"),
            ("Smtp", "bob@host.test", "Offsite"),
        ]);
        assert_eq!(unread_indicators(&app), 2, "both arrived unopened");
        let opened = app.conversations.snapshot().unwrap().threads[0]
            .thread_id
            .clone();
        apply_local(&mut app, Action::SelectThread(opened.0));
        // The tab is the agent's — and a mouse user's — way back to the list.
        let _ = app.apply(Page::Conversations);
        assert_eq!(app.conversations.mode, Mode::List);
        assert_eq!(
            unread_indicators(&app),
            1,
            "opening one thread reads no other"
        );
    }

    /// tui shows the list OR the thread, never both, so a thread the user left
    /// for the list is no longer open: the manager must be told, or everything
    /// that arrives in it afterwards is read on arrival and never flagged. Both
    /// ways out go through the same door — the tab and Esc.
    #[test]
    fn a_thread_left_for_the_list_is_no_longer_being_read() {
        for leave_by_esc in [false, true] {
            let mut app = conv_app(&[("Smtp", "alice@host.test", "Q4 budget")]);
            let id = app.conversations.snapshot().unwrap().threads[0]
                .thread_id
                .clone();
            apply_local(&mut app, Action::SelectThread(id.0));
            if leave_by_esc {
                app.handle_key(crossterm::event::KeyEvent::from(
                    crossterm::event::KeyCode::Esc,
                ));
            } else {
                let _ = app.apply(Page::Conversations);
            }
            assert_eq!(app.conversations.mode, Mode::List);
            assert_eq!(
                app.conversations.snapshot().unwrap().selected_thread_id,
                None,
                "leave_by_esc={leave_by_esc}"
            );
            assert!(inject_inbound(
                &app.conversations,
                &json!({ "rail": "Smtp", "sender": "alice@host.test",
                         "subject": "Q4 budget", "body": "one more" }),
            ));
            assert_eq!(unread_indicators(&app), 1, "leave_by_esc={leave_by_esc}");
        }
    }

    /// The compose sub-page registers only ui.yaml `conversations.sub_pages.compose`
    /// IDs (+ the `markdown-toolbar` component, all six buttons).
    /// `markdown-marker-toggle-button` is deliberately absent — a declared absence,
    /// not buildout (`markdown_toolbar`'s doc comment cites the ratifying line).
    #[test]
    fn the_compose_sub_page_registers_only_ui_yaml_ids() {
        let mut app = conv_app(&[]);
        assert!(apply_local(&mut app, Action::StartNewConversation).is_none());
        assert_eq!(app.conversations.mode, Mode::Compose);
        let allowed: BTreeSet<String> = [
            "page-heading",
            "conversations-view",
            "recipient-picker-input",
            "recipient-picker-chip",
            "recipient-picker-suggestion",
            "recipient-resolve-status",
            "recipient-picker-class",
            "recipient-picker-home-nest-toggle",
            "group-conversation-hint",
            "subject-input",
            "dm-text-field",
            "markdown-toolbar",
            "markdown-bold-button",
            "markdown-italic-button",
            "markdown-code-button",
            "markdown-link-button",
            "markdown-heading-button",
            "markdown-list-button",
            "attachment-button",
            "dm-compose-attachment-chip",
            "dm-compose-attachment-remove",
            "topic-toggle-button",
            "dm-send-button",
            "new-conversation-cancel",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let got = ids(&app);
        let invented: Vec<&String> = got.difference(&allowed).collect();
        assert!(invented.is_empty(), "invented ids: {invented:?}");
        for required in [
            "recipient-picker-input",
            "dm-text-field",
            "dm-send-button",
            "markdown-bold-button",
            "new-conversation-cancel",
        ] {
            assert!(got.contains(required), "compose must register {required}");
        }
        // The list is replaced (like feed's create_feed), not overlaid.
        assert!(!got.contains("conversation-search-box"));
    }

    /// `markdown-bold-button` on an empty composer inserts the shared "text"
    /// placeholder wrapped — the exact `**text**` shape
    /// `test_compose_richeditbox_value_and_toolbar` pins.
    #[test]
    fn bold_wraps_the_empty_composer_with_the_shared_placeholder() {
        let mut app = conv_app(&[]);
        apply_local(&mut app, Action::StartNewConversation);
        apply_local(
            &mut app,
            Action::MarkdownWrap {
                prefix: "**",
                suffix: "**",
            },
        );
        assert_eq!(
            field(&app.conversations, &ConversationsField::ComposeBody),
            "**text**"
        );
    }

    /// The two `optional_elements` buttons are **prefix-only** wraps: an empty
    /// suffix yields `## text` / `- text`, not `## text##`. Same markers linux
    /// passes (`compose_toolbar.rs:193`/`:200`) and web's `insert`, so the
    /// cross-app `test_conversations_markdown_toolbar_wrap.py` assertions hold
    /// verbatim on tui.
    #[test]
    fn heading_and_list_prefix_the_empty_composer_without_a_suffix() {
        for (prefix, expected) in [("## ", "## text"), ("- ", "- text")] {
            let mut app = conv_app(&[]);
            apply_local(&mut app, Action::StartNewConversation);
            apply_local(&mut app, Action::MarkdownWrap { prefix, suffix: "" });
            assert_eq!(
                field(&app.conversations, &ConversationsField::ComposeBody),
                expected,
                "prefix {prefix:?} should produce {expected:?}"
            );
        }
    }

    /// The toolbar's six buttons carry the exact marker pairs the cross-app
    /// `test_conversations_markdown_toolbar_wrap.py` assertions expect.
    ///
    /// **Why this exists as a unit test at all:** the sibling
    /// `heading_and_list_prefix_…` test fires `Action::MarkdownWrap` directly, so it
    /// proves the *rule* but not the *wiring* — a button handed the wrong markers
    /// passes it and fails only in e2e, 10 minutes and one contended build slot
    /// later. Asserting the wired pair in-process is what makes a mis-wired button
    /// a tier_1 red (the standing "assert latency-independent state" preference —
    /// `testing.md` point 14 — applied to the cheapest possible layer).
    #[test]
    fn the_toolbar_wires_the_six_cross_app_marker_pairs() {
        use crate::element::Role;
        let wired: Vec<(String, &'static str, &'static str)> = markdown_toolbar()
            .into_iter()
            .filter_map(|e| match e.role {
                Role::Button(Gesture::Conversations(Action::MarkdownWrap { prefix, suffix })) => {
                    Some((e.id, prefix, suffix))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            wired,
            vec![
                ("markdown-bold-button".to_string(), "**", "**"),
                ("markdown-italic-button".to_string(), "*", "*"),
                ("markdown-code-button".to_string(), "`", "`"),
                ("markdown-link-button".to_string(), "[", "](url)"),
                // Prefix-only: an empty suffix is what yields `## text` / `- text`
                // rather than `## text##`. Same pairs linux passes.
                ("markdown-heading-button".to_string(), "## ", ""),
                ("markdown-list-button".to_string(), "- ", ""),
            ],
            "the toolbar's wired marker pairs are a cross-app contract"
        );
    }

    // ── Attachments (`conversations.md` § Attachments) ──────────────────────
    //
    // tui's `attachment-button` is a typed-path `input_commit`, not a picker
    // (`tui.md` § Declared platform absences 4), so the whole outbound path is
    // reachable in-process: write the field, fire the commit, read the chips. No
    // e2e slot needed to prove the mechanism — the e2e proves the driver seam.

    /// Write a temp file and hand back its path — the stand-in for "the user typed
    /// a path that exists".
    fn temp_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("fauna-tui-attach-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn commit_path(app: &mut App, path: &std::path::Path) {
        set_field(
            &mut app.conversations,
            ConversationsField::AttachPath,
            path.to_string_lossy().into_owned(),
        );
        apply_local(app, Action::CommitAttachment);
    }

    /// Both composers register the attach control — ui.yaml declares
    /// `attachment-button` on `dm-compose-bar` AND `dm-compose-form`, and one
    /// builder serves both.
    #[test]
    fn attachment_button_paints_on_both_composers() {
        let mut compose = conv_app(&[]);
        apply_local(&mut compose, Action::StartNewConversation);
        assert!(
            ids(&compose).contains("attachment-button"),
            "the new-thread composer must register attachment-button"
        );

        let detail = detail_app(&[
            json!({ "rail": "Smtp", "sender": "carol@host.test", "body": "hi", "message_id": "m-1" }),
        ]);
        assert!(
            ids(&detail).contains("attachment-button"),
            "the reply compose bar must register attachment-button too"
        );
    }

    /// Committing a typed path stages it and paints one chip naming the file and
    /// its size through the SHARED `byte_size` formatter — the unit assertion the
    /// cross-app `test_staged_attachment_chip_shows_its_size` makes over e2e.
    #[test]
    fn committing_a_typed_path_stages_one_chip_with_name_and_size() {
        let path = temp_file("notes.txt", b"hello notes\n");
        let mut app = conv_app(&[]);
        apply_local(&mut app, Action::StartNewConversation);
        commit_path(&mut app, &path);

        assert_eq!(
            count_id(&app, "dm-compose-attachment-chip"),
            1,
            "one staged attachment paints exactly one chip"
        );
        let chip = &texts_of(&app, "dm-compose-attachment-chip")[0];
        assert!(
            chip.contains("notes.txt"),
            "chip must name the file: {chip:?}"
        );
        assert!(
            [" B", " KB", " MB"].iter().any(|u| chip.contains(u)),
            "chip must carry the shared byte_size unit: {chip:?}"
        );
        // The buffer is cleared on success, so a second Enter cannot double-stage.
        assert!(app.conversations.attach_path_draft.is_empty());
    }

    /// The remove × unstages it — the mutator that shipped built-and-unused on
    /// every app until the chip rows landed.
    #[test]
    fn removing_a_staged_attachment_drops_its_chip() {
        let path = temp_file("pic.png", b"not really a png");
        let mut app = conv_app(&[]);
        apply_local(&mut app, Action::StartNewConversation);
        commit_path(&mut app, &path);
        assert_eq!(count_id(&app, "dm-compose-attachment-chip"), 1);

        apply_local(&mut app, Action::RemoveAttachment { index: 0 });
        assert_eq!(
            count_id(&app, "dm-compose-attachment-chip"),
            0,
            "remove must drop the chip"
        );
    }

    /// **The loud-failure proof.** An unreadable path must reach `error-message`
    /// and must KEEP the buffer for a fix — never a silent drop, which is the
    /// exact shape of the `folder-save-paths` pending-path bug (2026-07-30) and
    /// what e2e convention 11 forbids.
    #[test]
    fn an_unreadable_path_reports_on_error_message_and_keeps_the_buffer() {
        let mut app = conv_app(&[]);
        apply_local(&mut app, Action::StartNewConversation);
        commit_path(
            &mut app,
            std::path::Path::new("/nope/definitely-not-here.png"),
        );

        assert_eq!(
            count_id(&app, "dm-compose-attachment-chip"),
            0,
            "nothing may stage from an unreadable path"
        );
        assert!(
            app.errors.contains_key(&Page::Conversations),
            "an unreadable path must surface on error-message, not vanish"
        );
        assert_eq!(
            app.conversations.attach_path_draft, "/nope/definitely-not-here.png",
            "the buffer survives so the user can correct the path"
        );
    }

    /// A rail that drops attachments paints the control **disabled**, not absent —
    /// the cross-app `capabilities.*` contract (`test_capability_gating.py`). Clients
    /// never branch on rail, so this asserts through the derived caps.
    #[test]
    fn a_rail_without_attachments_paints_the_control_disabled() {
        // The bridged rail's own vector withholds attachments (a bridge's
        // declared vector replaces it only for a registered bridge).
        for (rail, sender, expect_enabled) in [
            ("Bridged", "@alice:instance.test", false),
            ("FaunaMls", "bob-caps@self-nest.test", true),
        ] {
            let app = detail_app(&[json!({
                "rail": rail,
                "bridge_id": "example",
                "sender": sender,
                "body": "hi",
                "message_id": "m-1",
            })]);
            let attach: Vec<bool> = elements(&app)
                .into_iter()
                .filter(|e| e.id == "attachment-button")
                .map(|e| e.enabled)
                .collect();
            assert_eq!(
                attach,
                vec![expect_enabled],
                "{rail}: attachment-button must paint exactly once, enabled={expect_enabled}"
            );
            // Same gate on the topic toggle, which tui had never painted in Detail at
            // all — the absence is what made this suite's Detail assertions vacuous.
            let topic: Vec<bool> = elements(&app)
                .into_iter()
                .filter(|e| e.id == "topic-toggle-button")
                .map(|e| e.enabled)
                .collect();
            assert_eq!(
                topic,
                vec![expect_enabled],
                "{rail}: topic-toggle-button must paint in the reply bar, enabled={expect_enabled}"
            );
        }
    }

    /// A received message's attachments paint one leaf each off the shared
    /// `RenderBlock::Attachment` blocks: an image arm and a file arm, the file one
    /// naming its filename (the text the cross-app test reads).
    #[test]
    fn a_received_message_paints_image_and_file_attachment_leaves() {
        use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
        // A real 1x1 PNG so the rasterize arm actually decodes.
        let png = B64
            .decode(
                "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==",
            )
            .unwrap();
        let app = detail_app(&[json!({
            "rail": "FaunaMls",
            "sender": "bob@self-nest.test",
            "body": "see attached",
            "message_id": "m-1",
            "attachments": [
                { "filename": "pic.png", "mime_type": "image/png", "data_base64": B64.encode(&png) },
                { "filename": "notes.txt", "mime_type": "text/plain", "data_base64": B64.encode(b"hello notes\n") },
            ],
        })]);

        assert_eq!(
            count_id(&app, "dm-attachment-image"),
            1,
            "the image attachment must paint a dm-attachment-image leaf"
        );
        assert_eq!(
            count_id(&app, "dm-attachment-file"),
            1,
            "the non-image attachment must paint a dm-attachment-file leaf"
        );
        let file_text = &texts_of(&app, "dm-attachment-file")[0];
        assert!(
            file_text.contains("notes.txt"),
            "the file leaf must name its file: {file_text:?}"
        );
    }

    /// A received picture carrying content credentials paints `c2pa-badge` off
    /// its own block's verdict (`conversations.md` § Layout & flow; § Attachments
    /// "C2PA on-device"); an unsigned picture paints none.
    #[test]
    fn a_received_signed_picture_paints_its_c2pa_badge() {
        use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
        let signed = include_bytes!("../../../../tests/fixtures/c2pa-signed.png");
        let unsigned = B64
            .decode(
                "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==",
            )
            .unwrap();
        let mixed = detail_app(&[json!({
            "rail": "FaunaMls",
            "sender": "bob@self-nest.test",
            "body": "signed",
            "message_id": "m-1",
            "attachments": [
                { "filename": "signed.png", "mime_type": "image/png", "data_base64": B64.encode(signed) },
                { "filename": "plain.png", "mime_type": "image/png", "data_base64": B64.encode(&unsigned) },
            ],
        })]);
        assert_eq!(
            count_id(&mixed, "c2pa-badge"),
            1,
            "exactly the signed picture paints a badge"
        );

        let unsigned_only = detail_app(&[json!({
            "rail": "FaunaMls",
            "sender": "bob@self-nest.test",
            "body": "plain",
            "message_id": "m-1",
            "attachments": [
                { "filename": "plain.png", "mime_type": "image/png", "data_base64": B64.encode(&unsigned) },
            ],
        })]);
        assert_eq!(
            count_id(&unsigned_only, "c2pa-badge"),
            0,
            "an unsigned picture paints no badge"
        );
    }

    /// An image whose bytes this device does not hold paints the DECLARED
    /// placeholder under the same id — its filename and its size, the
    /// placeholder `conversations.md` § Attachments → *Retention* names for a
    /// handle whose bytes are gone or not yet fetched. Until 2026-09-21 the image
    /// arm painted the filename alone, so a picture the device could not fetch
    /// lost its size while a file in the same state kept it.
    #[test]
    fn an_image_without_its_bytes_paints_its_name_and_size() {
        use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
        let png = B64
            .decode(
                "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==",
            )
            .unwrap();
        let app = detail_app(&[json!({
            "rail": "FaunaMls",
            "sender": "bob@self-nest.test",
            "body": "see attached",
            "message_id": "m-1",
            "attachments": [
                { "filename": "pic.png", "mime_type": "image/png", "data_base64": B64.encode(&png) },
            ],
        })]);
        let manager = app
            .conversations
            .manager
            .clone()
            .expect("a signed-in manager");
        let thread = manager.snapshot().threads[0].thread_id.clone();
        assert_eq!(
            manager.evict_thread_attachments_for_test(thread, "pic.png".into()),
            1,
            "the picture's bytes must have been resident to evict"
        );

        let placeholder = &texts_of(&app, "dm-attachment-image")[0];
        let size = crate::wizard::localized(&fauna_core::format::byte_size(png.len() as u64));
        assert!(
            placeholder.contains("pic.png") && placeholder.contains(&size),
            "the declared placeholder must carry the filename and {size:?}: {placeholder:?}"
        );
    }

    /// `topic-toggle-button` reveals/hides `subject-input` (drives the manager's
    /// `subject_draft` Some/None), the gate every app follows.
    #[test]
    fn toggle_topic_reveals_and_hides_the_subject_input() {
        let mut app = conv_app(&[]);
        apply_local(&mut app, Action::StartNewConversation);
        assert!(!ids(&app).contains("subject-input"), "hidden by default");
        apply_local(&mut app, Action::ToggleTopic);
        assert!(ids(&app).contains("subject-input"), "shown after toggle");
        apply_local(&mut app, Action::ToggleTopic);
        assert!(
            !ids(&app).contains("subject-input"),
            "hidden after re-toggle"
        );
    }

    /// Typing into the composer body writes the manager's new-thread draft (the
    /// literal-markdown round-trip `test_compose_richeditbox_value_and_toolbar`
    /// asserts — no rich-text tree, no serialize boundary).
    #[test]
    fn composing_body_round_trips_literal_markdown() {
        let mut app = conv_app(&[]);
        apply_local(&mut app, Action::StartNewConversation);
        let src = "a *b* `c` **d** # e".to_string();
        let _ = set_field(
            &mut app.conversations,
            ConversationsField::ComposeBody,
            src.clone(),
        );
        assert_eq!(
            field(&app.conversations, &ConversationsField::ComposeBody),
            src
        );
    }

    /// The `recipient-resolve-status` element's `state` automation attribute —
    /// the value the recipient-picker suite reads via `get_attr`. Absent when the
    /// picker hasn't been touched (its label isn't painted yet).
    fn resolve_state_attr(app: &App) -> Option<String> {
        elements(app)
            .into_iter()
            .find(|e| e.id == "recipient-resolve-status")
            .and_then(|e| {
                e.attrs
                    .into_iter()
                    .find(|(k, _)| k == "state")
                    .map(|(_, v)| v)
            })
    }

    /// A `recipient-picker-input` write flips the `state` attr to a terminal
    /// value synchronously (the format-only resolve) AND hands back the async
    /// backend probe as a `PendingResolve` — the two-step linux's `on_input`
    /// does. Clearing the input returns it to `idle`.
    #[test]
    fn recipient_input_write_sets_the_state_attr_and_returns_a_pending_resolve() {
        let mut app = conv_app(&[]);
        apply_local(&mut app, Action::StartNewConversation);
        let pending = set_field(
            &mut app.conversations,
            ConversationsField::RecipientInput,
            "alice@self-nest.test".to_string(),
        );
        assert!(
            pending.is_some(),
            "a recipient-picker write implies the async resolve"
        );
        // Typing owes a probe: the sync write parks the picker on `resolving`
        // until the returned `PendingResolve` reports — never a shape-derived
        // `resolved` (`docs/goal/ui/conversations.md` § Errors & edge cases →
        // *The picker tells the truth*).
        assert_eq!(resolve_state_attr(&app).as_deref(), Some("resolving"));
        // Clearing returns to idle (the empty-input branch).
        let _ = set_field(
            &mut app.conversations,
            ConversationsField::RecipientInput,
            String::new(),
        );
        assert_eq!(resolve_state_attr(&app).as_deref(), Some("idle"));
    }

    /// Driving the returned `PendingResolve` (what the agent's type path awaits
    /// and the keyboard's spawns) settles the picker at a **terminal** resolve
    /// state — never stuck mid-`resolving` — so the suite's `_wait_resolve` poll
    /// always terminates.
    #[tokio::test]
    async fn async_resolve_settles_the_state_to_a_terminal_value() {
        let mut app = conv_app(&[]);
        apply_local(&mut app, Action::StartNewConversation);
        let pending = set_field(
            &mut app.conversations,
            ConversationsField::RecipientInput,
            "alice@self-nest.test".to_string(),
        )
        .expect("a recipient-picker write returns the async resolve");
        pending.run().await;
        let state = resolve_state_attr(&app);
        assert!(
            matches!(
                state.as_deref(),
                Some("resolved") | Some("not-found") | Some("error")
            ),
            "async resolve must reach a terminal state, got {state:?}"
        );
    }

    // ── Slice D: the conversation_detail read view ────────────────────────────

    /// An authed app whose conversations manager has `payloads` injected through
    /// the inbound seam, with the first resulting thread opened (`Mode::Detail`).
    /// The detail analogue of `conv_app`.
    fn detail_app(payloads: &[Value]) -> App {
        let mut app = authed_app();
        let manager = ConversationsManager::new();
        manager.install_mock_backends_for_test();
        // Struct-update, not a hand-listed field set: a branch that grows
        // `ConversationsState` then merges cleanly instead of colliding on the
        // grown axis — the project-wide fixture-shape convention.
        let state = ConversationsState {
            manager: Some(manager),
            ..Default::default()
        };
        for p in payloads {
            assert!(
                inject_inbound(&state, p),
                "inject_inbound should route through the mock backend"
            );
        }
        app.conversations = state;
        app.page = Page::Conversations;
        let thread_id = app.conversations.snapshot().unwrap().threads[0]
            .thread_id
            .clone();
        apply_local(&mut app, Action::SelectThread(thread_id.0.clone()));
        assert_eq!(app.conversations.mode, Mode::Detail(thread_id));
        app
    }

    /// The count of a given detail element id (the registry is the element list).
    fn count_id(app: &App, id: &str) -> usize {
        elements(app).iter().filter(|e| e.id == id).count()
    }

    /// Text of each detail element with `id`, in registration order.
    fn texts_of(app: &App, id: &str) -> Vec<String> {
        elements(app)
            .into_iter()
            .filter(|e| e.id == id)
            .map(|e| e.text)
            .collect()
    }

    /// Opening a thread renders the read view: a `thread-header` (so the driver's
    /// thread-open wait resolves) plus one `dm-message-text` + `dm-message-timestamp`
    /// per message. `test_conversations_bubble_timestamp` reads bubble index 0.
    #[test]
    fn detail_view_registers_header_and_one_bubble_per_message() {
        let app = detail_app(&[
            json!({ "rail": "FaunaMls", "sender": "carol@self-nest.test", "body": "hello there", "message_id": "m-1" }),
            json!({ "rail": "FaunaMls", "sender": "carol@self-nest.test", "body": "again", "message_id": "m-2" }),
        ]);
        let got = ids(&app);
        for required in ["thread-header", "dm-message-text", "dm-message-timestamp"] {
            assert!(got.contains(required), "detail must register {required}");
        }
        assert_eq!(
            count_id(&app, "dm-message-text"),
            2,
            "one bubble per message"
        );
        assert_eq!(count_id(&app, "dm-message-timestamp"), 2);
        // A just-injected message buckets as "today" → a local HH:MM clock.
        let ts = &texts_of(&app, "dm-message-timestamp")[0];
        assert!(
            ts.len() == 5 && ts.as_bytes()[2] == b':',
            "today timestamp reads as HH:MM, got {ts:?}"
        );
    }

    // ── The selected message (SearchNav::Mail's second half) ──────────────

    /// The `selected` attribute on every `dm-message-timestamp`, in message
    /// order — the cross-app observable (`ui.yaml` `dm-message-bubble`).
    fn selected_flags(app: &App) -> Vec<String> {
        app.page_elements()
            .into_iter()
            .filter(|e| e.id == "dm-message-timestamp")
            .map(|e| {
                e.attrs
                    .iter()
                    .find(|(k, _)| k == "selected")
                    .map(|(_, v)| v.clone())
                    .unwrap_or_else(|| "<absent>".into())
            })
            .collect()
    }

    /// The contract: the named message is marked and the others are not. Reads
    /// the attribute on every bubble, not just the selected one — an assertion
    /// that only checks the hit cannot tell a working marker from one that
    /// marks the whole thread.
    #[test]
    fn selecting_a_message_marks_that_bubble_and_no_other() {
        let mut app = detail_app(&[
            json!({ "rail": "FaunaMls", "sender": "carol@self-nest.test", "body": "first", "message_id": "m-1" }),
            json!({ "rail": "FaunaMls", "sender": "carol@self-nest.test", "body": "second", "message_id": "m-2" }),
            json!({ "rail": "FaunaMls", "sender": "carol@self-nest.test", "body": "third", "message_id": "m-3" }),
        ]);
        let thread_id = app.conversations.snapshot().unwrap().threads[0]
            .thread_id
            .clone();

        apply_local(
            &mut app,
            Action::SelectThreadAndMessage {
                thread_id: thread_id.0.clone(),
                message_id: "m-2".into(),
            },
        );

        assert_eq!(
            selected_flags(&app),
            vec!["false", "true", "false"],
            "exactly the named bubble carries selected=true"
        );
    }

    /// The attribute is present on every bubble even with nothing selected, so a
    /// test can tell "not selected" from "this app never surfaced the attribute"
    /// (`testing.md` point 6). A plain thread-open is the no-selection case.
    #[test]
    fn every_bubble_carries_the_selected_attribute_when_nothing_is_selected() {
        let app = detail_app(&[
            json!({ "rail": "FaunaMls", "sender": "carol@self-nest.test", "body": "first", "message_id": "m-1" }),
            json!({ "rail": "FaunaMls", "sender": "carol@self-nest.test", "body": "second", "message_id": "m-2" }),
        ]);
        assert_eq!(selected_flags(&app), vec!["false", "false"]);
    }

    /// The marker survives the arms that paint no body. A muted-collapsed
    /// message registers no `dm-message-text`, so hanging the observable there
    /// would lose exactly the search hit that most needs pointing at; the
    /// timestamp is painted by every arm, which is why it carries it.
    #[test]
    fn a_muted_collapsed_message_can_still_be_the_selected_one() {
        let mut app = detail_app(&[
            json!({ "rail": "FaunaMls", "sender": "carol@self-nest.test", "body": "hey, lunch tomorrow?", "message_id": "m-clean" }),
            json!({ "rail": "FaunaMls", "sender": "carol@self-nest.test", "body": "you won the LOTTERY jackpot", "message_id": "m-muted" }),
        ]);
        app.settings.muted_words.snapshot.keywords = vec!["lottery".into()];
        let thread_id = app.conversations.snapshot().unwrap().threads[0]
            .thread_id
            .clone();

        apply_local(
            &mut app,
            Action::SelectThreadAndMessage {
                thread_id: thread_id.0.clone(),
                message_id: "m-muted".into(),
            },
        );

        assert_eq!(
            count_id(&app, "dm-message-text"),
            1,
            "the muted bubble still paints no body"
        );
        assert_eq!(
            selected_flags(&app),
            vec!["false", "true"],
            "and is still markable"
        );
    }

    /// A selection the thread cannot place marks nothing — the manager's
    /// read-time resolve, seen from the page.
    #[test]
    fn selecting_a_message_this_thread_lacks_marks_nothing() {
        let mut app = detail_app(&[
            json!({ "rail": "FaunaMls", "sender": "carol@self-nest.test", "body": "first", "message_id": "m-1" }),
        ]);
        let thread_id = app.conversations.snapshot().unwrap().threads[0]
            .thread_id
            .clone();

        apply_local(
            &mut app,
            Action::SelectThreadAndMessage {
                thread_id: thread_id.0.clone(),
                message_id: "m-nowhere".into(),
            },
        );

        assert_eq!(selected_flags(&app), vec!["false"]);
    }

    // ── The muted-keyword collapse (content-moderation-and-ranking.md § Q3) ──

    /// A bubble whose decrypted body matches the user's sealed list collapses
    /// behind `dm-message-muted` + its reveal button, and — the load-bearing
    /// half — **stops registering `dm-message-text`**, which is how the shared
    /// `count("dm-message-text")` read excludes it on every app. A
    /// non-matching bubble in the same thread is untouched.
    #[test]
    fn a_matching_bubble_collapses_and_a_clean_one_does_not() {
        let mut app = detail_app(&[
            json!({ "rail": "FaunaMls", "sender": "carol@self-nest.test", "body": "hey, lunch tomorrow?", "message_id": "m-clean" }),
            json!({ "rail": "FaunaMls", "sender": "carol@self-nest.test", "body": "you won the LOTTERY jackpot", "message_id": "m-muted" }),
        ]);
        // Nothing muted yet — both bubbles render their bodies.
        assert_eq!(count_id(&app, "dm-message-text"), 2);
        assert_eq!(count_id(&app, "dm-message-muted"), 0);

        app.settings.muted_words.snapshot.keywords = vec!["lottery".into()];
        assert_eq!(
            count_id(&app, "dm-message-muted"),
            1,
            "exactly the matching bubble collapses"
        );
        assert_eq!(
            count_id(&app, "dm-message-text"),
            1,
            "the collapsed bubble must not register its body"
        );
        assert_eq!(
            count_id(&app, "dm-message-muted-reveal-button"),
            1,
            "a collapsed bubble carries its reveal button"
        );
        // The collapse never leaks the body — not even through the placeholder.
        assert!(
            !texts_of(&app, "dm-message-muted")[0]
                .to_lowercase()
                .contains("lottery")
        );
        // A collapsed bubble keeps its timestamp (the `dm-message-deleted` shape),
        // so the thread still reads as two messages.
        assert_eq!(count_id(&app, "dm-message-timestamp"), 2);
    }

    /// The match is the SHARED `fauna_core::keyword::body_excludes_matches`, so
    /// tui inherits its case-insensitive substring semantics rather than forking
    /// a second matcher — the whole reason that module exists.
    #[test]
    fn the_collapse_uses_the_shared_case_insensitive_substring_match() {
        let mut app = detail_app(&[
            json!({ "rail": "FaunaMls", "sender": "carol@self-nest.test", "body": "Congratulations — the LOTTERY jackpot!", "message_id": "m-1" }),
        ]);
        // Lower-case term vs. upper-case body, and a substring of a longer word.
        app.settings.muted_words.snapshot.keywords = vec!["lott".into()];
        assert_eq!(count_id(&app, "dm-message-muted"), 1);
        // A term that does not occur leaves the bubble alone.
        app.settings.muted_words.snapshot.keywords = vec!["sourdough".into()];
        assert_eq!(count_id(&app, "dm-message-muted"), 0);
        // An empty list is not a rule (it must not substring-match everything).
        app.settings.muted_words.snapshot.keywords = vec![];
        assert_eq!(count_id(&app, "dm-message-muted"), 0);
    }

    /// Revealing is session-local and per-message: the body comes back, the
    /// placeholder goes, and the sealed list is untouched (a reveal must never
    /// un-mute the term — `moderation.md` § Muted keywords).
    #[test]
    fn revealing_one_bubble_restores_its_body_without_unmuting_the_term() {
        let mut app = detail_app(&[
            json!({ "rail": "FaunaMls", "sender": "carol@self-nest.test", "body": "you won the LOTTERY jackpot", "message_id": "m-muted" }),
        ]);
        app.settings.muted_words.snapshot.keywords = vec!["lottery".into()];
        assert_eq!(count_id(&app, "dm-message-text"), 0);

        apply_local(
            &mut app,
            Action::RevealMuted {
                msg_id: MessageId("m-muted".to_string()),
            },
        );

        assert_eq!(count_id(&app, "dm-message-muted"), 0, "placeholder is gone");
        assert_eq!(count_id(&app, "dm-message-text"), 1, "the body is back");
        assert_eq!(
            app.settings.muted_words.snapshot.terms(),
            vec!["lottery".to_string()],
            "the term stays muted — the reveal is one instance, not an un-mute"
        );
    }

    /// A **deleted** message that also matches a muted term stays a tombstone:
    /// the deleted arm runs first, so a takedown is never re-labelled as "you
    /// muted a word" (linux's ordering), and the reveal button — which would
    /// offer to un-hide a deleted body — never paints.
    #[tokio::test]
    async fn a_deleted_message_stays_a_tombstone_even_when_it_matches() {
        let mut app = detail_app(&[json!({
            "rail": "FaunaMls", "sender": "carol@self-nest.test",
            "body": "you won the LOTTERY jackpot", "message_id": "m-1", "is_own": true,
        })]);
        app.settings.muted_words.snapshot.keywords = vec!["lottery".into()];
        assert_eq!(
            count_id(&app, "dm-message-muted"),
            1,
            "collapsed while live"
        );

        apply_local(
            &mut app,
            Action::OpenMessageActions {
                msg_id: MessageId("m-1".to_string()),
            },
        );
        apply_local(&mut app, Action::StartDeleteMessage);
        let op = apply_local(&mut app, Action::ConfirmDeleteMessage)
            .expect("confirm returns the network half");
        assert!(matches!(op.run().await, Outcome::Done));

        assert_eq!(count_id(&app, "dm-message-deleted"), 1, "tombstone wins");
        assert_eq!(count_id(&app, "dm-message-muted"), 0, "not re-labelled");
        assert_eq!(count_id(&app, "dm-message-muted-reveal-button"), 0);
    }

    /// A legally-withheld envelope collapses the bubble to the shared tombstone
    /// + its timestamp — never a blank or failed-decrypt bubble
    /// (`moderation.md` § Legal takedown). Like the `deleted` arm, the body is
    /// absent, which is what keeps `count("dm-message-text")` honest.
    #[tokio::test]
    async fn a_legally_taken_down_message_collapses_to_the_shared_tombstone() {
        let app = detail_app(&[json!({
            "rail": "FaunaMls", "sender": "carol@self-nest.test",
            "body": "", "message_id": "m-1",
            "legal_takedown_ref": "DMCA-2026-0001",
        })]);
        assert_eq!(
            count_id(&app, "dm-message-text"),
            0,
            "the withheld body never paints"
        );
        assert_eq!(count_id(&app, "dm-message-timestamp"), 1);
        assert!(
            elements(&app).iter().any(|e| e.text
                == fauna_i18n::strings::moderation::legal_takedown::tombstone("DMCA-2026-0001")),
            "the shared localized tombstone paints in place of the body",
        );
    }

    /// The bubble's `content-label-badge` reads `MessageSnapshot.labels` — the
    /// field `observe_local_detection` fills at post-decrypt — NOT the never-set
    /// `badges.content_warning` (the dead-field bug linux, android and apple
    /// each had to repoint). An unlabelled message paints no badge.
    ///
    /// ⚠ The badge is deliberately NOT asserted here off a spammy body: the
    /// classify hook runs in `ingest_inbound_to_thread` (the known-thread
    /// append), while this inject seam goes through `ingest_inbound` (the
    /// thread-CREATING path) which does not classify — correct per
    /// `moderation.md`, whose local-detection signal is the MLS rail, not the
    /// mail-style first-contact keying this seam performs. The positive arm is
    /// proven where it actually runs: the shared badge builder is unit-tested in
    /// `crate::moderation`, and the live post-decrypt path end-to-end by
    /// `test_moderation_local_detection.py`'s two-real-tui-engine leg.
    #[tokio::test]
    async fn an_unlabelled_message_paints_no_content_label_badge() {
        let app = detail_app(&[json!({
            "rail": "FaunaMls", "sender": "carol@self-nest.test",
            "body": "lunch at one?", "message_id": "m-1",
        })]);
        assert_eq!(count_id(&app, "content-label-badge"), 0);
        assert_eq!(
            count_id(&app, "dm-message-text"),
            1,
            "an ordinary bubble still paints its body"
        );
    }

    // ── Content-policy render enforcement (family-safety.md § Content policy) ──

    /// A spam-labeled inbound bubble, over the inject seam's `labels` field.
    /// 900‰ is well over the shared guardian trigger, so the floor bites.
    fn flagged_detail_app(body: &str) -> App {
        detail_app(&[json!({
            "rail": "FaunaMls", "sender": "carol@self-nest.test",
            "body": body, "message_id": "m-1",
            "labels": [{ "category": "spam", "confidence_per_mille": 900 }],
        })])
    }

    /// A guardian `block` floor collapses the bubble to
    /// `content-policy-blocked-notice` + its timestamp — no body, so
    /// `count("dm-message-text")` excludes it, and nothing leaks the text.
    #[tokio::test]
    async fn a_guardian_block_floor_collapses_the_bubble_and_leaks_nothing() {
        let app = flagged_detail_app("buy zzcheappills now");
        app.content_policy
            .set_ward_content_policy(Some(fauna_core::obligation::ContentPolicy {
                spam: fauna_core::obligation::ContentFloor::Block,
                ..Default::default()
            }));
        assert_eq!(count_id(&app, "content-policy-blocked-notice"), 1);
        assert_eq!(
            count_id(&app, "dm-message-text"),
            0,
            "the blocked body never paints"
        );
        assert_eq!(count_id(&app, "dm-message-timestamp"), 1);
        for el in elements(&app) {
            assert!(
                !el.text.contains("zzcheappills"),
                "element {:?} leaked the blocked body: {:?}",
                el.id,
                el.text
            );
        }
    }

    /// **Block beats mute**, the conversations twin of the feed's ordering test:
    /// a bubble that is both muted (revealable) and blocked renders blocked, so
    /// revealing the mute cannot walk it past the guardian's floor.
    #[tokio::test]
    async fn a_message_that_is_both_muted_and_blocked_renders_blocked() {
        let mut app = flagged_detail_app("you won the LOTTERY jackpot");
        app.settings.muted_words.snapshot.keywords = vec!["lottery".into()];
        assert_eq!(
            count_id(&app, "dm-message-muted"),
            1,
            "muted while no floor is set"
        );
        app.content_policy
            .set_ward_content_policy(Some(fauna_core::obligation::ContentPolicy {
                spam: fauna_core::obligation::ContentFloor::Block,
                ..Default::default()
            }));
        assert_eq!(count_id(&app, "content-policy-blocked-notice"), 1);
        assert_eq!(
            count_id(&app, "dm-message-muted"),
            0,
            "the muted arm — whose reveal would bypass the block — never runs"
        );
        assert_eq!(count_id(&app, "dm-message-muted-reveal-button"), 0);
    }

    /// A `collapse` floor is revealable, session-locally, and the reveal is a
    /// keyboard-reachable untagged control (ui.yaml scopes no id to this arm —
    /// linux and web made the same call, but a TUI has no mouse, so an
    /// unfocusable one would be a dead affordance).
    #[tokio::test]
    async fn a_collapse_floor_reveals_in_place_without_relaxing_the_policy() {
        let mut app = flagged_detail_app("buy zzcheappills now");
        app.content_policy
            .set_ward_content_policy(Some(fauna_core::obligation::ContentPolicy {
                spam: fauna_core::obligation::ContentFloor::Collapse,
                ..Default::default()
            }));
        assert_eq!(
            count_id(&app, "dm-message-text"),
            0,
            "the collapsed body does not paint"
        );
        let reveal = elements(&app)
            .into_iter()
            .find(|e| {
                matches!(e.role, crate::element::Role::Button(_))
                    && e.text == fauna_i18n::strings::family::CONTENT_REVEAL_BUTTON
            })
            .expect("a collapse offers a reveal");
        assert!(
            reveal.focusable(),
            "the reveal must be reachable by keyboard"
        );
        assert!(
            reveal.id.is_empty(),
            "and it must not mint a app-specific id"
        );

        apply_local(
            &mut app,
            Action::RevealContent {
                msg_id: MessageId("m-1".to_string()),
            },
        );
        assert_eq!(count_id(&app, "dm-message-text"), 1, "the body is back");
        // The floor itself is untouched — revealing one bubble is not relaxing
        // the guardian's policy.
        assert_eq!(
            app.content_policy
                .verdict_for(&[fauna_core::content_category::ContentLabelEntry {
                    category: "spam".into(),
                    confidence_per_mille: 900,
                }])
                .verdict,
            fauna_core::obligation::RenderVerdict::Collapse,
            "the policy still says collapse; only this instance was revealed"
        );
    }

    /// `dm-sender` is a *bubble* element (ui.yaml: indexed), one per message,
    /// matching linux/web/windows/apple/android. The inject seam stamps an
    /// empty `sender_display` (contact-name resolution is a real-session-only
    /// concern), so the label falls back to `TypedAddress::display(sender)` —
    /// same fallback linux/apple/web all apply (`conversations.md` § Where
    /// logic lives). The reply keys onto the parent's thread by
    /// `in_reply_to` (`keying.rs::key_for_inbound` — `ByMessageReference`
    /// resolves via `thread_for_message`, independent of participants), so
    /// both messages land in one thread despite different senders — a real
    /// two-participant exchange, not a fixture quirk.
    #[test]
    fn detail_view_registers_dm_sender_per_bubble() {
        let app = detail_app(&[
            json!({ "rail": "FaunaMls", "sender": "carol@self-nest.test", "body": "hello there", "message_id": "m-1" }),
            json!({ "rail": "FaunaMls", "sender": "dan@self-nest.test", "body": "hi back", "message_id": "m-2", "in_reply_to": "m-1" }),
        ]);
        assert_eq!(count_id(&app, "dm-sender"), 2, "one dm-sender per bubble");
        let senders = texts_of(&app, "dm-sender");
        assert!(
            senders[0].contains("carol"),
            "bubble 0 should identify carol, got {:?}",
            senders[0]
        );
        assert!(
            senders[1].contains("dan"),
            "bubble 1 should identify dan, got {:?}",
            senders[1]
        );
    }

    /// Crypto badges paint per set flag and ONLY then. The inject seam stamps
    /// `MessageBadges::default()` (all-false) — real crypto is what sets them —
    /// so the flagged message here goes through the manager seam directly, the
    /// state a real-session decrypt produces (`conversations.md` § Element IDs).
    #[test]
    fn crypto_badges_paint_per_flag_and_never_on_default_messages() {
        use fauna_conversations::Rail;
        use fauna_conversations::backend::RailInboundMessage;
        use fauna_conversations::message::{BodyFormat, MessageBadges};

        // Message 1 (inject seam): default badges → no badge elements.
        let mut app = detail_app(&[json!({
            "rail": "FaunaMls", "sender": "carol@self-nest.test",
            "body": "plain", "message_id": "m-1",
        })]);
        for badge in ["encrypted-badge", "signed-badge", "verified-badge"] {
            assert_eq!(
                count_id(&app, badge),
                0,
                "{badge} must not paint by default"
            );
        }

        // Message 2 (manager seam, flagged): all three paint, on the same thread.
        let manager = app.conversations.manager.clone().unwrap();
        manager
            .inject_inbound_for_test(RailInboundMessage {
                rail: Rail::FaunaMls,
                sender: TypedAddress::Fauna {
                    handle: "carol@self-nest.test".to_string(),
                    actor_id: fauna_core::identity::ActorId([7u8; 32]),
                },
                recipients: vec![TypedAddress::Fauna {
                    handle: "me@self-nest.test".to_string(),
                    actor_id: fauna_core::identity::ActorId([8u8; 32]),
                }],
                subject: None,
                body: "sealed".to_string(),
                body_format: BodyFormat::Markdown,
                timestamp_ms: now_ms(),
                message_id: MessageId("m-2".to_string()),
                in_reply_to: None,
                attachments: Vec::new(),
                badges: MessageBadges {
                    encrypted: true,
                    signed: true,
                    verified: true,
                    ..Default::default()
                },
                legal_takedown_ref: None,
                // This fixture is about crypto badges, not the plane.
                plane_ref: None,
            })
            .expect("flagged inject routes");
        // The flagged message may have keyed a second thread (address nuance is
        // irrelevant here) — open whichever thread carries it.
        let threads = app.conversations.snapshot().unwrap().threads;
        let tid = threads
            .iter()
            .find(|t| t.snippet.contains("sealed"))
            .expect("flagged message landed")
            .thread_id
            .clone();
        apply_local(&mut app, Action::SelectThread(tid.0.clone()));
        for badge in ["encrypted-badge", "signed-badge", "verified-badge"] {
            assert_eq!(
                count_id(&app, badge),
                1,
                "{badge} must paint when its flag is set"
            );
        }
    }

    /// The per-bubble ⋯ overflow gates on the shared capability matrix exactly
    /// as the GUI apps do: present on a FaunaMls bubble (`supports_reactions`),
    /// absent on an SMTP one (neither reactions nor delete —
    /// `test_capability_gate_no_button_on_smtp_thread`'s shape).
    #[test]
    fn the_actions_button_gates_on_capabilities() {
        // FaunaMls received: reactions (+ mark-as-spam) → the ⋯ paints.
        let fauna = detail_app(&[json!({
            "rail": "FaunaMls", "sender": "carol@self-nest.test",
            "body": "hi", "message_id": "m-1",
        })]);
        assert_eq!(count_id(&fauna, "dm-message-actions-button"), 1);

        // SMTP received: no reactions, no delete — but mark-as-spam is
        // rail-**independent** on any received (`!is_own`) message, so the ⋯
        // still paints for it (android/windows parity; `mail-spam.md`).
        let smtp_recv = detail_app(&[json!({
            "rail": "Smtp", "sender": "carol@host.test", "subject": "Q4",
            "body": "hi", "message_id": "m-1",
        })]);
        assert_eq!(
            count_id(&smtp_recv, "dm-message-actions-button"),
            1,
            "a received message always offers mark-as-spam, so the ⋯ paints"
        );

        // SMTP own: no reactions, no delete, and mark-as-spam is received-only —
        // so the ⋯ has nothing to offer and must not paint.
        let smtp_own = detail_app(&[json!({
            "rail": "Smtp", "sender": "carol@host.test", "subject": "Q4",
            "body": "mine", "message_id": "m-1", "is_own": true,
        })]);
        assert_eq!(
            count_id(&smtp_own, "dm-message-actions-button"),
            0,
            "own SMTP has no action (no reactions/delete, spam is received-only)"
        );
    }

    /// Opening the ⋯ menu paints the six shared quick-set options (the fixed
    /// order every app offers) + the "more" free-entry affordance; the
    /// sender-only delete entry stays absent on a received message. Esc closes
    /// the overlay before it would leave the thread.
    #[test]
    fn the_actions_menu_offers_the_shared_quickset_and_esc_closes_it() {
        let mut app = detail_app(&[json!({
            "rail": "FaunaMls", "sender": "carol@self-nest.test",
            "body": "hi", "message_id": "m-1",
        })]);
        apply_local(
            &mut app,
            Action::OpenMessageActions {
                msg_id: MessageId("m-1".to_string()),
            },
        );
        assert_eq!(
            texts_of(&app, "dm-reaction-option"),
            fauna_conversations::QUICKSET_EMOJIS
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>(),
            "the quick-set is the shared fixed order"
        );
        assert_eq!(count_id(&app, "dm-reaction-more-button"), 1);
        assert_eq!(
            count_id(&app, "dm-message-delete-button"),
            0,
            "delete is sender-only; this message is not own"
        );

        assert!(detail_overlay_open(&app.conversations));
        cancel_detail_overlay(&mut app.conversations);
        assert_eq!(
            count_id(&app, "dm-reaction-option"),
            0,
            "Esc closes the menu"
        );
    }

    /// The sender-only delete two-step: the menu's delete entry reveals the
    /// destructive confirm, whose [`Op`] posts the cooperative tombstone —
    /// after which the bubble repaints as the `dm-message-deleted` placeholder
    /// with body and actions hidden (ui.yaml `dm-message-bubble`).
    #[tokio::test]
    async fn the_delete_two_step_tombstones_an_own_message() {
        let mut app = detail_app(&[json!({
            "rail": "FaunaMls", "sender": "carol@self-nest.test",
            "body": "mine", "message_id": "m-1", "is_own": true,
        })]);
        assert_eq!(count_id(&app, "dm-message-actions-button"), 1);

        apply_local(
            &mut app,
            Action::OpenMessageActions {
                msg_id: MessageId("m-1".to_string()),
            },
        );
        assert_eq!(
            count_id(&app, "dm-message-delete-button"),
            1,
            "own → deletable"
        );
        apply_local(&mut app, Action::StartDeleteMessage);
        assert_eq!(count_id(&app, "dm-message-delete-confirm-button"), 1);

        let op = apply_local(&mut app, Action::ConfirmDeleteMessage)
            .expect("confirm returns the network half");
        assert!(
            matches!(op.run().await, Outcome::Done),
            "delete reports no user error"
        );

        assert_eq!(count_id(&app, "dm-message-deleted"), 1, "tombstone paints");
        assert_eq!(count_id(&app, "dm-message-text"), 0, "body hidden");
        assert_eq!(
            count_id(&app, "dm-message-actions-button"),
            0,
            "actions hidden"
        );
    }

    /// The ⋯ menu offers `dm-message-mark-as-spam-button` on a received
    /// (`!is_own`) message and hides it on an own one — the received-only train
    /// gesture (`mail-spam.md` § Training signal sources 1, gated `!is_own`).
    #[test]
    fn the_actions_menu_offers_mark_as_spam_only_on_a_received_message() {
        let mut recv = detail_app(&[json!({
            "rail": "FaunaMls", "sender": "carol@self-nest.test",
            "body": "spam?", "message_id": "m-1",
        })]);
        apply_local(
            &mut recv,
            Action::OpenMessageActions {
                msg_id: MessageId("m-1".to_string()),
            },
        );
        assert_eq!(
            count_id(&recv, "dm-message-mark-as-spam-button"),
            1,
            "received → offered"
        );

        let mut own = detail_app(&[json!({
            "rail": "FaunaMls", "sender": "carol@self-nest.test",
            "body": "mine", "message_id": "m-1", "is_own": true,
        })]);
        apply_local(
            &mut own,
            Action::OpenMessageActions {
                msg_id: MessageId("m-1".to_string()),
            },
        );
        assert_eq!(
            count_id(&own, "dm-message-mark-as-spam-button"),
            0,
            "own → never offered (the received-only gate)"
        );
    }

    /// Marking a received message as spam closes the menu and hands back the
    /// sealed-train [`Op`] carrying the message's body, subject and opaque id.
    /// Without a mail machine (mail not enabled) the dispatch is a **silent
    /// no-op** that still closes the menu — the dropped-command shape is
    /// deliberate here (there is no server-train fallback for a conversation
    /// message; `mail-spam.md`), not a swallowed command.
    #[test]
    fn mark_as_spam_dispatches_the_sealed_train_and_closes_the_menu() {
        let mut app = detail_app(&[json!({
            "rail": "FaunaMls", "sender": "carol@self-nest.test",
            "body": "BUY NOW", "message_id": "m-1",
        })]);

        // No machine yet → silent no-op, but the overlay still closes.
        apply_local(
            &mut app,
            Action::OpenMessageActions {
                msg_id: MessageId("m-1".to_string()),
            },
        );
        assert!(app.conversations.actions_overlay.is_some());
        assert!(
            apply_local(
                &mut app,
                Action::MarkMessageSpam {
                    msg_id: MessageId("m-1".to_string()),
                }
            )
            .is_none(),
            "no mail machine → silent no-op"
        );
        assert!(
            app.conversations.actions_overlay.is_none(),
            "the menu closes regardless of the no-op"
        );

        // With a machine present, the dispatch carries the body/subject/opaque id.
        let nest = fauna_client::NestClient::new(
            "http://127.0.0.1:1".to_string(),
            fauna_core::identity::ActorKeypair::generate(),
        );
        let machine = crate::mail_glue::build_mail_settings_machine(
            nest,
            &"11".repeat(32),
            std::sync::Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
            "http://127.0.0.1:1",
            Arc::new(fauna_client_config::NoLedgerStore),
            None,
        )
        .expect("build the mail-settings machine");
        app.settings.set_mail_machine_for_test(Arc::new(machine));

        apply_local(
            &mut app,
            Action::OpenMessageActions {
                msg_id: MessageId("m-1".to_string()),
            },
        );
        let op = apply_local(
            &mut app,
            Action::MarkMessageSpam {
                msg_id: MessageId("m-1".to_string()),
            },
        )
        .expect("received message + machine → the sealed-train op");
        match &op {
            Op::MarkMessageSpam {
                message_id,
                body,
                subject,
                ..
            } => {
                assert_eq!(
                    message_id, b"m-1",
                    "the opaque row reference is the message id's UTF-8 bytes"
                );
                assert_eq!(body, "BUY NOW");
                // A plain received conversation message carries no per-message
                // `subject_line` (the manager sets that only on an actual subject
                // *change*), so the raw subject is empty and the shared façade
                // derives a body snippet — android parity (`msg.subjectLine ?: ""`).
                assert_eq!(subject, "", "no subject change → empty, façade snippets it");
            }
            _ => panic!("expected an Op::MarkMessageSpam"),
        }
        assert!(
            app.conversations.actions_overlay.is_none(),
            "the menu closes on dispatch"
        );
    }

    /// The fuller picker's free-entry prompt is a **committing** input, not a
    /// plain one.
    ///
    /// The distinction is the whole reason the outcome is witnessable here at
    /// all: a `Role::Input` registers no action, so the automation surface's
    /// `click` answers *not actuable* and its `press_key` is a no-op — leaving
    /// Enter, which only a human at a terminal can press. `Role::InputCommit`
    /// keeps that Enter (through `actuate_focused`'s generic arm) and adds the
    /// type-then-click idiom the shared action layer drives a commit-on-Enter
    /// entry with (`actions/conversations.py::react_with_custom_emoji`).
    /// Demoting it back to `Element::input` would leave
    /// `docs/features/reactions-and-message-delete.md` outcome 5 green-looking
    /// nowhere, so it is pinned rather than left to the e2e alone.
    #[test]
    fn the_fuller_pickers_prompt_is_a_committing_input() {
        let mut app = detail_app(&[json!({
            "rail": "FaunaMls", "sender": "carol@self-nest.test",
            "body": "react to me", "message_id": "m-1",
        })]);
        apply_local(
            &mut app,
            Action::OpenMessageActions {
                msg_id: MessageId("m-1".to_string()),
            },
        );
        // In the menu step the id is the plain "more" button.
        let more = elements(&app)
            .into_iter()
            .find(|e| e.id == ids::DM_REACTION_MORE_BUTTON)
            .expect("the menu paints the fuller-picker button");
        assert!(
            matches!(
                more.role,
                crate::element::Role::Button(crate::element::Gesture::Conversations(
                    Action::StartReactionEntry
                ))
            ),
            "the menu step's more-button opens the prompt: {:?}",
            more.role
        );

        apply_local(&mut app, Action::StartReactionEntry);
        let prompt = elements(&app)
            .into_iter()
            .find(|e| e.id == ids::DM_REACTION_MORE_BUTTON)
            .expect("the prompt re-uses the same id as its input");
        match prompt.role {
            crate::element::Role::InputCommit { field, gesture } => {
                assert!(
                    matches!(
                        field,
                        crate::element::Field::Conversations(ConversationsField::ReactionEmoji)
                    ),
                    "typing writes the emoji draft: {field:?}"
                );
                assert!(
                    matches!(
                        gesture,
                        crate::element::Gesture::Conversations(Action::CommitReactionEntry)
                    ),
                    "and the commit toggles the typed emoji: {gesture:?}"
                );
            }
            other => {
                panic!("the prompt must be a committing input or nothing can drive it: {other:?}")
            }
        }
    }

    // ── The T1 body-rendered observation reporter (`crate::observation`) ──────

    const OBS_CHANNEL: &str = "3333333333333333333333333333333333333333333333333333333333333333";

    /// One injected inbound carrying a plane record, so the bubble it paints is
    /// reportable — the synthetic stand-in for a record `poll_inbound_conv`
    /// walked in. The `envelope` is faked; the DERIVATION is the production one.
    fn obs_payload(seq: i64, body: &str) -> Value {
        json!({
            "rail": "FaunaMls",
            "sender": "carol@self-nest.test",
            "body": body,
            "message_id": format!("conv:{OBS_CHANNEL}:{seq}"),
            "plane_record": { "channel_id": OBS_CHANNEL, "seq": seq, "envelope": body },
        })
    }

    /// What the shared intake would be told for the message carrying `body` on
    /// this channel. Keyed on the body alone: the record's plane identity is
    /// the content hash of its envelope, not its seq (the 2026-08-17
    /// record-identity cutover), and every fixture body here is distinct.
    fn obs_of(body: &str) -> fauna_sync_engine::observation_intake::Observation {
        let r =
            fauna_conversations::plane::plane_ref(OBS_CHANNEL, body.as_bytes()).expect("plane ref");
        fauna_sync_engine::observation_intake::Observation::parse(&r.scope, &r.record_digest)
            .expect("parse")
    }

    /// Render `app` at a terminal `height` rows tall through the production
    /// path — the same `terminal.draw` -> `RowHit`s the event loop keeps — and
    /// return exactly what the reporter would hand the intake.
    fn reported_at_height(
        app: &App,
        height: u16,
    ) -> Vec<fauna_sync_engine::observation_intake::Observation> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, height)).unwrap();
        let mut hits = Vec::new();
        terminal
            .draw(|frame| {
                let frame_hits = crate::ui::render(frame, app).hits;
                hits = frame_hits;
            })
            .unwrap();
        crate::observation::observed_this_frame(app, &hits)
    }

    /// **The red-verify this row turns on** (`account-data-plane.md` § The
    /// replica boundary -> T1): a body the viewport did not paint is NOT
    /// reported, even though the page registered it and every id assertion
    /// would find it.
    ///
    /// That gap is the whole hazard. tui's element list *is* its registry and
    /// the viewport clips paint only, so a thread of many messages registers
    /// every body while a short terminal shows a few — reporting from the
    /// element list would credit the account with having read messages that
    /// never touched the screen, permanently (the seen-set is grow-only).
    ///
    /// Red-verified by pointing the reporter at `app.page_elements()` instead
    /// of the frame's hit regions: it then reports all twelve and this fails.
    #[test]
    fn only_the_bubbles_this_frame_painted_are_reported() {
        let payloads: Vec<Value> = (1..=12)
            .map(|i| obs_payload(i, &format!("body {i}")))
            .collect();
        let app = detail_app(&payloads);

        // The registry holds every body — the precondition that makes the
        // assertion below discriminating rather than vacuous.
        assert_eq!(
            count_id(&app, "dm-message-text"),
            12,
            "the page registers every message; the viewport is what clips",
        );

        let reported = reported_at_height(&app, 24);
        assert!(
            !reported.is_empty(),
            "a painted bubble must be reported — otherwise this test proves nothing",
        );
        assert!(
            reported.len() < 12,
            "12 bubbles cannot fit a 24-row terminal, so a scrolled-off body must \
             be missing from the report; got all {}",
            reported.len(),
        );
        // And specifically: the last message, which the focus ring has not
        // scrolled to, is off screen and unreported.
        assert!(
            !reported.contains(&obs_of("body 12")),
            "an off-screen body was reported as displayed",
        );

        // A terminal tall enough to paint them all reports them all — the same
        // app, the same registry, a different viewport. This is what proves the
        // filter is the VIEWPORT and not some unrelated property of the tail.
        let all = reported_at_height(&app, 80);
        for i in 1..=12 {
            assert!(
                all.contains(&obs_of(&format!("body {i}"))),
                "body {i} painted on a tall terminal but was not reported",
            );
        }
    }

    /// A body that wraps to several rows is one observation, not one per line:
    /// the hit map carries a row per painted line, and the reporter dedups.
    #[test]
    fn a_body_spanning_several_rows_is_reported_once() {
        let long = "wrap ".repeat(60);
        let app = detail_app(&[obs_payload(1, &long)]);
        let reported = reported_at_height(&app, 40);
        assert_eq!(reported, vec![obs_of(&long)]);
    }

    /// A bubble whose body is **suppressed** reports nothing, however plainly
    /// it is on screen — there is no body handed to a visible view. Each arm
    /// registers no `dm-message-text`, which is exactly why the observation
    /// rides that element and needs no second list of conditions.
    #[test]
    fn a_suppressed_body_is_never_reported_though_the_bubble_is_on_screen() {
        // Legal takedown: the nest withheld the envelope, so production carries
        // no plane ref at all — asserted here as the seam's own behaviour.
        let withheld = json!({
            "rail": "FaunaMls",
            "sender": "carol@self-nest.test",
            "body": "",
            "message_id": format!("conv:{OBS_CHANNEL}:1"),
            "legal_takedown_ref": "DMCA-1",
        });
        let app = detail_app(&[withheld]);
        assert_eq!(count_id(&app, "dm-message-text"), 0, "no body element");
        assert!(reported_at_height(&app, 40).is_empty());

        // Muted keyword: the body IS on the plane and the bubble IS painted,
        // but collapsed — so the body element (and with it the observation)
        // never registers.
        let mut app = detail_app(&[obs_payload(1, "the quarterly badword report")]);
        assert_eq!(
            reported_at_height(&app, 40),
            vec![obs_of("the quarterly badword report")],
            "un-muted, it reports",
        );
        app.settings.muted_words.snapshot.keywords = vec!["badword".into()];
        assert_eq!(count_id(&app, "dm-message-text"), 0, "collapsed: no body");
        assert!(
            reported_at_height(&app, 40).is_empty(),
            "a collapsed body was reported as displayed",
        );
    }

    /// The list is replaced (like feed's post_detail), not overlaid — a detail
    /// view never registers the list chrome. Esc-back to the list is the app-level
    /// keymap (`app.rs`), so the page itself carries no dismiss element.
    #[test]
    fn the_detail_view_replaces_the_list() {
        let app = detail_app(&[
            json!({ "rail": "Smtp", "sender": "alice@host.test", "subject": "Q4", "body": "hi", "message_id": "m-1" }),
        ]);
        let got = ids(&app);
        assert!(!got.contains("conversation-search-box"));
        assert!(!got.contains("conversation-item"));
    }

    /// A `subject-divider` paints for a message that opens a new subject line and
    /// only then — the manager gates `subject_line: Some` on an actual change, so a
    /// same-subject thread stays divider-light (`test_subject_divider`).
    #[test]
    fn subject_divider_paints_on_a_forced_subject_change() {
        let changed = detail_app(&[
            json!({ "rail": "Smtp", "sender": "a@host.test", "subject": "Q4 budget", "body": "one", "message_id": "m-1" }),
            json!({ "rail": "Smtp", "sender": "a@host.test", "subject": "Q4 budget", "body": "two", "message_id": "m-2", "force_subject_change": "lunch tomorrow" }),
        ]);
        let dividers = texts_of(&changed, "subject-divider");
        assert!(
            dividers.iter().any(|d| d.contains("lunch tomorrow")),
            "a divider for the changed subject: {dividers:?}"
        );

        let same = detail_app(&[
            json!({ "rail": "Smtp", "sender": "b@host.test", "subject": "same", "body": "one", "message_id": "n-1" }),
            json!({ "rail": "Smtp", "sender": "b@host.test", "subject": "same", "body": "two", "message_id": "n-2" }),
        ]);
        assert!(
            count_id(&same, "subject-divider") <= 1,
            "no extra dividers when the subject never changes"
        );
    }

    /// A reply paints one `dm-message-quote` snippeting its parent; a non-reply
    /// paints none; a reply to an un-ingested parent paints none (the manager only
    /// folds a `QuotedMessage` when the parent is loaded in the same thread —
    /// `test_conversations_reply_quote`).
    #[test]
    fn a_reply_paints_a_quote_of_its_parent_only_when_the_parent_is_loaded() {
        let with_parent = detail_app(&[
            json!({ "rail": "FaunaMls", "sender": "carol@self-nest.test", "body": "the fountain in the park", "message_id": "p-1" }),
            json!({ "rail": "FaunaMls", "sender": "carol@self-nest.test", "body": "which fountain?", "message_id": "r-1", "in_reply_to": "p-1" }),
        ]);
        let quotes = texts_of(&with_parent, "dm-message-quote");
        assert_eq!(
            quotes.len(),
            1,
            "exactly one quote, for the reply: {quotes:?}"
        );
        assert!(
            quotes[0].contains("fountain"),
            "the quote snippets the parent body: {quotes:?}"
        );

        let no_reply = detail_app(&[
            json!({ "rail": "FaunaMls", "sender": "dan@self-nest.test", "body": "standalone", "message_id": "s-1" }),
        ]);
        assert_eq!(
            count_id(&no_reply, "dm-message-quote"),
            0,
            "a non-reply has no quote"
        );

        let missing_parent = detail_app(&[
            json!({ "rail": "FaunaMls", "sender": "eve@self-nest.test", "body": "reply", "message_id": "x-1", "in_reply_to": "never-ingested" }),
        ]);
        assert_eq!(
            count_id(&missing_parent, "dm-message-quote"),
            0,
            "a reply whose parent isn't loaded hides the quote"
        );
    }

    // ── D3 + D4 on the bubble: link previews + the remote-content reveal ──────
    //
    // The bubble twin of `feed/mod.rs`'s post-card extraction. Everything the
    // assertions read is folded by `ConversationsManager::thread_detail` before
    // the page sees it (`manager.rs:179-212`), so these tests exercise the
    // page's *extraction*, which is the whole of tui's gap (render-model.md
    // § Implementation status today, the D3 and D4 rows).

    /// A standalone `[url](url)` paragraph — the exact "bare url" the shared
    /// producer turns into a `LinkPreview` block (render-model.md § D4
    /// *Producer*). A bare `https://x` does NOT autolink, so the brackets are
    /// load-bearing, not decoration (`test_conversations_link_preview.py`).
    const PREVIEW_URL: &str = "https://example.com/article";

    fn bare_url_body() -> String {
        format!("[{PREVIEW_URL}]({PREVIEW_URL})")
    }

    /// A detail app on one bubble whose body is the bare url, with the preview
    /// already `Resolved` in the manager's cache — the state a real
    /// `fauna.linkpreview.resolve` reply leaves, and the state the cross-app
    /// `conversations_seed_resolved_link_preview` command drives.
    fn preview_app(image_hash: Option<&str>) -> App {
        let app = detail_app(&[json!({
            "rail": "FaunaMls",
            "sender": "carol-linkpreview@self-nest.test",
            "body": bare_url_body(),
            "message_id": "lp-1",
        })]);
        app.conversations
            .manager
            .as_ref()
            .expect("detail_app installs a manager")
            .seed_resolved_link_preview_for_test(
                PREVIEW_URL.to_string(),
                "Example Article Title".to_string(),
                "A short description of the example article.".to_string(),
                image_hash.map(str::to_string),
            );
        app
    }

    #[test]
    fn a_resolved_link_preview_paints_the_card_and_its_three_text_children() {
        let app = preview_app(None);
        assert_eq!(
            count_id(&app, "link-preview-card"),
            1,
            "one card per preview"
        );
        assert_eq!(
            texts_of(&app, "link-preview-title"),
            vec!["Example Article Title".to_string()],
        );
        assert_eq!(
            texts_of(&app, "link-preview-description"),
            vec!["A short description of the example article.".to_string()],
        );
        // The host only — the shared `url_host`, never the scheme/path/port
        // (render-model.md:221).
        assert_eq!(
            texts_of(&app, "link-preview-domain"),
            vec!["example.com".to_string()],
        );
    }

    #[test]
    fn a_resolving_preview_paints_no_card_at_all() {
        // Same body, nothing seeded: the block stays `Resolving`, and
        // render-model.md:222-227 ratifies **no card** for it (the inline body
        // link already shows the url — a skeleton would be a perpetual-loading
        // state). This is the assertion that stops the card being painted off
        // the mere *presence* of a `LinkPreview` block.
        let app = detail_app(&[json!({
            "rail": "FaunaMls",
            "sender": "carol-linkpreview@self-nest.test",
            "body": bare_url_body(),
            "message_id": "lp-resolving",
        })]);
        for id in [
            "link-preview-card",
            "link-preview-title",
            "link-preview-description",
            "link-preview-domain",
            "link-preview-image",
        ] {
            assert_eq!(count_id(&app, id), 0, "{id} must not paint while Resolving");
        }
        // …and a `Resolving` preview is not blocked *remote* content either: the
        // reveal button has nothing to reveal.
        assert_eq!(count_id(&app, "load-remote-content-button"), 0);
    }

    #[test]
    fn the_og_image_is_blocked_until_the_message_is_revealed() {
        let mut app = preview_app(Some(&"ab".repeat(32)));
        // Blocked-by-default (render-model.md:228-240): the card's text children
        // paint immediately, the og:image does not, and the message surfaces the
        // one reveal button even though the og:image is its ONLY remote content.
        assert_eq!(count_id(&app, "link-preview-card"), 1);
        assert_eq!(
            count_id(&app, "link-preview-image"),
            0,
            "the og:image must be blocked until the message is revealed"
        );
        assert_eq!(
            count_id(&app, "load-remote-content-button"),
            1,
            "a blocked og:image alone must surface the reveal button"
        );

        apply_local(
            &mut app,
            Action::RevealRemoteImages {
                msg_id: MessageId("lp-1".to_string()),
            },
        );

        assert_eq!(
            texts_of(&app, "link-preview-image"),
            vec!["ab".repeat(32)],
            "revealing wires the og:image child off its blob hash"
        );
        assert_eq!(
            count_id(&app, "load-remote-content-button"),
            0,
            "the reveal button clears once there is nothing blocked left"
        );
    }

    #[test]
    fn a_resolved_preview_without_an_og_image_never_blocks_anything() {
        // `image_hash: None` — the nest kept no og:image. There is no remote
        // content, so no reveal button, and revealing would be a no-op. Guards
        // against gating the button on "has a resolved preview" instead of the
        // shared `has_blocked_remote_images` predicate.
        let app = preview_app(None);
        assert_eq!(count_id(&app, "link-preview-card"), 1);
        assert_eq!(count_id(&app, "link-preview-image"), 0);
        assert_eq!(count_id(&app, "load-remote-content-button"), 0);
    }

    #[test]
    fn a_body_remote_image_surfaces_the_reveal_button_per_message() {
        // The D3 body-image arm (`test_conversations_remote_image.py`): a
        // markdown `![]()` of an off-nest url is a `RemoteImage` block, blocked
        // by default. Two messages, only one with a remote image → exactly one
        // button, and revealing it must not reveal the other bubble.
        let mut app = detail_app(&[
            json!({
                "rail": "FaunaMls",
                "sender": "carol-remoteimg@self-nest.test",
                "body": "Before ![cat](https://example.com/cat.png) after",
                "message_id": "ri-1",
            }),
            json!({
                "rail": "FaunaMls",
                "sender": "carol-remoteimg@self-nest.test",
                "body": "plain text, nothing remote",
                "message_id": "ri-2",
            }),
        ]);
        assert_eq!(
            count_id(&app, "load-remote-content-button"),
            1,
            "one button, on the one bubble with a blocked remote image"
        );

        // Revealing the *other* message leaves the button up — the reveal set is
        // per-message (render-model.md § D3), so a wrong `msg_id` in the gesture
        // would show up right here.
        apply_local(
            &mut app,
            Action::RevealRemoteImages {
                msg_id: MessageId("ri-2".to_string()),
            },
        );
        assert_eq!(
            count_id(&app, "load-remote-content-button"),
            1,
            "revealing a different message must not clear this one's button"
        );

        apply_local(
            &mut app,
            Action::RevealRemoteImages {
                msg_id: MessageId("ri-1".to_string()),
            },
        );
        assert_eq!(count_id(&app, "load-remote-content-button"), 0);
    }

    /// The bubble's half of the `doc-remote-image` promote: the same element, the
    /// same three states, and the same per-message gate as the reveal button
    /// above — the walker paints nothing, so if this leg were missing the bubble
    /// would silently lose the placeholder it had before the promote.
    #[test]
    fn a_bubble_registers_doc_remote_image_and_paints_it_after_the_reveal() {
        let url = "https://example.com/cat.png";
        let mut app = detail_app(&[
            json!({
                "rail": "FaunaMls",
                "sender": "carol-remoteimg@self-nest.test",
                "body": format!("Before ![cat]({url}) after"),
                "message_id": "ri-1",
            }),
            json!({
                "rail": "FaunaMls",
                "sender": "carol-remoteimg@self-nest.test",
                "body": "plain text, nothing remote",
                "message_id": "ri-2",
            }),
        ]);
        let text = |app: &App| texts_of(app, "doc-remote-image").pop();

        // Blocked: one element (only the bubble that has one), unpainted.
        assert_eq!(count_id(&app, "doc-remote-image"), 1);
        assert!(!text(&app).unwrap().contains('▀'));
        assert!(
            kick_remote_image_fetches(&mut app).is_none(),
            "nothing is fetched before the reader reveals it"
        );

        // Revealing the OTHER message must not offer this one's url — the gate is
        // per-message, and a wrong `msg_id` would surface as a fetch right here.
        apply_local(
            &mut app,
            Action::RevealRemoteImages {
                msg_id: MessageId("ri-2".to_string()),
            },
        );
        assert!(kick_remote_image_fetches(&mut app).is_none());

        apply_local(
            &mut app,
            Action::RevealRemoteImages {
                msg_id: MessageId("ri-1".to_string()),
            },
        );
        match kick_remote_image_fetches(&mut app) {
            Some(Op::FetchRemoteImages { urls }) => assert_eq!(urls, vec![url.to_string()]),
            _ => panic!("the revealed url must be requested"),
        }

        apply_outcome(
            &mut app,
            Outcome::RemoteImages(vec![(url.to_string(), Some(remote_art()))]),
        );
        assert!(text(&app).unwrap().contains('▀'), "the reveal paints");
    }

    /// A one-cell rasterized picture; its plaintext is a single `▀`.
    fn remote_art() -> crate::thumbnail::Thumbnail {
        crate::thumbnail::Thumbnail {
            art: crate::thumbnail::HalfBlockArt {
                rows: vec![vec![crate::thumbnail::HalfBlockCell {
                    top: [1, 2, 3],
                    bottom: [4, 5, 6],
                }]],
            },
            pixels: std::sync::Arc::new(image::RgbImage::from_pixel(1, 2, image::Rgb([1, 2, 3]))),
        }
    }

    #[test]
    fn the_reveal_button_carries_this_messages_id_in_its_gesture() {
        use crate::element::Role;
        // The button is only as good as the id it dispatches. Read the gesture
        // off the registered element rather than trusting the paint order: a
        // hard-coded or first-message id passes every count assertion above.
        let app = detail_app(&[
            json!({
                "rail": "FaunaMls",
                "sender": "carol-remoteimg@self-nest.test",
                "body": "plain text first",
                "message_id": "g-1",
            }),
            json!({
                "rail": "FaunaMls",
                "sender": "carol-remoteimg@self-nest.test",
                "body": "second ![cat](https://example.com/cat.png)",
                "message_id": "g-2",
            }),
        ]);
        let button = elements(&app)
            .into_iter()
            .find(|e| e.id == "load-remote-content-button")
            .expect("the second bubble's blocked image paints the button");
        let Role::Button(Gesture::Conversations(Action::RevealRemoteImages { msg_id })) =
            button.role
        else {
            panic!("the reveal button must carry a RevealRemoteImages gesture");
        };
        assert_eq!(
            msg_id,
            MessageId("g-2".to_string()),
            "the gesture must name the bubble it was painted on, not the first message"
        );
    }

    // ── Slice E: the conversation_detail overlays (membership + rename) ────────

    /// An authed app holding one FaunaMls **group** thread, opened (`Mode::Detail`).
    /// The MlsGroup flavor is what un-gates rename + membership (a 1:1 doesn't),
    /// so this is the analogue of `detail_app` for the overlay tests.
    fn group_detail_app(participants: &[&str]) -> App {
        let mut app = authed_app();
        let manager = ConversationsManager::new();
        manager.install_mock_backends_for_test();
        // Struct-update, not a hand-listed field set: a branch that grows
        // `ConversationsState` then merges cleanly instead of colliding on the
        // grown axis — the project-wide fixture-shape convention.
        let state = ConversationsState {
            manager: Some(manager),
            ..Default::default()
        };
        assert!(
            create_mls_group(&state, &json!({ "participants": participants })),
            "the create_mls_group seam should bootstrap a group thread"
        );
        let group_id = state
            .snapshot()
            .unwrap()
            .threads
            .iter()
            .find(|t| t.flavor == fauna_conversations::ThreadFlavor::MlsGroup)
            .expect("a group thread exists after create_mls_group")
            .thread_id
            .clone();
        app.conversations = state;
        app.page = Page::Conversations;
        apply_local(&mut app, Action::SelectThread(group_id.0.clone()));
        app
    }

    /// The header actions are the shared capability matrix, so tui shows exactly
    /// what the other apps show: `thread-rename-button` iff the thread is an
    /// MLS group (`supports_rename`), `thread-add-participant-button` iff its
    /// membership is mutable (`supports_membership_change`). The negatives are the
    /// contract `test_thread_rename`'s hidden-on-1:1/subject-keyed cases assert.
    #[test]
    fn detail_header_actions_are_capability_gated() {
        // FaunaMls group: both rename + add-participant.
        let group = group_detail_app(&["bob@self-nest.test"]);
        let g = ids(&group);
        assert!(g.contains("thread-rename-button"), "a group is renamable");
        assert!(
            g.contains("thread-add-participant-button"),
            "group membership is mutable"
        );

        // FaunaMls 1:1: add-participant (it forks to a group), but NOT rename.
        let one = detail_app(&[
            json!({ "rail": "FaunaMls", "sender": "carol@self-nest.test", "body": "hi", "message_id": "m-1" }),
        ]);
        let o = ids(&one);
        assert!(
            !o.contains("thread-rename-button"),
            "a 1:1 is not renamable"
        );
        assert!(
            o.contains("thread-add-participant-button"),
            "a 1:1 can fork to a group"
        );

        // Smtp: neither — mail membership is immutable and mail has no rename.
        let mail = detail_app(&[
            json!({ "rail": "Smtp", "sender": "alice@host.test", "subject": "Q4", "body": "hi", "message_id": "m-1" }),
        ]);
        let m = ids(&mail);
        assert!(!m.contains("thread-rename-button"), "mail has no rename");
        assert!(
            !m.contains("thread-add-participant-button"),
            "mail membership is immutable"
        );
    }

    /// on a membership-change-capable thread the member chip
    /// is a CONTROL — its tap reaches `manager.remove_participant` through the
    /// ordinary dispatch door (never the `conv_backend::e2e_remove` agent
    /// seam), and the roster actually shrinks. Driven end-to-end against the
    /// mock backend, so this pins the effect, not a call count.
    #[tokio::test]
    async fn a_member_chip_tap_evicts_that_participant() {
        use crate::element::Role;
        let mut app = group_detail_app(&["bob@self-nest.test", "carol@self-nest.test"]);
        let chips: Vec<Element> = elements(&app)
            .into_iter()
            .filter(|e| e.id == "thread-member-chip")
            .collect();
        assert!(!chips.is_empty(), "the group detail paints member chips");
        let mut bob = None;
        for chip in &chips {
            // Every chip on a mutable-membership thread is the remove control,
            // carrying the row's OWN address (zip, never an index).
            let Role::Button(Gesture::Conversations(Action::RemoveMember { addr })) = &chip.role
            else {
                panic!(
                    "a group-thread member chip must be a remove control, got {:?} for {:?}",
                    chip.role, chip.text
                );
            };
            if chip.text.contains("bob") {
                bob = Some(Action::RemoveMember { addr: addr.clone() });
            }
        }
        let action = bob.expect("bob's chip carries his own address");
        let op = apply_local(&mut app, action).expect("the eviction is an async op");
        assert!(matches!(op, Op::RemoveMember { .. }));
        op.run().await;
        let after: Vec<String> = elements(&app)
            .into_iter()
            .filter(|e| e.id == "thread-member-chip")
            .map(|e| e.text)
            .collect();
        assert!(
            !after.iter().any(|t| t.contains("bob")),
            "bob's chip must be gone after the eviction, roster: {after:?}"
        );
        assert!(
            after.iter().any(|t| t.contains("carol")),
            "carol must survive bob's eviction, roster: {after:?}"
        );
    }

    // ── The post-succession member-review pair ────────────────────────────────

    /// The actor id behind a rendered chip whose text contains `needle` — the
    /// same join the roster makes, read off the production element list rather
    /// than reconstructed, so a fixture cannot disagree with what the page
    /// actually rendered.
    fn chip_person(app: &App, needle: &str) -> ActorId {
        use crate::element::Role;
        elements(app)
            .into_iter()
            .find_map(|e| match (&e.id[..], &e.role) {
                (
                    "thread-member-chip",
                    Role::Button(Gesture::Conversations(Action::RemoveMember { addr })),
                ) if e.text.contains(needle) => addr.person_actor_id(),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no fauna member chip matching {needle:?}"))
    }

    /// The scope path of every element with `id`, as `(container, index)` pairs.
    fn scopes_of(app: &App, id: &str) -> Vec<Vec<(String, usize)>> {
        elements(app)
            .into_iter()
            .filter(|e| e.id == id)
            .map(|e| {
                e.path
                    .iter()
                    .map(|(container, index)| (container.clone(), *index))
                    .collect()
            })
            .collect()
    }

    /// A member nobody flagged renders **neither** half of the pair, and a
    /// flagged one renders both — the absent-not-empty rule every adjudication
    /// plane follows, and the reason it matters here more than anywhere: after a
    /// recovery almost every honest member would carry a permanently-painted
    /// mark, which trains the user straight past the one that matters.
    ///
    /// The two halves are asserted **on the same app**, one field apart, so this
    /// fails if the render stops consulting the roster — not merely if the
    /// predicate is wrong.
    #[test]
    fn the_review_pair_paints_only_for_a_flagged_member() {
        let mut app = group_detail_app(&["bob@self-nest.test", "carol@self-nest.test"]);
        let before = ids(&app);
        assert!(
            !before.contains("thread-member-unattested-mark")
                && !before.contains("thread-member-keep-button"),
            "an un-succeeded account flags nobody"
        );

        let bob = chip_person(&app, "bob");
        app.member_reviews = vec![fauna_core::data::MemberReview {
            person: bob,
            reasons: vec![fauna_core::data::MemberUnattestedReason::CompromiseWindow],
        }];
        let after = ids(&app);
        assert!(
            after.contains("thread-member-unattested-mark")
                && after.contains("thread-member-keep-button"),
            "a flagged member gets the mark AND its Keep"
        );
        assert_eq!(
            elements(&app)
                .iter()
                .filter(|e| e.id == "thread-member-keep-button")
                .count(),
            1,
            "only the flagged member of the two"
        );
    }

    /// The pair is scoped to the chip it is about — the property a flat push
    /// would silently break, because the pair renders on flagged members only
    /// while the chips render on all of them.
    ///
    /// Carol is flagged and bob is not, so a flat render would put the mark at
    /// index 0 while carol's chip is at index 1: the driver would read a mark
    /// that names the wrong person, and every count would still look right.
    #[test]
    fn the_review_pair_is_scoped_to_the_chip_it_names() {
        let mut app = group_detail_app(&["bob@self-nest.test", "carol@self-nest.test"]);
        let carol = chip_person(&app, "carol");
        let carol_index = elements(&app)
            .iter()
            .filter(|e| e.id == "thread-member-chip")
            .position(|e| e.text.contains("carol"))
            .expect("carol has a chip");
        assert_ne!(
            carol_index, 0,
            "the fixture is only meaningful while the flagged member is NOT first"
        );

        app.member_reviews = vec![fauna_core::data::MemberReview {
            person: carol,
            reasons: vec![fauna_core::data::MemberUnattestedReason::CompromiseWindow],
        }];
        for id in ["thread-member-unattested-mark", "thread-member-keep-button"] {
            assert_eq!(
                scopes_of(&app, id),
                vec![vec![("thread-member-chip".to_string(), carol_index)]],
                "{id} must be scoped to carol's own chip"
            );
        }
    }

    /// A participant with no actor id can never be flagged: the roster's people
    /// are MLS group members carried across a succession, and a far-network
    /// address on a bridge has no actor id to be one.
    ///
    /// ⚠ **The rail here is load-bearing, and mail is the WRONG choice** — the
    /// first version of this test used an SMTP thread and was vacuous, because
    /// mail's `supports_membership_change` is `false`, so its chips are plain
    /// labels and the review branch is never reached at all: deleting the
    /// actor-id guard entirely left it green. A bridge whose declared vector
    /// offers membership change is the reachable case — the chips are the
    /// remove control over addresses that carry no actor id — so the bridged
    /// backend is registered here with such a vector, and the guard is the
    /// thing being tested.
    ///
    /// Asserted with a roster that is **not** empty, so it fails if the join
    /// ever answers "flagged" for an address it cannot identify.
    #[test]
    fn a_participant_with_no_actor_id_is_never_under_review() {
        use fauna_conversations::backend::{
            BridgedOutbound, BridgedResolved, BridgedSent, BridgedSink,
        };
        use fauna_conversations::backends::bridged::{BridgeIdentity, BridgedBackend};

        struct NoWire;
        #[async_trait::async_trait]
        impl BridgedSink for NoWire {
            async fn resolve(&self, _raw: String) -> Result<Option<BridgedResolved>, String> {
                Ok(None)
            }
            async fn send(&self, _outbound: BridgedOutbound) -> Result<BridgedSent, String> {
                Err("no wire in this test".into())
            }
        }

        let mut app = authed_app();
        let manager = ConversationsManager::new();
        manager.install_mock_backends_for_test();
        let bridged = BridgedBackend::new(Arc::new(NoWire));
        bridged.set_identities(vec![BridgeIdentity {
            id: "example".into(),
            label: "Example".into(),
            glyph: fauna_core::source_glyph::SourceGlyph::Bridge,
            capabilities: fauna_conversations::ThreadCapabilities {
                supports_membership_change: true,
                ..fauna_conversations::capabilities::derive_capabilities(
                    Rail::Bridged,
                    fauna_conversations::ThreadFlavor::OneToOne,
                )
            },
            bridge_x25519: [0u8; 32],
        }]);
        manager.register_backend(Arc::new(bridged));
        let state = ConversationsState {
            manager: Some(manager),
            ..Default::default()
        };
        assert!(inject_inbound(
            &state,
            &json!({
                "rail": "Bridged",
                "bridge_id": "example",
                "sender": "@alice:instance.test",
                "body": "hi",
                "message_id": "m-1"
            })
        ));
        app.conversations = state;
        app.page = Page::Conversations;
        let thread_id = app.conversations.snapshot().unwrap().threads[0]
            .thread_id
            .clone();
        apply_local(&mut app, Action::SelectThread(thread_id.0.clone()));
        assert!(
            elements(&app).iter().any(|e| e.id == "thread-member-chip"
                && matches!(
                    e.role,
                    crate::element::Role::Button(Gesture::Conversations(
                        Action::RemoveMember { .. }
                    ))
                )),
            "the fixture is only meaningful while the chips are the REMOVE control — \
             on a rail whose membership is immutable the review branch is unreachable \
             and this test would pin nothing"
        );

        app.member_reviews = vec![fauna_core::data::MemberReview {
            person: ActorId([9u8; 32]),
            reasons: vec![fauna_core::data::MemberUnattestedReason::CompromiseWindow],
        }];
        let painted = ids(&app);
        assert!(
            !painted.contains("thread-member-unattested-mark")
                && !painted.contains("thread-member-keep-button"),
            "an acct-only chip has no actor id, so it can carry no verdict"
        );
    }

    /// A bridged room on the unified list paints what its snapshot carries and
    /// nothing tui decided: the bridge's declared glyph with its declared label
    /// beside it, the transport-only class on the header, and — on a
    /// supervised account's held room only — the family gate's marker on the
    /// row and in the detail, over a thread that still opens and reads
    /// (`family-safety.md` § The bridge-DM gate; `conversations.md` § Where
    /// logic lives → *The `Bridged` adapter*).
    #[test]
    fn a_bridged_room_paints_its_declared_identity_class_and_guardian_marker() {
        use fauna_conversations::backend::{
            BridgedOutbound, BridgedResolved, BridgedSent, BridgedSink,
        };
        use fauna_conversations::backends::bridged::{
            BridgeIdentity, BridgedBackend, BridgedRoomRecord,
        };
        use fauna_conversations::snapshot::GuardianState;

        struct NoWire;
        #[async_trait::async_trait]
        impl BridgedSink for NoWire {
            async fn resolve(&self, _raw: String) -> Result<Option<BridgedResolved>, String> {
                Ok(None)
            }
            async fn send(&self, _outbound: BridgedOutbound) -> Result<BridgedSent, String> {
                Err("no wire in this test".into())
            }
        }

        let room = |peer: &str, guardian_state| BridgedRoomRecord {
            bridge_id: "example".into(),
            identity: Some(BridgeIdentity {
                id: "example".into(),
                label: "Example".into(),
                glyph: fauna_core::source_glyph::SourceGlyph::Globe,
                capabilities: fauna_conversations::capabilities::derive_capabilities(
                    Rail::Bridged,
                    fauna_conversations::ThreadFlavor::OneToOne,
                ),
                bridge_x25519: [0u8; 32],
            }),
            participants: vec![peer.into()],
            // The inject seam's "you" — the room reports it as the account's
            // own far address, as a real bridge does.
            self_address: Some(fauna_conversations::manager::TEST_SEAM_SELF_ADDRESS.into()),
            guardian_state,
        };

        let mut app = authed_app();
        let manager = ConversationsManager::new();
        manager.install_mock_backends_for_test();
        let bridged = BridgedBackend::new(Arc::new(NoWire));
        bridged.set_rooms(vec![
            room("@known:instance.test", None),
            room("@cold:instance.test", Some(GuardianState::Held)),
        ]);
        manager.register_backend(Arc::new(bridged));
        let state = ConversationsState {
            manager: Some(manager),
            ..Default::default()
        };
        for (sender, id) in [
            ("@known:instance.test", "m-1"),
            ("@cold:instance.test", "m-2"),
        ] {
            assert!(inject_inbound(
                &state,
                &json!({
                    "rail": "Bridged",
                    "bridge_id": "example",
                    "sender": sender,
                    "body": "hi",
                    "message_id": id
                })
            ));
        }
        app.conversations = state;
        app.page = Page::Conversations;

        // The list: both rows carry the declared glyph and label; exactly one
        // carries the marker, and it is the cold peer's row.
        let threads = app.conversations.snapshot().unwrap().threads;
        let cold_row = threads
            .iter()
            .position(|t| t.guardian_state.is_some())
            .expect("the held room is on the list");
        let painted = elements(&app);
        let icons: Vec<&Element> = painted.iter().filter(|e| e.id == "protocol-icon").collect();
        assert_eq!(icons.len(), 2);
        for icon in &icons {
            assert_eq!(
                icon.text,
                format!(
                    "{} Example",
                    fauna_core::source_glyph::SourceGlyph::Globe.emoji()
                )
            );
            assert_eq!(attr(icon, "bridge"), Some("example"));
        }
        let markers: Vec<&Element> = painted
            .iter()
            .filter(|e| e.id == "conversation-guardian-state")
            .collect();
        assert_eq!(markers.len(), 1, "only the held room is marked");
        assert_eq!(attr(markers[0], "state"), Some("held"));
        assert_eq!(
            markers[0].text,
            fauna_i18n::strings::conversations::unified::GUARDIAN_STATE_HELD
        );
        assert_eq!(
            markers[0].path.first().map(|(c, i)| (c.as_str(), *i)),
            Some(("conversation-item", cold_row)),
            "the marker sits in its own row"
        );

        // The detail: the marker again, the transport-only class, and the
        // held thread's message still on screen.
        apply_local(
            &mut app,
            Action::SelectThread(threads[cold_row].thread_id.0.clone()),
        );
        assert_eq!(
            attr(&one(&app, "conversation-guardian-state"), "state"),
            Some("held")
        );
        assert_eq!(
            attr(&one(&app, "thread-room-class"), "class"),
            Some("transport-only")
        );
        assert!(
            ids(&app).contains("dm-message-text"),
            "a held thread stays fully readable"
        );

        // The known peer's detail carries no marker.
        apply_local(
            &mut app,
            Action::SelectThread(threads[1 - cold_row].thread_id.0.clone()),
        );
        assert!(!ids(&app).contains("conversation-guardian-state"));

        // The recipient picker names the bridge by its declared label.
        apply_local(&mut app, Action::StartNewConversation);
        assert!(
            elements(&app).iter().any(|e| e.id.is_empty()
                && e.text == conversations::unified::recipient_picker_bridges("Example")),
            "the picker lists the registered bridge"
        );
    }

    /// Keep dispatches the shared succession-ledger write for **that person** — the
    /// half a paint test cannot see. Pressing it with no session store yields no
    /// op rather than a panic (the pre-auth shape every gesture here takes).
    ///
    /// ⚠ This pins the *use*: a Keep button that painted but reached no store
    /// would pass every assertion above.
    #[test]
    fn keep_dispatches_the_shared_verdict_write_for_that_person() {
        use crate::element::Role;
        let mut app = group_detail_app(&["bob@self-nest.test"]);
        let bob = chip_person(&app, "bob");
        app.member_reviews = vec![fauna_core::data::MemberReview {
            person: bob,
            reasons: vec![fauna_core::data::MemberUnattestedReason::CompromiseWindow],
        }];

        let keep = elements(&app)
            .into_iter()
            .find(|e| e.id == "thread-member-keep-button")
            .expect("the flagged member has a Keep");
        let Role::Button(Gesture::Conversations(action)) = keep.role else {
            panic!("Keep must be a gesture button, got {:?}", keep.role);
        };
        assert!(
            matches!(action, Action::KeepMember { person } if person == bob),
            "Keep must carry the chip's OWN person, never an index into the roster: {action:?}"
        );

        // No session store (this fixture never authed an account store), so the
        // press is a no-op rather than a panic — and it must NOT be treated as a
        // local success that clears the mark.
        assert!(
            apply_local(&mut app, action).is_none(),
            "with no store the press yields no op"
        );
        assert!(
            ids(&app).contains("thread-member-unattested-mark"),
            "and the mark stays: nothing was written, so nothing was answered"
        );
    }

    /// The whole Keep path, end to end against the shared ledger double: the
    /// press writes the verdict **at rest**, the re-read drops the person, and
    /// the mark is gone from the next paint — while the un-flagged member beside
    /// them is untouched.
    ///
    /// ⚠ This is the pin that a button reaching *nothing* would fail. The paint
    /// tests above all pass against a Keep wired to a no-op, and so does the
    /// dispatch test: only running the op can tell the difference.
    ///
    /// ⚠ It also pins the verdict is **kept at rest, not deleted** — the
    /// encoding ratified 2026-08-06, without which a re-run of the raising sweep
    /// silently re-asks everything the owner just worked through.
    #[tokio::test]
    async fn a_keep_press_writes_the_verdict_and_clears_only_that_members_mark() {
        let mut app = group_detail_app(&["bob@self-nest.test", "carol@self-nest.test"]);
        let bob = chip_person(&app, "bob");
        let carol = chip_person(&app, "carol");
        assert_ne!(bob, carol, "the fixture must hold two distinct people");

        let store = std::sync::Arc::new(
            fauna_client_config::test_helpers::FakeSuccessionLedgerStore::empty(ActorId([1u8; 32])),
        );
        store.mutate(|cfg| {
            cfg.raise_member_reviews(
                [bob, carol],
                ActorId([2u8; 32]),
                fauna_core::data::MemberUnattestedReason::CompromiseWindow,
            );
        });
        app.ledger_store = Some(store.clone());
        app.member_reviews = store.current().open_member_reviews();
        assert_eq!(
            elements(&app)
                .iter()
                .filter(|e| e.id == "thread-member-keep-button")
                .count(),
            2,
            "both members start flagged"
        );

        let op = apply_local(&mut app, Action::KeepMember { person: bob })
            .expect("Keep is an async op once a session store exists");
        apply_outcome(&mut app, op.run().await);

        let stored = store.current();
        assert_eq!(
            stored
                .unattested_member_items
                .iter()
                .filter(|item| item.person == bob)
                .map(|item| item.verdict.clone())
                .collect::<Vec<_>>(),
            vec![fauna_core::data::UnattestedVerdict::Kept],
            "bob's item stays on file CARRYING the verdict: {:?}",
            stored.unattested_member_items
        );
        assert!(
            stored
                .unattested_member_items
                .iter()
                .any(|item| item.person == carol && item.verdict.is_open()),
            "carol was not asked about, so her item must stay open"
        );

        let keeps: Vec<String> = elements(&app)
            .into_iter()
            .filter(|e| e.id == "thread-member-keep-button")
            .map(|e| format!("{:?}", e.path))
            .collect();
        assert_eq!(
            keeps.len(),
            1,
            "exactly carol's Keep survives the paint: {keeps:?}"
        );
        let carol_index = elements(&app)
            .iter()
            .filter(|e| e.id == "thread-member-chip")
            .position(|e| e.text.contains("carol"))
            .expect("carol still has a chip");
        assert_eq!(
            scopes_of(&app, "thread-member-keep-button"),
            vec![vec![("thread-member-chip".to_string(), carol_index)]],
            "and it is hers"
        );
    }

    /// The roster read is what clears the pair, not the press — so a `Keep`
    /// outcome carrying `None` (a failed write or a failed re-read) leaves the
    /// mark standing, and one carrying a fresh roster drops it.
    ///
    /// The over-ask direction is the ratified one: a re-asked question is
    /// harmless, a silently hidden flagged person is the failure the surface
    /// exists to prevent.
    #[test]
    fn a_failed_keep_leaves_the_mark_and_a_fresh_roster_clears_it() {
        let mut app = group_detail_app(&["bob@self-nest.test"]);
        let bob = chip_person(&app, "bob");
        app.member_reviews = vec![fauna_core::data::MemberReview {
            person: bob,
            reasons: vec![fauna_core::data::MemberUnattestedReason::CompromiseWindow],
        }];

        apply_outcome(&mut app, Outcome::MemberReviews(None));
        assert!(
            ids(&app).contains("thread-member-unattested-mark"),
            "a failed Keep must not clear a mark that is still genuinely open"
        );

        apply_outcome(&mut app, Outcome::MemberReviews(Some(Vec::new())));
        assert!(
            !ids(&app).contains("thread-member-unattested-mark"),
            "the re-read roster is what clears it"
        );
    }

    /// The other half of the capability gate: a mail thread's chips stay
    /// informational labels — membership is immutable there, so no chip may
    /// carry an eviction gesture (`conversations.md` § Participants vs reply
    /// recipients; capability-gated, never rail-branched).
    #[test]
    fn mail_member_chips_stay_informational_labels() {
        use crate::element::Role;
        let app = detail_app(&[json!({
            "rail": "Smtp",
            "sender": "alice@host.test",
            "subject": "Q4",
            "body": "hi",
            "message_id": "m-1"
        })]);
        let chips: Vec<Element> = elements(&app)
            .into_iter()
            .filter(|e| e.id == "thread-member-chip")
            .collect();
        assert!(!chips.is_empty(), "the mail detail paints member chips");
        for chip in &chips {
            assert!(
                matches!(chip.role, Role::Label),
                "a mail member chip must stay a label, got {:?} for {:?}",
                chip.role,
                chip.text
            );
        }
    }

    /// Opening the rename overlay seeds the field with the current label and swaps
    /// the read view for `thread-rename-field` + `thread-rename-confirm`; the
    /// unified write path edits the draft; confirm hands back the async
    /// `RenameThread` op and exits edit mode.
    #[test]
    fn rename_overlay_opens_seeds_and_confirms() {
        let mut app = group_detail_app(&["bob@self-nest.test"]);
        assert!(apply_local(&mut app, Action::OpenRename).is_none());
        assert!(
            app.conversations.rename_draft.is_some(),
            "the draft is seeded with the current label"
        );
        let open = ids(&app);
        assert!(open.contains("thread-rename-field"));
        assert!(open.contains("thread-rename-confirm"));
        assert!(
            !open.contains("dm-message-text"),
            "the overlay replaces the read view"
        );
        // Edit via the one write door.
        let _ = set_field(
            &mut app.conversations,
            ConversationsField::ThreadRename,
            "Lunch Crew".to_string(),
        );
        assert_eq!(
            field(&app.conversations, &ConversationsField::ThreadRename),
            "Lunch Crew"
        );
        // Confirm: async op + exit edit mode.
        let op = apply_local(&mut app, Action::ConfirmRename).expect("confirm returns a rename op");
        match op {
            Op::RenameThread { label, .. } => assert_eq!(label, "Lunch Crew"),
            _ => panic!("expected a RenameThread op"),
        }
        assert!(
            app.conversations.rename_draft.is_none(),
            "confirm exits edit mode"
        );
    }

    /// A blank / whitespace-only rename is a no-op (linux's overlay gates Save on
    /// a non-empty trimmed field) — no op returned, but edit mode still exits so
    /// the field can't get stuck.
    #[test]
    fn confirm_blank_rename_is_a_noop() {
        let mut app = group_detail_app(&["bob@self-nest.test"]);
        apply_local(&mut app, Action::OpenRename);
        let _ = set_field(
            &mut app.conversations,
            ConversationsField::ThreadRename,
            "   ".to_string(),
        );
        assert!(
            apply_local(&mut app, Action::ConfirmRename).is_none(),
            "a blank rename is a no-op"
        );
        assert!(
            app.conversations.rename_draft.is_none(),
            "edit mode still exits"
        );
    }

    /// `thread-add-participant-button` opens the overlay (manager state), which
    /// replaces the read view with the recipient picker + `add-participant-confirm`.
    /// The picker input routes to the add-participant picker (overlay priority) and
    /// reads back through the same field; confirm hands back the async membership op.
    #[test]
    fn add_participant_overlay_opens_routes_input_and_confirms() {
        let mut app = group_detail_app(&["bob@self-nest.test", "alice@self-nest.test"]);
        assert!(apply_local(&mut app, Action::OpenAddParticipant).is_none());
        assert!(
            app.conversations
                .snapshot()
                .unwrap()
                .add_participant
                .is_some(),
            "the overlay is manager state, not a local mode"
        );
        let open = ids(&app);
        assert!(open.contains("recipient-picker-input"));
        assert!(open.contains("add-participant-confirm"));
        assert!(
            !open.contains("dm-message-text"),
            "the overlay replaces the read view"
        );
        // Overlay-aware routing: the write lands on the add-participant picker
        // (not the new-thread compose), read back through the same field.
        let pending = set_field(
            &mut app.conversations,
            ConversationsField::RecipientInput,
            "carol@self-nest.test".to_string(),
        );
        assert!(
            pending.is_some(),
            "a recipient write implies the async resolve"
        );
        assert_eq!(
            field(&app.conversations, &ConversationsField::RecipientInput),
            "carol@self-nest.test"
        );
        // Confirm returns the async membership op (the fork/add itself is
        // manager- + e2e-tested).
        assert!(matches!(
            apply_local(
                &mut app,
                Action::ConfirmAddParticipant {
                    in_place_mls_group: true,
                },
            ),
            Some(Op::ConfirmAddParticipant { .. })
        ));
    }

    /// Esc cancels an open overlay (either flavor) before it would leave the
    /// thread — `detail_overlay_open` gates it, `cancel_detail_overlay` clears
    /// whichever is open (rename = local draft, add-participant = manager state).
    #[test]
    fn detail_overlay_open_and_cancel_cover_both_flavors() {
        let mut app = group_detail_app(&["bob@self-nest.test"]);
        assert!(!detail_overlay_open(&app.conversations));

        apply_local(&mut app, Action::OpenRename);
        assert!(detail_overlay_open(&app.conversations));
        cancel_detail_overlay(&mut app.conversations);
        assert!(
            !detail_overlay_open(&app.conversations),
            "rename overlay cancelled"
        );
        assert!(app.conversations.rename_draft.is_none());

        apply_local(&mut app, Action::OpenAddParticipant);
        assert!(detail_overlay_open(&app.conversations));
        cancel_detail_overlay(&mut app.conversations);
        assert!(
            !detail_overlay_open(&app.conversations),
            "add-participant overlay cancelled"
        );
        assert!(
            app.conversations
                .snapshot()
                .unwrap()
                .add_participant
                .is_none()
        );
    }

    // ── Slice E parts 3+4: the reply compose bar ──────────────────────────────

    /// The open thread's id (the tests drive Detail mode, so this is `Some`).
    fn open_thread_id(app: &App) -> ThreadId {
        match &app.conversations.mode {
            Mode::Detail(t) => t.clone(),
            _ => panic!("expected a detail view"),
        }
    }

    /// On a MAIL thread the reply bar is fully shaped: per-message reply +
    /// reply-all, the editable To line, body + send. A reply seeds the To line
    /// with the sender; removing the chip drops it (thread history untouched).
    /// This is the same contract test_conversations_reply_recipients drives e2e.
    #[test]
    fn reply_bar_seeds_and_edits_recipients_on_a_mail_thread() {
        let mut app = detail_app(&[
            json!({ "rail": "Smtp", "sender": "alice@host.test", "subject": "Lunch", "body": "hi", "message_id": "m-1" }),
        ]);
        let base = ids(&app);
        for id in [
            "dm-reply-button",
            "dm-reply-all-button",
            "dm-reply-recipient-add",
            "dm-text-field",
            "dm-send-button",
        ] {
            assert!(base.contains(id), "mail reply bar must register {id}");
        }
        assert_eq!(
            count_id(&app, "dm-reply-recipient-chip"),
            0,
            "the To line is empty before a reply is seeded"
        );

        // Reply (sender-only) seeds exactly the sender as one chip.
        apply_local(
            &mut app,
            Action::ReplyToMessage {
                msg_id: MessageId("m-1".to_string()),
                reply_all: false,
            },
        );
        assert_eq!(
            count_id(&app, "dm-reply-recipient-chip"),
            1,
            "reply seeds exactly one chip"
        );
        assert!(
            texts_of(&app, "dm-reply-recipient-chip")[0].contains("alice@host.test"),
            "the chip names the sender: {:?}",
            texts_of(&app, "dm-reply-recipient-chip")
        );
        assert_eq!(
            count_id(&app, "dm-reply-recipient-remove"),
            1,
            "each chip carries a remove"
        );

        // × removes that recipient from this reply.
        let tid = open_thread_id(&app);
        let addr = app
            .conversations
            .manager
            .as_ref()
            .unwrap()
            .thread_detail(tid)
            .unwrap()
            .compose
            .reply_recipients[0]
            .clone();
        apply_local(&mut app, Action::RemoveReplyRecipient { addr });
        assert_eq!(
            count_id(&app, "dm-reply-recipient-chip"),
            0,
            "the chip is gone after remove"
        );
    }

    /// `dm-reply-preview`/`dm-reply-cancel` are absent with no reply armed,
    /// appear showing the TARGETED message's own body (not some other
    /// message's) once one is, and `dm-reply-cancel` both clears them and
    /// drops the seeded recipient chip — `set_reply_to`'s documented
    /// clear-on-cancel contract (`manager.rs`).
    #[test]
    fn reply_preview_shows_the_target_body_and_cancel_clears_it() {
        let mut app = detail_app(&[
            json!({ "rail": "Smtp", "sender": "alice@host.test", "subject": "Lunch", "body": "want to grab lunch?", "message_id": "m-1" }),
            json!({ "rail": "Smtp", "sender": "alice@host.test", "subject": "Lunch", "body": "actually let's do dinner", "message_id": "m-2" }),
        ]);
        assert!(
            !ids(&app).contains("dm-reply-preview"),
            "no reply armed yet"
        );
        assert!(!ids(&app).contains("dm-reply-cancel"));

        apply_local(
            &mut app,
            Action::ReplyToMessage {
                msg_id: MessageId("m-1".to_string()),
                reply_all: false,
            },
        );
        assert_eq!(count_id(&app, "dm-reply-preview"), 1);
        assert_eq!(
            texts_of(&app, "dm-reply-preview")[0],
            "alice@host.test: want to grab lunch?",
            "the preview names m-1's sender and shows m-1's own body, not m-2's"
        );
        assert_eq!(count_id(&app, "dm-reply-cancel"), 1);
        assert_eq!(
            count_id(&app, "dm-reply-recipient-chip"),
            1,
            "the reply seeded a recipient chip"
        );

        apply_local(&mut app, Action::CancelReply);
        assert!(
            !ids(&app).contains("dm-reply-preview"),
            "cancel clears the preview"
        );
        assert!(
            !ids(&app).contains("dm-reply-cancel"),
            "cancel clears its own button"
        );
        assert_eq!(
            count_id(&app, "dm-reply-recipient-chip"),
            0,
            "cancel also drops the seeded chip (set_reply_to's clear-on-cancel contract)"
        );
    }

    /// On a FaunaMls thread the recipients ARE the group membership, so the
    /// editable To line + reply-all are hidden (`supports_recipient_selection` ==
    /// false) — but a per-message reply and the body/send still render.
    #[test]
    fn reply_bar_hides_recipient_selection_on_a_fauna_thread() {
        let app = detail_app(&[
            json!({ "rail": "FaunaMls", "sender": "bob@self-nest.test", "body": "hey", "message_id": "m-1" }),
        ]);
        let got = ids(&app);
        assert!(
            got.contains("dm-reply-button"),
            "a per-message reply still exists on FaunaMls"
        );
        assert!(
            !got.contains("dm-reply-all-button"),
            "no reply-all off mail"
        );
        assert!(
            !got.contains("dm-reply-recipient-add"),
            "no editable To line off mail"
        );
        assert!(
            got.contains("dm-text-field") && got.contains("dm-send-button"),
            "the body + send render on every rail"
        );
    }

    /// Enter on `dm-reply-recipient-add` parses + commits the buffer as a chip and
    /// clears it; a malformed address is a no-op that keeps the buffer for a fix.
    /// `dm-send-button` in Detail is the async per-thread send.
    #[test]
    fn commit_reply_recipient_add_parses_then_send_thread_is_async() {
        let mut app = detail_app(&[
            json!({ "rail": "Smtp", "sender": "alice@host.test", "subject": "Lunch", "body": "hi", "message_id": "m-1" }),
        ]);
        // A valid add commits + clears.
        let _ = set_field(
            &mut app.conversations,
            ConversationsField::ReplyRecipientAdd,
            "carol@host.test".to_string(),
        );
        apply_local(&mut app, Action::CommitReplyRecipientAdd);
        assert!(
            app.conversations.reply_recipient_draft.is_empty(),
            "a committed add clears the buffer"
        );
        assert!(
            texts_of(&app, "dm-reply-recipient-chip")
                .iter()
                .any(|c| c.contains("carol@host.test")),
            "the committed recipient renders as a chip: {:?}",
            texts_of(&app, "dm-reply-recipient-chip")
        );
        // A malformed add keeps the buffer (spaces are never a valid address).
        let _ = set_field(
            &mut app.conversations,
            ConversationsField::ReplyRecipientAdd,
            "has spaces here".to_string(),
        );
        apply_local(&mut app, Action::CommitReplyRecipientAdd);
        assert_eq!(
            app.conversations.reply_recipient_draft, "has spaces here",
            "a malformed add is a no-op that keeps the buffer"
        );
        // Detail send is the async per-thread op.
        assert!(matches!(
            apply_local(&mut app, Action::SendThread { rail: Rail::Smtp }),
            Some(Op::SendThread { .. })
        ));
    }

    /// `Outcome::Done` now clears a prior page error, unifying with the six
    /// other `Outcome`-shaped pages (each clears on its own success variant —
    /// `settings::Outcome::FilterCreated`, `events::Outcome::EventMutated`, …).
    /// Every conversations mutation collapses into this one success variant,
    /// so success on ANY gesture clears a stale error from an unrelated prior
    /// failure — exactly the existing cross-page contract (`app.errors` is
    /// page-scoped, not gesture-scoped).
    #[test]
    fn outcome_done_clears_a_prior_error() {
        let mut app = conv_app(&[("Smtp", "alice@host.test", "Q4 budget")]);
        app.errors
            .insert(Page::Conversations, "a stale send failure".to_string());
        apply_outcome(&mut app, Outcome::Done);
        assert!(
            !app.errors.contains_key(&Page::Conversations),
            "a successful op must clear a prior page error"
        );
    }

    /// The observer-tick half of the page-error bridge (`tui.md` § The
    /// page-module contract): a send failure stamped on the manager by ANY
    /// path — not just an awaited `Op::SendThread`/`SendNewThread` — must
    /// surface on `error-message`. `inject_send_failure_for_test` mutates the
    /// manager directly (no `Op` in flight, exactly what the e2e test-agent
    /// command drives), so this is the same shape a real out-of-band failure
    /// leaves. Before this fix, `App`'s `ConversationsChanged` handler only
    /// called `clamp_focus()` — the failure was stamped but never bridged, a
    /// dropped-command shape (`../testing.md` point 10: no error, no effect).
    #[test]
    fn sync_page_error_surfaces_a_send_failure_stamped_outside_any_op() {
        let mut app = conv_app(&[("Smtp", "alice@host.test", "Q4 budget")]);
        let thread_id = app.conversations.snapshot().unwrap().threads[0]
            .thread_id
            .clone();
        apply_local(&mut app, Action::SelectThread(thread_id.0.clone()));
        assert!(
            !app.errors.contains_key(&Page::Conversations),
            "an opened thread with no send failure must start with no error"
        );

        app.conversations
            .manager
            .as_ref()
            .unwrap()
            .inject_send_failure_for_test(&thread_id, "nest rejected fauna.email.send".into());
        sync_page_error(&mut app);

        let shown = app
            .errors
            .get(&Page::Conversations)
            .expect(
                "a send_state=Failed stamp on the selected thread's compose must \
                 surface on error-message via the observer tick",
            )
            .clone();
        assert!(
            shown.contains("nest rejected fauna.email.send"),
            "the backend's own reason must reach the user, got {shown:?}"
        );
        assert!(
            !shown.contains("conversations.unified"),
            "the send reason is a LocalizedText too — resolve the key, never paint \
             it raw: {shown:?}"
        );
    }

    /// Skipped mail on `error-message`: a received record the receive path
    /// could not open under the account's key set is skipped, and the count of
    /// such records is a standing truth the manager holds
    /// (`ConversationsManager::unopenable_mail_count`), shown resolved with the
    /// count — ranked below every other page error, so a fresh failure of any
    /// gesture outranks it, and a success fold does not clear it. The record
    /// opening later (a re-drain under changed keys) retires it.
    #[test]
    fn sync_page_error_surfaces_skipped_unopenable_mail_below_every_other_error() {
        use fauna_conversations::backend::MailFeed;

        let mut app = conv_app(&[("Smtp", "alice@host.test", "Q4 budget")]);
        sync_page_error(&mut app);
        assert!(
            !app.errors.contains_key(&Page::Conversations),
            "sanity: nothing skipped shows no error"
        );

        let manager = app
            .conversations
            .manager
            .clone()
            .expect("conv_app installs a manager");
        manager.note_unopenable_mail(MailFeed::Inbox, 1);
        manager.note_unopenable_mail(MailFeed::Sent, 1);
        sync_page_error(&mut app);

        let shown = app
            .errors
            .get(&Page::Conversations)
            .expect("skipped unopenable mail must surface on error-message")
            .clone();
        assert!(
            shown.starts_with("2 ")
                && shown.contains("could not be opened")
                && !shown.contains("{count}")
                && !shown.contains("conversations.errors"),
            "resolve the key and substitute the count, never paint it raw: {shown:?}"
        );

        apply_outcome(&mut app, Outcome::Done);
        assert_eq!(
            app.errors.get(&Page::Conversations),
            Some(&shown),
            "a success fold cleared the standing skipped-mail notice"
        );

        // Every other truth outranks it: the notice is the floor, not a mask.
        let generation = manager.begin_receive_loop();
        manager.mark_receive_stopped(generation);
        sync_page_error(&mut app);
        assert_eq!(
            app.errors.get(&Page::Conversations),
            Some(&crate::wizard::key("conversations.errors.receive_stopped")),
            "a dead receive rail must outrank the skipped-mail floor"
        );
        manager.begin_receive_loop();

        // The records opening after all (a re-drain under changed keys) retire
        // their entries; the notice clears with the last one.
        manager.retire_unopenable_mail(MailFeed::Inbox, 1);
        sync_page_error(&mut app);
        assert!(
            app.errors
                .get(&Page::Conversations)
                .is_some_and(|s| s.starts_with("1 ")),
            "one retired entry leaves the other counted"
        );
        manager.retire_unopenable_mail(MailFeed::Sent, 1);
        sync_page_error(&mut app);
        assert!(
            !app.errors.contains_key(&Page::Conversations),
            "every skipped record opening retires the notice"
        );
    }

    /// The dead receive rail on `error-message`: a loop that died by panic is a
    /// standing truth the manager holds (`ConversationsManager::receive_stopped`),
    /// shown as the resolved notice — and, being standing, never cleared by the
    /// fold of an unrelated success. A later loop over the same manager retires
    /// it, which is what keeps a relaunched session from inheriting the banner.
    #[test]
    fn sync_page_error_surfaces_a_stopped_receive_rail_and_no_success_fold_clears_it() {
        let mut app = conv_app(&[("Smtp", "alice@host.test", "Q4 budget")]);
        sync_page_error(&mut app);
        assert!(
            !app.errors.contains_key(&Page::Conversations),
            "sanity: a live receive rail shows no error"
        );

        let manager = app
            .conversations
            .manager
            .clone()
            .expect("conv_app installs a manager");
        let generation = manager.begin_receive_loop();
        manager.mark_receive_stopped(generation);
        sync_page_error(&mut app);

        let shown = app
            .errors
            .get(&Page::Conversations)
            .expect("a receive loop that died by panic must surface on error-message")
            .clone();
        assert_eq!(
            shown,
            crate::wizard::key("conversations.errors.receive_stopped")
        );
        assert!(
            !shown.contains("conversations.errors"),
            "resolve the key, never paint it raw: {shown:?}"
        );

        apply_outcome(&mut app, Outcome::Done);
        assert_eq!(
            app.errors.get(&Page::Conversations),
            Some(&shown),
            "a success fold cleared the standing dead-rail notice"
        );

        manager.begin_receive_loop();
        sync_page_error(&mut app);
        assert!(
            !app.errors.contains_key(&Page::Conversations),
            "a newer receive loop over the same manager must retire the notice"
        );
    }

    /// The membership half of the same bridge: a failed add/remove/rename is
    /// stamped on the *snapshot* (not on any compose), so `sync_page_error`
    /// must read it too — otherwise the add-participant overlay closes and
    /// nothing at all appears, the dropped-command shape
    /// (`../testing.md` point 11) this track exists to close. The i18n key is
    /// resolved to real text here, so the surface is a message a user can act
    /// on rather than a raw key.
    #[test]
    fn sync_page_error_surfaces_a_failed_membership_op_from_the_snapshot() {
        let mut app = conv_app(&[("Smtp", "alice@host.test", "Q4 budget")]);
        assert!(
            !app.errors.contains_key(&Page::Conversations),
            "sanity: no error before the gesture"
        );

        app.conversations
            .manager
            .as_ref()
            .unwrap()
            .inject_page_error_for_test(fauna_core::localized::LocalizedText::key_arg(
                "conversations.unified.error_add_participant",
                "message",
                "no key package published",
            ));
        sync_page_error(&mut app);

        let shown = app
            .errors
            .get(&Page::Conversations)
            .expect("a failed membership op must surface on error-message");
        assert!(
            shown.contains("no key package published"),
            "the backend's own reason must reach the user, got {shown:?}"
        );
        assert!(
            !shown.contains("conversations.unified"),
            "the i18n key must be resolved, not painted raw: {shown:?}"
        );
    }

    /// The precedence [`sync_page_error`] must honor: a failed new-thread
    /// compose outranks a stale failure on a previously-selected thread — the
    /// same precedence linux's `detail.rs` `render()` uses.
    #[test]
    fn sync_page_error_prefers_the_new_thread_compose_over_a_selected_threads_failure() {
        let mut app = conv_app(&[("Smtp", "alice@host.test", "Q4 budget")]);
        let thread_id = app.conversations.snapshot().unwrap().threads[0]
            .thread_id
            .clone();
        let manager = app.conversations.manager.clone().unwrap();
        apply_local(&mut app, Action::SelectThread(thread_id.0.clone()));
        manager.inject_send_failure_for_test(&thread_id, "selected thread failure".into());
        sync_page_error(&mut app);
        assert!(
            app.errors
                .get(&Page::Conversations)
                .is_some_and(|e| e.contains("selected thread failure")),
            "the selected thread's failure surfaces first, resolved through i18n: {:?}",
            app.errors.get(&Page::Conversations)
        );

        apply_local(&mut app, Action::StartNewConversation);
        sync_page_error(&mut app);
        assert!(
            !app.errors.contains_key(&Page::Conversations),
            "opening a fresh (non-Failed) new-thread compose must clear the \
             stale selected-thread error, not shadow it"
        );
    }

    // ── The offline gate's conversations declarations (W4 (account-data-plane.md § Workstreams) phase 4, row 43) ──

    /// One instance of **every** [`Action`] variant, plus the extra arms of the
    /// three that carry a discriminant (both `ConfirmAddParticipant` arms, one
    /// `SendThread` per rail, and `SendNewThread`'s `None`).
    ///
    /// Hand-built for the reason the admin corpus records: walk invariants
    /// I6/I7 see only what a page *paints*, and this page paints a thread list
    /// and nothing else offline — every gesture below lives inside a thread
    /// detail, a compose bar or a modal overlay that only a loaded thread can
    /// open. A typo in any of these declarations would read as `Available`
    /// forever (`affordance`'s ruling 2).
    fn every_action() -> Vec<Action> {
        let msg = || MessageId("m1".to_string());
        let addr = || TypedAddress::Email {
            email_address: "a@b.test".to_string(),
        };
        vec![
            Action::SelectThread("t".to_string()),
            Action::SelectThreadAndMessage {
                thread_id: "t".to_string(),
                message_id: "m1".to_string(),
            },
            Action::StartNewConversation,
            Action::CancelNewConversation,
            Action::Sort,
            Action::AcceptRecipientChip,
            Action::ToggleTopic,
            Action::MarkdownWrap {
                prefix: "**",
                suffix: "**",
            },
            Action::SendNewThread {
                rail: None,
                founds_room: false,
            },
            Action::SendNewThread {
                rail: Some(Rail::FaunaMls),
                founds_room: false,
            },
            Action::SendNewThread {
                rail: Some(Rail::Smtp),
                founds_room: false,
            },
            Action::SendNewThread {
                rail: Some(Rail::FaunaMls),
                founds_room: true,
            },
            Action::ToggleHomeNest,
            Action::AcceptRoomInvitation { id: 1 },
            Action::DeclineRoomInvitation { id: 1 },
            Action::ToggleRoomNestRead,
            Action::OpenAddParticipant,
            Action::ConfirmAddParticipant {
                in_place_mls_group: true,
            },
            Action::ConfirmAddParticipant {
                in_place_mls_group: false,
            },
            Action::RemoveMember { addr: addr() },
            Action::KeepMember {
                person: fauna_core::identity::ActorId([1u8; 32]),
            },
            Action::OpenRename,
            Action::ConfirmRename,
            Action::OpenRoomSettings,
            Action::SetRoomJoinRule("invite".to_string()),
            Action::SetRoomHistoryPolicy("full".to_string()),
            Action::ToggleRoomAdmin { index: 0 },
            Action::ToggleRoomOwnerTransfer { index: 0 },
            Action::SaveRoomSettings,
            Action::ToggleRoomLabeler {
                labeler: "11".repeat(32),
            },
            Action::InspectRoomLabeler { index: 0 },
            Action::CloseRoomLabelerInspect,
            Action::StartRoomLeave,
            Action::ConfirmRoomLeave,
            Action::WithdrawRoomInvite {
                invitee_actor_hex: "11".repeat(32),
            },
            Action::ReplyToMessage {
                msg_id: msg(),
                reply_all: true,
            },
            Action::CancelReply,
            Action::RemoveReplyRecipient { addr: addr() },
            Action::CommitReplyRecipientAdd,
            Action::SendThread {
                rail: Rail::FaunaMls,
            },
            Action::SendThread { rail: Rail::Smtp },
            Action::SendThread {
                rail: Rail::Bridged,
            },
            Action::CommitAttachment,
            Action::RemoveAttachment { index: 0 },
            Action::OpenMessageActions { msg_id: msg() },
            Action::ToggleReaction {
                msg_id: msg(),
                emoji: "🦊".to_string(),
            },
            Action::StartReactionEntry,
            Action::CommitReactionEntry,
            Action::StartDeleteMessage,
            Action::ConfirmDeleteMessage,
            Action::MarkMessageSpam { msg_id: msg() },
            Action::RevealMuted { msg_id: msg() },
            Action::RevealContent { msg_id: msg() },
            Action::RevealRemoteImages { msg_id: msg() },
        ]
    }

    /// `Action` has this many variants; [`every_action`] carries one instance of
    /// each, plus 6 extra discriminant arms (2 more `SendNewThread` rails and
    /// its founding arm, 1 more `ConfirmAddParticipant`, 2 more `SendThread`
    /// rails).
    const ACTION_COUNT: usize = 48;

    #[test]
    fn every_conversations_action_is_in_the_corpus() {
        assert_eq!(
            every_action().len(),
            ACTION_COUNT + 6,
            "a new `Action` variant must be added to `every_action` — otherwise \
             its wire-kind declaration is never checked against the registry"
        );
    }

    /// I7 at the type level: every kind this page declares must be one the
    /// shared table knows. An unregistered kind reads as `Available` by design
    /// (ruling 2 — forward compatibility), so a misspelling here silently
    /// *ungates* that affordance and nothing reports it.
    #[test]
    fn every_declared_conversations_kind_is_registered() {
        crate::test_support::assert_every_wire_kind_is_registered(every_action(), |a| {
            a.wire_kind()
        });
    }

    /// The **exact kind** per gesture, not just its class. The rail-keyed sends
    /// are the point: three kinds, one class, so a class-only assertion would
    /// pass with the mail and MLS arms swapped — and the declaration's whole
    /// value is that a later reclassification of either reaches this page for
    /// free.
    /// Keep is a local succession-ledger door put the pump publishes later —
    /// no wire kind, so it stays live offline (the settings page's budget/stop
    /// precedent).
    #[test]
    fn keep_member_declares_no_wire_kind() {
        assert_eq!(
            Action::KeepMember {
                person: fauna_core::identity::ActorId([1u8; 32]),
            }
            .wire_kind(),
            None
        );
    }

    #[test]
    fn conversations_declares_the_exact_kind_per_gesture() {
        use fauna_protocol::offline_class::{OfflineClass, offline_class};
        for (action, expected, class) in [
            (
                Action::SendThread {
                    rail: Rail::FaunaMls,
                },
                "fauna.conversations.channel.send",
                OfflineClass::OfflineQueued,
            ),
            (
                Action::SendThread { rail: Rail::Smtp },
                "fauna.email.send",
                OfflineClass::OfflineQueued,
            ),
            (
                Action::SendNewThread {
                    rail: Some(Rail::FaunaMls),
                    founds_room: false,
                },
                "fauna.conversations.channel.send",
                OfflineClass::OfflineQueued,
            ),
            (
                Action::SendNewThread {
                    rail: Some(Rail::Smtp),
                    founds_room: false,
                },
                "fauna.email.send",
                OfflineClass::OfflineQueued,
            ),
            (
                Action::SendNewThread {
                    rail: Some(Rail::FaunaMls),
                    founds_room: true,
                },
                "fauna.conversations.room.create",
                OfflineClass::OnlineOnly,
            ),
            (
                Action::AcceptRoomInvitation { id: 1 },
                "fauna.conversations.room.accept_invite",
                OfflineClass::OnlineOnly,
            ),
            (
                Action::DeclineRoomInvitation { id: 1 },
                "fauna.inbox.ack",
                OfflineClass::OfflineSafe,
            ),
            (
                Action::ToggleReaction {
                    msg_id: MessageId("m1".to_string()),
                    emoji: "🦊".to_string(),
                },
                "fauna.conversations.channel.send",
                OfflineClass::OfflineQueued,
            ),
            (
                Action::ConfirmDeleteMessage,
                "fauna.conversations.channel.send",
                OfflineClass::OfflineQueued,
            ),
            (
                Action::ConfirmRename,
                "fauna.conversations.channel.send",
                OfflineClass::OfflineQueued,
            ),
            (
                Action::RemoveMember {
                    addr: TypedAddress::Email {
                        email_address: "a@b.test".to_string(),
                    },
                },
                "fauna.conversations.channel.send",
                OfflineClass::OfflineQueued,
            ),
            (
                Action::ConfirmAddParticipant {
                    in_place_mls_group: true,
                },
                "fauna.conversations.keypackage.fetch",
                OfflineClass::OnlineOnly,
            ),
            (
                Action::MarkMessageSpam {
                    msg_id: MessageId("m1".to_string()),
                },
                "fauna.bridges.put_spam_model",
                OfflineClass::OnlineOnly,
            ),
        ] {
            assert_eq!(
                action.wire_kind(),
                Some(expected),
                "{action:?} must declare {expected}"
            );
            assert_eq!(
                offline_class(expected),
                Some(class),
                "{expected} changed class — the gate's behaviour on this page \
                 changed with it, so re-read the declaration's reasoning"
            );
        }
    }

    /// **The pin that makes declaring `channel.send` honest rather than a
    /// guess.** `send_on_channel` picks `channel.send_remote` over
    /// `channel.send` when the channel is homed on another nest, and that home
    /// map is `FaunaMlsBackend`-private — no paint can see it, so every
    /// channel-application gesture above declares the local form. That is
    /// sound only while the two forms share a class. The day one is
    /// reclassified, this test reds and names the assumption instead of leaving
    /// a silently wrong gate on eight gestures.
    #[test]
    fn the_channel_send_pair_is_class_equivalent() {
        use fauna_protocol::offline_class::offline_class;
        assert_eq!(
            offline_class("fauna.conversations.channel.send"),
            offline_class("fauna.conversations.channel.send_remote"),
            "a foreign-homed channel sends via `channel.send_remote`, which no \
             paint can predict — `Action::wire_kind` declares the local form \
             for every channel-application gesture, and that is only honest \
             while the two share a class"
        );
    }

    /// A conversation is the archetypal offline surface: composing, sending
    /// on the MLS and mail rails, reacting, renaming, deleting and evicting
    /// all stay live with no nest. What greys is exactly the set of gestures
    /// whose act is the nest's own, named here one by one so that a gesture
    /// joining or leaving the set is a decision somebody reads. Asserted as
    /// **behaviour** through the shared rule rather than as kind strings.
    #[test]
    fn only_the_nest_bound_gestures_grey_offline() {
        use fauna_protocol::offline_class::{Affordance, affordance};
        let live = |action: &Action| {
            action
                .wire_kind()
                .map(|k| affordance(k, "disconnected"))
                .unwrap_or(Affordance::Available)
                == Affordance::Available
        };
        let greyed: Vec<String> = every_action()
            .into_iter()
            .filter(|a| !live(a))
            .map(|a| format!("{a:?}"))
            .collect();
        // (what the Debug form must contain, why it needs a nest)
        let expected: [(&[&str], &str); 7] = [
            (
                &["SendNewThread", "founds_room: true"],
                "a founding is the room ceremony: the room must exist on its home nest first",
            ),
            (
                &["AcceptRoomInvitation"],
                "accepting seats the account on the room's floor — the nest's act",
            ),
            (
                &["ConfirmAddParticipant", "true"],
                "the in-place add opens with a key-package fetch",
            ),
            (
                &["WithdrawRoomInvite"],
                "the invitation's envelope is consumed on the home nest",
            ),
            (
                &["ConfirmRoomLeave"],
                "the departure is the nest's self-scoped leave door",
            ),
            (
                &["SendThread", "Bridged"],
                "a bridged send is sealed to a bridge the nest names at send time",
            ),
            (
                &["MarkMessageSpam"],
                "the spam train writes through the mail bridge",
            ),
        ];
        for (needles, why) in expected {
            assert!(
                greyed.iter().any(|g| needles.iter().all(|n| g.contains(n))),
                "{needles:?} must grey offline — {why}. Got: {greyed:?}"
            );
        }
        assert_eq!(
            greyed.len(),
            expected.len(),
            "a gesture outside the named set greys offline. Got: {greyed:?}"
        );
    }

    /// The send button carries the open thread's rail — the property the
    /// exact-kind answer rests on. Driven through the real paint, because a
    /// hand-built action would prove nothing about what a press dispatches.
    #[test]
    fn the_send_button_carries_the_threads_rail() {
        let app = detail_app(&[json!({
            "rail": "Smtp",
            "sender": "carol@example.test",
            "subject": "hi",
            "body": "there",
        })]);
        let rail =
            app.page_elements()
                .into_iter()
                .find_map(|el| match (el.id.as_str(), el.gesture()) {
                    (
                        "dm-send-button",
                        Some(Gesture::Conversations(Action::SendThread { rail })),
                    ) => Some(rail),
                    _ => None,
                });
        assert_eq!(
            rail,
            Some(Rail::Smtp),
            "a mail thread's send must declare the mail rail — the gate's kind \
             comes from it"
        );
    }

    /// A governed room's mock projection: `me` owns it, every listed
    /// participant is a plain member, the policy is the initial one. Registered
    /// over the FaunaMls mock so `thread_detail().room` carries it.
    fn governed_group_app(participants: &[&str]) -> (App, ThreadId, Arc<MockRailBackend>) {
        governed_group_app_with(participants, false, false)
    }

    /// [`governed_group_app`] with the room's two notice facts set.
    fn governed_group_app_with(
        participants: &[&str],
        awaiting_key: bool,
        moderation_unverified: bool,
    ) -> (App, ThreadId, Arc<MockRailBackend>) {
        use fauna_conversations::{
            HistoryPolicy, JoinRule, PrincipalKind, RoomClass, RoomMemberSnapshot,
            RoomPolicySnapshot, RoomRole, RoomSnapshot,
        };
        let app = group_detail_app(participants);
        let Mode::Detail(thread_id) = app.conversations.mode.clone() else {
            panic!("group_detail_app opens the group");
        };
        let mock = Arc::new(MockRailBackend::new(Rail::FaunaMls));
        mock.set_room(
            thread_id.clone(),
            RoomSnapshot {
                class: RoomClass::EndToEnd,
                members: participants
                    .iter()
                    .map(|_| RoomMemberSnapshot {
                        kind: PrincipalKind::User,
                        role: Some(RoomRole::Member),
                    })
                    .collect(),
                policy: Some(RoomPolicySnapshot {
                    version: 1,
                    name: None,
                    join_rule: JoinRule::Invite,
                    history_policy: HistoryPolicy::None,
                }),
                my_role: Some(RoomRole::Owner),
                nest_read: None,
                labelers: None,
                awaiting_key,
                moderation_unverified,
                pending_invites: None,
            },
        );
        app.conversations
            .manager
            .as_ref()
            .unwrap()
            .register_backend(mock.clone());
        (app, thread_id, mock)
    }

    /// `thread-room-notice` is absent while neither room fact holds, states
    /// unverified moderation with its `state` attribute, and yields to
    /// `awaiting-key` when both hold — one room-level sentence on the header.
    #[tokio::test]
    async fn the_room_notice_is_absent_says_unverified_moderation_and_yields_to_awaiting_key() {
        use fauna_conversations::RoomNotice;
        let notice = |awaiting_key, moderation_unverified| {
            let (app, _thread_id, _mock) =
                governed_group_app_with(&["bob"], awaiting_key, moderation_unverified);
            let notices: Vec<Element> = elements(&app)
                .into_iter()
                .filter(|e| e.id == ids::THREAD_ROOM_NOTICE)
                .collect();
            assert!(notices.len() <= 1, "one notice at most");
            notices.into_iter().next().map(|e| {
                let state = e
                    .attrs
                    .iter()
                    .find(|(k, _)| k == "state")
                    .map(|(_, v)| v.clone())
                    .expect("the notice carries its state");
                (state, e.text.clone())
            })
        };
        assert_eq!(notice(false, false), None, "no fact, no notice");
        assert_eq!(
            notice(false, true),
            Some((
                "moderation-unverified".to_string(),
                RoomNotice::ModerationUnverified.label().to_string()
            ))
        );
        assert_eq!(
            notice(true, false),
            Some((
                "awaiting-key".to_string(),
                RoomNotice::AwaitingKey.label().to_string()
            ))
        );
        assert_eq!(
            notice(true, true).map(|(state, _)| state),
            Some("awaiting-key".to_string()),
            "awaiting-key wins when both hold"
        );
    }

    /// The header states the class with its driver attribute and offers the
    /// editor's door live for the owner; opening it paints the two token
    /// pickers, one admin switch per participant (live: the owner may appoint)
    /// and Save; the pickers stage by token, the switch flips, and Save hands
    /// back exactly the changed values as one op — which lands them on the
    /// rail and closes the editor.
    #[tokio::test]
    async fn room_settings_editor_stages_saves_and_closes() {
        use fauna_conversations::{HistoryPolicy, JoinRule, RoomRole};
        let (mut app, _thread_id, _mock) = governed_group_app(&["bob", "carol"]);
        let header = elements(&app);
        let class = header
            .iter()
            .find(|e| e.id == ids::THREAD_ROOM_CLASS)
            .expect("the header states the class");
        assert_eq!(
            class
                .attrs
                .iter()
                .find(|(k, _)| k == "class")
                .map(|(_, v)| v.as_str()),
            Some("end-to-end")
        );
        let door = header
            .iter()
            .find(|e| e.id == ids::THREAD_ROOM_SETTINGS_BUTTON)
            .expect("the editor's door is painted on a room");
        assert!(door.enabled, "the owner may set policy");
        assert_eq!(
            header
                .iter()
                .filter(|e| e.id == ids::THREAD_MEMBER_CHIP)
                .filter_map(|e| e
                    .attrs
                    .iter()
                    .find(|(k, _)| k == "role")
                    .map(|(_, v)| v.clone()))
                .collect::<Vec<_>>(),
            vec!["member".to_string(), "member".to_string()]
        );

        assert!(apply_local(&mut app, Action::OpenRoomSettings).is_none());
        assert!(detail_overlay_open(&app.conversations));
        let open = ids(&app);
        for id in [
            ids::ROOM_JOIN_RULE_SELECT,
            ids::ROOM_HISTORY_POLICY_SELECT,
            ids::ROOM_ADMIN_TOGGLE,
            ids::ROOM_SETTINGS_SAVE_BUTTON,
        ] {
            assert!(open.contains(id), "{id} paints in the editor");
        }
        assert!(
            !open.contains(ids::DM_MESSAGE_TEXT),
            "the editor replaces the read view"
        );
        let toggles: Vec<Element> = elements(&app)
            .into_iter()
            .filter(|e| e.id == ids::ROOM_ADMIN_TOGGLE)
            .collect();
        assert_eq!(toggles.len(), 2, "one switch per participant");
        assert!(
            toggles.iter().all(|t| t.enabled),
            "the owner may appoint either member"
        );
        assert!(toggles.iter().all(|t| {
            t.attrs
                .contains(&("checked".to_string(), "false".to_string()))
        }));

        apply_local(&mut app, Action::SetRoomHistoryPolicy("full".to_string()));
        apply_local(
            &mut app,
            Action::SetRoomJoinRule("no-such-rule".to_string()),
        );
        apply_local(&mut app, Action::ToggleRoomAdmin { index: 1 });
        let draft = app.conversations.room_settings.as_ref().unwrap();
        assert_eq!(draft.history_policy, HistoryPolicy::Full);
        assert_eq!(
            draft.join_rule,
            JoinRule::Invite,
            "an unoffered token stages nothing"
        );
        assert_eq!(draft.admins, vec![false, true]);
        assert!(
            elements(&app)
                .iter()
                .filter(|e| e.id == ids::ROOM_ADMIN_TOGGLE)
                .nth(1)
                .unwrap()
                .attrs
                .contains(&("checked".to_string(), "true".to_string()))
        );

        let op = apply_local(&mut app, Action::SaveRoomSettings).expect("changes staged → an op");
        let Op::SaveRoomSettings { edits, .. } = &op else {
            panic!("expected a SaveRoomSettings op");
        };
        assert_eq!(
            edits.len(),
            2,
            "only the changed values are committed: {edits:?}"
        );
        assert!(matches!(
            edits[0],
            RoomSettingsEdit::HistoryPolicy {
                policy: HistoryPolicy::Full
            }
        ));
        assert!(
            matches!(&edits[1], RoomSettingsEdit::Appoint { address: TypedAddress::Fauna { handle, .. } } if handle == "carol")
        );
        assert!(
            app.conversations.room_settings.is_some(),
            "the editor stays open until the commits land"
        );
        let outcome = op.run().await;
        assert!(
            matches!(outcome, Outcome::RoomSettingsSaved(true)),
            "{outcome:?}"
        );
        apply_outcome(&mut app, outcome);
        assert!(
            app.conversations.room_settings.is_none(),
            "every commit landed → closed"
        );
        let detail = app
            .conversations
            .manager
            .as_ref()
            .unwrap()
            .thread_detail(_thread_id.clone())
            .unwrap();
        let room = detail.room.expect("still a room");
        assert_eq!(room.policy.unwrap().history_policy, HistoryPolicy::Full);
        assert_eq!(room.members[1].role, Some(RoomRole::Admin));
        assert_eq!(room.members[0].role, Some(RoomRole::Member));
    }

    /// **The pending section.** Absent while the list is `None` (not served)
    /// or empty; otherwise one row per served invitation, between the
    /// per-participant controls and Save, with the shared sentence, its
    /// `lapsed` attribute and a live withdraw button on every row — no
    /// capability is consulted, the nest served the row because this viewer
    /// may withdraw it. Pressing one hands back the manager door at once and
    /// leaves the editor open: a withdrawal is not a policy edit.
    #[tokio::test]
    async fn the_room_editor_lists_pending_invitations_and_withdraws_one_at_once() {
        use fauna_conversations::{
            HistoryPolicy, JoinRule, PrincipalKind, RoomClass, RoomMemberSnapshot,
            RoomPendingInviteSnapshot, RoomPolicySnapshot, RoomRole, RoomSnapshot,
        };
        let (mut app, thread_id, mock) = governed_group_app(&["bob"]);
        let room = |pending| RoomSnapshot {
            class: RoomClass::Community,
            members: vec![
                RoomMemberSnapshot {
                    kind: PrincipalKind::User,
                    role: Some(RoomRole::Owner),
                },
                RoomMemberSnapshot {
                    kind: PrincipalKind::User,
                    role: Some(RoomRole::Member),
                },
            ],
            policy: Some(RoomPolicySnapshot {
                version: 1,
                name: None,
                join_rule: JoinRule::Invite,
                history_policy: HistoryPolicy::None,
            }),
            my_role: Some(RoomRole::Owner),
            nest_read: None,
            labelers: None,
            awaiting_key: false,
            moderation_unverified: false,
            pending_invites: pending,
        };
        let carol = "11".repeat(32);
        let pending_row =
            |invitee: &str, display: &str, inviter: &str, role, lapsed| RoomPendingInviteSnapshot {
                invitee_actor_hex: invitee.to_string(),
                invitee_display: display.to_string(),
                inviter_display: inviter.to_string(),
                role,
                invited_at_ms: 1,
                lapsed,
            };

        assert!(apply_local(&mut app, Action::OpenRoomSettings).is_none());
        assert!(
            !ids(&app).contains(ids::ROOM_PENDING_INVITE),
            "a list the nest has not served paints nothing"
        );
        mock.set_room(thread_id.clone(), room(Some(Vec::new())));
        assert!(
            !ids(&app).contains(ids::ROOM_PENDING_INVITE)
                && !app
                    .page_elements()
                    .iter()
                    .any(|e| e.text == conversations::unified::ROOM_PENDING_INVITES_LABEL),
            "an empty list paints no section either, heading included"
        );

        mock.set_room(
            thread_id.clone(),
            room(Some(vec![
                pending_row(
                    &carol,
                    "carol@home.test",
                    "me@home.test",
                    RoomRole::Member,
                    false,
                ),
                pending_row(
                    &"22".repeat(32),
                    "dan@home.test",
                    "bob@home.test",
                    RoomRole::Admin,
                    true,
                ),
            ])),
        );
        let view = elements(&app);
        let rows: Vec<_> = view
            .iter()
            .filter(|e| e.id == ids::ROOM_PENDING_INVITE)
            .collect();
        assert_eq!(rows.len(), 2, "one row per served invitation");
        assert_eq!(rows[0].text, "carol@home.test — invited by me@home.test");
        assert_eq!(
            rows[0].attrs,
            vec![("lapsed".to_string(), "false".to_string())]
        );
        assert_eq!(
            rows[1].text,
            "dan@home.test — invited by bob@home.test as an admin (can no longer be accepted)"
        );
        assert_eq!(
            rows[1].attrs,
            vec![("lapsed".to_string(), "true".to_string())]
        );
        let buttons: Vec<_> = view
            .iter()
            .filter(|e| e.id == ids::ROOM_PENDING_INVITE_WITHDRAW_BUTTON)
            .collect();
        assert_eq!(buttons.len(), 2, "a withdraw beside every row");
        assert!(
            buttons.iter().all(|b| b.enabled),
            "live on every row — the nest served it because this viewer may withdraw it"
        );
        assert!(
            matches!(
                buttons[0].gesture(),
                Some(Gesture::Conversations(Action::WithdrawRoomInvite { invitee_actor_hex }))
                    if invitee_actor_hex == carol
            ),
            "each button names its own row's invitee"
        );
        let position = |id: &str| {
            view.iter()
                .position(|e| e.id == id)
                .unwrap_or_else(|| panic!("{id} is painted"))
        };
        assert!(
            position(ids::ROOM_ADMIN_TOGGLE) < position(ids::ROOM_PENDING_INVITE)
                && position(ids::ROOM_PENDING_INVITE) < position(ids::ROOM_SETTINGS_SAVE_BUTTON),
            "the section sits between the per-participant controls and Save"
        );

        let op = apply_local(
            &mut app,
            Action::WithdrawRoomInvite {
                invitee_actor_hex: carol.clone(),
            },
        )
        .expect("the press hands back the withdrawal at once");
        assert!(
            matches!(
                &op,
                Op::WithdrawRoomInvite { thread_id: t, invitee_actor_hex: i, .. }
                    if *t == thread_id && *i == carol
            ),
            "one door on the open thread, naming the row's invitee"
        );
        assert!(
            detail_overlay_open(&app.conversations)
                && ids(&app).contains(ids::ROOM_SETTINGS_SAVE_BUTTON),
            "the editor stays open — a withdrawal is not one of Save's staged edits"
        );
    }

    /// **The walk-out.** A plain member gets it live; the owner gets it greyed
    /// rather than hidden, because the remedy is to hand the room over first
    /// (`conversation-rooms.md` § Roles and authorization → *Leaving — the
    /// mechanism*). The confirm is painted only once the button asks for it,
    /// and confirming hands back the departure op and closes the editor — it
    /// is never staged through Save, because leaving is an immediate act
    /// rather than a policy edit.
    #[tokio::test]
    async fn the_room_editor_offers_a_walk_out_that_the_owner_must_hand_over_before() {
        use fauna_conversations::{
            HistoryPolicy, JoinRule, PrincipalKind, RoomClass, RoomMemberSnapshot,
            RoomPolicySnapshot, RoomRole, RoomSnapshot,
        };
        let (mut app, thread_id, mock) = governed_group_app(&["bob", "carol"]);

        // The owner first: the control is painted and dead.
        assert!(apply_local(&mut app, Action::OpenRoomSettings).is_none());
        let owner_view = elements(&app);
        let leave = owner_view
            .iter()
            .find(|e| e.id == ids::ROOM_LEAVE_BUTTON)
            .expect("the walk-out is painted for the owner too — greyed, never hidden");
        assert!(
            !leave.enabled,
            "a room is never owner-less: the owner hands it over first"
        );
        assert!(
            apply_local(&mut app, Action::StartRoomLeave).is_none()
                && !ids(&app).contains(ids::ROOM_LEAVE_CONFIRM),
            "and the confirm never opens off a dead control"
        );

        // Now the same room seen by a plain member.
        mock.set_room(
            thread_id.clone(),
            RoomSnapshot {
                class: RoomClass::EndToEnd,
                members: vec![
                    RoomMemberSnapshot {
                        kind: PrincipalKind::User,
                        role: Some(RoomRole::Owner),
                    },
                    RoomMemberSnapshot {
                        kind: PrincipalKind::User,
                        role: Some(RoomRole::Member),
                    },
                ],
                policy: Some(RoomPolicySnapshot {
                    version: 1,
                    name: None,
                    join_rule: JoinRule::Invite,
                    history_policy: HistoryPolicy::None,
                }),
                my_role: Some(RoomRole::Member),
                nest_read: None,
                labelers: None,
                awaiting_key: false,
                moderation_unverified: false,
                pending_invites: None,
            },
        );

        let member_view = elements(&app);
        assert!(
            member_view
                .iter()
                .find(|e| e.id == ids::ROOM_LEAVE_BUTTON)
                .expect("still painted")
                .enabled,
            "an ordinary member may walk out — the one room verb that is              neither the owner's nor an admin's"
        );
        assert!(
            !ids(&app).contains(ids::ROOM_LEAVE_CONFIRM),
            "the confirm is not painted until it is asked for"
        );

        assert!(apply_local(&mut app, Action::StartRoomLeave).is_none());
        assert!(
            ids(&app).contains(ids::ROOM_LEAVE_CONFIRM),
            "the button opens the confirm"
        );
        assert!(
            ids(&app).contains(ids::ROOM_SETTINGS_SAVE_BUTTON),
            "and Save is still there — the departure is not one of its staged edits"
        );

        let op = apply_local(&mut app, Action::ConfirmRoomLeave)
            .expect("confirming hands back the departure");
        assert!(
            matches!(&op, Op::LeaveRoom { thread_id: t, .. } if *t == thread_id),
            "one verb on the open thread; the room's class picks the door              beneath it in shared Rust, never here"
        );
        assert!(
            !detail_overlay_open(&app.conversations),
            "the editor closes with the gesture"
        );
        assert!(
            !app.conversations.room_leave_confirm,
            "and the confirm does not survive it"
        );
    }

    /// The hand-over control: one per participant, live for the owner, at
    /// most one row staged (staging a second un-stages the first, staging the
    /// same row again clears it), and Save issues it LAST — after an
    /// appointment staged in the same edit — since the seat is a plain member
    /// once it lands. The mock completes the ceremony in place, so the
    /// projection reads the new owner as owner and this seat as member.
    #[tokio::test]
    async fn room_settings_editor_stages_a_hand_over_last() {
        use fauna_conversations::RoomRole;
        let (mut app, thread_id, _mock) = governed_group_app(&["bob", "carol"]);
        apply_local(&mut app, Action::OpenRoomSettings);
        let buttons: Vec<Element> = elements(&app)
            .into_iter()
            .filter(|e| e.id == ids::ROOM_OWNER_TRANSFER_BUTTON)
            .collect();
        assert_eq!(buttons.len(), 2, "one hand-over control per participant");
        assert!(
            buttons.iter().all(|b| b.enabled),
            "the owner may hand the room to either member"
        );
        assert!(buttons.iter().all(|b| {
            b.attrs
                .contains(&("checked".to_string(), "false".to_string()))
        }));

        apply_local(&mut app, Action::ToggleRoomOwnerTransfer { index: 0 });
        apply_local(&mut app, Action::ToggleRoomOwnerTransfer { index: 1 });
        assert_eq!(
            app.conversations
                .room_settings
                .as_ref()
                .unwrap()
                .transfer_to,
            Some(1),
            "staging a second row un-stages the first"
        );
        let checked: Vec<String> = elements(&app)
            .iter()
            .filter(|e| e.id == ids::ROOM_OWNER_TRANSFER_BUTTON)
            .filter_map(|e| {
                e.attrs
                    .iter()
                    .find(|(k, _)| k == "checked")
                    .map(|(_, v)| v.clone())
            })
            .collect();
        assert_eq!(checked, vec!["false".to_string(), "true".to_string()]);
        apply_local(&mut app, Action::ToggleRoomOwnerTransfer { index: 1 });
        assert_eq!(
            app.conversations
                .room_settings
                .as_ref()
                .unwrap()
                .transfer_to,
            None,
            "staging the same row again clears it"
        );

        // An appointment of bob and the hand-over to carol, in one Save.
        apply_local(&mut app, Action::ToggleRoomAdmin { index: 0 });
        apply_local(&mut app, Action::ToggleRoomOwnerTransfer { index: 1 });
        let op = apply_local(&mut app, Action::SaveRoomSettings).expect("changes staged → an op");
        let Op::SaveRoomSettings { edits, .. } = &op else {
            panic!("expected a SaveRoomSettings op");
        };
        assert_eq!(edits.len(), 2, "{edits:?}");
        assert!(
            matches!(&edits[0], RoomSettingsEdit::Appoint { address: TypedAddress::Fauna { handle, .. } } if handle == "bob")
        );
        assert!(
            matches!(&edits[1], RoomSettingsEdit::TransferOwnership { address: TypedAddress::Fauna { handle, .. } } if handle == "carol"),
            "the hand-over is issued last: {edits:?}"
        );
        let outcome = op.run().await;
        assert!(
            matches!(outcome, Outcome::RoomSettingsSaved(true)),
            "{outcome:?}"
        );
        apply_outcome(&mut app, outcome);
        assert!(app.conversations.room_settings.is_none());
        let detail = app
            .conversations
            .manager
            .as_ref()
            .unwrap()
            .thread_detail(thread_id)
            .unwrap();
        let room = detail.room.expect("still a room");
        assert_eq!(room.members[0].role, Some(RoomRole::Admin));
        assert_eq!(room.members[1].role, Some(RoomRole::Owner));
        assert_eq!(
            room.my_role,
            Some(RoomRole::Member),
            "the seat that handed the room over is a plain member now"
        );
        assert!(
            !detail.capabilities.can_transfer_ownership && !detail.capabilities.can_set_policy,
            "and holds no owner-only capability: {:?}",
            detail.capabilities
        );
    }

    /// Save with nothing staged is a close, not a commit; Esc (the overlay
    /// cancel) closes too; and a thread switch drops the draft.
    #[test]
    fn room_settings_editor_closes_without_a_commit_when_nothing_changed() {
        let (mut app, thread_id, _mock) = governed_group_app(&["bob"]);
        apply_local(&mut app, Action::OpenRoomSettings);
        assert!(apply_local(&mut app, Action::SaveRoomSettings).is_none());
        assert!(app.conversations.room_settings.is_none());
        apply_local(&mut app, Action::OpenRoomSettings);
        cancel_detail_overlay(&mut app.conversations);
        assert!(app.conversations.room_settings.is_none());
        apply_local(&mut app, Action::OpenRoomSettings);
        apply_local(&mut app, Action::SelectThread(thread_id.0.clone()));
        assert!(app.conversations.room_settings.is_none());
    }

    /// **The room's labeler set paints over the catalog, filtered and marked
    /// by shared Rust.** A community room whose set names one `wasm` labeler:
    /// the editor paints one toggle + one inspect button per catalog row a
    /// room may name (the `list` row is left out), `checked` off the staged
    /// set, each inspect carrying the catalog SNAPSHOT index (so the skipped
    /// row shifts nothing); a toggle stages by id and Save carries the whole
    /// set as one edit; the catalog's inspect view paints in place, its close
    /// wired back to this editor. An end-to-end room paints no section.
    #[test]
    fn room_settings_editor_stages_a_community_rooms_labeler_set() {
        use fauna_conversations::{
            HistoryPolicy, JoinRule, PrincipalKind, RoomClass, RoomMemberSnapshot,
            RoomPolicySnapshot, RoomRole, RoomSnapshot,
        };
        use fauna_labeler_catalog_machine::{
            LabelerCatalogEntry, LabelerCatalogSnapshot, LabelerInspectView,
        };
        let (mut app, thread_id, mock) = governed_group_app(&["bob"]);
        let (wasm, list, model) = ("1a".repeat(32), "2b".repeat(32), "3c".repeat(32));
        let entry = |id: &str, kind: &str| LabelerCatalogEntry {
            labeler_id: id.to_string(),
            artifact_kind: kind.to_string(),
            ..Default::default()
        };
        app.settings.labeler_catalog.snapshot = Some(LabelerCatalogSnapshot {
            entries: vec![
                entry(&wasm, "wasm"),
                entry(&list, "list"),
                entry(&model, "text-model"),
            ],
            inspecting: None,
            error: None,
            loaded: true,
        });

        // An end-to-end room has no set to stage: no section.
        apply_local(&mut app, Action::OpenRoomSettings);
        assert!(!ids(&app).contains(ids::ROOM_LABELER_TOGGLE));
        cancel_detail_overlay(&mut app.conversations);

        mock.set_room(
            thread_id.clone(),
            RoomSnapshot {
                class: RoomClass::Community,
                members: vec![RoomMemberSnapshot {
                    kind: PrincipalKind::User,
                    role: Some(RoomRole::Member),
                }],
                policy: Some(RoomPolicySnapshot {
                    version: 1,
                    name: None,
                    join_rule: JoinRule::Invite,
                    history_policy: HistoryPolicy::None,
                }),
                my_role: Some(RoomRole::Owner),
                nest_read: Some(true),
                labelers: Some(vec![wasm.clone()]),
                awaiting_key: false,
                moderation_unverified: false,
                pending_invites: None,
            },
        );
        apply_local(&mut app, Action::OpenRoomSettings);
        let toggles: Vec<Element> = elements(&app)
            .into_iter()
            .filter(|e| e.id == ids::ROOM_LABELER_TOGGLE)
            .collect();
        assert_eq!(toggles.len(), 2, "wasm and text-model rows; never the list");
        let checked = |e: &Element| {
            e.attrs
                .iter()
                .find(|(k, _)| k == "checked")
                .map(|(_, v)| v.clone())
        };
        assert_eq!(checked(&toggles[0]).as_deref(), Some("true"));
        assert_eq!(checked(&toggles[1]).as_deref(), Some("false"));
        assert!(
            toggles.iter().all(|t| t.enabled),
            "the owner may name labelers"
        );
        let inspects: Vec<Option<u32>> = elements(&app)
            .into_iter()
            .filter(|e| e.id == ids::ROOM_LABELER_INSPECT_BUTTON)
            .map(|e| match e.gesture() {
                Some(Gesture::Conversations(Action::InspectRoomLabeler { index })) => Some(index),
                _ => None,
            })
            .collect();
        assert_eq!(
            inspects,
            vec![Some(0), Some(2)],
            "each inspect addresses the catalog's own row"
        );

        let Some(Gesture::Conversations(toggle_model)) = toggles[1].gesture() else {
            panic!("the toggle dispatches a conversations gesture");
        };
        apply_local(&mut app, toggle_model);
        let flipped: Vec<Element> = elements(&app)
            .into_iter()
            .filter(|e| e.id == ids::ROOM_LABELER_TOGGLE)
            .collect();
        assert_eq!(checked(&flipped[1]).as_deref(), Some("true"));
        let op = apply_local(&mut app, Action::SaveRoomSettings).expect("a staged set → an op");
        let Op::SaveRoomSettings { edits, .. } = &op else {
            panic!("expected a SaveRoomSettings op");
        };
        assert_eq!(
            edits,
            &vec![RoomSettingsEdit::Labelers {
                labelers: vec![wasm.clone(), model.clone()],
            }],
            "the whole staged set, as one edit"
        );

        app.settings
            .labeler_catalog
            .snapshot
            .as_mut()
            .unwrap()
            .inspecting = Some(LabelerInspectView {
            labeler_id: model.clone(),
            version: 1,
            artifact_kind: "text-model".to_string(),
            wasm_hash: String::new(),
            wasm_size: 0,
            needs_text: true,
            needs_hashtags: false,
            needs_media_metadata: false,
            needs_author: false,
            needs_attachment_bytes: false,
            verified: true,
            list_name: None,
            list_entries: vec![],
            model_name: None,
            model_ngrams: vec![],
        });
        let open = elements(&app);
        for id in [
            ids::LABELER_INSPECT_PANEL,
            ids::LABELER_INSPECT_METADATA,
            ids::LABELER_INSPECT_MODEL_NAME,
            ids::ROOM_SETTINGS_SAVE_BUTTON,
        ] {
            assert!(open.iter().any(|e| e.id == id), "{id} paints in the editor");
        }
        assert!(
            matches!(
                open.iter()
                    .find(|e| e.id == ids::LABELER_INSPECT_CLOSE_BUTTON)
                    .and_then(Element::gesture),
                Some(Gesture::Conversations(Action::CloseRoomLabelerInspect))
            ),
            "the view's close returns to this editor"
        );
    }

    /// A plain member reads the set and may inspect, but every toggle is greyed
    /// and a raced toggle stages nothing.
    #[test]
    fn a_plain_member_reads_the_labeler_set_greyed() {
        use fauna_conversations::{
            HistoryPolicy, JoinRule, PrincipalKind, RoomClass, RoomMemberSnapshot,
            RoomPolicySnapshot, RoomRole, RoomSnapshot,
        };
        use fauna_labeler_catalog_machine::{LabelerCatalogEntry, LabelerCatalogSnapshot};
        let (mut app, thread_id, mock) = governed_group_app(&["bob"]);
        let wasm = "1a".repeat(32);
        app.settings.labeler_catalog.snapshot = Some(LabelerCatalogSnapshot {
            entries: vec![LabelerCatalogEntry {
                labeler_id: wasm.clone(),
                artifact_kind: "wasm".to_string(),
                ..Default::default()
            }],
            inspecting: None,
            error: None,
            loaded: true,
        });
        mock.set_room(
            thread_id,
            RoomSnapshot {
                class: RoomClass::Community,
                members: vec![RoomMemberSnapshot {
                    kind: PrincipalKind::User,
                    role: Some(RoomRole::Owner),
                }],
                policy: Some(RoomPolicySnapshot {
                    version: 1,
                    name: None,
                    join_rule: JoinRule::Invite,
                    history_policy: HistoryPolicy::None,
                }),
                my_role: Some(RoomRole::Member),
                nest_read: Some(true),
                labelers: Some(vec![]),
                awaiting_key: false,
                moderation_unverified: false,
                pending_invites: None,
            },
        );
        apply_local(&mut app, Action::OpenRoomSettings);
        let painted = elements(&app);
        let toggle = painted
            .iter()
            .find(|e| e.id == ids::ROOM_LABELER_TOGGLE)
            .expect("a member reads the set");
        assert!(!toggle.enabled, "greyed for a member");
        assert!(
            painted
                .iter()
                .find(|e| e.id == ids::ROOM_LABELER_INSPECT_BUTTON)
                .is_some_and(|e| e.enabled),
            "inspecting is a read any member may make"
        );
        apply_local(&mut app, Action::ToggleRoomLabeler { labeler: wasm });
        assert_eq!(
            app.conversations
                .room_settings
                .as_ref()
                .and_then(|d| d.labelers.clone()),
            Some(vec![]),
            "a greyed control stages nothing"
        );
    }

    /// The new-thread composer states the class of the room about to be
    /// created once a chip is committed — a mail chip means the conversation
    /// rides a bridge, so the statement is transport-only; nothing paints
    /// before the first chip.
    #[test]
    fn compose_states_the_prospective_room_class_once_a_chip_is_committed() {
        let mut app = conv_app(&[]);
        apply_local(&mut app, Action::StartNewConversation);
        assert!(!ids(&app).contains(ids::RECIPIENT_PICKER_CLASS));
        // Commit the chip the way the async resolve would (`accept_current_
        // recipient_chip` takes the RESOLVED address, which needs the probe).
        app.conversations
            .manager
            .clone()
            .unwrap()
            .accept_new_thread_chip(TypedAddress::Email {
                email_address: "someone@example.org".to_string(),
            });
        let class = elements(&app)
            .into_iter()
            .find(|e| e.id == ids::RECIPIENT_PICKER_CLASS)
            .expect("a committed chip paints the class");
        assert_eq!(
            class
                .attrs
                .iter()
                .find(|(k, _)| k == "class")
                .map(|(_, v)| v.as_str()),
            Some("transport-only")
        );
        assert_eq!(
            class.text,
            conversations::unified::ROOM_CLASS_TRANSPORT_ONLY
        );
    }

    fn attr<'a>(el: &'a Element, key: &str) -> Option<&'a str> {
        el.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn one(app: &App, id: &str) -> Element {
        elements(app)
            .into_iter()
            .find(|e| e.id == id)
            .unwrap_or_else(|| panic!("{id} is painted"))
    }

    /// The composer paints the home-nest choice before any chip; turning it on
    /// with a Fauna chip committed states the community class, and the send it
    /// arms declares the founding ceremony's kind — a founding needs a nest,
    /// where an ordinary first send only queues an append.
    #[test]
    fn the_home_nest_toggle_arms_a_community_founding() {
        let mut app = conv_app(&[]);
        apply_local(&mut app, Action::StartNewConversation);
        let toggle = one(&app, ids::RECIPIENT_PICKER_HOME_NEST_TOGGLE);
        assert_eq!(attr(&toggle, "checked"), Some("false"));
        assert!(toggle.enabled);

        app.conversations
            .manager
            .clone()
            .unwrap()
            .accept_new_thread_chip(TypedAddress::Fauna {
                handle: "bob@home.test".to_string(),
                actor_id: fauna_core::identity::ActorId([2u8; 32]),
            });
        assert_eq!(
            attr(&one(&app, ids::RECIPIENT_PICKER_CLASS), "class"),
            Some("end-to-end"),
            "off, a Fauna chip makes an end-to-end room"
        );

        assert!(apply_local(&mut app, Action::ToggleHomeNest).is_none());
        let toggle = one(&app, ids::RECIPIENT_PICKER_HOME_NEST_TOGGLE);
        assert_eq!(attr(&toggle, "checked"), Some("true"));
        assert_eq!(toggle.text, conversations::unified::ROOM_HOME_NEST_YES);
        assert_eq!(
            attr(&one(&app, ids::RECIPIENT_PICKER_CLASS), "class"),
            Some("community")
        );
        let send = one(&app, ids::DM_SEND_BUTTON);
        let Some(Gesture::Conversations(action @ Action::SendNewThread { founds_room, .. })) =
            send.gesture()
        else {
            panic!("the composer's send carries SendNewThread");
        };
        assert!(founds_room, "the paint saw a founding");
        assert_eq!(action.wire_kind(), Some("fauna.conversations.room.create"));

        apply_local(&mut app, Action::ToggleHomeNest);
        assert_eq!(
            attr(
                &one(&app, ids::RECIPIENT_PICKER_HOME_NEST_TOGGLE),
                "checked"
            ),
            Some("false"),
            "the toggle turns back off"
        );
    }

    fn invitation(
        id: i64,
        role: fauna_conversations::RoomRole,
    ) -> fauna_conversations::room::RoomInvitation {
        fauna_conversations::room::RoomInvitation {
            id,
            room_id: [id as u8; 32],
            inviter: fauna_core::identity::ActorId([9u8; 32]),
            role,
            policy_version: 1,
            room_node: None,
        }
    }

    /// Standing invitations paint atop the list, one row with its own
    /// accept/decline pair each; accepting opens the room's thread and the row
    /// goes, and declining just drops its row.
    #[tokio::test]
    async fn standing_invitations_paint_atop_the_list_and_accept_opens_the_room() {
        use fauna_conversations::RoomRole;
        let mut app = conv_app(&[]);
        let manager = app.conversations.manager.clone().unwrap();
        let mock = Arc::new(MockRailBackend::new(Rail::FaunaMls));
        mock.set_room_invitations(vec![
            invitation(1, RoomRole::Member),
            invitation(2, RoomRole::Admin),
        ]);
        manager.register_backend(mock.clone());
        assert!(
            !ids(&app).contains(ids::ROOM_INVITATION),
            "nothing stands yet"
        );
        manager.refresh_room_invitations().await;

        let painted = elements(&app);
        let rows: Vec<&Element> = painted
            .iter()
            .filter(|e| e.id == ids::ROOM_INVITATION)
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(attr(rows[0], "role"), Some("member"));
        assert_eq!(attr(rows[1], "role"), Some("admin"));
        assert!(rows[1].text.ends_with("invited you to a room as an admin"));
        for id in [
            ids::ROOM_INVITATION_ACCEPT_BUTTON,
            ids::ROOM_INVITATION_DECLINE_BUTTON,
        ] {
            assert_eq!(
                painted.iter().filter(|e| e.id == id).count(),
                2,
                "{id} per row"
            );
        }
        let first_row = painted
            .iter()
            .position(|e| e.id == ids::ROOM_INVITATION)
            .unwrap();
        let new_button = painted
            .iter()
            .position(|e| e.id == ids::NEW_CONVERSATION_BUTTON)
            .unwrap();
        assert!(
            new_button < first_row,
            "atop the threads, below the list's own controls"
        );

        let accept = Action::AcceptRoomInvitation { id: 1 };
        assert_eq!(
            accept.wire_kind(),
            Some("fauna.conversations.room.accept_invite")
        );
        let op = apply_local(&mut app, accept).expect("accepting is async");
        let outcome = op.run().await;
        let Outcome::RoomJoined(Some(thread_id)) = &outcome else {
            panic!("the accept opens the room: {outcome:?}");
        };
        let thread_id = thread_id.clone();
        apply_outcome(&mut app, outcome);
        assert_eq!(app.conversations.mode, Mode::Detail(thread_id));

        app.conversations.mode = Mode::List;
        let rows = elements(&app)
            .into_iter()
            .filter(|e| e.id == ids::ROOM_INVITATION)
            .count();
        assert_eq!(rows, 1, "the accepted invitation stops standing");

        let op = apply_local(&mut app, Action::DeclineRoomInvitation { id: 2 }).expect("async");
        apply_outcome(&mut app, op.run().await);
        assert!(!ids(&app).contains(ids::ROOM_INVITATION), "declined, gone");
        assert!(app.errors.get(&crate::pages::Page::Conversations).is_none());
    }

    /// The editor paints the home nest's read only where there is one to
    /// stage, greyed for a seat that may not set policy; it stages like every
    /// other control and Save carries it.
    #[test]
    fn the_nest_read_toggle_paints_only_on_a_read_and_stages_for_save() {
        let (mut app, thread_id, mock) = governed_group_app(&["bob"]);
        apply_local(&mut app, Action::OpenRoomSettings);
        assert!(
            !ids(&app).contains(ids::ROOM_NEST_READ_TOGGLE),
            "an end-to-end room has no nest read to stage"
        );

        let manager = app.conversations.manager.clone().unwrap();
        let mut room = manager
            .thread_detail(thread_id.clone())
            .and_then(|d| d.room)
            .expect("a governed room");
        room.class = fauna_conversations::RoomClass::Community;
        room.nest_read = Some(true);
        mock.set_room(thread_id.clone(), room);
        apply_local(&mut app, Action::OpenRoomSettings);

        let toggle = one(&app, ids::ROOM_NEST_READ_TOGGLE);
        assert_eq!(attr(&toggle, "checked"), Some("true"));
        assert!(toggle.enabled, "the owner may set policy");
        let order: Vec<String> = elements(&app).into_iter().map(|e| e.id).collect();
        let at = |id: &str| order.iter().position(|e| e == id).unwrap();
        assert!(at(ids::ROOM_NEST_READ_TOGGLE) < at(ids::ROOM_SETTINGS_SAVE_BUTTON));

        assert!(apply_local(&mut app, Action::ToggleRoomNestRead).is_none());
        let toggle = one(&app, ids::ROOM_NEST_READ_TOGGLE);
        assert_eq!(attr(&toggle, "checked"), Some("false"));
        assert_eq!(toggle.text, conversations::unified::ROOM_NEST_READ_NO);
        let Some(Op::SaveRoomSettings { edits, .. }) =
            apply_local(&mut app, Action::SaveRoomSettings)
        else {
            panic!("a staged read is a commit");
        };
        assert!(
            edits
                .iter()
                .any(|e| matches!(e, RoomSettingsEdit::NestRead { reads: false })),
            "{edits:?}"
        );
    }
}
