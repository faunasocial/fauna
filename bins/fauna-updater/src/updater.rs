//! What the updater decides and does each minute (`update-channel.md`
//! § The updater): read the channel the nest reports, ask the registry for that
//! channel's head when a check is due, run the shared never-downgrade decision,
//! and on Apply re-create the container on the new build. Every input — the
//! health body, the registry's answer, a label — is data; none of it can name
//! an image other than the one repository the updater follows.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use fauna_provisioning::update_channel::registry::{ChannelHead, Digest, Registry, platform_arch};
use fauna_provisioning::update_channel::{
    ImageIdentity, NEST_IMAGE_REPOSITORY, UPDATE_LABEL, UpdateChannel, UpdateDecision,
    VERSION_LABEL, decide,
};
use serde_json::Value;
use tokio::time::Instant;

use crate::catalogue::{CheckFailure, Event, PullFailure, Settled, Step};
use crate::engine::Engine;
use crate::handoff::{self, HandOff};
use crate::report::Reporter;
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
/// compose default when unset) on loopback under host networking — which the
/// updater shares in the home bundle — else its address on the bundle's
/// network at `:443`. The image binds the nest's own port on container
/// loopback (`deployment-threat-model.md` § Concern 1a), so from the bundle
/// network the one way in is the SNI router, whose no-SNI default route is the
/// nest. Nothing to set: the container already says where it listens.
///
/// TLS first, then plain HTTP — the image's own `HEALTHCHECK` pair: a deployed
/// nest serves TLS on that port (an ACME certificate, or the self-signed
/// bootstrap one a box without a public name synthesizes), which a plain-HTTP
/// read never reaches; a plain-HTTP nest still answers the second. The
/// certificate is not verified ([`health_client`]).
pub fn health_urls(container: &Value) -> Option<[String; 2]> {
    let (host, port) = if container["HostConfig"]["NetworkMode"].as_str() == Some("host") {
        let port = container["Config"]["Env"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .find_map(|e| e.strip_prefix("FAUNA_PORT="))
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(3000);
        ("127.0.0.1".to_string(), port)
    } else {
        let ip = container["NetworkSettings"]["Networks"]
            .as_object()?
            .values()
            .filter_map(|n| n["IPAddress"].as_str())
            .find(|ip| !ip.is_empty())?
            .to_string();
        (ip, 443)
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

/// Per container: the channel last checked and when; and whether a nest's
/// health answered since the loop last asked — the updater's own liveness
/// ([`crate::liveness`]).
#[derive(Default)]
pub struct Checks {
    last: HashMap<String, (UpdateChannel, Instant)>,
    nest_answered: bool,
}

impl Checks {
    /// A check is due on a channel change or once the channel's interval has
    /// passed (a minute on Dev and Test, an hour on Stable —
    /// `UpdateChannel::registry_check_interval`).
    pub fn due(&self, name: &str, channel: UpdateChannel, now: Instant) -> bool {
        match self.last.get(name) {
            Some((last, at)) => {
                *last != channel || now.duration_since(*at) >= channel.registry_check_interval()
            }
            None => true,
        }
    }

    pub fn record(&mut self, name: &str, channel: UpdateChannel, now: Instant) {
        self.last.insert(name.to_string(), (channel, now));
    }

    /// Whether a nest's health answered since the last call.
    pub fn take_nest_answered(&mut self) -> bool {
        std::mem::take(&mut self.nest_answered)
    }
}

/// One minute's pass over every managed container. `me` is the updater's own
/// container id, when it knows it: after moving the nest it moves itself to
/// the same build ([`handoff`]), and the pass ends there. Every transition and
/// decision goes into the container's report ([`Reporter`]).
#[allow(clippy::too_many_arguments)]
pub async fn check_once<E: Engine>(
    engine: &E,
    health_client: &reqwest::Client,
    registry: &Registry,
    repo: &str,
    checks: &mut Checks,
    reporter: &mut Reporter,
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
        checks.nest_answered |= health.is_some();
        if let Some(event) = reporter.nest_health(&name, health.is_some(), Instant::now()) {
            reporter.emit(engine, &id, event).await;
        }
        let channel = match follow(health.as_ref(), Some(&tag)) {
            Follow::Channel(c) => c,
            Follow::Pinned => {
                tracing::info!("{name}: follows no channel (pinned) — holding");
                if let Some(event) = reporter.pinned(&name, &tag) {
                    reporter.emit(engine, &id, event).await;
                }
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
            Ok(head) => {
                if let Some(event) = reporter.registry_succeeded(&name, Instant::now()) {
                    reporter.emit(engine, &id, event).await;
                }
                head
            }
            Err(e) => {
                tracing::warn!("{name}: check skipped: {e}");
                let kind = CheckFailure::of(&e);
                if let Some(event) = reporter.registry_failed(&name, kind, channel, Instant::now())
                {
                    reporter.emit(engine, &id, event).await;
                }
                continue;
            }
        };
        let image_id = container["Image"].as_str().unwrap_or("").to_string();
        let image = engine.inspect_image(&image_id).await?;
        match decide(&running_identity(&image), &head.identity) {
            UpdateDecision::Current => {
                tracing::info!("{name}: current on {}", channel.id());
                reporter.not_held(&name);
                // An updater a failed or interrupted hand-off left behind
                // catches up with the nest here, at the channel's head.
                if let Some(me) = me
                    && follow_the_nest(engine, reporter, &id, me, repo, &image_id).await
                {
                    return Ok(Pass::HandedOff);
                }
            }
            UpdateDecision::Hold { running, head } => {
                tracing::info!(
                    "{name}: following {} from its next build that is at least {running}; its head is {head}",
                    channel.id()
                );
                if let Some(event) =
                    reporter.hold(&name, channel, running.to_string(), head.to_string())
                {
                    reporter.emit(engine, &id, event).await;
                }
            }
            UpdateDecision::Apply => {
                tracing::info!("{name}: applying {} {}", channel.id(), head.digest);
                reporter.not_held(&name);
                let version = head.identity.version.as_ref().map(ToString::to_string);
                let started = Event::ApplyStarted {
                    channel,
                    version: version.clone(),
                    digest: head.digest.clone(),
                };
                reporter.emit(engine, &id, started).await;
                let since = Instant::now();
                let new_id = match apply(engine, &container, &image, repo, &tag, &head).await {
                    Ok(new_id) => new_id,
                    Err(e) => {
                        // The old container is back under its name: its report.
                        reporter
                            .emit(engine, &id, apply_failed(&e, &head.digest))
                            .await;
                        return Err(e.context(format!("{name}: re-create")));
                    }
                };
                tracing::info!("{name}: now runs {}", head.digest);
                reporter.forget(&id);
                let completed = Event::ApplyCompleted {
                    version,
                    digest: head.digest.clone(),
                    took: since.elapsed(),
                };
                reporter.emit(engine, &new_id, completed).await;
                let now_runs = engine
                    .inspect_image(&format!("{repo}:{tag}"))
                    .await
                    .ok()
                    .and_then(|i| i["Id"].as_str().map(str::to_string));
                if let (Some(me), Some(target)) = (me, now_runs)
                    && follow_the_nest(engine, reporter, &new_id, me, repo, &target).await
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

/// Move this updater to `target`, the image the nest `nest` now runs; `true`
/// once the successor runs. A failed hand-off leaves this updater working —
/// and says so in the nest's report — and the next check that finds the nest
/// current tries again. A finished one is reported by the successor.
async fn follow_the_nest<E: Engine>(
    engine: &E,
    reporter: &mut Reporter,
    nest: &str,
    me: &str,
    repo: &str,
    target: &str,
) -> bool {
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
            let failed = Event::SelfUpdateFailed { step: Step::of(&e) };
            reporter.emit(engine, nest, failed).await;
            false
        }
    }
}

/// Report `event` into every managed container (not one left aside): the
/// events that belong to the updater rather than to one container's pass.
pub async fn report_to_managed<E: Engine>(engine: &E, reporter: &mut Reporter, event: Event) {
    let Ok(ids) = engine.labelled(UPDATE_LABEL, "true").await else {
        return;
    };
    for id in ids {
        let Ok(container) = engine.inspect_container(&id).await else {
            continue;
        };
        if !container_name(&container).ends_with(OLD_SUFFIX) {
            reporter.emit(engine, &id, event.clone()).await;
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
/// A failure before the successor runs puts the old container back. The
/// successor's id; a failure carries its [`Step`].
pub async fn apply<E: Engine>(
    engine: &E,
    container: &Value,
    image: &Value,
    repo: &str,
    tag: &str,
    head: &ChannelHead,
) -> Result<String> {
    let pulled = format!("{repo}@{}", head.digest);
    engine.pull(&pulled).await.context(Step::Pull)?;
    engine.tag(&pulled, repo, tag).await.context(Step::Tag)?;
    let reference = format!("{repo}:{tag}");
    let body = successor_body(container, image, &reference);

    let id = container["Id"].as_str().unwrap_or("").to_string();
    let name = container_name(container);
    let aside = format!("{name}{OLD_SUFFIX}");
    engine.rename(&id, &aside).await.context(Step::Rename)?;
    if let Err(e) = engine.stop(&id, stop_timeout(container)).await {
        engine.rename(&id, &name).await.ok();
        return Err(e.context(Step::Stop));
    }
    let mut created = None;
    let outcome = async {
        let new_id = engine.create(&name, body).await.context(Step::Create)?;
        created = Some(new_id.clone());
        engine.start(&new_id).await.context(Step::Start)
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
    Ok(created.unwrap_or_default())
}

/// The report event of a failed [`apply`]: a pull (or the tag after it) is
/// `apply.pull_failed`, every later step `apply.recreate_failed`.
pub fn apply_failed(e: &anyhow::Error, digest: &Digest) -> Event {
    match Step::of(e) {
        Some(Step::Pull | Step::Tag) => Event::PullFailed {
            kind: PullFailure::of(e),
            digest: digest.clone(),
        },
        step => Event::RecreateFailed {
            step: step.unwrap_or(Step::Create),
        },
    }
}

/// Settle a re-create interrupted by a crash or a restart: a container still
/// aside whose successor runs is removed; one whose successor is missing or
/// never started is put back under its name and started. Each is reported to
/// the container left running.
pub async fn reconcile<E: Engine>(engine: &E, reporter: &mut Reporter) -> Result<()> {
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
                let settled = Event::ReconcileSettled {
                    found: Settled::RecreateFinished,
                };
                let successor_id = successor["Id"].as_str().unwrap_or("");
                reporter.emit(engine, successor_id, settled).await;
            }
            successor => {
                if let Some(id) = successor.and_then(|s| s["Id"].as_str()) {
                    tracing::warn!("{base}: a successor that never started — removing it");
                    engine.remove_container(id).await?;
                }
                tracing::warn!("{name}: an unfinished re-create — putting it back");
                engine.rename(aside_id, base).await?;
                engine.start(aside_id).await?;
                let settled = Event::ReconcileSettled {
                    found: Settled::RecreatePutBack,
                };
                reporter.emit(engine, aside_id, settled).await;
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
        // On a bundle network the nest's own port is container loopback, so
        // the read goes to the SNI router's :443 whatever `FAUNA_PORT` says.
        let bridged = json!({
            "Config": {"Env": ["FAUNA_PORT=3100"]},
            "HostConfig": {"NetworkMode": "fauna_default"},
            "NetworkSettings": {"Networks": {"fauna_default": {"IPAddress": "172.18.0.3"}}}
        });
        assert_eq!(
            health_urls(&bridged).unwrap(),
            [
                "https://172.18.0.3:443/api/v1/health",
                "http://172.18.0.3:443/api/v1/health"
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
            digest: fauna_provisioning::update_channel::registry::Digest::of(b"new"),
            identity: ImageIdentity::default(),
            revision: None,
        }
    }

    #[tokio::test]
    async fn apply_steps_aside_stops_within_the_grace_period_and_replaces() {
        let engine = Fake::default();
        let c = nest("old", "fauna-nest", true);
        let head = head();
        apply(
            &engine,
            &c,
            &json!({"Id": "sha256:oldconfig"}),
            "ghcr.io/faunasocial/nest",
            "test",
            &head,
        )
        .await
        .unwrap();
        let digest = &head.digest;
        assert_eq!(
            engine.calls(),
            [
                format!("pull ghcr.io/faunasocial/nest@{digest}").as_str(),
                format!("tag ghcr.io/faunasocial/nest@{digest} ghcr.io/faunasocial/nest:test")
                    .as_str(),
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
        let err = err.unwrap_err();
        assert_eq!(
            engine.calls()[5..],
            ["start new", "rm new", "rename old fauna-nest", "start old"]
        );
        assert_eq!(
            apply_failed(&err, &head().digest),
            Event::RecreateFailed { step: Step::Start }
        );
    }

    #[tokio::test]
    async fn a_failed_pull_is_reported_as_one_and_touches_no_container() {
        let engine = Fake {
            fail: Some("pull"),
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
        .await
        .unwrap_err();
        assert_eq!(engine.calls().len(), 1);
        assert_eq!(
            apply_failed(&err, &head().digest),
            Event::PullFailed {
                kind: PullFailure::Engine,
                digest: head().digest
            }
        );
    }

    #[tokio::test]
    async fn reconcile_puts_back_an_orphan_and_clears_a_finished_one() {
        let orphan = Fake {
            containers: vec![nest("old", "fauna-nest-fauna-updater-old", false)],
            ..Default::default()
        };
        reconcile(&orphan, &mut Reporter::default()).await.unwrap();
        assert_eq!(
            orphan.calls(),
            [
                "rename old fauna-nest",
                "start old",
                "put /var/lib/fauna-updater on old"
            ]
        );
        assert_eq!(orphan.reported("old"), ["reconcile.settled"]);

        let finished = Fake {
            containers: vec![
                nest("old", "fauna-nest-fauna-updater-old", false),
                nest("new", "fauna-nest", true),
            ],
            ..Default::default()
        };
        reconcile(&finished, &mut Reporter::default())
            .await
            .unwrap();
        assert_eq!(
            finished.calls(),
            ["rm old", "put /var/lib/fauna-updater on new"]
        );
        assert_eq!(finished.reported("new"), ["reconcile.settled"]);

        let stillborn = Fake {
            containers: vec![
                nest("old", "fauna-nest-fauna-updater-old", false),
                nest("new", "fauna-nest", false),
            ],
            ..Default::default()
        };
        reconcile(&stillborn, &mut Reporter::default())
            .await
            .unwrap();
        assert_eq!(
            stillborn.calls(),
            [
                "rm new",
                "rename old fauna-nest",
                "start old",
                "put /var/lib/fauna-updater on old"
            ]
        );
    }
}
