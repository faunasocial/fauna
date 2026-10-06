//! Resumable rotate-mail-keys algorithm.
//!
//! Authority: `docs/goal/behavior/mail-credentials.md` § Rotation
//! and recovery. The sentinel — the incoming MSEK, on the mail custody's
//! state row (`fauna.state.mail`, `MailStateRow::pending_rotation`) — lets
//! the flow resume idempotently from any crash between sentinel-set and
//! final commit. Which credentials are still owed a re-wrap is never stored:
//! it is derived from the rows' generation markers
//! ([`MailRows::owed_rewrap`], *The generation marker*), so a credential
//! another device adds mid-rotation is re-wrapped instead of dropped.

use fauna_core::data::{MailCredential, MsekFingerprint, PriorMsekRetirement, Timestamp};
use fauna_core::mail_rows::{MailRotationSentinel, MailStateRow};
use fauna_core::secret::SecretArray32;
use fauna_mls::wrapped_blob::derive_recipient_hpke_keypair;

use crate::credential::Credential;
use crate::error::DispatchError;
use crate::machine::{MailSettingsMachine, fresh_msek};
use crate::state::SettingsStatus;
use crate::wrap;

/// Bound on how many times finalize re-drives the `msek` swap when a concurrent
/// peer write keeps reverting it. Each re-drive
/// is stamped strictly above the row it read, so it wins the next join unless
/// a peer is *still* writing strictly-newer state rows — pathological. On
/// exhaustion finalize surfaces a loud error with the sentinel intact
/// (resumable), never a silent reverted "success".
const MAX_ROTATION_FINALIZE_REDRIVES: u32 = 8;

/// Entry point for `MailSettingsAction::StartRotation`. Generates
/// a fresh MSEK, persists the resume sentinel, then runs the
/// re-wrap loop. Excludes any credentials the caller flagged as
/// compromised: both of their resting blobs are revoked at the nest
/// (mirroring `succession.rs`'s burn leg) and their rows carry the
/// soft-revoke marker, so a flagged credential can neither read
/// post-rotation mail nor keep authenticating outbound mail for its
/// submission token's remaining ~30 d.
pub(crate) async fn start_rotation(
    machine: &MailSettingsMachine,
    excluded_credentials: &[String],
) -> Result<(), DispatchError> {
    let mail = machine.mail_store().load().await?;
    if mail.msek.is_none() {
        return Err(DispatchError::InvalidState(
            "mail not enabled — nothing to rotate".into(),
        ));
    }
    // A rotation that did not finish is resumed, never replaced
    // (`ui/mail-settings.md` § Architectural rules 5). Its staged MSEK′ may
    // already be published and wrapped on the nest, so overwriting the
    // sentinel with a fresh one would strand it: mail sealed to that pubkey
    // with no client-side key left to open it. Refusing here is what makes the
    // per-app disabled rotate button a convenience rather than the only guard.
    if mail.pending_rotation.is_some() {
        machine.set_snapshot_from(&machine.mail_store().load_rows().await?);
        return Err(DispatchError::InvalidState(
            "a previous rotation didn't finish — resume it instead of starting another".into(),
        ));
    }

    let new_msek: SecretArray32 = fresh_msek().into();

    // Step (c): persist the sentinel BEFORE any nest write. The staged
    // `new_msek` is irrecoverable once the rotation provisions it, so it
    // reaches the mail custody first; the recreatable-half write restates no
    // key material, so it cannot disturb the MSEK a concurrent device holds.
    machine
        .mail_store()
        .write_state(MailStateRow {
            pending_rotation: Some(MailRotationSentinel {
                new_msek: new_msek.clone(),
            }),
            ..MailStateRow::recreatable_of(&mail)
        })
        .await?;

    // Revoke each excluded credential — both resting blobs at the nest, then
    // the row's soft-revoke marker, exactly the revoke gesture's order. A
    // marked row is outside every owed set, so the loop below never re-wraps
    // MSEK′ under it. The submission token is the security-relevant half:
    // without its delete an excluded ("compromised") credential can keep
    // authenticating outbound mail (their domain's DKIM) until its token
    // naturally expires (≤30 d) — `mail-credentials.md` § Compromised-credential
    // handling.
    for credential_id in excluded_credentials {
        machine
            .nest()
            .revoke_wrapped_mls_blob(machine.actor_id(), credential_id.clone())
            .await?;
        machine
            .nest()
            .revoke_wrapped_submission_token(machine.actor_id(), credential_id.clone())
            .await?;
        machine.mail_store().revoke(credential_id.clone()).await?;
    }

    drive_rotation_loop(machine, new_msek).await
}

/// Entry point for `MailSettingsAction::ResumeRotation`. Reads the
/// persisted sentinel and re-runs the snapshot provision + every
/// still-owed credential re-wrap. No-op if no sentinel is present
/// (defensive — the per-app UI is expected to only surface the
/// "Resume?" banner when the sentinel is set).
pub(crate) async fn resume_rotation(machine: &MailSettingsMachine) -> Result<(), DispatchError> {
    let rows = machine.mail_store().load_rows().await?;
    let Some(sentinel) = rows.state.as_ref().and_then(|s| s.pending_rotation.clone()) else {
        return Ok(());
    };
    drive_rotation_loop(machine, sentinel.new_msek).await
}

/// Steps (d)–(f) of the rotation flow under the sentinel's `new_msek`;
/// idempotent re-provisioning of the snapshot is intentional — both
/// initial-start and resume paths pass through here.
async fn drive_rotation_loop(
    machine: &MailSettingsMachine,
    new_msek: SecretArray32,
) -> Result<(), DispatchError> {
    let rows = machine.mail_store().load_rows().await?;
    machine.set_snapshot_from(&rows);
    let mail = rows.config();

    // Step (d): always re-provision the snapshot under the new MSEK,
    // and re-register the new recipient-mail pubkey. Idempotent atomic
    // replace on nest; if a crash occurred before (d) we still need to
    // do it, if after the replay is free.
    //
    // The snapshot's grace list is [new, old, …prior] (capped to 3 by
    // the builder): the new MSEK-derived recipient keypair plus the
    // outgoing one(s), so in-flight mail sealed to the pre-rotation
    // pubkey still opens. `mail.msek` is usually still the OLD MSEK
    // here (finalize at step (f) is what swaps it). One narrow exception:
    // a *resume* that runs after finalize already committed the swap but
    // before it cleared the sentinel sees `mail.msek == new_msek` — the
    // shared recipe's dedup (`provision_snapshot_for`) collapses that
    // genuine duplicate so a still-needed prior key is not pushed out of
    // the grace window. (Distinct rotations have distinct random MSEKs.)
    let mut mseks = vec![new_msek.to_array()];
    if let Some(old) = mail.msek.as_ref() {
        mseks.push(old.to_array());
    }
    mseks.extend(mail.prior_mseks.iter().map(SecretArray32::to_array));
    machine.provision_snapshot_for(&mseks).await?;

    // Publish the post-quantum ML-KEM ek alongside the new X25519 pubkey, and
    // the content-sealing-epoch schedule rederived from the new MSEK (design
    // 2026-07-18 § 5: MSEK hard-revoke resets the epoch lineage, so the
    // schedule must republish from the new root at rotation time). Same
    // publication as first-enable (`provision_mailbox_blobs`), via the shared
    // machine helpers.
    let (_, recipient_pubkey) = derive_recipient_hpke_keypair(&new_msek);
    let mlkem_ek = MailSettingsMachine::recipient_mlkem_ek_to_publish(&new_msek);
    let epoch_keys = MailSettingsMachine::epoch_seal_keys_to_publish(&new_msek);
    machine
        .nest()
        .provision_recipient_mls_pubkey(
            machine.actor_id(),
            recipient_pubkey,
            mlkem_ek,
            Some(epoch_keys),
        )
        .await?;

    // Step (e): re-wrap MSEK′ under each credential still owed one — the live
    // rows not yet naming MSEK′'s generation, **re-derived from a fresh read
    // before every provision** (marked rows are outside the set, so a burn or
    // a revoke landing mid-loop is skipped, and a credential another device
    // adds mid-loop is picked up). After each provision the row's generation
    // marker records the wrap (step (e)(iii)), which is also the checkpoint a
    // resume continues from.
    let fingerprint = MsekFingerprint::of(&new_msek);
    loop {
        let rows = machine.mail_store().load_rows().await?;
        let Some(credential) = rows.owed_rewrap(&new_msek).first().map(|c| (*c).clone()) else {
            break;
        };
        machine.set_status(SettingsStatus::RotationInProgress {
            credentials_remaining: rows.owed_rewrap(&new_msek).len() as u64,
        });
        let blob = wrap::seal_msek_under_credential(
            &new_msek,
            &machine.actor_id(),
            &credential.credential_id,
            &wrap_credential(&credential)?,
        )?;
        machine.nest().provision_wrapped_mls_blob(blob).await?;
        if !machine
            .mail_store()
            .mark_wrapped(credential.credential_id.clone(), fingerprint)
            .await?
        {
            // Nothing moved: the row was marked (burned/revoked) since the
            // read, or already names MSEK′. Either way the next read no longer
            // owes it — unless the store is refusing to move it at all, which
            // would loop forever; re-read and stop if it is still owed.
            let still_owed = machine
                .mail_store()
                .load_rows()
                .await?
                .owed_rewrap(&new_msek)
                .iter()
                .any(|c| c.credential_id == credential.credential_id);
            if still_owed {
                return Err(DispatchError::InvalidState(format!(
                    "rotation could not record credential {} as re-wrapped; the \
                     sentinel is left set so the rotation can resume",
                    credential.credential_id
                )));
            }
        }
    }

    // Step (f): finalize — swap `msek` to `new_msek` and clear the sentinel,
    // **verifying the swap actually committed by reading the row back**
    // rather than trusting the write.
    //
    // Why the verify is load-bearing: the new
    // recipient pubkey is already published to nest (step d) and `new_msek` is
    // already wrapped under every credential (step e), so senders are already
    // sealing incoming mail to `new_msek`'s HPKE key. But the state row's MSEK
    // join (a deliberate-rotation latest-wins on the row's stamp —
    // `mail-credentials.md` § Cross-device finalize race) lets a concurrent
    // peer device whose state row carries the pre-rotation `msek` under a
    // newer stamp **revert our swap** when the walk merges it in — and the
    // displaced MSEK is NOT unioned into the grace window (the window unions
    // the two sides' `prior_mseks` only), so `new_msek` would be absent from
    // `msek`, `prior_mseks` and (once cleared) the sentinel, with NO
    // client-side flow to recover it from the nest wrapped blobs (only the
    // mail bridge unwraps them, session-local). The next rotation's grace list
    // would then omit it, eventually making mail sealed to the published new
    // pubkey undecryptable: silent mail loss from a benign cross-device race.
    //
    // So finalize loops: it keeps a sentinel carrying `new_msek` set across
    // every swap attempt (so `new_msek` stays recoverable from the custody
    // until the swap is durable), re-drives with a fresh stamp when the read
    // back shows a peer reverted the swap, and only clears the sentinel once
    // `msek == new_msek` is confirmed stored — and re-verifies after the
    // clear. On a persistent racer it errors out with the sentinel intact
    // (the rotation stays resumable), never a silent reverted success.
    // Idempotent on resume: a read already showing `msek == new_msek` just
    // clears the sentinel.
    let mut redrives = 0u32;
    loop {
        let mail = machine.mail_store().load().await?;

        if mail.msek.as_ref() == Some(&new_msek) {
            if mail.pending_rotation.is_some() {
                // The clear restates no key material (the recreatable half
                // alone), so it cannot itself revert the swap; a peer's stale
                // row still can, which is why the read back decides.
                machine
                    .mail_store()
                    .write_state(MailStateRow {
                        pending_rotation: None,
                        ..MailStateRow::recreatable_of(&mail)
                    })
                    .await?;
                let stored = machine.mail_store().load().await?;
                if stored.msek.as_ref() == Some(&new_msek) {
                    break;
                }
                // A concurrent peer reverted the swap (and the clear with it).
                // Re-drive (bounded): the next iteration re-establishes
                // `new_msek` + the sentinel before attempting the clear again.
                redrives += 1;
                if redrives >= MAX_ROTATION_FINALIZE_REDRIVES {
                    return Err(DispatchError::InvalidState(format!(
                        "rotation finalize could not durably clear the sentinel \
                         after {redrives} attempts — a concurrent device keeps \
                         reverting the swap"
                    )));
                }
                continue;
            }
            break;
        }

        // Not yet swapped. Retain the outgoing MSEK in the grace window (the
        // window's join keeps the cap-2 newest by retirement), set MSEK :=
        // new, record the retirement instant keyed by the retired MSEK (the
        // bounded-mail mint's generation seal intervals — content-sealing-
        // epochs amendment 2026-07-19; idempotent across re-drives: an
        // existing entry for `old` keeps its first-recorded instant), and KEEP
        // the sentinel carrying `new_msek` so it stays recoverable until
        // confirmed.
        let mut prior_mseks = Vec::new();
        let mut prior_msek_retirements = Vec::new();
        if let Some(old) = mail.msek.clone() {
            let retired_at_unix = mail
                .prior_msek_retired_at(&old)
                .unwrap_or_else(|| Timestamp::now_secs().max(0) as u64);
            prior_msek_retirements.push(PriorMsekRetirement {
                msek: old.clone(),
                retired_at_unix,
            });
            prior_mseks.push(old);
        }
        machine
            .mail_store()
            .write_state(MailStateRow {
                msek: Some(new_msek.clone()),
                prior_mseks,
                prior_msek_retirements,
                pending_rotation: Some(MailRotationSentinel {
                    new_msek: new_msek.clone(),
                }),
                ..MailStateRow::recreatable_of(&mail)
            })
            .await?;
        let stored = machine.mail_store().load().await?;
        if stored.msek.as_ref() == Some(&new_msek) {
            // Swap committed; the next loop iteration clears the sentinel.
            continue;
        }

        // A concurrent peer write reverted the swap. Re-drive (bounded).
        redrives += 1;
        if redrives >= MAX_ROTATION_FINALIZE_REDRIVES {
            return Err(DispatchError::InvalidState(format!(
                "rotation finalize could not commit the msek swap after {redrives} \
                 attempts — a concurrent device keeps reverting it; the sentinel is \
                 left set so the rotation can resume"
            )));
        }
    }
    machine.refresh_snapshot().await?;

    // The MSEK swap is durably committed and the sentinel cleared. Best-effort
    // heal of any outstanding bounded mail grants across the retired generation
    // (design § 5 amendment 2026-07-19): re-wrap each grant's window under the
    // post-rotation generation set and drive an equal-end renew. Log-only — a
    // failed heal never fails the rotation (§ 3 degradation posture; the grant
    // heals at its next renew). The early exit inside covers the common
    // no-bounded-grants path with no discovery RPC.
    machine.heal_outstanding_bounded_grants().await;
    Ok(())
}

/// The wrap-side `Credential` for a persisted credential row. The secret
/// bytes round-trip from the row's `secret` per the § Decision (c)
/// ratification (credentials are persisted to allow unattended rotation).
///
/// A row of a kind this build does not name refuses the rotation: its blobs
/// cannot be re-wrapped here, and the sentinel stays set so a build that knows
/// the kind can resume.
fn wrap_credential(row: &MailCredential) -> Result<Credential, DispatchError> {
    Credential::of_row(row)
}
