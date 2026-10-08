//! The **re-escrow pass** — the generation axis's succession rider
//! (`owner-key-material.md` § Path A-sibling-2 → *Rotation*, the succession
//! rider; `config-dissolution.md` § The `__config` dissolution schedule →
//! *The closure order*, step (4)), stated as the invariant it enforces on
//! every pass: **every live generation this device keys is escrowed to the
//! identity this runtime IS.** It deposits, and it never mints.
//!
//! # The state this heals
//!
//! An identity succession burns the predecessor's escrow wraps at the home
//! nest inside the ceremony's own transaction (`generation_escrow_wraps` is
//! `Succession::Burn`: those wraps seal to a target the RETIRED seed derives —
//! thief-readable, successor-unopenable), so from that instant no escrow copy
//! of any pre-succession generation exists anywhere. The keys survive only on
//! devices: a successor device's fresh slot inherits them from the
//! predecessor's slot on the same machine
//! (`PrincipalSlot::carry_predecessor_generation_keys`), and the fleet walk
//! carries the mint record of each generation it so keys into the successor's
//! merged state (`AccountStatePlane::with_predecessor_mint_keys`) — the row
//! this pass, like every pass that moves a generation key, iterates. Until
//! some device
//! deposits them again — sealed to the SUCCESSOR's published target, receipted
//! under the successor's target key — a fresh successor device holding only
//! the seed can recover nothing, and every `GenerationTip`-sealed row it
//! inherited is dark there. The blob rail's `BackupKey`-only floor was the one
//! thing standing in for this; the rail's retirement took that floor, so
//! this pass is what keeps a succession loss-free (closure order step (4)).
//!
//! # What deposits when
//!
//! Once per full pump pass, after the escrow-recovery pass (a key recovered
//! this pass is re-escrowed this pass) and before the top-up pass, per live
//! canonical `Minted` row whose Key↔id binding holds:
//!
//! - **Acked for this identity** (a trusted holder's receipt whose signed
//!   `target_key` is this identity's — `escrow_acked_generations` under the
//!   current key): nothing — unless this is a **holdings-checking pass** (the
//!   first after every assembly and every bind verification,
//!   `bind_leg`) and the holder's own answer lacks its wrap: then one deposit,
//!   and the receipt row already in merged state stands (a receipt proves a
//!   deposit, not a holding — a rebuilt box presents the identity that signed
//!   it and holds nothing). A predecessor's receipt does not count: its wrap
//!   is burned.
//! - **No target row for this identity yet**: nothing this pass —
//!   `fleet_bootstrap` publishes it at every seed-holding assembly, so the
//!   next pass finds it.
//! - **This device cannot key it**: nothing — a sibling that can will, and
//!   the top-up pass hands the key here in the meantime.
//! - **Otherwise**: ONE deposit — the key sealed to the current target under
//!   the current target key (`seal_generation_key_to_escrow`), the receipt
//!   verified (integrity, trusted holder, generation, wrap hash, target key)
//!   and written at the per-identity receipt cell
//!   (`escrow_receipt_cell_key`), which is the durable "done" the next pass
//!   reads. A door failure is witnessed and retried next pass.
//!
//! **The pass never mints — trigger (d) is no mint of its own.** A generation
//! that needed re-escrow here was escrowed either to a predecessor's target
//! (a succession) or to this identity's target at a holder this device no
//! longer trusts (a second nest, a rotated nest — `account-data-taxonomy.md`
//! § The generation machinery → *A holder change re-receipts and never
//! mints*). Either way the deposit is the whole of this pass's answer. After
//! a holder change the deposit restores the tip. After a succession the
//! carried generation is no sealing candidate under the successor at all —
//! its member set names the predecessor's device ids, which the successor's
//! device set never holds — so the first tip-sealed origination finds no
//! candidate and mints by trigger (a) at the writer door, naming the carried
//! generation among its parents (the rider → *Forward sealing*). Nothing
//! mints eagerly at a succession.
//!
//! # Why a pump pass and not an aftermath leg
//!
//! The aftermath runs once, on the ceremony device, before any runtime is up;
//! the keys it would need live in the successor slot only after that
//! runtime's assembly carried them, and any *other* successor device that
//! holds keys owes the same deposit. An invariant enforced on every pass by
//! every seed-holding runtime is the shape that needs no coordinator and
//! survives a crash between the burn and the deposit.

use std::collections::BTreeSet;

use anyhow::{Context, Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use fauna_account_store::{backend::StoreBackend, store::AccountStore};
use fauna_core::generation::{
    EscrowReceiptRecord, EscrowTargetRecord, GenerationMintRecord, escrow_acked_generations,
    escrow_receipt_cell_key, escrow_receipted_generations, escrow_target_identity_key,
    verify_escrow_receipt,
};
use fauna_mls::wrapped_blob::generation_wraps::seal_generation_key_to_escrow;
use fauna_protocol::generation_escrow::{EscrowPutReply, EscrowPutRequest, KIND_ESCROW_PUT};
use fauna_protocol::merge_policy::{KIND_ESCROW_RECEIPT, KIND_ESCROW_TARGET, KIND_GENERATION_MINT};
use fauna_protocol::{ByteBuf, RpcRequester};

use crate::account_state_plane::{AccountStatePlane, ItemId};
use crate::generation_store::{live_rows, row_ref};
use crate::generation_tip::{self, GenerationTrust};

/// What one re-escrow pass did (the pump's `generation_reescrow` slot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReescrowPass {
    /// Nothing to deposit: every live generation this device keys is already
    /// acked for this identity (and, on a holdings-checking pass, held), or
    /// no target row for this identity is published yet.
    Current,
    /// This many generations were deposited under this identity's target;
    /// `restored` of them were already acked and only lacked a wrap at the
    /// holder (the holdings check — no receipt row is rewritten for those).
    Reescrowed { deposited: usize, restored: usize },
}

/// Deposit, under this identity's target, every live generation this device
/// keys that no trusted holder has receipted for this identity — and, when
/// `holdings` names what the holder actually holds (the holdings check,
/// [`crate::bind_leg::holder_holdings`]), every acked one the holder lacks.
/// Deposits only: the pass never mints (module docs own the decision table
/// and why).
///
/// # Errors
///
/// Store I/O, a receipt that fails verification or binding (never acted on),
/// or the **last** door failure of the pass — every generation is still
/// attempted, and a failed one is deposited again next pass.
pub async fn ensure_reescrowed<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    trust: &GenerationTrust,
    writer_key: &SigningKey,
    holdings: Option<&BTreeSet<[u8; 32]>>,
) -> Result<ReescrowPass>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let holder = Holder::bound(fleet, trust);
    let Some(deposits) = deposit_owed(store, fleet, &holder, trust, writer_key, holdings).await?
    else {
        return Ok(ReescrowPass::Current);
    };
    let (deposited, restored) = (deposits.deposited(), deposits.restored);
    if let Some(e) = deposits.door_failure {
        if deposited > 0 {
            tracing::info!(
                deposited,
                restored,
                "generation re-escrow: deposited under this identity (a door failure remains)"
            );
        }
        return Err(e);
    }
    if deposited > 0 {
        tracing::info!(
            deposited,
            restored,
            "generation re-escrow: deposited under this identity"
        );
    }
    Ok(deposits.pass())
}

/// The same deposit at a **linked nest** — a holder too
/// (`account-data-taxonomy.md` § The generation machinery → *A holder change
/// re-receipts and never mints*, the linked-holder clause;
/// `account-sync-plane.md` § The bind leg, ruling 4): every generation this
/// device keys that `holder_id` has not receipted for this identity — and,
/// with `holdings`, every one it receipted but no longer holds — is deposited
/// over `linked`, the receipt verified against `holder_id` (the pairing row's
/// nest id, whose channel binding the secondary leg checked) and written in
/// that holder's own receipt cell through the bound `fleet` plane. Such a
/// receipt acks no tip — sealing reads the bound nest's pin alone. Like
/// [`ensure_reescrowed`] it is [`deposit_owed`] alone.
///
/// # Errors
///
/// As [`ensure_reescrowed`].
pub async fn ensure_deposited_at_linked<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    linked: &R,
    holder_id: [u8; 32],
    trust: &GenerationTrust,
    writer_key: &SigningKey,
    holdings: Option<&BTreeSet<[u8; 32]>>,
) -> Result<ReescrowPass>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let holder = Holder {
        rpc: linked,
        trusted: vec![holder_id],
    };
    let Some(deposits) = deposit_owed(store, fleet, &holder, trust, writer_key, holdings).await?
    else {
        return Ok(ReescrowPass::Current);
    };
    let pass = deposits.pass();
    if let Some(e) = deposits.door_failure {
        return Err(e.context("the deposit at a linked holder"));
    }
    Ok(pass)
}

/// The holder one deposit sweep deposits at.
struct Holder<'h, R> {
    /// The connection its escrow door answers on.
    rpc: &'h R,
    /// Whose receipts ack a generation here, and the one identity a receipt
    /// this sweep earns must be signed by.
    trusted: Vec<[u8; 32]>,
}

impl<'h, R: RpcRequester> Holder<'h, R> {
    /// The bound nest's holder: the plane's own requester, trusted as the
    /// pin says now.
    fn bound<B: StoreBackend>(
        fleet: &AccountStatePlane<'h, B, R>,
        trust: &GenerationTrust,
    ) -> Self {
        Self {
            rpc: fleet.requester(),
            trusted: trust.trusted_holders.get(),
        }
    }
}

/// What one sweep of deposits did.
struct Deposits {
    /// Generations whose fresh receipt row this sweep wrote.
    reescrowed: BTreeSet<[u8; 32]>,
    /// Acked generations the holder lacked, deposited again under their
    /// standing receipt (the holdings check).
    restored: usize,
    /// The last door failure — every generation is still attempted.
    door_failure: Option<anyhow::Error>,
}

impl Deposits {
    fn deposited(&self) -> usize {
        self.reescrowed.len() + self.restored
    }

    /// The sweep as the pump's report slot states it.
    fn pass(&self) -> ReescrowPass {
        match self.deposited() {
            0 => ReescrowPass::Current,
            deposited => ReescrowPass::Reescrowed {
                deposited,
                restored: self.restored,
            },
        }
    }
}

/// Deposit every generation owed under this identity's target (module docs,
/// *What deposits when*), and never mint. `None` when no target row for this
/// identity is published yet — nothing to seal to.
async fn deposit_owed<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    holder: &Holder<'_, R>,
    trust: &GenerationTrust,
    writer_key: &SigningKey,
    holdings: Option<&BTreeSet<[u8; 32]>>,
) -> Result<Option<Deposits>>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let target_key = escrow_target_identity_key(&trust.root);
    // No target row for THIS identity → nothing to seal to. `fleet_bootstrap`
    // publishes it at the next seed-holding assembly; a predecessor's row at
    // its own key is deliberately not consulted.
    let Some(target_entry) = store.state(KIND_ESCROW_TARGET, &target_key).await? else {
        return Ok(None);
    };
    if target_entry.tombstone {
        return Ok(None);
    }
    let target: EscrowTargetRecord = fauna_core::encoding::canonical_decode(&target_entry.value)
        .context("this identity's escrow-target row does not decode")?;

    let receipt_rows = live_rows(store, KIND_ESCROW_RECEIPT).await?;
    let acked = escrow_acked_generations(
        receipt_rows.iter().map(row_ref),
        &holder.trusted,
        &target_key,
        // The resolver owns reporting invalid receipts; here they just ack
        // nothing.
        &mut Vec::new(),
    );
    let custody = fleet.generation_custody();
    // Built once for the pass: the reader consults it for a shred's
    // authorship alone (`generation_tip::generation_key_for`).
    let view = crate::fleet_removal::fleet_view(store, trust).await?;
    let mut reescrowed = BTreeSet::new();
    let mut restored = 0usize;
    let mut door_failure = None;

    for entry in live_rows(store, KIND_GENERATION_MINT).await? {
        // One generation is one unit of local work — a key lookup, a KEM
        // encapsulation, a door round trip (`pass_breath` module docs).
        crate::pass_breath::pass_breath().await;
        let Ok(generation_id) = fauna_core::hex32::decode(&entry.key) else {
            continue;
        };
        if fauna_core::hex32::encode(&generation_id) != entry.key {
            continue;
        }
        // Acked and — when this pass checks holdings — held: nothing owed. An
        // acked generation the holder no longer holds (a rebuilt or restored
        // box presenting the identity that signed the receipt) is owed a
        // wrap, and its receipt row stands.
        let acked_here = acked.contains(&generation_id);
        if acked_here && holdings.is_none_or(|held| held.contains(&generation_id)) {
            continue;
        }
        let Ok(GenerationMintRecord::Minted { core, .. }) =
            fauna_core::encoding::canonical_decode::<GenerationMintRecord>(&entry.value)
        else {
            // Undecodable, or Shredded — a shred is a deletion, never re-escrowed.
            continue;
        };
        // Canonical-or-skip, then the Key↔id binding — a squatted row keys
        // nothing and escrows nothing (the recovery pass's own gates).
        if fauna_core::generation::generation_id(&core).ok() != Some(generation_id) {
            continue;
        }
        let Some(key) = generation_tip::generation_key_for(
            store,
            &generation_id,
            writer_key,
            custody,
            Some(&view),
        )
        .await?
        else {
            continue;
        };

        let wrap = seal_generation_key_to_escrow(&key, &target, &generation_id, &target_key)
            .map_err(|e| {
                anyhow!(
                    "re-escrow of generation {}: sealing to this identity's target failed: {e}",
                    entry.key
                )
            })?;
        let reply: EscrowPutReply = match holder
            .rpc
            .request(
                KIND_ESCROW_PUT,
                EscrowPutRequest {
                    generation_id: ByteBuf::from(generation_id.to_vec()),
                    wrap: ByteBuf::from(wrap.clone()),
                    target_key: target_key.clone(),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(reply) => reply,
            Err(e) => {
                door_failure = Some(anyhow!(
                    "re-escrow of generation {}: the holder could not be asked: {e}",
                    entry.key
                ));
                continue;
            }
        };

        // The receipt: integrity (holder-generic), trust (this account's
        // holder set), binding (this generation, this wrap, THIS identity's
        // target) — the mint sequence's own checks, so a re-escrow is acked
        // on exactly the evidence a first deposit is.
        let receipt: EscrowReceiptRecord = fauna_core::encoding::canonical_decode(&reply.receipt)
            .with_context(|| {
            format!(
                "re-escrow of generation {}: the holder's receipt does not decode",
                entry.key
            )
        })?;
        verify_escrow_receipt(&receipt).map_err(|e| {
            anyhow!(
                "re-escrow of generation {}: the holder's receipt fails verification: {e}",
                entry.key
            )
        })?;
        if !holder.trusted.contains(&receipt.holder_id) {
            // The one holder this pass deposits at IS the trusted one; a
            // receipt from anyone else is a nest that is not the pinned one.
            bail!(
                "re-escrow of generation {}: the receipt is signed by a holder this account does \
                 not trust — refusing to treat the deposit as escrow-acked",
                entry.key
            );
        }
        if receipt.generation_id != generation_id
            || receipt.wrap_hash != <[u8; 32]>::from(blake3::hash(&wrap))
            || receipt.target_key != target_key
        {
            bail!(
                "re-escrow of generation {}: the receipt does not bind this deposit (generation, \
                 wrap or target differ) — refusing an ack that could count for another identity",
                entry.key
            );
        }
        if acked_here {
            // The holdings restore: the wrap is back at the holder; the
            // receipt row in merged state already acks it and is immutable.
            restored += 1;
            continue;
        }
        fleet
            .put(
                &ItemId {
                    kind: KIND_ESCROW_RECEIPT.into(),
                    key: escrow_receipt_cell_key(&generation_id, &receipt.holder_id, &trust.root),
                },
                fauna_core::encoding::canonical_encode(&receipt)?,
                None,
            )
            .await
            .with_context(|| {
                format!(
                    "re-escrow of generation {}: publishing the receipt row",
                    entry.key
                )
            })?;
        reescrowed.insert(generation_id);
    }
    Ok(Some(Deposits {
        reescrowed,
        restored,
        door_failure,
    }))
}

/// The writer door's half of *A holder change re-receipts and never mints*
/// (`account-data-taxonomy.md` § The generation machinery, (2)), asked by
/// [`AccountStatePlane`]'s first-need trigger before it mints. When merged
/// state holds a receipt for this identity's target that no trusted holder
/// gave — the pin moved (a second nest, a rotated nest) and no re-escrow pass
/// has re-receipted it yet: a pass cut or ended early after its pin re-read,
/// or a door failure there — the answer is the re-escrow's own deposits, made
/// here, and never a mint.
///
/// `Ok(true)` when such a receipt existed and deposits landed (the caller
/// re-resolves); `Ok(false)` when no holder change is owed, which leaves the
/// ordinary first-need mint to the caller. An error is a deposit the holder
/// change owed and could not make: the caller refuses rather than mint.
pub(crate) async fn reescrow_owed_at_the_door<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    trust: &GenerationTrust,
    writer_key: &SigningKey,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let target_key = escrow_target_identity_key(&trust.root);
    let receipt_rows = live_rows(store, KIND_ESCROW_RECEIPT).await?;
    let acked = escrow_acked_generations(
        receipt_rows.iter().map(row_ref),
        &trust.trusted_holders.get(),
        &target_key,
        &mut Vec::new(),
    );
    let receipted = escrow_receipted_generations(receipt_rows.iter().map(row_ref), &target_key);
    if receipted.is_subset(&acked) {
        return Ok(false);
    }
    let holder = Holder::bound(fleet, trust);
    let Some(deposits) = deposit_owed(store, fleet, &holder, trust, writer_key, None).await? else {
        return Ok(false);
    };
    if let Some(e) = deposits.door_failure {
        return Err(e.context("the moved holder's re-escrow at the writer door"));
    }
    Ok(!deposits.reescrowed.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation_fixture_test_support::{
        Bundle, ESCROW_SEED, Fixture, THEM, US, device_key, enrollment_row, fixture, machinery_row,
        member_of, target_key,
    };
    use fauna_core::crypto::GenerationKey;
    use fauna_core::generation::{derive_escrow_xwing_keypair, sign_escrow_receipt};
    use fauna_mls::wrapped_blob::generation_wraps::{build_mint, open_generation_key_from_escrow};
    use fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE;
    use fauna_protocol::{decode_strict, encode_canonical};
    use std::sync::{Arc, Mutex};

    fn holder_key() -> SigningKey {
        SigningKey::from_bytes(&[0x66u8; 32])
    }

    /// The escrow-target key a PREDECESSOR identity published under — what a
    /// pre-succession receipt names.
    const PREDECESSOR_KEY: &str =
        "identity/0000000000000000000000000000000000000000000000000000000000000000";

    /// `(generation id, wrap ciphertext, target key)` as the holder stores them.
    type Deposits = Vec<([u8; 32], Vec<u8>, String)>;

    /// The holder's `put` door: stores what it is handed, signs a receipt
    /// naming the target key the request carried (or a lie, when told to),
    /// and can be told to be unreachable.
    #[derive(Clone, Default)]
    struct Door {
        deposits: Arc<Mutex<Deposits>>,
        unreachable: Arc<Mutex<bool>>,
        lie_about_target: Arc<Mutex<bool>>,
        /// Sign receipts as this key instead of [`holder_key`] — a holder the
        /// pin moved to.
        sign_as: Arc<Mutex<Option<SigningKey>>>,
    }

    impl RpcRequester for Door {
        type Error = anyhow::Error;

        async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            assert_eq!(kind, KIND_ESCROW_PUT, "the pass calls exactly one door");
            if *self.unreachable.lock().unwrap() {
                anyhow::bail!("holder unreachable");
            }
            let req: EscrowPutRequest =
                decode_strict(&encode_canonical(&payload).unwrap()).unwrap();
            let generation: [u8; 32] = req.generation_id.as_slice().try_into().unwrap();
            self.deposits.lock().unwrap().push((
                generation,
                req.wrap.to_vec(),
                req.target_key.clone(),
            ));
            let named = if *self.lie_about_target.lock().unwrap() {
                PREDECESSOR_KEY.to_string()
            } else {
                req.target_key.clone()
            };
            let signer = self
                .sign_as
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(holder_key);
            let receipt = sign_escrow_receipt(
                &signer,
                generation,
                blake3::hash(&req.wrap).into(),
                &named,
                7_000,
            );
            let reply = EscrowPutReply {
                receipt: ByteBuf::from(encode_canonical(&receipt).unwrap().to_vec()),
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

    /// Publish this identity's target row at its own key.
    async fn publish_target(f: &Fixture) {
        f.put(machinery_row(
            KIND_ESCROW_TARGET,
            target_key(),
            &escrow_target(),
        ))
        .await;
    }

    /// A generation minted by `minter` over `members`, acked by the trusted
    /// holder under `receipt_key` (a predecessor's key, or this identity's).
    async fn acked_generation(
        f: &mut Fixture,
        minter: [u8; 32],
        members: &[[u8; 32]],
        receipt_key: &str,
    ) -> ([u8; 32], GenerationKey) {
        f.trust.trusted_holders = vec![holder_key().verifying_key().to_bytes()].into();
        let fleet: Vec<_> = members.iter().map(|m| member_of(*m)).collect();
        let built = build_mint(
            &fleet,
            &escrow_target(),
            receipt_key,
            Vec::new(),
            &device_key(minter),
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
        let receipt = sign_escrow_receipt(
            &holder_key(),
            built.generation_id,
            blake3::hash(&built.escrow_wrap).into(),
            receipt_key,
            7_000,
        );
        f.put(machinery_row(
            KIND_ESCROW_RECEIPT,
            format!(
                "{id_hex}/{}/pre",
                fauna_core::hex32::encode(&receipt.holder_id)
            ),
            &receipt,
        ))
        .await;
        (built.generation_id, built.gen_key)
    }

    async fn pass(f: &Fixture, door: &Door, bundle: &Bundle) -> Result<ReescrowPass> {
        pass_checking(f, door, bundle, None).await
    }

    /// [`pass`], as a holdings-checking pass: `holdings` is what the holder
    /// answered it holds.
    async fn pass_checking(
        f: &Fixture,
        door: &Door,
        bundle: &Bundle,
        holdings: Option<&BTreeSet<[u8; 32]>>,
    ) -> Result<ReescrowPass> {
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
        ensure_reescrowed(&f.store, &plane, &f.trust, &f.writer_key, holdings).await
    }

    async fn acked_here(f: &Fixture, g: &[u8; 32]) -> bool {
        let rows = live_rows(&f.store, KIND_ESCROW_RECEIPT).await.unwrap();
        escrow_acked_generations(
            rows.iter().map(row_ref),
            &f.trust.trusted_holders.get(),
            &target_key(),
            &mut Vec::new(),
        )
        .contains(g)
    }

    /// The whole pass: a generation acked only under a predecessor's key,
    /// which this device keys (its inline wrap), is deposited ONCE under this
    /// identity's target — the wrap opens under this identity's key, the
    /// receipt row lands at the per-identity cell, and the next pass finds it
    /// acked and deposits nothing.
    #[tokio::test]
    async fn a_predecessor_acked_generation_this_device_keys_is_re_escrowed_once() {
        let mut f = fixture().await;
        let (door, bundle) = (Door::default(), Bundle::default());
        publish_target(&f).await;
        let (g, key) = acked_generation(&mut f, US, &[US], PREDECESSOR_KEY).await;
        assert!(
            !acked_here(&f, &g).await,
            "a predecessor's receipt acks nothing here"
        );

        let p = pass(&f, &door, &bundle).await.unwrap();
        assert_eq!(
            p,
            ReescrowPass::Reescrowed {
                deposited: 1,
                restored: 0
            }
        );
        let deposits = door.deposits.lock().unwrap().clone();
        assert_eq!(deposits.len(), 1);
        let (dep_g, dep_wrap, dep_key) = &deposits[0];
        assert_eq!(*dep_g, g);
        assert_eq!(*dep_key, target_key(), "sealed under THIS identity's key");
        let opened = open_generation_key_from_escrow(
            dep_wrap,
            &derive_escrow_xwing_keypair(&ESCROW_SEED).secret,
            &g,
            &target_key(),
            &key.commitment(),
        )
        .expect("this identity's escrow secret opens the re-deposit");
        assert_eq!(opened.as_bytes(), key.as_bytes());
        assert!(
            open_generation_key_from_escrow(
                dep_wrap,
                &derive_escrow_xwing_keypair(&ESCROW_SEED).secret,
                &g,
                PREDECESSOR_KEY,
                &key.commitment(),
            )
            .is_err(),
            "the wrap binds the identity: it does not open as the predecessor's"
        );
        assert!(
            acked_here(&f, &g).await,
            "the receipt row acks it for this identity"
        );
        let cell =
            escrow_receipt_cell_key(&g, &holder_key().verifying_key().to_bytes(), &f.trust.root);
        assert!(
            f.store
                .state(KIND_ESCROW_RECEIPT, &cell)
                .await
                .unwrap()
                .is_some()
        );

        // Idempotent: the next pass deposits nothing.
        let p = pass(&f, &door, &bundle).await.unwrap();
        assert_eq!(p, ReescrowPass::Current);
        assert_eq!(door.deposits.lock().unwrap().len(), 1);
    }

    /// A generation already acked under this identity's key is left alone.
    #[tokio::test]
    async fn a_generation_acked_for_this_identity_is_left_alone() {
        let mut f = fixture().await;
        let (door, bundle) = (Door::default(), Bundle::default());
        publish_target(&f).await;
        acked_generation(&mut f, US, &[US], &target_key()).await;

        let p = pass(&f, &door, &bundle).await.unwrap();
        assert_eq!(p, ReescrowPass::Current);
        assert!(door.deposits.lock().unwrap().is_empty());
    }

    /// No target row for this identity yet → nothing to seal to, nothing
    /// deposited; `fleet_bootstrap` publishes the row, the next pass acts.
    #[tokio::test]
    async fn no_published_target_for_this_identity_means_no_deposit() {
        let mut f = fixture().await;
        let (door, bundle) = (Door::default(), Bundle::default());
        acked_generation(&mut f, US, &[US], PREDECESSOR_KEY).await;

        let p = pass(&f, &door, &bundle).await.unwrap();
        assert_eq!(p, ReescrowPass::Current);
        assert!(door.deposits.lock().unwrap().is_empty());
    }

    /// A generation this device cannot key (minted by THEM over THEM alone)
    /// is a sibling's to re-escrow — no deposit, no error.
    #[tokio::test]
    async fn a_generation_this_device_cannot_key_is_a_siblings_to_re_escrow() {
        let mut f = fixture().await;
        let (door, bundle) = (Door::default(), Bundle::default());
        publish_target(&f).await;
        acked_generation(&mut f, THEM, &[THEM], PREDECESSOR_KEY).await;

        let p = pass(&f, &door, &bundle).await.unwrap();
        assert_eq!(p, ReescrowPass::Current);
        assert!(door.deposits.lock().unwrap().is_empty());
    }

    /// A key held only on the retained bundle (the succession carriage's
    /// shape — no wrap on the plane reaches this device) is re-escrowed too.
    #[tokio::test]
    async fn a_bundle_held_key_is_re_escrowed() {
        let mut f = fixture().await;
        let (door, bundle) = (Door::default(), Bundle::default());
        publish_target(&f).await;
        let (g, key) = acked_generation(&mut f, THEM, &[THEM], PREDECESSOR_KEY).await;
        crate::generation_tip::RetainedKeyCustody::record_generation_key(&bundle, &g, &key);

        let p = pass(&f, &door, &bundle).await.unwrap();
        assert_eq!(
            p,
            ReescrowPass::Reescrowed {
                deposited: 1,
                restored: 0
            }
        );
        assert!(acked_here(&f, &g).await);
    }

    /// An unreachable holder is witnessed as the pass's error and the deposit
    /// is retried next pass — nothing is acked meanwhile.
    #[tokio::test]
    async fn an_unreachable_holder_is_retried_next_pass() {
        let mut f = fixture().await;
        let (door, bundle) = (Door::default(), Bundle::default());
        publish_target(&f).await;
        let (g, _) = acked_generation(&mut f, US, &[US], PREDECESSOR_KEY).await;

        *door.unreachable.lock().unwrap() = true;
        assert!(pass(&f, &door, &bundle).await.is_err());
        assert!(!acked_here(&f, &g).await);

        *door.unreachable.lock().unwrap() = false;
        let p = pass(&f, &door, &bundle).await.unwrap();
        assert_eq!(
            p,
            ReescrowPass::Reescrowed {
                deposited: 1,
                restored: 0
            }
        );
        assert!(acked_here(&f, &g).await);
    }

    /// A holder whose receipt names another identity's target is refused:
    /// nothing is acked, and the pass says why. Red-verified: without the
    /// target-key compare the lying receipt lands and acks for this identity
    /// a wrap the successor could open but the receipt attributes elsewhere.
    #[tokio::test]
    async fn a_receipt_naming_another_target_is_refused() {
        let mut f = fixture().await;
        let (door, bundle) = (Door::default(), Bundle::default());
        publish_target(&f).await;
        let (g, _) = acked_generation(&mut f, US, &[US], PREDECESSOR_KEY).await;
        *door.lie_about_target.lock().unwrap() = true;

        let err = pass(&f, &door, &bundle).await.unwrap_err();
        assert!(
            err.to_string().contains("does not bind this deposit"),
            "unexpected error: {err:#}"
        );
        assert!(!acked_here(&f, &g).await);
    }

    /// The holdings check (`account-data-taxonomy.md` § The generation
    /// machinery → *A holder change re-receipts and never mints*, (3)): a
    /// generation acked for this identity whose wrap the holder does not hold
    /// — a rebuilt box presenting the identity that signed the receipt — is
    /// deposited again, and the receipt row already in merged state is the
    /// only one: nothing is written at the per-identity cell. Red-verified by
    /// skipping every acked generation regardless of `holdings`.
    #[tokio::test]
    async fn an_acked_generation_the_holder_no_longer_holds_is_deposited_again_under_its_receipt() {
        let mut f = fixture().await;
        let (door, bundle) = (Door::default(), Bundle::default());
        publish_target(&f).await;
        let (g, key) = acked_generation(&mut f, US, &[US], &target_key()).await;
        let receipts_before = live_rows(&f.store, KIND_ESCROW_RECEIPT).await.unwrap();

        let p = pass_checking(&f, &door, &bundle, Some(&BTreeSet::new()))
            .await
            .unwrap();
        assert_eq!(
            p,
            ReescrowPass::Reescrowed {
                deposited: 1,
                restored: 1
            }
        );
        let deposits = door.deposits.lock().unwrap().clone();
        assert_eq!(deposits.len(), 1);
        assert_eq!(deposits[0].0, g);
        assert!(
            open_generation_key_from_escrow(
                &deposits[0].1,
                &derive_escrow_xwing_keypair(&ESCROW_SEED).secret,
                &g,
                &target_key(),
                &key.commitment(),
            )
            .is_ok(),
            "the holder holds an openable wrap again"
        );
        assert_eq!(
            live_rows(&f.store, KIND_ESCROW_RECEIPT).await.unwrap(),
            receipts_before,
            "the receipt row in merged state stands; none is rewritten"
        );

        // Held → nothing owed, on a checking pass as on any other.
        let p = pass_checking(&f, &door, &bundle, Some(&BTreeSet::from([g])))
            .await
            .unwrap();
        assert_eq!(p, ReescrowPass::Current);
        assert_eq!(door.deposits.lock().unwrap().len(), 1);
    }

    /// An ordinary pass never asks about holdings: an acked generation is left
    /// alone whatever the holder holds.
    #[tokio::test]
    async fn a_pass_that_checks_no_holdings_leaves_an_acked_generation_alone() {
        let mut f = fixture().await;
        let (door, bundle) = (Door::default(), Bundle::default());
        publish_target(&f).await;
        acked_generation(&mut f, US, &[US], &target_key()).await;
        assert_eq!(
            pass_checking(&f, &door, &bundle, None).await.unwrap(),
            ReescrowPass::Current
        );
        assert!(door.deposits.lock().unwrap().is_empty());
    }

    /// A holder change (`account-data-taxonomy.md` § The generation machinery
    /// → *A holder change re-receipts and never mints*, (2)): a generation the
    /// OLD holder receipted for this identity acks nothing once the pin moves,
    /// so it is deposited at the new holder and the new receipt lands in that
    /// holder's own cell beside the old one — which stays.
    #[tokio::test]
    async fn a_moved_pin_re_receipts_a_generation_beside_the_old_holders_receipt() {
        let mut f = fixture().await;
        let (door, bundle) = (Door::default(), Bundle::default());
        publish_target(&f).await;
        let (g, _) = acked_generation(&mut f, US, &[US], &target_key()).await;
        let successor = SigningKey::from_bytes(&[0x67u8; 32]);
        f.trust
            .trusted_holders
            .replace(vec![successor.verifying_key().to_bytes()]);
        *door.sign_as.lock().unwrap() = Some(successor.clone());
        assert!(
            !acked_here(&f, &g).await,
            "the old holder's receipt acks nothing now"
        );

        let p = pass(&f, &door, &bundle).await.unwrap();
        assert_eq!(
            p,
            ReescrowPass::Reescrowed {
                deposited: 1,
                restored: 0
            }
        );
        assert!(acked_here(&f, &g).await);
        let cell =
            escrow_receipt_cell_key(&g, &successor.verifying_key().to_bytes(), &f.trust.root);
        assert!(
            f.store
                .state(KIND_ESCROW_RECEIPT, &cell)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            live_rows(&f.store, KIND_ESCROW_RECEIPT)
                .await
                .unwrap()
                .len(),
            2,
            "the old holder's receipt stands beside the new one"
        );
    }

    /// The writer door's holder-change test reads these receipts
    /// ([`reescrow_owed_at_the_door`]): any holder's receipt naming this
    /// identity's target counts, trusted or not; one naming another
    /// identity's does not.
    #[tokio::test]
    async fn the_holder_change_test_reads_every_holders_receipt_for_this_identity_only() {
        let mut f = fixture().await;
        let (g, _) = acked_generation(&mut f, US, &[US], &target_key()).await;
        let (h, _) = acked_generation(&mut f, THEM, &[THEM], PREDECESSOR_KEY).await;
        f.trust.trusted_holders.replace(Vec::new());
        let rows = live_rows(&f.store, KIND_ESCROW_RECEIPT).await.unwrap();
        let receipted = escrow_receipted_generations(rows.iter().map(row_ref), &target_key());
        assert!(
            receipted.contains(&g),
            "an untrusted holder's receipt counts"
        );
        assert!(
            !receipted.contains(&h),
            "another identity's receipt does not"
        );
    }

    /// [`Nest`]'s error — the nest leg's requester must classify.
    #[derive(Debug)]
    struct NestErr(anyhow::Error);
    impl std::fmt::Display for NestErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{:#}", self.0)
        }
    }
    impl fauna_protocol::RpcErrorClass for NestErr {
        fn is_rejection(&self) -> bool {
            false
        }
    }

    /// The nest leg over a [`Door`]: escrow deposits reach the holder's door,
    /// and every feed put is acked.
    struct Nest<'a>(&'a Door);

    impl RpcRequester for Nest<'_> {
        type Error = NestErr;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, NestErr>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            use fauna_protocol::account_state::{AccountStatePutReply, KIND_STATE_PUT};
            if kind == KIND_STATE_PUT {
                let reply = AccountStatePutReply {
                    seq: 1,
                    ..Default::default()
                };
                return Ok(decode_strict(&encode_canonical(&reply).unwrap()).unwrap());
            }
            self.0.request(kind, payload).await.map_err(NestErr)
        }
    }

    /// A generation this device keys, receipted for this identity by the old
    /// holder, and a pin since moved to a successor the door signs as — with
    /// no re-escrow pass run since: the state a pass cut, or ended early,
    /// after its pin re-read leaves behind.
    async fn moved_pin_before_any_reescrow(f: &mut Fixture, door: &Door) -> [u8; 32] {
        publish_target(f).await;
        f.put(enrollment_row(US)).await;
        let (g, _) = acked_generation(f, US, &[US], &target_key()).await;
        let successor = SigningKey::from_bytes(&[0x67u8; 32]);
        f.trust
            .trusted_holders
            .replace(vec![successor.verifying_key().to_bytes()]);
        *door.sign_as.lock().unwrap() = Some(successor);
        g
    }

    /// One tip-sealed write through the nest leg's writer door.
    async fn tip_sealed_write(f: &Fixture, door: &Door, bundle: &Bundle) -> Result<u64> {
        use fauna_core::group_generation::GroupReceptionKeyRecord;
        use fauna_protocol::merge_policy::KIND_GROUP_RECEPTION_KEY;
        let nest = Nest(door);
        let plane = AccountStatePlane::new(
            &f.store,
            &nest,
            &f.schedule,
            &f.writer_key,
            &f.trust,
            ACCOUNT_STATE_FLEET_SCOPE,
        )
        .unwrap()
        .with_generation_custody(bundle);
        assert!(
            plane
                .origination_mints(KIND_GROUP_RECEPTION_KEY)
                .await
                .unwrap(),
            "no tip resolves under the moved pin before the re-escrow"
        );
        let record = GroupReceptionKeyRecord::mint(1);
        plane
            .put(
                &ItemId {
                    kind: KIND_GROUP_RECEPTION_KEY.into(),
                    key: record.logical_key().unwrap(),
                },
                fauna_core::encoding::canonical_encode(&record).unwrap(),
                None,
            )
            .await
    }

    /// **A tip-sealed write after a pin move re-receipts at the writer door
    /// and never mints** (`account-data-taxonomy.md` § The generation
    /// machinery → *A holder change re-receipts and never mints*, (2)). The
    /// pin moved and no re-escrow pass has re-receipted the tip yet, so no tip
    /// resolves for the door: it deposits at the new holder instead of
    /// running the first-need mint, and the write lands under the one
    /// generation there is. Red-verified without the door's re-escrow: the
    /// write mints a second generation.
    #[tokio::test]
    async fn a_tip_sealed_write_after_a_pin_move_re_receipts_and_never_mints() {
        let mut f = fixture().await;
        let (door, bundle) = (Door::default(), Bundle::default());
        let g = moved_pin_before_any_reescrow(&mut f, &door).await;

        tip_sealed_write(&f, &door, &bundle)
            .await
            .expect("the write lands under the re-receipted tip");

        assert_eq!(
            live_rows(&f.store, KIND_GENERATION_MINT)
                .await
                .unwrap()
                .len(),
            1,
            "a holder change mints nothing"
        );
        assert!(acked_here(&f, &g).await, "re-receipted at the new holder");
        assert_eq!(door.deposits.lock().unwrap().len(), 1, "one deposit");
    }

    /// **The owed deposit failing refuses the write — it never falls back to
    /// a mint.** The holder cannot be reached, so the write answers the
    /// door's no-tip refusal, nothing is minted, and nothing is left behind
    /// (the door keeps no row for a refused `GenerationTip` origination).
    #[tokio::test]
    async fn a_pin_move_whose_deposit_fails_refuses_the_write_instead_of_minting() {
        let mut f = fixture().await;
        let (door, bundle) = (Door::default(), Bundle::default());
        moved_pin_before_any_reescrow(&mut f, &door).await;
        *door.unreachable.lock().unwrap() = true;

        tip_sealed_write(&f, &door, &bundle)
            .await
            .expect_err("the owed deposit could not be made");

        assert_eq!(
            live_rows(&f.store, KIND_GENERATION_MINT)
                .await
                .unwrap()
                .len(),
            1,
            "no mint in the deposit's place"
        );
    }
}
