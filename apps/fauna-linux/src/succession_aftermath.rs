//! The post-succession **aftermath**, driven — linux's leg of the ordered pass a
//! successor's sign-in runs (`succession-aftermath.md` § Re-key scope).
//!
//! **The pass is not here.** `fauna_client_recovery::aftermath::run_succession_aftermath`
//! owns the legs, their order (1, 2, 4, 7, 6) and every barrier between them —
//! read that module before changing anything about sequencing. Until this module
//! existed linux rendered the progress lines with nothing filling them, and a
//! successor's inherited account state, backups and mail passwords stayed exactly
//! as the retired identity left them. What lives here
//! is only what linux alone can answer, the same split tui's
//! `session::rekey_config_from_predecessors` draws:
//!
//! - **who is a successor** — the account registry's `predecessors_of`, so an
//!   ordinary identity pays one in-memory walk and starts nothing ([`inputs`]);
//! - **which predecessor material this device holds** — the registry's own
//!   `predecessor_backup_keys_by_actor` walk, never a hand-rolled filter
//!   (`AftermathInputs::config_predecessors`' ⚠);
//! - **when the store is ready** — the post-store-ready half
//!   ([`run_ledger`], `fauna_client_recovery::ledger_aftermath`) runs at the
//!   account runtime's install edge, off the ceremony the fold parked in the
//!   account registry;
//! - **where each leg's progress goes** — [`AftermathUi`], onto the Recovery
//!   kit section.
//!
//! The `__mls` (leg 3) and file-corpus (leg 5) re-seals are not this pass on any
//! app — one is a barrier inside `MlsStateSync::load`, the other the sync
//! agent's — so nothing here writes their lines.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_config::SuccessionLedgerStore;
use fauna_client_recovery::aftermath::{AftermathInputs, AftermathSink};
use fauna_core::crypto::BackupKey;
use fauna_core::identity::ActorId;

use crate::app::{DataMessage, UiMessage};
use crate::client::UiSender;
use crate::settings::recovery_kit::AftermathUpdate;

/// The pass's inputs for `actor_id`, or `None` for an identity that never
/// succeeded — which starts nothing and renders nothing.
///
/// `predecessor_rows` is the registry's `predecessors_of` walk and `resolve` its
/// `predecessor_backup_keys_by_actor` walk, taken from the caller
/// (`FaunaClient::run_succession_aftermath`) so this decision stays testable
/// without a registry, and so the second walk — which reads secrets — only runs
/// for a successor.
///
/// A row this device holds no secret for resolves to nothing and is skipped,
/// never refused: that is the ordinary state on a device the user did not
/// succeed from, and the pass still runs so it can report the drafts as owed by
/// another device instead of silently doing nothing. Fewer resolved keys than
/// rows is therefore meaningful, not a bug.
pub(crate) fn inputs(
    predecessor_rows: &[String],
    resolve: impl FnOnce() -> Vec<(ActorId, BackupKey)>,
    owner_secret: [u8; 32],
) -> Option<AftermathInputs> {
    if predecessor_rows.is_empty() {
        return None;
    }
    let predecessor_keys: Vec<BackupKey> = resolve().into_iter().map(|(_, key)| key).collect();
    if predecessor_keys.len() != predecessor_rows.len() {
        tracing::warn!(
            rows = predecessor_rows.len(),
            resolved = predecessor_keys.len(),
            "some predecessor rows did not resolve to material this device holds — the \
             aftermath skips them and another device is owed the pass"
        );
    }
    Some(AftermathInputs {
        owner_secret,
        predecessor_keys,
    })
}

/// Run the shared pass to completion, reporting into `sink`. Spawned on the
/// client's runtime by `FaunaClient::run_succession_aftermath`.
pub(crate) async fn run(nest: Arc<NestClient>, inputs: AftermathInputs, mut sink: AftermathUi) {
    fauna_client_recovery::aftermath::run_succession_aftermath(nest, inputs, &mut sink).await;
}

/// Run the post-store-ready half
/// (`fauna_client_recovery::ledger_aftermath::run_ledger_aftermath`) over the
/// freshly installed account-store handle — spawned once from
/// `account_runtime::install`'s installed arm, and again after a sweep retry
/// re-parked its roster. It drains a ceremony the succession fold parked in
/// the registry (the member-item and filter-mark raises), then re-reads both
/// review surfaces; on every ordinary sign-in it is one registry read and the
/// re-read.
pub(crate) async fn run_ledger(
    nest: Arc<NestClient>,
    handle: fauna_sync_engine::account_runtime::AccountStoreHandle,
    tx: UiSender,
) {
    // The same handle is the period-key custody legs 4 and 8 read.
    let period_keys: fauna_client_subscriptions::SharedPeriodKeyStore = Arc::new(handle.clone());
    // The same handle serves the ledger and the mail custody the re-mint
    // derives mail and calendar payloads from.
    let handle_mail = handle.clone();
    // ...and the `fauna.state.backup` door leg 2 (the `NestBackupKey` re-grant)
    // reads this box's destination list through.
    let backup: Arc<dyn fauna_client_config::BackupStateStore> = Arc::new(handle.clone());
    let ledger: Arc<dyn SuccessionLedgerStore> = Arc::new(handle);
    // Leg 6 (the mail burn) drives a mail-settings machine over the same
    // custody — it runs the shared rotate-mail-keys flow, which lives on it.
    let mail_burn = fauna_client_mail_settings::rpc_glue::build_mail_settings_machine_for_session(
        Arc::clone(&nest),
        Arc::new(handle_mail.clone()),
        Arc::clone(&ledger),
    )
    .map(Arc::new);
    // The box that list is keyed by. Unprovable here (a dropped connection) ⇒
    // `None`, which skips leg 2 for this pass rather than guessing a box.
    let bound_nest = crate::client::bound_source_nest(&nest)
        .await
        .inspect_err(|e| tracing::warn!("aftermath: bound nest id unresolved, leg 2 skipped: {e}"))
        .ok();
    let mut sink = AftermathUi::new(tx, Some(Arc::clone(&ledger)));
    // The succession cut's custody arm runs over the same account's folder-key
    // custody (`writer-signed-change-records.md` ruling (11)(a)).
    let custody_cut = fauna_client_folders::SetCustodyCut::new(
        fauna_client_folders::FoldersClient::new(Arc::clone(&nest)),
        crate::account_runtime::folder_key_store(),
    );
    let parked = fauna_client_recovery::ledger_aftermath::run_ledger_aftermath(
        nest,
        ledger.as_ref(),
        backup.as_ref(),
        bound_nest,
        period_keys,
        &handle_mail,
        mail_burn,
        &custody_cut,
        &crate::account_registry(),
        &mut sink,
    )
    .await;
    tracing::debug!(?parked, "the post-store-ready aftermath pass settled");
}

/// linux's [`AftermathSink`]: every leg's progress becomes one
/// `DataMessage::AftermathProgress`, which `app.rs` folds into the Recovery kit
/// section, and the one hook re-reads the review surfaces.
pub(crate) struct AftermathUi {
    tx: UiSender,
    /// The succession-ledger seam the hook re-reads the two review planes
    /// through — `Some` on the post-store-ready pass (the one that fires the
    /// hook), `None` on the post-auth pass, which never does.
    reviews_store: Option<Arc<dyn SuccessionLedgerStore>>,
}

impl AftermathUi {
    pub(crate) fn new(tx: UiSender, reviews_store: Option<Arc<dyn SuccessionLedgerStore>>) -> Self {
        Self { tx, reviews_store }
    }

    fn report(&self, update: AftermathUpdate) {
        self.tx
            .send(UiMessage::Data(DataMessage::AftermathProgress(update)));
    }
}

impl AftermathSink for AftermathUi {
    fn backup_regrant(&mut self, progress: fauna_client_config::BackupRegrantProgress) {
        self.report(AftermathUpdate::BackupRegrant(progress));
    }

    fn grant_remint(&mut self, progress: fauna_client_capabilities::GrantRemintProgress) {
        self.report(AftermathUpdate::GrantRemint(progress));
    }

    fn drafts_reseal(&mut self, progress: fauna_client_drafts::DraftsResealProgress) {
        self.report(AftermathUpdate::DraftsReseal(progress));
    }

    fn mail_burn(&mut self, progress: fauna_client_mail_settings::MailBurnProgress) {
        self.report(AftermathUpdate::MailBurn(progress));
    }

    /// Read back what the ceremony's two silent raises just wrote, so a
    /// successor's very first session shows the member marks on the contacts
    /// list and the inherited-filter count on the Recovery kit section rather
    /// than waiting for the next sign-in. Two reads, not one: the two planes are
    /// refreshed by different gestures everywhere else too (tui's hook, same
    /// shape). A failed read leaves both surfaces as they were — collapsing it
    /// into "nothing flagged" would hide a flagged person.
    async fn config_stage_settled(&mut self) {
        let Some(store) = self.reviews_store.clone() else {
            return;
        };
        match fauna_client_config::load_member_reviews(store.as_ref()).await {
            Ok(reviews) => self
                .tx
                .send(UiMessage::Data(DataMessage::MemberReviewsLoaded {
                    reviews,
                })),
            Err(e) => tracing::warn!(
                error = %e,
                "reading the member-review roster after the aftermath's config stage failed"
            ),
        }
        match fauna_client_config::load_filter_marks(store.as_ref()).await {
            Ok(ids) => self.report(AftermathUpdate::InheritedFilters(ids.len())),
            Err(e) => tracing::warn!(
                error = %e,
                "reading the inherited-filter marks after the aftermath's config stage failed"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(byte: u8) -> ActorId {
        ActorId([byte; 32])
    }

    fn resolved_one() -> Vec<(ActorId, BackupKey)> {
        vec![(actor(1), BackupKey::derive(&[1; 32]))]
    }

    /// **The gate that keeps every ordinary sign-in free**: no predecessor row
    /// means no pass, and the secret-reading walk is never paid.
    #[test]
    fn an_identity_with_no_predecessors_starts_nothing() {
        let mut resolved = false;
        let none = inputs(
            &[],
            || {
                resolved = true;
                resolved_one()
            },
            [7; 32],
        );
        assert!(none.is_none(), "no predecessors means no pass");
        assert!(
            !resolved,
            "an ordinary identity must not pay the secret-reading walk"
        );
        let ran =
            inputs(&[actor(1).to_hex()], resolved_one, [7; 32]).expect("a successor runs the pass");
        assert_eq!(ran.predecessor_keys.len(), 1);
    }

    /// The glue's half of "surfaced with progress": the spawned pass's reports
    /// reach this app's UI channel as the message `app.rs` folds, the drafts
    /// leg's start first. The *ordering* of every later leg is the shared module's own
    /// `the_legs_report_in_the_order_the_ordering_rules_require`, for all apps
    /// at once.
    #[test]
    fn a_successors_surface_hears_the_pass_start_first() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a runtime");
        let _guard = rt.enter();
        let seed = [9; 32];
        // Never connected — port 1, refused — so nothing but the start can beat
        // it to the channel; this asserts which message arrives first, not how
        // soon (convention 14: a generous ceiling, never a settle-sleep).
        let nest = NestClient::new(
            "http://127.0.0.1:1".into(),
            fauna_core::identity::ActorKeypair::from_secret(seed),
        );
        let (tx, rx) = crate::client::ui_channel();
        let inputs = AftermathInputs {
            owner_secret: seed,
            predecessor_keys: resolved_one().into_iter().map(|(_, key)| key).collect(),
        };
        rt.spawn(run(nest, inputs, AftermathUi::new(tx, None)));

        let first = rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("a successor's surface hears the pass start");
        match first {
            UiMessage::Data(DataMessage::AftermathProgress(AftermathUpdate::DraftsReseal(
                progress,
            ))) => assert_eq!(
                progress,
                fauna_client_drafts::DraftsResealProgress::Running,
                "the first thing a successor's surface hears is that the pass started"
            ),
            other => panic!("expected the drafts leg's Running report first, got {other:?}"),
        }
        drop(_guard);
        rt.shutdown_background();
    }
}
