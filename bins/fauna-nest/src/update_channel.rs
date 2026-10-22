//! The update channel as nest state: which builds this box's updater follows,
//! the `fauna.admin.update_channel.{get,set}` kinds that read and choose it, and
//! the value `/api/v1/health` reports for the updater — and the nest's own
//! `update overdue` line (§ What the updater reports → *The complement*).
//!
//! Owner: `docs/goal/architecture/nest/update-channel.md` § The channel as nest
//! state. The channel in force is the admin's `nest_update_channel` row, else the
//! seed the install's image tag gave (`FAUNA_IMAGE_TAG`, read into
//! `config.nest.image_tag` at boot; `UpdateChannel::seeded_from` is the rule) —
//! `None` is a *pinned* box. The mapping from a channel to an image tag stays
//! `fauna_provisioning::update_channel::UpdateChannel`'s. **Where the channel
//! applies** (that doc's paragraph of that name): only on a nest run from the
//! image, whose bundle runs the updater — told apart by the variable's
//! *presence* ([`enactable`]); a nest the image does not run follows no
//! channel whatever the row says, refuses `set`, spawns no registry watch and
//! reports `enactable: false` on the read.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use fauna_protocol::update_channel::{
    AdminUpdateChannelGetReply, AdminUpdateChannelGetRequest, AdminUpdateChannelSetReply,
    AdminUpdateChannelSetRequest,
};
use fauna_provisioning::update_channel::registry::{self, Registry};
use fauna_provisioning::update_channel::{ImageIdentity, UpdateChannel, UpdateDecision, decide};

use crate::routes::AppState;
use crate::rpc_errors::internal;

/// Whether a channel can be enacted on this box: the install's image tag is in
/// the environment (`config.nest.image_tag`, from `FAUNA_IMAGE_TAG`), which
/// every bundle sets and nothing else does — so its presence says "run from
/// the image, under an updater", its absence a Windows service, a macOS
/// daemon, a Linux package or a local build, where no updater holds the socket
/// (`nest/update-channel.md` § The channel as nest state → *Where the channel
/// applies*). Artifact-set IPC, bucket 1 of `principles.md` § One
/// configuration surface — the shape `FAUNA_FRONTED_BY_ROUTER` has. Not the
/// updater's report file: that is written only by a live updater, and the
/// overdue watch exists for the dead or absent one.
pub fn enactable(state: &AppState) -> bool {
    state.config.nest.image_tag.is_some()
}

/// Resolve the channel at boot: a present, readable row wins; absent — or
/// unreadable, which is logged — follows the seed (`seed_tag`, the install's
/// image tag), so a box always boots following what it was installed from. No
/// `seed_tag` at all is a nest the image does not run ([`enactable`]): no
/// channel applies there, whatever row an earlier build may have written.
pub async fn resolve_update_channel(
    db: &crate::db::CacheDb,
    seed_tag: Option<&str>,
) -> Option<UpdateChannel> {
    seed_tag?;
    match db.get_update_channel().await {
        Ok(Some(channel)) => Some(channel),
        Ok(None) => UpdateChannel::seeded_from(seed_tag),
        Err(e) => {
            tracing::error!(
                "get_update_channel at boot failed: {e:#}; following the image-tag seed"
            );
            UpdateChannel::seeded_from(seed_tag)
        }
    }
}

/// The single write path: upsert the row (the atomic decision point), then swap
/// the live value so health and the read report it at once.
pub async fn apply_update_channel_change(
    state: &AppState,
    channel: UpdateChannel,
) -> anyhow::Result<()> {
    state.db.set_update_channel(channel).await?;
    state.update_channel.store(Arc::new(Some(channel)));
    Ok(())
}

/// The channel in force, as the id `/api/v1/health` serves (`None` ⇒ pinned,
/// or a nest the image does not run).
pub fn reported(state: &AppState) -> Option<&'static str> {
    (**state.update_channel.load()).map(UpdateChannel::id)
}

/// The projection `fauna.admin.update_channel.get` answers. The channel head's
/// version is resolved from the registry on the read through the one shared
/// client (`fauna_provisioning::update_channel::registry`, the updater's own),
/// cached for [`CHANNEL_HEAD_TTL`], and absent whenever it cannot be had — the
/// read never fails on it (`update-channel.md` § The channel as nest state →
/// *What the nest reports*).
pub async fn projection(state: &AppState) -> AdminUpdateChannelGetReply {
    let channel = **state.update_channel.load();
    AdminUpdateChannelGetReply {
        channel: channel.map(|c| c.id().to_string()),
        running_version: env!("CARGO_PKG_VERSION").to_string(),
        channel_head_version: match channel {
            Some(channel) => channel_head(channel).await.and_then(|h| h.version),
            None => None,
        },
        enactable: enactable(state),
        extra: Default::default(),
    }
}

/// How long a resolved channel head (or a failure to resolve one) is reused.
const CHANNEL_HEAD_TTL: Duration = Duration::from_secs(5 * 60);

/// The registry read's budget, inside the kind's 5 s deadline.
const CHANNEL_HEAD_TIMEOUT: Duration = Duration::from_secs(3);

/// What the nest knows of a channel's head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    /// The digest the channel's tag names — what "once per channel head" keys.
    pub digest: String,
    /// Its version label, as the read serves it.
    pub version: Option<String>,
    /// The commit it was built from (its revision label).
    pub revision: Option<String>,
}

struct CachedHead {
    channel: UpdateChannel,
    at: Instant,
    head: Option<Head>,
}

static CHANNEL_HEAD: Mutex<Option<CachedHead>> = Mutex::new(None);

/// The channel's head, cached for [`CHANNEL_HEAD_TTL`] (a failure too) and
/// `None` whenever it cannot be had.
async fn channel_head(channel: UpdateChannel) -> Option<Head> {
    if let Some(hit) = CHANNEL_HEAD
        .lock()
        .ok()?
        .as_ref()
        .filter(|c| c.channel == channel && c.at.elapsed() < CHANNEL_HEAD_TTL)
    {
        return hit.head.clone();
    }
    let registry = registry()?;
    let head = match tokio::time::timeout(
        CHANNEL_HEAD_TIMEOUT,
        registry.head(channel.image_tag(), registry::platform_arch()),
    )
    .await
    {
        Ok(Ok(head)) => Some(Head {
            digest: head.digest.to_string(),
            version: head.identity.version.map(|v| v.to_string()),
            revision: head.revision,
        }),
        Ok(Err(e)) => {
            tracing::info!("update channel head unresolved: {e}");
            None
        }
        Err(_) => {
            tracing::info!("update channel head unresolved: the registry timed out");
            None
        }
    };
    if let Ok(mut slot) = CHANNEL_HEAD.lock() {
        *slot = Some(CachedHead {
            channel,
            at: Instant::now(),
            head: head.clone(),
        });
    }
    head
}

// ── update overdue ───────────────────────────────────────────────────────────

/// How long past the channel's check interval an Apply may take before the
/// nest says it is overdue: the serving gate's budget for a box to adopt a
/// staged build. Bucket 1.
pub const UPDATE_OVERDUE_ALLOWANCE: Duration = Duration::from_secs(10 * 60);

/// The allowance in force: [`UPDATE_OVERDUE_ALLOWANCE`], or in an e2e build
/// (`test-hooks`) the seconds `FAUNA_E2E_UPDATE_OVERDUE_ALLOWANCE_S` names.
fn overdue_allowance() -> Duration {
    #[cfg(feature = "test-hooks")]
    if let Some(s) = std::env::var("FAUNA_E2E_UPDATE_OVERDUE_ALLOWANCE_S")
        .ok()
        .and_then(|s| s.parse().ok())
    {
        return Duration::from_secs(s);
    }
    UPDATE_OVERDUE_ALLOWANCE
}

/// Whether the shared decision says this nest should move to `head`. The nest
/// cannot know its own image's digests, so "the same build" is asked of the
/// commit: the head's revision label against the commit this binary was built
/// from (`commit`; prefix-tolerant, as the two stamps differ in length). A
/// local build (`dev`) or a head without a revision cannot tell the same build
/// from a newer one, and says nothing — never a false "overdue". Otherwise the
/// never-downgrade rule on the versions alone: Apply, or Hold.
pub fn says_apply(commit: &str, running_version: &str, head: &Head) -> bool {
    let Some(revision) = head.revision.as_deref() else {
        return false;
    };
    if commit == "dev" || commit.starts_with(revision) || revision.starts_with(commit) {
        return false;
    }
    let identity = |v: Option<&str>| ImageIdentity {
        version: ImageIdentity::parse_version_label(v),
        digests: Vec::new(),
    };
    decide(
        &identity(Some(running_version)),
        &identity(head.version.as_deref()),
    ) == UpdateDecision::Apply
}

/// The overdue clock: since when the decision has said Apply for the head it
/// names, and whether that head was warned about.
#[derive(Debug, Default)]
pub struct Overdue {
    head: Option<String>,
    since: Option<Instant>,
    warned: bool,
}

impl Overdue {
    /// Observe the decision at `now`; `true` exactly once per channel head —
    /// when it has said Apply for longer than `after`.
    pub fn observe(&mut self, head: &str, apply: bool, now: Instant, after: Duration) -> bool {
        if self.head.as_deref() != Some(head) {
            *self = Self {
                head: Some(head.to_string()),
                ..Self::default()
            };
        }
        if !apply {
            self.since = None;
            return false;
        }
        let since = *self.since.get_or_insert(now);
        if self.warned || now.duration_since(since) <= after {
            return false;
        }
        self.warned = true;
        true
    }
}

/// One tick of the watch: resolve the head (through the read's cache) and warn
/// once per head that has waited past the channel's interval plus the
/// allowance. A pinned box, an unresolved head: nothing.
async fn overdue_once(state: &AppState, clock: &Mutex<Overdue>) {
    let Some(channel) = **state.update_channel.load() else {
        return;
    };
    let Some(head) = channel_head(channel).await else {
        return;
    };
    let running = env!("CARGO_PKG_VERSION");
    let apply = says_apply(crate::build_identity::commit(), running, &head);
    let after = channel.registry_check_interval() + overdue_allowance();
    let warn = clock
        .lock()
        .map(|mut c| c.observe(&head.digest, apply, Instant::now(), after))
        .unwrap_or(false);
    if warn {
        tracing::warn!(
            target: "update_channel",
            "update overdue: {} names {} and this box still runs {running} after {} min — the updater has not applied it",
            channel.id(),
            head.version.as_deref().unwrap_or("an unlabelled build"),
            after.as_secs() / 60
        );
    }
}

/// The overdue watch, scoped to the serving generation: a tick every
/// [`CHANNEL_HEAD_TTL`] (no more often than the allowance, so a shortened e2e
/// allowance is seen), the head resolved on it rather than only on the admin
/// read.
pub fn spawn_update_overdue_watch(state: Arc<AppState>) {
    let tick = CHANNEL_HEAD_TTL
        .min(overdue_allowance())
        .max(Duration::from_secs(1));
    let clock = Arc::new(Mutex::new(Overdue::default()));
    let scope = state.clone();
    scope.scope_handle(crate::sweeper::spawn_periodic_sweeper(
        tick,
        false,
        move || {
            let state = state.clone();
            let clock = clock.clone();
            async move { overdue_once(&state, &clock).await }
        },
    ));
}

/// The registry the head is read from: the nest image's public registry in
/// production. An e2e build (`test-hooks`) reads the harness's registry named
/// by `FAUNA_E2E_UPDATE_REGISTRY_ORIGIN` and, without one, none — a test nest
/// never reaches the real registry. The harness registry serves TLS under its
/// own throwaway CA, which `FAUNA_E2E_UPDATE_REGISTRY_CA` carries as PEM (the
/// updater's twin seam).
fn registry() -> Option<&'static Registry> {
    static REGISTRY: OnceLock<Option<Registry>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| {
            let http = reqwest::Client::builder().timeout(CHANNEL_HEAD_TIMEOUT);
            #[cfg(feature = "test-hooks")]
            {
                let origin = std::env::var("FAUNA_E2E_UPDATE_REGISTRY_ORIGIN").ok()?;
                let http = match std::env::var("FAUNA_E2E_UPDATE_REGISTRY_CA") {
                    Ok(pem) => http
                        .add_root_certificate(reqwest::Certificate::from_pem(pem.as_bytes()).ok()?),
                    Err(_) => http,
                };
                Some(Registry::new(
                    http.build().ok()?,
                    &origin,
                    fauna_provisioning::update_channel::NEST_IMAGE_REPOSITORY,
                ))
            }
            #[cfg(not(feature = "test-hooks"))]
            Some(Registry::nest(http.build().ok()?))
        })
        .as_ref()
}

// ── fauna.admin.update_channel.{get,set} ─────────────────────────────────────

pub const KIND_UPDATE_CHANNEL_GET: &str = "fauna.admin.update_channel.get";
pub const KIND_UPDATE_CHANNEL_SET: &str = "fauna.admin.update_channel.set";

const AUDIT_UPDATE_CHANNEL_SET: &str = "admin:update_channel.set";

/// This module's admin-plane kinds for an admin-consented principal —
/// the nest page's update channel — each its actor handler behind
/// [`crate::principal_handlers::admin_twin`] (`authorization-server.md`
/// § Scope grammar → *The admin arms* names the set).
pub(crate) fn principal_admin_handlers()
-> Vec<(&'static str, crate::principal_handlers::PrincipalHandler)> {
    use crate::principal_handlers::admin_twin;
    let kinds: [(&'static str, crate::rpc_router::RpcHandler); 2] = [
        ("fauna.admin.update_channel.get", get_handler()),
        ("fauna.admin.update_channel.set", set_handler()),
    ];
    kinds
        .into_iter()
        .map(|(kind, actor)| (kind, admin_twin(kind, actor)))
        .collect()
}

pub fn register_update_channel_handlers(b: &mut crate::rpc_router::RpcRouterBuilder) {
    // Mirror any change in `KindRegistry::register_admin_kinds`.
    b.add(
        KIND_UPDATE_CHANNEL_GET,
        crate::rpc_router::RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: get_handler(),
        },
    );
    b.add(
        KIND_UPDATE_CHANNEL_SET,
        crate::rpc_router::RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_handler(),
        },
    );
}

fn get_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission(
                &state.db,
                &actor_id,
                KIND_UPDATE_CHANNEL_GET,
                internal,
            )
            .await?;
            let _req: AdminUpdateChannelGetRequest = fauna_protocol::decode_strict(&payload)
                .map_err(|e| crate::rpc_errors::malformed_ns("update_channel", e))?;
            crate::rpc_errors::encode_reply(&projection(&state).await)
        })
    })
}

fn set_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission(
                &state.db,
                &actor_id,
                KIND_UPDATE_CHANNEL_SET,
                internal,
            )
            .await?;
            let req: AdminUpdateChannelSetRequest = fauna_protocol::decode_strict(&payload)
                .map_err(|e| crate::rpc_errors::malformed_ns("update_channel", e))?;
            // A nest the image does not run has no updater to enact a choice:
            // refusing here keeps "a channel chosen that nothing follows"
            // unrepresentable (the read says `enactable: false`, the apps
            // paint the section read-only).
            if !enactable(&state) {
                return Err(crate::rpc_errors::conflict_ns(
                    "update_channel",
                    "this nest was not installed from the container image, so no updater can enact an update channel here",
                ));
            }
            let channel = UpdateChannel::from_id(&req.channel).ok_or_else(|| {
                crate::rpc_errors::invalid_params_ns(
                    "update_channel",
                    format!("{:?} names no update channel", req.channel),
                )
            })?;
            apply_update_channel_change(&state, channel)
                .await
                .map_err(internal)?;
            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    AUDIT_UPDATE_CHANNEL_SET,
                    None,
                    Some(channel.id()),
                )
                .await;
            crate::rpc_errors::encode_reply(&AdminUpdateChannelSetReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;

    fn head(version: &str, revision: Option<&str>) -> Head {
        Head {
            digest: "sha256:head".into(),
            version: Some(version.into()),
            revision: revision.map(str::to_string),
        }
    }

    #[test]
    fn the_same_commit_is_current_a_newer_build_applies_an_older_holds() {
        let ours = "0123abc4def";
        // The same build, by its commit, whatever the versions say.
        assert!(!says_apply(ours, "0.2.0", &head("0.2.0", Some("0123abc4"))));
        assert!(!says_apply(&ours[..8], "0.2.0", &head("0.2.0", Some(ours))));
        // Another commit at the same or a newer version: Apply.
        assert!(says_apply(ours, "0.2.0", &head("0.2.0", Some("fedcba98"))));
        assert!(says_apply(ours, "0.2.0", &head("0.3.0", Some("fedcba98"))));
        // An older head: Hold.
        assert!(!says_apply(ours, "0.2.0", &head("0.1.9", Some("fedcba98"))));
        // Nothing to tell builds apart by: never overdue.
        assert!(!says_apply(
            "dev",
            "0.2.0",
            &head("0.3.0", Some("fedcba98"))
        ));
        assert!(!says_apply(ours, "0.2.0", &head("0.3.0", None)));
    }

    #[test]
    fn overdue_is_said_once_per_head_after_the_interval_plus_the_allowance() {
        let mut clock = Overdue::default();
        let t0 = Instant::now();
        let after = Duration::from_secs(60 + 600);
        let at = |s: u64| t0 + Duration::from_secs(s);
        assert!(!clock.observe("a", true, at(0), after));
        assert!(!clock.observe("a", true, at(660), after));
        assert!(clock.observe("a", true, at(661), after));
        assert!(!clock.observe("a", true, at(9_000), after), "once per head");
        // A new head starts its own clock.
        assert!(!clock.observe("b", true, at(9_000), after));
        assert!(clock.observe("b", true, at(9_661), after));
        // A decision that stops saying Apply resets the clock.
        assert!(!clock.observe("c", true, at(0), after));
        assert!(!clock.observe("c", false, at(500), after));
        assert!(!clock.observe("c", true, at(700), after));
        assert!(!clock.observe("c", true, at(1_300), after));
        assert!(clock.observe("c", true, at(1_361), after));
    }

    #[tokio::test]
    async fn the_seed_is_followed_until_a_row_exists() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(
            resolve_update_channel(&db, Some("test")).await,
            Some(UpdateChannel::Test)
        );
        assert_eq!(resolve_update_channel(&db, Some("sha-abc")).await, None);
        // The compose default, interpolated without one: Stable.
        assert_eq!(
            resolve_update_channel(&db, Some("")).await,
            Some(UpdateChannel::Stable)
        );
        db.set_update_channel(UpdateChannel::Dev).await.unwrap();
        for seed in [Some("test"), Some("sha-abc"), Some("")] {
            assert_eq!(
                resolve_update_channel(&db, seed).await,
                Some(UpdateChannel::Dev),
                "the admin's row must win over the seed {seed:?}"
            );
        }
    }

    /// No image tag in the environment is a nest the image does not run: no
    /// channel applies, and a row an earlier build wrote does not revive one.
    #[tokio::test]
    async fn a_nest_the_image_does_not_run_follows_no_channel_whatever_the_row_says() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(resolve_update_channel(&db, None).await, None);
        db.set_update_channel(UpdateChannel::Dev).await.unwrap();
        assert_eq!(resolve_update_channel(&db, None).await, None);
    }

    /// The read reports whether a channel can be enacted — the test `AppState`
    /// carries no image tag, as a packaged nest's environment carries none.
    #[tokio::test]
    async fn without_an_image_tag_the_read_says_not_enactable() {
        let db = std::sync::Arc::new(CacheDb::open_in_memory().unwrap());
        let state = AppState::for_test(db);
        assert!(!enactable(&state));
        let reply = projection(&state).await;
        assert!(!reply.enactable);
        assert_eq!(reply.channel, None);
    }
}
