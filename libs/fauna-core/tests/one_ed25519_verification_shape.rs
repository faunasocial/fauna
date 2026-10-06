//! **The whole Rust tree has ONE Ed25519 verification shape** — the strict
//! primitive [`fauna_core::identity::verify_detached`] (`verify_strict` plus a
//! small-order-key refusal), never `ed25519_dalek`'s permissive `Verifier`.
//!
//! This is the **tree-wide** walk that
//! `docs/goal/architecture/security.md` § Key material and signature
//! verification requires — every `libs/*/src/` and `bins/*/src/` source. The
//! nest keeps its own in-crate twin
//! (`bins/fauna-nest/src/state.rs::nest_has_one_ed25519_verification_shape`), so
//! a nest regression fails the nest's own suite rather than only this one; the
//! push relay carries the rule against its local copy of the primitive. Together
//! they are the *mechanical* form of a claim that was twice recorded as settled
//! fact and twice wrong:
//!
//! - The 2026-08-16 sweep enumerated the ceremonies by name and missed three
//!   nest sites of the same shape under different names.
//! - Hand census of `libs/` then listed "~a dozen" sites — and by the
//!   next morning today's group-machinery landings (`group_generation.rs`,
//!   `group_scope.rs`) had copied the permissive shape into four more that the
//!   list could not know about. **A hand list is stale the day it is written;
//!   only a walk stays true**, which is why this file exists rather than another
//!   enumeration.
//!
//! A site that legitimately hand-rolls a verify pays a one-line
//! `verify-ok(<class>)` marker in the preceding 8 lines, stating *why* the
//! permissive trait is harmless there — a test holding both halves of its own
//! keypair, a caller-supplied expected key, a TOFU key that IS the identity
//! being established. The marker is the per-site ruling, recorded where the next
//! reader will look.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // This crate lives at `<repo>/libs/fauna-core`.
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo");
    PathBuf::from(manifest)
        .parent()
        .and_then(Path::parent)
        .expect("<repo>/libs/fauna-core has two ancestors")
        .to_path_buf()
}

/// Every `.rs` file under `libs/*/src/` and `bins/*/src/`, as
/// `(repo-relative path, contents)`. Both halves, because the class does not
/// care which target kind hosts it — and a walk that stops at a directory
/// boundary is the enumeration this rule exists to replace.
fn rust_tree_sources() -> Vec<(String, String)> {
    let root = repo_root();
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = ["libs", "bins"]
        .iter()
        .flat_map(|top| {
            std::fs::read_dir(root.join(top))
                .expect("read libs/ and bins/")
                .filter_map(Result::ok)
                .map(|e| e.path().join("src"))
        })
        .filter(|p| p.is_dir())
        .collect();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read a crate src dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel = path
                    .strip_prefix(&root)
                    .expect("under the repo root")
                    .to_string_lossy()
                    .into_owned();
                out.push((rel, std::fs::read_to_string(&path).expect("read a source")));
            }
        }
    }
    out
}

#[test]
fn the_rust_tree_has_one_ed25519_verification_shape() {
    // Split so the guard's own source line doesn't match its needle.
    let needle = concat!("Veri", "fier");
    let mut violations = Vec::new();
    let mut scanned_files = 0usize;
    for (rel, text) in rust_tree_sources() {
        // `Verifier` is only the ed25519 trait in a file that names the crate;
        // this also keeps p256/k256 `ecdsa::signature::Verifier` (a different
        // curve with none of this class's small-order problem) out of the
        // population.
        if !text.contains("ed25519_dalek") {
            continue;
        }
        scanned_files += 1;
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            let Some(at) = line.find(needle) else {
                continue;
            };
            // Reject `DnsVerifier` and friends: a preceding identifier char
            // means this is a different type, not the trait.
            if line[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
            {
                continue;
            }
            // A quoted mention (an assert message, a doc string) is prose, not
            // an import. Odd quote count before the needle ⇒ inside a literal.
            if line[..at].matches('"').count() % 2 == 1 {
                continue;
            }
            let window = lines[i.saturating_sub(8)..i].join("\n");
            if !window.contains("verify-ok(") {
                violations.push(format!("{rel}:{}", i + 1));
            }
        }
    }
    // Beside-control: a walk that silently found nothing — a moved `src`, a
    // renamed dependency — would otherwise pass vacuously forever. Measured 89
    // matching sources on 2026-08-17 (64 under libs/, 25 under bins/).
    assert!(
        scanned_files >= 60,
        "the tree walk found only {scanned_files} sources naming ed25519_dalek; it \
         is not looking at what it claims to look at"
    );
    violations.sort();
    assert!(
        violations.is_empty(),
        "ed25519_dalek's permissive `Verifier` trait is imported without a ruling: \
         {violations:?}\n\
         Route the verification through `fauna_core::identity::verify_detached` (small-order key \
         refusal + `verify_strict`), the tree's only sanctioned Ed25519 verification shape. For a \
         wire-supplied key the permissive `verify` is not a binding check at all — PROBE-381-A \
         measured 63 of 256 forged payloads accepted through one such door. A site that must \
         hand-roll a verify pays a `verify-ok(<class>)` marker comment in the preceding 8 lines, \
         naming why the key there cannot be attacker-chosen."
    );
}

/// The **other** road to the permissive verify: a production caller of
/// `fauna_cbor::SignedEnvelope::verify_permissive`. It names neither the
/// `Verifier` trait nor (necessarily) `ed25519_dalek`, so the walk above never
/// sees it — the "delegated door one crate away" shape `security.md` § Key
/// material and signature verification warns about. That section states the
/// method has **zero production callers**; this is the sentence made
/// executable. Its definition is the one exempt line; any other call under
/// `libs/*/src/` or `bins/*/src/` reds unless it pays the same
/// `verify-ok(<class>)` marker in the preceding 8 lines.
#[test]
fn the_permissive_envelope_verify_has_no_production_caller() {
    // Split so the guard's own source line doesn't match its needle.
    let needle = concat!("verify_", "permissive(");
    let mut violations = Vec::new();
    let mut scanned_files = 0usize;
    for (rel, text) in rust_tree_sources() {
        scanned_files += 1;
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            let Some(at) = line.find(needle) else {
                continue;
            };
            // A quoted mention is prose, not a call.
            if line[..at].matches('"').count() % 2 == 1 {
                continue;
            }
            // The definition (`pub fn verify_permissive(`), not a call.
            if line[..at].trim_end().ends_with("fn") {
                continue;
            }
            let window = lines[i.saturating_sub(8)..i].join("\n");
            if !window.contains("verify-ok(") {
                violations.push(format!("{rel}:{}", i + 1));
            }
        }
    }
    // Beside-control: the walk must be looking at the tree, and must see the
    // definition it exempts (a renamed method would otherwise pass vacuously).
    assert!(
        scanned_files >= 500,
        "the tree walk found only {scanned_files} sources; it is not looking at what it \
         claims to look at"
    );
    let definition_seen = rust_tree_sources()
        .iter()
        .any(|(rel, text)| rel.ends_with("fauna-cbor/src/envelope.rs") && text.contains(needle));
    assert!(
        definition_seen,
        "fauna-cbor/src/envelope.rs no longer defines the permissive verify under this name; \
         re-point this guard at its new name"
    );
    violations.sort();
    assert!(
        violations.is_empty(),
        "`SignedEnvelope::verify_permissive` has a production caller without a ruling: \
         {violations:?}\n\
         It runs ed25519_dalek's permissive verify over whatever key the caller hands it. Route \
         the check through `fauna_core::encoding::verify_envelope` (small-order key refusal + \
         `verify_strict`), or pay a `verify-ok(<class>)` marker naming why the key there cannot \
         be attacker-chosen."
    );
}
