//! `AuthClient::bearer_only` stands up auth from a pre-minted bearer + the
//! owner's public actor_id, with NO identity keypair.
//!
//! This is the Windows on-demand hydration host's auth shape: a
//! capability-scoped helper holds a renewable nest bearer and the owner's
//! public `actor_id` (for the WS URL path) but never the Ed25519 keypair (that
//! lives only in the WinUI app). The data plane never signs with a keypair —
//! the authenticated WS presents the bearer in `Sec-WebSocket-Protocol`. This
//! test asserts the constructor wires `actor_id_hex()` from the supplied bytes,
//! `ensure_auth()` returns the source's bearer without a round trip, and
//! `keypair()` is `None` (the register / pairing / caldav secret-export paths
//! are unreachable for such a helper).

use std::sync::Arc;

use fauna_client::AuthClient;
use fauna_nest_http::StaticBearer;

#[tokio::test]
async fn bearer_only_wires_actor_id_and_bearer_without_keypair() {
    let actor_id = [0x5au8; 32];
    let bearer: Arc<dyn fauna_nest_http::BearerSource> = Arc::new(StaticBearer("tok".into()));
    let http = reqwest::Client::new();

    // Unreachable nest URL — `StaticBearer` never hits the network, so the test
    // passes regardless of reachability.
    let client = AuthClient::bearer_only(
        "http://127.0.0.1:1".into(),
        actor_id,
        Arc::clone(&bearer),
        http,
    );

    assert_eq!(client.actor_id_hex(), hex::encode(actor_id));
    let token = client.ensure_auth().await.expect("ensure_auth");
    assert_eq!(token, "tok");
    assert!(
        client.keypair().is_none(),
        "a bearer-only helper holds no identity keypair"
    );
}
