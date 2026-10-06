package imap

import (
	"context"
	"errors"
	"log/slog"
	"testing"

	"github.com/emersion/go-imap/v2"
	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// moveCaller is the per-file fake for the MOVE path. Same RPC fanout
// as copyCaller, except `fauna.bridges.move` replaces `fauna.bridges.copy`
// and the post-move source mailbox no longer carries the moved rows
// (source-side EXPUNGE responses come from the MoveWriter, not from
// nest's reply — emersion's MoveWriter contract is "WriteCopyData
// once, then WriteExpunge for each source UID").
//
// By convention, every IMAP-layer test file declares its
// own caller stub.
type moveCaller struct {
	srcMeta    []wsrpc.MessageMeta // pre-move source mailbox (for numSet → UID resolution)
	survivors  []wsrpc.MessageMeta // post-move source mailbox (drops the moved rows)
	destMeta   []wsrpc.MessageMeta // post-move dest mailbox (for train-signal message_id lookup)
	moveReply  wsrpc.MoveMessagesReply
	moveErr    error
	cipherByID map[string][]byte

	moveCalls     int
	capturedMove  capturedMoveReq
	metaCalls     int
	metaMailboxes []string
	postMoveSrc   bool // toggled true once `fauna.bridges.move` returns; the per-mailbox metadata switches to the post-move shape
	putSpamCalls  []capturedPutSpam
}

type capturedMoveReq struct {
	ActorID       []byte   `cbor:"actor_id"`
	SourceMailbox string   `cbor:"source_mailbox"`
	UIDs          []uint32 `cbor:"uids"`
	DestMailbox   string   `cbor:"dest_mailbox"`
}

func (c *moveCaller) Call(_ context.Context, method string, body, reply any) error {
	enc, err := dagcbor.Marshal(body)
	if err != nil {
		return err
	}
	switch method {
	case wsrpc.MethodFetchMessageMetadata:
		c.metaCalls++
		var req struct {
			Mailbox string   `cbor:"mailbox"`
			UIDs    []uint32 `cbor:"uids"`
		}
		_ = cbor.Unmarshal(enc, &req)
		c.metaMailboxes = append(c.metaMailboxes, req.Mailbox)
		var rows []wsrpc.MessageMeta
		switch {
		case req.Mailbox == c.capturedMove.SourceMailbox && c.postMoveSrc:
			rows = c.survivors
		case req.Mailbox == c.capturedMove.SourceMailbox:
			rows = c.srcMeta
		default:
			rows = c.destMeta
		}
		rep, err := dagcbor.Marshal(struct {
			Messages []wsrpc.MessageMeta `cbor:"messages"`
		}{Messages: rows})
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)

	case wsrpc.MethodMove:
		c.moveCalls++
		_ = cbor.Unmarshal(enc, &c.capturedMove)
		c.postMoveSrc = true
		if c.moveErr != nil {
			return c.moveErr
		}
		rep, err := dagcbor.Marshal(c.moveReply)
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)

	case wsrpc.MethodFetchMessageCiphertext:
		var req struct {
			ActorID   []byte `cbor:"actor_id"`
			MessageID []byte `cbor:"message_id"`
		}
		_ = cbor.Unmarshal(enc, &req)
		blob, ok := c.cipherByID[string(req.MessageID)]
		if !ok {
			rep, _ := dagcbor.Marshal(map[string]any{"outcome": "not_found"})
			return cbor.Unmarshal(rep, reply)
		}
		rep, _ := dagcbor.Marshal(map[string]any{
			"outcome":         "found",
			"encrypted_body":  blob,
			"ciphertext_size": uint32(len(blob)),
			"internal_date":   int64(1_700_000_000),
		})
		return cbor.Unmarshal(rep, reply)

	}
	if ok, err := answerSpamModelRPC(method, enc, reply, &c.putSpamCalls); ok {
		return err
	}
	return errors.New("moveCaller: unexpected " + method)
}

// moveWriterFake captures the CopyData payload and every WriteExpunge
// call. Satisfies the moveWriter seam declared in move.go.
type moveWriterFake struct {
	copyData    *imap.CopyData
	copyDataSet bool
	expungeSeqs []uint32
}

func (w *moveWriterFake) WriteCopyData(data *imap.CopyData) error {
	w.copyData = data
	w.copyDataSet = true
	return nil
}

func (w *moveWriterFake) WriteExpunge(seqNum uint32) error {
	w.expungeSeqs = append(w.expungeSeqs, seqNum)
	return nil
}

// moveOpenerStub mirrors storeOpenerStub (`mailfauna.RecordOpener`). Body
// fixtures are real sealed envelopes (build via sealedEnvelopeFixture)
// because a mail record rests sealed and `mailfauna.OpenStoredRecord`
// refuses an unsealed payload.
type moveOpenerStub struct{ table map[string][]byte }

func (d *moveOpenerStub) Open(envelope []byte) ([]byte, error) {
	pt, ok := d.table[string(envelope)]
	if !ok {
		return nil, errors.New("moveOpenerStub: no mapping")
	}
	out := make([]byte, len(pt))
	copy(out, pt)
	return out, nil
}

// ── Tests ────────────────────────────────────────────────────────────────

func TestMoveEmitsCopyDataThenPerSourceExpunge(t *testing.T) {
	// MOVE 1,3 INBOX → Archive. Pre-move source has UIDs [1,2,3,4]
	// (seqNums 1,2,3,4); post-move source survivors are [2,4]. The
	// MoveWriter contract: WriteCopyData once, then WriteExpunge per
	// moved source UID in descending pre-move seqNum order.
	caller := &moveCaller{
		srcMeta: []wsrpc.MessageMeta{
			{UID: 1, MessageID: bytes32x(0x11), Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: 1},
			{UID: 2, MessageID: bytes32x(0x12), Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: 1},
			{UID: 3, MessageID: bytes32x(0x13), Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: 1},
			{UID: 4, MessageID: bytes32x(0x14), Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: 1},
		},
		survivors: []wsrpc.MessageMeta{
			{UID: 2, MessageID: bytes32x(0x12), Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: 1},
			{UID: 4, MessageID: bytes32x(0x14), Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: 1},
		},
		moveReply: wsrpc.MoveMessagesReply{
			DestUIDValidity: 6,
			Moved: []wsrpc.CopyPair{
				{SourceUID: 1, DestUID: 30},
				{SourceUID: 3, DestUID: 31},
			},
			SourceHighestmodseq: 5,
			DestHighestmodseq:   2,
		},
	}
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0xC0),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	w := &moveWriterFake{}
	if err := s.move(context.Background(), w, imap.UIDSetNum(1, 3), "Archive", nil); err != nil {
		t.Fatalf("move: %v", err)
	}
	if !w.copyDataSet || w.copyData == nil {
		t.Fatalf("expected WriteCopyData call, got %+v", w.copyData)
	}
	if w.copyData.UIDValidity != 6 {
		t.Errorf("UIDValidity: %d want 6", w.copyData.UIDValidity)
	}
	// Per-source EXPUNGE: pre-move UIDs 1,3 had seqNums 1,3 in the
	// pre-move source. Emitted descending: 3,1.
	if got, want := w.expungeSeqs, []uint32{3, 1}; !equalUint32(got, want) {
		t.Errorf("expunge seqs: %v want %v", got, want)
	}
	if len(caller.putSpamCalls) != 0 {
		t.Errorf("no train signal expected on non-\\Junk MOVE, got %d", len(caller.putSpamCalls))
	}
}

func TestMoveIntoJunkFiresSpamTrainSignal(t *testing.T) {
	// MOVE 1 INBOX → Junk fires label=spam, source=imap_junk_move.
	msg := bytes32x(0x21)
	// A real sealed envelope so the per-record rule routes the body
	// through the opener stub.
	ct := sealedEnvelopeFixture(t, []byte("payload-move-spam"))
	pt := []byte("Subject: spammy\r\n\r\nclick here")
	caller := &moveCaller{
		srcMeta: []wsrpc.MessageMeta{
			{UID: 1, MessageID: msg, Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: uint32(len(ct))},
		},
		survivors: nil,
		destMeta: []wsrpc.MessageMeta{
			{UID: 77, MessageID: msg, Flags: []string{}, Modseq: 2, InternalDate: 1, CiphertextSize: uint32(len(ct))},
		},
		moveReply: wsrpc.MoveMessagesReply{
			DestUIDValidity: 4,
			Moved: []wsrpc.CopyPair{
				{SourceUID: 1, DestUID: 77},
			},
			SourceHighestmodseq: 3,
			DestHighestmodseq:   2,
		},
		cipherByID: map[string][]byte{string(msg): ct},
	}
	opener := &moveOpenerStub{table: map[string][]byte{string(ct): pt}}
	stubSealToRecipient(t)
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0xC1),
		actorMLSPubkey:  bytes32x(0xC9),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	w := &moveWriterFake{}
	if err := s.move(context.Background(), w, imap.UIDSetNum(1), "Junk", opener); err != nil {
		t.Fatalf("move: %v", err)
	}
	if len(caller.putSpamCalls) != 1 {
		t.Fatalf("expected 1 put_spam_model write, got %d", len(caller.putSpamCalls))
	}
	tc := requireHistoryInsert(t, caller.putSpamCalls[0])
	if tc.Label != "spam" || tc.Source != "imap_junk_move" {
		t.Errorf("train: label=%q source=%q (want spam / imap_junk_move)", tc.Label, tc.Source)
	}
	// The body trained a fresh model (untrained actor), re-sealed.
	fresh := mailfauna.ApplySpamTraining(nil, string(pt), true).NewModelBytes
	if string(caller.putSpamCalls[0].SealedModel) != "sealed:"+string(fresh) {
		t.Errorf("sealed_model must be a fresh model trained once on the moved body")
	}
}

func TestMoveOutOfJunkFiresHamTrainSignal(t *testing.T) {
	msg := bytes32x(0x31)
	ct := sealedEnvelopeFixture(t, []byte("payload-move-ham"))
	pt := []byte("Subject: legit\r\n\r\nreal mail")
	caller := &moveCaller{
		srcMeta: []wsrpc.MessageMeta{
			{UID: 1, MessageID: msg, Flags: []string{"\\Junk"}, Modseq: 1, InternalDate: 1, CiphertextSize: uint32(len(ct))},
		},
		survivors: nil,
		destMeta: []wsrpc.MessageMeta{
			{UID: 200, MessageID: msg, Flags: []string{}, Modseq: 2, InternalDate: 1, CiphertextSize: uint32(len(ct))},
		},
		moveReply: wsrpc.MoveMessagesReply{
			DestUIDValidity: 8,
			Moved: []wsrpc.CopyPair{
				{SourceUID: 1, DestUID: 200},
			},
			SourceHighestmodseq: 4,
			DestHighestmodseq:   3,
		},
		cipherByID: map[string][]byte{string(msg): ct},
	}
	opener := &moveOpenerStub{table: map[string][]byte{string(ct): pt}}
	stubSealToRecipient(t)
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0xC2),
		actorMLSPubkey:  bytes32x(0xC9),
		selectedMailbox: "Junk",
		logger:          slog.Default(),
	}
	w := &moveWriterFake{}
	if err := s.move(context.Background(), w, imap.UIDSetNum(1), "Inbox", opener); err != nil {
		t.Fatalf("move: %v", err)
	}
	if len(caller.putSpamCalls) != 1 {
		t.Fatalf("expected 1 put_spam_model write, got %d", len(caller.putSpamCalls))
	}
	if ins := requireHistoryInsert(t, caller.putSpamCalls[0]); ins.Label != "ham" {
		t.Errorf("label: %q want ham", ins.Label)
	}
}

func TestMoveEmptyResultEmitsNothing(t *testing.T) {
	// MOVE that misses (all source UIDs absent) → nest returns empty
	// `moved`. No CopyData write, no EXPUNGE, nil error.
	caller := &moveCaller{
		srcMeta: nil,
		moveReply: wsrpc.MoveMessagesReply{
			DestUIDValidity:     2,
			Moved:               nil,
			SourceHighestmodseq: 1,
			DestHighestmodseq:   1,
		},
	}
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0xC3),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	w := &moveWriterFake{}
	if err := s.move(context.Background(), w, imap.UIDSetNum(99), "Archive", nil); err != nil {
		t.Fatalf("move: %v", err)
	}
	if w.copyDataSet {
		t.Errorf("expected no WriteCopyData on empty moved, got %+v", w.copyData)
	}
	if len(w.expungeSeqs) != 0 {
		t.Errorf("expected no WriteExpunge calls, got %v", w.expungeSeqs)
	}
}

// TestMoveOverQuotaMapsToNoOverQuota: an over-quota destination on MOVE
// comes back as the typed `fauna.bridges.over_quota` error (the nest
// pre-check rolled back, source intact); the MDA surfaces it as
// `NO [OVERQUOTA]` (imap-server.md § Quota enforcement points).
func TestMoveOverQuotaMapsToNoOverQuota(t *testing.T) {
	payload, err := dagcbor.Marshal(map[string]any{"code": wsrpc.CodeOverQuota})
	if err != nil {
		t.Fatalf("marshal payload: %v", err)
	}
	caller := &moveCaller{
		srcMeta: []wsrpc.MessageMeta{
			{UID: 1, MessageID: bytes32x(0x11), Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: 1},
		},
		moveErr: &wsrpc.ServerError{Payload: payload},
	}
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0xC5),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	w := &moveWriterFake{}
	err = s.move(context.Background(), w, imap.UIDSetNum(1), "Archive", nil)
	var imapErr *imap.Error
	if !errors.As(err, &imapErr) {
		t.Fatalf("over-quota move must yield *imap.Error, got %T: %v", err, err)
	}
	if imapErr.Code != imap.ResponseCodeOverQuota {
		t.Errorf("response code = %q, want OVERQUOTA", imapErr.Code)
	}
}

// TestMoveHeldForReviewMapsToNo: a supervised account's MUA moving a
// message out of the guardian held mailbox while its hold is live comes
// back as the typed `fauna.bridges.held_for_review` error (the nest
// refused, nothing moved); the MDA surfaces it as a tagged `NO` telling
// the user why (family-safety.md § The mail gate).
func TestMoveHeldForReviewMapsToNo(t *testing.T) {
	payload, err := dagcbor.Marshal(map[string]any{"code": wsrpc.CodeHeldForReview})
	if err != nil {
		t.Fatalf("marshal payload: %v", err)
	}
	caller := &moveCaller{
		srcMeta: []wsrpc.MessageMeta{
			{UID: 1, MessageID: bytes32x(0x11), Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: 1},
		},
		moveErr: &wsrpc.ServerError{Payload: payload},
	}
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0xC5),
		selectedMailbox: "Guardian Review",
		logger:          slog.Default(),
	}
	w := &moveWriterFake{}
	err = s.move(context.Background(), w, imap.UIDSetNum(1), "INBOX", nil)
	var imapErr *imap.Error
	if !errors.As(err, &imapErr) {
		t.Fatalf("held-for-review move must yield *imap.Error, got %T: %v", err, err)
	}
	if imapErr.Type != imap.StatusResponseTypeNo {
		t.Errorf("response type = %q, want NO", imapErr.Type)
	}
	if imapErr.Text != "Message is held for guardian review" {
		t.Errorf("response text = %q", imapErr.Text)
	}
}
