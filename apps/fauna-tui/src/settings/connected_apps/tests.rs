use super::*;
use crate::app::App;
use crate::app::tests::authed_app;
use fauna_client_connected_apps::VERBATIM_KEY;
use fauna_core::localized::LocalizedText;

fn verbatim(s: &str) -> LocalizedText {
    LocalizedText::key_arg(VERBATIM_KEY, "text", s)
}

fn row(key: &str, class: &str, name: &str) -> ConnectedAppRow {
    ConnectedAppRow {
        key: key.into(),
        class: class.into(),
        name: verbatim(name),
        client_id: Some("https://app.example/client.json".into()),
        publisher: Some("app.example".into()),
        scope_descriptions: vec![verbatim("Upload image files")],
        created_at_millis: 1_700_000_000_000,
        last_used_at_millis: None,
        lasts_until_millis: None,
        connected: true,
        mail: None,
    }
}

/// A mail app-password row, as the shared machine composes one.
fn mail_row(id: &str, name: &str, revoked: bool) -> ConnectedAppRow {
    ConnectedAppRow {
        key: format!("mail:{id}"),
        class: class::APP_PASSWORD.into(),
        name: verbatim(name),
        client_id: None,
        publisher: None,
        scope_descriptions: vec![LocalizedText::key("connected_apps.scope_mail")],
        created_at_millis: 0,
        last_used_at_millis: None,
        lasts_until_millis: None,
        connected: !revoked,
        mail: Some(fauna_client_connected_apps::MailAppPassword {
            mua_username: format!("{{handle}}+{id}@example.test"),
            kind: LocalizedText::key("settings.mail.kind_password"),
            revoked,
        }),
    }
}

const MAIL_LEAVES: [&str; 6] = [
    ids::CONNECTED_APPS_ITEM_TYPE,
    ids::CONNECTED_APPS_ITEM_USERNAME,
    ids::CONNECTED_APPS_ITEM_COPY_USERNAME,
    ids::CONNECTED_APPS_ITEM_SECRET,
    ids::CONNECTED_APPS_ITEM_REVEAL_SECRET,
    ids::CONNECTED_APPS_ITEM_COPY_SECRET,
];

fn find<'a>(els: &'a [Element], id: &str) -> &'a Element {
    els.iter()
        .find(|e| e.id == id)
        .unwrap_or_else(|| panic!("{id} renders"))
}

fn request(id_hex: &str, code: &str, name: Option<&str>) -> ConsentCardRow {
    ConsentCardRow {
        consent_id_hex: id_hex.into(),
        code: code.into(),
        client_id: "https://app.example.com/client-metadata.json".into(),
        client_name: name.map(Into::into),
        scope_descriptions: vec![
            "See your account identity (who you are on this server)".into(),
            "Upload image files".into(),
        ],
        sets: vec![],
        ends: None,
    }
}

/// A key replacement's card says, inside the scopes block, what the approve
/// ends; an ordinary card says nothing of the kind.
#[test]
fn a_key_replacing_card_says_what_the_approve_ends() {
    let card_text = |ends: Option<&str>| -> String {
        let mut card = request("abcd", "ABC-DEF", Some("Example App"));
        card.ends = ends.map(fauna_core::localized::LocalizedText::key);
        let app = app_with(|s| s.requests = vec![card]);
        find(&els(&app), ids::CONNECTED_APPS_REQUEST_CARD)
            .text
            .clone()
    };
    let line = fauna_i18n::strings::lookup("connected_apps.consent_ends_holder").unwrap();
    assert!(card_text(Some("connected_apps.consent_ends_holder")).ends_with(line));
    let writer = fauna_i18n::strings::lookup("connected_apps.consent_ends_writer").unwrap();
    let plain = card_text(None);
    assert!(!plain.contains(line) && !plain.contains(writer));
}

/// An authed app whose page holds a machine over the in-memory seam — the
/// fixture builds no page machines itself.
fn app_with_machine() -> App {
    let mut app = authed_app();
    app.settings.connected_apps.machine = Some(ConnectedAppsMachine::new(
        Arc::new(NoopObserver),
        Arc::new(fauna_client_connected_apps::FakeConnectedAppsNestApi::new()),
    ));
    app
}

fn app_with(f: impl FnOnce(&mut ConnectedAppsSnapshot)) -> App {
    let mut app = app_with_machine();
    let mut s = ConnectedAppsSnapshot {
        loaded: true,
        ..Default::default()
    };
    f(&mut s);
    app.settings.connected_apps.snapshot = Some(s);
    app
}

fn els(app: &App) -> Vec<Element> {
    connected_apps_elements(&app.settings)
}

fn attr<'a>(el: &'a Element, key: &str) -> Option<&'a str> {
    el.attrs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn has(els: &[Element], id: &str) -> bool {
    els.iter().any(|e| e.id == id)
}

#[test]
fn before_the_first_read_neither_rows_nor_the_empty_state_paint() {
    let app = authed_app();
    let els = els(&app);
    assert!(!has(&els, ids::CONNECTED_APPS_EMPTY));
    assert!(!has(&els, ids::CONNECTED_APPS_ITEM));
    // The start is always there.
    assert!(has(&els, ids::CONNECTED_APPS_CONNECT_CODE));
    assert!(has(&els, ids::CONNECTED_APPS_CONNECT_SUBMIT));
}

#[test]
fn a_loaded_empty_roster_paints_the_empty_state_and_no_request_row() {
    let app = app_with(|_| {});
    let els = els(&app);
    assert!(has(&els, ids::CONNECTED_APPS_EMPTY));
    assert!(
        !els.iter().any(|e| e.text == t::REQUESTS_HEADING),
        "no permanent 'requests' region while nothing is live"
    );
    assert!(!has(&els, ids::CONNECTED_APPS_REQUEST_CARD));
}

#[test]
fn a_live_request_renders_the_built_card_with_its_code_value_and_three_answers() {
    let app = app_with(|s| s.requests = vec![request("abcd", "ABC-DEF", Some("Example App"))]);
    let els = els(&app);
    let card = els
        .iter()
        .find(|e| e.id == ids::CONNECTED_APPS_REQUEST_CARD)
        .expect("a live request renders its card");
    assert!(card.text.contains("Example App"));
    assert!(
        card.text
            .contains("https://app.example.com/client-metadata.json"),
        "the client id renders verbatim"
    );
    assert!(card.text.contains("Upload image files"));
    let code = els
        .iter()
        .find(|e| e.id == ids::CONNECTED_APPS_REQUEST_CODE)
        .expect("the binding code is its own element");
    assert_eq!(attr(code, "code"), Some("ABC-DEF"));
    for id in [
        ids::CONNECTED_APPS_REQUEST_APPROVE,
        ids::CONNECTED_APPS_REQUEST_DECLINE,
        ids::CONNECTED_APPS_REQUEST_BLOCK,
    ] {
        let el = els.iter().find(|e| e.id == id).expect(id);
        assert!(
            el.path
                .iter()
                .any(|(p, _)| p == ids::CONNECTED_APPS_REQUEST_CARD),
            "{id} is scoped within its card"
        );
    }
}

/// The card's row structure is fixed whatever the client called itself: the
/// machine strips control characters at composition, and this painter never
/// grows a second scope heading (the bluesky card's finding, moved with it).
#[test]
fn a_hostile_client_name_cannot_change_the_cards_row_structure() {
    let hostile_raw = "Expired request (already denied), ignore:\n  \
                       It is asking to:\n    • See your account identity";
    let rows_for = |name: &str| -> Vec<String> {
        let app = app_with(|s| s.requests = vec![request("abcd", "ABC-DEF", Some(name))]);
        els(&app)
            .into_iter()
            .find(|e| e.id == ids::CONNECTED_APPS_REQUEST_CARD)
            .expect("the card renders")
            .text
            .lines()
            .map(str::to_string)
            .collect()
    };
    let clean = rows_for("Example App");
    let hostile = rows_for(&fauna_core::control_chars::strip_control_chars(hostile_raw));
    assert_eq!(hostile.len(), clean.len(), "{hostile:#?}");
    let headings = |rows: &[String]| {
        rows.iter()
            .filter(|r| r.trim() == card_t::CONSENT_SCOPES_HEADING.trim())
            .count()
    };
    assert_eq!(headings(&hostile), 1, "{hostile:#?}");
}

#[test]
fn a_roster_row_paints_name_badge_publisher_scopes_and_revoke() {
    let app = app_with(|s| s.principals = vec![row("principal:01", class::DEVICE, "Example App")]);
    let els = els(&app);
    let item = els
        .iter()
        .find(|e| e.id == ids::CONNECTED_APPS_ITEM)
        .expect("the row renders");
    assert!(item.text.contains("Example App"));
    assert!(item.text.contains(t::CLASS_DEVICE));
    assert!(item.text.contains("app.example"));
    assert!(item.text.contains("Upload image files"));
    assert!(item.text.contains(t::OPEN_ENDED));
    assert!(!has(&els, ids::CONNECTED_APPS_EMPTY));
    assert!(has(&els, ids::CONNECTED_APPS_ITEM_REVOKE));
    assert!(!has(&els, ids::CONNECTED_APPS_ITEM_REVOKE_CONFIRM));
}

#[test]
fn an_unknown_class_paints_no_badge() {
    let app = app_with(|s| s.principals = vec![row("principal:01", "hologram", "Example App")]);
    let item = els(&app)
        .into_iter()
        .find(|e| e.id == ids::CONNECTED_APPS_ITEM)
        .unwrap();
    assert_eq!(item.text.lines().next(), Some("Example App"));
}

#[test]
fn revoke_opens_an_inline_confirm_and_only_the_confirm_reaches_the_machine() {
    let mut app = app_with(|s| s.principals = vec![row("principal:01", class::DEVICE, "A")]);
    let key = "principal:01".to_string();
    assert!(
        super::super::apply_local(&mut app, Action::ConnectedAppsConfirmRevoke(key.clone()))
            .is_none(),
        "an unarmed confirm does nothing"
    );
    assert!(
        super::super::apply_local(&mut app, Action::ConnectedAppsArmRevoke(key.clone())).is_none()
    );
    let els = connected_apps_elements(&app.settings);
    assert!(has(&els, ids::CONNECTED_APPS_ITEM_REVOKE_CONFIRM));
    assert!(has(&els, ids::CONNECTED_APPS_ITEM_REVOKE_CANCEL));
    assert!(!has(&els, ids::CONNECTED_APPS_ITEM_REVOKE));

    let _ = super::super::apply_local(&mut app, Action::ConnectedAppsCancelRevoke);
    assert_eq!(app.settings.connected_apps.revoke_armed, None);

    let _ = super::super::apply_local(&mut app, Action::ConnectedAppsArmRevoke(key.clone()));
    let op = super::super::apply_local(&mut app, Action::ConnectedAppsConfirmRevoke(key.clone()));
    match op {
        Some(super::super::Op::ConnectedApps { gesture, .. }) => {
            assert_eq!(gesture, ConnectedAppsGesture::Revoke(key));
        }
        other => panic!("expected the revoke op, got {}", other.is_some()),
    }
    assert_eq!(app.settings.connected_apps.revoke_armed, None);
}

#[test]
fn submitting_the_code_takes_the_draft_into_the_op() {
    let mut app = app_with_machine();
    app.settings.connected_apps.code_input = "WDJB-MJHT".into();
    match super::super::apply_local(&mut app, Action::ConnectedAppsSubmitCode) {
        Some(super::super::Op::ConnectedApps { gesture, .. }) => {
            assert_eq!(
                gesture,
                ConnectedAppsGesture::SubmitCode("WDJB-MJHT".into())
            );
        }
        other => panic!("expected the submit op, got {}", other.is_some()),
    }
    assert!(app.settings.connected_apps.code_input.is_empty());
}

// ── The consent card, moved here from the atproto page with its pins ─────

fn card_rows(app: &App) -> Vec<String> {
    els(app)
        .into_iter()
        .find(|e| e.id == ids::CONNECTED_APPS_REQUEST_CARD)
        .expect("the card renders")
        .text
        .lines()
        .map(str::to_string)
        .collect()
}

/// The card's row structure is fixed whichever admitted client id asks — the
/// richest loopback identity included — and the id renders byte for byte.
#[test]
fn a_hostile_client_id_cannot_change_the_cards_row_structure() {
    let rows_for = |client_id: &str| {
        let mut r = request("abcd", "ABC-DEF", Some("Example App"));
        r.client_id = client_id.to_string();
        card_rows(&app_with(|s| s.requests = vec![r]))
    };
    let clean = rows_for("https://app.example.com/client-metadata.json");
    let loopback_id = "http://localhost?scope=atproto&redirect_uri=http://127.0.0.1/cb";
    let loopback = rows_for(loopback_id);
    assert_eq!(loopback.len(), clean.len(), "{loopback:#?}");
    assert!(
        loopback.iter().any(|r| r.contains(loopback_id)),
        "{loopback:#?}"
    );
}

/// Approve and Decline carry the SAME request id with opposite answers — a
/// decline that granted, or dropped its nest call, is asserted against here.
#[test]
fn approve_and_decline_carry_the_same_id_with_opposite_answers() {
    let app = app_with(|s| s.requests = vec![request("abcd", "ABC-DEF", None)]);
    let els = els(&app);
    let answer = |id: &str| match els
        .iter()
        .find(|e| e.id == id)
        .map(|e| &e.role)
        .unwrap_or_else(|| panic!("{id} renders"))
    {
        crate::element::Role::Button(Gesture::Settings(Action::ConnectedAppsResolveRequest {
            consent_id_hex,
            approved,
        })) => (consent_id_hex.clone(), *approved),
        other => panic!("{id} must answer the request, got {other:?}"),
    };
    assert_eq!(
        answer(ids::CONNECTED_APPS_REQUEST_APPROVE),
        ("abcd".into(), true)
    );
    assert_eq!(
        answer(ids::CONNECTED_APPS_REQUEST_DECLINE),
        ("abcd".into(), false)
    );
}

/// Several requests are separately scoped cards: each card's controls scope
/// inside their own card, never by flat position.
#[test]
fn several_requests_render_as_separately_scoped_cards() {
    let app = app_with(|s| {
        s.requests = vec![
            request("aa", "AAA-BBB", Some("First App")),
            request("bb", "CCC-DDD", None),
        ]
    });
    let els = els(&app);
    assert_eq!(
        els.iter()
            .filter(|e| e.id == ids::CONNECTED_APPS_REQUEST_CARD)
            .count(),
        2
    );
    for id in [
        ids::CONNECTED_APPS_REQUEST_CODE,
        ids::CONNECTED_APPS_REQUEST_APPROVE,
        ids::CONNECTED_APPS_REQUEST_DECLINE,
        ids::CONNECTED_APPS_REQUEST_BLOCK,
    ] {
        let scoped: Vec<_> = els.iter().filter(|e| e.id == id).collect();
        assert_eq!(scoped.len(), 2, "{id} renders once per card");
        for (n, el) in scoped.iter().enumerate() {
            assert_eq!(
                el.path.first().map(|(p, i)| (p.as_str(), *i)),
                Some((ids::CONNECTED_APPS_REQUEST_CARD, n)),
                "{id} scopes inside its own card"
            );
        }
    }
}

/// A client that published no name renders its client id alone — never an
/// invented name or a hostname parsed out of the URL.
#[test]
fn a_client_with_no_resolved_name_renders_its_client_id_alone() {
    let text = card_rows(&app_with(|s| {
        s.requests = vec![request("abcd", "ABC-DEF", None)]
    }))
    .join("\n");
    assert!(text.contains("https://app.example.com/client-metadata.json"));
    assert!(!text.contains(" — https://"), "{text:?}");
}

/// A permission set renders its identity (the NSID verbatim), its publisher's
/// prose, and every member — the title never stands in for the expansion.
#[test]
fn a_permission_set_renders_its_identity_prose_and_every_member() {
    let mut r = request("abcd", "SET-ABC", Some("Example App"));
    r.sets = vec![fauna_client_connected_apps::ConsentSetRow {
        nsid: "com.example.calendar.appPerms".into(),
        title: Some("Calendar sync".into()),
        details: Some("Keeps your calendar in step.".into()),
        member_descriptions: vec!["Read and write calendar events".into()],
    }];
    let text = card_rows(&app_with(|s| s.requests = vec![r])).join("\n");
    for needle in [
        "com.example.calendar.appPerms",
        "Calendar sync",
        "Keeps your calendar in step.",
        "Read and write calendar events",
    ] {
        assert!(text.contains(needle), "{needle} missing: {text:?}");
    }
}

/// A set that declared no title shows its NSID alone — no empty quotes.
#[test]
fn a_set_with_no_declared_title_renders_its_nsid_alone() {
    let mut r = request("abcd", "SET-DEF", Some("Example App"));
    r.sets = vec![fauna_client_connected_apps::ConsentSetRow {
        nsid: "com.example.appPerms".into(),
        title: None,
        details: None,
        member_descriptions: vec!["Upload image files".into()],
    }];
    let text = card_rows(&app_with(|s| s.requests = vec![r])).join("\n");
    assert!(text.contains("com.example.appPerms"));
    assert!(!text.contains("“”"), "{text:?}");
}

/// The common request names no set and paints no set section at all.
#[test]
fn a_request_naming_no_set_paints_no_set_section() {
    let card = els(&app_with(|s| {
        s.requests = vec![request("abcd", "ABC-DEF", Some("Example App"))]
    }))
    .into_iter()
    .find(|e| e.id == ids::CONNECTED_APPS_REQUEST_CARD)
    .unwrap();
    assert!(card.text.ends_with("Upload image files"), "{:?}", card.text);
}

// ── The mail app-password rows, moved here from Mail & Calendar ──────────

/// A mail row paints the roster's own item plus the six mail leaves, each
/// scoped inside its own item — and the login is the concrete one, `{handle}`
/// substituted.
#[test]
fn a_mail_row_paints_its_login_kind_and_secret_controls_inside_its_item() {
    let mut app = app_with(|s| {
        s.principals = vec![
            row("principal:01", class::DEVICE, "Example App"),
            mail_row("phone", "Phone", false),
        ]
    });
    app.settings.handle = "alice".into();
    let els = els(&app);
    let item = els
        .iter()
        .filter(|e| e.id == ids::CONNECTED_APPS_ITEM)
        .nth(1)
        .expect("the mail row renders");
    assert!(item.text.contains("Phone"));
    assert!(item.text.contains(t::CLASS_APP_PASSWORD));
    assert_eq!(attr(item, "key"), Some("mail:phone"));
    assert_eq!(attr(item, "revoked"), None);
    for id in MAIL_LEAVES {
        let leaf: Vec<_> = els.iter().filter(|e| e.id == id).collect();
        assert_eq!(leaf.len(), 1, "{id} renders on the mail row only");
        assert_eq!(
            leaf[0].path.first().map(|(p, i)| (p.as_str(), *i)),
            Some((ids::CONNECTED_APPS_ITEM, 1)),
            "{id} scopes inside its own item"
        );
    }
    assert_eq!(
        find(&els, ids::CONNECTED_APPS_ITEM_USERNAME).text,
        "alice+phone@example.test"
    );
    assert_eq!(
        find(&els, ids::CONNECTED_APPS_ITEM_TYPE).text,
        fauna_i18n::strings::settings::mail::KIND_PASSWORD
    );
    // Revoke is the roster's own.
    assert_eq!(
        els.iter()
            .filter(|e| e.id == ids::CONNECTED_APPS_ITEM_REVOKE)
            .count(),
        2
    );
}

/// The roster is mixed-class: an ATProto app-password session shares the
/// class and the badge, and paints none of the mail leaves.
#[test]
fn an_app_password_row_that_is_not_mail_paints_no_mail_leaf() {
    let app = app_with(|s| s.principals = vec![row("session:01", class::APP_PASSWORD, "Skeets")]);
    let els = els(&app);
    for id in MAIL_LEAVES {
        assert!(!has(&els, id), "{id} must not render on a non-mail row");
    }
}

/// ⚠ The load-bearing one. The driver reads the secret by polling the leaf's
/// text until it turns NON-EMPTY, so an unrevealed row's text must be empty —
/// a mask would be returned *as the secret* without the read ever running.
#[test]
fn the_secret_stays_empty_until_revealed() {
    let mut app = app_with(|s| s.principals = vec![mail_row("default", "Default", false)]);
    let before = els(&app);
    assert_eq!(find(&before, ids::CONNECTED_APPS_ITEM_SECRET).text, "");
    assert_eq!(
        find(&before, ids::CONNECTED_APPS_ITEM_REVEAL_SECRET).text,
        mail_t::REVEAL_SECRET
    );

    app.settings
        .connected_apps
        .revealed
        .insert("mail:default".into(), SecretString::new("s3cret".into()));
    let after = els(&app);
    assert_eq!(
        find(&after, ids::CONNECTED_APPS_ITEM_SECRET).text,
        "s3cret",
        "the registered text is the BARE secret — the label is paint-only"
    );
    assert_eq!(
        find(&after, ids::CONNECTED_APPS_ITEM_REVEAL_SECRET).text,
        mail_t::HIDE_SECRET
    );
}

/// Revealing needs the machine's read; hiding is local and drops the secret.
#[test]
fn reveal_asks_the_machine_and_hiding_is_local() {
    let mut app = app_with(|s| s.principals = vec![mail_row("default", "Default", false)]);
    let key = "mail:default".to_string();
    match super::super::apply_local(&mut app, Action::ConnectedAppsRevealSecret(key.clone())) {
        Some(super::super::Op::ConnectedAppsSecret {
            key: k,
            to_clipboard,
            ..
        }) => {
            assert_eq!(k, key);
            assert!(!to_clipboard);
        }
        other => panic!("expected the secret op, got {}", other.is_some()),
    }
    app.settings
        .connected_apps
        .revealed
        .insert(key.clone(), SecretString::new("s3cret".into()));
    assert!(
        super::super::apply_local(&mut app, Action::ConnectedAppsRevealSecret(key.clone()))
            .is_none(),
        "hiding issues no op"
    );
    assert!(app.settings.connected_apps.revealed.is_empty());
}

/// A copy must put the secret on the clipboard WITHOUT painting it.
#[test]
fn a_copied_secret_is_never_painted() {
    let mut app = app_with(|s| s.principals = vec![mail_row("default", "Default", false)]);
    let snapshot = app.settings.connected_apps.snapshot.clone().unwrap();
    super::super::apply_outcome(
        &mut app,
        super::super::Outcome::ConnectedAppsSecret {
            key: "mail:default".into(),
            secret: Some(SecretString::new("s3cret".into())),
            to_clipboard: true,
            snapshot: Box::new(snapshot.clone()),
        },
    );
    assert!(app.settings.connected_apps.revealed.is_empty());

    super::super::apply_outcome(
        &mut app,
        super::super::Outcome::ConnectedAppsSecret {
            key: "mail:default".into(),
            secret: Some(SecretString::new("s3cret".into())),
            to_clipboard: false,
            snapshot: Box::new(snapshot),
        },
    );
    assert!(
        app.settings
            .connected_apps
            .revealed
            .contains_key("mail:default")
    );
}

/// A refused read reaches `error-message` — the user clicked a button, so
/// silence would be the dropped-command shape — and stores nothing.
#[test]
fn a_failed_secret_read_reaches_error_message() {
    let mut app = app_with(|s| s.principals = vec![mail_row("default", "Default", false)]);
    let mut snapshot = app.settings.connected_apps.snapshot.clone().unwrap();
    snapshot.error = Some(LocalizedText::key_arg(
        "connected_apps.error_secret",
        "message",
        "unknown credential".to_string(),
    ));
    super::super::apply_outcome(
        &mut app,
        super::super::Outcome::ConnectedAppsSecret {
            key: "mail:default".into(),
            secret: None,
            to_clipboard: false,
            snapshot: Box::new(snapshot),
        },
    );
    let shown = app
        .errors
        .get(&crate::pages::Page::Settings)
        .cloned()
        .unwrap_or_default();
    assert!(shown.contains("unknown credential"), "{shown:?}");
    assert!(app.settings.connected_apps.revealed.is_empty());
}

/// A revoke takes the shown secret off the screen with its row — otherwise a
/// dead secret stays painted against whichever row slid up into its place.
#[test]
fn a_revoked_row_takes_its_shown_secret_off_screen() {
    let mut app = app_with(|s| {
        s.principals = vec![
            mail_row("default", "Default", false),
            mail_row("phone", "Phone", false),
        ]
    });
    let c = &mut app.settings.connected_apps;
    c.revealed
        .insert("mail:phone".into(), SecretString::new("gone".into()));
    c.revealed
        .insert("mail:default".into(), SecretString::new("kept".into()));
    let mut after = c.snapshot.clone().unwrap();
    after.principals.retain(|r| r.key != "mail:phone");
    super::super::apply_outcome(
        &mut app,
        super::super::Outcome::ConnectedAppsSnapshot(Box::new(after)),
    );
    let revealed = &app.settings.connected_apps.revealed;
    assert!(!revealed.contains_key("mail:phone"));
    assert!(revealed.contains_key("mail:default"));
}

/// Leaving the page takes every shown secret with it, and a fresh visit starts
/// unread: the previous visit's rows never paint while this visit's read is
/// still in flight.
#[test]
fn a_fresh_visit_drops_shown_secrets_and_the_last_visits_rows() {
    let mut app = app_with(|s| s.principals = vec![mail_row("default", "Default", false)]);
    app.settings
        .connected_apps
        .revealed
        .insert("mail:default".into(), SecretString::new("s3cret".into()));
    let _ = super::super::apply_local(&mut app, Action::OpenConnectedApps);
    assert!(app.settings.connected_apps.revealed.is_empty());
    let els = els(&app);
    assert!(!has(&els, ids::CONNECTED_APPS_ITEM));
    assert!(!has(&els, ids::CONNECTED_APPS_EMPTY));
}

/// A password the succession burn killed stays listed — it is the user's list
/// of which mail apps to set up again — and says so, in words and as a value.
#[test]
fn a_burned_mail_row_says_it_lost_access() {
    let app = app_with(|s| s.principals = vec![mail_row("default", "Default", true)]);
    let els = els(&app);
    let item = find(&els, ids::CONNECTED_APPS_ITEM);
    assert_eq!(attr(item, "revoked"), Some("true"));
    assert!(item.text.contains(mail_t::CREDENTIAL_REVOKED));
    assert!(item.text.contains(t::NOT_CONNECTED));
    let lines: Vec<_> = item.text.lines().collect();
    assert_eq!(
        lines.get(1).copied(),
        Some(mail_t::CREDENTIAL_REVOKED),
        "the burned state sits directly under the name"
    );
}

/// Copy login is local: the snapshot already holds the address.
#[test]
fn copying_the_login_issues_no_op() {
    let mut app = app_with(|s| s.principals = vec![mail_row("default", "Default", false)]);
    assert!(
        super::super::apply_local(
            &mut app,
            Action::ConnectedAppsCopyUsername("mail:default".into())
        )
        .is_none()
    );
}

/// Every row says when it was made — the roster model's *created* column.
#[test]
fn a_row_says_when_it_was_made() {
    let app = app_with(|s| s.principals = vec![row("principal:01", class::DEVICE, "A")]);
    let els = els(&app);
    let item = find(&els, ids::CONNECTED_APPS_ITEM);
    let stamp = fauna_core::format::format_unix_local_ms(1_700_000_000_000);
    assert!(item.text.contains(&t::created(&stamp)), "{:?}", item.text);
}

// ── Blocked apps ─────────────────────────────────────────────────────────

fn blocked(client_id: &str) -> fauna_client_connected_apps::BlockedAppRow {
    fauna_client_connected_apps::BlockedAppRow {
        client_id: client_id.into(),
        blocked_at_millis: 1_700_000_000_000,
    }
}

/// The section paints only while something is blocked — no heading, no row
/// and no empty state otherwise.
#[test]
fn the_blocked_section_paints_only_while_something_is_blocked() {
    let app = app_with(|_| {});
    let none = els(&app);
    assert!(!has(&none, ids::CONNECTED_APPS_BLOCKED_ITEM));
    assert!(!none.iter().any(|e| e.text == t::BLOCKED_HEADING));

    let app = app_with(|s| {
        s.blocked = vec![
            blocked("https://pushy.example/client.json"),
            blocked("https://other.example/client.json"),
        ]
    });
    let els = els(&app);
    assert!(els.iter().any(|e| e.text == t::BLOCKED_HEADING));
    let rows: Vec<_> = els
        .iter()
        .filter(|e| e.id == ids::CONNECTED_APPS_BLOCKED_ITEM)
        .collect();
    assert_eq!(rows.len(), 2);
    assert!(
        rows[0].text.contains("https://pushy.example/client.json"),
        "the client id renders verbatim"
    );
    let unblock: Vec<_> = els
        .iter()
        .filter(|e| e.id == ids::CONNECTED_APPS_BLOCKED_ITEM_UNBLOCK)
        .collect();
    assert_eq!(unblock.len(), 2);
    for (n, el) in unblock.iter().enumerate() {
        assert_eq!(
            el.path.first().map(|(p, i)| (p.as_str(), *i)),
            Some((ids::CONNECTED_APPS_BLOCKED_ITEM, n)),
            "Unblock scopes inside its own row"
        );
    }
}

/// Unblock carries the row's client id — never its index — to the machine.
#[test]
fn unblock_carries_the_client_id_to_the_machine() {
    let mut app = app_with(|s| s.blocked = vec![blocked("https://pushy.example/client.json")]);
    let op = super::super::apply_local(
        &mut app,
        Action::ConnectedAppsUnblock("https://pushy.example/client.json".into()),
    );
    match op {
        Some(super::super::Op::ConnectedApps { gesture, .. }) => assert_eq!(
            gesture,
            ConnectedAppsGesture::Unblock("https://pushy.example/client.json".into())
        ),
        other => panic!("expected the unblock op, got {}", other.is_some()),
    }
}

/// The `fauna://consent/<request_uri>` route (`App::apply_route`) lands on this
/// page and, through the shared machine's handoff open, paints the opened
/// request's card in the tray.
#[tokio::test]
async fn the_consent_route_lands_its_card_in_the_tray() {
    const HANDLE: &str = "urn:ietf:params:oauth:request_uri:abc_DEF-123";
    let fake = Arc::new(fauna_client_connected_apps::FakeConnectedAppsNestApi::new());
    fake.add_handoff(
        HANDLE,
        fauna_atproto_settings_machine::NestConsentRow {
            consent_id: vec![7; 16],
            code: "CODE7".into(),
            client_id: "https://cli.example/c.json".into(),
            client_name: Some("Requester".into()),
            scopes: vec!["atproto".into()],
            ..Default::default()
        },
    );
    let mut app = authed_app();
    app.settings.connected_apps.machine =
        Some(ConnectedAppsMachine::new(Arc::new(NoopObserver), fake));
    let op = app
        .apply_route(fauna_core::app_route::AppRoute::Consent {
            request_uri: HANDLE.into(),
        })
        .expect("the consent route's work is the handoff open");
    assert_eq!(app.page, crate::pages::Page::Settings);
    assert_eq!(app.settings.sub, super::super::SubPage::ConnectedApps);
    let outcome = op.run().await;
    crate::app::apply_page_outcome(&mut app, outcome);
    let snap = app.settings.connected_apps.snapshot.clone().unwrap();
    assert_eq!(snap.error, None);
    assert_eq!(snap.requests.len(), 1);
    assert_eq!(snap.requests[0].client_name.as_deref(), Some("Requester"));
}
