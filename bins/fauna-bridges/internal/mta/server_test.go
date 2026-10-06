package mta

import (
	"bytes"
	"context"
	"crypto/ecdh"
	"crypto/rand"
	"crypto/tls"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/smtp"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/emersion/go-sasl"
	gosmtp "github.com/emersion/go-smtp"
	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc/wsrpctest"
)

// TestServerAcceptsBasicMessage drives Phase C.1's SMTP MX skeleton:
// dial the listener on an ephemeral port, run a full HELO/MAIL/RCPT/
// DATA conversation, expect 250 OK, and confirm the backend received
// the raw DATA bytes verbatim. Phases C.2-C.9 wrap policy / verify /
// score / tokenize / encrypt / ingest around this skeleton.
//
// The test injects a recordingBackend (defined in this file) into the
// runListenerWithBackend helper rather than going through Run; that
// keeps the test hermetic from Run's gates (which require a non-empty
// Domain + MailEnabled Snapshot) and lets us assert on the raw bytes
// the backend sees.

// permissiveSpamPolicyForTest returns a SpamPolicy whose tier thresholds
// (in 0–15 points) are set absurdly high so no realistic combined score
// ever trips a tier — i.e. every message is accepted to INBOX. Used by the
// server/forward tests that exercise the Data pipeline without wanting the
// spam disposition to interfere. (Real scores are ≤ ~15; these are 1000+.)
func permissiveSpamPolicyForTest() mailfauna.SpamPolicy {
	return mailfauna.SpamPolicy{
		SpamFolderThreshold:  1_000,
		RejectThreshold:      100_000,
		HonorDmarcQuarantine: false,
	}
}

func TestServerAcceptsBasicMessage(t *testing.T) {
	t.Parallel()

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}

	received := make(chan []byte, 1)
	backend := &recordingBackend{received: received}

	ctx, cancel := context.WithCancel(context.Background())
	serveErr := make(chan error, 1)
	go func() {
		serveErr <- runListenerWithBackend(ctx, ln, backend, nil, "test.example", 0, 0, nil, slog.Default())
	}()

	addr := ln.Addr().String()
	// Allow the goroutine a moment to enter Serve before we dial. go-smtp
	// is synchronous on Serve; without this the dial races the listener.
	time.Sleep(20 * time.Millisecond)

	c, err := smtp.Dial(addr)
	if err != nil {
		t.Fatalf("smtp.Dial: %v", err)
	}
	if err := c.Hello("test.example"); err != nil {
		t.Fatalf("HELO: %v", err)
	}
	if err := c.Mail("alice@example.org"); err != nil {
		t.Fatalf("MAIL: %v", err)
	}
	if err := c.Rcpt("bob@fauna.example"); err != nil {
		t.Fatalf("RCPT: %v", err)
	}
	w, err := c.Data()
	if err != nil {
		t.Fatalf("DATA: %v", err)
	}
	bodyText := "Subject: hi\r\n\r\nhello world\r\n"
	if _, err := io.WriteString(w, bodyText); err != nil {
		t.Fatalf("write body: %v", err)
	}
	if err := w.Close(); err != nil {
		t.Fatalf("close body (DATA terminator): %v", err)
	}
	if err := c.Quit(); err != nil {
		t.Fatalf("QUIT: %v", err)
	}

	select {
	case raw := <-received:
		if !bytes.Contains(raw, []byte("hello world")) {
			t.Errorf("backend received body %q, want it to contain %q", raw, "hello world")
		}
	case <-time.After(time.Second):
		t.Fatal("backend did not receive a message within 1s")
	}

	cancel()
	select {
	case err := <-serveErr:
		if err != nil && !errors.Is(err, net.ErrClosed) {
			t.Fatalf("listener exited with error: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("listener did not shut down within 1s of ctx cancel")
	}
}

// recordingBackend is the Phase C.1 test backend — every connection's
// full conversation is accepted, the DATA payload is dispatched into a
// channel for assertion, and nothing else happens. Phases C.2-C.9
// will replace this with a real backend that runs the full pipeline
// (validate_recipient, parse, verify, score, tokenize, encrypt,
// ingest); for C.1 the listener wiring is what's under test.
type recordingBackend struct {
	received chan []byte
}

func (b *recordingBackend) NewSession(_ *gosmtp.Conn) (gosmtp.Session, error) {
	return &recordingSession{received: b.received}, nil
}

type recordingSession struct {
	received chan []byte
}

func (s *recordingSession) AuthMechanisms() []string { return nil }

func (s *recordingSession) Auth(string) (sasl.Server, error) {
	return nil, &gosmtp.SMTPError{
		Code:         503,
		EnhancedCode: gosmtp.EnhancedCode{5, 5, 1},
		Message:      "Authentication not supported",
	}
}

func (s *recordingSession) Mail(string, *gosmtp.MailOptions) error { return nil }
func (s *recordingSession) Rcpt(string, *gosmtp.RcptOptions) error { return nil }

func (s *recordingSession) Data(r io.Reader) error {
	raw, err := io.ReadAll(r)
	if err != nil {
		return err
	}
	s.received <- raw
	return nil
}

func (s *recordingSession) Reset()        {}
func (s *recordingSession) Logout() error { return nil }

// TestInbound25STARTTLSRequired — T1.1:
// when a TLS config is wired, port 25 advertises STARTTLS and enforces
// InboundTLSMode=required (smtp-server.md § TLS posture per port): MAIL
// FROM before STARTTLS is rejected 530 5.7.10, and accepted after the
// upgrade. policy is nil (skeleton) so this isolates the TLS gate from
// the connection-time policy stack.
func TestInbound25STARTTLSRequired(t *testing.T) {
	t.Parallel()
	tlsCfg := newSelfSignedTLSConfig(t)
	backend := &inboundBackend{logger: slog.Default(), requireStartTLS: true}
	addr, cancel := startBackendListenerTLS(t, backend, tlsCfg)
	defer cancel()

	// Connection 1: STARTTLS advertised + cleartext MAIL FROM rejected.
	c, err := smtp.Dial(addr)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	if err := c.Hello("client.example"); err != nil {
		t.Fatalf("EHLO: %v", err)
	}
	if ok, _ := c.Extension("STARTTLS"); !ok {
		t.Fatal("port 25 must advertise STARTTLS when a TLS config is wired")
	}
	mailErr := c.Mail("sender@remote.test")
	if mailErr == nil {
		t.Fatal("cleartext MAIL FROM must be rejected when InboundTLSMode=required")
	}
	if !strings.Contains(mailErr.Error(), "530") || !strings.Contains(mailErr.Error(), "STARTTLS") {
		t.Errorf("expected 530 STARTTLS-required; got %v", mailErr)
	}
	_ = c.Close()

	// Connection 2: after STARTTLS, MAIL FROM is accepted.
	c2, err := smtp.Dial(addr)
	if err != nil {
		t.Fatalf("dial2: %v", err)
	}
	if err := c2.Hello("client.example"); err != nil {
		t.Fatalf("EHLO2: %v", err)
	}
	if err := c2.StartTLS(&tls.Config{InsecureSkipVerify: true, ServerName: "test.example.com"}); err != nil {
		t.Fatalf("STARTTLS upgrade: %v", err)
	}
	if err := c2.Mail("sender@remote.test"); err != nil {
		t.Errorf("MAIL FROM after STARTTLS must be accepted; got %v", err)
	}
	_ = c2.Close()
}

// TestInbound25NoTLSNoSTARTTLS — when no TLS config is wired (bridge not
// yet TLS-provisioned), port 25 binds plaintext-only: it does NOT
// advertise STARTTLS and a cleartext MAIL FROM is accepted (the bridge
// still serves inbound MX before TLS lands).
func TestInbound25NoTLSNoSTARTTLS(t *testing.T) {
	t.Parallel()
	backend := &inboundBackend{logger: slog.Default()} // requireStartTLS false; no tlsConfig
	addr, cancel := startBackendListenerTLS(t, backend, nil)
	defer cancel()

	c, err := smtp.Dial(addr)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	if err := c.Hello("client.example"); err != nil {
		t.Fatalf("EHLO: %v", err)
	}
	if ok, _ := c.Extension("STARTTLS"); ok {
		t.Error("port 25 must NOT advertise STARTTLS when no TLS config is wired")
	}
	if err := c.Mail("sender@remote.test"); err != nil {
		t.Errorf("cleartext MAIL FROM must be accepted when TLS is not required; got %v", err)
	}
	_ = c.Close()
}

// ── Phase C.2 wired-backend integration tests ─────────────────────
//
// These tests drive the production `inboundBackend` (with a populated
// `*Policy`) through a real go-smtp listener on an ephemeral port and
// assert the SMTP wire-level rejections match the plan's enhanced
// status codes. Resolver state is set up via fakeResolver from
// policy_test.go; the rate-limit / greylist clocks are fakeClock.

// startPolicyListener spins up runListenerWithBackend with the given
// policy and returns the listener's address + a cancel func that
// drains the server goroutine.
func startPolicyListener(t *testing.T, policy *Policy) (addr string, cancel func()) {
	t.Helper()
	// localDomains: []string{"test.example"} matches the RCPT TO
	// fixtures the policy-suite tests use (alice@test.example,
	// bob@test.example, …). Pre-refactor, empty inboundBackend.domain
	// short-circuited the RCPT filter (`!= "" &&` clause); the
	// multi-domain refactor flipped to "empty list ⇒ reject all",
	// matching mta.Run's idle-on-empty gate. The fixture now wires
	// the domain explicitly so the policy gates downstream of RCPT
	// (greylist, DNSBL, etc.) get exercised.
	return startBackendListener(t, &inboundBackend{
		logger: slog.Default(),
		cfg: staticMTAConfig(&mtaLiveConfig{
			policy:       policy,
			localDomains: []string{"test.example"},
		}),
	})
}

// startBackendListener is the more general variant — drives an
// arbitrary `*inboundBackend` (allowing policy + caller + domain
// injection for the C.3 RCPT tests). Returns the listener's address
// and a cancel func that drains the server goroutine.
func startBackendListener(t *testing.T, backend *inboundBackend) (addr string, cancel func()) {
	return startBackendListenerTLS(t, backend, nil)
}

// startBackendListenerTLS is startBackendListener with an optional TLS
// config wired into the port-25 listener (STARTTLS advertise + upgrade).
// Pass a non-nil cfg to exercise the InboundTLSMode=required path.
func startBackendListenerTLS(t *testing.T, backend *inboundBackend, tlsConfig *tls.Config) (addr string, cancel func()) {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	// SMTP banner identity = first local domain (test fixtures
	// typically configure a single domain). When localDomains is
	// empty, fall back to a synthetic "test.example" so the listener
	// can construct a banner — this only happens in tests that
	// deliberately exercise the empty-domain path.
	var domain string
	if ld := backend.cfg.current().localDomains; len(ld) > 0 {
		domain = ld[0]
	} else {
		domain = "test.example"
	}
	ctx, cancelFn := context.WithCancel(context.Background())
	serveErr := make(chan error, 1)
	go func() {
		serveErr <- runListenerWithBackend(ctx, ln, backend, tlsConfig, domain, 0, 0, nil, slog.Default())
	}()
	time.Sleep(20 * time.Millisecond)
	cancel = func() {
		cancelFn()
		select {
		case err := <-serveErr:
			if err != nil && !errors.Is(err, net.ErrClosed) {
				t.Errorf("listener exited with error: %v", err)
			}
		case <-time.After(time.Second):
			t.Error("listener did not shut down within 1s of ctx cancel")
		}
	}
	return ln.Addr().String(), cancel
}

// smtpCodeOf extracts an SMTP enhanced status code from a textproto
// error. Returns -1 when the error isn't an *textproto.Error.
func smtpCodeOf(err error) int {
	if err == nil {
		return 0
	}
	// go-smtp / net/smtp surface SMTP errors as *textproto.Error.
	type coder interface{ Error() string }
	_ = coder(nil)
	// net/textproto's Error type isn't exported through net/smtp's
	// public surface, so use a string-prefix match — every SMTP reply
	// starts with "NNN ".
	s := err.Error()
	if len(s) < 3 {
		return -1
	}
	code := 0
	if _, scanErr := fmt.Sscanf(s, "%d ", &code); scanErr != nil {
		return -1
	}
	return code
}

// TestInboundBackendRateLimitRejects exhausts the rate-limit bucket
// via NewSession (which fires at HELO/EHLO, not at TCP accept — see
// go-smtp v0.24's `handleGreet`), then asserts the next session
// surfaces the 421.
func TestInboundBackendRateLimitRejects(t *testing.T) {
	t.Parallel()
	clk := newFakeClock(time.Date(2026, 5, 14, 12, 0, 0, 0, time.UTC))
	pol := &Policy{
		RateLimiter: NewRateLimiter(2, time.Minute, clk),
		DNSBL:       NewDNSBLChecker(nil, newFakeResolver()),
		FCrDNS:      NewFCrDNSChecker(newFakeResolver()),
		FCrDNSMode:  FCrDNSModeOff,
	}
	addr, cancel := startPolicyListener(t, pol)
	defer cancel()
	// First two HELO exchanges consume the bucket; third must 421.
	for i := 0; i < 2; i++ {
		c, err := smtp.Dial(addr)
		if err != nil {
			t.Fatalf("dial #%d: %v", i+1, err)
		}
		if err := c.Hello("loopback.example"); err != nil {
			t.Fatalf("HELO #%d: %v", i+1, err)
		}
		_ = c.Close()
	}
	c, err := smtp.Dial(addr)
	if err != nil {
		t.Fatalf("dial #3 (TCP accept must still succeed): %v", err)
	}
	defer func() { _ = c.Close() }()
	err = c.Hello("loopback.example")
	if err == nil {
		t.Fatal("third HELO should be rate-limited at NewSession")
	}
	if code := smtpCodeOf(err); code != 421 {
		t.Errorf("rate-limit code: got %d, want 421 (err=%v)", code, err)
	}
}

// TestInboundBackendDNSBLRejects sets up a fake DNSBL hit for the
// peer IP (127.0.0.1's reversed zone is `1.0.0.127.zen.spamhaus.org`)
// and asserts the HELO/EHLO command surfaces 554 5.7.1 (NewSession
// rejects on a Spamhaus SBL code).
func TestInboundBackendDNSBLRejects(t *testing.T) {
	t.Parallel()
	res := newFakeResolver()
	res.ipAddr["1.0.0.127.zen.spamhaus.org"] = []net.IP{net.ParseIP("127.0.0.2")}
	clk := newFakeClock(time.Date(2026, 5, 14, 12, 0, 0, 0, time.UTC))
	pol := &Policy{
		RateLimiter: NewRateLimiter(60, time.Minute, clk),
		DNSBL:       NewDNSBLChecker([]string{"zen.spamhaus.org"}, res),
		FCrDNS:      NewFCrDNSChecker(res),
		FCrDNSMode:  FCrDNSModeOff,
	}
	addr, cancel := startPolicyListener(t, pol)
	defer cancel()
	c, err := smtp.Dial(addr)
	if err != nil {
		t.Fatalf("dial (TCP accept must succeed): %v", err)
	}
	defer func() { _ = c.Close() }()
	err = c.Hello("loopback.example")
	if err == nil {
		t.Fatal("DNSBL-listed peer must be rejected at HELO/NewSession")
	}
	if code := smtpCodeOf(err); code != 554 {
		t.Errorf("DNSBL code: got %d, want 554 (err=%v)", code, err)
	}
}

// TestInboundBackendFCrDNSEnforceRejects fires the FCrDNS enforce
// gate at MAIL FROM. Peer IP 127.0.0.1's PTR maps to "evil.example"
// whose forward A is 5.6.7.8 (mismatch); policy is enforce +
// reject_fcrdns_fail. Expected: 550 5.7.25 at MAIL FROM (not at
// NewSession — the gate fires once per session at MAIL).
func TestInboundBackendFCrDNSEnforceRejects(t *testing.T) {
	t.Parallel()
	res := newFakeResolver()
	res.ptr["127.0.0.1"] = []string{"evil.example."}
	res.host["evil.example"] = []string{"5.6.7.8"}
	clk := newFakeClock(time.Date(2026, 5, 14, 12, 0, 0, 0, time.UTC))
	pol := &Policy{
		RateLimiter:      NewRateLimiter(60, time.Minute, clk),
		DNSBL:            NewDNSBLChecker(nil, res),
		FCrDNS:           NewFCrDNSChecker(res),
		FCrDNSMode:       FCrDNSModeEnforce,
		RejectFCrDNSFail: true,
		HELOResolver:     res,
	}
	addr, cancel := startPolicyListener(t, pol)
	defer cancel()
	c, err := smtp.Dial(addr)
	if err != nil {
		t.Fatalf("dial: %v (FCrDNS enforce should reject at MAIL, not NewSession)", err)
	}
	defer func() { _ = c.Close() }()
	if err := c.Hello("loopback.example"); err != nil {
		t.Fatalf("HELO: %v", err)
	}
	err = c.Mail("alice@example.org")
	if err == nil {
		t.Fatal("MAIL FROM should fail FCrDNS enforce gate")
	}
	if code := smtpCodeOf(err); code != 550 {
		t.Errorf("FCrDNS-enforce code: got %d, want 550 (err=%v)", code, err)
	}
}

// TestInboundBackendHELOIdentityRejects exercises the HELO identity
// check with skipLoopbackExemption=true so the production-default
// rejection path fires from a loopback peer. HELO is "host.example"
// but its A record points elsewhere → 554 5.7.0 at MAIL FROM.
//
// This is the C.2 porting-hazard regression guard at the wire level.
func TestInboundBackendHELOIdentityRejects(t *testing.T) {
	t.Parallel()
	res := newFakeResolver()
	res.host["host.example"] = []string{"5.6.7.8"} // doesn't include 127.0.0.1
	clk := newFakeClock(time.Date(2026, 5, 14, 12, 0, 0, 0, time.UTC))
	pol := &Policy{
		RateLimiter:          NewRateLimiter(60, time.Minute, clk),
		DNSBL:                NewDNSBLChecker(nil, res),
		FCrDNS:               NewFCrDNSChecker(res),
		FCrDNSMode:           FCrDNSModeOff,
		HELOIdentityRequired: true,
		HELOResolver:         res,
		HELOLookupTimeout:    time.Second,
		SkipHELOLoopback:     true, // drive the production-default rule from 127.0.0.1
	}
	addr, cancel := startPolicyListener(t, pol)
	defer cancel()
	c, err := smtp.Dial(addr)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer func() { _ = c.Close() }()
	if err := c.Hello("host.example"); err != nil {
		t.Fatalf("HELO: %v", err)
	}
	err = c.Mail("alice@example.org")
	if err == nil {
		t.Fatal("MAIL FROM should fail HELO identity check")
	}
	if code := smtpCodeOf(err); code != 554 {
		t.Errorf("HELO-identity code: got %d, want 554 (err=%v)", code, err)
	}
	if !strings.Contains(err.Error(), "HELO identity") {
		t.Errorf("HELO-identity error string should mention 'HELO identity', got: %v", err)
	}
}

// senderDomainPolicy builds a Policy whose only active envelope gate is
// the sender-domain MX/A check (HELO identity + FCrDNS off), so the
// MAIL FROM tests below isolate the sender-domain verdict. skipLoopback
// drives the check ON for the loopback test peer (127.0.0.1); leave it
// false to exercise the production loopback exemption.
func senderDomainPolicy(res *fakeResolver, skipLoopback bool) *Policy {
	clk := newFakeClock(time.Date(2026, 5, 14, 12, 0, 0, 0, time.UTC))
	return &Policy{
		RateLimiter:              NewRateLimiter(60, time.Minute, clk),
		DNSBL:                    NewDNSBLChecker(nil, res),
		FCrDNS:                   NewFCrDNSChecker(res),
		FCrDNSMode:               FCrDNSModeOff,
		HELOResolver:             res,
		SenderDomain:             NewSenderDomainChecker(res),
		SkipSenderDomainLoopback: skipLoopback,
	}
}

// TestInboundBackendSenderDomainRejects drives a MAIL FROM whose sender
// domain has neither MX nor A/AAAA → 550 5.7.1 (smtp-server.md
// § Sender-domain, Error-tempfail row 254). skipLoopback=true forces the
// check on for the 127.0.0.1 test peer (mirrors the HELO-identity test).
func TestInboundBackendSenderDomainRejects(t *testing.T) {
	t.Parallel()
	res := newFakeResolver()
	res.nxMX["nodns.example"] = true
	res.nxHost["nodns.example"] = true
	addr, cancel := startPolicyListener(t, senderDomainPolicy(res, true))
	defer cancel()
	c, err := smtp.Dial(addr)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer func() { _ = c.Close() }()
	if err := c.Hello("relay.test.example"); err != nil {
		t.Fatalf("HELO: %v", err)
	}
	err = c.Mail("spammer@nodns.example")
	if err == nil {
		t.Fatal("MAIL FROM with no-DNS sender domain should be rejected")
	}
	if code := smtpCodeOf(err); code != 550 {
		t.Errorf("sender-domain reject code: got %d, want 550 (err=%v)", code, err)
	}
}

// TestInboundBackendSenderDomainAcceptsMX confirms a sender domain with
// an MX record passes the gate (MAIL FROM accepted) even with the
// loopback exemption forced off.
func TestInboundBackendSenderDomainAcceptsMX(t *testing.T) {
	t.Parallel()
	res := newFakeResolver()
	res.mx["good.example"] = []*net.MX{{Host: "mx.good.example.", Pref: 10}}
	addr, cancel := startPolicyListener(t, senderDomainPolicy(res, true))
	defer cancel()
	c, err := smtp.Dial(addr)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer func() { _ = c.Close() }()
	if err := c.Hello("relay.test.example"); err != nil {
		t.Fatalf("HELO: %v", err)
	}
	if err := c.Mail("alice@good.example"); err != nil {
		t.Fatalf("MAIL FROM with MX-having sender domain should be accepted: %v", err)
	}
}

// TestInboundBackendSenderDomainNullSenderBypasses confirms the null
// sender `<>` (RFC 5321 §4.5.5 bounce traffic) skips the MX/A check even
// when the gate is forced on and would otherwise reject every domain.
func TestInboundBackendSenderDomainNullSenderBypasses(t *testing.T) {
	t.Parallel()
	res := newFakeResolver() // every domain NXDOMAIN by default
	addr, cancel := startPolicyListener(t, senderDomainPolicy(res, true))
	defer cancel()
	c, err := smtp.Dial(addr)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer func() { _ = c.Close() }()
	if err := c.Hello("relay.test.example"); err != nil {
		t.Fatalf("HELO: %v", err)
	}
	if err := c.Mail(""); err != nil {
		t.Fatalf("null sender <> must bypass the sender-domain check: %v", err)
	}
}

// TestInboundBackendSenderDomainLoopbackExempt locks in the production
// loopback exemption (skipLoopback=false): a loopback peer sending from a
// non-resolving sender domain is accepted, mirroring postfix's
// permit_mynetworks ordering. This is what keeps the DNS-free tier_3
// inbound-MX e2e (sender@external.test from 127.0.0.1) passing.
func TestInboundBackendSenderDomainLoopbackExempt(t *testing.T) {
	t.Parallel()
	res := newFakeResolver() // every domain NXDOMAIN by default
	addr, cancel := startPolicyListener(t, senderDomainPolicy(res, false))
	defer cancel()
	c, err := smtp.Dial(addr)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer func() { _ = c.Close() }()
	if err := c.Hello("relay.test.example"); err != nil {
		t.Fatalf("HELO: %v", err)
	}
	if err := c.Mail("cron@external.test"); err != nil {
		t.Fatalf("loopback peer must be exempt from the sender-domain check: %v", err)
	}
}

// ── Phase C.3 mock Caller helpers ──────────────────────────────────

// rcptDecision is the per-(local_part, domain) outcome the mockCaller
// returns for a `fauna.bridges.validate_recipient` call.
type rcptDecision struct {
	resolved bool   // true → return resolved + ActorID; false → use reject path
	actorID  []byte // when resolved=true
	reason   string // when resolved=false: nest-supplied reject reason
	err      error  // when non-nil: transport-class error (overrides outcome)
	// resolve_recipient extensions (the inbound RCPT path; validate_recipient,
	// still used by submission, reads only the fields above). isRoleAddress +
	// headersToStamp ride a Resolved outcome; forward/forwardTarget/
	// forwarderActorID select the Forward outcome; rejectCode overrides the
	// default 550 on a reject.
	isRoleAddress    bool
	headersToStamp   []wsrpc.StampedHeader
	forward          bool
	forwardTarget    string
	forwarderActorID []byte
	rejectCode       uint16
	discard          bool // resolve_recipient → Discard (RFC 8058 mailto unsubscribe)
}

// mockCaller is the test substitute for *wsrpc.Client. It exposes
// `decisions` — a map keyed by `"<local_part>@<domain>"` — that
// drives the validate_recipient outcome. Unmapped recipients return
// a generic reject. Captures the number of calls per recipient for
// assertions about multi-RCPT behaviour.
type mockCaller struct {
	mu        sync.Mutex
	decisions map[string]rcptDecision
	calls     map[string]int
	// Phase C.9 method support. mlsPubkeys / indexPubkeys are keyed by
	// hex-encoded actor_id and consulted from the
	// fetch_recipient_{mls,index}_key handlers; a missing entry returns
	// a nil pubkey (Option<ByteBuf>::None on the wire) — matches the
	// "no provisioning yet" path the bridge falls back from.
	mlsPubkeys      map[string][]byte
	indexPubkeys    map[string][]byte
	ingestRequests  []ingestRecord
	ingestMessageID []byte
	// mlsSuccessionPending is keyed by hex(actor_id); a present, true entry
	// rides `succession_pending: true` on an mlsPubkeys-miss reply — a
	// succession's successor who has not yet re-provisioned a key
	// (smtp-server.md § Error / tempfail strategy). Meaningless for an
	// actor with an mlsPubkeys entry.
	mlsSuccessionPending map[string]bool
	// mail-forwarding N2. forwardConfigs is keyed by hex(actor_id); a
	// missing entry → forward-all disabled (Option::None). forwardRequests
	// captures every forward_message call for assertions.
	forwardConfigs       map[string]string
	forwardRequests      []forwardRecord
	forwardConfigFetches int // count of fetch_recipient_forward_config calls
	// forwardMessageErr, when non-nil, is returned from every forward_message
	// call (after the request is recorded) — nest refusing the enqueue (an
	// over-ceiling `redirect`, mail-forwarding.md § Queue ceiling) or a
	// transport failure; the forward was NOT durably enqueued.
	forwardMessageErr error
	// mail-forwarding N4. srsBounceDecisions drives decode_srs_bounce, keyed by
	// the RCPT local-part; a missing entry returns outcome "not_srs" (so a
	// plain SRS-prefixed local-part falls through to validate_recipient).
	srsBounceDecisions map[string]srsBounceDecision
	// filterRules drives fetch_recipient_filters, keyed by hex(actor_id); a
	// missing entry → no rules (the common case, spam-disposition placement).
	filterRules map[string][]wsrpc.EmailFilterWire
	// ingestErr, when non-nil, is returned from every ingest/submit call
	// (after the request is recorded) so tests can drive the error-mapping
	// paths — e.g. a *wsrpc.ServerError carrying an over_quota RpcError.
	ingestErr error
	// Greylist (check_greylist). greylistPass defaults to true (accept) so
	// the validate_recipient RCPT tests aren't greylisted; greylistErr forces
	// the fail-open path.
	greylistPass bool
	greylistErr  error
}

// srsBounceDecision drives the mockCaller's decode_srs_bounce reply for one
// local-part (mail-forwarding N4). `err` forces the transport-error path.
type srsBounceDecision struct {
	outcome     string
	forwarder   []byte
	sender      string
	destination string
	err         error
}

// forwardRecord captures one forward_message call (mail-forwarding N2).
type forwardRecord struct {
	actorID            []byte
	originalMsgID      string
	originalSender     string
	destination        string
	rawMessage         []byte
	ruleIDOrForwardAll string
	copyMode           string
}

// ingestRecord captures one ingest_inbound_mail (or
// submit_inbound_mail) call so tests can assert the request shape.
type ingestRecord struct {
	method             string
	actorID            []byte
	encryptedBody      []byte
	encryptedIndexHint []byte
	publicMetadata     wsrpcPublicMetadata
	verdictsRaw        []byte // raw CBOR — tests can decode if needed
	spamScore          uint32
	spamDisposition    string
	isRoleAddress      bool
}

// wsrpcPublicMetadata mirrors wsrpc.PublicMailMetadata for the
// mockCaller's request-side decode — kept local to the test so the
// production type can evolve without breaking the mock.
type wsrpcPublicMetadata struct {
	Timestamp      int64  `cbor:"timestamp"`
	CiphertextSize uint32 `cbor:"ciphertext_size"`
	SenderDomain   string `cbor:"sender_domain"`
}

func newMockCaller() *mockCaller {
	return &mockCaller{
		decisions:            map[string]rcptDecision{},
		calls:                map[string]int{},
		mlsPubkeys:           map[string][]byte{},
		mlsSuccessionPending: map[string]bool{},
		indexPubkeys:         map[string][]byte{},
		ingestMessageID:      bytes.Repeat([]byte{0xAB}, 32),
		forwardConfigs:       map[string]string{},
		srsBounceDecisions:   map[string]srsBounceDecision{},
		filterRules:          map[string][]wsrpc.EmailFilterWire{},
		greylistPass:         true,
	}
}

func (m *mockCaller) Call(_ context.Context, method string, body, reply any) error {
	bodyBytes, err := cbor.Marshal(body)
	if err != nil {
		return fmt.Errorf("mockCaller: encode body: %w", err)
	}
	switch method {
	case wsrpc.MethodValidateRecipient:
		return m.callValidateRecipient(bodyBytes, reply)
	case wsrpc.MethodResolveRecipient:
		return m.callResolveRecipient(bodyBytes, reply)
	case wsrpc.MethodCheckGreylist:
		return m.callCheckGreylist(reply)
	case wsrpc.MethodFetchRecipientMLSPubkey:
		return m.callFetchRecipientMLSPubkey(bodyBytes, reply)
	case wsrpc.MethodFetchRecipientIndexKey:
		return m.callFetchRecipientPubkey(bodyBytes, m.indexPubkeys, reply)
	case wsrpc.MethodIngestInboundMail, wsrpc.MethodSubmitInboundMail:
		return m.callIngest(method, bodyBytes, reply)
	case wsrpc.MethodFetchRecipientForwardConfig:
		return m.callFetchRecipientForwardConfig(bodyBytes, reply)
	case wsrpc.MethodForwardMessage:
		return m.callForwardMessage(bodyBytes, reply)
	case wsrpc.MethodDecodeSrsBounce:
		return m.callDecodeSrsBounce(bodyBytes, reply)
	case wsrpc.MethodFetchRecipientFilters:
		return m.callFetchRecipientFilters(bodyBytes, reply)
	default:
		return fmt.Errorf("mockCaller: unexpected method %q", method)
	}
}

func (m *mockCaller) callValidateRecipient(bodyBytes []byte, reply any) error {
	var req struct {
		LocalPart string `cbor:"local_part"`
		Domain    string `cbor:"domain"`
	}
	if err := cbor.Unmarshal(bodyBytes, &req); err != nil {
		return fmt.Errorf("mockCaller: decode validate_recipient: %w", err)
	}
	key := req.LocalPart + "@" + req.Domain
	m.mu.Lock()
	m.calls[key]++
	dec, ok := m.decisions[key]
	m.mu.Unlock()
	if dec.err != nil {
		return dec.err
	}
	var replyMap map[string]any
	switch {
	case ok && dec.resolved:
		replyMap = map[string]any{"outcome": "resolved", "actor_id": dec.actorID}
	case ok:
		replyMap = map[string]any{"outcome": "reject", "reason": dec.reason}
	default:
		replyMap = map[string]any{"outcome": "reject", "reason": "unknown_local_part"}
	}
	replyBytes, err := cbor.Marshal(replyMap)
	if err != nil {
		return fmt.Errorf("mockCaller: encode reply: %w", err)
	}
	return cbor.Unmarshal(replyBytes, reply)
}

// callResolveRecipient drives the inbound RCPT path's resolve_recipient outcome
// from the same `decisions` map validate_recipient used (so existing inbound
// fixtures carry over): a resolved decision → Resolved (+ optional
// is_role_address / headers_to_stamp), a `forward` decision → Forward, anything
// else → Reject (smtp_code defaults to 550).
func (m *mockCaller) callResolveRecipient(bodyBytes []byte, reply any) error {
	var req struct {
		LocalPart string `cbor:"local_part"`
		Domain    string `cbor:"domain"`
	}
	if err := cbor.Unmarshal(bodyBytes, &req); err != nil {
		return fmt.Errorf("mockCaller: decode resolve_recipient: %w", err)
	}
	key := req.LocalPart + "@" + req.Domain
	m.mu.Lock()
	m.calls[key]++
	dec, ok := m.decisions[key]
	m.mu.Unlock()
	if dec.err != nil {
		return dec.err
	}
	var replyMap map[string]any
	switch {
	case ok && dec.discard:
		replyMap = map[string]any{"outcome": "discard"}
	case ok && dec.forward:
		replyMap = map[string]any{
			"outcome":            "forward",
			"forward_target":     dec.forwardTarget,
			"forwarder_actor_id": dec.forwarderActorID,
		}
	case ok && dec.resolved:
		replyMap = map[string]any{
			"outcome":         "resolved",
			"actor_id":        dec.actorID,
			"is_role_address": dec.isRoleAddress,
		}
		if len(dec.headersToStamp) > 0 {
			hs := make([]map[string]any, len(dec.headersToStamp))
			for i, h := range dec.headersToStamp {
				hs[i] = map[string]any{"name": h.Name, "value": h.Value}
			}
			replyMap["headers_to_stamp"] = hs
		}
	default:
		code := dec.rejectCode
		if code == 0 {
			code = 550
		}
		reason := dec.reason
		if !ok {
			reason = "unknown_local_part"
		}
		replyMap = map[string]any{"outcome": "reject", "smtp_code": code, "reason": reason}
	}
	replyBytes, err := cbor.Marshal(replyMap)
	if err != nil {
		return fmt.Errorf("mockCaller: encode resolve_recipient reply: %w", err)
	}
	return cbor.Unmarshal(replyBytes, reply)
}

func (m *mockCaller) callFetchRecipientFilters(bodyBytes []byte, reply any) error {
	var req struct {
		ActorID []byte `cbor:"actor_id"`
	}
	if err := cbor.Unmarshal(bodyBytes, &req); err != nil {
		return fmt.Errorf("mockCaller: decode fetch_recipient_filters: %w", err)
	}
	m.mu.Lock()
	rules := m.filterRules[hex.EncodeToString(req.ActorID)]
	m.mu.Unlock()
	replyBytes, err := cbor.Marshal(map[string]any{"filters": rules})
	if err != nil {
		return fmt.Errorf("mockCaller: encode fetch_recipient_filters reply: %w", err)
	}
	return cbor.Unmarshal(replyBytes, reply)
}

func (m *mockCaller) callCheckGreylist(reply any) error {
	m.mu.Lock()
	greylistErr, pass := m.greylistErr, m.greylistPass
	m.mu.Unlock()
	if greylistErr != nil {
		return greylistErr
	}
	replyBytes, err := cbor.Marshal(map[string]any{"pass": pass})
	if err != nil {
		return fmt.Errorf("mockCaller: encode check_greylist reply: %w", err)
	}
	return cbor.Unmarshal(replyBytes, reply)
}

// callFetchRecipientMLSPubkey is the MLS-pubkey-specific sibling of
// callFetchRecipientPubkey — the MLS reply carries `succession_pending`
// (smtp-server.md § Error / tempfail strategy) while the index-key reply
// does not, so they can't share one generic helper once that field exists.
func (m *mockCaller) callFetchRecipientMLSPubkey(bodyBytes []byte, reply any) error {
	var req struct {
		ActorID []byte `cbor:"actor_id"`
	}
	if err := cbor.Unmarshal(bodyBytes, &req); err != nil {
		return fmt.Errorf("mockCaller: decode fetch_recipient_mls_pubkey: %w", err)
	}
	m.mu.Lock()
	pk, ok := m.mlsPubkeys[hex.EncodeToString(req.ActorID)]
	successionPending := m.mlsSuccessionPending[hex.EncodeToString(req.ActorID)]
	m.mu.Unlock()
	replyMap := map[string]any{}
	if ok {
		replyMap["key"] = wsrpctest.RecipientSealKey(pk)
	} else {
		replyMap["key"] = nil
		replyMap["succession_pending"] = successionPending
	}
	replyBytes, err := cbor.Marshal(replyMap)
	if err != nil {
		return fmt.Errorf("mockCaller: encode MLS pubkey reply: %w", err)
	}
	return cbor.Unmarshal(replyBytes, reply)
}

func (m *mockCaller) callFetchRecipientPubkey(bodyBytes []byte, table map[string][]byte, reply any) error {
	var req struct {
		ActorID []byte `cbor:"actor_id"`
	}
	if err := cbor.Unmarshal(bodyBytes, &req); err != nil {
		return fmt.Errorf("mockCaller: decode fetch_recipient_*_pubkey: %w", err)
	}
	m.mu.Lock()
	pk, ok := table[hex.EncodeToString(req.ActorID)]
	m.mu.Unlock()
	var replyMap map[string]any
	if ok {
		replyMap = map[string]any{"pubkey": pk}
	} else {
		replyMap = map[string]any{"pubkey": nil}
	}
	replyBytes, err := cbor.Marshal(replyMap)
	if err != nil {
		return fmt.Errorf("mockCaller: encode pubkey reply: %w", err)
	}
	return cbor.Unmarshal(replyBytes, reply)
}

func (m *mockCaller) callIngest(method string, bodyBytes []byte, reply any) error {
	var req struct {
		ActorID            []byte              `cbor:"actor_id"`
		EncryptedBody      []byte              `cbor:"encrypted_body"`
		EncryptedIndexHint []byte              `cbor:"encrypted_index_hint"`
		PublicMetadata     wsrpcPublicMetadata `cbor:"public_metadata"`
		Verdicts           cbor.RawMessage     `cbor:"verdicts"`
		SpamScore          uint32              `cbor:"spam_score"`
		SpamDisposition    string              `cbor:"spam_disposition"`
		IsRoleAddress      bool                `cbor:"is_role_address"`
	}
	if err := cbor.Unmarshal(bodyBytes, &req); err != nil {
		return fmt.Errorf("mockCaller: decode %s: %w", method, err)
	}
	m.mu.Lock()
	m.ingestRequests = append(m.ingestRequests, ingestRecord{
		method:             method,
		actorID:            req.ActorID,
		encryptedBody:      req.EncryptedBody,
		encryptedIndexHint: req.EncryptedIndexHint,
		publicMetadata:     req.PublicMetadata,
		verdictsRaw:        req.Verdicts,
		spamScore:          req.SpamScore,
		spamDisposition:    req.SpamDisposition,
		isRoleAddress:      req.IsRoleAddress,
	})
	injectedErr := m.ingestErr
	id := m.ingestMessageID
	m.mu.Unlock()
	if injectedErr != nil {
		return injectedErr
	}
	replyMap := map[string]any{"message_id": id}
	replyBytes, err := cbor.Marshal(replyMap)
	if err != nil {
		return fmt.Errorf("mockCaller: encode ingest reply: %w", err)
	}
	return cbor.Unmarshal(replyBytes, reply)
}

func (m *mockCaller) callFetchRecipientForwardConfig(bodyBytes []byte, reply any) error {
	var req struct {
		ActorID []byte `cbor:"actor_id"`
	}
	if err := cbor.Unmarshal(bodyBytes, &req); err != nil {
		return fmt.Errorf("mockCaller: decode fetch_recipient_forward_config: %w", err)
	}
	m.mu.Lock()
	m.forwardConfigFetches++
	target, ok := m.forwardConfigs[hex.EncodeToString(req.ActorID)]
	m.mu.Unlock()
	var replyMap map[string]any
	if ok {
		replyMap = map[string]any{"forward_all_to": target}
	} else {
		replyMap = map[string]any{"forward_all_to": nil} // disabled
	}
	replyBytes, err := cbor.Marshal(replyMap)
	if err != nil {
		return fmt.Errorf("mockCaller: encode forward-config reply: %w", err)
	}
	return cbor.Unmarshal(replyBytes, reply)
}

func (m *mockCaller) callForwardMessage(bodyBytes []byte, reply any) error {
	var req struct {
		ActorID            []byte `cbor:"actor_id"`
		OriginalMsgID      string `cbor:"original_msgid"`
		OriginalSender     string `cbor:"original_sender"`
		Destination        string `cbor:"destination"`
		RawMessage         []byte `cbor:"raw_message"`
		RuleIDOrForwardAll string `cbor:"rule_id_or_forward_all"`
		CopyMode           string `cbor:"copy_mode"`
	}
	if err := cbor.Unmarshal(bodyBytes, &req); err != nil {
		return fmt.Errorf("mockCaller: decode forward_message: %w", err)
	}
	m.mu.Lock()
	m.forwardRequests = append(m.forwardRequests, forwardRecord{
		actorID:            req.ActorID,
		originalMsgID:      req.OriginalMsgID,
		originalSender:     req.OriginalSender,
		destination:        req.Destination,
		rawMessage:         req.RawMessage,
		ruleIDOrForwardAll: req.RuleIDOrForwardAll,
		copyMode:           req.CopyMode,
	})
	id := int64(len(m.forwardRequests)) // 1-based row id
	forwardErr := m.forwardMessageErr
	m.mu.Unlock()
	if forwardErr != nil {
		return forwardErr
	}
	replyBytes, err := cbor.Marshal(map[string]any{"id": id})
	if err != nil {
		return fmt.Errorf("mockCaller: encode forward reply: %w", err)
	}
	return cbor.Unmarshal(replyBytes, reply)
}

func (m *mockCaller) callDecodeSrsBounce(bodyBytes []byte, reply any) error {
	var req struct {
		LocalPart string `cbor:"local_part"`
	}
	if err := cbor.Unmarshal(bodyBytes, &req); err != nil {
		return fmt.Errorf("mockCaller: decode decode_srs_bounce: %w", err)
	}
	m.mu.Lock()
	dec, ok := m.srsBounceDecisions[req.LocalPart]
	m.mu.Unlock()
	if dec.err != nil {
		return dec.err
	}
	replyMap := map[string]any{}
	if !ok {
		// No mapping → nest says it's not actually an SRS address.
		replyMap["outcome"] = "not_srs"
	} else {
		replyMap["outcome"] = dec.outcome
		if dec.outcome == "ok" {
			replyMap["forwarder_actor_id"] = dec.forwarder
			replyMap["original_sender"] = dec.sender
			replyMap["original_destination"] = dec.destination
		}
	}
	replyBytes, err := cbor.Marshal(replyMap)
	if err != nil {
		return fmt.Errorf("mockCaller: encode decode_srs_bounce reply: %w", err)
	}
	return cbor.Unmarshal(replyBytes, reply)
}

// TestInboundBackendRcptRejectsForeignDomain asserts the domain
// filter — a RCPT TO for a domain other than the bridge's served
// domain is `550 5.7.1 Relay access denied` without calling nest.
func TestInboundBackendRcptRejectsForeignDomain(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
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
	if err := c.Hello("loopback.example"); err != nil {
		t.Fatalf("HELO: %v", err)
	}
	if err := c.Mail("alice@example.org"); err != nil {
		t.Fatalf("MAIL: %v", err)
	}
	err = c.Rcpt("bob@other.example") // not test.example
	if err == nil {
		t.Fatal("RCPT TO for foreign domain must be rejected as relay")
	}
	if code := smtpCodeOf(err); code != 550 {
		t.Errorf("relay-reject code: got %d, want 550 (err=%v)", code, err)
	}
	if !strings.Contains(err.Error(), "Relay") {
		t.Errorf("relay-reject message should mention 'Relay'; got: %v", err)
	}
	// The domain filter short-circuits before any nest call.
	if len(mc.calls) != 0 {
		t.Errorf("foreign-domain RCPT must not call nest; got %d calls: %+v", len(mc.calls), mc.calls)
	}
}

// TestInboundBackendRcptRejectsUnknownLocalPart drives the nest-side
// "reject" path for a local part the validate_recipient handler
// doesn't recognise.
func TestInboundBackendRcptRejectsUnknownLocalPart(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	mc.decisions["mallory@test.example"] = rcptDecision{resolved: false, reason: "unknown_local_part"}
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
	if err := c.Hello("loopback.example"); err != nil {
		t.Fatalf("HELO: %v", err)
	}
	if err := c.Mail("alice@example.org"); err != nil {
		t.Fatalf("MAIL: %v", err)
	}
	err = c.Rcpt("mallory@test.example")
	if err == nil {
		t.Fatal("RCPT TO for unknown local part must be rejected")
	}
	if code := smtpCodeOf(err); code != 550 {
		t.Errorf("unknown-recipient code: got %d, want 550 (err=%v)", code, err)
	}
}

// TestInboundBackendRcptResolvesAndStoresActorID drives the happy
// path — two RCPT TOs for known local parts return 250 and each
// call hits nest exactly once. The wire-level "250 accepted" combined
// with the recorded call counts is the observable contract for C.3;
// the in-session `inboundRcpts` slice is internal scaffolding for C.9's
// ingest call and the C.9 test will assert on it.
func TestInboundBackendRcptResolvesAndStoresActorID(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	bobID := bytes.Repeat([]byte{0x42}, 32)
	carolID := bytes.Repeat([]byte{0x43}, 32)
	mc.decisions["bob@test.example"] = rcptDecision{resolved: true, actorID: bobID}
	mc.decisions["carol@test.example"] = rcptDecision{resolved: true, actorID: carolID}
	mc.mlsPubkeys[hex.EncodeToString(bobID)] = freshX25519Pubkey(t)
	mc.mlsPubkeys[hex.EncodeToString(carolID)] = freshX25519Pubkey(t)
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
	if err := c.Hello("loopback.example"); err != nil {
		t.Fatalf("HELO: %v", err)
	}
	if err := c.Mail("alice@example.org"); err != nil {
		t.Fatalf("MAIL: %v", err)
	}
	if err := c.Rcpt("bob@test.example"); err != nil {
		t.Fatalf("first RCPT: %v", err)
	}
	if err := c.Rcpt("carol@test.example"); err != nil {
		t.Fatalf("second RCPT: %v", err)
	}
	if mc.calls["bob@test.example"] != 1 {
		t.Errorf("bob nest call count: got %d, want 1", mc.calls["bob@test.example"])
	}
	if mc.calls["carol@test.example"] != 1 {
		t.Errorf("carol nest call count: got %d, want 1", mc.calls["carol@test.example"])
	}
}

// TestInboundBackendRcptDiscardAcceptsAndDropsBody — RFC 8058 mailto one-click
// unsubscribe (mail-mass-mailing.md § The mailto handler). resolve_recipient
// returns Discard for `unsubscribe+<token>@`; the bridge accepts the RCPT (250)
// and the DATA (250) but never ingests — the unsubscribe side effect already
// fired nest-side at RCPT. The observable contract: both 250s + zero ingest.
func TestInboundBackendRcptDiscardAcceptsAndDropsBody(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	mc.decisions["unsubscribe+MailTok_9-x@test.example"] = rcptDecision{discard: true}
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
	if err := c.Hello("loopback.example"); err != nil {
		t.Fatalf("HELO: %v", err)
	}
	if err := c.Mail("alice@example.org"); err != nil {
		t.Fatalf("MAIL: %v", err)
	}
	// RCPT accepted with 250 (token case preserved end-to-end).
	if err := c.Rcpt("unsubscribe+MailTok_9-x@test.example"); err != nil {
		t.Fatalf("discard RCPT must be accepted (250): %v", err)
	}
	// DATA accepted with 250; the body is read and dropped.
	w, err := c.Data()
	if err != nil {
		t.Fatalf("DATA open: %v", err)
	}
	if _, err := io.WriteString(w, "Subject: unsubscribe\r\n\r\nList-Unsubscribe=One-Click\r\n"); err != nil {
		t.Fatalf("write body: %v", err)
	}
	if err := w.Close(); err != nil {
		t.Fatalf("discard DATA must be accepted (250): %v", err)
	}
	// Resolution happened exactly once; nothing was ingested (discarded).
	if mc.calls["unsubscribe+MailTok_9-x@test.example"] != 1 {
		t.Errorf("resolve call count: got %d, want 1", mc.calls["unsubscribe+MailTok_9-x@test.example"])
	}
	if got := len(mc.ingestRequests); got != 0 {
		t.Errorf("discard-only envelope must not ingest: got %d ingest calls", got)
	}
}

// TestInboundBackendRcptTempfailsOnTransportError — when the nest
// call fails for a transport reason (not a nest-side reject), the
// bridge surfaces 451 4.7.0 so the sender retries rather than the
// message being permanently bounced.
func TestInboundBackendRcptTempfailsOnTransportError(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	mc.decisions["bob@test.example"] = rcptDecision{err: errors.New("nest connection refused")}
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
	if err := c.Hello("loopback.example"); err != nil {
		t.Fatalf("HELO: %v", err)
	}
	if err := c.Mail("alice@example.org"); err != nil {
		t.Fatalf("MAIL: %v", err)
	}
	err = c.Rcpt("bob@test.example")
	if err == nil {
		t.Fatal("transport error must tempfail")
	}
	if code := smtpCodeOf(err); code != 451 {
		t.Errorf("transport-error code: got %d, want 451 (err=%v)", code, err)
	}
}

// Greylisting moved nest-side: the
// state + the defer-then-accept-after-delay timing live in nest's
// `greylist_tuples` + `fauna.bridges.check_greylist` (unit-tested in
// `fauna_mail::greylist` + the nest handler; round-trip in the tier_3 e2e).
// The two tests below pin only the Go MTA's *wire mapping* of the nest
// verdict at RCPT TO, via the mockCaller's `greylistPass` field. The gate
// runs after validate_recipient, so the recipient must resolve first.

// TestInboundBackendGreylistDeferMaps451 — nest defer (pass=false) ⇒ 451 4.7.1.
func TestInboundBackendGreylistDeferMaps451(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	bobID := bytes.Repeat([]byte{0x01}, 32)
	mc.decisions["bob@test.example"] = rcptDecision{resolved: true, actorID: bobID}
	mc.mlsPubkeys[hex.EncodeToString(bobID)] = freshX25519Pubkey(t)
	mc.greylistPass = false
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
	if err := c.Hello("loopback.example"); err != nil {
		t.Fatalf("HELO: %v", err)
	}
	if err := c.Mail("alice@example.org"); err != nil {
		t.Fatalf("MAIL: %v", err)
	}
	err = c.Rcpt("bob@test.example")
	if err == nil {
		t.Fatal("greylist defer must tempfail at RCPT")
	}
	if code := smtpCodeOf(err); code != 451 {
		t.Errorf("greylist defer code: got %d, want 451 (err=%v)", code, err)
	}
}

// TestInboundBackendGreylistPassAccepts — nest pass (pass=true) ⇒ RCPT 250.
func TestInboundBackendGreylistPassAccepts(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	bobID := bytes.Repeat([]byte{0x01}, 32)
	mc.decisions["bob@test.example"] = rcptDecision{resolved: true, actorID: bobID}
	mc.mlsPubkeys[hex.EncodeToString(bobID)] = freshX25519Pubkey(t)
	mc.greylistPass = true
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
	if err := c.Hello("loopback.example"); err != nil {
		t.Fatalf("HELO: %v", err)
	}
	if err := c.Mail("alice@example.org"); err != nil {
		t.Fatalf("MAIL: %v", err)
	}
	if err := c.Rcpt("bob@test.example"); err != nil {
		t.Fatalf("greylist pass must accept RCPT, got: %v", err)
	}
}

// ── Phase C.4 Data-time tests ─────────────────────────────────────
//
// These exercise inboundSession.Data directly (rather than via a real
// SMTP listener) so the size-guard math + parse wiring are unit tests
// rather than wire-level tests. The wire-level pipeline is implicit in
// the existing C.1/C.2/C.3 tests; what's new here is "Data buffers,
// guards, parses, and stashes the result on the session".

// TestInboundBackendDataParsesSimpleMessage drives the happy path —
// a small valid RFC 5322 message under the configured size cap parses
// to a non-nil inboundSession.parsed with the expected From/Subject.
func TestInboundBackendDataParsesSimpleMessage(t *testing.T) {
	t.Parallel()
	s := &inboundSession{
		logger:          slog.Default(),
		clientIP:        "127.0.0.1",
		from:            "alice@example.org",
		maxMessageBytes: 50_000_000,
		spamPolicy:      permissiveSpamPolicyForTest(),
	}
	body := "From: alice@example.org\r\n" +
		"To: bob@test.example\r\n" +
		"Subject: parsed-by-c4\r\n" +
		"\r\n" +
		"hello fauna\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if s.parsed == nil {
		t.Fatal("Data did not stash parsed message on session")
	}
	if s.parsed.From != "alice@example.org" {
		t.Errorf("parsed.From: got %q, want %q", s.parsed.From, "alice@example.org")
	}
	if s.parsed.Subject != "parsed-by-c4" {
		t.Errorf("parsed.Subject: got %q, want %q", s.parsed.Subject, "parsed-by-c4")
	}
}

// TestInboundBackendDataSizeGuardRejects asserts the pre-parser size
// guard fires before ParseRFC5322 is called: a message exceeding the
// session's maxMessageBytes returns 552 5.3.4 and leaves parsed == nil
// so the C.5-C.9 pipeline can't act on an oversized body.
func TestInboundBackendDataSizeGuardRejects(t *testing.T) {
	t.Parallel()
	s := &inboundSession{
		logger:          slog.Default(),
		clientIP:        "127.0.0.1",
		from:            "alice@example.org",
		maxMessageBytes: 100,
	}
	// 200 bytes — comfortably over the 100-byte cap.
	body := bytes.Repeat([]byte("x"), 200)
	err := s.Data(bytes.NewReader(body))
	if err == nil {
		t.Fatal("oversized body must trigger 552 reject")
	}
	var smtpErr *gosmtp.SMTPError
	if !errors.As(err, &smtpErr) {
		t.Fatalf("expected *gosmtp.SMTPError, got %T: %v", err, err)
	}
	if smtpErr.Code != 552 {
		t.Errorf("size-guard code: got %d, want 552", smtpErr.Code)
	}
	if smtpErr.EnhancedCode != (gosmtp.EnhancedCode{5, 3, 4}) {
		t.Errorf("size-guard enhanced code: got %v, want {5,3,4}", smtpErr.EnhancedCode)
	}
	if s.parsed != nil {
		t.Error("size guard must not stash a parsed message on the session")
	}
}

// TestInboundDataRefusesOtherThanOneFromField is the DATA-path witness for the
// From-field count (smtp-server.md § Inbound policy stack): a message whose
// header section carries other than exactly one From field is refused 554
// 5.6.0 before ParseRFC5322 and VerifyInbound ever see it. Given two, DMARC
// (mail-auth) aligns against the first while every app displays the last, so a
// DMARC-passing attacker would render as the deployment's own address. The
// one-From case is the control: it still reaches the parser.
func TestInboundDataRefusesOtherThanOneFromField(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name   string
		body   string
		refuse bool
	}{
		{"two From fields", "From: anyone@evil.test\r\nFrom: security@test.example\r\nTo: bob@test.example\r\n\r\nhi\r\n", true},
		{"upper then lower case", "FROM: anyone@evil.test\r\nfrom: security@test.example\r\n\r\nhi\r\n", true},
		{"LF-only", "From: anyone@evil.test\nFrom: security@test.example\n\nhi\n", true},
		{"space before the colon", "From : security@test.example\r\nFrom: anyone@evil.test\r\n\r\nhi\r\n", true},
		{"no From field", "To: bob@test.example\r\nSubject: hi\r\n\r\nhi\r\n", true},
		{"one From field", "From: anyone@evil.test\r\nTo: bob@test.example\r\nSubject: hi\r\n\r\nhi\r\n", false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			s := &inboundSession{
				logger:          slog.Default(),
				clientIP:        "127.0.0.1",
				from:            "anyone@evil.test",
				maxMessageBytes: 50_000_000,
				spamPolicy:      permissiveSpamPolicyForTest(),
				sleep:           func(time.Duration) {},
			}
			err := s.Data(bytes.NewReader([]byte(tc.body)))
			if !tc.refuse {
				if err != nil {
					t.Fatalf("one From field must be accepted; got %v", err)
				}
				if s.parsed == nil {
					t.Fatal("one From field must reach the parser")
				}
				return
			}
			var smtpErr *gosmtp.SMTPError
			if !errors.As(err, &smtpErr) {
				t.Fatalf("want 554 5.6.0, got %T: %v", err, err)
			}
			if smtpErr.Code != 554 || smtpErr.EnhancedCode != (gosmtp.EnhancedCode{5, 6, 0}) {
				t.Errorf("got %d %v %q, want 554 {5 6 0}", smtpErr.Code, smtpErr.EnhancedCode, smtpErr.Message)
			}
			if s.parsed != nil {
				t.Error("the refusal must come before ParseRFC5322 (and so before VerifyInbound)")
			}
		})
	}
}

// TestInboundBackendDataMultipartExposesAttachment exercises the C.4
// surface that this whole task is about: a multipart message with a
// text body + attachment is parsed, and the attachment shows up in
// inboundSession.parsed.MimeParts where C.9's encrypt-to-recipient
// pipeline can find it.
func TestInboundBackendDataMultipartExposesAttachment(t *testing.T) {
	t.Parallel()
	s := &inboundSession{
		logger:          slog.Default(),
		clientIP:        "127.0.0.1",
		from:            "alice@example.org",
		maxMessageBytes: 50_000_000,
		spamPolicy:      permissiveSpamPolicyForTest(),
	}
	body := "From: alice@example.org\r\n" +
		"To: bob@test.example\r\n" +
		"Subject: with attachment\r\n" +
		"MIME-Version: 1.0\r\n" +
		"Content-Type: multipart/mixed; boundary=\"b1\"\r\n" +
		"\r\n" +
		"--b1\r\n" +
		"Content-Type: text/plain; charset=utf-8\r\n" +
		"\r\n" +
		"hello with an attachment\r\n" +
		"\r\n" +
		"--b1\r\n" +
		"Content-Type: text/plain; name=\"test.txt\"\r\n" +
		"Content-Disposition: attachment; filename=\"test.txt\"\r\n" +
		"\r\n" +
		"file payload\r\n" +
		"--b1--\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if s.parsed == nil {
		t.Fatal("Data did not stash parsed message on session")
	}
	var foundAttachment bool
	for _, p := range s.parsed.MimeParts {
		if p.Disposition == "attachment" && p.Filename == "test.txt" {
			foundAttachment = true
			break
		}
	}
	if !foundAttachment {
		t.Errorf("expected an attachment MIME part with Filename=\"test.txt\"; got parts: %+v", s.parsed.MimeParts)
	}
}

// TestInboundBackendDataTokenizesAfterSpamGate confirms the C.8 wiring:
// a clean message that clears parse / verify / DKIM gate / spam gate
// has indexHint populated on the session. The deterministic shape
// (NFKC + word-segment + dedupe) is unit-tested in
// mailfauna_tokenize_test.go; this test pins the integration — that
// Session.Data actually runs Tokenize on Subject + BodyText after the
// C.7 gate accepts the message.
func TestInboundBackendDataTokenizesAfterSpamGate(t *testing.T) {
	t.Parallel()
	s := &inboundSession{
		logger:          slog.Default(),
		clientIP:        "127.0.0.1",
		from:            "alice@example.org",
		maxMessageBytes: 50_000_000,
		spamPolicy:      permissiveSpamPolicyForTest(),
	}
	body := "From: alice@example.org\r\n" +
		"To: bob@test.example\r\n" +
		"Subject: hello world\r\n" +
		"\r\n" +
		"the quick brown fox\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if s.indexHint == nil {
		t.Fatal("Data did not stash indexHint on session after C.7 gate")
	}
	wantPresent := []string{"brown", "fox", "hello", "quick", "the", "world"}
	tokSet := make(map[string]struct{}, len(s.indexHint.Tokens))
	for _, tok := range s.indexHint.Tokens {
		tokSet[tok] = struct{}{}
	}
	for _, w := range wantPresent {
		if _, ok := tokSet[w]; !ok {
			t.Errorf("expected token %q in indexHint.Tokens, got %v", w, s.indexHint.Tokens)
		}
	}
	if len(s.indexHint.CanonicalBytes) == 0 {
		t.Error("expected non-empty CanonicalBytes")
	}
}

// TestInboundBackendDataVerifyStashesVerdicts asserts that a valid
// inbound message clears the C.5 verify_inbound step and stashes the
// resulting AuthVerdicts on the session (where C.6's DKIM enforce
// gate and C.7's spam scorer will pick them up).
func TestInboundBackendDataVerifyStashesVerdicts(t *testing.T) {
	t.Parallel()
	s := &inboundSession{
		logger:          slog.Default(),
		clientIP:        "127.0.0.1",
		from:            "alice@example.org",
		maxMessageBytes: 50_000_000,
		spamPolicy:      permissiveSpamPolicyForTest(),
	}
	body := "From: alice@example.org\r\n" +
		"To: bob@test.example\r\n" +
		"Subject: verified-by-c5\r\n" +
		"\r\n" +
		"hello fauna\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if s.parsed == nil {
		t.Fatal("C.4 parse did not stash parsed message")
	}
	if s.verdicts == nil {
		t.Fatal("C.5 verify did not stash verdicts on session")
	}
}

// TestInboundBackendDataVerifyUnparseableRejects pins the C.5 wiring
// of AuthError::Unparseable → 554 5.6.0 on the SMTP wire: when the
// verifier reports the unparseable sentinel, Session.Data rejects with
// that code and stashes no verdicts.
//
// The input is verifyUnparseableFixture, a header line with no
// header/body separator: mail-parser accepts it, so Data reaches the
// verify step, and the package's canned verifier (main_test.go) answers
// it with mailfauna.ErrAuthErrorUnparseable. Which inputs mail-auth
// itself refuses is the Rust pipeline's business, not this mapping's (it
// accepts this one, which is why the test used to skip instead of assert).
func TestInboundBackendDataVerifyUnparseableRejects(t *testing.T) {
	t.Parallel()
	s := &inboundSession{
		logger:          slog.Default(),
		clientIP:        "127.0.0.1",
		from:            "alice@example.org",
		maxMessageBytes: 50_000_000,
	}
	err := s.Data(bytes.NewReader([]byte(verifyUnparseableFixture)))
	if err == nil {
		t.Fatal("Data accepted a message the verifier reported unparseable")
	}
	var smtpErr *gosmtp.SMTPError
	if !errors.As(err, &smtpErr) {
		t.Fatalf("expected *gosmtp.SMTPError, got %T: %v", err, err)
	}
	// Both ParseRFC5322 (mail-parser) and verify_inbound (mail-auth)
	// map their respective parse-failure paths to 554 5.6.0. Whichever
	// one fires here, the wire code must match the spec.
	if smtpErr.Code != 554 {
		t.Errorf("unparseable code: got %d, want 554", smtpErr.Code)
	}
	if smtpErr.EnhancedCode != (gosmtp.EnhancedCode{5, 6, 0}) {
		t.Errorf("unparseable enhanced code: got %v, want {5,6,0}", smtpErr.EnhancedCode)
	}
	if s.verdicts != nil {
		t.Error("verify failure must not stash verdicts")
	}
}

// freshX25519Pubkey generates a real X25519 pubkey for tests; the hpke
// crate's deserializer rejects small-order points (all-zero buffer), so
// we use crypto/ecdh to get a valid point on the curve.
func freshX25519Pubkey(t *testing.T) []byte {
	t.Helper()
	sk, err := ecdh.X25519().GenerateKey(rand.Reader)
	if err != nil {
		t.Fatalf("ecdh GenerateKey: %v", err)
	}
	return sk.PublicKey().Bytes()
}

// TestInboundBackendDataIngestsAfterFullPipeline is the Phase C.9
// end-to-end integration test: parse → verify → DKIM gate → spam gate
// → tokenize → encrypt → ingest. Asserts the bridge produces a valid
// `IngestInboundMailRequest` per recipient with the expected wire
// shape, and that the message_id reply is propagated successfully.
func TestInboundBackendDataIngestsAfterFullPipeline(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	mc.mlsPubkeys[hex.EncodeToString(actorBob)] = freshX25519Pubkey(t)
	// Leave indexPubkeys empty so the bridge falls back to the MLS
	// pubkey for the index hint (Phase C.9's documented behaviour;
	// Phase E will provision real index pubkeys).

	s := &inboundSession{
		logger:          slog.Default(),
		clientIP:        "127.0.0.1",
		from:            "alice@example.org",
		inboundRcpts:    []resolvedInboundRcpt{{actorID: actorBob}},
		caller:          mc,
		localDomains:    []string{"test.example"},
		maxMessageBytes: 50_000_000,
		spamPolicy:      permissiveSpamPolicyForTest(),
	}
	body := "From: alice@example.org\r\n" +
		"To: bob@test.example\r\n" +
		"Subject: c9 integration\r\n" +
		"Date: Sun, 17 Mar 2024 12:34:56 +0000\r\n" +
		"\r\n" +
		"hello fauna inbound pipeline\r\n"
	rawBytes := []byte(body)
	if err := s.Data(bytes.NewReader(rawBytes)); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.ingestRequests); got != 1 {
		t.Fatalf("ingest call count: got %d, want 1; requests=%+v", got, mc.ingestRequests)
	}
	req := mc.ingestRequests[0]
	if req.method != wsrpc.MethodIngestInboundMail {
		t.Errorf("method: got %q, want fauna.bridges.ingest_inbound_mail", req.method)
	}
	if !bytes.Equal(req.actorID, actorBob) {
		t.Errorf("actor_id: got %x, want %x", req.actorID, actorBob)
	}
	if len(req.encryptedBody) == 0 {
		t.Error("encrypted_body must be non-empty")
	}
	if bytes.Contains(req.encryptedBody, rawBytes) {
		t.Error("encrypted_body must not contain the literal plaintext")
	}
	if len(req.encryptedIndexHint) == 0 {
		t.Error("encrypted_index_hint must be non-empty (Phase C.9 fallback to MLS pubkey)")
	}
	// public_metadata: timestamp from the parsed Date header (Mar 17,
	// 2024 12:34:56 UTC → 1710678896), ciphertext_size == len(body),
	// sender_domain extracted from "From: alice@example.org".
	if req.publicMetadata.Timestamp != 1710678896 {
		t.Errorf("public_metadata.timestamp: got %d, want 1710678896", req.publicMetadata.Timestamp)
	}
	if got, want := req.publicMetadata.CiphertextSize, uint32(len(req.encryptedBody)); got != want {
		t.Errorf("public_metadata.ciphertext_size: got %d, want %d", got, want)
	}
	if req.publicMetadata.SenderDomain != "example.org" {
		t.Errorf("public_metadata.sender_domain: got %q, want %q", req.publicMetadata.SenderDomain, "example.org")
	}
	if req.spamDisposition != "accept" {
		t.Errorf("spam_disposition: got %q, want %q", req.spamDisposition, "accept")
	}
	// Verdicts CBOR is opaque to this test; the wire shape is pinned by
	// the methods_test.go ingest-roundtrip test. Verify it's non-empty.
	if len(req.verdictsRaw) == 0 {
		t.Error("verdicts CBOR must be non-empty")
	}
}

// TestInboundBackendDataPlaintextModeSealsToo pins Phase-3 D1 (design
// `2026-07-07-phase-3-sealed-both-modes-design.md`): a committed
// plaintext-mode deployment seals inbound mail at ingest exactly like an
// encrypted-mode one — the design-(b) no-seal-at-ingest branch is deleted
// STRUCTURALLY: the inboundSession no longer carries a storage-mode bit at
// all, so there is nothing to set here; one at-rest byte shape, no plaintext
// body ever ships to nest core regardless of deployment mode.
func TestInboundBackendDataPlaintextModeSealsToo(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	mc.mlsPubkeys[hex.EncodeToString(actorBob)] = freshX25519Pubkey(t)

	s := &inboundSession{
		logger:          slog.Default(),
		clientIP:        "127.0.0.1",
		from:            "alice@example.org",
		inboundRcpts:    []resolvedInboundRcpt{{actorID: actorBob}},
		caller:          mc,
		localDomains:    []string{"test.example"},
		maxMessageBytes: 50_000_000,
		spamPolicy:      permissiveSpamPolicyForTest(),
	}
	body := "From: alice@example.org\r\n" +
		"To: bob@test.example\r\n" +
		"Subject: plaintext at rest\r\n" +
		"Date: Sun, 17 Mar 2024 12:34:56 +0000\r\n" +
		"\r\n" +
		"hello formerly-unsealed plaintext body\r\n"
	rawBytes := []byte(body)
	if err := s.Data(bytes.NewReader(rawBytes)); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.ingestRequests); got != 1 {
		t.Fatalf("ingest call count: got %d, want 1", got)
	}
	req := mc.ingestRequests[0]
	// Phase-3 D1: the body ships SEALED — a valid MailRecordEnvelope with no
	// plaintext body/subject bytes visible, exactly the encrypted-mode shape.
	if !mailfauna.IsSealedMailRecord(req.encryptedBody) {
		t.Error("plaintext mode: encrypted_body must be a sealed MailRecordEnvelope (Phase-3 D1)")
	}
	if bytes.Contains(req.encryptedBody, []byte("hello formerly-unsealed plaintext body")) {
		t.Error("plaintext mode: encrypted_body must NOT contain the literal plaintext body")
	}
	if bytes.Contains(req.encryptedBody, []byte("plaintext at rest")) {
		t.Error("plaintext mode: encrypted_body must NOT contain the literal plaintext subject")
	}
	if !mailfauna.IsSealedMailRecord(req.encryptedIndexHint) {
		t.Error("plaintext mode: encrypted_index_hint must be sealed too (Phase-3 D1)")
	}
	if got, want := req.publicMetadata.CiphertextSize, uint32(len(req.encryptedBody)); got != want {
		t.Errorf("ciphertext_size: got %d, want %d", got, want)
	}
}

// TestInboundBackendDataIngestBouncesMissingMLSPubkey pins the
// 550 5.1.1 path when the recipient has no MLS pubkey provisioned —
// nest's ingest handler also enforces this, but the bridge bounces
// earlier (saves a wasted RPC and produces a clearer SMTP-side error).
func TestInboundBackendDataIngestBouncesMissingMLSPubkey(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	// No mlsPubkeys entry → fetch returns nil → bridge bounces 550.

	s := &inboundSession{
		logger:          slog.Default(),
		clientIP:        "127.0.0.1",
		from:            "alice@example.org",
		inboundRcpts:    []resolvedInboundRcpt{{actorID: actorBob}},
		caller:          mc,
		localDomains:    []string{"test.example"},
		maxMessageBytes: 50_000_000,
		spamPolicy:      permissiveSpamPolicyForTest(),
	}
	body := "From: alice@example.org\r\n" +
		"To: bob@test.example\r\n" +
		"Subject: bounce-missing-mls\r\n" +
		"\r\n" +
		"hello fauna\r\n"
	err := s.Data(bytes.NewReader([]byte(body)))
	if err == nil {
		t.Fatal("expected SMTP error for missing MLS pubkey, got nil")
	}
	var smtpErr *gosmtp.SMTPError
	if !errors.As(err, &smtpErr) {
		t.Fatalf("expected *gosmtp.SMTPError, got %T: %v", err, err)
	}
	if smtpErr.Code != 550 {
		t.Errorf("missing-MLS-pubkey code: got %d, want 550", smtpErr.Code)
	}
	if smtpErr.EnhancedCode != (gosmtp.EnhancedCode{5, 1, 1}) {
		t.Errorf("missing-MLS-pubkey enhanced code: got %v, want {5,1,1}", smtpErr.EnhancedCode)
	}
	if got := len(mc.ingestRequests); got != 0 {
		t.Errorf("ingest must not be called when MLS pubkey is missing; got %d calls", got)
	}
}

// TestInboundBackendDataTempfailsSuccessionPendingRecipient pins the
// 451 4.7.1 path (instead of the 550 permanent reject above) when the
// no-pubkey recipient is a succession's successor who has not yet
// re-provisioned a key — the bounded window between the ceremony and
// their first sign-in must tempfail so the sender's MTA retries and the
// message is eventually delivered (smtp-server.md § Error / tempfail
// strategy; succession-aftermath.md § Re-key scope).
func TestInboundBackendDataTempfailsSuccessionPendingRecipient(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	mc.mlsSuccessionPending[hex.EncodeToString(actorBob)] = true
	// No mlsPubkeys entry → fetch returns nil, succession_pending=true.

	s := &inboundSession{
		logger:          slog.Default(),
		clientIP:        "127.0.0.1",
		from:            "alice@example.org",
		inboundRcpts:    []resolvedInboundRcpt{{actorID: actorBob}},
		caller:          mc,
		localDomains:    []string{"test.example"},
		maxMessageBytes: 50_000_000,
		spamPolicy:      permissiveSpamPolicyForTest(),
	}
	body := "From: alice@example.org\r\n" +
		"To: bob@test.example\r\n" +
		"Subject: tempfail-succession-pending\r\n" +
		"\r\n" +
		"hello fauna\r\n"
	err := s.Data(bytes.NewReader([]byte(body)))
	if err == nil {
		t.Fatal("expected SMTP error for a succession-pending recipient, got nil")
	}
	var smtpErr *gosmtp.SMTPError
	if !errors.As(err, &smtpErr) {
		t.Fatalf("expected *gosmtp.SMTPError, got %T: %v", err, err)
	}
	if smtpErr.Code != 451 {
		t.Errorf("succession-pending code: got %d, want 451", smtpErr.Code)
	}
	if smtpErr.EnhancedCode != (gosmtp.EnhancedCode{4, 7, 1}) {
		t.Errorf("succession-pending enhanced code: got %v, want {4,7,1}", smtpErr.EnhancedCode)
	}
	if got := len(mc.ingestRequests); got != 0 {
		t.Errorf("ingest must not be called when MLS pubkey is missing; got %d calls", got)
	}
}

// TestInboundBackendRcptTempfailsSuccessionPendingRecipientBeforeData pins
// the fix: the inbound MX arm's key
// check now runs at RCPT TO, mirroring submission's partial-failure
// guarantee (smtp-server.md § Recipient handling on submission), instead of
// deferring to DATA where a later recipient's missing key used to fail the
// whole transaction after earlier recipients had already been ingested
// (every sender retry then re-delivered to them). Two RCPTs — bob
// (provisioned) and carol (succession-pending) — carol's RCPT tempfails
// 451 4.7.1 immediately and is dropped from the envelope; bob's RCPT
// succeeds and DATA ingests for bob exactly once.
func TestInboundBackendRcptTempfailsSuccessionPendingRecipientBeforeData(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	bobID := bytes.Repeat([]byte{0x42}, 32)
	carolID := bytes.Repeat([]byte{0x43}, 32)
	mc.decisions["bob@test.example"] = rcptDecision{resolved: true, actorID: bobID}
	mc.decisions["carol@test.example"] = rcptDecision{resolved: true, actorID: carolID}
	mc.mlsPubkeys[hex.EncodeToString(bobID)] = freshX25519Pubkey(t)
	mc.mlsSuccessionPending[hex.EncodeToString(carolID)] = true
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
	if err := c.Hello("loopback.example"); err != nil {
		t.Fatalf("HELO: %v", err)
	}
	if err := c.Mail("alice@example.org"); err != nil {
		t.Fatalf("MAIL: %v", err)
	}
	if err := c.Rcpt("bob@test.example"); err != nil {
		t.Fatalf("bob RCPT must be accepted: %v", err)
	}
	err = c.Rcpt("carol@test.example")
	if err == nil {
		t.Fatal("carol RCPT (succession pending, no key) must tempfail at RCPT time")
	}
	if code := smtpCodeOf(err); code != 451 {
		t.Errorf("carol RCPT succession-pending code: got %d, want 451 (err=%v)", code, err)
	}
	w, err := c.Data()
	if err != nil {
		t.Fatalf("DATA open: %v", err)
	}
	if _, err := io.WriteString(w, "From: alice@example.org\r\nSubject: row-446\r\n\r\nhello\r\n"); err != nil {
		t.Fatalf("write body: %v", err)
	}
	if err := w.Close(); err != nil {
		t.Fatalf("DATA close (bob-only delivery must succeed): %v", err)
	}
	if got := len(mc.ingestRequests); got != 1 {
		t.Fatalf("ingest call count: got %d, want exactly 1 (bob only)", got)
	}
	if !bytes.Equal(mc.ingestRequests[0].actorID, bobID) {
		t.Errorf("ingest actor: got %x, want bob %x", mc.ingestRequests[0].actorID, bobID)
	}
}

// TestInboundBackendDataPreflightCatchesKeyRevokedAfterRcptZeroPartialCommits
// pins the residual race the preflight backstop closes: a key revoked between RCPT and DATA (both recipients had a
// key at RCPT time; the reviewed design prefers this over a mid-loop
// per-recipient drop specifically to keep the transaction-wide tempfail with
// ZERO partial commits — smtp-server.md § Error / tempfail strategy row
// :289). Bob and carol both pass RCPT; carol's key is then revoked before
// DATA. preflightRecipientSealKeys must catch it BEFORE the ingest loop
// starts, so the whole DATA 451s and NEITHER recipient — not even bob, who
// still has a valid key — gets ingested.
func TestInboundBackendDataPreflightCatchesKeyRevokedAfterRcptZeroPartialCommits(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	bobID := bytes.Repeat([]byte{0x42}, 32)
	carolID := bytes.Repeat([]byte{0x43}, 32)
	mc.decisions["bob@test.example"] = rcptDecision{resolved: true, actorID: bobID}
	mc.decisions["carol@test.example"] = rcptDecision{resolved: true, actorID: carolID}
	mc.mlsPubkeys[hex.EncodeToString(bobID)] = freshX25519Pubkey(t)
	mc.mlsPubkeys[hex.EncodeToString(carolID)] = freshX25519Pubkey(t)
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
	if err := c.Hello("loopback.example"); err != nil {
		t.Fatalf("HELO: %v", err)
	}
	if err := c.Mail("alice@example.org"); err != nil {
		t.Fatalf("MAIL: %v", err)
	}
	if err := c.Rcpt("bob@test.example"); err != nil {
		t.Fatalf("bob RCPT (has a key) must be accepted: %v", err)
	}
	if err := c.Rcpt("carol@test.example"); err != nil {
		t.Fatalf("carol RCPT (has a key) must be accepted: %v", err)
	}
	// Simulate the race: carol succeeds another identity (becomes
	// succession-pending) between RCPT and DATA, so her key is now gone but
	// imminent — the 451 tempfail condition (smtp-server.md § Error /
	// tempfail strategy row :289), not the permanent 550 no-key case.
	mc.mu.Lock()
	delete(mc.mlsPubkeys, hex.EncodeToString(carolID))
	mc.mlsSuccessionPending[hex.EncodeToString(carolID)] = true
	mc.mu.Unlock()
	w, err := c.Data()
	if err != nil {
		t.Fatalf("DATA open: %v", err)
	}
	if _, err := io.WriteString(w, "From: alice@example.org\r\nSubject: row-446-preflight\r\n\r\nhello\r\n"); err != nil {
		t.Fatalf("write body: %v", err)
	}
	err = w.Close()
	if err == nil {
		t.Fatal("DATA must tempfail the whole transaction when a key was revoked after RCPT")
	}
	if code := smtpCodeOf(err); code != 451 {
		t.Errorf("preflight no-key code: got %d, want 451 (err=%v)", code, err)
	}
	if got := len(mc.ingestRequests); got != 0 {
		t.Fatalf("ingest call count: got %d, want 0 (zero partial commits — bob must NOT be ingested either)", got)
	}
}

// TestInboundBackendDataIngestsPerRecipient confirms multi-RCPT
// behaviour: each resolved actor gets one ingest call with its own
// encrypted_body (different recipients → different ciphertexts).
func TestInboundBackendDataIngestsPerRecipient(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	actorCarol := bytes.Repeat([]byte{0x43}, 32)
	mc.mlsPubkeys[hex.EncodeToString(actorBob)] = freshX25519Pubkey(t)
	mc.mlsPubkeys[hex.EncodeToString(actorCarol)] = freshX25519Pubkey(t)

	s := &inboundSession{
		logger:          slog.Default(),
		clientIP:        "127.0.0.1",
		from:            "alice@example.org",
		inboundRcpts:    []resolvedInboundRcpt{{actorID: actorBob}, {actorID: actorCarol}},
		caller:          mc,
		localDomains:    []string{"test.example"},
		maxMessageBytes: 50_000_000,
		spamPolicy:      permissiveSpamPolicyForTest(),
	}
	body := "From: alice@example.org\r\n" +
		"To: bob@test.example, carol@test.example\r\n" +
		"Subject: multi-rcpt\r\n" +
		"\r\n" +
		"shared body\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.ingestRequests); got != 2 {
		t.Fatalf("ingest call count: got %d, want 2", got)
	}
	if !bytes.Equal(mc.ingestRequests[0].actorID, actorBob) {
		t.Errorf("request[0].actor_id: got %x, want bob %x", mc.ingestRequests[0].actorID, actorBob)
	}
	if !bytes.Equal(mc.ingestRequests[1].actorID, actorCarol) {
		t.Errorf("request[1].actor_id: got %x, want carol %x", mc.ingestRequests[1].actorID, actorCarol)
	}
	// Different recipients → different ciphertexts (different KEM
	// encapsulation under different pubkeys).
	if bytes.Equal(mc.ingestRequests[0].encryptedBody, mc.ingestRequests[1].encryptedBody) {
		t.Error("per-recipient encrypted_body must differ between recipients")
	}
}

// TestInboundBackendDataIngestOverQuotaBounces552 pins the inbound
// quota-enforcement mapping: when nest's ingest handler returns the typed
// `over_quota` RpcError (recipient mailbox full), the MTA bounces the
// delivery with `552 5.2.2 Mailbox full` (imap-server.md § Quota
// enforcement points → inbound mail delivery).
func TestInboundBackendDataIngestOverQuotaBounces552(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	mc.mlsPubkeys[hex.EncodeToString(actorBob)] = freshX25519Pubkey(t)
	// Simulate nest rejecting the ingest as over-quota: an ok=false reply
	// whose payload is the typed `fauna.bridges.over_quota` RpcError.
	overQuotaPayload, err := cbor.Marshal(map[string]any{"code": wsrpc.CodeOverQuota})
	if err != nil {
		t.Fatalf("encode over_quota payload: %v", err)
	}
	mc.ingestErr = &wsrpc.ServerError{Payload: overQuotaPayload}

	s := &inboundSession{
		logger:          slog.Default(),
		clientIP:        "127.0.0.1",
		from:            "alice@example.org",
		inboundRcpts:    []resolvedInboundRcpt{{actorID: actorBob}},
		caller:          mc,
		localDomains:    []string{"test.example"},
		maxMessageBytes: 50_000_000,
		spamPolicy:      permissiveSpamPolicyForTest(),
	}
	body := "From: alice@example.org\r\n" +
		"To: bob@test.example\r\n" +
		"Subject: over-quota\r\n" +
		"\r\n" +
		"hello fauna\r\n"
	derr := s.Data(bytes.NewReader([]byte(body)))
	if derr == nil {
		t.Fatal("expected SMTP error for over-quota ingest, got nil")
	}
	var smtpErr *gosmtp.SMTPError
	if !errors.As(derr, &smtpErr) {
		t.Fatalf("expected *gosmtp.SMTPError, got %T: %v", derr, derr)
	}
	if smtpErr.Code != 552 {
		t.Errorf("over-quota code: got %d, want 552", smtpErr.Code)
	}
	if smtpErr.EnhancedCode != (gosmtp.EnhancedCode{5, 2, 2}) {
		t.Errorf("over-quota enhanced code: got %v, want {5,2,2}", smtpErr.EnhancedCode)
	}
}

// TestInboundBackendDataThreadsRoleAddressBitToIngest confirms the
// role-address bit learned at RCPT (validate_recipient) rides onto the
// ingest request, so nest can skip the per-mailbox quota pre-check for a
// role-address delivery (smtp-server.md :204).
func TestInboundBackendDataThreadsRoleAddressBitToIngest(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	admin := bytes.Repeat([]byte{0x07}, 32)
	mc.mlsPubkeys[hex.EncodeToString(admin)] = freshX25519Pubkey(t)

	s := &inboundSession{
		logger:          slog.Default(),
		clientIP:        "127.0.0.1",
		from:            "operator@remote.example",
		inboundRcpts:    []resolvedInboundRcpt{{actorID: admin, isRoleAddress: true}},
		caller:          mc,
		localDomains:    []string{"test.example"},
		maxMessageBytes: 50_000_000,
		spamPolicy:      permissiveSpamPolicyForTest(),
	}
	body := "From: operator@remote.example\r\n" +
		"To: postmaster@test.example\r\n" +
		"Subject: mail loop report\r\n" +
		"\r\n" +
		"your MX is misbehaving\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.ingestRequests); got != 1 {
		t.Fatalf("ingest call count: got %d, want 1", got)
	}
	if !mc.ingestRequests[0].isRoleAddress {
		t.Error("ingest request must carry is_role_address=true for a role-address delivery")
	}
}

// TestExtractSenderDomain moved to internal/mailfauna/mailfauna_test.go
// as part of I5 Phase D.5 — the helper was lifted to the mailfauna
// package so both the MTA inbound path and the MDA APPEND path share
// the same extraction (priority #1 / #2).

// ── T2.2 connection-time limits ────────────────────────────────────
//
// Three inbound (port-25) defenses per smtp-server.md § Connection-time
// limits (compile-time constants per mail-policy-config.md § Compile-time
// decisions): a global concurrency cap, an escalating error tarpit, and
// an explicit header-section cap.

// smtpErrCode extracts the SMTP code from a directly-returned
// *gosmtp.SMTPError (whose .Error() is "SMTP error NNN: …", which
// smtpCodeOf — built for wire-format "NNN …" strings — can't parse).
func smtpErrCode(err error) int {
	var se *gosmtp.SMTPError
	if errors.As(err, &se) {
		return se.Code
	}
	return -1
}

// crlf builds a CRLF-terminated header block of n field lines followed
// by the `\r\n\r\n` body separator and a one-line body — the canonical
// shape checkHeaderSection scans.
func headerBlock(nLines int) []byte {
	var b strings.Builder
	for i := 0; i < nLines; i++ {
		fmt.Fprintf(&b, "X-Field-%d: value\r\n", i)
	}
	b.WriteString("\r\n")
	b.WriteString("body text\r\n")
	return []byte(b.String())
}

// TestCheckHeaderSection asserts the parser-bomb defense: header blocks
// within the caps pass; over-count, over-byte, and no-separator-within-
// cap all reject with 554 5.6.0; a small message that simply lacks a
// separator defers to the RFC-5322 parser (nil — not the cap's job).
func TestCheckHeaderSection(t *testing.T) {
	t.Parallel()

	bigLine := append([]byte("Subject: "), bytes.Repeat([]byte("a"), (1<<20)+100)...)
	bigLine = append(bigLine, []byte("\r\n\r\nbody\r\n")...)

	cases := []struct {
		name    string
		raw     []byte
		wantErr bool
	}{
		{"normal", []byte("From: a@b.com\r\nTo: c@d.com\r\nSubject: hi\r\n\r\nbody\r\n"), false},
		{"exactly 256 header lines", headerBlock(256), false},
		{"257 header lines", headerBlock(257), true},
		{"over 1MiB header, no separator", bytes.Repeat([]byte("X: aaaaaaaaaaaa\r\n"), 80000), true},
		{"single header line over 1MiB", bigLine, true},
		{"short, no separator (defer to parser)", []byte("From: a@b.com\r\nTo: c@d.com"), false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := checkHeaderSection(tc.raw)
			if tc.wantErr {
				if got == nil {
					t.Fatalf("checkHeaderSection: want 554 5.6.0, got nil")
				}
				if got.Code != 554 || got.EnhancedCode != (gosmtp.EnhancedCode{5, 6, 0}) {
					t.Errorf("checkHeaderSection: got code %d %v, want 554 {5 6 0}", got.Code, got.EnhancedCode)
				}
			} else if got != nil {
				t.Errorf("checkHeaderSection: want nil, got %d %v %q", got.Code, got.EnhancedCode, got.Message)
			}
		})
	}
}

// TestTarpitDelay pins the escalation curve: 250ms × n, clamped at 5s.
func TestTarpitDelay(t *testing.T) {
	t.Parallel()
	cases := []struct {
		n    int
		want time.Duration
	}{
		{0, 0},
		{1, 250 * time.Millisecond},
		{4, 1 * time.Second},
		{20, 5 * time.Second},
		{21, 5 * time.Second},
		{100, 5 * time.Second},
	}
	for _, tc := range cases {
		if got := tarpitDelay(tc.n); got != tc.want {
			t.Errorf("tarpitDelay(%d) = %v, want %v", tc.n, got, tc.want)
		}
	}
}

// TestInboundSessionTarpitEscalates drives the session through policy
// rejections and asserts (a) each reject sleeps the escalating tarpit,
// (b) a successful command neither sleeps nor resets the counter, and
// (c) an infra-class 451 tempfail (nest unavailable) is NOT tarpitted —
// we never slow a legitimate sender's retry because our backend blipped.
func TestInboundSessionTarpitEscalates(t *testing.T) {
	t.Parallel()

	t.Run("escalation across policy rejects", func(t *testing.T) {
		var slept []time.Duration
		s := &inboundSession{
			logger:       slog.Default(),
			clientIP:     "1.2.3.4",
			localDomains: []string{"test.example"},
			sleep:        func(d time.Duration) { slept = append(slept, d) },
		}
		for i := 0; i < 3; i++ {
			to := fmt.Sprintf("user%d@foreign.example", i)
			if err := s.Rcpt(to, &gosmtp.RcptOptions{}); smtpErrCode(err) != 550 {
				t.Fatalf("Rcpt %q: want relay-denied 550, got %v", to, err)
			}
		}
		want := []time.Duration{250 * time.Millisecond, 500 * time.Millisecond, 750 * time.Millisecond}
		if len(slept) != 3 || slept[0] != want[0] || slept[1] != want[1] || slept[2] != want[2] {
			t.Errorf("tarpit sequence = %v, want %v", slept, want)
		}
	})

	t.Run("success does not sleep or reset", func(t *testing.T) {
		var slept []time.Duration
		s := &inboundSession{
			logger:       slog.Default(),
			clientIP:     "1.2.3.4",
			localDomains: []string{"test.example"},
			sleep:        func(d time.Duration) { slept = append(slept, d) },
		}
		if err := s.Rcpt("bad@foreign.example", &gosmtp.RcptOptions{}); smtpErrCode(err) != 550 {
			t.Fatalf("first reject: want 550, got %v", err)
		}
		// caller=nil + domain in localDomains ⇒ accepted, no tarpit.
		if err := s.Rcpt("good@test.example", &gosmtp.RcptOptions{}); err != nil {
			t.Fatalf("accepted RCPT errored: %v", err)
		}
		if err := s.Rcpt("bad2@foreign.example", &gosmtp.RcptOptions{}); smtpErrCode(err) != 550 {
			t.Fatalf("second reject: want 550, got %v", err)
		}
		want := []time.Duration{250 * time.Millisecond, 500 * time.Millisecond}
		if len(slept) != 2 || slept[0] != want[0] || slept[1] != want[1] {
			t.Errorf("tarpit sequence = %v, want %v (success must not sleep or reset)", slept, want)
		}
	})

	t.Run("infra 451 tempfail is not tarpitted", func(t *testing.T) {
		var slept []time.Duration
		mc := newMockCaller()
		mc.decisions["bob@test.example"] = rcptDecision{err: errors.New("nest transport boom")}
		s := &inboundSession{
			logger:       slog.Default(),
			clientIP:     "1.2.3.4",
			localDomains: []string{"test.example"},
			caller:       mc,
			sleep:        func(d time.Duration) { slept = append(slept, d) },
		}
		if err := s.Rcpt("bob@test.example", &gosmtp.RcptOptions{}); smtpErrCode(err) != 451 {
			t.Fatalf("want 451 tempfail, got %v", err)
		}
		if len(slept) != 0 {
			t.Errorf("infra 451 must not tarpit; slept %v", slept)
		}
	})
}

// TestSenderDomainRejectPaysTarpit confirms a sender-domain MX/A reject
// (550 5.7.1) and a malformed-sender reject (554 5.1.7) both route
// through s.reject and pay the escalating tarpit, while the null-sender
// bypass neither rejects nor sleeps. mailChecked is pre-set so the HELO
// block (which needs a live conn) is skipped — conn stays nil.
func TestSenderDomainRejectPaysTarpit(t *testing.T) {
	t.Parallel()
	res := newFakeResolver()
	res.nxMX["nodns.example"] = true
	res.nxHost["nodns.example"] = true
	var slept []time.Duration
	s := &inboundSession{
		logger:      slog.Default(),
		clientIP:    "203.0.113.5",
		mailChecked: true,
		policy:      &Policy{SenderDomain: NewSenderDomainChecker(res)},
		sleep:       func(d time.Duration) { slept = append(slept, d) },
	}
	if err := s.Mail("spammer@nodns.example", &gosmtp.MailOptions{}); smtpErrCode(err) != 550 {
		t.Fatalf("no-DNS sender domain: want 550, got %v", err)
	}
	if err := s.Mail("garbage-no-at", &gosmtp.MailOptions{}); smtpErrCode(err) != 554 {
		t.Fatalf("malformed sender: want 554, got %v", err)
	}
	// Null sender bypasses entirely — no reject, no sleep added.
	if err := s.Mail("", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("null sender <> must bypass: %v", err)
	}
	want := []time.Duration{250 * time.Millisecond, 500 * time.Millisecond}
	if len(slept) != 2 || slept[0] != want[0] || slept[1] != want[1] {
		t.Errorf("tarpit sequence = %v, want %v (null sender must not sleep)", slept, want)
	}
}
