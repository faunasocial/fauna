//! The device shell's [`Host`]: the consent ceremony, the principal sessions
//! and the credential store behind the catalog.
//!
//! **Lifecycle.** `connect(handle)` resolves the handle to its nest, reads the
//! issuer's discovery document, mints a DPoP key and a holder key, starts the
//! typed-code ceremony and answers the `user_code` at once; a background task
//! polls the token endpoint at the interval the nest named until the human
//! approves (or declines, or the code expires), then dials the principal
//! session. Every tool call for that account in between is answered "waiting
//! for approval" with the code. A token near its expiry is rotated with the
//! refresh grant and the session redialled before the call; a session the nest
//! dropped (a restart, a network blip) is redialled when a call finds it gone
//! (`fauna_cli::tools::call`, which retries only a read-only tool).
//!
//! **Several accounts are several principals** (`apps/cli.md` § The device
//! shell): one [`Held`] entry per account, each with its own keys, grant and
//! session; a tool names its account by handle only when more than one is held.
//!
//! **What rests, and where.** Each account's DPoP key, holder key, refresh
//! grant and nest address rest in the credential store under this app's own
//! namespace (`apps/tui.md` § Credential storage), in a slot keyed by the
//! account's id (the token answer's `sub`) — never the handle, which the user
//! may spell several ways — beside a bounded index naming each slot's handle,
//! so a relaunch resumes every account without a new ceremony. The slots are
//! this app's own, never the shared `AccountRegistry`, which holds identity
//! keys this app must never hold (§ Architectural rules 1). The store is
//! `CredentialStore::new_with_unattended_fallback` over this app's one data
//! dir (`$XDG_CONFIG_HOME/fauna-cli`, [`config_dir`]): the OS secret store
//! where one is usable, else the unattended arm — a `0600` file in that dir,
//! auto-selected, no passphrase — which this process declares in every
//! `connect` answer and once on stderr ([`UNATTENDED_CUSTODY`];
//! `apps/cli.md` § The device shell → *Credentials*). The sealed passphrase
//! arm is tui's: this process has no surface to ask for one.

use std::sync::Arc;
use std::time::{Duration, Instant};

use fauna_anon_client::PrincipalNestClient;
use fauna_cli::catalog::consent_scope;
use fauna_cli::device::{
    DeviceClient, DeviceStart, DpopKey, HolderKey, Issuer, Poll, Tokens, development_client_id,
};
use fauna_cli::door::DoorIdentity;
use fauna_cli::tools::{ConnectStarted, ConversationOp, ToolData, UNATTENDED_CUSTODY};
use fauna_cli::{Host, SessionUnavailable, ToolError};
use fauna_client_accounts::SecretStore as _;
use fauna_credential_store::{CredentialStore, MAX_ITEM_VALUE_BYTES};
use fauna_protocol::discovery::{SetupStatusReply, SetupStatusRequest};
use tokio::sync::Mutex;

/// This app's credential namespace. tui used this name before its 2026-07-12
/// rename, so a box that dogfooded tui then may still hold that era's keys
/// here; this app reads only its own `cli/*` keys and never another.
const NAMESPACE: &str = "fauna-cli";
/// The account index: one `<account id>\t<handle>` line per held account.
/// One store item, so bounded by the store's item cap like the shared
/// registry's index (`long-term-store.md` § Multi-account evolution → *The
/// index is bounded*): a connect that would grow it past the cap is refused.
const KEY_INDEX: &str = "cli/principals";
const LEAF_DIAL_BASE: &str = "dial_base";
const LEAF_CLIENT_ID: &str = "client_id";
const LEAF_DPOP: &str = "dpop";
const LEAF_HOLDER: &str = "holder";
const LEAF_REFRESH: &str = "refresh";

/// The store key of one account's slot item.
fn slot_key(account: &str, leaf: &str) -> String {
    format!("cli/principal/{account}/{leaf}")
}

/// Rotate a token this long before the nest's own `exp` closes the session.
const REFRESH_MARGIN: Duration = Duration::from_secs(30);

/// The e2e stand-in for handle resolution (`e2e-conventions.md` convention
/// 15): the loopback nest the harness started, which no DNS names. Compiled
/// out of a release build without `e2e-agent`.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn e2e_dial_base() -> Option<String> {
    std::env::var("FAUNA_E2E_CLI_NEST_URL")
        .ok()
        .filter(|v| !v.is_empty())
}

#[cfg(not(any(debug_assertions, feature = "e2e-agent")))]
fn e2e_dial_base() -> Option<String> {
    None
}

struct Connected {
    device: Arc<DeviceClient>,
    session: Arc<PrincipalNestClient>,
    /// The token the session was opened with — what a redial reopens it with
    /// while it is not near expiry.
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<Instant>,
    /// The principal row the consent minted, hex — the token answer names it
    /// on every grant and rotation; what the shell names itself by at the
    /// conversations door.
    principal_id: Option<String>,
}

enum Slot {
    /// Rests in the store, not dialled yet this launch.
    Stored,
    Pending {
        user_code: String,
    },
    Connected(Connected),
    Failed(String),
}

/// One account the shell holds.
struct Held {
    /// The handle the user connected it with — what tools and answers name.
    handle: String,
    /// The account's hex actor id (the token's `sub`); `None` until a pending
    /// consent is approved.
    account: Option<String>,
    /// The holder key the consent attested — the conversations door's
    /// possession proof opens under it. Read from the slot on first use after
    /// a relaunch.
    holder: Option<Arc<HolderKey>>,
    slot: Slot,
}

/// One index line's two fields.
struct IndexEntry {
    account: String,
    handle: String,
}

fn encode_index(entries: &[IndexEntry]) -> String {
    entries
        .iter()
        .map(|e| format!("{}\t{}\n", e.account, e.handle))
        .collect()
}

fn decode_index(text: &str) -> Vec<IndexEntry> {
    text.lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(account, handle)| IndexEntry {
            account: account.to_owned(),
            handle: handle.to_owned(),
        })
        .collect()
}

/// The handle as the index and the answers carry it: no leading `@`, no
/// surrounding space.
fn normalize_handle(handle: &str) -> &str {
    handle.trim().trim_start_matches('@')
}

fn names(held: &Held, account: &str) -> bool {
    let account = normalize_handle(account);
    held.handle.eq_ignore_ascii_case(account) || held.account.as_deref() == Some(account)
}

/// The entry a call's `account` names: the one held when it names none.
fn resolve(held: &[Held], account: Option<&str>) -> Result<usize, SessionUnavailable> {
    let handles = || held.iter().map(|h| h.handle.clone()).collect::<Vec<_>>();
    match account {
        Some(account) => held.iter().position(|h| names(h, account)).ok_or_else(|| {
            SessionUnavailable::UnknownAccount {
                account: normalize_handle(account).to_owned(),
                held: handles(),
            }
        }),
        None => match held.len() {
            0 => Err(SessionUnavailable::NotConnected),
            1 => Ok(0),
            _ => Err(SessionUnavailable::AccountNeeded { held: handles() }),
        },
    }
}

pub struct DeviceHost {
    http: reqwest::Client,
    store: Option<Arc<CredentialStore>>,
    held: Arc<Mutex<Vec<Held>>>,
}

fn config_dir() -> Option<std::path::PathBuf> {
    fauna_core::platform_ids::xdg_app_config_dir(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
        NAMESPACE,
    )
}

/// What a consent start holds until its approval names the account.
struct Secrets {
    dial_base: String,
    client_id: String,
    dpop: String,
    holder: String,
}

impl DeviceHost {
    pub fn new() -> Self {
        let store = config_dir()
            .map(|dir| CredentialStore::new_with_unattended_fallback(NAMESPACE, dir))
            .map(Arc::new);
        if let Some(cause) = store.as_ref().and_then(|s| s.unattended_arm_cause()) {
            tracing::warn!(?cause, "{UNATTENDED_CUSTODY}");
        }
        let held = store
            .as_ref()
            .and_then(|store| store.get(KEY_INDEX))
            .map(|index| decode_index(&index))
            .unwrap_or_default()
            .into_iter()
            .map(|e| Held {
                handle: e.handle,
                account: Some(e.account),
                holder: None,
                slot: Slot::Stored,
            })
            .collect();
        Self {
            http: reqwest::Client::new(),
            store,
            held: Arc::new(Mutex::new(held)),
        }
    }

    async fn dial_base_for(&self, handle: &str) -> Result<String, ToolError> {
        if let Some(base) = e2e_dial_base() {
            return Ok(base);
        }
        let resolved = fauna_core::resolve::resolve_handle(handle)
            .await
            .ok_or_else(|| {
                ToolError::new(format!(
                    "{handle} does not resolve to a Fauna nest — check the handle with the user"
                ))
            })?;
        Ok(format!("https://{}", resolved.domain))
    }

    /// Dial the stored slot of `account`.
    async fn resume(&self, account: &str) -> Result<Connected, String> {
        let incomplete = || "its stored keys are incomplete".to_owned();
        let store = self.store.as_ref().ok_or_else(incomplete)?;
        let get = |leaf| store.get(&slot_key(account, leaf)).ok_or_else(incomplete);
        let dial_base = get(LEAF_DIAL_BASE)?;
        let client_id = get(LEAF_CLIENT_ID)?;
        let dpop = DpopKey::from_secret_b64(&get(LEAF_DPOP)?).ok_or_else(incomplete)?;
        let refresh = get(LEAF_REFRESH)?;
        let issuer = Issuer::discover(&self.http, &dial_base).await?;
        let device = Arc::new(DeviceClient::new(
            self.http.clone(),
            issuer,
            client_id,
            dpop,
        ));
        let tokens = device.refresh(&refresh).await?;
        dial(device, tokens, self.store.clone()).await
    }

    /// `held`'s live session, resuming or rotating it first when it needs it.
    async fn ready(&self, held: &mut Held) -> Result<Arc<PrincipalNestClient>, SessionUnavailable> {
        if matches!(held.slot, Slot::Stored) {
            let account = held.account.clone().unwrap_or_default();
            held.slot = match self.resume(&account).await {
                Ok(connected) => Slot::Connected(connected),
                Err(why) => {
                    tracing::warn!("could not resume the stored connection: {why}");
                    Slot::Failed(format!(
                        "the stored connection to {} could not be resumed ({why}) — call connect again",
                        held.handle
                    ))
                }
            };
        }
        match &mut held.slot {
            Slot::Stored => unreachable!("resumed above"),
            Slot::Pending { user_code } => Err(SessionUnavailable::Pending {
                user_code: user_code.clone(),
            }),
            Slot::Failed(why) => Err(SessionUnavailable::Failed(why.clone())),
            Slot::Connected(connected) => {
                let stale = connected
                    .expires_at
                    .is_some_and(|at| Instant::now() + REFRESH_MARGIN >= at);
                let refresh = connected.refresh_token.clone().filter(|_| stale);
                let Some(refresh) = refresh else {
                    return Ok(connected.session.clone());
                };
                let device = connected.device.clone();
                let renewed = match device.refresh(&refresh).await {
                    Ok(tokens) => dial(device, tokens, self.store.clone()).await,
                    Err(why) => Err(why),
                };
                match renewed {
                    Ok(fresh) => {
                        let session = fresh.session.clone();
                        held.slot = Slot::Connected(fresh);
                        Ok(session)
                    }
                    Err(why) => {
                        let why = format!("the connection ended: {why} — call connect again");
                        held.slot = Slot::Failed(why.clone());
                        Err(SessionUnavailable::Failed(why))
                    }
                }
            }
        }
    }

    /// Room in the index for one more account, `handle` — refused past the
    /// store's item cap.
    fn index_has_room(held: &[Held], handle: &str) -> bool {
        if held.iter().any(|h| h.handle.eq_ignore_ascii_case(handle)) {
            return true;
        }
        let mut entries: Vec<IndexEntry> = held
            .iter()
            .map(|h| IndexEntry {
                account: h.account.clone().unwrap_or_else(|| "0".repeat(64)),
                handle: h.handle.clone(),
            })
            .collect();
        entries.push(IndexEntry {
            account: "0".repeat(64),
            handle: handle.to_owned(),
        });
        encode_index(&entries).len() <= MAX_ITEM_VALUE_BYTES
    }
}

fn persist(store: &Option<Arc<CredentialStore>>, key: &str, value: &str) {
    if let Some(store) = store {
        store.set(key, value);
    }
}

/// Rest an approved account: its slot first, then the index line that names
/// it — the index is the decision point, so a crash between the two leaves an
/// unnamed slot, never a named one with missing keys.
fn persist_account(
    store: &Option<Arc<CredentialStore>>,
    held: &[Held],
    account: &str,
    secrets: &Secrets,
    refresh: Option<&str>,
) {
    let Some(store) = store else { return };
    store.set(&slot_key(account, LEAF_DIAL_BASE), &secrets.dial_base);
    store.set(&slot_key(account, LEAF_CLIENT_ID), &secrets.client_id);
    store.set(&slot_key(account, LEAF_DPOP), &secrets.dpop);
    store.set(&slot_key(account, LEAF_HOLDER), &secrets.holder);
    if let Some(refresh) = refresh {
        store.set(&slot_key(account, LEAF_REFRESH), refresh);
    }
    let entries: Vec<IndexEntry> = held
        .iter()
        .filter_map(|h| {
            h.account.clone().map(|account| IndexEntry {
                account,
                handle: h.handle.clone(),
            })
        })
        .collect();
    store.set(KEY_INDEX, &encode_index(&entries));
}

/// Open the principal session with `access`.
async fn open(device: &DeviceClient, access: &str) -> Result<PrincipalNestClient, String> {
    let htu = device
        .issuer()
        .published(fauna_anon_client::PRINCIPAL_WS_PATH);
    let (session, nonce) = PrincipalNestClient::connect(
        &device.issuer().dial_base,
        access,
        device.nonce().as_deref(),
        |nonce| device.dpop().proof("GET", &htu, nonce, Some(access)),
    )
    .await
    .map_err(|e| format!("could not open the connection to the nest: {e}"))?;
    if nonce.is_some() {
        device.set_nonce(nonce);
    }
    Ok(session)
}

/// Dial the principal session with `tokens`, and keep the rotated refresh
/// grant in the account's slot.
async fn dial(
    device: Arc<DeviceClient>,
    tokens: Tokens,
    store: Option<Arc<CredentialStore>>,
) -> Result<Connected, String> {
    let session = open(&device, &tokens.access_token).await?;
    if let (Some(refresh), Some(account)) = (&tokens.refresh_token, &tokens.sub) {
        persist(&store, &slot_key(account, LEAF_REFRESH), refresh);
    }
    Ok(Connected {
        device,
        session: Arc::new(session),
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_at: tokens
            .expires_in
            .map(|s| Instant::now() + Duration::from_secs(s)),
        principal_id: tokens.principal_id,
    })
}

/// Poll the typed-code start until it ends, then dial; the account id the
/// token answer named rides along.
async fn await_approval(
    device: Arc<DeviceClient>,
    start: DeviceStart,
    store: Option<Arc<CredentialStore>>,
) -> Result<(Connected, Option<String>), String> {
    let deadline = Instant::now() + Duration::from_secs(start.expires_in);
    let mut interval = Duration::from_secs(start.interval.max(1));
    loop {
        // The interval is the protocol's own pacing (RFC 8628 §3.5), the nest's
        // number — polling faster earns `slow_down`.
        tokio::time::sleep(interval).await;
        if Instant::now() >= deadline {
            return Err("the code expired before it was approved — call connect again".into());
        }
        match device.poll(&start.device_code).await? {
            Poll::Pending => {}
            Poll::SlowDown => interval += Duration::from_secs(5),
            Poll::Tokens(tokens) => {
                let account = tokens.sub.clone();
                return Ok((dial(device, tokens, store).await?, account));
            }
            Poll::Refused(why) => return Err(format!("the connection was not approved: {why}")),
        }
    }
}

impl Host for DeviceHost {
    type Rpc = Arc<PrincipalNestClient>;

    async fn session(&self, account: Option<&str>) -> Result<Self::Rpc, SessionUnavailable> {
        let mut held = self.held.lock().await;
        let i = resolve(&held, account)?;
        self.ready(&mut held[i]).await
    }

    async fn account(&self, account: Option<&str>) -> Result<String, SessionUnavailable> {
        let mut held = self.held.lock().await;
        let i = resolve(&held, account)?;
        self.ready(&mut held[i]).await?;
        held[i].account.clone().ok_or_else(|| {
            SessionUnavailable::Failed(
                "the nest did not say which account this connection acts for".into(),
            )
        })
    }

    async fn redial(
        &self,
        account: Option<&str>,
        lost: &Self::Rpc,
    ) -> Result<Self::Rpc, SessionUnavailable> {
        let mut held = self.held.lock().await;
        let i = resolve(&held, account)?;
        let Slot::Connected(connected) = &mut held[i].slot else {
            return self.ready(&mut held[i]).await;
        };
        if !Arc::ptr_eq(&connected.session, lost) {
            return Ok(connected.session.clone());
        }
        let device = connected.device.clone();
        let stale = connected
            .expires_at
            .is_none_or(|at| Instant::now() + REFRESH_MARGIN >= at);
        // The held token first while it is good: a nest restart keeps the
        // grant, and reopening spends no refresh rotation.
        let reopened = if stale {
            Err("the token is near its expiry".to_owned())
        } else {
            open(&device, &connected.access_token).await
        };
        let fresh = match reopened {
            Ok(session) => Ok(Arc::new(session)),
            Err(first) => match connected.refresh_token.clone() {
                None => Err(first),
                Some(refresh) => match device.refresh(&refresh).await {
                    Ok(tokens) => dial(device, tokens, self.store.clone()).await.map(|fresh| {
                        let session = fresh.session.clone();
                        *connected = fresh;
                        session
                    }),
                    Err(why) => Err(why),
                },
            },
        };
        match fresh {
            Ok(session) => {
                connected.session = session.clone();
                Ok(session)
            }
            // The slot stays connected on the lost session: the next call
            // that finds it gone tries again, so a nest still restarting is
            // a wait, never a fresh ceremony.
            Err(why) => Err(SessionUnavailable::Failed(format!(
                "the connection to the nest dropped and could not be reopened yet ({why}) — try again shortly"
            ))),
        }
    }

    async fn setup_status(&self, account: Option<&str>) -> Result<SetupStatusReply, ToolError> {
        let dial_base = {
            let mut held = self.held.lock().await;
            let i = resolve(&held, account)?;
            self.ready(&mut held[i]).await?;
            match &held[i].slot {
                Slot::Connected(connected) => connected.device.issuer().dial_base.clone(),
                _ => unreachable!("`ready` answers Ok only for a connected slot"),
            }
        };
        let anonymous = fauna_anon_client::AnonymousNestClient::connect(&dial_base)
            .await
            .map_err(|e| {
                ToolError::new(format!("could not open the connection to the nest: {e}"))
            })?;
        anonymous
            .request("fauna.setup.status", SetupStatusRequest::default())
            .await
            .map_err(|e| ToolError::new(format!("the nest did not answer its setup status: {e}")))
    }

    async fn conversation(
        &self,
        account: Option<&str>,
        op: ConversationOp,
    ) -> Result<ToolData, ToolError> {
        let identity = {
            let mut held = self.held.lock().await;
            let i = resolve(&held, account)?;
            self.ready(&mut held[i]).await?;
            let entry = &mut held[i];
            let principal = match &entry.slot {
                Slot::Connected(connected) => connected.principal_id.clone(),
                _ => unreachable!("`ready` answers Ok only for a connected slot"),
            }
            .ok_or_else(|| {
                ToolError::new(
                    "the nest did not name this connection's principal, so the Fauna app on this device cannot recognise it — call connect again",
                )
            })?;
            let actor = entry.account.clone().ok_or_else(|| {
                ToolError::new("the nest did not say which account this connection acts for")
            })?;
            if entry.holder.is_none() {
                entry.holder = self
                    .store
                    .as_ref()
                    .and_then(|store| store.get(&slot_key(&actor, LEAF_HOLDER)))
                    .and_then(|raw| HolderKey::from_secret_b64(&raw))
                    .map(Arc::new);
            }
            let holder = entry.holder.clone().ok_or_else(|| {
                ToolError::new("this connection's holder key is not stored — call connect again")
            })?;
            DoorIdentity::from_hex(&actor, &principal, holder)?
        };
        fauna_cli::door::perform(identity, op).await
    }

    async fn connect(&self, handle: &str, admin: bool) -> Result<ConnectStarted, ToolError> {
        let handle = normalize_handle(handle).to_owned();
        if handle.is_empty() || handle.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(ToolError::new(format!(
                "{handle:?} is not a Fauna handle — check it with the user"
            )));
        }
        if !Self::index_has_room(&self.held.lock().await, &handle) {
            return Err(ToolError::new(
                "this assistant already holds as many Fauna accounts as it can keep",
            ));
        }
        let dial_base = self.dial_base_for(&handle).await?;
        let issuer = Issuer::discover(&self.http, &dial_base)
            .await
            .map_err(ToolError::new)?;
        let nest = issuer.issuer.clone();
        let scope = consent_scope(admin);
        let client_id = development_client_id(&scope);
        let dpop = DpopKey::generate();
        let holder = HolderKey::generate();
        let secrets = Secrets {
            dial_base,
            client_id: client_id.clone(),
            dpop: dpop.to_secret_b64(),
            holder: holder.to_secret_b64(),
        };
        let device = Arc::new(DeviceClient::new(
            self.http.clone(),
            issuer,
            client_id,
            dpop,
        ));
        let start = device
            .start(&scope, &holder)
            .await
            .map_err(ToolError::new)?;
        let holder = Arc::new(holder);

        let started = ConnectStarted {
            account: handle.clone(),
            user_code: start.user_code.clone(),
            nest,
            expires_in: start.expires_in,
            custody: self
                .store
                .as_ref()
                .and_then(|s| s.unattended_arm_cause())
                .map(|_| UNATTENDED_CUSTODY),
        };
        {
            let mut held = self.held.lock().await;
            let pending = Slot::Pending {
                user_code: start.user_code.clone(),
            };
            match held
                .iter_mut()
                .find(|h| h.handle.eq_ignore_ascii_case(&handle))
            {
                Some(entry) => {
                    entry.slot = pending;
                    entry.holder = Some(holder);
                }
                None => held.push(Held {
                    handle: handle.clone(),
                    account: None,
                    holder: Some(holder),
                    slot: pending,
                }),
            }
        }
        let held = self.held.clone();
        let store = self.store.clone();
        let own_code = start.user_code.clone();
        tokio::spawn(async move {
            let outcome = await_approval(device, start, store.clone()).await;
            let mut held = held.lock().await;
            // Only this start's own pending entry is this poll's to settle: a
            // later `connect` for the same handle replaced it, and an earlier
            // code expiring must not turn the newer connection into a failure.
            let Some(i) = held.iter().position(
                |h| matches!(&h.slot, Slot::Pending { user_code } if *user_code == own_code),
            ) else {
                return;
            };
            match outcome {
                Err(why) => held[i].slot = Slot::Failed(why),
                Ok((connected, account)) => {
                    let refresh = connected.refresh_token.clone();
                    held[i].slot = Slot::Connected(connected);
                    held[i].account = account.clone();
                    let Some(account) = account else {
                        tracing::warn!(
                            "the nest named no account for this connection: it will not survive a relaunch"
                        );
                        return;
                    };
                    // The same account connected before under another
                    // spelling of its handle is this entry now.
                    let keep = held[i].handle.clone();
                    held.retain(|h| {
                        h.account.as_deref() != Some(account.as_str()) || h.handle == keep
                    });
                    persist_account(&store, &held, &account, &secrets, refresh.as_deref());
                }
            }
        });
        Ok(started)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(handle: &str, account: Option<&str>) -> Held {
        Held {
            handle: handle.into(),
            account: account.map(str::to_owned),
            holder: None,
            slot: Slot::Stored,
        }
    }

    #[test]
    fn a_call_names_its_account_only_when_several_are_held() {
        assert_eq!(resolve(&[], None), Err(SessionUnavailable::NotConnected));
        let one = [held("alice.example", Some("a1"))];
        assert_eq!(resolve(&one, None), Ok(0));
        let two = [held("alice.example", Some("a1")), held("bob.example", None)];
        assert_eq!(
            resolve(&two, None),
            Err(SessionUnavailable::AccountNeeded {
                held: vec!["alice.example".into(), "bob.example".into()]
            })
        );
        assert_eq!(resolve(&two, Some("@Bob.Example")), Ok(1));
        assert_eq!(resolve(&two, Some("a1")), Ok(0));
        assert!(matches!(
            resolve(&two, Some("carol.example")),
            Err(SessionUnavailable::UnknownAccount { .. })
        ));
    }

    #[test]
    fn the_index_round_trips_and_is_bounded_by_the_item_cap() {
        let entries = [
            IndexEntry {
                account: "a1".repeat(32),
                handle: "alice.example".into(),
            },
            IndexEntry {
                account: "b2".repeat(32),
                handle: "bob.example".into(),
            },
        ];
        let decoded = decode_index(&encode_index(&entries));
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[1].account, "b2".repeat(32));
        assert_eq!(decoded[1].handle, "bob.example");

        let mut many = Vec::new();
        while DeviceHost::index_has_room(&many, &format!("user{}.example", many.len())) {
            let n = many.len();
            many.push(held(&format!("user{n}.example"), Some(&"c3".repeat(32))));
        }
        assert!(
            encode_index(
                &many
                    .iter()
                    .map(|h| IndexEntry {
                        account: h.account.clone().unwrap(),
                        handle: h.handle.clone(),
                    })
                    .collect::<Vec<_>>()
            )
            .len()
                <= MAX_ITEM_VALUE_BYTES
        );
        // A handle already held replaces its entry, so never needs room.
        assert!(DeviceHost::index_has_room(&many, "USER0.example"));
    }
}
