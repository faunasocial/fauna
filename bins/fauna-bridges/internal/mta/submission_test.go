package mta

import (
	"bytes"
	"context"
	"crypto/ecdh"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"math/big"
	"net"
	"net/smtp"
	"os"
	"strings"
	"testing"
	"time"

	gosmtp "github.com/emersion/go-smtp"
	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// freshRecipientPubkey32 returns a real 32-byte X25519 pubkey by
// generating an ephemeral keypair via crypto/ecdh. HPKE-Seal rejects
// the all-zero buffer (small-order-point check) and the test fakes
// route every fetch_recipient_*_pubkey call through real
// EncryptToRecipient, so we need a valid pubkey rather than a stub.
// Mirrors mailfauna.freshRecipientPubkey but lives in the mta test
// package so the test file doesn't have to import mailfauna's
// internal test helpers.
func freshRecipientPubkey32(t *testing.T) []byte {
	t.Helper()
	sk, err := ecdh.X25519().GenerateKey(rand.Reader)
	if err != nil {
		t.Fatalf("ecdh GenerateKey: %v", err)
	}
	return sk.PublicKey().Bytes()
}

// Phase D.1 — submission listener skeleton tests.
//
// Three things we pin:
//   1. The implicit-TLS (465) listener accepts a TLS-handshaken
//      connection and serves a 220 banner.
//   2. The STARTTLS (587) listener accepts a plain connection,
//      advertises STARTTLS in its EHLO response, upgrades on STARTTLS,
//      and continues to serve.
//   3. Both listeners reject unauthenticated MAIL FROM with SMTP
//      `530 5.7.0`. D.2 wires actual AUTH; D.3 replaces the blanket
//      reject with domain validation.

// newSelfSignedTLSConfig builds an Ed25519-signed self-signed cert
// scoped to "test.example.com" with a 1-hour validity. Hermetic; no
// disk access or external CA. Suitable for both implicit-TLS and
// STARTTLS test harnesses.
func newSelfSignedTLSConfig(t *testing.T) *tls.Config {
	t.Helper()
	pub, priv, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatalf("ed25519 keygen: %v", err)
	}
	tmpl := &x509.Certificate{
		SerialNumber:          big.NewInt(1),
		Subject:               pkix.Name{CommonName: "test.example.com"},
		DNSNames:              []string{"test.example.com"},
		NotBefore:             time.Now().Add(-time.Minute),
		NotAfter:              time.Now().Add(time.Hour),
		KeyUsage:              x509.KeyUsageDigitalSignature,
		ExtKeyUsage:           []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
		BasicConstraintsValid: true,
	}
	der, err := x509.CreateCertificate(rand.Reader, tmpl, tmpl, pub, priv)
	if err != nil {
		t.Fatalf("create cert: %v", err)
	}
	cert := &tls.Certificate{
		Certificate: [][]byte{der},
		PrivateKey:  priv,
	}
	return &tls.Config{
		Certificates: []tls.Certificate{*cert},
		MinVersion:   tls.VersionTLS12,
		// Tests dial with InsecureSkipVerify; pin the server side to
		// the ServerName the client sends to keep things sane.
	}
}

// startSubmissionListenerForTest binds an ephemeral 127.0.0.1:0
// listener (TLS-wrapped if kind == implicitTLS, plain otherwise),
// starts the submission server in a goroutine, and returns the addr
// + a cancel func the test should call to tear down.
func startSubmissionListenerForTest(t *testing.T, kind submissionListenerKind, tlsCfg *tls.Config) (addr string, cancel func()) {
	t.Helper()
	rawL, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	var l net.Listener = rawL
	if kind == submissionImplicitTLS {
		l = tls.NewListener(rawL, tlsCfg)
	}
	backend := &submissionBackend{
		logger: slog.Default(),
		cfg: staticMTAConfig(&mtaLiveConfig{
			localDomains:  []string{"test.example.com"},
			primaryDomain: "test.example.com",
		}),
	}
	ctx, cancelCtx := context.WithCancel(context.Background())
	serveErr := make(chan error, 1)
	go func() {
		serveErr <- runSubmissionListener(ctx, l, backend, kind, tlsCfg, "test.example.com", 0, 0, nil, slog.Default())
	}()
	return l.Addr().String(), func() {
		cancelCtx()
		select {
		case err := <-serveErr:
			if err != nil {
				t.Errorf("runSubmissionListener: %v", err)
			}
		case <-time.After(3 * time.Second):
			t.Error("runSubmissionListener did not return within 3s of ctx cancel")
		}
	}
}

// TestSubmission465ImplicitTLSAcceptsConnection — port-465 path.
// Dial the listener over TLS; expect a 220 banner from go-smtp.
func TestSubmission465ImplicitTLSAcceptsConnection(t *testing.T) {
	t.Parallel()
	tlsCfg := newSelfSignedTLSConfig(t)
	addr, cancel := startSubmissionListenerForTest(t, submissionImplicitTLS, tlsCfg)
	defer cancel()

	dialer := &net.Dialer{Timeout: 2 * time.Second}
	conn, err := tls.DialWithDialer(dialer, "tcp", addr, &tls.Config{
		ServerName:         "test.example.com",
		InsecureSkipVerify: true, //nolint:gosec // self-signed test fixture
	})
	if err != nil {
		t.Fatalf("dial implicit-TLS: %v", err)
	}
	defer conn.Close()
	if err := conn.SetReadDeadline(time.Now().Add(2 * time.Second)); err != nil {
		t.Fatalf("set deadline: %v", err)
	}
	buf := make([]byte, 256)
	n, err := conn.Read(buf)
	if err != nil && !errors.Is(err, io.EOF) {
		t.Fatalf("read banner: %v", err)
	}
	banner := string(buf[:n])
	if !strings.HasPrefix(banner, "220") {
		t.Fatalf("expected 220 banner, got %q", banner)
	}
	if !strings.Contains(banner, "test.example.com") {
		t.Errorf("banner missing domain; got %q", banner)
	}
}

// TestSubmission587STARTTLSAcceptsConnection — port-587 path.
// Dial plain; EHLO; expect 250-STARTTLS in the multi-line response;
// upgrade with STARTTLS; expect a fresh 220 banner.
func TestSubmission587STARTTLSAcceptsConnection(t *testing.T) {
	t.Parallel()
	tlsCfg := newSelfSignedTLSConfig(t)
	addr, cancel := startSubmissionListenerForTest(t, submissionStartTLS, tlsCfg)
	defer cancel()

	c, err := smtp.Dial(addr)
	if err != nil {
		t.Fatalf("smtp.Dial: %v", err)
	}
	defer c.Close()
	if err := c.Hello("client.test.example.com"); err != nil {
		t.Fatalf("EHLO: %v", err)
	}
	ok, _ := c.Extension("STARTTLS")
	if !ok {
		t.Fatal("server did not advertise STARTTLS")
	}
	if err := c.StartTLS(&tls.Config{
		ServerName:         "test.example.com",
		InsecureSkipVerify: true, //nolint:gosec // self-signed test fixture
	}); err != nil {
		t.Fatalf("STARTTLS: %v", err)
	}
}

// TestSubmission465RejectsUnauthenticatedMailFrom — Phase D.1 gating
// pin. Until D.2 wires AUTH, MAIL FROM must reject with `530 5.7.0`.
// Done on the 465 path (implicit TLS) so the test exercises both the
// TLS wrap AND the gating.
func TestSubmission465RejectsUnauthenticatedMailFrom(t *testing.T) {
	t.Parallel()
	tlsCfg := newSelfSignedTLSConfig(t)
	addr, cancel := startSubmissionListenerForTest(t, submissionImplicitTLS, tlsCfg)
	defer cancel()

	conn, err := tls.Dial("tcp", addr, &tls.Config{
		ServerName:         "test.example.com",
		InsecureSkipVerify: true, //nolint:gosec // self-signed test fixture
	})
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer conn.Close()

	c, err := smtp.NewClient(conn, "test.example.com")
	if err != nil {
		t.Fatalf("smtp.NewClient: %v", err)
	}
	defer c.Close()
	if err := c.Hello("client.test.example.com"); err != nil {
		t.Fatalf("EHLO: %v", err)
	}
	mailErr := c.Mail("alice@test.example.com")
	if mailErr == nil {
		t.Fatal("expected MAIL FROM to fail without AUTH; got nil")
	}
	if !strings.Contains(mailErr.Error(), "530") {
		t.Errorf("expected 530 code in error; got %q", mailErr.Error())
	}
	if !strings.Contains(strings.ToLower(mailErr.Error()), "authentication required") {
		t.Errorf("expected 'authentication required' in error; got %q", mailErr.Error())
	}
}

// ── Phase D.3: MAIL FROM + RCPT TO + quota tests ─────────────────
//
// D.2 already exercises the AUTH path end-to-end (see auth_test.go);
// D.3 layers MAIL FROM identity validation and RCPT TO quota
// enforcement on top of the post-AUTH session. Tests hand-craft the
// post-AUTH session state so we don't re-prove the AEAD-unwrap path
// in every case — the unit under test is the MAIL/RCPT gate logic.

// newAuthedSubmissionSession constructs a session in the "AUTH-PLAIN
// just succeeded" shape: authedLocalPart="alice", domain="example.com",
// MaxRecipients=maxRecipients. Used by every D.3 test below.
// sessionActorID is the canonical 32-byte actor id every D.3 fixture
// authenticates as (bytes 1..32). Tests that drive resolve_recipient
// map an alias local-part to this value to mark it "owned by the
// authenticated actor".
func sessionActorID() []byte {
	actorID := make([]byte, 32)
	for i := range actorID {
		actorID[i] = byte(i + 1)
	}
	return actorID
}

func newAuthedSubmissionSession(t *testing.T, caller *submissionAuthCaller, maxRecipients uint32) *submissionSession {
	t.Helper()
	now := time.Unix(1_700_000_000, 0)
	backend := &submissionBackend{
		logger: slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelError})),
		client: caller,
		nowFn:  func() time.Time { return now },
		cfg: staticMTAConfig(&mtaLiveConfig{
			localDomains:  []string{"example.com"},
			primaryDomain: "example.com",
		}),
	}
	actorID := sessionActorID()
	return &submissionSession{
		backend:         backend,
		sourceIP:        "10.0.0.1",
		authenticated:   true,
		actorID:         actorID,
		credentialID:    "default",
		authedLocalPart: "alice",
		submissionToken: &mailfauna.SubmissionTokenFfi{
			ActorId:           actorID,
			CredentialId:      "default",
			IssuedAt:          0,
			ExpiresAt:         0,
			MaxRecipients:     maxRecipients,
			MaxMessagesPerDay: 1000,
		},
	}
}

// asSMTPError unwraps a gosmtp.SMTPError; t.Fatal on shape mismatch.
func asSMTPError(t *testing.T, err error) *gosmtp.SMTPError {
	t.Helper()
	if err == nil {
		t.Fatal("expected an SMTP error; got nil")
	}
	var se *gosmtp.SMTPError
	if !errors.As(err, &se) {
		t.Fatalf("expected *gosmtp.SMTPError; got %T: %v", err, err)
	}
	return se
}

func TestSubmissionMailFromMatchesAuthenticated(t *testing.T) {
	t.Parallel()
	sess := newAuthedSubmissionSession(t, &submissionAuthCaller{}, 100)
	if err := sess.Mail("alice@example.com", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("MAIL FROM with authed identity must succeed; got %v", err)
	}
}

func TestSubmissionMailFromCaseFoldedDomainMatches(t *testing.T) {
	t.Parallel()
	// Domains are case-insensitive per RFC 5321 §2.4 — `EXAMPLE.com`
	// must accept against `example.com`.
	sess := newAuthedSubmissionSession(t, &submissionAuthCaller{}, 100)
	if err := sess.Mail("alice@EXAMPLE.com", &gosmtp.MailOptions{}); err != nil {
		t.Errorf("MAIL FROM with case-folded domain must succeed; got %v", err)
	}
}

// TestSubmissionMailFromUnownedLocalPart — a MAIL FROM local-part that
// is neither the login handle nor an alias the actor owns (the stub
// resolver rejects it) is refused with 550 5.7.1 per
// mail-multidomain.md § Cross-domain submission policy.
func TestSubmissionMailFromUnownedLocalPart(t *testing.T) {
	t.Parallel()
	sess := newAuthedSubmissionSession(t, &submissionAuthCaller{}, 100)
	err := sess.Mail("bob@example.com", &gosmtp.MailOptions{})
	se := asSMTPError(t, err)
	if se.Code != 550 {
		t.Errorf("expected 550; got %d (%v)", se.Code, se)
	}
	want := gosmtp.EnhancedCode{5, 7, 1}
	if se.EnhancedCode != want {
		t.Errorf("expected enhanced code 5.7.1; got %v", se.EnhancedCode)
	}
}

// TestSubmissionMailFromOwnedAlias — a MAIL FROM local-part that differs
// from the login handle but resolves to the authenticated actor (an
// alias the user owns, e.g. one picked in the macOS Mail "From"
// selector) is accepted. mail-multidomain.md § Cross-domain submission
// policy line 334.
func TestSubmissionMailFromOwnedAlias(t *testing.T) {
	t.Parallel()
	caller := &submissionAuthCaller{
		validateRecipientByLocal: map[string][]byte{"sales": sessionActorID()},
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	if err := sess.Mail("sales@example.com", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("MAIL FROM with an owned alias must succeed; got %v", err)
	}
	if got := sess.envelopeFrom; got != "sales@example.com" {
		t.Errorf("envelopeFrom = %q, want the validated alias sales@example.com", got)
	}
}

// TestSubmissionMailFromOwnedAliasOnSecondDomain — an owned alias on a
// non-primary local domain is accepted and preserved as the envelope
// sender (the default permissive cross-domain policy).
func TestSubmissionMailFromOwnedAliasOnSecondDomain(t *testing.T) {
	t.Parallel()
	caller := &submissionAuthCaller{
		validateRecipientByLocal: map[string][]byte{"alice": sessionActorID()},
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	sess.backend.cfg = staticMTAConfig(&mtaLiveConfig{
		localDomains:  []string{"example.com", "second.example"},
		primaryDomain: "example.com",
	})
	// local-part == authedLocal("alice") but on the non-primary domain:
	// still the fast-path (handle owned on any local domain), and the
	// envelope sender must reflect the domain the user chose, not primary.
	if err := sess.Mail("alice@second.example", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("MAIL FROM alice@second.example must succeed; got %v", err)
	}
	if got := sess.envelopeFrom; got != "alice@second.example" {
		t.Errorf("envelopeFrom = %q, want alice@second.example", got)
	}
}

// TestSubmissionMailFromAliasOwnedByAnotherActor — a local-part that
// resolves to a DIFFERENT actor is refused 550 5.7.1 (you may only send
// as addresses you own).
func TestSubmissionMailFromAliasOwnedByAnotherActor(t *testing.T) {
	t.Parallel()
	other := make([]byte, 32)
	for i := range other {
		other[i] = 0xAA
	}
	caller := &submissionAuthCaller{
		validateRecipientByLocal: map[string][]byte{"carol": other},
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	err := sess.Mail("carol@example.com", &gosmtp.MailOptions{})
	se := asSMTPError(t, err)
	if se.Code != 550 || se.EnhancedCode != (gosmtp.EnhancedCode{5, 7, 1}) {
		t.Errorf("expected 550 5.7.1; got %d %v", se.Code, se.EnhancedCode)
	}
}

// TestSubmissionMailFromForwarderRejected — an admin external forwarder
// address (resolve → Forward, not a sending mailbox) is not an owned
// sending identity; refuse 550 5.7.1.
func TestSubmissionMailFromForwarderRejected(t *testing.T) {
	t.Parallel()
	caller := &submissionAuthCaller{
		resolveForwarders: map[string]submissionForwarderDecision{
			"info": {target: "ext@elsewhere.example", admin: sessionActorID()},
		},
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	err := sess.Mail("info@example.com", &gosmtp.MailOptions{})
	se := asSMTPError(t, err)
	if se.Code != 550 || se.EnhancedCode != (gosmtp.EnhancedCode{5, 7, 1}) {
		t.Errorf("expected 550 5.7.1 for a forwarder MAIL FROM; got %d %v", se.Code, se.EnhancedCode)
	}
}

// TestSubmissionMailFromAliasResolveTransportError — a resolver transport
// failure while checking alias ownership tempfails 451 4.7.1 (fail
// closed; never accept an unverified sender).
func TestSubmissionMailFromAliasResolveTransportError(t *testing.T) {
	t.Parallel()
	caller := &submissionAuthCaller{validateRecipientErr: fmt.Errorf("nest unreachable")}
	sess := newAuthedSubmissionSession(t, caller, 100)
	err := sess.Mail("bob@example.com", &gosmtp.MailOptions{})
	se := asSMTPError(t, err)
	if se.Code != 451 || se.EnhancedCode != (gosmtp.EnhancedCode{4, 7, 1}) {
		t.Errorf("expected 451 4.7.1 on resolve transport error; got %d %v", se.Code, se.EnhancedCode)
	}
}

// TestSubmissionMailFromAliasNoClientFailsClosed — with no nest client
// wired, an alias MAIL FROM cannot be verified and must fail closed
// (451), never accept.
func TestSubmissionMailFromAliasNoClientFailsClosed(t *testing.T) {
	t.Parallel()
	sess := newAuthedSubmissionSession(t, &submissionAuthCaller{}, 100)
	sess.backend.client = nil
	err := sess.Mail("bob@example.com", &gosmtp.MailOptions{})
	se := asSMTPError(t, err)
	if se.Code != 451 {
		t.Errorf("expected 451 (fail closed) with no client; got %d %v", se.Code, se)
	}
}

func TestSubmissionMailFromWrongDomain(t *testing.T) {
	t.Parallel()
	sess := newAuthedSubmissionSession(t, &submissionAuthCaller{}, 100)
	err := sess.Mail("alice@evil.com", &gosmtp.MailOptions{})
	se := asSMTPError(t, err)
	if se.Code != 553 {
		t.Errorf("expected 553; got %d (%v)", se.Code, se)
	}
	want := gosmtp.EnhancedCode{5, 7, 1}
	if se.EnhancedCode != want {
		t.Errorf("expected enhanced code 5.7.1; got %v", se.EnhancedCode)
	}
}

func TestSubmissionMailFromNullSender(t *testing.T) {
	t.Parallel()
	// MAIL FROM:<> is reserved for server-generated DSNs/bounces (RFC
	// 5321 §3.3); a MUA submission listener rejects it — no
	// authenticated identity can match the empty string.
	sess := newAuthedSubmissionSession(t, &submissionAuthCaller{}, 100)
	err := sess.Mail("", &gosmtp.MailOptions{})
	se := asSMTPError(t, err)
	if se.Code != 553 {
		t.Errorf("expected 553 for null sender; got %d (%v)", se.Code, se)
	}
}

func TestSubmissionMailFromSyntaxError(t *testing.T) {
	t.Parallel()
	// A clearly malformed address (no @) gets the syntax-error code
	// 501 5.5.2 rather than the identity-mismatch 553 — the wire
	// distinguishes "couldn't parse" from "doesn't match you".
	sess := newAuthedSubmissionSession(t, &submissionAuthCaller{}, 100)
	err := sess.Mail("not-an-address", &gosmtp.MailOptions{})
	se := asSMTPError(t, err)
	if se.Code != 501 {
		t.Errorf("expected 501 for syntactically invalid MAIL FROM; got %d (%v)", se.Code, se)
	}
}

func TestSubmissionMailFromRejectsBeforeAuth(t *testing.T) {
	t.Parallel()
	// Pre-AUTH: the existing 530 path stays — D.3 only adds gates
	// that fire after authentication.
	sess := newAuthedSubmissionSession(t, &submissionAuthCaller{}, 100)
	sess.authenticated = false
	sess.authedLocalPart = ""
	err := sess.Mail("alice@example.com", &gosmtp.MailOptions{})
	se := asSMTPError(t, err)
	if se.Code != 530 {
		t.Errorf("expected 530 pre-AUTH; got %d (%v)", se.Code, se)
	}
}

func TestSubmissionRcptWithinQuota(t *testing.T) {
	t.Parallel()
	caller := &submissionAuthCaller{}
	sess := newAuthedSubmissionSession(t, caller, 100)
	if err := sess.Mail("alice@example.com", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("MAIL FROM: %v", err)
	}
	for _, rcpt := range []string{"bob@external.com", "carol@another.org"} {
		if err := sess.Rcpt(rcpt, &gosmtp.RcptOptions{}); err != nil {
			t.Errorf("RCPT %q within quota must succeed; got %v", rcpt, err)
		}
	}
	if got := caller.callsOf("fauna.bridges.check_submission_quota"); len(got) != 2 {
		t.Errorf("check_submission_quota fired %d times, want 2", len(got))
	}
	sess.mu.Lock()
	defer sess.mu.Unlock()
	if sess.recipientCount != 2 {
		t.Errorf("recipientCount = %d, want 2", sess.recipientCount)
	}
}

// TestSubmissionRcptTellsNestWhichRecipientsStayLocal — every accepted RCPT
// makes one quota call carrying the running recipient_count (nest's
// per-message cap input) and whether THIS recipient stays on the
// deployment: a resolved mailbox is local; an admin external forwarder
// (the message leaves through the forward dispatch) and an outside address
// are not (smtp-server.md § Architectural rules, the charging rule).
func TestSubmissionRcptTellsNestWhichRecipientsStayLocal(t *testing.T) {
	t.Parallel()
	bobActor := make([]byte, 32)
	for i := range bobActor {
		bobActor[i] = 0x42
	}
	caller := &submissionAuthCaller{
		validateRecipientByLocal: map[string][]byte{"bob": bobActor},
		mlsPubkey:                freshRecipientPubkey32(t),
		indexPubkey:              freshRecipientPubkey32(t),
		resolveForwarders: map[string]submissionForwarderDecision{
			"info": {target: "ext@elsewhere.example", admin: sessionActorID()},
		},
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	if err := sess.Mail("alice@example.com", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("MAIL FROM: %v", err)
	}
	for _, rcpt := range []string{"bob@example.com", "info@example.com", "dave@external.test"} {
		if err := sess.Rcpt(rcpt, &gosmtp.RcptOptions{}); err != nil {
			t.Fatalf("RCPT %q must succeed; got %v", rcpt, err)
		}
	}
	calls := caller.callsOf("fauna.bridges.check_submission_quota")
	if len(calls) != 3 {
		t.Fatalf("check_submission_quota fired %d times, want one per accepted RCPT (3)", len(calls))
	}
	want := []struct {
		count uint32
		local bool
	}{{1, true}, {2, false}, {3, false}}
	for i, c := range calls {
		var body struct {
			RecipientCount   uint32 `cbor:"recipient_count"`
			RecipientIsLocal bool   `cbor:"recipient_is_local"`
		}
		if err := cbor.Unmarshal(c.body, &body); err != nil {
			t.Fatalf("decode quota call %d: %v", i+1, err)
		}
		if body.RecipientCount != want[i].count || body.RecipientIsLocal != want[i].local {
			t.Errorf("quota call %d: recipient_count=%d recipient_is_local=%v; want %d / %v",
				i+1, body.RecipientCount, body.RecipientIsLocal, want[i].count, want[i].local)
		}
	}
}

func TestSubmissionRcptOverPerMessageLimit(t *testing.T) {
	t.Parallel()
	// MaxRecipients=1 from the unwrapped submission token; the second
	// RCPT exceeds the per-message cap and must reject 452 4.7.12
	// without firing check_submission_quota (the fast-path short-
	// circuits the RPC).
	caller := &submissionAuthCaller{}
	sess := newAuthedSubmissionSession(t, caller, 1)
	if err := sess.Mail("alice@example.com", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("MAIL FROM: %v", err)
	}
	if err := sess.Rcpt("bob@external.com", &gosmtp.RcptOptions{}); err != nil {
		t.Fatalf("first RCPT must succeed; got %v", err)
	}
	err := sess.Rcpt("carol@external.com", &gosmtp.RcptOptions{})
	se := asSMTPError(t, err)
	if se.Code != 452 {
		t.Errorf("expected 452 over-per-message-limit; got %d (%v)", se.Code, se)
	}
	want := gosmtp.EnhancedCode{4, 7, 12}
	if se.EnhancedCode != want {
		t.Errorf("expected enhanced code 4.7.12; got %v", se.EnhancedCode)
	}
	// Exactly one quota RPC (the first RCPT's authoritative check);
	// the second RCPT's fast-path rejected before reaching nest.
	if got := caller.callsOf("fauna.bridges.check_submission_quota"); len(got) != 1 {
		t.Errorf("check_submission_quota fired %d times, want 1", len(got))
	}
}

func TestSubmissionRcptOverNestQuota(t *testing.T) {
	t.Parallel()
	// MaxRecipients high enough not to trigger the fast-path; nest
	// returns over_quota on the 2nd check, so the 2nd RCPT rejects
	// 452 4.7.12.
	caller := &submissionAuthCaller{
		quotaOutcomes: []checkSubmissionQuotaOutcome{
			{over: false},
			{over: true, remaining: 0},
		},
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	if err := sess.Mail("alice@example.com", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("MAIL FROM: %v", err)
	}
	if err := sess.Rcpt("bob@external.com", &gosmtp.RcptOptions{}); err != nil {
		t.Fatalf("first RCPT must succeed; got %v", err)
	}
	err := sess.Rcpt("carol@external.com", &gosmtp.RcptOptions{})
	se := asSMTPError(t, err)
	if se.Code != 452 {
		t.Errorf("expected 452 over-nest-quota; got %d (%v)", se.Code, se)
	}
	want := gosmtp.EnhancedCode{4, 7, 12}
	if se.EnhancedCode != want {
		t.Errorf("expected enhanced code 4.7.12; got %v", se.EnhancedCode)
	}
}

func TestSubmissionRcptQuotaCheckTransportError(t *testing.T) {
	t.Parallel()
	// Nest is unreachable mid-RCPT — tempfail 451 4.7.0 (don't 250
	// past a quota gate we can't evaluate).
	caller := &submissionAuthCaller{
		checkQuotaErr: errors.New("nest unreachable"),
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	if err := sess.Mail("alice@example.com", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("MAIL FROM: %v", err)
	}
	err := sess.Rcpt("bob@external.com", &gosmtp.RcptOptions{})
	se := asSMTPError(t, err)
	if se.Code != 451 {
		t.Errorf("expected 451 transient on transport error; got %d (%v)", se.Code, se)
	}
}

func TestSubmissionRcptResetsAcrossMailFrom(t *testing.T) {
	t.Parallel()
	// Per RFC 5321 §4.1.1.2, a new MAIL FROM starts a fresh
	// transaction; the recipient counter resets. Without the reset, a
	// long-lived MUA session would hit the per-message cap on its
	// second message even if it's well within the cap on its own.
	caller := &submissionAuthCaller{}
	sess := newAuthedSubmissionSession(t, caller, 2)
	if err := sess.Mail("alice@example.com", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("first MAIL FROM: %v", err)
	}
	for _, rcpt := range []string{"bob@external.com", "carol@external.com"} {
		if err := sess.Rcpt(rcpt, &gosmtp.RcptOptions{}); err != nil {
			t.Fatalf("first txn RCPT %q: %v", rcpt, err)
		}
	}
	// Reset (RSET) before next MAIL FROM (gosmtp normally fires Reset
	// implicitly between transactions when the prior one didn't reach
	// DATA; we exercise it explicitly here).
	sess.Reset()
	if err := sess.Mail("alice@example.com", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("second MAIL FROM: %v", err)
	}
	for _, rcpt := range []string{"dave@external.com", "eve@external.com"} {
		if err := sess.Rcpt(rcpt, &gosmtp.RcptOptions{}); err != nil {
			t.Errorf("second txn RCPT %q must succeed after reset; got %v", rcpt, err)
		}
	}
}

func TestSubmissionRcptRejectsBeforeAuth(t *testing.T) {
	t.Parallel()
	sess := newAuthedSubmissionSession(t, &submissionAuthCaller{}, 100)
	sess.authenticated = false
	err := sess.Rcpt("bob@external.com", &gosmtp.RcptOptions{})
	se := asSMTPError(t, err)
	if se.Code != 530 {
		t.Errorf("expected 530 pre-AUTH; got %d (%v)", se.Code, se)
	}
}

// TestSubmissionRcptPartialLocalFailureDeliversRest pins the behavior where
// one invalid local recipient is rejected at
// RCPT TO (550), and the message still delivers to every accepted
// recipient (the valid local + the external). Drives the realistic
// Mail → Rcpt×N → Data flow so the RCPT-time resolution cache is what
// Data consumes. See smtp-server.md § Recipient handling on submission.
func TestSubmissionRcptPartialLocalFailureDeliversRest(t *testing.T) {
	t.Parallel()
	bobActor := make([]byte, 32)
	for i := range bobActor {
		bobActor[i] = 0x42
	}
	caller := &submissionAuthCaller{
		// bob resolves; "ghost" is absent from the map → reject.
		validateRecipientByLocal: map[string][]byte{"bob": bobActor},
		mlsPubkey:                freshRecipientPubkey32(t),
		indexPubkey:              freshRecipientPubkey32(t),
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	if err := sess.Mail("alice@example.com", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("MAIL FROM: %v", err)
	}
	if err := sess.Rcpt("bob@example.com", &gosmtp.RcptOptions{}); err != nil {
		t.Fatalf("valid local RCPT must succeed; got %v", err)
	}
	ghostErr := sess.Rcpt("ghost@example.com", &gosmtp.RcptOptions{})
	se := asSMTPError(t, ghostErr)
	if se.Code != 550 || se.EnhancedCode != (gosmtp.EnhancedCode{5, 1, 1}) {
		t.Errorf("unknown local RCPT must reject 550 5.1.1 at RCPT TO; got %d %v", se.Code, se.EnhancedCode)
	}
	if err := sess.Rcpt("dave@external.test", &gosmtp.RcptOptions{}); err != nil {
		t.Fatalf("external RCPT must succeed; got %v", err)
	}

	if err := sess.Data(strings.NewReader(
		"From: alice@example.com\r\nTo: bob@example.com\r\nCc: dave@external.test\r\nMessage-ID: <m@example.com>\r\nSubject: partial\r\n\r\nhi\r\n")); err != nil {
		t.Fatalf("DATA must return 250 and deliver to the accepted recipients; got %v", err)
	}
	if len(caller.ingestInboundCalls) != 1 {
		t.Errorf("expected 1 ingest (valid local bob); got %d", len(caller.ingestInboundCalls))
	}
	if len(caller.submitInboundCalls) != 1 {
		t.Errorf("expected 1 submit (Sent copy); got %d", len(caller.submitInboundCalls))
	}
	if len(caller.enqueuedOutbound) != 1 {
		t.Fatalf("expected 1 enqueue (external dave); got %d", len(caller.enqueuedOutbound))
	}
	if got := caller.enqueuedOutbound[0].Recipients; len(got) != 1 || got[0] != "dave@external.test" {
		t.Errorf("enqueue recipients=%v want [dave@external.test]", got)
	}
}

// ── Phase D.4: Data() — From: header gates + outbound-pending shape ───

func TestSubmissionDataRejectsBeforeAuth(t *testing.T) {
	t.Parallel()
	// Pre-AUTH DATA: the gate fires before any body read happens. Mirrors Mail/Rcpt's 530 5.7.0 shape.
	sess := newAuthedSubmissionSession(t, &submissionAuthCaller{}, 100)
	sess.authenticated = false
	err := sess.Data(strings.NewReader("From: a@example.com\r\n\r\nhi\r\n"))
	se := asSMTPError(t, err)
	if se.Code != 530 {
		t.Errorf("expected 530 pre-AUTH; got %d (%v)", se.Code, se)
	}
}

// TestSubmissionDataRefusesOtherThanOneFromField is the submission door's side
// of the From-field rule (smtp-server.md § Architectural rules): checkFromLocal
// makes the local-domain check — and the nest picks the DKIM key — by the LAST
// From field, while a receiver's DMARC may align against the first — so a
// message carrying other than one From field is refused 554 5.6.0 before
// anything is filed or enqueued.
func TestSubmissionDataRefusesOtherThanOneFromField(t *testing.T) {
	t.Parallel()
	for name, body := range map[string]string{
		"two From fields": "From: ceo@bank.test\r\nFrom: alice@example.com\r\nTo: bob@external.test\r\nMessage-ID: <two@example.com>\r\n\r\nbody\r\n",
		"no From field":   "To: bob@external.test\r\nMessage-ID: <none@example.com>\r\n\r\nbody\r\n",
	} {
		t.Run(name, func(t *testing.T) {
			caller := &submissionAuthCaller{
				mlsPubkey:   freshRecipientPubkey32(t),
				indexPubkey: freshRecipientPubkey32(t),
			}
			sess := newAuthedSubmissionSession(t, caller, 100)
			sess.recipients = []string{"bob@external.test"}
			sess.recipientCount = 1
			se := asSMTPError(t, sess.Data(strings.NewReader(body)))
			if se.Code != 554 || se.EnhancedCode != (gosmtp.EnhancedCode{5, 6, 0}) {
				t.Errorf("got %d %v %q, want 554 {5 6 0}", se.Code, se.EnhancedCode, se.Message)
			}
			if n := len(caller.enqueuedOutbound) + len(caller.submitInboundCalls) + len(caller.ingestInboundCalls); n != 0 {
				t.Errorf("a refused submission must not enqueue or file anything; got %d calls", n)
			}
		})
	}
}

// ── From: header ownership (mail-multidomain.md § From: header ownership) ──
//
// Outbound mail is DKIM-signed on the From: header's domain, so a From:
// naming another local user leaves DMARC-aligned for that user. The envelope
// check alone never saw it: the header must name an
// address the authenticated actor owns — its handle on any active local
// domain (no RPC) or an owned alias (resolve_recipient) — through the SAME
// predicate MAIL FROM uses. None of these fixtures set signsOutbound, which
// pins that the check is independent of signing (the 550 5.7.7 off-domain
// refusal fires only on a deployment that signs; this one always does).

// resolveRecipientCalls counts the fake nest's resolve_recipient calls — zero
// pins the no-RPC handle fast path.
func resolveRecipientCalls(caller *submissionAuthCaller) int {
	caller.mu.Lock()
	defer caller.mu.Unlock()
	n := 0
	for _, c := range caller.calls {
		if c.method == wsrpc.MethodResolveRecipient {
			n++
		}
	}
	return n
}

// fromHeaderSession is an authenticated alice session that has already passed
// MAIL FROM as her own address and one external RCPT, so Data()'s only open
// question is the From: header.
func fromHeaderSession(t *testing.T, caller *submissionAuthCaller, localDomains ...string) *submissionSession {
	t.Helper()
	sess := newAuthedSubmissionSession(t, caller, 100)
	if len(localDomains) > 0 {
		sess.backend.cfg = staticMTAConfig(&mtaLiveConfig{
			localDomains:  localDomains,
			primaryDomain: localDomains[0],
		})
	}
	if err := sess.Mail("alice@example.com", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("MAIL FROM alice@example.com: %v", err)
	}
	if err := sess.Rcpt("dave@external.test", &gosmtp.RcptOptions{}); err != nil {
		t.Fatalf("RCPT dave@external.test: %v", err)
	}
	return sess
}

func fromHeaderMessage(from string) string {
	return "From: " + from + "\r\nTo: dave@external.test\r\nMessage-ID: <from-check@example.com>\r\nSubject: hi\r\n\r\nbody\r\n"
}

func assertNothingFiled(t *testing.T, caller *submissionAuthCaller) {
	t.Helper()
	if n := len(caller.enqueuedOutbound) + len(caller.submitInboundCalls) + len(caller.ingestInboundCalls); n != 0 {
		t.Errorf("a refused submission must not enqueue or file anything; got %d calls", n)
	}
}

// TestSubmissionDataRefusesFromHeaderOwnedByAnotherActor — the impersonation
// itself: alice's envelope, a From: naming ceo's address on the primary
// domain and on a second local domain. 550 5.7.1, the envelope check's code
// family; nothing signed, filed or enqueued.
func TestSubmissionDataRefusesFromHeaderOwnedByAnotherActor(t *testing.T) {
	t.Parallel()
	other := bytes.Repeat([]byte{0xAA}, 32)
	for name, tc := range map[string]struct {
		from    string
		domains []string
	}{
		"primary domain": {from: "ceo@example.com"},
		"second local domain": {
			from:    "\"The CEO\" <ceo@second.example>",
			domains: []string{"example.com", "second.example"},
		},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			caller := &submissionAuthCaller{
				validateRecipientByLocal: map[string][]byte{"ceo": other},
				mlsPubkey:                freshRecipientPubkey32(t),
				indexPubkey:              freshRecipientPubkey32(t),
			}
			sess := fromHeaderSession(t, caller, tc.domains...)
			se := asSMTPError(t, sess.Data(strings.NewReader(fromHeaderMessage(tc.from))))
			if se.Code != 550 || se.EnhancedCode != (gosmtp.EnhancedCode{5, 7, 1}) {
				t.Errorf("got %d %v %q, want 550 {5 7 1}", se.Code, se.EnhancedCode, se.Message)
			}
			assertNothingFiled(t, caller)
		})
	}
}

// TestSubmissionDataRefusesFromHeaderNobodyCanOwn — a local part the nest
// does not resolve at all, and an address on a subdomain of a local domain
// (the signer would key it under the parent, so it would leave aligned — yet
// no mailbox on that domain exists to be owned). Both 550 5.7.1.
func TestSubmissionDataRefusesFromHeaderNobodyCanOwn(t *testing.T) {
	t.Parallel()
	for name, from := range map[string]string{
		"unresolvable local part":      "ghost@example.com",
		"subdomain of a local domain":  "alice@sub.example.com",
		"own handle, subdomain, cased": "Alice@Sub.Example.COM",
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			caller := &submissionAuthCaller{
				mlsPubkey:   freshRecipientPubkey32(t),
				indexPubkey: freshRecipientPubkey32(t),
			}
			sess := fromHeaderSession(t, caller)
			se := asSMTPError(t, sess.Data(strings.NewReader(fromHeaderMessage(from))))
			if se.Code != 550 || se.EnhancedCode != (gosmtp.EnhancedCode{5, 7, 1}) {
				t.Errorf("got %d %v %q, want 550 {5 7 1}", se.Code, se.EnhancedCode, se.Message)
			}
			assertNothingFiled(t, caller)
		})
	}
}

// TestSubmissionDataAcceptsFromHeaderNamingAnOwnedAlias — the header may
// differ from the envelope (bounce routing stays the user's choice) as long as
// it is theirs: an alias resolve_recipient attributes to the authenticated
// actor is accepted and relayed.
func TestSubmissionDataAcceptsFromHeaderNamingAnOwnedAlias(t *testing.T) {
	t.Parallel()
	caller := &submissionAuthCaller{
		validateRecipientByLocal: map[string][]byte{"sales": sessionActorID()},
		mlsPubkey:                freshRecipientPubkey32(t),
		indexPubkey:              freshRecipientPubkey32(t),
	}
	sess := fromHeaderSession(t, caller)
	if err := sess.Data(strings.NewReader(fromHeaderMessage("\"Sales\" <sales@example.com>"))); err != nil {
		t.Fatalf("a From: naming the actor's own alias must be accepted; got %v", err)
	}
	if len(caller.enqueuedOutbound) != 1 {
		t.Errorf("want the message relayed once; got %d enqueues", len(caller.enqueuedOutbound))
	}
	if n := resolveRecipientCalls(caller); n != 1 {
		t.Errorf("an alias From: is verified through resolve_recipient exactly once; got %d calls", n)
	}
}

// TestSubmissionDataAcceptsFromHeaderNamingOwnHandleWithoutAnRPC — the
// login handle is owned on every active local domain and compared
// case-insensitively, so `Alice@Second.Example` passes with no resolver call
// (the same fast path MAIL FROM takes).
func TestSubmissionDataAcceptsFromHeaderNamingOwnHandleWithoutAnRPC(t *testing.T) {
	t.Parallel()
	caller := &submissionAuthCaller{
		mlsPubkey:   freshRecipientPubkey32(t),
		indexPubkey: freshRecipientPubkey32(t),
	}
	sess := fromHeaderSession(t, caller, "example.com", "second.example")
	if err := sess.Data(strings.NewReader(fromHeaderMessage("\"Alice\" <Alice@Second.Example>"))); err != nil {
		t.Fatalf("a From: naming the actor's own handle on a second local domain must be accepted; got %v", err)
	}
	if len(caller.enqueuedOutbound) != 1 {
		t.Errorf("want the message relayed once; got %d enqueues", len(caller.enqueuedOutbound))
	}
	if n := resolveRecipientCalls(caller); n != 0 {
		t.Errorf("the handle fast path makes no resolver call; got %d", n)
	}
}

// TestSubmissionDataFromHeaderCheckFailsClosed — the resolver unreachable
// while the header names something other than the handle: 451 4.7.1, never
// an unchecked accept (the MAIL FROM check's shape).
func TestSubmissionDataFromHeaderCheckFailsClosed(t *testing.T) {
	t.Parallel()
	caller := &submissionAuthCaller{
		mlsPubkey:   freshRecipientPubkey32(t),
		indexPubkey: freshRecipientPubkey32(t),
	}
	sess := fromHeaderSession(t, caller)
	caller.validateRecipientErr = fmt.Errorf("nest unreachable")
	se := asSMTPError(t, sess.Data(strings.NewReader(fromHeaderMessage("sales@example.com"))))
	if se.Code != 451 || se.EnhancedCode != (gosmtp.EnhancedCode{4, 7, 1}) {
		t.Errorf("got %d %v %q, want 451 {4 7 1}", se.Code, se.EnhancedCode, se.Message)
	}
	assertNothingFiled(t, caller)
}

// TestSubmissionDataRefusesFromFieldWithOtherThanOneMailbox — the one From
// field must name exactly one mailbox (RFC 5322 §3.6.2 allows a list; RFC
// 7489 §6.6.1 names rejection for it). Two mailboxes would sign under the
// first's domain with a foreign one riding along; none leaves nothing to
// own. 554 5.6.0, the field-count refusal's code, before any ownership
// question is asked.
func TestSubmissionDataRefusesFromFieldWithOtherThanOneMailbox(t *testing.T) {
	t.Parallel()
	for name, from := range map[string]string{
		"own address plus a foreign one": "alice@example.com, ceo@bank.example",
		"a group of two":                 "Team: alice@example.com, bob@example.com;",
		"display name only":              "\"Just A Name\"",
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			caller := &submissionAuthCaller{
				mlsPubkey:   freshRecipientPubkey32(t),
				indexPubkey: freshRecipientPubkey32(t),
			}
			sess := fromHeaderSession(t, caller)
			se := asSMTPError(t, sess.Data(strings.NewReader(fromHeaderMessage(from))))
			if se.Code != 554 || se.EnhancedCode != (gosmtp.EnhancedCode{5, 6, 0}) {
				t.Errorf("got %d %v %q, want 554 {5 6 0}", se.Code, se.EnhancedCode, se.Message)
			}
			assertNothingFiled(t, caller)
			if n := resolveRecipientCalls(caller); n != 0 {
				t.Errorf("a malformed From field is refused before any ownership lookup; got %d resolver calls", n)
			}
		})
	}
}

func TestSubmissionDataEnqueuesOnSuccess(t *testing.T) {
	t.Parallel()
	// D.5 + D.6: external-only RCPTs ride enqueue_outbound_mail, AND
	// the sender's "Sent" copy fires via submit_inbound_mail regardless
	// of who the RCPTs are. With at least one external RCPT and the
	// sender's actor configured with an MLS pubkey (so the Sent copy
	// succeeds), Data returns nil (250 OK) and the caller records the
	// enqueue + the Sent-copy submit.
	caller := &submissionAuthCaller{
		mlsPubkey:   freshRecipientPubkey32(t),
		indexPubkey: freshRecipientPubkey32(t),
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	// Two external RCPTs (domains don't match s.backend.domain) — D.6
	// routes them to the external path.
	sess.recipients = []string{"bob@external.test", "carol@elsewhere.test"}
	sess.recipientCount = 2

	err := sess.Data(strings.NewReader(
		"From: alice@example.com\r\nTo: bob@external.test\r\nMessage-ID: <abc@example.com>\r\nSubject: hi\r\n\r\nbody\r\n"))
	if err != nil {
		t.Fatalf("expected nil (250 OK); got %v", err)
	}
	if len(caller.enqueuedOutbound) != 1 {
		t.Fatalf("expected 1 enqueue_outbound_mail call; got %d", len(caller.enqueuedOutbound))
	}
	got := caller.enqueuedOutbound[0]
	if got.OriginalSender != "alice@example.com" {
		t.Errorf("original_sender=%q want alice@example.com", got.OriginalSender)
	}
	if got.OriginalMsgID != "abc@example.com" {
		t.Errorf("original_msgid=%q want abc@example.com", got.OriginalMsgID)
	}
	wantRcpts := []string{"bob@external.test", "carol@elsewhere.test"}
	if len(got.Recipients) != len(wantRcpts) {
		t.Fatalf("recipients=%v want %v", got.Recipients, wantRcpts)
	}
	for i, r := range got.Recipients {
		if r != wantRcpts[i] {
			t.Errorf("recipients[%d]=%q want %q", i, r, wantRcpts[i])
		}
	}
	if !bytes.Contains(got.RawMessage, []byte("body")) {
		t.Errorf("raw_message missing body: %q", got.RawMessage)
	}
	// D.6: a Sent copy for the sender's own actor always fires via
	// submit_inbound_mail. No Fauna-recipient ingest call here (no
	// local-domain RCPTs in this test).
	if len(caller.submitInboundCalls) != 1 {
		t.Errorf("expected 1 submit_inbound_mail (Sent copy); got %d", len(caller.submitInboundCalls))
	}
	if len(caller.ingestInboundCalls) != 0 {
		t.Errorf("expected 0 ingest_inbound_mail; got %d", len(caller.ingestInboundCalls))
	}
}

func TestSubmissionDataGeneratesMessageIDWhenAbsent(t *testing.T) {
	t.Parallel()
	// RFC 6409 §8.3: the submission server adds a Message-ID when the MUA
	// omitted one. Load-bearing, not cosmetic: nest's enqueue_outbound_mail
	// REJECTS an empty original_msgid, so before this a Message-ID-less
	// submission died at DATA with a misleading transient 451 ("Outbound
	// enqueue temporarily unavailable") that no retry could clear —
	// root-caused live against example.com + the :latest image, 2026-07-09.
	// The header is stamped BEFORE the enqueue so the nest's DKIM signature
	// covers it and the Sent copy carries the same id.
	caller := &submissionAuthCaller{
		mlsPubkey:   freshRecipientPubkey32(t),
		indexPubkey: freshRecipientPubkey32(t),
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	sess.recipients = []string{"bob@external.test"}
	sess.recipientCount = 1

	err := sess.Data(strings.NewReader(
		"From: alice@example.com\r\nTo: bob@external.test\r\nSubject: no msgid\r\n\r\nbody\r\n"))
	if err != nil {
		t.Fatalf("expected nil (250 OK); got %v", err)
	}
	if len(caller.enqueuedOutbound) != 1 {
		t.Fatalf("expected 1 enqueue_outbound_mail call; got %d", len(caller.enqueuedOutbound))
	}
	got := caller.enqueuedOutbound[0]
	if got.OriginalMsgID == "" {
		t.Errorf("original_msgid must be non-empty for a Message-ID-less submission " +
			"(nest rejects empty — the 2026-07-09 451 regression)")
	}
	if !bytes.Contains(got.RawMessage, []byte("Message-ID: <")) {
		t.Errorf("stamped Message-ID header missing from the outbound bytes: %q", got.RawMessage)
	}
	// The Sent copy fires too (it seals the same stamped bytes; the fixture
	// records only sealed sizes, so the header itself is asserted on the
	// enqueued RawMessage above).
	if len(caller.submitInboundCalls) != 1 {
		t.Fatalf("expected 1 submit_inbound_mail (Sent copy); got %d", len(caller.submitInboundCalls))
	}
}

func TestSubmissionDataStripsReceivedHeaders(t *testing.T) {
	t.Parallel()
	// smtp-server.md § Outbound delivery: internal `Received:` headers the
	// user's MUA / upstream relays attached MUST be stripped before the
	// message leaves the bridge, so the submitter's IP and our internal
	// hostnames don't reach the recipient. The strip happens before the
	// enqueue, so the nest's signature covers the stripped form and the
	// enqueued RawMessage is exactly the stripped bytes. Substring matches (`X-Received-By`, `Received-SPF`)
	// must survive — only whole `Received:` headers are removed.
	caller := &submissionAuthCaller{
		mlsPubkey:   freshRecipientPubkey32(t),
		indexPubkey: freshRecipientPubkey32(t),
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	sess.recipients = []string{"bob@external.test"}
	sess.recipientCount = 1

	// Two leaky Received headers (the second with an RFC 5322 §2.2.3
	// continuation line) plus two look-alike headers that must NOT be
	// stripped.
	raw := "Received: from sketchy.internal (10.0.0.5)\r\n" +
		"Received: from mua.local\r\n\tby relay.internal with ESMTP\r\n" +
		"X-Received-By: keepme\r\n" +
		"Received-SPF: pass\r\n" +
		"From: alice@example.com\r\n" +
		"To: bob@external.test\r\n" +
		"Message-ID: <strip@example.com>\r\n" +
		"Subject: hi\r\n\r\nbody\r\n"

	if err := sess.Data(strings.NewReader(raw)); err != nil {
		t.Fatalf("expected nil (250 OK); got %v", err)
	}
	if len(caller.enqueuedOutbound) != 1 {
		t.Fatalf("expected 1 enqueue_outbound_mail call; got %d", len(caller.enqueuedOutbound))
	}
	got := caller.enqueuedOutbound[0].RawMessage

	for _, leak := range []string{"sketchy.internal", "10.0.0.5", "mua.local", "relay.internal"} {
		if bytes.Contains(got, []byte(leak)) {
			t.Errorf("enqueued message still leaks %q (internal Received: not stripped):\n%s", leak, got)
		}
	}
	for _, keep := range []string{"X-Received-By: keepme", "Received-SPF: pass", "body"} {
		if !bytes.Contains(got, []byte(keep)) {
			t.Errorf("enqueued message dropped %q (over-stripped):\n%s", keep, got)
		}
	}
}

// TestSubmissionDataStripsForgedFaunaStamps pins the submission door of
// smtp-server.md § Architectural rules → The X-Fauna-* namespace: a submitter's
// MUA cannot file a reserved delivery stamp into the copies this DATA seals
// or relays. The strip runs beside the Received: strip, before the enqueue,
// so the enqueued RawMessage is exactly the stripped bytes. The forward-loop trace and a substring look-alike must survive.
func TestSubmissionDataStripsForgedFaunaStamps(t *testing.T) {
	t.Parallel()
	caller := &submissionAuthCaller{
		mlsPubkey:   freshRecipientPubkey32(t),
		indexPubkey: freshRecipientPubkey32(t),
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	sess.recipients = []string{"bob@external.test"}
	sess.recipientCount = 1

	raw := "X-Fauna-Spam-Threshold: 0\r\n" +
		"x-fauna-address-suffix: forged\r\n\tcontinued\r\n" +
		"X-Fauna-Forwarded-By: actor=peer; t=1; rule=forward-all\r\n" +
		"X-Not-Fauna: keepme\r\n" +
		"From: alice@example.com\r\n" +
		"To: bob@external.test\r\n" +
		"Message-ID: <stamps@example.com>\r\n" +
		"Subject: hi\r\n\r\nbody\r\n"

	if err := sess.Data(strings.NewReader(raw)); err != nil {
		t.Fatalf("expected nil (250 OK); got %v", err)
	}
	if len(caller.enqueuedOutbound) != 1 {
		t.Fatalf("expected 1 enqueue_outbound_mail call; got %d", len(caller.enqueuedOutbound))
	}
	got := caller.enqueuedOutbound[0].RawMessage

	for _, forged := range []string{"X-Fauna-Spam-Threshold", "x-fauna-address-suffix", "forged", "continued"} {
		if bytes.Contains(got, []byte(forged)) {
			t.Errorf("enqueued message still carries %q (forged X-Fauna-* stamp not stripped):\n%s", forged, got)
		}
	}
	for _, keep := range []string{"X-Fauna-Forwarded-By: actor=peer", "X-Not-Fauna: keepme", "body"} {
		if !bytes.Contains(got, []byte(keep)) {
			t.Errorf("enqueued message dropped %q (over-stripped):\n%s", keep, got)
		}
	}
}

func TestSubmissionDataReturns451WhenEnqueueFails(t *testing.T) {
	t.Parallel()
	// Nest transport error on enqueue_outbound_mail → 451 4.5.0 (the
	// MUA retries). The body never reaches outbound_mail_queue.
	caller := &submissionAuthCaller{
		mlsPubkey:              freshRecipientPubkey32(t),
		indexPubkey:            freshRecipientPubkey32(t),
		enqueueOutboundMailErr: fmt.Errorf("synthetic transport error"),
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	sess.recipients = []string{"bob@external.test"}
	sess.recipientCount = 1
	err := sess.Data(strings.NewReader(
		"From: alice@example.com\r\nTo: bob@external.test\r\n\r\nbody\r\n"))
	se := asSMTPError(t, err)
	if se.Code != 451 {
		t.Errorf("expected 451 (enqueue failed); got %d (%v)", se.Code, se)
	}
	want := gosmtp.EnhancedCode{4, 5, 0}
	if se.EnhancedCode != want {
		t.Errorf("expected enhanced 4.5.0; got %v", se.EnhancedCode)
	}
}

func TestSubmissionDataRejectsZeroRecipients(t *testing.T) {
	t.Parallel()
	// gosmtp normally fences this at the protocol layer; the inline
	// guard in Data defends against fixtures that drive the method
	// directly.
	sess := newAuthedSubmissionSession(t, &submissionAuthCaller{}, 100)
	// recipientCount stays zero, recipients stays nil.
	err := sess.Data(strings.NewReader("From: alice@example.com\r\n\r\nbody\r\n"))
	se := asSMTPError(t, err)
	if se.Code != 554 {
		t.Errorf("expected 554; got %d (%v)", se.Code, se)
	}
}

func TestSubmissionDataRejectsOversizeBody(t *testing.T) {
	t.Parallel()
	// § B6: an authenticated submitter must not be able to force a multi-GiB
	// transient — the DATA read is bounded by the snapshot `max_message_bytes`
	// (the same cap the inbound MX path enforces, server.go inboundSession.Data).
	// A body past the cap returns 552 BEFORE any strip/sign/parse/enqueue.
	caller := &submissionAuthCaller{
		mlsPubkey:   freshRecipientPubkey32(t),
		indexPubkey: freshRecipientPubkey32(t),
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	// Tighten the snapshot cap to a tiny value for the test (production carries
	// the default 50 MB; 0 = unbounded, the pre-guard behavior).
	sess.backend.cfg = staticMTAConfig(&mtaLiveConfig{
		localDomains:    []string{"example.com"},
		primaryDomain:   "example.com",
		maxMessageBytes: 64,
	})
	sess.recipients = []string{"bob@external.test"}
	sess.recipientCount = 1

	body := "From: alice@example.com\r\nTo: bob@external.test\r\n\r\n" + strings.Repeat("A", 200) + "\r\n"
	se := asSMTPError(t, sess.Data(strings.NewReader(body)))
	if se.Code != 552 {
		t.Errorf("oversize submission code = %d, want 552", se.Code)
	}
	if len(caller.enqueuedOutbound) != 0 || len(caller.submitInboundCalls) != 0 {
		t.Errorf("oversize body must be rejected before any send: enqueued=%d submitted=%d",
			len(caller.enqueuedOutbound), len(caller.submitInboundCalls))
	}
}

func TestSubmissionDataExternalOverInlineBudgetStages(t *testing.T) {
	t.Parallel()
	// Staged-envelope rule (smtp-server.md § Message size limits, S9.3): the
	// perimeter admits raw messages far larger than the 2 MiB WS-RPC frame, and
	// enqueue_outbound_mail ships the body inline — so an over-inline-budget
	// external body is now SEALED under a one-shot AEAD key and STAGED on the
	// bulk-byte plane, with enqueue_outbound_mail carrying a staged_body reference
	// (empty raw_message) instead of failing 552. (Inverts the former 552 guard;
	// the PRODUCT-ceiling 552 still fires at the DATA-read clamp — see
	// TestSubmissionDataRejectsOversizeBody.)
	plane, cleanup := newTestBytePlane(t)
	defer cleanup()
	caller := &submissionAuthCaller{
		mlsPubkey:   freshRecipientPubkey32(t),
		indexPubkey: freshRecipientPubkey32(t),
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	sess.backend.bytePlane = plane
	sess.backend.cfg = staticMTAConfig(&mtaLiveConfig{
		localDomains:    []string{"example.com"},
		primaryDomain:   "example.com",
		maxMessageBytes: 8 << 20, // perimeter admits 8 MiB — well past the frame
	})
	sess.recipients = []string{"bob@external.test"}
	sess.recipientCount = 1

	// A body over the inline budget, but built from SPACE-SEPARATED repeated
	// words so the tokenized index hint dedups to a handful of tokens (a single
	// giant unbroken "word" would itself overflow the inline hint budget and trip
	// the Sent-copy leg, which is a distinct limit, not what this test exercises).
	filler := strings.Repeat("the quick brown fox jumps over the lazy dog ",
		int(mailfauna.InlineMailRequestBudgetBytes())/40+1)
	body := "From: alice@example.com\r\nTo: bob@external.test\r\n" +
		"Message-ID: <big@example.com>\r\nSubject: big\r\n\r\n" + filler + "\r\n"
	if err := sess.Data(strings.NewReader(body)); err != nil {
		t.Fatalf("over-inline-budget external submission must stage + enqueue (250 OK); got %v", err)
	}
	if len(caller.enqueuedOutbound) != 1 {
		t.Fatalf("expected 1 enqueue_outbound_mail call; got %d", len(caller.enqueuedOutbound))
	}
	got := caller.enqueuedOutbound[0]
	if got.StagedBody == nil {
		t.Fatal("over-inline-budget enqueue must carry a staged_body reference")
	}
	if len(got.RawMessage) != 0 {
		t.Errorf("staged enqueue must send an EMPTY raw_message; got %d bytes", len(got.RawMessage))
	}
	if len(got.StagedBody.ChunkHashes) == 0 || got.StagedBody.TotalBytes == 0 || len(got.StagedBody.Key) == 0 {
		t.Fatalf("staged_body reference is incomplete: %+v", got.StagedBody)
	}
	// End-to-end: the staged reference resolves back (fetch chunks → join →
	// AEAD-open) to the exact body the submitter's message produced.
	resolved, err := resolveStagedOutboundBody(context.Background(), plane, got.StagedBody)
	if err != nil {
		t.Fatalf("staged reference did not resolve: %v", err)
	}
	if !bytes.Contains(resolved, []byte("Subject: big")) ||
		!bytes.Contains(resolved, []byte("the quick brown fox jumps over the lazy dog")) {
		t.Errorf("resolved staged body lost the original content (len=%d)", len(resolved))
	}
}

func TestSubmissionDataAcceptsBodyWithinCap(t *testing.T) {
	t.Parallel()
	// A non-zero cap must NOT break a normal submission within the limit — the
	// guard only fires past the cap (companion to the oversize test above).
	caller := &submissionAuthCaller{
		mlsPubkey:   freshRecipientPubkey32(t),
		indexPubkey: freshRecipientPubkey32(t),
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	sess.backend.cfg = staticMTAConfig(&mtaLiveConfig{
		localDomains:    []string{"example.com"},
		primaryDomain:   "example.com",
		maxMessageBytes: 1 << 20, // 1 MiB — generous for the tiny body below
	})
	sess.recipients = []string{"bob@external.test"}
	sess.recipientCount = 1

	body := "From: alice@example.com\r\nTo: bob@external.test\r\nMessage-ID: <abc@example.com>\r\nSubject: hi\r\n\r\nbody\r\n"
	if err := sess.Data(strings.NewReader(body)); err != nil {
		t.Fatalf("within-cap submission must succeed (250 OK); got %v", err)
	}
	if len(caller.enqueuedOutbound) != 1 {
		t.Errorf("within-cap body must enqueue; got %d", len(caller.enqueuedOutbound))
	}
}

// TestSubmissionFromLocalitySkippedWhenDeploymentSignsNothing: a deployment
// whose config snapshot projects no DKIM selectors signs for no domain
// (localhost / pre-claim), so there is no signing domain to hold the From:
// header to — an off-domain From passes the locality gate.
func TestSubmissionFromLocalitySkippedWhenDeploymentSignsNothing(t *testing.T) {
	t.Parallel()
	sess := newAuthedSubmissionSession(t, &submissionAuthCaller{}, 100)
	raw := []byte("From: mallory@notlocal.org\r\nSubject: hi\r\n\r\nbody\r\n")
	if err := sess.checkFromLocal(raw); err != nil {
		t.Fatalf("checkFromLocal must pass when the deployment signs nothing, got %v", err)
	}
}

// TestSubmissionFromLocalityAcceptsLocalAndSubdomain: on a deployment that
// signs, a From: on an active local domain — or a subdomain of one — passes,
// and so does a message with no parseable From (locality can't be decided).
func TestSubmissionFromLocalityAcceptsLocalAndSubdomain(t *testing.T) {
	t.Parallel()
	sess := newAuthedSubmissionSession(t, &submissionAuthCaller{}, 100)
	sess.backend.cfg = staticMTAConfig(&mtaLiveConfig{
		localDomains:  []string{"example.com"},
		primaryDomain: "example.com",
		signsOutbound: true,
	})
	for name, raw := range map[string]string{
		"local":     "From: alice@example.com\r\nSubject: hi\r\n\r\nbody\r\n",
		"subdomain": "From: alice@news.example.com\r\nSubject: hi\r\n\r\nbody\r\n",
		"no From":   "Subject: hi\r\n\r\nbody\r\n",
	} {
		if err := sess.checkFromLocal([]byte(raw)); err != nil {
			t.Errorf("%s: checkFromLocal must pass, got %v", name, err)
		}
	}
}

// TestSubmissionDataRejectsNonLocalFrom pins the From-domain locality gate
// (mail-multidomain.md § Signing-key selection): on a deployment that signs
// (the snapshot projects DKIM selectors), a message whose From: header domain
// is not a local domain (and not a subdomain) is refused `550 5.7.7 From:
// domain not local`, and nothing is filed or enqueued. The gate reads only the
// config snapshot — the bridge holds no DKIM key.
func TestSubmissionDataRejectsNonLocalFrom(t *testing.T) {
	t.Parallel()
	caller := &submissionAuthCaller{
		mlsPubkey:   freshRecipientPubkey32(t),
		indexPubkey: freshRecipientPubkey32(t),
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	sess.backend.cfg = staticMTAConfig(&mtaLiveConfig{
		localDomains:  []string{"example.com"},
		primaryDomain: "example.com",
		signsOutbound: true,
	})
	sess.recipients = []string{"bob@external.test"}
	sess.recipientCount = 1

	raw := []byte("From: mallory@notlocal.org\r\nSubject: hi\r\n\r\nbody\r\n")
	notLocal := sess.checkFromLocal(raw)
	if notLocal == nil {
		t.Fatalf("checkFromLocal must return errFromNotLocal for a non-local From")
	}
	if notLocal.domain != "notlocal.org" {
		t.Errorf("errFromNotLocal.domain = %q, want %q", notLocal.domain, "notlocal.org")
	}

	body := "From: mallory@notlocal.org\r\nTo: bob@external.test\r\nMessage-ID: <x@notlocal.org>\r\nSubject: hi\r\n\r\nbody\r\n"
	se := asSMTPError(t, sess.Data(strings.NewReader(body)))
	if se.Code != 550 || se.EnhancedCode != (gosmtp.EnhancedCode{5, 7, 7}) {
		t.Errorf("got %d %v %q, want 550 {5 7 7}", se.Code, se.EnhancedCode, se.Message)
	}
	if n := len(caller.enqueuedOutbound) + len(caller.submitInboundCalls) + len(caller.ingestInboundCalls); n != 0 {
		t.Errorf("a refused submission must not enqueue or file anything; got %d calls", n)
	}
}

// ── Phase D.6: Fauna-recipient routing tests ────────────────────

// TestSubmissionDataDeliversFaunaRecipient — one local-domain RCPT
// (not the sender). D.6 partitions it onto the Fauna path:
// validate_recipient resolves to bob's actor; ingest_inbound_mail
// (not submit, because actorID != sess.actorID) carries the encrypted
// body to bob's INBOX. The sender's Sent copy fires separately via
// submit_inbound_mail.
func TestSubmissionDataDeliversFaunaRecipient(t *testing.T) {
	t.Parallel()
	bobActor := make([]byte, 32)
	for i := range bobActor {
		bobActor[i] = 0x42
	}
	caller := &submissionAuthCaller{
		validateRecipientByLocal: map[string][]byte{"bob": bobActor},
		mlsPubkey:                freshRecipientPubkey32(t),
		indexPubkey:              freshRecipientPubkey32(t),
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	if err := sess.Mail("alice@example.com", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("MAIL FROM: %v", err)
	}
	if err := sess.Rcpt("bob@example.com", &gosmtp.RcptOptions{}); err != nil {
		t.Fatalf("RCPT bob: %v", err)
	}

	err := sess.Data(strings.NewReader(
		"From: alice@example.com\r\nTo: bob@example.com\r\nMessage-ID: <abc@example.com>\r\nSubject: hi\r\nDate: Mon, 1 Jan 2024 00:00:00 +0000\r\n\r\nbody\r\n"))
	if err != nil {
		t.Fatalf("expected nil (250 OK); got %v", err)
	}
	// resolve_recipient: 1 call (for bob, at RCPT TO — the cutover from the
	// exact-only validate_recipient). Seal keys resolve at DELIVERY through
	// the one Phase-3 D2 resolver (wsrpc.ResolveRecipientSealKeys), once per
	// delivered copy: bob's INBOX copy + alice's Sent copy = 2 MLS-pubkey +
	// 2 index-key fetches at DATA (RCPT-time resolution remains validation
	// only; bob's pubkey also fetched there — 3 MLS fetches total).
	if got := len(caller.callsOf("fauna.bridges.resolve_recipient")); got != 1 {
		t.Errorf("resolve_recipient fired %d times, want 1", got)
	}
	if got := len(caller.callsOf("fauna.bridges.validate_recipient")); got != 0 {
		t.Errorf("submission RCPT must no longer call validate_recipient; fired %d times", got)
	}
	if got := len(caller.callsOf("fauna.bridges.fetch_recipient_mls_pubkey")); got != 3 {
		t.Errorf("fetch_recipient_mls_pubkey fired %d times, want 3", got)
	}
	if got := len(caller.callsOf("fauna.bridges.fetch_recipient_index_key")); got != 2 {
		t.Errorf("fetch_recipient_index_key fired %d times, want 2", got)
	}
	if len(caller.ingestInboundCalls) != 1 {
		t.Fatalf("expected 1 ingest_inbound_mail (Fauna RCPT); got %d", len(caller.ingestInboundCalls))
	}
	if !bytes.Equal(caller.ingestInboundCalls[0].ActorID, bobActor) {
		t.Errorf("ingest actor=%x want %x", caller.ingestInboundCalls[0].ActorID, bobActor)
	}
	if caller.ingestInboundCalls[0].Verdicts["dkim"] != "pass" {
		t.Errorf("Fauna delivery should stamp DKIM=pass; got %q", caller.ingestInboundCalls[0].Verdicts["dkim"])
	}
	if caller.ingestInboundCalls[0].SpamDisposition != "accept" {
		t.Errorf("own-submission disposition=%q want accept", caller.ingestInboundCalls[0].SpamDisposition)
	}
	if caller.ingestInboundCalls[0].SenderDomain != "example.com" {
		t.Errorf("sender_domain=%q want example.com", caller.ingestInboundCalls[0].SenderDomain)
	}
	// Sender's Sent copy: 1 submit_inbound_mail targeting sess.actorID.
	if len(caller.submitInboundCalls) != 1 {
		t.Fatalf("expected 1 submit_inbound_mail (Sent copy); got %d", len(caller.submitInboundCalls))
	}
	if !bytes.Equal(caller.submitInboundCalls[0].ActorID, sess.actorID) {
		t.Errorf("submit actor=%x want sender actorID %x",
			caller.submitInboundCalls[0].ActorID, sess.actorID)
	}
	// No external RCPTs → no enqueue.
	if len(caller.enqueuedOutbound) != 0 {
		t.Errorf("expected 0 enqueue_outbound_mail; got %d", len(caller.enqueuedOutbound))
	}
}

// TestSubmissionDataRoutesMixedRecipients — 1 local + 1 external.
// D.6 splits: local recipient via ingest_inbound_mail, external via
// enqueue_outbound_mail, sender's Sent copy via submit_inbound_mail.
func TestSubmissionDataRoutesMixedRecipients(t *testing.T) {
	t.Parallel()
	bobActor := make([]byte, 32)
	for i := range bobActor {
		bobActor[i] = 0x42
	}
	caller := &submissionAuthCaller{
		validateRecipientByLocal: map[string][]byte{"bob": bobActor},
		mlsPubkey:                freshRecipientPubkey32(t),
		indexPubkey:              freshRecipientPubkey32(t),
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	if err := sess.Mail("alice@example.com", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("MAIL FROM: %v", err)
	}
	for _, rcpt := range []string{"bob@example.com", "dave@external.test"} {
		if err := sess.Rcpt(rcpt, &gosmtp.RcptOptions{}); err != nil {
			t.Fatalf("RCPT %q: %v", rcpt, err)
		}
	}

	err := sess.Data(strings.NewReader(
		"From: alice@example.com\r\nTo: bob@example.com\r\nCc: dave@external.test\r\nMessage-ID: <m@example.com>\r\nSubject: mixed\r\n\r\nhi\r\n"))
	if err != nil {
		t.Fatalf("expected 250 OK; got %v", err)
	}
	if len(caller.ingestInboundCalls) != 1 {
		t.Errorf("expected 1 ingest (Fauna RCPT bob); got %d", len(caller.ingestInboundCalls))
	}
	if len(caller.submitInboundCalls) != 1 {
		t.Errorf("expected 1 submit (Sent copy); got %d", len(caller.submitInboundCalls))
	}
	if len(caller.enqueuedOutbound) != 1 {
		t.Fatalf("expected 1 enqueue (external dave); got %d", len(caller.enqueuedOutbound))
	}
	got := caller.enqueuedOutbound[0]
	if len(got.Recipients) != 1 || got.Recipients[0] != "dave@external.test" {
		t.Errorf("enqueue recipients=%v want [dave@external.test]", got.Recipients)
	}
}

// TestSubmissionDataSenderAsRecipientNoDuplicateSent — sender names
// themselves in TO. dedupLocalRecipients dedups against sess.actorID,
// so the always-fires Sent copy is the only submit_inbound_mail call.
// The behaviour pins the dedup invariant — without it, a user emailing
// themselves would land in their Sent folder twice (once via the RCPT
// path, once via the Sent-copy path).
func TestSubmissionDataSenderAsRecipientNoDuplicateSent(t *testing.T) {
	t.Parallel()
	caller := &submissionAuthCaller{
		mlsPubkey:   freshRecipientPubkey32(t),
		indexPubkey: freshRecipientPubkey32(t),
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	// validate_recipient("alice", "example.com") → sess.actorID; the
	// helper map is keyed by local_part so we can resolve it on demand.
	caller.validateRecipientByLocal = map[string][]byte{"alice": sess.actorID}
	if err := sess.Mail("alice@example.com", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("MAIL FROM: %v", err)
	}
	if err := sess.Rcpt("alice@example.com", &gosmtp.RcptOptions{}); err != nil {
		t.Fatalf("RCPT alice (self): %v", err)
	}

	err := sess.Data(strings.NewReader(
		"From: alice@example.com\r\nTo: alice@example.com\r\nSubject: note-to-self\r\n\r\nremember\r\n"))
	if err != nil {
		t.Fatalf("expected 250 OK; got %v", err)
	}
	if len(caller.submitInboundCalls) != 1 {
		t.Fatalf("expected 1 submit_inbound_mail (dedup against Sent copy); got %d", len(caller.submitInboundCalls))
	}
	if len(caller.ingestInboundCalls) != 0 {
		t.Errorf("expected 0 ingest_inbound_mail (sender is the only RCPT and dedup folds it); got %d", len(caller.ingestInboundCalls))
	}
}

// TestSubmissionRcptLocalRecipientUnknownRejects — a local-domain RCPT
// that validate_recipient rejects is refused at RCPT TO with 550 5.1.1
// (per smtp-server.md § Recipient handling on submission); the
// recipient never reaches Data, and no delivery RPC fires for it.
func TestSubmissionRcptLocalRecipientUnknownRejects(t *testing.T) {
	t.Parallel()
	caller := &submissionAuthCaller{
		// Empty map → every local_part rejects.
		validateRecipientByLocal: map[string][]byte{},
		mlsPubkey:                freshRecipientPubkey32(t),
		indexPubkey:              freshRecipientPubkey32(t),
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	if err := sess.Mail("alice@example.com", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("MAIL FROM: %v", err)
	}
	err := sess.Rcpt("mallory@example.com", &gosmtp.RcptOptions{})
	se := asSMTPError(t, err)
	if se.Code != 550 {
		t.Errorf("expected 550 for unknown local RCPT; got %d (%v)", se.Code, se)
	}
	want := gosmtp.EnhancedCode{5, 1, 1}
	if se.EnhancedCode != want {
		t.Errorf("expected enhanced 5.1.1; got %v", se.EnhancedCode)
	}
	if len(caller.submitInboundCalls)+len(caller.ingestInboundCalls)+len(caller.enqueuedOutbound) != 0 {
		t.Errorf("no delivery RPC should fire when a local RCPT rejects; saw submit=%d ingest=%d enqueue=%d",
			len(caller.submitInboundCalls), len(caller.ingestInboundCalls), len(caller.enqueuedOutbound))
	}
}

// TestSubmissionRcptValidateRecipientTransportError — nest is
// unreachable at RCPT TO. resolveLocalRecipient maps the transport
// error to 451 4.7.1 (try again later) rather than 550 (the sender
// should not bounce — it's a transient infra issue).
func TestSubmissionRcptValidateRecipientTransportError(t *testing.T) {
	t.Parallel()
	caller := &submissionAuthCaller{
		validateRecipientErr: fmt.Errorf("nest unreachable"),
		mlsPubkey:            freshRecipientPubkey32(t),
		indexPubkey:          freshRecipientPubkey32(t),
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	if err := sess.Mail("alice@example.com", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("MAIL FROM: %v", err)
	}
	err := sess.Rcpt("bob@example.com", &gosmtp.RcptOptions{})
	se := asSMTPError(t, err)
	if se.Code != 451 {
		t.Errorf("expected 451 on transport error; got %d (%v)", se.Code, se)
	}
	if se.EnhancedCode != (gosmtp.EnhancedCode{4, 7, 1}) {
		t.Errorf("expected enhanced 4.7.1; got %v", se.EnhancedCode)
	}
}

// TestSubmissionRcptLocalRecipientMissingMLSPubkey — local RCPT
// resolves but the recipient hasn't provisioned an MLS pubkey
// (fetch_recipient_mls_pubkey returns nil). resolveLocalRecipient maps
// this to 550 5.1.1 at RCPT TO — permanent, per-recipient: the rest of
// the envelope is unaffected.
func TestSubmissionRcptLocalRecipientMissingMLSPubkey(t *testing.T) {
	t.Parallel()
	bobActor := make([]byte, 32)
	for i := range bobActor {
		bobActor[i] = 0x42
	}
	caller := &submissionAuthCaller{
		validateRecipientByLocal: map[string][]byte{"bob": bobActor},
		// mlsPubkey stays nil — fetch returns "no pubkey on file".
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	if err := sess.Mail("alice@example.com", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("MAIL FROM: %v", err)
	}
	err := sess.Rcpt("bob@example.com", &gosmtp.RcptOptions{})
	se := asSMTPError(t, err)
	if se.Code != 550 {
		t.Errorf("expected 550 for missing MLS pubkey; got %d (%v)", se.Code, se)
	}
	want := gosmtp.EnhancedCode{5, 1, 1}
	if se.EnhancedCode != want {
		t.Errorf("expected enhanced 5.1.1; got %v", se.EnhancedCode)
	}
}

// TestSubmissionRcptSuccessionPendingRecipientTempfails — local RCPT
// resolves but the recipient is a succession's successor who has not
// yet re-provisioned an MLS pubkey (succession_pending=true on the
// wire). resolveLocalRecipient must map this to 451 4.7.1 (tempfail —
// the sender's MTA retries) rather than the 550 permanent reject a
// never-onboarded recipient gets (smtp-server.md § Error / tempfail
// strategy; succession-aftermath.md § Re-key scope).
func TestSubmissionRcptSuccessionPendingRecipientTempfails(t *testing.T) {
	t.Parallel()
	bobActor := make([]byte, 32)
	for i := range bobActor {
		bobActor[i] = 0x42
	}
	caller := &submissionAuthCaller{
		validateRecipientByLocal:   map[string][]byte{"bob": bobActor},
		mlsPubkeySuccessionPending: true,
		// mlsPubkey stays nil — the successor hasn't re-provisioned yet.
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	if err := sess.Mail("alice@example.com", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("MAIL FROM: %v", err)
	}
	err := sess.Rcpt("bob@example.com", &gosmtp.RcptOptions{})
	se := asSMTPError(t, err)
	if se.Code != 451 {
		t.Errorf("expected 451 tempfail for a succession-pending recipient; got %d (%v)", se.Code, se)
	}
	want := gosmtp.EnhancedCode{4, 7, 1}
	if se.EnhancedCode != want {
		t.Errorf("expected enhanced 4.7.1; got %v", se.EnhancedCode)
	}
}

// ── partitionRecipientsByLocalDomains ─────────────────────────────

func TestPartitionRecipientsByLocalDomains(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name         string
		rcpts        []string
		localDomains []string
		wantLocal    []string
		wantExtern   []string
	}{
		{
			name:         "all local (single domain)",
			rcpts:        []string{"bob@example.com", "carol@example.com"},
			localDomains: []string{"example.com"},
			wantLocal:    []string{"bob@example.com", "carol@example.com"},
			wantExtern:   nil,
		},
		{
			name:         "all external",
			rcpts:        []string{"a@one.test", "b@two.test"},
			localDomains: []string{"example.com"},
			wantLocal:    nil,
			wantExtern:   []string{"a@one.test", "b@two.test"},
		},
		{
			name:         "case-insensitive local match",
			rcpts:        []string{"BOB@EXAMPLE.com"},
			localDomains: []string{"example.com"},
			wantLocal:    []string{"BOB@EXAMPLE.com"},
			wantExtern:   nil,
		},
		{
			name:         "malformed → external",
			rcpts:        []string{"not-an-address", "bob@example.com"},
			localDomains: []string{"example.com"},
			wantLocal:    []string{"bob@example.com"},
			wantExtern:   []string{"not-an-address"},
		},
		{
			name:         "multi-domain partition",
			rcpts:        []string{"a@primary.example", "b@secondary.example", "c@external.test"},
			localDomains: []string{"primary.example", "secondary.example"},
			wantLocal:    []string{"a@primary.example", "b@secondary.example"},
			wantExtern:   []string{"c@external.test"},
		},
		{
			name:         "empty localDomains → all external",
			rcpts:        []string{"a@anywhere.test"},
			localDomains: nil,
			wantLocal:    nil,
			wantExtern:   []string{"a@anywhere.test"},
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			gotLocal, gotExtern := partitionRecipientsByLocalDomains(tc.rcpts, tc.localDomains)
			if !equalStringSlice(gotLocal, tc.wantLocal) {
				t.Errorf("local=%v want %v", gotLocal, tc.wantLocal)
			}
			if !equalStringSlice(gotExtern, tc.wantExtern) {
				t.Errorf("external=%v want %v", gotExtern, tc.wantExtern)
			}
		})
	}
}

func equalStringSlice(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

// TestRunSubmissionListenerRejectsBadKind — sanity guard for an
// invalid kind argument. Would only surface if a future refactor
// added a new enum variant without wiring it; the function falls
// through to a clear error.
func TestRunSubmissionListenerRejectsBadKind(t *testing.T) {
	t.Parallel()
	l, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer l.Close()
	backend := &submissionBackend{logger: slog.Default(), cfg: staticMTAConfig(&mtaLiveConfig{localDomains: []string{"test.example.com"}, primaryDomain: "test.example.com"})}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	tlsCfg := newSelfSignedTLSConfig(t)
	err = runSubmissionListener(ctx, l, backend, submissionListenerKind(99), tlsCfg, "test.example.com", 0, 0, nil, slog.Default())
	if err == nil {
		t.Fatal("expected error for unknown listener kind; got nil")
	}
	if !strings.Contains(fmt.Sprint(err), "unknown submission listener kind") {
		t.Errorf("expected 'unknown submission listener kind' in error; got %v", err)
	}
}
