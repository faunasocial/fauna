//! The Settings → **Bluesky** sub-page (`ui/atproto.md`). One page answers
//! one question: *how deep is this user's Bluesky integration?* The spine is
//! the four-rung **integration-depth selector**; every other control reveals
//! below it as a sub-setting of the level that makes it meaningful:
//!
//! - **selector** (`atproto-depth-*`) — one ordered choice; selecting a
//!   *different* level stages the transition card, never mutates the level
//!   directly (the one exception, Off → Linked, is effect-free and applies on
//!   select — the machine decides this, not this file). Hosted rungs
//!   grey-with-reason on a non-public domain.
//! - **transition card** (`atproto-depth-confirm-card`) — the composed effect
//!   lines, rendered *verbatim* from the machine's `pending_transition.lines`,
//!   plus the history-backfill opt-in on a minting move and confirm/cancel.
//! - **Linked-account panel** — the consume-side link surface at level =
//!   `linked`: the shared `bridge-link-form`/`bridge-card` components,
//!   embedded verbatim via `crate::bridges::embed_bridge_card` (zero new
//!   element IDs — `ui/atproto.md` § Element IDs). Behavior stays owned by
//!   `behavior/bridges.md`.
//! - **hosted panel** — pre-mint: the DID-method radio + the either-way handle
//!   line; post-mint: the identity summary (`atproto-hosted-handle`).
//! - **full-PDS panel** — the F1 login-plane surface (app credentials, the
//!   connected-apps list, the external-apps kill-switch), gated on level =
//!   `hosted_full`, plus the **D10 authoring-delegation row**
//!   (`atproto-delegation-*`). That row answers a different question from the
//!   rest of the panel and the two must not blur: everything above it governs
//!   who may *sign in*; the delegation governs whether a signed-in app may
//!   *post as you*. Signing in is not authorization to author.
//! - **delete presence** (`atproto-delete-presence`) — visible whenever a
//!   hosted identity exists; the destructive flow itself is S5-scoped.
//!
//! A paint shell over the shared `AtprotoSettingsMachine`
//! (`libs/fauna-atproto-settings-machine`), consumed directly like linux's
//! `settings/atproto.rs` (tui is native Rust, not FFI-mediated — priority #2).
//! Unlike linux's push-observer render loop, this follows the Mail/Devices
//! sub-page shape already established in this module: an awaited nav-edge
//! hydrate + re-snapshot after every mutating gesture, no live observer tick
//! (`NoopObserver` below is wired but never fires a repaint itself). The
//! selector's `select_level`/`confirm_transition` gestures are async (a nest
//! round trip) and go through an `Op`; `cancel_transition`/`set_did_method`/
//! `set_history_backfill` are pure local machine mutations (no nest call) and
//! are applied synchronously in `apply_local` itself, mirroring linux's direct
//! `ctx.machine.cancel_transition()` calls.
//!
//! F1 collects no label/dm_allowed input at mint time — no such ui.yaml IDs
//! were approved, only `atproto-app-credential-mint` (a single button). The
//! client auto-labels ("App credential N") and defaults `dm_allowed` to
//! `false` (least privilege, matching the doc's default-closed posture); a
//! labeled/dm_allowed-choosing mint form is a follow-on needing its own ID
//! approval.

use fauna_ui_ids as ids;
use std::collections::HashMap;
use std::sync::Arc;

use fauna_atproto_settings_machine::{
    AppCredentialRow, AtprotoSettingsMachine, AtprotoSettingsObserver, AtprotoSettingsSnapshot,
    DelegationRow, depth_level_options,
};
use fauna_client::NestClient;
use fauna_core::localized::LocalizedText;
use fauna_i18n::strings::{atproto_settings as t, common};

use super::Action;
use crate::app::App;
use crate::element::{Element, Gesture};

fn resolve(text: &LocalizedText) -> String {
    text.clone().resolve(fauna_i18n::strings::lookup)
}

/// [`AtprotoSettingsObserver`] is only a construction requirement — the
/// machine notifies it synchronously after every mutation, but this page reads
/// a fresh `snapshot()` right after each awaited `Op` instead (the
/// Mail/Devices shape), so there is nothing for the callback to do.
struct NoopObserver;

impl AtprotoSettingsObserver for NoopObserver {
    fn on_changed(&self) {}
}

/// The AT Protocol sub-page's state.
///
/// The machine is built once at the post-auth hook (`attach_session`), the
/// same reasoning as [`super::mail::MailState`]: construction is sync and
/// cheap, and holding it as an `Arc` is what lets an `Op` carry it across a
/// `tokio::spawn`. Session-scoped: `clear_session` drops it.
#[derive(Default)]
pub struct AtprotoState {
    /// The shared machine. `None` pre-login, or if construction failed (an
    /// undecodable secret — see [`Self::build`]).
    pub machine: Option<Arc<AtprotoSettingsMachine>>,
    /// The last snapshot the page painted. `None` until the nav-edge hydrate
    /// folds one — the page then renders off [`AtprotoSettingsSnapshot::
    /// default`] (the same pre-fetch shape the real machine starts from:
    /// level `off`, hosted gate closed, kill-switch on), never a bespoke
    /// "nothing yet" branch.
    pub snapshot: Option<AtprotoSettingsSnapshot>,
    /// Secrets revealed (by mint or explicit reveal) this session, keyed by
    /// `credential_id`. Never persisted, never part of the snapshot (D3) —
    /// purely local UI state so a revealed secret survives an unrelated
    /// re-render. Mirrors linux's `AtprotoPageCtx::revealed`.
    pub revealed: HashMap<String, String>,
}

impl AtprotoState {
    /// Build the shared machine over `nest`'s authenticated connection. A
    /// build failure (an undecodable `secret_hex`) is not fatal to the rest of
    /// Settings — the AT Protocol page simply renders no machine-backed surface
    /// and its nav edge produces no `Op` (mirrors linux's `wire_machine`
    /// no-op-when-unregistered shape).
    ///
    /// `runtime` is this session's account-runtime slot: the minted
    /// app-credential secrets rest on the account plane
    /// (`fauna.state.atproto`), reached through the shared seam every
    /// runtime-hosting app wires, which reads the slot fresh on every call.
    pub fn build(
        nest: Arc<NestClient>,
        secret_hex: &str,
        alerts: Arc<fauna_client_alerts::CriticalAlerts>,
        runtime: super::AccountRuntimeSlot,
    ) -> Self {
        let observer: Arc<dyn AtprotoSettingsObserver> = Arc::new(NoopObserver);
        let seams = consent_grant_seams(secret_hex, Arc::clone(&nest), runtime.clone());
        let machine = match crate::mail_glue::build_atproto_settings_machine(
            nest, secret_hex, observer, alerts,
        ) {
            Ok(m) => {
                m.set_identity_store(identity_door(Arc::clone(&runtime)));
                m.set_credential_store(credential_door(Arc::clone(&runtime)));
                if let Some(seams) = seams {
                    m.set_consent_grant_seams(seams);
                }
                Some(m)
            }
            Err(e) => {
                tracing::error!("[settings/atproto] build machine: {e}");
                None
            }
        };
        AtprotoState {
            machine,
            snapshot: None,
            revealed: HashMap::new(),
        }
    }
}

/// The ATProto identity custody door — the account plane's
/// `fauna.state.atproto-identity` (the held senior rotation keys and their
/// custody records), through the shared impl every runtime-hosting app wires,
/// over this app's slot, read fresh per call: a machine or sweep started
/// before the runtime lands answers "cannot verify" until it does. The
/// settings machine and the critical-alert sweep both build it.
pub(crate) fn identity_door(
    runtime: super::AccountRuntimeSlot,
) -> Arc<dyn fauna_client_account_runtime::atproto_identity::AtprotoIdentityStore> {
    Arc::new(
        fauna_client_account_runtime::atproto_identity::RuntimeAtprotoIdentity::new(move || {
            runtime.lock().ok().and_then(|handle| handle.clone())
        }),
    )
}

/// The consent-time grant's seams (`fauna_atproto_settings_machine::
/// consent_grant`): the owner's keypair, the account plane's kind-manifest
/// rows and grant log over this app's slot, read fresh per call, and the
/// owner's folder custody over the same slot plus `nest`'s folder list (the
/// `folder:read` twin and the card's folder name). Both consent machines —
/// this page's card and the Connected apps tray — take the same seams. `None`
/// on an undecodable secret (an identity fault the launch flow already ruled
/// out): an approve of a twin-bearing consent is then refused.
pub(crate) fn consent_grant_seams(
    secret_hex: &str,
    nest: Arc<NestClient>,
    runtime: super::AccountRuntimeSlot,
) -> Option<Arc<fauna_atproto_settings_machine::ConsentGrantSeams>> {
    let keypair = fauna_core::identity::ActorKeypair::from_secret_hex(secret_hex)
        .inspect_err(|e| tracing::error!("[settings] consent grant seams: decode secret_hex: {e}"))
        .ok()?;
    let folders = fauna_atproto_settings_machine::CustodyConsentFolders {
        keys: super::folder_key_door(runtime.clone()),
        nest,
    };
    let door = Arc::new(fauna_client_config::ResolvingLedgerStore::new(move || {
        runtime.lock().ok().and_then(|handle| handle.clone())
    }));
    Some(Arc::new(
        fauna_atproto_settings_machine::ConsentGrantSeams::from_keypair(
            &keypair,
            door.clone(),
            door,
        )
        .with_folders(Arc::new(folders)),
    ))
}

/// The ATProto credential door — the account plane's `fauna.state.atproto`
/// (the minted app-credential secrets), through the shared impl every
/// runtime-hosting app wires, over this app's slot.
pub(crate) fn credential_door(
    runtime: super::AccountRuntimeSlot,
) -> Arc<dyn fauna_atproto_settings_machine::AtprotoCredentialStore> {
    Arc::new(
        fauna_client_account_runtime::atproto_credentials::RuntimeAtprotoCredentials::new(
            move || runtime.lock().ok().and_then(|handle| handle.clone()),
        ),
    )
}

/// The ordered ui.yaml `bluesky` element list. `page-heading` + `atproto-page`
/// (the page landmark) + the four depth rungs always render; every other
/// group reveals by level/state (`ui/atproto.md` § Layout & flow). `app` is
/// needed only for the Linked panel's embed
/// ([`crate::bridges::embed_bridge_card`], which reads the app-wide bridges
/// snapshot `app.bridges`).
pub(super) fn atproto_elements(app: &App, state: &AtprotoState) -> Vec<Element> {
    let mut els = vec![
        Element::label(ids::PAGE_HEADING, t::TITLE),
        Element::label(ids::ATPROTO_PAGE, String::new()),
    ];

    let empty = AtprotoSettingsSnapshot::default();
    let snap = state.snapshot.as_ref().unwrap_or(&empty);

    // ── The recovery-fork contest, ABOVE everything ──────────────────────
    contest_elements(snap, &mut els);

    // ── Depth selector ───────────────────────────────────────────────────
    els.push(Element::label(ids::ATPROTO_DEPTH_SELECTOR, String::new()).attr("state", &snap.level));
    // Rungs, titles, descriptions and the hosted-gate flag all come from the
    // shared catalog (`atproto.md` § Where logic lives already declared the
    // machine the owner of "level logic … all of it").
    for rung in depth_level_options() {
        let active = snap.level == rung.level;
        let gated = rung.hosted && !snap.hosted_allowed;
        // A hosted rung the user is already AT stays selectable so a step-down
        // is reachable even if the domain later stops being public; the gate
        // only ever blocks ENTERING a hosted level from a lower one (mirrors
        // linux's `!gated || active`).
        let enabled = !gated || active;
        let title = resolve(&rung.title);
        let desc = resolve(&rung.description);
        // ui.yaml declares the four rungs `radio` — one-of-N, radio paint. The
        // `state` attr keeps this page's established active/inactive
        // vocabulary (the e2e contract), not the toggle convention's on/off.
        let mut el = Element::radio_gesture(
            rung.ui_id,
            format!("{title} — {desc}"),
            active,
            Gesture::Settings(Action::AtprotoSelectLevel(rung.level)),
        )
        .attr("state", if active { "active" } else { "inactive" })
        .attr("reason", if gated { "gated" } else { "ok" });
        el.enabled = enabled;
        els.push(el);
    }
    if !snap.hosted_allowed
        && let Some(reason) = &snap.hosted_gate_reason
    {
        // Untagged — ui.yaml gives the gate-reason LINE no id of its own; the
        // rung's own `reason` attr is what the e2e reads (`depth_gate_marker`).
        els.push(Element::chrome(resolve(reason)));
    }

    // ── Transition card ──────────────────────────────────────────────────
    if let Some(card) = &snap.pending_transition {
        let lines = card
            .lines
            .iter()
            .map(resolve)
            .collect::<Vec<_>>()
            .join("\n");
        els.push(Element::label(ids::ATPROTO_DEPTH_CONFIRM_CARD, lines));
        if card.show_history_backfill {
            els.push(Element::checkbox_gesture(
                ids::ATPROTO_HISTORY_BACKFILL,
                t::HISTORY_BACKFILL_LABEL,
                snap.history_backfill,
                Gesture::Settings(Action::AtprotoSetHistoryBackfill(!snap.history_backfill)),
            ));
        }
        els.push(Element::gesture_button(
            ids::ATPROTO_DEPTH_CONFIRM,
            t::DEPTH_CONFIRM_BUTTON,
            !card.in_progress,
            Gesture::Settings(Action::AtprotoConfirmTransition),
        ));
        els.push(Element::gesture_button(
            ids::ATPROTO_DEPTH_CANCEL,
            t::DEPTH_CANCEL_BUTTON,
            !card.in_progress,
            Gesture::Settings(Action::AtprotoCancelTransition),
        ));
    }

    // ── Linked-account panel: the shared bridge surface at level = linked ──
    if snap.level == "linked" {
        crate::bridges::embed_bridge_card(app, "bluesky", &mut els);
    }

    // ── Hosted panel: at (or entering) a hosted level ───────────────────
    let targeting_hosted = snap
        .pending_transition
        .as_ref()
        .is_some_and(|p| p.target_level.starts_with("hosted"));
    let at_hosted = snap.level.starts_with("hosted");
    if at_hosted || targeting_hosted {
        // Pre-mint: the DID-method radio + the either-way handle line.
        if snap.show_did_method_radio {
            els.push(Element::label(ids::ATPROTO_DID_METHOD, String::new()));
            // "radio" in the field's own name — one-of-two, radio paint.
            let plc = snap.did_method == "plc";
            els.push(
                Element::radio_gesture(
                    ids::ATPROTO_DID_METHOD_PLC,
                    format!("{} — {}", t::DID_METHOD_PLC_TITLE, t::DID_METHOD_PLC_DESC),
                    plc,
                    Gesture::Settings(Action::AtprotoSetDidMethod("plc".to_string())),
                )
                .attr("state", if plc { "on" } else { "off" }),
            );
            let web = snap.did_method == "web";
            els.push(
                Element::radio_gesture(
                    ids::ATPROTO_DID_METHOD_WEB,
                    format!("{} — {}", t::DID_METHOD_WEB_TITLE, t::DID_METHOD_WEB_DESC),
                    web,
                    Gesture::Settings(Action::AtprotoSetDidMethod("web".to_string())),
                )
                .attr("state", if web { "on" } else { "off" }),
            );
            if !snap.handle_preview.is_empty() {
                els.push(Element::chrome(t::handle_either_way(&snap.handle_preview)));
            }
        }
    }

    // ── The identity summary: gated on the IDENTITY, not on the level ───
    //
    // `ui/atproto.md` § Errors & edge cases: "A deactivated identity at level
    // Off/Linked: the identity summary renders (marked deactivated) so the user
    // can see what re-enabling restores." Every app had this inside the hosted
    // panel's `at_hosted || targeting_hosted` gate, which is the one place the
    // rule can never hold — the states it names are exactly the two the gate
    // closes on. The shared machine already models it correctly (it keys
    // `show_delete_presence` off the identity's status, never the level), and
    // that mismatch is what made the omission invisible: the delete button
    // rendered at level off with nothing above it saying what would be deleted.
    // tui leads the fix; the other six are a trickle-down leg.
    if let Some(id) = &snap.identity {
        let status = resolve(&fauna_atproto_settings_machine::identity_status_label(
            &id.status,
        ));
        els.push(Element::label(
            ids::ATPROTO_HOSTED_HANDLE,
            format!(
                "{} · {} · {status}",
                t::hosted_handle_prefix(&id.handle),
                t::hosted_method_prefix(&id.method),
            ),
        ));
    }

    // ── Delete presence: whenever a hosted identity still stands ────────
    if snap.show_delete_presence {
        els.push(Element::gesture_button(
            ids::ATPROTO_DELETE_PRESENCE,
            t::DELETE_PRESENCE_BUTTON,
            true,
            Gesture::Settings(Action::AtprotoDeletePresence),
        ));
    }
    // Its own confirm card — never the depth selector's (`ui/atproto.md`
    // § User actions row 4). The copy is the machine's, rendered verbatim: the
    // ceremony's promises about what survives are not this file's to word.
    if let Some(card) = &snap.delete_confirm {
        let lines = card
            .lines
            .iter()
            .map(resolve)
            .collect::<Vec<_>>()
            .join("\n");
        els.push(Element::label(ids::ATPROTO_DELETE_CONFIRM_CARD, lines));
        // The terminal opt-in (S5 slice 5b). Always on the card, greyed with
        // the machine's reason where the identity cannot be retired — never a
        // live control that errors on press. `state` carries on/off/unavailable
        // so the three are assertable apart from the label's wording.
        let retire = &card.retire_identity;
        els.push(
            Element::checkbox_gesture(
                ids::ATPROTO_DELETE_TOMBSTONE,
                t::DELETE_RETIRE_IDENTITY_LABEL,
                retire.selected,
                Gesture::Settings(Action::AtprotoSetDeleteRetireIdentity(!retire.selected)),
            )
            .enabled(retire.available && !card.in_progress)
            .attr(
                "state",
                match (retire.available, retire.selected) {
                    (false, _) => "unavailable",
                    (true, true) => "on",
                    (true, false) => "off",
                },
            ),
        );
        if let Some(reason) = &retire.unavailable_reason {
            // Untagged, like the hosted rungs' gate reason: ui.yaml gives the
            // reason line no id of its own.
            els.push(Element::chrome(resolve(reason)));
        }
        els.push(Element::gesture_button(
            ids::ATPROTO_DELETE_CONFIRM,
            t::DELETE_CONFIRM_BUTTON,
            !card.in_progress,
            Gesture::Settings(Action::AtprotoConfirmDelete),
        ));
        els.push(Element::gesture_button(
            ids::ATPROTO_DELETE_CANCEL,
            t::DELETE_CANCEL_BUTTON,
            !card.in_progress,
            Gesture::Settings(Action::AtprotoCancelDelete),
        ));
    }

    // ── Full-PDS panel: gated on level = hosted_full ────────────────────
    if snap.level == "hosted_full" {
        // The consent cards and the connected-apps rows that used to lead and
        // close this panel moved to the Connected apps sub-page on tui (a lift,
        // never a duplication — `ui/connected-apps.md` § Architectural rules).
        els.push(
            Element::checkbox_gesture(
                ids::ATPROTO_EXTERNAL_APPS_ENABLE,
                t::EXTERNAL_APPS_TOGGLE,
                snap.external_apps_enabled,
                Gesture::Settings(Action::AtprotoToggleExternalApps),
            )
            .attr(
                "state",
                if snap.external_apps_enabled {
                    "on"
                } else {
                    "off"
                },
            ),
        );

        els.push(Element::gesture_button(
            ids::ATPROTO_APP_CREDENTIAL_MINT,
            t::MINT_BUTTON,
            true,
            Gesture::Settings(Action::AtprotoMint),
        ));

        els.push(Element::chrome(t::APP_CREDENTIALS_HEADING));
        if snap.credentials.is_empty() {
            els.push(Element::chrome(t::APP_CREDENTIALS_EMPTY));
        }
        for (i, cred) in snap.credentials.iter().enumerate() {
            credential_row_elements(state, cred, i, &mut els);
        }

        delegation_elements(snap.delegation.as_ref(), &mut els);
    }

    // `settings-nav-back` — Esc already returns to the Settings hub
    // (`Action::NavBack => state.sub = SubPage::Root`), but a live user report
    // found it was the ONLY way out on Folders, undiscoverable (user-approved
    // 2026-08-03; matches `account.rs`/`folders.rs`'s existing pattern).
    els.push(
        Element::gesture_button(
            ids::SETTINGS_NAV_BACK,
            common::BACK,
            true,
            Gesture::Settings(Action::NavBack),
        )
        .nav_back(),
    );

    els
}

/// The D10 authoring-delegation row — what authorizes an external ATProto app
/// to *post* as this account, as opposed to merely signing in (the credentials
/// and connected-apps groups above govern that).
///
/// Two states, one always-present control:
///
/// - **`None`** — no delegation, or one whose stored cert failed the
///   client-side verify under the account's own identity key. The row and its
///   leaves are **withheld**, never rendered as a grant the user cannot be
///   shown to have made (the mismatch surfaces on `error-message`, which the
///   machine has already set). Only `atproto-delegation-authorize` renders.
/// - **`Some`** — the leaves render, mirroring the shipped `nest-trust-grant-*`
///   rows: scope, lasts-until, liveness. `-authorize` STAYS rendered, because
///   re-authorizing is the renewal gesture — provisioning overwrites the cert,
///   so a lapsed grant recovers in one gesture with no revoke first.
fn delegation_elements(row: Option<&DelegationRow>, els: &mut Vec<Element>) {
    els.push(Element::chrome(t::DELEGATION_HEADING));

    let Some(row) = row else {
        els.push(Element::chrome(t::DELEGATION_EMPTY));
        els.push(Element::gesture_button(
            ids::ATPROTO_DELEGATION_AUTHORIZE,
            t::DELEGATION_AUTHORIZE_BUTTON,
            true,
            Gesture::Settings(Action::AtprotoAuthorizeDelegation),
        ));
        return;
    };

    els.push(Element::label(ids::ATPROTO_DELEGATION_ROW, String::new()));

    let capabilities = row
        .capability_labels()
        .iter()
        .map(resolve)
        .collect::<Vec<_>>()
        .join(", ");
    els.push(Element::label(
        ids::ATPROTO_DELEGATION_SCOPE,
        t::delegation_scope_prefix(&capabilities),
    ));

    // MICROseconds on this row — these come from the signed cert, not the wire,
    // unlike the credential/session rows above which carry milliseconds.
    let authorized =
        fauna_core::format::format_unix_local((row.authorized_at_micros / 1_000_000) as i64);
    els.push(Element::label(
        ids::ATPROTO_DELEGATION_LASTS_UNTIL,
        match row.expires_at_micros {
            Some(micros) => t::delegation_lasts_until(
                &authorized,
                &fauna_core::format::format_unix_local((micros / 1_000_000) as i64),
            ),
            None => t::delegation_lasts_until_no_expiry(&authorized),
        },
    ));

    // The liveness wire spelling rides the `state` attr so the e2e asserts the
    // STATE, not its prose — an unrecognized spelling from a newer nest still
    // renders (degrade, never fail to decode).
    els.push(
        Element::label(ids::ATPROTO_DELEGATION_STATUS, resolve(&row.status_label()))
            .attr("state", &row.liveness),
    );

    // The ADVISORY last-use hint (D10 § Audit, `atproto-pds-full.md`; ID
    // user-approved 2026-07-31). Every leaf above is derived from the SIGNED
    // cert, verified client-side under the account's own identity key. This one
    // is not: the nest simply asserts it, with nothing signing it. So the
    // wording hedges deliberately ("Last reported use", "No use reported yet")
    // and the leaf carries `advisory=true` — an absent stamp means nothing was
    // REPORTED, never that nothing happened, because a nest that under-reports
    // is exactly what this value cannot detect. The audit surface that IS
    // trustworthy is the feed's `delegated-origin-badge`, read from signed
    // bytes; the hint below points a user there rather than leaving them to
    // trust this number.
    els.push(
        Element::label(
            ids::ATPROTO_DELEGATION_LAST_USED,
            match row.last_used_at_millis {
                Some(millis) => {
                    t::delegation_last_used(&fauna_core::format::format_unix_local(millis / 1000))
                }
                None => t::DELEGATION_LAST_USED_NEVER.to_string(),
            },
        )
        .attr("advisory", "true"),
    );
    els.push(Element::chrome(t::DELEGATION_LAST_USED_HINT));

    // Renewal is the same control, relabeled — never a revoke-then-re-mint.
    els.push(Element::gesture_button(
        ids::ATPROTO_DELEGATION_AUTHORIZE,
        t::DELEGATION_REAUTHORIZE_BUTTON,
        true,
        Gesture::Settings(Action::AtprotoAuthorizeDelegation),
    ));
    els.push(Element::gesture_button(
        ids::ATPROTO_DELEGATION_REVOKE,
        t::DELEGATION_REVOKE_BUTTON,
        true,
        Gesture::Settings(Action::AtprotoRevokeDelegation),
    ));
}

/// One `atproto-app-credential-item` row: title/subtitle joined into the
/// row's own text (F1 has no separate per-field IDs — the same
/// container-descendant-label shape ui.yaml's F1 scope note describes).
/// Reveal is gated on `revealable` (rule 2), shown regardless if this session
/// already revealed it (mint's inline reveal). The reveal/revoke buttons carry
/// `.within(ids::ATPROTO_APP_CREDENTIAL_ITEM, i)` — real ancestor scoping, not
/// positional indexing — because `actions/atproto_settings.py::
/// reveal_credential_secret` reads them via `scope="atproto-app-credential-item[i]"`.
fn credential_row_elements(
    state: &AtprotoState,
    cred: &AppCredentialRow,
    i: usize,
    els: &mut Vec<Element>,
) {
    const ITEM: &str = ids::ATPROTO_APP_CREDENTIAL_ITEM;
    let created = fauna_core::format::format_unix_local(cred.created_at_millis / 1000);
    let subtitle = match cred.last_used_at_millis {
        Some(millis) => format!(
            "{} · {}",
            t::credential_created_prefix(&created),
            t::credential_last_used_prefix(&fauna_core::format::format_unix_local(millis / 1000))
        ),
        None => format!(
            "{} · {}",
            t::credential_created_prefix(&created),
            t::CREDENTIAL_NEVER_USED
        ),
    };
    els.push(Element::label(ITEM, format!("{} — {subtitle}", cred.label)));

    let already_revealed = state.revealed.get(&cred.credential_id).cloned();
    if cred.revealable || already_revealed.is_some() {
        // Already-revealed: the button's own TEXT carries the secret and it
        // disables (the e2e reads `get_text` on this id — no separate
        // secret-display id in F1, `actions/atproto_settings.py::
        // reveal_credential_secret`), matching linux's `set_label` +
        // `set_sensitive(false)`.
        match already_revealed {
            Some(secret) => els.push(
                Element::gesture_button(
                    ids::ATPROTO_APP_CREDENTIAL_REVEAL,
                    secret,
                    false,
                    Gesture::Settings(Action::AtprotoReveal(cred.credential_id.clone())),
                )
                .within(ITEM, i),
            ),
            None => els.push(
                Element::gesture_button(
                    ids::ATPROTO_APP_CREDENTIAL_REVEAL,
                    t::REVEAL_BUTTON,
                    true,
                    Gesture::Settings(Action::AtprotoReveal(cred.credential_id.clone())),
                )
                .within(ITEM, i),
            ),
        }
    }

    els.push(
        Element::gesture_button(
            ids::ATPROTO_APP_CREDENTIAL_REVOKE,
            t::REVOKE_BUTTON,
            true,
            Gesture::Settings(Action::AtprotoRevoke(cred.credential_id.clone())),
        )
        .within(ITEM, i),
    );
}

/// The recovery-fork contest surface (`atproto-contest-*`, IDs user-approved
/// 2026-08-02; mechanism owned by `behavior/atproto-pds-bridge.md` § State &
/// data shape, the recovery-fork contest).
///
/// **It renders at the very top of the page, above the depth selector** — the
/// one group here not revealed by integration level. A user whose identity has
/// been taken over should not have to scroll past four radio buttons to find the
/// only thing that undoes it, and the notice is composed off CLIENT-SIDE
/// evidence alone (the standing custody alarm + this client's own read of the
/// public PLC directory), so it is reachable precisely when a hostile nest is
/// denying the identity.
///
/// **This function derives nothing.** Every string is machine-composed and
/// rendered verbatim; the only decisions here are which ids exist, and they are
/// read straight off the snapshot (`show_contest` for the button, the presence
/// of `contest_confirm` for the ceremony). That is deliberate: the copy names
/// what undoing does NOT restore — the nest keeps the key it publishes with —
/// and seven shells paraphrasing that sentence seven ways is how a user ends up
/// believing a compromised box was fully evicted when it was not.
fn contest_elements(snap: &AtprotoSettingsSnapshot, els: &mut Vec<Element>) {
    let Some(card) = &snap.contest else {
        return;
    };
    els.push(Element::chrome(t::CONTEST_CARD_HEADING));
    els.push(Element::label(ids::ATPROTO_CONTEST_CARD, String::new()).attr("state", &card.state));
    els.push(Element::label(
        ids::ATPROTO_CONTEST_DETAIL,
        resolve(&card.detail),
    ));
    if let Some(deadline) = &card.deadline {
        els.push(Element::label(
            ids::ATPROTO_CONTEST_DEADLINE,
            resolve(deadline),
        ));
    }
    // Decision 2: the button exists only where pressing it can work. The three
    // states are honest states, not three renderings of one control.
    if card.show_contest {
        els.push(Element::gesture_button(
            ids::ATPROTO_CONTEST,
            t::CONTEST_BUTTON,
            true,
            Gesture::Settings(Action::AtprotoOpenContestConfirm),
        ));
    }
    let Some(confirm) = &snap.contest_confirm else {
        return;
    };
    let lines = confirm
        .lines
        .iter()
        .map(resolve)
        .collect::<Vec<_>>()
        .join("\n");
    els.push(Element::label(ids::ATPROTO_CONTEST_CONFIRM_CARD, lines));
    els.push(Element::gesture_button(
        ids::ATPROTO_CONTEST_CONFIRM,
        t::CONTEST_CONFIRM_BUTTON,
        !confirm.in_progress,
        Gesture::Settings(Action::AtprotoRequestContest),
    ));
    els.push(Element::gesture_button(
        ids::ATPROTO_CONTEST_CANCEL,
        t::CONTEST_CANCEL_BUTTON,
        !confirm.in_progress,
        Gesture::Settings(Action::AtprotoCancelContest),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::authed_app;
    use crate::element::Role;
    use fauna_atproto_settings_machine::{
        ContestCardRow, ContestConfirmCardModel, DeleteConfirmCardModel, IdentitySummaryRow,
        RetireIdentityOptIn, TransitionCardModel,
    };

    fn credential(
        id: &str,
        label: &str,
        revealable: bool,
        last_used: Option<i64>,
    ) -> AppCredentialRow {
        AppCredentialRow {
            credential_id: id.to_string(),
            label: label.to_string(),
            dm_allowed: false,
            created_at_millis: 1_700_000_000_000,
            last_used_at_millis: last_used,
            revealable,
        }
    }

    fn snap(f: impl FnOnce(&mut AtprotoSettingsSnapshot)) -> AtprotoSettingsSnapshot {
        let mut s = AtprotoSettingsSnapshot::default();
        f(&mut s);
        s
    }

    fn ids(app: &App) -> Vec<String> {
        atproto_elements(app, &app.settings.atproto)
            .into_iter()
            .map(|e| e.id)
            .collect()
    }

    /// Read one of an element's `attrs` (a `Vec` of pairs, not a map).
    fn attr<'a>(el: &'a Element, key: &str) -> Option<&'a str> {
        el.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn app_with_snapshot(snapshot: AtprotoSettingsSnapshot) -> App {
        let mut app = authed_app();
        app.settings.atproto.snapshot = Some(snapshot);
        app
    }

    #[test]
    fn pre_hydrate_paints_only_the_shell_and_the_depth_selector() {
        // Pre-hydrate renders off `AtprotoSettingsSnapshot::default()`: level
        // `off`, hosted rungs gated **with their reason line** (the default
        // carries one — see `snapshots.rs`'s
        // `the_prefetch_default_closes_the_hosted_gate_and_says_why`), no
        // full-PDS panel (that's hosted_full-only now). The reason is ID-less
        // chrome, so it lands here as the empty id after the last rung.
        let app = authed_app();
        let got = ids(&app);
        assert_eq!(
            got,
            [
                "page-heading",
                "atproto-page",
                "atproto-depth-selector",
                "atproto-depth-off",
                "atproto-depth-linked",
                "atproto-depth-hosted-visible",
                "atproto-depth-hosted-full",
                "",
                "settings-nav-back",
            ]
        );
    }

    /// Rule 5 on the very first paint: both hosted rungs come up DIM before
    /// anything has been fetched, so the screen owes the user a reason there —
    /// the one window `gate_reason` (which names the offending domain) cannot
    /// speak for, because no domain has been fetched to name.
    #[test]
    fn the_pre_hydrate_hosted_rungs_are_dim_and_the_page_says_why() {
        let app = authed_app();
        let els = atproto_elements(&app, &app.settings.atproto);
        for id in ["atproto-depth-hosted-visible", "atproto-depth-hosted-full"] {
            let rung = els.iter().find(|e| e.id == id).expect("rung paints");
            assert!(!rung.enabled, "{id} is gated before the fetch answers");
            assert_eq!(attr(rung, "reason"), Some("gated"));
        }
        assert!(
            els.iter()
                .any(|e| e.text == fauna_i18n::strings::atproto_settings::GATE_REASON_PENDING),
            "the greyed rungs must carry their reason on the first paint too"
        );
    }

    #[test]
    fn off_level_leaves_off_and_linked_selectable_and_hosted_rungs_gated() {
        let app = app_with_snapshot(snap(|s| {
            s.level = "off".to_string();
            s.hosted_allowed = false;
        }));
        let els = atproto_elements(&app, &app.settings.atproto);
        let rung = |id: &str| els.iter().find(|e| e.id == id).unwrap();
        assert!(rung("atproto-depth-off").enabled);
        assert!(rung("atproto-depth-linked").enabled);
        assert!(!rung("atproto-depth-hosted-visible").enabled);
        assert!(!rung("atproto-depth-hosted-full").enabled);
        assert_eq!(
            rung("atproto-depth-hosted-visible")
                .attrs
                .iter()
                .find(|(k, _)| k == "reason")
                .map(|(_, v)| v.as_str()),
            Some("gated")
        );
        assert_eq!(
            els.iter()
                .find(|e| e.id == "atproto-depth-selector")
                .unwrap()
                .attrs
                .iter()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.as_str()),
            Some("off")
        );
        // No full-PDS panel, no Linked panel, no hosted panel at Off.
        assert!(!els.iter().any(|e| e.id == "atproto-app-credential-mint"));
        assert!(!els.iter().any(|e| e.id == "bridge-action-button"));
        assert!(!els.iter().any(|e| e.id == "atproto-hosted-handle"));
    }

    // ── The recovery-fork contest ceremony (`atproto-contest-*`) ───────────

    fn contest(state: &str, show_contest: bool) -> ContestCardRow {
        ContestCardRow {
            state: state.to_string(),
            detail: LocalizedText::key("detail line"),
            deadline: Some(LocalizedText::key("deadline line")),
            show_contest,
        }
    }

    /// The page-lead rule (`ui/atproto.md` § Element IDs): an identity under
    /// attack outranks every settings row below it, so the card renders ABOVE
    /// the depth selector — not somewhere the user has to scroll to find the
    /// only control that undoes a takeover.
    #[test]
    fn the_contest_card_leads_the_page_above_the_selector() {
        let app = app_with_snapshot(snap(|s| {
            s.level = "hosted_visible".to_string();
            s.contest = Some(contest("contestable", true));
        }));
        let els = atproto_elements(&app, &app.settings.atproto);
        let ids: Vec<&str> = els.iter().map(|e| e.id.as_str()).collect();
        let card = ids
            .iter()
            .position(|id| *id == "atproto-contest-card")
            .expect("the card renders on a standing violation");
        let selector = ids
            .iter()
            .position(|id| *id == "atproto-depth-selector")
            .unwrap();
        assert!(card < selector, "the contest leads the page: {ids:?}");
        assert_eq!(
            els[card]
                .attrs
                .iter()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.as_str()),
            Some("contestable")
        );
        assert!(els.iter().any(|e| e.id == "atproto-contest-detail"));
        assert!(els.iter().any(|e| e.id == "atproto-contest-deadline"));
        assert!(els.iter().any(|e| e.id == "atproto-contest"));
    }

    /// Decision 2: the three states are honest states, not three renderings of
    /// one control. A user whose identity cannot be recovered is told so — and
    /// never left clicking a button that cannot work.
    #[test]
    fn a_hopeless_state_renders_the_notice_but_no_button() {
        for state in ["window-closed", "not-contestable"] {
            let app = app_with_snapshot(snap(|s| {
                s.level = "hosted_visible".to_string();
                s.contest = Some(contest(state, false));
            }));
            let els = atproto_elements(&app, &app.settings.atproto);
            assert!(
                els.iter().any(|e| e.id == "atproto-contest-card"),
                "{state}: the user is still told what happened"
            );
            assert!(
                !els.iter().any(|e| e.id == "atproto-contest"),
                "{state}: …but there is no dead button"
            );
        }
    }

    /// The ceremony is machine state, so the page renders exactly what the
    /// machine says — including the in-flight disable that stops a second press
    /// signing a second fork.
    #[test]
    fn the_confirm_card_renders_verbatim_and_gates_both_controls_on_in_progress() {
        let app = app_with_snapshot(snap(|s| {
            s.level = "hosted_visible".to_string();
            s.contest = Some(contest("contestable", true));
            s.contest_confirm = Some(ContestConfirmCardModel {
                lines: vec![
                    LocalizedText::key("what is undone"),
                    LocalizedText::key("what is signed"),
                ],
                in_progress: true,
            });
        }));
        let els = atproto_elements(&app, &app.settings.atproto);
        let card = els
            .iter()
            .find(|e| e.id == "atproto-contest-confirm-card")
            .expect("the confirm card renders while the ceremony is open");
        assert!(
            card.text.contains("what is undone") && card.text.contains("what is signed"),
            "the page renders the machine's lines verbatim: {:?}",
            card.text
        );
        assert!(
            !els.iter()
                .find(|e| e.id == "atproto-contest-confirm")
                .unwrap()
                .enabled,
            "in_progress disables confirm — one press, one fork"
        );
        assert!(
            !els.iter()
                .find(|e| e.id == "atproto-contest-cancel")
                .unwrap()
                .enabled,
            "…and cancel, which cannot recall a fork already on the wire"
        );
    }

    /// The overwhelmingly common state: nothing. A settings page must not carry
    /// a compromise notice it cannot justify.
    #[test]
    fn no_violation_renders_no_contest_surface_at_all() {
        let app = app_with_snapshot(snap(|s| s.level = "hosted_visible".to_string()));
        let els = atproto_elements(&app, &app.settings.atproto);
        assert!(!els.iter().any(|e| e.id.starts_with("atproto-contest")));
    }

    #[test]
    fn an_already_hosted_rung_stays_selectable_even_when_gated() {
        // Mirrors linux: a hosted rung the user is ALREADY at must stay
        // reachable (for stepping down) even if the domain later stops being
        // public.
        let app = app_with_snapshot(snap(|s| {
            s.level = "hosted_visible".to_string();
            s.hosted_allowed = false;
        }));
        let els = atproto_elements(&app, &app.settings.atproto);
        assert!(
            els.iter()
                .find(|e| e.id == "atproto-depth-hosted-visible")
                .unwrap()
                .enabled
        );
    }

    #[test]
    fn a_pending_transition_renders_the_card_verbatim_and_gates_confirm_on_in_progress() {
        let app = app_with_snapshot(snap(|s| {
            s.level = "off".to_string();
            s.pending_transition = Some(TransitionCardModel {
                target_level: "hosted_visible".to_string(),
                lines: vec![
                    LocalizedText::key("line one"),
                    LocalizedText::key("line two"),
                ],
                show_history_backfill: true,
                in_progress: true,
            });
        }));
        let els = atproto_elements(&app, &app.settings.atproto);
        let card = els
            .iter()
            .find(|e| e.id == "atproto-depth-confirm-card")
            .expect("the card renders while a transition is pending");
        assert!(card.text.contains("line one") && card.text.contains("line two"));
        assert!(els.iter().any(|e| e.id == "atproto-history-backfill"));
        assert!(
            !els.iter()
                .find(|e| e.id == "atproto-depth-confirm")
                .unwrap()
                .enabled,
            "in_progress disables confirm"
        );
        assert!(
            !els.iter()
                .find(|e| e.id == "atproto-depth-cancel")
                .unwrap()
                .enabled,
            "in_progress disables cancel"
        );
    }

    #[test]
    fn no_pending_transition_renders_no_card() {
        let app = app_with_snapshot(snap(|s| s.level = "off".to_string()));
        let els = atproto_elements(&app, &app.settings.atproto);
        assert!(!els.iter().any(|e| e.id == "atproto-depth-confirm-card"));
    }

    #[test]
    fn linked_level_embeds_the_shared_bridge_card() {
        let mut app = app_with_snapshot(snap(|s| s.level = "linked".to_string()));
        app.bridges.nest = Some(NestClient::new(
            "http://127.0.0.1:9".to_string(),
            fauna_core::identity::ActorKeypair::generate(),
        ));
        // No "bluesky" row in the fetch yet — `embed_bridge_card` still
        // renders the honest unlinked surface (the synthetic-status case).
        let els = atproto_elements(&app, &app.settings.atproto);
        assert!(
            els.iter().any(|e| e.id == "bridge-action-button"),
            "level=linked must embed the shared bridge card even pre-fetch"
        );
    }

    #[test]
    fn off_and_linked_never_render_the_linked_panel() {
        let app = app_with_snapshot(snap(|s| s.level = "off".to_string()));
        let els = atproto_elements(&app, &app.settings.atproto);
        assert!(!els.iter().any(|e| e.id == "bridge-action-button"));
    }

    #[test]
    fn hosted_panel_shows_the_did_method_radio_pre_mint_and_the_identity_post_mint() {
        let app = app_with_snapshot(snap(|s| {
            s.level = "hosted_visible".to_string();
            s.show_did_method_radio = true;
            s.did_method = "plc".to_string();
            s.handle_preview = "alice.example.com".to_string();
        }));
        let els = atproto_elements(&app, &app.settings.atproto);
        assert!(els.iter().any(|e| e.id == "atproto-did-method"));
        let plc = els
            .iter()
            .find(|e| e.id == "atproto-did-method-plc")
            .unwrap();
        assert!(matches!(plc.role, Role::Radio { selected: true, .. }));
        assert!(!els.iter().any(|e| e.id == "atproto-hosted-handle"));

        let app2 = app_with_snapshot(snap(|s| {
            s.level = "hosted_visible".to_string();
            s.show_did_method_radio = false;
            s.identity = Some(IdentitySummaryRow {
                handle: "alice.example.com".to_string(),
                method: "plc".to_string(),
                status: "pending".to_string(),
            });
        }));
        let els2 = atproto_elements(&app2, &app2.settings.atproto);
        assert!(!els2.iter().any(|e| e.id == "atproto-did-method"));
        let handle = els2
            .iter()
            .find(|e| e.id == "atproto-hosted-handle")
            .unwrap();
        assert!(handle.text.contains("alice.example.com"));
        assert!(handle.text.contains(t::IDENTITY_STATUS_PENDING));
    }

    #[test]
    fn delete_presence_renders_whenever_a_hosted_identity_exists() {
        let app = app_with_snapshot(snap(|s| {
            s.level = "off".to_string();
            s.show_delete_presence = true;
        }));
        let els = atproto_elements(&app, &app.settings.atproto);
        assert!(els.iter().any(|e| e.id == "atproto-delete-presence"));
    }

    #[test]
    fn the_identity_summary_renders_at_level_off_marked_with_its_status() {
        // `ui/atproto.md` § Errors & edge cases: a deactivated identity at
        // level Off/Linked keeps its summary, so the user can see what
        // re-enabling would restore — and, once the sweep has run, that the
        // presence is gone. Every app had this inside the hosted panel's
        // level gate, which is the one place the rule can never hold.
        for (status, label) in [
            ("deactivated", t::IDENTITY_STATUS_DEACTIVATED),
            ("deleted", t::IDENTITY_STATUS_DELETED),
        ] {
            let app = app_with_snapshot(snap(|s| {
                s.level = "off".to_string();
                s.identity = Some(IdentitySummaryRow {
                    handle: "alice.example.com".to_string(),
                    method: "plc".to_string(),
                    status: status.to_string(),
                });
            }));
            let els = atproto_elements(&app, &app.settings.atproto);
            let handle = els
                .iter()
                .find(|e| e.id == "atproto-hosted-handle")
                .unwrap_or_else(|| panic!("summary renders at level off for {status}"));
            assert!(handle.text.contains("alice.example.com"));
            assert!(
                handle.text.contains(label),
                "the raw wire word must not reach the screen: {}",
                handle.text
            );
        }
    }

    fn retire(available: bool, selected: bool) -> RetireIdentityOptIn {
        RetireIdentityOptIn {
            available,
            selected,
            unavailable_reason: (!available).then(|| LocalizedText::key("no log to retire")),
        }
    }

    fn delete_card_with(opt: RetireIdentityOptIn, in_progress: bool) -> App {
        app_with_snapshot(snap(move |s| {
            s.level = "hosted_visible".to_string();
            s.show_delete_presence = true;
            s.delete_confirm = Some(DeleteConfirmCardModel {
                lines: vec![LocalizedText::key("your posts go")],
                in_progress,
                retire_identity: opt,
            });
        }))
    }

    fn tombstone(els: &[Element]) -> &Element {
        els.iter()
            .find(|e| e.id == "atproto-delete-tombstone")
            .expect("the opt-in rides the delete card")
    }

    #[test]
    fn the_retire_opt_in_renders_on_the_delete_card_unticked_and_live() {
        let app = delete_card_with(retire(true, false), false);
        let els = atproto_elements(&app, &app.settings.atproto);
        let el = tombstone(&els);
        assert!(el.enabled);
        assert_eq!(attr(el, "state"), Some("off"));
        assert!(
            el.text.contains(t::DELETE_RETIRE_IDENTITY_LABEL),
            "the label says it cannot be undone: {}",
            el.text
        );
        assert!(matches!(
            &el.role,
            Role::Checkbox {
                gesture: Gesture::Settings(Action::AtprotoSetDeleteRetireIdentity(true)),
                checked: false
            }
        ));
    }

    #[test]
    fn a_ticked_opt_in_unticks_on_the_next_press() {
        let app = delete_card_with(retire(true, true), false);
        let els = atproto_elements(&app, &app.settings.atproto);
        let el = tombstone(&els);
        assert_eq!(attr(el, "state"), Some("on"));
        assert!(matches!(
            &el.role,
            Role::Checkbox {
                gesture: Gesture::Settings(Action::AtprotoSetDeleteRetireIdentity(false)),
                checked: true
            }
        ));
    }

    #[test]
    fn an_unretirable_identity_greys_the_opt_in_and_says_why() {
        let app = delete_card_with(retire(false, false), false);
        let els = atproto_elements(&app, &app.settings.atproto);
        let el = tombstone(&els);
        assert!(!el.enabled, "greyed, never a control that errors on press");
        assert_eq!(attr(el, "state"), Some("unavailable"));
        assert!(
            els.iter().any(|e| e.text.contains("no log to retire")),
            "the machine's reason is painted beside it"
        );
    }

    #[test]
    fn the_opt_in_freezes_while_the_confirm_is_in_flight() {
        let app = delete_card_with(retire(true, true), true);
        let els = atproto_elements(&app, &app.settings.atproto);
        assert!(!tombstone(&els).enabled);
    }

    #[test]
    fn no_delete_ceremony_card_until_the_machine_opens_one() {
        let app = app_with_snapshot(snap(|s| {
            s.level = "hosted_visible".to_string();
            s.show_delete_presence = true;
        }));
        let els = atproto_elements(&app, &app.settings.atproto);
        assert!(!els.iter().any(|e| e.id == "atproto-delete-confirm-card"));
        assert!(!els.iter().any(|e| e.id == "atproto-delete-confirm"));
        assert!(!els.iter().any(|e| e.id == "atproto-delete-cancel"));
        assert!(
            !els.iter().any(|e| e.id == "atproto-delete-tombstone"),
            "the opt-in exists only inside the ceremony (§ Don't do these)"
        );
    }

    #[test]
    fn the_delete_ceremony_card_renders_its_copy_verbatim_and_gates_on_in_progress() {
        // The card's lines are the MACHINE's — this page joins and paints them
        // and derives nothing, which is what keeps one ceremony's promises
        // identical on seven apps.
        let card = |in_progress: bool| {
            app_with_snapshot(snap(move |s| {
                s.level = "hosted_visible".to_string();
                s.show_delete_presence = true;
                s.delete_confirm = Some(DeleteConfirmCardModel {
                    lines: vec![
                        LocalizedText::key("your posts go"),
                        LocalizedText::key("your identity stays"),
                    ],
                    in_progress,
                    retire_identity: retire(true, false),
                });
            }))
        };

        let app = card(false);
        let els = atproto_elements(&app, &app.settings.atproto);
        let painted = els
            .iter()
            .find(|e| e.id == "atproto-delete-confirm-card")
            .expect("the ceremony's own card");
        assert!(painted.text.contains("your posts go"));
        assert!(
            painted.text.contains("your identity stays"),
            "every line, verbatim: {}",
            painted.text
        );
        // Its OWN card, never the depth selector's (§ User actions row 4).
        assert!(!els.iter().any(|e| e.id == "atproto-depth-confirm-card"));
        assert!(
            els.iter()
                .find(|e| e.id == "atproto-delete-confirm")
                .expect("confirm")
                .enabled
        );
        assert!(
            els.iter()
                .find(|e| e.id == "atproto-delete-cancel")
                .expect("cancel")
                .enabled
        );

        let busy = card(true);
        let els2 = atproto_elements(&busy, &busy.settings.atproto);
        assert!(
            !els2
                .iter()
                .find(|e| e.id == "atproto-delete-confirm")
                .expect("confirm")
                .enabled,
            "a second press must not send a second sweep"
        );
        assert!(
            !els2
                .iter()
                .find(|e| e.id == "atproto-delete-cancel")
                .expect("cancel")
                .enabled,
            "the sweep is already on the wire; nest decides, not this card"
        );
    }

    #[test]
    fn full_pds_panel_renders_only_at_hosted_full() {
        let at_visible = app_with_snapshot(snap(|s| s.level = "hosted_visible".to_string()));
        let els = atproto_elements(&at_visible, &at_visible.settings.atproto);
        assert!(!els.iter().any(|e| e.id == "atproto-app-credential-mint"));
        assert!(!els.iter().any(|e| e.id == "atproto-external-apps-enable"));

        let at_full = app_with_snapshot(snap(|s| s.level = "hosted_full".to_string()));
        let els2 = atproto_elements(&at_full, &at_full.settings.atproto);
        assert!(els2.iter().any(|e| e.id == "atproto-app-credential-mint"));
        assert!(els2.iter().any(|e| e.id == "atproto-external-apps-enable"));
    }

    /// A `DelegationRow` fixture. Micros, not millis — these come from the
    /// signed cert.
    fn delegation(liveness: &str, expires: Option<u64>) -> DelegationRow {
        DelegationRow {
            device_key_hex: "aa".repeat(32),
            capabilities: vec!["Post".to_string(), "UpdateProfile".to_string()],
            authorized_at_micros: 1_700_000_000_000_000,
            expires_at_micros: expires,
            liveness: liveness.to_string(),
            last_used_at_millis: None,
        }
    }

    /// The D10 advisory last-use hint (`atproto-pds-full.md` § D10 → *Audit*).
    ///
    /// The two arms exist because the WORDING is the load-bearing part here, not
    /// the presence of the element: this is the one value on the row that is NOT
    /// derived from the signed cert — the nest simply asserts it — so an absent
    /// stamp must read as "nothing was REPORTED", never as "nothing happened".
    /// A nest that under-reports is precisely what this value cannot detect, and
    /// a row implying otherwise would turn an advisory hint into false assurance.
    #[test]
    fn the_last_used_row_is_advisory_and_never_claims_a_delegation_went_unused() {
        let used = app_with_snapshot(snap(|s| {
            s.level = "hosted_full".to_string();
            let mut row = delegation("active", Some(1_800_000_000_000_000));
            row.last_used_at_millis = Some(1_700_000_500_000);
            s.delegation = Some(row);
        }));
        let els = atproto_elements(&used, &used.settings.atproto);
        let leaf = els
            .iter()
            .find(|e| e.id == "atproto-delegation-last-used")
            .expect("last-used leaf on a used delegation");
        assert_eq!(
            leaf.text,
            t::delegation_last_used(&fauna_core::format::format_unix_local(1_700_000_500)),
            "a reported use renders the reported instant"
        );
        assert_eq!(
            attr(leaf, "advisory"),
            Some("true"),
            "the leaf must mark itself advisory so no client renders it as proof"
        );

        // The never-used arm. `delegation()` leaves the stamp `None`, which is
        // also exactly what a use the nest never reported produces — both
        // honestly read the same, because the client genuinely cannot tell them
        // apart and must not pretend to.
        let never = app_with_snapshot(snap(|s| {
            s.level = "hosted_full".to_string();
            s.delegation = Some(delegation("active", Some(1_800_000_000_000_000)));
        }));
        let els = atproto_elements(&never, &never.settings.atproto);
        let leaf = els
            .iter()
            .find(|e| e.id == "atproto-delegation-last-used")
            .expect("last-used leaf on an unused delegation");
        assert_eq!(leaf.text, t::DELEGATION_LAST_USED_NEVER);
        assert!(
            !leaf.text.to_lowercase().contains("never used")
                && !leaf.text.to_lowercase().contains("not used"),
            "the absent-stamp wording must not assert that no app posted — only              that no use was REPORTED; got {:?}",
            leaf.text
        );
    }

    #[test]
    fn the_delegation_row_is_withheld_until_one_is_provisioned() {
        // `None` covers BOTH "never authorized" and "the stored cert failed the
        // client-side verify" — in neither case may the page render a grant the
        // user cannot be shown to have made. But the authorize control must
        // still be there: it is the only affordance in the empty state.
        let app = app_with_snapshot(snap(|s| s.level = "hosted_full".to_string()));
        let els = atproto_elements(&app, &app.settings.atproto);
        assert!(!els.iter().any(|e| e.id == "atproto-delegation-row"));
        assert!(!els.iter().any(|e| e.id == "atproto-delegation-status"));
        assert!(!els.iter().any(|e| e.id == "atproto-delegation-revoke"));
        assert!(els.iter().any(|e| e.id == "atproto-delegation-authorize"));
    }

    #[test]
    fn a_live_delegation_renders_its_leaves_with_the_liveness_state_attr() {
        let app = app_with_snapshot(snap(|s| {
            s.level = "hosted_full".to_string();
            s.delegation = Some(delegation(
                "active",
                Some(1_700_000_000_000_000 + 90 * 86_400 * 1_000_000),
            ));
        }));
        let els = atproto_elements(&app, &app.settings.atproto);
        assert!(els.iter().any(|e| e.id == "atproto-delegation-row"));

        // The e2e asserts the liveness STATE, not its prose — so the wire
        // spelling must ride the attr.
        let status = els
            .iter()
            .find(|e| e.id == "atproto-delegation-status")
            .expect("status leaf");
        assert_eq!(attr(status, "state"), Some("active"));
        assert_eq!(status.text, t::DELEGATION_STATUS_ACTIVE);

        // Capabilities render in user voice, never their wire spellings.
        let scope = els
            .iter()
            .find(|e| e.id == "atproto-delegation-scope")
            .expect("scope leaf");
        assert!(scope.text.contains(t::DELEGATION_CAPABILITY_POST));
        assert!(scope.text.contains(t::DELEGATION_CAPABILITY_UPDATE_PROFILE));
        assert!(
            !scope.text.contains("UpdateProfile"),
            "wire spelling leaked into the rendered scope"
        );

        assert!(els.iter().any(|e| e.id == "atproto-delegation-lasts-until"));
        assert!(els.iter().any(|e| e.id == "atproto-delegation-revoke"));
    }

    #[test]
    fn a_lapsed_delegation_still_offers_authorize_as_the_renewal_control() {
        // The `nests.md` § Expiry / renewal bar: a lapse reads as "re-authorize
        // here", never as silent feature loss — and renewal never requires a
        // revoke first, because provisioning overwrites the stored cert.
        let app = app_with_snapshot(snap(|s| {
            s.level = "hosted_full".to_string();
            s.delegation = Some(delegation("expired", Some(1_700_000_000_000_000)));
        }));
        let els = atproto_elements(&app, &app.settings.atproto);
        let status = els
            .iter()
            .find(|e| e.id == "atproto-delegation-status")
            .expect("status leaf");
        assert_eq!(attr(status, "state"), Some("expired"));
        assert!(els.iter().any(|e| e.id == "atproto-delegation-authorize"));
    }

    #[test]
    fn a_cert_with_no_expiry_says_so_rather_than_rendering_an_empty_date() {
        let app = app_with_snapshot(snap(|s| {
            s.level = "hosted_full".to_string();
            s.delegation = Some(delegation("never_expires", None));
        }));
        let els = atproto_elements(&app, &app.settings.atproto);
        let lasts = els
            .iter()
            .find(|e| e.id == "atproto-delegation-lasts-until")
            .expect("lasts-until leaf");
        assert_eq!(
            lasts.text,
            t::delegation_lasts_until_no_expiry(&fauna_core::format::format_unix_local(
                1_700_000_000
            ))
        );
    }

    #[test]
    fn an_unrecognized_liveness_or_capability_degrades_rather_than_vanishing() {
        // A shell built against an older spelling set must degrade, not fail to
        // decode — the reason these cross as strings (snapshots.rs).
        let app = app_with_snapshot(snap(|s| {
            s.level = "hosted_full".to_string();
            let mut row = delegation("some_future_state", Some(1_800_000_000_000_000));
            row.capabilities = vec!["Post".to_string(), "FutureCapability".to_string()];
            s.delegation = Some(row);
        }));
        let els = atproto_elements(&app, &app.settings.atproto);
        let status = els
            .iter()
            .find(|e| e.id == "atproto-delegation-status")
            .expect("status leaf");
        assert_eq!(status.text, "some_future_state");
        let scope = els
            .iter()
            .find(|e| e.id == "atproto-delegation-scope")
            .expect("scope leaf");
        assert!(scope.text.contains("FutureCapability"));
    }

    #[test]
    fn the_delegation_row_is_gated_on_hosted_full_like_the_rest_of_the_panel() {
        let app = app_with_snapshot(snap(|s| {
            s.level = "hosted_visible".to_string();
            s.delegation = Some(delegation("active", Some(1_800_000_000_000_000)));
        }));
        let els = atproto_elements(&app, &app.settings.atproto);
        assert!(!els.iter().any(|e| e.id == "atproto-delegation-row"));
        assert!(!els.iter().any(|e| e.id == "atproto-delegation-authorize"));
    }

    #[test]
    fn empty_lists_at_hosted_full_paint_the_untagged_empty_state_chrome() {
        let app = app_with_snapshot(snap(|s| s.level = "hosted_full".to_string()));
        let els = atproto_elements(&app, &app.settings.atproto);
        assert!(els.iter().any(|e| e.text == t::APP_CREDENTIALS_EMPTY));
        assert_eq!(
            els.iter()
                .filter(|e| e.id == "atproto-app-credential-item")
                .count(),
            0
        );
    }

    #[test]
    fn a_revealable_credential_renders_the_reveal_button_enabled() {
        let app = app_with_snapshot(snap(|s| {
            s.level = "hosted_full".to_string();
            s.credentials = vec![credential("ivory", "Ivory", true, None)];
        }));
        let els = atproto_elements(&app, &app.settings.atproto);
        let reveal = els
            .iter()
            .find(|e| e.id == "atproto-app-credential-reveal")
            .expect("reveal renders when revealable");
        assert!(reveal.enabled);
        assert_eq!(reveal.text, t::REVEAL_BUTTON);
    }

    #[test]
    fn a_non_revealable_credential_renders_no_reveal_button() {
        let app = app_with_snapshot(snap(|s| {
            s.level = "hosted_full".to_string();
            s.credentials = vec![credential("graysky", "Graysky", false, None)];
        }));
        let els = atproto_elements(&app, &app.settings.atproto);
        assert!(!els.iter().any(|e| e.id == "atproto-app-credential-reveal"));
        assert_eq!(
            els.iter()
                .filter(|e| e.id == "atproto-app-credential-item")
                .count(),
            1
        );
        assert_eq!(
            els.iter()
                .filter(|e| e.id == "atproto-app-credential-revoke")
                .count(),
            1
        );
    }

    #[test]
    fn a_locally_revealed_secret_paints_as_the_button_text_and_disables_it() {
        let mut app = app_with_snapshot(snap(|s| {
            s.level = "hosted_full".to_string();
            s.credentials = vec![credential("ivory", "Ivory", false, None)];
        }));
        app.settings
            .atproto
            .revealed
            .insert("ivory".to_string(), "the-secret-value".to_string());
        let els = atproto_elements(&app, &app.settings.atproto);
        let reveal = els
            .iter()
            .find(|e| e.id == "atproto-app-credential-reveal")
            .expect("a locally-revealed secret still renders the row, even if not `revealable`");
        assert_eq!(reveal.text, "the-secret-value");
        assert!(
            !reveal.enabled,
            "an already-revealed secret disables the button"
        );
    }

    #[test]
    fn last_used_never_falls_back_to_the_never_used_label() {
        let app = app_with_snapshot(snap(|s| {
            s.level = "hosted_full".to_string();
            s.credentials = vec![credential("ivory", "Ivory", true, None)];
        }));
        let els = atproto_elements(&app, &app.settings.atproto);
        let row = els
            .iter()
            .find(|e| e.id == "atproto-app-credential-item")
            .unwrap();
        assert!(row.text.contains(&t::CREDENTIAL_NEVER_USED.to_string()));
    }

    #[test]
    fn the_kill_switch_carries_the_state_attr_off_the_snapshot() {
        let app = app_with_snapshot(snap(|s| {
            s.level = "hosted_full".to_string();
            s.external_apps_enabled = false;
        }));
        let els = atproto_elements(&app, &app.settings.atproto);
        let toggle = els
            .iter()
            .find(|e| e.id == "atproto-external-apps-enable")
            .unwrap();
        assert_eq!(
            toggle
                .attrs
                .iter()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.as_str()),
            Some("off")
        );
    }
}
