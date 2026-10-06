// Tests for carrying a fired `Allow` filter rule past delivery
// (docs/goal/behavior/email-filters.md § Multi-action composition — `Allow`
// overrides the spam disposition at every scoring position). The MTA replaces
// the recipient's RCPT-time `X-Fauna-Spam-Threshold` stamp with the disabled
// tier `0`, as the FIRST threshold line, because every post-delivery scorer
// reads the first match. tier_1: in-process against a mockCaller.
package mta

import (
	"bytes"
	"encoding/hex"
	"log/slog"
	"strconv"
	"strings"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// allowRule is a stored filter whose SubjectContains condition fires on "fwd"
// (filterForwardBody's subject) and whose action is `Allow`.
func allowRule(t *testing.T, id int64, cont bool) wsrpc.EmailFilterWire {
	t.Helper()
	r := forwardRule(t, id, "unused@example.net", cont)
	r.Name = "allow rule"
	r.Action = mustCBOR(t, "Allow")
	return r
}

// fileIntoRule is allowRule with a `FileInto` action instead.
func fileIntoRule(t *testing.T, id int64, mailbox string, cont bool) wsrpc.EmailFilterWire {
	t.Helper()
	r := forwardRule(t, id, "unused@example.net", cont)
	r.Name = "file-into rule"
	r.Action = mustCBOR(t, map[string]any{"FileInto": map[string]any{"mailbox": mailbox}})
	return r
}

// thresholdLines returns every X-Fauna-Spam-Threshold field line in the
// header section of msg, in order (case-insensitive field-name match).
func thresholdLines(msg []byte) []string {
	head := string(msg)
	if i := strings.Index(head, "\r\n\r\n"); i >= 0 {
		head = head[:i]
	}
	var out []string
	for _, line := range strings.Split(head, "\r\n") {
		if strings.HasPrefix(strings.ToLower(line), "x-fauna-spam-threshold:") {
			out = append(out, line)
		}
	}
	return out
}

// stampString renders a ReadSpamThresholdStamp result for a failure message.
func stampString(v *uint32) string {
	if v == nil {
		return "<no stamp>"
	}
	return strconv.FormatUint(uint64(*v), 10)
}

// TestRecipientSealedCopyAllowReplacesTheRcptStamp pins the judge's ordering
// rule: the Allow override is the first — and only — threshold line, so the
// reader's first-match lands on 0, never on nest's RCPT-time value.
func TestRecipientSealedCopyAllowReplacesTheRcptStamp(t *testing.T) {
	t.Parallel()
	raw := []byte(filterForwardBody)
	stamps := []wsrpc.StampedHeader{
		{Name: "X-Fauna-Address-Suffix", Value: "work"},
		{Name: "X-Fauna-Spam-Threshold", Value: "5"},
	}

	plain := recipientSealedCopy(raw, "", stamps, false)
	if got := mailfauna.ReadSpamThresholdStamp(plain); got == nil || *got != 5 {
		t.Fatalf("no Allow: stamp = %s, want nest's 5", stampString(got))
	}
	if !bytes.Equal(plain, prependHeaders(raw, sealedCopyStampLines("", stamps))) {
		t.Errorf("no Allow must leave the stamped copy byte-identical")
	}

	allowed := recipientSealedCopy(raw, "", stamps, true)
	if got := mailfauna.ReadSpamThresholdStamp(allowed); got == nil || *got != 0 {
		t.Fatalf("Allow: stamp = %s, want 0", stampString(got))
	}
	lines := thresholdLines(allowed)
	if len(lines) != 1 || lines[0] != "X-Fauna-Spam-Threshold: 0" {
		t.Errorf("Allow: threshold lines = %q, want exactly [X-Fauna-Spam-Threshold: 0]", lines)
	}
	if !bytes.HasPrefix(allowed, []byte("X-Fauna-Spam-Threshold: 0\r\n")) {
		t.Errorf("Allow: override is not the first line:\n%q", allowed)
	}
	for _, kept := range []string{"X-Fauna-Address-Suffix: work\r\n", "Subject: please fwd this\r\n", "\r\n\r\nhello\r\n"} {
		if !bytes.Contains(allowed, []byte(kept)) {
			t.Errorf("Allow: lost %q:\n%q", kept, allowed)
		}
	}
}

// TestFilterAllowSealsTheZeroThresholdStamp drives Data(): a fired Allow seals
// the recipient's copy with the 0 stamp in place of nest's two-digit "15" —
// exactly one byte shorter than the unmatched control — while an Allow that a
// later FileInto overrides (last placement wins) keeps nest's stamp.
func TestFilterAllowSealsTheZeroThresholdStamp(t *testing.T) {
	t.Parallel()
	deliver := func(rules []wsrpc.EmailFilterWire) uint32 {
		t.Helper()
		mc := newMockCaller()
		actor := bytes.Repeat([]byte{0x42}, 32)
		mc.mlsPubkeys[hex.EncodeToString(actor)] = freshX25519Pubkey(t)
		mc.filterRules[hex.EncodeToString(actor)] = rules
		s := &inboundSession{
			logger:   slog.Default(),
			clientIP: "127.0.0.1",
			from:     "alice@example.org",
			inboundRcpts: []resolvedInboundRcpt{{
				actorID:  actor,
				rcptAddr: "bob@test.example",
				headersToStamp: []wsrpc.StampedHeader{
					{Name: "X-Fauna-Spam-Threshold", Value: "15"},
				},
			}},
			caller:          mc,
			localDomains:    []string{"test.example"},
			maxMessageBytes: 50_000_000,
			spamPolicy:      permissiveSpamPolicyForTest(),
		}
		if err := s.Data(bytes.NewReader([]byte(filterForwardBody))); err != nil {
			t.Fatalf("Data: %v", err)
		}
		if len(mc.ingestRequests) != 1 {
			t.Fatalf("want 1 ingest, got %d", len(mc.ingestRequests))
		}
		return mc.ingestRequests[0].publicMetadata.CiphertextSize
	}

	control := deliver(nil)
	allowed := deliver([]wsrpc.EmailFilterWire{allowRule(t, 1, false)})
	overridden := deliver([]wsrpc.EmailFilterWire{allowRule(t, 1, true), fileIntoRule(t, 2, "INBOX", false)})
	if control == 0 {
		t.Fatalf("no ciphertext sealed")
	}
	if allowed != control-1 {
		t.Errorf("Allow sealed %d bytes, control %d: want exactly 1 fewer ('0' replacing nest's '15')", allowed, control)
	}
	if overridden != control {
		t.Errorf("Allow then FileInto sealed %d bytes, control %d: a later FileInto wins placement and keeps nest's stamp", overridden, control)
	}
}
