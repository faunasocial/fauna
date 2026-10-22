//! tier_1: the `argv` shell (`apps/cli.md` § One catalog, many front-ends →
//! *The `argv` shell*) — the command tree is DERIVED from the catalog, so a
//! walk over the catalog proves every tool is a command and every property a
//! flag; the flags round-trip into the tool's input JSON; and the exit codes,
//! the output choice and the destructive-tool confirmation are pinned against
//! a mocked principal session.

use std::sync::Arc;

use fauna_cli::argv::{self, Confirm, EXIT_OK, EXIT_TOOL_ERROR, EXIT_USAGE, Parsed, Terminal};
use fauna_cli::tools::ConnectStarted;
use fauna_cli::{CATALOG, Host, SessionUnavailable, Tool, ToolError};
use fauna_client_testkit::{RejectingRequester, block_on};
use fauna_protocol::post_author::{KIND_CREATE, KIND_DELETE, PostAuthorCreateReply};
use serde_json::{Value, json};

struct FakeHost {
    session: Result<Arc<RejectingRequester>, SessionUnavailable>,
}

impl Host for FakeHost {
    type Rpc = Arc<RejectingRequester>;

    async fn session(&self, _account: Option<&str>) -> Result<Self::Rpc, SessionUnavailable> {
        self.session.clone()
    }

    async fn account(&self, _account: Option<&str>) -> Result<String, SessionUnavailable> {
        Ok("a1".repeat(32))
    }

    async fn redial(
        &self,
        _account: Option<&str>,
        _lost: &Self::Rpc,
    ) -> Result<Self::Rpc, SessionUnavailable> {
        self.session.clone()
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

    async fn setup_status(
        &self,
        _account: Option<&str>,
    ) -> Result<fauna_protocol::discovery::SetupStatusReply, ToolError> {
        Err(ToolError::new("no setup status in this fake"))
    }
}

fn host(session: Result<RejectingRequester, SessionUnavailable>) -> FakeHost {
    FakeHost {
        session: session.map(Arc::new),
    }
}

fn words(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_owned).collect()
}

fn parse(line: &str) -> Result<Parsed, argv::UsageError> {
    argv::parse(&words(line))
}

fn called(line: &str) -> argv::Invocation {
    match parse(line) {
        Ok(Parsed::Call(inv)) => inv,
        other => panic!("`{line}` should be a call, got {other:?}"),
    }
}

/// A value the schema accepts for `property`, as the command line spells it.
fn sample(property: &Value) -> Option<String> {
    let types: Vec<&str> = match &property["type"] {
        Value::String(t) => vec![t.as_str()],
        Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    };
    if let Some(first) = property["enum"].as_array().and_then(|e| e.first()) {
        return Some(first.as_str().unwrap().to_owned());
    }
    match types.first().copied() {
        Some("boolean") => None,
        Some("integer") => Some(property["minimum"].as_i64().unwrap_or(1).max(1).to_string()),
        Some("string") => Some("x".into()),
        // An array or object property is given as its JSON.
        Some("array") => Some("[]".into()),
        Some("object") => Some("{}".into()),
        other => panic!("a catalog property of type {other:?} has no command-line form"),
    }
}

fn path(tool: &Tool) -> String {
    argv::command_words(tool).join(" ")
}

#[test]
fn every_tool_is_a_command_and_every_property_a_flag() {
    let mut paths = std::collections::BTreeSet::new();
    for tool in CATALOG {
        let command = path(tool);
        assert!(
            paths.insert(command.clone()),
            "two tools derive `{command}`"
        );
        assert_ne!(
            argv::command_words(tool)[0],
            "mcp",
            "`mcp` is the MCP shell's own word"
        );
        match parse(&format!("{command} --help")) {
            Ok(Parsed::Help(t)) => assert_eq!(t.name, tool.name),
            other => panic!("`{command} --help` should be its help, got {other:?}"),
        }
        let help = argv::help(tool);
        assert!(
            help.contains(tool.description),
            "{command}: --help is the description"
        );
        let schema = (tool.input_schema)();
        let required: Vec<&str> = schema["required"]
            .as_array()
            .map(|r| r.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let properties = schema["properties"].as_object().unwrap();
        // Every required property, by flag: the smallest call that parses.
        let mut base = command.clone();
        for name in &required {
            let flag = argv::flag_name(name);
            match sample(&properties[*name]) {
                Some(v) => base.push_str(&format!(" {flag} {v}")),
                None => base.push_str(&format!(" {flag}")),
            }
        }
        // ... and each optional one on top of it, alone.
        for (name, property) in properties {
            assert!(
                !argv::GLOBAL_FLAGS.contains(&name.as_str()),
                "{}: property `{name}` shadows a global flag",
                tool.name
            );
            let flag = argv::flag_name(name);
            assert!(help.contains(&flag), "{command}: --help lists {flag}");
            if let Some(description) = property["description"].as_str() {
                assert!(
                    help.contains(description),
                    "{command}: {flag}'s help is the schema's"
                );
            }
            let line = if required.contains(&name.as_str()) {
                base.clone()
            } else {
                match sample(property) {
                    Some(v) => format!("{base} {flag} {v}"),
                    None => format!("{base} {flag}"),
                }
            };
            let inv = called(&line);
            assert_eq!(inv.tool.name, tool.name, "`{line}`");
            assert!(
                inv.arguments.get(name.as_str()).is_some(),
                "`{line}` carries {name}: {}",
                inv.arguments
            );
        }
    }
    assert_eq!(paths.len(), CATALOG.len());
}

#[test]
fn flags_round_trip_into_the_tools_input_json() {
    let inv = called("post create --text hello --content-warning cw");
    assert_eq!(inv.tool.name, "post_create");
    assert_eq!(
        inv.arguments,
        json!({ "text": "hello", "content_warning": "cw" })
    );
    assert!(!inv.json && !inv.yes);

    // `--flag=value`, an integer, and the globals anywhere after the command.
    let inv = called("feed posts --json --feed-id=f1 --limit 5");
    assert_eq!(inv.tool.name, "feed_posts");
    assert_eq!(inv.arguments, json!({ "feed_id": "f1", "limit": 5 }));
    assert!(inv.json);

    // A multi-word verb, and a nullable integer given as null.
    let inv = called("admin registration mode set --mode open --max-free-users null");
    assert_eq!(inv.tool.name, "admin_registration_mode_set");
    assert_eq!(
        inv.arguments,
        json!({ "mode": "open", "max_free_users": null })
    );

    // `connect <handle>`: the required properties bind positionally, in the
    // schema's order; a boolean flag takes no value.
    let inv = called("connect alice.example.com --admin");
    assert_eq!(inv.tool.name, "connect");
    assert_eq!(
        inv.arguments,
        json!({ "handle": "alice.example.com", "admin": true })
    );
    let inv = called("post delete abc --yes");
    assert_eq!(inv.arguments, json!({ "post_id": "abc" }));
    assert!(inv.yes);

    // An array property is its JSON, verbatim.
    let inv = called(r#"profile set --links [{"label":"site","uri":"https://example.com"}]"#);
    assert_eq!(
        inv.arguments,
        json!({ "links": [{ "label": "site", "uri": "https://example.com" }] })
    );

    // A tool with no arguments is an empty object.
    assert_eq!(called("feed list").arguments, json!({}));
}

#[test]
fn a_bad_command_line_is_a_usage_error() {
    for line in [
        "feed",
        "feed lst",
        "mail send --to x",
        "feed list --nope",
        "post create",
        "post create --text",
        "post create --text a --text b",
        "post create a b",
        "feed posts --feed-id f --limit many",
        "feed posts --feed-id f --limit 0",
        "admin registration mode set --mode sometimes --max-free-users null",
        "connect alice --admin=maybe",
    ] {
        assert!(parse(line).is_err(), "`{line}` must be a usage error");
    }
    assert!(matches!(parse(""), Ok(Parsed::Overview { asked: false })));
    assert!(matches!(
        parse("--help"),
        Ok(Parsed::Overview { asked: true })
    ));
    let overview = argv::overview();
    for tool in CATALOG {
        assert!(
            overview.contains(&path(tool)),
            "the overview lists `{}`",
            path(tool)
        );
    }
}

#[test]
fn the_exit_codes_are_pinned() {
    assert_eq!((EXIT_OK, EXIT_TOOL_ERROR, EXIT_USAGE), (0, 1, 2));
}

const PIPE: Terminal = Terminal {
    stdout_tty: false,
    stdin_tty: false,
};
const TTY: Terminal = Terminal {
    stdout_tty: true,
    stdin_tty: true,
};

fn never_asked(_: &str) -> bool {
    panic!("this command must not ask")
}

#[test]
fn a_tool_error_is_the_managers_error_on_stderr_with_exit_1() {
    let h = host(Err(SessionUnavailable::NotConnected));
    let out = block_on(argv::execute(&h, &called("feed local"), PIPE, never_asked));
    assert_eq!(out.code, EXIT_TOOL_ERROR);
    assert!(out.stdout.is_empty(), "{out:?}");
    assert!(out.stderr.contains("call connect"), "{out:?}");
}

#[test]
fn off_a_tty_or_under_json_the_result_is_the_tools_json_verbatim() {
    #[derive(serde::Serialize)]
    struct Feeds {
        feeds: Vec<()>,
    }
    let served = || {
        host(Ok(
            RejectingRequester::new().reply("fauna.feed.list", &Feeds { feeds: vec![] })
        ))
    };
    let h = served();
    let expected = block_on(fauna_cli::tools::call(
        &h,
        fauna_cli::catalog::tool("feed_list").unwrap(),
        &json!({}),
    ))
    .unwrap();
    for (inv, term) in [
        (called("feed list"), PIPE),
        (called("feed list --json"), TTY),
    ] {
        let out = block_on(argv::execute(&served(), &inv, term, never_asked));
        assert_eq!(out.code, EXIT_OK, "{out:?}");
        let printed: Value = serde_json::from_str(out.stdout.trim()).expect("stdout is JSON");
        assert_eq!(printed, expected);
    }
    // On a tty without --json it is rendered for a person, not JSON.
    let out = block_on(argv::execute(
        &served(),
        &called("feed list"),
        TTY,
        never_asked,
    ));
    assert_eq!(out.code, EXIT_OK);
    assert!(
        serde_json::from_str::<Value>(out.stdout.trim()).is_err(),
        "{out:?}"
    );
    assert!(out.stdout.contains("feeds"), "{out:?}");
}

#[test]
fn a_destructive_tool_refuses_off_a_tty_without_yes_and_asks_on_one() {
    let deleting = || host(Ok(RejectingRequester::new()));

    // Off a tty, no --yes: a usage error, and nothing reaches the nest.
    let h = deleting();
    let out = block_on(argv::execute(
        &h,
        &called("post delete abc"),
        PIPE,
        never_asked,
    ));
    assert_eq!(out.code, EXIT_USAGE, "{out:?}");
    assert!(out.stderr.contains("--yes"), "{out:?}");
    assert!(h.session.as_ref().unwrap().kinds().is_empty());

    // On a tty: asked, and a "no" deletes nothing.
    let h = deleting();
    let mut asked = None;
    let out = block_on(argv::execute(
        &h,
        &called("post delete abc"),
        TTY,
        |q: &str| {
            asked = Some(q.to_owned());
            false
        },
    ));
    assert!(asked.is_some_and(|q| q.contains("post delete")));
    assert_ne!(out.code, EXIT_OK);
    assert!(h.session.as_ref().unwrap().kinds().is_empty());

    // On a tty, "yes": the delete goes through the oracle door.
    let h = deleting();
    block_on(argv::execute(
        &h,
        &called("post delete abc"),
        TTY,
        |_: &str| true,
    ));
    assert_eq!(h.session.as_ref().unwrap().kinds(), vec![KIND_DELETE]);

    // --yes off a tty: no question, straight through.
    let h = deleting();
    block_on(argv::execute(
        &h,
        &called("post delete abc --yes"),
        PIPE,
        never_asked,
    ));
    assert_eq!(h.session.as_ref().unwrap().kinds(), vec![KIND_DELETE]);
    assert_eq!(
        argv::confirmation(
            fauna_cli::catalog::tool("post_delete").unwrap(),
            false,
            false
        ),
        Confirm::Refuse
    );
}

#[test]
fn a_write_that_is_not_destructive_never_asks() {
    let h = host(Ok(RejectingRequester::new().reply(
        KIND_CREATE,
        &PostAuthorCreateReply {
            post_id: "ab".repeat(32),
            ..Default::default()
        },
    )));
    let out = block_on(argv::execute(
        &h,
        &called("post create --text hi"),
        TTY,
        never_asked,
    ));
    assert_eq!(out.code, EXIT_OK, "{out:?}");
    assert!(out.stdout.contains(&"ab".repeat(32)));
}
