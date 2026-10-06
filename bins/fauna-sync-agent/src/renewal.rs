//! Agent-side bearer renewal — `sync-agent.md` § Credential model (ratified
//! 2026-07-19).
//!
//! App-dead, the agent's bearer (a 1-hour session token) would expire and
//! silently stop sync. This loop keeps it fresh: near expiry it mints a
//! successor over the pre-identity `fauna.auth.device_handshake` kind, signing
//! with a **device key** — never the identity seed. The app-pushed
//! `RefreshBearer` IPC remains the immediate path while the app runs; this loop
//! is the app-absent safety net, so it re-reads the shared capability slot every
//! wake and simply finds nothing to do when the app already refreshed.
//!
//! **Which device key: the store principal, alone** — the machine's writer key
//! in the shared T10 slot, read load-only (`sync-agent-credentials.md`
//! § Credential model → the RULED 2026-09-28 block, decision 1). It is the
//! machine's only renewal credential; see [`principal_signing_key`].
//!
//! Failure posture: a failed mint backs off (network down, nest restarting)
//! and retries, because giving up on a transient is exactly the silent-stop
//! this exists to close. A persistent typed failure (a transport error or refusal) is retried on the
//! same backoff, which caps the cost and app-pushed refresh still works. **The one
//! terminal answer is `fauna.auth.not_registered` on the principal's key**: the
//! nest holds no grant for this machine any more (deleted from the devices
//! list, a factory-reset nest, an un-enrolled machine), so no retry can
//! succeed. The loop then drops the bearer ([`crate::bearer::mark_renewal_refused`])
//! — which stops every client over the capability from dialling — and writes a
//! **refusal record** ([`crate::credentials::RefusalRecord`]) naming what was
//! refused (`sync-agent-credentials.md` § Credential model → *A refused
//! renewal is terminal*). Dialling and reporting are separate states from
//! there: an app's pushed bearer re-arms the slot's clients, but the pipe
//! reports `needs_reenrollment` for as long as the record stands, and only a
//! renewal that succeeds clears it. While it stands the loop asks the nest
//! again at once when the machine's evidence — nest, principal key,
//! registration latch — is something the record was not refused on (one probe
//! per distinct evidence), and otherwise only at the lead of a pushed bearer;
//! never on an empty bearer with unchanged evidence ([`decide`]).
//!
//! The loop plans on the slot bearer's expiry, but it also hears its readers:
//! a `401` to the slot's bearer, or `not_registered` on the device-principal
//! leg (same key), reaches it through [`crate::bearer::CapabilitySlot`] and
//! asks the nest at once — once per distinct bearer — so a rejected
//! credential is not redialled until the lead (same doc, *A rejected
//! credential asks at once*).

use std::sync::Arc;
use std::time::Duration;

use fauna_protocol::reconnect::Backoff;

use crate::credentials::RefusalRecord;
use crate::state::{IDLE_RECHECK_SECS, SyncServiceState};

/// Renew this long before the bearer's `expires_at`. Sized inside the
/// direct-auth session TTL with margin for clock skew + one retry round —
/// which is a relationship with a constant in another *binary*, so the value is
/// owned by [`fauna_protocol::auth::AGENT_RENEW_LEAD_SECS`] and the nest pins
/// the inequality at compile time beside its own `auth_core::TOKEN_TTL_SECS`.
/// At or above that TTL this loop mints forever; read the owner.
use fauna_protocol::auth::AGENT_RENEW_LEAD_SECS as RENEW_LEAD_SECS;

/// First retry delay after a failed mint; doubles up to [`BACKOFF_MAX_SECS`].
const BACKOFF_START_SECS: u64 = 60;
const BACKOFF_MAX_SECS: u64 = 900;

/// Ceiling on any single sleep, so the signed-out reconcile below is re-checked
/// at least this often (`sync-agent.md` § Credential model → *The signed-out
/// reconcile*).
///
/// Without it a provisioned agent sleeps until the renewal lead — up to ~55 min
/// on a fresh 1-hour bearer — and a sign-out whose message was lost would go
/// unnoticed for that whole window. Capping the sleep costs one credential-store
/// read per interval on an otherwise idle process, which is why this loop is the
/// right home for the check: it is the mechanism that keeps the capability
/// alive, so asking "may I still serve this?" before renewing is the same
/// question one layer earlier.
const SIGNED_OUT_RECHECK_SECS: u64 = 300;

/// Pure scheduling decision — what to do on this wake. Factored out of the
/// loop for direct testing.
#[derive(Debug, PartialEq)]
enum Wake {
    /// Bearer fresh: sleep until the renewal lead.
    Sleep(Duration),
    /// Inside the lead (or past expiry): mint now.
    RenewNow,
}

/// Decide from `(expires_at, now)` in unix seconds, both on this machine's
/// clock. The expiry is always known: every app's bearer source publishes the
/// deadline of the bearer it minted, and this loop's own mints anchor theirs at
/// receipt.
fn plan(expires_at: u64, now: u64) -> Wake {
    let renew_at = expires_at.saturating_sub(RENEW_LEAD_SECS);
    if now >= renew_at {
        Wake::RenewNow
    } else {
        Wake::Sleep(Duration::from_secs(renew_at - now))
    }
}

/// Whether a reader's report ([`crate::bearer::CapabilitySlot`]) asks the
/// nest now, given the slot's `current` bearer and the bearer the loop last
/// asked about after a report (`sync-agent-credentials.md` § Credential
/// model → *A rejected credential asks at once*). Asks at most once per
/// distinct slot bearer, never about an empty one (a refused slot dials
/// nothing), and never about a bearer the slot no longer holds.
fn rejection_asks_now(
    rejection: &crate::bearer::Rejection,
    current: &str,
    last_asked: Option<&str>,
) -> bool {
    use crate::bearer::Rejection;
    if current.is_empty() || last_asked == Some(current) {
        return false;
    }
    match rejection {
        Rejection::Bearer(rejected) => rejected.as_str() == current,
        Rejection::Principal => true,
    }
}

/// What one pass of the loop does — [`decide`]'s answer.
#[derive(Debug, PartialEq)]
enum Pass {
    /// Nothing to renew and nothing new to ask about.
    Idle,
    /// Bearer fresh: sleep until the renewal lead.
    Sleep(Duration),
    /// A refusal stands and the machine's evidence is news to it: mint now.
    Probe,
    /// A reader reported the nest rejecting the credential: mint now.
    AskOnReport,
    /// Inside the lead (or past expiry): mint now.
    RenewAtLead,
}

/// The whole per-wake decision, pure. In order: a standing refusal's
/// evidence probe, then a reader's report, then the ordinary plan
/// (`sync-agent-credentials.md` § Credential model → *A refused renewal is
/// terminal*, its *asks the nest again in exactly two cases* sub-bullet).
///
/// `evidence` is the machine's current evidence, read only while `refused`
/// stands; `None` when the slot holds no principal, which never probes. With
/// no news, an empty bearer idles — a refused agent no app has re-armed dials
/// nothing — and an armed one follows its report or its expiry as it would
/// with no record at all.
fn decide(
    refused: Option<&RefusalRecord>,
    evidence: Option<&RefusalRecord>,
    bearer_armed: bool,
    asks_now: bool,
    expires_at: u64,
    now: u64,
) -> Pass {
    if let (Some(refused), Some(evidence)) = (refused, evidence)
        && evidence.is_news_to(refused)
    {
        return Pass::Probe;
    }
    if !bearer_armed {
        return Pass::Idle;
    }
    if asks_now {
        return Pass::AskOnReport;
    }
    match plan(expires_at, now) {
        Wake::Sleep(d) => Pass::Sleep(d),
        Wake::RenewNow => Pass::RenewAtLead,
    }
}

fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs_or_zero() as u64
}

/// The renewal task. Spawned by `run_agent` beside the IPC server; exits when
/// `shutdown` flips.
pub async fn run(state: Arc<SyncServiceState>, mut shutdown: tokio::sync::watch::Receiver<bool>) {
    let mut backoff = Backoff::new(
        Duration::from_secs(BACKOFF_START_SECS),
        Duration::from_secs(BACKOFF_MAX_SECS),
    );
    // The bearer the loop last asked the nest about on a reader's report —
    // what bounds those asks to one per distinct slot bearer.
    let mut last_asked: Option<zeroize::Zeroizing<String>> = None;
    // Earliest next mint after a transient failure (see the `held` read below).
    let mut retry_after: Option<std::time::Instant> = None;
    loop {
        // The signed-out reconcile, ahead of any renewal decision: if the app
        // recorded a sign-out that this agent never heard about (the lost
        // un-provision message), stop serving now rather than minting the
        // account a fresh bearer.
        if signed_out_reconcile(&state).await {
            crate::pipe_server::unprovision_now(&state).await;
        }

        // Read the current slot. The signing key is not in it — it is the
        // store principal, read from the shared T10 slot (`read_principal`).
        //
        // A reader's report (a `401` to the slot's bearer, or the
        // device-principal leg refused) asks the nest now instead of at the
        // lead, once per distinct bearer (`rejection_asks_now`).
        let rejection = state.capability.take_rejection();
        let snapshot = {
            let cap = state.capability.read().await;
            cap.as_ref().and_then(|c| {
                let actor_id = c.actor_id_array()?;
                let asks_now = rejection.as_ref().is_some_and(|r| {
                    rejection_asks_now(
                        r,
                        &c.bearer.token,
                        last_asked.as_deref().map(String::as_str),
                    )
                });
                if asks_now {
                    last_asked = Some(zeroize::Zeroizing::new(c.bearer.token.clone()));
                }
                Some(Snapshot {
                    nest_url: c.nest_url.clone(),
                    actor_id,
                    // An empty bearer is the refused, dial-nothing slot.
                    bearer_armed: !crate::bearer::renewal_refused(c),
                    expires_at: c.bearer.expires_at,
                    asks_now,
                })
            })
        };

        let delay = match snapshot {
            None => Duration::from_secs(IDLE_RECHECK_SECS),
            Some(slot) => {
                // While a refusal stands, every wake reads the machine's
                // evidence — a credential-store read, which is why it lives
                // here and never on the status path.
                let refused = standing_refusal(&state);
                let mut principal = refused
                    .as_ref()
                    .and_then(|_| read_principal(&slot.nest_url, &slot.actor_id));
                let pass = decide(
                    refused.as_ref(),
                    principal.as_ref().map(|(_, evidence)| evidence),
                    slot.bearer_armed,
                    slot.asks_now,
                    slot.expires_at,
                    now_secs(),
                );
                // A failed mint's backoff holds across early wakes (a pushed
                // bearer, a reader's report), so a transient never turns a
                // wake into a mint.
                let held = retry_after
                    .and_then(|at| at.checked_duration_since(std::time::Instant::now()))
                    .filter(|left| !left.is_zero());
                match (pass, held) {
                    (Pass::Idle, _) => Duration::from_secs(IDLE_RECHECK_SECS),
                    (Pass::Sleep(d), _) => d,
                    (_, Some(left)) => left,
                    (pass, None) => {
                        match pass {
                            Pass::Probe => tracing::info!(
                                "this machine's enrollment evidence changed since the nest \
                                 refused its renewal; asking again now"
                            ),
                            Pass::AskOnReport => tracing::info!(
                                "the nest rejected this agent's credential on a live \
                                 connection; asking for a fresh bearer now rather than at \
                                 the renewal lead"
                            ),
                            _ => {}
                        }
                        match principal
                            .take()
                            .or_else(|| read_principal(&slot.nest_url, &slot.actor_id))
                        {
                            // No principal in the slot: nothing this loop can do
                            // until an app enrolls this machine. Same idle
                            // posture as an unprovisioned slot — never a mint.
                            None => Duration::from_secs(IDLE_RECHECK_SECS),
                            Some((key, evidence)) => {
                                match mint_over_principal(&slot.nest_url, slot.actor_id, key).await
                                {
                                    Ok(minted) => {
                                        let expires_at = minted.expires_at;
                                        adopt_minted(&state, minted.token, expires_at).await;
                                        tracing::info!(
                                            expires_at,
                                            "bearer self-renewed over device handshake"
                                        );
                                        // An app may have enrolled this machine
                                        // since the last refresh, which is exactly
                                        // what the mint above just proved.
                                        refresh_store_principal_presence(&state).await;
                                        backoff.reset();
                                        retry_after = None;
                                        // Immediately re-plan off the fresh expiry.
                                        Duration::from_secs(1)
                                    }
                                    Err(e) if e.refused => {
                                        tracing::error!(
                                            error = %e.message,
                                            "bearer self-renewal refused: the nest holds no grant \
                                             for this machine (fauna.auth.not_registered on the \
                                             store principal) — dropping the bearer and reporting \
                                             not enrolled until a renewal succeeds"
                                        );
                                        refuse_renewal(&state, evidence).await;
                                        backoff.reset();
                                        retry_after = None;
                                        Duration::from_secs(IDLE_RECHECK_SECS)
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            error = %e.message,
                                            retry_in_secs = backoff.ceiling().as_secs(),
                                            "bearer self-renewal failed; will retry"
                                        );
                                        let d = backoff.ceiling();
                                        backoff.grow();
                                        retry_after = Some(
                                            std::time::Instant::now()
                                                + d.min(Duration::from_secs(
                                                    SIGNED_OUT_RECHECK_SECS,
                                                )),
                                        );
                                        d
                                    }
                                }
                            }
                        }
                    }
                }
            }
        };

        // Never sleep past the reconcile's re-check interval.
        let delay = delay.min(Duration::from_secs(SIGNED_OUT_RECHECK_SECS));

        // Whatever this pass decided — a minted bearer, the terminal refusal,
        // a transient, or nothing to ask — readers waiting on a report read
        // the slot now.
        if rejection.is_some() {
            state.capability.answer();
        }

        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = state.capability.rejection_reported() => {}
            _ = state.renewal_wake.notified() => {}
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

/// The one device key this machine offers `fauna.auth.device_handshake`: the
/// **store principal**, the writer key in the shared T10 slot
/// (`sync-agent-credentials.md` § Credential model → the RULED 2026-09-28
/// block, decision 1). It is the machine's only renewal credential, so the
/// user's one delete gesture on the machine's row ends app-dead renewal by
/// construction — there is no second key to fall through to.
///
/// Never mints: [`load_writer_key`](fauna_sync_engine::principal_bundle::load_writer_key)
/// is load-only by construction, because a minted writer identity would be one
/// no nest has a grant for *and* a second writer for a store the app has not
/// enrolled yet. An empty slot yields no candidate.
///
/// The T10 slot is passed in rather than constructed here so a test can hand it
/// a file-backend store: `production_credential_store()` resolves the OS keyring
/// when no `FAUNA_E2E_CREDENTIAL_DIR` is set, and a unit test that built one
/// internally would read (and a careless one would write) the developer's real
/// Secret Service / Keychain — testing.md § point 10.
fn principal_signing_key(
    credentials: &fauna_credential_store::CredentialStore,
    actor_id: &[u8; 32],
) -> Option<ed25519_dalek::SigningKey> {
    fauna_sync_engine::principal_bundle::load_writer_key(credentials, &hex::encode(actor_id))
}

/// A failed mint: the error, and whether the nest answered
/// `fauna.auth.not_registered` — the terminal case.
#[derive(Debug)]
struct MintFailure {
    message: String,
    refused: bool,
}

/// Mint a bearer over the store principal's key.
async fn mint_over_principal(
    nest_url: &str,
    actor_id: [u8; 32],
    key: ed25519_dalek::SigningKey,
) -> Result<fauna_anon_client::MintedBearer, MintFailure> {
    mint_with(key, |key| async move {
        fauna_anon_client::mint_bearer_over_device_handshake(nest_url, actor_id, &key).await
    })
    .await
}

/// [`mint_over_principal`] over any mint — split so the terminal
/// classification is testable without a nest: only a typed
/// `not_registered` is terminal; a transport failure is a transient.
async fn mint_with<F, Fut>(
    key: ed25519_dalek::SigningKey,
    mint: F,
) -> Result<fauna_anon_client::MintedBearer, MintFailure>
where
    F: FnOnce(ed25519_dalek::SigningKey) -> Fut,
    Fut: std::future::Future<
            Output = Result<fauna_anon_client::MintedBearer, fauna_anon_client::AnonClientError>,
        >,
{
    mint(key).await.map_err(|e| MintFailure {
        refused: matches!(
            &e,
            fauna_anon_client::AnonClientError::Rpc(r) if r.is_not_registered()
        ),
        message: format!("store principal: {e}"),
    })
}

/// What one pass reads off the capability slot.
struct Snapshot {
    nest_url: String,
    actor_id: [u8; 32],
    bearer_armed: bool,
    expires_at: u64,
    asks_now: bool,
}

/// The machine's renewal credential and the evidence a mint under it would
/// rest on, read from the production T10 slot. `None` when the slot holds no
/// principal — there is then nothing to mint with and nothing to probe.
fn read_principal(
    nest_url: &str,
    actor_id: &[u8; 32],
) -> Option<(ed25519_dalek::SigningKey, RefusalRecord)> {
    let credentials = Arc::new(fauna_sync_engine::account_runtime::production_credential_store());
    let store_dir = fauna_sync_engine::account_runtime::StoreRoot::platform()
        .store_dir(&hex::encode(actor_id))
        .ok();
    principal_evidence(credentials, store_dir, nest_url, actor_id)
}

/// [`read_principal`] over a caller-injected slot and store dir, for the same
/// reason [`principal_signing_key`] takes its store by parameter (testing.md
/// § point 10). An unresolvable store dir reads the latch as unregistered.
fn principal_evidence(
    credentials: Arc<fauna_credential_store::CredentialStore>,
    store_dir: Option<std::path::PathBuf>,
    nest_url: &str,
    actor_id: &[u8; 32],
) -> Option<(ed25519_dalek::SigningKey, RefusalRecord)> {
    let key = principal_signing_key(&credentials, actor_id)?;
    let grant_registered = store_dir
        .is_some_and(|dir| principal_is_registered(credentials, dir, &hex::encode(actor_id)));
    let evidence = RefusalRecord {
        nest_url: nest_url.to_owned(),
        principal_key: hex::encode(key.verifying_key().to_bytes()),
        grant_registered,
    };
    Some((key, evidence))
}

/// The refusal record standing over this agent, if any — the in-memory mirror
/// (`SyncServiceState::renewal_refusal`), so a field read.
pub(crate) fn standing_refusal(state: &SyncServiceState) -> Option<RefusalRecord> {
    state
        .renewal_refusal
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn set_refusal(state: &SyncServiceState, record: Option<RefusalRecord>) {
    *state
        .renewal_refusal
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = record;
}

/// Load the persisted refusal record into memory — the boot path, beside the
/// capability restore, so a refused agent still reports it after a restart.
pub(crate) fn restore_refusal(state: &SyncServiceState) {
    if let Some(store) = &state.credentials {
        set_refusal(state, crate::credentials::load_refusal(store));
    }
}

/// Drop the refusal record, memory and store — a renewal succeeded, or the
/// capability it described is being un-provisioned. Nothing else calls this:
/// a pushed bearer never clears the report.
pub(crate) fn forget_refusal(state: &SyncServiceState) {
    set_refusal(state, None);
    if let Some(store) = &state.credentials {
        crate::credentials::delete_refusal(store);
    }
}

/// Wake the loop to re-read the machine's evidence now, if a refusal stands —
/// called by the provision and `RefreshBearer` handlers, whose arrival is the
/// sign that an app is signed in and may just have re-enrolled this machine.
pub(crate) fn wake_if_refused(state: &SyncServiceState) {
    if standing_refusal(state).is_some() {
        state.renewal_wake.notify_one();
    }
}

/// Enter the terminal refused state (see the module doc): drop the bearer in
/// the shared slot, persist that, record `refused` — the evidence the nest
/// just refused, so the same evidence is never probed twice — and wake the
/// account host so its mount, and the device-principal leg that rides the
/// same dead grant, comes down.
async fn refuse_renewal(state: &Arc<SyncServiceState>, refused: RefusalRecord) {
    {
        let mut guard = state.capability.write().await;
        if let Some(cap) = guard.as_mut() {
            crate::bearer::mark_renewal_refused(cap);
            if let Some(store) = &state.credentials {
                crate::credentials::persist_capability(store, cap);
                crate::credentials::persist_refusal(store, &refused);
            }
            set_refusal(state, Some(refused));
        }
        // Slot emptied meanwhile (an un-provision won): nothing to refuse.
    }
    crate::account_host::recheck_now(state);
}

/// A renewal under the principal succeeded: write the fresh bearer into the
/// slot, persist it, and end the refused state if one stood — the one thing
/// that does. The account host is woken then, because its device-principal
/// leg follows the refusal and may come back.
async fn adopt_minted(state: &Arc<SyncServiceState>, token: String, expires_at: u64) {
    {
        let mut guard = state.capability.write().await;
        if let Some(cap) = guard.as_mut() {
            use zeroize::Zeroize;
            cap.bearer.token.zeroize();
            cap.bearer.token = token;
            cap.bearer.expires_at = expires_at;
            if let Some(store) = &state.credentials {
                crate::credentials::persist_capability(store, cap);
            }
        }
        // Slot emptied between read and mint (an un-provision won): drop the
        // token.
    }
    if standing_refusal(state).is_some() {
        forget_refusal(state);
        tracing::info!("this machine is enrolled again; the refused state is over");
        crate::account_host::recheck_now(state);
    }
}

/// Recompute the principal-support advertisement
/// ([`SyncServiceState::store_principal_actor`]) — live status, read by the
/// signed-out onboarding reconcile (`sync-agent-credentials.md` § Credential
/// model → the RULED 2026-09-28 block, decision 3: it decides no mint).
///
/// The claim is deliberately **positive and self-verified**: it names an actor
/// only when this agent can show that actor's principal grant is actually
/// REGISTERED, not merely minted (see [`principal_is_registered`] — finding
/// : the writer-key mint establishes slot presence before any grant
/// exists, so presence alone was never sufficient evidence).
///
/// Called where the answer can change — a provision (the actor becomes known,
/// or changes), each renewal (an app may have enrolled the machine since), and
/// at boot restore (`service.rs`'s `restore_authorized_capability` call) —
/// never on the status path, which must stay a field read.
///
/// A `None` credential store is the unit-test posture: never touch the
/// developer's real Secret Service / Keychain (testing.md § point 10). The
/// advertisement then stays `None`, which is the safe reading — the
/// onboarding reconcile un-provisions nothing on an ambiguous read.
pub(crate) async fn refresh_store_principal_presence(state: &Arc<SyncServiceState>) {
    if state.credentials.is_none() {
        return;
    }
    let actor_id = {
        let cap = state.capability.read().await;
        cap.as_ref().and_then(|c| c.actor_id_array())
    };
    let answer = actor_id.filter(|actor| {
        let actor_hex = hex::encode(actor);
        let credentials =
            std::sync::Arc::new(fauna_sync_engine::account_runtime::production_credential_store());
        let Ok(store_dir) =
            fauna_sync_engine::account_runtime::StoreRoot::platform().store_dir(&actor_hex)
        else {
            return false;
        };
        principal_is_registered(credentials, store_dir, &actor_hex)
    });
    *state.store_principal_actor.write().await = answer;
}

/// Whether `actor`'s store principal is a **registered** grant, not merely a
/// key sitting in the slot (finding ).
///
/// Evidence is the same content-addressed latch the enrollment ceremony
/// itself writes on a successful `fauna.sync.register` +
/// `fauna.sync.device_grant.register` round trip
/// ([`fauna_sync_engine::principal_bundle::PrincipalSlot::grant_registration_row`]),
/// read **write-free** (`derived_backup: None` — the same seedless-consumer
/// shape `account_runtime::start`'s Seedless path uses; this call never mints
/// or heals anything in the slot). That durability is load-bearing: the latch
/// is persisted in the credential store, so it answers correctly from the
/// very first call after an agent restart — unlike an in-memory "have I
/// minted a bearer under this actor since this process started" flag, which
/// would read `false` on every restart and undo `service.rs`'s boot-restore
/// advertisement.
///
/// **A known residual gap, not closed here.** The latch is not nest-scoped —
/// it answers "registered on SOME nest", never "registered on THIS
/// capability's nest". A machine whose principal is enrolled against a
/// different nest than the one this capability names still reads as
/// registered; only an actual `fauna.auth.device_handshake` success against
/// that specific nest closes that case, which the renewal loop's own mint
/// answers on this agent's own renewal path. The device-delete state
/// `sync-agent-credentials.md` documents is deliberately NOT one of these gaps: a
/// tombstoned grant's device authorization fails to load (or its latch no
/// longer matches), so it reads as unregistered here — the correct, intended
/// fall-through, not a breach.
///
/// `credentials` and `store_dir` are caller-injected (never resolved inside)
/// so a test can hand this a tempdir-backed slot — same reason
/// [`principal_signing_key`] takes its store by parameter (testing.md § point
/// 10): a function that resolved `production_credential_store()` internally
/// would read (and a careless test would write) the developer's real Secret
/// Service / Keychain.
fn principal_is_registered(
    credentials: std::sync::Arc<fauna_credential_store::CredentialStore>,
    store_dir: std::path::PathBuf,
    actor_hex: &str,
) -> bool {
    let Some(writer_key) =
        fauna_sync_engine::principal_bundle::load_writer_key(&credentials, actor_hex)
    else {
        return false;
    };
    let writer_pub = writer_key.verifying_key().to_bytes();
    let slot = fauna_sync_engine::principal_bundle::PrincipalSlot::resolve(
        credentials,
        actor_hex.to_string(),
        store_dir,
        &writer_pub,
        None,
    );
    slot.grant_registration_row().is_some()
}

/// Does the currently-provisioned capability stand revoked by an app-written
/// sign-out marker? Reads both from live state, so it sees a sign-out that
/// landed while this process kept running.
///
/// Returns `false` whenever it cannot tell (no capability, no store, no marker,
/// an unreadable marker) — the marker may only ever *remove* authority, so
/// every ambiguous answer leaves the agent serving, exactly as it did before
/// this reconcile existed.
async fn signed_out_reconcile(state: &Arc<SyncServiceState>) -> bool {
    let Some(store) = &state.credentials else {
        return false;
    };
    let Some(marker) = crate::credentials::load_signed_out_marker(store) else {
        return false;
    };
    let guard = state.capability.read().await;
    let Some(cap) = guard.as_ref() else {
        return false;
    };
    if fauna_ipc::sync::capability_is_signed_out(cap, &marker) {
        tracing::warn!(
            signed_out_at_ms = marker.signed_out_at_ms,
            provisioned_at_ms = ?cap.provisioned_at_ms,
            "this account signed out on this machine and the un-provision message \
             never arrived; un-provisioning now"
        );
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── A rejected credential asks at once ──────────────────────────────────

    /// A reader's report asks the nest at once, but at most once per distinct
    /// slot bearer: a leg retrying on its own ceiling, or several clients
    /// refused the same bearer, never turn into a mint per retry. A report
    /// about a bearer the slot no longer holds, or while the slot is refused
    /// (empty), asks nothing.
    #[test]
    fn a_rejection_asks_once_per_distinct_bearer() {
        use crate::bearer::Rejection;
        let bearer = |t: &str| Rejection::Bearer(zeroize::Zeroizing::new(t.to_owned()));

        // A 401 to the bearer the slot holds asks; the principal's refusal too.
        assert!(rejection_asks_now(&bearer("tok"), "tok", None));
        assert!(rejection_asks_now(&Rejection::Principal, "tok", None));
        // Asked already about this bearer: neither asks again.
        assert!(!rejection_asks_now(&bearer("tok"), "tok", Some("tok")));
        assert!(!rejection_asks_now(
            &Rejection::Principal,
            "tok",
            Some("tok")
        ));
        // A new bearer in the slot may be asked about once more.
        assert!(rejection_asks_now(
            &Rejection::Principal,
            "tok-2",
            Some("tok")
        ));
        // A stale report (the slot moved on) and an empty slot ask nothing.
        assert!(!rejection_asks_now(&bearer("old"), "tok", None));
        assert!(!rejection_asks_now(&Rejection::Principal, "", None));
    }

    // ── The terminal refusal: `not_registered` on the principal's key ───────

    fn principal() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[1u8; 32])
    }

    fn not_registered() -> fauna_anon_client::AnonClientError {
        fauna_anon_client::AnonClientError::Rpc(fauna_protocol::RpcError::not_registered())
    }

    /// The nest refusing the principal `not_registered` is the terminal case:
    /// it is the machine's only renewal credential, so no grant for this
    /// machine exists and retrying cannot make one.
    #[tokio::test]
    #[allow(clippy::err_expect)] // MintedBearer carries a bearer token and deliberately derives no Debug
    async fn not_registered_on_the_principal_is_terminal() {
        let err = mint_with(principal(), |_| async { Err(not_registered()) })
            .await
            .err()
            .expect("the mint failed");
        assert!(err.refused, "refused not_registered: {}", err.message);
    }

    /// A transport failure is a transient, never the terminal case — the nest
    /// said nothing about the grant.
    #[tokio::test]
    #[allow(clippy::err_expect)] // MintedBearer carries a bearer token and deliberately derives no Debug
    async fn a_transport_failure_is_not_terminal() {
        let err = mint_with(principal(), |_| async {
            Err(fauna_anon_client::AnonClientError::WebSocket(
                "refused".into(),
            ))
        })
        .await
        .err()
        .expect("the mint failed");
        assert!(!err.refused);
    }

    /// The refused state reaches the shared slot: the bearer is dropped, so
    /// every client over it stops dialling, and the loop's own next wake finds
    /// nothing to renew.
    #[tokio::test]
    async fn a_refused_renewal_empties_the_slots_bearer() {
        let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        let state = SyncServiceState::new(
            crate::config::SyncConfig::default(),
            shutdown_tx,
            event_tx,
            crate::config::SyncPaths::new(None),
        );
        *state.capability.write().await = Some(fauna_ipc::sync::SyncCapability::new(
            vec![1u8; 32],
            vec![7u8; 32],
            "https://nest.example".into(),
            "dev".into(),
            fauna_ipc::sync::BearerToken::new("tok".into(), 1),
        ));
        refuse_renewal(&state, refused_evidence()).await;
        let cap = state.capability.read().await;
        let cap = cap.as_ref().expect("the capability itself stays");
        assert!(crate::bearer::renewal_refused(cap));
    }

    // ── The refusal record: what is reported, and when the nest is asked again ──

    fn refused_evidence() -> RefusalRecord {
        RefusalRecord {
            nest_url: "https://nest.example".into(),
            principal_key: hex::encode([0xc3u8; 32]),
            grant_registered: false,
        }
    }

    fn provisioned_capability() -> fauna_ipc::sync::SyncCapability {
        fauna_ipc::sync::SyncCapability::new(
            vec![1u8; 32],
            vec![7u8; 32],
            "https://nest.example".into(),
            "dev".into(),
            fauna_ipc::sync::BearerToken::new("tok".into(), u64::MAX),
        )
    }

    /// A state over a tempdir: scoped paths, and — with `store` — the agent's
    /// own credential store on a file backend, never the OS keyring.
    fn state_over(
        dir: &std::path::Path,
        store: Option<Arc<fauna_credential_store::CredentialStore>>,
    ) -> Arc<SyncServiceState> {
        let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        SyncServiceState::new_with_credentials(
            crate::config::SyncConfig::default(),
            shutdown_tx,
            event_tx,
            crate::config::SyncPaths::new(Some(dir.to_path_buf())),
            store,
        )
    }

    fn agent_store(dir: &std::path::Path) -> Arc<fauna_credential_store::CredentialStore> {
        Arc::new(fauna_credential_store::CredentialStore::with_file_backend(
            "fauna-sync-agent-test",
            dir.to_path_buf(),
        ))
    }

    /// What the pipe answers `GetServiceStatus` — through the real handler.
    async fn reports_needs_reenrollment(state: &Arc<SyncServiceState>) -> bool {
        match request(state, fauna_ipc::sync::RequestMethod::GetServiceStatus).await {
            fauna_ipc::sync::ResponseResult::Ok(
                fauna_ipc::sync::ResponsePayload::ServiceStatus(status),
            ) => status.needs_reenrollment,
            _ => panic!("GetServiceStatus did not answer a status"),
        }
    }

    async fn request(
        state: &Arc<SyncServiceState>,
        method: fauna_ipc::sync::RequestMethod,
    ) -> fauna_ipc::sync::ResponseResult {
        crate::pipe_server::handle_request(&fauna_ipc::sync::Request { id: 1, method }, state)
            .await
            .result
    }

    /// Whether the renewal loop has a wake waiting (a `Notify` permit).
    async fn loop_was_woken(state: &Arc<SyncServiceState>) -> bool {
        tokio::time::timeout(Duration::ZERO, state.renewal_wake.notified())
            .await
            .is_ok()
    }

    /// **The central pin: an app's pushed bearer re-arms dialling and leaves
    /// the report standing** (`sync-agent-credentials.md` § Credential model →
    /// *What the agent dials and what it reports are two separate states*).
    /// Every signed-in desktop app pushes `RefreshBearer` on its 30 s tick, so
    /// a report read off the bearer slot was true for one tick an hour.
    #[tokio::test]
    async fn a_pushed_bearer_re_arms_the_slot_and_the_report_still_stands() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_over(dir.path(), None);
        *state.capability.write().await = Some(provisioned_capability());
        assert!(!reports_needs_reenrollment(&state).await);

        refuse_renewal(&state, refused_evidence()).await;
        assert!(reports_needs_reenrollment(&state).await);

        let pushed = request(
            &state,
            fauna_ipc::sync::RequestMethod::RefreshBearer(fauna_ipc::sync::BearerToken::new(
                "app-tok".into(),
                u64::MAX,
            )),
        )
        .await;
        assert!(matches!(pushed, fauna_ipc::sync::ResponseResult::Ok(_)));
        {
            let cap = state.capability.read().await;
            assert!(
                !crate::bearer::renewal_refused(cap.as_ref().unwrap()),
                "the push re-arms the slot's clients"
            );
        }
        assert!(
            reports_needs_reenrollment(&state).await,
            "an app's bearer says nothing about this machine's grant: the report \
             stands until a renewal under the principal succeeds"
        );
        assert!(
            loop_was_woken(&state).await,
            "and the push wakes the loop to re-read the evidence"
        );
    }

    /// With no refusal standing, a pushed bearer wakes nothing: the loop keeps
    /// its ordinary schedule.
    #[tokio::test]
    async fn a_pushed_bearer_wakes_nothing_while_no_refusal_stands() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_over(dir.path(), None);
        *state.capability.write().await = Some(provisioned_capability());
        let pushed = request(
            &state,
            fauna_ipc::sync::RequestMethod::RefreshBearer(fauna_ipc::sync::BearerToken::new(
                "app-tok".into(),
                u64::MAX,
            )),
        )
        .await;
        assert!(matches!(pushed, fauna_ipc::sync::ResponseResult::Ok(_)));
        assert!(!loop_was_woken(&state).await);
    }

    /// A provision is the other handler that writes a bearer: same rule.
    #[tokio::test]
    async fn a_provision_leaves_the_report_standing_and_wakes_the_loop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_over(dir.path(), None);
        *state.capability.write().await = Some(provisioned_capability());
        refuse_renewal(&state, refused_evidence()).await;

        let provisioned = request(
            &state,
            fauna_ipc::sync::RequestMethod::ProvisionCapability(provisioned_capability()),
        )
        .await;
        assert!(matches!(
            provisioned,
            fauna_ipc::sync::ResponseResult::Ok(_)
        ));
        assert!(reports_needs_reenrollment(&state).await);
        assert!(loop_was_woken(&state).await);
    }

    /// The pure decision while a refusal stands: probe on evidence the record
    /// was not refused on — whether or not an app has re-armed the bearer —
    /// and on nothing else.
    #[test]
    fn a_standing_refusal_probes_once_per_distinct_evidence() {
        let refused = refused_evidence();
        let probe = |evidence: &RefusalRecord, armed: bool| {
            decide(Some(&refused), Some(evidence), armed, false, u64::MAX, 0)
        };

        let other_key = RefusalRecord {
            principal_key: hex::encode([0xd4u8; 32]),
            ..refused.clone()
        };
        let other_nest = RefusalRecord {
            nest_url: "https://other.example".into(),
            ..refused.clone()
        };
        let now_registered = RefusalRecord {
            grant_registered: true,
            ..refused.clone()
        };
        for news in [&other_key, &other_nest, &now_registered] {
            assert_eq!(probe(news, false), Pass::Probe, "{news:?}, empty bearer");
            assert_eq!(probe(news, true), Pass::Probe, "{news:?}, armed bearer");
        }

        // The evidence it was refused on: a refused, un-armed agent dials
        // nothing, and an armed one waits for the pushed bearer's lead.
        assert_eq!(probe(&refused, false), Pass::Idle);
        assert!(matches!(probe(&refused, true), Pass::Sleep(_)));
        assert_eq!(
            decide(Some(&refused), Some(&refused), true, false, 10_000, 9_800),
            Pass::RenewAtLead
        );
        // No principal in the slot: never a probe.
        assert_eq!(
            decide(Some(&refused), None, false, false, u64::MAX, 0),
            Pass::Idle
        );
    }

    /// Precedence, and the no-record path unchanged: the evidence probe, then
    /// a reader's report, then the plan; an empty bearer with no record idles.
    #[test]
    fn the_decision_orders_probe_then_report_then_plan() {
        let refused = refused_evidence();
        let news = RefusalRecord {
            grant_registered: true,
            ..refused.clone()
        };
        assert_eq!(
            decide(Some(&refused), Some(&news), true, true, u64::MAX, 0),
            Pass::Probe
        );
        assert_eq!(
            decide(Some(&refused), Some(&refused), true, true, u64::MAX, 0),
            Pass::AskOnReport
        );
        assert_eq!(
            decide(None, None, true, true, u64::MAX, 0),
            Pass::AskOnReport
        );
        assert_eq!(
            decide(None, None, true, false, 10_000, 6_400),
            Pass::Sleep(Duration::from_secs(3_300))
        );
        assert_eq!(
            decide(None, None, true, false, 10_000, 9_800),
            Pass::RenewAtLead
        );
        assert_eq!(decide(None, None, false, false, 0, 0), Pass::Idle);
    }

    /// A refused probe rewrites the record to the evidence it was refused on,
    /// so that evidence is not probed a second time — and drops the bearer an
    /// app had re-armed.
    #[tokio::test]
    async fn a_refused_probe_rewrites_the_record_to_what_it_was_refused_on() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = agent_store(dir.path());
        let state = state_over(dir.path(), Some(store.clone()));
        *state.capability.write().await = Some(provisioned_capability());
        refuse_renewal(&state, refused_evidence()).await;

        let successor = RefusalRecord {
            principal_key: hex::encode([0xd4u8; 32]),
            ..refused_evidence()
        };
        let before = standing_refusal(&state).unwrap();
        assert_eq!(
            decide(Some(&before), Some(&successor), false, false, 0, 0),
            Pass::Probe
        );
        if let Some(cap) = state.capability.write().await.as_mut() {
            cap.bearer = fauna_ipc::sync::BearerToken::new("app-tok".into(), u64::MAX);
        }
        refuse_renewal(&state, successor.clone()).await;

        let after = standing_refusal(&state).unwrap();
        assert_eq!(after, successor);
        assert_eq!(
            crate::credentials::load_refusal(&store),
            Some(successor.clone())
        );
        assert_eq!(
            decide(Some(&after), Some(&successor), false, false, 0, 0),
            Pass::Idle
        );
        let cap = state.capability.read().await;
        assert!(crate::bearer::renewal_refused(cap.as_ref().unwrap()));
    }

    /// Only a renewal that succeeds ends the refused state — memory and store.
    #[tokio::test]
    async fn a_successful_renewal_clears_the_record_and_its_stored_copy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = agent_store(dir.path());
        let state = state_over(dir.path(), Some(store.clone()));
        *state.capability.write().await = Some(provisioned_capability());
        refuse_renewal(&state, refused_evidence()).await;
        assert!(crate::credentials::load_refusal(&store).is_some());

        adopt_minted(&state, "minted".into(), 4_000_000_000).await;

        assert!(!reports_needs_reenrollment(&state).await);
        assert!(crate::credentials::load_refusal(&store).is_none());
        assert_eq!(
            crate::credentials::load_capability(&store).map(|c| c.bearer.token.clone()),
            Some("minted".into())
        );
    }

    /// The refused state survives an agent restart exactly as the capability
    /// does: a fresh process restoring from the same store still reports it.
    #[tokio::test]
    async fn the_refused_state_survives_an_agent_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = agent_store(dir.path());
        {
            let state = state_over(dir.path(), Some(store.clone()));
            *state.capability.write().await = Some(provisioned_capability());
            refuse_renewal(&state, refused_evidence()).await;
        }

        let restarted = state_over(dir.path(), Some(store.clone()));
        *restarted.capability.write().await =
            crate::credentials::restore_authorized_capability(&store);
        assert!(!reports_needs_reenrollment(&restarted).await);
        restore_refusal(&restarted);
        assert!(reports_needs_reenrollment(&restarted).await);
        assert_eq!(standing_refusal(&restarted), Some(refused_evidence()));
    }

    /// Un-provisioning deletes the record with the capability it described.
    #[tokio::test]
    async fn un_provisioning_deletes_the_refusal_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = agent_store(dir.path());
        let state = state_over(dir.path(), Some(store.clone()));
        *state.capability.write().await = Some(provisioned_capability());
        refuse_renewal(&state, refused_evidence()).await;

        crate::pipe_server::unprovision_now(&state).await;

        assert!(standing_refusal(&state).is_none());
        assert!(crate::credentials::load_refusal(&store).is_none());
        assert!(!reports_needs_reenrollment(&state).await);
    }

    // ── T11 convergence: which device key this machine renews under ─────────

    /// A T10 slot on a tempdir — never the OS keyring (testing.md § point 10).
    fn slot(dir: &std::path::Path) -> fauna_credential_store::CredentialStore {
        fauna_credential_store::CredentialStore::with_file_backend(
            fauna_sync_engine::account_runtime::CRED_NAMESPACE,
            dir.to_path_buf(),
        )
    }

    const ACTOR: [u8; 32] = [0xa1; 32];
    const WRITER: [u8; 32] = [0xc3; 32];

    fn enroll_writer_key(store: &fauna_credential_store::CredentialStore) {
        use fauna_client_accounts::SecretStore;
        store.set(&hex::encode(ACTOR), &hex::encode(WRITER));
    }

    /// The machine renews under the **store principal alone** — the writer key
    /// from the T10 slot, the machine's only renewal credential
    /// (`sync-agent-credentials.md` § Credential model → the RULED 2026-09-28
    /// block, decision 1).
    #[test]
    fn the_store_principal_is_the_one_renewal_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = slot(dir.path());
        enroll_writer_key(&store);

        let key = principal_signing_key(&store, &ACTOR).expect("the principal is offered");
        assert_eq!(
            key.to_bytes(),
            WRITER,
            "the candidate must be the writer key from the T10 slot — the \
             machine's device key, which is what makes the nest see ONE device"
        );
    }

    /// An empty slot: the loop must find nothing to try rather than fabricate
    /// one. A minted writer key here would be an identity no nest has a grant
    /// for — and a second writer for a store no app has enrolled, which is the
    /// divergence measured.
    #[test]
    fn an_empty_slot_offers_no_key_rather_than_minting_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = slot(dir.path());

        assert!(
            principal_signing_key(&store, &ACTOR).is_none(),
            "no principal must mean no candidate — never a minted one"
        );
        assert!(
            !dir.path()
                .join(fauna_sync_engine::account_runtime::CRED_NAMESPACE)
                .exists()
                || {
                    use fauna_client_accounts::SecretStore;
                    store.get(&hex::encode(ACTOR)).is_none()
                },
            "and the probe must not have WRITTEN a writer key into the slot"
        );
    }

    /// The evidence a mint rests on names the nest, the principal's PUBLIC
    /// key and the latch reading; an empty slot yields none, so nothing probes.
    #[test]
    fn the_evidence_names_the_nest_the_public_key_and_the_latch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let evidence = |dir: &std::path::Path| {
            principal_evidence(
                Arc::new(slot(dir)),
                Some(dir.join("store")),
                "https://nest.example",
                &ACTOR,
            )
            .map(|(_, evidence)| evidence)
        };
        assert!(evidence(dir.path()).is_none(), "no principal → no evidence");

        enroll_writer_key(&slot(dir.path()));
        let writer = ed25519_dalek::SigningKey::from_bytes(&WRITER);
        assert_eq!(
            evidence(dir.path()),
            Some(RefusalRecord {
                nest_url: "https://nest.example".into(),
                principal_key: hex::encode(writer.verifying_key().to_bytes()),
                grant_registered: false,
            })
        );
    }

    // ── : advertise on a REGISTERED principal, not a minted one ──

    /// Build a root-signed `DeviceAuthorization` over `device_key` — the same
    /// shape the enrollment ceremony mints and `PrincipalSlot::
    /// store_device_authorization` persists.
    fn grant_over(
        account: &fauna_core::identity::ActorKeypair,
        device_key: [u8; 32],
    ) -> fauna_core::encoding::EmbedAsBytes {
        let auth = fauna_core::data::DeviceAuthorization {
            actor_id: account.actor_id(),
            device_key,
            capabilities: vec![fauna_core::data::Capability::RenewBearer],
            created_at: fauna_core::data::Timestamp::now(),
            expires_at: None,
        };
        let (bytes, env) = fauna_core::encoding::sign_envelope(account, &auth).expect("sign");
        fauna_core::encoding::EmbedAsBytes::from_signed(bytes, env)
    }

    /// A FRESH read of `principal_is_registered` over `dir` — a brand-new
    /// `CredentialStore` and `PrincipalSlot` constructed on every call, exactly
    /// as production does (`refresh_store_principal_presence` resolves
    /// `production_credential_store()` fresh every time it runs). This is what
    /// makes the second half of
    /// `advertisement_requires_a_registered_grant_not_a_minted_key` below a
    /// genuine boot-restore case rather than an in-process cache hit.
    fn is_registered_fresh(dir: &std::path::Path, actor_hex: &str) -> bool {
        principal_is_registered(std::sync::Arc::new(slot(dir)), dir.join("store"), actor_hex)
    }

    /// ** pin (a)** — a writer key alone, minted but never
    /// registered, must not advertise principal support: that conflation is
    /// exactly the finding's gap (the writer-key mint establishes slot
    /// presence before any grant exists — a bare `load_writer_key(..)
    /// .is_some()` check, the pre-fix behavior, would already answer true
    /// here). Once the ceremony's registration latch is recorded, a FRESH
    /// read — never a value carried in this process's memory — answers true.
    /// That is the boot-restore requirement: an in-memory "have I minted a
    /// bearer under this actor since this process started" flag would still
    /// answer false here, undoing `service.rs`'s boot-restore advertisement.
    #[test]
    fn advertisement_requires_a_registered_grant_not_a_minted_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let account = fauna_core::identity::ActorKeypair::generate();
        let actor_hex = fauna_core::hex32::encode(&account.actor_id().0);
        let writer = ed25519_dalek::SigningKey::from_bytes(&WRITER);
        let writer_pub = writer.verifying_key().to_bytes();

        // Mint the writer key directly into the slot — slot PRESENCE, no
        // grant registered yet.
        {
            use fauna_client_accounts::SecretStore;
            slot(dir.path()).set(&actor_hex, &hex::encode(writer.to_bytes()));
        }
        assert!(
            !is_registered_fresh(dir.path(), &actor_hex),
            "a minted-but-unregistered writer key must not advertise principal support"
        );

        // The ceremony registers: persists the device authorization and
        // records the content-addressed latch on the machine's row — exactly
        // what a successful `fauna.sync.register` +
        // `fauna.sync.device_grant.register` round trip leaves behind.
        {
            let store = std::sync::Arc::new(slot(dir.path()));
            let principal_slot = fauna_sync_engine::principal_bundle::PrincipalSlot::resolve(
                std::sync::Arc::clone(&store),
                actor_hex.clone(),
                dir.path().join("store"),
                &writer_pub,
                None,
            );
            let wire = grant_over(&account, writer_pub);
            principal_slot
                .store_device_authorization(wire, &writer_pub)
                .expect("persist grant");
            principal_slot.record_grant_registered_on(&hex::encode(writer_pub));
        }

        assert!(
            is_registered_fresh(dir.path(), &actor_hex),
            "a registered grant must advertise principal support, read fresh — \
             the boot-restore case"
        );
    }

    /// **The lost-message case, live arm** — the half a reboot cannot fix.
    ///
    /// The agent is *wedged, not dead*: it missed the sign-out's single
    /// best-effort `UnprovisionCapability` and then keeps running, so nothing
    /// ever re-reads its authorization. Its bearer self-renews off a grant that
    /// carries no expiry, so before this reconcile the signed-out account's
    /// engines served for as long as the process lived.
    ///
    /// Drives the **real loop** (`run`), not the predicate: the assert is that
    /// the provisioned slot empties on the loop's own initiative, with no IPC
    /// message and no app. Latency-independent per convention 14 — a deadline
    /// poll on the state transition, so a green run pays nothing and a slow
    /// machine does not turn it red.
    #[tokio::test]
    async fn a_live_agent_stops_serving_an_account_that_signed_out() {
        use fauna_ipc::sync::{BearerToken, SignedOutMarker, SyncCapability};

        let dir = std::env::temp_dir().join(format!("sync-agent-wedged-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store =
            std::sync::Arc::new(fauna_credential_store::CredentialStore::with_file_backend(
                "fauna-sync-agent-test",
                dir.clone(),
            ));
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        let state = SyncServiceState::new_with_credentials(
            crate::config::SyncConfig::default(),
            shutdown_tx.clone(),
            event_tx,
            crate::config::SyncPaths::new(Some(dir.clone())),
            Some(store.clone()),
        );

        let actor = [0x42u8; 32];
        let cap = SyncCapability::new(
            vec![9u8; 32],
            actor.to_vec(),
            "https://nest.invalid".into(),
            "dev-wedged".into(),
            // Far-future expiry: this loop has no renewal work to do, so the
            // only thing that can empty the slot is the reconcile itself.
            BearerToken::new("wedged-tok".into(), u64::MAX),
        )
        .with_provisioned_at_ms(1_000);
        crate::credentials::persist_capability(&store, &cap);
        *state.capability.write().await = Some(cap);

        // The app signed out — the durable marker landed, the message did not.
        {
            use fauna_client_accounts::SecretStore;
            let record = SignedOutMarker::new(actor.to_vec(), 2_000)
                .encode_record()
                .expect("marker encodes");
            store.set(fauna_ipc::sync::SIGNED_OUT_KEY, &record);
        }

        let loop_state = state.clone();
        let handle = tokio::spawn(run(loop_state, shutdown_rx));

        // Generous ceiling, deadline-polled: the reconcile runs on the loop's
        // first iteration, so a healthy run settles immediately and this budget
        // is never spent.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut cleared = false;
        while std::time::Instant::now() < deadline {
            if state.capability.read().await.is_none() {
                cleared = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let _ = shutdown_tx.send(true);
        let _ = handle.await;

        assert!(
            cleared,
            "a live agent must stop serving an account that signed out, even though the \
             un-provision message never arrived"
        );
        assert!(
            crate::credentials::load_capability(&store).is_none(),
            "the persisted record goes with it, so a reboot cannot revive the account"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fresh_bearer_sleeps_until_the_lead() {
        // Expires in 1h; lead is 300s → sleep ~55min.
        let w = plan(10_000, 6_400);
        assert_eq!(w, Wake::Sleep(Duration::from_secs(3_300)));
    }

    #[test]
    fn inside_the_lead_renews_now() {
        assert_eq!(plan(10_000, 9_800), Wake::RenewNow);
    }

    #[test]
    fn past_expiry_renews_now() {
        assert_eq!(plan(10_000, 20_000), Wake::RenewNow);
    }
}
