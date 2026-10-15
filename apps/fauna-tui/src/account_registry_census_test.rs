//! The registry-construction census: `session.rs`'s `registry_over` is the
//! **only** place tui builds an `AccountRegistry`.
//!
//! A source-level pin, because a bypass is invisible at runtime — a registry
//! minted outside the choke point is silently no-op-locked and erases in the
//! wrong namespace. The scan, the rule and the full why live once, shared with
//! linux: `fauna_client_accounts::registry_census`. Add a writer that mints its
//! own registry and this test names your file and line; `session.rs` itself may
//! hold only the choke point's locked build and its degrade fallback.

#[test]
fn only_the_choke_point_constructs_the_account_registry() {
    fauna_client_accounts::registry_census::assert_only_the_choke_point_constructs(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        "session.rs",
        "crate::session::registry() / registry_over()",
    );
}
