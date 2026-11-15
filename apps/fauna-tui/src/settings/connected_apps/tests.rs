use super::*;
use crate::app::App;
use crate::app::tests::authed_app;
use fauna_client_connected_apps::compose::{ComposedText, oracle_lines, set_lines};
use fauna_client_connected_apps::{ConsentSetRow, VERBATIM_KEY, class};
use fauna_core::localized::LocalizedText;

fn resolved(lines: Vec<ComposedText>) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.resolve(fauna_i18n::strings::lookup))
        .collect()
}

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
        sets: Vec::new(),
        created_at_millis: 1_700_000_000_000,
        last_used_at_millis: None,
        lasts_until_millis: None,
        connected: true,
        paused: false,
        suspended: false,
        mail: None,
        signer_burned: false,
        oracle: None,
        trust: None,
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
        sets: Vec::new(),
        created_at_millis: 0,
        last_used_at_millis: None,
        lasts_until_millis: None,
        connected: !revoked,
        paused: false,
        suspended: false,
        mail: Some(fauna_client_connected_apps::MailAppPassword {
            mua_username: format!("{{handle}}+{id}@example.test"),
            kind: LocalizedText::key("settings.mail.kind_password"),
            revoked,
        }),
        signer_burned: false,
        oracle: None,
        trust: None,
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
        install: None,
        folder_picker: None,
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

fn install_request(development: bool) -> ConsentCardRow {
    let mut card = request("beef", "INS-TAL", Some("Hello Plugin"));
    card.install = Some(fauna_atproto_settings_machine::ConsentInstallRow {
        execution_form: "wasm".into(),
        requested_kinds: vec!["ext.plugins.example.notes".into()],
        settings: vec![fauna_atproto_settings_machine::ConsentInstallSettingRow {
            key: "greeting".into(),
            label: "Greeting".into(),
            setting_type: "string".into(),
        }],
        publisher_domain: "plugins.example".into(),
        publisher_key: (!development).then(|| "did:key:pub".into()),
        module_digest: "sha256:0123".into(),
        hosts: vec!["api.plugins.example".into()],
        ingress_paths: vec![],
        development,
        bind: false,
    });
    card
}

/// An install card paints its section's seven leaves inside the card, each
/// with its own fact; an ordinary card paints none of them.
#[test]
fn an_install_card_paints_its_section_inside_the_card() {
    let app = app_with(|s| s.requests = vec![install_request(false)]);
    let els = els(&app);
    let leaf = |id: &str| -> &Element {
        let el = find(&els, id);
        assert_eq!(
            el.path.first(),
            Some(&(ids::CONNECTED_APPS_REQUEST_CARD.to_string(), 0)),
            "{id} is scoped within its card"
        );
        el
    };
    assert!(!leaf(ids::CONNECTED_APPS_REQUEST_INSTALL).text.is_empty());
    let heading_at = els
        .iter()
        .position(|e| e.id == ids::CONNECTED_APPS_REQUEST_INSTALL)
        .unwrap();
    assert!(
        els[heading_at + 1]
            .text
            .starts_with("Installing runs this code on your nest."),
        "the framing line sits directly under the heading: {:?}",
        els[heading_at + 1].text
    );
    let form = leaf(ids::CONNECTED_APPS_REQUEST_INSTALL_FORM);
    assert_eq!(attr(form, "form"), Some("wasm"));
    assert!(form.text.contains("wasm"));
    let publisher = &leaf(ids::CONNECTED_APPS_REQUEST_INSTALL_PUBLISHER).text;
    assert!(publisher.contains("plugins.example") && publisher.contains("did:key:pub"));
    assert!(
        leaf(ids::CONNECTED_APPS_REQUEST_INSTALL_DIGEST)
            .text
            .contains("sha256:0123")
    );
    assert!(
        leaf(ids::CONNECTED_APPS_REQUEST_INSTALL_KINDS)
            .text
            .contains("ext.plugins.example.notes")
    );
    assert!(
        leaf(ids::CONNECTED_APPS_REQUEST_INSTALL_HOSTS)
            .text
            .contains("api.plugins.example")
    );
    assert!(
        leaf(ids::CONNECTED_APPS_REQUEST_INSTALL_SETTINGS)
            .text
            .contains("Greeting (string)")
    );

    let plain = app_with(|s| s.requests = vec![request("abcd", "ABC-DEF", Some("Example App"))]);
    assert!(!has(
        &connected_apps_elements(&plain.settings),
        ids::CONNECTED_APPS_REQUEST_INSTALL
    ));
}

/// A development plugin's card names it as one and paints no `did:key`.
#[test]
fn a_development_install_card_paints_no_key() {
    let app = app_with(|s| s.requests = vec![install_request(true)]);
    let publisher = find(&els(&app), ids::CONNECTED_APPS_REQUEST_INSTALL_PUBLISHER)
        .text
        .clone();
    assert!(publisher.contains("Development plugin"), "{publisher}");
    assert!(!publisher.contains("did:key"), "{publisher}");
}

/// An authed app whose page holds a machine over the in-memory seam — the
/// fixture builds no page machines itself.
fn app_with_machine() -> App {
    let mut app = authed_app();
    app.settings.connected_apps.machine = Some(ConnectedAppsMachine::new(
        Arc::new(NullObserver),
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

// ── The trust facet (connected-apps.md § The trust facet and history lens) ──

const FACET_LEAVES: [&str; 7] = [
    ids::NEST_TRUST_VIEW_NOW,
    ids::NEST_TRUST_VIEW_HISTORY,
    ids::NEST_TRUST_GRANT_LIST,
    ids::NEST_TRUST_EMPTY,
    ids::NEST_TRUST_GRANT_ITEM,
    ids::NEST_TRUST_HISTORY_LIST,
    ids::NEST_TRUST_HISTORY_ITEM,
];

fn ext_grant(lasts_until: i64, liveness: fauna_client_pair::TrustLiveness) -> TrustGrantRow {
    TrustGrantRow {
        grant_id: vec![7; 16],
        holder: vec![9; 32],
        scope: vec![
            fauna_client_pair::TrustScope {
                class: "content.read".into(),
                kind: Some("ext.example.com.notes".into()),
                tier: None,
            },
            fauna_client_pair::TrustScope {
                class: "content.write".into(),
                kind: Some("ext.example.com.notes".into()),
                tier: None,
            },
        ],
        lasts_until,
        liveness,
        unattested: false,
        folder: None,
    }
}

fn faceted(lens: TrustLens, grants: Vec<TrustGrantRow>) -> ConnectedAppRow {
    let history = grants
        .iter()
        .map(|g| TrustHistoryRow {
            grant_id: g.grant_id.clone(),
            holder: g.holder.clone(),
            kind: fauna_client_pair::TrustEventKind::Mint,
            scope: g.scope.clone(),
            window_start: 0,
            window_end: g.lasts_until,
            at: 1_700_000_000,
            folder: None,
        })
        .collect();
    ConnectedAppRow {
        trust: Some(PrincipalTrust {
            lens,
            grants,
            history,
        }),
        ..row("principal:01", class::DEVICE, "Notes")
    }
}

/// No facet leaf at all on a row with no facet — absent, never an empty state.
#[test]
fn a_row_with_no_facet_paints_no_trust_leaf() {
    let app = app_with(|s| s.principals = vec![row("principal:01", class::DEVICE, "Notes")]);
    let els = els(&app);
    for id in FACET_LEAVES {
        assert!(!has(&els, id), "{id} must be absent");
    }
}

/// A keyed principal with no live key paints the lens pair and the empty
/// state, and no grant list.
#[test]
fn a_facet_with_no_key_paints_the_empty_state() {
    let app = app_with(|s| s.principals = vec![faceted(TrustLens::Now, Vec::new())]);
    let els = els(&app);
    assert!(has(&els, ids::NEST_TRUST_VIEW_NOW) && has(&els, ids::NEST_TRUST_VIEW_HISTORY));
    assert_eq!(find(&els, ids::NEST_TRUST_EMPTY).text, t::TRUST_EMPTY);
    assert!(!has(&els, ids::NEST_TRUST_GRANT_LIST));
}

/// The grant row's leaves: the publisher and the kind name verbatim, read and
/// write, lasts-until, the liveness, the bound note — all inside the item,
/// with no renew or revoke of its own; an expired key says the app asks again.
#[test]
fn a_grant_row_names_publisher_kinds_and_its_liveness() {
    let app = app_with(|s| {
        s.principals = vec![faceted(
            TrustLens::Now,
            vec![ext_grant(
                4_000_000_000,
                fauna_client_pair::TrustLiveness::Expired,
            )],
        )]
    });
    let els = els(&app);
    let scope = find(&els, ids::NEST_TRUST_GRANT_SCOPE);
    assert_eq!(
        scope.text,
        t::trust_scope_ext_rw("example.com", "\"notes\"")
    );
    assert_eq!(
        scope.path.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(),
        [ids::CONNECTED_APPS_ITEM, ids::NEST_TRUST_GRANT_ITEM]
    );
    assert!(
        find(&els, ids::NEST_TRUST_GRANT_LASTS_UNTIL)
            .text
            .starts_with(nest_t::LASTS_UNTIL)
    );
    assert_eq!(
        find(&els, ids::NEST_TRUST_GRANT_STATUS).text,
        t::TRUST_STATUS_EXPIRED
    );
    assert_eq!(
        find(&els, ids::NEST_TRUST_GRANT_BOUND_NOTE).text,
        t::TRUST_BOUND_NOTE
    );
    for id in [
        ids::NEST_TRUST_GRANT_RENEW,
        ids::NEST_TRUST_GRANT_REVOKE,
        ids::NEST_TRUST_GRANT_UNATTESTED_MARK,
        ids::NEST_TRUST_GRANT_KEEP_BUTTON,
    ] {
        assert!(!has(&els, id), "{id} never renders on a principal row");
    }
    assert!(has(&els, ids::CONNECTED_APPS_ITEM_REVOKE));
}

/// The lens toggles the leaf set: History paints the timeline and no grant,
/// Now the grant and no timeline; the toggle dispatches by row key.
#[test]
fn the_lens_toggles_the_leaf_set() {
    let grants = || {
        vec![ext_grant(
            4_000_000_000,
            fauna_client_pair::TrustLiveness::Active,
        )]
    };
    let history = els(&app_with(|s| {
        s.principals = vec![faceted(TrustLens::History, grants())]
    }));
    assert!(has(&history, ids::NEST_TRUST_HISTORY_LIST));
    let line = &find(&history, ids::NEST_TRUST_HISTORY_ITEM).text;
    assert!(line.contains("\"notes\""), "{line}");
    assert!(!has(&history, ids::NEST_TRUST_GRANT_ITEM));

    let now = els(&app_with(|s| {
        s.principals = vec![faceted(TrustLens::Now, grants())]
    }));
    assert!(has(&now, ids::NEST_TRUST_GRANT_ITEM));
    assert!(!has(&now, ids::NEST_TRUST_HISTORY_LIST));
    assert!(matches!(
        &find(&now, ids::NEST_TRUST_VIEW_HISTORY).role,
        crate::element::Role::Button(Gesture::Settings(Action::ConnectedAppsSetLens { key, history: true }))
            if key == "principal:01"
    ));
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

/// A hosted plugin's row — and only that — links to its settings card, and the
/// link opens the Bridges page, where the card is.
#[test]
fn a_plugin_row_links_to_its_settings_card_on_the_bridges_page() {
    let mut app = app_with(|s| {
        s.principals = vec![
            row("principal:01", class::DEVICE, "Device App"),
            row("principal:02", class::WASM, "Hello Plugin"),
        ]
    });
    let reg = crate::automation::Registry::of(els(&app));
    let scope = |i| [(ids::CONNECTED_APPS_ITEM.to_string(), i)];
    assert_eq!(
        reg.count_scoped(ids::CONNECTED_APPS_ITEM_SETTINGS_LINK, &scope(0)),
        0
    );
    assert_eq!(
        reg.count_scoped(ids::CONNECTED_APPS_ITEM_SETTINGS_LINK, &scope(1)),
        1
    );
    let link = els(&app)
        .into_iter()
        .find(|e| e.id == ids::CONNECTED_APPS_ITEM_SETTINGS_LINK)
        .unwrap();
    let crate::element::Role::Button(gesture) = link.role else {
        panic!("the link is a button");
    };
    app.bridges.nest = Some(fauna_client::NestClient::new(
        "http://127.0.0.1:9".to_string(),
        fauna_core::identity::ActorKeypair::from_secret([7u8; 32]),
    ));
    let Gesture::Bridges(action) = gesture else {
        panic!("the link is a Bridges gesture");
    };
    let op = crate::bridges::apply_local(&mut app, action);
    assert_eq!(app.page, crate::pages::Page::Bridges);
    assert!(
        matches!(op, Some(crate::bridges::Op::Refresh { .. })),
        "opening the page reads it, as entering its tab does"
    );
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

/// A signer client the succession burn killed is the same state for its class:
/// listed, not connected, the revoked line directly under the name and the
/// `revoked` attribute beside it; a live signer row carries neither.
#[test]
fn a_burned_signer_row_says_it_lost_access() {
    let burned = ConnectedAppRow {
        connected: false,
        signer_burned: true,
        ..row("signer:b1", class::SIGNER, "Amethyst")
    };
    let live = row("signer:b2", class::SIGNER, "Damus");
    let app = app_with(|s| s.principals = vec![burned, live]);
    let els = els(&app);
    let items: Vec<_> = els
        .iter()
        .filter(|e| e.id == ids::CONNECTED_APPS_ITEM)
        .collect();
    assert_eq!(items.len(), 2);
    let burned = items[0];
    assert_eq!(attr(burned, "revoked"), Some("true"));
    assert!(burned.text.contains(t::NOT_CONNECTED));
    assert_eq!(
        burned.text.lines().nth(1),
        Some(t::SIGNER_REVOKED),
        "the burned state sits directly under the name"
    );
    assert_eq!(attr(items[1], "revoked"), None);
    assert!(!items[1].text.contains(t::SIGNER_REVOKED));
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

/// A row names the permission set its permissions came from, after the
/// permission list and in the consent card's own lines.
#[test]
fn a_row_names_the_permission_set_after_its_permissions() {
    let set = ConsentSetRow {
        nsid: "com.example.authFull".into(),
        title: Some("Full access".into()),
        details: None,
        member_descriptions: vec!["Post on your behalf".into()],
    };
    let app = app_with(|s| {
        s.principals = vec![ConnectedAppRow {
            sets: vec![set.clone()],
            ..row("principal:01", class::DEVICE, "A")
        }]
    });
    let els = els(&app);
    let item = find(&els, ids::CONNECTED_APPS_ITEM);
    let heading = card_t::consent_set_heading("Full access", "com.example.authFull");
    let at = item.text.find(&heading).expect(&item.text);
    assert!(
        item.text.find("Upload image files").unwrap() < at,
        "after the permission list: {}",
        item.text
    );
    assert!(
        item.text.contains(&resolved(set_lines(&set)).join("\n")),
        "the card's own lines: {}",
        item.text
    );
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

/// The external-apps switch paints at every integration level — it no longer
/// waits for a full-PDS account to reach it — before any read has returned,
/// and carries the nest's answer in its `state` attr.
#[test]
fn the_external_apps_switch_paints_before_and_after_the_first_read() {
    let before = els(&authed_app());
    let toggle = before
        .iter()
        .find(|e| e.id == ids::ATPROTO_EXTERNAL_APPS_ENABLE)
        .expect("the switch is on the page before the first read");
    assert_eq!(
        attr(toggle, "state"),
        Some("on"),
        "the flag's default is ON"
    );

    let app = app_with(|s| s.external_apps_off = true);
    let els = els(&app);
    let at = els
        .iter()
        .position(|e| e.id == ids::ATPROTO_EXTERNAL_APPS_ENABLE)
        .unwrap();
    assert_eq!(attr(&els[at], "state"), Some("off"));
    assert_eq!(
        els[at + 1].text,
        card_t::EXTERNAL_APPS_HINT,
        "the hint says what OFF pauses, right under the switch"
    );
    let roster = els
        .iter()
        .position(|e| e.text == t::ROSTER_HEADING)
        .unwrap();
    assert!(at < roster, "the switch sits above the rows it pauses");
}

/// The switch flips the nest's value, read off the snapshot — never a carried
/// guess — and routes through the shared connected-apps machine.
#[test]
fn the_switch_carries_the_flipped_value_to_the_machine() {
    for (off, want) in [(false, false), (true, true)] {
        let mut app = app_with(|s| s.external_apps_off = off);
        let op = super::super::apply_local(&mut app, Action::ConnectedAppsToggleExternalApps);
        match op {
            Some(super::super::Op::ConnectedApps { gesture, .. }) => {
                assert_eq!(gesture, ConnectedAppsGesture::SetExternalApps(want))
            }
            other => panic!("expected the switch op, got {}", other.is_some()),
        }
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
        Some(ConnectedAppsMachine::new(Arc::new(NullObserver), fake));
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

/// The account's external-apps switch OFF pauses a principal: the row stays
/// listed and says it is paused and how to undo it — never the bare "not
/// connected" — and reports the state as a value.
#[test]
fn a_paused_row_reads_paused_in_words_and_as_a_value() {
    let app = app_with(|s| {
        s.principals = vec![ConnectedAppRow {
            connected: false,
            paused: true,
            ..row("principal:01", class::DEVICE, "Example App")
        }]
    });
    let els = els(&app);
    let item = find(&els, ids::CONNECTED_APPS_ITEM);
    assert!(item.text.contains(t::PAUSED), "{}", item.text);
    assert!(!item.text.contains(t::NOT_CONNECTED), "{}", item.text);
    assert_eq!(attr(item, "status"), Some("paused"));
    assert!(
        has(&els, ids::CONNECTED_APPS_ITEM_REVOKE),
        "still revocable"
    );
}

/// A principal whose every OAuth grant a step-down suspended is paused too:
/// the same `status` value, worded with its own undo, no lasts-until, and
/// still revocable (`ui/atproto.md`'s downward matrix).
#[test]
fn a_suspended_row_reads_paused_with_its_own_undo() {
    let app = app_with(|s| {
        s.principals = vec![ConnectedAppRow {
            connected: false,
            paused: true,
            suspended: true,
            lasts_until_millis: None,
            ..row("principal:01", class::DEVICE, "Example App")
        }]
    });
    let els = els(&app);
    let item = find(&els, ids::CONNECTED_APPS_ITEM);
    assert!(item.text.contains(t::PAUSED_SUSPENDED), "{}", item.text);
    assert!(!item.text.contains(t::OPEN_ENDED), "{}", item.text);
    assert_eq!(attr(item, "status"), Some("paused"));
    assert!(
        has(&els, ids::CONNECTED_APPS_ITEM_REVOKE),
        "still revocable"
    );
}

#[test]
fn every_row_reports_whether_it_can_act() {
    let app = app_with(|s| {
        s.principals = vec![
            row("principal:01", class::DEVICE, "Working App"),
            ConnectedAppRow {
                connected: false,
                ..row("principal:02", class::DEVICE, "Idle App")
            },
        ]
    });
    let els = els(&app);
    let items: Vec<_> = els
        .iter()
        .filter(|e| e.id == ids::CONNECTED_APPS_ITEM)
        .collect();
    assert_eq!(attr(items[0], "status"), Some("connected"));
    assert_eq!(attr(items[1], "status"), Some("not_connected"));
    assert!(items[1].text.contains(t::NOT_CONNECTED));
}

/// A principal the oracle served carries its summary in the row's own text —
/// the window's count, every request answered, and the newest operation in the
/// card's words with its detail — and the count as a value; a row it never
/// served carries neither (`connected-apps.md` § Layout & flow, region 3).
#[test]
fn a_served_principal_row_shows_the_oracle_summary() {
    use fauna_client_connected_apps::{OracleOpRow, OracleSummaryRow};
    let words = "Ask your nest to sign Nostr posts";
    let app = app_with(|s| {
        s.principals = vec![
            ConnectedAppRow {
                oracle: Some(OracleSummaryRow {
                    ops_in_window: 3,
                    requests_served: 1,
                    last_op: Some(OracleOpRow {
                        at_millis: 1_700_000_300_000,
                        class: "nostr.sign_event".into(),
                        words: verbatim(words),
                        detail: verbatim("1"),
                    }),
                }),
                ..row("principal:01", class::DEVICE, "Served App")
            },
            row("principal:02", class::DEVICE, "Quiet App"),
        ]
    });
    let els = els(&app);
    let items: Vec<_> = els
        .iter()
        .filter(|e| e.id == ids::CONNECTED_APPS_ITEM)
        .collect();
    let text = &items[0].text;
    assert!(text.contains(&t::oracle_ops("3")), "{text}");
    assert!(text.contains(t::ORACLE_REQUESTS_ONE), "{text}");
    assert!(
        text.contains(&t::oracle_latest(&when(1_700_000_300_000), words, "1")),
        "{text}"
    );
    assert_eq!(attr(items[0], "oracle_ops"), Some("3"));
    assert_eq!(attr(items[1], "oracle_ops"), None);
    assert!(
        !items[1].text.contains(&t::oracle_ops("0")),
        "{}",
        items[1].text
    );
}

/// An operation whose class carries no detail reads without a dangling dash,
/// and a single operation in the window reads in the singular.
#[test]
fn an_oracle_summary_without_a_detail_reads_bare() {
    use fauna_client_connected_apps::{OracleOpRow, OracleSummaryRow};
    let lines = resolved(oracle_lines(&OracleSummaryRow {
        ops_in_window: 1,
        requests_served: 4,
        last_op: Some(OracleOpRow {
            at_millis: 0,
            class: "post.author".into(),
            words: verbatim("Post public posts as you"),
            detail: verbatim(""),
        }),
    }));
    assert_eq!(
        lines,
        [
            format!("{} · {}", t::ORACLE_OPS_ONE, t::oracle_requests("4")),
            t::oracle_latest_bare(&when(0), "Post public posts as you"),
        ]
    );
}

/// A served row's open control opens its record: closed, the row carries only
/// the control; open and read, the list newest first — each operation in the
/// shared words with its class as a value, scoped within the row — or the
/// empty state; another row's record paints nothing on this one
/// (`connected-apps.md` § Layout & flow, region 3).
#[test]
fn a_served_row_opens_its_record_newest_first_and_only_on_that_row() {
    use fauna_client_connected_apps::{OracleOpRow, OracleRecord, OracleSummaryRow};
    let served = |key: &str, name: &str| ConnectedAppRow {
        oracle: Some(OracleSummaryRow {
            ops_in_window: 2,
            requests_served: 2,
            last_op: None,
        }),
        ..row(key, class::DEVICE, name)
    };
    let op = |at: i64, cls: &str, detail: &str| OracleOpRow {
        at_millis: at,
        class: cls.into(),
        words: verbatim("Sign in to a service"),
        detail: verbatim(detail),
    };
    let closed = app_with(|s| {
        s.principals = vec![
            served("principal:01", "A"),
            row("principal:02", class::DEVICE, "Quiet"),
            // An app-password session's row carries the summary but no record.
            served("session:abc", "Session"),
        ]
    });
    let e = els(&closed);
    let opens: Vec<_> = e
        .iter()
        .filter(|el| el.id == ids::CONNECTED_APPS_ORACLE_OPEN)
        .collect();
    assert_eq!(
        opens.len(),
        1,
        "only the served principal row carries the control"
    );
    assert_eq!(opens[0].path[0].1, 0);
    assert_eq!(opens[0].text, t::ORACLE_OPEN);
    assert!(!has(&e, ids::CONNECTED_APPS_ORACLE_LIST));

    let open = app_with(|s| {
        s.principals = vec![served("principal:01", "A"), served("principal:02", "B")];
        s.record = Some(OracleRecord {
            key: "principal:02".into(),
            loaded: true,
            ops: vec![
                op(1_700_000_200_000, "atproto.service_auth", "did:web:x a.b"),
                op(1_700_000_100_000, "nostr.sign_event", "1"),
            ],
        });
    });
    let e = els(&open);
    let opens: Vec<_> = e
        .iter()
        .filter(|el| el.id == ids::CONNECTED_APPS_ORACLE_OPEN)
        .collect();
    assert_eq!(
        opens[0].text,
        t::ORACLE_OPEN,
        "row A's record is not the open one"
    );
    assert_eq!(opens[1].text, t::ORACLE_CLOSE);
    let list = find(&e, ids::CONNECTED_APPS_ORACLE_LIST);
    assert_eq!(list.path[0].0, ids::CONNECTED_APPS_ITEM);
    assert_eq!(list.path[0].1, 1, "the list sits within the open row");
    let ops: Vec<_> = e
        .iter()
        .filter(|el| el.id == ids::CONNECTED_APPS_ORACLE_OP)
        .collect();
    assert_eq!(ops.len(), 2);
    assert!(ops[0].text.contains("did:web:x a.b"), "{}", ops[0].text);
    assert!(
        ops[0].text.starts_with(&when(1_700_000_200_000)),
        "{}",
        ops[0].text
    );
    assert_eq!(attr(ops[0], "class"), Some("atproto.service_auth"));
    assert_eq!(attr(ops[1], "class"), Some("nostr.sign_event"));
    assert!(!has(&e, ids::CONNECTED_APPS_ORACLE_EMPTY));

    let empty = app_with(|s| {
        s.principals = vec![served("principal:01", "A")];
        s.record = Some(OracleRecord {
            key: "principal:01".into(),
            loaded: true,
            ops: vec![],
        });
    });
    let e = els(&empty);
    assert!(has(&e, ids::CONNECTED_APPS_ORACLE_LIST));
    assert_eq!(
        find(&e, ids::CONNECTED_APPS_ORACLE_EMPTY).text,
        t::ORACLE_EMPTY
    );

    // Before the read returns, neither the list nor the empty state paints.
    let reading = app_with(|s| {
        s.principals = vec![served("principal:01", "A")];
        s.record = Some(OracleRecord {
            key: "principal:01".into(),
            loaded: false,
            ops: vec![],
        });
    });
    let e = els(&reading);
    assert!(!has(&e, ids::CONNECTED_APPS_ORACLE_LIST));
    assert!(!has(&e, ids::CONNECTED_APPS_ORACLE_EMPTY));
}

/// The control toggles: on a row whose record is not the open one it opens
/// that row's; on the open one it closes it.
#[test]
fn the_record_control_opens_then_closes_through_the_machine() {
    use fauna_client_connected_apps::OracleRecord;
    let key = "principal:01".to_string();
    let mut app = app_with(|s| s.principals = vec![row("principal:01", class::DEVICE, "A")]);
    match super::super::apply_local(&mut app, Action::ConnectedAppsToggleRecord(key.clone())) {
        Some(super::super::Op::ConnectedApps { gesture, .. }) => {
            assert_eq!(gesture, ConnectedAppsGesture::OpenRecord(key.clone()))
        }
        other => panic!("expected the open op, got {}", other.is_some()),
    }
    app.settings
        .connected_apps
        .snapshot
        .as_mut()
        .unwrap()
        .record = Some(OracleRecord {
        key: key.clone(),
        loaded: true,
        ops: vec![],
    });
    match super::super::apply_local(&mut app, Action::ConnectedAppsToggleRecord(key)) {
        Some(super::super::Op::ConnectedApps { gesture, .. }) => {
            assert_eq!(gesture, ConnectedAppsGesture::CloseRecord)
        }
        other => panic!("expected the close op, got {}", other.is_some()),
    }
}

// ── The card's folder picker (`connected-apps-request-folder`) ──────────────

fn folder_request(
    options: &[(i64, &str)],
    chosen: Option<i64>,
    none: Option<&str>,
) -> ConsentCardRow {
    ConsentCardRow {
        folder_picker: Some(fauna_atproto_settings_machine::FolderPicker {
            options: options
                .iter()
                .map(|(id, name)| fauna_atproto_settings_machine::FolderOption {
                    id: *id,
                    name: (*name).into(),
                })
                .collect(),
            chosen,
            none_eligible: none.map(LocalizedText::key),
        }),
        ..request("abcd", "ABC-DEF", Some("Example App"))
    }
}

fn approve(els: &[Element]) -> &Element {
    find(els, ids::CONNECTED_APPS_REQUEST_APPROVE)
}

/// A bare folder card paints the picker inside the card — the folders' ids as
/// the option keys, their names as the labels — and its Approve stays
/// disabled until a folder is chosen; Decline never waits on it.
#[test]
fn a_bare_folder_card_paints_the_picker_and_holds_approve_until_a_choice() {
    let app = app_with(|s| {
        s.requests = vec![folder_request(&[(9, "notes"), (42, "photos")], None, None)]
    });
    let els = els(&app);
    let picker = find(&els, ids::CONNECTED_APPS_REQUEST_FOLDER);
    assert!(
        picker
            .path
            .iter()
            .any(|(p, _)| p == ids::CONNECTED_APPS_REQUEST_CARD),
        "the picker is scoped within its card"
    );
    assert_eq!(picker.text, "", "nothing is chosen until the user chooses");
    match &picker.role {
        crate::element::Role::Select { options, .. } => assert_eq!(options, &["9", "42"]),
        _ => panic!("the picker is a real select"),
    }
    assert_eq!(attr(picker, "options"), Some(r#"["notes","photos"]"#));
    assert!(!approve(&els).enabled, "no choice, no approve");
    assert!(find(&els, ids::CONNECTED_APPS_REQUEST_DECLINE).enabled);

    let app = app_with(|s| {
        s.requests = vec![folder_request(
            &[(9, "notes"), (42, "photos")],
            Some(42),
            None,
        )]
    });
    let els = super::connected_apps_elements(&app.settings);
    assert_eq!(find(&els, ids::CONNECTED_APPS_REQUEST_FOLDER).text, "42");
    assert!(approve(&els).enabled, "a chosen folder enables the approve");
}

/// A card no folder can be chosen on says why in its own text and cannot be
/// approved.
#[test]
fn a_card_with_no_eligible_folder_says_why_and_cannot_be_approved() {
    let app = app_with(|s| {
        s.requests = vec![folder_request(
            &[],
            None,
            Some("atproto_settings.consent_folder_none_deposit"),
        )]
    });
    let els = els(&app);
    let card = find(&els, ids::CONNECTED_APPS_REQUEST_CARD);
    assert!(card.text.contains(card_t::CONSENT_FOLDER_NONE_DEPOSIT));
    assert!(!approve(&els).enabled);
}

/// An ordinary card paints no picker and its Approve is live.
#[test]
fn an_ordinary_card_paints_no_picker() {
    let app = app_with(|s| s.requests = vec![request("abcd", "ABC-DEF", None)]);
    let els = els(&app);
    assert!(!has(&els, ids::CONNECTED_APPS_REQUEST_FOLDER));
    assert!(approve(&els).enabled);
}

/// Choosing an option sends the card's consent id and the folder's row id to
/// the machine; a key that is no row id sends nothing.
#[test]
fn choosing_a_folder_sends_the_cards_consent_id_and_the_row_id() {
    let mut app = app_with(|s| s.requests = vec![folder_request(&[(42, "photos")], None, None)]);
    let action = Action::ConnectedAppsSetFolder {
        card: 0,
        id: "42".into(),
    };
    match super::super::apply_local(&mut app, action) {
        Some(super::super::Op::ConnectedApps { gesture, .. }) => assert_eq!(
            gesture,
            ConnectedAppsGesture::SetFolder {
                consent_id_hex: "abcd".into(),
                id: 42,
            }
        ),
        other => panic!("expected the set-folder op, got {}", other.is_some()),
    }
    let bad = Action::ConnectedAppsSetFolder {
        card: 0,
        id: "photos".into(),
    };
    assert!(super::super::apply_local(&mut app, bad).is_none());
}
