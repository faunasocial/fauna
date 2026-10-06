//! Unit tests for the enum ledger checks — pure functions, no git. Each rule of
//! transport.md § Schema and forward-compat discipline → *Rule 3 in full*'s
//! "The ledger and the gate" has a red case and the case that excuses it.

use super::*;
use crate::parse_ratified_breaks;

/// Scan `src` as crate `c`'s root file.
fn scan(src: &str) -> EnumMap {
    let mut s = EnumScan::default();
    s.add_file(src, "").expect("parse");
    s.finish("c")
}

fn ledger(text: &str) -> EnumLedger {
    parse_enum_ledger(text).expect("ledger parses")
}

fn messages(v: Vec<Violation>) -> Vec<String> {
    v.into_iter()
        .map(|v| format!("{}: {}", v.key, v.message))
        .collect()
}

fn state(src: &str, ledger_text: &str) -> Vec<String> {
    messages(check_enum_ledger(&scan(src), &ledger(ledger_text)))
}

fn diff(
    base: &str,
    head: &str,
    base_ledger: &str,
    head_ledger: &str,
    ratified: &str,
) -> Vec<String> {
    messages(diff_enum_maps(
        &scan(base),
        &scan(head),
        &ledger(base_ledger),
        &ledger(head_ledger),
        &parse_ratified_breaks(ratified).expect("ratified-breaks parses"),
    ))
}

const COLOR: &str = r#"
    #[derive(Serialize, Deserialize)]
    pub enum Color { Red, Green }
"#;

// ── Scan ────────────────────────────────────────────────────────────────────

#[test]
fn scan_keys_by_crate_and_module_and_resolves_wire_names() {
    let map = scan(
        r#"
        mod inner {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "snake_case")]
            pub enum Mode { FastPath, #[serde(rename = "slow")] SlowPath, #[serde(skip)] Never }
        }
        #[derive(Serialize)]
        pub enum EncodeOnly { A }
        "#,
    );
    assert_eq!(
        map.keys().collect::<Vec<_>>(),
        ["c::inner::Mode"],
        "Serialize-only enums are not scanned"
    );
    let mode = &map["c::inner::Mode"];
    assert_eq!(
        mode.variants.iter().collect::<Vec<_>>(),
        ["fast_path", "slow"]
    );
    assert!(!mode.has_arm);
}

#[test]
fn scan_skips_cfg_test_items_and_file_modules_but_not_test_hooks() {
    let mut s = EnumScan::default();
    s.add_file(
        r#"
        #[cfg(test)] mod messages;
        #[cfg(test)] mod inline { #[derive(Deserialize)] pub enum A { X } }
        #[cfg(all(test, unix))] #[derive(Deserialize)] pub enum B { X }
        #[cfg(feature = "test-hooks")] #[derive(Deserialize)] pub enum Hooked { X }
        #[cfg(not(test))] #[derive(Deserialize)] pub enum Shipped { X }
        "#,
        "",
    )
    .unwrap();
    s.add_file(
        "#[derive(Deserialize)] pub enum InTestFile { X }",
        "messages",
    )
    .unwrap();
    let map = s.finish("c");
    assert_eq!(map.keys().collect::<Vec<_>>(), ["c::Hooked", "c::Shipped"]);
}

#[test]
fn scan_reaches_a_helper_enum_inside_a_hand_written_deserialize() {
    let map = scan(
        r#"
        impl<'de> Deserialize<'de> for Pin {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                #[derive(Deserialize)]
                #[serde(untagged)]
                enum Repr { Bytes(Vec<u8>), Text(String) }
                todo!()
            }
        }
        "#,
    );
    assert!(map.contains_key("c::Repr"), "{map:?}");
}

#[test]
fn scan_recognises_each_arm_form() {
    let map = scan(
        r#"
        #[derive(Deserialize)] pub enum Other { A, #[serde(other)] Unknown }
        #[derive(Deserialize)] #[serde(untagged)] pub enum Untagged { A(u8), #[serde(untagged)] Rest(Value) }
        #[derive(Deserialize)] pub enum LastUntagged { A(u8), #[serde(untagged)] Rest(Value) }
        #[derive(Deserialize)] pub enum UntaggedNotLast { #[serde(untagged)] Rest(Value), A(u8) }
        #[derive(Serialize)] pub enum HandWritten { A }
        impl<'de> serde::Deserialize<'de> for HandWritten { fn deserialize<D>(d: D) {} }
        #[derive(Deserialize)] pub enum HandWrittenToo { A }
        impl<'de> Deserialize<'de> for HandWrittenToo { fn deserialize<D>(d: D) {} }
        "#,
    );
    assert!(map["c::Other"].has_arm);
    assert!(map["c::Untagged"].has_arm);
    assert!(map["c::LastUntagged"].has_arm);
    assert!(!map["c::UntaggedNotLast"].has_arm);
    assert!(map["c::HandWrittenToo"].has_arm);
    // Derives no `Deserialize`, but decodes all the same: scanned, with the
    // hand-written impl as its arm.
    assert!(map["c::HandWritten"].has_arm);
}

// ── Ledger grammar ──────────────────────────────────────────────────────────

#[test]
fn ledger_refuses_malformed_lines() {
    for bad in [
        "maybe c::Color  # reason",                // unknown answer
        "open c::Color",                           // no reason
        "open Color  # reason",                    // no crate
        "open c::*  # reason",                     // wildcard on a non-local answer
        "open c::Color  # a\nlocal c::Color  # b", // listed twice
        "open c::Color extra  # reason",           // stray token
    ] {
        assert!(parse_enum_ledger(bad).is_err(), "{bad:?} must not parse");
    }
}

/// The committed ledger parses: a bad token in it fails the gate with exit 2.
#[test]
fn committed_ledger_parses() {
    parse_enum_ledger(include_str!("../enum_ledger.txt")).expect("enum_ledger.txt parses");
}

// ── State check ─────────────────────────────────────────────────────────────

#[test]
fn unlisted_enum_is_red_and_a_line_or_crate_wildcard_lists_it() {
    let red = state(COLOR, "");
    assert_eq!(red.len(), 1, "{red:?}");
    assert!(
        red[0].contains("c::Color: deserializable enum with no enum_ledger.txt line"),
        "{red:?}"
    );
    assert_eq!(
        state(COLOR, "closed-request c::Color  # executed and discarded"),
        Vec::<String>::new()
    );
    assert_eq!(
        state(COLOR, "local c::*  # view models"),
        Vec::<String>::new()
    );
}

#[test]
fn open_without_an_arm_is_red() {
    let red = state(COLOR, "open c::Color  # claims an arm");
    assert!(
        red.len() == 1 && red[0].contains("carries no arm"),
        "{red:?}"
    );
    let with_arm = r#"
        #[derive(Deserialize)]
        pub enum Color { Red, Green, #[serde(other)] Unknown }
    "#;
    assert_eq!(
        state(with_arm, "open c::Color  # other"),
        Vec::<String>::new()
    );
}

#[test]
fn stale_line_and_stale_wildcard_are_red() {
    let red = state(
        COLOR,
        "closed-fixed c::Color  # two states\nopen c::Gone  # moved away\nlocal d::*  # no enums",
    );
    assert_eq!(red.len(), 2, "{red:?}");
    assert!(
        red.iter()
            .any(|m| m.starts_with("c::Gone: enum_ledger.txt line names no")),
        "{red:?}"
    );
    assert!(
        red.iter()
            .any(|m| m.starts_with("d::*: enum_ledger.txt crate wildcard covers no")),
        "{red:?}"
    );
}

#[test]
fn no_owed_is_red_on_each_owed_line() {
    let l = ledger(
        "owed-carry c::A  # x\nowed-collapse c::B  # x\nowed-skip c::C  # x\nopen c::D  # x",
    );
    assert_eq!(check_no_owed(&l).len(), 3);
    assert_eq!(l.owed().len(), 3);
}

/// The committed ledger parses. (It owes no arm today, so `--no-owed` is
/// green; whether a line is owed is `--no-owed`'s question, so this test
/// does not pin either state.)
#[test]
fn todays_ledger_parses() {
    parse_enum_ledger(include_str!("../enum_ledger.txt")).unwrap();
}

// ── Diff check ──────────────────────────────────────────────────────────────

const RED_ONLY: &str = r#"
    #[derive(Serialize, Deserialize)]
    pub enum Color { Red }
"#;
const CLOSED: &str = "closed-consensus c::Color  # every reader must agree";

#[test]
fn removed_variant_is_red_and_a_ratified_line_excuses_it() {
    let red = diff(COLOR, RED_ONLY, CLOSED, CLOSED, "");
    assert!(
        red.len() == 1 && red[0].contains("removed variant `Green`"),
        "{red:?}"
    );
    let ratified = "rust c::Color::Green removed 2026-10-02 (ruled)";
    assert_eq!(
        diff(COLOR, RED_ONLY, CLOSED, CLOSED, ratified),
        Vec::<String>::new()
    );
}

#[test]
fn renamed_wire_name_is_a_removal_unless_an_alias_keeps_it() {
    let renamed = r#"
        #[derive(Serialize, Deserialize)]
        pub enum Color { Red, #[serde(rename = "Verde")] Green }
    "#;
    let red = diff(
        COLOR,
        renamed,
        "skip c::Color  # x",
        "skip c::Color  # x",
        "",
    );
    assert!(
        red.len() == 1 && red[0].contains("removed variant `Green`"),
        "{red:?}"
    );
    let aliased = r#"
        #[derive(Serialize, Deserialize)]
        pub enum Color { Red, #[serde(rename = "Verde", alias = "Green")] Green }
    "#;
    assert_eq!(
        diff(
            COLOR,
            aliased,
            "skip c::Color  # x",
            "skip c::Color  # x",
            ""
        ),
        Vec::<String>::new()
    );
}

#[test]
fn removal_binds_only_where_the_base_ledger_puts_the_enum_in_scope() {
    for out_of_scope in [
        "local c::Color  # x",
        "locked c::Color  # x",
        "foreign c::Color  # x",
        "local c::*  # x",
        "",
    ] {
        assert_eq!(
            diff(COLOR, RED_ONLY, out_of_scope, CLOSED, ""),
            Vec::<String>::new(),
            "base ledger {out_of_scope:?} promised nothing"
        );
    }
    // Flipping the enum out of scope in the same change does not excuse it.
    let red = diff(COLOR, RED_ONLY, CLOSED, "local c::Color  # x", "");
    assert_eq!(red.len(), 1, "{red:?}");
}

#[test]
fn revived_ratified_variant_is_red() {
    let ratified = "rust c::Color::Green removed 2026-10-02 (ruled)";
    let skip = "skip c::Color  # x"; // not closed: the revival is the only finding
    let red = diff(RED_ONLY, COLOR, skip, skip, ratified);
    assert!(
        red.len() == 1 && red[0].contains("revives variant `Green`"),
        "{red:?}"
    );
}

#[test]
fn added_variant_on_a_closed_enum_is_red_unless_its_line_changed() {
    let blue = r#"
        #[derive(Serialize, Deserialize)]
        pub enum Color { Red, Green, Blue }
    "#;
    for ground in [
        "closed-consensus",
        "closed-ladder",
        "closed-extension",
        "closed-fixed",
    ] {
        let line = format!("{ground} c::Color  # reason");
        let red = diff(COLOR, blue, &line, &line, "");
        assert!(
            red.len() == 1 && red[0].contains("added variant `Blue`"),
            "{ground}: {red:?}"
        );
        let changed = format!("{ground} c::Color  # reason; Blue ships behind the v2 stamp");
        assert_eq!(
            diff(COLOR, blue, &line, &changed, ""),
            Vec::<String>::new(),
            "{ground}"
        );
    }
    for passes in [
        "closed-request",
        "skip",
        "open",
        "owed-carry",
        "local",
        "locked",
        "foreign",
    ] {
        let line = format!("{passes} c::Color  # reason");
        assert_eq!(
            diff(COLOR, blue, &line, &line, ""),
            Vec::<String>::new(),
            "{passes}"
        );
    }
}

#[test]
fn adding_an_alias_to_a_closed_enum_is_not_an_addition() {
    let aliased = r#"
        #[derive(Serialize, Deserialize)]
        pub enum Color { Red, #[serde(alias = "Verde")] Green }
    "#;
    assert_eq!(
        diff(COLOR, aliased, CLOSED, CLOSED, ""),
        Vec::<String>::new()
    );
}

// ── ratified-breaks.txt's enum key ──────────────────────────────────────────

#[test]
fn ratified_breaks_enum_key_is_removed_only() {
    parse_ratified_breaks(
        "rust fauna-ipc::sync::RequestMethod::ListFolders removed 2026-10-02 (r)",
    )
    .expect("an enum variant's removal parses");
    for bad in [
        "rust fauna-ipc::sync::RequestMethod::ListFolders optional→required 2026-10-02 (r)",
        "rust Color::Green removed 2026-10-02 (r)", // no crate, so not an enum key
        "rust Color removed 2026-10-02 (r)",
    ] {
        assert!(parse_ratified_breaks(bad).is_err(), "{bad:?}");
    }
}
