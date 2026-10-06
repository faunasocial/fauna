// Cross-language Rust→Go round-trip verifier for every WS-RPC *reply body*
// type the Go mail-bridge hand-mirrors from the Rust serde shapes in
// libs/fauna-protocol/src/{bridge_routing,wrapped_blob,email}.rs.
//
// # Why this exists
//
// methods.go mirrors each Rust reply type into a Go struct with `cbor:"..."`
// tags. A Rust-side field rename or type change silently breaks the mirror:
// the renamed wire key becomes unknown to the Go struct, is dropped on decode,
// and vanishes on re-encode — wire drift no Go→Go test can see.
//
// These tests close that gap with the same decode-then-re-encode byte-equality
// idiom as libs/fauna-protocol/tests/conformance.rs. For each committed fixture
// (produced by the Rust canonical encoder over a fully-populated instance):
//
//	got := roundTrip[GoMirror](fixtureBytes)  // Unmarshal[T] then Marshal
//	assert bytes.Equal(got, fixtureBytes)
//
// A Rust rename → the renamed key is unknown to the Go mirror → dropped on
// decode → absent on re-encode → bytes differ → RED, pointing at the regen
// command and the type. (Fixtures populate EVERY field with a non-zero value
// so omitempty Go fields survive the round-trip and a rename is observable.)
//
// FIXTURE REGEN (from the workspace root):
//
//	cargo run -p fauna-protocol --example regen_go_wsrpc_reply_fixtures
//
// then commit the updated testdata/reply-*.cbor files.
//
// Coordination boundary: like wsrpc_conformance_test.go, this is the COVERAGE
// half — do not edit codec.go / methods.go / methods_test.go here (that is
// separate, coordinated work). A genuine Rust↔Go drift surfaced here is a
// finding to report there, not a methods.go edit in this one.
package wsrpc

import (
	"bytes"
	"os"
	"path/filepath"
	"reflect"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
)

// roundTrip decodes b into T then re-encodes it, returning the re-encoded
// bytes. The production decode path (dagcbor.Unmarshal[T]) followed by the
// production encode path (dagcbor.Marshal) is exactly what a reply takes on
// the wire, so byte-inequality against the Rust-produced fixture is real drift.
func roundTrip[T any](b []byte) ([]byte, error) {
	v, err := dagcbor.Unmarshal[T](b)
	if err != nil {
		return nil, err
	}
	return dagcbor.Marshal(v)
}

// replyFixture binds one fixture file to its Go mirror type and the round-trip
// closure that exercises that type's decode+encode.
type replyFixture struct {
	typ reflect.Type
	rt  func([]byte) ([]byte, error)
}

// rf builds a replyFixture for the Go mirror type T.
func rf[T any]() replyFixture {
	var zero T
	return replyFixture{
		typ: reflect.TypeOf(zero),
		rt:  roundTrip[T],
	}
}

// replyFixtures maps each committed reply-*.cbor fixture to its Go mirror type.
// One entry per fixture; a `#[serde(tag = ...)]` Rust enum has one entry per
// variant (all variants share the same flat Go struct, so they map to the same
// Go type). Kept in sync with the Rust generator
// (libs/fauna-protocol/examples/regen_go_wsrpc_reply_fixtures.rs) and with the
// "── reply bodies ──" block of wsrpc_conformance_test.go's conformanceTypes.
var replyFixtures = map[string]replyFixture{
	// ── wrapped_blob.rs replies ──
	"reply-request-enrollment.cbor":             rf[requestEnrollmentReply](),
	"reply-register-service-user.cbor":          rf[registerServiceUserReply](),
	"reply-fetch-tls-cert-blob.cbor":            rf[fetchTLSCertBlobReply](),
	"reply-fetch-bridge-pubkey.cbor":            rf[fetchBridgePubkeyReply](),
	"reply-report-auth-event.cbor":              rf[reportAuthEventReply](),
	"reply-fetch-wrapped-submission-token.cbor": rf[fetchWrappedSubmissionTokenReply](),
	"reply-fetch-wrapped-mls-blob.cbor":         rf[fetchWrappedMLSBlobReply](),
	"reply-fetch-mls-snapshot-blob.cbor":        rf[fetchMLSSnapshotBlobReply](),

	// ── bridge_routing.rs replies ──
	"reply-whoami.cbor":               rf[WhoamiReply](),
	"reply-report-session-close.cbor": rf[reportSessionCloseReply](),

	"reply-validate-recipient-resolved.cbor": rf[validateRecipientReply](),
	"reply-validate-recipient-reject.cbor":   rf[validateRecipientReply](),

	"reply-check-greylist.cbor": rf[checkGreylistReply](),

	"reply-check-submission-quota-allowed.cbor":    rf[checkSubmissionQuotaReply](),
	"reply-check-submission-quota-over-quota.cbor": rf[checkSubmissionQuotaReply](),

	"reply-fetch-config.cbor": rf[ConfigSnapshot](),

	"reply-ingest-inbound-mail.cbor":  rf[ingestInboundMailReply](),
	"reply-report-rejected-scan.cbor": rf[reportRejectedScanReply](),

	"reply-fetch-recipient-mls-pubkey.cbor": rf[fetchRecipientMLSPubkeyReply](),
	"reply-fetch-recipient-index-key.cbor":  rf[fetchRecipientIndexKeyReply](),

	"reply-fetch-message-ciphertext-found.cbor":     rf[fetchMessageCiphertextReply](),
	"reply-fetch-message-ciphertext-not-found.cbor": rf[fetchMessageCiphertextReply](),

	"reply-list-mailboxes.cbor": rf[listMailboxesReply](),

	"reply-select-mailbox-selected.cbor":        rf[selectMailboxReply](),
	"reply-select-mailbox-no-such-mailbox.cbor": rf[selectMailboxReply](),

	"reply-list-messages.cbor":              rf[ListMessagesReply](),
	"reply-fetch-message-metadata.cbor":     rf[fetchMessageMetadataReply](),
	"reply-fetch-index-segments-since.cbor": rf[FetchIndexSegmentsSinceReply](),
	"reply-search-messages.cbor":            rf[searchMessagesReply](),
	"reply-get-quota.cbor":                  rf[GetQuotaReply](),
	"reply-store-flags.cbor":                rf[StoreFlagsReply](),
	"reply-copy-messages.cbor":              rf[CopyMessagesReply](),
	"reply-move-messages.cbor":              rf[MoveMessagesReply](),
	"reply-expunge.cbor":                    rf[ExpungeReply](),
	"reply-append.cbor":                     rf[AppendReply](),

	"reply-fetch-outbound-due.cbor":      rf[fetchOutboundDueReply](),
	"reply-mark-outbound-delivered.cbor": rf[markOutboundDeliveredReply](),
	"reply-mark-outbound-failed.cbor":    rf[markOutboundFailedReply](),
	"reply-mark-outbound-bounced.cbor":   rf[markOutboundBouncedReply](),
	"reply-enqueue-outbound-mail.cbor":   rf[enqueueOutboundMailReply](),

	"reply-fetch-mta-sts-policy.cbor": rf[FetchMtaStsPolicyReply](),
	"reply-fetch-tlsa.cbor":           rf[FetchTlsaReply](),
	"reply-report-tls-attempt.cbor":   rf[reportTlsAttemptReply](),

	"reply-fetch-recipient-forward-config.cbor": rf[fetchRecipientForwardConfigReply](),
	"reply-fetch-recipient-filters.cbor":        rf[fetchRecipientFiltersReply](),
	"reply-forward-message.cbor":                rf[forwardMessageReply](),
	"reply-decode-srs-bounce.cbor":              rf[decodeSrsBounceReply](),

	"reply-create-mailbox-created.cbor":        rf[createMailboxReply](),
	"reply-create-mailbox-already-exists.cbor": rf[createMailboxReply](),
	"reply-create-mailbox-reserved.cbor":       rf[createMailboxReply](),
	"reply-create-mailbox-invalid-name.cbor":   rf[createMailboxReply](),

	"reply-delete-mailbox-deleted.cbor":         rf[deleteMailboxReply](),
	"reply-delete-mailbox-no-such-mailbox.cbor": rf[deleteMailboxReply](),
	"reply-delete-mailbox-reserved.cbor":        rf[deleteMailboxReply](),
	"reply-delete-mailbox-not-empty.cbor":       rf[deleteMailboxReply](),

	"reply-rename-mailbox-renamed.cbor":         rf[renameMailboxReply](),
	"reply-rename-mailbox-no-such-source.cbor":  rf[renameMailboxReply](),
	"reply-rename-mailbox-reserved-source.cbor": rf[renameMailboxReply](),
	"reply-rename-mailbox-target-reserved.cbor": rf[renameMailboxReply](),
	"reply-rename-mailbox-target-exists.cbor":   rf[renameMailboxReply](),
	"reply-rename-mailbox-invalid-name.cbor":    rf[renameMailboxReply](),

	"reply-subscribe-mailbox-subscribed.cbor":       rf[subscribeMailboxReply](),
	"reply-unsubscribe-mailbox-unsubscribed.cbor":   rf[unsubscribeMailboxReply](),
	"reply-subscribe-mailbox-state-subscribed.cbor": rf[subscribeMailboxStateReply](),

	"reply-provision-calendar-created.cbor":        rf[provisionCalendarReply](),
	"reply-provision-calendar-already-exists.cbor": rf[provisionCalendarReply](),
	"reply-provision-calendar-conflict.cbor":       rf[provisionCalendarReply](),
	"reply-provision-calendar-updated.cbor":        rf[provisionCalendarReply](),
	"reply-provision-calendar-not-found.cbor":      rf[provisionCalendarReply](),

	"reply-list-calendars.cbor": rf[listCalendarsReply](),

	"reply-put-event-ciphertext-created.cbor":             rf[putEventCiphertextReply](),
	"reply-put-event-ciphertext-updated.cbor":             rf[putEventCiphertextReply](),
	"reply-put-event-ciphertext-precondition-failed.cbor": rf[putEventCiphertextReply](),
	"reply-put-event-ciphertext-calendar-not-found.cbor":  rf[putEventCiphertextReply](),

	"reply-delete-event-deleted.cbor":             rf[deleteEventReply](),
	"reply-delete-event-not-found.cbor":           rf[deleteEventReply](),
	"reply-delete-event-precondition-failed.cbor": rf[deleteEventReply](),

	"reply-query-events-ok.cbor":                 rf[queryEventsReply](),
	"reply-query-events-calendar-not-found.cbor": rf[queryEventsReply](),

	"reply-sync-calendar-since-ok.cbor":                 rf[syncCalendarSinceReply](),
	"reply-sync-calendar-since-calendar-not-found.cbor": rf[syncCalendarSinceReply](),
	"reply-sync-calendar-since-stale.cbor":              rf[syncCalendarSinceReply](),
}

// TestWsrpcReplyCrossLanguage round-trips every committed reply fixture through
// its Go mirror type and asserts the re-encoded bytes equal the Rust-produced
// fixture byte-for-byte.
func TestWsrpcReplyCrossLanguage(t *testing.T) {
	t.Parallel()
	for name, fx := range replyFixtures {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			b, err := os.ReadFile(filepath.Join("testdata", name))
			if err != nil {
				t.Fatalf("read fixture %s: %v (run `cargo run -p fauna-protocol "+
					"--example regen_go_wsrpc_reply_fixtures` to regenerate)", name, err)
			}
			got, err := fx.rt(b)
			if err != nil {
				t.Fatalf("%s: round-trip (decode+re-encode) as %s failed: %v",
					name, fx.typ.Name(), err)
			}
			if !bytes.Equal(got, b) {
				t.Errorf("%s: re-encoded bytes differ from the Rust-produced fixture for "+
					"Go type %s.\nThis means the Go mirror's cbor shape no longer matches the "+
					"Rust serde shape (a field rename / type change / dropped key).\n"+
					" fixture (% x)\n re-encoded (% x)\n"+
					"If the Rust type changed intentionally, update the Go mirror in methods.go "+
					"and regenerate: "+
					"`cargo run -p fauna-protocol --example regen_go_wsrpc_reply_fixtures`.",
					name, fx.typ.Name(), b, got)
			}
		})
	}
}

// replyConformanceTypes is the authoritative enumeration of every WS-RPC reply
// body type — mirrors the "── reply bodies ──" block of conformanceTypes in
// wsrpc_conformance_test.go. TestWsrpcReplyFixtureCoverage asserts each one has
// at least one fixture in replyFixtures, so a new reply type added without a
// fixture fails LOUDLY instead of leaving a silent coverage gap.
var replyConformanceTypes = []any{
	requestEnrollmentReply{}, WhoamiReply{}, registerServiceUserReply{},
	fetchTLSCertBlobReply{}, fetchBridgePubkeyReply{}, ConfigSnapshot{},
	reportAuthEventReply{}, validateRecipientReply{},
	checkGreylistReply{}, fetchWrappedSubmissionTokenReply{}, checkSubmissionQuotaReply{},
	ingestInboundMailReply{}, reportRejectedScanReply{}, fetchRecipientMLSPubkeyReply{},
	fetchRecipientIndexKeyReply{}, fetchWrappedMLSBlobReply{}, fetchMLSSnapshotBlobReply{},
	reportSessionCloseReply{}, listMailboxesReply{}, selectMailboxReply{},
	ListMessagesReply{}, fetchMessageMetadataReply{}, fetchMessageCiphertextReply{},
	FetchIndexSegmentsSinceReply{}, searchMessagesReply{}, GetQuotaReply{},
	StoreFlagsReply{}, CopyMessagesReply{}, MoveMessagesReply{},
	ExpungeReply{}, AppendReply{},
	fetchOutboundDueReply{}, markOutboundDeliveredReply{}, markOutboundFailedReply{},
	markOutboundBouncedReply{}, enqueueOutboundMailReply{}, FetchMtaStsPolicyReply{},
	FetchTlsaReply{}, reportTlsAttemptReply{}, fetchRecipientForwardConfigReply{},
	fetchRecipientFiltersReply{}, forwardMessageReply{}, decodeSrsBounceReply{},
	createMailboxReply{}, deleteMailboxReply{}, renameMailboxReply{},
	subscribeMailboxReply{}, unsubscribeMailboxReply{}, subscribeMailboxStateReply{},
	provisionCalendarReply{}, listCalendarsReply{}, putEventCiphertextReply{},
	deleteEventReply{}, queryEventsReply{}, syncCalendarSinceReply{},
}

// TestWsrpcReplyFixtureCoverage fails if any reply body type in
// replyConformanceTypes has no fixture in replyFixtures whose Go mirror type
// matches it. A new reply type must ship with at least one fixture.
func TestWsrpcReplyFixtureCoverage(t *testing.T) {
	t.Parallel()
	covered := map[reflect.Type]bool{}
	for _, fx := range replyFixtures {
		covered[fx.typ] = true
	}
	for _, v := range replyConformanceTypes {
		rt := reflect.TypeOf(v)
		if !covered[rt] {
			t.Errorf("reply type %s has no fixture in replyFixtures — add a fixture to "+
				"libs/fauna-protocol/examples/regen_go_wsrpc_reply_fixtures.rs, regenerate, "+
				"and register it (and one per `#[serde(tag)]` enum variant).", rt.Name())
		}
	}
}
