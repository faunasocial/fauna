//! The **reach hint**'s dial rule, against a scripted [`MockAuthConnector`]
//! (no network) — `docs/goal/behavior/onboarding.md` § Reach hint:
//!
//! > dial the domain first; if that fails to connect (unresolvable, refused, or
//! > — on web — untrusted), dial the hint […]; the first successful domain dial
//! > deletes the hint.
//!
//! The transport halves (native's resolve override with SNI/`Host` = the domain,
//! wasm's authority rewrite) are the connector's; what is asserted here is the
//! *policy*, which is the half that must not be re-decided per app: when the
//! hint is used, when it is not, and when it is deleted. The hint is an
//! optimization, so the no-hint path must be byte-for-byte today's behaviour —
//! that is a test here, not a comment.
//!
//! Run with `cargo test -p fauna-launch-machine --features test-helpers,test-observer`.
#![cfg(all(feature = "test-helpers", feature = "test-observer"))]

use std::sync::Arc;

use fauna_launch_machine::{
    InMemoryPersistence, LaunchMachine, LaunchPersistence, MockAuthConnector, NullObserver,
    SilentChallengeOutcome, TokenStatus,
};
use fauna_protocol::auth::VerifyReply;

fn test_secret() -> [u8; 32] {
    [0x42; 32]
}

fn verify_reply() -> VerifyReply {
    VerifyReply {
        token: "actor.opaque".into(),
        token_id: "0".repeat(16),
        handle: "alice".into(),
        domain: "nest.example".into(),
        tier: "free".into(),
        expires_at: 1_700_003_600,
        expires_in: 3600,
        ..Default::default()
    }
}

/// An account whose domain does not resolve yet but whose box is reachable at
/// the address the wizard used — the whole case the hint exists for.
fn persistence_with_hint() -> Arc<InMemoryPersistence> {
    Arc::new(
        InMemoryPersistence::new()
            .with_identity(test_secret().to_vec())
            .with_nest_url("https://nest.example")
            .with_reach_ipv4("203.0.113.9"),
    )
}

fn persistence_without_hint() -> Arc<InMemoryPersistence> {
    Arc::new(
        InMemoryPersistence::new()
            .with_identity(test_secret().to_vec())
            .with_nest_url("https://nest.example"),
    )
}

async fn run(
    connector: MockAuthConnector,
    p: Arc<InMemoryPersistence>,
) -> (Arc<MockAuthConnector>, Arc<InMemoryPersistence>) {
    let connector = Arc::new(connector);
    let m = LaunchMachine::new_with_connector(Arc::new(NullObserver), p.clone(), connector.clone());
    m.start().await;
    (connector, p)
}

#[tokio::test]
async fn the_domain_is_dialled_first_and_the_hint_never_is() {
    // The stale-hint safety argument rests entirely on this ordering: a hint
    // left over from a re-provisioned box is harmless *because* it is only ever
    // the fallback. Dialling it first would silently prefer a stranger's box.
    let connector = MockAuthConnector::new()
        .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply()));
    let (connector, _) = run(connector, persistence_with_hint()).await;

    assert_eq!(
        connector.silent_challenge_hints(),
        vec![None],
        "one dial, and it carried no hint"
    );
}

#[tokio::test]
async fn a_successful_domain_dial_deletes_the_hint() {
    let connector = MockAuthConnector::new()
        .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply()));
    let (_, p) = run(connector, persistence_with_hint()).await;

    assert_eq!(
        p.load_reach_ipv4(),
        None,
        "the domain works; the hint is spent"
    );
}

#[tokio::test]
async fn an_unreachable_domain_falls_back_to_the_hint_and_lands_online() {
    let connector = MockAuthConnector::new()
        .push_silent_challenge(SilentChallengeOutcome::Transient {
            error: "dns: no such host".into(),
        })
        .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply()));
    let (connector, p) = run(connector, persistence_with_hint()).await;

    assert_eq!(
        connector.silent_challenge_hints(),
        vec![None, Some("203.0.113.9".to_string())],
        "domain first, then the hint"
    );
    assert!(
        matches!(p.load_reach_ipv4(), Some(ip) if ip == "203.0.113.9"),
        "the hint carried this launch, so it is still needed next time"
    );
}

#[tokio::test]
async fn a_hint_dial_that_also_fails_is_just_a_failed_fallback() {
    let connector = MockAuthConnector::new()
        .push_silent_challenge(SilentChallengeOutcome::Transient {
            error: "dns: no such host".into(),
        })
        .push_silent_challenge(SilentChallengeOutcome::Transient {
            error: "connection refused".into(),
        });
    let (connector, p) = run(connector, persistence_with_hint()).await;

    assert_eq!(connector.silent_challenge_hints().len(), 2);
    assert!(
        p.load_reach_ipv4().is_some(),
        "a failing hint is not a wrong hint — deletion is the DOMAIN's success, nothing else"
    );
}

#[tokio::test]
async fn without_a_hint_the_transient_path_is_exactly_todays() {
    // The hint is an optimization: an account that never provisioned its own box
    // — every second device, every sign-in by handle — must take the same single
    // dial and the same retry surface it took before the hint existed.
    let connector =
        MockAuthConnector::new().push_silent_challenge(SilentChallengeOutcome::Transient {
            error: "dns: no such host".into(),
        });
    let (connector, _) = run(connector, persistence_without_hint()).await;

    assert_eq!(
        connector.silent_challenge_hints(),
        vec![None],
        "one dial, no fallback invented"
    );
}

#[tokio::test]
async fn a_changed_nest_identity_never_falls_back_to_the_hint() {
    // `IdentityChanged` is terminal by design (security.md § Transport trust):
    // re-dialling the same box by a different authority would be a second TOFU
    // question asked behind the user's back, which is the whole thing the pin
    // exists to prevent. Only a reachability failure earns the fallback.
    let connector =
        MockAuthConnector::new().push_silent_challenge(SilentChallengeOutcome::IdentityChanged {
            host: "nest.example".into(),
            pinned_hex: "aa".repeat(32),
            seen_hex: Some("bb".repeat(32)),
            fork: false,
        });
    let (connector, p) = run(connector, persistence_with_hint()).await;

    assert_eq!(connector.silent_challenge_hints(), vec![None]);
    assert!(
        p.load_reach_ipv4().is_some(),
        "not a domain success, so the hint stands"
    );
}

#[tokio::test]
async fn a_not_registered_answer_is_the_domain_answering_and_ends_the_dial() {
    // `NotRegistered` came *from the nest*, so the domain resolved and the box
    // answered: the hint has done its job and the launch routes to the wizard.
    let connector =
        MockAuthConnector::new().push_silent_challenge(SilentChallengeOutcome::NotRegistered);
    let (connector, _) = run(connector, persistence_with_hint()).await;

    assert_eq!(connector.silent_challenge_hints(), vec![None]);
}

#[tokio::test]
async fn the_token_status_is_untouched_by_a_hint_dial() {
    // A hint dial is the same ceremony over a different socket; nothing about
    // the resulting session may differ, or the hint would be observable.
    let connector = MockAuthConnector::new()
        .push_silent_challenge(SilentChallengeOutcome::Transient {
            error: "dns: no such host".into(),
        })
        .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply()));
    let p = persistence_with_hint();
    let m = LaunchMachine::new_with_connector(Arc::new(NullObserver), p, Arc::new(connector));
    m.start().await;

    // The fixture's `expires_in` (3600) anchored on this clock at receipt —
    // exactly what a direct dial records.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(matches!(
        m.snapshot().token,
        TokenStatus::Valid { expires_at_secs } if (now + 3600 - 5..=now + 3600 + 5).contains(&expires_at_secs)
    ));
}

/// The session connector's half (`onboarding-provisioning.md` § Reach hint →
/// *The session connector's half*): the launch's verdict reaches the process
/// reach table the session's dials fall back to, and the deleter it carries is
/// this account's slot deletion. One test over the four verdicts rather than
/// four: the table is process-global and the file's tests run in parallel, so
/// every scenario here dials a host of its own and the sequence is asserted in
/// the order a session meets it.
#[tokio::test]
async fn the_launchs_verdict_reaches_the_session_connectors_reach_table() {
    use fauna_anon_client::reach_hint;

    // A hint-carried launch installs the hint — at the URL's own port, the
    // socket the launch's own hint dial used — and leaves the slot in place:
    // the session has not reached the domain yet.
    let p = Arc::new(
        InMemoryPersistence::new()
            .with_identity(test_secret().to_vec())
            .with_nest_url("https://hint-carried.example:8443")
            .with_reach_ipv4("203.0.113.9"),
    );
    let connector = MockAuthConnector::new()
        .push_silent_challenge(SilentChallengeOutcome::Transient {
            error: "dns: no such host".into(),
        })
        .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply()));
    let (_, p) = run(connector, p).await;
    assert_eq!(
        reach_hint::hint_for("hint-carried.example"),
        Some(std::net::SocketAddr::from(([203, 0, 113, 9], 8443))),
        "the session's dials fall back to the address the launch reached the box at"
    );
    assert_eq!(
        p.load_reach_ipv4().as_deref(),
        Some("203.0.113.9"),
        "installing is not spending: the slot stays until the domain answers"
    );
    // The first bearer socket that opens over the domain spends it, through
    // the deleter the launch bound to this persistence.
    reach_hint::domain_reached("hint-carried.example");
    assert_eq!(
        p.load_reach_ipv4(),
        None,
        "the session's first domain success deletes the persisted hint"
    );
    assert_eq!(reach_hint::hint_for("hint-carried.example"), None);

    // A domain-carried launch installs nothing: the hint is spent at launch.
    let p = Arc::new(
        InMemoryPersistence::new()
            .with_identity(test_secret().to_vec())
            .with_nest_url("https://domain-carried.example")
            .with_reach_ipv4("203.0.113.9"),
    );
    let connector = MockAuthConnector::new()
        .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply()));
    run(connector, p).await;
    assert_eq!(reach_hint::hint_for("domain-carried.example"), None);

    // A hint dial that also fails installs nothing: a failed fallback is not
    // an address the session should keep trying.
    let p = Arc::new(
        InMemoryPersistence::new()
            .with_identity(test_secret().to_vec())
            .with_nest_url("https://failed-hint.example")
            .with_reach_ipv4("203.0.113.9"),
    );
    let connector =
        MockAuthConnector::new().push_silent_challenge(SilentChallengeOutcome::Transient {
            error: "dns: no such host".into(),
        });
    run(connector, p).await;
    assert_eq!(reach_hint::hint_for("failed-hint.example"), None);

    // A launch phase forgets what an earlier launch installed before its own
    // domain dial, so that dial is never answered from the table and the
    // decision is re-made from the slot.
    reach_hint::install(
        "stale-entry.example",
        std::net::SocketAddr::from(([203, 0, 113, 9], 443)),
        Arc::new(|| {}),
    );
    let p = Arc::new(
        InMemoryPersistence::new()
            .with_identity(test_secret().to_vec())
            .with_nest_url("https://stale-entry.example"),
    );
    let connector = MockAuthConnector::new()
        .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply()));
    run(connector, p).await;
    assert_eq!(
        reach_hint::hint_for("stale-entry.example"),
        None,
        "the phase cleared the entry before dialling, and the domain carried the launch"
    );
}
