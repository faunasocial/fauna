//! The Settings → **Connected apps** sub-page (`docs/goal/ui/connected-apps.md`;
//! ui.yaml page `connected-apps`, reached `{"view":"settings","id":"connected-apps"}`).
//! tui is the lead app; the rail slot is directly after Task delegation
//! (`settings.md` § Navigation model).
//!
//! Four regions, top to bottom:
//!
//! 1. **Requests** — the quiet-push tray: one built consent card per live
//!    request, with Approve / Decline / *Never show requests from this app*.
//!    Painted only while a request is live — never a "no requests" row, which
//!    would train the user to ignore the one place the anti-phishing check
//!    happens.
//! 2. **Connect an app** — the typed-code start: a code field and a submit.
//! 3. **The roster** — one row per connected app, Revoke with an inline confirm.
//!    A mail app-password row additionally carries its login, kind and secret
//!    controls.
//! 4. **Blocked apps** — one row per blocked client with Unblock. Painted only
//!    while something is blocked.
//!
//! **A paint shell over the shared `ConnectedAppsMachine`**
//! (`libs/fauna-client-connected-apps`): the roster's composition, the scope
//! words, the class badge key, *lasts-until*, and which verb revokes a row are
//! all the machine's. This file never picks a revoke verb — a row's `key` is
//! opaque here. The consent card painted in the tray is the built card
//! (`fauna_atproto_settings_machine::consent_card_row`'s composition, the same
//! card the atproto page painted before the lift), never a second one.
//!
//! **The lift.** The atproto page's consent cards and connected-apps rows, the
//! Nostr page's bunker rows and the Mail & Calendar page's app-password rows
//! render HERE on tui and no longer on their old pages — a row moves, it is
//! never shown twice (`connected-apps.md` § Architectural rules).

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client_connected_apps::{
    ConnectedAppRow, ConnectedAppsMachine, ConnectedAppsObserver, ConnectedAppsSnapshot,
    ConsentCardRow, class,
};
use fauna_client_mail_settings::{MailSettingsMachine, resolve_mua_username};
use fauna_core::secret::SecretString;
use fauna_i18n::strings::settings::mail as mail_t;
use fauna_i18n::strings::{atproto_settings as card_t, connected_apps as t};

use super::{Action, SettingsField, SettingsState};
use crate::element::{Element, Field, Gesture};

/// The machine notifies synchronously after every mutation, but this page reads
/// a fresh `snapshot()` after each awaited `Op` instead (the Mail/Bluesky
/// shape), so the callback has nothing to do.
struct NoopObserver;

impl ConnectedAppsObserver for NoopObserver {
    fn on_changed(&self) {}
}

/// One gesture on the page, carried by `Op::ConnectedApps` across the spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConnectedAppsGesture {
    /// The nav-edge re-read.
    Refresh,
    SubmitCode(String),
    /// A `fauna://consent/<request_uri>` route's open (`App::apply_route`).
    OpenHandoff(String),
    Resolve {
        consent_id_hex: String,
        approved: bool,
    },
    Block(String),
    /// Lift a per-client block, by the blocked client's id.
    Unblock(String),
    Revoke(String),
}

impl ConnectedAppsGesture {
    pub(crate) async fn run(self, machine: &ConnectedAppsMachine) {
        match self {
            Self::Refresh => machine.refresh().await,
            Self::SubmitCode(code) => machine.submit_code(code).await,
            Self::OpenHandoff(request_uri) => machine.open_handoff(request_uri).await,
            Self::Resolve {
                consent_id_hex,
                approved,
            } => machine.resolve_request(consent_id_hex, approved).await,
            Self::Block(consent_id_hex) => machine.block_request(consent_id_hex).await,
            Self::Unblock(client_id) => machine.unblock(client_id).await,
            Self::Revoke(key) => machine.revoke(key).await,
        }
    }
}

/// The page's state. The machine is built once at the post-auth hook
/// (`attach_session`), the `MailAliasesState` shape; `clear_session` drops it.
#[derive(Default)]
pub(crate) struct ConnectedAppsState {
    pub(crate) machine: Option<Arc<ConnectedAppsMachine>>,
    /// The last snapshot painted. `None` until the first fold — the page then
    /// paints neither rows nor its empty state.
    pub(crate) snapshot: Option<ConnectedAppsSnapshot>,
    /// The *Connect an app* field's draft.
    pub(crate) code_input: String,
    /// The row key whose inline revoke confirm is open.
    pub(crate) revoke_armed: Option<String>,
    /// The mail app-password secrets currently shown, by row key.
    ///
    /// Empty by default: the secret is never in the snapshot, so a row shows
    /// one only after the user asks and the on-demand read resolves. Held as
    /// [`SecretString`] so it stays zeroizing and redacted up to the paint.
    /// Keyed by row key, never by index, so a roster that re-orders under a
    /// fresh snapshot cannot show one password's secret against another row.
    pub(crate) revealed: std::collections::HashMap<String, SecretString>,
}

impl ConnectedAppsState {
    /// `mail` is the session's mail-settings machine, whose app passwords are
    /// rows of this roster; `None` (its build failed) leaves the roster
    /// without mail rows.
    ///
    /// `secret_hex` and `runtime` feed the consent-time grant an approve of a
    /// records or folder-read consent mints (`super::atproto::consent_grant_seams`, the AT
    /// Protocol page's same seams).
    pub(super) fn build(
        nest: Arc<fauna_client::NestClient>,
        mail: Option<Arc<MailSettingsMachine>>,
        secret_hex: &str,
        runtime: super::AccountRuntimeSlot,
    ) -> Self {
        let observer: Arc<dyn ConnectedAppsObserver> = Arc::new(NoopObserver);
        let seams = super::atproto::consent_grant_seams(secret_hex, Arc::clone(&nest), runtime);
        let machine =
            fauna_client_connected_apps::build_connected_apps_machine(nest, observer, mail);
        if let Some(seams) = seams {
            machine.set_consent_grant_seams(seams);
        }
        Self {
            machine: Some(machine),
            ..Self::default()
        }
    }

    /// Drop page-local drafts on a fresh visit, so an armed revoke never
    /// survives a nav-away and fires against a later visit's list — and take
    /// every shown secret off the screen: a secret is shown because the user
    /// asked on THIS visit.
    ///
    /// The last visit's snapshot goes too: rows are nest state read on every
    /// open, so a visit paints neither rows nor the empty state until its own
    /// read has returned (`connected-apps.md` § Errors & edge cases) — never
    /// the previous visit's list while the fresh one is in flight.
    pub(super) fn reset_form(&mut self) {
        self.code_input.clear();
        self.revoke_armed = None;
        self.revealed.clear();
        self.snapshot = None;
    }
}

/// The page's ordered element list. `error-message` is registered globally
/// from `App::errors` (the fold copies the machine's error there).
pub(super) fn connected_apps_elements(state: &SettingsState) -> Vec<Element> {
    let c = &state.connected_apps;
    let mut els = vec![
        Element::label(ids::PAGE_HEADING, t::TITLE),
        Element::chrome(t::DESCRIPTION),
    ];
    let snap = c.snapshot.as_ref();

    // ── 1. Requests — only while a request is live ──
    let requests: &[ConsentCardRow] = snap.map(|s| s.requests.as_slice()).unwrap_or(&[]);
    if !requests.is_empty() {
        els.push(Element::chrome(t::REQUESTS_HEADING));
        for (i, request) in requests.iter().enumerate() {
            consent_card_elements(request, i, &mut els);
        }
    }

    // ── 2. Connect an app ──
    els.push(Element::chrome(t::CONNECT_HEADING));
    els.push(Element::chrome(t::CONNECT_HINT));
    els.push(
        Element::input_commit(
            ids::CONNECTED_APPS_CONNECT_CODE,
            c.code_input.clone(),
            Field::Settings(SettingsField::ConnectedAppsCode),
            Gesture::Settings(Action::ConnectedAppsSubmitCode),
        )
        .labelled(t::CONNECT_PLACEHOLDER),
    );
    els.push(Element::gesture_button(
        ids::CONNECTED_APPS_CONNECT_SUBMIT,
        t::CONNECT_SUBMIT,
        !c.code_input.trim().is_empty(),
        Gesture::Settings(Action::ConnectedAppsSubmitCode),
    ));

    // ── 3. The roster ──
    els.push(Element::chrome(t::ROSTER_HEADING));
    // The three-state list: nothing until the roster read has returned, then
    // either rows or the empty state (`ui/README.md` § List pages).
    if let Some(snap) = snap.filter(|s| s.loaded) {
        if snap.principals.is_empty() {
            els.push(Element::label(ids::CONNECTED_APPS_EMPTY, t::EMPTY));
        }
        for (i, row) in snap.principals.iter().enumerate() {
            row_elements(c, row, i, &state.handle, &mut els);
        }
    }

    // ── 4. Blocked apps — only while something is blocked ──
    let blocked = snap.map(|s| s.blocked.as_slice()).unwrap_or(&[]);
    if !blocked.is_empty() {
        els.push(Element::chrome(t::BLOCKED_HEADING));
        els.push(Element::chrome(t::BLOCKED_HINT));
        for (i, b) in blocked.iter().enumerate() {
            const BLOCKED: &str = ids::CONNECTED_APPS_BLOCKED_ITEM;
            // The client id verbatim, as the request card showed it — nothing
            // here parses it into a host or a name.
            els.push(Element::label(
                BLOCKED,
                format!(
                    "{}\n{}",
                    b.client_id,
                    t::blocked_since(&when(b.blocked_at_millis))
                ),
            ));
            els.push(
                Element::gesture_button(
                    ids::CONNECTED_APPS_BLOCKED_ITEM_UNBLOCK,
                    t::UNBLOCK,
                    true,
                    Gesture::Settings(Action::ConnectedAppsUnblock(b.client_id.clone())),
                )
                .within(BLOCKED, i),
            );
        }
    }

    els.push(
        Element::gesture_button(
            ids::SETTINGS_NAV_BACK,
            fauna_i18n::strings::common::BACK,
            true,
            Gesture::Settings(Action::NavBack),
        )
        .nav_back(),
    );
    els
}

/// The class badge's words — the one per-app half of the grouping key, which
/// the machine derives. An unknown class paints no badge rather than a guess.
fn class_label(class: &str) -> Option<&'static str> {
    Some(match class {
        class::REMOTE => t::CLASS_REMOTE,
        class::DEVICE => t::CLASS_DEVICE,
        class::WASM => t::CLASS_WASM,
        class::CONTAINER => t::CLASS_CONTAINER,
        class::APP_PASSWORD => t::CLASS_APP_PASSWORD,
        class::SIGNER => t::CLASS_SIGNER,
        class::OAUTH => t::CLASS_OAUTH,
        _ => return None,
    })
}

fn when(ms: i64) -> String {
    fauna_core::format::format_unix_local_ms(ms)
}

/// One roster row: the joined description as the item's own text (the
/// `nostr-bunker-app-item` shape — ui.yaml mints no per-field leaves for the
/// columns every row has), a mail app password's own leaves, then Revoke, or
/// the armed confirm pair.
fn row_elements(
    c: &ConnectedAppsState,
    row: &ConnectedAppRow,
    i: usize,
    handle: &str,
    els: &mut Vec<Element>,
) {
    const ITEM: &str = ids::CONNECTED_APPS_ITEM;
    let name = crate::wizard::localized(&row.name);
    let mut head = name.clone();
    if let Some(badge) = class_label(&row.class) {
        head = format!("{head} · {badge}");
    }
    let mut lines = vec![head];
    let burned = row.mail.as_ref().is_some_and(|m| m.revoked);
    // The burned state sits directly under the name and above everything a
    // user might copy into a mail app: the login below it authenticates
    // nothing any more (`mail-credentials.md` § Rotation and recovery →
    // *Succession*).
    if burned {
        lines.push(mail_t::CREDENTIAL_REVOKED.to_string());
    }
    if let (Some(client_id), Some(domain)) = (&row.client_id, &row.publisher) {
        lines.push(format!("{} — {client_id}", t::publisher(domain)));
    }
    for scope in &row.scope_descriptions {
        lines.push(format!("  • {}", crate::wizard::localized(scope)));
    }
    let mut facts = Vec::new();
    if !row.connected {
        facts.push(t::NOT_CONNECTED.to_string());
    }
    facts.push(t::created(&when(row.created_at_millis)));
    facts.push(match row.last_used_at_millis {
        Some(ms) => t::last_used(&when(ms)),
        None => t::NEVER_USED.to_string(),
    });
    facts.push(match row.lasts_until_millis {
        Some(ms) => t::lasts_until(&when(ms)),
        None => t::OPEN_ENDED.to_string(),
    });
    lines.push(facts.join(" · "));
    let mut item = Element::label(ITEM, lines.join("\n")).attr("key", &row.key);
    if burned {
        // The burned state as a value a reader can count, beside the words.
        item = item.attr("revoked", "true");
    }
    els.push(item);

    if let Some(mail) = &row.mail {
        els.push(
            Element::label(
                ids::CONNECTED_APPS_ITEM_TYPE,
                crate::wizard::localized(&mail.kind),
            )
            .within(ITEM, i),
        );
        // The concrete login: shared Rust resolved everything but `{handle}`,
        // so the paint is one substitution and never a locally built address.
        els.push(
            Element::label(
                ids::CONNECTED_APPS_ITEM_USERNAME,
                resolve_mua_username(&mail.mua_username, handle),
            )
            .within(ITEM, i),
        );
        els.push(
            Element::gesture_button(
                ids::CONNECTED_APPS_ITEM_COPY_USERNAME,
                mail_t::COPY_USERNAME,
                true,
                Gesture::Settings(Action::ConnectedAppsCopyUsername(row.key.clone())),
            )
            .within(ITEM, i),
        );
        // ⚠ The hidden secret's `text` MUST stay EMPTY. The cross-app driver
        // polls it until it turns non-empty and returns that AS the secret, so
        // a mask ("••••") would pass the reveal test without the on-demand
        // read ever running.
        let revealed = c.revealed.get(&row.key);
        let secret = Element::label(
            ids::CONNECTED_APPS_ITEM_SECRET,
            revealed.map(|s| s.as_str().to_owned()).unwrap_or_default(),
        );
        // `labelled` is paint-only: the registered text stays the bare secret.
        els.push(
            if revealed.is_some() {
                secret.labelled(mail_t::SECRET_LABEL)
            } else {
                secret
            }
            .within(ITEM, i),
        );
        els.push(
            Element::gesture_button(
                ids::CONNECTED_APPS_ITEM_REVEAL_SECRET,
                if revealed.is_some() {
                    mail_t::HIDE_SECRET
                } else {
                    mail_t::REVEAL_SECRET
                },
                true,
                Gesture::Settings(Action::ConnectedAppsRevealSecret(row.key.clone())),
            )
            .within(ITEM, i),
        );
        // Copy is independent of the reveal toggle: the secret reaches the
        // clipboard without being painted on a screen someone else can read.
        els.push(
            Element::gesture_button(
                ids::CONNECTED_APPS_ITEM_COPY_SECRET,
                mail_t::COPY_SECRET,
                true,
                Gesture::Settings(Action::ConnectedAppsCopySecret(row.key.clone())),
            )
            .within(ITEM, i),
        );
    }

    if c.revoke_armed.as_deref() == Some(row.key.as_str()) {
        els.push(Element::chrome(t::revoke_prompt(&name)).within(ITEM, i));
        els.push(
            Element::gesture_button(
                ids::CONNECTED_APPS_ITEM_REVOKE_CONFIRM,
                t::REVOKE_CONFIRM,
                true,
                Gesture::Settings(Action::ConnectedAppsConfirmRevoke(row.key.clone())),
            )
            .within(ITEM, i),
        );
        els.push(
            Element::gesture_button(
                ids::CONNECTED_APPS_ITEM_REVOKE_CANCEL,
                t::REVOKE_CANCEL,
                true,
                Gesture::Settings(Action::ConnectedAppsCancelRevoke),
            )
            .within(ITEM, i),
        );
    } else {
        els.push(
            Element::gesture_button(
                ids::CONNECTED_APPS_ITEM_REVOKE,
                t::REVOKE,
                true,
                Gesture::Settings(Action::ConnectedAppsArmRevoke(row.key.clone())),
            )
            .within(ITEM, i),
        );
    }
}

/// One `connected-apps-request-card`: a third-party app is asking to act for
/// this account and is waiting for the answer — the built consent card, moved
/// here from the atproto page with its wording (`atproto_settings.consent_*`)
/// unchanged: one card, a new start, never a second card
/// (`connected-apps.md` § Architectural rules). Every request the nest lists is
/// painted — an unhinted browser request included, which is listed to every
/// account by design (`authorization-server.md` § As built: "the honest cost
/// of a flow that named none"); the binding code is the user's check.
///
/// Three things render, and each is load-bearing:
///
/// - **who is asking** — the *resolved* `client_name` plus the `client_id`
///   **verbatim**. Nothing here derives an origin, a host or a fetch target from
///   that string; it is a URL, and it is parsed exactly once, by the component
///   that dials it. There is deliberately **no logo**: `logo_uri` never crosses
///   to the app at all, because loading an attacker-named URL inside the user's
///   app would disclose their address to whoever published the document — and
///   the consent card is exactly where an attacker controls that URL.
/// - **what it is asking for** — one line per scope, worded by the shared
///   `authz::describe_scope` the browser's own consent page renders from. The
///   machine has already composed these; this file must never write a second
///   wording, because the two surfaces sit side by side and a divergence is the
///   "is this the same request?" doubt the binding code exists to remove.
/// - **where those permissions came from**, when the client named a *permission
///   set* instead of listing them: the set's NSID verbatim, its publisher's
///   title and description, and every member it expanded to. The set-level
///   description never stands in for the expansion — a user approving
///   "Calendar sync" is entitled to read what that means without asking a
///   second surface (`atproto-pds-full.md:334`). The browser page renders the
///   same section for the same reason the scope wording is shared.
/// - **the binding code** (`connected-apps-request-code`) — minted by the *nest*, so
///   the value here and the one in the browser have one origin. The user's whole
///   job is to check they match; a consent push the user never started shows a
///   code their browser is not showing, and is visibly wrong.
///
/// ⚠ Both controls render unconditionally — **never gated on how close the
/// request is to expiring.** A resolution is reported even past `expires_at`, so
/// "this request just expired" may only ever come from the resolve reply. The
/// snapshot row carries no timestamp to check against, which is what keeps that
/// ruling structural rather than remembered.
fn consent_card_elements(consent: &ConsentCardRow, i: usize, els: &mut Vec<Element>) {
    const CARD: &str = ids::CONNECTED_APPS_REQUEST_CARD;
    let who = match &consent.client_name {
        Some(name) => card_t::consent_client(name, &consent.client_id),
        None => card_t::consent_client_unnamed(&consent.client_id),
    };
    let asks = consent
        .scope_descriptions
        .iter()
        .map(|line| format!("  • {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    // Permission-set provenance, after the effective list rather than instead
    // of it: who decided the bundle (the NSID, verbatim) and what its publisher
    // says it is for. Every member is already in `asks`.
    let sets = consent
        .sets
        .iter()
        .map(|set| {
            let heading = match set.title.as_deref() {
                Some(title) => card_t::consent_set_heading(title, &set.nsid),
                None => card_t::consent_set_heading_unnamed(&set.nsid),
            };
            let details = set
                .details
                .as_deref()
                .map(|d| format!("\n  {d}"))
                .unwrap_or_default();
            let members = set
                .member_descriptions
                .iter()
                .map(|line| format!("\n  • {line}"))
                .collect::<String>();
            format!("\n{heading}{details}{members}")
        })
        .collect::<String>();
    // What the approve ends besides granting — a key replacement's line,
    // inside the scopes-in-words block (`connected-apps.md` § User actions).
    let ends = consent
        .ends
        .as_ref()
        .map(|e| format!("\n{}", crate::wizard::localized(e)))
        .unwrap_or_default();
    els.push(Element::label(
        CARD,
        format!(
            "{}\n{who}\n{}\n{asks}{sets}{ends}",
            card_t::CONSENT_HEADING,
            card_t::CONSENT_SCOPES_HEADING
        ),
    ));
    els.push(
        Element::label(
            ids::CONNECTED_APPS_REQUEST_CODE,
            card_t::consent_code(&consent.code),
        )
        .attr("code", &consent.code)
        .within(CARD, i),
    );
    els.push(Element::chrome(card_t::CONSENT_CODE_HINT));
    for (id, label, approved) in [
        (
            ids::CONNECTED_APPS_REQUEST_APPROVE,
            card_t::CONSENT_APPROVE_BUTTON,
            true,
        ),
        (
            ids::CONNECTED_APPS_REQUEST_DECLINE,
            card_t::CONSENT_DENY_BUTTON,
            false,
        ),
    ] {
        els.push(
            Element::gesture_button(
                id,
                label,
                true,
                Gesture::Settings(Action::ConnectedAppsResolveRequest {
                    consent_id_hex: consent.consent_id_hex.clone(),
                    approved,
                }),
            )
            .within(CARD, i),
        );
    }
    els.push(
        Element::gesture_button(
            ids::CONNECTED_APPS_REQUEST_BLOCK,
            t::BLOCK,
            true,
            Gesture::Settings(Action::ConnectedAppsBlockRequest(
                consent.consent_id_hex.clone(),
            )),
        )
        .within(CARD, i),
    );
}

#[cfg(test)]
mod tests;
