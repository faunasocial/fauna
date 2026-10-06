package mailfauna

import (
	"bytes"
	"testing"
)

const sampleRFC5322 = "From: alice@example.org\r\n" +
	"To: bob@fauna.example\r\n" +
	"Subject: smoke\r\n" +
	"Message-ID: <smoke@example.org>\r\n" +
	"Date: Mon, 14 May 2026 12:00:00 +0000\r\n" +
	"\r\n" +
	"hello fauna\r\n"

// sampleMultipartMIME is a text/plain + one attachment message. The
// boundary delimiters are deliberately minimal so the fixture stays
// readable inline; mail-parser tolerates the missing trailing CRLF on
// the closing boundary the same way the Rust-side fixture does.
const sampleMultipartMIME = "From: alice@example.org\r\n" +
	"To: bob@fauna.example\r\n" +
	"Subject: with attachment\r\n" +
	"Message-ID: <attach-001@example.org>\r\n" +
	"Date: Thu, 14 May 2026 12:00:00 +0000\r\n" +
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

func TestParseRFC5322RoundTrip(t *testing.T) {
	parsed, err := ParseRFC5322([]byte(sampleRFC5322))
	if err != nil {
		t.Fatalf("ParseRFC5322: %v", err)
	}
	// The shared Rust parser (mail-parser) strips the surrounding
	// angle brackets from the Message-ID header value.
	if parsed.MessageID != "smoke@example.org" {
		t.Fatalf("MessageID: want %q, got %q", "smoke@example.org", parsed.MessageID)
	}
	if parsed.Subject != "smoke" {
		t.Fatalf("Subject: want %q, got %q", "smoke", parsed.Subject)
	}
	if parsed.From != "alice@example.org" {
		t.Fatalf("From: want %q, got %q", "alice@example.org", parsed.From)
	}
	// Date header is "Mon, 14 May 2026 12:00:00 +0000" → unix seconds
	// 1778760000. mail-parser exposes this through the Date header
	// regardless of MIME structure.
	if parsed.DateUnixSeconds != 1778760000 {
		t.Fatalf("DateUnixSeconds: want %d, got %d", 1778760000, parsed.DateUnixSeconds)
	}
}

// TestParseRFC5322MultipartExposesAttachment is the C.4 RED test: the
// Go wrapper currently drops everything past BodyText; the multipart
// case below must surface the attachment via MimeParts so C.9's
// encrypt-to-recipient pipeline can carry attachment metadata
// (filename, content type, size) on the wire.
func TestParseRFC5322MultipartExposesAttachment(t *testing.T) {
	parsed, err := ParseRFC5322([]byte(sampleMultipartMIME))
	if err != nil {
		t.Fatalf("ParseRFC5322: %v", err)
	}
	if got := len(parsed.MimeParts); got < 2 {
		t.Fatalf("len(MimeParts): want >=2 (text + attachment), got %d", got)
	}
	var attach *ParsedMimePart
	for i := range parsed.MimeParts {
		if parsed.MimeParts[i].Disposition == "attachment" {
			attach = &parsed.MimeParts[i]
			break
		}
	}
	if attach == nil {
		t.Fatalf("no MIME part with Disposition==\"attachment\" found; parts: %+v", parsed.MimeParts)
	}
	if attach.Filename != "test.txt" {
		t.Fatalf("attachment Filename: want %q, got %q", "test.txt", attach.Filename)
	}
	if attach.ContentType != "text/plain" {
		t.Fatalf("attachment ContentType: want %q, got %q", "text/plain", attach.ContentType)
	}
	if attach.SizeBytes == 0 {
		t.Fatalf("attachment SizeBytes: want >0, got 0")
	}
}

// TestSenderDomainGoldenCorpus drives the lifted `from_norm` rule over the
// UniFFI binding with the SAME vectors as the Rust golden corpus
// (libs/fauna-mail/src/envelope.rs::the_golden_corpus_pins_the_lifted_from_norm_rule),
// so the Go wiring cannot drift from the shared implementation — the
// dedup_key golden-vector pattern. Semantics (multi-address-takes-first,
// group flattening, lenient header grammar, envelope fallback) are owned by
// the Rust side; this proves the FFI seam carries them.
func TestSenderDomainGoldenCorpus(t *testing.T) {
	cases := []struct {
		name     string
		raw      string
		mailFrom string
		want     string
	}{
		{"header wins over envelope, lowercased",
			"From: Alice <alice@Example.COM>\r\n\r\nbody", "bounce@env.test", "example.com"},
		{"multi-address From takes the first (delta 1)",
			"From: a@first.test, b@second.test\r\n\r\nbody", "bounce@env.test", "first.test"},
		{"group syntax is flattened (delta 1)",
			"From: Team:a@x.test,b@y.test;\r\n\r\nbody", "", "x.test"},
		{"no header falls back to the envelope",
			"Subject: no from\r\n\r\nbody", "bounce@env.test", "env.test"},
		{"angle-bracket envelope addr-spec, lowercased",
			"Subject: no from\r\n\r\nbody", "<bounce@ENV.test>", "env.test"},
		{"null reverse-path yields empty",
			"Subject: no from\r\n\r\nbody", "<>", ""},
		{"no envelope (APPEND) yields empty",
			"Subject: no from\r\n\r\nbody", "", ""},
		{"unparseable header falls back",
			"From: not-an-address\r\n\r\nbody", "bounce@env.test", "env.test"},
		{"empty header domain falls back",
			"From: <a@>\r\n\r\nbody", "bounce@env.test", "env.test"},
		{"empty local parts on both sides yield empty",
			"From: <@x.test>\r\n\r\nbody", "@y.test", ""},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := SenderDomainWithEnvelopeFallback([]byte(tc.raw), tc.mailFrom)
			if got != tc.want {
				t.Errorf("got %q, want %q", got, tc.want)
			}
		})
	}
}

// TestReportHashBinding smoke-tests the shared-Rust report-hash over the
// UniFFI binding (report-sharing.md § Content identity). Semantics (NFC,
// case-fold, whitespace collapse, domain separation) are pinned by the Rust
// unit tests in libs/fauna-mail/src/report_hash.rs — this proves the Go
// wiring: 32 bytes, deterministic, canonicalization-invariant, and sensitive
// to content.
func TestReportHashBinding(t *testing.T) {
	base := ReportHash("Win A PRIZE", "click here now")
	if len(base) != 32 {
		t.Fatalf("report hash length: want 32, got %d", len(base))
	}
	invariant := ReportHash(" win\ta  prize ", "CLICK\nHERE  NOW")
	if !bytes.Equal(base, invariant) {
		t.Fatalf("canonicalization-equivalent inputs must hash identically")
	}
	if bytes.Equal(base, ReportHash("Win A PRIZE", "different body")) {
		t.Fatalf("distinct content must hash differently")
	}
}

// TestMailDedupKeyBinding pins the cross-language contract: the key pair the
// Go bridges compute must be byte-identical to the one the Rust importer
// computes, or dedup silently stops working. The golden vector is derived from
// mailbox-migration.md § Dedup, independently of either implementation, and is
// asserted on both sides (libs/fauna-mail/src/dedup_key.rs
// `envelope_form_matches_the_specs_golden_vector`).
func TestMailDedupKeyBinding(t *testing.T) {
	const golden = "env:v1:ae6d5a8b9de5d0e09b958ea8b6ab5d023262dbf015d996cc247c004e4c5ea576"

	withID := []byte("From: a@x.test\r\nTo: b@y.test\r\nSubject: Hi\r\n" +
		"Message-ID: <ABC@Example.COM>\r\nDate: Mon, 1 Jan 2024 00:00:00 +0000\r\n\r\nbody text\r\n")
	keys := MailDedupKeys(withID)
	if keys.DedupKey != "msgid:v1:abc@example.com" {
		t.Fatalf("Message-ID form: want msgid:v1:abc@example.com, got %q", keys.DedupKey)
	}
	// The Message-ID is not an envelope field: the same golden vector binds
	// the envelope key of the message that carries one.
	if keys.EnvelopeKey != golden {
		t.Fatalf("envelope key must match the spec golden vector:\n want %s\n got  %s", golden, keys.EnvelopeKey)
	}

	noID := []byte("From: a@x.test\r\nTo: b@y.test\r\nSubject: Hi\r\n" +
		"Date: Mon, 1 Jan 2024 00:00:00 +0000\r\n\r\nbody text\r\n")
	keys = MailDedupKeys(noID)
	if keys.DedupKey != golden || keys.EnvelopeKey != golden {
		t.Fatalf("envelope form must match the spec golden vector:\n want %s\n got  %+v", golden, keys)
	}

	// A garbage message must still yield stable keys — never a panic, never an
	// error: the callers are mid-delivery and must not drop mail over dedup.
	if a, b := MailDedupKeys([]byte{0xff, 0xfe}), MailDedupKeys([]byte{0xff, 0xfe}); a != b {
		t.Fatalf("unparseable input must be deterministic")
	}
}
