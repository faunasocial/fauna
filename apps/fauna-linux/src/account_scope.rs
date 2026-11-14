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
/// actor in hand (`app.rs`'s `SessionEstablished` handler does, via `state.borrow()`
/// — this is for `settings::privacy`/`sync`, whose entry points are called
/// with no session state threaded through).
pub(crate) fn active_actor_id_hex() -> Option<String> {
    crate::account_registry().active()
}

/// Erase actor `actor_id_hex`'s account-scoped local state under the flat
/// `fauna/` base passed in — the remove-account's erase; a sign-out's goes
/// through [`finish`]. Same
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
/// The shared finish (`fauna_client_accounts::finish_sign_out`) sweeps the same three.
fn erase_bases(fauna_base: &Path) -> [PathBuf; 3] {
    [
        fauna_base.to_path_buf(),
        fauna_base.join("sync"),
        fauna_base.join("backup"),
    ]
}

/// Become (or remain) one of this account's serving instances — called from
/// `app.rs`'s `SessionEstablished` arm BEFORE any of the account's scoped state
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
    fauna_client_accounts::with_platform_serving_bases(base.as_deref(), f)
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
/// base is [`finish`]'s own no-base case — it erases no
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

/// What a remove-account still owes once its registry step has landed: the
/// nest half over the snapshot the removal took ([`Self::removed`]), then the
/// erase of the account's scopes ([`Self::erase`]) — in that order, since the
/// nest half assembles over the account's still-present store
/// (`account-scoping.md` § Erasure follows scope → *Removing an account
/// retires this machine from it, as a sign-out does*, decision 1). Twin of
/// tui's `account_scope::RemovalOwed`.
pub(crate) struct RemovalOwed {
    /// The removed account's slots, in memory — what the nest half signs in
    /// and assembles with. Zeroized when the last holder drops it.
    pub(crate) removed: std::sync::Arc<fauna_client_accounts::RemovedAccountCredentials>,
    /// The base the erase sweeps; `None` when the process has no config dir,
    /// as [`finish`]'s no-base case.
    fauna_base: Option<PathBuf>,
}

impl RemovalOwed {
    /// Erase the removed account's scopes — the gesture's last step, run once
    /// its nest half has finished (or found nothing to do) — and settle the
    /// account out of the sign-out record the removal joined.
    pub(crate) fn erase(self) {
        if let Some(fauna_base) = &self.fauna_base {
            let actor = self.removed.actor_id();
            erase_under(fauna_base, actor);
            fauna_client_accounts::SignOutRecord::settle(
                &fauna_client_accounts::SignOutRecordFile::under(fauna_base),
                actor,
            );
        }
    }
}

/// Remove one account from this install — the `account-remove-button`
/// gesture, in the only order that is safe to refuse part-way through:
/// **ask, then drop the registry entry, then retire, then erase**
/// (`account-scoping.md` § Erasure follows scope → *Removing an account
/// retires this machine from it, as a sign-out does*). Twin of tui's
/// `account_scope::remove_account`.
///
/// The question comes first because every later step destroys something a
/// refusal cannot give back: the registry removal drops the account's secret
/// slots, the retirement ends this machine's grant on the account's nest, and
/// the erase unlinks the scope a live sibling is running out of
/// (`account-scoping.md` § Concurrent instances → *An erase refuses while a
/// sibling serves the account*). The registry removal is the one decision
/// point and lands before anything awaits; the slots the retirement needs are
/// taken into memory in the same mutation
/// (`AccountRegistry::remove_with_snapshot`).
///
/// Returns as soon as the registry step lands — the switcher row goes then.
/// The nest half and the erase behind it run as a stop
/// ([`crate::account_runtime::finish_removal`]).
///
/// `Err` is the line for the Account page's `error-message`: the shared
/// refusal ([`fauna_client_accounts::remove_account_blocked_copy`]) or the
/// registry's own failure; both are logged.
pub(crate) fn remove_account(actor_id_hex: &str) -> Result<(), String> {
    let registry = crate::account_registry();
    let fauna_base = install_state_base();
    let serving_here = fauna_client_accounts::process_session_account();
    let owed = with_serving_bases(|bases| {
        remove_account_under(
            &registry,
            serving_here.as_deref(),
            bases,
            fauna_base.as_deref(),
            actor_id_hex,
        )
    })?;
    crate::account_runtime::finish_removal(owed);
    Ok(())
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
) -> Result<RemovalOwed, String> {
    let store_root = bases.store_root;
    if let Some(blocked) =
        fauna_client_accounts::remove_account_blocked_as(serving_here, bases, actor_id_hex)
    {
        tracing::warn!("[settings/account] remove-account REFUSED ({blocked:?}) — {actor_id_hex}");
        return Err(blocked.copy().resolve(crate::i18n::strings::lookup));
    }
    if let Some(owed) =
        fauna_client_accounts::remove_account_carry_owed(registry, store_root, actor_id_hex)
    {
        tracing::warn!("[settings/account] remove-account REFUSED (carry owed) — {actor_id_hex}");
        return Err(owed.copy().resolve(crate::i18n::strings::lookup));
    }
    // The removal joins the sign-out record before its registry step, so the
    // store that step's crash-safety argument leaves unreached is finished by
    // the next launch (`account-scoping.md` § Erasure follows scope → *The
    // sign-out record is on all 7 apps*, decision 1); settled after the erase
    // ([`RemovalOwed::erase`]).
    let slot = fauna_base.map(fauna_client_accounts::SignOutRecordFile::under);
    if let Some(slot) = &slot {
        fauna_client_accounts::SignOutRecord::record_removal(slot, actor_id_hex);
    }
    let removed = registry.remove_with_snapshot(actor_id_hex).map_err(|e| {
        tracing::error!("[settings/account] remove account failed: {e:#}");
        if let Some(slot) = &slot {
            fauna_client_accounts::SignOutRecord::settle(slot, actor_id_hex);
        }
        crate::i18n::strings::onboarding::session_error::remove_account(&e.to_string())
    })?;
    // Erasure follows scope (account-scoping.md § Serialized switching):
    // removing an account erases its local account-scoped state, not only the
    // registry entry — once the nest half is done with its store.
    Ok(RemovalOwed {
        removed: std::sync::Arc::new(removed),
        fauna_base: fauna_base.map(Path::to_path_buf),
    })
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

/// How long the launch's finish waits on the sync agent's dismissal of one
/// recorded account (`account-scoping.md` § Erasure follows scope → *The
/// sign-out record is on all 7 apps*, decision 5) — tui's budget. The exchange
/// is two IPC round trips; an agent that answers neither in this long is left
/// to meet the revoke at the nest, as an offline sign-out's agent is.
const AGENT_DISMISS_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// **The sign-out's decision**, written by the sign-out handler before
/// anything else — after the serving question passed at the confirm, before
/// the teardown is counted and before the stop is spawned (`account-scoping.md`
/// § Erasure follows scope → *The sign-out record is on all 7 apps*, decision
/// 1): the native record (`fauna_client_accounts::SignOutRecordFile`, directly
/// under linux's install base) names every account the registry lists plus the
/// account this process serves — the holder's actor, which an unreadable index
/// would not name — and owes the credential wipe. A process killed anywhere
/// after this line is finished by the next launch ([`finish_at_launch`]). Twin
/// of tui's `account_scope::record_sign_out`.
///
/// Returns the record as written, which the gesture's tail hands to the
/// finish: a record the install base refused (or no resolvable base at all)
/// still finishes in this process.
pub(crate) fn record_sign_out() -> fauna_client_accounts::SignOutRecord {
    let reach: Vec<String> = crate::account_registry()
        .list()
        .into_iter()
        .map(|entry| entry.actor_id)
        .chain(fauna_client_accounts::process_session_account())
        .collect();
    let record = match install_state_base() {
        Some(base) => fauna_client_accounts::SignOutRecord::record_sign_out(
            &fauna_client_accounts::SignOutRecordFile::under(&base),
            reach,
        ),
        None => {
            let mut accounts: Vec<String> = Vec::new();
            for actor in reach.into_iter().map(|a| a.to_ascii_lowercase()) {
                if !accounts.contains(&actor) {
                    accounts.push(actor);
                }
            }
            fauna_client_accounts::SignOutRecord {
                wipe_owed: true,
                accounts,
            }
        }
    };
    tracing::info!(
        "[sign-out] recorded for {} account(s)",
        record.accounts.len()
    );
    record
}

/// **The one routine** the sign-out's tail, the launch and Remove Again run —
/// the shared [`fauna_client_accounts::finish_sign_out_with`] over linux's
/// install base, its three [`erase_bases`], the serving bases and its
/// registry, with linux's own credential acts ([`LinuxFinishHost`]). Twin of
/// tui's `account_scope::finish`.
///
/// `caller` says which of the three this run is: the sync agent is not
/// dismissed again for the gesture, whose own stop already dismissed it (`account_runtime::teardown` awaits the un-provision), and
/// only Remove Again re-sweeps the residue's paths while the registry names
/// somebody;
/// `written` is the gesture's own record (`None` elsewhere).
///
/// When the run wiped the credentials, the in-memory state deliberately built
/// to survive an account SWITCH goes too — the account-cache mirror and the
/// two succession cells (`settings::recovery_kit::{LANDED_SWEEP,
/// SUCCESSION_KIT_OWED}`): an erase destroys every identity on the box, so
/// there is no successor left to owe a kit to.
pub(crate) fn finish(
    caller: fauna_client_accounts::FinishCaller,
    written: Option<fauna_client_accounts::SignOutRecord>,
) -> fauna_client_accounts::SignOutFinish {
    let registry = crate::account_registry();
    let namespace = crate::client::credential_namespace();
    let outcome = match install_state_base() {
        Some(base) => with_serving_bases(|serving| {
            finish_under(&registry, &namespace, &base, serving, caller, written)
        }),
        None => finish_without_base(&registry, &namespace, written),
    };
    if matches!(
        outcome,
        fauna_client_accounts::SignOutFinish::Finished {
            credentials_wiped: true,
            ..
        }
    ) {
        crate::client::forget_account_cache();
        crate::settings::recovery_kit::clear_succession_sweep();
        crate::settings::recovery_kit::clear_succession_kit_debt();
    }
    outcome
}

/// No resolvable install base: no directory to sweep and nowhere to keep a
/// record, so the gesture's tail wipes the credentials of the accounts it
/// recorded in memory and nothing else — what a sign-out with no base always
/// did. A launch has no record to finish.
fn finish_without_base(
    registry: &fauna_client_accounts::AccountRegistry,
    namespace: &fauna_credential_store::CredentialStore,
    written: Option<fauna_client_accounts::SignOutRecord>,
) -> fauna_client_accounts::SignOutFinish {
    use fauna_client_accounts::FinishHost as _;
    let Some(record) = written else {
        return fauna_client_accounts::SignOutFinish::NOTHING;
    };
    let mut host = LinuxFinishHost {
        registry,
        namespace,
        dismiss_agent: false,
    };
    let mut credentials = host.wipe();
    for actor in &record.accounts {
        credentials
            .survivors
            .extend(host.erase_account_slots(actor).survivors);
    }
    fauna_client_accounts::log_residue(&[], &credentials);
    fauna_client_accounts::SignOutFinish::Finished {
        residue: None,
        credentials_wiped: true,
        found_record: true,
    }
}

/// The pure half of [`finish`], every base and store passed in — same
/// env-mutation-free reasoning as [`erase_under`].
fn finish_under(
    registry: &fauna_client_accounts::AccountRegistry,
    namespace: &fauna_credential_store::CredentialStore,
    fauna_base: &Path,
    serving: fauna_client_accounts::ServingBases<'_>,
    caller: fauna_client_accounts::FinishCaller,
    written: Option<fauna_client_accounts::SignOutRecord>,
) -> fauna_client_accounts::SignOutFinish {
    let swept = erase_bases(fauna_base);
    let mut host = LinuxFinishHost {
        registry,
        namespace,
        // The gesture's own stop already dismissed the sync agent.
        dismiss_agent: caller != fauna_client_accounts::FinishCaller::Gesture,
    };
    fauna_client_accounts::finish_sign_out_with(
        fauna_client_accounts::FinishSeat {
            install_base: fauna_base,
            swept_bases: &swept,
            serving,
            registry,
            caller,
        },
        &mut host,
        written,
    )
}

/// linux's own acts inside the shared finish: the wipe is the app's own
/// credential namespace (`fauna-desktop`: the index, the app's own per-actor
/// slots, the wholesale `delete_namespace`); each account's slots in the
/// shared `fauna-account-store` namespace ride with its scopes; the residue
/// retry's credential half is the re-erase it has always been. Twin of tui's
/// `TuiFinishHost`.
struct LinuxFinishHost<'a> {
    registry: &'a fauna_client_accounts::AccountRegistry,
    namespace: &'a fauna_credential_store::CredentialStore,
    dismiss_agent: bool,
}

impl fauna_client_accounts::FinishHost for LinuxFinishHost<'_> {
    fn wipe(&mut self) -> fauna_client_accounts::CredentialSweep {
        fauna_credential_store::wipe_own_credentials(self.registry, self.namespace)
    }

    fn erase_account_slots(
        &mut self,
        actor_id_hex: &str,
    ) -> fauna_client_accounts::CredentialSweep {
        fauna_credential_store::erase_account_credentials(self.registry, actor_id_hex)
    }

    fn dismiss_agent(&mut self, actor_id_hex: &str) {
        if self.dismiss_agent {
            dismiss_agent_from(actor_id_hex);
        }
    }

    fn re_erase_credentials(
        &mut self,
        recorded: fauna_client_accounts::CredentialSweep,
    ) -> fauna_client_accounts::CredentialSweep {
        fauna_credential_store::re_erase_credentials(self.registry, self.namespace, recorded)
    }
}

/// Dismiss the sync agent from a recorded account, awaited — the actor-matched
/// exchange the wizard landing runs (`GetServiceStatus`, then
/// `UnprovisionCapability` only when the agent's advertised principal is this
/// account), over a marker built from the record rather than read from the
/// provisioner's slot: a provisioner killed inside its stop never wrote one
/// (decision 5). Its own thread and runtime, so the launch's synchronous call
/// site — `main`, before any runtime or GApplication exists — can wait on it;
/// bounded by [`AGENT_DISMISS_BUDGET`]. No agent reachable answers at once.
fn dismiss_agent_from(actor_id_hex: &str) {
    let Ok(actor) = fauna_core::hex32::decode(actor_id_hex) else {
        return;
    };
    let Ok(endpoint) = fauna_ipc::endpoint::AgentEndpoint::default_for_user() else {
        return;
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let marker = fauna_ipc::sync::SignedOutMarker::new(actor.to_vec(), now_ms);
    let label = actor_id_hex.to_string();
    let spawned = std::thread::Builder::new()
        .name("sign-out-agent".into())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async {
                let exchange =
                    fauna_client_sync::agent::signed_out_onboarding_reconcile_with_marker(
                        &endpoint, &marker,
                    );
                if tokio::time::timeout(AGENT_DISMISS_BUDGET, exchange)
                    .await
                    .is_err()
                {
                    tracing::warn!(
                        "[sign-out] the sync agent did not answer its dismissal from {label} \
                         within {AGENT_DISMISS_BUDGET:?} — erasing all the same"
                    );
                }
            });
        });
    if let Ok(handle) = spawned {
        let _ = handle.join();
    }
}

/// The residue as the `sign-out-residue` view paints it — the one shared
/// [`fauna_client_accounts::ResidueSurface`].
pub(crate) use fauna_client_accounts::ResidueSurface;

thread_local! {
    /// What the last finish left, while it still owes work — read by
    /// `identity_choice`'s `sign-out-residue` view on every tick. GTK main
    /// thread only, like every other widget-facing cell here (the launch's
    /// finish runs on `main`'s thread, which becomes the GTK main thread).
    /// Held OUTSIDE the wizard because the wizard is rebuilt by every sign-out
    /// and does not own the fact: the residue belongs to the device.
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
/// view goes, and the record stays on disk for the next signed-out launch's
/// finish to re-sweep ([`finish_at_launch`]).
pub(crate) fn forget_sign_out_residue_view() {
    set_sign_out_residue(None);
}

/// Paint what a [`finish`] run answered as the `sign-out-residue` view: the
/// residue line, the retry's refusal line, or nothing for a clean device.
pub(crate) fn paint_finish(outcome: &fauna_client_accounts::SignOutFinish) {
    set_sign_out_residue(outcome.surface(fauna_i18n::strings::lookup));
}

/// The launch's call, before the launch law reads the registry — ahead of
/// `launch_collision_detected` in `main` (`account-scoping.md` § Erasure
/// follows scope → *The sign-out record is on all 7 apps*, decision 3):
/// whatever a sign-out cut short owes is finished first — the agent
/// dismissed, the wipe, each recorded account's erase, the residue re-sweep —
/// and what is left is kept for `identity_choice` to paint. Retires the
/// post-routing `recheck_residue_at_launch`.
///
/// `true` means the launch **fails closed**: a refused finish whose record
/// still owes its wipe — the launch must not route over the registry, so it
/// lands on the fresh wizard painting the retry's refusal line, whose Remove
/// Again re-runs the routine (decision 4).
pub(crate) fn finish_at_launch() -> bool {
    let outcome = finish(fauna_client_accounts::FinishCaller::Launch, None);
    paint_finish(&outcome);
    outcome.fails_closed()
}

/// Remove Again (`sign-out-residue-retry-button`): the same routine, and paint
/// what is left — nothing, when the device is now clean.
pub(crate) fn retry_residue() {
    if sign_out_residue().is_none() {
        return;
    }
    let outcome = finish(fauna_client_accounts::FinishCaller::RemoveAgain, None);
    paint_finish(&outcome);
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

    // "Every account served somewhere → nothing to offer" is pinned once, in
    // `fauna_client_accounts::instance_lock`; the view's two exits are pinned
    // in the view's own tests.

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

    /// The line a [`finish`] paints, without its side effects (the log,
    /// the install-scoped record under the real config dir, the view cell).
    fn report_residue(
        survivors: &[PathBuf],
        credentials: &fauna_client_accounts::CredentialSweep,
    ) -> Option<String> {
        fauna_client_accounts::ResidueSurface::for_record(
            fauna_client_accounts::SignOutResidue::record(survivors, credentials),
            fauna_i18n::strings::lookup,
        )
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

        // A sign-out record naming nobody — the unreadable-index floor's
        // start-over — finished by the one routine: its residual sweep is
        // what reaches the orphan.
        let credentials = tmp.path().join("credentials");
        let registry = crate::account_registry_in(credentials.clone());
        let namespace = fauna_credential_store::CredentialStore::with_file_backend(
            "fauna-linux-test",
            credentials,
        );
        let slot = fauna_client_accounts::SignOutRecordFile::under(&fauna_base);
        fauna_client_accounts::SignOutRecord::record_sign_out(&slot, Vec::<String>::new());
        let outcome = finish_under(
            &registry,
            &namespace,
            &fauna_base,
            fauna_client_accounts::ServingBases {
                state_base: Some(&fauna_base),
                store_root: Some(&fauna_base.join("sync")),
            },
            fauna_client_accounts::FinishCaller::Gesture,
            None,
        );

        assert!(
            matches!(
                outcome,
                fauna_client_accounts::SignOutFinish::Finished { residue: None, .. }
            ),
            "{outcome:?}"
        );
        assert!(
            fauna_client_accounts::SignOutRecord::load(&slot).is_none(),
            "the record goes with its last account"
        );
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

    /// A finish with every base and store from `seats` — the one routine the
    /// launch, the gesture's tail and Remove Again run ([`finish_under`]).
    fn finish_seats(
        seats: &Seats,
        registry: &fauna_client_accounts::AccountRegistry,
    ) -> fauna_client_accounts::SignOutFinish {
        let namespace = fauna_credential_store::CredentialStore::with_file_backend(
            "fauna-linux-test",
            seats.tmp.path().join("credentials"),
        );
        finish_under(
            registry,
            &namespace,
            &seats.fauna_base,
            seats.linux(),
            fauna_client_accounts::FinishCaller::Gesture,
            None,
        )
    }

    /// **The launch after the app died mid-sign-out** (`account-scoping.md`
    /// § Erasure follows scope → *The sign-out record is on all 7 apps*,
    /// decision 7): the record is on disk and nothing after it ran — the
    /// registry still lists both accounts, their scopes are all there. The
    /// launch's finish wipes the registry, erases every recorded account's
    /// scopes and clears the record, so routing then finds nobody.
    #[test]
    fn a_launch_finish_over_a_recorded_sign_out_wipes_erases_and_clears_the_record() {
        let seats = Seats::new();
        let (registry, second) = seats.registry_with_two_accounts();
        let first = "01".repeat(32);
        let scopes: Vec<PathBuf> = seats
            .scopes_for(&first)
            .into_iter()
            .chain(seats.scopes_for(&second))
            .collect();
        let slot = fauna_client_accounts::SignOutRecordFile::under(&seats.fauna_base);
        fauna_client_accounts::SignOutRecord::record_sign_out(
            &slot,
            registry.list().into_iter().map(|e| e.actor_id),
        );
        assert!(slot.path().exists(), "the gesture's record is on disk");

        let outcome = finish_seats(&seats, &registry);

        assert!(
            matches!(
                outcome,
                fauna_client_accounts::SignOutFinish::Finished {
                    residue: None,
                    credentials_wiped: true,
                    found_record: true,
                }
            ),
            "{outcome:?}"
        );
        assert!(registry.list().is_empty(), "the wipe took the index");
        assert!(registry.secrets(&first).is_none() && registry.secrets(&second).is_none());
        for scope in &scopes {
            assert!(!scope.exists(), "{} must be erased", scope.display());
        }
        assert!(
            fauna_client_accounts::SignOutRecord::load(&slot).is_none(),
            "the record goes with its last account"
        );
    }

    /// **A refused finish wipes and erases nothing, and the launch fails
    /// closed** (decision 4): a sibling still serves a recorded account, so the
    /// record stands whole, the registry and every scope stay, and the outcome
    /// says the launch must not route over the registry — painting the retry's
    /// refusal line instead. Once the sibling is gone the same routine (Remove
    /// Again) finishes it.
    #[test]
    fn a_finish_refuses_while_a_sibling_serves_a_recorded_account_and_fails_closed() {
        let seats = Seats::new();
        let (registry, _) = seats.registry_with_two_accounts();
        // An actor of this test's own: the in-process sibling holders of the
        // other tests here serve `02…`, and a probe in the same process sees
        // them.
        let actor = registry
            .add_account(&"05".repeat(32), None, None)
            .expect("a third account");
        let scopes = seats.scopes_for(&actor);
        let slot = fauna_client_accounts::SignOutRecordFile::under(&seats.fauna_base);
        fauna_client_accounts::SignOutRecord::record_sign_out(
            &slot,
            registry.list().into_iter().map(|e| e.actor_id),
        );

        let tui = seats.tui_serving(&actor);
        let outcome = finish_seats(&seats, &registry);

        assert!(outcome.fails_closed(), "{outcome:?}");
        assert_eq!(
            outcome
                .surface(fauna_i18n::strings::lookup)
                .map(|surface| surface.line),
            Some(
                fauna_client_accounts::sign_out_residue_retry_blocked_copy()
                    .resolve(fauna_i18n::strings::lookup)
            ),
            "the retry's refusal line is what identity_choice paints"
        );
        assert_eq!(registry.list().len(), 3, "nothing wiped");
        for scope in &scopes {
            assert!(
                scope.join("data").exists(),
                "{scope:?} erased under a live sibling"
            );
        }
        let record = fauna_client_accounts::SignOutRecord::load(&slot).expect("record stands");
        assert!(record.wipe_owed, "the record still owes its wipe");

        drop(tui);
        let outcome = finish_seats(&seats, &registry);
        assert!(!outcome.fails_closed(), "{outcome:?}");
        assert!(registry.list().is_empty());
        for scope in &scopes {
            assert!(
                !scope.exists(),
                "{} must go once the sibling is gone",
                scope.display()
            );
        }
        assert!(fauna_client_accounts::SignOutRecord::load(&slot).is_none());
    }

    /// **A remove-account joins the sign-out record before its registry step
    /// and settles after its erase** (decision 1): a crash between the two
    /// leaves the record naming the account, and the next launch's finish
    /// erases the store the registry step's crash-safety left unreached —
    /// owing no wipe, so the remaining account stays signed in.
    #[test]
    fn a_removal_joins_the_record_and_a_launch_finishes_its_unreached_store() {
        let seats = Seats::new();
        let (registry, _) = seats.registry_with_two_accounts();
        // An actor of this test's own, never served by another test's sibling.
        let removable = registry
            .add_account(&"06".repeat(32), None, None)
            .expect("a third account");
        let scopes = seats.scopes_for(&removable);
        let slot = fauna_client_accounts::SignOutRecordFile::under(&seats.fauna_base);

        let owed = remove_account_under(
            &registry,
            None,
            seats.linux(),
            Some(&seats.fauna_base),
            &removable,
        )
        .expect("the removal goes through");
        let record = fauna_client_accounts::SignOutRecord::load(&slot).expect("joined");
        assert!(!record.wipe_owed, "a removal owes no wipe");
        assert_eq!(record.accounts, vec![removable.clone()]);

        // The process dies before the erase: the launch's finish reaches it.
        drop(owed);
        let outcome = finish_seats(&seats, &registry);
        assert!(!outcome.fails_closed(), "{outcome:?}");
        for scope in &scopes {
            assert!(!scope.exists(), "{} must be erased", scope.display());
        }
        assert_eq!(registry.list().len(), 2, "the other accounts are untouched");
        assert!(fauna_client_accounts::SignOutRecord::load(&slot).is_none());

        // The uninterrupted gesture settles its own entry after the erase.
        let third = registry
            .add_account(&"03".repeat(32), None, None)
            .expect("a third account");
        remove_account_under(
            &registry,
            None,
            seats.linux(),
            Some(&seats.fauna_base),
            &third,
        )
        .expect("the removal goes through")
        .erase();
        assert!(
            fauna_client_accounts::SignOutRecord::load(&slot).is_none(),
            "settled after the erase"
        );
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
            )
            .map(RemovalOwed::erase),
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
            )
            .map(RemovalOwed::erase),
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
            )
            .map(RemovalOwed::erase),
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
            )
            .map(RemovalOwed::erase),
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
                .map(RemovalOwed::erase)
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
                phase: std::rc::Rc::new(std::cell::RefCell::new(
                    crate::views::launch::LaunchPhase::Launching,
                )),
                settle_stolen: std::rc::Rc::new(|_| {}),
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
