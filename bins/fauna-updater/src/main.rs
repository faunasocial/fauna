//! `fauna-updater` — the Fauna updater: a second binary of the nest image, run
//! as its own container holding the Docker socket and nothing else, that keeps
//! a box on the update channel its nest reports and never moves it onto an
//! older build (`docs/goal/architecture/nest/update-channel.md` § The updater).
//!
//! No arguments, no environment (an e2e build reads one, its registry's
//! origin), no listener: it reads the nest's health every minute, the registry
//! at the channel's interval (a minute on Dev and Test, an hour on Stable) and
//! at once on a channel change, and re-creates the containers labelled
//! `social.fauna.update=true` — then moves itself to the build it moved the
//! nest to (`handoff`).

mod engine;
mod handoff;
mod successor;
mod updater;

use anyhow::Result;
use fauna_provisioning::update_channel::NEST_POLL_INTERVAL;
use fauna_provisioning::update_channel::registry::Registry;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .init();
    let engine = engine::Bollard::connect()?;
    let health = updater::health_client()?;
    let (registry, repository) = registry(
        reqwest::Client::builder()
            .timeout(updater::HEALTH_TIMEOUT)
            .build()?,
    )?;

    let me = own_id(&engine).await;
    if me.is_none() {
        tracing::warn!("cannot find its own container — it will not update itself");
    }

    let mut reconciled = false;
    let mut checks = updater::Checks::default();
    let mut tick = tokio::time::interval(NEST_POLL_INTERVAL);
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = term.recv() => return Ok(()),
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
        let mut parked = false;
        if let Some(me) = &me {
            match handoff::settle(&engine, me).await {
                Ok(handoff::Settled::Working) => {}
                Ok(handoff::Settled::Superseded) => parked = true,
                Err(e) => tracing::warn!("settle: {e:#}"),
            }
        }
        // The re-create reconcile runs once, by the one updater settle left
        // working — never by a predecessor about to be retired.
        if !parked && !reconciled {
            reconciled = true;
            if let Err(e) = updater::reconcile(&engine).await {
                tracing::warn!("reconcile at start: {e:#}");
            }
        }
        if !parked {
            match updater::check_once(
                &engine,
                &health,
                &registry,
                &repository,
                &mut checks,
                me.as_deref(),
            )
            .await
            {
                Ok(updater::Pass::HandedOff) => parked = true,
                Ok(updater::Pass::Working) => {}
                Err(e) => tracing::warn!("check: {e:#}"),
            }
        }
        // Superseded: wait for the successor's stop; work again only if the
        // successor does not run by the time the hand-off should be over.
        while parked {
            tokio::select! {
                _ = tokio::time::sleep(handoff::HANDOFF_TIMEOUT) => {}
                _ = term.recv() => return Ok(()),
                _ = tokio::signal::ctrl_c() => return Ok(()),
            }
            if let Some(me) = &me {
                match handoff::reclaim(&engine, me).await {
                    Ok(reclaimed) => parked = !reclaimed,
                    Err(e) => tracing::warn!("reclaim: {e:#}"),
                }
            }
        }
    }
}

/// This updater's own container id — from the bind mounts the engine gives
/// every container, else from the hostname (the short id, off host
/// networking) — confirmed by an inspect.
async fn own_id(engine: &engine::Bollard) -> Option<String> {
    use engine::Engine;
    let from_mounts = std::fs::read_to_string("/proc/self/mountinfo")
        .ok()
        .and_then(|m| handoff::own_id_from_mountinfo(&m));
    // Under host networking the hostname is the host's, which may name some
    // other container: only a short id is taken from it.
    let from_hostname = std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| h.len() == 12 && h.bytes().all(|b| b.is_ascii_hexdigit()));
    for candidate in from_mounts.into_iter().chain(from_hostname) {
        if let Ok(inspect) = engine.inspect_container(&candidate).await
            && let Some(id) = inspect["Id"].as_str()
            && id.starts_with(&candidate)
        {
            return Some(id.to_string());
        }
    }
    None
}

/// The registry the updater reads and the repository it manages: the nest
/// image on its public registry. An e2e build (`test-hooks`) reads the harness
/// registry `FAUNA_E2E_UPDATE_REGISTRY_ORIGIN` names and manages that
/// registry's repository, and without one refuses to start — an e2e updater
/// never reaches the real registry (the nest's `update_channel::registry` seam).
fn registry(http: reqwest::Client) -> Result<(Registry, String)> {
    #[cfg(feature = "test-hooks")]
    {
        let origin = std::env::var("FAUNA_E2E_UPDATE_REGISTRY_ORIGIN")
            .map_err(|_| anyhow::anyhow!("an e2e build needs FAUNA_E2E_UPDATE_REGISTRY_ORIGIN"))?;
        let repository = updater::nest_repository(updater::registry_host(&origin));
        Ok((
            Registry::new(
                http,
                &origin,
                fauna_provisioning::update_channel::NEST_IMAGE_REPOSITORY,
            ),
            repository,
        ))
    }
    #[cfg(not(feature = "test-hooks"))]
    Ok((
        Registry::nest(http),
        updater::nest_repository(fauna_provisioning::update_channel::NEST_IMAGE_REGISTRY),
    ))
}
