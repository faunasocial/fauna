//! `#[wasm_bindgen]` face of the **Sessions page** (`docs/goal/ui/sessions.md`
//! § Where logic lives — the wasm twin of `libs/fauna-ffi/src/sessions.rs`).
//! One shared pass, exposed once at each boundary (priority #2):
//! `fauna_client_account::SessionsClient::pass` acts and re-folds,
//! `sessions_view` owns the rows, the roster reduction, the lock's word gate
//! and the keep-the-painted-rows rule (`SessionsPass::onto`).
//!
//! Two inputs differ from the UniFFI face, both because of where web keeps
//! them: this tab's own session ids live in the SPA's bearer cache
//! (`#lib/own-session-ids.ts`, the documented TypeScript twin of
//! `OwnSessionIds`), so they are handed in on every call; and the roster is the
//! rows the SPA's Devices machine already rendered. Times cross as epoch
//! seconds and the SPA formats them itself (`ui/sessions.md` § State & data
//! shape — the custody-row precedent). Every call resolves (never rejects) to
//! `{ snapshot: { rows, error }, locked }`.

use fauna_client_account::sessions_view::{self, SessionRowView, SessionsSnapshot};
use fauna_client_account::{SessionsAct, SessionsClient};
use fauna_core::localized::LocalizedText;
use fauna_protocol::auth::OwnSessionSource;
use js_sys::Promise;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

use crate::rpc::{WsRpcClient, from_js, to_js};

/// What every call reads besides the act. Field names are the SPA's
/// (`camelCase`).
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct SessionsInput {
    /// This tab's live own ids (`OwnSessionIds.ids(now)`).
    own_token_ids: Vec<String>,
    /// The newest live own id (`OwnSessionIds.currentAt(now)`) — the keep-id
    /// sign out everywhere else sends.
    current_token_id: Option<String>,
    /// The Devices machine's rendered roster rows.
    devices: Vec<RosterRow>,
    /// The roster row this machine enrolled, when the SPA knows one.
    this_device_row: Option<String>,
    /// The page the SPA painted last: a failed re-read keeps its rows.
    previous: Option<Snapshot>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RosterRow {
    device_id: String,
    principal: Option<String>,
    label: String,
}

/// `SessionsSnapshot` across the boundary.
#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    rows: Vec<Row>,
    error: Option<LocalizedText>,
}

/// `SessionRowView` across the boundary.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    token_id: String,
    kind: LocalizedText,
    mark: Option<LocalizedText>,
    created_at: u64,
    last_used_at: u64,
    expires_at: u64,
    ip_address: Option<String>,
    can_revoke: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    snapshot: Snapshot,
    locked: bool,
}

/// This tab's own ids, as the SPA handed them in.
struct HandedOwn {
    ids: Vec<String>,
    current: Option<String>,
}

impl OwnSessionSource for HandedOwn {
    async fn own_token_ids(&self) -> Vec<String> {
        self.ids.clone()
    }
    async fn current_token_id(&self) -> Option<String> {
        self.current.clone()
    }
}

impl From<&SessionRowView> for Row {
    fn from(r: &SessionRowView) -> Self {
        Row {
            token_id: r.token_id.clone(),
            kind: r.kind.clone(),
            mark: r.mark.clone(),
            created_at: r.created_at,
            last_used_at: r.last_used_at,
            expires_at: r.expires_at,
            ip_address: r.ip_address.clone(),
            can_revoke: r.can_revoke,
        }
    }
}

impl From<Row> for SessionRowView {
    fn from(r: Row) -> Self {
        SessionRowView {
            token_id: r.token_id,
            kind: r.kind,
            mark: r.mark,
            created_at: r.created_at,
            last_used_at: r.last_used_at,
            expires_at: r.expires_at,
            ip_address: r.ip_address,
            can_revoke: r.can_revoke,
        }
    }
}

fn now_secs() -> u64 {
    (js_sys::Date::now() / 1000.0) as u64
}

fn run(client: &WsRpcClient, input: JsValue, act: Option<SessionsAct>) -> Promise {
    let nest = client.inner_client();
    future_to_promise(async move {
        let input: SessionsInput = from_js(input)?;
        let previous = input.previous.map(|p| SessionsSnapshot {
            rows: p.rows.into_iter().map(SessionRowView::from).collect(),
            error: p.error,
        });
        let roster = sessions_view::roster_from(
            input
                .devices
                .into_iter()
                .map(|d| (d.device_id, d.principal, d.label)),
            input.this_device_row.as_deref(),
        );
        let own = HandedOwn {
            ids: input.own_token_ids,
            current: input.current_token_id,
        };
        let client = SessionsClient::new(nest, own);
        let pass = client.pass(act, async { roster }, now_secs).await;
        let locked = pass.locked;
        let page = pass.onto(previous);
        to_js(&Page {
            snapshot: Snapshot {
                rows: page.rows.iter().map(Row::from).collect(),
                error: page.error,
            },
            locked,
        })
    })
}

/// The nav-edge hydrate: read and fold.
#[wasm_bindgen(js_name = sessionsLoad)]
pub fn sessions_load(client: &WsRpcClient, input: JsValue) -> Promise {
    run(client, input, None)
}

/// `session-revoke-button[i]` — revoke `token_id`, then re-fold.
#[wasm_bindgen(js_name = sessionsRevoke)]
pub fn sessions_revoke(client: &WsRpcClient, input: JsValue, token_id: String) -> Promise {
    run(client, input, Some(SessionsAct::Revoke(token_id)))
}

/// `sessions-revoke-others-confirm-button` — end every session but this tab's
/// (`currentTokenId` is the keep-id; none → refused on `error`), then re-fold.
#[wasm_bindgen(js_name = sessionsRevokeOthers)]
pub fn sessions_revoke_others(client: &WsRpcClient, input: JsValue) -> Promise {
    run(client, input, Some(SessionsAct::RevokeOthers))
}

/// `sessions-lockout-button` — `confirm_word` is the confirm field verbatim,
/// checked here (the action arm): a wrong word refuses on `error` and reaches
/// no nest. A landed lock resolves `locked: true`.
#[wasm_bindgen(js_name = sessionsLockout)]
pub fn sessions_lockout(client: &WsRpcClient, input: JsValue, confirm_word: String) -> Promise {
    let word = fauna_client_recovery::ceremony::LOCKOUT_CONFIRM_WORD;
    match sessions_view::lockout_word_refusal(&confirm_word, word) {
        None => run(client, input, Some(SessionsAct::Lockout)),
        Some(refusal) => future_to_promise(async move {
            let input: SessionsInput = from_js(input)?;
            to_js(&Page {
                snapshot: Snapshot {
                    rows: input.previous.map(|p| p.rows).unwrap_or_default(),
                    error: Some(refusal),
                },
                locked: false,
            })
        }),
    }
}

/// The lock's confirm word, `LOCK` — never localized; for the render's enable
/// check only.
#[wasm_bindgen(js_name = sessionsLockoutConfirmWord)]
pub fn sessions_lockout_confirm_word() -> String {
    fauna_client_recovery::ceremony::LOCKOUT_CONFIRM_WORD.to_string()
}
