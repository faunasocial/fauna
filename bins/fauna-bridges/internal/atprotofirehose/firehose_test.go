package atprotofirehose

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	comatproto "github.com/bluesky-social/indigo/api/atproto"
	"github.com/bluesky-social/indigo/atproto/atcrypto"
	"github.com/bluesky-social/indigo/atproto/syntax"
	"github.com/bluesky-social/indigo/events"
	"github.com/bluesky-social/indigo/events/schedulers/sequential"
	gorilla "github.com/gorilla/websocket"
	"nhooyr.io/websocket"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotorepo"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/xrpc"
)

const (
	testDID      = "did:plc:fire0000000000000000000000"
	feedPostNSID = "app.bsky.feed.post"

	// waitBudget bounds every harness wait. Generous by design: these tests
	// share a machine with a whole fleet of builds and sessions, so ceilings
	// are sized far above any non-pathological delay — a green run pays
	// nothing (every wait returns the moment its condition holds), only a
	// genuine failure spends the budget (testing.md § convention 14).
	waitBudget = 60 * time.Second
)

func quietLogger() *slog.Logger { return slog.New(slog.NewTextHandler(io.Discard, nil)) }

// harness is a real store + funnel + broadcaster behind a real xrpc.Server on an
// httptest listener — the same stack production wires, minus TLS and the FFI.
type harness struct {
	store  *atprotorepo.Store
	funnel *atprotorepo.Funnel
	bc     *Broadcaster
	signer atcrypto.PrivateKey
	url    string
	clock  *syntax.TIDClock
}

func newHarness(t *testing.T) *harness {
	return newHarnessKeepalive(t, pingInterval, pongTimeout)
}

// newHarnessKeepalive is newHarness with the keepalive cadence injected, so the
// eviction test does not wait out the production interval.
func newHarnessKeepalive(t *testing.T, ping, pong time.Duration) *harness {
	t.Helper()
	ctx := context.Background()
	store, err := atprotorepo.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { store.Close() })
	funnel, err := atprotorepo.NewFunnel(ctx, store, nil)
	if err != nil {
		t.Fatal(err)
	}
	signer, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	// The funnel refuses to emit for an identity that has never been announced
	// (atprotorepo.ErrFirstEmitGated). These are transport tests, so the account
	// is past that point exactly as it is in production before any frame.
	if err := store.SetFirstEmitGated(ctx, testDID, false); err != nil {
		t.Fatalf("open first-emit gate: %v", err)
	}
	bc := New(store, quietLogger())
	funnel.SetOnCommit(bc.Notify)

	srv := xrpc.NewServer(nil, nil, nil, nil, quietLogger())
	registerWithKeepalive(srv, bc, quietLogger(), ping, pong)
	ts := httptest.NewServer(srv)
	t.Cleanup(ts.Close)

	return &harness{
		store:  store,
		funnel: funnel,
		bc:     bc,
		signer: signer,
		url:    "ws" + strings.TrimPrefix(ts.URL, "http") + "/xrpc/com.atproto.sync.subscribeRepos",
		clock:  syntax.NewTIDClock(0),
	}
}

// run starts the emitter for the duration of the test, returning only once it
// has observably seeded its cursor from the outbox head. Without that barrier
// every caller races the seed: on a loaded machine the Run goroutine can be
// scheduled late enough that frames the test commits AFTER run() are already
// in the outbox when the seed reads the head — reclassified as pre-start
// history and never fanned out. The flood test then can never overflow the
// subscriber buffer no matter how long it waits (the exact way
// TestSlowConsumerIsDropped used to burn its whole deadline under load), and
// single-post tests never see their frame arrive. Production cannot hit this:
// Run starts well before the listener binds, so nothing can attach or commit
// in the window.
func (h *harness) run(t *testing.T) context.Context {
	t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	t.Cleanup(cancel)
	done := make(chan struct{})
	go func() { defer close(done); h.bc.Run(ctx) }()
	t.Cleanup(func() { cancel(); <-done })
	deadline := time.Now().Add(waitBudget)
	for !h.bc.Seeded() && time.Now().Before(deadline) {
		time.Sleep(time.Millisecond)
	}
	if !h.bc.Seeded() {
		t.Fatal("emitter never seeded its cursor")
	}
	return ctx
}

// post commits one public post into testDID's repo, returning its firehose seq.
func (h *harness) post(t *testing.T, text string) int64 {
	t.Helper()
	rec, err := atprotorepo.JSONRecordToDagCBOR(
		`{"$type":"app.bsky.feed.post","text":"` + text + `","createdAt":"2026-01-01T00:00:00Z"}`)
	if err != nil {
		t.Fatal(err)
	}
	res, err := h.funnel.ApplyBatch(context.Background(), testDID, h.signer, []atprotorepo.RepoOp{{
		Action:      atprotorepo.ActionCreate,
		Collection:  feedPostNSID,
		Rkey:        h.clock.Next().String(),
		RecordCBOR:  rec,
		FaunaPostID: text,
	}})
	if err != nil {
		t.Fatal(err)
	}
	return res.Seq
}

// deferredPost is post's collapse-mode twin: the commit lands in the repo but
// its #commit frame is withheld, the state a huge downtime catch-up builds up
// before its single #sync (S5 slice 4).
func (h *harness) deferredPost(t *testing.T, text string) {
	t.Helper()
	rec, err := atprotorepo.JSONRecordToDagCBOR(
		`{"$type":"app.bsky.feed.post","text":"` + text + `","createdAt":"2026-01-01T00:00:00Z"}`)
	if err != nil {
		t.Fatal(err)
	}
	res, err := h.funnel.ApplyBatch(context.Background(), testDID, h.signer, []atprotorepo.RepoOp{{
		Action:      atprotorepo.ActionCreate,
		Collection:  feedPostNSID,
		Rkey:        h.clock.Next().String(),
		RecordCBOR:  rec,
		FaunaPostID: text,
	}}, atprotorepo.DeferFrameToSync())
	if err != nil {
		t.Fatal(err)
	}
	if res.Seq != 0 {
		t.Fatalf("deferred post allocated firehose seq %d, want none", res.Seq)
	}
}

// dial opens a subscriber connection, optionally with a cursor.
func (h *harness) dial(t *testing.T, ctx context.Context, cursor string) *websocket.Conn {
	t.Helper()
	url := h.url
	if cursor != "" {
		url += "?cursor=" + cursor
	}
	conn, _, err := websocket.Dial(ctx, url, nil)
	if err != nil {
		t.Fatalf("dial %s: %v", url, err)
	}
	t.Cleanup(func() { conn.CloseNow() })
	return conn
}

// waitCursor blocks until the emitter's cursor reaches seq — i.e. the drain
// has fanned out (or, for frames staged before run(), seeded past) everything
// up to it. The seed itself is already synchronized inside run(); this waits
// for drain progress beyond that point.
func (h *harness) waitCursor(t *testing.T, seq int64) {
	t.Helper()
	deadline := time.Now().Add(waitBudget)
	for time.Now().Before(deadline) {
		if h.bc.CursorSeq() >= seq {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("emitter cursor never reached %d (now %d)", seq, h.bc.CursorSeq())
}

// waitSubscribers blocks until the broadcaster sees n consumers, so a test never
// races the handler's attach.
func (h *harness) waitSubscribers(t *testing.T, n int) {
	t.Helper()
	deadline := time.Now().Add(waitBudget)
	for time.Now().Before(deadline) {
		if h.bc.SubscriberCount() == n {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("subscriber count never reached %d (now %d)", n, h.bc.SubscriberCount())
}

// dialIndigo opens a subscription with gorilla — the connection type indigo's
// own stream consumer accepts. Production never uses gorilla; this is purely how
// we hand our frames to the parser a relay runs.
func dialIndigo(t *testing.T, url string) *gorilla.Conn {
	t.Helper()
	conn, _, err := gorilla.DefaultDialer.Dial(url, nil)
	if err != nil {
		t.Fatalf("gorilla dial %s: %v", url, err)
	}
	t.Cleanup(func() { conn.Close() })
	return conn
}

func readFrame(t *testing.T, ctx context.Context, conn *websocket.Conn) []byte {
	t.Helper()
	rctx, cancel := context.WithTimeout(ctx, waitBudget)
	defer cancel()
	typ, data, err := conn.Read(rctx)
	if err != nil {
		t.Fatalf("read frame: %v", err)
	}
	if typ != websocket.MessageBinary {
		t.Fatalf("frame type = %v, want binary", typ)
	}
	return data
}

// TestLiveCommitReachesSubscriber is the core end-to-end assertion of task 7b:
// a post committed through the funnel lands on a connected consumer's socket.
func TestLiveCommitReachesSubscriber(t *testing.T) {
	h := newHarness(t)
	ctx := h.run(t)
	conn := h.dial(t, ctx, "")
	h.waitSubscribers(t, 1)

	seq := h.post(t, "hello")
	frame := readFrame(t, ctx, conn)
	if len(frame) == 0 {
		t.Fatal("empty frame")
	}
	if seq != 1 {
		t.Fatalf("first commit seq = %d, want 1", seq)
	}
}

// TestFramesParseWithIndigoStreamConsumer is the conformance proof: our frames
// are consumed by indigo's OWN repo-stream parser — the library a relay runs —
// rather than by an assertion we wrote about our own bytes. It also pins the
// Sync v1.1 inductive `prevData` field: the second commit must carry the first
// commit's MST root.
func TestFramesParseWithIndigoStreamConsumer(t *testing.T) {
	h := newHarness(t)
	ctx := h.run(t)
	conn := dialIndigo(t, h.url)
	h.waitSubscribers(t, 1)

	h.post(t, "first")
	h.post(t, "second")

	var mu sync.Mutex
	var commits []*comatproto.SyncSubscribeRepos_Commit
	got := make(chan struct{}, 8)
	callbacks := &events.RepoStreamCallbacks{
		RepoCommit: func(evt *comatproto.SyncSubscribeRepos_Commit) error {
			mu.Lock()
			commits = append(commits, evt)
			mu.Unlock()
			got <- struct{}{}
			return nil
		},
	}
	streamCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	go func() {
		sched := sequential.NewScheduler("test", callbacks.EventHandler)
		_ = events.HandleRepoStream(streamCtx, conn, sched, quietLogger())
	}()

	for i := 0; i < 2; i++ {
		select {
		case <-got:
		case <-time.After(10 * time.Second):
			t.Fatalf("indigo consumer parsed only %d of 2 #commit frames", i)
		}
	}
	mu.Lock()
	defer mu.Unlock()

	if commits[0].Repo != testDID {
		t.Errorf("commit repo = %q, want %q", commits[0].Repo, testDID)
	}
	if commits[0].Seq != 1 || commits[1].Seq != 2 {
		t.Errorf("seqs = %d,%d, want 1,2", commits[0].Seq, commits[1].Seq)
	}
	// Genesis has no predecessor; the second commit is inductive.
	if commits[0].PrevData != nil {
		t.Errorf("genesis commit carries prevData %v, want nil", commits[0].PrevData)
	}
	if commits[1].PrevData == nil {
		t.Fatal("second commit has no prevData — Sync v1.1 inductive field missing")
	}
	if commits[1].Since == nil || *commits[1].Since != commits[0].Rev {
		t.Errorf("second commit since = %v, want %q", commits[1].Since, commits[0].Rev)
	}
	// prevData must be the MST root the FIRST commit published, not its commit CID.
	if prev := commits[1].PrevData.String(); prev == commits[0].Commit.String() {
		t.Errorf("prevData = commit CID %s; want the previous MST root", prev)
	}
}

// TestCursorReplayServesHistoryThenLive proves a reconnecting consumer gets the
// frames it missed and then follows the tail, with no gap and no duplicate.
func TestCursorReplayServesHistoryThenLive(t *testing.T) {
	h := newHarness(t)
	ctx := h.run(t)

	h.post(t, "one")
	h.post(t, "two")
	h.post(t, "three")

	// Reconnect claiming to have seen through seq 1.
	conn := h.dial(t, ctx, "1")
	h.waitSubscribers(t, 1)

	for _, want := range []int64{2, 3} {
		frame := readFrame(t, ctx, conn)
		if seq := seqOf(t, frame); seq != want {
			t.Fatalf("replayed seq = %d, want %d", seq, want)
		}
	}
	// Then the live tail continues from 4 — no duplicate of 2/3.
	h.post(t, "four")
	if seq := seqOf(t, readFrame(t, ctx, conn)); seq != 4 {
		t.Fatalf("live seq after replay = %d, want 4", seq)
	}
}

// TestCursorOutsideRetentionDegradesToSync pins the sanctioned degraded path:
// a cursor whose frames aged out yields one #sync per repo (authoritative head)
// instead of a silent gap, so the relay refetches with getRepo.
func TestCursorOutsideRetentionDegradesToSync(t *testing.T) {
	h := newHarness(t)
	ctx := h.run(t)

	h.post(t, "one")
	h.post(t, "two")
	// Age the first frame out of the window and prune it.
	if _, err := h.store.DB().ExecContext(ctx,
		`UPDATE firehose_events SET created_at = ? WHERE seq = 1`,
		time.Now().Add(-2*atprotorepo.EventRetention).UnixMicro()); err != nil {
		t.Fatal(err)
	}
	if n, err := h.store.PruneEvents(ctx, time.Now()); err != nil || n != 1 {
		t.Fatalf("prune removed %d rows (err %v), want 1", n, err)
	}

	// Cursor 0 wants seq 1, which no longer exists.
	conn := h.dial(t, ctx, "0")
	frame := readFrame(t, ctx, conn)

	var sync *comatproto.SyncSubscribeRepos_Sync
	done := make(chan struct{})
	callbacks := &events.RepoStreamCallbacks{
		RepoSync: func(evt *comatproto.SyncSubscribeRepos_Sync) error {
			sync = evt
			close(done)
			return nil
		},
	}
	// Feed the single frame through indigo's parser via a loopback socket, so
	// the degraded frame is validated by the same library a relay uses.
	replayFrameThroughIndigo(t, ctx, frame, callbacks, done)
	if sync == nil {
		t.Fatal("degraded path did not emit a #sync frame")
	}
	if sync.Did != testDID {
		t.Errorf("#sync did = %q, want %q", sync.Did, testDID)
	}
	if len(sync.Blocks) == 0 {
		t.Error("#sync carries no commit block")
	}
}

// TestReplayGapDegradesToSync covers the narrow race the retention sweep opens:
// the outbox runs dry mid-replay (a prune swept the window while the consumer
// was reading it). The handover must degrade to #sync, never silently jump the
// gap — a consumer that skips commits it was never told about is exactly the
// corruption #sync exists to prevent.
func TestReplayGapDegradesToSync(t *testing.T) {
	h := newHarness(t)
	ctx := h.run(t)
	h.post(t, "one")
	h.post(t, "two")
	h.post(t, "three")
	// Let the emitter reach seq 3, so a replay's handover point is 3.
	h.waitCursor(t, 3)

	// Now the tail of the replay range disappears while seq 1 survives: the
	// retention check at replay start still passes (seq 1 is retained), but the
	// rows between it and the handover point are gone.
	if _, err := h.store.DB().ExecContext(ctx,
		`DELETE FROM firehose_events WHERE seq IN (2,3)`); err != nil {
		t.Fatal(err)
	}

	conn := h.dial(t, ctx, "0")
	if seq := seqOf(t, readFrame(t, ctx, conn)); seq != 1 {
		t.Fatalf("first replayed seq = %d, want the surviving frame 1", seq)
	}

	var got *comatproto.SyncSubscribeRepos_Sync
	done := make(chan struct{})
	replayFrameThroughIndigo(t, ctx, readFrame(t, ctx, conn), &events.RepoStreamCallbacks{
		RepoSync: func(evt *comatproto.SyncSubscribeRepos_Sync) error {
			got = evt
			close(done)
			return nil
		},
	}, done)
	if got == nil || got.Did != testDID {
		t.Fatalf("mid-replay gap did not degrade to #sync (got %+v)", got)
	}
}

// TestPruneKeepsLatestFrame guards the retention floor: however quiet a PDS is,
// its most recent frame stays, so SeqRange keeps answering and a head cursor
// still validates.
func TestPruneKeepsLatestFrame(t *testing.T) {
	h := newHarness(t)
	ctx := context.Background()
	h.post(t, "only")
	if _, err := h.store.DB().ExecContext(ctx,
		`UPDATE firehose_events SET created_at = ?`,
		time.Now().Add(-100*atprotorepo.EventRetention).UnixMicro()); err != nil {
		t.Fatal(err)
	}
	n, err := h.store.PruneEvents(ctx, time.Now())
	if err != nil {
		t.Fatal(err)
	}
	if n != 0 {
		t.Fatalf("pruned %d rows, want 0 (the last frame is always kept)", n)
	}
	if _, _, ok, err := h.store.SeqRange(ctx); err != nil || !ok {
		t.Fatalf("SeqRange after prune: ok=%v err=%v, want ok", ok, err)
	}
}

// TestFutureCursorIsRejected proves a consumer ahead of our stream is told so
// (and disconnected) rather than silently hung.
func TestFutureCursorIsRejected(t *testing.T) {
	h := newHarness(t)
	ctx := h.run(t)
	h.post(t, "one")

	conn := h.dial(t, ctx, "99")
	frame := readFrame(t, ctx, conn)
	if !strings.Contains(string(frame), "FutureCursor") {
		t.Fatalf("error frame does not name FutureCursor: %q", frame)
	}
	rctx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	if _, _, err := conn.Read(rctx); err == nil {
		t.Fatal("connection stayed open after FutureCursor")
	}
}

// TestSlowConsumerIsDropped pins the no-unbounded-buffering contract: a consumer
// that falls further behind than its buffer is dropped, not queued forever.
//
// Asserted at the broadcaster, not through a socket: a real socket's kernel and
// WS buffers absorb hundreds of small frames before a write ever blocks, so a
// "connect and don't read" test proves only that the OS has buffer space. Here
// the subscriber is attached directly and never drained, which is exactly the
// state a stalled consumer puts us in.
//
// The causal chain run()'s seed barrier buys: the subscriber attaches after the
// seed, so every one of the subscriberBuffer+16 frames posted below is past the
// seeded cursor and MUST be offered to it. Nobody drains, so the buffer is full
// after subscriberBuffer deliveries and a later offer must drop. The drop is
// causally inevitable — only latency remains, so an event wait under the
// generous shared budget gives a trustworthy verdict at any machine load.
func TestSlowConsumerIsDropped(t *testing.T) {
	h := newHarness(t)
	h.run(t)

	sub, _, err := h.bc.subscribe("127.0.0.1")
	if err != nil {
		t.Fatalf("subscribe: %v", err)
	}
	for i := 0; i < subscriberBuffer+16; i++ {
		h.post(t, "spam")
	}
	select {
	case <-sub.dropped:
	case <-time.After(waitBudget):
		t.Fatal("consumer that fell behind was never dropped")
	}
	if n := h.bc.SubscriberCount(); n != 0 {
		t.Fatalf("dropped subscriber still attached (count %d)", n)
	}
}

// TestFramesAreNotRebroadcastAtStartup pins the emitter's seed: frames already
// in the outbox when the process starts are history (cursor-replayable), never
// pushed at a fresh live subscriber.
func TestFramesAreNotRebroadcastAtStartup(t *testing.T) {
	h := newHarness(t)
	h.post(t, "before start")
	ctx := h.run(t)

	// A read whose context expires poisons an nhooyr connection, so the
	// nothing-arrives probe gets its own throwaway conn.
	probe := h.dial(t, ctx, "")
	h.waitSubscribers(t, 1)
	rctx, cancel := context.WithTimeout(ctx, 500*time.Millisecond)
	defer cancel()
	if _, _, err := probe.Read(rctx); err == nil {
		t.Fatal("pre-start frame was re-broadcast to a live subscriber")
	}
	probe.CloseNow()
	h.waitSubscribers(t, 0)

	// A new commit still arrives on a fresh subscriber.
	conn := h.dial(t, ctx, "")
	h.waitSubscribers(t, 1)
	h.post(t, "after start")
	if seq := seqOf(t, readFrame(t, ctx, conn)); seq != 2 {
		t.Fatalf("live seq = %d, want 2", seq)
	}
}

// seqOf parses a frame through indigo's consumer and returns its #commit seq.
func seqOf(t *testing.T, frame []byte) int64 {
	t.Helper()
	var seq int64
	done := make(chan struct{})
	callbacks := &events.RepoStreamCallbacks{
		RepoCommit: func(evt *comatproto.SyncSubscribeRepos_Commit) error {
			seq = evt.Seq
			close(done)
			return nil
		},
	}
	replayFrameThroughIndigo(t, context.Background(), frame, callbacks, done)
	return seq
}

// replayFrameThroughIndigo serves `frame` from a throwaway WS endpoint and runs
// indigo's HandleRepoStream against it, so every assertion about a frame's
// CONTENT is made on what indigo's parser produced, never on our own encoder.
func replayFrameThroughIndigo(t *testing.T, ctx context.Context, frame []byte,
	callbacks *events.RepoStreamCallbacks, done chan struct{}) {
	t.Helper()
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		c, err := websocket.Accept(w, r, &websocket.AcceptOptions{InsecureSkipVerify: true})
		if err != nil {
			return
		}
		defer c.CloseNow()
		wctx, cancel := context.WithTimeout(r.Context(), 5*time.Second)
		defer cancel()
		_ = c.Write(wctx, websocket.MessageBinary, frame)
		<-r.Context().Done()
	}))
	defer ts.Close()

	conn := dialIndigo(t, "ws"+strings.TrimPrefix(ts.URL, "http"))

	streamCtx, streamCancel := context.WithCancel(ctx)
	defer streamCancel()
	go func() {
		sched := sequential.NewScheduler("frame", callbacks.EventHandler)
		_ = events.HandleRepoStream(streamCtx, conn, sched, quietLogger())
	}()
	select {
	case <-done:
	case <-time.After(10 * time.Second):
		t.Fatal("indigo consumer never parsed the frame")
	}
}

// TestIdentityFrameReachesSubscribersAndParsesInIndigo is the rename hook's
// conformance proof (task 8): the frame Funnel.EmitIdentity produces is fanned
// by the broadcaster exactly like a #commit — it is frame-type agnostic — and
// INDIGO's own stream consumer, the parser a relay runs, decodes it as an
// #identity carrying the new handle. Interleaving it with commits pins that the
// two producers share one PDS-global seq line with no collision and no hole.
func TestIdentityFrameReachesSubscribersAndParsesInIndigo(t *testing.T) {
	h := newHarness(t)
	ctx := h.run(t)
	conn := h.dial(t, ctx, "")
	h.waitSubscribers(t, 1)

	_ = h.post(t, "before the rename")
	if seq := seqOf(t, readFrame(t, ctx, conn)); seq != 1 {
		t.Fatalf("first commit seq = %d, want 1", seq)
	}

	seq, err := h.funnel.EmitIdentity(ctx, testDID, "renamed.example.com")
	if err != nil {
		t.Fatalf("EmitIdentity: %v", err)
	}
	if seq != 2 {
		t.Fatalf("#identity seq = %d, want 2 (one seq line shared with #commit)", seq)
	}

	frame := readFrame(t, ctx, conn)
	var got *comatproto.SyncSubscribeRepos_Identity
	done := make(chan struct{})
	callbacks := &events.RepoStreamCallbacks{
		RepoIdentity: func(evt *comatproto.SyncSubscribeRepos_Identity) error {
			got = evt
			close(done)
			return nil
		},
	}
	replayFrameThroughIndigo(t, ctx, frame, callbacks, done)
	if got == nil {
		t.Fatal("indigo's consumer did not parse the frame as #identity")
	}
	if got.Did != testDID {
		t.Errorf("#identity did = %q, want %q", got.Did, testDID)
	}
	if got.Handle == nil || *got.Handle != "renamed.example.com" {
		t.Errorf("#identity handle = %v, want the renamed handle", got.Handle)
	}
	if got.Seq != 2 {
		t.Errorf("#identity seq = %d, want 2", got.Seq)
	}

	// A commit after the rename continues the same seq line.
	if s := h.post(t, "after the rename"); s != 3 {
		t.Errorf("post-rename commit seq = %d, want 3", s)
	}
}

// TestIdentityFrameIsCursorReplayable: an #identity frame is outbox history
// like any other, so a consumer reconnecting with an older cursor gets it in
// seq order rather than silently missing the rename.
func TestIdentityFrameIsCursorReplayable(t *testing.T) {
	h := newHarness(t)
	_ = h.post(t, "one")
	if _, err := h.funnel.EmitIdentity(context.Background(), testDID, "renamed.example.com"); err != nil {
		t.Fatalf("EmitIdentity: %v", err)
	}
	ctx := h.run(t)
	h.waitCursor(t, 2)

	conn := h.dial(t, ctx, "0")
	if seq := seqOf(t, readFrame(t, ctx, conn)); seq != 1 {
		t.Fatalf("replayed commit seq = %d, want 1", seq)
	}
	var got *comatproto.SyncSubscribeRepos_Identity
	done := make(chan struct{})
	replayFrameThroughIndigo(t, ctx, readFrame(t, ctx, conn), &events.RepoStreamCallbacks{
		RepoIdentity: func(evt *comatproto.SyncSubscribeRepos_Identity) error {
			got = evt
			close(done)
			return nil
		},
	}, done)
	if got == nil || got.Seq != 2 {
		t.Fatalf("replayed #identity = %v, want seq 2", got)
	}
}

// TestAccountFrameReachesSubscribersAndParsesInIndigo is the layer-2 disable
// conformance proof (S4-D): the frame Funnel.EmitAccount produces is fanned by
// the broadcaster exactly like a #commit — frame-type agnostic — and INDIGO's
// own stream consumer, the parser a relay runs, decodes it as an #account with
// the right active flag and status. A step-down carries active=false + status
// "deactivated"; re-entry carries active=true + NO status (a bare reactivation
// needs no reason). Both share the one PDS-global seq line with the commit.
func TestAccountFrameReachesSubscribersAndParsesInIndigo(t *testing.T) {
	h := newHarness(t)
	ctx := h.run(t)
	conn := h.dial(t, ctx, "")
	h.waitSubscribers(t, 1)

	_ = h.post(t, "before the step-down")
	if seq := seqOf(t, readFrame(t, ctx, conn)); seq != 1 {
		t.Fatalf("first commit seq = %d, want 1", seq)
	}

	// Deactivation: active=false + status "deactivated".
	seq, err := h.funnel.EmitAccount(ctx, testDID, false, atprotorepo.AccountStatusDeactivated)
	if err != nil {
		t.Fatalf("EmitAccount(false): %v", err)
	}
	if seq != 2 {
		t.Fatalf("#account seq = %d, want 2 (one seq line shared with #commit)", seq)
	}
	down := readAccountFrame(t, ctx, conn)
	if down.Did != testDID {
		t.Errorf("#account did = %q, want %q", down.Did, testDID)
	}
	if down.Active {
		t.Error("#account active = true, want false on a step-down")
	}
	if down.Status == nil || *down.Status != atprotorepo.AccountStatusDeactivated {
		t.Errorf("#account status = %v, want %q", down.Status, atprotorepo.AccountStatusDeactivated)
	}
	if down.Seq != 2 {
		t.Errorf("#account seq = %d, want 2", down.Seq)
	}

	// Re-entry: active=true + no status.
	seq, err = h.funnel.EmitAccount(ctx, testDID, true, "")
	if err != nil {
		t.Fatalf("EmitAccount(true): %v", err)
	}
	if seq != 3 {
		t.Fatalf("re-entry #account seq = %d, want 3", seq)
	}
	up := readAccountFrame(t, ctx, conn)
	if !up.Active {
		t.Error("#account active = false, want true on re-entry")
	}
	if up.Status != nil {
		t.Errorf("#account status = %v, want nil on re-entry (active=true needs no reason)", up.Status)
	}
	if up.Seq != 3 {
		t.Errorf("re-entry #account seq = %d, want 3", up.Seq)
	}
}

// readAccountFrame reads one frame off conn and asserts INDIGO's consumer parses
// it as an #account, returning the parsed event.
func readAccountFrame(t *testing.T, ctx context.Context, conn *websocket.Conn) *comatproto.SyncSubscribeRepos_Account {
	t.Helper()
	var got *comatproto.SyncSubscribeRepos_Account
	done := make(chan struct{})
	replayFrameThroughIndigo(t, ctx, readFrame(t, ctx, conn), &events.RepoStreamCallbacks{
		RepoAccount: func(evt *comatproto.SyncSubscribeRepos_Account) error {
			got = evt
			close(done)
			return nil
		},
	}, done)
	if got == nil {
		t.Fatal("indigo's consumer did not parse the frame as #account")
	}
	return got
}

// TestSyncFrameFromTheCollapseProducerParsesInIndigo covers S5 slice 4's
// producer half: after a huge downtime gap is applied with its #commit frames
// deferred, ONE #sync must reach live subscribers announcing the authoritative
// head, and it must be the same frame shape a relay already understands — so it
// is parsed here by indigo's own stream consumer, not by an assertion about our
// own bytes (atproto-pds-bridge.md § Projection & backfill, watermark row 3:
// "collapse huge gaps to one #sync + relay re-getRepo").
func TestSyncFrameFromTheCollapseProducerParsesInIndigo(t *testing.T) {
	h := newHarness(t)
	ctx := h.run(t)
	conn := h.dial(t, ctx, "")
	h.waitSubscribers(t, 1)

	// A collapsed catch-up: two commits land in the repo, neither is announced.
	h.deferredPost(t, "gap one")
	h.deferredPost(t, "gap two")

	seq, err := h.funnel.EmitSync(ctx, testDID)
	if err != nil {
		t.Fatalf("EmitSync: %v", err)
	}
	if seq != 1 {
		t.Fatalf("#sync seq = %d, want 1 — the deferred commits must burn no seq", seq)
	}

	var got *comatproto.SyncSubscribeRepos_Sync
	done := make(chan struct{})
	replayFrameThroughIndigo(t, ctx, readFrame(t, ctx, conn), &events.RepoStreamCallbacks{
		RepoSync: func(evt *comatproto.SyncSubscribeRepos_Sync) error {
			got = evt
			close(done)
			return nil
		},
	}, done)
	if got == nil {
		t.Fatal("indigo's consumer did not parse the collapse frame as #sync")
	}
	if got.Did != testDID {
		t.Errorf("#sync did = %q, want %q", got.Did, testDID)
	}
	if len(got.Blocks) == 0 {
		t.Error("#sync carries no commit block — a relay cannot learn the head from it")
	}
	// The announced rev must be the head AFTER both deferred commits, not the
	// one the network last saw: that is the whole point of the collapse.
	_, headRev, ok, err := h.store.CommitCAR(ctx, testDID)
	if err != nil || !ok {
		t.Fatalf("read head: ok=%v err=%v", ok, err)
	}
	if got.Rev != headRev {
		t.Errorf("#sync rev = %q, want the current head rev %q", got.Rev, headRev)
	}

	// The seq line continues normally afterwards.
	if s := h.post(t, "after the collapse"); s != 2 {
		t.Errorf("post-collapse commit seq = %d, want 2", s)
	}
}

// ── Connection-exhaustion hardening (task 12) ────────────────────────────────

// TestSubscriberCapRefusesBeyondThePerIPLimit: the public, unauthenticated WS
// is connection-exhaustible without a CONCURRENCY cap — the route's rate
// limiter bounds establishment RATE, and a firehose connection is held open by
// design, so one IP otherwise ramps without bound. The N+1th subscribe from the
// same IP must be refused, and refused BEFORE the upgrade (a cheap 429, not a
// held connection). Freeing one slot must let the next in.
func TestSubscriberCapRefusesBeyondThePerIPLimit(t *testing.T) {
	h := newHarness(t)
	ctx := h.run(t)

	conns := make([]*websocket.Conn, 0, maxSubscribersPerIP)
	for i := 0; i < maxSubscribersPerIP; i++ {
		conns = append(conns, h.dial(t, ctx, ""))
	}
	h.waitSubscribers(t, maxSubscribersPerIP)

	// The upgrade itself must fail — websocket.Dial surfaces the non-101 status
	// on the response it returns.
	_, resp, err := websocket.Dial(ctx, h.url, nil)
	if err == nil {
		t.Fatal("the subscriber past the per-IP cap was admitted")
	}
	if resp == nil {
		t.Fatalf("expected an HTTP refusal past the cap, got a transport error: %v", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusTooManyRequests {
		t.Errorf("refusal status = %d, want 429 (the same shape the route's rate limiter uses)", resp.StatusCode)
	}
	if n := h.bc.SubscriberCount(); n != maxSubscribersPerIP {
		t.Errorf("a REFUSED subscribe still took a slot: count = %d, want %d", n, maxSubscribersPerIP)
	}

	// Releasing a slot readmits: the cap is a live count, not a high-water mark.
	conns[0].CloseNow()
	h.waitSubscribers(t, maxSubscribersPerIP-1)
	readmitted := h.dial(t, ctx, "")
	h.waitSubscribers(t, maxSubscribersPerIP)
	readmitted.CloseNow()
}

// TestSubscriberSlotIsReleasedOnEveryExitPath: a slot leak is as fatal as no cap
// at all — it just takes longer. Connect/disconnect past the cap's worth of
// connections and assert the count returns to zero every time, including the
// too-slow drop path, whose removal and the handler's deferred unsubscribe both
// touch the same subscriber (a double per-IP decrement would leak capacity the
// other way, letting an IP exceed the cap forever).
func TestSubscriberSlotIsReleasedOnEveryExitPath(t *testing.T) {
	h := newHarness(t)
	ctx := h.run(t)

	for i := 0; i < maxSubscribersPerIP+4; i++ {
		c := h.dial(t, ctx, "")
		h.waitSubscribers(t, 1)
		c.CloseNow()
		h.waitSubscribers(t, 0)
	}

	// The too-slow drop, attached directly: a real socket cannot reach it (WS/OS
	// buffers absorb hundreds of small frames before a write blocks — the lesson
	// TestSlowConsumerIsDropped already encodes), so the subscriber is attached
	// and never drained, which IS the stalled-consumer state.
	sub, _, err := h.bc.subscribe("198.51.100.7")
	if err != nil {
		t.Fatalf("subscribe: %v", err)
	}
	for i := 0; i < subscriberBuffer+16; i++ {
		h.post(t, fmt.Sprintf("flood %d", i))
	}
	select {
	case <-sub.dropped:
	case <-time.After(10 * time.Second):
		t.Fatal("stalled subscriber was never dropped")
	}

	// Both removal paths now touch the SAME subscriber: fanout dropped it, and
	// the handler's deferred unsubscribe follows. A second decrement would leak
	// capacity the other way — that IP could then hold more than the cap allows,
	// forever — so releasing twice must be a no-op, not a double release.
	h.bc.unsubscribe(sub)
	h.bc.unsubscribe(sub)

	h.bc.mu.Lock()
	perIP := len(h.bc.perIP)
	h.bc.mu.Unlock()
	if perIP != 0 {
		t.Errorf("per-IP tally leaked %d entries after every consumer left: %d", perIP, perIP)
	}

	// Capacity really is back: the cap's worth of fresh connections all land.
	for i := 0; i < maxSubscribersPerIP; i++ {
		h.dial(t, ctx, "")
	}
	h.waitSubscribers(t, maxSubscribersPerIP)
}

// TestGlobalSubscriberCap covers the whole-PDS ceiling directly (the per-IP cap
// hides it in a loopback test, where every consumer shares an IP): admission is
// refused once the global set is full even for an IP with slots to spare.
func TestGlobalSubscriberCap(t *testing.T) {
	h := newHarness(t)
	for i := 0; i < maxSubscribers; i++ {
		ip := fmt.Sprintf("10.0.%d.%d", i/256, i%256)
		if _, _, err := h.bc.subscribe(ip); err != nil {
			t.Fatalf("subscribe %d from a fresh IP was refused below the global cap: %v", i, err)
		}
	}
	if _, _, err := h.bc.subscribe("10.9.9.9"); !errors.Is(err, ErrTooManySubscribers) {
		t.Fatalf("subscribe past the global cap returned %v, want ErrTooManySubscribers", err)
	}
}

// TestIdleSubscriberIsClosedWhenItStopsAnsweringPings: the eviction the "no idle
// timeout" finding is really about. CloseRead's context only cancels on a
// GRACEFUL peer close, and on a quiet PDS no frame is ever written — so a
// half-open socket held a slot forever. The keepalive ping demands proof of
// life; a peer that never pongs is closed.
//
// A raw idle-READ deadline would be wrong here and this test would pass under
// it for the wrong reason, so the healthy half matters as much: a consumer that
// sends nothing at all but whose WS stack answers pongs must SURVIVE.
func TestIdleSubscriberIsClosedWhenItStopsAnsweringPings(t *testing.T) {
	h := newHarnessKeepalive(t, 50*time.Millisecond, 200*time.Millisecond)
	ctx := h.run(t)

	// A silent-but-healthy consumer: it sends no application message ever, and
	// sits in its read loop exactly as a real firehose consumer does (indigo's
	// HandleRepoStream is one). That read loop is what lets its WS stack answer
	// pings — which is the contract this keepalive assumes and every real
	// subscriber satisfies by construction.
	healthy := h.dial(t, ctx, "")
	frames := make(chan []byte, 4)
	readErr := make(chan error, 1)
	go func() {
		for {
			_, msg, err := healthy.Read(ctx)
			if err != nil {
				readErr <- err
				return
			}
			frames <- msg
		}
	}()
	h.waitSubscribers(t, 1)
	time.Sleep(500 * time.Millisecond) // several ping cycles
	select {
	case err := <-readErr:
		t.Fatalf("a healthy but SILENT consumer was evicted (%v) — an idle-READ "+
			"deadline would do exactly this, and it is the availability bug inverted", err)
	default:
	}
	if n := h.bc.SubscriberCount(); n != 1 {
		t.Fatalf("healthy consumer count = %d, want 1", n)
	}
	h.post(t, "still connected")
	select {
	case msg := <-frames:
		if seq := seqOf(t, msg); seq != 1 {
			t.Fatalf("healthy consumer got seq=%d, want 1", seq)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("healthy consumer never received the live frame")
	}

	// A wedged peer: a raw TCP socket that completes the handshake and then
	// never reads or writes another byte, so it never answers a ping.
	wedged := dialRawWedged(t, h.url)
	defer wedged.Close()
	h.waitSubscribers(t, 2)

	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		if h.bc.SubscriberCount() == 1 {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatal("a peer that never answers a ping held its subscriber slot indefinitely")
}

// dialRawWedged completes a WebSocket handshake over a raw TCP socket and then
// goes silent forever — the shape a slowloris/half-open peer has, which no
// cooperating WS client library can produce (they all answer pings).
func dialRawWedged(t *testing.T, wsURL string) net.Conn {
	t.Helper()
	addr := strings.TrimPrefix(wsURL, "ws://")
	if i := strings.Index(addr, "/"); i >= 0 {
		addr = addr[:i]
	}
	path := wsURL[strings.Index(wsURL, "/xrpc/"):]

	c, err := net.Dial("tcp", addr)
	if err != nil {
		t.Fatalf("raw dial: %v", err)
	}
	req := "GET " + path + " HTTP/1.1\r\n" +
		"Host: " + addr + "\r\n" +
		"Upgrade: websocket\r\n" +
		"Connection: Upgrade\r\n" +
		"Sec-WebSocket-Key: AAAAAAAAAAAAAAAAAAAAAA==\r\n" +
		"Sec-WebSocket-Version: 13\r\n\r\n"
	if _, err := c.Write([]byte(req)); err != nil {
		t.Fatalf("raw handshake write: %v", err)
	}
	// Read just the handshake response, then never touch the socket again.
	br := bufio.NewReader(c)
	for {
		line, err := br.ReadString('\n')
		if err != nil {
			t.Fatalf("raw handshake read: %v", err)
		}
		if line == "\r\n" {
			break
		}
		if strings.HasPrefix(line, "HTTP/1.1") && !strings.Contains(line, "101") {
			t.Fatalf("raw handshake was not upgraded: %s", strings.TrimSpace(line))
		}
	}
	return c
}
