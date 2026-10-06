//! Shared PLC-operation audit-log-entry construction, behind the
//! `test-fixtures` feature for external consumers and `cfg(test)` for this
//! crate's own unit tests (mirrors `fauna-client-features::test_fixtures`'s
//! gating shape, widened with the `any(test, …)` half `fauna-client-accounts`
//! already uses for a module its own inline tests also consume).
//!
//! `entry`/`audit_log` were hand-copied byte-for-byte across
//! `genesis_verify.rs`, `tombstone.rs` and `fauna-client-alert-sweep`'s custody
//! feeder tests — same field order, same `verificationMethods`/`alsoKnownAs`/
//! `services` literals, same fake box-key value. This is the one canonical
//! body; each caller keeps only what genuinely varies — `cid`, `op_type`,
//! `rotation_keys`, `nullified`, and its own arbitrary `created_at` fixture
//! literal (the three prior copies each picked a different date; none of
//! them is semantically meaningful).

use serde_json::Value;

/// A standing PLC-operation audit-log entry — the JSON shape every
/// PLC-authenticating crate's tests build to feed `verify_audit_log` /
/// `standing_head` / the custody feeder's own log parser.
pub fn plc_operation_entry_json(
    cid: &str,
    op_type: &str,
    rotation_keys: &[&str],
    nullified: bool,
    created_at: &str,
) -> Value {
    serde_json::json!({
        "cid": cid,
        "nullified": nullified,
        "createdAt": created_at,
        "operation": {
            "type": op_type,
            "rotationKeys": rotation_keys,
            "verificationMethods": {"atproto": "did:key:zQ3shBoxJuniorKey"}, // gitleaks:allow
            "alsoKnownAs": ["at://alice.example.com"],
            "services": {},
            "prev": null,
            "sig": "fakesig"
        }
    })
}
