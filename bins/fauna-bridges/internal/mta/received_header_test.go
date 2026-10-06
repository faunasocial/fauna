// Tests for the inbound `Received:` trace-header prepend (smtp-server.md
// § Architectural rules). The canonical header itself
// is exhaustively unit-tested in shared Rust (libs/fauna-mail/src/received_header.rs);
// these tier_1 tests pin the Go *wiring* — that Data builds the opts from the
// right session fields, that the header crosses the UniFFI boundary, that the
// sanitizer fires end-to-end, and that exactly one Received header is stamped.
//
// We observe the plaintext post-prepend `raw` via the forward path (the forward
// copy carries the Received-stamped bytes), since the locally-delivered copy is
// HPKE-sealed before it reaches the mock caller.
package mta

import (
	"bytes"
	"encoding/hex"
	"regexp"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
)

func TestDataPrependsCanonicalReceivedHeader(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actor := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actor, "alice@example.org")
	// Single envelope recipient → the `for <addr>;` clause is emitted.
	s.rcpts = []string{"bob@test.example"}
	mc.forwardConfigs[hex.EncodeToString(actor)] = "bob-personal@example.net"

	body := "From: alice@example.org\r\n" +
		"To: bob@test.example\r\n" +
		"Subject: hi\r\n" +
		"\r\n" +
		"hello\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.forwardRequests); got != 1 {
		t.Fatalf("forward count: got %d, want 1", got)
	}
	raw := mc.forwardRequests[0].rawMessage

	// Exactly one Fauna Received header (no original-message Received in the body).
	if got := bytes.Count(raw, []byte("Received: from")); got != 1 {
		t.Fatalf("want exactly one Received header, got %d in:\n%s", got, raw)
	}
	// helo is empty (nil conn) → sanitized to `unknown`; clientIP rides verbatim;
	// `by` host = first local domain; cleartext → `with ESMTP` (no cipher).
	wantFrom := "Received: from unknown ([127.0.0.1])\r\n\tby test.example with ESMTP\r\n\tid "
	if !bytes.Contains(raw, []byte(wantFrom)) {
		t.Errorf("missing canonical from/by/with prefix; got:\n%s", raw)
	}
	// Single recipient → `for` clause with the closing `;`.
	if !bytes.Contains(raw, []byte("\r\n\tfor <bob@test.example>;\r\n")) {
		t.Errorf("missing single-recipient `for` clause; got:\n%s", raw)
	}
	// The queue id is a fresh 16-hex token from NewQueueID.
	if !regexp.MustCompile(`\r\n\tid [0-9a-f]{16}\r\n`).Match(raw) {
		t.Errorf("queue id is not 16 hex chars; got:\n%s", raw)
	}
}

func TestDataReceivedHeaderSanitizesClientIPCrossingFFI(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actor := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actor, "alice@example.org")
	// A header-injection attempt smuggled through the client IP — the shared
	// Rust sanitizer must collapse it to `unknown` (CR/LF → `unknown`), and no
	// forged header line may appear.
	s.clientIP = "10.0.0.1\r\nX-Evil-Injected: pwned"
	mc.forwardConfigs[hex.EncodeToString(actor)] = "bob-personal@example.net"

	body := "From: alice@example.org\r\nTo: bob@test.example\r\nSubject: hi\r\n\r\nhello\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.forwardRequests); got != 1 {
		t.Fatalf("forward count: got %d, want 1", got)
	}
	raw := mc.forwardRequests[0].rawMessage
	if !bytes.Contains(raw, []byte("from unknown ([unknown])")) {
		t.Errorf("malicious client IP not sanitized to unknown; got:\n%s", raw)
	}
	if bytes.Contains(raw, []byte("X-Evil-Injected")) {
		t.Fatalf("header injection NOT defeated — forged line survived:\n%s", raw)
	}
}

// TestDataStripsForgedFaunaStampHeaders (EF-2): a sender forges reserved
// `X-Fauna-*` delivery-stamp headers, trying to spoof matched-alias metadata
// the recipient's filter rules match on. Data must strip them before parse +
// seal, so they reach neither the filter context nor any delivered/forwarded
// copy. We observe via the forward copy (which carries the *unstamped* raw, so a
// genuine `X-Fauna-Address-*` is never added there — any occurrence would be the
// sender's forgery surviving). The inbound `X-Fauna-Forwarded-By` preservation
// is covered end-to-end by TestForwardSuppressedSelfSeen and at the unit level
// by libs/fauna-mail/src/received_header.rs.
func TestDataStripsForgedFaunaStampHeaders(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actor := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actor, "alice@example.org")
	s.rcpts = []string{"bob@test.example"}
	mc.forwardConfigs[hex.EncodeToString(actor)] = "bob-personal@example.net"

	body := "X-Fauna-Address-Suffix: admin\r\n" +
		"X-Fauna-Scan-Clamav: clean\r\n" +
		"X-Fauna-Evil: pwned\r\n" +
		"From: alice@example.org\r\n" +
		"To: bob@test.example\r\n" +
		"Subject: hi\r\n\r\nhello\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.forwardRequests); got != 1 {
		t.Fatalf("forward count: got %d, want 1", got)
	}
	raw := mc.forwardRequests[0].rawMessage
	for _, forged := range []string{
		"X-Fauna-Address-Suffix",
		"X-Fauna-Evil",
		"admin",
		"pwned",
	} {
		if bytes.Contains(raw, []byte(forged)) {
			t.Errorf("forged %q survived the inbound X-Fauna-* strip:\n%s", forged, raw)
		}
	}
	// The legitimate sender header is untouched.
	if !bytes.Contains(raw, []byte("From: alice@example.org")) {
		t.Errorf("legitimate sender header was lost:\n%s", raw)
	}
}

// TestNewQueueIDFormat pins the queue-id shape used by BuildReceivedHeader's
// `id` clause: 16 lowercase hex chars, fresh per call.
func TestNewQueueIDFormat(t *testing.T) {
	t.Parallel()
	re := regexp.MustCompile(`^[0-9a-f]{16}$`)
	a, b := mailfauna.NewQueueID(), mailfauna.NewQueueID()
	if !re.MatchString(a) || !re.MatchString(b) {
		t.Fatalf("queue ids not 16-hex: %q %q", a, b)
	}
	if a == b {
		t.Errorf("queue ids should differ across calls: %q == %q", a, b)
	}
}
