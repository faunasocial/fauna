//! tier_1: the catalog over the MCP method layer against a mocked principal
//! session (`apps/cli.md` § Test surface) — what a host sees for each tool
//! before a connection, after one, and when the nest refuses.

use std::sync::Arc;

use fauna_cli::tools::ConnectStarted;
use fauna_cli::{Host, Server, SessionUnavailable, ToolError};
use fauna_client_testkit::{RejectingRequester, block_on};
use fauna_protocol::RpcError;
use fauna_protocol::post_author::{KIND_CREATE, PostAuthorCreateReply};
use serde_json::{Value, json};

/// The account the fake session acts for, as its hex actor id.
fn own() -> String {
    "a1".repeat(32)
}

struct FakeHost {
    session: Result<Arc<RejectingRequester>, SessionUnavailable>,
}

impl Host for FakeHost {
    type Rpc = Arc<RejectingRequester>;

    async fn session(&self, _account: Option<&str>) -> Result<Self::Rpc, SessionUnavailable> {
        self.session.clone()
    }

    async fn account(&self, _account: Option<&str>) -> Result<String, SessionUnavailable> {
        Ok(own())
    }

    async fn redial(
        &self,
        _account: Option<&str>,
        _lost: &Self::Rpc,
    ) -> Result<Self::Rpc, SessionUnavailable> {
        self.session.clone()
    }

    async fn connect(&self, handle: &str, admin: bool) -> Result<ConnectStarted, ToolError> {
        Ok(ConnectStarted {
            account: handle.into(),
            user_code: if admin { "ADMN-1234" } else { "WXYZ-1234" }.into(),
            nest: format!("https://{handle}"),
            expires_in: 600,
            custody: None,
        })
    }

    async fn setup_status(
        &self,
        _account: Option<&str>,
    ) -> Result<fauna_protocol::discovery::SetupStatusReply, ToolError> {
        Ok(fauna_protocol::discovery::SetupStatusReply {
            auto_enable_mail_for_new_users: true,
            ..Default::default()
        })
    }
}

fn server(session: Result<RejectingRequester, SessionUnavailable>) -> Server<FakeHost> {
    Server::new(FakeHost {
        session: session.map(Arc::new),
    })
}

fn ask(server: &Server<FakeHost>, message: Value) -> Value {
    block_on(server.handle(&message.to_string())).expect("a request is answered")
}

fn call(server: &Server<FakeHost>, name: &str, arguments: Value) -> Value {
    ask(
        server,
        json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                "params": { "name": name, "arguments": arguments } }),
    )["result"]
        .clone()
}

#[test]
fn initialize_answers_the_asked_revision_or_the_newest() {
    let s = server(Err(SessionUnavailable::NotConnected));
    let known = ask(
        &s,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": "2025-03-26", "capabilities": {},
                            "clientInfo": { "name": "t", "version": "0" } } }),
    );
    assert_eq!(known["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(known["result"]["serverInfo"]["name"], "fauna-cli");
    assert!(known["result"]["capabilities"]["tools"].is_object());
    let unknown = ask(
        &s,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize",
                "params": { "protocolVersion": "1999-01-01" } }),
    );
    assert_eq!(
        unknown["result"]["protocolVersion"],
        fauna_cli::mcp::PROTOCOL_VERSIONS[0]
    );
}

#[test]
fn a_notification_takes_no_answer_and_a_bad_line_a_parse_error() {
    let s = server(Err(SessionUnavailable::NotConnected));
    let n = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    assert!(block_on(s.handle(&n.to_string())).is_none());
    let bad = block_on(s.handle("{not json")).unwrap();
    assert_eq!(bad["error"]["code"], -32700);
    let batch = block_on(s.handle("[]")).unwrap();
    assert_eq!(batch["error"]["code"], -32600);
    let unknown = ask(
        &s,
        json!({ "jsonrpc": "2.0", "id": 3, "method": "completion/complete" }),
    );
    assert_eq!(unknown["error"]["code"], -32601);
}

#[test]
fn tools_list_carries_every_tool_with_truthful_annotations() {
    let s = server(Err(SessionUnavailable::NotConnected));
    let listed = ask(
        &s,
        json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/list" }),
    );
    let tools = listed["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names.len(), fauna_cli::CATALOG.len());
    let by_name = |n: &str| tools.iter().find(|t| t["name"] == n).unwrap().clone();
    assert_eq!(by_name("feed_list")["annotations"]["readOnlyHint"], true);
    assert_eq!(by_name("post_create")["annotations"]["readOnlyHint"], false);
    assert_eq!(
        by_name("post_delete")["annotations"]["destructiveHint"],
        true
    );
    assert_eq!(
        by_name("connect")["inputSchema"]["required"],
        json!(["handle"])
    );
}

#[test]
fn before_a_connection_a_tool_tells_the_host_to_connect() {
    let s = server(Err(SessionUnavailable::NotConnected));
    let r = call(&s, "feed_local", json!({}));
    assert_eq!(r["isError"], true);
    assert!(
        r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("call connect")
    );

    let pending = server(Err(SessionUnavailable::Pending {
        user_code: "AB12-CD34".into(),
    }));
    let r = call(&pending, "feed_local", json!({}));
    assert!(
        r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("AB12-CD34")
    );
}

#[test]
fn connect_answers_the_code_for_the_human_and_nothing_secret() {
    let s = server(Err(SessionUnavailable::NotConnected));
    let r = call(&s, "connect", json!({ "handle": "alice.example" }));
    assert_eq!(r["isError"], false);
    assert_eq!(r["structuredContent"]["user_code"], "WXYZ-1234");
    assert!(
        r["structuredContent"]["for_the_user"]
            .as_str()
            .unwrap()
            .contains("Connect an app")
    );
    // The admin arms are asked for only on the user's word (`admin: true`).
    let r = call(
        &s,
        "connect",
        json!({ "handle": "alice.example", "admin": true }),
    );
    assert_eq!(r["structuredContent"]["user_code"], "ADMN-1234", "{r}");
    let r = call(
        &s,
        "connect",
        json!({ "handle": "alice.example", "admin": "yes" }),
    );
    assert_eq!(r["isError"], true, "{r}");
}

#[test]
fn a_refused_kind_is_the_nests_error_verbatim_never_an_empty_success() {
    let s = server(Ok(RejectingRequester::new().reject(
        "fauna.feed.list",
        RpcError::new("fauna.auth.permission_denied", "errors.permission_denied"),
    )));
    let r = call(&s, "feed_list", json!({}));
    assert_eq!(r["isError"], true);
    assert!(
        r["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("fauna.auth.permission_denied")
    );
    assert!(r.get("structuredContent").is_none());
}

/// The admin plane's first tool: both dashboard reads folded into one
/// answer, and the nest's refusal (a member's principal, rule (e)) verbatim.
#[test]
fn the_admin_dashboard_is_the_two_reads_or_the_nests_refusal() {
    use fauna_protocol::admin::{AdminStatsReply, AdminStatusReply};
    let stats = AdminStatsReply {
        total_users: 3,
        users_by_tier: Default::default(),
        suspended_users: 0,
        total_inbox_bytes: 0,
        total_storage_bytes: 0,
        ws_connections: 1,
        extra: Default::default(),
    };
    let status = AdminStatusReply {
        version: "0.1.9".into(),
        update_available: None,
        extra: Default::default(),
    };
    let s = server(Ok(RejectingRequester::new()
        .reply("fauna.admin.stats", &stats)
        .reply("fauna.admin.status", &status)));
    let r = call(&s, "admin_dashboard", json!({}));
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["stats"]["total_users"], 3);
    assert_eq!(r["structuredContent"]["status"]["version"], "0.1.9");

    let refusing = server(Ok(RejectingRequester::new().reject(
        "fauna.admin.stats",
        RpcError::new(
            "fauna.admin.permission_denied",
            "the account holds no admin authority",
        ),
    )));
    let r = call(&refusing, "admin_dashboard", json!({}));
    assert_eq!(r["isError"], true, "{r}");
    assert!(
        r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("fauna.admin.permission_denied"),
        "{r}"
    );
}

/// The users page's tools (`admin.md` § 2 Users): a member's id comes back
/// as the hex the tools take, an update with no label keeps the member's
/// current one (the wire sets both), and the registration save states both
/// halves — the cap's absence is never a default.
#[test]
fn the_users_tools_speak_hex_ids_and_never_blank_what_they_were_not_given() {
    use fauna_protocol::admin::{AdminOkReply, AdminUser, AdminUserGetReply, AdminUsersListReply};
    let member = AdminUser {
        actor_id: fauna_protocol::ByteBuf::from(vec![0xc3; 32]),
        tier: "free".into(),
        label: "kept".into(),
        handle: Some("bob".into()),
        ..Default::default()
    };
    let rpc = Arc::new(
        RejectingRequester::new()
            .reply(
                "fauna.admin.users.list",
                &AdminUsersListReply {
                    users: vec![member.clone()],
                    total: 1,
                    extra: Default::default(),
                },
            )
            .reply(
                "fauna.admin.users.get",
                &AdminUserGetReply {
                    user: member,
                    extra: Default::default(),
                },
            )
            .reply("fauna.admin.users.update", &AdminOkReply::default()),
    );
    let s = Server::new(FakeHost {
        session: Ok(rpc.clone()),
    });
    let r = call(&s, "admin_users_list", json!({}));
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(
        r["structuredContent"]["users"][0]["actor_id"],
        "c3".repeat(32)
    );

    let r = call(
        &s,
        "admin_users_update",
        json!({ "actor_id": "c3".repeat(32), "tier": "plus" }),
    );
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(
        rpc.kinds(),
        [
            "fauna.admin.users.list",
            "fauna.admin.users.get",
            "fauna.admin.users.update"
        ]
    );

    for bad in [
        json!({ "actor_id": "c3" }),
        json!({ "actor_id": "zz".repeat(32) }),
    ] {
        let r = call(&s, "admin_users_get", bad);
        assert_eq!(r["isError"], true, "{r}");
    }
    let r = call(&s, "admin_registration_mode_set", json!({ "mode": "open" }));
    assert_eq!(r["isError"], true, "{r}");
    let r = call(
        &s,
        "admin_registration_mode_set",
        json!({ "mode": "anyone", "max_free_users": null }),
    );
    assert_eq!(r["isError"], true, "{r}");
}

/// The nest-side pages' tools (`admin.md` § N Nest, § 7 Web, the DNS page):
/// the apex actor comes back as hex, a clearing write states its `null`
/// (never a default from an omitted argument), and a value the wire would
/// refuse is refused before any request.
#[test]
fn the_nest_side_tools_speak_hex_and_never_clear_what_they_were_not_told_to() {
    use fauna_protocol::region::AdminRegionSetReply;
    use fauna_protocol::web::WebGetApexActorReply;
    let rpc = Arc::new(
        RejectingRequester::new()
            .reply(
                "fauna.web.get_apex_actor",
                &WebGetApexActorReply {
                    actor_id: Some(fauna_protocol::ByteBuf::from(vec![0xc3; 32])),
                    extra: Default::default(),
                },
            )
            .reply(
                "fauna.admin.region.set",
                &AdminRegionSetReply {
                    ok: true,
                    ..Default::default()
                },
            ),
    );
    let s = Server::new(FakeHost {
        session: Ok(rpc.clone()),
    });
    let r = call(&s, "admin_apex_actor_get", json!({}));
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["actor_id"], "c3".repeat(32));

    // Withdrawing the region is a stated `null`; an omitted one is refused.
    let r = call(&s, "admin_region_set", json!({}));
    assert_eq!(r["isError"], true, "{r}");
    let r = call(
        &s,
        "admin_region_set",
        json!({ "region": "no such region" }),
    );
    assert_eq!(r["isError"], true, "{r}");
    let r = call(&s, "admin_region_set", json!({ "region": null }));
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(
        rpc.kinds(),
        ["fauna.web.get_apex_actor", "fauna.admin.region.set"]
    );

    for (tool, args) in [
        ("admin_apex_actor_set", json!({})),
        ("admin_apex_actor_set", json!({ "actor_id": "c3" })),
        (
            "admin_report_resolve",
            json!({ "report_id": "r1", "outcome": "deleted" }),
        ),
        ("admin_update_channel_set", json!({ "channel": "nightly" })),
        ("admin_web_app_origin_set", json!({ "mode": "elsewhere" })),
        ("admin_serving_port_set", json!({ "port": 0 })),
        ("admin_cert_status", json!({ "domains": [] })),
    ] {
        let r = call(&s, tool, args.clone());
        assert_eq!(r["isError"], true, "{tool} {args}: {r}");
    }
    assert_eq!(rpc.kinds().len(), 2, "no refused value reached the nest");
}

/// The tools over the kinds row 18 served without one: the domain add refuses
/// while no domain is active (the first one becomes the permanent main
/// domain — the admin's own act), a clearing write states its `null`, a value
/// the wire would refuse never reaches the nest, and the rules document reads
/// back as data.
#[test]
fn the_nest_page_documents_and_the_guarded_domain_add() {
    use fauna_protocol::bridge_routing::{
        AddLocalDomainReply, DEFAULT_CERT_MODE, ListLocalDomainsReply, MailDomainRow,
    };
    use fauna_protocol::content_policy::AdminContentPolicyGetReply;
    use fauna_protocol::node_policy::SetMaxStorageBytesReply;

    // No active domain: the add is refused after the one list read.
    let empty = Arc::new(RejectingRequester::new().reply(
        "fauna.bridges.list_local_domains",
        &ListLocalDomainsReply::default(),
    ));
    let s = Server::new(FakeHost {
        session: Ok(empty.clone()),
    });
    let r = call(
        &s,
        "admin_domain_add",
        json!({ "domain": "second.example" }),
    );
    assert_eq!(r["isError"], true, "{r}");
    assert!(
        r.to_string().contains("own Fauna app"),
        "the refusal says who adds the first domain: {r}"
    );
    assert_eq!(empty.kinds(), ["fauna.bridges.list_local_domains"]);

    // With an active domain the add is sent, with the apps' certificate mode.
    let row = MailDomainRow {
        domain_name: "main.example".into(),
        is_primary: true,
        ..Default::default()
    };
    let rpc = Arc::new(
        RejectingRequester::new()
            .reply(
                "fauna.bridges.list_local_domains",
                &ListLocalDomainsReply {
                    active: vec![row.clone()],
                    soft_deleted_within_30d: vec![],
                },
            )
            .reply(
                "fauna.bridges.add_local_domain",
                &AddLocalDomainReply {
                    domain: MailDomainRow {
                        domain_name: "second.example".into(),
                        mta_sts_cert_mode: DEFAULT_CERT_MODE.into(),
                        ..row
                    },
                    skipped: false,
                },
            )
            .reply(
                "fauna.admin.set_max_storage_bytes",
                &SetMaxStorageBytesReply {
                    ok: true,
                    extra: Default::default(),
                },
            )
            .reply(
                "fauna.admin.content_policy.get",
                &AdminContentPolicyGetReply::default(),
            ),
    );
    let s = Server::new(FakeHost {
        session: Ok(rpc.clone()),
    });
    let r = call(
        &s,
        "admin_domain_add",
        json!({ "domain": "second.example" }),
    );
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(
        r["structuredContent"]["domain"]["mta_sts_cert_mode"],
        DEFAULT_CERT_MODE
    );

    // The storage cap: omitted is refused, null clears, a number sets.
    let r = call(&s, "admin_storage_cap_set", json!({}));
    assert_eq!(r["isError"], true, "{r}");
    let r = call(&s, "admin_storage_cap_set", json!({ "max_bytes": null }));
    assert_eq!(r["isError"], false, "{r}");

    // A server that never wrote rules answers no document, not an error.
    let r = call(&s, "admin_content_rules_get", json!({}));
    assert_eq!(r["isError"], false, "{r}");
    assert!(r["structuredContent"]["document"].is_null(), "{r}");
    assert_eq!(
        rpc.kinds(),
        [
            "fauna.bridges.list_local_domains",
            "fauna.bridges.add_local_domain",
            "fauna.admin.set_max_storage_bytes",
            "fauna.admin.content_policy.get",
        ]
    );

    // Values the wire or the tool would refuse never reach the nest.
    let before = rpc.kinds().len();
    for (tool, args) in [
        ("admin_catch_all_set", json!({ "domain": "a.example" })),
        (
            "admin_catch_all_set",
            json!({ "domain": "a.example", "actor_id": "c3" }),
        ),
        (
            "admin_role_address_set",
            json!({ "domain": "a.example", "role": "root", "actor_id": null }),
        ),
        ("admin_dkim_interval_set", json!({ "domain": "a.example" })),
        (
            "admin_domain_config_set",
            json!({ "domain": "a.example", "dmarc_policy_mode": "strict" }),
        ),
        ("admin_transport_set", json!({ "max_conns_per_ip": 0 })),
        ("admin_transport_set", json!({})),
        ("admin_cors_set", json!({ "origins": [1] })),
        ("admin_feature_limit_set", json!({ "feature": "payments" })),
        (
            "admin_feature_limit_set",
            json!({ "feature": "no-such-feature", "policy": null }),
        ),
        ("admin_content_rules_set", json!({})),
    ] {
        let r = call(&s, tool, args.clone());
        assert_eq!(r["isError"], true, "{tool} {args}: {r}");
    }
    assert_eq!(
        rpc.kinds().len(),
        before,
        "no refused value reached the nest"
    );
}

/// The mail page's policy save (`admin.md` § 6 Mail): the wire's put is a
/// full PUT, so the tool reads the effective policy, lays the given fields
/// over it and sends every field — a save of one field never resets the
/// others to the catalog default — and a field the policy does not have is
/// refused before anything is sent.
#[test]
fn a_mail_policy_save_sends_the_whole_section_with_the_change_laid_over() {
    use fauna_protocol::bridge_routing::{FetchConfigReply, PutPolicyReply};
    let mut config = FetchConfigReply::default();
    config.spam.greylist_delay_secs = 77;
    let rpc = Arc::new(
        RejectingRequester::new()
            .reply("fauna.bridges.get_mail_config", &config)
            .reply("fauna.bridges.put_spam_policy", &PutPolicyReply::default()),
    );
    let s = Server::new(FakeHost {
        session: Ok(rpc.clone()),
    });
    let r = call(
        &s,
        "admin_mail_policy_set",
        json!({ "section": "spam", "values": { "greylist_enabled": true } }),
    );
    assert_eq!(r["isError"], false, "{r}");
    let calls = rpc.payloads();
    let (kind, sent) = calls.last().expect("a put");
    assert_eq!(*kind, "fauna.bridges.put_spam_policy");
    assert_eq!(sent["greylist_enabled"], true, "{sent}");
    assert_eq!(
        sent["greylist_delay_secs"], 77,
        "the untouched field is kept: {sent}"
    );

    let before = rpc.payloads().len();
    for bad in [
        json!({ "section": "spam", "values": { "no_such_knob": 1 } }),
        json!({ "section": "spam", "values": { "extra": {} } }),
        json!({ "section": "dkim", "values": {} }),
        json!({ "section": "spam", "values": 3 }),
    ] {
        let r = call(&s, "admin_mail_policy_set", bad);
        assert_eq!(r["isError"], true, "{r}");
    }
    assert!(
        rpc.payloads()[before..]
            .iter()
            .all(|(k, _)| !k.starts_with("fauna.bridges.put_")),
        "a refused save sends no put"
    );
}

/// The mail page's settings read (`admin.md` § 6 Mail): the effective config
/// with the alias and DMARC policies beside it, and the new-user default the
/// public setup status carries — read on the shell's anonymous connection,
/// since no admin string's kind returns it.
#[test]
fn the_mail_settings_fold_the_policies_and_the_new_user_default() {
    use fauna_protocol::bridge_routing::{AliasPolicy, DmarcReportingPolicy, FetchConfigReply};
    let config = FetchConfigReply::default();
    let alias = AliasPolicy {
        exact_aliases_max: 5,
        ..AliasPolicy::default()
    };
    let rpc = Arc::new(
        RejectingRequester::new()
            .reply("fauna.bridges.get_mail_config", &config)
            .reply("fauna.bridges.get_alias_policy", &alias)
            .reply(
                "fauna.bridges.get_dmarc_policy",
                &DmarcReportingPolicy::default(),
            ),
    );
    let s = Server::new(FakeHost {
        session: Ok(rpc.clone()),
    });
    let r = call(&s, "admin_mail_settings", json!({}));
    assert_eq!(r["isError"], false, "{r}");
    let v = &r["structuredContent"];
    assert_eq!(v["alias_policy"]["exact_aliases_max"], 5, "{v}");
    assert_eq!(v["mail_enabled"], config.mail_enabled, "{v}");
    assert_eq!(v["auto_enable_mail_for_new_users"], true, "{v}");
    assert_eq!(
        rpc.kinds(),
        [
            "fauna.bridges.get_mail_config",
            "fauna.bridges.get_alias_policy",
            "fauna.bridges.get_dmarc_policy"
        ]
    );
}

/// The mail-reports page's raw view (`dmarc-reporting.md` § The raw view):
/// each get tool sends the storage key a list entry names and answers the
/// entry beside its archive gunzipped to text — the forensic mail decoded
/// lossily, an oversize archive cut at the cap and said so — and a corrupt
/// archive or a missing key is refused.
#[test]
fn the_report_get_tools_answer_the_archive_as_text() {
    use fauna_mail::dmarc_report::gzip_xml;
    use fauna_protocol::bridge_routing::{
        DmarcAggregateReportEntry, DmarcForensicReportEntry, GetDmarcAggregateReportReply,
        GetDmarcForensicReportReply, GetTlsrptInboundReportReply, TlsrptInboundReportEntry,
    };
    let xml =
        "<feedback><report_metadata><org_name>example.net</org_name></report_metadata></feedback>";
    let mail = b"Subject: failed\r\n\r\nbody \xff end".to_vec();
    let json_report = r#"{"organization-name":"example.net","policies":[]}"#;
    let rpc = Arc::new(
        RejectingRequester::new()
            .reply(
                "fauna.bridges.get_dmarc_aggregate_report",
                &GetDmarcAggregateReportReply {
                    report: DmarcAggregateReportEntry {
                        our_domain: "example.org".into(),
                        submitter: "example.net".into(),
                        report_id: "r-1".into(),
                        ..Default::default()
                    },
                    payload_xml_gz: gzip_xml(xml.as_bytes()),
                    extra: Default::default(),
                },
            )
            .reply(
                "fauna.bridges.get_dmarc_forensic_report",
                &GetDmarcForensicReportReply {
                    report: DmarcForensicReportEntry {
                        id: 42,
                        ..Default::default()
                    },
                    payload_raw_gz: gzip_xml(&mail),
                    extra: Default::default(),
                },
            )
            .reply(
                "fauna.bridges.get_tlsrpt_inbound_report",
                &GetTlsrptInboundReportReply {
                    report: TlsrptInboundReportEntry {
                        report_id: "t-1".into(),
                        ..Default::default()
                    },
                    payload_json_gz: gzip_xml(json_report.as_bytes()),
                    extra: Default::default(),
                },
            ),
    );
    let s = Server::new(FakeHost {
        session: Ok(rpc.clone()),
    });

    let key =
        json!({ "our_domain": "example.org", "submitter": "example.net", "report_id": "r-1" });
    let r = call(&s, "admin_mail_reports_dmarc_get", key.clone());
    assert_eq!(r["isError"], false, "{r}");
    let v = &r["structuredContent"];
    assert_eq!(v["report"]["report_id"], "r-1", "{v}");
    assert_eq!(v["raw"], xml, "{v}");
    assert_eq!(v["raw_truncated"], false, "{v}");
    assert!(v.get("payload_xml_gz").is_none(), "no byte array: {v}");
    let (_, sent) = rpc.payloads().last().cloned().expect("a get");
    assert_eq!(sent["our_domain"], "example.org");
    assert_eq!(sent["submitter"], "example.net");
    assert_eq!(sent["report_id"], "r-1");

    let r = call(&s, "admin_mail_reports_forensic_get", json!({ "id": 42 }));
    assert_eq!(r["isError"], false, "{r}");
    let v = &r["structuredContent"];
    assert_eq!(v["report"]["id"], 42, "{v}");
    assert_eq!(v["raw"], "Subject: failed\r\n\r\nbody \u{fffd} end", "{v}");
    assert_eq!(rpc.payloads().last().unwrap().1["id"], 42);

    let r = call(
        &s,
        "admin_mail_reports_tls_get",
        json!({ "our_domain": "example.org", "submitter": "example.net", "report_id": "t-1" }),
    );
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["raw"], json_report);

    let before = rpc.kinds().len();
    for (tool, args) in [
        (
            "admin_mail_reports_dmarc_get",
            json!({ "our_domain": "example.org" }),
        ),
        ("admin_mail_reports_forensic_get", json!({})),
        ("admin_mail_reports_tls_get", json!({ "report_id": "t-1" })),
    ] {
        let r = call(&s, tool, args.clone());
        assert_eq!(r["isError"], true, "{tool} {args}: {r}");
    }
    assert_eq!(rpc.kinds().len(), before, "a missing key sends nothing");
}

/// An archive past the tool's cap is cut there and said so, with its whole
/// length named; one that does not gunzip is the decoder's refusal.
#[test]
fn a_report_archive_is_capped_and_a_corrupt_one_refused() {
    use fauna_mail::dmarc_report::gzip_xml;
    use fauna_protocol::bridge_routing::{
        GetDmarcAggregateReportReply, GetDmarcForensicReportReply,
    };
    let big = "é".repeat(fauna_cli::tools::REPORT_RAW_TEXT_CAP);
    let rpc = Arc::new(
        RejectingRequester::new()
            .reply(
                "fauna.bridges.get_dmarc_aggregate_report",
                &GetDmarcAggregateReportReply {
                    report: Default::default(),
                    payload_xml_gz: gzip_xml(big.as_bytes()),
                    extra: Default::default(),
                },
            )
            .reply(
                "fauna.bridges.get_dmarc_forensic_report",
                &GetDmarcForensicReportReply {
                    report: Default::default(),
                    payload_raw_gz: b"not gzip".to_vec(),
                    extra: Default::default(),
                },
            ),
    );
    let s = Server::new(FakeHost {
        session: Ok(rpc.clone()),
    });
    let r = call(
        &s,
        "admin_mail_reports_dmarc_get",
        json!({ "our_domain": "a", "submitter": "b", "report_id": "c" }),
    );
    assert_eq!(r["isError"], false, "{r}");
    let v = &r["structuredContent"];
    assert_eq!(v["raw_truncated"], true);
    assert_eq!(v["raw_bytes"], big.len());
    let raw = v["raw"].as_str().unwrap();
    assert!(raw.len() <= fauna_cli::tools::REPORT_RAW_TEXT_CAP);
    assert!(raw.len() > fauna_cli::tools::REPORT_RAW_TEXT_CAP - 4);

    let r = call(&s, "admin_mail_reports_forensic_get", json!({ "id": 1 }));
    assert_eq!(r["isError"], true, "{r}");
}

/// The pending-bridge roster (`admin.md` § Approved-bridges roster): its
/// keys come back as hex — what the human finds the card by — and the
/// approve is a coaching prompt, never a tool.
#[test]
fn the_bridge_roster_is_read_and_its_approve_is_a_prompt() {
    use fauna_protocol::wrapped_blob::{ListPendingBridgesReply, ServiceUserInfo};
    let s = server(Ok(RejectingRequester::new().reply(
        "fauna.bridges.list_pending_bridges",
        &ListPendingBridgesReply {
            bridges: vec![ServiceUserInfo {
                role: "mta".into(),
                status: "pending".into(),
                ed25519_pubkey: vec![0xab; 32],
                ..Default::default()
            }],
            extra: Default::default(),
        },
    )));
    let r = call(&s, "admin_bridges_pending_list", json!({}));
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(
        r["structuredContent"]["bridges"][0]["ed25519_pubkey"],
        "ab".repeat(32)
    );
    let listed = ask(
        &s,
        json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/list" }),
    );
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(
        names.iter().all(|n| !n.contains("approve_pending")),
        "{names:?}"
    );

    let prompts = ask(
        &s,
        json!({ "jsonrpc": "2.0", "id": 5, "method": "prompts/list" }),
    );
    assert_eq!(
        prompts["result"]["prompts"][0]["name"],
        "approve_pending_bridge"
    );
    let got = ask(
        &s,
        json!({ "jsonrpc": "2.0", "id": 6, "method": "prompts/get",
                "params": { "name": "approve_pending_bridge", "arguments": { "pubkey": "ab".repeat(32) } } }),
    );
    let text = got["result"]["messages"][0]["content"]["text"]
        .as_str()
        .expect("a text message");
    assert!(text.contains(&"ab".repeat(32)), "{text}");
    let refused = ask(
        &s,
        json!({ "jsonrpc": "2.0", "id": 8, "method": "prompts/get",
                "params": { "name": "approve_pending_bridge", "arguments": { "pubkey": "not hex!" } } }),
    );
    assert_eq!(refused["error"]["code"], -32602);
}

#[test]
fn a_served_read_comes_back_as_data() {
    #[derive(serde::Serialize)]
    struct Feeds {
        feeds: Vec<()>,
    }
    let s = server(Ok(
        RejectingRequester::new().reply("fauna.feed.list", &Feeds { feeds: vec![] })
    ));
    let r = call(&s, "feed_list", json!({}));
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["feeds"], json!([]));
}

#[test]
fn post_create_goes_through_the_oracle_door() {
    let requester = RejectingRequester::new().reply(
        KIND_CREATE,
        &PostAuthorCreateReply {
            post_id: "ab".repeat(32),
            ..Default::default()
        },
    );
    let s = server(Ok(requester));
    let r = call(&s, "post_create", json!({ "text": "hello" }));
    assert_eq!(r["structuredContent"]["post_id"], "ab".repeat(32));
    let kinds = s.host().session.as_ref().unwrap().kinds();
    assert_eq!(kinds, vec![KIND_CREATE]);
}

/// `profile_set` sends only the fields given — absent stays absent, so the
/// custodian keeps them — and answers the stored profile's display fields.
#[test]
fn profile_set_goes_through_the_oracle_door() {
    use fauna_protocol::profile_author::{KIND_SET, ProfileAuthorSetReply};
    let requester = RejectingRequester::new().reply(
        KIND_SET,
        &ProfileAuthorSetReply {
            body: fauna_protocol::ByteBuf::from(signed_profile("Alice")),
            extra: Default::default(),
        },
    );
    let s = server(Ok(requester));
    let r = call(
        &s,
        "profile_set",
        json!({ "bio": "hi", "links": [{ "label": "site", "uri": "https://a.example" }] }),
    );
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["display_name"], "Alice");
    let session = s.host().session.as_ref().unwrap();
    assert_eq!(session.kinds(), vec![KIND_SET]);
    let r = call(&s, "profile_set", json!({ "links": "not a list" }));
    assert_eq!(r["isError"], true, "{r}");
}

#[test]
fn an_unknown_tool_or_a_missing_argument_is_refused() {
    let s = server(Ok(RejectingRequester::new()));
    let unknown = ask(
        &s,
        json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call",
                "params": { "name": "mail_send", "arguments": {} } }),
    );
    assert_eq!(unknown["error"]["code"], -32602);
    let r = call(&s, "feed_get", json!({}));
    assert_eq!(r["isError"], true);
    assert!(
        r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("feed_id")
    );
}

fn signed_profile(display_name: &str) -> Vec<u8> {
    use fauna_core::data::{InboxMode, Profile, Timestamp};
    use fauna_core::identity::ActorKeypair;
    let kp = ActorKeypair::generate();
    let profile = Profile {
        actor_id: kp.actor_id(),
        display_name: Some(display_name.into()),
        bio: Some("hi".into()),
        avatar: None,
        banner: None,
        links: vec![],
        nests: vec![],
        admin_nests: vec![],
        load_hint: None,
        inbox_mode: InboxMode::Open,
        recovery_head: None,
        audience: fauna_core::data::ProfileAudience::ThisNest,
        updated_at: Timestamp(0),
    };
    fauna_client_profile::build_profile(&kp, &profile).expect("signed profile")
}

#[test]
fn initialize_advertises_resources_and_the_lists_split_uris_from_templates() {
    let s = server(Err(SessionUnavailable::NotConnected));
    let init = ask(
        &s,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
    );
    assert!(init["result"]["capabilities"]["resources"].is_object());
    let list = ask(
        &s,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "resources/list" }),
    );
    assert_eq!(
        list["result"]["resources"],
        json!([{
            "uri": "nest://profile", "name": "profile", "title": "The account's profile",
            "description": fauna_cli::catalog::RESOURCES[0].description,
            "mimeType": "application/json",
        }])
    );
    let templates = ask(
        &s,
        json!({ "jsonrpc": "2.0", "id": 3, "method": "resources/templates/list" }),
    );
    let t = &templates["result"]["resourceTemplates"];
    assert_eq!(t.as_array().unwrap().len(), 1);
    assert_eq!(t[0]["uriTemplate"], "nest://profile/{actor_id}");
}

#[test]
fn a_profile_resource_is_the_shared_decode_as_json() {
    use fauna_protocol::profile::ProfileGetReply;
    let s = server(Ok(RejectingRequester::new().reply(
        "fauna.profile.get",
        &ProfileGetReply {
            body: fauna_protocol::ByteBuf::from(signed_profile("Alice")),
            ..Default::default()
        },
    )));
    for uri in ["nest://profile", "nest://profile/b2b2"] {
        let r = ask(
            &s,
            json!({ "jsonrpc": "2.0", "id": 4, "method": "resources/read",
                    "params": { "uri": uri } }),
        );
        let content = &r["result"]["contents"][0];
        assert_eq!(content["uri"], uri, "{r}");
        assert_eq!(content["mimeType"], "application/json");
        let body: Value = serde_json::from_str(content["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["display_name"], "Alice");
        assert_eq!(body["bio"], "hi");
    }
    let kinds = s.host().session.as_ref().unwrap().kinds();
    assert_eq!(kinds, vec!["fauna.profile.get", "fauna.profile.get"]);

    let missing = ask(
        &s,
        json!({ "jsonrpc": "2.0", "id": 5, "method": "resources/read",
                "params": { "uri": "fauna://profile" } }),
    );
    assert_eq!(missing["error"]["code"], -32002);
}

#[test]
fn a_refused_resource_read_is_the_nests_error_verbatim() {
    let s = server(Ok(RejectingRequester::new().reject(
        "fauna.profile.get",
        RpcError::new(
            "fauna.profile.permission_denied",
            "errors.permission_denied",
        ),
    )));
    let r = ask(
        &s,
        json!({ "jsonrpc": "2.0", "id": 6, "method": "resources/read",
                "params": { "uri": "nest://profile" } }),
    );
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("fauna.profile.permission_denied"),
        "{r}"
    );
}

#[test]
fn notifications_poll_lists_and_counts_and_never_marks() {
    use fauna_protocol::notifications::{NotifCountReply, NotifListReply};
    let s = server(Ok(RejectingRequester::new()
        .reply(
            "fauna.notifications.list",
            &NotifListReply {
                notifications: vec![],
                cursor: None,
                extra: Default::default(),
            },
        )
        .reply(
            "fauna.notifications.count",
            &NotifCountReply {
                count: 3,
                extra: Default::default(),
            },
        )));
    let r = call(&s, "notifications_poll", json!({}));
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["unread"], 3);
    assert_eq!(r["structuredContent"]["notifications"], json!([]));
    let kinds = s.host().session.as_ref().unwrap().kinds();
    assert_eq!(
        kinds,
        vec!["fauna.notifications.list", "fauna.notifications.count"]
    );
}

#[test]
fn notifications_mark_read_is_a_write_the_host_confirms() {
    use fauna_protocol::notifications::NotifMarkReadReply;
    let s = server(Ok(RejectingRequester::new().reply(
        "fauna.notifications.mark_read",
        &NotifMarkReadReply {
            marked_read: 2,
            extra: Default::default(),
        },
    )));
    let listed = ask(
        &s,
        json!({ "jsonrpc": "2.0", "id": 8, "method": "tools/list" }),
    );
    let tool = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "notifications_mark_read")
        .unwrap()
        .clone();
    assert_eq!(tool["annotations"]["readOnlyHint"], false);
    assert_eq!(tool["annotations"]["destructiveHint"], false);
    let r = call(&s, "notifications_mark_read", json!({}));
    assert_eq!(r["structuredContent"]["marked_read"], 2, "{r}");
}

/// A shell with no device door — the nest-hosted one — answers every
/// conversations tool with the one sentence naming where conversations are
/// reached, never asking the nest for them; a malformed argument is refused
/// before any door is asked.
#[test]
fn a_shell_without_a_door_reaches_no_conversations() {
    let s = server(Ok(RejectingRequester::new()));
    for (name, arguments) in [
        ("conversations_list", json!({})),
        ("conversations_read", json!({ "room_id": "r" })),
        ("conversations_search", json!({ "query": "trip" })),
        (
            "conversations_send",
            json!({ "room_id": "r", "text": "hi" }),
        ),
    ] {
        let r = call(&s, name, arguments);
        assert_eq!(r["isError"], true, "{name}");
        assert_eq!(
            r["content"][0]["text"],
            fauna_cli::tools::NO_CONVERSATIONS_DOOR,
            "{name}"
        );
    }
    let r = call(
        &s,
        "conversations_read",
        json!({ "room_id": "r", "before": -1 }),
    );
    assert!(
        r["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("before must be"),
        "{r}"
    );
}
