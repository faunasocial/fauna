// Tests for the forward spoof floor (docs/goal/behavior/mail-forwarding.md
// § Spoof floor — a local From: must have authenticated). A forward leaves
// unsigned with its envelope SRS-rewritten onto the primary mail domain, so a
// forwarded message whose From: names a served domain would SPF-align and pass
// DMARC downstream. The MX door therefore never forwards a local From: whose
// DMARC verdict on arrival was not pass — whatever let the perimeter admit it.
// Like the loop floors, the floor drops only the forward: local delivery still
// completes.
package mta

import (
	"bytes"
	"encoding/hex"
	"log/slog"
	"net"
	"net/textproto"
	"strings"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// spoofFloorBody is a message From: a local mailbox (test.example is the
// session's served domain), carrying the hermetic verifier's DMARC knob
// (main_test.go) when dmarc is non-empty.
func spoofFloorBody(from, dmarc string) string {
	knob := ""
	if dmarc != "" {
		knob = hermeticDmarcHeader + ": " + dmarc + "\r\n"
	}
	return "From: " + from + "\r\n" +
		"To: bob@test.example\r\n" +
		"Message-ID: <spoof-1@test.example>\r\n" +
		knob +
		"Subject: wire the money\r\n" +
		"\r\n" +
		"hello\r\n"
}

// TestForwardSpoofFloorSuppressesAdmittedLocalSpoof forwards a perimeter-
// admitted spoof of the served domain through forward-all under every way the
// perimeter admits a DMARC non-pass, and asserts the forward is refused while
// the local copy is delivered.
func TestForwardSpoofFloorSuppressesAdmittedLocalSpoof(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name    string
		from    string
		dmarc   string
		logOnly bool
		enforce bool
	}{
		// The domain publishes no DMARC record: verdict none.
		{name: "no DMARC record", from: "ceo@test.example"},
		// The domain's own p=none: a fail no gate refuses.
		{name: "own p=none", from: "ceo@test.example", dmarc: "fail", enforce: true},
		// The domain's own p=quarantine: admitted to Junk.
		{name: "own p=quarantine", from: "ceo@test.example", dmarc: "quarantine", enforce: true},
		// p=reject, but the admin runs with DMARC enforcement off.
		{name: "enforcement off", from: "ceo@test.example", dmarc: "reject"},
		// p=reject, enforcement on, but the whole stage is log-only.
		{name: "log-only", from: "ceo@test.example", dmarc: "reject", logOnly: true, enforce: true},
		// A subdomain of the served domain aligns relaxed just the same.
		{name: "subdomain From", from: "ceo@hr.test.example"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			mc := newMockCaller()
			actorBob := bytes.Repeat([]byte{0x42}, 32)
			s := forwardingSession(t, mc, actorBob, "attacker@evil.example")
			s.authPolicy = wsrpc.AuthPolicy{LogOnly: tc.logOnly, EnforceDmarc: tc.enforce}
			mc.forwardConfigs[hex.EncodeToString(actorBob)] = "bob-personal@example.net"

			if err := s.Data(bytes.NewReader([]byte(spoofFloorBody(tc.from, tc.dmarc)))); err != nil {
				t.Fatalf("Data: %v (the perimeter must admit this case)", err)
			}
			if got := len(mc.ingestRequests); got != 1 {
				t.Fatalf("local delivery must still complete; ingest count %d", got)
			}
			if got := len(mc.forwardRequests); got != 0 {
				t.Fatalf("an unauthenticated local From: must not be forwarded; got %d forward(s)", got)
			}
		})
	}
}

// TestForwardSpoofFloorForwardsAuthenticatedOrForeignFrom pins the floor's
// other side: a local From: that passed DMARC on arrival, and a foreign From:
// that failed it, both forward as before.
func TestForwardSpoofFloorForwardsAuthenticatedOrForeignFrom(t *testing.T) {
	t.Parallel()
	cases := []struct{ name, from, dmarc string }{
		{name: "local From, DMARC pass", from: "ceo@test.example", dmarc: "pass"},
		{name: "foreign From, DMARC fail", from: "alice@example.org", dmarc: "fail"},
		{name: "look-alike domain", from: "ceo@attest.example"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			mc := newMockCaller()
			actorBob := bytes.Repeat([]byte{0x42}, 32)
			s := forwardingSession(t, mc, actorBob, "alice@example.org")
			mc.forwardConfigs[hex.EncodeToString(actorBob)] = "bob-personal@example.net"

			if err := s.Data(bytes.NewReader([]byte(spoofFloorBody(tc.from, tc.dmarc)))); err != nil {
				t.Fatalf("Data: %v", err)
			}
			if got := len(mc.forwardRequests); got != 1 {
				t.Fatalf("forward count: got %d, want 1", got)
			}
		})
	}
}

// TestForwardSpoofFloorSiblingOfPrimary: the SRS envelope is on the primary
// mail domain, so when the primary sits below its organizational domain
// (mail.corp.example, corp.example not served) a spoof of a sibling name
// (hr.corp.example) aligns relaxed downstream; the floor refuses it, while a
// look-alike outside the primary's organizational domain still forwards.
func TestForwardSpoofFloorSiblingOfPrimary(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name    string
		from    string
		forward int
	}{
		{name: "sibling of the primary", from: "ceo@hr.corp.example", forward: 0},
		{name: "the primary's parent", from: "ceo@corp.example", forward: 0},
		{name: "foreign look-alike", from: "ceo@hr.corp.example.net", forward: 1},
		{name: "label-boundary look-alike", from: "ceo@evilcorp.example", forward: 1},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			mc := newMockCaller()
			actorBob := bytes.Repeat([]byte{0x42}, 32)
			s := forwardingSession(t, mc, actorBob, "attacker@evil.example")
			s.localDomains = []string{"mail.corp.example"}
			s.primaryDomain = "mail.corp.example"
			mc.forwardConfigs[hex.EncodeToString(actorBob)] = "bob-personal@example.net"

			if err := s.Data(bytes.NewReader([]byte(spoofFloorBody(tc.from, "")))); err != nil {
				t.Fatalf("Data: %v", err)
			}
			if got := len(mc.ingestRequests); got != 1 {
				t.Fatalf("local delivery must still complete; ingest count %d", got)
			}
			if got := len(mc.forwardRequests); got != tc.forward {
				t.Fatalf("forward count: got %d, want %d", got, tc.forward)
			}
		})
	}
}

// TestForwardSpoofFloorAdminForwarder: the admin external forwarder (no local
// copy) is held to the same floor at the MX door — a spoof of the served
// domain addressed to info@ is not relayed outward.
func TestForwardSpoofFloorAdminForwarder(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	admin := bytes.Repeat([]byte{0x07}, 32)
	s := forwarderSession(mc, admin, "oldaccount@example.net", "attacker@evil.example")

	if err := s.Data(bytes.NewReader([]byte(spoofFloorBody("ceo@test.example", "")))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.forwardRequests); got != 0 {
		t.Fatalf("an unauthenticated local From: must not be relayed by a forwarder; got %d", got)
	}
}

// spoofFloorRuleBody is spoofFloorBody with a Subject the forwardRule /
// redirectRule fixtures (filter_forward_test.go) fire on.
func spoofFloorRuleBody() string {
	return strings.Replace(spoofFloorBody("ceo@test.example", ""),
		"Subject: wire the money", "Subject: please fwd this", 1)
}

// TestForwardSpoofFloorPerRuleCopy: a per-rule Forward in copy mode is held to
// the floor on its own arm — an admitted local spoof is delivered locally and
// the rule's forward is refused. (Pins the dispatchFilterForwards door; the
// forward-all and admin-forwarder arms are pinned above.)
func TestForwardSpoofFloorPerRuleCopy(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actorBob, "attacker@evil.example")
	mc.filterRules[hex.EncodeToString(actorBob)] = []wsrpc.EmailFilterWire{
		forwardRule(t, 77, "bob-elsewhere@example.net", false),
	}

	if err := s.Data(bytes.NewReader([]byte(spoofFloorRuleBody()))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.ingestRequests); got != 1 {
		t.Fatalf("local copy must still be ingested; got %d ingests", got)
	}
	if got := len(mc.forwardRequests); got != 0 {
		t.Fatalf("a per-rule copy forward must not relay an unauthenticated local From:; got %d", got)
	}
}

// TestForwardSpoofFloorPerRuleRedirect: a per-rule Forward in redirect mode
// whose forward the floor refuses falls back to local delivery — the spoof is
// not relayed and the mail is not lost.
func TestForwardSpoofFloorPerRuleRedirect(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actorBob, "attacker@evil.example")
	mc.filterRules[hex.EncodeToString(actorBob)] = []wsrpc.EmailFilterWire{
		redirectRule(t, 78, "bob-elsewhere@example.net", false),
	}

	if err := s.Data(bytes.NewReader([]byte(spoofFloorRuleBody()))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.forwardRequests); got != 0 {
		t.Fatalf("a per-rule redirect must not relay an unauthenticated local From:; got %d", got)
	}
	if got := len(mc.ingestRequests); got != 1 {
		t.Fatalf("a floor-suppressed redirect must fall back to local delivery; got %d ingests", got)
	}
}

// TestForwardSpoofFloorSiblingArmReachedThroughNewSession: the sibling
// arm reads the session's primaryDomain, which inboundBackend.NewSession copies
// from the live config. TestForwardSpoofFloorSiblingOfPrimary assigns that field
// by hand, so only this wire case — a real listener, primaryDomain set in the
// config alone — witnesses the copy: with it dropped the arm is off and the
// sibling spoof forwards.
func TestForwardSpoofFloorSiblingArmReachedThroughNewSession(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name    string
		from    string
		dmarc   string
		forward int
	}{
		{name: "sibling spoof, no DMARC", from: "ceo@hr.example.com", forward: 0},
		{name: "sibling, DMARC pass", from: "ceo@hr.example.com", dmarc: "pass", forward: 1},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			mc := newMockCaller()
			bobID := bytes.Repeat([]byte{0x42}, 32)
			mc.decisions["bob@mail.example.com"] = rcptDecision{resolved: true, actorID: bobID}
			mc.mlsPubkeys[hex.EncodeToString(bobID)] = freshX25519Pubkey(t)
			mc.forwardConfigs[hex.EncodeToString(bobID)] = "bob-personal@example.net"

			addr, cancel := startBackendListener(t, &inboundBackend{
				logger: slog.Default(),
				caller: mc,
				cfg: staticMTAConfig(&mtaLiveConfig{
					localDomains:  []string{"mail.example.com"},
					primaryDomain: "mail.example.com",
				}),
			})
			defer cancel()

			conn, err := net.DialTimeout("tcp", addr, 5*time.Second)
			if err != nil {
				t.Fatalf("dial: %v", err)
			}
			defer func() { _ = conn.Close() }()
			_ = conn.SetDeadline(time.Now().Add(30 * time.Second))
			c := textproto.NewConn(conn)
			smtpExpect(t, c, 220, "")
			smtpExpect(t, c, 250, "HELO loopback.example")
			smtpExpect(t, c, 250, "MAIL FROM:<attacker@evil.example>")
			smtpExpect(t, c, 250, "RCPT TO:<bob@mail.example.com>")
			smtpExpect(t, c, 354, "DATA")
			if _, err := conn.Write([]byte(spoofFloorBody(tc.from, tc.dmarc) + "\r\n.\r\n")); err != nil {
				t.Fatalf("write DATA: %v", err)
			}
			smtpExpect(t, c, 250, "")
			smtpExpect(t, c, 221, "QUIT")

			if got := len(mc.ingestRequests); got != 1 {
				t.Fatalf("local delivery must still complete; ingest count %d", got)
			}
			if got := len(mc.forwardRequests); got != tc.forward {
				t.Fatalf("forward count: got %d, want %d", got, tc.forward)
			}
		})
	}
}
