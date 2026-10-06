//! The domain-expiry watch's read kind — `fauna.domain.expiry.get`.
//!
//! Owner: `docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Domain
//! loss → *Detection*. The watch that writes the record is
//! [`crate::domain_expiry`]; the decision the reply feeds is
//! `fauna_protocol::domain_expiry::evaluate`.
//!
//! **User-class, not Admin-class, and that is the ratified point.** Only an
//! admin can renew a domain, but a resident's stake in it is real and different
//! — their `@domain` addresses die and their phrase-only recovery *locator*
//! breaks — so § Detection puts every authenticated user in the audience and
//! differentiates the *lines*, rather than scoping the alert to admins and
//! leaving residents to learn at the failure. That is also why this kind sits in
//! its own `fauna.domain.*` namespace instead of `fauna.dns.*`, which is
//! Admin-only by its own declaration.

use std::sync::Arc;
use std::time::Duration;

use fauna_protocol::domain_expiry::{DomainExpiryReply, DomainExpiryRequest};
use fauna_protocol::{RpcError, decode_strict as decode};

use crate::routes::AppState;
use crate::rpc_errors::{encode_reply, internal_ns, malformed_ns};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

const NS: &str = "domain";

async fn require_permission(state: &Arc<AppState>, actor_id: &[u8; 32]) -> Result<(), RpcError> {
    crate::bridge_method_allowlist::require_permission(
        &state.db,
        actor_id,
        "fauna.domain.expiry.get",
        |e| internal_ns(NS, e),
    )
    .await?;
    Ok(())
}

fn expiry_get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id).await?;
            let _req: DomainExpiryRequest = decode(&payload).map_err(|e| malformed_ns(NS, e))?;

            let record = state
                .db
                .get_domain_expiry()
                .await
                .map_err(|e| internal_ns(NS, e))?;

            // Mirrors `am_i_admin_handler`'s `unwrap_or(false)`: a db hiccup on
            // the *role* lookup must not fail the whole read. The consequence is
            // stated rather than assumed — a lapse alarm would then render the
            // resident line to an admin, which still says "the deployment's name
            // is lapsing" and is strictly better than no alarm at all. The
            // reverse default (assume admin) would tell every resident to go
            // renew a domain they cannot reach.
            let admin = state.db.is_admin(&actor_id).await.unwrap_or(false);

            encode_reply(&DomainExpiryReply {
                record,
                admin,
                extra: Default::default(),
            })
        })
    })
}

/// Register `fauna.domain.expiry.get` on the authenticated router.
///
/// `forbid_replay = false` @5 s, matching the other pure reads: replaying a read
/// costs nothing and lands the same bytes.
pub fn register_domain_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.domain.expiry.get",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: expiry_get_handler(),
        },
    );
}
