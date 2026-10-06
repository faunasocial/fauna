//! Per-account (actor-id-scoped) state directory for tui's own stores under
//! its config dir — `<config>/fauna-tui/<actor-hex>/` holds `mls_state.db`,
//! the scope's sync `device.db` and the P2P sub-page's WireGuard keypair
//! (`settings/p2p.rs::P2pState::build`, `wg.key`), resolved through the one
//! shared derivation [`fauna_sync_engine::db::actor_state_dir_or_unresolved`]
//! (`account-scoping.md` § The scoping taxonomy, class 1) — the linux
//! `account_scope.rs` twin. Nothing is ever read from the flat base itself: a
//! store sitting there is no account's.

use std::path::{Path, PathBuf};

use crate::app::App;

/// Resolve — and create — actor `actor_id_hex`'s state directory under tui's
/// config dir. Always returns a usable directory, even on the
/// (should-never-happen) no-config-dir path — degraded to a temp base rather
/// than a hard failure, since every caller here is a best-effort post-auth
/// init; a malformed actor id lands in the shared `-unresolved-` scope.
pub(crate) fn account_state_dir(actor_id_hex: &str) -> PathBuf {
    let flat =
        crate::session::config_dir().unwrap_or_else(|| std::env::temp_dir().join("fauna-tui"));
    account_state_dir_under(&flat, actor_id_hex)
}

/// The pure half of [`account_state_dir`], the base passed in rather than
/// read from the environment — so tests never mutate process env.
/// `pub(crate)` for `media`'s device-id seam, which resolves the same scope.
pub(crate) fn account_state_dir_under(flat: &Path, actor_id_hex: &str) -> PathBuf {
    let dir = fauna_sync_engine::db::actor_state_dir_or_unresolved(flat, actor_id_hex);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(
            "account state dir {} could not be created: {e}",
            dir.display()
        );
    }
    dir
}

/// tui's install-scoped sync dir under the flat base: home of the install
/// device secret (`fauna_sync_engine::engine_lifecycle::INSTALL_DEVICE_SECRET_FILE`)
/// that every account's device id is derived from. It names no account and
/// no sweep names it, so it survives sign-out (`sync-agent-credentials.md`
/// § Credential model, the 2026-09-20 ruling).
pub(crate) fn install_sync_dir_under(flat: &Path) -> PathBuf {
    flat.join("sync")
}

/// Become (or remain) one of this account's serving instances, or exit —
/// tui's leg of the (OS login, account) lock (`account-scoping.md`
/// § Concurrent instances).
///
/// **tui is the first retired app (W5.6 (account-data-plane.md § Workstreams), 2026-08-15): it serves
/// [`ServingMode::Concurrent`]** — same-account instances coexist over the
/// multi-process-safe account store, each holding a *shared* per-account
/// lock (which keeps `is_served` truthful for the chooser), with the three
/// genuinely exclusive critical sections carrying their own locks; the
/// conversations surface of a non-role-holder refuses honestly via
/// `MlsError::ServedElsewhere` rather than this process exiting.
///
/// The holder logic is shared
/// ([`fauna_client_accounts::become_process_session_instance`]) — reuse on a
/// same-account rebuild, swap on a cross-account switch, bound-or-refuse,
/// degrade open. This wrapper adds only tui's two platform duties: resolving
/// the install-scoped base ([`crate::session::config_dir`], the same base the
/// per-actor scope dirs sit in) and reporting the outcome.
///
/// **Refusal is still terminal** — rarer now, not gone: `AlreadyServed`
/// survives exactly for an *exclusive* holder (a process serving this
/// account in exclusive mode),
/// and `BoundMismatch` is mode-independent ("launch bound or refuse").
/// The caller never falls back onto another account.
///
/// stderr as well as `tracing`: `exit` skips the log appender's flush, and a
/// TUI user who just lost a launch needs the reason on the terminal they
/// launched from.
///
/// [`ServingMode::Concurrent`]: fauna_client_accounts::ServingMode::Concurrent
pub(crate) fn become_session_instance_or_exit(actor_id_hex: &str) {
    match with_serving_bases(|bases| {
        fauna_client_accounts::become_process_session_instance(
            bases,
            actor_id_hex,
            fauna_client_accounts::ServingMode::Concurrent,
        )
    }) {
        fauna_client_accounts::SessionInstanceOutcome::Acquired
        | fauna_client_accounts::SessionInstanceOutcome::Reused => {}
        fauna_client_accounts::SessionInstanceOutcome::Degraded(cause) => {
            // The guard narrows a race; it must not widen a failure into a
            // client that refuses to launch (works-out-of-the-box).
            tracing::warn!(
                "[instance-lock] degraded acquire for {actor_id_hex} ({cause:?}) — proceeding unguarded"
            );
        }
        fauna_client_accounts::SessionInstanceOutcome::Refused(refusal) => {
            refusal.exit(actor_id_hex);
        }
    }
}

/// The launch-collision chooser's list: the registry's accounts that no live
/// instance currently serves, as `(actor_id, display label)` in registry
/// order. The account that collided is excluded by the probe itself.
///
/// Thin wrapper over the shared [`fauna_client_accounts::choosable_accounts`]
/// (every chooser platform — windows, linux, tui — answers "which accounts
/// may I offer?" identically). Base and registry rows are passed in rather
/// than read from the environment, so the filter is testable without a real
/// `App`/credential store (same env-mutation-free reasoning as
/// [`account_state_dir_under`]). Its one caller is
/// `launch::collision_chooser_surface`, the pure decision function that
/// resolves both from the app; a `None` base never reaches here — it means no
/// lock files to probe and no way to tell free from served, which that
/// function answers by declining the collision entirely rather than offering
/// everything.
pub(crate) fn choosable_accounts_under(
    base: &Path,
    entries: &[(String, Option<String>)],
) -> Vec<(String, String)> {
    fauna_client_accounts::choosable_accounts(base, entries)
}

/// Erase actor `actor_id_hex`'s account-scoped local state — the main scoped
/// dir (`mls_state.db`, the P2P `wg.key`) plus the backup-audit dir
/// ([`crate::backup_audit`], the `<flat>/backup/<actor-hex>/` base
/// `backup_audit::state_path` scopes its `audit-state.json` under) **plus the
/// W3 account store under the W6 unified root** — the linux
/// `account_scope::erase_under` twin. Best-effort — a missing dir is a silent
/// no-op, and a caller with no resolvable config dir does not call this at all
/// (nothing was attempted, so nothing was left behind). This is the *erasure*
/// half of `account-scoping.md`'s isolation contract ("Erasure follows
/// scope"): a plain switch or app quit must PRESERVE state (never call this),
/// only sign-out ([`erase_all_known_accounts`]) and "remove account"
/// ([`remove_account`], which asks first) do.
///
/// ⚠ **The account store is NOT under the flat base, and that is the whole
/// reason this takes a second root.** W6 path unification put it at
/// `StoreRoot::platform()` = `<config>/fauna/sync/<actor>/store`, shared with
/// linux and the sync agent, while tui's flat base is `<config>/fauna-tui`.
/// This doc used to claim the main scoped dir held the store, and the erase
/// iterated the flat base alone — so from W6 until 2026-08-18 a tui sign-out
/// wiped the credential namespace (the T10 slot holding the store's writer
/// key) while leaving the store itself on disk, and the next sign-in minted a
/// fresh key the store then refused forever. See the covering test.
///
/// Both bases are passed in — same env-mutation-free testing reasoning as
/// [`account_state_dir_under`]. The iterate-and-remove loop itself is
/// [`fauna_sync_engine::db::erase_actor_state`] (the linux twin's identical
/// copy) — this is only tui's own base list, [`erase_bases`].
///
/// **Returns what survived** — the paths the sweep tried to remove and could
/// not. Non-empty means the erased account's data is still readable on this
/// device.
fn erase_under(flat: &Path, store_root: &Path, actor_id_hex: &str) -> Vec<PathBuf> {
    let bases = erase_bases(flat, store_root);
    fauna_sync_engine::db::erase_actor_state(bases.iter().map(PathBuf::as_path), actor_id_hex)
}

/// Erase every currently-registered account's scoped local state — the
/// sign-out ("all-accounts erase") half of the same corollary. **Call
/// BEFORE the credential namespace itself is wiped**, since this reads the
/// registry to know which actors existed; calling it after would find an
/// already-empty registry and erase nothing.
///
/// **Returns what survived, folded across every account.** Best-effort is not
/// the same as silent: the shared per-actor sweep names the paths it could not
/// remove, so this no longer has to choose between completing the sign-out and
/// telling the truth about it. A non-empty return means the signed-out user's
/// data is still readable on this device, which is what
/// [`crate::app::App::reset`] turns into a line the user can see
/// (`account-scoping.md` § Erasure follows scope, the corollary on telling the
/// user).
#[must_use = "a sign-out that drops this reports success over a device that may \
              still hold the user's data - the defect this return exists to prevent"]
pub(crate) fn erase_all_known_accounts(app: &App) -> Vec<PathBuf> {
    let Some(flat) = crate::session::config_dir() else {
        return Vec::new();
    };
    let actors: Vec<String> = crate::session::registry(app)
        .list()
        .into_iter()
        .map(|entry| entry.actor_id)
        .collect();
    erase_all_known_accounts_under(
        &flat,
        fauna_sync_engine::root::StoreRoot::platform().base(),
        &actors,
    )
    .survivors
}

/// The pure half of [`erase_all_known_accounts`], both bases and the actor list
/// passed in — same env-mutation-free reasoning as [`erase_under`].
///
/// Folded through [`fauna_sync_engine::db::EraseSweep`], never a `Vec::extend`:
/// the two passes below deliberately overlap (the residual sweep re-walks the
/// same bases the per-actor loop just did), and `absorb` owns the rule that a
/// survivor is a *location*, so one undeletable scope is one item on the user's
/// line rather than two. linux told its users *"2 item(s)"* about a single
/// directory until it folded this way.
fn erase_all_known_accounts_under(
    flat: &Path,
    store_root: &Path,
    actors: &[String],
) -> fauna_sync_engine::db::EraseSweep {
    let mut sweep = fauna_sync_engine::db::EraseSweep::default();
    for actor in actors {
        sweep.absorb(fauna_sync_engine::db::EraseSweep {
            // `erase_under` reports only what it could NOT remove; this
            // function's contract is the survivors, so 0 here is honest.
            erased: 0,
            survivors: erase_under(flat, store_root, actor),
        });
    }
    sweep.absorb(erase_residual_scopes_under(flat, store_root));
    sweep
}

/// Sweep what the per-actor loop above **cannot reach**: any actor scope the
/// registry no longer names, under each of the three [`erase_bases`]. Twin of
/// linux's `account_scope::erase_all_scopes_under`. An actor the registry has
/// forgotten (which is exactly what a *failed* erase can create) has no entry to
/// iterate, so without this its scope would never be re-swept by any later
/// sign-out.
///
/// Anything that is not an actor scope **survives**, which is how tui's
/// install-scoped state keeps its class-2 disposition through a sign-out: the
/// log files (`session::install_logging`), the sealed credential file and the
/// install device secret under [`install_sync_dir_under`] are all untouched.
fn erase_residual_scopes_under(
    flat: &Path,
    store_root: &Path,
) -> fauna_sync_engine::db::EraseSweep {
    let mut sweep = fauna_sync_engine::db::EraseSweep::default();
    for base in erase_bases(flat, store_root) {
        sweep.absorb(fauna_sync_engine::db::erase_all_account_scopes(&base));
    }
    sweep
}

/// The residue as the `sign-out-residue` view paints it: the install-scoped
/// record the re-sweep needs, and the one line the user is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResidueSurface {
    pub(crate) record: fauna_client_accounts::SignOutResidue,
    /// `sign-out-residue-message` — the shared `Rendered` copy, or the retry's
    /// own refusal while another instance serves one of the residue's accounts.
    pub(crate) line: String,
}

/// Turn what a sign-out's erase left behind into the reports it owes: the
/// survivor **paths** to the log, for whoever debugs the next one
/// (`account-scoping.md` § Erasure follows scope → the ⚠ *the erase must SAY
/// what it did* corollary), the **record** to install-scoped state so it
/// outlives this process, and the **count** to the user, as the
/// `sign-out-residue` view `identity_choice` paints
/// (`wizard::identity_choice::residue_elements`).
///
/// `None` is the clean outcome and the only one — an empty sweep says nothing,
/// because *"0 items were left behind"* is reassurance by vacuity, and it leaves
/// no record on disk. Every word of the line is the shared projection's; tui
/// only localizes it and declares that it paints the retry control.
///
/// ⚠ **Both halves of the erase, or it answers for half a sign-out.**
/// `credentials` is what the credential erase left
/// (`fauna_credential_store::erase_all_credentials`), so the caller can only
/// build this after that erase has run — which is the point: built beside the
/// filesystem erase, it painted a clean line over a keyring that refused the
/// wipe.
pub(crate) fn record_residue(
    survivors: &[PathBuf],
    credentials: &fauna_client_accounts::CredentialSweep,
) -> Option<ResidueSurface> {
    log_residue(survivors, credentials);
    let record = fauna_client_accounts::SignOutResidue::record(survivors, credentials);
    persist_residue(&record);
    surface_for(record)
}

/// Remove Again (`sign-out-residue-retry-button`): re-sweep what the painted
/// residue recorded and paint what is left — nothing, when the device is now
/// clean.
pub(crate) fn retry_residue(app: &mut App) {
    let Some(surface) = app.sign_out_residue.take() else {
        return;
    };
    app.sign_out_residue = run_residue_retry(app, surface.record);
}

/// The signed-out launch's silent re-check: a record a previous sign-out left
/// is re-swept FIRST, and the view is painted only if something is still left
/// (`account-scoping.md` § Erasure follows scope → *the residue surface*).
///
/// Only on a launch with no account in the registry — the launch a sign-out
/// hands back. A launch that is signed in, or mid-onboarding with an identity
/// already stored, is not the user this residue was reported to, and its
/// scopes are not residue; the record waits for the next signed-out launch.
pub(crate) fn recheck_residue_at_launch(app: &mut App) {
    let Some(base) = crate::session::config_dir() else {
        return;
    };
    let Some(record) = fauna_client_accounts::SignOutResidue::load(&base) else {
        return;
    };
    if !crate::session::registry(app).list().is_empty() {
        return;
    }
    app.sign_out_residue = run_residue_retry(app, record);
}

/// The one re-sweep both gestures run — the shared
/// [`fauna_client_accounts::retry_sign_out_residue`], behind the sign-out's own
/// serving-lock question and over the recorded paths only (never a second
/// sign-out: the launch-time run of the full sweep is what once erased a live
/// sibling's store).
fn run_residue_retry(
    app: &mut App,
    record: fauna_client_accounts::SignOutResidue,
) -> Option<ResidueSurface> {
    let registry = crate::session::registry(app);
    let mut erased_credentials = false;
    let outcome = with_serving_bases(|bases| {
        fauna_client_accounts::retry_sign_out_residue(&record, bases, |recorded| {
            erased_credentials = true;
            fauna_credential_store::re_erase_credentials(&registry, &app.credentials, recorded)
        })
    });
    // The sealed headless arm's wipe relocks the store, exactly as the
    // sign-out's does (`App::erase_after_sign_out`): the create surface must
    // come back before the wizard can persist anything.
    if erased_credentials && app.credentials.needs_unlock() {
        app.launch = crate::launch::LaunchSurface::CreatePassphrase;
    }
    match outcome {
        fauna_client_accounts::ResidueRetry::Blocked(_) => Some(ResidueSurface {
            record,
            line: fauna_client_accounts::sign_out_residue_retry_blocked_copy()
                .resolve(fauna_i18n::strings::lookup),
        }),
        fauna_client_accounts::ResidueRetry::Swept(left) => {
            let paths: Vec<PathBuf> = left.paths.iter().map(|p| p.path.clone()).collect();
            log_residue(&paths, &left.credentials());
            persist_residue(&left);
            surface_for(left)
        }
    }
}

/// The paint for an owing record; `None` for a clean one.
fn surface_for(record: fauna_client_accounts::SignOutResidue) -> Option<ResidueSurface> {
    let line = record
        .view()
        .copy()
        .warning?
        .resolve(fauna_i18n::strings::lookup);
    Some(ResidueSurface { record, line })
}

/// Write the record into tui's install base — or remove it, when clean. A
/// failed write is logged and not fatal: the view still paints for this
/// process, and the paths are in the log the erase wrote.
fn persist_residue(record: &fauna_client_accounts::SignOutResidue) {
    let Some(base) = crate::session::config_dir() else {
        return;
    };
    if let Err(e) = record.save(&base) {
        tracing::warn!(
            "[reset] the sign-out residue record under {} could not be written: {e}",
            base.display()
        );
    }
}

/// The residue's log half — paths and key names, which never reach the user.
fn log_residue(survivors: &[PathBuf], credentials: &fauna_client_accounts::CredentialSweep) {
    if !survivors.is_empty() {
        tracing::warn!(
            "[reset] {} path(s) SURVIVED the erase and still hold this user's data: {}",
            survivors.len(),
            survivors
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !credentials.is_clean() {
        tracing::warn!(
            "[reset] sign-in credentials SURVIVED the erase (namespace wipe failed: {}); \
             still readable: [{}]",
            credentials.wipe_failed,
            credentials.survivors.join(", ")
        );
    }
}

/// Run `f` with the two directories this instance is keyed under: tui's own
/// install base (the launch law's lock) and the per-user account-store root every
/// app on this OS login shares (the cross-app presence lock) —
/// `account-scoping.md` § Concurrent instances → *An erase refuses while a
/// sibling serves the account*. One helper so the instance that *declares* itself
/// and the erase that *asks* can never disagree about where.
fn with_serving_bases<T>(f: impl FnOnce(fauna_client_accounts::ServingBases<'_>) -> T) -> T {
    let base = crate::session::config_dir();
    let store_root = fauna_sync_engine::root::StoreRoot::platform();
    f(fauna_client_accounts::ServingBases {
        state_base: base.as_deref(),
        store_root: Some(store_root.base()),
    })
}

/// The three bases every tui erase sweeps — the flat base, its backup-audit
/// base and the W6 account-store root. Named once, because the refusal must ask
/// about exactly what the erase will reach ([`all_accounts_erase_blocked_under`]):
/// a base the erase sweeps and the question skips is an account erased under a
/// live sibling. [`erase_residual_scopes_under`] walks the same three.
fn erase_bases(flat: &Path, store_root: &Path) -> [PathBuf; 3] {
    [
        flat.to_path_buf(),
        flat.join("backup"),
        store_root.to_path_buf(),
    ]
}

/// **May this device run its all-accounts erase at all?** — the refusal a
/// second window makes necessary (`account-scoping.md` § Concurrent instances →
/// *An erase refuses while a sibling serves the account*).
///
/// Asked by both user-facing gestures that run [`crate::app::App::reset`]: the
/// Settings sign-out confirm, which paints this line, and the unreadable-index
/// floor's start-over ([`start_over_blocked`]). The automation surface's
/// factory reset asks nothing — no user is in front of it.
///
/// `Some(line)` is the user-facing refusal: another instance — of this app or
/// of a sibling app sharing the account store — is still serving an account
/// the erase would reach, so the caller must erase nothing, wipe no
/// credentials, and stay signed in. Erasing under a live sibling unlinks the
/// directory its conversations engine is running out of; wiping credentials
/// anyway would strand the stores it leaves behind.
///
/// The decision and the line are both shared
/// (`fauna_client_accounts::{sign_out_blocked, sign_out_blocked_copy}`): seven
/// seats asking this question differently is how a device ends up half-erased
/// on one app and refused on another.
pub(crate) fn sign_out_blocked(app: &App) -> Option<String> {
    all_accounts_erase_blocked(app, "sign-out")?;
    Some(fauna_client_accounts::sign_out_blocked_copy().resolve(fauna_i18n::strings::lookup))
}

/// [`sign_out_blocked`] for the unreadable-index floor ("start over on this
/// device"), which runs the same reset over an index it cannot read — so the
/// registry names nobody, and the question has to come from the disk. Its own
/// line: the user on that screen pressed *start over*, not *sign out*.
pub(crate) fn start_over_blocked(app: &App) -> Option<String> {
    all_accounts_erase_blocked(app, "start-over")?;
    Some(fauna_client_accounts::start_over_blocked_copy().resolve(fauna_i18n::strings::lookup))
}

fn all_accounts_erase_blocked(
    app: &App,
    gesture: &str,
) -> Option<fauna_client_accounts::EraseBlocked> {
    let registry = crate::session::registry(app);
    let flat = crate::session::config_dir();
    let store_root = fauna_sync_engine::root::StoreRoot::platform();
    let blocked = with_serving_bases(|bases| {
        all_accounts_erase_blocked_under(&registry, bases, flat.as_deref(), store_root.base())
    })?;
    tracing::warn!(
        "[reset] {gesture} REFUSED — another Fauna instance still serves {} account(s): {}",
        blocked.accounts.len(),
        blocked.accounts.join(", ")
    );
    Some(blocked)
}

/// The pure half of the all-accounts question, every base passed in. A `None`
/// flat base is [`erase_all_known_accounts`]'s own no-op case — it erases no
/// directory at all then — so only the registry is asked about.
fn all_accounts_erase_blocked_under(
    registry: &fauna_client_accounts::AccountRegistry,
    bases: fauna_client_accounts::ServingBases<'_>,
    flat: Option<&Path>,
    store_root: &Path,
) -> Option<fauna_client_accounts::EraseBlocked> {
    let swept = flat.map(|flat| erase_bases(flat, store_root));
    let swept: Vec<&Path> = swept.iter().flatten().map(PathBuf::as_path).collect();
    fauna_client_accounts::sign_out_blocked(registry, bases, &swept)
}

/// Remove one account from this install — the local half of the
/// `account-remove-button` gesture, in the only order that is safe to refuse
/// part-way through: **ask, then drop the registry entry, then erase.**
///
/// The question comes first because both later steps destroy something a
/// refusal cannot give back: the registry removal drops the account's secret
/// slots (delete-only, per `AccountRegistry::remove`),
/// and the erase unlinks the scope a
/// live sibling is running out of (`account-scoping.md` § Concurrent instances
/// → *An erase refuses while a sibling serves the account*). The scope is
/// erased only after the registry removal succeeds (§ Serialized switching,
/// "Erasure follows scope").
///
/// `Err` is the line for the Settings `error-message`: the shared refusal
/// ([`fauna_client_accounts::remove_account_blocked_copy`]) or the registry's
/// own failure; both are logged.
pub(crate) fn remove_account(
    registry: &fauna_client_accounts::AccountRegistry,
    actor_id_hex: &str,
) -> Result<(), String> {
    let flat = crate::session::config_dir();
    let store_root = fauna_sync_engine::root::StoreRoot::platform();
    let serving_here = fauna_client_accounts::process_session_account();
    with_serving_bases(|bases| {
        remove_account_under(
            registry,
            serving_here.as_deref(),
            bases,
            flat.as_deref(),
            store_root.base(),
            actor_id_hex,
        )
    })
}

/// The pure half of [`remove_account`], every base passed in — same
/// env-mutation-free reasoning as [`erase_under`] — and `serving_here`, the
/// account this process serves, passed in so a test need not seed the
/// process-global holder.
fn remove_account_under(
    registry: &fauna_client_accounts::AccountRegistry,
    serving_here: Option<&str>,
    bases: fauna_client_accounts::ServingBases<'_>,
    flat: Option<&Path>,
    store_root: &Path,
    actor_id_hex: &str,
) -> Result<(), String> {
    if let Some(blocked) =
        fauna_client_accounts::remove_account_blocked_as(serving_here, bases, actor_id_hex)
    {
        tracing::warn!("[settings/account] remove-account REFUSED ({blocked:?}) — {actor_id_hex}");
        return Err(blocked.copy().resolve(fauna_i18n::strings::lookup));
    }
    registry.remove(actor_id_hex).map_err(|e| {
        tracing::error!("[session] remove account failed: {e:#}");
        fauna_i18n::strings::onboarding::session_error::remove_account(&e.to_string())
    })?;
    if let Some(flat) = flat {
        erase_under(flat, store_root, actor_id_hex);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTOR_A: &str = "aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11";

    /// The line [`record_residue`] paints, without its two side effects (the
    /// log and the install-scoped record, which reads the process config dir).
    fn report_residue(
        survivors: &[PathBuf],
        credentials: &fauna_client_accounts::CredentialSweep,
    ) -> Option<String> {
        surface_for(fauna_client_accounts::SignOutResidue::record(
            survivors,
            credentials,
        ))
        .map(|surface| surface.line)
    }

    /// tui paints the retry control, so its line names it — never the
    /// "sign in and out again" remedy of a seat that has none.
    #[test]
    fn the_line_names_the_remove_again_control_tui_paints() {
        let line = report_residue(
            &[PathBuf::from("/nowhere/aa11")],
            &fauna_client_accounts::CredentialSweep::default(),
        )
        .expect("a survivor owes the user a line");
        assert!(
            line.contains(fauna_i18n::strings::settings::SIGN_OUT_RESIDUE_RETRY),
            "the Rendered copy names the control beside it: {line:?}"
        );
    }

    /// A sign-out whose erase left something behind must not look like one that
    /// did not — the whole point of finding. The
    /// count reaches the user's line; the clean sweep says nothing.
    #[test]
    fn a_survivor_produces_a_user_line_and_a_clean_sweep_produces_none() {
        assert_eq!(
            report_residue(&[], &fauna_client_accounts::CredentialSweep::default()),
            None,
            "a clean sign-out says nothing: \"0 items were left behind\" is \
             reassurance by vacuity"
        );
        let line = report_residue(
            &[PathBuf::from("/nowhere/aa11/mls_state.db")],
            &fauna_client_accounts::CredentialSweep::default(),
        )
        .expect("a survivor owes the user a line");
        assert!(
            line.contains('1'),
            "the line names how many items survived, got {line:?}"
        );
        assert!(
            !line.contains("/nowhere/"),
            "the survivor PATH belongs in the log, never on the user's line: {line:?}"
        );
    }

    /// The fault injection is a read-only **scope directory**, not a held
    /// handle and — the correction — not a read-only *parent*.
    ///
    /// ⚠ **The parent shape, which this test used until 2026-09-10, is weaker
    /// than it looks.** `remove_dir_all` unlinks the files INSIDE the scope
    /// first (needing write on the scope, which a read-only parent still
    /// grants) and only then fails to `rmdir` the scope itself. So the user's
    /// `mls_state.db` was *gone*, an empty directory survived, and the
    /// assertions below passed on a survivor that held nothing anyone would
    /// care about. Denying the write bit on the scope's OWN directory is what
    /// makes real, readable user data survive — the condition the residue line
    /// exists to report.
    ///
    /// A held handle is the windows-only shape: POSIX `unlink` removes an open
    /// file. Mirrors the shared sweep's own pin
    /// (`fauna_account_store::db::a_scope_that_will_not_go_is_reported_and_does_not_abandon_the_others`).
    #[cfg(unix)]
    #[test]
    fn a_scope_that_will_not_go_comes_back_as_a_survivor_rather_than_vanishing() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tmpdir");
        let flat = tmp.path().join("fauna-tui");
        let store_root = tmp.path().join("fauna").join("sync");
        let scope = flat.join(ACTOR_A);
        std::fs::create_dir_all(&scope).expect("scope");
        let pinned = scope.join("mls_state.db");
        std::fs::write(&pinned, b"ratchets").expect("db");
        std::fs::set_permissions(&scope, std::fs::Permissions::from_mode(0o555)).expect("chmod");

        // Probe with a real removal: a process that writes through a read-only
        // directory (root) makes every assertion below pass for the wrong
        // reason, and a test that silently proves nothing is worse than none.
        if std::fs::remove_file(&pinned).is_ok() {
            std::fs::set_permissions(&scope, std::fs::Permissions::from_mode(0o755)).expect("back");
            eprintln!("skipping: this process can write through a read-only dir (root?)");
            return;
        }

        let survivors = erase_under(&flat, &store_root, ACTOR_A);
        std::fs::set_permissions(&scope, std::fs::Permissions::from_mode(0o755)).expect("back");

        assert!(
            pinned.exists(),
            "precondition: the read-only scope is what makes the user's DATA \
             survive — an empty surviving directory would prove nothing"
        );
        assert_eq!(
            survivors,
            vec![scope],
            "the scope that would not go must NAME itself, not vanish: {survivors:?}"
        );
        assert!(
            report_residue(
                &survivors,
                &fauna_client_accounts::CredentialSweep::default()
            )
            .is_some(),
            "and reach the user"
        );
    }

    /// ⚠ **The credential half on its own** — a sign-out whose every scope went
    /// but whose credential erase did not (a keyring that refused the wipe) owes
    /// the user a line too, and must never read as clean. Until 2026-09-13 this
    /// line was built from the filesystem sweep alone, so this outcome painted
    /// nothing. The key names go to the log, never onto the line.
    #[test]
    fn surviving_credentials_owe_a_line_even_when_every_scope_went() {
        let credentials = fauna_client_accounts::CredentialSweep {
            survivors: vec![format!("fauna/{ACTOR_A}/secret")],
            wipe_failed: true,
        };
        let line =
            report_residue(&[], &credentials).expect("surviving credentials owe the user a line");
        assert!(
            !line.contains(ACTOR_A) && !line.contains("fauna/"),
            "a credential KEY belongs in the log, never on the user's line: {line:?}"
        );
        let mut refused_wipe = fauna_client_accounts::CredentialSweep::default();
        refused_wipe.record_wipe_failure();
        assert!(
            report_residue(&[], &refused_wipe).is_some(),
            "a wipe that failed with nothing readable (the locked-keyring shape) \
             is still a residue"
        );
    }

    const ACTOR_B: &str = "bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22";

    /// The sign-out reaches what a per-actor loop CANNOT: an orphan scope whose
    /// actor the registry no longer names (exactly what a failed erase leaves
    /// behind). Install-scoped state is the control — logs, the sealed
    /// credential file and the install device secret survive, and so does a
    /// store sitting flat at the base, which is no account's.
    #[test]
    fn sign_out_sweeps_an_orphan_scope_but_not_install_state() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let flat = tmp.path().join("fauna-tui");
        let store_root = tmp.path().join("fauna").join("sync");

        // The registered account, with state under both bases.
        std::fs::create_dir_all(flat.join(ACTOR_A)).expect("scope a");
        std::fs::write(flat.join(ACTOR_A).join("mls_state.db"), b"a").expect("a db");
        std::fs::create_dir_all(store_root.join(ACTOR_A)).expect("store a");
        std::fs::write(store_root.join(ACTOR_A).join("store.db"), b"a").expect("a store");
        // An ORPHAN under each base.
        for base in erase_bases(&flat, &store_root) {
            std::fs::create_dir_all(base.join(ACTOR_B)).expect("orphan");
            std::fs::write(base.join(ACTOR_B).join("x"), b"b").expect("orphan file");
        }

        // Install-scoped (class 2) — must survive.
        std::fs::create_dir_all(flat.join("logs")).expect("logs");
        std::fs::write(flat.join("logs").join("fauna.log"), b"lines").expect("log");
        let sealed = flat.join("credentials.sealed");
        std::fs::write(&sealed, b"sealed").expect("sealed");
        let install_sync = install_sync_dir_under(&flat);
        std::fs::create_dir_all(&install_sync).expect("install sync dir");
        let install_device_secret =
            install_sync.join(fauna_sync_engine::engine_lifecycle::INSTALL_DEVICE_SECRET_FILE);
        std::fs::write(&install_device_secret, [7u8; 32]).expect("install secret");
        let flat_store = flat.join("mls_state.db");
        std::fs::write(&flat_store, b"flat").expect("flat store");

        let sweep = erase_all_known_accounts_under(&flat, &store_root, &[ACTOR_A.to_string()]);

        assert!(
            sweep.is_clean(),
            "nothing should have survived: {:?}",
            sweep.survivors
        );
        assert!(!flat.join(ACTOR_A).exists(), "the registered scope goes");
        assert!(!store_root.join(ACTOR_A).exists(), "and its account store");
        for base in erase_bases(&flat, &store_root) {
            assert!(
                !base.join(ACTOR_B).exists(),
                "an orphan scope the registry no longer names is swept too — no \
                 later sign-out would ever iterate it"
            );
        }
        assert!(flat.join("logs").join("fauna.log").exists());
        assert!(sealed.exists(), "so must the sealed credential file");
        assert!(
            install_device_secret.exists(),
            "the install device secret is install-scoped and must survive a sign-out"
        );
        assert!(flat_store.exists(), "a flat file is no account's scope");
    }

    /// A store sitting flat at the base is no account's: the first account to
    /// sign in gets a fresh scope, and the flat file is neither copied in nor
    /// touched (the pre-scoping layout it would once have been is refused, not
    /// adopted — `version-compatibility.md` § Dimension 2).
    #[test]
    fn a_flat_store_is_never_adopted_into_an_account_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let flat = tmp.path().join("fauna-tui");
        std::fs::create_dir_all(install_sync_dir_under(&flat)).unwrap();
        rusqlite::Connection::open(flat.join("mls_state.db")).unwrap();
        rusqlite::Connection::open(install_sync_dir_under(&flat).join("device.db")).unwrap();

        let dir = account_state_dir_under(&flat, ACTOR_A);
        assert!(dir.is_dir());
        assert!(!dir.join("mls_state.db").exists());
        assert!(!dir.join("device.db").exists());
        assert!(flat.join("mls_state.db").exists());
    }

    /// The ruling's point, through tui's own sweep: the account-scoped
    /// `device.db` IS erased by the sign-out (it is only a cache), and the
    /// sign-in that follows re-derives the SAME id from the surviving install
    /// secret — so the machine comes back to its own `sync_devices` row rather
    /// than registering a new one per cycle.
    #[test]
    fn a_sign_out_then_sign_in_comes_back_to_the_same_device_id() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let flat = tmp.path().join("fauna-tui");
        let store_root = tmp.path().join("fauna").join("sync");

        let before = crate::media::device_id_hex_under(&flat, ACTOR_A).expect("resolves");
        let sweep = erase_all_known_accounts_under(&flat, &store_root, &[ACTOR_A.to_string()]);
        assert!(sweep.is_clean(), "{:?}", sweep.survivors);
        assert!(
            !flat.join(ACTOR_A).exists(),
            "precondition: the cached id was erased with its scope, so the next \
             read re-derives rather than finding a survivor"
        );

        assert_eq!(
            crate::media::device_id_hex_under(&flat, ACTOR_A),
            Some(before),
            "the returning sign-in must come back to the same device id"
        );
    }

    #[test]
    fn resolves_under_the_flat_base_scoped_to_the_actor() {
        let tmp = tempfile::tempdir().unwrap();
        let flat = tmp.path().join("fauna-tui");
        let dir = account_state_dir_under(&flat, ACTOR_A);
        assert_eq!(dir, flat.join(ACTOR_A));
        assert!(dir.is_dir());
    }

    #[test]
    fn erase_removes_one_actors_scoped_dir_leaving_the_other() {
        let tmp = tempfile::tempdir().unwrap();
        let flat = tmp.path().join("fauna-tui");
        let a = fauna_sync_engine::db::actor_state_dir(&flat, ACTOR_A).unwrap();
        let b = fauna_sync_engine::db::actor_state_dir(&flat, ACTOR_B).unwrap();
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("marker"), b"a").unwrap();
        std::fs::write(b.join("marker"), b"b").unwrap();

        erase_under(&flat, &tmp.path().join("fauna").join("sync"), ACTOR_A);

        assert!(!a.exists(), "actor A's scoped dir must be gone");
        assert!(
            b.join("marker").exists(),
            "actor B's scoped dir must survive A's erasure"
        );
    }

    /// The bug this test was written to catch: `erase_under` used to wipe
    /// only the main scoped dir, leaving [`crate::backup_audit`]'s
    /// `backup/<actor-hex>/audit-state.json` behind after "remove account" —
    /// stale evidence readable by whichever account the freed slot goes to
    /// next. Mirrors linux's `erase_removes_one_actors_scoped_dirs_across_all_three_bases_leaving_others`,
    /// minus the `sync/` base tui doesn't (yet) scope per-actor.
    #[test]
    fn erase_removes_one_actors_scoped_dirs_across_both_bases_leaving_the_other() {
        let tmp = tempfile::tempdir().unwrap();
        let flat = tmp.path().join("fauna-tui");
        let bases = [flat.clone(), flat.join("backup")];

        for base in &bases {
            let a = fauna_sync_engine::db::actor_state_dir(base, ACTOR_A).unwrap();
            let b = fauna_sync_engine::db::actor_state_dir(base, ACTOR_B).unwrap();
            std::fs::create_dir_all(&a).unwrap();
            std::fs::create_dir_all(&b).unwrap();
            std::fs::write(a.join("marker"), b"a").unwrap();
            std::fs::write(b.join("marker"), b"b").unwrap();
        }

        erase_under(&flat, &tmp.path().join("fauna").join("sync"), ACTOR_A);

        for base in &bases {
            assert!(
                !fauna_sync_engine::db::actor_state_dir(base, ACTOR_A)
                    .unwrap()
                    .exists(),
                "actor A's dir under {} must be gone",
                base.display()
            );
            assert!(
                fauna_sync_engine::db::actor_state_dir(base, ACTOR_B)
                    .unwrap()
                    .join("marker")
                    .exists(),
                "actor B's dir under {} must survive A's erasure",
                base.display()
            );
        }
    }

    /// Erasing an actor with no scoped dir yet (never adopted, or already
    /// erased) is a silent no-op, not an error.
    #[test]
    fn erase_of_a_never_scoped_actor_is_a_silent_no_op() {
        let tmp = tempfile::tempdir().unwrap();
        let flat = tmp.path().join("fauna-tui");
        std::fs::create_dir_all(&flat).unwrap();
        erase_under(&flat, &tmp.path().join("fauna").join("sync"), ACTOR_A);
    }

    /// **The W3 account store is a THIRD base, and it is not under the flat
    /// one** — measured 2026-08-18.
    ///
    /// W6 path unification moved the account store to the per-user root
    /// `<config>/fauna/sync/<actor>/store` — shared with linux and the sync
    /// agent — while tui's flat base is `<config>/fauna-tui`. A different
    /// tree, so iterating the flat base cannot reach it, and this function's
    /// own doc comment claimed it did.
    ///
    /// What the miss costs is not stale bytes but a **store the user cannot
    /// open again**: sign-out wipes the credential namespace, and the T10 slot
    /// it wipes is where the store's Ed25519 writer key lives. Leave the store
    /// behind and the next sign-in reads an empty slot, mints a fresh key, and
    /// the store refuses it — "belongs to a different writer", permanently and
    /// across restarts, with every store-backed surface failing and nothing
    /// on screen saying why. That is the failure shape
    /// `fauna_sync_engine::account_runtime`'s own migration-section comment
    /// names as the client-state-recoverability law's (`nest/common.md`
    /// § Client-state recoverability).
    #[test]
    fn erase_removes_the_w6_account_store_which_lives_outside_the_flat_base() {
        let tmp = tempfile::tempdir().unwrap();
        let flat = tmp.path().join("fauna-tui");
        // The W6 unified root, exactly as `StoreRoot::platform()` resolves it
        // on unix: `<config>/fauna/sync` — a sibling of the flat base, never
        // under it.
        let store_root = tmp.path().join("fauna").join("sync");

        let a_store = fauna_sync_engine::db::actor_state_dir(&store_root, ACTOR_A)
            .unwrap()
            .join("store");
        let b_store = fauna_sync_engine::db::actor_state_dir(&store_root, ACTOR_B)
            .unwrap()
            .join("store");
        std::fs::create_dir_all(&a_store).unwrap();
        std::fs::create_dir_all(&b_store).unwrap();
        std::fs::write(a_store.join("account.sqlite"), b"a").unwrap();
        std::fs::write(b_store.join("account.sqlite"), b"b").unwrap();

        erase_under(&flat, &store_root, ACTOR_A);

        assert!(
            !a_store.exists(),
            "actor A's W3 account store must go with the credential slot that \
             holds its writer key — leaving it strands the store on the next \
             sign-in"
        );
        assert!(
            b_store.join("account.sqlite").exists(),
            "actor B's account store must survive A's erasure"
        );
    }

    const CHOOSER_ACTOR_C: &str =
        "cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33";

    #[test]
    fn choosable_accounts_exclude_the_served_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let _served = match fauna_client_accounts::AccountInstanceLock::acquire(tmp.path(), ACTOR_A)
        {
            fauna_client_accounts::InstanceLockOutcome::Held(l) => l,
            _ => panic!("the collided account must be held"),
        };
        let entries = vec![
            (ACTOR_A.to_string(), Some("ana".to_string())),
            (ACTOR_B.to_string(), Some("bo".to_string())),
            (CHOOSER_ACTOR_C.to_string(), None),
        ];
        let offered = choosable_accounts_under(tmp.path(), &entries);
        assert_eq!(
            offered
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            vec![ACTOR_B, CHOOSER_ACTOR_C],
            "the served account must not be offered"
        );
    }

    #[test]
    fn choosable_accounts_can_be_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let _a = fauna_client_accounts::AccountInstanceLock::acquire(tmp.path(), ACTOR_A);
        let _b = fauna_client_accounts::AccountInstanceLock::acquire(tmp.path(), ACTOR_B);
        let entries = vec![(ACTOR_A.to_string(), None), (ACTOR_B.to_string(), None)];
        assert!(choosable_accounts_under(tmp.path(), &entries).is_empty());
    }

    #[test]
    fn choosable_accounts_offers_everyone_with_no_lock_holders() {
        let tmp = tempfile::tempdir().unwrap();
        let entries = vec![(ACTOR_A.to_string(), None), (ACTOR_B.to_string(), None)];
        assert_eq!(
            choosable_accounts_under(tmp.path(), &entries)
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            vec![ACTOR_A, ACTOR_B],
        );
    }

    /// tui and linux on one OS login: two install bases, one shared store root.
    struct Seats {
        _tmp: tempfile::TempDir,
        flat: PathBuf,
        linux_base: PathBuf,
        store_root: PathBuf,
    }

    impl Seats {
        fn new() -> Self {
            let tmp = tempfile::tempdir().expect("tmpdir");
            let flat = tmp.path().join("fauna-tui");
            let linux_base = tmp.path().join("fauna");
            let store_root = tmp.path().join("store-root");
            for dir in [&flat, &linux_base, &store_root] {
                std::fs::create_dir_all(dir).expect("base");
            }
            Self {
                _tmp: tmp,
                flat,
                linux_base,
                store_root,
            }
        }
        fn tui(&self) -> fauna_client_accounts::ServingBases<'_> {
            fauna_client_accounts::ServingBases {
                state_base: Some(&self.flat),
                store_root: Some(&self.store_root),
            }
        }
        /// A live linux instance serving `actor`, for as long as it is held.
        fn linux_serving(&self, actor: &str) -> fauna_client_accounts::SessionInstanceHolder {
            let mut linux = fauna_client_accounts::SessionInstanceHolder::new();
            linux.become_session_instance(
                fauna_client_accounts::ServingBases {
                    state_base: Some(&self.linux_base),
                    store_root: Some(&self.store_root),
                },
                actor,
                None,
                fauna_client_accounts::ServingMode::Concurrent,
            );
            assert!(linux.holds_lock(), "the sibling is genuinely serving it");
            linux
        }
        /// Every scope tui's erase reaches for `actor`, created on disk.
        fn scopes_for(&self, actor: &str) -> Vec<PathBuf> {
            let scopes: Vec<PathBuf> = erase_bases(&self.flat, &self.store_root)
                .iter()
                .map(|base| base.join(actor))
                .collect();
            for scope in &scopes {
                std::fs::create_dir_all(scope).expect("scope");
                std::fs::write(scope.join("data"), b"the user's").expect("data");
            }
            scopes
        }
    }

    /// Two accounts in an app's own registry; the second is the one a
    /// switcher's remove button would target (the first is active).
    fn registry_with_two_accounts() -> (crate::app::App, String) {
        let app = crate::app::tests::test_app();
        let registry = crate::session::registry(&app);
        registry
            .add_account(&"01".repeat(32), None, None)
            .expect("first account");
        let removable = registry
            .add_account(&"02".repeat(32), None, None)
            .expect("second account");
        (app, removable)
    }

    /// ⚠ **Until 2026-09-21, remove-account erased an actor's scopes with no
    /// question** — only the sign-out gesture asked whether a sibling still
    /// served the account (`account-scoping.md` § Concurrent instances → *An
    /// erase refuses while a sibling serves the account*). The refusal stops
    /// the gesture before EITHER destructive step: the registry removal drops
    /// the account's secret slots, so a refusal after it would leave the
    /// scopes behind with nothing left to sign in to them.
    #[test]
    fn remove_account_refuses_while_another_instance_serves_it_and_touches_nothing() {
        let seats = Seats::new();
        let (app, actor) = registry_with_two_accounts();
        let registry = crate::session::registry(&app);
        let scopes = seats.scopes_for(&actor);

        let linux = seats.linux_serving(&actor);
        let refused = remove_account_under(
            &registry,
            None,
            seats.tui(),
            Some(&seats.flat),
            &seats.store_root,
            &actor,
        );
        assert_eq!(
            refused,
            Err(fauna_i18n::strings::settings::REMOVE_ACCOUNT_BLOCKED_OTHER_WINDOW.to_string()),
            "the shared remove-account refusal, on the surface's error line"
        );
        assert!(
            registry.list().iter().any(|a| a.actor_id == actor),
            "a refused remove keeps the registry entry"
        );
        assert!(
            registry.secrets(&actor).is_some(),
            "and the account's secret slots — the stores are useless without them"
        );
        for scope in &scopes {
            assert!(
                scope.join("data").exists(),
                "{scope:?} erased under a live sibling"
            );
        }

        drop(linux);
        assert_eq!(
            remove_account_under(
                &registry,
                None,
                seats.tui(),
                Some(&seats.flat),
                &seats.store_root,
                &actor,
            ),
            Ok(()),
            "with the sibling gone, the remove goes through"
        );
        assert!(!registry.list().iter().any(|a| a.actor_id == actor));
        for scope in &scopes {
            assert!(!scope.exists(), "{scope:?} survived an unrefused remove");
        }
    }

    /// A sibling on some OTHER account is none of remove-account's business.
    #[test]
    fn remove_account_is_not_stopped_by_a_sibling_on_another_account() {
        let seats = Seats::new();
        let (app, actor) = registry_with_two_accounts();
        let registry = crate::session::registry(&app);
        let _linux = seats.linux_serving(ACTOR_A);

        assert_eq!(
            remove_account_under(
                &registry,
                None,
                seats.tui(),
                Some(&seats.flat),
                &seats.store_root,
                &actor,
            ),
            Ok(())
        );
    }

    /// ⚠ **A bound instance must not remove the account it is serving**. The registry's active account is the first one,
    /// and this process serves the second — a bound secondary, which never moves
    /// the active pointer (`account-scoping.md` § Concurrent instances). No
    /// sibling holds anything: the sibling probe puts this process's own lock
    /// down by design, so it alone would call the account free and unlink the
    /// stores this very process runs from.
    #[test]
    fn remove_account_refuses_the_account_this_process_serves_and_touches_nothing() {
        let seats = Seats::new();
        let (app, served_here) = registry_with_two_accounts();
        let registry = crate::session::registry(&app);
        assert_ne!(
            registry.active().as_deref(),
            Some(served_here.as_str()),
            "the precondition: the served account is not the registry's active one"
        );
        let scopes = seats.scopes_for(&served_here);

        assert_eq!(
            remove_account_under(
                &registry,
                Some(&served_here),
                seats.tui(),
                Some(&seats.flat),
                &seats.store_root,
                &served_here,
            ),
            Err(fauna_i18n::strings::settings::REMOVE_ACCOUNT_BLOCKED_THIS_WINDOW.to_string()),
            "the served-here refusal, on the surface's error line"
        );
        assert!(
            registry.list().iter().any(|a| a.actor_id == served_here),
            "a refused remove keeps the registry entry"
        );
        assert!(registry.secrets(&served_here).is_some());
        for scope in &scopes {
            assert!(
                scope.join("data").exists(),
                "{scope:?} erased from under the process serving it"
            );
        }
    }

    /// ⚠ **The all-accounts question — sign-out and the unreadable-index floor
    /// — asks about every scope tui's erase reaches, not only its registry.**
    /// The account here is in NO registry tui can read (linux signed in to it;
    /// or tui's index is malformed and names nobody), yet tui's erase sweeps
    /// the shared store root and would unlink linux's store.
    #[test]
    fn the_all_accounts_erase_asks_about_scopes_its_registry_does_not_name() {
        let seats = Seats::new();
        let app = crate::app::tests::test_app();
        let registry = crate::session::registry(&app);
        assert!(
            registry.list().is_empty(),
            "the precondition: tui knows no account"
        );

        // Only on the shared root — the cross-app case.
        std::fs::create_dir_all(seats.store_root.join(ACTOR_B)).expect("store scope");
        let linux = seats.linux_serving(ACTOR_B);
        assert_eq!(
            all_accounts_erase_blocked_under(
                &registry,
                seats.tui(),
                Some(&seats.flat),
                &seats.store_root
            )
            .map(|b| b.accounts),
            Some(vec![ACTOR_B.to_string()]),
        );
        drop(linux);
        assert_eq!(
            all_accounts_erase_blocked_under(
                &registry,
                seats.tui(),
                Some(&seats.flat),
                &seats.store_root
            ),
            None,
            "with nobody serving it, the erase may run"
        );
    }

    /// ⚠ **The guard's predicate was pinned only in the library — the gesture
    /// that calls it had no witness of its own**: before this test,
    /// deleting `settings::sign_out_confirm_unless`'s one-line guard reddened
    /// nothing, anywhere (`account-scoping.md` § Concurrent instances → *An
    /// erase refuses while a sibling serves the account*). Drives the real
    /// gesture — not a fake refusal — over temp bases: a live sibling refuses
    /// it first, and once it is gone the sign-out succeeds despite an on-disk
    /// scope no registry entry names (`account-scoping.md:981`, the erase's
    /// actual reach).
    ///
    /// A tokio test rather than a bare one: the one-window arm's `app.reset()`
    /// tears down the (fixture) session, which spawns (`session::sign_out`).
    #[tokio::test]
    async fn sign_out_confirm_refuses_under_a_sibling_and_then_signs_out_around_an_orphan_scope() {
        let seats = Seats::new();
        let mut app = crate::app::tests::authed_app();
        let actor = crate::app::tests::test_actor_id();
        let registry = crate::session::registry(&app);
        let scopes = seats.scopes_for(&actor);

        // A scope on disk that no registry entry names — the erase's actual
        // reach, not just what this app's own index happens to list.
        std::fs::create_dir_all(seats.store_root.join(ACTOR_B)).expect("orphan scope");

        let blocked = |_app: &App| {
            all_accounts_erase_blocked_under(
                &registry,
                seats.tui(),
                Some(&seats.flat),
                &seats.store_root,
            )
            .map(|_| {
                fauna_client_accounts::sign_out_blocked_copy().resolve(fauna_i18n::strings::lookup)
            })
        };

        let linux = seats.linux_serving(&actor);
        crate::settings::sign_out_confirm_unless(&mut app, blocked);
        assert!(
            app.session.is_some(),
            "refused: nothing erased, no credentials wiped, still signed in"
        );
        assert_eq!(
            app.errors.get(&crate::pages::Page::Settings).cloned(),
            Some(
                fauna_client_accounts::sign_out_blocked_copy().resolve(fauna_i18n::strings::lookup)
            ),
            "the refusal reaches the page's error line"
        );
        assert!(
            registry.list().iter().any(|a| a.actor_id == actor),
            "a refused sign-out keeps the registry entry"
        );
        for scope in &scopes {
            assert!(
                scope.join("data").exists(),
                "{scope:?} erased under a live sibling"
            );
        }

        drop(linux);
        crate::settings::sign_out_confirm_unless(&mut app, blocked);
        assert!(
            app.session.is_none(),
            "with the sibling gone, sign-out goes through — the orphan scope must not block it"
        );
    }
}
