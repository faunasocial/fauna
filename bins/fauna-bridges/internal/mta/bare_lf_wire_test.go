// Wire-level witness for bare-LF mail at the inbound DATA stage
// (smtp-server.md § Architectural rules: the EF-2 reserved-namespace strip,
// and SMTP-smuggling resistance — "Bare CR / bare LF terminators are NOT
// honored; they remain content").
//
// Unlike the in-process Data() tests, this drives a real listener over TCP and
// writes the DATA payload straight onto the socket: net/smtp's DATA writer
// rewrites bare LF to CRLF, which would hide exactly the bytes under test. So
// what reaches Data() here is whatever go-smtp's DATA reader hands it for a
// bare-LF message — the leg an in-process test cannot measure.
package mta

import (
	"bufio"
	"bytes"
	"encoding/hex"
	"log/slog"
	"net"
	"net/textproto"
	"strings"
	"testing"
	"time"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// bareLFForgedMessage carries a remote sender's forged `X-Fauna-*` delivery
// stamps in a header section whose lines end in bare LF, then a body holding
// a bare-LF `.` line — a smuggling-shaped terminator that must stay content.
const bareLFForgedMessage = "From: alice@example.org\n" +
	"To: bob@test.example\n" +
	"X-Fauna-Address-Matched: forged-alias\n" +
	"X-Fauna-Spam-Threshold: 99\n" +
	"Subject: bare lf\n" +
	"\n" +
	"first body line\n" +
	".\n" +
	"still content after a bare-LF dot\n"

// smtpExpect sends one command (skipped when cmd is empty) and requires the
// reply code.
func smtpExpect(t *testing.T, tc *textproto.Conn, code int, cmd string) {
	t.Helper()
	if cmd != "" {
		if err := tc.PrintfLine("%s", cmd); err != nil {
			t.Fatalf("%s: send: %v", cmd, err)
		}
	}
	if _, msg, err := tc.ReadResponse(code); err != nil {
		t.Fatalf("%q: want %d, got %v (%s)", cmd, code, err, msg)
	}
}

// TestWireBareLFHeadersCannotForgeFaunaStamps: bob's filter rule discards any
// message whose X-Fauna-Address-Matched contains the forged value, so a
// surviving forgery would drop his copy; carol forwards all, and her forward
// copy is the plaintext of the same stripped bytes the sealed copies are built
// from (the sealed ones are opaque to the mock).
func TestWireBareLFHeadersCannotForgeFaunaStamps(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	bobID := bytes.Repeat([]byte{0x42}, 32)
	carolID := bytes.Repeat([]byte{0x43}, 32)
	mc.decisions["bob@test.example"] = rcptDecision{resolved: true, actorID: bobID}
	mc.decisions["carol@test.example"] = rcptDecision{resolved: true, actorID: carolID}
	mc.mlsPubkeys[hex.EncodeToString(bobID)] = freshX25519Pubkey(t)
	mc.mlsPubkeys[hex.EncodeToString(carolID)] = freshX25519Pubkey(t)
	mc.filterRules[hex.EncodeToString(bobID)] = []wsrpc.EmailFilterWire{{
		ID:          1,
		Name:        "drop forged alias match",
		Combination: "All",
		Rules: []cbor.RawMessage{mustCBOR(t, map[string]any{
			"HeaderContains": map[string]any{"name": "X-Fauna-Address-Matched", "value": "forged-alias"},
		})},
		Action: mustCBOR(t, "Discard"),
	}}
	mc.forwardConfigs[hex.EncodeToString(carolID)] = "carol-personal@example.net"

	addr, cancel := startBackendListener(t, &inboundBackend{
		logger: slog.Default(),
		caller: mc,
		cfg: staticMTAConfig(&mtaLiveConfig{
			localDomains: []string{"test.example"},
		}),
	})
	defer cancel()

	conn, err := net.DialTimeout("tcp", addr, 5*time.Second)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer func() { _ = conn.Close() }()
	_ = conn.SetDeadline(time.Now().Add(30 * time.Second))
	tc := textproto.NewConn(conn)
	smtpExpect(t, tc, 220, "")
	smtpExpect(t, tc, 250, "HELO loopback.example")
	smtpExpect(t, tc, 250, "MAIL FROM:<alice@example.org>")
	smtpExpect(t, tc, 250, "RCPT TO:<bob@test.example>")
	smtpExpect(t, tc, 250, "RCPT TO:<carol@test.example>")
	smtpExpect(t, tc, 354, "DATA")
	// Raw bytes on the socket: only the CRLF.CRLF terminator is CRLF.
	w := bufio.NewWriter(conn)
	if _, err := w.WriteString(bareLFForgedMessage + "\r\n.\r\n"); err != nil {
		t.Fatalf("write DATA: %v", err)
	}
	if err := w.Flush(); err != nil {
		t.Fatalf("flush DATA: %v", err)
	}
	smtpExpect(t, tc, 250, "")
	smtpExpect(t, tc, 221, "QUIT")

	// The filter context carried no forged stamp: bob's Discard rule did not
	// fire, so both recipients were ingested.
	ingested := map[string]bool{}
	for _, req := range mc.ingestRequests {
		ingested[hex.EncodeToString(req.actorID)] = true
	}
	if !ingested[hex.EncodeToString(bobID)] {
		t.Errorf("bob's copy was discarded: a forged X-Fauna-Address-Matched reached his filter context")
	}
	if !ingested[hex.EncodeToString(carolID)] {
		t.Errorf("carol's copy was not ingested; ingests=%d", len(mc.ingestRequests))
	}

	if got := len(mc.forwardRequests); got != 1 {
		t.Fatalf("forward count: got %d, want 1 (carol)", got)
	}
	raw := mc.forwardRequests[0].rawMessage
	for _, forged := range []string{"X-Fauna-Address-Matched", "X-Fauna-Spam-Threshold", "forged-alias"} {
		if bytes.Contains(raw, []byte(forged)) {
			t.Errorf("forged %q survived the inbound X-Fauna-* strip:\n%q", forged, raw)
		}
	}
	// The bare-LF `.` line did not end DATA: everything after it arrived as
	// content, and the sender's headers survived intact.
	for _, kept := range []string{"From: alice@example.org", "Subject: bare lf", "first body line", "still content after a bare-LF dot"} {
		if !bytes.Contains(raw, []byte(kept)) {
			t.Errorf("lost %q from the delivered bytes:\n%q", kept, raw)
		}
	}
	// What go-smtp hands Data() for a bare-LF line: recorded, not assumed.
	t.Logf("forward copy as delivered: %q", raw)
	if !strings.Contains(string(raw), "Subject: bare lf\n") && !strings.Contains(string(raw), "Subject: bare lf\r\n") {
		t.Errorf("Subject line framing unrecognised:\n%q", raw)
	}
}
