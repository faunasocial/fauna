//! Turning an OAuth `client_id` into a resolved client identity — the nest's
//! seat of what the bridge's `resolveClient` did (TP5 S2).
//!
//! The *policy* is untouched and stays where it already was:
//! [`fauna_bridge_atproto::oauth_client`] classifies a `client_id`, validates a
//! fetched metadata document, and completes a confidential client's key set.
//! What lives here is the half a pure module cannot do — fetch bytes from a
//! host the caller named, and remember the answer.
//!
//! # The fetch is guarded twice, on purpose, and parsed once
//!
//! A `client_id` is whatever the requesting client says it is, so resolving one
//! is an **attacker-directed outbound fetch** and both seats of the nest's SSRF
//! policy apply:
//!
//! * [`crate::ssrf::resolve_global_addrs`] resolves the host and refuses unless
//!   every address is globally routable, returning the verified addresses so
//!   the connection can be **pinned** to them (that is its documented reason for
//!   returning them, and it is what closes the DNS-rebinding window).
//! * [`fauna_bridge_atproto::fetch_guard::check_fetch_target`] is the pure,
//!   default-deny policy over the already-parsed components plus those
//!   addresses. It adds what an address classifier cannot see: the host must be
//!   a **public, registrable DNS name** — never an IP literal, a single label,
//!   or a `.local` / `.localhost` name.
//!
//! Neither is redundant and neither is drift: they are one policy in two seats, sharing one classifier
//! ([`fauna_core::resolve::is_global_ip`]), and the split is what keeps the
//! check and the dial from being two different parses. The URL is parsed
//! **once**, here, by the component that dials — passing a URL string to the
//! policy instead would re-open the parser-differential bypass where the check
//! inspects host A and the connection goes to host B.
//!
//! The size and time caps are the caller's half of the guard — they govern the
//! transfer, not the target — so they live with the fetch, exactly as they do
//! in the Go original.
//!
//! # Why the failure reason is never echoed
//!
//! A fetch failure describes *our* outbound network: a refused address, a
//! timeout, a TLS failure. Echoed to the caller it would make `/oauth/par` an
//! oracle for what this nest can reach. Every fetch failure therefore answers
//! with one flat `invalid_client` and is logged instead, where an admin can see
//! it — the Go original's rule, carried across by value.

use std::time::Duration;

use async_trait::async_trait;
use fauna_bridge_atproto::oauth_client::{
    ClientIdPlan, ClientResolution, ResolvedClient, attach_client_jwks, parse_client_metadata,
    plan_client_id,
};

use crate::oauth_as_error::{ERR_INVALID_CLIENT, ERR_INVALID_GRANT, OAuthDeny};
use crate::oauth_as_state::{CachedResolution, ClientMetadataCache};

/// Caps a client-metadata document. Real documents are well under 2 KiB; the
/// cap exists because the URL is attacker-chosen, so the response size is an
/// attacker's choice too.
pub const CLIENT_METADATA_MAX_BYTES: usize = 64 * 1024;

/// Bounds one resolution attempt. A PAR request waits on this, so it is short:
/// a client-metadata host that cannot answer in ten seconds is one this flow
/// should fail against rather than hold a request task for.
pub const CLIENT_METADATA_FETCH_TIMEOUT: Duration = Duration::from_secs(10);

// ── The fetch seam ───────────────────────────────────────────────────────────

/// Why a client-metadata fetch produced no document.
///
/// Deliberately coarse. The variants exist so the guard's behaviour is
/// unit-testable, **not** so a caller can report them: every one of them
/// reaches the client as the same `invalid_client` (module docs).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MetadataFetchError {
    #[error("url did not parse")]
    InvalidUrl,
    #[error("target refused by the fetch guard: {0}")]
    Refused(String),
    #[error("upstream status {0}")]
    Status(u16),
    #[error("response exceeded the size cap")]
    TooLarge,
    #[error("response was not valid utf-8")]
    NotUtf8,
    #[error("network error")]
    Network,
}

/// The outbound seam, mirroring [`crate::link_preview::fetch::LinkPreviewFetcher`]:
/// production wires [`GuardedMetadataFetcher`], tests inject a fake.
///
/// It exists so this module's *decisions* — the cache's TTLs, the two-step
/// resolution, which failures are cached negatively — are testable without a
/// network, which is the same reason the Go original put the fetcher behind an
/// interface.
///
/// The same seam carries the nest's other third-party-directed fetches — a
/// hosted plugin's module at install ([`Self::fetch_bytes`]) and the plugin's
/// own outbound requests ([`Self::request`]) — so all three dial through one
/// guard and one test fake observes them all. A fake that serves documents
/// only inherits the two defaults, which refuse.
#[async_trait]
pub trait ClientMetadataFetcher: Send + Sync {
    /// GET `url`, reading at most [`CLIENT_METADATA_MAX_BYTES`] of the body.
    ///
    /// Implementors MUST apply the full guard described in the module docs.
    async fn fetch(&self, url: &str) -> Result<String, MetadataFetchError>;

    /// GET `url` as raw bytes, reading at most `max_bytes` of the body — a
    /// hosted plugin's module (`third-party.md` § The manifest, the `wasm`
    /// form's `execution.module`). The same guard as [`Self::fetch`].
    async fn fetch_bytes(
        &self,
        url: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, MetadataFetchError> {
        let _ = (url, max_bytes);
        Err(MetadataFetchError::Refused(
            "this fetcher serves metadata documents only".into(),
        ))
    }

    /// One request a hosted plugin's `http.fetch` asked for, AFTER the
    /// plugin's declared-host policy admitted its URL — any method, the
    /// plugin's headers and body, the reply's body read to at most
    /// `max_bytes`. The same guard as [`Self::fetch`]: a declared host that
    /// resolves to a private address is still refused.
    async fn request(
        &self,
        req: fauna_plugin_host::HttpRequest,
        max_bytes: usize,
    ) -> Result<fauna_plugin_host::HttpResponse, MetadataFetchError> {
        let _ = (req, max_bytes);
        Err(MetadataFetchError::Refused(
            "this fetcher serves metadata documents only".into(),
        ))
    }
}

/// Bounds one plugin-module download — up to
/// [`fauna_plugin_host::PLUGIN_MODULE_MAX_BYTES`] over a slow link, behind an
/// admin waiting on the install card, so longer than a document's fetch.
pub const PLUGIN_MODULE_FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// Bounds one hosted plugin's outbound request.
pub const PLUGIN_HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// The production fetcher: one parse, both guard seats, a pinned dial
/// ([`crate::ssrf::guarded_dial`]), and a capped streaming read.
#[derive(Debug, Default)]
pub struct GuardedMetadataFetcher;

/// The guarded client for `url` — under `test-hooks` only, a mapped host
/// dials its loopback test server ([`test_hook_client`]); every other host
/// meets the full guard. Shared with the events webhook's POST
/// (`crate::events_webhook`), the one other attacker-directed dial a
/// consented document aims.
pub(crate) async fn dial(
    url: &str,
    timeout: Duration,
) -> Result<(reqwest::Client, url::Url), MetadataFetchError> {
    #[cfg(feature = "test-hooks")]
    if let Some(hooked) = test_hook_client(url, timeout)? {
        return Ok(hooked);
    }
    crate::ssrf::guarded_dial(url, reqwest::Client::builder().timeout(timeout))
        .await
        .map_err(|e| match e {
            crate::ssrf::GuardedDialError::InvalidUrl => MetadataFetchError::InvalidUrl,
            crate::ssrf::GuardedDialError::Refused(why) => MetadataFetchError::Refused(why),
            crate::ssrf::GuardedDialError::Network => MetadataFetchError::Network,
        })
}

#[async_trait]
impl ClientMetadataFetcher for GuardedMetadataFetcher {
    async fn fetch(&self, url: &str) -> Result<String, MetadataFetchError> {
        let (client, parsed) = dial(url, CLIENT_METADATA_FETCH_TIMEOUT).await?;
        let body = get_capped(client, parsed, CLIENT_METADATA_MAX_BYTES).await?;
        String::from_utf8(body).map_err(|_| MetadataFetchError::NotUtf8)
    }

    async fn fetch_bytes(
        &self,
        url: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, MetadataFetchError> {
        let (client, parsed) = dial(url, PLUGIN_MODULE_FETCH_TIMEOUT).await?;
        get_capped(client, parsed, max_bytes).await
    }

    async fn request(
        &self,
        req: fauna_plugin_host::HttpRequest,
        max_bytes: usize,
    ) -> Result<fauna_plugin_host::HttpResponse, MetadataFetchError> {
        let (client, parsed) = dial(&req.url, PLUGIN_HTTP_TIMEOUT).await?;
        let method = reqwest::Method::from_bytes(req.method.as_bytes())
            .map_err(|_| MetadataFetchError::InvalidUrl)?;
        let mut builder = client.request(method, parsed);
        for (name, value) in &req.headers {
            // The dial is pinned to the parsed URL's host; a plugin-chosen
            // `Host` would name a different virtual host on the same address.
            if name.eq_ignore_ascii_case("host") {
                continue;
            }
            builder = builder.header(name.as_str(), value.as_str());
        }
        let response = builder
            .body(req.body)
            .send()
            .await
            .map_err(|_| MetadataFetchError::Network)?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(k, v)| Some((k.to_string(), v.to_str().ok()?.to_string())))
            .collect();
        let body = read_body(response, max_bytes).await?;
        Ok(fauna_plugin_host::HttpResponse {
            status,
            headers,
            body,
        })
    }
}

/// GET on a client already pinned to its target, reading at most
/// `max_bytes` of a success body.
async fn get_capped(
    client: reqwest::Client,
    parsed: url::Url,
    max_bytes: usize,
) -> Result<Vec<u8>, MetadataFetchError> {
    let response = client
        .get(parsed)
        .send()
        .await
        .map_err(|_| MetadataFetchError::Network)?;
    let status = response.status();
    if !status.is_success() {
        return Err(MetadataFetchError::Status(status.as_u16()));
    }
    read_body(response, max_bytes).await
}

async fn read_body(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, MetadataFetchError> {
    crate::ssrf::read_capped(response, max_bytes)
        .await
        .map_err(|e| match e {
            crate::ssrf::CappedReadError::TooLarge => MetadataFetchError::TooLarge,
            crate::ssrf::CappedReadError::Network => MetadataFetchError::Network,
        })
}

/// Under `test-hooks` **only**, and only when
/// `FAUNA_TEST_CLIENT_METADATA_RESOLVE_JSON` is set: a client-metadata
/// document on a real hostname, served by the e2e harness on a loopback port
/// with a test-CA cert — so a tier_3 journey can present an `https`
/// `client_id` whose host a kind manifest names (`third-party-kinds.md`
/// § The manifest). `FAUNA_TEST_CLIENT_METADATA_EXTRA_CA_PEM` names the CA.
/// The resolve map is loopback-only and mapped-host-only
/// (`crate::ssrf::test_resolve_client`), so an unmapped host still meets the
/// full guard; production builds no `test-hooks`.
#[cfg(feature = "test-hooks")]
fn test_hook_client(
    url: &str,
    timeout: Duration,
) -> Result<Option<(reqwest::Client, url::Url)>, MetadataFetchError> {
    let Ok(resolve_json) = std::env::var("FAUNA_TEST_CLIENT_METADATA_RESOLVE_JSON") else {
        return Ok(None);
    };
    let extra_ca = match std::env::var_os("FAUNA_TEST_CLIENT_METADATA_EXTRA_CA_PEM") {
        Some(path) => Some(std::fs::read(path).map_err(|_| MetadataFetchError::Network)?),
        None => None,
    };
    crate::ssrf::test_resolve_client(
        url,
        &resolve_json,
        extra_ca.as_deref(),
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout),
    )
    .map_err(|e| MetadataFetchError::Refused(e.to_string()))
}

// ── Resolution ───────────────────────────────────────────────────────────────

/// Resolve a `client_id` into a client identity, through the cache.
///
/// The three-way plan is the shared module's: a loopback client resolves with
/// no network at all, an `https` `client_id` becomes a fetch, and anything else
/// is refused before any I/O happens.
///
/// **Every refusal on this path is cached negatively**, including a malformed
/// `client_id` the pure planner rejected. Without that, a flood of distinct bad
/// identifiers costs one decision — or one outbound fetch — per request, which
/// is the amplification an unauthenticated endpoint must not offer.
/// The resolved client's kind manifest, verified by the one shared-Rust door
/// against the host its document was served from — `None` for a document
/// without a `fauna` member. Run at resolution (a refusal there refuses the
/// client) and again by the consent's mint, which records what it verified;
/// the second run cannot fail on a client the first one accepted.
///
/// Past the shared parse it applies the two refusals `third-party.md` § The
/// manifest gives the `service_auth` member: a declared `lxm` the custodian
/// would never mint for (`service_auth_lxm_admitted`, the deny half) refuses
/// the document, and so does a document whose `scope` names
/// `fauna:identity:op:atproto.service_auth` while declaring no entry — the
/// scope would grant nothing.
///
/// # Errors
/// `invalid_client` naming the manifest's refusal.
pub fn verified_manifest(
    client: &ResolvedClient,
) -> Result<Option<fauna_protocol::kind_manifest::VerifiedManifest>, OAuthDeny> {
    use fauna_bridge_atproto::authz::service_auth_lxm_admitted;
    use fauna_bridge_atproto::fauna_scope::SCOPE_IDENTITY_OP_ATPROTO_SERVICE_AUTH;
    use fauna_protocol::kind_manifest::{client_id_host, verify_manifest};
    let refuse = |why: String| {
        OAuthDeny::new(
            ERR_INVALID_CLIENT,
            format!("client metadata `fauna` manifest refused: {why}"),
        )
    };
    let asks_service_auth = client
        .declared_scopes
        .iter()
        .any(|s| s == SCOPE_IDENTITY_OP_ATPROTO_SERVICE_AUTH);
    let Some(jws) = client.fauna_manifest.as_deref() else {
        if asks_service_auth {
            return Err(refuse(format!(
                "the document's scope names {SCOPE_IDENTITY_OP_ATPROTO_SERVICE_AUTH} but it \
                 carries no manifest declaring a service_auth entry"
            )));
        }
        return Ok(None);
    };
    let host = client_id_host(&client.client_id)
        .ok_or_else(|| refuse("the document is not served over https".to_string()))?;
    let verified = verify_manifest(jws, &host).map_err(|e| refuse(e.to_string()))?;
    if let Some((entry, lxm)) = verified.service_auth.iter().find_map(|e| {
        e.lxm
            .iter()
            .find(|m| !service_auth_lxm_admitted(m))
            .map(|m| (e, m))
    }) {
        return Err(refuse(format!(
            "service_auth names {lxm:?} for {:?}, a method no service-auth token is minted for",
            entry.aud
        )));
    }
    if asks_service_auth && verified.service_auth.is_empty() {
        return Err(refuse(format!(
            "the document's scope names {SCOPE_IDENTITY_OP_ATPROTO_SERVICE_AUTH} but its \
             manifest declares no service_auth entry"
        )));
    }
    Ok(Some(verified))
}

/// Key continuity (`third-party-kinds.md` § The manifest): the resolved
/// document's manifest must still be signed by the key the account's roster
/// row pinned at its last consent. Run wherever an existing principal is used
/// without a fresh consent — the refresh grant — so a publisher's key rotation
/// is a visible re-consent, never a silent swap under existing grants; the
/// consent ceremony itself is what replaces the pinned key, so it never runs
/// this. A row that pinned no key (consented from a document without a
/// manifest) has nothing to continue.
///
/// # Errors
/// `invalid_grant` when the key changed or the manifest is gone — the remedy
/// is a new consent, not a different client.
pub fn manifest_key_continues(
    client: &ResolvedClient,
    pinned: Option<&[u8; 32]>,
) -> Result<(), OAuthDeny> {
    use fauna_protocol::kind_manifest::{client_id_host, verify_manifest_pinned};
    let Some(pinned) = pinned else {
        return Ok(());
    };
    let refuse = |why: &str| {
        OAuthDeny::new(
            ERR_INVALID_GRANT,
            format!(
                "the client's manifest {why} since this account consented — re-consent required"
            ),
        )
    };
    let (Some(jws), Some(host)) = (
        client.fauna_manifest.as_deref(),
        client_id_host(&client.client_id),
    ) else {
        return Err(refuse("is gone"));
    };
    verify_manifest_pinned(jws, &host, pinned)
        .map(|_| ())
        .map_err(|_| refuse("is signed by a different publisher key"))
}

pub async fn resolve_client(
    cache: &ClientMetadataCache,
    fetcher: &dyn ClientMetadataFetcher,
    client_id: &str,
    now: i64,
) -> Result<ResolvedClient, OAuthDeny> {
    if let Some(cached) = cache.get(client_id, now) {
        return match cached {
            CachedResolution::Resolved(client) => Ok(*client),
            CachedResolution::Refused { error, description } => {
                Err(OAuthDeny { error, description })
            }
        };
    }

    let fetch_url = match plan_client_id(client_id.to_string()) {
        ClientIdPlan::Deny { error, description } => {
            let deny = OAuthDeny { error, description };
            cache_deny(cache, client_id, &deny, now);
            return Err(deny);
        }
        // The loopback development client — synthesized, so there is nothing to
        // go stale and the positive TTL is simply the memory bound.
        ClientIdPlan::Resolved { client } => {
            cache.put(
                client_id.to_string(),
                CachedResolution::Resolved(Box::new(client.clone())),
                now,
            );
            return Ok(client);
        }
        ClientIdPlan::Fetch { url } => url,
    };

    let body = match fetcher.fetch(&fetch_url).await {
        Ok(body) => body,
        Err(e) => {
            tracing::info!(
                client_id = %client_id,
                error = %e,
                "oauth: client metadata fetch failed"
            );
            let deny = OAuthDeny::new(
                ERR_INVALID_CLIENT,
                "client metadata could not be retrieved from client_id",
            );
            cache_deny(cache, client_id, &deny, now);
            return Err(deny);
        }
    };

    // The URL that was fetched, not the one that was asked for — they are the
    // same string by construction (the plan carries `client_id` back verbatim),
    // and passing the plan's copy is what keeps that true if it ever stops
    // being.
    let client = match parse_client_metadata(fetch_url, body) {
        ClientResolution::Resolved { client } => client,
        ClientResolution::Deny { error, description } => {
            let deny = OAuthDeny { error, description };
            cache_deny(cache, client_id, &deny, now);
            return Err(deny);
        }
    };

    // The kind manifest, verified before the client is cached: a document
    // whose `fauna` member does not verify against its own host never yields
    // a client, so no card is ever drawn from it (`third-party-kinds.md`
    // § The manifest — every refusal happens at resolution).
    if let Err(deny) = verified_manifest(&client) {
        cache_deny(cache, client_id, &deny, now);
        return Err(deny);
    }

    // A confidential client that declared `jwks_uri` is completed HERE, before
    // it is cached, so the cached client always carries a usable key set and
    // authenticating an assertion never touches the network. The second fetch
    // is a second CALL SITE of the one guarded fetcher — never a second fetch
    // path, which is what the parser-differential rule forbids.
    let client = match jwks_uri_to_follow(&client) {
        None => client,
        Some(jwks_uri) => match complete_jwks(fetcher, client, &jwks_uri, client_id).await {
            Ok(completed) => completed,
            Err(deny) => {
                cache_deny(cache, client_id, &deny, now);
                return Err(deny);
            }
        },
    };

    cache.put(
        client_id.to_string(),
        CachedResolution::Resolved(Box::new(client.clone())),
        now,
    );
    Ok(client)
}

/// The `jwks_uri` that still needs following, if any.
///
/// ⚠ [`ResolvedClient::confidential`] is the single owner of "is this client
/// confidential" — a published key set never implies it. Reading `jwks` here
/// instead would hold a public client to a contract its document never made.
fn jwks_uri_to_follow(client: &ResolvedClient) -> Option<String> {
    if !client.confidential {
        return None;
    }
    client
        .jwks_uri
        .as_ref()
        .filter(|uri| !uri.is_empty())
        .cloned()
}

/// Follow a confidential client's `jwks_uri` and complete it.
///
/// The URL is the client's own, from a document this nest already fetched and
/// validated — so it is exactly as attacker-influenced as the `client_id` was,
/// and goes through exactly the same guard.
async fn complete_jwks(
    fetcher: &dyn ClientMetadataFetcher,
    client: ResolvedClient,
    jwks_uri: &str,
    client_id: &str,
) -> Result<ResolvedClient, OAuthDeny> {
    let body = match fetcher.fetch(jwks_uri).await {
        Ok(body) => body,
        Err(e) => {
            tracing::info!(
                client_id = %client_id,
                jwks_uri = %jwks_uri,
                error = %e,
                "oauth: client jwks fetch failed"
            );
            return Err(OAuthDeny::new(
                ERR_INVALID_CLIENT,
                "the client's key set could not be retrieved from its jwks_uri",
            ));
        }
    };
    match attach_client_jwks(client, body) {
        ClientResolution::Resolved { client } => Ok(client),
        ClientResolution::Deny { error, description } => Err(OAuthDeny { error, description }),
    }
}

fn cache_deny(cache: &ClientMetadataCache, client_id: &str, deny: &OAuthDeny, now: i64) {
    cache.put(
        client_id.to_string(),
        CachedResolution::Refused {
            error: deny.error.clone(),
            description: deny.description.clone(),
        },
        now,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Counts what it was asked for, so a test can assert the *absence* of a
    /// fetch — which is how "the cache actually caches" is checkable at all.
    #[derive(Default)]
    struct FakeFetcher {
        answers: Mutex<Vec<(String, Result<String, MetadataFetchError>)>>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeFetcher {
        fn with(url: &str, body: &str) -> Self {
            let f = Self::default();
            f.answers
                .lock()
                .unwrap()
                .push((url.to_string(), Ok(body.to_string())));
            f
        }

        fn failing() -> Self {
            Self::default()
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ClientMetadataFetcher for FakeFetcher {
        async fn fetch(&self, url: &str) -> Result<String, MetadataFetchError> {
            self.calls.lock().unwrap().push(url.to_string());
            let answers = self.answers.lock().unwrap();
            for (candidate, answer) in answers.iter() {
                if candidate == url {
                    return answer.clone();
                }
            }
            Err(MetadataFetchError::Network)
        }
    }

    fn metadata_doc(client_id: &str) -> String {
        serde_json::json!({
            "client_id": client_id,
            "client_name": "Test Client",
            "redirect_uris": ["https://client.example/cb"],
            "scope": "atproto",
            "grant_types": ["authorization_code"],
            "response_types": ["code"],
            "application_type": "web",
            "dpop_bound_access_tokens": true,
        })
        .to_string()
    }

    /// A document carrying a kind manifest resolves only when the manifest
    /// verifies against the host the document was served from — a manifest
    /// signed for another publisher refuses the client at resolution
    /// (`third-party-kinds.md` § The manifest, same-origin anchoring).
    #[tokio::test]
    async fn a_kind_manifest_must_verify_against_the_documents_host() {
        use fauna_protocol::kind_manifest::{ed25519_did_key, sign_manifest};
        let key = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]);
        let manifest_for = |domain: &str| {
            sign_manifest(
                &key,
                &serde_json::json!({
                    "version": 1,
                    "publisher": {
                        "domain": domain,
                        "key": ed25519_did_key(&key.verifying_key().to_bytes()),
                    },
                    "kinds": [{
                        "kind": format!("ext.{domain}.notes"),
                        "class": "state", "merge": "latest-wins", "floor": "none",
                    }],
                }),
                None,
            )
        };
        let doc_with = |client_id: &str, jws: String| {
            let mut doc: serde_json::Value =
                serde_json::from_str(&metadata_doc(client_id)).unwrap();
            doc["fauna"] = serde_json::Value::String(jws);
            doc.to_string()
        };
        let client_id = "https://client.example/metadata.json";

        let good = doc_with(client_id, manifest_for("client.example"));
        let client = resolve_client(
            &ClientMetadataCache::new(),
            &FakeFetcher::with(client_id, &good),
            client_id,
            1_000,
        )
        .await
        .expect("a manifest of this host's own resolves");
        let verified = verified_manifest(&client).unwrap().unwrap();
        assert_eq!(verified.publisher_key, key.verifying_key().to_bytes());
        assert_eq!(
            verified.kinds[0].kind.to_string(),
            "ext.client.example.notes"
        );

        let foreign = doc_with(client_id, manifest_for("other.example"));
        let deny = resolve_client(
            &ClientMetadataCache::new(),
            &FakeFetcher::with(client_id, &foreign),
            client_id,
            1_000,
        )
        .await
        .expect_err("another publisher's manifest refuses");
        assert_eq!(deny.error, ERR_INVALID_CLIENT);
        assert!(deny.description.contains("manifest refused"), "{deny:?}");
    }

    /// A document carrying `manifest` (a payload, signed here under `key`)
    /// and, when given, a space-delimited `scope`.
    fn doc_with_manifest(
        client_id: &str,
        key: &ed25519_dalek::SigningKey,
        extra: serde_json::Value,
        scope: Option<&str>,
    ) -> String {
        use fauna_protocol::kind_manifest::{ed25519_did_key, sign_manifest};
        let mut payload = serde_json::json!({
            "version": 1,
            "publisher": {
                "domain": "client.example",
                "key": ed25519_did_key(&key.verifying_key().to_bytes()),
            },
        });
        for (k, v) in extra.as_object().unwrap() {
            payload[k] = v.clone();
        }
        let mut doc: serde_json::Value = serde_json::from_str(&metadata_doc(client_id)).unwrap();
        doc["fauna"] = serde_json::Value::String(sign_manifest(key, &payload, None));
        if let Some(scope) = scope {
            doc["scope"] = serde_json::Value::String(scope.to_string());
        }
        doc.to_string()
    }

    async fn resolve_doc(client_id: &str, doc: &str) -> Result<ResolvedClient, OAuthDeny> {
        resolve_client(
            &ClientMetadataCache::new(),
            &FakeFetcher::with(client_id, doc),
            client_id,
            1_000,
        )
        .await
    }

    const SERVICE_AUTH_SCOPE: &str = "atproto fauna:identity:op:atproto.service_auth";

    /// `third-party.md` § The manifest: the `service_auth` member resolves
    /// typed; a method the custodian never mints for refuses the document; a
    /// document asking for the service-auth scope without declaring an entry
    /// refuses — with a manifest and without one.
    #[tokio::test]
    async fn the_service_auth_member_is_refused_at_resolution_on_both_halves() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]);
        let client_id = "https://client.example/metadata.json";
        let feed = serde_json::json!({ "service_auth": [
            { "aud": "did:web:api.bsky.app#bsky_appview", "lxm": ["app.bsky.feed.getFeedSkeleton"] }
        ]});

        let client = resolve_doc(
            client_id,
            &doc_with_manifest(client_id, &key, feed.clone(), Some(SERVICE_AUTH_SCOPE)),
        )
        .await
        .expect("a declared feed method with the scope resolves");
        let verified = verified_manifest(&client).unwrap().unwrap();
        assert_eq!(
            verified.service_auth[0].aud,
            "did:web:api.bsky.app#bsky_appview"
        );

        for (doc, why) in [
            (
                doc_with_manifest(
                    client_id,
                    &key,
                    serde_json::json!({ "service_auth": [
                        { "aud": "did:web:api.bsky.app", "lxm": ["com.atproto.identity.submitPlcOperation"] }
                    ]}),
                    None,
                ),
                "a denied lxm",
            ),
            (
                doc_with_manifest(
                    client_id,
                    &key,
                    serde_json::json!({}),
                    Some(SERVICE_AUTH_SCOPE),
                ),
                "the scope with a manifest declaring no entry",
            ),
            (
                {
                    let mut d: serde_json::Value =
                        serde_json::from_str(&metadata_doc(client_id)).unwrap();
                    d["scope"] = serde_json::Value::String(SERVICE_AUTH_SCOPE.into());
                    d.to_string()
                },
                "the scope with no manifest at all",
            ),
        ] {
            let deny = resolve_doc(client_id, &doc).await.expect_err(why);
            assert_eq!(deny.error, ERR_INVALID_CLIENT, "{why}");
            assert!(
                deny.description.contains("manifest refused"),
                "{why}: {deny:?}"
            );
        }
    }

    /// `third-party-kinds.md` § The manifest, key continuity: a row's pinned
    /// key continues only under the same key; a rotated key, or a document
    /// that dropped its manifest, is `invalid_grant` until a re-consent; a
    /// row that pinned nothing has nothing to continue.
    #[tokio::test]
    async fn the_pinned_publisher_key_must_continue() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]);
        let rotated = ed25519_dalek::SigningKey::from_bytes(&[0x43; 32]);
        let client_id = "https://client.example/metadata.json";
        let pinned = key.verifying_key().to_bytes();

        let same = resolve_doc(
            client_id,
            &doc_with_manifest(client_id, &key, serde_json::json!({}), None),
        )
        .await
        .unwrap();
        manifest_key_continues(&same, Some(&pinned)).expect("the same key continues");
        manifest_key_continues(&same, None).expect("nothing pinned, nothing to continue");

        let swapped = resolve_doc(
            client_id,
            &doc_with_manifest(client_id, &rotated, serde_json::json!({}), None),
        )
        .await
        .unwrap();
        let deny = manifest_key_continues(&swapped, Some(&pinned)).expect_err("a rotated key");
        assert_eq!(deny.error, ERR_INVALID_GRANT);
        assert!(deny.description.contains("re-consent"), "{deny:?}");

        let bare = resolve_doc(client_id, &metadata_doc(client_id))
            .await
            .unwrap();
        let deny = manifest_key_continues(&bare, Some(&pinned)).expect_err("a dropped manifest");
        assert_eq!(deny.error, ERR_INVALID_GRANT);
        manifest_key_continues(&bare, None).unwrap();
    }

    /// A resolved client is remembered: the second resolution performs no
    /// fetch. This is the property the positive TTL exists for — a burst of
    /// authorizations for one client costs a single fetch.
    #[tokio::test]
    async fn a_resolved_client_is_cached_and_the_second_call_does_not_fetch() {
        let client_id = "https://client.example/metadata.json";
        let fetcher = FakeFetcher::with(client_id, &metadata_doc(client_id));
        let cache = ClientMetadataCache::new();

        let first = resolve_client(&cache, &fetcher, client_id, 1_000)
            .await
            .expect("resolves");
        assert_eq!(first.client_id, client_id);
        assert_eq!(fetcher.calls(), vec![client_id.to_string()]);

        let second = resolve_client(&cache, &fetcher, client_id, 1_001)
            .await
            .expect("resolves from cache");
        assert_eq!(second.client_id, client_id);
        assert_eq!(
            fetcher.calls(),
            vec![client_id.to_string()],
            "the second resolution fetched again"
        );
    }

    /// A failed fetch is remembered too. Without this a hostile `client_id`
    /// costs one outbound fetch per request — the amplification an anonymous
    /// endpoint must not offer.
    #[tokio::test]
    async fn a_failed_fetch_is_cached_negatively() {
        let client_id = "https://hostile.example/metadata.json";
        let fetcher = FakeFetcher::failing();
        let cache = ClientMetadataCache::new();

        let first = resolve_client(&cache, &fetcher, client_id, 1_000)
            .await
            .expect_err("refuses");
        assert_eq!(first.error, ERR_INVALID_CLIENT);
        assert_eq!(fetcher.calls().len(), 1);

        let second = resolve_client(&cache, &fetcher, client_id, 1_001)
            .await
            .expect_err("refuses from cache");
        assert_eq!(second, first);
        assert_eq!(fetcher.calls().len(), 1, "the refusal was re-fetched");
    }

    /// The negative TTL is a minute and the positive one fifteen, so a client
    /// whose host had a blip recovers far sooner than a good one goes stale.
    #[tokio::test]
    async fn the_negative_entry_expires_before_the_positive_one_would() {
        let client_id = "https://blip.example/metadata.json";
        let fetcher = FakeFetcher::failing();
        let cache = ClientMetadataCache::new();

        resolve_client(&cache, &fetcher, client_id, 1_000)
            .await
            .expect_err("refuses");
        // One second past the negative TTL, and far inside the positive one.
        let later = 1_000 + crate::oauth_as_state::CLIENT_CACHE_NEGATIVE_TTL_SECS + 1;
        assert!(later < 1_000 + crate::oauth_as_state::CLIENT_CACHE_POSITIVE_TTL_SECS);
        resolve_client(&cache, &fetcher, client_id, later)
            .await
            .expect_err("refuses again");
        assert_eq!(
            fetcher.calls().len(),
            2,
            "the negative entry outlived a minute"
        );
    }

    /// A `client_id` the pure planner refuses never reaches the network — and
    /// is still cached, so a flood of malformed identifiers costs one decision
    /// each rather than one per request.
    #[tokio::test]
    async fn a_refused_client_id_never_fetches_and_is_still_cached() {
        let fetcher = FakeFetcher::failing();
        let cache = ClientMetadataCache::new();

        let deny = resolve_client(&cache, &fetcher, "not-a-url", 1_000)
            .await
            .expect_err("refuses");
        assert!(fetcher.calls().is_empty(), "a planner refusal dialled out");

        let again = resolve_client(&cache, &fetcher, "not-a-url", 1_001)
            .await
            .expect_err("refuses from cache");
        assert_eq!(again, deny);
        assert!(fetcher.calls().is_empty());
    }

    /// A public client that happens to publish `jwks_uri` is NOT followed:
    /// `confidential` is the single owner of the question, and following a
    /// public client's URI would hold it to a contract its document never made
    /// — and spend an outbound fetch doing it.
    #[tokio::test]
    async fn a_public_client_with_a_jwks_uri_is_not_followed() {
        let client_id = "https://public.example/metadata.json";
        let doc = serde_json::json!({
            "client_id": client_id,
            "redirect_uris": ["https://public.example/cb"],
            "scope": "atproto",
            "grant_types": ["authorization_code"],
            "response_types": ["code"],
            "application_type": "web",
            "dpop_bound_access_tokens": true,
            "jwks_uri": "https://public.example/jwks.json",
        })
        .to_string();
        let fetcher = FakeFetcher::with(client_id, &doc);
        let cache = ClientMetadataCache::new();

        let client = resolve_client(&cache, &fetcher, client_id, 1_000)
            .await
            .expect("resolves");
        assert!(!client.confidential);
        assert_eq!(
            fetcher.calls(),
            vec![client_id.to_string()],
            "a public client's jwks_uri was followed"
        );
    }

    /// The internal-address half of the guard, with no DNS in the loop: an IP
    /// literal naming loopback is refused before anything is dialled.
    #[tokio::test]
    async fn the_guarded_fetcher_refuses_an_internal_address() {
        let fetcher = GuardedMetadataFetcher;
        for url in [
            "https://127.0.0.1/metadata.json",
            "https://[::1]/metadata.json",
            "https://169.254.169.254/metadata.json",
        ] {
            let err = fetcher.fetch(url).await.expect_err("must refuse");
            assert!(
                matches!(err, MetadataFetchError::Refused(_)),
                "{url} produced {err:?} rather than a guard refusal"
            );
        }
    }

    /// The two rules the **second** guard seat contributes on top of the
    /// address classifier — a non-`https` scheme, and a host that is an IP
    /// literal rather than a public registrable name. Both targets resolve to a
    /// perfectly global address, so `resolve_global_addrs` alone would allow
    /// them; only `check_fetch_target` refuses. Deliberately IP literals, so no
    /// DNS query leaves the box for a target we were never going to dial.
    #[tokio::test]
    async fn the_guarded_fetcher_refuses_what_only_the_policy_seat_can_see() {
        let fetcher = GuardedMetadataFetcher;
        // A globally-routable literal, so the address classifier is satisfied.
        for url in [
            "http://93.184.216.34/metadata.json",
            "https://93.184.216.34/metadata.json",
        ] {
            let err = fetcher.fetch(url).await.expect_err("must refuse");
            assert!(
                matches!(err, MetadataFetchError::Refused(_)),
                "{url} produced {err:?} rather than a guard refusal"
            );
        }
    }
}
