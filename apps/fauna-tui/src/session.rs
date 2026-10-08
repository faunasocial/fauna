//! Session lifecycle: stored credentials → `NestClient` → connection pump.
//!
//! The tui app consumes the shared seams directly (`common.md` § WS-RPC
//! client binding): identity from `fauna_core::ActorKeypair`, credentials in
//! the shared freedesktop store (`fauna-credential-store`) through the
//! `fauna-client-accounts` registry, and the per-actor WS-RPC channel from
//! `libs/fauna-client::NestClient` — connection lifecycle, auth handshake,
//! and the reconnect supervisor all live in shared Rust. This module is glue:
//! it builds the client, pumps its `ConnectionState` watch into the
//! `UiMessage` channel, and owns sign-out.
//!
//! Boot does **not** connect directly: it hands the long-term store to the
//! shared `LaunchMachine`, which owns the routing decision. See
//! [`crate::launch`].

use std::sync::Arc;

use fauna_client::{AuthClient, NestClient};
use fauna_client_accounts::{AccountRegistry, RegistryLaunchPersistence, SecretStore};
use fauna_core::identity::ActorKeypair;
use fauna_core::secret::SecretString;
use fauna_credential_store::CredentialStore;
use fauna_launch_machine::{AwaitingDnsRecord, PendingInviteRecord};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{App, DataMessage, UiMessage};

/// The tui app's keyring namespace default (`FAUNA_KEYRING_APP` overrides).
///
/// Renamed from `fauna-cli` with the 2026-07-12 `cli` → `tui` rename. That is a
/// change to an **at-rest** namespace, so it is called out rather than done
/// silently: a credential stored under the old name is not found under the new
/// one. It is safe here only because this client has no alpha users — it is
/// pre-parity and outside the shipped client set (`tui.md` § Rollout), so no
/// user-irrecoverable data sits under `fauna-cli`. Anyone who dogfooded the
/// client locally re-onboards. The no-user-data-loss invariant would forbid this
/// once the client ships; a rename after that would need a read-old-write-new
/// migration.
pub const DEFAULT_KEYRING_APP: &str = "fauna-tui";

/// The tui app's shared credential store, resolved from the environment —
/// **with the headless passphrase fallback** (`tui.md` § Credential storage):
/// e2e file dir → existing sealed store → keyring probe → sealed store. The
/// sealed file lives in [`config_dir`] beside the pin store and `mls_state.db`
/// (so the e2e driver's per-run `XDG_CONFIG_HOME` isolation covers it too).
///
/// Call this **once**, at [`App::new`] — every other site reads `app.credentials`.
/// Rebuilding it per call re-reads the environment, which is how a unit test
/// ends up sweeping the developer's real `fauna-tui` keyring namespace.
pub fn secret_store() -> CredentialStore {
    match config_dir() {
        Some(dir) => CredentialStore::new_with_headless_fallback(DEFAULT_KEYRING_APP, dir),
        // No HOME at all: the sealed file has no durable home, so keep the
        // pre-M7 resolution (keyring, loud warnings on failure).
        None => {
            tracing::warn!("no config dir; the headless credential store is unavailable");
            CredentialStore::new(DEFAULT_KEYRING_APP)
        }
    }
}

/// Install the disk-backed nest-identity pin store at startup so TOFU pins
/// survive restarts (`docs/goal/architecture/security.md` § Transport trust). Call **once**, early — before the first authenticated connect —
/// so it replaces the process-global in-memory default (`MemoryPinStore`, which
/// evaporates on exit) before any handshake TOFU-pins a nest.
///
/// Mirrors linux's `client::install_disk_pin_store`: the canonical filename
/// (`cert_binding::NEST_IDENTITY_PIN_FILE`) and the install seam
/// (`DiskPinStore::open_in_dir` → `trust::install_pin_store`) are shared Rust, so
/// every native app's on-disk layout is identical. Without this the pin lives
/// in `MemoryPinStore` and is gone the moment the process exits, so the "warn
/// when a nest's identity changes across restarts" guarantee — the entire point
/// of the SSH-`known_hosts` pin — could never fire on tui (the pin never
/// survives to be violated).
///
/// The store lives at the **install-scoped** trust home
/// (`cert_binding::install_scoped_trust_home` — `$XDG_CONFIG_HOME/fauna` here,
/// the same dir linux writes and the child `fauna-sync-agent` reads), NEVER
/// tui's own `fauna-tui/` config dir: § Pin custody rule 1 scopes the pin
/// store to the *install*, and the per-app store tui used to write was one no
/// consumer ever read — so a tui-provisioned sync agent could never
/// authenticate a self-signed (TOFU-rooted) nest and looped `PinRequired`
/// forever. (The one-time adoption of pins pre-fix builds minted into the
/// per-app dir was retired by the compat-remnant sweep —
/// `version-compatibility.md` § Dimension 2, program 4.)
pub fn install_disk_pin_store() {
    let dir = fauna_client::cert_binding::install_scoped_trust_home();
    let _ = std::fs::create_dir_all(&dir);
    fauna_client::trust::install_pin_store(Arc::new(
        fauna_client::cert_binding::DiskPinStore::open_in_dir(&dir),
    ));
}

/// Install the process-global tracing subscriber — the shared `fauna_log` ring
/// (which powers Settings → Logs, `observability.md` § Surfaces) + a daily
/// rolling on-disk file + stderr — mirroring linux's `client::install_logging`.
///
/// The **stderr** layer is suppressed when stderr is the terminal: a live tui
/// owns the alternate screen, and log lines painted over it corrupt the display.
/// Under the e2e driver (stderr → `app.err`) or any `2>file` launch stderr is a
/// plain file and the one debugging surface a full-screen client has, so it is
/// kept there. The in-memory ring is installed **regardless** — it must always
/// hold the log for the Logs page. `RUST_LOG` filters as usual (default `info`).
///
/// Call **once**, before `ratatui::init()` (so the stderr decision is made
/// against the real terminal, before the alternate screen is entered). Mirrors
/// linux: the returned file-writer guard is `mem::forget`-ed to live for the
/// whole process, and a lifecycle line is logged so the ring — and each
/// launch's on-disk file — always has at least one entry (redaction rule: no
/// secrets / paths).
pub(crate) fn install_logging() {
    use std::io::IsTerminal;
    let to_stderr = !std::io::stderr().is_terminal();
    let dir = config_dir().unwrap_or_else(|| std::env::temp_dir().join("fauna-tui"));
    if let Some(guard) = fauna_log::init_with_stderr(&dir, to_stderr) {
        std::mem::forget(guard);
    }
    tracing::info!(target: "fauna_tui", "fauna-tui client logging initialised");
}

/// The tui app's per-app config dir (`$XDG_CONFIG_HOME/fauna-tui`, falling back
/// to `~/.config/fauna-tui`) — the persistent home for the nest-identity pin
/// store and the MLS engine's `mls_state.db`. Namespaced by app, like
/// [`DEFAULT_KEYRING_APP`], so it never contends
/// with the linux app's `~/.config/fauna` pin file on a box running both.
/// `None` only when neither `XDG_CONFIG_HOME` nor `HOME` is set. The e2e driver's
/// per-run `XDG_CONFIG_HOME` isolation (`tests/e2e-unified/drivers/tui.py`) makes
/// the store land in — and survive a `preserve_state_across_relaunch()` relaunch
/// under — the pinned XDG base for free.
pub(crate) fn config_dir() -> Option<std::path::PathBuf> {
    fauna_core::platform_ids::xdg_app_config_dir(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
        "fauna-tui",
    )
}

/// A registry view over the app's credential store. Cheap (both are stateless
/// views), so callers build one per operation, exactly as linux and web do.
pub(crate) fn registry(app: &App) -> AccountRegistry {
    fauna_credential_store::account_registry(Arc::clone(&app.credentials) as Arc<dyn SecretStore>)
}

/// THIS session's canonical mail address `<handle>@<domain>` — the SMTP
/// rail's RFC 5322 `From:` — or `None` when no domain is known yet.
///
/// Reads the actor id off the **held session** (`app.session`), never
/// `registry.active()` and never a caller-supplied id: on a bound launch (a
/// coexisting secondary instance) the bound account and the process-wide
/// active account can genuinely differ (`account-scoping.md` § Concurrent
/// instances — *Session identity resolves through the session's account*),
/// and resolving against `active()` — or against whatever id a call site
/// happens to pass — would seed the session's `From:`/MLS routing domain from
/// the WRONG account's cached handle/domain. Making this the ONE accessor, with no id parameter a call site
/// could get wrong, is what the goal doc means by "unrepresentable": there is
/// no longer a call site to mutate into reading the wrong account.
///
/// `None` when no session is installed, matching every other session-scoped
/// reader in this module.
///
/// `domain` is the **whoami/verify handle domain** (`AccountEntry.domain`,
/// populated by the launch machine's silent challenge or by
/// [`spawn_domain_refresh`]), NOT the mail-settings domain — they coincide in a
/// real deployment but can diverge, and a wrong domain silently sends
/// non-replyable mail (tracked internally). Mirrors linux's `ensure_smtp_backend`
/// (`<handle>@<domain>` from its account cache); tui reads the same shape from
/// the shared registry instead of a private cache.
pub fn session_self_address(app: &App) -> Option<String> {
    let session = app.session.as_ref()?;
    let entry = registry(app)
        .list()
        .into_iter()
        .find(|a| a.actor_id == session.actor_id)?;
    resolve_self_address(
        &session.handle,
        entry.handle.as_deref(),
        entry.domain.as_deref(),
    )
}

/// Pure resolution of `<handle>@<domain>` from the session handle + the registry
/// entry's cached handle/domain — factored out of [`session_self_address`] so the
/// branch table is unit-testable without building an `App`.
///
/// A handle that already carries an `@domain` **is** the address (the
/// onboarding-wizard path, where the entered handle is a full address).
/// Otherwise the address is the registry's cached handle at the registry's
/// cached domain — **both** must come from the nest.
///
/// The session handle is deliberately *not* a fallback for a missing registry
/// handle. The nest does sender-handle verification on every `fauna.email.send`
/// (`mail-app-surface.md` § First-party client send), so `<session-handle>@<domain>` for
/// an actor whose nest row has no handle is an address this account does not
/// own — the nest refuses it, and claiming it is what made every tui mail send
/// fail as `fauna.email.permission_denied` for an admin-admitted actor.
///
/// The "no handle yet" and "no handle at all" cases are distinguishable here
/// *because* the domain is required first: `entry_domain` is written only from
/// a nest ceremony's reply (`silent_refresh`, or the launch machine), and that
/// same reply carries the handle. So reaching the handle lookup at all proves
/// the nest has answered — an absent handle there is the nest's answer, not a
/// pending load.
fn resolve_self_address(
    session_handle: &str,
    entry_handle: Option<&str>,
    entry_domain: Option<&str>,
) -> Option<String> {
    if session_handle.contains('@') {
        return Some(session_handle.to_string());
    }
    let domain = entry_domain.map(str::trim).filter(|d| !d.is_empty())?;
    let handle = entry_handle.map(str::trim).filter(|h| !h.is_empty())?;
    Some(format!("{handle}@{domain}"))
}

/// Kick a best-effort background silent challenge and merge the nest's
/// handle/domain/tier into the account registry for `actor_id`. Empty fields from
/// the challenge preserve the existing cached values (never clear a good handle).
/// Fire-and-forget; a challenge failure leaves the cache untouched.
///
/// **This is also tui's post-auth identity re-check channel** (`security.md`
/// § Post-auth surfacing — channel 3 of the three structural re-check points).
/// The outcome therefore goes through the SHARED classifier rather than a local
/// `let Success(..) else { return }`: that shape swallowed every non-success
/// outcome, the `IdentityChanged` verdict included, which left a possible-MITM
/// session running silently — the pre-2026-07-23 behavior the § forbids.
fn spawn_domain_refresh(
    app: &App,
    nest_url: &str,
    secret: [u8; 32],
    actor_id: String,
    tx: &UnboundedSender<UiMessage>,
) {
    let fut = silent_refresh(
        registry(app),
        nest_url.to_string(),
        secret,
        actor_id,
        tx.clone(),
    );
    tokio::spawn(fut);
}

/// Run the post-auth silent challenge **to completion** over the active account
/// and apply its verdict — the awaited form of [`spawn_domain_refresh`], for the
/// `silent_sign_in` agent command.
///
/// Deliberately the *same* body as the production background refresh rather than
/// a test-only re-implementation: the e2e that drives this is proving the
/// production classify → escalate → re-enter path, so a shortcut that synthesized
/// the verdict would assert nothing about it (`security.md` § Post-auth
/// surfacing; the harness contract in `test_nest_identity_pin_post_auth.py`).
///
/// `Err` is the convention-11 loud-failure reason: no active account to
/// challenge with (pre-auth), or a malformed stored secret.
pub async fn run_silent_sign_in(app: &App, tx: &UnboundedSender<UiMessage>) -> Result<(), String> {
    let (nest_url, secret_hex, _) =
        stored_account(app).ok_or_else(|| "no active account".to_string())?;
    let nest_url = nest_url.ok_or_else(|| "active account has no nest url".to_string())?;
    // The actor id is derived from the same secret `stored_account` handed us,
    // via the shared `session_actor_id` seam — never from
    // `registry(app).active()`, which can name a different account on a
    // bound launch and would write a bound account's handle/domain/tier onto
    // the active account's registry row.
    let keypair = session_actor_id(&secret_hex)?;
    let actor_id = keypair.actor_id_hex();
    let secret = *keypair.secret_bytes();
    silent_refresh(registry(app), nest_url, secret, actor_id, tx.clone()).await;
    Ok(())
}

/// The one body behind both forms: challenge → shared classify → apply.
///
/// The message a stopped supervisor's reason escalates as, if it is a
/// session-ending verdict — the same three the background silent refresh
/// escalates, read through the one shared classifier
/// (`NestClientError::session_ending_verdict`). Any other stop (a clean close,
/// a version skew, a mint fault) stays the connection indicator's business.
fn session_ending_escalation(stop: &fauna_client::NestClientError) -> Option<DataMessage> {
    use fauna_client::SessionEndingVerdict as V;
    Some(match stop.session_ending_verdict()? {
        V::NestIdentityChanged => DataMessage::NestIdentityChanged,
        V::Superseded => DataMessage::IdentitySuperseded,
        V::SignInRefused => DataMessage::SignInRefused,
    })
}

/// The session-ending verdicts escalate — `IdentityChanged`, `Superseded`,
/// `NotRegistered`; every other failure class stays logged-and-swallowed,
/// because a background refresh must not tear down a healthy session over a
/// fault (`security.md` § Post-auth surfacing).
async fn silent_refresh(
    registry: fauna_client_accounts::AccountRegistry,
    nest_url: String,
    secret: [u8; 32],
    actor_id: String,
    tx: UnboundedSender<UiMessage>,
) {
    use fauna_launch_machine::{
        AuthConnector, SilentSignInVerdict, WsAuthConnector, classify_silent_challenge,
    };
    // No reach hint. The dial policy (domain first, hint only behind a
    // reachability failure) has exactly one copy, in
    // `LaunchMachine::run_silent_challenge_phase` (`onboarding.md` § Reach
    // hint's dial rule), and this is a background refresh of a session the
    // domain already answered for — a hint here would be a second, divergent
    // copy of a policy that exists to be shared.
    let (handle, domain, tier) = match classify_silent_challenge(
        WsAuthConnector
            .silent_challenge(&nest_url, None, &secret)
            .await,
    ) {
        SilentSignInVerdict::Refreshed {
            handle,
            domain,
            tier,
        } => (handle, domain, tier),
        SilentSignInVerdict::IdentityChanged => {
            let _ = tx.send(UiMessage::Data(DataMessage::NestIdentityChanged));
            return;
        }
        // Suspended or removed while signed in — this runs post-auth only, so
        // the refusal is never onboarding's "not yet". The third escalating
        // verdict, by the `Superseded` arm's reasoning below: the session is
        // already de-facto dead (`onboarding.md` § App-launch routing, the
        // previously-signed-in row, mid-session).
        SilentSignInVerdict::NotRegistered => {
            tracing::warn!("[session] this nest no longer signs this identity in");
            let _ = tx.send(UiMessage::Data(DataMessage::SignInRefused));
            return;
        }
        // The identity was succeeded while this session was running. The
        // second escalating verdict: the session is already de-facto dead —
        // every connection it opens from here is refused — so it goes to
        // the launch surface rather than a banner over a broken session.
        SilentSignInVerdict::Superseded { new_actor_id_hex } => {
            tracing::error!(
                "[session] this identity was succeeded (claimed successor \
                     {new_actor_id_hex}) — routing to the identity-import flow"
            );
            let _ = tx.send(UiMessage::Data(DataMessage::IdentitySuperseded));
            return;
        }
        // Locked while this session was running (`devices.md` § The locked
        // state). The fourth escalating verdict, by the `Superseded` arm's
        // reasoning: the lock revoked every bearer and the nest mints no new
        // one, so the session is already dead — it goes to the launch flow,
        // whose re-run challenge lands the locked surface on a live machine.
        SilentSignInVerdict::Locked { locked_until_secs } => {
            tracing::warn!("[session] this account is locked until {locked_until_secs}");
            let _ = tx.send(UiMessage::Data(DataMessage::AccountLocked));
            return;
        }
        SilentSignInVerdict::Failed { error } => {
            tracing::debug!("[session] background silent refresh failed: {error}");
            return;
        }
    };
    // Merge: a returned empty field keeps whatever the cache already holds
    // (the launch-machine populated it, or an earlier refresh did).
    let cur = registry.list().into_iter().find(|a| a.actor_id == actor_id);
    let handle = if handle.is_empty() {
        cur.as_ref().and_then(|a| a.handle.clone())
    } else {
        Some(handle)
    };
    let domain = if domain.is_empty() {
        cur.as_ref().and_then(|a| a.domain.clone())
    } else {
        Some(domain)
    };
    let tier = if tier.is_empty() {
        cur.as_ref().and_then(|a| a.tier.clone())
    } else {
        Some(tier)
    };
    let _ = registry.update_cache(
        &actor_id,
        handle.as_deref(),
        domain.as_deref(),
        tier.as_deref(),
    );
    // The address may just have become resolvable (or have changed — a
    // server-side handle rename rides this same refresh): tell the app loop to
    // push the new resolution into the conversations session's live cell
    // (`conversations.md` § State & data shape → *Self-address: live, never
    // baked*). Sent unconditionally — the handler re-resolves and no-ops when
    // nothing is resolvable yet.
    let _ = tx.send(UiMessage::Data(DataMessage::SelfAddressRefreshed));
}

/// An authenticated session: the identity + the live WS-RPC client.
pub struct Session {
    pub handle: String,
    pub actor_id: String,
    pub client: Arc<NestClient>,
}

/// Build the client for `(nest_url, secret_hex)`, spawn its connect + a
/// `ConnectionState` pump into the UI channel, and install the session on the
/// app. Errors only on a malformed secret.
///
/// When the launch machine reached `Online` it already holds a valid bearer, so
/// the WS handshake mints over [`LaunchMachineBearer`] rather than a private
/// `WsChallengeBearer` cache — one silent-challenge token, one TTL
/// pre-expiry refresh loop, one 401-reactive invalidation path, exactly as
/// `fauna-desktop` wires it (`apps/fauna-linux/src/client.rs`).
///
/// **The `Online` gate is load-bearing, not an optimization.** A machine that
/// never ran a successful silent challenge (the freshly-onboarded `adopt` path,
/// or the e2e session patch) sits in `WizardAt`/`Offline`, where
/// `LaunchMachineBearer::bearer()` cannot mint: `refresh_token` no-ops off
/// `Online`/`Refreshing` (`machine.rs:311`) and `retry_silent_challenge`
/// self-guards to `Offline { transient: true }`, so it would return `Err`
/// forever and the WS handshake would never authenticate. Off the `Online` path
/// the standalone `NestClient::new` shape (its own `WsChallengeBearer`) is the
/// only one that can bootstrap a token.
pub fn establish(
    app: &mut App,
    tx: &UnboundedSender<UiMessage>,
    nest_url: &str,
    secret_hex: &str,
    handle: String,
) -> Result<(), String> {
    let keypair = ActorKeypair::from_secret_hex(secret_hex).map_err(|e| {
        fauna_i18n::strings::onboarding::session_error::invalid_secret(&e.to_string())
    })?;
    let actor_id = keypair.actor_id_hex();
    // The typed form, read before the keypair moves into the client below: the
    // critical-alert sweep keys its alerts by actor (`critical-alerts.md`
    // § Mechanism — keys are identity-scoped).
    let actor = keypair.actor_id();
    // The (OS login, account) single-instance guard — become this account's
    // one instance BEFORE any of its scoped state opens below (the MLS db via
    // `account_scope::account_state_dir`, the device-local account store, the mail
    // epoch schedule). This is also tui's switch seam: `launch::start` re-runs
    // the whole routing on every account switch, so the shared holder reuses
    // on a same-account rebuild and swaps by replacement on a cross-account
    // one (`account-scoping.md` § Concurrent instances). A succession is the
    // one cross-account switch a BOUND instance makes by itself, and it passes
    // here because the binding followed the account in `adopt_successor`'s
    // `record_succession` — before `switch_account` re-entered this function.
    crate::account_scope::become_session_instance_or_exit(&actor_id);
    // A signed-in user is not the one a sign-out residue was reported to; its
    // record stays on disk for the next signed-out launch to re-check
    // (`account_scope::recheck_residue_at_launch`), but the view goes.
    app.sign_out_residue = None;
    // The `FeedManager` builds + signs posts itself, so it needs the raw signing
    // secret. Read it before the keypair moves into the client.
    let secret = *keypair.secret_bytes();
    let client = match online_launch_machine(app) {
        Some(machine) => {
            let bearer = Arc::new(fauna_nest_http::LaunchMachineBearer(machine));
            NestClient::with_auth(Arc::new(AuthClient::with_bearer_source(
                nest_url.to_string(),
                keypair,
                bearer,
                fauna_client::pinned_http_client(nest_url),
            )))
        }
        None => NestClient::new(nest_url.to_string(), keypair),
    };

    // The post-auth half of the aftermath ([`spawn_succession_aftermath`]).
    spawn_succession_aftermath(app, Arc::clone(&client), &actor_id, secret_hex, tx);

    // Learn a succession link this device's registry does not hold (it never
    // held the predecessor's row, or the user removed the retired account) —
    // the shared hop, best-effort and one read for an ordinary identity. After
    // the aftermath hook on purpose: that gate reads the registry as it stands
    // and a device with no predecessor material has no pass to run today; the
    // link this records is what the profile writers read at the next save, and
    // what the next sign-in's gate sees.
    if let Ok(link_keypair) = ActorKeypair::from_secret_hex(secret_hex) {
        let accounts = registry(app);
        let link_client = Arc::clone(&client);
        tokio::spawn(async move {
            fauna_client_recovery::ceremony::learn_succession_link(
                link_client,
                &accounts,
                &link_keypair,
            )
            .await;
        });
    }

    // Opportunistically refresh the published mail content-sealing epoch
    // schedule at this universal post-auth hook (encryption-at-rest.md
    // § Capability tiering → Content-sealing epochs), mirroring linux's
    // `refresh_mail_epoch_schedule` posture exactly. Best-effort/log-only;
    // a no-op when mail is disabled.
    crate::mail_glue::spawn_refresh_mail_epoch_schedule(
        Arc::clone(&client),
        secret_hex,
        app.settings.mail_store(),
        nest_url,
    );

    // The deployment-seed custody leg (box-recovery.md § The plane-era
    // recovery floor, (c) The writes) runs at whichever of this post-auth edge
    // and the store-ready edge lands second. The handle is normally still
    // `None` here (the assembly is spawned at the bottom of this function), in
    // which case `app.rs`'s `AccountStoreReady` arm runs it; a handle already
    // installed means this edge is the second one.
    if let Some(store) = app.settings.account_store.clone() {
        crate::recovery::spawn_custody_leg(Arc::clone(&client), store, tx.clone());
    }

    // Run the feeders that have no page of their own, at the same universal
    // post-auth hook (`critical-alerts.md` § Goal — a set-and-forget deployment
    // must still raise its banner). Today that is the pending-RecoveryKey-
    // replacement window: a 30-day window authorized by the identity seed alone
    // is exactly the "someone else holds my seed" case the banner exists for,
    // and until this call nothing polled it on any client.
    //
    // A fresh wake per identity, so the e2e poke can only ever reach THIS
    // session's loop (`critical_alerts::SweepWake`).
    app.alert_sweep_wake = crate::critical_alerts::SweepWake::default();
    crate::critical_alerts::spawn_session_start_sweep(
        Arc::clone(&client),
        // Feeder #1 reads the held rotation keyring from this session's
        // account runtime — the slot the assembly below fills.
        Arc::clone(&app.settings.account_runtime),
        Arc::clone(&app.alerts),
        actor,
        app.alert_sweep_wake.clone(),
    );

    // Pump the supervisor's ConnectionState watch into the UiMessage channel —
    // the linux WsEvent pump's shape (its client.rs:5035-5068), driving the
    // global `connection-status` element.
    //
    // A `Disconnected` is also where a supervisor that stopped for good says
    // why (`NestClient::supervisor_stop` is recorded before the state is
    // announced): a session-ending verdict — above all a post-4401 re-mint
    // the nest refused because the user was suspended — escalates to the launch
    // surface (`security.md` § Post-auth surfacing). Weak, so the pump never
    // keeps a signed-out client alive.
    let mut state_rx = client.connection_state();
    let stop_client = Arc::downgrade(&client);
    let pump_tx = tx.clone();
    tokio::spawn(async move {
        loop {
            let state = *state_rx.borrow();
            if pump_tx
                .send(UiMessage::Data(DataMessage::ConnectionState(state)))
                .is_err()
            {
                return; // UI gone — app is shutting down.
            }
            if state == fauna_client::ConnectionState::Disconnected
                && let Some(escalation) = stop_client
                    .upgrade()
                    .and_then(|c| c.supervisor_stop())
                    .and_then(|stop| session_ending_escalation(&stop))
            {
                let _ = pump_tx.send(UiMessage::Data(escalation));
                return; // The session ends here; the escalation tears it down.
            }
            if state_rx.changed().await.is_err() {
                return; // client dropped (sign-out).
            }
        }
    });

    // Reconnect pump. The shared `NestClient` bumps a counter on every reconnect
    // (a `Connected` after the first connect); each bump → one
    // `DataMessage::Reconnected`, which re-hydrates the visible snapshot surfaces
    // (the feed has no poll backstop). Distinct from the ConnectionState pump
    // above, which fires `Connected` on the *initial* connect too. `transport.md`
    // § Push events: observers re-pull through their snapshot-refresh path because
    // the push `seq` resets to 0 on reconnect. The linux twin is
    // `apps/fauna-linux/src/client.rs::start_ws_rpc` (b2).
    let mut reconn_rx = client.subscribe_reconnects();
    let reconn_tx = tx.clone();
    tokio::spawn(async move {
        while reconn_rx.changed().await.is_ok() {
            let _ = *reconn_rx.borrow_and_update();
            if reconn_tx
                .send(UiMessage::Data(DataMessage::Reconnected))
                .is_err()
            {
                return; // UI gone — app is shutting down.
            }
        }
    });

    // Push pump. `subscribe_pushes()` is a `broadcast::Receiver`; on `Lagged(n)`
    // the broker dropped the oldest events but the receiver stays usable (the nest
    // then emits `ResyncRequired`, which sweeps the snapshot set) — log + continue.
    // `Closed` fires only when every sender is dropped (the broker is `Arc`d
    // alongside the client), i.e. terminal shutdown. The linux twin is
    // `start_ws_rpc` (c).
    let mut push_rx = client.subscribe_pushes();
    let push_tx = tx.clone();
    tokio::spawn(async move {
        use tokio::sync::broadcast::error::RecvError;
        loop {
            match push_rx.recv().await {
                Ok(event) => {
                    if push_tx
                        .send(UiMessage::Data(DataMessage::Push(event)))
                        .is_err()
                    {
                        return; // UI gone.
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!(
                        "ws-rpc: push pump lagged, dropped {n} events \
                         (nest will emit ResyncRequired)"
                    );
                }
                Err(RecvError::Closed) => return,
            }
        }
    });

    let connect_client = Arc::clone(&client);
    tokio::spawn(async move {
        // The supervisor owns retries from here; failures surface through the
        // ConnectionState watch, never as a one-shot error dialog.
        let _supervisor = connect_client.connect().await;
    });

    // Refresh the account registry's cached handle/domain/tier from a background
    // silent challenge — linux's post-auth `silent_sign_in()` shape
    // (client.rs, `do_silent_sign_in`). In the online returning-user path the
    // shared launch machine has already populated these (via
    // `RegistryLaunchPersistence::save_authenticated`), but the e2e `set_state`
    // login (`apply_session_patch`) skips the challenge, so the domain would stay
    // unset. When the refresh lands it emits `DataMessage::SelfAddressRefreshed`,
    // whose handler pushes the resolved `<handle>@<domain>` into the
    // conversations session's live self-address cell — the client's outbound
    // `From:` and MLS routing domain (the whoami handle domain, NOT the
    // mail-settings domain; tracked internally).
    spawn_domain_refresh(app, nest_url, secret, actor_id.clone(), tx);

    // The HTTP/bulk plane for blob upload, built over the **same** bearer source
    // the WS client holds. `AuthClient::bearer()` hands back the `Arc<dyn
    // BearerSource>` it already uses, so this shares its one token mint, one TTL
    // refresh loop and one 401-reactive path rather than minting a parallel
    // token (the shape `fauna_nest_http::BearerSource`'s own docs prescribe).
    let content: Arc<dyn fauna_nest_http::NestContentApi> = Arc::new(client.auth().content_api());

    // The one post-auth hook: every path that produces a session — onboarding,
    // the launch router, the e2e patch — gets a feed manager here, so none of
    // them has to remember to wire one.
    // ⚠ Resolved HERE, above every consumer, and deliberately once: the same
    // retired-owner-key list feeds the Media page's read custody, the Settings
    // devices/folders label custody, the out-of-process sync agent (the byte
    // plane — `sync-agent.md` § Credential model) and the `__mls` replica
    // barrier below. Two resolutions of one fact is exactly the silent
    // divergence `AccountRegistry::predecessor_backup_keys` exists to prevent —
    // a plane that resolved it differently would read as corruption on that
    // plane alone. Empty for every identity that never succeeded, so the
    // overwhelmingly common path pays nothing.
    let succession_predecessors = succession_predecessor_backup_keys(app, &actor_id);
    // The same walk's ATTESTED identities — their ids are the R14 fleet view's
    // `prior` (`account-data-taxonomy.md` § The generation machinery → *The
    // source of `prior`*), their delegable schedules what the account
    // runtime's walk carries a predecessor's rows under
    // (`succession-aftermath.md` § Re-key scope): one fact, resolved beside
    // the keys it pairs with, handed to the account runtime below and — the
    // ids — to the sync agent (which holds no registry and cannot attest one
    // itself). Never a writer-asserted list.
    let attested_predecessors = fauna_client_account_runtime::AttestedPredecessors::from_registry(
        &registry(app),
        &actor_id,
    );
    // …and the keys paired with those identities, nearest hop first: the
    // per-signer bound offers a row signed as a predecessor only that
    // identity's root and its predecessors' (`mls-group-key-material.md`
    // § M2 → *Writer-signed change records*, ruling (8)(c)), so every plane
    // that opens such a row needs to know which key is whose.
    let predecessor_chain = registry(app).predecessor_backup_keys_by_actor(&actor_id);
    app.feed = crate::feed::init(
        Arc::clone(&client),
        Arc::clone(&content),
        secret,
        crate::settings::period_key_door(app.settings.account_runtime.clone()),
        std::sync::Arc::new(app.settings.preference_store()),
        tx,
        app.session_generation,
    );
    // Draft-persistence v2, posts rail (`reserved-folders.md` § Drafts Sync):
    // restore the user's half-written post from the `__drafts` reserved folder
    // on launch, and autosave (debounced) after each compose edit, so an unsent
    // post survives a restart and reaches the user's other devices — the exact
    // treatment the conversations rail already gets a few lines below.
    if let Some(manager) = app.feed.manager.clone() {
        app.feed.drafts_sync = crate::feed::drafts::start(
            manager,
            Arc::clone(&client),
            secret_hex,
            &succession_predecessors,
        );
    }
    // The Media page's machine rides the same hook. Its first `fauna.media.list`
    // pull is deliberately NOT kicked here — entering the tab is the trigger
    // (`media::nav_enter_op`, awaited on the nav edge), so a login never pays for
    // a page the user may not open.
    app.media = crate::media::init(
        Arc::clone(&client),
        secret,
        crate::settings::folder_key_door(app.settings.account_runtime.clone()),
        tx,
        &succession_predecessors,
        attested_predecessors.actor_ids(),
        &predecessor_chain,
        crate::settings::follows_door(app.settings.account_runtime.clone()),
    );
    // The A6 sync-agent control surface rides the same hook (`sync-agent.md`
    // § Control plane split). It provisions the external per-user
    // `fauna-sync-agent` — registers the `RenewBearer` device grant, starts the
    // convergence loop, and pushes the content-key blob so bound sets seal under
    // their M2 content key — and on a headless SSH box is what MINTS the
    // capability (tui holds identity via its sealed store). Under e2e it still
    // provisions, direct-child-spawning the agent into the launch's isolated
    // `XDG_RUNTIME_DIR` (Slice 4, the linux shape); no-op only on non-unix.
    // Unprovisioned on sign-out/switch/reset via `sign_out`, never on a plain quit.
    // Leg 5's surface context, resolved off the SAME registry walk the keys
    // above came from. Both halves are needed and neither is derivable from the
    // other: with no retired keys the re-seal pass returns before touching its
    // DB, so a successor's device that holds no predecessor secret records
    // nothing at all — indistinguishable, from the engine's side, from an
    // identity that never succeeded. Only `predecessors_of` separates them, and
    // that separation is the whole "another device owes this" line.
    let succession_corpus = fauna_client_sync::agent::SuccessionCorpusContext {
        succeeded: !registry(app).predecessors_of(&actor_id).is_empty(),
        holds_predecessor_material: !succession_predecessors.is_empty(),
    };
    app.sync_agent = crate::sync_agent::init(
        Arc::clone(&client),
        secret,
        nest_url,
        tx,
        &succession_predecessors,
        attested_predecessors.actor_ids(),
        &predecessor_chain,
        succession_corpus,
    );
    // The conversations manager rides the same hook, then the real
    // `ConversationsSession` (MLS engine + `NestConversationsRpc` + the unified
    // receive loop) is wired over it — the tui twin of linux's AuthSuccess
    // `start_conversations_session`. In e2e the real FaunaMls + SMTP
    // registrations land over the mock rail entries `init` just installed,
    // exactly as linux orders it.
    app.conversations = crate::conversations::init(tx);
    // Install the session identity NOW, ahead of the self-address seed below,
    // so `session_self_address` can read it as the single accessor
    // (`account-scoping.md` § Concurrent instances) instead of this call site
    // choosing an id. Every field is cloned in — `client`/`handle`/`actor_id`
    // stay live locals for the rest of this function (the account-runtime
    // block below, `app.events`'s `&handle`, …) — and the final move into
    // `app.session` at the end of this function is gone; this is the one
    // write.
    app.session = Some(Session {
        handle: handle.clone(),
        actor_id: actor_id.clone(),
        client: Arc::clone(&client),
    });
    // Seed the session's live self-address cell with the best resolution now —
    // registry handle@domain when the launch machine already landed them, the
    // full-address session handle on the onboarding path, else empty until the
    // background refresh above pushes it (`conversations.md` § State & data
    // shape → *Self-address: live, never baked*).
    let self_address = session_self_address(app).unwrap_or_default();
    // ⚠ Resolved BEFORE the plane is built, and passed in rather than fetched
    // later: the `__mls` re-seal is a barrier inside the replica's own `load()`,
    // not a hook racing it (`MlsStateSync::with_predecessors`). A restore that
    // read the still-predecessor-sealed replica would classify the unseal
    // failure as *permanent* and leave a successor's conversations dark for the
    // whole session.
    let mls_predecessors = succession_predecessors.clone();
    // The account-custody ceremony sink captures through the account store,
    // which lands after login: it resolves this slot per payload.
    let runtime = app.settings.account_runtime.clone();
    let mail_store = app.settings.mail_store();
    crate::conversations::conv_backend::start_conversations_session(
        &mut app.conversations,
        Arc::clone(&client),
        secret_hex,
        mail_store,
        self_address,
        tx,
        mls_predecessors,
        attested_predecessors
            .actor_ids()
            .iter()
            .map(|id| id.0)
            .collect(),
        runtime,
    );
    // Surface the standing conversations-engine-role refusal (if any) NOW:
    // an engine-less session may never emit the `ConversationsChanged` tick
    // that otherwise bridges it onto `error-message`, and a refusal that only
    // ever reached a log would read as a dropped page (convention 11).
    crate::conversations::sync_page_error(app);
    // Draft-persistence v2 (`reserved-folders.md` § Drafts Sync): restore the
    // user's conversation compose drafts from the `__drafts` reserved folder
    // on launch, and autosave (debounced) after each compose edit, so an unsent
    // message survives a restart and reaches the user's other devices.
    //
    // **Outside the conversations session on purpose**, exactly as linux orders
    // it (`app.rs` AuthSuccess): drafts seal under the owner's `BackupKey` and
    // need no MLS, so neither of `start_conversations_session`'s non-fatal
    // early-returns (no config dir, failed engine init) may also cost the user
    // their unsent compose.
    if let Some(manager) = app.conversations.manager.clone() {
        app.conversations.drafts_sync = crate::conversations::drafts::start(
            manager,
            Arc::clone(&client),
            secret_hex,
            &succession_predecessors,
        );
    }
    // Contacts rides the same hook: a transport-only page (no shared manager —
    // `contacts.md` § Where logic lives), hydrated here and refetched on every
    // nav to its tab.
    app.contacts = crate::contacts::init(
        Arc::clone(&client),
        nest_url,
        secret,
        app.settings.mail_store(),
        tx,
        app.session_generation,
    );
    app.notifications = crate::notifications::init(Arc::clone(&client), tx, app.session_generation);
    app.moderation = crate::moderation::init(Arc::clone(&client));
    app.report = crate::report::init(Arc::clone(&client));
    // The Nostr page rides the same hook but deliberately does NOT fetch here —
    // entering the tab is the trigger (`nostr::nav_enter_op`, awaited on the nav
    // edge), the same posture Media takes, so a login never pays for a page the
    // user may not open. The succession-aftermath npub check (`nostr.md` § Key
    // succession and rotation) reads the account plane through
    // `settings.account_store`, taken when each op is built.
    app.nostr = crate::nostr::init(Arc::clone(&client));
    // The co-present offline-share affordance's identity half (`p2p.md`
    // § Offline share initiation). Same no-work-here posture, one step further:
    // it does not even prepare a fetch. All this does is resolve the actor id
    // whose hex IS the compare code, which is what makes the two entry buttons
    // render. Nothing listens yet: the state carries this sign-in's empty
    // `SessionSeat`, which binds when the user opens a panel or the share
    // plane first has a set to serve — whichever comes first — and the other
    // door is handed that one seat (`offline_share::SessionSeat`).
    #[cfg(feature = "p2p-share")]
    {
        app.settings.offline_share = crate::offline_share::init(secret_hex);
    }
    // The unified Bridges page rides the same hook and the same no-fetch-here
    // posture as Nostr — entering the tab is the trigger (`bridges::nav_enter_op`,
    // awaited on the nav edge), so a login never pays for a page the user may
    // not open.
    app.bridges = crate::bridges::init(Arc::clone(&client));
    // The Backups page's destination surface rides the same hook and the same
    // no-fetch-here posture — entering the tab is the trigger
    // (`backups::nav_enter_op`). It takes the owner secret as well as the
    // client: the `NestBackupKey` grant and the cross-nest resolve derive from
    // that one seed. The destination list itself rests on the account plane
    // (`fauna.state.backup`), reached through the settings-owned runtime slot.
    app.backups = crate::backups::init(
        Arc::clone(&client),
        secret,
        crate::settings::folder_key_door(app.settings.account_runtime.clone()),
        crate::settings::nests::backup_door(app.settings.account_runtime.clone()),
        &succession_predecessors,
    );
    // Profile rides the same hook — no observer manager (`ui/profile.md`
    // § Persistence); it stores the identity inputs (own actor_id + cached
    // handle for the SELF header fallback) and re-fetches on every open.
    app.profile = crate::profile::init(
        Arc::clone(&client),
        Arc::clone(&content),
        fauna_core::secret::SecretArray32::from(secret),
        crate::settings::period_key_door(app.settings.account_runtime.clone()),
        actor_id.clone(),
        handle.clone(),
        nest_url.to_string(),
    );
    // The author-side reconcile loop (`monetization.md` § Pillar 1 grant path 2):
    // heals crash-staged subscriber removals and drains `auto_approve` follows,
    // which the nest cannot grant itself in encrypted mode. Same shared
    // orchestration linux runs (`subscriptions_author.rs`).
    crate::subscriptions_author::start(
        Arc::clone(&client),
        secret,
        crate::settings::period_key_door(app.settings.account_runtime.clone()),
    );
    // Events rides the same hook — transport-only over `CalDavClient` (no
    // shared manager), hydrated here and refetched on every nav to its tab.
    app.events = crate::events::init(
        Arc::clone(&client),
        secret_hex,
        &handle,
        app.settings.mail_store(),
    );
    // Draft-persistence v2, events rail (`reserved-folders.md` § Drafts Sync) —
    // the third and last rail of the wire's closed enumeration, and tui is its
    // first app leg. Restores the user's half-written event from `__drafts` on
    // launch and autosaves (debounced) after each compose edit, so an unsent
    // event survives a restart and reaches the user's other devices, exactly as
    // the posts and conversations rails already do above.
    (app.events.drafts_tx, app.events.drafts_sync) = crate::events::drafts::start(
        Arc::clone(&client),
        secret_hex,
        &succession_predecessors,
        // The identity seam the launch restore travels under: nothing cancels
        // that nest round-trip, and the outcome channel is process-wide, so a
        // load outliving an account switch must be dropped rather than painted
        // into the incoming actor's compose buffers.
        app.session_generation,
        tx,
    );
    crate::events::spawn_refresh(
        &app.events,
        app.settings.account_store.clone(),
        tx,
        app.session_generation,
    );
    // Search rides the same hook — the shared `SearchManager` (no prefetch:
    // there is no query until the user submits one). The observer it attaches
    // is what paints the in-flight state, so it needs the UI channel.
    app.search = crate::search::init(Arc::clone(&client), tx);
    // Backend 2's arm — the sealed local index — attached once both halves
    // exist: the manager above, and the conversations session's index launcher
    // (the one holder of the MSEK that can open it). Async and fire-and-forget;
    // the page is nest-only until it lands, which is a normal rendered state.
    crate::search::attach_local_index(&app.search, &app.conversations);
    // The posts trickle chokepoint (`content-index.md` § Ingest triggers, v1 —
    // the posts ruling): a nest-confirmed compose is staged into the local
    // index at once, instead of at the next reconcile walk. Wired here because
    // this is where the two owners meet — the feed manager owns the create
    // flows, the launcher owns the builder.
    if let (Some(feed), Some(launcher)) = (
        app.feed.manager.as_ref(),
        app.conversations.index_launcher.as_ref(),
    ) {
        feed.set_post_index_observer(launcher.own_post_observer());
    }
    // The room-post seam (`ui/feed.md` § Encryption at rest → *Room-restricted
    // — the ruling*): the feed opens and seals room-restricted posts through
    // the conversations session, which alone holds a room's keys and knows
    // which rooms there are. Without it every room post stays locked and no
    // room is offered as an audience — the honest state for a device with no
    // conversations plane. Wired here for the same reason as the observer
    // above: this is where the two owners meet.
    if let (Some(feed), Some(session)) = (
        app.feed.manager.as_ref(),
        app.conversations.real_session.as_ref(),
    ) {
        feed.set_room_post_keys(session.clone());
    }
    // The admin shell rides the same hook. Reset the gate to fail-closed FIRST,
    // then `init` fires the `fauna.account.am_i_admin` check whose result reveals
    // the gated `admin-tab` row — so during the check window (and for a re-auth
    // as a non-admin) the row stays hidden. The dashboard itself is nav-edge
    // loaded, not prefetched here (`crate::admin` module docs).
    app.am_i_admin = false;
    // `nest_url` + `secret_hex` build the shared `AdminNatModeMachine` (which takes
    // a url + secret pair, not a `NestClient`) and back the admin-nest factory-reset.
    app.admin = crate::admin::init(
        Arc::clone(&client),
        nest_url,
        secret_hex,
        app.settings.account_runtime_source(),
        tx,
        app.session_generation,
    );
    // The gated Family page rides the same hook, with the same fail-closed
    // ordering as the admin gate: reset the gate FIRST, then fire the one
    // post-auth `fauna.family.status` read whose result reveals the `family-tab`
    // row and the global `supervised-indicator`. Unlike the ungated pages this
    // read is NOT deferred to the nav edge — the read IS the gate, so the row
    // could never appear if it waited for the user to open the tab
    // (`family-safety.md` § App surface: "driven by fauna.family.status read
    // at login"); the nav edge re-reads it on top.
    app.has_family = false;
    app.family = crate::family::init(Arc::clone(&client));
    // Clause 2 of the unfetched-policy ruling (`family-safety.md` § Content
    // policy): restore the last-known supervision snapshot BEFORE firing the
    // read below. The fail-closed reset above is the right default only for an
    // account nothing is known about; for one whose last successful read saw a
    // guardian, "unknown" and "unsupervised" are different facts, and rendering
    // the second while waiting for the network is what made airplane mode a
    // bedtime-lock bypass. Restores nothing when the slot is absent (a device
    // that has never completed one successful read — clause 3's declared
    // residual), so the ordering here is free for every unsupervised user.
    crate::family::restore_supervision_snapshot(app, &actor_id);
    crate::family::spawn_status_check(&app.family, tx, app.session_generation);
    // The once-per-sign-in look for a newer version (`installers/README.md`
    // § Knowing a newer version is out): one unasked round trip through the
    // shared `fauna_client::update_look`, painting the About block's notice
    // only when a newer release is out. A failed look is silent; sign-out
    // resets the block, so the next sign-in looks again.
    app.spawn_page_op(crate::app::PageOp::Settings(
        crate::settings::Op::LookForUpdatesAtSignIn,
    ));
    // The content-policy engine's two halves are hydrated by two reads, both
    // fired here rather than on a nav edge because they gate a *render* on the
    // feed and conversations pages, not a page's own data (`family-safety.md`
    // § Content policy). The guardian floor rides the `fauna.family.status` read
    // above; this is the every-user half — the viewer's own spam/phishing
    // thresholds, which apply supervised or not.
    crate::content_policy::spawn_spam_preferences_check(Arc::clone(&client), tx);
    // The third source — the region content policies on the declared chain —
    // refreshes through this nest's relay (`region-blocking.md` § How an app
    // obtains its region's policy). The device record already armed the engine
    // at start; this only ever replaces it with a newer verified document.
    crate::region::attach_session(app, Arc::clone(&client), tx);
    // Settings rides the same hook — the Status account/quota surface + the
    // quota/handle-change RPC seam (`ui/settings.md`). Unlike the others this
    // AUGMENTS the pre-loaded client-local prefs (`SettingsState::load`) rather
    // than replacing them, then kicks the initial quota fetch.
    let ledger = ledger_seam(app);
    app.settings.attach_session(
        Arc::clone(&client),
        actor_id.clone(),
        handle.clone(),
        secret_hex.to_string(),
        nest_url,
        tx,
        Arc::clone(&app.alerts),
        &succession_predecessors,
        app.session_generation,
        ledger,
    );
    // The B3 member-row join-filter, wired here rather than inside
    // `attach_session` because the live `ConversationsSession` lives on
    // `App::conversations` (started above) and `SettingsState` cannot reach it.
    // Without it `DevicesMachine` stays fail-safe and drops EVERY `role ==
    // "member"` row, so a set shared WITH this user never appears on the
    // Folders page (`settings::devices::TuiMlsQuery`).
    app.settings
        .wire_folder_join_filter(app.conversations.real_session.as_ref());
    // The reporter-side hide list (`moderation.md` § Corollary) — a render
    // source on the feed and the conversations, so read eagerly here like the
    // spam thresholds above; after `attach_session` because the read needs the
    // account secret it installs. Re-read when the account store lands
    // (`AccountStoreReady`), since that rail may hold hides the blob does not.
    crate::report::spawn_load_hidden(app);

    // The W3 (account-data-plane.md § Workstreams) account-store lifecycle rides the same hook (`account-data-plane.md`
    // § The account store → *The client-side lifecycle*): assemble the runtime —
    // open the per-actor store under the UNIFIED per-user root
    // (`StoreRoot::platform()`, W6 path unification — the same root every
    // app and the sync agent resolve, which is what keeps this machine on
    // one journal per account),
    // mint-or-load the writer key from the T10 slot, start the pump — and land
    // the handle on Settings via `AccountStoreReady` (which guards against a
    // sign-out/switch racing the assembly). Spawned because assembly does real
    // work (store open, credential slot), and best-effort by construction: on
    // any failure every store-backed surface fails its gesture and the sign-in
    // stands. Teardown lives in `sign_out` (deterministic `shutdown`);
    // a plain quit drops the last handle clone — the ruling's drop-path
    // teardown. The pump's nudge feed is `app.rs`'s scope-tagged
    // `SyncChanged` arm; in-app pumping is arbitrated by W5.1's `engine.lock`
    // election, taken inside `AccountStoreRuntime::start` — so this runtime
    // comes up beside the always-on agent host (and beside any other app that
    // hosts one) as a plain reader/writer whenever it loses, and picks the
    // pump role up on its next backstop tick when the holder exits.
    if let Ok(keypair) = ActorKeypair::from_secret_hex(secret_hex) {
        // The member half of the content-scope set: this account's joined
        // conversation channels, read off the live MLS session once per pump
        // pass (`fauna_sync_engine::scope_set`). Local state, so it works with
        // no nest; a join or leave needs no notification path, since the next
        // pass simply derives a different set. `None` when no session is
        // installed — "cannot tell right now", which the runtime holds the last
        // set through rather than reading as "left every channel".
        let conversations = app.conversations.real_session.clone();
        let memberships: fauna_sync_engine::account_runtime::MembershipSource =
            Arc::new(move || {
                conversations
                    .as_ref()
                    .map(|session| session.joined_conv_channels())
            });
        // The assembly itself is shared — `fauna-client-account-runtime`, one
        // implementation of the eleven non-app params for every native app
        // (priority #2; linux is the other consumer). Its two phases are what
        // keep the I/O off the login path: `build_params` is pure struct
        // construction, and `resolve_and_start` carries the writer-key resolve,
        // this device's id, the co-located agent's enrollment-target probe and
        // the store open.
        let params = fauna_client_account_runtime::build_params(
            fauna_client_account_runtime::AppRuntimeInputs {
                actor_id_hex: actor_id.clone(),
                // This identity with the seeds of the predecessors the same
                // registry attests — the kept wrap's recovery
                // (`SeedHolder`).
                principal: fauna_client_account_runtime::SeedHolder::from_registry(
                    keypair,
                    &registry(app),
                ),
                // The R14 `prior`: the post-auth hook's one registry walk,
                // shared with the sync agent above (the field's docs own why
                // it is never a writer-asserted list).
                attested_predecessors: attested_predecessors.clone(),
                memberships: Some(memberships),
                // The peer-leg transport factory (W5.7 — tui's first
                // `fauna-iroh` dep, lead-app-first). The runtime invokes it only
                // as the elected engine-singleton with every gate open
                // (enrollment witness in the slot, `peer-sync` brake advertised
                // — `fauna_sync_engine::peer_leg`), handing over its own
                // resolved writer key: the endpoint's secret IS the machine's
                // device principal (R5 (account-data-plane.md § The ratified decisions) — NodeId = writer key), so the QUIC
                // handshake proves the same identity the admission witness
                // names. The relay URL rides the inputs (own-nest
                // provenance, the runtime's node-info read): attached to the
                // builder it makes the relay *available* to the dial cascade and
                // the endpoint reachable at the URL its published facts
                // advertise. The other apps pass `None` until they take the dep.
                peer_transport: Some(std::sync::Arc::new(
                    |inputs: fauna_sync_engine::account_runtime::PeerLegFactoryInputs| {
                        Box::pin(async move {
                            let (transport, bound_addrs) = fauna_iroh::peer_leg_transport(
                                inputs.writer_key.to_bytes(),
                                inputs.relay_url.as_deref(),
                            )
                            .await?;
                            Ok(fauna_sync_engine::account_runtime::PeerLegBinding {
                                transport,
                                bound_addrs,
                                file_sync: None,
                            })
                        })
                    },
                )),
                // Desktop: the per-OS constant. Only a sandboxed mobile shell
                // supplies its own container.
                store_container: None,
            },
            Arc::clone(&client),
            nest_url,
        );
        let params = fauna_client_account_runtime::with_session_wakes(
            params,
            client.subscribe_reconnects(),
            client.subscribe_pushes(),
        );
        let ready_tx = tx.clone();
        let ready_actor = actor_id.clone();
        let ctx = fauna_client_account_runtime::ResolveContext {
            nest_url: nest_url.to_string(),
            // A `device.db` open, so it is resolved inside phase 2 rather than
            // here — the same reason the writer-key resolve is. THIS account's
            // id, the one its sync agent registers under: a machine-wide read
            // would point the enrollment and the this-device marker at another
            // account's row.
            own_device_id_hex: {
                let actor = actor_id.clone();
                Box::new(move || crate::media::device_id_hex(&actor))
            },
        };
        // The teardown's view of this assembly, published the instant it
        // settles. `AccountStoreReady` alone is not enough: it rides the UI
        // queue, so between `resolve_and_start` returning and that message
        // being processed the store is OPEN on its own OS thread while
        // `settings.account_store` still reads `None` — and a `sign_out` in
        // that window stopped nothing and erased anyway (the field's own
        // docs carry the measurement). This channel is what makes the handle
        // reachable to a teardown without draining the UI queue; a failed
        // assembly sends `None`, so the wait ends at once rather than at the
        // budget.
        //
        // The seam itself is SHARED — `fauna_client_account_runtime
        // ::assembly_channel`, the same one `AccountRuntimeHost` registers for
        // linux and the FFI seat — so the three hosts cannot drift on a wait
        // whose absence is silent on POSIX. What stays tui's own is where the
        // receiver lives: `App` state, not a process static (the goal doc's
        // ⚠ *tui is a deliberate NON-consumer of the lifecycle* note).
        let (settle, pending) = fauna_client_account_runtime::assembly_channel();
        app.settings.account_store_assembly = Some(pending);
        // This session's fresh-read slot, filled here the instant the assembly
        // settles rather than when `AccountStoreReady` crosses the UI queue: a
        // seam waiting on the handle (the DNS record's store) may itself be
        // awaited ON that queue — the automation agent awaits a click's page
        // op — and would otherwise wait on a message it is blocking. A sign-out
        // swaps in a fresh slot (`SettingsState::set_account_store`), so a late
        // assembly of a retired session lands only in this orphaned one.
        let runtime_slot = Arc::clone(&app.settings.account_runtime);
        tokio::spawn(async move {
            match fauna_client_account_runtime::resolve_and_start(params, ctx).await {
                Ok(handle) => {
                    // The teardown's clone FIRST: it is the one that must
                    // never be missed, and a dropped receiver (no teardown
                    // pending) makes this a cheap no-op.
                    settle.started(&handle);
                    if let Ok(mut slot) = runtime_slot.lock() {
                        *slot = Some(handle.clone());
                    }
                    let _ = ready_tx.send(UiMessage::Data(DataMessage::AccountStoreReady {
                        actor_id: ready_actor,
                        handle,
                    }));
                }
                Err(e) => {
                    settle.failed();
                    tracing::warn!(
                        "account runtime: assembly failed; every store-backed surface \
                         answers not-running: {e:#}"
                    );
                }
            }
        });
    }

    discharge_succession_obligations(app);
    Ok(())
}

/// Post a payload-free [`DataMessage::AccountStoreChanged`] whenever the
/// account store may have changed — the one shared watch
/// (`fauna_client_account_runtime::store_change`; `account-runtime.md`
/// § Multi-instance concurrency → *A runtime's own pump is a source of
/// the notice too*): this runtime's own pump changing an entry a read can
/// answer (its change generation), or another connection committing to the
/// shared store (the `data_version` floor — a sibling same-account process,
/// or the always-on agent host). A gesture's own write fires neither: the
/// page that made it repaints from its own answer.
///
/// Exits when the runtime is gone — sign-out's deterministic `shutdown()`
/// makes the floor's read err, and a plain quit takes the whole process — so
/// the handle clone held here never keeps a runtime alive that the app has
/// let go of.
pub(crate) async fn account_store_watch(
    handle: fauna_sync_engine::account_runtime::AccountStoreHandle,
    tx: UnboundedSender<UiMessage>,
) {
    let mut watch = fauna_client_account_runtime::store_change::StoreChangeWatch::new(handle).await;
    while watch.changed().await {
        if tx
            .send(UiMessage::Data(DataMessage::AccountStoreChanged))
            .is_err()
        {
            break; // main loop gone — the app is exiting
        }
    }
}

/// Mint and show the successor's recovery kit — the closing act of an identity
/// succession, run on the *successor's* first authenticated session
/// (`identity-succession.md` § The RecoveryKey → *At succession*: "the successor
/// identity mints a fresh RecoveryKey — a new kit is part of the ceremony").
///
/// Nothing happens unless [`App::succession_kit_owed`] is set, so an ordinary
/// login pays one boolean check. See that field for why the obligation has to
/// cross the account switch, and why a *silent* background mint is not an option.
///
/// **Ordering, both halves load-bearing.** The navigation is set synchronously
/// **before** the op is spawned: `Action::OpenAccount` clears any kit on screen
/// (the shown-once custody rule), so navigating after the mint landed would wipe
/// the very thing this exists to show. And the op is spawned **after**
/// `attach_session` above, because the minted-kit fold reads
/// `settings.{account_actor_id, handle, node_url}` to build the `fauna://recovery`
/// payload — spawning earlier would mint a kit whose URI names no account, which
/// is exactly the impoverished form the 2026-08-02 copy-button ruling retired.
///
/// Best-effort by construction: with no `nest` yet there is nothing to mint
/// against, and the flag stays set so the next authenticated session retries
/// rather than the obligation being silently dropped.
///
/// The kit half of [`take_succession_obligations`]: consumes the flag and
/// navigates, returning the mint's op for the caller to spawn.
fn take_owed_kit(app: &mut App) -> Option<crate::settings::Op> {
    if !app.succession_kit_owed {
        return None;
    }
    let op = succession_kit_discharge_op(app)?;
    app.succession_kit_owed = false;
    app.succession_predecessor = None;
    // Land the user on the section that renders it. The successor is
    // `NeverCreated` by construction, but `create_kit` reads the chain head and
    // picks its own arm, so this stays correct even if a kit somehow exists.
    app.page = crate::pages::Page::Settings;
    app.settings.sub = crate::settings::SubPage::Account;
    Some(op)
}

/// Discharge every obligation a succession left the successor's first
/// authenticated session — run by the post-auth hook, after `attach_session`.
/// Spawns [`take_succession_obligations`]'s ops in its order.
pub(crate) fn discharge_succession_obligations(app: &mut App) {
    for op in take_succession_obligations(app) {
        app.spawn_page_op(crate::app::PageOp::Settings(op));
    }
}

/// The successor's owed ops, in the ceremony's own order — **the group sweep,
/// then the kit** (`succession-propagation.md` § Propagation → *Own device
/// fleet*, the relaunch-adoption clause). Split from the spawn so the order and
/// the payload are testable; each flag is consumed only when its op is built,
/// so with no nest yet both stay set and the next authenticated session retries.
///
/// The owed sweep is set only by a relaunch adoption ([`App::succession_sweep_owed`]),
/// and its op is the retry button's own, marked `owed` so the answer parks a
/// report whatever it is. It carries the LIVE successor engine: conversations
/// are started earlier in this same hook, so the retry never opens a second
/// engine over the store they hold. It is NEVER the ceremony's pre-switch
/// `sweep_after_succession` — no old engine exists at relaunch; the retry
/// rebuilds the sweep off the retired identity's seed and store, which survive
/// on the device that ran the ceremony.
pub(crate) fn take_succession_obligations(app: &mut App) -> Vec<crate::settings::Op> {
    let mut ops = Vec::new();
    if app.succession_sweep_owed {
        let old_secret_hex =
            crate::settings::sweep_retry_predecessor(app).and_then(|(_old_hex, seed)| seed);
        if let Some(op) = crate::settings::sweep_retry_op(app, old_secret_hex, true) {
            app.succession_sweep_owed = false;
            ops.push(op);
        }
    }
    ops.extend(take_owed_kit(app));
    ops
}

/// Persist the predecessor identities a phrase-only restore recovered from the
/// escrow blob's additive section (`identity-succession.md` § Seed escrow).
///
/// **This is the restore's half of the device-loss race.** The successor's kit
/// sealed these seeds precisely so a user who lost every device could get them
/// back; recovering them and then dropping them on the floor would make that
/// seal pointless. Written as ordinary registry rows because that is exactly
/// the state the ceremony's own device is left in — [`adopt_successor`]
/// deliberately keeps the retired account's row, and this reconstructs it.
///
/// **Ordering:** call *after* the restored identity is added, never before.
/// `add_account` claims `active` when no account holds it, so persisting a
/// predecessor first would land the app on an identity the nest refuses.
///
/// Best-effort per row: a failure is logged and the rest still land. The
/// account is already back by this point, so turning a predecessor's bad row
/// into a failed sign-in would trade a partial recovery for none at all.
pub fn persist_restored_predecessors(
    app: &App,
    predecessors: &[fauna_onboarding_machine::nest_api::RestoredPredecessorSeed],
) {
    if predecessors.is_empty() {
        return;
    }
    // The per-row add + succession link + logging is the registry's own
    // (`AccountRegistry::persist_restored_predecessors`), shared with linux.
    // The identity these are predecessors *of* — the restored one, which this
    // function's ordering contract guarantees is already added and active.
    let registry = registry(app);
    registry.persist_restored_predecessors(
        registry.active().as_deref(),
        predecessors
            .iter()
            .map(|p| (p.seed_hex.as_str(), p.actor_id_hex.as_str())),
    );
}

/// The op [`discharge_succession_obligations`] spawns — split out from the spawn so a
/// test can assert on the **payload**, not just on the navigation the discharge
/// performs. That split is the point: the predecessor seeds are the whole
/// reason this op differs from the ordinary create gesture, and a discharge
/// that navigated correctly while sealing nothing would look identical from the
/// outside.
pub(crate) fn succession_kit_discharge_op(app: &App) -> Option<crate::settings::Op> {
    let profile_predecessors =
        fauna_core::identity::ActorKeypair::from_secret_hex(app.settings.active_secret_hex())
            .map(|kp| profile_predecessors(app, &kp.actor_id_hex()))
            .unwrap_or_default();
    crate::settings::succession_kit_op(
        &app.settings,
        succession_predecessor_seeds(app),
        profile_predecessors,
    )
}

/// The people this session's group sweep could vouch for **nothing** about —
/// the roster [`fauna_client_config::raise_succession_member_reviews`] turns
/// into durable review items (`identity-succession.md` § Propagation → *MLS
/// groups*).
///
/// Reads the sweep's own report rather than re-deriving the roster from the
/// successor's engine, and that is the whole point: the report is the roster
/// **as it stood across the compromise window**, while a later re-derivation
/// would flag people who joined afterwards and miss people who have since left.
/// It is a fact about a moment, and the ceremony is the only witness to it.
///
/// Empty for every session that did not just sweep — an ordinary sign-in, a
/// device the user succeeded elsewhere on, a ceremony whose conversations were
/// never up ([`crate::settings::SweepStatus::NoEngine`]). The failed arm is
/// empty too: nothing was attempted, so nothing was observed.
pub(crate) fn succession_review_roster(app: &App) -> Vec<fauna_core::identity::ActorId> {
    // The per-arm answer lives on the shared status
    // ([`SweepStatus::review_roster`]), so tui and web read one copy of it
    // rather than each spelling out which arms report nobody. What is tui's own
    // is only *where the status is held*: `App::succession_sweep`, declared to
    // survive the account switch.
    app.succession_sweep
        .as_ref()
        .map(crate::settings::SweepStatus::review_roster)
        .unwrap_or_default()
}

/// The predecessor seed(s) the owed kit must seal into its escrow blob — the
/// ephemeral in-flight hop unioned with the full registry chain
/// (`identity-succession.md` § Seed escrow — "the successor's blob carries the
/// predecessor seed(s)", every ancestor this device holds).
///
/// **Empty is a legitimate answer, never an error to raise.** The registry row
/// can be gone (a factory reset, a user who removed the old account, or a
/// successor signing in on a device that never held the predecessor), and the
/// kit ceremony must still run: a kit with no predecessor section is strictly
/// better than no kit at all, which is what refusing here would produce. What
/// is lost is only the device-loss backstop for a corpus this device could not
/// have re-sealed anyway.
///
/// The **whole chain**, not one hop: the previous kit's blob is deleted in the
/// succession transaction (§ Seed escrow — "the row dies with the key it is
/// sealed to"), so nothing carries a grandparent's seed forward except this
/// ceremony reading it out of the registry. The ephemeral
/// `succession_predecessor` hop still rides along explicitly because
/// `adopt_successor`'s `record_succession` is deliberately never fatal — a
/// failed link write must not cost the escrow section too.
fn succession_predecessor_seeds(app: &App) -> Vec<fauna_client_recovery::PredecessorSeed> {
    let mut out: Vec<fauna_client_recovery::PredecessorSeed> = Vec::new();
    if let Some(actor_hex) = app.succession_predecessor.as_deref() {
        match (
            registry(app).secrets(actor_hex),
            fauna_core::hex32::decode(actor_hex),
        ) {
            (Some(stored), Ok(actor_id)) => {
                match fauna_core::identity::ActorKeypair::from_secret_hex(&stored.secret_hex) {
                    Ok(keypair) => out.push(fauna_client_recovery::PredecessorSeed {
                        actor_id,
                        seed: *keypair.secret_bytes(),
                    }),
                    Err(_) => tracing::warn!(
                        actor = %actor_hex,
                        "the predecessor's stored material did not parse — minting the \
                         successor kit without it"
                    ),
                }
            }
            _ => tracing::warn!(
                actor = %actor_hex,
                "the predecessor's registry row is gone — minting the successor kit without it"
            ),
        }
    }
    for seed in escrow_predecessor_seeds(app, app.settings.active_secret_hex()) {
        if !out.iter().any(|s| s.actor_id == seed.actor_id) {
            out.push(seed);
        }
    }
    out
}

/// Whom `actor_id_hex` succeeded from, per this device's account registry, as
/// the typed list the profile writers admit an inherited base against
/// (`profile.md` § After an identity succession, the successor RE-PUBLISHES).
/// The registry's own walk, never a hand-rolled filter — the same rows linux,
/// web and the FFI apps read.
pub(crate) fn profile_predecessors(
    app: &App,
    actor_id_hex: &str,
) -> Vec<fauna_core::identity::ActorId> {
    fauna_client_profile::predecessors_from_hex(&registry(app).predecessors_of(actor_id_hex))
}

/// The full-chain predecessor seeds an **escrow-writing** ceremony must carry,
/// resolved from the registry for the identity `secret_hex` names — the
/// shared resolution behind every blob re-put (`identity-succession.md`
/// § Seed escrow: a kit *replacement* re-puts the resting blob too, and the
/// blob it replaces may be carrying predecessor seed(s) inside the corpus
/// re-seal window; writing it without them silently reopens the device-loss
/// race the escrow backstop exists to close).
pub(crate) fn escrow_predecessor_seeds(
    app: &App,
    secret_hex: &str,
) -> Vec<fauna_client_recovery::PredecessorSeed> {
    let Ok(keypair) = fauna_core::identity::ActorKeypair::from_secret_hex(secret_hex) else {
        return Vec::new();
    };
    // The decode (and its deliberate skip-a-bad-row behaviour) is shared — linux
    // and the FFI apps resolve the same rows the same way.
    fauna_client_recovery::predecessor_seeds_from_rows(
        registry(app).predecessor_seeds(&keypair.actor_id_hex()),
    )
}

/// The retired identities' `BackupKey`s this device can resolve — the input to
/// the aftermath's `__mls` re-seal (`succession-aftermath.md` § Re-key scope, the
/// `BackupKey` corpus row).
///
/// Same resolution as [`spawn_succession_aftermath`]'s, and deliberately the
/// same honest gaps: a predecessor whose secret this device never held is simply
/// absent, never an error. Empty for every identity that never succeeded, which
/// is what makes the pass free for them — `MlsStateSync::load` skips it without
/// a round trip.
pub(crate) fn succession_predecessor_backup_keys(
    app: &App,
    actor_id: &str,
) -> Vec<fauna_client_mls_sync::BackupKey> {
    // The resolution itself is shared (`AccountRegistry::predecessor_backup_keys`)
    // rather than open-coded here: the `__mls` and chunk-corpus legs
    // all want this exact list, and a per-app copy of a filter whose failure mode
    // is silent — a dropped row reads as "no key opens it" forever — is precisely
    // the divergence priority #2 exists to prevent.
    registry(app).predecessor_backup_keys(actor_id)
}

/// Read the open unattested-member reviews off the succession ledger and hand them to the
/// UI (`identity-succession.md` § Propagation → *MLS groups*).
///
/// **The one place this app re-reads that roster**, so that every surface
/// rendering it — the mark + Keep pair on a `thread-member-chip`, the badge on a
/// contact row — is refreshed by the same call rather than each page inventing
/// its own read. It is what runs after the aftermath's raise and after every
/// adjudication.
///
/// Best-effort and log-only, like every other leg of the aftermath: the roster
/// is a *review* surface, so a read that fails leaves the marks absent for this
/// session and the next read restores them. It must never be allowed to fail a
/// sign-in.
///
/// ⚠ Deliberately **not** wired to a nav-edge refresh. A member list paints on
/// every frame while the ledger changes only when a ceremony raises items or the
/// owner answers one, and putting a round trip on the Conversations tab press
/// would tax the most-used page in the app for state that almost never moves.
/// The window that leaves — a peer device's verdict, unseen here until the next
/// read — is the direction § Propagation blesses: a re-asked question is
/// harmless, a silently hidden flagged person is the failure the surface exists
/// to prevent.
pub(crate) async fn refresh_member_reviews(
    store: Arc<dyn fauna_client_config::SuccessionLedgerStore>,
    tx: &UnboundedSender<UiMessage>,
) {
    match fauna_client_config::load_member_reviews(store.as_ref()).await {
        Ok(roster) => {
            tracing::debug!(open = roster.len(), "read the member-review roster");
            let _ = tx.send(UiMessage::Data(DataMessage::MemberReviews(roster)));
        }
        Err(e) => tracing::warn!(
            error = %e,
            "reading the member-review roster failed; the marks stay absent this session"
        ),
    }
}

/// The filter plane's twin of [`refresh_member_reviews`] — read the ids the
/// filter list marks, over the same succession-ledger seam.
///
/// Separate calls rather than one combined read because the two are refreshed
/// by different gestures: adjudicating a group member must not re-read the
/// filter plane, and vice versa. Both are cheap (`load` is one local account
/// store read), and keeping them apart is what lets each surface own its own
/// refresh point.
pub(crate) async fn refresh_filter_marks(
    store: Arc<dyn fauna_client_config::SuccessionLedgerStore>,
    tx: &UnboundedSender<UiMessage>,
) {
    match fauna_client_config::load_filter_marks(store.as_ref()).await {
        Ok(ids) => {
            tracing::debug!(open = ids.len(), "read the inherited-filter marks");
            let _ = tx.send(UiMessage::Data(DataMessage::FilterMarks(ids)));
        }
        Err(e) => tracing::warn!(
            error = %e,
            "reading the inherited-filter marks failed; they stay absent this session"
        ),
    }
}

/// Run the post-succession **aftermath** for this identity — the ordered pass
/// that repairs what a succession moves ownership of but not the seal on.
///
/// **The pass itself is shared**
/// ([`fauna_client_recovery::aftermath::run_succession_aftermath`]), and so is
/// every reason its legs run in the order they do — read that module before
/// changing anything about sequencing. What stays here is what only this app
/// can answer: which predecessor material this device holds, what the ceremony
/// this session ran knows, where this device's account state lives, and how a
/// leg's progress reaches the UI.
///
/// **Ordering — why this is the FIRST hook in [`establish`].** Two post-auth
/// paths read account state without waiting for anything:
/// `subscriptions_author::start` and `events::spawn_refresh`. For a successor
/// those reads can run before the pass has carried its inherited state, so
/// spawning this ahead of them is what stops the user meeting an error the
/// pass would have fixed. ⚠ **The residual is honest and declared:** this
/// spawns rather than blocks (`establish` is synchronous and fire-and-forget by
/// construction, as is its whole launch-routing chain), so the ordering is a
/// head start, not a barrier — a reader can still lose the race by one
/// round-trip. What makes that acceptable rather than hidden is that every one
/// of those readers self-heals: the subscriptions loop re-polls and events
/// refetch on the nav edge. A genuine barrier would mean threading a readiness
/// gate through both,
/// and is worth doing only if that race is ever observed to bite. (The barriers
/// *inside* the pass are real ones — that is the shared driver's whole point.)
///
/// **Why it is not folded into [`discharge_succession_obligations`].** That fires once,
/// gated on [`App::succession_kit_owed`], on the one device that ran the
/// ceremony. This must run at **every** sign-in on **every** device until the
/// corpus is actually re-sealed — which is why there is no progress state at
/// rest: the corpus is its own progress record.
///
/// **Zero cost for everyone else.** [`AccountRegistry::predecessors_of`] is an
/// in-memory walk of the account list, so an ordinary identity — one that never
/// succeeded — returns here having done nothing and rendered nothing.
pub(crate) fn spawn_succession_aftermath(
    app: &App,
    client: Arc<NestClient>,
    actor_id: &str,
    secret_hex: &str,
    tx: &UnboundedSender<UiMessage>,
) {
    let registry = registry(app);
    let predecessors = registry.predecessors_of(actor_id);
    if predecessors.is_empty() {
        return;
    }
    let Ok(keypair) = fauna_core::identity::ActorKeypair::from_secret_hex(secret_hex) else {
        tracing::warn!("the corpus re-seal could not parse this session's own secret; skipping");
        return;
    };

    // Resolve material per row through the registry's OWN walk — never a
    // hand-rolled filter here (`AftermathInputs::predecessor_keys`' ⚠). A
    // predecessor whose secret this device does not hold is skipped, never
    // refused: that is the ordinary state on a device the user did not succeed
    // from, and the pass still runs so it can report the drafts as owed by
    // another device rather than silently doing nothing. Passing an empty
    // list is therefore meaningful, not a bug.
    let resolved: Vec<fauna_client_config::BackupKey> = registry
        .predecessor_backup_keys_by_actor(actor_id)
        .into_iter()
        .map(|(_, key)| key)
        .collect();
    if resolved.len() != predecessors.len() {
        tracing::warn!(
            rows = predecessors.len(),
            resolved = resolved.len(),
            "some predecessor rows did not resolve to material this device holds — \
             the re-seal skips them and another device is owed the pass"
        );
    }

    let owner_secret = *keypair.secret_bytes();
    let tx = tx.clone();

    tokio::spawn(async move {
        // No review re-read here: the review surfaces live on the succession
        // ledger, which the post-store-ready pass re-reads.
        let mut sink = AftermathUi {
            tx,
            reviews_store: None,
        };
        let inputs = fauna_client_recovery::aftermath::AftermathInputs {
            owner_secret,
            predecessor_keys: resolved,
        };
        fauna_client_recovery::aftermath::run_succession_aftermath(client, inputs, &mut sink).await;
    });
}

/// The succession-ledger seam a machine built before the store is up holds
/// ([`crate::settings::nests::ledger_door`] over
/// `SettingsState::account_runtime`).
pub(crate) fn ledger_seam(app: &App) -> Arc<dyn fauna_client_config::SuccessionLedgerStore> {
    crate::settings::nests::ledger_door(app.settings.account_runtime.clone())
}

/// The `fauna.state.backup` seam over the same slot
/// ([`crate::settings::nests::backup_door`]).
pub(crate) fn backup_seam(app: &App) -> Arc<dyn fauna_client_config::BackupStateStore> {
    crate::settings::nests::backup_door(app.settings.account_runtime.clone())
}

/// Run the post-store-ready half of the aftermath
/// ([`fauna_client_recovery::ledger_aftermath::run_ledger_aftermath`]) — called
/// once from the `AccountStoreReady` arm, the edge at which the succession
/// ledger's seam ([`App::ledger_store`]) exists. It drains a ceremony the
/// succession fold parked in the registry (the member-item and filter-mark
/// raises) and then re-reads both review surfaces; on every ordinary sign-in
/// it is one in-memory registry read and the re-read.
pub(crate) fn spawn_ledger_aftermath(app: &App) {
    let (Some(session), Some(ledger)) = (app.session.as_ref(), app.ledger_store.clone()) else {
        return;
    };
    let client = Arc::clone(&session.client);
    // Leg 2's `fauna.state.backup` door — the same account store the ledger
    // seam wraps (set together at `AccountStoreReady`).
    let backup = backup_seam(app);
    let registry = registry(app);
    let mail = app.settings.mail_store();
    // Leg 6 drives the Mail page's own machine, so the burn's snapshot refresh
    // lands on the page the user is shown.
    let mail_burn = app.settings.mail_machine();
    let tx = app.tx.clone();
    // The period-key custody legs 4 and 8 read — the same store as the ledger.
    let period_keys = crate::settings::period_key_door(app.settings.account_runtime.clone());
    // The succession cut's custody arm runs over the same account's folder-key
    // custody (`writer-signed-change-records.md` ruling (11)(a)).
    let custody_cut = fauna_client_folders::SetCustodyCut::new(
        fauna_client_folders::FoldersClient::new(Arc::clone(&client)),
        crate::settings::folder_key_door(app.settings.account_runtime.clone()),
    );
    tokio::spawn(async move {
        let mut sink = AftermathUi {
            tx,
            reviews_store: Some(Arc::clone(&ledger)),
        };
        // The box this connection is bound to keys leg 2's per-box
        // destination list; unprovable ⇒ `None`, which skips leg 2 for this
        // pass rather than guessing a row.
        let bound_nest = match fauna_client_pair::resolve_this_nest_id(&client).await {
            Ok(id) => <[u8; 32]>::try_from(id).ok(),
            Err(e) => {
                tracing::warn!(error = %e, "aftermath: bound nest id unprovable; leg 2 skipped");
                None
            }
        };
        let parked = fauna_client_recovery::ledger_aftermath::run_ledger_aftermath(
            client,
            ledger.as_ref(),
            backup.as_ref(),
            bound_nest,
            period_keys,
            mail.as_ref(),
            mail_burn,
            &custody_cut,
            &registry,
            &mut sink,
        )
        .await;
        tracing::debug!(?parked, "the post-store-ready aftermath pass settled");
    });
}

/// This app's [`AftermathSink`](fauna_client_recovery::aftermath::AftermathSink):
/// every leg's progress becomes the `UiMessage` the recovery-kit page already
/// renders, and the one hook re-reads the review surfaces.
struct AftermathUi {
    tx: UnboundedSender<UiMessage>,
    /// The succession-ledger seam the review re-read goes through — `Some` on
    /// the post-store-ready pass (the one that fires the hook), `None` on the
    /// post-auth pass, which never does.
    reviews_store: Option<Arc<dyn fauna_client_config::SuccessionLedgerStore>>,
}

impl fauna_client_recovery::aftermath::AftermathSink for AftermathUi {
    fn backup_regrant(&mut self, progress: fauna_client_config::BackupRegrantProgress) {
        let _ = self
            .tx
            .send(UiMessage::Data(DataMessage::BackupRegrantProgress(
                progress,
            )));
    }

    fn grant_remint(&mut self, progress: fauna_client_capabilities::GrantRemintProgress) {
        let _ = self
            .tx
            .send(UiMessage::Data(DataMessage::GrantRemintProgress(progress)));
    }

    fn drafts_reseal(&mut self, progress: fauna_client_drafts::DraftsResealProgress) {
        let _ = self
            .tx
            .send(UiMessage::Data(DataMessage::DraftsResealProgress(progress)));
    }

    fn mail_burn(&mut self, progress: fauna_client_mail_settings::MailBurnProgress) {
        let _ = self
            .tx
            .send(UiMessage::Data(DataMessage::MailBurnProgress(progress)));
    }

    /// The roster the review surfaces render, read back directly behind the
    /// raises, so a successor's very first session shows the marks the ceremony
    /// it just ran produced rather than waiting for the next sign-in. Two calls
    /// rather than one: the two planes are refreshed by different gestures
    /// everywhere else too, and each read is one local account-store read.
    async fn config_stage_settled(&mut self) {
        if let Some(store) = self.reviews_store.clone() {
            refresh_member_reviews(store.clone(), &self.tx).await;
            refresh_filter_marks(store, &self.tx).await;
        }
    }
}

/// Persist a submitted invite request so the next launch resumes at
/// `invite_request` instead of `handle_entry`.
///
/// Deliberately writes **no `nest_url`**: `onboarding.md` § App-launch routing
/// keys the pending-invite row on `identity + nest_url absent + pending_invite`,
/// so a saved `nest_url` here would divert the relaunch onto the silent-challenge
/// row. The record carries its own `nest_url` for seeding.
///
/// The slot is the account registry's per-actor one — the same slot
/// `RegistryLaunchPersistence::load_pending_invite()` reads, which is what lets
/// the shared `LaunchMachine` route this row itself rather than a pre-machine
/// branch in the client.
pub fn persist_pending_invite(
    app: &App,
    secret_hex: &str,
    record: &PendingInviteRecord,
) -> Result<(), String> {
    fauna_client_accounts::persist_pending_invite(&registry(app), secret_hex, record)
        .map(|_| ())
        .map_err(|e| {
            fauna_i18n::strings::onboarding::session_error::persist_account(&e.to_string())
        })
}

/// Where the wizard commits a secret the moment the user confirms it — **moment
/// 1** of the two-moment contract (`long-term-store.md`): the registry, plus
/// whether this wizard runs in append mode.
///
/// The append flag is what the shared moment branches on: a second-account
/// wizard running over a live session writes nothing at confirm and commits at
/// its own terminal ([`adopt_appended`]) precisely so an abandoned append
/// leaves the store untouched (`apps/tui.md` § Append-mode "Add account"). The
/// first-run wizard has no such alternative — its not-yet-written secret
/// exists nowhere else at all. The rule itself lives in
/// `fauna_client_accounts::persist_confirmed_identity` (2026-09-24), shared by
/// all seven apps; tui only says which mode it is in.
///
/// Computed by the caller rather than read off `App` inside [`crate::wizard::
/// run_action`] because that runs spawned, outliving the borrow: the sink is a
/// cheap owned registry view, so the decision is made where `App` is in hand
/// and travels into the task.
pub(crate) fn confirm_identity_sink(app: &App) -> ConfirmIdentitySink {
    ConfirmIdentitySink {
        registry: registry(app),
        append: app.adding_account,
    }
}

/// [`confirm_identity_sink`]'s value: the registry to commit into and the
/// wizard mode the shared moment branches on.
#[derive(Clone)]
pub(crate) struct ConfirmIdentitySink {
    registry: AccountRegistry,
    append: bool,
}

/// Commit a confirmed identity through [`confirm_identity_sink`]'s registry.
///
/// Best-effort and log-only, matching every other app's confirm-identity write
/// (linux `tracing::error!`, web's swallowing commit, apple `try?`): the
/// machine has already advanced, so a throw here would only desync the UI from
/// wizard state. The shared helper's read-back is what makes this log line
/// fire on a keystore that silently kept nothing; in append mode the helper
/// writes nothing at all.
pub(crate) fn persist_confirmed_identity(sink: &ConfirmIdentitySink, secret_hex: &str) {
    if let Err(e) =
        fauna_client_accounts::persist_confirmed_identity(&sink.registry, secret_hex, sink.append)
    {
        tracing::error!("[onboarding] committing the confirmed identity: {e}");
    }
}

/// Persist a deferred-DNS nest so the next launch resumes on the "Almost ready"
/// surface instead of `handle_entry`.
///
/// Like [`persist_pending_invite`], this writes **no `nest_url`**: the nest is
/// provisioned but not yet claimed, so there is nothing a silent challenge could
/// authenticate against. The record carries its own `nest_url` for seeding, and
/// the awaiting-manual-dns row outranks the silent-challenge row anyway
/// (`onboarding.md` § App-launch routing) — but leaving the slot empty keeps the
/// store honest about what has actually been authenticated.
///
/// The slot is the account registry's per-actor one — the same slot
/// `RegistryLaunchPersistence::load_awaiting_dns()` reads, so the shared
/// `LaunchMachine` routes this row itself rather than a pre-machine branch.
pub fn persist_awaiting_dns(
    app: &App,
    secret_hex: &str,
    record: &AwaitingDnsRecord,
) -> Result<(), String> {
    fauna_client_accounts::persist_awaiting_dns(&registry(app), secret_hex, record)
        .map(|_| ())
        .map_err(|e| {
            fauna_i18n::strings::onboarding::session_error::persist_account(&e.to_string())
        })
}

/// Adopt a freshly-onboarded identity: persist the trio through the shared
/// registry, then bring the session up.
///
/// This is `onboarding.md` § Wizard exit handling's `LoggedIn` row — "save
/// `(nest_url, handle)` to the long-term identity store … navigate to the
/// authenticated UI". Persist *before* connecting so a crash between the two
/// leaves a relaunchable account rather than a lost secret.
#[allow(clippy::too_many_arguments)] // each arg is a distinct onboarding fact; a wrapper struct would just move the list
pub fn adopt(
    app: &mut App,
    tx: &UnboundedSender<UiMessage>,
    nest_url: &str,
    dial_url: &str,
    secret_hex: &str,
    handle: String,
    // The reach hint, captured off the machine before the wizard is torn down
    // (`onboarding.md` § Reach hint). `Some` only when this session provisioned
    // the box it is signing in to; `None` on every other path, and then the
    // account simply waits for DNS exactly as before.
    reach_ipv4: Option<&str>,
    // The predecessor seeds a phrase-only restore recovered; empty on every
    // other path. Persisted before the session is built — see
    // [`persist_logged_in_identity`].
    restored_predecessors: &[fauna_onboarding_machine::nest_api::RestoredPredecessorSeed],
) -> Result<(), String> {
    persist_logged_in_identity(app, secret_hex, nest_url, reach_ipv4, restored_predecessors)?;
    // `nest_url` is the identity truth (persisted above); `dial_url` is the
    // socket to open. They are the same string in every production build —
    // they differ only under the e2e `provider_base_urls["nest"]` override,
    // which is what lets a domain-shaped claim reach a local nest
    // (`OnboardingMachine::resolved_nest_dial_url`).
    establish(app, tx, dial_url, secret_hex, handle)
}

/// [`adopt`]'s persistence half: the signed-in identity, then any predecessor
/// seeds a phrase-only restore recovered — both BEFORE [`establish`] runs.
///
/// **The order is the point.** `establish` resolves the predecessor chain ONCE,
/// into the Media machine's read custody and the drafts rails
/// ([`succession_predecessor_backup_keys`]); a predecessor persisted after it
/// reaches the registry but not the session, so a freshly restored device lists
/// nothing of the corpus still sealed under the retired identity until a
/// relaunch (measured on a tier_3 run, 2026-09-26: `media rows omitted …
/// dropped=1` with the predecessor seed already restored). And they cannot go
/// first either: [`persist_restored_predecessors`] needs the restored identity
/// added and active, or `add_account` would hand `active` to a retired one.
pub(crate) fn persist_logged_in_identity(
    app: &App,
    secret_hex: &str,
    nest_url: &str,
    reach_ipv4: Option<&str>,
    restored_predecessors: &[fauna_onboarding_machine::nest_api::RestoredPredecessorSeed],
) -> Result<(), String> {
    let registry = registry(app);
    // The shared `LoggedIn` moment (add the account with its home nest, activate
    // it, spend the pending-invite and awaiting-DNS slots, record the reach
    // hint) — one implementation for all seven apps. This was tui's own inline
    // copy until the hint gave it a fourth thing to remember; the awaiting-DNS
    // clear followed on 2026-09-21, once its moment was ruled to be exactly this
    // terminal (`onboarding.md` § Long-term store contract).
    fauna_client_accounts::persist_logged_in(&registry, secret_hex, nest_url, None, reach_ipv4)
        .map_err(|e| {
            add_refusal_line(
                &e,
                fauna_i18n::strings::onboarding::session_error::persist_account,
            )
        })?;
    persist_restored_predecessors(app, restored_predecessors);
    Ok(())
}

/// The line an account add refused by the registry paints: the shared
/// [`fauna_client_accounts::add_refused_copy`] when the user can act on the
/// refusal (the device's account list is full — `long-term-store.md`
/// § Multi-account evolution → *The index is bounded*), else `wrap` around
/// the error's own message.
fn add_refusal_line(err: &fauna_client_accounts::AccountError, wrap: fn(&str) -> String) -> String {
    match fauna_client_accounts::add_refused_copy(err) {
        Some(line) => line.resolve(fauna_i18n::strings::lookup),
        None => wrap(&err.to_string()),
    }
}

/// Adopt the successor identity a succession ceremony just minted, then bring
/// the session up as it (`identity-succession.md` § Propagation → *Own device
/// fleet*).
///
/// **Persist first, and unconditionally.** At the moment the ceremony returns,
/// the successor seed exists nowhere but that return value while the *account*
/// already belongs to it on the nest — so a crash between the two would leave an
/// account nobody holds the key to, the client-only-resident key material the
/// no-user-data-loss invariant names by name. That is the same "persist before
/// connecting" ordering [`adopt`] takes, for a sharper version of the same
/// reason.
///
/// The **old** account row is deliberately kept, exactly as the launch-side
/// superseded path keeps credentials: the old secret is still the user's, it is
/// what any later re-import or audit reasons about, and the account moved rather
/// than the person. It simply no longer authenticates.
///
/// Nothing here re-derives the switch: `switch_account` already tears the old
/// session down and re-launches over the now-active account, which is what makes
/// the handle come back (it moved to the successor inside the nest's succession
/// transaction, so the ordinary launch resolves it).
///
/// **The link is recorded BEFORE the switch, and that order is load-bearing for
/// a bound instance** (`FAUNA_BOUND_ACCOUNT`, or the chooser's pick): the shared
/// `record_succession` is also where the process's launch binding follows the
/// account to the successor (`account-scoping.md` § Concurrent instances → *The
/// binding follows the account*), and `switch_account` re-enters `establish`,
/// whose bound-or-refuse gate would otherwise see a binding still on the retired
/// id and exit the process mid-ceremony — taking the closing act's kit render and the sweep view with it.
pub fn adopt_successor(
    app: &mut App,
    nest_url: &str,
    secret_hex: &str,
    succeeded_at: Option<i64>,
) -> Result<(), String> {
    let actor_id = registry(app)
        .add_account(secret_hex, Some(nest_url), None)
        .map_err(|e| {
            add_refusal_line(
                &e,
                fauna_i18n::strings::onboarding::session_error::persist_successor,
            )
        })?;
    // Record which row the retired identity is, durably, while we still know.
    // `succession_predecessor` is ephemeral app state cleared at the kit
    // discharge, and after the switch below nothing else names that row — but
    // the corpus re-seal needs it at *every* later sign-in, on every device, to
    // open blobs still sealed under the predecessor's `BackupKey`
    // (`succession-aftermath.md` § Re-key scope).
    if let Some(predecessor) = app.succession_predecessor.clone() {
        park_succession_ceremony(app, &predecessor, &actor_id, succeeded_at);
    }
    if let Some(predecessor) = app.succession_predecessor.clone()
        && let Err(e) = registry(app).record_succession(&predecessor, &actor_id)
    {
        // Never fatal: the succession has already landed on the nest, and a
        // failure here costs the automatic re-seal, not the account.
        tracing::warn!(
            predecessor = %predecessor,
            successor = %actor_id,
            error = %e,
            "recording the succession link failed — the corpus re-seal will \
             not find this predecessor automatically"
        );
    }
    app.switch_account(&actor_id, false).map_err(|e| {
        fauna_i18n::strings::onboarding::session_error::switch_successor(&e.to_string())
    })
}

/// Hand a landed succession to the account switch — the one fold both of the
/// ceremony's surfaces end in (the Settings Recovery kit section and the locked
/// launch surface's `identity_stolen_entry`).
///
/// Everything the successor's session must find is set on `App` **before** the
/// switch, because `switch_account` tears the departing identity's state down
/// and these three fields are the ones declared to survive it:
///
/// - `succession_sweep` — what the pre-switch group sweep managed;
/// - `succession_kit_owed` — the ceremony's last step, which only the
///   successor's session can perform. Set on the adoption-failure path too,
///   deliberately: the succession landed either way, so the account is already
///   kitless and escrowless, and a user who recovers by importing the seed
///   still owes it;
/// - `succession_predecessor` — which identity that kit must carry into its
///   escrow blob, named here because after the switch nothing else says which
///   registry row it is. `None` only when the caller could not derive it.
///
/// `succeeded_at` is the nest's commit stamp, parked by [`adopt_successor`]
/// beside the link; `None` is a real value (the reconcile arm).
pub fn adopt_landed_succession(
    app: &mut App,
    nest_url: &str,
    predecessor: Option<String>,
    sweep: crate::settings::SweepStatus,
    successor_secret_hex: &str,
    succeeded_at: Option<i64>,
) -> Result<(), String> {
    app.succession_sweep = Some(sweep);
    app.succession_kit_owed = true;
    app.succession_predecessor = predecessor;
    adopt_successor(app, nest_url, successor_secret_hex, succeeded_at)
}

/// Park what only this ceremony knows — the raising predecessor, the sweep's
/// review roster, the nest's commit stamp — durably in the account registry,
/// for the successor's post-store-ready pass
/// ([`fauna_client_recovery::aftermath::PendingCeremony`]). Called beside the
/// `record_succession` it precedes, pre-switch: the single durable decision
/// point of the member-item and filter-mark raises.
///
/// The roster is read off the report the sweep just produced
/// ([`succession_review_roster`]), never re-derived later — the report is the
/// membership as it stood across the compromise window.
fn park_succession_ceremony(
    app: &App,
    predecessor_hex: &str,
    successor_hex: &str,
    succeeded_at: Option<i64>,
) {
    let Ok(predecessor) = fauna_core::hex32::decode(predecessor_hex) else {
        tracing::warn!("the ceremony's predecessor did not parse — nothing parked for the raises");
        return;
    };
    fauna_client_recovery::aftermath::PendingCeremony::new(
        &fauna_core::identity::ActorId(predecessor),
        &succession_review_roster(app),
        succeeded_at,
    )
    .park(&registry(app), successor_hex);
}

/// Adopt a freshly-onboarded identity in APPEND mode — the "Add account" flow,
/// where a session is already live. Persist the new identity (plus any
/// pending-resume slot the wizard's terminal implies) through the shared
/// registry, then switch the whole client to it via [`crate::app::App::switch_account`]
/// (tear the live session down + re-launch over the now-active new account).
///
/// This is the tui analog of linux's "add_account + trigger_switch_account"
/// (`long-term-store.md` § Multi-account evolution): unlike [`adopt`] (the
/// no-session onboarding path, which `establish`es a session directly over the
/// current surface), append must DROP the old session first — exactly what
/// `switch_account` does. `add_account` is idempotent, so importing an identity
/// already in the registry just re-activates it. All three wizard terminals are
/// handled: `LoggedIn` lands the new account `Online`; the two pending terminals
/// carry their resume slot so the post-switch re-launch lands on that account's
/// own resume surface (invite-request / "Almost ready").
/// Adopt an append whose wizard reached `PendingReview` — the pending-invite
/// journey's append arm, which since the 2026-08-12 retirement is triggered by
/// the **submit return** rather than by a wizard exit.
///
/// `onboarding.md` § Multi-account is explicit that this is the *same*
/// registration-and-switch the retired `InviteSubmitted` terminal used to run
/// (register the append identity, write its per-actor pending-invite slot,
/// switch to it) — only the trigger moved. The wizard then stays on
/// `invite_request`, which is simply the newly-active account's launch surface.
pub fn adopt_appended_pending_invite(
    app: &mut App,
    slot: &fauna_onboarding_machine::PendingInviteSlot,
    secret_hex: &str,
) -> Result<(), String> {
    let record = PendingInviteRecord {
        nest_url: slot.nest_url.clone(),
        handle: slot.handle.clone(),
        request_id: slot.request_id.clone(),
        status_json: slot.status_json.clone(),
    };
    // The same `persist_*` path the no-session flow uses, so the "no nest_url on
    // the slot" routing invariant stays the one already pinned by
    // `a_submitted_invite_persists_the_identity_but_no_nest_url`.
    persist_pending_invite(app, secret_hex, &record)?;
    let actor_id = registry(app).active().ok_or_else(|| {
        fauna_i18n::strings::onboarding::session_error::NO_ACTIVE_ACCOUNT.to_string()
    })?;
    app.adding_account = false;
    app.switch_account(&actor_id, false).map_err(|e| {
        fauna_i18n::strings::onboarding::session_error::switch_appended(&e.to_string())
    })
}

pub fn adopt_appended(
    app: &mut App,
    outcome: &fauna_onboarding_machine::WizardOutcome,
    secret_hex: &str,
) -> Result<(), String> {
    let actor_id = persist_appended(app, outcome, secret_hex)?;
    // Leave append mode and switch the live client to the new (now-active) account:
    // tear the old session down + re-launch over it (its slots resolve the launch
    // machine — `LoggedIn` → Online → establish; a pending terminal → its own
    // resume surface). `switch_account`'s `set_active` re-activates the new account,
    // never flagged at birth, so no `ConfirmationRequired`.
    app.adding_account = false;
    app.switch_account(&actor_id, false).map_err(|e| {
        fauna_i18n::strings::onboarding::session_error::switch_appended(&e.to_string())
    })
}

/// The registry half of [`adopt_appended`], split out so it is unit-testable
/// without the async switch: persist the new identity + the resume slot the
/// wizard's terminal implies (each path sets the new account **active**), and
/// return its actor id. `LoggedIn` carries the nest_url and spends any pending
/// slot; the two pending terminals reuse the exact `persist_*` paths the
/// no-session flow uses (so their "no nest_url on the slot" routing invariant is
/// the one already pinned by `a_submitted_invite_persists_the_identity_but_no_nest_url`).
fn persist_appended(
    app: &App,
    outcome: &fauna_onboarding_machine::WizardOutcome,
    secret_hex: &str,
) -> Result<String, String> {
    use fauna_onboarding_machine::WizardOutcome;
    match outcome {
        WizardOutcome::LoggedIn { nest_url, .. } => {
            // The same shared terminal [`adopt`] runs — add with the home nest,
            // activate, spend the pending-invite and awaiting-DNS slots — so the
            // append path cannot drift from the no-session one (this was an
            // inline copy of the helper's first three lines until 2026-09-21).
            // No reach hint on the append path yet, so `None` keeps it as it was.
            fauna_client_accounts::persist_logged_in(
                &registry(app),
                secret_hex,
                nest_url.as_str(),
                None,
                None,
            )
            .map_err(|e| {
                add_refusal_line(
                    &e,
                    fauna_i18n::strings::onboarding::session_error::persist_account,
                )
            })
        }
        WizardOutcome::AwaitingManualDns {
            nest_url,
            dns_records,
            claim_code,
        } => {
            let record = AwaitingDnsRecord {
                nest_url: nest_url.clone(),
                handle: app.wizard.machine.current_handle(),
                dns_records_json: serde_json::to_string(dns_records).unwrap_or_default(),
                claim_code: claim_code.clone(),
                // Completed, not replaced — see the shared writer.
                reach_ipv4: None,
                nest_actor_id: None,
            };
            persist_awaiting_dns(app, secret_hex, &record)?;
            registry(app).active().ok_or_else(|| {
                fauna_i18n::strings::onboarding::session_error::NO_ACTIVE_ACCOUNT.to_string()
            })
        }
    }
}

/// The launch machine, but only once it is `Online` — the sole phase in which
/// its bearer cache can serve a token. See [`establish`] for why this gate is
/// correctness, not tuning.
fn online_launch_machine(app: &App) -> Option<Arc<fauna_launch_machine::LaunchMachine>> {
    let machine = app.launch_machine.as_ref()?;
    matches!(
        machine.snapshot().phase,
        fauna_launch_machine::LaunchPhase::Online
    )
    .then(|| Arc::clone(machine))
}

/// The launch-target registry account's `(nest_url, secret_hex, cached_handle)`
/// — the **bound** account's material when this process was launched bound,
/// never `active`; the store-active account's otherwise.
///
/// A bound launch must resolve its own account's material and never consult
/// or move `active`: falling back to it would build the session from the
/// WRONG account while the launch machine routed on the right one (the same
/// silent-wrong-account failure `account-scoping.md` § Concurrent instances
/// names as the reason bound reads bypass the active pointer entirely).
/// Mirrors linux's `launch_credentials`/`session_binding` split
/// (`main.rs`) — `resolve_launch_binding` already resolves a bound id through
/// the succession chain, exactly as [`crate::launch::start_or_offer_chooser`]
/// uses it for the collision gate.
///
/// The launch router reads this to seed the wizard / build the session after
/// the `LaunchMachine` has decided *which* branch applies — the machine reads
/// the same slots through `RegistryLaunchPersistence` (see
/// [`launch_persistence`], bound the same way), so the two never disagree.
/// `nest_url` is `None` on the hydration rows (identity but no nest).
pub fn stored_account(app: &App) -> Option<(Option<String>, SecretString, String)> {
    let registry = registry(app);
    if let Some(bound) = registry.resolve_launch_binding() {
        let material = registry.session_material(&bound)?;
        return Some((
            material.nest_url,
            material.secret_hex,
            material.handle.unwrap_or_default(),
        ));
    }
    let active = registry.active()?;
    let stored = registry.secrets(&active)?;
    let handle = registry
        .list()
        .into_iter()
        .find(|a| a.actor_id == active)
        .and_then(|a| a.handle)
        .unwrap_or_default();
    Some((stored.nest_url, stored.secret_hex, handle))
}

/// Derive an [`ActorKeypair`] from the SAME secret [`stored_account`] handed
/// back — the one seam `verify_superseded_successor` (launch.rs) and
/// `run_silent_sign_in` both go through, so neither can independently
/// regress to reading `registry(app).active()` for its actor id, which can
/// name a DIFFERENT account on a bound launch and mix identities in one
/// operation (`account-scoping.md` § Concurrent instances). Pinned by `a_bound_accounts_secret_derives_its_own_actor_not_active`:
/// a regression that stops calling this and reads `registry(app).active()`
/// instead is a diff at the CALL SITE, not inside this function — the seam
/// pin alone cannot see that regression, which is why both call sites route
/// through it rather than each re-deriving independently.
pub(crate) fn session_actor_id(secret_hex: &SecretString) -> Result<ActorKeypair, String> {
    ActorKeypair::from_secret_hex(secret_hex).map_err(|e| format!("invalid stored secret: {e}"))
}

/// The `LaunchPersistence` view the `LaunchMachine` routes on — bound to the
/// launch-target account exactly as [`stored_account`] is, so the machine and
/// the session build can never resolve two different accounts.
///
/// The launch router reads the pending-invite slot back through this same
/// adapter, so the record it seeds is byte-for-byte the record the machine
/// branched on — the alternative (re-reading the registry key directly) is a
/// second source of truth that drifts the moment either side changes.
pub fn launch_persistence(app: &App) -> RegistryLaunchPersistence {
    let registry = registry(app);
    match registry.resolve_launch_binding() {
        Some(bound) => registry.bound_launch_persistence(bound),
        None => RegistryLaunchPersistence::from_registry(registry),
    }
}

// ── Account switcher (`long-term-store.md` § Multi-account evolution) ──────────
//
// The multi-account surface reads/mutates the shared `AccountRegistry` over the
// app's credential store — the same seam the launch machine routes on, so the
// two can never disagree about which account is active. Kept here (not in
// `settings`) so registry construction lives in exactly one module, exactly as
// linux/web keep theirs behind their own client seam.

/// One row of the account switcher, projected from an [`AccountRegistry`] entry
/// for rendering. Built by [`account_switcher_rows`]; consumed by the Account
/// sub-page's switcher paint. A plain snapshot (not the live registry) so the
/// immediate-mode render never re-hits the credential store per frame.
pub struct AccountSwitcherRow {
    /// The account's actor-id hex — the switch / remove / toggle target.
    pub actor_id: String,
    /// The display label — the cached handle when known, else a short actor-id
    /// prefix (shared `fauna_core::format::account_display_label`, single-sourced
    /// so the label can't drift across clients).
    pub label: String,
    /// Whether this is the account **this instance serves** — the session's
    /// account, not the registry's active pointer, which a bound secondary never
    /// moves (`account-scoping.md` § Concurrent instances). It is not itself a
    /// switch target and paints the active indicator instead of a switch/remove
    /// affordance (the toggle still renders — see [`AccountSwitcherRow`] callers).
    pub is_active: bool,
    /// The Stage-2 `require_confirm_to_activate` flag — the per-row toggle's state.
    pub require_confirm: bool,
}

/// Project the registry's accounts into switcher rows, in registry order. Read
/// at the edges the snapshot can go stale (nav into the Account sub-page, and
/// after a toggle/remove) — never per frame.
pub fn account_switcher_rows(app: &App) -> Vec<AccountSwitcherRow> {
    let registry = registry(app);
    let serving = fauna_client_accounts::session_account(&registry);
    switcher_rows_serving(&registry, serving.as_deref())
}

/// The pure half of [`account_switcher_rows`]: `serving` is the account this
/// instance serves, passed in so a test need not seed the process-global holder.
fn switcher_rows_serving(
    registry: &fauna_client_accounts::AccountRegistry,
    serving: Option<&str>,
) -> Vec<AccountSwitcherRow> {
    registry
        .list()
        .into_iter()
        .map(|entry| {
            let label =
                fauna_core::format::account_display_label(entry.handle.as_deref(), &entry.actor_id);
            let is_active = serving.is_some_and(|s| s.eq_ignore_ascii_case(&entry.actor_id));
            AccountSwitcherRow {
                actor_id: entry.actor_id,
                label,
                is_active,
                require_confirm: entry.require_confirm_to_activate,
            }
        })
        .collect()
}

/// Read the Stage-2 `require_confirm_to_activate` flag **fresh** from the
/// registry — the gate must never trust the render snapshot, which can lag the
/// admin auto-default (`long-term-store.md` § Per-account re-auth: "clients must
/// read the flag fresh at activation"). A gone account reads `false`.
pub fn account_requires_confirm(app: &App, actor_id: &str) -> bool {
    registry(app)
        .list()
        .into_iter()
        .find(|a| a.actor_id == actor_id)
        .is_some_and(|a| a.require_confirm_to_activate)
}

/// Set the per-account Stage-2 flag. The write also marks `require_confirm_user_set`
/// (inside `set_require_confirm`), which is what pins an explicit user choice
/// against the admin auto-default — an explicit OFF sticks. Logs on failure (the
/// toggle is best-effort local state, never a fatal path).
pub fn set_account_require_confirm(app: &App, actor_id: &str, require: bool) {
    if let Err(e) = registry(app).set_require_confirm(actor_id, require) {
        tracing::error!("[session] set_require_confirm failed: {e:#}");
    }
}

/// Remove an account from this install: refused while another instance serves
/// it, else its registry entry and then its scoped local state go
/// ([`crate::account_scope::remove_account`] owns the order and why). Returns
/// the line for the caller to surface; also logged.
pub fn remove_account(app: &App, actor_id: &str) -> Result<(), String> {
    crate::account_scope::remove_account(&registry(app), actor_id)
}

/// Set the active account through the registry — the raw registry write half of
/// a switch, before the app-level teardown. `confirmed` picks
/// `set_active_confirmed` (the post-re-auth path — the ONLY confirmed call site,
/// the gate's audit surface) over `set_active`, which refuses a flagged account
/// with [`fauna_client_accounts::AccountError::ConfirmationRequired`]. See
/// [`crate::app::App::switch_account`] for the full teardown + re-launch.
pub fn set_active_account(
    app: &App,
    actor_id: &str,
    confirmed: bool,
) -> Result<(), fauna_client_accounts::AccountError> {
    let registry = registry(app);
    if confirmed {
        registry.set_active_confirmed(actor_id)
    } else {
        registry.set_active(actor_id)
    }
}

/// The admin auto-default (`long-term-store.md` § Per-account re-auth): flip the
/// **session's** account's `require_confirm_to_activate` ON iff the user has not
/// set it. Idempotent; never turns the flag off; does not consume the override
/// right. Keyed to the session's actor id — the identity the `am_i_admin` gate
/// actually observed — not `registry.active()`, so a mid-switch transient active
/// pointer can never flag the wrong account (web hit exactly that). A no-op when
/// signed out; the caller gates on `am_i_admin` being true.
pub fn auto_enable_admin_confirm(app: &App) {
    let Some(session) = app.session.as_ref() else {
        return;
    };
    if let Err(e) = registry(app).auto_enable_require_confirm(&session.actor_id) {
        tracing::error!("[session] auto_enable_require_confirm failed: {e:#}");
    }
}

/// The e2e `session` state patch (`_login_app_as` / `logged_in_app`): persist
/// the trio through the real registry, then establish the session — the same
/// seam linux's set_state handler drives (its main.rs:1578-1690). Returns
/// whether the patch was applied.
pub fn apply_session_patch(
    app: &mut App,
    tx: &UnboundedSender<UiMessage>,
    patch: &serde_json::Value,
) -> bool {
    let authenticated = patch
        .get("authenticated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let node_url = patch.get("node_url").and_then(|v| v.as_str());
    let secret_hex = patch.get("secret_hex").and_then(|v| v.as_str());
    let handle = patch
        .get("handle")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let device_id = patch.get("device_id").and_then(|v| v.as_str());

    let (Some(node_url), Some(secret_hex)) = (node_url, secret_hex) else {
        if !authenticated {
            sign_out(app, fauna_client_account_runtime::StopReason::AccountSwitch);
            return true;
        }
        tracing::warn!("[agent] session patch missing node_url/secret_hex");
        return false;
    };

    // Persist through the real store first — a relaunch must find the trio.
    let registry = registry(app);
    let new_actor_id = match registry.add_account(secret_hex, Some(node_url), device_id) {
        Ok(actor_id) => {
            let _ = registry.set_active(&actor_id);
            actor_id
        }
        Err(e) => {
            tracing::warn!("[agent] session patch: store failed: {e}");
            return false;
        }
    };

    // ...and adopt the patch's device id as this device's REAL sync identity,
    // not just the registry's per-actor credential slot `add_account` just
    // wrote. `devices.md` § New Platform Implementation Checklist steps 4-5 +
    // § This-device marker make one value do both jobs — the id the app
    // registers with is the id the marker compares against — so a patch that
    // named a device id while `device.db` kept a random one left the app
    // registering as one device and marking another. Must precede `establish`:
    // it starts the sync agent, which reads this id once and registers under it.
    // Adopted into the patch's OWN account scope — the id is per account.
    if let Some(device_id) = device_id
        && !crate::media::adopt_device_id_hex(&new_actor_id, device_id)
    {
        tracing::warn!("[agent] session patch: could not adopt the device id");
    }

    if !authenticated {
        sign_out(app, fauna_client_account_runtime::StopReason::AccountSwitch);
        return true;
    }
    // An authenticated→authenticated patch naming the SAME actor and nest is
    // the driver replaying a login the app already holds — the CR-1
    // relaunch-with-pinned-store shape, where launch auto-restored this very
    // session before the patch arrived. Converge instead of re-establishing:
    // a second `establish` here would open a second conversations engine over
    // the live session's own `mls_state.db` and hit its role lock
    // (`StateServedElsewhere`), a standing refusal that leaves the rail dark
    // for the rest of the process.
    if app
        .session
        .as_ref()
        .is_some_and(|s| s.actor_id == new_actor_id && s.client.nest_url() == node_url)
    {
        reconverge_post_auth(app, secret_hex, node_url);
        return true;
    }
    // An authenticated→authenticated patch to a DIFFERENT actor is the e2e
    // agent's in-place account-switch shortcut — the same event
    // `App::switch_account` handles for the real UI by resetting before
    // re-launching. Without this, per-actor UI state (Guardian Notify's
    // pending accumulator, content-policy thresholds, …) from the outgoing
    // actor would survive into the incoming one's render: the account-scoping
    // switch/sign-out isolation contract `drop_authenticated_state` exists to
    // uphold (measured via a leaked Guardian Notify count, row 83).
    if app
        .session
        .as_ref()
        .is_some_and(|s| s.actor_id != new_actor_id)
    {
        app.drop_authenticated_state(fauna_client_account_runtime::StopReason::AccountSwitch);
        // The incoming session is established only once the outgoing
        // account's runtime has stopped — the same order `App::switch_account`
        // keeps — so this arm re-dispatches its `establish` as a continuation
        // of that stop. The loop holds the command's ack until it has run
        // (`App::teardown_pending`), and a failure there is still loud: it
        // lands on the app's own refusal line, as a refused patch would.
        // Nothing stopping (no runtime was assembled) → straight on, below.
        if app.teardown_pending() {
            let (tx, node_url, secret_hex, handle) = (
                tx.clone(),
                node_url.to_string(),
                secret_hex.to_string(),
                handle.to_string(),
            );
            app.after_stops(move |app| {
                if let Err(e) = establish(app, &tx, &node_url, &secret_hex, handle) {
                    tracing::warn!("[agent] session patch: {e}");
                    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
                    app.report_failed_agent_command("patch", &e);
                }
            });
            return true;
        }
    }
    match establish(app, tx, node_url, secret_hex, handle.to_string()) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("[agent] session patch: {e}");
            false
        }
    }
}

/// Re-run the universal post-auth hook's **idempotent convergences** on the
/// live session's client — the converge arm's half of
/// [`apply_session_patch`], where a same-actor/same-nest patch cannot re-enter
/// [`establish`].
///
/// **Why this exists.** `establish` is the one place these hooks run, and the
/// converge arm above deliberately never reaches it (a second `establish`
/// opens a second conversations engine over the live `mls_state.db` and hits
/// its `StateServedElsewhere` role lock). Before this, a re-established
/// session on tui therefore re-ran *nothing* post-auth: the critical-alert
/// sweep in particular answered once per **process** rather than once per
/// session establishment, so a directory tampered with — or a
/// RecoveryKey-replacement window opened — after the first login could never
/// be observed. `critical-alerts.md` § Mechanism → *Lifetime* is explicit that
/// alerts are identity-scoped, and linux's own converge arm has re-run its
/// four hooks since TRACK 10 for exactly this reason (§ Implementation status
/// today); this is tui's leg of the same shape.
///
/// **What re-runs, and what deliberately does not.** Only the best-effort,
/// log-only convergences `establish` fires and beside which a repeat is a
/// cheap no-op. `establish`'s *pumps* and long-lived tasks (the
/// `ConnectionState`/reconnect pumps, the drafts rails, the subscriptions
/// author pump) are NOT re-run: they are still alive on this very client, and
/// a second copy would duplicate every event they carry. The sweep is the same
/// distinction drawn one level down — [`crate::critical_alerts::spawn_one_shot_sweep`]
/// runs a single pass rather than a second `run_alert_sweep_loop`, whose first
/// copy is still looping (that function's doc carries the reasoning, and
/// windows' e2e seam ratified the same one-shot posture).
fn reconverge_post_auth(app: &App, secret_hex: &str, node_url: &str) {
    let Some(client) = app.session.as_ref().map(|s| Arc::clone(&s.client)) else {
        return;
    };
    // Same order, same posture, same log-only contract as `establish`'s own:
    // a nest that cannot answer must not make the patch fail.
    crate::mail_glue::spawn_refresh_mail_epoch_schedule(
        Arc::clone(&client),
        secret_hex,
        app.settings.mail_store(),
        node_url,
    );
    // Every later post-auth edge re-runs the custody leg over the installed
    // handle (a no-op round-trip-free check in the steady state); before the
    // store is ready, the `AccountStoreReady` arm owns the run.
    if let Some(store) = app.settings.account_store.clone() {
        crate::recovery::spawn_custody_leg(Arc::clone(&client), store, app.tx.clone());
    }
    // The actor is unchanged by construction on this arm, so the alert keys the
    // pass posts under are the ones already on screen.
    match ActorKeypair::from_secret_hex(secret_hex) {
        Ok(keypair) => crate::critical_alerts::spawn_one_shot_sweep(
            client,
            Arc::clone(&app.settings.account_runtime),
            Arc::clone(&app.alerts),
            keypair.actor_id(),
        ),
        Err(e) => tracing::warn!(error = %e, "[agent] session patch: converge sweep skipped"),
    }
}

/// Drive `sign_out`'s stop OFF the UI loop and report its end back THROUGH it,
/// where [`DataMessage::AccountRuntimeStopped`] releases every continuation
/// queued behind it ([`App::after_stops`]) — the erase above all.
///
/// Spawned, because `sign_out` runs on the UI loop's own task: awaiting the
/// stop there (the `block_in_place` bridge this replaced) froze the terminal
/// for the whole stop budget whenever a sign-out landed behind a prologue.
/// Counted before the spawn, so nothing queued from here on can overtake it.
///
/// The stop runs as a task of its own under a watcher, so a stop that panics
/// still reports: the sign-out completes and erases anyway, loudly — the
/// contract's own answer to a stop that cannot be awaited
/// (`apps/account-scoping.md` § Erasure follows scope). A runtime shutting
/// down under it is process exit, where nothing is left to erase for.
fn spawn_stop(app: &mut App, stop: impl std::future::Future<Output = ()> + Send + 'static) {
    app.stops.stop_started();
    let tx = app.tx.clone();
    tokio::spawn(async move {
        if let Err(e) = tokio::spawn(stop).await {
            tracing::warn!(
                "[sign-out] the account runtime's stop task ended without finishing \
                 ({e}); continuing (and erasing) without it"
            );
        }
        let _ = tx.send(UiMessage::Data(DataMessage::AccountRuntimeStopped));
    });
}

/// Sign out: tear down the WS supervisor and drop the session. This function
/// touches no credentials itself — each caller owns the namespace (`App::reset`
/// wipes it, a switch keeps it); the e2e driver's per-launch
/// `FAUNA_E2E_CREDENTIAL_DIR`/`FAUNA_KEYRING_APP` isolation keeps tests
/// hermetic regardless.
///
/// `reason` states which of those the caller is about to do, because the
/// account runtime's stop cannot infer it and the two differ in what the nest
/// is told: a `SignOut` retires this machine's enrollment nest-side before
/// the store closes (the slot is about to be erased), an `AccountSwitch`
/// leaves the machine enrolled (the slot survives, and its key will be loaded
/// again). Owner: `fauna_client_account_runtime::StopReason`.
///
/// **What returns before the stop has finished — and what the caller owes.**
/// Everything in memory is dropped before this returns: the session, both
/// runtime slots, the agent surface, the share seat, the conversations engine.
/// The waits are not — the agent's un-provision, the account runtime's stop
/// and the session client's disconnect run on ONE spawned task
/// ([`spawn_stop`]), so tui's UI loop keeps rendering while the account stops.
/// So a caller hands [`App::after_stops`] everything that must follow the
/// stop — the erase above all, which must never meet a store still open
/// (`account-scoping.md` § Erasure follows scope), and the next session's
/// launch, so an account switch never briefly runs two accounts' pumps — and
/// assumes nothing about the stop itself. While the stop is in flight the loop
/// refuses input to the outgoing session ([`App::teardown_pending`]).
/// How long a leave gesture waits for its push-row drop before going on
/// without it — the drop is best-effort, never a gate (`common.md`
/// § Registration), and the nest's own deadline for the kind is 5 s.
const PUSH_DROP_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);

pub fn sign_out(app: &mut App, reason: fauna_client_account_runtime::StopReason) {
    // Unprovision the sync agent (stop its convergence loop + delete its persisted
    // capability + stop its engines) BEFORE dropping the session client. Sign-out,
    // account-switch, and factory-reset all route here (`drop_authenticated_state`
    // calls `sign_out` first); a plain app quit does NOT, so the agent keeps
    // syncing app-dead — the whole point of it. A no-op when no surface was
    // installed (e2e, non-unix, pre-auth). The loops stop now; the round trip
    // leads the spawned stop below.
    let unprovision = app.sync_agent.teardown();
    // The account-store runtime's deterministic teardown (charter § The
    // client-side lifecycle: teardown at sign-out AND quit — quit is the
    // drop path; sign-out shuts down NOW, so an account switch never briefly
    // runs two accounts' pumps). Taking both slots also flips the preference
    // surfaces' ops to meet the empty slot and be refused (`LEDGER_NOT_READY`)
    // for any op built mid-teardown.
    //
    // **Two slots, because the runtime is reachable two ways and BOTH hold the
    // store open.** `account_store` is the settled handle; `account_store_assembly`
    // is the assembly still in flight, which is the case a sign-out actually
    // tends to hit — `AccountStoreRuntime::start` opens the database on its own
    // OS thread *before* returning, and `AccountStoreReady` then has to cross
    // the UI queue, so there is a real window where a live connection holds
    // `account-store.db` while this field reads `None`. Measured `--app tui` on
    // Windows, 2026-09-03: sign-out landed 475 ms after the assembly minted the
    // writer key, took `None`, stopped nothing, and the erase that followed
    // failed with `os error 32` — leaving the signed-out user's entire account
    // store on disk. `shutdown()` is the only thing that closes that store
    // thread, so the teardown has to be able to WAIT for a handle, not just
    // check for one.
    //
    // Both slots are TAKEN here, synchronously, even though the stop itself
    // runs later: the in-flight half's `PendingAssembly` is what hands the
    // assembly's handle to this teardown's stop, under this teardown's
    // `reason` — so a sign-out's retirement reaches the runtime it stops, and
    // the late `AccountStoreReady` that assembly still sends finds no live
    // session and queues its re-shut behind this stop (`App::handle_message`).
    let settled = app.settings.set_account_store(None);
    let assembling = app.settings.account_store_assembly.take();
    let store_stop = (settled.is_some() || assembling.is_some()).then(|| {
        // WAITED FOR, never merely spawned — bounded, with a loud elapse. A
        // spawned shutdown with nothing sequenced behind it is a race the erase
        // wins: `account_scope::erase_all_known_accounts` is entirely
        // synchronous, so the sweep would run before the task had even reached
        // the assembly's channel. On Windows that is not untidiness, it is a
        // leak: an open file cannot be deleted (`os error 32`), so the
        // signed-out user's `account-store/` survived the erase
        // (`account-scoping.md` § Erasure follows scope → the ⚠ *An OPEN store
        // is an unerasable store* note). POSIX `unlink` removes an open file,
        // which is why linux never showed it. So the stop is spawned
        // ([`spawn_stop`]) and the erase is a continuation of its completion
        // ([`App::after_stops`]); until 2026-09-25 the ordering was bought by
        // blocking the UI loop on the stop instead, which froze the terminal
        // for the whole budget.
        //
        // Awaiting `shutdown()` is SUFFICIENT to close the database — no
        // `Option<Connection>` surgery in `fauna-account-store` — because
        // `Cmd::Shutdown`'s reply is sent only after the assembly loop's locals
        // drop, which is what closes the `SqliteBackend`'s connection. Witnessed
        // headlessly, on the platform that cares, by
        // `account_runtime::tests::shutdown_closes_the_db_and_leaves_the_store_dir_removable`
        // (the `-wal`/`-shm` sidecars are gone afterwards, which SQLite does only
        // on a clean last close).
        //
        // The stop itself — both halves under ONE budget — is SHARED
        // (`fauna_client_account_runtime::stop_account_runtime`), and so is the
        // budget: the user asked to be signed out, "up to 5 s" is the promise,
        // and waiting for an assembly and then stopping what it produced are two
        // halves of one stop. It also owns the two diagnostic lines this path
        // owes (`account-scoping.md` § Erasure follows scope → the ⚠ *the erase
        // must SAY what it did* corollary), so linux and the FFI shells cannot
        // report a sign-out differently from this one.
        fauna_client_account_runtime::stop_account_runtime(
            settled,
            assembling,
            fauna_client_account_runtime::ACCOUNT_RUNTIME_STOP_BUDGET,
            reason,
        )
    });
    // The share plane dies with the account runtime (its pump loop exits on
    // the handle's death) — but the SEAT must not outlive the session: rule 5
    // ties the listener to participation, and a signed-out session
    // participates in nothing. Dropping the state releases this side's Arc;
    // the listener closes when the glue task's clone follows (≤ one pump
    // tick). Clears the panel-bound ceremony seat by the same rule.
    #[cfg(feature = "p2p-share")]
    {
        app.settings.offline_share = crate::offline_share::OfflineShareState::default();
        app.settings.share_plane = None;
    }
    // The conversations engine's role hand-over, BEFORE anything erases this
    // account's scoped directories (`account-scoping.md` § Erasure follows scope →
    // the ⚠ *An OPEN store is an unerasable store* note). `reset` reaches the erase
    // through `drop_authenticated_state` → here, so covering it in `sign_out`
    // covers sign-out, account switch and factory reset alike — the same placement
    // windows uses.
    //
    // ⚠ Dropping the manager is NOT a substitute, which is why this is a call and
    // not a `take()`. `retire_conversations_engine` flushes the provider snapshot,
    // releases the role lock and closes `mls_state.db` **however many `Arc`s
    // survive** — the receive loop holds its own, and on tui the search page's arm
    // may hold another. Windows measured what refcount-hoping costs: an open handle
    // makes a file undeletable there (`os error 32`), so the sweep aborted on the
    // actor scope and left a signed-out user's conversations on disk.
    //
    // tui is affected for the same reason and on the same platform — it runs on
    // Windows too, where the same open handle blocks the same sweep. On POSIX the
    // erase succeeds regardless, because `unlink` removes an open file; the engine is then left writing to an unreachable
    // inode, which is untidy rather than a leak. Retiring fixes both readings.
    if let Some(manager) = app.conversations.manager.as_ref() {
        manager.retire_conversations_engine();
    }
    // The session leaves `App` now; its client disconnects only AFTER the
    // stop, on the same task — the order the blocking teardown always had.
    // The runtime was assembled over this client and a sign-out's stop still
    // talks to the nest (the enrollment retirement), so the disconnect must
    // not be what that round trip races.
    let client = app.session.take().map(|session| session.client);
    // Any decrypted external-media handoff file belongs to the session that
    // consented to it (`apps/tui.md` § External media handoff — the
    // sign-out sweep).
    crate::media_handoff::sweep();
    // The push row follows the signed-in identity (`common.md` § Registration
    // → the leave-shapes): every way this session leaves — sign-out, switch,
    // factory reset all reach here — drops the leaving actor's row, issued by
    // the leaving session over its own client before that client disconnects.
    // Never touches the install's opt-in bit, so the next identity re-arms.
    // Best-effort and bounded: a leave gesture completes offline.
    let push_drop = client.clone().map(|client| {
        let actor = app.settings.active_actor_id().to_string();
        async move {
            let drop = crate::push::drop_actor_row(client, &actor);
            if tokio::time::timeout(PUSH_DROP_BUDGET, drop).await.is_err() {
                tracing::warn!("push: dropping the leaving account's row timed out");
            }
        }
    });

    if unprovision.is_none() && store_stop.is_none() {
        // Nothing to wait for — every teardown before an account runtime was
        // ever assembled. Still no inline disconnect: the loop owns this task.
        if let Some(client) = client {
            tokio::spawn(async move {
                if let Some(push_drop) = push_drop {
                    push_drop.await;
                }
                client.disconnect().await
            });
        }
        return;
    }
    spawn_stop(app, async move {
        if let Some(push_drop) = push_drop {
            push_drop.await;
        }
        // The agent first, as before: its reply is the receipt that its own
        // mount of the account store is down.
        if let Some(unprovision) = unprovision {
            unprovision.await;
        }
        if let Some(stop) = store_stop {
            // The outcome's diagnostic lines are the shared stop's own.
            let _ = stop.await;
        }
        if let Some(client) = client {
            client.disconnect().await;
        }
    });
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::app::tests::test_app;
    use fauna_launch_machine::LaunchPersistence;

    /// A test build never reaches the session's live keyring
    /// (`fauna_credential_store::live_keyring_allowed`). Before the gate, the
    /// sign-out / reset / remove-account tests made ~370 real Secret Service
    /// calls per `cargo test -p fauna-tui --bins`, each a fresh DH session
    /// against the developer's `gnome-keyring-daemon` — which aborts on a client
    /// that vanishes mid-session and re-locks the login keyring box-wide.
    #[test]
    fn a_test_build_never_reaches_the_live_keyring() {
        use fauna_client_accounts::SecretStore as _;
        use fauna_credential_store as cs;
        assert!(
            cs::keyring_app_override().is_none(),
            "a unit test never runs under a harness keyring namespace"
        );
        assert!(
            !cs::live_keyring_allowed(),
            "the dev-dependency feature `no-live-keyring` is missing"
        );
        assert!(
            !cs::keyring_probe(),
            "a test build reports no usable keyring"
        );
        // So the notification arm never asks the desktop either.
        assert!(!crate::os_notify::desktop_session());

        // The keyring-arm constructor — the one `secret_store()` falls back to.
        let store = CredentialStore::new(&format!("fauna-keyring-guard-{}", std::process::id()));
        assert_eq!(
            store.file_backend_dir(),
            None,
            "the keyring arm, not the e2e file arm"
        );
        store.set("fauna/guard", "x");
        assert_eq!(store.get("fauna/guard"), None);
        store.delete("fauna/guard");
        assert_eq!(
            cs::live_keyring_calls(),
            0,
            "no call reached a native keyring arm"
        );
    }

    /// A supervisor that stopped because the nest refused its post-4401 re-mint
    /// — the user suspended mid-session — escalates to the launch surface
    /// (`onboarding.md` § App-launch routing, the previously-signed-in row);
    /// an ordinary stop does not.
    #[test]
    fn a_stopped_supervisors_session_ending_reason_escalates_and_nothing_else_does() {
        use fauna_client::NestClientError as E;
        let refused = E::Rpc(fauna_protocol::RpcError::not_registered());
        assert!(matches!(
            session_ending_escalation(&refused),
            Some(DataMessage::SignInRefused)
        ));
        assert!(matches!(
            session_ending_escalation(&E::NestIdentityChanged {
                host: "nest.example".into(),
                pinned_hex: "aa".repeat(32),
                seen_hex: None,
            }),
            Some(DataMessage::NestIdentityChanged)
        ));
        for ordinary in [
            E::RpcDisconnected {
                was_in_flight: false,
            },
            E::SubprotocolMismatch,
            E::Auth("(401) fauna.auth.signature_failed".into()),
        ] {
            assert!(
                session_ending_escalation(&ordinary).is_none(),
                "{ordinary:?} must stay the indicator's business"
            );
        }
    }

    /// An empty long-term store. Local rather than the crate's feature-gated
    /// `InMemoryPersistence`, so the test needs no extra dev-dependency feature.
    /// `pub(crate)`: the locked surface's snapshot-following pin
    /// (`crate::locked`) builds a machine over it too.
    pub(crate) struct NoPersistence;

    impl fauna_launch_machine::LaunchPersistence for NoPersistence {
        // An empty store has no account index at all, so there is nothing for
        // this build to refuse. The trait carries no default — `uniffi::export`
        // forbids one — which is why a shared-Rust addition reaches this
        // test-only impl as a compile error rather than silently.
        fn account_index_refusal(&self) -> Option<fauna_launch_machine::AccountIndexRefusal> {
            None
        }
        fn load_identity(&self) -> Option<Vec<u8>> {
            None
        }
        fn load_nest_url(&self) -> Option<String> {
            None
        }
        fn load_pending_invite(&self) -> Option<fauna_launch_machine::PendingInviteRecord> {
            None
        }
        fn load_awaiting_dns(&self) -> Option<fauna_launch_machine::AwaitingDnsRecord> {
            None
        }
        fn load_pending_factory_reset(
            &self,
        ) -> Option<fauna_launch_machine::PendingFactoryResetRecord> {
            None
        }
        fn save_pending_factory_reset(&self, _: fauna_launch_machine::PendingFactoryResetRecord) {}
        fn delete_pending_factory_reset(&self) {}
        fn save_authenticated(&self, _: String, _: String, _: String, _: String) {}
        fn delete_pending_invite(&self) {}
        fn load_reach_ipv4(&self) -> Option<String> {
            None
        }
        fn delete_reach_ipv4(&self) {}
    }

    /// A launch machine that never ran a silent challenge cannot serve a bearer:
    /// `refresh_token` no-ops off `Online`/`Refreshing` and `retry_silent_challenge`
    /// self-guards to `Offline { transient: true }`, so `LaunchMachineBearer` would
    /// return `Err` on every WS handshake. Handing it to `establish` anyway is how
    /// the freshly-onboarded `adopt` path (machine still at `WizardAt`) would build
    /// a client that can never authenticate — a silent, connection-only failure no
    /// e2e assertion on `session.authenticated` would catch.
    #[test]
    fn a_non_online_launch_machine_is_never_used_as_a_bearer_source() {
        let mut app = test_app();
        assert!(
            online_launch_machine(&app).is_none(),
            "no machine at all → no bearer"
        );

        app.launch_machine = Some(fauna_launch_machine::LaunchMachine::new(
            Arc::new(fauna_launch_machine::NullObserver),
            Arc::new(NoPersistence),
        ));
        // A freshly-built machine is at `Boot` — precisely the `adopt`-path state.
        assert_ne!(
            app.launch_machine.as_ref().unwrap().snapshot().phase,
            fauna_launch_machine::LaunchPhase::Online
        );
        assert!(
            online_launch_machine(&app).is_none(),
            "a machine that never reached Online must not back the WS handshake"
        );
    }

    /// `establish`'s one error path (a malformed stored secret) must render
    /// through i18n, not a hardcoded `format!("invalid secret: {e}")` — the
    /// same text reaches both the wizard's `error-message` (via `adopt`) and
    /// the app-launch `LaunchSurface::TransientRetry` screen (via
    /// `launch.rs`'s relaunch path, which paints `error` VERBATIM once
    /// non-empty: `fauna_launch_machine::render_text::transient_error_text`).
    /// Part of the "zero raw English error strings remain" audit.
    #[test]
    fn establish_on_a_malformed_secret_returns_the_localized_message() {
        let mut app = test_app();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let err = establish(
            &mut app,
            &tx,
            "https://nest.example",
            "not-valid-hex",
            "joiner".to_string(),
        )
        .expect_err("a malformed secret must not establish a session");
        assert!(
            err.starts_with("This device's saved secret key isn't valid:"),
            "the error text must come from the localized template, never a raw \
             format!(\"invalid secret: {{e}}\") — this is what a real user would \
             see verbatim on the launch retry screen; got: {err}"
        );
        assert!(
            !err.starts_with("invalid secret:"),
            "must not regress to the old raw, unlocalized prefix: {err}"
        );
    }

    /// A same-actor, same-nest authenticated session patch CONVERGES on the
    /// live session instead of re-running `establish`. The relaunch-with-
    /// pinned-store flow hits exactly this shape — launch auto-restores the
    /// session, then the driver replays the same login — and a re-`establish`
    /// opens a second conversations engine over the live session's own
    /// `mls_state.db`, hitting its role lock (`StateServedElsewhere`): a
    /// standing refusal that leaves the rail dark for the whole process.
    ///
    /// Observable: the session's client `Arc` is the same object after the
    /// patch — a rebuild mints a new one.
    ///
    /// A tokio test rather than a bare one: the converge arm re-fires the
    /// post-auth convergences (`reconverge_post_auth`), which spawn.
    #[tokio::test]
    async fn a_same_actor_same_nest_session_patch_converges_without_reestablishing() {
        let mut app = crate::app::tests::authed_app();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let before = Arc::as_ptr(&app.session.as_ref().expect("authed fixture").client);
        let node_url = app
            .session
            .as_ref()
            .expect("authed fixture")
            .client
            .nest_url();
        let patch = serde_json::json!({
            "authenticated": true,
            "node_url": node_url,
            "secret_hex": hex::encode(crate::app::tests::TEST_SECRET),
            "handle": "test-handle",
        });
        assert!(
            apply_session_patch(&mut app, &tx, &patch),
            "a replayed login the app already holds is applied (as a no-op)"
        );
        let after = Arc::as_ptr(&app.session.as_ref().expect("session survives").client);
        assert_eq!(
            before, after,
            "the patch must keep the live session's client — a new Arc means \
             `establish` re-ran over the live session's own MLS store"
        );
    }

    /// The converge arm still owes the post-auth feeders their re-run.
    ///
    /// Before this, `apply_session_patch`'s same-actor/same-nest arm returned
    /// without touching anything, so the critical-alert sweep answered once per
    /// **process**: `run_alert_sweep_loop` was dispatched by the first
    /// `establish` and nothing ever asked it again, even though
    /// `critical-alerts.md` § Mechanism → *Lifetime* scopes alerts to the
    /// identity, not the process. `test_alert_sweep_directory_feeders_e2e.py`
    /// read that defect as `sweep_passes_started` stuck at 1 across a
    /// re-establish (both its tui arms red).
    ///
    /// The observable is the sweep's own causal barrier — the started counter,
    /// bumped by `run_session_start_sweep_at` before any feeder reads anything,
    /// so this test needs no reachable nest and no clock: it asserts a pass
    /// **began**, which is exactly the property the e2e barrier asserts.
    #[tokio::test]
    async fn the_converge_arm_dispatches_another_sweep_pass() {
        let mut app = crate::app::tests::authed_app();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let node_url = app
            .session
            .as_ref()
            .expect("authed fixture")
            .client
            .nest_url();
        let before = app.alerts.sweep_passes_started();
        let patch = serde_json::json!({
            "authenticated": true,
            "node_url": node_url,
            "secret_hex": hex::encode(crate::app::tests::TEST_SECRET),
            "handle": "test-handle",
        });
        assert!(apply_session_patch(&mut app, &tx, &patch), "patch applies");

        // The spawned pass bumps the counter before its first read; yield until
        // it has been scheduled rather than sleeping for a duration
        // (`e2e-latency-independent-assertions.md` § point 14 — the same
        // discipline off the e2e path).
        for _ in 0..1_000 {
            if app.alerts.sweep_passes_started() > before {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!(
            "the converge arm must dispatch a sweep pass: started stayed at {before} — \
             a re-established session would run on the previous session's alert verdict"
        );
    }

    const SECRET: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    fn a_record() -> PendingInviteRecord {
        PendingInviteRecord {
            nest_url: "https://nest.example".to_string(),
            handle: "joiner".to_string(),
            request_id: "req-1".to_string(),
            status_json: r#"{"PendingReview":{"request_id":"req-1","last_checked_ms":0}}"#
                .to_string(),
        }
    }

    /// `onboarding.md` § App-launch routing keys the pending-invite row on
    /// `identity + nest_url ABSENT + pending_invite`. Persisting a `nest_url`
    /// here would send the relaunch down the silent-challenge row instead, and
    /// the user would never see their submitted request again.
    #[test]
    fn a_submitted_invite_persists_the_identity_but_no_nest_url() {
        let app = test_app();
        persist_pending_invite(&app, SECRET, &a_record()).expect("persisting the invite");

        let (nest_url, secret_hex, _handle) = stored_account(&app).expect("an account was created");
        assert_eq!(
            nest_url, None,
            "a persisted nest_url diverts the relaunch onto the silent-challenge row"
        );
        assert_eq!(secret_hex.as_str(), SECRET);
        assert_eq!(
            launch_persistence(&app).load_pending_invite(),
            Some(a_record()),
            "the slot the LaunchMachine routes on must hold the submitted record"
        );
    }

    /// Append-mode `LoggedIn`: `persist_appended` adds the new identity WITH its
    /// nest_url, sets it active, and spends any pending slot — so the post-switch
    /// re-launch routes it straight to `Online`. Returns the new (now-active) actor.
    #[test]
    fn append_logged_in_persists_the_identity_active_with_its_nest_url() {
        let app = test_app();
        let outcome = fauna_onboarding_machine::WizardOutcome::LoggedIn {
            nest_url: "https://nest.example".to_string(),
            handle: "newbie@nest.example".to_string(),
        };
        let actor = persist_appended(&app, &outcome, SECRET).expect("persisting the append");

        let (nest_url, secret_hex, _handle) = stored_account(&app).expect("an account was created");
        assert_eq!(
            nest_url,
            Some("https://nest.example".to_string()),
            "a LoggedIn append carries its nest_url so the relaunch goes Online, not the wizard"
        );
        assert_eq!(secret_hex.as_str(), SECRET);
        assert_eq!(
            registry(&app).active().as_deref(),
            Some(actor.as_str()),
            "the appended account is now active (the switch target)"
        );
        assert_eq!(
            launch_persistence(&app).load_pending_invite(),
            None,
            "reaching LoggedIn spends any pending-invite slot"
        );
    }

    /// The append-mode pending-invite adoption, now triggered by the **submit
    /// return** instead of the retired `InviteSubmitted` exit (2026-08-12).
    ///
    /// The invariants are unchanged and are what this pins: the same
    /// `persist_pending_invite` path the no-session flow uses, a new identity
    /// with **no nest_url** on the slot (a nest_url would divert the relaunch
    /// onto the silent-challenge routing row instead of the invite-request one),
    /// the slot written, and the appended account active — `onboarding.md`
    /// § Multi-account's "register the append identity, write its per-actor
    /// pending-invite slot, switch to it". Only the trigger moved, so the test
    /// moved with it rather than retiring.
    #[test]
    fn append_pending_invite_persists_the_pending_slot_with_no_nest_url() {
        let app = test_app();
        let slot = fauna_onboarding_machine::PendingInviteSlot {
            nest_url: "https://joinme.example".to_string(),
            handle: "joiner".to_string(),
            request_id: "req-append".to_string(),
            status_json: r#"{"PendingReview":{"request_id":"req-append","last_checked_ms":0}}"#
                .to_string(),
        };
        let record = PendingInviteRecord {
            nest_url: slot.nest_url.clone(),
            handle: slot.handle.clone(),
            request_id: slot.request_id.clone(),
            status_json: slot.status_json.clone(),
        };
        persist_pending_invite(&app, SECRET, &record).expect("persisting the append");
        let actor = registry(&app).active().expect("an active account");

        let (nest_url, secret_hex, _handle) = stored_account(&app).expect("an account was created");
        assert_eq!(
            nest_url, None,
            "an appended invite must leave the nest_url off the slot (invite-request routing)"
        );
        assert_eq!(secret_hex.as_str(), SECRET);
        assert_eq!(
            registry(&app).active().as_deref(),
            Some(actor.as_str()),
            "the appended (pending-invite) account is now active"
        );
        assert!(
            launch_persistence(&app).load_pending_invite().is_some(),
            "the pending-invite slot the LaunchMachine resumes on must be written"
        );
    }

    /// **The submit return actually writes the slot.**
    ///
    /// This is the pin for the failure mode the whole 2026-08-12 retirement was
    /// shaped around: deleting `WizardOutcome::InviteSubmitted` compiles
    /// cleanly and *silently stops the app persisting*, and nothing else notices
    /// — the same-session journey still works end to end, because the slot only
    /// matters on a RELAUNCH. `test_pending_invite_journey.py` (the real approve
    /// drive) passes either way, which is exactly why it cannot be the coverage
    /// here.
    ///
    /// So this asserts the write itself, through the production path
    /// (`wizard::persist_pending_invite_slot`), from a machine seeded into
    /// `PendingReview` the way a relaunch or a submit leaves it.
    #[test]
    fn the_submit_return_writes_the_pending_invite_slot() {
        let mut app = test_app();
        app.wizard.machine.seed_identity(SECRET.to_string());
        app.wizard.machine.seed_pending_invite(
            "https://nest.example".to_string(),
            "joiner".to_string(),
            "req-1".to_string(),
            r#"{"PendingReview":{"request_id":"req-1","last_checked_ms":0}}"#.to_string(),
        );

        crate::wizard::persist_pending_invite_slot(&mut app);

        let stored = launch_persistence(&app)
            .load_pending_invite()
            .expect("the submit return must write the resume slot");
        assert_eq!(stored.request_id, "req-1");
        assert_eq!(stored.handle, "joiner");
        assert_eq!(stored.nest_url, "https://nest.example");
        // The account carries NO nest_url — a saved one would divert the
        // relaunch onto the silent-challenge routing row instead of the
        // invite-request one (`onboarding.md` § App-launch routing).
        let (nest_url, _secret, _handle) = stored_account(&app).expect("an account was created");
        assert_eq!(nest_url, None);
    }

    /// On a bound launch, `stored_account`'s secret must name the BOUND
    /// account — never `registry(app).active()`, which can name a
    /// *different* account (`account-scoping.md` § Concurrent instances —
    /// "bound reads bypass the active pointer entirely"). Two call sites
    /// (`launch::verify_superseded_successor`, `session::run_silent_sign_in`)
    /// used to read the actor id from `active()` *beside* the bound secret,
    /// sending an anonymous succession lookup (or a cache write) for the
    /// wrong identity. Both now derive the
    /// actor id through the shared `session_actor_id` seam, from the same
    /// secret `stored_account` hands back — this pins that seam, which is the
    /// one point both call sites route through (verify-back
    /// : pinning the seam alone
    /// does not prove a call site still routes through it — the composed
    /// property, not just this predicate, is what a caller-side regression
    /// would need to violate).
    ///
    /// This exercises `registry.session_material` — the exact accessor
    /// `stored_account`'s bound branch calls once `resolve_launch_binding()`
    /// names the bound account — rather than the process-global launch
    /// binding itself: that cell has no per-test "clear" path, and setting it
    /// here would leak a bound state into whichever of this binary's other
    /// ~1700 tests race this one, corrupting their own (unrelated)
    /// `stored_account`/`launch_persistence` reads. `session_material` is
    /// per-registry (backed by this test's own throwaway store), so it needs
    /// no such isolation.
    #[test]
    fn a_bound_accounts_secret_derives_its_own_actor_not_active() {
        let app = test_app();
        let (active_actor, bound_actor) = seed_two_accounts(&app);
        assert_ne!(
            active_actor, bound_actor,
            "the fixture must seed two genuinely distinct accounts"
        );

        let material = registry(&app)
            .session_material(&bound_actor)
            .expect("the bound account resolves");
        let derived_actor = session_actor_id(&material.secret_hex)
            .expect("a valid stored secret")
            .actor_id_hex();

        assert_eq!(
            derived_actor, bound_actor,
            "the actor derived from the bound account's OWN secret, through the \
             same seam verify_superseded_successor and run_silent_sign_in both \
             call, must name that account"
        );
        assert_eq!(
            registry(&app).active().as_deref(),
            Some(active_actor.as_str()),
            "active() must still name the OTHER account — the two reads \
             genuinely diverge, which is exactly why mixing them was unsafe"
        );
    }

    /// The write happens once per request, not once per main-loop wake.
    ///
    /// tui reaches this from two input paths that both funnel through the main
    /// loop's post-action check, so it runs on *every* wake. Without the
    /// `pending_invite_persisted` guard it rewrites the slot continuously —
    /// the shape `onboarding.md` recorded for the retired exit ("re-writes the
    /// slot on every UI wake").
    ///
    /// ⚠ **How this is made observable matters.** Asserting "the slot is still
    /// there" after a second call proves NOTHING: the rewrite is idempotent in
    /// data, so a missing guard looks identical. (The first version of this test
    /// did exactly that and survived its own mutant.) So the second wake runs
    /// against a slot that was cleared out from under it — if the guard holds,
    /// the clear survives; if the write re-enters, it comes back.
    #[test]
    fn the_slot_write_is_idempotent_per_request() {
        let mut app = test_app();
        app.wizard.machine.seed_identity(SECRET.to_string());
        app.wizard.machine.seed_pending_invite(
            "https://nest.example".to_string(),
            "joiner".to_string(),
            "req-1".to_string(),
            r#"{"PendingReview":{"request_id":"req-1","last_checked_ms":0}}"#.to_string(),
        );

        crate::wizard::persist_pending_invite_slot(&mut app);
        assert_eq!(
            app.wizard.pending_invite_persisted.as_deref(),
            Some("req-1")
        );
        assert!(launch_persistence(&app).load_pending_invite().is_some());

        // Something else consumes the slot (the real analogue: reaching
        // LoggedIn spends it). The machine is untouched, so it is still in
        // PendingReview and a guard-less implementation would write it straight
        // back on the next wake.
        let actor = registry(&app).active().expect("an active account");
        registry(&app).clear_pending_invite(&actor);
        assert!(launch_persistence(&app).load_pending_invite().is_none());

        crate::wizard::persist_pending_invite_slot(&mut app);
        crate::wizard::persist_pending_invite_slot(&mut app);

        assert!(
            launch_persistence(&app).load_pending_invite().is_none(),
            "a repeat wake re-entered the write path — the once-per-request \
             guard is gone, so tui is back to rewriting the slot on every wake"
        );
    }

    /// The end-to-end routing assertion: the *real* `LaunchMachine`, fed the
    /// store `persist_pending_invite` just wrote, must land on `InviteRequest`.
    /// Pins the seam that was dead before — nothing wrote the registry's
    /// pending-invite slot, so this row could never fire on any client.
    #[tokio::test]
    async fn a_persisted_invite_routes_the_real_launch_machine_to_invite_request() {
        let app = test_app();
        persist_pending_invite(&app, SECRET, &a_record()).expect("persisting the invite");

        let machine = fauna_launch_machine::LaunchMachine::new(
            Arc::new(fauna_launch_machine::NullObserver),
            Arc::new(launch_persistence(&app)),
        );
        machine.start().await;

        assert_eq!(
            machine.snapshot().phase,
            fauna_launch_machine::LaunchPhase::WizardAt {
                entry: fauna_launch_machine::LaunchWizardEntry::InviteRequest
            },
            "identity + no nest_url + pending invite must route to InviteRequest \
             (no network: the silent-challenge row needs a nest_url)"
        );
    }

    /// Reaching `LoggedIn` spends the invite. Leaving the slot behind would send
    /// the *next* launch back to `invite_request` for a request already granted.
    #[test]
    fn persisting_a_logged_in_account_is_what_clears_the_invite() {
        let app = test_app();
        persist_pending_invite(&app, SECRET, &a_record()).expect("persisting the invite");
        assert!(launch_persistence(&app).load_pending_invite().is_some());

        // The registry half of `adopt` (its `establish` half needs a live nest).
        let registry = registry(&app);
        let actor_id = registry
            .add_account(SECRET, Some("https://nest.example"), None)
            .expect("adopting the account");
        registry.clear_pending_invite(&actor_id);

        assert_eq!(
            launch_persistence(&app).load_pending_invite(),
            None,
            "a spent invite must not survive into the next launch"
        );
    }

    fn a_dns_record() -> AwaitingDnsRecord {
        AwaitingDnsRecord {
            nest_url: "https://nest.example".to_string(),
            handle: "admin".to_string(),
            dns_records_json:
                r#"[{"record_type":"A","name":"@","value":"203.0.113.7","ttl":300,"priority":null}]"#
                    .to_string(),
            claim_code: "claim-abc".to_string(),
            reach_ipv4: None,
            nest_actor_id: None,
        }
    }

    /// The whole client-side chain, through a **real** `LaunchMachine`: the
    /// deferred-DNS exit writes the registry slot, and the machine — reading that
    /// same slot through `RegistryLaunchPersistence` — routes the next launch to
    /// the "Almost ready" surface on its own. No pre-machine branch in the client.
    #[tokio::test]
    async fn a_persisted_awaiting_dns_routes_the_real_launch_machine_to_the_surface() {
        let app = test_app();
        persist_awaiting_dns(&app, SECRET, &a_dns_record()).expect("persisting the deferred nest");

        let machine = fauna_launch_machine::LaunchMachine::new(
            Arc::new(fauna_launch_machine::NullObserver),
            Arc::new(launch_persistence(&app)),
        );
        machine.start().await;

        assert_eq!(
            machine.snapshot().phase,
            fauna_launch_machine::LaunchPhase::WizardAt {
                entry: fauna_launch_machine::LaunchWizardEntry::AwaitingManualDns
            },
            "identity + an awaiting-manual-dns slot must route to the 'Almost ready' surface"
        );
    }

    /// The deferred-DNS exit persists the identity but **no `nest_url`**: the nest
    /// is provisioned, not yet claimed, so there is nothing a silent challenge
    /// could authenticate against. (The row outranks the silent-challenge row
    /// regardless — this keeps the store honest about what was authenticated.)
    #[test]
    fn a_deferred_dns_nest_persists_the_identity_but_no_nest_url() {
        let app = test_app();
        persist_awaiting_dns(&app, SECRET, &a_dns_record()).expect("persisting the deferred nest");

        let (nest_url, secret_hex, _handle) = stored_account(&app).expect("an active account");
        assert_eq!(secret_hex.as_str(), SECRET);
        assert_eq!(nest_url, None, "no nest_url until the claim lands");
        assert_eq!(
            launch_persistence(&app).load_awaiting_dns(),
            Some(a_dns_record()),
            "the record round-trips through the registry's opaque-JSON slot"
        );
    }

    // ── resolve_self_address (the SMTP rail's `From:` resolution) ───────────

    #[test]
    fn self_address_is_the_registry_handle_at_the_registry_domain() {
        // The returning-user shape: the silent challenge cached BOTH the nest's
        // handle and its domain on the registry entry → `<handle>@<domain>`.
        assert_eq!(
            resolve_self_address("alice", Some("alice"), Some("fauna.test")),
            Some("alice@fauna.test".to_string()),
        );
    }

    #[test]
    fn self_address_prefers_the_registry_handle_over_the_session_one() {
        // The nest's whoami handle wins when present (production); the session
        // handle is only the fallback.
        assert_eq!(
            resolve_self_address("e2e-user", Some("bob"), Some("fauna.test")),
            Some("bob@fauna.test".to_string()),
        );
    }

    #[test]
    fn self_address_is_none_without_a_domain() {
        // No cached domain yet (the gap this whole change closes): no address,
        // rather than a malformed `alice@` — the SMTP rail keeps its empty From.
        assert_eq!(resolve_self_address("alice", None, None), None);
        assert_eq!(resolve_self_address("alice", Some("bob"), Some("")), None);
    }

    #[test]
    fn self_address_passes_through_a_handle_that_already_carries_a_domain() {
        // The onboarding-wizard path: the entered handle is already a full
        // address, so it IS the From regardless of the registry entry.
        assert_eq!(
            resolve_self_address("alice@fauna.test", None, None),
            Some("alice@fauna.test".to_string()),
        );
        assert_eq!(
            resolve_self_address("alice@fauna.test", Some("bob"), Some("other.test")),
            Some("alice@fauna.test".to_string()),
        );
    }

    #[test]
    fn self_address_is_none_when_the_nest_reports_no_handle() {
        // A registered actor whose nest handle is empty has NO address it may
        // claim, and the session handle is not a substitute: the nest does
        // sender-handle verification on every `fauna.email.send`
        // (mail-app-surface.md § First-party client send), so `<session-handle>@<domain>`
        // is an address this actor does not own and the nest refuses it.
        //
        // The empty-vs-absent distinction is decidable here precisely because
        // the domain is present: `entry_domain` is written only by the silent
        // refresh (`spawn_domain_refresh` → `silent_refresh`), so reaching this
        // line proves the refresh has run and the nest genuinely reports no
        // handle — this is not a not-yet-loaded race.
        assert_eq!(
            resolve_self_address("e2e-user", Some(""), Some("fauna.test")),
            None,
        );
        assert_eq!(
            resolve_self_address("e2e-user", None, Some("fauna.test")),
            None
        );
    }

    /// `session_self_address` resolves the HELD SESSION's own actor id
    /// (`app.session`, never `registry.active()`) — the fix for the bound-
    /// launch identity mixup: on a bound
    /// (coexisting secondary) instance the bound and active accounts
    /// genuinely differ, and resolving through `active()` would seed the
    /// session's `From:`/MLS routing domain from the WRONG account. Mirrors
    /// `a_bound_accounts_secret_derives_its_own_actor_not_active`'s shape:
    /// install a session over the non-active account, assert ITS
    /// handle/domain resolve while `active()` still names the other — no
    /// global binding cell involved, since the function reads its own held
    /// session rather than taking a caller-chosen id. There is no longer a
    /// call site left to mutate into reading `active()` instead: this is the
    /// only place the account is chosen.
    #[test]
    fn session_self_address_resolves_the_held_session_not_the_active_account() {
        let mut app = test_app();
        let (active_actor, other_actor) = seed_two_accounts(&app);
        let registry = registry(&app);
        registry
            .update_cache(&active_actor, Some("alice"), Some("active.test"), None)
            .expect("cache the active account's handle/domain");
        registry
            .update_cache(&other_actor, Some("bob"), Some("bound.test"), None)
            .expect("cache the other account's handle/domain");

        app.session = Some(Session {
            handle: "bob".to_string(),
            actor_id: other_actor.clone(),
            client: NestClient::new("http://127.0.0.1:1".to_string(), ActorKeypair::generate()),
        });

        let addr = session_self_address(&app)
            .expect("the held session's own cached handle/domain resolves");

        assert_eq!(
            addr, "bob@bound.test",
            "the HELD SESSION's own handle/domain must resolve"
        );
        assert_eq!(
            registry.active().as_deref(),
            Some(active_actor.as_str()),
            "active() still names the OTHER account — proves the resolution \
             didn't fall back to it"
        );
    }

    /// With no session installed, `session_self_address` has nothing to
    /// resolve — the single-accessor contract's other half: absent state
    /// yields `None`, never a panic or a fallback to `active()`.
    #[test]
    fn session_self_address_is_none_with_no_session_installed() {
        let app = test_app();
        seed_two_accounts(&app);
        assert_eq!(session_self_address(&app), None);
    }

    // ── Account switcher (long-term-store.md § Multi-account evolution) ─────

    const SECRET_B: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    /// Seed two accounts into the app's registry and pin the first active.
    /// Returns `(active_actor, other_actor)`.
    fn seed_two_accounts(app: &App) -> (String, String) {
        let registry = registry(app);
        let a = registry
            .add_account(SECRET, Some("https://nest.example"), None)
            .expect("seed account a");
        let b = registry
            .add_account(SECRET_B, Some("https://nest.example"), None)
            .expect("seed account b");
        registry.set_active(&a).expect("pin a active");
        (a, b)
    }

    #[test]
    fn switcher_rows_project_active_and_the_stage2_flag() {
        let app = test_app();
        let (a, b) = seed_two_accounts(&app);
        set_account_require_confirm(&app, &b, true);

        let rows = switcher_rows_serving(&registry(&app), Some(&a));
        assert_eq!(rows.len(), 2, "both seeded accounts render");
        let row_a = rows.iter().find(|r| r.actor_id == a).expect("row a");
        let row_b = rows.iter().find(|r| r.actor_id == b).expect("row b");
        assert!(row_a.is_active, "the active account is marked active");
        assert!(!row_b.is_active);
        assert!(!row_a.require_confirm, "a is unflagged");
        assert!(
            row_b.require_confirm,
            "b's Stage-2 flag reads back on its row"
        );
        assert!(!row_a.label.is_empty(), "the row carries a display label");
    }

    /// ⚠ **On a bound instance the switcher marks the account it SERVES**. A bound secondary never moves the registry's
    /// active pointer, so keyed on `registry.active()` the served account read
    /// as "not active" and was offered a remove button — which erased the
    /// stores this very process runs from. The row that paints the active
    /// indicator (and so no switch/remove affordance) is the session's.
    #[test]
    fn switcher_marks_the_served_account_not_the_registry_active_one() {
        let app = test_app();
        let (a, b) = seed_two_accounts(&app);
        let registry = registry(&app);
        assert_eq!(registry.active().as_deref(), Some(a.as_str()));

        let rows = switcher_rows_serving(&registry, Some(&b));
        let row_a = rows.iter().find(|r| r.actor_id == a).expect("row a");
        let row_b = rows.iter().find(|r| r.actor_id == b).expect("row b");
        assert!(row_b.is_active, "the served account is the one in use here");
        assert!(
            !row_a.is_active,
            "the registry's active account is only another account to this window"
        );
    }

    #[test]
    fn require_confirm_reads_fresh_and_pins_the_user_choice() {
        let app = test_app();
        let (_a, b) = seed_two_accounts(&app);
        assert!(!account_requires_confirm(&app, &b), "off by default");
        set_account_require_confirm(&app, &b, true);
        assert!(
            account_requires_confirm(&app, &b),
            "a fresh read sees the write (never a stale snapshot)"
        );
        // The write also marks `require_confirm_user_set`, which is what makes an
        // explicit choice stick against the admin auto-default.
        let entry = registry(&app)
            .list()
            .into_iter()
            .find(|e| e.actor_id == b)
            .expect("entry b");
        assert!(
            entry.require_confirm_user_set,
            "an explicit toggle pins the user's choice"
        );
    }

    #[test]
    fn remove_account_shrinks_the_switcher() {
        let app = test_app();
        let (a, b) = seed_two_accounts(&app);
        remove_account(&app, &b).expect("remove b");
        let rows = account_switcher_rows(&app);
        assert_eq!(rows.len(), 1, "the removed account is gone");
        assert_eq!(rows[0].actor_id, a, "the survivor is the one kept");
    }

    /// The Stage-2 gate: a switch to a **flagged** account arms the in-app
    /// re-auth prompt and mutates nothing; declining (Escape/cancel) is a pure
    /// no-op. Runtime-free — the flagged branch returns before any teardown.
    #[test]
    fn a_flagged_switch_arms_the_prompt_and_decline_is_a_no_op() {
        use crate::settings::{Action, apply_local};
        let mut app = test_app();
        let (a, b) = seed_two_accounts(&app);
        set_account_require_confirm(&app, &b, true);
        crate::settings::refresh_accounts(&mut app);

        let op = apply_local(&mut app, Action::SwitchAccount(b.clone()));
        assert!(op.is_none(), "the switch is a local gesture");
        assert!(
            app.settings.reauth_prompt_open(),
            "a flagged target arms the re-auth prompt"
        );
        assert_eq!(
            registry(&app).active().as_deref(),
            Some(a.as_str()),
            "the gate has NOT switched yet — a is still active"
        );

        let op = apply_local(&mut app, Action::ReauthCancel);
        assert!(op.is_none());
        assert!(
            !app.settings.reauth_prompt_open(),
            "decline clears the prompt"
        );
        assert_eq!(
            registry(&app).active().as_deref(),
            Some(a.as_str()),
            "decline mutated nothing (pure no-op)"
        );
    }

    /// An **unflagged** target is not gated — the gate reads the flag fresh and
    /// finds it clear, so no prompt would arm (the switch itself tears the
    /// surface down + re-launches, exercised by the e2e). Pins the read the gate
    /// branches on.
    #[test]
    fn an_unflagged_target_is_not_gated() {
        let app = test_app();
        let (_a, b) = seed_two_accounts(&app);
        assert!(
            !account_requires_confirm(&app, &b),
            "an unflagged account switches straight through, no prompt"
        );
    }
}

#[cfg(test)]
mod predecessor_persistence_tests {
    use super::*;
    use crate::app::tests::test_app;
    use fauna_client_config::test_helpers::FakeSuccessionLedgerStore;
    use fauna_onboarding_machine::nest_api::RestoredPredecessorSeed;

    /// The roster read hands the UI **what is still open**, and only that — the
    /// one seam that populates the two review surfaces, re-read by the
    /// post-store-ready pass.
    ///
    /// (Declared residual: what is pinned here is the *function*, not the
    /// pass's **call** to it. The call sits inside `spawn_ledger_aftermath`'s
    /// `tokio::spawn`, which needs a real `NestClient`; the pass's own ordering
    /// is pinned in `fauna_client_recovery::ledger_aftermath`, and the wire by
    /// the tier_3 aftermath journey.)
    #[tokio::test]
    async fn the_roster_read_hands_the_ui_only_the_open_reviews() {
        use fauna_core::identity::ActorId;
        let person = ActorId([7u8; 32]);
        let answered = ActorId([8u8; 32]);
        let mut cfg = fauna_core::succession_ledger::SuccessionLedger::empty(ActorId([1u8; 32]));
        cfg.raise_member_reviews(
            [person, answered],
            ActorId([2u8; 32]),
            fauna_core::data::MemberUnattestedReason::CompromiseWindow,
        );
        cfg.decide_member_reviews_for(&answered, fauna_core::data::UnattestedVerdict::Kept);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        refresh_member_reviews(
            std::sync::Arc::new(FakeSuccessionLedgerStore::with(cfg)),
            &tx,
        )
        .await;

        let Some(UiMessage::Data(DataMessage::MemberReviews(roster))) = rx.recv().await else {
            panic!("the read must reach the UI as a MemberReviews message");
        };
        assert_eq!(
            roster.iter().map(|r| r.person).collect::<Vec<_>>(),
            vec![person],
            "the answered person must not come back: {roster:?}"
        );
    }

    fn entry(kp: &fauna_core::identity::ActorKeypair) -> RestoredPredecessorSeed {
        RestoredPredecessorSeed {
            actor_id_hex: fauna_core::hex32::encode(&kp.actor_id().0),
            seed_hex: fauna_core::hex32::encode(kp.secret_bytes()).into(),
        }
    }

    /// **A restored predecessor seed reaches the account registry — recovering
    /// it and dropping it would make the successor's seal pointless.**
    ///
    /// This is the far end of the device-loss race the successor's kit ceremony
    /// opened: the seal exists so a user with no devices left can get the
    /// predecessor material back, and the material is only *back* once it rests
    /// somewhere the re-seal driver can read it.
    /// **A restore's predecessor keys are resolvable BEFORE the session is
    /// built** — the persistence half of `adopt` lands them, so the one
    /// resolution `establish` makes (Media read custody, the drafts rails) sees
    /// them.
    ///
    /// Persisted after `adopt` instead, the seed reached the registry and not the
    /// session: a freshly restored device listed nothing of the corpus still
    /// sealed under the retired identity until a relaunch (tier_3,
    /// `test_a_kit_replaced_after_a_succession_still_restores_the_predecessor_corpus`,
    /// 2026-09-26). And the restored identity must still be the active one — a
    /// predecessor persisted first would claim `active`.
    ///
    /// Red-verify by dropping the `persist_restored_predecessors` call from
    /// `persist_logged_in_identity`.
    #[test]
    fn a_restored_logins_predecessor_keys_resolve_before_the_session_is_built() {
        let app = test_app();
        let restored = fauna_core::identity::ActorKeypair::generate();
        let predecessor = fauna_core::identity::ActorKeypair::generate();
        let restored_hex = fauna_core::hex32::encode(&restored.actor_id().0);

        persist_logged_in_identity(
            &app,
            &fauna_core::hex32::encode(restored.secret_bytes()),
            "https://nest.example",
            None,
            &[entry(&predecessor)],
        )
        .expect("the restored login persists");

        assert_eq!(
            registry(&app).active().as_deref(),
            Some(restored_hex.as_str()),
            "the RESTORED identity lands active, never its predecessor"
        );
        let keys = succession_predecessor_backup_keys(&app, &restored_hex);
        let expected = fauna_core::crypto::BackupKey::derive(predecessor.secret_bytes());
        assert!(
            keys.iter().any(|k| k.to_bytes() == expected.to_bytes()),
            "the predecessor's BackupKey must resolve for the restored identity by \
             the time `establish` reads it; got {} key(s)",
            keys.len()
        );
    }

    /// A sign-in on a full account list paints the shared "list is full" line —
    /// not the registry error's English `Display` inside the generic "couldn't
    /// save your new account" wrapper — and the account the user was on stays
    /// active (`long-term-store.md` § Multi-account evolution → *The index is
    /// bounded*).
    ///
    /// Red-verify by restoring `persist_account(&e.to_string())` in
    /// `persist_logged_in_identity`.
    #[test]
    fn a_sign_in_on_a_full_account_list_paints_the_list_full_line() {
        let app = test_app();
        let reg = registry(&app);
        let mut first = None;
        let refused_secret = loop {
            let kp = fauna_core::identity::ActorKeypair::generate();
            let secret = fauna_core::hex32::encode(kp.secret_bytes());
            match reg.add_account(&secret, None, None) {
                Ok(id) => {
                    first.get_or_insert(id);
                }
                Err(fauna_client_accounts::AccountError::IndexFull { .. }) => break secret,
                Err(e) => panic!("unexpected refusal while filling the list: {e}"),
            }
        };
        let first = first.expect("the list holds at least one account");
        reg.set_active(&first).expect("a bare account activates");

        let err =
            persist_logged_in_identity(&app, &refused_secret, "https://nest.example", None, &[])
                .expect_err("a sign-in the list cannot hold must not read as added");

        let want = fauna_client_accounts::add_refused_copy(
            &fauna_client_accounts::AccountError::IndexFull {
                needed: 0,
                limit: 0,
            },
        )
        .expect("a full list has a line")
        .resolve(fauna_i18n::strings::lookup);
        assert_eq!(err, want);
        assert_eq!(registry(&app).active(), Some(first));
    }

    #[test]
    fn a_restored_predecessor_seed_lands_in_the_account_registry() {
        let app = test_app();
        let predecessor = fauna_core::identity::ActorKeypair::generate();

        persist_restored_predecessors(&app, &[entry(&predecessor)]);

        let actor_hex = fauna_core::hex32::encode(&predecessor.actor_id().0);
        let stored = registry(&app)
            .secrets(&actor_hex)
            .expect("the predecessor's row rests after the restore");
        assert_eq!(
            stored.secret_hex.as_str(),
            fauna_core::hex32::encode(predecessor.secret_bytes()),
            "and it is that identity's real seed, readable by the re-seal driver"
        );
    }

    /// **The restored device learns which rows are predecessors, not just that
    /// the seeds exist.** The seed alone is not enough for the corpus re-seal:
    /// a device holding several accounts cannot offer them all as predecessors
    /// (an unrelated account's key opens that account's own device-local
    /// account state, so a pass fed the whole registry would fold a
    /// different account's deployment seeds into this one). The link is what
    /// makes "every predecessor in the registry" a safe query — and on a
    /// freshly-restored device this is the only place it can be reconstructed.
    #[test]
    fn a_restored_predecessor_is_linked_to_the_identity_it_precedes() {
        let app = test_app();
        let predecessor = fauna_core::identity::ActorKeypair::generate();
        // The ordering this function documents and depends on: the restored
        // identity is added (and so active) before its predecessors land.
        let restored_kp = fauna_core::identity::ActorKeypair::generate();
        let restored = registry(&app)
            .add_account(
                &fauna_core::hex32::encode(restored_kp.secret_bytes()),
                None,
                None,
            )
            .expect("the restored identity lands first");

        persist_restored_predecessors(&app, &[entry(&predecessor)]);

        let actor_hex = fauna_core::hex32::encode(&predecessor.actor_id().0);
        assert_eq!(
            registry(&app).predecessors_of(&restored),
            vec![actor_hex],
            "the recovered identity must read back as a predecessor of the \
             account it was recovered for"
        );
    }

    /// The ordinary onboarding path writes nothing — no stray rows, and no
    /// registry mutation on a flow that recovered no predecessors.
    #[test]
    fn an_ordinary_restore_writes_no_predecessor_rows() {
        let app = test_app();
        let before = registry(&app).list().len();

        persist_restored_predecessors(&app, &[]);

        assert_eq!(registry(&app).list().len(), before);
    }

    /// **A bad row does not cost the good ones.** The account is already back
    /// by the time this runs, so one unusable entry must never abort the rest —
    /// each seed is a separate identity's only remaining copy.
    #[test]
    fn one_unusable_predecessor_entry_does_not_stop_the_others() {
        let app = test_app();
        let good = fauna_core::identity::ActorKeypair::generate();
        let bad = RestoredPredecessorSeed {
            actor_id_hex: "a1".repeat(32),
            seed_hex: "not-hex".into(),
        };

        persist_restored_predecessors(&app, &[bad, entry(&good)]);

        assert!(
            registry(&app)
                .secrets(&fauna_core::hex32::encode(&good.actor_id().0))
                .is_some(),
            "the usable seed still landed"
        );
    }

    // -----------------------------------------------------------------------
    // The post-succession aftermath hook.
    // -----------------------------------------------------------------------

    /// Register `predecessor` as having been succeeded by `successor`, both as
    /// real registry rows — the state `adopt_successor` leaves behind.
    fn seed_succession(
        app: &App,
        predecessor: &fauna_core::identity::ActorKeypair,
        successor: &fauna_core::identity::ActorKeypair,
    ) -> String {
        let reg = registry(app);
        let old_hex = reg
            .add_account(
                &fauna_core::hex32::encode(predecessor.secret_bytes()),
                None,
                None,
            )
            .expect("predecessor row");
        let new_hex = reg
            .add_account(
                &fauna_core::hex32::encode(successor.secret_bytes()),
                None,
                None,
            )
            .expect("successor row");
        reg.record_succession(&old_hex, &new_hex)
            .expect("the succession link");
        new_hex
    }

    /// **The gate that keeps every ordinary login free.** An identity with no
    /// predecessors must not start a pass, and must not paint a progress line
    /// for work that does not exist — the pass only exists for a successor.
    #[tokio::test]
    async fn an_ordinary_identity_starts_no_reseal() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let app = test_app();
        let me = fauna_core::identity::ActorKeypair::generate();
        let my_hex = registry(&app)
            .add_account(&fauna_core::hex32::encode(me.secret_bytes()), None, None)
            .expect("my row");

        spawn_succession_aftermath(
            &app,
            NestClient::new("http://127.0.0.1:1".into(), me),
            &my_hex,
            &fauna_core::hex32::encode(
                fauna_core::identity::ActorKeypair::generate().secret_bytes(),
            ),
            &tx,
        );

        assert!(
            rx.try_recv().is_err(),
            "no predecessors means no pass, and therefore no progress to report"
        );
    }

    /// A successor's surface hears the pass **start** before any outcome — that
    /// `Running` message is the "surfaced with progress" half of § Re-key scope,
    /// and without it the surface would stay blank through the whole round-trip
    /// and only ever show a result.
    ///
    /// The *ordering* claim itself is no longer tui's: since the aftermath's
    /// order was lifted into `fauna_client_recovery::aftermath`, that module's
    /// own `the_legs_report_in_the_order_the_ordering_rules_require` is what
    /// pins `drafts:running` first, for all seven apps at once.
    /// What is asserted here is tui's remaining half — the glue spawns that
    /// driver and pipes its progress into the UI channel.
    #[tokio::test]
    async fn a_successors_surface_hears_the_pass_start_first() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let app = test_app();
        let old = fauna_core::identity::ActorKeypair::generate();
        let new = fauna_core::identity::ActorKeypair::generate();
        let new_hex = seed_succession(&app, &old, &new);
        let secret_hex = fauna_core::hex32::encode(new.secret_bytes());

        spawn_succession_aftermath(
            &app,
            // Never connected, so the spawned pass cannot reach a nest — this
            // asserts on the first announcement, not on the outcome.
            NestClient::new("http://127.0.0.1:1".into(), new),
            &new_hex,
            &secret_hex,
            &tx,
        );

        // The announcement rides the spawned pass now that the shared driver
        // makes it, so this waits for the first message rather than polling
        // once. A generous ceiling, never a settle-sleep: the assertion is on
        // *which* message arrives first, not on how soon (convention 14). The
        // pass can never reach a nest — port 1, refused — so nothing but the
        // start can beat it to the channel.
        let first = tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv())
            .await
            .expect("a successor's surface hears the pass start")
            .expect("the aftermath's sink outlives its own first message");
        match first {
            UiMessage::Data(DataMessage::DraftsResealProgress(progress)) => assert_eq!(
                progress,
                fauna_client_drafts::DraftsResealProgress::Running,
                "the first thing a successor's surface hears is that the pass started"
            ),
            other => panic!("expected a Running progress message, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // The member-review roster the aftermath writes down.
    // -----------------------------------------------------------------------

    fn swept(unattested: Vec<Vec<u8>>) -> crate::settings::SweepStatus {
        use fauna_client_recovery::{GroupSweepOutcome, GroupSweepState, SweepReport};
        let mut report = SweepReport::default();
        for (i, group) in unattested.into_iter().enumerate() {
            report.outcomes.push(GroupSweepOutcome {
                channel_id: fauna_mls::types::ChannelId([i as u8; 32]),
                state: GroupSweepState::Swept,
                unattested_members: group
                    .into_iter()
                    .map(|b| fauna_core::identity::ActorId([b; 32]))
                    .collect(),
            });
        }
        crate::settings::SweepStatus::Ran(Box::new(report))
    }

    /// **The join.** What the aftermath writes down is the sweep's own roster,
    /// deduplicated across groups — not a count, not a re-derivation. A
    /// correspondent shared between two groups is one person to adjudicate.
    #[test]
    fn the_review_roster_is_the_sweeps_own_deduplicated_list() {
        let mut app = test_app();
        assert!(
            succession_review_roster(&app).is_empty(),
            "an ordinary session swept nothing"
        );

        app.succession_sweep = Some(swept(vec![vec![7, 8], vec![8, 9]]));
        let roster = succession_review_roster(&app);
        assert_eq!(
            roster,
            vec![
                fauna_core::identity::ActorId([7; 32]),
                fauna_core::identity::ActorId([8; 32]),
                fauna_core::identity::ActorId([9; 32]),
            ],
            "three people, asked about once each"
        );
    }

    /// The two arms that observed nothing must report nothing. A sweep that
    /// never ran has no roster — and inventing an empty one is fine, while
    /// inventing a *non*-empty one from anywhere else would flag people this
    /// ceremony never looked at.
    #[test]
    fn a_sweep_that_never_ran_owes_no_roster() {
        let mut app = test_app();

        app.succession_sweep = Some(crate::settings::SweepStatus::NoEngine);
        assert!(succession_review_roster(&app).is_empty());

        app.succession_sweep = Some(crate::settings::SweepStatus::Failed("nest down".into()));
        assert!(succession_review_roster(&app).is_empty());
    }

    /// A swept ceremony that flagged nobody is the healthy case, and it must
    /// stay distinguishable from "did not sweep" only in that both owe an empty
    /// roster — the driver's own `NothingToRaise` arm is what keeps it free.
    #[test]
    fn a_clean_sweep_owes_an_empty_roster() {
        let mut app = test_app();
        app.succession_sweep = Some(swept(vec![vec![]]));
        assert!(succession_review_roster(&app).is_empty());
    }
}
