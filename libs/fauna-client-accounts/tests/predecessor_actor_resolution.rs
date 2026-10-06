//! Cross-language conformance guard: the `__mls`
//! post-succession re-seal's predecessor `BackupKey` list must be resolved
//! off the **session's own actor** on every app — never a registry "active
//! account" pointer, a class-level static, or any other slot a switch or an
//! append-mode sign-in can move independently of the session actually being
//! built.
//!
//! # Why this exists
//!
//! A review found apple and android resolving `FfiAccountRegistry::predecessor_backup_keys`
//! off exactly such a pointer while windows, web, linux and tui already resolved
//! it off the session's own actor. `FfiNestClient::build_conversations_session`
//! (`libs/fauna-ffi/src/nest_client.rs`) cannot catch a wrong-but-well-formed
//! list itself — it has no registry to cross-check against, by design — so an
//! app that reaches for the wrong pointer makes the re-seal silently do
//! nothing, with nothing to observe the mistake by. Nothing else pins this:
//! each app's own suite tests its own wiring, not whether that wiring reads
//! the right variable, and the FFI door's length-only validation is
//! structurally blind to it.
//!
//! Read the **production source that ships**, so an
//! edit on any of the six resolution sites is red immediately — no regen
//! step, no Swift/Kotlin/C# toolchain.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn app_source(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "predecessor-actor guard: cannot read the production source {rel}: {e}\n\
             If the app moved this file, re-point the constant in \
             libs/fauna-client-accounts/tests/predecessor_actor_resolution.rs — do NOT \
             delete the check: an edit here is exactly the class of bug the \
             review found."
        )
    })
}

/// `(app, file, the call that resolves predecessor keys off the SESSION'S
/// OWN actor)`.
const RESOLVES_OFF_SESSION_ACTOR: &[(&str, &str, &str)] = &[
    (
        "windows",
        "apps/fauna-windows/FaunaApp/FaunaApp.Core/Services/NestRpcClient.cs",
        "PredecessorBackupKeys(_crypto.ActorIdHex)",
    ),
    (
        "web",
        "libs/fauna-wasm/src/conversations.rs",
        ".predecessor_backup_keys(&keypair.actor_id_hex())",
    ),
    (
        "linux",
        "apps/fauna-linux/src/client.rs",
        ".predecessor_backup_keys(&actor_id))",
    ),
    (
        "tui",
        "apps/fauna-tui/src/session.rs",
        "registry(app).predecessor_backup_keys(actor_id)",
    ),
    (
        // FaunaClient.resolvedPredecessorBackupKeys() resolves off
        // ownActorIdHex — an instance property fixed at construction from
        // this instance's own secret — never activeActorIdHex, the
        // class-level static the account-switch path rewrites.
        "apple",
        "apps/fauna-apple/FaunaKit/Sources/FaunaKit/Core/FaunaClient.swift",
        "guard let actor = ownActorIdHex",
    ),
    (
        // sessionActorHex() derives the actor from THIS connection's own
        // cached secret — never accountStores.activeActorHex(), the
        // registry's active-account pointer.
        "android",
        "apps/fauna-android/app/src/main/java/com/fauna/app/core/ApiClient.kt",
        "sessionActorHex()?.let { accountStores.predecessorBackupKeys(it) }",
    ),
];

#[test]
fn every_app_resolves_predecessor_backup_keys_off_its_own_session_actor() {
    for (app, file, call) in RESOLVES_OFF_SESSION_ACTOR {
        assert!(
            app_source(file).contains(call),
            "{app} no longer resolves the __mls re-seal's predecessor keys off \
             its own session actor.\n\n\
             `{file}` no longer contains `{call}`. Resolving off any OTHER \
             pointer — a registry \"active account\" slot, a class-level static \
             a switch can rewrite — makes the re-seal silently do nothing \
             whenever the two disagree."
        );
    }
}

/// The two legs the review actually found wrong — a regression back
/// to either literal is the exact defect this whole file exists to catch. A
/// plumbing-only test (asserting a non-empty list arrives) would pass on
/// this wrong pointer just as readily as on the right one; these must-NOT
/// assertions are what actually pin the agreement.
const MUST_NOT_REGRESS: &[(&str, &str, &str)] = &[
    (
        "apple",
        "apps/fauna-apple/FaunaKit/Sources/FaunaKit/Core/FaunaClient.swift",
        "Self.activeActorIdHex.map {",
    ),
    (
        "android",
        "apps/fauna-android/app/src/main/java/com/fauna/app/core/ApiClient.kt",
        "accountStores.predecessorBackupKeys()",
    ),
];

#[test]
fn apple_and_android_never_resolve_off_the_active_pointer_again() {
    for (app, file, call) in MUST_NOT_REGRESS {
        assert!(
            !app_source(file).contains(call),
            "{app} regressed: `{file}` again contains `{call}` — the exact \
             active-pointer resolution the review found wrong. Resolve \
             off the session's own actor instead (see the sibling test in this \
             file)."
        );
    }
}
