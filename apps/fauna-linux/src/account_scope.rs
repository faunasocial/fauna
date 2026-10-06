//! Per-account (actor-id-scoped) state directory for the account's own stores
//! under `~/.config/fauna/` — MLS state and the P2P DBs live in
//! `~/.config/fauna/<actor-hex>/` (`account-scoping.md` § The scoping
//! taxonomy, class 1), resolved through the one shared derivation
//! [`fauna_sync_engine::db::actor_state_dir_or_unresolved`]. Nothing is ever
//! read from the flat base itself: a store sitting there is no account's.
//!
//! Distinct from `fauna-sync-agent`'s own `sync/<actor-hex>/` scoping: a
//! different process and a different base (`~/.config/fauna/sync/`, the
//! shared account-store root).

use std::path::{Path, PathBuf};

/// The install-scoped state base — `<xdg-config>/fauna/`, one per OS login and
/// never per account. Everything keyed to the install resolves through here so
/// the relationships are structural rather than four coincidentally-identical
/// `join("fauna")` calls: the per-actor scope dirs hang off it, the sign-out
/// sweeps clear it, the `AccountInstanceLock` files sit in it, and
/// `main.rs::account_registry()` puts the registry's cross-process mutation
/// lock there too (`account-scoping.md` § Concurrent instances requires the
/// two locks to share this base; `long-term-store.md` § Cross-process mutation
/// lock requires the mutation lock to be install-scoped, since it guards the
/// account index that is shared *between* accounts).
///
/// `None` when no config dir resolves at all; every caller degrades rather
/// than failing (the locks proceed unguarded, the sweeps no-op).
pub(crate) fn install_state_base() -> Option<PathBuf> {
    crate::window_state::dirs_config().map(|c| c.join("fauna"))
}

/// Resolve — and create — actor `actor_id_hex`'s state directory under the
/// install base `<xdg-config>/fauna/`. Always returns a usable directory, even
/// on the (should-never-happen) malformed-actor-id path — degraded to the
/// shared `-unresolved-` scope rather than a hard failure, since every caller
/// here is a best-effort post-auth init.
pub(crate) fn account_state_dir(actor_id_hex: &str) -> PathBuf {
    let base = install_state_base().unwrap_or_else(|| PathBuf::from("/tmp/fauna"));
    account_state_dir_under(&base, actor_id_hex)
}

/// The pure half of [`account_state_dir`], the base passed in rather than
/// read from `XDG_CONFIG_HOME`/`HOME` — so tests never mutate process env
/// (same reasoning as `fauna-sync-agent`'s `SyncPaths::unix_config_base`).
fn account_state_dir_under(base: &std::path::Path, actor_id_hex: &str) -> PathBuf {
    let dir = fauna_sync_engine::db::actor_state_dir_or_unresolved(base, actor_id_hex);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(
            "account state dir {} could not be created: {e}",
            dir.display()
        );
    }
    dir
}

/// The active account's actor id (hex), if any is signed in yet. Shared
/// resolution for the free-function call sites that don't already have the
/// actor in hand (`app.rs`'s `AuthSuccess` handler does, via `state.borrow()`
/// — this is for `settings::privacy`/`sync`, whose entry points are called
/// with no session state threaded through).
pub(crate) fn active_actor_id_hex() -> Option<String> {
    crate::account_registry().active()
}

/// Erase actor `actor_id_hex`'s account-scoped local state: the flat-`fauna/`
/// account dir (MLS db, P2P DBs, spam model — all co-located
/// there) plus its scoped subdirs under `sync/` and `backup/`. Best-effort —
/// a missing dir is not an error, and a `dirs_config()` failure is a silent
/// no-op (nothing to erase without a resolvable base). This is the *erasure*
/// half of `account-scoping.md`'s isolation contract ("Erasure follows
/// scope"): a plain switch or app quit must PRESERVE state (never call this),
/// only sign-out ([`erase_all_known_accounts`]) and "remove account"
/// ([`remove_account`], which asks first) do.
///
/// **Returns what survived** — the paths the sweep tried to remove and could
/// not. An unresolvable base is not one of them: nothing was attempted, so
/// nothing was left behind by this call.
pub(crate) fn erase(actor_id_hex: &str) -> Vec<PathBuf> {
    let Some(flat) = install_state_base() else {
        return Vec::new();
    };
    erase_under(&flat, actor_id_hex)
}

/// The pure half of [`erase`], the flat `fauna/` base passed in — same
/// env-mutation-free testing reasoning as [`account_state_dir_under`]. The
/// iterate-and-remove loop itself is [`fauna_sync_engine::db::erase_actor_state`]
/// (the tui twin's identical copy) — this is only linux's own base list,
/// [`erase_bases`].
fn erase_under(fauna_base: &Path, actor_id_hex: &str) -> Vec<PathBuf> {
    let bases = erase_bases(fauna_base);
    fauna_sync_engine::db::erase_actor_state(bases.iter().map(PathBuf::as_path), actor_id_hex)
}

/// The three bases every linux erase sweeps — `fauna_base` itself and its
/// `sync/` and `backup/` subdirs. Named once, because the refusal must ask
/// about exactly what the erase will reach
/// ([`all_accounts_erase_blocked_under`]): a base the erase sweeps and the
/// question skips is an account erased under a live sibling.
/// [`erase_all_scopes_under`] walks the same three.
fn erase_bases(fauna_base: &Path) -> [PathBuf; 3] {
    [
        fauna_base.to_path_buf(),
        fauna_base.join("sync"),
        fauna_base.join("backup"),
    ]
}

/// Become (or remain) one of this account's serving instances — called from
/// `app.rs`'s `AuthSuccess` arm BEFORE any of the account's scoped state
/// opens (`account-scoping.md` § Concurrent instances).
///
/// **linux is RETIRED (W5.6 (account-data-plane.md § Workstreams) trickle-down, mirroring tui's 2026-08-15 leg): it
/// serves [`ServingMode::Concurrent`]** — same-account instances coexist over
/// the multi-process-safe account store, each holding a *shared* per-account
/// lock, with the three genuinely exclusive critical sections carrying their
/// own locks; the conversations surface of a non-role-holder refuses
/// honestly via `MlsError::ServedElsewhere` rather than this process exiting.
///
/// The holder logic is shared
/// (`fauna_client_accounts::become_process_session_instance`): reuse on a
/// same-account rebuild, swap-by-replacement on an in-process cross-account
/// switch, bound-or-refuse, degrade open. This wrapper adds only linux's three
/// platform duties — resolving the install-scoped base, logging the degrade
/// (the goal doc assigns that log to the platform), and claiming the
/// per-account **activation endpoint** (§ *The per-(OS login, account) raise
/// channel*).
///
/// **Refusal is still terminal** — rarer now, not gone: `AlreadyServed`
/// survives exactly for an *exclusive* holder (a process serving this
/// account in exclusive mode),
/// and `BoundMismatch` is mode-independent ("launch bound or refuse"). On
/// `Err` the caller exits — the same terminal contract as apple's
/// `refuseLaunch`.
///
/// [`ServingMode::Concurrent`]: fauna_client_accounts::ServingMode::Concurrent
///
/// The endpoint claim lives here, beside the acquire, so **endpoint ownership
/// tracks lock ownership at one site**: every path that goes on to serve the
/// account claims it (the degrade included — a process serving unguarded is
/// still the one to raise), every path that refuses claims nothing, and an
/// in-process switch swaps the name exactly where it swaps the lock. Splitting
/// the two would let them disagree, and a stale endpoint is worse than none:
/// it answers raises for an account this process no longer serves.
pub(crate) fn become_session_instance(
    actor_id_hex: &str,
) -> Result<(), fauna_client_accounts::InstanceRefusal> {
    match with_serving_bases(|bases| {
        fauna_client_accounts::become_process_session_instance(
            bases,
            actor_id_hex,
            fauna_client_accounts::ServingMode::Concurrent,
        )
    }) {
        fauna_client_accounts::SessionInstanceOutcome::Acquired
        | fauna_client_accounts::SessionInstanceOutcome::Reused => {
            crate::instance_remote::claim_account_endpoint(actor_id_hex);
            forget_sign_out_residue_view();
            Ok(())
        }
        fauna_client_accounts::SessionInstanceOutcome::Degraded(cause) => {
            tracing::warn!(
                "[instance-lock] degraded acquire for {actor_id_hex} ({cause:?}) — proceeding unguarded"
            );
            crate::instance_remote::claim_account_endpoint(actor_id_hex);
            forget_sign_out_residue_view();
            Ok(())
        }
        fauna_client_accounts::SessionInstanceOutcome::Refused(r) => Err(r),
    }
}

/// Run `f` with the two directories this instance is keyed under: linux's own
/// install base (the launch law's lock) and the per-user account-store root every
/// app on this OS login shares (the cross-app presence lock) —
/// `account-scoping.md` § Concurrent instances → *An erase refuses while a
/// sibling serves the account*. One helper so the instance that *declares* itself
/// and the erase that *asks* can never disagree about where.
///
/// ⚠ The store root happens to sit *inside* linux's install base
/// (`<config>/fauna/sync` under `<config>/fauna`), which is an accident of
/// naming and not a relationship: it is resolved through `StoreRoot::platform()`
/// like every other seat's, never joined onto the base.
fn with_serving_bases<T>(f: impl FnOnce(fauna_client_accounts::ServingBases<'_>) -> T) -> T {
    let base = install_state_base();
    let store_root = fauna_sync_engine::root::StoreRoot::platform();
    f(fauna_client_accounts::ServingBases {
        state_base: base.as_deref(),
        store_root: Some(store_root.base()),
    })
}

/// **May this device run its all-accounts erase at all?** — the refusal a
/// second window makes necessary (`account-scoping.md` § Concurrent instances →
/// *An erase refuses while a sibling serves the account*).
///
/// Asked by both user-facing gestures that run
/// [`crate::settings::trigger_sign_out`]: the Settings sign-out confirm, which
/// paints this line, and the unreadable-index floor's start-over
/// ([`start_over_blocked`]). The e2e agent's `reset|logout` arm asks nothing —
/// no user is in front of it.
///
/// `Some(line)` is the user-facing refusal: another instance — of this app or of
/// a sibling app sharing the account store — still serves an account the erase
/// would reach, so the caller must erase nothing, wipe no credentials, and stay
/// signed in. Erasing under a live sibling unlinks the directory its
/// conversations engine is running out of; wiping credentials anyway would
/// strand the stores it leaves behind.
///
/// The decision and the line are both shared
/// (`fauna_client_accounts::{sign_out_blocked, sign_out_blocked_copy}`) — this
/// wrapper adds only linux's platform facts: the install-scoped base, the
/// bases its erase sweeps, and the registry handle.
pub(crate) fn sign_out_blocked() -> Option<String> {
    all_accounts_erase_blocked("sign-out")?;
    Some(fauna_client_accounts::sign_out_blocked_copy().resolve(crate::i18n::strings::lookup))
}

/// [`sign_out_blocked`] for the unreadable-index floor ("start over on this
/// device"), which runs the same erase over an index it cannot read — so the
/// registry names nobody, and the question has to come from the disk. Its own
/// line: the user on that screen pressed *start over*, not *sign out*.
pub(crate) fn start_over_blocked() -> Option<String> {
    all_accounts_erase_blocked("start-over")?;
    Some(fauna_client_accounts::start_over_blocked_copy().resolve(crate::i18n::strings::lookup))
}

fn all_accounts_erase_blocked(gesture: &str) -> Option<fauna_client_accounts::EraseBlocked> {
    let registry = crate::account_registry();
    let fauna_base = install_state_base();
    let blocked = with_serving_bases(|bases| {
        all_accounts_erase_blocked_under(&registry, bases, fauna_base.as_deref())
    })?;
    tracing::warn!(
        "[{gesture}] REFUSED — another Fauna instance still serves {} account(s): {}",
        blocked.accounts.len(),
        blocked.accounts.join(", ")
    );
    Some(blocked)
}

/// The pure half of the all-accounts question, every base passed in. A `None`
/// base is [`erase_all_known_accounts`]'s own no-op case — it erases no
/// directory at all then — so only the registry is asked about.
fn all_accounts_erase_blocked_under(
    registry: &fauna_client_accounts::AccountRegistry,
    bases: fauna_client_accounts::ServingBases<'_>,
    fauna_base: Option<&Path>,
) -> Option<fauna_client_accounts::EraseBlocked> {
    let swept = fauna_base.map(erase_bases);
    let swept: Vec<&Path> = swept.iter().flatten().map(PathBuf::as_path).collect();
    fauna_client_accounts::sign_out_blocked(registry, bases, &swept)
}

/// Remove one account from this install — the local half of the
/// `account-remove-button` gesture, in the only order that is safe to refuse
/// part-way through: **ask, then drop the registry entry, then erase.**
///
/// The question comes first because both later steps destroy something a
/// refusal cannot give back: the registry removal drops the account's secret
/// slots, and the erase unlinks the scope a live sibling is running out of
/// (`account-scoping.md` § Concurrent instances → *An erase refuses while a
/// sibling serves the account*). The scope is erased only after the registry
/// removal succeeds (§ Serialized switching, "Erasure follows scope"). Twin of
/// tui's `account_scope::remove_account`.
///
/// `Err` is the line for the Account page's `error-message`: the shared
/// refusal ([`fauna_client_accounts::remove_account_blocked_copy`]) or the
/// registry's own failure; both are logged.
pub(crate) fn remove_account(actor_id_hex: &str) -> Result<(), String> {
    let registry = crate::account_registry();
    let fauna_base = install_state_base();
    let serving_here = fauna_client_accounts::process_session_account();
    with_serving_bases(|bases| {
        remove_account_under(
            &registry,
            serving_here.as_deref(),
            bases,
            fauna_base.as_deref(),
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
    fauna_base: Option<&Path>,
    actor_id_hex: &str,
) -> Result<(), String> {
    if let Some(blocked) =
        fauna_client_accounts::remove_account_blocked_as(serving_here, bases, actor_id_hex)
    {
        tracing::warn!("[settings/account] remove-account REFUSED ({blocked:?}) — {actor_id_hex}");
        return Err(blocked.copy().resolve(crate::i18n::strings::lookup));
    }
    registry.remove(actor_id_hex).map_err(|e| {
        tracing::error!("[settings/account] remove account failed: {e:#}");
        crate::i18n::strings::onboarding::session_error::remove_account(&e.to_string())
    })?;
    // Erasure follows scope (account-scoping.md § Serialized switching):
    // removing an account erases its local account-scoped state, not only the
    // registry entry.
    if let Some(fauna_base) = fauna_base {
        erase_under(fauna_base, actor_id_hex);
    }
    Ok(())
}

/// Is `actor_id_hex` served by a live instance right now?
///
/// The same display-only probe the chooser's list is built from, for the one
/// caller that needs a single account rather than a filter: focus-existing's
/// degrade (`account-scoping.md` § the per-(OS login, account) raise channel —
/// "if the endpoint is unowned … the raiser re-probes the lock"). Without the
/// re-probe, "the sibling exited while the chooser was up" and "the sibling
/// lives but claims no endpoint" are indistinguishable, and only one of them
/// should strand the user on an error.
///
/// No resolvable base means no lock files to read, and the truthful answer to
/// "is a live holder holding one?" is then `false` — the same degrade-open
/// posture as the shared probe itself.
pub(crate) fn is_account_served(actor_id_hex: &str) -> bool {
    install_state_base().is_some_and(|base| {
        fauna_client_accounts::AccountInstanceLock::is_served(&base, actor_id_hex)
    })
}

/// The launch-collision chooser's list: the registry's accounts that no live
/// instance currently serves, as `(actor_id, display label)` in registry
/// order. The account that collided is excluded by the probe itself (a live
/// holder is why the chooser rendered at all).
///
/// Thin wrapper over the shared [`fauna_client_accounts::choosable_accounts`]
/// (every chooser platform — windows, linux, tui — answers "which accounts
/// may I offer?" identically).
pub(crate) fn choosable_accounts(
    registry: &fauna_client_accounts::AccountRegistry,
) -> Vec<(String, String)> {
    let Some(base) = install_state_base() else {
        // No resolvable base means no lock files to probe and no way to tell
        // free from served — offer nothing rather than offer everything. The
        // chooser's two exits still work, so the user is never stranded.
        return Vec::new();
    };
    let entries: Vec<(String, Option<String>)> = registry
        .list()
        .into_iter()
        .map(|e| (e.actor_id, e.handle))
        .collect();
    choosable_accounts_under(&base, &entries)
}

/// The pure half of [`choosable_accounts`] — base and registry rows passed in,
/// so the filter is testable without XDG or a live secret store (the same
/// env-mutation-free reasoning as [`account_state_dir_under`]).
fn choosable_accounts_under(
    base: &std::path::Path,
    entries: &[(String, Option<String>)],
) -> Vec<(String, String)> {
    fauna_client_accounts::choosable_accounts(base, entries)
}

/// Erase every currently-registered account's scoped local state — the
/// sign-out ("all-accounts erase") half of the same corollary. **Call BEFORE
/// the credential namespace itself is wiped** (`client::delete_credentials`),
/// since this reads the registry to know which actors existed; calling it
/// after would find an already-empty registry and erase nothing.
///
/// **Returns what survived, folded across every account and every base.**
/// Best-effort is not the same as silent: the shared sweeps name the paths they
/// could not remove, so this no longer has to choose between completing the
/// sign-out and telling the truth about it. A non-empty return means the
/// signed-out user's data is still readable on this device, and the caller owes
/// the user a line saying so — [`record_residue`] records and paints it.
#[must_use = "a sign-out that drops this reports success over a device that may \
              still hold the user's data - the defect this return exists to prevent"]
pub(crate) fn erase_all_known_accounts() -> Vec<PathBuf> {
    // Folded through the shared `EraseSweep`, NOT a plain `Vec::extend` — the
    // two passes below deliberately overlap (the all-scopes sweep re-walks the
    // same bases the per-actor loop just did), so an undeletable scope is found
    // by both. `absorb` owns the rule that a survivor is a *location*, and
    // `survivors.len()` is the number the user is shown: extending a Vec told
    // them *"2 item(s)"* about one surviving directory until the sign-out
    // journey e2e looked.
    //
    // `erased: 0` is honest rather than lossy: `erase` reports only what it
    // could NOT remove, and this function's contract is the survivors.
    let mut sweep = fauna_sync_engine::db::EraseSweep::default();
    for entry in crate::account_registry().list() {
        sweep.absorb(fauna_sync_engine::db::EraseSweep {
            erased: 0,
            survivors: erase(&entry.actor_id),
        });
    }
    // The loop above only reaches the accounts the registry names. A scope the
    // registry no longer names (an orphan) is still a signed-out user's data,
    // so every actor scope under the three bases goes too.
    if let Some(base) = install_state_base() {
        sweep.absorb(erase_all_scopes_under(&base));
    }
    // The two pieces of in-memory state deliberately built to survive an
    // account SWITCH — so they have to be dropped explicitly here, where the
    // switch's `reset_actor_scoped_state` must not touch them
    // (`settings::recovery_kit::{LANDED_SWEEP, SUCCESSION_KIT_OWED}`). An erase
    // destroys every identity on the box, so there is no successor left to owe
    // a kit to. (The ceremony's raise context is parked in the account
    // registry, per successor, and erased with its row.)
    crate::settings::recovery_kit::clear_succession_sweep();
    crate::settings::recovery_kit::clear_succession_kit_debt();
    sweep.survivors
}

/// The residue as the `sign-out-residue` view paints it: the install-scoped
/// record the re-sweep needs, and the one line the user is shown. Twin of
/// tui's `account_scope::ResidueSurface`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResidueSurface {
    pub(crate) record: fauna_client_accounts::SignOutResidue,
    /// `sign-out-residue-message` — the shared `Rendered` copy, or the retry's
    /// own refusal while another instance serves one of the residue's accounts.
    pub(crate) line: String,
}

thread_local! {
    /// What the last sign-out left, while it still owes work — read by
    /// `identity_choice`'s `sign-out-residue` view on every tick. GTK main
    /// thread only, like every other widget-facing cell here. Held OUTSIDE the
    /// wizard because the wizard is rebuilt by every sign-out and does not own
    /// the fact: the residue belongs to the device.
    static SIGN_OUT_RESIDUE: std::cell::RefCell<Option<ResidueSurface>> =
        const { std::cell::RefCell::new(None) };
}

/// The residue the `sign-out-residue` view paints, if any.
pub(crate) fn sign_out_residue() -> Option<ResidueSurface> {
    SIGN_OUT_RESIDUE.with(|cell| cell.borrow().clone())
}

fn set_sign_out_residue(surface: Option<ResidueSurface>) {
    SIGN_OUT_RESIDUE.with(|cell| *cell.borrow_mut() = surface);
}

/// A signed-in user is not the one a sign-out residue was reported to: the
/// view goes, and the record stays on disk for the next signed-out launch to
/// re-check ([`recheck_residue_at_launch`]).
pub(crate) fn forget_sign_out_residue_view() {
    set_sign_out_residue(None);
}

/// Turn what a sign-out's erase left behind into the reports it owes: the
/// survivor **paths** to the log (the sweeps have already written theirs; this
/// is the folded one, naming every base at once), the **record** to
/// install-scoped state so it outlives this process, and the **count** to the
/// user, as the `sign-out-residue` view `identity_choice` paints. Twin of tui's
/// `account_scope::record_residue`.
///
/// A clean sweep paints nothing and leaves no record — *"0 items were left
/// behind"* is reassurance by vacuity. Every word of the line is the shared
/// projection's; linux only localizes it and declares that it paints the retry
/// control.
///
/// ⚠ **Both halves of the erase, or it answers for half a sign-out.**
/// `credentials` is what [`crate::client::delete_credentials`] left, so the
/// caller can only record this after the credential wipe has run — which is
/// the point: built beside the filesystem erase, it painted a clean line over a
/// keyring that refused the wipe.
pub(crate) fn record_residue(
    survivors: &[PathBuf],
    credentials: &fauna_client_accounts::CredentialSweep,
) {
    log_residue(survivors, credentials);
    let record = fauna_client_accounts::SignOutResidue::record(survivors, credentials);
    persist_residue(&record);
    set_sign_out_residue(surface_for(record));
}

/// Remove Again (`sign-out-residue-retry-button`): re-sweep what the painted
/// residue recorded and paint what is left — nothing, when the device is now
/// clean.
pub(crate) fn retry_residue() {
    let Some(surface) = sign_out_residue() else {
        return;
    };
    set_sign_out_residue(run_residue_retry(surface.record));
}

/// The signed-out launch's silent re-check: a record a previous sign-out left
/// is re-swept FIRST, and the view is painted only if something is still left
/// (`account-scoping.md` § Erasure follows scope → *the residue surface*).
///
/// Only on a launch with no account in the registry — the fresh-wizard route a
/// sign-out hands back. Its scope is the record and nothing more: it never
/// re-lists the shared store root for actor scopes, which is how a signed-out
/// launch once erased a running sibling's live store.
pub(crate) fn recheck_residue_at_launch() {
    let Some(base) = install_state_base() else {
        return;
    };
    let Some(record) = fauna_client_accounts::SignOutResidue::load(&base) else {
        return;
    };
    if !crate::account_registry().list().is_empty() {
        return;
    }
    set_sign_out_residue(run_residue_retry(record));
}

/// The one re-sweep both gestures run — the shared
/// [`fauna_client_accounts::retry_sign_out_residue`], behind the sign-out's own
/// serving-lock question and over the recorded paths only.
fn run_residue_retry(record: fauna_client_accounts::SignOutResidue) -> Option<ResidueSurface> {
    let outcome = with_serving_bases(|bases| {
        fauna_client_accounts::retry_sign_out_residue(&record, bases, |recorded| {
            crate::client::re_delete_credentials(recorded)
        })
    });
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

/// Write the record into linux's install base — or remove it, when clean. A
/// failed write is logged and not fatal: the view still paints for this
/// process, and the paths are in the log the erase wrote.
fn persist_residue(record: &fauna_client_accounts::SignOutResidue) {
    let Some(base) = install_state_base() else {
        return;
    };
    if let Err(e) = record.save(&base) {
        tracing::warn!(
            "sign-out: the residue record under {} could not be written: {e}",
            base.display()
        );
    }
}

/// The residue's log half — paths and key names, which never reach the user.
fn log_residue(survivors: &[PathBuf], credentials: &fauna_client_accounts::CredentialSweep) {
    if !survivors.is_empty() {
        tracing::warn!(
            "sign-out: {} path(s) SURVIVED the erase and still hold this user's data: {}",
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
            "sign-out: sign-in credentials SURVIVED the erase (namespace wipe failed: {}); \
             still readable: [{}]",
            credentials.wipe_failed,
            credentials.survivors.join(", ")
        );
    }
}

/// Every actor scope under each of the three bases (`fauna_base` itself, its
/// `sync/` and `backup/` subdirs) — the safety net [`erase_all_known_accounts`]
/// runs beside its per-actor loop, for a scope the registry no longer names.
/// Anything that is not an actor scope survives: install-scoped state, the lock
/// files, the install device secret.
///
/// **Returns what survived**, folded across all three bases. A non-empty return
/// means the signed-out user's data is still readable on this device.
fn erase_all_scopes_under(fauna_base: &Path) -> fauna_sync_engine::db::EraseSweep {
    let mut sweep = fauna_sync_engine::db::EraseSweep::default();
    for base in erase_bases(fauna_base) {
        sweep.absorb(fauna_sync_engine::db::erase_all_account_scopes(&base));
    }
    if !sweep.is_clean() {
        tracing::warn!(
            "sign-out: {} path(s) SURVIVED the erase and still hold this user's data: {}",
            sweep.survivors.len(),
            sweep
                .survivors
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    sweep
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTOR_A: &str = "aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11";
    const ACTOR_B: &str = "bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22";
    const ACTOR_C: &str = "cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33";

    /// The chooser offers the accounts no live instance serves, in registry
    /// order, labelled with the shared display formatter — and never offers
    /// the account that collided (a live holder is why the chooser rendered).
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
            (ACTOR_C.to_string(), None),
        ];
        let offered = choosable_accounts_under(tmp.path(), &entries);
        assert_eq!(
            offered
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            vec![ACTOR_B, ACTOR_C],
            "the served account must not be offered"
        );
        assert_eq!(offered[0].1, "bo", "handle labels the row when cached");
        assert_eq!(
            offered[1].1,
            fauna_core::format::account_display_label(None, ACTOR_C),
            "a handle-less account falls back to the shared short-id label"
        );
    }

    /// Every account served somewhere → nothing to offer. The chooser renders
    /// its "all open" explanation rather than an empty list, and its two exits
    /// still work (pinned in the view's own tests).
    #[test]
    fn choosable_accounts_can_be_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let _a = fauna_client_accounts::AccountInstanceLock::acquire(tmp.path(), ACTOR_A);
        let _b = fauna_client_accounts::AccountInstanceLock::acquire(tmp.path(), ACTOR_B);
        let entries = vec![(ACTOR_A.to_string(), None), (ACTOR_B.to_string(), None)];
        assert!(choosable_accounts_under(tmp.path(), &entries).is_empty());
    }

    #[test]
    fn resolves_under_the_flat_base_scoped_to_the_actor() {
        let tmp = tempfile::tempdir().unwrap();
        let flat = tmp.path().join("fauna");
        let dir = account_state_dir_under(&flat, ACTOR_A);
        assert_eq!(dir, flat.join(ACTOR_A));
        assert!(dir.is_dir());
    }

    /// A store sitting flat at the install base is no account's: the first
    /// account to sign in gets a fresh scope, and the flat file is neither
    /// copied in nor touched (the pre-scoping layout it would once have been is
    /// refused, not adopted — `version-compatibility.md` § Dimension 2).
    #[test]
    fn a_flat_store_is_never_adopted_into_an_account_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let flat = tmp.path().join("fauna");
        std::fs::create_dir_all(&flat).unwrap();
        std::fs::write(flat.join("mls_state.db"), b"").unwrap();

        let dir = account_state_dir_under(&flat, ACTOR_A);
        assert!(dir.is_dir());
        assert!(!dir.join("mls_state.db").exists());
        assert!(flat.join("mls_state.db").exists());
        assert!(
            !flat
                .join(fauna_sync_engine::db::UNRESOLVED_ACTOR_COMPONENT)
                .exists()
        );
    }

    // The (OS login, account) instance-lock holder's own tests moved to
    // `fauna_client_accounts::instance_lock` (2026-07-22) together with the
    // logic — linux now consumes the shared holder, so its behaviours are
    // pinned once for every native leg rather than per client.

    /// The line [`record_residue`] paints, without its side effects (the log,
    /// the install-scoped record under the real config dir, the view cell).
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

    /// linux paints the retry control, so its line names it.
    #[test]
    fn the_line_names_the_remove_again_control_linux_paints() {
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
    /// did not. The twin of tui's pin of the same name — one shape, five seats.
    #[test]
    fn a_survivor_produces_a_user_line_and_a_clean_sweep_produces_none() {
        assert_eq!(
            report_residue(&[], &fauna_client_accounts::CredentialSweep::default()),
            None,
            "a clean sign-out says nothing: \"0 items were left behind\" is \
             reassurance by vacuity"
        );
        let line = report_residue(
            &[PathBuf::from("/nowhere/aa11/mls.db")],
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

    /// ⚠ **The credential half on its own** — twin of tui's pin of the same
    /// name. A sign-out whose every scope went but whose credential erase did
    /// not owes the user a line, and the key names stay in the log. Until
    /// 2026-09-13 linux built this line before the credential wipe ran and
    /// dropped the wipe's `Result`, so this outcome painted nothing.
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

    /// The fault injection is a **read-only parent**, not a held handle: POSIX
    /// `unlink` removes an open file, so the `share_mode(0)` shape that catches
    /// this on Windows proves nothing here.
    #[test]
    fn a_scope_that_will_not_go_comes_back_as_a_survivor_rather_than_vanishing() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let fauna_base = tmp.path().join("fauna");
        let scope = fauna_base.join(ACTOR_A);
        std::fs::create_dir_all(scope.join("inner")).expect("scope");
        std::fs::write(scope.join("inner").join("mls.db"), b"x").expect("db");

        let deny = |mode: u32| {
            let mut perms = std::fs::metadata(&fauna_base).expect("meta").permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                perms.set_mode(mode);
            }
            std::fs::set_permissions(&fauna_base, perms).expect("chmod");
        };
        deny(0o500);
        let survivors = erase_under(&fauna_base, ACTOR_A);
        // Restore before asserting, so a failing assert cannot leave the tmpdir
        // undeletable for the harness.
        deny(0o700);

        if scope.exists() {
            assert_eq!(
                survivors.len(),
                1,
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
        } else {
            // Running as root (or on a filesystem that ignores the mode) — the
            // fault could not be injected, so this proves nothing. Say so
            // loudly rather than passing for the wrong reason.
            eprintln!("SKIP: this process can write through a read-only dir; no fault injected");
        }
    }

    #[test]
    fn erase_removes_one_actors_scoped_dirs_across_all_three_bases_leaving_others() {
        let tmp = tempfile::tempdir().unwrap();
        let fauna_base = tmp.path().join("fauna");
        let bases = [
            fauna_base.clone(),
            fauna_base.join("sync"),
            fauna_base.join("backup"),
        ];
        std::fs::create_dir_all(&fauna_base).unwrap();

        for base in &bases {
            let a = fauna_sync_engine::db::actor_state_dir(base, ACTOR_A).unwrap();
            let b = fauna_sync_engine::db::actor_state_dir(base, ACTOR_B).unwrap();
            std::fs::create_dir_all(&a).unwrap();
            std::fs::create_dir_all(&b).unwrap();
            std::fs::write(a.join("marker"), b"a").unwrap();
            std::fs::write(b.join("marker"), b"b").unwrap();
        }

        erase_under(&fauna_base, ACTOR_A);

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

    /// The sign-out's safety net: every actor scope under all three bases goes,
    /// an orphan the registry no longer names included, while install-scoped
    /// state survives — the settings files, the two lock files (unlinking a
    /// *held* lock lets the next opener create a fresh inode, so the exclusion
    /// silently stops excluding) and the install device secret (the returning
    /// sign-in re-derives its device id from it — `sync-agent-credentials.md`
    /// § Credential model). A flat file at a base is no account's and stays.
    #[test]
    fn erase_all_scopes_sweeps_every_actor_scope_but_not_install_scoped_files() {
        let tmp = tempfile::tempdir().unwrap();
        let fauna_base = tmp.path().join("fauna");
        let scopes: Vec<PathBuf> = erase_bases(&fauna_base)
            .iter()
            .map(|base| fauna_sync_engine::db::actor_state_dir(base, ACTOR_B).unwrap())
            .collect();
        for scope in &scopes {
            std::fs::create_dir_all(scope).unwrap();
            std::fs::write(scope.join("marker"), b"b").unwrap();
        }
        for name in ["theme.json", "window-state.json", "app-settings.json"] {
            std::fs::write(fauna_base.join(name), b"{}").unwrap();
        }
        std::fs::write(fauna_base.join("mls_state.db"), b"flat").unwrap();
        let registry_lock = fauna_base.join("account-registry.lock");
        let instance_lock = fauna_base.join(format!("instance-{ACTOR_A}.lock"));
        std::fs::write(&registry_lock, b"").unwrap();
        std::fs::write(&instance_lock, b"").unwrap();
        let install_device_secret = fauna_base
            .join("sync")
            .join(fauna_sync_engine::engine_lifecycle::INSTALL_DEVICE_SECRET_FILE);
        std::fs::write(&install_device_secret, [7u8; 32]).unwrap();

        let sweep = erase_all_scopes_under(&fauna_base);

        assert!(sweep.is_clean(), "{:?}", sweep.survivors);
        for scope in &scopes {
            assert!(!scope.exists(), "{} must be swept", scope.display());
        }
        for name in [
            "theme.json",
            "window-state.json",
            "app-settings.json",
            "mls_state.db",
        ] {
            assert!(fauna_base.join(name).exists(), "{name} must survive");
        }
        assert!(registry_lock.exists());
        assert!(instance_lock.exists());
        assert!(install_device_secret.exists());
    }

    /// linux and tui on one OS login, over temp dirs: linux's install base,
    /// the account-store root inside it (the accident of naming
    /// [`with_serving_bases`] warns about, reproduced), and tui's own base.
    struct Seats {
        tmp: tempfile::TempDir,
        fauna_base: PathBuf,
        store_root: PathBuf,
        tui_base: PathBuf,
    }

    impl Seats {
        fn new() -> Self {
            let tmp = tempfile::tempdir().expect("tmpdir");
            let fauna_base = tmp.path().join("fauna");
            let store_root = fauna_base.join("sync");
            let tui_base = tmp.path().join("fauna-tui");
            for dir in [&store_root, &tui_base] {
                std::fs::create_dir_all(dir).expect("base");
            }
            Self {
                tmp,
                fauna_base,
                store_root,
                tui_base,
            }
        }
        fn linux(&self) -> fauna_client_accounts::ServingBases<'_> {
            fauna_client_accounts::ServingBases {
                state_base: Some(&self.fauna_base),
                store_root: Some(&self.store_root),
            }
        }
        /// A live tui instance serving `actor`, for as long as it is held.
        fn tui_serving(&self, actor: &str) -> fauna_client_accounts::SessionInstanceHolder {
            let mut tui = fauna_client_accounts::SessionInstanceHolder::new();
            tui.become_session_instance(
                fauna_client_accounts::ServingBases {
                    state_base: Some(&self.tui_base),
                    store_root: Some(&self.store_root),
                },
                actor,
                None,
                fauna_client_accounts::ServingMode::Concurrent,
            );
            assert!(tui.holds_lock(), "the sibling is genuinely serving it");
            tui
        }
        /// A registry over a file-backed store in this temp dir, holding two
        /// accounts; returns the second — what a switcher's remove button
        /// targets (the first is active).
        fn registry_with_two_accounts(&self) -> (fauna_client_accounts::AccountRegistry, String) {
            let registry = crate::account_registry_in(self.tmp.path().join("credentials"));
            registry
                .add_account(&"01".repeat(32), None, None)
                .expect("first account");
            let removable = registry
                .add_account(&"02".repeat(32), None, None)
                .expect("second account");
            (registry, removable)
        }
        /// Every scope linux's erase reaches for `actor`, created on disk.
        fn scopes_for(&self, actor: &str) -> Vec<PathBuf> {
            let scopes: Vec<PathBuf> = erase_bases(&self.fauna_base)
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
        let (registry, actor) = seats.registry_with_two_accounts();
        let scopes = seats.scopes_for(&actor);

        let tui = seats.tui_serving(&actor);
        assert_eq!(
            remove_account_under(
                &registry,
                None,
                seats.linux(),
                Some(&seats.fauna_base),
                &actor
            ),
            Err(crate::i18n::strings::settings::REMOVE_ACCOUNT_BLOCKED_OTHER_WINDOW.to_string()),
            "the shared remove-account refusal, for the page's error line"
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

        drop(tui);
        assert_eq!(
            remove_account_under(
                &registry,
                None,
                seats.linux(),
                Some(&seats.fauna_base),
                &actor
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
        let (registry, actor) = seats.registry_with_two_accounts();
        let _tui = seats.tui_serving(ACTOR_A);
        assert_eq!(
            remove_account_under(
                &registry,
                None,
                seats.linux(),
                Some(&seats.fauna_base),
                &actor
            ),
            Ok(())
        );
    }

    /// ⚠ **A bound instance must not remove the account it is serving**. The registry's active account is the first one,
    /// and this process serves the second — a bound secondary, which never moves
    /// the active pointer (`account-scoping.md` § Concurrent instances). No
    /// sibling holds anything: the sibling probe puts this process's own lock
    /// down by design, so it alone would call the account free and unlink the
    /// stores this very process runs from. Twin of tui's pin.
    #[test]
    fn remove_account_refuses_the_account_this_process_serves_and_touches_nothing() {
        let seats = Seats::new();
        let (registry, served_here) = seats.registry_with_two_accounts();
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
                seats.linux(),
                Some(&seats.fauna_base),
                &served_here,
            ),
            Err(crate::i18n::strings::settings::REMOVE_ACCOUNT_BLOCKED_THIS_WINDOW.to_string()),
            "the served-here refusal, for the page's error line"
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

    /// ⚠ **The unreadable-index floor runs the sign-out's erase over an index
    /// that names nobody** — so a gate fed by the registry asked about nothing
    /// and refused nothing, and the floor erased every scope on disk under a
    /// live sibling. The question now comes from the bases the erase sweeps.
    #[test]
    fn the_all_accounts_erase_asks_about_scopes_a_malformed_index_cannot_name() {
        let seats = Seats::new();
        let registry = crate::account_registry_in(seats.tmp.path().join("credentials"));
        registry
            .add_account(&"03".repeat(32), None, None)
            .expect("an account the index would have named");
        let credentials = fauna_credential_store::CredentialStore::with_file_backend(
            "fauna-linux-test",
            seats.tmp.path().join("credentials"),
        );
        fauna_client_accounts::SecretStore::set(
            &credentials,
            fauna_client_accounts::INDEX_KEY,
            "{ not an index",
        );
        assert_eq!(
            registry.index_refusal(),
            Some(fauna_launch_machine::AccountIndexRefusal::Malformed),
            "the precondition: this is the floor's index"
        );
        assert!(registry.list().is_empty(), "and it names nobody");

        // A scope on disk only — under linux's own base, and under the shared
        // root a tui-only account would occupy.
        std::fs::create_dir_all(seats.fauna_base.join(ACTOR_B)).expect("scope");
        std::fs::create_dir_all(seats.store_root.join(ACTOR_C)).expect("store scope");
        let tui = seats.tui_serving(ACTOR_B);
        let tui_too = seats.tui_serving(ACTOR_C);
        assert_eq!(
            all_accounts_erase_blocked_under(&registry, seats.linux(), Some(&seats.fauna_base))
                .map(|b| b.accounts),
            Some(vec![ACTOR_B.to_string(), ACTOR_C.to_string()]),
        );
        drop((tui, tui_too));
        assert_eq!(
            all_accounts_erase_blocked_under(&registry, seats.linux(), Some(&seats.fauna_base)),
            None,
            "with nobody serving them, the floor may run"
        );
    }

    /// ⚠ **The guard's predicate was pinned only in the library — the GTK
    /// confirm closure that calls it had no witness of its own**:
    /// before this test, deleting `settings::account::sign_out_confirm_unless`'s
    /// one-line guard reddened nothing, anywhere (`account-scoping.md`
    /// § Concurrent instances → *An erase refuses while a sibling serves the
    /// account*). Drives the real gesture — not a fake refusal — over temp
    /// bases: a live sibling refuses it first, and once it is gone the
    /// sign-out succeeds despite an on-disk scope no registry entry names
    /// (`account-scoping.md:981`, the erase's actual reach).
    #[test]
    fn sign_out_confirm_refuses_under_a_sibling_and_then_signs_out_around_an_orphan_scope() {
        use gtk::prelude::WidgetExt;

        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let seats = Seats::new();
            let (registry, actor) = seats.registry_with_two_accounts();
            let scopes = seats.scopes_for(&actor);

            // A scope on disk that no registry entry names — the erase's
            // actual reach, not just what this app's own index happens to
            // list.
            std::fs::create_dir_all(seats.store_root.join(ACTOR_C)).expect("orphan scope");

            let blocked = || {
                all_accounts_erase_blocked_under(&registry, seats.linux(), Some(&seats.fauna_base))
                    .map(|_| {
                        fauna_client_accounts::sign_out_blocked_copy()
                            .resolve(crate::i18n::strings::lookup)
                    })
            };

            let error_label = gtk::Label::new(None);
            let signed_out = std::rc::Rc::new(std::cell::Cell::new(false));
            {
                let signed_out = signed_out.clone();
                crate::settings::set_sign_out_handler(move || signed_out.set(true));
            }

            let tui = seats.tui_serving(&actor);
            crate::settings::account::sign_out_confirm_unless(&error_label, blocked);
            assert!(
                !signed_out.get(),
                "refused: nothing erased, no credentials wiped, still signed in"
            );
            assert_eq!(
                error_label.text().to_string(),
                fauna_client_accounts::sign_out_blocked_copy()
                    .resolve(crate::i18n::strings::lookup),
                "the refusal reaches the page's error label"
            );
            assert!(error_label.is_visible());
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

            drop(tui);
            crate::settings::account::sign_out_confirm_unless(&error_label, blocked);
            assert!(
                signed_out.get(),
                "with the sibling gone, sign-out goes through — the orphan scope must not block it"
            );
        });
    }

    /// ⚠ **The remove-account button's closure had no witness of its own**: `account_scope::remove_account` is well witnessed
    /// (`remove_account_refuses_while_another_instance_serves_it_and_touches_nothing`),
    /// but deleting the `if` in `remove_account_confirm_unless`'s call site
    /// reddened nothing. Drives the real `remove_account_under` — not a
    /// stub — over temp bases: a live sibling refuses it (nothing dropped,
    /// nothing unparented), and once it is gone the removal actually runs
    /// the listbox surgery. Remove-account's reach is its one actor
    /// (`account-scoping.md:981`), so — unlike the all-accounts sweeps — no
    /// orphan scope belongs in its one-window arm.
    #[test]
    fn remove_account_confirm_refuses_under_a_sibling_and_then_removes_it() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let seats = Seats::new();
            let (registry, actor) = seats.registry_with_two_accounts();
            let scopes = seats.scopes_for(&actor);

            let remove = |actor_id: &str| {
                remove_account_under(
                    &registry,
                    None,
                    seats.linux(),
                    Some(&seats.fauna_base),
                    actor_id,
                )
            };

            let error_label = gtk::Label::new(None);
            let removed = std::rc::Rc::new(std::cell::Cell::new(false));

            let tui = seats.tui_serving(&actor);
            {
                let removed = removed.clone();
                crate::settings::account::remove_account_confirm_unless(
                    &actor,
                    &error_label,
                    remove,
                    move || removed.set(true),
                );
            }
            assert!(
                !removed.get(),
                "refused: nothing removed, nothing unparented, while a sibling serves it"
            );
            assert_eq!(
                error_label.text().to_string(),
                crate::i18n::strings::settings::REMOVE_ACCOUNT_BLOCKED_OTHER_WINDOW,
                "the refusal reaches the page's error label"
            );
            assert!(
                registry.list().iter().any(|a| a.actor_id == actor),
                "a refused remove keeps the registry entry"
            );
            for scope in &scopes {
                assert!(
                    scope.join("data").exists(),
                    "{scope:?} removed under a live sibling"
                );
            }

            drop(tui);
            {
                let removed = removed.clone();
                crate::settings::account::remove_account_confirm_unless(
                    &actor,
                    &error_label,
                    remove,
                    move || removed.set(true),
                );
            }
            assert!(
                removed.get(),
                "with the sibling gone, the removal goes through and unparents the row"
            );
            assert!(!registry.list().iter().any(|a| a.actor_id == actor));
            for scope in &scopes {
                assert!(!scope.exists(), "{scope:?} survived an unrefused remove");
            }
        });
    }

    /// ⚠ **The unreadable-index floor's confirm had no witness of its own**: `account_scope::start_over_blocked` is well
    /// witnessed at the predicate level
    /// (`the_all_accounts_erase_asks_about_scopes_a_malformed_index_cannot_name`),
    /// but deleting the `if` in `main::start_over_confirm_unless` reddened
    /// nothing. Drives the real gesture over temp bases: a live sibling
    /// refuses it (the window survives, nothing is signed out), and once it
    /// is gone the floor still runs despite an on-disk actor scope no
    /// registry entry names (`account-scoping.md:981`, the erase's actual
    /// reach — the floor IS the all-accounts sweep, unlike remove-account).
    #[test]
    fn start_over_confirm_refuses_under_a_sibling_and_then_signs_out_around_an_orphan_scope() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let seats = Seats::new();
            let (registry, actor) = seats.registry_with_two_accounts();
            let scopes = seats.scopes_for(&actor);

            // A scope on disk that no registry entry names — the erase's
            // actual reach, not just what this app's own index happens to
            // list.
            std::fs::create_dir_all(seats.store_root.join(ACTOR_C)).expect("orphan scope");

            let blocked = || {
                all_accounts_erase_blocked_under(&registry, seats.linux(), Some(&seats.fauna_base))
                    .map(|_| {
                        fauna_client_accounts::start_over_blocked_copy()
                            .resolve(crate::i18n::strings::lookup)
                    })
            };

            let window = adw::ApplicationWindow::builder().build();
            let error_label = gtk::Label::new(None);
            let view = crate::views::launch::LaunchView {
                window: window.clone(),
                set_phase: std::rc::Rc::new(|_| {}),
                set_recover_boxes: std::rc::Rc::new(|_| {}),
                error_label: error_label.clone(),
            };
            let view_holder = std::rc::Rc::new(std::cell::RefCell::new(Some(view)));

            let signed_out = std::rc::Rc::new(std::cell::Cell::new(false));
            {
                let signed_out = signed_out.clone();
                crate::settings::set_sign_out_handler(move || signed_out.set(true));
            }

            let tui = seats.tui_serving(&actor);
            crate::start_over_confirm_unless(&view_holder, blocked);
            assert!(
                !signed_out.get(),
                "refused: nothing erased, no credentials wiped, still signed in"
            );
            assert_eq!(
                error_label.text().to_string(),
                fauna_client_accounts::start_over_blocked_copy()
                    .resolve(crate::i18n::strings::lookup),
                "the refusal reaches the launch screen's error label"
            );
            assert!(
                view_holder.borrow().is_some(),
                "refused: the launch window is not torn down"
            );
            for scope in &scopes {
                assert!(
                    scope.join("data").exists(),
                    "{scope:?} erased under a live sibling"
                );
            }

            drop(tui);
            crate::start_over_confirm_unless(&view_holder, blocked);
            assert!(
                signed_out.get(),
                "with the sibling gone, the floor goes through — the orphan scope must not block it"
            );
            assert!(
                view_holder.borrow().is_none(),
                "allowed: the launch window is taken and torn down"
            );
        });
    }
}
