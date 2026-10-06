package mailfauna

import (
	"fmt"
	"testing"
)

// A complete RFC 3464 report: human-readable notice, delivery-status, and the
// returned message. The third part is what carries the correlation the nest
// authorizes on — the original Message-ID.
const genuineDSN = "From: Mail Delivery System <>\r\n" +
	"To: kid@fauna.test\r\n" +
	"Subject: Undelivered Mail Returned to Sender\r\n" +
	"Content-Type: multipart/report; report-type=delivery-status; boundary=\"BNDR\"\r\n" +
	"\r\n" +
	"--BNDR\r\n" +
	"Content-Type: text/plain\r\n" +
	"\r\n" +
	"Your message could not be delivered.\r\n" +
	"--BNDR\r\n" +
	"Content-Type: message/delivery-status\r\n" +
	"\r\n" +
	"Reporting-MTA: dns; mx.remote.test\r\n" +
	"\r\n" +
	"Original-Recipient: rfc822; X@Remote.Test\r\n" +
	"Final-Recipient: rfc822; x@remote.test\r\n" +
	"Action: failed\r\n" +
	"Status: 5.1.1\r\n" +
	"--BNDR\r\n" +
	"Content-Type: message/rfc822\r\n" +
	"\r\n" +
	"From: kid@fauna.test\r\n" +
	"To: x@remote.test\r\n" +
	"Message-ID: <7f3a9c2e@fauna.test>\r\n" +
	"Subject: hi\r\n" +
	"\r\n" +
	"the original body\r\n" +
	"--BNDR--\r\n"

func TestDsnCorrelationGenuineReport(t *testing.T) {
	got := DsnCorrelation([]byte(genuineDSN))
	if got.OriginalMsgID != "<7f3a9c2e@fauna.test>" {
		t.Errorf("OriginalMsgID = %q, want <7f3a9c2e@fauna.test>", got.OriginalMsgID)
	}
}

// RFC 3464 lets a reporting MTA return only the headers of the failed message.
// The Message-ID is in there, so the correlation still works.
func TestDsnCorrelationRfc822HeadersOnly(t *testing.T) {
	msg := "Content-Type: multipart/report; report-type=delivery-status; boundary=\"B\"\r\n" +
		"\r\n" +
		"--B\r\n" +
		"Content-Type: message/delivery-status\r\n" +
		"\r\n" +
		"Reporting-MTA: dns; mx.remote.test\r\n" +
		"\r\n" +
		"Final-Recipient: rfc822; x@remote.test\r\n" +
		"--B\r\n" +
		"Content-Type: text/rfc822-headers\r\n" +
		"\r\n" +
		"From: kid@fauna.test\r\n" +
		"Message-ID: <abc123@fauna.test>\r\n" +
		"--B--\r\n"
	got := DsnCorrelation([]byte(msg))
	if got.OriginalMsgID != "<abc123@fauna.test>" {
		t.Errorf("OriginalMsgID = %q, want <abc123@fauna.test>", got.OriginalMsgID)
	}
}

// A report that returns no message carries no correlation. The nest holds it —
// fail-closed, never over-deliver.
func TestDsnCorrelationNoReturnedMessageYieldsNoMsgID(t *testing.T) {
	got := DsnCorrelation([]byte("Content-Type: multipart/report; report-type=delivery-status; boundary=\"B\"\r\n" +
		"\r\n" +
		"--B\r\n" +
		"Content-Type: message/delivery-status\r\n" +
		"\r\n" +
		"Final-Recipient: rfc822; x@remote.test\r\n" +
		"--B--\r\n"))
	if got.OriginalMsgID != "" {
		t.Errorf("OriginalMsgID = %q, want empty", got.OriginalMsgID)
	}
}

// A DSN costume — the report-type parameter and even a returned-message part,
// but no actual message/delivery-status. It is not a report, so BOTH facts are
// void. NOTE this is a WELL-FORMEDNESS check, not a security boundary: an
// attacker composing the whole message trivially includes a delivery-status
// part too (three lines of MIME). The gate's security rests on Message-ID
// unguessability and the nest-side consumption budget, nothing else
// (family-safety.md § The mail gate).
func TestDsnCorrelationCostumeWithoutStatusPart(t *testing.T) {
	msg := "Content-Type: multipart/report; report-type=delivery-status; boundary=\"B\"\r\n" +
		"\r\n" +
		"--B\r\n" +
		"Content-Type: text/plain\r\n" +
		"\r\n" +
		"click here\r\n" +
		"--B\r\n" +
		"Content-Type: message/rfc822\r\n" +
		"\r\n" +
		"Message-ID: <stolen@fauna.test>\r\n" +
		"--B--\r\n"
	got := DsnCorrelation([]byte(msg))
	if got.OriginalMsgID != "" || got.ReplyAddresses != nil {
		t.Errorf("DsnCorrelation = %+v, want zero for a costume", got)
	}
}

func TestDsnCorrelationPlainMessage(t *testing.T) {
	msg := "From: someone@ex.com\r\n" +
		"Content-Type: text/plain\r\n" +
		"\r\n" +
		"hello\r\n"
	if got := (DsnCorrelation([]byte(msg))); got.OriginalMsgID != "" || got.ReplyAddresses != nil {
		t.Errorf("DsnCorrelation = %+v, want zero for plain mail", got)
	}
}

// The address-header set is what a one-click reply/reply-all to the report
// can be addressed to — the nest records it on a correlated delivery and
// declines to auto-seed the ward's allowlist for it (family-safety.md § The
// mail gate). The genuine fixture's `From:` is the null angle-addr (skipped
// as unparseable — a smaller set is only ever *less* deliverable); `To:` is
// the ward.
func TestDsnCorrelationReplyAddressesFromGenuineReport(t *testing.T) {
	got := DsnCorrelation([]byte(genuineDSN))
	if len(got.ReplyAddresses) != 1 || got.ReplyAddresses[0] != "kid@fauna.test" {
		t.Errorf("ReplyAddresses = %v, want [kid@fauna.test]", got.ReplyAddresses)
	}
}

// Every address header contributes (From, Sender, Reply-To, To, Cc),
// lowercased and deduplicated in header order — an attacker parking an
// accomplice in Cc: (what reply-all addresses) or swapping Reply-To (what
// reply addresses) must not escape the recorded set.
func TestDsnCorrelationReplyAddressesCollectAllHeaders(t *testing.T) {
	msg := "From: Carol <CAROL@Evil.Test>\r\n" +
		"Reply-To: drop@evil.test\r\n" +
		"To: kid@fauna.test\r\n" +
		"Cc: Accomplice <accomplice@evil.test>, carol@evil.test\r\n" +
		"Content-Type: multipart/report; report-type=delivery-status; boundary=\"B\"\r\n" +
		"\r\n" +
		"--B\r\n" +
		"Content-Type: message/delivery-status\r\n" +
		"\r\n" +
		"Final-Recipient: rfc822; x@remote.test\r\n" +
		"--B--\r\n"
	got := DsnCorrelation([]byte(msg))
	want := []string{"carol@evil.test", "drop@evil.test", "kid@fauna.test", "accomplice@evil.test"}
	if len(got.ReplyAddresses) != len(want) {
		t.Fatalf("ReplyAddresses = %v, want %v", got.ReplyAddresses, want)
	}
	for i, w := range want {
		if got.ReplyAddresses[i] != w {
			t.Errorf("ReplyAddresses[%d] = %q, want %q", i, got.ReplyAddresses[i], w)
		}
	}
}

// Truncated at one over the nest's plausibility bound: an over-stuffed list
// must still arrive over-stuffed (the nest holds it), never disguised as a
// plausible set by the truncation itself.
func TestDsnCorrelationReplyAddressesCapped(t *testing.T) {
	cc := ""
	for i := 0; i < 40; i++ {
		if i > 0 {
			cc += ", "
		}
		cc += fmt.Sprintf("cc%d@evil.test", i)
	}
	msg := "From: mailer-daemon@remote.test\r\n" +
		"Cc: " + cc + "\r\n" +
		"Content-Type: multipart/report; report-type=delivery-status; boundary=\"B\"\r\n" +
		"\r\n" +
		"--B\r\n" +
		"Content-Type: message/delivery-status\r\n" +
		"\r\n" +
		"Final-Recipient: rfc822; x@remote.test\r\n" +
		"--B--\r\n"
	got := DsnCorrelation([]byte(msg))
	if len(got.ReplyAddresses) != dsnReplyAddressCap {
		t.Errorf("len(ReplyAddresses) = %d, want the cap %d",
			len(got.ReplyAddresses), dsnReplyAddressCap)
	}
}

// A costume (no delivery-status part) yields no addresses either — the
// address set rides the same genuineness condition as the other two facts.
func TestDsnCorrelationCostumeHasNoReplyAddresses(t *testing.T) {
	msg := "From: carol@evil.test\r\n" +
		"Content-Type: multipart/report; report-type=delivery-status; boundary=\"B\"\r\n" +
		"\r\n" +
		"--B\r\n" +
		"Content-Type: text/plain\r\n" +
		"\r\n" +
		"click here\r\n" +
		"--B--\r\n"
	if got := DsnCorrelation([]byte(msg)); len(got.ReplyAddresses) != 0 {
		t.Errorf("ReplyAddresses = %v, want empty for a costume", got.ReplyAddresses)
	}
}

// Mail-Reply-To/Mail-Followup-To are one-click reply targets a mutt-class MUA
// honors ahead of From/Reply-To, so the recorded set must include them or the
// ward's reply routes to an un-recorded accomplice and the decline misses.
func TestDsnCorrelationReplyAddressesIncludesMailReplyHeaders(t *testing.T) {
	msg := "From: mailer-daemon@remote.test\r\n" +
		"Mail-Reply-To: accomplice@evil.test\r\n" +
		"Mail-Followup-To: list@evil.test, second@evil.test\r\n" +
		"Content-Type: multipart/report; report-type=delivery-status; boundary=\"B\"\r\n" +
		"\r\n" +
		"--B\r\n" +
		"Content-Type: message/delivery-status\r\n" +
		"\r\n" +
		"Final-Recipient: rfc822; x@remote.test\r\n" +
		"--B--\r\n"
	got := DsnCorrelation([]byte(msg))
	want := map[string]bool{
		"mailer-daemon@remote.test": true,
		"accomplice@evil.test":      true,
		"list@evil.test":            true,
		"second@evil.test":          true,
	}
	if len(got.ReplyAddresses) != len(want) {
		t.Fatalf("ReplyAddresses = %v, want %d addresses incl. the Mail-Reply-To/Mail-Followup-To ones",
			got.ReplyAddresses, len(want))
	}
	for _, a := range got.ReplyAddresses {
		if !want[a] {
			t.Errorf("unexpected recorded address %q", a)
		}
	}
}

// asciiLower must fold exactly what Rust's to_ascii_lowercase folds (the nest's
// normalize_mail_address): ASCII A–Z only, every non-ASCII byte preserved. If
// it folded non-ASCII uppercase, the recorded correlated-origin key would
// diverge from the outbound address the nest looks up and the decline could
// miss on an SMTPUTF8/EAI address.
func TestAsciiLowerMatchesNestNormalizer(t *testing.T) {
	cases := map[string]string{
		"CAROL@Evil.Test":  "carol@evil.test",
		"already@low.test": "already@low.test",
		// Ä (U+00C4, a non-ASCII uppercase) must survive unfolded; only the
		// ASCII M and the domain fold.
		"MÄX@X.test": "mÄx@x.test",
	}
	for in, want := range cases {
		if got := asciiLower(in); got != want {
			t.Errorf("asciiLower(%q) = %q, want %q", in, got, want)
		}
	}
}
