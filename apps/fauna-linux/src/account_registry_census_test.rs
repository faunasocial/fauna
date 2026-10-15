//! The registry-construction census: `main.rs::account_registry()` is the
//! **only** place linux builds an `AccountRegistry`.
//!
//! A source-level pin, because a bypass is invisible at runtime — a registry
//! minted outside the choke point is silently no-op-locked and erases in the
//! wrong namespace. The scan, the rule and the full why live once, shared with
//! tui: `fauna_client_accounts::registry_census`. Add a writer that mints its
//! own registry and this test names your file and line; `main.rs` itself may
//! hold only the choke point's locked build and its degrade fallback.

#[test]
fn only_the_choke_point_constructs_the_account_registry() {
    fauna_client_accounts::registry_census::assert_only_the_choke_point_constructs(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        "main.rs",
        "crate::account_registry()",
    );
}
