//! The updater moving itself to the build it just moved the nest to
//! (`update-channel.md` § The updater → *It updates itself, in lockstep*).
//!
//! The updater is not labelled `social.fauna.update`, so the nest re-create
//! never touches it; it finds its own container by id instead. The hand-off
//! reuses the re-create's shape: the predecessor steps aside under
//! [`OLD_SUFFIX`], creates its successor under its own name from the new image
//! with its own configuration ([`successor_body`]), starts it and parks. The
//! successor's first act retires the predecessor by an API stop — an exit
//! alone would be undone by the bundles' `restart: unless-stopped` — then
//! removes it and, once nothing runs it, its image.
//!
//! Every interruption settles to one updater ([`settle`], run at start and on
//! every tick): a predecessor found aside whose successor was created but never
//! started starts it — the newer build wins, since the nest already runs it —
//! and puts itself back only when that start fails or the successor is
//! missing; a parked predecessor whose successor is not running after
//! [`HANDOFF_TIMEOUT`] takes its name back ([`reclaim`]).

use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::engine::Engine;
use crate::successor::{container_name, successor_body};
use crate::updater::{OLD_SUFFIX, split_reference};

/// How long a parked predecessor waits for its successor to retire it before
/// checking that the successor runs at all.
pub const HANDOFF_TIMEOUT: Duration = Duration::from_secs(300);

/// The updater's own container id, from `/proc/self/mountinfo`: the engine
/// bind-mounts `/etc/hostname`, `/etc/hosts` and `/etc/resolv.conf` from the
/// container's own directory (`…/containers/<id>/…`), under host networking
/// too, where the hostname is the host's and names no container.
pub fn own_id_from_mountinfo(mountinfo: &str) -> Option<String> {
    mountinfo.lines().find_map(|line| {
        let (_, rest) = line.split_once("/containers/")?;
        let id = rest.get(..64)?;
        let hex = id.bytes().all(|b| b.is_ascii_hexdigit());
        (hex && rest[64..].starts_with('/')).then(|| id.to_string())
    })
}

/// Whether a container runs steadily — up, and not in a restart loop.
fn runs(container: &Value) -> bool {
    container["State"]["Running"].as_bool() == Some(true)
        && container["State"]["Restarting"].as_bool() != Some(true)
}

fn id_of(container: &Value) -> &str {
    container["Id"].as_str().unwrap_or("")
}

/// A container by name, or `None` when the engine knows none.
async fn by_name<E: Engine>(engine: &E, name: &str) -> Option<Value> {
    engine.inspect_container(name).await.ok()
}

/// What [`settle`] leaves this updater as.
#[derive(Debug, PartialEq, Eq)]
pub enum Settled {
    /// The one updater: it does the checks.
    Working,
    /// A predecessor whose successor runs: it parks until retired.
    Superseded,
}

/// Settle an interrupted or finished hand-off, from this updater's side.
pub async fn settle<E: Engine>(engine: &E, me_id: &str) -> Result<Settled> {
    let me = engine
        .inspect_container(me_id)
        .await
        .context("inspect self")?;
    let name = container_name(&me);
    if let Some(base) = name.strip_suffix(OLD_SUFFIX) {
        match by_name(engine, base).await {
            Some(successor) if runs(&successor) => return Ok(Settled::Superseded),
            Some(successor) => {
                tracing::warn!("{base}: a successor created but never started — starting it");
                match engine.start(id_of(&successor)).await {
                    Ok(()) => return Ok(Settled::Superseded),
                    Err(e) => {
                        tracing::warn!("{base}: the successor does not start: {e:#} — removing it");
                        engine.remove_container(id_of(&successor)).await?;
                    }
                }
            }
            None => tracing::warn!("{name}: a hand-off with no successor — taking the name back"),
        }
        engine.rename(me_id, base).await.context("rename back")?;
        return Ok(Settled::Working);
    }
    let aside = format!("{name}{OLD_SUFFIX}");
    if let Some(predecessor) = by_name(engine, &aside).await {
        let id = id_of(&predecessor);
        tracing::info!("{aside}: retiring the predecessor");
        if let Err(e) = engine.stop(id, None).await {
            tracing::info!("{aside}: stop: {e:#}");
        }
        engine
            .remove_container(id)
            .await
            .context("remove the predecessor")?;
        let old_image = predecessor["Image"].as_str().unwrap_or("");
        if !old_image.is_empty() && me["Image"].as_str() != Some(old_image) {
            // The nest's re-create could not remove this image while the
            // predecessor still ran it; nothing does now.
            if let Err(e) = engine.remove_image(old_image).await {
                tracing::info!("old image {old_image} kept: {e:#}");
            }
        }
    }
    Ok(Settled::Working)
}

/// What [`hand_off`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum HandOff {
    /// The successor runs; this updater parks.
    Done,
    /// Nothing to do, with why.
    Skipped(String),
}

/// Move this updater to `target` (the image id the nest now runs): tag it as
/// the updater's own reference, step aside, create and start the successor
/// under the updater's name. A failure before the successor runs puts this
/// updater back under its name, still working.
pub async fn hand_off<E: Engine>(
    engine: &E,
    me_id: &str,
    repo: &str,
    target: &str,
) -> Result<HandOff> {
    let me = engine
        .inspect_container(me_id)
        .await
        .context("inspect self")?;
    if me["Image"].as_str() == Some(target) {
        return Ok(HandOff::Skipped("already on that build".into()));
    }
    let reference = me["Config"]["Image"].as_str().unwrap_or("").to_string();
    let (my_repo, tag) = split_reference(&reference);
    if my_repo != repo {
        return Ok(HandOff::Skipped(format!("runs {reference:?}, not {repo}")));
    }
    let Some(tag) = tag.map(str::to_string) else {
        return Ok(HandOff::Skipped("runs a digest reference — pinned".into()));
    };
    let my_image_id = me["Image"].as_str().unwrap_or("").to_string();
    let my_image = engine
        .inspect_image(&my_image_id)
        .await
        .context("inspect own image")?;
    engine.tag(target, repo, &tag).await.context("tag")?;
    let body = successor_body(&me, &my_image, &format!("{repo}:{tag}"));

    let name = container_name(&me);
    let aside = format!("{name}{OLD_SUFFIX}");
    engine.rename(me_id, &aside).await.context("rename aside")?;
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
        engine.rename(me_id, &name).await.ok();
        return Err(e);
    }
    Ok(HandOff::Done)
}

/// A parked predecessor's check after [`HANDOFF_TIMEOUT`]: `true` when it took
/// its name back and works again — its successor is gone or not running —
/// `false` while the successor runs.
pub async fn reclaim<E: Engine>(engine: &E, me_id: &str) -> Result<bool> {
    let me = engine
        .inspect_container(me_id)
        .await
        .context("inspect self")?;
    let name = container_name(&me);
    let Some(base) = name.strip_suffix(OLD_SUFFIX) else {
        return Ok(true);
    };
    match by_name(engine, base).await {
        Some(successor) if runs(&successor) => return Ok(false),
        Some(successor) => {
            tracing::warn!("{base}: the successor is not running — taking the name back");
            if let Err(e) = engine.stop(id_of(&successor), None).await {
                tracing::info!("{base}: stop: {e:#}");
            }
            engine.remove_container(id_of(&successor)).await?;
        }
        None => tracing::warn!("{base}: the successor is gone — taking the name back"),
    }
    engine.rename(me_id, base).await.context("rename back")?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::fake::Fake;
    use serde_json::json;

    const REPO: &str = "ghcr.io/faunasocial/nest";

    /// The compose bundle's updater, as `docker inspect` gives it.
    fn updater(id: &str, name: &str, image: &str, running: bool) -> Value {
        json!({
            "Id": id, "Name": format!("/{name}"), "Image": image,
            "State": {"Running": running, "Restarting": false},
            "Config": {
                "Image": "ghcr.io/faunasocial/nest:test",
                "Entrypoint": ["/usr/local/bin/fauna-updater"],
                "Labels": {"com.docker.compose.service": "fauna-updater"}
            },
            "HostConfig": {
                "NetworkMode": "host",
                "RestartPolicy": {"Name": "unless-stopped"},
                "Binds": ["/var/run/docker.sock:/var/run/docker.sock"]
            }
        })
    }

    #[test]
    fn the_own_id_is_read_from_the_bind_mounted_hostname() {
        // The shape of a host-networked container's lines on a ZFS data root;
        // the id is built, not a 64-hex literal (the publish secret scan).
        let id = format!("ef54d3566484{}", "0".repeat(52));
        let mountinfo = format!(
            "8167 7821 0:54 / / rw - overlay overlay rw\n\
             8168 7821 0:54 /containers/{id}/hostname /etc/hostname rw,noatime - zfs work/docker rw,xattr\n"
        );
        assert_eq!(own_id_from_mountinfo(&mountinfo), Some(id));
        assert_eq!(
            own_id_from_mountinfo("8167 7821 0:54 / / rw - overlay\n"),
            None
        );
        assert_eq!(
            own_id_from_mountinfo("1 2 0:5 /containers/short/hostname /etc/hostname rw\n"),
            None
        );
    }

    #[tokio::test]
    async fn the_hand_off_carries_the_updaters_own_configuration_and_keeps_it_working_until_started()
     {
        let engine = Fake {
            containers: vec![updater("me", "fauna-fauna-updater-1", "sha256:old", true)],
            ..Default::default()
        };
        let done = hand_off(&engine, "me", REPO, "sha256:new").await.unwrap();
        assert_eq!(done, HandOff::Done);
        assert_eq!(
            engine.calls(),
            [
                "tag sha256:new ghcr.io/faunasocial/nest:test",
                "rename me fauna-fauna-updater-1-fauna-updater-old",
                "create fauna-fauna-updater-1 ghcr.io/faunasocial/nest:test",
                "start new",
            ],
            "the predecessor is neither stopped nor removed by its own hand"
        );
        let body = engine.created();
        assert_eq!(body["Entrypoint"], json!(["/usr/local/bin/fauna-updater"]));
        assert_eq!(
            body["HostConfig"]["Binds"],
            json!(["/var/run/docker.sock:/var/run/docker.sock"])
        );
        assert_eq!(
            body["HostConfig"]["RestartPolicy"]["Name"],
            "unless-stopped"
        );
        assert_eq!(
            body["Labels"],
            json!({"com.docker.compose.service": "fauna-updater"})
        );
        assert!(
            body["Labels"].get("social.fauna.update").is_none(),
            "the updater is never one of the containers it re-creates"
        );
    }

    #[tokio::test]
    async fn a_successor_that_fails_to_start_leaves_the_predecessor_working() {
        let engine = Fake {
            containers: vec![updater("me", "fauna-fauna-updater-1", "sha256:old", true)],
            fail: Some("start new"),
            ..Default::default()
        };
        assert!(hand_off(&engine, "me", REPO, "sha256:new").await.is_err());
        assert_eq!(
            engine.calls()[3..],
            ["start new", "rm new", "rename me fauna-fauna-updater-1"]
        );
    }

    #[tokio::test]
    async fn no_hand_off_onto_the_running_build_or_outside_the_repository() {
        let engine = Fake {
            containers: vec![updater("me", "fauna-fauna-updater-1", "sha256:new", true)],
            ..Default::default()
        };
        assert!(matches!(
            hand_off(&engine, "me", REPO, "sha256:new").await.unwrap(),
            HandOff::Skipped(_)
        ));
        let mut pinned = updater("me", "fauna-fauna-updater-1", "sha256:old", true);
        pinned["Config"]["Image"] = json!("ghcr.io/faunasocial/nest@sha256:old");
        let mut elsewhere = updater("me2", "u", "sha256:old", true);
        elsewhere["Config"]["Image"] = json!("example.org/other:test");
        let engine = Fake {
            containers: vec![pinned, elsewhere],
            ..Default::default()
        };
        for me in ["me", "me2"] {
            assert!(matches!(
                hand_off(&engine, me, REPO, "sha256:new").await.unwrap(),
                HandOff::Skipped(_)
            ));
        }
        assert!(engine.calls().is_empty());
    }

    #[tokio::test]
    async fn the_successors_first_act_stops_and_removes_the_predecessor_and_its_image() {
        let engine = Fake {
            containers: vec![
                updater(
                    "old",
                    "fauna-fauna-updater-1-fauna-updater-old",
                    "sha256:old",
                    true,
                ),
                updater("new", "fauna-fauna-updater-1", "sha256:new", true),
            ],
            ..Default::default()
        };
        assert_eq!(settle(&engine, "new").await.unwrap(), Settled::Working);
        assert_eq!(
            engine.calls(),
            ["stop old None", "rm old", "rmi sha256:old"]
        );
    }

    #[tokio::test]
    async fn a_lone_updater_settles_to_working_untouched() {
        let engine = Fake {
            containers: vec![updater("me", "fauna-fauna-updater-1", "sha256:new", true)],
            ..Default::default()
        };
        assert_eq!(settle(&engine, "me").await.unwrap(), Settled::Working);
        assert!(engine.calls().is_empty());
    }

    #[tokio::test]
    async fn a_kill_between_create_and_start_settles_to_the_newer_updater() {
        // The predecessor, restarted by the engine, finds itself aside and its
        // successor created but never started: it starts it and parks; the
        // successor's first act then retires it. One updater, on the new build.
        let mut engine = Fake {
            containers: vec![
                updater(
                    "old",
                    "fauna-fauna-updater-1-fauna-updater-old",
                    "sha256:old",
                    true,
                ),
                updater("new", "fauna-fauna-updater-1", "sha256:new", false),
            ],
            ..Default::default()
        };
        assert_eq!(settle(&engine, "old").await.unwrap(), Settled::Superseded);
        assert_eq!(engine.calls(), ["start new"]);

        engine.containers[1]["State"]["Running"] = json!(true);
        assert_eq!(settle(&engine, "new").await.unwrap(), Settled::Working);
        assert_eq!(
            engine.calls()[1..],
            ["stop old None", "rm old", "rmi sha256:old"]
        );
    }

    #[tokio::test]
    async fn a_stillborn_successor_that_will_not_start_gives_the_name_back() {
        let engine = Fake {
            containers: vec![
                updater(
                    "old",
                    "fauna-fauna-updater-1-fauna-updater-old",
                    "sha256:old",
                    true,
                ),
                updater("new", "fauna-fauna-updater-1", "sha256:new", false),
            ],
            fail: Some("start new"),
            ..Default::default()
        };
        assert_eq!(settle(&engine, "old").await.unwrap(), Settled::Working);
        assert_eq!(
            engine.calls(),
            ["start new", "rm new", "rename old fauna-fauna-updater-1"]
        );
    }

    #[tokio::test]
    async fn a_kill_between_rename_and_create_takes_the_name_back() {
        let engine = Fake {
            containers: vec![updater(
                "old",
                "fauna-fauna-updater-1-fauna-updater-old",
                "sha256:old",
                true,
            )],
            ..Default::default()
        };
        assert_eq!(settle(&engine, "old").await.unwrap(), Settled::Working);
        assert_eq!(engine.calls(), ["rename old fauna-fauna-updater-1"]);
    }

    #[tokio::test]
    async fn a_parked_predecessor_waits_on_a_running_successor_and_reclaims_a_dead_one() {
        let mut engine = Fake {
            containers: vec![
                updater(
                    "old",
                    "fauna-fauna-updater-1-fauna-updater-old",
                    "sha256:old",
                    true,
                ),
                updater("new", "fauna-fauna-updater-1", "sha256:new", true),
            ],
            ..Default::default()
        };
        assert!(!reclaim(&engine, "old").await.unwrap());
        assert!(engine.calls().is_empty());

        engine.containers[1]["State"]["Restarting"] = json!(true);
        assert!(reclaim(&engine, "old").await.unwrap());
        assert_eq!(
            engine.calls(),
            [
                "stop new None",
                "rm new",
                "rename old fauna-fauna-updater-1"
            ]
        );
    }
}
