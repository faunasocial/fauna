package imap

import (
	"context"
	"errors"
	"log/slog"
	"testing"

	"github.com/emersion/go-imap/v2"
	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// expungeCaller is the per-file fake for the EXPUNGE path. It
// dispatches the two RPCs Session.Expunge can fan out to:
//
//   - fauna.bridges.expunge: the actual write. Reply carries the UIDs
//     nest actually expunged (after the \Deleted-flag intersection
//     RFC 4315 §2.1 mandates for UID EXPUNGE), plus the new HIGHESTMODSEQ.
//   - fauna.bridges.fetch_message_metadata: returned *after* the
//     expunge call, so `metaReplies` holds the *post-expunge survivor
//     list* — the rows still in the mailbox after the removal. The
//     MDA derives each removed UID's pre-expunge sequence number from
//     this survivor list + the reply's removed-UID list arithmetically
//     (see expunge.go's pre_seq formula); a real-production fake would
//     drop the removed UIDs from the metadata reply, which is what
//     these fixtures do.
//
// By convention, every IMAP-layer test file declares its
// own caller stub so future drift on one test file's fake doesn't
// take the rest of the suite down with it.
type expungeCaller struct {
	// fixtures
	survivorMeta []wsrpc.MessageMeta // post-expunge — what fetch_message_metadata returns
	expungeReply wsrpc.ExpungeReply
	expungeErr   error

	// captured state
	expungeCalls    int
	capturedExpunge capturedExpungeReq
	metaCalls       int
}

type capturedExpungeReq struct {
	ActorID []byte   `cbor:"actor_id"`
	Mailbox string   `cbor:"mailbox"`
	UIDs    []uint32 `cbor:"uids"`
}

func (c *expungeCaller) Call(_ context.Context, method string, body, reply any) error {
	enc, err := dagcbor.Marshal(body)
	if err != nil {
		return err
	}
	switch method {
	case wsrpc.MethodFetchMessageMetadata:
		c.metaCalls++
		rep, err := dagcbor.Marshal(struct {
			Messages []wsrpc.MessageMeta `cbor:"messages"`
		}{Messages: c.survivorMeta})
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)

	case wsrpc.MethodExpunge:
		c.expungeCalls++
		_ = cbor.Unmarshal(enc, &c.capturedExpunge)
		if c.expungeErr != nil {
			return c.expungeErr
		}
		rep, err := dagcbor.Marshal(c.expungeReply)
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)
	}
	return errors.New("expungeCaller: unexpected " + method)
}

// expungeWriterFake records every WriteExpunge call. Satisfies the
// expungeWriter seam declared in expunge.go so Session.expunge can be
// tested without standing up a real *imapserver.ExpungeWriter (which
// needs a live TCP connection).
type expungeWriterFake struct {
	seqs     []uint32
	vanished []imap.UIDSet // QRESYNC `* VANISHED <set>` calls
	err      error         // nil → all writes succeed
}

func (w *expungeWriterFake) WriteExpunge(seqNum uint32) error {
	w.seqs = append(w.seqs, seqNum)
	return w.err
}

func (w *expungeWriterFake) WriteVanished(uids imap.UIDSet) error {
	w.vanished = append(w.vanished, uids)
	return w.err
}

// ── Tests ────────────────────────────────────────────────────────────────

func TestExpungeFullEmitsPerUIDSeqInDescendingOrder(t *testing.T) {
	// Full EXPUNGE on a 5-message mailbox where UIDs 1, 3, 5 carried
	// \Deleted. Pre-expunge seqNums are 1..5; post-expunge survivors
	// are UIDs 2, 4 (the metadata RPC sees this state because it runs
	// after the expunge call). The MDA derives the pre-expunge
	// seqNums via the pre_seq formula:
	//   UID 1: |surv≤1| (0) + |rmvd≤1| (1) = 1
	//   UID 3: |surv≤3| (1: UID 2) + |rmvd≤3| (2: UIDs 1,3) = 3
	//   UID 5: |surv≤5| (2: UIDs 2,4) + |rmvd≤5| (3: UIDs 1,3,5) = 5
	// Emitted descending: 5, 3, 1.
	caller := &expungeCaller{
		survivorMeta: []wsrpc.MessageMeta{
			{UID: 2, MessageID: bytes32x(0x02), Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: 1},
			{UID: 4, MessageID: bytes32x(0x04), Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: 1},
		},
		expungeReply: wsrpc.ExpungeReply{
			ExpungedUIDs:  []uint32{1, 3, 5},
			HighestModseq: 10,
		},
	}
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0xA0),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	w := &expungeWriterFake{}
	if err := s.expunge(context.Background(), w, nil); err != nil {
		t.Fatalf("expunge: %v", err)
	}

	// Wire: full EXPUNGE → uids empty on the RPC.
	if len(caller.capturedExpunge.UIDs) != 0 {
		t.Errorf("full EXPUNGE wire uids: %v want empty", caller.capturedExpunge.UIDs)
	}
	if caller.expungeCalls != 1 {
		t.Errorf("expunge_calls = %d, want 1", caller.expungeCalls)
	}

	// Per-UID EXPUNGE responses: UIDs 1,3,5 → seqNums 1,3,5 in pre-
	// expunge order; emitted descending → 5,3,1.
	if got, want := w.seqs, []uint32{5, 3, 1}; !equalUint32(got, want) {
		t.Errorf("WriteExpunge seqs: %v want %v", got, want)
	}
}

func TestUIDExpungeForwardsUIDSetVerbatim(t *testing.T) {
	// UID EXPUNGE (RFC 4315 §2.1) bounds the operation to the
	// intersection of the requested UID set and the \Deleted-flagged
	// rows. Pre-state: 5 rows; UIDs 2,4,5 are \Deleted. Request UID set
	// {1,2,3,4} ⇒ nest returns expunged {2,4} (UID 5 is \Deleted but
	// excluded from the request). Post-expunge survivors: UIDs 1,3,5.
	// pre_seq derivation:
	//   UID 2: |surv≤2| (1: UID 1) + |rmvd≤2| (1: UID 2) = 2
	//   UID 4: |surv≤4| (2: UIDs 1,3) + |rmvd≤4| (2: UIDs 2,4) = 4
	// Emitted descending: 4, 2.
	caller := &expungeCaller{
		survivorMeta: []wsrpc.MessageMeta{
			{UID: 1, MessageID: bytes32x(0x01), Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: 1},
			{UID: 3, MessageID: bytes32x(0x03), Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: 1},
			{UID: 5, MessageID: bytes32x(0x05), Flags: []string{"\\Deleted"}, Modseq: 1, InternalDate: 1, CiphertextSize: 1},
		},
		expungeReply: wsrpc.ExpungeReply{
			ExpungedUIDs:  []uint32{2, 4},
			HighestModseq: 7,
		},
	}
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0xA1),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	w := &expungeWriterFake{}
	requested := []uint32{1, 2, 3, 4}
	if err := s.expunge(context.Background(), w, &requested); err != nil {
		t.Fatalf("expunge: %v", err)
	}

	// Wire: requested UIDs forwarded verbatim.
	if got, want := caller.capturedExpunge.UIDs, []uint32{1, 2, 3, 4}; !equalUint32(got, want) {
		t.Errorf("UID EXPUNGE wire uids: %v want %v", got, want)
	}

	// Per-UID EXPUNGE responses: only the intersection (UIDs 2,4) →
	// seqNums 2,4 → descending → 4,2.
	if got, want := w.seqs, []uint32{4, 2}; !equalUint32(got, want) {
		t.Errorf("WriteExpunge seqs: %v want %v", got, want)
	}
}

func TestExpungeUnderQResyncEmitsVanishedNotPerUIDSeq(t *testing.T) {
	// Once the client has ENABLE QRESYNC'd, EXPUNGE reports the removed
	// set as a single `* VANISHED <uid_set>` (RFC 7162 §3.2.10) — no
	// per-UID sequence derivation and, crucially, no post-expunge
	// survivor metadata round-trip (VANISHED carries UIDs directly).
	caller := &expungeCaller{
		// survivorMeta would only be consumed by the per-UID fallback;
		// under QRESYNC the MDA must NOT fetch it.
		survivorMeta: []wsrpc.MessageMeta{
			{UID: 2, MessageID: bytes32x(0x02), Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: 1},
			{UID: 4, MessageID: bytes32x(0x04), Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: 1},
		},
		expungeReply: wsrpc.ExpungeReply{
			ExpungedUIDs:  []uint32{1, 3, 5},
			HighestModseq: 10,
		},
	}
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0xA1),
		selectedMailbox: "INBOX",
		qresyncEnabled:  true, // ENABLE QRESYNC observed (conn is nil in unit tests)
		logger:          slog.Default(),
	}
	w := &expungeWriterFake{}
	if err := s.expunge(context.Background(), w, nil); err != nil {
		t.Fatalf("expunge: %v", err)
	}

	// Exactly one VANISHED response carrying the removed set, range-coalesced.
	if len(w.vanished) != 1 {
		t.Fatalf("want exactly one WriteVanished call, got %d (seqs=%v)", len(w.vanished), w.seqs)
	}
	if got, want := w.vanished[0].String(), "1,3,5"; got != want {
		t.Errorf("VANISHED set = %q, want %q", got, want)
	}
	// No per-UID EXPUNGE fallback under QRESYNC.
	if len(w.seqs) != 0 {
		t.Errorf("WriteExpunge must not fire under QRESYNC, got seqs %v", w.seqs)
	}
	// No survivor-metadata round-trip under QRESYNC (VANISHED needs no seqs).
	if caller.metaCalls != 0 {
		t.Errorf("fetch_message_metadata must not fire under QRESYNC, got %d calls", caller.metaCalls)
	}
}

func TestExpungeEmptyResultIsNotAnError(t *testing.T) {
	// EXPUNGE on a mailbox with no \Deleted rows: nest returns an empty
	// expunged_uids. The MDA must not error; the IMAP wire just gets a
	// tagged OK with no untagged EXPUNGE responses (the client's pre-
	// state was already correct). The metadata round-trip is skipped on
	// the empty-reply fast path.
	caller := &expungeCaller{
		expungeReply: wsrpc.ExpungeReply{
			ExpungedUIDs:  []uint32{},
			HighestModseq: 3,
		},
	}
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0xA2),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	w := &expungeWriterFake{}
	if err := s.expunge(context.Background(), w, nil); err != nil {
		t.Fatalf("expunge: %v", err)
	}
	if len(w.seqs) != 0 {
		t.Errorf("expected no WriteExpunge calls, got %v", w.seqs)
	}
	// We do not need the seq↔UID metadata round-trip when the reply is
	// empty — the optimization saves one RPC on a no-op EXPUNGE.
	if caller.metaCalls != 0 {
		t.Errorf("metadata round-trip should be skipped on empty reply, got %d calls", caller.metaCalls)
	}
}

func equalUint32(a, b []uint32) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}
