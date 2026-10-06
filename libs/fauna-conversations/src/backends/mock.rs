//! In-memory mock backend used by manager unit tests and as the basis
//! for `inject_inbound_for_test` (when the `test-helpers` feature is on).

use crate::address::{Rail, TypedAddress};
use crate::backend::{
    BackendError, InboundBucket, MailFeed, RailBackend, RailInboundMessage, ResolveResult,
    ResolvedAttachment, SendOutcome,
};
use crate::capabilities::{ThreadCapabilities, derive_capabilities};
use crate::compose::ComposeState;
use crate::message::{MessageId, MessageSnapshot};
use crate::snapshot::ThreadDetail;
use crate::thread::ThreadId;
use async_trait::async_trait;
use fauna_core::identity::ActorId;
use std::sync::Mutex;

pub struct MockRailBackend {
    pub rail: Rail,
    pub sent: Mutex<Vec<MessageSnapshot>>,
    pub resolve_overrides: Mutex<std::collections::HashMap<String, ResolveResult>>,
    /// Optional self address for test scenarios that need `me_actor()` to resolve
    /// (e.g. the reactions/delete manager tests). Defaults to `None`.
    self_address: Mutex<Option<TypedAddress>>,
    /// What `ensure_keypackages` reports as minted (default `0` = the trait's
    /// at-target no-op) — drives the manager's notify-on-mint contract test.
    mint_on_ensure: Mutex<u64>,
    /// Threads whose `remove_participant` fails, with the reason. Drives the
    /// PARTIAL arm of the cross-group eviction, which is the arm that decides
    /// whether that surface is honest — see `crate::eviction`. Without an
    /// injectable failure the partial case is unreachable in a unit test and
    /// would be left to a code comment.
    remove_fails_on: Mutex<std::collections::HashMap<ThreadId, String>>,
    /// Threads whose `add_participant` fails, with the reason — the twin of
    /// [`Self::remove_fails_on`], and there for the same stated reason: without
    /// an injectable failure the *rollback* half of
    /// `ConversationsManager::add_participant` is unreachable in a unit test,
    /// and that half is where a refused add decides whether to evict a member
    /// who was already seated.
    add_fails_on: Mutex<std::collections::HashMap<ThreadId, String>>,
    /// Threads for which this mock claims an *authoritative* roster, standing in
    /// for the MLS engine's `group_members`. Empty by default, which reports
    /// `None` per rail and leaves the thread store's participant list
    /// authoritative — the shape every other manager test wants. A test sets an
    /// entry to make the two sources **disagree**, which is the only way to
    /// reach the eviction driver's engine-vs-snapshot arm in a unit test: the
    /// production divergence is written by a foreign-authored MLS Commit, which
    /// no mock rail can post.
    engine_rosters: Mutex<std::collections::HashMap<ThreadId, Vec<ActorId>>>,
    /// Per-person thread-less engine seats this mock reports from
    /// `unbound_seats_of` — the stand-in for engine groups no thread points at
    /// (an un-rebound chat channel, a folder channel, a scheduling one-off).
    /// Empty by default; the eviction-driver class-dispatch tests seat people
    /// here, since a real unbound group only exists inside an `MlsEngine`.
    unbound_seats: Mutex<std::collections::HashMap<ActorId, Vec<crate::backend::UnboundSeat>>>,
    /// Every participant the manager showed this rail through
    /// [`RailBackend::observe_participants`] (last call wins) — lets a manager
    /// test pin that the probe hands the rails their evidence first.
    observed: Mutex<Vec<TypedAddress>>,
    /// The room this mock projects onto a thread (`room_state`), keyed by
    /// thread. Empty by default — no rail models a room, the shape every
    /// other manager test wants. A test sets one to paint a **governed** room
    /// without an MLS engine, and `update_room_policy` then edits it in place
    /// (the join rule and history policy on the stored policy; an admin
    /// appointment or demotion is remembered by actor and overlaid onto the
    /// member whose `TypedAddress::Fauna` carries that id when the room is
    /// next projected), so an app's editor tests can drive Save through the
    /// manager and read the result back off `ThreadDetail::room`.
    rooms: Mutex<std::collections::HashMap<ThreadId, crate::room::RoomSnapshot>>,
    room_admins: Mutex<std::collections::HashMap<ThreadId, Vec<(ActorId, bool)>>>,
    /// The member a `TransferOwnership` edit handed the room to, overlaid
    /// onto the projection like the admin changes above: that member reads
    /// as the owner, the previous owner as a member, and `my_role` follows
    /// the mock's self address.
    room_owner: Mutex<std::collections::HashMap<ThreadId, ActorId>>,
    /// Threads this mock reports this device **unseated** from
    /// (`seated_on_room` → `false`) — the stand-in for an MLS group the engine
    /// has processed its own removal from. Empty by default: every room is
    /// seated, the shape every other manager test wants.
    unseated: Mutex<std::collections::HashSet<ThreadId>>,
    /// The resolved attachments each `send()` call actually received, in call
    /// order — what a security test must assert against instead of a direct
    /// store read, so a corrupted `ConversationsManager::resolve_attachments`
    /// reddens it rather than going unwitnessed (a security review finding).
    sent_attachments: Mutex<Vec<Vec<ResolvedAttachment>>>,
    /// Every raw address this rail was asked to resolve, in call order — the
    /// witness for "no rail was asked": a test pinning that a path issues no
    /// probe reads this rather than inferring it from a picker state.
    resolve_calls: Mutex<Vec<String>>,
    /// The community-room invitations this mock reports standing
    /// (`room_invitations`). Empty by default. Accepting or declining one
    /// drops it, the way a settled inbox envelope stops being listed.
    room_invitations: Mutex<Vec<crate::room::RoomInvitation>>,
}

impl MockRailBackend {
    pub fn new(rail: Rail) -> Self {
        Self {
            rail,
            sent: Mutex::new(Vec::new()),
            resolve_overrides: Mutex::new(Default::default()),
            self_address: Mutex::new(None),
            mint_on_ensure: Mutex::new(0),
            remove_fails_on: Mutex::new(Default::default()),
            add_fails_on: Mutex::new(Default::default()),
            engine_rosters: Mutex::new(Default::default()),
            unbound_seats: Mutex::new(Default::default()),
            observed: Mutex::new(Vec::new()),
            rooms: Mutex::new(Default::default()),
            room_admins: Mutex::new(Default::default()),
            room_owner: Mutex::new(Default::default()),
            unseated: Mutex::new(Default::default()),
            sent_attachments: Mutex::new(Vec::new()),
            resolve_calls: Mutex::new(Vec::new()),
            room_invitations: Mutex::new(Vec::new()),
        }
    }

    /// Every raw address [`RailBackend::resolve_address`] was called with, in
    /// call order (see the `resolve_calls` field).
    pub fn resolve_calls(&self) -> Vec<String> {
        self.resolve_calls.lock().unwrap().clone()
    }

    /// Report this device removed from `thread`'s room from now on (see the
    /// `unseated` field).
    pub fn unseat(&self, thread: ThreadId) {
        self.unseated.lock().unwrap().insert(thread);
    }

    /// Report `invitations` standing from now on (see the
    /// `room_invitations` field).
    pub fn set_room_invitations(&self, invitations: Vec<crate::room::RoomInvitation>) {
        *self.room_invitations.lock().unwrap() = invitations;
    }

    /// Project `room` onto `thread` from now on (see the `rooms` field).
    pub fn set_room(&self, thread: ThreadId, room: crate::room::RoomSnapshot) {
        self.rooms.lock().unwrap().insert(thread, room);
    }

    /// The participants the most recent [`RailBackend::observe_participants`]
    /// call carried.
    pub fn observed_participants(&self) -> Vec<TypedAddress> {
        self.observed.lock().unwrap().clone()
    }

    /// Claim an authoritative roster for `thread` — the mock's stand-in for the
    /// MLS engine's `group_members`, so a test can seat someone in the "engine"
    /// that the thread store's participant list does not carry.
    pub fn set_engine_roster(&self, thread: ThreadId, members: Vec<ActorId>) {
        self.engine_rosters.lock().unwrap().insert(thread, members);
    }

    /// Seat `person` in thread-less engine channels — the mock's stand-in for
    /// groups the engine holds that no thread points at, classified as the real
    /// backend would classify them ([`RailBackend::unbound_seats_of`]).
    pub fn set_unbound_seats(&self, person: ActorId, seats: Vec<crate::backend::UnboundSeat>) {
        self.unbound_seats.lock().unwrap().insert(person, seats);
    }

    /// Make `add_participant` fail for `thread` with `reason`.
    pub fn fail_add_on(&self, thread: ThreadId, reason: &str) {
        self.add_fails_on
            .lock()
            .unwrap()
            .insert(thread, reason.to_string());
    }

    /// Make `remove_participant` fail for `thread` with `reason`.
    pub fn fail_remove_on(&self, thread: ThreadId, reason: &str) {
        self.remove_fails_on
            .lock()
            .unwrap()
            .insert(thread, reason.to_string());
    }

    /// Let every thread's `remove_participant` succeed again — the retry half of
    /// the partial-eviction test.
    pub fn clear_remove_failures(&self) {
        self.remove_fails_on.lock().unwrap().clear();
    }

    /// Make `ensure_keypackages` report `minted` fresh packages (0 = at target).
    pub fn set_mint_on_ensure(&self, minted: u64) {
        *self.mint_on_ensure.lock().unwrap() = minted;
    }

    /// Set the mock's self address so `me_actor()` in the manager resolves to
    /// a known actor (used by the reactions/delete tests).
    pub fn set_self_address(&self, addr: TypedAddress) {
        *self.self_address.lock().unwrap() = Some(addr);
    }

    pub fn override_resolve(&self, raw: &str, result: ResolveResult) {
        self.resolve_overrides
            .lock()
            .unwrap()
            .insert(raw.to_string(), result);
    }

    pub fn take_sent(&self) -> Vec<MessageSnapshot> {
        std::mem::take(&mut *self.sent.lock().unwrap())
    }

    /// The resolved attachments from the most recent `send()` call — what
    /// `ConversationsManager::send` actually put on the wire, as opposed to
    /// what a test reading the attachment store directly infers it would.
    pub fn last_sent_attachments(&self) -> Vec<ResolvedAttachment> {
        self.sent_attachments
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl RailBackend for MockRailBackend {
    fn rail(&self) -> Rail {
        self.rail
    }

    fn seated_on_room(&self, thread: &ThreadDetail) -> bool {
        !self.unseated.lock().unwrap().contains(&thread.thread_id)
    }

    async fn room_invitations(&self) -> Result<Vec<crate::room::RoomInvitation>, BackendError> {
        Ok(self.room_invitations.lock().unwrap().clone())
    }

    async fn accept_room_invitation(
        &self,
        _thread_id: ThreadId,
        invitation: &crate::room::RoomInvitation,
    ) -> Result<(), BackendError> {
        self.room_invitations
            .lock()
            .unwrap()
            .retain(|i| i.id != invitation.id);
        Ok(())
    }

    async fn decline_room_invitation(
        &self,
        invitation: &crate::room::RoomInvitation,
    ) -> Result<(), BackendError> {
        self.room_invitations
            .lock()
            .unwrap()
            .retain(|i| i.id != invitation.id);
        Ok(())
    }

    fn room_state(&self, thread: &ThreadDetail) -> Option<crate::room::RoomSnapshot> {
        let Some(mut room) = self.rooms.lock().unwrap().get(&thread.thread_id).cloned() else {
            // Unseeded, the mock answers as the real rail would: a mail thread
            // and a bridged room are transport-only rooms by derivation
            // (`SmtpBackend::room_state`, `BridgedBackend::room_state`), every
            // other rail models no room until a test seeds one.
            return matches!(self.rail, Rail::Smtp | Rail::Bridged)
                .then(|| crate::room::transport_room(thread.participants.len()));
        };
        if let Some(changes) = self.room_admins.lock().unwrap().get(&thread.thread_id) {
            for (i, participant) in thread.participants.iter().enumerate() {
                let TypedAddress::Fauna { actor_id, .. } = participant else {
                    continue;
                };
                // The last change for this actor wins (appoint, then demote,
                // reads as a member again).
                if let Some((_, admin)) = changes.iter().rev().find(|(a, _)| a == actor_id)
                    && let Some(member) = room.members.get_mut(i)
                {
                    member.role = Some(if *admin {
                        crate::room::RoomRole::Admin
                    } else {
                        crate::room::RoomRole::Member
                    });
                }
            }
        }
        if let Some(new_owner) = self
            .room_owner
            .lock()
            .unwrap()
            .get(&thread.thread_id)
            .copied()
        {
            use crate::room::RoomRole;
            for (i, participant) in thread.participants.iter().enumerate() {
                let Some(member) = room.members.get_mut(i) else {
                    continue;
                };
                if member.role == Some(RoomRole::Owner) {
                    member.role = Some(RoomRole::Member);
                }
                if matches!(participant, TypedAddress::Fauna { actor_id, .. } if *actor_id == new_owner)
                {
                    member.role = Some(RoomRole::Owner);
                }
            }
            let me_owns = self
                .self_address
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|a| a.person_actor_id())
                == Some(new_owner);
            room.my_role = match (room.my_role, me_owns) {
                (None, _) => None,
                (_, true) => Some(RoomRole::Owner),
                (Some(RoomRole::Owner), false) => Some(RoomRole::Member),
                (other, false) => other,
            };
        }
        Some(room)
    }

    async fn update_room_policy(
        &self,
        thread_id: ThreadId,
        edit: crate::backend::RoomPolicyEdit,
    ) -> Result<(), BackendError> {
        use crate::backend::RoomPolicyEdit;
        let mut rooms = self.rooms.lock().unwrap();
        let Some(room) = rooms.get_mut(&thread_id) else {
            return Err(BackendError::NotSupported);
        };
        let Some(policy) = room.policy.as_mut() else {
            return Err(BackendError::NotSupported);
        };
        match edit {
            RoomPolicyEdit::Rename(name) => policy.name = Some(name),
            RoomPolicyEdit::JoinRule(rule) => policy.join_rule = rule,
            RoomPolicyEdit::HistoryPolicy(history) => policy.history_policy = history,
            RoomPolicyEdit::AppointAdmin(actor) => self
                .room_admins
                .lock()
                .unwrap()
                .entry(thread_id)
                .or_default()
                .push((actor, true)),
            RoomPolicyEdit::DemoteAdmin(actor) => self
                .room_admins
                .lock()
                .unwrap()
                .entry(thread_id.clone())
                .or_default()
                .push((actor, false)),
            // The mock completes the ceremony in place: the named member
            // owns the room, the previous owner is a plain member, and the
            // new owner leaves the admin set.
            RoomPolicyEdit::TransferOwnership(actor) => {
                self.room_admins
                    .lock()
                    .unwrap()
                    .entry(thread_id.clone())
                    .or_default()
                    .push((actor, false));
                self.room_owner.lock().unwrap().insert(thread_id, actor);
            }
        }
        policy.version += 1;
        Ok(())
    }

    async fn ensure_keypackages(&self, _target: u64) -> Result<u64, BackendError> {
        Ok(*self.mint_on_ensure.lock().unwrap())
    }

    fn capabilities(&self, thread: &ThreadDetail) -> ThreadCapabilities {
        derive_capabilities(self.rail, thread.flavor.clone())
    }

    fn self_address(&self) -> Option<TypedAddress> {
        self.self_address.lock().unwrap().clone()
    }

    fn authoritative_roster(&self, thread_id: &ThreadId) -> Option<Vec<ActorId>> {
        self.engine_rosters.lock().unwrap().get(thread_id).cloned()
    }

    fn unbound_seats_of(&self, person: &ActorId) -> Vec<crate::backend::UnboundSeat> {
        self.unbound_seats
            .lock()
            .unwrap()
            .get(person)
            .cloned()
            .unwrap_or_default()
    }

    fn observe_participants(&self, participants: &[TypedAddress]) {
        *self.observed.lock().unwrap() = participants.to_vec();
    }

    async fn resolve_address(&self, raw: &str) -> ResolveResult {
        self.resolve_calls.lock().unwrap().push(raw.to_string());
        if let Some(over) = self.resolve_overrides.lock().unwrap().get(raw) {
            return over.clone();
        }
        // Default: parse as Email for test convenience
        ResolveResult::Resolved(TypedAddress::Email {
            email_address: raw.to_string(),
        })
    }

    fn bucket_inbound(
        &self,
        msg: RailInboundMessage,
        _mailbox: Option<MailFeed>,
    ) -> Result<InboundBucket, BackendError> {
        // The complete render document (body + attachment blocks), produced once
        // from the raw body + format + attachments (render-model.md § D1/D2);
        // computed before `msg.body` is moved below.
        let document =
            crate::message::document_for_message(&msg.body, msg.body_format, &msg.attachments);
        Ok(InboundBucket {
            rail: msg.rail,
            participants: {
                let mut v = msg.recipients.clone();
                v.push(msg.sender.clone());
                v.sort_by_key(|a| a.display());
                v.dedup_by_key(|a| a.display());
                v
            },
            subject: msg.subject.clone(),
            in_reply_to: msg.in_reply_to.clone(),
            message: MessageSnapshot {
                message_id: msg.message_id,
                sender: msg.sender,
                sender_display: String::new(),
                body: msg.body,
                document,
                timestamp_ms: msg.timestamp_ms,
                subject_line: None,
                badges: msg.badges,
                reply_to: msg.in_reply_to,
                reactions: vec![],
                deleted: false,
                is_own: false,
                legal_takedown_ref: msg.legal_takedown_ref,
                labels: vec![],
                // Carried through from the driver, never invented here: the
                // driver is the layer that holds the sealed record bytes the
                // plane identity is derived from. `None` on every rail that is
                // not on the plane at all.
                plane_ref: msg.plane_ref,
                can_delete: false,
            },
        })
    }

    async fn send(
        &self,
        _thread: &ThreadDetail,
        _compose: &ComposeState,
        attachments: &[crate::backend::ResolvedAttachment],
    ) -> Result<SendOutcome, BackendError> {
        let id = MessageId(format!("mock-msg-{}", self.sent.lock().unwrap().len()));
        self.sent_attachments
            .lock()
            .unwrap()
            .push(attachments.to_vec());
        // Append a synthetic message
        // (Real impls do real wire I/O; mock just records.)
        Ok(SendOutcome {
            message_id: id,
            timestamp_ms: 0,
            sender: TypedAddress::Email {
                email_address: "me@self-nest.test".to_string(),
            },
            // Not on the account data plane: this rail's records are not
            // content-scope feed records, so there is no T1 observation to
            // report for them.
            plane_ref: None,
            attachment_coordinates: Vec::new(),
        })
    }

    async fn add_participant(&self, t: ThreadId, _a: TypedAddress) -> Result<(), BackendError> {
        match self.add_fails_on.lock().unwrap().get(&t) {
            Some(reason) => Err(BackendError::transport_from_seam(reason.clone())),
            None => Ok(()),
        }
    }
    async fn remove_participant(&self, t: ThreadId, _a: TypedAddress) -> Result<(), BackendError> {
        match self.remove_fails_on.lock().unwrap().get(&t) {
            Some(reason) => Err(BackendError::transport_from_seam(reason.clone())),
            None => Ok(()),
        }
    }
    async fn rename(&self, _t: ThreadId, _label: String) -> Result<(), BackendError> {
        Ok(())
    }
}

/// A [`crate::backend::ConversationsRpc`] that never reaches a nest — for a
/// test that needs a real [`crate::session::ConversationsSession`] only to wire
/// something onto it (a seam, a witness), never to move a message. Every call
/// that would carry data is refused, naming itself; the calls a session makes
/// on its own at start (`channel_fetch`, the key-package count and upload,
/// `blob_get`) answer empty, so a receive loop idles rather than errors.
pub struct InertConversationsRpc;

impl InertConversationsRpc {
    fn refused(what: &str) -> crate::backend::ConvRpcError {
        crate::backend::ConvRpcError::Rejected {
            message: format!("an inert test nest never dials ({what})"),
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl crate::backend::ConversationsRpc for InertConversationsRpc {
    async fn channel_send(
        &self,
        _c: String,
        _e: Vec<u8>,
        _expect_no_commit_since: Option<i64>,
        _attachment_refs: Vec<String>,
    ) -> Result<i64, crate::backend::ConvRpcError> {
        Err(Self::refused("channel_send"))
    }
    async fn channel_send_remote(
        &self,
        _c: String,
        _u: String,
        _e: Vec<u8>,
        _expect_no_commit_since: Option<i64>,
        _attachment_refs: Vec<String>,
    ) -> Result<i64, crate::backend::ConvRpcError> {
        Err(Self::refused("channel_send_remote"))
    }
    async fn channel_fetch(
        &self,
        _c: String,
        _a: i64,
        _l: i64,
        _h: Option<String>,
    ) -> Result<Vec<crate::backend::FetchedRecord>, crate::backend::ConvRpcError> {
        Ok(vec![])
    }
    async fn keypackage_count(&self, _a: String) -> Result<u64, crate::backend::ConvRpcError> {
        Ok(0)
    }
    async fn actor_by_handle(
        &self,
        _h: String,
    ) -> Result<Option<crate::backend::ResolvedHandle>, crate::backend::ConvRpcError> {
        Err(Self::refused("actor_by_handle"))
    }
    async fn actor_by_handle_remote(
        &self,
        _d: String,
        _l: String,
    ) -> Result<Option<crate::backend::ResolvedHandle>, crate::backend::ConvRpcError> {
        Err(Self::refused("actor_by_handle_remote"))
    }
    async fn keypackage_fetch(
        &self,
        _a: String,
        _p: Option<String>,
    ) -> Result<Option<Vec<u8>>, crate::backend::ConvRpcError> {
        Err(Self::refused("keypackage_fetch"))
    }
    async fn keypackage_upload(
        &self,
        _p: Vec<Vec<u8>>,
        _l: bool,
    ) -> Result<u64, crate::backend::ConvRpcError> {
        Ok(0)
    }
    async fn welcome_deliver(
        &self,
        _r: String,
        _c: String,
        _w: Vec<u8>,
        _k: crate::backend::WelcomeChannelKind,
        _p: Option<String>,
    ) -> Result<(), crate::backend::ConvRpcError> {
        Err(Self::refused("welcome_deliver"))
    }
    async fn blob_put(
        &self,
        _c: String,
        _h: Option<String>,
        _s: String,
        _b: Vec<u8>,
    ) -> Result<(), crate::backend::ConvRpcError> {
        Err(Self::refused("blob_put"))
    }
    async fn blob_get(
        &self,
        _c: String,
        _h: Option<String>,
        _s: String,
    ) -> Result<Option<Vec<u8>>, crate::backend::ConvRpcError> {
        Ok(None)
    }
}
