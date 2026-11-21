//! tier_1: a call whose precondition is unmet answers a refusal that names
//! the nest's reason and the step that unblocks it — never the bare
//! permission sentence, never the generic retry sentence — and a resource
//! refusal is never JSON-RPC's internal-error code (`apps/cli.md` § A tool
//! that cannot run, rule 3). A fault keeps the generic sentence.

use std::sync::Arc;

use fauna_cli::tools::ConnectStarted;
use fauna_cli::{Host, Server, SessionUnavailable, ToolError};
use fauna_client_testkit::{RejectingRequester, block_on};
use fauna_protocol::discovery::SetupStatusReply;
use fauna_protocol::{RpcError, RpcErrorClass, RpcRequester};
use serde_json::{Value, json};

/// The generic sentence a fault keeps and a precondition never ends in.
const RETRY: &str = "Please try again.";
/// The bare permission sentence a refusal never answers alone.
const BARE_PERMISSION: &str = "You do not have permission to do this.";

struct TestHost<R> {
    session: Option<Arc<R>>,
}

impl<R> Host for TestHost<R>
where
    Arc<R>: RpcRequester<Error: RpcErrorClass>,
    R: Send + Sync,
{
    type Rpc = Arc<R>;

    async fn session(&self, _account: Option<&str>) -> Result<Self::Rpc, SessionUnavailable> {
        self.session.clone().ok_or(SessionUnavailable::NotConnected)
    }

    async fn account(&self, _account: Option<&str>) -> Result<String, SessionUnavailable> {
        Ok("a1".repeat(32))
    }

    async fn redial(
        &self,
        _account: Option<&str>,
        lost: &Self::Rpc,
    ) -> Result<Self::Rpc, SessionUnavailable> {
        Ok(lost.clone())
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

fn server(session: RejectingRequester) -> Server<TestHost<RejectingRequester>> {
    Server::new(TestHost {
        session: Some(Arc::new(session)),
    })
}

/// A nest refusal as its `rpc_errors` seams build it: the code, its
/// `error.*` key, the reason in `details`.
fn refusal(code: &str, reason: &str) -> RpcError {
    let key = format!("error.{}", code.strip_prefix("fauna.").unwrap_or(code));
    RpcError::new(code, key).with_details_text(reason)
}

/// The tool's error text; panics on a success.
fn refused(server: &Server<TestHost<RejectingRequester>>, name: &str, arguments: Value) -> String {
    let message = json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                          "params": { "name": name, "arguments": arguments } });
    let result = block_on(server.handle(&message.to_string())).expect("answered")["result"].clone();
    assert_eq!(result["isError"], json!(true), "{name} answered {result}");
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// The JSON-RPC error a `resources/read` answers.
fn read_error<R>(server: &Server<TestHost<R>>, uri: &str) -> Value
where
    TestHost<R>: Host,
{
    let message = json!({ "jsonrpc": "2.0", "id": 9, "method": "resources/read",
                          "params": { "uri": uri } });
    block_on(server.handle(&message.to_string())).expect("answered")["error"].clone()
}

fn assert_precondition(text: &str, reason: &str, step: &str) {
    assert!(text.contains(reason), "the nest's reason is lost: {text}");
    assert!(text.contains(step), "no step `{step}` in: {text}");
    assert!(
        !text.ends_with(RETRY),
        "a precondition ends in the retry sentence: {text}"
    );
    assert!(
        !text.contains(BARE_PERMISSION),
        "the bare permission sentence: {text}"
    );
}

#[test]
fn an_admin_tool_without_the_admin_arms_names_connect_with_admin() {
    let reason = "fauna.admin.stats: not covered by the principal's scopes";
    let s = server(RejectingRequester::new().reject(
        "fauna.admin.stats",
        refusal("fauna.admin.permission_denied", reason),
    ));
    let text = refused(&s, "admin_dashboard", json!({}));
    assert_precondition(&text, reason, "admin: true");
    assert!(
        text.starts_with("fauna.admin.permission_denied: "),
        "{text}"
    );
}

#[test]
fn an_account_without_admin_authority_is_told_plainly_to_stop() {
    let reason = "fauna.admin.stats: the account holds no admin authority";
    let s = server(RejectingRequester::new().reject(
        "fauna.admin.stats",
        refusal("fauna.admin.permission_denied", reason),
    ));
    let text = refused(&s, "admin_dashboard", json!({}));
    assert_precondition(&text, reason, "does not administer its Fauna server");
    assert!(
        !text.contains("admin: true"),
        "offers a connect that changes nothing: {text}"
    );
}

#[test]
fn an_unknown_feed_id_names_the_list_tool() {
    let reason = "feed not found";
    let s = server(
        RejectingRequester::new().reject("fauna.feed.get", refusal("fauna.feed.not_found", reason)),
    );
    let text = refused(&s, "feed_get", json!({ "feed_id": "nope" }));
    assert_precondition(&text, reason, "feed_list");
}

#[test]
fn a_domain_tool_with_no_active_domain_names_the_list_and_the_domain_add() {
    let reason = "no active mail_domains row for example.org";
    let s = server(RejectingRequester::new().reject(
        "fauna.bridges.update_local_domain_config",
        refusal("fauna.protocol.not_found", reason),
    ));
    let text = refused(
        &s,
        "admin_domain_config_set",
        json!({ "domain": "example.org", "spf_record": "v=spf1 -all" }),
    );
    assert_precondition(&text, reason, "admin_domains_list");
    assert!(text.contains("admin_domain_add"), "{text}");
}

#[test]
fn a_membership_designation_with_no_tier_names_admin_tier_create() {
    let reason = "subscription tier not found";
    let s = server(RejectingRequester::new().reject(
        "fauna.admin.membership_tiers.set",
        refusal("fauna.admin.not_found", reason),
    ));
    let text = refused(
        &s,
        "admin_membership_tier_set",
        json!({ "tier_name": "supporter", "admin_tier": "free" }),
    );
    assert_precondition(&text, reason, "admin_tier_create");
}

#[test]
fn any_other_refusal_keeps_the_nests_reason() {
    let reason = "tier_name is required";
    let s = server(RejectingRequester::new().reject(
        "fauna.admin.membership_tiers.set",
        refusal("fauna.admin.invalid_params", reason),
    ));
    let text = refused(
        &s,
        "admin_membership_tier_set",
        json!({ "tier_name": "x", "admin_tier": "free" }),
    );
    assert_precondition(&text, reason, "Change what the reason names");
}

#[test]
fn a_fault_keeps_the_generic_sentence() {
    let s = server(RejectingRequester::new().reject(
        "fauna.feed.get",
        refusal("fauna.protocol.internal", "database is locked"),
    ));
    let text = refused(&s, "feed_get", json!({ "feed_id": "f" }));
    assert!(
        !text.contains("database is locked"),
        "a fault's details leak: {text}"
    );
    assert!(
        !text.contains("feed_list"),
        "a fault is not a precondition: {text}"
    );
}

#[test]
fn a_resource_read_without_a_session_is_no_internal_error_and_names_connect() {
    let s: Server<TestHost<RejectingRequester>> = Server::new(TestHost { session: None });
    let error = read_error(&s, "nest://profile");
    assert_ne!(error["code"], json!(-32603), "{error}");
    let text = error["message"].as_str().unwrap_or_default();
    assert!(text.contains("connect"), "{text}");
}

#[test]
fn an_unknown_actor_is_resource_not_found_with_the_step() {
    let reason = "profile not found";
    let s = server(RejectingRequester::new().reject(
        "fauna.profile.get",
        refusal("fauna.profile.not_found", reason),
    ));
    let error = read_error(&s, &format!("nest://profile/{}", "b2".repeat(32)));
    assert_eq!(error["code"], json!(-32002), "{error}");
    let text = error["message"].as_str().unwrap_or_default();
    assert_precondition(text, reason, "sender_id");
}

#[test]
fn a_resource_fault_keeps_the_internal_error_code() {
    // An unmapped kind is the test requester's transport fault.
    let s = server(RejectingRequester::new());
    let error = read_error(&s, "nest://profile");
    assert_eq!(error["code"], json!(-32603), "{error}");
}
