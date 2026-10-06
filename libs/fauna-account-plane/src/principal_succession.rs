//! Principal succession — the ceremony-time probe, the in-place writer
//! rotation and the lost-slot heal, platform-generic (owner:
//! `account-replica-posture.md` § The store device principal → *Principal
//! succession after a device delete*, refinements 10 and 11). Every host's
//! assembly runs both steps in one order — the probe after the slot resolves
//! and before the backend opens, the heal between the backend open and the
//! store open: natively `fauna_sync_engine::account_runtime` (which keeps
//! every name at its old path, `fauna_sync_engine::principal_succession`),
//! on web [`crate::web_host`].
//!
//! The seams, where the native code named its store dir: the slot is the
//! bundle's [`SecretStore`]; the migration critical section is the bundle's
//! own [`SlotSection`] (natively the store dir's `migration.lock`, on web the
//! tab's section — `principal_bundle` says why that is enough there), whose
//! `None` is refused here, never degraded open; the store is any
//! [`StoreBackend`], and the rotation's own backend comes from the caller's
//! opener, because it runs before the assembly opens one.
//!
//! The shape, end to end: a user's `fauna.sync.devices.delete` tombstones the
//! machine's writer key on the nest. The next **ceremony-capable sign-in** (a
//! seed-holding assembly) re-registers the held grant idempotently
//! ([`ceremony_probe`]); the nest's typed `fauna.sync.device_grant_revoked` answer —
//! its own memory that the identity was ended — is the ONE piece of evidence
//! that licenses the automatic remedy (`rotate`): mint a fresh writer, re-key
//! the slot, re-sign the `DeviceAuthorization`, then land the **fence**
//! (`rotate_writer_identity` — the store's writer meta re-stamped atomically
//! with the pending-re-author marker). The assembly restarts, opens the store
//! as the successor, and the pump's
//! [`tail_reauthor_pass`](crate::succession_tail::tail_reauthor_pass)
//! re-journals every not-provably-pushed predecessor row under the successor
//! before the same pass's `publish_pending` sends them — the no-data-loss half
//! (decision 3).
//!
//! **Decision 1's third trigger — the fleet plane's own evidence
//! (2026-10-01):** this machine's own `fauna.state.device-set` row reads
//! `Removed`. The row is absorbing, so the principal is ended as a fleet
//! member exactly as a tombstone ends it — but the nest's answer does not
//! always follow the row (a guardian-marked row keeps its grant, a removal by
//! key reaches the nest only through a sibling's pass, a rebuilt box
//! remembers nothing), and then the probe above registers happily for ever.
//! The probe cannot read merged state (it runs before the backend opens), so
//! the pump's enrollment step is the reader, ahead of its latch; a
//! seed-holding worker reassembles on the answer and hands the finding to
//! [`ceremony_probe`], which runs the same `rotate` with no nest leg. A
//! sign-out's own `Removed` row is not this evidence — the store's sign-out
//! stamp tells them apart (`EnrollmentPass::SignedOut`).
//!
//! **The second trigger — the lost-slot arm (refinement 10, 2026-08-27):**
//! the same fence, fired on the shape *the slot holds a key the stamped store
//! does not name* ([`lost_slot_heal`]), with no nest evidence and no seed. A
//! slot lost while the store survives (a reset login keychain / Credential
//! Manager / Secret Service collection, a keyring swept by a tool, a browser's
//! localStorage cleared or evicted apart from its IndexedDB) used to strand
//! the machine for good: the next assembly minted a fresh writer into the
//! empty slot, `AccountStore::open` refused the store ("belongs to a different
//! writer"), and every later launch found the slot FULL of that doomed key.
//! Now the assembly reads the store's stamp between the backend open and the
//! store open and fences onto the slot's key — the store's writer retired, its
//! un-pushed tail re-authored under the slot's key by the same pass. Keyed on
//! *disagreement*, not emptiness, so it also heals a slot key that
//! merely differs from the store's writer.
//!
//! **The journal-bound writer (refinement 11, 2026-09-15):**
//! a writer lives exactly as long as its journal, because the journal is the
//! only home of its seq counter. Two shapes fire the same rotation machinery
//! from the other side. **A key LOADED from the slot over a store with no
//! stamped writer** ([`lost_slot_heal`]'s inverse arm, on [`WriterKeyProvenance`])
//! is a key whose history this journal cannot vouch for: a fresh journal would
//! re-issue seqs 1.. under it, the nest refuses each as `stale_writer_seq`
//! where it holds a head for the item and ACCEPTS a reused coordinate where it
//! does not, and every other replica then meets journal equivocation. The key
//! is abandoned on the spot — a fresh writer minted into the slot, the fresh
//! store stamped with it (`retire_unjournaled_writer`), assembly restarted —
//! and the machine is a new fleet device, at the cost the lost-slot arm's
//! bound (c) already records: the old device row lingers until the user
//! removes it. **A walk that serves the CURRENT writer's row this journal does
//! not hold** — above what it holds, or at a held coordinate under a different
//! item — is the burnt-journal signature (a store restored from an older
//! backup while the slot kept the key): the walk refuses the row and stamps the durable
//! burnt marker, the pump reassembles, and this arm rotates the burnt writer
//! onto a fresh mint through the ordinary fence — retired memory, re-author
//! marker, the un-pushed tail re-journaled under the successor on the next
//! pass. Rejected: seeding the counter from the nest's head or walking before
//! the first write (an offline first launch cannot, and the nest is not the
//! whole record — a peer may hold a coordinate the nest never saw, the relay
//! plane's own premise).
//!
//! What this module deliberately does NOT do:
//! - **No revocation rotation without the typed evidence** — a generic
//!   register failure (bad signature, offline) stays in today's
//!   retry loop. (The lost-slot arm's evidence is the shape itself — a local
//!   fact read under the store's own section, never a nest answer; the
//!   fleet plane's is the machine's own absorbing `Removed` row.)
//! - **No revocation rotation on a seedless host** — the successor grant is
//!   root-signed; a host with no seed surfaces
//!   `EnrollmentPass::RemovedFromAccount` loudly instead. The lost-slot FENCE
//!   does run seedless — it signs nothing — and the grant follows at the next
//!   seed-holding ceremony, exactly the seedless posture of refinement 7.
//! - **No un-tombstoning, ever** — the dead key is abandoned, never re-used
//!   (the ruling's rejected shapes); a RETIRED writer found in the slot is
//!   replaced by a fresh mint, never fenced back to work.
//! - **No re-stamp of fleet-held history** — the re-author bound is
//!   `writer_seq > frontier high-water`, one-directional by construction.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ed25519_dalek::SigningKey;
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::{
    retire_unjournaled_writer, retired_writers, rotate_writer_identity, stamped_and_burnt_writer,
};
use fauna_account_store::types::WriterId;
use fauna_core::identity::ActorKeypair;
use fauna_protocol::{RpcErrorClass, RpcRequester};

pub use crate::principal_bundle::WriterKeyProvenance;
use crate::principal_bundle::{self, PrincipalBundle, SecretStore, SlotSection};

/// How long a sign-in may spend on the succession probe's two nest legs
/// before proceeding as an offline sign-in would. A generous ceiling, not a
/// timing assumption (convention 14): a green online probe answers in one
/// round-trip; the budget only bounds how long an UNREACHABLE nest can delay
/// the readiness barrier. On timeout the machine simply keeps today's state —
/// the evidence re-derives at the next ceremony-capable assembly.
const SUCCESSION_PROBE_BUDGET: Duration = Duration::from_secs(15);

/// What the ceremony-time succession probe concluded.
pub enum CeremonyProbe {
    /// The held grant registered (or re-registered) — the latch now records
    /// it, so the pump's enrollment step answers `Current` without an RPC.
    Registered,
    /// The held key is tombstoned and the rotation ran (or a sibling's
    /// already had): the slot and the store meta now name the successor.
    /// The caller restarts assembly to adopt it.
    Rotated,
    /// No grant in the slot, a transport failure, or the probe budget
    /// elapsed — proceed exactly as an offline sign-in does today.
    Skipped,
}

/// Decision 1's evidence gate, run once per **seed-holding** assembly, after
/// the slot's section block (the probe is an RPC — never inside the section)
/// and before the store opens (a rotation restarts assembly with nothing to
/// tear down).
///
/// Idempotent re-registration is the probe: the same two legs the pump's
/// enrollment step runs, minus the content-addressed latch short-circuit —
/// which is exactly why the pump alone can never surface a delete (the latch
/// keys on the grant wire, and a nest-side delete changes no local byte).
///
/// `open_backend` opens the store's backend for the rotation's fence — only
/// called when the evidence licenses one: the nest's typed revocation, or
/// `own_row_removed` naming the held writer.
///
/// `own_row_removed` is the third trigger's evidence
/// (`AccountDriver::own_row_removed`): the writer whose own device-set row
/// the pump read `Removed`. This gate runs before the backend opens and
/// cannot read merged state itself, so the worker carries the finding here
/// across the reassembly it caused — and when it names the held writer the
/// rotation runs with no nest leg at all.
// Eleven distinct identity/credential inputs, each read exactly once by a
// different leg of the gate (transport, credential store, actor, section,
// backend opener, seed, held writer, slot, enrollment target, rotation
// policy, the pump's finding). Bundling them would name a struct whose only member function is
// this one — the same call the engine's over-limit plumbing makes
// (`peer_leg.rs`, `custody_leg.rs`).
#[allow(clippy::too_many_arguments)]
pub async fn ceremony_probe<R, S, X, B, F, Fut>(
    rpc: &R,
    credentials: &Arc<S>,
    actor_id_hex: &str,
    section: &X,
    open_backend: F,
    seed: &ActorKeypair,
    held_writer: &SigningKey,
    slot: &PrincipalBundle<S, X>,
    enrollment_target: &str,
    allow_rotation: bool,
    own_row_removed: Option<[u8; 32]>,
) -> Result<CeremonyProbe>
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
    S: SecretStore + ?Sized,
    X: SlotSection + Clone,
    B: StoreBackend,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<B>>,
{
    // Decision 1's third trigger: the pump read this machine's own
    // device-set row `Removed` and the worker reassembled on it. The row is
    // absorbing, so the held principal is ended as a fleet member whatever
    // the nest would answer — and it may well answer OK: a guardian-marked
    // row keeps its grant, a removal by key reaches the nest only through a
    // sibling's later pass, a rebuilt box remembers no revocation. So the
    // finding is the evidence, and nothing is asked. Keyed on the writer it
    // was read for: once a sibling has rotated the slot, it names nobody
    // this assembly holds.
    if own_row_removed.is_some_and(|removed| removed == held_writer.verifying_key().to_bytes()) {
        if !allow_rotation {
            // The cap is spent, and a removed machine never re-registers: no
            // register for a key the fleet has ended.
            return Ok(CeremonyProbe::Skipped);
        }
        tracing::info!(
            "principal succession: this machine's own device-set row reads Removed — it \
             was removed on the fleet plane — minting a successor principal"
        );
        rotate(
            credentials,
            actor_id_hex,
            section,
            open_backend,
            seed,
            held_writer,
        )
        .await?;
        return Ok(CeremonyProbe::Rotated);
    }
    let Some(loaded) = slot.device_authorization() else {
        // Nothing to probe: the ceremony's mint arm either just minted (the
        // pump registers it) or failed (healed at the next assembly). A
        // grant-less machine cannot be the tombstoned-key case — the
        // tombstone answers a *register*, and there is nothing to register.
        return Ok(CeremonyProbe::Skipped);
    };
    // The row to probe is the machine's named row, unconditionally
    // (`sync-agent-credentials.md` § Credential model → the RULED 2026-09-28
    // block, decision 3): the store principal is the machine's only renewal
    // credential, so there is no second credential on that row this register
    // could evict. The nest's tombstone check is keyed on
    // `(actor, auth_device_key)` and runs ahead of its row check, so the
    // evidence gate is row-independent either way.
    let target = enrollment_target.to_string();
    let sync = fauna_client_sync::SyncClient::new(rpc.clone());
    // GRANT-FIRST, deliberately inverted from register-then-grant: the nest's
    // revocation-memory check runs ahead of its row check
    // (`set_sync_device_grant` — the tombstone answers even with the row
    // deleted), so leading with the grant means a probe on a deleted machine
    // learns "revoked" WITHOUT re-creating the row the user just deleted —
    // no ghost row undoing the delete gesture. Only a `not_found` answer (the
    // row genuinely missing on a live, un-deleted machine) falls back to the
    // ordinary register + one grant retry.
    let legs = async {
        match sync
            .device_grant_register(target.clone(), loaded.wire.clone())
            .await
        {
            Err(e) if fauna_client_sync::is_device_grant_no_device(&e) => {
                sync.register(target.clone(), fauna_client_sync::SELF_REGISTER_LABEL, None)
                    .await?;
                sync.device_grant_register(target.clone(), loaded.wire.clone())
                    .await
                    .map(|_| ())
            }
            other => other.map(|_| ()),
        }
    };
    // The cross-target sleep, never tokio's timer (it does not build for
    // wasm32 — `tests/no_native_time.rs`).
    let probe = tokio::select! {
        biased;
        answer = legs => Some(answer),
        () = fauna_sleep::sleep(SUCCESSION_PROBE_BUDGET) => None,
    };
    match probe {
        None => Ok(CeremonyProbe::Skipped),
        Some(Ok(())) => {
            slot.record_grant_registered_on(&target);
            Ok(CeremonyProbe::Registered)
        }
        Some(Err(e)) if fauna_client_sync::is_device_grant_revoked(&e) => {
            if !allow_rotation {
                // The livelock/adversary cap: at most ONE rotation per
                // assembly chain. A nest revoking a key minted moments ago
                // is not a user gesture to keep healing — a broken or
                // hostile nest would otherwise draw an unbounded mint loop
                // out of every sign-in. The pump's enrollment step surfaces
                // the loud `RemovedFromAccount` state instead.
                tracing::warn!(
                    "principal succession: the SUCCESSOR's key was refused as revoked \
                     too — not rotating again this sign-in (a fresh key cannot have \
                     been legitimately tombstoned; the removed-from-account state \
                     stays loud instead)"
                );
                return Ok(CeremonyProbe::Skipped);
            }
            tracing::info!(
                "principal succession: this machine's device was deleted from the \
                 account — minting a successor principal"
            );
            rotate(
                credentials,
                actor_id_hex,
                section,
                open_backend,
                seed,
                held_writer,
            )
            .await?;
            Ok(CeremonyProbe::Rotated)
        }
        Some(Err(e)) => {
            tracing::debug!("succession probe: register not reachable/answerable ({e}) — skipped");
            Ok(CeremonyProbe::Skipped)
        }
    }
}

/// Decision 2 — the in-place writer rotation. Serialized by the migration
/// critical section, **refusing** a degraded one (unlike the mint's
/// degrade-open: a half-rotated slot beside a live sibling is the W5.3
/// (account-data-plane.md § Workstreams) forked store, and succession always
/// has a working fallback — stay on today's state and let the next sign-in
/// retry).
///
/// Ordering inside, each step read-back-verified, every crash window healing
/// forward:
/// 1. Re-validate the slot under the section (a sibling may have rotated
///    between our evidence and our section — then adopt theirs, mint nothing).
/// 2. Mint the successor and overwrite the slot's writer secret (read-back
///    COMPARED — an overwrite the platform store silently dropped must not
///    reach the fence, or the store would name a writer no slot holds).
/// 3. Re-sign the `DeviceAuthorization` over the successor key and store it
///    (a crash before this leaves a fresh writer whose stale grant reads as
///    not-enrolled — the ceremony re-mints at the next assembly; the
///    content-addressed enrollment latch self-invalidates either way).
/// 4. The FENCE: `rotate_writer_identity` re-stamps the store's writer meta
///    and the pending-re-author marker in one transaction. From this moment
///    every stale handle's local append refuses typed (`StaleWriter`), so
///    the re-author walk can never miss a late predecessor row.
///
/// Section discipline: held only across effectively-synchronous work (the
/// native `SqliteBackend`'s async methods complete without parking; every
/// slot op is sync) — the native migration lock's never-park-while-holding
/// contract (`locks.rs`) — and the backend is opened BEFORE the section,
/// because the native backend's own open takes the same section internally.
async fn rotate<S, X, B, F, Fut>(
    credentials: &Arc<S>,
    actor_id_hex: &str,
    section: &X,
    open_backend: F,
    seed: &ActorKeypair,
    dead: &SigningKey,
) -> Result<()>
where
    S: SecretStore + ?Sized,
    X: SlotSection + Clone,
    B: StoreBackend,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<B>>,
{
    // The backend FIRST: the native `SqliteBackend::open` takes the migration
    // section for itself internally (its own probe-then-migrate), so opening
    // it while we hold the section would self-deadlock on the second fd.
    // Opening before our entry is safe: the fence below re-validates the
    // predecessor under OUR hold, and `rotate_writer_identity` refuses any
    // picture a racing rotator invalidated.
    let backend = open_backend()
        .await
        .context("succession rotation: open the backend for the fence")?;
    let Some(_section) = section.enter(
        "refusing to rotate unserialized (a half-rotated slot beside a live sibling is a \
         forked store; the next sign-in retries)",
    ) else {
        bail!("succession rotation: the migration section is unavailable — not rotating");
    };
    let dead_pub = dead.verifying_key().to_bytes();
    match principal_bundle::load_writer_key(&**credentials, actor_id_hex) {
        None => bail!(
            "succession rotation: the slot lost its writer key between the evidence \
             and the section — refusing to guess"
        ),
        Some(current) if current.verifying_key().to_bytes() != dead_pub => {
            tracing::info!(
                "principal succession: a sibling already rotated this machine — \
                 adopting its successor"
            );
            return Ok(());
        }
        Some(_) => {}
    }

    let successor = mint_into_slot(&**credentials, actor_id_hex)
        .context("succession rotation: mint the successor")?;

    let successor_pub = successor.verifying_key().to_bytes();
    let succ_slot = PrincipalBundle::<S, X>::resolve(
        Arc::clone(credentials),
        actor_id_hex.to_string(),
        section.clone(),
        &successor_pub,
        None,
    );
    match fauna_client_sync::build_principal_grant(seed, &successor_pub) {
        Ok(wire) => {
            if let Err(e) = succ_slot.store_device_authorization(wire, &successor_pub) {
                tracing::warn!(
                    "succession rotation: successor grant not persisted ({e:#}) — the \
                     next assembly's ceremony re-mints it"
                );
            }
        }
        Err(e) => tracing::warn!(
            "succession rotation: successor grant mint failed ({e}) — the next \
             assembly's ceremony re-mints it"
        ),
    }

    rotate_writer_identity(&backend, &WriterId(dead_pub), &WriterId(successor_pub))
        .await
        .context("succession rotation: the fence")?;
    tracing::info!(
        "principal succession: this machine's store writer is rotated — the dead \
         key is abandoned, the un-pushed tail re-authors on the next pump pass"
    );
    Ok(())
}

/// Mint a fresh writer and persist its secret in the slot, read-back
/// COMPARED: a write the platform store silently dropped must never reach a
/// fence, or the store would name a writer no slot holds — the exact strand
/// the lost-slot arm exists to heal. Shared by both rotation triggers.
pub fn mint_into_slot<S: SecretStore + ?Sized>(
    credentials: &S,
    actor_id_hex: &str,
) -> Result<SigningKey> {
    let minted = ActorKeypair::generate();
    let successor = SigningKey::from_bytes(minted.secret_bytes());
    let successor_hex = hex::encode(minted.secret_bytes());
    credentials.set(actor_id_hex, &successor_hex);
    principal_bundle::note_slot_write();
    match credentials.get(actor_id_hex) {
        Some(v) if v == successor_hex => Ok(successor),
        _ => bail!(
            "the credential store did not retain the successor writer key — aborting \
             before the fence (the store still names its current writer, so nothing \
             is forked)"
        ),
    }
}

/// What [`lost_slot_heal`] found and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LostSlotHeal {
    /// The slot's key is the store's writer, or the store is unstamped and
    /// the key was minted by this assembly (its first open adopts the key):
    /// nothing to heal. The common path — no section is entered.
    Consistent,
    /// The store named a writer the slot does not hold: the fence landed
    /// from it onto the slot's key, which is now the successor. The caller
    /// opens the store as that key; the pump's next pass re-authors the
    /// predecessor's tail.
    Fenced,
    /// The caller's picture of the slot is stale — a sibling changed it under
    /// the section. Restart assembly so everything resolves the slot afresh
    /// (the restarted assembly's section block mints the grant over the new
    /// key; this arm then fences onto it).
    ///
    /// ⚠ **Nothing was minted here**, which is why this is a variant of its own
    /// rather than a second cause behind [`Self::RemintedIntoSlot`]. The
    /// caller's re-mint cap counts *mints*, and a benign sibling race that
    /// burned it would leave the next genuine retired-writer-in-slot — the
    /// designed "credential backup older than a rotation" path — taking the
    /// refusal instead of healing, ending the account runtime over a race
    /// that cost nothing.
    RestartAssembly,
    /// A fresh successor was minted into the slot: restart assembly to adopt
    /// it. Three causes share the variant, and the caller's re-mint cap
    /// (refinement 10, bound (a)) counts every one — a slot that comes back
    /// with the abandoned key after a fresh mint is a credential store not
    /// retaining writes, whichever arm minted: the slot held a RETIRED
    /// writer; the slot held a key LOADED over a store with no stamped writer
    /// (refinement 11's inverse arm — the store is now stamped with the
    /// mint); or the stamped writer carried the walk's BURNT verdict
    /// (refinement 11's heal — the fence landed from it onto the mint).
    RemintedIntoSlot,
}

/// Decision 1's SECOND trigger — the lost-slot arm (charter § The store
/// device principal, refinement 10): the rotation fired on the shape *the
/// slot holds a key the stamped store does not name*, with no nest evidence
/// and no seed.
///
/// Called by the assembly on every host — seedless included — right after
/// the backend opens and before the store does. The mint-or-load before it
/// is untouched (an empty slot still mints), so the shape this meets is
/// always "slot key X, store writer W ≠ X": the key just minted over a store
/// whose slot was lost; the revocation
/// rotation's crash window between its slot write and its fence; the loser
/// of an unserialized W5.3 mint race. Refusing the store (`adopt_identity`'s
/// answer) stranded all of them for good; the fence heals all of them the
/// same way and loses nothing — W is retired, its un-pushed tail re-authors
/// under X on the next pump pass. A live predecessor (a replica restored
/// from an older backup while the original device lives on) is covered by
/// decision 3's live-predecessor bound: its later rows still ingest, and a
/// re-authored duplicate converges under its preserved `merge_meta`.
///
/// Section discipline mirrors `rotate`: the backend is the caller's, opened
/// BEFORE the section (the native `SqliteBackend::open` takes it internally);
/// the stamp is peeked outside the section, so agreement — the common path —
/// never enters it; only a disagreement does, refusing a degraded one
/// (refinement 6: the store stays exactly as it was and the next assembly
/// retries — strictly better than the store open's refusal, which also
/// changes nothing). Under the section the slot is re-read (a sibling's
/// re-mint or rotation wins → restart) and `rotate_writer_identity` re-checks
/// the stamp itself.
///
/// The one thing this never does is put a RETIRED writer back to work: a
/// slot restored from a credential backup older than a rotation names a key
/// the ruling abandoned for good (and the nest may hold its tombstone), so
/// that shape mints a fresh key into the slot and restarts. `allow_remint`
/// is the caller's cap on that restart — a slot that comes back retired
/// after a fresh mint is a credential store not retaining writes, and
/// looping on it would be refinement 7's livelock in a new coat.
pub async fn lost_slot_heal<S, X, B>(
    credentials: &S,
    actor_id_hex: &str,
    section: &X,
    backend: &B,
    held: &SigningKey,
    provenance: WriterKeyProvenance,
    allow_remint: bool,
) -> Result<LostSlotHeal>
where
    S: SecretStore + ?Sized,
    X: SlotSection,
    B: StoreBackend,
{
    let held_pub = held.verifying_key().to_bytes();
    let held_hex = fauna_core::hex32::encode(&held_pub);
    // The stamp and the walk's burnt verdict in ONE read: the pair is what
    // the journal-bound writer's arms decide on, and a fence moves both.
    let (stamped, burnt) = stamped_and_burnt_writer(backend).await?;
    let shape = match stamped {
        None => match provenance {
            // The key this assembly minted a moment ago over a store its
            // open will stamp: the first launch on a fresh machine.
            WriterKeyProvenance::Minted => return Ok(LostSlotHeal::Consistent),
            // Refinement 11's inverse arm: a key with a history this
            // journal cannot vouch for. Decided under the section below.
            WriterKeyProvenance::Loaded => HealShape::LoadedOverFresh,
        },
        Some(w) if w.0 == held_pub => match burnt {
            // Refinement 11's heal: the walk found this very journal burnt.
            Some(_) => HealShape::Burnt,
            None => return Ok(LostSlotHeal::Consistent),
        },
        Some(w) => HealShape::Disagree(w),
    };
    let Some(_section) = section.enter(
        "refusing to heal the writer unserialized (the store is untouched; the next \
         assembly retries)",
    ) else {
        bail!(
            "lost-slot heal: the slot holds {held_hex} over a store {} and the migration \
             section is unavailable — refusing to act unserialized",
            shape.describe()
        );
    };
    // Re-read the slot under the section: a sibling may have re-minted or
    // rotated between our resolve and our entry. Its picture wins; ours is
    // stale, and the caller restarts to adopt it.
    match principal_bundle::load_writer_key(credentials, actor_id_hex) {
        Some(current) if current.verifying_key().to_bytes() == held_pub => {}
        _ => {
            tracing::info!(
                "lost-slot heal: the slot changed under a sibling — restarting assembly to \
                 adopt it"
            );
            return Ok(LostSlotHeal::RestartAssembly);
        }
    }
    let stamped = match shape {
        HealShape::Disagree(stamped) => stamped,
        HealShape::LoadedOverFresh => {
            // Under the section, the two facts that tell a mint in flight
            // from a lost journal: a sibling's open may have stamped the
            // store since the peek, and the slot's own unstamped-mint
            // marker says the key was minted and no store has adopted it
            // yet (a concurrent cold assembly on this store — the app beside
            // its agent — or a crash before the first open). Either way the
            // key never published: adopt it, exactly as a first open does.
            //
            // The marker is read FIRST, the stamp second — the reverse of the
            // order that sibling writes them in. Its open stamps the store
            // and only then spends the marker, both outside this section, so
            // a marker read as spent means the stamp had already landed and
            // the read below sees it. Read the other way round, both writes
            // fit between the two reads: no stamp, then no marker — the
            // lost-journal picture over a store the sibling had just stamped,
            // and the mint below was then refused by that stamp (measured
            // under contention: two cold assemblies on one store dir, the
            // second failing here).
            let unstamped_mint = principal_bundle::writer_unstamped(credentials, actor_id_hex);
            let (stamped_now, _) = stamped_and_burnt_writer(backend).await?;
            if stamped_now.is_some_and(|w| w.0 == held_pub) {
                return Ok(LostSlotHeal::Consistent);
            }
            if let Some(other) = stamped_now {
                // A sibling stamped a DIFFERENT writer while we held this
                // one: our picture of the slot is stale — restart to
                // re-resolve it.
                tracing::info!(
                    "journal-bound writer: a sibling stamped the store with {} under us — \
                     restarting assembly",
                    other.to_hex()
                );
                return Ok(LostSlotHeal::RestartAssembly);
            }
            if unstamped_mint {
                tracing::debug!(
                    "journal-bound writer: the slot's key {held_hex} is a fresh mint no store \
                     has stamped yet (a sibling assembly's, or a crash before the first \
                     open) — adopting it"
                );
                return Ok(LostSlotHeal::Consistent);
            }
            if !allow_remint {
                bail!(
                    "journal-bound writer: the slot holds {held_hex}, a key LOADED over a \
                     store with no stamped writer, again after a fresh mint — the \
                     credential store is not retaining writes; refusing to loop"
                );
            }
            tracing::warn!(
                "journal-bound writer: the slot holds {held_hex}, a key loaded over a \
                 store with no stamped writer — the journal that authored its history is \
                 gone (a store deleted while the credential store kept the key), so the \
                 key is abandoned: minting a fresh writer, and this machine enrolls as a \
                 new fleet device (charter § The store device principal, refinement 11)"
            );
            let minted = mint_into_slot(credentials, actor_id_hex)
                .context("journal-bound writer: fresh mint over a fresh store")?;
            retire_unjournaled_writer(
                backend,
                &WriterId(held_pub),
                &WriterId(minted.verifying_key().to_bytes()),
            )
            .await
            .context("journal-bound writer: stamp the fresh store with the mint")?;
            return Ok(LostSlotHeal::RemintedIntoSlot);
        }
        HealShape::Burnt => {
            if !allow_remint {
                tracing::warn!(
                    "journal-bound writer: the store's writer {held_hex} carries the \
                     walk's burnt verdict again after a fresh mint this worker already \
                     made — not rotating again this run (a relaunch re-arms it)"
                );
                return Ok(LostSlotHeal::Consistent);
            }
            tracing::warn!(
                "journal-bound writer: the walk found this store's writer {held_hex} \
                 burnt — a feed holds rows under it that this journal never authored \
                 (a store restored from an older backup) — rotating onto a fresh mint: the burnt \
                 key is retired and its un-pushed tail re-authors on the next pump pass \
                 (charter § The store device principal, refinement 11)"
            );
            let minted = mint_into_slot(credentials, actor_id_hex)
                .context("journal-bound writer: fresh mint over a burnt journal")?;
            rotate_writer_identity(
                backend,
                &WriterId(held_pub),
                &WriterId(minted.verifying_key().to_bytes()),
            )
            .await
            .context("journal-bound writer: the fence off the burnt writer")?;
            return Ok(LostSlotHeal::RemintedIntoSlot);
        }
    };
    if retired_writers(backend)
        .await?
        .iter()
        .any(|w| w.0 == held_pub)
    {
        if !allow_remint {
            bail!(
                "lost-slot heal: the slot holds RETIRED writer {held_hex} again after a fresh \
                 mint — the credential store is not retaining writes; refusing to loop"
            );
        }
        tracing::warn!(
            "lost-slot heal: the slot holds a RETIRED writer ({held_hex}) — a credential \
             backup older than a rotation landed over a current store; minting a fresh \
             successor instead of putting the abandoned key back to work"
        );
        mint_into_slot(credentials, actor_id_hex).context("lost-slot heal: fresh mint")?;
        return Ok(LostSlotHeal::RemintedIntoSlot);
    }
    rotate_writer_identity(backend, &stamped, &WriterId(held_pub))
        .await
        .context("lost-slot heal: the fence")?;
    tracing::info!(
        "lost-slot heal: the store named writer {} but the slot holds {held_hex} — fenced \
         onto the slot's key; the predecessor is retired and its un-pushed tail re-authors \
         on the next pump pass (charter § The store device principal, refinement 10)",
        stamped.to_hex()
    );
    Ok(LostSlotHeal::Fenced)
}

/// The shape [`lost_slot_heal`] met, decided from the one-transaction read
/// before the section is entered and acted on inside it.
enum HealShape {
    /// The store names a writer the slot does not hold — refinement 10.
    Disagree(WriterId),
    /// A key LOADED from the slot over a store with no stamped writer —
    /// refinement 11's inverse arm.
    LoadedOverFresh,
    /// The slot's key is the stamped writer, and the walk found its journal
    /// burnt — refinement 11's heal.
    Burnt,
}

impl HealShape {
    fn describe(&self) -> String {
        match self {
            HealShape::Disagree(w) => format!("stamped {}", w.to_hex()),
            HealShape::LoadedOverFresh => "with no stamped writer".to_string(),
            HealShape::Burnt => "whose writer the walk found burnt".to_string(),
        }
    }
}
