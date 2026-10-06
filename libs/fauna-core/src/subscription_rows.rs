//! The `fauna.state.subscriptions` plane rows — the key grammar, the row
//! values and the per-row join of the subscription-tier period-key custody
//! (`config-dissolution.md` owns the
//! kind's birth, plane-only, and § Phases and gates → *Bounded rows* the
//! row shape; `key-material-hierarchy.md` § Audience: an opaque set of
//! subscriber pubkeys → *Encrypted-mode at-rest custody* owns what the keys
//! are; [`SubscriptionsConfig::merge`] owns the shipped rule).
//!
//! **One row per period key and one per staged removal, never one per
//! account.** A tier's `prior` history is uncapped by design (an archival
//! mint for a new subscriber needs every period), and every subscriber
//! removal rotates to a fresh period, so a per-account row grows with use and
//! would outgrow the per-entry cap — after which the writer door refuses it
//! on every pass and the newest period key, irrecoverable, never leaves the
//! device. Pruning is refused for the same reason the ruling refuses it for
//! the ceremony envelopes: the history is read for good.
//!
//! The key is the row's **content digest**, not `(tier, version)`: two
//! devices rotating the same tier concurrently each mint a period at the same
//! version with a different key, and both keys must survive (the shipped rule
//! keeps the loser in `prior`). So:
//!
//! - `period/<digest hex32>` holds one [`TierPeriodRow`] — a tier name and
//!   one period — whose digest covers the whole row. Write-once: two rows at
//!   one key are the same bytes, so the join is equality.
//! - `removal/<digest hex32>` holds one [`PendingRemovalRow`] — a staged
//!   removal and its `settled` marker — whose digest covers the removal
//!   alone. The marker is the one monotone field (OR): a CRDT kind has no
//!   deletion, so a sentinel the orchestration is done with is *settled*,
//!   never dropped, and a stale replica's unsettled copy cannot resurrect it.
//!
//! The digest is [`canonical_tiebreak_key`](crate::encoding::canonical_tiebreak_key)
//! (BLAKE3 over the canonical encoding, the plaintext buffer zeroized). Row
//! keys are blinded on the wire, and a 256-bit digest of a record carrying a
//! uniformly random 256-bit key reveals nothing of it.
//!
//! The composite [`SubscriptionsConfig`] is the READ fold: every period row
//! and every unsettled removal row, folded through the shipped rule
//! ([`SubscriptionsConfig::fold_row`]); [`SubscriptionsConfig::rows`] is the
//! other direction.
//!
//! Both row values decode with `deny_unknown_fields`, down to the period and
//! the removal they carry: the kind is CrdtPerField, and a tolerant reader
//! would strip a newer build's field from the bytes it re-encodes
//! (`config-dissolution.md` P4). Every fixed-width byte field inside is a
//! CBOR byte string already (`SecretArray32`, `ActorId`).

use serde::{Deserialize, Serialize};

use crate::data::{PendingRemoval, SubscriptionsConfig, TierPeriod, TierPeriodKeys};
use crate::encoding::{canonical_decode, canonical_encode, canonical_tiebreak_key};
use crate::error::{Error, Result};

/// Key prefix of a period row: `period/<digest hex32>`.
pub const PERIOD_KEY_PREFIX: &str = "period/";

/// Key prefix of a staged-removal row: `removal/<digest hex32>`.
pub const REMOVAL_KEY_PREFIX: &str = "removal/";

/// One period key of one tier — a `period/` row's value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TierPeriodRow {
    /// The tier the period belongs to (`TierPeriodKeys::tier_name`).
    pub tier_name: String,
    /// The period itself.
    pub period: TierPeriod,
}

impl TierPeriodRow {
    /// The row's plane key — the digest of the whole row.
    #[must_use]
    pub fn plane_key(&self) -> String {
        format!(
            "{PERIOD_KEY_PREFIX}{}",
            crate::hex32::encode(&canonical_tiebreak_key(self))
        )
    }
}

/// One staged subscriber removal and whether the orchestration is done with
/// it — a `removal/` row's value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingRemovalRow {
    /// The staged removal, carrying its irrecoverable fresh period.
    pub removal: PendingRemoval,
    /// Monotone: once any device settles the removal it stays settled. A
    /// settled row leaves the fold's `pending_removals`; its fresh period is
    /// written as a period row by the same settle
    /// (`fauna_account_plane::subscription_rows::settle_pending_removal`), so
    /// settling never strands the key.
    pub settled: bool,
}

impl PendingRemovalRow {
    /// The row's plane key — the digest of the removal alone, so settling
    /// the row keeps its key.
    #[must_use]
    pub fn plane_key(&self) -> String {
        format!(
            "{REMOVAL_KEY_PREFIX}{}",
            crate::hex32::encode(&canonical_tiebreak_key(&self.removal))
        )
    }
}

/// One plane row's value: the record its key names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubscriptionsRow {
    /// A `period/` row.
    Period(TierPeriodRow),
    /// A `removal/` row.
    Removal(PendingRemovalRow),
}

impl SubscriptionsRow {
    /// The row's plane key.
    #[must_use]
    pub fn plane_key(&self) -> String {
        match self {
            Self::Period(r) => r.plane_key(),
            Self::Removal(r) => r.plane_key(),
        }
    }

    /// The canonical value bytes.
    ///
    /// # Errors
    /// Canonical-encoding failure.
    pub fn encode(&self) -> Result<Vec<u8>> {
        match self {
            Self::Period(r) => canonical_encode(r),
            Self::Removal(r) => canonical_encode(r),
        }
        .map(|b| b.to_vec())
    }

    /// The per-row join — the halves [`SubscriptionsConfig::merge`] runs on
    /// one period or one staged removal. A period row is write-once (its key
    /// is its content), so two sides must be equal; a removal row ORs its
    /// `settled` marker over one removal.
    ///
    /// # Errors
    /// The two sides are different row types, or name different records.
    pub fn merge(&self, other: &Self) -> Result<Self> {
        match (self, other) {
            (Self::Period(a), Self::Period(b)) if a == b => Ok(self.clone()),
            (Self::Removal(a), Self::Removal(b)) if a.removal == b.removal => {
                Ok(Self::Removal(PendingRemovalRow {
                    removal: a.removal.clone(),
                    settled: a.settled || b.settled,
                }))
            }
            _ => Err(Error::Encoding(
                "subscriptions rows name different records".into(),
            )),
        }
    }
}

/// Decode one `fauna.state.subscriptions` row: the key's first segment picks
/// the row type, the value must decode as it (`deny_unknown_fields`) and its
/// digest must be the key's — a record is never filed under another key, and
/// only the canonical lowercase spelling parses, so one record has one key.
///
/// # Errors
/// An unparseable key, an undecodable value, or a key/value mismatch.
pub fn decode_subscriptions_row(key: &str, value: &[u8]) -> Result<SubscriptionsRow> {
    let digest = |rest: &str| -> Result<()> {
        if crate::hex32::is_lowercase_hex64(rest) {
            Ok(())
        } else {
            Err(Error::Encoding(format!(
                "subscriptions key digest is not lowercase hex32: {key:?}"
            )))
        }
    };
    let row = if let Some(rest) = key.strip_prefix(PERIOD_KEY_PREFIX) {
        digest(rest)?;
        SubscriptionsRow::Period(canonical_decode(value)?)
    } else if let Some(rest) = key.strip_prefix(REMOVAL_KEY_PREFIX) {
        digest(rest)?;
        SubscriptionsRow::Removal(canonical_decode(value)?)
    } else {
        return Err(Error::Encoding(format!("not a subscriptions key: {key:?}")));
    };
    let named = row.plane_key();
    if named != key {
        return Err(Error::Encoding(format!(
            "subscriptions row at {key:?} holds the record for {named:?}"
        )));
    }
    Ok(row)
}

impl SubscriptionsConfig {
    /// Every period and every staged removal as its plane row, `(key, row)`,
    /// in key order, one row per distinct record: each tier's `current` and
    /// `prior` periods, then each pending removal (unsettled — the composite
    /// holds only live sentinels).
    #[must_use]
    pub fn rows(&self) -> Vec<(String, SubscriptionsRow)> {
        let periods = self.tiers.iter().flat_map(|t| {
            std::iter::once(&t.current).chain(t.prior.iter()).map(|p| {
                SubscriptionsRow::Period(TierPeriodRow {
                    tier_name: t.tier_name.clone(),
                    period: p.clone(),
                })
            })
        });
        let removals = self.pending_removals.iter().map(|r| {
            SubscriptionsRow::Removal(PendingRemovalRow {
                removal: r.clone(),
                settled: false,
            })
        });
        let by_key: std::collections::BTreeMap<String, SubscriptionsRow> = periods
            .chain(removals)
            .map(|row| (row.plane_key(), row))
            .collect();
        by_key.into_iter().collect()
    }

    /// Fold one plane row into the composite through the shipped rule — the
    /// read side of the per-record rows. A settled removal row folds to
    /// nothing: the sentinel is done with, and its period has its own row.
    ///
    /// **One mint reads as one period.** Two period rows with the same
    /// version and the same key are one mint re-stamped: a republish that had
    /// to advance past a stored blob re-records `current` with the
    /// `rotated_at` the nest accepted, and a removal's commit records its
    /// period beside the staged copy — the composite rewrites either in place, the
    /// plane has no in-place rewrite, and every row stays. The fold keeps the
    /// later-stamped copy alone, so an archival backfill never mints one
    /// version twice. Keeping the maximum of each (version, key) group is a
    /// join itself, so the fold stays order-free; the key is never lost,
    /// since the kept copy carries it.
    ///
    /// # Errors
    /// An unparseable key, an undecodable value, or a value filed under
    /// another record's key ([`decode_subscriptions_row`]).
    pub fn fold_row(&mut self, key: &str, value: &[u8]) -> Result<()> {
        let one = match decode_subscriptions_row(key, value)? {
            SubscriptionsRow::Period(r) => Self {
                tiers: vec![TierPeriodKeys {
                    tier_name: r.tier_name,
                    current: r.period,
                    prior: Vec::new(),
                }],
                pending_removals: Vec::new(),
            },
            SubscriptionsRow::Removal(r) if !r.settled => Self {
                tiers: Vec::new(),
                pending_removals: vec![r.removal],
            },
            SubscriptionsRow::Removal(_) => return Ok(()),
        };
        *self = self.merge(&one);
        for tier in &mut self.tiers {
            // `prior` is sorted most-recent first by the merge, so the first
            // copy of each mint is its latest stamp; `current` is the latest
            // of all, so a restamped copy of it is always behind it.
            let mut seen = vec![(tier.current.version, tier.current.key.clone())];
            tier.prior.retain(|p| {
                let mint = (p.version, p.key.clone());
                if seen.contains(&mint) {
                    false
                } else {
                    seen.push(mint);
                    true
                }
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::ActorId;

    fn period(version: u64, key: u8, rotated_at: u64) -> TierPeriod {
        TierPeriod {
            version,
            key: [key; 32].into(),
            rotated_at,
            minted_by: Some(ActorId([0xA1; 32])),
        }
    }

    fn removal(tier: &str, sub: u8, version: u64, key: u8) -> PendingRemoval {
        PendingRemoval {
            tier_name: tier.into(),
            subscriber_id: ActorId([sub; 32]),
            new_period: period(version, key, version * 1000),
        }
    }

    fn fold(rows: &[(String, SubscriptionsRow)]) -> SubscriptionsConfig {
        let mut out = SubscriptionsConfig::default();
        for (k, r) in rows {
            out.fold_row(k, &r.encode().unwrap()).unwrap();
        }
        out
    }

    /// The fold is the shipped rule: two devices' composites, split into
    /// rows and folded back, read exactly as
    /// [`SubscriptionsConfig::merge`] of the two — including a concurrent
    /// rotation (two version-3 periods), whose loser stays in `prior`.
    #[test]
    fn the_row_fold_reads_as_the_shipped_merge() {
        let a = SubscriptionsConfig {
            tiers: vec![
                TierPeriodKeys {
                    tier_name: "gold".into(),
                    current: period(3, 0x33, 3000),
                    prior: vec![period(2, 0x22, 2000), period(1, 0x11, 1000)],
                },
                TierPeriodKeys {
                    tier_name: "silver".into(),
                    current: period(1, 0x51, 1000),
                    prior: vec![],
                },
            ],
            pending_removals: vec![removal("gold", 7, 4, 0x44)],
        };
        let b = SubscriptionsConfig {
            tiers: vec![TierPeriodKeys {
                tier_name: "gold".into(),
                current: period(3, 0x3B, 3001),
                prior: vec![period(2, 0x22, 2000), period(1, 0x11, 1000)],
            }],
            pending_removals: vec![removal("gold", 8, 4, 0x45)],
        };
        let mut rows = a.rows();
        rows.extend(b.rows());
        let folded = fold(&rows);
        assert_eq!(folded, a.merge(&b));
        assert_eq!(folded.tiers[0].current, period(3, 0x3B, 3001));
        assert_eq!(folded.tiers[0].prior.len(), 3, "the concurrent loser stays");
        // Order-free: the reverse fold reads the same.
        rows.reverse();
        assert_eq!(fold(&rows), folded);
        // And round-trips: the fold's own rows fold back to it.
        assert_eq!(fold(&folded.rows()), folded);
        assert_eq!(folded.rows().len(), 7, "one row per distinct record");
    }

    /// A re-stamped mint — the same version and key under a later
    /// `rotated_at`, which a republish or a removal's commit records beside
    /// the earlier copy — folds to ONE period, the later stamp, whether it is
    /// the tier's `current` or has been rotated into `prior`, in any fold
    /// order; both rows stay.
    #[test]
    fn a_restamped_mint_folds_to_one_period_at_its_later_stamp() {
        let row = |p: TierPeriod| {
            let r = SubscriptionsRow::Period(TierPeriodRow {
                tier_name: "gold".into(),
                period: p,
            });
            (r.plane_key(), r)
        };
        let mut rows = vec![
            row(period(1, 0x11, 1000)),
            row(period(2, 0x22, 2000)),
            row(period(2, 0x22, 2500)),
        ];
        let folded = fold(&rows);
        assert_eq!(folded.tiers[0].current, period(2, 0x22, 2500));
        assert_eq!(folded.tiers[0].prior, vec![period(1, 0x11, 1000)]);
        rows.reverse();
        assert_eq!(fold(&rows), folded, "order-free");

        // Rotated past: the re-stamped mint is now history, still one entry.
        rows.push(row(period(3, 0x33, 3000)));
        let folded = fold(&rows);
        assert_eq!(folded.tiers[0].current, period(3, 0x33, 3000));
        assert_eq!(
            folded.tiers[0].prior,
            vec![period(2, 0x22, 2500), period(1, 0x11, 1000)]
        );
        rows.rotate_left(2);
        assert_eq!(fold(&rows), folded, "order-free");

        // Two DIFFERENT keys at one version are two mints (a concurrent
        // rotation), never collapsed.
        rows.push(row(period(3, 0x3B, 2900)));
        assert_eq!(fold(&rows).tiers[0].prior.len(), 3);
    }

    /// A settled removal leaves the fold, and settling keeps the row's key;
    /// the per-row join ORs the marker in either order, so a stale
    /// unsettled copy never resurrects a settled sentinel.
    #[test]
    fn a_settled_removal_stays_settled_and_leaves_the_fold() {
        let r = removal("gold", 7, 4, 0x44);
        let live = SubscriptionsRow::Removal(PendingRemovalRow {
            removal: r.clone(),
            settled: false,
        });
        let done = SubscriptionsRow::Removal(PendingRemovalRow {
            removal: r.clone(),
            settled: true,
        });
        assert_eq!(live.plane_key(), done.plane_key());
        assert_eq!(live.merge(&done).unwrap(), done);
        assert_eq!(done.merge(&live).unwrap(), done);
        assert!(
            fold(&[(done.plane_key(), done)])
                .pending_removals
                .is_empty()
        );
    }

    /// A key outside the grammar, a misfiled value, a mismatched join and a
    /// field from a newer build are all refused.
    #[test]
    fn junk_misfiled_or_newer_rows_are_refused() {
        let row = SubscriptionsRow::Period(TierPeriodRow {
            tier_name: "gold".into(),
            period: period(1, 0x11, 1000),
        });
        let key = row.plane_key();
        let value = row.encode().unwrap();
        assert!(decode_subscriptions_row(&key, &value).is_ok());
        assert!(decode_subscriptions_row("self", &value).is_err());
        assert!(decode_subscriptions_row(&key.to_uppercase(), &value).is_err());
        let removal_key = key.replace(PERIOD_KEY_PREFIX, REMOVAL_KEY_PREFIX);
        assert!(decode_subscriptions_row(&removal_key, &value).is_err());
        let other = SubscriptionsRow::Period(TierPeriodRow {
            tier_name: "gold".into(),
            period: period(1, 0x12, 1000),
        });
        assert!(decode_subscriptions_row(&other.plane_key(), &value).is_err());
        assert!(row.merge(&other).is_err());
        #[derive(Serialize)]
        struct Newer<'a> {
            #[serde(flatten)]
            row: &'a TierPeriodRow,
            from_the_future: u8,
        }
        let SubscriptionsRow::Period(inner) = &row else {
            unreachable!()
        };
        let newer = canonical_encode(&Newer {
            row: inner,
            from_the_future: 1,
        })
        .unwrap();
        assert!(decode_subscriptions_row(&key, &newer).is_err());
    }
}
