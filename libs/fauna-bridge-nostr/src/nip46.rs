//! NIP-46 Nostr Connect — both halves.
//!
//! **Client side** (`BunkerUrl`, `NostrConnectClient`): the nest as an *app*
//! talking to the user's own bunker. Used for exactly one thing today — a
//! `remote` account link proves possession of the user pubkey by completing
//! the handshake (`connect` → `get_public_key` → `sign_event` over the nest's
//! challenge) before any row is written (`docs/goal/ui/nostr.md` § Errors &
//! edge cases → *Proof of possession*). The nest never holds the key.
//!
//! **Signer side** (below the divider): the nest as the user's bunker.

use std::time::Duration;

use anyhow::{Context, Result};
use fauna_core::identity_op::IdentityOpClass;

use crate::nip01::{ClientMessage, RelayMessage};
use crate::relay_client::{RelayClient, RelayDialPolicy};
use crate::signing::Keypair;
use crate::types::{Event, Filter, Tag, UnsignedEvent};

/// NIP-46 request/response events ride kind 24133.
pub const NOSTR_CONNECT_KIND: u64 = 24133;

/// A parsed `bunker://<remote-signer-pubkey>?relay=<url>[&relay=<url>…][&secret=<s>]`
/// connection string (NIP-46 § Connection). The pubkey names the **remote
/// signer**, which by design need not be the user's key — the user pubkey is
/// learned with `get_public_key` after `connect`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BunkerUrl {
    /// 64 lowercase hex — the remote signer's pubkey.
    pub signer_pubkey: String,
    /// Every `relay=` parameter, in order; never empty.
    pub relays: Vec<String>,
    /// The optional one-time `secret=` the bunker minted with the string.
    pub secret: Option<String>,
}

impl BunkerUrl {
    /// Parse a bunker:// URL. Query values may be percent-encoded (real
    /// bunkers do both); the pubkey is normalized to lowercase hex.
    pub fn parse(url: &str) -> Result<Self> {
        let rest = url
            .trim()
            .strip_prefix("bunker://")
            .context("bunker URL must start with bunker://")?;

        let (pubkey, query) = rest.split_once('?').context("bunker URL missing ?relay=")?;
        let pubkey = pubkey.trim_end_matches('/').to_ascii_lowercase();
        if pubkey.len() != 64 || hex::decode(&pubkey).is_err() {
            anyhow::bail!("bunker pubkey must be 64 hex chars");
        }

        let mut relays = Vec::new();
        let mut secret = None;
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let value = urlencoding::decode(value)
                .map(|v| v.into_owned())
                .unwrap_or_else(|_| value.to_string());
            match key {
                "relay" if !value.is_empty() => relays.push(value),
                "secret" if !value.is_empty() => secret = Some(value),
                _ => {}
            }
        }
        if relays.is_empty() {
            anyhow::bail!("bunker URL missing relay= parameter");
        }

        Ok(Self {
            signer_pubkey: pubkey,
            relays,
            secret,
        })
    }

    /// The first relay — the one [`NostrConnectClient::connect`] dials first.
    pub fn relay_url(&self) -> &str {
        &self.relays[0]
    }

    /// The connection string with its one-time secret removed — what a nest
    /// may keep at rest after the handshake consumed the secret.
    pub fn without_secret(&self) -> String {
        let mut s = format!("bunker://{}", self.signer_pubkey);
        for (i, relay) in self.relays.iter().enumerate() {
            s.push(if i == 0 { '?' } else { '&' });
            s.push_str("relay=");
            s.push_str(relay);
        }
        s
    }
}

/// A NIP-46 Nostr Connect client: one relay connection, one ephemeral client
/// keypair, one remote signer. Requests are NIP-44-encrypted (the current
/// spec); responses are accepted in whichever scheme the signer answers
/// ([`detect_scheme`]). Life cycle: [`Self::dial`] (reach a relay) →
/// [`Self::connect`] (the handshake) → the methods.
pub struct NostrConnectClient {
    relay: RelayClient,
    client_keypair: Keypair,
    signer_pubkey: String,
    signer_pk: [u8; 32],
    /// The `connect` secret, echoed by spec-current bunkers instead of `ack`.
    secret: Option<String>,
}

impl NostrConnectClient {
    /// Dial the bunker's relays in order until one connects and subscribe to
    /// the signer's answers. A failure here is a *reachability* failure (no
    /// relay answered); nothing has been asked of the signer yet. `timeout`
    /// bounds each relay dial. The `relay=` parameters are whatever the user
    /// pasted, so each dial is verified under `policy` before any socket is
    /// opened — a bunker string naming a loopback, private or
    /// cloud-metadata relay is a reachability failure like any other.
    pub async fn dial(
        bunker: &BunkerUrl,
        timeout: Duration,
        policy: RelayDialPolicy,
    ) -> Result<Self> {
        let mut signer_pk = [0u8; 32];
        signer_pk.copy_from_slice(&hex::decode(&bunker.signer_pubkey).context("signer pubkey")?);

        let mut last_err = None;
        let mut relay = None;
        for url in &bunker.relays {
            match tokio::time::timeout(timeout, RelayClient::connect(url, policy)).await {
                Ok(Ok(r)) => {
                    relay = Some(r);
                    break;
                }
                Ok(Err(e)) => last_err = Some(e.context(format!("relay {url}"))),
                Err(_) => last_err = Some(anyhow::anyhow!("relay {url}: connect timeout")),
            }
        }
        let relay = relay.ok_or_else(|| {
            last_err.unwrap_or_else(|| anyhow::anyhow!("bunker URL names no relay"))
        })?;

        let client_keypair = Keypair::generate();
        let mut client = Self {
            relay,
            client_keypair,
            signer_pubkey: bunker.signer_pubkey.clone(),
            signer_pk,
            secret: bunker.secret.clone(),
        };

        // Answers are kind-24133 events p-tagged with OUR pubkey; subscribe
        // before the first request so the reply cannot race the REQ.
        let mut filter = Filter {
            kinds: Some(vec![NOSTR_CONNECT_KIND]),
            ..Default::default()
        };
        filter
            .tags
            .insert("#p".into(), vec![client.client_keypair.public_key_hex()]);
        client
            .relay
            .subscribe("nip46", vec![filter])
            .await
            .context("subscribe for NIP-46 answers")?;
        Ok(client)
    }

    /// Complete `connect [remote-signer-pubkey, secret?]`. A spec-current
    /// bunker answers with the secret it minted, an older one with "ack";
    /// anything else — "unauthorized" for a wrong or spent secret above all —
    /// is a refusal.
    pub async fn connect(&mut self, timeout: Duration) -> Result<()> {
        let mut params = vec![self.signer_pubkey.clone()];
        if let Some(secret) = &self.secret {
            params.push(secret.clone());
        }
        let result = self.request("connect", params, timeout).await?;
        let accepted = result == "ack" || self.secret.as_deref() == Some(result.as_str());
        if !accepted {
            anyhow::bail!("bunker did not accept connect: {result}");
        }
        Ok(())
    }

    /// The client's ephemeral public key (hex).
    pub fn client_pubkey(&self) -> String {
        self.client_keypair.public_key_hex()
    }

    /// `get_public_key` — the **user** pubkey the bunker signs as (64 hex).
    pub async fn get_public_key(&mut self, timeout: Duration) -> Result<String> {
        let pubkey = self
            .request("get_public_key", vec![], timeout)
            .await?
            .to_ascii_lowercase();
        if pubkey.len() != 64 || hex::decode(&pubkey).is_err() {
            anyhow::bail!("bunker returned a malformed pubkey");
        }
        Ok(pubkey)
    }

    /// `sign_event` — hand the bunker an unsigned event as NIP-01 JSON
    /// (`{"kind","created_at","tags","content"[,"pubkey"]}`) and get the
    /// signed event back.
    pub async fn sign_event(&mut self, unsigned_json: &str, timeout: Duration) -> Result<Event> {
        let signed = self
            .request("sign_event", vec![unsigned_json.to_string()], timeout)
            .await?;
        serde_json::from_str(&signed).context("bunker returned a malformed signed event")
    }

    /// One NIP-46 round trip: encrypt + publish the request, then read the
    /// relay until the signer's answer with our request id arrives. `Ok` is
    /// the JSON-RPC `result` string; a signer-side `error` is an `Err`.
    async fn request(
        &mut self,
        method: &str,
        params: Vec<String>,
        timeout: Duration,
    ) -> Result<String> {
        let mut id_bytes = [0u8; 8];
        getrandom::fill(&mut id_bytes).context("generate request id")?;
        let request_id = hex::encode(id_bytes);

        let request = serde_json::json!({
            "id": request_id,
            "method": method,
            "params": params,
        });
        let content = crate::nip44::nip44_encrypt(
            &self.client_keypair.secret_bytes(),
            &self.signer_pk,
            &serde_json::to_string(&request)?,
        )
        .context("NIP-44 encrypt request")?;

        let wrapper = UnsignedEvent {
            pubkey: self.client_keypair.public_key_bytes(),
            created_at: fauna_core::data::Timestamp::now_secs() as u64,
            kind: NOSTR_CONNECT_KIND,
            tags: vec![Tag::new(vec!["p".into(), self.signer_pubkey.clone()])],
            content,
        };
        let signed_wrapper = self.client_keypair.sign_event(wrapper);
        // `send`, not `publish`: with a live subscription the next relay
        // message may be the answer itself rather than our OK, so both are
        // handled in the one read loop below.
        self.relay
            .send(&ClientMessage::Event(signed_wrapper))
            .await
            .context("send NIP-46 request")?;

        let answer = tokio::time::timeout(timeout, async {
            loop {
                match self.relay.recv().await? {
                    Some(RelayMessage::Ok {
                        accepted: false,
                        message,
                        ..
                    }) => anyhow::bail!("relay refused the NIP-46 request: {message}"),
                    Some(RelayMessage::Event { event, .. }) => {
                        if event.kind != NOSTR_CONNECT_KIND || event.pubkey != self.signer_pubkey {
                            continue;
                        }
                        let decrypted = match detect_scheme(&event.content) {
                            EncryptionScheme::Nip44 => crate::nip44::nip44_decrypt(
                                &self.client_keypair.secret_bytes(),
                                &self.signer_pk,
                                &event.content,
                            ),
                            EncryptionScheme::Nip04 => crate::nip04::decrypt(
                                &self.client_keypair,
                                &self.signer_pk,
                                &event.content,
                            ),
                        };
                        // An answer we cannot open is not ours (another app's
                        // traffic at the same signer) — keep reading.
                        let Ok(decrypted) = decrypted else { continue };
                        let Ok(response) = serde_json::from_str::<serde_json::Value>(&decrypted)
                        else {
                            continue;
                        };
                        if response["id"] != request_id {
                            continue;
                        }
                        if let Some(error) = response.get("error").and_then(|e| e.as_str()) {
                            anyhow::bail!("signer error: {error}");
                        }
                        let Some(result) = response.get("result") else {
                            anyhow::bail!("signer answered with neither result nor error");
                        };
                        // `result` is a string for every v1 method; a bunker
                        // that returns the signed event as an object is
                        // tolerated by re-serializing it.
                        return Ok(match result.as_str() {
                            Some(s) => s.to_string(),
                            None => result.to_string(),
                        });
                    }
                    None => anyhow::bail!("relay connection closed"),
                    _ => continue,
                }
            }
        })
        .await;

        match answer {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!(
                "NIP-46 {method}: no answer from the bunker"
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Signer side (the nest as the user's bunker) — nostr.md § The nest as the
// user's NIP-46 signer. Pure protocol: parse/build/encrypt only; policy
// (roster membership, rate limits, key access) lives nest-side.
// ---------------------------------------------------------------------------

/// Hard cap on a decrypted NIP-46 request's JSON size. Requests arrive on an
/// unauthenticated relay carve-out; anything larger is refused before parsing.
pub const MAX_REQUEST_JSON_BYTES: usize = 64 * 1024;

/// NIP-46 methods, v1 surface. Unknown methods are preserved (with the request
/// id) so the signer can answer an error instead of silently dropping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Nip46Method {
    Connect,
    GetPublicKey,
    Ping,
    SignEvent,
    Nip44Encrypt,
    Nip44Decrypt,
    Nip04Encrypt,
    Nip04Decrypt,
    Unknown(String),
}

impl Nip46Method {
    pub fn parse(s: &str) -> Self {
        match s {
            "connect" => Self::Connect,
            "get_public_key" => Self::GetPublicKey,
            "ping" => Self::Ping,
            "sign_event" => Self::SignEvent,
            "nip44_encrypt" => Self::Nip44Encrypt,
            "nip44_decrypt" => Self::Nip44Decrypt,
            "nip04_encrypt" => Self::Nip04Encrypt,
            "nip04_decrypt" => Self::Nip04Decrypt,
            other => Self::Unknown(other.to_string()),
        }
    }

    /// The oracle operation class a method is performed under when the
    /// caller is a **third-party principal** rather than an invite-minted
    /// bunker app (TP11 — `key-material-hierarchy.md` § Audience: deployment
    /// infrastructure → *The oracle*, the `nostr.sign_event` / `nostr.nip44`
    /// row). `Ok(None)` for the methods that touch no key (`connect`,
    /// `ping`, `get_public_key` — the npub is public); `Err` for the methods
    /// a principal is never served: the legacy NIP-04 pair (a principal
    /// speaks the current scheme or nothing — the per-method narrowing the
    /// row names) and any unknown method.
    pub fn oracle_class(&self) -> Result<Option<IdentityOpClass>, &'static str> {
        match self {
            Nip46Method::Connect | Nip46Method::GetPublicKey | Nip46Method::Ping => Ok(None),
            Nip46Method::SignEvent => Ok(Some(IdentityOpClass::NostrSignEvent)),
            Nip46Method::Nip44Encrypt | Nip46Method::Nip44Decrypt => {
                Ok(Some(IdentityOpClass::NostrNip44))
            }
            Nip46Method::Nip04Encrypt | Nip46Method::Nip04Decrypt => {
                Err("nip04 is not served to a third-party principal")
            }
            Nip46Method::Unknown(_) => Err("unsupported method"),
        }
    }
}

/// Event kinds the oracle never signs for a principal under
/// `nostr.sign_event` — the Nostr custodian's half of the sovereign deny
/// list (`fauna_core::identity_op::SovereignOp`), stated as code: a signed
/// event of one of these kinds would change *who the user is* to other
/// Nostr clients, or would let the principal act as a signer itself.
///
/// * `1776`, `1777` — NIP-41 key migration (the migration statement and its
///   whitelist): a successor-key announcement is key succession, which only
///   the user's own device may perform
///   (`docs/goal/ui/nostr.md` § Key succession and rotation).
/// * `24133` — NIP-46 itself: a request or response signed by the USER key
///   would present the principal as a remote signer for the user to other
///   apps; the principal is a bunker *client*, never a bunker.
///
/// A hard constant, no configuration surface; a user-minted per-kind
/// allowlist on top of it is the declared refinement the bunker section
/// names, not v1.
pub const ORACLE_SIGN_EVENT_DENIED_KINDS: &[u64] = &[1776, 1777, 24133];

/// Does the oracle sign an event of `kind` for a principal? The complement
/// of [`ORACLE_SIGN_EVENT_DENIED_KINDS`].
#[must_use]
pub fn oracle_sign_event_kind_admitted(kind: u64) -> bool {
    !ORACLE_SIGN_EVENT_DENIED_KINDS.contains(&kind)
}

/// The encryption scheme a request arrived in. Responses always answer in the
/// same scheme (NIP-44 preferred; NIP-04 is the legacy fallback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionScheme {
    Nip44,
    Nip04,
}

/// Detect the scheme of an encrypted kind-24133 content payload. NIP-04
/// ciphertext carries a `?iv=` marker; NIP-44 is a single base64 payload.
pub fn detect_scheme(content: &str) -> EncryptionScheme {
    if content.contains("?iv=") {
        EncryptionScheme::Nip04
    } else {
        EncryptionScheme::Nip44
    }
}

/// A parsed NIP-46 request.
#[derive(Debug, Clone)]
pub struct Nip46Request {
    pub id: String,
    pub method: Nip46Method,
    pub params: Vec<String>,
}

impl Nip46Request {
    /// `connect` params are `[signer-pubkey, ?secret, ?perms]` — the secret is
    /// at index 1. Absent or empty → None.
    pub fn connect_secret(&self) -> Option<&str> {
        self.params
            .get(1)
            .map(String::as_str)
            .filter(|s| !s.is_empty())
    }

    /// `sign_event` params are `[event-json]` — index 0.
    pub fn sign_event_payload(&self) -> Result<&str> {
        self.params
            .first()
            .map(String::as_str)
            .context("sign_event requires params[0] = event JSON")
    }

    /// The `kind` of the event a `sign_event` request asks to sign — read
    /// before anything is signed, so the oracle's kind policy
    /// ([`oracle_sign_event_kind_admitted`]) decides on the request as sent.
    /// A payload that is not JSON, or carries no integer `kind`, is an error
    /// here and is never signed.
    pub fn sign_event_kind(&self) -> Result<u64> {
        let payload = self.sign_event_payload()?;
        let v: serde_json::Value =
            serde_json::from_str(payload).context("sign_event payload is not JSON")?;
        v.get("kind")
            .and_then(serde_json::Value::as_u64)
            .context("sign_event payload missing kind")
    }

    /// `nipXX_encrypt` / `nipXX_decrypt` params are
    /// `[third-party-pubkey, payload]` — pubkey at index 0, 64 lowercase hex.
    pub fn crypt_third_party_pubkey(&self) -> Result<[u8; 32]> {
        let pubkey_hex = self
            .params
            .first()
            .context("encrypt/decrypt requires params[0] = third-party pubkey")?;
        let bytes = hex::decode(pubkey_hex).context("third-party pubkey must be hex")?;
        bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("third-party pubkey must be 32 bytes"))
    }

    /// `nipXX_encrypt` / `nipXX_decrypt` payload — index 1.
    pub fn crypt_payload(&self) -> Result<&str> {
        self.params
            .get(1)
            .map(String::as_str)
            .context("encrypt/decrypt requires params[1] = payload")
    }
}

/// Parse a decrypted NIP-46 request JSON. Strict: string id, string method,
/// params an array of strings (absent → empty).
fn parse_request_json(json: &str) -> Result<Nip46Request> {
    let value: serde_json::Value = serde_json::from_str(json).context("request is not JSON")?;
    let obj = value.as_object().context("request is not a JSON object")?;
    let id = obj
        .get("id")
        .and_then(|i| i.as_str())
        .context("request id must be a string")?
        .to_string();
    let method = obj
        .get("method")
        .and_then(|m| m.as_str())
        .context("request method must be a string")?;
    let params = match obj.get("params") {
        None => Vec::new(),
        Some(p) => p
            .as_array()
            .context("request params must be an array")?
            .iter()
            .map(|e| {
                e.as_str()
                    .map(str::to_string)
                    .context("request params must be strings")
            })
            .collect::<Result<Vec<_>>>()?,
    };
    Ok(Nip46Request {
        id,
        method: Nip46Method::parse(method),
        params,
    })
}

/// Decrypt and parse a kind-24133 request content addressed to the signer.
/// `app_pubkey` is the event author (the app's client pubkey). Enforces
/// [`MAX_REQUEST_JSON_BYTES`] on the decrypted JSON before parsing. Strict
/// JSON-RPC shape: string id, string method, array-of-strings params
/// (params may be absent → empty).
pub fn decrypt_request(
    signer: &Keypair,
    app_pubkey: &[u8; 32],
    content: &str,
) -> Result<(Nip46Request, EncryptionScheme)> {
    let scheme = detect_scheme(content);
    let json = match scheme {
        EncryptionScheme::Nip44 => {
            crate::nip44::nip44_decrypt(&signer.secret_bytes(), app_pubkey, content)
                .context("NIP-44 decrypt request")?
        }
        EncryptionScheme::Nip04 => {
            crate::nip04::decrypt(signer, app_pubkey, content).context("NIP-04 decrypt request")?
        }
    };
    if json.len() > MAX_REQUEST_JSON_BYTES {
        anyhow::bail!(
            "NIP-46 request too large: {} bytes (max {MAX_REQUEST_JSON_BYTES})",
            json.len()
        );
    }
    Ok((parse_request_json(&json)?, scheme))
}

/// Build the response JSON for a request: `{"id", "result"}` on Ok,
/// `{"id", "error"}` on Err. Key order is part of the pinned wire shape.
pub fn build_response_json(id: &str, outcome: std::result::Result<&str, &str>) -> String {
    let id_json = serde_json::Value::from(id);
    match outcome {
        Ok(result) => format!(
            "{{\"id\":{id_json},\"result\":{}}}",
            serde_json::Value::from(result)
        ),
        Err(error) => format!(
            "{{\"id\":{id_json},\"error\":{}}}",
            serde_json::Value::from(error)
        ),
    }
}

/// Encrypt a response JSON to the app in the request's scheme and wrap it in a
/// signed kind-24133 event (p-tag = app pubkey), signed by the signer key.
pub fn build_response_event(
    signer: &Keypair,
    app_pubkey: &[u8; 32],
    scheme: EncryptionScheme,
    response_json: &str,
    created_at: u64,
) -> Result<Event> {
    let content = match scheme {
        EncryptionScheme::Nip44 => {
            crate::nip44::nip44_encrypt(&signer.secret_bytes(), app_pubkey, response_json)
                .context("NIP-44 encrypt response")?
        }
        EncryptionScheme::Nip04 => crate::nip04::encrypt(signer, app_pubkey, response_json)
            .context("NIP-04 encrypt response")?,
    };
    let unsigned = UnsignedEvent {
        pubkey: signer.public_key_bytes(),
        created_at,
        kind: 24133,
        tags: vec![Tag::new(vec!["p".into(), hex::encode(app_pubkey)])],
        content,
    };
    Ok(signer.sign_event(unsigned))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TP11 — the method → class table a principal is served under: the two
    /// key-touching classes, the three keyless methods, and the legacy NIP-04
    /// pair refused (per-method narrowing); every denied kind refused, the
    /// kinds Fauna itself bridges admitted.
    #[test]
    fn oracle_method_table_and_kind_policy() {
        assert_eq!(
            Nip46Method::SignEvent.oracle_class(),
            Ok(Some(IdentityOpClass::NostrSignEvent))
        );
        for m in [Nip46Method::Nip44Encrypt, Nip46Method::Nip44Decrypt] {
            assert_eq!(m.oracle_class(), Ok(Some(IdentityOpClass::NostrNip44)));
        }
        for m in [
            Nip46Method::Connect,
            Nip46Method::GetPublicKey,
            Nip46Method::Ping,
        ] {
            assert_eq!(m.oracle_class(), Ok(None));
        }
        for m in [
            Nip46Method::Nip04Encrypt,
            Nip46Method::Nip04Decrypt,
            Nip46Method::Unknown("sign_everything".into()),
        ] {
            assert!(m.oracle_class().is_err(), "{m:?}");
        }
        for denied in ORACLE_SIGN_EVENT_DENIED_KINDS {
            assert!(!oracle_sign_event_kind_admitted(*denied), "{denied}");
        }
        for admitted in [0, 1, 3, 5, 6, 7, 1059, 10002, crate::nip23::KIND_LONG_FORM] {
            assert!(oracle_sign_event_kind_admitted(admitted), "{admitted}");
        }

        let req = |payload: &str| Nip46Request {
            id: "r".into(),
            method: Nip46Method::SignEvent,
            params: vec![payload.to_string()],
        };
        assert_eq!(
            req(r#"{"kind":1,"content":"x"}"#)
                .sign_event_kind()
                .unwrap(),
            1
        );
        assert!(req(r#"{"content":"x"}"#).sign_event_kind().is_err());
        assert!(req("not json").sign_event_kind().is_err());
        assert!(
            Nip46Request {
                id: "r".into(),
                method: Nip46Method::SignEvent,
                params: vec![],
            }
            .sign_event_kind()
            .is_err()
        );
    }

    #[test]
    fn parse_bunker_url() {
        let url = format!("bunker://{}?relay=wss://relay.example.com", "a".repeat(64));
        let parsed = BunkerUrl::parse(&url).unwrap();
        assert_eq!(parsed.signer_pubkey, "a".repeat(64));
        assert_eq!(parsed.relay_url(), "wss://relay.example.com");
        assert_eq!(parsed.relays, vec!["wss://relay.example.com"]);
        assert_eq!(parsed.secret, None);
        assert_eq!(parsed.without_secret(), url);
    }

    /// The string a spec-current bunker (our own `create_invite` included)
    /// mints: several relays, a one-time secret, percent-encoded values.
    #[test]
    fn parse_bunker_url_with_secret_and_several_relays() {
        let url = format!(
            "bunker://{}?relay=wss%3A%2F%2Fa.example%2Fnostr&relay=wss://b.example&secret=s3cr3t",
            "A".repeat(64)
        );
        let parsed = BunkerUrl::parse(&url).unwrap();
        assert_eq!(
            parsed.signer_pubkey,
            "a".repeat(64),
            "pubkey normalized to lowercase"
        );
        assert_eq!(
            parsed.relays,
            vec!["wss://a.example/nostr", "wss://b.example"]
        );
        assert_eq!(parsed.secret.as_deref(), Some("s3cr3t"));
        assert_eq!(
            parsed.without_secret(),
            format!(
                "bunker://{}?relay=wss://a.example/nostr&relay=wss://b.example",
                "a".repeat(64)
            )
        );
    }

    #[test]
    fn parse_bunker_url_missing_prefix() {
        assert!(BunkerUrl::parse("https://example.com").is_err());
    }

    #[test]
    fn parse_bunker_url_bad_pubkey_length() {
        assert!(BunkerUrl::parse("bunker://short?relay=wss://r.com").is_err());
    }

    #[test]
    fn parse_bunker_url_missing_relay() {
        let url = format!("bunker://{}?other=value", "a".repeat(64));
        assert!(BunkerUrl::parse(&url).is_err());
    }

    /// A pasted bunker string's `relay=` parameters are dialed under the same
    /// guard as every other relay: under the production policy a
    /// loopback relay and a cloud-metadata relay are both refused with no
    /// socket opened — the listener behind the loopback URL never sees a
    /// connection — and the dial fails as a reachability failure.
    #[tokio::test]
    async fn dial_refuses_a_private_relay_before_any_tcp_connect() {
        use crate::relay_client::test_support::ProbeListener;
        let probe = ProbeListener::bind().await;
        let bunker = BunkerUrl {
            signer_pubkey: "a".repeat(64),
            relays: vec![
                "ws://169.254.169.254/latest/meta-data/".to_string(),
                probe.url.clone(),
            ],
            secret: None,
        };
        let err =
            NostrConnectClient::dial(&bunker, Duration::from_secs(5), RelayDialPolicy::PublicOnly)
                .await
                .err()
                .expect("every relay is refused under PublicOnly");
        assert!(
            format!("{err:#}").contains("not permitted"),
            "the failure is the guard's: {err:#}"
        );
        assert!(
            !probe
                .saw_a_connection_within(Duration::from_millis(300))
                .await,
            "the guard must refuse before opening a socket"
        );
    }
}

#[cfg(test)]
mod signer_tests {
    use super::*;

    fn request_json(id: &str, method: &str, params: &[&str]) -> String {
        serde_json::to_string(&serde_json::json!({
            "id": id,
            "method": method,
            "params": params,
        }))
        .unwrap()
    }

    /// App-side encrypt of a request to the signer, in the given scheme.
    fn encrypt_as_app(
        app: &Keypair,
        signer_pubkey: &[u8; 32],
        json: &str,
        scheme: EncryptionScheme,
    ) -> String {
        match scheme {
            EncryptionScheme::Nip44 => {
                crate::nip44::nip44_encrypt(&app.secret_bytes(), signer_pubkey, json).unwrap()
            }
            EncryptionScheme::Nip04 => crate::nip04::encrypt(app, signer_pubkey, json).unwrap(),
        }
    }

    #[test]
    fn scheme_detection() {
        assert_eq!(
            detect_scheme("aGVsbG8=?iv=YWJjZA=="),
            EncryptionScheme::Nip04
        );
        assert_eq!(detect_scheme("AgEC3q2+7w=="), EncryptionScheme::Nip44);
    }

    #[test]
    fn request_roundtrip_nip44() {
        let app = Keypair::generate();
        let signer = Keypair::generate();
        let json = request_json("req1", "ping", &[]);
        let content = encrypt_as_app(
            &app,
            &signer.public_key_bytes(),
            &json,
            EncryptionScheme::Nip44,
        );

        let (req, scheme) = decrypt_request(&signer, &app.public_key_bytes(), &content).unwrap();
        assert_eq!(scheme, EncryptionScheme::Nip44);
        assert_eq!(req.id, "req1");
        assert_eq!(req.method, Nip46Method::Ping);
        assert!(req.params.is_empty());
    }

    #[test]
    fn request_roundtrip_nip04() {
        let app = Keypair::generate();
        let signer = Keypair::generate();
        let json = request_json("req2", "get_public_key", &[]);
        let content = encrypt_as_app(
            &app,
            &signer.public_key_bytes(),
            &json,
            EncryptionScheme::Nip04,
        );

        let (req, scheme) = decrypt_request(&signer, &app.public_key_bytes(), &content).unwrap();
        assert_eq!(scheme, EncryptionScheme::Nip04);
        assert_eq!(req.id, "req2");
        assert_eq!(req.method, Nip46Method::GetPublicKey);
    }

    #[test]
    fn connect_secret_is_param_position_one() {
        let req = Nip46Request {
            id: "c".into(),
            method: Nip46Method::Connect,
            params: vec!["a".repeat(64), "s3cret".into()],
        };
        assert_eq!(req.connect_secret(), Some("s3cret"));

        // Absent or empty secret → None.
        let no_secret = Nip46Request {
            id: "c".into(),
            method: Nip46Method::Connect,
            params: vec!["a".repeat(64)],
        };
        assert_eq!(no_secret.connect_secret(), None);
        let empty_secret = Nip46Request {
            id: "c".into(),
            method: Nip46Method::Connect,
            params: vec!["a".repeat(64), String::new()],
        };
        assert_eq!(empty_secret.connect_secret(), None);
    }

    #[test]
    fn sign_event_payload_is_param_position_zero() {
        let req = Nip46Request {
            id: "s".into(),
            method: Nip46Method::SignEvent,
            params: vec!["{\"kind\":1}".into()],
        };
        assert_eq!(req.sign_event_payload().unwrap(), "{\"kind\":1}");

        let missing = Nip46Request {
            id: "s".into(),
            method: Nip46Method::SignEvent,
            params: vec![],
        };
        assert!(missing.sign_event_payload().is_err());
    }

    #[test]
    fn crypt_params_are_pubkey_then_payload() {
        let third_party = Keypair::generate();
        let req = Nip46Request {
            id: "e".into(),
            method: Nip46Method::Nip44Encrypt,
            params: vec![third_party.public_key_hex(), "hello".into()],
        };
        assert_eq!(
            req.crypt_third_party_pubkey().unwrap(),
            third_party.public_key_bytes()
        );
        assert_eq!(req.crypt_payload().unwrap(), "hello");

        // Bad hex pubkey refused.
        let bad = Nip46Request {
            id: "e".into(),
            method: Nip46Method::Nip44Encrypt,
            params: vec!["zz".repeat(32), "hello".into()],
        };
        assert!(bad.crypt_third_party_pubkey().is_err());
    }

    #[test]
    fn unknown_method_parses_with_id_preserved() {
        let app = Keypair::generate();
        let signer = Keypair::generate();
        let json = request_json("u1", "frobnicate", &["x"]);
        let content = encrypt_as_app(
            &app,
            &signer.public_key_bytes(),
            &json,
            EncryptionScheme::Nip44,
        );

        let (req, _) = decrypt_request(&signer, &app.public_key_bytes(), &content).unwrap();
        assert_eq!(req.id, "u1");
        assert_eq!(req.method, Nip46Method::Unknown("frobnicate".into()));
    }

    #[test]
    fn malformed_json_rpc_refused() {
        let app = Keypair::generate();
        let signer = Keypair::generate();
        for bad in [
            "not json at all",
            "{\"method\":\"ping\",\"params\":[]}", // missing id
            "{\"id\":42,\"method\":\"ping\",\"params\":[]}", // non-string id
            "{\"id\":\"x\",\"params\":[]}",        // missing method
            "{\"id\":\"x\",\"method\":\"ping\",\"params\":\"nope\"}", // params not array
            "{\"id\":\"x\",\"method\":\"ping\",\"params\":[1,2]}", // non-string params
        ] {
            let content = encrypt_as_app(
                &app,
                &signer.public_key_bytes(),
                bad,
                EncryptionScheme::Nip44,
            );
            assert!(
                decrypt_request(&signer, &app.public_key_bytes(), &content).is_err(),
                "should refuse: {bad}"
            );
        }
    }

    #[test]
    fn wrong_key_decrypt_refused() {
        let app = Keypair::generate();
        let signer = Keypair::generate();
        let other = Keypair::generate();
        let json = request_json("w1", "ping", &[]);
        // Encrypted to `other`, not to `signer`.
        let content = encrypt_as_app(
            &app,
            &other.public_key_bytes(),
            &json,
            EncryptionScheme::Nip44,
        );
        assert!(decrypt_request(&signer, &app.public_key_bytes(), &content).is_err());

        let content04 = encrypt_as_app(
            &app,
            &other.public_key_bytes(),
            &json,
            EncryptionScheme::Nip04,
        );
        assert!(decrypt_request(&signer, &app.public_key_bytes(), &content04).is_err());
    }

    #[test]
    fn oversized_request_refused() {
        let app = Keypair::generate();
        let signer = Keypair::generate();
        let big = "x".repeat(MAX_REQUEST_JSON_BYTES + 1);
        let json = request_json("big", "sign_event", &[&big]);

        // NIP-44 can't even carry it: the spec caps plaintext at 65535 bytes,
        // one short of MAX_REQUEST_JSON_BYTES — refusal happens at encrypt.
        assert!(
            crate::nip44::nip44_encrypt(&app.secret_bytes(), &signer.public_key_bytes(), &json)
                .is_err()
        );

        // NIP-04 has no length cap, so the decrypted-JSON cap is what refuses.
        let content = encrypt_as_app(
            &app,
            &signer.public_key_bytes(),
            &json,
            EncryptionScheme::Nip04,
        );
        assert!(decrypt_request(&signer, &app.public_key_bytes(), &content).is_err());
    }

    #[test]
    fn response_json_shapes() {
        assert_eq!(
            build_response_json("r1", Ok("ack")),
            "{\"id\":\"r1\",\"result\":\"ack\"}"
        );
        assert_eq!(
            build_response_json("r1", Err("unauthorized")),
            "{\"id\":\"r1\",\"error\":\"unauthorized\"}"
        );
    }

    #[test]
    fn response_event_roundtrip_both_schemes() {
        let app = Keypair::generate();
        let signer = Keypair::generate();
        let response_json = build_response_json("r2", Ok("pong"));

        for scheme in [EncryptionScheme::Nip44, EncryptionScheme::Nip04] {
            let event = build_response_event(
                &signer,
                &app.public_key_bytes(),
                scheme,
                &response_json,
                1_700_000_000,
            )
            .unwrap();

            assert_eq!(event.kind, 24133);
            assert_eq!(event.pubkey, signer.public_key_hex());
            assert_eq!(event.created_at, 1_700_000_000);
            assert!(crate::signing::verify_event(&event), "response must verify");
            let p_tag = event.tags.iter().find(|t| t.name() == Some("p")).unwrap();
            assert_eq!(p_tag.value(), Some(app.public_key_hex().as_str()));

            // The app can decrypt it in the same scheme.
            let plaintext = match scheme {
                EncryptionScheme::Nip44 => crate::nip44::nip44_decrypt(
                    &app.secret_bytes(),
                    &signer.public_key_bytes(),
                    &event.content,
                )
                .unwrap(),
                EncryptionScheme::Nip04 => {
                    crate::nip04::decrypt(&app, &signer.public_key_bytes(), &event.content).unwrap()
                }
            };
            assert_eq!(plaintext, response_json);
        }
    }
}
