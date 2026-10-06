//! The R14 (account-data-plane.md § The ratified decisions) **mint sequence** — R14 build step 5 (`account-data-plane.md`
//! § The generation machinery → *The mint protocol*): read merged device-set
//! state → refuse without a published escrow target → assemble the mint
//! (`fauna_mls::wrapped_blob::generation_wraps::build_mint`) → **deposit
//! escrow first** and verify the holder's receipt → hand back the two plane
//! rows to write (mint entry + escrow-receipt entry) and the minted key.
//!
//! Deposit-first is the crash-safety shape the charter ratifies: a crash
//! between deposit and publish leaves an acked-but-unpublished generation —
//! harmless (never sealed under; a re-mint supersedes; the holder's orphan
//! wrap is garbage-collectable by wrap hash). The inverse order would publish
//! a generation whose escrow never landed, exactly what escrow-before-first-
//! seal exists to prevent — so the receipt is **input** to the entries this
//! function returns, structurally.
//!
//! **This function returns the entries; it does not write them.** Since build
//! step 6 the writer door admits machinery kinds (`Gen0` epoch — the
//! sealing-epoch dispatch in `crate::account_state_plane`), so the caller
//! writes the returned entries through an ordinary fleet-scope
//! `AccountStatePlane::put`. Trigger wiring: first-need is built (the writer
//! door's `resolve_or_mint`), and so is succession (`crate::generation_reescrow`
//! mints past a predecessor-minted tip); observed removal and cadence remain,
//! acting on `crate::generation_tip::resolve_tip`, with the engine-singleton
//! (W5 (account-data-plane.md § Workstreams)) the preferred place to act.

use anyhow::{Context, Result, bail};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_core::crypto::GenerationKey;
use fauna_core::generation::{
    EscrowReceiptRecord, EscrowTargetRecord, FleetMember, FleetView, GenerationMintRecord,
    MAX_INLINE_MEMBER_WRAPS, MAX_MINT_MEMBERS, escrow_receipt_cell_key, escrow_target_identity_key,
    escrow_target_record, verify_escrow_receipt,
};
use fauna_core::identity::ActorId;
use fauna_mls::wrapped_blob::generation_wraps::build_mint;
use fauna_protocol::generation_escrow::{EscrowPutReply, EscrowPutRequest, KIND_ESCROW_PUT};
use fauna_protocol::merge_policy::{
    KIND_DEVICE_SET, KIND_ESCROW_RECEIPT, KIND_ESCROW_TARGET, KIND_GENERATION_MINT,
    home_scope_for_kind,
};
use fauna_protocol::{ByteBuf, RpcRequester};

/// The account identity line + this device's mint role — who is minting, for
/// which identity, trusting which escrow holders.
pub struct MintContext<'a> {
    /// The account's current identity (the device-set certs verify against
    /// this line — `FleetView::build`).
    pub root: &'a ActorId,
    /// The minting device's signing key — its public half is the minter id,
    /// which must be a verified, non-removed member of the merged device-set
    /// view ("any enrolled device may mint", and only one). The key, not just
    /// the id, because a mint is an authenticated statement: `build_mint`
    /// signs the content-derived generation id as the minter (ST-007).
    pub minter_key: &'a ed25519_dalek::SigningKey,
    /// Holder identities this account accepts receipts from — v1: the nest
    /// deployment identity the client already pins. `verify_escrow_receipt`
    /// checks integrity holder-generically; membership here is the *trust*
    /// half the shared contract deliberately leaves to the caller.
    pub trusted_holders: &'a [[u8; 32]],
}

/// One completed mint sequence: the escrow deposit is acked (receipt in
/// hand, verified), and these are the rows to put on the plane.
///
/// `Debug` is manual and **redacts the key** — the one field that must never
/// reach a log line.
pub struct MintedGeneration {
    /// The content-derived generation id.
    pub generation_id: [u8; 32],
    /// The `fauna.state.generation-mint` row (logical key = the id's hex).
    pub mint_entry: StateEntry,
    /// The `fauna.state.escrow-receipt` row (logical key =
    /// `escrow_receipt_cell_key` — `<generation-hex>/<holder-hex>/<actor-hex>`)
    /// — the row step 6's tip resolution checks for escrow-acked.
    pub receipt_entry: StateEntry,
    /// The verified receipt the entry carries, decoded.
    pub receipt: EscrowReceiptRecord,
    /// The minted key — the caller's generation-N schedule root
    /// (`FleetOnlySchedule::derive_for_generation`) and top-up wrap source.
    pub gen_key: GenerationKey,
    /// The bounded mint's **spill** (`build_mint` — charter § The mint
    /// protocol → *The bounded mint*): every listed member without an inline
    /// wrap, in id order. The caller writes one ordinary top-up per member
    /// from [`Self::gen_key`] — `generation_topup::put_heal`, the one
    /// production healer shape — right after the mint row and BEFORE the
    /// receipt row, so an escrow-acked mint is one whose every wrap already
    /// precedes it on the minter's log. Empty for a fleet of at most
    /// `MAX_INLINE_MEMBER_WRAPS` members.
    pub spilled: Vec<FleetMember>,
}

impl std::fmt::Debug for MintedGeneration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MintedGeneration")
            .field(
                "generation_id",
                &fauna_core::hex32::encode(&self.generation_id),
            )
            .field("receipt", &self.receipt)
            .field("gen_key", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// The `fauna.state.escrow-target` row a **seed-holding surface** publishes —
/// first onboarding for new accounts; any seed-holding session once, for
/// existing accounts, before their first mint — at the identity's OWN key
/// (`escrow_target_identity_key` of the actor the seed derives), so a
/// successor's runtime publishes its own row beside the predecessor's.
/// Deterministic from the seed (byte-identical across concurrent seed-holding
/// writers — the kind is registered Immutable on exactly that argument),
/// sealing into the fleet-only home scope like every machinery kind.
pub fn escrow_target_entry(identity_seed: &[u8; 32]) -> Result<StateEntry> {
    let actor_id = fauna_core::identity::ActorKeypair::from_secret(*identity_seed).actor_id();
    Ok(StateEntry {
        kind: KIND_ESCROW_TARGET.into(),
        key: escrow_target_identity_key(&actor_id),
        scope: machinery_scope(KIND_ESCROW_TARGET)?.into(),
        value: fauna_core::encoding::canonical_encode(&escrow_target_record(identity_seed))?,
        merge_meta: None,
        entry_version: 0,
        tombstone: false,
    })
}

/// Run one mint sequence against merged plane state and the escrow doors.
///
/// `parents` are the tip id(s) this mint supersedes — step 6's tip resolution
/// is their production source (an empty list is the first generation);
/// `now_ms` stamps the mint (advisory, like every machinery stamp).
///
/// # Errors
///
/// Precise refusals, in sequence order: no published escrow target (the
/// charter's "fleet-only sealing stays refused **and says why**"); a minter
/// that is not a verified member; an escrow door failure; a receipt that
/// fails integrity, names an untrusted holder, or binds to the wrong
/// (generation, wrap); a listed member removed while the deposit was in
/// flight. On any error **nothing has been staged anywhere** —
/// the only externally visible effect of a failed sequence is at most an
/// orphaned holder-side wrap, which is the ratified harmless state.
pub async fn mint_generation<B: StoreBackend, R: RpcRequester>(
    store: &AccountStore<B>,
    rpc: &R,
    ctx: &MintContext<'_>,
    parents: Vec<[u8; 32]>,
    now_ms: i64,
) -> Result<MintedGeneration> {
    // 1. The escrow target — THIS identity's row, without which no mint can
    //    escrow, so the refusal happens before any key material exists. A
    //    predecessor's row at its own key never satisfies a successor's mint.
    let target_key = escrow_target_identity_key(ctx.root);
    let target_entry = store.state(KIND_ESCROW_TARGET, &target_key).await?;
    let target: EscrowTargetRecord = match target_entry {
        Some(entry) if !entry.tombstone => fauna_core::encoding::canonical_decode(&entry.value)
            .context("the published escrow-target row does not decode")?,
        _ => bail!(
            "no escrow target is published for this account: a seed-holding surface must write \
             `{KIND_ESCROW_TARGET}` (key {target_key:?}) before the first mint — \
             until then fleet-only sealing stays refused \
             (account-data-taxonomy.md § The generation machinery)"
        ),
    };

    // 2. The verified fleet view over merged device-set rows — the wrap
    //    target set, and the minter's own admission.
    let rows = store.states_of_kind(KIND_DEVICE_SET).await?;
    let view = FleetView::build(
        ctx.root,
        rows.iter()
            .filter(|e| !e.tombstone)
            .map(|e| (e.key.as_str(), e.value.as_slice())),
    );
    let minter_device = ctx.minter_key.verifying_key().to_bytes();
    if !view.is_verified_member(&minter_device) {
        bail!(
            "the minting device is not a verified, non-removed member of the merged device-set \
             view — only enrolled fleet members mint"
        );
    }
    let members: Vec<_> = view.wrap_targets().cloned().collect();

    // 3. Assemble: fresh key, commitment, content-derived id, the minter's
    //    signature over it, every member's wrap, the escrow wrap.
    let built = build_mint(
        &members,
        &target,
        &target_key,
        parents,
        ctx.minter_key,
        now_ms,
    )
    .map_err(|e| anyhow::anyhow!("mint assembly failed: {e}"))?;

    // 3b. The plane's per-entry cap, BEFORE the deposit — the backstop behind
    //     the bounded mint. `build_mint` caps the inline wraps and refuses a
    //     fleet over the member ceiling, so this cannot fire while those two
    //     constants honour the arithmetic their tests pin; if it ever does,
    //     depositing anyway would orphan a holder-side wrap on every attempt
    //     for an entry that could still never publish.
    let mint_key = fauna_core::hex32::encode(&built.generation_id);
    let mint_value = fauna_core::encoding::canonical_encode(&built.record)?;
    crate::account_state_plane::SizedEntry::size(
        &fauna_core::account_entry_crypto::EntryPlaintext {
            kind: KIND_GENERATION_MINT.into(),
            key: mint_key.clone(),
            merge_meta: None,
            value: mint_value.clone().into(),
            tombstone: false,
        },
    )
    .with_context(|| {
        format!(
            "minting over {} enrolled members: even bounded to {MAX_INLINE_MEMBER_WRAPS} inline \
             wraps the mint entry does not fit the plane's per-entry cap — a build defect while \
             the {MAX_MINT_MEMBERS}-member ceiling holds — so the sequence refuses before the \
             escrow deposit (account-data-taxonomy.md § The generation machinery → The bounded \
             mint)",
            members.len()
        )
    })?;

    // 4. Deposit FIRST. The receipt is input to the entries below — the
    //    inverse order is unrepresentable here.
    let reply: EscrowPutReply = rpc
        .request(
            KIND_ESCROW_PUT,
            EscrowPutRequest {
                generation_id: ByteBuf::from(built.generation_id.to_vec()),
                wrap: ByteBuf::from(built.escrow_wrap.clone()),
                target_key: target_key.clone(),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("escrow deposit failed: {e}"))?;

    // 5. Verify the receipt: integrity (holder-generic), then trust (the
    //    caller's pinned holder set), then binding (this deposit exactly).
    let receipt: EscrowReceiptRecord = fauna_core::encoding::canonical_decode(&reply.receipt)
        .context("the holder's receipt does not decode")?;
    verify_escrow_receipt(&receipt)
        .map_err(|e| anyhow::anyhow!("the holder's receipt fails verification: {e}"))?;
    if !ctx.trusted_holders.contains(&receipt.holder_id) {
        bail!(
            "the escrow receipt is signed by a holder this account does not trust — refusing to \
             treat the deposit as escrow-acked"
        );
    }
    if receipt.generation_id != built.generation_id {
        bail!("the escrow receipt names a different generation than was deposited");
    }
    if receipt.wrap_hash != <[u8; 32]>::from(blake3::hash(&built.escrow_wrap)) {
        bail!("the escrow receipt binds a different wrap than was deposited");
    }
    if receipt.target_key != target_key {
        bail!(
            "the escrow receipt names a different escrow target than the deposit was sealed \
             under — refusing an ack that would count for another identity"
        );
    }

    // 5b. The member list was fixed at step 2, BEFORE the deposit's await —
    //     and the devices page's removal is a local command a running pass
    //     serves at exactly such an await. The list is
    //     inside the content-derived id, so a member removed meanwhile cannot
    //     be filtered out: the mint is refused instead. The deposit it leaves
    //     is the orphaned holder-side wrap this function's contract already
    //     calls harmless, and the next origination re-mints over the fresh
    //     view. The caller's writes follow with no await between (store
    //     calls are synchronous underneath), so the answer holds for them.
    let removed = crate::generation_topup::no_longer_wrap_targets(
        store,
        ctx.root,
        members.iter().map(|m| m.device_id),
    )
    .await?;
    if !removed.is_empty() {
        bail!(
            "{} member(s) of this mint were removed while the escrow deposit was in flight — \
             refusing a mint that would wrap the generation key to a removed device; the next \
             origination mints over the fresh device-set view (account-data-taxonomy.md § The \
             generation machinery → The mint protocol)",
            removed.len()
        );
    }

    // 6. The two rows the caller writes (door-lessly in tests; through the
    //    plane once step 6's tip resolution admits machinery kinds).
    let mint_entry = StateEntry {
        kind: KIND_GENERATION_MINT.into(),
        key: mint_key,
        scope: machinery_scope(KIND_GENERATION_MINT)?.into(),
        value: mint_value,
        merge_meta: None,
        entry_version: 0,
        tombstone: false,
    };
    let receipt_entry = StateEntry {
        kind: KIND_ESCROW_RECEIPT.into(),
        key: escrow_receipt_cell_key(&built.generation_id, &receipt.holder_id, ctx.root),
        scope: machinery_scope(KIND_ESCROW_RECEIPT)?.into(),
        value: fauna_core::encoding::canonical_encode(&receipt)?,
        merge_meta: None,
        entry_version: 0,
        tombstone: false,
    };

    debug_assert!(matches!(
        fauna_core::encoding::canonical_decode::<GenerationMintRecord>(&mint_entry.value),
        Ok(GenerationMintRecord::Minted { .. })
    ));

    Ok(MintedGeneration {
        generation_id: built.generation_id,
        mint_entry,
        receipt_entry,
        receipt,
        gen_key: built.gen_key,
        spilled: built.spilled,
    })
}

fn machinery_scope(kind: &str) -> Result<&'static str> {
    match home_scope_for_kind(kind) {
        Some(std::borrow::Cow::Borrowed(scope)) => Ok(scope),
        _ => Err(anyhow::anyhow!(
            "kind {kind:?} is not registered — machinery kinds always are"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_account_store::sqlite::SqliteBackend;
    use fauna_account_store::types::WriterId;
    use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
    use fauna_core::encoding::{EmbedAsBytes, sign_envelope};
    use fauna_core::generation::{
        DeviceSetRecord, MAX_MINT_PARENTS, derive_device_xwing_keypair, derive_escrow_xwing_keypair,
    };
    use fauna_core::identity::ActorKeypair;
    use fauna_mls::wrapped_blob::generation_wraps::open_generation_key_from_escrow;
    use fauna_protocol::account_state::MAX_STATE_ENTRY_BYTES;
    use fauna_protocol::{decode_strict, encode_canonical};
    use std::sync::Mutex;

    fn root() -> ActorKeypair {
        ActorKeypair::from_secret([0x77u8; 32])
    }

    /// The root identity's seed — `escrow_target_entry` keys the row by the
    /// actor the seed derives, so it must be `root()`'s.
    const SEED: [u8; 32] = [0x77u8; 32];

    fn holder_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[0x66u8; 32])
    }

    fn holder_id() -> [u8; 32] {
        holder_key().verifying_key().to_bytes()
    }

    async fn store() -> AccountStore<SqliteBackend> {
        AccountStore::open(
            SqliteBackend::open_in_memory().unwrap(),
            &root().actor_id_hex(),
            WriterId([0x0Au8; 32]),
        )
        .await
        .unwrap()
    }

    fn enrollment_row(device: &ed25519_dalek::SigningKey) -> StateEntry {
        let id = device.verifying_key().to_bytes();
        let cert = DeviceAuthorization {
            actor_id: root().actor_id(),
            device_key: id,
            capabilities: vec![Capability::RenewBearer],
            created_at: Timestamp(1_000),
            expires_at: None,
        };
        let (bytes, env) = sign_envelope(&root(), &cert).unwrap();
        let authorization =
            fauna_core::encoding::canonical_encode(&EmbedAsBytes::from_signed(bytes, env)).unwrap();
        StateEntry {
            kind: KIND_DEVICE_SET.into(),
            key: fauna_core::hex32::encode(&id),
            scope: machinery_scope(KIND_DEVICE_SET).unwrap().into(),
            // Production's own shape: self-signed, the KEM half derived from
            // the secret that signs.
            value: fauna_core::encoding::canonical_encode(
                &fauna_core::generation::sign_device_enrollment(device, authorization, 5_000),
            )
            .unwrap(),
            merge_meta: None,
            entry_version: 0,
            tombstone: false,
        }
    }

    /// A fake escrow door: signs honest receipts with `holder_key()` unless
    /// told to misbehave, and counts deposits.
    struct FakeDoor {
        deposits: Mutex<Vec<([u8; 32], Vec<u8>)>>,
        mode: DoorMode,
    }

    enum DoorMode {
        Honest,
        /// Honest, after one scheduler yield — the deposit's network await
        /// made visible to a hand-polling test.
        YieldThenHonest,
        Refuse,
        WrongWrapHash,
    }

    impl FakeDoor {
        fn new(mode: DoorMode) -> Self {
            FakeDoor {
                deposits: Mutex::new(Vec::new()),
                mode,
            }
        }
    }

    #[derive(Debug)]
    struct DoorError;
    impl std::fmt::Display for DoorError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "door refused")
        }
    }

    impl RpcRequester for FakeDoor {
        type Error = DoorError;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, DoorError>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            assert_eq!(kind, KIND_ESCROW_PUT, "the sequence calls exactly one door");
            let req: EscrowPutRequest =
                decode_strict(&encode_canonical(&payload).unwrap()).unwrap();
            if matches!(self.mode, DoorMode::Refuse) {
                return Err(DoorError);
            }
            if matches!(self.mode, DoorMode::YieldThenHonest) {
                tokio::task::yield_now().await;
            }
            let generation: [u8; 32] = req.generation_id.as_slice().try_into().unwrap();
            let wrap_hash = match self.mode {
                DoorMode::WrongWrapHash => [0xEEu8; 32],
                _ => <[u8; 32]>::from(blake3::hash(&req.wrap)),
            };
            self.deposits
                .lock()
                .unwrap()
                .push((generation, req.wrap.to_vec()));
            let receipt = fauna_core::generation::sign_escrow_receipt(
                &holder_key(),
                generation,
                wrap_hash,
                &req.target_key,
                7_000,
            );
            let reply = EscrowPutReply {
                receipt: ByteBuf::from(fauna_core::encoding::canonical_encode(&receipt).unwrap()),
                ..Default::default()
            };
            Ok(decode_strict(&encode_canonical(&reply).unwrap()).unwrap())
        }
    }

    /// `removed`'s `Removed` row, attributed to `by` — what
    /// `fleet_removal::write_removed` writes.
    fn removal_row(removed: &[u8; 32], by: &[u8; 32]) -> StateEntry {
        StateEntry {
            kind: KIND_DEVICE_SET.into(),
            key: fauna_core::hex32::encode(removed),
            scope: machinery_scope(KIND_DEVICE_SET).unwrap().into(),
            value: fauna_core::encoding::canonical_encode(&DeviceSetRecord::Removed {
                removed_at_ms: 6_000,
                removed_by: *by,
            })
            .unwrap(),
            merge_meta: None,
            entry_version: 0,
            tombstone: false,
        }
    }

    /// A device's signing key — `[seed; 32]` is the secret, the device id its
    /// public half (real keys since the mint is minter-signed).
    fn device_key(seed: u8) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
    }

    fn device(seed: u8) -> [u8; 32] {
        device_key(seed).verifying_key().to_bytes()
    }

    fn ctx<'a>(
        minter_key: &'a ed25519_dalek::SigningKey,
        trusted: &'a [[u8; 32]],
        root_id: &'a ActorId,
    ) -> MintContext<'a> {
        MintContext {
            root: root_id,
            minter_key,
            trusted_holders: trusted,
        }
    }

    /// The whole sequence: staged fleet + published target → mint → the door
    /// saw exactly one deposit whose wrap the escrow secret opens to the
    /// minted key, and the two returned rows decode to the mint record and
    /// the verified receipt under the machinery kinds' fleet scope.
    #[tokio::test]
    async fn a_mint_deposits_first_and_returns_the_two_plane_rows() {
        let store = store().await;
        let (a, b) = (device(0x0A), device(0x0B));
        store
            .put_state(enrollment_row(&device_key(0x0A)))
            .await
            .unwrap();
        store
            .put_state(enrollment_row(&device_key(0x0B)))
            .await
            .unwrap();
        store
            .put_state(escrow_target_entry(&SEED).unwrap())
            .await
            .unwrap();

        let door = FakeDoor::new(DoorMode::Honest);
        let root_id = root().actor_id();
        let trusted = [holder_id()];
        let a_key = device_key(0x0A);
        let minted = mint_generation(
            &store,
            &door,
            &ctx(&a_key, &trusted, &root_id),
            vec![],
            9_000,
        )
        .await
        .expect("the sequence completes");

        // Exactly one deposit, and the escrow secret recovers the minted key
        // from the exact bytes the door persisted.
        let deposits = door.deposits.lock().unwrap();
        assert_eq!(deposits.len(), 1);
        let (dep_generation, dep_wrap) = &deposits[0];
        assert_eq!(*dep_generation, minted.generation_id);
        let core_commitment = minted.gen_key.commitment();
        let recovered = open_generation_key_from_escrow(
            dep_wrap,
            &derive_escrow_xwing_keypair(&SEED).secret,
            &minted.generation_id,
            &escrow_target_identity_key(&root().actor_id()),
            &core_commitment,
        )
        .expect("the identity's escrow secret opens the deposit");
        assert_eq!(recovered.as_bytes(), minted.gen_key.as_bytes());

        // The mint row: right kind/key/scope, both members wrapped.
        assert_eq!(minted.mint_entry.kind, KIND_GENERATION_MINT);
        assert_eq!(
            minted.mint_entry.key,
            fauna_core::hex32::encode(&minted.generation_id)
        );
        assert_eq!(minted.mint_entry.scope, "state-fleet");
        let record: GenerationMintRecord =
            fauna_core::encoding::canonical_decode(&minted.mint_entry.value).unwrap();
        let GenerationMintRecord::Minted {
            core,
            minter_sig: _,
            wraps,
        } = record
        else {
            panic!("a fresh mint is Minted");
        };
        let mut expected_members = vec![a, b];
        expected_members.sort();
        assert_eq!(core.member_ids, expected_members);
        assert_eq!(core.minter, a);
        assert_eq!(core.key_commitment, core_commitment);
        assert_eq!(wraps.len(), 2);

        // The receipt row: verified, trusted, bound to this deposit.
        assert_eq!(minted.receipt_entry.kind, KIND_ESCROW_RECEIPT);
        assert_eq!(
            minted.receipt_entry.key,
            format!(
                "{}/{}/{}",
                fauna_core::hex32::encode(&minted.generation_id),
                fauna_core::hex32::encode(&holder_id()),
                fauna_core::hex32::encode(&root_id.0)
            ),
            "one receipt cell per (generation, holder, identity)"
        );
        assert_eq!(minted.receipt_entry.scope, "state-fleet");
        assert_eq!(minted.receipt.holder_id, holder_id());
        assert_eq!(
            minted.receipt.target_key,
            escrow_target_identity_key(&root_id),
            "the receipt binds this identity's escrow target"
        );
    }

    /// The keyless-posture badge's derivation
    /// (`generation_tip::keyed_principals_at_tip`): before any mint the
    /// answer is `None` — the fail-safe that keeps every badge off an
    /// unresolved world; after a mint wrapping members A+B, the answer is
    /// exactly the wrapped set — an enrolled-later device with no wrap at
    /// the tip is NOT in it, which is precisely what the badge marks.
    #[tokio::test]
    async fn keyed_principals_at_tip_names_the_wrapped_members_only() {
        let store = store().await;
        let (a, b, c) = (device(0x0A), device(0x0B), device(0x0C));
        let a_key = device_key(0x0A);
        let trust = crate::generation_tip::GenerationTrust {
            root: root().actor_id(),
            prior: Vec::new(),
            trusted_holders: vec![holder_id()].into(),
        };

        // No mint yet → no tip → None (never an empty "everyone is keyless").
        assert!(
            crate::generation_tip::keyed_principals_at_tip(&store, &trust, &a_key, None)
                .await
                .expect("resolves")
                .is_none(),
            "no tip must answer None, not an empty set"
        );

        let a_row = enrollment_row(&a_key);
        store.put_state(a_row).await.unwrap();
        store
            .put_state(enrollment_row(&device_key(0x0B)))
            .await
            .unwrap();
        store
            .put_state(escrow_target_entry(&SEED).unwrap())
            .await
            .unwrap();
        let door = FakeDoor::new(DoorMode::Honest);
        let root_id = root().actor_id();
        let trusted = [holder_id()];
        let minted = mint_generation(
            &store,
            &door,
            &ctx(&a_key, &trusted, &root_id),
            vec![],
            9_000,
        )
        .await
        .expect("mint");
        store.put_state(minted.mint_entry.clone()).await.unwrap();
        store.put_state(minted.receipt_entry.clone()).await.unwrap();
        // A third device enrolls AFTER the mint: fleet-visible, but no wrap
        // at the tip — the keyless posture, derived.
        store
            .put_state(enrollment_row(&device_key(0x0C)))
            .await
            .unwrap();

        let keyed = crate::generation_tip::keyed_principals_at_tip(&store, &trust, &a_key, None)
            .await
            .expect("resolves")
            .expect("a tip resolves after the acked mint");
        assert!(
            keyed.contains(&a) && keyed.contains(&b),
            "inline wraps count"
        );
        assert!(
            !keyed.contains(&c),
            "no wrap at the tip = not keyed — the badge's positive fact"
        );
    }

    /// The charter's precise refusal: no published escrow target → no key
    /// material is minted and no door is called; the error says exactly what
    /// a seed-holding surface must do.
    #[tokio::test]
    async fn a_mint_refuses_without_a_published_escrow_target_and_says_why() {
        let store = store().await;
        store
            .put_state(enrollment_row(&device_key(0x0A)))
            .await
            .unwrap();
        let door = FakeDoor::new(DoorMode::Honest);
        let root_id = root().actor_id();
        let trusted = [holder_id()];
        let a_key = device_key(0x0A);
        let err = mint_generation(&store, &door, &ctx(&a_key, &trusted, &root_id), vec![], 1)
            .await
            .expect_err("no target → refuse");
        assert!(err.to_string().contains("no escrow target is published"));
        assert!(err.to_string().contains("fauna.state.escrow-target"));
        assert!(
            door.deposits.lock().unwrap().is_empty(),
            "no deposit happened"
        );
    }

    // ── The bounded mint ─────────────────────────────────────────────────────
    //
    // Before it, the plane's per-entry cap was checked here before the escrow
    // door and a few dozen members was the end of minting: every enrolled
    // member added an inline wrap to the one mint row. That refusal is now
    // the backstop; the tests below pin the shape that made it unreachable.

    /// A 32-byte seed for member `i` of a fleet larger than `u8` can name.
    fn seed_n(i: u16) -> [u8; 32] {
        let mut s = [0xA5u8; 32];
        s[0] = i as u8;
        s[1] = (i >> 8) as u8;
        s
    }

    fn device_key_n(i: u16) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&seed_n(i))
    }

    /// The production member shape: the KEM key derives from the device's
    /// Ed25519 secret bytes.
    fn member_n(i: u16) -> FleetMember {
        FleetMember {
            device_id: device_key_n(i).verifying_key().to_bytes(),
            xwing_pubkey: derive_device_xwing_keypair(&seed_n(i))
                .public
                .to_bytes()
                .to_vec(),
            enrolled_at_ms: 5_000,
        }
    }

    /// The sealed size of a mint row, in its `Gen0` form — what the nest
    /// measures against `MAX_STATE_ENTRY_BYTES`.
    fn sealed_len_of(record: &GenerationMintRecord, generation_id: &[u8; 32]) -> usize {
        fauna_core::account_entry_crypto::sealed_envelope_len(
            &fauna_core::account_entry_crypto::EntryPlaintext {
                kind: KIND_GENERATION_MINT.into(),
                key: fauna_core::hex32::encode(generation_id),
                merge_meta: None,
                value: fauna_core::encoding::canonical_encode(record)
                    .unwrap()
                    .into(),
                tombstone: false,
            },
            false,
        )
        .unwrap()
    }

    /// **The defect this closes, inverted.** Before the bounded mint a
    /// 64-member fleet's sequence refused here (one inline wrap per member
    /// overflowed the per-entry cap, and the guard that landed with the
    /// capture refused before the deposit); now it mints: every member
    /// listed, the cap wrapped inline with the minter among them, the rest
    /// named as the spill for the caller's top-ups, the row under the cap,
    /// exactly one deposit.
    #[tokio::test]
    async fn a_64_member_fleet_mints_a_bounded_generation_and_names_its_spill() {
        let store = store().await;
        for seed in 1..=64u8 {
            store
                .put_state(enrollment_row(&device_key(seed)))
                .await
                .unwrap();
        }
        store
            .put_state(escrow_target_entry(&SEED).unwrap())
            .await
            .unwrap();
        let door = FakeDoor::new(DoorMode::Honest);
        let root_id = root().actor_id();
        let trusted = [holder_id()];
        let minter = device_key(1);
        let minted = mint_generation(&store, &door, &ctx(&minter, &trusted, &root_id), vec![], 1)
            .await
            .expect("a 64-member fleet mints a bounded generation");
        assert_eq!(door.deposits.lock().unwrap().len(), 1);

        let GenerationMintRecord::Minted { core, wraps, .. } =
            fauna_core::encoding::canonical_decode(&minted.mint_entry.value).unwrap()
        else {
            panic!("a fresh mint is Minted");
        };
        assert_eq!(core.member_ids.len(), 64, "every member is listed");
        assert_eq!(wraps.len(), MAX_INLINE_MEMBER_WRAPS, "the cap is inline");
        assert!(
            wraps.iter().any(|w| w.device_id == device(1)),
            "the minter is inline"
        );
        assert_eq!(minted.spilled.len(), 64 - MAX_INLINE_MEMBER_WRAPS);
        for s in &minted.spilled {
            assert!(core.member_ids.contains(&s.device_id));
            assert!(!wraps.iter().any(|w| w.device_id == s.device_id));
        }
        let len = sealed_len_of(
            &fauna_core::encoding::canonical_decode(&minted.mint_entry.value).unwrap(),
            &minted.generation_id,
        );
        assert!(
            len <= MAX_STATE_ENTRY_BYTES,
            "the bounded row seals under the cap: {len} > {MAX_STATE_ENTRY_BYTES}"
        );
    }

    /// One member past the ceiling refuses in the sequence BEFORE the escrow
    /// deposit — the same orphan-wrap discipline as the pre-bounding guard,
    /// now at the member ceiling instead of the byte cap.
    #[tokio::test]
    async fn a_fleet_over_the_member_ceiling_refuses_before_any_deposit() {
        let store = store().await;
        for i in 0..=MAX_MINT_MEMBERS as u16 {
            store
                .put_state(enrollment_row(&device_key_n(i)))
                .await
                .unwrap();
        }
        store
            .put_state(escrow_target_entry(&SEED).unwrap())
            .await
            .unwrap();
        let door = FakeDoor::new(DoorMode::Honest);
        let root_id = root().actor_id();
        let trusted = [holder_id()];
        let minter = device_key_n(0);
        let err = mint_generation(&store, &door, &ctx(&minter, &trusted, &root_id), vec![], 1)
            .await
            .expect_err("one member over the ceiling cannot mint");
        let msg = format!("{err:#}");
        assert!(msg.contains("member ceiling"), "{msg}");
        assert!(
            door.deposits.lock().unwrap().is_empty(),
            "refused before the escrow door: {msg}"
        );
    }

    /// **The ceiling is measured, not assumed.** A mint at `MAX_MINT_MEMBERS`
    /// with a full `MAX_MINT_PARENTS` parent list and the inline cap wrapped
    /// seals under `MAX_STATE_ENTRY_BYTES` with headroom; the marginal costs
    /// the constants' docs quote (34 B per listed member and per parent, each
    /// id a 32-byte CBOR byte string; about 1.3 KB per inline wrap, its bytes
    /// a byte string too) are measured here from
    /// the encodings themselves —
    /// the sweep log that first sized the defect is not re-checkable, this
    /// is. Moving either constant re-runs this arithmetic.
    #[test]
    fn the_bounded_mint_fits_the_cap_at_the_member_ceiling_with_a_full_parent_list() {
        use fauna_core::encoding::canonical_encode;
        let members: Vec<FleetMember> = (0..MAX_MINT_MEMBERS as u16).map(member_n).collect();
        // Parent ids shaped like real ones — content-derived hashes. As byte
        // strings their width no longer depends on the byte values, but a
        // regression to the integer-array spelling would (and would show as
        // up to 66 B each, failing the exact pin below).
        let parents: Vec<[u8; 32]> = (0..MAX_MINT_PARENTS as u8)
            .map(|i| *blake3::hash(&[i]).as_bytes())
            .collect();
        let target = EscrowTargetRecord {
            xwing_escrow_pubkey: derive_escrow_xwing_keypair(&SEED)
                .public
                .to_bytes()
                .to_vec(),
        };
        let built = build_mint(
            &members,
            &target,
            &escrow_target_identity_key(&root().actor_id()),
            parents,
            &device_key_n(0),
            1,
        )
        .expect("a mint at the ceiling builds");
        let at_ceiling = sealed_len_of(&built.record, &built.generation_id);
        assert!(
            at_ceiling <= MAX_STATE_ENTRY_BYTES,
            "a mint at the ceiling must seal under the cap: {at_ceiling} > {MAX_STATE_ENTRY_BYTES}"
        );
        let headroom = MAX_STATE_ENTRY_BYTES - at_ceiling;
        assert!(
            headroom >= 4096,
            "the ceiling must leave headroom for encoding drift, not sit on the edge: {headroom} B"
        );

        // Marginal costs, read off the record's own components (never by
        // diffing two builds: every build mints a fresh random key, so its
        // wrap ciphertexts differ from any other build's).
        let GenerationMintRecord::Minted { core, wraps, .. } = &built.record else {
            panic!("a fresh mint is Minted");
        };
        let core_len = canonical_encode(core).unwrap().len();
        let mut one_fewer = core.clone();
        one_fewer.member_ids.pop();
        let per_member = core_len - canonical_encode(&one_fewer).unwrap().len();
        let mut no_parents = core.clone();
        no_parents.parents.clear();
        let per_parent =
            (core_len - canonical_encode(&no_parents).unwrap().len()) / MAX_MINT_PARENTS;
        let per_wrap = canonical_encode(&wraps[0]).unwrap().len();
        eprintln!(
            "bounded mint: {per_member} B per listed member, {per_wrap} B per inline wrap, \
             {per_parent} B per parent; {at_ceiling} B at the ceiling of {MAX_STATE_ENTRY_BYTES}"
        );
        assert_eq!(
            per_member, 34,
            "a listed member is one 32-byte CBOR byte string (2-byte header + 32)"
        );
        assert_eq!(
            per_parent, 34,
            "a parent id is one 32-byte CBOR byte string (2-byte header + 32)"
        );
        assert!(
            (1_100..=1_600).contains(&per_wrap),
            "an inline wrap costs about 1.3 KB as encoded, got {per_wrap}"
        );
    }

    #[tokio::test]
    async fn a_non_member_minter_is_refused() {
        let store = store().await;
        store
            .put_state(enrollment_row(&device_key(0x0A)))
            .await
            .unwrap();
        store
            .put_state(escrow_target_entry(&SEED).unwrap())
            .await
            .unwrap();
        let door = FakeDoor::new(DoorMode::Honest);
        let root_id = root().actor_id();
        let trusted = [holder_id()];
        let stranger_key = device_key(0xFF);
        let err = mint_generation(
            &store,
            &door,
            &ctx(&stranger_key, &trusted, &root_id),
            vec![],
            1,
        )
        .await
        .expect_err("a stranger cannot mint");
        assert!(
            err.to_string()
                .contains("not a verified, non-removed member")
        );
        assert!(door.deposits.lock().unwrap().is_empty());
    }

    /// Deposit-first, negatively: the door failing means the sequence fails —
    /// no entries exist for anyone to write, so an unescrowed generation is
    /// unrepresentable as an output of this function.
    #[tokio::test]
    async fn a_door_failure_fails_the_sequence_before_any_entry_exists() {
        let store = store().await;
        store
            .put_state(enrollment_row(&device_key(0x0A)))
            .await
            .unwrap();
        store
            .put_state(escrow_target_entry(&SEED).unwrap())
            .await
            .unwrap();
        let door = FakeDoor::new(DoorMode::Refuse);
        let root_id = root().actor_id();
        let trusted = [holder_id()];
        let a_key = device_key(0x0A);
        let err = mint_generation(&store, &door, &ctx(&a_key, &trusted, &root_id), vec![], 1)
            .await
            .expect_err("door down → sequence fails");
        assert!(err.to_string().contains("escrow deposit failed"));
    }

    /// The trust half: a receipt that verifies holder-generically but names a
    /// holder outside the caller's pin set is refused — integrity is the
    /// shared contract's job, trust is this sequence's.
    #[tokio::test]
    async fn an_untrusted_holders_receipt_is_refused() {
        let store = store().await;
        store
            .put_state(enrollment_row(&device_key(0x0A)))
            .await
            .unwrap();
        store
            .put_state(escrow_target_entry(&SEED).unwrap())
            .await
            .unwrap();
        let door = FakeDoor::new(DoorMode::Honest);
        let root_id = root().actor_id();
        let trusted = [[0xDDu8; 32]]; // pins some other holder
        let a_key = device_key(0x0A);
        let err = mint_generation(&store, &door, &ctx(&a_key, &trusted, &root_id), vec![], 1)
            .await
            .expect_err("wrong holder → refuse");
        assert!(err.to_string().contains("does not trust"));
    }

    /// The binding half: a receipt over the wrong wrap hash is refused even
    /// from a trusted holder — the receipt must ack THIS deposit.
    #[tokio::test]
    async fn a_receipt_binding_the_wrong_wrap_is_refused() {
        let store = store().await;
        store
            .put_state(enrollment_row(&device_key(0x0A)))
            .await
            .unwrap();
        store
            .put_state(escrow_target_entry(&SEED).unwrap())
            .await
            .unwrap();
        let door = FakeDoor::new(DoorMode::WrongWrapHash);
        let root_id = root().actor_id();
        let trusted = [holder_id()];
        let a_key = device_key(0x0A);
        let err = mint_generation(&store, &door, &ctx(&a_key, &trusted, &root_id), vec![], 1)
            .await
            .expect_err("wrong wrap hash → refuse");
        assert!(err.to_string().contains("binds a different wrap"));
    }

    /// A removed device is not a wrap target even while its row's cert still
    /// verifies — the mint reads the VIEW, and exclusion is unconditional.
    #[tokio::test]
    async fn a_removed_member_is_not_wrapped_to() {
        let store = store().await;
        let (a, b) = (device(0x0A), device(0x0B));
        store
            .put_state(enrollment_row(&device_key(0x0A)))
            .await
            .unwrap();
        store
            .put_state(enrollment_row(&device_key(0x0B)))
            .await
            .unwrap();
        // B is removed (merged state: the lattice would keep Removed whatever
        // the order; staging the merged truth directly is the store's job).
        store.put_state(removal_row(&b, &a)).await.unwrap();
        store
            .put_state(escrow_target_entry(&SEED).unwrap())
            .await
            .unwrap();

        let door = FakeDoor::new(DoorMode::Honest);
        let root_id = root().actor_id();
        let trusted = [holder_id()];
        let a_key = device_key(0x0A);
        let minted = mint_generation(&store, &door, &ctx(&a_key, &trusted, &root_id), vec![], 1)
            .await
            .unwrap();
        let record: GenerationMintRecord =
            fauna_core::encoding::canonical_decode(&minted.mint_entry.value).unwrap();
        let GenerationMintRecord::Minted {
            core,
            minter_sig: _,
            wraps,
        } = record
        else {
            panic!()
        };
        assert_eq!(
            core.member_ids,
            vec![a],
            "the removed id is out of the member set"
        );
        assert_eq!(wraps.len(), 1);
        assert_eq!(wraps[0].device_id, a);
    }

    /// **a member removed while the deposit is in flight
    /// refuses the mint.** The member list — inside the content-derived id,
    /// so it cannot be filtered after the fact — is fixed before the escrow
    /// await, and the fleet-removal quartet is a local command a running
    /// pass serves at exactly such an await. Polled by hand over a door that
    /// yields once: B's `Removed` row lands mid-deposit, and the sequence
    /// must not hand back a mint row carrying B's wrap.
    #[tokio::test]
    async fn a_member_removed_during_the_deposit_refuses_the_mint() {
        let store = store().await;
        let (a, b) = (device(0x0A), device(0x0B));
        store
            .put_state(enrollment_row(&device_key(0x0A)))
            .await
            .unwrap();
        store
            .put_state(enrollment_row(&device_key(0x0B)))
            .await
            .unwrap();
        store
            .put_state(escrow_target_entry(&SEED).unwrap())
            .await
            .unwrap();

        let door = FakeDoor::new(DoorMode::YieldThenHonest);
        let root_id = root().actor_id();
        let trusted = [holder_id()];
        let a_key = device_key(0x0A);
        let mint_ctx = ctx(&a_key, &trusted, &root_id);
        let mut mint = std::pin::pin!(mint_generation(&store, &door, &mint_ctx, vec![], 1));
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        let mut yields = 0usize;
        let outcome = loop {
            match mint.as_mut().poll(&mut cx) {
                std::task::Poll::Ready(out) => break out,
                std::task::Poll::Pending => {
                    yields += 1;
                    store.put_state(removal_row(&b, &a)).await.unwrap();
                }
            }
        };
        assert_eq!(yields, 1, "the deposit is the sequence's one await");
        assert_eq!(door.deposits.lock().unwrap().len(), 1, "the deposit ran");
        let err = outcome.expect_err("a stale member list must not become a mint row");
        assert!(
            err.to_string()
                .contains("removed while the escrow deposit was in flight"),
            "the refusal says why: {err:#}"
        );
    }
}
