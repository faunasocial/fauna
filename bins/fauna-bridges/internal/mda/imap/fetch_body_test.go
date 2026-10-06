package imap

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// sealedEnvelopeFixture seals `pt` to a fresh throwaway X25519 leaf key and
// returns the shape-valid `MailRecordEnvelope` bytes — the only shape a mail
// record rests in. The throwaway secret is discarded: stub openers key on the
// envelope bytes and return the mapped plaintext themselves.
func sealedEnvelopeFixture(t *testing.T, pt []byte) []byte {
	t.Helper()
	leaf := faunaFfi.GenerateX25519Keypair()
	env, err := mailfauna.EncryptToRecipient(pt, leaf.Pubkey)
	if err != nil {
		t.Fatalf("sealedEnvelopeFixture: EncryptToRecipient: %v", err)
	}
	return env
}

// fixtureKey is one package-wide leaf key for fakes that serve mail records
// without a *testing.T (a fake Caller seals its plaintext fixtures at serve
// time with fixtureSeal; the test opens them with fixtureOpener). A mail
// record rests sealed, so a fake nest serves a real sealed envelope too.
var fixtureKey = sync.OnceValues(func() (*mailfauna.MailRecordOpener, []byte) {
	leaf := faunaFfi.GenerateX25519Keypair()
	snapshot, err := faunaFfi.EncodeMlsSnapshotPlaintextV1([]faunaFfi.X25519Keypair{leaf})
	if err != nil {
		panic(fmt.Sprintf("fixtureKey: EncodeMlsSnapshotPlaintextV1: %v", err))
	}
	opener, err := mailfauna.NewMailRecordOpener(snapshot)
	if err != nil {
		panic(fmt.Sprintf("fixtureKey: NewMailRecordOpener: %v", err))
	}
	return opener, leaf.Pubkey
})

// fixtureOpener is the real opener for everything fixtureSeal sealed.
func fixtureOpener() *mailfauna.MailRecordOpener {
	opener, _ := fixtureKey()
	return opener
}

// fixtureSeal seals `pt` to fixtureKey.
func fixtureSeal(pt []byte) []byte {
	_, pubkey := fixtureKey()
	env, err := mailfauna.EncryptToRecipient(pt, pubkey)
	if err != nil {
		panic(fmt.Sprintf("fixtureSeal: EncryptToRecipient: %v", err))
	}
	return env
}

// realOpenerFixture builds a REAL per-connection opener plus a sealer bound
// to its leaf key: `seal(pt)` produces envelopes the returned opener can
// actually HPKE-open (the production round-trip, no stub). Used by the
// three-arm serve-rule tests.
func realOpenerFixture(t *testing.T) (*mailfauna.MailRecordOpener, func(pt []byte) []byte) {
	t.Helper()
	leaf := faunaFfi.GenerateX25519Keypair()
	snapshotPlaintext, err := faunaFfi.EncodeMlsSnapshotPlaintextV1(
		[]faunaFfi.X25519Keypair{leaf},
	)
	if err != nil {
		t.Fatalf("EncodeMlsSnapshotPlaintextV1: %v", err)
	}
	opener, err := mailfauna.NewMailRecordOpener(snapshotPlaintext)
	if err != nil {
		t.Fatalf("NewMailRecordOpener: %v", err)
	}
	t.Cleanup(opener.Zeroize)
	seal := func(pt []byte) []byte {
		env, err := mailfauna.EncryptToRecipient(pt, leaf.Pubkey)
		if err != nil {
			t.Fatalf("EncryptToRecipient: %v", err)
		}
		return env
	}
	return opener, seal
}

// bodyFetchCaller fakes both fetch_message_metadata and
// fetch_message_ciphertext on one Caller. The Phase C.6 body-fetch
// path issues both RPCs (metadata to resolve UID→message_id, then
// ciphertext per row); the fake records the call sequence so tests can
// assert cache hits don't repeat the ciphertext RPC.
type bodyFetchCaller struct {
	metaReplies []wsrpc.MessageMeta
	// cipherByID maps string(message_id) → encrypted_body bytes the
	// fake returns. Absent IDs reply with `outcome="not_found"`.
	cipherByID    map[string][]byte
	ciphertextHit int    // counter of fetch_message_ciphertext calls
	metaHit       int    // counter of fetch_message_metadata calls
	lastCipherMID []byte // captures the message_id of the last ciphertext call

	// store_flags capture: the implicit-\Seen pre-pass
	// issues one batched store_flags(+\Seen) for the
	// non-PEEK body-fetch UIDs that lack \Seen, before the per-row emit. The
	// fake echoes each requested UID with the union of its seeded flags + the
	// requested flags and a monotonically-bumped modseq, so a test can assert
	// the merge-back into the emitted FLAGS / MODSEQ.
	storeFlagsHit  int      // counter of store_flags calls
	lastStoreUIDs  []uint32 // UIDs of the last store_flags call
	lastStoreOp    string   // op of the last store_flags call ("add"/…)
	lastStoreFlags []string // flags of the last store_flags call
	storeModseqSeq int64    // bump source for the echoed StoreFlagsResultEntry.ModSeq
}

func (f *bodyFetchCaller) Call(_ context.Context, method string, body, reply any) error {
	enc, err := dagcbor.Marshal(body)
	if err != nil {
		return err
	}
	switch method {
	case wsrpc.MethodFetchMessageMetadata:
		f.metaHit++
		rep, err := dagcbor.Marshal(struct {
			Messages []wsrpc.MessageMeta `cbor:"messages"`
		}{Messages: f.metaReplies})
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)

	case wsrpc.MethodFetchMessageCiphertext:
		f.ciphertextHit++
		var bod struct {
			ActorID   []byte `cbor:"actor_id"`
			MessageID []byte `cbor:"message_id"`
		}
		_ = cbor.Unmarshal(enc, &bod)
		f.lastCipherMID = bod.MessageID
		key := string(bod.MessageID)
		blob, ok := f.cipherByID[key]
		if !ok {
			rep, _ := dagcbor.Marshal(map[string]any{"outcome": "not_found"})
			return cbor.Unmarshal(rep, reply)
		}
		rep, err := dagcbor.Marshal(map[string]any{
			"outcome":         "found",
			"encrypted_body":  blob,
			"ciphertext_size": uint32(len(blob)),
			"internal_date":   int64(1_700_000_000),
		})
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)

	case wsrpc.MethodStoreFlags:
		f.storeFlagsHit++
		var req struct {
			UIDs  []uint32 `cbor:"uids"`
			Op    string   `cbor:"op"`
			Flags []string `cbor:"flags"`
		}
		_ = cbor.Unmarshal(enc, &req)
		f.lastStoreUIDs = append([]uint32(nil), req.UIDs...)
		f.lastStoreOp = req.Op
		f.lastStoreFlags = append([]string(nil), req.Flags...)
		updated := make([]wsrpc.StoreFlagsResultEntry, 0, len(req.UIDs))
		for _, u := range req.UIDs {
			f.storeModseqSeq++
			updated = append(updated, wsrpc.StoreFlagsResultEntry{
				UID:    u,
				Flags:  unionFlags(f.metaFlagsForUID(u), req.Flags),
				ModSeq: 1_000 + f.storeModseqSeq,
			})
		}
		rep, err := dagcbor.Marshal(wsrpc.StoreFlagsReply{
			Updated:       updated,
			HighestModSeq: 1_000 + f.storeModseqSeq,
		})
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)

	default:
		return errors.New("bodyFetchCaller: unexpected " + method)
	}
}

// metaFlagsForUID returns the flags the fake seeded for UID u (the row's
// pre-STORE flag set), or nil if the UID isn't in metaReplies.
func (f *bodyFetchCaller) metaFlagsForUID(u uint32) []string {
	for _, m := range f.metaReplies {
		if m.UID == u {
			return m.Flags
		}
	}
	return nil
}

// unionFlags merges b into a, dropping duplicates, preserving a's order then
// appending the new entries from b. Mirrors nest's store_flags(op=add) result.
func unionFlags(a, b []string) []string {
	seen := make(map[string]struct{}, len(a)+len(b))
	out := make([]string, 0, len(a)+len(b))
	for _, f := range a {
		if _, ok := seen[f]; ok {
			continue
		}
		seen[f] = struct{}{}
		out = append(out, f)
	}
	for _, f := range b {
		if _, ok := seen[f]; ok {
			continue
		}
		seen[f] = struct{}{}
		out = append(out, f)
	}
	return out
}

// stubDecryptor is the body-open seam fake (`mailfauna.RecordOpener`): the
// body-fetch path calls `Open(envelope)` to recover the plaintext through
// the session's per-connection record opener. The stub uses a string-keyed
// lookup table so tests can map an opaque envelope bytestring to a
// deterministic plaintext. Fixture envelopes are real sealed envelopes built
// via sealedEnvelopeFixture — a mail record rests sealed.
type stubDecryptor struct {
	mapping     map[string][]byte
	hits        int
	lastEnv     []byte
	failOnHit   int
	failOnHitOK bool
	failErr     error
}

func (d *stubDecryptor) Open(envelope []byte) ([]byte, error) {
	d.hits++
	d.lastEnv = envelope
	if d.failOnHitOK && d.hits == d.failOnHit {
		return nil, d.failErr
	}
	pt, ok := d.mapping[string(envelope)]
	if !ok {
		return nil, errors.New("stubDecryptor: no mapping for envelope")
	}
	return pt, nil
}

// fakeBodyResponseWriter records every body-axis Write* the fetch
// implementation makes. Per the TODO body, this fake stays in its own
// file (fetch_body_test.go) so future drift on the metadata-only fake
// in fetch_test.go doesn't break C.6 coverage.
type fakeBodyResponseWriter struct {
	uid          imap.UID
	uidCalled    bool
	flagsCalled  bool
	flags        []imap.Flag
	modseq       uint64
	modseqCalled bool
	intCalled    bool
	internal     time.Time
	sizeCalled   bool
	rfcSize      int64
	bsCalled     bool
	bs           imap.BodyStructure
	envCalled    bool
	env          *imap.Envelope
	bodySections []*recordedBodySection
	binSections  []*recordedBinarySection
	binSizes     []*recordedBinarySize
	closed       bool
}

type recordedBodySection struct {
	section  *imap.FetchItemBodySection
	declared int64
	written  *bytes.Buffer
	closed   bool
}

func (s *recordedBodySection) Write(p []byte) (int, error) { return s.written.Write(p) }
func (s *recordedBodySection) Close() error                { s.closed = true; return nil }

// recordedBinarySection records one BINARY[…] emit: the declared literal8 size
// and the bytes the fetch path wrote into it.
type recordedBinarySection struct {
	section  *imap.FetchItemBinarySection
	declared int64
	written  *bytes.Buffer
	closed   bool
}

func (s *recordedBinarySection) Write(p []byte) (int, error) { return s.written.Write(p) }
func (s *recordedBinarySection) Close() error                { s.closed = true; return nil }

// recordedBinarySize records one BINARY.SIZE[…] emit (the decoded octet count).
type recordedBinarySize struct {
	section *imap.FetchItemBinarySectionSize
	size    uint32
}

func (w *fakeBodyResponseWriter) WriteUID(u imap.UID) { w.uid = u; w.uidCalled = true }
func (w *fakeBodyResponseWriter) WriteFlags(f []imap.Flag) {
	w.flags = f
	w.flagsCalled = true
}
func (w *fakeBodyResponseWriter) WriteModSeq(m uint64) { w.modseq = m; w.modseqCalled = true }
func (w *fakeBodyResponseWriter) WriteInternalDate(t time.Time) {
	w.internal = t
	w.intCalled = true
}
func (w *fakeBodyResponseWriter) WriteRFC822Size(n int64) {
	w.rfcSize = n
	w.sizeCalled = true
}
func (w *fakeBodyResponseWriter) WriteBodyStructure(bs imap.BodyStructure) {
	w.bs = bs
	w.bsCalled = true
}
func (w *fakeBodyResponseWriter) WriteEnvelope(env *imap.Envelope) {
	w.env = env
	w.envCalled = true
}
func (w *fakeBodyResponseWriter) WriteBodySection(section *imap.FetchItemBodySection, size int64) io.WriteCloser {
	r := &recordedBodySection{section: section, declared: size, written: &bytes.Buffer{}}
	w.bodySections = append(w.bodySections, r)
	return r
}
func (w *fakeBodyResponseWriter) WriteBinarySection(section *imap.FetchItemBinarySection, size int64) io.WriteCloser {
	r := &recordedBinarySection{section: section, declared: size, written: &bytes.Buffer{}}
	w.binSections = append(w.binSections, r)
	return r
}
func (w *fakeBodyResponseWriter) WriteBinarySectionSize(section *imap.FetchItemBinarySectionSize, size uint32) {
	w.binSizes = append(w.binSizes, &recordedBinarySize{section: section, size: size})
}
func (w *fakeBodyResponseWriter) Close() error { w.closed = true; return nil }

type bodyFetchWriter struct{ rows []*fakeBodyResponseWriter }

func (w *bodyFetchWriter) CreateMessage(uint32) fetchResponseWriter {
	r := &fakeBodyResponseWriter{}
	w.rows = append(w.rows, r)
	return r
}

func (w *bodyFetchWriter) WriteVanishedEarlier(imap.UIDSet) error { return nil }

// --- Tests ---

const plainTextRFC5322 = "" +
	"From: alice@example.com\r\n" +
	"To: bob@example.com\r\n" +
	"Subject: hello\r\n" +
	"Date: Mon, 14 May 2026 12:00:00 +0000\r\n" +
	"Message-ID: <abc@example.com>\r\n" +
	"Content-Type: text/plain; charset=utf-8\r\n" +
	"\r\n" +
	"hello body\r\n"

func TestFetchBodyRFC822DecryptsAndEmitsPlaintext(t *testing.T) {
	mid := bytes32x(0x77)
	ct := sealedEnvelopeFixture(t, []byte("payload-1"))
	caller := &bodyFetchCaller{
		metaReplies: []wsrpc.MessageMeta{
			{UID: 5, MessageID: mid[:], Modseq: 42, Flags: []string{}, InternalDate: 1_700_000_500, CiphertextSize: uint32(len(ct))},
		},
		cipherByID: map[string][]byte{string(mid[:]): ct},
	}
	dec := &stubDecryptor{mapping: map[string][]byte{string(ct): []byte(plainTextRFC5322)}}
	s := &Session{
		client:              caller,
		actorID:             bytes32x(0x55),
		selectedMailbox:     "INBOX",
		selectedUIDValidity: 100,
		cache:               newBodyStructureCache(4),
	}
	w := &bodyFetchWriter{}
	uidSet := imap.UIDSet{}
	uidSet.AddNum(5)
	if err := s.fetchWithDecryptor(w, uidSet, &imap.FetchOptions{
		UID:         true,
		BodySection: []*imap.FetchItemBodySection{{}}, // BODY[] only
	}, dec); err != nil {
		t.Fatalf("fetch: %v", err)
	}
	if len(w.rows) != 1 {
		t.Fatalf("rows: %d", len(w.rows))
	}
	row := w.rows[0]
	if !row.uidCalled || row.uid != 5 {
		t.Errorf("UID emitted: called=%v uid=%d", row.uidCalled, row.uid)
	}
	if len(row.bodySections) != 1 {
		t.Fatalf("BodySection emit count: %d", len(row.bodySections))
	}
	bs := row.bodySections[0]
	if bs.section.Part != nil || bs.section.Specifier != imap.PartSpecifierNone {
		t.Errorf("BODY[] section: %+v", bs.section)
	}
	if bs.written.String() != plainTextRFC5322 {
		t.Errorf("plaintext mismatch: got %q", bs.written.String())
	}
	if !bs.closed {
		t.Errorf("body section writer must be Closed before next item")
	}
	if dec.hits != 1 {
		t.Errorf("Decrypt call count: %d, want 1", dec.hits)
	}
}

func TestFetchBodyStructureCacheHit(t *testing.T) {
	mid := bytes32x(0x88)
	ct := sealedEnvelopeFixture(t, []byte("payload-2"))
	caller := &bodyFetchCaller{
		metaReplies: []wsrpc.MessageMeta{
			{UID: 5, MessageID: mid[:], Modseq: 42, Flags: []string{}, InternalDate: 1_700_000_500, CiphertextSize: uint32(len(ct))},
		},
		cipherByID: map[string][]byte{string(mid[:]): ct},
	}
	dec := &stubDecryptor{mapping: map[string][]byte{string(ct): []byte(plainTextRFC5322)}}
	cache := newBodyStructureCache(4)
	s := &Session{
		client:              caller,
		actorID:             bytes32x(0x55),
		selectedMailbox:     "INBOX",
		selectedUIDValidity: 100,
		cache:               cache,
	}
	uidSet := imap.UIDSet{}
	uidSet.AddNum(5)
	w1 := &bodyFetchWriter{}
	if err := s.fetchWithDecryptor(w1, uidSet, &imap.FetchOptions{
		UID:           true,
		BodyStructure: &imap.FetchItemBodyStructure{},
	}, dec); err != nil {
		t.Fatalf("first fetch: %v", err)
	}
	if caller.ciphertextHit != 1 {
		t.Fatalf("first call ciphertextHit: %d, want 1", caller.ciphertextHit)
	}
	if dec.hits != 1 {
		t.Fatalf("first call Decrypt hits: %d, want 1", dec.hits)
	}
	// Second BODYSTRUCTURE FETCH for the same (actor, mailbox,
	// uid_validity, uid) must hit the cache: zero new ciphertext RPCs,
	// zero new Decrypt calls.
	w2 := &bodyFetchWriter{}
	if err := s.fetchWithDecryptor(w2, uidSet, &imap.FetchOptions{
		UID:           true,
		BodyStructure: &imap.FetchItemBodyStructure{},
	}, dec); err != nil {
		t.Fatalf("second fetch: %v", err)
	}
	if caller.ciphertextHit != 1 {
		t.Errorf("second call must hit cache: ciphertextHit=%d, want 1", caller.ciphertextHit)
	}
	if dec.hits != 1 {
		t.Errorf("second call must hit cache: Decrypt hits=%d, want 1", dec.hits)
	}
	if !w2.rows[0].bsCalled {
		t.Errorf("BODYSTRUCTURE must be emitted from cache on the second call")
	}
}

func TestFetchEnvelopeAndBodyStructureBothEmittedFromOneDecrypt(t *testing.T) {
	mid := bytes32x(0x99)
	ct := sealedEnvelopeFixture(t, []byte("payload-3"))
	caller := &bodyFetchCaller{
		metaReplies: []wsrpc.MessageMeta{
			{UID: 7, MessageID: mid[:], Modseq: 42, Flags: []string{}, InternalDate: 1_700_000_500, CiphertextSize: uint32(len(ct))},
		},
		cipherByID: map[string][]byte{string(mid[:]): ct},
	}
	dec := &stubDecryptor{mapping: map[string][]byte{string(ct): []byte(plainTextRFC5322)}}
	s := &Session{
		client: caller, actorID: bytes32x(0x55), selectedMailbox: "INBOX",
		selectedUIDValidity: 100, cache: newBodyStructureCache(4),
	}
	uidSet := imap.UIDSet{}
	uidSet.AddNum(7)
	w := &bodyFetchWriter{}
	if err := s.fetchWithDecryptor(w, uidSet, &imap.FetchOptions{
		UID:           true,
		BodyStructure: &imap.FetchItemBodyStructure{},
		Envelope:      true,
	}, dec); err != nil {
		t.Fatalf("fetch: %v", err)
	}
	row := w.rows[0]
	if !row.bsCalled || !row.envCalled {
		t.Errorf("both BS and Envelope must be emitted: bs=%v env=%v", row.bsCalled, row.envCalled)
	}
	if dec.hits != 1 {
		t.Errorf("Decrypt must fire once for combined BS+Envelope: hits=%d", dec.hits)
	}
	// Envelope subject must round-trip from the plaintext.
	if row.env == nil || row.env.Subject != "hello" {
		t.Errorf("envelope subject: %+v", row.env)
	}
}

func TestFetchEvictedOnNewUIDValidity(t *testing.T) {
	mid := bytes32x(0xaa)
	ct := sealedEnvelopeFixture(t, []byte("payload-4"))
	caller := &bodyFetchCaller{
		metaReplies: []wsrpc.MessageMeta{
			{UID: 5, MessageID: mid[:], Modseq: 1, Flags: []string{}, InternalDate: 1_700_000_500, CiphertextSize: uint32(len(ct))},
		},
		cipherByID: map[string][]byte{string(mid[:]): ct},
	}
	dec := &stubDecryptor{mapping: map[string][]byte{string(ct): []byte(plainTextRFC5322)}}
	cache := newBodyStructureCache(4)
	s := &Session{
		client: caller, actorID: bytes32x(0x55), selectedMailbox: "INBOX",
		selectedUIDValidity: 100, cache: cache,
	}
	uidSet := imap.UIDSet{}
	uidSet.AddNum(5)
	if err := s.fetchWithDecryptor(&bodyFetchWriter{}, uidSet, &imap.FetchOptions{
		UID:           true,
		BodyStructure: &imap.FetchItemBodyStructure{},
	}, dec); err != nil {
		t.Fatalf("first fetch: %v", err)
	}
	// Simulate a re-SELECT with a different UIDVALIDITY: cache MUST
	// miss because uid_validity is part of the key.
	s.selectedUIDValidity = 200
	if err := s.fetchWithDecryptor(&bodyFetchWriter{}, uidSet, &imap.FetchOptions{
		UID:           true,
		BodyStructure: &imap.FetchItemBodyStructure{},
	}, dec); err != nil {
		t.Fatalf("second fetch: %v", err)
	}
	if caller.ciphertextHit != 2 || dec.hits != 2 {
		t.Errorf("uid_validity change must invalidate cache: ciphertextHit=%d dec.hits=%d",
			caller.ciphertextHit, dec.hits)
	}
}

func TestFetchNumberedPartNonMultipartServesBody(t *testing.T) {
	// Numbered-part BODY[N…] addressing is now served (RFC 9051 §6.4.5). On a
	// non-multipart message, BODY[1] refers to the message itself — its body.
	// (Multipart + message/rfc822 recursion is covered in fetch_section_test.go.)
	got := fetchSectionBytes(t, &imap.FetchItemBodySection{Part: []int{1}})
	if string(got) != "hello body\r\n" {
		t.Errorf("BODY[1] on a non-multipart message = %q, want %q", string(got), "hello body\r\n")
	}
}

func TestFetchMissingCiphertextSkipsRow(t *testing.T) {
	// UID resolves to a message_id that nest reports as not_found
	// (expunged between metadata-fetch and ciphertext-fetch). The
	// FETCH must skip the row without erroring the whole command.
	mid := bytes32x(0xcc)
	caller := &bodyFetchCaller{
		metaReplies: []wsrpc.MessageMeta{
			{UID: 5, MessageID: mid[:], Modseq: 1, Flags: []string{}, InternalDate: 1_700_000_500, CiphertextSize: 0},
		},
		cipherByID: map[string][]byte{}, // empty → fetch_message_ciphertext returns not_found
	}
	dec := &stubDecryptor{mapping: map[string][]byte{}}
	s := &Session{
		client: caller, actorID: bytes32x(0x55), selectedMailbox: "INBOX",
		selectedUIDValidity: 100, cache: newBodyStructureCache(4),
	}
	uidSet := imap.UIDSet{}
	uidSet.AddNum(5)
	w := &bodyFetchWriter{}
	if err := s.fetchWithDecryptor(w, uidSet, &imap.FetchOptions{
		UID:           true,
		BodyStructure: &imap.FetchItemBodyStructure{},
	}, dec); err != nil {
		t.Fatalf("not-found row should skip silently, got err: %v", err)
	}
	for _, r := range w.rows {
		if r.bsCalled {
			t.Errorf("skipped UID must not emit BodyStructure")
		}
	}
}

// ── The uniform serve rule (three arms) ──
//
// One serve path (design D1): (a) a sealed record HPKE-opens via the
// per-connection opener; (b) an unsealed payload is refused — never served
// verbatim; (c) a sealed record the opener cannot open (wrong key) ERRORS.

// fetchBodyOnce drives a single-UID BODY[] fetch through fetchWithDecryptor
// with `stored` as the nest-served record and returns (emitted body, error).
func fetchBodyOnce(t *testing.T, s *Session, stored []byte, opener mailfauna.RecordOpener) (string, error) {
	t.Helper()
	mid := bytes32x(0x66)
	caller := &bodyFetchCaller{
		metaReplies: []wsrpc.MessageMeta{
			{UID: 5, MessageID: mid[:], Modseq: 42, Flags: []string{}, InternalDate: 1_700_000_500, CiphertextSize: uint32(len(stored))},
		},
		cipherByID: map[string][]byte{string(mid[:]): stored},
	}
	s.client = caller
	s.actorID = bytes32x(0x55)
	s.selectedMailbox = "INBOX"
	s.selectedUIDValidity = 100
	s.cache = newBodyStructureCache(4)
	w := &bodyFetchWriter{}
	uidSet := imap.UIDSet{}
	uidSet.AddNum(5)
	err := s.fetchWithDecryptor(w, uidSet, &imap.FetchOptions{
		UID:         true,
		BodySection: []*imap.FetchItemBodySection{{Peek: true}},
	}, opener)
	if err != nil {
		return "", err
	}
	if len(w.rows) != 1 || len(w.rows[0].bodySections) != 1 {
		t.Fatalf("expected one row with one body section, rows=%d", len(w.rows))
	}
	return w.rows[0].bodySections[0].written.String(), nil
}

// (a) A sealed record opens via a REAL opener. The storage mode is no longer
// an input to the serve path AT ALL — the Session carries no mode bit (the
// field itself is deleted, the structural pin of Phase-3 D1).
func TestFetchSealedRecordOpensRegardlessOfStorageMode(t *testing.T) {
	opener, seal := realOpenerFixture(t)
	sealed := seal([]byte(plainTextRFC5322))
	s := &Session{}
	got, err := fetchBodyOnce(t, s, sealed, opener)
	if err != nil {
		t.Fatalf("sealed record must open: %v", err)
	}
	if got != plainTextRFC5322 {
		t.Errorf("sealed body mismatch: got %q", got)
	}
}

// (b) An unsealed payload (plain RFC 5322 bytes) is refused — with a real
// opener and with none. Every mail record rests sealed, so raw bytes are
// corruption, never a record to serve verbatim.
func TestFetchRawRecordIsRefused(t *testing.T) {
	opener, _ := realOpenerFixture(t)
	if got, err := fetchBodyOnce(t, &Session{}, []byte(plainTextRFC5322), opener); err == nil {
		t.Fatalf("raw record must be refused, got served %q", got)
	}
	if got, err := fetchBodyOnce(t, &Session{}, []byte(plainTextRFC5322), nil); err == nil {
		t.Fatalf("raw record with no opener must be refused, got served %q", got)
	}
}

// (c) A sealed record the session's opener CANNOT open (sealed to a
// different leaf key) errors — never served verbatim.
func TestFetchSealedRecordWrongKeyErrors(t *testing.T) {
	opener, _ := realOpenerFixture(t)
	wrongKeySealed := sealedEnvelopeFixture(t, []byte(plainTextRFC5322)) // throwaway key ≠ opener's leaf
	s := &Session{}
	if _, err := fetchBodyOnce(t, s, wrongKeySealed, opener); err == nil {
		t.Fatalf("wrong-key sealed record must error, not serve")
	}
	// And with no opener at all, a sealed record surfaces the
	// missing-snapshot error.
	s = &Session{}
	if _, err := fetchBodyOnce(t, s, sealedEnvelopeFixture(t, []byte("x")), nil); err == nil ||
		!strings.Contains(err.Error(), "no MLS snapshot") {
		t.Fatalf("sealed record with no opener must surface the missing-snapshot error, got %v", err)
	}
}
