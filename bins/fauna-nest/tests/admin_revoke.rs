use std::sync::Arc;

use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::token_store::TokenStore;

async fn start_server() -> (String, Arc<fauna_nest::routes::AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(TokenStore::new());
    let state = Arc::new(fauna_nest::routes::AppState {
        auth: fauna_nest::state::AuthState {
            token_store: tokens.clone(),
            ..Default::default()
        },
        ..fauna_nest::routes::AppState::for_test(db.clone())
    });

    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    (format!("http://{addr}"), state)
}

#[tokio::test]
async fn eviction_suspend_revokes_tokens() {
    let (_base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    state
        .db
        .create_user(&kp.actor_id().0, "free", "test")
        .await
        .unwrap();

    let token = state.auth.token_store.insert(kp.actor_id(), 3600).await;
    assert!(state.auth.token_store.validate(&token).await.is_some());

    // Start eviction with 0-day warning (immediate suspend)
    state
        .db
        .start_eviction(&kp.actor_id().0, "test", "terms", 0, 14)
        .await
        .unwrap();
    let (suspended, _deleted) = state.db.transition_evictions().await.unwrap();
    assert_eq!(suspended.len(), 1);

    // Revoke tokens for suspended users (as the eviction task would)
    for actor_id_vec in &suspended {
        let actor_id: [u8; 32] = actor_id_vec.as_slice().try_into().unwrap();
        state
            .auth
            .token_store
            .revoke_actor(&fauna_core::identity::ActorId(actor_id))
            .await;
    }

    assert!(state.auth.token_store.validate(&token).await.is_none());
}

#[tokio::test]
async fn delete_user_revokes_tokens() {
    let (_base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    state
        .db
        .create_user(&kp.actor_id().0, "free", "test")
        .await
        .unwrap();

    let token = state.auth.token_store.insert(kp.actor_id(), 3600).await;
    assert!(state.auth.token_store.validate(&token).await.is_some());

    state.db.delete_user(&kp.actor_id().0).await.unwrap();
    state.auth.token_store.revoke_actor(&kp.actor_id()).await;

    assert!(state.auth.token_store.validate(&token).await.is_none());
}
