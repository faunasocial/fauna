//! What the updater decides and does each minute (`update-channel.md`
//! § The updater): read the channel the nest reports, ask the registry for that
//! channel's head when a check is due, run the shared never-downgrade decision,
//! and on Apply re-create the container on the new build. Every input — the
//! health body, the registry's answer, a label — is data; none of it can name
//! an image other than the one repository the updater follows.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use fauna_provisioning::update_channel::registry::{ChannelHead, Registry, platform_arch};
use fauna_provisioning::update_channel::{
    ImageIdentity, NEST_IMAGE_REPOSITORY, UPDATE_LABEL, UpdateChannel, UpdateDecision,
    VERSION_LABEL, decide,
};
use serde_json::Value;
use tokio::time::Instant;

use crate::engine::Engine;
use crate::handoff::{self, HandOff};
use crate::successor::{container_name, stop_timeout, successor_body};

/// The suffix a container carries between its stop and its successor's start.
/// A container found under it at startup is a re-create the updater did not
/// finish, and [`reconcile`] settles it.
pub const OLD_SUFFIX: &str = "-fauna-updater-old";

/// The one repository the updater follows, as an image reference spells it,
/// on the registry host it reads (`NEST_IMAGE_REGISTRY` in production).
pub fn nest_repository(registry_host: &str) -> String {
    format!("{registry_host}/{NEST_IMAGE_REPOSITORY}")
}

/// The host part of a registry origin (`http://127.0.0.1:5000` →
/// `127.0.0.1:5000`), as an image reference names its registry.
#[cfg_attr(not(feature = "test-hooks"), allow(dead_code))]
pub fn registry_host(origin: &str) -> &str {
    origin
        .split_once("://")
        .map_or(origin, |(_, host)| host)
        .trim_end_matches('/')
}

/// Split an image reference into its repository and its tag (`None` for a
/// digest reference or an untagged one). A registry port is not a tag.
pub fn split_reference(reference: &str) -> (&str, Option<&str>) {
    let without_digest = reference.split('@').next().unwrap_or(reference);
    let has_digest = without_digest.len() != reference.len();
    let last_slash = without_digest.rfind('/').map_or(0, |i| i + 1);
    match without_digest[last_slash..].rfind(':') {
        Some(i) if !has_digest => {
            let at = last_slash + i;
            (&without_digest[..at], Some(&without_digest[at + 1..]))
        }
        Some(i) => (&without_digest[..last_slash + i], None),
        None => (without_digest, None),
    }
}

/// What the running image is, for the decision: its version label and every
/// digest the engine knows it by (its id and the digests it was pulled as).
pub fn running_identity(image: &Value) -> ImageIdentity {
    let mut digests: Vec<String> = image["Id"]
        .as_str()
        .map(str::to_string)
        .into_iter()
        .collect();
    for repo_digest in image["RepoDigests"].as_array().into_iter().flatten() {
        if let Some((_, digest)) = repo_digest.as_str().and_then(|d| d.split_once('@')) {
            digests.push(digest.to_string());
        }
    }
    ImageIdentity {
        version: ImageIdentity::parse_version_label(
            image["Config"]["Labels"][VERSION_LABEL].as_str(),
        ),
        digests,
    }
}

/// The nest's health URLs, from its own inspect: its `FAUNA_PORT` (the
/// compose default when unset), on loopback under host networking — which the
/// updater shares in the home bundle — else at its address on the bundle's
/// network. Nothing to set: the container already says where it listens.
///
/// TLS first, then plain HTTP — the image's own `HEALTHCHECK` pair: a deployed
/// nest serves TLS on that port (an ACME certificate, or the self-signed
/// bootstrap one a box without a public name synthesizes), which a plain-HTTP
/// read never reaches; a plain-HTTP nest still answers the second. The
/// certificate is not verified ([`health_client`]).
pub fn health_urls(container: &Value) -> Option<[String; 2]> {
    let port = container["Config"]["Env"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find_map(|e| e.strip_prefix("FAUNA_PORT="))
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(3000);
    let host = if container["HostConfig"]["NetworkMode"].as_str() == Some("host") {
        "127.0.0.1".to_string()
    } else {
        container["NetworkSettings"]["Networks"]
            .as_object()?
            .values()
            .filter_map(|n| n["IPAddress"].as_str())
            .find(|ip| !ip.is_empty())?
            .to_string()
    };
    Some(["https", "http"].map(|scheme| format!("{scheme}://{host}:{port}/api/v1/health")))
}

/// The client the health read uses: the nest's certificate names its public
/// name (or nothing — the bootstrap one is self-signed), never the bundle
/// address the updater dials, so it is not verified. Nothing rides on it: the
/// body is a selector between Fauna's own channels, read as data, and the
/// never-downgrade decision holds whatever it says (`update-channel.md`
/// § The updater → *What it refuses*).
pub fn health_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(HEALTH_TIMEOUT)
        .danger_accept_invalid_certs(true)
        .build()
}

/// What the updater follows for one container this minute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Follow {
    Channel(UpdateChannel),
    /// No channel: the reference's tag is none and the nest reports none.
    Pinned,
    /// No check this minute, with why.
    Skip(String),
}

/// The channel to follow: the one the nest reports on health; when the field
/// is `null` or absent (a pinned or degraded nest, or one too old to serve
/// it), the tag on the container's own reference. An unreachable nest is a
/// skipped check, never a fallback; a channel id this build does not know is
/// a skipped check too.
pub fn follow(health: Option<&Value>, tag: Option<&str>) -> Follow {
    let Some(health) = health else {
        return Follow::Skip("the nest's health is unreachable".into());
    };
    match &health["update_channel"] {
        Value::String(id) => match UpdateChannel::from_id(id) {
            Some(c) => Follow::Channel(c),
            None => Follow::Skip(format!("the nest reports an unknown channel {id:?}")),
        },
        Value::Null => match tag.and_then(UpdateChannel::from_image_tag) {
            Some(c) => Follow::Channel(c),
            None => Follow::Pinned,
        },
        other => Follow::Skip(format!("the nest reports an unparseable channel {other}")),
    }
}

/// Per container: the channel last checked and when.
#[derive(Default)]
pub struct Checks(HashMap<String, (UpdateChannel, Instant)>);

impl Checks {
    /// A check is due on a channel change or once the channel's interval has
    /// passed (a minute on Dev and Test, an hour on Stable —
    /// `UpdateChannel::registry_check_interval`).
    pub fn due(&self, name: &str, channel: UpdateChannel, now: Instant) -> bool {
        match self.0.get(name) {
            Some((last, at)) => {
                *last != channel || now.duration_since(*at) >= channel.registry_check_interval()
            }
            None => true,
        }
    }

    pub fn record(&mut self, name: &str, channel: UpdateChannel, now: Instant) {
        self.0.insert(name.to_string(), (channel, now));
    }
}

/// One minute's pass over every managed container. `me` is the updater's own
/// container id, when it knows it: after moving the nest it moves itself to
/// the same build ([`handoff`]), and the pass ends there.
pub async fn check_once<E: Engine>(
    engine: &E,
    health_client: &reqwest::Client,
    registry: &Registry,
    repo: &str,
    checks: &mut Checks,
    me: Option<&str>,
) -> Result<Pass> {
    for id in engine.labelled(UPDATE_LABEL, "true").await? {
        let container = engine.inspect_container(&id).await?;
        let name = container_name(&container);
        if name.ends_with(OLD_SUFFIX) {
            continue;
        }
        let reference = container["Config"]["Image"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let (container_repo, tag) = split_reference(&reference);
        if container_repo != repo {
            tracing::warn!("{name}: runs {reference:?}, not {repo} — left alone");
            continue;
        }
        let Some(tag) = tag.map(str::to_string) else {
            tracing::info!("{name}: runs a digest reference — pinned");
            continue;
        };
        let mut health = None;
        for url in health_urls(&container).into_iter().flatten() {
            health = read_health(health_client, &url).await;
            if health.is_some() {
                break;
            }
        }
        let channel = match follow(health.as_ref(), Some(&tag)) {
            Follow::Channel(c) => c,
            Follow::Pinned => {
                tracing::info!("{name}: follows no channel (pinned) — holding");
                continue;
            }
            Follow::Skip(why) => {
                tracing::info!("{name}: check skipped: {why}");
                continue;
            }
        };
        let now = Instant::now();
        if !checks.due(&name, channel, now) {
            continue;
        }
        checks.record(&name, channel, now);
        let head = match registry.head(channel.image_tag(), platform_arch()).await {
            Ok(head) => head,
            Err(e) => {
                tracing::warn!("{name}: check skipped: {e}");
                continue;
            }
        };
        let image_id = container["Image"].as_str().unwrap_or("").to_string();
        let image = engine.inspect_image(&image_id).await?;
        match decide(&running_identity(&image), &head.identity) {
            UpdateDecision::Current => {
                tracing::info!("{name}: current on {}", channel.id());
                // An updater a failed or interrupted hand-off left behind
                // catches up with the nest here, at the channel's head.
                if let Some(me) = me
                    && follow_the_nest(engine, me, repo, &image_id).await
                {
                    return Ok(Pass::HandedOff);
                }
            }
            UpdateDecision::Hold { running, head } => tracing::info!(
                "{name}: following {} from its next build that is at least {running}; its head is {head}",
                channel.id()
            ),
            UpdateDecision::Apply => {
                tracing::info!("{name}: applying {} {}", channel.id(), head.digest);
                apply(engine, &container, &image, repo, &tag, &head)
                    .await
                    .with_context(|| format!("{name}: re-create"))?;
                tracing::info!("{name}: now runs {}", head.digest);
                let now_runs = engine
                    .inspect_image(&format!("{repo}:{tag}"))
                    .await
                    .ok()
                    .and_then(|i| i["Id"].as_str().map(str::to_string));
                if let (Some(me), Some(target)) = (me, now_runs)
                    && follow_the_nest(engine, me, repo, &target).await
                {
                    return Ok(Pass::HandedOff);
                }
            }
        }
    }
    Ok(Pass::Working)
}

/// What a pass leaves the updater as.
#[derive(Debug, PartialEq, Eq)]
pub enum Pass {
    Working,
    /// It handed off to its successor and parks until retired.
    HandedOff,
}

/// Move this updater to `target`, the image the nest now runs; `true` once
/// the successor runs. A failed hand-off leaves this updater working, and the
/// next check that finds the nest current tries again.
async fn follow_the_nest<E: Engine>(engine: &E, me: &str, repo: &str, target: &str) -> bool {
    match handoff::hand_off(engine, me, repo, target).await {
        Ok(HandOff::Done) => {
            tracing::info!("handed off to a successor on {target}");
            true
        }
        Ok(HandOff::Skipped(why)) => {
            tracing::debug!("no self-update: {why}");
            false
        }
        Err(e) => {
            tracing::warn!("self-update: {e:#}");
            false
        }
    }
}

async fn read_health(http: &reqwest::Client, url: &str) -> Option<Value> {
    let resp = http.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body = resp.bytes().await.ok()?;
    match serde_json::from_slice(&body) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!("health at {url} does not parse: {e}");
            None
        }
    }
}

/// The rolling re-create: pull the head by digest, tag it as the container's
/// own reference, step the old container aside and stop it within its grace
/// period (so the nest's WS 1001 drain completes), create and start the
/// successor under the old name, then remove the old container and its image.
/// A failure before the successor runs puts the old container back.
pub async fn apply<E: Engine>(
    engine: &E,
    container: &Value,
    image: &Value,
    repo: &str,
    tag: &str,
    head: &ChannelHead,
) -> Result<()> {
    let pulled = format!("{repo}@{}", head.digest);
    engine.pull(&pulled).await.context("pull")?;
    engine.tag(&pulled, repo, tag).await.context("tag")?;
    let reference = format!("{repo}:{tag}");
    let body = successor_body(container, image, &reference);

    let id = container["Id"].as_str().unwrap_or("").to_string();
    let name = container_name(container);
    let aside = format!("{name}{OLD_SUFFIX}");
    engine.rename(&id, &aside).await.context("rename aside")?;
    if let Err(e) = engine.stop(&id, stop_timeout(container)).await {
        engine.rename(&id, &name).await.ok();
        return Err(e.context("stop"));
    }
    let mut created = None;
    let outcome = async {
        let new_id = engine.create(&name, body).await.context("create")?;
        created = Some(new_id.clone());
        engine.start(&new_id).await.context("start")
    }
    .await;
    if let Err(e) = outcome {
        if let Some(new_id) = created {
            engine.remove_container(&new_id).await.ok();
        }
        engine.rename(&id, &name).await.ok();
        engine.start(&id).await.ok();
        return Err(e);
    }

    if let Err(e) = engine.remove_container(&id).await {
        tracing::warn!("{aside}: not removed: {e:#}");
    }
    let old_image = image["Id"].as_str().unwrap_or("");
    let new_image = engine
        .inspect_image(&reference)
        .await
        .ok()
        .and_then(|i| i["Id"].as_str().map(str::to_string));
    if !old_image.is_empty() && new_image.as_deref() != Some(old_image) {
        // Fails while anything else still runs the old image — the updater
        // itself, whose successor removes it once the predecessor is retired
        // (`handoff::settle`) — which is the rule: removed once nothing runs it.
        if let Err(e) = engine.remove_image(old_image).await {
            tracing::info!("old image {old_image} kept: {e:#}");
        }
    }
    Ok(())
}

/// Settle a re-create interrupted by a crash or a restart: a container still
/// aside whose successor runs is removed; one whose successor is missing or
/// never started is put back under its name and started.
pub async fn reconcile<E: Engine>(engine: &E) -> Result<()> {
    let mut by_name = HashMap::new();
    for id in engine.labelled(UPDATE_LABEL, "true").await? {
        let inspect = engine.inspect_container(&id).await?;
        by_name.insert(container_name(&inspect), inspect);
    }
    for (name, aside) in &by_name {
        let Some(base) = name.strip_suffix(OLD_SUFFIX) else {
            continue;
        };
        let aside_id = aside["Id"].as_str().unwrap_or("");
        match by_name.get(base) {
            Some(successor) if successor["State"]["Running"].as_bool() == Some(true) => {
                tracing::info!("{name}: its successor runs — removing it");
                engine.remove_container(aside_id).await?;
            }
            successor => {
                if let Some(id) = successor.and_then(|s| s["Id"].as_str()) {
                    tracing::warn!("{base}: a successor that never started — removing it");
                    engine.remove_container(id).await?;
                }
                tracing::warn!("{name}: an unfinished re-create — putting it back");
                engine.rename(aside_id, base).await?;
                engine.start(aside_id).await?;
            }
        }
    }
    Ok(())
}

/// How long the updater waits for one health read.
pub const HEALTH_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::fake::Fake;
    use serde_json::json;

    #[test]
    fn references_split_into_repository_and_tag() {
        assert_eq!(
            split_reference("ghcr.io/faunasocial/nest:test"),
            ("ghcr.io/faunasocial/nest", Some("test"))
        );
        assert_eq!(
            split_reference("127.0.0.1:5000/faunasocial/nest:dev"),
            ("127.0.0.1:5000/faunasocial/nest", Some("dev"))
        );
        assert_eq!(
            split_reference("ghcr.io/faunasocial/nest@sha256:abc"),
            ("ghcr.io/faunasocial/nest", None)
        );
        assert_eq!(
            split_reference("ghcr.io/faunasocial/nest:test@sha256:abc"),
            ("ghcr.io/faunasocial/nest", None)
        );
        assert_eq!(
            split_reference("localhost:5000/nest"),
            ("localhost:5000/nest", None)
        );
    }

    #[test]
    fn the_repository_names_the_registry_it_reads() {
        assert_eq!(
            nest_repository(fauna_provisioning::update_channel::NEST_IMAGE_REGISTRY),
            "ghcr.io/faunasocial/nest"
        );
        assert_eq!(registry_host("http://127.0.0.1:5000/"), "127.0.0.1:5000");
        assert_eq!(
            nest_repository(registry_host("http://127.0.0.1:5000")),
            "127.0.0.1:5000/faunasocial/nest"
        );
    }

    #[test]
    fn the_reported_channel_wins_and_null_falls_back_to_the_tag() {
        let reported = json!({"update_channel": "dev"});
        assert_eq!(
            follow(Some(&reported), Some("test")),
            Follow::Channel(UpdateChannel::Dev)
        );
        let null = json!({"update_channel": null});
        assert_eq!(
            follow(Some(&null), Some("test")),
            Follow::Channel(UpdateChannel::Test)
        );
        let old_nest = json!({"version": "0.1.0"});
        assert_eq!(
            follow(Some(&old_nest), Some("latest")),
            Follow::Channel(UpdateChannel::Stable)
        );
        assert_eq!(follow(Some(&null), Some("sha-abc")), Follow::Pinned);
    }

    #[test]
    fn an_unreachable_nest_or_an_unknown_channel_skips_the_check() {
        assert!(matches!(follow(None, Some("test")), Follow::Skip(_)));
        let unknown = json!({"update_channel": "nightly"});
        assert!(matches!(
            follow(Some(&unknown), Some("test")),
            Follow::Skip(_)
        ));
        let junk = json!({"update_channel": {"image": "evil/image:latest"}});
        assert!(matches!(follow(Some(&junk), Some("test")), Follow::Skip(_)));
    }

    #[test]
    fn a_check_is_due_on_a_channel_change_or_after_the_channels_interval() {
        let mut checks = Checks::default();
        let t0 = Instant::now();
        assert!(checks.due("nest", UpdateChannel::Test, t0));
        checks.record("nest", UpdateChannel::Test, t0);
        assert!(!checks.due("nest", UpdateChannel::Test, t0 + Duration::from_secs(30)));
        assert!(checks.due("nest", UpdateChannel::Stable, t0 + Duration::from_secs(30)));
        // A pre-release channel is checked again after a minute …
        assert!(checks.due("nest", UpdateChannel::Test, t0 + Duration::from_secs(60)));
        // … and Stable only after an hour.
        checks.record("nest", UpdateChannel::Stable, t0);
        assert!(!checks.due("nest", UpdateChannel::Stable, t0 + Duration::from_secs(60)));
        assert!(!checks.due(
            "nest",
            UpdateChannel::Stable,
            t0 + Duration::from_secs(59 * 60)
        ));
        assert!(checks.due(
            "nest",
            UpdateChannel::Stable,
            t0 + Duration::from_secs(60 * 60)
        ));
    }

    #[test]
    fn the_running_identity_reads_the_label_and_every_digest() {
        let image = json!({
            "Id": "sha256:config",
            "RepoDigests": ["ghcr.io/faunasocial/nest@sha256:index"],
            "Config": {"Labels": {VERSION_LABEL: "0.3.1"}}
        });
        let identity = running_identity(&image);
        assert_eq!(identity.version.unwrap().to_string(), "0.3.1");
        assert_eq!(identity.digests, ["sha256:config", "sha256:index"]);
        assert_eq!(running_identity(&json!({"Id": "sha256:x"})).version, None);
    }

    #[test]
    fn the_health_urls_come_from_the_containers_own_inspect_tls_first() {
        let bridged = json!({
            "Config": {"Env": ["FAUNA_PORT=3100"]},
            "HostConfig": {"NetworkMode": "fauna_default"},
            "NetworkSettings": {"Networks": {"fauna_default": {"IPAddress": "172.18.0.3"}}}
        });
        assert_eq!(
            health_urls(&bridged).unwrap(),
            [
                "https://172.18.0.3:3100/api/v1/health",
                "http://172.18.0.3:3100/api/v1/health"
            ]
        );
        let host = json!({"Config": {"Env": []}, "HostConfig": {"NetworkMode": "host"}});
        assert_eq!(
            health_urls(&host).unwrap(),
            [
                "https://127.0.0.1:3000/api/v1/health",
                "http://127.0.0.1:3000/api/v1/health"
            ]
        );
    }

    fn nest(id: &str, name: &str, running: bool) -> Value {
        json!({
            "Id": id, "Name": format!("/{name}"), "Image": "sha256:oldconfig",
            "State": {"Running": running},
            "Config": {"Image": "ghcr.io/faunasocial/nest:test", "StopTimeout": 15},
            "HostConfig": {"NetworkMode": "host"},
        })
    }

    fn head() -> ChannelHead {
        ChannelHead {
            digest: "sha256:new".into(),
            identity: ImageIdentity::default(),
        }
    }

    #[tokio::test]
    async fn apply_steps_aside_stops_within_the_grace_period_and_replaces() {
        let engine = Fake::default();
        let c = nest("old", "fauna-nest", true);
        apply(
            &engine,
            &c,
            &json!({"Id": "sha256:oldconfig"}),
            "ghcr.io/faunasocial/nest",
            "test",
            &head(),
        )
        .await
        .unwrap();
        assert_eq!(
            engine.calls(),
            [
                "pull ghcr.io/faunasocial/nest@sha256:new",
                "tag ghcr.io/faunasocial/nest@sha256:new ghcr.io/faunasocial/nest:test",
                "rename old fauna-nest-fauna-updater-old",
                "stop old Some(15)",
                "create fauna-nest ghcr.io/faunasocial/nest:test",
                "start new",
                "rm old",
                "rmi sha256:oldconfig",
            ]
        );
    }

    #[tokio::test]
    async fn a_successor_that_fails_to_start_puts_the_old_container_back() {
        let engine = Fake {
            fail: Some("start new"),
            ..Default::default()
        };
        let c = nest("old", "fauna-nest", true);
        let err = apply(
            &engine,
            &c,
            &json!({"Id": "sha256:oldconfig"}),
            "ghcr.io/faunasocial/nest",
            "test",
            &head(),
        )
        .await;
        assert!(err.is_err());
        assert_eq!(
            engine.calls()[5..],
            ["start new", "rm new", "rename old fauna-nest", "start old"]
        );
    }

    #[tokio::test]
    async fn reconcile_puts_back_an_orphan_and_clears_a_finished_one() {
        let orphan = Fake {
            containers: vec![nest("old", "fauna-nest-fauna-updater-old", false)],
            ..Default::default()
        };
        reconcile(&orphan).await.unwrap();
        assert_eq!(orphan.calls(), ["rename old fauna-nest", "start old"]);

        let finished = Fake {
            containers: vec![
                nest("old", "fauna-nest-fauna-updater-old", false),
                nest("new", "fauna-nest", true),
            ],
            ..Default::default()
        };
        reconcile(&finished).await.unwrap();
        assert_eq!(finished.calls(), ["rm old"]);

        let stillborn = Fake {
            containers: vec![
                nest("old", "fauna-nest-fauna-updater-old", false),
                nest("new", "fauna-nest", false),
            ],
            ..Default::default()
        };
        reconcile(&stillborn).await.unwrap();
        assert_eq!(
            stillborn.calls(),
            ["rm new", "rename old fauna-nest", "start old"]
        );
    }
}
