//! The Rust half of the **policy-value** wire-vocabulary contract with the Go
//! mail bridge.
//!
//! # Why a second fixture rather than a second group in the first
//!
//! `libs/fauna-mail/tests/go_wire_outcome_contract.rs` pins the *reply
//! discriminators* nest produces and the bridge switches on, deriving them from
//! owners that live in `fauna-mail`. This file pins the *policy values* nest
//! sends on `fetch_config` — a different class, and one whose owners live here,
//! because the same tokens are also what three admin-mail pickers write.
//!
//! They could not share a fixture. `fauna-client-mail-settings` depends on
//! `fauna-mail`, so the other file cannot see these owners without a
//! dev-dependency cycle, and **one fixture with two writers is worse than two
//! fixtures with one writer each**: whichever test ran last would decide the
//! file's contents, and each would report the other's groups as stale. So each
//! owning crate writes its own fixture, and the Go half reads both — its family
//! table names which file each family comes from.
//!
//! # What it pins, and why this one is wider than the mail vocabularies
//!
//! `fcrdns_mode` crosses more boundaries than any discriminator does. It is:
//!
//! * a wire value nest sends the Go MTA, which `internal/mta.ParseFCrDNSMode`
//!   switches on to decide whether a failing forward-confirmed-rDNS check is
//!   ignored, scored, or **rejects the connection** (`550 5.7.25`);
//! * a stored admin policy, so the *same* tokens are the values the
//!   `admin-mail-fcrdns-mode-select` picker writes back;
//! * position-addressed in two of the three apps that render it (a GTK
//!   `DropDown` index, a tui select index), so the ORDER is part of the
//!   contract too — that half is pinned by the owner's own
//!   `picker_vocabulary_tests`, next to the fallback rule.
//!
//! Before 2026-08-23 it was hand-written five times: tui's `FCRDNS_OPTIONS`,
//! linux's `FCRDNS_MODES` **plus** its own `fcrdns_index` fallback, web's three
//! `<option value=…>` literals, and Go's `ParseFCrDNSMode` — which restated the
//! unknown-token fallback as well, a safety decision rather than a rendering
//! detail.
//!
//! # Exhaustiveness is compiler-enforced — AND generated from one list, not two
//!
//! `fcrdns_mode_tokens()` calls [`variant_set!`], which both builds the
//! instance list and derives the exhaustive `match` from that SAME token
//! list. The sibling `fauna-core` contract had a hand-written `all` array
//! next to a hand-written `match`, which let a minimal compile-fix add a
//! match arm without adding an `all` entry — both tests stayed green on a
//! variant the Go side never learned existed. There is no second, independently-editable
//! structure a variant addition could touch just one half of: adding a
//! variant to `FcrdnsMode` makes `variant_set!`'s generated match
//! non-exhaustive, and the only fix is adding the variant to the macro's list
//! — which is also the list the fixture is built from. A `_ =>` arm would
//! retire the guarantee.

use fauna_client_mail_settings::admin_policy::FcrdnsMode;
use std::collections::BTreeMap;
use std::path::PathBuf;

const FIXTURE: &str = "bins/fauna-bridges/internal/wsrpc/testdata/go-wire-policy-values.json";

/// Declares one enum's variant list ONCE, expanding to both the `(name,
/// instance)` list a fixture is built from and an exhaustive `match` checked
/// against every constructor `$ty` has. The two cannot drift apart because
/// they are not two pieces of code — they are one macro invocation. Adding an
/// enum variant makes the generated match non-exhaustive, and the only lever
/// available to fix that compile error is adding the variant HERE, which
/// necessarily extends the instance list too.
macro_rules! variant_set {
    ($ty:ident; $( $variant:ident $( { $($field:ident : $val:expr),* $(,)? } )? ),+ $(,)?) => {{
        let all: Vec<(&'static str, $ty)> = vec![
            $( (stringify!($variant), $ty::$variant $( { $($field: $val),* } )? ) ),+
        ];
        for (_, v) in &all {
            match v {
                $( $ty::$variant $( { $($field: _),* } )? )|+ => {}
            }
        }
        all
    }};
}

fn fcrdns_mode_tokens() -> BTreeMap<String, String> {
    variant_set!(FcrdnsMode;
        Off,
        ScoreSignal,
        Enforce,
    )
    .iter()
    .map(|(ident, v)| (ident.to_string(), v.as_str().to_string()))
    .collect()
}

fn live_map() -> BTreeMap<String, BTreeMap<String, String>> {
    BTreeMap::from([("FcrdnsMode".to_string(), fcrdns_mode_tokens())])
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = libs/fauna-client-mail-settings
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("libs/fauna-client-mail-settings sits two levels under the workspace root")
        .to_path_buf()
}

#[test]
fn the_go_policy_fixture_matches_the_live_tokens() {
    let path = workspace_root().join(FIXTURE);
    let live = live_map();
    // Trailing newline so the file is a well-formed text file and `git diff`
    // does not report "\ No newline at end of file" on every regen.
    let rendered = format!(
        "{}\n",
        serde_json::to_string_pretty(&live).expect("the token map serializes")
    );

    // Deliberately NO regen env var — a test with a write mode is a test that
    // can be made to bless whatever it finds.
    let committed = std::fs::read_to_string(&path).unwrap_or_default();

    assert_eq!(
        committed.trim_end(),
        rendered.trim_end(),
        "\n\nthe committed Go wire-POLICY fixture is STALE (or missing).\n\n\
         The admin-mail policy vocabularies no longer match {FIXTURE}. Bring the \
         fixture up to date, then expect internal/wsrpc/go_wire_outcome_contract_test.go \
         to go red until the Go const family follows — that second red is the point \
         of the chain, not a nuisance. The per-app pickers need no edit: they read \
         the owner directly.\n\n\
         Write EXACTLY this into {FIXTURE}:\n\n{rendered}\n\
         Do not compose it by hand from the enum source: the text above came from the \
         owning enum, and a hand-typed token list is exactly the copy this contract \
         exists to prevent.\n"
    );
}

#[test]
fn the_fallback_is_reachable_from_the_pinned_token_set() {
    // The owner's `picker_vocabulary_tests` pin *which* value the unknown-token
    // fallback is and why. What belongs here is the cross-language half: the
    // fallback must be one of the tokens the Go side also knows, or the two ends
    // disagree about what an unrecognised policy value means — Go would log its
    // warning and fall back to a token this fixture never told it about.
    let tokens = fcrdns_mode_tokens();
    let fallback = FcrdnsMode::from_wire_or_default("something-a-newer-nest-sent");
    assert!(
        tokens.values().any(|t| t == fallback.as_str()),
        "the fallback {:?} is not in the pinned token set",
        fallback.as_str()
    );
}
