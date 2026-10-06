// Tests for the `X-Fauna-Authenticated-Sender` stamp at the two MTA doors
// (smtp-server.md § Architectural rules → The `X-Fauna-*` namespace, the
// authenticated-sender stamp): the MX door stamps the `From:` addr-spec only
// under a DMARC pass, the submission door stamps the envelope sender it
// validated as owned — and neither lets a sender-supplied copy through, nor
// lets the stamp leak onto a message that leaves the deployment.
//
// The sealed copies are opaque to the mocks (no test holds a recipient's
// private half), so the Data-level assertions measure the one observable that
// depends on the stamp — the sealed ciphertext's size, which grows by exactly
// the stamp line's length — the same technique as
// TestStampedHeaderValueEntersTheSealedLocalCopy. The line's exact content is
// pinned on the pure helpers the doors call.
package mta

import (
	"bytes"
	"encoding/hex"
	"log/slog"
	"strings"
	"testing"

	gosmtp "github.com/emersion/go-smtp"

	faunaCore "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_core"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

const authSenderHeader = "X-Fauna-Authenticated-Sender"

// stampLineLen is the byte count prependHeaders adds for one stamp line.
func stampLineLen(addr string) uint32 {
	return uint32(len(authSenderHeader + ": " + addr + "\r\n"))
}

func TestMxAuthenticatedSenderStampOnlyUnderDmarcPass(t *testing.T) {
	t.Parallel()
	withDmarc := func(d faunaCore.DmarcVerdict) *mailfauna.AuthVerdicts {
		v := verdictsWith(dkimPass(), d)
		return &v
	}
	for _, tc := range []struct {
		name     string
		verdicts *mailfauna.AuthVerdicts
		from     string
		want     string
	}{
		{"pass, bare addr-spec", withDmarc(dmarcPass()), "alice@example.org",
			"X-Fauna-Authenticated-Sender: alice@example.org"},
		{"pass, display name stripped and lower-cased", withDmarc(dmarcPass()), `"Alice Example" <Alice@Example.ORG>`,
			"X-Fauna-Authenticated-Sender: alice@example.org"},
		{"pass, unparseable From", withDmarc(dmarcPass()), "not an address", ""},
		{"pass, two mailboxes", withDmarc(dmarcPass()), "a@example.org, b@example.org", ""},
		{"pass, empty From", withDmarc(dmarcPass()), "", ""},
		{"none", withDmarc(dmarcNone()), "alice@example.org", ""},
		{"fail policy=none", withDmarc(dmarcFailNone()), "alice@example.org", ""},
		{"fail policy=quarantine", withDmarc(dmarcFailQuarantine()), "alice@example.org", ""},
		{"fail policy=reject", withDmarc(dmarcFailReject()), "alice@example.org", ""},
		{"temperror", withDmarc(faunaCore.DmarcVerdictTempError{}), "alice@example.org", ""},
		{"permerror", withDmarc(faunaCore.DmarcVerdictPermError{}), "alice@example.org", ""},
		{"no verdicts", nil, "alice@example.org", ""},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			if got := mxAuthenticatedSenderStamp(tc.verdicts, tc.from); got != tc.want {
				t.Errorf("mxAuthenticatedSenderStamp(%s, %q) = %q, want %q", tc.name, tc.from, got, tc.want)
			}
		})
	}
}

func TestSealedCopyStampLines(t *testing.T) {
	t.Parallel()
	alias := []wsrpc.StampedHeader{{Name: "X-Fauna-Address-Suffix", Value: "work"}}
	got := sealedCopyStampLines("X-Fauna-Authenticated-Sender: a@example.org", alias)
	want := []string{"X-Fauna-Authenticated-Sender: a@example.org", "X-Fauna-Address-Suffix: work"}
	if strings.Join(got, "\n") != strings.Join(want, "\n") {
		t.Errorf("with a stamp: got %q, want %q", got, want)
	}
	if got := sealedCopyStampLines("", alias); len(got) != 1 || got[0] != "X-Fauna-Address-Suffix: work" {
		t.Errorf("without a stamp the recipient's own stamps pass through alone; got %q", got)
	}
	if got := sealedCopyStampLines("", nil); len(got) != 0 {
		t.Errorf("nothing to stamp → no lines; got %q", got)
	}
}

// mxDelivery drives the MX door's Data for one local recipient who also has
// forward-all set, under the hermetic DMARC verdict `dmarc` (four bytes, so
// every arm's payload is the same length), and returns the sealed copy's
// ciphertext size plus the plaintext of the external forward copy.
func mxDelivery(t *testing.T, dmarc, extraHeaders string) (uint32, []byte) {
	t.Helper()
	mc := newMockCaller()
	actor := bytes.Repeat([]byte{0x42}, 32)
	mc.mlsPubkeys[hex.EncodeToString(actor)] = freshX25519Pubkey(t)
	mc.forwardConfigs[hex.EncodeToString(actor)] = "bob-personal@example.net"
	s := &inboundSession{
		logger:          slog.Default(),
		clientIP:        "127.0.0.1",
		from:            "alice@example.org",
		inboundRcpts:    []resolvedInboundRcpt{{actorID: actor, rcptAddr: "bob@test.example"}},
		caller:          mc,
		localDomains:    []string{"test.example"},
		maxMessageBytes: 50_000_000,
		spamPolicy:      permissiveSpamPolicyForTest(),
	}
	body := hermeticDmarcHeader + ": " + dmarc + "\r\n" +
		extraHeaders +
		"From: \"Alice Example\" <Alice@Example.org>\r\n" +
		"To: bob@test.example\r\n" +
		"Subject: stamp me\r\n" +
		"\r\n" +
		"hello\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data (dmarc=%s): %v", dmarc, err)
	}
	if len(mc.ingestRequests) != 1 {
		t.Fatalf("dmarc=%s: want 1 ingest, got %d", dmarc, len(mc.ingestRequests))
	}
	if len(mc.forwardRequests) != 1 {
		t.Fatalf("dmarc=%s: want 1 forward, got %d", dmarc, len(mc.forwardRequests))
	}
	size := mc.ingestRequests[0].publicMetadata.CiphertextSize
	if size == 0 {
		t.Fatalf("dmarc=%s: no ciphertext sealed", dmarc)
	}
	return size, mc.forwardRequests[0].rawMessage
}

// TestMxDataStampsFromOnlyUnderDmarcPass: under a DMARC pass the recipient's
// sealed copy grows by exactly the stamp line naming the lower-cased `From:`
// addr-spec (display name dropped); under DMARC none or fail it carries no
// stamp. The external forward copy never carries it — the stamp is internal
// delivery metadata, prepended per recipient, after the forward split.
func TestMxDataStampsFromOnlyUnderDmarcPass(t *testing.T) {
	t.Parallel()
	none, noneFwd := mxDelivery(t, "none", "")
	fail, failFwd := mxDelivery(t, "fail", "")
	pass, passFwd := mxDelivery(t, "pass", "")

	if want := none + stampLineLen("alice@example.org"); pass != want {
		t.Errorf("DMARC pass sealed %d bytes, want %d (DMARC-none size %d + the stamp line naming alice@example.org)",
			pass, want, none)
	}
	if fail != none {
		t.Errorf("DMARC fail sealed %d bytes, want %d (same as DMARC none: no stamp)", fail, none)
	}
	for name, fwd := range map[string][]byte{"none": noneFwd, "fail": failFwd, "pass": passFwd} {
		if bytes.Contains(bytes.ToLower(fwd), bytes.ToLower([]byte(authSenderHeader))) {
			t.Errorf("dmarc=%s: the external forward copy carries the internal %s stamp:\n%s", name, authSenderHeader, fwd)
		}
	}
}

// TestMxDataStripsForgedAuthenticatedSender: a sender-supplied stamp (naming
// someone else) never survives the MX door. Under DMARC fail the delivered
// copy is exactly the unstamped size — the forgery was stripped and nothing
// replaced it; under DMARC pass it is exactly one genuine stamp line longer.
func TestMxDataStripsForgedAuthenticatedSender(t *testing.T) {
	t.Parallel()
	forged := authSenderHeader + ": mallory@evil.example\r\n"
	failClean, _ := mxDelivery(t, "fail", "")
	failForged, failForgedFwd := mxDelivery(t, "fail", forged)
	passClean, _ := mxDelivery(t, "pass", "")
	passForged, _ := mxDelivery(t, "pass", forged)

	if failForged != failClean {
		t.Errorf("DMARC fail with a forged stamp sealed %d bytes, want %d (forgery stripped, no stamp written)",
			failForged, failClean)
	}
	if passForged != passClean {
		t.Errorf("DMARC pass with a forged stamp sealed %d bytes, want %d (forgery stripped, one genuine stamp)",
			passForged, passClean)
	}
	if bytes.Contains(failForgedFwd, []byte("mallory@evil.example")) {
		t.Errorf("the forged stamp survived the MX door into the forward copy:\n%s", failForgedFwd)
	}
}

// TestSubmissionFaunaRecipientCopyStampsValidatedEnvelopeSender: the
// submission door stamps the envelope sender MAIL FROM validated as owned
// (alice@example.com) — not the `From:` header, which names a different
// address of the same actor (an owned alias; a From: the actor does not own
// is refused outright — mail-multidomain.md § From: header ownership). Only
// the Fauna recipient's copy carries it: the Sent copy is the same bytes
// without it, and the relayed external message carries neither the genuine
// stamp nor the sender's forged one.
func TestSubmissionFaunaRecipientCopyStampsValidatedEnvelopeSender(t *testing.T) {
	t.Parallel()
	bobActor := bytes.Repeat([]byte{0x42}, 32)
	caller := &submissionAuthCaller{
		validateRecipientByLocal: map[string][]byte{
			"bob":                   bobActor,
			"someone.else.entirely": sessionActorID(),
		},
		mlsPubkey:   freshRecipientPubkey32(t),
		indexPubkey: freshRecipientPubkey32(t),
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	if err := sess.Mail("alice@example.com", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("MAIL FROM: %v", err)
	}
	for _, rcpt := range []string{"bob@example.com", "dave@external.test"} {
		if err := sess.Rcpt(rcpt, &gosmtp.RcptOptions{}); err != nil {
			t.Fatalf("RCPT %s: %v", rcpt, err)
		}
	}
	err := sess.Data(strings.NewReader(
		authSenderHeader + ": bob@example.com\r\n" +
			"From: \"Someone Else\" <someone.else.entirely@example.com>\r\n" +
			"To: bob@example.com, dave@external.test\r\n" +
			"Message-ID: <stamp@example.com>\r\n" +
			"Subject: hi\r\n" +
			"Date: Mon, 1 Jan 2024 00:00:00 +0000\r\n\r\n" +
			// A body long enough that both copies' plaintexts sit on the same
			// side of the seal envelope's 256-byte length-prefix boundary, so
			// the size difference is the stamp line alone.
			strings.Repeat("body ", 80) + "\r\n"))
	if err != nil {
		t.Fatalf("expected nil (250 OK); got %v", err)
	}
	if len(caller.ingestInboundCalls) != 1 || len(caller.submitInboundCalls) != 1 || len(caller.enqueuedOutbound) != 1 {
		t.Fatalf("want 1 Fauna-recipient ingest, 1 Sent copy, 1 relay; got %d, %d, %d",
			len(caller.ingestInboundCalls), len(caller.submitInboundCalls), len(caller.enqueuedOutbound))
	}
	recipient := caller.ingestInboundCalls[0].EncryptedBodySize
	sent := caller.submitInboundCalls[0].EncryptedBodySize
	if want := sent + int(stampLineLen("alice@example.com")); recipient != want {
		t.Errorf("Fauna recipient copy sealed %d bytes, want %d (the Sent copy's %d + the stamp line naming "+
			"the validated envelope sender alice@example.com; a From:-header stamp would differ in length)",
			recipient, want, sent)
	}
	relayed := caller.enqueuedOutbound[0].RawMessage
	if bytes.Contains(bytes.ToLower(relayed), bytes.ToLower([]byte(authSenderHeader))) {
		t.Errorf("the relayed external message carries an %s stamp:\n%s", authSenderHeader, relayed)
	}
}
