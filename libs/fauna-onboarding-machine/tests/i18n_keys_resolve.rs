//! Every i18n key this crate names resolves in the shared table — pinned
//! textually, so a key the machine emits that no app can look up is a red
//! here instead of a raw key painted on screen (`docs/goal/behavior/onboarding.md`
//! § Architectural rules #4: an app looks a `LocalizedText` key up in its
//! generated i18n table and never recomputes the text). The handle check once
//! emitted every `handle_check.*` key without its `onboarding.` namespace:
//! tui, linux, web and windows showed the key, and macos/ios hid it behind a
//! per-app prefix fallback.
//!
//! The scan reads every string literal under `src/` shaped like a catalog key
//! (three or more dot-separated `[a-z0-9_]` segments) and asserts
//! `fauna_i18n::strings::lookup` resolves it. A literal of that shape which is
//! not a key is an RFC 2606 hostname (an `example` label or the `test` TLD), a
//! WS-RPC kind or error code (the `fauna.` wire namespace, which the catalog
//! never uses), or named in [`NOT_KEYS`] with why.

use std::path::{Path, PathBuf};

/// Dotted literals that are not i18n keys, each with what it is instead.
const NOT_KEYS: &[(&str, &str)] = &[
    ("social.fauna.fauna", "the app id an age claim is bound to"),
    (
        "invite.error.not_found",
        "a sentinel invite cause `snapshots::invite_request::message_for` maps \
         to `onboarding.invite.error.not_found`",
    ),
    (
        "invite.error.already_registered",
        "a sentinel invite cause `snapshots::invite_request::message_for` maps \
         to `onboarding.invite.error.already_registered`",
    ),
];

/// `true` for a literal the scan never treats as a key: a reserved hostname
/// or a WS-RPC kind / error code.
fn not_a_key(lit: &str) -> bool {
    let labels: Vec<&str> = lit.split('.').collect();
    labels.contains(&"example")
        || labels.last() == Some(&"test")
        || labels.first() == Some(&"fauna")
        || NOT_KEYS.iter().any(|(k, _)| *k == lit)
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read the src tree") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// `true` when `s` is three or more `[a-z0-9_]+` segments joined by dots,
/// the first starting with a letter.
fn key_shaped(s: &str) -> bool {
    let segments: Vec<&str> = s.split('.').collect();
    segments.len() >= 3
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && segments.iter().all(|seg| {
            !seg.is_empty()
                && seg
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
}

/// The contents of every `"…"` literal on `line` (no escapes needed: a key
/// never contains one).
fn literals(line: &str) -> Vec<&str> {
    line.split('"').skip(1).step_by(2).collect()
}

#[test]
fn every_key_shaped_literal_resolves_in_the_i18n_table() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    files.sort();
    assert!(
        files.len() > 10,
        "the scan found only {} files under {}",
        files.len(),
        src.display()
    );

    let mut checked = 0usize;
    let mut unresolved = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read a source file");
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for lit in literals(line) {
                if !key_shaped(lit) || not_a_key(lit) {
                    continue;
                }
                checked += 1;
                if fauna_i18n::strings::lookup(lit).is_none() {
                    unresolved.push(format!(
                        "{}:{}: \"{lit}\"",
                        file.strip_prefix(&src).unwrap_or(file).display(),
                        n + 1
                    ));
                }
            }
        }
    }

    assert!(
        checked > 50,
        "the scan matched only {checked} key-shaped literals"
    );
    assert!(
        unresolved.is_empty(),
        "{} key-shaped literal(s) do not resolve in fauna_i18n::strings::lookup \
         (a missing namespace prefix? a key absent from i18n/strings/en.yaml? \
         a non-key literal → name it in NOT_KEYS):\n{}",
        unresolved.len(),
        unresolved.join("\n")
    );
}
