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
//! `<origin>/<repository>`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use super::{ImageIdentity, NEST_IMAGE_REGISTRY, NEST_IMAGE_REPOSITORY, VERSION_LABEL};

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

/// A channel's head as the registry names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelHead {
    /// The digest the tag points at — what an Apply pulls (`<repo>@<digest>`).
    pub digest: String,
    /// Its version and every digest naming it (the tag's, this platform's
    /// manifest's, the config blob's), for [`super::decide`].
    pub identity: ImageIdentity,
}

/// The registry read for one repository.
#[derive(Debug, Clone)]
pub struct Registry {
    http: reqwest::Client,
    origin: String,
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
            repository: repository.to_string(),
            resolved: Arc::default(),
        }
    }

    /// The production read: the nest image on its public registry.
    pub fn nest(http: reqwest::Client) -> Self {
        Self::new(
            http,
            &format!("https://{NEST_IMAGE_REGISTRY}"),
            NEST_IMAGE_REPOSITORY,
        )
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
        let digest = resp
            .headers()
            .get("docker-content-digest")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .ok_or_else(|| RegistryError::Malformed("no Docker-Content-Digest".into()))?;
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
        let mut digests = vec![digest.clone()];

        let mut manifest: Manifest = self.get_json(&self.manifest_url(&digest), &token).await?;
        if let Some(entries) = manifest.manifests.take() {
            let entry = entries
                .into_iter()
                .find(|m| {
                    m.platform
                        .as_ref()
                        .is_some_and(|p| p.os == "linux" && p.architecture == arch)
                })
                .ok_or_else(|| RegistryError::Malformed(format!("no linux/{arch} manifest")))?;
            digests.push(entry.digest.clone());
            manifest = self
                .get_json(&self.manifest_url(&entry.digest), &token)
                .await?;
        }
        let config = manifest
            .config
            .ok_or_else(|| RegistryError::Malformed("manifest has no config".into()))?;
        digests.push(config.digest.clone());
        let blob_url = format!(
            "{}/v2/{}/blobs/{}",
            self.origin, self.repository, config.digest
        );
        let blob: ConfigBlob = self.get_json(&blob_url, &token).await?;
        let label = blob
            .config
            .and_then(|c| c.labels)
            .and_then(|mut l| l.remove(VERSION_LABEL));
        let head = ChannelHead {
            digest,
            identity: ImageIdentity {
                version: ImageIdentity::parse_version_label(label.as_deref()),
                digests,
            },
        };
        self.resolved
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, head.clone());
        Ok(head)
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

    async fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
        token: &Option<String>,
    ) -> Result<T, RegistryError> {
        let resp = self
            .authed(self.http.get(url), token)
            .header(reqwest::header::ACCEPT, MANIFEST_ACCEPT)
            .send()
            .await?;
        let body = ok_or_status(resp, url)?.bytes().await?;
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
        let resp = self.http.get(&challenge.realm).query(&query).send().await?;
        let token: TokenReply =
            serde_json::from_slice(&ok_or_status(resp, &challenge.realm)?.bytes().await?)
                .map_err(|e| RegistryError::Malformed(format!("token reply: {e}")))?;
        token
            .token
            .or(token.access_token)
            .map(Some)
            .ok_or_else(|| RegistryError::Malformed("token reply carries no token".into()))
    }
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

    const INDEX: &str = "sha256:index";
    const ARM: &str = "sha256:arm64manifest";
    const CONFIG: &str = "sha256:config";

    async fn registry_with(label: Option<&str>, auth: bool) -> MockServer {
        let server = MockServer::start().await;
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
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/token"))
            .and(query_param("scope", "repository:faunasocial/nest:pull"))
            .and(query_param("service", "reg"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"token": "anon"})),
            )
            .mount(&server)
            .await;
        let auth_ok = |m: wiremock::MockBuilder| {
            if auth {
                m.and(header("authorization", "Bearer anon"))
            } else {
                m
            }
        };
        auth_ok(Mock::given(method("HEAD")).and(path("/v2/faunasocial/nest/manifests/test")))
            .respond_with(ResponseTemplate::new(200).insert_header("docker-content-digest", INDEX))
            .mount(&server)
            .await;
        auth_ok(Mock::given(method("GET")).and(path(format!("/v2/faunasocial/nest/manifests/{INDEX}"))))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "mediaType": "application/vnd.oci.image.index.v1+json",
                "manifests": [
                    {"digest": "sha256:amd64manifest", "platform": {"os": "linux", "architecture": "amd64"}},
                    {"digest": ARM, "platform": {"os": "linux", "architecture": "arm64"}},
                ]
            })))
            .mount(&server)
            .await;
        auth_ok(
            Mock::given(method("GET")).and(path(format!("/v2/faunasocial/nest/manifests/{ARM}"))),
        )
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "config": {"digest": CONFIG}
        })))
        .mount(&server)
        .await;
        let labels = match label {
            Some(l) => {
                serde_json::json!({ VERSION_LABEL: l, "org.opencontainers.image.revision": "abc" })
            }
            None => serde_json::json!({ "org.opencontainers.image.revision": "abc" }),
        };
        auth_ok(
            Mock::given(method("GET")).and(path(format!("/v2/faunasocial/nest/blobs/{CONFIG}"))),
        )
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "config": {"Labels": labels}
        })))
        .mount(&server)
        .await;
        server
    }

    #[tokio::test]
    async fn resolves_a_tag_through_the_index_to_its_version_label_with_an_anonymous_token() {
        let server = registry_with(Some("0.3.1"), true).await;
        let reg = Registry::new(reqwest::Client::new(), &server.uri(), NEST_IMAGE_REPOSITORY);
        let head = reg.head("test", "arm64").await.unwrap();
        assert_eq!(head.digest, INDEX);
        assert_eq!(head.identity.version, Some(semver::Version::new(0, 3, 1)));
        assert_eq!(head.identity.digests, [INDEX, ARM, CONFIG]);
    }

    #[tokio::test]
    async fn a_registry_without_auth_and_an_image_without_the_label() {
        let server = registry_with(None, false).await;
        let reg = Registry::new(reqwest::Client::new(), &server.uri(), NEST_IMAGE_REPOSITORY);
        let head = reg.head("test", "arm64").await.unwrap();
        assert_eq!(head.identity.version, None);
    }

    #[tokio::test]
    async fn a_missing_platform_or_an_unknown_tag_is_an_error_not_a_guess() {
        let server = registry_with(Some("0.3.1"), false).await;
        let reg = Registry::new(reqwest::Client::new(), &server.uri(), NEST_IMAGE_REPOSITORY);
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
        let server = registry_with(Some("0.3.1"), true).await;
        let reg = Registry::new(reqwest::Client::new(), &server.uri(), NEST_IMAGE_REPOSITORY);
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
        let server = registry_with(Some("0.3.1"), false).await;
        let reg = Registry::new(reqwest::Client::new(), &server.uri(), NEST_IMAGE_REPOSITORY);
        reg.head("test", "arm64").await.unwrap();
        server.reset().await;
        let moved = "sha256:movedindex";
        Mock::given(method("GET"))
            .and(path("/v2/"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("HEAD"))
            .and(path("/v2/faunasocial/nest/manifests/test"))
            .respond_with(ResponseTemplate::new(200).insert_header("docker-content-digest", moved))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/v2/faunasocial/nest/manifests/{moved}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "config": {"digest": CONFIG}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/v2/faunasocial/nest/blobs/{CONFIG}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "config": {"Labels": {VERSION_LABEL: "0.4.0"}}
            })))
            .mount(&server)
            .await;
        let head = reg.head("test", "arm64").await.unwrap();
        assert_eq!(head.digest, moved);
        assert_eq!(head.identity.version, Some(semver::Version::new(0, 4, 0)));
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
