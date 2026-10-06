//! Shared mail/settings machine construction, ported from
//! `apps/fauna-linux/src/mail_glue.rs`.
//!
//! Every constructor here is a thin adapter of the session's handles onto a
//! shared `libs/fauna-*` builder — tui *is* Rust, so unlike windows/apple/android
//! it calls those builders directly rather than through `fauna-ffi` (priority
//! #2: no per-app logic lives in this file, and none should be added to it).
//! Callers are the Settings sub-pages (`settings/mail.rs`, `settings/nests.rs`,
//! `settings/atproto.rs`, `settings/mail_spam.rs`), the post-auth session hook,
//! and the test-only `events_ensure_mail_enabled` automation command that mints
//! the actor's MSEK as e2e fixture setup for the Events page.
//!
//! Every mail-keyed builder takes the account's mail custody
//! (`fauna.state.mail`, `SettingsState::mail_store`) — the MSEK and the
//! credentials rest there and nowhere else.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_config::{MailStore, SuccessionLedgerStore};
use fauna_client_mail_settings::MailSettingsMachine;
use fauna_core::identity::ActorKeypair;

pub fn build_mail_settings_machine(
    nest: Arc<NestClient>,
    secret_hex: &str,
    mail: Arc<dyn MailStore>,
    node_url: &str,
    ledger: Arc<dyn SuccessionLedgerStore>,
    folder_keys: fauna_client_mail_settings::rpc_glue::FolderKeys,
) -> Result<MailSettingsMachine, String> {
    let keypair =
        ActorKeypair::from_secret_hex(secret_hex).map_err(|e| format!("decode secret_hex: {e}"))?;
    Ok(
        fauna_client_mail_settings::rpc_glue::build_mail_settings_machine(
            nest,
            keypair,
            mail,
            node_url,
            ledger,
            folder_keys,
        ),
    )
}

/// Build the `MailSpamMachine` for the authenticated actor (the Settings →
/// Mail → Spam page), ported from
/// `apps/fauna-linux/src/mail_glue.rs::build_mail_spam_machine`.
///
/// Needs a keypair — like mail-settings, unlike devices — because undoing a
/// **client-written** (sealed) training row runs the reseal loop, which derives
/// the actor's MSEK (`rpc_glue::build_mail_spam_machine` builds a
/// `MailSettingsMachine` from the same keypair as its `SealedModelWriter`). A
/// server-written (plaintext) row still undoes through the seam's server path,
/// so an `Err` here degrades the page rather than breaking it.
pub fn build_mail_spam_machine(
    nest: Arc<NestClient>,
    secret_hex: &str,
    mail: Arc<dyn MailStore>,
    node_url: &str,
    ledger: Arc<dyn SuccessionLedgerStore>,
) -> Result<fauna_client_mail_settings::MailSpamMachine, String> {
    let keypair =
        ActorKeypair::from_secret_hex(secret_hex).map_err(|e| format!("decode secret_hex: {e}"))?;
    Ok(
        fauna_client_mail_settings::rpc_glue::build_mail_spam_machine(
            nest, keypair, mail, node_url, ledger,
        ),
    )
}

/// Build a `MailExportMachine` for the authenticated actor (the user-facing
/// `mail-export` wizard) — **with key custody**, because tui spawns the drive
/// loop. tui is the lead app for this feature (`docs/goal/architecture/
/// testing.md` § Default app and nest mode — the TUI-first feature flow); the
/// other six still build the seam-only machine until their trickle-down.
///
/// Needs a keypair for the same class of reason as `build_mail_spam_machine`
/// above, twice over: the loop opens every record it exports under the actor's
/// complete standing key set, and it wraps the per-session blob key under the
/// actor's key (`mail-export.md` § Key material). Same keypair ⇒ same MSEK ⇒ the
/// export opens exactly what the inbox opens. `handle` names the archive's root
/// directory inside the container.
///
/// **Custody and the spawn are one change, never two.** An app given custody
/// without spawning `run_export` opens a real session that nothing drives — a
/// Progress screen stuck at zero holding one of the user's three concurrency
/// slots, which is the fake-green `Start` the export track forbids
/// (`rpc_glue::build_mail_export_machine_without_key_custody`'s own doc). So an
/// undecodable secret falls back to the custody-less machine rather than to
/// nothing: the listing, the resume list, Cancel and Discard keep working over
/// the real seam and `Start` refuses honestly before any session exists.
///
/// **Where a downloaded archive lands** (§ Download flow step 5): the user's
/// downloads directory — the same [`crate::backups::download_dir`] the snapshot
/// file-save uses, `FAUNA_E2E_DOWNLOAD_DIR` bypass included, because tui has no
/// file-save dialog to raise (`tui.md` § Declared platform absences). One
/// destination for every file this app hands back, rather than a second rule
/// the user has to learn.
pub fn build_mail_export_machine(
    nest: Arc<NestClient>,
    secret_hex: &str,
    mail: Arc<dyn MailStore>,
    node_url: &str,
    handle: &str,
) -> Result<fauna_client_mail_settings::MailExportMachine, String> {
    let keypair =
        ActorKeypair::from_secret_hex(secret_hex).map_err(|e| format!("decode secret_hex: {e}"))?;
    let save_dir = crate::backups::download_dir()
        .ok_or_else(|| "no downloads directory to save an export into".to_string())?;
    Ok(
        fauna_client_mail_settings::rpc_glue::build_mail_export_machine(
            nest, keypair, mail, node_url, handle, save_dir,
        ),
    )
}

/// The custody-less fallback for a session whose `secret_hex` would not decode
/// — see [`build_mail_export_machine`]. Kept as its own named door rather than
/// inlined at the call site so the reason a machine lacks custody is readable
/// from the type of build that produced it.
pub fn build_mail_export_machine_without_key_custody(
    nest: Arc<NestClient>,
) -> fauna_client_mail_settings::MailExportMachine {
    fauna_client_mail_settings::rpc_glue::build_mail_export_machine_without_key_custody(nest)
}

/// Build a `MailImportMachine` for the authenticated actor (the user-facing
/// `mail-import` wizard), ported from the same `build_mail_export_machine`
/// shape — like the export twin it needs **no** keypair (the user-tier import
/// RPCs derive the owning actor from the authenticated WS-RPC caller). Unlike
/// export, both the nest half (S9.4) and the native foreign-IMAP-source half
/// are REAL, not stubs — `mailbox-migration.md` § Implementation status today
/// — so this wizard can drive a genuine end-to-end import.
pub fn build_mail_import_machine(
    nest: Arc<NestClient>,
) -> fauna_client_mail_settings::MailImportMachine {
    fauna_client_mail_settings::rpc_glue::build_mail_import_machine(nest)
}

/// Build a trust-enabled `LinkedNestsMachine` for the authenticated actor (the
/// Settings → Nests page), ported from
/// `apps/fauna-linux/src/mail_glue.rs::build_linked_nests_machine_with_mail_relay_and_trust`.
///
/// One machine carries both halves the page renders: the **linking** half
/// (`fauna.pair.list` / add / unlink, plus the post-`LinkBoth` mailbox
/// auto-provision hook that makes the home-with-public-relay flow work with no
/// separate "enable mail" step) and the **trust facet** — the home nest's
/// content-processing grants, its grant-event history, the mint catalog, and
/// the backup trust rows, with Mint / Renew / Revoke / SetLens /
/// RevokeBackupSeal / RevokeBackupWriter (`nests.md` § Trust facet). All of
/// that is shared Rust (priority #2); tui only adapts its session handles, and
/// unlike windows/apple/android it calls the constructor directly rather than
/// through `fauna-ffi` (it *is* Rust).
///
/// The shared constructor builds all three trust seams internally from
/// `(nest, keypair)` — including the backup seams, so the `nest-trust-backup-*`
/// rows need no extra wiring here beyond `backup_state`, the account plane's
/// `fauna.state.backup` door (`settings::nests::backup_door`). Returns `Err` only on an unrecoverable
/// identity fault (the launch flow has already validated `secret_hex` to reach
/// Online); the caller falls back to the plain machine so the page still lists
/// / links / unlinks without the facet, mirroring linux.
#[allow(clippy::too_many_arguments)]
pub fn build_linked_nests_machine_with_trust(
    nest: Arc<NestClient>,
    secret_hex: &str,
    ledger: Arc<dyn SuccessionLedgerStore>,
    backup_state: Arc<dyn fauna_client_config::BackupStateStore>,
    blessings: Arc<dyn fauna_client_pair::BlessedNestsStore>,
    period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
    mail: Arc<dyn MailStore>,
    folder_keys: Arc<dyn fauna_client_folders::FolderKeyStore>,
) -> Result<fauna_client_pair::LinkedNestsMachine, String> {
    let keypair =
        ActorKeypair::from_secret_hex(secret_hex).map_err(|e| format!("decode secret_hex: {e}"))?;
    // The set names a web-serve paywall grant's folder is named from — folder
    // custody plus the owner's folder list over the same connection.
    let folder_names = fauna_client_pair::TrustFolderNames::new(
        &keypair,
        Arc::new(fauna_client_folders::CustodyOwnedSetNames {
            keys: folder_keys,
            nest: Arc::clone(&nest),
        }),
    );
    Ok(
        fauna_client_mail_settings::rpc_glue::build_linked_nests_machine_with_mail_relay_and_trust(
            nest,
            keypair,
            ledger,
            backup_state,
            blessings,
            period_keys,
            mail,
        )
        .with_folder_names(folder_names),
    )
}

/// Build an `AtprotoSettingsMachine` for the authenticated actor (the
/// Settings → AT Protocol page), ported from
/// `apps/fauna-linux/src/mail_glue.rs::build_atproto_settings_machine`. Needs a
/// keypair — like mail-settings, unlike devices — because the machine signs
/// the D10 delegation with it and its rotation-key custody rests on the account
/// plane (`fauna.state.atproto-identity`), sealed from that seed. The minted app-credential secrets rest on the account
/// plane instead (`atproto-pds-full.md` § D3); the caller
/// (`settings::atproto::AtprotoState::build`) wires that seam over its
/// runtime slot. Returns `Err` only on an unrecoverable
/// identity fault (the launch flow has already validated `secret_hex` to reach
/// Online).
pub fn build_atproto_settings_machine(
    nest: Arc<NestClient>,
    secret_hex: &str,
    observer: Arc<dyn fauna_atproto_settings_machine::AtprotoSettingsObserver>,
    alerts: Arc<fauna_client_alerts::CriticalAlerts>,
) -> Result<Arc<fauna_atproto_settings_machine::AtprotoSettingsMachine>, String> {
    let keypair =
        ActorKeypair::from_secret_hex(secret_hex).map_err(|e| format!("decode secret_hex: {e}"))?;
    // The app-wide critical-alerts registry (`critical-alerts.md` § Feeders):
    // the machine's genesis-seniority custody check posts here on a confirmed
    // mismatch and clears on a passing re-check, and `ui::render_shell` paints
    // it as the every-page banner. Passing `None` here is what made the check
    // run-and-log-only, which is how tui shipped from S4-C until this wiring.
    Ok(
        fauna_atproto_settings_machine::build_atproto_settings_machine(
            nest,
            keypair,
            observer,
            Some(alerts),
        ),
    )
}

/// Opportunistically refresh the published mail content-sealing epoch
/// schedule on every successful (re)connect
/// (`docs/goal/architecture/encryption-at-rest.md` § Capability tiering →
/// *Content-sealing epochs*), mirroring linux's
/// `FaunaClient::refresh_mail_epoch_schedule` posture exactly: fired at the
/// same universal post-auth hook `session::establish` wires everything else
/// off, best-effort/log-only. Delegates to the shared, idempotent
/// `MailSettingsMachine::refresh_epoch_schedule`, which no-ops when mail
/// isn't enabled (no MSEK) — so this is always safe to call unconditionally.
/// Without it, a schedule published at enable-mail time slides stale past
/// the 26-week `MAIL_EPOCH_PUBLISH_HORIZON` for an actor who never re-runs
/// enable/rotate; the design's § 3 step 2 degradation (seal under the newest
/// published epoch — still correct, just coarser) covers that gap safely in
/// the meantime, so a failure here is never surfaced to the user.
pub fn spawn_refresh_mail_epoch_schedule(
    nest: Arc<NestClient>,
    secret_hex: &str,
    mail: Arc<dyn MailStore>,
    node_url: &str,
) {
    // The epoch-schedule refresh never touches the grant log, so the machine
    // carries no ledger.
    let machine = match build_mail_settings_machine(
        nest,
        secret_hex,
        mail,
        node_url,
        Arc::new(fauna_client_config::NoLedgerStore),
        // The epoch refresh never reads the served state.
        None,
    ) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("refresh_mail_epoch_schedule: build machine: {e}");
            return;
        }
    };
    tokio::spawn(async move {
        match machine.refresh_epoch_schedule().await {
            Ok(()) => tracing::debug!(
                "mail epoch schedule refresh: no-op or republished (mail disabled \
                 no-ops silently)"
            ),
            Err(e) => tracing::warn!(
                "mail epoch schedule refresh failed (best-effort; re-converges next connect): {e}"
            ),
        }
    });
}

/// The post-claim serving enablement (`onboarding.md` § 3b *Mechanism*): hand
/// the four intents read off the onboarding machine to the shared
/// [`fauna_client_mail_settings::rpc_glue::dispatch_post_claim_serving_enablement`],
/// which owns the whole firing — the first-setup mail provision (admin mailbox
/// or new-user auto-mint), the three DAV deployment toggles, and the
/// one-MSEK-mint-path companion mints — and publishes the
/// `fauna_e2e_agent::SERVING_ENABLEMENT_KEY` completion anchor. Reached only
/// from the wizard's `LoggedIn` handoff; a returning-user relaunch never calls
/// it. Spawned: the handoff must not wait on the nest.
pub fn apply_post_claim_serving_enablement(
    nest: Arc<NestClient>,
    secret_hex: &str,
    mail: Arc<dyn MailStore>,
    node_url: &str,
    ledger: Arc<dyn SuccessionLedgerStore>,
    intents: fauna_client_mail_settings::serving_enablement::ServingEnablementIntents,
) {
    let keypair = match ActorKeypair::from_secret_hex(secret_hex) {
        Ok(k) => k,
        Err(e) => {
            tracing::error!("apply_post_claim_serving_enablement: decode secret_hex: {e}");
            return;
        }
    };
    tokio::spawn(
        fauna_client_mail_settings::rpc_glue::dispatch_post_claim_serving_enablement(
            nest,
            keypair,
            mail,
            node_url.to_string(),
            ledger,
            intents,
        ),
    );
}
