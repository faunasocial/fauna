//! XRPC helpers for calling Bluesky API methods via an authenticated session.

use atrium_api::agent::{Agent, SessionManager};
use atrium_api::app::bsky::actor::get_profile;
use atrium_api::types::string::AtIdentifier;

use crate::types::BlueskyActor;

/// Call `app.bsky.actor.getProfile` and translate to [`BlueskyActor`].
pub async fn get_profile_for<M>(agent: &Agent<M>, actor: &str) -> anyhow::Result<BlueskyActor>
where
    M: SessionManager + Send + Sync,
{
    let actor_id: AtIdentifier = actor
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid actor identifier: {e}"))?;

    let params = get_profile::Parameters::from(get_profile::ParametersData { actor: actor_id });

    let profile = agent
        .api
        .app
        .bsky
        .actor
        .get_profile(params)
        .await
        .map_err(|e| anyhow::anyhow!("getProfile failed: {e}"))?;

    let viewer = profile.viewer.as_deref();
    Ok(BlueskyActor {
        did: profile.did.to_string(),
        handle: profile.handle.to_string(),
        display_name: profile.display_name.clone(),
        avatar: profile.avatar.clone(),
        description: profile.description.clone(),
        followers_count: profile.followers_count.unwrap_or(0) as u64,
        follows_count: profile.follows_count.unwrap_or(0) as u64,
        posts_count: profile.posts_count.unwrap_or(0) as u64,
        viewer_following: viewer.and_then(|v| v.following.clone()),
        viewer_followed_by: viewer.and_then(|v| v.followed_by.clone()),
    })
}
