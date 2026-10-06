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

// copyCaller is the per-file fake for the COPY path. Dispatches the
// four RPCs Session.Copy can fan out to:
//
//   - fauna.bridges.fetch_message_metadata: numSet → UID resolution
//     when the wire numSet is a SeqSet or dynamic UIDSet (mirrors the
//     same fanout shape as store.go's resolveStoreNumSet).
//   - fauna.bridges.copy: the actual write; returns the dest-UID
//     mapping per source UID.
//   - fauna.bridges.fetch_message_ciphertext: encrypted body fetch for
//     the \Junk training signal path (per matching dest UID).
//   - fauna.bridges.fetch_spam_model / put_spam_model: the agent-side
//     train (answerSpamModelRPC — an untrained actor, so each UID writes
//     a model trained from empty with source=imap_junk_move).
//
// By convention, every IMAP-layer test file declares its
// own caller stub so future drift on one file's fake doesn't take the
// rest of the suite down with it.
type copyCaller struct {
	// fixtures
	srcMeta    []wsrpc.MessageMeta
	destMeta   []wsrpc.MessageMeta // post-copy dest mailbox state
	copyReply  wsrpc.CopyMessagesReply
	copyErr    error
	cipherByID map[string][]byte

	// captured state
	copyCalls      int
	capturedCopy   capturedCopyReq
	metaCalls      int
	metaMailboxes  []string // per-call mailbox arg (order)
	ciphertextHits int
	putSpamCalls   []capturedPutSpam
}

type capturedCopyReq struct {
	ActorID       []byte   `cbor:"actor_id"`
	SourceMailbox string   `cbor:"source_mailbox"`
	UIDs          []uint32 `cbor:"uids"`
	DestMailbox   string   `cbor:"dest_mailbox"`
}

func (c *copyCaller) Call(_ context.Context, method string, body, reply any) error {
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
		// Pick which fixture set to return based on the mailbox the
		// caller asked about. The dest-side metadata is what the train
		// signal path consults for message_id → UID resolution.
		rows := c.srcMeta
		if req.Mailbox != c.capturedCopy.SourceMailbox && c.destMeta != nil {
			rows = c.destMeta
		}
		rep, err := dagcbor.Marshal(struct {
			Messages []wsrpc.MessageMeta `cbor:"messages"`
		}{Messages: rows})
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)

	case wsrpc.MethodCopy:
		c.copyCalls++
		_ = cbor.Unmarshal(enc, &c.capturedCopy)
		if c.copyErr != nil {
			return c.copyErr
		}
		rep, err := dagcbor.Marshal(c.copyReply)
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)

	case wsrpc.MethodFetchMessageCiphertext:
		c.ciphertextHits++
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
	return errors.New("copyCaller: unexpected " + method)
}

// copyOpenerStub mirrors storeOpenerStub (`mailfauna.RecordOpener`).
// Per-file fake so a future drift on the body-fetch path's stub doesn't
// break us. Build bodies via sealedEnvelopeFixture — a mail record rests
// sealed, so a fixture body is a real sealed envelope.
type copyOpenerStub struct {
	table map[string][]byte
}

func (d *copyOpenerStub) Open(envelope []byte) ([]byte, error) {
	pt, ok := d.table[string(envelope)]
	if !ok {
		return nil, errors.New("copyOpenerStub: no mapping for envelope")
	}
	out := make([]byte, len(pt))
	copy(out, pt)
	return out, nil
}

// ── Tests ────────────────────────────────────────────────────────────────

func TestCopyEmitsCopyUIDWithDestPairs(t *testing.T) {
	// Plain COPY into a non-\Junk mailbox: nest assigns dest UIDs; MDA
	// returns *imap.CopyData carrying UIDValidity + parallel source/
	// dest NumSets, which emersion writes as the tagged-OK
	// `[COPYUID <uid_validity> <source-set> <dest-set>]` response.
	caller := &copyCaller{
		srcMeta: []wsrpc.MessageMeta{
			{UID: 1, MessageID: bytes32x(0xC1), Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: 1},
			{UID: 2, MessageID: bytes32x(0xC2), Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: 1},
		},
		copyReply: wsrpc.CopyMessagesReply{
			DestUIDValidity: 7,
			Copied: []wsrpc.CopyPair{
				{SourceUID: 1, DestUID: 10},
				{SourceUID: 2, DestUID: 11},
			},
			DestHighestmodseq: 5,
		},
	}
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0xB0),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	data, err := s.copy(context.Background(), imap.UIDSetNum(1, 2), "Archive", nil)
	if err != nil {
		t.Fatalf("copy: %v", err)
	}
	if data == nil {
		t.Fatalf("expected *imap.CopyData, got nil")
	}
	if data.UIDValidity != 7 {
		t.Errorf("UIDValidity: %d want 7", data.UIDValidity)
	}
	// Source + dest UID sets carry the pairs in request order.
	srcNums, ok := data.SourceUIDs.Nums()
	if !ok {
		t.Fatalf("SourceUIDs is dynamic; want static set")
	}
	destNums, ok := data.DestUIDs.Nums()
	if !ok {
		t.Fatalf("DestUIDs is dynamic; want static set")
	}
	if len(srcNums) != 2 || srcNums[0] != 1 || srcNums[1] != 2 {
		t.Errorf("source UIDs: %v want [1 2]", srcNums)
	}
	if len(destNums) != 2 || destNums[0] != 10 || destNums[1] != 11 {
		t.Errorf("dest UIDs: %v want [10 11]", destNums)
	}
	// Wire shape: source/dest mailboxes carried verbatim.
	if caller.capturedCopy.SourceMailbox != "INBOX" {
		t.Errorf("source_mailbox: %q", caller.capturedCopy.SourceMailbox)
	}
	if caller.capturedCopy.DestMailbox != "Archive" {
		t.Errorf("dest_mailbox: %q", caller.capturedCopy.DestMailbox)
	}
	// No \Junk involvement → no training signal.
	if len(caller.putSpamCalls) != 0 {
		t.Errorf("no train signal expected on non-\\Junk COPY, got %d", len(caller.putSpamCalls))
	}
}

func TestCopyIntoJunkFiresSpamTrainSignal(t *testing.T) {
	// COPY into the canonical "Junk" mailbox trains agent-side once per
	// UID with label=spam, source=imap_junk_move (per mail-spam.md
	// § Training signal sources): one sealed put_spam_model per UID.
	msg1 := bytes32x(0xD1)
	msg2 := bytes32x(0xD2)
	// Real sealed envelopes so the per-record rule routes the bodies
	// through the opener stub.
	ct1 := sealedEnvelopeFixture(t, []byte("payload-junk-1"))
	ct2 := sealedEnvelopeFixture(t, []byte("payload-junk-2"))
	pt1 := []byte("Subject: spam-1\r\n\r\nbody-1")
	pt2 := []byte("Subject: spam-2\r\n\r\nbody-2")
	caller := &copyCaller{
		srcMeta: []wsrpc.MessageMeta{
			{UID: 1, MessageID: msg1, Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: uint32(len(ct1))},
			{UID: 2, MessageID: msg2, Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: uint32(len(ct2))},
		},
		destMeta: []wsrpc.MessageMeta{
			{UID: 50, MessageID: msg1, Flags: []string{}, Modseq: 2, InternalDate: 1, CiphertextSize: uint32(len(ct1))},
			{UID: 51, MessageID: msg2, Flags: []string{}, Modseq: 2, InternalDate: 1, CiphertextSize: uint32(len(ct2))},
		},
		copyReply: wsrpc.CopyMessagesReply{
			DestUIDValidity: 9,
			Copied: []wsrpc.CopyPair{
				{SourceUID: 1, DestUID: 50},
				{SourceUID: 2, DestUID: 51},
			},
			DestHighestmodseq: 2,
		},
		cipherByID: map[string][]byte{string(msg1): ct1, string(msg2): ct2},
	}
	opener := &copyOpenerStub{table: map[string][]byte{string(ct1): pt1, string(ct2): pt2}}
	stubSealToRecipient(t)
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0xB1),
		actorMLSPubkey:  bytes32x(0xB9),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	if _, err := s.copy(context.Background(), imap.UIDSetNum(1, 2), "Junk", opener); err != nil {
		t.Fatalf("copy: %v", err)
	}
	if len(caller.putSpamCalls) != 2 {
		t.Fatalf("expected 2 put_spam_model writes, got %d", len(caller.putSpamCalls))
	}
	for _, put := range caller.putSpamCalls {
		tc := requireHistoryInsert(t, put)
		if tc.Mailbox != "Junk" {
			t.Errorf("mailbox: %q want Junk (the dest)", tc.Mailbox)
		}
		if tc.Label != "spam" {
			t.Errorf("label: %q want spam", tc.Label)
		}
		if tc.Source != "imap_junk_move" {
			t.Errorf("source: %q want imap_junk_move", tc.Source)
		}
	}
}

func TestCopyOutOfJunkFiresHamTrainSignal(t *testing.T) {
	// COPY from the canonical "Junk" mailbox into a non-\Junk mailbox
	// fires a label=ham training signal — the user implicitly classifies
	// the message as legitimate by relocating it out of Junk.
	msg := bytes32x(0xE1)
	pt := []byte("Subject: real\r\n\r\nlegit body")
	ct := sealedEnvelopeFixture(t, pt)
	opener := &copyOpenerStub{table: map[string][]byte{string(ct): pt}}
	stubSealToRecipient(t)
	caller := &copyCaller{
		srcMeta: []wsrpc.MessageMeta{
			{UID: 1, MessageID: msg, Flags: []string{"\\Junk"}, Modseq: 1, InternalDate: 1, CiphertextSize: uint32(len(ct))},
		},
		destMeta: []wsrpc.MessageMeta{
			{UID: 99, MessageID: msg, Flags: []string{}, Modseq: 2, InternalDate: 1, CiphertextSize: uint32(len(ct))},
		},
		copyReply: wsrpc.CopyMessagesReply{
			DestUIDValidity: 4,
			Copied: []wsrpc.CopyPair{
				{SourceUID: 1, DestUID: 99},
			},
			DestHighestmodseq: 2,
		},
		cipherByID: map[string][]byte{string(msg): ct},
	}
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0xB2),
		actorMLSPubkey:  bytes32x(0xB9),
		selectedMailbox: "Junk",
		logger:          slog.Default(),
	}
	if _, err := s.copy(context.Background(), imap.UIDSetNum(1), "Inbox", opener); err != nil {
		t.Fatalf("copy: %v", err)
	}
	if len(caller.putSpamCalls) != 1 {
		t.Fatalf("expected 1 put_spam_model write, got %d", len(caller.putSpamCalls))
	}
	ins := requireHistoryInsert(t, caller.putSpamCalls[0])
	if ins.Label != "ham" {
		t.Errorf("label: %q want ham", ins.Label)
	}
	if ins.Source != "imap_junk_move" {
		t.Errorf("source: %q want imap_junk_move", ins.Source)
	}
}

func TestCopyEmptyResultReturnsNilCopyData(t *testing.T) {
	// All source UIDs missing → nest returns empty `copied`. RFC 9051
	// §6.4.7 doesn't require an error; emersion writes a plain `OK
	// COPY completed` without a [COPYUID …] code. Returning nil
	// *imap.CopyData signals that to emersion.
	caller := &copyCaller{
		srcMeta: []wsrpc.MessageMeta{
			// Mailbox is empty — the UID 99 the client asked for isn't
			// here.
		},
		copyReply: wsrpc.CopyMessagesReply{
			DestUIDValidity:   3,
			Copied:            nil,
			DestHighestmodseq: 1,
		},
	}
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0xB3),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	data, err := s.copy(context.Background(), imap.UIDSetNum(99), "Archive", nil)
	if err != nil {
		t.Fatalf("copy: %v", err)
	}
	if data != nil {
		t.Errorf("expected nil *imap.CopyData on empty copied, got %+v", data)
	}
}

func TestCopyPropagatesNestError(t *testing.T) {
	// A transport-level RPC failure must propagate so emersion can
	// surface it as `NO COPY failed` (or BAD on a malformed request).
	caller := &copyCaller{
		srcMeta: []wsrpc.MessageMeta{
			{UID: 1, MessageID: bytes32x(0xF1), Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: 1},
		},
		copyErr: errors.New("synthetic-copy-failure"),
	}
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0xB4),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	_, err := s.copy(context.Background(), imap.UIDSetNum(1), "Archive", nil)
	if err == nil {
		t.Fatalf("expected error from synthetic-copy-failure")
	}
}

// TestCopyOverQuotaMapsToNoOverQuota: the nest copy handler rejects an
// over-quota destination with the typed `fauna.bridges.over_quota` error;
// the MDA must surface it as `NO [OVERQUOTA]` (imap-server.md § Quota
// enforcement points), not a bare NO.
func TestCopyOverQuotaMapsToNoOverQuota(t *testing.T) {
	payload, err := dagcbor.Marshal(map[string]any{"code": wsrpc.CodeOverQuota})
	if err != nil {
		t.Fatalf("marshal payload: %v", err)
	}
	caller := &copyCaller{
		srcMeta: []wsrpc.MessageMeta{
			{UID: 1, MessageID: bytes32x(0xF1), Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: 1},
		},
		copyErr: &wsrpc.ServerError{Payload: payload},
	}
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0xB5),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	_, err = s.copy(context.Background(), imap.UIDSetNum(1), "Archive", nil)
	var imapErr *imap.Error
	if !errors.As(err, &imapErr) {
		t.Fatalf("over-quota copy must yield *imap.Error, got %T: %v", err, err)
	}
	if imapErr.Code != imap.ResponseCodeOverQuota {
		t.Errorf("response code = %q, want OVERQUOTA", imapErr.Code)
	}
}
