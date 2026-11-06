//! Phase two's two records: the [`ArchiveSignal`] one app sends another inside
//! the pair's sealed MLS channel, and the [`LinksState`] each archive folder
//! keeps at `state/links.cbor` (`archive-import.md` § Storage; § Linking and
//! merging (phase two) → *Phase two — the rulings*, rulings 3, 4 and 10).
//!
//! Both are canonical DAG-CBOR through `fauna_cbor` and evolve additively:
//! every enum carries its arm (`transport.md` § Schema and forward-compat
//! discipline → *Rule 3 in full*; the per-enum answers are the enum ledger),
//! and the at-rest structs keep keys they do not name in an `extra`
//! catch-all, so a state a newer build wrote still decodes, and what it
//! carries is written back as it was read.

use std::collections::BTreeMap;

use fauna_archive::{ExternalActorRef, ExternalId, Platform};
use fauna_core::carried::CarriedValue;
use fauna_core::data::Timestamp;
use serde::{Deserialize, Serialize};

/// Where each archive folder keeps its link state.
pub const LINKS_PATH: &str = "state/links.cbor";

// ── the signal ─────────────────────────────────────────────────────────────

/// One archive-link signal (ruling 10): externally tagged, data variants, and
/// the carrying arm last. A signal a newer build sent decodes as
/// [`ArchiveSignal::Unknown`], which the sink ignores and never writes back.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveSignal {
    /// "I imported this platform's archive": the sender's [`super::fingerprint`],
    /// and — when that one is keyed on a platform user id — the name-keyed
    /// twin beside it ([`super::name_fingerprint`]), since a friends list may
    /// name the sender by display name alone (Facebook's always does).
    /// Additive: absent reads as none.
    Announce {
        platform: Platform,
        #[serde(with = "serde_bytes")]
        fingerprint: [u8; 32],
        #[serde(default, skip_serializing_if = "Option::is_none", with = "serde_bytes")]
        name_fingerprint: Option<[u8; 32]>,
    },
    /// "Yes, that is me" — the sender's own platform ref, so the receiver can
    /// ask the mirror question. Awaits [`Self::LinkConfirm`] or
    /// [`Self::LinkDecline`].
    LinkOffer {
        platform: Platform,
        me: ExternalActorRef,
    },
    /// The second yes: the pair is linked.
    LinkConfirm {
        platform: Platform,
        me: ExternalActorRef,
    },
    /// A no: neither side asks or offers again for this (contact, platform).
    LinkDecline { platform: Platform },
    /// Stop merging; the receiver drops the sender's stored index.
    Unlink { platform: Platform },
    /// The sender's imported posts the receiver may read, replacing any
    /// index stored before.
    OriginIndex {
        platform: Platform,
        entries: Vec<OriginEntry>,
    },
    /// Replies and reactions the sender signed against the receiver's posts.
    Claimed {
        platform: Platform,
        items: Vec<ClaimedItem>,
    },
    /// A signal a newer build sent that this one cannot name, carried whole.
    #[serde(untagged)]
    Unknown(CarriedValue),
}

impl ArchiveSignal {
    /// The platform the signal is about; `None` for [`Self::Unknown`].
    pub fn platform(&self) -> Option<&Platform> {
        match self {
            ArchiveSignal::Announce { platform, .. }
            | ArchiveSignal::LinkOffer { platform, .. }
            | ArchiveSignal::LinkConfirm { platform, .. }
            | ArchiveSignal::LinkDecline { platform }
            | ArchiveSignal::Unlink { platform }
            | ArchiveSignal::OriginIndex { platform, .. }
            | ArchiveSignal::Claimed { platform, .. } => Some(platform),
            ArchiveSignal::Unknown(_) => None,
        }
    }
}

/// One imported post in an origin index (§ Linking and merging, *The origin
/// index*).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OriginEntry {
    pub external_id: ExternalId,
    /// The post's permalink, when the archive recorded one — ruling 5's
    /// rung (ii).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The links the post shared — ruling 5's rung (iii). Absent reads as
    /// none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
    pub created_at: Timestamp,
    /// The Fauna post id (hex) the import authored it as.
    pub post_id: String,
    /// Whether the post is public — the one fact ruling 6's audience rule
    /// needs. Absent reads as `false`: a claimant that cannot tell treats the
    /// target as restricted and seals, never the wrong direction.
    #[serde(default)]
    pub public: bool,
}

/// One merged item a `Claimed` signal names: the claimant's record's external
/// id (both archives derive the same one for one comment), the post the
/// claimant signed, and the receiver's own post it references.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClaimedItem {
    pub external_id: ExternalId,
    pub post_id: String,
    pub target_post_id: String,
}

// ── the folder's link state ────────────────────────────────────────────────

/// Where one (contact, platform) pair stands.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PairState {
    /// This user is being asked "is your contact the person from your
    /// friends?" — by an `Announce` that hit, or by a `LinkOffer`
    /// ([`PairLink::asked_by_offer`]).
    Asked,
    /// This user said yes first; a `LinkOffer` awaits the contact's answer.
    Offered,
    Linked,
    /// Answered no on either side — never asked or offered again.
    Declined,
    /// The tombstone an unlink writes; the folder union honours it.
    Unlinked,
    /// A state a newer build wrote, carried whole; no transition touches it.
    #[serde(untagged)]
    Unknown(CarriedValue),
}

/// One (contact, platform) pair (ruling 3: the folder union keeps the newest
/// `updated_at` per pair).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairLink {
    /// The contact's actor id, hex.
    pub contact: String,
    pub platform: Platform,
    pub state: PairState,
    /// `Asked` by the contact's `LinkOffer` (a yes confirms) rather than by
    /// their `Announce` (a yes offers).
    #[serde(default)]
    pub asked_by_offer: bool,
    /// The contact's platform ref — the friend the `Announce` matched, or
    /// the ref their offer or confirm carried. The prompt's archived name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub their_ref: Option<ExternalActorRef>,
    /// The contact's latest origin index; dropped on unlink.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_index: Option<Vec<OriginEntry>>,
    /// When [`Self::origin_index`] arrived — a claim run is due while it is
    /// newer than [`Self::claims_run_at`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_received_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claims_run_at: Option<Timestamp>,
    /// When this side last sent its own index; `None` makes one due.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_sent_at: Option<Timestamp>,
    pub updated_at: Timestamp,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, CarriedValue>,
}

/// What a sent signal waits for (ruling 4).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Awaits {
    /// Dropped from the pending list once it has left.
    Nothing,
    /// A `LinkOffer`: re-driven every session until a confirm or decline.
    LinkAnswer,
    /// A wait a newer build recorded, carried whole; re-driven like
    /// [`Self::LinkAnswer`] until that build clears it.
    #[serde(untagged)]
    Unknown(CarriedValue),
}

/// One signal this side owes a contact (ruling 4).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingSend {
    pub contact: String,
    pub signal: ArchiveSignal,
    pub awaits: Awaits,
    /// Whether it has left at least once.
    #[serde(default)]
    pub sent: bool,
    pub at: Timestamp,
}

/// The claim ledger's verdict on one of this user's own comments or
/// reactions (rulings 5–7), keyed by external id so a re-import never claims
/// twice (outcome 55).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClaimEntry {
    /// Signed and created; `media_held` counts the comment's media items the
    /// claim left behind (§ Linking and merging, *Held for phase three*).
    Claimed {
        post_id: String,
        target_post_id: String,
        #[serde(default)]
        media_held: u64,
    },
    /// No ladder rung fired, or a reaction kind with no mapping — never
    /// placed by time.
    Unmatched { reason: String },
    /// A reply under a restricted post the contact could not open (ruling
    /// 6); retried on the next round.
    HeldUnreadable,
    /// A comment that is media only — nothing to claim in phase two.
    HeldMedia,
    /// A verdict a newer build wrote, carried whole; counts as neither.
    #[serde(untagged)]
    Unknown(CarriedValue),
}

impl ClaimEntry {
    /// A verdict no claim run revisits: `Claimed`, or one a newer build
    /// wrote and this one cannot read.
    pub fn is_final(&self) -> bool {
        matches!(self, ClaimEntry::Claimed { .. } | ClaimEntry::Unknown(_))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClaimRecord {
    pub contact: String,
    pub platform: Platform,
    pub external_id: ExternalId,
    pub entry: ClaimEntry,
    pub at: Timestamp,
}

/// A contact's verified claim against this user's post: the private copy of
/// that comment is hidden from here on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReceivedClaim {
    pub contact: String,
    pub platform: Platform,
    pub item: ClaimedItem,
    pub at: Timestamp,
}

/// A sealed claimed reply written ahead of its create — the import run's
/// `InFlightPost` rule: a crash in between replays these bytes instead of
/// sealing a second, different post.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InFlightClaim {
    pub contact: String,
    pub platform: Platform,
    pub external_id: ExternalId,
    pub target_post_id: String,
    #[serde(with = "serde_bytes")]
    pub post_bytes: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub sealed_body: Vec<u8>,
    #[serde(default)]
    pub media_held: u64,
}

/// `state/links.cbor`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinksState {
    #[serde(default)]
    pub pairs: Vec<PairLink>,
    #[serde(default)]
    pub pending: Vec<PendingSend>,
    #[serde(default)]
    pub claims: Vec<ClaimRecord>,
    #[serde(default)]
    pub received: Vec<ReceivedClaim>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_flight: Option<InFlightClaim>,
    #[serde(default)]
    pub updated_at: Timestamp,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, CarriedValue>,
}

impl LinksState {
    pub fn pair(&self, contact: &str, platform: &Platform) -> Option<&PairLink> {
        self.pairs
            .iter()
            .find(|p| p.contact == contact && &p.platform == platform)
    }

    pub(crate) fn pair_mut(&mut self, contact: &str, platform: &Platform) -> Option<&mut PairLink> {
        self.pairs
            .iter_mut()
            .find(|p| p.contact == contact && &p.platform == platform)
    }

    pub fn claim(
        &self,
        contact: &str,
        platform: &Platform,
        id: &ExternalId,
    ) -> Option<&ClaimEntry> {
        self.claims
            .iter()
            .find(|c| c.contact == contact && &c.platform == platform && &c.external_id == id)
            .map(|c| &c.entry)
    }

    /// Records `entry`, refusing to touch a record already `Claimed` — the
    /// ledger's idempotency (outcome 55) — or one a newer build wrote (which
    /// may name a post this build cannot see). Returns whether it wrote.
    pub fn record_claim(
        &mut self,
        contact: &str,
        platform: &Platform,
        external_id: &ExternalId,
        entry: ClaimEntry,
        at: Timestamp,
    ) -> bool {
        match self.claims.iter_mut().find(|c| {
            c.contact == contact && &c.platform == platform && &c.external_id == external_id
        }) {
            Some(existing) if existing.entry.is_final() => false,
            Some(existing) => {
                existing.entry = entry;
                existing.at = at;
                true
            }
            None => {
                self.claims.push(ClaimRecord {
                    contact: contact.to_string(),
                    platform: platform.clone(),
                    external_id: external_id.clone(),
                    entry,
                    at,
                });
                true
            }
        }
    }

    /// The folder union (ruling 3): `states` newest folder first. Per pair
    /// the newest `updated_at` wins (an earlier folder on a tie), so an
    /// unlink's tombstone outlives an older link; the claim ledger and the
    /// received claims are monotone and merge — a `Claimed` verdict beats
    /// any other, else the newest; the pending list, the write-ahead and the
    /// unnamed keys are the newest folder's that holds a state, since every
    /// write lands there whole.
    pub fn union(states: Vec<LinksState>) -> LinksState {
        let mut iter = states.into_iter();
        let Some(mut out) = iter.next() else {
            return LinksState::default();
        };
        for older in iter {
            for pair in older.pairs {
                match out.pair_mut(&pair.contact, &pair.platform) {
                    Some(mine) if mine.updated_at >= pair.updated_at => {}
                    Some(mine) => *mine = pair,
                    None => out.pairs.push(pair),
                }
            }
            for record in older.claims {
                match out.claims.iter_mut().find(|c| {
                    c.contact == record.contact
                        && c.platform == record.platform
                        && c.external_id == record.external_id
                }) {
                    Some(mine) => {
                        let mine_claimed = matches!(mine.entry, ClaimEntry::Claimed { .. });
                        let theirs_claimed = matches!(record.entry, ClaimEntry::Claimed { .. });
                        if (theirs_claimed && !mine_claimed)
                            || (theirs_claimed == mine_claimed && record.at > mine.at)
                        {
                            *mine = record;
                        }
                    }
                    None => out.claims.push(record),
                }
            }
            for claim in older.received {
                if !out.received.iter().any(|r| {
                    r.contact == claim.contact
                        && r.platform == claim.platform
                        && r.item.external_id == claim.item.external_id
                }) {
                    out.received.push(claim);
                }
            }
            out.updated_at = out.updated_at.max(older.updated_at);
        }
        out
    }
}
