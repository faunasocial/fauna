//! Web's succession-ceremony driver — the browser twin of
//! [`fauna_client_recovery::ceremony`]'s three native functions
//!.
//!
//! **What is shared and what is here.** The ceremony *primitives*
//! (`succeed_with_held_kit`, `reconcile_succession`, `sweep_groups`) are
//! transport-generic and already wasm-clean, and so are the outcome types and
//! the typed outcome and its wording ([`SweepStatus`], [`LandedSuccession`],
//! [`StolenOutcome`] — un-gated for this leg). What genuinely
//! cannot be shared is the transport-bound sequencing, and only because the
//! three things native reaches for do not exist in a browser:
//!
//! | native (`ceremony.rs`) | here |
//! |---|---|
//! | `AnonymousNestClient::connect` | `AnonymousWsRpcClient::connect` |
//! | `NestClient::new` + `connect()` | anon handshake → `TokenWsRpcClient` |
//! | `MlsEngine::new(signer, &db_path)` | `MlsEngine::new_in_memory` |
//! | `engine.save_state()` (SQLite) | the nest replica plane |
//!
//! Everything else — the ORDER, which is the whole safety property — is the
//! same, item for item, as the nine-item contract row 255 carries. In
//! particular: the successor seed is persisted off the attempt **before** the
//! confirmed/unconfirmed arms are matched (both arms carry it, and the
//! unconfirmed one is exactly the case where the nest may already have
//! committed); the persist is verified by **read-back**, never off
//! `add_account`'s return, because `SecretStore::set` is infallible by
//! signature; and the successor's session is established **before** its engine
//! is built, so an unreachable nest fails before any state is written.
//!
//! The one thing web does not inherit is the post-window sweep RETRY
//! (`recovery-kit-sweep-retry-button`): that needs the *retired* identity's own
//! MLS store, which on web rests in the nest replica behind bearers the
//! succession just revoked. A web device that missed the window has no old
//! engine to construct and answers with the member-side remedy — the same
//! answer any device without the old store gives (`ui.yaml`'s own
//! `recovery-kit-sweep-retry-button` note: the render gate is unfinished work,
//! never "this device can retry").

use std::sync::Arc;

use fauna_client_accounts::{AccountRegistry, LocalStorageSecretStore};
use fauna_client_conversations::WsMlsReplicaTransport;
use fauna_client_mls_sync::{MlsStateSync, ReplicaResealProgress};
use fauna_client_recovery::{
    ReconciledSuccession, RecoveryClient, SuccessionAttempt,
    aftermath::PendingCeremony,
    ceremony::{LandedSuccession, StolenOutcome, SweepStatus},
};
use fauna_conversations::ConversationsManager;
use fauna_conversations::backends::fauna_mls::FaunaMlsBackend;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_mls::{engine::MlsEngine, state_replica::ProviderReplica};
use fauna_protocol::RpcRequester;
use fauna_rpc_wasm::{AnonymousWsRpcClient, TokenWsRpcClient, WsRpcClient as InnerClient};
use serde::Serialize;

/// The ceremony's outcome, as the SPA sees it.
///
/// `secretHex` crosses to JS deliberately and is the reason this shape exists at
/// all: at the instant it arrives it exists nowhere else in the world, and the
/// settings page shows it exactly as the mint ceremonies show a fresh kit — the
/// only way back into the account if the local persist did not take.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LandedSuccessionJs {
    /// The successor identity's 64-hex secret — the account, from here on.
    pub secret_hex: String,
    /// The actor id the account now belongs to, lowercase hex.
    pub new_actor_id: String,
    /// Whether the seed was verified present in this browser's account store by
    /// a read-back. `false` means the SPA MUST keep the secret on screen.
    pub persisted: bool,
    /// `"no-engine"` / `"failed"` / `"ran"` — the propagation half's outcome,
    /// never the ceremony's ([`SweepStatus`]'s own three arms).
    pub sweep: String,
    /// Present on `failed`, and on `ran` when some group did not take.
    pub sweep_detail: Option<String>,
    /// Unix seconds the nest applied the succession, when it told us. `None` on
    /// the reconcile arm, where the reply that carries it never arrived.
    pub succeeded_at: Option<i64>,
}

/// How `identity-stolen-button`'s ceremony ended, as the SPA receives it — the
/// shared [`StolenOutcome`] in the FFI record's shape (`FfiStolenOutcome`), so
/// web paints the same arms with the same rule (`settings.md` § Recovery kit →
/// *The ceremony's outcome is headlined by its arm*): `message` verbatim on
/// `error-message`, wrapping nothing.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StolenOutcomeJs {
    /// `"landed"` / `"not-landed"` / `"landed-for-another"` / `"undecided"`.
    pub kind: &'static str,
    /// The sentence to resolve and paint; `None` on `"landed"` alone.
    pub message: Option<fauna_core::localized::LocalizedText>,
    /// On `"landed"`, the succession; `None` on every other arm.
    pub landed: Option<LandedSuccessionJs>,
    /// The undecided arm whose persist was not verified: `message` carries the
    /// only copy of the seed, so the SPA parks it as it parks
    /// `stolen_persist_failed`.
    pub carries_the_only_seed: bool,
}

impl StolenOutcomeJs {
    /// Every arm but `Landed` — the landed arm owes the post-landing
    /// bookkeeping first, and is built at the end of the driver.
    fn unlanded(outcome: &StolenOutcome) -> Self {
        Self {
            kind: outcome.kind(),
            message: outcome.message(),
            landed: None,
            carries_the_only_seed: outcome.carries_the_only_seed(),
        }
    }
}

/// The live conversations session's engine plus the way to persist it.
///
/// A struct rather than a bare `&MlsEngine` because web's "save" is not a method
/// on the engine — it is a snapshot of the engine *and* the manager, sealed and
/// uploaded through the session's own [`MlsStateSync`]. Keeping the pair
/// together makes it impossible to sweep an engine this driver then cannot land.
pub(crate) struct OldEngineHandle {
    pub engine: Arc<MlsEngine>,
    pub backend: Arc<FaunaMlsBackend>,
    pub manager: Arc<ConversationsManager>,
    pub sync: Arc<MlsStateSync>,
}

impl OldEngineHandle {
    /// The web twin of `old_engine.save_state()` — snapshot on this thread (the
    /// engine and manager state live here), then seal + upload.
    async fn save(&self) {
        let snapshot = fauna_client_mls_sync::orchestration::snapshot_replica(
            &self.backend,
            &self.manager,
            &self.sync,
        );
        if let Err(e) =
            fauna_client_mls_sync::orchestration::save_snapshot(&self.sync, &snapshot).await
        {
            tracing::error!(
                "[wasm/succession] persisting the succeeded engine after the sweep: {e}"
            );
        }
    }
}

/// Flatten a [`SweepStatus`] into the two JS fields.
///
/// ⚠ **The projection itself moved to the shared crate 2026-08-21**
/// ([`SweepStatus::render_view`]) — this was its only definition while web was
/// the only non-tui driver, and the native drivers (apple/windows/android, over
/// `fauna-ffi`) would each have needed the same three arms. What is left here is
/// the JS *shape* (a tuple for `LandedSuccessionJs`'s two fields), which is
/// genuinely web's own; the judgment of what each arm says is not.
fn sweep_view(sweep: &SweepStatus) -> (String, Option<String>) {
    let view = sweep.render_view();
    (view.kind, view.detail)
}

/// Run the post-succession **aftermath** in this browser — the ordered pass
/// (legs 1, 2, 7 here; 4, 8 and 6 in the post-store-ready pass) a successor's
/// first authenticated session owes
/// (`succession-aftermath.md` § Re-key scope → the `BackupKey` corpus row:
/// "started at first successor sign-in, surfaced with progress, resumed until
/// complete").
///
/// **The pass itself is shared**
/// ([`fauna_client_recovery::aftermath::run_succession_aftermath`]) — the same
/// one tui drives, and the same one the five FFI apps will. What is web's is
/// this function's three jobs: resolve the predecessor material out of the
/// browser's own account store, build the mail machine leg 6 drives on the
/// browser transport (stashed for the post-store-ready pass, which runs it),
/// and turn each leg's progress line into a call the SPA can paint.
/// ⚠ **Do not re-express the leg ORDER here** — that ordering, and which legs
/// are barriers, is the whole safety property, and it now has exactly one
/// statement.
///
/// **Free for an identity that never succeeded** — the predecessor walk is
/// empty and this returns before any round trip, which is what lets the SPA
/// call it on every actor settle rather than trying to detect a succession
/// first. Idempotent: a completed pass costs one `get` per plane and writes
/// nothing.
///
/// The predecessor walk is the registry's
/// ([`AccountRegistry::predecessor_backup_keys_by_actor`] — the full ancestor
/// chain resolved to `(actor, key)` pairs),
/// never re-derived here: a dropped row reads as "no key opens it" forever,
/// which is exactly the silent failure priority #2 exists to prevent. ⚠ It used
/// to be `predecessor_seeds`, which is declared for escrow-container producers
/// — this consumer only ever needed to *open* bytes, so routing through it
/// handed a raw identity seed to a caller with no use for one (fixed
/// 2026-08-23, when the shared pair walk was written). A predecessor whose seed
/// this browser never held is simply absent, never an error — the ordinary
/// state on a device the user did not succeed from, where the pass still runs
/// so it can report `NoKeyOpensIt` (owed by another device) rather than doing
/// nothing silently.
///
/// This tab's `sessionStorage` — the SPA's established analogue of a Rust
/// process-global across the account switch's **full document swap**
/// (`performSwitch`'s `window.location.assign`; `$lib/api.ts`'s nest-dial
/// override, `$lib/generation-e2e.ts`'s teardown counter): it survives the swap
/// and dies with the tab. Here it holds the ephemeral review pass's witness
/// and the owed kit; the ceremony's raise context is NOT here — it is parked
/// durably in the account registry (`PendingCeremony::park`).
fn session_storage() -> Option<web_sys::Storage> {
    // Storage can be denied outright (privacy modes), which every other reader
    // in this SPA treats as "no value" rather than an error. So does this: the
    // park no-ops, the consume finds nothing, and every restoring leg of the
    // aftermath still runs.
    web_sys::window()?.session_storage().ok().flatten()
}

/// The `sessionStorage` key for [`mark_ephemeral_review_pass_witness`] — one
/// per successor: an unrelated account activated in the same tab must not
/// inherit a witness raised for a different identity.
fn ephemeral_review_pass_witness_key(successor_actor_hex: &str) -> String {
    format!("fauna_ephemeral_review_pass:{successor_actor_hex}")
}

/// Witness "a succession sweep ran THIS session" for the ephemeral kit-side
/// review pass's render gate (`succession-aftermath.md` § Propagation, item
/// (ii)) — the production twin of `publish_sweep_state_for_e2e`, but never
/// feature-gated: that key exists only under `test-helpers` for a journey to
/// assert against; this one is what the pass actually renders from.
///
/// **Why this cannot be the parked `PendingCeremony` itself.** The park is
/// drained (cleared) by the post-store-ready pass once its raises land, while
/// this witness has the opposite lifetime: set once, by the succession fold
/// beside the park, then **read repeatedly** — every render of the Settings
/// page asks the same question until the user acts — and cleared only by
/// [`clear_ephemeral_review_pass_witness`], the *Review The Rest Later*
/// action. The roster itself is never duplicated into this
/// key: the pass reads the same `$lib/member-reviews` store the permanent
/// page and the member chips already refresh, this key answers only "was a
/// sweep the reason it's non-empty right now".
fn mark_ephemeral_review_pass_witness(successor_actor_hex: &str) {
    let Some(store) = session_storage() else {
        tracing::warn!(
            "[wasm/succession] no sessionStorage to witness the sweep in — the ephemeral \
             review pass will not render this session (the permanent page still will)"
        );
        return;
    };
    if let Err(e) = store.set_item(&ephemeral_review_pass_witness_key(successor_actor_hex), "1") {
        tracing::warn!(
            ?e,
            "[wasm/succession] witnessing the ephemeral review pass failed"
        );
    }
}

/// Whether a sweep ran this session for `successor_actor_hex` — the
/// ephemeral pass's render gate. `false` on every ordinary sign-in and on a
/// successor's *second* one, same as tui's `App::succession_sweep.is_none()`.
pub(crate) fn ephemeral_review_pass_active(successor_actor_hex: &str) -> bool {
    session_storage()
        .and_then(|store| {
            store
                .get_item(&ephemeral_review_pass_witness_key(successor_actor_hex))
                .ok()
                .flatten()
        })
        .is_some()
}

/// *Review The Rest Later* — hides the pass and decides nothing: the open
/// items stay exactly where they are on the account plane
/// (`fauna.state.succession-ledger`), inherited by the
/// permanent page (`succession-aftermath.md` § Propagation, item (ii)). Never
/// touches the roster itself, only this witness.
pub(crate) fn clear_ephemeral_review_pass_witness(successor_actor_hex: &str) {
    if let Some(store) = session_storage() {
        let _ = store.remove_item(&ephemeral_review_pass_witness_key(successor_actor_hex));
    }
}

/// The `sessionStorage` key for the owed-kit obligation — one per successor,
/// same scoping reason as [`PendingCeremony::storage_key`], and here it is not
/// merely scoping but **the seat guard itself**: the obligation is claimable
/// only by the identity it was recorded for, so no other account activated in
/// this tab can take a kit that was owed to someone else. apple carries the
/// same rule as an explicit `successorActorIdHex` field on its
/// `SuccessionHandoff`, having paid for its absence (a dying view claimed the
/// kit and minted 147 ms after its own `onDisappear`, 2026-08-26); keying the
/// storage slot makes the same guard unrepresentable to get wrong.
fn kit_owed_key(successor_actor_hex: &str) -> String {
    format!("fauna_succession_kit_owed:{successor_actor_hex}")
}

/// Record that the successor owes itself a fresh RecoveryKey — the fourth and
/// last thing this ceremony parks across the document swap, and the one the
/// account is least able to do without.
///
/// **Why it must be written here, in the fold, on BOTH `persisted` arms.** The
/// succession transaction deletes the old `recovery_escrow` row and the old kit
/// retires with the old identity, so from the instant the statement lands until
/// this obligation is discharged the account has **no kit and no escrow at
/// all** — for a user who has just proven they are a theft target. The account
/// moved whether or not the seed persisted, so a fold that reached its end
/// without recording this would leave nothing anywhere remembering a kit is
/// owed (`identity-succession.md` § Implementation status today, the closing-act
/// bullet: "set in the succession fold the instant the ceremony lands — before
/// the switch and on the adoption-failure path too").
///
/// **Why the obligation and not the kit.** The mint authenticates *as the
/// successor*, which does not exist as a session until this document is gone.
/// So the ceremony cannot perform the closing act; it can only hand it forward.
/// That is the same reason tui declares `succession_kit_owed` to survive
/// `drop_authenticated_state` and apple keeps `SuccessionHandoff` out of
/// `ActorScope.resetSharedState()`.
///
/// Never fatal, like its three siblings: a landed succession is not worth
/// refusing over a bookkeeping write. Logged, because a silent loss here costs
/// the user the kit.
fn park_kit_owed(successor_actor_hex: &str) {
    let Some(store) = session_storage() else {
        tracing::warn!(
            "[wasm/succession] no sessionStorage to record the owed kit in — the successor \
             will not be offered one unbidden, and stays kitless until it creates one by hand"
        );
        return;
    };
    if let Err(e) = store.set_item(&kit_owed_key(successor_actor_hex), "1") {
        tracing::warn!(?e, "[wasm/succession] recording the owed kit failed");
    }
}

/// Whether `successor_actor_hex` still owes itself a kit — a **peek**, which is
/// the whole point: this answers the *navigation* question (does this launch
/// belong on the surface that renders a kit?) and must not consume an
/// obligation the surface has not yet discharged.
///
/// The split mirrors apple's, where each target's launch reads `kitOwed` to set
/// `selectedSettingsPage = .account` while the section's own hydrate does the
/// claiming. One reader decides where to land; exactly one claimant performs.
pub(crate) fn succession_kit_owed(successor_actor_hex: &str) -> bool {
    session_storage()
        .and_then(|store| {
            store
                .get_item(&kit_owed_key(successor_actor_hex))
                .ok()
                .flatten()
        })
        .is_some()
}

/// Take the obligation, clearing it in the same breath — the one-shot claim.
///
/// Returns `false` when nothing was owed, which is every ordinary sign-in and a
/// successor's *second* one. The claim is take-and-clear rather than
/// clear-on-success so two racing renders of the same section cannot both mint;
/// a mint that then fails is expected to call [`rearm_owed_succession_kit`].
pub(crate) fn claim_owed_succession_kit(successor_actor_hex: &str) -> bool {
    let Some(store) = session_storage() else {
        return false;
    };
    let key = kit_owed_key(successor_actor_hex);
    let owed = store.get_item(&key).ok().flatten().is_some();
    if owed {
        let _ = store.remove_item(&key);
    }
    owed
}

/// Put back an obligation whose mint did not reach the screen.
///
/// ⚠ **A failed mint must RE-ARM, never spend.** The ceremony revokes every
/// session of the account inside the nest's own transaction, so the successor's
/// first mint races its own reconnect **on every platform** — apple recorded
/// this as a cross-platform lesson after macOS passed on timing luck and iOS
/// did not (`identity-succession.md`, the closing-act bullet). Re-arming is safe
/// because the mint is a *replace*: `create_kit` re-reads the chain head and
/// picks its own arm, so a second mint supersedes a stranded first rather than
/// colliding with it. The cost of a spurious re-arm is one extra kit; the cost
/// of a missed one is an account whose only route back to a held kit is the
/// 30-day seed-alone window — which would additionally fire the
/// pending-replacement critical alert on the user's own remediation. That
/// asymmetry is the whole argument for erring this way.
///
/// Re-binds to the successor it is given, exactly like apple's
/// `rearmUnshownKit(successor:)`: never a general un-claim that could hand the
/// obligation to a different identity than the one that owed it.
pub(crate) fn rearm_owed_succession_kit(successor_actor_hex: &str) {
    park_kit_owed(successor_actor_hex);
}

/// The `sessionStorage` key for the owed-sweep obligation a **relaunch
/// adoption** records — one per successor, the seat guard by construction, for
/// [`kit_owed_key`]'s reason.
fn sweep_owed_key(successor_actor_hex: &str) -> String {
    format!("fauna_succession_sweep_owed:{successor_actor_hex}")
}

/// A launch refused as superseded whose **chain-verified** successor this
/// browser holds: adopt it (`identity-succession.md` § Implementation status
/// today, *a lost submit reply no longer destroys the account*). `true` means
/// the caller switches to `verified_successor` now.
///
/// Whether to adopt — and the succession link — is the shared
/// `AccountRegistry::adopt_held_successor`. What is parked here is what the
/// lost ceremony never reached, carried across the document swap beside its own
/// slots: the successor's kit ([`park_kit_owed`]) and the group sweep
/// (`succession-propagation.md` § Propagation → *Own device fleet*, the
/// relaunch-adoption clause), discharged by [`discharge_owed_sweep`]. tui's
/// `App::adopt_held_successor`, apple's `recordRelaunchAdoption` and windows'
/// `SuccessionHandoff.RecordRelaunchAdoption` are the twins.
///
/// ⚠ `verified_successor` must be `resolveVerifiedSuccessor`'s answer — the
/// registration chain's, never the nest's claim.
pub(crate) fn adopt_held_successor(predecessor: &str, verified_successor: &str) -> bool {
    if !account_registry().adopt_held_successor(predecessor, verified_successor) {
        return false;
    }
    park_kit_owed(verified_successor);
    let Some(store) = session_storage() else {
        tracing::warn!(
            "[wasm/succession] no sessionStorage to record the owed sweep in — the adopted \
             successor's groups will show no sweep line until the retry is pressed"
        );
        return true;
    };
    if let Err(e) = store.set_item(&sweep_owed_key(verified_successor), "1") {
        tracing::warn!(?e, "[wasm/succession] recording the owed sweep failed");
    }
    true
}

/// Discharge the sweep a relaunch adoption owes — the unbidden press of
/// `recovery-kit-sweep-retry-button`, claimed once and seat-bound by the key.
/// `None` when nothing was owed (every ordinary sign-in); otherwise the press's
/// sentence, for `error-message` as a press's would go.
///
/// The answer is [`succession_sweep_retry`]'s — on web always `NoOldState`, for
/// the reason that function gives — and what it parks is shared Rust's call
/// (`SweepRetryAnswer::into_owed_status`): an arm that still owes work, so the
/// retry button renders and the groups are never reported swept. Parked as the
/// view the Settings page paints and as the e2e state, exactly as a ceremony's
/// own report is.
pub(crate) fn discharge_owed_sweep(
    successor_actor_hex: &str,
) -> Option<fauna_core::localized::LocalizedText> {
    let store = session_storage()?;
    let key = sweep_owed_key(successor_actor_hex);
    store.get_item(&key).ok().flatten()?;
    let _ = store.remove_item(&key);
    let answer = fauna_client_recovery::ceremony::SweepRetryAnswer::NoOldState;
    let sentence = answer.message();
    let parked = answer.into_owed_status();
    publish_sweep_state_for_e2e(&parked);
    park_sweep_view(successor_actor_hex, &parked.render_view());
    sentence
}

/// The `sessionStorage` key for [`park_sweep_view`] — one per successor, same
/// scoping reason as [`PendingCeremony::storage_key`]: an unrelated account
/// activated in the same tab must not paint a sweep that was not its own.
fn sweep_view_key(successor_actor_hex: &str) -> String {
    format!("fauna_succession_sweep_view:{successor_actor_hex}")
}

/// Park the sweep's render view for the successor's Settings page.
///
/// The ceremony runs before the document swap and the lines render after it,
/// so this is web's twin of tui's `App::succession_sweep` (declared to outlive
/// the account switch) and of apple's/windows' `SuccessionHandoff` slot: the
/// **view**, never the lines — the copy is selected at paint time by the
/// shared `SweepView::copy`, so web's retry-affordance declaration lives in
/// exactly one place ([`succession_sweep_copy`]) rather than being frozen
/// into a parked string. Per-tab like every other slot here, and never fatal.
fn park_sweep_view(successor_actor_hex: &str, view: &fauna_client_recovery::ceremony::SweepView) {
    let Some(store) = session_storage() else {
        tracing::warn!(
            "[wasm/succession] no sessionStorage to park the sweep view in — the sweep's \
             lines will not render this session"
        );
        return;
    };
    match serde_json::to_string(view) {
        Ok(raw) => {
            if let Err(e) = store.set_item(&sweep_view_key(successor_actor_hex), &raw) {
                tracing::warn!(?e, "[wasm/succession] parking the sweep view failed");
            }
        }
        Err(e) => tracing::warn!(error = %e, "[wasm/succession] encoding the sweep view"),
    }
}

/// The sweep's own lines, as the SPA paints them — the shared `SweepCopy` plus
/// the retry gate, for the successor session the document swap separated
/// from the ceremony that produced them.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SweepCopyJs {
    /// What the sweep did, or `None` for a succession over an account with no
    /// groups at all (the shared projection's own silence).
    pub outcome: Option<fauna_core::localized::LocalizedText>,
    /// The roster the sweep cannot vouch for, as its own line, or `None`.
    pub unattested: Option<fauna_core::localized::LocalizedText>,
    /// The shared `SweepView::owes_work` — what `recovery-kit-sweep-retry-button`
    /// would gate on, once web builds it.
    pub owes_work: bool,
}

/// The sweep's lines for `successor_actor_hex`, or `None` when no succession
/// ran in this tab — the render source for the Settings page's sweep chrome.
///
/// **`Rendered` is web's declaration, made here and nowhere else** (flipped
/// 2026-08-27): the SPA paints
/// `recovery-kit-sweep-retry-button` on every arm `owes_work` answers true for,
/// so the degraded lines may name it. It renders even though **no** web device
/// can run the sweep (the module doc's reason: the retired identity's MLS state
/// rests in the nest replica behind bearers the succession revoked), because
/// `settings.md` § Recovery kit → *Finishing an unfinished group sweep* gates
/// the render on unfinished work and explicitly **not** on whether this device
/// can retry — a device that cannot answers in words instead of being silently
/// button-less. [`succession_sweep_retry`] is that answer.
pub(crate) fn succession_sweep_copy(successor_actor_hex: &str) -> Option<SweepCopyJs> {
    use fauna_client_recovery::ceremony::{SweepRetryAffordance, SweepView};
    let store = session_storage()?;
    let raw = store
        .get_item(&sweep_view_key(successor_actor_hex))
        .ok()
        .flatten()?;
    let view: SweepView = serde_json::from_str(&raw).ok()?;
    let copy = view.copy(SweepRetryAffordance::Rendered);
    Some(SweepCopyJs {
        outcome: copy.outcome,
        unattested: copy.unattested,
        owes_work: view.owes_work(),
    })
}

/// `recovery-kit-sweep-retry-button`'s press, on web.
///
/// **The answer is the whole gesture here, and it is always the same one.** The
/// retry needs the retired identity's own MLS state to author the old leaf's
/// commit; on web that state was never on this device — it rests in the nest
/// replica, behind bearers the succession revoked — so no browser can ever hold
/// it, and `NoOldState` is not a *this-session* accident but web's permanent
/// arm. Which is exactly why the sentence it resolves to had to stop naming the
/// ceremony device (`settings.md` § Recovery kit → *Finishing an unfinished
/// group sweep*, ratified 2026-08-27): on web that device is this one.
///
/// ⚠ **This is the shared answer, not a web-shaped one.** The wording comes
/// from `SweepRetryAnswer::message`, the same projection the native driver
/// answers through, so a native device holding no conversation history says the
/// identical words — the condition is what the sentence turns on, never the
/// platform. Nothing is posted and nothing is read: the store check precedes
/// the chain walk in the native driver too, so answering here without a round
/// trip is the same order, not a shortcut.
pub(crate) fn succession_sweep_retry() -> fauna_core::localized::LocalizedText {
    fauna_client_recovery::ceremony::SweepRetryAnswer::NoOldState
        .message()
        .expect("every non-sweeping retry answer carries a sentence")
}

/// The `sessionStorage` slot carrying the sweep's own account of what it did,
/// for the e2e state protocol's `succession_sweep` key.
///
/// Namespaced like the SPA's other test-only stored keys
/// (`fauna_e2e_session_generation`, `fauna_e2e_nest_dial_override`); the web
/// bridge's `agent.js` reads this exact string.
#[cfg(feature = "test-helpers")]
const E2E_SWEEP_STATE_KEY: &str = "fauna_e2e_succession_sweep";

/// Publish the sweep's account of itself so a journey can assert that a real
/// succession re-pointed the user's groups.
///
/// **Why this needs a store at all, rather than a live read.** The ceremony
/// renders its outcome as ID-less prose and then the app *navigates away*
/// (`performSwitch`), so by the time a journey can read anything the document
/// that held the report is gone. tui keeps it on `App::succession_sweep`,
/// declared to outlive `clear_session()` at the account switch; `sessionStorage`
/// is web's twin of exactly that declaration — the same reasoning
/// `$lib/generation-e2e.ts` records for the teardown counter, where an
/// in-memory value "would reset to 0 across exactly the relaunch this key
/// exists to detect — a false PASS, not a missing signal".
///
/// ⚠ **Gated on the feature ALONE, never on the profile** (convention 15 rule
/// (b), this crate's own `test-helpers` doc): the generated JS face must stay a
/// pure function of the feature set, and a production `just wasm` is a release
/// build, so `debug_assertions` would be no lever here.
#[cfg(feature = "test-helpers")]
fn publish_sweep_state_for_e2e(sweep: &SweepStatus) {
    let Some(store) = session_storage() else {
        return;
    };
    // The shape is the shared cross-app contract, never re-expressed here — the
    // journeys assert `groups` / `old_leaf_removed_everywhere` / `outcomes`,
    // not merely `status`, and a second spelling is how the render view already
    // drifted (`no-engine` there, `no_engine` in the contract).
    let _ = store.set_item(&E2E_SWEEP_STATE_KEY, &sweep.state_json().to_string());
}

/// No-op twin for every build without the feature, so the call site stays one
/// unconditional line instead of a `#[cfg]` block in the middle of the
/// ceremony.
#[cfg(not(feature = "test-helpers"))]
fn publish_sweep_state_for_e2e(_sweep: &SweepStatus) {}

thread_local! {
    /// Where the SPA's aftermath progress callback is parked so **leg 3** can reach
    /// it — the `__mls` re-seal's narration (the `BackupKey` corpus row).
    ///
    /// **Why a thread-local rather than a parameter, which is what every other leg
    /// uses.** Legs 1/2/4/6/7 all report from `run_aftermath_web`, which is handed
    /// the callback directly. Leg 3 cannot: it is a **barrier inside the
    /// conversations replica's own `load()`**, so it reports from a plane the SPA
    /// builds separately and earlier, and the sink has to be in place before that
    /// load rather than when the post-auth pass runs.
    ///
    /// **And why it cannot simply be boxed into the sink.**
    /// `ResealSink = Box<dyn Fn(..) + Send + Sync>` — a bound the native launchers
    /// genuinely need, since tui hands the progress to an mpsc sender — while a
    /// `js_sys::Function` is neither `Send` nor `Sync`. Parking it here keeps the
    /// closure below capture-free (and therefore trivially `Send + Sync`) without
    /// relaxing a bound five other apps rely on. Sound because wasm is
    /// single-threaded: the local is reached only from the one thread that set it.
    static MLS_RESEAL_SINK: std::cell::RefCell<Option<js_sys::Function>> =
        const { std::cell::RefCell::new(None) };
}

/// Register (or clear, with `None`) the SPA's aftermath progress callback for
/// leg 3. Idempotent; a later call replaces the earlier one, which is what an
/// actor switch wants.
pub(crate) fn set_mls_reseal_sink(cb: Option<js_sys::Function>) {
    MLS_RESEAL_SINK.with(|slot| *slot.borrow_mut() = cb);
}

/// The `ResealSink` body — leg 3's progress, localized by the **shared**
/// projection (`ReplicaResealProgress::status_line`) and filed under the SPA's
/// own `mlsReseal` field name, exactly as [`JsAftermathSink`] files the others.
///
/// A `None` line is a real value, not an absence (an outcome owing the user
/// nothing to read), and is passed through as JS `null` so the render hides
/// that line rather than leaving a stale one standing.
fn report_mls_reseal(progress: ReplicaResealProgress) {
    let line = progress.status_line();
    let cb = MLS_RESEAL_SINK.with(|slot| slot.borrow().clone());
    // The borrow above is dropped before the call below on purpose: the
    // callback re-enters wasm (it writes a Svelte store, whose subscribers can
    // run synchronously), and holding a `RefCell` borrow across that boundary
    // is how a re-entrant report becomes a panic instead of a paint.
    let Some(cb) = cb else { return };
    // ⚠ `crate::rpc::to_js`, never a bare `serde_wasm_bindgen::to_value` —
    // `LocalizedText::args` is a map, and the default serializer renders it as
    // a JS `Map`, which reaches `resolveLocalized` with no own properties and
    // silently drops every placeholder. Same reasoning `JsAftermathSink::emit`
    // states.
    let value = line
        .and_then(|l| crate::rpc::to_js(&l).ok())
        .unwrap_or(wasm_bindgen::JsValue::NULL);
    // A throwing or detached callback must not break the re-seal: the pass
    // behind it unlocks the successor's conversations, and a paint failure is
    // the least important thing happening here.
    if let Err(e) = cb.call2(
        &wasm_bindgen::JsValue::NULL,
        &wasm_bindgen::JsValue::from_str("mlsReseal"),
        &value,
    ) {
        tracing::warn!("[wasm/succession] the leg-3 progress callback threw: {e:?}");
    }
}

/// The capture-free `ResealSink` every web `MlsStateSync` is built with — see
/// [`MLS_RESEAL_SINK`] for why it dispatches through a thread-local instead of
/// closing over the callback.
pub(crate) fn mls_reseal_sink() -> fauna_client_mls_sync::sync::ResealSink {
    Box::new(report_mls_reseal)
}

/// The **ceremony context** is taken from the parking slot
/// ([`take_pending_ceremony`]) rather than held in memory: the SPA reloads as
/// the successor between the ceremony and this pass, so there is no object to
/// hang it off the way tui hangs it off `App`. `None` here is the ordinary
/// answer on every sign-in that did not just run a ceremony — including a
/// successor's second one — and it means exactly what it means on tui: no
/// roster to report, so the two silent raises no-op while every restoring leg
/// runs regardless.
pub(crate) async fn run_aftermath_web(
    client: InnerClient,
    secret_hex: &str,
    nest_url: &str,
    on_progress: js_sys::Function,
) -> Result<&'static str, String> {
    use fauna_client_recovery::aftermath::{AftermathInputs, run_succession_aftermath};

    let keypair = ActorKeypair::from_secret_hex(secret_hex)
        .map_err(|e| format!("reading this identity: {e}"))?;
    // The post-store-ready pass's progress half, stashed for its other edge
    // (`startAccountRuntime`); this call is one of its two edges — whichever
    // lands second runs it (`spawn_ledger_pass`).
    LEDGER_PROGRESS.with(|held| *held.borrow_mut() = Some(on_progress.clone()));
    // …and the mail machine that pass's leg 6 (the mail burn) drives: the burn
    // runs the shared rotate-mail-keys flow, which lives on the machine, and
    // this edge is the one that holds the identity and the nest URL.
    let mail_burn = fauna_client_mail_settings::rpc_glue::build_mail_settings_machine(
        client.clone(),
        ActorKeypair::from_secret_hex(secret_hex)
            .map_err(|e| format!("reading this identity: {e}"))?,
        crate::account_runtime::mail_store(),
        nest_url,
        // The burn's MSEK rotation heals bounded grants and records their
        // Renew events on the ledger.
        crate::account_runtime::ledger_seam(),
        // The burn never reads the served state.
        None,
    );
    LEDGER_MAIL_BURN.with(|held| *held.borrow_mut() = Some(std::sync::Arc::new(mail_burn)));
    spawn_ledger_pass(client.clone());
    // The registry's OWN walk, never a hand-rolled filter here
    // (`AftermathInputs::predecessor_keys`' ⚠). ⚠ Deliberately NOT
    // `predecessor_seeds`: that accessor is declared for escrow-container
    // producers, and this consumer only needs to *open* bytes — routing
    // through it handed a raw identity seed to a caller that never needed one.
    let registry = account_registry();
    // ⚠ The gate is "are there predecessor ROWS", not "did any resolve to
    // material this browser holds". It used to be the latter, which made this
    // answer "not-a-successor" on a successor's seat that simply never held the
    // predecessor's seed — the exact silent-nothing
    // `AftermathInputs::predecessor_keys` warns about, where the honest answer
    // is the drafts leg's owed-by-another-device line. tui has always gated
    // this way; web and the FFI apps were aligned onto it 2026-08-23.
    //
    // Learn a link this browser's registry does not hold BEFORE the gate reads
    // it — web's `fauna_client_recovery::ceremony::learn_succession_link`
    // (that seam is native; the read-prove-record it wraps is shared, and is
    // the same body the edit form's `loadProfileEditBase` runs). Best-effort: a
    // failure leaves the link unlearned until the next sign-in.
    if let Ok((base_body, _)) = fauna_client_recovery::ceremony::read_own_profile_learning_link(
        client.clone(),
        &registry,
        &keypair,
    )
    .await
    {
        // …and, over the same base read, the harvest anchors an external-app
        // edit left absent — the other half of that native seam
        // (`fauna_client_profile::restore_delegated_anchors` owns the rule).
        let home = crate::rpc::wasm_home_nest(&client);
        let predecessors = fauna_client_profile::predecessors_from_hex(
            &registry.predecessors_of(&keypair.actor_id_hex()),
        );
        match fauna_client_profile::restore_delegated_anchors(
            client.clone(),
            &keypair,
            &predecessors,
            base_body.as_deref(),
            Some(home),
        )
        .await
        {
            Ok(fauna_client_profile::AnchorRestore::Published) => {
                tracing::info!(
                    "[wasm/succession] restored the profile's anchors after a delegated edit"
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("[wasm/succession] restoring the profile's anchors: {e}"),
        }
    }
    if registry.predecessors_of(&keypair.actor_id_hex()).is_empty() {
        return Ok("not-a-successor");
    }
    let predecessor_keys = registry
        .predecessor_backup_keys_by_actor(&keypair.actor_id_hex())
        .into_iter()
        .map(|(_, key)| key)
        .collect();

    let mut sink = JsAftermathSink { on_progress };
    let inputs = AftermathInputs {
        owner_secret: *keypair.secret_bytes(),
        predecessor_keys,
    };
    run_succession_aftermath(client, inputs, &mut sink).await;
    Ok("ran")
}

thread_local! {
    /// The progress callback the most recent `runSuccessionAftermath` call
    /// handed over — the post-store-ready pass's sink half, late-populated
    /// like fauna-ffi's `LedgerPassSeams` for the same either-order reason.
    static LEDGER_PROGRESS: std::cell::RefCell<Option<js_sys::Function>> =
        const { std::cell::RefCell::new(None) };
    /// The mail-settings machine the post-store-ready pass's leg 6 (the mail
    /// burn) drives, built by the most recent `runSuccessionAftermath` call —
    /// the edge that holds the identity and the nest URL.
    static LEDGER_MAIL_BURN: std::cell::RefCell<
        Option<std::sync::Arc<fauna_client_mail_settings::MailSettingsMachine>>,
    > = const { std::cell::RefCell::new(None) };
}

/// Run the post-store-ready half of the aftermath
/// ([`fauna_client_recovery::ledger_aftermath::run_ledger_aftermath`]) once
/// both halves exist — this tab's account-store handle and the progress
/// callback. Called from BOTH edges (`crate::account_runtime::start` after
/// `wire_parts`, and [`run_aftermath_web`]), because the two land in either
/// order: whichever comes second runs it; both may, which is harmless — a
/// second run puts nothing. A no-op while either half is missing.
///
/// It drains a ceremony the succession fold parked in the registry (the
/// member-item, filter-mark and destination-mark raises), runs leg 2's
/// `NestBackupKey` re-grant off the bound box's `fauna.state.backup` list (its
/// `backupRegrant` line fires from here, not from [`run_aftermath_web`]), then
/// reports `configStageSettled` so the SPA re-reads the review surfaces.
pub(crate) fn spawn_ledger_pass(client: InnerClient) {
    let (Some(handle), Some(on_progress)) = (
        crate::account_runtime::handle(),
        LEDGER_PROGRESS.with(|held| held.borrow().clone()),
    ) else {
        return;
    };
    let mail_burn = LEDGER_MAIL_BURN.with(|held| held.borrow().clone());
    wasm_bindgen_futures::spawn_local(async move {
        let mut sink = JsAftermathSink { on_progress };
        // The same handle is the period-key custody legs 4 and 8 read.
        let period_keys: fauna_client_subscriptions::SharedPeriodKeyStore =
            std::sync::Arc::new(handle.clone());
        // Leg 2 (the `NestBackupKey` re-grant) reconciles from THIS box's
        // `fauna.state.backup` list, so it needs the id the connection is
        // bound to. An id this pass cannot prove is `None`, which skips leg 2
        // for this pass — never a guess, never another box's list.
        let bound_nest = match crate::rpc::bound_nest_id(&client).await {
            Ok(id) => Some(id.0),
            Err(e) => {
                tracing::warn!(
                    ?e,
                    "[wasm/succession] could not prove the bound nest id; leg 2 waits for the next pass"
                );
                None
            }
        };
        // The succession cut's custody arm runs over this tab's account's
        // folder-key custody (`writer-signed-change-records.md` ruling (11)(a)).
        let custody_cut = {
            use fauna_client_config::SuccessionLedgerStore as _;
            handle.self_actor().ok().map(|actor| {
                fauna_client_folders::SetCustodyCut::new(
                    fauna_client_folders::FoldersClient::new(client.clone()),
                    crate::account_runtime::folder_key_store(actor.to_hex()),
                )
            })
        };
        let Some(custody_cut) = custody_cut else {
            tracing::warn!("[wasm/succession] the runtime names no account; the ledger pass waits");
            return;
        };
        let parked = fauna_client_recovery::ledger_aftermath::run_ledger_aftermath(
            client,
            &handle,
            &handle,
            bound_nest,
            period_keys,
            &handle,
            mail_burn,
            &custody_cut,
            &account_registry(),
            &mut sink,
        )
        .await;
        tracing::debug!(
            ?parked,
            "[wasm/succession] the post-store-ready aftermath pass settled"
        );
    });
}

/// Web's [`AftermathSink`](fauna_client_recovery::aftermath::AftermathSink):
/// each leg's already-localized status line becomes one
/// `(leg, line | null) => void` call into the SPA, which files it under the
/// matching `recoveryAftermath` field and renders it through the same
/// `resolveLocalized` helper every other `LocalizedText` on the page uses.
///
/// **The leg keys are the SPA's field names**, so the callback indexes the
/// store directly rather than matching a second vocabulary into it — the same
/// no-per-key-match-table reasoning the render layer already documents.
///
/// `null` is a real value here, not an absence: a leg whose outcome owes the
/// user nothing (`NothingConfigured`, `AlreadyEnrolled`, …) reports `None` from
/// its own `status_line`, and the render hides the line. Deciding that here
/// would be a second answer to a question the shared projection already owns.
struct JsAftermathSink {
    on_progress: js_sys::Function,
}

impl JsAftermathSink {
    fn emit(&self, leg: &str, line: Option<fauna_core::localized::LocalizedText>) {
        // ⚠ `crate::rpc::to_js`, never a bare `serde_wasm_bindgen::to_value`:
        // that helper exists because the default serializer renders a Rust map
        // as a JS `Map`, and `LocalizedText::args` is a map. A `Map` reaches
        // `resolveLocalized` as an object with no own properties, so every
        // placeholder in a line would silently render unsubstituted — the
        // failure mode being visible only in the one line the user reads.
        let value = line
            .and_then(|l| crate::rpc::to_js(&l).ok())
            .unwrap_or(wasm_bindgen::JsValue::NULL);
        // A throwing or detached callback must not abort the pass: the legs
        // behind it repair real breaks, and a paint failure is the least
        // important thing happening here.
        if let Err(e) = self.on_progress.call2(
            &wasm_bindgen::JsValue::NULL,
            &wasm_bindgen::JsValue::from_str(leg),
            &value,
        ) {
            tracing::warn!("[wasm/succession] the aftermath progress callback threw: {e:?}");
        }
    }
}

impl fauna_client_recovery::aftermath::AftermathSink for JsAftermathSink {
    fn backup_regrant(&mut self, progress: fauna_client_config::BackupRegrantProgress) {
        self.emit("backupRegrant", progress.status_line());
    }
    fn grant_remint(&mut self, progress: fauna_client_capabilities::GrantRemintProgress) {
        self.emit("grantRemint", progress.status_line());
    }
    fn drafts_reseal(&mut self, progress: fauna_client_drafts::DraftsResealProgress) {
        self.emit("draftsReseal", progress.status_line());
    }
    fn mail_burn(&mut self, progress: fauna_client_mail_settings::MailBurnProgress) {
        self.emit("mailBurn", progress.status_line());
    }
    /// Not a leg — it carries no line. It tells the SPA the post-store-ready
    /// pass's two silent raises have had their turn, so the review surfaces
    /// re-read their marks now rather than at the next visit (linux's and
    /// apple's sinks do the same read here).
    async fn config_stage_settled(&mut self) {
        self.emit("configStageSettled", None);
    }
}

/// Build the account registry over this browser's `localStorage` secret store.
///
/// Hoisted for the reason tui's is: the unconfirmed arm READS BACK through the
/// same registry to decide whether it may claim the seed is saved, and a second
/// registry over a second store handle could answer about a different store than
/// the one just written to.
pub(crate) fn account_registry() -> AccountRegistry {
    AccountRegistry::new(Arc::new(LocalStorageSecretStore))
}

/// Ask the registry for the secret back — the ONLY proof the persist took.
///
/// `SecretStore::set` is infallible by signature, so a clean `add_account`
/// return proves the call was made and nothing more. The equality half
/// is defence-in-depth against a corrupt store, exactly as native's is.
fn persist_verified(accounts: &AccountRegistry, actor_id: &ActorId, secret_hex: &str) -> bool {
    accounts
        .secrets(&actor_id.to_hex())
        .is_some_and(|stored| stored.secret_hex.as_str() == secret_hex)
}

/// Open the successor's OWN authenticated session.
///
/// The succession transaction revoked the old identity's bearers, so the
/// connection that ran the ceremony is already dead — the successor needs its
/// own, and it must exist BEFORE its engine does (native's reason: an
/// unreachable nest must fail before anything is written). On web the
/// pre-identity handshake mints the bearer, mirroring
/// `rpc::resolve_and_authorize_destination_inner`'s mint-then-reconnect shape.
async fn connect_as_successor(
    dial_url: &str,
    successor: &ActorKeypair,
) -> Result<TokenWsRpcClient, String> {
    use fauna_client_core::succession_delivery::SignIn;
    match sign_in(dial_url, successor).await {
        SignIn::Connected(client) => Ok(client),
        SignIn::NoAccount => {
            Err("connecting as the successor: the nest holds no account for it".into())
        }
        SignIn::Failed(e) => Err(format!("connecting as the successor: {e}")),
        SignIn::NoSeed => unreachable!("a sign-in with the keypair in hand"),
    }
}

/// Sign in at `dial_url` as `keypair`: the pre-identity handshake on an
/// anonymous socket mints the bearer, and a token client carries it — the
/// successor's own session above, and the road's sign-in as a retired
/// identity (`crate::account_runtime`'s owed-nest reach). A refusal of the
/// handshake keeps its code, so `fauna.auth.not_registered` reads as
/// [`SignIn::NoAccount`] (the one shared reading,
/// `fauna_client_core::succession_delivery::SignIn::from_error`).
///
/// [`SignIn::NoAccount`]: fauna_client_core::succession_delivery::SignIn::NoAccount
pub(crate) async fn sign_in(
    dial_url: &str,
    keypair: &ActorKeypair,
) -> fauna_client_core::succession_delivery::SignIn<TokenWsRpcClient> {
    use fauna_client_core::succession_delivery::SignIn;
    use fauna_protocol::auth::HandshakeReply;

    let anon = match AnonymousWsRpcClient::connect(dial_url) {
        Ok(anon) => anon,
        Err(e) => return SignIn::Failed(e.to_string()),
    };
    let req = match crate::build_handshake_request(&anon, dial_url, keypair).await {
        Ok(req) => req,
        Err(e) => {
            return SignIn::Failed(e.as_string().unwrap_or_else(|| "nest identity".into()));
        }
    };
    let handshake: HandshakeReply = match anon.request("fauna.auth.handshake", req).await {
        Ok(handshake) => handshake,
        Err(e) => return SignIn::from_error(&e),
    };
    drop(anon);
    match TokenWsRpcClient::connect(dial_url, &keypair.actor_id_hex(), &handshake.token) {
        Ok(client) => SignIn::Connected(client),
        Err(e) => SignIn::Failed(e.to_string()),
    }
}

/// Re-point every MLS group the succeeded identity held — web's
/// [`fauna_client_recovery::ceremony::sweep_as_successor`].
///
/// Same ordering, same degrade-to-a-report failure policy (the account has
/// already moved; losing group re-key is bad, reading it as "the succession
/// failed" would be worse). The two differences are the transport above and the
/// persistence below: a web engine has no SQLite to `save_state()` into, so both
/// engines land in their owners' **nest replica** planes instead — the old one
/// through the live conversations session's own sync, the successor's through a
/// fresh one over the session just opened for it.
async fn sweep_as_successor_web(
    dial_url: &str,
    old: Option<&OldEngineHandle>,
    successor_secret_hex: &str,
    statement: &fauna_core::recovery::SignedIdentitySuccession,
) -> SweepStatus {
    let Some(old) = old else {
        return SweepStatus::NoEngine;
    };
    // Built twice from the one secret rather than cloned: `ActorKeypair` is
    // deliberately not `Clone` (it is signing key material), and the engine and
    // the replica sync each need their own.
    let build_successor = || ActorKeypair::from_secret_hex(successor_secret_hex);
    let signer = match build_successor() {
        Ok(keypair) => keypair,
        Err(e) => return SweepStatus::Failed(format!("{e}")),
    };
    let client = match connect_as_successor(dial_url, &signer).await {
        Ok(client) => client,
        Err(e) => return SweepStatus::Failed(e),
    };
    let successor = match build_successor() {
        Ok(keypair) => keypair,
        Err(e) => return SweepStatus::Failed(format!("{e}")),
    };
    // In-memory, because every web engine is: the durable copy is the replica
    // plane, written below.
    let successor_engine = match MlsEngine::new_in_memory(successor) {
        Ok(engine) => engine,
        Err(e) => return SweepStatus::Failed(format!("{e}")),
    };
    let recovery = RecoveryClient::new(client.clone());
    let report =
        fauna_client_recovery::sweep_groups(&recovery, &old.engine, &successor_engine, statement)
            .await;

    // Persistence is the driver's, per the shared module's own doc. BOTH engines
    // moved — the old one ratcheted past remove-old, the successor joined every
    // group — so both must land before the page reloads as the successor and
    // this pair stops existing.
    old.save().await;
    let successor_signer = match build_successor() {
        Ok(keypair) => keypair,
        Err(e) => {
            tracing::error!("[wasm/succession] rebuilding the successor signer to persist: {e}");
            return SweepStatus::Ran(Box::new(report));
        }
    };
    let sync = MlsStateSync::new(
        Box::new(WsMlsReplicaTransport::new(client)),
        &successor_signer,
    );
    // Provider only: the successor has no message history of its own yet (the
    // slices belong to the session that is about to be torn down), and what the
    // post-switch session must find is the crypto state carrying its new leaf.
    //
    // `publish_provider`, not `save_provider_if_changed` — the latter no-ops
    // before a successful `load()`, and this sync never loads, so until
    // 2026-08-27 this write never happened: the post-reload session found the
    // PREDECESSOR's snapshot at its path (moved there by the nest, re-keyed by
    // the `__mls` re-seal) and restored it, seating the successor as the old
    // leaf. The publish replaces that occupant outright — no merge, which would
    // re-import the retired leaf — and it is the only durable home the
    // successor's join has on web (`succession-aftermath.md` § Re-key scope →
    // *What a successor's replica restore may take from a predecessor's*).
    if let Err(e) = sync
        .publish_provider(&ProviderReplica::from_engine(&successor_engine))
        .await
    {
        tracing::error!("[wasm/succession] publishing the successor's swept MLS state: {e}");
    }
    SweepStatus::Ran(Box::new(report))
}

/// Finish a succession whose submit reply never arrived — web's
/// [`fauna_client_recovery::ceremony::finish_unconfirmed_succession`].
///
/// The nest commits the succession *before* it replies, so the ceremony can
/// return "failed" about an account that has already moved. The reconcile gets
/// its own ANONYMOUS connection for native's reason, which holds twice over in a
/// browser: the overwhelmingly likely reason we are here is that the connection
/// which ran the ceremony died, and even when it did not, the succession revoked
/// its bearer. `succession.lookup` and `registration.chain` are both pre-identity
/// kinds, so anonymous is all the verification needs.
///
/// The caller has already persisted the seed and verified it by read-back when
/// this runs; `persisted` is that verdict, and it decides which of the two ways
/// back into the account the undecidable arms name.
async fn finish_unconfirmed_web(
    dial_url: &str,
    old: Option<&OldEngineHandle>,
    old_actor_id: ActorId,
    successor_secret_hex: &str,
    persisted: bool,
    reported: &fauna_client_recovery::RecoveryError,
) -> StolenOutcome {
    // The persisted seed as a keypair: the reconcile matches on the successor we
    // hold the KEY for, never on a successor the nest names, so a nest that
    // invents one cannot make this device adopt it.
    let successor = match ActorKeypair::from_secret_hex(successor_secret_hex) {
        Ok(successor) => successor,
        Err(e) => {
            // Native's reasoning: undecidable, and with no key no read-back can
            // license "saved on this device".
            return StolenOutcome::undecided(
                format!("the successor key this device minted is unreadable ({e})"),
                false,
                successor_secret_hex,
                reported,
            );
        }
    };
    let anon = match AnonymousWsRpcClient::connect(dial_url) {
        Ok(anon) => anon,
        Err(e) => {
            // Undecided, and undecidable from here. Say so plainly rather than
            // reporting the original failure as final.
            return StolenOutcome::undecided(
                format!("the nest could not be reached to check ({e})"),
                persisted,
                successor_secret_hex,
                reported,
            );
        }
    };
    let client = RecoveryClient::new(anon);
    match fauna_client_recovery::reconcile_succession(&client, old_actor_id, &successor, None).await
    {
        // It landed after all. Finish the ceremony as if the reply had arrived.
        Ok(ReconciledSuccession::Landed(handoff)) => {
            tracing::info!(
                "[wasm/succession] the succession landed despite the lost reply; finishing"
            );
            let sweep = sweep_as_successor_web(
                dial_url,
                old,
                handoff.successor_secret_hex(),
                &handoff.statement,
            )
            .await;
            StolenOutcome::Landed(LandedSuccession::new(
                handoff.successor_secret_hex().to_string(),
                handoff.new_actor_id,
                sweep,
                handoff.succeeded_at,
            ))
        }
        // Nothing committed: the minted seed authorizes nothing, the account is
        // still the user's old identity, and the honest thing to show is the
        // failure that actually happened. They may simply try again.
        Ok(ReconciledSuccession::NotLanded) => StolenOutcome::not_landed(reported),
        // Someone else's ceremony won. `old_actor_id` is the succession table's
        // primary key, so this is final — and must never read as "try again".
        Ok(ReconciledSuccession::LandedForAnother { new_actor_id }) => {
            StolenOutcome::LandedForAnother { new_actor_id }
        }
        Err(e) => StolenOutcome::undecided(
            format!("checking with the nest failed ({e})"),
            persisted,
            successor_secret_hex,
            reported,
        ),
    }
}

/// Run the succession ceremony — `identity-stolen-button`'s whole body.
///
/// Take the account back from a stolen secret using the kit in hand: mint a
/// successor identity, re-point the account to it, and refuse the old key from
/// then on (`identity-succession.md` § The succession statement).
///
/// ⚠ **No status re-read is folded in here**, unlike every other recovery
/// ceremony on this client. The succession transaction revokes this connection's
/// bearer, so a re-read would race a dying session and report a failure that
/// means nothing; the status the user sees next is the SUCCESSOR's, read after
/// the switch by the section's own post-auth hydrate.
///
/// ⚠ **`nest_url` and `dial_url` are two different strings on web and must not
/// be collapsed** — the split native does not have (`long-term-store.md`
/// § Implementation status today; web's `storedNestUrl()` vs `nodeUrl()`, its
/// twin of `fauna_launch_machine::dial::resolved_dial_url`). `nest_url` is the
/// nest the successor's account row is BOUND to — the truth, and the only thing
/// written to the registry. `dial_url` is where sockets actually go. They are
/// the same string in production and differ under e2e automation, which is
/// exactly where this ceremony is exercised: binding the row to a dial override
/// would leave the successor pointing at a URL that stops existing when the test
/// ends.
pub(crate) async fn succeed_with_held_kit_web(
    client: InnerClient,
    secret_hex: &str,
    phrase: &str,
    nest_url: &str,
    dial_url: &str,
    old: Option<&OldEngineHandle>,
) -> Result<StolenOutcomeJs, String> {
    let identity = ActorKeypair::from_secret_hex(secret_hex)
        .map_err(|e| format!("reading this identity: {e}"))?;
    let old_actor_id = identity.actor_id();
    let recovery = RecoveryClient::new(client);
    // The old identity is passed as `old_identity`: this browser still holds the
    // seed (it is signed in), and `old_sig` is informational continuity — never
    // load-bearing (`identity-succession.md` § The succession statement), so
    // supplying it changes no consumer's verdict.
    // A refusal before the submit is the not-landed arm, not a fault: nothing
    // moved, and the seed it minted authorizes nothing.
    let attempt = match fauna_client_recovery::succeed_with_held_kit(
        &recovery,
        old_actor_id,
        phrase,
        Some(&identity),
    )
    .await
    {
        Ok(attempt) => attempt,
        Err(e) => return Ok(StolenOutcomeJs::unlanded(&StolenOutcome::not_landed(e))),
    };

    // FIRST, before anything that can fail or block: make the successor seed
    // durable. From the nest's commit until this line, that seed is the only copy
    // of the key the account now belongs to, and the sweep below makes network
    // calls over every group.
    //
    // Read off the attempt rather than a matched arm: BOTH arms carry the seed,
    // and the `Unconfirmed` one is precisely the case where the nest may already
    // have committed while telling us it did not. Persisting only the
    // confirmed arm would reproduce that finding one layer up.
    let accounts = account_registry();
    let successor_secret_hex = attempt.successor_secret_hex().to_string();
    // A registry mutator, so it runs inside the cross-tab mutation lock like
    // every other one (`accounts.rs`'s module note); the read-back below is a
    // read and takes none.
    let persist = {
        let secret_hex = successor_secret_hex.clone();
        let nest_url = nest_url.to_string();
        fauna_client_accounts::with_web_mutation_lock(move || {
            account_registry().add_account(&secret_hex, Some(&nest_url), None)
        })
        .await
    };
    if let Err(e) = persist {
        // Not fatal: the outcome carries the secret to the surface, which shows
        // it as the way back. Logged because this is the moment of maximum
        // exposure.
        tracing::error!(
            "[wasm/succession] persisting the successor seed straight after the succession \
             landed: {e}"
        );
    }
    // The id the ceremony derived from that same seed — never re-derived here:
    // one derivation, one answer, and the read-back below asks the store about
    // the row `add_account` just keyed.
    let persisted = persist_verified(
        &accounts,
        &attempt.successor_actor_id(),
        &successor_secret_hex,
    );

    let outcome = match attempt {
        SuccessionAttempt::Confirmed(handoff) => {
            let sweep = sweep_as_successor_web(
                dial_url,
                old,
                handoff.successor_secret_hex(),
                &handoff.statement,
            )
            .await;
            StolenOutcome::Landed(LandedSuccession::new(
                handoff.successor_secret_hex().to_string(),
                handoff.new_actor_id,
                sweep,
                handoff.succeeded_at,
            ))
        }
        SuccessionAttempt::Unconfirmed(unconfirmed) => {
            finish_unconfirmed_web(
                dial_url,
                old,
                old_actor_id,
                unconfirmed.successor_secret_hex(),
                persisted,
                &unconfirmed.error,
            )
            .await
        }
    };
    let StolenOutcome::Landed(landed) = &outcome else {
        return Ok(StolenOutcomeJs::unlanded(&outcome));
    };

    // Record the predecessor → successor LINK, now that an arm has actually
    // resolved to a landed succession.
    //
    // ⚠ Without this the ceremony "works" and the whole aftermath silently does
    // nothing. `predecessors_of` walks `succeeded_by`, which ONLY
    // `record_succession` writes, so a browser missing this link answers "no
    // predecessors" to every consumer that asks: the `__mls` re-seal, `predecessor_backup_keys`, the escrow re-put's
    // predecessor section. Each then degrades quietly — a stuck config plane
    // reads as an empty list, not as an error. Caught by
    // `test_the_successor_can_still_read_the_config_it_inherited --app web`,
    // which passed the ceremony and failed on `words=[]`.
    //
    // Placed HERE, after the match, rather than beside `add_account` above: the
    // seed must be persisted before anything that can fail or block, but the
    // *link* is a claim that the account moved, and on the unconfirmed arm that
    // is not known until the reconcile answers. `landed.new_actor_id` is the
    // identity an arm actually resolved to — never the one the attempt claimed.
    //
    // Never fatal, exactly as tui's `adopt_successor` treats it: the account has
    // already moved, and refusing the whole ceremony over a bookkeeping write
    // would strand the user for a reason the next sign-in could fix.
    //
    // Parked FIRST, in the same registry: what only this ceremony knows — the
    // identity we succeeded FROM, the roster the sweep could not vouch for,
    // and the nest's own commit stamp — is the member-item and filter-mark
    // raises' input, drained by the successor's post-store-ready pass after
    // the document swap. The single durable decision point of those raises
    // (`PendingCeremony`'s doc). ⚠ The roster is read off the report the sweep
    // just produced and never re-derived later — the report is the membership
    // as it stood across the compromise window (`succession-aftermath.md`
    // § Propagation).
    PendingCeremony::new(
        &old_actor_id,
        &landed.sweep.review_roster(),
        landed.succeeded_at,
    )
    .park(&accounts, &landed.new_actor_id.to_hex());
    if let Err(e) =
        accounts.record_succession(&old_actor_id.to_hex(), &landed.new_actor_id.to_hex())
    {
        tracing::error!(
            "[wasm/succession] recording the predecessor link — the aftermath passes will find \
             no predecessors until this is repaired: {e}"
        );
    }

    // The ephemeral review pass's render gate — witnessed here, the instant a
    // ceremony really did just run (the fact tui's `App::succession_sweep`
    // witnesses), and read across the document swap.
    mark_ephemeral_review_pass_witness(&landed.new_actor_id.to_hex());

    // The same document swap that separates the ceremony from the aftermath
    // also separates it from every journey that wants to know what the sweep
    // did, so the report is published across it too. Compiled out entirely
    // without `test-helpers`.
    publish_sweep_state_for_e2e(&landed.sweep);
    // And from the successor's own Settings page, which paints the sweep's
    // lines after the swap — so the render view is parked across it as well.
    park_sweep_view(&landed.new_actor_id.to_hex(), &landed.sweep.render_view());
    // The ceremony's CLOSING ACT, handed to the only session that can perform
    // it. Recorded here rather than on either `persisted` arm above precisely
    // because both arms reach this point: the account moved whether or not the
    // seed was saved, and it is kitless and escrowless until the successor
    // discharges this (`identity-succession.md` § The RecoveryKey → *At
    // succession*).
    park_kit_owed(&landed.new_actor_id.to_hex());

    let (sweep, sweep_detail) = sweep_view(&landed.sweep);
    Ok(StolenOutcomeJs {
        kind: outcome.kind(),
        message: None,
        landed: Some(LandedSuccessionJs {
            secret_hex: landed.successor_secret_hex.to_string(),
            new_actor_id: landed.new_actor_id.to_hex(),
            persisted,
            sweep,
            sweep_detail,
            succeeded_at: landed.succeeded_at,
        }),
        carries_the_only_seed: false,
    })
}
