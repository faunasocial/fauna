//! The Spaceship API transport, shared by the registrar and DNS adapters
//! (`registrar/spaceship.rs`, `dns/spaceship.rs`) the way `namecheap_api` is
//! shared by Namecheap's two.
//!
//! Spaceship authenticates every call with a header PAIR, `X-Api-Key` plus
//! `X-Api-Secret` — never a query parameter, so the credential stays out of
//! every URL the redacting error path keeps (`error::RedactedHttpError`). Its
//! API answers a browser's preflight `401` with no
//! `Access-Control-Allow-Origin` (measured 2026-11-17), so the registry declares
//! it `cors_policy: proxy` and the default base honours that through
//! [`crate::proxy::default_api_base`]
//! (`docs/goal/architecture/provisioning/registry.md` § Proxy boundary).
//!
//! Shapes are read from Spaceship's published API reference (its embedded
//! OpenAPI document, read 2026-11-17), not a signed call — what a live
//! credential still has to witness is listed in `registry.md` § Delegation
//! doors.

use serde::Deserialize;

use crate::error::{ProvisionError, ensure_success};
use crate::proxy::{BuildEnv, default_api_base, default_api_base_for_env};

/// Direct Spaceship API base. Native builds call it; wasm32 builds route
/// through the CORS forwarder's `spaceship` entry.
pub const DIRECT_API: &str = "https://spaceship.dev/api/v1";
/// The forwarder's provider key followed by the direct URL's path.
pub const PROXY_PREFIX: &str = "spaceship/api/v1";

/// The largest page `GET /domains` answers (`take` is 1..100).
const DOMAINS_PAGE: u32 = 100;

/// The credential pair and the base every Spaceship call is made against.
#[derive(Clone)]
pub struct SpaceshipApi {
    api_key: String,
    api_secret: String,
    api_base: String,
}

impl SpaceshipApi {
    pub fn new(api_key: String, api_secret: String) -> Self {
        Self::with_base_url(
            api_key,
            api_secret,
            default_api_base(DIRECT_API, PROXY_PREFIX),
        )
    }

    /// [`Self::new`] with the build env supplied explicitly — lets a native
    /// test assert the browser routing (`tests/cors_policy_bijection.rs`).
    pub fn new_for_env(api_key: String, api_secret: String, env: BuildEnv) -> Self {
        Self::with_base_url(
            api_key,
            api_secret,
            default_api_base_for_env(DIRECT_API, PROXY_PREFIX, env),
        )
    }

    /// Point the transport at another base — a wiremock server in the
    /// conformance tests, the e2e fake through dispatch's `api-base` override.
    pub fn with_base_url(api_key: String, api_secret: String, api_base: String) -> Self {
        Self {
            api_key,
            api_secret,
            api_base: api_base.trim_end_matches('/').to_string(),
        }
    }

    /// The API base this transport calls.
    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    /// A request to `{base}{path}` carrying the credential pair.
    pub(crate) fn request(
        &self,
        client: &reqwest::Client,
        method: reqwest::Method,
        path: &str,
    ) -> reqwest::RequestBuilder {
        client
            .request(method, format!("{}{path}", self.api_base))
            .header("X-Api-Key", &self.api_key)
            .header("X-Api-Secret", &self.api_secret)
    }

    /// Every domain on the account, ASCII (A-label) names, walking
    /// `GET /domains?take=&skip=` to the reported `total`.
    pub(crate) async fn list_domains(
        &self,
        client: &reqwest::Client,
    ) -> Result<Vec<String>, ProvisionError> {
        let mut out = Vec::new();
        let mut skip: u32 = 0;
        loop {
            let resp = self
                .request(
                    client,
                    reqwest::Method::GET,
                    &format!("/domains?take={DOMAINS_PAGE}&skip={skip}"),
                )
                .send()
                .await?;
            let page: DomainList = ensure_success(resp)
                .await?
                .json()
                .await
                .map_err(ProvisionError::parse)?;
            let got = page.items.len() as u32;
            out.extend(page.items.into_iter().map(|d| d.name));
            skip += got;
            if got == 0 || skip >= page.total {
                return Ok(out);
            }
        }
    }
}

#[derive(Deserialize)]
struct DomainList {
    #[serde(default)]
    items: Vec<DomainItem>,
    #[serde(default)]
    total: u32,
}

#[derive(Deserialize)]
struct DomainItem {
    /// The A-label form (`xn--…`); `unicodeName` carries the U-label.
    name: String,
}
