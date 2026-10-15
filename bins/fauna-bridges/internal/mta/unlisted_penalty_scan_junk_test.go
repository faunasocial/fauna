package mta

import (
	"bytes"
	"encoding/hex"
	"log/slog"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/scan"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// TestUnlistedRecipientPenaltyNeverLowersClamavJunk pins: the
// unlisted-recipient penalty re-derives a catch-all recipient's disposition
// from the penalized score, and that re-derivation must never lower the
// message-global ClamAV junk filing. With the admin's infected action set to
// junk and a penalized score still under the spam-folder tier, the recipient
// must still get PolicyJunk (the nest then records action_taken=junked), never
// Accept (INBOX, recorded "tagged"). mail-content-scanning.md § ClamAV routing;
// mail-spam.md § Unlisted-recipient penalty.
func TestUnlistedRecipientPenaltyNeverLowersClamavJunk(t *testing.T) {
	t.Parallel()
	clamdAddr, stop := fakeClamd(t, "stream: Win.Test.EICAR FOUND\x00")
	defer stop()

	mc := newMockCaller()
	actor := bytes.Repeat([]byte{0x42}, 32)
	mc.mlsPubkeys[hex.EncodeToString(actor)] = freshX25519Pubkey(t)

	policy := scan.PolicyDefault()
	policy.ClamavEnabled = true
	policy.RspamdEnabled = false
	policy.ClamavActionOnInfected = mailfauna.ClamavActionJunk

	s := &inboundSession{
		logger:   slog.Default(),
		clientIP: "127.0.0.1",
		from:     "alice@example.org",
		inboundRcpts: []resolvedInboundRcpt{{
			actorID: actor,
			headersToStamp: []wsrpc.StampedHeader{
				{Name: headerAddressCatchall, Value: "1"},
			},
		}},
		caller:          mc,
		localDomains:    []string{"test.example"},
		maxMessageBytes: 50_000_000,
		// Spam-folder tier far above the penalty, so the penalized score alone
		// decides Accept.
		spamPolicy:               mailfauna.SpamPolicy{SpamFolderThreshold: 50_000, RejectThreshold: 0},
		unlistedRecipientPenalty: 1,
		scanConfig:               scan.Config{ClamdAddr: clamdAddr, Policy: policy},
	}
	body := "From: alice@example.org\r\n" +
		"To: nobody@test.example\r\n" +
		"Subject: infected\r\n" +
		"Date: Sun, 17 Mar 2024 12:34:56 +0000\r\n" +
		"\r\n" +
		"X5O!P%@AP\r\n"
	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.ingestRequests); got != 1 {
		t.Fatalf("ingest call count: got %d, want 1", got)
	}
	wantWire, _ := mailfauna.SpamDispositionToWire(mailfauna.SpamDispositionPolicyJunk)
	if got := mc.ingestRequests[0].spamDisposition; got != wantWire {
		t.Errorf("spam_disposition: got %q, want %q (the penalty lowered the ClamAV junk filing)", got, wantWire)
	}
}
