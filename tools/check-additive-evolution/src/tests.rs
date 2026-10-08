//! Pure-function unit tests on the parse + diff core — no git, no filesystem.
//! Each blocked change MUST produce a violation; each allowed change MUST be
//! clean. Mirrors the blocked/allowed contract of
//! `scripts/test_check_cddl_evolution.py`, expressed in Rust struct syntax.
//! The end-to-end git merge-base path is exercised in `tests/integration.rs`.

use super::*;

/// Parse one source string at the crate root (empty module prefix).
fn parse(src: &str) -> StructMap {
    let mut map = StructMap::new();
    collect_structs(src, "", &mut map).expect("parse");
    map
}

fn diff(base: &str, head: &str) -> Vec<String> {
    diff_struct_maps(&parse(base), &parse(head))
        .into_iter()
        .map(|v| format!("{}: {}", v.key, v.message))
        .collect()
}

const BASE: &str = r#"
    #[derive(Serialize, Deserialize)]
    pub struct Foo {
        pub id: String,
        pub note: Option<String>,
        #[serde(flatten, default)]
        pub extra: BTreeMap<String, Value>,
    }
"#;

// ── Blocked changes ───────────────────────────────────────────────────────

#[test]
fn removed_field_is_blocked() {
    let head = r#"
        #[derive(Serialize, Deserialize)]
        pub struct Foo {
            pub id: String,
            #[serde(flatten, default)]
            pub extra: BTreeMap<String, Value>,
        }
    "#;
    let errs = diff(BASE, head);
    assert!(
        errs.iter().any(|e| e.contains("removed wire field `note`")),
        "{errs:?}"
    );
}

#[test]
fn optional_to_required_is_blocked() {
    let head = r#"
        #[derive(Serialize, Deserialize)]
        pub struct Foo {
            pub id: String,
            pub note: String,
            #[serde(flatten, default)]
            pub extra: BTreeMap<String, Value>,
        }
    "#;
    let errs = diff(BASE, head);
    assert!(
        errs.iter()
            .any(|e| e.contains("`note` was optional, now required")),
        "{errs:?}"
    );
}

#[test]
fn type_change_is_blocked() {
    let head = r#"
        #[derive(Serialize, Deserialize)]
        pub struct Foo {
            pub id: u64,
            pub note: Option<String>,
            #[serde(flatten, default)]
            pub extra: BTreeMap<String, Value>,
        }
    "#;
    let errs = diff(BASE, head);
    assert!(
        errs.iter().any(|e| e.contains("`id` type changed")),
        "{errs:?}"
    );
}

#[test]
fn boxing_a_field_is_not_a_wire_change() {
    // `Box<T>` is wire-identical to `T`: serde's `Serialize`/`Deserialize` for
    // `Box<T>` delegate straight through, so the canonical encoding is
    // byte-for-byte the same. Boxing a field is the project's established way
    // to shrink a hot `Result` payload without touching the wire
    // (`transport.md` § Wire format → In-memory representation: `RpcError`'s
    // `details`, then `message`), and a syntactic type comparison would block
    // it forever.
    let head = r#"
        #[derive(Serialize, Deserialize)]
        pub struct Foo {
            pub id: Box<String>,
            pub note: Option<String>,
            #[serde(flatten, default)]
            pub extra: BTreeMap<String, Value>,
        }
    "#;
    let errs = diff(BASE, head);
    assert!(
        !errs.iter().any(|e| e.contains("`id` type changed")),
        "{errs:?}"
    );
}

#[test]
fn boxing_under_option_is_not_a_wire_change() {
    let head = r#"
        #[derive(Serialize, Deserialize)]
        pub struct Foo {
            pub id: String,
            pub note: Option<Box<String>>,
            #[serde(flatten, default)]
            pub extra: BTreeMap<String, Value>,
        }
    "#;
    let errs = diff(BASE, head);
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn unboxing_a_field_is_not_a_wire_change_either() {
    // The reverse direction is equally wire-invisible — a later session
    // undoing a boxing must not be blocked by this gate either.
    let base = r#"
        #[derive(Serialize, Deserialize)]
        pub struct Foo {
            pub id: Box<String>,
            pub note: Option<String>,
            #[serde(flatten, default)]
            pub extra: BTreeMap<String, Value>,
        }
    "#;
    let head = r#"
        #[derive(Serialize, Deserialize)]
        pub struct Foo {
            pub id: String,
            pub note: Option<String>,
            #[serde(flatten, default)]
            pub extra: BTreeMap<String, Value>,
        }
    "#;
    assert!(diff(base, head).is_empty(), "{:?}", diff(base, head));
}

#[test]
fn a_retype_under_a_box_is_still_blocked() {
    // Stripping the `Box` must not blind the gate to the type *inside* it.
    let head = r#"
        #[derive(Serialize, Deserialize)]
        pub struct Foo {
            pub id: Box<u64>,
            pub note: Option<String>,
            #[serde(flatten, default)]
            pub extra: BTreeMap<String, Value>,
        }
    "#;
    let errs = diff(BASE, head);
    assert!(
        errs.iter().any(|e| e.contains("`id` type changed")),
        "{errs:?}"
    );
}

#[test]
fn inner_type_change_under_option_is_blocked() {
    // Option<String> -> Option<u64>: optionality unchanged, inner type changed.
    let head = r#"
        #[derive(Serialize, Deserialize)]
        pub struct Foo {
            pub id: String,
            pub note: Option<u64>,
            #[serde(flatten, default)]
            pub extra: BTreeMap<String, Value>,
        }
    "#;
    let errs = diff(BASE, head);
    assert!(
        errs.iter().any(|e| e.contains("`note` type changed")),
        "{errs:?}"
    );
    assert!(
        !errs.iter().any(|e| e.contains("optional, now required")),
        "{errs:?}"
    );
}

#[test]
fn ident_rename_without_serde_rename_is_blocked_as_removal() {
    // Renaming the Rust ident changes the wire name -> remove + add.
    let head = r#"
        #[derive(Serialize, Deserialize)]
        pub struct Foo {
            pub identifier: String,
            pub note: Option<String>,
            #[serde(flatten, default)]
            pub extra: BTreeMap<String, Value>,
        }
    "#;
    let errs = diff(BASE, head);
    assert!(
        errs.iter().any(|e| e.contains("removed wire field `id`")),
        "{errs:?}"
    );
}

#[test]
fn removed_field_from_strict_no_catch_all_struct_is_blocked() {
    // A strict at-rest shape: `#[serde(deny_unknown_fields)]`, per-field
    // `#[serde(default)]`, no flatten catch-all. `diff_struct_maps` doesn't
    // special-case `strict` for field removal, but this pins the shape the
    // gate was widened to cover (libs/fauna-core/src) down as a regression
    // test, since that crate's real structs are not part of this
    // pure-function suite.
    let base = r#"
        #[derive(Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct StrictConfig {
            pub id: String,
            #[serde(default)]
            pub retired_flag: bool,
        }
    "#;
    let head = r#"
        #[derive(Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct StrictConfig {
            pub id: String,
        }
    "#;
    let errs = diff(base, head);
    assert!(
        errs.iter()
            .any(|e| e.contains("removed wire field `retired_flag`")),
        "{errs:?}"
    );
}

#[test]
fn rename_all_flip_is_blocked_as_mass_removal() {
    let base = r#"
        #[derive(Serialize, Deserialize)]
        pub struct Foo { pub user_id: String }
    "#;
    let head = r#"
        #[derive(Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct Foo { pub user_id: String }
    "#;
    let errs = diff(base, head);
    // base wire name `user_id` is gone (head emits `userId`).
    assert!(
        errs.iter()
            .any(|e| e.contains("removed wire field `user_id`")),
        "{errs:?}"
    );
}

// ── Allowed changes ───────────────────────────────────────────────────────

#[test]
fn new_optional_field_is_allowed() {
    let head = r#"
        #[derive(Serialize, Deserialize)]
        pub struct Foo {
            pub id: String,
            pub note: Option<String>,
            pub added: Option<u64>,
            #[serde(flatten, default)]
            pub extra: BTreeMap<String, Value>,
        }
    "#;
    assert!(diff(BASE, head).is_empty(), "{:?}", diff(BASE, head));
}

#[test]
fn required_to_optional_is_allowed() {
    // The looser direction, like the CDDL gate: not flagged.
    let head = r#"
        #[derive(Serialize, Deserialize)]
        pub struct Foo {
            pub id: Option<String>,
            pub note: Option<String>,
            #[serde(flatten, default)]
            pub extra: BTreeMap<String, Value>,
        }
    "#;
    assert!(diff(BASE, head).is_empty(), "{:?}", diff(BASE, head));
}

#[test]
fn ident_rename_preserving_wire_name_is_allowed() {
    // serde(rename) keeps the wire key stable while the Rust ident changes.
    let base = r#"
        #[derive(Serialize, Deserialize)]
        pub struct Foo { pub id: String }
    "#;
    let head = r#"
        #[derive(Serialize, Deserialize)]
        pub struct Foo { #[serde(rename = "id")] pub identifier: String }
    "#;
    assert!(diff(base, head).is_empty(), "{:?}", diff(base, head));
}

#[test]
fn removed_struct_is_not_flagged() {
    // A whole struct vanishing (rename/move/v2) is not a field-level violation.
    let base = format!("{BASE}\n#[derive(Serialize, Deserialize)] pub struct Bar {{ pub x: u8 }}");
    assert!(diff(&base, BASE).is_empty(), "{:?}", diff(&base, BASE));
}

#[test]
fn new_field_on_struct_without_catch_all_is_allowed() {
    // Adding an optional field to an existing struct that lacks a catch-all
    // is still additive — the catch-all check only fires on *new* structs.
    let base = r#"#[derive(Serialize, Deserialize)] pub struct Foo { pub id: String }"#;
    let head = r#"#[derive(Serialize, Deserialize)] pub struct Foo { pub id: String, pub added: Option<u64> }"#;
    assert!(diff(base, head).is_empty(), "{:?}", diff(base, head));
}

#[test]
fn non_serde_struct_is_ignored() {
    let base = r#"pub struct Foo { pub id: String }"#;
    let head = r#"pub struct Foo {}"#;
    assert!(parse(base).is_empty());
    assert!(diff(base, head).is_empty());
}

#[test]
fn skip_field_is_off_wire() {
    let base = r#"
        #[derive(Serialize, Deserialize)]
        pub struct Foo { pub id: String, #[serde(skip)] pub cache: Vec<u8> }
    "#;
    let head = r#"
        #[derive(Serialize, Deserialize)]
        pub struct Foo { pub id: String }
    "#;
    // Dropping a skip(ped) field is not a wire change.
    assert!(diff(base, head).is_empty(), "{:?}", diff(base, head));
}

// ── Catch-all state check ──────────────────────────────────────────────────
//
// This used to be a diff-against-base "new struct" check, which could only
// ever see a violation on the one commit that introduced it — a struct
// already older than the merge-base was permanently invisible (the measured
// miss: `push_events::StaleSurfaces`). It is now a STATE check over the whole head tree against an
// explicit baseline, so a violation stays visible until it's actually fixed.

fn catch_all_errs(head: &str, baseline: &[&str]) -> Vec<String> {
    let baseline: std::collections::BTreeSet<String> =
        baseline.iter().map(|s| s.to_string()).collect();
    check_catch_all_violations(&parse(head), &baseline)
        .into_iter()
        .map(|v| v.key)
        .collect()
}

#[test]
fn non_strict_struct_without_catch_all_is_flagged() {
    let head = r#"#[derive(Serialize, Deserialize)] pub struct NewReq { pub a: String }"#;
    assert_eq!(catch_all_errs(head, &[]), vec!["NewReq".to_string()]);
}

#[test]
fn struct_with_catch_all_is_clean() {
    let head = r#"
        #[derive(Serialize, Deserialize)]
        pub struct NewReq { pub a: String, #[serde(flatten, default)] pub extra: BTreeMap<String, Value> }
    "#;
    assert!(catch_all_errs(head, &[]).is_empty());
}

#[test]
fn strict_struct_is_exempt() {
    let head = r#"
        #[derive(Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct NewReq { pub a: String }
    "#;
    assert!(catch_all_errs(head, &[]).is_empty());
}

#[test]
fn empty_marker_struct_is_exempt() {
    let head = r#"#[derive(Serialize, Deserialize)] pub struct Ping {}"#;
    assert!(catch_all_errs(head, &[]).is_empty());
}

#[test]
fn baseline_membership_grandfathers_a_struct() {
    let head = r#"#[derive(Serialize, Deserialize)] pub struct Old { pub a: String }"#;
    assert!(catch_all_errs(head, &["Old"]).is_empty());
}

#[test]
fn a_struct_not_new_but_not_baselined_is_still_flagged() {
    // The property the diff-based form structurally lacked: a struct that
    // "was already there" (nothing about this test's `head` distinguishes
    // new from pre-existing — there IS no base map any more) is still
    // reported unless its key is explicitly in the baseline.
    let head = r#"#[derive(Serialize, Deserialize)] pub struct Old { pub a: String }"#;
    assert_eq!(catch_all_errs(head, &[]), vec!["Old".to_string()]);
    // A DIFFERENT struct in the baseline does not grandfather this one.
    assert_eq!(catch_all_errs(head, &["Other"]), vec!["Old".to_string()]);
}

#[test]
fn serialize_only_struct_is_skipped_regardless_of_baseline() {
    // `push_events::StaleSurfaces`'s shape: derives Serialize but not
    // Deserialize, so it has no decode side and neither catch-all annotation
    // is anything but a cosmetic no-op on it.
    let head = r#"#[derive(Serialize)] pub struct StaleSurfaces { pub a: String }"#;
    assert!(catch_all_errs(head, &[]).is_empty());
}

#[test]
fn domain_extra_without_flatten_is_not_a_catch_all() {
    // bridges_ui's `extra: Option<Value>` (no flatten) is a real domain field,
    // so a struct carrying only it still needs the real catch-all.
    let head = r#"
        #[derive(Serialize, Deserialize)]
        pub struct NewFollow { pub id: String, pub extra: Option<Value> }
    "#;
    assert_eq!(catch_all_errs(head, &[]), vec!["NewFollow".to_string()]);
}

// ── Baseline parsing ────────────────────────────────────────────────────────

#[test]
fn baseline_parsing_skips_blank_and_comment_lines() {
    let text = "# header comment\n\nfoo::Bar\n  \nbaz::Qux\n# trailing\n";
    let parsed = parse_catch_all_baseline(text);
    assert_eq!(
        parsed,
        ["foo::Bar", "baz::Qux"]
            .into_iter()
            .map(str::to_string)
            .collect()
    );
}

// ── rename_all transforms ─────────────────────────────────────────────────

#[test]
fn rename_all_rules() {
    assert_eq!(
        apply_rename_all(Some(RenameRule::Camel), "user_id"),
        "userId"
    );
    assert_eq!(
        apply_rename_all(Some(RenameRule::Pascal), "user_id"),
        "UserId"
    );
    assert_eq!(
        apply_rename_all(Some(RenameRule::Snake), "user_id"),
        "user_id"
    );
    assert_eq!(
        apply_rename_all(Some(RenameRule::ScreamingSnake), "user_id"),
        "USER_ID"
    );
    assert_eq!(
        apply_rename_all(Some(RenameRule::Kebab), "user_id"),
        "user-id"
    );
    assert_eq!(
        apply_rename_all(Some(RenameRule::Lower), "user_id"),
        "userid"
    );
    assert_eq!(apply_rename_all(None, "user_id"), "user_id");
}

// ── module qualification ──────────────────────────────────────────────────

#[test]
fn structs_are_module_qualified() {
    // Same struct name in two inline modules must not collide.
    let src = r#"
        pub mod a { #[derive(Serialize, Deserialize)] pub struct Dup { pub x: String } }
        pub mod b { #[derive(Serialize, Deserialize)] pub struct Dup { pub y: u64 } }
    "#;
    let map = parse(src);
    assert!(
        map.contains_key("a::Dup"),
        "{:?}",
        map.keys().collect::<Vec<_>>()
    );
    assert!(
        map.contains_key("b::Dup"),
        "{:?}",
        map.keys().collect::<Vec<_>>()
    );
}

#[test]
fn cfg_test_modules_are_skipped() {
    let src = r#"
        #[derive(Serialize, Deserialize)] pub struct Real { pub x: String }
        #[cfg(test)]
        mod tests { #[derive(Serialize, Deserialize)] pub struct Fake { pub y: u64 } }
    "#;
    let map = parse(src);
    assert!(map.contains_key("Real"));
    assert!(
        !map.keys().any(|k| k.contains("Fake")),
        "{:?}",
        map.keys().collect::<Vec<_>>()
    );
}

#[test]
fn only_exact_cfg_test_is_skipped() {
    let src = r#"
        #[cfg(feature = "test-hooks")]
        #[derive(Serialize, Deserialize)] pub struct Hooked { pub a: String }
        #[cfg(not(test))]
        #[derive(Serialize, Deserialize)] pub struct NotTest { pub b: String }
        #[cfg(test)]
        #[derive(Serialize, Deserialize)] pub struct Plain { pub c: String }
        #[cfg(all(unix, test))]
        #[derive(Serialize, Deserialize)] pub struct AllTest { pub d: String }
    "#;
    let keys: Vec<String> = parse(src).into_keys().collect();
    assert_eq!(keys, ["Hooked", "NotTest"]);
}

#[test]
fn out_of_line_cfg_test_module_files_are_not_scanned() {
    let mut scan = StructScan::default();
    scan.add_file(
        "#[cfg(test)] mod fixtures; mod real;\n\
         #[derive(Serialize, Deserialize)] pub struct Top { pub a: String }",
        "",
    )
    .expect("parse");
    let wire = "#[derive(Serialize, Deserialize)] pub struct Inner { pub x: String }";
    scan.add_file(wire, "fixtures").expect("parse");
    scan.add_file(wire, "real").expect("parse");
    let keys: Vec<String> = scan.finish().into_keys().collect();
    assert_eq!(keys, ["Top", "real::Inner"]);
}

#[test]
fn path_qualified_and_cfg_attr_derives_are_tracked() {
    let src = r#"
        #[derive(serde::Deserialize)] pub struct De { pub a: String }
        #[derive(serde::Serialize)] pub struct Ser { pub b: String }
        #[cfg_attr(feature = "x", derive(Debug, serde::Serialize, serde::Deserialize))]
        pub struct Gated { pub c: String }
    "#;
    let map = parse(src);
    assert!(map["De"].derives_deserialize);
    assert!(!map["Ser"].derives_deserialize);
    assert!(map["Gated"].derives_deserialize);
    // A decodable path-qualified struct without the catch-all is now a finding.
    let found = check_catch_all_violations(&map, &BTreeSet::new());
    let keys: Vec<&str> = found.iter().map(|v| v.key.as_str()).collect();
    assert_eq!(keys, ["De", "Gated"]);
}

// ── Ratified in-place breaks (libs/fauna-protocol/schemas/ratified-breaks.txt) ──

fn ratified(entries: &[(&str, Transition)]) -> RatifiedBreaks {
    entries
        .iter()
        .map(|(k, t)| (k.to_string(), t.clone()))
        .collect()
}

fn diff_allowing(base: &str, head: &str, allow: &RatifiedBreaks) -> Vec<String> {
    diff_struct_maps_allowing(&parse(base), &parse(head), allow)
        .into_iter()
        .map(|v| format!("{}: {}", v.key, v.message))
        .collect()
}

#[test]
fn parse_ratified_breaks_takes_only_rust_keys_with_their_transition() {
    let text = "# comment\n\
                rust auth::CertBinding.sig   removed 2026-09-24 untagged half  # trailing\n\
                cddl EventRsvpPayload        removed 2026-09-24 dead kind\n\
                \n\
                rust nat_mode::NatModeRequest.nest_id optional→required 2026-09-24 nest-bound\n";
    assert_eq!(
        parse_ratified_breaks(text).expect("well-formed"),
        ratified(&[
            ("auth::CertBinding.sig", Transition::Removed),
            (
                "nat_mode::NatModeRequest.nest_id",
                Transition::OptionalToRequired
            ),
        ])
    );
}

#[test]
fn parse_ratified_breaks_refuses_a_malformed_entry() {
    for line in [
        "rust auth::CertBinding.sig 2026-09-24 the pre-transition grammar",
        "rust auth::CertBinding.sig retyped 2026-09-24 a retype names its target type",
        "rust auth::CertBinding.sig retyped→ 2026-09-24 an empty target type",
        "rust auth::CertBinding optional→required 2026-09-24 no wire field",
        "rust auth::CertBinding.sig removed",
        "rust",
        // A 4-token line — gate, key, transition, ratified-on, but no
        // ratification text — used to slip past the old `>= 4`-element slice
        // pattern. The grammar's fifth field is not optional.
        "rust auth::CertBinding.sig removed 2026-09-24",
    ] {
        assert!(parse_ratified_breaks(line).is_err(), "accepted {line:?}");
    }
}

/// An unrecognized gate token used to be silently skipped by BOTH parsers
/// (`parts.first() != Some(&"rust")` treated `cdl`/`rsut` exactly like a
/// `cddl` line — ignored, not validated), which meant a mistyped gate token
/// on a `removed` entry silently switched off that key's revival refusal. It
/// is now a parse error, same as any other malformed line.
#[test]
fn parse_ratified_breaks_refuses_an_unknown_gate_token() {
    for line in [
        "cdl auth::CertBinding.sig removed 2026-09-24 typo'd gate",
        "rsut auth::CertBinding.sig removed 2026-09-24 typo'd gate",
        "RUST auth::CertBinding.sig removed 2026-09-24 wrong case",
    ] {
        assert!(parse_ratified_breaks(line).is_err(), "accepted {line:?}");
    }
}

#[test]
fn parse_ratified_breaks_skips_cddl_lines_without_validating_them() {
    // A `cddl` line malformed by this gate's OWN grammar (missing
    // ratification) is still skipped here — it belongs to the CDDL gate's
    // parser, which validates it instead (mirrored in
    // `test_check_cddl_evolution.py`).
    assert_eq!(
        parse_ratified_breaks("cddl Foo.note removed 2026-09-24\n").unwrap(),
        RatifiedBreaks::new()
    );
}

/// The pin: an `optional→required` ratification of
/// `nat_mode::NatModeRequest.nest_id` — the nest-binding half of the
/// admin-signed NAT-mode commit — excuses that tightening and never a later
/// removal or retype of the field.
#[test]
fn optional_to_required_entry_does_not_excuse_a_later_removal() {
    const NAT_BASE: &str = r#"
        pub mod nat_mode {
            #[derive(Serialize, Deserialize)]
            pub struct NatModeRequest {
                pub mode: String,
                pub nest_id: String,
                #[serde(flatten, default)]
                pub extra: BTreeMap<String, Value>,
            }
        }
    "#;
    let allow = ratified(&[(
        "nat_mode::NatModeRequest.nest_id",
        Transition::OptionalToRequired,
    )]);

    let tightened_from = NAT_BASE.replace("pub nest_id: String", "pub nest_id: Option<String>");
    assert_eq!(
        diff_allowing(&tightened_from, NAT_BASE, &allow),
        Vec::<String>::new()
    );

    let removed = NAT_BASE.replace("pub nest_id: String,", "");
    let errs = diff_allowing(NAT_BASE, &removed, &allow);
    assert!(
        errs.iter()
            .any(|e| e.contains("removed wire field `nest_id`")),
        "{errs:?}"
    );

    let retyped = NAT_BASE.replace("pub nest_id: String", "pub nest_id: u64");
    let errs = diff_allowing(NAT_BASE, &retyped, &allow);
    assert!(errs.iter().any(|e| e.contains("type changed")), "{errs:?}");
}

#[test]
fn removed_entry_does_not_excuse_a_tightening() {
    let head = BASE.replace("pub note: Option<String>", "pub note: String");
    let allow = ratified(&[("Foo.note", Transition::Removed)]);
    let errs = diff_allowing(BASE, &head, &allow);
    assert!(
        errs.iter()
            .any(|e| e.contains("was optional, now required")),
        "{errs:?}"
    );
}

/// A removed name never comes back: a head carrying a field the list
/// records as `removed` is refused, even on a struct new in this change.
#[test]
fn a_ratified_removed_field_never_comes_back() {
    let allow = ratified(&[("Foo.note", Transition::Removed)]);
    let errs = diff_allowing("", BASE, &allow);
    assert!(
        errs.iter()
            .any(|e| e.contains("revives") && e.contains("`note`")),
        "{errs:?}"
    );
}

#[test]
fn ratified_break_is_excused_and_nothing_else_is() {
    let head = r#"
        #[derive(Serialize, Deserialize)]
        pub struct Foo {
            pub note: String,
            #[serde(flatten, default)]
            pub extra: BTreeMap<String, Value>,
        }
    "#;
    // Unlisted: both breaks (id removed, note tightened) are reported.
    let errs = diff(BASE, head);
    assert_eq!(errs.len(), 2, "{errs:?}");

    let only_note = ratified(&[("Foo.note", Transition::OptionalToRequired)]);
    let errs = diff_allowing(BASE, head, &only_note);
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(errs[0].contains("removed wire field `id`"), "{errs:?}");

    // The key is module-qualified: a same-named struct elsewhere is not excused.
    let wrong_module = ratified(&[("other::Foo.note", Transition::OptionalToRequired)]);
    let errs = diff_allowing(BASE, head, &wrong_module);
    assert_eq!(errs.len(), 2, "{errs:?}");
}

#[test]
fn parse_ratified_breaks_reads_a_retype_with_its_target_type() {
    let text = "rust data::MailCredential.secret retyped→SecretByteBuf 2026-09-24 byte string\n";
    assert_eq!(
        parse_ratified_breaks(text).expect("well-formed"),
        ratified(&[(
            "data::MailCredential.secret",
            Transition::Retyped("SecretByteBuf".to_string())
        )])
    );
}

#[test]
fn ratified_retype_excuses_only_the_retype_to_its_named_type() {
    let base = r#"
        #[derive(Serialize, Deserialize)]
        pub struct Cred {
            pub secret: SecretBytes,
        }
    "#;
    let to = |ty: &str| {
        format!(
            "#[derive(Serialize, Deserialize)]\npub struct Cred {{\n    pub secret: {ty},\n}}\n"
        )
    };
    let allow = ratified(&[(
        "Cred.secret",
        Transition::Retyped("SecretByteBuf".to_string()),
    )]);
    assert!(diff_allowing(base, &to("SecretByteBuf"), &allow).is_empty());
    // A retype to any other type is still a finding.
    let errs = diff_allowing(base, &to("Vec<u8>"), &allow);
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(errs[0].contains("type changed"), "{errs:?}");
    // A `removed` or `optional→required` entry never excuses a retype.
    for t in [Transition::Removed, Transition::OptionalToRequired] {
        let errs = diff_allowing(base, &to("SecretByteBuf"), &ratified(&[("Cred.secret", t)]));
        assert!(errs.iter().any(|e| e.contains("type changed")), "{errs:?}");
    }
}

// ── The allowlist's own monotonic invariant ─────────────────────────────────
//
// "The list only grows" was, before this, enforced only as a side effect of
// the revival check above: deleting an entry and never reviving the name
// slipped past both gates entirely, since neither read the list from the
// merge base. `check_ratified_breaks_monotonic` closes that: it diffs the
// list itself, base against head, independently of what the struct diff
// finds.

#[test]
fn dropping_a_base_entry_is_flagged_even_without_a_revival() {
    let base = ratified(&[("auth::CertBinding.sig", Transition::Removed)]);
    let head = RatifiedBreaks::new();
    let errs = check_ratified_breaks_monotonic(&base, &head);
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(
        errs[0].message.contains("auth::CertBinding.sig"),
        "{errs:?}"
    );
    assert!(errs[0].message.contains("missing from HEAD"), "{errs:?}");
    assert!(errs[0].message.contains("only grows"), "{errs:?}");
}

#[test]
fn an_unchanged_list_is_clean() {
    let list = ratified(&[("auth::CertBinding.sig", Transition::Removed)]);
    assert!(check_ratified_breaks_monotonic(&list, &list).is_empty());
}

#[test]
fn a_list_that_only_grows_is_clean() {
    let base = ratified(&[("auth::CertBinding.sig", Transition::Removed)]);
    let head = ratified(&[
        ("auth::CertBinding.sig", Transition::Removed),
        (
            "nat_mode::NatModeRequest.nest_id",
            Transition::OptionalToRequired,
        ),
    ]);
    assert!(check_ratified_breaks_monotonic(&base, &head).is_empty());
}

/// The named attack: deleting a `removed` entry AND reviving the field in
/// the same change. Before this fix, `diff_struct_maps_allowing`'s revival
/// check alone read the list from HEAD only, where the deleted entry is
/// already gone — so it found nothing. The monotonic check catches the
/// deletion on its own, so the combined attack is caught even though the
/// revival-only check still sees a clean list.
#[test]
fn delete_and_revive_is_caught_by_the_monotonic_check_even_though_revival_alone_is_not() {
    let allow_with_entry = ratified(&[("Foo.note", Transition::Removed)]);
    let allow_without_entry = RatifiedBreaks::new();

    // Revival check alone, against the (already-shrunk) HEAD list: no
    // violation — this is exactly the gap the finding names.
    let head = r#"
        #[derive(Serialize, Deserialize)]
        pub struct Foo {
            pub note: String,
            #[serde(flatten, default)]
            pub extra: BTreeMap<String, Value>,
        }
    "#;
    let errs = diff_allowing("", head, &allow_without_entry);
    assert!(
        !errs.iter().any(|e| e.contains("revives")),
        "revival check alone should miss this: {errs:?}"
    );

    // The monotonic check, run alongside it as `main` now does, catches the
    // dropped entry regardless.
    let monotonic_errs = check_ratified_breaks_monotonic(&allow_with_entry, &allow_without_entry);
    assert_eq!(monotonic_errs.len(), 1, "{monotonic_errs:?}");
}

/// The product version is read from `[workspace.package]` only — a member
/// crate's own `version` key elsewhere in the file is not it.
#[test]
fn product_version_reads_the_workspace_package_section_only() {
    let toml = "[package]\nversion = \"9.9.9\"\n\n[workspace.package]\nedition = \"2024\"\nversion = \"0.1.3\"\n\n[workspace.dependencies]\nversion = \"7\"\n";
    assert_eq!(product_version(toml).as_deref(), Some("0.1.3"));
    assert_eq!(product_version("[package]\nversion = \"1.0.0\"\n"), None);
}

/// Every 0.1.x is inside the compat-free window; 0.2.0 and every later
/// version is outside it (version-compatibility.md § Dimension 2, the fifth
/// ratified exception).
#[test]
fn the_compat_free_window_is_exactly_0_1_x() {
    for v in ["0.1.0", "0.1.3", "0.1.99"] {
        assert!(in_compat_free_window(v), "{v}");
    }
    for v in ["0.2.0", "0.10.0", "1.1.0", "0.0.9", "1.0.0"] {
        assert!(!in_compat_free_window(v), "{v}");
    }
}
