package imap

import (
	"testing"

	"github.com/emersion/go-imap/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// TestWantsSeen pins the implicit-\Seen predicate (RFC 9051 §6.4.5): a
// non-PEEK BODY[…] or BINARY[…] section sets \Seen; the
// .PEEK forms suppress it; BINARY.SIZE[…] is a size query that never sets it;
// metadata-only items (FLAGS / ENVELOPE / BODYSTRUCTURE / UID / RFC822.SIZE)
// never set it.
func TestWantsSeen(t *testing.T) {
	cases := []struct {
		name string
		opts *imap.FetchOptions
		want bool
	}{
		{"nil", nil, false},
		{"metadata only — FLAGS", &imap.FetchOptions{Flags: true, UID: true}, false},
		{"metadata only — ENVELOPE+BODYSTRUCTURE", &imap.FetchOptions{Envelope: true, BodyStructure: &imap.FetchItemBodyStructure{}}, false},
		{"BODY[] non-PEEK", &imap.FetchOptions{BodySection: []*imap.FetchItemBodySection{{}}}, true},
		{"BODY.PEEK[]", &imap.FetchOptions{BodySection: []*imap.FetchItemBodySection{{Peek: true}}}, false},
		{"BODY[HEADER] non-PEEK", &imap.FetchOptions{BodySection: []*imap.FetchItemBodySection{{Specifier: imap.PartSpecifierHeader}}}, true},
		{"BODY.PEEK[HEADER]", &imap.FetchOptions{BodySection: []*imap.FetchItemBodySection{{Specifier: imap.PartSpecifierHeader, Peek: true}}}, false},
		{"mixed PEEK + non-PEEK BODY", &imap.FetchOptions{BodySection: []*imap.FetchItemBodySection{{Peek: true}, {Specifier: imap.PartSpecifierText}}}, true},
		{"BINARY[1] non-PEEK", &imap.FetchOptions{BinarySection: []*imap.FetchItemBinarySection{{Part: []int{1}}}}, true},
		{"BINARY.PEEK[1]", &imap.FetchOptions{BinarySection: []*imap.FetchItemBinarySection{{Part: []int{1}, Peek: true}}}, false},
		{"BINARY.SIZE[1] — size query", &imap.FetchOptions{BinarySectionSize: []*imap.FetchItemBinarySectionSize{{Part: []int{1}}}}, false},
		{"BINARY.SIZE + BODY.PEEK", &imap.FetchOptions{
			BinarySectionSize: []*imap.FetchItemBinarySectionSize{{Part: []int{1}}},
			BodySection:       []*imap.FetchItemBodySection{{Peek: true}},
		}, false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := wantsSeen(tc.opts); got != tc.want {
				t.Errorf("wantsSeen(%s) = %v, want %v", tc.name, got, tc.want)
			}
		})
	}
}

// seenSession builds a Session wired to a bodyFetchCaller with two messages —
// UID 5 (unseen) and UID 7 (already \Seen) — both with a sealed ciphertext
// that the stub opens to plainTextRFC5322. condStore toggles CONDSTORE for
// the session. The body-open seam (recordOpener) is a concrete type, so the
// stubDecryptor is returned to pass as the explicit opener arg to
// fetchWithDecryptor.
func seenSession(t *testing.T, condStore bool) (*Session, *bodyFetchCaller, *stubDecryptor, *bodyFetchWriter) {
	t.Helper()
	mid5 := bytes32x(0x55)
	mid7 := bytes32x(0x77)
	ct5 := sealedEnvelopeFixture(t, []byte("payload-5"))
	ct7 := sealedEnvelopeFixture(t, []byte("payload-7"))
	caller := &bodyFetchCaller{
		metaReplies: []wsrpc.MessageMeta{
			{UID: 5, MessageID: mid5[:], Modseq: 10, Flags: []string{}, InternalDate: 1_700_000_500, CiphertextSize: uint32(len(ct5))},
			{UID: 7, MessageID: mid7[:], Modseq: 11, Flags: []string{"\\Seen"}, InternalDate: 1_700_000_600, CiphertextSize: uint32(len(ct7))},
		},
		cipherByID: map[string][]byte{string(mid5[:]): ct5, string(mid7[:]): ct7},
	}
	dec := &stubDecryptor{mapping: map[string][]byte{
		string(ct5): []byte(plainTextRFC5322),
		string(ct7): []byte(plainTextRFC5322),
	}}
	s := &Session{
		client:              caller,
		actorID:             bytes32x(0x11),
		selectedMailbox:     "INBOX",
		selectedUIDValidity: 100,
		cache:               newBodyStructureCache(4),
		condStoreEnabled:    condStore,
	}
	return s, caller, dec, &bodyFetchWriter{}
}

func flagsContain(flags []imap.Flag, want imap.Flag) bool {
	for _, f := range flags {
		if f == want {
			return true
		}
	}
	return false
}

// TestFetchNonPeekBodySetsSeen: a non-PEEK BODY[] over an unseen + an
// already-seen message issues ONE batched store_flags(add, \Seen) for only the
// unseen UID, and force-emits FLAGS (carrying \Seen) for that row even though
// the client didn't request FLAGS (Dovecot parity). The already-seen row is
// neither re-stored nor force-flagged.
func TestFetchNonPeekBodySetsSeen(t *testing.T) {
	s, caller, dec, w := seenSession(t, false)
	numSet := imap.SeqSet{}
	numSet.AddRange(1, 0) // 1:*
	if err := s.fetchWithDecryptor(w, numSet, &imap.FetchOptions{
		UID:         true,
		BodySection: []*imap.FetchItemBodySection{{}},
	}, dec); err != nil {
		t.Fatalf("fetch: %v", err)
	}

	if caller.storeFlagsHit != 1 {
		t.Fatalf("store_flags calls = %d, want 1 (one batched +\\Seen)", caller.storeFlagsHit)
	}
	if len(caller.lastStoreUIDs) != 1 || caller.lastStoreUIDs[0] != 5 {
		t.Errorf("store_flags UIDs = %v, want [5] (only the unseen UID)", caller.lastStoreUIDs)
	}
	if caller.lastStoreOp != "add" {
		t.Errorf("store_flags op = %q, want \"add\"", caller.lastStoreOp)
	}
	if len(caller.lastStoreFlags) != 1 || caller.lastStoreFlags[0] != "\\Seen" {
		t.Errorf("store_flags flags = %v, want [\\Seen]", caller.lastStoreFlags)
	}

	if len(w.rows) != 2 {
		t.Fatalf("emitted rows = %d, want 2", len(w.rows))
	}
	row5 := w.rows[0] // seq 1 = UID 5
	if !row5.uidCalled || row5.uid != 5 {
		t.Fatalf("row0 UID = %d (called=%v), want 5", row5.uid, row5.uidCalled)
	}
	if !row5.flagsCalled {
		t.Errorf("newly-\\Seen row must force-emit FLAGS even without options.Flags")
	}
	if !flagsContain(row5.flags, imap.FlagSeen) {
		t.Errorf("row5 FLAGS = %v, want to contain \\Seen", row5.flags)
	}
	if row5.modseqCalled {
		t.Errorf("MODSEQ must NOT be emitted without CONDSTORE")
	}

	row7 := w.rows[1] // seq 2 = UID 7 (already \Seen)
	if row7.flagsCalled {
		t.Errorf("already-\\Seen row must NOT force-emit FLAGS when options.Flags is unset")
	}
}

// TestFetchBinaryNonPeekSetsSeen: BINARY[1] (non-PEEK) sets \Seen, same as BODY.
func TestFetchBinaryNonPeekSetsSeen(t *testing.T) {
	s, caller, dec, w := seenSession(t, false)
	numSet := imap.UIDSet{}
	numSet.AddNum(5)
	if err := s.fetchWithDecryptor(w, numSet, &imap.FetchOptions{
		UID:           true,
		BinarySection: []*imap.FetchItemBinarySection{{Part: []int{1}}},
	}, dec); err != nil {
		t.Fatalf("fetch: %v", err)
	}
	if caller.storeFlagsHit != 1 || len(caller.lastStoreUIDs) != 1 || caller.lastStoreUIDs[0] != 5 {
		t.Errorf("BINARY[1] non-PEEK should set \\Seen on UID 5: hits=%d uids=%v", caller.storeFlagsHit, caller.lastStoreUIDs)
	}
}

// TestFetchBodyPeekDoesNotSetSeen: BODY.PEEK[] never sets \Seen.
func TestFetchBodyPeekDoesNotSetSeen(t *testing.T) {
	s, caller, dec, w := seenSession(t, false)
	numSet := imap.UIDSet{}
	numSet.AddNum(5)
	if err := s.fetchWithDecryptor(w, numSet, &imap.FetchOptions{
		UID:         true,
		BodySection: []*imap.FetchItemBodySection{{Peek: true}},
	}, dec); err != nil {
		t.Fatalf("fetch: %v", err)
	}
	if caller.storeFlagsHit != 0 {
		t.Errorf("BODY.PEEK[] must not call store_flags: hits=%d", caller.storeFlagsHit)
	}
	if len(w.rows) != 1 || w.rows[0].flagsCalled {
		t.Errorf("BODY.PEEK[] row must not force-emit FLAGS")
	}
}

// TestFetchBinarySizeDoesNotSetSeen: BINARY.SIZE[…] is a size query.
func TestFetchBinarySizeDoesNotSetSeen(t *testing.T) {
	s, caller, dec, w := seenSession(t, false)
	numSet := imap.UIDSet{}
	numSet.AddNum(5)
	if err := s.fetchWithDecryptor(w, numSet, &imap.FetchOptions{
		UID:               true,
		BinarySectionSize: []*imap.FetchItemBinarySectionSize{{Part: []int{1}}},
	}, dec); err != nil {
		t.Fatalf("fetch: %v", err)
	}
	if caller.storeFlagsHit != 0 {
		t.Errorf("BINARY.SIZE[…] must not call store_flags: hits=%d", caller.storeFlagsHit)
	}
}

// TestFetchCondStoreEmitsBumpedModSeqOnImplicitSeen: under CONDSTORE, the
// implicit-\Seen FETCH carries the post-set (bumped) MODSEQ on the affected row
// (RFC 7162 §3.1.4) — proving the store_flags modseq merge-back into the row.
func TestFetchCondStoreEmitsBumpedModSeqOnImplicitSeen(t *testing.T) {
	s, _, dec, w := seenSession(t, true)
	numSet := imap.UIDSet{}
	numSet.AddNum(5)
	if err := s.fetchWithDecryptor(w, numSet, &imap.FetchOptions{
		UID:         true,
		BodySection: []*imap.FetchItemBodySection{{}},
	}, dec); err != nil {
		t.Fatalf("fetch: %v", err)
	}
	if len(w.rows) != 1 {
		t.Fatalf("rows = %d, want 1", len(w.rows))
	}
	row := w.rows[0]
	if !row.modseqCalled {
		t.Fatalf("CONDSTORE: MODSEQ must be emitted on the implicit-\\Seen row")
	}
	// The fake bumps modseq to 1_000 + seq; the original row modseq was 10, so a
	// post-set value > 10 proves the merge-back (not the stale pre-set value).
	if row.modseq <= 10 {
		t.Errorf("emitted MODSEQ = %d, want the post-store bumped value (> 10)", row.modseq)
	}
}
