//! Cross-language Go→Rust round-trip verifier for every WS-RPC *request body*
//! type the Go mail-bridge hand-mirrors from the Rust serde shapes in
//! `libs/fauna-protocol/src/{bridge_routing,wrapped_blob}.rs`.
//!
//! # Why this exists
//!
//! `internal/wsrpc/methods.go` mirrors each Rust request type into a Go struct
//! with `cbor:"..."` tags. A Go-side field rename or type change silently breaks
//! the mirror in the OUTBOUND direction: the renamed wire key becomes unknown to
//! the Rust strict decoder — which drops it on decode (or errors on a now-missing
//! required field) and it vanishes on re-encode — wire drift no Go→Go test can
//! see. This is the symmetric mirror of the Rust→Go reply-body guard
//! (`internal/wsrpc/wsrpc_reply_cross_language_test.go`).
//!
//! These tests close that gap with the same decode-then-re-encode byte-equality
//! idiom as `libs/fauna-protocol/tests/conformance.rs`. For each committed
//! fixture (produced by the Go canonical encoder over a fully-populated
//! instance — see `internal/wsrpc/wsrpc_request_fixtures_gen_test.go`):
//!
//!   let bytes = read("request-<kebab>.cbor");
//!   let v: RustMirror = decode_strict(&bytes)?;          // production decode
//!   let re = encode_canonical(&v)?;                      // production encode
//!   assert_eq!(re.as_ref(), bytes);                      // byte-for-byte
//!
//! A Go-side rename → the renamed key is unknown to the Rust mirror → dropped on
//! decode → absent on re-encode → bytes differ → RED. (Fixtures populate EVERY
//! field with a non-zero value so an `Option`/`skip_serializing_if` field
//! survives the round-trip and a rename is observable.)
//!
//! # Fixture regen (from the workspace root)
//!
//!     export REGEN_WSRPC_FIXTURES=1
//!     go test -C bins/fauna-bridges ./internal/wsrpc/ -run TestRegenerateRequestFixtures
//!
//! then commit the updated `testdata/request-*.cbor` files.
//!
//! Coordination boundary: this file (and the Go-side coverage gate) are the
//! COVERAGE half — a genuine Go↔Rust drift surfaced here is a finding for
//! whoever maintains `methods.go` on the nest side, not a `methods.go` edit
//! in this lane.

use fauna_protocol::bridge_routing::*;
use fauna_protocol::wrapped_blob::{
    FetchBridgePubkeyRequest, FetchMlsSnapshotBlobRequest, FetchTlsCertBlobRequest,
    FetchWrappedMlsBlobRequest, FetchWrappedSubmissionTokenRequest, RegisterServiceUserRequest,
    ReportAuthEventRequest, RequestEnrollmentRequest,
};
use fauna_protocol::{decode_strict, encode_canonical};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The testdata dir holding the Go-produced `request-*.cbor` fixtures.
fn testdata_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../bins/fauna-bridges/internal/wsrpc/testdata")
}

/// `check::<T>` reads `request-<name>.cbor`, decodes it as the Rust mirror `T`
/// (production strict decode), re-encodes canonically (production encode), and
/// asserts byte-equality against the committed fixture. A mismatch / decode
/// failure means the Go fixture's wire shape no longer matches `T`'s serde
/// shape — a field rename / type change / dropped key.
fn check<T>(name: &str)
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let path = testdata_dir().join(name);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "read fixture {name}: {e}\n\
             regenerate: `export REGEN_WSRPC_FIXTURES=1` then \
             `go test -C bins/fauna-bridges ./internal/wsrpc/ -run TestRegenerateRequestFixtures`"
        )
    });
    let decoded: T = decode_strict(&bytes).unwrap_or_else(|e| {
        panic!(
            "{name}: decode_strict::<{}> failed: {e:?}\n\
             The Go fixture's wire shape no longer matches the Rust serde shape \
             (a field rename / type change / dropped key). If the Go mirror in \
             methods.go (maintained on the nest side) changed intentionally, update \
             the Rust type and regenerate the fixture.",
            std::any::type_name::<T>()
        )
    });
    let re =
        encode_canonical(&decoded).unwrap_or_else(|e| panic!("{name}: encode_canonical: {e:?}"));
    assert_eq!(
        re.as_ref(),
        &bytes[..],
        "{name}: re-encoded bytes differ from the Go-produced fixture for Rust type {}.\n\
         This means the Go mirror's cbor shape no longer matches the Rust serde shape \
         (a field rename / type change / dropped key).\n\
         If the Rust type changed intentionally, update the Go mirror in methods.go \
         (maintained on the nest side) and regenerate: \
         `export REGEN_WSRPC_FIXTURES=1` then \
         `go test -C bins/fauna-bridges ./internal/wsrpc/ -run TestRegenerateRequestFixtures`.",
        std::any::type_name::<T>()
    );
}

/// Every committed `request-*.cbor` fixture, paired with the `check::<T>` that
/// exercises its Rust mirror type. Kept in sync with the Go generator
/// (`internal/wsrpc/wsrpc_request_fixtures_gen_test.go`) and the
/// "── request bodies ──" block of `wsrpc_conformance_test.go`'s
/// `conformanceTypes`. The closures defer the `check` so the coverage test can
/// also enumerate the fixture names without running every round-trip.
fn registry() -> Vec<(&'static str, fn())> {
    vec![
        // ── wrapped_blob.rs requests ──
        ("request-request-enrollment.cbor", || {
            check::<RequestEnrollmentRequest>("request-request-enrollment.cbor")
        }),
        ("request-register-service-user.cbor", || {
            check::<RegisterServiceUserRequest>("request-register-service-user.cbor")
        }),
        ("request-fetch-tls-cert-blob.cbor", || {
            check::<FetchTlsCertBlobRequest>("request-fetch-tls-cert-blob.cbor")
        }),
        ("request-fetch-bridge-pubkey.cbor", || {
            check::<FetchBridgePubkeyRequest>("request-fetch-bridge-pubkey.cbor")
        }),
        ("request-report-auth-event.cbor", || {
            check::<ReportAuthEventRequest>("request-report-auth-event.cbor")
        }),
        ("request-fetch-wrapped-submission-token.cbor", || {
            check::<FetchWrappedSubmissionTokenRequest>(
                "request-fetch-wrapped-submission-token.cbor",
            )
        }),
        ("request-fetch-wrapped-mls-blob.cbor", || {
            check::<FetchWrappedMlsBlobRequest>("request-fetch-wrapped-mls-blob.cbor")
        }),
        ("request-fetch-mls-snapshot-blob.cbor", || {
            check::<FetchMlsSnapshotBlobRequest>("request-fetch-mls-snapshot-blob.cbor")
        }),
        // ── bridge_routing.rs requests ──
        ("request-whoami.cbor", || {
            check::<WhoamiRequest>("request-whoami.cbor")
        }),
        ("request-fetch-config.cbor", || {
            check::<FetchConfigRequest>("request-fetch-config.cbor")
        }),
        ("request-report-session-close.cbor", || {
            check::<ReportSessionCloseRequest>("request-report-session-close.cbor")
        }),
        ("request-validate-recipient.cbor", || {
            check::<ValidateRecipientRequest>("request-validate-recipient.cbor")
        }),
        ("request-check-greylist.cbor", || {
            check::<CheckGreylistRequest>("request-check-greylist.cbor")
        }),
        ("request-check-submission-quota.cbor", || {
            check::<CheckSubmissionQuotaRequest>("request-check-submission-quota.cbor")
        }),
        ("request-ingest-inbound-mail.cbor", || {
            check::<IngestInboundMailRequest>("request-ingest-inbound-mail.cbor")
        }),
        ("request-report-rejected-scan.cbor", || {
            check::<ReportRejectedScanRequest>("request-report-rejected-scan.cbor")
        }),
        ("request-fetch-recipient-mls-pubkey.cbor", || {
            check::<FetchRecipientMlsPubkeyRequest>("request-fetch-recipient-mls-pubkey.cbor")
        }),
        ("request-fetch-recipient-index-key.cbor", || {
            check::<FetchRecipientIndexKeyRequest>("request-fetch-recipient-index-key.cbor")
        }),
        ("request-fetch-message-ciphertext.cbor", || {
            check::<FetchMessageCiphertextRequest>("request-fetch-message-ciphertext.cbor")
        }),
        ("request-fetch-index-segments-since.cbor", || {
            check::<FetchIndexSegmentsSinceRequest>("request-fetch-index-segments-since.cbor")
        }),
        ("request-list-mailboxes.cbor", || {
            check::<ListMailboxesRequest>("request-list-mailboxes.cbor")
        }),
        ("request-select-mailbox.cbor", || {
            check::<SelectMailboxRequest>("request-select-mailbox.cbor")
        }),
        ("request-list-messages.cbor", || {
            check::<ListMessagesRequest>("request-list-messages.cbor")
        }),
        ("request-fetch-message-metadata.cbor", || {
            check::<FetchMessageMetadataRequest>("request-fetch-message-metadata.cbor")
        }),
        ("request-search-messages.cbor", || {
            check::<SearchMessagesRequest>("request-search-messages.cbor")
        }),
        ("request-get-quota.cbor", || {
            check::<GetQuotaRequest>("request-get-quota.cbor")
        }),
        ("request-store-flags.cbor", || {
            check::<StoreFlagsRequest>("request-store-flags.cbor")
        }),
        ("request-copy-messages.cbor", || {
            check::<CopyMessagesRequest>("request-copy-messages.cbor")
        }),
        ("request-move-messages.cbor", || {
            check::<MoveMessagesRequest>("request-move-messages.cbor")
        }),
        ("request-expunge.cbor", || {
            check::<ExpungeRequest>("request-expunge.cbor")
        }),
        ("request-append.cbor", || {
            check::<AppendMessageRequest>("request-append.cbor")
        }),
        ("request-fetch-outbound-due.cbor", || {
            check::<FetchOutboundDueRequest>("request-fetch-outbound-due.cbor")
        }),
        ("request-mark-outbound-delivered.cbor", || {
            check::<MarkOutboundDeliveredRequest>("request-mark-outbound-delivered.cbor")
        }),
        ("request-mark-outbound-failed.cbor", || {
            check::<MarkOutboundFailedRequest>("request-mark-outbound-failed.cbor")
        }),
        ("request-mark-outbound-bounced.cbor", || {
            check::<MarkOutboundBouncedRequest>("request-mark-outbound-bounced.cbor")
        }),
        ("request-enqueue-outbound-mail.cbor", || {
            check::<EnqueueOutboundMailRequest>("request-enqueue-outbound-mail.cbor")
        }),
        ("request-fetch-mta-sts-policy.cbor", || {
            check::<FetchMtaStsPolicyRequest>("request-fetch-mta-sts-policy.cbor")
        }),
        ("request-fetch-tlsa.cbor", || {
            check::<FetchTlsaRequest>("request-fetch-tlsa.cbor")
        }),
        ("request-report-tls-attempt.cbor", || {
            check::<ReportTlsAttemptRequest>("request-report-tls-attempt.cbor")
        }),
        ("request-fetch-recipient-forward-config.cbor", || {
            check::<FetchRecipientForwardConfigRequest>(
                "request-fetch-recipient-forward-config.cbor",
            )
        }),
        ("request-fetch-recipient-filters.cbor", || {
            check::<FetchRecipientFiltersRequest>("request-fetch-recipient-filters.cbor")
        }),
        ("request-forward-message.cbor", || {
            check::<ForwardMessageRequest>("request-forward-message.cbor")
        }),
        ("request-decode-srs-bounce.cbor", || {
            check::<DecodeSrsBounceRequest>("request-decode-srs-bounce.cbor")
        }),
        ("request-create-mailbox.cbor", || {
            check::<CreateMailboxRequest>("request-create-mailbox.cbor")
        }),
        ("request-delete-mailbox.cbor", || {
            check::<DeleteMailboxRequest>("request-delete-mailbox.cbor")
        }),
        ("request-rename-mailbox.cbor", || {
            check::<RenameMailboxRequest>("request-rename-mailbox.cbor")
        }),
        ("request-subscribe-mailbox.cbor", || {
            check::<SubscribeMailboxRequest>("request-subscribe-mailbox.cbor")
        }),
        ("request-unsubscribe-mailbox.cbor", || {
            check::<UnsubscribeMailboxRequest>("request-unsubscribe-mailbox.cbor")
        }),
        ("request-subscribe-mailbox-state.cbor", || {
            check::<SubscribeMailboxStateRequest>("request-subscribe-mailbox-state.cbor")
        }),
        ("request-provision-calendar.cbor", || {
            check::<ProvisionCalendarRequest>("request-provision-calendar.cbor")
        }),
        ("request-list-calendars.cbor", || {
            check::<ListCalendarsRequest>("request-list-calendars.cbor")
        }),
        ("request-put-event-ciphertext.cbor", || {
            check::<PutEventCiphertextRequest>("request-put-event-ciphertext.cbor")
        }),
        ("request-delete-event.cbor", || {
            check::<DeleteEventRequest>("request-delete-event.cbor")
        }),
        ("request-query-events.cbor", || {
            check::<QueryEventsRequest>("request-query-events.cbor")
        }),
        ("request-sync-calendar-since.cbor", || {
            check::<SyncCalendarSinceRequest>("request-sync-calendar-since.cbor")
        }),
        // The sidecar log plane's bridge leg. Its Go mirror lives in
        // internal/wsrpc (LogEvent + reportLogEventsRequest) rather than being
        // generated, so this is the guard that its `cbor:"…"` tags still match
        // the serde field names here.
        ("request-report-log-events.cbor", || {
            check::<fauna_protocol::log_plane::ReportLogEventsRequest>(
                "request-report-log-events.cbor",
            )
        }),
    ]
}

/// Round-trips every committed request fixture through its Rust mirror type and
/// asserts the re-encoded bytes equal the Go-produced fixture byte-for-byte.
#[test]
fn wsrpc_request_cross_language() {
    for (_name, run) in registry() {
        run();
    }
}

/// Fails if any `request-*.cbor` fixture on disk is not exercised by the
/// registry (an orphan fixture — e.g. a Go type renamed without updating this
/// mirror), and confirms the registry references no fixture missing from disk.
#[test]
fn no_orphan_request_fixtures() {
    let dir = testdata_dir();
    let on_disk: BTreeSet<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read testdata dir {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| {
            n.starts_with("request-")
                && n.ends_with(".cbor")
                && n != "request-validate-recipient-frame.cbor"
        })
        .collect();

    let registered: BTreeSet<String> = registry().into_iter().map(|(n, _)| n.to_string()).collect();

    let orphans: Vec<&String> = on_disk.difference(&registered).collect();
    assert!(
        orphans.is_empty(),
        "orphan request fixtures on disk not exercised by the registry: {orphans:?}\n\
         add them to the registry in wsrpc_request_cross_language.rs (or delete the stale fixture)."
    );

    let missing: Vec<&String> = registered.difference(&on_disk).collect();
    assert!(
        missing.is_empty(),
        "registry references fixtures missing from disk: {missing:?}\n\
         regenerate: `export REGEN_WSRPC_FIXTURES=1` then \
         `go test -C bins/fauna-bridges ./internal/wsrpc/ -run TestRegenerateRequestFixtures`."
    );
}

/// Sanity: the bare-body fixtures live alongside the FRAMED
/// `request-validate-recipient-frame.cbor` (an envelope fixture, owned by
/// `regen_go_wsrpc_fixture.rs` / `envelope_test.go`); it is deliberately
/// excluded from the bare-body registry above.
#[test]
fn frame_fixture_is_excluded() {
    let frame = testdata_dir().join("request-validate-recipient-frame.cbor");
    assert!(
        Path::new(&frame).exists(),
        "the framed envelope fixture {} should exist (renamed from request-validate-recipient.cbor)",
        frame.display()
    );
    let registered: BTreeSet<String> = registry().into_iter().map(|(n, _)| n.to_string()).collect();
    assert!(
        !registered.contains("request-validate-recipient-frame.cbor"),
        "the framed envelope fixture must not be in the bare-body registry"
    );
}

/// The Go mail bridge hand-mirrors the log plane's admission bounds
/// (`bins/fauna-bridges/internal/logplane/logplane.go`) because Go cannot
/// import Rust constants. Its own catalogue tests assert the catalogue fits
/// inside the *Go* copies — so if those drifted from the numbers nest actually
/// enforces, the Go suite would stay green while nest silently truncated
/// messages or rejected event ids. This is the guard that keeps the two in step:
/// a mismatch fails here, in the crate that owns the real values.
#[test]
fn go_mirror_of_the_log_plane_bounds_matches_rust() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let go = PathBuf::from(manifest).join("../../bins/fauna-bridges/internal/logplane/logplane.go");
    let src = std::fs::read_to_string(&go).unwrap_or_else(|e| panic!("read {}: {e}", go.display()));

    for (ident, expected) in [
        (
            "MaxEventsPerBatch",
            fauna_protocol::log_plane::MAX_EVENTS_PER_BATCH,
        ),
        (
            "MaxMessageBytes",
            fauna_protocol::log_plane::MAX_MESSAGE_BYTES,
        ),
        ("MaxEventBytes", fauna_protocol::log_plane::MAX_EVENT_BYTES),
    ] {
        let found = go_const(&src, ident).unwrap_or_else(|| {
            panic!(
                "{ident} not found in {} — did the Go mirror move?",
                go.display()
            )
        });
        assert_eq!(
            found,
            expected,
            "{ident} drifted: Go says {found}, fauna_protocol::log_plane says {expected}. \
             Update the Go mirror in {}.",
            go.display()
        );
    }
}

/// Pull `Ident = <number>` out of a Go const block. Deliberately dumb — it only
/// has to read a handful of `Name = 128` lines, and a parser that quietly
/// matched the wrong thing would be worse than one that finds nothing (which
/// fails loudly at the call site).
fn go_const(src: &str, ident: &str) -> Option<usize> {
    for line in src.lines() {
        let Some(rest) = line.trim().strip_prefix(ident) else {
            continue;
        };
        // Require the '=' so `MaxEventBytes` does not match a longer identifier
        // that happens to start with it.
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let digits: String = rest
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '_')
            .filter(|c| *c != '_')
            .collect();
        if !digits.is_empty() {
            return digits.parse().ok();
        }
    }
    None
}
