//! Background task that advances the eviction state machine.

use std::sync::Arc;

use crate::routes::AppState;

/// Spawn a background task that runs [`run_eviction_tick`] every 60 seconds.
pub fn spawn_eviction_task(state: Arc<AppState>) {
    state.clone().spawn_scoped(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            match run_eviction_tick(&state).await {
                Ok(tick) => {
                    if tick.suspended > 0 || tick.deleted > 0 || tick.held > 0 {
                        tracing::info!(
                            "eviction tick: {} suspended, {} deleted, {} held for retry",
                            tick.suspended,
                            tick.deleted,
                            tick.held
                        );
                    }
                }
                Err(e) => {
                    tracing::error!("eviction transition error: {e}");
                }
            }
        }
    });
}

/// What one [`run_eviction_tick`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct EvictionTick {
    /// Actors moved `warning -> suspended` this tick.
    pub suspended: usize,
    /// `deleting` actors whose finalize completed this tick.
    pub deleted: usize,
    /// `deleting` actors whose finalize refused or failed; they stay marked,
    /// and the next tick retries them.
    pub held: usize,
}

/// Advance the eviction ladder once (`admin.md` § Cutting a user off):
/// `warning -> suspended` (revoke tokens, push, close sockets), then
/// `suspended -> deleting`, then finalize every `deleting` account.
///
/// The finalize is [`crate::pending_actions::finalize_user_deletion`] — the same
/// path an admin deletion takes, called directly rather than through a pending
/// action: `account.delete`'s arm would add the room-owner refusal (letting an
/// evicted user block their own eviction by seating one other member), and
/// `schedule()` would add its own delay on top of the ladder's hard-coded
/// windows. A refused or failed finalize (an admin, a guardian with wards, an
/// I/O error) leaves the row `deleting`, never falls back to a raw delete, and
/// is retried next tick — the executor's posture for a persisted deletion.
pub async fn run_eviction_tick(state: &Arc<AppState>) -> anyhow::Result<EvictionTick> {
    let (suspended, deleting) = state.db.transition_evictions().await?;
    let mut tick = EvictionTick {
        suspended: suspended.len(),
        ..Default::default()
    };
    for actor_id_vec in &suspended {
        if let Ok(actor_id) = <[u8; 32]>::try_from(actor_id_vec.as_slice()) {
            // Revoke all auth tokens
            state
                .auth
                .token_store
                .revoke_actor(&fauna_core::identity::ActorId(actor_id))
                .await;
            // Notify connected clients, then close their sockets.
            // The push is best-effort (the close does not drain);
            // the 4401 is the authoritative signal.
            state.ws.notify_push(
                &actor_id,
                fauna_protocol::PushEvent::AccountUpdated(
                    fauna_protocol::push_events::AccountUpdatedPayload {
                        changes: vec!["eviction".into()],
                        timestamp: fauna_core::data::Timestamp::now_secs() as u64,
                        extra: std::collections::BTreeMap::new(),
                    },
                ),
            );
            // Tokens alone leave the already-open socket serving
            // a now-suspended actor its Push stream
            // (`transport.md` § Revocation teardown).
            state.close_actor_sockets(&actor_id);
        }
    }
    for actor_id_vec in &deleting {
        let Ok(actor_id) = <[u8; 32]>::try_from(actor_id_vec.as_slice()) else {
            continue;
        };
        match crate::pending_actions::finalize_user_deletion(state, &actor_id).await {
            Ok(()) => {
                state.revoke_actor_authority(&actor_id).await;
                let _ = state
                    .db
                    .audit(None, "eviction.delete", Some(&hex::encode(actor_id)), None)
                    .await;
                tick.deleted += 1;
            }
            Err(e) => {
                tracing::warn!(
                    actor = %hex::encode(actor_id),
                    error = %e,
                    "eviction finalize held; retrying next tick"
                );
                tick.held += 1;
            }
        }
    }
    Ok(tick)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;

    fn state() -> (Arc<CacheDb>, Arc<AppState>) {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        (db, state)
    }

    /// Start an eviction whose both windows have already run out, then tick
    /// twice: the first tick suspends, the second marks `deleting` and
    /// finalizes (a same-tick suspend never deletes).
    async fn evict_now(state: &Arc<AppState>, actor: &[u8; 32]) -> (EvictionTick, EvictionTick) {
        assert!(
            state
                .db
                .start_eviction(actor, "capacity", "capacity", 0, 0)
                .await
                .unwrap()
        );
        let first = run_eviction_tick(state).await.unwrap();
        let second = run_eviction_tick(state).await.unwrap();
        (first, second)
    }

    /// The row's success bar: an evicted account goes through the one
    /// production deletion path. Its `users` row, its `Purge` rows and its
    /// local predecessors' rows are gone, a bystander is untouched, the
    /// `Retain` succession chain survives, and the audit log names the
    /// deletion. Also pins `admin.md` § Held-for-friends: "stop hosting" a
    /// `backup`-tier guest drops the guest's reserved folders.
    #[tokio::test]
    async fn an_evicted_account_is_finalized_through_the_purge_walk() {
        let (db, state) = state();
        let (first, guest) = ([0xA1u8; 32], [0xA2u8; 32]);
        let bystander = [0xB1u8; 32];
        db.create_user(&first, "backup", "").await.unwrap();
        db.create_user(&bystander, "free", "").await.unwrap();
        db.record_succession(&first, &guest, b"s", 1)
            .await
            .unwrap()
            .unwrap();
        for id in [first, guest, bystander] {
            db.mark_replicated("inbox", &id, None).await.unwrap();
        }
        for id in [guest, bystander] {
            db.create_folder_with_options(
                "__mail",
                &id,
                crate::db::FolderOptions {
                    custody_copy: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }

        let (first_tick, second_tick) = evict_now(&state, &guest).await;
        assert_eq!(
            first_tick,
            EvictionTick {
                suspended: 1,
                ..Default::default()
            },
            "a same-tick suspend waits a tick before its deletion"
        );
        assert_eq!(
            second_tick,
            EvictionTick {
                deleted: 1,
                ..Default::default()
            }
        );

        for (who, id, survives) in [
            ("the evicted account", guest, false),
            ("its local predecessor", first, false),
            ("a bystander", bystander, true),
        ] {
            assert_eq!(
                db.is_actor_registered(&id).await.unwrap(),
                survives,
                "{who}'s users row"
            );
            assert_eq!(
                db.is_replicated("inbox", &id, None).await.unwrap(),
                survives,
                "{who}'s Purge row (the replication marker)"
            );
        }
        assert!(
            db.get_folder_for_actor("__mail", &guest)
                .await
                .unwrap()
                .is_none(),
            "the evicted backup guest's reserved folder is reclaimed"
        );
        assert!(
            db.get_folder_for_actor("__mail", &bystander)
                .await
                .unwrap()
                .is_some()
        );
        assert!(db.succession_for(&first).await.unwrap().is_some());
        let audit = db.list_audit(50, None).await.unwrap();
        assert!(
            audit.iter().any(|a| a.action == "eviction.delete"
                && a.target.as_deref() == Some(&hex::encode(guest))),
            "the deletion is audited"
        );

        // Nothing left to do: the next tick is a no-op.
        assert_eq!(
            run_eviction_tick(&state).await.unwrap(),
            EvictionTick::default()
        );
    }

    /// A refused finalize never falls back to a raw delete: an account that
    /// picked up the admin role after its eviction started stays `deleting`
    /// with its data intact, restore is refused (the deletion has begun), and
    /// it completes on the first tick after the role is removed.
    #[tokio::test]
    async fn a_refused_finalize_holds_the_row_and_retries_next_tick() {
        let (db, state) = state();
        let actor = [0xC1u8; 32];
        db.create_user(&actor, "free", "").await.unwrap();
        db.mark_replicated("inbox", &actor, None).await.unwrap();
        assert!(
            db.start_eviction(&actor, "terms", "terms", 0, 0)
                .await
                .unwrap()
        );
        run_eviction_tick(&state).await.unwrap();
        // The box's real superadmin, so the roster floor lets the role go below.
        db.add_admin_actor(&[0xEEu8; 32]).await.unwrap();
        db.add_admin_actor(&actor).await.unwrap();

        let held = run_eviction_tick(&state).await.unwrap();
        assert_eq!(
            held,
            EvictionTick {
                held: 1,
                ..Default::default()
            }
        );
        let row = db.get_user(&actor).await.unwrap().expect("row intact");
        assert_eq!(row.eviction_status, "deleting");
        assert!(row.suspended, "still cut off while held");
        assert!(db.is_replicated("inbox", &actor, None).await.unwrap());
        assert!(
            !db.cancel_eviction(&actor).await.unwrap(),
            "a deletion that has begun is not restorable"
        );
        // Held again, not dropped, on the next tick.
        assert_eq!(run_eviction_tick(&state).await.unwrap().held, 1);

        assert_eq!(
            db.remove_admin_actor(&actor).await.unwrap(),
            crate::db::admin::RosterWrite::Applied
        );
        assert_eq!(run_eviction_tick(&state).await.unwrap().deleted, 1);
        assert!(!db.is_actor_registered(&actor).await.unwrap());
        assert!(!db.is_replicated("inbox", &actor, None).await.unwrap());
    }

    /// An evicted account is not findable by the handle it held — the Search
    /// corpus row goes with the `users` row (`fts::sync_profile_row`, run by
    /// `delete_user` inside the finalize).
    #[tokio::test]
    async fn an_evicted_account_leaves_no_profile_hit() {
        let (db, state) = state();
        let actor = [0xD1u8; 32];
        db.create_user_with_handle(&actor, "free", "quillon", None)
            .await
            .unwrap();
        let hits = |db: Arc<CacheDb>| async move {
            db.search_fts("raw:q*", Some("profile"), None, None, 100, 0)
                .await
                .unwrap()
                .len()
        };
        assert_eq!(hits(db.clone()).await, 1);
        evict_now(&state, &actor).await;
        assert!(!db.is_actor_registered(&actor).await.unwrap());
        assert_eq!(hits(db.clone()).await, 0);
    }
}
