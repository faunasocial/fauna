//! **The unkeyed hold's predicate** — whether a listed replica may answer a
//! read of a tip-sealed kind yet (`account-client-lifecycle.md` § The
//! client-side lifecycle → *The first listing*, clause (5)).
//!
//! `listed` says the replica has seen every row the bound nest holds; it does
//! not say the replica could open them. A row a listing left unopened for want
//! of a generation's key may be the account's value of any tip-sealed kind —
//! the envelope names its generation and nothing else — so a fold of what the
//! replica could open is no answer. The store records which generations those
//! were (`AccountStore::unkeyed`, written by the bound fleet plane's listings,
//! the answered-empty bit by the escrow recovery), and this module decides,
//! from merged state, whether any of them still **holds**: whether a source of
//! its key still stands for this device. The first-listing gate refuses every
//! gated tip-sealed read while one does
//! (`account_driver::handle_source::read_gate`).
//!
//! One function, one rule — the clause's three sources and nothing else. In
//! particular the top-up pass's inline and per-healer coverage is NOT sibling
//! evidence (clause (5), *Rejected*): that predicate checks no authorship, so
//! an invented mint naming a live member beside a wrap that opens nothing
//! would hold for ever. Only the reach map counts, whose rows are each signed
//! by a currently verified member.

use anyhow::Result;
use ed25519_dalek::SigningKey;
use fauna_account_store::{backend::StoreBackend, store::AccountStore};
use fauna_core::generation::{
    FleetView, GenerationMintRecord, escrow_acked_generations, escrow_target_identity_key,
    verify_mint_authorship,
};
use fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE;
use fauna_protocol::merge_policy::{KIND_DEVICE_SET, KIND_ESCROW_RECEIPT, KIND_GENERATION_MINT};

use crate::generation_store::{live_rows, row_ref};
use crate::generation_tip::{self, GenerationTrust, RetainedKeyCustody};

/// Which sources of a held generation's key stand for this device — the
/// union over every generation that holds. Nothing standing: no hold.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HoldSources {
    /// (a) The device itself keys the generation now — a top-up merged since,
    /// or a recovery another runtime of the machine made — and no full
    /// listing has re-presented the rows yet.
    pub device: bool,
    /// (b) This runtime holds the identity seed, merged state carries the
    /// receipt the escrow recovery asks on, and the holder has not answered
    /// with no wrap that opens.
    pub holder: bool,
    /// (c) A currently verified, non-removed device other than this one
    /// minted the generation (its authorship verifying) or lists it in its
    /// verifying reach row.
    pub sibling: bool,
}

impl HoldSources {
    /// Whether any generation holds.
    pub fn holds(&self) -> bool {
        self.device || self.holder || self.sibling
    }

    /// Whether the nest is what the hold waits for — the device itself or the
    /// holder stands (`common.needs_nest`) — rather than a sibling alone.
    pub fn needs_nest(&self) -> bool {
        self.device || self.holder
    }
}

/// What the predicate reads beside the store: this runtime's identity and
/// what it holds.
pub struct HoldContext<'a> {
    pub trust: &'a GenerationTrust,
    /// This device's writer key — its fleet id is the public half.
    pub writer_key: &'a SigningKey,
    /// The retained-bundle custody the planes read keys through.
    pub custody: Option<&'a dyn RetainedKeyCustody>,
    /// Whether this runtime holds the identity seed — the seed-holding app,
    /// never the seedless agent (bound (ii)).
    pub seed_holding: bool,
    /// The pinned holders' verified ancestors (`bind_leg::BindMemo`), which
    /// count for the receipt exactly as they do for the recovery that asks.
    pub ancestors: &'a [[u8; 32]],
}

/// **The hold predicate**: which sources stand for the generations the store
/// records this replica's fleet listings left rows unopened under.
///
/// A recorded generation holds only while merged state carries its mint row
/// live, canonical, `Minted` and id-bound (an invented or orphaned id holds
/// nothing), and then only on the clause's three sources ([`HoldSources`]).
///
/// # Errors
///
/// Store I/O only — row content never fails it.
pub async fn unkeyed_hold<B: StoreBackend>(
    store: &AccountStore<B>,
    cx: &HoldContext<'_>,
) -> Result<HoldSources> {
    let recorded = store.unkeyed(ACCOUNT_STATE_FLEET_SCOPE).await?;
    let mut sources = HoldSources::default();
    if recorded.is_empty() {
        return Ok(sources);
    }
    let me = cx.writer_key.verifying_key().to_bytes();
    // Built once, read per generation: the receipt set (b) and the view and
    // reach map (c).
    let holder_may_answer = cx.seed_holding && cx.custody.is_some();
    let acked = if holder_may_answer {
        let receipt_rows = live_rows(store, KIND_ESCROW_RECEIPT).await?;
        let mut holders = cx.trust.trusted_holders.get();
        holders.extend(
            cx.ancestors
                .iter()
                .filter(|a| !holders.contains(a))
                .copied()
                .collect::<Vec<_>>(),
        );
        escrow_acked_generations(
            receipt_rows.iter().map(row_ref),
            &holders,
            &escrow_target_identity_key(&cx.trust.root),
            &mut Vec::new(),
        )
    } else {
        Default::default()
    };
    let device_rows = live_rows(store, KIND_DEVICE_SET).await?;
    let view = FleetView::build(&cx.trust.root, device_rows.iter().map(row_ref));
    let reach = crate::generation_topup::wrap_coverage(store, &view)
        .await?
        .reach;

    for (generation_id, answered_empty) in recorded {
        let key_hex = fauna_core::hex32::encode(&generation_id);
        let Some(entry) = store.state(KIND_GENERATION_MINT, &key_hex).await? else {
            continue;
        };
        if entry.tombstone {
            continue;
        }
        let Ok(GenerationMintRecord::Minted {
            core, minter_sig, ..
        }) = fauna_core::encoding::canonical_decode::<GenerationMintRecord>(&entry.value)
        else {
            continue;
        };
        if fauna_core::generation::generation_id(&core).ok() != Some(generation_id) {
            continue;
        }
        if generation_tip::generation_key_for(store, &generation_id, cx.writer_key, cx.custody)
            .await?
            .is_some()
        {
            sources.device = true;
        }
        if holder_may_answer && acked.contains(&generation_id) && !answered_empty {
            sources.holder = true;
        }
        let minted_by_a_sibling = core.minter != me
            && view.is_verified_member(&core.minter)
            && verify_mint_authorship(&generation_id, &core, &minter_sig).is_ok();
        let reached_by_a_sibling = reach
            .iter()
            .any(|(device, record)| *device != me && record.holds.contains(&generation_id));
        if minted_by_a_sibling || reached_by_a_sibling {
            sources.sibling = true;
        }
    }
    Ok(sources)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation_fixture_test_support::{
        ESCROW_SEED, THEM, US, device_key, enrollment_row, fixture, machinery_row, member_of,
        target_key,
    };
    use fauna_core::generation::{EscrowTargetRecord, derive_escrow_xwing_keypair};
    use fauna_mls::wrapped_blob::generation_wraps::build_mint;
    use fauna_protocol::merge_policy::KIND_DEVICE_REACH;

    /// A seed outside the fleet: a vandal holding the `BackupKey` — a removed
    /// device keeps it for ever — can file rows, never a member's signature.
    const VANDAL: [u8; 32] = [0x5Au8; 32];

    /// A mint signed by `minter` over `members`, put into merged state and
    /// recorded as a generation this replica's listing left rows unopened
    /// under. No wrap reaches this device ([`US`] is never a member here).
    async fn recorded_mint(
        f: &crate::generation_fixture_test_support::Fixture,
        minter: [u8; 32],
        members: &[[u8; 32]],
    ) -> [u8; 32] {
        let escrow = EscrowTargetRecord {
            xwing_escrow_pubkey: derive_escrow_xwing_keypair(&ESCROW_SEED)
                .public
                .to_bytes()
                .to_vec(),
        };
        let members: Vec<_> = members.iter().map(|s| member_of(*s)).collect();
        let built = build_mint(
            &members,
            &escrow,
            &target_key(),
            Vec::new(),
            &device_key(minter),
            7_000,
        )
        .expect("mint");
        f.put(machinery_row(
            KIND_GENERATION_MINT,
            fauna_core::hex32::encode(&built.generation_id),
            &built.record,
        ))
        .await;
        f.store
            .add_unkeyed(
                ACCOUNT_STATE_FLEET_SCOPE,
                &[built.generation_id].into_iter().collect(),
            )
            .await
            .unwrap();
        built.generation_id
    }

    async fn sources(f: &crate::generation_fixture_test_support::Fixture) -> HoldSources {
        unkeyed_hold(
            &f.store,
            &HoldContext {
                trust: &f.trust,
                writer_key: &f.writer_key,
                custody: None,
                seed_holding: false,
                ancestors: &[],
            },
        )
        .await
        .unwrap()
    }

    /// The control: a generation an enrolled sibling minted holds this
    /// device, on the sibling's source alone.
    #[tokio::test]
    async fn a_generation_a_verified_sibling_minted_holds() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        recorded_mint(&f, THEM, &[THEM]).await;
        assert_eq!(
            sources(&f).await,
            HoldSources {
                sibling: true,
                ..Default::default()
            }
        );
    }

    /// **An invented mint holds nothing** (clause (5), *Why no generation
    /// holds for ever*): a self-consistent core whose minter signed it but is
    /// no verified member, naming a live member beside a wrap that opens
    /// nothing, with rows recorded under its id — no receipt, no member's
    /// signature, so no source stands.
    ///
    /// Shown to pin the authorship clause: with the minter's membership
    /// check taken out, it holds (the minter is in its own member set and
    /// its signature verifies).
    #[tokio::test]
    async fn an_invented_mint_naming_a_live_member_holds_nothing() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        recorded_mint(&f, VANDAL, &[VANDAL, THEM]).await;
        assert_eq!(sources(&f).await, HoldSources::default());
    }

    /// A sibling that lists the generation in its verifying reach row holds
    /// it, whoever minted it — the evidence a top-up-only sibling has.
    #[tokio::test]
    async fn a_verified_siblings_reach_row_holds_the_generation_it_lists() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        let id = recorded_mint(&f, VANDAL, &[VANDAL, THEM]).await;
        let reach = fauna_core::generation::sign_device_reach(&device_key(THEM), 8_000, [id]);
        f.put(machinery_row(
            KIND_DEVICE_REACH,
            fauna_core::generation::reach_cell_key(&reach.device_id),
            &reach,
        ))
        .await;
        assert_eq!(
            sources(&f).await,
            HoldSources {
                sibling: true,
                ..Default::default()
            }
        );
    }

    /// An id the store records with no mint row merged holds nothing: an
    /// orphaned id is no evidence.
    #[tokio::test]
    async fn a_recorded_id_with_no_mint_row_holds_nothing() {
        let f = fixture().await;
        f.store
            .add_unkeyed(
                ACCOUNT_STATE_FLEET_SCOPE,
                &[[0x42u8; 32]].into_iter().collect(),
            )
            .await
            .unwrap();
        assert_eq!(sources(&f).await, HoldSources::default());
    }
}
