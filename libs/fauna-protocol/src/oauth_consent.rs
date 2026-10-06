//! Wire types for the consent starts' **user** half (`fauna.oauth.consent.*`),
//! per `docs/goal/behavior/authorization-server.md` § Consent.
//!
//! Four User-class, self-scoped kinds beside the built card's
//! `fauna.bridges.atproto.{list_pending_consents,resolve_consent}`:
//!
//! * `fauna.oauth.consent.lookup_code` — the typed-code start's door. The user
//!   opens Fauna, types the code a device shows, and gets back the pending row
//!   the card renders; the row is claimed to the caller's account by the same
//!   act, after which it is listed and resolved like any other.
//! * `fauna.oauth.consent.open_handoff` — the same-device handoff's door. The
//!   user's Fauna app was opened on `fauna://consent/<request_uri>`; the kind
//!   spends that pushed request and opens its row assigned to the caller.
//! * `fauna.oauth.consent.block_client` — the per-client "never show requests
//!   from this app" block (§ Consent rule (c)), and its lifting. Nest state,
//!   honoured silently: a blocked client's quiet push opens nothing and its
//!   caller is answered exactly as an unresolved hint is.
//! * `fauna.oauth.consent.list_blocked_clients` — the read half of the block,
//!   so the user can see and lift what they set from their own app.
//!
//! # Why `fauna.oauth.*` and not `fauna.bridges.atproto.*`
//!
//! The consent starts are the nest's own authorization server's
//! (§ The issuer rules that namespace for it), and they answer in every nest
//! flavor, bridge or none. The two built card kinds keep their names; a rename
//! would be a wire break bought for tidiness.

use crate::Value;
use crate::atproto_pds::PendingConsentRow;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// `fauna.oauth.consent.lookup_code` — USER class. Claim the live typed-code
/// request whose code the user typed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LookupConsentCodeRequest {
    /// As typed. Hyphens, spaces and letter case are insignificant.
    pub user_code: String,
    /// Forward-compat catch-all (transport.md § Schema and forward-compat
    /// discipline, rule 4): app-callable, so both directions tolerate a newer
    /// peer's added field.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LookupConsentCodeReply {
    /// The row the card renders, now the caller's — or `None`. One answer for
    /// every miss (no such code, expired, answered, claimed by another
    /// account), so the reply tells a caller nothing about codes it does not
    /// hold.
    #[serde(default)]
    pub consent: Option<PendingConsentRow>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.oauth.consent.open_handoff` — USER class. Open the pushed request
/// a `fauna://consent/<request_uri>` route carried, as the caller's own row.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OpenHandoffRequest {
    /// The PAR handle, exactly as the route carried it
    /// (`urn:ietf:params:oauth:request_uri:…`).
    pub request_uri: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OpenHandoffReply {
    /// The row the card renders, assigned to the caller — or `None`. One
    /// answer for every miss (no such handle, expired, already spent by either
    /// door, malformed, a `login_hint` naming another account), so the reply
    /// tells a caller nothing about handles it does not hold.
    #[serde(default)]
    pub consent: Option<PendingConsentRow>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.oauth.consent.block_client` — USER class. Set or lift the caller's
/// block on one client.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BlockClientRequest {
    /// Verbatim, as the card shows it.
    pub client_id: String,
    /// `true` blocks; `false` lifts the block. Idempotent both ways.
    pub blocked: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BlockClientReply {
    /// Whether the client is blocked now.
    pub blocked: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.oauth.consent.list_blocked_clients` — USER class. No parameters.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListBlockedClientsRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListBlockedClientsReply {
    /// Oldest first.
    pub clients: Vec<BlockedClient>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BlockedClient {
    pub client_id: String,
    /// When the block was set, unix milliseconds.
    pub blocked_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}
