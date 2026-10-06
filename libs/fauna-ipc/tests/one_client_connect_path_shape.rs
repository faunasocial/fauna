//! **One client-connect-path claim, mechanically enforced.** `sync-agent.md`
//! § Transports (`:60`) states, of the per-SID windows sync pipe, that
//! `sync_pipe_client::connect_pipe_to` is *"the one place every consumer
//! passes through"*; § Implementation status today S1 (`:231`) restates it
//! post-fix: *"Every in-tree consumer of the per-SID pipe goes through it."*
//! That claim is prose, and it rotted once already: the fourth consumer
//! (a since-removed diagnostic tool's sync client, a raw
//! `tokio::net::windows::named_pipe::ClientOptions::new().open`) was invisible to both greps an earlier
//! verify-back knew (`NamedPipeClientStream` is
//! C#-only; there is no `CreateFile` call — `ClientOptions` is a third
//! spelling of an open), so it survived the fix **and** its own grading
//! until a later pass caught it by hand.
//!
//! A hand list is stale the day it is written — the same lesson
//! `libs/fauna-core/tests/one_ed25519_verification_shape.rs` exists for — so
//! this is a full source-text census, not a sample, over every `.rs` file
//! under `apps/`, `libs/` and `bins/` (`src/` **and** `tests/` alike, so
//! `#[cfg(windows)]`-only modules are covered even on a non-windows dev
//! machine, where they are never type-checked).
//!
//! Two things are censused, in the two `#[test]` functions below:
//!
//! 1. Every non-comment call to `fauna_ipc::sync::current_user_pipe_name(`
//!    outside the four files that legitimately define, resolve or serve the
//!    per-user pipe name ([`PIPE_NAME_CALLER_ALLOWLIST`]).
//! 2. Every non-comment, pipe-qualified appearance of the `fauna-sync.`
//!    pipe-name prefix (i.e. preceded by `pipe` — not the bare substring,
//!    which also names unrelated things: `fauna-sync.service`,
//!    `fauna-sync.exe`, `fauna-sync.stdout.log`) outside those same four
//!    files, plus one named exception that holds it only as an inert STRING,
//!    never a raw open ([`PIPE_LITERAL_ALLOWLIST`]).
//!
//! **What this does NOT cover, stated so it is not read as more than it is:**
//! a raw open that builds the pipe name some other way than the literal
//! prefix (e.g. string concatenation piece by piece), and the C# side —
//! today `NamedPipeClientStream` appears only in a comment
//! (`apps/fauna-windows/FaunaApp/FaunaApp/Views/MainPage.xaml.cs:41`), no
//! opener.
//!
//! **Marginal value, stated plainly.** The finding this guards was LOW impact
//! on two unshipped dev tools. The census's worth is the *universality
//! claim* surviving the next consumer, not those two tools specifically.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // This crate lives at `<repo>/libs/fauna-ipc`.
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo");
    PathBuf::from(manifest)
        .parent()
        .and_then(Path::parent)
        .expect("<repo>/libs/fauna-ipc has two ancestors")
        .to_path_buf()
}

/// Cargo crate roots (directories containing a `Cargo.toml`) up to two levels
/// under `top` — covers both the single-crate app shape
/// (`apps/fauna-linux/Cargo.toml`) and the multi-crate workspace shape
/// (`apps/fauna-windows/fauna-bridge-service/Cargo.toml`) without hand-listing which app
/// is which, so a new windows crate is picked up automatically.
fn crate_roots(top: &str, root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root.join(top)) else {
        return out;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.join("Cargo.toml").is_file() {
            out.push(path);
            continue;
        }
        // Not a crate itself — maybe a multi-crate container (apps/fauna-windows).
        let Ok(inner) = std::fs::read_dir(&path) else {
            continue;
        };
        for e in inner.filter_map(Result::ok) {
            let p = e.path();
            if p.is_dir() && p.join("Cargo.toml").is_file() {
                out.push(p);
            }
        }
    }
    out
}

/// Every `.rs` file under each crate root's `src/` and `tests/` dirs, as
/// `(repo-relative path, contents)`. Both dirs, because the census must cover
/// a caller sitting in an integration test as much as one in `src/`.
fn rust_tree_sources() -> Vec<(String, String)> {
    let root = repo_root();
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = ["apps", "libs", "bins"]
        .iter()
        .flat_map(|top| crate_roots(top, &root))
        .flat_map(|crate_root| [crate_root.join("src"), crate_root.join("tests")])
        .filter(|p| p.is_dir())
        .collect();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read a crate src/tests dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel = path
                    .strip_prefix(&root)
                    .expect("under the repo root")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, std::fs::read_to_string(&path).expect("read a source")));
            }
        }
    }
    out
}

/// Is `line` a comment (`//`, `///` or `//!`) once leading whitespace is
/// trimmed? All three share the `//` prefix, so one check covers them.
fn is_comment_line(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

/// Files where `current_user_pipe_name(` may legitimately be called: its own
/// definition + its own tests (`sync.rs`), the two client connect entry
/// points that resolve it (`sync_pipe_client.rs`'s `connect_pipe`,
/// `endpoint.rs`'s `default_for_user`), and the server half that creates the
/// pipe rather than opening it (`bins/fauna-sync-agent/src/lib.rs`). Verified
/// 2026-09-21/23 as the full non-comment census.
const PIPE_NAME_CALLER_ALLOWLIST: &[&str] = &[
    "libs/fauna-ipc/src/sync.rs",
    "libs/fauna-ipc/src/sync_pipe_client.rs",
    "libs/fauna-ipc/src/endpoint.rs",
    "bins/fauna-sync-agent/src/lib.rs",
];

/// Files where the pipe-qualified `fauna-sync.` prefix may appear outside a
/// comment: the same four files above (they own the name, including their
/// own `#[cfg(test)]` fixtures that assert its shape), plus one file that
/// holds it only as an inert STRING, never a raw open — the tool connects
/// through `connect_pipe`/`AgentEndpoint::connect` like every other
/// consumer, same as the [`PIPE_NAME_CALLER_ALLOWLIST`] fix made true of
/// it:
///
/// - `apps/fauna-windows/shellext-fixture/src/main.rs` — a `.context(...)`
///   error message naming the pipe for a human reading the dev tool's output.
const PIPE_LITERAL_ALLOWLIST: &[&str] = &[
    "libs/fauna-ipc/src/sync.rs",
    "libs/fauna-ipc/src/sync_pipe_client.rs",
    "libs/fauna-ipc/src/endpoint.rs",
    "bins/fauna-sync-agent/src/lib.rs",
    "apps/fauna-windows/shellext-fixture/src/main.rs",
];

/// Beside-control floor: a walk that silently found nothing (a moved `src`, a
/// renamed workspace member) would otherwise pass every check below
/// vacuously forever. Measured 2778 `.rs` files under `apps/`, `libs/`,
/// `bins/` (`src/` + `tests/`) on 2026-09-23.
const MIN_SCANNED_FILES: usize = 1500;

fn assert_walk_is_looking_at_the_tree(sources: &[(String, String)]) {
    assert!(
        sources.len() >= MIN_SCANNED_FILES,
        "the tree walk found only {} sources; it is not looking at what it \
         claims to look at",
        sources.len()
    );
}

#[test]
fn the_tree_has_one_current_user_pipe_name_caller_set() {
    // Split so this guard's own source line doesn't match its needle — this
    // file lives in `libs/fauna-ipc/tests/`, so the walk below scans it too.
    let needle = concat!("current_user_pipe_name", "(");
    let sources = rust_tree_sources();
    assert_walk_is_looking_at_the_tree(&sources);

    let mut violations = Vec::new();
    for (rel, text) in &sources {
        if PIPE_NAME_CALLER_ALLOWLIST.contains(&rel.as_str()) {
            continue;
        }
        for (i, line) in text.lines().enumerate() {
            if is_comment_line(line) || !line.contains(needle) {
                continue;
            }
            violations.push(format!("{rel}:{}", i + 1));
        }
    }
    violations.sort();
    assert!(
        violations.is_empty(),
        "a new caller of current_user_pipe_name outside the allowlist: {violations:?}\n\
         `sync-agent.md` § Transports (\"the one place every consumer passes through\") means \
         every consumer resolves the pipe name through `AgentEndpoint::default_for_user` + \
         `AgentEndpoint::connect` (or, off windows, the unix socket path), never by calling \
         current_user_pipe_name and opening it raw — a raw open skips the server-identity \
         check that refuses a pipe squatted by another local account. If this caller is one \
         of the files that legitimately own the name, add it to PIPE_NAME_CALLER_ALLOWLIST \
         in this test."
    );
}

#[test]
fn the_tree_has_one_pipe_qualified_prefix_owner_set() {
    // Split the same way as the needle above, for the same reason.
    let bare_needle = concat!("fauna-sync", ".");
    // How far back from a `fauna-sync.` match to look for `pipe` — generous
    // enough to cover both a raw string (`\\.\pipe\fauna-sync.`, 9 chars of
    // prefix) and an escaped one (`\\\\.\\pipe\\fauna-sync.`, ~13 chars),
    // since the two encode the same runtime value with a different number of
    // source-text backslashes.
    const PIPE_LOOKBACK: usize = 20;

    let sources = rust_tree_sources();
    assert_walk_is_looking_at_the_tree(&sources);

    let mut violations = Vec::new();
    for (rel, text) in &sources {
        if PIPE_LITERAL_ALLOWLIST.contains(&rel.as_str()) {
            continue;
        }
        for (i, line) in text.lines().enumerate() {
            if is_comment_line(line) {
                continue;
            }
            let is_pipe_qualified = line.match_indices(bare_needle).any(|(at, _)| {
                let window_start = at.saturating_sub(PIPE_LOOKBACK);
                line.get(window_start..at)
                    .is_some_and(|w| w.contains("pipe"))
            });
            if is_pipe_qualified {
                violations.push(format!("{rel}:{}", i + 1));
            }
        }
    }
    violations.sort();
    assert!(
        violations.is_empty(),
        "a new non-comment appearance of the pipe-qualified fauna-sync prefix outside the \
         allowlist: {violations:?}\n\
         Building this literal outside the files that own the pipe name is how a raw open \
         (skipping the server-identity check) gets reintroduced without ever calling \
         current_user_pipe_name. If this is a legitimate owner, add it to \
         PIPE_LITERAL_ALLOWLIST in this test; if it is a display/test string like the \
         named exception there, document why in the same place."
    );
}
