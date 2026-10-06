//! The enrollment ceremony's nest legs, the sign-out retirement, the fleet
//! bootstrap rows and the generation writer-door trust — the driver steps that act as
//! *this device* on the fleet plane (`account-replica-posture.md` § The store
//! device principal; `account-data-taxonomy.md` § The generation machinery).

use std::time::Duration;

use anyhow::Result;
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::WriterId;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_protocol::merge_policy::{KIND_DEVICE_SET, KIND_ESCROW_TARGET};
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::account_state_plane::{AccountStatePlane, ItemId};
use crate::principal_custody::EnrollmentRefusal;

use super::pass::FleetWriter;

/// What the enrollment-registration step found (one entry per pump
/// pass in [`PumpReport::enrollment`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollmentPass {
    /// The slot carries no grant — the assembly's mint half failed, or the
    /// host holds no seed; nothing to register. Healed at the next
    /// seed-holding assembly.
    Unenrolled,
    /// The slot's grant is already registered (the content-addressed latch
    /// matches) — no RPC spent.
    Current,
    /// Both nest legs succeeded this pass; the latch now records this grant
    /// **and the row it went on**.
    Registered,
    /// **The machine was removed from the account.** Either its own
    /// `fauna.state.device-set` row reads `Removed` in merged state — read
    /// first, ahead of the latch, with no RPC ([`PumpReport::own_row_removed`]
    /// says it was this) — or the nest's revocation
    /// memory (`fauna.sync.device_grant_revoked`) refuses this principal's
    /// key permanently — the user deleted its device row — so no retry of
    /// this pass can ever succeed and the runtime must not latch as if one
    /// had (decision 4: never a silent latched death). Reviving the machine
    /// is a *fresh enrollment* under a successor principal: the
    /// remedy — the automatic successor mint — runs at a **ceremony-capable
    /// sign-in** (a seed-holding assembly's succession probe; a seed-holding
    /// worker seeing this answer reassembles, since a reassembly IS such a
    /// ceremony), never mid-pump — this pump may be seedless (the app-dead
    /// agent), and a seedless host can mint nothing.
    ///
    /// [`PumpReport::own_row_removed`]: super::PumpReport::own_row_removed
    RemovedFromAccount,
    /// **This machine signed out.** Its own device-set row reads `Removed`
    /// and the store's sign-out stamp names this writer
    /// (`AccountStore::signed_out_writer`): the row is the sign-out's own
    /// severance, not a removal by the fleet. Nothing registers and nothing
    /// rotates — the erase follows, and a successor minted here would leave
    /// the signed-out machine a fleet member under a key nobody holds.
    /// Answered by every runtime on the store, the signing-out one included
    /// (a pass can run between its retirement and its shutdown). A
    /// seed-holding host start clears the stamp, after which the same row
    /// reads [`Self::RemovedFromAccount`] and heals.
    SignedOut,
    /// **The account is at its tier's device cap.** The nest refused the
    /// machine's `fauna.sync.register` with `fauna.sync.device_limit_exceeded`
    /// (`devices.md` § Step 4) and wrote nothing, so there is no row to latch
    /// and no retry that clears it on its own: a slot frees only when the
    /// user removes a device or the admin raises the tier. Unhealthy (never
    /// re-arms the rotation cap), never latched, and retried on the ordinary
    /// cadence — the refusal is one cheap RPC the nest answers without
    /// writing. Recorded in the credential slot
    /// ([`EnrollmentRefusal::DeviceLimitExceeded`]) so the app's Devices page
    /// can render it even when the co-located agent's pump is the process
    /// that met it (`ui/devices.md` § Errors & edge cases).
    DeviceLimitExceeded,
}

/// The enrollment step's answer: the verdict, and whether a
/// [`EnrollmentPass::RemovedFromAccount`] was read off this machine's own
/// `Removed` device-set row ([`PumpReport::own_row_removed`]) rather than
/// answered by the nest.
///
/// [`PumpReport::own_row_removed`]: super::PumpReport::own_row_removed
pub(crate) struct EnrollmentStep {
    pub(crate) pass: EnrollmentPass,
    pub(crate) own_row_removed: bool,
}

impl From<EnrollmentPass> for EnrollmentStep {
    fn from(pass: EnrollmentPass) -> Self {
        Self {
            pass,
            own_row_removed: false,
        }
    }
}

/// What a **sign-out's** nest-side retirement of this machine's enrollment
/// found ([`AccountStoreHandle::shutdown_for_sign_out`] —
/// `sync-agent-credentials.md` § Credential model → *The signed-out
/// reconcile*, the nest-side leg, built 2026-09-14).
///
/// Best-effort by design, like every other half of a sign-out: the answer is
/// logged and reported, never a reason to refuse the sign-out. The local
/// erase that follows ends the machine's use of the key regardless; what this
/// leg adds is that the nest forgets the credential too — the named row's
/// grant is cleared (the row itself stays, carrying the user's label) and
/// every bearer the key minted dies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrollmentRetirement {
    /// The slot carried no grant — nothing was ever registered from this
    /// runtime, so there is nothing to retire.
    NotEnrolled,
    /// The nest confirmed the retirement. `cleared` is whether a stored grant
    /// was actually cleared (`false` is success: the key is tombstoned either
    /// way); `sessions_revoked` is how many bearers the key had minted.
    Retired {
        cleared: bool,
        sessions_revoked: u32,
    },
    /// The nest did not confirm — a transport
    /// failure or the retirement budget elapsing. The sign-out proceeds; the
    /// grant stays on the named row until the user deletes the device from
    /// the devices page, which tombstones the key.
    Deferred(String),
}

/// How long a sign-out waits for the nest to confirm the enrollment's
/// retirement before erasing anyway. One authenticated round trip on the
/// session the app is signing out of; the whole stop the hosts run is
/// budgeted at five seconds, and this leaves the store's own shutdown its
/// share of it.
pub const ENROLLMENT_RETIRE_BUDGET: Duration = Duration::from_secs(3);

/// The generation writer-door trust this runtime assembles: the root
/// is this account, `prior` is the caller's **attested** predecessor set
/// ([`AccountRuntimeParams::attested_predecessors`]) and `trusted_holders` is
/// whatever escrow holders the app pinned. The fleet view verifies
/// enrollment certs against the root alone; `prior` feeds the group
/// authority view only (`GenerationTrust::prior`).
///
/// The ONE place a `GenerationTrust` is built for production, so the source
/// of every half is visible in one signature: nothing here reads a
/// device-local replica's `prior_actor_ids` — it is
/// writer-asserted and carried across a succession unmarked, and reading it
/// as signer trust was the finding (`account-data-taxonomy.md` § The
/// generation machinery → *The source of `prior`*, ruled 2026-09-13). An
/// attested set that is empty is fail-*safe*, not fail-open: the group
/// authority view then admits no predecessor-signed authority rather than
/// anything extra.
pub fn r14_trust(
    actor_id_hex: &str,
    attested_predecessors: &[ActorId],
    trusted_escrow_holders: &[[u8; 32]],
) -> Result<crate::generation_tip::GenerationTrust> {
    let root = fauna_core::hex32::decode(actor_id_hex)
        .map_err(|e| anyhow::anyhow!("account runtime: actor id is not 32 hex bytes: {e}"))?;
    Ok(crate::generation_tip::GenerationTrust {
        root: ActorId(root),
        prior: attested_predecessors.to_vec(),
        trusted_holders: trusted_escrow_holders.to_vec().into(),
    })
}

/// The two machinery rows a seed-holding surface owes its own account before
/// any generation can exist: this device's fleet enrollment, and the account's
/// escrow target.
///
/// Both are pure functions of key material the runtime already holds, so
/// neither is a user choice and neither gets an app surface: they are the
/// product invariants' bucket (1) — "not chosen by any human" — and a settings
/// screen for either would be configuration theatre.
pub struct FleetBootstrapRows {
    /// This device's id — the enrollment's cell, and the key its
    /// self-signature verifies under.
    device_id: [u8; 32],
    /// `fauna.state.device-set` keyed by this device id, self-signed.
    enrollment: (ItemId, Vec<u8>),
    /// `fauna.state.escrow-target` — identity-derived, published once.
    escrow_target: (ItemId, Vec<u8>),
}

/// Build [`FleetBootstrapRows`] from the account root and this device's writer
/// key.
///
/// The enrollment carries a root-signed `DeviceAuthorization` over **this**
/// device id, which is what the reader-side [`fauna_core::generation::FleetView`]
/// verifies before counting the device a member (charter § The generation
/// machinery → the device-set kind's *Authority*). The runtime can produce it
/// because it is a seed-holding surface by contract
/// ([`AccountRuntimeParams`]) — it holds both the account root and the device
/// principal.
///
/// **Capabilities are deliberately empty.** The view does not consult them
/// (membership is bundle-level — sealing under the fleet-only gen-0 branch is
/// the possession proof), and `Capability` is a real authorization surface, so
/// naming one here would grant a power this cert has no business conveying.
pub fn fleet_bootstrap_rows(
    actor_keypair: &ActorKeypair,
    writer_key: &ed25519_dalek::SigningKey,
) -> Option<FleetBootstrapRows> {
    let device_id = writer_key.verifying_key().to_bytes();
    let cert = fauna_core::data::DeviceAuthorization {
        actor_id: actor_keypair.actor_id(),
        device_key: device_id,
        capabilities: Vec::new(),
        created_at: fauna_core::data::Timestamp::now(),
        // No expiry, for `build_principal_grant`'s reason: an expiring cert would
        // silently drop this device out of the fleet — and out of every future
        // mint's wrap-target set — with no signal. Removal is the control.
        expires_at: None,
    };
    let build = || -> anyhow::Result<FleetBootstrapRows> {
        let (bytes, env) = fauna_core::encoding::sign_envelope(actor_keypair, &cert)?;
        let authorization = fauna_core::encoding::canonical_encode(
            &fauna_core::encoding::EmbedAsBytes::from_signed(bytes, env),
        )?;
        // Self-signed under the device id (the KEM public half derived from
        // the same secret inside the builder) — the one production shape of
        // an enrollment since the 2026-09-16 ruling; `fleet_bootstrap`
        // re-publishes it over any row at its cell that is not its own.
        let enrollment = fauna_core::encoding::canonical_encode(
            &fauna_core::generation::sign_device_enrollment(
                writer_key,
                authorization,
                i64::try_from(fauna_core::data::Timestamp::now_millis_or_zero()).unwrap_or(0),
            ),
        )?;
        // At THIS identity's key: after a succession the runtime runs as the
        // successor, so write-if-absent below publishes the successor's own
        // target beside the predecessor's Immutable row rather than skipping
        // it (the succession rider, 2026-09-28).
        let target = crate::generation_mint::escrow_target_entry(actor_keypair.secret_bytes())?;
        Ok(FleetBootstrapRows {
            device_id,
            enrollment: (
                ItemId {
                    kind: KIND_DEVICE_SET.into(),
                    key: fauna_core::hex32::encode(&device_id),
                },
                enrollment,
            ),
            escrow_target: (
                ItemId {
                    kind: KIND_ESCROW_TARGET.into(),
                    key: target.key,
                },
                target.value,
            ),
        })
    };
    match build() {
        Ok(rows) => Some(rows),
        Err(e) => {
            // Non-fatal by the same rule as every other assembly-adjacent
            // derivation here: the class-2 legs must keep pumping. What is lost
            // is generation sealing, which then refuses precisely rather than
            // silently falling back to gen-0 keys.
            tracing::warn!("account runtime: fleet bootstrap rows not built ({e:#})");
            None
        }
    }
}

/// Write the bootstrap rows this replica does not have yet.
///
/// **Write-if-absent — where "present" means *this device's own verifying
/// row*, not any row at all.** Re-writing an enrollment on every start would
/// churn the feed for nothing, so a merged `Enrolled` that self-verifies at
/// this device's cell is left alone. But the mere presence of a merged row
/// was never proof it was ours: a `BackupKey` holder can file a forgery at
/// this id (the finding), signed with junk or not signed at all.
/// Either is re-published over, signed — count-neutral (a
/// same-writer put replaces the writer's own row at the item), after which
/// the key-aware join keeps the signed row at every reader. A device that has
/// been *removed* must not re-announce itself — a merged `Removed` at its own
/// cell is honoured as a stop — though the plane makes that safe either way:
/// `Removed` is absorbing per id, so a re-enrollment on a removed id merges
/// back to `Removed` at every reader and add-wins resurrection is
/// unrepresentable. The escrow target keeps the plain write-if-absent: it is
/// `Immutable`, identity-derived, and every seed-holding writer produces the
/// same bytes.
///
/// **Local only — nothing is sent here** ([`AccountStatePlane::put_local`]).
/// The bootstrap runs inside `start()`'s readiness barrier, and until `ready`
/// the host holds no handle: a sign-out landing then cannot cut anything and
/// waits the whole assembly out under the hosts' stop budget
/// (`fauna_client_account_runtime::ACCOUNT_RUNTIME_STOP_BUDGET`). A nest leg
/// here — slow exactly when a fresh sign-in's principal is still waiting for
/// its grant — spent that budget by itself, and the erase then met a
/// still-open store (`apps/account-scoping.md` § Erasure follows scope). The
/// rows publish on the prologue's publish step, the first thing the pass
/// does, where a sign-out cuts it after `SIGN_OUT_PASS_GRACE`.
///
/// Errors are logged, never fatal: an account whose bootstrap has not landed
/// simply keeps refusing `GenerationTip` sealing with the precise reason.
pub async fn fleet_bootstrap<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    rows: Option<FleetBootstrapRows>,
) where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let Some(rows) = rows else { return };
    let device_id = rows.device_id;
    for (is_enrollment, (item, value)) in [(true, rows.enrollment), (false, rows.escrow_target)] {
        match store.state(&item.kind, &item.key).await {
            Ok(Some(row)) if !row.tombstone => {
                if !is_enrollment || enrollment_needs_no_republish(&row.value, &device_id) {
                    continue;
                }
                tracing::info!(
                    "account runtime: fleet bootstrap re-publishes this device's enrollment \
                     signed (the merged row at its cell is not its own verifying one)"
                );
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    "account runtime: fleet bootstrap read ({}): {e:#}",
                    item.kind
                );
                continue;
            }
        }
        if let Err(e) = fleet.put_local(&item, value, None).await {
            tracing::warn!(
                "account runtime: fleet bootstrap write ({}): {e:#}",
                item.kind
            );
        }
    }
}

/// Does the merged device-set row at this device's own cell make a
/// re-publish pointless? Yes for a `Removed` (never re-announce) and for an
/// `Enrolled` that self-verifies at the cell (already ours, already signed);
/// no for an unsigned row, a forgery, or bytes that do not decode.
fn enrollment_needs_no_republish(merged: &[u8], device_id: &[u8; 32]) -> bool {
    match fauna_core::encoding::canonical_decode::<fauna_core::generation::DeviceSetRecord>(merged)
    {
        Ok(fauna_core::generation::DeviceSetRecord::Removed { .. }) => true,
        Ok(record) => record.self_verifies_at(device_id),
        Err(_) => false,
    }
}

/// The sign-out leg of the enrollment ceremony: retire the grant the slot
/// carries, nest-side, by proof of possession of the writer key
/// (`fauna.sync.device_grant.revoke`, the self arm — `sync-agent-credentials.md`
/// § Credential model → the RULED 2026-09-28 block, decision 4: a revoke
/// clears the named row's grant columns and keeps the row).
///
/// Rides the app-session requester like the registration legs, because the
/// principal's own session is the one being revoked. Nothing here touches the
/// slot: the erase that follows a sign-out sweeps it whole, and a slot that
/// survives a failed erase reads as removed-from-account at the next
/// assembly, which is the loud state decision 4 of the RULED 2026-08-15
/// block wants rather than a silent one.
pub(crate) async fn retire_enrollment<R>(fleet_writer: FleetWriter<'_, R>) -> EnrollmentRetirement
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let Some(loaded) = fleet_writer.slot.device_authorization() else {
        return EnrollmentRetirement::NotEnrolled;
    };
    let req = fauna_client_sync::build_grant_revoke_request(
        &loaded.authorization.actor_id.0,
        fleet_writer.key,
    );
    let sync = fauna_client_sync::SyncClient::new(fleet_writer.session_rpc.clone());
    match sync.device_grant_revoke(req).await {
        Ok(reply) => {
            tracing::info!(
                cleared_on_nest = reply.revoked,
                sessions_revoked = reply.sessions_revoked,
                "enrollment: retired this machine's principal at sign-out — the nest forgets \
                 the credential; the machine's named row stays"
            );
            EnrollmentRetirement::Retired {
                cleared: reply.revoked,
                sessions_revoked: reply.sessions_revoked,
            }
        }
        Err(e) => {
            // The consequence is a grant that outlives this sign-out on the
            // named row until the user deletes the device.
            tracing::warn!(
                error = %e,
                "enrollment: the nest did not confirm the sign-out retirement — the machine's \
                 grant may survive this sign-out"
            );
            EnrollmentRetirement::Deferred(e.to_string())
        }
    }
}

/// The enrollment ceremony's retryable nest legs (`sync-agent-credentials.md`
/// § Credential model → the RULED 2026-09-28 block, decisions 1 and 3): make
/// the machine's **named** `sync_devices` row — the app's own derived id,
/// [`FleetWriter::enrollment_target`] — exist and carry the slot's
/// `RenewBearer` grant, so any co-located process can mint its per-process
/// bearer over `fauna.auth.device_handshake` with the writer key alone. The
/// store principal is the machine's only renewal credential, so there is
/// nothing on that row this registration could displace.
///
/// The `ensure_published` discipline, adapted: idempotent per pass, latched
/// content-addressed in the slot (`grant_registration_recorded` — a
/// re-minted grant mismatches and re-registers; an unchanged one costs no
/// RPC). Both legs ride the caller's ordinary requester — the app session:
/// they are authenticated kinds, and the grant they install is what the
/// *next* connection can authenticate with instead.
///
/// A row that does not exist yet registers under
/// [`fauna_client_sync::SELF_REGISTER_LABEL`] with no seal: no human has named
/// this machine yet (a label is a user choice — there is no hostname fallback
/// by design), and the provisioner's labeled register supersedes it. A row
/// that **does** exist gets only the grant columns — never a re-registration,
/// which would replace the user's device name with the placeholder label
/// (label custody).
///
/// **The latch is a memory about one nest replica**
/// (`account-replica-posture.md` § The store device principal): `latch_void`
/// — the pass is bound to a replica other than the settled one, the bind
/// verification (`crate::bind_leg`) — skips the latch and runs the grant-first
/// probe against the bound nest, which re-creates the row on a second or
/// rebuilt nest and answers "revoked" on one that remembers a deletion. A
/// latch the device handshake's `not_registered` answer voided
/// ([`PrincipalCustody::void_grant_registration`]) reads as no latch and takes
/// the same probe. Either way, a machine whose own device-set row reads
/// `Removed` in merged state never re-registers: a rebuilt box has lost its
/// revocation memory, so the refusal comes from the device's side, from the
/// rows the fleet re-pushes. That read is the step's first, ahead of the
/// latch, and it is principal succession's third trigger: a seed-holding
/// worker reassembles on it and the assembly mints the successor
/// ([`EnrollmentStep::own_row_removed`]) — unless the store says the row is
/// this machine's own sign-out ([`EnrollmentPass::SignedOut`]).
///
/// [`PrincipalCustody::void_grant_registration`]: crate::principal_custody::PrincipalCustody::void_grant_registration
pub(crate) async fn ensure_enrollment_registered<B, R>(
    store: &AccountStore<B>,
    fleet_writer: FleetWriter<'_, R>,
    latch_void: bool,
) -> Result<EnrollmentStep>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let Some(loaded) = fleet_writer.slot.device_authorization() else {
        // Silent until 2026-08-26, and the silence is expensive: this arm
        // means no `RenewBearer` grant will EVER reach the nest from this
        // process, so every co-located device-principal client is refused
        // `fauna.auth.not_registered` forever, its data path never connects,
        // and — because the escrow deposit is the plane's only synchronous
        // nest call — every `GenerationTip` origination refuses while the
        // rest of the plane still reads healthy. A seedless host sitting here
        // is the ratified steady state ("unenrolled until an app signs in"),
        // which is exactly why the arm cannot be a warn; but it must at least
        // be *findable*, or the whole chain has no first link in any log.
        tracing::debug!(
            "enrollment: this runtime's principal slot carries no device authorization, so \
             the grant legs are skipped — no co-located process can mint a bearer over \
             `fauna.auth.device_handshake` until a seed-holding assembly mints one"
        );
        return Ok(EnrollmentPass::Unenrolled.into());
    };
    let target = fleet_writer.enrollment_target.to_string();
    if target.is_empty() {
        // A wiring bug in the host, never a supported state: the app's own
        // derived id is always resolvable once it has signed in. Loud and
        // unlatched, so the fix heals on the next pass.
        return Err(anyhow::anyhow!(
            "enrollment: no enrollment target — the host passed an empty device id"
        ));
    }
    let sync = fauna_client_sync::SyncClient::new(fleet_writer.session_rpc.clone());

    // A removed machine never re-registers, on any replica — and the read
    // comes AHEAD of the latch (decision 1's third trigger): the latch is
    // content-addressed on the grant wire, and a removal written on the fleet
    // plane changes no byte of it, so a latch consulted first answered
    // `Current` for ever and the removed machine never noticed.
    if own_row_removed(store, fleet_writer.key).await? {
        // A sign-out writes this same row for its own machine. The store's
        // stamp — local, never published, landed before the row — is what
        // tells the two apart; the row's `removed_by` is any writer's claim.
        let own = WriterId(fleet_writer.key.verifying_key().to_bytes());
        if store.signed_out_writer().await? == Some(own) {
            tracing::info!(
                target_row = %target,
                "enrollment: this machine's own device-set row reads Removed and the store \
                 says a sign-out wrote it — nothing registers and nothing rotates"
            );
            return Ok(EnrollmentPass::SignedOut.into());
        }
        tracing::error!(
            target_row = %target,
            "enrollment: this machine's own device-set row reads Removed — it was removed \
             from the account and does not register itself again, on this nest or any other"
        );
        return Ok(EnrollmentStep {
            pass: EnrollmentPass::RemovedFromAccount,
            own_row_removed: true,
        });
    }
    // Already on the machine's named row: the steady state, and it costs no
    // RPC. A latch naming any other row (or none — a re-ceremony minted a fresh
    // grant) registers onto the named row below; so does any latch while the
    // pass is bound to a replica the latch was never learned from.
    if !latch_void && fleet_writer.slot.grant_registration_row().as_deref() == Some(target.as_str())
    {
        return Ok(EnrollmentPass::Current.into());
    }

    // Register-create only when the row is absent. The grant register is tried
    // FIRST and its typed not-found is what tells us the row does not exist —
    // one RPC on the common path, and no `sync.register` over a row whose
    // label a human chose.
    match sync
        .device_grant_register(target.clone(), loaded.wire.clone())
        .await
    {
        Ok(_) => {}
        Err(e) if fauna_client_sync::is_device_grant_no_device(&e) => {
            match sync
                .register(target.clone(), fauna_client_sync::SELF_REGISTER_LABEL, None)
                .await
            {
                Ok(_) => {}
                // The tier device-cap refusal: nothing was written, and no
                // retry clears it — only a freed slot does. Named as its own
                // verdict rather than folded into the pass's error list (which
                // no log prints and no page reads), and recorded in the slot
                // so the Devices page can tell the user the remedy. Warn, not
                // debug: unlike an offline nest this does NOT heal by itself.
                Err(e) if fauna_client_sync::is_device_limit_exceeded(&e) => {
                    fleet_writer
                        .slot
                        .record_enrollment_refused(EnrollmentRefusal::DeviceLimitExceeded);
                    tracing::warn!(
                        target_row = %target,
                        "enrollment: the nest refused to register this machine — the account \
                         already has as many devices as its tier allows ({}); remove a device \
                         under Settings → Devices or ask the admin for a bigger tier; retried \
                         next pass",
                        fauna_protocol::RpcError::CODE_SYNC_DEVICE_LIMIT_EXCEEDED
                    );
                    return Ok(EnrollmentPass::DeviceLimitExceeded.into());
                }
                Err(e) => return Err(anyhow::anyhow!("sync.register: {e}")),
            }
            sync.device_grant_register(target.clone(), loaded.wire)
                .await
                .map_err(|e| anyhow::anyhow!("device_grant.register: {e}"))?;
        }
        Err(e) if fauna_client_sync::is_device_grant_revoked(&e) => {
            // Decision 4's loud removed-from-account state. The nest remembers
            // that this key was revoked by a device deletion, so no retry can
            // ever succeed: the machine was removed from the account and only
            // a successor principal revives it. Reported rather than
            // retried silently, and deliberately NOT latched — a latch here
            // would turn a recoverable state into the silent latched death
            // decision 4 exists to prevent.
            tracing::error!(
                target_row = %target,
                "enrollment: this machine's principal was revoked by a device \
                 deletion — the machine is removed from the account and will \
                 not renew until it is enrolled afresh"
            );
            return Ok(EnrollmentPass::RemovedFromAccount.into());
        }
        Err(e) => return Err(anyhow::anyhow!("device_grant.register: {e}")),
    }
    fleet_writer.slot.record_grant_registered_on(&target);
    Ok(EnrollmentPass::Registered.into())
}

/// Whether this machine's own `fauna.state.device-set` row reads `Removed` in
/// merged state.
async fn own_row_removed<B: StoreBackend>(
    store: &AccountStore<B>,
    writer_key: &ed25519_dalek::SigningKey,
) -> Result<bool> {
    let cell = fauna_core::hex32::encode(&writer_key.verifying_key().to_bytes());
    Ok(match store.state(KIND_DEVICE_SET, &cell).await? {
        Some(row) if !row.tombstone => matches!(
            fauna_core::encoding::canonical_decode::<fauna_core::generation::DeviceSetRecord>(
                &row.value
            ),
            Ok(fauna_core::generation::DeviceSetRecord::Removed { .. })
        ),
        _ => false,
    })
}
