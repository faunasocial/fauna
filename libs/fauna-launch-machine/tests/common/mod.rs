//! Shared fixtures for the LaunchMachine token-refresh integration tests
//! (`token_refresh.rs`, `ttl_refresh.rs`): scripted silent-challenge replies
//! and the machine-to-Online builder both files drove by hand.

use std::sync::Arc;

use fauna_launch_machine::{
    InMemoryPersistence, LaunchMachine, LaunchPhase, MockAuthConnector, NullObserver,
    SilentChallengeOutcome,
};
use fauna_protocol::auth::VerifyReply;

pub fn test_secret() -> [u8; 32] {
    [0x42; 32]
}

/// A verify reply for the LAUNCH mint: session id `0`*16, catalog metadata.
pub fn verify_reply(token: &str, expires_at: u64) -> VerifyReply {
    verify_reply_with_id(token, &"0".repeat(16), expires_at)
}

/// A verify reply naming its own session id — the refresh mints, whose ids the
/// own-session assertions tell apart from the launch mint's.
pub fn verify_reply_with_id(token: &str, token_id: &str, expires_at: u64) -> VerifyReply {
    // The tests script deadlines as absolute seconds on THIS clock; the
    // machine anchors on `expires_in` at receipt, so derive the TTL the nest
    // would have sent for that deadline.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after the epoch")
        .as_secs();
    VerifyReply {
        token: token.into(),
        token_id: token_id.into(),
        handle: "alice".into(),
        domain: "nest.example".into(),
        tier: "free".into(),
        expires_at,
        expires_in: expires_at.saturating_sub(now),
        ..Default::default()
    }
}

/// A successful refresh reply — the shape every refresh test used to spell as
/// the handshake's `TokenRefreshOutcome::Success`; since the refresh became the
/// silent challenge too it is a verify reply like the launch mint's.
pub fn refreshed(token: &str, token_id: &str, expires_at: u64) -> SilentChallengeOutcome {
    SilentChallengeOutcome::Success(verify_reply_with_id(token, token_id, expires_at))
}

/// Drive the machine to Online with `expires_at_secs` from a scripted silent
/// challenge, with the given refresh outcomes queued after it (FIFO; last
/// sticks). Launch and refresh share the mock's one `silent_challenge` queue
/// because the machine runs one ceremony for both.
pub async fn online_with_expiry(
    expires_at_secs: u64,
    refreshes: Vec<SilentChallengeOutcome>,
) -> Arc<LaunchMachine> {
    let mut connector = MockAuthConnector::new().push_silent_challenge(
        SilentChallengeOutcome::Success(verify_reply("initial.token", expires_at_secs)),
    );
    for r in refreshes {
        connector = connector.push_silent_challenge(r);
    }
    let p = Arc::new(
        InMemoryPersistence::new()
            .with_identity(test_secret().to_vec())
            .with_nest_url("https://nest.example"),
    );
    let m = LaunchMachine::new_with_connector(Arc::new(NullObserver), p, Arc::new(connector));
    m.start().await;
    assert_eq!(m.snapshot().phase, LaunchPhase::Online);
    m
}
