//! The renderable connected-apps page (`docs/goal/ui/connected-apps.md`
//! § State & data shape): `ConnectedAppsSnapshot { loaded, requests,
//! principals }` plus the blocked-clients read, the open oracle record and
//! the page error.

use fauna_core::localized::LocalizedText;
use serde::{Deserialize, Serialize};

pub use fauna_atproto_settings_machine::{ConsentCardRow, ConsentSetRow};

/// A row's credential class — the page's grouping key and badge
/// (`third-party.md` § The roster model: "execution-form badge"). A principal
/// row carries its `execution_form` verbatim (`remote` | `device` today,
/// `wasm` | `container` once hosted code runs), so an app meeting a class it
/// does not know renders the row without a badge rather than guessing.
pub mod class {
    /// A remote server that authenticated as a confidential client.
    pub const REMOTE: &str = "remote";
    /// A public client — a device app.
    pub const DEVICE: &str = "device";
    /// Hosted code, the WASM execution form.
    pub const WASM: &str = "wasm";
    /// Hosted code, the curated-container execution form.
    pub const CONTAINER: &str = "container";
    /// A client signed in with an app password: an ATProto app-credential
    /// session, or a mail app password (the row then carries
    /// [`super::ConnectedAppRow::mail`]).
    pub const APP_PASSWORD: &str = "app_password";
    /// A NIP-46 signer client (a Nostr Connect app).
    pub const SIGNER: &str = "signer";
    /// An OAuth grant read from a nest too old to answer the principal
    /// roster — the execution form is not known.
    pub const OAUTH: &str = "oauth";
}

/// The i18n template a verbatim string (a resolved client name, a described
/// scope) rides through, so every row field is one [`LocalizedText`] type.
pub const VERBATIM_KEY: &str = "connected_apps.verbatim";

/// One roster row: one thing acting for the user from outside the seven apps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ConnectedAppRow {
    /// Opaque handle [`crate::ConnectedAppsMachine::revoke`] takes. The machine
    /// encodes the credential class in it, which is how it — never the app —
    /// picks the revoke verb.
    pub key: String,
    /// One of [`class`]'s values, or a principal's execution form verbatim.
    pub class: String,
    /// The display name: the resolved client name (control-stripped), else the
    /// client id verbatim, else a translated fallback.
    pub name: LocalizedText,
    /// The client's self-authenticating identity (its metadata document URL),
    /// rendered verbatim beside the name. `None` for a row that has none (an
    /// app-password session, a signer client).
    pub client_id: Option<String>,
    /// The publisher domain — the host of `client_id`.
    pub publisher: Option<String>,
    /// What the row may reach, in words, through the one describe path the
    /// consent card uses.
    pub scope_descriptions: Vec<LocalizedText>,
    /// The permission sets the row's permissions came from — the provenance
    /// the consent card showed at approval, composed by the card's own
    /// function (`atproto-oauth-provider.md` § Implementation status today).
    /// Each set once, over the client's live OAuth grants; empty for a row
    /// whose grants named none and for every non-principal row.
    #[cfg_attr(feature = "uniffi", uniffi(default = []))]
    pub sets: Vec<ConsentSetRow>,
    /// Epoch milliseconds.
    pub created_at_millis: i64,
    /// Epoch milliseconds; `None` when never used (or not known).
    pub last_used_at_millis: Option<i64>,
    /// *Lasts-until*, epoch milliseconds; `None` for an open-ended row.
    pub lasts_until_millis: Option<i64>,
    /// Whether the row can act right now. A principal with no live grant
    /// family, a suspended grant or a pending signer is listed and revocable
    /// but not connected.
    pub connected: bool,
    /// The account's external-apps switch is OFF and this row is on the
    /// external-app plane it suspends — a principal or an ATProto
    /// app-password session (`atproto-pds-full.md` § F1 detail, the
    /// kill-switch bullet). A paused row is not [`Self::connected`], stays
    /// listed and revocable, and works again with nothing redone once the
    /// switch is back ON; apps word it as paused, not as merely not
    /// connected.
    pub paused: bool,
    /// `Some` on a mail app-password row, and only there: the login, kind and
    /// burned state the row shows beside the columns every row has.
    pub mail: Option<MailAppPassword>,
    /// A NIP-46 signer row the identity-succession burn killed and kept
    /// (`ui/nostr.md` § Key succession and rotation): listed so the successor
    /// sees which apps held standing authority, not connected, and Revoke
    /// removes it — the mail row's `revoked`, for the signer class.
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    pub signer_burned: bool,
    /// `Some` on a principal row the oracle's custodians have served, or an
    /// ATProto app-password session that has written through the
    /// `atproto.commit` door, and only there: what they did for it
    /// (`connected-apps.md` § Layout & flow, region 3). A principal row's
    /// per-operation record behind it is read on demand
    /// ([`crate::ConnectedAppsMachine::open_record`]).
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub oracle: Option<OracleSummaryRow>,
}

/// The oracle summary a served principal's row carries
/// (`third-party.md` § The roster model, the oracle record's columns), summed
/// across the custodians that served it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct OracleSummaryRow {
    /// Key operations admitted in the rate ceiling's current window.
    pub ops_in_window: u32,
    /// Every request a custodian served for the principal, key-less ones
    /// (connect, ping, the public-key read) included.
    pub requests_served: u64,
    /// The newest operation; `None` before the first.
    pub last_op: Option<OracleOpRow>,
}

/// One key operation a custodian performed for a principal — a row of the
/// per-operation record, and the summary's newest operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct OracleOpRow {
    /// Epoch milliseconds.
    pub at_millis: i64,
    /// The `identity.op` class verbatim (`nostr.sign_event`, …) — a value a
    /// reader can match on; the words below are what a user reads.
    pub class: String,
    /// The class in words, through the one describe path the consent card
    /// uses for its `fauna:identity:op:<class>` scope — never a second
    /// wording. A class this build does not know reads as its name.
    pub words: LocalizedText,
    /// The class's own detail (the event kind, the method, `<method> <host>`),
    /// verbatim and control-stripped.
    pub detail: LocalizedText,
}

/// One principal's per-operation record, open on the page
/// ([`crate::ConnectedAppsMachine::open_record`]). At most one is open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct OracleRecord {
    /// The row key it belongs to.
    pub key: String,
    /// Whether the read has returned — the three-state list rule
    /// (`docs/goal/ui/README.md` § List pages): before it, the record paints
    /// neither operations nor its empty state.
    pub loaded: bool,
    /// Newest first, at most `fauna_protocol::principals::PRINCIPAL_OPS_PAGE`.
    pub ops: Vec<OracleOpRow>,
}

/// What a mail app-password row carries beyond the common columns
/// (`docs/goal/ui/connected-apps.md` § Layout & flow). The secret is never
/// here — the passive snapshot carries no secrets; it is read on demand with
/// [`crate::ConnectedAppsMachine::reveal_secret`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MailAppPassword {
    /// The login a mail app uses, with only `{handle}` left for the renderer
    /// to substitute (`fauna_client_mail_settings::resolve_mua_username`).
    pub mua_username: String,
    /// The kind badge — "Password" / "Bearer token" — from the mail machine's
    /// one `credential_kind_badge`.
    pub kind: LocalizedText,
    /// The identity-succession burn killed this password and kept the row
    /// (`mail-credentials.md` § Rotation and recovery → *Succession*): its
    /// secret is gone and the login authenticates nothing. The row stays so
    /// the user sees which mail apps to set up again; Revoke removes it.
    pub revoked: bool,
}

/// One per-client block — "Never show requests from this app".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct BlockedAppRow {
    /// Verbatim, as the card showed it; what `unblock` takes.
    pub client_id: String,
    /// Epoch milliseconds.
    pub blocked_at_millis: i64,
}

/// The whole renderable page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ConnectedAppsSnapshot {
    /// Whether the roster read has ever returned. The three-state list rule
    /// (`docs/goal/ui/README.md` § List pages): before the first read the page
    /// paints neither rows nor its empty state — an empty roster is a claim
    /// only a read that returned may make.
    pub loaded: bool,
    /// The Requests tray: every live consent addressed to this account, as the
    /// built consent card, minus requests from a blocked client. Rendered only
    /// while non-empty — there is no "no requests" row.
    pub requests: Vec<ConsentCardRow>,
    /// The roster, oldest first.
    pub principals: Vec<ConnectedAppRow>,
    /// The per-client blocks, oldest first.
    pub blocked: Vec<BlockedAppRow>,
    /// The open per-operation record, if any.
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub record: Option<OracleRecord>,
    /// The page error (`error-message`), if the last gesture or read failed.
    pub error: Option<LocalizedText>,
}
