use fauna_core::identity::ActorKeypair;
use fauna_nest::token_store::TokenStore;

#[tokio::test]
async fn insert_and_validate_token() {
    let store = TokenStore::new();
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();

    let token = store.insert(actor_id, 3600).await;
    let result = store.validate(&token).await;
    assert!(result.is_some());
    assert_eq!(result.unwrap(), actor_id);
}

#[tokio::test]
async fn expired_token_rejected() {
    let store = TokenStore::new();
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();

    // TTL of 0 = already expired
    let token = store.insert(actor_id, 0).await;
    let result = store.validate(&token).await;
    assert!(result.is_none());
}

#[tokio::test]
async fn revoke_actor_removes_tokens() {
    let store = TokenStore::new();
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();

    let token = store.insert(actor_id, 3600).await;
    store.revoke_actor(&actor_id).await;
    let result = store.validate(&token).await;
    assert!(result.is_none());
}

#[tokio::test]
async fn invalid_token_rejected() {
    let store = TokenStore::new();
    let result = store.validate("nonexistent_token").await;
    assert!(result.is_none());
}
