//! The **escrow-recovery pass** — the one production caller of
//! `fauna.generation.escrow.get` (`account-data-taxonomy.md` § The generation
//! machinery → *Escrow recovery*).
//!
//! # The state this heals
//!
//! A single-device account signs out and back in: the new sign-in enrolls a
//! fresh device key, no member is left to top it up, and every row sealed
//! under the old generations — the account's own group-reception keypair among
//! them — is unreadable, though nothing was destroyed. The generation's escrow
//! wrap is the one thing that can re-open it, and a seed-holding runtime holds
//! what opens that wrap: the escrow secret is derived from the identity seed
//! (`fauna_core::generation::derive_escrow_xwing_keypair`).
//!
//! # What asks when
//!
//! Once per full pump pass, after the fleet walk and **before** the top-up
//! pass, per live canonical `Minted` row whose Key↔id binding holds:
//!
//! - **Keys here already** (retained bundle, inline wrap, any merged top-up):
//!   nothing — byte-quiet, no request.
//! - **No verifying receipt in merged state from a trusted holder or a
//!   verified ancestor of one**: nothing. A receipt is holder-signed and binds
//!   the generation id, so a forged mint row can never carry one — a
//!   forged-mint flood extracts no request. (An unacked mint was never sealed
//!   under, so there is nothing to read either.) The ancestors — superseded
//!   identities of the pinned nest, proven by its rotation chain
//!   (`bind_leg::fetch_verified_ancestors`) — count here and nowhere else
//!   (`account-data-taxonomy.md` § The generation machinery → *A holder change
//!   re-receipts and never mints*): a generation receipted only under the
//!   predecessor and keyed by no surviving device would otherwise never be
//!   asked for, though its wrap rests at the nest. The clause bounds requests;
//!   an opened wrap is still checked against its mint's key commitment.
//! - **Already answered this runtime life** ([`EscrowRecoveryMemo`]): nothing.
//! - **Otherwise**: one `escrow.get` filtered to that generation; every wrap
//!   in the reply is opened with the seed-derived secret and checked against
//!   the mint's key commitment. The first key that opens is recorded on the
//!   retained bundle — and that is the whole effect: from there
//!   `generation_key_for` answers from the bundle, this pass's top-up heals
//!   later devices, and the reclamation pass's reach lists the generation.
//!
//! A transport failure is not an answer: the generation stays un-memoed and
//! the next pass asks again, like every other best-effort pump step.
//!
//! # The kept wrap — a predecessor's generation
//!
//! A succession keeps the predecessor-keyed wraps at the holder
//! (`owner-key-material.md` § Path A-sibling-2 → *Rotation*, the succession
//! rider → *The kept wrap*), and the holder serves them to the successor. The
//! generations they key are ones this replica holds no mint row of — the walk
//! carries a predecessor's record only once the key is in hand — so the pass
//! takes them from the fleet walk instead: every predecessor mint record it
//! opened under the mint-kind keys and declined for want of the key
//! ([`AccountStatePlane::unkeyed_predecessor_mints`]). For each, not keyed
//! here and not yet answered, one filtered `escrow.get`; every wrap in the
//! reply is opened under this identity's escrow secret and then each attested
//! predecessor's, checked against the record's key commitment. The request
//! bound is the mint-kind open itself — only a holder of the retired
//! `BackupKey` could have sealed the record — plus the answered-once memo. A
//! key that checks is recorded and counted [`EscrowRecoveryPass::Recovered`],
//! so the pump re-presents the scope: the walk then carries the record, and
//! the re-escrow pass behind it deposits the generation under this identity,
//! which sweeps the kept wrap at the holder.
//!
//! # Why it does not wait for a healer
//!
//! The charter owns the argument: a dead-but-enrolled sibling's reach lists
//! the generation for ever and heals nobody, and a sleeping one leaves a new
//! sign-in blind until it wakes. A seed holder is entitled to every generation
//! key by design, so asking the holder at once exposes nothing.

use std::collections::BTreeSet;
use std::sync::{Mutex, OnceLock};

use anyhow::Result;
use ed25519_dalek::SigningKey;
use fauna_account_store::{backend::StoreBackend, store::AccountStore, types::StateEntry};
use fauna_core::crypto::GenerationKey;
use fauna_core::generation::{
    GenerationMintRecord, MintCore, derive_escrow_xwing_keypair, escrow_acked_generations,
};
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_mls::wrapped_blob::generation_wraps::open_generation_key_from_escrow;
use fauna_protocol::generation_escrow::{
    EscrowGetReply, EscrowGetRequest, EscrowWrapRow, KIND_ESCROW_GET,
};
use fauna_protocol::merge_policy::{KIND_ESCROW_RECEIPT, KIND_GENERATION_MINT};
use fauna_protocol::{ByteBuf, RpcRequester};

use crate::account_state_plane::AccountStatePlane;
use crate::generation_store::{live_rows, row_ref};
use crate::generation_tip::{self, GenerationTrust};

/// The generations the holder has already **answered** for, this runtime life
/// — with a key, or with no wrap that opens. It is what bounds the pass to one
/// request per live unkeyable generation per runtime start; held by the
/// runtime beside the pump, never persisted (a fresh start may ask once more,
/// which is the retry a changed holder state deserves).
#[derive(Debug, Default)]
pub struct EscrowRecoveryMemo(Mutex<BTreeSet<[u8; 32]>>);

impl EscrowRecoveryMemo {
    fn answered(&self, generation_id: &[u8; 32]) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(generation_id)
    }

    fn record_answer(&self, generation_id: [u8; 32]) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(generation_id);
    }
}

/// Opens escrow wraps with the identity seed's escrow secret — **the one
/// statement of how a seed holder opens a wrap** (`account-data-taxonomy.md`
/// § The generation machinery → *Escrow recovery*): the wrap names the
/// generation, it opens under the seed-derived secret bound to this identity's
/// target key, and the key it yields matches the mint's key commitment. Both
/// openers go through it — [`ensure_recovered`] and the box-recovery cold read
/// (`crate::deployment_seed_recovery`) — so neither can drift from the check.
///
/// The X-Wing keypair is derived on the first open and kept for the opener's
/// life (one derivation per pass, not per wrap).
pub struct EscrowOpener<'a> {
    identity_seed: &'a [u8; 32],
    target_key: String,
    keypair: OnceLock<fauna_pq_kem::XWingKeyPair>,
}

impl<'a> EscrowOpener<'a> {
    /// An opener for the identity `root` whose seed is `identity_seed`.
    pub fn new(identity_seed: &'a [u8; 32], root: &ActorId) -> Self {
        Self {
            identity_seed,
            target_key: fauna_core::generation::escrow_target_identity_key(root),
            keypair: OnceLock::new(),
        }
    }

    /// This identity's escrow-target key — the string its wraps seal under
    /// and its receipts name.
    pub fn target_key(&self) -> &str {
        &self.target_key
    }

    /// The first of `wraps` for `generation_id` that opens and matches
    /// `core`'s key commitment, or `None`.
    pub fn open(
        &self,
        wraps: &[EscrowWrapRow],
        generation_id: &[u8; 32],
        core: &MintCore,
    ) -> Option<GenerationKey> {
        let secret = &self
            .keypair
            .get_or_init(|| derive_escrow_xwing_keypair(self.identity_seed))
            .secret;
        wraps
            .iter()
            .filter(|w| w.generation_id.as_slice() == generation_id)
            .find_map(|w| {
                open_generation_key_from_escrow(
                    &w.wrap,
                    secret,
                    generation_id,
                    &self.target_key,
                    &core.key_commitment,
                )
                .ok()
            })
    }
}

/// A live `fauna.state.generation-mint` row's generation id and core, when the
/// row is canonical (its key is its id's canonical hex), `Minted` (a shred is a
/// deletion, never healed) and its Key↔id binding holds — the gates every
/// opener applies before trusting a mint's commitment; a squatted row keys
/// nothing.
pub fn minted_core(entry: &StateEntry) -> Option<([u8; 32], MintCore)> {
    let generation_id = fauna_core::hex32::decode(&entry.key).ok()?;
    if fauna_core::hex32::encode(&generation_id) != entry.key {
        return None;
    }
    let Ok(GenerationMintRecord::Minted { core, .. }) =
        fauna_core::encoding::canonical_decode::<GenerationMintRecord>(&entry.value)
    else {
        return None;
    };
    (fauna_core::generation::generation_id(&core).ok() == Some(generation_id))
        .then_some((generation_id, core))
}

/// What one recovery pass did (the pump's `generation_escrow_recovery` slot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscrowRecoveryPass {
    /// Nothing to recover: every acked live generation keys here, or the
    /// holder has already answered for the ones that do not.
    Current,
    /// This many generation keys were recovered onto the retained bundle.
    Recovered(usize),
}

/// Key every live generation this device cannot key from its escrow wrap.
/// Module docs own the decision table.
///
/// # Errors
///
/// Store I/O, or the **last** escrow-door failure of the pass — every
/// generation is still attempted, and a failed one is asked again next pass.
#[allow(clippy::too_many_arguments)] // Each is a distinct input the pump
// already holds for the pass; a bundling struct would only restate the list.
pub async fn ensure_recovered<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    trust: &GenerationTrust,
    writer_key: &SigningKey,
    identity_seed: &[u8; 32],
    predecessors: &[ActorKeypair],
    memo: &EscrowRecoveryMemo,
    ancestors: &[[u8; 32]],
) -> Result<EscrowRecoveryPass>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    // Nowhere to hold a recovered key → nothing this pass could change.
    let Some(custody) = fleet.generation_custody() else {
        return Ok(EscrowRecoveryPass::Current);
    };
    let receipt_rows = live_rows(store, KIND_ESCROW_RECEIPT).await?;
    // Built once for the pass: the reader consults it for a shred's
    // authorship alone (`generation_tip::generation_key_for`).
    let view = crate::fleet_removal::fleet_view(store, trust).await?;
    // This identity's target key: the wraps worth asking for are the ones
    // acked under it, and the ones that open are the ones sealed to it.
    let opener = EscrowOpener::new(identity_seed, &trust.root);
    // Recovery-only trust: the pinned holders and their verified ancestors.
    let mut holders = trust.trusted_holders.get();
    holders.extend(
        ancestors
            .iter()
            .filter(|a| !holders.contains(a))
            .copied()
            .collect::<Vec<_>>(),
    );
    let acked = escrow_acked_generations(
        receipt_rows.iter().map(row_ref),
        &holders,
        opener.target_key(),
        // The resolver owns reporting invalid receipts; here they just ack
        // nothing.
        &mut Vec::new(),
    );
    let mut recovered = 0usize;
    let mut door_failure = None;

    for entry in live_rows(store, KIND_GENERATION_MINT).await? {
        // One generation is one unit of local work — a KEM decap and a
        // bundle rewrite each — and by the middle of a whole-suite sweep an
        // account has 50–70 of them (`pass_breath` module docs).
        crate::pass_breath::pass_breath().await;
        // Canonical-or-skip, then the Key↔id binding — the unkeyable pass's
        // own gates, for the same reason: a squatted row keys nothing.
        let Some((generation_id, core)) = minted_core(&entry) else {
            continue;
        };
        if !acked.contains(&generation_id) || memo.answered(&generation_id) {
            continue;
        }
        if generation_tip::generation_key_for(
            store,
            &generation_id,
            writer_key,
            Some(custody),
            Some(&view),
        )
        .await?
        .is_some()
        {
            continue;
        }

        let reply: EscrowGetReply = match fleet
            .requester()
            .request(
                KIND_ESCROW_GET,
                EscrowGetRequest {
                    generation_id: Some(ByteBuf::from(generation_id.to_vec())),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(reply) => reply,
            Err(e) => {
                door_failure = Some(anyhow::anyhow!(
                    "escrow recovery of generation {}: the holder could not be asked: {e}",
                    entry.key
                ));
                continue;
            }
        };
        memo.record_answer(generation_id);

        match opener.open(&reply.wraps, &generation_id, &core) {
            Some(key) => {
                custody.record_generation_key(&generation_id, &key);
                recovered += 1;
            }
            // Deleted at the holder, or sealed to a predecessor's escrow key
            // (the succession re-escrow's business) — witnessed, not fatal.
            // The answer is remembered durably as the generation's
            // answered-empty bit, so the unkeyed hold stops counting the
            // holder as a source of its key across a relaunch
            // (`account-client-lifecycle.md` § The client-side lifecycle →
            // *The first listing*, clause (5)); the memo above still bounds
            // this runtime to the one question.
            None => {
                tracing::warn!(
                    generation = %entry.key,
                    wraps = reply.wraps.len(),
                    "escrow recovery: the holder serves no wrap of this generation that opens \
                     under this identity's escrow secret"
                );
                if store
                    .mark_unkeyed_answered_empty(fleet.scope(), &generation_id)
                    .await?
                    && let Some(first_listings) = fleet.first_listings()
                {
                    first_listings.note_unkeyed_changed();
                }
            }
        }
    }

    // The kept wrap: the predecessor generations the fleet walk declined for
    // want of the key (module docs).
    let predecessor_openers: Vec<EscrowOpener<'_>> = predecessors
        .iter()
        .map(|kp| EscrowOpener::new(kp.secret_bytes(), &kp.actor_id()))
        .collect();
    for candidate in fleet.unkeyed_predecessor_mints().unwrap_or_default() {
        crate::pass_breath::pass_breath().await;
        let generation_id = candidate.generation_id;
        if memo.answered(&generation_id) {
            continue;
        }
        if generation_tip::generation_key_for(
            store,
            &generation_id,
            writer_key,
            Some(custody),
            Some(&view),
        )
        .await?
        .is_some()
        {
            continue;
        }
        let reply: EscrowGetReply = match fleet
            .requester()
            .request(
                KIND_ESCROW_GET,
                EscrowGetRequest {
                    generation_id: Some(ByteBuf::from(generation_id.to_vec())),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(reply) => reply,
            Err(e) => {
                door_failure = Some(anyhow::anyhow!(
                    "escrow recovery of predecessor generation {}: the holder could not be \
                     asked: {e}",
                    fauna_core::hex32::encode(&generation_id)
                ));
                continue;
            }
        };
        memo.record_answer(generation_id);
        match std::iter::once(&opener)
            .chain(&predecessor_openers)
            .find_map(|o| o.open(&reply.wraps, &generation_id, &candidate.core))
        {
            Some(key) => {
                custody.record_generation_key(&generation_id, &key);
                recovered += 1;
            }
            None => tracing::warn!(
                generation = %fauna_core::hex32::encode(&generation_id),
                wraps = reply.wraps.len(),
                "escrow recovery: the holder serves no wrap of this predecessor generation that \
                 opens under this identity's or an attested predecessor's escrow secret"
            ),
        }
    }

    if let Some(e) = door_failure {
        if recovered > 0 {
            tracing::info!(recovered, "generation escrow recovery: keys recovered");
        }
        return Err(e);
    }
    if recovered == 0 {
        Ok(EscrowRecoveryPass::Current)
    } else {
        tracing::info!(recovered, "generation escrow recovery: keys recovered");
        Ok(EscrowRecoveryPass::Recovered(recovered))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation_fixture_test_support::{
        Bundle, ESCROW_SEED, Fixture, THEM, device_key, fixture, machinery_row, member_of,
        target_key,
    };
    use crate::generation_tip::RetainedKeyCustody;
    use fauna_core::crypto::GenerationKey;
    use fauna_core::generation::{EscrowTargetRecord, sign_escrow_receipt};
    use fauna_mls::wrapped_blob::generation_wraps::{build_mint, seal_generation_key_to_escrow};
    use fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE;
    use fauna_protocol::generation_escrow::EscrowWrapRow;
    use fauna_protocol::{decode_strict, encode_canonical};
    use std::sync::Arc;

    fn holder_key() -> SigningKey {
        SigningKey::from_bytes(&[0x66u8; 32])
    }

    /// `(generation id, wrap ciphertext)` rows, as the holder stores them.
    type DepositedWraps = Vec<([u8; 32], Vec<u8>)>;

    /// The holder's `get` door: serves the wraps it was handed, counts every
    /// request, and can be told to be unreachable.
    #[derive(Clone, Default)]
    struct Door {
        wraps: Arc<Mutex<DepositedWraps>>,
        asked: Arc<Mutex<Vec<[u8; 32]>>>,
        unreachable: Arc<Mutex<bool>>,
    }

    impl RpcRequester for Door {
        type Error = anyhow::Error;

        async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            assert_eq!(kind, KIND_ESCROW_GET, "the pass calls exactly one door");
            if *self.unreachable.lock().unwrap() {
                anyhow::bail!("holder unreachable");
            }
            let req: EscrowGetRequest =
                decode_strict(&encode_canonical(&payload).unwrap()).unwrap();
            let generation: [u8; 32] = req
                .generation_id
                .expect("the pass always filters to one generation")
                .as_slice()
                .try_into()
                .unwrap();
            self.asked.lock().unwrap().push(generation);
            let reply = EscrowGetReply {
                wraps: self
                    .wraps
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(g, _)| *g == generation)
                    .map(|(g, wrap)| EscrowWrapRow {
                        generation_id: ByteBuf::from(g.to_vec()),
                        wrap: ByteBuf::from(wrap.clone()),
                        deposited_at_ms: 7_000,
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            };
            Ok(decode_strict(&encode_canonical(&reply).unwrap()).unwrap())
        }
    }

    /// This identity's escrow target — `ESCROW_SEED`'s public half.
    fn escrow_target() -> EscrowTargetRecord {
        EscrowTargetRecord {
            xwing_escrow_pubkey: derive_escrow_xwing_keypair(&ESCROW_SEED)
                .public
                .to_bytes()
                .to_vec(),
        }
    }

    /// A generation minted by THEM alone — so this device holds no wrap of it
    /// — whose escrow wrap seals to `ESCROW_SEED`'s target and rests with the
    /// door; `acked` also lands the trusted holder's receipt row.
    async fn unkeyable_generation(f: &mut Fixture, door: &Door, acked: bool) -> [u8; 32] {
        f.trust.trusted_holders = vec![holder_key().verifying_key().to_bytes()].into();
        let built = build_mint(
            &[member_of(THEM)],
            &escrow_target(),
            &target_key(),
            Vec::new(),
            &device_key(THEM),
            7_000,
        )
        .expect("mint");
        let id_hex = fauna_core::hex32::encode(&built.generation_id);
        f.put(machinery_row(
            KIND_GENERATION_MINT,
            id_hex.clone(),
            &built.record,
        ))
        .await;
        if acked {
            let receipt = sign_escrow_receipt(
                &holder_key(),
                built.generation_id,
                blake3::hash(&built.escrow_wrap).into(),
                &target_key(),
                7_000,
            );
            f.put(machinery_row(
                KIND_ESCROW_RECEIPT,
                format!("{id_hex}/{}", fauna_core::hex32::encode(&receipt.holder_id)),
                &receipt,
            ))
            .await;
        }
        door.wraps
            .lock()
            .unwrap()
            .push((built.generation_id, built.escrow_wrap));
        built.generation_id
    }

    async fn pass(
        f: &Fixture,
        door: &Door,
        bundle: &Bundle,
        seed: &[u8; 32],
        memo: &EscrowRecoveryMemo,
    ) -> Result<EscrowRecoveryPass> {
        pass_admitting(f, door, bundle, seed, memo, &[]).await
    }

    /// [`pass`], admitting receipts from `ancestors` too.
    async fn pass_admitting(
        f: &Fixture,
        door: &Door,
        bundle: &Bundle,
        seed: &[u8; 32],
        memo: &EscrowRecoveryMemo,
        ancestors: &[[u8; 32]],
    ) -> Result<EscrowRecoveryPass> {
        let plane = AccountStatePlane::new_pull_only(
            &f.store,
            door,
            &f.schedule,
            &f.writer_key,
            &f.trust,
            ACCOUNT_STATE_FLEET_SCOPE,
        )
        .unwrap()
        .with_generation_custody(bundle);
        ensure_recovered(
            &f.store,
            &plane,
            &f.trust,
            &f.writer_key,
            seed,
            &[],
            memo,
            ancestors,
        )
        .await
    }

    /// The whole pass: an acked generation this device cannot key is asked
    /// for once, keyed from its escrow wrap onto the bundle, and never asked
    /// for again — neither by the memo's runtime nor by a fresh one, which
    /// finds it keyed.
    #[tokio::test]
    async fn an_acked_unkeyable_generation_is_recovered_and_asked_for_once() {
        let mut f = fixture().await;
        let (door, bundle) = (Door::default(), Bundle::default());
        let memo = EscrowRecoveryMemo::default();
        let g = unkeyable_generation(&mut f, &door, true).await;

        let first = pass(&f, &door, &bundle, &ESCROW_SEED, &memo).await.unwrap();
        assert_eq!(first, EscrowRecoveryPass::Recovered(1));
        assert!(bundle.retained_generation_key(&g).is_some());

        for memo in [&memo, &EscrowRecoveryMemo::default()] {
            let again = pass(&f, &door, &bundle, &ESCROW_SEED, memo).await.unwrap();
            assert_eq!(again, EscrowRecoveryPass::Current);
        }
        assert_eq!(*door.asked.lock().unwrap(), vec![g], "one request, ever");
    }

    /// **Recovery admits a verified ancestor** (`account-data-taxonomy.md`
    /// § The generation machinery → *A holder change re-receipts and never
    /// mints*): after the box rotated, a generation receipted only under its
    /// predecessor identity is asked for when — and only when — the pass is
    /// handed that predecessor as a verified ancestor of the pinned one; its
    /// wrap still rests at the nest, and it opens. Red-verified by dropping
    /// `ancestors` from the ack set.
    #[tokio::test]
    async fn a_generation_receipted_only_under_the_predecessor_is_recovered_through_its_ancestry() {
        let mut f = fixture().await;
        let (door, bundle) = (Door::default(), Bundle::default());
        let g = unkeyable_generation(&mut f, &door, true).await;
        let predecessor = holder_key().verifying_key().to_bytes();
        // The box rotated: the pin — the trusted set — is the successor now.
        f.trust.trusted_holders.replace(vec![
            SigningKey::from_bytes(&[0x67u8; 32])
                .verifying_key()
                .to_bytes(),
        ]);

        let memo = EscrowRecoveryMemo::default();
        let blind = pass(&f, &door, &bundle, &ESCROW_SEED, &memo).await.unwrap();
        assert_eq!(blind, EscrowRecoveryPass::Current);
        assert!(
            door.asked.lock().unwrap().is_empty(),
            "no trusted receipt, no ancestry: nothing is asked"
        );

        let healed = pass_admitting(&f, &door, &bundle, &ESCROW_SEED, &memo, &[predecessor])
            .await
            .unwrap();
        assert_eq!(healed, EscrowRecoveryPass::Recovered(1));
        assert!(bundle.retained_generation_key(&g).is_some());
    }

    /// **Every generation the pass considers ends in a breath** (`pass_breath`
    /// module docs): polled by hand over the instant fake door — nothing
    /// else in this pass can return `Pending` — a one-generation account
    /// yields exactly once per pass, on the recovering pass and on the
    /// already-keyed pass alike (the breath is per row considered, ahead of
    /// every skip). Red-verified: without the breath the pass resolves on
    /// its first poll.
    #[tokio::test]
    async fn every_generation_considered_ends_in_a_breath() {
        let mut f = fixture().await;
        let (door, bundle) = (Door::default(), Bundle::default());
        let memo = EscrowRecoveryMemo::default();
        unkeyable_generation(&mut f, &door, true).await;

        fn breaths_of(
            fut: impl std::future::Future<Output = Result<EscrowRecoveryPass>>,
        ) -> (usize, EscrowRecoveryPass) {
            let mut fut = std::pin::pin!(fut);
            let waker = std::task::Waker::noop();
            let mut cx = std::task::Context::from_waker(waker);
            let mut breaths = 0usize;
            loop {
                match fut.as_mut().poll(&mut cx) {
                    std::task::Poll::Ready(out) => return (breaths, out.unwrap()),
                    std::task::Poll::Pending => breaths += 1,
                }
            }
        }
        let (breaths, first) = breaths_of(pass(&f, &door, &bundle, &ESCROW_SEED, &memo));
        assert_eq!(first, EscrowRecoveryPass::Recovered(1));
        assert_eq!(breaths, 1, "one generation considered, one breath");
        let (breaths, again) = breaths_of(pass(&f, &door, &bundle, &ESCROW_SEED, &memo));
        assert_eq!(again, EscrowRecoveryPass::Current);
        assert_eq!(breaths, 1, "considered and skipped, still one breath");
    }

    /// A mint row with no trusted holder's receipt — the shape of every forged
    /// row — draws no request at all.
    #[tokio::test]
    async fn a_generation_without_a_trusted_receipt_is_never_asked_for() {
        let mut f = fixture().await;
        let (door, bundle) = (Door::default(), Bundle::default());
        let memo = EscrowRecoveryMemo::default();
        unkeyable_generation(&mut f, &door, false).await;

        let p = pass(&f, &door, &bundle, &ESCROW_SEED, &memo).await.unwrap();
        assert_eq!(p, EscrowRecoveryPass::Current);
        assert!(door.asked.lock().unwrap().is_empty());
    }

    /// An unreachable holder is not an answer: the pass errors, memoes
    /// nothing, and the next pass asks again and recovers.
    #[tokio::test]
    async fn an_unreachable_holder_is_asked_again_next_pass() {
        let mut f = fixture().await;
        let (door, bundle) = (Door::default(), Bundle::default());
        let memo = EscrowRecoveryMemo::default();
        let g = unkeyable_generation(&mut f, &door, true).await;

        *door.unreachable.lock().unwrap() = true;
        assert!(pass(&f, &door, &bundle, &ESCROW_SEED, &memo).await.is_err());
        *door.unreachable.lock().unwrap() = false;
        let p = pass(&f, &door, &bundle, &ESCROW_SEED, &memo).await.unwrap();
        assert_eq!(p, EscrowRecoveryPass::Recovered(1));
        assert!(bundle.retained_generation_key(&g).is_some());
    }

    /// **A substituted wrap keys nothing.** The escrow target's public key and
    /// the generation's `(id, target key)` binding are both public, so the
    /// holder can seal ANY key under the generation's own binding to this
    /// identity's escrow target — a wrap that opens. The door's commitment
    /// check is what refuses it: nothing lands on the bundle, and the reader
    /// still keys nothing. Red-verified: with the door's commitment compare
    /// gone the forged key is recorded and served.
    #[tokio::test]
    async fn a_substituted_wrap_under_the_generations_own_binding_keys_nothing() {
        let mut f = fixture().await;
        let (door, bundle) = (Door::default(), Bundle::default());
        let memo = EscrowRecoveryMemo::default();
        let g = unkeyable_generation(&mut f, &door, true).await;
        let forged = seal_generation_key_to_escrow(
            &GenerationKey::from_bytes([0x42u8; 32]),
            &escrow_target(),
            &g,
            &target_key(),
        )
        .unwrap();
        *door.wraps.lock().unwrap() = vec![(g, forged)];

        let p = pass(&f, &door, &bundle, &ESCROW_SEED, &memo).await.unwrap();
        assert_eq!(p, EscrowRecoveryPass::Current);
        assert!(bundle.retained_generation_key(&g).is_none());
        assert!(
            generation_tip::generation_key_for(&f.store, &g, &f.writer_key, Some(&bundle), None)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(*door.asked.lock().unwrap(), vec![g], "asked, and answered");
    }

    /// A mint row keyed G whose core derives another id — a squatted row,
    /// which only a fleet writer can plant — draws no escrow request even
    /// when a trusted holder acked G. Red-verified: without the pass's own
    /// Key↔id compare the pass asks the holder for G.
    #[tokio::test]
    async fn a_squatted_mint_row_draws_no_escrow_request() {
        let mut f = fixture().await;
        let (door, bundle) = (Door::default(), Bundle::default());
        let memo = EscrowRecoveryMemo::default();
        let g = unkeyable_generation(&mut f, &door, true).await;
        let other = build_mint(
            &[member_of(THEM)],
            &escrow_target(),
            &target_key(),
            Vec::new(),
            &device_key(THEM),
            8_000,
        )
        .expect("mint");
        assert_ne!(other.generation_id, g);
        f.put(machinery_row(
            KIND_GENERATION_MINT,
            fauna_core::hex32::encode(&g),
            &other.record,
        ))
        .await;

        let p = pass(&f, &door, &bundle, &ESCROW_SEED, &memo).await.unwrap();
        assert_eq!(p, EscrowRecoveryPass::Current);
        assert!(door.asked.lock().unwrap().is_empty());
        assert!(bundle.retained_generation_key(&g).is_none());
    }

    /// A wrap that does not open under this identity's escrow secret (another
    /// seed's target — the predecessor-targeted shape) keys nothing, and the
    /// holder's answer is remembered: no second request this runtime life.
    #[tokio::test]
    async fn a_wrap_that_does_not_open_is_an_answer_and_is_not_asked_for_again() {
        let mut f = fixture().await;
        let (door, bundle) = (Door::default(), Bundle::default());
        let memo = EscrowRecoveryMemo::default();
        let g = unkeyable_generation(&mut f, &door, true).await;

        for _ in 0..2 {
            let p = pass(&f, &door, &bundle, &[0x99u8; 32], &memo)
                .await
                .unwrap();
            assert_eq!(p, EscrowRecoveryPass::Current);
        }
        assert!(bundle.retained_generation_key(&g).is_none());
        assert_eq!(door.asked.lock().unwrap().len(), 1);
    }
}
