//! End-to-end test of the binary through the real `git merge-base` path the
//! unit tests skip. Mirrors `scripts/test_check_cddl_evolution.py`'s integration
//! layer: build a temp repo whose `origin/main` carries a base struct, then
//! drive the compiled gate against a HEAD that does (or doesn't) break it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

static SEQ: AtomicU32 = AtomicU32::new(0);

const SRC_DIR: &str = "libs/fauna-protocol/src";

const BASE_FOO: &str = r#"
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
pub struct Foo {
    pub id: String,
    pub note: Option<String>,
    #[serde(flatten, default)]
    pub extra: std::collections::BTreeMap<String, fauna_cbor::Value>,
}
"#;

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(repo)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

/// A temp repo whose `origin/main` branch has `libs/fauna-protocol/src/foo.rs`.
fn base_repo() -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let repo = std::env::temp_dir().join(format!("check-additive-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(repo.join(SRC_DIR)).unwrap();
    std::fs::write(repo.join(SRC_DIR).join("foo.rs"), BASE_FOO).unwrap();

    git(&repo, &["init", "-q", "-b", "work"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    // `git merge-base HEAD origin/main` resolves against a local branch of that name.
    git(&repo, &["branch", "origin/main"]);
    repo
}

fn run_gate(repo: &Path) -> std::process::Output {
    run_gate_with_args(repo, &[])
}

fn run_gate_with_args(repo: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_check-additive-evolution"))
        .current_dir(repo)
        .args(args)
        .output()
        .expect("run gate")
}

fn rev_parse(repo: &Path, rev: &str) -> String {
    let out = Command::new("git")
        .current_dir(repo)
        .args(["rev-parse", rev])
        .output()
        .expect("git rev-parse");
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn clean_tree_passes() {
    let repo = base_repo();
    let out = run_gate(&repo);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stdout={stdout}\nstderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("passed"), "{stdout}");
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn removed_field_on_head_fails() {
    let repo = base_repo();
    // Drop the `note` field on HEAD only — a blocked change.
    std::fs::write(
        repo.join(SRC_DIR).join("foo.rs"),
        r#"
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
pub struct Foo {
    pub id: String,
    #[serde(flatten, default)]
    pub extra: std::collections::BTreeMap<String, fauna_cbor::Value>,
}
"#,
    )
    .unwrap();
    git(&repo, &["commit", "-aqm", "drop note"]);

    let out = run_gate(&repo);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr={stderr}");
    assert!(stderr.contains("removed wire field `note`"), "{stderr}");
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn new_struct_missing_catch_all_fails() {
    let repo = base_repo();
    // Add a brand-new non-strict wire struct with a field but no catch-all.
    std::fs::write(
        repo.join(SRC_DIR).join("bar.rs"),
        r#"
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
pub struct BarReq {
    pub name: String,
}
"#,
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "add bar"]);

    let out = run_gate(&repo);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr={stderr}");
    assert!(stderr.contains("BarReq"), "{stderr}");
    assert!(stderr.contains("catch-all"), "{stderr}");
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn added_optional_field_passes() {
    let repo = base_repo();
    std::fs::write(
        repo.join(SRC_DIR).join("foo.rs"),
        r#"
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
pub struct Foo {
    pub id: String,
    pub note: Option<String>,
    pub added: Option<u64>,
    #[serde(flatten, default)]
    pub extra: std::collections::BTreeMap<String, fauna_cbor::Value>,
}
"#,
    )
    .unwrap();
    git(&repo, &["commit", "-aqm", "add field"]);

    let out = run_gate(&repo);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&repo);
}

/// Pins the exact bug
/// found: the async dev-fleet merge-gate check builds in a tree PINNED at
/// the tip under test, so there HEAD, `origin/main`, and the working tree are all the
/// SAME commit — `merge-base(HEAD, origin/main)` resolves to that same commit,
/// and diffing it against itself reports zero violations unconditionally,
/// forever. `--base <rev>` is the fix: it lets the caller hand in the real
/// prior tip (LAST_GREEN in the check script) instead of trusting the
/// self-referential merge-base.
#[test]
fn explicit_base_catches_a_regression_the_self_diff_venue_would_hide() {
    let repo = base_repo();
    let last_green = rev_parse(&repo, "HEAD");

    // Drop `note` — a blocked, non-additive change — in a second commit, then
    // move BOTH `HEAD` and `origin/main` onto it: this is exactly the pinned
    // check-tree shape, where the tip under test IS origin/main IS HEAD.
    std::fs::write(
        repo.join(SRC_DIR).join("foo.rs"),
        r#"
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
pub struct Foo {
    pub id: String,
    #[serde(flatten, default)]
    pub extra: std::collections::BTreeMap<String, fauna_cbor::Value>,
}
"#,
    )
    .unwrap();
    git(&repo, &["commit", "-aqm", "drop note"]);
    git(&repo, &["branch", "-f", "origin/main", "HEAD"]);

    // Sanity: reproduce the bug first. No --base means merge-base(HEAD,
    // origin/main) == HEAD == origin/main, a self-diff that must pass
    // silently — if this ever stops passing, the temp-repo setup above no
    // longer models the pinned-tree venue and the test needs re-deriving.
    let self_diff = run_gate(&repo);
    assert!(
        self_diff.status.success(),
        "sanity check failed: the self-diff venue is supposed to hide the \
         regression with no --base (that's the bug this test exists to catch \
         the FIX for) — stdout={}",
        String::from_utf8_lossy(&self_diff.stdout)
    );

    // The fix: an explicit --base pointing at the real prior tip catches it.
    let out = run_gate_with_args(&repo, &["--base", &last_green]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "explicit --base did not catch the dropped field — stderr={stderr}"
    );
    assert!(stderr.contains("removed wire field `note`"), "{stderr}");
    let _ = std::fs::remove_dir_all(&repo);
}

/// one layer down: an EMPTY `--base` must be REFUSED, not silently
/// accepted. This is the same temp repo as
/// `explicit_base_catches_a_regression_the_self_diff_venue_would_hide` —
/// pinned tree, blocked change already committed — so an empty base that
/// degenerated back to a self-diff would exit 0 and hide the dropped field,
/// indistinguishable from a real pass. The tool must exit 2 (config error)
/// instead. Guards the caller-independent half of the fix: `merge-gate-
/// check.sh` wraps its call in `[ -n "$BASE" ]`, but a future caller that
/// forgets to must not get a green.
#[test]
fn empty_base_is_refused_rather_than_self_diffing() {
    let repo = base_repo();

    std::fs::write(
        repo.join(SRC_DIR).join("foo.rs"),
        r#"
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
pub struct Foo {
    pub id: String,
    #[serde(flatten, default)]
    pub extra: std::collections::BTreeMap<String, fauna_cbor::Value>,
}
"#,
    )
    .unwrap();
    git(&repo, &["commit", "-aqm", "drop note"]);
    git(&repo, &["branch", "-f", "origin/main", "HEAD"]);

    for arg in [&["--base", ""][..], &["--base="][..]] {
        let out = run_gate_with_args(&repo, arg);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(2),
            "an empty --base ({arg:?}) was not refused: git reads the base side from \
             the index and lists no files, so the gate compares nothing and passes \
             unconditionally — the self-diff wearing the fix's clothes. \
             stderr={stderr}"
        );
        assert!(stderr.contains("empty value"), "{stderr}");
    }
    let _ = std::fs::remove_dir_all(&repo);
}

/// The retired diff-based catch-all check only ever saw a struct on the
/// commit that introduced it — a struct present in BOTH base and head
/// ("old", by that retired test) was invisible to it forever. This repo has no committed `catch_all_baseline.txt` (its
/// relative path resolves inside the real checkout, not this temp repo), so
/// `load_catch_all_baseline` sees an empty baseline: nothing is
/// grandfathered, and the struct is flagged purely because it's
/// non-compliant TODAY, regardless of when it was added.
#[test]
fn a_struct_present_in_both_base_and_head_is_still_flagged_for_missing_catch_all() {
    let repo = base_repo();
    std::fs::write(
        repo.join(SRC_DIR).join("old_struct.rs"),
        r#"
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
pub struct OldNoCatchAll {
    pub a: String,
}
"#,
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "add old_struct present at base"]);
    let last_green = rev_parse(&repo, "HEAD");

    // A trivial, unrelated HEAD-only commit — old_struct.rs itself is
    // untouched, so it is present at BOTH base and head.
    std::fs::write(repo.join(SRC_DIR).join("noop.rs"), "// noop\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "unrelated head commit"]);

    let out = run_gate_with_args(&repo, &["--base", &last_green]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr={stderr}");
    assert!(stderr.contains("OldNoCatchAll"), "{stderr}");
    let _ = std::fs::remove_dir_all(&repo);
}

// ── enums: the ledger, through the real binary and `git cat-file --batch` ──

const ENUM_CRATE: &str = "libs/fauna-colors";
const LEDGER: &str = "tools/check-additive-evolution/enum_ledger.txt";

fn write(repo: &Path, path: &str, text: &str) {
    let p = repo.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// `base_repo()` plus a crate `fauna-colors` holding one listed enum.
fn enum_repo(ledger: &str) -> PathBuf {
    let repo = base_repo();
    write(
        &repo,
        &format!("{ENUM_CRATE}/Cargo.toml"),
        "[package]\nname = \"fauna-colors\"\n",
    );
    write(
        &repo,
        &format!("{ENUM_CRATE}/src/lib.rs"),
        "#[derive(serde::Deserialize)]\npub enum Color { Red, Green }\n",
    );
    write(&repo, LEDGER, ledger);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "enum base"]);
    git(&repo, &["branch", "-f", "origin/main"]);
    repo
}

#[test]
fn listed_enum_passes_and_counts_owed_lines() {
    let repo = enum_repo("owed-collapse fauna-colors::Color  # ruled open, not built\n");
    let out = run_gate(&repo);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stdout={stdout}\nstderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("scanned 1 deserializable enum(s) in 1 crate(s)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("1 `owed-` line(s) (1 owed-collapse)"),
        "{stdout}"
    );

    let out = run_gate_with_args(&repo, &["--no-owed"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr={stderr}");
    assert!(
        stderr.contains("fauna-colors::Color: enum_ledger.txt still owes"),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(&repo);
}

/// The base side is read through `git cat-file --batch`, so a removal seen
/// here proves that reader returned the base file.
#[test]
fn removed_variant_on_head_fails_and_an_unlisted_enum_fails() {
    let repo = enum_repo("closed-request fauna-colors::Color  # executed and discarded\n");
    write(
        &repo,
        &format!("{ENUM_CRATE}/src/lib.rs"),
        "#[derive(serde::Deserialize)]\npub enum Color { Red }\n\
         #[derive(serde::Deserialize)]\npub enum Shade { Light }\n",
    );
    git(&repo, &["commit", "-aqm", "drop Green, add Shade"]);
    let out = run_gate(&repo);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr={stderr}");
    assert!(
        stderr.contains("fauna-colors::Color: removed variant `Green`"),
        "{stderr}"
    );
    assert!(
        stderr.contains("fauna-colors::Shade: deserializable enum with no enum_ledger.txt line"),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn malformed_ledger_is_a_config_error() {
    let repo = enum_repo("perhaps fauna-colors::Color  # not an answer\n");
    let out = run_gate(&repo);
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn base_equals_form_is_accepted() {
    let repo = base_repo();
    let last_green = rev_parse(&repo, "HEAD");
    let arg = format!("--base={last_green}");
    let out = run_gate_with_args(&repo, &[&arg]);
    assert!(
        out.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&repo);
}
