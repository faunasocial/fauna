//! App-launch routing and the `launch_retry` surface.
//!
//! `onboarding.md` § App-launch routing. The shared [`LaunchMachine`] owns the
//! decision — it reads the long-term store, branches on the four documented
//! cases, and runs the silent challenge inline; this module only maps the
//! resulting [`LaunchPhase`] onto what the terminal paints. The routing rules
//! themselves are never re-derived here (priority #2).
//!
//! **`launch_retry` is not an `OnboardingStep`.** It is a launch-flow surface
//! that exists *before* the wizard mounts, so it lives outside
//! `machine.step()` — the same launch↔wizard boundary linux keeps in its glue
//! (`apps/fauna-linux/src/views/launch.rs`). [`LaunchSurface`] is that slot.
//!
//! Sub-states, matching every peer client:
//!
//! - [`LaunchSurface::Launching`] — silent challenge in flight. Registers **no**
//!   ui.yaml element: `onboarding.launch_retry` enumerates only the retry-phase
//!   IDs, and linux/windows/android expose no ID for the spinner either.
//! - [`LaunchSurface::TransientRetry`] — `Offline { transient: true }`. Paints
//!   `launch-transient-error` + `launch-retry-button` + `launch-fallthrough-button`
//!   and the always-shown `launch-retire-button` (`nest-retirement.md` § Layout &
//!   flow — it needs only a cloud token), plus — once a launch-time read finds ≥1
//!   custodied box — the surviving-device `launch-recover-button`
//!   (`box-recovery.md` § Recovery UI (step 4)).
//! - [`LaunchSurface::NeedsUpdate`] — `Offline { transient: false }`, today only
//!   the nest authoritatively reporting `fauna.nest.outdated`. Paints the
//!   localized message in the canonical `error-message` element, **omits**
//!   `launch-retry-button` (retrying an outdated nest is futile), and keeps
//!   `launch-fallthrough-button`. `version-compatibility.md` Dim 4.
//! - [`LaunchSurface::SignInRefused`] — also `Offline { transient: false }`, told
//!   apart by the snapshot's `sign_in_refused` side channel: a nest this app
//!   signed in to before no longer signs the identity in (`onboarding.md`
//!   § App-launch routing — the previously-signed-in row). The honest sentence
//!   in `error-message`, plus `launch-retry-button` (the admin's restore is
//!   off-device; a retry is the way back in) and `launch-fallthrough-button`.
//! - [`LaunchSurface::AccountIndexUnreadable`] — also `Offline { transient: false }`,
//!   but told apart by the snapshot's `account_index_refusal` side channel and
//!   checked first: the saved account index is present and this build cannot
//!   use it. Paints `account-index-refusal-warning`, never a retry, and never
//!   `launch-fallthrough-button` — the nest is not the problem. Only the
//!   malformed verdict adds the start-over pair (`onboarding.md` § App-launch
//!   routing).

use fauna_ui_ids as ids;
use std::path::Path;
use std::sync::Arc;

use fauna_i18n::strings::launch as t;
use fauna_launch_machine::{
    AccountIndexRefusal, LaunchMachine, LaunchPersistence, LaunchPhase, LaunchSnapshot,
    LaunchWizardEntry,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{App, DataMessage, UiMessage};
use crate::wizard::Element;

/// What the unauthenticated screen is showing.
///
/// `Wizard` hands the screen to `App::wizard`; the other three are the launch
/// flow's own surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchSurface {
    /// The onboarding wizard owns the screen.
    Wizard,
    /// Hydrating / silent challenge in flight. Chrome only — no ui.yaml element.
    Launching,
    /// A reachability fault the client cannot reliably classify. Always offer
    /// Retry (`onboarding.md` § App-launch routing — the transient row).
    TransientRetry {
        error: String,
        /// The custodied boxes a launch-time reachable-nest read turned up
        /// (`box-recovery.md` § Recovery UI (step 4) — the surviving-device
        /// entry). Empty until that best-effort read resolves, and **empty is
        /// the gate**: `launch-recover-button` only paints once ≥1 box is known,
        /// so the CTA never offers a recovery the admin cannot actually perform.
        recover_boxes: Vec<String>,
    },
    /// Terminal: the nest told us it is outdated. Non-retryable.
    NeedsUpdate { error: String },
    /// A nest this app had signed in to before no longer signs the identity in
    /// — suspended, or removed; the app cannot tell, by design (`onboarding.md`
    /// § App-launch routing → *the previously-signed-in row*; the snapshot's
    /// `sign_in_refused` side channel). Also `Offline { transient: false }`,
    /// told apart from `NeedsUpdate` by the field and checked before it. The
    /// honest sentence in its own `launch-sign-in-refused-notice` (ui.yaml page
    /// `launch_sign_in_refused`), **with** `launch-retry-button` — the admin's
    /// restore is a button on *their* app, and a retry is the user's way back
    /// in — and `launch-fallthrough-button`. Never the invite wizard: the nest
    /// already holds the account.
    SignInRefused { error: String },
    /// The account is locked (`devices.md` § The locked state; ui.yaml page
    /// `launch_account_locked`). Also `Offline { transient: false }`, told
    /// apart by the snapshot's `locked_until_secs` and checked before the
    /// generic arm. Terminal until that time, so NO retry and NO fallthrough:
    /// the standing `launch-account-locked-notice` and one action, the
    /// stolen-identity ceremony. The machine runs the one refresh at the
    /// unlock time itself, and [`crate::locked::follow_snapshot`] moves the
    /// surface on when it does — nothing here keeps time.
    AccountLocked { locked_until_secs: u64 },
    /// The locked surface's one action: the stolen-identity ceremony from
    /// outside a session (ui.yaml step `identity_stolen_entry`). App-owned,
    /// like [`Self::Unlock`] — the inputs paint `App::locked`'s buffers —
    /// so `App::page_elements` routes it to `crate::locked::entry_elements`.
    /// Carries the unlock time only so Back can repaint the notice.
    IdentityStolenEntry { locked_until_secs: u64 },
    /// The nest's pinned deployment identity changed, or a pinned nest can no
    /// longer prove any identity (`security.md` § Transport trust —
    /// the SSH `known_hosts` model). Non-retryable **by design**: a retry cannot
    /// change the verdict and must never silently re-pin. The two ways out are
    /// the re-trust CTA (`nest-identity-changed-trust-button` →
    /// `LaunchMachine::trust_nest_identity()`) and the fallthrough — the same
    /// uniform surface web renders, since `ui.yaml`'s `launch_identity_changed`
    /// was widened out of `platforms: [web]` to all apps (user-approved,
    /// rule A, 2026-07-13).
    IdentityChanged { error: String },
    /// The saved account index at `fauna/index` is present and this build
    /// cannot use it (`version-compatibility.md` § 5 item 9;
    /// `onboarding.md` § App-launch routing — the row checked before every
    /// other). Terminal and non-retryable: no retry reparses a blob. Without
    /// this arm the registry answered no session account and launch routed to
    /// `IdentityChoice` — offering a new identity to someone whose accounts are
    /// intact behind a blob this build merely cannot parse.
    AccountIndexUnreadable {
        refusal: AccountIndexRefusal,
        /// The malformed verdict's start-over has been pressed and the confirm
        /// — which states the residual — is showing. Always `false` for the
        /// version verdict, which offers no action at all.
        confirming: bool,
        /// Set when the confirm was refused because another instance still
        /// serves an account the reset would erase (`account-scoping.md`
        /// § Concurrent instances → *An erase refuses while a sibling serves
        /// the account*) — painted as `error-message`, with the confirm still
        /// showing, so closing the other window and confirming again is the
        /// whole remedy.
        error: Option<String>,
    },
    /// The sealed headless credential store exists and is locked: passphrase
    /// entry **before** [`start`] may read the long-term store (`tui.md`
    /// § Credential storage; ui.yaml page `tui-unlock`). Elements/title/error
    /// live on `App` (`crate::unlock`), like the `Wizard` arm — the inputs
    /// paint `App::unlock`'s buffers, which this enum cannot reach.
    Unlock,
    /// First headless run (no sealed file yet): choose the passphrase — also
    /// before launch routing, because mid-wizard exits persist durable secrets
    /// (`crate::unlock` module docs). Same App-owned surface as [`Self::Unlock`].
    CreatePassphrase,
    /// The launch-collision chooser (`account-scoping.md` § Concurrent
    /// instances → "the colliding instance's surface"; ui.yaml page
    /// `launch_instance_chooser`). Rendered instead of routing when a plain
    /// launch's would-be account (the store-active one) is already served by
    /// a live `fauna-tui` instance — see [`start_or_offer_chooser`], the ONLY
    /// call site that produces this arm. `switch_account`'s re-entry into
    /// [`start`] never collision-checks: an in-session switch keeps today's
    /// terminal-refusal behavior via `become_session_instance_or_exit` inside
    /// `session::establish`, matching account-scoping.md's own statement that
    /// the chooser is strictly a plain-launch affordance.
    InstanceChooser {
        /// Display label of the account this process collided with (the
        /// reason we are here) — shared `account_display_label`.
        served_label: String,
        /// That account's actor id — what focus-existing re-probes when the
        /// user asks for it (`fauna_client_accounts::resolve_focus_existing`).
        served_actor: String,
        /// `(actor_id, display label)` pairs this process may become, in
        /// registry order — [`crate::account_scope::choosable_accounts`]'s
        /// display-only probe already excludes served accounts.
        choices: Vec<(String, String)>,
        /// Set after a pick loses the race (`account_taken`) — the two ways
        /// this surface can fail *after* it renders. Never painted as
        /// `error-message` alongside a torn-down surface: both failures leave
        /// every other choice still valid, matching linux's `show_error`.
        error: Option<String>,
    },
}

/// A gesture on the launch surface. Unlike [`crate::wizard::Action`] these do
/// not map onto `OnboardingMachine` mutators — they drive the `LaunchMachine`
/// (Retry) or hand the screen to the wizard (Fallthrough / Recover).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchAction {
    /// Re-run the silent challenge against the same `nest_url`.
    Retry,
    /// The malformed account index's "Start over on this device": reveals the
    /// confirm, which states the residual. Performs nothing itself.
    StartOver,
    /// The confirm: perform the documented floor — the same factory reset
    /// every other app runs (`App::reset`: erase every known account's scope,
    /// `AccountRegistry::clear_all`, wipe the credential namespace) — and land
    /// on onboarding. Only reachable from the malformed verdict.
    ConfirmStartOver,
    /// "Use a different nest": `seed_identity(secret)` + wizard at `handle_entry`.
    Fallthrough,
    /// "Recover a lost box": the surviving-device entry into box recovery. Seeds
    /// the machine for recovery and hands the screen to the wizard at
    /// `nest_recovery`. Carries no payload — the box list lives on the surface,
    /// which is the only thing that knows the read resolved.
    Recover,
    /// "Trust this nest and continue": `LaunchMachine::trust_nest_identity()` —
    /// forget the TOFU pin, re-TOFU, re-run the silent challenge. The ONLY path
    /// that forgets a pin (security.md § Transport trust: never a
    /// silent re-pin), and a no-op unless the machine is in `IdentityChanged`.
    Trust,
    /// `launch-account-locked-stolen-button`: open `identity_stolen_entry`.
    OpenStolenEntry,
    /// `recovery-entry-back-button` on `identity_stolen_entry`: back to
    /// wherever the machine now stands (`crate::locked::back`).
    BackToLocked,
    /// `identity-stolen-button` on `identity_stolen_entry`: run the shared
    /// stolen-identity ceremony over an anonymous connection and adopt the
    /// successor (`crate::locked`).
    SubmitStolen,
    /// `launch-instance-chooser-item` — bind THIS process to the carried
    /// actor id and re-enter routing as it (`account-scoping.md`: "no third
    /// process"). Reuses `App::switch_account` — the identical mechanism the
    /// authenticated switcher already uses to re-route onto a different
    /// account, since a chooser pick IS a switch, just one made before any
    /// account was ever established on this process.
    PickInstance(String),
    /// `launch-instance-focus-existing-button` — tui claims no per-account
    /// activation endpoint and raises no window (declared platform absence:
    /// no window manager can raise a terminal — `tui.md` § Declared platform
    /// absences); it prints where the account is already being served and
    /// exits (`account-scoping.md` § Concurrent instances → the raise
    /// channel: "tui neither claims an endpoint... nor raises — its
    /// focus-existing exit prints where the account is served and exits").
    FocusExisting,
}

impl LaunchSurface {
    /// The ordered ui.yaml element list this surface paints — the one source
    /// paint, the automation registry, and the focus ring all read.
    ///
    /// `NeedsUpdate` deliberately omits `launch-retry-button`; its message
    /// renders in `error-message` (see [`Self::error_text`]), not in
    /// `launch-transient-error`, matching web/linux.
    pub fn elements(&self) -> Vec<Element> {
        match self {
            LaunchSurface::Wizard | LaunchSurface::Launching => Vec::new(),
            // App-owned (the inputs paint `App::unlock`'s buffers):
            // `App::page_elements` routes these to `crate::unlock::elements`.
            LaunchSurface::Unlock | LaunchSurface::CreatePassphrase => Vec::new(),
            // App-owned too (`App::locked`'s buffers): `crate::locked::entry_elements`.
            LaunchSurface::IdentityStolenEntry { .. } => Vec::new(),
            // `launch_account_locked` (ui.yaml): the notice in its OWN element,
            // and one action. NO retry and NO fallthrough.
            LaunchSurface::AccountLocked { locked_until_secs } => {
                crate::locked::notice_elements(*locked_until_secs)
            }
            LaunchSurface::TransientRetry {
                error,
                recover_boxes,
            } => {
                let mut out = vec![
                    Element::label(
                        ids::LAUNCH_TRANSIENT_ERROR,
                        fauna_launch_machine::render_text::transient_error_text(error),
                    ),
                    Element::launch_button(
                        ids::LAUNCH_RETRY_BUTTON,
                        t::RETRY_BUTTON,
                        LaunchAction::Retry,
                    ),
                    Element::launch_button(
                        ids::LAUNCH_FALLTHROUGH_BUTTON,
                        t::USE_DIFFERENT_NEST,
                        LaunchAction::Fallthrough,
                    ),
                    // Always shown — unlike the recover CTA below it needs no
                    // custodied seed, only a cloud token (`nest-retirement.md`
                    // § Layout & flow). An unreachable nest is the commonest
                    // reason to want the box gone.
                    Element::gesture_button(
                        ids::LAUNCH_RETIRE_BUTTON,
                        fauna_i18n::strings::onboarding::launch::RETIRE_SERVER,
                        true,
                        crate::element::Gesture::Retire(
                            crate::wizard::nest_retire::RetireAction::OpenFromLaunch,
                        ),
                    ),
                ];
                // Revealed only once the launch-time read finds ≥1 custodied box
                // — the same gate web and linux apply. The saved nest being
                // unreachable is exactly when the admin needs this, but it is
                // also exactly when the read may return nothing; offering the CTA
                // regardless would dead-end them on an empty hub.
                if !recover_boxes.is_empty() {
                    out.push(Element::launch_button(
                        ids::LAUNCH_RECOVER_BUTTON,
                        fauna_i18n::strings::onboarding::launch::RECOVER_LOST_BOX,
                        LaunchAction::Recover,
                    ));
                }
                out
            }
            // NO `launch-retry-button`: futile against an outdated nest, and
            // actively harmful against a changed identity.
            LaunchSurface::NeedsUpdate { .. } => vec![Element::launch_button(
                ids::LAUNCH_FALLTHROUGH_BUTTON,
                t::USE_DIFFERENT_NEST,
                LaunchAction::Fallthrough,
            )],
            // `launch_sign_in_refused` (ui.yaml): the one terminal surface WITH
            // a retry — the remedy (the admin's restore) happens off this
            // device, and the machine honours the retry from this state
            // (`LaunchMachine::retry_silent_challenge`). The sentence gets its
            // OWN element, like the identity warning below, so an e2e asserting
            // "the user was told this nest no longer signs them in" cannot be
            // satisfied by any old error text.
            LaunchSurface::SignInRefused { error } => vec![
                Element::label(ids::LAUNCH_SIGN_IN_REFUSED_NOTICE, error.clone()),
                Element::launch_button(
                    ids::LAUNCH_RETRY_BUTTON,
                    t::RETRY_BUTTON,
                    LaunchAction::Retry,
                ),
                Element::launch_button(
                    ids::LAUNCH_FALLTHROUGH_BUTTON,
                    t::USE_DIFFERENT_NEST,
                    LaunchAction::Fallthrough,
                ),
            ],
            // `launch_identity_changed` (ui.yaml): the warning gets its OWN
            // element — not the generic `error-message` — so an e2e asserting
            // "the user was warned their nest's identity changed" cannot be
            // satisfied by any old error text. Exactly two ways out, no retry.
            LaunchSurface::IdentityChanged { error } => vec![
                Element::label(ids::NEST_IDENTITY_CHANGED_WARNING, error.clone()),
                Element::launch_button(
                    ids::NEST_IDENTITY_CHANGED_TRUST_BUTTON,
                    fauna_i18n::strings::onboarding::launch::IDENTITY_CHANGED_TRUST,
                    LaunchAction::Trust,
                ),
                Element::launch_button(
                    ids::LAUNCH_FALLTHROUGH_BUTTON,
                    t::USE_DIFFERENT_NEST,
                    LaunchAction::Fallthrough,
                ),
            ],
            // `launch_account_index_unreadable` (ui.yaml): its own warning
            // element, like the identity warning above, so an e2e asserting
            // "the user was told their accounts could not be read" cannot be
            // satisfied by any old error text. NO retry and NO fallthrough.
            LaunchSurface::AccountIndexUnreadable {
                refusal,
                confirming,
                ..
            } => {
                use fauna_i18n::strings::onboarding::launch as l;
                match (refusal, confirming) {
                    // The version case: the accounts are intact and updating
                    // brings them back. Nothing else helps, so nothing else is
                    // offered — a start-over here would destroy what an update
                    // would have restored.
                    (AccountIndexRefusal::NewerBuild { .. }, _) => vec![Element::label(
                        ids::ACCOUNT_INDEX_REFUSAL_WARNING,
                        l::INDEX_NEWER_BUILD,
                    )],
                    (AccountIndexRefusal::Malformed, false) => vec![
                        Element::label(ids::ACCOUNT_INDEX_REFUSAL_WARNING, l::INDEX_MALFORMED),
                        Element::launch_button(
                            ids::ACCOUNT_INDEX_RESET_BUTTON,
                            l::INDEX_MALFORMED_RESET,
                            LaunchAction::StartOver,
                        ),
                    ],
                    // The confirm states the residual in the warning itself,
                    // before the one button that acts on it.
                    (AccountIndexRefusal::Malformed, true) => vec![
                        Element::label(
                            ids::ACCOUNT_INDEX_REFUSAL_WARNING,
                            l::INDEX_MALFORMED_RESET_RESIDUAL,
                        ),
                        Element::launch_button(
                            ids::ACCOUNT_INDEX_RESET_CONFIRM_BUTTON,
                            l::INDEX_MALFORMED_RESET_CONFIRM,
                            LaunchAction::ConfirmStartOver,
                        ),
                    ],
                }
            }
            LaunchSurface::InstanceChooser {
                served_label,
                choices,
                ..
            } => {
                use fauna_i18n::strings::onboarding::instance_chooser as ic;
                // The page anchor itself (ui.yaml `launch-instance-chooser`,
                // `type: view`) — an inert container id, same convention as
                // `media-item-detail` (`media/mod.rs:1068`): a real registered
                // label, not a shim, carrying text a driver can read.
                let mut out = vec![Element::label(ids::LAUNCH_INSTANCE_CHOOSER, ic::TITLE)];
                // `launch-instance-chooser-item` is `indexed: true` — one row
                // per offered account, in registry order. No dedicated
                // "none available" element: an empty list plus the two exits
                // below already reads correctly (linux paints its own
                // `NONE_AVAILABLE` prose chrome-only, via `description()`).
                for (actor_id, label) in choices {
                    out.push(Element::gesture_button(
                        ids::LAUNCH_INSTANCE_CHOOSER_ITEM,
                        label.clone(),
                        true,
                        crate::element::Gesture::Launch(LaunchAction::PickInstance(
                            actor_id.clone(),
                        )),
                    ));
                }
                // NO `launch-instance-add-account-button`: declared absent on
                // tui (`tui.md` § Declared platform absences #5; ui.yaml
                // scopes it to `platform_elements: [windows, linux]`, user
                // decision 2026-08-01) — tui has no app<->app IPC channel to
                // forward the intent, and building one was rejected as
                // disproportionate to this one affordance.
                out.push(Element::launch_button(
                    ids::LAUNCH_INSTANCE_FOCUS_EXISTING_BUTTON,
                    ic::FOCUS_EXISTING,
                    LaunchAction::FocusExisting,
                ));
                let _ = served_label; // read by `description()`, not painted as an element
                out
            }
        }
    }

    /// Text for the canonical `error-message` element. Only `NeedsUpdate`,
    /// `InstanceChooser` and a refused start-over use it — the transient
    /// surface has its own `launch-transient-error` element, the identity
    /// warning has `nest-identity-changed-warning`, the refused sign-in has
    /// `launch-sign-in-refused-notice`, the account-index verdict has
    /// `account-index-refusal-warning`, and an empty string never registers
    /// (apple's lesson).
    pub fn error_text(&self) -> Option<String> {
        match self {
            LaunchSurface::NeedsUpdate { error } if !error.is_empty() => Some(error.clone()),
            LaunchSurface::InstanceChooser { error: Some(e), .. }
            | LaunchSurface::AccountIndexUnreadable { error: Some(e), .. }
                if !e.is_empty() =>
            {
                Some(e.clone())
            }
            _ => None,
        }
    }

    /// Pane title (chrome, not an automatable element). The unlock surfaces'
    /// title lives with their elements (`crate::unlock::title`), routed by
    /// `App::screen_title`.
    pub fn title(&self) -> String {
        match self {
            LaunchSurface::Launching => t::SIGNING_IN.to_string(),
            LaunchSurface::TransientRetry { .. } => t::RETRY_TITLE.to_string(),
            // Both of these reached the nest — "Couldn't reach your nest"
            // would misstate what happened (copy-audit, 2026-08-04).
            LaunchSurface::NeedsUpdate { .. } => t::NEEDS_UPDATE_TITLE.to_string(),
            LaunchSurface::SignInRefused { .. } => {
                fauna_i18n::strings::onboarding::launch::SIGN_IN_REFUSED_TITLE.to_string()
            }
            LaunchSurface::IdentityChanged { .. } => t::IDENTITY_CHANGED_TITLE.to_string(),
            LaunchSurface::AccountLocked { .. } => {
                fauna_i18n::strings::onboarding::launch::ACCOUNT_LOCKED_TITLE.to_string()
            }
            LaunchSurface::IdentityStolenEntry { .. } => crate::locked::entry_title(),
            LaunchSurface::AccountIndexUnreadable { refusal, .. } => match refusal {
                AccountIndexRefusal::NewerBuild { .. } => {
                    fauna_i18n::strings::onboarding::launch::INDEX_NEWER_BUILD_TITLE.to_string()
                }
                AccountIndexRefusal::Malformed => {
                    fauna_i18n::strings::onboarding::launch::INDEX_MALFORMED_TITLE.to_string()
                }
            },
            LaunchSurface::InstanceChooser { .. } => {
                fauna_i18n::strings::onboarding::instance_chooser::TITLE.to_string()
            }
            LaunchSurface::Wizard | LaunchSurface::Unlock | LaunchSurface::CreatePassphrase => {
                String::new()
            }
        }
    }

    /// Help text painted above the elements (chrome, not automatable).
    pub fn description(&self) -> Vec<String> {
        match self {
            LaunchSurface::Launching => vec![t::SIGNING_IN.to_string()],
            LaunchSurface::InstanceChooser {
                served_label,
                choices,
                ..
            } => {
                use fauna_i18n::strings::onboarding::instance_chooser as ic;
                let mut lines = vec![ic::subtitle(served_label)];
                // Every registered account is served somewhere: replace the
                // (empty) list with the explanation rather than leaving a
                // silent gap — the two exits below still work.
                if choices.is_empty() {
                    lines.push(ic::NONE_AVAILABLE.to_string());
                }
                lines
            }
            LaunchSurface::IdentityStolenEntry { .. } => crate::locked::entry_description(),
            _ => Vec::new(),
        }
    }
}

/// The `superseded`-refusal affordance: drop the user onto the identity-import
/// screen with the reason on its `error-message`
/// (`identity-succession.md` § Propagation → *Own device fleet* — *"the client
/// surfaces 'this identity was succeeded — import the new identity' and the user
/// imports the new seed"*; `onboarding.md` § 1 Identity states the same routing
/// for `recovery_entry`'s own superseded refusal).
///
/// **No new ui.yaml elements.** The affordance IS the existing import flow: the
/// user lands on page `identity_import` (`paste-secret-field` /
/// `import-submit-button`, plus `qr-camera-view` — the "QR / paste" the goal doc
/// names) and the explanation rides that page's existing `error-message`. A
/// dedicated launch surface would mint a near-twin of a page the user has to
/// reach anyway, and rule A makes an extra ID an explicit user decision.
///
/// **The message deliberately does not name the successor.** The refusal's
/// `claimed` successor is exactly that — claimed. Presenting it as fact before
/// `fauna_client_recovery::resolve_successor` has verified it against the
/// registration chain would make this client trust the nest as an *authorizer*,
/// which `identity-succession.md` § Propagation forbids ("verify the claim, not
/// trust it"). Naming the verified successor — and prefilling it — is the
/// follow-on that adds the anonymous chain fetch; until then the user is told
/// what happened and what to do, with nothing asserted that has not been proven.
fn route_superseded_to_import(app: &mut App, tx: &UnboundedSender<UiMessage>, claimed: String) {
    // Kept out of the message, but not out of the log: an admin debugging a
    // fleet needs the claim even though the user must not be shown it as fact.
    tracing::debug!("[launch] superseded refusal claimed successor {claimed}");
    verify_superseded_successor(app, tx);
    // Step and reason land together, in machine state, through the one shared
    // transition linux's arm also calls (`main.rs`, the `superseded_successor`
    // guard) — priority #1/#2: one mechanism, not a per-app one each. It has to
    // be machine state rather than the client-side `wizard.error` slot because
    // linux's view re-reads `error_message()` on every observer tick; keeping
    // tui on its own slot would leave the two apps expressing the same
    // affordance through different state with different lifetimes.
    app.wizard.machine.begin_import_identity_with_reason(
        fauna_i18n::strings::onboarding::launch::IDENTITY_SUPERSEDED.to_string(),
    );
    app.launch = LaunchSurface::Wizard;
}

/// Best-effort: verify the succession against the registration chain, so the
/// screen can name the successor as *fact* rather than as the nest's claim.
///
/// **Why anonymous.** The refused identity cannot authenticate — that is the
/// whole point of the refusal — so this would be impossible over the session
/// connection. It works because `succession.lookup` and `registration.chain`
/// are **pre-identity** kinds (`identity-succession.md` § Built — succession
/// enforcement: "`lookup` is pre-identity too (peers hold *old* ids)").
///
/// **Why the claimed successor is not passed in.** `resolve_successor` walks the
/// chain from the old identity and returns what the chain *authorizes*; the
/// nest's claim is an untrusted hint that plays no part in the verdict. If the
/// two ever disagree, the chain wins and the claim was a lie worth logging.
///
/// **Why it may silently never arrive.** Unreachable nest, an empty lookup, a
/// non-contiguous path, a hostile chain — every one of them leaves the
/// claim-free message standing, which is the correct fallback: telling the user
/// less is always safe, and naming an unverified successor is exactly the
/// nest-as-authorizer trust the goal doc forbids.
fn verify_superseded_successor(app: &App, tx: &UnboundedSender<UiMessage>) {
    let Some((Some(nest_url), secret_hex, _handle)) = crate::session::stored_account(app) else {
        return; // No stored nest ⇒ nowhere to fetch the chain from.
    };
    let tx = tx.clone();
    tokio::spawn(async move {
        // Derived from the same (bound) secret `stored_account` handed us, via
        // the shared `session_actor_id` seam `run_silent_sign_in` also goes
        // through — never from `registry(app).active()` directly, which can
        // name a different account on a bound launch, and mixing them sends
        // an anonymous succession lookup for the WRONG identity to this
        // nest; linux's twin
        // `verify_succession_successor` derives the same way, from the secret.
        let Ok(old_actor_id) =
            crate::session::session_actor_id(&secret_hex).map(|kp| kp.actor_id())
        else {
            return;
        };
        let Ok(anon) = fauna_anon_client::AnonymousNestClient::connect(&nest_url).await else {
            return; // Unreachable — the claim-free message stands.
        };
        let client = fauna_client_recovery::RecoveryClient::new(anon);
        match fauna_client_recovery::resolve_successor(&client, old_actor_id, None).await {
            Ok(Some(verified)) => {
                let successor = verified.new_actor_id.to_hex();
                tracing::info!(
                    "[launch] succession verified against the registration chain; \
                     successor {successor}"
                );
                let _ = tx.send(UiMessage::Data(DataMessage::IdentitySupersededVerified {
                    successor,
                    predecessor: old_actor_id.to_hex(),
                }));
            }
            Ok(None) => {
                // The nest refused as superseded but serves no succession for
                // this identity. Not something to show the user — but precisely
                // the disagreement an admin wants in the log.
                tracing::warn!("[launch] refused as superseded, yet the chain shows no succession");
            }
            Err(e) => tracing::warn!("[launch] could not verify the succession: {e}"),
        }
    });
}

/// The launch-retry surface's recover entry: the custodied boxes for the stored
/// account, from this device's own account store joined with a cold read from
/// the saved nest (`box-recovery.md` § The plane-era recovery floor, (b) The
/// reads). The local half is what reveals the button when the saved nest IS the
/// dead box — the case this surface exists for.
fn fetch_launch_recover_boxes(app: &App, tx: &UnboundedSender<UiMessage>) {
    let Some((nest_url, secret_hex, _handle)) = crate::session::stored_account(app) else {
        return; // No stored identity ⇒ no account to read custody for.
    };
    let tx = tx.clone();
    tokio::spawn(async move {
        let boxes = crate::recovery::load_recoverable_boxes(nest_url.as_deref(), &secret_hex).await;
        if !boxes.is_empty() {
            let _ = tx.send(UiMessage::Data(DataMessage::LaunchRecoverBoxes(boxes)));
        }
    });
}

/// The two boot-adjacent entry points ([`crate::app::App::route_locked_store`]'s
/// unsealed-store branch, and the deferred post-unlock path in
/// `crate::unlock::submit`) call this INSTEAD of [`start`] directly: it is the
/// launch-collision check (`account-scoping.md` § Concurrent instances → "the
/// colliding instance's surface"), mirroring linux's
/// `main.rs::launch_collision_detected` — a plain launch whose would-be
/// account (the store-active one) is already served by a live instance
/// renders the chooser instead of running the silent challenge for an
/// account this process cannot become.
///
/// Three conditions, all required, in linux's gate order:
/// 1. **No launch binding.** The chooser is a plain-launch affordance; a bound
///    collision is terminally refused instead. Checked first, so a bound
///    launch never even probes.
/// 2. **A resolvable would-be account** — the store-active one. `None` (a
///    fresh install, or an index not yet materialized) means there is nothing
///    to collide with *and* nothing to offer.
/// 3. **That account is currently served**, per the shared display-only probe.
///
/// **Deliberately NOT called from [`crate::app::App::switch_account`].** An
/// in-session switch is not a process collision — account-scoping.md
/// describes the chooser as strictly a plain-launch affordance, and
/// `switch_account`'s re-entry into [`start`] keeps today's terminal-refusal
/// behavior via `become_session_instance_or_exit` inside `session::establish`
/// (unchanged by this function).
pub fn start_or_offer_chooser(app: &mut App, tx: &UnboundedSender<UiMessage>) {
    let registry = crate::session::registry(app);
    let entries: Vec<(String, Option<String>)> = registry
        .list()
        .into_iter()
        .map(|e| (e.actor_id, e.handle))
        .collect();
    // The binding, resolved through the succession chain FIRST: a bound launch
    // whose named id has a recorded successor in this install's registry binds
    // to the successor (`account-scoping.md` § Concurrent instances → *The
    // binding follows the account*, rider 2). This is the earliest point tui
    // holds a registry, and it precedes `establish`'s bound-or-refuse — which,
    // tui having no bound session build yet, compares the binding against the
    // *active* account, itself moved to the successor by the ceremony. Asked
    // through the shared *binding cell* rather than `FAUNA_BOUND_ACCOUNT`
    // directly, because a binding can also arise after launch — the chooser's
    // own pick binds this process — and a client that re-read the environment
    // would silently disagree with `become_process_session_instance` about
    // what this process is (`instance_lock.rs::session_launch_binding`). linux
    // may read the environment at its own call site only because that site
    // runs once, before GApplication exists, and so cannot be re-entered after
    // a pick.
    let bound = registry.resolve_launch_binding();
    match collision_chooser_surface(
        crate::session::config_dir().as_deref(),
        bound.as_deref(),
        registry.active().as_deref(),
        &entries,
    ) {
        Some(surface) => app.launch = surface,
        None => start(app, tx),
    }
}

/// The pure half of [`start_or_offer_chooser`] — install-scoped base, launch
/// binding and registry rows passed in rather than read from the environment,
/// so every gate is testable without mutating process env or a real `App`
/// (the same reasoning as `account_scope::choosable_accounts_under`, and the
/// reason all three conditions above have deterministic tier_1 pins rather
/// than resting on a load-sensitive two-driver e2e run).
///
/// `Some(surface)` = collision, render the chooser; `None` = ordinary routing.
fn collision_chooser_surface(
    base: Option<&Path>,
    bound: Option<&str>,
    active: Option<&str>,
    entries: &[(String, Option<String>)],
) -> Option<LaunchSurface> {
    // Gate 1 — a **bound** launch never even probes: the chooser is strictly a
    // human affordance, so a `FAUNA_BOUND_ACCOUNT` collision stays terminally
    // refused at `session::establish`'s acquire ("wired IPC must be
    // deterministic" — `account-scoping.md` § Concurrent instances → the
    // colliding instance's surface). linux gates identically, as step 1 of
    // `main.rs::launch_collision_detected`.
    if bound.is_some() {
        return None;
    }
    // Gate 2 — a resolvable would-be account, and gate 3 — it is served.
    let base = base?;
    let active = active?;
    if !fauna_client_accounts::AccountInstanceLock::is_served(base, active) {
        return None;
    }
    tracing::info!(
        "[launch-collision] {active} is already served by a live instance — offering the chooser"
    );
    let served_label = entries
        .iter()
        .find(|(id, _)| id == active)
        .map(|(id, handle)| fauna_core::format::account_display_label(handle.as_deref(), id))
        .unwrap_or_else(|| fauna_core::format::account_display_label(None, active));
    Some(LaunchSurface::InstanceChooser {
        served_label,
        served_actor: active.to_string(),
        choices: crate::account_scope::choosable_accounts_under(base, entries),
        error: None,
    })
}

/// Build the launch machine over the shared registry adapter and drive
/// `start()` on a worker, reporting the settled snapshot back through the UI
/// channel. Called at boot (via [`start_or_offer_chooser`]) and again after
/// every account switch ([`crate::app::App::switch_account`], directly — NOT
/// through the chooser check) — the single re-entrant "begin launch routing
/// over the active account" seam.
///
/// The machine is kept on the app: `Retry` needs it, and on `Online` it becomes
/// the session's bearer source.
pub fn start(app: &mut App, tx: &UnboundedSender<UiMessage>) {
    // No boot re-mirror any more (2026-09-24): launch routes on the registry's
    // active account alone, and an append-mode wizard moves the active pointer
    // only when its own terminal registers and switches (`apps/tui.md`
    // § Append-mode "Add account"; a provisioning run's custody mint registers
    // the appended identity INACTIVE, `onboarding.md` § Multi-account), so
    // there is no single slot an abandoned append could have polluted and
    // nothing to heal before routing reads the store.

    // `LaunchMachine::new` already returns `Arc<Self>` — do not wrap it again.
    let machine = LaunchMachine::new(
        // The machine changes state on its own in exactly one case this app
        // must hear about: the refresh it runs when a lock lapses
        // (`devices.md` § The locked state). Every other transition is driven
        // by a gesture that reads the settled snapshot itself, so the relay
        // only ever moves the locked notice (`crate::locked::follow_snapshot`).
        Arc::new(SnapshotRelay { tx: tx.clone() }),
        // Route through the multi-account registry, BOUND to the launch
        // target exactly as `session::stored_account` is (same function,
        // same bound-vs-active branch) — a bound launch's machine and its
        // eventual session build must resolve the identical account, never
        // silently disagree on one reading the active account and the other
        // the bound account's own slots.
        // `docs/goal/architecture/long-term-store.md` § Shared seam.
        Arc::new(crate::session::launch_persistence(app)),
    );
    app.launch_machine = Some(Arc::clone(&machine));
    app.launch = LaunchSurface::Launching;

    let tx = tx.clone();
    tokio::spawn(async move {
        machine.start().await;
        let _ = tx.send(UiMessage::Data(DataMessage::LaunchPhase(
            machine.snapshot(),
        )));
    });
}

/// The launch machine's observer: forwards "the machine changed" onto the UI
/// channel as a payload-free tick. The handler re-reads the machine's snapshot
/// — a tick carries no state, so a burst of them cannot apply a stale one.
struct SnapshotRelay {
    tx: UnboundedSender<UiMessage>,
}

impl fauna_launch_machine::LaunchObserver for SnapshotRelay {
    fn on_changed(&self) {
        let _ = self
            .tx
            .send(UiMessage::Data(DataMessage::LaunchMachineChanged));
    }
}

/// `sync-agent.md` § Credential model → *The signed-out reconcile*, shape
/// (a): landing on the onboarding wizard means this app has no
/// account, so nudge a reachable agent to drop the capability now rather
/// than wait for its own renewal-loop cadence. Best-effort and guarded — see
/// [`fauna_client_sync::agent::signed_out_onboarding_reconcile`] for the
/// actor-matched licensing that keeps a co-resident sibling account safe.
fn spawn_signed_out_onboarding_reconcile() {
    let Ok(endpoint) = fauna_ipc::endpoint::AgentEndpoint::default_for_user() else {
        return;
    };
    tokio::spawn(async move {
        fauna_client_sync::agent::signed_out_onboarding_reconcile(&endpoint).await;
    });
}

/// Dispatch the settled [`LaunchSnapshot`] to the surface it selects.
///
/// Phase mapping per `onboarding.md` § App-launch routing:
///
/// - `Online` → establish the session (the authenticated shell).
/// - `WizardAt(ClaimCode)` → `/verify` 404 **and** `setup-status.claimed == false`:
///   seed identity + land the wizard at `claim_code`.
/// - `WizardAt(InviteRequest)` → `/verify` 404 on a claimed nest (or an
///   unreadable setup-status — the safer default): seed identity + land at
///   `invite_request`.
/// - `WizardAt(HandleEntry | IdentityChoice)` → the hydration branch: no saved
///   nest_url (or no identity at all), so the wizard starts where it always does.
/// - `Offline { transient }` → the retry surface, or the non-retry
///   "update required" surface when the nest reported itself outdated.
pub fn route(app: &mut App, tx: &UnboundedSender<UiMessage>, snapshot: LaunchSnapshot) {
    match snapshot.phase {
        LaunchPhase::Online => {
            let Some((nest_url, secret_hex, handle)) = crate::session::stored_account(app) else {
                // The store emptied under us between `start()` and here — or,
                // bound, the named account's material never resolved. Either
                // way the fallthrough below is the onboarding wizard, which a
                // bound launch may never silently land on.
                fauna_client_accounts::refuse_if_bound_from_onboarding(
                    "the onboarding wizard (no resolvable stored account)",
                );
                tracing::error!("[launch] Online with no stored account");
                app.launch = LaunchSurface::Wizard;
                return;
            };
            let Some(nest_url) = nest_url else {
                fauna_client_accounts::refuse_if_bound_from_onboarding(
                    "the onboarding wizard (no stored nest_url)",
                );
                tracing::error!("[launch] Online with no stored nest_url");
                app.launch = LaunchSurface::Wizard;
                return;
            };
            // The machine is Online, so its bearer cache is warm and
            // `session::establish` mints the WS handshake over it.
            //
            // This is the **store-read** dial — the one every app shares, and
            // the one tui's own `LoggedIn` parameter never covered: an "Add
            // account" append lands here via `switch_account`, and so does every
            // ordinary relaunch. `stored_account` returns the literal persisted
            // URL; the socket resolves through the shared seam
            // (`fauna_launch_machine::dial`), identical in production.
            let dial_url = fauna_launch_machine::resolved_dial_url(&nest_url);
            if let Err(e) = crate::session::establish(app, tx, &dial_url, &secret_hex, handle) {
                tracing::error!("[launch] Online but the session failed to build: {e}");
                app.launch = LaunchSurface::TransientRetry {
                    error: e,
                    recover_boxes: Vec::new(),
                };
                return;
            }
            // Only once a session actually owns that bearer: the TTL pre-expiry
            // refresh loop keeps it warm (the machine's contract — "spawning is
            // the caller's responsibility"). Spawning it before `establish` would
            // leave it refreshing tokens forever for a session that never existed.
            if let Some(machine) = &app.launch_machine {
                tokio::spawn(Arc::clone(machine).ttl_refresh_loop());
            }
            app.launch = LaunchSurface::Wizard;
        }
        LaunchPhase::WizardAt { entry } => {
            // Every entry this arm can be reached with (IdentityChoice,
            // HandleEntry, InviteRequest, ClaimCode, PendingFactoryReset)
            // hands control to the shared onboarding scratchpad — a bound
            // launch never gets it (`account-scoping.md` § Concurrent
            // instances — "launch bound or refuse").
            fauna_client_accounts::refuse_if_bound_from_onboarding("the onboarding wizard");
            route_wizard_entry(app, entry);
            app.launch = LaunchSurface::Wizard;
            // A residue an earlier sign-out recorded is re-swept silently here,
            // and painted on `identity_choice` only if something is still left
            // (`account-scoping.md` § Erasure follows scope → *the residue
            // surface*). After the surface is set: a re-erase that relocks the
            // sealed store moves it on to the create surface.
            crate::account_scope::recheck_residue_at_launch(app);
            spawn_signed_out_onboarding_reconcile();
        }
        // The identity was succeeded — the account belongs to someone else's
        // keypair now (`identity-succession.md` § Propagation → *Own device
        // fleet*). Checked BEFORE the generic `Offline` arms below, because the
        // machine deliberately projects this to `Offline { transient: false }`:
        // the successor rides the snapshot side channel, not the phase, so that
        // apps which cannot yet render it still stop retrying. See
        // `fauna_launch_machine::State::Superseded` for why.
        // The saved account index is present and unusable
        // (`onboarding.md` § App-launch routing — the row checked before every
        // other). Checked before BOTH `Offline` arms below for the reason the
        // superseded arm gives: the machine projects it to
        // `Offline { transient: false }` and carries the verdict on a side
        // channel, so an app that cannot render it still stops. Without this
        // arm it would fall into `NeedsUpdate`, whose "use a different nest"
        // CTA misstates the problem — the nest is fine.
        LaunchPhase::Offline { .. } if snapshot.account_index_refusal.is_some() => {
            let refusal = snapshot.account_index_refusal.expect("guarded by is_some");
            tracing::error!("[launch] the saved account index is unreadable: {refusal:?}");
            app.launch = LaunchSurface::AccountIndexUnreadable {
                refusal,
                confirming: false,
                error: None,
            };
        }
        // The live launch machine hears the same refusal the session does (its
        // bearer's refresh meets `superseded` once the ceremony's connection
        // dies), so this arm is the SECOND channel by which a device's own
        // succession ceremony was pre-empted: the session arm's hold-back
        // (`App::defer_own_supersession`, `settings.md` § Recovery kit) alone
        // still moved the user to the import screen here, and a lost reply's
        // unknown-outcome message never showed. Held back the same way — the
        // deferral is recorded, and performed once the user leaves Account.
        LaunchPhase::Offline { .. }
            if snapshot.superseded_successor.is_some() && app.owns_its_supersession() =>
        {
            app.defer_own_supersession();
            tracing::info!(
                "[launch] superseded by this device's own succession ceremony — held back \
                 until the ceremony's outcome has been seen"
            );
        }
        LaunchPhase::Offline { .. } if snapshot.superseded_successor.is_some() => {
            let claimed = snapshot.superseded_successor.unwrap_or_default();
            tracing::error!(
                "[launch] this identity was succeeded (claimed successor {claimed}) — routing to \
                 the identity-import flow"
            );
            route_superseded_to_import(app, tx, claimed);
        }
        // A nest this app signed in to before refused the identity
        // (`onboarding.md` § App-launch routing — the previously-signed-in
        // row). Checked before the generic `Offline` arm for the reason the
        // two arms above are: the machine projects it to
        // `Offline { transient: false }` and carries the verdict on a side
        // channel, so an app that cannot render it still stops. Without this
        // arm it would fall into `NeedsUpdate`, whose "Update needed" title
        // misstates the problem and whose surface offers no way back in.
        LaunchPhase::Offline { .. } if snapshot.sign_in_refused => {
            let error = snapshot.last_error.unwrap_or_default();
            tracing::error!("[launch] the saved nest no longer signs this identity in: {error}");
            app.launch = LaunchSurface::SignInRefused { error };
        }
        // The account is locked (`devices.md` § The locked state). Checked
        // before the generic `Offline` arm for the reason the arms above are:
        // the machine projects it to `Offline { transient: false }` and carries
        // the unlock time on a side channel, so without this arm it would fall
        // into `NeedsUpdate`, whose "Update needed" title and "use a different
        // nest" CTA both misstate the problem. After the superseded and
        // sign-in-refused arms on purpose — the nest checks the lock last
        // (`login.md` § Silent Challenge), so a snapshot never carries both.
        LaunchPhase::Offline { .. } if snapshot.locked_until_secs.is_some() => {
            let locked_until_secs = snapshot.locked_until_secs.expect("guarded by is_some");
            tracing::warn!("[launch] this account is locked until {locked_until_secs}");
            app.launch = LaunchSurface::AccountLocked { locked_until_secs };
        }
        LaunchPhase::Offline { transient } => {
            let error = snapshot.last_error.unwrap_or_default();
            tracing::error!("[launch] silent-challenge failure (transient={transient}): {error}");
            app.launch = if transient {
                // The saved nest is unreachable — which is precisely the
                // total-box-loss case. Ask it (best-effort) whether it custodies
                // any boxes; a non-empty answer reveals `launch-recover-button`.
                fetch_launch_recover_boxes(app, tx);
                LaunchSurface::TransientRetry {
                    error,
                    recover_boxes: Vec::new(),
                }
            } else {
                // Today the only non-transient launch outcome is the nest
                // reporting `fauna.nest.outdated`, whose localized message the
                // machine already carries in `last_error`.
                LaunchSurface::NeedsUpdate { error }
            };
        }
        LaunchPhase::IdentityChanged { .. } => {
            //auto-entry is blocked and the bearer already dropped
            // machine-side. Warn with the SAME shared string every other app
            // shows, and offer NO retry — before this arm existed the phase fell
            // into the `other =>` catch-all below and painted a *dead* Retry
            // button (`retry_silent_challenge()` no-ops outside
            // `Offline{transient:true}`) over a raw debug dump.
            tracing::error!(
                "[launch] nest identity changed: {}",
                snapshot.last_error.as_deref().unwrap_or("(no detail)")
            );
            app.launch = LaunchSurface::IdentityChanged {
                error: fauna_i18n::strings::onboarding::launch::IDENTITY_CHANGED_WARNING
                    .to_string(),
            };
        }
        // Boot / Hydrating / SilentChallenge / Refreshing cannot survive
        // `start()`. Surface as transient so the user keeps a recovery path
        // rather than staring at a spinner (linux's `other` arm does the same).
        other => {
            // Empty, not a hand-rolled English sentence: `transient_error_text`
            // falls back to the localized generic retry copy on an empty
            // `error` — this Debug dump is a developer diagnostic, not
            // something translatable, so it stays in the log line above, never
            // on screen.
            tracing::error!("[launch] unexpected phase after start: {other:?}");
            app.launch = LaunchSurface::TransientRetry {
                error: String::new(),
                recover_boxes: Vec::new(),
            };
        }
    }
}

/// Tear down the authenticated session (keeping credentials) and re-seed the
/// wizard onto the pre-filled claim-code surface — the in-process re-onboard the
/// admin-nest Factory Reset triggers once `fauna.admin.factory_reset` has
/// dispatched and its claim code is durably persisted (`admin.md` § N Nest; the
/// tui twin of linux's `FactoryResetComplete` handler, `app.rs:2852`). The
/// persisted `PendingFactoryReset` record carries the code, read back through the
/// same adapter [`route_wizard_entry`] branches on.
///
/// This is `App::reset`'s teardown **without** the credential-namespace wipe: the
/// credentials AND the freshly-minted claim-code record must survive the
/// transition — wiping them here would destroy the code CR-1 exists to protect.
pub fn enter_factory_reset_reonboard(app: &mut App) {
    crate::session::sign_out(app, fauna_client_account_runtime::StopReason::AccountSwitch);
    app.launch_machine = None;
    // The re-onboard claims the nest and signs in again, so it waits for the
    // outgoing account's stop — one account's runtime at a time.
    app.after_stops(|app| {
        // Fresh wizard machine so the seed below lands cleanly (App::reset's wizard half).
        app.wizard.reset();
        route_wizard_entry(app, LaunchWizardEntry::PendingFactoryReset);
        app.launch = LaunchSurface::Wizard;
    });
}

/// Seed the onboarding machine for the wizard entry the launch flow chose.
///
/// `ClaimCode` / `InviteRequest` are the silent-challenge fallbacks, so a
/// `nest_url` is present and the wizard skips straight to the page for that
/// known nest — the user never retypes their handle.
fn route_wizard_entry(app: &mut App, entry: LaunchWizardEntry) {
    let Some((nest_url, secret_hex, handle)) = crate::session::stored_account(app) else {
        // `IdentityChoice` is the no-identity row: nothing to seed.
        return;
    };
    let machine = &app.wizard.machine;
    match entry {
        LaunchWizardEntry::IdentityChoice => {}
        LaunchWizardEntry::HandleEntry => machine.seed_identity(secret_hex.to_string()),
        // The shared machine yields `InviteRequest` for two different rows, so
        // `nest_url` disambiguates them (`onboarding.md` § App-launch routing):
        //   * `Some` — the silent-challenge 404-on-a-claimed-nest fallback: the
        //     saved nest is up, this actor just isn't registered on it.
        //   * `None` — the hydration pending-invite row: a request is already
        //     submitted, so reseed it and land back on its PendingReview state.
        LaunchWizardEntry::InviteRequest => {
            machine.seed_identity(secret_hex.to_string());
            match nest_url {
                Some(nest_url) => {
                    machine.navigate_to_invite_request_for_known_nest(nest_url, handle)
                }
                None => {
                    // The same read the machine routed on, so the record it saw
                    // is the record we seed — no second source of truth.
                    if let Some(rec) = crate::session::launch_persistence(app).load_pending_invite()
                    {
                        machine.seed_pending_invite(
                            rec.nest_url,
                            rec.handle,
                            rec.request_id,
                            rec.status_json,
                        );
                    }
                }
            }
        }
        LaunchWizardEntry::ClaimCode => {
            machine.seed_identity(secret_hex.to_string());
            if let Some(nest_url) = nest_url {
                machine.navigate_to_claim_code_for_known_nest(nest_url, handle);
            }
        }
        // Factory-reset resume (gap CR-1, `common.md` § Client-state
        // recoverability): the admin reset their box and the client died before
        // the re-claim. Same surface as `ClaimCode`, but the code is **pre-filled**
        // from the slot the client persisted before dispatching the reset — it
        // exists nowhere else, since the reply that carried it was never rendered.
        // Read back through the adapter the machine branched on, so the record we
        // seed is the record it routed on.
        LaunchWizardEntry::PendingFactoryReset => {
            machine.seed_identity(secret_hex.to_string());
            if let Some(rec) = crate::session::launch_persistence(app).load_pending_factory_reset()
            {
                machine.navigate_to_claim_code_for_known_nest_with_code(
                    rec.nest_url,
                    rec.handle,
                    rec.claim_code,
                );
            }
        }
        // Deferred-DNS resume: the user provisioned a nest, chose "Set up later"
        // for DNS and quit. Seeding sets `wizard_outcome()` to `AwaitingManualDns`,
        // which is what paints the "Almost ready" surface — the same surface the
        // same-session exit paints, so hydration and exit are one code path
        // (`onboarding.md` § "Almost ready" surface).
        LaunchWizardEntry::AwaitingManualDns => {
            machine.seed_identity(secret_hex.to_string());
            // Read back through the adapter the machine branched on, so the
            // record we seed is byte-for-byte the record it routed on.
            if let Some(rec) = crate::session::launch_persistence(app).load_awaiting_dns() {
                machine.seed_awaiting_manual_dns_record(rec);
            }
        }
    }
}

/// Run one launch-surface gesture to completion.
///
/// Awaited on the agent's click path so the driver's next (single-shot,
/// un-retried) element read already observes the new surface; the keyboard path
/// spawns it, exactly as the wizard's gestures split.
pub async fn perform(app: &mut App, tx: &UnboundedSender<UiMessage>, action: LaunchAction) {
    match action {
        LaunchAction::Retry => {
            let Some(machine) = app.launch_machine.clone() else {
                return;
            };
            // `retry_silent_challenge` self-guards to `Offline { transient: true }`
            // and the sign-in-refused state (the one terminal whose remedy is
            // off-device), so a NeedsUpdate surface can never spin a doomed
            // retry — but that surface paints no Retry button anyway.
            app.launch = LaunchSurface::Launching;
            machine.retry_silent_challenge().await;
            let snapshot = machine.snapshot();
            route(app, tx, snapshot);
        }
        LaunchAction::Trust => {
            let Some(machine) = app.launch_machine.clone() else {
                return;
            };
            // `trust_nest_identity` self-guards to `IdentityChanged`, so this can
            // only ever forget a pin the machine actually flagged — and it forgets
            // it through the same store the check consulted.
            app.launch = LaunchSurface::Launching;
            machine.trust_nest_identity().await;
            let snapshot = machine.snapshot();
            route(app, tx, snapshot);
        }
        LaunchAction::Fallthrough => {
            // "Use a different nest": seed the identity and hand the screen to
            // the wizard, which `seed_identity` lands at `HandleEntry`.
            match crate::session::stored_account(app) {
                Some((_, secret_hex, _)) => {
                    app.wizard.machine.seed_identity(secret_hex.to_string())
                }
                None => tracing::error!("[launch] fallthrough with no stored identity"),
            }
            app.launch = LaunchSurface::Wizard;
        }
        LaunchAction::Recover => enter_recovery(app),
        LaunchAction::PickInstance(actor_id) => pick_instance(app, tx, &actor_id),
        LaunchAction::FocusExisting => focus_existing(app, tx),
        LaunchAction::StartOver => reveal_start_over(app),
        LaunchAction::ConfirmStartOver => start_over(app),
        LaunchAction::OpenStolenEntry => crate::locked::open_entry(app),
        LaunchAction::BackToLocked => crate::locked::back(app, tx),
        // Awaited: the driver's next read sees the ceremony's settled outcome.
        LaunchAction::SubmitStolen => crate::locked::submit(app).await,
    }
}

/// "Start over on this device": reveal the confirm. A no-op on any surface
/// but the malformed verdict — the version verdict paints no start-over, and
/// this must not become a second way to reach the reset from it.
fn reveal_start_over(app: &mut App) {
    if let LaunchSurface::AccountIndexUnreadable {
        refusal: AccountIndexRefusal::Malformed,
        confirming,
        ..
    } = &mut app.launch
    {
        *confirming = true;
    }
}

/// The confirm: the documented floor (`long-term-store.md` § Cleanup
/// contract), which is exactly the factory reset — one path, never a second.
/// Guarded the same way as [`reveal_start_over`], and additionally on the
/// confirm having been shown: the residual is stated before this can run.
///
/// ⚠ **Refused while another instance serves an account the reset would
/// erase** — the same question the Settings sign-out asks, because it is the
/// same erase (`account-scoping.md` § Concurrent instances → *An erase refuses
/// while a sibling serves the account*). The factory reset's exemption is the
/// automation surface's, which has no user in front of it; this confirm is a
/// user's button.
fn start_over(app: &mut App) {
    start_over_unless(app, crate::account_scope::start_over_blocked);
}

/// [`start_over`] with its refusal passed in, so a test can reach the gesture
/// without a live sibling on this machine's real config dirs (the question's
/// own answer is pinned in `account_scope`, over temp bases).
fn start_over_unless(app: &mut App, blocked: impl FnOnce(&App) -> Option<String>) {
    let armed = matches!(
        app.launch,
        LaunchSurface::AccountIndexUnreadable {
            refusal: AccountIndexRefusal::Malformed,
            confirming: true,
            ..
        }
    );
    if !armed {
        return;
    }
    if let Some(line) = blocked(app) {
        if let LaunchSurface::AccountIndexUnreadable { error, .. } = &mut app.launch {
            *error = Some(line);
        }
        return;
    }
    app.reset();
}

/// The surviving-device entry into box recovery: seed the machine for recovery
/// and hand the screen to the wizard, which lands on `nest_recovery` with the
/// launch-time box list already rendered.
///
/// `seed_identity_for_recovery` is a superset of `seed_identity`: same imported
/// secret, but it flips `recovery_intent` + `recovery_came_from = Launch` and
/// lands the step on `NestRecovery` — so the wizard opens *at* the hub rather
/// than walking the admin back through identity import (they are on a device that
/// still has the identity; that is the whole point of this entry).
fn enter_recovery(app: &mut App) {
    let boxes = match &app.launch {
        LaunchSurface::TransientRetry { recover_boxes, .. } => recover_boxes.clone(),
        // Unreachable: the button paints only on the transient-retry surface.
        _ => return,
    };
    let Some((_, secret_hex, _)) = crate::session::stored_account(app) else {
        tracing::error!("[launch] recover with no stored identity");
        return;
    };
    app.wizard
        .machine
        .seed_identity_for_recovery(secret_hex.to_string());
    // Push the list the launch-time read already paid for, so the hub renders its
    // rows immediately instead of blanking while the page-entry read re-fetches.
    app.wizard.machine.set_recovery_boxes(boxes);
    app.launch = LaunchSurface::Wizard;
}

/// `launch-instance-chooser-item`: bind this process to `actor_id` and
/// re-enter routing as it. Reuses [`crate::app::App::switch_account`]
/// unconfirmed — the identical mechanism the authenticated switcher already
/// uses, since a chooser pick IS a switch, just one made before this process
/// ever established a session (mirrors `session::adopt_appended`'s identical
/// call shape for the append-mode wizard's completion).
///
/// A pick can still lose the race between the chooser's display-only probe
/// and this call (someone else took the account) — `switch_account`'s
/// `bind_account` refusal surfaces here as the chooser's own `error-message`,
/// same as linux's `account_taken` (arbitration stays at the instance lock's
/// acquire inside `session::establish`, not here).
fn pick_instance(app: &mut App, tx: &UnboundedSender<UiMessage>, actor_id: &str) {
    if let Err(e) = app.switch_account(actor_id, false) {
        tracing::warn!("[launch-collision] pick {actor_id} refused: {e}");
        if let LaunchSurface::InstanceChooser { error, .. } = &mut app.launch {
            *error =
                Some(fauna_i18n::strings::onboarding::instance_chooser::ACCOUNT_TAKEN.to_string());
        }
    }
    let _ = tx; // kept for signature symmetry with the other LaunchAction arms
}

/// `launch-instance-focus-existing-button`, through the shared decision every
/// chooser seat makes (`fauna_client_accounts::resolve_focus_existing`). tui
/// claims no per-account activation endpoint and raises no window — declared
/// platform absence, no window manager can raise a terminal (`tui.md`
/// § Declared platform absences) — so its raise never lands and the decision
/// always re-probes the lock (`account-scoping.md` § Concurrent instances →
/// the raise channel, the ratified degrade):
///
/// - **no longer served** — the sibling went away between the probe that
///   rendered the chooser and the click, so there is no collision left: this
///   process continues as the plain launch it was always trying to be
///   ([`start`], the same re-entry a switch takes);
/// - **still served** — it prints where the account is being served and
///   exits ([`focus_existing_and_exit`]): the terminal analog of raising it.
fn focus_existing(app: &mut App, tx: &UnboundedSender<UiMessage>) {
    let served = match &app.launch {
        LaunchSurface::InstanceChooser { served_actor, .. } => served_actor.clone(),
        _ => return,
    };
    let base = crate::session::config_dir();
    let outcome = fauna_client_accounts::resolve_focus_existing(
        &served,
        |_| false,
        |id| {
            base.as_deref()
                .is_some_and(|b| fauna_client_accounts::AccountInstanceLock::is_served(b, id))
        },
    );
    match outcome {
        fauna_client_accounts::FocusExistingOutcome::NoLongerServed => {
            tracing::info!(
                "[launch-instance-chooser] {served} is no longer served — continuing as a plain launch"
            );
            start(app, tx);
        }
        fauna_client_accounts::FocusExistingOutcome::Raised
        | fauna_client_accounts::FocusExistingOutcome::StillServedNoChannel => {
            focus_existing_and_exit(app)
        }
    }
}

/// The still-served arm of [`focus_existing`]: prints where the account is
/// already being served and exits (`account-scoping.md` § Concurrent
/// instances → the raise channel).
///
/// stderr as well as `tracing`, matching `become_session_instance_or_exit`:
/// `exit` skips the log appender's flush, and the user needs the reason on
/// the terminal they launched from.
fn focus_existing_and_exit(app: &App) -> ! {
    let served_label = match &app.launch {
        LaunchSurface::InstanceChooser { served_label, .. } => served_label.as_str(),
        _ => "",
    };
    eprintln!(
        "[launch-instance-chooser] {served_label} is already running in another terminal — \
         switch to it there."
    );
    tracing::info!(
        "[launch-instance-chooser] focus-existing: printed and exiting ({served_label})"
    );
    std::process::exit(0);
}

/// Spawn a launch gesture from the keyboard path (the observer/redraw tick
/// picks up the result), mirroring `wizard::spawn_action`.
///
/// The gesture mutates `App`, which the spawned task cannot hold, so it round-
/// trips through the UI channel: the task drives the machine and posts the
/// settled snapshot back for `route` to apply on the main loop.
pub fn spawn_launch_action(app: &mut App, tx: &UnboundedSender<UiMessage>, action: LaunchAction) {
    match action {
        LaunchAction::Retry => {
            let Some(machine) = app.launch_machine.clone() else {
                return;
            };
            app.launch = LaunchSurface::Launching;
            let tx = tx.clone();
            tokio::spawn(async move {
                machine.retry_silent_challenge().await;
                let _ = tx.send(UiMessage::Data(DataMessage::LaunchPhase(
                    machine.snapshot(),
                )));
            });
        }
        LaunchAction::Trust => {
            let Some(machine) = app.launch_machine.clone() else {
                return;
            };
            app.launch = LaunchSurface::Launching;
            let tx = tx.clone();
            tokio::spawn(async move {
                machine.trust_nest_identity().await;
                let _ = tx.send(UiMessage::Data(DataMessage::LaunchPhase(
                    machine.snapshot(),
                )));
            });
        }
        // Purely local — no machine call to await.
        LaunchAction::Fallthrough => {
            match crate::session::stored_account(app) {
                Some((_, secret_hex, _)) => {
                    app.wizard.machine.seed_identity(secret_hex.to_string())
                }
                None => tracing::error!("[launch] fallthrough with no stored identity"),
            }
            app.launch = LaunchSurface::Wizard;
        }
        // Also purely local — the box list was fetched at launch.
        LaunchAction::Recover => enter_recovery(app),
        // Also purely local (a registry mutation + a re-entrant `start` that
        // spawns its own async work) — no separate task to spawn here.
        LaunchAction::PickInstance(actor_id) => pick_instance(app, tx, &actor_id),
        LaunchAction::FocusExisting => focus_existing(app, tx),
        // Both purely local: one flips a flag, the other is the synchronous
        // factory reset the agent path also runs.
        LaunchAction::StartOver => reveal_start_over(app),
        LaunchAction::ConfirmStartOver => start_over(app),
        LaunchAction::OpenStolenEntry => crate::locked::open_entry(app),
        LaunchAction::BackToLocked => crate::locked::back(app, tx),
        // Network work: the outcome rides the UI channel back to the fold.
        LaunchAction::SubmitStolen => crate::locked::spawn_submit(app, tx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(surface: &LaunchSurface) -> Vec<String> {
        surface.elements().iter().map(|e| e.id.clone()).collect()
    }

    fn refusal_snapshot(refusal: AccountIndexRefusal) -> LaunchSnapshot {
        LaunchSnapshot {
            phase: LaunchPhase::Offline { transient: false },
            last_error: Some("unreadable".into()),
            account_index_refusal: Some(refusal),
            ..LaunchSnapshot::initial()
        }
    }

    /// `onboarding.md` § App-launch routing — the previously-signed-in row. The
    /// machine parks it on the same `Offline { transient: false }` an outdated
    /// nest gets, so only the side channel tells the two apart: with it set the
    /// surface is the refused one — the sentence in its own
    /// `launch-sign-in-refused-notice`, a Retry (the way back in after the admin
    /// restores) and the fallthrough; without it the same snapshot is
    /// `NeedsUpdate`, so the arm order is load-bearing.
    #[test]
    fn a_refused_sign_in_paints_the_sentence_with_retry_and_fallthrough_never_the_wizard() {
        let mut app = crate::app::tests::test_app();
        let tx = app.tx.clone();
        let copy = fauna_i18n::strings::onboarding::launch::SIGN_IN_REFUSED;
        let snapshot = LaunchSnapshot {
            phase: LaunchPhase::Offline { transient: false },
            last_error: Some(copy.to_string()),
            sign_in_refused: true,
            ..LaunchSnapshot::initial()
        };

        route(&mut app, &tx, snapshot.clone());

        assert!(
            matches!(app.launch, LaunchSurface::SignInRefused { .. }),
            "its own surface — not NeedsUpdate, whose title misstates the problem, \
             and never the wizard: got {:?}",
            app.launch
        );
        assert_eq!(
            ids(&app.launch),
            vec![
                "launch-sign-in-refused-notice",
                "launch-retry-button",
                "launch-fallthrough-button"
            ]
        );
        assert_eq!(
            app.launch.elements()[0].text,
            copy,
            "the notice carries the sentence"
        );
        // Its own element, not the generic `error-message` — an e2e reading the
        // notice cannot be satisfied by any old error text.
        assert_eq!(app.launch.error_text(), None);
        assert_eq!(
            app.launch.title(),
            fauna_i18n::strings::onboarding::launch::SIGN_IN_REFUSED_TITLE
        );

        // The control: the same snapshot without the side channel is the
        // outdated-nest surface (no retry) — the field is what routes.
        route(
            &mut app,
            &tx,
            LaunchSnapshot {
                sign_in_refused: false,
                ..snapshot
            },
        );
        assert!(matches!(app.launch, LaunchSurface::NeedsUpdate { .. }));
        assert_eq!(ids(&app.launch), vec!["launch-fallthrough-button"]);
    }

    /// `onboarding.md` § App-launch routing — the account-index row. The
    /// version verdict's accounts are intact and an update brings them back,
    /// so it paints the warning and **nothing else**: no retry (no retry
    /// reparses a blob), no fallthrough (the nest is not the problem), and no
    /// start-over — which would destroy exactly what an update would restore.
    #[test]
    fn a_newer_builds_index_paints_the_warning_and_offers_no_action() {
        let mut app = crate::app::tests::test_app();
        let tx = app.tx.clone();

        route(
            &mut app,
            &tx,
            refusal_snapshot(AccountIndexRefusal::NewerBuild {
                index_v: 2,
                index_min: 2,
                bin_v: 1,
            }),
        );

        assert!(
            matches!(app.launch, LaunchSurface::AccountIndexUnreadable { .. }),
            "its own surface — not NeedsUpdate, whose fallthrough misstates the \
             problem, and never the wizard: got {:?}",
            app.launch
        );
        assert_eq!(ids(&app.launch), vec!["account-index-refusal-warning"]);
        assert_eq!(
            app.launch.error_text(),
            None,
            "the warning has its own element, so any old error text cannot pass for it"
        );

        // And the start-over cannot be reached from here by any gesture.
        reveal_start_over(&mut app);
        start_over(&mut app);
        assert_eq!(
            ids(&app.launch),
            vec!["account-index-refusal-warning"],
            "the version verdict must never reach the reset, not even by a stray gesture"
        );
    }

    /// The malformed verdict is the only one that may reach the documented
    /// floor, and only behind a confirm that states the residual — so the
    /// first press reveals the confirm and performs nothing.
    #[test]
    fn a_malformed_index_reveals_a_confirm_stating_the_residual_before_it_resets() {
        let mut app = crate::app::tests::test_app();
        let tx = app.tx.clone();

        route(
            &mut app,
            &tx,
            refusal_snapshot(AccountIndexRefusal::Malformed),
        );
        assert_eq!(
            ids(&app.launch),
            vec![
                "account-index-refusal-warning",
                "account-index-reset-button"
            ]
        );
        let texts = |app: &App| -> Vec<String> {
            app.launch.elements().into_iter().map(|e| e.text).collect()
        };
        assert_eq!(
            texts(&app)[0],
            fauna_i18n::strings::onboarding::launch::INDEX_MALFORMED,
            "the shared localized string, not per-app prose"
        );

        // A premature confirm does nothing: the residual has not been shown.
        start_over(&mut app);
        assert!(
            matches!(
                app.launch,
                LaunchSurface::AccountIndexUnreadable {
                    confirming: false,
                    ..
                }
            ),
            "the reset must not run before its residual was stated"
        );

        reveal_start_over(&mut app);
        assert_eq!(
            ids(&app.launch),
            vec![
                "account-index-refusal-warning",
                "account-index-reset-confirm-button"
            ]
        );
        assert_eq!(
            texts(&app)[0],
            fauna_i18n::strings::onboarding::launch::INDEX_MALFORMED_RESET_RESIDUAL,
            "the confirm's surface states what is lost and what is left behind"
        );

        // The confirm is the factory reset, which lands on onboarding.
        start_over(&mut app);
        assert!(
            matches!(app.launch, LaunchSurface::Wizard),
            "the documented floor lands on onboarding: got {:?}",
            app.launch
        );
    }

    /// ⚠ **The floor runs the sign-out's erase, so it owes the sign-out's
    /// refusal** (`account-scoping.md` § Concurrent instances → *An erase
    /// refuses while a sibling serves the account*). Refused,
    /// the confirm resets nothing, keeps showing — closing the other window
    /// and confirming again is the whole remedy — and says why on
    /// `error-message`. Whether the question is answered truthfully is pinned
    /// in `account_scope` over temp bases; this pins that the gesture asks.
    #[test]
    fn a_refused_start_over_resets_nothing_and_says_why() {
        let mut app = crate::app::tests::test_app();
        let tx = app.tx.clone();
        route(
            &mut app,
            &tx,
            refusal_snapshot(AccountIndexRefusal::Malformed),
        );
        reveal_start_over(&mut app);

        let line =
            fauna_i18n::strings::onboarding::launch::INDEX_MALFORMED_RESET_BLOCKED_OTHER_WINDOW;
        let mut asked = false;
        start_over_unless(&mut app, |_| {
            asked = true;
            Some(line.to_string())
        });

        assert!(asked, "the confirm must ask before it resets");
        assert!(
            matches!(
                app.launch,
                LaunchSurface::AccountIndexUnreadable {
                    confirming: true,
                    ..
                }
            ),
            "refused, nothing was reset: got {:?}",
            app.launch
        );
        assert_eq!(
            ids(&app.launch),
            vec![
                "account-index-refusal-warning",
                "account-index-reset-confirm-button"
            ],
            "the confirm stays, so the user can try again once the window is closed"
        );
        assert_eq!(app.launch.error_text().as_deref(), Some(line));

        // The other window closed: the same confirm now runs the floor.
        start_over_unless(&mut app, |_| None);
        assert!(
            matches!(app.launch, LaunchSurface::Wizard),
            "got {:?}",
            app.launch
        );
    }

    /// Only the armed confirm asks — a stray gesture on any other surface
    /// neither resets nor probes.
    #[test]
    fn an_unarmed_start_over_does_not_even_ask() {
        let mut app = crate::app::tests::test_app();
        let tx = app.tx.clone();
        route(
            &mut app,
            &tx,
            refusal_snapshot(AccountIndexRefusal::Malformed),
        );
        start_over_unless(&mut app, |_| panic!("asked before the residual was stated"));
        assert!(matches!(
            app.launch,
            LaunchSurface::AccountIndexUnreadable {
                confirming: false,
                error: None,
                ..
            }
        ));
    }

    /// The launch machine's `superseded` verdict, arriving while THIS device's
    /// own succession ceremony is in flight, is the ceremony's consequence and
    /// must not route anywhere — the second channel the lost-reply journey
    /// caught (`App::defer_own_supersession` guards the first). Measured: with
    /// only the session arm guarded, this arm still moved the user to the
    /// import screen and the unknown-outcome message never showed.
    ///
    /// Red-verify by deleting the in-flight arm: the step becomes IdentityImport.
    #[test]
    fn a_superseded_verdict_during_this_devices_own_ceremony_routes_nowhere() {
        let mut app = crate::app::tests::test_app();
        let tx = app.tx.clone();
        app.stolen_ceremony_in_flight = true;
        let surface_before = std::mem::discriminant(&app.launch);
        let snapshot = LaunchSnapshot {
            phase: LaunchPhase::Offline { transient: false },
            superseded_successor: Some("a7".repeat(32)),
            ..LaunchSnapshot::initial()
        };

        route(&mut app, &tx, snapshot.clone());
        assert_ne!(
            app.wizard.machine.step(),
            fauna_onboarding_machine::OnboardingStep::IdentityImport,
            "the ceremony's fold decides — the user must not be moved to import"
        );
        assert_eq!(std::mem::discriminant(&app.launch), surface_before);

        // The fold may land FIRST (measured ~30 ms ahead of this arm): its
        // message on Account still owns the screen until the user leaves it.
        app.stolen_ceremony_in_flight = false;
        app.stolen_outcome_on_screen = true;
        route(&mut app, &tx, snapshot.clone());
        assert_ne!(
            app.wizard.machine.step(),
            fauna_onboarding_machine::OnboardingStep::IdentityImport,
            "an undecidable fold's message must not be replaced by the import screen"
        );

        // The control: with nothing of the ceremony's left on screen it routes.
        app.stolen_outcome_on_screen = false;
        route(&mut app, &tx, snapshot);
        assert_eq!(
            app.wizard.machine.step(),
            fauna_onboarding_machine::OnboardingStep::IdentityImport
        );
    }

    /// The `superseded`-refusal affordance (`identity-succession.md`
    /// § Propagation → *Own device fleet*). Three properties, each of which was
    /// a real hole before this track: the user reaches the import flow at all,
    /// the reason is on that page's `error-message`, and the CLAIMED successor
    /// is nowhere in what they are told.
    #[test]
    fn a_superseded_refusal_lands_on_the_import_screen_with_the_reason() {
        let mut app = crate::app::tests::test_app();
        let tx = app.tx.clone();
        let claimed = "a7".repeat(32);

        route_superseded_to_import(&mut app, &tx, claimed.clone());

        assert_eq!(
            app.wizard.machine.step(),
            fauna_onboarding_machine::OnboardingStep::IdentityImport,
            "the affordance IS the existing import flow — the user must land on it"
        );
        assert!(
            matches!(app.launch, LaunchSurface::Wizard),
            "the wizard owns the screen once we route into it"
        );
        // Read through `error_text()` — the accessor `error-message` itself
        // renders (`app.rs`'s Wizard arm) — rather than either backing field, so
        // the assertion survives the reason moving between wizard-local and
        // machine state (it moved to the machine so linux shares the transition).
        let shown = app.wizard.error_text().expect("the reason must be shown");
        assert_eq!(
            shown,
            fauna_i18n::strings::onboarding::launch::IDENTITY_SUPERSEDED,
            "the shared localized string, not per-app prose"
        );
        // The load-bearing negative: the nest is enforcer and distributor, never
        // authorizer, so an UNVERIFIED successor must not be presented as fact.
        assert!(
            !shown.contains(&claimed),
            "the claimed successor must not be shown before the chain verifies it"
        );
    }

    /// The deferred-DNS hydration entry: seed the identity, then reseed the
    /// wizard from the slot so `wizard_outcome()` is `AwaitingManualDns` — which
    /// is what paints the "Almost ready" surface. The records must survive the
    /// round trip through the slot's opaque JSON, or the user relaunches into a
    /// surface with nothing to add at their registrar.
    #[test]
    fn awaiting_manual_dns_entry_seeds_the_wizard_onto_the_almost_ready_surface() {
        let mut app = crate::app::tests::test_app();
        let record = fauna_launch_machine::AwaitingDnsRecord {
            nest_url: "https://nest.example".into(),
            handle: "admin".into(),
            dns_records_json:
                r#"[{"record_type":"A","name":"@","value":"203.0.113.7","ttl":300,"priority":null}]"#
                    .into(),
            claim_code: "claim-abc".into(),
            reach_ipv4: None,
            nest_actor_id: None,
        };
        crate::session::persist_awaiting_dns(&app, &"11".repeat(32), &record)
            .expect("persisting the deferred nest");

        route_wizard_entry(&mut app, LaunchWizardEntry::AwaitingManualDns);

        assert!(
            app.wizard.is_awaiting_manual_dns(),
            "the entry must land the wizard on the 'Almost ready' surface"
        );
        let snap = app.wizard.machine.awaiting_manual_dns_snapshot();
        assert_eq!(
            snap.dns_records.len(),
            1,
            "the records must survive the slot"
        );
        assert_eq!(snap.dns_records[0].value, "203.0.113.7");
    }

    /// `onboarding.md` § App-launch routing (transient row) + ui.yaml
    /// `onboarding.launch_retry.elements`.
    #[test]
    fn transient_retry_paints_exactly_the_four_ui_yaml_elements() {
        let surface = LaunchSurface::TransientRetry {
            error: "connection refused".to_string(),
            recover_boxes: Vec::new(),
        };
        assert_eq!(
            ids(&surface),
            vec![
                "launch-transient-error",
                "launch-retry-button",
                "launch-fallthrough-button",
                "launch-retire-button",
            ]
        );
        // The transient surface never fills `error-message` — its error has a
        // dedicated element.
        assert_eq!(surface.error_text(), None);
    }

    /// `route`'s `other =>` catch-all (a phase that should be unreachable
    /// after `start()`, e.g. `Boot`) must land an EMPTY `error` — never a
    /// hand-rolled `format!("unexpected launch state: {other:?}")` Debug
    /// dump — so `transient_error_text` falls back to the
    /// localized generic retry copy instead of untranslatable Rust debug
    /// output landing on a real user's screen.
    #[test]
    fn an_unexpected_phase_after_start_falls_back_to_the_localized_retry_text() {
        let mut app = crate::app::tests::test_app();
        let tx = app.tx.clone();

        route(&mut app, &tx, LaunchSnapshot::initial());

        let LaunchSurface::TransientRetry { error, .. } = &app.launch else {
            panic!(
                "expected TransientRetry for an unreachable Boot phase, got {:?}",
                app.launch
            );
        };
        assert!(
            error.is_empty(),
            "must stay empty so the shared renderer falls back to the localized generic \
             retry text, not a raw Debug dump: {error:?}"
        );
    }

    /// `account-scoping.md` § Concurrent instances → "the colliding
    /// instance's surface" + ui.yaml `onboarding.launch_instance_chooser`
    /// (minus `launch-instance-add-account-button`, platform-scoped to
    /// windows/linux — tui's declared 5th platform absence).
    #[test]
    fn instance_chooser_paints_the_anchor_one_row_per_choice_and_focus_existing() {
        let surface = LaunchSurface::InstanceChooser {
            served_label: "@ana".to_string(),
            served_actor: "a".repeat(64),
            choices: vec![
                ("b".repeat(64), "@bo".to_string()),
                ("c".repeat(64), "@cy".to_string()),
            ],
            error: None,
        };
        assert_eq!(
            ids(&surface),
            vec![
                "launch-instance-chooser",
                "launch-instance-chooser-item",
                "launch-instance-chooser-item",
                "launch-instance-focus-existing-button",
            ],
            "one row per offered account, no add-account button on tui"
        );
        assert_eq!(
            surface.error_text(),
            None,
            "silent until a pick loses the race"
        );
    }

    /// The three gates of [`collision_chooser_surface`], pinned deterministically
    /// (`account-scoping.md` § Concurrent instances → the colliding instance's
    /// surface). The e2e leg needs two live drivers and a real nest, so it is
    /// load-sensitive by construction and cannot be the only statement of a
    /// contract this sharp (testing.md § conventions point 14).
    mod collision_gates {
        use super::*;

        const SERVED: &str = "aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11";
        const FREE: &str = "bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22";

        fn entries() -> Vec<(String, Option<String>)> {
            vec![
                (SERVED.to_string(), Some("ana".to_string())),
                (FREE.to_string(), Some("bo".to_string())),
            ]
        }

        /// Hold `SERVED`'s lock under a throwaway base, so the collision
        /// condition is genuinely true rather than assumed.
        fn served_world() -> (
            tempfile::TempDir,
            fauna_client_accounts::AccountInstanceLock,
        ) {
            let dir = tempfile::tempdir().unwrap();
            let lock = match fauna_client_accounts::AccountInstanceLock::acquire(dir.path(), SERVED)
            {
                fauna_client_accounts::InstanceLockOutcome::Held(l) => l,
                _ => panic!("the served account's lock must be held"),
            };
            (dir, lock)
        }

        /// Gate 1 — **the regression this pins is a real defect that shipped**:
        /// `start_or_offer_chooser` had no bound-launch gate, so a
        /// `FAUNA_BOUND_ACCOUNT` launch onto an already-served account reached
        /// the chooser instead of the instance guard's terminal refusal, and
        /// `test_account_instance_lock_tui.py`'s bound-onto-served case was red
        /// on `origin/main`. The chooser is strictly a human affordance; wired
        /// IPC must be deterministic.
        #[test]
        fn a_bound_launch_never_offers_the_chooser_even_when_its_account_is_served() {
            let (dir, _held) = served_world();
            assert!(
                collision_chooser_surface(
                    Some(dir.path()),
                    Some(SERVED),
                    Some(SERVED),
                    &entries(),
                )
                .is_none(),
                "a bound collision must fall through to the guard's terminal \
                 refusal, never render the chooser"
            );
        }

        /// The non-vacuity proof for the gate-1 test above: same base, same
        /// served account, binding removed — the chooser *does* render, so the
        /// `None` above is the binding's doing and not a dead collision check.
        #[test]
        fn a_plain_launch_onto_a_served_account_offers_the_chooser() {
            let (dir, _held) = served_world();
            let surface =
                collision_chooser_surface(Some(dir.path()), None, Some(SERVED), &entries())
                    .expect("a plain collision must render the chooser");
            match surface {
                LaunchSurface::InstanceChooser {
                    served_label,
                    served_actor,
                    choices,
                    error,
                } => {
                    assert_eq!(served_actor, SERVED, "focus-existing re-probes this id");
                    assert_eq!(
                        served_label,
                        fauna_core::format::account_display_label(Some("ana"), SERVED),
                        "the collided account labels the header, through the same \
                         shared formatter the switcher uses"
                    );
                    assert_eq!(
                        choices
                            .iter()
                            .map(|(id, _)| id.as_str())
                            .collect::<Vec<_>>(),
                        vec![FREE],
                        "only not-currently-served accounts are offerable — the \
                         served account must never be offered"
                    );
                    assert!(error.is_none(), "silent until a pick loses the race");
                }
                other => panic!("expected the chooser surface, got {other:?}"),
            }
        }

        /// Gate 3 — a free account is not a collision, so routing proceeds.
        #[test]
        fn a_plain_launch_onto_a_free_account_routes_normally() {
            let dir = tempfile::tempdir().unwrap();
            assert!(
                collision_chooser_surface(Some(dir.path()), None, Some(SERVED), &entries())
                    .is_none(),
                "nobody serves the account — ordinary routing"
            );
        }

        /// Gate 2 — a fresh install (no active account, or no resolvable base)
        /// has nothing to collide with *and* nothing to offer.
        #[test]
        fn no_active_account_or_no_base_never_offers_the_chooser() {
            let (dir, _held) = served_world();
            assert!(
                collision_chooser_surface(Some(dir.path()), None, None, &entries()).is_none(),
                "no store-active account — nothing to collide with"
            );
            assert!(
                collision_chooser_surface(None, None, Some(SERVED), &entries()).is_none(),
                "no resolvable install base — cannot tell free from served, so \
                 decline the collision rather than offer everything"
            );
        }
    }

    /// Picking a row hands back that row's **actor id** as the gesture's
    /// payload, not its display label — mirrors linux's
    /// `activating_a_row_reports_its_actor_id`.
    #[test]
    fn instance_chooser_items_carry_their_actor_id_as_the_pick_gesture() {
        use crate::element::{Gesture, Role};
        let surface = LaunchSurface::InstanceChooser {
            served_label: "@ana".to_string(),
            served_actor: "a".repeat(64),
            choices: vec![("b".repeat(64), "@bo".to_string())],
            error: None,
        };
        let items: Vec<_> = surface
            .elements()
            .into_iter()
            .filter(|e| e.id == "launch-instance-chooser-item")
            .collect();
        assert_eq!(items.len(), 1);
        match &items[0].role {
            Role::Button(Gesture::Launch(LaunchAction::PickInstance(actor_id))) => {
                assert_eq!(actor_id, &"b".repeat(64));
            }
            other => panic!("expected a PickInstance gesture, got {other:?}"),
        }
    }

    /// Every registered account already served elsewhere: the row list is
    /// empty but the exit survives — a chooser with nothing to pick must
    /// still let the user reach the running instance.
    #[test]
    fn instance_chooser_with_no_free_accounts_keeps_focus_existing() {
        let surface = LaunchSurface::InstanceChooser {
            served_label: "@ana".to_string(),
            served_actor: "a".repeat(64),
            choices: Vec::new(),
            error: None,
        };
        assert_eq!(
            ids(&surface),
            vec![
                "launch-instance-chooser",
                "launch-instance-focus-existing-button",
            ]
        );
        assert!(
            surface.description().len() >= 2,
            "must explain nothing is free"
        );
    }

    /// A pick that lost the race surfaces on the canonical `error-message`,
    /// same slot `NeedsUpdate` uses — every other choice on the surface stays
    /// valid (the surface itself is never torn down).
    #[test]
    fn instance_chooser_reports_a_lost_pick_on_error_message() {
        let surface = LaunchSurface::InstanceChooser {
            served_label: "@ana".to_string(),
            served_actor: "a".repeat(64),
            choices: vec![("b".repeat(64), "@bo".to_string())],
            error: Some("that account was just opened".to_string()),
        };
        assert_eq!(
            surface.error_text(),
            Some("that account was just opened".to_string())
        );
    }

    /// The surviving-device recovery entry (`box-recovery.md` § Recovery UI
    /// (step 4)) is gated on the launch-time box read finding **≥1 custodied
    /// box** — the same gate web and linux apply.
    ///
    /// The gate is the whole point: this surface paints precisely when the saved
    /// nest is unreachable, which is also when the read that would populate the
    /// hub may return nothing. An ungated CTA would walk the admin from a broken
    /// launch into an empty recovery hub — a dead end dressed as a fix.
    #[test]
    fn launch_recover_button_appears_only_once_a_custodied_box_is_known() {
        let no_boxes = LaunchSurface::TransientRetry {
            error: "connection refused".to_string(),
            recover_boxes: Vec::new(),
        };
        assert!(
            !ids(&no_boxes).contains(&"launch-recover-button".to_string()),
            "no custodied box known ⇒ no recovery CTA"
        );
        // Retiring needs only a cloud token, so its entry is not gated on the
        // box read (`nest-retirement.md` § Layout & flow: "always shown").
        assert!(
            ids(&no_boxes).contains(&"launch-retire-button".to_string()),
            "the retire entry is always on the retry surface"
        );

        let with_box = LaunchSurface::TransientRetry {
            error: "connection refused".to_string(),
            recover_boxes: vec!["aa".repeat(32)],
        };
        assert_eq!(
            ids(&with_box),
            vec![
                "launch-transient-error",
                "launch-retry-button",
                "launch-fallthrough-button",
                "launch-retire-button",
                "launch-recover-button",
            ],
            "a custodied box reveals the recovery CTA, after the existing four"
        );
    }

    /// `onboarding.md:544` — the nest authoritatively reports it is outdated:
    /// NON-retry surface, message in `error-message`, no `launch-retry-button`.
    #[test]
    fn needs_update_omits_the_retry_button_and_uses_error_message() {
        let surface = LaunchSurface::NeedsUpdate {
            error: "This nest is running an outdated version".to_string(),
        };
        assert_eq!(ids(&surface), vec!["launch-fallthrough-button"]);
        assert!(
            !ids(&surface).iter().any(|i| i == "launch-retry-button"),
            "retrying an outdated nest is futile — the doc forbids the CTA"
        );
        assert_eq!(
            surface.error_text().as_deref(),
            Some("This nest is running an outdated version")
        );
    }

    /// The spinner phase exposes no ID — ui.yaml scopes none, and no peer
    /// client does either.
    #[test]
    fn launching_and_wizard_register_no_elements() {
        assert!(ids(&LaunchSurface::Launching).is_empty());
        assert!(ids(&LaunchSurface::Wizard).is_empty());
        assert_eq!(LaunchSurface::Launching.error_text(), None);
    }

    /// An empty `last_error` still leaves `launch-transient-error` visible —
    /// the driver asserts on its visibility, and an empty label would not
    /// register (the `error-message` lesson).
    #[test]
    fn transient_error_falls_back_to_the_localized_generic() {
        let surface = LaunchSurface::TransientRetry {
            error: String::new(),
            recover_boxes: Vec::new(),
        };
        let element = &surface.elements()[0];
        assert_eq!(element.id, "launch-transient-error");
        assert!(!element.text.is_empty());
    }

    /// Every launch element is either a focusable button or the error label, so
    /// the keyboard ring can always reach Retry / fallthrough / retire.
    #[test]
    fn every_cta_is_focusable_and_the_error_is_not() {
        let surface = LaunchSurface::TransientRetry {
            error: "boom".to_string(),
            recover_boxes: Vec::new(),
        };
        let focusable: Vec<String> = surface
            .elements()
            .iter()
            .filter(|e| e.focusable())
            .map(|e| e.id.clone())
            .collect();
        assert_eq!(
            focusable,
            vec![
                "launch-retry-button",
                "launch-fallthrough-button",
                "launch-retire-button"
            ]
        );
    }
}
