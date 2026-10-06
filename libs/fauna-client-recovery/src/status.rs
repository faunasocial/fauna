//! The Settings status line — the four states `recovery-kit-status` renders,
//! and which of the section's actions each one enables.
//!
//! `ui/settings.md` § Recovery kit specifies exactly four states and requires
//! them read **from the registration chain, never a local flag**, so that a kit
//! created on another device is reflected here. This module is that read,
//! composed once: the alternative is seven apps each deciding what "registered
//! but no blob rests" looks like and which buttons it permits, which is the
//! divergence priority #1 exists to prevent.
//!
//! Nothing here is a stored flag. Every state is derived from what the nest
//! answers at read time, for the same reason the alert projection is
//! (`critical-alerts.md` § Mechanism → *Lifetime*): a cached "you have a kit"
//! outlives the kit.

use fauna_core::identity::ActorId;
use fauna_core::localized::LocalizedText;
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::error::Result;
use crate::nest::RecoveryClient;
use crate::replacement::PendingReplacement;

const KEY_NEVER_CREATED: &str = "settings.recovery_kit.status_never_created";
const KEY_REGISTERED: &str = "settings.recovery_kit.status_registered";
const KEY_REGISTERED_NO_ESCROW: &str = "settings.recovery_kit.status_registered_no_escrow";
const KEY_REPLACEMENT_PENDING: &str = "settings.recovery_kit.status_replacement_pending";

/// What `recovery-kit-status` shows, and what the section's buttons may do.
///
/// The four states of `ui/settings.md` § Recovery kit, in the precedence
/// [`kit_status`] resolves them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryKitStatus {
    /// No registration on the chain — the standing warning after a skip.
    ///
    /// Deliberately one state for every account with no registration:
    /// § The RecoveryKey's *Creation UX* wants the same warning
    /// however it got there, so no "did they skip?" bit is kept anywhere.
    NeverCreated,
    /// A kit is registered and an escrow blob rests. The neutral state.
    Registered,
    /// The chain has a kit but no blob rests — recovery *by phrase* is
    /// unavailable until a re-put, though succession still works.
    ///
    /// A real loss-protection gap rather than an error: the nest drops the row
    /// whenever the key sealing it retires (§ Seed escrow → *Lifecycle on the
    /// nest*), so this is the honest signal that a re-put is owed. It is
    /// surfaced on a signed-in surface because a signed-in device is the only
    /// party that can repair it.
    RegisteredNoEscrow,
    /// A seed-alone replacement is in its 30-day window.
    ///
    /// Takes precedence over the escrow states: it is the one state that means
    /// *someone may hold your identity secret*, and it is the only one with a
    /// deadline. The loud half is the every-page `critical-alerts` banner
    /// ([`crate::alerts`]); this section carries the veto action.
    ReplacementPending(PendingReplacement),
}

impl RecoveryKitStatus {
    /// Is a kit registered at all? True for every state but
    /// [`Self::NeverCreated`].
    pub fn is_registered(&self) -> bool {
        !matches!(self, Self::NeverCreated)
    }

    /// `recovery-kit-create-button` — only where nothing is registered.
    ///
    /// Retrofit and first registration are the same act, so this covers both.
    /// Replacing a registered kit goes through the two paths below, which is
    /// what stops a stray click from silently opening a 30-day window.
    pub fn allows_create(&self) -> bool {
        matches!(self, Self::NeverCreated)
    }

    /// `recovery-kit-replace-button` — replace using the kit you hold.
    ///
    /// Enabled for every registered state, **including a pending window**:
    /// there it is precisely the override arm § The RecoveryKey → *Replacement*
    /// gives the current key ("the current RecoveryKey can veto/override
    /// instantly"). On [`Self::RegisteredNoEscrow`] it stays available as the
    /// sterner path — it retires the held kit and mints a fresh one to write
    /// down; the gap's own repair is [`Self::allows_escrow_reseal`], which
    /// keeps the kit in hand.
    pub fn allows_replace(&self) -> bool {
        self.is_registered()
    }

    /// `recovery-kit-escrow-reseal-button` — restore phrase recovery with the
    /// kit in hand, without retiring it
    /// (`fauna_client_recovery::reseal_escrow_with_held_kit`).
    ///
    /// [`Self::RegisteredNoEscrow`] only, which is why `ui.yaml` marks it
    /// optional (the veto button's reason): everywhere else either no kit
    /// exists to seal to, a blob already rests — a re-put would *replace* it,
    /// which is the replace ceremony's job, with its read-the-resting-section
    /// rule — or a pending window makes the veto the urgent affordance.
    pub fn allows_escrow_reseal(&self) -> bool {
        matches!(self, Self::RegisteredNoEscrow)
    }

    /// `recovery-kit-lost-button` — seed-alone replacement, opening the window.
    ///
    /// Registered states only; there is nothing to replace otherwise (a user
    /// with no kit wants create). A second request during an open window is
    /// permitted because the nest resolves it latest-wins rather than refusing
    /// — the user who mislaid the *new* kit too has no other way forward.
    pub fn allows_lost(&self) -> bool {
        self.is_registered()
    }

    /// `identity-stolen-button` — the succession ceremony.
    ///
    /// Enabled in **every** state, per § Recovery kit's "stolen (any)". Theft
    /// does not wait for the account to be in a tidy state, and a user whose
    /// seed is stolen while a replacement pends is in the worst state of all.
    /// The ceremony itself still needs a RecoveryKey the caller must supply, so
    /// enabling the button promises a screen, not an outcome.
    pub fn allows_stolen(&self) -> bool {
        true
    }

    /// `recovery-pending-veto-button` — rendered **only** while a window is
    /// open (`ui.yaml` marks it optional for exactly this reason).
    pub fn allows_veto(&self) -> bool {
        matches!(self, Self::ReplacementPending(_))
    }

    /// The status line, as shared localized text.
    ///
    /// Pure — the caller owns the clock, as everywhere else in this crate — so
    /// the copy is identical on all seven apps and testable on every platform.
    /// `now` is unix seconds and is read only by the pending arm, whose
    /// countdown rounds **up** in whole days for [`crate::alerts`]'s reason: a
    /// window in its final hours must never read as already lost.
    pub fn status_line(&self, now: i64) -> LocalizedText {
        match self {
            Self::NeverCreated => LocalizedText::key(KEY_NEVER_CREATED),
            Self::Registered => LocalizedText::key(KEY_REGISTERED),
            Self::RegisteredNoEscrow => LocalizedText::key(KEY_REGISTERED_NO_ESCROW),
            Self::ReplacementPending(pending) => {
                let days = pending.days_remaining(now);
                let mut text = LocalizedText::key(KEY_REPLACEMENT_PENDING);
                text.args.insert("days".into(), days.to_string());
                text
            }
        }
    }
}

/// Read this identity's recovery-kit status off the nest. USER class.
///
/// Three reads, resolved in precedence order:
///
/// 1. **The registration chain** decides whether a kit exists at all. Nothing
///    registered is terminal — neither of the reads below can say anything
///    about an account with no kit, so they are not made.
/// 2. **A pending replacement** outranks the escrow question. It is the only
///    state carrying a deadline and the only one that implies someone else may
///    hold the identity secret.
/// 3. **Escrow presence** separates the two registered states.
///
/// Every read's refusal is an error — including `unknown_kind` on (3), whose
/// "older nest ⇒ neutral `Registered`" arm left with the compat-remnant sweep
/// (`version-compatibility.md` § Dimension 2).
pub async fn kit_status<R>(
    client: &RecoveryClient<R>,
    actor_id: &ActorId,
) -> Result<RecoveryKitStatus>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    if client.chain_head(actor_id).await?.is_none() {
        return Ok(RecoveryKitStatus::NeverCreated);
    }

    if let Some(pending) = crate::replacement::pending_replacement(client).await? {
        return Ok(RecoveryKitStatus::ReplacementPending(pending));
    }

    match client.escrow_status().await {
        Ok(Some(_)) => Ok(RecoveryKitStatus::Registered),
        Ok(None) => Ok(RecoveryKitStatus::RegisteredNoEscrow),
        Err(e) => Err(e),
    }
}

/// Pair a ceremony's returned secret with a fresh [`kit_status`] read, inside
/// the same background task — the ceremony moved the registration chain, so a
/// status read from before it started is stale the instant the ceremony
/// returns. `minted`'s error is flattened to `String` with [`ToString`]
/// (`E: Display`) since both native call sites (tui, linux) want a page-level
/// error string, not the typed error chain — tui's own ceremony returns
/// [`crate::error::RecoveryError`] directly, linux's already carries a pre-flattened
/// `String`.
///
/// A failed status re-read is not allowed to discard the minted secret — it
/// is the only copy in existence — so the status falls back to the state the
/// ceremony is known to have produced ([`RecoveryKitStatus::Registered`]).
pub async fn kit_status_after_mint<R, E>(
    client: &RecoveryClient<R>,
    identity: &fauna_core::identity::ActorKeypair,
    minted: std::result::Result<String, E>,
) -> std::result::Result<(String, RecoveryKitStatus), String>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
    E: core::fmt::Display,
{
    let secret_hex = minted.map_err(|e| e.to_string())?;
    let status = status_after_ceremony(client, &identity.actor_id()).await;
    Ok((secret_hex, status))
}

/// `recovery-pending-veto-button`'s whole ceremony, for every app: parse the
/// kit the user pasted, contest the pending window at the bound nest **and at
/// every linked nest** (`dial`; `identity-succession.md` § Enforcement on the
/// home nest → *Every nest the identity is linked to*, clause (c) —
/// [`crate::linked_fanout::veto_everywhere`]), then re-read the status in the
/// SAME call — the re-read status is the gesture's only receipt (the countdown
/// line and the button disappear), so a second async hop would leave an
/// awaited click asserting the old state (testing.md convention 14).
///
/// The account is passed explicitly rather than left to the payload: Settings
/// always knows whose account it is, and a bare 64-hex phrase names none.
/// Returns whether anything was pending to cancel at any nest, beside the
/// fresh status. The bound nest's refusal is the gesture's error; a linked
/// nest that could not be asked is logged, never a failure.
pub async fn veto_with_status<R, D>(
    client: &RecoveryClient<R>,
    actor_id: ActorId,
    held_kit_input: &str,
    dial: &D,
) -> std::result::Result<(bool, RecoveryKitStatus), String>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
    D: crate::linked_fanout::LinkedNestDial,
    <D::Anonymous as RpcRequester>::Error: RpcErrorClass,
{
    // Parse first: a malformed phrase costs no round trip.
    let kit = crate::restore::parse_kit(held_kit_input).map_err(|e| e.to_string())?;
    let (bound, linked) = crate::linked_fanout::veto_everywhere(client, &kit, actor_id, dial).await;
    let cancelled = bound.map_err(|e| e.to_string())?
        || linked.nests.iter().any(|n| {
            matches!(
                n.outcome,
                crate::linked_fanout::LinkedNestOutcome::Answered(true)
            )
        });
    Ok((cancelled, status_after_ceremony(client, &actor_id).await))
}

/// `recovery-kit-escrow-reseal-button`'s whole ceremony (the no-escrow repair),
/// for the in-process apps: parse the held kit, run the head-checked re-put
/// WITHOUT retiring that kit, then re-read the status the section renders —
/// the state flipping to [`RecoveryKitStatus::Registered`] is the receipt.
///
/// `predecessors` carries [`crate::kit::create_kit`]'s own warning: pass what
/// the account registry resolved, never an empty slice by default.
pub async fn reseal_escrow_with_status<R>(
    client: &RecoveryClient<R>,
    identity: &fauna_core::identity::ActorKeypair,
    held_kit_input: &str,
    predecessors: &[crate::PredecessorSeed],
) -> std::result::Result<RecoveryKitStatus, String>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let kit = crate::restore::parse_kit(held_kit_input).map_err(|e| e.to_string())?;
    crate::replacement::reseal_escrow_with_held_kit(client, identity, &kit, predecessors)
        .await
        .map_err(|e| e.to_string())?;
    Ok(status_after_ceremony(client, &identity.actor_id()).await)
}

/// The status re-read every ceremony above folds in. A failed read never turns
/// a ceremony that landed into a reported failure: it falls back to the state
/// each of them is known to produce — [`RecoveryKitStatus::Registered`] (a kit
/// is registered, escrow put, nothing pending).
async fn status_after_ceremony<R>(
    client: &RecoveryClient<R>,
    actor_id: &ActorId,
) -> RecoveryKitStatus
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    kit_status(client, actor_id).await.unwrap_or_else(|e| {
        tracing::warn!("[settings/recovery] status re-read after a ceremony: {e}");
        RecoveryKitStatus::Registered
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(lands_at: i64) -> PendingReplacement {
        PendingReplacement {
            new_recovery_pubkey_hex: "ab".repeat(32),
            requested_at: 0,
            lands_at,
        }
    }

    /// The enablement matrix of § Recovery kit, state by state. Written as the
    /// whole matrix rather than one assert per button so a future edit that
    /// widens one state has to restate every cell it changes.
    #[test]
    fn enablement_follows_the_status() {
        let cases = [
            // (status, create, replace, lost, stolen, veto, escrow_reseal)
            (
                RecoveryKitStatus::NeverCreated,
                true,
                false,
                false,
                true,
                false,
                false,
            ),
            (
                RecoveryKitStatus::Registered,
                false,
                true,
                true,
                true,
                false,
                false,
            ),
            (
                RecoveryKitStatus::RegisteredNoEscrow,
                false,
                true,
                true,
                true,
                false,
                true,
            ),
            (
                RecoveryKitStatus::ReplacementPending(pending(100)),
                false,
                true,
                true,
                true,
                true,
                false,
            ),
        ];
        for (status, create, replace, lost, stolen, veto, escrow_reseal) in cases {
            assert_eq!(status.allows_create(), create, "create for {status:?}");
            assert_eq!(status.allows_replace(), replace, "replace for {status:?}");
            assert_eq!(status.allows_lost(), lost, "lost for {status:?}");
            assert_eq!(status.allows_stolen(), stolen, "stolen for {status:?}");
            assert_eq!(status.allows_veto(), veto, "veto for {status:?}");
            assert_eq!(
                status.allows_escrow_reseal(),
                escrow_reseal,
                "escrow_reseal for {status:?}"
            );
        }
    }

    /// Succession is reachable from every state — theft does not wait for the
    /// account to be tidy.
    #[test]
    fn stolen_is_reachable_from_every_state() {
        for status in [
            RecoveryKitStatus::NeverCreated,
            RecoveryKitStatus::Registered,
            RecoveryKitStatus::RegisteredNoEscrow,
            RecoveryKitStatus::ReplacementPending(pending(1)),
        ] {
            assert!(status.allows_stolen(), "{status:?}");
        }
    }

    /// Each state renders its own key, so a mis-wired arm cannot silently show
    /// a neighbouring state's copy.
    #[test]
    fn each_state_renders_its_own_key() {
        let keys: Vec<String> = [
            RecoveryKitStatus::NeverCreated,
            RecoveryKitStatus::Registered,
            RecoveryKitStatus::RegisteredNoEscrow,
            RecoveryKitStatus::ReplacementPending(pending(1)),
        ]
        .iter()
        .map(|s| s.status_line(0).key)
        .collect();
        let mut unique = keys.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), keys.len(), "keys collide: {keys:?}");
    }

    /// The countdown rounds **up**: a window with any time left never reads
    /// "0 days", and 47 remaining hours never read "1 day".
    #[test]
    fn pending_countdown_rounds_up() {
        let day = 86_400i64;
        for (lands_at, now, expect) in [
            (30 * day, 0, "30"),
            // 47 hours left — not yet 1 day.
            (2 * day - 3600, 0, "2"),
            // The final hour still reads as a day, never zero.
            (3600, 0, "1"),
            // Already elapsed — saturates rather than wrapping.
            (0, day, "0"),
        ] {
            let status = RecoveryKitStatus::ReplacementPending(pending(lands_at));
            assert_eq!(
                status.status_line(now).args.get("days").map(String::as_str),
                Some(expect),
                "lands_at={lands_at} now={now}"
            );
        }
    }
}
