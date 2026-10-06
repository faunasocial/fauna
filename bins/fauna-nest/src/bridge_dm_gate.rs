//! The family bridge-DM gate's nest-side reads (`docs/goal/behavior/family-safety.md`
//! § The bridge-DM gate), shared by every bridge-DM family — the Nostr DM
//! surfaces and the bridged-conversation family — so the composition lives
//! once.
//!
//! - **Inbound** ([`peer_verdict`]): the knob and the stored verdict composed
//!   by `fauna_core::data::supervised_dm_verdict`; a `Blocked` peer's new
//!   message is refused before storage by the caller.
//! - **Outbound** ([`outbound_blocked`]): only an explicit guardian `block`
//!   refuses — the knob is not consulted.
//! - **At read** ([`gate_context`] + [`guardian_state_for`]): the marker is
//!   computed, never stored, so relaxing the knob releases everything by
//!   construction.

use std::collections::HashMap;

use fauna_core::data::{DmVerdict, UnknownPeerDm, supervised_dm_verdict};
use fauna_protocol::RpcError;

use crate::routes::AppState;
use crate::rpc_errors::internal;

/// A supervised caller's knob plus every stored verdict, keyed
/// `(bridge_id, peer)`. `None` for an unsupervised caller — the common case,
/// which costs exactly one policy read.
pub type GateContext = (UnknownPeerDm, HashMap<(String, String), String>);

/// Read the caller's gate context once per request.
pub async fn gate_context(
    state: &AppState,
    actor_id: &[u8],
) -> Result<Option<GateContext>, RpcError> {
    let Some(row) = state
        .db
        .get_guardian_policy(actor_id)
        .await
        .map_err(internal)?
    else {
        return Ok(None);
    };
    let knob = UnknownPeerDm::from_wire(&row.unknown_peer_dm);
    let verdicts = state
        .db
        .list_dm_peer_verdicts(actor_id)
        .await
        .map_err(internal)?
        .into_iter()
        .map(|(bridge, peer, verdict)| ((bridge, peer), verdict))
        .collect();
    Ok(Some((knob, verdicts)))
}

/// The verdict for one peer under a read context; `Deliver` without one.
pub fn verdict_in(ctx: Option<&GateContext>, bridge_id: &str, peer: &str) -> DmVerdict {
    let Some((knob, verdicts)) = ctx else {
        return DmVerdict::Deliver;
    };
    supervised_dm_verdict(
        Some(*knob),
        verdicts
            .get(&(bridge_id.to_string(), peer.to_string()))
            .map(String::as_str),
    )
}

/// The wire marker for one peer: `"held"` / `"blocked"` / `None` (deliver —
/// nothing to render).
pub fn guardian_state_for(
    ctx: Option<&GateContext>,
    bridge_id: &str,
    peer: &str,
) -> Option<String> {
    marker(verdict_in(ctx, bridge_id, peer))
}

/// A room's marker over its far participants: the strictest one's — a room
/// with any `block`ed peer reads blocked, else any held peer reads held.
pub fn room_guardian_state(
    ctx: Option<&GateContext>,
    bridge_id: &str,
    participants: &[String],
) -> Option<String> {
    let rank = |v: DmVerdict| match v {
        DmVerdict::Deliver => 0,
        DmVerdict::Held => 1,
        DmVerdict::Blocked => 2,
    };
    participants
        .iter()
        .map(|p| verdict_in(ctx, bridge_id, p))
        .max_by_key(|v| rank(*v))
        .and_then(marker)
}

fn marker(v: DmVerdict) -> Option<String> {
    match v {
        DmVerdict::Deliver => None,
        DmVerdict::Held => Some("held".to_string()),
        DmVerdict::Blocked => Some("blocked".to_string()),
    }
}

/// The inbound verdict for one peer — what a DM write path composes before
/// storing (`family-safety.md` § The bridge-DM gate → *Enforcement points*).
pub async fn peer_verdict(
    state: &AppState,
    actor_id: &[u8],
    bridge_id: &str,
    peer: &str,
) -> Result<DmVerdict, RpcError> {
    let Some(row) = state
        .db
        .get_guardian_policy(actor_id)
        .await
        .map_err(internal)?
    else {
        return Ok(DmVerdict::Deliver);
    };
    let verdict = state
        .db
        .dm_peer_verdict(actor_id, bridge_id, peer)
        .await
        .map_err(internal)?;
    Ok(supervised_dm_verdict(
        Some(UnknownPeerDm::from_wire(&row.unknown_peer_dm)),
        verdict.as_deref(),
    ))
}

/// Is a send to `peer` refused — an explicit guardian `block`? The knob is
/// deliberately not consulted: outbound stays ungated but for a block.
pub async fn outbound_blocked(
    state: &AppState,
    actor_id: &[u8],
    bridge_id: &str,
    peer: &str,
) -> Result<bool, RpcError> {
    // Only `Blocked` refuses, and only a stored `block` yields it — a cold
    // peer under a `hold` knob reads `Held`, which a send does not consult.
    Ok(peer_verdict(state, actor_id, bridge_id, peer).await? == DmVerdict::Blocked)
}
