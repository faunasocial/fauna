//! Escrow restore — the phrase-only loss-recovery ceremony behind the
//! recovery-entry screen (`docs/goal/behavior/identity-succession.md`
//! § Seed escrow → *Restore path*).
//!
//! A fresh client holding **only** the recovery phrase recovers the identity
//! seed: challenge → sign → fetch → unseal, all pre-identity because by
//! construction there is no device left to authenticate with. After this the
//! account is simply signed into with the restored seed — no succession is
//! needed, because the seed was lost, not stolen.

use fauna_core::identity::ActorId;
use fauna_core::recovery::{EscrowChallenge, ImportedRecoveryKit, RecoveryKey};
use fauna_mls::wrapped_blob::{
    PredecessorSeedOpened, PredecessorsOutcome, unseal_seed_escrow_with_predecessors,
};
use fauna_protocol::{RpcErrorClass, RpcRequester};
use zeroize::Zeroizing;

use crate::error::{RecoveryError, Result};
use crate::nest::RecoveryClient;

/// A recovery-kit field the user pasted or scanned, parsed into the three
/// things a restore may need.
///
/// Neither account half is guaranteed: a Settings-minted QR embeds both, the
/// onboarding kit screen's QR embeds only the actor (no handle is chosen yet at
/// its position), and a hand-typed 64-hex secret embeds neither. That is why
/// [`restore_seed`] takes the actor id as a separate argument the parse can
/// fill in, and why the screen asks for the account handle whenever
/// [`Self::handle`] is `None` — the handle's `@domain` is the only thing that
/// locates the home nest to connect to.
pub struct ParsedKit {
    /// The recovery root itself.
    pub recovery: RecoveryKey,
    /// The account the payload named, if it named one. Authoritative for
    /// *which* account: the escrow blob is AAD-bound to this id.
    pub actor_id: Option<ActorId>,
    /// The handle the payload named, if it named one — *where* the account
    /// lives. Kept as the raw `user@domain` string the payload carried; nest
    /// resolution is the caller's step (it owns the DNS/SRV machinery).
    pub handle: Option<String>,
}

impl core::fmt::Debug for ParsedKit {
    /// Redacted for the same reason [`crate::RecoveryKit`]'s is: a recovery
    /// root reaching a log line or a panic message defeats the offline-only
    /// custody rule.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ParsedKit")
            .field("recovery", &"<redacted>")
            .field("actor_id", &self.actor_id)
            .field("handle", &self.handle)
            .finish()
    }
}

/// Parse a pasted or scanned recovery-kit field.
///
/// Accepts exactly the grammar `fauna_core::recovery::parse_recovery_kit_input`
/// defines — bare 64-hex, the `fauna://recovery?secret=…&actor=…` query form,
/// and the colon form — so all 7 apps read one grammar rather than each
/// hand-rolling three (priority #4). A `fauna://identity` payload is
/// deliberately refused there, and so is refused here.
pub fn parse_kit(input: &str) -> Result<ParsedKit> {
    let ImportedRecoveryKit {
        secret,
        actor_id,
        handle,
    } = fauna_core::recovery::parse_recovery_kit_input(input).ok_or_else(|| {
        RecoveryError::Malformed("not a recovery kit (expected 64-hex or fauna://recovery)".into())
    })?;
    let recovery = RecoveryKey::from_hex(&secret)
        .map_err(|e| RecoveryError::Malformed(format!("recovery secret: {e}")))?;
    let actor_id = match actor_id {
        Some(hex) => Some(
            ActorId::from_hex(&hex)
                .map_err(|e| RecoveryError::Malformed(format!("actor id: {e}")))?,
        ),
        None => None,
    };
    Ok(ParsedKit {
        recovery,
        actor_id,
        handle,
    })
}

/// Recover the identity seed from the nest's escrow blob.
///
/// `actor_id` names the account being recovered; pass `None` to use the one the
/// payload carried, which fails with [`RecoveryError::Malformed`] if it carried
/// none (a bare hex secret does not identify its account, and guessing is not
/// an option — the blob is AAD-bound to the actor).
///
/// Runs entirely over an **anonymous** connection to the account's home nest.
///
/// Two refusals are answers rather than faults, and the screen must render each
/// distinctly (`identity-succession.md:51`):
///
/// - [`RecoveryError::NoEscrow`] — no blob rests for this account. Phrase-only
///   recovery is unavailable; the kit must be re-created from a signed-in
///   device. This is the honest signal, not a bug.
/// - [`RecoveryError::Superseded`] — the identity was succeeded. Route to the
///   identity-import flow, uniform with every other superseded refusal.
///
/// **The predecessor section rides back with the seed** ([`RestoredSeed`]).
/// This crate has no persistence seam by design, so it recovers the material
/// and hands it on; persisting the predecessor seeds into the account registry
/// — which is what lets the corpus re-seal driver open a corpus still sealed
/// under a predecessor — is the caller's half.
pub async fn restore_seed<R>(
    client: &RecoveryClient<R>,
    kit: &ParsedKit,
    actor_id: Option<ActorId>,
) -> Result<RestoredSeed>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let actor_id = actor_id.or(kit.actor_id).ok_or_else(|| {
        RecoveryError::Malformed(
            "this recovery kit does not name an account — enter the account being recovered".into(),
        )
    })?;

    let challenge = client.escrow_challenge(&actor_id).await?;
    // The nonce is single-use and account-bound: one challenge buys exactly one
    // attempt, so a captured exchange cannot be replayed
    // (`identity-succession.md:50`).
    let signature = EscrowChallenge::new(actor_id, challenge.nonce)
        .sign(&kit.recovery)
        .map_err(|e| RecoveryError::Crypto(format!("signing the escrow challenge: {e}")))?;

    let blob = client
        .escrow_fetch(&actor_id, &challenge.nonce, &signature)
        .await?;

    let opened = unseal_seed_escrow_with_predecessors(&blob, &kit.recovery.escrow_secret())
        .map_err(|e| RecoveryError::Crypto(format!("unsealing the seed escrow: {e}")))?;
    tracing::debug!("identity seed restored from escrow");
    Ok(RestoredSeed {
        seed: opened.seed,
        predecessors: opened.predecessors,
    })
}

/// What a phrase-only restore recovered: the account's own identity seed, plus
/// whatever the blob's predecessor section held.
///
/// The two halves are deliberately **not** flattened into one seed list. The
/// primary is what brings the account back and its absence is a hard failure;
/// the predecessors only widen what the recovered account can *open* during the
/// corpus re-seal window, and a broken section must never cost the restore
/// (`identity-succession.md` § Seed escrow).
pub struct RestoredSeed {
    /// The account's identity seed, zeroized on drop.
    pub seed: Zeroizing<[u8; 32]>,
    /// The predecessor section — `Absent` on every pre-succession kit and on
    /// every blob re-put after the re-seal completed, `Unreadable` when a
    /// section is present but did not open. **`Unreadable` must be surfaced,
    /// not swallowed:** the account is back either way, but a corpus still
    /// sealed under a predecessor has just become unopenable, and the user is
    /// the only party who can act on that (re-run the restore elsewhere, or
    /// accept the loss knowingly).
    pub predecessors: PredecessorsOutcome,
}

/// Redacted by hand, like every other secret-bearing type in this crate: a
/// `Result<RestoredSeed, _>::unwrap_err()` in a test — or any `{:?}` on a log
/// line — must not print identity seeds. The shape is still reported, since
/// "how many predecessors, and did the section open" is exactly what a failing
/// test needs.
impl std::fmt::Debug for RestoredSeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let predecessors = match &self.predecessors {
            PredecessorsOutcome::Absent => "absent".to_string(),
            PredecessorsOutcome::Opened(list) => format!("opened({})", list.len()),
            PredecessorsOutcome::Unreadable(e) => format!("unreadable({e})"),
        };
        f.debug_struct("RestoredSeed")
            .field("seed", &"<redacted>")
            .field("predecessors", &predecessors)
            .finish()
    }
}

impl RestoredSeed {
    /// The recovered predecessor seeds, or an empty slice for both non-`Opened`
    /// outcomes. Convenience for the persistence half; a caller that must tell
    /// `Absent` from `Unreadable` — and every user-facing one must — reads
    /// [`Self::predecessors`] directly.
    pub fn opened_predecessors(&self) -> &[PredecessorSeedOpened] {
        match &self.predecessors {
            PredecessorsOutcome::Opened(list) => list,
            PredecessorsOutcome::Absent | PredecessorsOutcome::Unreadable(_) => &[],
        }
    }
}
