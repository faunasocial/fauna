//! The A7 custody receipt — a custodian's signed, dated attestation of what it
//! holds for one owner.
//!
//! Authority: `docs/goal/architecture/message-segment-store.md` § Nest
//! dehydration owns the receipt *mechanics* — "a signed, dated attestation by a
//! holding replica covering a scope/CID-range"; `account-data-plane.md`
//! § Replica posture → *Custody policy* (T15) owns what a custody receipt
//! carries and why: "the custodian's periodic check-ins **are** the A7 custody
//! receipts — signed, dated attestations covering scope/CID ranges plus a
//! held-bytes summary … Receipts feed the dehydration predicate and both UIs."
//!
//! # What this module is, and is not
//!
//! It is the payload and its two verification rules. It is **not** the
//! dehydration predicate: the parameters that decide when a set of receipts is
//! enough to evict — N-of-M, aging margins, re-verification cadence — remain
//! T18's first-build detail, unscheduled. A receipt is deliberately usable
//! before that predicate exists, because it has a second consumer with no
//! parameters at all: the T16 UI, which renders *last confirmed ‹time›* and a
//! held-bytes summary on the owner's custody rows.
//!
//! # A receipt is a claim, not a fact
//!
//! `message-segment-store.md` states this outright, and the shape honours it:
//! the receipt is signed by the **custodian's device principal** — the exact
//! key the owner's grant named — so a receipt is attributable, and a lying
//! custodian is caught by re-verification rather than by trusting the number.
//! Nothing here treats a receipt's contents as verified coverage; it verifies
//! only *who said it, about which grant, when*.
//!
//! # Why the eviction numbers ride the receipt
//!
//! T15 requires eviction to be **receipt-visible**: "coverage shrinkage
//! surfaces owner-side as degraded redundancy (A7's honest failure mode), never
//! silently". So a receipt carries not just what is held but what was *dropped*
//! and what the custodian could not fit — [`CustodyReceipt::evicted_bytes`] and
//! [`CustodyReceipt::unreclaimable_bytes`]. A receipt that reported only
//! held-bytes would let a shrinking custodian look merely small.

use serde::{Deserialize, Serialize};

use crate::custody_grant::check_grant_id;
use crate::custody_policy::{CustodyBudgetState, CustodyMeter};
use crate::data::Timestamp;
use crate::encoding::{EmbedAsBytes, Signed, decode_signed_bytes, sign_envelope, verify_envelope};
use crate::error::{Error, Result};
use crate::identity::ActorKeypair;

/// One scope family's coverage, as attested. The public half of
/// [`crate::custody_policy::ScopeMeter`] — counts and the coordinate, never
/// anything that could describe the sealed contents.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeCoverage {
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub item_class: String,
    /// Rows held for this family — payload-evicted rows included, since their
    /// coordinate floor is still held and still served.
    #[serde(default)]
    pub rows: u64,
    /// Payload bytes held for this family at attestation time.
    #[serde(default)]
    pub payload_bytes: u64,
}

/// What a custodian attests about one custody, at one moment.
///
/// # Evolution posture
///
/// Every field `#[serde(default)]`, unknown fields ignored: a receipt crosses a
/// version boundary in both directions (an older owner reads a newer
/// custodian's receipt and vice versa), and additive evolution must never turn
/// into a refusal to read coverage. The signature covers the canonical bytes as
/// sent, so a reader verifies what was signed even where it cannot interpret
/// every field.
///
/// `Default` (all-zero, `attested_at = 0` = maximally stale) exists for
/// struct-update fixtures — the growing-wire-type convention that keeps two
/// branches adding fields merge-clean.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyReceipt {
    /// The custody this attests to (16 bytes, the capability-grant id space).
    #[serde(with = "serde_bytes", default)]
    pub grant_id: Vec<u8>,
    /// The custodied account's actor id — the raw-bytes spelling
    /// [`crate::custodies_held::CustodyHeld::owner`] uses, so the row and the
    /// receipt about it agree without a conversion.
    #[serde(default)]
    #[serde(with = "serde_bytes")]
    pub owner: [u8; 32],
    /// The attesting device's principal key — the signer, and exactly the key
    /// the owner's witness named as `custodian_key`. The owner cross-checks it
    /// against the grant it minted; a receipt from any other key is not this
    /// custody's.
    #[serde(default)]
    #[serde(with = "serde_bytes")]
    pub custodian_key: [u8; 32],
    /// Coverage by scope family.
    #[serde(default)]
    pub covered: Vec<ScopeCoverage>,
    /// Total payload bytes held.
    #[serde(default)]
    pub held_bytes: u64,
    /// The accepted byte budget this custodian is holding against — so the
    /// owner reads coverage against the bound the host actually chose, not the
    /// bound the owner last heard about.
    #[serde(default)]
    pub retained_bytes_cap: u64,
    /// Payload bytes dropped under budget pressure since the previous receipt.
    /// Non-zero is T15's *coverage shrinkage*: the owner sees degraded
    /// redundancy rather than a silently thinner custodian.
    #[serde(default)]
    pub evicted_bytes: u64,
    /// Bytes the custodian is over its cap and **cannot** free, because what
    /// remains is the always-present floor. Non-zero means this custodian is
    /// permanently over budget — an honest state, and one the owner should act
    /// on (widen the budget, or narrow the scopes).
    #[serde(default)]
    pub unreclaimable_bytes: u64,
    /// When this was attested. The *dated* half of "signed, dated attestation":
    /// freshness is the whole point, and a receipt with no time is a claim with
    /// no expiry.
    #[serde(default = "epoch")]
    pub attested_at: Timestamp,
}

/// The absent-timestamp fallback for tolerant decoding. Deliberately the epoch
/// rather than "now": a receipt that arrived without a date is maximally
/// **stale**, which is the honest reading — never freshly-confirmed coverage.
fn epoch() -> Timestamp {
    Timestamp(0)
}

impl Signed for CustodyReceipt {
    fn signer_public_key(&self) -> &[u8; 32] {
        &self.custodian_key
    }
}

impl CustodyReceipt {
    /// Build a receipt from a metering pass's own numbers — the seam that makes
    /// T15's "receipt-visible by construction" literal: the receipt cannot
    /// report coverage the meter did not measure, because it is derived from
    /// the meter rather than assembled beside it.
    ///
    /// `meter` is the state **after** eviction (what is held now); `evicted`
    /// and the budget state come from the same pass.
    #[allow(clippy::too_many_arguments)] // one metering pass's numbers, verbatim
    pub fn from_meter(
        grant_id: Vec<u8>,
        owner: [u8; 32],
        custodian_key: [u8; 32],
        meter: &CustodyMeter,
        retained_bytes_cap: u64,
        evicted_bytes: u64,
        state: CustodyBudgetState,
        unreclaimable_bytes: u64,
        attested_at: Timestamp,
    ) -> Self {
        Self {
            grant_id,
            owner,
            custodian_key,
            covered: meter
                .scopes
                .iter()
                .map(|s| ScopeCoverage {
                    scope: s.scope.clone(),
                    item_class: s.item_class.clone(),
                    rows: s.rows,
                    payload_bytes: s.payload_bytes,
                })
                .collect(),
            held_bytes: meter.held_bytes(),
            retained_bytes_cap,
            evicted_bytes,
            // Only `AtFloor` carries an unreclaimable overage; any other state
            // reporting one would be a contradiction the owner cannot resolve,
            // so the type never emits it.
            unreclaimable_bytes: match state {
                CustodyBudgetState::AtFloor => unreclaimable_bytes,
                _ => 0,
            },
            attested_at,
        }
    }

    /// Whether this receipt reports coverage that shrank or is capped short —
    /// the "degraded redundancy the owner sees" predicate, in one place so
    /// every app asks it the same way.
    pub fn is_degraded(&self) -> bool {
        self.evicted_bytes > 0 || self.unreclaimable_bytes > 0
    }
}

/// The custodian's check-in interval — how long an attestation may stand
/// before the next budget pass mints a fresh one (T15: "the custodian's
/// periodic check-ins **are** the A7 custody receipts"; the emit rate is W8 (account-data-plane.md § Workstreams)
/// build detail, distinct from T18's *re-verification* cadence, which judges
/// receipts owner-side). One day: cheap on the channel (one application
/// message per custody per day), and comfortably inside any aging margin T18
/// could plausibly pick. Not a user choice — nobody would want to configure
/// their replica's attestation rate, so this is a constant, not a knob.
pub const CUSTODY_RECEIPT_INTERVAL_MICROS: u64 = 24 * 60 * 60 * 1_000_000;

/// Whether the custodian owes a fresh receipt mint this budget pass — the
/// check-in cadence, pure so it tiers down to mock-clock tests.
///
/// Due when any of:
/// - **never attested** (`minted_at` = 0) — a custody's first pass mints
///   immediately, so a new custodian confirms promptly;
/// - **the interval elapsed** since the last mint;
/// - **this pass evicted payload** — T15's "eviction is always
///   receipt-visible … never silently" wants the shrink reported now, not at
///   the next scheduled check-in;
/// - **the degraded verdict flipped** in either direction — a custody going
///   degraded must surface promptly, and one healing should stop reading
///   degraded promptly too.
///
/// Deliberately NOT due on mere byte-count drift: coverage numbers move with
/// every pull, and re-attesting each pass would turn the cadence into noise.
pub fn receipt_due(
    now: Timestamp,
    minted_at: Timestamp,
    evicted_this_pass: bool,
    degraded_now: bool,
    degraded_last_minted: bool,
) -> bool {
    minted_at.0 == 0
        || now.0.saturating_sub(minted_at.0) >= CUSTODY_RECEIPT_INTERVAL_MICROS
        || evicted_this_pass
        || degraded_now != degraded_last_minted
}

/// Sign a receipt with the custodian's **device principal** keypair — the
/// keypair whose public key the owner's witness named, not the custodian
/// account's identity key. (A device principal is an ordinary Ed25519 keypair;
/// `ActorKeypair::from_secret` is how a raw device secret enters the signing
/// idiom, as elsewhere in the custodian runtime.)
///
/// Refuses a receipt whose `custodian_key` is not the signing keypair's public
/// key — a receipt attributed to a device that did not sign it is exactly the
/// forgery the attestation exists to prevent.
pub fn sign_custody_receipt(
    custodian: &ActorKeypair,
    receipt: &CustodyReceipt,
) -> Result<EmbedAsBytes> {
    if receipt.custodian_key != custodian.actor_id().0 {
        return Err(Error::Encoding(
            "custody receipt names a custodian key other than the signing keypair".into(),
        ));
    }
    check_grant_id(&receipt.grant_id, "receipt")?;
    let (bytes, env) = sign_envelope(custodian, receipt)?;
    Ok(EmbedAsBytes::from_signed(bytes, env))
}

/// Verify a received receipt envelope.
///
/// Two rules, both checkable from the payload plus what the owner already
/// knows from the grant it minted:
///
/// 1. The envelope verifies under the receipt's own `custodian_key`.
/// 2. That key **is** `expected_custodian` — the `custodian_key` from this
///    custody's witness. Without this a valid receipt signed by *some* device
///    would pass as coverage for a custody it has nothing to do with.
///
/// The owner also checks the grant id matches the custody it is folding into;
/// that is the caller's, since only the caller knows which row it is updating.
/// Freshness is deliberately not judged here — how old is too old is T18's
/// aging margin, and the UI's three-state rendering needs the raw timestamp.
pub fn verify_custody_receipt(
    envelope: &EmbedAsBytes,
    expected_custodian: &[u8; 32],
) -> Result<CustodyReceipt> {
    let (bytes, env) = envelope.clone().into_signed()?;
    let receipt: CustodyReceipt = decode_signed_bytes(&bytes)?;
    verify_envelope(&receipt, &bytes, &env)
        .map_err(|_| Error::Encoding("custody receipt signature invalid".into()))?;
    if receipt.custodian_key != *expected_custodian {
        return Err(Error::Encoding(
            "custody receipt is signed by a device other than this custody's custodian".into(),
        ));
    }
    check_grant_id(&receipt.grant_id, "receipt")?;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::custody_policy::ScopeMeter;

    const CUSTODIAN_SECRET: [u8; 32] = [0xC5; 32];
    const OTHER_SECRET: [u8; 32] = [0x77; 32];

    fn custodian() -> ActorKeypair {
        ActorKeypair::from_secret(CUSTODIAN_SECRET)
    }

    fn meter() -> CustodyMeter {
        CustodyMeter {
            scopes: vec![
                ScopeMeter {
                    scope: "state".into(),
                    item_class: "state-entry".into(),
                    rows: 12,
                    payload_bytes: 900,
                    evictable_rows: 10,
                    evictable_bytes: 800,
                },
                ScopeMeter {
                    scope: "state-fleet".into(),
                    item_class: "state-entry".into(),
                    rows: 3,
                    payload_bytes: 100,
                    evictable_rows: 3,
                    evictable_bytes: 100,
                },
            ],
        }
    }

    fn receipt() -> CustodyReceipt {
        CustodyReceipt::from_meter(
            vec![0x1D; 16],
            [0xA1; 32],
            custodian().actor_id().0,
            &meter(),
            2000,
            0,
            CustodyBudgetState::Ok,
            0,
            Timestamp(1_700_000_000),
        )
    }

    #[test]
    fn a_receipt_round_trips_through_sign_and_verify() {
        let r = receipt();
        let env = sign_custody_receipt(&custodian(), &r).unwrap();
        let back = verify_custody_receipt(&env, &custodian().actor_id().0).unwrap();
        assert_eq!(back, r);
        assert_eq!(back.held_bytes, 1000);
        assert_eq!(back.covered.len(), 2);
    }

    /// The coverage is derived from the meter, not assembled beside it — T15's
    /// "receipt-visible by construction".
    #[test]
    fn coverage_is_derived_from_the_meter() {
        let r = receipt();
        assert_eq!(r.held_bytes, meter().held_bytes());
        assert_eq!(
            r.covered
                .iter()
                .map(|c| (c.scope.as_str(), c.rows, c.payload_bytes))
                .collect::<Vec<_>>(),
            vec![("state", 12, 900), ("state-fleet", 3, 100)]
        );
    }

    /// A device may not attest for another device's custody.
    #[test]
    fn a_receipt_signed_by_the_wrong_device_is_refused() {
        let r = receipt();
        let err = sign_custody_receipt(&ActorKeypair::from_secret(OTHER_SECRET), &r)
            .expect_err("signing a receipt attributed to another device must refuse");
        assert!(
            err.to_string().contains("other than the signing keypair"),
            "{err}"
        );
    }

    /// And a genuinely-signed receipt from a device that is not THIS custody's
    /// custodian is refused at verification — the check that stops a valid
    /// signature standing in as coverage for the wrong custody.
    #[test]
    fn a_receipt_from_another_custodian_is_refused_at_verify() {
        let other = ActorKeypair::from_secret(OTHER_SECRET);
        let mut r = receipt();
        r.custodian_key = other.actor_id().0;
        let env = sign_custody_receipt(&other, &r).unwrap();

        let err = verify_custody_receipt(&env, &custodian().actor_id().0)
            .expect_err("a receipt from another device must not pass as this custody's");
        assert!(
            err.to_string()
                .contains("other than this custody's custodian"),
            "{err}"
        );
    }

    /// A tampered receipt fails the signature, not merely a field check.
    #[test]
    fn a_tampered_receipt_fails_verification() {
        let env = sign_custody_receipt(&custodian(), &receipt()).unwrap();
        let (bytes, sig_env) = env.clone().into_signed().unwrap();
        let mut forged: CustodyReceipt = decode_signed_bytes(&bytes).unwrap();
        forged.held_bytes = 999_999;
        let forged_bytes = crate::encoding::canonical_encode(&forged).unwrap();
        let tampered = EmbedAsBytes::from_signed(forged_bytes.to_vec(), sig_env);

        assert!(verify_custody_receipt(&tampered, &custodian().actor_id().0).is_err());
    }

    /// "Both ways" is sign AND verify — the sign door refuses outright, and a
    /// malformed id forced through the generic `sign_envelope` path proves
    /// the verify door holds its own check rather than trusting the signer
    /// (this test's name previously covered only the
    /// sign half, mirroring `custody_grant`'s
    /// `custody_grant_id_length_is_enforced_at_sign_and_verify`).
    #[test]
    fn a_malformed_grant_id_is_refused_both_ways() {
        let mut r = receipt();
        r.grant_id = vec![0x1D; 4];
        sign_custody_receipt(&custodian(), &r).expect_err("a malformed grant id must not sign");
        let (bytes, env) = sign_envelope(&custodian(), &r).expect("raw sign");
        let err = verify_custody_receipt(
            &EmbedAsBytes::from_signed(bytes, env),
            &custodian().actor_id().0,
        )
        .expect_err("a malformed grant id must not verify");
        assert!(err.to_string().contains("grant id"), "{err}");
    }

    /// Eviction reaches the owner: a receipt from a pass that dropped payload
    /// reports it, and reads as degraded.
    #[test]
    fn an_eviction_is_reported_and_reads_as_degraded() {
        let r = CustodyReceipt::from_meter(
            vec![0x1D; 16],
            [0xA1; 32],
            custodian().actor_id().0,
            &meter(),
            1000,
            250,
            CustodyBudgetState::OverBudget,
            0,
            Timestamp(1),
        );
        assert_eq!(r.evicted_bytes, 250);
        assert!(r.is_degraded());
        assert_eq!(
            r.unreclaimable_bytes, 0,
            "only AtFloor carries an unreclaimable overage"
        );
    }

    /// The permanently-over-budget custodian: the overage reaches the owner
    /// rather than being absorbed into a plausible-looking held-bytes number.
    #[test]
    fn an_at_floor_custodian_reports_its_unreclaimable_overage() {
        let r = CustodyReceipt::from_meter(
            vec![0x1D; 16],
            [0xA1; 32],
            custodian().actor_id().0,
            &meter(),
            100,
            0,
            CustodyBudgetState::AtFloor,
            900,
            Timestamp(1),
        );
        assert_eq!(r.unreclaimable_bytes, 900);
        assert!(r.is_degraded());
        assert_eq!(
            r.retained_bytes_cap, 100,
            "the owner reads the HOST's bound"
        );
    }

    /// A healthy custodian is not degraded — the predicate must not fire on
    /// ordinary coverage, or the UI's honest-failure signal means nothing.
    #[test]
    fn a_healthy_receipt_is_not_degraded() {
        assert!(!receipt().is_degraded());
    }

    /// Tolerant decoding in both skew directions (the `custodies_held`
    /// posture): a newer custodian's receipt still decodes for an older owner.
    #[test]
    fn a_newer_builds_receipt_decodes_tolerantly() {
        #[derive(Serialize)]
        struct V2 {
            #[serde(with = "serde_bytes")]
            grant_id: Vec<u8>,
            #[serde(with = "serde_bytes")]
            owner: [u8; 32],
            #[serde(with = "serde_bytes")]
            custodian_key: [u8; 32],
            held_bytes: u64,
            attested_at: Timestamp,
            future_field: u64,
        }
        let bytes = crate::encoding::canonical_encode(&V2 {
            grant_id: vec![0x1D; 16],
            owner: [0xA1; 32],
            custodian_key: custodian().actor_id().0,
            held_bytes: 42,
            attested_at: Timestamp(7),
            future_field: 1,
        })
        .unwrap();
        let got: CustodyReceipt = crate::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(got.held_bytes, 42);
        assert!(got.covered.is_empty());
    }

    /// The check-in cadence, exhaustively over its four triggers — pure, so
    /// the clock is an argument and no test ever waits on one (convention 14:
    /// cadence *logic* tiers down to mock-clock tier_1).
    #[test]
    fn receipt_due_fires_on_exactly_its_four_triggers() {
        let t0 = Timestamp(1_000);
        let later = Timestamp(1_000 + CUSTODY_RECEIPT_INTERVAL_MICROS);
        let within = Timestamp(1_000 + CUSTODY_RECEIPT_INTERVAL_MICROS - 1);

        // Never attested → due immediately, whatever else is true.
        assert!(receipt_due(t0, Timestamp(0), false, false, false));

        // Freshly minted, nothing changed → not due.
        assert!(!receipt_due(within, t0, false, false, false));
        // ... and still-degraded (no flip) within the interval → not due.
        assert!(!receipt_due(within, t0, false, true, true));

        // The interval elapsed → due.
        assert!(receipt_due(later, t0, false, false, false));

        // Eviction this pass → due now, interval or not.
        assert!(receipt_due(within, t0, true, false, false));

        // The degraded verdict flipped — in EITHER direction → due now.
        assert!(receipt_due(within, t0, false, true, false));
        assert!(receipt_due(within, t0, false, false, true));

        // A clock that went backwards saturates rather than wrapping into
        // "due" (u64 subtraction would otherwise underflow).
        assert!(!receipt_due(Timestamp(500), t0, false, false, false));
    }
}
