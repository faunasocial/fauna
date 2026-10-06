//! HTTP client for the push relay at push.fauna.social.

use anyhow::Result;
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};

pub struct RelayClient {
    base_url: String,
    http: reqwest::Client,
}

#[derive(Serialize)]
pub struct WakeRequest {
    pub target_actor_id: String,
    pub requester_actor_id: String,
    pub requester_endpoint: String,
    pub timestamp: u64,
    pub signature: String,
}

#[derive(Deserialize)]
pub struct WakeResponse {
    pub nonce: String,
}

#[derive(Deserialize)]
pub struct EndpointResponse {
    pub responder_endpoint: String,
}

/// Domain tag for the wake signature. Mirrors `DOMAIN_WAKE` in
/// `bins/fauna-push-relay/src/api.rs`; the two must stay byte-identical.
const DOMAIN_WAKE: &str = "fauna-push-relay/v1/wake";

/// Canonical signing encoding — the mirror of `signing_bytes` in
/// `bins/fauna-push-relay/src/api.rs` (kept as a second definition on purpose:
/// the relay deliberately depends on neither `fauna-core` nor this crate, so
/// the two ends agree by a shared golden vector rather than a shared symbol).
///
/// Each element, the leading domain tag included, is emitted as an 8-byte
/// big-endian length followed by its UTF-8 bytes. The length prefix is what
/// makes the encoding injective: a bare concatenation of adjacent
/// variable-length fields lets an observer re-split one valid signature into a
/// different field tuple. `signing_bytes_matches_the_relays_golden_vector`
/// pins these bytes against the relay's own test.
fn signing_bytes(elements: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for element in elements {
        out.extend_from_slice(&(element.len() as u64).to_be_bytes());
        out.extend_from_slice(element.as_bytes());
    }
    out
}

impl WakeRequest {
    pub fn new(target: &[u8; 32], requester: &[u8; 32], endpoint: &str) -> Self {
        Self {
            target_actor_id: hex::encode(target),
            requester_actor_id: hex::encode(requester),
            requester_endpoint: endpoint.to_string(),
            timestamp: fauna_core::data::Timestamp::now_secs_or_zero() as u64,
            signature: String::new(),
        }
    }

    pub fn sign(&mut self, signing_key: &SigningKey) {
        let timestamp = self.timestamp.to_string();
        let message = signing_bytes(&[
            DOMAIN_WAKE,
            &self.target_actor_id,
            &self.requester_actor_id,
            &self.requester_endpoint,
            &timestamp,
        ]);
        let sig = signing_key.sign(&message);
        self.signature = hex::encode(sig.to_bytes());
    }
}

impl RelayClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }

    pub async fn wake(
        &self,
        target: &[u8; 32],
        signing_key: &SigningKey,
        our_endpoint: &str,
    ) -> Result<WakeResponse> {
        let requester = signing_key.verifying_key().to_bytes();
        let mut req = WakeRequest::new(target, &requester, our_endpoint);
        req.sign(signing_key);
        let resp = self
            .http
            .post(format!("{}/v1/wake", self.base_url))
            .json(&req)
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("wake failed: {}", resp.status());
        }
        Ok(resp.json().await?)
    }

    pub async fn poll_endpoint(&self, nonce: &str) -> Result<Option<String>> {
        let resp = self
            .http
            .get(format!("{}/v1/endpoint/{nonce}", self.base_url))
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(None);
        }
        if !resp.status().is_success() {
            anyhow::bail!("endpoint poll failed: {}", resp.status());
        }
        let body: EndpointResponse = resp.json().await?;
        Ok(Some(body.responder_endpoint))
    }

    pub async fn report_endpoint(&self, nonce: &str, our_endpoint: &str) -> Result<()> {
        let resp = self
            .http
            .post(format!("{}/v1/endpoint/{nonce}", self.base_url))
            .json(&serde_json::json!({ "responder_endpoint": our_endpoint }))
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("report endpoint failed: {}", resp.status());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_wake_request() {
        let req = WakeRequest::new(&[0xAA; 32], &[0xBB; 32], "1.2.3.4:51820");
        assert_eq!(req.target_actor_id, hex::encode([0xAA; 32]));
        assert_eq!(req.requester_endpoint, "1.2.3.4:51820");
        assert!(!req.target_actor_id.is_empty());
    }

    #[test]
    fn sign_wake_request() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let mut req = WakeRequest::new(
            &[0xAA; 32],
            &key.verifying_key().to_bytes(),
            "1.2.3.4:51820",
        );
        req.sign(&key);
        assert!(!req.signature.is_empty());
        assert_eq!(req.signature.len(), 128); // 64 bytes = 128 hex chars
    }

    /// The relay verifies what this client signs, and the two encoders are
    /// separate definitions (see `signing_bytes`), so this vector is the only
    /// thing keeping them from silently forking. It is copied verbatim from
    /// `signing_bytes_is_the_documented_golden_vector` in
    /// `bins/fauna-push-relay/src/api.rs` — change both or neither.
    #[test]
    fn signing_bytes_matches_the_relays_golden_vector() {
        const GOLDEN: &str = "00000000000000186661756e612d707573682d72656c61792f76312f77616b650000000000000002616100000000000000026262000000000000000d312e322e332e343a353138323000000000000000023432";
        let bytes = signing_bytes(&[DOMAIN_WAKE, "aa", "bb", "1.2.3.4:51820", "42"]);
        assert_eq!(hex::encode(&bytes), GOLDEN);
    }

    /// Distinct field tuples must never share signing bytes. Under the
    /// pre-2026-08-15 bare concatenation, an endpoint of `1.2.3.4:5182` with
    /// timestamp `042` and one of `1.2.3.4:51820` with timestamp `42` produced
    /// the same string, so one observed wake signature verified over both.
    #[test]
    fn a_resplit_of_endpoint_and_timestamp_has_distinct_signing_bytes() {
        let a = signing_bytes(&[DOMAIN_WAKE, "aa", "bb", "1.2.3.4:51820", "42"]);
        let b = signing_bytes(&[DOMAIN_WAKE, "aa", "bb", "1.2.3.4:5182", "042"]);
        assert_eq!(
            "1.2.3.4:51820".to_string() + "42",
            "1.2.3.4:5182".to_string() + "042",
            "the two field tuples must be indistinguishable under plain concatenation, \
             or this test is not pinning the hole it claims to"
        );
        assert_ne!(a, b);
    }
}
