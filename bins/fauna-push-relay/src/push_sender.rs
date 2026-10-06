//! Push notification sender abstraction.

use anyhow::Result;
use p256::pkcs8::DecodePrivateKey;
use serde::{Deserialize, Serialize};

/// Payload sent in a push notification to wake a peer.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WakePayload {
    pub requester_actor_id: String,
    pub nonce: String,
}

/// Platform-agnostic push notification sender.
#[async_trait::async_trait]
pub trait PushSender: Send + Sync {
    async fn send(&self, platform: &str, push_token: &str, payload: &WakePayload) -> Result<()>;
}

/// Mock sender that logs but doesn't send. Used when no credentials are configured.
pub struct LogPushSender;

#[async_trait::async_trait]
impl PushSender for LogPushSender {
    async fn send(&self, platform: &str, _push_token: &str, payload: &WakePayload) -> Result<()> {
        tracing::info!(
            platform,
            requester = %payload.requester_actor_id,
            nonce = %payload.nonce,
            "push notification (log-only, no credentials configured)"
        );
        Ok(())
    }
}

/// APNs push sender using HTTP/2 JWT auth.
pub struct ApnsPushSender {
    client: reqwest::Client,
    key_id: String,
    team_id: String,
    signing_key: p256::ecdsa::SigningKey,
    topic: String,
    base_url: String,
}

impl ApnsPushSender {
    pub fn new(
        key_id: String,
        team_id: String,
        p8_key_path: &str,
        topic: String,
        production: bool,
    ) -> Result<Self> {
        let pem = std::fs::read_to_string(p8_key_path)?;
        let signing_key = parse_p8_key(&pem)?;
        let client = reqwest::Client::builder().http2_prior_knowledge().build()?;
        let base_url = if production {
            "https://api.push.apple.com".to_string()
        } else {
            "https://api.sandbox.push.apple.com".to_string()
        };
        Ok(Self {
            client,
            key_id,
            team_id,
            signing_key,
            topic,
            base_url,
        })
    }

    fn make_jwt(&self) -> Result<String> {
        use p256::ecdsa::{Signature, signature::Signer};

        let header = serde_json::json!({"alg": "ES256", "kid": &self.key_id});
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        let claims = serde_json::json!({"iss": &self.team_id, "iat": now});

        finish_jwt(&header, &claims, |signing_input| {
            let signature: Signature = self.signing_key.sign(signing_input);
            signature.to_bytes().to_vec()
        })
    }
}

/// Base64url-encode `header`/`claims`, join them with `.`, hand the joined
/// bytes to `sign`, then base64url-encode the returned signature and append
/// it — the JWT-building scaffolding for the ES256 APNs token (`make_jwt`),
/// with the signer left to the caller.
fn finish_jwt(
    header: &serde_json::Value,
    claims: &serde_json::Value,
    sign: impl FnOnce(&[u8]) -> Vec<u8>,
) -> Result<String> {
    use base64::Engine;
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;

    let header_b64 = engine.encode(serde_json::to_vec(header)?);
    let claims_b64 = engine.encode(serde_json::to_vec(claims)?);
    let signing_input = format!("{header_b64}.{claims_b64}");

    let sig_b64 = engine.encode(sign(signing_input.as_bytes()));

    Ok(format!("{signing_input}.{sig_b64}"))
}

fn parse_p8_key(pem: &str) -> Result<p256::ecdsa::SigningKey> {
    p256::ecdsa::SigningKey::from_pkcs8_pem(pem).map_err(|e| anyhow::anyhow!("invalid P8 key: {e}"))
}

#[async_trait::async_trait]
impl PushSender for ApnsPushSender {
    async fn send(&self, _platform: &str, push_token: &str, payload: &WakePayload) -> Result<()> {
        let jwt = self.make_jwt()?;
        let url = format!("{}/3/device/{}", self.base_url, push_token);
        let body = serde_json::json!({
            "aps": { "content-available": 1 },
            "fauna": {
                "type": "peer_wake",
                "requester": payload.requester_actor_id,
                "nonce": payload.nonce,
            }
        });
        let resp = self
            .client
            .post(&url)
            .header("authorization", format!("bearer {jwt}"))
            .header("apns-topic", &self.topic)
            .header("apns-push-type", "background")
            .header("apns-priority", "5")
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("APNs error {status}: {text}");
        }
        tracing::info!("APNs push sent");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_p8_key_rejects_invalid_input() {
        assert!(parse_p8_key("not a key").is_err());
        assert!(
            parse_p8_key("-----BEGIN PRIVATE KEY-----\ninvalid\n-----END PRIVATE KEY-----") // gitleaks:allow
                .is_err()
        );
    }

    #[test]
    fn parse_p8_key_round_trips_a_generated_pem() {
        use p256::pkcs8::{EncodePrivateKey, LineEnding};

        let original = p256::ecdsa::SigningKey::random(&mut rand::rngs::OsRng);
        let pem = original.to_pkcs8_pem(LineEnding::LF).unwrap();

        let parsed = parse_p8_key(&pem).unwrap();
        assert_eq!(parsed.to_bytes(), original.to_bytes());
    }

    #[test]
    fn make_jwt_produces_the_expected_three_part_es256_shape() {
        use base64::Engine;
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let signing_key = p256::ecdsa::SigningKey::random(&mut rand::rngs::OsRng);
        let sender = ApnsPushSender {
            client: reqwest::Client::new(),
            key_id: "KEY123".to_string(),
            team_id: "TEAM456".to_string(),
            signing_key,
            topic: "com.example.app".to_string(),
            base_url: "https://api.sandbox.push.apple.com".to_string(),
        };

        let jwt = sender.make_jwt().unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3, "header.claims.signature");

        let header: serde_json::Value =
            serde_json::from_slice(&engine.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["kid"], "KEY123");

        let claims: serde_json::Value =
            serde_json::from_slice(&engine.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["iss"], "TEAM456");
        assert!(claims["iat"].as_u64().is_some());

        // ES256 signatures are fixed 64 bytes (r || s).
        assert_eq!(engine.decode(parts[2]).unwrap().len(), 64);
    }

    #[derive(Clone, Default)]
    struct CapturedLog(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLog {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn log_sender_logs_the_send_but_never_the_push_token() {
        let captured = CapturedLog::default();
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        // tokio::test is single-threaded, so the thread-local default covers the await.
        let _guard = tracing::subscriber::set_default(subscriber);

        let payload = WakePayload {
            requester_actor_id: "abc".into(),
            nonce: "123".into(),
        };
        LogPushSender
            .send("apns", "device-token-abc123", &payload)
            .await
            .unwrap();

        let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert!(
            log.contains("push notification"),
            "send was not logged: {log:?}"
        );
        assert!(
            !log.contains("device-token-abc123"),
            "push token leaked: {log:?}"
        );
    }
}
