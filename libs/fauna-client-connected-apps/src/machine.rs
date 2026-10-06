//! The connected-apps page machine.
//!
//! Each app holds an `Arc<ConnectedAppsMachine>`, drives its gestures and paints
//! `snapshot()`. `std::sync::Mutex`, never held across an `await` — the shape
//! every page machine in `libs/` shares.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use fauna_atproto_settings_machine::consent_grant::{self, AnswerError};
use fauna_atproto_settings_machine::{
    ConsentCardRow, ConsentGrantSeams, NestConsentRow, consent_card_row, consent_folder_names,
    scope_words,
};
use fauna_client_capabilities::grant_log;
use fauna_client_capabilities::view_model::{
    PrincipalKeys, ReplacedKey, grants_ended_by_key_replacement, principal_trust_facet,
    replaced_key,
};
use fauna_client_mail_settings::{MailCredentialSummary, credential_kind_badge};
use fauna_core::control_chars::strip_control_chars;
use fauna_core::localized::LocalizedText;
use fauna_core::secret::SecretString;
use fauna_core::succession_ledger::SuccessionLedger;
use fauna_protocol::atproto_pds::{AtprotoGrantInfo, AtprotoSessionInfo};
use fauna_protocol::nostr::BunkerAppEntry;
use fauna_protocol::principals::PrincipalInfo;

use crate::nest_api::{ConnectedAppsApiError, ConnectedAppsNestApi};
use crate::observer::ConnectedAppsObserver;
use crate::snapshots::{
    BlockedAppRow, ConnectedAppRow, ConnectedAppsSnapshot, MailAppPassword, VERBATIM_KEY, class,
};

/// "Failed to load connected apps: {message}".
const REFRESH_ERROR_KEY: &str = "connected_apps.error_refresh";
/// "Failed to disconnect the app: {message}" — the row stays.
const REVOKE_ERROR_KEY: &str = "connected_apps.error_revoke";
/// "Failed to answer the request: {message}".
const RESOLVE_ERROR_KEY: &str = "connected_apps.error_resolve";
/// "That request is no longer waiting — it was answered or it expired."
const REQUEST_GONE_KEY: &str = "connected_apps.error_request_gone";
/// "That code has expired — ask the app for a new one." The ONE answer for
/// every miss, so unknown and expired are never a distinguishable pair
/// (`connected-apps.md` § User actions).
const CODE_EXPIRED_KEY: &str = "connected_apps.error_code_expired";
/// "That link has expired or was already used — start again from the app."
/// The ONE answer for every same-device handoff miss, for the same reason as
/// the typed code's (`authorization-server.md` § Consent → *How the
/// same-device handoff is built*).
const HANDOFF_EXPIRED_KEY: &str = "connected_apps.error_handoff_expired";
/// The card's line when an approve replaces the app's holder key: every
/// third-party grant to the old key ends (`third-party.md` § The principal
/// model → *Key replacement*).
const ENDS_HOLDER_KEY: &str = "connected_apps.consent_ends_holder";
/// The card's line when an approve replaces only the app's writer key: the
/// grants letting the old key write end.
const ENDS_WRITER_KEY: &str = "connected_apps.consent_ends_writer";
/// "Failed to block the app: {message}".
const BLOCK_ERROR_KEY: &str = "connected_apps.error_block";
/// Fallback name for a row with no resolved name and no client id.
const UNNAMED_KEY: &str = "connected_apps.unnamed";
/// A signer client still waiting for the app to connect.
const SIGNER_PENDING_KEY: &str = "connected_apps.signer_pending";
/// A NIP-46 signer's one scope, in words — it may ask the nest to sign Nostr
/// events with the user's key (`third-party.md` § The roster model:
/// `fauna:identity:op:nostr.*`).
const SIGNER_SCOPE_KEY: &str = "connected_apps.scope_nostr_sign";
/// A mail app password's reach, in words: the one credential signs a mail app
/// in to mail, calendar, contacts and files alike (`mail-settings.md`
/// § Layout & flow).
const MAIL_SCOPE_KEY: &str = "connected_apps.scope_mail";
/// "Could not read the secret: {message}".
const SECRET_ERROR_KEY: &str = "connected_apps.error_secret";

/// Row-key prefixes. The key is how the machine — never the app — picks the
/// revoke verb for a row (`connected-apps.md` § User actions).
const PRINCIPAL: &str = "principal:";
const SESSION: &str = "session:";
const SIGNER: &str = "signer:";
const MAIL: &str = "mail:";

#[derive(Default)]
struct State {
    loaded: bool,
    requests: Vec<ConsentCardRow>,
    /// The raw rows behind [`Self::requests`] — what an approve reads the
    /// attested keys and the manifest from (`consent_grant`).
    request_rows: Vec<NestConsentRow>,
    /// The owner's name for each folder a pending request's `folder:read`
    /// scope names, from the consent seams' folder custody
    /// (`consent_folder_names`) — what the card words the read row with.
    folder_names: BTreeMap<i64, String>,
    principals: Vec<ConnectedAppRow>,
    /// Each principal row's attested keys, as the last roster read named
    /// them — what a revoke records its `Revoke`s against, and what an
    /// approve compares the consent's attested keys to.
    principal_keys: Vec<RosterKeys>,
    blocked: Vec<BlockedAppRow>,
    error: Option<LocalizedText>,
}

/// One roster row's keys, by its row key and its `client_id`.
#[derive(Clone)]
struct RosterKeys {
    key: String,
    client_id: String,
    keys: PrincipalKeys,
}

impl State {
    /// The keys the roster names for `client_id` — empty for a client the
    /// account holds no row for (a first ceremony).
    fn roster_keys(&self, client_id: &str) -> PrincipalKeys {
        self.principal_keys
            .iter()
            .find(|r| r.client_id == client_id)
            .map(|r| r.keys)
            .unwrap_or_default()
    }

    /// What approving `row` would replace of the roster's keys.
    fn replaced_by(&self, row: &NestConsentRow) -> Option<ReplacedKey> {
        replaced_key(self.roster_keys(&row.client_id), attested_keys(row))
    }

    /// The card for `row`, saying what its approve ends.
    fn card(&self, row: NestConsentRow) -> ConsentCardRow {
        let folders = &self.folder_names;
        let ends = self.replaced_by(&row).map(|r| {
            LocalizedText::key(match r {
                ReplacedKey::Holder(_) => ENDS_HOLDER_KEY,
                ReplacedKey::Writer { .. } => ENDS_WRITER_KEY,
            })
        });
        ConsentCardRow {
            ends,
            ..consent_card_row(row, folders)
        }
    }
}

#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct ConnectedAppsMachine {
    state: Mutex<State>,
    observer: Arc<dyn ConnectedAppsObserver>,
    nest_api: Arc<dyn ConnectedAppsNestApi>,
    /// The consent-time grant's owner-side seams, wired post-construction by
    /// the host ([`Self::set_consent_grant_seams`]). `None` refuses an approve
    /// naming a `fauna:records:` scope rather than resolving it keyless.
    consent_grants: Mutex<Option<Arc<ConsentGrantSeams>>>,
}

impl ConnectedAppsMachine {
    /// Not a `#[uniffi::constructor]` — the seam has no FFI ABI. Apps build via
    /// `build_connected_apps_machine`; tests pass a `FakeConnectedAppsNestApi`.
    pub fn new(
        observer: Arc<dyn ConnectedAppsObserver>,
        nest_api: Arc<dyn ConnectedAppsNestApi>,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            observer,
            nest_api,
            consent_grants: Mutex::new(None),
        })
    }

    /// Wire the consent-time grant's seams — the `set_*` seam pattern, never
    /// over UniFFI. Without them an approve of a records consent is refused.
    pub fn set_consent_grant_seams(&self, seams: Arc<ConsentGrantSeams>) {
        *self.consent_grants.lock().unwrap() = Some(seams);
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl ConnectedAppsMachine {
    pub fn snapshot(&self) -> ConnectedAppsSnapshot {
        let s = self.state.lock().unwrap();
        ConnectedAppsSnapshot {
            loaded: s.loaded,
            requests: s.requests.clone(),
            principals: s.principals.clone(),
            blocked: s.blocked.clone(),
            error: s.error.clone(),
        }
    }

    /// Re-read the whole page. Non-optimistic: every gesture below ends here,
    /// so what paints is what the nest now says. Notifies once.
    pub async fn refresh(&self) {
        self.reload().await;
        self.observer.on_changed();
    }

    /// *Connect an app* — claim the typed code and show its card in the tray.
    /// Any miss (unknown, expired, answered, someone else's) is one error.
    pub async fn submit_code(&self, code: String) {
        if code.trim().is_empty() {
            self.set_error(LocalizedText::key(CODE_EXPIRED_KEY));
            self.observer.on_changed();
            return;
        }
        match self.nest_api.lookup_code(code).await {
            Ok(Some(row)) => {
                self.state.lock().unwrap().error = None;
                self.reload().await;
                self.ensure_request(row).await;
            }
            Ok(None) => self.set_error(LocalizedText::key(CODE_EXPIRED_KEY)),
            Err(e) => self.set_error(LocalizedText::key(CODE_EXPIRED_KEY).with_log(e.detail())),
        }
        self.observer.on_changed();
    }

    /// The same-device handoff — open the pending request whose PAR handle a
    /// `fauna://consent/<request_uri>` route carried and show its card in the
    /// tray. Navigation only: the card is revealed, never answered. Any miss
    /// (unknown, expired, spent by either door, someone else's) is one error,
    /// over the tray as the nest now lists it.
    pub async fn open_handoff(&self, request_uri: String) {
        match self.nest_api.open_handoff(request_uri).await {
            Ok(Some(row)) => {
                self.state.lock().unwrap().error = None;
                self.reload().await;
                self.ensure_request(row).await;
            }
            Ok(None) => {
                self.set_error(LocalizedText::key(HANDOFF_EXPIRED_KEY));
                self.reload_keeping_error().await;
            }
            Err(e) => {
                tracing::warn!(target: "fauna_connected_apps", "open_handoff: {}", e.detail());
                self.set_error(LocalizedText::key(HANDOFF_EXPIRED_KEY));
                self.reload_keeping_error().await;
            }
        }
        self.observer.on_changed();
    }

    /// Approve or Decline one request in the tray (`resolve_consent`). A
    /// decline is recorded nest-side, never dismissed locally. An approve
    /// naming a `fauna:records:` scope mints and deposits the app's
    /// consent-time grant first (`fauna_atproto_settings_machine::
    /// consent_grant`, the AT Protocol page's same path). An approve whose
    /// attested key replaces the roster's then ends the old key's grants
    /// ([`Self::end_replaced_key_grants`]) — only once the resolve answered
    /// live, so a failed approve ends nothing.
    pub async fn resolve_request(&self, consent_id_hex: String, approved: bool) {
        let Ok(id) = hex::decode(&consent_id_hex) else {
            return;
        };
        let row = self
            .state
            .lock()
            .unwrap()
            .request_rows
            .iter()
            .find(|r| r.consent_id == id)
            .cloned();
        // A decline of a row this tray never listed mints nothing; an approve
        // reads what it grants off the row, so it needs one.
        let row = match row {
            Some(row) => row,
            None if !approved => NestConsentRow {
                consent_id: id,
                ..NestConsentRow::default()
            },
            None => {
                self.set_error(LocalizedText::key(REQUEST_GONE_KEY));
                self.reload_keeping_error().await;
                self.observer.on_changed();
                return;
            }
        };
        let replaced = approved
            .then(|| self.state.lock().unwrap().replaced_by(&row))
            .flatten();
        let seams = self.consent_grants.lock().unwrap().clone();
        let nest = &self.nest_api;
        let answer = consent_grant::answer_consent(
            seams.as_deref(),
            &row,
            approved,
            |blob| nest.mint_grant(blob),
            |id, approved| nest.resolve_consent(id, approved),
            |grant_id| nest.revoke_grant(grant_id),
        )
        .await;
        if let (Ok(true), Some(replaced)) = (&answer, replaced) {
            self.end_replaced_key_grants(replaced).await;
        }
        match answer {
            Ok(true) => self.state.lock().unwrap().error = None,
            Ok(false) => self.set_error(LocalizedText::key(REQUEST_GONE_KEY)),
            Err(AnswerError::Nest(e)) => self.set_error(keyed(RESOLVE_ERROR_KEY, e.detail())),
            Err(AnswerError::Grant(e)) => self.set_error(keyed(RESOLVE_ERROR_KEY, &e.to_string())),
        }
        self.reload_keeping_error().await;
        self.observer.on_changed();
    }

    /// *Never show requests from this app* — block the request's client (nest
    /// state) and decline the live request, so it leaves the tray and the
    /// client's wait ends. No future request from that client renders.
    pub async fn block_request(&self, consent_id_hex: String) {
        let client_id = {
            let s = self.state.lock().unwrap();
            s.requests
                .iter()
                .find(|r| r.consent_id_hex == consent_id_hex)
                .map(|r| r.client_id.clone())
        };
        let Some(client_id) = client_id else {
            return;
        };
        match self.nest_api.block_client(client_id, true).await {
            Ok(_) => {
                self.state.lock().unwrap().error = None;
                if let Ok(id) = hex::decode(&consent_id_hex) {
                    // Best-effort: the block alone already hides the card; a
                    // failed decline leaves a row that simply expires.
                    let _ = self.nest_api.resolve_consent(id, false).await;
                }
            }
            Err(e) => self.set_error(keyed(BLOCK_ERROR_KEY, e.detail())),
        }
        self.reload_keeping_error().await;
        self.observer.on_changed();
    }

    /// Lift a per-client block.
    pub async fn unblock(&self, client_id: String) {
        match self.nest_api.block_client(client_id, false).await {
            Ok(_) => self.state.lock().unwrap().error = None,
            Err(e) => self.set_error(keyed(BLOCK_ERROR_KEY, e.detail())),
        }
        self.reload_keeping_error().await;
        self.observer.on_changed();
    }

    /// Revoke one roster row. The verb is chosen here from the row's class:
    /// a principal's delete, an ATProto session's `revoke_session` (which
    /// cascades to the OAuth grant whose id it is), a signer's bunker revoke,
    /// a mail app password's `RevokeCredential`.
    /// A failure leaves the row and sets the page error.
    pub async fn revoke(&self, key: String) {
        let result = if let Some(hex_id) = key.strip_prefix(PRINCIPAL) {
            match hex::decode(hex_id) {
                Ok(id) => {
                    let result = self.nest_api.revoke_principal(id).await;
                    if result.is_ok() {
                        self.record_principal_revoke(&key).await;
                    }
                    result
                }
                Err(_) => return,
            }
        } else if let Some(hex_id) = key.strip_prefix(SESSION) {
            match hex::decode(hex_id) {
                Ok(id) => self.nest_api.revoke_session(id).await,
                Err(_) => return,
            }
        } else if let Some(id) = key.strip_prefix(SIGNER) {
            match id.parse::<i64>() {
                Ok(id) => self.nest_api.revoke_bunker_app(id).await,
                Err(_) => return,
            }
        } else if let Some(credential_id) = key.strip_prefix(MAIL) {
            self.nest_api
                .revoke_mail_credential(credential_id.to_string())
                .await
        } else {
            return;
        };
        match result {
            // `false` is "already gone" — the end state the user asked for.
            Ok(_) => self.state.lock().unwrap().error = None,
            Err(e) => self.set_error(keyed(REVOKE_ERROR_KEY, e.detail())),
        }
        self.reload_keeping_error().await;
        self.observer.on_changed();
    }

    /// Read one mail app password's secret on demand, so the user can set a
    /// mail app up again without revoking and re-adding the password. `None`
    /// for a row that has no secret (every class but a mail app password) and
    /// on a failed read, which also sets the page error. The secret never
    /// enters the snapshot.
    pub async fn reveal_secret(&self, key: String) -> Option<SecretString> {
        let credential_id = key.strip_prefix(MAIL)?.to_string();
        match self.nest_api.reveal_mail_secret(credential_id).await {
            Ok(secret) => {
                self.state.lock().unwrap().error = None;
                self.observer.on_changed();
                Some(secret)
            }
            Err(e) => {
                self.set_error(keyed(SECRET_ERROR_KEY, e.detail()));
                self.observer.on_changed();
                None
            }
        }
    }
}

impl ConnectedAppsMachine {
    fn set_error(&self, err: LocalizedText) {
        tracing::warn!(target: "fauna_connected_apps", "{}", err.log_line());
        self.state.lock().unwrap().error = Some(err);
    }

    fn consent_grant_seams(&self) -> Option<Arc<ConsentGrantSeams>> {
        self.consent_grants.lock().unwrap().clone()
    }

    /// The `Revoke`s `fauna.principals.revoke` owes the owner's log: the nest
    /// has just deleted every capability grant minted to the principal's key,
    /// so each one the log still holds live is recorded as ended
    /// (`third-party.md` § The principal model). Best-effort, like the
    /// consent-grant withdrawal: the revoke itself already landed, so a
    /// failure here is logged, never the page error. Nothing to record on a
    /// machine without the seams or for a principal that attested no key.
    async fn record_principal_revoke(&self, key: &str) {
        let Some(seams) = self.consent_grant_seams() else {
            return;
        };
        let holder = {
            let s = self.state.lock().unwrap();
            s.principal_keys
                .iter()
                .find(|r| r.key == key)
                .and_then(|r| r.keys.holder)
        };
        let Some(holder) = holder else {
            return;
        };
        if let Err(e) = grant_log::record_principal_revoke(
            seams.ledger.as_ref(),
            seams.signer.as_ref(),
            seams.actor_id,
            holder,
            now_epoch_secs(),
        )
        .await
        {
            tracing::warn!(
                target: "fauna_connected_apps",
                error = %e,
                "principal revoked; its ended grants' Revoke events were not recorded"
            );
        }
    }

    /// The owner's end of a key replacement: the approve the user just made
    /// replaced the key the roster named, and the nest will end the grants
    /// the old key reached when the app redeems (`third-party.md` § The
    /// principal model → *Key replacement*). Each grant the owner's log holds
    /// live for it is revoked on the nest first (revoke narrows, so the nest
    /// leads) and its `Revoke` recorded after; a grant the nest refused is
    /// left unrecorded. Best-effort like [`Self::record_principal_revoke`]:
    /// the approve already landed, so a miss is logged, never the page error,
    /// and the nest's own end at `/oauth/token` is the guard either way.
    async fn end_replaced_key_grants(&self, replaced: ReplacedKey) {
        let Some(seams) = self.consent_grant_seams() else {
            return;
        };
        let log = match seams.ledger.load().await {
            Ok(log) => log,
            Err(e) => {
                tracing::warn!(
                    target: "fauna_connected_apps",
                    error = %e,
                    "key replaced; grant log unreadable, its ended grants were not recorded"
                );
                return;
            }
        };
        let mut ended = Vec::new();
        for grant in grants_ended_by_key_replacement(&log, replaced) {
            let (Ok(grant_id), Ok(holder)) = (
                <[u8; 16]>::try_from(grant.grant_id.as_slice()),
                <[u8; 32]>::try_from(grant.holder.as_slice()),
            ) else {
                continue;
            };
            match self.nest_api.revoke_grant(grant_id).await {
                Ok(()) => ended.push((grant_id, holder)),
                Err(e) => tracing::warn!(
                    target: "fauna_connected_apps",
                    "key replaced; the nest refused to end a grant of the old key: {}",
                    e.detail()
                ),
            }
        }
        if let Err(e) = grant_log::record_revokes(
            seams.ledger.as_ref(),
            seams.signer.as_ref(),
            seams.actor_id,
            &ended,
            now_epoch_secs(),
        )
        .await
        {
            tracing::warn!(
                target: "fauna_connected_apps",
                error = %e,
                "key replaced; its ended grants' Revoke events were not recorded"
            );
        }
    }

    /// The owner's grant log, when the seams are wired and it reads — the
    /// source of a principal's trust facet. `None` degrades every principal
    /// row to its roster-only *lasts-until*, never the page error.
    async fn load_grant_log(&self) -> Option<SuccessionLedger> {
        let seams = self.consent_grant_seams()?;
        match seams.ledger.load().await {
            Ok(ledger) => Some(ledger),
            Err(e) => {
                tracing::warn!(
                    target: "fauna_connected_apps",
                    error = %e,
                    "grant log unreadable; principal rows fall back to their OAuth horizon"
                );
                None
            }
        }
    }

    /// The claimed or opened row is an ordinary pending row, so the re-list
    /// carries it; should that read have failed, the row the nest answered
    /// still paints — it is the card the user just asked for.
    async fn ensure_request(&self, row: NestConsentRow) {
        let folders = consent_folder_names(
            self.consent_grant_seams().as_deref(),
            std::slice::from_ref(&row),
        )
        .await;
        let mut s = self.state.lock().unwrap();
        s.folder_names.extend(folders);
        let card = s.card(row.clone());
        if !s
            .requests
            .iter()
            .any(|r| r.consent_id_hex == card.consent_id_hex)
        {
            s.requests.insert(0, card);
        }
        if !s
            .request_rows
            .iter()
            .any(|r| r.consent_id == row.consent_id)
        {
            s.request_rows.insert(0, row);
        }
    }

    /// Re-read after a gesture without clearing that gesture's error.
    async fn reload_keeping_error(&self) {
        let err = self.state.lock().unwrap().error.clone();
        self.reload().await;
        if err.is_some() {
            self.state.lock().unwrap().error = err;
        }
    }

    /// Every read, composed. Each read degrades on its own: a failure keeps
    /// what is on screen and sets the page error; an `Unavailable` read (a
    /// feature-excised surface, one this account lacks) contributes nothing and is no
    /// error. The mail app passwords are read through the mail-settings
    /// machine, not the nest's roster, so their failure keeps the mail rows
    /// already on screen without holding back the rest of the roster.
    async fn reload(&self) {
        let mut error: Option<LocalizedText> = None;
        let mut fail = |e: ConnectedAppsApiError| {
            if !matches!(e, ConnectedAppsApiError::Unavailable { .. }) && error.is_none() {
                error = Some(keyed(REFRESH_ERROR_KEY, e.detail()));
            }
        };

        let principals = unavailable_is_empty(self.nest_api.list_principals().await);
        let grants = unavailable_is_empty(self.nest_api.list_grants().await);
        let sessions = unavailable_is_empty(self.nest_api.list_sessions().await);
        let bunker = unavailable_is_empty(self.nest_api.list_bunker_apps().await);
        let mail = unavailable_is_empty(self.nest_api.list_mail_credentials().await);
        let consents = unavailable_is_empty(self.nest_api.list_pending_consents().await);
        let blocked = unavailable_is_empty(self.nest_api.list_blocked_clients().await);
        let grant_log = self.load_grant_log().await;

        let mail = match mail {
            Ok(mail) => Some(mail),
            Err(e) => {
                fail(e);
                None
            }
        };
        let roster = match (principals, grants, sessions, bunker) {
            (Ok(principals), Ok(grants), Ok(sessions), Ok(bunker)) => {
                let fresh_mail = mail.is_some();
                let holders = principals
                    .iter()
                    .map(|p| RosterKeys {
                        key: principal_key(p),
                        client_id: p.client_id.clone(),
                        keys: PrincipalKeys {
                            holder: holder_key(p),
                            writer: key32(p.writer_ed25519.as_deref().map(|w| w.as_slice())),
                        },
                    })
                    .collect::<Vec<_>>();
                let rows = compose_roster(
                    principals,
                    grants,
                    sessions,
                    bunker,
                    mail.unwrap_or_default(),
                    grant_log.as_ref(),
                );
                Some((rows, holders, fresh_mail))
            }
            (p, g, s, b) => {
                for e in [p.err(), g.err(), s.err(), b.err()].into_iter().flatten() {
                    fail(e);
                }
                None
            }
        };
        let blocked = match blocked {
            Ok(b) => Some(
                b.into_iter()
                    .map(|b| BlockedAppRow {
                        client_id: b.client_id,
                        blocked_at_millis: b.blocked_at,
                    })
                    .collect::<Vec<_>>(),
            ),
            Err(e) => {
                fail(e);
                None
            }
        };
        let consents = match consents {
            Ok(c) => Some(c),
            Err(e) => {
                fail(e);
                None
            }
        };
        let folder_names = match &consents {
            Some(rows) => {
                Some(consent_folder_names(self.consent_grant_seams().as_deref(), rows).await)
            }
            None => None,
        };

        let mut s = self.state.lock().unwrap();
        if let Some((mut roster, holders, fresh_mail)) = roster {
            if !fresh_mail {
                roster.extend(s.principals.iter().filter(|r| r.mail.is_some()).cloned());
                sort_roster(&mut roster);
            }
            s.principals = roster;
            s.principal_keys = holders;
            s.loaded = true;
        }
        if let Some(blocked) = blocked {
            s.blocked = blocked;
        }
        if let Some(folder_names) = folder_names {
            s.folder_names = folder_names;
        }
        if let Some(consents) = consents {
            let blocked = &s.blocked;
            let rows: Vec<NestConsentRow> = consents
                .into_iter()
                .filter(|c| !blocked.iter().any(|b| b.client_id == c.client_id))
                .collect();
            let cards = rows.iter().cloned().map(|r| s.card(r)).collect();
            s.requests = cards;
            s.request_rows = rows;
        }
        if let Some(err) = &error {
            tracing::warn!(target: "fauna_connected_apps", "{}", err.log_line());
        }
        s.error = error;
    }
}

fn keyed(key: &str, detail: &str) -> LocalizedText {
    LocalizedText::key_arg(key, "message", detail.to_string())
}

fn verbatim(text: &str) -> LocalizedText {
    LocalizedText::key_arg(VERBATIM_KEY, "text", strip_control_chars(text).into_owned())
}

trait WithLog {
    fn with_log(self, detail: &str) -> Self;
}

impl WithLog for LocalizedText {
    /// The code miss's text stays the one uniform sentence; the transport
    /// detail goes to the log only, never to the user.
    fn with_log(self, detail: &str) -> Self {
        tracing::warn!(target: "fauna_connected_apps", "lookup_code: {detail}");
        self
    }
}

fn unavailable_is_empty<T>(
    r: Result<Vec<T>, ConnectedAppsApiError>,
) -> Result<Vec<T>, ConnectedAppsApiError> {
    match r {
        Err(ConnectedAppsApiError::Unavailable { .. }) => Ok(Vec::new()),
        other => other,
    }
}

/// The publisher domain: the host of a client-id URL. `None` when the id is
/// not a URL with a host.
fn publisher_domain(client_id: &str) -> Option<String> {
    let rest = client_id.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if host.starts_with('[') {
        host.split_once(']').map(|(h, _)| format!("{h}]"))?
    } else {
        host.split(':').next()?.to_string()
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// A row name: the resolved client name, else the client id, else the
/// translated fallback — control-stripped at composition (the fence the
/// consent card documents: a name carrying newlines would own painted rows).
fn row_name(label: Option<&str>, client_id: Option<&str>) -> LocalizedText {
    match (label.filter(|l| !l.trim().is_empty()), client_id) {
        (Some(l), _) => verbatim(l),
        (None, Some(c)) if !c.is_empty() => verbatim(c),
        _ => LocalizedText::key(UNNAMED_KEY),
    }
}

fn principal_key(p: &PrincipalInfo) -> String {
    format!("{PRINCIPAL}{}", hex::encode(&p.principal_id))
}

/// The principal's attested X25519 holder key — `None` for a standard client
/// that presented none.
fn holder_key(p: &PrincipalInfo) -> Option<[u8; 32]> {
    key32(p.holder_x25519.as_deref().map(|h| h.as_slice()))
}

/// A 32-byte key, or `None` for one absent or of the wrong length.
fn key32(key: Option<&[u8]>) -> Option<[u8; 32]> {
    key.and_then(|k| <[u8; 32]>::try_from(k).ok())
}

/// The keys a consent row attests — what its approve hands the principal.
fn attested_keys(row: &NestConsentRow) -> PrincipalKeys {
    PrincipalKeys {
        holder: key32(row.holder_x25519.as_deref()),
        writer: key32(row.writer_ed25519.as_deref()),
    }
}

/// *Lasts-until* for a principal (`connected-apps.md` § State & data shape):
/// the trust facet's — the latest window end among the capability grants the
/// owner's log holds live for its holder key
/// (`fauna_client_capabilities::view_model::principal_trust_facet`). A
/// principal the facet holds no grant for — one that consented to no
/// `records` scope, attested no key, or a machine with no grant log wired —
/// reads the declared fallback, [`oauth_lasts_until`].
fn principal_lasts_until(
    p: &PrincipalInfo,
    grants: &[AtprotoGrantInfo],
    grant_log: Option<&SuccessionLedger>,
) -> Option<i64> {
    let facet = grant_log.zip(holder_key(p)).and_then(|(log, holder)| {
        principal_trust_facet(log, &holder, now_epoch_secs()).lasts_until()
    });
    match facet {
        Some(secs) => Some(secs_to_millis(secs)),
        None => oauth_lasts_until(&p.client_id, grants),
    }
}

/// The fallback *lasts-until*: the latest horizon among the principal's live
/// OAuth grants, or `None` (open-ended) when any live grant has none.
fn oauth_lasts_until(client_id: &str, grants: &[AtprotoGrantInfo]) -> Option<i64> {
    let live: Vec<_> = grants
        .iter()
        .filter(|g| g.client_id == client_id && !g.suspended)
        .collect();
    if live.is_empty() || live.iter().any(|g| g.expires_at.is_none()) {
        return None;
    }
    live.iter().filter_map(|g| g.expires_at).max()
}

fn now_epoch_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs().max(0) as u64
}

fn principal_row(
    p: &PrincipalInfo,
    grants: &[AtprotoGrantInfo],
    grant_log: Option<&SuccessionLedger>,
) -> ConnectedAppRow {
    ConnectedAppRow {
        key: principal_key(p),
        class: p.execution_form.clone(),
        name: row_name(p.label.as_deref(), Some(&p.client_id)),
        client_id: Some(p.client_id.clone()),
        publisher: publisher_domain(&p.client_id),
        scope_descriptions: scope_words(&p.granted_scopes)
            .into_iter()
            .map(|w| verbatim(&w))
            .collect(),
        created_at_millis: p.created_at,
        last_used_at_millis: p.last_used_at,
        lasts_until_millis: principal_lasts_until(p, grants, grant_log),
        connected: p.live_grants > 0,
        mail: None,
    }
}

/// An ATProto app-password session: a client that signed in with one of the
/// user's app passwords.
fn app_password_row(s: &AtprotoSessionInfo) -> ConnectedAppRow {
    let label = s.client_note.as_deref().or(s.credential_id.as_deref());
    ConnectedAppRow {
        key: format!("{SESSION}{}", hex::encode(&s.session_id)),
        class: class::APP_PASSWORD.to_string(),
        name: row_name(label, None),
        client_id: None,
        publisher: None,
        scope_descriptions: Vec::new(),
        created_at_millis: s.created_at,
        last_used_at_millis: s.last_refreshed_at,
        lasts_until_millis: Some(s.expires_at),
        connected: true,
        mail: None,
    }
}

/// A mail app password: what a mail, calendar, contacts or files app signs in
/// with. Open-ended, and never "last used" — the mail custody records no use
/// time. A password the succession burn killed is listed, not connected.
fn mail_row(c: &MailCredentialSummary) -> ConnectedAppRow {
    ConnectedAppRow {
        key: format!("{MAIL}{}", c.credential_id),
        class: class::APP_PASSWORD.to_string(),
        name: row_name(Some(&c.display_name), None),
        client_id: None,
        publisher: None,
        scope_descriptions: vec![LocalizedText::key(MAIL_SCOPE_KEY)],
        created_at_millis: secs_to_millis(c.created_at),
        last_used_at_millis: None,
        lasts_until_millis: None,
        connected: !c.revoked,
        mail: Some(MailAppPassword {
            mua_username: c.mua_username.clone(),
            kind: credential_kind_badge(c.kind),
            revoked: c.revoked,
        }),
    }
}

fn secs_to_millis(secs: u64) -> i64 {
    i64::try_from(secs)
        .unwrap_or(i64::MAX / 1000)
        .saturating_mul(1000)
}

/// A NIP-46 signer client — the bunker roster's row.
fn signer_row(b: &BunkerAppEntry) -> ConnectedAppRow {
    let pending = b.status == "pending";
    let name = if !b.label.trim().is_empty() {
        verbatim(&b.label)
    } else if pending {
        LocalizedText::key(SIGNER_PENDING_KEY)
    } else {
        LocalizedText::key(UNNAMED_KEY)
    };
    ConnectedAppRow {
        key: format!("{SIGNER}{}", b.id),
        class: class::SIGNER.to_string(),
        name,
        client_id: None,
        publisher: None,
        scope_descriptions: vec![LocalizedText::key(SIGNER_SCOPE_KEY)],
        created_at_millis: secs_to_millis(b.created_at),
        last_used_at_millis: b.last_used_at.map(secs_to_millis),
        lasts_until_millis: Some(secs_to_millis(b.expires_at)),
        connected: !pending,
        mail: None,
    }
}

/// The one roster. Every OAuth grant belongs to a principal row and is not
/// listed again (a row is never shown twice). OAuth-plane sessions are always
/// covered by a grant; app-password sessions, signer clients and mail app
/// passwords are rows of their own. Oldest first ([`sort_roster`]).
fn compose_roster(
    principals: Vec<PrincipalInfo>,
    grants: Vec<AtprotoGrantInfo>,
    sessions: Vec<AtprotoSessionInfo>,
    bunker: Vec<BunkerAppEntry>,
    mail: Vec<MailCredentialSummary>,
    grant_log: Option<&SuccessionLedger>,
) -> Vec<ConnectedAppRow> {
    let mut rows: Vec<ConnectedAppRow> = principals
        .iter()
        .map(|p| principal_row(p, &grants, grant_log))
        .collect();
    rows.extend(
        sessions
            .iter()
            .filter(|s| s.plane != "oauth")
            .map(app_password_row),
    );
    rows.extend(bunker.iter().map(signer_row));
    rows.extend(mail.iter().map(mail_row));
    sort_roster(&mut rows);
    rows
}

/// Oldest first. Rows made in the same millisecond order by key, except mail
/// app passwords, which keep the mail machine's own order among themselves
/// (the sort is stable, and they are appended in that order) — custody stamps
/// whole seconds, so a burst of passwords shares one.
fn sort_roster(rows: &mut [ConnectedAppRow]) {
    rows.sort_by(|a, b| {
        a.created_at_millis
            .cmp(&b.created_at_millis)
            .then_with(|| a.mail.is_some().cmp(&b.mail.is_some()))
            .then_with(|| match (&a.mail, &b.mail) {
                (Some(_), Some(_)) => std::cmp::Ordering::Equal,
                _ => a.key.cmp(&b.key),
            })
    });
}

#[cfg(test)]
mod tests;
