//! **The successor's owed nests, as Settings shows them** —
//! `succession-linked-nests.md` § The road → *What the dial does with an entry
//! it cannot land*, its second rule (*the user ends what the runtime cannot*;
//! ruled 2026-11-07): the Recovery kit section lists each entry the keeping
//! nest serves on `fauna.recovery.succession.status`'s `owed_nests`, with what
//! this device's last delivery found, and a dismiss gesture settles one entry
//! on the user's word (`fauna.recovery.succession.owed_settle`).
//!
//! The projection is shared so all seven apps select the same arm and the same
//! copy: [`OwedNestsView::project`] joins the served list (the presence of an
//! entry is the keeping nest's word, never the device's) with the runtime's
//! last deliveries (`fauna_account_plane::owed_delivery::OwedNestsPass`, read
//! off the account handle), and the apps resolve [`OwedNestsView::status`] and
//! each [`OwedNestEntry::outcome_copy`] through their own string tables.
//!
//! **Copy rule.** The lines say what is still being retired where and let the
//! user drop an entry they do not recognise. They never grade the user safe or
//! unsafe, and never call a dial dangerous: an entry is a fact about the road,
//! not a verdict.
//!
//! **Every keeper.** A nest that applied a statement keeps its own owed nests
//! (§ The road), so the list holds the entries the BOUND nest serves (read on
//! the device's session at hydrate) and those each LINKED nest served the
//! runtime's secondary leg on its last pass (the leg's own read, carried on
//! the account handle). Each entry names its keeper ([`OwedNestEntry::keeper`]):
//! a bound entry's dismiss rides the bound session, a linked entry's opens an
//! owner connection to its keeper through the host's shared
//! [`LinkedNestDial`] — the clause-(c) fan-out's dial, so no app writes a
//! second one — and settles there. The same owed nest kept at two keepers is
//! two entries: each keeper settles only its own record.

use fauna_client_core::succession_delivery::{Delivery, OwedReason};
use fauna_core::localized::LocalizedText;
use fauna_protocol::recovery::OwedNest;
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::linked_fanout::{LinkedNestDial, linked_nests};
use crate::nest::RecoveryClient;

const KEY_STATUS: &str = "settings.recovery_kit.owed_nests_status";
const KEY_OUTCOME_NOT_TRIED: &str = "settings.recovery_kit.owed_nest_not_tried";
const KEY_OUTCOME_LANDED: &str = "settings.recovery_kit.owed_nest_landed";
const KEY_OUTCOME_NO_ADDRESS: &str = "settings.recovery_kit.owed_nest_no_address";
const KEY_OUTCOME_UNREACHABLE: &str = "settings.recovery_kit.owed_nest_unreachable";
const KEY_OUTCOME_OTHER_NEST: &str = "settings.recovery_kit.owed_nest_other_nest";
const KEY_OUTCOME_REFUSED: &str = "settings.recovery_kit.owed_nest_refused";
const KEY_OUTCOME_NO_SEED: &str = "settings.recovery_kit.owed_nest_no_seed";
const KEY_OUTCOME_CHAIN_FORKED: &str = "settings.recovery_kit.owed_nest_chain_forked";
const KEY_OUTCOME_SUCCESSOR_EXISTS: &str = "settings.recovery_kit.owed_nest_successor_exists";
const KEY_OUTCOME_WAITING: &str = "settings.recovery_kit.owed_nest_waiting";
const KEY_ADDRESS: &str = "settings.recovery_kit.owed_nest_address";
const KEY_ADDRESS_NONE: &str = "settings.recovery_kit.owed_nest_address_none";
const KEY_DISMISS_FAILED: &str = "settings.recovery_kit.owed_nest_dismiss_failed";

/// What this device's last delivery of one entry found — the arm the outcome
/// line renders, as a stable token too ([`Self::kind`]) for the boundaries that
/// carry rather than paint it (FFI records, the e2e state protocol).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwedOutcome {
    /// This device has not delivered the entry yet: it holds no seed, another
    /// app on the machine delivers, or no pass has run since sign-in.
    NotTried,
    /// The statement is at the owed nest (or that nest holds no account for
    /// the retired identity); the keeping nest drops the entry once settled.
    Landed,
    /// The burned pairing carried no address, so there is nothing to dial.
    NoAddress,
    /// The owed nest could not be reached.
    Unreachable,
    /// The address answered as another nest — parked on this device until a
    /// relaunch.
    OtherNest,
    /// The owed nest refused, with its code.
    Refused(String),
    /// The owed nest cannot verify, and this device holds no seed to replay
    /// the chain there.
    NoSeed,
    /// The owed nest holds a different recovery chain (stated bound 3).
    ChainForked,
    /// An account for the successor was made there by hand (stated bound 4).
    SuccessorExists,
    /// A wait on the keeping nest itself (unread list, no statement, the
    /// settle unanswered) — asked again next pass.
    Waiting,
}

impl OwedOutcome {
    /// The arm of one delivery.
    #[must_use]
    pub fn of(delivery: &Delivery) -> Self {
        match delivery {
            Delivery::Landed { .. } | Delivery::NoAccount => Self::Landed,
            Delivery::Owed(reason) => match reason {
                OwedReason::NoAddress => Self::NoAddress,
                OwedReason::Unreachable(_) => Self::Unreachable,
                OwedReason::IdentityMismatch { .. } | OwedReason::Parked { .. } => Self::OtherNest,
                OwedReason::Refused(code) => Self::Refused(code.clone()),
                OwedReason::NoSeed { .. } => Self::NoSeed,
                OwedReason::ChainForked => Self::ChainForked,
                OwedReason::SuccessorExists => Self::SuccessorExists,
                OwedReason::Malformed(_)
                | OwedReason::Unread(_)
                | OwedReason::NoStatement
                | OwedReason::Unsettled(_) => Self::Waiting,
            },
        }
    }

    /// The arm as a stable token.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotTried => "not-tried",
            Self::Landed => "landed",
            Self::NoAddress => "no-address",
            Self::Unreachable => "unreachable",
            Self::OtherNest => "other-nest",
            Self::Refused(_) => "refused",
            Self::NoSeed => "no-seed",
            Self::ChainForked => "chain-forked",
            Self::SuccessorExists => "successor-exists",
            Self::Waiting => "waiting",
        }
    }

    /// The outcome line.
    #[must_use]
    pub fn copy(&self) -> LocalizedText {
        match self {
            Self::NotTried => LocalizedText::key(KEY_OUTCOME_NOT_TRIED),
            Self::Landed => LocalizedText::key(KEY_OUTCOME_LANDED),
            Self::NoAddress => LocalizedText::key(KEY_OUTCOME_NO_ADDRESS),
            Self::Unreachable => LocalizedText::key(KEY_OUTCOME_UNREACHABLE),
            Self::OtherNest => LocalizedText::key(KEY_OUTCOME_OTHER_NEST),
            Self::Refused(code) => {
                LocalizedText::key_arg(KEY_OUTCOME_REFUSED, "code", code.clone())
            }
            Self::NoSeed => LocalizedText::key(KEY_OUTCOME_NO_SEED),
            Self::ChainForked => LocalizedText::key(KEY_OUTCOME_CHAIN_FORKED),
            Self::SuccessorExists => LocalizedText::key(KEY_OUTCOME_SUCCESSOR_EXISTS),
            Self::Waiting => LocalizedText::key(KEY_OUTCOME_WAITING),
        }
    }
}

/// One linked keeper's last pass, as [`OwedNestsView::project`] takes it: the
/// keeper's nest id and the entries it served the runtime, each with what its
/// delivery found.
pub type LinkedKeeperPass<'a> = ([u8; 32], &'a [(OwedNest, Delivery)]);

/// What names one entry for a dismiss: its keeper and the settle key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OwedNestKey {
    /// `None` is the bound nest; `Some` the linked nest of that identity.
    pub keeper: Option<[u8; 32]>,
    pub old_actor_id: Vec<u8>,
    pub nest_id: Vec<u8>,
}

/// One owed entry as Settings lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwedNestEntry {
    /// The nest that keeps the entry — where its dismiss settles: `None` is
    /// the bound nest, `Some` the linked nest of that identity.
    pub keeper: Option<[u8; 32]>,
    /// The retired identity whose pairing named the nest — half of the settle
    /// key.
    pub old_actor_id: Vec<u8>,
    /// The owed nest's id — the other half.
    pub nest_id: Vec<u8>,
    /// The owed nest's address, when the burned pairing carried one.
    pub address: Option<String>,
    /// What this device's last delivery found.
    pub outcome: OwedOutcome,
}

impl OwedNestEntry {
    /// The address line: the address, or a sentence saying none was recorded.
    #[must_use]
    pub fn address_copy(&self) -> LocalizedText {
        match self.address.as_deref().filter(|a| !a.is_empty()) {
            Some(a) => LocalizedText::key_arg(KEY_ADDRESS, "address", a),
            None => LocalizedText::key(KEY_ADDRESS_NONE),
        }
    }

    /// The outcome line ([`OwedOutcome::copy`]).
    #[must_use]
    pub fn outcome_copy(&self) -> LocalizedText {
        self.outcome.copy()
    }

    /// A stable key for the entry — what a dismiss names, never a painted
    /// index (a refresh may re-order the list).
    #[must_use]
    pub fn key(&self) -> OwedNestKey {
        OwedNestKey {
            keeper: self.keeper,
            old_actor_id: self.old_actor_id.clone(),
            nest_id: self.nest_id.clone(),
        }
    }
}

/// The owed-nests list Settings shows: the bound nest's entries first, then
/// each linked keeper's, each in its keeping nest's order (earliest hop first,
/// oldest pairing first).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OwedNestsView {
    pub entries: Vec<OwedNestEntry>,
}

impl OwedNestsView {
    /// Join the bound nest's served list with this device's last deliveries
    /// at the bound nest (`deliveries`: the bound keeper's pass, `None` when
    /// the runtime has delivered nothing), and add each linked keeper's
    /// entries from the runtime's last pass at it (`linked`: the keeper's
    /// nest id and that pass's deliveries — the list the keeper served the
    /// leg, each entry with what its delivery found). A linked entry that
    /// landed is not listed: the runtime settled it at its keeper in the same
    /// pass. `None` when nothing is owed — the element renders only for a
    /// successor with a non-empty list.
    #[must_use]
    pub fn project(
        served: &[OwedNest],
        deliveries: Option<&[(OwedNest, Delivery)]>,
        linked: &[LinkedKeeperPass<'_>],
    ) -> Option<Self> {
        let mut entries: Vec<OwedNestEntry> = served
            .iter()
            .map(|owed| {
                let outcome = deliveries
                    .and_then(|d| {
                        d.iter().find(|(o, _)| {
                            o.old_actor_id == owed.old_actor_id && o.nest_id == owed.nest_id
                        })
                    })
                    .map_or(OwedOutcome::NotTried, |(_, delivery)| {
                        OwedOutcome::of(delivery)
                    });
                OwedNestEntry {
                    keeper: None,
                    old_actor_id: owed.old_actor_id.to_vec(),
                    nest_id: owed.nest_id.to_vec(),
                    address: owed.nest_url.clone(),
                    outcome,
                }
            })
            .collect();
        for (keeper, passed) in linked {
            for (owed, delivery) in *passed {
                let outcome = OwedOutcome::of(delivery);
                if outcome == OwedOutcome::Landed {
                    continue;
                }
                entries.push(OwedNestEntry {
                    keeper: Some(*keeper),
                    old_actor_id: owed.old_actor_id.to_vec(),
                    nest_id: owed.nest_id.to_vec(),
                    address: owed.nest_url.clone(),
                    outcome,
                });
            }
        }
        if entries.is_empty() {
            return None;
        }
        Some(Self { entries })
    }

    /// The section's one line: how many entries are still owed.
    #[must_use]
    pub fn status(&self) -> LocalizedText {
        Self::status_of(self.entries.len())
    }

    /// The status line for a list of `count` entries — the one place its copy
    /// is composed, so an app folding a settled dismiss over the FFI's flat
    /// record recounts with the same words [`Self::status`] paints.
    #[must_use]
    pub fn status_of(count: usize) -> LocalizedText {
        LocalizedText::key_arg(KEY_STATUS, "count", count.to_string())
    }

    /// Drop one entry locally once its settle answered — the list then reads
    /// as the keeping nest's next serve will.
    pub fn remove(&mut self, key: &OwedNestKey) {
        self.entries.retain(|e| e.key() != *key);
    }
}

/// Read the bound nest's owed list for the account `rpc` is signed in as.
///
/// # Errors
///
/// The request failed.
pub async fn read_owed_nests<R: RpcRequester>(rpc: &R) -> Result<Vec<OwedNest>, R::Error> {
    fauna_client_core::succession_delivery::fetch_owed_nests(rpc).await
}

/// The dismiss: settle one entry at its keeper on the user's word. A bound
/// entry settles over the bound session; a linked entry over an owner
/// connection to its keeper, opened through `dial` for the pairing row the
/// bound session lists for that nest and checked to be bound to it — nothing
/// is sent over a connection bound to another identity. Nothing is dialed
/// at the owed nest itself. Idempotent at the keeper, so a second press after
/// a lost reply is harmless.
///
/// # Errors
///
/// The sentence to show when the keeper refused, could not be listed or
/// reached, or answered as another nest ([`dismiss_failed_copy`]'s argument).
pub async fn dismiss_owed_nest<R, D>(
    bound: &RecoveryClient<R>,
    dial: &D,
    entry: &OwedNestEntry,
) -> Result<(), String>
where
    R: RpcRequester,
    R::Error: RpcErrorClass + core::fmt::Display,
    D: LinkedNestDial,
    <D::Owner as RpcRequester>::Error: core::fmt::Display,
{
    use fauna_client_core::succession_delivery::settle_owed_nest as settle;
    let Some(keeper) = entry.keeper else {
        return settle(bound.transport(), &entry.old_actor_id, &entry.nest_id)
            .await
            .map_err(|e| e.to_string());
    };
    let target = linked_nests(bound)
        .await?
        .into_iter()
        .find(|t| t.nest_id == keeper)
        .ok_or_else(|| {
            format!(
                "no pairing names the keeping nest {}",
                fauna_core::hex32::encode(&keeper)
            )
        })?;
    let conn = dial.owner(&target).await?;
    if conn.bound_identity != keeper {
        return Err(format!(
            "{} answered as another nest ({})",
            target.nest_url,
            fauna_core::hex32::encode(&conn.bound_identity)
        ));
    }
    settle(&conn.rpc, &entry.old_actor_id, &entry.nest_id)
        .await
        .map_err(|e| e.to_string())
}

/// The line a failed dismiss renders as.
#[must_use]
pub fn dismiss_failed_copy(reason: &str) -> LocalizedText {
    LocalizedText::key_arg(KEY_DISMISS_FAILED, "reason", reason.to_string())
}
