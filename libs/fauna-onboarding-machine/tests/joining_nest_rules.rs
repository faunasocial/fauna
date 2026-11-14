//! `joining_nest_rules` — the nest's rules on the joining surfaces
//! (`nest-rules.md` § Acceptance at joining): `invite_request`, and
//! `handle_entry` while its Continue signs in (`terms_notice_visible`), read
//! before sign-in and verified against the identity the connection proved.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use fauna_client_policy::{NestRulesNotApplied, NestRulesSummary};
use fauna_core::content_policy::{
    ContentPolicyDocument, ContentRule, ContentVerdict, GRAMMAR_VERSION, REASON_DEFAULT_KEY,
    encode_document, sign_nest_policy,
};
use fauna_core::identity::ActorKeypair;
use fauna_onboarding_machine::nest_api::{FakeNestApi, NestRulesAnswer, NestRulesError};
use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::state::OnboardingStep;
use fauna_onboarding_machine::{HandleCheckOutcome, OnboardingMachine};
use fauna_protocol::content_policy::SignedContentPolicy;

const DOMAIN: &str = "rules.test";
const NEST_URL: &str = "https://rules.test";

fn nest() -> ActorKeypair {
    ActorKeypair::from_secret([7; 32])
}

fn one_rule() -> ContentPolicyDocument {
    let mut doc = ContentPolicyDocument::empty();
    doc.rules.push(ContentRule {
        factor: "spam".to_string(),
        min_permille: 350,
        verdict: ContentVerdict::Collapse,
        reason_code: "rule-1".to_string(),
        reason: BTreeMap::from([(REASON_DEFAULT_KEY.to_string(), "No spam".to_string())]),
        extra: BTreeMap::new(),
    });
    doc
}

fn signed_by(key: &ActorKeypair, document: &ContentPolicyDocument) -> SignedContentPolicy {
    let document = encode_document(document).unwrap();
    let signature = sign_nest_policy(key.signing_key(), &document, 1, 1_700_000_000).unwrap();
    SignedContentPolicy {
        document,
        sequence: 1,
        issued_at: 1_700_000_000,
        signature: signature.to_vec(),
        extra: Default::default(),
    }
}

fn answer(policy: Option<SignedContentPolicy>) -> Result<NestRulesAnswer, NestRulesError> {
    Ok(NestRulesAnswer {
        nest_id: nest().actor_id_hex(),
        domain: DOMAIN.to_string(),
        content_policy: policy,
    })
}

fn machine(
    reply: Result<NestRulesAnswer, NestRulesError>,
) -> (Arc<OnboardingMachine>, Arc<FakeNestApi>) {
    let api = Arc::new(FakeNestApi::new());
    api.set_nest_rules_response(reply);
    let m = OnboardingMachine::with_nest_api(Arc::new(NullObserver), api.clone());
    (m, api)
}

/// The read runs in the background: wait until it has finished, store
/// included. A fixed sleep after the fake records the call raced the
/// signature check and the store on a loaded machine. The background runtime
/// is one current-thread runtime that runs remote spawns in order, and the
/// fake's read never yields, so the read task finishes in a single poll. A
/// marker spawned behind it therefore runs only once the read is done.
async fn settle(api: &FakeNestApi) {
    let (done, finished) = tokio::sync::oneshot::channel();
    fauna_sleep::spawn_background(async move {
        let _ = done.send(());
    });
    tokio::time::timeout(Duration::from_secs(10), finished)
        .await
        .expect("the background runtime ran the read")
        .expect("the marker task ran");
    assert_eq!(api.nest_rules_reads().len(), 1, "exactly one read was made");
}

async fn on_invite_request(m: &OnboardingMachine, api: &FakeNestApi) -> Option<NestRulesSummary> {
    m.navigate_to_invite_request_for_known_nest(NEST_URL.into(), "bob@rules.test".into());
    settle(api).await;
    m.joining_nest_rules("en")
}

#[tokio::test]
async fn invite_request_shows_the_verified_rules() {
    let (m, api) = machine(answer(Some(signed_by(&nest(), &one_rule()))));
    let rules = on_invite_request(&m, &api).await.expect("rules shown");
    assert_eq!(api.nest_rules_reads(), vec![NEST_URL.to_string()]);
    assert_eq!(rules.domain, DOMAIN);
    assert_eq!(rules.rules.len(), 1);
    assert_eq!(rules.rules[0].reason, "No spam");
    assert_eq!(rules.not_applied, None);
}

/// A newer grammar's document decodes to no rules here, and joining still
/// accepts it: the surface shows the heading and the not-applied sentence.
#[tokio::test]
async fn a_newer_version_with_no_rules_is_still_shown() {
    let mut newer = ContentPolicyDocument::empty();
    newer.version = GRAMMAR_VERSION + 1;
    let (m, api) = machine(answer(Some(signed_by(&nest(), &newer))));
    let rules = on_invite_request(&m, &api)
        .await
        .expect("not-applied shown");
    assert!(rules.rules.is_empty());
    assert_eq!(rules.not_applied, Some(NestRulesNotApplied::NewerVersion));
}

#[tokio::test]
async fn an_applied_empty_document_shows_nothing() {
    let (m, api) = machine(answer(Some(signed_by(
        &nest(),
        &ContentPolicyDocument::empty(),
    ))));
    assert_eq!(on_invite_request(&m, &api).await, None);
}

#[tokio::test]
async fn a_nest_with_no_rules_shows_nothing() {
    let (m, api) = machine(answer(None));
    assert_eq!(on_invite_request(&m, &api).await, None);
}

/// Rules signed by another key than the one the connection proved are not
/// this nest's.
#[tokio::test]
async fn rules_signed_by_another_key_are_refused() {
    let other = ActorKeypair::from_secret([9; 32]);
    let (m, api) = machine(answer(Some(signed_by(&other, &one_rule()))));
    assert_eq!(on_invite_request(&m, &api).await, None);
}

#[tokio::test]
async fn a_failed_read_shows_nothing() {
    let (m, api) = machine(Err(NestRulesError::Transient {
        cause: "down".into(),
    }));
    assert_eq!(on_invite_request(&m, &api).await, None);
}

/// On `handle_entry` the rules show exactly while the terms notice does.
#[tokio::test]
async fn handle_entry_shows_them_only_while_continue_signs_in() {
    let (m, api) = machine(answer(Some(signed_by(&nest(), &one_rule()))));
    assert!(on_invite_request(&m, &api).await.is_some());

    m.set_step_for_test(OnboardingStep::HandleEntry);
    assert_eq!(m.joining_nest_rules("en"), None, "no outcome yet");

    m.set_handle_check_outcome_for_test(HandleCheckOutcome::AlreadyOnNest {
        handle_matches: true,
        current_handle: Some("bob".into()),
    });
    assert!(m.handle_check_snapshot().terms_notice_visible);
    assert!(m.joining_nest_rules("en").is_some());

    m.set_handle_check_outcome_for_test(HandleCheckOutcome::UnregisteredUnclaimedNest);
    assert_eq!(m.joining_nest_rules("en"), None);
}

#[tokio::test]
async fn other_steps_show_nothing() {
    let (m, api) = machine(answer(Some(signed_by(&nest(), &one_rule()))));
    assert!(on_invite_request(&m, &api).await.is_some());
    m.set_step_for_test(OnboardingStep::ClaimCode);
    assert_eq!(m.joining_nest_rules("en"), None);
}

#[tokio::test]
async fn a_reset_forgets_them() {
    let (m, api) = machine(answer(Some(signed_by(&nest(), &one_rule()))));
    assert!(on_invite_request(&m, &api).await.is_some());
    m.reset();
    m.set_step_for_test(OnboardingStep::InviteRequest);
    assert_eq!(m.joining_nest_rules("en"), None);
}
