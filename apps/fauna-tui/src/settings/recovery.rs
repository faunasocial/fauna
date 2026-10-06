//! The Settings Account sub-page's **Recovery kit** section
//! (`docs/goal/ui/settings.md` § Recovery kit) — the RecoveryKey's Settings
//! home, placed immediately after Identity export as its sibling root-secret
//! affordance.
//!
//! Everything with a decision in it lives in shared Rust
//! (`fauna_client_recovery`): the four states, which actions each enables, and
//! the status copy. This module is the tui *shell* over that — a paint of what
//! [`fauna_client_recovery::RecoveryKitStatus`] already decided, plus the
//! view state a returned kit needs. Nothing here re-derives a chain arm or a
//! button's enablement; if a future session finds itself writing `match
//! status` to decide whether a button is live, the projection is the place to
//! change (priority #2).
//!
//! **The section holds no secret longer than the screen shows it.** A minted
//! kit is displayed once (§ The RecoveryKey — *Custody*: offline-only, never
//! stored on any device) and is dropped when the user dismisses it or leaves
//! the page. There is deliberately no "show it again" path, and there can
//! never be one.

use fauna_client_recovery::RecoveryKitStatus;
use fauna_core::secret::SecretString;
use fauna_i18n::strings::{common, settings as t};
use fauna_ui_ids as ids;

use super::{Action, SettingsField, SettingsState};
use crate::element::{Element, Field, Gesture};

/// A kit a ceremony just minted, held only while the screen shows it.
///
/// The secret is a [`SecretString`] so dropping it zeroizes rather than merely
/// shortens — the same reasoning as the identity secret on the page above.
///
/// A failed escrow half needs no field here: `create_kit` returns the kit
/// regardless (the registration has landed, so the secret shown is the only
/// copy in existence), and the re-read that follows every ceremony renders the
/// gap in `recovery-kit-status` as `RegisteredNoEscrow` — the same signal, from
/// the nest rather than from a local flag that could disagree with it.
#[derive(Default)]
pub struct MintedKit {
    /// The 64-hex recovery secret, shown once.
    pub secret_hex: SecretString,
    /// The `fauna://recovery` URI for this kit — the richest encoding of the
    /// same artifact, carrying the actor id **and** the host-qualified handle
    /// this surface truthfully knows (`identity-succession.md` § The
    /// RecoveryKey — *Kit payload*).
    ///
    /// Held rather than rebuilt because both the QR and
    /// [`Action::RecoveryCopySecret`](super::Action::RecoveryCopySecret) render
    /// it, and the handle it embeds is read at mint time. A [`SecretString`]
    /// for [`Self::secret_hex`]'s reason: it *contains* the secret, so dropping
    /// it must zeroize.
    pub uri: SecretString,
    /// The QR of that URI, built at mint time.
    pub qr: Option<fauna_core::qr_matrix::QrMatrix>,
}

impl MintedKit {
    /// What `recovery-kit-secret-copy-btn` puts on the clipboard: the
    /// `fauna://recovery` URI, **not** the bare hex the screen displays.
    ///
    /// Its own accessor so the choice is one greppable place with one reason
    /// attached, and so a test can pin the payload without capturing the OSC-52
    /// write itself. See [`super::Action::RecoveryCopySecret`].
    pub fn copy_payload(&self) -> &str {
        self.uri.as_str()
    }
}

impl core::fmt::Debug for MintedKit {
    /// Redacted, for [`fauna_client_recovery::RecoveryKit`]'s reason: a kit in
    /// a log line or panic message is exactly the leak offline-only custody
    /// exists to prevent.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MintedKit")
            .field("secret_hex", &"<redacted>")
            .finish()
    }
}

/// The section's view state.
#[derive(Default, Debug)]
pub struct RecoveryState {
    /// The projected status, or `None` until the read resolves. The section
    /// renders its container and heading either way — a status line that
    /// appears late is better than a section that pops in.
    pub status: Option<RecoveryKitStatus>,
    /// A kit the user is being shown right now, if a ceremony just minted one.
    pub minted: Option<MintedKit>,
    /// Set while a ceremony is in flight, so a second click cannot start a
    /// second registration against the same chain head.
    pub busy: bool,
    /// `recovery-entry-phrase-field` — the kit the user holds, for the
    /// ceremonies that take one (`settings.md` § Recovery kit → *Kit-in-hand
    /// entry*, user-approved 2026-08-01: the onboarding screen's own input,
    /// reused inline here because a pasted phrase is the same artifact as a
    /// displayed one).
    ///
    /// A [`SecretString`] for [`MintedKit::secret_hex`]'s reason — this buffer
    /// holds a RecoveryKey, the credential that outranks the identity seed, so
    /// dropping it must zeroize rather than merely shorten. It is never parsed
    /// here: `fauna_client_recovery::parse_kit` owns the one grammar all seven
    /// apps read (priority #4).
    pub phrase_input: SecretString,
    /// `identity-stolen-confirm-field` — the type-to-confirm buffer that gates
    /// `identity-stolen-button` (`settings.md` § Recovery kit: "the same
    /// type-to-confirm idiom as `settings-delete-account-button`").
    ///
    /// A plain `String`, unlike [`Self::phrase_input`]: it holds the literal
    /// word [`STOLEN_CONFIRM_WORD`] and never a secret.
    pub stolen_confirm_input: String,
    /// The dead generations the account runtime last read
    /// (`fauna_account_plane::generation_let_go::dead_generations`) — what the
    /// let-go renders from, and only while non-empty (`settings.md` § Recovery
    /// kit; `account-data-taxonomy.md` clause (3)(j)). Empty until the read
    /// lands, and whenever there is no runtime.
    pub dead_generations: Vec<fauna_sync_engine::generation_let_go::DeadGeneration>,
    /// `recovery-kit-let-go-confirm-field` — the type-to-confirm buffer that
    /// gates `recovery-kit-let-go-button`, the stolen gate's idiom: the data
    /// cannot be brought back. Holds the literal [`LET_GO_CONFIRM_WORD`].
    pub let_go_confirm_input: String,
}

/// The let-go's confirm word — shared Rust's one constant, so all seven apps
/// gate on the same text.
pub use fauna_sync_engine::generation_let_go::LET_GO_CONFIRM_WORD;

/// The succession twin of the delete gate's `"DELETE"`, re-exported so this
/// module's ~10 call sites keep their `recovery::STOLEN_CONFIRM_WORD` idiom.
/// It moved to shared Rust 2026-09-01 when linux built its own stolen gate —
/// the owner's doc says why it is a shared constant rather than a per-app one.
pub use fauna_client_recovery::ceremony::STOLEN_CONFIRM_WORD;

impl RecoveryState {
    /// Drop everything a session should not outlive — called from the page's
    /// reset and from `clear_session`. The minted kit goes first: it is the
    /// only secret this state ever holds.
    pub fn reset(&mut self) {
        self.minted = None;
        self.status = None;
        self.busy = false;
        // The pasted kit is a secret too — it outranks the seed — so it dies
        // with the session exactly as the minted one does.
        self.phrase_input = SecretString::default();
        // An armed confirm gate must not survive a page leave or a session
        // change: the delete gate clears for the same reason, so that returning
        // to the page never finds an irreversible action already unlocked.
        self.stolen_confirm_input = String::new();
        self.dead_generations = Vec::new();
        self.let_go_confirm_input = String::new();
    }
}

/// What the ephemeral kit-side review pass renders from.
///
/// Gathered by the caller as one named thing rather than threaded as three
/// loose arguments, because all three live on `App` (not in `SettingsState`)
/// and are meaningless apart: the roster is what to ask about, the manager is
/// what names the people in it, and the flag is whether to ask at all.
pub(super) struct MemberReviewPass<'a> {
    /// The open reviews, one entry per person — already collapsed by
    /// `SuccessionLedger::open_member_reviews`, which is what
    /// makes this one row per *person* and not one per item.
    pub reviews: &'a [fauna_core::data::MemberReview],
    /// Resolves a person to the handle they are seated under, for the row
    /// label. `None` pre-auth or before conversations come up — the rows still
    /// render, under the "no longer in any of your groups" wording, because a
    /// row nobody can name is a row nobody can close.
    pub manager: Option<&'a std::sync::Arc<fauna_conversations::ConversationsManager>>,
    /// The user pressed *Review The Rest Later* this run.
    pub deferred: bool,
    /// How many email filter rules the succession carried across that the owner
    /// has yet to keep or remove (`App::filter_marks`).
    ///
    /// A **count**, not a roster: unlike the member pass, this surface does not
    /// adjudicate — it routes. The rules are already listed, named and
    /// deletable on Settings ▸ Privacy, so re-rendering them here would be a
    /// second surface for one decision, and the per-action succession ruling's
    /// remedy (*"the successor's own filter list"*) is precisely the list that
    /// needed pointing at (`succession-aftermath.md` § Adjudicating what the
    /// aftermath carries across).
    pub inherited_filters: usize,
}

#[cfg(test)]
impl MemberReviewPass<'_> {
    /// Nothing to ask about — the state of every session that never ran a
    /// recovery ceremony, and the right default for a test about anything else
    /// on this page.
    pub(super) fn empty() -> Self {
        Self {
            reviews: &[],
            manager: None,
            deferred: false,
            inherited_filters: 0,
        }
    }
}

/// The let-go of a dead generation, beside the four actions
/// (`settings.md` § Recovery kit; `account-data-taxonomy.md` § The generation
/// machinery → *Fleet-scope reclamation*, clause (3)(j)): the line, its
/// type-to-confirm field and its button — all three ONLY while the runtime's
/// dead read answers non-empty, the veto's shape, which is why ui.yaml has
/// them in `optional_elements`. The line's copy is the shared projection's
/// (`generation_let_go::unreadable_status`); the button arms on the confirm
/// word alone, and [`Action::RecoveryLetGo`] re-checks it when it fires.
pub(super) fn let_go_elements(recovery: &RecoveryState) -> Vec<Element> {
    let Some(line) = fauna_sync_engine::generation_let_go::unreadable_status(
        &recovery.dead_generations,
        fauna_core::format::format_unix_local_date_ms,
    ) else {
        return Vec::new();
    };
    vec![
        Element::label(
            ids::RECOVERY_KIT_UNREADABLE_STATUS,
            line.resolve(fauna_i18n::strings::lookup),
        ),
        Element::input(
            ids::RECOVERY_KIT_LET_GO_CONFIRM_FIELD,
            recovery.let_go_confirm_input.clone(),
            Field::Settings(SettingsField::LetGoConfirmInput),
        )
        .labelled(t::recovery_kit::LET_GO_CONFIRM_PLACEHOLDER),
        Element::gesture_button(
            ids::RECOVERY_KIT_LET_GO_BUTTON,
            t::recovery_kit::LET_GO,
            !recovery.busy && recovery.let_go_confirm_input == LET_GO_CONFIRM_WORD,
            Gesture::Settings(Action::RecoveryLetGo),
        ),
    ]
}

/// Render the section (`settings.md` § Recovery kit).
///
/// The element list is flat, in the ratified order: container, status line,
/// then the actions whose enablement the shared projection decides. The veto
/// button renders **only** while a window is open, which is why `ui.yaml` has
/// it in `optional_elements` rather than `elements`.
pub(super) fn recovery_elements(
    state: &SettingsState,
    sweep: Option<&crate::settings::SweepStatus>,
    aftermath: &crate::app::AftermathProgress,
    pass: MemberReviewPass<'_>,
) -> Vec<Element> {
    let recovery = &state.recovery;
    let mut els = vec![Element::label(
        ids::RECOVERY_KIT_SECTION,
        t::recovery_kit::TITLE,
    )];

    // The last succession's group sweep, if one ran on this app run — the
    // ceremony's own outcome, rendered in the flow that ran it
    // (`identity-succession.md` § Propagation → *MLS groups*, which rules this
    // explicitly NOT a critical-alerts banner: a healthy group has members the
    // ceremony did not add, so a standing alarm would accuse the user's own
    // correspondents after every *successful* succession).
    //
    // It rides at the TOP of the section because it is the freshest thing here
    // and describes what the buttons below just did. The two lines carry their
    // own ids (`recovery-kit-sweep-status` / `recovery-kit-sweep-unvouched-status`,
    // user-approved 2026-09-25) so a journey can witness what the user is TOLD,
    // not only the state behind it — they are still prose, not affordances.
    // ⚠ The two lines below are deliberately SEPARATE: eviction of the stolen
    // identity and the roster it cannot vouch for are two facts, and there is no
    // combined "you are safe" verdict to render (the same reason `SweepReport`
    // refuses to expose one — see its `NOTE` in `group_sweep.rs`).
    els.extend(sweep_elements(sweep));

    // The ephemeral review pass, directly under the sweep line that reports the
    // roster it works through — this IS that line's affordance, which is why
    // `sweep_unattested` no longer redirects anywhere.
    els.extend(member_review_elements(sweep, &pass));

    // The aftermath's legs, directly under the sweep and above the kit status
    // for the same reason the sweep sits at the top: they are the freshest
    // thing here and describe the *aftermath* of the ceremony the lines below
    // offer. One line per aftermath leg, in the order the sequence runs them,
    // so a successor reads their progress top-down.
    els.extend(backup_regrant_elements(aftermath.backup_regrant.as_ref()));
    els.extend(mls_reseal_elements(aftermath.mls_reseal.as_ref()));
    els.extend(grant_remint_elements(aftermath.grant_remint.as_ref()));
    els.extend(corpus_reseal_elements(aftermath.corpus_reseal.as_ref()));
    // Leg 7 before leg 6 for the same reason the task runs them in that order:
    // the burn is the only leg that takes something away, so every restoring
    // leg reports above it.
    els.extend(drafts_reseal_elements(aftermath.drafts_reseal.as_ref()));
    els.extend(mail_burn_elements(aftermath.mail_burn.as_ref()));
    els.extend(inherited_filters_elements(pass.inherited_filters));

    // The status line. Its text comes from the shared projection as
    // `LocalizedText`; tui resolves the key through the generated table rather
    // than composing prose, so all seven apps say the same thing.
    // The line paints in every state, including the un-read one. All four
    // ceremonies below gate on `status.is_some_and(...)`, so before the status
    // resolves they are *all* dead at once — the section would otherwise show
    // four DIM buttons under a bare "Recovery Kit" heading with nothing saying
    // why (`docs/goal/ui/README.md` § Copy comprehensibility, rule 5).
    els.push(Element::label(
        ids::RECOVERY_KIT_STATUS,
        match &recovery.status {
            Some(status) => status
                .status_line(now_secs())
                .resolve(fauna_i18n::strings::lookup),
            None => t::recovery_kit::STATUS_LOADING.to_string(),
        },
    ));

    // The four ceremonies. `enabled` is the projection's answer, never a local
    // `match` — and a ceremony in flight disables every one of them, so a
    // double click cannot race two registrations onto one chain head.
    let live = |allowed: bool| allowed && !recovery.busy;
    let status = recovery.status.as_ref();
    els.push(Element::gesture_button(
        ids::RECOVERY_KIT_CREATE_BUTTON,
        t::recovery_kit::CREATE,
        live(status.is_some_and(RecoveryKitStatus::allows_create)),
        Gesture::Settings(Action::RecoveryCreateKit),
    ));
    els.push(Element::gesture_button(
        ids::RECOVERY_KIT_REPLACE_BUTTON,
        t::recovery_kit::REPLACE,
        live(status.is_some_and(RecoveryKitStatus::allows_replace)),
        Gesture::Settings(Action::RecoveryReplaceKit),
    ));
    els.push(Element::gesture_button(
        ids::RECOVERY_KIT_LOST_BUTTON,
        t::recovery_kit::LOST,
        live(status.is_some_and(RecoveryKitStatus::allows_lost)),
        Gesture::Settings(Action::RecoveryLostKit),
    ));
    // The no-escrow repair: restore phrase recovery with the kit in hand,
    // WITHOUT retiring it (`identity-succession.md` § Seed escrow → *Lifecycle
    // on the nest* — the landed re-put is user-prompted by construction, and
    // its ceremony is the head-checked re-put, never `create_kit`, which would
    // retire the kit the user just received to fix a blob). Rendered ONLY in
    // `RegisteredNoEscrow`, the veto button's shape: an affordance for a
    // state, not a standing button — which is why ui.yaml has it in
    // `optional_elements`.
    if status.is_some_and(RecoveryKitStatus::allows_escrow_reseal) {
        els.push(Element::gesture_button(
            ids::RECOVERY_KIT_ESCROW_RESEAL_BUTTON,
            t::recovery_kit::ESCROW_RESEAL,
            !recovery.busy,
            Gesture::Settings(Action::RecoveryResealEscrow),
        ));
    }
    // The group-sweep retry — the same shape one row up, for the same reason:
    // an affordance for a state. It renders directly under the sweep chrome
    // whose two degraded arms now name it by label.
    //
    // ⚠ **The gate is UNFINISHED WORK, not "this device can retry."** Whether
    // the retry can actually run here depends on the retired identity's own MLS
    // store, which survives only on the device the ceremony ran on — and that
    // is deliberately NOT consulted for the render. A device without it answers
    // with the member-side remedy when pressed
    // ([`super::Action::RecoverySweepRetry`]); gating the render on the store
    // instead would leave `sweep_none`/`sweep_partial` pointing at a button
    // that is not on screen, which is the exact class of dishonesty removed from that copy in the first place. The gate itself is the shared
    // `owes_work` — the same projection that selects the copy naming this
    // button, so the two cannot disagree.
    if sweep.is_some_and(crate::settings::SweepStatus::owes_work) {
        els.push(Element::gesture_button(
            ids::RECOVERY_KIT_SWEEP_RETRY_BUTTON,
            t::recovery_kit::SWEEP_RETRY,
            !recovery.busy,
            Gesture::Settings(Action::RecoverySweepRetry),
        ));
    }
    // Succession is irreversible and re-points the whole account, so it is the
    // one action gated behind a type-to-confirm field as well as its status
    // (`settings.md` § Recovery kit — "the same type-to-confirm idiom as
    // `settings-delete-account-button`"). The warning rides as ID-less chrome:
    // it is copy, not an element any test drives, and inventing an ID for prose
    // would put a second name on the page for the same affordance.
    els.push(Element::chrome(t::recovery_kit::STOLEN_WARNING));
    els.push(
        Element::input(
            ids::IDENTITY_STOLEN_CONFIRM_FIELD,
            recovery.stolen_confirm_input.clone(),
            Field::Settings(SettingsField::StolenConfirmInput),
        )
        .labelled(t::recovery_kit::STOLEN_CONFIRM_PLACEHOLDER),
    );
    // ⚠ Deliberately `is_none_or`, not `is_some_and` — armed while the status
    // is UNREAD or the chain read failed, matching windows' `stolenArmed` (which carries no status check at
    // all). `allows_stolen` is unconditionally true once resolved, and the
    // ceremony's authorization is the KIT, never the status:
    // `succession_succeed_with_held_kit` deliberately does no status re-read
    // of its own (`recovery.rs`: "the kit is the whole authorization, and a
    // chain read here would only add a round trip a locked-out owner can
    // fail on"). The confirm word is the only real gate.
    els.push(Element::gesture_button(
        ids::IDENTITY_STOLEN_BUTTON,
        t::recovery_kit::STOLEN,
        !recovery.busy
            && status.is_none_or(RecoveryKitStatus::allows_stolen)
            && recovery.stolen_confirm_input == STOLEN_CONFIRM_WORD,
        Gesture::Settings(Action::RecoveryStolen),
    ));
    // Rendered ONLY while a window is open — which is why `ui.yaml` has it in
    // `optional_elements` rather than `elements`.
    if status.is_some_and(RecoveryKitStatus::allows_veto) {
        els.push(Element::gesture_button(
            ids::RECOVERY_PENDING_VETO_BUTTON,
            t::recovery_kit::VETO,
            !recovery.busy,
            Gesture::Settings(Action::RecoveryVeto),
        ));
    }
    els.extend(let_go_elements(recovery));

    // The kit-in-hand entry, inline in the section (`settings.md` § Recovery kit
    // → *Kit-in-hand entry*, user-approved 2026-08-01). It is the onboarding
    // `recovery_entry` screen's OWN id, reused rather than twinned: a pasted
    // phrase is the same artifact as a displayed one, by the same argument that
    // put the display trio here (priority #3). One id on two pages, by design.
    //
    // Rendered while a wired ceremony can consume it — which is what makes
    // it `optional_elements` in ui.yaml rather than `elements`. FOUR ceremonies
    // read it now: replace, stolen, veto, and the no-escrow re-seal (whose
    // state always has `allows_replace` true too, so the condition below
    // already covers it). `allows_stolen` answers `true` in every state
    // (`settings.md` § Recovery kit ratifies "stolen (any)" — theft does not
    // wait for the account to be tidy), so the field is effectively always up
    // once wired. ⚠ Also up with the status UNREAD or failed — `is_none_or`,
    // not `is_some_and` — same reasoning as `IDENTITY_STOLEN_BUTTON` above:
    // an unread status must not take the field away from the ceremony that
    // needs it most.
    if status.is_none_or(|s| s.allows_replace() || s.allows_veto() || s.allows_stolen()) {
        els.push(
            Element::input(
                ids::RECOVERY_ENTRY_PHRASE_FIELD,
                recovery.phrase_input.as_str().to_string(),
                Field::Settings(SettingsField::RecoveryPhraseInput),
            )
            .labelled(t::recovery_kit::KIT_PHRASE_PLACEHOLDER),
        );
    }

    // A freshly minted kit, rendered through the **onboarding screen's** IDs
    // rather than settings-specific twins: it is the same artifact shown the
    // same way (`settings.md` § Recovery kit — *Displaying a returned kit*,
    // priority #3).
    if let Some(minted) = &recovery.minted {
        els.push(Element::label(
            ids::RECOVERY_KIT_SECRET_DISPLAY,
            minted.secret_hex.as_str().to_string(),
        ));
        els.push(Element::gesture_button(
            ids::RECOVERY_KIT_SECRET_COPY_BTN,
            common::COPY,
            true,
            Gesture::Settings(Action::RecoveryCopySecret),
        ));
        if let Some(matrix) = &minted.qr {
            // Pinned dark-on-light for the identity-export QR's reason — a
            // theme-inverted QR does not scan.
            els.push(
                Element::label(ids::RECOVERY_KIT_QR, super::account::render_qr(matrix))
                    .colors([0, 0, 0], [255, 255, 255]),
            );
        }
    }

    els
}

/// The aftermath's `NestBackupKey` leg — the successor's backups restarting, or
/// nothing when the leg owes nothing (`succession-aftermath.md` § Re-key scope,
/// the `NestBackupKey` row).
///
/// **Every decision here belongs to the shared projection**
/// ([`fauna_client_config::BackupRegrantProgress::status_line`]) — whether a
/// line renders at all, and which one; this resolves the key, so the line reads
/// identically on all seven apps (priority #2). It renders with an ID: it is a
/// live progress state a test must be able to observe.
fn backup_regrant_elements(
    regrant: Option<&fauna_client_config::BackupRegrantProgress>,
) -> Vec<Element> {
    let Some(line) = regrant.and_then(fauna_client_config::BackupRegrantProgress::status_line)
    else {
        return Vec::new();
    };
    vec![Element::label(
        ids::RECOVERY_KIT_BACKUP_REGRANT_STATUS,
        line.resolve(fauna_i18n::strings::lookup),
    )]
}

/// The aftermath's `__mls` leg — the successor's conversations being unlocked,
/// or nothing when the leg owes nothing (`succession-aftermath.md` § Re-key
/// scope, the `BackupKey` corpus row).
///
/// Same division of labour as [`backup_regrant_elements`]: the shared projection
/// ([`fauna_client_mls_sync::ReplicaResealProgress::status_line`]) decides
/// whether a line renders and which one; this resolves the key. The arm worth
/// not flattening here is the **partly-owed** one — a pass that unlocked some
/// conversations and left others sealed to an identity this device cannot open
/// (a twice-succeeded chain). It is progress *and* unfinished; rendering the
/// done line there would tell the user every conversation is back when some
/// are not.
fn mls_reseal_elements(
    reseal: Option<&fauna_client_mls_sync::ReplicaResealProgress>,
) -> Vec<Element> {
    let Some(line) = reseal.and_then(fauna_client_mls_sync::ReplicaResealProgress::status_line)
    else {
        return Vec::new();
    };
    vec![Element::label(
        ids::RECOVERY_KIT_MLS_RESEAL_STATUS,
        line.resolve(fauna_i18n::strings::lookup),
    )]
}

/// The aftermath's capability-grant leg — the trust the owner had given to
/// services, restored under the successor, or nothing when the leg owes nothing
/// (`succession-aftermath.md` § Re-key scope, the capability-grants row).
///
/// Same division of labour as [`backup_regrant_elements`]: the shared projection
/// ([`fauna_client_capabilities::GrantRemintProgress::status_line`]) decides
/// whether a line renders and which one; this resolves the key.
///
/// ⚠ This line reports only whether the **pass ran**. The adjudication half —
/// which of the restored grants the owner still has to review — deliberately
/// does not render here: it is a per-grant mark on the Nests page, because the
/// verdict is about one grant at a time and a count in Settings would be a
/// number with nothing to press. The done line says so in words.
fn grant_remint_elements(
    remint: Option<&fauna_client_capabilities::GrantRemintProgress>,
) -> Vec<Element> {
    let Some(line) = remint.and_then(fauna_client_capabilities::GrantRemintProgress::status_line)
    else {
        return Vec::new();
    };
    vec![Element::label(
        ids::RECOVERY_KIT_GRANT_REMINT_STATUS,
        line.resolve(fauna_i18n::strings::lookup),
    )]
}

/// The aftermath's **file corpus** leg — the user's own files, photos and file
/// sets moved out from under the retired identity's `BackupKey`, or nothing when
/// the leg owes nothing (`succession-aftermath.md` § Re-key scope, the
/// `BackupKey` corpus row).
///
/// Same division of labour as [`backup_regrant_elements`]: the shared projection
/// ([`fauna_client_sync::agent::CorpusResealProgress::status_line`]) decides
/// whether a line renders and which one; this resolves the key.
///
/// ⚠ Unlike its four siblings, the work behind this line does not run in this
/// process — it runs in the per-user `fauna-sync-agent`'s catch-up pass, and the
/// app folds it from that agent's `ListEngines` roster
/// (`crate::sync_agent`'s status poll). So the line can move while Settings is
/// open, and it can be `None` for a while after sign-in with nothing wrong: the
/// agent simply has not recorded a pass yet.
fn corpus_reseal_elements(
    progress: Option<&fauna_client_sync::agent::CorpusResealProgress>,
) -> Vec<Element> {
    let Some(line) = progress.and_then(fauna_client_sync::agent::CorpusResealProgress::status_line)
    else {
        return Vec::new();
    };
    vec![Element::label(
        ids::RECOVERY_KIT_CORPUS_RESEAL_STATUS,
        line.resolve(fauna_i18n::strings::lookup),
    )]
}

/// The aftermath's **mail** leg — every pre-succession credential revoked and
/// the MSEK rotated out from under the retired identity's seed holder, or
/// nothing when no mail plane owed a burn (`succession-aftermath.md` § Re-key
/// scope, the MSEK row; mechanics in `mail-credentials.md` § Rotation and
/// recovery → *Succession*).
///
/// Same division of labour as its five siblings: the shared projection
/// ([`fauna_client_mail_settings::MailBurnProgress::status_line`]) decides
/// whether a line renders and which one; this resolves the key.
///
/// ⚠ **The one leg whose done line is not good news.** The others restore
/// something; this one deliberately breaks every configured mail app, because
/// whoever held the retired identity also held those app passwords. The line has
/// to carry that or the user reads the resulting IMAP failures as a broken nest
/// and goes hunting for the old password — which is exactly the credential that
/// no longer works and must never be re-entered.
fn mail_burn_elements(
    progress: Option<&fauna_client_mail_settings::MailBurnProgress>,
) -> Vec<Element> {
    let Some(line) = progress.and_then(fauna_client_mail_settings::MailBurnProgress::status_line)
    else {
        return Vec::new();
    };
    vec![Element::label(
        ids::RECOVERY_KIT_MAIL_BURN_STATUS,
        line.resolve(fauna_i18n::strings::lookup),
    )]
}

/// The aftermath's **drafts** leg — every `__drafts` rail re-sealed under the
/// successor's own key, or nothing when the leg owes nothing
/// (`succession-aftermath.md` § Re-key scope, the `BackupKey` corpus row, which
/// names `__drafts` explicitly).
///
/// Same division of labour as [`backup_regrant_elements`]: the shared projection
/// ([`fauna_client_drafts::DraftsResealProgress::status_line`]) decides whether
/// a line renders and which one; this resolves the key.
///
/// ⚠ Multi-unit like the `__mls` leg, so **partly-owed is an ordinary
/// outcome**, not an edge case: a device holding only one of two retired seeds
/// recovers the rails it can and genuinely still owes the rest. That arm gets
/// its own line rather than the done one — collapsing it would tell a user
/// every draft is back while some are still sealed.
fn drafts_reseal_elements(
    progress: Option<&fauna_client_drafts::DraftsResealProgress>,
) -> Vec<Element> {
    let Some(line) = progress.and_then(fauna_client_drafts::DraftsResealProgress::status_line)
    else {
        return Vec::new();
    };
    vec![Element::label(
        ids::RECOVERY_KIT_DRAFTS_RESEAL_STATUS,
        line.resolve(fauna_i18n::strings::lookup),
    )]
}

/// The line that **routes the successor to their filter list** — the whole
/// remedy found missing.
///
/// The per-action succession disposition moves `Discard` and `FileInto` rules
/// intact and `Reject` disarmed, and justified that remainder with *"the
/// successor's own filter list is its remedy"*. Nothing routed anyone there, so
/// a thief-armed suppression or diversion rule survived the ceremony
/// unannounced. This is that route (`succession-aftermath.md` § Adjudicating
/// what the aftermath carries across, the 2026-08-14 paragraphs).
///
/// ⚠ **Renders only while rules are still un-adjudicated, and disappears by
/// itself.** It is not a leg status like its six siblings above — there is no
/// "done" line to show, because the honest done state of a review backlog is an
/// absent line. That is also what keeps the Recovery Kit section from carrying
/// permanent clutter for every account that ever ran a recovery.
fn inherited_filters_elements(open: usize) -> Vec<Element> {
    if open == 0 {
        return Vec::new();
    }
    vec![Element::label(
        ids::RECOVERY_KIT_INHERITED_FILTERS_STATUS,
        t::recovery_kit::inherited_filters(&open.to_string()),
    )]
}

/// Unix seconds — the countdown's clock. The projection itself is pure and
/// takes `now` as a parameter (`fauna_client_recovery::status`); reading the
/// clock is the shell's job, exactly as in `events`.
fn now_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs_or_zero()
}

/// Resolve the shared projection's `LocalizedText` into the tui's own string.
///
/// The generated i18n layer has no runtime key→string resolver (only
/// compile-time constants), so the key is matched here — the same shape every
/// other app's renderer takes. A key this build does not know falls back to
/// the never-created warning, which is the safe direction: it over-warns
/// rather than reassuring a user whose kit may not exist.
/// The post-succession sweep's lines, or nothing when no succession ran.
///
/// **The selection is the shared projection's**, `SweepView::copy`
/// (`settings.md` § Recovery kit → *The sweep's own lines*): which arm says
/// what, the `groups == 0` suppression and the two-facts split all live in
/// `fauna_client_recovery::ceremony` — lifted there 2026-08-27 from this
/// function, which had been the only app to select the line while web painted
/// nothing and apple/windows were about to re-derive it. tui resolves keys,
/// exactly as the aftermath legs below. `Rendered` because this app paints
/// `recovery-kit-sweep-retry-button` on every owing arm
/// ([`recovery_elements`]'s gate, `SweepStatus::owes_work`) — the copy names
/// the button by label, so the two must agree, and the projection is what makes
/// them agree.
///
/// Each line carries its own id and is absent — id included — whenever the
/// projection returns `None` for it (no groups at all, or an empty roster), so
/// a driver reading `recovery-kit-sweep-unvouched-status` can never find it
/// standing empty beside an eviction line.
///
/// Split out of [`recovery_elements`] so the wording stays unit-testable
/// without building a whole settings state.
fn sweep_elements(sweep: Option<&crate::settings::SweepStatus>) -> Vec<Element> {
    use fauna_client_recovery::ceremony::SweepRetryAffordance;
    let Some(sweep) = sweep else {
        return Vec::new();
    };
    let copy = sweep.render_copy(SweepRetryAffordance::Rendered);
    [
        (ids::RECOVERY_KIT_SWEEP_STATUS, copy.outcome),
        (ids::RECOVERY_KIT_SWEEP_UNVOUCHED_STATUS, copy.unattested),
    ]
    .into_iter()
    .filter_map(|(id, line)| {
        line.map(|line| Element::label(id, line.resolve(fauna_i18n::strings::lookup)))
    })
    .collect()
}

/// The **ephemeral** kit-side review pass — one row per still-open person, each
/// with *Keep* and *Remove*, plus *Review The Rest Later*
/// (`identity-succession.md` § Propagation → *MLS groups*, ruling 1).
///
/// ⚠ **`sweep.is_some()` is the ruling, not a convenience.** This surface is
/// *"shown once per recovery, in the ceremony flow that produced it"* — so it
/// renders only where that ceremony ran, and its absence is what makes the
/// permanent view (`member_review`) the one place a deferred backlog lives.
/// Dropping this gate would paint the pass on every ordinary session that
/// happens to carry open items, which is precisely the standing clutter the
/// two-surface split was ratified to avoid. `succession_sweep` is the right
/// witness because it is the one piece of ceremony state deliberately kept
/// across the account switch (`App::succession_sweep`), i.e. it is live exactly
/// in the successor's first session and nowhere else.
///
/// ⚠ **Remove is rendered here even though the member chip's Remove is not.**
/// On a chip the row *is* the removal affordance; here the person is rendered
/// outside any one group, so there is nothing to route to — which is the whole
/// reason [`fauna_conversations::ConversationsManager::evict_person_everywhere`]
/// exists. The verdict it earns is **derived**, never chosen: see the op.
fn member_review_elements(
    sweep: Option<&crate::settings::SweepStatus>,
    pass: &MemberReviewPass<'_>,
) -> Vec<Element> {
    if sweep.is_none() || pass.deferred || pass.reviews.is_empty() {
        return Vec::new();
    }
    let mut els = vec![Element::chrome(t::recovery_kit::REVIEW_INTRO)];
    // ⚠ The rows themselves are built by the PERMANENT view's module, not here.
    // The two surfaces render one id family (`settings.md` § Recovery kit →
    // *The ephemeral member-review pass*: "the same four IDs serve the permanent
    // view … one family, two surfaces"), so a second copy of this loop would be
    // two places for a row's wording, scoping or gesture to drift — and the
    // whole point of reusing the ids is that a driver reading
    // `member-review-row[i]` reads the same thing on either page. What is local
    // to this surface is the GATE above and the defer button below; the row is
    // not.
    els.extend(super::member_review::review_rows(
        pass.reviews,
        pass.manager,
    ));
    // Renders only alongside rows, because it is the *rest* it defers: with an
    // empty pass there is nothing to postpone and the button would be a control
    // whose press changes nothing the user can see.
    els.push(Element::gesture_button(
        ids::MEMBER_REVIEW_DEFER_BUTTON,
        t::recovery_kit::REVIEW_DEFER,
        true,
        Gesture::Settings(Action::MemberReviewDefer),
    ));
    els
}

/// The same sweep facts [`sweep_elements`] paints, as machine-readable state.
///
/// **Why this exists beside the two lines' own ids.** The lines say what the
/// user is TOLD, as resolved prose; this says what HAPPENED, as counts and
/// per-group outcomes a journey can reason over — whether the old leaf left
/// every group, how many members the sweep cannot vouch for — without parsing
/// a sentence. It is also the witness on apps that paint the lines but have not
/// tagged them yet. The lines were ID-less chrome until 2026-09-25, when this
/// was the only witness at all; it stays as their machine twin.
///
/// ⚠ **The two facts stay separate here for the same reason they are two
/// lines**: `succession-aftermath.md` § Implementation status today (the
/// sweep-driver bullet) rules that there is deliberately **no** combined "is the
/// user safe" boolean, so a
/// surface cannot round a completion count up to safety. Do not add one — an
/// `unattested` count of 0 is *not* a safety verdict, and a test asserting
/// `old_leaf_removed_everywhere` is asserting eviction of the succeeded
/// credential, which is all a completed sweep ever settles.
pub(crate) fn sweep_state_json(sweep: Option<&crate::settings::SweepStatus>) -> serde_json::Value {
    // The shape is the CROSS-APP contract, so it lives on the shared status
    // rather than here (convention 11) — web publishes the same object from
    // the same function, and the journeys assert its whole shape, not just
    // `status`. Absent stays `Null` rather than an empty object, for the reason
    // the shared fn documents: a journey must be able to tell "no succession
    // ran on this app run" from "one ran and swept nothing".
    crate::settings::SweepStatus::state_json_or_null(sweep)
}

#[cfg(test)]
mod status_line_tests {
    use fauna_client_recovery::RecoveryKitStatus;
    use fauna_client_recovery::replacement::PendingReplacement;

    /// `recovery_elements`' status label resolves `RecoveryKitStatus::status_line`
    /// straight through `LocalizedText::resolve`, with no per-app re-match — the
    /// consumption-surface cleanup this test exists to guard against regressing.
    #[test]
    fn every_status_resolves_through_the_shared_line() {
        let resolve = |s: &RecoveryKitStatus, now: i64| {
            s.status_line(now).resolve(fauna_i18n::strings::lookup)
        };

        assert_eq!(
            resolve(&RecoveryKitStatus::NeverCreated, 0),
            fauna_i18n::strings::settings::recovery_kit::STATUS_NEVER_CREATED
        );
        assert_eq!(
            resolve(&RecoveryKitStatus::Registered, 0),
            fauna_i18n::strings::settings::recovery_kit::STATUS_REGISTERED
        );
        assert_eq!(
            resolve(&RecoveryKitStatus::RegisteredNoEscrow, 0),
            fauna_i18n::strings::settings::recovery_kit::STATUS_REGISTERED_NO_ESCROW
        );

        let pending = RecoveryKitStatus::ReplacementPending(PendingReplacement {
            new_recovery_pubkey_hex: "ab".repeat(32),
            requested_at: 0,
            lands_at: 3 * 86_400,
        });
        assert_eq!(
            resolve(&pending, 0),
            fauna_i18n::strings::settings::recovery_kit::status_replacement_pending("3")
        );
    }
}
