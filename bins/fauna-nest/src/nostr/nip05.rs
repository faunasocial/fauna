//! NIP-05 identity serving — `GET /.well-known/nostr.json?name=<handle>`
//! (`docs/goal/ui/nostr.md` § Goal — the `you@<nest-domain>` identity leg — +
//! § Architecture).
//!
//! Answers **only** an exact `?name=` query: a missing or unknown name returns
//! an empty `names` map, never a full dump of the box's identities (the
//! enumeration posture the F5/SMTP-enumeration rulings hold — `network-
//! exposure.md`). The `name` is a Fauna handle (which rests lowercase and is a
//! valid NIP-05 local-part), resolved to its actor and that actor's linked
//! Nostr pubkey.
//!
//! **Identity ≠ agency.** Serving is keyed on the account being *linked*, not on
//! a deposited nsec: an NIP-07 / remote-signer account has a pubkey without a
//! deposit and is a legitimate `you@<domain>` identity. The nsec-deposit
//! bridging gate (`nostr::nostr_bridging_available`, § The bridging gate) gates
//! *agent acts* — signing, relaying, gift-wrap unwrap — not the publication of
//! an identity mapping, so this route deliberately does **not** consult it.
//!
//! CORS is `*` because web-based Nostr clients fetch this endpoint cross-origin
//! (NIP-05 § "the well-known URL"). No configuration surface — the identity is
//! derived from linked accounts + the deployment domain.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use fauna_bridge_nostr::nip05::build_nip05_response_with_relays;

use crate::nostr::db;
use crate::routes::AppState;

#[derive(Deserialize)]
pub(crate) struct Nip05Query {
    name: Option<String>,
}

/// Serve `GET /.well-known/nostr.json`.
pub(crate) async fn nip05_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<Nip05Query>,
) -> Response {
    let body = resolve(&state, params.name.as_deref()).await;
    (
        [
            ("content-type", "application/json"),
            ("access-control-allow-origin", "*"),
        ],
        body,
    )
        .into_response()
}

/// Build the NIP-05 JSON body for an optional `name` query. Returns an empty
/// `names` map (no `relays`) for a missing name, an unknown handle, or a handle
/// with no linked Nostr account.
async fn resolve(state: &AppState, name: Option<&str>) -> String {
    let empty = || build_nip05_response_with_relays(&[]);
    let Some(name) = name else { return empty() };

    // Handles rest lowercase; NIP-05 names are case-insensitive.
    let name = name.to_ascii_lowercase();
    let Ok(Some(actor)) = state.db.resolve_handle(&name).await else {
        return empty();
    };

    let conn = state.db.conn().await;
    let account = db::get_account(&conn, &hex::encode(actor));
    drop(conn);
    let Ok(Some(account)) = account else {
        return empty();
    };

    // Relay hint: the nest's own relay, if a deployment domain is set. Reads the
    // claim-refreshed identity domain, not the `node.domain` boot seed — a
    // provisioned box boots domainless and would otherwise advertise no relay
    // at all until a restart.
    let relay = state
        .handle_domain_if_set()
        .map(|d| format!("wss://{d}/nostr"));
    match relay.as_deref() {
        Some(r) => build_nip05_response_with_relays(&[(&name, &account.nostr_pubkey, &[r])]),
        None => build_nip05_response_with_relays(&[(&name, &account.nostr_pubkey, &[])]),
    }
}
