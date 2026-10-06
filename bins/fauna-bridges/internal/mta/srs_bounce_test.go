// Tests for the inbound SRS-bounce path (mail-forwarding N4,
// docs/goal/behavior/mail-forwarding.md § Bounce decode / § NDR routing).
// These drive RCPT recognition (the decode_srs_bounce outcome → SMTP wire
// mapping) and the DATA-stage delivery to the forwarder (vs. orphan drop)
// in-process with a mockCaller (tier_1 — no nest binary, no driver).
package mta

import (
	"bytes"
	"encoding/hex"
	"errors"
	"log/slog"
	"testing"
	"time"

	gosmtp "github.com/emersion/go-smtp"
)

// srsRcptSession builds a minimal inbound session for driving Rcpt directly.
// sleep is a no-op so the tarpit on reject paths doesn't slow the test.
func srsRcptSession(mc *mockCaller) *inboundSession {
	return &inboundSession{
		logger:       slog.Default(),
		clientIP:     "127.0.0.1",
		from:         "", // a bounce is null-sender; the SRS branch returns before greylist anyway
		caller:       mc,
		localDomains: []string{"test.example"},
		sleep:        func(time.Duration) {},
	}
}

// A representative SRS0 local-part + its full RCPT address on our domain.
const (
	srsLocalPart = "SRS0=ABCD=EF=42=example.org=carol"
	srsRcptAddr  = srsLocalPart + "@test.example"
)

func TestIsSrsBounceLocalPart(t *testing.T) {
	t.Parallel()
	cases := []struct {
		in   string
		want bool
	}{
		{"SRS0=ABCD=EF=1=x.test=a", true},
		{"SRS1=ABCD=EF=1=x.test=a", true},
		{"srs0=lower=case=ok", true}, // case-insensitive per the codec
		{"Srs1=Mixed", true},
		{"bob", false},
		{"srs0", false},    // no '=' — not the prefix
		{"srs", false},     // too short
		{"", false},        // empty
		{"xSRS0=y", false}, // prefix not at the start
	}
	for _, c := range cases {
		if got := isSrsBounceLocalPart(c.in); got != c.want {
			t.Errorf("isSrsBounceLocalPart(%q) = %v, want %v", c.in, got, c.want)
		}
	}
}

func TestRcptSrsBounceOkResolvesToForwarder(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	forwarder := bytes.Repeat([]byte{0x42}, 32)
	mc.srsBounceDecisions[srsLocalPart] = srsBounceDecision{
		outcome:     "ok",
		forwarder:   forwarder,
		sender:      "carol@example.org",
		destination: "bob@example.net",
	}
	s := srsRcptSession(mc)

	if err := s.Rcpt(srsRcptAddr, &gosmtp.RcptOptions{}); err != nil {
		t.Fatalf("SRS bounce RCPT must be accepted; got %v", err)
	}
	if len(s.inboundRcpts) != 1 {
		t.Fatalf("inboundRcpts: got %d, want 1", len(s.inboundRcpts))
	}
	rc := s.inboundRcpts[0]
	if !rc.srsBounce {
		t.Errorf("rcpt must be marked srsBounce")
	}
	if rc.srsBounceOrphan {
		t.Errorf("rcpt must not be marked orphan")
	}
	if !bytes.Equal(rc.actorID, forwarder) {
		t.Errorf("rcpt actor: got %x, want forwarder %x", rc.actorID, forwarder)
	}
	// validate_recipient must NOT have run for an SRS bounce.
	if mc.calls[srsRcptAddr] != 0 {
		t.Errorf("validate_recipient must not run for an SRS bounce; got %d calls", mc.calls[srsRcptAddr])
	}
}

func TestRcptSrsBounceOrphanAcceptsButMarksDrop(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	mc.srsBounceDecisions[srsLocalPart] = srsBounceDecision{outcome: "orphan"}
	s := srsRcptSession(mc)

	if err := s.Rcpt(srsRcptAddr, &gosmtp.RcptOptions{}); err != nil {
		t.Fatalf("orphan SRS bounce RCPT must be accepted (drop is at DATA); got %v", err)
	}
	if len(s.inboundRcpts) != 1 {
		t.Fatalf("inboundRcpts: got %d, want 1", len(s.inboundRcpts))
	}
	rc := s.inboundRcpts[0]
	if !rc.srsBounceOrphan {
		t.Errorf("orphan rcpt must be marked srsBounceOrphan")
	}
	if len(rc.actorID) != 0 {
		t.Errorf("orphan rcpt must carry no actor; got %x", rc.actorID)
	}
}

func TestRcptSrsBounceMacFailRejects550_511(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	mc.srsBounceDecisions[srsLocalPart] = srsBounceDecision{outcome: "mac_fail"}
	s := srsRcptSession(mc)

	se := asSMTPError(t, s.Rcpt(srsRcptAddr, &gosmtp.RcptOptions{}))
	if se.Code != 550 || se.EnhancedCode != (gosmtp.EnhancedCode{5, 1, 1}) {
		t.Errorf("mac_fail: got %d %v, want 550 5.1.1", se.Code, se.EnhancedCode)
	}
	if len(s.inboundRcpts) != 0 {
		t.Errorf("rejected RCPT must not be recorded; got %d", len(s.inboundRcpts))
	}
}

func TestRcptSrsBounceExpiredRejects550_544(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	mc.srsBounceDecisions[srsLocalPart] = srsBounceDecision{outcome: "expired"}
	s := srsRcptSession(mc)

	se := asSMTPError(t, s.Rcpt(srsRcptAddr, &gosmtp.RcptOptions{}))
	if se.Code != 550 || se.EnhancedCode != (gosmtp.EnhancedCode{5, 4, 4}) {
		t.Errorf("expired: got %d %v, want 550 5.4.4", se.Code, se.EnhancedCode)
	}
}

func TestRcptSrsBounceMalformedRejects550(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	mc.srsBounceDecisions[srsLocalPart] = srsBounceDecision{outcome: "malformed"}
	s := srsRcptSession(mc)

	se := asSMTPError(t, s.Rcpt(srsRcptAddr, &gosmtp.RcptOptions{}))
	if se.Code != 550 {
		t.Errorf("malformed: got %d, want 550", se.Code)
	}
}

// not_srs (the cheap prefix check matched but nest disagrees) falls through to
// the normal validate_recipient path — the address resolves as an ordinary
// recipient, NOT an SRS bounce.
func TestRcptSrsBounceNotSrsFallsThroughToValidate(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	// No srsBounceDecisions entry → decode returns "not_srs".
	actor := bytes.Repeat([]byte{0x55}, 32)
	mc.decisions[srsRcptAddr] = rcptDecision{resolved: true, actorID: actor}
	mc.mlsPubkeys[hex.EncodeToString(actor)] = freshX25519Pubkey(t)
	s := srsRcptSession(mc)

	if err := s.Rcpt(srsRcptAddr, &gosmtp.RcptOptions{}); err != nil {
		t.Fatalf("not_srs fall-through RCPT must resolve normally; got %v", err)
	}
	if mc.calls[srsRcptAddr] != 1 {
		t.Errorf("validate_recipient must run on not_srs fall-through; got %d calls", mc.calls[srsRcptAddr])
	}
	if len(s.inboundRcpts) != 1 || s.inboundRcpts[0].srsBounce {
		t.Errorf("not_srs must resolve as a normal (non-srsBounce) recipient; got %+v", s.inboundRcpts)
	}
	if !bytes.Equal(s.inboundRcpts[0].actorID, actor) {
		t.Errorf("not_srs actor: got %x, want %x", s.inboundRcpts[0].actorID, actor)
	}
}

func TestRcptSrsBounceTransportErrorTempfails451(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	mc.srsBounceDecisions[srsLocalPart] = srsBounceDecision{err: errors.New("synthetic nest blip")}
	s := srsRcptSession(mc)

	se := asSMTPError(t, s.Rcpt(srsRcptAddr, &gosmtp.RcptOptions{}))
	if se.Code != 451 {
		t.Errorf("transport error: got %d, want 451", se.Code)
	}
}

// TestDataSrsBounceDeliversToForwarderNotReforwarded pins the DATA stage: a
// recipient resolved as an SRS bounce receives the message in their mailbox
// (one ingest to the forwarder actor) and is NEVER re-forwarded — even with
// forward-all configured and a non-null sender (so the guard, not just the
// null-sender skip, is what suppresses the re-forward, :255).
func TestDataSrsBounceDeliversToForwarderNotReforwarded(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	forwarder := bytes.Repeat([]byte{0x77}, 32)
	// A non-null sender isolates the srsBounce guard from the null-sender skip.
	s := forwardingSession(t, mc, forwarder, "mailer-daemon@example.net")
	s.inboundRcpts = []resolvedInboundRcpt{{actorID: forwarder, srsBounce: true}}
	// Forward-all IS configured for the forwarder — proving the guard suppresses
	// re-forwarding a bounce we just delivered to them.
	mc.forwardConfigs[hex.EncodeToString(forwarder)] = "downstream@example.net"

	body := "From: mailer-daemon@example.net\r\n" +
		"To: " + srsRcptAddr + "\r\n" +
		"Subject: Delivery Status Notification (Failure)\r\n" +
		"Message-ID: <dsn-1@example.net>\r\n" +
		"\r\n" +
		"Your message to bob@example.net could not be delivered.\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.ingestRequests); got != 1 {
		t.Fatalf("ingest count: got %d, want 1 (the bounce → forwarder mailbox)", got)
	}
	if !bytes.Equal(mc.ingestRequests[0].actorID, forwarder) {
		t.Errorf("bounce delivered to %x, want forwarder %x", mc.ingestRequests[0].actorID, forwarder)
	}
	if got := len(mc.forwardRequests); got != 0 {
		t.Errorf("an inbound SRS bounce must never be re-forwarded; got %d forwards", got)
	}
}

// TestDataSrsBounceOrphanDropped pins the orphan path: a verified-ours bounce
// whose forwarding row is gone is dropped at DATA (no ingest, no forward) —
// never landed in the admin mailbox (:104,:117).
func TestDataSrsBounceOrphanDropped(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	// forwardingSession needs an actor for its MLS-pubkey seed, but the orphan
	// rcpt carries no actor and is dropped before ingest — so the seed is unused.
	unused := bytes.Repeat([]byte{0x01}, 32)
	s := forwardingSession(t, mc, unused, "mailer-daemon@example.net")
	s.inboundRcpts = []resolvedInboundRcpt{{srsBounceOrphan: true}}

	body := "From: mailer-daemon@example.net\r\n" +
		"To: " + srsRcptAddr + "\r\n" +
		"Subject: Delivery Status Notification (Failure)\r\n" +
		"Message-ID: <dsn-2@example.net>\r\n" +
		"\r\n" +
		"orphaned bounce\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.ingestRequests); got != 0 {
		t.Errorf("orphan bounce must be dropped (no mailbox); got %d ingests", got)
	}
	if got := len(mc.forwardRequests); got != 0 {
		t.Errorf("orphan bounce must not forward; got %d", got)
	}
}
