//! VAPID key management and web-push notification dispatch.
//!
//! [`PushService`] wraps a VAPID private key (EC P-256, PEM format) and sends
//! Web Push notifications to browser subscriptions stored by [`crate::db::push`].
//!
//! This module implements RFC 8030 (Generic Event Delivery Using HTTP Push),
//! RFC 8291 (Message Encryption for Web Push, `aes128gcm` content encoding),
//! and RFC 8292 (Voluntary Application Server Identification for Web Push,
//! VAPID) using only pure-Rust crates already in the workspace.
//!
//! The `web-push` crate on crates.io (v0.11) transitively requires `openssl`
//! (via `ece` → `openssl`), which is not available on this macOS build machine.
//! We therefore implement the required crypto primitives directly.
//!
//! # The dial policy
//!
//! A `web-push` endpoint is a URL the subscriber chose, so dialling it is a
//! caller-supplied-URL fetch and goes through [`crate::ssrf`] like every other
//! one: https only, every resolved address globally routable, DNS pinned, no
//! redirects. It is enforced twice — [`validate_subscription_endpoint`] refuses
//! at `fauna.push.subscribe` whatever the URL text alone decides, and
//! [`web_push_client`] runs the full resolving guard at every dial, because a
//! name's answer can change between the two. Every request is bounded by
//! [`PUSH_REQUEST_TIMEOUT`] and a whole dispatch by [`PUSH_DISPATCH_DEADLINE`],
//! and a sender-triggered dispatch never runs on the sender's reply path
//! ([`dispatch_offline_push`]). Rule: `docs/goal/architecture/apps/common.md`
//! § Push Notifications → § Registration, § Dispatch Logic.
//!
//! # The per-device decision
//!
//! Whether a subscription row is pushed is decided **per device**, never per
//! actor ([`DevicePresence::action_for`]): a `web-push`/`apns` row is skipped
//! while a live connection announcing its `device_id` exists
//! (`fauna.push.presence`) and dialled otherwise; a `ws-device` row is
//! delivered as a `fauna.push.notification` frame to those connections and
//! skipped, with nothing queued, when there are none. A live connection that
//! announced nothing (no app sends `fauna.push.presence` yet) suppresses every `web-push`/`apns`
//! row, the pre-ruling behaviour (the compatibility rider). Rule:
//! `docs/goal/architecture/apps/common.md` § Dispatch Logic.

use std::sync::Arc;
use std::time::Duration;

use aes_gcm::{AeadInPlace, Aes128Gcm, KeyInit, Nonce as GcmNonce};
use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hkdf::Hkdf;
use p256::{
    PublicKey, SecretKey,
    ecdh::EphemeralSecret,
    ecdsa::{DerSignature, SigningKey, signature::Signer as _},
    pkcs8::DecodePrivateKey,
};
use rand::RngCore;
use sha2::Sha256;
use tracing::{debug, warn};

use fauna_protocol::PushEvent;
use fauna_protocol::push_events::PushNotificationPayload;

use crate::db::CacheDb;
use crate::routes::AppState;
use crate::ssrf::SsrfError;

/// Ceiling on one push request (a `web-push` POST or an APNs POST), connect to
/// last byte. A push service that accepts the connection and never answers
/// costs the dispatch this long and no longer. A Rust constant, not a knob:
/// nobody has a reason to choose it (`principles.md` § One configuration
/// surface).
const PUSH_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Ceiling on one whole dispatch — every subscription of one actor, dialled in
/// turn. Subscriptions per actor are uncapped, so without this an actor holding
/// many slow endpoints would multiply [`PUSH_REQUEST_TIMEOUT`] into a task that
/// lives for minutes per delivery. Subscriptions the deadline cuts off are
/// simply not pushed this time; push is best-effort.
const PUSH_DISPATCH_DEADLINE: Duration = Duration::from_secs(30);

/// The strings a `web-push` payload and a `ws-device` frame carry — generic on
/// purpose: a browser vendor's relay must not read the content, and the sync
/// agent that posts a `ws-device` frame links no MLS, so it can render no more
/// (`apps/common.md` § Push Notifications → *Transports*, § Content
/// Encryption). Only `apns` carries the rich strings, encrypted.
const GENERIC_TITLE: &str = "Fauna";
const GENERIC_BODY: &str = "New message";

/// Which of one actor's devices are present right now — the input to the
/// per-device dispatch decision (module docs, § The per-device decision).
/// Built by [`crate::ws::WsState::device_presence`] as a snapshot taken at the
/// event, and handed whole to the dispatch that runs later.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DevicePresence {
    /// The `device_id`s the actor's live connections announced.
    pub announced: std::collections::BTreeSet<String>,
    /// Some live connection of the actor announced no device (no app sends the presence announce yet).
    pub any_unannounced: bool,
}

/// What the dispatch does with one subscription row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowAction {
    /// Dial the row's relay (`web-push`, `apns`): its device is absent.
    Dial,
    /// Send the `fauna.push.notification` frame to the device's own live
    /// connections (`ws-device`): presence *is* the delivery.
    Deliver,
    /// Nothing: the device renders the event itself, or is not reachable.
    Skip,
}

impl DevicePresence {
    /// The per-device rule for one row (`apps/common.md` § Dispatch Logic).
    pub fn action_for(&self, transport: &str, device_id: &str) -> RowAction {
        let present = self.announced.contains(device_id);
        match transport {
            "ws-device" if present => RowAction::Deliver,
            // The compatibility rider: an unannounced connection keeps the actor-wide
            // skip for the relay transports, so it never gets double banners.
            "web-push" | "apns" if !present && !self.any_unannounced => RowAction::Dial,
            _ => RowAction::Skip,
        }
    }
}

/// What one dispatch did, counted as it went. Returned by
/// [`PushService::maybe_send_push`]; a `test-hooks` build also sums it into the
/// counters served at `GET /api/v1/test/push/dispatches`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DispatchOutcome {
    /// Rows the dispatch set out to dial (a [`RowAction::Dial`] row carrying
    /// its keys) — whether the dial guard then admitted it or not.
    pub dialled: u64,
    /// `ws-device` rows whose frame was offered to at least one live
    /// connection.
    pub delivered: u64,
}

/// Longest `apns` device token accepted, in hex characters. Today's tokens are
/// 32 bytes (64 characters); Apple documents the length as variable, so the cap
/// is generous rather than exact.
const APNS_DEVICE_TOKEN_MAX_HEX: usize = 512;

/// APNs configuration for sending push notifications to iOS devices.
pub struct ApnsConfig {
    /// P-256 signing key from the Apple .p8 file.
    signing_key: SecretKey,
    /// Key ID from App Store Connect.
    key_id: String,
    /// Apple Developer team ID.
    team_id: String,
    /// App bundle ID (e.g., `social.fauna.fauna`).
    topic: String,
    /// APNs base URL (production or sandbox).
    base_url: String,
    /// HTTP/2 client for APNs requests.
    client: reqwest::Client,
}

impl ApnsConfig {
    /// Load APNs configuration from environment variables.
    ///
    /// Returns `Ok(None)` if `APNS_KEY_PATH` is not set (APNs disabled).
    /// Returns `Err` if `APNS_KEY_PATH` is set but other required vars are
    /// missing or the key file cannot be read/parsed.
    pub fn from_env() -> Result<Option<Self>> {
        let key_path = match std::env::var("APNS_KEY_PATH") {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };

        let key_id = std::env::var("APNS_KEY_ID")
            .context("APNS_KEY_ID must be set when APNS_KEY_PATH is set")?;
        let team_id = std::env::var("APNS_TEAM_ID")
            .context("APNS_TEAM_ID must be set when APNS_KEY_PATH is set")?;
        let topic = std::env::var("APNS_TOPIC")
            .context("APNS_TOPIC must be set when APNS_KEY_PATH is set")?;
        let production = std::env::var("APNS_PRODUCTION")
            .map(|v| v == "true")
            .unwrap_or(false);

        let pem = std::fs::read_to_string(&key_path)
            .with_context(|| format!("read APNs .p8 key file at {key_path}"))?;
        let signing_key = parse_p8_key(&pem)?;

        let base_url = if production {
            "https://api.push.apple.com".to_string()
        } else {
            "https://api.sandbox.push.apple.com".to_string()
        };

        let client = reqwest::Client::builder()
            .http2_prior_knowledge()
            .timeout(PUSH_REQUEST_TIMEOUT)
            .build()
            .context("build HTTP/2 client for APNs")?;

        Ok(Some(Self {
            signing_key,
            key_id,
            team_id,
            topic,
            base_url,
            client,
        }))
    }
}

/// Load this nest's persisted VAPID keypair, generating and persisting a fresh
/// P-256 keypair on first call if none exists yet.
///
/// The VAPID key is pure nest infrastructure (`apps/common.md` § Push
/// Notifications — "target: self-generated per nest"): no user or admin ever
/// has a reason to choose one, so it is minted once and kept, exactly like the
/// deployment signing key (`crate::deployment_key`) — just without that key's
/// factory-reset-survival requirement, since a VAPID identity rotating on
/// reset only costs subscribers a re-subscribe, not a broken TOFU pin.
pub async fn ensure_vapid_pem(db: &CacheDb) -> Result<Vec<u8>> {
    use p256::pkcs8::{EncodePrivateKey, LineEnding};

    if let Some(pem) = db.get_vapid_pem().await? {
        return Ok(pem);
    }

    let secret = SecretKey::random(&mut rand::thread_rng());
    let pem = secret
        .to_pkcs8_pem(LineEnding::LF)
        .context("encode generated VAPID key as PKCS8 PEM")?
        .as_bytes()
        .to_vec();
    db.set_vapid_pem(&pem).await?;
    tracing::info!("VAPID keypair: generated and persisted a new one (first boot)");
    Ok(pem)
}

/// A service that holds the VAPID key pair and sends push notifications.
pub struct PushService {
    db: Arc<CacheDb>,
    /// P-256 private key used for VAPID JWT signing and re-used across calls.
    vapid_key: SecretKey,
    /// Uncompressed P-256 public key bytes, base64url-encoded without padding.
    /// This is the value that the client needs to subscribe (`applicationServerKey`).
    vapid_public_key_base64: String,
    /// Optional APNs configuration for iOS push notifications.
    apns: Option<ApnsConfig>,
    /// What [`dispatch_offline_push`] has decided and done so far — the causal
    /// anchors for a test's per-device asserts; see [`Self::dispatch_counters`].
    #[cfg(feature = "test-hooks")]
    counters: DispatchCounterCells,
}

/// The `test-hooks` counters behind [`PushService::dispatch_counters`].
#[cfg(feature = "test-hooks")]
#[derive(Default)]
struct DispatchCounterCells {
    initiated: std::sync::atomic::AtomicU64,
    settled: std::sync::atomic::AtomicU64,
    dialled: std::sync::atomic::AtomicU64,
    delivered: std::sync::atomic::AtomicU64,
}

/// A snapshot of [`PushService::dispatch_counters`].
#[cfg(feature = "test-hooks")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DispatchCounters {
    /// Dispatches decided — bumped inside the sender's handler, at the
    /// presence snapshot, strictly before its reply.
    pub initiated: u64,
    /// Dispatches whose background task has finished. Bumped **after** the
    /// task's [`DispatchOutcome`] is summed in, so `settled == initiated`
    /// makes `dialled` / `delivered` final for every dispatch decided so far.
    pub settled: u64,
    /// Sum of [`DispatchOutcome::dialled`] over settled dispatches.
    pub dialled: u64,
    /// Sum of [`DispatchOutcome::delivered`] over settled dispatches.
    pub delivered: u64,
}

impl PushService {
    /// Create a new `PushService` from a VAPID EC private key in PEM format.
    ///
    /// The PEM may use PKCS8 (`BEGIN PRIVATE KEY` header) or SEC1
    /// (`BEGIN EC PRIVATE KEY` header) encoding.  Generate one with:
    ///
    /// ```bash
    /// openssl ecparam -name prime256v1 -genkey -noout | \
    ///   openssl pkcs8 -topk8 -nocrypt -out vapid_private.pem
    /// ```
    pub fn new(db: Arc<CacheDb>, vapid_pem: Vec<u8>, apns: Option<ApnsConfig>) -> Result<Self> {
        let pem_str = std::str::from_utf8(&vapid_pem).context("VAPID PEM is not valid UTF-8")?;

        // Try PKCS8 first, then SEC1.
        let vapid_key = SecretKey::from_pkcs8_pem(pem_str)
            .or_else(|_| SecretKey::from_sec1_pem(pem_str))
            .context("parse VAPID private key PEM (expected PKCS8 or SEC1 EC P-256)")?;

        // Derive the uncompressed public key (65 bytes: 0x04 || X || Y).
        let pub_key = vapid_key.public_key();
        let pub_key_bytes: Vec<u8> = p256::EncodedPoint::from(&pub_key).as_bytes().to_vec();
        let vapid_public_key_base64 = URL_SAFE_NO_PAD.encode(&pub_key_bytes);

        Ok(Self {
            db,
            vapid_key,
            vapid_public_key_base64,
            apns,
            #[cfg(feature = "test-hooks")]
            counters: DispatchCounterCells::default(),
        })
    }

    /// What [`dispatch_offline_push`] has decided and done, so far.
    ///
    /// The dispatch itself is spawned, so "nothing was POSTed" read right after
    /// a sender's call returns is the settle-window false-pass e2e convention 14
    /// forbids: a push that is merely late reads identical to one that never
    /// existed. Two facts turn it into read–call–read. The *decision* is
    /// synchronous — the presence snapshot is taken inside the sender's
    /// handler, strictly before its reply, and `initiated` is bumped there, so
    /// the RPC reply is the barrier for "a dispatch exists" (the
    /// [`crate::post_fanout_test_hook`] shape). Its *outcome* needs the
    /// subscription read, which is background work — so the task sums what it
    /// did into `dialled` / `delivered` and only then bumps `settled`: waiting
    /// for `settled` to reach `initiated` is waiting for a positive event, after
    /// which an unchanged `dialled` is "never dialled", not "not yet". The
    /// counters are content-free. Served at `GET /api/v1/test/push/dispatches`
    /// ([`crate::push_test_hooks`]).
    #[cfg(feature = "test-hooks")]
    pub fn dispatch_counters(&self) -> DispatchCounters {
        use std::sync::atomic::Ordering::Acquire;
        // `settled` first: its Release store follows the sums' stores, so the
        // sums read after it are at least as new as the count it reports.
        let settled = self.counters.settled.load(Acquire);
        DispatchCounters {
            initiated: self.counters.initiated.load(Acquire),
            settled,
            dialled: self.counters.dialled.load(Acquire),
            delivered: self.counters.delivered.load(Acquire),
        }
    }

    /// Returns the base64url-encoded uncompressed P-256 public key that clients
    /// must pass as `applicationServerKey` when subscribing.
    pub fn vapid_public_key(&self) -> &str {
        &self.vapid_public_key_base64
    }

    /// Push an event to each of an actor's subscribed devices that is absent,
    /// per the per-device rule (module docs, § The per-device decision).
    ///
    /// * `ws` – the live connection registry: where a `ws-device` row's
    ///   `fauna.push.notification` frame is delivered.
    /// * `presence` – the actor's devices present **at the event**
    ///   ([`crate::ws::WsState::device_presence`]), taken by the caller.
    /// * `actor_id` – raw actor identifier bytes.
    /// * `rich_title` / `rich_body` – human-readable notification text.
    ///   APNs receives it, encrypted; web push and `ws-device` carry the
    ///   generic strings ([`GENERIC_TITLE`]).
    /// * `url` – deep link path for the notification tap action.
    ///
    /// Subscriptions that return 404 or 410 ("endpoint gone") are removed from
    /// the database automatically, and so is a `web-push` row whose endpoint the
    /// dial guard refuses from its text alone (it can never be dialled).
    ///
    /// Bounded: each request by [`PUSH_REQUEST_TIMEOUT`], the whole call by
    /// [`PUSH_DISPATCH_DEADLINE`]. A caller on a sender's reply path still does
    /// not await it — that is what [`dispatch_offline_push`] is for; awaiting
    /// is for a caller that is already a background task (`cert_nudge`).
    pub async fn maybe_send_push(
        &self,
        ws: &crate::ws::WsState,
        presence: &DevicePresence,
        actor_id: &[u8],
        rich_title: &str,
        rich_body: &str,
        url: &str,
    ) -> Result<DispatchOutcome> {
        let mut outcome = DispatchOutcome::default();
        let sends = self.send_to_subscriptions(
            ws,
            presence,
            actor_id,
            rich_title,
            rich_body,
            url,
            &mut outcome,
        );
        match tokio::time::timeout(PUSH_DISPATCH_DEADLINE, sends).await {
            Ok(result) => result?,
            Err(_) => {
                warn!("push dispatch hit its deadline; remaining subscriptions skipped");
            }
        }
        Ok(outcome)
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Push to every subscription of `actor_id` the per-device rule selects,
    /// in turn, counting into `outcome` as it goes (so a deadline cut still
    /// reports what was done).
    #[allow(clippy::too_many_arguments)]
    async fn send_to_subscriptions(
        &self,
        ws: &crate::ws::WsState,
        presence: &DevicePresence,
        actor_id: &[u8],
        rich_title: &str,
        rich_body: &str,
        url: &str,
        outcome: &mut DispatchOutcome,
    ) -> Result<()> {
        let subs = self
            .db
            .list_push_subscriptions(actor_id)
            .await
            .context("list push subscriptions")?;

        let subs: Vec<_> = subs
            .into_iter()
            .filter_map(
                |sub| match presence.action_for(&sub.transport, &sub.device_id) {
                    RowAction::Skip => None,
                    action => Some((sub, action)),
                },
            )
            .collect();
        if subs.is_empty() {
            debug!("no subscribed device of the actor is absent, skipping push");
            return Ok(());
        }

        // Badge count for APNs notifications.
        let badge_count = self
            .db
            .count_unread_notifications(actor_id)
            .await
            .unwrap_or(0);

        for (sub, action) in subs {
            if action == RowAction::Deliver {
                // Presence is the delivery: the frame rides the device's own
                // authenticated connection(s), nothing is dialled, nothing is
                // queued if the device dropped since the snapshot.
                let offered = <&[u8; 32]>::try_from(actor_id).map_or(0, |actor| {
                    ws.notify_push_to_device(
                        actor,
                        &sub.device_id,
                        PushEvent::PushNotification(PushNotificationPayload {
                            title: GENERIC_TITLE.to_owned(),
                            body: GENERIC_BODY.to_owned(),
                            url: url.to_owned(),
                            extra: Default::default(),
                        }),
                    )
                });
                if offered > 0 {
                    outcome.delivered += 1;
                }
                continue;
            }
            let result = match sub.transport.as_str() {
                "web-push" => {
                    let (Some(p256dh), Some(auth)) =
                        (sub.key_p256dh.as_deref(), sub.key_auth.as_deref())
                    else {
                        warn!(
                            device_id = %sub.device_id,
                            "web-push subscription missing keys, skipping"
                        );
                        continue;
                    };
                    outcome.dialled += 1;
                    // The guard runs at EVERY dial, not only at subscribe: a
                    // name that resolved public then can resolve internal now.
                    match web_push_client(&sub.endpoint).await {
                        Ok((client, endpoint)) => {
                            self.send_web_push(&client, endpoint, p256dh, auth).await
                        }
                        Err(e) => {
                            warn!(
                                device_id = %sub.device_id,
                                error = %e,
                                "web-push endpoint refused by the dial guard, not dialled"
                            );
                            // A refusal the URL text alone decides is permanent:
                            // the row can never be dialled (a dial policy tighter
                            // than the subscribe-time check), so it goes the way of a
                            // dead endpoint. A resolver's answer can change, so
                            // those rows stay.
                            if !matches!(e, SsrfError::Resolve | SsrfError::NonGlobal)
                                && let Err(e) = self
                                    .db
                                    .delete_push_subscription(actor_id, &sub.device_id)
                                    .await
                            {
                                warn!("failed to delete undialable push subscription: {e:#}");
                            }
                            continue;
                        }
                    }
                }
                "apns" => {
                    let (Some(p256dh), Some(auth)) =
                        (sub.key_p256dh.as_deref(), sub.key_auth.as_deref())
                    else {
                        warn!(
                            device_id = %sub.device_id,
                            "apns subscription missing keys, skipping"
                        );
                        continue;
                    };
                    outcome.dialled += 1;
                    self.send_apns(
                        &sub.endpoint,
                        p256dh,
                        auth,
                        rich_title,
                        rich_body,
                        url,
                        badge_count,
                    )
                    .await
                }
                _ => continue,
            };

            match result {
                Ok(status) if status == 404 || status == 410 => {
                    warn!(
                        device_id = %sub.device_id,
                        transport = %sub.transport,
                        status,
                        "push endpoint gone, removing subscription"
                    );
                    if let Err(e) = self
                        .db
                        .delete_push_subscription(actor_id, &sub.device_id)
                        .await
                    {
                        warn!("failed to delete expired push subscription: {e:#}");
                    }
                }
                Ok(status) if !(200..300).contains(&status) => {
                    warn!(
                        device_id = %sub.device_id,
                        transport = %sub.transport,
                        status,
                        "push send failed"
                    );
                }
                Ok(_) => {
                    debug!(
                        device_id = %sub.device_id,
                        transport = %sub.transport,
                        "push sent successfully"
                    );
                }
                Err(e) => {
                    warn!(
                        device_id = %sub.device_id,
                        transport = %sub.transport,
                        error = %e,
                        "push request failed"
                    );
                }
            }
        }

        Ok(())
    }

    /// Send a web push notification with generic content (hides real content
    /// from browser vendor relays).  Returns the HTTP status code.
    async fn send_web_push(
        &self,
        client: &reqwest::Client,
        endpoint: url::Url,
        p256dh_b64: &str,
        auth_b64: &str,
    ) -> Result<u16> {
        let payload_json =
            serde_json::json!({ "title": GENERIC_TITLE, "body": GENERIC_BODY }).to_string();

        let p256dh = URL_SAFE_NO_PAD
            .decode(p256dh_b64)
            .context("decode p256dh")?;
        let auth_secret = URL_SAFE_NO_PAD.decode(auth_b64).context("decode auth")?;

        let ciphertext = encrypt_payload(&p256dh, &auth_secret, payload_json.as_bytes())?;
        let vapid_header = build_vapid_jwt(&self.vapid_key, &endpoint)?;

        let resp = client
            .post(endpoint)
            .header("TTL", "2419200")
            .header("Content-Encoding", "aes128gcm")
            .header("Content-Type", "application/octet-stream")
            .header("Authorization", vapid_header)
            .body(ciphertext)
            .send()
            .await
            .context("send web push HTTP request")?;

        Ok(resp.status().as_u16())
    }

    /// Send an APNs push notification with encrypted rich content.
    /// Returns the HTTP status code, or an error if APNs is not configured
    /// or the HTTP request failed.
    async fn send_apns(
        &self,
        device_token: &str,
        p256dh_b64: &str,
        auth_b64: &str,
        rich_title: &str,
        rich_body: &str,
        url: &str,
        badge_count: i64,
    ) -> Result<u16> {
        let apns = match self.apns.as_ref() {
            Some(a) => a,
            None => {
                debug!("APNs not configured, skipping apns subscription");
                return Ok(200); // Treat as success so we don't delete the subscription
            }
        };

        // Encrypt the rich content using the subscription's P-256 public key.
        let rich_json = serde_json::json!({
            "title": rich_title,
            "body": rich_body,
            "url": url,
        })
        .to_string();

        let p256dh = URL_SAFE_NO_PAD
            .decode(p256dh_b64)
            .context("decode p256dh for APNs")?;
        let auth_secret = URL_SAFE_NO_PAD
            .decode(auth_b64)
            .context("decode auth for APNs")?;

        let ciphertext = encrypt_payload(&p256dh, &auth_secret, rich_json.as_bytes())?;
        let encrypted_b64 = URL_SAFE_NO_PAD.encode(&ciphertext);

        // Build the APNs JSON payload.
        let apns_payload = serde_json::json!({
            "aps": {
                "alert": {
                    "title": "Fauna",
                    "body": "New message"
                },
                "mutable-content": 1,
                "badge": badge_count
            },
            "encrypted_payload": encrypted_b64
        });

        // Build JWT for APNs auth.
        let jwt = build_apns_jwt(&apns.signing_key, &apns.key_id, &apns.team_id)?;

        let apns_url = format!("{}/3/device/{}", apns.base_url, device_token);

        let resp = apns
            .client
            .post(&apns_url)
            .header("authorization", format!("bearer {jwt}"))
            .header("apns-topic", &apns.topic)
            .header("apns-push-type", "alert")
            .header("apns-priority", "10")
            .json(&apns_payload)
            .send()
            .await
            .context("send APNs HTTP request")?;

        Ok(resp.status().as_u16())
    }
}

// ---------------------------------------------------------------------------
// The dial policy — what may be subscribed, what may be dialled
// ---------------------------------------------------------------------------

/// Why `fauna.push.subscribe` refused an endpoint. One message per transport,
/// deliberately not one per cause: the caller learns what shape is accepted,
/// never which check its URL tripped.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EndpointRefusal {
    #[error("a web-push endpoint must be an https URL on a public address")]
    WebPush,
    #[error("an apns endpoint must be a hex device token")]
    Apns,
    #[error("a ws-device endpoint must be the subscription's own device_id, with no keys")]
    WsDevice,
}

/// The subscribe-time shape of a `ws-device` row (`apps/common.md`
/// § Registration): its `endpoint` is its own `device_id` — the row names the
/// device whose live connections receive the frame, and nothing is dialled —
/// and it carries no keys, because nothing leaves the nest's authenticated
/// channel to be encrypted against.
pub fn validate_ws_device_subscription(
    device_id: &str,
    endpoint: &str,
    has_keys: bool,
) -> Result<(), EndpointRefusal> {
    (endpoint == device_id && !has_keys)
        .then_some(())
        .ok_or(EndpointRefusal::WsDevice)
}

/// The subscribe-time check, with **no test hook in front of it** — exactly what
/// [`validate_subscription_endpoint`] compiles down to in every artifact we
/// ship. Its own function for the reason `activitypub::outbound`'s strict twin
/// is: being cfg-free, the release posture is assertable from the all-features
/// arm of the lib gate, and no ambient state can change its answer.
fn validate_subscription_endpoint_strict(
    transport: &str,
    endpoint: &str,
) -> Result<(), EndpointRefusal> {
    match transport {
        // The token is interpolated into the APNs request path, which carries
        // this deployment's APNs credential: hex only, so it can never name
        // another path on Apple's host.
        "apns" => {
            let hex = !endpoint.is_empty()
                && endpoint.len() <= APNS_DEVICE_TOKEN_MAX_HEX
                && endpoint.bytes().all(|b| b.is_ascii_hexdigit());
            hex.then_some(()).ok_or(EndpointRefusal::Apns)
        }
        // Text-only, no DNS: the answer depends on nothing but the request (no
        // resolver latency inside the RPC deadline, no resolver oracle, no
        // refusal of a good relay during a DNS outage). A name that *resolves*
        // somewhere internal is the dial's to refuse — `web_push_client`.
        _ => crate::ssrf::parse_https_url(endpoint)
            .map(|_| ())
            .map_err(|_| EndpointRefusal::WebPush),
    }
}

/// Decide whether `fauna.push.subscribe` may store `endpoint` for `transport`
/// (`"web-push"` or `"apns"` — the handler has already refused anything else,
/// and checks a `ws-device` row with [`validate_ws_device_subscription`]).
pub fn validate_subscription_endpoint(
    transport: &str,
    endpoint: &str,
) -> Result<(), EndpointRefusal> {
    #[cfg(feature = "test-hooks")]
    if transport == "web-push" && test_loopback_endpoint(endpoint) {
        return Ok(());
    }
    validate_subscription_endpoint_strict(transport, endpoint)
}

/// The guarded dial with no test hook in front of it — see
/// [`validate_subscription_endpoint_strict`] for why it is its own function.
async fn web_push_client_strict(endpoint: &str) -> Result<(reqwest::Client, url::Url), SsrfError> {
    // No `User-Agent`: the VAPID header already identifies this nest to the
    // relay, and the relay is the only party dialled.
    crate::ssrf::ssrf_safe_https_client(endpoint, PUSH_REQUEST_TIMEOUT, None).await
}

/// Build the SSRF-guarded, time-bounded client for one `web-push` dial, plus the
/// parsed endpoint. Redirects are off and DNS is pinned to the verified
/// addresses, so a permitted relay can neither `302` nor re-resolve inward.
async fn web_push_client(endpoint: &str) -> Result<(reqwest::Client, url::Url), SsrfError> {
    #[cfg(feature = "test-hooks")]
    if test_loopback_endpoint(endpoint) {
        let url = url::Url::parse(endpoint).map_err(|_| SsrfError::InvalidUrl)?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(PUSH_REQUEST_TIMEOUT)
            .build()
            .map_err(|_| SsrfError::Resolve)?;
        return Ok((client, url));
    }
    web_push_client_strict(endpoint).await
}

/// Under `test-hooks` **only**: an endpoint on a **loopback IP literal**, over
/// `http` or `https`, is subscribable and dialable, so the dispatch witness
/// (`tests/e2e-unified/tests/api/test_push_dispatch.py`) can stand a listener
/// of its own in for the browser vendor's relay.
///
/// Deliberately narrow, as `activitypub::outbound::test_loopback_allowed` is:
/// an IP literal only (no name, so no resolver is trusted), loopback only — a
/// private-range, link-local or cloud-metadata address and a non-loopback
/// `http://` URL are refused in a test build exactly as in a shipped one, which
/// is what lets the same e2e nest witness those refusals. Compile-time alone,
/// no env var: production is built without `test-hooks`, so none of this is in
/// a shipped nest (e2e convention 15).
#[cfg(feature = "test-hooks")]
fn test_loopback_endpoint(endpoint: &str) -> bool {
    let Ok(url) = url::Url::parse(endpoint) else {
        return false;
    };
    matches!(url.scheme(), "http" | "https")
        && match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        }
}

/// Push to `actor_id`'s absent devices — **without** making the caller wait for
/// it. The one entry point for every sender-triggered push (inbox delivery,
/// knocks, invites, removals).
///
/// Two halves, split on purpose:
///
///   * the **decision** — which of the actor's devices are present? — is taken
///     here, synchronously, as a [`DevicePresence`] snapshot, so it reflects
///     the moment of the delivery and is ordered before the caller's reply;
///   * the **dispatch** — the DB read, one bounded POST per absent relay row
///     and one frame per present `ws-device` row — is spawned on the serving
///     generation (`AppState::spawn_scoped`) with that snapshot. The
///     endpoints are the *recipient's* choice, and a sender's
///     `fauna.inbox.send` must never run at the speed of a stranger's push
///     relay; nothing a sender is told depends on the outcome, which is why no
///     synchronous-before-reply property is kept.
pub fn dispatch_offline_push(
    state: &AppState,
    actor_id: &[u8; 32],
    rich_title: &str,
    rich_body: &str,
    url: &str,
) {
    let Some(push) = state.push_service.clone() else {
        return;
    };
    let presence = state.ws.device_presence(actor_id);
    // At the decision, never inside the task — see `dispatch_counters`.
    #[cfg(feature = "test-hooks")]
    push.counters
        .initiated
        .fetch_add(1, std::sync::atomic::Ordering::Release);

    let ws = Arc::clone(&state.ws);
    let actor_id = *actor_id;
    let (rich_title, rich_body, url) =
        (rich_title.to_owned(), rich_body.to_owned(), url.to_owned());
    state.spawn_scoped(async move {
        let outcome = match push
            .maybe_send_push(&ws, &presence, &actor_id, &rich_title, &rich_body, &url)
            .await
        {
            Ok(outcome) => outcome,
            Err(e) => {
                warn!("push dispatch failed: {e:#}");
                DispatchOutcome::default()
            }
        };
        #[cfg(feature = "test-hooks")]
        {
            use std::sync::atomic::Ordering::Release;
            push.counters.dialled.fetch_add(outcome.dialled, Release);
            push.counters
                .delivered
                .fetch_add(outcome.delivered, Release);
            // Last: `settled` vouches for the sums above (`dispatch_counters`).
            push.counters.settled.fetch_add(1, Release);
        }
        #[cfg(not(feature = "test-hooks"))]
        let _ = outcome;
    });
}

// ---------------------------------------------------------------------------
// RFC 8291 — Message Encryption for Web Push (aes128gcm)
// ---------------------------------------------------------------------------

/// Encrypt `plaintext` for a Web Push subscription using the `aes128gcm`
/// content encoding defined in RFC 8291.
///
/// Returns the complete encrypted record: salt (16 B) || rs (4 B) || keyid
/// length (1 B) || keyid (65 B uncompressed sender public key) || ciphertext.
fn encrypt_payload(
    receiver_public_key_bytes: &[u8],
    auth_secret: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    // Parse the receiver's public key.
    let receiver_pub = PublicKey::from_sec1_bytes(receiver_public_key_bytes)
        .context("parse receiver P-256 public key")?;

    // Generate an ephemeral sender key pair.
    let mut rng = rand::thread_rng();
    let sender_secret = EphemeralSecret::random(&mut rng);
    let sender_pub = sender_secret.public_key();
    let sender_pub_bytes = p256::EncodedPoint::from(&sender_pub);
    let sender_pub_uncompressed = sender_pub_bytes.as_bytes(); // 65 bytes

    // ECDH shared secret.
    let shared_secret = sender_secret.diffie_hellman(&receiver_pub);
    let ecdh_bytes = shared_secret.raw_secret_bytes(); // 32 bytes

    // Random 16-byte salt.
    let mut salt = [0u8; 16];
    rng.fill_bytes(&mut salt);

    // --- Key derivation (RFC 8291 §3.3) ---
    //
    // PRK_key = HKDF-Extract(auth_secret, ecdh_secret)
    // key_info = "WebPush: info\x00" || ua_public || as_public
    // IKM     = HKDF-Expand(PRK_key, key_info, 32)
    //
    // PRK     = HKDF-Extract(salt, IKM)
    // CEK     = HKDF-Expand(PRK, "Content-Encoding: aes128gcm\x00", 16)
    // nonce   = HKDF-Expand(PRK, "Content-Encoding: nonce\x00", 12)

    let receiver_pub_uncompressed = p256::EncodedPoint::from(&receiver_pub);
    let receiver_pub_bytes_full = receiver_pub_uncompressed.as_bytes(); // 65 bytes

    // PRK_key
    let (prk_key, _) = Hkdf::<Sha256>::extract(Some(auth_secret), ecdh_bytes.as_slice());

    // key_info
    let mut key_info = b"WebPush: info\x00".to_vec();
    key_info.extend_from_slice(receiver_pub_bytes_full);
    key_info.extend_from_slice(sender_pub_uncompressed);

    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::from_prk(prk_key.as_slice())
        .map_err(|e| anyhow::anyhow!("HKDF PRK_key invalid: {e}"))?
        .expand(&key_info, &mut ikm)
        .map_err(|e| anyhow::anyhow!("HKDF expand IKM: {e}"))?;

    // PRK (with salt)
    let (prk, _) = Hkdf::<Sha256>::extract(Some(&salt), &ikm);
    let prk_hkdf = Hkdf::<Sha256>::from_prk(prk.as_slice())
        .map_err(|e| anyhow::anyhow!("HKDF PRK invalid: {e}"))?;

    // CEK (16 bytes)
    let mut cek = [0u8; 16];
    prk_hkdf
        .expand(b"Content-Encoding: aes128gcm\x00", &mut cek)
        .map_err(|e| anyhow::anyhow!("HKDF expand CEK: {e}"))?;

    // Nonce (12 bytes)
    let mut nonce_bytes = [0u8; 12];
    prk_hkdf
        .expand(b"Content-Encoding: nonce\x00", &mut nonce_bytes)
        .map_err(|e| anyhow::anyhow!("HKDF expand nonce: {e}"))?;

    // --- AES-128-GCM encryption ---
    // Padding: plaintext || \x02 (delimiter byte, no padding)
    let mut buf = plaintext.to_vec();
    buf.push(0x02); // record delimiter

    let cipher = Aes128Gcm::new_from_slice(&cek).context("init AES-128-GCM")?;
    let nonce = GcmNonce::from_slice(&nonce_bytes);
    cipher
        .encrypt_in_place(nonce, b"", &mut buf)
        .map_err(|_| anyhow::anyhow!("AES-128-GCM encryption failed"))?;
    // `buf` now contains ciphertext || 16-byte tag

    // --- Build the aes128gcm content encoding record header ---
    // salt (16) || rs (4, big-endian u32) || idlen (1) || keyid (sender pub, 65)
    let rs: u32 = (buf.len() + 1) as u32; // conservative: single record
    let idlen: u8 = sender_pub_uncompressed.len() as u8; // 65

    let mut output = Vec::with_capacity(16 + 4 + 1 + sender_pub_uncompressed.len() + buf.len());
    output.extend_from_slice(&salt);
    output.extend_from_slice(&rs.to_be_bytes());
    output.push(idlen);
    output.extend_from_slice(sender_pub_uncompressed);
    output.extend_from_slice(&buf);

    Ok(output)
}

// ---------------------------------------------------------------------------
// RFC 8292 — VAPID JWT (ES256)
// ---------------------------------------------------------------------------

/// Build a VAPID `Authorization` header value for `endpoint`.
///
/// The JWT is signed with the server's P-256 private key using ES256 (ECDSA
/// over P-256 with SHA-256).  The token expires in 12 hours.
fn build_vapid_jwt(private_key: &SecretKey, url: &url::Url) -> Result<String> {
    // The audience is the endpoint's origin (scheme + host).
    let audience = format!(
        "{}://{}",
        url.scheme(),
        url.host_str().unwrap_or("localhost")
    );

    let now = fauna_core::data::Timestamp::now_secs() as u64;
    let exp = now + 12 * 3600;

    // Header: { "typ": "JWT", "alg": "ES256" }
    let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#);

    // Claims: { "aud": <origin>, "exp": <unix>, "sub": "mailto:push@localhost" }
    let claims_json =
        format!(r#"{{"aud":"{audience}","exp":{exp},"sub":"mailto:push@localhost"}}"#);
    let claims = URL_SAFE_NO_PAD.encode(claims_json.as_bytes());

    // Signing input.
    let signing_input = format!("{header}.{claims}");

    // Sign with ECDSA P-256 / SHA-256 (ES256).
    let signing_key = SigningKey::from(private_key.clone());
    let der_sig: DerSignature = signing_key.sign(signing_input.as_bytes());

    // Convert DER signature to raw (r || s, 64 bytes) for JWT.
    let sig_bytes = der_to_raw_p256_sig(der_sig.as_bytes())
        .context("convert DER signature to raw P-256 signature")?;
    let sig_b64 = URL_SAFE_NO_PAD.encode(&sig_bytes);

    let token = format!("{signing_input}.{sig_b64}");

    // The public key in uncompressed, base64url-encoded form.
    let pub_key_bytes = p256::EncodedPoint::from(private_key.public_key());
    let pub_key_b64 = URL_SAFE_NO_PAD.encode(pub_key_bytes.as_bytes());

    Ok(format!("vapid t={token},k={pub_key_b64}"))
}

// ---------------------------------------------------------------------------
// APNs — Apple Push Notification service
// ---------------------------------------------------------------------------

/// Parse an Apple .p8 key file (PKCS8 PEM containing an EC P-256 private key)
/// and return the `SecretKey`.
fn parse_p8_key(pem: &str) -> Result<SecretKey> {
    // Apple .p8 files are PKCS8-encoded P-256 keys in PEM format.
    // Strip PEM headers and decode the base64 body.
    let der_b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    let der_bytes = base64::engine::general_purpose::STANDARD
        .decode(&der_b64)
        .context("decode .p8 key base64")?;
    SecretKey::from_pkcs8_der(&der_bytes).map_err(|e| anyhow::anyhow!("invalid .p8 key: {e}"))
}

/// Build an APNs JWT token signed with ES256.
///
/// The JWT has header `{"alg":"ES256","kid":key_id}` and claims
/// `{"iss":team_id,"iat":now}`.  Returns the complete `header.claims.signature`
/// token string.
fn build_apns_jwt(signing_key: &SecretKey, key_id: &str, team_id: &str) -> Result<String> {
    let now = fauna_core::data::Timestamp::now_secs() as u64;

    let header_json = format!(r#"{{"alg":"ES256","kid":"{key_id}"}}"#);
    let claims_json = format!(r#"{{"iss":"{team_id}","iat":{now}}}"#);

    let header = URL_SAFE_NO_PAD.encode(header_json.as_bytes());
    let claims = URL_SAFE_NO_PAD.encode(claims_json.as_bytes());

    let signing_input = format!("{header}.{claims}");

    let sk = SigningKey::from(signing_key.clone());
    let der_sig: DerSignature = sk.sign(signing_input.as_bytes());

    let sig_bytes = der_to_raw_p256_sig(der_sig.as_bytes())
        .context("convert DER signature to raw P-256 signature")?;
    let sig_b64 = URL_SAFE_NO_PAD.encode(&sig_bytes);

    Ok(format!("{signing_input}.{sig_b64}"))
}

/// Convert a DER-encoded P-256 ECDSA signature to the raw 64-byte (r || s)
/// format required by JSON Web Signatures (RFC 7518).
fn der_to_raw_p256_sig(der: &[u8]) -> Result<Vec<u8>> {
    // DER SEQUENCE { INTEGER r; INTEGER s }
    // Minimal parser — assumes well-formed DER from p256.
    if der.len() < 2 || der[0] != 0x30 {
        bail!("DER signature does not start with SEQUENCE tag");
    }
    let seq_len = der[1] as usize;
    if der.len() < 2 + seq_len {
        bail!("DER signature truncated");
    }
    let body = &der[2..2 + seq_len];

    let (r_bytes, rest) = parse_der_integer(body)?;
    let (s_bytes, _) = parse_der_integer(rest)?;

    // Pad each to 32 bytes.
    let mut out = vec![0u8; 64];
    let r_start = 32usize.saturating_sub(r_bytes.len());
    let s_start = 32usize.saturating_sub(s_bytes.len());
    let r_len = r_bytes.len().min(32);
    let s_len = s_bytes.len().min(32);
    out[r_start..r_start + r_len].copy_from_slice(&r_bytes[r_bytes.len() - r_len..]);
    out[32 + s_start..32 + s_start + s_len].copy_from_slice(&s_bytes[s_bytes.len() - s_len..]);
    Ok(out)
}

/// Parse a DER INTEGER TLV from `data`, returning the (value bytes, remainder).
fn parse_der_integer(data: &[u8]) -> Result<(&[u8], &[u8])> {
    if data.len() < 2 || data[0] != 0x02 {
        bail!("expected DER INTEGER tag");
    }
    let len = data[1] as usize;
    if data.len() < 2 + len {
        bail!("DER INTEGER truncated");
    }
    let val = &data[2..2 + len];
    // Strip leading zero byte (sign byte) if present.
    let val = val.strip_prefix(&[0x00]).unwrap_or(val);
    Ok((val, &data[2 + len..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use crate::ws::WsState;
    use p256::elliptic_curve::pkcs8::EncodePrivateKey as _;
    use std::sync::Arc;

    fn make_db() -> Arc<CacheDb> {
        Arc::new(CacheDb::open_in_memory().unwrap())
    }

    /// Generate a fresh P-256 private key and return it as a PKCS8 PEM-encoded
    /// `Vec<u8>` suitable for passing to `PushService::new`.
    fn generate_test_vapid_pem() -> Vec<u8> {
        let mut rng = rand::thread_rng();
        let secret = SecretKey::random(&mut rng);
        let der = secret.to_pkcs8_der().expect("encode PKCS8 DER");
        let b64 = base64::engine::general_purpose::STANDARD.encode(der.as_bytes());
        let mut pem = String::from("-----BEGIN PRIVATE KEY-----\n"); // gitleaks:allow
        for chunk in b64.as_bytes().chunks(64) {
            pem.push_str(std::str::from_utf8(chunk).unwrap());
            pem.push('\n');
        }
        pem.push_str("-----END PRIVATE KEY-----\n");
        pem.into_bytes()
    }

    fn make_push_service(db: Arc<CacheDb>) -> PushService {
        let pem = generate_test_vapid_pem();
        PushService::new(db, pem, None).unwrap()
    }

    /// First boot (no persisted row): `ensure_vapid_pem` mints a fresh keypair,
    /// persists it, and hands back a PEM `PushService::new` can parse — the
    /// out-of-the-box path this row exists for (no `--vapid-pem` needed).
    #[tokio::test]
    async fn ensure_vapid_pem_generates_and_persists_on_first_boot() {
        let db = make_db();
        assert!(
            db.get_vapid_pem().await.unwrap().is_none(),
            "no row before the first ensure_vapid_pem call"
        );

        let pem = ensure_vapid_pem(&db).await.unwrap();
        assert_eq!(
            db.get_vapid_pem().await.unwrap().as_deref(),
            Some(pem.as_slice()),
            "the generated PEM must be persisted"
        );
        // The generated PEM must be usable, exactly like a caller-supplied one.
        PushService::new(db, pem, None).expect("generated PEM must parse as a valid VAPID key");
    }

    /// A later boot (row already present) must NOT rotate the keypair — a
    /// changing VAPID identity would silently invalidate every browser's
    /// existing push subscription.
    #[tokio::test]
    async fn ensure_vapid_pem_does_not_rotate_on_a_later_boot() {
        let db = make_db();
        let first = ensure_vapid_pem(&db).await.unwrap();
        let second = ensure_vapid_pem(&db).await.unwrap();
        assert_eq!(
            first, second,
            "ensure_vapid_pem must return the SAME key once one is persisted"
        );
    }

    #[tokio::test]
    async fn push_service_parses_vapid_key() {
        let db = make_db();
        let svc = make_push_service(db);

        let pub_key = svc.vapid_public_key();

        // Uncompressed P-256 public key is 65 bytes; base64url without padding
        // encodes 65 bytes as ceil(65*4/3) = 87 characters.
        assert_eq!(
            pub_key.len(),
            87,
            "base64url-encoded uncompressed P-256 public key must be 87 chars, got: {pub_key}"
        );

        // Must be valid base64url (no '=' padding, no '+' or '/').
        assert!(
            pub_key
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_'),
            "public key must be valid base64url: {pub_key}"
        );

        // Decode and verify it starts with 0x04 (uncompressed point marker).
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(pub_key)
            .expect("public key must decode as base64url");
        assert_eq!(decoded.len(), 65, "uncompressed P-256 point is 65 bytes");
        assert_eq!(
            decoded[0], 0x04,
            "must start with uncompressed point marker 0x04"
        );
    }

    const TEST_P256DH: &str =
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    fn presence(announced: &[&str], any_unannounced: bool) -> DevicePresence {
        DevicePresence {
            announced: announced.iter().map(|d| d.to_string()).collect(),
            any_unannounced,
        }
    }

    /// The per-device rule, row by row (`apps/common.md` § Dispatch Logic):
    /// a relay row of a present device is skipped and of an absent one dialled;
    /// a `ws-device` row is delivered when present and skipped when absent; an
    /// unannounced connection suppresses every relay row and no `ws-device` one.
    #[test]
    fn the_decision_is_per_device() {
        let none = presence(&[], false);
        let phone_here = presence(&["phone"], false);
        let desk_here = presence(&["desk"], false);
        let unannounced = presence(&[], true);
        let unannounced_and_desk = presence(&["desk"], true);

        for relay in ["web-push", "apns"] {
            assert_eq!(
                none.action_for(relay, "phone"),
                RowAction::Dial,
                "{relay}: absent"
            );
            assert_eq!(
                phone_here.action_for(relay, "phone"),
                RowAction::Skip,
                "{relay}: present"
            );
            assert_eq!(
                desk_here.action_for(relay, "phone"),
                RowAction::Dial,
                "{relay}: another device's presence does not quiet this one"
            );
            assert_eq!(
                unannounced.action_for(relay, "phone"),
                RowAction::Skip,
                "{relay}: an unannounced connection keeps the old actor-wide skip"
            );
        }
        assert_eq!(
            desk_here.action_for("ws-device", "desk"),
            RowAction::Deliver
        );
        assert_eq!(none.action_for("ws-device", "desk"), RowAction::Skip);
        assert_eq!(phone_here.action_for("ws-device", "desk"), RowAction::Skip);
        assert_eq!(
            unannounced_and_desk.action_for("ws-device", "desk"),
            RowAction::Deliver,
            "the compatibility rider covers the relay transports only"
        );
        assert_eq!(none.action_for("carrier-pigeon", "phone"), RowAction::Skip);
    }

    #[test]
    fn a_ws_device_row_is_its_own_device_id_with_no_keys() {
        assert_eq!(
            validate_ws_device_subscription("desk", "desk", false),
            Ok(())
        );
        for (endpoint, has_keys) in [("other", false), ("", false), ("desk", true)] {
            assert_eq!(
                validate_ws_device_subscription("desk", endpoint, has_keys),
                Err(EndpointRefusal::WsDevice),
                "endpoint {endpoint:?}, keys {has_keys}"
            );
        }
    }

    /// A relay row whose device is present — announced on a live connection,
    /// or covered by an unannounced one — is never dialled. Its endpoint is a
    /// shape the dial guard would pass, so only the decision stands between it
    /// and a POST.
    #[tokio::test]
    async fn a_present_device_is_not_dialled() {
        let db = make_db();
        let actor_id = [0xABu8; 32];
        db.upsert_push_subscription(
            &actor_id,
            "device-test",
            "web-push",
            "https://push.example.com/wp/fake-endpoint",
            Some(TEST_P256DH),
            Some("AAAAAAAAAAAAAAAA"),
        )
        .await
        .unwrap();
        let svc = make_push_service(Arc::clone(&db));

        for present in [presence(&["device-test"], false), presence(&[], true)] {
            let outcome = svc
                .maybe_send_push(&WsState::new(), &present, &actor_id, "t", "b", "/app/test")
                .await
                .unwrap();
            assert_eq!(outcome, DispatchOutcome::default(), "{present:?}");
        }
    }

    /// A `ws-device` row is delivered as a `fauna.push.notification` frame to
    /// the connections that announced its device — and only those — carrying
    /// the generic strings and the deep link; nothing is dialled.
    #[tokio::test]
    async fn a_ws_device_row_is_delivered_to_its_announcing_connection() {
        let db = make_db();
        let actor_id = [0xB1u8; 32];
        db.upsert_push_subscription(&actor_id, "desk", "ws-device", "desk", None, None)
            .await
            .unwrap();
        let svc = make_push_service(Arc::clone(&db));
        let ws = WsState::new();
        let (desk, mut desk_rx) = ws.subscribe(actor_id);
        let (laptop, mut laptop_rx) = ws.subscribe(actor_id);
        assert!(ws.announce_device(&actor_id, desk.conn_id, "desk"));
        assert!(ws.announce_device(&actor_id, laptop.conn_id, "laptop"));

        let outcome = svc
            .maybe_send_push(
                &ws,
                &ws.device_presence(&actor_id),
                &actor_id,
                "t",
                "b",
                "/app/inbox",
            )
            .await
            .unwrap();
        assert_eq!(
            outcome,
            DispatchOutcome {
                dialled: 0,
                delivered: 1
            }
        );

        let frame = fauna_protocol::decode_frame(&desk_rx.try_recv().expect("a frame for desk"))
            .expect("a well-formed frame");
        let fauna_protocol::Frame::Push(push) = frame else {
            panic!("expected a Push frame, got {frame:?}");
        };
        match fauna_protocol::PushEvent::from_push(&push.kind, push.payload) {
            fauna_protocol::PushEvent::PushNotification(n) => {
                assert_eq!(
                    (n.title.as_str(), n.body.as_str()),
                    (GENERIC_TITLE, GENERIC_BODY)
                );
                assert_eq!(n.url, "/app/inbox");
            }
            other => panic!("expected fauna.push.notification, got {other:?}"),
        }
        assert!(
            laptop_rx.try_recv().is_err(),
            "another device's connection gets nothing"
        );
    }

    /// With no live connection announcing the row's device, a `ws-device` row
    /// is skipped and nothing is queued — the event itself is durable.
    #[tokio::test]
    async fn an_absent_ws_device_is_skipped() {
        let db = make_db();
        let actor_id = [0xB2u8; 32];
        db.upsert_push_subscription(&actor_id, "desk", "ws-device", "desk", None, None)
            .await
            .unwrap();
        let svc = make_push_service(Arc::clone(&db));
        let ws = WsState::new();
        // An unannounced connection is live: it covers relay rows, not this one.
        let (_old_app, mut old_app_rx) = ws.subscribe(actor_id);

        let outcome = svc
            .maybe_send_push(
                &ws,
                &ws.device_presence(&actor_id),
                &actor_id,
                "t",
                "b",
                "/app",
            )
            .await
            .unwrap();
        assert_eq!(outcome, DispatchOutcome::default());
        assert!(
            old_app_rx.try_recv().is_err(),
            "nothing sent to an unannounced connection"
        );
    }

    #[tokio::test]
    async fn push_skips_actors_without_subscriptions() {
        let db = make_db();
        let actor_id = [0xCDu8; 32];

        // No subscriptions registered for this actor.
        let svc = make_push_service(db);

        let result = svc
            .maybe_send_push(
                &WsState::new(),
                &DevicePresence::default(),
                &actor_id,
                "Test title",
                "Test body",
                "/app/test",
            )
            .await;

        assert!(
            result.is_ok(),
            "maybe_send_push should return Ok(()) when no subscriptions exist"
        );
    }

    /// The shipped posture at `fauna.push.subscribe`, asserted on the cfg-free
    /// strict function so the all-features gate arm runs it too: no loopback,
    /// private, link-local or metadata address, no plain http, and an `apns`
    /// token that can only ever be a token.
    #[test]
    fn subscribe_refuses_an_undialable_endpoint_in_a_shipped_build() {
        for endpoint in [
            "http://127.0.0.1:8080/push/x",
            "https://127.0.0.1:8080/push/x",
            "https://[::1]/push/x",
            "https://localhost/push/x",
            "https://192.168.1.10/push/x",
            "https://10.0.0.7/push/x",
            "https://169.254.169.254/latest/meta-data/",
            "http://fcm.googleapis.com/fcm/send/x",
            "fcm.googleapis.com/fcm/send/x",
            "",
        ] {
            assert_eq!(
                validate_subscription_endpoint_strict("web-push", endpoint),
                Err(EndpointRefusal::WebPush),
                "{endpoint:?} must be refused"
            );
        }
        assert_eq!(
            validate_subscription_endpoint_strict(
                "web-push",
                "https://fcm.googleapis.com/fcm/send/x"
            ),
            Ok(())
        );

        assert_eq!(
            validate_subscription_endpoint_strict("apns", "a1f1b2c3d4e5f6a7b8c9d0e1f2a3b4c5"),
            Ok(())
        );
        for token in ["", "a]f1b2", "../../3/device/abc", "abc?x=1", "abc def"] {
            assert_eq!(
                validate_subscription_endpoint_strict("apns", token),
                Err(EndpointRefusal::Apns),
                "{token:?} must be refused"
            );
        }
        let too_long = "a".repeat(APNS_DEVICE_TOKEN_MAX_HEX + 1);
        assert_eq!(
            validate_subscription_endpoint_strict("apns", &too_long),
            Err(EndpointRefusal::Apns)
        );
    }

    /// The shipped posture at the DIAL — what stands between a row that got in
    /// anyway (a dial policy tighter than the subscribe check, or its name re-resolved) and a
    /// socket.
    #[tokio::test]
    async fn dial_refuses_a_loopback_or_http_endpoint_in_a_shipped_build() {
        assert_eq!(
            web_push_client_strict("https://127.0.0.1:8444/push/x")
                .await
                .map(|_| ()),
            Err(SsrfError::NonGlobal)
        );
        assert_eq!(
            web_push_client_strict("http://127.0.0.1:8444/push/x")
                .await
                .map(|_| ()),
            Err(SsrfError::BadScheme)
        );
        assert_eq!(
            web_push_client_strict("https://169.254.169.254/push/x")
                .await
                .map(|_| ()),
            Err(SsrfError::NonGlobal)
        );
    }

    /// The test hook admits a loopback IP literal and nothing else, so a test
    /// build refuses every other internal address exactly as a shipped one does.
    #[cfg(feature = "test-hooks")]
    #[test]
    fn test_hook_admits_a_loopback_literal_and_nothing_else() {
        assert!(test_loopback_endpoint("http://127.0.0.1:9/push/x"));
        assert!(test_loopback_endpoint("https://[::1]:9/push/x"));
        for endpoint in [
            "http://localhost:9/push/x",
            "http://10.0.0.7/push/x",
            "http://192.168.1.10/push/x",
            "http://169.254.169.254/latest/meta-data/",
            "http://example.com/push/x",
            "ftp://127.0.0.1/push/x",
        ] {
            assert!(!test_loopback_endpoint(endpoint), "{endpoint}");
            assert_eq!(
                validate_subscription_endpoint("web-push", endpoint),
                Err(EndpointRefusal::WebPush),
                "{endpoint}"
            );
        }
    }

    /// A listener that accepts connections and never answers — the hung push
    /// service. Returns its endpoint URL; the listener lives as long as the
    /// returned guard.
    #[cfg(feature = "test-hooks")]
    async fn hung_endpoint() -> (String, tokio::net::TcpListener) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/push/hung", listener.local_addr().unwrap());
        (url, listener)
    }

    #[cfg(feature = "test-hooks")]
    async fn subscribe_web_push(db: &CacheDb, actor_id: &[u8], device_id: &str, endpoint: &str) {
        let receiver = SecretKey::random(&mut rand::thread_rng()).public_key();
        let p256dh = URL_SAFE_NO_PAD.encode(p256::EncodedPoint::from(&receiver).as_bytes());
        let auth = URL_SAFE_NO_PAD.encode([7u8; 16]);
        db.upsert_push_subscription(
            actor_id,
            device_id,
            "web-push",
            endpoint,
            Some(&p256dh),
            Some(&auth),
        )
        .await
        .unwrap();
    }

    /// An endpoint that accepts and never answers costs a dispatch one request
    /// bound, not forever. Virtual time: the paused clock auto-advances to the
    /// request timeout, so the test takes no real seconds and asserts the bound
    /// itself rather than a latency.
    #[cfg(feature = "test-hooks")]
    #[tokio::test(start_paused = true)]
    async fn a_hung_endpoint_costs_one_request_bound() {
        let db = make_db();
        let actor_id = [0x11u8; 32];
        let (endpoint, _listener) = hung_endpoint().await;
        subscribe_web_push(&db, &actor_id, "hung-1", &endpoint).await;
        let svc = make_push_service(Arc::clone(&db));

        let started = tokio::time::Instant::now();
        svc.maybe_send_push(
            &WsState::new(),
            &DevicePresence::default(),
            &actor_id,
            "t",
            "b",
            "/app",
        )
        .await
        .unwrap();
        let elapsed = started.elapsed();

        assert!(
            elapsed >= PUSH_REQUEST_TIMEOUT && elapsed < PUSH_DISPATCH_DEADLINE,
            "one hung endpoint must end at the request bound, took {elapsed:?}"
        );
        // A timeout says nothing about the endpoint being gone: the row stays.
        assert_eq!(
            db.list_push_subscriptions(&actor_id).await.unwrap().len(),
            1
        );
    }

    /// Many hung endpoints do not multiply the request bound: the whole
    /// dispatch ends at its own deadline.
    #[cfg(feature = "test-hooks")]
    #[tokio::test(start_paused = true)]
    async fn many_hung_endpoints_end_at_the_dispatch_deadline() {
        let db = make_db();
        let actor_id = [0x12u8; 32];
        let (endpoint, _listener) = hung_endpoint().await;
        for n in 0..6 {
            subscribe_web_push(&db, &actor_id, &format!("hung-{n}"), &endpoint).await;
        }
        let svc = make_push_service(Arc::clone(&db));

        let started = tokio::time::Instant::now();
        svc.maybe_send_push(
            &WsState::new(),
            &DevicePresence::default(),
            &actor_id,
            "t",
            "b",
            "/app",
        )
        .await
        .unwrap();
        let elapsed = started.elapsed();

        assert!(
            elapsed >= PUSH_DISPATCH_DEADLINE && elapsed < PUSH_DISPATCH_DEADLINE * 2,
            "six hung endpoints must end at the dispatch deadline, took {elapsed:?}"
        );
    }

    /// The decision reflects the moment of the event (`apps/common.md`
    /// § Dispatch Logic, bound 1): the presence snapshot taken in the sender's
    /// handler travels to the background dispatch, so a device that was present
    /// when the event happened is not dialled even if it has left by the time
    /// the task runs. `settled` only moves after the outcome is summed in.
    #[cfg(feature = "test-hooks")]
    #[tokio::test(start_paused = true)]
    async fn the_dispatch_decides_on_the_snapshot_at_the_event() {
        let db = make_db();
        let actor_id = [0x15u8; 32];
        let (endpoint, _listener) = hung_endpoint().await;
        subscribe_web_push(&db, &actor_id, "phone", &endpoint).await;

        let mut state = AppState::for_test(Arc::clone(&db));
        let svc = Arc::new(make_push_service(Arc::clone(&db)));
        state.push_service = Some(Arc::clone(&svc));
        let (phone, _rx) = state.ws.subscribe(actor_id);
        assert!(state.ws.announce_device(&actor_id, phone.conn_id, "phone"));

        dispatch_offline_push(&state, &actor_id, "t", "b", "/app");
        // The phone's app closes before the spawned dispatch gets to run.
        state.ws.remove(&actor_id, phone.conn_id);

        state.serve_tasks.close();
        state.serve_tasks.wait().await;
        let counters = svc.dispatch_counters();
        assert_eq!((counters.initiated, counters.settled), (1, 1));
        assert_eq!(
            counters.dialled, 0,
            "present at the event, so never dialled"
        );
    }

    /// The sender's side of the bargain: `dispatch_offline_push` is not even
    /// async, so a caller cannot wait on the endpoint — it records the decision
    /// and hands the dial to the serving generation.
    #[cfg(feature = "test-hooks")]
    #[tokio::test(start_paused = true)]
    async fn a_sender_never_waits_on_the_endpoint() {
        let db = make_db();
        let actor_id = [0x13u8; 32];
        let (endpoint, _listener) = hung_endpoint().await;
        subscribe_web_push(&db, &actor_id, "hung-1", &endpoint).await;

        let mut state = AppState::for_test(Arc::clone(&db));
        let svc = Arc::new(make_push_service(Arc::clone(&db)));
        state.push_service = Some(Arc::clone(&svc));

        let started = tokio::time::Instant::now();
        dispatch_offline_push(&state, &actor_id, "t", "b", "/app");
        assert_eq!(
            started.elapsed(),
            Duration::ZERO,
            "the decision must not await the dial"
        );
        assert_eq!(svc.dispatch_counters().initiated, 1, "one offline decision");
        assert_eq!(state.serve_tasks.len(), 1, "the dial rides a scoped task");

        // Teardown reaps it: the task is scoped to the serving generation.
        state.serve_generation.cancel();
        state.serve_tasks.close();
        state.serve_tasks.wait().await;
    }

    /// A row the dial guard refuses from its text alone can never be dialled, so
    /// it is cleaned up like a dead endpoint; nothing is POSTed to it. (Such a
    /// row appears when the dial policy tightens past the subscribe-time check.)
    #[tokio::test]
    async fn an_undialable_row_is_removed_without_a_dial() {
        let db = make_db();
        let actor_id = [0x14u8; 32];
        db.upsert_push_subscription(
            &actor_id,
            "legacy-plain-http",
            "web-push",
            "http://push.example.com/wp/legacy",
            Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
            Some("AAAAAAAAAAAAAAAA"),
        )
        .await
        .unwrap();
        let svc = make_push_service(Arc::clone(&db));

        svc.maybe_send_push(
            &WsState::new(),
            &DevicePresence::default(),
            &actor_id,
            "t",
            "b",
            "/app",
        )
        .await
        .unwrap();

        assert!(
            db.list_push_subscriptions(&actor_id)
                .await
                .unwrap()
                .is_empty(),
            "a plain-http row must be removed, not dialled"
        );
    }

    #[tokio::test]
    async fn der_to_raw_p256_sig_roundtrip() {
        // Generate a fresh key, sign a message, and verify the DER→raw conversion
        // produces exactly 64 bytes.
        use p256::ecdsa::{DerSignature, SigningKey, signature::Signer as _};

        let pem = generate_test_vapid_pem();
        let pem_str = std::str::from_utf8(&pem).unwrap();
        let secret = SecretKey::from_pkcs8_pem(pem_str).unwrap();
        let signing_key = SigningKey::from(&secret);
        let msg = b"test signing input";
        let der_sig: DerSignature = signing_key.sign(msg);

        let raw = der_to_raw_p256_sig(der_sig.as_bytes()).unwrap();
        assert_eq!(raw.len(), 64, "raw EC signature must be 64 bytes (r || s)");
    }

    #[test]
    fn apns_jwt_has_correct_structure() {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;

        // Generate a test P-256 key.
        let mut rng = rand::thread_rng();
        let secret = SecretKey::random(&mut rng);

        let jwt = build_apns_jwt(&secret, "ABC123KEYID", "TEAMID9999").unwrap();

        // JWT must have 3 dot-separated parts.
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT must have 3 parts, got {}", parts.len());

        // Decode and verify header.
        let header_bytes = URL_SAFE_NO_PAD.decode(parts[0]).expect("decode header");
        let header: serde_json::Value =
            serde_json::from_slice(&header_bytes).expect("parse header JSON");
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["kid"], "ABC123KEYID");

        // Decode and verify claims.
        let claims_bytes = URL_SAFE_NO_PAD.decode(parts[1]).expect("decode claims");
        let claims: serde_json::Value =
            serde_json::from_slice(&claims_bytes).expect("parse claims JSON");
        assert_eq!(claims["iss"], "TEAMID9999");
        assert!(
            claims["iat"].is_number(),
            "iat must be a number, got: {}",
            claims["iat"]
        );

        // Decode signature — must be exactly 64 bytes (r || s for P-256).
        let sig_bytes = URL_SAFE_NO_PAD.decode(parts[2]).expect("decode signature");
        assert_eq!(
            sig_bytes.len(),
            64,
            "ES256 signature must be 64 bytes, got {}",
            sig_bytes.len()
        );
    }

    #[tokio::test]
    async fn push_skips_apns_when_not_configured() {
        let db = make_db();
        let actor_id = [0xEFu8; 32];

        // Register an APNs subscription (with valid-looking keys).
        db.upsert_push_subscription(
            &actor_id,
            "ios-device-1",
            "apns",
            "abc123def456", // device token (hex)
            Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
            Some("AAAAAAAAAAAAAAAA"),
        )
        .await
        .unwrap();

        // Create PushService with no APNs config.
        let svc = make_push_service(Arc::clone(&db));

        // Should return Ok without error — the APNs subscription is silently
        // skipped when APNs is not configured.
        let result = svc
            .maybe_send_push(
                &WsState::new(),
                &DevicePresence::default(),
                &actor_id,
                "Test title",
                "Test body",
                "/app/test",
            )
            .await;

        assert!(
            result.is_ok(),
            "maybe_send_push should return Ok(()) even with unconfigured APNs"
        );
    }
}
