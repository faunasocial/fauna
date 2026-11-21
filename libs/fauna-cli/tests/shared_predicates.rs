//! tier_1: a tool asks the shared draft predicate the seven apps paint from
//! before it sends anything, and never answers success for a call that would
//! change nothing (`apps/cli.md` § A tool that cannot run, rules 4 and 5).
//! A blocked draft refuses with the predicate's own sentence and no request
//! for the write reaches the nest.

use std::sync::Arc;

use fauna_cli::tools::ConnectStarted;
use fauna_cli::{Host, Server, SessionUnavailable, ToolError};
use fauna_client_testkit::{CapturingRequester, RejectingRequester, block_on};
use fauna_protocol::RpcRequester;
use fauna_protocol::bridge_routing::{ListLocalDomainsReply, MailDomainRow};
use fauna_protocol::discovery::SetupStatusReply;
use fauna_protocol::{ByteBuf, RpcErrorClass};
use serde_json::{Value, json};

struct TestHost<R> {
    session: Arc<R>,
}

impl<R> Host for TestHost<R>
where
    Arc<R>: RpcRequester<Error: RpcErrorClass>,
    R: Send + Sync,
{
    type Rpc = Arc<R>;

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
        Ok(SetupStatusReply::default())
    }
}

/// The tool's answer: `Err(text)` for an MCP tool error, `Ok(result)` else.
fn call<R>(server: &Server<TestHost<R>>, name: &str, arguments: Value) -> Result<Value, String>
where
    TestHost<R>: Host,
{
    let message = json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                          "params": { "name": name, "arguments": arguments } });
    let result = block_on(server.handle(&message.to_string())).expect("answered")["result"].clone();
    if result["isError"] == json!(true) {
        Err(result["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned())
    } else {
        Ok(result)
    }
}

fn capturing() -> (
    Server<TestHost<CapturingRequester>>,
    Arc<CapturingRequester>,
) {
    let session = Arc::new(CapturingRequester::transport_fault());
    (
        Server::new(TestHost {
            session: session.clone(),
        }),
        session,
    )
}

fn domain(name: &str, primary: bool) -> MailDomainRow {
    MailDomainRow {
        domain_id: ByteBuf::from(vec![0u8; 16]),
        domain_name: name.into(),
        is_primary: primary,
        ..Default::default()
    }
}

fn with_domains() -> (
    Server<TestHost<RejectingRequester>>,
    Arc<RejectingRequester>,
) {
    let session = Arc::new(RejectingRequester::new().reply(
        "fauna.bridges.list_local_domains",
        &ListLocalDomainsReply {
            active: vec![domain("example.org", true), domain("example.net", false)],
            soft_deleted_within_30d: Vec::new(),
        },
    ));
    (
        Server::new(TestHost {
            session: session.clone(),
        }),
        session,
    )
}

#[test]
fn a_blocked_draft_refuses_with_the_predicates_sentence_and_sends_nothing() {
    let (s, session) = capturing();
    for (tool, args, sentence) in [
        (
            "admin_users_admit",
            json!({ "actor_id": "zz", "tier": "free" }),
            "The account key must be exactly 64 hex characters.",
        ),
        (
            "admin_tier_create",
            json!({ "name": " ", "max_inbox_bytes": 1, "max_storage_bytes": 1,
                    "max_devices": 1, "max_blob_size": 1, "max_feeds": 1 }),
            "Can't add this tier — give it a name.",
        ),
        (
            "admin_serving_port_set",
            json!({ "port": 0 }),
            "Enter a port number between 1 and 65535.",
        ),
        (
            "admin_calendar_port_set",
            json!({ "port": 70000 }),
            "Enter a port number between 1 and 65535.",
        ),
        (
            "admin_forwarders_create",
            json!({ "local_domain": "example.org", "pattern": "", "forward_target": "a@b.c" }),
            "Choose a domain and fill in the local part and the destination first.",
        ),
    ] {
        assert_eq!(call(&s, tool, args), Err(sentence.to_owned()), "{tool}");
    }
    assert!(session.calls().is_empty(), "{:?}", session.kinds());
}

#[test]
fn a_possible_draft_is_sent() {
    let (s, session) = capturing();
    let _ = call(&s, "admin_serving_port_set", json!({ "port": 8443 }));
    let _ = call(&s, "admin_calendar_port_set", json!({ "port": "5232" }));
    assert_eq!(
        session.kinds(),
        [
            "fauna.admin.set_serving_port",
            "fauna.bridges.set_caldav_port"
        ]
    );
}

#[test]
fn adding_an_active_domain_refuses_rather_than_answering_success() {
    let (s, session) = with_domains();
    assert_eq!(
        call(&s, "admin_domain_add", json!({ "domain": "Example.NET" })),
        Err("That domain is already added.".to_owned())
    );
    assert_eq!(session.kinds(), ["fauna.bridges.list_local_domains"]);
}

#[test]
fn removing_the_primary_domain_refuses_with_the_rename_step() {
    let (s, session) = with_domains();
    assert_eq!(
        call(
            &s,
            "admin_domain_remove",
            json!({ "domain": "example.org" })
        ),
        Err("Rename the primary domain onto another domain to remove it.".to_owned())
    );
    assert_eq!(session.kinds(), ["fauna.bridges.list_local_domains"]);
}

#[test]
fn a_call_that_changes_nothing_refuses() {
    let (s, session) = capturing();
    let empty_policy = call(
        &s,
        "admin_mail_policy_set",
        json!({ "section": "spam", "values": {} }),
    );
    assert!(empty_policy.is_err_and(|e| e.contains("nothing would be saved")));
    let empty_profile = call(&s, "profile_set", json!({}));
    assert!(empty_profile.is_err_and(|e| e.contains("nothing would be saved")));
    assert!(session.calls().is_empty(), "{:?}", session.kinds());
}

/// `web-content-hosting.md` § Same-origin security model → *While the central
/// origin is not live, the choice is not offered*, rule 5: the set tool reads
/// the status first and refuses a central mode the nest says it cannot serve,
/// with the nest's own refusal sentence; nothing is written. Once the nest says
/// `central_live`, the same call is sent.
#[test]
fn a_central_web_app_origin_is_refused_while_the_nest_says_it_is_not_live() {
    use fauna_protocol::web_app_origin::{AdminWebAppOriginGetReply, WebAppOrigin};
    for (central_live, sent) in [(false, false), (true, true)] {
        let reply = AdminWebAppOriginGetReply::project(WebAppOrigin::Bundled, None, central_live);
        let session = Arc::new(
            RejectingRequester::new()
                .reply("fauna.admin.web_app_origin.get", &reply)
                .reply("fauna.admin.web_app_origin.set", &reply),
        );
        let s = Server::new(TestHost {
            session: session.clone(),
        });
        let answer = call(&s, "admin_web_app_origin_set", json!({ "mode": "central" }));
        if sent {
            assert!(answer.is_ok(), "{answer:?}");
            assert_eq!(
                session.kinds(),
                [
                    "fauna.admin.web_app_origin.get",
                    "fauna.admin.web_app_origin.set"
                ]
            );
        } else {
            assert_eq!(
                answer,
                Err(fauna_i18n::strings::error::admin::web_app_origin::CENTRAL_NOT_LIVE.to_owned())
            );
            assert_eq!(session.kinds(), ["fauna.admin.web_app_origin.get"]);
        }
        // Bundled is always sent, with no read first.
        let session =
            Arc::new(RejectingRequester::new().reply("fauna.admin.web_app_origin.set", &reply));
        let s = Server::new(TestHost {
            session: session.clone(),
        });
        assert!(call(&s, "admin_web_app_origin_set", json!({ "mode": "bundled" })).is_ok());
        assert_eq!(session.kinds(), ["fauna.admin.web_app_origin.set"]);
    }
}
