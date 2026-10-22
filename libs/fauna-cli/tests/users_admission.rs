//! tier_1: the users page's supervised admission and its registration read
//! (`admin.md` § 2 Users; `apps/cli.md` § Implementation status today) — the
//! request each admission tool sends carries the guardian and the band as the
//! wire spells them, and the posture is read off the shell's own public status
//! read, never the principal session.

use std::sync::Arc;

use fauna_cli::tools::ConnectStarted;
use fauna_cli::{Host, Server, SessionUnavailable, ToolError};
use fauna_client_testkit::{CapturingRequester, block_on};
use fauna_protocol::age::AgeBand;
use fauna_protocol::discovery::SetupStatusReply;
use serde_json::{Value, json};

struct AdmissionHost {
    session: Arc<CapturingRequester>,
}

impl Host for AdmissionHost {
    type Rpc = Arc<CapturingRequester>;

    async fn session(&self, _account: Option<&str>) -> Result<Self::Rpc, SessionUnavailable> {
        Ok(self.session.clone())
    }

    async fn account(&self, _account: Option<&str>) -> Result<String, SessionUnavailable> {
        Ok("a1".repeat(32))
    }

    async fn redial(
        &self,
        _account: Option<&str>,
        _lost: &Self::Rpc,
    ) -> Result<Self::Rpc, SessionUnavailable> {
        Ok(self.session.clone())
    }

    async fn connect(&self, handle: &str, _admin: bool) -> Result<ConnectStarted, ToolError> {
        Ok(ConnectStarted {
            account: handle.into(),
            user_code: "WXYZ-1234".into(),
            nest: format!("https://{handle}"),
            expires_in: 600,
            custody: None,
        })
    }

    async fn setup_status(&self, _account: Option<&str>) -> Result<SetupStatusReply, ToolError> {
        Ok(SetupStatusReply {
            registration_mode: Some("invite_required".into()),
            max_free_users: Some(25),
            ..Default::default()
        })
    }
}

fn server() -> (Server<AdmissionHost>, Arc<CapturingRequester>) {
    let session = Arc::new(CapturingRequester::transport_fault());
    (
        Server::new(AdmissionHost {
            session: session.clone(),
        }),
        session,
    )
}

fn call(server: &Server<AdmissionHost>, name: &str, arguments: Value) -> Value {
    let message = json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                          "params": { "name": name, "arguments": arguments } });
    block_on(server.handle(&message.to_string())).expect("answered")["result"].clone()
}

/// The `guardian_actor` field as the wire carries it: the raw 32 bytes.
fn guardian_bytes(payload: &Value) -> Vec<u8> {
    serde_json::from_value(payload["guardian_actor"].clone()).expect("guardian_actor as bytes")
}

#[test]
fn the_three_admission_tools_send_the_guardian_and_the_band() {
    let guardian = "5e".repeat(32);
    let band = AgeBand::Teen13To15.as_str();
    let supervised = json!({ "guardian_actor": guardian, "age_band": band });
    let (s, session) = server();
    for (tool, mut args) in [
        (
            "admin_users_admit",
            json!({ "actor_id": "c3".repeat(32), "tier": "free", "handle": "kid" }),
        ),
        (
            "admin_invite_codes_create",
            json!({ "tier": "free", "uses": 1 }),
        ),
        (
            "admin_invite_requests_approve",
            json!({ "id": 4, "tier": "free" }),
        ),
    ] {
        args.as_object_mut()
            .unwrap()
            .extend(supervised.as_object().unwrap().clone());
        // The double never answers: the request it captured is the assertion.
        call(&s, tool, args);
    }
    let calls = session.calls();
    assert_eq!(
        calls.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
        [
            "fauna.admin.users.create",
            "fauna.admin.invite_codes.create",
            "fauna.admin.invite_requests.approve"
        ]
    );
    for (kind, payload) in &calls {
        assert_eq!(guardian_bytes(payload), vec![0x5e; 32], "{kind}");
        assert_eq!(payload["age_band"], band, "{kind}");
    }
}

#[test]
fn an_admission_without_them_sends_neither_and_a_bad_one_never_leaves_the_shell() {
    let (s, session) = server();
    call(
        &s,
        "admin_users_admit",
        json!({ "actor_id": "c3".repeat(32), "tier": "free" }),
    );
    let calls = session.calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].1["guardian_actor"].is_null(), "{}", calls[0].1);
    assert!(calls[0].1["age_band"].is_null(), "{}", calls[0].1);

    for bad in [
        json!({ "guardian_actor": "5e" }),
        json!({ "guardian_actor": "5e".repeat(32), "age_band": "toddler" }),
    ] {
        let mut args = json!({ "actor_id": "c3".repeat(32), "tier": "free" });
        args.as_object_mut()
            .unwrap()
            .extend(bad.as_object().unwrap().clone());
        let r = call(&s, "admin_users_admit", args);
        assert_eq!(r["isError"], true, "{r}");
    }
    assert_eq!(session.calls().len(), 1, "a refused argument sent nothing");
}

#[test]
fn the_registration_posture_is_read_without_the_principal_session() {
    let (s, session) = server();
    let r = call(&s, "admin_registration_mode_get", json!({}));
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(
        r["structuredContent"],
        json!({ "registration_mode": "invite_required", "max_free_users": 25 })
    );
    assert!(session.calls().is_empty(), "no principal call was made");
}

#[test]
fn the_band_enum_lists_every_wire_token() {
    let (s, _) = server();
    let listed = block_on(
        s.handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }).to_string()),
    )
    .expect("answered");
    let tool = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "admin_users_admit")
        .expect("admit is listed");
    let tokens: Vec<&str> = AgeBand::ORDER.iter().map(|b| b.as_str()).collect();
    assert_eq!(
        tool["inputSchema"]["properties"]["age_band"]["enum"],
        json!(tokens)
    );
}
