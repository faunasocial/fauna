// Tests for the resolve_recipient RCPT-TO cutover (mail-forwarding): the inbound MTA now resolves a RCPT via fauna.bridges.resolve_recipient
// (the fixed-order resolver, mail-aliases.md § Resolution order) instead of the
// exact-only validate_recipient. This covers the three new behaviours the cutover
// introduces — the admin external-forwarder Forward outcome (redirect dispatch,
// no local copy), the per-recipient X-Fauna-Address-* header stamping reaching the
// recipient's filter context, and the leak guard that keeps those stamps off the
// external forward copy. All tier_1 (in-process Data()/Rcpt() with a mockCaller).
package mta

import (
	"bytes"
	"encoding/hex"
	"errors"
	"log/slog"
	"testing"
	"time"

	gosmtp "github.com/emersion/go-smtp"
	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

func mustCBOR(t *testing.T, v any) cbor.RawMessage {
	t.Helper()
	b, err := cbor.Marshal(v)
	if err != nil {
		t.Fatalf("cbor marshal: %v", err)
	}
	return b
}

// forwarderSession builds an inboundSession whose single RCPT resolved to an
// admin external forwarder (the Forward outcome): no local mailbox, redirect to
// `target`, attributed to `admin`.
func forwarderSession(mc *mockCaller, admin []byte, target, from string) *inboundSession {
	return &inboundSession{
		logger:   slog.Default(),
		clientIP: "127.0.0.1",
		from:     from,
		inboundRcpts: []resolvedInboundRcpt{{
			forward:          true,
			forwardTarget:    target,
			forwarderActorID: admin,
			rcptAddr:         "info@test.example",
		}},
		caller:          mc,
		localDomains:    []string{"test.example"},
		maxMessageBytes: 50_000_000,
		spamPolicy:      permissiveSpamPolicyForTest(),
	}
}

// TestResolveForwardRedirectsExternalNoLocalCopy: an admin external forwarder
// (mail-aliases.md § Kind 7) redirects the inbound to its external target —
// copy_mode=redirect, attributed to the managing admin, with NO local mailbox
// write (the AF-dispatch deliverable). The SRS rewrite is nest-side at queue-out,
// so this asserts the enqueue shape, not the on-wire envelope.
func TestResolveForwardRedirectsExternalNoLocalCopy(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	admin := bytes.Repeat([]byte{0x07}, 32)
	s := forwarderSession(mc, admin, "oldaccount@example.net", "alice@example.org")

	body := "From: alice@example.org\r\n" +
		"To: info@test.example\r\n" +
		"Message-ID: <m1@example.org>\r\n" +
		"Subject: contact us\r\n" +
		"\r\n" +
		"hello\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}

	// No local copy is kept (redirect-shaped, mail-forwarding.md:59).
	if got := len(mc.ingestRequests); got != 0 {
		t.Errorf("forwarder must write NO local copy; got %d ingest calls", got)
	}
	if got := len(mc.forwardRequests); got != 1 {
		t.Fatalf("forward count: got %d, want 1; reqs=%+v", got, mc.forwardRequests)
	}
	fr := mc.forwardRequests[0]
	if !bytes.Equal(fr.actorID, admin) {
		t.Errorf("forward attributed to managing admin: got %x, want %x", fr.actorID, admin)
	}
	if fr.destination != "oldaccount@example.net" {
		t.Errorf("destination: got %q", fr.destination)
	}
	if fr.copyMode != "redirect" {
		t.Errorf("copy_mode: got %q, want redirect", fr.copyMode)
	}
	if fr.ruleIDOrForwardAll != "forwarder" {
		t.Errorf("rule: got %q, want forwarder", fr.ruleIDOrForwardAll)
	}
	if fr.originalSender != "alice@example.org" {
		t.Errorf("original_sender: got %q", fr.originalSender)
	}
	if fr.originalMsgID != "m1@example.org" {
		t.Errorf("original_msgid: got %q, want the source Message-ID", fr.originalMsgID)
	}
	// The forwarded copy carries our loop-detection stamp with the admin actor.
	if !bytes.Contains(fr.rawMessage, []byte("actor="+hex.EncodeToString(admin))) {
		t.Errorf("forwarded stamp missing the admin actor id")
	}
	if !bytes.Contains(fr.rawMessage, []byte("rule=forwarder")) {
		t.Errorf("forwarded stamp missing rule=forwarder")
	}
}

// TestResolveForwardNoMessageIDFallsBackToQueueID: a forwarder with no source
// Message-ID still forwards with a non-empty original_msgid (nest rejects an
// empty one). There is no local ingest id to fall back to, so a fresh queue id
// is used.
func TestResolveForwardNoMessageIDFallsBackToQueueID(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	admin := bytes.Repeat([]byte{0x07}, 32)
	s := forwarderSession(mc, admin, "oldaccount@example.net", "alice@example.org")

	body := "From: alice@example.org\r\n" +
		"To: info@test.example\r\n" +
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

// TestForwarderNullSenderDropped: a null-sender (bounce) to a forward-only
// address forwards nothing (:255) and has no local mailbox — accepted at RCPT,
// dropped at DATA.
func TestForwarderNullSenderDropped(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	admin := bytes.Repeat([]byte{0x07}, 32)
	s := forwarderSession(mc, admin, "oldaccount@example.net", "") // null sender

	body := "From: mailer-daemon@example.org\r\n" +
		"To: info@test.example\r\n" +
		"Subject: bounce\r\n" +
		"\r\n" +
		"delivery failed\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.forwardRequests); got != 0 {
		t.Errorf("null-sender forwarder must not forward; got %d", got)
	}
	if got := len(mc.ingestRequests); got != 0 {
		t.Errorf("forwarder writes no local copy; got %d ingests", got)
	}
}

// errForwardRefused stands in for nest refusing a `redirect` forward it cannot
// park without evicting the only copy of other accepted mail (mail-forwarding.md
// § Queue ceiling) — or any failure that leaves the forward un-enqueued.
var errForwardRefused = errors.New("forward_message: forward queue full")

// wantTempfail451 asserts err is a 451 SMTPError.
func wantTempfail451(t *testing.T, err error) {
	t.Helper()
	var smtpErr *gosmtp.SMTPError
	if !errors.As(err, &smtpErr) {
		t.Fatalf("expected a 451 *gosmtp.SMTPError, got %T: %v", err, err)
	}
	if smtpErr.Code != 451 {
		t.Fatalf("code: got %d, want 451 (%v)", smtpErr.Code, smtpErr)
	}
}

// TestForwarderEnqueueRefusedTempfails: an admin external forwarder keeps no
// local copy, so a forward nest did not durably enqueue — refused over the
// queue ceiling, or unreachable — must NOT be answered 250: that would be
// accepted mail with no copy anywhere. A 451 hands the message back to the
// sending MTA to retry (mail-forwarding.md § Queue ceiling).
func TestForwarderEnqueueRefusedTempfails(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	mc.forwardMessageErr = errForwardRefused
	admin := bytes.Repeat([]byte{0x07}, 32)
	s := forwarderSession(mc, admin, "oldaccount@example.net", "alice@example.org")

	body := "From: alice@example.org\r\n" +
		"To: info@test.example\r\n" +
		"Message-ID: <m-refused@example.org>\r\n" +
		"Subject: contact us\r\n" +
		"\r\n" +
		"hello\r\n"
	wantTempfail451(t, s.Data(bytes.NewReader([]byte(body))))
	if got := len(mc.forwardRequests); got != 1 {
		t.Fatalf("forward attempts: got %d, want 1", got)
	}
}

// TestForwarderRefusedInMixedEnvelopeCommitsNothingLocally: DATA carries ONE
// reply for the whole envelope, so the forwarder's 451 must be decided before
// any local recipient commits — otherwise the sender's retry re-delivers to the
// local mailbox (a duplicate). Forwarders dispatch first; the local recipient,
// listed FIRST in the envelope, is never ingested on the refused attempt.
func TestForwarderRefusedInMixedEnvelopeCommitsNothingLocally(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	mc.forwardMessageErr = errForwardRefused
	admin := bytes.Repeat([]byte{0x07}, 32)
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	s := forwarderSession(mc, admin, "oldaccount@example.net", "alice@example.org")
	mc.mlsPubkeys[hex.EncodeToString(actorBob)] = freshX25519Pubkey(t)
	s.inboundRcpts = append([]resolvedInboundRcpt{{actorID: actorBob, rcptAddr: "bob@test.example"}},
		s.inboundRcpts...)

	body := "From: alice@example.org\r\n" +
		"To: bob@test.example, info@test.example\r\n" +
		"Message-ID: <m-mixed@example.org>\r\n" +
		"Subject: contact us\r\n" +
		"\r\n" +
		"hello\r\n"
	wantTempfail451(t, s.Data(bytes.NewReader([]byte(body))))
	if got := len(mc.ingestRequests); got != 0 {
		t.Errorf("a refused forwarder must 451 before any local commit (else the retry duplicates); got %d ingests", got)
	}

	// The sender's retry, with nest now taking the forward, delivers both.
	mc.forwardMessageErr = nil
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("retry: %v", err)
	}
	if got := len(mc.ingestRequests); got != 1 {
		t.Errorf("retry: local recipient ingests: got %d, want 1", got)
	}
}

// TestForwarderLoopSuppressedStillAccepted: the 451 is for a forward nest did
// not take, never for one a floor deliberately suppressed — a looping forwarder
// message is dropped as before (§ Loop detection), not retried forever.
func TestForwarderLoopSuppressedStillAccepted(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	admin := bytes.Repeat([]byte{0x07}, 32)
	s := forwarderSession(mc, admin, "oldaccount@example.net", "alice@example.org")

	body := "X-Fauna-Forwarded-By: actor=" + hex.EncodeToString(admin) + "; t=1; rule=forwarder\r\n" +
		"From: alice@example.org\r\n" +
		"To: info@test.example\r\n" +
		"Subject: loop\r\n" +
		"\r\n" +
		"hello\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("a loop-suppressed forwarder is accepted, not tempfailed: %v", err)
	}
	if got := len(mc.forwardRequests); got != 0 {
		t.Errorf("self-seen loop must suppress the forward; got %d", got)
	}
}

// TestStampedAliasHeaderReachesFilterContext is the load-bearing cutover guard:
// a subaddress / wildcard / disposable / catch-all match stamps an
// X-Fauna-Address-* header (aliases/mod.rs:55), and the recipient's filter rules
// must be able to match on it. Proven with a Discard rule keyed on
// X-Fauna-Address-Suffix: when the resolver stamps `work`, the rule fires and the
// recipient is dropped (no ingest); without the stamp the same rule can't match
// and the recipient is delivered.
func TestStampedAliasHeaderReachesFilterContext(t *testing.T) {
	t.Parallel()
	actor := bytes.Repeat([]byte{0x42}, 32)
	body := "From: alice@example.org\r\n" +
		"To: bob+work@test.example\r\n" +
		"Subject: subaddress\r\n" +
		"\r\n" +
		"hello\r\n"
	// A rule that discards anything whose X-Fauna-Address-Suffix contains "work".
	dropWorkRule := []wsrpc.EmailFilterWire{{
		ID:          1,
		Name:        "drop work subaddress",
		Combination: "All",
		Rules: []cbor.RawMessage{mustCBOR(t, map[string]any{
			"HeaderContains": map[string]any{"name": "X-Fauna-Address-Suffix", "value": "work"},
		})},
		Action: mustCBOR(t, "Discard"),
	}}

	// With the resolver's stamp present, the rule matches → recipient dropped.
	mcStamped := newMockCaller()
	mcStamped.mlsPubkeys[hex.EncodeToString(actor)] = freshX25519Pubkey(t)
	mcStamped.filterRules[hex.EncodeToString(actor)] = dropWorkRule
	sStamped := &inboundSession{
		logger:   slog.Default(),
		clientIP: "127.0.0.1",
		from:     "alice@example.org",
		inboundRcpts: []resolvedInboundRcpt{{
			actorID:        actor,
			rcptAddr:       "bob+work@test.example",
			headersToStamp: []wsrpc.StampedHeader{{Name: "X-Fauna-Address-Suffix", Value: "work"}},
		}},
		caller:          mcStamped,
		localDomains:    []string{"test.example"},
		maxMessageBytes: 50_000_000,
		spamPolicy:      permissiveSpamPolicyForTest(),
	}
	if err := sStamped.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data (stamped): %v", err)
	}
	if got := len(mcStamped.ingestRequests); got != 0 {
		t.Errorf("filter rule keyed on the stamped header must fire (Discard → no ingest); got %d ingests", got)
	}

	// Control: identical rule + message, but NO stamp on the recipient → the rule
	// can't see the header → no match → delivered. This is exactly the regression
	// the cutover wiring guards against (filterCtx built from parsed.Headers alone
	// would never carry the per-recipient stamp).
	mcPlain := newMockCaller()
	mcPlain.mlsPubkeys[hex.EncodeToString(actor)] = freshX25519Pubkey(t)
	mcPlain.filterRules[hex.EncodeToString(actor)] = dropWorkRule
	sPlain := &inboundSession{
		logger:          slog.Default(),
		clientIP:        "127.0.0.1",
		from:            "alice@example.org",
		inboundRcpts:    []resolvedInboundRcpt{{actorID: actor, rcptAddr: "bob+work@test.example"}},
		caller:          mcPlain,
		localDomains:    []string{"test.example"},
		maxMessageBytes: 50_000_000,
		spamPolicy:      permissiveSpamPolicyForTest(),
	}
	if err := sPlain.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data (plain): %v", err)
	}
	if got := len(mcPlain.ingestRequests); got != 1 {
		t.Errorf("without the stamp the rule must not match → delivered; got %d ingests, want 1", got)
	}
}

// TestForwardAllCopyOmitsAliasStampHeaders is the leak guard: a recipient that
// matched an alias kind (so the resolver stamped X-Fauna-Address-*) AND has
// forward-all set delivers the stamped header to their own mailbox but must NOT
// leak that internal routing metadata onto the external forward copy.
func TestForwardAllCopyOmitsAliasStampHeaders(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actor := bytes.Repeat([]byte{0x42}, 32)
	mc.mlsPubkeys[hex.EncodeToString(actor)] = freshX25519Pubkey(t)
	mc.forwardConfigs[hex.EncodeToString(actor)] = "bob@example.com"
	s := &inboundSession{
		logger:   slog.Default(),
		clientIP: "127.0.0.1",
		from:     "alice@example.org",
		inboundRcpts: []resolvedInboundRcpt{{
			actorID:        actor,
			rcptAddr:       "bob+work@test.example",
			headersToStamp: []wsrpc.StampedHeader{{Name: "X-Fauna-Address-Suffix", Value: "work"}},
		}},
		caller:          mc,
		localDomains:    []string{"test.example"},
		maxMessageBytes: 50_000_000,
		spamPolicy:      permissiveSpamPolicyForTest(),
	}
	body := "From: alice@example.org\r\n" +
		"To: bob+work@test.example\r\n" +
		"Subject: forward me\r\n" +
		"\r\n" +
		"hello\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.forwardRequests); got != 1 {
		t.Fatalf("forward count: got %d, want 1", got)
	}
	if bytes.Contains(mc.forwardRequests[0].rawMessage, []byte("X-Fauna-Address-Suffix")) {
		t.Errorf("the external forward copy must NOT carry the internal X-Fauna-Address-* stamp")
	}
}

// TestStampedHeaderValueEntersTheSealedLocalCopy pins the one seam between the
// resolver's stamp and the MDA that reads it back: that the STAMPED buffer —
// not the raw one — is what gets sealed and ingested for the recipient
// (mail-aliases.md § Spam-threshold override; `server.go`'s
// `rcptRaw := prependHeaders(raw, stampHeaderLines(...))` feeding
// `ingestForRecipient`). The per-recipient filter context is built separately,
// from the structured headers, so `TestStampedAliasHeaderReachesFilterContext`
// does NOT cover this path — without this test the delivery-time spam-threshold
// stamp could reach the filter rules and never reach the message.
//
// The sealed body cannot be read back here (no test holds the recipient's
// private half), so the assertion is on the ONE observable that still depends
// on the stamped value: two deliveries of the same message differing only in a
// stamp value one byte longer must produce ciphertexts differing by exactly one
// byte. That is decisive — a copy sealed from the unstamped `raw` would give
// identical sizes — and it is latency-independent.
func TestStampedHeaderValueEntersTheSealedLocalCopy(t *testing.T) {
	t.Parallel()
	body := "From: alice@example.org\r\n" +
		"To: bob@test.example\r\n" +
		"Subject: seal me\r\n" +
		"\r\n" +
		"hello\r\n"

	deliver := func(stampValue string) uint32 {
		t.Helper()
		mc := newMockCaller()
		actor := bytes.Repeat([]byte{0x42}, 32)
		mc.mlsPubkeys[hex.EncodeToString(actor)] = freshX25519Pubkey(t)
		s := &inboundSession{
			logger:   slog.Default(),
			clientIP: "127.0.0.1",
			from:     "alice@example.org",
			inboundRcpts: []resolvedInboundRcpt{{
				actorID:  actor,
				rcptAddr: "bob@test.example",
				headersToStamp: []wsrpc.StampedHeader{
					{Name: "X-Fauna-Spam-Threshold", Value: stampValue},
				},
			}},
			caller:          mc,
			localDomains:    []string{"test.example"},
			maxMessageBytes: 50_000_000,
			spamPolicy:      permissiveSpamPolicyForTest(),
		}
		if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
			t.Fatalf("Data: %v", err)
		}
		if len(mc.ingestRequests) != 1 {
			t.Fatalf("want 1 ingest, got %d", len(mc.ingestRequests))
		}
		return mc.ingestRequests[0].publicMetadata.CiphertextSize
	}

	short := deliver("5") // one-digit threshold
	long := deliver("15") // two-digit threshold, one byte longer
	if short == 0 || long == 0 {
		t.Fatalf("no ciphertext sealed (short=%d long=%d)", short, long)
	}
	if long != short+1 {
		t.Errorf("sealed sizes = %d and %d; want the two-digit stamp to seal exactly 1 byte more — "+
			"equal sizes mean the recipient's copy was sealed from the UNSTAMPED buffer", short, long)
	}
}

// TestRcptForwarderAcceptedAndRecorded drives the RCPT handler directly: a
// Forward outcome is accepted (no error) and recorded as a forward recipient
// (not a local actor), and a Reject outcome surfaces the resolver's smtp_code +
// reason on the wire.
func TestRcptForwarderAcceptedAndRecorded(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	admin := bytes.Repeat([]byte{0x07}, 32)
	mc.decisions["info@test.example"] = rcptDecision{forward: true, forwardTarget: "oldaccount@example.net", forwarderActorID: admin}
	mc.decisions["nobody@test.example"] = rcptDecision{reason: "User unknown"}

	s := &inboundSession{
		logger:       slog.Default(),
		clientIP:     "127.0.0.1",
		from:         "alice@example.org",
		caller:       mc,
		localDomains: []string{"test.example"},
		sleep:        func(time.Duration) {}, // no tarpit delay in tests
	}

	// Forward outcome → accepted, recorded as a forward recipient.
	if err := s.Rcpt("info@test.example", nil); err != nil {
		t.Fatalf("forwarder RCPT must be accepted: %v", err)
	}
	if len(s.inboundRcpts) != 1 {
		t.Fatalf("forwarder RCPT must record one recipient; got %d", len(s.inboundRcpts))
	}
	rec := s.inboundRcpts[0]
	if !rec.forward {
		t.Errorf("recipient must be marked forward")
	}
	if rec.forwardTarget != "oldaccount@example.net" || !bytes.Equal(rec.forwarderActorID, admin) {
		t.Errorf("forward record: got target=%q forwarder=%x", rec.forwardTarget, rec.forwarderActorID)
	}
	if rec.actorID != nil {
		t.Errorf("a forwarder has no local actor; got actorID=%x", rec.actorID)
	}

	// Reject outcome → the resolver's smtp_code + reason ride to the wire.
	err := s.Rcpt("nobody@test.example", nil)
	if err == nil {
		t.Fatal("reject outcome must surface an SMTP error")
	}
	se, ok := err.(*gosmtp.SMTPError)
	if !ok {
		t.Fatalf("reject must be a *gosmtp.SMTPError; got %T", err)
	}
	if se.Code != 550 {
		t.Errorf("reject code: got %d, want 550", se.Code)
	}
	if se.Message != "User unknown" {
		t.Errorf("reject message must carry the resolver reason; got %q", se.Message)
	}
}

// TestStampHeaderLines covers the pure helper: nil/empty → nil (prependHeaders
// then returns the buffer untouched), and each StampedHeader renders as a
// "Name: Value" line.
func TestStampHeaderLines(t *testing.T) {
	t.Parallel()
	if got := stampHeaderLines(nil); got != nil {
		t.Errorf("nil input → nil; got %v", got)
	}
	got := stampHeaderLines([]wsrpc.StampedHeader{
		{Name: "X-Fauna-Address-Suffix", Value: "work"},
		{Name: "X-Fauna-Address-Catchall", Value: "true"},
	})
	want := []string{"X-Fauna-Address-Suffix: work", "X-Fauna-Address-Catchall: true"}
	if len(got) != len(want) {
		t.Fatalf("len: got %d, want %d", len(got), len(want))
	}
	for i := range want {
		if got[i] != want[i] {
			t.Errorf("line %d: got %q, want %q", i, got[i], want[i])
		}
	}
}
