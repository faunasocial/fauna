//! Cross-language Rust↔Go conformance guard for the **DAV collection / resource
//! identity contract** — the derivations a Fauna app and the Go mail-bridge MDA
//! must perform identically to land on the *same* `bridge_caldav_*` /
//! `bridge_carddav_*` row.
//!
//! # Why this exists
//!
//! A CalDAV/CardDAV collection has two independent writers: a Fauna app over the
//! encrypted `fauna.bridges.*` RPCs, and a stock DAV client (Apple Calendar /
//! Contacts, DAVx5, Thunderbird) talking to the MDA. Neither is handed a
//! server-assigned key — both *derive* every identifier. So a one-character edit
//! on either side (a rebranded default colour, a renamed slug, a salted hash)
//! silently forks one logical calendar / address book / event into two rows,
//! and nothing in either language's own test suite can see it: each side stays
//! perfectly self-consistent.
//!
//! Until this file existed the contract was held by four doc comments asking a
//! future human to "keep these two in sync" — one of which had already gone
//! stale about the seam available to it. A mechanism does not get to rest on a
//! human promise.
//!
//! # What it pins, and how
//!
//! `libs/fauna-protocol/src/dav_identity.rs` is the Rust owner. This test reads
//! the MDA's **production Go source** and asserts that every derivation there
//! still matches that owner. Reading source rather than a generated fixture is
//! deliberate for this contract: these are a handful of literals, and a fixture
//! would only catch Go drift on the day someone remembered to regenerate it —
//! whereas the Go source is the artifact that actually ships, and reading it
//! makes *any* edit on either side fail this gated `cargo test` immediately,
//! with no Go toolchain and no regen step.
//!
//! **A pattern that cannot be found is RED, never a silent skip.** Every
//! extractor below panics on absence, so a Go refactor that moves a constant
//! fails here and asks a human to re-point the guard — the opposite of a
//! vacuous assert that quietly stops checking anything. The last three tests in
//! this file prove those failure paths actually fire.
//!
//! # Scope boundary
//!
//! This is a *source-level* agreement guard: it proves both sides derive from
//! the same inputs with the same unsalted primitive. The complementary
//! **runtime** witness that the two writers really do collide on one row is the
//! tier_3 e2e pair `tests/e2e-unified/tests/test_events.py` and
//! `test_addressbook.py`, which drive the app and the DAV surface against one
//! nest.
//!
//! Coordination boundary: a genuine drift surfaced here is a finding for
//! whoever owns the edit, not licence to "fix" the other side to match — the
//! goal docs (`caldav-server.md` § Lazy "Personal" calendar, `carddav-server.md`
//! § Write surface) own the values, and they arbitrate.

use fauna_protocol::dav_identity::{
    CONTACTS_ADDRESSBOOK_SLUG, DEFAULT_ADDRESSBOOK_DISPLAYNAME, DEFAULT_CALENDAR_COLOR,
    DEFAULT_CALENDAR_DISPLAYNAME, PERSONAL_CALENDAR_SLUG,
};
use std::path::PathBuf;

/// Repo-relative paths to the MDA's production Go sources (never `_test.go` —
/// the contract lives in what ships).
const CALDAV_BACKEND: &str = "bins/fauna-bridges/internal/mda/caldav/backend.go";
const CALDAV_PUT: &str = "bins/fauna-bridges/internal/mda/caldav/put.go";
const CARDDAV_BACKEND: &str = "bins/fauna-bridges/internal/mda/carddav/backend.go";
const CARDDAV_PUT: &str = "bins/fauna-bridges/internal/mda/carddav/put.go";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Read a Go source file, or fail loudly. A moved/renamed file is a RED that
/// asks for the guard to be re-pointed — it is never allowed to pass silently.
fn go_source(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "DAV identity guard: cannot read the MDA's production Go source {rel}: {e}\n\
             If the MDA moved this file, re-point the constant in \
             libs/fauna-protocol/tests/dav_identity_cross_language.rs — do NOT delete the check."
        )
    })
}

/// Collapse runs of whitespace so the scanners are insensitive to gofmt
/// alignment without becoming insensitive to the values themselves.
fn normalized(src: &str) -> String {
    src.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Extract the string literal from a top-level `const <name> = "<value>"`.
///
/// Fails loudly when the declaration is absent — including the case where a
/// future refactor folds it into a grouped `const ( … )` block, which this
/// deliberately does not match: an unreadable declaration must stop the guard
/// with a RED rather than let it drift into checking nothing.
fn go_string_const(rel: &str, name: &str) -> String {
    let src = normalized(&go_source(rel));
    let needle = format!("const {name} = \"");
    let start = src.find(&needle).unwrap_or_else(|| {
        panic!(
            "DAV identity guard: no top-level `const {name} = \"…\"` in {rel}.\n\
             Either the MDA renamed/regrouped it (re-point this guard) or it was deleted \
             (the Rust owner libs/fauna-protocol/src/dav_identity.rs and the goal docs \
             must then be reconciled too)."
        )
    }) + needle.len();
    let rest = &src[start..];
    let end = rest.find('"').unwrap_or_else(|| {
        panic!("DAV identity guard: unterminated string literal for `{name}` in {rel}")
    });
    rest[..end].to_string()
}

/// Assert the Go source contains `needle`, with an explanation of the contract
/// it carries. Absence is RED.
fn assert_go_contains(rel: &str, needle: &str, contract: &str) {
    let src = normalized(&go_source(rel));
    assert!(
        src.contains(&normalized(needle)),
        "DAV identity guard: {rel} no longer contains `{needle}`.\n\
         Contract: {contract}\n\
         Rust owner: libs/fauna-protocol/src/dav_identity.rs"
    );
}

// ── The lazy `Personal` calendar (caldav-server.md § Lazy "Personal" calendar) ──

#[test]
fn go_derives_the_personal_calendar_id_from_the_same_slug() {
    assert_go_contains(
        CALDAV_BACKEND,
        &format!("blake3.Sum256([]byte(\"{PERSONAL_CALENDAR_SLUG}\"))"),
        "the lazy Personal calendar's id is blake3 of the slug the Rust owner names \
         (dav_identity::PERSONAL_CALENDAR_SLUG). A different slug here forks the calendar \
         a Fauna app provisions from the one a CalDAV MUA's first PROPFIND triggers.",
    );
}

#[test]
fn go_and_rust_agree_on_the_personal_calendar_display_name() {
    assert_eq!(
        go_string_const(CALDAV_BACKEND, "defaultDisplayname"),
        DEFAULT_CALENDAR_DISPLAYNAME,
        "the MDA and a Fauna app seal DIFFERENT default metadata for the same calendar id — \
         whichever provisions first wins and the other's name is silently discarded \
         (caldav-server.md § Lazy \"Personal\" calendar)"
    );
}

#[test]
fn go_and_rust_agree_on_the_personal_calendar_colour() {
    assert_eq!(
        go_string_const(CALDAV_BACKEND, "defaultColor"),
        DEFAULT_CALENDAR_COLOR,
        "the MDA and a Fauna app seal DIFFERENT default metadata for the same calendar id \
         (caldav-server.md § Lazy \"Personal\" calendar)"
    );
}

// ── The lazy `Contacts` address book (carddav-server.md § Write surface) ──

#[test]
fn go_derives_the_contacts_addressbook_id_from_the_same_slug() {
    assert_go_contains(
        CARDDAV_BACKEND,
        &format!("blake3.Sum256([]byte(\"{CONTACTS_ADDRESSBOOK_SLUG}\"))"),
        "the lazy Contacts book's id is blake3 of the slug the Rust owner names. The goal doc \
         relies on this id being STABLE across re-provisioning (a collection DELETE clears \
         tombstones precisely because the book comes back with the same id).",
    );
}

#[test]
fn go_and_rust_agree_on_the_contacts_addressbook_display_name() {
    assert_eq!(
        go_string_const(CARDDAV_BACKEND, "defaultDisplayname"),
        DEFAULT_ADDRESSBOOK_DISPLAYNAME,
        "the MDA and a Fauna app would seal different default metadata for the lazy \
         Contacts book (carddav-server.md § Write surface)"
    );
}

// ── `uid_hash` — one rule across both rails ──

/// Both DAV rails derive the resource dedup key as a **bare, unsalted**
/// `blake3(UID)` over the plaintext UID parsed out of the body. Rust's
/// `dav_identity::uid_hash` is the same primitive; a salt or a domain separator
/// appearing on either side would fork every event/card row.
#[test]
fn go_derives_uid_hash_as_a_bare_blake3_of_the_plaintext_uid_on_both_rails() {
    for (rel, rail) in [(CALDAV_PUT, "iCalendar VEVENT"), (CARDDAV_PUT, "vCard")] {
        assert_go_contains(
            rel,
            "blake3.Sum256([]byte(uid))",
            &format!(
                "the {rail} rail's uid_hash must stay a bare blake3 over the parsed plaintext \
                 UID, matching dav_identity::uid_hash. Any salt, prefix or truncation here \
                 forks the row a Fauna app writes from the row a DAV client PUTs for the \
                 same UID."
            ),
        );
    }
}

/// The MDA hands nest a **32-byte** id, which is the whole blake3-256 digest —
/// the goal docs' `[:32]` is not a truncation. If Go ever started slicing
/// shorter, the ids would stop matching Rust's `[u8; 32]`.
#[test]
fn go_carries_the_full_32_byte_digest_on_both_rails() {
    for rel in [CALDAV_BACKEND, CARDDAV_BACKEND, CALDAV_PUT, CARDDAV_PUT] {
        assert_go_contains(
            rel,
            "make([]byte, 32)",
            "the derived id is the FULL blake3-256 digest; the goal docs' `[:32]` describes \
             the whole hash, not a truncation of a wider one",
        );
    }
}

// ── The collection-id rule the segment resolvers share ──

/// `caldav-server.md` § Collections: a non-hex URL segment "maps
/// deterministically to `calendar_id = blake3(segment)[:32]`. The mapping is one
/// source of truth, shared by every path parser." Rust exposes that rule as
/// `dav_identity::collection_id`; this pins both MDA resolvers to it, so the
/// slug a stock client picks for a `MKCALENDAR`/`MKCOL` resolves the same way a
/// Fauna app would resolve it.
#[test]
fn go_segment_resolvers_use_the_shared_collection_id_rule() {
    for (rel, what) in [
        (CALDAV_BACKEND, "calendar"),
        (CARDDAV_BACKEND, "addressbook"),
    ] {
        assert_go_contains(
            rel,
            "blake3.Sum256([]byte(segment))",
            &format!(
                "the {what} segment resolver must derive ids with the same unsalted \
                 blake3 rule as dav_identity::collection_id"
            ),
        );
    }
}

// ── The guard's own integrity ──

/// The extractors must be able to *fail*. These three prove the "not found"
/// paths panic rather than silently passing — the vacuous-assert class that let
/// an earlier census pin check nothing at all.
#[test]
#[should_panic(expected = "no top-level `const")]
fn a_missing_go_constant_is_red_not_a_silent_skip() {
    let _ = go_string_const(CALDAV_BACKEND, "aConstantThatDoesNotExist");
}

#[test]
#[should_panic(expected = "cannot read the MDA's production Go source")]
fn a_moved_go_source_file_is_red_not_a_silent_skip() {
    let _ = go_source("bins/fauna-bridges/internal/mda/caldav/no_such_file.go");
}

#[test]
#[should_panic(expected = "no longer contains")]
fn a_vanished_go_derivation_is_red_not_a_silent_skip() {
    assert_go_contains(
        CALDAV_BACKEND,
        "blake3.Sum256([]byte(\"nonexistent\"))",
        "self-test",
    );
}
