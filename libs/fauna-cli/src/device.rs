//! The device shell's half of the principal shape (`apps/cli.md` § The device
//! shell): the DPoP key the session's token is bound to, the holder key grants
//! are wrapped to, the issuer discovery read, and the **typed-code** consent
//! start with its token poll (`authorization-server.md` § Consent → the typed
//! code, RFC 8628, built nest side).
//!
//! **Nothing here ever reaches the model** (architectural rule 6): the keys and
//! tokens live in the shell's process and its credential store; what the LLM
//! sees of the ceremony is the `user_code` the human types elsewhere.
//!
//! **Two addresses per endpoint.** A nest publishes its endpoints at its
//! issuer origin, and a DPoP proof's `htu` must name that published URL; the
//! shell may DIAL another base (the e2e harness's loopback nest), so every
//! request dials `dial_base + path` while proving the published URL — the same
//! split the e2e OAuth client draws between `base` and `htu_origin`.

use std::sync::Mutex;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{Signature, SigningKey};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

/// The grant type of the typed-code poll (RFC 8628 §3.4).
pub const GRANT_TYPE_DEVICE_CODE: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// The form parameter carrying the holder key the principal attests
/// (`third-party.md` § The principal model, rule 2).
pub const HOLDER_X25519_PARAM: &str = "fauna_holder_x25519";
/// The token answer's member naming this principal's row, as hex — what the
/// shell names itself by at the conversations oracle's door
/// (`conversations-oracle.md` § The custodian and the door).
pub const PRINCIPAL_ID_FIELD: &str = "fauna_principal_id";

/// The cli's client identity until the organisation's Client ID Metadata
/// Document is published (`apps/cli.md` § Open decisions 1): the loopback
/// development client, whose document is synthesized from this very string,
/// declaring exactly the catalog's arms.
pub fn development_client_id(scope: &str) -> String {
    let encoded: String = url::form_urlencoded::byte_serialize(scope.as_bytes())
        .collect::<String>()
        .replace('+', "%20");
    format!("http://localhost?scope={encoded}")
}

fn b64u(bytes: &[u8]) -> String {
    B64.encode(bytes)
}

/// The ES256 key a principal's tokens are bound to (RFC 9449).
pub struct DpopKey {
    key: SigningKey,
}

impl DpopKey {
    pub fn generate() -> Self {
        Self {
            key: SigningKey::random(&mut rand::rngs::OsRng),
        }
    }

    /// The private scalar, for the credential store.
    pub fn to_secret_b64(&self) -> String {
        b64u(&self.key.to_bytes())
    }

    pub fn from_secret_b64(raw: &str) -> Option<Self> {
        let bytes = B64.decode(raw).ok()?;
        SigningKey::from_slice(&bytes).ok().map(|key| Self { key })
    }

    fn jwk(&self) -> Value {
        let point = self.key.verifying_key().to_encoded_point(false);
        json!({
            "kty": "EC",
            "crv": "P-256",
            "x": b64u(point.x().expect("uncompressed point")),
            "y": b64u(point.y().expect("uncompressed point")),
        })
    }

    /// A proof over one request: `htm` + the published `htu`, the server's
    /// `nonce` when it named one, and `ath` over `access_token` when the
    /// request presents one (the resource-server shape).
    pub fn proof(
        &self,
        htm: &str,
        htu: &str,
        nonce: Option<&str>,
        access_token: Option<&str>,
    ) -> String {
        let header = json!({ "typ": "dpop+jwt", "alg": "ES256", "jwk": self.jwk() });
        let mut claims = json!({
            "jti": b64u(&rand::random::<[u8; 16]>()),
            "htm": htm,
            "htu": htu,
            "iat": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        });
        if let Some(nonce) = nonce {
            claims["nonce"] = json!(nonce);
        }
        if let Some(token) = access_token {
            claims["ath"] = json!(b64u(&Sha256::digest(token.as_bytes())));
        }
        let signing_input = format!(
            "{}.{}",
            b64u(header.to_string().as_bytes()),
            b64u(claims.to_string().as_bytes())
        );
        let signature: Signature = self.key.sign(signing_input.as_bytes());
        format!("{signing_input}.{}", b64u(&signature.to_bytes()))
    }
}

/// The X25519 key the user's grants are wrapped to — attested at the consent
/// start, held by the shell, never by the model.
pub struct HolderKey {
    secret: x25519_dalek::StaticSecret,
}

impl HolderKey {
    pub fn generate() -> Self {
        Self {
            secret: x25519_dalek::StaticSecret::random_from_rng(rand::rngs::OsRng),
        }
    }

    pub fn to_secret_b64(&self) -> String {
        b64u(self.secret.as_bytes())
    }

    pub fn from_secret_b64(raw: &str) -> Option<Self> {
        let bytes: [u8; 32] = B64.decode(raw).ok()?.try_into().ok()?;
        Some(Self {
            secret: x25519_dalek::StaticSecret::from(bytes),
        })
    }

    pub fn public_b64(&self) -> String {
        b64u(x25519_dalek::PublicKey::from(&self.secret).as_bytes())
    }

    /// The public key, as the consent attested it.
    pub fn public(&self) -> [u8; 32] {
        x25519_dalek::PublicKey::from(&self.secret).to_bytes()
    }

    /// The secret, borrowed for the conversations door's possession proof
    /// ([`crate::door`]) — never copied out of the key.
    pub(crate) fn secret(&self) -> &[u8; 32] {
        self.secret.as_bytes()
    }
}

/// What the issuer's RFC 8414 document tells the shell.
#[derive(Debug, Clone)]
pub struct Issuer {
    /// Where requests are dialled.
    pub dial_base: String,
    /// The issuer identifier — the origin every published URL hangs off.
    pub issuer: String,
    pub device_authorization_endpoint: String,
    pub token_endpoint: String,
}

#[derive(Deserialize)]
struct DiscoveryDocument {
    issuer: String,
    device_authorization_endpoint: Option<String>,
    token_endpoint: String,
}

impl Issuer {
    /// Read `/.well-known/oauth-authorization-server` at `dial_base`.
    pub async fn discover(http: &reqwest::Client, dial_base: &str) -> Result<Self, String> {
        let dial_base = dial_base.trim_end_matches('/').to_string();
        let url = format!("{dial_base}/.well-known/oauth-authorization-server");
        let doc: DiscoveryDocument = http
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("could not reach the nest at {dial_base}: {e}"))?
            .error_for_status()
            .map_err(|e| format!("the nest at {dial_base} has no sign-in service yet: {e}"))?
            .json()
            .await
            .map_err(|e| format!("the nest's sign-in document did not parse: {e}"))?;
        Ok(Self {
            dial_base,
            issuer: doc.issuer.trim_end_matches('/').to_string(),
            device_authorization_endpoint: doc.device_authorization_endpoint.ok_or_else(|| {
                "the nest does not offer the typed-code start this app connects with".to_string()
            })?,
            token_endpoint: doc.token_endpoint,
        })
    }

    /// The dial address of a published URL: its path on [`Self::dial_base`].
    pub fn dial(&self, published: &str) -> String {
        let path = match published.strip_prefix(&self.issuer) {
            Some(path) => path.to_string(),
            None => url::Url::parse(published)
                .map(|u| u.path().to_string())
                .unwrap_or_default(),
        };
        format!("{}{path}", self.dial_base)
    }

    /// The published URL of `path` on this nest — a proof's `htu` for a
    /// request the issuer's document does not list (the principal session).
    pub fn published(&self, path: &str) -> String {
        format!("{}{path}", self.issuer)
    }
}

/// What the typed-code start answers.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceStart {
    pub device_code: String,
    pub user_code: String,
    pub expires_in: u64,
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_interval() -> u64 {
    5
}

/// A token reply.
#[derive(Debug, Clone, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    /// The token's subject — for a grant of Fauna-family scopes only, the
    /// account's hex actor id (the nest's `grant_subject`), which
    /// `nest://profile` reads.
    #[serde(default)]
    pub sub: Option<String>,
    /// This principal's row id, hex ([`PRINCIPAL_ID_FIELD`]).
    #[serde(default, rename = "fauna_principal_id")]
    pub principal_id: Option<String>,
}

/// One poll's answer.
#[derive(Debug)]
pub enum Poll {
    /// `authorization_pending`: ask again after the interval.
    Pending,
    /// `slow_down`: ask again, five seconds slower (RFC 8628 §3.5).
    SlowDown,
    Tokens(Tokens),
    /// Anything else ends the ceremony: the user declined, the code expired.
    Refused(String),
}

/// The typed-code ceremony against one issuer, under one DPoP key.
pub struct DeviceClient {
    http: reqwest::Client,
    issuer: Issuer,
    client_id: String,
    dpop: DpopKey,
    nonce: Mutex<Option<String>>,
}

impl DeviceClient {
    pub fn new(http: reqwest::Client, issuer: Issuer, client_id: String, dpop: DpopKey) -> Self {
        Self {
            http,
            issuer,
            client_id,
            dpop,
            nonce: Mutex::new(None),
        }
    }

    pub fn issuer(&self) -> &Issuer {
        &self.issuer
    }

    pub fn dpop(&self) -> &DpopKey {
        &self.dpop
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// The nonce the issuer last handed out — a principal-session proof binds it.
    pub fn nonce(&self) -> Option<String> {
        self.nonce.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_nonce(&self, nonce: Option<String>) {
        *self.nonce.lock().unwrap_or_else(|e| e.into_inner()) = nonce;
    }

    /// One form POST with the protocol's one `use_dpop_nonce` retry. Every
    /// response's nonce is harvested, success included.
    async fn post_form(
        &self,
        published: &str,
        form: &[(&str, &str)],
    ) -> Result<(u16, Value), String> {
        let send = |nonce: Option<String>| {
            let proof = self.dpop.proof("POST", published, nonce.as_deref(), None);
            self.http
                .post(self.issuer.dial(published))
                .header("DPoP", proof)
                .form(form)
                .send()
        };
        let mut response = send(self.nonce())
            .await
            .map_err(|e| format!("the nest did not answer: {e}"))?;
        let mut status = response.status().as_u16();
        let mut nonce = header_nonce(&response);
        let mut body: Value = response.json().await.unwrap_or(Value::Null);
        if status != 200
            && matches!(
                body.get("error").and_then(Value::as_str),
                Some("use_dpop_nonce" | "invalid_dpop_proof")
            )
            && nonce.is_some()
        {
            response = send(nonce.clone())
                .await
                .map_err(|e| format!("the nest did not answer: {e}"))?;
            status = response.status().as_u16();
            nonce = header_nonce(&response).or(nonce);
            body = response.json().await.unwrap_or(Value::Null);
        }
        if nonce.is_some() {
            self.set_nonce(nonce);
        }
        Ok((status, body))
    }

    /// `POST /oauth/device_authorization` for `scope`, attesting `holder`.
    pub async fn start(&self, scope: &str, holder: &HolderKey) -> Result<DeviceStart, String> {
        let holder_pub = holder.public_b64();
        let (status, body) = self
            .post_form(
                &self.issuer.device_authorization_endpoint,
                &[
                    ("client_id", &self.client_id),
                    ("scope", scope),
                    (HOLDER_X25519_PARAM, &holder_pub),
                ],
            )
            .await?;
        if status != 200 {
            return Err(format!(
                "the nest refused to start connecting: {}",
                refusal(&body)
            ));
        }
        serde_json::from_value(body).map_err(|e| format!("the start's answer did not parse: {e}"))
    }

    /// One poll of the token endpoint with the device code.
    pub async fn poll(&self, device_code: &str) -> Result<Poll, String> {
        let (status, body) = self
            .post_form(
                &self.issuer.token_endpoint,
                &[
                    ("grant_type", GRANT_TYPE_DEVICE_CODE),
                    ("device_code", device_code),
                    ("client_id", &self.client_id),
                ],
            )
            .await?;
        if status == 200 {
            return serde_json::from_value(body)
                .map(Poll::Tokens)
                .map_err(|e| format!("the token answer did not parse: {e}"));
        }
        Ok(match body.get("error").and_then(Value::as_str) {
            Some("authorization_pending") => Poll::Pending,
            Some("slow_down") => Poll::SlowDown,
            _ => Poll::Refused(refusal(&body)),
        })
    }

    /// Rotate the token pair with `refresh_token`.
    pub async fn refresh(&self, refresh_token: &str) -> Result<Tokens, String> {
        let (status, body) = self
            .post_form(
                &self.issuer.token_endpoint,
                &[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", refresh_token),
                    ("client_id", &self.client_id),
                ],
            )
            .await?;
        if status != 200 {
            return Err(format!(
                "the nest refused to renew the connection: {}",
                refusal(&body)
            ));
        }
        serde_json::from_value(body).map_err(|e| format!("the token answer did not parse: {e}"))
    }
}

fn header_nonce(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get("dpop-nonce")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

fn refusal(body: &Value) -> String {
    let error = body.get("error").and_then(Value::as_str).unwrap_or("error");
    match body.get("error_description").and_then(Value::as_str) {
        Some(why) => format!("{error}: {why}"),
        None => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::VerifyingKey;
    use p256::ecdsa::signature::Verifier as _;

    fn decode(part: &str) -> Value {
        serde_json::from_slice(&B64.decode(part).unwrap()).unwrap()
    }

    #[test]
    fn a_proof_verifies_under_its_own_jwk_and_carries_the_request() {
        let key = DpopKey::generate();
        let proof = key.proof(
            "GET",
            "https://nest.example/api/v1/principal/ws",
            Some("n1"),
            Some("tok"),
        );
        let parts: Vec<&str> = proof.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header = decode(parts[0]);
        let claims = decode(parts[1]);
        assert_eq!(header["typ"], "dpop+jwt");
        assert_eq!(header["alg"], "ES256");
        assert_eq!(claims["htm"], "GET");
        assert_eq!(claims["htu"], "https://nest.example/api/v1/principal/ws");
        assert_eq!(claims["nonce"], "n1");
        assert_eq!(claims["ath"], b64u(&Sha256::digest(b"tok")));

        let x = B64.decode(header["jwk"]["x"].as_str().unwrap()).unwrap();
        let y = B64.decode(header["jwk"]["y"].as_str().unwrap()).unwrap();
        let mut sec1 = vec![4u8];
        sec1.extend_from_slice(&x);
        sec1.extend_from_slice(&y);
        let verifying = VerifyingKey::from_sec1_bytes(&sec1).unwrap();
        let signature = Signature::from_slice(&B64.decode(parts[2]).unwrap()).unwrap();
        verifying
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
            .expect("the proof verifies under the key it names");
    }

    #[test]
    fn an_authorization_server_proof_carries_no_ath() {
        let proof = DpopKey::generate().proof("POST", "https://n.example/oauth/token", None, None);
        let claims = decode(proof.split('.').nth(1).unwrap());
        assert!(claims.get("ath").is_none() && claims.get("nonce").is_none());
    }

    #[test]
    fn keys_survive_the_credential_store_round_trip() {
        let dpop = DpopKey::generate();
        let back = DpopKey::from_secret_b64(&dpop.to_secret_b64()).unwrap();
        assert_eq!(dpop.jwk(), back.jwk());
        let holder = HolderKey::generate();
        let back = HolderKey::from_secret_b64(&holder.to_secret_b64()).unwrap();
        assert_eq!(holder.public_b64(), back.public_b64());
    }

    #[test]
    fn the_development_client_declares_the_scope_in_its_own_id() {
        assert_eq!(
            development_client_id("fauna:feed:read fauna:identity:op:post.author"),
            "http://localhost?scope=fauna%3Afeed%3Aread%20fauna%3Aidentity%3Aop%3Apost.author"
        );
    }

    #[test]
    fn a_published_url_is_dialled_on_the_dial_base() {
        let issuer = Issuer {
            dial_base: "http://127.0.0.1:9000".into(),
            issuer: "https://alice.example".into(),
            device_authorization_endpoint: "https://alice.example/oauth/device_authorization"
                .into(),
            token_endpoint: "https://alice.example/oauth/token".into(),
        };
        assert_eq!(
            issuer.dial(&issuer.token_endpoint),
            "http://127.0.0.1:9000/oauth/token"
        );
        assert_eq!(
            issuer.published("/api/v1/principal/ws"),
            "https://alice.example/api/v1/principal/ws"
        );
    }
}
