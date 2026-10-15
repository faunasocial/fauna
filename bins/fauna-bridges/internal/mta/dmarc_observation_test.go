package mta

// Session.Data reports each verified inbound message's DMARC observation to
// nest (`fauna.bridges.report_dmarc_observation`) before any enforcement gate,
// so the outbound DMARC aggregate report counts refused mail too
// (docs/goal/behavior/dmarc-reporting.md § Outbound aggregate-report sender).

import (
	"bytes"
	"encoding/hex"
	"errors"
	"log/slog"
	"testing"

	gosmtp "github.com/emersion/go-smtp"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

func dmarcObservationSession(t *testing.T, mc *mockCaller, policy wsrpc.AuthPolicy) *inboundSession {
	t.Helper()
	actor := bytes.Repeat([]byte{0x42}, 32)
	mc.mlsPubkeys[hex.EncodeToString(actor)] = freshX25519Pubkey(t)
	return &inboundSession{
		logger:          slog.Default(),
		clientIP:        "198.51.100.7",
		from:            "alice@example.org",
		inboundRcpts:    []resolvedInboundRcpt{{actorID: actor, rcptAddr: "bob@test.example"}},
		caller:          mc,
		localDomains:    []string{"test.example"},
		maxMessageBytes: 50_000_000,
		spamPolicy:      permissiveSpamPolicyForTest(),
		authPolicy:      policy,
	}
}

func dmarcObservationBody(dmarc string) []byte {
	return []byte(hermeticDmarcHeader + ": " + dmarc + "\r\n" +
		"From: alice@example.org\r\n" +
		"To: bob@test.example\r\n" +
		"Subject: count me\r\n" +
		"\r\n" +
		"hello\r\n")
}

func TestDataReportsTheDmarcObservationOfADeliveredMessage(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	s := dmarcObservationSession(t, mc, wsrpc.AuthPolicy{})
	if err := s.Data(bytes.NewReader(dmarcObservationBody("pass"))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if len(mc.dmarcObservations) != 1 {
		t.Fatalf("want one report_dmarc_observation, got %d", len(mc.dmarcObservations))
	}
	got := mc.dmarcObservations[0]
	if got.ClientIP != "198.51.100.7" {
		t.Errorf("client_ip = %q, want the connecting MTA's 198.51.100.7", got.ClientIP)
	}
	if got.Dmarc.Kind != "pass" {
		t.Errorf("dmarc = %q, want pass", got.Dmarc.Kind)
	}
	if got.Observation.HeaderFromDomain != hermeticObservedFromDomain {
		t.Errorf("header_from_domain = %q, want the verifier's %q",
			got.Observation.HeaderFromDomain, hermeticObservedFromDomain)
	}
	if got.Observation.DkimResults == nil {
		t.Error("dkim_results must ride as a list, never null (nest decodes a Vec)")
	}
}

func TestDataReportsTheDmarcObservationOfARefusedMessage(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	s := dmarcObservationSession(t, mc, wsrpc.AuthPolicy{EnforceDmarc: true})
	err := s.Data(bytes.NewReader(dmarcObservationBody("reject")))
	var smtpErr *gosmtp.SMTPError
	if !errors.As(err, &smtpErr) || smtpErr.Code != 550 {
		t.Fatalf("a p=reject failure under enforcement must be refused 550; got %v", err)
	}
	if len(mc.dmarcObservations) != 1 {
		t.Fatalf("the refused message must still be reported; got %d reports", len(mc.dmarcObservations))
	}
	d := mc.dmarcObservations[0].Dmarc
	if d.Kind != "fail" || d.Data == nil || d.Data.Policy != "reject" {
		t.Errorf("dmarc = %+v, want fail/reject (nest derives the reject disposition from it)", d)
	}
	if len(mc.ingestRequests) != 0 {
		t.Errorf("a refused message is never ingested; got %d ingests", len(mc.ingestRequests))
	}
}
