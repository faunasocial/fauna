//! ActivityPub federation integration routes and background tasks.
//! Gated behind the `activitypub` cargo feature.

pub mod actor_routes;
pub mod bridge_provider;
pub mod db_helpers;
pub mod dm_leg;
pub mod inbox_routes;
pub mod instance_actor;
pub mod interact;
pub mod key_crypto;
pub mod outbound;
pub mod push;
pub mod sync_worker;

use crate::db::CacheDb;
use crate::routes::AppState;
use axum::Router;
use std::sync::Arc;

/// **The one place this bridge's schema is applied.**
///
/// `init_db` calls it, and so does the `actor_tables` registry guards' own
/// seeding (`apply_available_bridge_schemas`) — which is the point.
/// The guards applied
/// `CREATE_TABLES_SQL` and none of the post-`CREATE` migrations, so
/// `ap_post_map.remote_actor_uri` was invisible to every column belt *by
/// construction*, and a belt cannot report a column that was never created.
/// Adding the missing calls would have fixed that day and re-opened at the
/// next migration; a single function makes the drift unrepresentable instead,
/// because a migration can only be added where both callers see it.
///
/// `fauna_bridge_activitypub::db::CREATE_TABLES_SQL` is this bridge's genesis
/// (`ap_dead_inboxes` included), applied in the one shape every bridge shares
/// ([`crate::bridge_schema::apply_genesis`]: the block plus the additive column
/// reconciler, no hand-written `ALTER`).
pub fn apply_schema(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    crate::bridge_schema::apply_genesis(conn, fauna_bridge_activitypub::db::CREATE_TABLES_SQL)
}

/// Initialize ActivityPub bridge tables in the nest database.
pub async fn init_db(db: &CacheDb) -> anyhow::Result<()> {
    let conn = db.conn().await;
    apply_schema(&conn)
}

/// ActivityPub routes merged into the main router.
///
/// Third-party-facing HTTP only — the actor/WebFinger/NodeInfo surface and the
/// inboxes, whose far end is another AP server (`api-layers.md`: permanent
/// Layer-6 residue). The user-facing control plane is NOT here: it is the
/// `ActivityPubProvider` on the `fauna.bridges.*` WS-RPC wire.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .merge(actor_routes::routes())
        .merge(inbox_routes::routes())
}

/// A bare `AppState` over a fresh in-memory `CacheDb` carrying the AP schema
/// — domainless, unclaimed. `instance_actor::state_with_domain`,
/// `inbox_routes::state`/`domainless_state` and `push::state` each hand-copied
/// this exact "open in-memory, execute `CREATE_TABLES_SQL`, wrap in
/// `AppState::for_test`" prefix before diverging on whether/how a domain gets
/// claimed. Its deployment keypair is the seed its identity is a view over, as
/// `start_server` leaves a nest: the instance actor's mint seals under the seed
/// the database holds and its reads open under the generation's copy.
/// `#[cfg(test)]`: nothing outside this module's own tests needs it.
#[cfg(test)]
async fn ap_state_for_test() -> AppState {
    let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
    {
        let conn = db.conn().await;
        conn.execute_batch(fauna_bridge_activitypub::db::CREATE_TABLES_SQL)
            .expect("AP schema");
    }
    let state = AppState::for_test(db);
    crate::test_support::seat_own_deployment_seed(&state).await;
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `init_db` is idempotent on a current-schema database.
    #[tokio::test(flavor = "multi_thread")]
    async fn init_db_is_idempotent() {
        let db = CacheDb::open_in_memory().expect("in-memory CacheDb");
        init_db(&db).await.expect("applies the genesis");
        init_db(&db).await.expect("idempotent on current schema");

        let conn = db.conn().await;
        super::db_helpers::insert_post_map(&conn, "p1", "https://r.example/n/1", "a1", Some("u1"))
            .expect("genesis carries remote_actor_uri");
        let (_, actor) = super::db_helpers::get_ap_target_for_post(&conn, "p1")
            .unwrap()
            .unwrap();
        assert_eq!(actor.as_deref(), Some("u1"));
    }

    /// The bridge's genesis is reconciled, not ALTERed: every additive column
    /// a long-lived database lacks comes back through `apply_schema`.
    #[test]
    fn apply_schema_reconciles_every_droppable_column() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // The nest genesis first: the bridge's triggers reach its tables.
        crate::db::migrations::run_migrations(&conn).unwrap();
        apply_schema(&conn).unwrap();
        let dropped = crate::bridge_schema::drop_additive_columns_and_reapply(
            &conn,
            fauna_bridge_activitypub::db::CREATE_TABLES_SQL,
            apply_schema,
        );
        assert!(
            dropped >= 5,
            "the probe must actually drop columns ({dropped})"
        );
    }
}
