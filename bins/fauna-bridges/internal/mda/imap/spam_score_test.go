package imap

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"strings"
	"sync"
	"testing"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// spamScoreCaller is the per-file fake for the SELECT-time scoring pass.
// It answers the five RPCs scoreSelectedInbox issues — fetch_spam_model,
// fetch_message_metadata, fetch_message_ciphertext, store_flags, move —
// and captures the store_flags + move requests for assertion.
//
// By convention, every IMAP-layer test file declares its own
// caller stub.
type spamScoreCaller struct {
	modelBlob  []byte              // sealed model returned by fetch_spam_model (nil ⇒ untrained)
	inboxMeta  []wsrpc.MessageMeta // INBOX rows returned by fetch_message_metadata
	cipherByID map[string][]byte   // message_id → PLAINTEXT body, sealed at serve time (fixtureSeal)

	mu          sync.Mutex
	fetchModel  int
	metaCalls   int
	storeCalls  []capturedSpamStore
	moveCalls   []capturedSpamMove
	cipherCalls int
}

type capturedSpamStore struct {
	Mailbox string   `cbor:"mailbox"`
	UIDs    []uint32 `cbor:"uids"`
	Op      string   `cbor:"op"`
	Flags   []string `cbor:"flags"`
}

type capturedSpamMove struct {
	SourceMailbox string   `cbor:"source_mailbox"`
	UIDs          []uint32 `cbor:"uids"`
	DestMailbox   string   `cbor:"dest_mailbox"`
}

func (c *spamScoreCaller) Call(_ context.Context, method string, body, reply any) error {
	enc, err := dagcbor.Marshal(body)
	if err != nil {
		return err
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	switch method {
	case wsrpc.MethodFetchSpamModel:
		c.fetchModel++
		rep, err := dagcbor.Marshal(struct {
			Blob []byte `cbor:"blob"`
		}{Blob: c.modelBlob})
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)

	case wsrpc.MethodFetchMessageMetadata:
		c.metaCalls++
		rep, err := dagcbor.Marshal(struct {
			Messages []wsrpc.MessageMeta `cbor:"messages"`
		}{Messages: c.inboxMeta})
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)

	case wsrpc.MethodFetchMessageCiphertext:
		c.cipherCalls++
		var req struct {
			MessageID []byte `cbor:"message_id"`
		}
		_ = cbor.Unmarshal(enc, &req)
		pt, ok := c.cipherByID[string(req.MessageID)]
		if !ok {
			rep, _ := dagcbor.Marshal(map[string]any{"outcome": "not_found"})
			return cbor.Unmarshal(rep, reply)
		}
		blob := fixtureSeal(pt)
		rep, _ := dagcbor.Marshal(map[string]any{
			"outcome":         "found",
			"encrypted_body":  blob,
			"ciphertext_size": uint32(len(blob)),
			"internal_date":   int64(1_700_000_000),
		})
		return cbor.Unmarshal(rep, reply)

	case wsrpc.MethodStoreFlags:
		var s capturedSpamStore
		_ = cbor.Unmarshal(enc, &s)
		c.storeCalls = append(c.storeCalls, s)
		rep, _ := dagcbor.Marshal(map[string]any{
			"updated":       []any{},
			"highestmodseq": int64(1),
		})
		return cbor.Unmarshal(rep, reply)

	case wsrpc.MethodMove:
		var m capturedSpamMove
		_ = cbor.Unmarshal(enc, &m)
		c.moveCalls = append(c.moveCalls, m)
		rep, _ := dagcbor.Marshal(wsrpc.MoveMessagesReply{})
		return cbor.Unmarshal(rep, reply)
	}
	return errors.New("spamScoreCaller: unexpected " + method)
}

// spamScoreOpenerStub maps the spam MODEL's arbitrary fixture "sealed" bytes
// to its plaintext (the model's STRICT `opener.Open(sealed)` rail) and opens
// every other envelope — the message bodies the fake nest sealed with
// fixtureSeal — with the real fixtureOpener.
type spamScoreOpenerStub struct{ table map[string][]byte }

func (d *spamScoreOpenerStub) Open(envelope []byte) ([]byte, error) {
	pt, ok := d.table[string(envelope)]
	if !ok {
		return fixtureOpener().Open(envelope)
	}
	out := make([]byte, len(pt))
	copy(out, pt)
	return out, nil
}

// keywordScorer returns a deterministic scoreFn: any body containing
// "SPAMMY" scores 9000 milli (above the spam_folder default of 5000),
// everything else 100 (below). Lets the test drive the disposition
// without the cgo FFI scorer.
func keywordScorer() func(model []byte, text string) int32 {
	return func(_ []byte, text string) int32 {
		if strings.Contains(text, "SPAMMY") {
			return 9000
		}
		return 100
	}
}

func newSpamScoreSession(c wsrpc.Caller, scoreFn func([]byte, string) int32, threshold uint32) *Session {
	return &Session{
		actorID:    []byte("actor-0000000000000000000000000000"),
		client:     c,
		logger:     slog.Default(),
		spamPolicy: mailfauna.SpamPolicy{SpamFolderThreshold: threshold},
		scoreFn:    scoreFn,
	}
}

func TestScoreSelectedInboxMovesSpamAndWatermarksAll(t *testing.T) {
	caller := &spamScoreCaller{
		modelBlob: []byte("SEALED-MODEL"),
		inboxMeta: []wsrpc.MessageMeta{
			{UID: 10, MessageID: []byte("mid-10")},
			{UID: 11, MessageID: []byte("mid-11")},
		},
		// Plaintext bodies — the fake nest seals them at serve time.
		cipherByID: map[string][]byte{
			"mid-10": []byte("subject SPAMMY body buy pills"),
			"mid-11": []byte("subject hello body lunch friend"),
		},
	}
	opener := &spamScoreOpenerStub{table: map[string][]byte{
		"SEALED-MODEL": []byte("MODEL-BYTES"),
	}}
	s := newSpamScoreSession(caller, keywordScorer(), 5)

	s.scoreSelectedInbox(context.Background(), opener)

	if len(caller.storeCalls) != 1 {
		t.Fatalf("want 1 store_flags call, got %d", len(caller.storeCalls))
	}
	st := caller.storeCalls[0]
	if st.Mailbox != "INBOX" || st.Op != string(wsrpc.StoreFlagsOpAdd) {
		t.Fatalf("store_flags mailbox/op = %q/%q, want INBOX/add", st.Mailbox, st.Op)
	}
	if len(st.Flags) != 1 || st.Flags[0] != spamScoredKeyword {
		t.Fatalf("store_flags flags = %v, want [%s]", st.Flags, spamScoredKeyword)
	}
	if !equalUint32Set(st.UIDs, []uint32{10, 11}) {
		t.Fatalf("watermarked UIDs = %v, want {10,11} (every scored message)", st.UIDs)
	}
	if len(caller.moveCalls) != 1 {
		t.Fatalf("want 1 move call, got %d", len(caller.moveCalls))
	}
	mv := caller.moveCalls[0]
	if mv.SourceMailbox != "INBOX" || mv.DestMailbox != "Junk" {
		t.Fatalf("move %q→%q, want INBOX→Junk", mv.SourceMailbox, mv.DestMailbox)
	}
	if !equalUint32Set(mv.UIDs, []uint32{10}) {
		t.Fatalf("moved UIDs = %v, want {10} (only the SPAMMY message)", mv.UIDs)
	}
}

func TestScoreSelectedInboxAllHamWatermarksNoMove(t *testing.T) {
	caller := &spamScoreCaller{
		modelBlob:  []byte("SEALED-MODEL"),
		inboxMeta:  []wsrpc.MessageMeta{{UID: 7, MessageID: []byte("mid-7")}},
		cipherByID: map[string][]byte{"mid-7": []byte("perfectly normal mail")},
	}
	opener := &spamScoreOpenerStub{table: map[string][]byte{
		"SEALED-MODEL": []byte("MODEL-BYTES"),
	}}
	s := newSpamScoreSession(caller, keywordScorer(), 5)

	s.scoreSelectedInbox(context.Background(), opener)

	if len(caller.storeCalls) != 1 || !equalUint32Set(caller.storeCalls[0].UIDs, []uint32{7}) {
		t.Fatalf("want the ham message watermarked, got store calls %+v", caller.storeCalls)
	}
	if len(caller.moveCalls) != 0 {
		t.Fatalf("ham must not move to Junk, got %d move calls", len(caller.moveCalls))
	}
}

func TestScoreSelectedInboxUntrainedIsCheapNoOp(t *testing.T) {
	caller := &spamScoreCaller{modelBlob: nil} // untrained ⇒ blob None
	opener := &spamScoreOpenerStub{table: map[string][]byte{}}
	s := newSpamScoreSession(caller, keywordScorer(), 5)

	s.scoreSelectedInbox(context.Background(), opener)

	if caller.fetchModel != 1 {
		t.Fatalf("want exactly 1 fetch_spam_model, got %d", caller.fetchModel)
	}
	if caller.metaCalls != 0 || caller.cipherCalls != 0 {
		t.Fatalf("untrained actor must not scan INBOX (meta=%d cipher=%d)", caller.metaCalls, caller.cipherCalls)
	}
	if len(caller.storeCalls) != 0 || len(caller.moveCalls) != 0 {
		t.Fatalf("untrained actor must not store/move")
	}
}

func TestScoreSelectedInboxSkipsAlreadyWatermarked(t *testing.T) {
	caller := &spamScoreCaller{
		modelBlob: []byte("SEALED-MODEL"),
		inboxMeta: []wsrpc.MessageMeta{
			{UID: 1, MessageID: []byte("mid-1"), Flags: []string{spamScoredKeyword}},
			{UID: 2, MessageID: []byte("mid-2")},
		},
		cipherByID: map[string][]byte{
			"mid-1": []byte("SPAMMY"), // would score spam but is already watermarked
			"mid-2": []byte("ham"),
		},
	}
	opener := &spamScoreOpenerStub{table: map[string][]byte{
		"SEALED-MODEL": []byte("MODEL-BYTES"),
	}}
	s := newSpamScoreSession(caller, keywordScorer(), 5)

	s.scoreSelectedInbox(context.Background(), opener)

	// Only UID 2 (un-watermarked) is fetched + scored; UID 1 is skipped, so
	// the spam it would have scored never moves.
	if caller.cipherCalls != 1 {
		t.Fatalf("want 1 ciphertext fetch (the un-watermarked message), got %d", caller.cipherCalls)
	}
	if len(caller.moveCalls) != 0 {
		t.Fatalf("the already-watermarked spam must not be re-scored/moved, got %d moves", len(caller.moveCalls))
	}
	if len(caller.storeCalls) != 1 || !equalUint32Set(caller.storeCalls[0].UIDs, []uint32{2}) {
		t.Fatalf("want only UID 2 watermarked, got %+v", caller.storeCalls)
	}
}

func TestScoreSelectedInboxNoCapabilityNoOp(t *testing.T) {
	caller := &spamScoreCaller{modelBlob: []byte("SEALED-MODEL")}
	s := newSpamScoreSession(caller, keywordScorer(), 5)

	// nil opener (no MLS snapshot provisioned) ⇒ can't unwrap the sealed
	// model ⇒ no RPCs at all.
	s.scoreSelectedInbox(context.Background(), nil)

	if caller.fetchModel != 0 || caller.metaCalls != 0 {
		t.Fatalf("missing capability must short-circuit before any RPC (fetchModel=%d meta=%d)", caller.fetchModel, caller.metaCalls)
	}
}

// stamped renders a message body carrying the delivery-time spam-threshold
// stamp nest folds at RCPT (mail-aliases.md § Spam-threshold override), in the
// shape the MTA prepends it: the header first, CRLF-terminated, then the rest.
func stamped(threshold int, body string) []byte {
	return []byte(fmt.Sprintf("X-Fauna-Spam-Threshold: %d\r\nSubject: t\r\n\r\n%s", threshold, body))
}

// TestScoreSelectedInboxSpamFolderDisabledHonoursAPerMessageStamp is the
// regression pin for the trap `mail-aliases.md:160` names by hand: the scorer
// used to exit the whole pass when the SESSION threshold was 0, which under
// per-message thresholds would skip exactly the messages a user set an override
// for. A deployment with auto-Junk disabled must still file a message whose own
// stamp asks for it — that override is the user's safety valve (mail-spam.md:29).
func TestScoreSelectedInboxSpamFolderDisabledHonoursAPerMessageStamp(t *testing.T) {
	caller := &spamScoreCaller{
		modelBlob: []byte("SEALED-MODEL"),
		inboxMeta: []wsrpc.MessageMeta{
			{UID: 20, MessageID: []byte("mid-20")},
			{UID: 21, MessageID: []byte("mid-21")},
		},
		cipherByID: map[string][]byte{
			// Carries its own threshold of 5 points ⇒ the 9000-milli SPAMMY
			// score crosses it even though the session policy is disabled.
			"mid-20": stamped(5, "SPAMMY buy pills"),
			// No stamp ⇒ falls back to the session policy, which is 0 ⇒ this
			// message is not Junk-routable at all.
			"mid-21": []byte("SPAMMY but unstamped (as via APPEND or import)"),
		},
	}
	opener := &spamScoreOpenerStub{table: map[string][]byte{
		"SEALED-MODEL": []byte("MODEL-BYTES"),
	}}
	s := newSpamScoreSession(caller, keywordScorer(), 0) // admin disabled auto-Junk

	s.scoreSelectedInbox(context.Background(), opener)

	if len(caller.moveCalls) != 1 {
		t.Fatalf("want 1 move call (the stamped message), got %d", len(caller.moveCalls))
	}
	if !equalUint32Set(caller.moveCalls[0].UIDs, []uint32{20}) {
		t.Fatalf("moved UIDs = %v, want {20} — only the message carrying its own threshold",
			caller.moveCalls[0].UIDs)
	}
	// Both are watermarked: the unstamped one WAS scored, its verdict was
	// simply "not routable". It must not be re-decrypted on every later SELECT.
	if len(caller.storeCalls) != 1 || !equalUint32Set(caller.storeCalls[0].UIDs, []uint32{20, 21}) {
		t.Fatalf("want both messages watermarked once, got %+v", caller.storeCalls)
	}
}

// TestScoreSelectedInboxPerMessageStampOutranksTheSessionPolicy pins both
// directions of the override — stricter than the deployment and more lenient
// than it — since a fold that only ever tightened would still pass a one-sided
// test while silently ignoring the `+newsletter` case the goal doc opens with.
func TestScoreSelectedInboxPerMessageStampOutranksTheSessionPolicy(t *testing.T) {
	// A flat 3-point (3000 milli) score on every message: below the session
	// threshold of 5, above a stamped 2, below a stamped 9.
	flat := func(_ []byte, _ string) int32 { return 3000 }
	caller := &spamScoreCaller{
		modelBlob: []byte("SEALED-MODEL"),
		inboxMeta: []wsrpc.MessageMeta{
			{UID: 30, MessageID: []byte("mid-30")},
			{UID: 31, MessageID: []byte("mid-31")},
			{UID: 32, MessageID: []byte("mid-32")},
		},
		cipherByID: map[string][]byte{
			// Stricter than the deployment (a `+banking` alias): 3 ≥ 2 ⇒ Junk.
			"mid-30": stamped(2, "ordinary looking mail"),
			// More lenient (a `+newsletter` alias): 3 < 9 ⇒ stays in INBOX.
			"mid-31": stamped(9, "ordinary looking mail"),
			// Unstamped ⇒ session policy 5: 3 < 5 ⇒ stays.
			"mid-32": []byte("ordinary looking mail"),
		},
	}
	opener := &spamScoreOpenerStub{table: map[string][]byte{
		"SEALED-MODEL": []byte("MODEL-BYTES"),
	}}
	s := newSpamScoreSession(caller, flat, 5)

	s.scoreSelectedInbox(context.Background(), opener)

	if len(caller.moveCalls) != 1 {
		t.Fatalf("want 1 move call, got %d", len(caller.moveCalls))
	}
	if !equalUint32Set(caller.moveCalls[0].UIDs, []uint32{30}) {
		t.Fatalf("moved UIDs = %v, want {30} — only the message whose own threshold it crossed",
			caller.moveCalls[0].UIDs)
	}
}

// TestScoreSelectedInboxStampedZeroIsNotRoutable pins the disabled tier at the
// per-message layer: a user who sets 0 has turned auto-Junk off for that
// alias/account, so even an obviously spammy message stays put. Without the
// `spamFolderMilli > 0` guard a 0 threshold would Junk EVERY message instead —
// the exact inversion of what the setting means.
func TestScoreSelectedInboxStampedZeroIsNotRoutable(t *testing.T) {
	caller := &spamScoreCaller{
		modelBlob:  []byte("SEALED-MODEL"),
		inboxMeta:  []wsrpc.MessageMeta{{UID: 40, MessageID: []byte("mid-40")}},
		cipherByID: map[string][]byte{"mid-40": stamped(0, "SPAMMY buy pills")},
	}
	opener := &spamScoreOpenerStub{table: map[string][]byte{
		"SEALED-MODEL": []byte("MODEL-BYTES"),
	}}
	s := newSpamScoreSession(caller, keywordScorer(), 5) // deployment WOULD file it

	s.scoreSelectedInbox(context.Background(), opener)

	if len(caller.moveCalls) != 0 {
		t.Fatalf("a stamped 0 means auto-Junk off for this message; got %d moves", len(caller.moveCalls))
	}
	if len(caller.storeCalls) != 1 || !equalUint32Set(caller.storeCalls[0].UIDs, []uint32{40}) {
		t.Fatalf("want the message watermarked as scored, got %+v", caller.storeCalls)
	}
}

// TestScoreSelectedInboxCapsPerPass exercises a
// cold-start INBOX of more un-watermarked messages than the per-pass cap
// must score only the cap'd count of *most-recent* (highest-UID) messages
// this SELECT and defer the rest to the next one. The deferred tail is NOT
// watermarked, so a later SELECT picks it up (eventual consistency).
func TestScoreSelectedInboxCapsPerPass(t *testing.T) {
	const extra = 5
	total := spamScorePerPassCap + extra
	meta := make([]wsrpc.MessageMeta, 0, total)
	cipher := make(map[string][]byte, total)
	table := map[string][]byte{"SEALED-MODEL": []byte("MODEL-BYTES")}
	for i := 1; i <= total; i++ {
		mid := fmt.Sprintf("mid-%d", i)
		meta = append(meta, wsrpc.MessageMeta{UID: uint32(i), MessageID: []byte(mid)})
		cipher[mid] = []byte("ordinary ham body") // all ham ⇒ no move; isolate the cap
	}
	caller := &spamScoreCaller{modelBlob: []byte("SEALED-MODEL"), inboxMeta: meta, cipherByID: cipher}
	opener := &spamScoreOpenerStub{table: table}
	s := newSpamScoreSession(caller, keywordScorer(), 5)

	s.scoreSelectedInbox(context.Background(), opener)

	// Only the cap'd count of messages is fetched + scored this pass.
	if caller.cipherCalls != spamScorePerPassCap {
		t.Fatalf("want %d ciphertext fetches (the per-pass cap), got %d", spamScorePerPassCap, caller.cipherCalls)
	}
	if len(caller.storeCalls) != 1 {
		t.Fatalf("want 1 watermark store, got %d", len(caller.storeCalls))
	}
	got := caller.storeCalls[0].UIDs
	if len(got) != spamScorePerPassCap {
		t.Fatalf("want %d watermarked UIDs, got %d", spamScorePerPassCap, len(got))
	}
	set := make(map[uint32]bool, len(got))
	for _, u := range got {
		set[u] = true
	}
	// The oldest `extra` UIDs (1..extra) are deferred — NOT watermarked.
	for i := 1; i <= extra; i++ {
		if set[uint32(i)] {
			t.Fatalf("UID %d (oldest) should be deferred to the next SELECT, but was watermarked this pass", i)
		}
	}
	// The most-recent cap'd UIDs (extra+1..total) ARE scored this pass.
	for i := extra + 1; i <= total; i++ {
		if !set[uint32(i)] {
			t.Fatalf("UID %d (recent) should be scored this pass, but was not watermarked", i)
		}
	}
}

func equalUint32Set(got, want []uint32) bool {
	if len(got) != len(want) {
		return false
	}
	seen := make(map[uint32]int, len(got))
	for _, g := range got {
		seen[g]++
	}
	for _, w := range want {
		if seen[w] == 0 {
			return false
		}
		seen[w]--
	}
	return true
}

// orderingFetcher is a metadataFetcher seam that runs onFetch (an ordering
// probe) the moment FetchAllUIDs is called, then returns canned UIDs — so a
// test can assert whether the IDLE append gate scored BEFORE it took the
// announce snapshot.
type orderingFetcher struct {
	uids    []uint32
	onFetch func()
}

func (f *orderingFetcher) FetchAllUIDs(_ context.Context, _ []byte, _ string) ([]uint32, error) {
	if f.onFetch != nil {
		f.onFetch()
	}
	return f.uids, nil
}

// SeqInfo mirrors FetchAllUIDs' ordering probe — the F1 append/flags path now
// takes its announce snapshot via SeqInfo, so the scored-before-visible
// assertion must observe the same firing point. Derives seq/total from the
// canned (sorted) uids like the nest.
func (f *orderingFetcher) SeqInfo(_ context.Context, _ []byte, _ string, uid uint32) (uint32, uint32, bool, error) {
	if f.onFetch != nil {
		f.onFetch()
	}
	seq := seqOf(f.uids, uid)
	return seq, uint32(len(f.uids)), seq != 0, nil
}

// TestHandleEventAppendScoresInboxBeforeAnnounce is the Arm-2 (serve gate)
// scored-before-visible proof at the seam level (content-scoring.md § Timing;
// Phase-3 design D5): a spam message delivered to INBOX while the client is
// IDLE'd is per-user scored and re-filed INBOX→Junk BEFORE handleEvent takes
// the metadata snapshot it announces — the IDLE twin of the SELECT-time pass
// (select.go). Ordering is asserted directly: the fetcher records whether the
// spam move had already fired at snapshot time, and the post-move snapshot (an
// empty INBOX) is what the EXISTS reflects.
func TestHandleEventAppendScoresInboxBeforeAnnounce(t *testing.T) {
	caller := &spamScoreCaller{
		modelBlob:  []byte("SEALED-MODEL"),
		inboxMeta:  []wsrpc.MessageMeta{{UID: 42, MessageID: []byte("mid-42")}},
		cipherByID: map[string][]byte{"mid-42": []byte("subject SPAMMY body buy pills")},
	}
	opener := &spamScoreOpenerStub{table: map[string][]byte{"SEALED-MODEL": []byte("MODEL")}}
	s := &Session{
		actorID:     []byte("actor-0000000000000000000000000000"),
		client:      caller,
		logger:      slog.Default(),
		spamPolicy:  mailfauna.SpamPolicy{SpamFolderThreshold: 5},
		scoreFn:     keywordScorer(),
		scoreOpener: opener,
	}

	var movedBeforeSnapshot bool
	// The move has re-filed the spam out of INBOX by snapshot time → an empty
	// surviving-UID list, so the announced EXISTS excludes the un-scored spam.
	fetcher := &orderingFetcher{
		uids:    []uint32{},
		onFetch: func() { movedBeforeSnapshot = len(caller.moveCalls) > 0 },
	}
	w := &updateWriterFake{}
	ev := wsrpc.MailboxStateEvent{Kind: wsrpc.MailboxStateEventAppend, Uid: 42, Modseq: 100}

	if err := s.handleEvent(context.Background(), w, fetcher, s.actorID, "INBOX", ev); err != nil {
		t.Fatalf("handleEvent(append, INBOX): %v", err)
	}

	if len(caller.moveCalls) != 1 || !equalUint32Set(caller.moveCalls[0].UIDs, []uint32{42}) {
		t.Fatalf("IDLE append did not score+re-file the spam before announce: moves=%v", caller.moveCalls)
	}
	if caller.moveCalls[0].SourceMailbox != "INBOX" || caller.moveCalls[0].DestMailbox != "Junk" {
		t.Fatalf("spam must be re-filed INBOX→Junk; got %q→%q",
			caller.moveCalls[0].SourceMailbox, caller.moveCalls[0].DestMailbox)
	}
	if !movedBeforeSnapshot {
		t.Fatal("the spam re-file must fire BEFORE the announce snapshot (scored-before-visible)")
	}
	nums, _, _ := w.snapshot()
	if len(nums) != 1 || nums[0] != 0 {
		t.Fatalf("EXISTS must reflect the post-move INBOX (0 messages); got %v", nums)
	}
}

// TestHandleEventAppendNonInboxSkipsScoring guards the INBOX-only gate: an
// append to a non-INBOX mailbox announces without any per-user scoring pass
// (per-user Junk routing is INBOX-only — mail-spam.md § Scoring placement).
func TestHandleEventAppendNonInboxSkipsScoring(t *testing.T) {
	caller := &spamScoreCaller{
		modelBlob:  []byte("SEALED-MODEL"),
		inboxMeta:  []wsrpc.MessageMeta{{UID: 5, MessageID: []byte("mid-5")}},
		cipherByID: map[string][]byte{"mid-5": []byte("subject SPAMMY body")},
	}
	opener := &spamScoreOpenerStub{table: map[string][]byte{"SEALED-MODEL": []byte("MODEL")}}
	s := &Session{
		actorID:     []byte("actor-0000000000000000000000000000"),
		client:      caller,
		logger:      slog.Default(),
		spamPolicy:  mailfauna.SpamPolicy{SpamFolderThreshold: 5},
		scoreFn:     keywordScorer(),
		scoreOpener: opener,
	}
	fetcher := &orderingFetcher{uids: []uint32{5}}
	w := &updateWriterFake{}
	ev := wsrpc.MailboxStateEvent{Kind: wsrpc.MailboxStateEventAppend, Uid: 5, Modseq: 1}

	if err := s.handleEvent(context.Background(), w, fetcher, s.actorID, "Archive", ev); err != nil {
		t.Fatalf("handleEvent(append, Archive): %v", err)
	}
	if caller.fetchModel != 0 || len(caller.moveCalls) != 0 {
		t.Fatalf("a non-INBOX append must not run the per-user scoring pass "+
			"(fetch_spam_model calls=%d, moves=%d)", caller.fetchModel, len(caller.moveCalls))
	}
}
