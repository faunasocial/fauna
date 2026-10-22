//! The anonymous registry read that resolves a channel's head: the digests and
//! version label behind one tag of the nest image, without pulling it — the one
//! home shared by the Fauna updater's per-channel check and the nest's
//! `channel_head_version` (`update-channel.md` § The updater → *What it reads*,
//! § The channel as nest state → *What the nest reports*).
//!
//! Three reads of the OCI distribution API: `HEAD` the tag's manifest for its
//! digest, `GET` it (stepping from an index to this platform's manifest), `GET`
//! the config blob for its labels. A registry that answers `401` with a bearer
//! challenge is asked for an anonymous pull token first. Everything the
//! registry returns is data: an unparseable body is an error, never an
//! instruction, and nothing here can name an image other than
//! `<origin>/<repository>`. Every digest the registry supplies is parsed as a
//! [`Digest`] before it reaches a URL path or a pull reference, and each body
//! fetched by digest must hash to the digest that named it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use super::{
    ImageIdentity, NEST_IMAGE_REGISTRY, NEST_IMAGE_REPOSITORY, REVISION_LABEL, VERSION_LABEL,
};

const MANIFEST_ACCEPT: &str = "application/vnd.oci.image.index.v1+json, \
     application/vnd.docker.distribution.manifest.list.v2+json, \
     application/vnd.oci.image.manifest.v1+json, \
     application/vnd.docker.distribution.manifest.v2+json";

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("registry request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("registry answered {status} for {what}")]
    Status {
        status: reqwest::StatusCode,
        what: String,
    },
    #[error("registry reply unusable: {0}")]
    Malformed(String),
}

/// A content digest as the registry names one: `sha256:` and 64 lowercase hex
/// digits, nothing else. The only way to hold a registry-supplied digest, so a
/// URL path or a pull reference built from one cannot carry a path segment, a
/// query or any other text.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Digest(String);

impl Digest {
    const PREFIX: &'static str = "sha256:";

    pub fn parse(raw: &str) -> Result<Self, RegistryError> {
        let well_formed = raw.strip_prefix(Self::PREFIX).is_some_and(|hex| {
            hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        });
        if well_formed {
            Ok(Self(raw.to_string()))
        } else {
            Err(RegistryError::Malformed(format!(
                "digest is not sha256:<64 lowercase hex>: {:?}",
                raw.chars().take(80).collect::<String>()
            )))
        }
    }

    /// The digest of `body`.
    pub fn of(body: &[u8]) -> Self {
        Self(format!(
            "{}{}",
            Self::PREFIX,
            hex::encode(Sha256::digest(body))
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `body` is what this digest names, or an error.
    fn verify(&self, body: &[u8], what: &str) -> Result<(), RegistryError> {
        let actual = Self::of(body);
        if actual == *self {
            Ok(())
        } else {
            Err(RegistryError::Malformed(format!(
                "{what}: body hashes to {actual}, not the {self} that named it"
            )))
        }
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A channel's head as the registry names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelHead {
    /// The digest the tag points at — what an Apply pulls (`<repo>@<digest>`).
    pub digest: Digest,
    /// Its version and every digest naming it (the tag's, this platform's
    /// manifest's, the config blob's), for [`super::decide`].
    pub identity: ImageIdentity,
    /// The commit the build was made from (the [`REVISION_LABEL`]), when it is
    /// one — the identity the nest compares its own build commit against, since
    /// it cannot know its image's digests (`update-channel.md` § What the
    /// updater reports → *The complement*).
    pub revision: Option<String>,
}

/// The registry read for one repository.
#[derive(Debug, Clone)]
pub struct Registry {
    http: reqwest::Client,
    origin: String,
    /// The registry's own origin when [`Self::origin`] is a forwarder in front
    /// of it ([`Self::nest_for_wizard`] on WASM): a bearer-challenge `realm`
    /// naming this origin is re-pointed at the forwarder, as the ACME client
    /// re-points the CA's absolute URLs (`services/fauna-cors-proxy/README.md`).
    direct_origin: Option<String>,
    repository: String,
    /// The last head resolved per `(tag, arch)`. A digest names its content
    /// for good, so a `HEAD` answering the digest remembered here ends the
    /// check there. Clones share it: the updater and the nest each hold one
    /// `Registry` for their lifetime.
    resolved: Arc<Mutex<HashMap<(String, String), ChannelHead>>>,
}

impl Registry {
    /// `origin` is the scheme and host (`https://ghcr.io`); `repository` the
    /// path inside it (`faunasocial/nest`).
    pub fn new(http: reqwest::Client, origin: &str, repository: &str) -> Self {
        Self {
            http,
            origin: origin.trim_end_matches('/').to_string(),
            direct_origin: None,
            repository: repository.to_string(),
            resolved: Arc::default(),
        }
    }

    /// [`Self::new`] behind a forwarder: requests go to `forwarder` (the
    /// registry's paths appended), and a bearer-challenge realm under `direct`
    /// is fetched through `forwarder` too. With `forwarder == direct` this is
    /// exactly [`Self::new`].
    pub fn proxied(http: reqwest::Client, forwarder: &str, direct: &str, repository: &str) -> Self {
        let mut registry = Self::new(http, forwarder, repository);
        let direct = direct.trim_end_matches('/');
        if direct != registry.origin {
            registry.direct_origin = Some(direct.to_string());
        }
        registry
    }

    /// The production read: the nest image on its public registry.
    pub fn nest(http: reqwest::Client) -> Self {
        Self::new(
            http,
            &format!("https://{NEST_IMAGE_REGISTRY}"),
            NEST_IMAGE_REPOSITORY,
        )
    }

    /// The setup wizard's read of the nest image: direct on native, through
    /// the CORS forwarder's `ghcr` route on WASM, where the browser cannot
    /// read ghcr.io cross-origin (`provisioning/registry.md` § Proxy boundary;
    /// `behavior/onboarding-provisioning.md` § 5 → *What exists decides the
    /// default*).
    pub fn nest_for_wizard(http: reqwest::Client) -> Self {
        let direct = format!("https://{NEST_IMAGE_REGISTRY}");
        let base = crate::proxy::default_api_base(&direct, "ghcr");
        Self::proxied(http, &base, &direct, NEST_IMAGE_REPOSITORY)
    }

    /// A challenge realm, re-pointed at the forwarder when it names the
    /// registry this one fronts; any other realm is left as the registry sent
    /// it.
    fn realm_url(&self, realm: &str) -> String {
        match &self.direct_origin {
            Some(direct) => match realm.strip_prefix(direct.as_str()) {
                Some(rest) if rest.is_empty() || rest.starts_with('/') => {
                    format!("{}{rest}", self.origin)
                }
                _ => realm.to_string(),
            },
            None => realm.to_string(),
        }
    }

    /// Resolve `tag` for the `linux/<arch>` platform (`arch` as OCI spells it:
    /// [`platform_arch`]). A tag whose `HEAD` digest is the one last resolved
    /// costs the probe, the token and the `HEAD` only.
    pub async fn head(&self, tag: &str, arch: &str) -> Result<ChannelHead, RegistryError> {
        let token = self.anonymous_token().await?;
        let tag_url = self.manifest_url(tag);
        let resp = self
            .authed(self.http.head(&tag_url), &token)
            .header(reqwest::header::ACCEPT, MANIFEST_ACCEPT)
            .send()
            .await?;
        let resp = ok_or_status(resp, &tag_url)?;
        let digest = Digest::parse(
            resp.headers()
                .get("docker-content-digest")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| RegistryError::Malformed("no Docker-Content-Digest".into()))?,
        )?;
        let key = (tag.to_string(), arch.to_string());
        if let Some(known) = self
            .resolved
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .filter(|known| known.digest == digest)
        {
            return Ok(known.clone());
        }
        let mut digests = vec![digest.to_string()];

        let mut manifest: Manifest = self
            .get_json(&self.manifest_url(digest.as_str()), &digest, &token)
            .await?;
        if let Some(entries) = manifest.manifests.take() {
            let entry = entries
                .into_iter()
                .find(|m| {
                    m.platform
                        .as_ref()
                        .is_some_and(|p| p.os == "linux" && p.architecture == arch)
                })
                .ok_or_else(|| RegistryError::Malformed(format!("no linux/{arch} manifest")))?;
            let entry_digest = Digest::parse(&entry.digest)?;
            digests.push(entry_digest.to_string());
            manifest = self
                .get_json(
                    &self.manifest_url(entry_digest.as_str()),
                    &entry_digest,
                    &token,
                )
                .await?;
        }
        let config = manifest
            .config
            .ok_or_else(|| RegistryError::Malformed("manifest has no config".into()))?;
        let config_digest = Digest::parse(&config.digest)?;
        digests.push(config_digest.to_string());
        let blob_url = format!(
            "{}/v2/{}/blobs/{config_digest}",
            self.origin, self.repository
        );
        let blob: ConfigBlob = self.get_json(&blob_url, &config_digest, &token).await?;
        let mut labels = blob.config.and_then(|c| c.labels).unwrap_or_default();
        let label = labels.remove(VERSION_LABEL);
        let revision = labels.remove(REVISION_LABEL).filter(|r| is_commit(r));
        let head = ChannelHead {
            digest,
            identity: ImageIdentity {
                version: ImageIdentity::parse_version_label(label.as_deref()),
                digests,
            },
            revision,
        };
        self.resolved
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, head.clone());
        Ok(head)
    }

    /// Whether `err` is the registry answering `404` for `tag`'s own manifest
    /// — nothing published under the tag. A `404` on a fetch past it (a
    /// manifest or blob a resolved tag named) is a broken image, not a
    /// missing one.
    pub fn is_missing_tag(&self, err: &RegistryError, tag: &str) -> bool {
        matches!(err, RegistryError::Status { status, what }
            if *status == reqwest::StatusCode::NOT_FOUND && *what == self.manifest_url(tag))
    }

    fn manifest_url(&self, reference: &str) -> String {
        format!(
            "{}/v2/{}/manifests/{reference}",
            self.origin, self.repository
        )
    }

    fn authed(
        &self,
        req: reqwest::RequestBuilder,
        token: &Option<String>,
    ) -> reqwest::RequestBuilder {
        match token {
            Some(t) => req.bearer_auth(t),
            None => req,
        }
    }

    /// GET `url`, require the body to hash to `expected` (the digest that named
    /// it) and only then parse it.
    async fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
        expected: &Digest,
        token: &Option<String>,
    ) -> Result<T, RegistryError> {
        let resp = self
            .authed(self.http.get(url), token)
            .header(reqwest::header::ACCEPT, MANIFEST_ACCEPT)
            .send()
            .await?;
        let body = ok_or_status(resp, url)?.bytes().await?;
        expected.verify(&body, url)?;
        serde_json::from_slice(&body).map_err(|e| RegistryError::Malformed(format!("{url}: {e}")))
    }

    /// Probe `/v2/`: a registry that wants a token says so with a bearer
    /// challenge, answered by an anonymous pull token for this repository; one
    /// that answers `200` (a harness registry on loopback) needs none.
    async fn anonymous_token(&self) -> Result<Option<String>, RegistryError> {
        let probe = format!("{}/v2/", self.origin);
        let resp = self.http.get(&probe).send().await?;
        if resp.status() != reqwest::StatusCode::UNAUTHORIZED {
            ok_or_status(resp, &probe)?;
            return Ok(None);
        }
        let challenge = resp
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_bearer_challenge)
            .ok_or_else(|| RegistryError::Malformed("401 without a bearer challenge".into()))?;
        let scope = format!("repository:{}:pull", self.repository);
        let mut query = vec![("scope", scope.as_str())];
        if let Some(service) = challenge.service.as_deref() {
            query.push(("service", service));
        }
        let realm = self.realm_url(&challenge.realm);
        let resp = self.http.get(&realm).query(&query).send().await?;
        let token: TokenReply = serde_json::from_slice(&ok_or_status(resp, &realm)?.bytes().await?)
            .map_err(|e| RegistryError::Malformed(format!("token reply: {e}")))?;
        token
            .token
            .or(token.access_token)
            .map(Some)
            .ok_or_else(|| RegistryError::Malformed("token reply carries no token".into()))
    }
}

/// A commit as a build stamps one: 7 to 40 lowercase hex digits. Anything
/// else is no revision — a local build's `dev`, or text that is data.
fn is_commit(revision: &str) -> bool {
    (7..=40).contains(&revision.len())
        && revision
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// This build's platform architecture as OCI spells it.
pub fn platform_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        other => other,
    }
}

fn ok_or_status(resp: reqwest::Response, what: &str) -> Result<reqwest::Response, RegistryError> {
    if resp.status().is_success() {
        Ok(resp)
    } else {
        Err(RegistryError::Status {
            status: resp.status(),
            what: what.to_string(),
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
struct BearerChallenge {
    realm: String,
    service: Option<String>,
}

/// `Bearer realm="https://ghcr.io/token",service="ghcr.io",scope="…"`.
fn parse_bearer_challenge(header: &str) -> Option<BearerChallenge> {
    let params = header.trim().strip_prefix("Bearer ")?;
    let mut realm = None;
    let mut service = None;
    for part in params.split(',') {
        let (key, value) = part.trim().split_once('=')?;
        let value = value.trim_matches('"').to_string();
        match key {
            "realm" => realm = Some(value),
            "service" => service = Some(value),
            _ => {}
        }
    }
    let realm = realm?;
    // The realm is a URL the client will call; only http(s) is one.
    if !(realm.starts_with("https://") || realm.starts_with("http://")) {
        return None;
    }
    Some(BearerChallenge { realm, service })
}

#[derive(Deserialize)]
struct TokenReply {
    token: Option<String>,
    access_token: Option<String>,
}

#[derive(Deserialize)]
struct Manifest {
    manifests: Option<Vec<IndexEntry>>,
    config: Option<Descriptor>,
}

#[derive(Deserialize)]
struct IndexEntry {
    digest: String,
    platform: Option<Platform>,
}

#[derive(Deserialize)]
struct Platform {
    os: String,
    architecture: String,
}

#[derive(Deserialize)]
struct Descriptor {
    digest: String,
}

#[derive(Deserialize)]
struct ConfigBlob {
    config: Option<ImageConfig>,
}

#[derive(Deserialize)]
struct ImageConfig {
    #[serde(rename = "Labels")]
    labels: Option<std::collections::HashMap<String, String>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The bodies a registry serves and the real digests that name them.
    struct Fixture {
        index: Digest,
        arm: Digest,
        config: Digest,
        index_body: Vec<u8>,
        arm_body: Vec<u8>,
        config_body: Vec<u8>,
    }

    fn fixture(label: Option<&str>) -> Fixture {
        let labels = match label {
            Some(l) => {
                serde_json::json!({ VERSION_LABEL: l, REVISION_LABEL: "0123abc4" })
            }
            None => serde_json::json!({ REVISION_LABEL: "0123abc4" }),
        };
        let config_body =
            serde_json::to_vec(&serde_json::json!({ "config": {"Labels": labels} })).unwrap();
        let config = Digest::of(&config_body);
        let arm_body = serde_json::to_vec(&serde_json::json!({
            "config": {"digest": config.as_str()}
        }))
        .unwrap();
        let arm = Digest::of(&arm_body);
        let index_body = serde_json::to_vec(&serde_json::json!({
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [
                {"digest": Digest::of(b"amd64").as_str(), "platform": {"os": "linux", "architecture": "amd64"}},
                {"digest": arm.as_str(), "platform": {"os": "linux", "architecture": "arm64"}},
            ]
        }))
        .unwrap();
        Fixture {
            index: Digest::of(&index_body),
            arm,
            config,
            index_body,
            arm_body,
            config_body,
        }
    }

    async fn mount_oauth(server: &MockServer, auth: bool) {
        let v2 = if auth {
            ResponseTemplate::new(401).insert_header(
                "www-authenticate",
                format!(
                    "Bearer realm=\"{}/token\",service=\"reg\",scope=\"repository:faunasocial/nest:pull\"",
                    server.uri()
                )
                .as_str(),
            )
        } else {
            ResponseTemplate::new(200)
        };
        Mock::given(method("GET"))
            .and(path("/v2/"))
            .respond_with(v2)
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/token"))
            .and(query_param("scope", "repository:faunasocial/nest:pull"))
            .and(query_param("service", "reg"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"token": "anon"})),
            )
            .mount(server)
            .await;
    }

    /// Serve `body` at `url_path` for `verb`.
    async fn serve(server: &MockServer, verb: &str, url_path: String, body: &[u8], auth: bool) {
        let m = Mock::given(method(verb)).and(path(url_path));
        let m = if auth {
            m.and(header("authorization", "Bearer anon"))
        } else {
            m
        };
        m.respond_with(ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
            .mount(server)
            .await;
    }

    async fn serve_head(server: &MockServer, digest: &str, auth: bool) {
        let m = Mock::given(method("HEAD")).and(path("/v2/faunasocial/nest/manifests/test"));
        let m = if auth {
            m.and(header("authorization", "Bearer anon"))
        } else {
            m
        };
        m.respond_with(ResponseTemplate::new(200).insert_header("docker-content-digest", digest))
            .mount(server)
            .await;
    }

    async fn mount_image(server: &MockServer, f: &Fixture, auth: bool) {
        serve_head(server, f.index.as_str(), auth).await;
        serve(
            server,
            "GET",
            format!("/v2/faunasocial/nest/manifests/{}", f.index),
            &f.index_body,
            auth,
        )
        .await;
        serve(
            server,
            "GET",
            format!("/v2/faunasocial/nest/manifests/{}", f.arm),
            &f.arm_body,
            auth,
        )
        .await;
        serve(
            server,
            "GET",
            format!("/v2/faunasocial/nest/blobs/{}", f.config),
            &f.config_body,
            auth,
        )
        .await;
    }

    async fn registry_with(label: Option<&str>, auth: bool) -> (MockServer, Fixture) {
        let server = MockServer::start().await;
        let f = fixture(label);
        mount_oauth(&server, auth).await;
        mount_image(&server, &f, auth).await;
        (server, f)
    }

    fn registry(server: &MockServer) -> Registry {
        Registry::new(reqwest::Client::new(), &server.uri(), NEST_IMAGE_REPOSITORY)
    }

    #[tokio::test]
    async fn resolves_a_tag_through_the_index_to_its_version_label_with_an_anonymous_token() {
        let (server, f) = registry_with(Some("0.3.1"), true).await;
        let head = registry(&server).head("test", "arm64").await.unwrap();
        assert_eq!(head.digest, f.index);
        assert_eq!(head.identity.version, Some(semver::Version::new(0, 3, 1)));
        assert_eq!(head.revision.as_deref(), Some("0123abc4"));
        assert_eq!(
            head.identity.digests,
            [f.index.to_string(), f.arm.to_string(), f.config.to_string()]
        );
    }

    /// Behind a forwarder the registry's own token realm is fetched through
    /// the forwarder; a realm on another host is left alone, and a host that
    /// merely starts with the registry's name is another host.
    #[test]
    fn a_proxied_registry_re_points_its_own_realm_at_the_forwarder() {
        let r = Registry::proxied(
            reqwest::Client::new(),
            "https://proxy.example/ghcr/",
            "https://ghcr.io",
            NEST_IMAGE_REPOSITORY,
        );
        assert_eq!(
            r.realm_url("https://ghcr.io/token"),
            "https://proxy.example/ghcr/token"
        );
        assert_eq!(r.realm_url("https://ghcr.io"), "https://proxy.example/ghcr");
        assert_eq!(
            r.realm_url("https://auth.example/token"),
            "https://auth.example/token"
        );
        assert_eq!(
            r.realm_url("https://ghcr.io.evil/token"),
            "https://ghcr.io.evil/token"
        );
        let direct = Registry::proxied(
            reqwest::Client::new(),
            "https://ghcr.io",
            "https://ghcr.io",
            NEST_IMAGE_REPOSITORY,
        );
        assert_eq!(
            direct.realm_url("https://ghcr.io/token"),
            "https://ghcr.io/token"
        );
        // On a native build the wizard dials the registry directly.
        let wizard = Registry::nest_for_wizard(reqwest::Client::new());
        assert_eq!(wizard.origin, "https://ghcr.io");
        assert_eq!(wizard.direct_origin, None);
    }

    /// The forwarded token fetch end to end: the challenge names the
    /// "registry's" origin, and the token is asked of the forwarder.
    #[tokio::test]
    async fn a_proxied_registry_fetches_its_token_through_the_forwarder() {
        let forwarder = MockServer::start().await;
        let f = fixture(Some("0.1.4"));
        Mock::given(method("GET"))
            .and(path("/v2/"))
            .respond_with(ResponseTemplate::new(401).insert_header(
                "www-authenticate",
                "Bearer realm=\"https://registry.invalid/token\",service=\"reg\"",
            ))
            .mount(&forwarder)
            .await;
        Mock::given(method("GET"))
            .and(path("/token"))
            .and(query_param("service", "reg"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"token": "anon"})),
            )
            .mount(&forwarder)
            .await;
        mount_image(&forwarder, &f, true).await;
        let r = Registry::proxied(
            reqwest::Client::new(),
            &forwarder.uri(),
            "https://registry.invalid",
            NEST_IMAGE_REPOSITORY,
        );
        let head = r.head("test", "arm64").await.unwrap();
        assert_eq!(head.identity.version, Some(semver::Version::new(0, 1, 4)));
    }

    #[test]
    fn a_revision_is_a_commit_or_nothing() {
        assert!(is_commit("0123abc4"));
        assert!(is_commit(&"a".repeat(40)));
        for bad in ["dev", "abc", "0123ABC4", "0123abc4/../x", &"a".repeat(41)] {
            assert!(!is_commit(bad), "{bad}");
        }
    }

    #[tokio::test]
    async fn a_registry_without_auth_and_an_image_without_the_label() {
        let (server, _) = registry_with(None, false).await;
        let head = registry(&server).head("test", "arm64").await.unwrap();
        assert_eq!(head.identity.version, None);
    }

    #[tokio::test]
    async fn a_missing_platform_or_an_unknown_tag_is_an_error_not_a_guess() {
        let (server, _) = registry_with(Some("0.3.1"), false).await;
        let reg = registry(&server);
        assert!(matches!(
            reg.head("test", "riscv64").await,
            Err(RegistryError::Malformed(_))
        ));
        assert!(matches!(
            reg.head("dev", "arm64").await,
            Err(RegistryError::Status { .. })
        ));
    }

    /// Requests the mock server saw, as `METHOD path`.
    async fn seen(server: &MockServer) -> Vec<String> {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| format!("{} {}", r.method, r.url.path()))
            .collect()
    }

    #[tokio::test]
    async fn an_unmoved_tag_ends_at_the_head_so_a_steady_check_is_three_requests() {
        let (server, _) = registry_with(Some("0.3.1"), true).await;
        let reg = registry(&server);
        let first = reg.head("test", "arm64").await.unwrap();
        assert_eq!(seen(&server).await.len(), 6);
        let second = reg.head("test", "arm64").await.unwrap();
        assert_eq!(second, first);
        let all = seen(&server).await;
        assert_eq!(
            all[6..],
            [
                "GET /v2/",
                "GET /token",
                "HEAD /v2/faunasocial/nest/manifests/test"
            ]
        );
        // A clone shares what was learned.
        reg.clone().head("test", "arm64").await.unwrap();
        assert_eq!(seen(&server).await.len(), 12);
    }

    #[tokio::test]
    async fn a_moved_digest_resolves_afresh() {
        let (server, f) = registry_with(Some("0.3.1"), false).await;
        let reg = registry(&server);
        reg.head("test", "arm64").await.unwrap();
        server.reset().await;
        // A single-platform manifest: the tag points at it directly.
        let moved_config = serde_json::to_vec(&serde_json::json!({
            "config": {"Labels": {VERSION_LABEL: "0.4.0"}}
        }))
        .unwrap();
        let moved_config_digest = Digest::of(&moved_config);
        let moved_body = serde_json::to_vec(&serde_json::json!({
            "config": {"digest": moved_config_digest.as_str()}
        }))
        .unwrap();
        let moved = Digest::of(&moved_body);
        assert_ne!(moved, f.index);
        mount_oauth(&server, false).await;
        serve_head(&server, moved.as_str(), false).await;
        serve(
            &server,
            "GET",
            format!("/v2/faunasocial/nest/manifests/{moved}"),
            &moved_body,
            false,
        )
        .await;
        serve(
            &server,
            "GET",
            format!("/v2/faunasocial/nest/blobs/{moved_config_digest}"),
            &moved_config,
            false,
        )
        .await;
        let head = reg.head("test", "arm64").await.unwrap();
        assert_eq!(head.digest, moved);
        assert_eq!(head.identity.version, Some(semver::Version::new(0, 4, 0)));
    }

    #[test]
    fn a_digest_is_sha256_and_64_lowercase_hex_or_nothing() {
        let good = format!("sha256:{}", "ab01".repeat(16));
        assert_eq!(Digest::parse(&good).unwrap().as_str(), good);
        for bad in [
            "",
            "sha256:",
            "sha256:index",
            "sha512:abcd",
            &format!("sha256:{}", "AB01".repeat(16)),
            &format!("sha256:{}0", "ab01".repeat(16)),
            &format!("sha256:{}", "ab01".repeat(15)),
            &format!("sha256:{}/../x", "ab01".repeat(16)),
            "sha256:../../other/manifests/x",
            &format!(" sha256:{}", "ab01".repeat(16)),
        ] {
            assert!(
                matches!(Digest::parse(bad), Err(RegistryError::Malformed(_))),
                "{bad:?} must be refused"
            );
        }
    }

    #[tokio::test]
    async fn a_head_naming_a_traversal_is_an_error_and_no_path_beyond_the_head_is_fetched() {
        let server = MockServer::start().await;
        mount_oauth(&server, false).await;
        serve_head(&server, "sha256:../../other/manifests/x", false).await;
        let err = registry(&server).head("test", "arm64").await.unwrap_err();
        assert!(matches!(err, RegistryError::Malformed(_)), "{err}");
        assert_eq!(
            seen(&server).await,
            ["GET /v2/", "HEAD /v2/faunasocial/nest/manifests/test"]
        );
    }

    #[tokio::test]
    async fn an_index_entry_or_config_naming_a_malformed_digest_is_refused_before_the_fetch() {
        for which in ["entry", "config"] {
            let server = MockServer::start().await;
            mount_oauth(&server, false).await;
            // Each body is served under the digest that really names it, so
            // only the malformed inner digest can fail the read.
            let (index_body, arm_body) = if which == "entry" {
                let index = serde_json::to_vec(&serde_json::json!({
                    "manifests": [{"digest": "sha256:../../x", "platform": {"os": "linux", "architecture": "arm64"}}]
                }))
                .unwrap();
                (index, Vec::new())
            } else {
                let arm = serde_json::to_vec(&serde_json::json!({
                    "config": {"digest": "sha256:../../x"}
                }))
                .unwrap();
                let index = serde_json::to_vec(&serde_json::json!({
                    "manifests": [{"digest": Digest::of(&arm).as_str(), "platform": {"os": "linux", "architecture": "arm64"}}]
                }))
                .unwrap();
                (index, arm)
            };
            let index_digest = Digest::of(&index_body);
            serve_head(&server, index_digest.as_str(), false).await;
            serve(
                &server,
                "GET",
                format!("/v2/faunasocial/nest/manifests/{index_digest}"),
                &index_body,
                false,
            )
            .await;
            if !arm_body.is_empty() {
                serve(
                    &server,
                    "GET",
                    format!("/v2/faunasocial/nest/manifests/{}", Digest::of(&arm_body)),
                    &arm_body,
                    false,
                )
                .await;
            }
            let err = registry(&server).head("test", "arm64").await.unwrap_err();
            assert!(matches!(err, RegistryError::Malformed(_)), "{which}: {err}");
            // The exact request list, not a search for `..`: URL parsing
            // resolves dot segments before the request leaves, so a traversed
            // fetch never shows one.
            let mut expected = vec![
                "GET /v2/".to_string(),
                "HEAD /v2/faunasocial/nest/manifests/test".to_string(),
                format!("GET /v2/faunasocial/nest/manifests/{index_digest}"),
            ];
            if !arm_body.is_empty() {
                expected.push(format!(
                    "GET /v2/faunasocial/nest/manifests/{}",
                    Digest::of(&arm_body)
                ));
            }
            assert_eq!(
                seen(&server).await,
                expected,
                "{which}: a traversed path was requested"
            );
        }
    }

    #[tokio::test]
    async fn a_body_that_does_not_hash_to_the_digest_that_named_it_is_an_error() {
        for tampered in ["index", "arm", "config"] {
            let server = MockServer::start().await;
            let mut f = fixture(Some("0.3.1"));
            // Same JSON shape, different bytes: only the hash can tell.
            match tampered {
                "index" => f.index_body.push(b' '),
                "arm" => f.arm_body.push(b' '),
                _ => {
                    f.config_body = serde_json::to_vec(&serde_json::json!({
                        "config": {"Labels": {VERSION_LABEL: "9.9.9"}}
                    }))
                    .unwrap()
                }
            }
            mount_oauth(&server, false).await;
            mount_image(&server, &f, false).await;
            let err = registry(&server).head("test", "arm64").await.unwrap_err();
            assert!(
                matches!(&err, RegistryError::Malformed(m) if m.contains("hashes to")),
                "{tampered}: {err}"
            );
        }
    }

    #[test]
    fn a_bearer_challenge_names_an_http_realm_or_nothing() {
        assert_eq!(
            parse_bearer_challenge(
                r#"Bearer realm="https://ghcr.io/token",service="ghcr.io",scope="repository:faunasocial/nest:pull""#
            ),
            Some(BearerChallenge {
                realm: "https://ghcr.io/token".into(),
                service: Some("ghcr.io".into()),
            })
        );
        assert_eq!(parse_bearer_challenge(r#"Basic realm="x""#), None);
        assert_eq!(
            parse_bearer_challenge(r#"Bearer realm="file:///etc/passwd""#),
            None
        );
        assert_eq!(parse_bearer_challenge(r#"Bearer service="x""#), None);
    }
}
