//! The update channel as nest state: which builds this box's updater follows,
//! the `fauna.admin.update_channel.{get,set}` kinds that read and choose it, and
//! the value `/api/v1/health` reports for the updater.
//!
//! Owner: `docs/goal/architecture/nest/update-channel.md` § The channel as nest
//! state. The channel in force is the admin's `nest_update_channel` row, else the
//! seed the install's image tag gave (`FAUNA_IMAGE_TAG`, read into
//! `config.nest.image_tag` at boot; `UpdateChannel::seeded_from` is the rule) —
//! `None` is a *pinned* box. The mapping from a channel to an image tag stays
//! `fauna_provisioning::update_channel::UpdateChannel`'s.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use fauna_protocol::update_channel::{
    AdminUpdateChannelGetReply, AdminUpdateChannelGetRequest, AdminUpdateChannelSetReply,
    AdminUpdateChannelSetRequest,
};
use fauna_provisioning::update_channel::UpdateChannel;
use fauna_provisioning::update_channel::registry::{self, Registry};

use crate::routes::AppState;
use crate::rpc_errors::internal;

/// Resolve the channel at boot: a present, readable row wins; absent — or
/// unreadable, which is logged — follows the seed (`seed_tag`, the install's
/// image tag), so a box always boots following what it was installed from.
pub async fn resolve_update_channel(
    db: &crate::db::CacheDb,
    seed_tag: Option<&str>,
) -> Option<UpdateChannel> {
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

/// The channel in force, as the id `/api/v1/health` serves (`None` ⇒ pinned).
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
        channel_head_version: channel_head_version(channel).await,
        extra: Default::default(),
    }
}

/// How long a resolved channel head (or a failure to resolve one) is reused.
const CHANNEL_HEAD_TTL: Duration = Duration::from_secs(5 * 60);

/// The registry read's budget, inside the kind's 5 s deadline.
const CHANNEL_HEAD_TIMEOUT: Duration = Duration::from_secs(3);

struct CachedHead {
    channel: UpdateChannel,
    at: Instant,
    version: Option<String>,
}

static CHANNEL_HEAD: Mutex<Option<CachedHead>> = Mutex::new(None);

async fn channel_head_version(channel: Option<UpdateChannel>) -> Option<String> {
    let channel = channel?;
    if let Some(hit) = CHANNEL_HEAD
        .lock()
        .ok()?
        .as_ref()
        .filter(|c| c.channel == channel && c.at.elapsed() < CHANNEL_HEAD_TTL)
    {
        return hit.version.clone();
    }
    let registry = registry()?;
    let version = match tokio::time::timeout(
        CHANNEL_HEAD_TIMEOUT,
        registry.head(channel.image_tag(), registry::platform_arch()),
    )
    .await
    {
        Ok(Ok(head)) => head.identity.version.map(|v| v.to_string()),
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
            version: version.clone(),
        });
    }
    version
}

/// The registry the head is read from: the nest image's public registry in
/// production. An e2e build (`test-hooks`) reads the harness's registry named
/// by `FAUNA_E2E_UPDATE_REGISTRY_ORIGIN` and, without one, none — a test nest
/// never reaches the real registry.
fn registry() -> Option<&'static Registry> {
    static REGISTRY: OnceLock<Option<Registry>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| {
            let http = reqwest::Client::builder()
                .timeout(CHANNEL_HEAD_TIMEOUT)
                .build()
                .ok()?;
            #[cfg(feature = "test-hooks")]
            {
                let origin = std::env::var("FAUNA_E2E_UPDATE_REGISTRY_ORIGIN").ok()?;
                Some(Registry::new(
                    http,
                    &origin,
                    fauna_provisioning::update_channel::NEST_IMAGE_REPOSITORY,
                ))
            }
            #[cfg(not(feature = "test-hooks"))]
            Some(Registry::nest(http))
        })
        .as_ref()
}

// ── fauna.admin.update_channel.{get,set} ─────────────────────────────────────

pub const KIND_UPDATE_CHANNEL_GET: &str = "fauna.admin.update_channel.get";
pub const KIND_UPDATE_CHANNEL_SET: &str = "fauna.admin.update_channel.set";

const AUDIT_UPDATE_CHANNEL_SET: &str = "admin:update_channel.set";

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

    #[tokio::test]
    async fn the_seed_is_followed_until_a_row_exists() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(
            resolve_update_channel(&db, Some("test")).await,
            Some(UpdateChannel::Test)
        );
        assert_eq!(resolve_update_channel(&db, Some("sha-abc")).await, None);
        assert_eq!(
            resolve_update_channel(&db, None).await,
            Some(UpdateChannel::Stable)
        );
        db.set_update_channel(UpdateChannel::Dev).await.unwrap();
        for seed in [Some("test"), Some("sha-abc"), None] {
            assert_eq!(
                resolve_update_channel(&db, seed).await,
                Some(UpdateChannel::Dev),
                "the admin's row must win over the seed {seed:?}"
            );
        }
    }
}
