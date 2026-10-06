package imap

import (
	"errors"
	"strings"
	"testing"

	"github.com/emersion/go-imap/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// fetchSectionBytes drives a single-row body-axis FETCH for `section` against
// `plainTextRFC5322` (decrypted via the stub opener) and returns the bytes the
// MDA wrote into that section's literal. Exercises the real shared-Rust
// extractor through the FFI — the Go↔Rust seam, not a stub.
func fetchSectionBytes(t *testing.T, section *imap.FetchItemBodySection) []byte {
	t.Helper()
	return fetchSectionBytesOf(t, plainTextRFC5322, section)
}

// fetchSectionBytesOf is fetchSectionBytes with an explicit decrypted
// plaintext, for multipart / message/rfc822 fixtures that exercise the
// numbered-part recursion (RFC 9051 §6.4.5).
func fetchSectionBytesOf(t *testing.T, plaintext string, section *imap.FetchItemBodySection) []byte {
	t.Helper()
	mid := bytes32x(0xd0)
	ct := sealedEnvelopeFixture(t, []byte("payload-sect"))
	caller := &bodyFetchCaller{
		metaReplies: []wsrpc.MessageMeta{
			{UID: 5, MessageID: mid[:], Modseq: 1, Flags: []string{}, InternalDate: 1_700_000_500, CiphertextSize: uint32(len(ct))},
		},
		cipherByID: map[string][]byte{string(mid[:]): ct},
	}
	dec := &stubDecryptor{mapping: map[string][]byte{string(ct): []byte(plaintext)}}
	s := &Session{
		client: caller, actorID: bytes32x(0x55), selectedMailbox: "INBOX",
		selectedUIDValidity: 100, cache: newBodyStructureCache(4),
	}
	uidSet := imap.UIDSet{}
	uidSet.AddNum(5)
	w := &bodyFetchWriter{}
	if err := s.fetchWithDecryptor(w, uidSet, &imap.FetchOptions{
		UID:         true,
		BodySection: []*imap.FetchItemBodySection{section},
	}, dec); err != nil {
		t.Fatalf("fetch: %v", err)
	}
	if len(w.rows) != 1 || len(w.rows[0].bodySections) != 1 {
		t.Fatalf("expected one row with one body section; rows=%d", len(w.rows))
	}
	bs := w.rows[0].bodySections[0]
	if !bs.closed {
		t.Errorf("section writer must be Closed before the next data item")
	}
	if bs.declared != int64(bs.written.Len()) {
		t.Errorf("declared literal size %d != written %d", bs.declared, bs.written.Len())
	}
	return bs.written.Bytes()
}

func TestFetchBodyHeaderSection(t *testing.T) {
	// BODY[HEADER] is the full header block including the terminating blank
	// line (everything up to and including the first CRLFCRLF).
	got := fetchSectionBytes(t, &imap.FetchItemBodySection{Specifier: imap.PartSpecifierHeader})
	idx := strings.Index(plainTextRFC5322, "\r\n\r\n")
	want := plainTextRFC5322[:idx+4]
	if string(got) != want {
		t.Errorf("BODY[HEADER] mismatch:\n want %q\n  got %q", want, string(got))
	}
}

func TestFetchBodyTextSection(t *testing.T) {
	// BODY[TEXT] is everything after the blank line.
	got := fetchSectionBytes(t, &imap.FetchItemBodySection{Specifier: imap.PartSpecifierText})
	if string(got) != "hello body\r\n" {
		t.Errorf("BODY[TEXT] = %q, want %q", string(got), "hello body\r\n")
	}
}

func TestFetchBodyHeaderAndTextReconstructWhole(t *testing.T) {
	hdr := fetchSectionBytes(t, &imap.FetchItemBodySection{Specifier: imap.PartSpecifierHeader})
	txt := fetchSectionBytes(t, &imap.FetchItemBodySection{Specifier: imap.PartSpecifierText})
	if string(hdr)+string(txt) != plainTextRFC5322 {
		t.Errorf("HEADER ++ TEXT must reconstruct the whole message")
	}
}

func TestFetchBodyHeaderFieldsMessageOrder(t *testing.T) {
	// Request order [Message-ID, Subject] differs from message order
	// [Subject, …, Message-ID]; output must follow MESSAGE order (matching
	// the go-imap reference + Dovecot), not request order.
	got := fetchSectionBytes(t, &imap.FetchItemBodySection{
		Specifier:    imap.PartSpecifierHeader,
		HeaderFields: []string{"Message-ID", "Subject"},
	})
	want := "Subject: hello\r\nMessage-ID: <abc@example.com>\r\n\r\n"
	if string(got) != want {
		t.Errorf("BODY[HEADER.FIELDS] mismatch:\n want %q\n  got %q", want, string(got))
	}
}

func TestFetchBodyHeaderFieldsCaseInsensitiveAndMissingSkipped(t *testing.T) {
	got := fetchSectionBytes(t, &imap.FetchItemBodySection{
		Specifier:    imap.PartSpecifierHeader,
		HeaderFields: []string{"subject", "x-not-present"},
	})
	want := "Subject: hello\r\n\r\n"
	if string(got) != want {
		t.Errorf("BODY[HEADER.FIELDS (subject x-not-present)] = %q, want %q", string(got), want)
	}
}

func TestFetchBodyHeaderFieldsNot(t *testing.T) {
	got := fetchSectionBytes(t, &imap.FetchItemBodySection{
		Specifier:       imap.PartSpecifierHeader,
		HeaderFieldsNot: []string{"From", "To", "Date", "Message-ID", "Content-Type"},
	})
	want := "Subject: hello\r\n\r\n"
	if string(got) != want {
		t.Errorf("BODY[HEADER.FIELDS.NOT] = %q, want %q", string(got), want)
	}
}

func TestFetchBodyPartialSubstring(t *testing.T) {
	got := fetchSectionBytes(t, &imap.FetchItemBodySection{
		Specifier: imap.PartSpecifierText,
		Partial:   &imap.SectionPartial{Offset: 0, Size: 5},
	})
	if string(got) != "hello" {
		t.Errorf("BODY[TEXT]<0.5> = %q, want %q", string(got), "hello")
	}
}

// ---- Numbered MIME parts (RFC 9051 §6.4.5 part numbering) ----

// multipartPlaintext: multipart/mixed > [ text/plain, message/rfc822 ] where
// the encapsulated message is itself multipart/alternative.
const multipartPlaintext = "" +
	"From: alice@example.com\r\n" +
	"Subject: cover\r\n" +
	"Content-Type: multipart/mixed; boundary=BB\r\n" +
	"\r\n" +
	"--BB\r\n" +
	"Content-Type: text/plain\r\n" +
	"\r\n" +
	"first body\r\n" +
	"--BB\r\n" +
	"Content-Type: message/rfc822\r\n" +
	"\r\n" +
	"Subject: inner subject\r\n" +
	"From: inner@example.com\r\n" +
	"\r\n" +
	"inner body line\r\n" +
	"--BB--\r\n"

func TestFetchNumberedPartLeafBody(t *testing.T) {
	// BODY[1] = the leaf part's contents (body), without its MIME header.
	got := fetchSectionBytesOf(t, multipartPlaintext, &imap.FetchItemBodySection{Part: []int{1}})
	if string(got) != "first body" {
		t.Errorf("BODY[1] = %q, want %q", string(got), "first body")
	}
}

func TestFetchNumberedPartMIMEHeader(t *testing.T) {
	// BODY[1.MIME] = the part's own MIME header block (incl. blank line).
	got := fetchSectionBytesOf(t, multipartPlaintext, &imap.FetchItemBodySection{
		Part:      []int{1},
		Specifier: imap.PartSpecifierMIME,
	})
	if string(got) != "Content-Type: text/plain\r\n\r\n" {
		t.Errorf("BODY[1.MIME] = %q", string(got))
	}
}

func TestFetchMessageRFC822PartWholeAndInner(t *testing.T) {
	// BODY[2] = the whole encapsulated message (header + body); the CRLF before
	// the outer --BB-- delimiter belongs to the boundary, so no trailing CRLF.
	whole := fetchSectionBytesOf(t, multipartPlaintext, &imap.FetchItemBodySection{Part: []int{2}})
	wantWhole := "Subject: inner subject\r\nFrom: inner@example.com\r\n\r\ninner body line"
	if string(whole) != wantWhole {
		t.Errorf("BODY[2] = %q, want %q", string(whole), wantWhole)
	}
	// BODY[2.MIME] = the message/rfc822 part's own MIME header, not the inner.
	mimeHdr := fetchSectionBytesOf(t, multipartPlaintext, &imap.FetchItemBodySection{
		Part:      []int{2},
		Specifier: imap.PartSpecifierMIME,
	})
	if string(mimeHdr) != "Content-Type: message/rfc822\r\n\r\n" {
		t.Errorf("BODY[2.MIME] = %q", string(mimeHdr))
	}
	// BODY[2.HEADER] opens the encapsulated message and returns its header.
	innerHdr := fetchSectionBytesOf(t, multipartPlaintext, &imap.FetchItemBodySection{
		Part:      []int{2},
		Specifier: imap.PartSpecifierHeader,
	})
	if string(innerHdr) != "Subject: inner subject\r\nFrom: inner@example.com\r\n\r\n" {
		t.Errorf("BODY[2.HEADER] = %q", string(innerHdr))
	}
	// BODY[2.TEXT] = the encapsulated message's body.
	innerText := fetchSectionBytesOf(t, multipartPlaintext, &imap.FetchItemBodySection{
		Part:      []int{2},
		Specifier: imap.PartSpecifierText,
	})
	if string(innerText) != "inner body line" {
		t.Errorf("BODY[2.TEXT] = %q", string(innerText))
	}
	// BODY[2.HEADER.FIELDS (Subject)] selects from the inner header.
	innerField := fetchSectionBytesOf(t, multipartPlaintext, &imap.FetchItemBodySection{
		Part:         []int{2},
		Specifier:    imap.PartSpecifierHeader,
		HeaderFields: []string{"Subject"},
	})
	if string(innerField) != "Subject: inner subject\r\n\r\n" {
		t.Errorf("BODY[2.HEADER.FIELDS (Subject)] = %q", string(innerField))
	}
}

func TestRejectTopLevelMIMEServesEverythingElse(t *testing.T) {
	// Top-level BODY[MIME] (no part) is rejected — MIME needs a numbered part.
	if err := rejectSectionedBodyFetches(&imap.FetchOptions{
		BodySection: []*imap.FetchItemBodySection{{Specifier: imap.PartSpecifierMIME}},
	}); err == nil {
		t.Error("top-level BODY[MIME] must be rejected")
	}
	// A numbered-part section is NOT rejected (served).
	if err := rejectSectionedBodyFetches(&imap.FetchOptions{
		BodySection: []*imap.FetchItemBodySection{{Part: []int{1}}},
	}); err != nil {
		t.Errorf("numbered-part BODY[1] must be served, got reject: %v", err)
	}
	if err := rejectSectionedBodyFetches(&imap.FetchOptions{
		BodySection: []*imap.FetchItemBodySection{{Part: []int{2}, Specifier: imap.PartSpecifierMIME}},
	}); err != nil {
		t.Errorf("numbered-part BODY[2.MIME] must be served, got reject: %v", err)
	}
	// BINARY[…] / BINARY.SIZE[…] are now served (RFC 3516; emit + CTE-decode
	// happen in fetchOne, so the gate must not pre-reject them).
	if err := rejectSectionedBodyFetches(&imap.FetchOptions{
		BinarySection: []*imap.FetchItemBinarySection{{Part: []int{1}}},
	}); err != nil {
		t.Errorf("BINARY[1] must be served, got reject: %v", err)
	}
	if err := rejectSectionedBodyFetches(&imap.FetchOptions{
		BinarySectionSize: []*imap.FetchItemBinarySectionSize{{Part: []int{1}}},
	}); err != nil {
		t.Errorf("BINARY.SIZE[1] must be served, got reject: %v", err)
	}
}

// ---- BINARY[…] / BINARY.SIZE[…] (RFC 3516 / RFC 9051 §6.4.5) ----

// binMultipart: multipart/mixed > [ octet-stream base64 "Zm9vYmFy"→"foobar",
// text/plain quoted-printable "caf=C3=A9"→b"caf\xc3\xa9" ]. Exercises the
// CTE-decode (no charset conversion) through the real shared-Rust extractor.
const binMultipart = "" +
	"From: a@x\r\n" +
	"Content-Type: multipart/mixed; boundary=BB\r\n" +
	"\r\n" +
	"--BB\r\n" +
	"Content-Type: application/octet-stream\r\n" +
	"Content-Transfer-Encoding: base64\r\n" +
	"\r\n" +
	"Zm9vYmFy\r\n" +
	"--BB\r\n" +
	"Content-Type: text/plain\r\n" +
	"Content-Transfer-Encoding: quoted-printable\r\n" +
	"\r\n" +
	"caf=C3=A9\r\n" +
	"--BB--\r\n"

// fetchBinaryOf drives a single-row body-axis FETCH carrying the given BINARY
// items against `plaintext` (decrypted via the stub opener) and returns the
// recording row plus the fetch error. Exercises the real shared-Rust binary
// extractor through the FFI — the Go↔Rust seam, not a stub.
func fetchBinaryOf(t *testing.T, plaintext string, opts *imap.FetchOptions) (*bodyFetchWriter, error) {
	t.Helper()
	mid := bytes32x(0xd1)
	ct := sealedEnvelopeFixture(t, []byte("payload-bin"))
	caller := &bodyFetchCaller{
		metaReplies: []wsrpc.MessageMeta{
			{UID: 5, MessageID: mid[:], Modseq: 1, Flags: []string{}, InternalDate: 1_700_000_500, CiphertextSize: uint32(len(ct))},
		},
		cipherByID: map[string][]byte{string(mid[:]): ct},
	}
	dec := &stubDecryptor{mapping: map[string][]byte{string(ct): []byte(plaintext)}}
	s := &Session{
		client: caller, actorID: bytes32x(0x55), selectedMailbox: "INBOX",
		selectedUIDValidity: 100, cache: newBodyStructureCache(4),
	}
	uidSet := imap.UIDSet{}
	uidSet.AddNum(5)
	opts.UID = true
	w := &bodyFetchWriter{}
	err := s.fetchWithDecryptor(w, uidSet, opts, dec)
	if err != nil {
		return w, err
	}
	if len(w.rows) != 1 {
		t.Fatalf("expected exactly one row, got %d", len(w.rows))
	}
	return w, nil
}

func TestFetchBinarySectionDecodesCTEOnly(t *testing.T) {
	// BINARY[1] = base64-decoded part 1 ("foobar"); the declared literal8 size
	// must equal the bytes written, and the writer must be Closed.
	w, err := fetchBinaryOf(t, binMultipart, &imap.FetchOptions{
		BinarySection: []*imap.FetchItemBinarySection{{Part: []int{1}}},
	})
	if err != nil {
		t.Fatalf("fetch: %v", err)
	}
	if len(w.rows[0].binSections) != 1 {
		t.Fatalf("expected one binary section, got %d", len(w.rows[0].binSections))
	}
	bs := w.rows[0].binSections[0]
	if !bs.closed {
		t.Error("binary section writer must be Closed before the next data item")
	}
	if bs.declared != int64(bs.written.Len()) {
		t.Errorf("declared literal8 size %d != written %d", bs.declared, bs.written.Len())
	}
	if got := bs.written.String(); got != "foobar" {
		t.Errorf("BINARY[1] = %q, want %q", got, "foobar")
	}
}

func TestFetchBinarySizeIsDecodedOctetCount(t *testing.T) {
	// BINARY.SIZE is the *decoded* count: base64 8-char → 6; QP → 5.
	w, err := fetchBinaryOf(t, binMultipart, &imap.FetchOptions{
		BinarySectionSize: []*imap.FetchItemBinarySectionSize{{Part: []int{1}}, {Part: []int{2}}},
	})
	if err != nil {
		t.Fatalf("fetch: %v", err)
	}
	if len(w.rows[0].binSizes) != 2 {
		t.Fatalf("expected two binary sizes, got %d", len(w.rows[0].binSizes))
	}
	if got := w.rows[0].binSizes[0].size; got != 6 {
		t.Errorf("BINARY.SIZE[1] = %d, want 6", got)
	}
	if got := w.rows[0].binSizes[1].size; got != 5 {
		t.Errorf("BINARY.SIZE[2] = %d, want 5", got)
	}
}

func TestFetchBinaryUnknownCTEMapsToResponseCode(t *testing.T) {
	const raw = "" +
		"Content-Type: application/octet-stream\r\n" +
		"Content-Transfer-Encoding: x-uuencode\r\n" +
		"\r\n" +
		"begin 644 x\r\n"
	_, err := fetchBinaryOf(t, raw, &imap.FetchOptions{
		BinarySection: []*imap.FetchItemBinarySection{{Part: []int{1}}},
	})
	if err == nil {
		t.Fatal("BINARY of an undecodable Content-Transfer-Encoding must error")
	}
	var ierr *imap.Error
	if !errors.As(err, &ierr) {
		t.Fatalf("want *imap.Error, got %T: %v", err, err)
	}
	if ierr.Code != imap.ResponseCodeUnknownCTE {
		t.Errorf("response code = %q, want %q", ierr.Code, imap.ResponseCodeUnknownCTE)
	}
}
