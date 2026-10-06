//! The **room** every conversation is — its members as principals with a
//! kind and a role, the class the member set derives, and the owner-signed
//! policy — rendered onto the thread snapshot so every app paints it and
//! none computes it (`docs/goal/behavior/conversation-rooms.md` § The room,
//! § The three classes, § Roles and authorization; `docs/goal/ui/conversations.md`
//! § State & data shape → *Room class*).
//!
//! Two products come out of this module and they answer two different
//! questions:
//!
//! - [`RoomSnapshot`] — the **render** facts: the class label on the header,
//!   the role mark on each member chip, the policy the editor shows, the
//!   viewer's own role. Index-parallel with `ThreadDetail::participants`,
//!   exactly as `participant_displays` is.
//! - The role-gated fields of `ThreadCapabilities`
//!   (`can_invite` / `can_remove_members` / `can_set_policy` /
//!   `can_appoint_admins`, and `supports_rename` on a policy room) — the
//!   **gating** facts, so an app greys an affordance off `capabilities.*`
//!   and never branches on a role itself (`conversations.md` § Architectural
//!   rules 5).
//!
//! The policy's wire shape and the commit verdict live in
//! `fauna_mls::room_policy`; this module mirrors its three small enums onto
//! the snapshot (the wire type and the snapshot type are different things,
//! the same split `ChannelMessageBody` / `MessageSnapshot` already make).

use fauna_mls::room_policy as wire;
use serde::{Deserialize, Serialize};

use crate::capabilities::{ThreadCapabilities, ThreadEncryption};

/// What kind of principal a member is (`conversation-rooms.md` § The room →
/// *Principals*). The class derives from the kinds alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum PrincipalKind {
    /// A user — the actor and every device in their fleet, one entry.
    User,
    /// The room's home nest, holding a room-read key (community rooms only).
    Nest,
    /// A bridge principal or a mail transfer agent.
    Bridge,
}

/// A room's confidentiality class — **derived from its member set, never
/// stored, never chosen** (`conversation-rooms.md` § The three classes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum RoomClass {
    /// Every member is a user's device fleet; the members read, nests relay
    /// opaque bytes. MLS.
    EndToEnd,
    /// The users plus the room's home nest, which reads under its room-read
    /// key: moderatable, searchable, fanned out by the nest.
    Community,
    /// A bridge or mail transfer agent is a member: the far network reads it
    /// by construction, and the room says so honestly.
    TransportOnly,
}

/// The one derivation rule — total and ordered: any bridge/MTA member ⇒
/// transport-only; else the home nest a member ⇒ community; else end-to-end.
pub fn derive_room_class(kinds: impl IntoIterator<Item = PrincipalKind>) -> RoomClass {
    let mut nest = false;
    for kind in kinds {
        match kind {
            PrincipalKind::Bridge => return RoomClass::TransportOnly,
            PrincipalKind::Nest => nest = true,
            PrincipalKind::User => {}
        }
    }
    if nest {
        RoomClass::Community
    } else {
        RoomClass::EndToEnd
    }
}

/// The room a thread is when its transport is a **bridge principal or a mail
/// transfer agent** — the bridged family's rooms and every mail thread
/// (`conversation-rooms.md` § The three classes → *Transport-only*;
/// `ui/conversations.md` § Where logic lives → *The `Bridged` adapter*,
/// ruling 1). One shape for both rails, so neither carries a per-rail
/// encryption constant: the class is derived, not declared.
///
/// The floor is the user and the transport principal; the participants are
/// people, each a user principal, as `FaunaMlsBackend::room_state` seats them
/// — so `members` stays index-parallel with `participants` (the contract every
/// app's chip render relies on) and the transport principal enters the
/// derivation beside them, as a floor member does that no thread renders as a
/// participant. Policy-less: such a room enforces what the far side enforces,
/// and has no roles to mark.
pub fn transport_room(participant_count: usize) -> RoomSnapshot {
    let members = vec![
        RoomMemberSnapshot {
            kind: PrincipalKind::User,
            role: None,
        };
        participant_count
    ];
    let class = derive_room_class(
        members
            .iter()
            .map(|m| m.kind)
            .chain([PrincipalKind::User, PrincipalKind::Bridge]),
    );
    RoomSnapshot {
        class,
        members,
        policy: None,
        my_role: None,
        nest_read: None,
        labelers: None,
        awaiting_key: false,
        moderation_unverified: false,
        pending_invites: None,
    }
}

/// The class of the room a new-thread composer is about to create, from the
/// recipient chips it has committed so far — what the picker states before
/// the first message goes out (`conversation-rooms.md` § The three classes;
/// `ui/conversations.md` § Element IDs, `recipient-picker-class`). `None`
/// before any chip is committed: there is no room to speak of yet.
///
/// Every app paints this, none computes it: a `Fauna` chip is a user
/// principal, and every other rail's chip means the conversation rides a
/// bridge or mail transfer agent — a bridge principal by construction, so
/// the room is transport-only under [`derive_room_class`]'s ordered rule.
///
/// A nest principal is never a chip; `include_home_nest` is how it enters the
/// member set (`recipient-picker-home-nest-toggle`), and with it the community
/// arm. It cannot lift a bridge-ridden room out of transport-only — the rule
/// is ordered, and a bridge member means the far network reads it whatever
/// else is seated.
pub fn prospective_room_class(
    chips: &[crate::address::TypedAddress],
    include_home_nest: bool,
) -> Option<RoomClass> {
    use crate::address::TypedAddress;
    if chips.is_empty() {
        return None;
    }
    let chips = chips.iter().map(|chip| match chip {
        TypedAddress::Fauna { .. } => PrincipalKind::User,
        _ => PrincipalKind::Bridge,
    });
    let nest = include_home_nest.then_some(PrincipalKind::Nest);
    Some(derive_room_class(chips.chain(nest)))
}

impl RoomClass {
    /// The stable driver-facing token of the class — the `class` attribute
    /// `thread-room-class` and `recipient-picker-class` carry
    /// (`ui/conversations.md` § Element IDs): the same three names on every
    /// app, the `recipient-resolve-status` state-attribute shape.
    pub fn attr_token(self) -> &'static str {
        match self {
            RoomClass::EndToEnd => "end-to-end",
            RoomClass::Community => "community",
            RoomClass::TransportOnly => "transport-only",
        }
    }
}

impl JoinRule {
    /// The rules `room-join-rule-select` offers, in the order it offers them —
    /// one list for all seven editors, so no painter hand-types the token set
    /// (`ui/conversations.md` § Element IDs: `invite | member-invite`).
    /// [`JoinRule::Request`] is not offered: it is the community class's rule,
    /// and the end-to-end policy refuses it (`RoomPolicy::validate`).
    pub const EDITOR_CHOICES: [JoinRule; 2] = [JoinRule::Invite, JoinRule::MemberInvite];

    /// The picker token (`room-join-rule-select` round-trips it on
    /// `select`/`get_text`, the cross-app contract).
    pub fn token(self) -> &'static str {
        match self {
            JoinRule::Invite => "invite",
            JoinRule::MemberInvite => "member-invite",
            JoinRule::Request => "request",
        }
    }

    /// The inverse of [`Self::token`]; `None` for a token no picker offers.
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "invite" => Some(JoinRule::Invite),
            "member-invite" => Some(JoinRule::MemberInvite),
            "request" => Some(JoinRule::Request),
            _ => None,
        }
    }
}

impl HistoryPolicy {
    /// The policies `room-history-policy-select` offers, in its order — the
    /// same one-list rule as [`JoinRule::EDITOR_CHOICES`] (`none | full`).
    pub const EDITOR_CHOICES: [HistoryPolicy; 2] = [HistoryPolicy::None, HistoryPolicy::Full];

    /// The picker token (`room-history-policy-select`).
    pub fn token(self) -> &'static str {
        match self {
            HistoryPolicy::None => "none",
            HistoryPolicy::Full => "full",
        }
    }

    /// The inverse of [`Self::token`]; `None` for a token no picker offers.
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "none" => Some(HistoryPolicy::None),
            "full" => Some(HistoryPolicy::Full),
            _ => None,
        }
    }
}

impl RoomRole {
    /// The chip's `role` attribute value (`ui/conversations.md` § Element
    /// IDs: `owner | admin | member`).
    pub fn attr_token(self) -> &'static str {
        match self {
            RoomRole::Owner => "owner",
            RoomRole::Admin => "admin",
            RoomRole::Member => "member",
        }
    }

    /// The localized mark a member chip carries for this role, or `None` for
    /// a plain member, who carries none (`conversation-rooms.md` § Roles and
    /// authorization). One mapping for all seven apps, so no app invents its
    /// own word for "admin".
    pub fn chip_mark(self) -> Option<&'static str> {
        use fauna_i18n::strings::conversations::unified;
        match self {
            RoomRole::Owner => Some(unified::ROOM_ROLE_OWNER),
            RoomRole::Admin => Some(unified::ROOM_ROLE_ADMIN),
            RoomRole::Member => None,
        }
    }
}

/// A member chip's text: the display name, and on a governed room the role
/// mark for an owner or admin (`conversation-rooms.md` § Roles and
/// authorization; `ui/conversations.md` § Element IDs — "the chip's text
/// carries the localized owner/admin mark"). A plain member, and every seat
/// of a policy-less room, is the bare display name.
pub fn member_chip_text(display: &str, role: Option<RoomRole>) -> String {
    match role.and_then(RoomRole::chip_mark) {
        Some(mark) => format!("{display} · {mark}"),
        None => display.to_string(),
    }
}

impl RoomClass {
    /// The class's localized sentence — what `thread-room-class` states on
    /// the header and `recipient-picker-class` states before a new room's
    /// first message (`conversation-rooms.md` § The three classes). The same
    /// three sentences on every app.
    pub fn label(self) -> &'static str {
        use fauna_i18n::strings::conversations::unified;
        match self {
            RoomClass::EndToEnd => unified::ROOM_CLASS_END_TO_END,
            RoomClass::Community => unified::ROOM_CLASS_COMMUNITY,
            RoomClass::TransportOnly => unified::ROOM_CLASS_TRANSPORT_ONLY,
        }
    }
}

/// The room's standing notice — what `thread-room-notice` states on the
/// thread header while one holds (`ui/conversations.md` § Element IDs).
/// Derived from two shared room facts, so no app re-decides which one speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum RoomNotice {
    /// A newcomer waiting to be keyed in ([`RoomSnapshot::awaiting_key`];
    /// `community-rooms.md` § Implementation status today → *A newcomer's
    /// walk waits for its key-in*).
    AwaitingKey,
    /// Some of the room's moderation could not be verified on this device
    /// ([`RoomSnapshot::moderation_unverified`]; `conversation-rooms.md` §
    /// Roles and authorization → *Members verify what they paint*).
    ModerationUnverified,
}

impl RoomNotice {
    /// The notice that holds for these two room facts, or `None` when neither
    /// does (the element is then absent). `AwaitingKey` wins when both hold:
    /// nothing sealed opens, so there is no moderation in view to speak of.
    pub fn for_facts(awaiting_key: bool, moderation_unverified: bool) -> Option<Self> {
        if awaiting_key {
            Some(RoomNotice::AwaitingKey)
        } else if moderation_unverified {
            Some(RoomNotice::ModerationUnverified)
        } else {
            None
        }
    }

    /// The stable driver-facing token — the notice's `state` attribute, the
    /// `recipient-resolve-status` shape.
    pub fn attr_token(self) -> &'static str {
        match self {
            RoomNotice::AwaitingKey => "awaiting-key",
            RoomNotice::ModerationUnverified => "moderation-unverified",
        }
    }

    /// The notice's localized sentence, one per state, the same on every app.
    pub fn label(self) -> &'static str {
        use fauna_i18n::strings::conversations::unified;
        match self {
            RoomNotice::AwaitingKey => unified::ROOM_NOTICE_AWAITING_KEY,
            RoomNotice::ModerationUnverified => unified::ROOM_NOTICE_MODERATION_UNVERIFIED,
        }
    }
}

/// The join rule's localized label (`room-join-rule-select`'s display value).
pub fn join_rule_label(rule: JoinRule) -> &'static str {
    use fauna_i18n::strings::conversations::unified;
    match rule {
        JoinRule::Invite => unified::ROOM_JOIN_RULE_INVITE,
        JoinRule::MemberInvite => unified::ROOM_JOIN_RULE_MEMBER_INVITE,
        // Not offered by the end-to-end editor (`RoomPolicy::validate`
        // refuses it); painted by its token if a room ever carried it.
        JoinRule::Request => "request",
    }
}

/// The history policy's localized label (`room-history-policy-select`'s
/// display value).
pub fn history_policy_label(policy: HistoryPolicy) -> &'static str {
    use fauna_i18n::strings::conversations::unified;
    match policy {
        HistoryPolicy::None => unified::ROOM_HISTORY_POLICY_NONE,
        HistoryPolicy::Full => unified::ROOM_HISTORY_POLICY_FULL,
    }
}

// ── the FFI twins ──
//
// UniFFI exports free functions, not inherent methods, and hands a string
// across as an owned `String` — so each render helper above gets one twin for
// the apps that paint through the FFI (apple, android, windows). They are the
// SAME mappings linux and tui call directly, so no painter hand-types a token,
// a label or an editor's choice list (`ui/conversations.md` § Element IDs).

/// FFI-exported twin of [`RoomClass::label`] — what `thread-room-class` and
/// `recipient-picker-class` state.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_class_label(class: RoomClass) -> String {
    class.label().to_string()
}

/// FFI-exported twin of [`RoomClass::attr_token`] — the `class` attribute.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_class_attr_token(class: RoomClass) -> String {
    class.attr_token().to_string()
}

/// FFI-exported twin of [`RoomNotice::for_facts`] — which notice, if any,
/// `thread-room-notice` states for a room's `awaiting_key` and
/// `moderation_unverified`.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_notice_for(awaiting_key: bool, moderation_unverified: bool) -> Option<RoomNotice> {
    RoomNotice::for_facts(awaiting_key, moderation_unverified)
}

/// FFI-exported twin of [`RoomNotice::label`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_notice_label(notice: RoomNotice) -> String {
    notice.label().to_string()
}

/// FFI-exported twin of [`RoomNotice::attr_token`] — the `state` attribute.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_notice_attr_token(notice: RoomNotice) -> String {
    notice.attr_token().to_string()
}

/// FFI-exported twin of [`RoomRole::attr_token`] — the member chip's `role`
/// attribute.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_role_attr_token(role: RoomRole) -> String {
    role.attr_token().to_string()
}

/// FFI-exported twin of [`member_chip_text`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_member_chip_text(display: String, role: Option<RoomRole>) -> String {
    member_chip_text(&display, role)
}

/// FFI-exported twin of [`RoomInvitationSnapshot::text`] — the
/// `room-invitation[i]` row's sentence.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_invitation_text(invitation: RoomInvitationSnapshot) -> String {
    invitation.text()
}

/// FFI-exported twin of [`RoomPendingInviteSnapshot::text`] — the
/// `room-pending-invite[i]` row's sentence.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_pending_invite_text(invite: RoomPendingInviteSnapshot) -> String {
    invite.text()
}

/// FFI-exported twin of [`JoinRule::EDITOR_CHOICES`] — what
/// `room-join-rule-select` offers, in its order.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_join_rule_editor_choices() -> Vec<JoinRule> {
    JoinRule::EDITOR_CHOICES.to_vec()
}

/// FFI-exported twin of [`JoinRule::token`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_join_rule_token(rule: JoinRule) -> String {
    rule.token().to_string()
}

/// FFI-exported twin of [`join_rule_label`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_join_rule_label(rule: JoinRule) -> String {
    join_rule_label(rule).to_string()
}

/// FFI-exported twin of [`HistoryPolicy::EDITOR_CHOICES`] — what
/// `room-history-policy-select` offers, in its order.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_history_policy_editor_choices() -> Vec<HistoryPolicy> {
    HistoryPolicy::EDITOR_CHOICES.to_vec()
}

/// FFI-exported twin of [`HistoryPolicy::token`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_history_policy_token(policy: HistoryPolicy) -> String {
    policy.token().to_string()
}

/// FFI-exported twin of [`history_policy_label`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_history_policy_label(policy: HistoryPolicy) -> String {
    history_policy_label(policy).to_string()
}

/// FFI-exported twin of [`prospective_room_class`] — the new-thread picker's
/// `recipient-picker-class`, from its committed chips and the home-nest choice
/// (`RecipientPickerState::include_home_nest`).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_prospective_class(
    chips: Vec<crate::address::TypedAddress>,
    include_home_nest: bool,
) -> Option<RoomClass> {
    prospective_room_class(&chips, include_home_nest)
}

impl From<RoomClass> for ThreadEncryption {
    /// `ThreadEncryption` is the class's render (`conversation-rooms.md`
    /// § The three classes: end-to-end ⇒ `E2E`, community ⇒ the
    /// nest-readable arm, transport-only ⇒ `TransportOnly`).
    fn from(class: RoomClass) -> Self {
        match class {
            RoomClass::EndToEnd => ThreadEncryption::E2E,
            RoomClass::Community => ThreadEncryption::NestReadable,
            RoomClass::TransportOnly => ThreadEncryption::TransportOnly,
        }
    }
}

/// A member's role (`conversation-rooms.md` § Roles and authorization).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum RoomRole {
    Owner,
    Admin,
    Member,
}

impl From<wire::RoomRole> for RoomRole {
    fn from(r: wire::RoomRole) -> Self {
        match r {
            wire::RoomRole::Owner => Self::Owner,
            wire::RoomRole::Admin => Self::Admin,
            wire::RoomRole::Member => Self::Member,
        }
    }
}

impl From<RoomRole> for wire::RoomRole {
    fn from(r: RoomRole) -> Self {
        match r {
            RoomRole::Owner => Self::Owner,
            RoomRole::Admin => Self::Admin,
            RoomRole::Member => Self::Member,
        }
    }
}

impl RoomRole {
    /// Owner or admin.
    pub fn is_admin_or_owner(self) -> bool {
        matches!(self, Self::Owner | Self::Admin)
    }
}

/// One invitation standing for this account, **already verified** against the
/// inviter it names (`conversation-rooms.md` § Join rules and invites) — the
/// render facts of a room the user has been asked to join, and the id an accept
/// takes.
///
/// The third product of this module, and it answers a third question from the
/// two above: not "what does this thread look like" but "what am I being asked
/// to join". Every field is read off the inviter's signed act, so nothing here
/// is the delivery path's word — see
/// `FaunaMlsBackend::pending_room_invitations`, which drops anything that does
/// not verify rather than surfacing it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomInvitation {
    /// Opaque handle this invitation is settled by
    /// (`FaunaMlsBackend::settle_room_invitation`), after accepting or on a
    /// decline.
    pub id: i64,
    /// The room being joined — what `FaunaMlsBackend::accept_room_invite` takes,
    /// and the whole point of delivering the invitation at all.
    pub room_id: [u8; 32],
    /// The principal whose key signed the invitation.
    pub inviter: fauna_core::identity::ActorId,
    /// The role the invitee is invited into. Never [`RoomRole::Owner`] — a room
    /// has exactly one owner and it moves by transfer, which
    /// `RoomInvite::validate` refuses to sign around.
    pub role: RoomRole,
    /// The policy version the inviter read the join rule under. No door
    /// compares this, on either leg, by ruling rather than omission
    /// (`fauna_mls::room_policy::RoomInvite::policy_version`): the room's
    /// home judges an invitation against its *current* stored policy when it
    /// is issued and again when it is accepted, and the invitee's own nest —
    /// which relays a cross-nest acceptance — decides nothing about the room.
    /// `0` from a room carrying no version (they start at 1) — "unstated".
    pub policy_version: u64,
    /// The room's home nest when it is not this account's own; `None`
    /// same-nest. Written by this account's nest from the delivering peer's
    /// verified identity on a cross-nest delivery, and the next hop the
    /// acceptance is relayed to (`conversation-rooms.md` § Join rules and
    /// invites → *A cross-nest invitation*).
    pub room_node: Option<String>,
}

impl RoomInvitation {
    /// The room id as the hex string every id-taking door and rendered surface
    /// uses — written once here so the apps cannot drift on the encoding.
    pub fn room_id_hex(&self) -> String {
        hex::encode(self.room_id)
    }
}

/// Who may invite (`conversation-rooms.md` § Join rules and invites).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum JoinRule {
    Invite,
    MemberInvite,
    Request,
}

impl From<wire::JoinRule> for JoinRule {
    fn from(j: wire::JoinRule) -> Self {
        match j {
            wire::JoinRule::Invite => Self::Invite,
            wire::JoinRule::MemberInvite => Self::MemberInvite,
            wire::JoinRule::Request => Self::Request,
        }
    }
}

impl From<JoinRule> for wire::JoinRule {
    fn from(j: JoinRule) -> Self {
        match j {
            JoinRule::Invite => Self::Invite,
            JoinRule::MemberInvite => Self::MemberInvite,
            JoinRule::Request => Self::Request,
        }
    }
}

/// What a newcomer sees of the room before their admission
/// (`conversation-rooms.md` § History for joiners).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum HistoryPolicy {
    None,
    Full,
}

impl From<wire::HistoryPolicy> for HistoryPolicy {
    fn from(h: wire::HistoryPolicy) -> Self {
        match h {
            wire::HistoryPolicy::None => Self::None,
            wire::HistoryPolicy::Full => Self::Full,
        }
    }
}

impl From<HistoryPolicy> for wire::HistoryPolicy {
    fn from(h: HistoryPolicy) -> Self {
        match h {
            HistoryPolicy::None => Self::None,
            HistoryPolicy::Full => Self::Full,
        }
    }
}

/// One member's render facts — **index-parallel with
/// `ThreadDetail::participants`** (and `participant_displays`), so
/// `members[i]` is the role mark on `thread-member-chip[i]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RoomMemberSnapshot {
    pub kind: PrincipalKind,
    /// `None` on a room with no policy (a policy-less room): there are no roles
    /// to mark, not "everyone is a member".
    pub role: Option<RoomRole>,
}

/// The owner-signed policy's rendered fields (`conversation-rooms.md`
/// § Roles and authorization — the policy editor's values).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RoomPolicySnapshot {
    pub version: u64,
    pub name: Option<String>,
    pub join_rule: JoinRule,
    pub history_policy: HistoryPolicy,
}

impl From<&wire::RoomPolicy> for RoomPolicySnapshot {
    fn from(p: &wire::RoomPolicy) -> Self {
        Self {
            version: p.version,
            name: p.name.clone(),
            join_rule: p.join_rule.into(),
            history_policy: p.history_policy.into(),
        }
    }
}

/// The room a thread is, as every app paints it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RoomSnapshot {
    /// Derived from `members` by [`derive_room_class`]; rendered on the
    /// thread header, and as `ThreadCapabilities::encryption`.
    pub class: RoomClass,
    /// Index-parallel with `ThreadDetail::participants`.
    pub members: Vec<RoomMemberSnapshot>,
    /// `None` on a policy-less room — a 1:1, or a group whose context
    /// carries no policy (every group this app forks is born governed; a
    /// policy-less group is one a peer minted without a policy).
    pub policy: Option<RoomPolicySnapshot>,
    /// The viewer's own effective role; `None` on a policy-less room.
    pub my_role: Option<RoomRole>,
    /// Whether the room's home nest reads it — its current generation carries
    /// a wrap to the nest. `room-nest-read-toggle`'s `checked` attribute.
    ///
    /// `Some` only for a **community** room whose floor this device has read
    /// from a nest that answers it; `None` for every other class (there is no
    /// home-nest read to speak of) and for a community room whose answer is not
    /// in yet, where the toggle is not painted rather than painted as a guess.
    /// A community room whose nest does not read is still a community room —
    /// the nest stays on the floor; the members have withdrawn its grant
    /// (`conversation-rooms.md` § Implementation status today, the revoke).
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub nest_read: Option<bool>,
    /// The transparent labelers the room's home nest applies to its messages —
    /// the room's signed labeler set, **verified** (its signature, and that it
    /// names this room), as published labeler ids in lowercase hex
    /// (`conversation-rooms.md` § The three classes → *What the home nest does
    /// with its read*, purpose 2). What the editor stages from, and what an app
    /// says reads the room.
    ///
    /// `Some(empty)` for a community room that names none. `None` for every
    /// other class (no nest reads, so none labels), for a community room whose
    /// floor this device has not read, and for one whose stored set does not
    /// verify — nothing this device can stand behind, so nothing painted.
    ///
    /// A set stays in force whoever signed it, including a signer who has since
    /// lost the rank to sign another: the home nest runs the stored set, exactly
    /// as a policy version outlives its signer's demotion — so it renders as in
    /// force, and the next owner or admin to change it replaces it whole.
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub labelers: Option<Vec<String>>,
    /// Whether this is a **community room this account is not keyed into
    /// yet** — it accepted, and no owner's or admin's device has wrapped the
    /// room's key to it — so nothing sealed in the room opens here until one
    /// does. What the room's own row says while it waits, rather than a page
    /// error: the wait is an expected state, not a fault
    /// (`community-rooms.md` § Implementation status today → *A newcomer's walk
    /// waits for its key-in*).
    ///
    /// `false` for every other class and for a room whose walk has not yet met
    /// a record it cannot open. Painted as `thread-room-notice`'s
    /// `awaiting-key` state ([`RoomSnapshot::notice`]).
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    pub awaiting_key: bool,
    /// Whether this room carries **moderation this device could not verify**:
    /// an owner's or admin's floor delete record is parked because the chain
    /// under the policy version it names rests on a name this device holds no
    /// anchor for, and the peer-anchor harvest has **settled** that name for
    /// the session without finding one — a retired owner or admin homed on
    /// another nest, which the harvest's reach never covers
    /// (`conversation-rooms.md` § Roles and authorization → *Delete any
    /// message — the mechanism* → *Members verify what they paint*, the
    /// "says so" rule). The record's target stays painted, exactly as before:
    /// what this adds is that the room says so, instead of showing the message
    /// as if nobody had acted on it. A room-level statement, never a mark on
    /// the target — that would paint the record's claim before it is verified.
    ///
    /// `false` for every other class, for a room with nothing parked, and while
    /// a parked record still waits on a harvest that could yet anchor its name
    /// — then it is *not yet* verified, not *unverifiable*, and the next pass
    /// may paint it. Clears when the parked records paint. Across a relaunch
    /// the parked set rides the `history/<ch>` replica and is taken up again
    /// on the first pass over the room, so the notice returns with it once the
    /// harvest has settled the name again (the harvest's own memory is per
    /// session). Painted as `thread-room-notice`'s `moderation-unverified`
    /// state ([`RoomSnapshot::notice`]), which yields to `awaiting_key`.
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    pub moderation_unverified: bool,
    /// The invitations pending on this room that the viewer may withdraw,
    /// oldest first (`conversation-rooms.md` § Join rules and invites →
    /// *Pending invitations are visible to whoever may withdraw them*). The
    /// home nest scopes the list — everything for the owner and admins, what
    /// it issued for any other seated member — so **every row served carries
    /// the withdraw gesture** and no app gates it a second time
    /// (`ConversationsManager::withdraw_room_invite`).
    ///
    /// `Some(empty)` is a real answer: nothing the viewer may withdraw stands
    /// pending. `None` for every other class (an end-to-end room's floor
    /// holds no invitations), for a community room homed on another nest (the
    /// doors have no relay), for one whose list this device has not read, and
    /// when the nest's reply omits the list — not painted rather than painted as a
    /// guess. Not painted by any app yet: it needs element ids ui.yaml does
    /// not carry (rule A).
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub pending_invites: Option<Vec<RoomPendingInviteSnapshot>>,
}

/// One invitation pending on a room, as the room's member surface paints it
/// for a viewer who may withdraw it ([`RoomSnapshot::pending_invites`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RoomPendingInviteSnapshot {
    /// The invitee's actor id, lowercase hex — the handle
    /// `ConversationsManager::withdraw_room_invite` names the invitation by.
    /// Opaque to every app.
    pub invitee_actor_hex: String,
    /// Who was invited: the `handle@domain` the home nest knows them under,
    /// else their elided actor id (`TypedAddress::display`). Display only.
    pub invitee_display: String,
    /// Who invited them, named the same way. Attribution — never re-pointed by
    /// a succession.
    pub inviter_display: String,
    /// The rank on offer — admin or member, never owner.
    pub role: RoomRole,
    /// When the invitation was (last) issued, epoch millis.
    pub invited_at_ms: i64,
    /// Whether the invitation has **lapsed in waiting**: the accept door would
    /// refuse it today (its inviter was demoted, departed or succeeded, or the
    /// policy no longer names an admin invitee). It seats nobody and is listed
    /// so it can be cleared rather than sit invisible until somebody tries it.
    pub lapsed: bool,
}

impl RoomPendingInviteSnapshot {
    /// The row's sentence — the invitee, who invited them, the rank on offer
    /// when it is more than a plain seat, and the words a lapsed row adds
    /// (it would no longer be accepted; it is listed so it can be cleared).
    /// One wording for all seven apps, so no app invents its own
    /// (`room-pending-invite[i]`; `ui/conversations.md` § Element IDs). An
    /// owner rank is never on offer, so it reads as the plain invitation.
    pub fn text(&self) -> String {
        use fauna_i18n::strings::conversations::unified;
        let template = match self.role {
            RoomRole::Admin => unified::ROOM_PENDING_INVITE_ADMIN,
            RoomRole::Member | RoomRole::Owner => unified::ROOM_PENDING_INVITE_MEMBER,
        };
        let sentence = template
            .replace("{invitee}", &self.invitee_display)
            .replace("{inviter}", &self.inviter_display);
        if self.lapsed {
            unified::ROOM_PENDING_INVITE_LAPSED.replace("{sentence}", &sentence)
        } else {
            sentence
        }
    }
}

/// One room invitation standing for this account, as the conversation list
/// paints it (`room-invitation[i]`, with `room-invitation-accept-button[i]` and
/// `room-invitation-decline-button[i]`) — already verified against its signer
/// ([`RoomInvitation`]), so what it names is what the inviter signed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RoomInvitationSnapshot {
    /// The handle accept and decline name it by — opaque to every app.
    pub id: i64,
    /// Who invited, as this device can name them: the handle it has met them
    /// under, else their elided actor id (`TypedAddress::display`). Display
    /// only — the invitation's authority is the verified signature, never this.
    pub inviter_display: String,
    /// The rank the invitee will hold once they accept.
    pub role: RoomRole,
}

impl RoomInvitationSnapshot {
    /// The row's sentence — who invited, and the rank on offer when it is
    /// more than a plain seat. One wording for all seven apps, so no app
    /// invents its own (`room-invitation[i]`). An owner rank is never on
    /// offer (a room's ownership moves only by hand-over), so it reads as
    /// the plain invitation rather than inventing a word for it.
    pub fn text(&self) -> String {
        use fauna_i18n::strings::conversations::unified;
        let template = match self.role {
            RoomRole::Admin => unified::ROOM_INVITATION_ADMIN,
            RoomRole::Member | RoomRole::Owner => unified::ROOM_INVITATION_MEMBER,
        };
        template.replace("{inviter}", &self.inviter_display)
    }
}

impl RoomSnapshot {
    /// The room's standing notice, if one holds — what `thread-room-notice`
    /// paints ([`RoomNotice::for_facts`]).
    pub fn notice(&self) -> Option<RoomNotice> {
        RoomNotice::for_facts(self.awaiting_key, self.moderation_unverified)
    }

    /// Whether this room carries a policy (roles are enforced).
    pub fn is_governed(&self) -> bool {
        self.policy.is_some()
    }

    /// Overlay the roles table onto `caps` for this room's viewer — the one
    /// place the table becomes gating, so no app re-derives it.
    ///
    /// On a room with no policy every field keeps `caps`' rail/flavor answer
    /// (today's behaviour: any member of a policy-less group may add, remove and
    /// rename). On a governed room:
    ///
    /// | field | owner | admin | member |
    /// |---|---|---|---|
    /// | `can_invite` | ✓ | ✓ | per join rule |
    /// | `can_remove_members` | ✓ | ✓ | — |
    /// | `supports_rename`, `can_set_policy` | ✓ | ✓ | — |
    /// | `can_appoint_admins`, `can_transfer_ownership` | ✓ | — | — |
    /// | `can_leave_room` | — (transfer first) | ✓ | ✓ |
    /// Whether the viewer may delete **another member's** message here
    /// (`conversation-rooms.md` § Roles and authorization → *Delete any message
    /// — the mechanism*): owner or admin of a **governed** room, of either
    /// class that carries the act — an end-to-end room seals it, a community
    /// room files it as a floor delete record
    /// ([`crate::backend::ConversationBackend::send_delete_any`]). A role with
    /// no policy behind it governs nothing (a policy-less room), and a
    /// transport-only room has no such act.
    pub fn viewer_deletes_any(&self) -> bool {
        matches!(self.class, RoomClass::EndToEnd | RoomClass::Community)
            && self.policy.is_some()
            && self.my_role.is_some_and(|r| r.is_admin_or_owner())
    }

    pub fn gate(&self, mut caps: ThreadCapabilities) -> ThreadCapabilities {
        caps.encryption = self.class.into();
        let (Some(policy), Some(role)) = (&self.policy, self.my_role) else {
            return caps;
        };
        let governs = role.is_admin_or_owner();
        caps.can_invite = caps.supports_membership_change
            && (governs || policy.join_rule == JoinRule::MemberInvite);
        caps.can_remove_members = caps.supports_membership_change && governs;
        caps.supports_rename = caps.supports_rename && governs;
        caps.can_set_policy = governs;
        caps.can_appoint_admins = role == RoomRole::Owner;
        caps.can_transfer_ownership = role == RoomRole::Owner;
        // The roles table's *leave (remove self)* row: admin and member walk
        // out; the owner transfers first, because a room is never owner-less
        // (§ Roles and authorization → *Leaving — the mechanism*). Narrowing
        // only — a rail with no leave door keeps its `false`, and a policy-less
        // room never reaches here, which is right: it has no owner to strand.
        caps.can_leave_room = caps.can_leave_room && role != RoomRole::Owner;
        caps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::Rail;
    use crate::capabilities::derive_capabilities;
    use crate::thread::ThreadFlavor;

    /// The room's notice: absent while neither fact holds, each fact alone
    /// names its own state, and `awaiting-key` wins when both hold; the FFI
    /// twins say the same.
    #[test]
    fn the_room_notice_is_absent_without_a_fact_and_awaiting_key_wins_over_unverified_moderation() {
        assert_eq!(RoomNotice::for_facts(false, false), None);
        assert_eq!(
            RoomNotice::for_facts(false, true),
            Some(RoomNotice::ModerationUnverified)
        );
        assert_eq!(
            RoomNotice::for_facts(true, false),
            Some(RoomNotice::AwaitingKey)
        );
        assert_eq!(
            RoomNotice::for_facts(true, true),
            Some(RoomNotice::AwaitingKey)
        );
        assert_eq!(RoomNotice::AwaitingKey.attr_token(), "awaiting-key");
        assert_eq!(
            RoomNotice::ModerationUnverified.attr_token(),
            "moderation-unverified"
        );
        assert_ne!(
            RoomNotice::AwaitingKey.label(),
            RoomNotice::ModerationUnverified.label()
        );
        assert_eq!(room_notice_for(true, true), Some(RoomNotice::AwaitingKey));
        assert_eq!(
            room_notice_label(RoomNotice::ModerationUnverified),
            RoomNotice::ModerationUnverified.label()
        );
        assert_eq!(
            room_notice_attr_token(RoomNotice::AwaitingKey),
            "awaiting-key"
        );
    }

    /// The pending row names the invitee and the inviter, the rank only when
    /// it is more than a seat, and says in words when it has lapsed; the FFI
    /// twin says the same.
    #[test]
    fn a_pending_invitation_row_names_both_parties_the_admin_rank_and_a_lapse() {
        let pending = |role, lapsed| RoomPendingInviteSnapshot {
            invitee_actor_hex: "00".repeat(32),
            invitee_display: "bob@home.test".to_string(),
            inviter_display: "ada@home.test".to_string(),
            role,
            invited_at_ms: 1,
            lapsed,
        };
        assert_eq!(
            pending(RoomRole::Member, false).text(),
            "bob@home.test — invited by ada@home.test"
        );
        assert_eq!(
            pending(RoomRole::Admin, false).text(),
            "bob@home.test — invited by ada@home.test as an admin"
        );
        assert_eq!(
            pending(RoomRole::Member, true).text(),
            "bob@home.test — invited by ada@home.test (can no longer be accepted)"
        );
        assert_eq!(
            room_pending_invite_text(pending(RoomRole::Admin, true)),
            "bob@home.test — invited by ada@home.test as an admin (can no longer be accepted)"
        );
    }

    /// The invitation row names the inviter, and the rank on offer only when
    /// it is more than a seat; the FFI twin says the same.
    #[test]
    fn an_invitation_row_names_its_inviter_and_an_admin_rank() {
        let invitation = |role| RoomInvitationSnapshot {
            id: 7,
            inviter_display: "ada@home.test".to_string(),
            role,
        };
        assert_eq!(
            invitation(RoomRole::Member).text(),
            "ada@home.test invited you to a room"
        );
        assert_eq!(
            invitation(RoomRole::Admin).text(),
            "ada@home.test invited you to a room as an admin"
        );
        assert_eq!(
            room_invitation_text(invitation(RoomRole::Admin)),
            invitation(RoomRole::Admin).text()
        );
    }

    /// Every FFI twin answers exactly what the method it wraps answers, so an
    /// app painting through the FFI (apple, android, windows) and one calling
    /// the Rust directly (linux, tui) cannot disagree on a token or a label.
    #[test]
    fn the_ffi_twins_answer_what_the_methods_do() {
        for class in [
            RoomClass::EndToEnd,
            RoomClass::Community,
            RoomClass::TransportOnly,
        ] {
            assert_eq!(room_class_label(class), class.label());
            assert_eq!(room_class_attr_token(class), class.attr_token());
        }
        for role in [RoomRole::Owner, RoomRole::Admin, RoomRole::Member] {
            assert_eq!(room_role_attr_token(role), role.attr_token());
            assert_eq!(
                room_member_chip_text("ada".to_string(), Some(role)),
                member_chip_text("ada", Some(role))
            );
        }
        assert_eq!(room_member_chip_text("ada".to_string(), None), "ada");
        assert_eq!(
            room_join_rule_editor_choices(),
            JoinRule::EDITOR_CHOICES.to_vec()
        );
        for rule in [JoinRule::Invite, JoinRule::MemberInvite, JoinRule::Request] {
            assert_eq!(room_join_rule_token(rule), rule.token());
            assert_eq!(room_join_rule_label(rule), join_rule_label(rule));
        }
        assert_eq!(
            room_history_policy_editor_choices(),
            HistoryPolicy::EDITOR_CHOICES.to_vec()
        );
        for policy in [HistoryPolicy::None, HistoryPolicy::Full] {
            assert_eq!(room_history_policy_token(policy), policy.token());
            assert_eq!(
                room_history_policy_label(policy),
                history_policy_label(policy)
            );
        }
        let fauna = crate::address::TypedAddress::Fauna {
            handle: "a".to_string(),
            actor_id: fauna_core::identity::ActorId([7u8; 32]),
        };
        assert_eq!(room_prospective_class(vec![], false), None);
        assert_eq!(
            room_prospective_class(vec![fauna.clone()], false),
            prospective_room_class(std::slice::from_ref(&fauna), false)
        );
        assert_eq!(
            room_prospective_class(vec![fauna.clone()], true),
            prospective_room_class(&[fauna], true)
        );
    }

    /// The picker's statement: nothing before a chip; end-to-end while every
    /// chip is a Fauna address; transport-only the moment a bridge-ridden
    /// address is committed, in either order.
    #[test]
    fn the_prospective_class_follows_the_committed_chips() {
        use crate::address::TypedAddress;
        let fauna = |h: &str| TypedAddress::Fauna {
            handle: h.to_string(),
            actor_id: fauna_core::identity::ActorId([7u8; 32]),
        };
        let mail = TypedAddress::Email {
            email_address: "someone@example.org".to_string(),
        };
        assert_eq!(prospective_room_class(&[], false), None);
        assert_eq!(
            prospective_room_class(&[fauna("a")], false),
            Some(RoomClass::EndToEnd)
        );
        assert_eq!(
            prospective_room_class(&[fauna("a"), fauna("b")], false),
            Some(RoomClass::EndToEnd)
        );
        assert_eq!(
            prospective_room_class(&[fauna("a"), mail.clone()], false),
            Some(RoomClass::TransportOnly)
        );
        assert_eq!(
            prospective_room_class(&[mail, fauna("a")], false),
            Some(RoomClass::TransportOnly)
        );
    }

    /// The home-nest toggle is a member choice, so it moves the class by the
    /// same ordered rule as a chip: community over Fauna chips, and never out
    /// of transport-only — a bridge member means the far network reads the
    /// room whatever else is seated. Nothing before a chip, toggle or not.
    #[test]
    fn the_home_nest_toggle_makes_the_prospective_room_a_community() {
        use crate::address::TypedAddress;
        let fauna = TypedAddress::Fauna {
            handle: "a".to_string(),
            actor_id: fauna_core::identity::ActorId([7u8; 32]),
        };
        let mail = TypedAddress::Email {
            email_address: "someone@example.org".to_string(),
        };
        assert_eq!(prospective_room_class(&[], true), None);
        assert_eq!(
            prospective_room_class(std::slice::from_ref(&fauna), true),
            Some(RoomClass::Community)
        );
        assert_eq!(
            prospective_room_class(&[fauna, mail], true),
            Some(RoomClass::TransportOnly)
        );
    }

    /// The tokens the pickers and attributes carry round-trip, and a token no
    /// picker offers parses to nothing rather than to a default.
    #[test]
    fn tokens_round_trip() {
        for rule in [JoinRule::Invite, JoinRule::MemberInvite, JoinRule::Request] {
            assert_eq!(JoinRule::from_token(rule.token()), Some(rule));
        }
        for policy in [HistoryPolicy::None, HistoryPolicy::Full] {
            assert_eq!(HistoryPolicy::from_token(policy.token()), Some(policy));
        }
        assert_eq!(JoinRule::from_token("open"), None);
        assert_eq!(HistoryPolicy::from_token("some"), None);
        assert_eq!(RoomClass::EndToEnd.attr_token(), "end-to-end");
        assert_eq!(RoomRole::Admin.attr_token(), "admin");
    }

    /// The editor's offered choices are exactly ui.yaml's token sets, in
    /// order — every app's picker paints this list, so a change here is a
    /// change to all seven — and never offer the rule an end-to-end policy
    /// refuses.
    #[test]
    fn the_editor_offers_the_ui_yaml_token_sets() {
        let rules: Vec<&str> = JoinRule::EDITOR_CHOICES.iter().map(|r| r.token()).collect();
        assert_eq!(rules, ["invite", "member-invite"]);
        assert!(!JoinRule::EDITOR_CHOICES.contains(&JoinRule::Request));
        let policies: Vec<&str> = HistoryPolicy::EDITOR_CHOICES
            .iter()
            .map(|p| p.token())
            .collect();
        assert_eq!(policies, ["none", "full"]);
    }

    /// The three classes from three member sets — the `libs/fauna-conversations`
    /// unit `conversation-rooms.md` § Done definition names.
    #[test]
    fn the_three_classes_derive_from_the_member_set() {
        use PrincipalKind::*;
        assert_eq!(derive_room_class([User, User]), RoomClass::EndToEnd);
        assert_eq!(derive_room_class([User]), RoomClass::EndToEnd);
        assert_eq!(derive_room_class([User, Nest, User]), RoomClass::Community);
        assert_eq!(derive_room_class([User, Bridge]), RoomClass::TransportOnly);
        // Ordered: a bridge wins over the nest.
        assert_eq!(
            derive_room_class([Nest, User, Bridge]),
            RoomClass::TransportOnly
        );
        // Total: even the empty set has a class.
        assert_eq!(derive_room_class([]), RoomClass::EndToEnd);
    }

    #[test]
    fn the_class_renders_as_thread_encryption() {
        assert_eq!(
            ThreadEncryption::from(RoomClass::EndToEnd),
            ThreadEncryption::E2E
        );
        assert_eq!(
            ThreadEncryption::from(RoomClass::Community),
            ThreadEncryption::NestReadable
        );
        assert_eq!(
            ThreadEncryption::from(RoomClass::TransportOnly),
            ThreadEncryption::TransportOnly
        );
    }

    fn governed(role: RoomRole, join_rule: JoinRule) -> RoomSnapshot {
        RoomSnapshot {
            class: RoomClass::EndToEnd,
            members: vec![],
            policy: Some(RoomPolicySnapshot {
                version: 1,
                name: None,
                join_rule,
                history_policy: HistoryPolicy::None,
            }),
            my_role: Some(role),
            nest_read: None,
            labelers: None,
            awaiting_key: false,
            moderation_unverified: false,
            pending_invites: None,
        }
    }

    #[test]
    fn gating_follows_the_roles_table_on_a_governed_room() {
        let base = derive_capabilities(Rail::FaunaMls, ThreadFlavor::MlsGroup);

        let owner = governed(RoomRole::Owner, JoinRule::Invite).gate(base);
        assert!(owner.can_invite && owner.can_remove_members);
        assert!(owner.supports_rename && owner.can_set_policy);
        assert!(owner.can_appoint_admins && owner.can_transfer_ownership);

        let admin = governed(RoomRole::Admin, JoinRule::Invite).gate(base);
        assert!(admin.can_invite && admin.can_remove_members);
        assert!(admin.supports_rename && admin.can_set_policy);
        assert!(!admin.can_appoint_admins && !admin.can_transfer_ownership);

        let member = governed(RoomRole::Member, JoinRule::Invite).gate(base);
        assert!(!member.can_invite);
        assert!(!member.can_remove_members);
        assert!(!member.supports_rename && !member.can_set_policy);
        assert!(!member.can_appoint_admins && !member.can_transfer_ownership);
        // The affordance stays rendered (greyed): the rail still supports
        // membership change.
        assert!(member.supports_membership_change);

        let member = governed(RoomRole::Member, JoinRule::MemberInvite).gate(base);
        assert!(member.can_invite, "member-invite lets a member invite");
        assert!(!member.can_remove_members);
    }

    #[test]
    fn a_policy_less_room_keeps_the_rail_answer() {
        let base = derive_capabilities(Rail::FaunaMls, ThreadFlavor::MlsGroup);
        let policy_less = RoomSnapshot {
            class: RoomClass::EndToEnd,
            members: vec![RoomMemberSnapshot {
                kind: PrincipalKind::User,
                role: None,
            }],
            policy: None,
            my_role: None,
            nest_read: None,
            labelers: None,
            awaiting_key: false,
            moderation_unverified: false,
            pending_invites: None,
        };
        let caps = policy_less.gate(base);
        assert!(caps.can_invite && caps.can_remove_members && caps.supports_rename);
        assert!(!caps.can_set_policy && !caps.can_appoint_admins && !caps.can_transfer_ownership);
        assert_eq!(caps.encryption, ThreadEncryption::E2E);
    }
}
