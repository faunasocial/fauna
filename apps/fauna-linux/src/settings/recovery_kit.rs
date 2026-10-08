//! Recovery kit — the Settings/Account section that is the RecoveryKey's
//! Settings home (`docs/goal/ui/settings.md` § Recovery kit), placed
//! immediately after Identity export as its sibling root-secret affordance.
//! tui shipped this section first (`apps/fauna-tui/src/settings/recovery.rs`);
//! this module ports its 5-element core family — `recovery-kit-section` +
//! `-status` + `-create-button` + `-replace-button` + `-lost-button`
//! (`ui.yaml:846-850`, shared `elements`, not optional) — plus the
//! optional_elements two of those three ceremonies need to actually work: the
//! kit-in-hand phrase entry (`recovery-entry-phrase-field`) and the shown-once
//! minted-kit display trio (`recovery-kit-secret-display` / `-secret-copy-btn`
//! / `-qr`).
//!
//! **The stolen-identity ceremony and the group-sweep retry landed
//! 2026-09-01** —
//! `identity-stolen-button` behind its `identity-stolen-confirm-field`
//! type-to-confirm gate, the sweep's own two ID-less lines, and
//! `recovery-kit-sweep-retry-button`. All three consume `fauna_client_recovery`
//! **in-process**: linux is Rust-native like tui, so the
//! `succession_retry_group_sweep` UniFFI face apple and windows call is not a
//! layer this app goes through. Still NOT ported here: the escrow-reseal
//! button, the pending-replacement veto, and the ephemeral member-review pass
//! (the permanent review surface is `settings::member_review`).
//!
//! **The eight aftermath-progress lines are ported here — render-only, mirroring tui's `apps/fauna-tui/src/settings/recovery.rs`
//! shape.** Each reads a shared projection's `status_line()` — a pure function,
//! no network — so this module never re-derives copy from a raw outcome. The
//! **drive** that fills six of them is `crate::succession_aftermath`, which
//! runs the shared `run_succession_aftermath` pass at every post-auth sign-in;
//! each leg's report is folded into [`SESSION_AFTERMATH`], which this section
//! seeds from and repaints on. **The `__mls` line is
//! NOT that pass's** — it reports from `MlsStateSync::with_reseal_sink`, a
//! barrier inside the replica's own launch `load()`
//! (`conversations::conv_backend::start_conversations_session`), beside the pass rather than behind it. The file-corpus line is
//! still nothing linux writes, so it starts and stays hidden.
//!
//! **Everything with a decision in it lives in shared Rust**
//! (`fauna_client_recovery`, called in-process — linux is Rust-native like
//! tui, not an FFI/UniFFI consumer of this crate): the four states, which
//! actions each enables, and the status copy. This module is the GTK shell
//! over that, exactly as `identity_export` is for the sibling section above
//! it — nothing here re-derives a chain arm or a button's enablement.
//!
//! **The section holds no secret longer than the screen shows it.** A minted
//! kit is displayed once and dropped on the next repaint or page rebuild;
//! there is deliberately no "show it again" path, and there can never be one
//! (`identity-succession.md` § The RecoveryKey — *Custody*).

use adw::prelude::*;
use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;

use crate::i18n::strings::common;
use crate::i18n::strings::settings::recovery_kit as rk;
use fauna_client_recovery::RecoveryKitStatus;
use fauna_client_recovery::ceremony::{STOLEN_CONFIRM_WORD, SweepRetryAffordance, SweepStatus};
use fauna_core::qr_matrix::QrMatrix;

/// The landed succession's group sweep, parked so it outlives the account
/// switch the ceremony ends with.
///
/// ⚠ **Process-global on purpose, and it is the ONE piece of ceremony state
/// that has to be.** The ceremony runs signed in as the *retired* identity and
/// the lines render as the *successor*, on a section
/// `main.rs::register_switch_account_handler` has torn down and rebuilt
/// (it `destroy()`s every window, drops every `FaunaClient` clone and calls
/// `actor_scope::reset_actor_scoped_state`). So this cannot live in
/// [`RecoveryState`], in the client, or in anything that reset clears — it is
/// linux's twin of tui's `App::succession_sweep` and apple's
/// `SuccessionHandoff` (`settings.md` § Recovery kit → *The sweep's own
/// lines*: "an app carries the view across the account switch — the ceremony
/// runs before it, the lines render after").
///
/// The **enum** is parked rather than its [`SweepView`], unlike web's
/// `sessionStorage` and apple's FFI view: linux is Rust-native and calls the
/// projection in-process, so it can hold the richer thing and still select at
/// paint time — and the e2e state provider's `data.succession_sweep` wants the
/// per-group report only the enum carries
/// (`SweepStatus::state_json_or_null`). Nothing here is persisted: it
/// describes one ceremony on one app run, and a fresh process has none.
///
/// [`SweepView`]: fauna_client_recovery::ceremony::SweepView
static LANDED_SWEEP: Mutex<Option<SweepStatus>> = Mutex::new(None);

/// The closing act the ceremony still owes the successor: a **fresh
/// RecoveryKey, minted and shown** on the successor's first authenticated
/// session (`identity-succession.md` § The RecoveryKey → *At succession*).
/// `Some(actor_hex)` names the successor that owes it; `None` is every ordinary
/// session.
///
/// ⚠ **Process-global for exactly [`LANDED_SWEEP`]'s reason, and one harder
/// one.** The sweep merely *describes* the departing identity; this kit can only
/// be **performed** by the arriving one — the mint authenticates as the
/// successor, and that identity has no session until the switch completes. So
/// the switch is not incidental to the obligation, it is the boundary the
/// obligation exists to cross, and nothing `actor_scope::reset_actor_scoped_state`
/// clears could carry it. linux's twin of tui's `App::succession_kit_owed` and
/// apple's `SuccessionHandoff.kitOwed`.
///
/// **The actor id is the flag** — one cell rather than a bool beside an id,
/// because every read of the flag wants the seat check anyway. Holding the
/// SUCCESSOR's id (never the predecessor's) is what makes
/// [`claim_succession_kit`] refuse a session that is not the one that can pay:
/// between the fold setting this and the switch landing there is a 100 ms
/// window (`main.rs`'s deferred teardown) in which the *retired* session could
/// still reconnect and reach its own post-auth hook, and a discharge there would
/// mint under the identity the ceremony just retired. Nothing is persisted: it
/// describes one ceremony on one app run, and a fresh process owes nothing.
///
/// The predecessor id has no cell here, unlike tui's `succession_predecessor`:
/// linux resolves the escrow blob's predecessor section from the account
/// registry's own succession link (`client.rs`'s `escrow_predecessor_seeds`,
/// written by `record_succession` inside the ceremony), so the chain is already
/// nameable from the successor id alone.
static SUCCESSION_KIT_OWED: Mutex<Option<String>> = Mutex::new(None);

/// Record the kit a just-landed succession owes, immediately before the account
/// switch adopts `successor_actor_hex`.
pub(crate) fn owe_succession_kit(successor_actor_hex: String) {
    *SUCCESSION_KIT_OWED
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(successor_actor_hex);
}

/// Claim the owed kit for the session that is actually the successor — once.
///
/// Two guards in one `if`, each preventing its own failure. The **seat bind**
/// keeps a session that is not the successor from taking an obligation it
/// cannot perform (see [`SUCCESSION_KIT_OWED`]). The **single claim** keeps two
/// post-auth passes from minting two kits, the second of which would register a
/// kit nobody was shown — strictly worse than never-created
/// (`identity-succession.md` § The RecoveryKey → *At succession*).
pub(crate) fn claim_succession_kit(actor_hex: &str) -> bool {
    let mut owed = SUCCESSION_KIT_OWED
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if owed.as_deref() != Some(actor_hex) {
        return false;
    }
    // Moved, not dropped: [`settle_kit_discharge`] needs to know whose
    // obligation this outcome is settling, and re-arms it if the kit never
    // reached the screen.
    *KIT_DISCHARGE_IN_FLIGHT
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = owed.take();
    true
}

/// The successor whose owed kit is being minted **right now** — held only
/// between [`claim_succession_kit`] and the `RecoveryKitMinted` fold that
/// settles it.
///
/// The claim has to empty [`SUCCESSION_KIT_OWED`] (a second post-auth pass must
/// not mint a second kit), but the obligation is not discharged until the secret
/// is on the screen — so the id parks here for the length of one mint rather
/// than being dropped on the floor at the claim.
static KIT_DISCHARGE_IN_FLIGHT: Mutex<Option<String>> = Mutex::new(None);

/// Settle the in-flight discharge from the `RecoveryKitMinted` fold: `shown`
/// discharges it, anything else re-arms it ([`rearm_succession_kit`]).
///
/// A no-op for every ordinary create/replace/seed-alone press, which is every
/// mint with nothing in flight — and nothing else can be in flight *during* a
/// discharge, which is spawned at post-auth before any button exists to press.
pub(crate) fn settle_kit_discharge(shown: bool) {
    let successor = KIT_DISCHARGE_IN_FLIGHT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    let Some(successor) = successor else {
        return;
    };
    if shown {
        return;
    }
    tracing::warn!(
        "[succession] the owed kit for {successor} was not shown — re-arming for the \
         next authenticated session"
    );
    rearm_succession_kit(successor);
}

/// Put a claimed-but-never-SHOWN obligation back, so the next authenticated
/// session mints again.
///
/// ⚠ **"Minted" and "shown" are different events, and only the second one
/// discharges anything** (apple's `rearmUnshownKit`, iOS 2026-08-26). Two ways
/// linux reaches this: the mint FAILED — and it races its own reconnect on every
/// platform, because the ceremony revokes every session of the account inside
/// the nest's own transaction — or it landed with no section registered to paint
/// it, which puts a kit on the nest and in nobody's hands.
///
/// Re-arming is safe precisely because the mint is chain-derived: `create_kit`
/// re-reads the head and picks its own arm, so a second mint supersedes the
/// stranded one rather than colliding with it. The cost of a spurious re-arm is
/// one extra kit; the cost of a missed one is an account whose only route back
/// is the 30-day seed-alone window. That asymmetry is the whole argument for
/// erring this way.
pub(crate) fn rearm_succession_kit(successor_actor_hex: String) {
    owe_succession_kit(successor_actor_hex);
}

/// The group sweep a **relaunch adoption** still owes the successor
/// (`succession-propagation.md` § Propagation → *Own device fleet*, the
/// relaunch-adoption clause). `Some(actor_hex)` names the successor that owes
/// it; `None` is every ordinary session, and every ceremony that ran its own
/// sweep.
///
/// The ceremony sweeps the retired leaf out of every group itself; an adoption
/// at relaunch ([`crate::settings::adopt_held_successor`]) adopts a successor
/// whose ceremony reply was lost, so nothing swept. The successor's first
/// authenticated session discharges it as an unbidden press of
/// `recovery-kit-sweep-retry-button` (`FaunaClient::discharge_owed_sweep`),
/// ahead of the kit. Process-global and seat-bound for exactly
/// [`SUCCESSION_KIT_OWED`]'s reasons — the obligation exists to cross the
/// account switch, and only the successor can author the sweep. Twin of
/// windows' `SuccessionHandoff.SweepOwedTo` and apple's `sweepOwedTo`.
static SUCCESSION_SWEEP_OWED: Mutex<Option<String>> = Mutex::new(None);

/// Record the sweep a relaunch adoption owes, immediately before the account
/// switch adopts `successor_actor_hex`.
pub(crate) fn owe_succession_sweep(successor_actor_hex: String) {
    *SUCCESSION_SWEEP_OWED
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(successor_actor_hex);
}

/// Claim the owed sweep for the session that really is the successor — once.
/// The seat bind and the single claim guard what [`claim_succession_kit`]'s do,
/// for the same reasons.
pub(crate) fn claim_succession_sweep(actor_hex: &str) -> bool {
    let mut owed = SUCCESSION_SWEEP_OWED
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if owed.as_deref() != Some(actor_hex) {
        return false;
    }
    *owed = None;
    true
}

/// Forget the owed kit and the owed sweep — sign-out and factory reset only,
/// beside [`clear_succession_sweep`].
///
/// A plain account *switch* must never call this: crossing exactly one switch is
/// the whole point. A reset destroys every identity on the box, so there is no
/// successor left to owe a kit to — and no predecessor row left in the registry
/// for that kit to have sealed.
pub(crate) fn clear_succession_kit_debt() {
    *SUCCESSION_KIT_OWED
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *KIT_DISCHARGE_IN_FLIGHT
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *SUCCESSION_SWEEP_OWED
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
}

/// Park the sweep a just-landed succession produced, immediately before the
/// account switch adopts the successor.
pub(crate) fn park_succession_sweep(sweep: SweepStatus) {
    *LANDED_SWEEP.lock().unwrap_or_else(|e| e.into_inner()) = Some(sweep);
}

/// Forget the parked sweep — sign-out and factory reset only.
///
/// A plain account *switch* must never call this: carrying the sweep across
/// exactly one switch is the whole point of parking it.
pub(crate) fn clear_succession_sweep() {
    *LANDED_SWEEP.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The parked sweep as the e2e state provider's `data.succession_sweep` — the
/// machine vocabulary, straight off the shared projection (tui's
/// `sweep_state_json` twin, and the same function underneath).
pub(crate) fn succession_sweep_state_json() -> serde_json::Value {
    let parked = LANDED_SWEEP.lock().unwrap_or_else(|e| e.into_inner());
    SweepStatus::state_json_or_null(parked.as_ref())
}

/// The post-succession aftermath's per-leg progress — mirrors tui's
/// `crate::app::AftermathProgress` field-for-field.
#[derive(Debug, Clone, Default)]
struct AftermathProgress {
    backup_regrant: Option<fauna_client_config::BackupRegrantProgress>,
    mls_reseal: Option<fauna_client_mls_sync::ReplicaResealProgress>,
    grant_remint: Option<fauna_client_capabilities::GrantRemintProgress>,
    corpus_reseal: Option<fauna_client_sync::agent::CorpusResealProgress>,
    mail_burn: Option<fauna_client_mail_settings::MailBurnProgress>,
    drafts_reseal: Option<fauna_client_drafts::DraftsResealProgress>,
}

/// One report from the aftermath's background task, on its way to the GTK
/// thread (`DataMessage::AftermathProgress`). One variant per line the pass
/// writes, plus the inherited-filter count its `config_stage_settled` hook
/// re-reads — tui's separate `DataMessage` arms, as one enum so `app.rs` routes
/// a single message.
#[derive(Debug, Clone)]
pub enum AftermathUpdate {
    BackupRegrant(fauna_client_config::BackupRegrantProgress),
    GrantRemint(fauna_client_capabilities::GrantRemintProgress),
    DraftsReseal(fauna_client_drafts::DraftsResealProgress),
    MailBurn(fauna_client_mail_settings::MailBurnProgress),
    /// The `__mls` state-replica re-seal, reported from
    /// `MlsStateSync::with_reseal_sink` (`conversations::conv_backend::start_conversations_session`)
    /// rather than from `run_succession_aftermath` — it runs as a barrier inside
    /// the replica's own launch `load()`, beside the pass rather than behind it.
    MlsReseal(fauna_client_mls_sync::ReplicaResealProgress),
    /// How many inherited email filters are still marked for review.
    InheritedFilters(usize),
}

/// What this session's aftermath has reported so far.
///
/// **Held here rather than only in a section's [`RecoveryState`]**: the pass
/// starts at post-auth, and the section is a page the shell can rebuild at any
/// time, so a report folded into one section's closure would vanish with it and
/// a successor re-opening Settings would read a blank surface for work that
/// finished. Every section seeds from this and repaints from it on each report.
/// **Actor-scoped**, unlike [`LANDED_SWEEP`]: each identity's sign-in runs its
/// own pass, so `actor_scope::reset_actor_scoped_state` clears it.
static SESSION_AFTERMATH: Mutex<Option<SessionAftermath>> = Mutex::new(None);

#[derive(Debug, Clone, Default)]
struct SessionAftermath {
    progress: AftermathProgress,
    inherited_filters: usize,
}

/// Fold one report into [`SESSION_AFTERMATH`]. Stored verbatim — the surface
/// renders what the pass says and never re-derives a verdict from it, so an
/// owed-by-another-device arm cannot be flattened into a success here (tui's
/// `App` arms, same reason).
pub(crate) fn fold_aftermath(update: AftermathUpdate) {
    let mut cell = SESSION_AFTERMATH.lock().unwrap_or_else(|e| e.into_inner());
    let session = cell.get_or_insert_with(SessionAftermath::default);
    match update {
        AftermathUpdate::BackupRegrant(p) => session.progress.backup_regrant = Some(p),
        AftermathUpdate::GrantRemint(p) => session.progress.grant_remint = Some(p),
        AftermathUpdate::DraftsReseal(p) => session.progress.drafts_reseal = Some(p),
        AftermathUpdate::MailBurn(p) => session.progress.mail_burn = Some(p),
        AftermathUpdate::MlsReseal(p) => session.progress.mls_reseal = Some(p),
        AftermathUpdate::InheritedFilters(open) => session.inherited_filters = open,
    }
}

fn session_aftermath() -> SessionAftermath {
    SESSION_AFTERMATH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

/// Forget this session's reports — every actor change.
pub(crate) fn clear_session_aftermath() {
    *SESSION_AFTERMATH.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Side of the drawn QR, in px — same box `identity_export` uses.
const QR_SIZE_PX: i32 = 220;

/// A kit a ceremony just minted, held only while the screen shows it. The
/// 64-hex secret itself is not kept here — it is written straight to
/// `recovery-kit-secret-display` at mint time and never read back.
struct MintedKit {
    /// What `recovery-kit-secret-copy-btn` puts on the clipboard — the
    /// `fauna://recovery` URI, not the bare hex: it carries the actor id and
    /// the host-qualified handle, which is what lets a later restore find the
    /// home nest with nothing typed (`settings.md` § Recovery kit, ruling
    /// 2026-08-02).
    uri: String,
    matrix: Option<QrMatrix>,
}

/// The section's live widgets, held so the repaint hooks below can update
/// them in place without rebuilding the page.
struct Widgets {
    status_label: gtk::Label,
    create_btn: gtk::Button,
    replace_btn: gtk::Button,
    lost_btn: gtk::Button,
    veto_btn: gtk::Button,
    escrow_reseal_btn: gtk::Button,
    sweep_retry_btn: gtk::Button,
    stolen_confirm_row: adw::EntryRow,
    stolen_btn: gtk::Button,
    sweep_outcome_label: gtk::Label,
    sweep_unattested_label: gtk::Label,
    phrase_row: adw::EntryRow,
    minted_box: gtk::Box,
    secret_label: gtk::Label,
    qr_area: gtk::DrawingArea,
    error_label: gtk::Label,
    backup_regrant_status_label: gtk::Label,
    mls_reseal_status_label: gtk::Label,
    grant_remint_status_label: gtk::Label,
    corpus_reseal_status_label: gtk::Label,
    drafts_reseal_status_label: gtk::Label,
    mail_burn_status_label: gtk::Label,
    inherited_filters_status_label: gtk::Label,
}

/// The section's view state — mirrors tui's `RecoveryState`, narrowed to the
/// fields the section's ceremonies need, plus the aftermath-progress
/// bundle and the inherited-filters open count.
#[derive(Default)]
struct RecoveryState {
    status: Option<RecoveryKitStatus>,
    busy: bool,
    aftermath: AftermathProgress,
    inherited_filters: usize,
}

/// Build the "Recovery Kit" preferences group.
///
/// `error_label` is the page-level `error-message` element `build_account_page`
/// already owns (E2E Rule 2 — one per page, never a second one here). Returns
/// the group plus a refresh closure the caller should invoke whenever the page
/// becomes visible, exactly like the Stage-2 toggle refresh above it: the
/// status is read fresh from the registration chain, never a local flag, so a
/// kit created on another device is reflected here (`settings.md` § Recovery
/// kit).
pub fn build_recovery_kit_group(error_label: &gtk::Label) -> (adw::PreferencesGroup, Rc<dyn Fn()>) {
    let group = adw::PreferencesGroup::builder().title(rk::TITLE).build();
    crate::testid::set_test_id(&group, ids::RECOVERY_KIT_SECTION);

    // The last succession's group sweep, if one ran on this app run
    // (`settings.md` § Recovery kit → *The sweep's own lines*). At the TOP of
    // the section because it is the freshest thing here and describes what the
    // buttons below just did, and as **ID-less chrome** because it is prose the
    // user reads rather than an affordance any test drives.
    //
    // ⚠ The two lines are deliberately SEPARATE and neither qualifies the
    // other: eviction of the stolen identity and the roster the sweep cannot
    // vouch for are two facts, and there is no combined "you are safe" verdict
    // to render — the same refusal `SweepReport` itself makes.
    let new_sweep_line = || {
        let label = gtk::Label::builder().wrap(true).build();
        label.set_halign(gtk::Align::Start);
        label.set_visible(false);
        group.add(&label);
        label
    };
    let sweep_outcome_label = new_sweep_line();
    let sweep_unattested_label = new_sweep_line();

    // The eight aftermath-progress lines, in the order the legs
    // run (`settings.md` § Recovery kit — leg 7 renders ABOVE leg 6). Every
    // one starts hidden with no text; `repaint` shows it only once its
    // shared projection has something to say. They sit ABOVE the kit status
    // line, exactly as tui's `recovery_elements` orders them.
    let new_status_label = |id: &str| {
        let label = gtk::Label::builder().wrap(true).build();
        label.set_halign(gtk::Align::Start);
        label.add_css_class("dim-label");
        label.set_visible(false);
        crate::testid::set_test_id(&label, id);
        group.add(&label);
        label
    };
    let backup_regrant_status_label = new_status_label(ids::RECOVERY_KIT_BACKUP_REGRANT_STATUS);
    let mls_reseal_status_label = new_status_label(ids::RECOVERY_KIT_MLS_RESEAL_STATUS);
    let grant_remint_status_label = new_status_label(ids::RECOVERY_KIT_GRANT_REMINT_STATUS);
    let corpus_reseal_status_label = new_status_label(ids::RECOVERY_KIT_CORPUS_RESEAL_STATUS);
    // Leg 7 before leg 6, same reason the task runs them in that order: the
    // burn is the only leg that takes something away, so every restoring leg
    // reports above it.
    let drafts_reseal_status_label = new_status_label(ids::RECOVERY_KIT_DRAFTS_RESEAL_STATUS);
    let mail_burn_status_label = new_status_label(ids::RECOVERY_KIT_MAIL_BURN_STATUS);
    let inherited_filters_status_label =
        new_status_label(ids::RECOVERY_KIT_INHERITED_FILTERS_STATUS);

    let status_label = gtk::Label::builder()
        .label(rk::STATUS_LOADING)
        .wrap(true)
        .build();
    status_label.set_halign(gtk::Align::Start);
    status_label.add_css_class("dim-label");
    crate::testid::set_test_id(&status_label, ids::RECOVERY_KIT_STATUS);
    group.add(&status_label);

    // The kit-in-hand entry (`settings.md` § Recovery kit → *Kit-in-hand
    // entry*, user-approved 2026-08-01). Four ceremonies read it — replace,
    // veto, the escrow re-seal and stolen — and `allows_stolen` is true in
    // every resolved state, so it starts UP rather than hidden: an unread
    // status must not take the field away from the ceremony that needs it
    // most, and until the first status lands `repaint` has not run to put it
    // there.
    let phrase_row = adw::EntryRow::builder()
        .title(rk::KIT_PHRASE_PLACEHOLDER)
        .build();
    crate::testid::set_test_id(&phrase_row, ids::RECOVERY_ENTRY_PHRASE_FIELD);
    group.add(&phrase_row);

    let create_btn = gtk::Button::builder()
        .label(rk::CREATE)
        .halign(gtk::Align::Start)
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    crate::testid::set_test_id(&create_btn, ids::RECOVERY_KIT_CREATE_BUTTON);
    crate::offline_gate::declare_wire_kind(&create_btn, "fauna.recovery.registration.submit");
    group.add(&create_btn);

    let replace_btn = gtk::Button::builder()
        .label(rk::REPLACE)
        .halign(gtk::Align::Start)
        .sensitive(false)
        .build();
    crate::testid::set_test_id(&replace_btn, ids::RECOVERY_KIT_REPLACE_BUTTON);
    crate::offline_gate::declare_wire_kind(&replace_btn, "fauna.recovery.registration.submit");
    group.add(&replace_btn);

    let lost_btn = gtk::Button::builder()
        .label(rk::LOST)
        .halign(gtk::Align::Start)
        .build();
    lost_btn.set_sensitive(false);
    crate::testid::set_test_id(&lost_btn, ids::RECOVERY_KIT_LOST_BUTTON);
    crate::offline_gate::declare_wire_kind(&lost_btn, "fauna.recovery.replacement.request");
    group.add(&lost_btn);

    // The pending window's contest (`settings.md` § Recovery kit): renders only
    // while a seed-alone replacement pends, which is also the only state whose
    // status line names it — tui's `recovery_elements` gate, `allows_veto`
    // being the pending arm itself.
    let veto_btn = gtk::Button::builder()
        .label(rk::VETO)
        .halign(gtk::Align::Start)
        .css_classes(["destructive-action"])
        .visible(false)
        .build();
    crate::testid::set_test_id(&veto_btn, ids::RECOVERY_PENDING_VETO_BUTTON);
    crate::offline_gate::declare_wire_kind(&veto_btn, "fauna.recovery.replacement.veto");
    group.add(&veto_btn);

    // The no-escrow repair (ID user-approved 2026-08-16): renders in
    // `RegisteredNoEscrow` only — an affordance for a state, never a standing
    // button — and deliberately NOT replace, which would retire the kit the
    // user holds (the status line's own wording names this button).
    let escrow_reseal_btn = gtk::Button::builder()
        .label(rk::ESCROW_RESEAL)
        .halign(gtk::Align::Start)
        .css_classes(["suggested-action"])
        .visible(false)
        .build();
    crate::testid::set_test_id(&escrow_reseal_btn, ids::RECOVERY_KIT_ESCROW_RESEAL_BUTTON);
    crate::offline_gate::declare_wire_kind(&escrow_reseal_btn, "fauna.recovery.escrow.put");
    group.add(&escrow_reseal_btn);

    // The group-sweep retry, directly under the sweep chrome whose two degraded
    // arms name it by label (`settings.md` § Recovery kit → *Finishing an
    // unfinished group sweep*).
    //
    // ⚠ **The render gate is UNFINISHED WORK, never "this device can retry."**
    // The retry needs the retired identity's own MLS store, which survives only
    // on the device the ceremony ran on — and that is deliberately NOT consulted
    // here. A device without it answers with the member-side remedy when pressed;
    // hiding the button there would leave the sweep's own degraded copy naming a
    // control that is not on screen, which is exactly the dishonesty that copy's
    // narrowing removed. The gate is the shared `owes_work` — the same projection
    // that selects the copy naming this button, so the two cannot disagree.
    let sweep_retry_btn = gtk::Button::builder()
        .label(rk::SWEEP_RETRY)
        .halign(gtk::Align::Start)
        .visible(false)
        .build();
    crate::testid::set_test_id(&sweep_retry_btn, ids::RECOVERY_KIT_SWEEP_RETRY_BUTTON);
    crate::offline_gate::declare_wire_kind(&sweep_retry_btn, "fauna.conversations.channel.send");
    group.add(&sweep_retry_btn);

    // Succession is irreversible and re-points the whole account, so it is the
    // one action gated behind a type-to-confirm field as well as its status
    // (`settings.md` § Recovery kit — "the same type-to-confirm idiom as
    // `settings-delete-account-button`"). The warning rides as ID-less chrome
    // for the sweep lines' reason: it is copy, not an affordance, and inventing
    // an ID for prose would put a second name on the page for one control.
    let stolen_warning = gtk::Label::builder()
        .label(rk::STOLEN_WARNING)
        .wrap(true)
        .build();
    stolen_warning.set_halign(gtk::Align::Start);
    stolen_warning.add_css_class("dim-label");
    stolen_warning.set_margin_top(8);
    group.add(&stolen_warning);

    let stolen_confirm_row = adw::EntryRow::builder()
        .title(rk::STOLEN_CONFIRM_PLACEHOLDER)
        .build();
    crate::testid::set_test_id(&stolen_confirm_row, ids::IDENTITY_STOLEN_CONFIRM_FIELD);
    group.add(&stolen_confirm_row);

    let stolen_btn = gtk::Button::builder()
        .label(rk::STOLEN)
        .halign(gtk::Align::Start)
        .css_classes(["destructive-action"])
        .sensitive(false)
        .build();
    crate::testid::set_test_id(&stolen_btn, ids::IDENTITY_STOLEN_BUTTON);
    crate::offline_gate::declare_wire_kind(&stolen_btn, "fauna.recovery.succession.submit");
    group.add(&stolen_btn);

    // The shown-once minted-kit display trio, through the ONBOARDING screen's
    // own IDs rather than settings-specific twins (`settings.md` § Recovery
    // kit → *Displaying a returned kit* — "it is the same artifact",
    // priority #3). Hidden until a ceremony mints something.
    let minted_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    minted_box.set_margin_top(8);
    minted_box.set_visible(false);

    let secret_label = gtk::Label::builder().wrap(true).selectable(true).build();
    secret_label.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&secret_label, ids::RECOVERY_KIT_SECRET_DISPLAY);
    minted_box.append(&secret_label);

    let minted: Rc<RefCell<Option<MintedKit>>> = Rc::new(RefCell::new(None));

    let copy_btn = gtk::Button::builder()
        .label(common::COPY)
        .halign(gtk::Align::Start)
        .build();
    crate::testid::set_test_id(&copy_btn, ids::RECOVERY_KIT_SECRET_COPY_BTN);
    copy_btn.connect_clicked({
        let minted = minted.clone();
        move |_| {
            if let Some(kit) = minted.borrow().as_ref() {
                crate::clipboard::copy_text(&kit.uri);
            }
        }
    });
    minted_box.append(&copy_btn);

    let qr_area = gtk::DrawingArea::builder()
        .content_width(QR_SIZE_PX)
        .content_height(QR_SIZE_PX)
        .build();
    qr_area.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&qr_area, ids::RECOVERY_KIT_QR);
    qr_area.set_draw_func({
        let minted = minted.clone();
        move |_area, cr, width, height| {
            let borrowed = minted.borrow();
            let Some(m) = borrowed.as_ref().and_then(|k| k.matrix.as_ref()) else {
                return;
            };
            crate::qr_widget::draw_matrix(cr, m, width, height);
        }
    });
    minted_box.append(&qr_area);

    group.add(&minted_box);

    let widgets = Rc::new(Widgets {
        status_label,
        create_btn: create_btn.clone(),
        replace_btn: replace_btn.clone(),
        lost_btn: lost_btn.clone(),
        veto_btn: veto_btn.clone(),
        escrow_reseal_btn: escrow_reseal_btn.clone(),
        sweep_retry_btn: sweep_retry_btn.clone(),
        stolen_confirm_row: stolen_confirm_row.clone(),
        stolen_btn: stolen_btn.clone(),
        sweep_outcome_label,
        sweep_unattested_label,
        phrase_row: phrase_row.clone(),
        minted_box,
        secret_label,
        qr_area,
        error_label: error_label.clone(),
        backup_regrant_status_label,
        mls_reseal_status_label,
        grant_remint_status_label,
        corpus_reseal_status_label,
        drafts_reseal_status_label,
        mail_burn_status_label,
        inherited_filters_status_label,
    });
    // Seeded from the session's aftermath so a section built after a leg
    // reported — a page rebuild, or the switch's window rebuild racing the
    // pass — still carries it.
    let seeded = session_aftermath();
    let state: Rc<RefCell<RecoveryState>> = Rc::new(RefCell::new(RecoveryState {
        aftermath: seeded.progress,
        inherited_filters: seeded.inherited_filters,
        ..Default::default()
    }));

    // ── Button gestures. Each clears the page error, marks the section busy
    //    (so a double click cannot race a second registration onto one chain
    //    head — mirrors tui), and hands off to the FaunaClient ceremony.
    //    Every write to `widgets.error_label` goes through
    //    `render_account_error_label`, never a bare `set_visible`/
    //    `render_error_label` — a pending stolen-ceremony persist-failure
    //    message on that shared label outranks any of these
    //    (`settings.md` § Recovery kit → *The persist-failure message
    //    survives the page*; ). ──
    create_btn.connect_clicked({
        let state = state.clone();
        let widgets = widgets.clone();
        move |_| {
            super::render_account_error_label(&widgets.error_label, None);
            state.borrow_mut().busy = true;
            repaint(&state.borrow(), &widgets);
            if let Some(client) = crate::settings::get_client() {
                client.create_recovery_kit();
            }
        }
    });

    lost_btn.connect_clicked({
        let state = state.clone();
        let widgets = widgets.clone();
        move |_| {
            super::render_account_error_label(&widgets.error_label, None);
            state.borrow_mut().busy = true;
            repaint(&state.borrow(), &widgets);
            if let Some(client) = crate::settings::get_client() {
                client.request_recovery_kit_lost();
            }
        }
    });

    replace_btn.connect_clicked({
        let state = state.clone();
        let widgets = widgets.clone();
        move |_| {
            let phrase = widgets.phrase_row.text().to_string();
            if phrase.trim().is_empty() {
                super::render_account_error_label(
                    &widgets.error_label,
                    Some(rk::KIT_PHRASE_REQUIRED),
                );
                return;
            }
            super::render_account_error_label(&widgets.error_label, None);
            state.borrow_mut().busy = true;
            repaint(&state.borrow(), &widgets);
            if let Some(client) = crate::settings::get_client() {
                client.replace_recovery_kit(&phrase);
            }
        }
    });

    // The two kit-in-hand repairs share replace's refusal: an empty field is
    // answered in words on `error-message`, never a silently dropped gesture.
    for (btn, run) in [
        (
            veto_btn.clone(),
            (|c: &crate::client::FaunaClient, p: &str| c.veto_recovery_replacement(p))
                as fn(&crate::client::FaunaClient, &str),
        ),
        (
            escrow_reseal_btn.clone(),
            |c: &crate::client::FaunaClient, p: &str| c.reseal_recovery_escrow(p),
        ),
    ] {
        let state = state.clone();
        let widgets = widgets.clone();
        btn.connect_clicked(move |_| {
            let phrase = widgets.phrase_row.text().to_string();
            if phrase.trim().is_empty() {
                super::render_account_error_label(
                    &widgets.error_label,
                    Some(rk::KIT_PHRASE_REQUIRED),
                );
                return;
            }
            super::render_account_error_label(&widgets.error_label, None);
            state.borrow_mut().busy = true;
            repaint(&state.borrow(), &widgets);
            if let Some(client) = crate::settings::get_client() {
                run(&client, &phrase);
            }
        });
    }

    // Re-arm the stolen gate on every keystroke. `repaint` is what actually
    // decides the sensitivity — the field is only one of its two conditions —
    // so this hands the whole state back to it rather than flipping the button
    // here (the delete gate's `connect_changed` predates the projection and
    // flips its own button; doing that here would let the busy flag and the
    // status arm disagree with the render).
    stolen_confirm_row.connect_changed({
        let state = state.clone();
        let widgets = widgets.clone();
        move |_| repaint(&state.borrow(), &widgets)
    });

    stolen_btn.connect_clicked({
        let state = state.clone();
        let widgets = widgets.clone();
        move |_| {
            // Re-checked HERE and not only in the render (`settings.md`
            // § Recovery kit: both conditions "are re-checked when the action
            // fires rather than only in the render"). A disabled button emits no
            // gesture, but a test agent driving the id reaches this handler, and
            // an irreversible ceremony must refuse out loud rather than run —
            // or, worse, drop the command silently (convention 11).
            if widgets.stolen_confirm_row.text() != STOLEN_CONFIRM_WORD {
                super::render_account_error_label(
                    &widgets.error_label,
                    Some(rk::STOLEN_CONFIRM_PLACEHOLDER),
                );
                return;
            }
            // The kit is the whole authorization, so the phrase is the second
            // refusal — and both are checked before anything is torn down.
            let phrase = widgets.phrase_row.text().to_string();
            if phrase.trim().is_empty() {
                super::render_account_error_label(
                    &widgets.error_label,
                    Some(rk::KIT_PHRASE_REQUIRED),
                );
                return;
            }
            super::render_account_error_label(&widgets.error_label, None);
            state.borrow_mut().busy = true;
            repaint(&state.borrow(), &widgets);
            if let Some(client) = crate::settings::get_client() {
                // From here a supersession is this device's own doing, and the
                // fold owns what happens next (`settings::stolen_hold`).
                crate::settings::begin_stolen_ceremony();
                client.succeed_identity(&phrase);
            }
        }
    });

    // The retry takes NO phrase: it re-acquires the landed statement from the
    // chain rather than from a kit, so demanding one would gate a repair on a
    // credential the ceremony already told the user to retire.
    sweep_retry_btn.connect_clicked({
        let state = state.clone();
        let widgets = widgets.clone();
        move |_| {
            super::render_account_error_label(&widgets.error_label, None);
            state.borrow_mut().busy = true;
            repaint(&state.borrow(), &widgets);
            if let Some(client) = crate::settings::get_client() {
                client.retry_group_sweep();
            }
        }
    });

    // ── Repaint hooks, invoked from `app.rs`'s `RecoveryStatusLoaded` /
    //    `RecoveryKitMinted` handlers via the thread-local registry in
    //    `settings::mod` (the same shape `inbox_mode`'s handler uses).
    //    Same rule as the button gestures above: every write to
    //    `widgets.error_label` goes through `render_account_error_label`,
    //    because any of these can land while an unrelated stolen-ceremony
    //    persist-failure message is still pending acknowledgment on that
    //    shared label — the case a stolen-ceremony persist-failure fix
    //    names (a mint landing on ANOTHER device,
    //    or a status poll, wiping the one surviving copy of a successor's
    //    key). The stolen-failed handler
    //    below is the sole exception: it is the write that SHOWS that
    //    message, so it must bypass the guard or it would block itself. ──
    crate::settings::set_recovery_status_handler(Rc::new({
        let state = state.clone();
        let widgets = widgets.clone();
        move |result| {
            match result {
                Ok(status) => {
                    state.borrow_mut().status = Some(status);
                }
                Err(msg) => {
                    super::render_account_error_label(
                        &widgets.error_label,
                        Some(&rk::status_failed(&msg)),
                    );
                }
            }
            repaint(&state.borrow(), &widgets);
        }
    }));

    crate::settings::set_recovery_kit_minted_handler(Rc::new({
        let state = state.clone();
        let widgets = widgets.clone();
        let minted = minted.clone();
        move |result| {
            state.borrow_mut().busy = false;
            match result {
                Ok((secret_hex, status)) => {
                    super::render_account_error_label(&widgets.error_label, None);
                    state.borrow_mut().status = Some(status);
                    let uri = recovery_kit_uri(&secret_hex);
                    let matrix = fauna_core::qr_matrix::qr_matrix(&uri).ok();
                    widgets.secret_label.set_text(&secret_hex);
                    minted.replace(Some(MintedKit { uri, matrix }));
                    widgets.minted_box.set_visible(true);
                    widgets.qr_area.queue_draw();
                }
                Err(msg) => {
                    super::render_account_error_label(
                        &widgets.error_label,
                        Some(&rk::action_failed(&msg)),
                    );
                }
            }
            repaint(&state.borrow(), &widgets);
        }
    }));

    crate::settings::set_recovery_repaired_handler(Rc::new({
        let state = state.clone();
        let widgets = widgets.clone();
        move |result| {
            state.borrow_mut().busy = false;
            match result {
                Ok(status) => {
                    super::render_account_error_label(&widgets.error_label, None);
                    state.borrow_mut().status = Some(status);
                    // The pasted kit has done its job, and it outranks the
                    // seed: it does not linger in the field (tui's fold). No
                    // success notice either — the re-read status line IS the
                    // receipt, and the button that was pressed stops rendering.
                    widgets.phrase_row.set_text("");
                }
                Err(msg) => {
                    super::render_account_error_label(&widgets.error_label, Some(&msg));
                }
            }
            repaint(&state.borrow(), &widgets);
        }
    }));

    crate::settings::set_recovery_stolen_failed_handler(Rc::new({
        let state = state.clone();
        let widgets = widgets.clone();
        move |message| {
            state.borrow_mut().busy = false;
            // Unguarded, deliberately: this IS the display of the pending
            // message, so it must not be blocked by its own guard.
            super::render_error_label(&widgets.error_label, Some(&message));
            repaint(&state.borrow(), &widgets);
        }
    }));

    crate::settings::set_aftermath_handler(Rc::new({
        let state = state.clone();
        let widgets = widgets.clone();
        move || {
            let session = session_aftermath();
            {
                let mut s = state.borrow_mut();
                s.aftermath = session.progress;
                s.inherited_filters = session.inherited_filters;
            }
            repaint(&state.borrow(), &widgets);
        }
    }));

    crate::settings::set_sweep_retried_handler(Rc::new({
        let state = state.clone();
        let widgets = widgets.clone();
        move |answer| {
            state.borrow_mut().busy = false;
            // The `Some` case is one of the three answers whose sentence IS the
            // whole gesture (`settings.md` § Recovery kit — the button "must
            // answer in words on every press"); `None` is the press that
            // actually swept, whose outcome renders through the sweep's own
            // lines instead (`repaint` re-reads the freshly parked report) —
            // deliberately NOT a sentence, which is why the shared
            // `SweepRetryAnswer::Swept` carries none.
            super::render_account_error_label(&widgets.error_label, answer.as_deref());
            repaint(&state.borrow(), &widgets);
        }
    }));

    // Initial read, so the section does not sit on "Checking…" until the user
    // navigates away and back.
    if let Some(client) = crate::settings::get_client() {
        client.fetch_recovery_status();
    }

    let refresh: Rc<dyn Fn()> = Rc::new({
        let minted = minted.clone();
        let minted_box = widgets.minted_box.clone();
        let secret_label = widgets.secret_label.clone();
        move || {
            // **The kit is shown ONCE** (`identity-succession.md` § The
            // RecoveryKey — *Custody*): entering the section drops any secret
            // still on screen from an earlier mint, so there is no "show it
            // again" path and never can be. The section is built once with the
            // window and only ever hidden/re-shown by the shell's stack, so
            // without this the display trio — secret, copy button, QR — would
            // survive every navigation for the life of the process.
            //
            // ⚠ **This is why the succession's closing act navigates BEFORE it
            // spawns the mint** (`app.rs`'s `AuthSuccess` arm). The nav edge
            // fires this synchronously; a discharge that minted first and
            // navigated after would wipe the very kit it exists to show.
            //
            // The `MintedKit` goes with it, not just the visibility: it holds
            // the `fauna://recovery` URI the copy button puts on the clipboard,
            // and a hidden-but-live copy target is the same secret still in
            // reach.
            minted.replace(None);
            secret_label.set_text("");
            minted_box.set_visible(false);
            if let Some(client) = crate::settings::get_client() {
                client.fetch_recovery_status();
            }
            // A stolen-identity persist-failure message may have landed while
            // this page was off-screen — paint it now rather than leaving it
            // parked (`settings::show_stolen_failed_message`'s doc comment).
            crate::settings::show_stolen_failed_message();
        }
    });

    (group, refresh)
}

/// Build the `fauna://recovery` display URI for a just-minted secret — the
/// account's own actor id and host-qualified handle, through the one builder
/// every app shares (`fauna_client_recovery::kit_display_uri`).
fn recovery_kit_uri(secret_hex: &str) -> String {
    let actor_id = crate::settings::get_client()
        .and_then(|c| c.actor_id())
        .unwrap_or_default();
    let handle = crate::settings::get_handle().unwrap_or_default();
    let node_url = crate::settings::get_client()
        .map(|c| c.node_url().to_string())
        .unwrap_or_default();
    fauna_client_recovery::kit_display_uri(secret_hex, &actor_id, &handle, &node_url)
}

/// Repaint the status line and every gesture's sensitivity from `state` — the
/// projection's answer, never a local `match` (priority #2): `allows_*` on
/// [`RecoveryKitStatus`] is the one source every app reads.
fn repaint(state: &RecoveryState, widgets: &Widgets) {
    let live = |allowed: bool| allowed && !state.busy;
    let status = state.status.as_ref();

    // The sweep's two lines and the retry that finishes it — all three off the
    // ONE parked outcome, so the copy naming the button and the button's own
    // presence are the same projection's answer and cannot disagree
    // (`settings.md` § Recovery kit → *The sweep's own lines*, judgment 3).
    //
    // `Rendered` is honest here as of this change: linux paints
    // `recovery-kit-sweep-retry-button` on every arm `owes_work` answers true
    // for, which is exactly what that flag promises the projection.
    {
        let parked = LANDED_SWEEP.lock().unwrap_or_else(|e| e.into_inner());
        let view = parked.as_ref().map(SweepStatus::render_view);
        let copy = view
            .as_ref()
            .map(|v| v.copy(SweepRetryAffordance::Rendered));
        set_optional_line(
            &widgets.sweep_outcome_label,
            copy.as_ref().and_then(|c| {
                c.outcome
                    .as_ref()
                    .map(|line| line.resolve(crate::i18n::strings::lookup))
            }),
        );
        set_optional_line(
            &widgets.sweep_unattested_label,
            copy.as_ref().and_then(|c| {
                c.unattested
                    .as_ref()
                    .map(|line| line.resolve(crate::i18n::strings::lookup))
            }),
        );
        widgets
            .sweep_retry_btn
            .set_visible(view.as_ref().is_some_and(|v| v.owes_work()));
        widgets.sweep_retry_btn.set_sensitive(!state.busy);
    }

    set_optional_line(
        &widgets.backup_regrant_status_label,
        backup_regrant_status_text(state.aftermath.backup_regrant.as_ref()),
    );
    set_optional_line(
        &widgets.mls_reseal_status_label,
        mls_reseal_status_text(state.aftermath.mls_reseal.as_ref()),
    );
    set_optional_line(
        &widgets.grant_remint_status_label,
        grant_remint_status_text(state.aftermath.grant_remint.as_ref()),
    );
    set_optional_line(
        &widgets.corpus_reseal_status_label,
        corpus_reseal_status_text(state.aftermath.corpus_reseal.as_ref()),
    );
    set_optional_line(
        &widgets.drafts_reseal_status_label,
        drafts_reseal_status_text(state.aftermath.drafts_reseal.as_ref()),
    );
    set_optional_line(
        &widgets.mail_burn_status_label,
        mail_burn_status_text(state.aftermath.mail_burn.as_ref()),
    );
    set_optional_line(
        &widgets.inherited_filters_status_label,
        inherited_filters_status_text(state.inherited_filters),
    );

    widgets.status_label.set_text(&match status {
        Some(s) => s
            .status_line(now_secs())
            .resolve(crate::i18n::strings::lookup),
        None => rk::STATUS_LOADING.to_string(),
    });

    widgets
        .create_btn
        .set_sensitive(live(status.is_some_and(RecoveryKitStatus::allows_create)));
    widgets
        .replace_btn
        .set_sensitive(live(status.is_some_and(RecoveryKitStatus::allows_replace)));
    widgets
        .lost_btn
        .set_sensitive(live(status.is_some_and(RecoveryKitStatus::allows_lost)));
    // Rendered only in the state each repairs — the section's two affordances
    // for a state, never standing buttons (tui's `recovery_elements` gates).
    let pending = matches!(status, Some(RecoveryKitStatus::ReplacementPending(_)));
    widgets.veto_btn.set_visible(pending);
    widgets.veto_btn.set_sensitive(!state.busy);
    let reseal = status.is_some_and(RecoveryKitStatus::allows_escrow_reseal);
    widgets.escrow_reseal_btn.set_visible(reseal);
    widgets.escrow_reseal_btn.set_sensitive(!state.busy);
    // ⚠ Deliberately `is_none_or`, not `is_some_and` — armed while the status is
    // UNREAD or the chain read failed, matching tui and windows. `allows_stolen`
    // is unconditionally true once resolved, and the ceremony's authorization is
    // the KIT, never the status: `succeed_with_held_kit` does no status re-read
    // of its own, because a chain read there is a round trip a locked-out owner
    // can fail. The confirm word is the only real gate.
    widgets.stolen_btn.set_sensitive(
        !state.busy
            && status.is_none_or(RecoveryKitStatus::allows_stolen)
            && widgets.stolen_confirm_row.text() == STOLEN_CONFIRM_WORD,
    );

    // The kit-in-hand entry serves every ceremony that takes a kit the user
    // holds (`settings.md` § Recovery kit → *Kit-in-hand entry*) — replace,
    // veto, the escrow re-seal and stolen, tui's condition. `allows_stolen`
    // answers true in every resolved state ("stolen (any)" — theft does not
    // wait for the account to be tidy), so the field is effectively always up. ⚠ `is_none_or` for the button's
    // reason: an unread status must not take the field away from the ceremony
    // that needs it most.
    widgets.phrase_row.set_visible(status.is_none_or(|s| {
        s.allows_replace()
            || s.allows_stolen()
            || s.allows_escrow_reseal()
            || matches!(s, RecoveryKitStatus::ReplacementPending(_))
    }));
}

/// Unix seconds — the pending countdown's clock (`RecoveryKitStatus::status_line`
/// takes `now` as a parameter; reading the clock is the shell's job).
fn now_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs_or_zero()
}

/// Show or hide an aftermath-progress line — `None` hides it (the shared
/// projection decided nothing is owed or the pass has not started), `Some`
/// shows the resolved text. Mirrors tui's `Vec<Element>` push-or-skip, in
/// GTK's set-text/set-visible idiom.
fn set_optional_line(label: &gtk::Label, text: Option<String>) {
    match text {
        Some(t) => {
            label.set_text(&t);
            label.set_visible(true);
        }
        None => label.set_visible(false),
    }
}

/// The aftermath's `NestBackupKey` leg. `settings.md` § Recovery kit, leg 2.
fn backup_regrant_status_text(
    regrant: Option<&fauna_client_config::BackupRegrantProgress>,
) -> Option<String> {
    regrant
        .and_then(fauna_client_config::BackupRegrantProgress::status_line)
        .map(|line| line.resolve(crate::i18n::strings::lookup))
}

/// The aftermath's `__mls` leg, carrying the fifth (partly-owed) state its two
/// siblings above lack. `settings.md` § Recovery kit, leg 3.
fn mls_reseal_status_text(
    reseal: Option<&fauna_client_mls_sync::ReplicaResealProgress>,
) -> Option<String> {
    reseal
        .and_then(fauna_client_mls_sync::ReplicaResealProgress::status_line)
        .map(|line| line.resolve(crate::i18n::strings::lookup))
}

/// The aftermath's capability-grant leg. `settings.md` § Recovery kit, leg 4.
fn grant_remint_status_text(
    remint: Option<&fauna_client_capabilities::GrantRemintProgress>,
) -> Option<String> {
    remint
        .and_then(fauna_client_capabilities::GrantRemintProgress::status_line)
        .map(|line| line.resolve(crate::i18n::strings::lookup))
}

/// The aftermath's file-corpus leg. `settings.md` § Recovery kit, leg 5.
fn corpus_reseal_status_text(
    progress: Option<&fauna_client_sync::agent::CorpusResealProgress>,
) -> Option<String> {
    progress
        .and_then(fauna_client_sync::agent::CorpusResealProgress::status_line)
        .map(|line| line.resolve(crate::i18n::strings::lookup))
}

/// The aftermath's mail leg. The one leg whose *done* state is not good news
/// (`settings.md` § Recovery kit, leg 6).
fn mail_burn_status_text(
    progress: Option<&fauna_client_mail_settings::MailBurnProgress>,
) -> Option<String> {
    progress
        .and_then(fauna_client_mail_settings::MailBurnProgress::status_line)
        .map(|line| line.resolve(crate::i18n::strings::lookup))
}

/// The aftermath's drafts leg. Multi-unit like leg 3, so partly-owed is an
/// ordinary reading, not an edge case. `settings.md` § Recovery kit, leg 7
/// (rendered above leg 6).
fn drafts_reseal_status_text(
    progress: Option<&fauna_client_drafts::DraftsResealProgress>,
) -> Option<String> {
    progress
        .and_then(fauna_client_drafts::DraftsResealProgress::status_line)
        .map(|line| line.resolve(crate::i18n::strings::lookup))
}

/// The route to the successor's filter list — NOT a leg status like its seven
/// siblings above: there is no "done" line, because the honest done state of
/// a review backlog is an absent line (mirrors tui's
/// `inherited_filters_elements`).
fn inherited_filters_status_text(open: usize) -> Option<String> {
    if open == 0 {
        return None;
    }
    Some(rk::inherited_filters(&open.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testid::{find_by_test_id, widget_names};

    /// The section exposes every ui.yaml `recovery-kit-*` core-family ID, plus
    /// the two optional ones this scope wires (phrase field, minted trio) —
    /// and the three actions start disabled: no status has loaded yet, so
    /// nothing is enabled by construction (mirrors `identity_export`'s
    /// starts-collapsed pin, one layer earlier — here it's "starts unknown").
    #[test]
    fn recovery_kit_group_exposes_ui_yaml_ids_and_starts_disabled() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let error_label = gtk::Label::new(None);
            let (group, _refresh) = build_recovery_kit_group(&error_label);
            let names = widget_names(&group);
            for id in [
                "recovery-kit-section",
                "recovery-kit-status",
                "recovery-kit-create-button",
                "recovery-kit-replace-button",
                "recovery-kit-lost-button",
                "recovery-pending-veto-button",
                "recovery-kit-escrow-reseal-button",
                "recovery-entry-phrase-field",
                "recovery-kit-secret-display",
                "recovery-kit-secret-copy-btn",
                "recovery-kit-qr",
                "recovery-kit-backup-regrant-status",
                "recovery-kit-mls-reseal-status",
                "recovery-kit-grant-remint-status",
                "recovery-kit-corpus-reseal-status",
                "recovery-kit-drafts-reseal-status",
                "recovery-kit-mail-burn-status",
                "recovery-kit-inherited-filters-status",
                "recovery-kit-sweep-retry-button",
                "identity-stolen-confirm-field",
                "identity-stolen-button",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing ui.yaml ID {id}; got {names:?}"
                );
            }

            for btn_id in [
                "recovery-kit-create-button",
                "recovery-kit-replace-button",
                "recovery-kit-lost-button",
                // Disabled with the confirm field empty — the type-to-confirm
                // gate is a SECOND condition on top of the status one, and it
                // is unmet at build (`settings.md` § Recovery kit).
                "identity-stolen-button",
            ] {
                let w = find_by_test_id(&group, btn_id).expect(btn_id);
                assert!(!w.is_sensitive(), "{btn_id} must start disabled");
            }

            // No succession ran on this app run, so the sweep owes nothing and
            // its retry is absent from the screen — a standing button here would
            // offer to finish a pass that never started.
            let retry = find_by_test_id(&group, "recovery-kit-sweep-retry-button").unwrap();
            assert!(
                !retry.is_visible(),
                "recovery-kit-sweep-retry-button must start hidden"
            );
            // The two kit-in-hand repairs are affordances for a STATE: with the
            // status unread there is no pending window to veto and no missing
            // escrow to re-seal, so neither is on screen.
            for id in [
                "recovery-pending-veto-button",
                "recovery-kit-escrow-reseal-button",
            ] {
                let w = find_by_test_id(&group, id).expect(id);
                assert!(!w.is_visible(), "{id} must start hidden");
            }
            // The kit-in-hand field is up from the first paint: the succession
            // ceremony consumes it in every status, including the unread one.
            let phrase = find_by_test_id(&group, "recovery-entry-phrase-field").unwrap();
            assert!(phrase.is_visible(), "phrase field must start visible");

            // Every aftermath-progress line starts hidden: a section paints a
            // line only once a leg has reported (`succession_aftermath`), and
            // nothing has at build time.
            for id in [
                "recovery-kit-backup-regrant-status",
                "recovery-kit-mls-reseal-status",
                "recovery-kit-grant-remint-status",
                "recovery-kit-corpus-reseal-status",
                "recovery-kit-drafts-reseal-status",
                "recovery-kit-mail-burn-status",
                "recovery-kit-inherited-filters-status",
            ] {
                let w = find_by_test_id(&group, id).expect(id);
                assert!(!w.is_visible(), "{id} must start hidden");
            }
        });
    }

    /// The enablement matrix — one state per row, same cells
    /// `fauna_client_recovery::status`'s own test pins, read through this
    /// section's `repaint` rather than re-derived here (priority #2: nothing
    /// in this module decides enablement, so nothing in this test should
    /// either).
    ///
    /// The last rows read the *gate* rather than the projection, because the
    /// two are not independent: all three buttons declare `OnlineOnly` kinds
    /// (`build_recovery_kit_group`), and the gate re-decides a control the
    /// moment the page enables it. So the projection's own verdicts are only
    /// legible while the link is up, and the offline verdict is the other half
    /// of the same matrix rather than a separate concern.
    #[test]
    fn repaint_follows_the_projection_not_a_local_match() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            // Start from a link that gates nothing, so the assertions below
            // read `repaint`'s intent and not `offline_gate`'s veto over it —
            // and from an empty registry, since the gate's state is a
            // `thread_local!` this process's ONE GTK thread shares with every
            // other widget test (`offline_gate::reset_for_test`'s own reason).
            crate::offline_gate::reset_for_test("connected");
            let error_label = gtk::Label::new(None);
            let (group, _refresh) = build_recovery_kit_group(&error_label);
            let create = find_by_test_id(&group, "recovery-kit-create-button").unwrap();
            let replace = find_by_test_id(&group, "recovery-kit-replace-button").unwrap();
            let lost = find_by_test_id(&group, "recovery-kit-lost-button").unwrap();
            let veto = find_by_test_id(&group, "recovery-pending-veto-button").unwrap();
            let reseal = find_by_test_id(&group, "recovery-kit-escrow-reseal-button").unwrap();
            let phrase = find_by_test_id(&group, "recovery-entry-phrase-field").unwrap();
            let status_label = find_by_test_id(&group, "recovery-kit-status").unwrap();

            let widgets = Widgets {
                status_label: status_label.downcast().unwrap(),
                create_btn: create.clone().downcast().unwrap(),
                replace_btn: replace.clone().downcast().unwrap(),
                lost_btn: lost.clone().downcast().unwrap(),
                veto_btn: veto.clone().downcast().unwrap(),
                escrow_reseal_btn: reseal.clone().downcast().unwrap(),
                sweep_retry_btn: find_by_test_id(&group, "recovery-kit-sweep-retry-button")
                    .unwrap()
                    .downcast()
                    .unwrap(),
                stolen_confirm_row: find_by_test_id(&group, "identity-stolen-confirm-field")
                    .unwrap()
                    .downcast()
                    .unwrap(),
                stolen_btn: find_by_test_id(&group, "identity-stolen-button")
                    .unwrap()
                    .downcast()
                    .unwrap(),
                sweep_outcome_label: gtk::Label::new(None),
                sweep_unattested_label: gtk::Label::new(None),
                phrase_row: phrase.clone().downcast().unwrap(),
                minted_box: gtk::Box::new(gtk::Orientation::Vertical, 0),
                secret_label: gtk::Label::new(None),
                qr_area: gtk::DrawingArea::new(),
                error_label: gtk::Label::new(None),
                backup_regrant_status_label: gtk::Label::new(None),
                mls_reseal_status_label: gtk::Label::new(None),
                grant_remint_status_label: gtk::Label::new(None),
                corpus_reseal_status_label: gtk::Label::new(None),
                drafts_reseal_status_label: gtk::Label::new(None),
                mail_burn_status_label: gtk::Label::new(None),
                inherited_filters_status_label: gtk::Label::new(None),
            };

            let never_created = RecoveryState {
                status: Some(RecoveryKitStatus::NeverCreated),
                busy: false,
                ..Default::default()
            };
            repaint(&never_created, &widgets);
            assert!(create.is_sensitive());
            assert!(!replace.is_sensitive());
            assert!(!lost.is_sensitive());
            // The stolen gate's SECOND condition, independent of the status one:
            // `allows_stolen` is true in every resolved state ("stolen (any)"),
            // so what is holding the button down here is the empty confirm field.
            assert!(
                !widgets.stolen_btn.is_sensitive(),
                "an empty confirm field must hold the succession gate shut"
            );
            widgets.stolen_confirm_row.set_text("succeed");
            repaint(&never_created, &widgets);
            assert!(
                !widgets.stolen_btn.is_sensitive(),
                "the confirm word is a fixed literal, not a case-insensitive match"
            );
            widgets
                .stolen_confirm_row
                .set_text(fauna_client_recovery::ceremony::STOLEN_CONFIRM_WORD);
            repaint(&never_created, &widgets);
            assert!(
                widgets.stolen_btn.is_sensitive(),
                "the exact confirm word arms the succession gate in every status"
            );
            // ⚠ Re-assert the fixture AFTER the field edits. `set_text` fires the
            // section's own `connect_changed`, which repaints from the section's
            // live `RecoveryState` (status unread here) over these same widgets —
            // so a status assertion taken straight after an edit reads the
            // section's state, not this test's.
            widgets.stolen_confirm_row.set_text("");
            repaint(&never_created, &widgets);
            // Up even in `NeverCreated`, where replace is dead: theft does not
            // wait for the account to be tidy, and the ceremony that answers it
            // reads its kit from this field.
            assert!(phrase.is_visible());
            assert_eq!(widgets.status_label.text(), rk::STATUS_NEVER_CREATED);

            let registered = RecoveryState {
                status: Some(RecoveryKitStatus::Registered),
                busy: false,
                ..Default::default()
            };
            repaint(&registered, &widgets);
            assert!(!create.is_sensitive());
            assert!(replace.is_sensitive());
            assert!(lost.is_sensitive());
            assert!(phrase.is_visible());
            assert_eq!(widgets.status_label.text(), rk::STATUS_REGISTERED);
            // A healthy kit owes neither repair.
            assert!(!veto.is_visible(), "no window pends, so nothing to veto");
            assert!(
                !reseal.is_visible(),
                "escrow is stored, so nothing to re-seal"
            );

            // The no-escrow gap: the re-seal is the ONE repair that keeps the
            // kit in hand, and it renders only here.
            let no_escrow = RecoveryState {
                status: Some(RecoveryKitStatus::RegisteredNoEscrow),
                busy: false,
                ..Default::default()
            };
            repaint(&no_escrow, &widgets);
            assert!(reseal.is_visible() && reseal.is_sensitive());
            assert!(!veto.is_visible());
            assert!(
                phrase.is_visible(),
                "the re-seal reads the kit-in-hand field"
            );

            // The templated arm — the one place a resolve-vs-hand-match
            // divergence would show up as a wrong day count rather than a
            // wrong string entirely.
            let pending = RecoveryState {
                status: Some(RecoveryKitStatus::ReplacementPending(
                    fauna_client_recovery::replacement::PendingReplacement {
                        new_recovery_pubkey_hex: "ab".repeat(32),
                        requested_at: now_secs(),
                        lands_at: now_secs() + 3 * 86_400,
                    },
                )),
                busy: false,
                ..Default::default()
            };
            repaint(&pending, &widgets);
            assert_eq!(
                widgets.status_label.text(),
                rk::status_replacement_pending("3")
            );
            // The pending window is the veto's one state.
            assert!(veto.is_visible() && veto.is_sensitive());
            assert!(!reseal.is_visible());
            assert!(phrase.is_visible(), "the veto reads the kit-in-hand field");

            // Busy disables every gesture at once, whatever the status —
            // a double click cannot race a second registration.
            let busy = RecoveryState {
                status: Some(RecoveryKitStatus::Registered),
                busy: true,
                ..Default::default()
            };
            repaint(&busy, &widgets);
            assert!(!replace.is_sensitive());
            assert!(!lost.is_sensitive());
            let busy_pending = RecoveryState {
                busy: true,
                ..pending
            };
            repaint(&busy_pending, &widgets);
            assert!(!veto.is_sensitive(), "busy holds the veto too");

            // The gate rides ON TOP of the projection, never underneath it:
            // the same `Registered` status that enables replace/lost must not
            // hand a user with no nest a gesture that can only fail — both
            // kinds are `OnlineOnly` (`walk.rs`'s I6, on this one surface,
            // where I6 itself can only see what the registry still holds).
            repaint(&registered, &widgets);
            assert!(replace.is_sensitive(), "precondition: the link is still up");
            crate::offline_gate::set_connection_state("disconnected");
            assert!(
                !replace.is_sensitive(),
                "replace issues fauna.recovery.registration.submit, which is OnlineOnly"
            );
            assert!(
                !lost.is_sensitive(),
                "lost issues fauna.recovery.replacement.request, which is OnlineOnly"
            );
            // Left `"disconnected"` deliberately — it is the state `app.rs`
            // itself starts in, and the state `walk.rs`'s I6 needs to be
            // asserting anything at all.
        });
    }

    /// Each aftermath-progress line renders iff its shared projection has
    /// something to say, and shows exactly the resolved copy — the mechanism
    /// test the module doc promises: fixture `Progress` values constructed
    /// directly, no live succession ceremony needed (split the mile,
    /// test the mechanism headlessly).
    #[test]
    fn aftermath_progress_lines_show_only_when_the_projection_has_something_to_say() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let error_label = gtk::Label::new(None);
            let (group, _refresh) = build_recovery_kit_group(&error_label);
            let regrant = find_by_test_id(&group, "recovery-kit-backup-regrant-status").unwrap();
            let mls = find_by_test_id(&group, "recovery-kit-mls-reseal-status").unwrap();
            let remint = find_by_test_id(&group, "recovery-kit-grant-remint-status").unwrap();
            let corpus = find_by_test_id(&group, "recovery-kit-corpus-reseal-status").unwrap();
            let drafts = find_by_test_id(&group, "recovery-kit-drafts-reseal-status").unwrap();
            let mail = find_by_test_id(&group, "recovery-kit-mail-burn-status").unwrap();
            let filters = find_by_test_id(&group, "recovery-kit-inherited-filters-status").unwrap();
            let status_label = find_by_test_id(&group, "recovery-kit-status").unwrap();

            let widgets = Widgets {
                status_label: status_label.downcast().unwrap(),
                create_btn: find_by_test_id(&group, "recovery-kit-create-button")
                    .unwrap()
                    .downcast()
                    .unwrap(),
                replace_btn: find_by_test_id(&group, "recovery-kit-replace-button")
                    .unwrap()
                    .downcast()
                    .unwrap(),
                lost_btn: find_by_test_id(&group, "recovery-kit-lost-button")
                    .unwrap()
                    .downcast()
                    .unwrap(),
                veto_btn: find_by_test_id(&group, "recovery-pending-veto-button")
                    .unwrap()
                    .downcast()
                    .unwrap(),
                escrow_reseal_btn: find_by_test_id(&group, "recovery-kit-escrow-reseal-button")
                    .unwrap()
                    .downcast()
                    .unwrap(),
                sweep_retry_btn: find_by_test_id(&group, "recovery-kit-sweep-retry-button")
                    .unwrap()
                    .downcast()
                    .unwrap(),
                stolen_confirm_row: find_by_test_id(&group, "identity-stolen-confirm-field")
                    .unwrap()
                    .downcast()
                    .unwrap(),
                stolen_btn: find_by_test_id(&group, "identity-stolen-button")
                    .unwrap()
                    .downcast()
                    .unwrap(),
                sweep_outcome_label: gtk::Label::new(None),
                sweep_unattested_label: gtk::Label::new(None),
                phrase_row: find_by_test_id(&group, "recovery-entry-phrase-field")
                    .unwrap()
                    .downcast()
                    .unwrap(),
                minted_box: gtk::Box::new(gtk::Orientation::Vertical, 0),
                secret_label: gtk::Label::new(None),
                qr_area: gtk::DrawingArea::new(),
                error_label: gtk::Label::new(None),
                backup_regrant_status_label: regrant.clone().downcast().unwrap(),
                mls_reseal_status_label: mls.clone().downcast().unwrap(),
                grant_remint_status_label: remint.clone().downcast().unwrap(),
                corpus_reseal_status_label: corpus.clone().downcast().unwrap(),
                drafts_reseal_status_label: drafts.clone().downcast().unwrap(),
                mail_burn_status_label: mail.clone().downcast().unwrap(),
                inherited_filters_status_label: filters.clone().downcast().unwrap(),
            };

            // All `None` (the ordinary account, or before the drive
            // ever runs): every line stays hidden.
            repaint(&RecoveryState::default(), &widgets);
            for w in [&regrant, &mls, &remint, &corpus, &drafts, &mail, &filters] {
                assert!(!w.is_visible(), "must stay hidden with nothing to report");
            }

            // Every leg's Running/Done/owed-elsewhere/Failed arm, one fixture
            // each, asserted against the SAME generated strings tui renders
            // (never a locally re-derived string).
            use fauna_client_capabilities::GrantRemintProgress;
            use fauna_client_config::BackupRegrantProgress;
            use fauna_client_drafts::{DraftsResealOutcome, DraftsResealProgress};
            use fauna_client_mail_settings::MailBurnProgress;
            use fauna_client_mls_sync::{ReplicaResealOutcome, ReplicaResealProgress};
            use fauna_client_sync::agent::{CorpusResealOutcome, CorpusResealProgress};

            let state = RecoveryState {
                aftermath: AftermathProgress {
                    backup_regrant: Some(BackupRegrantProgress::Running),
                    // The fifth (partly-owed) state: `owed > 0` on `Resealed`,
                    // never a dedicated variant.
                    mls_reseal: Some(ReplicaResealProgress::Settled(
                        ReplicaResealOutcome::Resealed {
                            provider: true,
                            histories: 1,
                            owed: 1,
                        },
                    )),
                    grant_remint: Some(GrantRemintProgress::Failed("nest unreachable".into())),
                    // The silent steady state: nothing moved, nothing owed.
                    corpus_reseal: Some(CorpusResealProgress::Settled(
                        CorpusResealOutcome::Resealed {
                            resealed: 0,
                            owed: 0,
                        },
                    )),
                    drafts_reseal: Some(DraftsResealProgress::Settled(
                        DraftsResealOutcome::AlreadyCurrent,
                    )),
                    mail_burn: Some(MailBurnProgress::Running),
                },
                inherited_filters: 3,
                ..Default::default()
            };
            repaint(&state, &widgets);

            assert_eq!(
                widgets.backup_regrant_status_label.text(),
                rk::BACKUP_REGRANT_RUNNING
            );
            assert!(regrant.is_visible());
            assert_eq!(
                widgets.mls_reseal_status_label.text(),
                rk::MLS_RESEAL_PARTLY_OWED_ELSEWHERE
            );
            assert!(mls.is_visible());
            assert_eq!(
                widgets.grant_remint_status_label.text().as_str(),
                rk::grant_remint_failed("nest unreachable")
            );
            assert!(remint.is_visible());
            // Corpus's silent steady state (`resealed: 0, owed: 0`) and
            // drafts' `AlreadyCurrent` are both "nothing is owed" — the line
            // must stay hidden, mirroring tui's "no permanent no-op clutter"
            // reasoning.
            assert!(
                !corpus.is_visible(),
                "the silent steady state renders nothing"
            );
            assert!(!drafts.is_visible(), "AlreadyCurrent renders nothing");
            assert_eq!(widgets.mail_burn_status_label.text(), rk::MAIL_BURN_RUNNING);
            assert!(mail.is_visible());
            assert_eq!(
                widgets.inherited_filters_status_label.text().as_str(),
                rk::inherited_filters("3")
            );
            assert!(filters.is_visible());

            // Zero open filters hides the line again — it is a backlog
            // count, not a leg status, and disappears once the backlog does.
            repaint(
                &RecoveryState {
                    inherited_filters: 0,
                    ..Default::default()
                },
                &widgets,
            );
            assert!(!filters.is_visible());
        });
    }

    /// The drive's door into this section, through production code: a leg's
    /// report repaints the section on screen at once, and a section rebuilt
    /// after a report still carries it — the shell rebuilds pages, and the
    /// account switch rebuilds every window while the pass runs.
    #[test]
    fn an_aftermath_report_paints_now_and_survives_a_section_rebuild() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            clear_session_aftermath();
            let error_label = gtk::Label::new(None);

            let (first, _refresh) = build_recovery_kit_group(&error_label);
            let reseal = find_by_test_id(&first, "recovery-kit-drafts-reseal-status").unwrap();
            assert!(!reseal.is_visible(), "nothing has reported yet");

            crate::settings::apply_aftermath_progress(AftermathUpdate::DraftsReseal(
                fauna_client_drafts::DraftsResealProgress::Settled(
                    fauna_client_drafts::DraftsResealOutcome::NoKeyOpensIt,
                ),
            ));
            assert!(
                reseal.is_visible(),
                "a report repaints the section on screen"
            );
            assert_eq!(
                reseal.downcast_ref::<gtk::Label>().unwrap().text(),
                rk::DRAFTS_RESEAL_OWED_ELSEWHERE
            );

            // A rebuilt section: the next report must repaint the line reported
            // before it existed as well as its own.
            let (second, _refresh) = build_recovery_kit_group(&error_label);
            crate::settings::apply_aftermath_progress(AftermathUpdate::InheritedFilters(2));
            let reseal = find_by_test_id(&second, "recovery-kit-drafts-reseal-status").unwrap();
            let filters =
                find_by_test_id(&second, "recovery-kit-inherited-filters-status").unwrap();
            assert!(
                reseal.is_visible(),
                "a report from before the rebuild still renders"
            );
            assert_eq!(
                filters
                    .downcast_ref::<gtk::Label>()
                    .unwrap()
                    .text()
                    .as_str(),
                rk::inherited_filters("2")
            );

            // An actor change forgets it: the next identity runs its own pass.
            clear_session_aftermath();
            crate::settings::apply_aftermath_progress(AftermathUpdate::InheritedFilters(0));
            assert!(!reseal.is_visible());
            assert!(!filters.is_visible());
            clear_session_aftermath();
        });
    }

    /// A settled mail burn re-reads the Mail page (tui's
    /// `refold_mail_snapshot`, web's re-hydrate on `configStageSettled`): the
    /// burn marked rows on the account's mail custody that the page's own
    /// machine hydrated before, and without the re-read the page keeps
    /// painting them as live until it is reopened. Only a SETTLED burn fires
    /// it — a running or failed one changed nothing the page shows.
    #[test]
    fn a_settled_mail_burn_rereads_the_mail_page() {
        use fauna_client_mail_settings::{MailBurnOutcome, MailBurnProgress};
        use std::cell::Cell;
        use std::rc::Rc;
        crate::testid::run_on_gtk_thread(|| {
            clear_session_aftermath();
            let rereads = Rc::new(Cell::new(0usize));
            {
                let rereads = Rc::clone(&rereads);
                crate::settings::set_mail_burn_settled_handler(Rc::new(move || {
                    rereads.set(rereads.get() + 1);
                }));
            }

            crate::settings::apply_aftermath_progress(AftermathUpdate::MailBurn(
                MailBurnProgress::Running,
            ));
            crate::settings::apply_aftermath_progress(AftermathUpdate::MailBurn(
                MailBurnProgress::Failed("nest unreachable".into()),
            ));
            assert_eq!(rereads.get(), 0, "an unfinished burn marked no row");

            crate::settings::apply_aftermath_progress(AftermathUpdate::MailBurn(
                MailBurnProgress::Settled(MailBurnOutcome::Burned { credentials: 2 }),
            ));
            assert_eq!(rereads.get(), 1, "a settled burn re-reads the Mail page");

            // Another leg's report is not the Mail page's business.
            crate::settings::apply_aftermath_progress(AftermathUpdate::InheritedFilters(0));
            assert_eq!(rereads.get(), 1);
            clear_session_aftermath();
        });
    }

    /// A minted kit leaves the screen when the section is re-entered — the
    /// shown-once custody rule (`identity-succession.md` § The RecoveryKey —
    /// *Custody*), driven through the production doors: the `RecoveryKitMinted`
    /// fold paints it, the nav-edge `refresh` closure clears it.
    ///
    /// ⚠ **Newly reachable, and it was a real hole.** The section is built once
    /// with the window and only hidden/re-shown by the shell's stack, so
    /// `minted_box.set_visible(true)` had no counterpart anywhere: a kit minted
    /// by `recovery-kit-create-button` stayed on screen — secret, copy target and
    /// QR — for the life of the process, across every navigation. Nothing
    /// witnessed it until linux grew the succession's closing act (this row) and
    /// the journey's own re-entry assertion finally had a kit to find.
    ///
    /// The `MintedKit` is asserted gone alongside the label, not just the
    /// visibility: it holds the `fauna://recovery` URI `recovery-kit-secret-copy-btn`
    /// puts on the clipboard, and a hidden-but-live copy target is the same
    /// secret still in reach.
    #[test]
    fn a_minted_kit_leaves_the_screen_when_the_section_is_re_entered() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let error_label = gtk::Label::new(None);
            let (group, refresh) = build_recovery_kit_group(&error_label);
            let secret_display: gtk::Label = find_by_test_id(&group, "recovery-kit-secret-display")
                .unwrap()
                .downcast()
                .unwrap();

            const MINTED: &str = "f00dcafe";
            crate::settings::apply_recovery_kit_minted(Ok((
                MINTED.to_string(),
                RecoveryKitStatus::Registered,
            )));
            assert_eq!(
                secret_display.text().as_str(),
                MINTED,
                "the mint shows the secret once — this is the whole point of the ceremony"
            );

            // The nav edge into Account. `refresh` is what the settings shell
            // calls on `visible-child-name`, so this is the same door a user
            // walking back into the section takes.
            refresh();
            assert!(
                secret_display.text().is_empty(),
                "re-entering the section must not re-display the minted secret"
            );
        });
    }

    /// This fix's own probe, run and reverted at
    /// grading time before this fix landed (`git status --short` confirmed empty
    /// afterward): a pending stolen-ceremony persist-failure message is the
    /// ONLY surviving copy of a successor's identity secret, and must survive
    /// the account page's next UNRELATED event — here, a completely separate
    /// kit mint succeeding (`settings.md` § Recovery kit → *The
    /// persist-failure message survives the page*; ). Reverting
    /// any of `render_account_error_label`'s call sites back to a bare
    /// `render_error_label`/`Label::set_visible` reds this.
    #[test]
    fn a_pending_persist_failure_message_survives_an_unrelated_mint() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            // Defensive: `PENDING_STOLEN_FAILED_MESSAGE` is shared by every
            // test on this process's one dedicated GTK thread (every
            // `run_on_gtk_thread` test runs there in sequence — see its doc
            // comment) — start from "nothing pending" regardless of order.
            crate::settings::acknowledge_stolen_failed_message();

            let error_label = gtk::Label::new(None);
            let (_group, _refresh) = build_recovery_kit_group(&error_label);

            const SEED_MESSAGE: &str = "import this recovery phrase: fffff...";
            crate::settings::park_stolen_failed_message(SEED_MESSAGE.to_string());
            assert_eq!(error_label.text().as_str(), SEED_MESSAGE);
            assert!(error_label.is_visible());

            // The unrelated event: a completely separate ceremony's kit mint
            // lands successfully. Before this fix, its `Ok` arm hid the
            // shared label unconditionally.
            crate::settings::apply_recovery_kit_minted(Ok((
                "unrelated-secret".to_string(),
                RecoveryKitStatus::Registered,
            )));
            assert_eq!(
                error_label.text().as_str(),
                SEED_MESSAGE,
                "an unrelated mint success must never clobber a still-pending \
                 persist-failure message — it is the only surviving copy of \
                 a successor's identity secret"
            );
            assert!(
                error_label.is_visible(),
                "the message must stay ON SCREEN, not just retained off-screen"
            );

            // The acknowledgment gesture: the user leaves the Account
            // sub-page. Only now may an unrelated write clear the label.
            crate::settings::acknowledge_stolen_failed_message();
            crate::settings::apply_recovery_kit_minted(Ok((
                "unrelated-secret-2".to_string(),
                RecoveryKitStatus::Registered,
            )));
            assert!(
                !error_label.is_visible(),
                "once acknowledged, an unrelated write may clear the label again"
            );
        });
    }

    /// The succession's owed kit: who owes it, who may claim it, and what it
    /// takes to discharge it — the whole contract of
    /// [`SUCCESSION_KIT_OWED`], linux's leg of `identity-succession.md`
    /// § The RecoveryKey → *At succession*.
    ///
    /// ⚠ Drives the process-globals directly, for [`LANDED_SWEEP`]'s reason:
    /// they ARE the mechanism. The obligation exists precisely to cross an
    /// account switch that destroys every window and clears everything
    /// actor-scoped, so a test over a local fixture would pass on a build where
    /// the carrying never happened. Run through `run_on_gtk_thread` — not for
    /// any widget, but because it is this file's one serialization point, and
    /// these statics are shared by every test in the process.
    ///
    /// The mint itself is not exercised here (it needs a nest); the journey
    /// `test_identity_succession_ceremony.py::test_a_kit_holder_takes_the_
    /// account_back_and_comes_back_up_as_the_successor` is what proves a real
    /// secret reaches the screen unbidden.
    #[test]
    fn only_the_successor_can_claim_the_owed_kit_and_only_once() {
        crate::testid::run_on_gtk_thread(|| {
            const SUCCESSOR: &str = "aa11";
            const PREDECESSOR: &str = "bb22";
            clear_succession_kit_debt();

            // Nothing is owed on an ordinary sign-in — the cost of the check on
            // every post-auth pass that never ran a ceremony.
            assert!(
                !claim_succession_kit(SUCCESSOR),
                "a session that ran no ceremony owes nothing"
            );

            owe_succession_kit(SUCCESSOR.to_string());
            // The seat bind: between the fold recording this and the switch
            // landing, the RETIRED session can still reach its own post-auth
            // hook — and a mint there would authenticate as the identity the
            // ceremony just retired.
            assert!(
                !claim_succession_kit(PREDECESSOR),
                "only the successor may perform the mint"
            );
            assert!(claim_succession_kit(SUCCESSOR), "the successor may");
            // Single claim: a second post-auth pass minting again would register
            // a kit nobody was shown, which the § names as strictly worse than
            // never-created.
            assert!(
                !claim_succession_kit(SUCCESSOR),
                "the obligation is claimed exactly once"
            );

            clear_succession_kit_debt();
        });
    }

    /// The relaunch adoption's owed sweep: seat-bound and claimed once, exactly
    /// like the kit, and cleared with it.
    #[test]
    fn only_the_successor_can_claim_the_owed_sweep_and_only_once() {
        crate::testid::run_on_gtk_thread(|| {
            const SUCCESSOR: &str = "aa11";
            const PREDECESSOR: &str = "bb22";
            clear_succession_kit_debt();

            assert!(
                !claim_succession_sweep(SUCCESSOR),
                "a session that adopted nothing owes no sweep"
            );
            owe_succession_sweep(SUCCESSOR.to_string());
            assert!(
                !claim_succession_sweep(PREDECESSOR),
                "only the successor can author the sweep"
            );
            assert!(claim_succession_sweep(SUCCESSOR), "the successor may");
            assert!(
                !claim_succession_sweep(SUCCESSOR),
                "the sweep is pressed exactly once"
            );

            owe_succession_sweep(SUCCESSOR.to_string());
            clear_succession_kit_debt();
            assert!(
                !claim_succession_sweep(SUCCESSOR),
                "sign-out and reset forget the owed sweep with the owed kit"
            );
        });
    }

    /// A kit that was minted but never SHOWN leaves the obligation outstanding.
    ///
    /// The two events are different and only the second one discharges anything
    /// (apple's `rearmUnshownKit`, iOS 2026-08-26). The failure is not exotic:
    /// the ceremony revokes every session of the account inside the nest's own
    /// transaction, so the successor's very first mint races its own reconnect.
    #[test]
    fn a_mint_that_never_reached_the_screen_leaves_the_kit_owed() {
        crate::testid::run_on_gtk_thread(|| {
            const SUCCESSOR: &str = "cc33";
            clear_succession_kit_debt();

            owe_succession_kit(SUCCESSOR.to_string());
            assert!(claim_succession_kit(SUCCESSOR));
            settle_kit_discharge(false);
            assert!(
                claim_succession_kit(SUCCESSOR),
                "an unshown kit is still owed — the next session must mint again"
            );

            // And the shown case spends it for good: re-arming a discharged
            // obligation would mint a second kit over a first the user holds.
            settle_kit_discharge(true);
            assert!(
                !claim_succession_kit(SUCCESSOR),
                "a kit the user was shown discharges the obligation"
            );

            clear_succession_kit_debt();
        });
    }

    /// An ordinary create/replace press that fails must not touch a succession's
    /// obligation — `settle_kit_discharge` answers only for a discharge it
    /// actually claimed.
    ///
    /// Without this, a user who pressed `recovery-kit-create-button` and got a
    /// network error would silently re-arm (or spend) a debt that press had
    /// nothing to do with.
    #[test]
    fn an_ordinary_failed_mint_does_not_touch_the_owed_kit() {
        crate::testid::run_on_gtk_thread(|| {
            const SUCCESSOR: &str = "dd44";
            clear_succession_kit_debt();

            owe_succession_kit(SUCCESSOR.to_string());
            // No claim ran: this outcome belongs to some other gesture.
            settle_kit_discharge(false);
            settle_kit_discharge(true);
            assert!(
                claim_succession_kit(SUCCESSOR),
                "an unrelated mint outcome leaves the succession's debt exactly as it was"
            );

            clear_succession_kit_debt();
        });
    }

    /// The owed kit outlives an account SWITCH but not an all-accounts erase.
    ///
    /// The switch is the boundary the obligation exists to cross, so
    /// `reset_actor_scoped_state` deliberately cannot reach these cells; the
    /// erase destroys every identity on the box, so there is no successor left
    /// to owe a kit to. `account_scope::erase_all_known_accounts` is the one
    /// caller, beside `clear_succession_sweep`.
    #[test]
    fn the_owed_kit_does_not_survive_an_all_accounts_erase() {
        crate::testid::run_on_gtk_thread(|| {
            const SUCCESSOR: &str = "ee55";
            clear_succession_kit_debt();

            owe_succession_kit(SUCCESSOR.to_string());
            // The switch's own teardown touches neither cell — nothing to call
            // here is exactly the point.
            crate::actor_scope::reset_actor_scoped_state(
                fauna_client_account_runtime::StopReason::AccountSwitch,
                || {},
            );
            assert!(
                claim_succession_kit(SUCCESSOR),
                "the obligation crosses the account switch"
            );

            // And a claim still in flight when the box is erased dies with it,
            // rather than re-arming onto an identity that no longer exists.
            owe_succession_kit(SUCCESSOR.to_string());
            assert!(claim_succession_kit(SUCCESSOR));
            clear_succession_kit_debt();
            settle_kit_discharge(false);
            assert!(
                !claim_succession_kit(SUCCESSOR),
                "an erase leaves no successor to owe a kit to"
            );
        });
    }

    /// The sweep's two lines and the retry button are ONE projection's answer.
    ///
    /// The ruling this pins (`settings.md` § Recovery kit → *The sweep's own
    /// lines*, judgment 3) is that the copy naming the button and the button's
    /// own presence must agree — which they can only do if both are read off
    /// the same `SweepView`. So the assertions below always check them
    /// together: an arm that owes work must show the button AND a line, and the
    /// one `ran` arm that owes nothing must show neither.
    ///
    /// ⚠ Drives the process-global `LANDED_SWEEP` rather than a field, because
    /// that global IS the mechanism — it is what carries the outcome across the
    /// account switch, and a test over a local fixture would pass on a build
    /// where the parking never happened. Cleared at the end for the same reason
    /// `offline_gate::reset_for_test` exists: this process has one GTK thread
    /// and every widget test shares its statics.
    #[test]
    fn the_sweep_lines_and_the_retry_are_the_same_projections_answer() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            crate::offline_gate::reset_for_test("connected");
            let error_label = gtk::Label::new(None);
            let (group, _refresh) = build_recovery_kit_group(&error_label);
            let retry: gtk::Button = find_by_test_id(&group, "recovery-kit-sweep-retry-button")
                .unwrap()
                .downcast()
                .unwrap();
            let widgets = Widgets {
                status_label: find_by_test_id(&group, "recovery-kit-status")
                    .unwrap()
                    .downcast()
                    .unwrap(),
                create_btn: find_by_test_id(&group, "recovery-kit-create-button")
                    .unwrap()
                    .downcast()
                    .unwrap(),
                replace_btn: find_by_test_id(&group, "recovery-kit-replace-button")
                    .unwrap()
                    .downcast()
                    .unwrap(),
                lost_btn: find_by_test_id(&group, "recovery-kit-lost-button")
                    .unwrap()
                    .downcast()
                    .unwrap(),
                veto_btn: gtk::Button::new(),
                escrow_reseal_btn: gtk::Button::new(),
                sweep_retry_btn: retry.clone(),
                stolen_confirm_row: find_by_test_id(&group, "identity-stolen-confirm-field")
                    .unwrap()
                    .downcast()
                    .unwrap(),
                stolen_btn: find_by_test_id(&group, "identity-stolen-button")
                    .unwrap()
                    .downcast()
                    .unwrap(),
                sweep_outcome_label: gtk::Label::new(None),
                sweep_unattested_label: gtk::Label::new(None),
                phrase_row: find_by_test_id(&group, "recovery-entry-phrase-field")
                    .unwrap()
                    .downcast()
                    .unwrap(),
                minted_box: gtk::Box::new(gtk::Orientation::Vertical, 0),
                secret_label: gtk::Label::new(None),
                qr_area: gtk::DrawingArea::new(),
                error_label: gtk::Label::new(None),
                backup_regrant_status_label: gtk::Label::new(None),
                mls_reseal_status_label: gtk::Label::new(None),
                grant_remint_status_label: gtk::Label::new(None),
                corpus_reseal_status_label: gtk::Label::new(None),
                drafts_reseal_status_label: gtk::Label::new(None),
                mail_burn_status_label: gtk::Label::new(None),
                inherited_filters_status_label: gtk::Label::new(None),
            };
            let state = RecoveryState::default();

            // No succession this run: no lines, no button.
            clear_succession_sweep();
            repaint(&state, &widgets);
            assert!(!widgets.sweep_outcome_label.is_visible());
            assert!(!widgets.sweep_unattested_label.is_visible());
            assert!(!retry.is_visible());

            // Conversations were never up, so the groups still hold the old
            // leaf — the commonest owing arm, and the one whose copy names this
            // button by label.
            park_succession_sweep(SweepStatus::NoEngine);
            repaint(&state, &widgets);
            assert!(
                retry.is_visible(),
                "no-engine owes work, so the retry renders"
            );
            assert_eq!(widgets.sweep_outcome_label.text().as_str(), rk::SWEEP_NONE);
            assert!(widgets.sweep_outcome_label.is_visible());
            // Two facts, two lines — the roster half says nothing on an arm that
            // swept nobody, and it never rides as a qualifier on the first.
            assert!(!widgets.sweep_unattested_label.is_visible());

            // A succession over an account with no groups at all: the one `ran`
            // arm that owes nothing and says nothing. "Removed from all 0 of
            // your groups" is reassurance by vacuity, and a standing button
            // would offer to finish a pass that never started.
            park_succession_sweep(SweepStatus::Ran(Box::default()));
            repaint(&state, &widgets);
            assert!(!retry.is_visible(), "an empty sweep owes nothing");
            assert!(!widgets.sweep_outcome_label.is_visible());

            // Busy desensitizes without hiding: the gate is unfinished work, and
            // a ceremony in flight must not make the affordance vanish.
            park_succession_sweep(SweepStatus::Failed("nest unreachable".into()));
            repaint(
                &RecoveryState {
                    busy: true,
                    ..Default::default()
                },
                &widgets,
            );
            assert!(retry.is_visible());
            assert!(!retry.is_sensitive());

            clear_succession_sweep();
            // ⚠ Hand the shared statics back the way this file's sibling test
            // does, and for its stated reason: this process has ONE GTK thread,
            // so `LANDED_SWEEP` and the offline gate's `thread_local!` are shared
            // with every other widget test. Leaving the gate `"connected"` makes
            // `walk.rs`'s I6 pass VACUOUSLY on every surface — which its own
            // vacuity guard turns into five reds rather than a silent pass
            // (measured here 2026-09-01: this test's first cut left it connected
            // and took the whole `walk::` suite down with it).
            crate::offline_gate::set_connection_state("disconnected");
        });
    }
}
