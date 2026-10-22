use super::*;
use crate::snapshots::{MailAppPassword, OracleOpRow};
use fauna_core::format::format_unix_local_ms;

fn lookup(key: &str) -> Option<&'static str> {
    fauna_i18n::strings::lookup(key)
}

fn row() -> ConnectedAppRow {
    ConnectedAppRow {
        key: "k".into(),
        class: class::REMOTE.into(),
        name: LocalizedText::key_arg(VERBATIM_KEY, "text", "Example App"),
        client_id: None,
        publisher: None,
        scope_descriptions: vec![],
        sets: vec![],
        created_at_millis: 1_700_000_000_000,
        last_used_at_millis: None,
        lasts_until_millis: None,
        connected: true,
        paused: false,
        mail: None,
        signer_burned: false,
        oracle: None,
    }
}

fn text(row: &ConnectedAppRow) -> Vec<String> {
    roster_row_text(row).resolve_lines(lookup)
}

/// Web hands in its own (JS) formatter: every instant on the row goes through
/// the injected function, and the native default is the same call.
#[test]
fn an_injected_formatter_renders_every_instant() {
    let mut r = row();
    r.last_used_at_millis = Some(2_000);
    r.lasts_until_millis = Some(3_000);
    let stamp = |ms: i64| format!("T{ms}");
    let rt = roster_row_text_with(&r, &stamp);
    let times: Vec<&str> = rt
        .facts
        .iter()
        .filter_map(|f| f.text.args.get("time").map(String::as_str))
        .collect();
    assert_eq!(times, ["T1700000000000", "T2000", "T3000"]);
    assert_eq!(
        roster_row_text(&r),
        roster_row_text_with(&r, &format_unix_local_ms)
    );
}

/// Every key the composition can name is in the string table: a typo here
/// would paint the raw key on seven apps.
#[test]
fn every_composed_key_resolves() {
    let mut full = row();
    full.class = class::SIGNER.into();
    full.client_id = Some("https://example.com/oauth".into());
    full.publisher = Some("example.com".into());
    full.scope_descriptions = vec![LocalizedText::key_arg(VERBATIM_KEY, "text", "Read posts")];
    full.paused = true;
    full.last_used_at_millis = Some(1_700_000_100_000);
    full.lasts_until_millis = Some(1_800_000_000_000);
    full.signer_burned = true;
    let mut keys: Vec<String> = Vec::new();
    let rt = roster_row_text(&full);
    for line in rt.lines.iter().chain(rt.facts.iter()) {
        keys.push(line.text.key.clone());
        keys.extend(line.nested.values().map(|n| n.key.clone()));
    }
    for class_value in [
        class::REMOTE,
        class::DEVICE,
        class::WASM,
        class::CONTAINER,
        class::APP_PASSWORD,
        class::SIGNER,
        class::OAUTH,
    ] {
        keys.extend(class_label_key(class_value).map(String::from));
    }
    for key in keys {
        assert!(lookup(&key).is_some(), "no string for {key}");
    }
}

#[test]
fn a_publisher_row_reads_head_publisher_scopes_then_facts() {
    let mut r = row();
    r.client_id = Some("https://example.com/oauth".into());
    r.publisher = Some("example.com".into());
    r.scope_descriptions = vec![LocalizedText::key_arg(VERBATIM_KEY, "text", "Read posts")];
    let lines = text(&r);
    assert_eq!(lines[0], "Example App · Website or service");
    assert_eq!(lines[1], "From example.com — https://example.com/oauth");
    assert_eq!(lines[2], "  • Read posts");
    let facts = lines.last().unwrap();
    assert!(facts.starts_with("Added "), "{facts}");
    assert!(
        facts.ends_with(" · Never used · Until you disconnect it"),
        "{facts}"
    );
}

/// The settings link rides the composed row, so a shell that calls only
/// `roster_row_text` paints it for a hosted plugin's row and no other.
#[test]
fn only_a_hosted_plugins_row_opens_its_settings_card() {
    let mut r = row();
    assert!(!roster_row_text(&r).opens_settings_card);
    for hosted in [class::WASM, class::CONTAINER] {
        r.class = hosted.into();
        assert!(roster_row_text(&r).opens_settings_card, "{hosted}");
    }
}

#[test]
fn an_unknown_class_paints_no_badge() {
    let mut r = row();
    r.class = "something-new".into();
    assert_eq!(text(&r)[0], "Example App");
}

#[test]
fn a_burned_mail_row_says_so_directly_under_the_name() {
    let mut r = row();
    r.class = class::APP_PASSWORD.into();
    r.mail = Some(MailAppPassword {
        mua_username: "{handle}".into(),
        kind: LocalizedText::key("connected_apps.verbatim"),
        revoked: true,
    });
    let lines = text(&r);
    assert_eq!(
        lines[1],
        lookup("settings.mail.credential_revoked").unwrap()
    );
}

#[test]
fn a_burned_signer_row_says_so_in_its_own_words() {
    let mut r = row();
    r.class = class::SIGNER.into();
    r.signer_burned = true;
    assert_eq!(
        text(&r)[1],
        lookup("connected_apps.signer_revoked").unwrap()
    );
}

#[test]
fn a_paused_row_says_paused_not_merely_not_connected() {
    let mut r = row();
    r.connected = false;
    r.paused = true;
    let facts = text(&r).pop().unwrap();
    assert!(facts.starts_with(lookup("connected_apps.paused").unwrap()));
    assert!(!facts.contains(lookup("connected_apps.not_connected").unwrap()));
    r.paused = false;
    let facts = text(&r).pop().unwrap();
    assert!(facts.starts_with(lookup("connected_apps.not_connected").unwrap()));
}

#[test]
fn dated_facts_carry_the_shared_formatted_time() {
    let mut r = row();
    r.last_used_at_millis = Some(1_700_000_100_000);
    r.lasts_until_millis = Some(1_800_000_000_000);
    let facts = text(&r).pop().unwrap();
    for ms in [1_700_000_000_000, 1_700_000_100_000, 1_800_000_000_000] {
        assert!(facts.contains(&format_unix_local_ms(ms)), "{facts}");
    }
    assert!(!facts.contains("Never used") && !facts.contains("Until you disconnect"));
}

#[test]
fn the_oracle_summary_counts_then_names_the_newest_operation() {
    let mut r = row();
    r.oracle = Some(OracleSummaryRow {
        ops_in_window: 1,
        requests_served: 5,
        last_op: Some(OracleOpRow {
            at_millis: 1_700_000_000_000,
            class: "sign".into(),
            words: LocalizedText::key_arg(VERBATIM_KEY, "text", "Sign an event"),
            detail: LocalizedText::key_arg(VERBATIM_KEY, "text", "kind 1"),
        }),
    });
    let lines = text(&r);
    assert_eq!(
        lines[1],
        "Your nest used your keys for this app once in the current window · 5 requests answered in all"
    );
    assert!(
        lines[2].ends_with(": Sign an event — kind 1"),
        "{}",
        lines[2]
    );
    // A blank detail drops the dash rather than trailing it.
    r.oracle.as_mut().unwrap().last_op.as_mut().unwrap().detail =
        LocalizedText::key_arg(VERBATIM_KEY, "text", "  ");
    assert!(text(&r)[2].ends_with(": Sign an event"));
}

fn card() -> ConsentCardRow {
    ConsentCardRow {
        consent_id_hex: "abcd".into(),
        code: "ABC-DEF".into(),
        client_id: "https://example.com/oauth".into(),
        client_name: Some("Example App".into()),
        scope_descriptions: vec!["Read your posts".into()],
        sets: vec![],
        ends: None,
        install: None,
    }
}

fn card_text(c: &ConsentCardRow) -> Vec<String> {
    request_card_lines(c)
        .iter()
        .map(|l| l.resolve(lookup))
        .collect()
}

#[test]
fn a_request_card_names_who_asks_and_what_for() {
    let lines = card_text(&card());
    assert_eq!(
        lines[0],
        lookup("atproto_settings.consent_heading").unwrap()
    );
    assert_eq!(lines[1], "Example App — https://example.com/oauth");
    assert_eq!(lines[3], "  • Read your posts");
    assert_eq!(lines.len(), 4);
}

#[test]
fn a_request_naming_a_permission_set_shows_nsid_title_details_members() {
    let mut c = card();
    c.sets = vec![ConsentSetRow {
        nsid: "com.example.calendar.appPerms".into(),
        title: Some("Calendar sync".into()),
        details: Some("Keeps your calendar in step.".into()),
        member_descriptions: vec!["Read and write calendar events".into()],
    }];
    let lines = card_text(&c);
    assert!(
        lines[4].contains("Calendar sync") && lines[4].contains("com.example.calendar.appPerms")
    );
    assert_eq!(lines[5], "  Keeps your calendar in step.");
    assert_eq!(lines[6], "  • Read and write calendar events");
    c.sets[0].title = None;
    assert!(card_text(&c)[4].contains("com.example.calendar.appPerms"));
}

#[test]
fn a_key_replacing_request_says_what_the_approve_ends_last() {
    let mut c = card();
    c.ends = Some(LocalizedText::key("connected_apps.consent_ends_holder"));
    let lines = card_text(&c);
    assert_eq!(
        lines.last().map(String::as_str),
        lookup("connected_apps.consent_ends_holder")
    );
    assert_eq!(card_text(&card()).len(), lines.len() - 1);
}

fn install_card(development: bool) -> ConsentCardRow {
    let mut c = card();
    c.install = Some(fauna_atproto_settings_machine::ConsentInstallRow {
        execution_form: "wasm".into(),
        requested_kinds: vec!["ext.plugins.example.notes".into()],
        settings: vec![fauna_atproto_settings_machine::ConsentInstallSettingRow {
            key: "greeting".into(),
            label: "Greeting".into(),
            setting_type: "string".into(),
        }],
        publisher_domain: if development {
            "localhost"
        } else {
            "plugins.example"
        }
        .into(),
        publisher_key: (!development).then(|| "did:key:z6MkExample".into()),
        module_digest: "sha256:abcd".into(),
        hosts: vec![],
        ingress_paths: vec!["/plugins/hello/".into()],
        development,
        bind: false,
    });
    c
}

/// A user's bind card carries the same section, rebuilt from the installed
/// rows, but its heading says the plugin is already on the nest and the
/// approve connects it — never "install".
#[test]
fn a_bind_card_heads_its_section_as_connecting_the_plugin() {
    let mut c = install_card(false);
    c.install.as_mut().unwrap().bind = true;
    let s = request_install_text(&c).expect("the bind card's section");
    assert_eq!(s.framing, None, "the plugin is already installed");
    assert_eq!(
        s.heading.resolve(lookup),
        "Connect this plugin on your nest to your account"
    );
    assert_eq!(s.form.resolve(lookup), "Runs as: wasm");
}

fn resolve_all(lines: &[ComposedText]) -> Vec<String> {
    lines.iter().map(|l| l.resolve(lookup)).collect()
}

/// Only an install card has a section; each leaf reads its own fact, a
/// declared-nothing list says "none" rather than painting an empty heading,
/// and the ingress paths ride under the hosts as the plugin's network reach.
#[test]
fn an_install_card_composes_one_text_per_leaf() {
    assert_eq!(request_install_text(&card()), None);
    let s = request_install_text(&install_card(false)).expect("an install section");
    assert_eq!(
        s.heading.resolve(lookup),
        "Install this plugin on your nest"
    );
    assert_eq!(
        s.framing.as_ref().map(|f| f.resolve(lookup)).as_deref(),
        Some(
            "Installing runs this code on your nest. It gets no access to anyone's private data until that person connects it from their own app."
        )
    );
    assert_eq!(s.form.resolve(lookup), "Runs as: wasm");
    assert_eq!(
        s.publisher.resolve(lookup),
        "Publisher: plugins.example — did:key:z6MkExample"
    );
    assert_eq!(s.digest.resolve(lookup), "Module: sha256:abcd");
    assert_eq!(
        resolve_all(&s.kinds),
        ["Kinds of data it defines:", "  • ext.plugins.example.notes"]
    );
    assert_eq!(
        resolve_all(&s.hosts),
        [
            "Sites it may reach: none",
            "Addresses it answers on your nest:",
            "  • /plugins/hello/"
        ]
    );
    assert_eq!(
        resolve_all(&s.settings),
        ["Settings:", "  • Greeting (string)"]
    );
}

/// A development plugin has no `did:key`: the card names it, never an empty
/// key row.
#[test]
fn a_development_install_card_names_the_plugin_not_an_empty_key() {
    let s = request_install_text(&install_card(true)).unwrap();
    assert_eq!(s.publisher.resolve(lookup), "Publisher: Development plugin");
}
