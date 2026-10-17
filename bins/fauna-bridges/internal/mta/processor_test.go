// Tests for the processor hand-off (dmarc-reporting.md § Envelope-time
// routing): a RCPT resolve_recipient answers Processor for (dmarc-report@) is
// accepted, and at DATA the message goes to the nest's DMARC ingest — never to
// a mailbox. All tier_1 (in-process, mockCaller).
package mta

import (
	"bytes"
	"errors"
	"log/slog"
	"net/smtp"
	"testing"

	gosmtp "github.com/emersion/go-smtp"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

const dmarcReportBody = "From: dmarc-noreply@reporter.test\r\n" +
	"To: dmarc-report@test.example\r\n" +
	"Message-ID: <r1@reporter.test>\r\n" +
	"Subject: Report domain: test.example\r\n" +
	"MIME-Version: 1.0\r\n" +
	"Content-Type: application/xml\r\n" +
	"\r\n" +
	"<feedback/>\r\n"

func processorSession(mc *mockCaller) *inboundSession {
	return &inboundSession{
		logger:   slog.Default(),
		clientIP: "198.51.100.7",
		from:     "dmarc-noreply@reporter.test",
		inboundRcpts: []resolvedInboundRcpt{{
			processor: wsrpc.ProcessorDmarcReport,
			rcptAddr:  "dmarc-report@test.example",
		}},
		caller:          mc,
		localDomains:    []string{"test.example"},
		maxMessageBytes: 50_000_000,
		spamPolicy:      permissiveSpamPolicyForTest(),
	}
}

// TestProcessorRcptIsAcceptedNotRejected: resolve_recipient's Processor outcome
// is accepted at RCPT TO with 250 — a role address never rejects for want of a
// mailbox — and no MLS-key check runs (there is no recipient actor).
func TestProcessorRcptIsAcceptedNotRejected(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	mc.decisions["dmarc-report@test.example"] = rcptDecision{processor: wsrpc.ProcessorDmarcReport}
	addr, cancel := startBackendListener(t, &inboundBackend{
		logger: slog.Default(),
		caller: mc,
		cfg: staticMTAConfig(&mtaLiveConfig{
			localDomains: []string{"test.example"},
		}),
	})
	defer cancel()
	c, err := smtp.Dial(addr)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer func() { _ = c.Close() }()
	if err := c.Hello("reporter.test"); err != nil {
		t.Fatalf("HELO: %v", err)
	}
	if err := c.Mail("dmarc-noreply@reporter.test"); err != nil {
		t.Fatalf("MAIL: %v", err)
	}
	if err := c.Rcpt("dmarc-report@test.example"); err != nil {
		t.Fatalf("processor RCPT must be accepted (250): %v", err)
	}
	if mc.calls["dmarc-report@test.example"] != 1 {
		t.Errorf("resolve call count: got %d, want 1", mc.calls["dmarc-report@test.example"])
	}
}

// TestProcessorDataHandsTheReportToTheNestNotAMailbox: at DATA the message is
// handed to ingest_dmarc_report with the raw bytes and the
// transaction's envelope (client IP, MAIL FROM, auth verdicts); no mailbox
// ingest happens.
func TestProcessorDataHandsTheReportToTheNestNotAMailbox(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	s := processorSession(mc)
	if err := s.Data(bytes.NewReader([]byte(dmarcReportBody))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.ingestRequests); got != 0 {
		t.Errorf("a processor recipient must reach no mailbox; got %d ingest calls", got)
	}
	if got := len(mc.dmarcIngests); got != 1 {
		t.Fatalf("dmarc ingest count: got %d, want 1", got)
	}
	rec := mc.dmarcIngests[0]
	if !bytes.Contains(rec.RawMessage, []byte("<feedback/>")) {
		t.Errorf("the raw message must be handed over whole; got %q", rec.RawMessage)
	}
	if rec.SourceEnvelope.ClientIP != "198.51.100.7" {
		t.Errorf("client_ip: got %q", rec.SourceEnvelope.ClientIP)
	}
	if rec.SourceEnvelope.MailFrom != "dmarc-noreply@reporter.test" {
		t.Errorf("mail_from: got %q", rec.SourceEnvelope.MailFrom)
	}
	if rec.SourceEnvelope.AuthResults.Dmarc.Kind == "" {
		t.Errorf("the carrying mail's own verdicts must ride along")
	}
	if rec.ReceivedAt == 0 {
		t.Errorf("received_at must carry the accept instant")
	}
}

// TestProcessorHandOffFailureTempfails: a hand-off the nest could not take is a
// 451, so the sender retries and the report is not lost.
func TestProcessorHandOffFailureTempfails(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	mc.dmarcIngestErr = errors.New("nest unreachable")
	s := processorSession(mc)
	err := s.Data(bytes.NewReader([]byte(dmarcReportBody)))
	var smtpErr *gosmtp.SMTPError
	if !errors.As(err, &smtpErr) || smtpErr.Code != 451 {
		t.Fatalf("want 451, got %v", err)
	}
	if got := len(mc.ingestRequests); got != 0 {
		t.Errorf("no mailbox fallback on a failed hand-off; got %d ingest calls", got)
	}
}

const tlsrptReportBody = "From: tlsrpt-noreply@reporter.test\r\n" +
	"To: tlsrpt@test.example\r\n" +
	"Message-ID: <t1@reporter.test>\r\n" +
	"Subject: Report Domain: test.example\r\n" +
	"MIME-Version: 1.0\r\n" +
	"Content-Type: application/tlsrpt+json\r\n" +
	"\r\n" +
	"{\"report-id\":\"t1\"}\r\n"

// TestTlsrptProcessorDataHandsTheReportToItsOwnIngest: a tlsrpt@ recipient
// (Processor{tlsrpt_inbound}) goes to ingest_tlsrpt_report with the raw bytes
// and the envelope — not to the DMARC ingest, not to a mailbox
// (smtp-server.md § TLSRPT inbound report listener).
func TestTlsrptProcessorDataHandsTheReportToItsOwnIngest(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	s := processorSession(mc)
	s.from = "tlsrpt-noreply@reporter.test"
	s.inboundRcpts = []resolvedInboundRcpt{{
		processor: wsrpc.ProcessorTlsrptInbound,
		rcptAddr:  "tlsrpt@test.example",
	}}
	if err := s.Data(bytes.NewReader([]byte(tlsrptReportBody))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.ingestRequests) + len(mc.dmarcIngests); got != 0 {
		t.Errorf("a tlsrpt@ recipient reaches neither a mailbox nor the DMARC ingest; got %d calls", got)
	}
	if got := len(mc.tlsrptIngests); got != 1 {
		t.Fatalf("tlsrpt ingest count: got %d, want 1", got)
	}
	rec := mc.tlsrptIngests[0]
	if !bytes.Contains(rec.RawMessage, []byte(`"report-id":"t1"`)) {
		t.Errorf("the raw message must be handed over whole; got %q", rec.RawMessage)
	}
	if rec.SourceEnvelope.MailFrom != "tlsrpt-noreply@reporter.test" || rec.ReceivedAt == 0 {
		t.Errorf("envelope / accept instant must ride along; got %+v", rec)
	}
}

// TestTlsrptProcessorHandOffFailureTempfails: as for DMARC, a hand-off the
// nest could not take is a 451 with no mailbox fallback.
func TestTlsrptProcessorHandOffFailureTempfails(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	mc.tlsrptIngestErr = errors.New("nest unreachable")
	s := processorSession(mc)
	s.inboundRcpts = []resolvedInboundRcpt{{
		processor: wsrpc.ProcessorTlsrptInbound,
		rcptAddr:  "tlsrpt@test.example",
	}}
	err := s.Data(bytes.NewReader([]byte(tlsrptReportBody)))
	var smtpErr *gosmtp.SMTPError
	if !errors.As(err, &smtpErr) || smtpErr.Code != 451 {
		t.Fatalf("want 451, got %v", err)
	}
	if got := len(mc.ingestRequests); got != 0 {
		t.Errorf("no mailbox fallback on a failed hand-off; got %d ingest calls", got)
	}
}
