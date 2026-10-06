use fauna_core::identity::ActorKeypair;
use fauna_nest::token_store::TokenStore;

#[tokio::test]
async fn gc_removes_expired_tokens() {
    let store = TokenStore::new();
    let kp = ActorKeypair::generate();

    // Insert a token with 0-second TTL (already expired)
    let token = store.insert(kp.actor_id(), 0).await;

    // It should be invalid immediately
    assert!(store.validate(&token).await.is_none());

    // GC should remove it
    let removed = store.gc().await;
    assert!(removed > 0);
}
