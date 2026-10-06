// Tests for the per-account "forward all" stage of the inbound pipeline
// (mail-forwarding N2-go, docs/goal/behavior/mail-forwarding.md § Trigger
// point / § Loop detection). These drive the full Data() flow in-process with
// a mockCaller (tier_1 — no nest binary, no driver), asserting which inbound
// messages produce a forward_message enqueue and which are suppressed while
// still completing local delivery.
package mta

import (
	"bytes"
	"context"
	"encoding/hex"
	"log/slog"
	"strings"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// forwardingSession builds an inboundSession wired to mc with a single
// resolved recipient that has an MLS pubkey (so ingest succeeds) and the
// given non-null sender. Mirrors TestInboundBackendDataIngestsAfterFullPipeline.
func forwardingSession(t *testing.T, mc *mockCaller, actorID []byte, from string) *inboundSession {
	t.Helper()
	mc.mlsPubkeys[hex.EncodeToString(actorID)] = freshX25519Pubkey(t)
	return &inboundSession{
		logger:          slog.Default(),
		clientIP:        "127.0.0.1",
		from:            from,
		inboundRcpts:    []resolvedInboundRcpt{{actorID: actorID}},
		caller:          mc,
		localDomains:    []string{"test.example"},
		maxMessageBytes: 50_000_000,
		spamPolicy:      permissiveSpamPolicyForTest(),
	}
}

// TestForwardAllEnqueuesForwardMessage pins the happy path: a recipient with
// forward-all set receives the inbound locally AND a forward_message enqueue
// fires with the configured destination, copy_mode=copy (forward-all always
// keeps the local copy, :32), and the X-Fauna-Forwarded-By stamp prepended.
func TestForwardAllEnqueuesForwardMessage(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actorBob, "alice@example.org")
	mc.forwardConfigs[hex.EncodeToString(actorBob)] = "bob-personal@example.net"

	body := "From: alice@example.org\r\n" +
		"To: bob@test.example\r\n" +
		"Message-ID: <msg-42@example.org>\r\n" +
		"Subject: forward me\r\n" +
		"\r\n" +
		"hello\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}

	// Local delivery still happened.
	if got := len(mc.ingestRequests); got != 1 {
		t.Fatalf("ingest count: got %d, want 1", got)
	}
	if got := len(mc.forwardRequests); got != 1 {
		t.Fatalf("forward count: got %d, want 1; reqs=%+v", got, mc.forwardRequests)
	}
	fr := mc.forwardRequests[0]
	if !bytes.Equal(fr.actorID, actorBob) {
		t.Errorf("forward actor_id: got %x, want %x", fr.actorID, actorBob)
	}
	if fr.destination != "bob-personal@example.net" {
		t.Errorf("destination: got %q", fr.destination)
	}
	if fr.originalSender != "alice@example.org" {
		t.Errorf("original_sender: got %q", fr.originalSender)
	}
	// The RFC 5322 parser returns the Message-ID without angle brackets.
	if fr.originalMsgID != "msg-42@example.org" {
		t.Errorf("original_msgid: got %q, want the source Message-ID", fr.originalMsgID)
	}
	if fr.ruleIDOrForwardAll != "forward-all" {
		t.Errorf("rule: got %q, want forward-all", fr.ruleIDOrForwardAll)
	}
	if fr.copyMode != "copy" {
		t.Errorf("copy_mode: got %q, want copy", fr.copyMode)
	}
	// The forwarded copy carries our loop-detection stamp with this actor.
	stamp := "X-Fauna-Forwarded-By:"
	if !bytes.Contains(fr.rawMessage, []byte(stamp)) {
		t.Errorf("forwarded body missing %s header", stamp)
	}
	if !bytes.Contains(fr.rawMessage, []byte("actor="+hex.EncodeToString(actorBob))) {
		t.Errorf("forwarded stamp missing our actor id")
	}
	if !bytes.Contains(fr.rawMessage, []byte("rule=forward-all")) {
		t.Errorf("forwarded stamp missing rule=forward-all")
	}
}

// TestForwardAllFallsBackToNestMsgIDWhenNoMessageID covers the original_msgid
// fallback: a message with no RFC 5322 Message-ID still forwards (the nest
// hard-requires a non-empty original_msgid), keyed on the nest-assigned id.
func TestForwardAllFallsBackToNestMsgIDWhenNoMessageID(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actorBob, "alice@example.org")
	mc.forwardConfigs[hex.EncodeToString(actorBob)] = "bob@example.net"

	body := "From: alice@example.org\r\n" +
		"To: bob@test.example\r\n" +
		"Subject: no message-id\r\n" +
		"\r\n" +
		"hello\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.forwardRequests); got != 1 {
		t.Fatalf("forward count: got %d, want 1", got)
	}
	if got := mc.forwardRequests[0].originalMsgID; got == "" {
		t.Errorf("original_msgid must be non-empty (nest rejects empty); got %q", got)
	}
}

// TestForwardSuppressedSelfSeen: an inbound that already carries our own
// X-Fauna-Forwarded-By is local-delivered but NOT re-forwarded (:146).
func TestForwardSuppressedSelfSeen(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actorBob, "alice@example.org")
	mc.forwardConfigs[hex.EncodeToString(actorBob)] = "bob@example.net"

	body := "From: alice@example.org\r\n" +
		"To: bob@test.example\r\n" +
		"X-Fauna-Forwarded-By: actor=" + hex.EncodeToString(actorBob) + "; t=1; rule=forward-all\r\n" +
		"Subject: loop\r\n" +
		"\r\n" +
		"hello\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.ingestRequests); got != 1 {
		t.Errorf("local delivery must still happen; ingest count got %d, want 1", got)
	}
	if got := len(mc.forwardRequests); got != 0 {
		t.Errorf("self-seen forward must be suppressed; got %d forwards", got)
	}
}

// TestForwardNotSuppressedPeerForwardedBy: a peer's DIFFERENT actor= does not
// suppress — we forward through, accreting the chain (:148).
func TestForwardNotSuppressedPeerForwardedBy(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actorBob, "alice@example.org")
	mc.forwardConfigs[hex.EncodeToString(actorBob)] = "bob@example.net"

	peer := strings.Repeat("99", 32)
	body := "From: alice@example.org\r\n" +
		"To: bob@test.example\r\n" +
		"X-Fauna-Forwarded-By: actor=" + peer + "; t=1; rule=forward-all\r\n" +
		"Subject: peer hop\r\n" +
		"\r\n" +
		"hello\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.forwardRequests); got != 1 {
		t.Errorf("peer forwarded-by must NOT suppress; got %d forwards, want 1", got)
	}
}

// TestForwardSuppressedReceivedChain: >10 Received: hops suppress the forward
// while local delivery completes (:134).
func TestForwardSuppressedReceivedChain(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actorBob, "alice@example.org")
	mc.forwardConfigs[hex.EncodeToString(actorBob)] = "bob@example.net"

	var hdrs strings.Builder
	hdrs.WriteString("From: alice@example.org\r\n")
	hdrs.WriteString("To: bob@test.example\r\n")
	for i := 0; i < 11; i++ { // 11 > MAX_RECEIVED_HOPS (10)
		hdrs.WriteString("Received: from h" + string(rune('a'+i)) + ".example by us\r\n")
	}
	hdrs.WriteString("Subject: long chain\r\n\r\nhello\r\n")
	if err := s.Data(bytes.NewReader([]byte(hdrs.String()))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.ingestRequests); got != 1 {
		t.Errorf("local delivery must still happen; ingest count got %d, want 1", got)
	}
	if got := len(mc.forwardRequests); got != 0 {
		t.Errorf("received-chain forward must be suppressed; got %d forwards", got)
	}
}

// TestForwardSkippedNullSender: forwards never run on MAIL FROM: <> (:255) —
// the forward-config fetch isn't even attempted.
func TestForwardSkippedNullSender(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actorBob, "") // null sender
	mc.forwardConfigs[hex.EncodeToString(actorBob)] = "bob@example.net"

	body := "From: postmaster@example.org\r\n" +
		"To: bob@test.example\r\n" +
		"Subject: bounce\r\n" +
		"\r\n" +
		"delivery failed\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.forwardRequests); got != 0 {
		t.Errorf("null-sender forward must be skipped; got %d forwards", got)
	}
	if mc.forwardConfigFetches != 0 {
		t.Errorf("null-sender must not even fetch forward config; got %d fetches", mc.forwardConfigFetches)
	}
}

// TestForwardDisabledNoForward: a recipient with no forward-all config is
// local-delivered with no forward_message call.
func TestForwardDisabledNoForward(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actorBob, "alice@example.org")
	// forwardConfigs left empty → disabled.

	body := "From: alice@example.org\r\n" +
		"To: bob@test.example\r\n" +
		"Subject: no forward\r\n" +
		"\r\n" +
		"hello\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.ingestRequests); got != 1 {
		t.Errorf("ingest count got %d, want 1", got)
	}
	if got := len(mc.forwardRequests); got != 0 {
		t.Errorf("disabled forward-all must not forward; got %d", got)
	}
}

// TestForwardAllNudgesOutboundWorker: a live forward pokes the outbound worker
// (mail-forwarding N4 latency polish) so the forward delivers on the next poll
// instead of waiting the full PollInterval; a suppressed/disabled forward does
// not nudge (there's no row to drain).
func TestForwardAllNudgesOutboundWorker(t *testing.T) {
	t.Parallel()
	actorBob := bytes.Repeat([]byte{0x42}, 32)

	// Live forward → exactly one nudge.
	mc := newMockCaller()
	s := forwardingSession(t, mc, actorBob, "alice@example.org")
	mc.forwardConfigs[hex.EncodeToString(actorBob)] = "bob@example.net"
	nudges := 0
	s.outboundTrigger = func() { nudges++ }
	body := "From: alice@example.org\r\n" +
		"To: bob@test.example\r\n" +
		"Subject: nudge\r\n\r\nhello\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if len(mc.forwardRequests) != 1 {
		t.Fatalf("forward count: got %d, want 1", len(mc.forwardRequests))
	}
	if nudges != 1 {
		t.Errorf("live forward must nudge the worker once; got %d", nudges)
	}

	// Disabled forward-all → no enqueue → no nudge.
	mc2 := newMockCaller()
	s2 := forwardingSession(t, mc2, actorBob, "alice@example.org")
	nudges2 := 0
	s2.outboundTrigger = func() { nudges2++ }
	if err := s2.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if nudges2 != 0 {
		t.Errorf("disabled forward-all must not nudge; got %d", nudges2)
	}
}

// TestForwardSuppressedTooLarge pins the interim size guard (smtp-server.md
// § Message size limits, the staged-envelope rule): a body over the inline
// WS-RPC request budget rides forward_message inline and could only be
// refused at the 2 MiB frame, so the dispatch suppresses the doomed attempt
// with an explicit verdict — no forward RPC fires. The guard is lifted when
// the outbound staged-envelope leg lands.
func TestForwardSuppressedTooLarge(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	big := bytes.Repeat([]byte{'A'}, int(mailfauna.InlineMailRequestBudgetBytes())+1)
	dispatchForward(
		context.Background(), mc, slog.Default(), nil,
		"127.0.0.1", "alice@example.org",
		nil, // headers: no loop floors trip
		big, actorBob,
		"bob-personal@example.net", "<msg-42@example.org>", "forward-all",
		wsrpc.ForwardCopyModeCopy,
	)
	if got := len(mc.forwardRequests); got != 0 {
		t.Fatalf("over-budget forward must be suppressed before the RPC; got %d requests", got)
	}
}

// TestForwardStillFiresJustUnderBudget is the companion: the size guard must
// not suppress a forward the inline leg can actually carry.
func TestForwardStillFiresJustUnderBudget(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	// Comfortably under the budget (the stamp header adds a few hundred bytes).
	raw := bytes.Repeat([]byte{'A'}, 1024)
	dispatchForward(
		context.Background(), mc, slog.Default(), nil,
		"127.0.0.1", "alice@example.org",
		nil,
		raw, actorBob,
		"bob-personal@example.net", "<msg-43@example.org>", "forward-all",
		wsrpc.ForwardCopyModeCopy,
	)
	if got := len(mc.forwardRequests); got != 1 {
		t.Fatalf("under-budget forward must fire; got %d requests", got)
	}
}
