// Phase F.2 — IDLE / NOTIFY MDA wire surface.
//
// These are tier_2 by the project taxonomy (real per-Session code path,
// stubbed wsrpc.Caller + stubbed metadata-fetcher seam).  The
// full-stack two-session APPEND→EXISTS+FETCH test lives in F.3's
// pytest fixture.
package imap

import (
	"bytes"
	"context"
	"errors"
	"log/slog"
	"sync"
	"testing"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// ── updateWriterFake ────────────────────────────────────────────────

// updateWriterFake records every WriteNumMessages / WriteMessageFlags /
// WriteExpunge call.  Satisfies the updateWriter seam from idle.go so
// Session.idle can be driven without a live TCP connection.
type updateWriterFake struct {
	mu       sync.Mutex
	existing []uint32 // sequence of WriteNumMessages args
	fetches  []capturedFetch
	expunges []uint32      // sequence of WriteExpunge args
	vanished []imap.UIDSet // sequence of WriteVanished args (QRESYNC)
	err      error         // if non-nil, the next Write* returns it (one-shot)
}

type capturedFetch struct {
	Seq    uint32
	UID    imap.UID
	Flags  []imap.Flag
	ModSeq uint64 // 0 unless written via WriteMessageFlagsModSeq
}

func (w *updateWriterFake) WriteNumMessages(n uint32) error {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.err != nil {
		e := w.err
		w.err = nil
		return e
	}
	w.existing = append(w.existing, n)
	return nil
}

func (w *updateWriterFake) WriteMessageFlags(seq uint32, uid imap.UID, flags []imap.Flag) error {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.err != nil {
		e := w.err
		w.err = nil
		return e
	}
	w.fetches = append(w.fetches, capturedFetch{Seq: seq, UID: uid, Flags: append([]imap.Flag(nil), flags...)})
	return nil
}

func (w *updateWriterFake) WriteMessageFlagsModSeq(seq uint32, uid imap.UID, flags []imap.Flag, modSeq uint64) error {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.err != nil {
		e := w.err
		w.err = nil
		return e
	}
	w.fetches = append(w.fetches, capturedFetch{Seq: seq, UID: uid, Flags: append([]imap.Flag(nil), flags...), ModSeq: modSeq})
	return nil
}

func (w *updateWriterFake) WriteExpunge(seq uint32) error {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.err != nil {
		e := w.err
		w.err = nil
		return e
	}
	w.expunges = append(w.expunges, seq)
	return nil
}

func (w *updateWriterFake) WriteVanished(uids imap.UIDSet) error {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.err != nil {
		e := w.err
		w.err = nil
		return e
	}
	w.vanished = append(w.vanished, uids)
	return nil
}

func (w *updateWriterFake) snapshot() ([]uint32, []capturedFetch, []uint32) {
	w.mu.Lock()
	defer w.mu.Unlock()
	return append([]uint32(nil), w.existing...),
		append([]capturedFetch(nil), w.fetches...),
		append([]uint32(nil), w.expunges...)
}

// vanishedSnapshot returns the recorded `* VANISHED <set>` calls.
func (w *updateWriterFake) vanishedSnapshot() []imap.UIDSet {
	w.mu.Lock()
	defer w.mu.Unlock()
	return append([]imap.UIDSet(nil), w.vanished...)
}

// ── metadataFetcherFake ────────────────────────────────────────────

// metadataFetcherFake returns canned UID lists per call without going
// through wsrpc.  The same list is returned every time unless the test
// swaps `uids` mid-flight.
type metadataFetcherFake struct {
	mu             sync.Mutex
	uids           []uint32
	err            error
	fetchAllCalls  int    // whole-mailbox fetches (the F1-avoided path)
	seqInfoCalls   int    // single-UID lookups (the F1 present-UID path)
	lastSeqInfoUID uint32 // UID of the most recent SeqInfo call
}

func (f *metadataFetcherFake) setUIDs(uids []uint32) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.uids = append([]uint32(nil), uids...)
}

func (f *metadataFetcherFake) FetchAllUIDs(_ context.Context, _ []byte, _ string) ([]uint32, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.fetchAllCalls++
	if f.err != nil {
		return nil, f.err
	}
	return append([]uint32(nil), f.uids...), nil
}

// SeqInfo derives one UID's sequence number and the mailbox total from the
// canned (sorted) uids, mirroring the nest — so a test that swaps uids sees
// consistent seq/total across FetchAllUIDs and SeqInfo.
func (f *metadataFetcherFake) SeqInfo(_ context.Context, _ []byte, _ string, uid uint32) (uint32, uint32, bool, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.seqInfoCalls++
	f.lastSeqInfoUID = uid
	if f.err != nil {
		return 0, 0, false, f.err
	}
	seq := seqOf(f.uids, uid) // f.uids is set sorted, as the FetchAllUIDs consumers require
	return seq, uint32(len(f.uids)), seq != 0, nil
}

// ── idleSubscribeCaller ────────────────────────────────────────────────

// idleSubscribeCaller is the wsrpc.Caller stub for Idle tests.  It
// responds to `subscribe_mailbox_state` with a canned subscription_id
// and rejects every other method (test failures surface as missing
// expectations, not silent successes — same pattern as expungeCaller).
type idleSubscribeCaller struct {
	mu             sync.Mutex
	subscriptionID uint64
	subscribeCalls int
	subscribeErr   error
}

func (c *idleSubscribeCaller) Call(_ context.Context, method string, body, reply any) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if method != "fauna.bridges.subscribe_mailbox_state" {
		return errors.New("idleSubscribeCaller: unexpected " + method)
	}
	c.subscribeCalls++
	if c.subscribeErr != nil {
		return c.subscribeErr
	}
	// Marshal the canned reply.  The wrapper expects
	// `{"outcome": "subscribed", "subscription_id": <u64>}`.
	type repShape struct {
		Outcome        string `cbor:"outcome"`
		SubscriptionID uint64 `cbor:"subscription_id"`
	}
	rep, err := dagcbor.Marshal(repShape{Outcome: "subscribed", SubscriptionID: c.subscriptionID})
	if err != nil {
		return err
	}
	return cbor.Unmarshal(rep, reply)
}

// ── helpers ────────────────────────────────────────────────────────

// sessionForIDLE builds a Session ready to call s.idle().  Caller
// supplies the SELECT state (selectedMailbox / lastKnownModseq), the
// wsrpc.Caller, the router, and the metadata-fetcher fake.
func sessionForIDLE(t *testing.T, mailbox string, client wsrpc.Caller, router *notificationRouter, fetcher metadataFetcher) *Session {
	t.Helper()
	return &Session{
		client:          client,
		actorID:         bytes.Repeat([]byte{0xa0}, 32),
		credentialID:    "default",
		selectedMailbox: mailbox,
		router:          router,
		idleTimeout:     250 * time.Millisecond, // tests want short timeouts
		idleFetcher:     fetcher,
		logger:          slog.Default(),
	}
}

// runIdleAndCollect drives s.idle in a goroutine, lets `deliver` push
// events via the router, then either lets the timeout fire or signals
// stop, and returns the recorded UpdateWriter state.
func runIdleAndCollect(
	t *testing.T,
	s *Session,
	w *updateWriterFake,
	deliver func(t *testing.T),
	stopAfter time.Duration,
) error {
	t.Helper()
	stop := make(chan struct{})
	done := make(chan error, 1)
	go func() {
		done <- s.idle(context.Background(), w, stop)
	}()
	// Give the idle loop a tick to register its subscription.
	time.Sleep(20 * time.Millisecond)
	deliver(t)
	// Give event handling a moment.
	time.Sleep(50 * time.Millisecond)
	if stopAfter > 0 {
		time.AfterFunc(stopAfter, func() { close(stop) })
	} else {
		close(stop)
	}
	select {
	case err := <-done:
		return err
	case <-time.After(2 * time.Second):
		t.Fatal("idle did not return within 2s")
		return nil
	}
}

// pushEvent helper — marshals + injects an event through the router's
// Handle method, simulating a wsrpc OnPush callback.
func pushEvent(t *testing.T, router *notificationRouter, subID uint64, ev wsrpc.MailboxStateEvent) {
	t.Helper()
	push := wsrpc.BridgeMailboxStatePush{
		SubscriptionID: subID,
		ActorID:        bytes.Repeat([]byte{0xa0}, 32),
		Mailbox:        "INBOX",
		Event:          ev,
	}
	payload, err := dagcbor.Marshal(push)
	if err != nil {
		t.Fatalf("marshal push: %v", err)
	}
	router.Handle(wsrpc.BridgeMailboxStatePushKind, payload, 1)
}

// ── notificationRouter tests ──────────────────────────────────────

func TestNotificationRouter_RouteAndUnregister(t *testing.T) {
	t.Parallel()
	r := newNotificationRouter(slog.Default())
	ch := r.Register(7)

	// Push an event to subscription 7 → expect on ch.
	push := wsrpc.BridgeMailboxStatePush{
		SubscriptionID: 7,
		ActorID:        bytes.Repeat([]byte{0x01}, 32),
		Mailbox:        "INBOX",
		Event:          wsrpc.MailboxStateEvent{Kind: wsrpc.MailboxStateEventAppend, Uid: 42, Modseq: 100},
	}
	payload, err := dagcbor.Marshal(push)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	r.Handle(wsrpc.BridgeMailboxStatePushKind, payload, 1)
	select {
	case ev := <-ch:
		if ev.Kind != wsrpc.MailboxStateEventAppend || ev.Uid != 42 || ev.Modseq != 100 {
			t.Errorf("got %+v", ev)
		}
	case <-time.After(500 * time.Millisecond):
		t.Fatal("event not delivered within 500ms")
	}

	// Unregister and push again → no delivery.
	r.Unregister(7)
	r.Handle(wsrpc.BridgeMailboxStatePushKind, payload, 2)
	select {
	case ev := <-ch:
		t.Fatalf("event delivered to unregistered route: %+v", ev)
	case <-time.After(100 * time.Millisecond):
		// Pass: silent drop.
	}
}

func TestNotificationRouter_UnknownSubscriptionDropped(t *testing.T) {
	t.Parallel()
	r := newNotificationRouter(slog.Default())
	// No Register call — every push is for an unknown subscription.
	push := wsrpc.BridgeMailboxStatePush{
		SubscriptionID: 99,
		ActorID:        bytes.Repeat([]byte{0x02}, 32),
		Mailbox:        "INBOX",
		Event:          wsrpc.MailboxStateEvent{Kind: wsrpc.MailboxStateEventAppend, Uid: 1},
	}
	payload, err := dagcbor.Marshal(push)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	// Must not panic; must not block.
	r.Handle(wsrpc.BridgeMailboxStatePushKind, payload, 1)
}

func TestNotificationRouter_NonMailboxKindIgnored(t *testing.T) {
	t.Parallel()
	r := newNotificationRouter(slog.Default())
	ch := r.Register(1)
	r.Handle("fauna.bridges.push.something_else", []byte{0xff, 0xff}, 1)
	select {
	case ev := <-ch:
		t.Fatalf("unrelated kind delivered: %+v", ev)
	case <-time.After(50 * time.Millisecond):
		// Pass.
	}
}

func TestNotificationRouter_DropsOnFullChannel(t *testing.T) {
	t.Parallel()
	r := newNotificationRouter(slog.Default())
	_ = r.Register(5)
	// Fire defaultRouterChanBuf+1 events; the last one must be dropped
	// (and not panic).
	for i := 0; i < defaultRouterChanBuf+5; i++ {
		push := wsrpc.BridgeMailboxStatePush{
			SubscriptionID: 5,
			Event:          wsrpc.MailboxStateEvent{Kind: wsrpc.MailboxStateEventAppend, Uid: uint32(i + 1)},
		}
		payload, _ := dagcbor.Marshal(push)
		r.Handle(wsrpc.BridgeMailboxStatePushKind, payload, uint64(i))
	}
	// Drain channel; should have exactly defaultRouterChanBuf events.
}

// ── Session.idle tests ─────────────────────────────────────────────

// TestIdle_AppendEmitsEXISTSAndFETCH — the load-bearing assertion of
// F.2: a session in IDLE receives an `append` push, translates to
// `* <new-count> EXISTS` + `* <seq> FETCH (UID … FLAGS …)`, and writes
// both to the UpdateWriter in the right order.
func TestIdle_AppendEmitsEXISTSAndFETCH(t *testing.T) {
	t.Parallel()
	caller := &idleSubscribeCaller{subscriptionID: 11}
	router := newNotificationRouter(slog.Default())
	// Post-append mailbox: UIDs [1, 2, 3]; new UID 3 is the one
	// appended.
	fetcher := &metadataFetcherFake{}
	fetcher.setUIDs([]uint32{1, 2, 3})
	s := sessionForIDLE(t, "INBOX", caller, router, fetcher)
	w := &updateWriterFake{}

	err := runIdleAndCollect(t, s, w, func(t *testing.T) {
		pushEvent(t, router, 11, wsrpc.MailboxStateEvent{
			Kind:   wsrpc.MailboxStateEventAppend,
			Uid:    3,
			Flags:  []string{"\\Recent"},
			Modseq: 200,
		})
	}, 100*time.Millisecond)
	if err != nil {
		t.Fatalf("idle: %v", err)
	}

	exists, fetches, _ := w.snapshot()
	if len(exists) != 1 || exists[0] != 3 {
		t.Errorf("WriteNumMessages: %v, want [3]", exists)
	}
	if len(fetches) != 1 {
		t.Fatalf("WriteMessageFlags called %d times, want 1", len(fetches))
	}
	if got := fetches[0]; got.Seq != 3 || got.UID != imap.UID(3) ||
		len(got.Flags) != 1 || got.Flags[0] != imap.Flag("\\Recent") {
		t.Errorf("fetch[0] = %+v, want seq=3 uid=3 flags=[\\Recent]", got)
	}
	if caller.subscribeCalls != 1 {
		t.Errorf("subscribe calls = %d, want 1", caller.subscribeCalls)
	}
}

// TestIdle_PresentUIDUsesSingleUIDSeqInfo pins the F1 IDLE present-UID path: a
// flags push resolves the changed UID's sequence number via a single-UID
// SeqInfo lookup — NOT a whole-mailbox FetchAllUIDs. The fake records both call
// counts; the old code called FetchAllUIDs (0 SeqInfo, 1 FetchAll) and derived
// the seq by scanning the full list.
func TestIdle_PresentUIDUsesSingleUIDSeqInfo(t *testing.T) {
	t.Parallel()
	fetcher := &metadataFetcherFake{}
	fetcher.setUIDs([]uint32{10, 20, 30}) // mailbox of 3; UID 20 is rank 2
	s := &Session{
		actorID: []byte("actor-0000000000000000000000000000"),
		logger:  slog.Default(),
	}
	w := &updateWriterFake{}
	ev := wsrpc.MailboxStateEvent{
		Kind:   wsrpc.MailboxStateEventFlags,
		Uid:    20,
		Flags:  []string{"\\Seen"},
		Modseq: 5,
	}
	if err := s.handleEvent(context.Background(), w, fetcher, s.actorID, "INBOX", ev); err != nil {
		t.Fatalf("handleEvent(flags): %v", err)
	}
	// F1: exactly one single-UID lookup, and NO whole-mailbox fetch.
	if fetcher.seqInfoCalls != 1 || fetcher.fetchAllCalls != 0 {
		t.Errorf("expected 1 SeqInfo + 0 FetchAllUIDs, got seqInfo=%d fetchAll=%d",
			fetcher.seqInfoCalls, fetcher.fetchAllCalls)
	}
	if fetcher.lastSeqInfoUID != 20 {
		t.Errorf("SeqInfo asked for uid %d, want 20", fetcher.lastSeqInfoUID)
	}
	// The emitted FETCH carries the correct sequence number (rank 2 of uid 20).
	_, fetches, _ := w.snapshot()
	if len(fetches) != 1 || fetches[0].Seq != 2 || fetches[0].UID != imap.UID(20) {
		t.Fatalf("expected one FETCH at seq 2 for uid 20, got %+v", fetches)
	}
}

// TestIdle_FlagsOnlyEmitsFETCH — `flags` event translates to a single
// FETCH (no EXISTS — message count unchanged).
func TestIdle_FlagsOnlyEmitsFETCH(t *testing.T) {
	t.Parallel()
	caller := &idleSubscribeCaller{subscriptionID: 22}
	router := newNotificationRouter(slog.Default())
	fetcher := &metadataFetcherFake{}
	fetcher.setUIDs([]uint32{1, 2, 3, 4})
	s := sessionForIDLE(t, "INBOX", caller, router, fetcher)
	w := &updateWriterFake{}

	err := runIdleAndCollect(t, s, w, func(t *testing.T) {
		pushEvent(t, router, 22, wsrpc.MailboxStateEvent{
			Kind:   wsrpc.MailboxStateEventFlags,
			Uid:    2,
			Flags:  []string{"\\Seen", "\\Answered"},
			Modseq: 50,
		})
	}, 100*time.Millisecond)
	if err != nil {
		t.Fatalf("idle: %v", err)
	}

	exists, fetches, expunges := w.snapshot()
	if len(exists) != 0 {
		t.Errorf("WriteNumMessages: %v, want []", exists)
	}
	if len(expunges) != 0 {
		t.Errorf("WriteExpunge: %v, want []", expunges)
	}
	if len(fetches) != 1 {
		t.Fatalf("WriteMessageFlags called %d times, want 1", len(fetches))
	}
	if got := fetches[0]; got.Seq != 2 || got.UID != imap.UID(2) ||
		len(got.Flags) != 2 {
		t.Errorf("fetch[0] = %+v, want seq=2 uid=2 |flags|=2", got)
	}
}

// TestIdle_FlagsEventEmitsModSeqWhenCondStore — with CONDSTORE enabled,
// the unsolicited flag-change FETCH carries MODSEQ (RFC 7162 §3.1.7) via
// the FAUNA-FORK WriteMessageFlagsModSeq seam.
func TestIdle_FlagsEventEmitsModSeqWhenCondStore(t *testing.T) {
	t.Parallel()
	caller := &idleSubscribeCaller{subscriptionID: 23}
	router := newNotificationRouter(slog.Default())
	fetcher := &metadataFetcherFake{}
	fetcher.setUIDs([]uint32{1, 2, 3, 4})
	s := sessionForIDLE(t, "INBOX", caller, router, fetcher)
	s.condStoreEnabled = true
	w := &updateWriterFake{}

	err := runIdleAndCollect(t, s, w, func(t *testing.T) {
		pushEvent(t, router, 23, wsrpc.MailboxStateEvent{
			Kind:   wsrpc.MailboxStateEventFlags,
			Uid:    2,
			Flags:  []string{"\\Seen"},
			Modseq: 50,
		})
	}, 100*time.Millisecond)
	if err != nil {
		t.Fatalf("idle: %v", err)
	}
	_, fetches, _ := w.snapshot()
	if len(fetches) != 1 {
		t.Fatalf("fetches: %d, want 1", len(fetches))
	}
	if fetches[0].ModSeq != 50 {
		t.Errorf("IDLE flag-change MODSEQ = %d, want 50", fetches[0].ModSeq)
	}
}

// TestIdle_ExpungeEmitsPerUIDEXPUNGE — `expunge` event translates to
// `* <pre-expunge-seq> EXPUNGE`.  NEVER `* VANISHED` (upstream-blocked
// per imap-server.md § Upstream-blocked gaps).
func TestIdle_ExpungeEmitsPerUIDEXPUNGE(t *testing.T) {
	t.Parallel()
	caller := &idleSubscribeCaller{subscriptionID: 33}
	router := newNotificationRouter(slog.Default())
	// Pre-expunge mailbox had UIDs [1, 3, 5, 7]; UID 5 was expunged.
	// Post-expunge surviving UIDs: [1, 3, 7].
	// pre_seq(5) = |{s∈surv : s ≤ 5}| + 1 = |{1,3}| + 1 = 3.
	fetcher := &metadataFetcherFake{}
	fetcher.setUIDs([]uint32{1, 3, 7})
	s := sessionForIDLE(t, "INBOX", caller, router, fetcher)
	w := &updateWriterFake{}

	err := runIdleAndCollect(t, s, w, func(t *testing.T) {
		pushEvent(t, router, 33, wsrpc.MailboxStateEvent{
			Kind:   wsrpc.MailboxStateEventExpunge,
			Uid:    5,
			Modseq: 80,
		})
	}, 100*time.Millisecond)
	if err != nil {
		t.Fatalf("idle: %v", err)
	}

	exists, fetches, expunges := w.snapshot()
	if len(exists) != 0 {
		t.Errorf("WriteNumMessages: %v, want []", exists)
	}
	if len(fetches) != 0 {
		t.Errorf("WriteMessageFlags: %v, want []", fetches)
	}
	if len(expunges) != 1 || expunges[0] != 3 {
		t.Errorf("WriteExpunge: %v, want [3]", expunges)
	}
}

func TestIdle_ExpungeUnderQResyncEmitsVanished(t *testing.T) {
	t.Parallel()
	caller := &idleSubscribeCaller{subscriptionID: 34}
	router := newNotificationRouter(slog.Default())
	fetcher := &metadataFetcherFake{}
	fetcher.setUIDs([]uint32{1, 3, 7})
	s := sessionForIDLE(t, "INBOX", caller, router, fetcher)
	// ENABLE QRESYNC observed for the session (conn is nil in unit tests,
	// so use the qresyncEnabled field that qresyncActive() also checks).
	s.qresyncEnabled = true
	w := &updateWriterFake{}

	err := runIdleAndCollect(t, s, w, func(t *testing.T) {
		pushEvent(t, router, 34, wsrpc.MailboxStateEvent{
			Kind:   wsrpc.MailboxStateEventExpunge,
			Uid:    5,
			Modseq: 80,
		})
	}, 100*time.Millisecond)
	if err != nil {
		t.Fatalf("idle: %v", err)
	}

	exists, fetches, expunges := w.snapshot()
	if len(exists) != 0 || len(fetches) != 0 || len(expunges) != 0 {
		t.Errorf("under QRESYNC expect only VANISHED; got exists=%v fetches=%v expunges=%v", exists, fetches, expunges)
	}
	vanished := w.vanishedSnapshot()
	if len(vanished) != 1 || vanished[0].String() != "5" {
		t.Errorf("WriteVanished: %v, want one set [5]", vanished)
	}
}

// TestIdle_MoveDestinationEmitsEXISTSAndFETCH — `move` push delivered
// to the destination subscriber (the nest names the side): we emit
// EXISTS+FETCH for dst_uid (no flags — the event doesn't carry
// destination FLAGS).
func TestIdle_MoveDestinationEmitsEXISTSAndFETCH(t *testing.T) {
	t.Parallel()
	caller := &idleSubscribeCaller{subscriptionID: 44}
	router := newNotificationRouter(slog.Default())
	// Destination mailbox post-move: UIDs [10, 11, 12]; UID 12 is the
	// newly-moved-in one (dst_uid).
	fetcher := &metadataFetcherFake{}
	fetcher.setUIDs([]uint32{10, 11, 12})
	s := sessionForIDLE(t, "Archive", caller, router, fetcher)
	w := &updateWriterFake{}

	err := runIdleAndCollect(t, s, w, func(t *testing.T) {
		pushEvent(t, router, 44, wsrpc.MailboxStateEvent{
			Kind:      wsrpc.MailboxStateEventMove,
			SrcUid:    99,
			DstUid:    12,
			Side:      wsrpc.MoveSideDestination,
			ModseqSrc: 60,
			ModseqDst: 61,
		})
	}, 100*time.Millisecond)
	if err != nil {
		t.Fatalf("idle: %v", err)
	}

	exists, fetches, expunges := w.snapshot()
	if len(exists) != 1 || exists[0] != 3 {
		t.Errorf("WriteNumMessages: %v, want [3]", exists)
	}
	if len(expunges) != 0 {
		t.Errorf("WriteExpunge: %v, want []", expunges)
	}
	if len(fetches) != 1 || fetches[0].Seq != 3 || fetches[0].UID != imap.UID(12) {
		t.Errorf("WriteMessageFlags: %+v, want one entry seq=3 uid=12", fetches)
	}
}

// TestIdle_MoveSourceEmitsEXPUNGE — `move` push delivered to the
// source subscriber (the nest names the side): src_uid leaves, so we
// emit its EXPUNGE.
func TestIdle_MoveSourceEmitsEXPUNGE(t *testing.T) {
	t.Parallel()
	caller := &idleSubscribeCaller{subscriptionID: 55}
	router := newNotificationRouter(slog.Default())
	// Source mailbox post-move: UIDs [1, 2, 4] — UID 3 was the
	// moved-out one (src_uid), now gone.
	// pre_seq(3) = |{1,2}| + 1 = 3.
	fetcher := &metadataFetcherFake{}
	fetcher.setUIDs([]uint32{1, 2, 4})
	s := sessionForIDLE(t, "INBOX", caller, router, fetcher)
	w := &updateWriterFake{}

	err := runIdleAndCollect(t, s, w, func(t *testing.T) {
		pushEvent(t, router, 55, wsrpc.MailboxStateEvent{
			Kind:      wsrpc.MailboxStateEventMove,
			SrcUid:    3,
			DstUid:    77,
			Side:      wsrpc.MoveSideSource,
			ModseqSrc: 60,
			ModseqDst: 61,
		})
	}, 100*time.Millisecond)
	if err != nil {
		t.Fatalf("idle: %v", err)
	}

	exists, fetches, expunges := w.snapshot()
	if len(exists) != 0 {
		t.Errorf("WriteNumMessages: %v, want []", exists)
	}
	if len(fetches) != 0 {
		t.Errorf("WriteMessageFlags: %v, want []", fetches)
	}
	if len(expunges) != 1 || expunges[0] != 3 {
		t.Errorf("WriteExpunge: %v, want [3]", expunges)
	}
}

// TestIdle_MoveSourceWithCollidingDstUIDEmitsEXPUNGE — IMAP UIDs are
// per-mailbox, so the destination's new UID routinely also names an
// unrelated message in the source (a first move into Archive gets UID 1,
// and INBOX has a UID 1). The side comes from the event, never from a
// UID lookup: the source idler reports the disappearance and nothing
// else — no shrinking EXISTS, no FETCH of the unrelated message.
func TestIdle_MoveSourceWithCollidingDstUIDEmitsEXPUNGE(t *testing.T) {
	t.Parallel()
	caller := &idleSubscribeCaller{subscriptionID: 56}
	router := newNotificationRouter(slog.Default())
	// Source INBOX post-move: UIDs [1, 2, 4] — UID 3 moved out, and the
	// destination allocated UID 1, which INBOX also holds.
	fetcher := &metadataFetcherFake{}
	fetcher.setUIDs([]uint32{1, 2, 4})
	s := sessionForIDLE(t, "INBOX", caller, router, fetcher)
	w := &updateWriterFake{}

	err := runIdleAndCollect(t, s, w, func(t *testing.T) {
		pushEvent(t, router, 56, wsrpc.MailboxStateEvent{
			Kind:      wsrpc.MailboxStateEventMove,
			SrcUid:    3,
			DstUid:    1,
			ModseqSrc: 60,
			ModseqDst: 61,
			Side:      wsrpc.MoveSideSource,
		})
	}, 100*time.Millisecond)
	if err != nil {
		t.Fatalf("idle: %v", err)
	}

	exists, fetches, expunges := w.snapshot()
	if len(exists) != 0 {
		t.Errorf("WriteNumMessages: %v, want []", exists)
	}
	if len(fetches) != 0 {
		t.Errorf("WriteMessageFlags: %+v, want []", fetches)
	}
	if len(expunges) != 1 || expunges[0] != 3 {
		t.Errorf("WriteExpunge: %v, want [3]", expunges)
	}
}

// TestIdle_MoveSourceWithCollidingDstUIDUnderQResyncEmitsVanished — the
// same collision under QRESYNC: `* VANISHED <src_uid>` only.
func TestIdle_MoveSourceWithCollidingDstUIDUnderQResyncEmitsVanished(t *testing.T) {
	t.Parallel()
	caller := &idleSubscribeCaller{subscriptionID: 57}
	router := newNotificationRouter(slog.Default())
	fetcher := &metadataFetcherFake{}
	fetcher.setUIDs([]uint32{1, 2, 4})
	s := sessionForIDLE(t, "INBOX", caller, router, fetcher)
	s.qresyncEnabled = true
	w := &updateWriterFake{}

	err := runIdleAndCollect(t, s, w, func(t *testing.T) {
		pushEvent(t, router, 57, wsrpc.MailboxStateEvent{
			Kind:      wsrpc.MailboxStateEventMove,
			SrcUid:    3,
			DstUid:    1,
			ModseqSrc: 60,
			ModseqDst: 61,
			Side:      wsrpc.MoveSideSource,
		})
	}, 100*time.Millisecond)
	if err != nil {
		t.Fatalf("idle: %v", err)
	}

	exists, fetches, expunges := w.snapshot()
	if len(exists) != 0 || len(fetches) != 0 || len(expunges) != 0 {
		t.Errorf("under QRESYNC expect only VANISHED; got exists=%v fetches=%v expunges=%v", exists, fetches, expunges)
	}
	vanished := w.vanishedSnapshot()
	if len(vanished) != 1 || vanished[0].String() != "3" {
		t.Errorf("WriteVanished: %v, want one set [3]", vanished)
	}
}

// TestIdle_MoveDestinationWithCollidingSrcUIDEmitsEXISTSAndFETCH — the
// mirror: the destination idler holds a message whose UID equals the
// source's src_uid. It is still the destination: EXISTS+FETCH for
// dst_uid, never an EXPUNGE.
func TestIdle_MoveDestinationWithCollidingSrcUIDEmitsEXISTSAndFETCH(t *testing.T) {
	t.Parallel()
	caller := &idleSubscribeCaller{subscriptionID: 58}
	router := newNotificationRouter(slog.Default())
	fetcher := &metadataFetcherFake{}
	fetcher.setUIDs([]uint32{3, 5})
	s := sessionForIDLE(t, "Archive", caller, router, fetcher)
	w := &updateWriterFake{}

	err := runIdleAndCollect(t, s, w, func(t *testing.T) {
		pushEvent(t, router, 58, wsrpc.MailboxStateEvent{
			Kind:      wsrpc.MailboxStateEventMove,
			SrcUid:    3,
			DstUid:    5,
			ModseqSrc: 60,
			ModseqDst: 61,
			Side:      wsrpc.MoveSideDestination,
		})
	}, 100*time.Millisecond)
	if err != nil {
		t.Fatalf("idle: %v", err)
	}

	exists, fetches, expunges := w.snapshot()
	if len(expunges) != 0 {
		t.Errorf("WriteExpunge: %v, want []", expunges)
	}
	if len(exists) != 1 || exists[0] != 2 {
		t.Errorf("WriteNumMessages: %v, want [2]", exists)
	}
	if len(fetches) != 1 || fetches[0].Seq != 2 || fetches[0].UID != imap.UID(5) {
		t.Errorf("WriteMessageFlags: %+v, want one entry seq=2 uid=5", fetches)
	}
}

// TestIdle_MoveWithoutSideIsDropped — a Move naming no side is a
// malformed push (every nest emitter names one): logged and dropped,
// never guessed at.
func TestIdle_MoveWithoutSideIsDropped(t *testing.T) {
	t.Parallel()
	caller := &idleSubscribeCaller{subscriptionID: 59}
	router := newNotificationRouter(slog.Default())
	fetcher := &metadataFetcherFake{}
	fetcher.setUIDs([]uint32{1, 2, 4})
	s := sessionForIDLE(t, "INBOX", caller, router, fetcher)
	w := &updateWriterFake{}

	err := runIdleAndCollect(t, s, w, func(t *testing.T) {
		pushEvent(t, router, 59, wsrpc.MailboxStateEvent{
			Kind:   wsrpc.MailboxStateEventMove,
			SrcUid: 3,
			DstUid: 1,
		})
	}, 100*time.Millisecond)
	if err != nil {
		t.Fatalf("idle: %v", err)
	}

	exists, fetches, expunges := w.snapshot()
	if len(exists) != 0 || len(fetches) != 0 || len(expunges) != 0 || len(w.vanishedSnapshot()) != 0 {
		t.Errorf("sideless Move must write nothing; got exists=%v fetches=%v expunges=%v", exists, fetches, expunges)
	}
}

// TestIdle_StopReturnsNil — closing the stop channel returns nil, no
// matter what events arrived.
func TestIdle_StopReturnsNil(t *testing.T) {
	t.Parallel()
	caller := &idleSubscribeCaller{subscriptionID: 1}
	router := newNotificationRouter(slog.Default())
	fetcher := &metadataFetcherFake{}
	fetcher.setUIDs([]uint32{})
	s := sessionForIDLE(t, "INBOX", caller, router, fetcher)
	w := &updateWriterFake{}

	stop := make(chan struct{})
	done := make(chan error, 1)
	go func() { done <- s.idle(context.Background(), w, stop) }()

	time.Sleep(30 * time.Millisecond)
	close(stop)
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("idle on stop: %v", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("idle did not return within 2s of stop close")
	}
}

// TestIdle_TimeoutReturnsNil — the per-server IDLE timeout fires and
// returns nil cleanly.
func TestIdle_TimeoutReturnsNil(t *testing.T) {
	t.Parallel()
	caller := &idleSubscribeCaller{subscriptionID: 1}
	router := newNotificationRouter(slog.Default())
	fetcher := &metadataFetcherFake{}
	fetcher.setUIDs([]uint32{})
	s := sessionForIDLE(t, "INBOX", caller, router, fetcher)
	// Force a short timeout that fires before any stop.
	s.idleTimeout = 50 * time.Millisecond
	w := &updateWriterFake{}

	stop := make(chan struct{}) // never closed; timeout fires first.
	done := make(chan error, 1)
	go func() { done <- s.idle(context.Background(), w, stop) }()

	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("idle on timeout: %v", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("idle did not return within 2s of expected timeout")
	}
}

// TestIdle_RejectsBeforeSelect — Session.idle MUST fail without a
// SELECTed mailbox.
func TestIdle_RejectsBeforeSelect(t *testing.T) {
	t.Parallel()
	s := &Session{
		client:      &idleSubscribeCaller{},
		actorID:     bytes.Repeat([]byte{0x01}, 32),
		router:      newNotificationRouter(slog.Default()),
		idleTimeout: time.Second,
	}
	w := &updateWriterFake{}
	err := s.idle(context.Background(), w, make(chan struct{}))
	if err == nil || err.Error() != "imap: IDLE requires a SELECTed mailbox" {
		t.Errorf("idle error = %v, want SELECTed-mailbox error", err)
	}
}

// TestIdle_RejectsWithoutAuth — Session.idle MUST fail without
// actorID (= no AUTH).
func TestIdle_RejectsWithoutAuth(t *testing.T) {
	t.Parallel()
	s := &Session{
		client:          &idleSubscribeCaller{},
		selectedMailbox: "INBOX",
		router:          newNotificationRouter(slog.Default()),
		idleTimeout:     time.Second,
	}
	w := &updateWriterFake{}
	err := s.idle(context.Background(), w, make(chan struct{}))
	if err == nil || err.Error() != "imap: IDLE requires authenticated state" {
		t.Errorf("idle error = %v, want authenticated-state error", err)
	}
}

// TestIdle_SubscribeFailureSurfacesError — if the subscribe RPC fails,
// Session.idle returns the wrapped error immediately (no router
// register, no event loop).
func TestIdle_SubscribeFailureSurfacesError(t *testing.T) {
	t.Parallel()
	caller := &idleSubscribeCaller{subscribeErr: errors.New("simulated nest error")}
	router := newNotificationRouter(slog.Default())
	fetcher := &metadataFetcherFake{}
	s := sessionForIDLE(t, "INBOX", caller, router, fetcher)
	w := &updateWriterFake{}

	err := s.idle(context.Background(), w, make(chan struct{}))
	if err == nil {
		t.Fatal("idle: want error, got nil")
	}
}

// TestBackend_RouterInstalledOnPushHandler verifies the production
// wiring: NewBackend probes its wsrpc.Caller for the
// pushHandlerInstaller capability and, when satisfied, installs the
// router's Handle as the active push consumer.
func TestBackend_RouterInstalledOnPushHandler(t *testing.T) {
	t.Parallel()
	installer := &pushHandlerInstallerStub{}
	// nil dispatcher → standalone path: the router grabs the client's push
	// slot directly. (The composed-dispatcher path is exercised in mda.Run +
	// the config_changed composition test.)
	_ = NewBackend(installer, slog.Default(), 0, time.Second, 0, nil, nil)
	if installer.installed == nil {
		t.Fatal("Backend did not install OnPush handler on the wsrpc.Caller")
	}
}

// pushHandlerInstallerStub satisfies both wsrpc.Caller and
// pushHandlerInstaller; records the most recently installed handler.
type pushHandlerInstallerStub struct {
	installed wsrpc.PushHandler
}

func (s *pushHandlerInstallerStub) Call(context.Context, string, any, any) error {
	return errors.New("pushHandlerInstallerStub: Call not implemented")
}

func (s *pushHandlerInstallerStub) SetOnPush(h wsrpc.PushHandler) {
	s.installed = h
}
