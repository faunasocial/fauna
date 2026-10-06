//! The room policy editor's staged state — the `room_settings` sub-page's
//! whole non-visual half, shared by all seven apps.
//!
//! `docs/goal/ui/conversations.md` § Element IDs (the room model's elements)
//! declares the editor: two token pickers, one admin switch and one hand-over
//! control per member, and a Save that "commits every staged change as its own
//! policy commit, the hand-over last, and closes only when all landed".
//! Everything in that sentence except the pixels is decided here — the seed
//! off the projected room, which rows either control may act on, the
//! at-most-one-staged hand-over rule, and the diff-to-edits — so an app's
//! editor is a painter over this draft and never re-derives a role rule
//! (`conversation-rooms.md` § Roles and authorization; priority #2).

use crate::address::TypedAddress;
use crate::room::{HistoryPolicy, JoinRule, RoomRole};
use crate::snapshot::ThreadDetail;
use serde::{Deserialize, Serialize};

/// One staged change of the room policy editor, in the manager's own terms.
///
/// Ordered by [`RoomSettingsDraft::edits`]: the two rules first, then the
/// appointments and demotions, then the home nest's read and the labeler set,
/// then the hand-over **last** — after a hand-over lands this seat is a plain
/// member and could commit none of the others.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum RoomSettingsEdit {
    JoinRule {
        rule: JoinRule,
    },
    HistoryPolicy {
        policy: HistoryPolicy,
    },
    Appoint {
        address: TypedAddress,
    },
    Demote {
        address: TypedAddress,
    },
    /// Grant or withdraw the home nest's read of a community room
    /// (`room-nest-read-toggle`) — a key **rotation**, not a policy commit
    /// (`conversation-rooms.md` § Implementation status today, the revoke).
    /// Before the hand-over, because a rotation needs the owner's or an
    /// admin's rank.
    NestRead {
        reads: bool,
    },
    /// Replace a community room's labeler set with `labelers` — published
    /// labeler ids, lowercase hex (`conversation-rooms.md` § The three classes →
    /// *What the home nest does with its read*, purpose 2). One edit for the
    /// whole set: the set is one signed record, replaced whole. Before the
    /// hand-over, because naming what reads the room is the owner's or an
    /// admin's act.
    Labelers {
        labelers: Vec<String>,
    },
    /// The hand-over (`conversation-rooms.md` § Roles and authorization →
    /// *Ownership transfer*) — always the last edit Save issues.
    TransferOwnership {
        address: TypedAddress,
    },
}

/// The `room_settings` editor's staged state, seeded from the projected room
/// and diffed back to [`RoomSettingsEdit`]s on Save.
///
/// Every `Vec` here is index-parallel **with each other and with
/// [`Self::seats`]** — the roster as it stood when the editor opened. They are
/// deliberately NOT parallel with the *live* `ThreadDetail::participants`,
/// which moves under an open editor: an inbound membership commit drops a row
/// from the middle of that list (`ConversationsManager::apply_inbound_roster`
/// → `ThreadStore::retain_participants`), shifting every later participant one
/// place left.
///
/// ⚠ **So every method here takes the live participant list and resolves
/// through [`Self::seats`] — never by position.** Staging by position let a
/// roster that shifted mid-edit carry the owner's choice onto whoever slid
/// into that slot, and `update_room_policy` signs the result with the owner's
/// own credential, so every member verifies and accepts an appointment the
/// owner never made. The paint index
/// (`room-admin-toggle[i]`) is still a live-list index — that is what the
/// element contract means — which is exactly why translating it here, once,
/// is the fix rather than pushing the resolution into seven painters.
//
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RoomSettingsDraft {
    /// `room-join-rule-select`'s staged value.
    pub join_rule: JoinRule,
    /// `room-history-policy-select`'s staged value.
    pub history_policy: HistoryPolicy,
    /// **The identity column: the actor each staged row belongs to**, as
    /// lowercase hex, index-parallel with every other `Vec` here. Empty for a
    /// participant that carries no actor id — a non-`Fauna` row, which is
    /// never a staging target anyway ([`Self::eligible`]).
    ///
    /// This is what makes the draft survive a roster that moves under it: a
    /// live participant is matched to its staged flags by actor, and the two
    /// residues are both handled honestly — a staged row that has since LEFT
    /// emits nothing (`update_room_policy` has no membership check of its own,
    /// so emitting it would write a departed member into the room's signed
    /// policy), and a member seated after the editor opened is simply not in
    /// this column, so Save says nothing about them either way.
    pub seats: Vec<String>,
    /// The staged admin flag per seeded row — `room-admin-toggle[i]`'s
    /// `checked` attribute, once `i` has been resolved through
    /// [`Self::seats`].
    pub admins: Vec<bool>,
    /// Whether that participant is the owner — never a toggle target: the
    /// owner is not in the admin set by construction (`RoomPolicy::validate`),
    /// and neither control is ever live on the owner's own row.
    pub owners: Vec<bool>,
    /// Whether either control may act on that row **at all**, before the
    /// viewer's capability is consulted: a `Fauna` participant (a room's
    /// members are user principals) who is not the owner. An app paints
    /// `room-admin-toggle[i]` live iff
    /// `capabilities.can_appoint_admins && eligible[i]`, and
    /// `room-owner-transfer-button[i]` live iff
    /// `capabilities.can_transfer_ownership && eligible[i]` — the one
    /// expression every app writes, so no app re-derives who is eligible
    /// (`ui/conversations.md` § Architectural rules 5: greyed, never hidden).
    pub eligible: Vec<bool>,
    /// The participant staged as the room's new owner
    /// (`room-owner-transfer-button[i]`), at most one; `None` stages no
    /// hand-over.
    ///
    /// An index into the **seeded** vectors, not into the live participant
    /// list — the seeded vectors never change length or order after
    /// [`Self::seed`], so this stays valid however the roster moves, and
    /// [`Self::seats`] says who it means.
    pub transfer_to: Option<u32>,
    /// The projected join rule the staged one is diffed against. Set by
    /// [`RoomSettingsDraft::seed`] and never edited by a painter.
    pub seed_join_rule: JoinRule,
    /// The projected history policy the staged one is diffed against.
    pub seed_history_policy: HistoryPolicy,
    /// The projected admin flags the staged ones are diffed against.
    pub seed_admins: Vec<bool>,
    /// `room-nest-read-toggle`'s staged value — whether the home nest reads
    /// the room. `None` where there is no such read to stage: an end-to-end
    /// room, or a community room whose answer this device has not read yet
    /// (`RoomSnapshot::nest_read`); the toggle is not painted there.
    pub nest_read: Option<bool>,
    /// The projected nest read the staged one is diffed against.
    pub seed_nest_read: Option<bool>,
    /// The staged labeler set — published labeler ids, lowercase hex, sorted —
    /// the editor's per-labeler toggle state. `None` where there is none to
    /// stage (`RoomSnapshot::labelers`): the control is not painted there.
    /// At most `fauna_mls::room_policy::MAX_ROOM_LABELERS` long.
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub labelers: Option<Vec<String>>,
    /// The projected set the staged one is diffed against.
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub seed_labelers: Option<Vec<String>>,
}

impl RoomSettingsDraft {
    /// The editor's opening state, read off the projected room. `None` on a
    /// thread that is no room or a policy-less room (no policy to edit) — the
    /// button is greyed there, so this is the belt behind that gate.
    pub fn seed(detail: &ThreadDetail) -> Option<Self> {
        let room = detail.room.as_ref()?;
        let policy = room.policy.as_ref()?;
        let admins: Vec<bool> = room
            .members
            .iter()
            .map(|m| m.role == Some(RoomRole::Admin))
            .collect();
        let owners: Vec<bool> = room
            .members
            .iter()
            .map(|m| m.role == Some(RoomRole::Owner))
            .collect();
        let eligible = owners
            .iter()
            .enumerate()
            .map(|(i, &is_owner)| {
                !is_owner && matches!(detail.participants.get(i), Some(TypedAddress::Fauna { .. }))
            })
            .collect();
        // The identity column, captured with the flags it belongs to.
        let seats: Vec<String> = detail
            .participants
            .iter()
            .map(|p| p.person_actor_id().map(|a| a.to_hex()).unwrap_or_default())
            .collect();
        Some(Self {
            join_rule: policy.join_rule,
            history_policy: policy.history_policy,
            seats,
            admins: admins.clone(),
            owners,
            eligible,
            transfer_to: None,
            seed_join_rule: policy.join_rule,
            seed_history_policy: policy.history_policy,
            seed_admins: admins,
            nest_read: room.nest_read,
            seed_nest_read: room.nest_read,
            labelers: room.labelers.clone().map(sorted),
            seed_labelers: room.labelers.clone().map(sorted),
        })
    }

    /// `room-nest-read-toggle` — flip the staged home-nest read. A no-op where
    /// there is none to stage ([`Self::nest_read`] `None`).
    pub fn toggle_nest_read(&mut self) {
        if let Some(reads) = self.nest_read.as_mut() {
            *reads = !*reads;
        }
    }

    /// The labeler toggle for one published labeler (`labeler`, lowercase hex):
    /// name it if the staged set does not, drop it if it does. A no-op where
    /// there is no set to stage, and for a name past
    /// `fauna_mls::room_policy::MAX_ROOM_LABELERS` — the nest refuses a larger
    /// set, and a painter greys the remaining toggles once the set is full.
    pub fn toggle_labeler(&mut self, labeler: &str) {
        let Some(set) = self.labelers.as_mut() else {
            return;
        };
        let labeler = labeler.to_ascii_lowercase();
        if let Some(i) = set.iter().position(|l| *l == labeler) {
            set.remove(i);
        } else if set.len() < fauna_mls::room_policy::MAX_ROOM_LABELERS {
            set.push(labeler);
            set.sort();
        }
    }

    /// Whether the staged set names `labeler` — the `checked` a painter puts
    /// on `room-labeler-toggle[i]`. Ids compare lowercase, as the record
    /// stores them.
    pub fn labeler_staged(&self, labeler: &str) -> bool {
        self.labelers
            .as_ref()
            .is_some_and(|set| set.iter().any(|l| l.eq_ignore_ascii_case(labeler)))
    }

    /// Whether `labeler`'s toggle may act, capability aside: there is a set to
    /// stage, and either it already names this one (un-naming is always open)
    /// or it has room for one more. A full set greys only the rows it does not
    /// name — the painter adds its own `capabilities.can_set_policy` term.
    pub fn labeler_toggle_live(&self, labeler: &str) -> bool {
        match self.labelers.as_ref() {
            None => false,
            Some(set) => {
                self.labeler_staged(labeler)
                    || set.len() < fauna_mls::room_policy::MAX_ROOM_LABELERS
            }
        }
    }

    /// `room-join-rule-select`'s `select` — a token the picker offers
    /// (`JoinRule::from_token`); an unknown token stages nothing.
    pub fn set_join_rule_token(&mut self, token: &str) {
        if let Some(rule) = JoinRule::from_token(token) {
            self.join_rule = rule;
        }
    }

    /// `room-history-policy-select`'s `select`.
    pub fn set_history_policy_token(&mut self, token: &str) {
        if let Some(policy) = HistoryPolicy::from_token(token) {
            self.history_policy = policy;
        }
    }

    /// The seeded slot a **live** participant index refers to, or `None` when
    /// this draft never saw that participant (one seated after the editor
    /// opened) or the row carries no actor id.
    ///
    /// The one translation every gesture and every painter goes through. It is
    /// a scan rather than a map because a room's roster is a handful of rows
    /// and a `Vec<String>` is what crosses the FFI as a record field; the cost
    /// is a keystroke's worth of comparisons.
    pub fn slot_of(&self, index: usize, participants: &[TypedAddress]) -> Option<usize> {
        let actor = participants.get(index)?.person_actor_id()?.to_hex();
        self.seats.iter().position(|s| *s == actor)
    }

    /// `room-admin-toggle[i]` — flip the staged admin flag for the person at
    /// **live** index `i`. A row this draft never saw, one with no actor id,
    /// or an ineligible one stages nothing (the control is greyed there
    /// anyway).
    pub fn toggle_admin(&mut self, index: usize, participants: &[TypedAddress]) {
        let Some(slot) = self.slot_of(index, participants) else {
            return;
        };
        if !self.eligible.get(slot).copied().unwrap_or(false) {
            return;
        }
        if let Some(flag) = self.admins.get_mut(slot) {
            *flag = !*flag;
        }
    }

    /// `room-owner-transfer-button[i]` — stage the person at **live** index
    /// `i` as the new owner, or un-stage them if they are already staged.
    /// **At most one row is ever staged: staging a second un-stages the
    /// first** (`ui/conversations.md` § Element IDs).
    pub fn toggle_transfer(&mut self, index: usize, participants: &[TypedAddress]) {
        let Some(slot) = self.slot_of(index, participants) else {
            return;
        };
        if !self.eligible.get(slot).copied().unwrap_or(false) {
            return;
        }
        let slot = slot as u32;
        self.transfer_to = if self.transfer_to == Some(slot) {
            None
        } else {
            Some(slot)
        };
    }

    /// Whether either control may act on the person at **live** index `index`,
    /// before the viewer's capability is consulted — see [`Self::eligible`].
    ///
    /// A participant seated after the editor opened is not eligible: the draft
    /// holds no seed to diff their flag against, so a toggle could only
    /// invent one.
    pub fn is_eligible(&self, index: usize, participants: &[TypedAddress]) -> bool {
        self.slot_of(index, participants)
            .and_then(|slot| self.eligible.get(slot).copied())
            .unwrap_or(false)
    }

    /// The staged admin flag to PAINT at **live** index `index` —
    /// `room-admin-toggle[i]`'s `checked`. `false` for a row this draft never
    /// saw, which is what an unstaged newcomer should read as.
    ///
    /// Painters must use this rather than indexing `admins` directly: reading
    /// the seeded vector at a live index is the same defect on the paint side,
    /// and it lands the checkmark on the wrong row *in the same frame*, so the
    /// owner cannot see what they are about to sign.
    pub fn admin_at(&self, index: usize, participants: &[TypedAddress]) -> bool {
        self.slot_of(index, participants)
            .and_then(|slot| self.admins.get(slot).copied())
            .unwrap_or(false)
    }

    /// Whether the person at **live** index `index` is the room's owner — the
    /// guard that keeps the owner's own row un-togglable. Same rule as
    /// [`Self::admin_at`]: resolved, never indexed.
    pub fn is_owner_at(&self, index: usize, participants: &[TypedAddress]) -> bool {
        self.slot_of(index, participants)
            .and_then(|slot| self.owners.get(slot).copied())
            .unwrap_or(false)
    }

    /// Whether the person at **live** index `index` is the row staged for the
    /// hand-over — `room-owner-transfer-button[i]`'s pressed state.
    pub fn transfer_staged_at(&self, index: usize, participants: &[TypedAddress]) -> bool {
        match (self.transfer_to, self.slot_of(index, participants)) {
            (Some(staged), Some(slot)) => staged as usize == slot,
            _ => false,
        }
    }

    /// The commits Save issues, in order: the two rules first, then one
    /// appointment or demotion per participant whose staged flag differs from
    /// the seed, and the hand-over **last** — after it lands this seat is a
    /// plain member and could commit none of the others. Only a `Fauna`
    /// participant can be appointed or handed the room (a room's members are
    /// user principals); any other address is skipped.
    pub fn edits(&self, participants: &[TypedAddress]) -> Vec<RoomSettingsEdit> {
        let mut out = Vec::new();
        if self.join_rule != self.seed_join_rule {
            out.push(RoomSettingsEdit::JoinRule {
                rule: self.join_rule,
            });
        }
        if self.history_policy != self.seed_history_policy {
            out.push(RoomSettingsEdit::HistoryPolicy {
                policy: self.history_policy,
            });
        }
        // Walk the LIVE list, but read each row's staged flags through the
        // identity column — so a roster that moved under the editor changes
        // which rows are still actionable, never WHO a staged flag was about.
        // A live row this draft never saw resolves to no slot and is skipped;
        // a seeded row that has since left is simply never reached.
        for (i, addr) in participants.iter().enumerate() {
            let Some(slot) = self.slot_of(i, participants) else {
                continue;
            };
            let (Some(&staged), Some(&seed)) = (self.admins.get(slot), self.seed_admins.get(slot))
            else {
                continue;
            };
            if staged == seed || !matches!(addr, TypedAddress::Fauna { .. }) {
                continue;
            }
            out.push(if staged {
                RoomSettingsEdit::Appoint {
                    address: addr.clone(),
                }
            } else {
                RoomSettingsEdit::Demote {
                    address: addr.clone(),
                }
            });
        }
        // The nest read is a rotation, and a rotation is the owner's or an
        // admin's act — so it goes before the hand-over, which would leave this
        // seat a plain member.
        if let (Some(reads), Some(seed)) = (self.nest_read, self.seed_nest_read)
            && reads != seed
        {
            out.push(RoomSettingsEdit::NestRead { reads });
        }
        // The labeler set, whole — it is one signed record — and like the read
        // an owner's or admin's act, so before the hand-over too.
        if let (Some(set), Some(seed)) = (&self.labelers, &self.seed_labelers)
            && set != seed
        {
            out.push(RoomSettingsEdit::Labelers {
                labelers: set.clone(),
            });
        }
        // The hand-over names a seeded slot, so it is resolved the other way:
        // find the LIVE address of the actor that slot holds. A staged heir
        // who has left the room emits nothing — handing the room to a
        // departed member would strand it.
        if let Some(slot) = self.transfer_to.map(|i| i as usize)
            && !self.owners.get(slot).copied().unwrap_or(false)
            && let Some(actor) = self.seats.get(slot).filter(|a| !a.is_empty())
            && let Some(addr @ TypedAddress::Fauna { .. }) = participants
                .iter()
                .find(|p| p.person_actor_id().map(|a| a.to_hex()).as_ref() == Some(actor))
        {
            out.push(RoomSettingsEdit::TransferOwnership {
                address: addr.clone(),
            });
        }
        out
    }
}

// ── the FFI twins ──
//
// UniFFI passes a record by value, so each staging gesture is "hand me the
// draft, take back the staged one" — the shape a Swift/Kotlin editor already
// uses for every other value type it holds.

/// FFI-exported twin of [`RoomSettingsDraft::seed`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_settings_seed(detail: ThreadDetail) -> Option<RoomSettingsDraft> {
    RoomSettingsDraft::seed(&detail)
}

/// FFI-exported twin of [`RoomSettingsDraft::set_join_rule_token`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_settings_set_join_rule(draft: RoomSettingsDraft, token: String) -> RoomSettingsDraft {
    let mut draft = draft;
    draft.set_join_rule_token(&token);
    draft
}

/// FFI-exported twin of [`RoomSettingsDraft::set_history_policy_token`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_settings_set_history_policy(
    draft: RoomSettingsDraft,
    token: String,
) -> RoomSettingsDraft {
    let mut draft = draft;
    draft.set_history_policy_token(&token);
    draft
}

/// FFI-exported twin of [`RoomSettingsDraft::toggle_nest_read`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_settings_toggle_nest_read(draft: RoomSettingsDraft) -> RoomSettingsDraft {
    let mut draft = draft;
    draft.toggle_nest_read();
    draft
}

/// FFI-exported twin of [`RoomSettingsDraft::toggle_labeler`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_settings_toggle_labeler(
    draft: RoomSettingsDraft,
    labeler: String,
) -> RoomSettingsDraft {
    let mut draft = draft;
    draft.toggle_labeler(&labeler);
    draft
}

/// FFI-exported twin of [`RoomSettingsDraft::labeler_staged`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_settings_labeler_staged(draft: RoomSettingsDraft, labeler: String) -> bool {
    draft.labeler_staged(&labeler)
}

/// FFI-exported twin of [`RoomSettingsDraft::labeler_toggle_live`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_settings_labeler_toggle_live(draft: RoomSettingsDraft, labeler: String) -> bool {
    draft.labeler_toggle_live(&labeler)
}

/// Whether a community room may name a labeler of this artifact kind
/// (`LabelerCatalogEntry::artifact_kind`, already normalized by the catalog
/// machine) — the filter every editor applies to the catalog before painting
/// a `room-labeler-toggle` row, over the list the home nest admits by
/// (`fauna_mls::room_policy::ROOM_LABELER_KINDS`).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_may_name_labeler_kind(kind: String) -> bool {
    fauna_mls::room_policy::ROOM_LABELER_KINDS.contains(&kind.as_str())
}

/// A labeler set in the one order the signed record accepts — lowercase hex,
/// sorted, deduplicated — so a draft seeded from a room diffs equal to it.
fn sorted(mut set: Vec<String>) -> Vec<String> {
    for id in &mut set {
        id.make_ascii_lowercase();
    }
    set.sort();
    set.dedup();
    set
}

/// FFI-exported twin of [`RoomSettingsDraft::toggle_admin`].
///
/// Takes the **live** participant list beside the paint index, like
/// [`room_settings_edits`] already does: the index names a row on screen, and
/// only the live list says who that is (`RoomSettingsDraft`'s own doc carries
/// the argument).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_settings_toggle_admin(
    draft: RoomSettingsDraft,
    participants: Vec<TypedAddress>,
    index: u32,
) -> RoomSettingsDraft {
    let mut draft = draft;
    draft.toggle_admin(index as usize, &participants);
    draft
}

/// FFI-exported twin of [`RoomSettingsDraft::toggle_transfer`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_settings_toggle_transfer(
    draft: RoomSettingsDraft,
    participants: Vec<TypedAddress>,
    index: u32,
) -> RoomSettingsDraft {
    let mut draft = draft;
    draft.toggle_transfer(index as usize, &participants);
    draft
}

/// FFI-exported twin of [`RoomSettingsDraft::admin_at`] — the `checked` a
/// painter puts on `room-admin-toggle[i]`.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_settings_admin_at(
    draft: RoomSettingsDraft,
    participants: Vec<TypedAddress>,
    index: u32,
) -> bool {
    draft.admin_at(index as usize, &participants)
}

/// FFI-exported twin of [`RoomSettingsDraft::is_eligible`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_settings_is_eligible(
    draft: RoomSettingsDraft,
    participants: Vec<TypedAddress>,
    index: u32,
) -> bool {
    draft.is_eligible(index as usize, &participants)
}

/// FFI-exported twin of [`RoomSettingsDraft::is_owner_at`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_settings_is_owner_at(
    draft: RoomSettingsDraft,
    participants: Vec<TypedAddress>,
    index: u32,
) -> bool {
    draft.is_owner_at(index as usize, &participants)
}

/// FFI-exported twin of [`RoomSettingsDraft::transfer_staged_at`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_settings_transfer_staged_at(
    draft: RoomSettingsDraft,
    participants: Vec<TypedAddress>,
    index: u32,
) -> bool {
    draft.transfer_staged_at(index as usize, &participants)
}

/// FFI-exported twin of [`RoomSettingsDraft::edits`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn room_settings_edits(
    draft: RoomSettingsDraft,
    participants: Vec<TypedAddress>,
) -> Vec<RoomSettingsEdit> {
    draft.edits(&participants)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::derive_capabilities;
    use crate::compose::ComposeState;
    use crate::room::{
        PrincipalKind, RoomClass, RoomMemberSnapshot, RoomPolicySnapshot, RoomSnapshot,
    };
    use crate::thread::{ThreadFlavor, ThreadId};
    use crate::{Rail, TypedAddress};
    use fauna_core::identity::ActorId;

    fn fauna(handle: &str) -> TypedAddress {
        TypedAddress::Fauna {
            handle: handle.into(),
            actor_id: ActorId([handle.as_bytes()[0]; 32]),
        }
    }

    /// A governed three-seat room: alice owns it, bob is an admin, carol a
    /// plain member. `roles` overrides that when a case needs another shape.
    fn governed(participants: Vec<TypedAddress>, roles: Vec<Option<RoomRole>>) -> ThreadDetail {
        let capabilities = derive_capabilities(Rail::FaunaMls, ThreadFlavor::MlsGroup);
        ThreadDetail {
            thread_id: ThreadId("t-room".into()),
            rail: Rail::FaunaMls,
            glyph: Rail::FaunaMls.glyph(),
            flavor: ThreadFlavor::MlsGroup,
            label: "room".into(),
            participant_displays: participants.iter().map(|p| p.display()).collect(),
            capabilities,
            messages: vec![],
            compose: ComposeState::default(),
            selected_message_id: None,
            bridge: None,
            guardian_state: None,
            room: Some(RoomSnapshot {
                class: RoomClass::EndToEnd,
                members: roles
                    .iter()
                    .map(|role| RoomMemberSnapshot {
                        kind: PrincipalKind::User,
                        role: *role,
                    })
                    .collect(),
                policy: Some(RoomPolicySnapshot {
                    version: 1,
                    name: None,
                    join_rule: JoinRule::Invite,
                    history_policy: HistoryPolicy::None,
                }),
                my_role: roles.first().copied().flatten(),
                nest_read: None,
                labelers: None,
                awaiting_key: false,
                moderation_unverified: false,
                pending_invites: None,
            }),
            participants,
        }
    }

    fn three_seats() -> ThreadDetail {
        governed(
            vec![fauna("alice"), fauna("bob"), fauna("carol")],
            vec![
                Some(RoomRole::Owner),
                Some(RoomRole::Admin),
                Some(RoomRole::Member),
            ],
        )
    }

    /// **A roster that shifts under an open editor must not move the owner's
    /// choice onto somebody else**.
    ///
    /// Every staged vector is seeded index-parallel with the participant list
    /// *as it stood when the editor opened*, and Save diffs them against the
    /// list *as it stands now*. Those are not the same list: since the roster
    /// reconcile landed, an inbound membership commit drops a row from the
    /// middle of the live vector while the overlay is open
    /// (`ConversationsManager::apply_inbound_roster` →
    /// `ThreadStore::retain_participants`), shifting every later participant
    /// one place left.
    ///
    /// Positionally, the owner's `true` at slot 2 then lands on whoever slid
    /// into slot 2 — and `update_room_policy` signs it with the owner's own
    /// credential, so every member verifies and accepts an appointment the
    /// owner never made. The person they *did* choose is not appointed.
    ///
    /// The fix is to key the staged flags by identity, so this asserts the
    /// emitted edit names the actor the owner toggled — not the one standing
    /// where they used to.
    #[test]
    fn a_roster_that_shifts_mid_edit_cannot_move_a_staged_appointment() {
        let opened = governed(
            vec![fauna("alice"), fauna("bob"), fauna("carol"), fauna("dave")],
            vec![
                Some(RoomRole::Owner),
                Some(RoomRole::Member),
                Some(RoomRole::Member),
                Some(RoomRole::Member),
            ],
        );
        let mut draft = RoomSettingsDraft::seed(&opened).expect("a governed room seeds");

        // The owner stages carol — slot 2 as the list stands now.
        draft.toggle_admin(2, &opened.participants);
        assert_eq!(
            draft.edits(&opened.participants),
            vec![RoomSettingsEdit::Appoint {
                address: fauna("carol")
            }],
            "against an unmoved list the staged appointment names carol"
        );

        // An admin removes bob; the inbound commit drops him from the middle
        // and dave slides up. Nothing re-seeds the draft — the overlay is
        // still open, and only Esc or a thread switch clears it.
        let live = vec![fauna("alice"), fauna("carol"), fauna("dave")];

        assert_eq!(
            draft.edits(&live),
            vec![RoomSettingsEdit::Appoint {
                address: fauna("carol")
            }],
            "the owner chose CAROL — a roster that moved under the editor must \
             not turn that into an appointment of whoever now stands at slot 2"
        );
    }

    /// **The paint twins must resolve by identity too, not only `edits()`.**
    /// The test above only pins Save; every painter reads the staged state
    /// through `admin_at`, `is_eligible`, `is_owner_at` and
    /// `transfer_staged_at` on every frame the overlay is open, so a
    /// positional regression in any one of those puts the checkmark on the
    /// wrong row *before* Save is ever pressed — the owner's natural
    /// correction (untoggle the "wrong" row) then stages a real wrong
    /// appointment.
    ///
    /// The roster opens with the owner **not** at slot 0
    /// (`[bob, alice(owner), carol, dave]`), so dropping bob also moves the
    /// owner's live slot — a shift that only touched trailing rows would let
    /// a positional `owners.get(index)` coincide with the identity-correct
    /// answer at slot 0 and hide the defect.
    #[test]
    fn a_roster_that_shifts_mid_edit_repaints_every_twin_by_identity() {
        let opened = governed(
            vec![fauna("bob"), fauna("alice"), fauna("carol"), fauna("dave")],
            vec![
                Some(RoomRole::Member),
                Some(RoomRole::Owner),
                Some(RoomRole::Member),
                Some(RoomRole::Member),
            ],
        );
        let mut draft = RoomSettingsDraft::seed(&opened).expect("a governed room seeds");
        draft.toggle_admin(2, &opened.participants); // carol
        draft.toggle_transfer(3, &opened.participants); // dave

        // Bob leaves; alice, carol and dave each slide one place left.
        let live = vec![fauna("alice"), fauna("carol"), fauna("dave")];

        assert!(
            draft.admin_at(1, &live),
            "carol's staged admin flag must follow her to live slot 1 — a \
             positional read of live slot 1 would land on alice's old slot \
             and answer false"
        );
        assert!(!draft.admin_at(0, &live), "alice was never staged as admin");

        assert!(
            draft.is_owner_at(0, &live),
            "alice is still the owner at her new live slot 0 — a positional \
             read would land on bob's old slot 0, which was never the owner"
        );
        assert!(
            !draft.is_owner_at(1, &live),
            "carol is not the owner, though a positional read of live slot 1 \
             would land on alice's old slot and answer true"
        );

        assert!(
            draft.is_eligible(1, &live),
            "carol is still eligible at her new live slot — a positional \
             read of live slot 1 would land on the owner's old slot and \
             answer false"
        );
        assert!(
            !draft.is_eligible(0, &live),
            "alice, the owner, is never eligible — a positional read of live \
             slot 0 would land on bob's old slot and answer true"
        );

        assert!(
            draft.transfer_staged_at(2, &live),
            "dave is still the staged heir at his new live slot 2 — a \
             positional read comparing the seeded slot (3) against the live \
             index (2) directly, without resolving identity first, answers \
             false"
        );
        assert!(
            !draft.transfer_staged_at(0, &live),
            "alice was never staged for the hand-over"
        );
    }

    /// The other half of the same rule: a row the owner staged who is **gone**
    /// from the live roster emits nothing at all.
    ///
    /// Skipping is the only honest answer. The appointment cannot be
    /// meaningful — `update_room_policy` pushes the actor into `policy.admins`
    /// with no membership check of its own, so emitting it would write a
    /// departed member into the room's signed policy — and refusing the whole
    /// Save would discard the owner's other, still-valid choices.
    #[test]
    fn a_staged_row_that_left_the_roster_emits_nothing() {
        let opened = governed(
            vec![fauna("alice"), fauna("bob"), fauna("carol")],
            vec![
                Some(RoomRole::Owner),
                Some(RoomRole::Member),
                Some(RoomRole::Member),
            ],
        );
        let mut draft = RoomSettingsDraft::seed(&opened).expect("seeds");
        draft.toggle_admin(1, &opened.participants); // bob
        draft.toggle_admin(2, &opened.participants); // carol

        // Bob leaves while the editor is open.
        let live = vec![fauna("alice"), fauna("carol")];
        assert_eq!(
            draft.edits(&live),
            vec![RoomSettingsEdit::Appoint {
                address: fauna("carol")
            }],
            "carol's appointment still stands; bob's is dropped rather than \
             written into the policy for a member the room no longer has"
        );
    }

    /// A hand-over is staged by identity too — the same defect, on the control
    /// that gives away the room.
    #[test]
    fn a_roster_that_shifts_mid_edit_cannot_move_a_staged_hand_over() {
        let opened = governed(
            vec![fauna("alice"), fauna("bob"), fauna("carol"), fauna("dave")],
            vec![
                Some(RoomRole::Owner),
                Some(RoomRole::Member),
                Some(RoomRole::Member),
                Some(RoomRole::Member),
            ],
        );
        let mut draft = RoomSettingsDraft::seed(&opened).expect("seeds");
        draft.toggle_transfer(3, &opened.participants); // dave

        let live = vec![fauna("alice"), fauna("carol"), fauna("dave")];
        assert_eq!(
            draft.edits(&live),
            vec![RoomSettingsEdit::TransferOwnership {
                address: fauna("dave")
            }],
            "the room is handed to dave, whom the owner chose - never to \
             whoever slid into slot 3"
        );
    }

    /// A member who joined **after** the editor opened was never staged, so
    /// Save says nothing about them — it does not read a neighbour's flag off
    /// the end of the seeded vectors, and it does not treat "absent from the
    /// draft" as a demotion.
    #[test]
    fn a_member_seated_after_the_editor_opened_is_not_staged_either_way() {
        let opened = governed(
            vec![fauna("alice"), fauna("bob")],
            vec![Some(RoomRole::Owner), Some(RoomRole::Admin)],
        );
        let draft = RoomSettingsDraft::seed(&opened).expect("seeds");

        // Erin is added by another device while the overlay sits open.
        let live = vec![fauna("alice"), fauna("bob"), fauna("erin")];
        assert_eq!(
            draft.edits(&live),
            vec![],
            "an untouched draft stages nothing, and a newcomer the owner never \
             saw must not acquire or lose a role by arriving"
        );
    }

    #[test]
    fn seed_reads_the_projected_policy_and_roles() {
        let draft = RoomSettingsDraft::seed(&three_seats()).expect("a governed room seeds");
        assert_eq!(draft.join_rule, JoinRule::Invite);
        assert_eq!(draft.history_policy, HistoryPolicy::None);
        assert_eq!(draft.admins, vec![false, true, false]);
        assert_eq!(draft.owners, vec![true, false, false]);
        assert_eq!(draft.transfer_to, None);
    }

    #[test]
    fn a_policy_less_room_and_a_non_room_seed_nothing() {
        let mut policy_less = three_seats();
        policy_less.room.as_mut().unwrap().policy = None;
        assert!(
            RoomSettingsDraft::seed(&policy_less).is_none(),
            "a policy-less room has no policy to edit"
        );
        let mut plain = three_seats();
        plain.room = None;
        assert!(RoomSettingsDraft::seed(&plain).is_none());
    }

    #[test]
    fn eligibility_excludes_the_owner_and_every_non_fauna_seat() {
        // A bridge seat sits where a room's member list would never put one,
        // but the editor must still refuse to appoint or hand it the room.
        let detail = governed(
            vec![
                fauna("alice"),
                fauna("bob"),
                TypedAddress::Email {
                    email_address: "mta@example.test".into(),
                },
            ],
            vec![
                Some(RoomRole::Owner),
                Some(RoomRole::Member),
                Some(RoomRole::Member),
            ],
        );
        let draft = RoomSettingsDraft::seed(&detail).unwrap();
        assert_eq!(
            draft.eligible,
            vec![false, true, false],
            "the owner's own row and a non-Fauna row are never live"
        );
    }

    #[test]
    fn an_unoffered_token_stages_nothing() {
        let mut draft = RoomSettingsDraft::seed(&three_seats()).unwrap();
        draft.set_join_rule_token("no-such-rule");
        assert_eq!(draft.join_rule, JoinRule::Invite);
        draft.set_history_policy_token("sometimes");
        assert_eq!(draft.history_policy, HistoryPolicy::None);
        draft.set_join_rule_token("member-invite");
        assert_eq!(draft.join_rule, JoinRule::MemberInvite);
    }

    #[test]
    fn an_ineligible_row_stages_nothing() {
        let detail = three_seats();
        let seats = &detail.participants;
        let mut draft = RoomSettingsDraft::seed(&detail).unwrap();
        draft.toggle_admin(0, seats); // the owner's own row
        draft.toggle_transfer(0, seats);
        draft.toggle_admin(99, seats); // out of range
        assert_eq!(draft.admins, vec![false, true, false]);
        assert_eq!(draft.transfer_to, None);
    }

    #[test]
    fn at_most_one_row_is_staged_for_the_hand_over() {
        let detail = three_seats();
        let seats = &detail.participants;
        let mut draft = RoomSettingsDraft::seed(&detail).unwrap();
        draft.toggle_transfer(1, seats);
        assert_eq!(draft.transfer_to, Some(1));
        // Staging a second un-stages the first (`ui/conversations.md`
        // § Element IDs).
        draft.toggle_transfer(2, seats);
        assert_eq!(draft.transfer_to, Some(2));
        // Tapping the staged row again clears it.
        draft.toggle_transfer(2, seats);
        assert_eq!(draft.transfer_to, None);
    }

    #[test]
    fn only_changed_values_become_edits_and_the_hand_over_is_last() {
        let detail = three_seats();
        let mut draft = RoomSettingsDraft::seed(&detail).unwrap();
        assert!(
            draft.edits(&detail.participants).is_empty(),
            "an untouched draft commits nothing"
        );

        draft.set_history_policy_token("full");
        draft.toggle_admin(2, &detail.participants); // appoint carol
        draft.toggle_admin(1, &detail.participants); // demote bob
        draft.toggle_transfer(1, &detail.participants); // hand the room to bob

        let edits = draft.edits(&detail.participants);
        assert_eq!(edits.len(), 4, "{edits:?}");
        assert_eq!(
            edits[0],
            RoomSettingsEdit::HistoryPolicy {
                policy: HistoryPolicy::Full
            },
            "the rules go first"
        );
        assert!(
            matches!(&edits[1], RoomSettingsEdit::Demote { address } if address == &fauna("bob")),
            "{edits:?}"
        );
        assert!(
            matches!(&edits[2], RoomSettingsEdit::Appoint { address } if address == &fauna("carol")),
            "{edits:?}"
        );
        assert!(
            matches!(
                &edits[3],
                RoomSettingsEdit::TransferOwnership { address } if address == &fauna("bob")
            ),
            "the hand-over is always last: {edits:?}"
        );
    }

    #[test]
    fn the_ffi_twins_stage_the_same_draft_the_methods_do() {
        let detail = three_seats();
        let draft = room_settings_seed(detail.clone()).expect("a governed room seeds");
        let draft = room_settings_set_history_policy(draft, "full".to_string());
        let draft = room_settings_toggle_admin(draft, detail.participants.clone(), 2);
        let draft = room_settings_toggle_transfer(draft, detail.participants.clone(), 1);

        let mut expected = RoomSettingsDraft::seed(&detail).unwrap();
        expected.set_history_policy_token("full");
        expected.toggle_admin(2, &detail.participants);
        expected.toggle_transfer(1, &detail.participants);
        assert_eq!(draft, expected);

        // The read twins answer what the painters would paint.
        assert!(room_settings_admin_at(
            draft.clone(),
            detail.participants.clone(),
            2
        ));
        assert!(!room_settings_is_eligible(
            draft.clone(),
            detail.participants.clone(),
            0
        ));
        assert!(room_settings_is_owner_at(
            draft.clone(),
            detail.participants.clone(),
            0
        ));
        assert!(room_settings_transfer_staged_at(
            draft.clone(),
            detail.participants.clone(),
            1
        ));

        assert_eq!(
            room_settings_edits(draft, detail.participants.clone()),
            expected.edits(&detail.participants)
        );
    }

    /// **The home nest's read stages like any other control, and commits
    /// before the hand-over.** A rotation is the owner's or an admin's act, so
    /// once a staged hand-over has landed this seat could no longer mint —
    /// the same reason the hand-over is last for everything else.
    #[test]
    fn a_staged_nest_read_commits_before_the_hand_over() {
        let mut detail = three_seats();
        if let Some(room) = detail.room.as_mut() {
            room.class = RoomClass::Community;
            room.nest_read = Some(true);
        }
        let mut draft = RoomSettingsDraft::seed(&detail).expect("a governed room seeds");
        assert_eq!(draft.nest_read, Some(true), "seeded off the projected room");
        assert!(
            draft.edits(&detail.participants).is_empty(),
            "an untouched toggle stages nothing"
        );

        draft.toggle_nest_read();
        draft.toggle_transfer(2, &detail.participants);
        let edits = draft.edits(&detail.participants);
        assert_eq!(
            edits.first(),
            Some(&RoomSettingsEdit::NestRead { reads: false }),
            "the withdrawal is staged: {edits:?}"
        );
        assert!(
            matches!(
                edits.last(),
                Some(RoomSettingsEdit::TransferOwnership { .. })
            ),
            "and the hand-over still goes last: {edits:?}"
        );

        draft.toggle_nest_read();
        assert!(
            !draft
                .edits(&detail.participants)
                .iter()
                .any(|e| matches!(e, RoomSettingsEdit::NestRead { .. })),
            "toggled back to the seed, it stages nothing"
        );
    }

    /// Where there is no nest read to stage — an end-to-end room, or a
    /// community room whose answer is not in — the toggle is inert rather than
    /// inventing a value to commit.
    #[test]
    fn a_room_with_no_nest_read_stages_none() {
        let detail = three_seats();
        let mut draft = RoomSettingsDraft::seed(&detail).expect("a governed room seeds");
        assert_eq!(draft.nest_read, None);
        draft.toggle_nest_read();
        assert_eq!(draft.nest_read, None);
        assert!(draft.edits(&detail.participants).is_empty());
    }

    fn labeler(byte: u8) -> String {
        hex::encode([byte; 32])
    }

    /// **The room's labeler set stages like the nest read, and commits before
    /// the hand-over** — naming what reads the room is the owner's or an
    /// admin's act (`conversation-rooms.md` rule 6), which a seat that has just
    /// handed the room away no longer holds.
    #[test]
    fn a_staged_labeler_set_commits_as_one_edit_before_the_hand_over() {
        let mut detail = three_seats();
        if let Some(room) = detail.room.as_mut() {
            room.class = RoomClass::Community;
            room.labelers = Some(vec![labeler(0x1A)]);
        }
        let mut draft = RoomSettingsDraft::seed(&detail).expect("a governed room seeds");
        assert_eq!(
            draft.labelers,
            Some(vec![labeler(0x1A)]),
            "seeded off the room"
        );
        assert!(draft.edits(&detail.participants).is_empty());

        draft.toggle_labeler(&labeler(0x1C));
        draft.toggle_labeler(&labeler(0x1A));
        draft.toggle_transfer(2, &detail.participants);
        let edits = draft.edits(&detail.participants);
        assert_eq!(
            edits.first(),
            Some(&RoomSettingsEdit::Labelers {
                labelers: vec![labeler(0x1C)],
            }),
            "the whole staged set, as one edit: {edits:?}"
        );
        assert!(
            matches!(
                edits.last(),
                Some(RoomSettingsEdit::TransferOwnership { .. })
            ),
            "and the hand-over still goes last: {edits:?}"
        );

        draft.toggle_labeler(&labeler(0x1C));
        draft.toggle_labeler(&labeler(0x1A));
        assert!(
            !draft
                .edits(&detail.participants)
                .iter()
                .any(|e| matches!(e, RoomSettingsEdit::Labelers { .. })),
            "toggled back to the seed, it stages nothing"
        );
    }

    /// The set is bounded by what the nest admits — every named labeler runs
    /// on every send — so a toggle past the bound stages nothing rather than a
    /// set Save would carry to a refusal. And a room with no set to stage (an
    /// end-to-end room, or a community room whose set did not verify) stages
    /// none.
    #[test]
    fn the_labeler_set_is_bounded_and_absent_where_there_is_none_to_stage() {
        let mut detail = three_seats();
        if let Some(room) = detail.room.as_mut() {
            room.labelers = Some(Vec::new());
        }
        let mut draft = RoomSettingsDraft::seed(&detail).expect("a governed room seeds");
        for byte in 0..=fauna_mls::room_policy::MAX_ROOM_LABELERS as u8 {
            draft.toggle_labeler(&labeler(byte));
        }
        assert_eq!(
            draft.labelers.as_ref().map(Vec::len),
            Some(fauna_mls::room_policy::MAX_ROOM_LABELERS)
        );

        let mut none = RoomSettingsDraft::seed(&three_seats()).expect("a governed room seeds");
        assert_eq!(none.labelers, None);
        none.toggle_labeler(&labeler(0x1A));
        assert_eq!(none.labelers, None);
        assert!(none.edits(&three_seats().participants).is_empty());
    }

    /// **What a painter asks of one catalog row, answered once for all seven
    /// apps**: is it staged (the `checked` attribute), may the toggle act (a
    /// full set greys only the rows it does not already name, so un-naming
    /// stays possible), and may a room name that artifact kind at all (the
    /// nest's own admission rule, not a copy of it).
    #[test]
    fn a_painter_reads_staged_live_and_nameable_off_the_draft() {
        let mut detail = three_seats();
        if let Some(room) = detail.room.as_mut() {
            room.labelers = Some(vec![labeler(0x1A)]);
        }
        let mut draft = RoomSettingsDraft::seed(&detail).expect("a governed room seeds");
        assert!(draft.labeler_staged(&labeler(0x1A)));
        assert!(
            draft.labeler_staged(&labeler(0x1A).to_ascii_uppercase()),
            "ids compare as the record stores them, lowercase"
        );
        assert!(!draft.labeler_staged(&labeler(0x1B)));
        assert!(draft.labeler_toggle_live(&labeler(0x1B)), "room to name it");

        for byte in 0x1B..0x1B + fauna_mls::room_policy::MAX_ROOM_LABELERS as u8 {
            draft.toggle_labeler(&labeler(byte));
        }
        assert_eq!(
            draft.labelers.as_ref().map(Vec::len),
            Some(fauna_mls::room_policy::MAX_ROOM_LABELERS)
        );
        let unnamed = labeler(0x1B + fauna_mls::room_policy::MAX_ROOM_LABELERS as u8);
        assert!(
            !draft.labeler_toggle_live(&unnamed),
            "a full set greys a row it does not name"
        );
        assert!(
            draft.labeler_toggle_live(&labeler(0x1A)),
            "and keeps every named row live, so the owner can make room"
        );

        let none = RoomSettingsDraft::seed(&three_seats()).expect("a governed room seeds");
        assert!(!none.labeler_staged(&labeler(0x1A)));
        assert!(
            !none.labeler_toggle_live(&labeler(0x1A)),
            "no set to stage, nothing live"
        );

        assert!(room_may_name_labeler_kind("wasm".into()));
        assert!(room_may_name_labeler_kind("text-model".into()));
        assert!(!room_may_name_labeler_kind("list".into()));
        assert!(!room_may_name_labeler_kind("".into()));
    }
}
