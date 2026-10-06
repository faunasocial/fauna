use std::sync::Arc;

use fauna_atproto_settings_machine::NestConsentRow;
use fauna_protocol::atproto_pds::{AtprotoGrantInfo, AtprotoSessionInfo};
use fauna_protocol::nostr::BunkerAppEntry;
use fauna_protocol::oauth_consent::BlockedClient;
use fauna_protocol::principals::PrincipalInfo;

use super::*;
use crate::nest_api::{FakeCall, FakeConnectedAppsNestApi};
use crate::observer::CountingObserver;

fn machine() -> (
    Arc<ConnectedAppsMachine>,
    Arc<FakeConnectedAppsNestApi>,
    Arc<CountingObserver>,
) {
    let api = Arc::new(FakeConnectedAppsNestApi::new());
    let obs = CountingObserver::new();
    let m = ConnectedAppsMachine::new(obs.clone(), api.clone());
    (m, api, obs)
}

fn principal(id: u8, client_id: &str, label: Option<&str>, created: i64) -> PrincipalInfo {
    PrincipalInfo {
        principal_id: vec![id; 16],
        client_id: client_id.into(),
        execution_form: "device".into(),
        granted_scopes: "atproto transition:generic".into(),
        created_at: created,
        last_used_at: Some(created + 5),
        label: label.map(Into::into),
        live_grants: 1,
        ..Default::default()
    }
}

fn grant(id: u8, client_id: &str, expires: Option<i64>) -> AtprotoGrantInfo {
    AtprotoGrantInfo {
        grant_id: vec![id; 16],
        client_id: client_id.into(),
        client_name: Some("Legacy App".into()),
        scopes: "atproto".into(),
        created_at: 1_000,
        expires_at: expires,
        ..Default::default()
    }
}

fn session(id: u8, plane: &str, created: i64) -> AtprotoSessionInfo {
    AtprotoSessionInfo {
        session_id: vec![id; 16],
        plane: plane.into(),
        credential_id: Some("cred-1".into()),
        client_note: Some("Skeets for Mac".into()),
        created_at: created,
        last_refreshed_at: None,
        expires_at: created + 1_000_000,
        extra: Default::default(),
    }
}

fn signer(id: i64, label: &str, status: &str) -> BunkerAppEntry {
    BunkerAppEntry {
        id,
        label: label.into(),
        status: status.into(),
        created_at: 50,
        expires_at: 9_000,
        ..Default::default()
    }
}

fn consent(id: u8, client_id: &str) -> NestConsentRow {
    NestConsentRow {
        consent_id: vec![id; 16],
        code: format!("CODE{id}"),
        client_id: client_id.into(),
        client_name: Some("Requester".into()),
        scopes: vec!["atproto".into()],
        ..NestConsentRow::default()
    }
}

fn resolve(t: &LocalizedText) -> String {
    t.resolve(|k| match k {
        VERBATIM_KEY => Some("{text}"),
        _ => None,
    })
}

#[tokio::test]
async fn nothing_is_loaded_until_a_read_returns() {
    let (m, api, _) = machine();
    assert!(!m.snapshot().loaded);
    api.fail_all();
    m.refresh().await;
    let snap = m.snapshot();
    assert!(
        !snap.loaded,
        "a failed read must not license the empty state"
    );
    assert_eq!(snap.error.unwrap().key, REFRESH_ERROR_KEY);
}

#[tokio::test]
async fn an_empty_roster_is_loaded_and_empty() {
    let (m, _, obs) = machine();
    m.refresh().await;
    let snap = m.snapshot();
    assert!(snap.loaded);
    assert!(snap.principals.is_empty() && snap.requests.is_empty());
    assert_eq!(snap.error, None);
    assert_eq!(obs.count(), 1);
}

#[tokio::test]
async fn a_principal_row_is_composed_in_words_and_its_grants_are_not_listed_twice() {
    let (m, api, _) = machine();
    let cid = "https://App.Example:8443/client.json";
    api.set_principals(vec![principal(1, cid, Some("Example\nApp"), 100)]);
    api.set_grants(vec![grant(9, cid, Some(5_000)), grant(8, cid, Some(7_000))]);
    api.set_sessions(vec![session(9, "oauth", 100)]);
    m.refresh().await;
    let rows = m.snapshot().principals;
    assert_eq!(
        rows.len(),
        1,
        "one principal, its grants and oauth session folded in: {rows:?}"
    );
    let r = &rows[0];
    assert_eq!(r.key, format!("principal:{}", hex::encode([1u8; 16])));
    assert_eq!(r.class, "device");
    assert_eq!(
        resolve(&r.name),
        "ExampleApp",
        "control chars stripped at composition"
    );
    assert_eq!(r.client_id.as_deref(), Some(cid));
    assert_eq!(r.publisher.as_deref(), Some("app.example"));
    assert_eq!(r.scope_descriptions.len(), 2);
    assert!(
        r.scope_descriptions.iter().all(|d| d.key == VERBATIM_KEY),
        "scopes ride the one describe path"
    );
    assert_eq!(r.lasts_until_millis, Some(7_000));
    assert_eq!(r.last_used_at_millis, Some(105));
    assert!(r.connected);
}

#[tokio::test]
async fn an_open_ended_grant_makes_the_principal_open_ended_and_no_live_grant_is_not_connected() {
    let (m, api, _) = machine();
    let mut p = principal(1, "https://a.example/c.json", None, 1);
    p.live_grants = 0;
    api.set_principals(vec![p]);
    api.set_grants(vec![grant(1, "https://a.example/c.json", None)]);
    m.refresh().await;
    let r = &m.snapshot().principals[0];
    assert_eq!(r.lasts_until_millis, None);
    assert!(!r.connected);
    assert_eq!(
        resolve(&r.name),
        "https://a.example/c.json",
        "no label → the client id verbatim"
    );
}

#[tokio::test]
async fn app_password_sessions_and_signers_are_rows_of_the_same_roster_oldest_first() {
    let (m, api, _) = machine();
    api.set_principals(vec![principal(
        1,
        "https://a.example/c.json",
        Some("A"),
        200_000,
    )]);
    api.set_sessions(vec![session(2, "app_credential", 10)]);
    api.set_bunker(vec![
        signer(7, "", "pending"),
        signer(8, "Amethyst", "active"),
    ]);
    m.refresh().await;
    let rows = m.snapshot().principals;
    let classes: Vec<_> = rows.iter().map(|r| r.class.as_str()).collect();
    assert_eq!(classes, ["app_password", "signer", "signer", "device"]);
    assert_eq!(resolve(&rows[0].name), "Skeets for Mac");
    assert_eq!(rows[1].name.key, SIGNER_PENDING_KEY);
    assert!(!rows[1].connected);
    assert_eq!(resolve(&rows[2].name), "Amethyst");
    assert_eq!(rows[2].created_at_millis, 50_000, "bunker seconds → millis");
    assert_eq!(rows[2].scope_descriptions[0].key, SIGNER_SCOPE_KEY);
}

#[tokio::test]
async fn a_surface_the_account_lacks_contributes_nothing_and_is_no_error() {
    let (m, api, _) = machine();
    api.bunker_unavailable();
    m.refresh().await;
    assert_eq!(m.snapshot().error, None);
    assert!(m.snapshot().loaded);
}

#[tokio::test]
async fn the_machine_picks_the_revoke_verb_per_row_and_re_reads() {
    let (m, api, _) = machine();
    api.set_principals(vec![principal(1, "https://a.example/c.json", None, 1)]);
    api.set_sessions(vec![session(2, "app_credential", 2)]);
    api.set_bunker(vec![signer(7, "Nos", "active")]);
    m.refresh().await;
    for key in m
        .snapshot()
        .principals
        .iter()
        .map(|r| r.key.clone())
        .collect::<Vec<_>>()
    {
        m.revoke(key).await;
    }
    let calls = api.calls();
    assert!(calls.contains(&FakeCall::RevokePrincipal(vec![1; 16])));
    assert!(calls.contains(&FakeCall::RevokeSession(vec![2; 16])));
    assert!(calls.contains(&FakeCall::RevokeBunkerApp(7)));
    let snap = m.snapshot();
    assert!(
        snap.principals.is_empty(),
        "non-optimistic: the re-read paints the result"
    );
    assert_eq!(snap.error, None);
}

#[tokio::test]
async fn a_failed_revoke_keeps_the_row_and_shows_the_error() {
    let (m, api, _) = machine();
    api.set_principals(vec![principal(1, "https://a.example/c.json", None, 1)]);
    m.refresh().await;
    api.fail_revokes();
    let key = m.snapshot().principals[0].key.clone();
    m.revoke(key).await;
    let snap = m.snapshot();
    assert_eq!(snap.principals.len(), 1);
    assert_eq!(snap.error.unwrap().key, REVOKE_ERROR_KEY);
}

#[tokio::test]
async fn a_typed_code_claims_the_request_and_its_card_is_the_built_card() {
    let (m, api, _) = machine();
    api.add_code("WDJB-MJHT", consent(5, "https://tv.example/c.json"));
    m.submit_code("wdjb mjht".into()).await;
    let snap = m.snapshot();
    assert_eq!(snap.error, None);
    assert_eq!(snap.requests.len(), 1);
    assert_eq!(
        snap.requests[0],
        consent_card_row(consent(5, "https://tv.example/c.json"), &Default::default())
    );
    assert!(
        api.calls()
            .contains(&FakeCall::LookupCode("wdjb mjht".into()))
    );
}

#[tokio::test]
async fn every_code_miss_reads_the_same() {
    let (m, api, _) = machine();
    m.submit_code("NOPE-NOPE".into()).await;
    let miss = m.snapshot().error;
    m.submit_code("   ".into()).await;
    let blank = m.snapshot().error;
    api.fail_all();
    m.submit_code("WDJB-MJHT".into()).await;
    let fault = m.snapshot().error;
    assert_eq!(miss.as_ref().unwrap().key, CODE_EXPIRED_KEY);
    assert_eq!(miss, blank);
    assert_eq!(
        miss, fault,
        "a transport fault never tells the user more than a miss"
    );
}

const HANDLE: &str = "urn:ietf:params:oauth:request_uri:abc_DEF-123";

#[tokio::test]
async fn an_opened_handoff_shows_its_card_in_the_tray() {
    let (m, api, obs) = machine();
    api.add_handoff(HANDLE, consent(6, "https://cli.example/c.json"));
    m.open_handoff(HANDLE.into()).await;
    let snap = m.snapshot();
    assert_eq!(snap.error, None);
    assert_eq!(
        snap.requests,
        vec![consent_card_row(
            consent(6, "https://cli.example/c.json"),
            &Default::default()
        )]
    );
    assert!(api.calls().contains(&FakeCall::OpenHandoff(HANDLE.into())));
    assert_eq!(obs.count(), 1, "one gesture, one notification");
}

#[tokio::test]
async fn an_opened_handoff_paints_even_when_the_re_list_misses_it() {
    let (m, api, _) = machine();
    api.add_handoff(HANDLE, consent(6, "https://cli.example/c.json"));
    api.drop_opened_from_list();
    m.open_handoff(HANDLE.into()).await;
    assert_eq!(
        m.snapshot().requests,
        vec![consent_card_row(
            consent(6, "https://cli.example/c.json"),
            &Default::default()
        )]
    );
}

#[tokio::test]
async fn every_handoff_miss_reads_the_same_and_the_tray_is_what_the_nest_says() {
    let (m, api, _) = machine();
    api.set_consents(vec![consent(1, "https://other.example/c.json")]);
    m.open_handoff(HANDLE.into()).await;
    let miss = m.snapshot();
    // A second open of a spent handle is a miss like any other.
    api.add_handoff(HANDLE, consent(6, "https://cli.example/c.json"));
    m.open_handoff(HANDLE.into()).await;
    m.open_handoff(HANDLE.into()).await;
    let spent = m.snapshot().error;
    api.fail_all();
    m.open_handoff(HANDLE.into()).await;
    let fault = m.snapshot().error;
    assert_eq!(miss.error.as_ref().unwrap().key, HANDOFF_EXPIRED_KEY);
    assert_eq!(
        miss.requests,
        vec![consent_card_row(
            consent(1, "https://other.example/c.json"),
            &Default::default()
        )]
    );
    assert_eq!(miss.error, spent);
    assert_eq!(
        miss.error, fault,
        "a transport fault never tells the user more than a miss"
    );
}

#[tokio::test]
async fn approving_a_request_resolves_it_and_it_leaves_the_tray() {
    let (m, api, _) = machine();
    api.set_consents(vec![consent(4, "https://a.example/c.json")]);
    m.refresh().await;
    let id = m.snapshot().requests[0].consent_id_hex.clone();
    m.resolve_request(id, true).await;
    assert!(
        api.calls()
            .contains(&FakeCall::ResolveConsent(vec![4; 16], true))
    );
    assert!(m.snapshot().requests.is_empty());
    assert_eq!(m.snapshot().error, None);
}

/// A records request from `https://app.example.com/…` whose document declares
/// one kind, with the holder key attested.
fn records_consent(id: u8) -> NestConsentRow {
    let key = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]);
    let did = fauna_protocol::kind_manifest::ed25519_did_key(&key.verifying_key().to_bytes());
    let payload = serde_json::json!({
        "version": 1,
        "publisher": { "domain": "app.example.com", "key": did },
        "kinds": [{
            "kind": "ext.app.example.com.notes",
            "class": "state", "merge": "latest-wins", "floor": "none"
        }],
    });
    NestConsentRow {
        scopes: vec![
            "atproto".into(),
            "fauna:records:rw:ext.app.example.com.*".into(),
        ],
        holder_x25519: Some(vec![9; 32]),
        fauna_manifest: Some(fauna_protocol::kind_manifest::sign_manifest(
            &key, &payload, None,
        )),
        ..consent(id, "https://app.example.com/client-metadata.json")
    }
}

/// The tray answers a records request through the AT Protocol page's same
/// path: the grant is deposited before the resolve, and its `Mint` is in the
/// owner's log.
#[tokio::test]
async fn approving_a_records_request_deposits_its_grant_before_resolving() {
    let (m, api, _) = machine();
    let owner = fauna_core::identity::ActorKeypair::generate();
    let ledger = Arc::new(
        fauna_client_config::test_helpers::FakeSuccessionLedgerStore::empty(owner.actor_id()),
    );
    m.set_consent_grant_seams(Arc::new(ConsentGrantSeams::from_keypair(
        &owner,
        Arc::new(fauna_client_config::test_helpers::FakeKindManifestStore::empty()),
        ledger.clone(),
    )));
    api.set_consents(vec![records_consent(4)]);
    m.refresh().await;

    let id = m.snapshot().requests[0].consent_id_hex.clone();
    m.resolve_request(id, true).await;

    let order: Vec<_> = api
        .calls()
        .into_iter()
        .filter(|c| matches!(c, FakeCall::MintGrant(_) | FakeCall::ResolveConsent(..)))
        .map(|c| matches!(c, FakeCall::MintGrant(_)))
        .collect();
    assert_eq!(order, [true, false], "mint, then resolve");
    assert_eq!(m.snapshot().error, None);
    assert_eq!(ledger.current().grant_events.len(), 1);
}

/// A machine wired with the consent-grant seams over an owner log holding one
/// third-party grant (window end `window_end` seconds) to holder `[9; 32]`.
async fn machine_with_grant_log(
    window_end: u64,
) -> (
    Arc<ConnectedAppsMachine>,
    Arc<FakeConnectedAppsNestApi>,
    Arc<fauna_client_config::test_helpers::FakeSuccessionLedgerStore>,
) {
    use fauna_client_capabilities::grant_log::{
        GrantEventSigner, KeypairGrantEventSigner, build_mint_event,
    };
    use fauna_client_config::SuccessionLedgerStore;
    use fauna_core::grant_event::GrantEventScope;
    let (m, api, _) = machine();
    let owner = fauna_core::identity::ActorKeypair::generate();
    let ledger = Arc::new(
        fauna_client_config::test_helpers::FakeSuccessionLedgerStore::empty(owner.actor_id()),
    );
    let scope = GrantEventScope {
        class: "content.read".into(),
        kind: Some("ext.app.example.com.notes".into()),
        tier: None,
    };
    let mint = KeypairGrantEventSigner::new(&owner)
        .sign_grant_event(build_mint_event(
            [7; 16],
            [9; 32],
            vec![scope],
            0,
            window_end,
            1,
        ))
        .unwrap();
    ledger
        .merge(
            fauna_core::succession_ledger::SuccessionLedger::events_replica(
                owner.actor_id(),
                vec![mint],
            ),
        )
        .await
        .unwrap();
    m.set_consent_grant_seams(Arc::new(ConsentGrantSeams::from_keypair(
        &owner,
        Arc::new(fauna_client_config::test_helpers::FakeKindManifestStore::empty()),
        ledger.clone(),
    )));
    (m, api, ledger)
}

fn keyed_principal(id: u8, client_id: &str) -> PrincipalInfo {
    PrincipalInfo {
        holder_x25519: Some(fauna_protocol::ByteBuf::from(vec![9; 32])),
        ..principal(id, client_id, Some("Notes"), 1)
    }
}

/// The row's *lasts-until* is the trust facet's: the capability grant's window
/// end, not the OAuth horizon beside it.
#[tokio::test]
async fn a_principals_lasts_until_is_its_trust_facets() {
    let (m, api, _) = machine_with_grant_log(4_000_000_000).await;
    let cid = "https://app.example.com/client-metadata.json";
    api.set_principals(vec![keyed_principal(1, cid)]);
    api.set_grants(vec![grant(1, cid, Some(5_000))]);
    m.refresh().await;
    assert_eq!(
        m.snapshot().principals[0].lasts_until_millis,
        Some(4_000_000_000_000)
    );
}

/// A principal the facet holds nothing for — another key, or none attested —
/// reads the declared OAuth fallback.
#[tokio::test]
async fn a_principal_with_no_capability_grant_reads_its_oauth_horizon() {
    let (m, api, _) = machine_with_grant_log(4_000_000_000).await;
    let other = PrincipalInfo {
        holder_x25519: Some(fauna_protocol::ByteBuf::from(vec![8; 32])),
        ..principal(1, "https://a.example/c.json", None, 1)
    };
    let keyless = principal(2, "https://b.example/c.json", None, 2);
    api.set_principals(vec![other, keyless]);
    api.set_grants(vec![
        grant(1, "https://a.example/c.json", Some(5_000)),
        grant(2, "https://b.example/c.json", Some(6_000)),
    ]);
    m.refresh().await;
    let until: Vec<_> = m
        .snapshot()
        .principals
        .iter()
        .map(|r| r.lasts_until_millis)
        .collect();
    assert_eq!(until, [Some(5_000), Some(6_000)]);
}

/// Revoking a principal records a `Revoke` for each capability grant the nest
/// ended with it, so the history lens can say why it vanished.
#[tokio::test]
async fn revoking_a_principal_records_the_revoke_of_its_capability_grants() {
    let (m, api, ledger) = machine_with_grant_log(4_000_000_000).await;
    api.set_principals(vec![keyed_principal(1, "https://app.example.com/c.json")]);
    m.refresh().await;
    let key = m.snapshot().principals[0].key.clone();
    m.revoke(key).await;

    let log = ledger.current();
    let history = fauna_client_capabilities::view_model::principal_history(&log, &[9; 32]);
    let kinds: Vec<_> = history.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        [
            fauna_core::grant_event::GrantEventKind::Revoke,
            fauna_core::grant_event::GrantEventKind::Mint
        ]
    );
    assert!(
        fauna_client_capabilities::view_model::principal_trust_facet(&log, &[9; 32], 0)
            .grants
            .is_empty()
    );
}

/// A failed principal revoke records nothing: the grants still stand.
#[tokio::test]
async fn a_failed_principal_revoke_records_no_revoke() {
    let (m, api, ledger) = machine_with_grant_log(4_000_000_000).await;
    api.set_principals(vec![keyed_principal(1, "https://app.example.com/c.json")]);
    m.refresh().await;
    api.fail_revokes();
    let key = m.snapshot().principals[0].key.clone();
    m.revoke(key).await;
    assert_eq!(ledger.current().grant_events.len(), 1);
}

/// Without the seams the trust facet is not wired, and the row is the
/// roster-only one, never the page error.
#[tokio::test]
async fn an_unwired_machine_reads_the_roster_only_row() {
    let (m, api, _) = machine();
    let cid = "https://app.example.com/c.json";
    api.set_principals(vec![keyed_principal(1, cid)]);
    api.set_grants(vec![grant(1, cid, Some(5_000))]);
    m.refresh().await;
    let snap = m.snapshot();
    assert_eq!(snap.principals[0].lasts_until_millis, Some(5_000));
    assert_eq!(snap.error, None);
}

/// Without the seams the tray refuses a records approve and resolves nothing.
#[tokio::test]
async fn an_unwired_tray_refuses_a_records_approve() {
    let (m, api, _) = machine();
    api.set_consents(vec![records_consent(4)]);
    m.refresh().await;

    let id = m.snapshot().requests[0].consent_id_hex.clone();
    m.resolve_request(id, true).await;

    assert!(
        !api.calls()
            .iter()
            .any(|c| matches!(c, FakeCall::ResolveConsent(..) | FakeCall::MintGrant(_)))
    );
    assert_eq!(m.snapshot().error.unwrap().key, RESOLVE_ERROR_KEY);
}

#[tokio::test]
async fn resolving_a_request_that_is_gone_says_so() {
    let (m, api, _) = machine();
    api.set_consents(vec![consent(4, "https://a.example/c.json")]);
    m.refresh().await;
    api.set_consents(Vec::new());
    let id = m.snapshot().requests[0].consent_id_hex.clone();
    m.resolve_request(id, false).await;
    assert_eq!(m.snapshot().error.unwrap().key, REQUEST_GONE_KEY);
}

#[tokio::test]
async fn never_show_blocks_the_client_declines_the_request_and_hides_it() {
    let (m, api, _) = machine();
    let cid = "https://pushy.example/c.json";
    api.set_consents(vec![consent(6, cid)]);
    m.refresh().await;
    let id = m.snapshot().requests[0].consent_id_hex.clone();
    m.block_request(id).await;
    let calls = api.calls();
    assert!(calls.contains(&FakeCall::BlockClient(cid.into(), true)));
    assert!(calls.contains(&FakeCall::ResolveConsent(vec![6; 16], false)));
    let snap = m.snapshot();
    assert!(snap.requests.is_empty());
    assert_eq!(snap.blocked.len(), 1);
    assert_eq!(snap.blocked[0].client_id, cid);

    m.unblock(cid.into()).await;
    assert!(
        api.calls()
            .contains(&FakeCall::BlockClient(cid.into(), false))
    );
    assert!(m.snapshot().blocked.is_empty());
}

#[tokio::test]
async fn a_request_from_a_blocked_client_never_renders() {
    let (m, api, _) = machine();
    let cid = "https://pushy.example/c.json";
    api.set_blocked(vec![BlockedClient {
        client_id: cid.into(),
        blocked_at: 1,
        extra: Default::default(),
    }]);
    api.set_consents(vec![
        consent(6, cid),
        consent(7, "https://ok.example/c.json"),
    ]);
    m.refresh().await;
    let requests = m.snapshot().requests;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].client_id, "https://ok.example/c.json");
}

fn mail_credential(id: &str, name: &str, created: u64, revoked: bool) -> MailCredentialSummary {
    MailCredentialSummary {
        credential_id: id.into(),
        display_name: name.into(),
        kind: fauna_client_mail_settings::CredentialKind::Plain,
        created_at: created,
        mua_username: format!("{{handle}}+{id}@nest.example"),
        revoked,
    }
}

#[tokio::test]
async fn a_mail_app_password_is_a_roster_row_with_its_login_kind_and_reach() {
    let (m, api, _) = machine();
    api.set_mail_credentials(vec![mail_credential("phone", "Phone", 20, false)]);
    m.refresh().await;
    let rows = m.snapshot().principals;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let r = &rows[0];
    assert_eq!(r.key, "mail:phone");
    assert_eq!(r.class, class::APP_PASSWORD);
    assert_eq!(resolve(&r.name), "Phone");
    assert_eq!(r.created_at_millis, 20_000, "custody seconds → millis");
    assert_eq!(r.lasts_until_millis, None, "open-ended");
    assert_eq!(r.scope_descriptions[0].key, MAIL_SCOPE_KEY);
    assert!(r.connected);
    let mail = r.mail.as_ref().expect("a mail row carries its mail half");
    assert_eq!(mail.mua_username, "{handle}+phone@nest.example");
    assert_eq!(mail.kind.key, "settings.mail.kind_password");
    assert!(!mail.revoked);
}

/// The roster is mixed-class: an ATProto app-password session shares the mail
/// rows' class, so only `mail` tells them apart.
#[tokio::test]
async fn only_a_mail_row_carries_the_mail_half() {
    let (m, api, _) = machine();
    api.set_sessions(vec![session(2, "app_credential", 10)]);
    api.set_mail_credentials(vec![mail_credential("default", "Default", 20, false)]);
    m.refresh().await;
    let rows = m.snapshot().principals;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.class == class::APP_PASSWORD));
    let mail: Vec<_> = rows.iter().filter(|r| r.mail.is_some()).collect();
    assert_eq!(mail.len(), 1);
    assert_eq!(mail[0].key, "mail:default");
}

#[tokio::test]
async fn a_burned_mail_password_stays_listed_and_reads_as_not_connected() {
    let (m, api, _) = machine();
    api.set_mail_credentials(vec![mail_credential("default", "Default", 20, true)]);
    m.refresh().await;
    let r = &m.snapshot().principals[0];
    assert!(!r.connected);
    assert!(r.mail.as_ref().unwrap().revoked);
}

/// Mail passwords made in the same second keep the mail machine's order — the
/// row key must not re-sort them.
#[tokio::test]
async fn mail_rows_made_in_the_same_second_keep_the_mail_machines_order() {
    let (m, api, _) = machine();
    api.set_mail_credentials(vec![
        mail_credential("zeta", "Zeta", 20, false),
        mail_credential("alpha", "Alpha", 20, false),
        mail_credential("first", "First", 5, false),
    ]);
    m.refresh().await;
    let keys: Vec<_> = m
        .snapshot()
        .principals
        .iter()
        .map(|r| r.key.clone())
        .collect();
    assert_eq!(keys, ["mail:first", "mail:zeta", "mail:alpha"]);
}

/// The mail rows come from the mail-settings machine, not the nest roster: a
/// failed mail read shows the page error and keeps the mail rows on screen,
/// and never holds back the rows every other read returned.
#[tokio::test]
async fn a_failed_mail_read_keeps_the_mail_rows_and_the_rest_of_the_roster() {
    let (m, api, _) = machine();
    api.set_mail_credentials(vec![mail_credential("phone", "Phone", 20, false)]);
    m.refresh().await;
    api.fail_mail();
    api.set_principals(vec![principal(1, "https://a.example/c.json", None, 1)]);
    m.refresh().await;
    let snap = m.snapshot();
    assert_eq!(snap.error.unwrap().key, REFRESH_ERROR_KEY);
    let keys: Vec<_> = snap.principals.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            format!("principal:{}", hex::encode([1u8; 16])).as_str(),
            "mail:phone"
        ]
    );
}

#[tokio::test]
async fn revoking_a_mail_row_is_the_mail_revoke_and_the_row_is_gone_on_re_read() {
    let (m, api, _) = machine();
    api.set_mail_credentials(vec![
        mail_credential("default", "Default", 10, false),
        mail_credential("phone", "Phone", 20, false),
    ]);
    m.refresh().await;
    m.revoke("mail:phone".into()).await;
    assert!(
        api.calls()
            .contains(&FakeCall::RevokeMailCredential("phone".into()))
    );
    let snap = m.snapshot();
    assert_eq!(snap.error, None);
    assert_eq!(
        snap.principals
            .iter()
            .map(|r| r.key.as_str())
            .collect::<Vec<_>>(),
        ["mail:default"]
    );
}

#[tokio::test]
async fn a_failed_mail_revoke_keeps_the_row_and_shows_the_error() {
    let (m, api, _) = machine();
    api.set_mail_credentials(vec![mail_credential("phone", "Phone", 20, false)]);
    m.refresh().await;
    api.fail_revokes();
    m.revoke("mail:phone".into()).await;
    let snap = m.snapshot();
    assert_eq!(snap.principals.len(), 1);
    assert_eq!(snap.error.unwrap().key, REVOKE_ERROR_KEY);
}

#[tokio::test]
async fn a_mail_secret_is_read_on_demand_and_never_enters_the_snapshot() {
    let (m, api, obs) = machine();
    api.set_mail_credentials(vec![mail_credential("phone", "Phone", 20, false)]);
    m.refresh().await;
    let before = obs.count();
    let secret = m.reveal_secret("mail:phone".into()).await;
    assert_eq!(secret.as_ref().map(|s| s.as_str()), Some("secret-phone"));
    assert!(obs.count() > before);
    let snap = m.snapshot();
    assert_eq!(snap.error, None);
    assert!(
        !format!("{snap:?}").contains("secret-phone"),
        "the passive snapshot carries no secret"
    );
}

#[tokio::test]
async fn a_secret_read_that_fails_says_so_and_only_a_mail_row_has_a_secret() {
    let (m, api, _) = machine();
    api.set_principals(vec![principal(1, "https://a.example/c.json", None, 1)]);
    m.refresh().await;
    let key = m.snapshot().principals[0].key.clone();
    assert!(m.reveal_secret(key).await.is_none());
    assert!(
        !api.calls()
            .iter()
            .any(|c| matches!(c, FakeCall::RevealMailSecret(_))),
        "a row of another class is never asked for a secret"
    );
    assert_eq!(m.snapshot().error, None);

    assert!(m.reveal_secret("mail:gone".into()).await.is_none());
    assert_eq!(m.snapshot().error.unwrap().key, SECRET_ERROR_KEY);
}

#[test]
fn the_publisher_is_the_client_ids_host() {
    assert_eq!(
        publisher_domain("https://A.example/x.json").as_deref(),
        Some("a.example")
    );
    assert_eq!(
        publisher_domain("http://localhost?redirect_uri=x").as_deref(),
        Some("localhost")
    );
    assert_eq!(
        publisher_domain("https://u@h.example:1/").as_deref(),
        Some("h.example")
    );
    assert_eq!(publisher_domain("not a url"), None);
}

// ── Key replacement: the owner's end at the approve ─────────────────────────

const CID: &str = "https://app.example.com/c.json";
const OLD_WRITER: [u8; 32] = [0x0A; 32];

/// A machine wired with the seams over an owner log holding, for holder
/// `[9; 32]`: a read grant (`[7; 16]`) and a grant licensing `OLD_WRITER`
/// (`[6; 16]`); and a roster naming that holder and writer for [`CID`].
async fn machine_replacing_keys() -> (
    Arc<ConnectedAppsMachine>,
    Arc<FakeConnectedAppsNestApi>,
    Arc<fauna_client_config::test_helpers::FakeSuccessionLedgerStore>,
) {
    use fauna_client_capabilities::grant_log::{
        GrantEventSigner, KeypairGrantEventSigner, build_mint_event,
    };
    use fauna_client_config::SuccessionLedgerStore;
    use fauna_core::grant_event::{CLASS_CONTENT_WRITE, GrantEventScope, writer_factor};
    let (m, api, _) = machine();
    let owner = fauna_core::identity::ActorKeypair::generate();
    let ledger = Arc::new(
        fauna_client_config::test_helpers::FakeSuccessionLedgerStore::empty(owner.actor_id()),
    );
    let read = GrantEventScope {
        class: "content.read".into(),
        kind: Some("ext.app.example.com.notes".into()),
        tier: None,
    };
    let write = GrantEventScope {
        class: CLASS_CONTENT_WRITE.into(),
        ..read.clone()
    }
    .with_factor(&writer_factor(&OLD_WRITER));
    let signer = KeypairGrantEventSigner::new(&owner);
    let mints = [([7; 16], vec![read.clone()]), ([6; 16], vec![read, write])]
        .into_iter()
        .map(|(id, scope)| {
            signer
                .sign_grant_event(build_mint_event(id, [9; 32], scope, 0, 4_000_000_000, 1))
                .unwrap()
        })
        .collect();
    ledger
        .merge(
            fauna_core::succession_ledger::SuccessionLedger::events_replica(
                owner.actor_id(),
                mints,
            ),
        )
        .await
        .unwrap();
    m.set_consent_grant_seams(Arc::new(ConsentGrantSeams::from_keypair(
        &owner,
        Arc::new(fauna_client_config::test_helpers::FakeKindManifestStore::empty()),
        ledger.clone(),
    )));
    api.set_principals(vec![PrincipalInfo {
        writer_ed25519: Some(fauna_protocol::ByteBuf::from(OLD_WRITER.to_vec())),
        ..keyed_principal(1, CID)
    }]);
    (m, api, ledger)
}

/// A consent for [`CID`] attesting `holder` and `writer`.
fn rekeyed_consent(holder: Option<[u8; 32]>, writer: Option<[u8; 32]>) -> NestConsentRow {
    NestConsentRow {
        holder_x25519: holder.map(|h| h.to_vec()),
        writer_ed25519: writer.map(|w| w.to_vec()),
        ..consent(4, CID)
    }
}

fn revoked_grants(api: &FakeConnectedAppsNestApi) -> Vec<u8> {
    let mut ids: Vec<_> = api
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            FakeCall::RevokeGrant(id) => Some(id[0]),
            _ => None,
        })
        .collect();
    ids.sort_unstable();
    ids
}

/// The ids of the grants the owner's log records a `Revoke` for.
fn recorded_revokes(
    ledger: &fauna_client_config::test_helpers::FakeSuccessionLedgerStore,
) -> Vec<u8> {
    let mut ids: Vec<_> = ledger
        .current()
        .grant_events
        .iter()
        .filter(|e| e.kind == fauna_core::grant_event::GrantEventKind::Revoke)
        .map(|e| e.grant_id[0])
        .collect();
    ids.sort_unstable();
    ids
}

async fn approve_only_request(m: &ConnectedAppsMachine) {
    let id = m.snapshot().requests[0].consent_id_hex.clone();
    m.resolve_request(id, true).await;
}

#[tokio::test]
async fn an_approve_replacing_the_holder_ends_and_records_the_old_keys_grants() {
    let (m, api, ledger) = machine_replacing_keys().await;
    api.set_consents(vec![rekeyed_consent(Some([0x10; 32]), Some(OLD_WRITER))]);
    m.refresh().await;
    assert_eq!(
        m.snapshot().requests[0]
            .ends
            .as_ref()
            .map(|e| e.key.as_str()),
        Some(ENDS_HOLDER_KEY),
        "the card says what the approve ends"
    );

    approve_only_request(&m).await;

    let calls = api.calls();
    let resolved = calls
        .iter()
        .position(|c| matches!(c, FakeCall::ResolveConsent(_, true)))
        .unwrap();
    let first_revoke = calls
        .iter()
        .position(|c| matches!(c, FakeCall::RevokeGrant(_)))
        .unwrap();
    assert!(
        resolved < first_revoke,
        "the resolve answers before anything ends"
    );
    assert_eq!(revoked_grants(&api), [6, 7], "the nest first");
    assert_eq!(recorded_revokes(&ledger), [6, 7], "then the owner's log");
    assert_eq!(m.snapshot().error, None);
}

#[tokio::test]
async fn an_approve_replacing_the_writer_ends_only_the_old_writers_grants() {
    let (m, api, ledger) = machine_replacing_keys().await;
    api.set_consents(vec![rekeyed_consent(Some([9; 32]), Some([0x0B; 32]))]);
    m.refresh().await;
    assert_eq!(
        m.snapshot().requests[0]
            .ends
            .as_ref()
            .map(|e| e.key.as_str()),
        Some(ENDS_WRITER_KEY)
    );

    approve_only_request(&m).await;

    assert_eq!(revoked_grants(&api), [6]);
    assert_eq!(recorded_revokes(&ledger), [6], "the read grant stays");
}

#[tokio::test]
async fn a_failed_approve_ends_nothing() {
    let (m, api, ledger) = machine_replacing_keys().await;
    api.set_consents(vec![rekeyed_consent(Some([0x10; 32]), None)]);
    m.refresh().await;
    // Answered elsewhere before this approve lands: the resolve says `false`.
    api.set_consents(Vec::new());

    approve_only_request(&m).await;

    assert!(revoked_grants(&api).is_empty());
    assert!(recorded_revokes(&ledger).is_empty());
    assert_eq!(m.snapshot().error.unwrap().key, REQUEST_GONE_KEY);
}

#[tokio::test]
async fn a_same_key_or_keyless_approve_ends_nothing_and_says_nothing() {
    for row in [
        rekeyed_consent(Some([9; 32]), Some(OLD_WRITER)),
        rekeyed_consent(None, None),
    ] {
        let (m, api, ledger) = machine_replacing_keys().await;
        api.set_consents(vec![row]);
        m.refresh().await;
        assert_eq!(m.snapshot().requests[0].ends, None);

        approve_only_request(&m).await;

        assert!(revoked_grants(&api).is_empty());
        assert!(recorded_revokes(&ledger).is_empty());
    }
}

#[tokio::test]
async fn a_first_ceremony_replaces_nothing() {
    let (m, api, ledger) = machine_replacing_keys().await;
    api.set_principals(Vec::new());
    api.set_consents(vec![rekeyed_consent(Some([0x10; 32]), Some([0x0B; 32]))]);
    m.refresh().await;
    assert_eq!(m.snapshot().requests[0].ends, None);

    approve_only_request(&m).await;

    assert!(revoked_grants(&api).is_empty());
    assert!(recorded_revokes(&ledger).is_empty());
}

/// A grant the nest refused to end is left unrecorded: the log never says
/// "revoked" over a grant the nest still holds.
#[tokio::test]
async fn a_nest_refusal_leaves_the_log_unwritten() {
    let (m, api, ledger) = machine_replacing_keys().await;
    api.set_consents(vec![rekeyed_consent(Some([0x10; 32]), None)]);
    m.refresh().await;
    api.fail_revokes();

    approve_only_request(&m).await;

    assert_eq!(revoked_grants(&api), [6, 7], "both were attempted");
    assert!(recorded_revokes(&ledger).is_empty());
    assert_eq!(m.snapshot().error, None, "the approve itself landed");
}
