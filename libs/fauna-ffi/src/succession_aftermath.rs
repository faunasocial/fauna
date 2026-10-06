//! UniFFI façade for the **post-succession aftermath**
//! (`docs/goal/behavior/succession-aftermath.md` § Implementation status today;
//! § Re-key scope's `BackupKey` corpus row — *"started at first successor
//! sign-in, surfaced with progress, resumed until complete"*) — the pass the
//! four FFI apps (macOS, iOS, windows, android) owe at a successor's first
//! authenticated session.
//!
//! **This module composes; it does not sequence.** The order the legs run in,
//! and which of them are barriers, is the whole safety property, and it lives
//! once in [`fauna_client_recovery::aftermath::run_succession_aftermath`] —
//! lifted there on 2026-08-21 precisely so the second app to drive it would not
//! re-derive the reasons and the seventh would not have six chances to get one
//! wrong. tui consumes it directly; web drives the same function over browser
//! transports (`libs/fauna-wasm/src/succession.rs::run_aftermath_web`). What was
//! missing was any route to it from this crate at all, so a successor on
//! macOS/iOS/windows/android reached its first session with every inherited
//! blob still sealed to the retired identity — the corpus is *stuck, not
//! corrupt*, and nothing on the device was doing anything about it.
//!
//! **Why one export and not five.** Same reasoning as [`crate::recovery`]'s
//! ceremony: exporting the legs would put the ordering back into every app,
//! which is the shape the shared driver exists to remove. The app calls
//! [`run_succession_aftermath`] once, at its post-auth hook, and renders what
//! arrives on the sink.
//!
//! ## What this module does NOT drive
//!
//! One of the seven legs deliberately runs elsewhere on *every* app, so its
//! absence here is the design rather than a gap:
//!
//! * **Leg 5, the file-corpus re-seal** — the sync agent's, in no app process.
//!
//! **Leg 3, the `__mls` re-seal, is a partial exception.** It is still a
//! barrier *inside* `MlsStateSync::load`, ahead of the conversations plane's
//! own restore, and still not forked in here — `crate::mls_sync_launch`
//! assembles it, and the caller of `conversations_session`/`_over_manager`
//! resolves the predecessor list off its own account registry
//! (`FfiAccountRegistry::predecessor_backup_keys`) and passes it through,
//! mirroring leg 5's own resolution shape. What changed
//! : this function now registers its `sink` onto
//! [`crate::nest_client::FfiNestClient::set_aftermath_sink`] before running
//! the shared pass, so leg 3 reports through the **same** sink the app
//! already supplied here — whichever of the two builds first, since the
//! `__mls` reseal reads the holder at the moment its own pass reports, not at
//! session-build time.
//!
//! Every leg reports into the same progress surface from where its work
//! happens.

use std::sync::Arc;

use fauna_client_config::BackupRegrantProgress;
use fauna_client_recovery::aftermath::{
    AftermathInputs, AftermathSink, run_succession_aftermath as run_shared,
};
use fauna_core::identity::ActorKeypair;
use fauna_core::localized::LocalizedText;

use crate::FfiError;
use crate::accounts_registry::FfiAccountRegistry;
use crate::crypto::secret32;
use crate::nest_client::FfiNestClient;

/// Which leg a [`FfiAftermathSink::progress`] call is reporting.
///
/// The numbering is § Re-key scope's, kept rather than renumbered to 1..5 so a
/// reader can move between this enum, the goal doc and the shared driver's
/// comments without a translation table. Leg 5 is the one leg this module does
/// not drive — see the module doc — and leg 1, the blob re-seal, retired
/// with the blob rail (`config-dissolution.md` § The `__config` dissolution
/// schedule → *The closure order*, step (6)). Leg 3 (`MlsReseal`) reports
/// from a different pass than the other five (see the module doc's "partial
/// exception"), but shares this enum and the same sink.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiAftermathLeg {
    /// Leg 2 — the `NestBackupKey` re-grant. Reported by the post-store-ready
    /// pass ([`spawn_ledger_pass`]), not the post-auth one: the destinations it
    /// re-grants to live in the account plane's `fauna.state.backup`.
    BackupRegrant,
    /// Leg 3 — the `__mls` re-seal. Reports from inside `MlsStateSync::load`
    /// (`crate::mls_sync_launch`), not from this function — see the module
    /// doc's "partial exception".
    MlsReseal,
    /// Leg 4 — the capability-grant re-mint.
    GrantRemint,
    /// Leg 7 — the `__drafts` re-seal.
    DraftsReseal,
    /// Leg 6 — the mail burn.
    MailBurn,
}

/// Where each leg reports — the app's own progress surface, one call per edge.
///
/// **`line` is `None` as a real value, not an absence.** A leg whose outcome
/// owes the user nothing (`NothingConfigured`, `AlreadyEnrolled`, …) reports
/// `None` from the shared projection's own `status_line`, and the app hides
/// that line. Deciding it app-side would be a second answer to a question the
/// shared projection already owns — which is exactly how seven apps end up
/// saying seven things about one outcome.
///
/// Every method is called from the tokio runtime the export runs on, so an
/// implementation must not block on the app's main thread.
#[uniffi::export(with_foreign)]
pub trait FfiAftermathSink: Send + Sync {
    /// One leg moved: `Running` before it, its settled/failed value after —
    /// the same two edges every app's surface renders.
    fn progress(&self, leg: FfiAftermathLeg, line: Option<LocalizedText>);

    /// Fired once, at the end of the post-store-ready pass
    /// ([`spawn_ledger_pass`]), once the silent raises have had their turn on
    /// the succession ledger.
    ///
    /// It exists so an app that renders the review surfaces re-reads them there
    /// and nowhere else: a successor's very first session then shows the marks
    /// the ceremony it just ran produced. An app with no such surface leaves
    /// this a no-op.
    ///
    /// ⚠ **Unconditional — it fires even when a raise was refused.** The
    /// surfaces render items raised by *earlier* ceremonies too, and those are
    /// already at rest.
    fn config_stage_settled(&self);
}

/// Whether the pass ran. Every result reaches the app through the sink.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiAftermathOutcome {
    /// This identity has no predecessors, so no pass ran. The overwhelmingly
    /// common answer, and **not** an error.
    NotASuccessor,
    /// The pass ran; each leg reported its own outcome through the sink.
    Ran,
}

/// Run the post-succession aftermath: legs 2, 4, 7, 6, in that order, with
/// the barriers between them that make the order mean something.
///
/// **Call this at the post-auth hook of every authenticated session, not only
/// after a ceremony.** The pass is resumable by design and returns
/// [`FfiAftermathOutcome::NotASuccessor`] having done nothing for an identity
/// that never succeeded, which is the overwhelmingly common case — so gating
/// the call on "did we just succeed" is both unnecessary and wrong: a re-seal
/// interrupted by a lost connection is finished by the *next* sign-in, and that
/// sign-in has no ceremony to notice.
///
/// **Every leg is best-effort and none is fatal to the session.** The account is
/// already the successor's; refusing a sign-in over a pass that can retry would
/// be strictly worse than a plane that is briefly still owed. This function
/// therefore reports through `sink` and its return value, and errors only when
/// it could not *start* — a malformed secret, which is a caller bug.
///
/// ## Parameters
///
/// `owner_secret` is this session's own 32-byte identity seed — the
/// successor's. `app_data_dir` is not read: it located the device-local
/// replica, which retired with the blob rail, and stays only so the
/// apps' callers keep compiling until their trickle-down drops it.
#[fauna_uniffi_async::export]
pub async fn run_succession_aftermath(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    app_data_dir: String,
    accounts: Arc<FfiAccountRegistry>,
    sink: Arc<dyn FfiAftermathSink>,
) -> Result<FfiAftermathOutcome, FfiError> {
    // Leg 3 reports from `crate::mls_sync_launch`'s launcher — built
    // separately over the same `nest`, possibly before, during or after this
    // call — so register this sink there NOW, ahead of the early
    // `NotASuccessor` return below, rather than only on the path that finds
    // predecessors: whichever of the two builds first, the `__mls` reseal
    // then reports through the same sink (module doc's "partial exception").
    // Cheap and idempotent — a later sign-in's call simply replaces it.
    nest.set_aftermath_sink(Arc::clone(&sink));
    // The post-store-ready pass's registry half, stashed beside the sink; this
    // call is one of the pass's two edges (the account runtime's installed
    // arm is the other) — whichever lands second runs it.
    let seams = nest.ledger_pass_seams();
    *seams.registry.lock().unwrap() = Some(Arc::clone(&accounts));
    spawn_ledger_pass(nest.nest_arc(), &seams);

    let keypair = ActorKeypair::from_secret(secret32(&owner_secret)?);
    let actor_hex = keypair.actor_id().to_hex();

    // ⚠ The gate is "are there predecessor ROWS", NOT "did any resolve to
    // material this device holds" — tui's shape, and the distinction is the
    // whole of `AftermathInputs::predecessor_keys`' "an empty list is
    // meaningful rather than a bug". An identity that never succeeded has no
    // rows and owes nothing; a successor's device that simply never held the
    // predecessor's secret has rows it cannot open, and the pass must still run
    // so leg 7 reports its drafts as owed by another device instead of
    // answering "not a successor" and doing nothing silently.
    let registry = accounts.registry();
    // Learn a link this device's registry does not hold BEFORE the gate reads
    // it — a device that never held the predecessor's row, or whose user
    // removed the retired account. The shared hop: best-effort, one profile
    // read for an ordinary identity. This is every FFI app's per-sign-in seam,
    // which is why it lives here rather than in four apps.
    fauna_client_recovery::ceremony::learn_succession_link(nest.nest_arc(), registry, &keypair)
        .await;
    if registry.predecessors_of(&actor_hex).is_empty() {
        return Ok(FfiAftermathOutcome::NotASuccessor);
    }

    // The registry's OWN walk, never a hand-rolled filter here
    // (`AftermathInputs::predecessor_keys`' ⚠). A row this device holds no
    // secret for is skipped, never refused, and an empty result is passed on
    // deliberately per the gate above.
    let resolved = registry.predecessor_backup_keys_by_actor(&actor_hex);

    // ⚠ Say WHICH filter dropped a predecessor row: without it, an empty
    // resolution is a dead end. The walk above drops a row for three reasons
    // that need three different fixes, and the shared accessor is deliberately
    // silent about all of them — a row is "skipped, never refused". Measured
    // 2026-08-25 on macOS: rows non-empty, `offered=0`, and nothing anywhere
    // said which of the three it was.
    //
    // Read-only and public-API-only (`secrets`, `hex32::decode`), so this
    // re-derives the same verdicts the walk reached rather than reaching into
    // it — and it runs once per aftermath, never on a hot path. It logs only
    // when a row was actually dropped, so a healthy pass stays quiet.
    let rows = registry.predecessors_of(&actor_hex);
    if resolved.len() < rows.len() {
        for hex in &rows {
            if resolved
                .iter()
                .any(|(actor_id, _)| actor_id.to_hex() == *hex)
            {
                continue;
            }
            let why = if registry.secrets(hex).is_none() {
                // The interesting one: the registry lists a predecessor this
                // device holds no secret for. Ordinarily it genuinely never
                // held it (another device owes the pass); anything else would
                // be a lost secret slot rather than an honest absence.
                "no secret slot for this row on this device"
            } else if fauna_core::hex32::decode(hex).is_err() {
                // `predecessor_backup_keys_by_actor`'s own extra skip: the pair
                // must be able to NAME the actor, and this id does not decode.
                "row id is not decodable as a 32-byte actor id"
            } else {
                "secret present but it does not parse as an actor keypair"
            };
            tracing::warn!(
                predecessor = %hex,
                why,
                "a predecessor row resolved to no key material"
            );
        }
    }

    // Leg 6 (the mail burn) runs in the post-store-ready pass
    // ([`spawn_ledger_pass`]) — its rows are the account store's — so this pass
    // builds no mail machine and takes no nest URL.

    let _ = app_data_dir;
    let inputs = AftermathInputs {
        owner_secret: *keypair.secret_bytes(),
        predecessor_keys: resolved.into_iter().map(|(_, key)| key).collect(),
    };

    let mut sink = ForeignAftermathSink { inner: sink };

    // ⚠ **SPAWNED, never awaited inline — the foreign executor's stack is too
    // small to run this pass on.** The rule and the whole mechanism are owned by
    // `docs/goal/architecture/apps/native-async-execution.md` (§ The second
    // measured incident; § The rule's *"a synchronous heavy leaf takes the other
    // mechanism"*) — read it there before changing this. In short: UniFFI's
    // `async_runtime = "tokio"` enters the tokio runtime *context* but leaves the
    // polling on whichever thread the foreign side drives it from. On Apple that
    // is Swift concurrency's cooperative pool, whose stacks are **544 KB** — and
    // two of this pass's legs run ML-KEM-768 through `fauna_pq_kem`, which needs
    // ~548 KB in one frame chain and so lands 3–4 KB *inside the guard page*:
    // `SIGBUS`, process dead, no Rust panic to catch and nothing on the sink.
    //
    // ⚠ **`Box::pin` — that doc's usual remedy — cannot fix this one**, which is
    // why the spawn is here and not a boxed leaf: the bottom of the chain is
    // plain SYNCHRONOUS crypto, and moving a future to the heap does not change
    // how much stack a synchronous callee needs. There is no leaf future to box.
    //
    // Measured on `test_identity_succession_aftermath.py --app
    // macos` — two crash reports, one
    // per leg, and they are the whole of that run's two `still reads actor=''` failures:
    //   * leg 4 — `remint_capability_grants` → `mint_grant` →
    //     `seal_capability_xwing` → `xwing_seal` → `fauna_pq_kem::encapsulate`
    //   * leg 6 — `burn_mail_after_succession` → `start_rotation` →
    //     `provision_snapshot_for` → `build_mls_snapshot_plaintext` →
    //     `derive_recipient_xwing_keypair` → `fauna_pq_kem::derive_keypair`
    // Both need a predecessor who actually HELD the plane (a grant; a mailbox),
    // which is why the two journeys with nothing held to re-key pass —
    // the crash is not rare, it is gated on the user having anything to re-key.
    //
    // `tokio::spawn` puts every leg on a runtime worker (2 MB stacks — what
    // linux, tui and the sync agent have always run this exact code on), so
    // this is drift resolved toward the pattern the other apps already have,
    // not a new one. Same seam and same reasoning as
    // `crate::account_runtime::install`'s spawn. Awaiting the handle keeps the
    // call's contract identical; a foreign caller that drops it now leaves the
    // pass to finish rather than tearing it mid-leg, which is the safer half of
    // a change we did not need to make for the stack — the pass is resumable
    // and idempotent either way ("the corpus is its own progress record").
    let nest_arc = nest.nest_arc();
    tokio::spawn(async move { run_shared(nest_arc, inputs, &mut sink).await })
        .await
        .map_err(|e| FfiError::General {
            msg: format!("succession aftermath: the pass could not be driven to completion: {e}"),
        })?;
    Ok(FfiAftermathOutcome::Ran)
}

/// The post-store-ready pass's two app-supplied halves, late-populated on the
/// connection: the sink ([`FfiNestClient::set_aftermath_sink`]) and the account
/// registry, both stashed by [`run_succession_aftermath`] at post-auth. The
/// third half is the account-store handle, which the runtime's installed arm
/// provides.
#[derive(Clone)]
pub(crate) struct LedgerPassSeams {
    pub(crate) sink: Arc<std::sync::Mutex<Option<Arc<dyn FfiAftermathSink>>>>,
    pub(crate) registry: Arc<std::sync::Mutex<Option<Arc<FfiAccountRegistry>>>>,
}

/// Run the post-store-ready half of the aftermath
/// ([`fauna_client_recovery::ledger_aftermath::run_ledger_aftermath`]) once
/// all three halves exist — the handle, the registry, the sink. Called from
/// BOTH edges (the runtime's installed arm and the post-auth
/// [`run_succession_aftermath`] call), because the two land in either order:
/// whichever comes second finds the other's half and runs it; if both see
/// all three, the pass runs twice, which is harmless — a second run puts
/// nothing (`ledger_aftermath`'s idempotence). A no-op while any half is
/// missing.
///
/// It drains a ceremony the succession fold parked in the registry (the
/// member-item and filter-mark raises, and the backup destinations' marks),
/// runs the `NestBackupKey` re-grant over the bound box's destination list
/// (leg 2), then fires the sink's
/// `config_stage_settled` so the app re-reads both review surfaces.
pub(crate) fn spawn_ledger_pass(nest: Arc<fauna_client::NestClient>, seams: &LedgerPassSeams) {
    let (Some(handle), Some(registry), Some(sink)) = (
        crate::account_runtime::handle(),
        seams.registry.lock().ok().and_then(|held| held.clone()),
        seams.sink.lock().ok().and_then(|held| held.clone()),
    ) else {
        return;
    };
    // Spawned for the reason `run_succession_aftermath`'s own pass is: a
    // runtime worker's stack, never the foreign executor's.
    tokio::spawn(async move {
        let mut sink = ForeignAftermathSink { inner: sink };
        // The same handle is the period-key custody legs 4 and 8 read.
        let period_keys: fauna_client_subscriptions::SharedPeriodKeyStore =
            Arc::new(handle.clone());
        // Leg 6 (the mail burn) drives a mail-settings machine over the same
        // custody — it runs the shared rotate-mail-keys flow, which lives on it.
        let mail_burn =
            fauna_client_mail_settings::rpc_glue::build_mail_settings_machine_for_session(
                Arc::clone(&nest),
                Arc::new(handle.clone()),
                Arc::new(handle.clone()),
            )
            .map(Arc::new);
        // The box this connection is bound to keys the backup list the
        // `NestBackupKey` re-grant (leg 2) reads. Unresolvable ⇒ `None`, which
        // skips that leg for this pass — never a guessed row.
        let bound_nest = match fauna_client_pair::resolve_this_nest_id(&nest).await {
            Ok(id) => <[u8; 32]>::try_from(id.as_slice()).ok(),
            Err(e) => {
                tracing::warn!(error = %e, "aftermath pass: bound nest id unresolved; backup re-grant skipped");
                None
            }
        };
        // The succession cut's custody arm runs over the same account's
        // folder-key custody (`writer-signed-change-records.md` ruling (11)(a)).
        let custody_cut = fauna_client_folders::SetCustodyCut::new(
            fauna_client_folders::FoldersClient::new(Arc::clone(&nest)),
            crate::account_runtime::folder_key_store(),
        );
        let parked = fauna_client_recovery::ledger_aftermath::run_ledger_aftermath(
            nest,
            &handle,
            &handle,
            bound_nest,
            period_keys,
            &handle,
            mail_burn,
            &custody_cut,
            registry.registry(),
            &mut sink,
        )
        .await;
        tracing::debug!(?parked, "the post-store-ready aftermath pass settled");
    });
}

/// This crate's [`AftermathSink`]: every leg's already-localized status line
/// becomes one `(leg, line)` call across the UniFFI boundary.
///
/// **The projection is called here, not app-side**, for the reason every one of
/// its five siblings gives: `status_line()` is the shared answer to "what does
/// this outcome say", and four apps each matching the progress enum themselves
/// is four chances to say something different about one event.
struct ForeignAftermathSink {
    inner: Arc<dyn FfiAftermathSink>,
}

impl AftermathSink for ForeignAftermathSink {
    fn backup_regrant(&mut self, progress: BackupRegrantProgress) {
        self.inner
            .progress(FfiAftermathLeg::BackupRegrant, progress.status_line());
    }
    fn grant_remint(&mut self, progress: fauna_client_capabilities::GrantRemintProgress) {
        self.inner
            .progress(FfiAftermathLeg::GrantRemint, progress.status_line());
    }
    fn drafts_reseal(&mut self, progress: fauna_client_drafts::DraftsResealProgress) {
        self.inner
            .progress(FfiAftermathLeg::DraftsReseal, progress.status_line());
    }
    fn mail_burn(&mut self, progress: fauna_client_mail_settings::MailBurnProgress) {
        self.inner
            .progress(FfiAftermathLeg::MailBurn, progress.status_line());
    }
    async fn config_stage_settled(&mut self) {
        self.inner.config_stage_settled();
    }
}
