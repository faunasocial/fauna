//! The **target-authored "cannot key generation G" signal** — the production
//! `fauna.state.generation-unkeyable` writer (`account-data-plane.md`
//! § The generation machinery, the unkeyable kind's bullet).
//!
//! # The partition this closes
//!
//! The hardening left exactly one durable forged-partition case:
//! **in-place corruption of a mint member's own inline wrap**. The member is
//! in the signed `member_ids` set and a wrap is present, so the top-up pass
//! reads the pair as covered — healers cannot distinguish corruption from
//! health, because only the target can try the open. The read path already
//! falls through to top-up rows, but no healer publishes one; the partition
//! is durable until the target itself testifies. The same costume fits a
//! malicious *verified member* healer publishing a verifying garbage top-up:
//! authenticated-looking coverage that opens nothing.
//!
//! This pass is that testimony: for every live mint this device **cannot key
//! despite apparent coverage**, publish a target-signed `Asserted` row naming
//! the evidence already tried and failed; once the generation keys again,
//! retract with a later `Satisfied` in the same cell.
//!
//! # What publishes when
//!
//! Once per full pump pass, per live canonical `Minted` row that passes the
//! **authorship gate**: its Key↔id binding holds, its authorship verifies
//! ([`fauna_core::generation::verify_mint_authorship`] — the resolver's own
//! ST-007 gate, shared: non-empty member set, minter inside it, `minter_sig`
//! verifying over the recomputed id), and its minter is a currently-verified
//! non-removed member. A squatted row keys nothing and heals nothing; an
//! *invented* core binds to its own hash, so the binding alone does not stop
//! one — and mint rows are `Gen0`, adopted with no signature check, so any
//! `BackupKey` holder (a removed device included) could otherwise list the
//! current members beside a garbage wrap and extract a permanent signal row
//! per member per forgery. The verified-minter clause is what refuses the
//! removed device's validly *self-signed* invention. The cost: a live mint
//! whose honest minter has since been removed extracts no testimony — the
//! resolver already treats it as inadmissible, and the removal's heal-mint
//! supersedes it.
//!
//! - **Can key it** (plane wrap or retained bundle): publish `Satisfied` iff
//!   the merged own cell currently holds a verifying `Asserted` — otherwise
//!   byte-quiet. Retraction is what returns the fleet to the ordinary
//!   coverage rules.
//! - **Cannot key it, and the pair is apparently covered** (inline coverage
//!   on a gated mint — this device in the authenticated `member_ids` plus a
//!   wrap entry present, the wrap itself being outside every signature; or a
//!   verifying top-up by a currently-verified non-removed member — the healer
//!   pass's own predicate, mirrored):
//!   publish `Asserted` with the current evidence set — iff the merged own
//!   cell does not already hold a verifying `Asserted` with the identical
//!   `tried` set (byte-quiet on convergence; a healer answers each assertion
//!   at most once, so re-publishing identical testimony would churn without
//!   healing).
//! - **Cannot key it, no apparent coverage**: publish nothing. The ordinary
//!   top-up pass already heals a plainly-missing wrap, and signalling it
//!   would cost two rows per ordinary enrollment race for nothing.
//! - **A `Shredded` mint**: nothing — no assertion (there is nothing to
//!   heal; handing out the key would defeat the crypto-shred) and no
//!   retraction (healers skip shredded mints before consulting signals, so a
//!   stale assertion is inert, not churning).
//!
//! # The evidence, and why it is exactly this set
//!
//! `tried` names the BLAKE3 hashes of the wrap ciphertexts this device
//! attempted: its own inline `MemberWrap` entries on the winning mint row,
//! plus every cell-verifying per-healer row targeting it **whose healer is a
//! currently-verified non-removed member**. The healer gate consults only its
//! own per-healer cell, so those are the only hashes that gate anything;
//! unverified-healer wraps are still *tried* on read
//! (integrity-only, pure gain) but excluded here — the cap
//! ([`fauna_core::generation::MAX_UNKEYABLE_TRIED`]) is sized for the real
//! fleet bound, and admitting unauthenticated junk would let a flood evict
//! the hashes that matter.
//!
//! # The anti-churn bound
//!
//! A forged signal cannot exist (rows verify only under the target's own
//! device key). A target that crashes after asserting extracts at most one
//! wrap per healer for that assertion, then the fleet is byte-quiet forever.
//! A malicious target must sign a fresh assertion naming the healers'
//! *current* row hashes to extract another round — amplification bounded by
//! fleet size per target-signed row, self-inflicted, vandalism-grade (the
//! target is enrolled). A converged pair (heal opened, `Satisfied` merged)
//! stops producing bytes on both sides.

use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use fauna_account_store::{backend::StoreBackend, store::AccountStore};
use fauna_core::generation::{
    FleetView, GenerationMintRecord, GenerationUnkeyableRecord, MAX_UNKEYABLE_TRIED,
    UNKEYABLE_VARIANT_ASSERTED, UNKEYABLE_VARIANT_SATISFIED, sign_unkeyable_as_target,
    unkeyable_cell_key, verify_mint_authorship,
};
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::{
    KIND_DEVICE_SET, KIND_GENERATION_MINT, KIND_GENERATION_UNKEYABLE, LwwStamp,
};

use crate::account_state_plane::{AccountStatePlane, ItemId};
use crate::generation_store::{live_rows, row_ref};
use crate::generation_tip::{self, GenerationTrust};
use crate::generation_topup::{WrapCoverage, wrap_coverage};

/// What one signal pass did (the pump's `generation_unkeyable` report slot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnkeyablePass {
    /// Nothing to say: every apparently-covered generation keys here, and no
    /// stale assertion of ours needed retracting.
    Current,
    /// This many `fauna.state.generation-unkeyable` rows (assertions +
    /// retractions) went through the door.
    Published(usize),
}

/// Publish this device's "cannot key" testimony where — and only where — it
/// is true and load-bearing. Module docs own the decision table.
pub async fn ensure_signalled<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    trust: &GenerationTrust,
    writer_key: &SigningKey,
) -> Result<UnkeyablePass>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let device_id = writer_key.verifying_key().to_bytes();
    let device_rows = live_rows(store, KIND_DEVICE_SET).await?;
    let view = FleetView::build(&trust.root, device_rows.iter().map(row_ref));
    let coverage = wrap_coverage(store, &view).await?;
    let own_cells = own_signal_cells(store, &device_id).await?;
    let mut published = 0usize;

    for entry in live_rows(store, KIND_GENERATION_MINT).await? {
        // One generation is one unit of local work (`pass_breath` module docs).
        crate::pass_breath::pass_breath().await;
        // Canonical-or-skip, the Key↔id binding, then authenticated
        // authorship by a currently-verified minter (module docs: squatted
        // and invented rows alike must extract no testimony).
        let Ok(generation_id) = fauna_core::hex32::decode(&entry.key) else {
            continue;
        };
        if fauna_core::hex32::encode(&generation_id) != entry.key {
            continue;
        }
        let record: GenerationMintRecord =
            match fauna_core::encoding::canonical_decode(&entry.value) {
                Ok(r) => r,
                Err(_) => continue,
            };
        let GenerationMintRecord::Minted {
            core,
            minter_sig,
            wraps,
        } = record
        else {
            // Shredded — module docs: no assertion, no retraction.
            continue;
        };
        if fauna_core::generation::generation_id(&core).ok() != Some(generation_id) {
            continue;
        }
        if verify_mint_authorship(&generation_id, &core, &minter_sig).is_err()
            || !view.is_verified_member(&core.minter)
        {
            continue;
        }

        let can_key = generation_tip::generation_key_for(
            store,
            &generation_id,
            writer_key,
            fleet.generation_custody(),
            Some(&view),
        )
        .await?
        .is_some();
        let own_cell = own_cells.get(&generation_id);

        if can_key {
            // Retract a standing assertion; anything else is byte-quiet.
            if let Some(GenerationUnkeyableRecord::Asserted { .. }) = own_cell {
                put_signal(fleet, &generation_id, writer_key, None, own_cell).await?;
                published += 1;
            }
            continue;
        }

        // Apparent coverage — the healer pass's predicate, mirrored: this is
        // exactly the case the ordinary top-up pass will never heal.
        let inline_covered =
            core.member_ids.contains(&device_id) && wraps.iter().any(|w| w.device_id == device_id);
        let topup_covered = coverage.covered.contains(&(generation_id, device_id));
        if !inline_covered && !topup_covered {
            continue;
        }

        let evidence = tried_evidence(&generation_id, &device_id, &wraps, &coverage, &view);
        if let Some(GenerationUnkeyableRecord::Asserted { tried, .. }) = own_cell
            && *tried == evidence
        {
            // The merged cell already carries exactly this testimony — the
            // healers' once-per-assertion answers are in flight or spent.
            continue;
        }
        put_signal(fleet, &generation_id, writer_key, Some(evidence), own_cell).await?;
        published += 1;
    }

    if published == 0 {
        Ok(UnkeyablePass::Current)
    } else {
        tracing::info!(
            rows = published,
            "generation unkeyable: published cannot-key testimony / retractions"
        );
        Ok(UnkeyablePass::Published(published))
    }
}

/// The `tried` evidence for one (generation, this device) assertion — sorted,
/// deduped, capped by byte order (module docs own why exactly this set).
fn tried_evidence(
    generation_id: &[u8; 32],
    device_id: &[u8; 32],
    inline: &[fauna_core::generation::MemberWrap],
    coverage: &WrapCoverage,
    view: &FleetView,
) -> Vec<[u8; 32]> {
    let mut tried: std::collections::BTreeSet<[u8; 32]> = inline
        .iter()
        .filter(|w| w.device_id == *device_id)
        .map(|w| *blake3::hash(&w.wrap).as_bytes())
        .collect();
    for ((g, t, healer), facts) in &coverage.rows {
        if g == generation_id && t == device_id && view.is_verified_member(healer) {
            tried.insert(facts.wrap_hash);
        }
    }
    tried.into_iter().take(MAX_UNKEYABLE_TRIED).collect()
}

/// Publish one signal into this device's own cell: `Some(tried)` asserts,
/// `None` retracts. The signed stamp rises past the merged cell's current
/// record structurally — a clock regression must not make the join keep the
/// superseded record, which would freeze the cell's state forever.
async fn put_signal<B, R>(
    fleet: &AccountStatePlane<'_, B, R>,
    generation_id: &[u8; 32],
    writer_key: &SigningKey,
    tried: Option<Vec<[u8; 32]>>,
    current: Option<&GenerationUnkeyableRecord>,
) -> Result<()>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let device_id = writer_key.verifying_key().to_bytes();
    let at_ms = (fauna_core::data::Timestamp::now_millis_or_zero() as i64)
        .max(current.map_or(i64::MIN, |r| r.asserted_at_ms().saturating_add(1)));
    let record = match tried {
        Some(tried) => {
            let target_sig = sign_unkeyable_as_target(
                writer_key,
                generation_id,
                UNKEYABLE_VARIANT_ASSERTED,
                at_ms,
                &tried,
            );
            GenerationUnkeyableRecord::Asserted {
                generation_id: *generation_id,
                target_device: device_id,
                asserted_at_ms: at_ms,
                tried,
                target_sig,
            }
        }
        None => {
            let target_sig = sign_unkeyable_as_target(
                writer_key,
                generation_id,
                UNKEYABLE_VARIANT_SATISFIED,
                at_ms,
                &[],
            );
            GenerationUnkeyableRecord::Satisfied {
                generation_id: *generation_id,
                target_device: device_id,
                asserted_at_ms: at_ms,
                target_sig,
            }
        }
    };
    // The outer stamp is transport convention only — the record's truth is
    // its signed stamp, and the kind's join never consults the outer one.
    let stamp = LwwStamp {
        at_ms,
        writer: device_id,
    };
    fleet
        .put(
            &ItemId {
                kind: KIND_GENERATION_UNKEYABLE.into(),
                key: unkeyable_cell_key(generation_id, &device_id),
            },
            fauna_core::encoding::canonical_encode(&record)?,
            Some(stamp.encode()?),
        )
        .await
        .with_context(|| {
            format!(
                "publishing a generation-unkeyable signal for {}",
                fauna_core::hex32::encode(generation_id)
            )
        })?;
    Ok(())
}

/// This device's own merged signal cells, generation → the verifying record.
/// A junk or forged occupant of our own cell reads as no record — the next
/// publication displaces it (the join prefers verifying bytes).
async fn own_signal_cells<B: StoreBackend>(
    store: &AccountStore<B>,
    device_id: &[u8; 32],
) -> Result<std::collections::BTreeMap<[u8; 32], GenerationUnkeyableRecord>> {
    use fauna_core::generation::parse_unkeyable_cell_key;
    let mut out = std::collections::BTreeMap::new();
    for entry in live_rows(store, KIND_GENERATION_UNKEYABLE).await? {
        let Some((generation_id, target_device)) = parse_unkeyable_cell_key(&entry.key) else {
            continue;
        };
        if target_device != *device_id {
            continue;
        }
        let Ok(record) =
            fauna_core::encoding::canonical_decode::<GenerationUnkeyableRecord>(&entry.value)
        else {
            continue;
        };
        if !record.verifies_at(&generation_id, &target_device) {
            continue;
        }
        out.insert(generation_id, record);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation_fixture_test_support::*;
    use fauna_core::generation::{
        EscrowTargetRecord, derive_escrow_xwing_keypair, sign_mint_as_minter,
    };
    use fauna_mls::wrapped_blob::generation_wraps::{build_mint, build_topup_wrap_v2};

    // `NoNest`, the `ROOT_SEED`/`US`/`THEM`/`ESCROW_SEED` key set, `root`/
    // `device_key`/`device_id_of`/`member_of`/`enrollment_row`/`machinery_row`,
    // and `Fixture`/`fixture`/`Fixture::{plane,put,mint_over}` all come from
    // `generation_fixture_test_support` now — this suite's own twin in
    // `generation_topup.rs` hand-copied the identical scaffolding.

    impl Fixture {
        /// Corrupt OUR OWN inline wrap in place — the durable case's exact
        /// shape: we stay in the signed `member_ids`, a wrap stays present,
        /// and the poisoned variant wins the mint join (byte-order max among
        /// `Minted`; 0xFF-filled bytes at the first difference).
        async fn corrupt_own_inline_wrap(&self, generation_id: &[u8; 32]) -> [u8; 32] {
            let honest = live_rows(&self.store, KIND_GENERATION_MINT).await.unwrap();
            let honest_bytes = honest
                .iter()
                .find(|r| r.key == fauna_core::hex32::encode(generation_id))
                .expect("mint row")
                .value
                .clone();
            let mut rec: GenerationMintRecord =
                fauna_core::encoding::canonical_decode(&honest_bytes).unwrap();
            let GenerationMintRecord::Minted { wraps, .. } = &mut rec else {
                panic!("minted")
            };
            let own = wraps
                .iter_mut()
                .find(|w| w.device_id == device_id_of(US))
                .expect("our inline wrap");
            own.wrap = vec![0xFF; own.wrap.len()];
            let corrupted_hash = *blake3::hash(&own.wrap).as_bytes();
            let poisoned_bytes = fauna_core::encoding::canonical_encode(&rec).unwrap();
            assert_eq!(
                fauna_core::generation::join_generation_mint(&honest_bytes, &poisoned_bytes)
                    .unwrap(),
                poisoned_bytes,
                "the poisoned variant wins the mint join"
            );
            self.put(machinery_row(
                KIND_GENERATION_MINT,
                fauna_core::hex32::encode(generation_id),
                &rec,
            ))
            .await;
            corrupted_hash
        }

        async fn own_cell(&self, generation_id: &[u8; 32]) -> Option<GenerationUnkeyableRecord> {
            own_signal_cells(&self.store, &device_id_of(US))
                .await
                .unwrap()
                .remove(generation_id)
        }
    }

    /// **The pin this pass exists for.** Our own inline wrap is corrupted in
    /// place — the durable case — so we publish a verifying
    /// `Asserted` naming exactly the corrupted ciphertext's hash as tried.
    #[tokio::test]
    async fn a_corrupted_inline_wrap_produces_a_verifying_assertion() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        let (generation_id, _key, _core) = f.mint_over(&[member_of(US), member_of(THEM)]).await;
        let corrupted_hash = f.corrupt_own_inline_wrap(&generation_id).await;

        let plane = f.plane();
        let outcome = ensure_signalled(&f.store, &plane, &f.trust, &f.writer_key)
            .await
            .expect("pass");
        assert_eq!(outcome, UnkeyablePass::Published(1));

        let record = f.own_cell(&generation_id).await.expect("the cell row");
        let GenerationUnkeyableRecord::Asserted { tried, .. } = &record else {
            panic!("asserted");
        };
        assert_eq!(
            tried.as_slice(),
            &[corrupted_hash],
            "the evidence names exactly the wrap we tried and failed"
        );
    }

    /// Byte-quiet on convergence: identical evidence republishes nothing —
    /// the anti-churn half of the assertion side.
    #[tokio::test]
    async fn a_second_pass_with_identical_evidence_is_byte_quiet() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        let (generation_id, _key, _core) = f.mint_over(&[member_of(US), member_of(THEM)]).await;
        f.corrupt_own_inline_wrap(&generation_id).await;

        let plane = f.plane();
        assert_eq!(
            ensure_signalled(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            UnkeyablePass::Published(1)
        );
        let first = f.own_cell(&generation_id).await.expect("cell");
        assert_eq!(
            ensure_signalled(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            UnkeyablePass::Current,
            "identical testimony is never republished"
        );
        assert_eq!(f.own_cell(&generation_id).await.unwrap(), first);
    }

    /// A plainly-missing wrap (not in the member set, no top-up) signals
    /// nothing: the ordinary top-up pass heals it, and signalling would cost
    /// two rows per ordinary enrollment race.
    #[tokio::test]
    async fn no_apparent_coverage_publishes_no_signal() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        // A mint over THEM alone, minted by THEM: we are not in `member_ids`,
        // no wrap, no top-up — cannot key, but plainly so.
        let escrow = EscrowTargetRecord {
            xwing_escrow_pubkey: derive_escrow_xwing_keypair(&ESCROW_SEED)
                .public
                .to_bytes()
                .to_vec(),
        };
        let built = build_mint(
            &[member_of(THEM)],
            &escrow,
            &crate::generation_fixture_test_support::target_key(),
            Vec::new(),
            &device_key(THEM),
            7_000,
        )
        .expect("mint");
        f.put(machinery_row(
            KIND_GENERATION_MINT,
            fauna_core::hex32::encode(&built.generation_id),
            &built.record,
        ))
        .await;
        let generation_id = built.generation_id;

        let plane = f.plane();
        assert_eq!(
            ensure_signalled(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            UnkeyablePass::Current
        );
        assert!(f.own_cell(&generation_id).await.is_none());
    }

    /// A generation we can key produces nothing — and retires a standing
    /// assertion with a later `Satisfied` once a heal lands, after which the
    /// pass is byte-quiet again.
    #[tokio::test]
    async fn a_heal_is_retracted_with_a_later_satisfied_then_quiet() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        let (generation_id, gen_key, _core) = f.mint_over(&[member_of(US), member_of(THEM)]).await;
        f.corrupt_own_inline_wrap(&generation_id).await;

        let plane = f.plane();
        assert_eq!(
            ensure_signalled(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            UnkeyablePass::Published(1)
        );
        let asserted_at = f.own_cell(&generation_id).await.unwrap().asserted_at_ms();

        // A healer's top-up lands (THEM heals us).
        let v2 = build_topup_wrap_v2(
            &gen_key,
            &generation_id,
            &member_of(US),
            &device_key(THEM),
            8_000,
        )
        .unwrap();
        f.put(machinery_row(
            fauna_protocol::merge_policy::KIND_GENERATION_WRAP,
            fauna_core::generation::wrap_cell_key_per_healer(
                &generation_id,
                &device_id_of(US),
                &device_id_of(THEM),
            ),
            &v2,
        ))
        .await;

        assert_eq!(
            ensure_signalled(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            UnkeyablePass::Published(1),
            "the standing assertion is retracted"
        );
        let record = f.own_cell(&generation_id).await.expect("cell");
        assert!(matches!(
            record,
            GenerationUnkeyableRecord::Satisfied { .. }
        ));
        assert!(
            record.asserted_at_ms() > asserted_at,
            "the retraction's signed stamp rises past the assertion's"
        );
        assert!(record.verifies_at(&generation_id, &device_id_of(US)));

        assert_eq!(
            ensure_signalled(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            UnkeyablePass::Current,
            "a converged pair goes byte-quiet"
        );
    }

    /// **The stamp-floor pin, timing-independent (convention 14).** Our own
    /// cell holds a verifying assertion stamped in the far future (a skewed
    /// clock's leftover); once we can key the generation, the retraction must
    /// outrank it in the join — without the floor, the `Satisfied` loses to
    /// the old stamp and the fleet keeps answering an assertion that is no
    /// longer true.
    #[tokio::test]
    async fn a_retraction_outranks_a_far_future_assertion() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        // An ordinary healthy mint — we can key it.
        let (generation_id, _key, _core) = f.mint_over(&[member_of(US), member_of(THEM)]).await;

        let far_future = i64::MAX - 10;
        let target_sig = sign_unkeyable_as_target(
            &f.writer_key,
            &generation_id,
            UNKEYABLE_VARIANT_ASSERTED,
            far_future,
            &[[7u8; 32]],
        );
        f.put(machinery_row(
            KIND_GENERATION_UNKEYABLE,
            unkeyable_cell_key(&generation_id, &device_id_of(US)),
            &GenerationUnkeyableRecord::Asserted {
                generation_id,
                target_device: device_id_of(US),
                asserted_at_ms: far_future,
                tried: vec![[7u8; 32]],
                target_sig,
            },
        ))
        .await;

        let plane = f.plane();
        assert_eq!(
            ensure_signalled(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            UnkeyablePass::Published(1)
        );
        let record = f.own_cell(&generation_id).await.expect("cell");
        assert!(
            matches!(record, GenerationUnkeyableRecord::Satisfied { .. }),
            "the retraction must actually displace the assertion in the merged cell"
        );
        assert!(record.asserted_at_ms() > far_future);
    }

    /// A squatted mint row (key↔id binding fails) extracts no testimony,
    /// however covered it makes itself look — a forged-mint flood must not
    /// extract a signal row per forgery.
    #[tokio::test]
    async fn a_squatted_mint_row_extracts_no_signal() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        let (generation_id, _key, _core) = f.mint_over(&[member_of(US), member_of(THEM)]).await;

        // Re-file the honest record's bytes under a squatted key.
        let honest = live_rows(&f.store, KIND_GENERATION_MINT).await.unwrap();
        let rec: GenerationMintRecord =
            fauna_core::encoding::canonical_decode(&honest[0].value).unwrap();
        let mut squatted_id = generation_id;
        squatted_id[0] ^= 0xFF;
        // Corrupt our wrap inside it too — covered-looking and unkeyable.
        let mut rec = rec;
        if let GenerationMintRecord::Minted { wraps, .. } = &mut rec
            && let Some(own) = wraps.iter_mut().find(|w| w.device_id == device_id_of(US))
        {
            own.wrap = vec![0xFF; own.wrap.len()];
        }
        f.put(machinery_row(
            KIND_GENERATION_MINT,
            fauna_core::hex32::encode(&squatted_id),
            &rec,
        ))
        .await;

        let plane = f.plane();
        assert_eq!(
            ensure_signalled(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            UnkeyablePass::Current,
            "a squatted row keys nothing, heals nothing, and extracts nothing"
        );
        assert!(f.own_cell(&squatted_id).await.is_none());
    }

    /// A third device's seed — never a member of the fixture's fleet.
    const OUTSIDER: [u8; 32] = [0x3Cu8; 32];

    /// A **newly invented** mint core — its id is its own hash, so the Key↔id
    /// binding holds — listing `members` (we are among them) beside a garbage
    /// inline wrap for us: covered-looking and unkeyable, the
    /// flood's unit. `sign` produces the `minter_sig` over the recomputed id.
    async fn invent_mint(
        f: &Fixture,
        minter: [u8; 32],
        members: &[[u8; 32]],
        sign: impl FnOnce(&[u8; 32]) -> Vec<u8>,
    ) -> [u8; 32] {
        let core = fauna_core::generation::MintCore {
            parents: Vec::new(),
            member_ids: members.iter().map(|s| device_id_of(*s)).collect(),
            minter: device_id_of(minter),
            key_commitment: [0x42; 32],
            minted_at_ms: 8_000,
        };
        let generation_id = fauna_core::generation::generation_id(&core).unwrap();
        let minter_sig = sign(&generation_id);
        f.put(machinery_row(
            KIND_GENERATION_MINT,
            fauna_core::hex32::encode(&generation_id),
            &GenerationMintRecord::Minted {
                core,
                minter_sig,
                wraps: vec![fauna_core::generation::MemberWrap {
                    device_id: device_id_of(US),
                    wrap: vec![0xAB; 1_200],
                }],
            },
        ))
        .await;
        generation_id
    }

    async fn assert_extracts_nothing(f: &Fixture, generation_id: &[u8; 32], why: &str) {
        let plane = f.plane();
        assert_eq!(
            ensure_signalled(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            UnkeyablePass::Current,
            "{why}"
        );
        assert!(f.own_cell(generation_id).await.is_none(), "{why}");
    }

    /// **The unsigned invention.** Any `BackupKey` holder can write a `Gen0`
    /// mint row; an invented core with no `minter_sig` binds to its own hash
    /// yet extracts no testimony.
    #[tokio::test]
    async fn an_invented_unsigned_mint_extracts_no_signal() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        let g = invent_mint(&f, THEM, &[US, THEM], |_| Vec::new()).await;
        assert_extracts_nothing(&f, &g, "an unsigned invented mint extracted testimony").await;
    }

    /// **The non-member signature.** Naming a verified member as minter under
    /// someone else's signature fails authorship; a genuinely self-signed
    /// outsider naming itself as minter fails the member-set clause. Neither
    /// extracts testimony.
    #[tokio::test]
    async fn an_invented_non_member_signed_mint_extracts_no_signal() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        let forged = invent_mint(&f, THEM, &[US, THEM], |id| {
            sign_mint_as_minter(&device_key(OUTSIDER), id)
        })
        .await;
        let outside = invent_mint(&f, OUTSIDER, &[US, THEM], |id| {
            sign_mint_as_minter(&device_key(OUTSIDER), id)
        })
        .await;
        assert_extracts_nothing(&f, &forged, "a non-member's signature extracted testimony").await;
        assert!(f.own_cell(&outside).await.is_none());
    }

    /// **The removed device's self-signed invention.** The authorship gate
    /// alone passes it (the minter is in its own member set and signs validly
    /// with its own key); only the verified-minter clause refuses it.
    #[tokio::test]
    async fn a_removed_devices_self_signed_mint_extracts_no_signal() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        f.put(machinery_row(
            KIND_DEVICE_SET,
            fauna_core::hex32::encode(&device_id_of(THEM)),
            &fauna_core::generation::DeviceSetRecord::Removed {
                removed_at_ms: 9_000,
                removed_by: device_id_of(US),
            },
        ))
        .await;
        let g = invent_mint(&f, THEM, &[US, THEM], |id| {
            sign_mint_as_minter(&device_key(THEM), id)
        })
        .await;
        assert_extracts_nothing(
            &f,
            &g,
            "a removed minter's self-signed mint extracted testimony",
        )
        .await;
    }

    /// The gate's control: the identical covered-and-unkeyable shape, signed
    /// by a currently-verified member minter, DOES assert — the gate refuses
    /// forgeries, not unkeyability.
    #[tokio::test]
    async fn a_verified_minters_unkeyable_mint_still_asserts() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        let g = invent_mint(&f, THEM, &[US, THEM], |id| {
            sign_mint_as_minter(&device_key(THEM), id)
        })
        .await;
        let plane = f.plane();
        assert_eq!(
            ensure_signalled(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            UnkeyablePass::Published(1)
        );
        assert!(matches!(
            f.own_cell(&g).await,
            Some(GenerationUnkeyableRecord::Asserted { .. })
        ));
    }
}
