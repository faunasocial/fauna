//! Shared transport for the three bundled-provider adapters
//! (`registrar::bundled`, `dns::bundled`, `vps::bundled`) — the client half of
//! the open **Fauna Bundled Provider API v1**
//! (`docs/goal/architecture/provisioning/bundled-provider-api.md`).
//!
//! One intermediary serves all three capabilities from ONE authenticated base
//! URL, so — like `namecheap_api` — the URL building, the Bearer header, the
//! error envelope and the RFC 8628 device-authorization flow live here once
//! rather than being re-derived per adapter. The base URL is the **user's**
//! (`base-url` credential field, `registry.md` § Bundled provider): there is
//! no canonical host, and every conformant company plugs in through the same
//! code with zero Rust changes.
//!
//! Wire shapes are deserialized with unknown fields ignored (serde's default)
//! — v1 evolves additively (spec § Versioning), so a later implementation's
//! extra fields must never break an older client.

use reqwest::Client;
use serde::Deserialize;

use crate::error::{ProvisionError, ensure_success};

/// The fixed public OAuth `client_id` every Fauna app presents (spec
/// § Authentication): a public client with no secret, which the intermediary
/// accepts without pre-registration (RFC 8252 § 8.5).
pub const CLIENT_ID: &str = "fauna";
/// The one scope v1 defines.
pub const SCOPE: &str = "provisioning";
/// The API major version this adapter speaks — the `/v1` path prefix.
pub const API_VERSION: u32 = 1;
/// RFC 8628 § 3.2: the poll interval to use when the server names none.
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 5;

/// Normalize the user-typed base URL: surrounding whitespace and trailing
/// slashes are noise (`https://x.example/` and `https://x.example` are the
/// same server), so both spellings produce the same request URLs.
pub fn normalize_base_url(raw: &str) -> String {
    raw.trim().trim_end_matches('/').to_string()
}

/// Normalize `raw` and refuse it unless it is `https://`, per spec § Endpoints
/// (*"All paths are relative to the user's `base-url` (`https://…`, no
/// trailing slash; the adapter normalizes)"*) — the property `normalize_base_url`
/// never checked. This is the credential the product holds the widest scope
/// over (registrar + DNS + VPS in one bearer token), so a cleartext base is a
/// domain/DNS/box takeover from one capture, not just a leaked login.
///
/// **The one carve-out is an explicit loopback host on `http://`** — a
/// self-hosted intermediary on `127.0.0.1`/`::1`/`localhost` is a legitimate
/// development shape, and it's also what every test in this crate and the
/// onboarding machine already drives through the user-typed `base-url` field
/// (wiremock, pytest-httpserver). The carve-out is by **host**, not by call
/// site — "only skip the check under `#[cfg(test)]`" would make the guard
/// untested; "skip it for any override path" would make it not a guard.
/// ⚠ The scheme and host are read off a **parsed** URL, never split out of the
/// string by hand, and that is the whole point rather than a style preference.
/// A hand-split gate and the client that dials do not have to agree, and when
/// they disagree the gate is decorative: everything before the last `@` in an
/// authority is *userinfo* to a URL parser, so
/// `http://localhost:8080@evil.example` names the host `evil.example` while
/// reading — to a hand-split rule, and to a human reviewer — as `localhost:8080`.
/// This gate accepted exactly that spelling, and refused the honest
/// `http://evil.example`, until 2026-09-02. Parsing with the same `url` crate
/// `reqwest` dials through makes the two agree by construction; the pin
/// `an_accepted_base_dials_the_host_the_gate_approved` asserts the agreement on
/// the URL actually dialed rather than on the base string.
pub fn checked_base_url(raw: &str) -> Result<String, ProvisionError> {
    let base = normalize_base_url(raw);
    let ok = match reqwest::Url::parse(&base) {
        Ok(url) => match url.scheme() {
            "https" => url.has_host(),
            "http" => is_loopback_url(&url),
            _ => false,
        },
        Err(_) => false,
    };
    if ok {
        // The stored base stays the string the caller gave (normalized), not the
        // parser's re-serialization: the parse is a *decision*, not a rewrite,
        // and every existing caller and stored credential keeps its exact bytes.
        Ok(base)
    } else {
        Err(ProvisionError::Other(
            "the provider address must start with https://".to_string(),
        ))
    }
}

/// True when `url`'s host is the IPv4 loopback, the IPv6 loopback, or
/// `localhost` — the only hosts [`checked_base_url`]'s `http://` carve-out
/// admits, named literally by `bundled-provider-api.md` § Endpoints.
///
/// Deliberately **not** widened to `127.0.0.0/8`, unlike
/// `fauna_client::trust::is_loopback_authority` (the transport-trust predicate,
/// which serves a different rule and may be wider); the spec names `127.0.0.1`.
/// Two spellings do start passing that a hand-split rejected, and both are the
/// parser canonicalizing rather than the rule widening: `http://LOCALHOST` (the
/// host is lowercased) and `http://[0::1]` (the IPv6 literal is canonicalized to
/// `::1`). Both are the same host to everything that dials them.
///
/// ⚠ This crate cannot delegate to `fauna_client::trust::is_loopback_authority`
/// even though that one is hardened against the same userinfo trick:
/// `fauna-anon-client` DEPENDS on this crate, so the edge would be a cycle, and
/// it is native-only where this crate also builds for wasm32. The shared
/// authority-splitting primitives in `fauna_core::web` are the other option and
/// are the right tool where no parser is at hand — but here the input is a whole
/// URL on its way to `reqwest`, so the parser is both available and the only
/// thing that cannot disagree with the dial.
fn is_loopback_url(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    // `host_str()` keeps the brackets on an IPv6 literal (`[::1]`); the
    // carve-out's vocabulary is the bare host.
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    host == "127.0.0.1" || host == "::1" || host == "localhost"
}

/// The authenticated client every adapter holds: base URL + Bearer token.
#[derive(Clone)]
pub struct BundledApi {
    base: String,
    token: String,
}

impl BundledApi {
    pub fn new(base_url: String, token: String) -> Self {
        Self {
            base: normalize_base_url(&base_url),
            token,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// Attach the Bearer header to a request the adapter builds itself (the
    /// token stays private to this transport).
    pub fn apply_bearer(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        rb.bearer_auth(&self.token)
    }

    /// `{base}/v1{path}` — `path` starts with `/`.
    pub fn url(&self, path: &str) -> String {
        format!("{}/v{}{}", self.base, API_VERSION, path)
    }

    pub async fn get(
        &self,
        client: &Client,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<reqwest::Response, ProvisionError> {
        let resp = client
            .get(self.url(path))
            .bearer_auth(&self.token)
            .query(query)
            .send()
            .await?;
        ensure_success(resp).await
    }

    pub async fn post_json(
        &self,
        client: &Client,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<reqwest::Response, ProvisionError> {
        let resp = client
            .post(self.url(path))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?;
        ensure_success(resp).await
    }

    pub async fn put_json(
        &self,
        client: &Client,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<reqwest::Response, ProvisionError> {
        let resp = client
            .put(self.url(path))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?;
        ensure_success(resp).await
    }

    /// Raw DELETE — the caller decides what a `404` means (both spec deletes
    /// treat it as success, via `vps::finish_delete`).
    pub async fn delete_raw(
        &self,
        client: &Client,
        path: &str,
    ) -> Result<reqwest::Response, ProvisionError> {
        Ok(client
            .delete(self.url(path))
            .bearer_auth(&self.token)
            .send()
            .await?)
    }

    /// `GET /v1/me` — the one call all three `verify()`s share.
    pub async fn me(&self, client: &Client) -> Result<MeResponse, ProvisionError> {
        let resp = self.get(client, "/me", &[]).await?;
        let me: MeResponse = resp.json().await?;
        if me.api_version != API_VERSION {
            return Err(ProvisionError::Other(format!(
                "bundled provider speaks API version {}, this app speaks v{}",
                me.api_version, API_VERSION
            )));
        }
        Ok(me)
    }
}

/// `GET /v1/me` (spec § Endpoints).
#[derive(Debug, Deserialize)]
pub struct MeResponse {
    pub api_version: u32,
    #[serde(default)]
    pub zones: Vec<ZoneJson>,
    #[serde(default)]
    pub locations: Vec<LocationJson>,
}

#[derive(Debug, Deserialize)]
pub struct ZoneJson {
    #[serde(deserialize_with = "deserialize_id")]
    pub id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct LocationJson {
    #[serde(deserialize_with = "deserialize_id")]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub city: String,
    #[serde(default)]
    pub country: String,
}

/// Spec ids are strings, but an implementer fronting a numeric-id supplier
/// may leak a JSON number; accept both rather than fail the whole flow on
/// the least interesting field in the response.
pub(crate) fn deserialize_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum IdRepr {
        Str(String),
        Num(u64),
    }
    Ok(match IdRepr::deserialize(deserializer)? {
        IdRepr::Str(s) => s,
        IdRepr::Num(n) => n.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Error envelope
// ---------------------------------------------------------------------------

/// The `code` of a spec error envelope (`{"error":{"code":…}}`), if the body
/// is one. Adapters key structured decisions (`price_changed`,
/// `tld_not_supported`) on this rather than on prose.
pub fn error_code(body: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Envelope {
        error: ErrorBody,
    }
    #[derive(Deserialize)]
    struct ErrorBody {
        code: String,
    }
    serde_json::from_str::<Envelope>(body)
        .ok()
        .map(|e| e.error.code)
}

// ---------------------------------------------------------------------------
// hosted-auth — RFC 8628 device authorization (spec § Authentication)
// ---------------------------------------------------------------------------

/// The device-authorization response (RFC 8628 § 3.2).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct DeviceAuthorization {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_interval() -> u64 {
    DEFAULT_POLL_INTERVAL_SECS
}

impl DeviceAuthorization {
    /// The URL the app opens: the `_complete` form (code pre-filled) when the
    /// server offers it, else the bare verification page where the user types
    /// `user_code`.
    pub fn open_url(&self) -> &str {
        self.verification_uri_complete
            .as_deref()
            .unwrap_or(&self.verification_uri)
    }
}

/// One poll of the token endpoint (RFC 8628 § 3.4/3.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevicePoll {
    /// The user approved; this is the Bearer token to store.
    Token(String),
    /// Not yet — poll again after `interval`.
    Pending,
    /// Not yet, and the server wants a slower cadence (+5 s per RFC 8628).
    SlowDown,
    /// The device code expired before the user finished.
    Expired,
    /// The user declined.
    Denied,
}

/// `POST {base}/v1/auth/device` (form-encoded, unauthenticated).
pub async fn device_authorize(
    client: &Client,
    base_url: &str,
) -> Result<DeviceAuthorization, ProvisionError> {
    let base = checked_base_url(base_url)?;
    let resp = client
        .post(format!("{base}/v{API_VERSION}/auth/device"))
        .form(&[("client_id", CLIENT_ID), ("scope", SCOPE)])
        .send()
        .await?;
    let resp = ensure_success(resp).await?;
    Ok(resp.json().await?)
}

/// `POST {base}/v1/auth/token` (form-encoded, unauthenticated) — one poll.
/// RFC 8628 reports the pending states as `400` with an `error` field, so a
/// non-2xx is inspected before it is treated as a failure.
pub async fn device_token(
    client: &Client,
    base_url: &str,
    device_code: &str,
) -> Result<DevicePoll, ProvisionError> {
    #[derive(Deserialize)]
    struct TokenOk {
        access_token: String,
    }
    #[derive(Deserialize)]
    struct TokenErr {
        error: String,
    }
    let base = checked_base_url(base_url)?;
    let resp = client
        .post(format!("{base}/v{API_VERSION}/auth/token"))
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device_code),
            ("client_id", CLIENT_ID),
        ])
        .send()
        .await?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if status.is_success() {
        let ok: TokenOk = serde_json::from_str(&body).map_err(ProvisionError::parse)?;
        return Ok(DevicePoll::Token(ok.access_token));
    }
    match serde_json::from_str::<TokenErr>(&body)
        .map(|e| e.error)
        .as_deref()
    {
        Ok("authorization_pending") => Ok(DevicePoll::Pending),
        Ok("slow_down") => Ok(DevicePoll::SlowDown),
        Ok("expired_token") => Ok(DevicePoll::Expired),
        Ok("access_denied") => Ok(DevicePoll::Denied),
        _ => Err(ProvisionError::provider(status.as_u16(), body)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Near-miss hosts and the userinfo trick, refused by
    /// [`checked_base_url`]'s loopback carve-out — everything before the last
    /// `@` in an authority is userinfo to a URL parser, so each of the last
    /// five DIALS `evil.example` while reading as loopback to a human, and the
    /// bearer here is the widest-scope credential the product mints, sent in
    /// cleartext. Shared with `an_accepted_base_dials_the_host_the_gate_approved`
    /// below, which needs the identical list to check the gate against a
    /// **widening**, not just a narrowing.
    const NOT_LOOPBACK_LOOKALIKES: &[&str] = &[
        "http://127.0.0.1.evil.example",
        "http://notlocalhost",
        "http://192.168.1.1",
        "http://[::2]",
        // Not widened to 127.0.0.0/8: the spec names `127.0.0.1` literally.
        "http://127.0.0.2",
        "http://localhost:80@evil.example",
        "http://127.0.0.1:80@evil.example",
        "http://[::1]@evil.example",
        "http://localhost@evil.example",
        "http://localhost:8080@evil.example/bundled",
    ];

    #[test]
    fn base_url_normalization_drops_trailing_slashes_and_whitespace() {
        assert_eq!(
            normalize_base_url("  https://x.example/// "),
            "https://x.example"
        );
        assert_eq!(
            BundledApi::new("https://x.example/".into(), "t".into()).url("/me"),
            "https://x.example/v1/me"
        );
    }

    #[test]
    fn checked_base_url_requires_https_with_an_explicit_loopback_carve_out() {
        // Spec-conformant.
        assert_eq!(
            checked_base_url("https://bundle.example/ ").unwrap(),
            "https://bundle.example"
        );
        // Refused: no scheme, wrong scheme, or an https:// with no host.
        for bad in ["bundle.example", "http://bundle.example", "https://"] {
            assert!(
                checked_base_url(bad).is_err(),
                "{bad} should be refused for a non-conformant scheme"
            );
        }
        // The loopback carve-out: IPv4, IPv6 (bracketed), and `localhost`,
        // each with and without a port/path — but ONLY on `http://`, and
        // ONLY for these exact hosts, never a lookalike.
        for ok in [
            "http://127.0.0.1",
            "http://127.0.0.1:8080",
            "http://127.0.0.1:8080/bundled",
            "http://[::1]",
            "http://[::1]:8080",
            "http://localhost",
            "http://localhost:8080",
        ] {
            assert_eq!(
                checked_base_url(ok).unwrap(),
                normalize_base_url(ok),
                "{ok} is the documented loopback carve-out and must be accepted"
            );
        }
        // USERINFO. Everything before the last `@` in an authority is
        // userinfo to a URL parser, so each of the last five DIALS
        // `evil.example` while reading as loopback to a human — and the
        // bearer here is the widest-scope credential the product mints, sent
        // in cleartext.
        for not_loopback in NOT_LOOPBACK_LOOKALIKES {
            assert!(
                checked_base_url(not_loopback).is_err(),
                "{not_loopback} is not one of the three loopback hosts and must be refused"
            );
        }
    }

    /// The gate's notion of the host must equal the parser's — asserted on the
    /// URL that is actually DIALED, not on the base string.
    ///
    /// Refusing a bad spelling is necessary but not sufficient: the bug this
    /// pins was a *disagreement* between two host extractors, and only a
    /// comparison against the dialed URL can catch the next spelling where they
    /// diverge. Iterating only the bases the gate *already* accepts can witness
    /// a **narrowing** (an accepted base starts dialing elsewhere) but is
    /// structurally blind to a **widening** (a new spelling the gate should
    /// refuse gets accepted instead) — the direction this pin's review
    /// finding found it actually failed in. So this iterates the
    /// **union** of the accepted spellings and the refused lookalikes above and
    /// asserts the *implication* — `gate accepted ⟹ dialed host ∈ {127.0.0.1,
    /// ::1, localhost}` — rather than the postcondition over accepted-only
    /// inputs; a refused candidate simply contributes nothing.
    #[test]
    fn an_accepted_base_dials_the_host_the_gate_approved() {
        let candidates = [
            "http://127.0.0.1",
            "http://127.0.0.1:8080",
            "http://127.0.0.1:8080/bundled",
            "http://[::1]",
            "http://[::1]:8080",
            "http://localhost",
            "http://localhost:8080",
            // WHATWG lowercases the host, so this is the same host as
            // `localhost` to everything that dials it.
            "http://LOCALHOST",
        ]
        .into_iter()
        .chain(NOT_LOOPBACK_LOOKALIKES.iter().copied());
        for candidate in candidates {
            // A refused candidate contributes nothing to the implication — only
            // what the gate actually accepted gets held to the dial-agreement.
            let Ok(base) = checked_base_url(candidate) else {
                continue;
            };
            let api = BundledApi::new(base, "token".to_string());
            let dialed = reqwest::Url::parse(&api.url("/me"))
                .unwrap_or_else(|e| panic!("{candidate} produced an unparseable dial URL: {e}"));
            // `host_str()` keeps IPv6 brackets (`[::1]`); the gate's vocabulary
            // is the bare host, so compare on that.
            let host = dialed.host_str().unwrap_or("");
            let host = host
                .strip_prefix('[')
                .and_then(|h| h.strip_suffix(']'))
                .unwrap_or(host);
            assert!(
                matches!(host, "127.0.0.1" | "::1" | "localhost"),
                "{candidate} was accepted by the gate but DIALS {host:?} — the \
                 gate and the parser disagree about the host, which is the \
                 whole class of bug this pin exists for"
            );
        }
    }

    #[test]
    fn error_code_reads_the_spec_envelope_only() {
        assert_eq!(
            error_code(r#"{"error":{"code":"price_changed","message":"x"}}"#).as_deref(),
            Some("price_changed")
        );
        assert_eq!(error_code(r#"{"error":"authorization_pending"}"#), None);
        assert_eq!(error_code("not json"), None);
    }

    #[test]
    fn device_authorization_defaults_the_interval_and_prefers_the_complete_uri() {
        let d: DeviceAuthorization = serde_json::from_str(
            r#"{"device_code":"d","user_code":"ABCD-EFGH","verification_uri":"https://x/act","expires_in":600}"#,
        )
        .unwrap();
        assert_eq!(d.interval, DEFAULT_POLL_INTERVAL_SECS);
        assert_eq!(d.open_url(), "https://x/act");
        let d: DeviceAuthorization = serde_json::from_str(
            r#"{"device_code":"d","user_code":"ABCD-EFGH","verification_uri":"https://x/act","verification_uri_complete":"https://x/act?c=ABCD-EFGH","expires_in":600,"interval":7}"#,
        )
        .unwrap();
        assert_eq!(d.interval, 7);
        assert_eq!(d.open_url(), "https://x/act?c=ABCD-EFGH");
    }

    #[test]
    fn ids_accept_numbers_as_well_as_strings() {
        let z: ZoneJson = serde_json::from_str(r#"{"id":42,"name":"example.com"}"#).unwrap();
        assert_eq!(z.id, "42");
        let z: ZoneJson = serde_json::from_str(r#"{"id":"z-1","name":"example.com"}"#).unwrap();
        assert_eq!(z.id, "z-1");
    }
}
