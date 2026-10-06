//! The post-succession **aftermath**'s MSEK leg — the one that ends the
//! successor's mailbox exposure.
//!
//! Authority: `docs/goal/behavior/succession-aftermath.md` § Re-key scope, the
//! MSEK row (ratified 2026-08-05) and `docs/goal/behavior/mail-credentials.md`
//! § Rotation and recovery → *Succession*, which owns the mechanics deltas from
//! an ordinary hard revoke. Read those, not this module, for the ruling.
//!
//! # What is actually broken until this runs
//!
//! A succession does not touch the mail plane at all. The pre-succession seed
//! holder — the thief the whole feature answers — read the mail custody, so they hold
//! the MSEK, every retained `prior_mseks` generation, **and every credential's
//! raw `secret` bytes**. Those secrets are what unwrap the on-nest
//! wrapped-MSEK blobs, so without this leg they keep reading the *successor's*
//! mail: everything sealed after the ceremony, silently, for as long as the
//! successor keeps using the mailbox. The ceremony's handle move cuts their
//! bridge AUTH (`<handle>+<credential_id>` resolves the actor at AUTH time), so
//! the leg is not racing them — but nothing else ever closes the hole.
//!
//! # Why nothing survives, and why that is not the adjudication shape
//!
//! § Adjudicating what the aftermath carries across says: re-mint, then *mark*
//! the row un-adjudicated and let the owner Keep or Remove. That instrument is
//! for rows whose **target** a thief may have chosen — a planted destination, a
//! planted grantee. It is the wrong instrument here, because the compromise is
//! of the *secret material* of rows the owner legitimately created: a user
//! cannot tell their own iPhone Mail credential from their own iPhone Mail
//! credential the thief also holds — those are the same row — and a *Keep*
//! cannot re-secret stolen bytes. So the burn is total, and **uniform for theft
//! and loss** (the statement carries no reason, a self-declared one could not be
//! trusted exactly where it matters, and the costs are asymmetric: the
//! loss-recovering user re-adds MUAs, bounded and visible; the theft case with
//! surviving credentials loses the whole mailbox, silently, forever).
//!
//! # The idempotency device, and why it is a record rather than a derivation
//!
//! The sibling legs need no state at rest: a grant's latest event is *signed*,
//! so its signer names its era. The mail plane has no such evidence — an MSEK is
//! raw key bytes, a credential secret is a raw password — so nothing in
//! `MailConfig` distinguishes a predecessor-era row from one the successor
//! minted afterwards. Hence [`fauna_core::data::MailConfig::succession_burns`]
//! (on the mail custody's state row, `fauna.state.mail`): one union-merged
//! entry per burned predecessor, and *owed* is `attested predecessors ⊄ burns`.
//! Two consequences worth stating out loud:
//!
//! * **The record lands even when there is nothing to burn** (mail disabled).
//!   Skipping it would leave the leg owed forever, and the credentials a later
//!   "Enable mail" mints — fresh secrets the predecessor never saw — would be
//!   burned as if they were stolen.
//! * **The leg cannot prove a row is post-succession**, so it burns every live
//!   row it finds. The window is one sign-in wide (a credential added on
//!   another device between the ceremony and this pass), and over-burning costs
//!   a re-add where under-burning costs the mailbox — the same asymmetry the
//!   ruling itself turns on.
//!
//! # Where it runs, and what is left behind on purpose
//!
//! The credentials and the burn record are the mail custody's rows
//! (`fauna.state.mail`), so the leg runs in the **post-store-ready** pass
//! (`fauna_client_recovery::ledger_aftermath`), behind the chain re-point and
//! under the same attested-predecessor gate as leg 4 — not in the post-auth
//! pass, which runs before, or unordered with, the successor's account store
//! (`succession-aftermath.md` § Re-key scope → *Where the plane raises run*).
//! The predecessor set is the registry's attested one, never a chain a row
//! asserts.
//!
//! The burned rows deliberately **stay** — each its own credential row,
//! emptied of its secret bytes and marked, which the READ fold shows. They
//! are the successor's list of which MUAs to re-add; a list that simply
//! emptied would make them remember it. The existing per-row revoke gesture
//! marks one revoked (and so hidden) when the user is done with it.

use fauna_core::data::{MailCredential, MailSuccessionBurn, Timestamp};
use fauna_core::identity::ActorId;
use fauna_core::localized::LocalizedText;
use fauna_core::mail_rows::MailStateRow;
use fauna_core::progress::ProgressOutcome;

use crate::error::DispatchError;
use crate::machine::MailSettingsMachine;
use crate::rotation;

/// What [`burn_mail_after_succession`] found. Mirrors the other aftermath legs'
/// outcome discipline: the arms that mean "no write happened" stay distinct,
/// because a progress surface that collapsed them would report success while a
/// thief-readable mailbox stayed readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailBurnOutcome {
    /// No predecessor owes a burn: an identity that never succeeded (the
    /// overwhelmingly common case, costing one custody read), or one whose
    /// every predecessor is already recorded in
    /// [`fauna_core::data::MailConfig::succession_burns`]. Writes nothing.
    NothingToBurn,
    /// There was no mail material to burn — the successor's plane holds no
    /// MSEK. **The burn is still recorded**, so a later "Enable mail" is not
    /// mistaken for predecessor-era material (module docs).
    NoMailMaterial,
    /// The burn ran: `credentials` rows lost access and are marked, both blob
    /// kinds are deleted for each, and the MSEK rotated to one the predecessor
    /// never saw.
    Burned { credentials: usize },
}

/// The burn as a **progress surface**, mirroring `GrantRemintProgress` and
/// `BackupRegrantProgress` arm for arm via the shared
/// [`fauna_core::progress::Passage`] — the projection all 7 apps render, so
/// the copy lives where the outcome lives and no app writes a `match` over
/// the outcome.
pub type MailBurnProgress = fauna_core::progress::Passage<MailBurnOutcome>;

/// i18n keys for [`MailBurnProgress::status_line`] — `settings.recovery_kit.*`,
/// beside the other aftermath legs' lines.
const KEY_BURN_RUNNING: &str = "settings.recovery_kit.mail_burn_running";
const KEY_BURN_DONE: &str = "settings.recovery_kit.mail_burn_done";
const KEY_BURN_FAILED: &str = "settings.recovery_kit.mail_burn_failed";

/// The done line names the **consequence the user has to act on** — every
/// mail app has to be set up again with a fresh password — because that
/// consequence is the whole visible surface of this leg: the mailbox keeps
/// receiving mail either way, so a user who is not told will read the MUA
/// failures as a broken nest.
///
/// `NoMailMaterial` renders nothing ([`ProgressOutcome::settled_line`]
/// returns `None`): it is the "you don't use mail here" answer, and a line
/// about mail keys shown to someone with no mailbox is exactly the noise
/// that trains a user past the line that matters.
impl ProgressOutcome for MailBurnOutcome {
    const RUNNING_KEY: &'static str = KEY_BURN_RUNNING;
    const FAILED_KEY: &'static str = KEY_BURN_FAILED;

    fn settled_line(&self) -> Option<LocalizedText> {
        match self {
            Self::Burned { credentials } => {
                let mut text = LocalizedText::key(KEY_BURN_DONE);
                text.args.insert("count".into(), credentials.to_string());
                Some(text)
            }
            Self::NothingToBurn | Self::NoMailMaterial => None,
        }
    }

    /// Never: every settled arm is final. The leg reads the mail custody's
    /// rows in the post-store-ready pass or does not run, so no arm defers to
    /// another device; a burn that could not finish is the progress surface's
    /// `Failed`, which the next store-ready retries.
    fn still_owed(&self) -> bool {
        match self {
            Self::NothingToBurn | Self::NoMailMaterial | Self::Burned { .. } => false,
        }
    }
}

/// Burn every pre-succession mail credential and rotate the MSEK out from under
/// the predecessor's seed holder — the aftermath's mail leg. See the module
/// docs for the full contract.
///
/// `predecessors` is the ATTESTED predecessor set — the retired identities whose keys this
/// device holds (`AccountRegistry`'s attested predecessors) — never an owner chain a config asserts
/// (`config-dissolution.md` § Phases and gates → *Bounded rows* → *The
/// ledger*).
///
/// **Safe to call unconditionally, on every device, at every store-ready.** An
/// identity that never succeeded pays one custody read and returns
/// [`MailBurnOutcome::NothingToBurn`]; no nest write is spent unless a burn is
/// genuinely owed.
pub async fn burn_mail_after_succession(
    machine: &MailSettingsMachine,
    predecessors: &[ActorId],
) -> Result<MailBurnOutcome, DispatchError> {
    // The attested predecessors come in as `predecessors`; the burn record and
    // the credentials are the mail custody's.
    let mail = machine.mail_store().load().await?;
    let owed: Vec<ActorId> = predecessors
        .iter()
        .filter(|p| !mail.burned_for(p))
        .copied()
        .collect();
    if owed.is_empty() {
        // Every ordinary identity lands here — it has no predecessor at all —
        // as does every sign-in after a completed burn.
        return Ok(MailBurnOutcome::NothingToBurn);
    }

    // ── Step 1: mark the rows, then record the burn, BEFORE any nest write ──
    //
    // Same discipline as the rotation's own resume sentinel, and for the same
    // reason: from here on the rows are dead — their blobs are about to be
    // deleted and the MSEK rotated — so the record of *that* must not depend on
    // the rest of the pass surviving. The rows are marked FIRST and the burn
    // recorded after, because the record is what ends the leg: a crash between
    // the two leaves the leg still owed (the next sign-in marks whatever is
    // still live and records then), where the other order could record a burn
    // over rows that were never marked. A crash after the record leaves the
    // rotation's sentinel to resume the key half (step 3), while the record
    // keeps the *next* sign-in from starting a second burn over credentials
    // the successor may have added in between.
    //
    // The secret bytes go with the mark (the row's join empties a marked
    // secret anyway): they are precisely what the predecessor's seed holder
    // read, they can never wrap anything again, and an at-rest copy of a
    // known-compromised password serves nobody. The row keeps naming its
    // generation until step 2's deletes land — what the heal's dual reads to
    // re-issue them if this pass stops in between.
    let at_unix = Timestamp::now_secs().max(0) as u64;
    let mark = MailSuccessionBurn {
        // The most recent unburned predecessor names this burn. Every id in
        // `owed` is recorded below; the per-row mark carries one, and on the
        // (rare) twice-succeeded chain that reaches this pass with two owed
        // predecessors at once, the one that matters for the row is the latest.
        predecessor: *owed.last().expect("owed is non-empty"),
        at_unix,
    };
    let live: Vec<MailCredential> = mail.live_credentials().cloned().collect();
    for credential in &live {
        machine
            .mail_store()
            .put_credential(MailCredential {
                secret: Vec::new().into(),
                burned: Some(mark.clone()),
                ..credential.clone()
            })
            .await?;
    }
    machine
        .mail_store()
        .write_state(MailStateRow {
            succession_burns: owed
                .iter()
                .map(|predecessor| MailSuccessionBurn {
                    predecessor: *predecessor,
                    at_unix,
                })
                .collect(),
            ..MailStateRow::recreatable_of(&mail)
        })
        .await?;
    machine.refresh_snapshot().await?;

    if mail.msek.is_none() {
        // Nothing to revoke and nothing to rotate — but the record above is
        // exactly what keeps a later "Enable mail" from being burned as
        // predecessor-era material.
        return Ok(MailBurnOutcome::NoMailMaterial);
    }

    // ── Step 2: delete both blob kinds for every burned credential ────────
    //
    // Ahead of the rotation because it is the half that cuts access *now*: an
    // ordinary hard revoke can leave an excluded credential's blobs resting
    // (the snapshot re-seal kills reads), but that tolerance does not carry
    // here, and it leaves the pre-existing submission-token hole
    // (`mail-credentials.md` § Rotation and recovery → *Succession*: an
    // excluded credential can still SUBMIT until its token expires) open for
    // the one case where the holder is known hostile. Both deletes are
    // idempotent `DELETE`s on nest, so a retry after a partial pass is free.
    // Each row's generation is cleared once its deletes land.
    for credential in &live {
        machine
            .nest()
            .revoke_wrapped_mls_blob(machine.actor_id(), credential.credential_id.clone())
            .await?;
        machine
            .nest()
            .revoke_wrapped_submission_token(machine.actor_id(), credential.credential_id.clone())
            .await?;
        machine
            .mail_store()
            .put_credential(MailCredential {
                secret: Vec::new().into(),
                burned: Some(mark.clone()),
                wrapped_under: None,
                ..credential.clone()
            })
            .await?;
    }
    let burned = live.len();

    // ── Step 3: the degenerate rotation ──────────────────────────────────
    //
    // No explicit exclusions are passed: the rows were marked in step 1 and the
    // rotation's survivor set is the *live* credentials, so the survivors list
    // is empty by construction — step (e) is a no-op and finalize runs
    // immediately (`mail-credentials.md` § Rotation and recovery →
    // *Succession*). What the shared flow still does, and what this leg needs
    // it for: a fresh MSEK′ the predecessor never saw, the snapshot re-sealed
    // under it, the recipient pubkey re-registered under the SUCCESSOR actor
    // (which is what ends the degraded-delivery window the ceremony opened),
    // and MSEK_old parked on `prior_mseks` for grace decrypt — legitimate for
    // the owner, worthless to the thief, whose every unwrap path is now
    // deleted or re-sealed.
    rotation::start_rotation(machine, &[]).await?;

    Ok(MailBurnOutcome::Burned {
        credentials: burned,
    })
}
