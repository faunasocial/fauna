package imap

import (
	"context"
	"errors"
	"io"
	"log/slog"
	"testing"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// storeCaller is the per-file fake for the STORE path. It dispatches
// the RPCs Session.Store can fan out to:
//
//   - fauna.bridges.fetch_message_metadata: seq↔UID resolution +
//     enumerating the selected mailbox's UIDs.
//   - fauna.bridges.store_flags: the actual STORE write.
//   - fauna.bridges.fetch_message_ciphertext: encrypted body fetch for
//     the `\Junk` training signal path.
//   - fauna.bridges.fetch_spam_model / put_spam_model: the agent-side
//     train's model read and its sealed write-back (one per UID).
//
// By convention, every IMAP-layer test file declares its
// own caller stub so future drift on one test file's fake doesn't break
// the rest of the suite.
type storeCaller struct {
	// fixtures
	metaReplies []wsrpc.MessageMeta
	storeReply  wsrpc.StoreFlagsReply
	cipherByID  map[string][]byte
	// putSpamReplies answers put_spam_model per call, in order (the
	// one-lesson rule plays "duplicate_signal"); an absent or "" entry
	// omits the outcome key, the bare ack a nest from before it sends.
	putSpamReplies []string
	// spamModelBlob / spamModelStoredSealed answer fetch_spam_model.
	// Zero values (nil blob, false) model an untrained actor ⇒ the
	// agent-side train starts from an empty model. A non-nil blob with
	// stored_sealed=false models the nest's cold-start seed, which the
	// train path must never open or train on.
	spamModelBlob         []byte
	spamModelStoredSealed bool
	// spamModelFetchErr fails fetch_spam_model ⇒ the signal is skipped.
	spamModelFetchErr error
	// spamModelContributeBaseline / spamModelHolderSealTarget answer the
	// piece-(b4) write-signal fields on the same fetch_spam_model reply.
	// Zero values (false, nil) model the common not-opted-in case ⇒ no
	// holder copy attached.
	spamModelContributeBaseline bool
	spamModelHolderSealTarget   *wsrpc.HolderSealTarget

	// error injection
	storeErr error

	// captured state
	storeCalls    int
	capturedStore capturedStoreReq
	metaCalls     int
	ciphertextHit int
	putSpamCalls  []capturedPutSpam
}

// capturedPutSpam decodes the put_spam_model wire request the agent-side
// train ships (model re-seal + atomic sealed history row).
type capturedPutSpam struct {
	ActorID     []byte `cbor:"actor_id"`
	SealedModel []byte `cbor:"sealed_model"`
	SampleCount uint32 `cbor:"sample_count"`
	HistoryOp   *struct {
		Insert *capturedHistoryInsert `cbor:"Insert"`
	} `cbor:"history_op"`
	HolderCopy *wsrpc.SpamModelHolderCopy `cbor:"holder_copy"`
}

type capturedHistoryInsert struct {
	MessageID     []byte `cbor:"message_id"`
	Mailbox       string `cbor:"mailbox"`
	SealedSubject []byte `cbor:"sealed_subject"`
	SealedDelta   []byte `cbor:"sealed_delta"`
	Label         string `cbor:"label"`
	Source        string `cbor:"source"`
}

type capturedStoreReq struct {
	ActorID        []byte   `cbor:"actor_id"`
	Mailbox        string   `cbor:"mailbox"`
	UIDs           []uint32 `cbor:"uids"`
	Op             string   `cbor:"op"`
	Flags          []string `cbor:"flags"`
	UnchangedSince *int64   `cbor:"unchanged_since"`
}

// answerSpamModelRPC is the shared fetch_spam_model / put_spam_model
// half of the per-file IMAP fakes whose tests only need an untrained actor
// (nil blob ⇒ train from an empty model) and an always-"written" put.
// Reports whether it handled `method`; every put lands in `puts`.
func answerSpamModelRPC(method string, enc []byte, reply any, puts *[]capturedPutSpam) (bool, error) {
	switch method {
	case wsrpc.MethodFetchSpamModel:
		rep, _ := dagcbor.Marshal(map[string]any{"blob": nil, "stored_sealed": false})
		return true, cbor.Unmarshal(rep, reply)
	case wsrpc.MethodPutSpamModel:
		var p capturedPutSpam
		_ = cbor.Unmarshal(enc, &p)
		*puts = append(*puts, p)
		rep, _ := dagcbor.Marshal(map[string]any{"outcome": "written"})
		return true, cbor.Unmarshal(rep, reply)
	}
	return false, nil
}

// stubSealToRecipient swaps the package-level seal (the same seam Append
// tests use) for a recognizable `"sealed:" + plaintext` stub for the
// test's lifetime, returning the recorded seal inputs in call order.
func stubSealToRecipient(t *testing.T) *[][]byte {
	t.Helper()
	var inputs [][]byte
	orig := sealToRecipient
	sealToRecipient = func(pt, pubkey, mlkemEk []byte) ([]byte, error) {
		inputs = append(inputs, append([]byte(nil), pt...))
		return append([]byte("sealed:"), pt...), nil
	}
	t.Cleanup(func() { sealToRecipient = orig })
	return &inputs
}

// requireHistoryInsert returns the sealed history row a put_spam_model
// carried, failing the test when the write shipped without one.
func requireHistoryInsert(t *testing.T, put capturedPutSpam) *capturedHistoryInsert {
	t.Helper()
	if put.HistoryOp == nil || put.HistoryOp.Insert == nil {
		t.Fatalf("the sealed history row must ride the same put_spam_model (atomic)")
	}
	return put.HistoryOp.Insert
}

func (c *storeCaller) Call(_ context.Context, method string, body, reply any) error {
	enc, err := dagcbor.Marshal(body)
	if err != nil {
		return err
	}
	switch method {
	case wsrpc.MethodFetchMessageMetadata:
		c.metaCalls++
		rep, err := dagcbor.Marshal(struct {
			Messages []wsrpc.MessageMeta `cbor:"messages"`
		}{Messages: c.metaReplies})
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)

	case wsrpc.MethodStoreFlags:
		c.storeCalls++
		_ = cbor.Unmarshal(enc, &c.capturedStore)
		if c.storeErr != nil {
			return c.storeErr
		}
		rep, err := dagcbor.Marshal(c.storeReply)
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)

	case wsrpc.MethodFetchMessageCiphertext:
		c.ciphertextHit++
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

	case wsrpc.MethodFetchSpamModel:
		if c.spamModelFetchErr != nil {
			return c.spamModelFetchErr
		}
		rep, _ := dagcbor.Marshal(map[string]any{
			"blob":                c.spamModelBlob,
			"stored_sealed":       c.spamModelStoredSealed,
			"contribute_baseline": c.spamModelContributeBaseline,
			"holder_seal_target":  c.spamModelHolderSealTarget,
		})
		return cbor.Unmarshal(rep, reply)

	case wsrpc.MethodPutSpamModel:
		var p capturedPutSpam
		_ = cbor.Unmarshal(enc, &p)
		c.putSpamCalls = append(c.putSpamCalls, p)
		body := map[string]any{}
		if i := len(c.putSpamCalls) - 1; i < len(c.putSpamReplies) && c.putSpamReplies[i] != "" {
			body["outcome"] = c.putSpamReplies[i]
		}
		rep, _ := dagcbor.Marshal(body)
		return cbor.Unmarshal(rep, reply)
	}
	return errors.New("storeCaller: unexpected " + method)
}

// storeFetchWriterFake records every per-message FETCH response the
// STORE path writes. Satisfies fetchWriter from fetch.go so we can
// route Session.store through it without standing up a real
// imapserver.FetchWriter.
type storeFetchWriterFake struct {
	messages []*storeFetchResponseFake
}

func (w *storeFetchWriterFake) CreateMessage(seqNum uint32) fetchResponseWriter {
	m := &storeFetchResponseFake{seqNum: seqNum}
	w.messages = append(w.messages, m)
	return m
}

// WriteVanishedEarlier satisfies the fetchWriter seam; STORE never emits
// VANISHED, so it is unused here.
func (w *storeFetchWriterFake) WriteVanishedEarlier(imap.UIDSet) error { return nil }

type storeFetchResponseFake struct {
	seqNum    uint32
	uid       imap.UID
	uidSet    bool
	flags     []imap.Flag
	flagsSet  bool
	modSeq    uint64
	modSeqSet bool
	closed    bool
}

func (m *storeFetchResponseFake) WriteUID(u imap.UID)                   { m.uid = u; m.uidSet = true }
func (m *storeFetchResponseFake) WriteFlags(f []imap.Flag)              { m.flags = f; m.flagsSet = true }
func (m *storeFetchResponseFake) WriteModSeq(v uint64)                  { m.modSeq = v; m.modSeqSet = true }
func (m *storeFetchResponseFake) WriteInternalDate(time.Time)           {}
func (m *storeFetchResponseFake) WriteRFC822Size(int64)                 {}
func (m *storeFetchResponseFake) WriteBodyStructure(imap.BodyStructure) {}
func (m *storeFetchResponseFake) WriteEnvelope(*imap.Envelope)          {}
func (m *storeFetchResponseFake) WriteBodySection(*imap.FetchItemBodySection, int64) io.WriteCloser {
	return nopCloser{}
}
func (m *storeFetchResponseFake) WriteBinarySection(*imap.FetchItemBinarySection, int64) io.WriteCloser {
	return nopCloser{}
}
func (m *storeFetchResponseFake) WriteBinarySectionSize(*imap.FetchItemBinarySectionSize, uint32) {}
func (m *storeFetchResponseFake) Close() error                                                    { m.closed = true; return nil }

type nopCloser struct{}

func (nopCloser) Write(p []byte) (int, error) { return len(p), nil }
func (nopCloser) Close() error                { return nil }

// storeOpenerStub is a content-keyed stub (`mailfauna.RecordOpener`):
// lookup envelope bytes → plaintext bytes. Per-file fake (not the shared
// stub in fetch_body_test.go) so future drift on the body-fetch fake
// doesn't take D.2 down with it. BODY fixtures are real sealed envelopes
// (build via sealedEnvelopeFixture) because a mail record rests sealed and
// `mailfauna.OpenStoredRecord` refuses an unsealed payload; the sealed
// spam MODEL reaches it unconditionally (STRICT `Open`, no shape
// check), so model fixtures can stay arbitrary bytes.
type storeOpenerStub struct {
	table map[string][]byte
	hits  int
}

func (d *storeOpenerStub) Open(envelope []byte) ([]byte, error) {
	d.hits++
	pt, ok := d.table[string(envelope)]
	if !ok {
		return nil, errors.New("storeOpenerStub: no mapping for envelope")
	}
	// Hand the caller a fresh copy: Session.Store zeroizes the
	// plaintext after the train RPC, which would poison our fixture
	// table on subsequent calls if we returned the shared slice.
	// The production opener returns fresh bytes per call.
	out := make([]byte, len(pt))
	copy(out, pt)
	return out, nil
}

// ── Tests ────────────────────────────────────────────────────────────────

func TestStoreAddSeenEmitsFetchResponse(t *testing.T) {
	msg1ID := bytes32x(0xA1)
	msg2ID := bytes32x(0xA2)
	caller := &storeCaller{
		metaReplies: []wsrpc.MessageMeta{
			{UID: 1, MessageID: msg1ID, Flags: []string{}, Modseq: 1, InternalDate: 1_700_000_001, CiphertextSize: 100},
			{UID: 2, MessageID: msg2ID, Flags: []string{}, Modseq: 1, InternalDate: 1_700_000_002, CiphertextSize: 100},
		},
		storeReply: wsrpc.StoreFlagsReply{
			Updated: []wsrpc.StoreFlagsResultEntry{
				{UID: 1, Flags: []string{"\\Seen"}, ModSeq: 11},
				{UID: 2, Flags: []string{"\\Seen"}, ModSeq: 11},
			},
			HighestModSeq: 11,
		},
	}
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0x90),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	w := &storeFetchWriterFake{}
	err := s.store(context.Background(), w, imap.UIDSetNum(1, 2),
		&imap.StoreFlags{Op: imap.StoreFlagsAdd, Flags: []imap.Flag{"\\Seen"}},
		nil, nil)
	if err != nil {
		t.Fatalf("store: %v", err)
	}
	if caller.storeCalls != 1 {
		t.Errorf("store_calls = %d, want 1", caller.storeCalls)
	}
	if caller.capturedStore.Op != "add" {
		t.Errorf("op: %q want add", caller.capturedStore.Op)
	}
	if caller.capturedStore.UnchangedSince != nil {
		t.Errorf("unchanged_since must be nil, got %v", caller.capturedStore.UnchangedSince)
	}
	if len(w.messages) != 2 {
		t.Fatalf("expected 2 FETCH responses, got %d", len(w.messages))
	}
	if !w.messages[0].uidSet || w.messages[0].uid != 1 {
		t.Errorf("message[0] uid: %v want 1", w.messages[0].uid)
	}
	if !w.messages[1].uidSet || w.messages[1].uid != 2 {
		t.Errorf("message[1] uid: %v want 2", w.messages[1].uid)
	}
	if !w.messages[0].flagsSet || len(w.messages[0].flags) != 1 || w.messages[0].flags[0] != "\\Seen" {
		t.Errorf("message[0] flags: %v want [\\Seen]", w.messages[0].flags)
	}
	if !w.messages[0].closed || !w.messages[1].closed {
		t.Errorf("FETCH writers must be closed")
	}
	if len(caller.putSpamCalls) != 0 {
		t.Errorf("no train signal expected on \\Seen STORE, got %d", len(caller.putSpamCalls))
	}
}

// TestStoreEmitsModSeqWhenCondStore — with CONDSTORE enabled, each STORE
// FETCH response carries the per-UID post-store MODSEQ (RFC 7162 §3.1.4;
// imap-server.md:152). Without CONDSTORE it must be omitted.
func TestStoreEmitsModSeqWhenCondStore(t *testing.T) {
	caller := &storeCaller{
		metaReplies: []wsrpc.MessageMeta{
			{UID: 1, MessageID: bytes32x(0xC1), Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: 1},
		},
		storeReply: wsrpc.StoreFlagsReply{
			Updated:       []wsrpc.StoreFlagsResultEntry{{UID: 1, Flags: []string{"\\Seen"}, ModSeq: 11}},
			HighestModSeq: 11,
		},
	}
	// CONDSTORE on → MODSEQ emitted.
	s := &Session{client: caller, actorID: bytes32x(0x90), selectedMailbox: "INBOX", logger: slog.Default(), condStoreEnabled: true}
	w := &storeFetchWriterFake{}
	if err := s.store(context.Background(), w, imap.UIDSetNum(1),
		&imap.StoreFlags{Op: imap.StoreFlagsAdd, Flags: []imap.Flag{"\\Seen"}}, nil, nil); err != nil {
		t.Fatalf("store: %v", err)
	}
	if len(w.messages) != 1 {
		t.Fatalf("messages: %d", len(w.messages))
	}
	if !w.messages[0].modSeqSet || w.messages[0].modSeq != 11 {
		t.Errorf("STORE MODSEQ: set=%v val=%d, want 11", w.messages[0].modSeqSet, w.messages[0].modSeq)
	}

	// CONDSTORE off → no MODSEQ.
	s2 := &Session{client: caller, actorID: bytes32x(0x90), selectedMailbox: "INBOX", logger: slog.Default()}
	w2 := &storeFetchWriterFake{}
	if err := s2.store(context.Background(), w2, imap.UIDSetNum(1),
		&imap.StoreFlags{Op: imap.StoreFlagsAdd, Flags: []imap.Flag{"\\Seen"}}, nil, nil); err != nil {
		t.Fatalf("store (no condstore): %v", err)
	}
	if w2.messages[0].modSeqSet {
		t.Errorf("STORE must not emit MODSEQ without CONDSTORE")
	}
}

func TestStoreUnchangedSinceEmitsModifiedCode(t *testing.T) {
	caller := &storeCaller{
		metaReplies: []wsrpc.MessageMeta{
			{UID: 1, MessageID: bytes32x(0xB1), Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: 1},
			{UID: 2, MessageID: bytes32x(0xB2), Flags: []string{}, Modseq: 5, InternalDate: 1, CiphertextSize: 1},
		},
		storeReply: wsrpc.StoreFlagsReply{
			Updated: []wsrpc.StoreFlagsResultEntry{
				{UID: 1, Flags: []string{"\\Seen"}, ModSeq: 6},
			},
			HighestModSeq: 6,
			Modified:      []uint32{2},
		},
	}
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0x91),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	w := &storeFetchWriterFake{}
	err := s.store(context.Background(), w, imap.UIDSetNum(1, 2),
		&imap.StoreFlags{Op: imap.StoreFlagsAdd, Flags: []imap.Flag{"\\Seen"}},
		&imap.StoreOptions{UnchangedSince: 3},
		nil)

	// The MODIFIED partition is emitted as a typed imap.Error with
	// Type=OK so emersion writes `<tag> OK [MODIFIED <uids>] STORE
	// completed` as the tagged response per RFC 7162 §3.1.3.
	if err == nil {
		t.Fatalf("expected MODIFIED imap.Error, got nil")
	}
	var imapErr *imap.Error
	if !errors.As(err, &imapErr) {
		t.Fatalf("expected *imap.Error, got %T: %v", err, err)
	}
	if imapErr.Type != imap.StatusResponseTypeOK {
		t.Errorf("err.Type: %v want StatusResponseTypeOK", imapErr.Type)
	}
	// RFC 7162 §3.1.3: the stale uid-set rides inside the response code,
	// `[MODIFIED <uid-set>]`, not the free text — so a CONDSTORE-aware MUA
	// can parse which messages to refetch+retry. uid 2 is the modseq-stale
	// member of {1,2} here.
	if string(imapErr.Code) != "MODIFIED 2" {
		t.Errorf("err.Code: %q want `MODIFIED 2` (uid-set in the response code)", imapErr.Code)
	}

	// Wire-side: unchanged_since must be sent on the store_flags RPC.
	if caller.capturedStore.UnchangedSince == nil || *caller.capturedStore.UnchangedSince != 3 {
		t.Errorf("unchanged_since: %v want 3", caller.capturedStore.UnchangedSince)
	}
	// The applied UID still gets a FETCH response.
	if len(w.messages) != 1 || w.messages[0].uid != 1 {
		t.Errorf("expected one FETCH for uid 1, got %+v", w.messages)
	}
}

func TestStoreMissingUIDsSilentlySkipped(t *testing.T) {
	// Per RFC 9051 §6.4.6: STORE on a UID that doesn't exist is silently
	// dropped. nest's handler already returns an empty `updated` for
	// such UIDs; Session.Store must not error.
	caller := &storeCaller{
		metaReplies: []wsrpc.MessageMeta{
			// UID 99 is not in the mailbox.
		},
		storeReply: wsrpc.StoreFlagsReply{
			Updated:       []wsrpc.StoreFlagsResultEntry{},
			HighestModSeq: 1,
		},
	}
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0x92),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	w := &storeFetchWriterFake{}
	err := s.store(context.Background(), w, imap.UIDSetNum(99),
		&imap.StoreFlags{Op: imap.StoreFlagsAdd, Flags: []imap.Flag{"\\Seen"}},
		nil, nil)
	if err != nil {
		t.Fatalf("store: %v", err)
	}
	if len(w.messages) != 0 {
		t.Errorf("expected no FETCH responses, got %d", len(w.messages))
	}
}

// An UNTRAINED actor (fetch_spam_model: no blob) trains agent-side from an
// EMPTY model: the FFI trains a fresh model, the MDA re-seals it to the
// actor's own key and writes it back with the sealed history row via
// put_spam_model. There is no server-side train to relay to.
func TestStoreAddJunkUntrainedActorTrainsFromEmptyModel(t *testing.T) {
	msgID := bytes32x(0xC1)
	// A real sealed envelope, as a mail record rests sealed: the body
	// opens through the opener stub (a raw fixture would be refused).
	ciphertext := sealedEnvelopeFixture(t, []byte("payload-spam-1"))
	plaintext := []byte("Subject: free crypto\r\n\r\nclick here!")
	caller := &storeCaller{
		metaReplies: []wsrpc.MessageMeta{
			{UID: 1, MessageID: msgID, Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: uint32(len(ciphertext))},
		},
		storeReply: wsrpc.StoreFlagsReply{
			Updated: []wsrpc.StoreFlagsResultEntry{
				{UID: 1, Flags: []string{"\\Junk"}, ModSeq: 2},
			},
			HighestModSeq: 2,
		},
		cipherByID: map[string][]byte{string(msgID): ciphertext},
	}
	opener := &storeOpenerStub{table: map[string][]byte{string(ciphertext): plaintext}}
	sealedInputs := stubSealToRecipient(t)
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0x93),
		actorMLSPubkey:  bytes32x(0x95),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	w := &storeFetchWriterFake{}
	err := s.store(context.Background(), w, imap.UIDSetNum(1),
		&imap.StoreFlags{Op: imap.StoreFlagsAdd, Flags: []imap.Flag{"\\Junk"}},
		nil, opener)
	if err != nil {
		t.Fatalf("store: %v", err)
	}
	if len(caller.putSpamCalls) != 1 {
		t.Fatalf("expected 1 put_spam_model write, got %d", len(caller.putSpamCalls))
	}
	put := caller.putSpamCalls[0]
	if string(put.ActorID) != string(bytes32x(0x93)) {
		t.Errorf("put actor_id must name the served actor")
	}
	fresh := mailfauna.ApplySpamTraining(nil, string(plaintext), true).NewModelBytes
	if string(put.SealedModel) != "sealed:"+string(fresh) {
		t.Errorf("sealed_model must be a FRESH model trained once on the body, re-sealed; got %q", put.SealedModel)
	}
	ins := requireHistoryInsert(t, put)
	if ins.Mailbox != "INBOX" || ins.Label != "spam" || ins.Source != "imap_junk_flag" {
		t.Errorf("insert row: %+v", ins)
	}
	if string(ins.MessageID) != string(msgID) {
		t.Errorf("message_id mismatch")
	}
	if len(ins.SealedSubject) == 0 || len(ins.SealedDelta) == 0 {
		t.Errorf("subject + delta must ship sealed (non-empty ciphertext)")
	}
	if len(*sealedInputs) != 3 || string((*sealedInputs)[1]) != "free crypto" {
		t.Errorf("expected 3 seals (model, subject \"free crypto\", delta), got %q", *sealedInputs)
	}
	// Only the body was opened — there was no stored model to open.
	if opener.hits != 1 {
		t.Errorf("opener hits = %d, want 1 (the body only)", opener.hits)
	}
}

// The cold-start seed (fetch_spam_model: a blob with stored_sealed=false —
// the deployment baseline folded onto a fresh model, sealed on read) is
// NEVER trained on: the fold is read-time only (mail-spam.md § Cold start
// Path 2). The train starts from an EMPTY model and the seed is never even
// opened.
func TestStoreAddJunkColdStartSeedIsNeverTrainedOn(t *testing.T) {
	msgID := bytes32x(0xC3)
	ciphertext := sealedEnvelopeFixture(t, []byte("payload-seed-1"))
	plaintext := []byte("Subject: free crypto\r\n\r\nclick here!")
	seedEnvelope := []byte("cold-start-seed-envelope")
	caller := &storeCaller{
		metaReplies: []wsrpc.MessageMeta{
			{UID: 1, MessageID: msgID, Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: uint32(len(ciphertext))},
		},
		storeReply: wsrpc.StoreFlagsReply{
			Updated:       []wsrpc.StoreFlagsResultEntry{{UID: 1, Flags: []string{"\\Junk"}, ModSeq: 2}},
			HighestModSeq: 2,
		},
		cipherByID:            map[string][]byte{string(msgID): ciphertext},
		spamModelBlob:         seedEnvelope,
		spamModelStoredSealed: false,
	}
	// The seed would open to a model with prior training; if the train path
	// ever opened it, the write-back would not be the fresh model below.
	seeded := mailfauna.ApplySpamTraining(nil, "baseline prior text", false).NewModelBytes
	opener := &storeOpenerStub{table: map[string][]byte{
		string(ciphertext):   plaintext,
		string(seedEnvelope): seeded,
	}}
	stubSealToRecipient(t)
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0x93),
		actorMLSPubkey:  bytes32x(0x95),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	w := &storeFetchWriterFake{}
	if err := s.store(context.Background(), w, imap.UIDSetNum(1),
		&imap.StoreFlags{Op: imap.StoreFlagsAdd, Flags: []imap.Flag{"\\Junk"}},
		nil, opener); err != nil {
		t.Fatalf("store: %v", err)
	}
	if len(caller.putSpamCalls) != 1 {
		t.Fatalf("expected 1 put_spam_model write, got %d", len(caller.putSpamCalls))
	}
	fresh := mailfauna.ApplySpamTraining(nil, string(plaintext), true).NewModelBytes
	if got := caller.putSpamCalls[0].SealedModel; string(got) != "sealed:"+string(fresh) {
		t.Fatalf("the cold-start seed was trained on / persisted; want a fresh model trained once, got %q", got)
	}
	if opener.hits != 1 {
		t.Errorf("opener hits = %d, want 1 (the body only — the seed is never opened)", opener.hits)
	}
}

// A fetch_spam_model failure skips the whole signal: no body is fetched and
// nothing is written (best-effort; there is no server-side fallback).
func TestStoreAddJunkModelFetchErrorSkipsTheSignal(t *testing.T) {
	msgID := bytes32x(0xC4)
	ciphertext := sealedEnvelopeFixture(t, []byte("payload-fetch-err"))
	caller := &storeCaller{
		metaReplies: []wsrpc.MessageMeta{
			{UID: 1, MessageID: msgID, Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: uint32(len(ciphertext))},
		},
		storeReply: wsrpc.StoreFlagsReply{
			Updated:       []wsrpc.StoreFlagsResultEntry{{UID: 1, Flags: []string{"\\Junk"}, ModSeq: 2}},
			HighestModSeq: 2,
		},
		cipherByID:        map[string][]byte{string(msgID): ciphertext},
		spamModelFetchErr: errors.New("synthetic fetch_spam_model failure"),
	}
	opener := &storeOpenerStub{table: map[string][]byte{string(ciphertext): []byte("Subject: x\r\n\r\ny")}}
	stubSealToRecipient(t)
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0x93),
		actorMLSPubkey:  bytes32x(0x95),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	w := &storeFetchWriterFake{}
	if err := s.store(context.Background(), w, imap.UIDSetNum(1),
		&imap.StoreFlags{Op: imap.StoreFlagsAdd, Flags: []imap.Flag{"\\Junk"}},
		nil, opener); err != nil {
		t.Fatalf("the STORE itself must still succeed: %v", err)
	}
	if len(caller.putSpamCalls) != 0 {
		t.Errorf("no write expected after a fetch_spam_model failure, got %d", len(caller.putSpamCalls))
	}
	if caller.ciphertextHit != 0 {
		t.Errorf("no body fetch expected after a fetch_spam_model failure, got %d", caller.ciphertextHit)
	}
}

// A stored (sealed-at-rest) model (fetch_spam_model says
// stored_sealed=true): a +\\Junk STORE opens it, mutates it via the
// shared FFI, re-seals, and writes it back via put_spam_model with the
// atomic sealed history row.
func TestStoreAddJunkOnSealedModelTrainsAgentSide(t *testing.T) {
	msgID := bytes32x(0xC7)
	// The body must be a real sealed envelope to route through the opener
	// stub; the MODEL rail is a STRICT `Open` with no shape check, so its
	// fixture can stay arbitrary bytes.
	ciphertext := sealedEnvelopeFixture(t, []byte("payload-sealed-train-1"))
	plaintext := []byte("Subject: free crypto\r\n\r\nclick here!")
	sealedModelEnvelope := []byte("sealed-model-envelope")
	caller := &storeCaller{
		metaReplies: []wsrpc.MessageMeta{
			{UID: 1, MessageID: msgID, Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: uint32(len(ciphertext))},
		},
		storeReply: wsrpc.StoreFlagsReply{
			Updated: []wsrpc.StoreFlagsResultEntry{
				{UID: 1, Flags: []string{"\\Junk"}, ModSeq: 2},
			},
			HighestModSeq: 2,
		},
		cipherByID:            map[string][]byte{string(msgID): ciphertext},
		spamModelBlob:         sealedModelEnvelope,
		spamModelStoredSealed: true,
	}
	opener := &storeOpenerStub{table: map[string][]byte{
		string(ciphertext): plaintext,
		// The opened model: empty bytes ⇒ the FFI trains onto a fresh
		// model (the load_or_create mirror), which is all this path needs.
		string(sealedModelEnvelope): {},
	}}
	// Stub the package-level seal (the same seam Append tests use): record
	// what got sealed and return recognizable non-empty ciphertexts.
	var sealedInputs [][]byte
	origSeal := sealToRecipient
	sealToRecipient = func(pt, pubkey, mlkemEk []byte) ([]byte, error) {
		sealedInputs = append(sealedInputs, append([]byte(nil), pt...))
		return append([]byte("sealed:"), pt...), nil
	}
	t.Cleanup(func() { sealToRecipient = origSeal })

	s := &Session{
		client:          caller,
		actorID:         bytes32x(0x94),
		actorMLSPubkey:  bytes32x(0x95),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	w := &storeFetchWriterFake{}
	err := s.store(context.Background(), w, imap.UIDSetNum(1),
		&imap.StoreFlags{Op: imap.StoreFlagsAdd, Flags: []imap.Flag{"\\Junk"}},
		nil, opener)
	if err != nil {
		t.Fatalf("store: %v", err)
	}
	if len(caller.putSpamCalls) != 1 {
		t.Fatalf("expected 1 put_spam_model write-back, got %d", len(caller.putSpamCalls))
	}
	put := caller.putSpamCalls[0]
	if string(put.ActorID) != string(bytes32x(0x94)) {
		t.Errorf("put actor_id must name the served actor")
	}
	if len(put.SealedModel) == 0 || string(put.SealedModel[:7]) != "sealed:" {
		t.Errorf("sealed_model must be the re-sealed mutated model, got %q", put.SealedModel)
	}
	if put.HistoryOp == nil || put.HistoryOp.Insert == nil {
		t.Fatalf("the sealed history row must ride the same request (atomic)")
	}
	ins := put.HistoryOp.Insert
	if ins.Mailbox != "INBOX" || ins.Label != "spam" || ins.Source != "imap_junk_flag" {
		t.Errorf("insert row: %+v", ins)
	}
	if string(ins.MessageID) != string(msgID) {
		t.Errorf("message_id mismatch")
	}
	if len(ins.SealedSubject) == 0 || len(ins.SealedDelta) == 0 {
		t.Errorf("subject + delta must ship sealed (non-empty ciphertext)")
	}
	// Three seals: model, subject, delta — subject plaintext is the parsed
	// RFC-2047 subject, the delta is the FFI's forward n-gram JSON.
	if len(sealedInputs) != 3 {
		t.Fatalf("expected 3 seals (model, subject, delta), got %d", len(sealedInputs))
	}
	if string(sealedInputs[1]) != "free crypto" {
		t.Errorf("sealed subject plaintext: got %q, want the parsed Subject", sealedInputs[1])
	}
	if put.HolderCopy != nil {
		t.Errorf("not opted into the baseline ⇒ no holder copy attached, got %+v", put.HolderCopy)
	}
}

// The one-lesson rule (mail-spam.md § 3) on the sealed path: when the nest
// answers a put_spam_model with `duplicate_signal`, it wrote nothing, so the
// batch must NOT carry that UID's mutation into the next UID — the second
// write is the LAST ACCEPTED model plus one lesson, not the discarded
// mutation plus one. (The stub answers every metadata fetch with the same
// message, so both UIDs train the same text: a carried-forward mutation
// would show as the text trained twice.)
func TestStoreAddJunkOnSealedModelDuplicateKeepsTheLastAcceptedModel(t *testing.T) {
	msgID := bytes32x(0xC9)
	ciphertext := sealedEnvelopeFixture(t, []byte("payload-sealed-train-dup"))
	plaintext := []byte("Subject: free crypto\r\n\r\nclick here!")
	sealedModelEnvelope := []byte("sealed-model-envelope-dup")
	caller := &storeCaller{
		metaReplies: []wsrpc.MessageMeta{
			{UID: 1, MessageID: msgID, Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: uint32(len(ciphertext))},
		},
		storeReply: wsrpc.StoreFlagsReply{
			Updated: []wsrpc.StoreFlagsResultEntry{
				{UID: 1, Flags: []string{"\\Junk"}, ModSeq: 2},
				{UID: 2, Flags: []string{"\\Junk"}, ModSeq: 2},
			},
			HighestModSeq: 2,
		},
		cipherByID:            map[string][]byte{string(msgID): ciphertext},
		spamModelBlob:         sealedModelEnvelope,
		spamModelStoredSealed: true,
		// UID 1's write is a duplicate (nothing written); UID 2's lands.
		putSpamReplies: []string{"duplicate_signal", "written"},
	}
	opener := &storeOpenerStub{table: map[string][]byte{
		string(ciphertext):          plaintext,
		string(sealedModelEnvelope): {},
	}}
	origSeal := sealToRecipient
	sealToRecipient = func(pt, pubkey, mlkemEk []byte) ([]byte, error) {
		return append([]byte("sealed:"), pt...), nil
	}
	t.Cleanup(func() { sealToRecipient = origSeal })

	s := &Session{
		client:          caller,
		actorID:         bytes32x(0x94),
		actorMLSPubkey:  bytes32x(0x95),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	w := &storeFetchWriterFake{}
	err := s.store(context.Background(), w, imap.UIDSetNum(1, 2),
		&imap.StoreFlags{Op: imap.StoreFlagsAdd, Flags: []imap.Flag{"\\Junk"}},
		nil, opener)
	if err != nil {
		t.Fatalf("store: %v", err)
	}
	if len(caller.putSpamCalls) != 2 {
		t.Fatalf("expected 2 put_spam_model write-backs (one per UID), got %d", len(caller.putSpamCalls))
	}
	// What the second write must carry: the FRESH model (the last accepted
	// state — the duplicate's mutation was discarded) trained on the text
	// ONCE.
	once := mailfauna.ApplySpamTraining(nil, string(plaintext), true).NewModelBytes
	twice := mailfauna.ApplySpamTraining(once, string(plaintext), true).NewModelBytes
	if string(twice) == string(once) {
		t.Fatalf("precondition: training the same text twice must change the model")
	}
	got := caller.putSpamCalls[1].SealedModel
	if string(got) != "sealed:"+string(once) {
		if string(got) == "sealed:"+string(twice) {
			t.Fatalf("the duplicate's mutation was carried into the next UID (the text trained twice)")
		}
		t.Fatalf("second write-back is neither the once- nor the twice-trained model: %q", got)
	}
}

// Piece (b4): opted-in + a holder enrolled ⇒ every sealed-model re-seal
// also attaches a fresh holder copy to the SAME put_spam_model write (the
// re-seal-on-every-write rule, mail-spam.md § Encrypted-mode interaction).
func TestStoreAddJunkOnSealedModelAttachesHolderCopy(t *testing.T) {
	msgID := bytes32x(0xC8)
	ciphertext := sealedEnvelopeFixture(t, []byte("payload-sealed-train-2"))
	plaintext := []byte("Subject: free crypto\r\n\r\nclick here!")
	sealedModelEnvelope := []byte("sealed-model-envelope-2")
	holderPubkey := bytes32x(0x96)
	caller := &storeCaller{
		metaReplies: []wsrpc.MessageMeta{
			{UID: 1, MessageID: msgID, Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: uint32(len(ciphertext))},
		},
		storeReply: wsrpc.StoreFlagsReply{
			Updated: []wsrpc.StoreFlagsResultEntry{
				{UID: 1, Flags: []string{"\\Junk"}, ModSeq: 2},
			},
			HighestModSeq: 2,
		},
		cipherByID:                  map[string][]byte{string(msgID): ciphertext},
		spamModelBlob:               sealedModelEnvelope,
		spamModelStoredSealed:       true,
		spamModelContributeBaseline: true,
		spamModelHolderSealTarget:   &wsrpc.HolderSealTarget{X25519Pubkey: holderPubkey},
	}
	opener := &storeOpenerStub{table: map[string][]byte{
		string(ciphertext):          plaintext,
		string(sealedModelEnvelope): {},
	}}
	origSeal := sealToRecipient
	sealToRecipient = func(pt, pubkey, mlkemEk []byte) ([]byte, error) {
		return append([]byte("sealed:"), pt...), nil
	}
	t.Cleanup(func() { sealToRecipient = origSeal })

	var copyCalls [][]byte
	origSealCopy := sealSpamModelCopy
	sealSpamModelCopy = func(modelBytes, ownerActorID, holderX25519Pubkey, holderMlkemEk []byte) ([]byte, error) {
		copyCalls = append(copyCalls, append([]byte(nil), holderX25519Pubkey...))
		return append([]byte("holder-copy:"), modelBytes...), nil
	}
	t.Cleanup(func() { sealSpamModelCopy = origSealCopy })

	s := &Session{
		client:          caller,
		actorID:         bytes32x(0x94),
		actorMLSPubkey:  bytes32x(0x95),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	w := &storeFetchWriterFake{}
	err := s.store(context.Background(), w, imap.UIDSetNum(1),
		&imap.StoreFlags{Op: imap.StoreFlagsAdd, Flags: []imap.Flag{"\\Junk"}},
		nil, opener)
	if err != nil {
		t.Fatalf("store: %v", err)
	}
	if len(copyCalls) != 1 {
		t.Fatalf("expected 1 holder-copy seal, got %d", len(copyCalls))
	}
	if string(copyCalls[0]) != string(holderPubkey) {
		t.Errorf("holder-copy seal target: got %x, want %x", copyCalls[0], holderPubkey)
	}
	if len(caller.putSpamCalls) != 1 {
		t.Fatalf("expected 1 put_spam_model write-back, got %d", len(caller.putSpamCalls))
	}
	put := caller.putSpamCalls[0]
	if put.HolderCopy == nil {
		t.Fatalf("opted-in + holder enrolled ⇒ holder_copy must ride the same write")
	}
	if string(put.HolderCopy.HolderPubkey) != string(holderPubkey) {
		t.Errorf("holder_copy.holder_pubkey: got %x, want %x", put.HolderCopy.HolderPubkey, holderPubkey)
	}
	if len(put.HolderCopy.SealedCopy) == 0 || string(put.HolderCopy.SealedCopy[:12]) != "holder-copy:" {
		t.Errorf("holder_copy.sealed_copy must be the stubbed seal output, got %q", put.HolderCopy.SealedCopy)
	}
}

// Piece (b4): a holder-copy seal FAILURE is non-fatal — the model write
// itself (to the actor's OWN key) still completes; only the copy is
// omitted this write (freshness ground 5 — a v-older/failed writer merely
// leaves the previously-stored copy stale for the next publish).
func TestStoreAddJunkHolderCopySealFailureIsNonFatal(t *testing.T) {
	msgID := bytes32x(0xC9)
	ciphertext := sealedEnvelopeFixture(t, []byte("payload-sealed-train-3"))
	plaintext := []byte("Subject: free crypto\r\n\r\nclick here!")
	sealedModelEnvelope := []byte("sealed-model-envelope-3")
	holderPubkey := bytes32x(0x97)
	caller := &storeCaller{
		metaReplies: []wsrpc.MessageMeta{
			{UID: 1, MessageID: msgID, Flags: []string{}, Modseq: 1, InternalDate: 1, CiphertextSize: uint32(len(ciphertext))},
		},
		storeReply: wsrpc.StoreFlagsReply{
			Updated: []wsrpc.StoreFlagsResultEntry{
				{UID: 1, Flags: []string{"\\Junk"}, ModSeq: 2},
			},
			HighestModSeq: 2,
		},
		cipherByID:                  map[string][]byte{string(msgID): ciphertext},
		spamModelBlob:               sealedModelEnvelope,
		spamModelStoredSealed:       true,
		spamModelContributeBaseline: true,
		spamModelHolderSealTarget:   &wsrpc.HolderSealTarget{X25519Pubkey: holderPubkey},
	}
	opener := &storeOpenerStub{table: map[string][]byte{
		string(ciphertext):          plaintext,
		string(sealedModelEnvelope): {},
	}}
	origSeal := sealToRecipient
	sealToRecipient = func(pt, pubkey, mlkemEk []byte) ([]byte, error) {
		return append([]byte("sealed:"), pt...), nil
	}
	t.Cleanup(func() { sealToRecipient = origSeal })

	origSealCopy := sealSpamModelCopy
	sealSpamModelCopy = func(modelBytes, ownerActorID, holderX25519Pubkey, holderMlkemEk []byte) ([]byte, error) {
		return nil, errors.New("synthetic holder-copy seal failure")
	}
	t.Cleanup(func() { sealSpamModelCopy = origSealCopy })

	s := &Session{
		client:          caller,
		actorID:         bytes32x(0x94),
		actorMLSPubkey:  bytes32x(0x95),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	w := &storeFetchWriterFake{}
	err := s.store(context.Background(), w, imap.UIDSetNum(1),
		&imap.StoreFlags{Op: imap.StoreFlagsAdd, Flags: []imap.Flag{"\\Junk"}},
		nil, opener)
	if err != nil {
		t.Fatalf("store: %v", err)
	}
	if len(caller.putSpamCalls) != 1 {
		t.Fatalf("model write must still complete despite the copy-seal failure, got %d put calls", len(caller.putSpamCalls))
	}
	if caller.putSpamCalls[0].HolderCopy != nil {
		t.Errorf("a failed copy-seal must attach no holder_copy, got %+v", caller.putSpamCalls[0].HolderCopy)
	}
}

func TestStoreRemoveJunkFiresHamTrainSignal(t *testing.T) {
	msgID := bytes32x(0xC2)
	ciphertext := sealedEnvelopeFixture(t, []byte("payload-ham-1"))
	plaintext := []byte("Subject: real mail\r\n\r\nhi there")
	caller := &storeCaller{
		metaReplies: []wsrpc.MessageMeta{
			{UID: 1, MessageID: msgID, Flags: []string{"\\Junk"}, Modseq: 1, InternalDate: 1, CiphertextSize: uint32(len(ciphertext))},
		},
		storeReply: wsrpc.StoreFlagsReply{
			Updated: []wsrpc.StoreFlagsResultEntry{
				{UID: 1, Flags: []string{}, ModSeq: 2},
			},
			HighestModSeq: 2,
		},
		cipherByID: map[string][]byte{string(msgID): ciphertext},
	}
	opener := &storeOpenerStub{table: map[string][]byte{string(ciphertext): plaintext}}
	stubSealToRecipient(t)
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0x94),
		actorMLSPubkey:  bytes32x(0x95),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	w := &storeFetchWriterFake{}
	err := s.store(context.Background(), w, imap.UIDSetNum(1),
		&imap.StoreFlags{Op: imap.StoreFlagsDel, Flags: []imap.Flag{"\\Junk"}},
		nil, opener)
	if err != nil {
		t.Fatalf("store: %v", err)
	}
	if len(caller.putSpamCalls) != 1 {
		t.Fatalf("expected 1 put_spam_model write, got %d", len(caller.putSpamCalls))
	}
	ins := requireHistoryInsert(t, caller.putSpamCalls[0])
	if ins.Label != "ham" {
		t.Errorf("label: %q want ham", ins.Label)
	}
	if ins.Source != "imap_junk_flag" {
		t.Errorf("source: %q want imap_junk_flag", ins.Source)
	}
	fresh := mailfauna.ApplySpamTraining(nil, string(plaintext), false).NewModelBytes
	if string(caller.putSpamCalls[0].SealedModel) != "sealed:"+string(fresh) {
		t.Errorf("sealed_model must be a fresh model trained ham once")
	}
}

func TestStoreNonJunkFlagDoesNotFireTrainSignal(t *testing.T) {
	// STORE +\Seen on a message that already has \Junk must NOT fire
	// a training signal — only the flag we actually toggle matters.
	caller := &storeCaller{
		metaReplies: []wsrpc.MessageMeta{
			{UID: 1, MessageID: bytes32x(0xD1), Flags: []string{"\\Junk"}, Modseq: 1, InternalDate: 1, CiphertextSize: 1},
		},
		storeReply: wsrpc.StoreFlagsReply{
			Updated: []wsrpc.StoreFlagsResultEntry{
				{UID: 1, Flags: []string{"\\Junk", "\\Seen"}, ModSeq: 2},
			},
			HighestModSeq: 2,
		},
	}
	s := &Session{
		client:          caller,
		actorID:         bytes32x(0x95),
		selectedMailbox: "INBOX",
		logger:          slog.Default(),
	}
	w := &storeFetchWriterFake{}
	err := s.store(context.Background(), w, imap.UIDSetNum(1),
		&imap.StoreFlags{Op: imap.StoreFlagsAdd, Flags: []imap.Flag{"\\Seen"}},
		nil, nil)
	if err != nil {
		t.Fatalf("store: %v", err)
	}
	if len(caller.putSpamCalls) != 0 {
		t.Errorf("no training signal expected, got %d", len(caller.putSpamCalls))
	}
	if caller.ciphertextHit != 0 {
		t.Errorf("ciphertext should not be fetched for non-\\Junk STORE, hit count %d", caller.ciphertextHit)
	}
}
