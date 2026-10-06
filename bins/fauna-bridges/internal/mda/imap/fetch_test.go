package imap

import (
	"context"
	"errors"
	"io"
	"testing"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// fetchCaller stubs fetch_message_metadata replies and captures the
// outgoing UID list for assertion.
type fetchCaller struct {
	gotMethod string
	gotUIDs   []uint32
	gotMbox   string
	reply     wsrpc.MessageMeta // emit one — extend with replies for multi-row tests
	replies   []wsrpc.MessageMeta
	err       error
}

func (f *fetchCaller) Call(_ context.Context, method string, body, reply any) error {
	f.gotMethod = method
	if f.err != nil {
		return f.err
	}
	if method != wsrpc.MethodFetchMessageMetadata {
		return errors.New("fetchCaller: unexpected " + method)
	}
	enc, err := dagcbor.Marshal(body)
	if err != nil {
		return err
	}
	var bod struct {
		Mailbox string   `cbor:"mailbox"`
		Uids    []uint32 `cbor:"uids"`
	}
	_ = cbor.Unmarshal(enc, &bod)
	f.gotMbox = bod.Mailbox
	f.gotUIDs = bod.Uids
	rows := f.replies
	if rows == nil {
		rows = []wsrpc.MessageMeta{f.reply}
	}
	rep, err := dagcbor.Marshal(struct {
		Messages []wsrpc.MessageMeta `cbor:"messages"`
	}{Messages: rows})
	if err != nil {
		return err
	}
	return cbor.Unmarshal(rep, reply)
}

// fakeFetchResponseWriter records every Write* call; Close ends a row.
type fakeFetchResponseWriter struct {
	seqNum       uint32 // the sequence number CreateMessage was called with
	uid          imap.UID
	flagsWritten []imap.Flag
	flagsCalled  bool
	modSeq       uint64
	modSeqCalled bool
	internal     time.Time
	intCalled    bool
	rfcSize      int64
	sizeCalled   bool
	closed       bool
}

func (w *fakeFetchResponseWriter) WriteUID(u imap.UID)      { w.uid = u }
func (w *fakeFetchResponseWriter) WriteFlags(f []imap.Flag) { w.flagsWritten = f; w.flagsCalled = true }
func (w *fakeFetchResponseWriter) WriteModSeq(m uint64)     { w.modSeq = m; w.modSeqCalled = true }
func (w *fakeFetchResponseWriter) WriteInternalDate(t time.Time) {
	w.internal = t
	w.intCalled = true
}
func (w *fakeFetchResponseWriter) WriteRFC822Size(n int64) { w.rfcSize = n; w.sizeCalled = true }

// Body-axis methods are no-ops on this fake — metadata-only tests
// never invoke them. fetch_body_test.go ships its own fake that
// records body-axis calls.
func (w *fakeFetchResponseWriter) WriteBodyStructure(imap.BodyStructure) {}
func (w *fakeFetchResponseWriter) WriteEnvelope(*imap.Envelope)          {}
func (w *fakeFetchResponseWriter) WriteBodySection(*imap.FetchItemBodySection, int64) io.WriteCloser {
	return discardWriteCloser{}
}
func (w *fakeFetchResponseWriter) WriteBinarySection(*imap.FetchItemBinarySection, int64) io.WriteCloser {
	return discardWriteCloser{}
}
func (w *fakeFetchResponseWriter) WriteBinarySectionSize(*imap.FetchItemBinarySectionSize, uint32) {}

func (w *fakeFetchResponseWriter) Close() error { w.closed = true; return nil }

// discardWriteCloser is the metadata-test fake's WriteBodySection
// return: writes succeed and are discarded; Close is a no-op.
type discardWriteCloser struct{}

func (discardWriteCloser) Write(p []byte) (int, error) { return len(p), nil }
func (discardWriteCloser) Close() error                { return nil }

type fakeFetchWriter struct {
	rows            []*fakeFetchResponseWriter
	vanishedEarlier []imap.UIDSet // QRESYNC `* VANISHED (EARLIER) <set>` calls
}

func (w *fakeFetchWriter) CreateMessage(seqNum uint32) fetchResponseWriter {
	r := &fakeFetchResponseWriter{seqNum: seqNum}
	w.rows = append(w.rows, r)
	// stash seq via the UID slot pre-emptively so test assertions can
	// compare; production paths overwrite via WriteUID.
	r.uid = imap.UID(seqNum)
	return r
}

func (w *fakeFetchWriter) WriteVanishedEarlier(uids imap.UIDSet) error {
	w.vanishedEarlier = append(w.vanishedEarlier, uids)
	return nil
}

func TestFetchMetadataOnlyEmitsUIDFlagsInternalDate(t *testing.T) {
	mid := bytes32x(0x77)
	c := &fetchCaller{
		replies: []wsrpc.MessageMeta{
			{
				UID:            5,
				MessageID:      mid[:32],
				Modseq:         42,
				Flags:          []string{"\\Seen", "\\Flagged"},
				InternalDate:   1_700_000_500,
				CiphertextSize: 1024,
			},
			{
				UID:            6,
				MessageID:      mid[:32],
				Modseq:         43,
				Flags:          []string{},
				InternalDate:   1_700_000_600,
				CiphertextSize: 2048,
			},
		},
	}
	s := &Session{
		client:          c,
		actorID:         bytes32x(0x55),
		selectedMailbox: "INBOX",
	}
	w := &fakeFetchWriter{}
	uidSet := imap.UIDSet{}
	uidSet.AddRange(5, 6)
	if err := s.fetch(w, uidSet, &imap.FetchOptions{
		UID:          true,
		Flags:        true,
		InternalDate: true,
	}); err != nil {
		t.Fatalf("fetch: %v", err)
	}
	if c.gotMethod != wsrpc.MethodFetchMessageMetadata {
		t.Errorf("RPC method: %q", c.gotMethod)
	}
	if c.gotMbox != "INBOX" {
		t.Errorf("mailbox: %q", c.gotMbox)
	}
	if len(w.rows) != 2 {
		t.Fatalf("rows: %d", len(w.rows))
	}
	if w.rows[0].uid != 5 || w.rows[1].uid != 6 {
		t.Errorf("UIDs: %d, %d", w.rows[0].uid, w.rows[1].uid)
	}
	if !w.rows[0].flagsCalled || len(w.rows[0].flagsWritten) != 2 {
		t.Errorf("row 0 flags: %v", w.rows[0].flagsWritten)
	}
	if w.rows[0].flagsWritten[0] != imap.FlagSeen || w.rows[0].flagsWritten[1] != imap.FlagFlagged {
		t.Errorf("row 0 flag values: %v", w.rows[0].flagsWritten)
	}
	if !w.rows[0].intCalled || w.rows[0].internal != time.Unix(1_700_000_500, 0).UTC() {
		t.Errorf("row 0 internal date: %v", w.rows[0].internal)
	}
	if w.rows[0].sizeCalled {
		t.Errorf("RFC822Size must not fire when not requested")
	}
	if !w.rows[0].closed || !w.rows[1].closed {
		t.Errorf("rows must be closed")
	}
}

// TestFetchUIDSubsetPassesExplicitUIDsAndUsesNestSeqNum pins the F1 fast path:
// a `UID FETCH` of a bounded explicit set (a) passes those UIDs to the RPC
// instead of pulling the whole mailbox, and (b) emits each row's nest-computed
// sequence number — NOT its position in the fetched subset. The fake returns a
// single row for UID 10 whose true mailbox rank is 7; the old whole-mailbox
// path would have numbered it 1 (subset position) and fetched all metadata.
func TestFetchUIDSubsetPassesExplicitUIDsAndUsesNestSeqNum(t *testing.T) {
	mid := bytes32x(0x88)
	c := &fetchCaller{
		replies: []wsrpc.MessageMeta{
			{
				UID:            10,
				MessageID:      mid[:32],
				Modseq:         50,
				Flags:          []string{"\\Seen"},
				InternalDate:   1_700_000_700,
				CiphertextSize: 512,
				SeqNum:         7, // nest-computed rank in the full mailbox
			},
		},
	}
	s := &Session{client: c, actorID: bytes32x(0x55), selectedMailbox: "INBOX"}
	w := &fakeFetchWriter{}
	if err := s.fetch(w, imap.UIDSetNum(10), &imap.FetchOptions{UID: true, Flags: true}); err != nil {
		t.Fatalf("fetch: %v", err)
	}
	// (a) The explicit UID subset is passed to the RPC — no whole-mailbox fetch.
	if len(c.gotUIDs) != 1 || c.gotUIDs[0] != 10 {
		t.Errorf("expected explicit uids [10] passed to fetch_message_metadata, got %v", c.gotUIDs)
	}
	if len(w.rows) != 1 {
		t.Fatalf("rows: %d", len(w.rows))
	}
	// (b) The emitted sequence number is the nest's seq_num (7), not the subset
	// position (1) — the correctness guarantee that makes the fast path safe.
	if w.rows[0].seqNum != 7 {
		t.Errorf("seqNum: got %d, want 7 (nest seq_num, not subset position 1)", w.rows[0].seqNum)
	}
	if w.rows[0].uid != 10 {
		t.Errorf("uid: got %d, want 10", w.rows[0].uid)
	}
}

func TestFetchMetadataOnlyHonorsRFC822Size(t *testing.T) {
	c := &fetchCaller{
		replies: []wsrpc.MessageMeta{
			{
				UID:            10,
				MessageID:      make([]byte, 32),
				Modseq:         1,
				Flags:          []string{},
				InternalDate:   1_700_000_000,
				CiphertextSize: 4096,
			},
		},
	}
	s := &Session{client: c, actorID: bytes32x(0x66), selectedMailbox: "INBOX"}
	w := &fakeFetchWriter{}
	uidSet := imap.UIDSet{}
	uidSet.AddNum(10)
	if err := s.fetch(w, uidSet, &imap.FetchOptions{UID: true, RFC822Size: true}); err != nil {
		t.Fatalf("fetch: %v", err)
	}
	if len(w.rows) != 1 {
		t.Fatalf("rows: %d", len(w.rows))
	}
	if !w.rows[0].sizeCalled || w.rows[0].rfcSize != 4096 {
		t.Errorf("RFC822Size: called=%v size=%d", w.rows[0].sizeCalled, w.rows[0].rfcSize)
	}
}

// The old TestFetchBodyAxisRequiresDecryptor (an UPFRONT "body-axis FETCH
// requires an opener" gate) is retired by Phase-3 S2: a record without an
// opener errors at open time instead. The replacement coverage is the
// three-arm suite in fetch_body_test.go
// (TestFetchSealedRecordOpensRegardlessOfStorageMode / TestFetchRawRecordIsRefused /
// TestFetchSealedRecordWrongKeyErrors).

// TestFetchEmitsModSeqWhenRequested — a FETCH (MODSEQ) (options.ModSeq
// set by the FAUNA-FORK parser) emits the row's modseq via WriteModSeq.
func TestFetchEmitsModSeqWhenRequested(t *testing.T) {
	c := &fetchCaller{
		replies: []wsrpc.MessageMeta{
			{UID: 5, MessageID: make([]byte, 32), Modseq: 42, Flags: []string{"\\Seen"}, InternalDate: 1, CiphertextSize: 1},
		},
	}
	s := &Session{client: c, actorID: bytes32x(0x55), selectedMailbox: "INBOX"}
	w := &fakeFetchWriter{}
	uidSet := imap.UIDSet{}
	uidSet.AddNum(5)
	if err := s.fetch(w, uidSet, &imap.FetchOptions{UID: true, Flags: true, ModSeq: true}); err != nil {
		t.Fatalf("fetch: %v", err)
	}
	if len(w.rows) != 1 {
		t.Fatalf("rows: %d", len(w.rows))
	}
	if !w.rows[0].modSeqCalled || w.rows[0].modSeq != 42 {
		t.Errorf("MODSEQ: called=%v val=%d, want 42", w.rows[0].modSeqCalled, w.rows[0].modSeq)
	}
}

// TestFetchOmitsModSeqWithoutCondStore — a plain metadata FETCH (no
// MODSEQ item, CONDSTORE not enabled) MUST NOT emit MODSEQ; RFC 9051
// MUAs reject unsolicited MODSEQ.
func TestFetchOmitsModSeqWithoutCondStore(t *testing.T) {
	c := &fetchCaller{
		replies: []wsrpc.MessageMeta{
			{UID: 5, MessageID: make([]byte, 32), Modseq: 42, Flags: []string{}, InternalDate: 1, CiphertextSize: 1},
		},
	}
	s := &Session{client: c, actorID: bytes32x(0x55), selectedMailbox: "INBOX"}
	w := &fakeFetchWriter{}
	uidSet := imap.UIDSet{}
	uidSet.AddNum(5)
	if err := s.fetch(w, uidSet, &imap.FetchOptions{UID: true, Flags: true}); err != nil {
		t.Fatalf("fetch: %v", err)
	}
	if w.rows[0].modSeqCalled {
		t.Errorf("MODSEQ must not be emitted without CONDSTORE / MODSEQ request")
	}
}

// TestFetchCondStoreEnabledAutoEmitsModSeq — once CONDSTORE is enabled
// for the session, every FETCH FLAGS response carries MODSEQ even when
// the client didn't name the MODSEQ item (RFC 7162 §3.1.4.1).
func TestFetchCondStoreEnabledAutoEmitsModSeq(t *testing.T) {
	c := &fetchCaller{
		replies: []wsrpc.MessageMeta{
			{UID: 5, MessageID: make([]byte, 32), Modseq: 42, Flags: []string{}, InternalDate: 1, CiphertextSize: 1},
		},
	}
	s := &Session{client: c, actorID: bytes32x(0x55), selectedMailbox: "INBOX", condStoreEnabled: true}
	w := &fakeFetchWriter{}
	uidSet := imap.UIDSet{}
	uidSet.AddNum(5)
	if err := s.fetch(w, uidSet, &imap.FetchOptions{UID: true, Flags: true}); err != nil {
		t.Fatalf("fetch: %v", err)
	}
	if !w.rows[0].modSeqCalled || w.rows[0].modSeq != 42 {
		t.Errorf("CONDSTORE-enabled FETCH must auto-emit MODSEQ (got called=%v val=%d)", w.rows[0].modSeqCalled, w.rows[0].modSeq)
	}
}

// TestFetchChangedSinceFilters — CHANGEDSINCE drops rows whose modseq is
// at or below the supplied value (RFC 7162 §3.1.4); the parser sets
// ModSeq alongside ChangedSince, so survivors carry MODSEQ.
func TestFetchChangedSinceFilters(t *testing.T) {
	c := &fetchCaller{
		replies: []wsrpc.MessageMeta{
			{UID: 1, MessageID: make([]byte, 32), Modseq: 5, Flags: []string{}, InternalDate: 1, CiphertextSize: 1},
			{UID: 2, MessageID: make([]byte, 32), Modseq: 12, Flags: []string{}, InternalDate: 1, CiphertextSize: 1},
		},
	}
	s := &Session{client: c, actorID: bytes32x(0x55), selectedMailbox: "INBOX"}
	w := &fakeFetchWriter{}
	all := imap.UIDSet{}
	all.AddRange(1, 2)
	if err := s.fetch(w, all, &imap.FetchOptions{UID: true, Flags: true, ChangedSince: 10, ModSeq: true}); err != nil {
		t.Fatalf("fetch: %v", err)
	}
	if len(w.rows) != 1 {
		t.Fatalf("rows: %d, want 1 (UID 1 modseq 5 must be filtered)", len(w.rows))
	}
	if w.rows[0].uid != 2 {
		t.Errorf("survivor uid: %d, want 2", w.rows[0].uid)
	}
	if !w.rows[0].modSeqCalled || w.rows[0].modSeq != 12 {
		t.Errorf("survivor MODSEQ: called=%v val=%d, want 12", w.rows[0].modSeqCalled, w.rows[0].modSeq)
	}
}

func TestFetchUnauthenticatedRejected(t *testing.T) {
	s := &Session{client: &fetchCaller{}, selectedMailbox: "INBOX"} // no actorID
	w := &fakeFetchWriter{}
	uidSet := imap.UIDSet{}
	uidSet.AddNum(1)
	if err := s.fetch(w, uidSet, &imap.FetchOptions{UID: true, Flags: true}); err == nil {
		t.Fatalf("expected error without authenticated actor")
	}
}

func TestFetchNoMailboxSelectedRejected(t *testing.T) {
	s := &Session{client: &fetchCaller{}, actorID: bytes32x(0x68)} // no selectedMailbox
	w := &fakeFetchWriter{}
	uidSet := imap.UIDSet{}
	uidSet.AddNum(1)
	if err := s.fetch(w, uidSet, &imap.FetchOptions{UID: true, Flags: true}); err == nil {
		t.Fatalf("expected error without a SELECTed mailbox")
	}
}

// qresyncFetchCaller serves both fetch_message_metadata (the changed
// rows) and list_messages (whose ExpungedUIDs feed the VANISHED EARLIER
// response) for the QRESYNC `(CHANGEDSINCE n VANISHED)` FETCH path.
type qresyncFetchCaller struct {
	metaRows       []wsrpc.MessageMeta
	expungedUIDs   []uint32
	gotSinceModseq *int64 // captured list_messages since_modseq
}

func (c *qresyncFetchCaller) Call(_ context.Context, method string, body, reply any) error {
	enc, err := dagcbor.Marshal(body)
	if err != nil {
		return err
	}
	switch method {
	case wsrpc.MethodListMessages:
		var bod struct {
			SinceModseq *int64 `cbor:"since_modseq"`
		}
		_ = cbor.Unmarshal(enc, &bod)
		c.gotSinceModseq = bod.SinceModseq
		rep, err := dagcbor.Marshal(wsrpc.ListMessagesReply{
			Messages:      c.metaRows,
			ExpungedUIDs:  c.expungedUIDs,
			HighestModseq: 100,
		})
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)
	case wsrpc.MethodFetchMessageMetadata:
		rep, err := dagcbor.Marshal(struct {
			Messages []wsrpc.MessageMeta `cbor:"messages"`
		}{Messages: c.metaRows})
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)
	}
	return errors.New("qresyncFetchCaller: unexpected " + method)
}

func TestFetchChangedSinceVanishedEmitsVanishedEarlier(t *testing.T) {
	// UID FETCH 1:* (FLAGS) (CHANGEDSINCE 50 VANISHED) under an
	// ENABLE QRESYNC'd session: emit `* VANISHED (EARLIER) <expunged>`
	// (from list_messages since_modseq=50) before the changed-message
	// FETCH responses, and skip rows whose modseq is at/below 50.
	caller := &qresyncFetchCaller{
		metaRows: []wsrpc.MessageMeta{
			{UID: 1, MessageID: bytes32x(0x01), Flags: []string{}, Modseq: 10, InternalDate: 1, CiphertextSize: 1},
			{UID: 5, MessageID: bytes32x(0x05), Flags: []string{"\\Seen"}, Modseq: 60, InternalDate: 1, CiphertextSize: 1},
		},
		expungedUIDs: []uint32{2, 4},
	}
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0xB0),
		selectedMailbox: "INBOX",
		qresyncEnabled:  true,
	}
	w := &fakeFetchWriter{}
	opts := &imap.FetchOptions{Flags: true, ModSeq: true, ChangedSince: 50, Vanished: true, UID: true}
	if err := s.fetch(w, nil, opts); err != nil {
		t.Fatalf("fetch: %v", err)
	}

	// VANISHED (EARLIER) for the expunged set, exactly once.
	if len(w.vanishedEarlier) != 1 || w.vanishedEarlier[0].String() != "2,4" {
		t.Fatalf("WriteVanishedEarlier: %v, want one set [2,4]", w.vanishedEarlier)
	}
	// list_messages was queried with since_modseq = 50.
	if caller.gotSinceModseq == nil || *caller.gotSinceModseq != 50 {
		t.Errorf("list_messages since_modseq = %v, want 50", caller.gotSinceModseq)
	}
	// CHANGEDSINCE filter: only UID 5 (modseq 60 > 50) gets a FETCH row;
	// UID 1 (modseq 10) is skipped.
	if len(w.rows) != 1 {
		t.Fatalf("FETCH rows = %d, want 1 (UID 5 only)", len(w.rows))
	}
	if w.rows[0].uid != 5 {
		t.Errorf("FETCH row uid = %d, want 5", w.rows[0].uid)
	}
}
