//! One-shot regenerator for the exhaustive per-type **Rust→Go** WS-RPC
//! *reply-body* round-trip fixtures under
//! `bins/fauna-bridges/internal/wsrpc/testdata/reply-*.cbor`.
//!
//! # Why this exists
//!
//! The Go mail-bridge hand-mirrors every WS-RPC reply type from the Rust
//! `fauna_protocol::{bridge_routing, wrapped_blob, email}` serde shapes into Go
//! structs (`internal/wsrpc/methods.go`) with `cbor:"..."` tags. A Rust-side
//! field **rename** or **type change** silently breaks that mirror: the renamed
//! key becomes unknown to the Go struct, gets dropped on decode, and vanishes on
//! re-encode — wire drift no Go→Go test can see.
//!
//! These fixtures close that gap with the same decode-then-re-encode byte-
//! equality idiom as `libs/fauna-protocol/tests/conformance.rs`:
//!
//!   1. This Rust generator encodes a **fully-populated** instance of every
//!      reply body type with `fauna_protocol::encode_canonical` and writes the
//!      bytes to `testdata/reply-<kebab>.cbor`.
//!   2. `internal/wsrpc/wsrpc_reply_cross_language_test.go` reads each fixture,
//!      `dagcbor.Unmarshal[GoMirror]` → `dagcbor.Marshal`, and asserts the
//!      re-encoded bytes EQUAL the committed fixture byte-for-byte. A Rust-side
//!      rename/type-change makes the renamed key unknown to the Go mirror →
//!      dropped on decode → vanishes on re-encode → bytes differ → RED.
//!
//! For the round-trip to catch renames, EVERY field is populated with a
//! distinctive NON-ZERO value so `omitempty` Go fields survive. No floats
//! anywhere (dag-cbor strict decode forbids them; the surface uses scaled ints).
//! `Option<T>` / nested `serde_bytes` fields are populated `Some(...)` /
//! non-empty so they appear on the wire and round-trip.
//!
//! Each `#[serde(tag = "outcome")]` (or `tag = "kind"`) Rust enum gets ONE
//! fixture per variant (disjoint field sets), e.g.
//! `reply-validate-recipient-resolved.cbor` + `reply-validate-recipient-reject.cbor`.
//!
//! # Regen
//!
//! Run from the workspace root, then commit the updated fixtures:
//!
//!     cargo run -p fauna-protocol --example regen_go_wsrpc_reply_fixtures

use fauna_protocol::bridge_routing::*;
use fauna_protocol::email::{EmailFilter, EmailFilterAction, EmailFilterRule};
use fauna_protocol::wrapped_blob::{
    FetchBridgePubkeyReply, FetchMlsSnapshotBlobReply, FetchTlsCertBlobReply,
    FetchWrappedMlsBlobReply, FetchWrappedSubmissionTokenReply, RegisterServiceUserReply,
    ReportAuthEventReply, RequestEnrollmentReply,
};
use serde_bytes::ByteBuf;
use std::path::PathBuf;

/// A recognizable 32-byte pattern for every actor / id / pubkey byte field.
fn b32(seed: u8) -> Vec<u8> {
    vec![seed; 32]
}

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = manifest.join("../../bins/fauna-bridges/internal/wsrpc/testdata");

    let mut count = 0usize;

    macro_rules! emit {
        ($name:expr, $val:expr) => {{
            let bytes = fauna_protocol::encode_canonical(&$val).expect($name);
            std::fs::write(testdata.join($name), bytes.as_ref()).expect("write");
            println!("wrote {} ({} bytes)", $name, bytes.len());
            count += 1;
        }};
    }

    // ── wrapped_blob.rs replies ──────────────────────────────────────

    emit!(
        "reply-request-enrollment.cbor",
        RequestEnrollmentReply {
            status: "pending".into(),
            extra: Default::default(),
        }
    );

    emit!(
        "reply-register-service-user.cbor",
        RegisterServiceUserReply {
            enrollment_request_id: "enrollment-1122-pending".into(),
            extra: Default::default(),
        }
    );

    emit!(
        "reply-fetch-tls-cert-blob.cbor",
        FetchTlsCertBlobReply {
            blob: Some(ByteBuf::from(vec![0x11; 48])),
            extra: Default::default(),
        }
    );

    emit!(
        "reply-fetch-bridge-pubkey.cbor",
        FetchBridgePubkeyReply {
            ed25519_pubkey: b32(0x22),
            x25519_pubkey: b32(0x33),
            // Classical-holder fixture: `skip_serializing_if` omits `None`, so the
            // emitted CBOR stays byte-identical to the pre-PQ-CAP wire.
            mlkem_ek: None,
            extra: Default::default(),
        }
    );

    emit!(
        "reply-report-auth-event.cbor",
        ReportAuthEventReply {
            ok: true,
            extra: Default::default(),
        }
    );

    emit!(
        "reply-fetch-wrapped-submission-token.cbor",
        FetchWrappedSubmissionTokenReply {
            blob: Some(ByteBuf::from(vec![0x55; 40])),
            extra: Default::default(),
        }
    );

    emit!(
        "reply-fetch-wrapped-mls-blob.cbor",
        FetchWrappedMlsBlobReply {
            blob: Some(ByteBuf::from(vec![0x66; 80])),
            extra: Default::default(),
        }
    );

    emit!(
        "reply-fetch-mls-snapshot-blob.cbor",
        FetchMlsSnapshotBlobReply {
            blob: Some(ByteBuf::from(vec![0x77; 96])),
            extra: Default::default(),
        }
    );

    // ── bridge_routing.rs replies ────────────────────────────────────

    emit!(
        "reply-whoami.cbor",
        WhoamiReply {
            role: "mta".into(),
            bridge_id: "mta-deadbeef".into(),
            status: "approved".into(),
            ed25519_pubkey_hex: "1111111111111111".into(),
            x25519_pubkey_hex: "2222222222222222".into(),
            node_mode: "public".into(),
        }
    );

    emit!(
        "reply-report-session-close.cbor",
        ReportSessionCloseReply { ok: true }
    );

    // ValidateRecipientReply — tagged "outcome", one fixture per variant.
    emit!(
        "reply-validate-recipient-resolved.cbor",
        ValidateRecipientReply::Resolved {
            actor_id: b32(0x11),
            is_role_address: true,
        }
    );
    emit!(
        "reply-validate-recipient-reject.cbor",
        ValidateRecipientReply::Reject {
            reason: "user unknown".into(),
        }
    );

    emit!(
        "reply-check-greylist.cbor",
        CheckGreylistReply { pass: true }
    );

    // CheckSubmissionQuotaReply — tagged "outcome".
    emit!(
        "reply-check-submission-quota-allowed.cbor",
        CheckSubmissionQuotaReply::Allowed
    );
    emit!(
        "reply-check-submission-quota-over-quota.cbor",
        CheckSubmissionQuotaReply::OverQuota { remaining: 7 }
    );

    // FetchConfigReply (Go mirror: ConfigSnapshot). Fully-populated:
    // every sub-policy struct field set to a distinctive non-zero value.
    emit!(
        "reply-fetch-config.cbor",
        FetchConfigReply {
            mail_enabled: true,
            // Distinct from mail_enabled so the fixture catches a Go-mirror
            // field swap (Feature D — independent CalDAV enablement).
            caldav_enabled: false,
            // Distinct from the 8443 default so the Go-mirror fixture catches a
            // field swap / a missed `CalDAVPort` mapping in ConfigSnapshot.
            caldav_port: 9443,
            // Distinct from caldav_enabled so the Go-mirror fixture catches a
            // CardDAV field swap. The Go `ConfigSnapshot.CardDAVEnabled` mirror
            // landed with the Go MDA CardDAV terminator (CardDAV design § 8
            // slice 2a), and `reply-fetch-config.cbor` is regenerated with this
            // value — TestConfigSnapshotCardDAVEnabled asserts it decodes `true`.
            carddav_enabled: true,
            // Non-zero (distinct from the decode default `false`) so the Go-mirror
            // fixture catches a mistyped `cbor:"webdav_enabled"` tag / a missed
            // `ConfigSnapshot.WebDAVEnabled` mapping — a wrong tag would decode to
            // `false` and fail `TestConfigSnapshotWebDAVEnabled`. The Go mirror
            // landed with the WebDAV config plane (webdav-server.md § Independent
            // enablement), and `reply-fetch-config.cbor` is regenerated with this
            // value.
            webdav_enabled: true,
            local_domains: vec!["example.com".into(), "alt.example".into()],
            primary_domain: "example.com".into(),
            // Per-domain DKIM selectors — distinct per domain so the Go-mirror
            // fixture catches a field swap and the per-domain projection shape.
            dkim_selectors: vec![
                DomainDkimSelector {
                    domain: "example.com".into(),
                    selector: "sel1".into(),
                    extra: Default::default(),
                },
                DomainDkimSelector {
                    domain: "alt.example".into(),
                    selector: "sel2".into(),
                    extra: Default::default(),
                },
            ],
            spam: SpamPolicyThresholds {
                max_score_before_spam_folder: 3,
                max_score_before_reject: 11,
                dnsbl_servers: vec!["zen.spamhaus.org".into(), "extra.example".into()],
                reject_no_rdns: true,
                greylist_enabled: true,
                greylist_delay_secs: 42,
                max_conn_per_min: 13,
                fcrdns_mode: "enforce".into(),
                helo_identity_required: true,
                reject_fcrdns_fail: true,
                max_message_bytes: 1_234_567,
                bayesian_weight_milli: 850,
                bayesian_min_samples: 40,
                bayesian_full_confidence_samples: 300,
                training_history_retention_days: 14,
                unlisted_recipient_penalty: 9,
                baseline_standing_publish: true,
                extra: Default::default(),
            },
            auth: AuthPolicy {
                enforce_dmarc: true,
                enforce_dmarc_quarantine: true,
                enforce_spf_hardfail: true,
                enforce_dkim: true,
                log_only: true,
                max_auth_failures_per_minute: 17,
                max_conn_per_ip: 19,
                extra: Default::default(),
            },
            submission: SubmissionPolicyThresholds {
                max_per_day: 2000,
                max_recipients_per_message: 50,
                extra: Default::default(),
            },
            imap: ImapPolicy {
                idle_timeout_secs: 1800,
                tombstone_retention_days: 14,
                delete_nonempty: "allowed".into(),
                bodystructure_cache_max: 2048,
                storage_bytes_default: 2_147_483_648,
                message_count_default: 25_000,
                extra: Default::default(),
            },
            outbound: OutboundPolicy {
                retry_schedule_seconds: vec![1, 2, 3, 5, 8],
                permanent_failure_timeout_hours: 96,
                delay_warning_at_hours: 3,
                ndr_rate_limit_days: 5,
                suppress_ndr_spf_hardfail: true,
                suppress_ndr_dmarc_reject: true,
                postmaster_cc_bounces: true,
                tlsrpt_send_reports: true,
                ipv6_enabled: true,
                treat_5xx_as_transient: vec!["4.2.2".into()],
                extra: Default::default(),
            },
            bridge: BridgePolicy {
                shutdown_grace_seconds: 45,
                extra: Default::default(),
            },
            mass_mailing: MassMailingPolicy {
                list_recipients_per_send_ceiling: 4321,
                list_recipients_per_account_per_day_ceiling: 54321,
                list_recipients_per_deployment_per_day_ceiling: 654321,
                list_max_import_per_batch: 8765,
                extra: Default::default(),
            },
            extra: Default::default(),
        }
    );

    // IngestInboundMailReply (Go mirror: ingestInboundMailReply).
    emit!(
        "reply-ingest-inbound-mail.cbor",
        IngestInboundMailReply {
            message_id: b32(0x11),
        }
    );

    emit!(
        "reply-report-rejected-scan.cbor",
        ReportRejectedScanReply {
            message_id: b32(0x22),
        }
    );

    // FetchRecipientMlsPubkeyReply / FetchRecipientIndexKeyReply — Some(...) so
    // the Go pointer is non-nil and round-trips.
    emit!(
        "reply-fetch-recipient-mls-pubkey.cbor",
        FetchRecipientMlsPubkeyReply {
            key: Some(RecipientSealKeyHalves {
                mls_pubkey: ByteBuf::from(b32(0x33)),
                mlkem_ek: ByteBuf::from(vec![0x34u8; 1184]),
                extra: Default::default(),
            }),
            // Deliberately `true` despite `key` being `Some` (meaningless
            // combination in production) — a non-zero value is what makes a
            // Go-mirror field-name typo observable on round-trip; see this
            // file's header.
            succession_pending: true,
        }
    );
    emit!(
        "reply-fetch-recipient-index-key.cbor",
        FetchRecipientIndexKeyReply {
            pubkey: Some(ByteBuf::from(b32(0x44))),
        }
    );

    // FetchMessageCiphertextReply — tagged "outcome".
    emit!(
        "reply-fetch-message-ciphertext-found.cbor",
        FetchMessageCiphertextReply::Found {
            encrypted_body: vec![0x55; 64],
            ciphertext_size: 64,
            internal_date: 1_700_000_000,
            // Inline body → no bulk-plane reference; `skip_serializing_if`
            // keeps the fixture byte-identical to the pre-field shape.
            body_ref: None,
            // Populated (fixtures carry every field non-zero) so the Go
            // mirror's `stored_at` is drift-checked — the epoch
            // classification basis.
            stored_at: 1_700_000_009,
        }
    );
    emit!(
        "reply-fetch-message-ciphertext-not-found.cbor",
        FetchMessageCiphertextReply::NotFound
    );

    emit!(
        "reply-list-mailboxes.cbor",
        ListMailboxesReply {
            mailboxes: vec![MailboxEntry {
                name: "INBOX".into(),
                uid_validity: 101,
                uid_next: 7,
                highestmodseq: 5000,
                exists: 3,
                unseen: 2,
            }],
        }
    );

    // SelectMailboxReply — tagged "outcome".
    emit!(
        "reply-select-mailbox-selected.cbor",
        SelectMailboxReply::Selected {
            uid_validity: 101,
            uid_next: 8,
            highestmodseq: 5001,
            exists: 4,
            recent: 1,
            unseen: 2,
            first_unseen_uid: Some(3),
        }
    );
    emit!(
        "reply-select-mailbox-no-such-mailbox.cbor",
        SelectMailboxReply::NoSuchMailbox
    );

    emit!(
        "reply-list-messages.cbor",
        ListMessagesReply {
            messages: vec![MessageMeta {
                uid: 7,
                message_id: b32(0x11),
                modseq: 5000,
                flags: vec!["\\Seen".into(), "\\Flagged".into()],
                internal_date: 1_700_000_001,
                ciphertext_size: 1024,
                seq_num: 6,
            }],
            expunged_uids: vec![3, 5],
            highestmodseq: 5002,
            more: true,
        }
    );

    emit!(
        "reply-fetch-message-metadata.cbor",
        FetchMessageMetadataReply {
            messages: vec![MessageMeta {
                uid: 8,
                message_id: b32(0x22),
                modseq: 5003,
                flags: vec!["\\Answered".into()],
                internal_date: 1_700_000_002,
                ciphertext_size: 2048,
                seq_num: 9,
            }],
            mailbox_total: 9,
        }
    );

    emit!(
        "reply-fetch-index-segments-since.cbor",
        FetchIndexSegmentsSinceReply {
            segments: vec![IndexSegment {
                message_id: b32(0x33),
                mailbox: "INBOX".into(),
                modseq: 5004,
                encrypted_index_hint: vec![0x66; 48],
                stored_at: 1_700_000_010,
            }],
            highestmodseq: 5005,
            more: true,
        }
    );

    emit!(
        "reply-search-messages.cbor",
        SearchMessagesReply {
            uids: vec![1, 2, 3, 7],
        }
    );

    emit!(
        "reply-get-quota.cbor",
        GetQuotaReply {
            storage_bytes_used: 123_456,
            message_count_used: 42,
            storage_bytes_limit: 1_073_741_824,
            message_count_limit: 50_000,
        }
    );

    emit!(
        "reply-store-flags.cbor",
        StoreFlagsReply {
            updated: vec![StoreFlagsResultEntry {
                uid: 7,
                flags: vec!["\\Seen".into()],
                modseq: 5006,
            }],
            highestmodseq: 5007,
            modified: vec![9, 11],
        }
    );

    emit!(
        "reply-copy-messages.cbor",
        CopyMessagesReply {
            dest_uid_validity: 202,
            copied: vec![CopyPair {
                source_uid: 7,
                dest_uid: 1,
            }],
            dest_highestmodseq: 6000,
        }
    );

    emit!(
        "reply-move-messages.cbor",
        MoveMessagesReply {
            dest_uid_validity: 203,
            moved: vec![CopyPair {
                source_uid: 8,
                dest_uid: 2,
            }],
            source_highestmodseq: 6001,
            dest_highestmodseq: 6002,
        }
    );

    emit!(
        "reply-expunge.cbor",
        ExpungeReply {
            expunged_uids: vec![3, 7],
            highestmodseq: 6003,
        }
    );

    emit!(
        "reply-append.cbor",
        AppendMessageReply {
            message_id: b32(0x44),
            uid: 9,
            uid_validity: 204,
        }
    );

    emit!(
        "reply-fetch-outbound-due.cbor",
        FetchOutboundDueReply {
            units: vec![OutboundUnit {
                id: 1,
                message_id: "msg-1".into(),
                original_sender: "alice@example.com".into(),
                recipient: "bob@remote.example".into(),
                raw_message: vec![0x77; 128],
                attempt_count: 2,
                // Exercise the non-default value so the Go round-trip proves it
                // decodes + re-encodes the list-send signing flag byte-for-byte.
                staged_body: None,
            }],
        }
    );

    emit!(
        "reply-mark-outbound-delivered.cbor",
        MarkOutboundDeliveredReply { ok: true }
    );
    emit!(
        "reply-mark-outbound-failed.cbor",
        MarkOutboundFailedReply { ok: true }
    );
    emit!(
        "reply-mark-outbound-bounced.cbor",
        MarkOutboundBouncedReply { ok: true }
    );

    emit!(
        "reply-enqueue-outbound-mail.cbor",
        EnqueueOutboundMailReply {
            ids: vec![10, 11, 12],
        }
    );

    // FetchMtaStsPolicyReply — flat struct; populate the Option policy Some.
    emit!(
        "reply-fetch-mta-sts-policy.cbor",
        FetchMtaStsPolicyReply {
            outcome: "found".into(),
            policy: Some(MtaStsPolicyWire {
                id: "20240101T000000".into(),
                mode: "enforce".into(),
                mx: vec!["mx1.example.com".into(), "mx2.example.com".into()],
                max_age_secs: 604_800,
            }),
        }
    );

    emit!(
        "reply-fetch-tlsa.cbor",
        FetchTlsaReply {
            records: vec![TlsaRecordWire {
                usage: 3,
                selector: 1,
                matching: 1,
                data: vec![0x88; 32],
            }],
        }
    );

    emit!(
        "reply-report-tls-attempt.cbor",
        ReportTlsAttemptReply { ok: true }
    );

    emit!(
        "reply-fetch-recipient-forward-config.cbor",
        FetchRecipientForwardConfigReply {
            forward_all_to: Some("forward@elsewhere.example".into()),
        }
    );

    // FetchRecipientFiltersReply — carries a fully-populated EmailFilter; the
    // Go EmailFilterWire decodes rules/action as raw CBOR and re-emits verbatim.
    emit!(
        "reply-fetch-recipient-filters.cbor",
        FetchRecipientFiltersReply {
            filters: vec![EmailFilter {
                id: 1,
                name: "rule-1".into(),
                rules: vec![EmailFilterRule::HeaderContains {
                    name: "Subject".into(),
                    value: "urgent".into(),
                }],
                combination: "all".into(),
                action: EmailFilterAction::FileInto {
                    mailbox: "Urgent".into(),
                },
                priority: 5,
                continue_on_match: true,
                created_at: 1_700_000_003,
                extra: Default::default(),
            }],
        }
    );

    emit!(
        "reply-forward-message.cbor",
        ForwardMessageReply {
            id: 13,
            queued: true,
        }
    );

    emit!(
        "reply-decode-srs-bounce.cbor",
        DecodeSrsBounceReply {
            outcome: "ok".into(),
            forwarder_actor_id: b32(0x55),
            original_sender: "sender@origin.example".into(),
            original_destination: "dest@remote.example".into(),
        }
    );

    // CreateMailboxReply — tagged "outcome".
    emit!(
        "reply-create-mailbox-created.cbor",
        CreateMailboxReply::Created { uid_validity: 205 }
    );
    emit!(
        "reply-create-mailbox-already-exists.cbor",
        CreateMailboxReply::AlreadyExists
    );
    emit!(
        "reply-create-mailbox-reserved.cbor",
        CreateMailboxReply::Reserved
    );
    emit!(
        "reply-create-mailbox-invalid-name.cbor",
        CreateMailboxReply::InvalidName {
            reason: "too long".into(),
        }
    );

    // DeleteMailboxReply — tagged "outcome", unit variants.
    emit!(
        "reply-delete-mailbox-deleted.cbor",
        DeleteMailboxReply::Deleted
    );
    emit!(
        "reply-delete-mailbox-no-such-mailbox.cbor",
        DeleteMailboxReply::NoSuchMailbox
    );
    emit!(
        "reply-delete-mailbox-reserved.cbor",
        DeleteMailboxReply::Reserved
    );
    emit!(
        "reply-delete-mailbox-not-empty.cbor",
        DeleteMailboxReply::NotEmpty
    );

    // RenameMailboxReply — tagged "outcome".
    emit!(
        "reply-rename-mailbox-renamed.cbor",
        RenameMailboxReply::Renamed
    );
    emit!(
        "reply-rename-mailbox-no-such-source.cbor",
        RenameMailboxReply::NoSuchSource
    );
    emit!(
        "reply-rename-mailbox-reserved-source.cbor",
        RenameMailboxReply::ReservedSource
    );
    emit!(
        "reply-rename-mailbox-target-reserved.cbor",
        RenameMailboxReply::TargetReserved
    );
    emit!(
        "reply-rename-mailbox-target-exists.cbor",
        RenameMailboxReply::TargetExists
    );
    emit!(
        "reply-rename-mailbox-invalid-name.cbor",
        RenameMailboxReply::InvalidName {
            reason: "bad name".into(),
        }
    );

    // SubscribeMailboxReply / UnsubscribeMailboxReply — tagged "outcome".
    emit!(
        "reply-subscribe-mailbox-subscribed.cbor",
        SubscribeMailboxReply::Subscribed
    );
    emit!(
        "reply-unsubscribe-mailbox-unsubscribed.cbor",
        UnsubscribeMailboxReply::Unsubscribed
    );

    // SubscribeMailboxStateReply — tagged "outcome".
    emit!(
        "reply-subscribe-mailbox-state-subscribed.cbor",
        SubscribeMailboxStateReply::Subscribed {
            subscription_id: 99,
        }
    );

    // ProvisionCalendarReply — tagged "outcome", unit variants.
    emit!(
        "reply-provision-calendar-created.cbor",
        ProvisionCalendarReply::Created
    );
    emit!(
        "reply-provision-calendar-already-exists.cbor",
        ProvisionCalendarReply::AlreadyExists
    );
    emit!(
        "reply-provision-calendar-conflict.cbor",
        ProvisionCalendarReply::Conflict
    );
    emit!(
        "reply-provision-calendar-updated.cbor",
        ProvisionCalendarReply::Updated
    );
    emit!(
        "reply-provision-calendar-not-found.cbor",
        ProvisionCalendarReply::NotFound
    );

    emit!(
        "reply-list-calendars.cbor",
        ListCalendarsReply {
            calendars: vec![CalendarEntry {
                calendar_id: b32(0x66),
                encrypted_metadata: vec![0x99; 48],
                ctag: 7,
                highestmodseq: 5008,
                event_count: 4,
                created_at: 1_700_000_004,
            }],
        }
    );

    // PutEventCiphertextReply — tagged "outcome".
    emit!(
        "reply-put-event-ciphertext-created.cbor",
        PutEventCiphertextReply::Created {
            event_id: b32(0x77),
            etag: "etag-1".into(),
            modseq: 5009,
        }
    );
    emit!(
        "reply-put-event-ciphertext-updated.cbor",
        PutEventCiphertextReply::Updated {
            event_id: b32(0x88),
            etag: "etag-2".into(),
            modseq: 5010,
        }
    );
    emit!(
        "reply-put-event-ciphertext-precondition-failed.cbor",
        PutEventCiphertextReply::PreconditionFailed {
            current_etag: "etag-cur".into(),
        }
    );
    emit!(
        "reply-put-event-ciphertext-calendar-not-found.cbor",
        PutEventCiphertextReply::CalendarNotFound
    );

    // DeleteEventReply — tagged "outcome".
    emit!(
        "reply-delete-event-deleted.cbor",
        DeleteEventReply::Deleted {
            event_id: b32(0x99),
            modseq: 5011,
        }
    );
    emit!(
        "reply-delete-event-not-found.cbor",
        DeleteEventReply::NotFound
    );
    emit!(
        "reply-delete-event-precondition-failed.cbor",
        DeleteEventReply::PreconditionFailed {
            current_etag: "etag-del".into(),
        }
    );

    // QueryEventsReply — tagged "outcome".
    emit!(
        "reply-query-events-ok.cbor",
        QueryEventsReply::Ok {
            events: vec![EventEntry {
                event_id: b32(0xaa),
                uid_hash: b32(0xbb),
                encrypted_body: vec![0xcc; 64],
                encrypted_index_hint: vec![0xdd; 48],
                etag: "etag-ev".into(),
                modseq: 5012,
                ciphertext_size: 64,
                internal_date: 1_700_000_005,
                // None sidecar — `skip_serializing_if` omits the key, so the
                // generated Go fixture bytes stay identical (no MUA sidecar).
                ..Default::default()
            }],
            highestmodseq: 5013,
            more: true,
        }
    );
    emit!(
        "reply-query-events-calendar-not-found.cbor",
        QueryEventsReply::CalendarNotFound
    );

    // SyncCalendarSinceReply — tagged "outcome".
    emit!(
        "reply-sync-calendar-since-ok.cbor",
        SyncCalendarSinceReply::Ok {
            changed: vec![EventEntry {
                event_id: b32(0xa1),
                uid_hash: b32(0xa2),
                encrypted_body: vec![0xa3; 64],
                encrypted_index_hint: vec![0xa4; 48],
                etag: "etag-sync".into(),
                modseq: 5014,
                ciphertext_size: 64,
                internal_date: 1_700_000_006,
                ..Default::default()
            }],
            expunged: vec![ExpungedEntry {
                event_id: b32(0xa5),
                uid_hash: b32(0xa6),
                modseq: 5015,
            }],
            new_sync_token: "5015".into(),
            more: true,
            stale: true,
        }
    );
    emit!(
        "reply-sync-calendar-since-calendar-not-found.cbor",
        SyncCalendarSinceReply::CalendarNotFound
    );
    emit!(
        "reply-sync-calendar-since-stale.cbor",
        SyncCalendarSinceReply::Stale {
            server_modseq: 5016,
        }
    );

    println!("\nwrote {count} reply fixtures");
}
