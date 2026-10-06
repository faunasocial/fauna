// Tests for the submission RCPT-TO cutover from the exact-only
// validate_recipient to the alias-expanding resolve_recipient (mail-forwarding
// Slice 4). The Resolved + Reject + transport-error outcomes are covered by the
// existing submission_test.go RCPT/Data tests (now routed through
// resolve_recipient); this file covers the genuinely new behavior the cutover
// unlocks on the submission path — the admin external-forwarder Forward outcome
// (mail-aliases.md § Kind 7): a local authenticated user emailing a forwarder
// address (info@<our-domain>) is redirected to the external target, attributed to
// the managing admin, with NO local copy (copy_mode=redirect, mail-forwarding.md
// § Admin external forwarders). All tier_1 (in-process Mail/Rcpt/Data with a
// submissionAuthCaller; the message parse rides the shared-Rust FFI).
package mta

import (
	"bytes"
	"encoding/hex"
	"strings"
	"testing"

	gosmtp "github.com/emersion/go-smtp"
)

// adminActor returns a deterministic 32-byte managing-admin actor id.
func adminActor() []byte {
	a := make([]byte, 32)
	for i := range a {
		a[i] = 0x7E
	}
	return a
}

// TestSubmissionForwarderRedirectsExternalNoLocalCopy: a local authenticated
// user submits to an admin external forwarder (info@example.com → an external
// address). resolve_recipient returns Forward; Data redirects it through the
// shared forward dispatch (copy_mode=redirect, attributed to the admin) and keeps
// no local copy. The sender's own Sent copy still fires; the forwarder address —
// being local-domain — is never enqueued as an external recipient.
func TestSubmissionForwarderRedirectsExternalNoLocalCopy(t *testing.T) {
	t.Parallel()
	admin := adminActor()
	caller := &submissionAuthCaller{
		resolveForwarders: map[string]submissionForwarderDecision{
			"info": {target: "oldaccount@example.net", admin: admin},
		},
		mlsPubkey:   freshRecipientPubkey32(t),
		indexPubkey: freshRecipientPubkey32(t),
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	if err := sess.Mail("alice@example.com", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("MAIL FROM: %v", err)
	}
	if err := sess.Rcpt("info@example.com", &gosmtp.RcptOptions{}); err != nil {
		t.Fatalf("RCPT info (forwarder) must be accepted at RCPT TO; got %v", err)
	}
	err := sess.Data(strings.NewReader(
		"From: alice@example.com\r\nTo: info@example.com\r\nMessage-ID: <fwd@example.com>\r\nSubject: hello info\r\nDate: Mon, 1 Jan 2024 00:00:00 +0000\r\n\r\nbody\r\n"))
	if err != nil {
		t.Fatalf("expected 250 OK; got %v", err)
	}

	// One forward, redirect-shaped, attributed to the managing admin.
	if got := len(caller.forwardRequests); got != 1 {
		t.Fatalf("forward count: got %d, want 1; reqs=%+v", got, caller.forwardRequests)
	}
	fr := caller.forwardRequests[0]
	if !bytes.Equal(fr.actorID, admin) {
		t.Errorf("forward attributed to managing admin: got %x, want %x", fr.actorID, admin)
	}
	if fr.destination != "oldaccount@example.net" {
		t.Errorf("destination: got %q, want oldaccount@example.net", fr.destination)
	}
	if fr.copyMode != "redirect" {
		t.Errorf("copy_mode: got %q, want redirect", fr.copyMode)
	}
	if fr.ruleIDOrForwardAll != "forwarder" {
		t.Errorf("rule: got %q, want forwarder", fr.ruleIDOrForwardAll)
	}
	// The original sender is the authenticated local user (the SRS-rewrite-at-
	// queue-out encodes them; bounces route to the admin forwarder owner).
	if fr.originalSender != "alice@example.com" {
		t.Errorf("original_sender: got %q, want alice@example.com", fr.originalSender)
	}
	if fr.originalMsgID != "fwd@example.com" {
		t.Errorf("original_msgid: got %q, want the source Message-ID", fr.originalMsgID)
	}
	// The forwarded copy carries our loop-detection stamp with the admin actor.
	if !bytes.Contains(fr.rawMessage, []byte("actor="+hex.EncodeToString(admin))) {
		t.Errorf("forwarded stamp missing the admin actor id")
	}
	if !bytes.Contains(fr.rawMessage, []byte("rule=forwarder")) {
		t.Errorf("forwarded stamp missing rule=forwarder")
	}

	// No local copy for the forwarder (redirect-shaped, mail-forwarding.md:59).
	if got := len(caller.ingestInboundCalls); got != 0 {
		t.Errorf("forwarder must write NO local copy; got %d ingest calls", got)
	}
	// The forwarder address is local-domain → never enqueued as external.
	if got := len(caller.enqueuedOutbound); got != 0 {
		t.Errorf("a local-domain forwarder must not be enqueued external; got %d", got)
	}
	// The sender's own Sent copy still fires (alice sent the message).
	if got := len(caller.submitInboundCalls); got != 1 {
		t.Errorf("expected 1 submit_inbound_mail (sender Sent copy); got %d", got)
	}
}

// TestSubmissionForwarderNoMessageIDFallsBackToQueueID: a forwarder submission
// with no source Message-ID still forwards with a non-empty original_msgid (nest
// rejects an empty one), via the fresh queue-id fallback.
func TestSubmissionForwarderNoMessageIDFallsBackToQueueID(t *testing.T) {
	t.Parallel()
	admin := adminActor()
	caller := &submissionAuthCaller{
		resolveForwarders: map[string]submissionForwarderDecision{
			"info": {target: "oldaccount@example.net", admin: admin},
		},
		mlsPubkey:   freshRecipientPubkey32(t),
		indexPubkey: freshRecipientPubkey32(t),
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	if err := sess.Mail("alice@example.com", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("MAIL FROM: %v", err)
	}
	if err := sess.Rcpt("info@example.com", &gosmtp.RcptOptions{}); err != nil {
		t.Fatalf("RCPT info: %v", err)
	}
	// No Message-ID header.
	err := sess.Data(strings.NewReader(
		"From: alice@example.com\r\nTo: info@example.com\r\nSubject: no msgid\r\nDate: Mon, 1 Jan 2024 00:00:00 +0000\r\n\r\nbody\r\n"))
	if err != nil {
		t.Fatalf("expected 250 OK; got %v", err)
	}
	if got := len(caller.forwardRequests); got != 1 {
		t.Fatalf("forward count: got %d, want 1", got)
	}
	if got := caller.forwardRequests[0].originalMsgID; got == "" {
		t.Errorf("original_msgid must fall back to a non-empty queue id; got empty")
	}
}

// TestSubmissionForwarderRefusedTempfailsBeforeLocalCopies: a forwarder keeps
// no local copy, so a forward nest did not durably enqueue must fail the
// submission with a 451 rather than a 250 that loses the message for that
// recipient (mail-forwarding.md § Queue ceiling). DATA's one reply covers
// every recipient, so the forward is decided BEFORE any local copy — the
// local recipient's and the sender's own Sent copy — or the MUA's resend
// would duplicate them.
func TestSubmissionForwarderRefusedTempfailsBeforeLocalCopies(t *testing.T) {
	t.Parallel()
	admin := adminActor()
	bob := bytes.Repeat([]byte{0xBB}, 32)
	caller := &submissionAuthCaller{
		resolveForwarders: map[string]submissionForwarderDecision{
			"info": {target: "oldaccount@example.net", admin: admin},
		},
		validateRecipientByLocal: map[string][]byte{"bob": bob},
		mlsPubkey:                freshRecipientPubkey32(t),
		indexPubkey:              freshRecipientPubkey32(t),
		forwardMessageErr:        errForwardRefused,
	}
	sess := newAuthedSubmissionSession(t, caller, 100)
	if err := sess.Mail("alice@example.com", &gosmtp.MailOptions{}); err != nil {
		t.Fatalf("MAIL FROM: %v", err)
	}
	for _, rcpt := range []string{"bob@example.com", "info@example.com"} {
		if err := sess.Rcpt(rcpt, &gosmtp.RcptOptions{}); err != nil {
			t.Fatalf("RCPT %s: %v", rcpt, err)
		}
	}
	err := sess.Data(strings.NewReader(
		"From: alice@example.com\r\nTo: bob@example.com, info@example.com\r\nMessage-ID: <fwd-refused@example.com>\r\nSubject: hello\r\nDate: Mon, 1 Jan 2024 00:00:00 +0000\r\n\r\nbody\r\n"))
	wantTempfail451(t, err)
	if got := len(caller.forwardRequests); got != 1 {
		t.Fatalf("forward attempts: got %d, want 1", got)
	}
	if got := len(caller.ingestInboundCalls); got != 0 {
		t.Errorf("no local recipient copy before the forwarder's 451; got %d", got)
	}
	if got := len(caller.submitInboundCalls); got != 0 {
		t.Errorf("no Sent copy before the forwarder's 451; got %d", got)
	}
}
