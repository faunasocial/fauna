//! The **post-store-ready** half of the succession aftermath — the ledger legs
//! that write the successor's account store, which the post-auth pass
//! ([`crate::aftermath::run_succession_aftermath`]) cannot reach
//! (`succession-aftermath.md` § Re-key scope → *Adjudicating what the
//! aftermath carries across*, the 2026-09-30 paragraph).
//!
//! **Why a second pass.** The post-auth pass runs before, or unordered with,
//! the successor's account-store handle on every host family, and the
//! succession ledger (`fauna.state.succession-ledger`) lives in that store. So
//! every host runs this pass once at its **store-ready edge** — the edge that
//! already lends the offline-share seat record and wires the conversation
//! seams — handing it the handle (as the [`SuccessionLedgerStore`] seam), the
//! account registry and the nest requester. A host passes no identity and no
//! ceremony fact: the seam knows whom it serves, and the ceremony's facts rest
//! in the registry's durable park ([`PendingCeremony`]).
//!
//! **Its legs, in order:** the chain re-point and the grant-mark raise (they
//! need only the attested predecessor set, so no park — they re-run off
//! `AccountRegistry::attested_predecessor_actor_ids` at every store-ready,
//! idempotent by key); the member-item raise, the filter-mark raise and the
//! destination-mark raise, off the parked ceremony (the destination raise
//! rides the park, run once per ceremony, so a destination the successor adds
//! afterwards is never raised as carried across); leg 2's `NestBackupKey`
//! re-grant, off the bound box's `fauna.state.backup` list; then
//! leg 4's re-mint, signed with this account's own stored seed; then leg 8,
//! the subscriber-tier period-key rotation, signed with the same seed (its keys
//! are `fauna.state.subscriptions` rows in this store — the host hands in the
//! handle as the period-key store, which leg 4's post-tier re-mints read too);
//! then leg 6, the mail burn, last because it is the only leg that takes
//! something away (its credentials and burn record are `fauna.state.mail`
//! rows in this store); then the review-surface re-read
//! ([`AftermathSink::config_stage_settled`]). **The succession cut's custody
//! arm** (`writer-signed-change-records.md` ruling (11)(a) — every owned set
//! re-minted under the successor, over custody, its adoption marker first)
//! runs beside the legs, at every store-ready the account attests a
//! predecessor at; its order against them, and against the corpus re-seal in
//! the sync agent, is immaterial (ruling (11)(a)), and the launch reconcile
//! runs the same arm again on every identity-holding app.
//!
//! **The chain put comes first because everything after it reads through
//! it.** Until the re-point lands, the READ fold links no predecessor — their
//! events are invisible — and a mark write's chain forks from the
//! predecessor-named stored one, so the door refuses it. The chain put is the
//! successor's first tip-sealed write and mints its first generation on first
//! need; a put the door refuses stays owed, and every later leg of the pass
//! is skipped with it (the next store-ready re-runs them all).
//!
//! **Crash-safety** (`common.md` § Client-state recoverability): the park is
//! the single durable decision point, written before the switch; this pass at
//! every store-ready is the boot reconcile. It consumes a park only when its
//! raising predecessor is in the attested set (attribution is never guessed),
//! and clears it only once every put of both raises landed — a crash or a
//! door refusal anywhere in between leaves it parked and the next store-ready
//! re-runs it, which re-asks nothing: on the plane a raise is a per-row put of
//! an `Open` mark whose join never demotes a decided verdict.
//!
//! Every leg is best-effort and none is fatal, like the post-auth pass's: a
//! door refusal is the transient no-tip refusal (offline, no escrow target
//! yet, no trusted holder reachable), never a retry loop — the next
//! store-ready re-runs it.

use fauna_client_accounts::AccountRegistry;
use fauna_client_capabilities::{GrantRemintProgress, remint_capability_grants};
use fauna_client_config::{
    BackupRegrantProgress, BackupStateStore, FilterMarkRaise, StoreError, SuccessionLedgerStore,
    raise_succession_destination_marks, raise_succession_filter_marks,
    raise_succession_member_reviews, regrant_nest_backup_key,
};
use fauna_client_mail_settings::{MailBurnProgress, MailSettingsMachine};
use fauna_core::identity::ActorId;
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::aftermath::{AftermathCeremony, AftermathSink, PendingCeremony};

/// What the pass did with the registry's parked ceremony — the one piece of
/// its state a caller (or a test) can observe beyond the ledger itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkedRaises {
    /// Nothing was parked for this account — every ordinary store-ready.
    NoneParked,
    /// Both raises landed and the park was cleared.
    Drained,
    /// A raise did not land (a door refusal, an unreadable filter list, a
    /// succession stamp neither the park nor the nest could supply) — the park
    /// stays and the next store-ready re-runs it.
    StillOwed,
    /// The park's raising predecessor is not (yet) in this device's attested
    /// set, so it is left untouched rather than attributed to a guess.
    Unattested,
    /// The park no longer decodes, or names no parseable predecessor — no
    /// later store-ready could consume it either, so it was cleared.
    Voided,
}

/// Run the post-store-ready pass for the account `ledger` serves.
///
/// `nest` is the successor's signed-in requester (the filter raise reads the
/// filter list and, when the park carries no stamp,
/// `fauna.recovery.succession.status`); `period_keys` is the same account
/// store as the period-key custody (`fauna.state.subscriptions` — the handle
/// implements both seams), which leg 4's post-tier re-mints read and leg 8
/// rotates; `sink` is the same [`AftermathSink`] the host's post-auth pass
/// reports into — this pass fires legs 2's, 4's and 8's lines and its
/// review-surface re-read. `backup` is the same account store as the
/// backup-destination state (`fauna.state.backup` — the handle implements
/// that seam too), and `bound_nest` the identity `nest`'s connection proved
/// (`fauna_client_pair::LinkedNestsMachine::bound_nest_id`): leg 2 re-grants
/// off that box's list and no other's, and a host that could not establish
/// it passes `None`, which skips leg 2 for this pass. `mail` is the same
/// account's mail custody (`fauna.state.mail`), the MSEK a re-minted mail or
/// calendar grant derives its payload from. `mail_burn` is the host's
/// mail-settings machine over that custody, which leg 6 drives — a whole
/// machine rather than a bare store because the burn runs the shared
/// rotate-mail-keys flow, which lives on it, and the two transports build
/// theirs through different constructors; `None` skips leg 6 (the host could
/// not build one, and has already logged why). `custody_cut` is the account's
/// folder-key custody behind the succession cut's arm
/// (`fauna_client_folders::SetCustodyCut` over the same store and this
/// requester), run as the identity `ledger` serves.
#[allow(clippy::too_many_arguments)]
pub async fn run_ledger_aftermath<R, S>(
    nest: R,
    ledger: &dyn SuccessionLedgerStore,
    backup: &dyn BackupStateStore,
    bound_nest: Option<[u8; 32]>,
    period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
    mail: &dyn fauna_client_config::MailStore,
    mail_burn: Option<std::sync::Arc<MailSettingsMachine>>,
    custody_cut: &dyn fauna_client_config::FolderCustodyCut,
    registry: &AccountRegistry,
    sink: &mut S,
) -> ParkedRaises
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass + core::fmt::Display,
    S: AftermathSink,
{
    let parked = match ledger.self_actor() {
        Ok(self_actor) => {
            let self_hex = self_actor.to_hex();
            // ── The chain re-point and the grant-mark raise ───────────────
            // Ahead of everything else: the chain put is the successor's
            // first tip-sealed write, and every later read and mark write
            // goes through the chain it lands.
            let attested = registry.attested_predecessor_actor_ids(&self_hex);
            // ── The succession cut's custody arm ─────────────────────────
            // Where this device attests a predecessor, re-mint every owned
            // set under the successor (`writer-signed-change-records.md`
            // ruling (11)(a)); best-effort — the launch reconcile runs the
            // same arm, so a refusal here costs a launch's wait.
            if !attested.is_empty() {
                match custody_cut.cut(self_actor).await {
                    Ok(0) => {}
                    Ok(reminted) => tracing::info!(
                        reminted,
                        "the succession cut re-minted the owned sets' nonces"
                    ),
                    Err(e) => tracing::warn!(
                        error = %e,
                        "the succession cut's custody arm did not finish; the launch reconcile \
                         runs it again"
                    ),
                }
            }
            match repoint_and_raise(ledger, &attested).await {
                Ok(()) => {
                    // ── The member-item, filter-mark and destination-mark
                    //    raises, off the park ──
                    let parked =
                        drain_parked_ceremony(&nest, ledger, backup, registry, &self_actor).await;

                    // ── Leg 2: the `NestBackupKey` re-grant ───────────────
                    // After the raises, before leg 4 (`succession-aftermath.md`,
                    // the 2026-09-30 paragraph). Unconditional, like the
                    // post-auth leg it replaces: an ordinary identity's nest
                    // already projects every destination, which renders no
                    // line.
                    if let Some(bound) = bound_nest {
                        regrant(nest.clone(), backup, bound, registry, &self_hex, sink).await;
                    }

                    // ── Leg 4's re-mint ───────────────────────────────────
                    // Only where this device attests a predecessor — the
                    // post-auth pass's own gate (`predecessors_of` non-empty),
                    // so an ordinary identity's every store-ready renders no
                    // progress line at all.
                    if !attested.is_empty() {
                        remint(
                            nest.clone(),
                            ledger,
                            &*period_keys,
                            mail,
                            registry,
                            &self_hex,
                            sink,
                        )
                        .await;
                        // ── Leg 8: the period-key rotation ────────────────
                        rotate_periods(nest.clone(), period_keys, registry, &self_hex, sink).await;
                        // ── Leg 6: the mail burn ──────────────────────────
                        // Last: the only leg that takes something away.
                        if let Some(machine) = mail_burn.as_deref() {
                            burn_mail(machine, &attested, sink).await;
                        }
                    }
                    parked
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "the succession ledger's chain re-point or grant-mark raise was refused; \
                         the pass's ledger legs stay owed for the next store-ready"
                    );
                    if !attested.is_empty() {
                        sink.grant_remint(GrantRemintProgress::Failed(format!("{e}")));
                        sink.period_rotation(
                            fauna_client_subscriptions::orchestration::PeriodRotationProgress::Failed(
                                format!("{e}"),
                            ),
                        );
                        if mail_burn.is_some() {
                            sink.mail_burn(MailBurnProgress::Failed(format!("{e}")));
                        }
                    }
                    ParkedRaises::StillOwed
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "the post-store-ready pass has no account identity");
            ParkedRaises::StillOwed
        }
    };

    // Unconditional — a refused raise must not blank a mark an earlier
    // ceremony raised (the trait method's own doc).
    sink.config_stage_settled().await;
    parked
}

/// Leg 6 — the mail burn, reported through the sink's mail-burn line.
///
/// ⚠ **The sharpest exposure the aftermath closes.** Until it runs, a
/// succession does not touch the mail plane at all, so whoever held the retired
/// identity's seed — who read every credential secret AND the MSEK — keeps
/// reading the successor's mail: everything sealed after the ceremony,
/// silently, indefinitely (`identity-succession.md` § Re-key scope, the MSEK
/// row).
///
/// Here, not in the post-auth pass: the credentials it burns and the burn
/// record that makes it idempotent are the mail custody's rows in this store.
/// It burns for every ATTESTED predecessor (the registry's, never a chain a
/// row asserts), re-runs at every store-ready, and costs one custody read once
/// the burn is recorded. A failure leaves the exposure open until a later
/// store-ready succeeds, which is why the rendered line says so; the burn
/// record is written before any nest call, so a failure part-way never re-arms
/// the leg against the successor's own later credentials.
async fn burn_mail<S: AftermathSink>(
    machine: &MailSettingsMachine,
    attested: &[ActorId],
    sink: &mut S,
) {
    sink.mail_burn(MailBurnProgress::Running);
    let progress = match fauna_client_mail_settings::burn_mail_after_succession(machine, attested)
        .await
    {
        Ok(outcome) => {
            tracing::info!(?outcome, "the mail burn leg settled");
            MailBurnProgress::Settled(outcome)
        }
        Err(e) => {
            tracing::warn!(error = %e, "the mail burn failed; it retries at the next store-ready");
            MailBurnProgress::Failed(format!("{e}"))
        }
    };
    sink.mail_burn(progress);
}

/// Legs (a) and (b): re-point the ledger's chain from every ATTESTED
/// predecessor (the registry's, never an id a row asserts), then raise each
/// one's grant marks. Both idempotent by key — a second store-ready puts
/// nothing — and the first refusal ends the pair (the chain is what the
/// raise reads through).
async fn repoint_and_raise(
    ledger: &dyn SuccessionLedgerStore,
    attested: &[ActorId],
) -> Result<(), StoreError> {
    for predecessor in attested {
        ledger.repoint(*predecessor).await?;
    }
    for predecessor in attested {
        ledger.raise_grant_marks(*predecessor).await?;
    }
    Ok(())
}

/// Leg 8 — the subscriber-tier period-key rotation, reported through the
/// sink's period-rotation line. Signed with this account's own stored seed,
/// leg 4's way; a device holding none reports the failure.
///
/// ⚠ **The sharpest gap left after the mail burn.** The ceremony MOVES the
/// tier plane, key material included, because those keys seal the author's
/// own back catalogue and stranding them would be user-irrecoverable — so a
/// seed thief's copy stays live, and every audience-restricted post the
/// successor publishes afterwards opens under a key they already read. The
/// nest holds no period key and rotates none, so this leg is the only thing
/// that closes it (`succession-repoint-axis.md` § Implementation status today,
/// the tier bullet).
///
/// Here, not in the post-auth pass: the keys are this store's rows. It needs
/// no park and no ceremony fact — what it owes is derived from each tier's own
/// period stamp (`TierPeriod::minted_by`) on every pass, so it re-runs at every
/// store-ready. An unreadable custody or stored blob is a failure for that
/// tier, never "nothing owed", and the exposure stays open until a later
/// store-ready succeeds, which is why the rendered line says so.
async fn rotate_periods<R, S>(
    nest: R,
    period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
    registry: &AccountRegistry,
    self_hex: &str,
    sink: &mut S,
) where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass + core::fmt::Display,
    S: AftermathSink,
{
    use fauna_client_subscriptions::orchestration::{PeriodRotationProgress, SubscriptionsAuthor};
    let Some(secret) = registry
        .secrets(self_hex)
        .and_then(|stored| fauna_core::hex32::decode(stored.secret_hex.as_str()).ok())
    else {
        sink.period_rotation(PeriodRotationProgress::Failed(
            "this device holds no seed for the account".into(),
        ));
        return;
    };
    sink.period_rotation(PeriodRotationProgress::Running);
    let author = SubscriptionsAuthor::over(
        nest,
        fauna_core::identity::ActorKeypair::from_secret(secret),
        period_keys,
    );
    let progress = match author.rotate_period_keys_after_succession().await {
        Ok(outcome) => {
            tracing::info!(?outcome, "the tier period-key rotation leg settled");
            PeriodRotationProgress::Settled(outcome)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "the tier period-key rotation failed; it retries at the next store-ready"
            );
            PeriodRotationProgress::Failed(format!("{e}"))
        }
    };
    sink.period_rotation(progress);
}

/// Leg 4 — the capability-grant re-mint off the ledger, reported through the
/// sink's grant-remint line. Signed with this account's own stored seed
/// (`AccountRegistry::secrets`); a device holding none reports the failure.
///
/// ⚠ Every grant this re-mints is stamped un-adjudicated, and the mark
/// renders on the NESTS page, not on the recovery kit (`succession-aftermath.md`
/// § Adjudicating what the aftermath carries across). That is the whole
/// safety property of this leg: the aftermath re-confers read capability from
/// a ledger a pre-succession seed thief could have written, so it is never
/// gated — and never silent either.
async fn remint<R, S>(
    nest: R,
    ledger: &dyn SuccessionLedgerStore,
    period_keys: &dyn fauna_client_subscriptions::PeriodKeyStore,
    mail: &dyn fauna_client_config::MailStore,
    registry: &AccountRegistry,
    self_hex: &str,
    sink: &mut S,
) where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass + core::fmt::Display,
    S: AftermathSink,
{
    let Some(secret) = registry
        .secrets(self_hex)
        .and_then(|stored| fauna_core::hex32::decode(stored.secret_hex.as_str()).ok())
    else {
        sink.grant_remint(GrantRemintProgress::Failed(
            "this device holds no seed for the account".into(),
        ));
        return;
    };
    sink.grant_remint(GrantRemintProgress::Running);
    // An unreadable mail custody reads as no MSEK: a mail or calendar grant
    // then stays owed and retries at the next store-ready, like any payload
    // this device cannot derive.
    let mail = mail.load().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "the grant re-mint could not read the mail custody");
        Default::default()
    });
    let progress =
        match remint_capability_grants(nest, secret, ledger, Some(period_keys), &mail).await {
            Ok(outcome) => {
                tracing::info!(?outcome, "the capability-grant re-mint leg settled");
                GrantRemintProgress::Settled(outcome)
            }
            // Best-effort: the derived replacement id makes a retry converge on
            // the same grant, so the next store-ready re-runs it harmlessly.
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "the capability-grant re-mint failed; it retries at the next store-ready"
                );
                GrantRemintProgress::Failed(format!("{e}"))
            }
        };
    sink.grant_remint(progress);
}

/// The roster-bound raises — the member-item raise and the filter-mark raise —
/// off the registry's parked ceremony for `self_actor`, cleared only once both
/// landed.
async fn drain_parked_ceremony<R>(
    nest: &R,
    ledger: &dyn SuccessionLedgerStore,
    backup: &dyn BackupStateStore,
    registry: &AccountRegistry,
    self_actor: &ActorId,
) -> ParkedRaises
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    let self_hex = self_actor.to_hex();
    let Some(raw) = registry.pending_aftermath_ceremony(&self_hex) else {
        return ParkedRaises::NoneParked;
    };
    let Some(ceremony) = PendingCeremony::from_json(&raw).and_then(PendingCeremony::into_ceremony)
    else {
        tracing::warn!("the parked aftermath ceremony does not decode — cleared, nothing raised");
        clear_if_unchanged(registry, &self_hex, &raw);
        return ParkedRaises::Voided;
    };
    let attested = registry.attested_predecessor_actor_ids(&self_hex);
    if !attested.contains(&ceremony.raising_predecessor) {
        tracing::info!(
            predecessor = %ceremony.raising_predecessor.to_hex(),
            "the parked ceremony's raising predecessor is not attested on this device — left \
             parked, nothing raised"
        );
        return ParkedRaises::Unattested;
    }

    let members = raise_members(ledger, &ceremony).await;
    let filters = raise_filters(nest, ledger, &ceremony).await;
    let destinations = raise_destinations(backup, &ceremony).await;
    if members && filters && destinations {
        clear_if_unchanged(registry, &self_hex, &raw);
        ParkedRaises::Drained
    } else {
        ParkedRaises::StillOwed
    }
}

/// Clear the park only if it still holds exactly what this pass drained — a
/// sweep retry that re-parked a wider roster meanwhile keeps its park for the
/// next store-ready.
fn clear_if_unchanged(registry: &AccountRegistry, self_hex: &str, drained: &str) {
    if registry.pending_aftermath_ceremony(self_hex).as_deref() == Some(drained) {
        registry.clear_aftermath_ceremony(self_hex);
    }
}

/// The destination-mark raise: an `Open` mark per destination any of the
/// account's boxes lists, keyed on the park's raising predecessor. Returns
/// whether it landed.
///
/// ⚠ **Off the park, once per ceremony — a refinement made with the cut
/// (2026-09-30).** The ruling first put it with the chain and grant legs,
/// re-run at every store-ready off the attested set; but a destination has no
/// signed provenance the way a grant event does, so a raise that re-runs for
/// good would mark every destination the successor adds afterwards as carried
/// across. The blob raised once, at the re-key, before the successor could
/// have added anything; the park is the plane's "once": it is written by the
/// ceremony and cleared only after every raise landed.
async fn raise_destinations(backup: &dyn BackupStateStore, ceremony: &AftermathCeremony) -> bool {
    match raise_succession_destination_marks(backup, ceremony.raising_predecessor).await {
        Ok(raised) => {
            tracing::info!(raised, "the succession's destination marks settled");
            true
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "raising the succession's destination marks was refused; it stays parked for \
                 the next store-ready"
            );
            false
        }
    }
}

/// Leg 2 — the `NestBackupKey` re-grant off `bound`'s list, reported through
/// the sink's backup line. The granted key derives from this
/// account's own stored seed (`NestBackupKey::derive` over
/// it); a device holding none reports the failure.
async fn regrant<R, S>(
    nest: R,
    backup: &dyn BackupStateStore,
    bound: [u8; 32],
    registry: &AccountRegistry,
    self_hex: &str,
    sink: &mut S,
) where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass + core::fmt::Display,
    S: AftermathSink,
{
    let destinations = match backup.backup_state(bound).await {
        Ok(state) => state.backup.destinations,
        Err(e) => {
            tracing::warn!(error = %e, "reading the backup list for leg 2 failed; it retries");
            sink.backup_regrant(BackupRegrantProgress::Failed(format!("{e}")));
            return;
        }
    };
    if destinations.is_empty() {
        // Nothing listed on this box: nothing owed, no line.
        return;
    }
    let Some(secret) = registry
        .secrets(self_hex)
        .and_then(|stored| fauna_core::hex32::decode(stored.secret_hex.as_str()).ok())
    else {
        sink.backup_regrant(BackupRegrantProgress::Failed(
            "this device holds no seed for the account".into(),
        ));
        return;
    };
    sink.backup_regrant(BackupRegrantProgress::Running);
    let progress = match regrant_nest_backup_key(nest, secret, &destinations).await {
        Ok(outcome) => {
            tracing::info!(?outcome, "the NestBackupKey re-grant leg settled");
            BackupRegrantProgress::Settled(outcome)
        }
        // Best-effort: the reconcile is idempotent, so the next store-ready
        // re-runs it; refusing anything over it would be strictly worse than
        // backups that restart one store-ready later.
        Err(e) => {
            tracing::warn!(error = %e, "the NestBackupKey re-grant failed; it retries");
            BackupRegrantProgress::Failed(format!("{e}"))
        }
    };
    sink.backup_regrant(progress);
}

/// The group sweep's roster, written down. Returns whether it landed.
async fn raise_members(ledger: &dyn SuccessionLedgerStore, ceremony: &AftermathCeremony) -> bool {
    match raise_succession_member_reviews(
        ledger,
        ceremony.raising_predecessor,
        &ceremony.review_roster,
    )
    .await
    {
        Ok(raised) => {
            tracing::info!(?raised, "the succession's member-review roster settled");
            true
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "writing the succession's member-review roster was refused; it stays parked for \
                 the next store-ready"
            );
            false
        }
    }
}

/// The inherited-filter raise. Returns whether it landed.
///
/// ⚠ The rows are fetched **here** rather than parked. The classification
/// must see what the account owns *now*, after the ceremony re-pointed
/// `email_filters.owner`; a list cached before the switch belongs to the
/// predecessor's session. A stamp neither the park nor the nest can supply
/// ([`FilterMarkRaise::SuccessionTimeUnknown`]) is not a landing: the park
/// stays, so a nest that learns to answer later still gets the rules raised.
async fn raise_filters<R>(
    nest: &R,
    ledger: &dyn SuccessionLedgerStore,
    ceremony: &AftermathCeremony,
) -> bool
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    let filters = match fauna_client_email::EmailClient::new(nest.clone())
        .filters_list()
        .await
    {
        Ok(filters) => filters,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "reading the filter list for the inherited-filter raise failed; it stays parked"
            );
            return false;
        }
    };
    match raise_succession_filter_marks(
        nest,
        ledger,
        ceremony.raising_predecessor,
        &filters,
        ceremony.succession_time,
    )
    .await
    {
        Ok(FilterMarkRaise::SuccessionTimeUnknown) => {
            tracing::warn!(
                "the succession's recorded time is unavailable — no inherited filter raised; \
                 it stays parked"
            );
            false
        }
        Ok(raised) => {
            tracing::info!(?raised, "the succession's inherited-filter marks settled");
            true
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "writing the succession's inherited-filter marks was refused; it stays parked"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_accounts::InMemorySecretStore;
    use fauna_client_config::test_helpers::{FakeBackupStateStore, FakeSuccessionLedgerStore};
    use fauna_client_config::{BackupRegrantOutcome, BackupRegrantProgress, NoLedgerStore};
    use fauna_client_testkit::{RecordingRequester, block_on};
    use fauna_core::data::{BackupDestination, DESTINATION_KIND_NEST};
    use fauna_core::identity::ActorKeypair;
    use std::sync::Arc;

    #[derive(Default)]
    struct Log(Vec<&'static str>);

    impl AftermathSink for Log {
        fn backup_regrant(&mut self, progress: BackupRegrantProgress) {
            use fauna_core::progress::Passage;
            self.0.push(match progress {
                Passage::Running => "regrant:running",
                Passage::Settled(BackupRegrantOutcome::Regranted { .. }) => "regrant:regranted",
                Passage::Settled(_) => "regrant:settled-quiet",
                Passage::Failed(_) => "regrant:failed",
            });
        }
        fn grant_remint(&mut self, progress: fauna_client_capabilities::GrantRemintProgress) {
            use fauna_core::progress::Passage;
            self.0.push(match progress {
                Passage::Running => "remint:running",
                Passage::Settled(_) => "remint:settled",
                Passage::Failed(_) => "remint:failed",
            });
        }
        fn drafts_reseal(&mut self, _: fauna_client_drafts::DraftsResealProgress) {}
        fn mail_burn(&mut self, _: fauna_client_mail_settings::MailBurnProgress) {}
        fn period_rotation(
            &mut self,
            progress: fauna_client_subscriptions::orchestration::PeriodRotationProgress,
        ) {
            use fauna_client_subscriptions::orchestration::PeriodRotationOutcome;
            use fauna_core::progress::Passage;
            self.0.push(match progress {
                Passage::Running => "rotation:running",
                Passage::Settled(PeriodRotationOutcome::NothingHeld) => "rotation:nothing-held",
                Passage::Settled(_) => "rotation:settled",
                Passage::Failed(_) => "rotation:failed",
            });
        }
        async fn config_stage_settled(&mut self) {
            self.0.push("review-reread");
        }
    }

    /// The succession cut's custody arm, recording whom it was run as.
    #[derive(Default)]
    struct RecordingCut(std::sync::Mutex<Vec<ActorId>>);

    #[async_trait::async_trait]
    impl fauna_client_config::FolderCustodyCut for RecordingCut {
        async fn cut(&self, identity: ActorId) -> Result<usize, fauna_client_config::StoreError> {
            self.0.lock().unwrap().push(identity);
            Ok(1)
        }
    }

    /// One inherited filter (created long before the stamp), and a nest that
    /// knows the succession's time.
    fn nest_with_one_inherited_filter(kind: &'static str) -> Vec<u8> {
        match kind {
            "fauna.email.filters.list" => {
                fauna_protocol::encode_canonical(&fauna_protocol::email::ListEmailFiltersReply {
                    filters: vec![fauna_protocol::email::EmailFilter {
                        id: 7,
                        name: "rule".into(),
                        rules: Vec::new(),
                        combination: "all".into(),
                        action: fauna_protocol::email::EmailFilterAction::Discard,
                        priority: 0,
                        continue_on_match: false,
                        created_at: 1_000,
                        extra: Default::default(),
                    }],
                    extra: Default::default(),
                })
            }
            // Leg 4's read: an empty holder roster — a live predecessor grant
            // then stays owed, never minted blind.
            "fauna.bridges.list_service_users" => fauna_protocol::encode_canonical(
                &fauna_protocol::wrapped_blob::ListServiceUsersReply {
                    service_users: Vec::new(),
                    enrollment_strict: None,
                    extra: Default::default(),
                },
            ),
            // Leg 2's reads and writes: a successor's nest projects no
            // enrollment until the re-grant lands.
            "fauna.backup.status" => {
                fauna_protocol::encode_canonical(&fauna_protocol::backup::BackupStatusReply {
                    enrolled: false,
                    destinations: Vec::new(),
                    extra: Default::default(),
                })
            }
            "fauna.backup.nest_key.grant" => {
                fauna_protocol::encode_canonical(&fauna_protocol::backup::NestKeyGrantReply {
                    ok: true,
                    extra: Default::default(),
                })
            }
            "fauna.backup.destination.register" => fauna_protocol::encode_canonical(
                &fauna_protocol::backup::DestinationRegisterReply {
                    ok: true,
                    extra: Default::default(),
                },
            ),
            "fauna.recovery.succession.status" => {
                fauna_protocol::encode_canonical(&fauna_protocol::recovery::SuccessionStatusReply {
                    succeeded_at: Some(1_700_000_000),
                    ..Default::default()
                })
            }
            other => panic!("the pass sent an unexpected kind: {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    /// A registry holding a predecessor → successor link (so the predecessor
    /// is attested) — the state every post-ceremony fold leaves behind.
    struct World {
        registry: AccountRegistry,
        predecessor: ActorId,
        predecessor_kp: ActorKeypair,
        successor_kp: ActorKeypair,
        successor_hex: String,
        ledger: FakeSuccessionLedgerStore,
        /// The account's `fauna.state.backup` rows.
        backup: FakeBackupStateStore,
    }

    /// The box the pass's connection is bound to.
    const BOUND_BOX: [u8; 32] = [0xaa; 32];

    fn world() -> World {
        let registry = AccountRegistry::new(Arc::new(InMemorySecretStore::new()));
        let pred = ActorKeypair::generate();
        let succ = ActorKeypair::generate();
        let pred_hex = registry
            .add_account(&fauna_core::hex32::encode(pred.secret_bytes()), None, None)
            .expect("predecessor row");
        let successor_hex = registry
            .add_account(&fauna_core::hex32::encode(succ.secret_bytes()), None, None)
            .expect("successor row");
        registry
            .record_succession(&pred_hex, &successor_hex)
            .expect("the link");
        World {
            registry,
            predecessor: pred.actor_id(),
            ledger: FakeSuccessionLedgerStore::empty(succ.actor_id()),
            backup: FakeBackupStateStore::empty(),
            predecessor_kp: pred,
            successor_kp: succ,
            successor_hex,
        }
    }

    fn run(w: &World) -> (ParkedRaises, Log) {
        let (parked, log, _) = run_with(w, &w.backup, Some(BOUND_BOX));
        (parked, log)
    }

    /// One pass over `backup` and `bound_nest`; also answers the kinds the
    /// nest was sent.
    fn run_with(
        w: &World,
        backup: &dyn BackupStateStore,
        bound_nest: Option<[u8; 32]>,
    ) -> (ParkedRaises, Log, Vec<&'static str>) {
        let (parked, log, kinds, _) = run_cutting(w, backup, bound_nest);
        (parked, log, kinds)
    }

    /// [`run_with`], also answering whom the succession cut ran as.
    fn run_cutting(
        w: &World,
        backup: &dyn BackupStateStore,
        bound_nest: Option<[u8; 32]>,
    ) -> (ParkedRaises, Log, Vec<&'static str>, Vec<ActorId>) {
        let nest = Arc::new(RecordingRequester::new(nest_with_one_inherited_filter));
        let mut log = Log::default();
        let cut = RecordingCut::default();
        let parked = block_on(run_ledger_aftermath(
            nest.clone(),
            &w.ledger,
            backup,
            bound_nest,
            fauna_client_subscriptions::period_keys::MemoryPeriodKeyStore::new().shared(),
            &fauna_client_config::test_helpers::FakeMailStore::empty(),
            None,
            &cut,
            &w.registry,
            &mut log,
        ));
        let cut_as = cut.0.lock().unwrap().clone();
        (parked, log, nest.kinds(), cut_as)
    }

    /// Ruling (11)(a): where this device attests a predecessor, the pass runs
    /// the succession cut's custody arm as the successor it serves.
    #[test]
    fn the_pass_cuts_as_the_successor_where_a_predecessor_is_attested() {
        let w = world();
        let (_, _, _, cut_as) = run_cutting(&w, &w.backup, Some(BOUND_BOX));
        assert_eq!(cut_as, vec![w.successor_kp.actor_id()]);
    }

    fn listed(id: &str) -> BackupDestination {
        BackupDestination {
            destination_id: id.into(),
            destination_nest_url: format!("https://{id}.example/"),
            destination_actor_pubkey: [9u8; 32],
            kind: DESTINATION_KIND_NEST.to_string(),
            ..Default::default()
        }
    }

    /// The parked ceremony drains into the ledger, the park clears, and a
    /// second store-ready puts nothing — idempotent, and the re-read fires
    /// both times.
    #[test]
    fn a_parked_ceremony_drains_once_and_a_second_store_ready_puts_nothing() {
        let w = world();
        let person = ActorId([7u8; 32]);
        PendingCeremony::new(&w.predecessor, &[person], None).park(&w.registry, &w.successor_hex);

        let (parked, log) = run(&w);
        assert_eq!(parked, ParkedRaises::Drained);
        assert_eq!(
            log.0,
            vec![
                "remint:running",
                "remint:settled",
                "rotation:running",
                "rotation:nothing-held",
                "review-reread"
            ],
            "leg 4 reports, then the review surfaces re-read"
        );
        let ledger = w.ledger.current();
        assert_eq!(ledger.open_member_reviews().len(), 1, "the roster landed");
        assert_eq!(
            ledger.open_filter_reviews(),
            vec![7],
            "the inherited rule landed"
        );
        assert!(
            w.registry
                .pending_aftermath_ceremony(&w.successor_hex)
                .is_none(),
            "the park clears once both raises landed"
        );

        let merges = w.ledger.merges();
        let (parked, log) = run(&w);
        assert_eq!(parked, ParkedRaises::NoneParked);
        assert_eq!(
            log.0,
            vec![
                "remint:running",
                "remint:settled",
                "rotation:running",
                "rotation:nothing-held",
                "review-reread"
            ]
        );
        assert_eq!(
            w.ledger.merges(),
            merges,
            "a second store-ready puts nothing"
        );
    }

    /// A refused first put (the door's no-tip refusal) leaves the slot parked,
    /// and a later store-ready drains it.
    #[test]
    fn a_refused_put_leaves_the_slot_parked_and_a_later_run_drains_it() {
        let w = world();
        PendingCeremony::new(&w.predecessor, &[ActorId([7u8; 32])], Some(1_700_000_000))
            .park(&w.registry, &w.successor_hex);
        w.ledger.refuse_next_merges(1);

        let (parked, _) = run(&w);
        assert_eq!(parked, ParkedRaises::StillOwed);
        assert!(
            w.registry
                .pending_aftermath_ceremony(&w.successor_hex)
                .is_some(),
            "a refused raise stays owed, never lost"
        );

        let (parked, _) = run(&w);
        assert_eq!(parked, ParkedRaises::Drained);
        assert_eq!(w.ledger.current().open_member_reviews().len(), 1);
        assert_eq!(w.ledger.current().open_filter_reviews(), vec![7]);
    }

    /// A park whose raising predecessor this device does not attest is left
    /// untouched — never attributed to a guess, never silently dropped.
    #[test]
    fn a_park_raised_by_an_unattested_predecessor_is_left_alone() {
        let w = world();
        PendingCeremony::new(&ActorId([9u8; 32]), &[ActorId([7u8; 32])], None)
            .park(&w.registry, &w.successor_hex);

        let (parked, _) = run(&w);
        assert_eq!(parked, ParkedRaises::Unattested);
        assert!(
            w.ledger.current().open_member_reviews().is_empty()
                && w.ledger.current().open_filter_reviews().is_empty(),
            "nothing raised"
        );
        assert!(
            w.registry
                .pending_aftermath_ceremony(&w.successor_hex)
                .is_some()
        );
    }

    /// A park that no longer decodes can never be consumed, so it is cleared.
    #[test]
    fn an_undecodable_park_is_voided() {
        let w = world();
        w.registry
            .park_aftermath_ceremony(&w.successor_hex, "{not json");
        let (parked, _) = run(&w);
        assert_eq!(parked, ParkedRaises::Voided);
        assert!(
            w.registry
                .pending_aftermath_ceremony(&w.successor_hex)
                .is_none()
        );
    }

    /// The sweep retry re-parks the UNION by person under the same predecessor,
    /// keeping the parked stamp, and the next store-ready drains both rosters.
    #[test]
    fn the_retry_re_parks_a_union_that_the_next_store_ready_drains() {
        let w = world();
        let (a, b) = (ActorId([7u8; 32]), ActorId([8u8; 32]));
        PendingCeremony::new(&w.predecessor, &[a], Some(1_700_000_000))
            .park(&w.registry, &w.successor_hex);

        PendingCeremony::repark_retried_roster(
            &w.registry,
            &w.successor_hex,
            &w.predecessor,
            &[b, a],
        );
        let parked = PendingCeremony::parked(&w.registry, &w.successor_hex)
            .expect("parked")
            .expect("decodes");
        assert_eq!(parked.review_roster, vec![b.to_hex(), a.to_hex()]);
        assert_eq!(
            parked.succession_seconds,
            Some(1_700_000_000),
            "the stamp is kept"
        );

        let (outcome, _) = run(&w);
        assert_eq!(outcome, ParkedRaises::Drained);
        assert_eq!(w.ledger.current().open_member_reviews().len(), 2);
    }

    /// The park is erased with the successor's registry row.
    #[test]
    fn the_park_dies_with_the_successors_row() {
        let w = world();
        PendingCeremony::new(&w.predecessor, &[], None).park(&w.registry, &w.successor_hex);
        w.registry.remove(&w.successor_hex).expect("remove");
        assert!(
            w.registry
                .pending_aftermath_ceremony(&w.successor_hex)
                .is_none()
        );
    }

    /// A signed grant event of the given signer, live for a long while.
    fn live_grant(signer: &ActorKeypair, grant: u8) -> fauna_core::grant_event::GrantEvent {
        fauna_core::grant_event::GrantEvent {
            grant_id: vec![grant; 16],
            holder: vec![0xAA; 32],
            kind: fauna_core::grant_event::GrantEventKind::Mint,
            scope: Vec::new(),
            window_start: 1,
            window_end: u64::MAX / 2,
            at: 1,
            sig: vec![0u8; fauna_core::grant_event::GRANT_EVENT_SIGNATURE_LEN],
        }
        .sign(signer.signing_key())
        .expect("sign")
    }

    /// An identity that never succeeded: no attested predecessor, so the pass
    /// writes nothing and renders no re-mint line — every ordinary
    /// store-ready pays one registry read and the review re-read.
    #[test]
    fn an_identity_with_no_predecessor_writes_nothing_and_reports_no_remint() {
        let registry = AccountRegistry::new(Arc::new(InMemorySecretStore::new()));
        let own = ActorKeypair::generate();
        registry
            .add_account(&fauna_core::hex32::encode(own.secret_bytes()), None, None)
            .expect("the account row");
        let ledger = FakeSuccessionLedgerStore::empty(own.actor_id());
        let nest = Arc::new(RecordingRequester::new(nest_with_one_inherited_filter));
        let mut log = Log::default();
        let cut = RecordingCut::default();

        let parked = block_on(run_ledger_aftermath(
            nest,
            &ledger,
            &FakeBackupStateStore::empty(),
            Some(BOUND_BOX),
            fauna_client_subscriptions::period_keys::MemoryPeriodKeyStore::new().shared(),
            &fauna_client_config::test_helpers::FakeMailStore::empty(),
            None,
            &cut,
            &registry,
            &mut log,
        ));
        assert!(
            cut.0.lock().unwrap().is_empty(),
            "no attested predecessor — no cut on an ordinary store-ready"
        );
        assert_eq!(parked, ParkedRaises::NoneParked);
        assert_eq!(
            log.0,
            vec!["review-reread"],
            "no re-mint line for an ordinary identity"
        );
        assert_eq!(ledger.merges(), 0, "nothing written");
    }

    /// **Legs (a) and (b).** The chain re-points from the attested
    /// predecessor, the predecessor-signed grant it carried gets its `Open`
    /// mark, the successor's own grant gets none — the raise re-runs at every
    /// store-ready, so it must never ask the owner about their own act — and a
    /// second store-ready puts nothing.
    #[test]
    fn the_chain_re_points_and_only_the_predecessors_grant_is_marked_once() {
        let w = world();
        w.ledger.mutate(|l| {
            l.grant_events.push(live_grant(&w.predecessor_kp, 1));
            l.grant_events.push(live_grant(&w.successor_kp, 2));
        });

        let (parked, log) = run(&w);
        assert_eq!(parked, ParkedRaises::NoneParked);
        let ledger = w.ledger.current();
        assert_eq!(
            ledger.prior_actor_ids,
            vec![w.predecessor],
            "the chain names the attested predecessor"
        );
        assert_eq!(ledger.unattested_grant_marks.len(), 1, "one mark");
        assert_eq!(ledger.unattested_grant_marks[0].grant_id, vec![1u8; 16]);
        assert_eq!(ledger.unattested_grant_marks[0].predecessor, w.predecessor);
        assert!(ledger.unattested_grant_marks[0].verdict.is_open());
        assert_eq!(
            log.0,
            vec![
                "remint:running",
                "remint:settled",
                "rotation:running",
                "rotation:nothing-held",
                "review-reread"
            ],
            "leg 4 ran over the re-pointed chain (its holder is off the roster, so owed)"
        );

        let merges = w.ledger.merges();
        run(&w);
        assert_eq!(
            w.ledger.merges(),
            merges,
            "a second store-ready puts nothing"
        );
        assert_eq!(w.ledger.current().unattested_grant_marks.len(), 1);
    }

    /// A refused chain put (the door's no-tip refusal: the successor's first
    /// tip-sealed write could not mint yet) skips every later leg — they read
    /// through the chain — and leaves the park owed; the next store-ready runs
    /// them all.
    #[test]
    fn a_refused_chain_put_skips_the_later_legs_and_the_next_store_ready_runs_them() {
        let w = world();
        PendingCeremony::new(&w.predecessor, &[ActorId([7u8; 32])], Some(1_700_000_000))
            .park(&w.registry, &w.successor_hex);
        w.ledger.refuse_next_merges(1);

        let (parked, log) = run(&w);
        assert_eq!(parked, ParkedRaises::StillOwed);
        assert_eq!(
            log.0,
            vec!["remint:failed", "rotation:failed", "review-reread"],
            "leg 4 reports the owed chain; the review re-read still fires"
        );
        assert!(
            w.ledger.current().prior_actor_ids.is_empty(),
            "nothing re-pointed"
        );
        assert!(
            w.registry
                .pending_aftermath_ceremony(&w.successor_hex)
                .is_some()
        );

        let (parked, _) = run(&w);
        assert_eq!(parked, ParkedRaises::Drained);
        assert_eq!(w.ledger.current().prior_actor_ids, vec![w.predecessor]);
    }

    /// **The destination-mark raise rides the park.** A parked ceremony's
    /// drain raises an `Open` mark, keyed on the raising predecessor, on every
    /// destination any of the account's boxes lists; when the backup store
    /// refuses (not up yet), the member and filter raises still land but the
    /// park is kept, so the next store-ready raises the destinations.
    #[test]
    fn a_parked_ceremony_raises_the_destination_marks_and_a_refusing_store_keeps_the_park() {
        let w = world();
        w.backup.seed_list(BOUND_BOX, vec![listed("here")]);
        w.backup.seed_list([0xbb; 32], vec![listed("there")]);
        PendingCeremony::new(&w.predecessor, &[ActorId([7u8; 32])], None)
            .park(&w.registry, &w.successor_hex);

        // The backup store is not up: the park stays owed.
        let (parked, _, _) = run_with(&w, &NoLedgerStore, None);
        assert_eq!(parked, ParkedRaises::StillOwed);
        assert!(
            w.registry
                .pending_aftermath_ceremony(&w.successor_hex)
                .is_some(),
            "a refused destination raise stays owed, never lost"
        );
        assert!(
            w.backup.marks().is_empty(),
            "nothing raised through a refusing store"
        );

        // The store comes up: the next store-ready drains the park.
        let (parked, _, _) = run_with(&w, &w.backup, None);
        assert_eq!(parked, ParkedRaises::Drained);
        let mut marked: Vec<(String, bool)> = w
            .backup
            .marks()
            .into_iter()
            .filter(|m| m.predecessor == w.predecessor)
            .map(|m| (m.destination_id, m.verdict.is_open()))
            .collect();
        marked.sort();
        assert_eq!(
            marked,
            vec![("here".to_string(), true), ("there".to_string(), true)],
            "every destination on every box is raised against the predecessor"
        );
        assert!(
            w.registry
                .pending_aftermath_ceremony(&w.successor_hex)
                .is_none()
        );
    }

    /// **Leg 2 runs in this pass, off the bound box's list.** With a bound
    /// box whose list holds a destination and a nest reporting no
    /// enrollment, the re-grant reports `Running` then `Regranted` through
    /// the sink's backup line, before leg 4.
    #[test]
    fn leg_2_regrants_off_the_bound_boxs_list_and_reports_its_line() {
        let w = world();
        w.backup.seed_list(BOUND_BOX, vec![listed("dest-1")]);

        let (_, log, kinds) = run_with(&w, &w.backup, Some(BOUND_BOX));
        assert_eq!(
            log.0,
            vec![
                "regrant:running",
                "regrant:regranted",
                "remint:running",
                "remint:settled",
                "rotation:running",
                "rotation:nothing-held",
                "review-reread"
            ],
            "leg 2 reports first, then leg 4"
        );
        assert!(kinds.contains(&"fauna.backup.nest_key.grant"));
        assert!(kinds.contains(&"fauna.backup.destination.register"));
    }

    /// No bound box: leg 2 is skipped for this pass — it never guesses a box.
    /// An empty list on the bound box: nothing owed, no line, no round trip.
    #[test]
    fn leg_2_is_skipped_without_a_bound_box_and_silent_on_an_empty_list() {
        let w = world();
        w.backup.seed_list(BOUND_BOX, vec![listed("dest-1")]);
        let (_, log, kinds) = run_with(&w, &w.backup, None);
        assert!(
            !log.0.iter().any(|l| l.starts_with("regrant:")),
            "no bound box, no leg 2: {:?}",
            log.0
        );
        assert!(!kinds.contains(&"fauna.backup.status"));

        // A box with no list of its own — another box's list is not its.
        let (_, log, kinds) = run_with(&w, &w.backup, Some([0xcc; 32]));
        assert!(
            !log.0.iter().any(|l| l.starts_with("regrant:")),
            "an empty list renders nothing: {:?}",
            log.0
        );
        assert!(!kinds.contains(&"fauna.backup.status"));
    }
}
