// One-shot regenerator + coverage gate for the exhaustive per-type Go→Rust
// WS-RPC *request-body* round-trip fixtures under
// internal/wsrpc/testdata/request-<kebab>.cbor.
//
// # Why this exists
//
// methods.go hand-mirrors every WS-RPC request type from the Rust serde
// shapes in libs/fauna-protocol/src/{bridge_routing,wrapped_blob}.rs into a Go
// struct with `cbor:"..."` tags. A Go-side field rename or type change silently
// breaks that mirror in the OUTBOUND direction: the renamed wire key becomes
// unknown to the Rust strict decoder, which drops it on decode (or errors on a
// now-missing required field) and it vanishes on re-encode — wire drift no
// Go→Go test can see. This is the symmetric mirror of the Rust→Go reply-body
// guard (wsrpc_reply_cross_language_test.go + regen_go_wsrpc_reply_fixtures.rs).
//
// The guard closes that gap with the same decode-then-re-encode byte-equality
// idiom as libs/fauna-protocol/tests/conformance.rs:
//
//  1. TestRegenerateRequestFixtures (gated on REGEN_WSRPC_FIXTURES) marshals a
//     fully-populated instance of every request body type with
//     dagcbor.Marshal (the production canonical encoder) and writes the bare
//     body bytes to testdata/request-<kebab>.cbor.
//  2. libs/fauna-protocol/tests/wsrpc_request_cross_language.rs reads each
//     fixture, decode_strict::<RustMirror>, encode_canonical, and asserts the
//     re-encoded bytes EQUAL the committed fixture byte-for-byte. A Go-side
//     rename/type-change makes the renamed key unknown to the Rust mirror →
//     dropped on decode → vanishes on re-encode → bytes differ → RED.
//
// For the round-trip to catch renames, EVERY field is populated with a
// distinctive non-zero value (ints 1,2,3,7,42,101…; non-empty strings;
// non-empty slices; bools true; []byte = a recognizable repeated pattern) so an
// omitempty Go field / skip_serializing_if Rust field survives the round-trip
// and a rename is observable. No floats anywhere (dag-cbor strict decode forbids
// them). Every Go pointer / omitempty field is populated non-nil/non-zero so it
// appears on the wire.
//
// FIXTURE REGEN (from the workspace root):
//
//	export REGEN_WSRPC_FIXTURES=1
//	go test -C bins/fauna-bridges ./internal/wsrpc/ -run TestRegenerateRequestFixtures
//
// then commit the updated testdata/request-*.cbor files.
//
// Coordination boundary: like wsrpc_conformance_test.go / wsrpc_reply_cross_
// language_test.go, this is the COVERAGE half — do not edit codec.go /
// methods.go / methods_test.go here (that is separate, coordinated work). A
// genuine Go↔Rust drift surfaced by the Rust consumer is a finding to report
// there, not a methods.go edit in this one.
package wsrpc

import (
	"bytes"
	"os"
	"path/filepath"
	"reflect"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
)

// b32 is a recognizable 32-byte pattern for every actor / id / pubkey / blob
// byte field. The seed makes distinct fields visibly distinct in a hex dump.
func b32(seed byte) []byte { return bytes.Repeat([]byte{seed}, 32) }

// strPtr / i64Ptr / u32Ptr / bytesPtr build the non-nil pointers the Option /
// omitempty fields need so they ride on the wire (a nil pointer would encode as
// CBOR null = Rust None, which still round-trips but does NOT exercise the
// field's payload shape — we want the populated shape).
func strPtr(s string) *string   { return &s }
func i64Ptr(n int64) *int64     { return &n }
func u32Ptr(n uint32) *uint32   { return &n }
func bytesPtr(b []byte) *[]byte { return &b }

// requestFixture binds one fixture name to the fully-populated Go request body
// value that the regenerator marshals into testdata/<name>.
type requestFixture struct {
	name string
	val  any
}

// requestFixtures is the authoritative list of every request-body fixture, one
// entry per fixture file. Plain-struct requests get one entry; the rare
// request that is itself a tagged union would get one per variant (none today —
// every WS-RPC request body is a plain struct; the tagged unions live on the
// reply side). Kept in sync with the Rust consumer registry in
// libs/fauna-protocol/tests/wsrpc_request_cross_language.rs and with the
// "── request bodies ──" block of wsrpc_conformance_test.go's conformanceTypes.
func requestFixtures() []requestFixture {
	return []requestFixture{
		// ── wrapped_blob.rs requests ──
		{"request-request-enrollment.cbor", requestEnrollmentRequest{
			Ed25519Pubkey: b32(0x11), RoleHint: "mta", BridgeID: "mta-deadbeef",
		}},
		{"request-register-service-user.cbor", registerServiceUserRequest{
			Ed25519Pubkey: b32(0x22), X25519Pubkey: b32(0x33),
			MlkemEk: bytesPtr(bytes.Repeat([]byte{0x44}, 1184)),
			Role:    "mda", BridgeID: "mda-cafef00d",
		}},
		{"request-fetch-tls-cert-blob.cbor", fetchTLSCertBlobRequest{
			BridgeRole: "mta", BridgeID: "mta-1", Domain: "example.com",
		}},
		{"request-fetch-bridge-pubkey.cbor", fetchBridgePubkeyRequest{
			BridgeRole: "mda", BridgeID: "mda-1",
		}},
		{"request-report-auth-event.cbor", reportAuthEventRequest{
			ActorID: b32(0x44), CredentialID: "default", Result: "fail",
			SourceIP: "203.0.113.7", OccurredAt: 1_700_000_000, Reason: strPtr("bad password"),
		}},
		{"request-fetch-wrapped-submission-token.cbor", fetchWrappedSubmissionTokenRequest{
			ActorID: b32(0x55), CredentialID: "default",
		}},
		{"request-fetch-wrapped-mls-blob.cbor", fetchWrappedMLSBlobRequest{
			ActorID: b32(0x66), CredentialID: "default",
		}},
		{"request-fetch-mls-snapshot-blob.cbor", fetchMLSSnapshotBlobRequest{
			ActorID: b32(0x77),
		}},

		// ── bridge_routing.rs requests ──
		{"request-whoami.cbor", whoamiRequest{}},
		{"request-fetch-config.cbor", fetchConfigRequest{Scope: "mta"}},
		{"request-report-session-close.cbor", reportSessionCloseRequest{
			ActorID: b32(0x11), CredentialID: "default", Reason: "logout", OccurredAt: 1_700_000_001,
		}},
		{"request-validate-recipient.cbor", validateRecipientRequest{
			LocalPart: "alice", Domain: "example.com",
		}},
		{"request-check-greylist.cbor", checkGreylistRequest{
			From: "sender@remote.example", To: "alice@example.com", ClientIP: "198.51.100.4",
		}},
		{"request-check-submission-quota.cbor", checkSubmissionQuotaRequest{
			ActorID: b32(0x22), RecipientCount: 3,
		}},
		// ingest_inbound_mail — the richest request: every nested verdict variant
		// populated (DKIM fail w/ reason, SPF pass, DMARC fail w/ policy, ARC pass),
		// ClamAV infected w/ signature, full RspamdScore, both Option fields set.
		{"request-ingest-inbound-mail.cbor", ingestInboundMailRequest{
			ActorID:            b32(0x11),
			EncryptedBody:      bytes.Repeat([]byte{0xab}, 64),
			EncryptedIndexHint: bytes.Repeat([]byte{0xcd}, 48),
			PublicMetadata: PublicMailMetadata{
				Timestamp: 1_700_000_002, CiphertextSize: 64, SenderDomain: "remote.example",
			},
			Verdicts: AuthVerdicts{
				Dkim:  DkimVerdict{Kind: "fail", Data: &DkimVerdictFail{Reason: "body hash mismatch"}},
				Spf:   SpfVerdict{Kind: "pass"},
				Dmarc: DmarcVerdict{Kind: "fail", Data: &DmarcVerdictFail{Policy: "quarantine"}},
				Arc:   ArcVerdict{Kind: "pass"},
			},
			SpamScore:       7,
			SpamDisposition: "accept_to_spam_folder",
			ClamavVerdict:   ClamavVerdict{Kind: "infected", Data: &ClamavVerdictData{Signature: "Eicar-Test-Signature"}},
			RspamdScore: &RspamdScore{
				RawMilli: 4200, ScaledMilli: 3100,
				FlaggedRules: []string{"BAYES_SPAM", "MIME_HTML_ONLY"},
				Breakdown: []RspamdRuleContribution{
					{Rule: "BAYES_SPAM", ScoreMilli: 3000},
					{Rule: "MIME_HTML_ONLY", ScoreMilli: 1200},
				},
			},
			IsRoleAddress: true,
			TargetMailbox: strPtr("Urgent"),
			ExtraFlags:    []string{"$label1", "\\Flagged"},
			Scores: []ScoreEntry{
				{Factor: "phishing", Score: 990, Tier: 3, ScorerVersion: 2},
				{Factor: "spam", Score: -42, Tier: 1, ScorerVersion: 1},
			},
			DedupKey:    "msgid:v1:inbound@remote.example",
			EnvelopeKey: "env:v1:inbound-envelope",
		}},
		{"request-report-rejected-scan.cbor", reportRejectedScanRequest{
			ClamavSignature: "Win.Test.EICAR_HDB-1",
			RspamdScore: &RspamdScore{
				RawMilli: 9000, ScaledMilli: 8000,
				FlaggedRules: []string{"CLAM_VIRUS"},
				Breakdown:    []RspamdRuleContribution{{Rule: "CLAM_VIRUS", ScoreMilli: 9000}},
			},
			ReceivedAt: 1_700_000_003, SenderDomain: "evil.example",
		}},
		{"request-fetch-recipient-mls-pubkey.cbor", fetchRecipientMLSPubkeyRequest{ActorID: b32(0x33)}},
		{"request-fetch-recipient-index-key.cbor", fetchRecipientIndexKeyRequest{ActorID: b32(0x44)}},
		{"request-fetch-message-ciphertext.cbor", fetchMessageCiphertextRequest{
			ActorID: b32(0x55), MessageID: b32(0x66),
		}},
		{"request-fetch-index-segments-since.cbor", fetchIndexSegmentsSinceRequest{
			ActorID: b32(0x77), Mailbox: strPtr("INBOX"), SinceModseq: 5000, Limit: 100,
		}},
		{"request-list-mailboxes.cbor", listMailboxesRequest{
			ActorID: b32(0x11), SubscribedOnly: true,
		}},
		{"request-select-mailbox.cbor", selectMailboxRequest{
			ActorID: b32(0x22), Mailbox: "INBOX",
			ClientQresync: &QResyncHint{LastUIDValidity: 101, LastModseq: 4999},
			MuaID:         strPtr("Thunderbird/128"),
		}},
		{"request-list-messages.cbor", listMessagesRequest{
			ActorID: b32(0x33), Mailbox: "INBOX",
			SinceModseq: i64Ptr(5000), Limit: 50, AfterUID: u32Ptr(7),
		}},
		{"request-fetch-message-metadata.cbor", fetchMessageMetadataRequest{
			ActorID: b32(0x44), Mailbox: "INBOX", Uids: []uint32{1, 2, 3, 7},
		}},
		{"request-search-messages.cbor", searchMessagesRequest{
			ActorID: b32(0x55), Mailbox: "INBOX",
			// One of EACH SearchTerm variant so every variant's disjoint field
			// set is exercised in a single fixture (the SearchTerm enum is a
			// nested #[serde(tag="kind")] union — see the task brief).
			Terms: []SearchTerm{
				NewSearchTermHasFlag("\\Seen"),
				NewSearchTermLacksFlag("\\Flagged"),
				NewSearchTermHeaderContains(HeaderFieldSubject, "urgent"),
				NewSearchTermSinceInternalDate(1_700_000_000),
				NewSearchTermBeforeInternalDate(1_700_009_999),
				NewSearchTermLarger(1024),
				NewSearchTermSmaller(1_048_576),
			},
		}},
		{"request-get-quota.cbor", getQuotaRequest{ActorID: b32(0x66)}},
		{"request-store-flags.cbor", storeFlagsRequest{
			ActorID: b32(0x77), Mailbox: "INBOX", UIDs: []uint32{7, 9},
			Op: StoreFlagsOpAdd, Flags: []string{"\\Seen", "$label1"}, UnchangedSince: i64Ptr(5000),
		}},
		{"request-copy-messages.cbor", copyMessagesRequest{
			ActorID: b32(0x11), SourceMailbox: "INBOX", UIDs: []uint32{7, 9}, DestMailbox: "Archive",
		}},
		{"request-move-messages.cbor", moveMessagesRequest{
			ActorID: b32(0x22), SourceMailbox: "INBOX", UIDs: []uint32{8}, DestMailbox: "Trash",
		}},
		{"request-expunge.cbor", expungeRequest{
			ActorID: b32(0x33), Mailbox: "INBOX", UIDs: []uint32{3, 7},
		}},
		{"request-append.cbor", appendRequest{
			ActorID: b32(0x44), Mailbox: "Sent", Flags: []string{"\\Seen"},
			EncryptedBody: bytes.Repeat([]byte{0xab}, 64), EncryptedIndexHint: bytes.Repeat([]byte{0xcd}, 48),
			Timestamp: 1_700_000_004, CiphertextSize: 64, SenderDomain: "example.com",
			DedupKey: "msgid:v1:append@example.com", EnvelopeKey: "env:v1:append-envelope",
		}},
		{"request-fetch-outbound-due.cbor", fetchOutboundDueRequest{Max: 10, LeaseSeconds: 300}},
		{"request-mark-outbound-delivered.cbor", markOutboundDeliveredRequest{ID: 42}},
		{"request-mark-outbound-failed.cbor", markOutboundFailedRequest{
			ID: 43, RetryAfterSeconds: 600, LastError: "451 4.7.1 try later",
		}},
		{"request-mark-outbound-bounced.cbor", markOutboundBouncedRequest{
			ID: 44, Reason: "550 5.1.1 user unknown",
		}},
		{"request-enqueue-outbound-mail.cbor", enqueueOutboundMailRequest{
			OriginalMsgID: "<m1@example.com>", OriginalSender: "alice@example.com",
			Recipients: []string{"bob@remote.example", "carol@remote.example"},
			RawMessage: bytes.Repeat([]byte{0x77}, 128),
			// Populated non-nil so the MDA auto-schedule caller-scope field
			// (Option<Vec<u8>> on the Rust side) survives the round-trip.
			OnBehalfOfActor: bytesPtr(bytes.Repeat([]byte{0x42}, 32)),
		}},
		{"request-fetch-mta-sts-policy.cbor", fetchMtaStsPolicyRequest{Domain: "remote.example"}},
		{"request-fetch-tlsa.cbor", fetchTlsaRequest{MxHost: "mx1.remote.example"}},
		{"request-report-tls-attempt.cbor", reportTlsAttemptRequest{
			RecipientDomain: "remote.example", MxHost: "mx1.remote.example",
			ResultType:    strPtr("certificate-host-mismatch"),
			MtaStsOutcome: "found",
			MtaStsPolicy:  &MtaStsPolicyWire{ID: "20240101T000000", Mode: "enforce", Mx: []string{"mx1.remote.example", "mx2.remote.example"}, MaxAgeSecs: 604_800},
			TlsaRecords:   []TlsaRecordWire{{Usage: 3, Selector: 1, Matching: 1, Data: b32(0x88)}},
		}},
		{"request-fetch-recipient-forward-config.cbor", fetchRecipientForwardConfigRequest{ActorID: b32(0x11)}},
		{"request-fetch-recipient-filters.cbor", fetchRecipientFiltersRequest{ActorID: b32(0x22)}},
		{"request-forward-message.cbor", forwardMessageRequest{
			ActorID: b32(0x33), OriginalMsgID: "<m2@example.com>", OriginalSender: "alice@example.com",
			Destination: "external@elsewhere.example", RawMessage: bytes.Repeat([]byte{0x99}, 96),
			RuleIDOrForwardAll: "forward_all", CopyMode: ForwardCopyModeCopy,
		}},
		{"request-decode-srs-bounce.cbor", decodeSrsBounceRequest{
			LocalPart: "SRS0=abcd=tt=remote.example=bob",
		}},
		{"request-create-mailbox.cbor", createMailboxRequest{ActorID: b32(0x44), Name: "Projects"}},
		{"request-delete-mailbox.cbor", deleteMailboxRequest{ActorID: b32(0x55), Name: "Projects"}},
		{"request-rename-mailbox.cbor", renameMailboxRequest{
			ActorID: b32(0x66), OldName: "Projects", NewName: "Archive/Projects",
		}},
		{"request-subscribe-mailbox.cbor", subscribeMailboxRequest{ActorID: b32(0x77), Mailbox: "INBOX"}},
		{"request-unsubscribe-mailbox.cbor", unsubscribeMailboxRequest{ActorID: b32(0x11), Mailbox: "INBOX"}},
		{"request-subscribe-mailbox-state.cbor", subscribeMailboxStateRequest{ActorID: b32(0x22), Mailbox: "INBOX"}},
		{"request-provision-calendar.cbor", provisionCalendarRequest{
			ActorID: b32(0x33), CalendarID: b32(0x44),
			EncryptedMetadata: bytes.Repeat([]byte{0x55}, 48), UpdateMetadata: true,
		}},
		{"request-list-calendars.cbor", listCalendarsRequest{ActorID: b32(0x66)}},
		{"request-put-event-ciphertext.cbor", putEventCiphertextRequest{
			ActorID: b32(0x77), CalendarID: b32(0x11), UIDHash: b32(0x22),
			EncryptedBody: bytes.Repeat([]byte{0xab}, 64), EncryptedIndexHint: bytes.Repeat([]byte{0xcd}, 48),
			Timestamp: 1_700_000_005, CiphertextSize: 64, IfMatch: strPtr("etag-1"),
		}},
		{"request-delete-event.cbor", deleteEventRequest{
			ActorID: b32(0x33), CalendarID: b32(0x44), UIDHash: b32(0x55), IfMatch: strPtr("etag-2"),
		}},
		{"request-query-events.cbor", queryEventsRequest{
			ActorID: b32(0x66), CalendarID: b32(0x77),
			SinceModseq: i64Ptr(5000), AfterEventID: bytesPtr(b32(0x88)), Limit: 50,
		}},
		{"request-sync-calendar-since.cbor", syncCalendarSinceRequest{
			ActorID: b32(0x11), CalendarID: b32(0x22), SyncToken: "5000", Limit: 50,
			MuaID: strPtr("DAVx5/4.3"),
		}},
		{"request-report-log-events.cbor", reportLogEventsRequest{
			Events: []LogEvent{{
				TimestampMs: 1_700_000_000_123,
				Level:       "warn",
				Event:       "tls_cert_fetch_failed",
				Message:     "TLS cert fetch from nest failed; retrying on the backoff schedule",
			}},
			Dropped: 7,
		}},
	}
}

// requestConformanceTypes is the authoritative enumeration of every WS-RPC
// request body type — mirrors the "── request bodies ──" block of
// conformanceTypes in wsrpc_conformance_test.go. TestWsrpcRequestFixtureCoverage
// asserts each one has at least one fixture, so a new request type added
// without a fixture fails LOUDLY instead of leaving a silent coverage gap.
var requestConformanceTypes = []any{
	requestEnrollmentRequest{}, whoamiRequest{}, registerServiceUserRequest{},
	fetchTLSCertBlobRequest{}, fetchBridgePubkeyRequest{}, fetchConfigRequest{},
	reportAuthEventRequest{}, validateRecipientRequest{},
	checkGreylistRequest{}, fetchWrappedSubmissionTokenRequest{}, checkSubmissionQuotaRequest{},
	ingestInboundMailRequest{}, reportRejectedScanRequest{}, fetchRecipientMLSPubkeyRequest{},
	fetchRecipientIndexKeyRequest{}, fetchWrappedMLSBlobRequest{}, fetchMLSSnapshotBlobRequest{},
	reportSessionCloseRequest{}, listMailboxesRequest{}, selectMailboxRequest{},
	listMessagesRequest{}, fetchMessageMetadataRequest{}, fetchMessageCiphertextRequest{},
	fetchIndexSegmentsSinceRequest{}, searchMessagesRequest{}, getQuotaRequest{},
	storeFlagsRequest{}, copyMessagesRequest{}, moveMessagesRequest{},
	expungeRequest{}, appendRequest{},
	fetchOutboundDueRequest{}, markOutboundDeliveredRequest{}, markOutboundFailedRequest{},
	markOutboundBouncedRequest{}, enqueueOutboundMailRequest{}, fetchMtaStsPolicyRequest{},
	fetchTlsaRequest{}, reportTlsAttemptRequest{}, fetchRecipientForwardConfigRequest{},
	fetchRecipientFiltersRequest{}, forwardMessageRequest{}, decodeSrsBounceRequest{},
	createMailboxRequest{}, deleteMailboxRequest{}, renameMailboxRequest{},
	subscribeMailboxRequest{}, unsubscribeMailboxRequest{}, subscribeMailboxStateRequest{},
	provisionCalendarRequest{}, listCalendarsRequest{}, putEventCiphertextRequest{},
	deleteEventRequest{}, queryEventsRequest{}, syncCalendarSinceRequest{},
	reportLogEventsRequest{},
}

// TestRegenerateRequestFixtures (gated on REGEN_WSRPC_FIXTURES) marshals every
// fully-populated request body in requestFixtures() to its testdata fixture.
func TestRegenerateRequestFixtures(t *testing.T) {
	if os.Getenv("REGEN_WSRPC_FIXTURES") == "" {
		t.Skip("set REGEN_WSRPC_FIXTURES=1 to regenerate request-*.cbor")
	}
	written := 0
	for _, fx := range requestFixtures() {
		b, err := dagcbor.Marshal(fx.val)
		if err != nil {
			t.Fatalf("%s: dagcbor.Marshal(%T) failed: %v", fx.name, fx.val, err)
		}
		out := filepath.Join("testdata", fx.name)
		if err := os.WriteFile(out, b, 0o644); err != nil {
			t.Fatalf("%s: write: %v", fx.name, err)
		}
		t.Logf("wrote %s (%d bytes, %T)", fx.name, len(b), fx.val)
		written++
	}
	t.Logf("wrote %d request fixtures", written)
}

// TestWsrpcRequestFixtureCoverage fails if any request body type in
// requestConformanceTypes has no fixture in requestFixtures() whose value is
// that type. A new request type must ship with at least one fixture.
func TestWsrpcRequestFixtureCoverage(t *testing.T) {
	t.Parallel()
	covered := map[reflect.Type]bool{}
	for _, fx := range requestFixtures() {
		covered[reflect.TypeOf(fx.val)] = true
	}
	for _, v := range requestConformanceTypes {
		rt := reflect.TypeOf(v)
		if !covered[rt] {
			t.Errorf("request type %s has no fixture in requestFixtures() — add one to "+
				"wsrpc_request_fixtures_gen_test.go, regenerate (REGEN_WSRPC_FIXTURES=1), "+
				"and register the Rust mirror in wsrpc_request_cross_language.rs.", rt.Name())
		}
	}
}

// TestWsrpcRequestFixturesExist asserts every fixture named in requestFixtures()
// is committed under testdata/ (so a missing regen surfaces as a clear failure
// here rather than only failing the Rust consumer in another crate).
func TestWsrpcRequestFixturesExist(t *testing.T) {
	t.Parallel()
	for _, fx := range requestFixtures() {
		if _, err := os.Stat(filepath.Join("testdata", fx.name)); err != nil {
			t.Errorf("fixture %s missing: %v (run `REGEN_WSRPC_FIXTURES=1 go test -C "+
				"bins/fauna-bridges ./internal/wsrpc/ -run TestRegenerateRequestFixtures`)",
				fx.name, err)
		}
	}
}
