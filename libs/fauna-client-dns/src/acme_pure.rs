//! Pure-Rust (RustCrypto) ACME v2 client crypto — the **wasm-safe** twin of the
//! `instant-acme`-backed [`acme_order`](crate::acme_order) (D2,
//! `tls-certificates.md` § C).
//!
//! `instant-acme` hard-wires its account-key JWS to `ring`/`aws-lc-rs` and its CSR
//! to `rcgen`, none of which build for `wasm32-unknown-unknown`, so that native
//! order core is `#[cfg(not(target_arch = "wasm32"))]`. This module is its **wasm
//! twin** so **web issues certs natively too** — it re-implements the same ACME v2
//! primitives on RustCrypto
//! (`p256`/`ecdsa` for the ES256 account-key JWS, `sha2` for the RFC-7638 JWK
//! thumbprint and the DNS-01 key-authorization digest, `x509-cert` for the PKCS#10
//! CSR) so the order can run in the browser. The HTTP protocol flow (directory →
//! nonce → newAccount → newOrder → authz → challenge → finalize → fetch) lands in
//! a later slice (W3 (account-data-plane.md § Workstreams)) on top of these primitives; this slice (W1) is the CA-free
//! crypto core, fully unit-tested without a live CA.
//!
//! **Not `cfg`-gated** — it compiles on *both* targets (`reqwest` + RustCrypto are
//! cross-target). Production uses it only on wasm (native keeps `instant-acme`, no
//! regression — `tls-certificates.md` § Implementation status); compiling on native
//! is what lets the pebble real-wire test (W6) prove it against a real CA without a
//! headless browser.
//!
//! **Account-credential interop (D6).** The serialized [`AccountCredentials`] is
//! byte-compatible with `instant_acme::AccountCredentials` (0.7.2 `types.rs`): the
//! ACME account is persisted BackupKey-sealed in `DnsConfig.acme_account` and
//! **reused across the admin's devices** (`tls-certificates.md` § C.3), so whichever
//! device (native `instant-acme` or wasm `acme_pure`) created the account, the other
//! must restore it. Same `{id, key_pkcs8, directory}` shape, same base64url
//! PKCS#8 key encoding.

// W1 crypto-core imports (always compiled, both targets). The W3 order-flow's own
// imports live inside `mod order` (gated), so a plain native build that excludes
// the order flow carries no unused imports.
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use p256::ecdsa::SigningKey;
use p256::ecdsa::signature::Signer;
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Failure modes of the pure-Rust ACME crypto. (The HTTP/order errors join in W3;
/// W1 surfaces only the key/CSR primitives.)
#[derive(Debug, thiserror::Error)]
pub enum AcmePureError {
    /// Loading, generating, or encoding the P-256 account/leaf key failed.
    #[error("ACME key: {0}")]
    Key(String),
    /// Assembling or signing the PKCS#10 CSR failed.
    #[error("certificate signing request: {0}")]
    Csr(String),
}

/// The ACME directory URLs we use (a subset of the full directory document).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryUrls {
    #[serde(rename = "newNonce")]
    pub new_nonce: String,
    #[serde(rename = "newAccount")]
    pub new_account: String,
    #[serde(rename = "newOrder")]
    pub new_order: String,
    #[serde(
        rename = "revokeCert",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub revoke_cert: Option<String>,
}

/// Persisted ACME account credentials — **byte-interoperable** with
/// `instant_acme::AccountCredentials` (D6, `tls-certificates.md` § C.3). Stored as
/// the `DnsConfig.acme_account` blob (`serde_json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountCredentials {
    /// The account URL — the JWS `kid` for every authenticated ACME request.
    pub id: String,
    /// PKCS#8 DER of the P-256 account key, base64url-no-pad (the exact field
    /// `instant-acme` writes via its `pkcs8_serde`).
    #[serde(with = "pkcs8_b64url")]
    pub key_pkcs8: Vec<u8>,
    /// The ACME directory URL — written by both `acme_pure` and the pinned
    /// `instant-acme` (pinned by `acme_order.rs`'s round-trip tests); on restore
    /// the directory is re-fetched from it.
    pub directory: String,
}

/// base64url-no-pad (de)serialization of the PKCS#8 key bytes — matches
/// `instant_acme`'s `pkcs8_serde` exactly, so the blob round-trips between the two
/// implementations.
mod pkcs8_b64url {
    use super::B64URL;
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(key_pkcs8: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&B64URL.encode(key_pkcs8))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(d)?;
        B64URL
            .decode(encoded.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}

/// A P-256 (ES256) ACME **account** key — the JWS signing identity for the account.
/// RustCrypto throughout (no `ring`), so it builds for wasm.
pub struct AccountKey {
    signing: SigningKey,
}

impl AccountKey {
    /// Generate a fresh account key from the OS CSPRNG. Uses `p256`'s own
    /// (rand_core 0.6) `OsRng` — the version `ecdsa::SigningKey::random` binds —
    /// rather than the rand_core 0.9 `OsRng` fauna-mls's HPKE uses, so the
    /// `CryptoRngCore` bound resolves. The wasm backend is the `getrandom 0.2`/`js`
    /// Cargo entry.
    pub fn generate() -> Self {
        Self {
            signing: SigningKey::random(&mut p256::elliptic_curve::rand_core::OsRng),
        }
    }

    /// Restore from PKCS#8 DER (the [`AccountCredentials::key_pkcs8`] field — which
    /// a native `instant-acme`/ring device may have written, D6).
    pub fn from_pkcs8_der(der: &[u8]) -> Result<Self, AcmePureError> {
        let signing = SigningKey::from_pkcs8_der(der)
            .map_err(|e| AcmePureError::Key(format!("parse PKCS#8: {e}")))?;
        Ok(Self { signing })
    }

    /// PKCS#8 DER of this key for persistence ([`AccountCredentials::key_pkcs8`]).
    /// RustCrypto's encoding embeds the public key in the inner SEC1 `ECPrivateKey`,
    /// so the blob is restorable by both `acme_pure` and `instant-acme`/ring (D6).
    pub fn to_pkcs8_der(&self) -> Result<Vec<u8>, AcmePureError> {
        Ok(self
            .signing
            .to_pkcs8_der()
            .map_err(|e| AcmePureError::Key(format!("encode PKCS#8: {e}")))?
            .as_bytes()
            .to_vec())
    }

    /// The account public key as a JOSE JWK value
    /// (`{"crv":"P-256","kty":"EC","x":…,"y":…}`), members in RFC-7638 lexicographic
    /// order so serializing it compactly IS the thumbprint input. Used as the
    /// `jwk` member of the protected header on the `newAccount` request.
    pub fn jwk_json(&self) -> serde_json::Value {
        let jwk = self.public_jwk();
        serde_json::json!({ "crv": jwk.crv, "kty": jwk.kty, "x": jwk.x, "y": jwk.y })
    }

    /// RFC-7638 JWK thumbprint: `base64url(sha256(canonical-jwk-json))`. 43 chars.
    pub fn thumbprint(&self) -> String {
        // serde_json compact output of the canonical-order struct = the RFC-7638
        // input: `{"crv":"P-256","kty":"EC","x":"…","y":"…"}`, no whitespace.
        let json = serde_json::to_vec(&self.public_jwk()).expect("JWK serializes");
        B64URL.encode(Sha256::digest(&json))
    }

    /// The ACME key authorization for a challenge `token` (RFC 8555 §8.1):
    /// `token || "." || thumbprint`.
    pub fn key_authorization(&self, token: &str) -> String {
        format!("{token}.{}", self.thumbprint())
    }

    /// The DNS-01 `_acme-challenge` TXT value (RFC 8555 §8.4):
    /// `base64url(sha256(key_authorization))`. 43 chars.
    pub fn dns_value(&self, token: &str) -> String {
        B64URL.encode(Sha256::digest(self.key_authorization(token).as_bytes()))
    }

    /// Sign a **flattened JWS** for an ACME POST. `protected_json` is the
    /// already-serialized JOSE protected header (carrying `alg`/`nonce`/`url` and
    /// either `jwk` or `kid`); `payload` is the raw request body (empty for a
    /// POST-as-GET). The ES256 signature is the raw P1363 `r‖s` (64 bytes,
    /// base64url) — NOT the DER form. Returns the flattened-JSON JWS object to POST.
    pub fn sign_jws(&self, protected_json: &[u8], payload: &[u8]) -> serde_json::Value {
        let protected_b64 = B64URL.encode(protected_json);
        let payload_b64 = B64URL.encode(payload);
        let signing_input = format!("{protected_b64}.{payload_b64}");
        // `SigningKey::sign` is ECDSA/P-256 over SHA-256 (deterministic, RFC 6979).
        let sig: p256::ecdsa::Signature = self.signing.sign(signing_input.as_bytes());
        let sig_b64 = B64URL.encode(sig.to_bytes());
        serde_json::json!({
            "protected": protected_b64,
            "payload": payload_b64,
            "signature": sig_b64,
        })
    }

    /// The uncompressed public-key coordinates as base64url field elements.
    fn public_jwk(&self) -> Jwk {
        use p256::elliptic_curve::sec1::ToEncodedPoint;
        let public = p256::PublicKey::from(self.signing.verifying_key());
        let point = public.to_encoded_point(false); // 0x04 ‖ x(32) ‖ y(32)
        Jwk {
            crv: "P-256",
            kty: "EC",
            x: B64URL.encode(point.x().expect("uncompressed point has x")),
            y: B64URL.encode(point.y().expect("uncompressed point has y")),
        }
    }
}

/// JWK in RFC-7638 canonical member order (`crv`, `kty`, `x`, `y`). Private —
/// the public surface is [`AccountKey::jwk_json`] / [`AccountKey::thumbprint`].
#[derive(Serialize)]
struct Jwk {
    crv: &'static str,
    kty: &'static str,
    x: String,
    y: String,
}

/// Generate a fresh P-256 **leaf** keypair and a multi-SAN PKCS#10 CSR for the
/// `domains`, ECDSA/SHA-256-signed (the wasm-safe twin of the native order's
/// `rcgen` finalize step). Returns `(csr_der, leaf_privkey_pem)`: the DER CSR for
/// the ACME `finalize`, and the leaf private key as PKCS#8 PEM for the issued
/// `TlsCertBundle`. The subject DN is empty — Let's Encrypt ignores the CN and
/// validates the SANs (RFC 8555 §7.4).
pub fn build_csr(domains: &[String]) -> Result<(Vec<u8>, String), AcmePureError> {
    use der::{Encode, asn1::Ia5String};
    use x509_cert::builder::{Builder, RequestBuilder};
    use x509_cert::ext::pkix::{SubjectAltName, name::GeneralName};
    use x509_cert::name::Name;

    let leaf = SigningKey::random(&mut p256::elliptic_curve::rand_core::OsRng);

    let subject = Name::default(); // empty DN — SANs carry identity (LE ignores CN)
    let mut builder = RequestBuilder::new(subject, &leaf)
        .map_err(|e| AcmePureError::Csr(format!("init CSR builder: {e}")))?;

    let names = domains
        .iter()
        .map(|d| {
            Ia5String::new(d)
                .map(GeneralName::DnsName)
                .map_err(|e| AcmePureError::Csr(format!("SAN {d}: {e}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    builder
        .add_extension(&SubjectAltName(names))
        .map_err(|e| AcmePureError::Csr(format!("add SAN extension: {e}")))?;

    let csr = builder
        .build::<p256::ecdsa::DerSignature>()
        .map_err(|e| AcmePureError::Csr(format!("sign CSR: {e}")))?;
    let csr_der = csr
        .to_der()
        .map_err(|e| AcmePureError::Csr(format!("encode CSR DER: {e}")))?;

    let privkey_der = leaf
        .to_pkcs8_der()
        .map_err(|e| AcmePureError::Csr(format!("encode leaf key: {e}")))?;
    let privkey_pem = pkcs8_der_to_pem(privkey_der.as_bytes());

    Ok((csr_der, privkey_pem))
}

/// Wrap PKCS#8 DER as a `PRIVATE KEY` PEM (standard base64, 64-char lines).
/// The line-wrap itself is `fauna_core::mime_wrap::base64_wrap` — hand-rolling
/// only the armor here avoids enabling `p256`'s `pem` feature workspace-wide
/// for one call.
fn pkcs8_der_to_pem(der: &[u8]) -> String {
    let body = fauna_core::mime_wrap::base64_wrap(der, 64, "\n");
    format!("-----BEGIN PRIVATE KEY-----\n{body}-----END PRIVATE KEY-----\n")
}

/// The ACME v2 **HTTP/order flow** (W3) — the wasm-safe order driver built on the
/// W1 crypto primitives above.
///
/// Gated `#[cfg(any(target_arch = "wasm32", feature = "test-helpers"))]`: in
/// production it runs **only on wasm** (web cert issuance — native keeps the
/// instant-acme [`acme_order`](crate::acme_order) driver, no regression); on native
/// it is compiled solely for the pebble real-wire proof (`test-helpers`, W6). The
/// W1 crypto core above is unconditional (both targets, with its own CA-free unit
/// tests). `lib.rs` re-exports `order`'s entry points per target — production names
/// (`begin_dns01_order` / `obtain_certificate_dns01` / `complete_dns01_order` /
/// `Dns01OrderInProgress`) on wasm, `pure_*` aliases of the `_with_http` proof
/// entries on native.
#[cfg(any(target_arch = "wasm32", feature = "test-helpers"))]
mod order {
    use std::time::Duration;

    use fauna_core::data::DnsZoneRef;
    use fauna_core::secret::SecretString;
    use fauna_provisioning::proxy::{BuildEnv, DEFAULT_PROXY_ROOT, current_build_env};

    // The W1 crypto core (`AccountKey`, `AccountCredentials`, `DirectoryUrls`,
    // `build_csr`, `B64URL`, …) from the parent module.
    use super::*;
    use crate::DnsProviderSeam;
    use crate::acme_shared::{
        Dns01Challenge, Dns01Error, Dns01Issued, Dns01OrderConfig, with_published_challenges,
    };
    use crate::acme_shared::{Dns01ResolvabilityProbe, PropagationGate};

    // ───────────────────────────────────────────────────────────────────────
    // ACME v2 HTTP protocol flow (W3) — the wasm-safe order driver.
    // ───────────────────────────────────────────────────────────────────────
    //
    // The pure-Rust twin of [`acme_order`](crate::acme_order)'s instant-acme driver:
    // the same ACME v2 dance (directory → nonce → newAccount → newOrder → authz →
    // challenge-ready → poll → finalize → fetch) over `reqwest` + the W1 RustCrypto
    // JWS/CSR primitives above, so it runs in the browser. It shares the order data
    // types and the publish/teardown choreography with the native driver via
    // [`acme_shared`](crate::acme_shared) ([`with_published_challenges`]); only the
    // order-driving here is target-specific. `lib.rs` re-exports the entry points
    // ([`begin_dns01_order`]/[`obtain_certificate_dns01`]/[`complete_dns01_order`] +
    // [`Dns01OrderInProgress`]) under the same names the native driver exposes, per
    // target, so the cross-target `DnsManagementMachine::issue_cert` orchestration
    // calls one symbol on both.
    //
    // **Proxy (web).** Browsers cannot reach Let's Encrypt's ACME endpoints
    // cross-origin (no CORS), so on wasm every request URL — the directory, the
    // newNonce, and the absolute follow-up URLs the CA returns (account, order, authz,
    // challenge, finalize, certificate) — is re-pointed at the credential-blind
    // `services/fauna-cors-proxy` before the fetch ([`rewrite_acme_url`]). Requests are
    // JWS-signed end-to-end, so the proxy stays credential-blind. The JWS `url`
    // protected-header field always carries the **real** LE URL (what the CA validates
    // against), never the proxy URL — the proxy forwards transparently. Native passes
    // every URL through unchanged.

    /// Let's Encrypt ACME host base → the `services/fauna-cors-proxy` path prefix that
    /// forwards to it. **Must** match the proxy's `BASE_URLS` (`acme-le`,
    /// `acme-le-staging`) — the proxy maps `/<prefix>/<path>` back to `<base>/<path>`,
    /// so re-pointing `<base><rest>` → `<root>/<prefix><rest>` round-trips exactly
    /// (`acme-v02.api.letsencrypt.org` is also [`crate::acme_shared::LETS_ENCRYPT_PRODUCTION`]
    /// minus the `/directory` path).
    const ACME_PROXY_PREFIXES: &[(&str, &str)] = &[
        ("https://acme-v02.api.letsencrypt.org", "acme-le"),
        (
            "https://acme-staging-v02.api.letsencrypt.org",
            "acme-le-staging",
        ),
    ];

    /// Re-point an absolute ACME URL at the credential-blind CORS proxy on web; pass
    /// it through unchanged on native. Production wraps [`current_build_env`] +
    /// [`DEFAULT_PROXY_ROOT`]; the inner [`rewrite_acme_url_for`] is the pure,
    /// test-exercisable core.
    fn rewrite_acme_url(url: &str) -> String {
        rewrite_acme_url_for(url, current_build_env(), DEFAULT_PROXY_ROOT)
    }

    /// Pure URL rewriter (env + proxy root injected for tests). On `Web`, an absolute
    /// Let's Encrypt URL `<base><rest>` becomes `<proxy_root>/<prefix><rest>` (the
    /// `rest` keeps its leading `/`); a non-LE host (a self-hosted ACME, or pebble in
    /// the native test) is passed through. On `Native`, every URL passes through.
    fn rewrite_acme_url_for(url: &str, env: BuildEnv, proxy_root: &str) -> String {
        match env {
            BuildEnv::Native => url.to_string(),
            BuildEnv::Web => {
                let root = proxy_root.trim_end_matches('/');
                for (base, prefix) in ACME_PROXY_PREFIXES {
                    if let Some(rest) = url.strip_prefix(base) {
                        return format!("{root}/{prefix}{rest}");
                    }
                }
                url.to_string()
            }
        }
    }

    /// Cross-target async sleep for the propagation wait + poll loops. The
    /// `#[cfg]` split this used to carry — and the sibling copies in
    /// `acme_shared` / the onboarding machines it used to point at — now live
    /// once, in `fauna-sleep`.
    async fn sleep(d: Duration) {
        fauna_sleep::sleep(d).await;
    }

    /// Which JWS authentication form the protected header carries: the embedded public
    /// JWK (only the `newAccount` request, before an account URL exists) or the account
    /// URL `kid` (every other authenticated ACME request — RFC 8555 §6.2).
    enum JwsAuth<'a> {
        Jwk,
        Kid(&'a str),
    }

    /// A minimal ACME HTTP client: a `reqwest` client plus the rolling replay nonce
    /// (RFC 8555 §6.5 — each authenticated POST consumes a nonce and every response
    /// hands back a fresh one). Lazily fetches a nonce from `newNonce` when the cache
    /// is empty (mirrors instant-acme's on-demand refresh), so the order-driving code
    /// never threads nonces by hand.
    struct AcmeHttp {
        client: reqwest::Client,
        new_nonce_url: String,
        nonce: Option<String>,
    }

    impl AcmeHttp {
        /// Pop the cached replay nonce, or fetch a fresh one from `newNonce`. RFC 8555
        /// §7.2: a GET to `newNonce` returns 204 with a `Replay-Nonce` header.
        async fn nonce(&mut self) -> Result<String, Dns01Error> {
            if let Some(n) = self.nonce.take() {
                return Ok(n);
            }
            let resp = self
                .client
                .get(rewrite_acme_url(&self.new_nonce_url))
                .send()
                .await
                .map_err(|e| Dns01Error::Ca(format!("fetch newNonce: {e}")))?;
            replay_nonce(&resp)
                .ok_or_else(|| Dns01Error::Ca("no Replay-Nonce in newNonce response".to_string()))
        }

        /// Sign and POST a flattened JWS to `url` (the real CA URL — rewritten to the
        /// proxy on web before sending, but signed verbatim so the CA's `url`-match
        /// holds). Caches the response's fresh `Replay-Nonce`. On a non-2xx status the
        /// `application/problem+json` `detail` (or the raw body) is returned as the
        /// error string for the caller to wrap into the right [`Dns01Error`] variant;
        /// `payload` is empty (`b""`) for a POST-as-GET (RFC 8555 §6.3).
        async fn post_jws(
            &mut self,
            url: &str,
            auth: JwsAuth<'_>,
            payload: &[u8],
            key: &AccountKey,
        ) -> Result<reqwest::Response, String> {
            let nonce = self.nonce().await.map_err(|e| e.to_string())?;
            let protected = match auth {
                JwsAuth::Jwk => serde_json::json!({
                    "alg": "ES256", "jwk": key.jwk_json(), "nonce": nonce, "url": url,
                }),
                JwsAuth::Kid(kid) => serde_json::json!({
                    "alg": "ES256", "kid": kid, "nonce": nonce, "url": url,
                }),
            };
            let protected_bytes =
                serde_json::to_vec(&protected).expect("protected header serializes");
            let jws = key.sign_jws(&protected_bytes, payload);
            let body = serde_json::to_vec(&jws).expect("jws serializes");

            let resp = self
                .client
                .post(rewrite_acme_url(url))
                .header("content-type", "application/jose+json")
                .body(body)
                .send()
                .await
                .map_err(|e| format!("send: {e}"))?;

            // Cache the fresh nonce from this response (present on every ACME reply),
            // so the next POST reuses it without a round-trip to `newNonce`.
            if let Some(n) = replay_nonce(&resp) {
                self.nonce = Some(n);
            }

            if resp.status().is_success() {
                Ok(resp)
            } else {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                // ACME errors are `application/problem+json {type, detail, status}`;
                // surface `detail` when present, else the raw body.
                let detail = serde_json::from_str::<Problem>(&text)
                    .ok()
                    .and_then(|p| p.detail)
                    .unwrap_or_else(|| text.trim().to_string());
                Err(format!("{status}: {detail}"))
            }
        }
    }

    /// Read the `Replay-Nonce` header off a response as an owned string.
    fn replay_nonce(resp: &reqwest::Response) -> Option<String> {
        resp.headers()
            .get("replay-nonce")?
            .to_str()
            .ok()
            .map(str::to_string)
    }

    /// Read the `Location` header (the account URL on `newAccount`, the order URL on
    /// `newOrder`) as an owned string.
    fn location(resp: &reqwest::Response) -> Option<String> {
        resp.headers()
            .get(reqwest::header::LOCATION)?
            .to_str()
            .ok()
            .map(str::to_string)
    }

    // ── ACME wire shapes (the subset the DNS-01 order reads) ──────────────────────

    /// `application/problem+json` error body (RFC 8555 §6.7).
    #[derive(Debug, Deserialize)]
    struct Problem {
        detail: Option<String>,
    }

    /// Order object (RFC 8555 §7.1.3) — `camelCase` on the wire.
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct OrderState {
        status: OrderStatus,
        #[serde(default)]
        authorizations: Vec<String>,
        finalize: String,
        certificate: Option<String>,
        error: Option<Problem>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
    #[serde(rename_all = "camelCase")]
    enum OrderStatus {
        Pending,
        Ready,
        Processing,
        Valid,
        Invalid,
    }

    /// Authorization object (RFC 8555 §7.1.4).
    #[derive(Debug, Deserialize)]
    struct Authorization {
        identifier: AcmeIdentifier,
        status: AuthorizationStatus,
        challenges: Vec<AcmeChallenge>,
    }

    #[derive(Debug, Deserialize)]
    struct AcmeIdentifier {
        value: String,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
    #[serde(rename_all = "camelCase")]
    enum AuthorizationStatus {
        Pending,
        Valid,
        Invalid,
        Deactivated,
        Revoked,
        Expired,
    }

    /// Challenge object (RFC 8555 §7.1.5) — `type`/`url`/`token` are all we need.
    #[derive(Debug, Deserialize)]
    struct AcmeChallenge {
        #[serde(rename = "type")]
        kind: String,
        url: String,
        token: String,
    }

    /// `newAccount` request payload (RFC 8555 §7.3). An empty `contact` is valid —
    /// Fauna drives renewal reminders in-product, not via the CA's expiry email — so
    /// the field is omitted when empty (mirrors the native driver).
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct NewAccountPayload {
        terms_of_service_agreed: bool,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        contact: Vec<String>,
    }

    /// A live pure-Rust ACME order — the wasm twin of `instant_acme::Order` held by the
    /// native [`acme_order::Dns01OrderInProgress`](crate::acme_order::Dns01OrderInProgress).
    /// Owns the HTTP client (with its rolling nonce), the account signing key + `kid`,
    /// and the order/finalize URLs, so it can be suspended across the manual-mode paste
    /// and resumed by [`complete_dns01_order`].
    struct PureOrder {
        http: AcmeHttp,
        key: AccountKey,
        kid: String,
        order_url: String,
        finalize_url: String,
    }

    impl PureOrder {
        /// POST-as-GET the order URL and parse its current state (RFC 8555 §7.1.3).
        async fn refresh(&mut self) -> Result<OrderState, Dns01Error> {
            let kid = self.kid.clone();
            let resp = self
                .http
                .post_jws(&self.order_url, JwsAuth::Kid(&kid), b"", &self.key)
                .await
                .map_err(|d| Dns01Error::Ca(format!("refresh order: {d}")))?;
            resp.json::<OrderState>()
                .await
                .map_err(|e| Dns01Error::Ca(format!("parse order state: {e}")))
        }
    }

    /// A DNS-01 order opened but not yet validated — the suspendable manual-mode handle
    /// (tier 3). The wasm twin of [`acme_order::Dns01OrderInProgress`](crate::acme_order::Dns01OrderInProgress):
    /// same shape, same [`challenges_to_publish`](Self::challenges_to_publish)
    /// projection, driven by the pure [`PureOrder`] instead of `instant_acme::Order`.
    pub struct Dns01OrderInProgress {
        order: PureOrder,
        account_credentials: Vec<u8>,
        challenges: Vec<Dns01Challenge>,
        domains: Vec<String>,
    }

    impl Dns01OrderInProgress {
        /// The transient `_acme-challenge.<domain>` TXT record(s) to present — one per
        /// still-pending SAN. Identical projection to the native driver's
        /// `challenges_to_publish` (the single shared record-shape builder), so the
        /// `admin-dns` paste surface renders the same row on web and native.
        pub fn challenges_to_publish(&self) -> Vec<crate::PublishRecord> {
            self.challenges
                .iter()
                .map(|c| crate::acme_challenge_publish_record_named(&c.publish_name, &c.dns_value))
                .collect()
        }

        /// See the native twin's [`acme_order::Dns01OrderInProgress::account_credentials`](crate::acme_order::Dns01OrderInProgress::account_credentials).
        pub fn account_credentials(&self) -> &[u8] {
            &self.account_credentials
        }
    }

    /// Phase 1 of a DNS-01 order (wasm twin of [`acme_order::begin_dns01_order`](crate::acme_order::begin_dns01_order)):
    /// establish the ACME account (reuse the persisted one or create a fresh one — D6),
    /// open the order over the SAN set, and collect each pending authorization's
    /// `_acme-challenge` TXT. Builds the default browser fetch `reqwest` client —
    /// wasm-only (native production uses [`acme_order::begin_dns01_order`](crate::acme_order::begin_dns01_order);
    /// the native pebble proof uses [`begin_dns01_order_with_http`] with a CA-trusting
    /// client instead).
    #[cfg(target_arch = "wasm32")]
    pub async fn begin_dns01_order(
        cfg: &Dns01OrderConfig,
        account_credentials: Option<&[u8]>,
    ) -> Result<Dns01OrderInProgress, Dns01Error> {
        begin_dns01_order_with_client(cfg, account_credentials, reqwest::Client::new()).await
    }

    /// As [`begin_dns01_order`], but over a caller-supplied `reqwest::Client` — the
    /// pebble real-wire entry (W6), which builds one whose roots trust pebble's
    /// throwaway CA. `test-helpers` only.
    #[cfg(feature = "test-helpers")]
    pub async fn begin_dns01_order_with_http(
        cfg: &Dns01OrderConfig,
        account_credentials: Option<&[u8]>,
        client: reqwest::Client,
    ) -> Result<Dns01OrderInProgress, Dns01Error> {
        begin_dns01_order_with_client(cfg, account_credentials, client).await
    }

    /// Drive a **managed-mode** client-published DNS-01 order to completion over the
    /// pure driver and return the issued cert + the (possibly newly-created) account
    /// credentials. Signature-identical to the native
    /// [`acme_order::obtain_certificate_dns01`](crate::acme_order::obtain_certificate_dns01)
    /// so `lib.rs` re-exports one under the other's name per target. Wasm-only (builds
    /// the default browser fetch client; the native pebble proof uses
    /// [`obtain_certificate_dns01_with_http`]).
    #[cfg(target_arch = "wasm32")]
    pub async fn obtain_certificate_dns01(
        cfg: &Dns01OrderConfig,
        account_credentials: Option<&[u8]>,
        seam: &dyn DnsProviderSeam,
        provider_id: &str,
        fields: &[(String, SecretString)],
        zone: &DnsZoneRef,
        probe: Option<&dyn Dns01ResolvabilityProbe>,
    ) -> Result<Dns01Issued, Dns01Error> {
        let in_progress =
            begin_dns01_order_with_client(cfg, account_credentials, reqwest::Client::new()).await?;
        publish_then_complete(in_progress, cfg, seam, provider_id, fields, zone, probe).await
    }

    /// As [`obtain_certificate_dns01`], but over a caller-supplied client (pebble proof
    /// — W6). `test-helpers` only.
    // Mirrors `obtain_certificate_dns01`'s parameter list exactly, plus the
    // injected client — the shadowing is the point (see the native twin in
    // `acme_order.rs`).
    #[allow(clippy::too_many_arguments)]
    #[cfg(feature = "test-helpers")]
    pub async fn obtain_certificate_dns01_with_http(
        cfg: &Dns01OrderConfig,
        account_credentials: Option<&[u8]>,
        seam: &dyn DnsProviderSeam,
        provider_id: &str,
        fields: &[(String, SecretString)],
        zone: &DnsZoneRef,
        probe: Option<&dyn Dns01ResolvabilityProbe>,
        client: reqwest::Client,
    ) -> Result<Dns01Issued, Dns01Error> {
        let in_progress = begin_dns01_order_with_client(cfg, account_credentials, client).await?;
        publish_then_complete(in_progress, cfg, seam, provider_id, fields, zone, probe).await
    }

    /// Phase 2 of a **manual-mode** order (wasm twin of
    /// [`acme_order::complete_dns01_order`](crate::acme_order::complete_dns01_order)):
    /// the admin has pasted the surfaced `_acme-challenge` TXT(s) and the page verified
    /// them — signal every challenge ready, poll to `Ready`, finalize with a fresh CSR,
    /// and fetch the issued chain. No provider seam (the admin owns the pasted record).
    ///
    /// Runs the same [`PropagationGate`] as the native twin before signalling ready
    /// — the admin's confirmation describes their registrar's control plane, not
    /// what the authoritative NS serves. Since 2026-08-22 the browser supplies a
    /// probe too — [`NestRelayedProbe`](crate::NestRelayedProbe), which asks the
    /// nest to run the identical authoritative-direct query — so this no longer
    /// reduces to `propagation_wait`. (It never *could* here in practice: the
    /// manual caller passes `Duration::ZERO`, so a probe-less browser validated
    /// with no wait at all.) `None` remains meaningful for tests.
    pub async fn complete_dns01_order(
        in_progress: Dns01OrderInProgress,
        propagation_wait: Duration,
        zone_name: &str,
        probe: Option<&dyn Dns01ResolvabilityProbe>,
    ) -> Result<Dns01Issued, Dns01Error> {
        let Dns01OrderInProgress {
            mut order,
            account_credentials,
            challenges,
            domains,
        } = in_progress;
        let ready_urls: Vec<String> = challenges.iter().map(|c| c.challenge_url.clone()).collect();
        PropagationGate::manual(probe, propagation_wait)
            .wait(zone_name, &challenges)
            .await;
        let (cert_chain_pem, privkey_pem) =
            run_ca_dance(&mut order, ready_urls, &domains, Duration::ZERO).await?;
        Ok(Dns01Issued {
            cert_chain_pem,
            privkey_pem,
            account_credentials,
        })
    }

    /// The shared body behind [`begin_dns01_order`] / [`begin_dns01_order_with_http`]:
    /// load-or-create the account, then open the order and collect challenges — the
    /// only difference between production and the pebble proof is which `reqwest::Client`
    /// (default fetch/native-roots vs pebble-CA-trusting) backs the calls.
    async fn begin_dns01_order_with_client(
        cfg: &Dns01OrderConfig,
        account_credentials: Option<&[u8]>,
        client: reqwest::Client,
    ) -> Result<Dns01OrderInProgress, Dns01Error> {
        let (session, account_credentials) =
            load_or_create_account(cfg, account_credentials, client).await?;
        open_order_and_collect(session, account_credentials, cfg).await
    }

    /// The managed-mode tail (wasm twin of `acme_order::publish_then_complete`):
    /// publish every `_acme-challenge` TXT through the seam, run the CA dance, and
    /// ALWAYS tear the TXTs down — the **shared** [`with_published_challenges`]
    /// choreography with this driver's [`run_ca_dance`] plugged in.
    async fn publish_then_complete(
        in_progress: Dns01OrderInProgress,
        cfg: &Dns01OrderConfig,
        seam: &dyn DnsProviderSeam,
        provider_id: &str,
        fields: &[(String, SecretString)],
        zone: &DnsZoneRef,
        probe: Option<&dyn Dns01ResolvabilityProbe>,
    ) -> Result<Dns01Issued, Dns01Error> {
        let Dns01OrderInProgress {
            mut order,
            account_credentials,
            challenges,
            domains,
        } = in_progress;
        let ready_urls: Vec<String> = challenges.iter().map(|c| c.challenge_url.clone()).collect();
        // The propagation gate is owned by `with_published_challenges` (probe
        // poll where one exists — browsers have none today, so wasm takes the
        // fixed-wait arm); the dance itself starts with a zero wait. Mirrors
        // `acme_order::publish_then_complete`.
        let (cert_chain_pem, privkey_pem) = with_published_challenges(
            seam,
            provider_id,
            fields,
            zone,
            &challenges,
            PropagationGate::from_config(cfg, probe),
            run_ca_dance(&mut order, ready_urls, &domains, Duration::ZERO),
        )
        .await?;
        Ok(Dns01Issued {
            cert_chain_pem,
            privkey_pem,
            account_credentials,
        })
    }

    /// An established ACME account session: the HTTP client (with its nonce), the
    /// account signing key + `kid`, and the directory's `newOrder` URL.
    struct AcmeSession {
        http: AcmeHttp,
        key: AccountKey,
        kid: String,
        new_order_url: String,
    }

    /// Load a persisted ACME account (D6 — `Some`), or create a fresh one (`None`),
    /// over `client`. Returns the session + the serialized credentials to hand back for
    /// persistence (the input bytes unchanged when reusing, the new account's when
    /// creating) — byte-interoperable with the native `instant-acme` blob.
    async fn load_or_create_account(
        cfg: &Dns01OrderConfig,
        account_credentials: Option<&[u8]>,
        client: reqwest::Client,
    ) -> Result<(AcmeSession, Vec<u8>), Dns01Error> {
        match account_credentials {
            Some(bytes) => {
                let creds: AccountCredentials = serde_json::from_slice(bytes)
                    .map_err(|e| Dns01Error::Account(format!("parse stored credentials: {e}")))?;
                let key = AccountKey::from_pkcs8_der(&creds.key_pkcs8)
                    .map_err(|e| Dns01Error::Account(format!("restore account key: {e}")))?;
                // Resolve the directory URLs by re-fetching from `directory`.
                let dir = fetch_directory(&client, &creds.directory).await?;
                let http = AcmeHttp {
                    client,
                    new_nonce_url: dir.new_nonce,
                    nonce: None,
                };
                Ok((
                    AcmeSession {
                        http,
                        key,
                        kid: creds.id,
                        new_order_url: dir.new_order,
                    },
                    bytes.to_vec(),
                ))
            }
            None => {
                let dir = fetch_directory(&client, &cfg.directory_url).await?;
                let mut http = AcmeHttp {
                    client,
                    new_nonce_url: dir.new_nonce,
                    nonce: None,
                };
                let key = AccountKey::generate();
                // Empty contact is valid (see `NewAccountPayload`); a contact email, if
                // configured, gets the `mailto:` prefix the CA expects.
                let contact = if cfg.contact_email.is_empty() {
                    Vec::new()
                } else {
                    vec![format!("mailto:{}", cfg.contact_email)]
                };
                let payload = NewAccountPayload {
                    terms_of_service_agreed: true,
                    contact,
                };
                let payload_bytes =
                    serde_json::to_vec(&payload).expect("newAccount payload serializes");
                let resp = http
                    .post_jws(&dir.new_account, JwsAuth::Jwk, &payload_bytes, &key)
                    .await
                    .map_err(|d| Dns01Error::Account(format!("create account: {d}")))?;
                let kid = location(&resp).ok_or_else(|| {
                    Dns01Error::Account(
                        "newAccount response carried no Location header".to_string(),
                    )
                })?;
                let creds = AccountCredentials {
                    id: kid.clone(),
                    key_pkcs8: key
                        .to_pkcs8_der()
                        .map_err(|e| Dns01Error::Account(format!("encode account key: {e}")))?,
                    directory: cfg.directory_url.clone(),
                };
                let creds_bytes = serde_json::to_vec(&creds)
                    .map_err(|e| Dns01Error::Account(format!("serialize credentials: {e}")))?;
                Ok((
                    AcmeSession {
                        http,
                        key,
                        kid,
                        new_order_url: dir.new_order,
                    },
                    creds_bytes,
                ))
            }
        }
    }

    /// GET the ACME directory document (RFC 8555 §7.1.1) and parse the resource URLs we
    /// use. A plain GET (no JWS); rewritten to the proxy on web.
    async fn fetch_directory(
        client: &reqwest::Client,
        directory_url: &str,
    ) -> Result<DirectoryUrls, Dns01Error> {
        let resp = client
            .get(rewrite_acme_url(directory_url))
            .send()
            .await
            .map_err(|e| Dns01Error::Account(format!("fetch directory: {e}")))?;
        if !resp.status().is_success() {
            return Err(Dns01Error::Account(format!(
                "directory fetch failed: {}",
                resp.status()
            )));
        }
        resp.json::<DirectoryUrls>()
            .await
            .map_err(|e| Dns01Error::Account(format!("parse directory: {e}")))
    }

    /// Open the order over the full SAN set (`newOrder`) and collect each pending
    /// authorization's DNS-01 challenge — the wasm twin of
    /// `acme_order::open_order_and_collect`.
    async fn open_order_and_collect(
        mut session: AcmeSession,
        account_credentials: Vec<u8>,
        cfg: &Dns01OrderConfig,
    ) -> Result<Dns01OrderInProgress, Dns01Error> {
        let identifiers: Vec<serde_json::Value> = cfg
            .domains
            .iter()
            .map(|d| serde_json::json!({ "type": "dns", "value": d }))
            .collect();
        let payload = serde_json::json!({ "identifiers": identifiers });
        let payload_bytes = serde_json::to_vec(&payload).expect("newOrder payload serializes");

        let kid = session.kid.clone();
        let new_order_url = session.new_order_url.clone();
        let resp = session
            .http
            .post_jws(
                &new_order_url,
                JwsAuth::Kid(&kid),
                &payload_bytes,
                &session.key,
            )
            .await
            .map_err(|d| Dns01Error::Ca(format!("create order: {d}")))?;
        let order_url = location(&resp).ok_or_else(|| {
            Dns01Error::Ca("newOrder response carried no Location header".to_string())
        })?;
        let order_state: OrderState = resp
            .json()
            .await
            .map_err(|e| Dns01Error::Ca(format!("parse order: {e}")))?;

        // Collect the DNS-01 challenge each pending authorization needs (a cached-valid
        // authorization from a reused account — D6 — is already proven, so skipped).
        let mut challenges = Vec::with_capacity(order_state.authorizations.len());
        for authz_url in &order_state.authorizations {
            let resp = session
                .http
                .post_jws(authz_url, JwsAuth::Kid(&kid), b"", &session.key)
                .await
                .map_err(|d| Dns01Error::Ca(format!("get authorization: {d}")))?;
            let authz: Authorization = resp
                .json()
                .await
                .map_err(|e| Dns01Error::Ca(format!("parse authorization: {e}")))?;
            if authz.status == AuthorizationStatus::Valid {
                continue;
            }
            let domain = authz.identifier.value;
            let challenge = authz
                .challenges
                .iter()
                .find(|c| c.kind == "dns-01")
                .ok_or_else(|| Dns01Error::NoChallenge(domain.clone()))?;
            let dns_value = session.key.dns_value(&challenge.token);
            challenges.push(Dns01Challenge {
                publish_name: cfg.publish_name_for(&domain),
                domain,
                dns_value,
                challenge_url: challenge.url.clone(),
            });
        }

        let order = PureOrder {
            http: session.http,
            key: session.key,
            kid: session.kid,
            order_url,
            finalize_url: order_state.finalize,
        };
        Ok(Dns01OrderInProgress {
            order,
            account_credentials,
            challenges,
            domains: cfg.domains.clone(),
        })
    }

    /// The CA-side half (wasm twin of `acme_order::run_ca_dance`): wait for DNS
    /// propagation, signal every challenge ready (`POST {}`), poll the order to `Ready`,
    /// then finalize with a fresh CSR and fetch the chain. This driver's `ca_dance`
    /// future for the shared [`with_published_challenges`].
    async fn run_ca_dance(
        order: &mut PureOrder,
        ready_urls: Vec<String>,
        domains: &[String],
        propagation_wait: Duration,
    ) -> Result<(String, String), Dns01Error> {
        if !propagation_wait.is_zero() {
            sleep(propagation_wait).await;
        }
        let kid = order.kid.clone();
        for url in &ready_urls {
            order
                .http
                .post_jws(url, JwsAuth::Kid(&kid), b"{}", &order.key)
                .await
                .map_err(|d| Dns01Error::Ca(format!("set challenge ready: {d}")))?;
        }

        if poll_order_ready(order).await? {
            finalize_and_fetch(order, domains).await
        } else {
            Err(Dns01Error::ValidBeforeFinalize)
        }
    }

    /// Poll the order until it is `Ready` (→ `Ok(true)`, finalize) or `Valid`
    /// (→ `Ok(false)`, already issued). Mirrors the native 20×2s budget.
    async fn poll_order_ready(order: &mut PureOrder) -> Result<bool, Dns01Error> {
        let mut retries = 20u8;
        loop {
            sleep(Duration::from_secs(2)).await;
            let state = order.refresh().await?;
            match state.status {
                OrderStatus::Ready => return Ok(true),
                OrderStatus::Valid => return Ok(false),
                OrderStatus::Pending | OrderStatus::Processing => {
                    retries = retries.checked_sub(1).ok_or_else(|| {
                        Dns01Error::Ca("order did not become ready after 20 polls".to_string())
                    })?;
                }
                OrderStatus::Invalid => {
                    return Err(Dns01Error::Ca(format!(
                        "order became invalid: {:?}",
                        state.error.and_then(|p| p.detail)
                    )));
                }
            }
        }
    }

    /// Generate a fresh keypair + multi-SAN CSR ([`build_csr`]), finalize the order
    /// (`POST {csr}`), poll until the order is `Valid` with a `certificate` URL (10×2s),
    /// and fetch the chain (POST-as-GET; body is `application/pem-certificate-chain`).
    /// Returns `(cert_chain_pem, privkey_pem)`.
    async fn finalize_and_fetch(
        order: &mut PureOrder,
        domains: &[String],
    ) -> Result<(String, String), Dns01Error> {
        let (csr_der, privkey_pem) =
            build_csr(domains).map_err(|e| Dns01Error::Csr(e.to_string()))?;
        let payload = serde_json::json!({ "csr": B64URL.encode(&csr_der) });
        let payload_bytes = serde_json::to_vec(&payload).expect("finalize payload serializes");

        let kid = order.kid.clone();
        let finalize_url = order.finalize_url.clone();
        order
            .http
            .post_jws(
                &finalize_url,
                JwsAuth::Kid(&kid),
                &payload_bytes,
                &order.key,
            )
            .await
            .map_err(|d| Dns01Error::Ca(format!("finalize order: {d}")))?;

        let mut retries = 10u8;
        let cert_url = loop {
            sleep(Duration::from_secs(2)).await;
            let state = order.refresh().await?;
            match state.status {
                OrderStatus::Valid => match state.certificate {
                    Some(url) => break url,
                    None => {
                        retries = retries.checked_sub(1).ok_or_else(|| {
                            Dns01Error::Ca(
                                "order valid but no certificate URL after 10 polls".into(),
                            )
                        })?;
                    }
                },
                OrderStatus::Processing | OrderStatus::Ready | OrderStatus::Pending => {
                    retries = retries.checked_sub(1).ok_or_else(|| {
                        Dns01Error::Ca("certificate not available after 10 polls".to_string())
                    })?;
                }
                OrderStatus::Invalid => {
                    return Err(Dns01Error::Ca(format!(
                        "order became invalid during finalize: {:?}",
                        state.error.and_then(|p| p.detail)
                    )));
                }
            }
        };

        let resp = order
            .http
            .post_jws(&cert_url, JwsAuth::Kid(&kid), b"", &order.key)
            .await
            .map_err(|d| Dns01Error::Ca(format!("fetch certificate: {d}")))?;
        let cert_chain_pem = resp
            .text()
            .await
            .map_err(|e| Dns01Error::Ca(format!("read certificate body: {e}")))?;

        Ok((cert_chain_pem, privkey_pem))
    }

    #[cfg(test)]
    mod order_tests {
        use super::*;

        // ── W3: ACME URL rewriter (web→proxy, native passthrough) ────────────────

        /// On native every ACME URL is used verbatim (direct CA conversation).
        #[test]
        fn rewrite_native_passes_through() {
            let url = "https://acme-v02.api.letsencrypt.org/acme/new-order";
            assert_eq!(
                rewrite_acme_url_for(url, BuildEnv::Native, "https://proxy.fauna.social"),
                url
            );
        }

        /// On web a production LE URL is re-pointed at `<proxy>/acme-le/<path>` — the
        /// exact inverse of the cors-proxy's `acme-le` → base mapping, so a round-trip
        /// reaches the real LE path. The directory and an absolute follow-up URL both
        /// rewrite the same way.
        #[test]
        fn rewrite_web_production_routes_through_proxy() {
            let root = "https://proxy.fauna.social";
            assert_eq!(
                rewrite_acme_url_for(
                    "https://acme-v02.api.letsencrypt.org/directory",
                    BuildEnv::Web,
                    root
                ),
                "https://proxy.fauna.social/acme-le/directory"
            );
            assert_eq!(
                rewrite_acme_url_for(
                    "https://acme-v02.api.letsencrypt.org/acme/authz/abc123",
                    BuildEnv::Web,
                    root
                ),
                "https://proxy.fauna.social/acme-le/acme/authz/abc123"
            );
        }

        /// The staging host maps to the distinct `acme-le-staging` prefix.
        #[test]
        fn rewrite_web_staging_routes_through_distinct_prefix() {
            assert_eq!(
                rewrite_acme_url_for(
                    "https://acme-staging-v02.api.letsencrypt.org/acme/finalize/9",
                    BuildEnv::Web,
                    "https://proxy.fauna.social"
                ),
                "https://proxy.fauna.social/acme-le-staging/acme/finalize/9"
            );
        }

        /// A trailing slash on the proxy root is normalized (no doubled `//`).
        #[test]
        fn rewrite_web_handles_trailing_slash_on_root() {
            assert_eq!(
                rewrite_acme_url_for(
                    "https://acme-v02.api.letsencrypt.org/directory",
                    BuildEnv::Web,
                    "https://proxy.fauna.social/"
                ),
                "https://proxy.fauna.social/acme-le/directory"
            );
        }

        /// A non-LE ACME host (a self-hosted CA, or pebble in the native proof) is not
        /// re-pointed even on web — only the two known LE hosts route through the proxy.
        #[test]
        fn rewrite_web_passes_through_unknown_host() {
            let url = "https://127.0.0.1:14000/dir";
            assert_eq!(
                rewrite_acme_url_for(url, BuildEnv::Web, "https://proxy.fauna.social"),
                url
            );
        }

        /// The proxy prefixes must stay in lockstep with the cors-proxy bases: the
        /// production host base is the directory URL minus the `/directory` path.
        #[test]
        fn proxy_prefix_bases_match_directory_constants() {
            assert!(
                crate::acme_shared::LETS_ENCRYPT_PRODUCTION.starts_with(ACME_PROXY_PREFIXES[0].0),
                "acme-le base must prefix the production directory URL"
            );
            assert!(
                crate::acme_shared::LETS_ENCRYPT_STAGING.starts_with(ACME_PROXY_PREFIXES[1].0),
                "acme-le-staging base must prefix the staging directory URL"
            );
        }
    }
}

/// Re-export the `order` driver's entry points per target. On wasm these are the
/// production names the cross-target `DnsManagementMachine::issue_cert` calls (the
/// same symbols `acme_order` exposes on native); the suspendable handle +
/// `complete_dns01_order` are needed on both wasm and the native pebble proof. The
/// native `_with_http` proof entries get `pure_*` aliases in `lib.rs`.
#[cfg(any(target_arch = "wasm32", feature = "test-helpers"))]
pub use order::{Dns01OrderInProgress, complete_dns01_order};
#[cfg(target_arch = "wasm32")]
pub use order::{begin_dns01_order, obtain_certificate_dns01};
#[cfg(all(not(target_arch = "wasm32"), feature = "test-helpers"))]
pub use order::{begin_dns01_order_with_http, obtain_certificate_dns01_with_http};

/// The exact `key_pkcs8` from `instant-acme 0.7.2`'s own credential test
/// vector (`lib.rs:890`) — a P-256 PKCS#8 written by ring, published in that
/// crate's public test suite, so no account's live key. Proves the
/// native(ring)→wasm(p256) account-key restore direction (D6). One home for
/// the fixture: `acme_order`'s cross-crate credential pins borrow it rather
/// than carrying a second copy of the literal.
#[cfg(test)]
pub(crate) const INSTANT_ACME_KEY_PKCS8_B64URL: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgJVWC_QzOTCS5vtsJp2IG-UDc8cdDfeoKtxSZxaznM-mhRANCAAQenCPoGgPFTdPJ7VLLKt56RxPlYT1wNXnHc54PEyBg3LxKaH0-sJkX0mL8LyPEdsfL_Oz4TxHkWLJGrXVtNhfH";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_instant_acme_ring_pkcs8_key() {
        let der = B64URL
            .decode(INSTANT_ACME_KEY_PKCS8_B64URL.as_bytes())
            .expect("vector decodes");
        let key = AccountKey::from_pkcs8_der(&der).expect("p256 parses ring's PKCS#8");
        // Deterministic — same key yields the same thumbprint every time.
        let tp = key.thumbprint();
        assert_eq!(tp.len(), 43, "sha256 base64url-no-pad is 43 chars");
        assert_eq!(tp, key.thumbprint());
    }

    #[test]
    fn pkcs8_round_trips() {
        let key = AccountKey::generate();
        let der = key.to_pkcs8_der().unwrap();
        let restored = AccountKey::from_pkcs8_der(&der).unwrap();
        assert_eq!(key.thumbprint(), restored.thumbprint());
        assert_eq!(key.jwk_json(), restored.jwk_json());
    }

    #[test]
    fn credentials_deserialize_instant_acme_directory_form() {
        let json = format!(
            r#"{{"id":"https://acme/acct/1","key_pkcs8":"{INSTANT_ACME_KEY_PKCS8_B64URL}","directory":"https://acme-staging-v02.api.letsencrypt.org/directory"}}"#
        );
        let creds: AccountCredentials = serde_json::from_str(&json).expect("parses");
        assert_eq!(creds.id, "https://acme/acct/1");
        assert_eq!(
            creds.directory,
            "https://acme-staging-v02.api.letsencrypt.org/directory"
        );
        // The decoded key must load.
        AccountKey::from_pkcs8_der(&creds.key_pkcs8).expect("key loads");
        // Re-serialize → key_pkcs8 round-trips to the same base64url field.
        let reser = serde_json::to_string(&creds).unwrap();
        assert!(reser.contains(INSTANT_ACME_KEY_PKCS8_B64URL));
    }

    #[test]
    fn credentials_without_directory_do_not_parse() {
        let json = format!(r#"{{"id":"id","key_pkcs8":"{INSTANT_ACME_KEY_PKCS8_B64URL}"}}"#);
        assert!(serde_json::from_str::<AccountCredentials>(&json).is_err());
    }

    #[test]
    fn credentials_full_round_trip() {
        let key = AccountKey::generate();
        let creds = AccountCredentials {
            id: "https://acme/acct/9".to_string(),
            key_pkcs8: key.to_pkcs8_der().unwrap(),
            directory: "https://acme/dir".to_string(),
        };
        let bytes = serde_json::to_vec(&creds).unwrap();
        let back: AccountCredentials = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.id, creds.id);
        assert_eq!(back.key_pkcs8, creds.key_pkcs8);
        assert_eq!(back.directory, creds.directory);
        // The restored key matches.
        let restored = AccountKey::from_pkcs8_der(&back.key_pkcs8).unwrap();
        assert_eq!(restored.thumbprint(), key.thumbprint());
    }

    #[test]
    fn dns_value_matches_manual_recomputation() {
        let key = AccountKey::generate();
        let token = "evaGxfADs6pSRb2LAv9IZf17Dt3juxGJ-PCt92wr-oA"; // gitleaks:allow
        let key_auth = key.key_authorization(token);
        assert_eq!(key_auth, format!("{token}.{}", key.thumbprint()));
        let expected = B64URL.encode(Sha256::digest(key_auth.as_bytes()));
        assert_eq!(key.dns_value(token), expected);
        assert_eq!(key.dns_value(token).len(), 43);
    }

    #[test]
    fn jws_is_well_formed_and_verifies() {
        use p256::ecdsa::signature::Verifier;

        let key = AccountKey::generate();
        let protected = br#"{"alg":"ES256","nonce":"abc","url":"https://acme/new-order"}"#;
        let payload = br#"{"identifiers":[]}"#;
        let jws = key.sign_jws(protected, payload);

        let protected_b64 = jws["protected"].as_str().unwrap();
        let payload_b64 = jws["payload"].as_str().unwrap();
        let sig_b64 = jws["signature"].as_str().unwrap();
        // Members are base64url-no-pad of the inputs.
        assert_eq!(B64URL.decode(protected_b64).unwrap(), protected);
        assert_eq!(B64URL.decode(payload_b64).unwrap(), payload);
        // Signature is 64-byte raw P1363 and verifies over `protected.payload`.
        let sig_bytes = B64URL.decode(sig_b64).unwrap();
        assert_eq!(sig_bytes.len(), 64, "ES256 JWS sig is raw r‖s, not DER");
        let sig = p256::ecdsa::Signature::from_slice(&sig_bytes).unwrap();
        let signing_input = format!("{protected_b64}.{payload_b64}");
        key.signing
            .verifying_key()
            .verify(signing_input.as_bytes(), &sig)
            .expect("JWS signature verifies");
    }

    #[test]
    fn csr_carries_all_sans_and_self_verifies() {
        use der::Decode;
        use x509_cert::request::CertReq;

        let domains = vec![
            "home.example.com".to_string(),
            "mail.home.example.com".to_string(),
        ];
        let (csr_der, privkey_pem) = build_csr(&domains).expect("CSR builds");

        assert!(privkey_pem.starts_with("-----BEGIN PRIVATE KEY-----"));
        assert!(
            privkey_pem
                .trim_end()
                .ends_with("-----END PRIVATE KEY-----")
        );

        // Re-parse the CSR and confirm every SAN dNSName is present.
        let csr = CertReq::from_der(&csr_der).expect("CSR re-parses");
        let san_dns = extract_csr_dns_sans(&csr);
        assert_eq!(san_dns, domains, "CSR SAN set == ordered domains");
    }

    /// Pull the dNSName SANs out of a parsed CSR's `extensionRequest` attribute.
    fn extract_csr_dns_sans(csr: &x509_cert::request::CertReq) -> Vec<String> {
        use der::{Decode, oid::AssociatedOid};
        use x509_cert::ext::pkix::{SubjectAltName, name::GeneralName};

        let mut out = Vec::new();
        for attr in csr.info.attributes.iter() {
            // The extensionRequest attribute (PKCS#9) carries a sequence of X.509
            // extensions; find SubjectAltName within it.
            for any in attr.values.iter() {
                let Ok(exts) = any.decode_as::<x509_cert::ext::Extensions>() else {
                    continue;
                };
                for ext in exts.iter() {
                    if ext.extn_id == SubjectAltName::OID
                        && let Ok(san) = SubjectAltName::from_der(ext.extn_value.as_bytes())
                    {
                        for gn in san.0.iter() {
                            if let GeneralName::DnsName(d) = gn {
                                out.push(d.as_str().to_string());
                            }
                        }
                    }
                }
            }
        }
        out
    }
}
