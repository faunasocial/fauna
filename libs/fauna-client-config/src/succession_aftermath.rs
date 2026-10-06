//! The post-succession **aftermath**'s `NestBackupKey` leg — the second piece
//! of the urgent sequence § Re-key scope requires of a successor's client
//! (`docs/goal/behavior/identity-succession.md`, the `NestBackupKey` row:
//! *"Old grant revoked; successor derives + grants the new one; the source
//! nest's next pass re-uploads under it"*).
//!
//! # What is actually broken until this runs
//!
//! The succession transaction deletes the old grant outright —
//! `DELETE FROM nest_backup_keys WHERE owner_actor_id = ?1`
//! (`bins/fauna-nest/src/db/successions.rs`, whose own table names the client
//! leg: *"the successor derives and grants a fresh `NestBackupKey`"*). That row
//! is not an optimisation: the nest's backup sweep enumerates owners with
//! `list_nest_backup_key_owners()` (`delegation_runner.rs`), so a successor
//! without one is **skipped entirely** — no segment is backed up anywhere, and
//! nothing says so.
//!
//! The registry half is stranded the same way for a different reason.
//! `backup_destinations` rows are keyed by owner and are deliberately **not**
//! re-pointed by the succession transaction (they are absent from § Re-key
//! scope's ownership blockquote, which enumerates what moves): the authoritative
//! destination list is the owner's `fauna.state.backup` list row for the box,
//! and the nest-side registry is a projection of it. So the successor's
//! projection starts empty and is rebuilt from that list here, rather than
//! migrated.
//!
//! Both halves are exactly what [`reconcile_backup_enrollment`] already
//! re-issues, so this leg is a **driver, not new machinery**: its whole job is
//! to run that reconcile at the successor's store-ready edge instead of
//! waiting for someone to open the Backups page (`read_backup_status` heals
//! there, which is demand-driven and therefore not the "without the user
//! issuing a single command" § Re-key scope asks for).
//!
//! # Where it runs
//!
//! The list is an account-store row, so the leg runs in the **post-store-ready
//! pass** (`fauna_client_recovery::ledger_aftermath`), which reads the list of
//! the box its connection is bound to and hands it in — never another box's,
//! because the grant it owes is that nest's (`succession-aftermath.md` § Re-key
//! scope → *Adjudicating what the aftermath carries across*, the 2026-09-30
//! paragraph).

use fauna_client_backup::BackupClient;
use fauna_core::data::{BackupDestination, DESTINATION_KIND_NEST};
use fauna_core::localized::LocalizedText;
use fauna_core::progress::ProgressOutcome;
use fauna_protocol::RpcRequester;

use crate::backup_enroll::{ReconcileError, reconcile_backup_enrollment};

/// What [`regrant_nest_backup_key`] found. Two of the three values mean "no
/// write happened", and a progress surface that collapsed them with the third
/// would report success while this owner's backups are silently switched off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupRegrantOutcome {
    /// The box's list is empty. Nothing is owed — an owner who never asked
    /// for backups must not be handed a `NestBackupKey` grant as a side effect
    /// of succeeding (the same rule [`reconcile_backup_enrollment`] applies to
    /// an empty list).
    NothingConfigured,
    /// The nest already holds this identity's grant **and** already projects
    /// every listed destination. The idempotent arm: a pass that completed,
    /// or a second device arriving after the first. Writes nothing.
    AlreadyEnrolled,
    /// The grant and/or the missing destination registrations were re-issued.
    /// Carries how many destination rows the reconcile re-registered.
    Regranted {
        /// Listed destinations re-registered with the source nest.
        destinations: usize,
    },
}

/// The re-grant pass as a **progress surface**, shaped like every sibling leg
/// arm for arm via the shared
/// [`fauna_core::progress::Passage`] — the projection all 7 apps render, so
/// the copy lives where the outcome lives and no app writes a `match` over
/// the outcome (`succession-aftermath.md` § Re-key scope: the aftermath is
/// "surfaced with progress").
pub type BackupRegrantProgress = fauna_core::progress::Passage<BackupRegrantOutcome>;

/// i18n keys for [`BackupRegrantProgress::status_line`] — `settings.recovery_kit.*`,
/// beside the other recovery-kit succession lines this renders under.
const KEY_REGRANT_RUNNING: &str = "settings.recovery_kit.backup_regrant_running";
const KEY_REGRANT_DONE: &str = "settings.recovery_kit.backup_regrant_done";
const KEY_REGRANT_FAILED: &str = "settings.recovery_kit.backup_regrant_failed";

/// Two arms deliberately render nothing
/// ([`ProgressOutcome::settled_line`] returns `None`): `NothingConfigured`
/// and `AlreadyEnrolled` are both "nothing is owed", and a line announcing a
/// no-op at every later sign-in trains the user to ignore the one that
/// matters.
impl ProgressOutcome for BackupRegrantOutcome {
    const RUNNING_KEY: &'static str = KEY_REGRANT_RUNNING;
    const FAILED_KEY: &'static str = KEY_REGRANT_FAILED;

    fn settled_line(&self) -> Option<LocalizedText> {
        match self {
            Self::Regranted { .. } => Some(LocalizedText::key(KEY_REGRANT_DONE)),
            Self::NothingConfigured | Self::AlreadyEnrolled => None,
        }
    }

    fn still_owed(&self) -> bool {
        false
    }
}

/// Failure from [`regrant_nest_backup_key`]. Typed rather than stringly,
/// mirroring [`ReconcileError`]; `Display` is what the per-app glue renders.
#[derive(Debug)]
pub enum BackupRegrantError<E> {
    /// The `fauna.backup.status` probe failed. Nothing was changed.
    Status(E),
    /// The reconcile itself failed. Partially applied by construction — the
    /// grant may have landed and some registrations not; it is idempotent, so
    /// the next store-ready re-runs it.
    Reconcile(ReconcileError<E>),
}

impl<E: core::fmt::Display> core::fmt::Display for BackupRegrantError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Status(e) => write!(f, "reading the backup status: {e}"),
            Self::Reconcile(e) => write!(f, "{e}"),
        }
    }
}

impl<E: core::fmt::Display + core::fmt::Debug> std::error::Error for BackupRegrantError<E> {}

/// Re-grant this successor's `NestBackupKey` and rebuild the nest-side
/// destination registry from `destinations` — the aftermath's second leg.
///
/// `destinations` is the bound box's list as the post-store-ready pass read it
/// (`fauna.state.backup`). `owner_secret` is the **successor's own** seed — the
/// granted key is `NestBackupKey::derive` over it, so the retired identity's
/// key is never re-granted (the same `[u8; 32]` convention `read_backup_status`
/// and [`reconcile_backup_enrollment`] take).
///
/// **Safe to call unconditionally, on every device, at every store-ready.**
/// Two of the three arms write nothing, and the empty list short-circuits
/// before any round trip. It keeps no progress state at rest — the nest's own
/// projection is the progress record — which is what makes it resumable.
pub async fn regrant_nest_backup_key<R: RpcRequester + Clone>(
    nest: R,
    owner_secret: [u8; 32],
    destinations: &[BackupDestination],
) -> Result<BackupRegrantOutcome, BackupRegrantError<R::Error>> {
    if destinations.is_empty() {
        return Ok(BackupRegrantOutcome::NothingConfigured);
    }

    // Is anything actually owed? Two independent halves, and checking only the
    // first would leave a whole class of owner permanently unhealed: an owner
    // whose destinations are all their own devices never grants a
    // `NestBackupKey` at all (a client custodian seals for itself —
    // `behavior/backup-destinations.md` § Third destination kind), so `enrolled` is *legitimately*
    // false forever and would look like work owed on every store-ready, while
    // their stranded custodian registrations would never be noticed.
    let status = BackupClient::new(nest.clone())
        .status()
        .await
        .map_err(BackupRegrantError::Status)?;
    let grant_owed =
        destinations.iter().any(|d| d.kind == DESTINATION_KIND_NEST) && !status.enrolled;
    let registration_owed = destinations.iter().any(|d| {
        !status
            .destinations
            .iter()
            .any(|row| row.destination_id == d.destination_id)
    });
    if !grant_owed && !registration_owed {
        return Ok(BackupRegrantOutcome::AlreadyEnrolled);
    }

    reconcile_backup_enrollment(nest, owner_secret, destinations)
        .await
        .map(|destinations| BackupRegrantOutcome::Regranted { destinations })
        .map_err(BackupRegrantError::Reconcile)
}
