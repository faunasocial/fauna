package logplane

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"testing"
	"time"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// The queue is process-global (mirroring fauna_sidecar_client::log_plane's
// statics), so the tests that drive it must not interleave.
var testMu sync.Mutex

func setup(t *testing.T) {
	t.Helper()
	testMu.Lock()
	t.Cleanup(func() {
		Reset()
		testMu.Unlock()
	})
	Reset()
}

// fakeCaller records the batches a flush ships and can be told to fail.
type fakeCaller struct {
	mu      sync.Mutex
	batches [][]wsrpc.LogEvent
	dropped []uint64
	err     error
	calls   int
}

// wireBatch is the shape the fake expects to find on the wire. Declaring it
// here rather than reusing wsrpc's unexported request type is deliberate: a
// round-trip through real CBOR proves the wrapper's struct tags produce the
// keys nest's serde decoder reads, which is the half a Go-only test would
// otherwise miss.
type wireBatch struct {
	Events  []wsrpc.LogEvent `cbor:"events"`
	Dropped uint64           `cbor:"dropped"`
}

func (f *fakeCaller) Call(_ context.Context, method string, body any, _ any) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls++
	if method != wsrpc.MethodReportLogEvents {
		return fmt.Errorf("unexpected method %q", method)
	}
	if f.err != nil {
		return f.err
	}
	raw, err := cbor.Marshal(body)
	if err != nil {
		return fmt.Errorf("marshal: %w", err)
	}
	var got wireBatch
	if err := cbor.Unmarshal(raw, &got); err != nil {
		return fmt.Errorf("unmarshal: %w", err)
	}
	f.batches = append(f.batches, got.Events)
	f.dropped = append(f.dropped, got.Dropped)
	return nil
}

func (f *fakeCaller) lastBatch() ([]wsrpc.LogEvent, uint64) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if len(f.batches) == 0 {
		return nil, 0
	}
	return f.batches[len(f.batches)-1], f.dropped[len(f.dropped)-1]
}

func (f *fakeCaller) count() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.calls
}

func TestEmitQueuesWithWireLevelAndTimestamp(t *testing.T) {
	setup(t)
	Emit(LevelWarn, "cert_fetch_failed", "code 42")
	batch, dropped, ok := takeBatch()
	if !ok {
		t.Fatal("expected a batch")
	}
	if len(batch) != 1 {
		t.Fatalf("len = %d, want 1", len(batch))
	}
	if batch[0].Level != "warn" {
		t.Errorf("level = %q, want warn", batch[0].Level)
	}
	if batch[0].Event != "cert_fetch_failed" {
		t.Errorf("event = %q", batch[0].Event)
	}
	if batch[0].Message != "code 42" {
		t.Errorf("message = %q", batch[0].Message)
	}
	if batch[0].TimestampMs == 0 {
		t.Error("timestamp not stamped")
	}
	if dropped != 0 {
		t.Errorf("dropped = %d, want 0", dropped)
	}
}

func TestQueueIsBoundedAndDropsOldestFirstCountingThem(t *testing.T) {
	setup(t)
	for i := 0; i < QueueCapacity+10; i++ {
		Emit(LevelInfo, "tick", fmt.Sprintf("%d", i))
	}
	if got := QueuedLen(); got != QueueCapacity {
		t.Fatalf("queued = %d, want %d (bounded)", got, QueueCapacity)
	}
	batch, dropped, ok := takeBatch()
	if !ok {
		t.Fatal("expected a batch")
	}
	if dropped != 10 {
		t.Errorf("dropped = %d, want 10 — the evicted must be counted for nest", dropped)
	}
	if batch[0].Message != "10" {
		t.Errorf("oldest survivor = %q, want \"10\" — a recent failure outlives a stale one", batch[0].Message)
	}
}

func TestBatchNeverExceedsTheWireCap(t *testing.T) {
	setup(t)
	for i := 0; i < QueueCapacity; i++ {
		Emit(LevelInfo, "tick", fmt.Sprintf("%d", i))
	}
	batch, _, ok := takeBatch()
	if !ok {
		t.Fatal("expected a batch")
	}
	if len(batch) != MaxEventsPerBatch {
		t.Errorf("batch = %d, want %d (the wire cap)", len(batch), MaxEventsPerBatch)
	}
	if got, want := QueuedLen(), QueueCapacity-MaxEventsPerBatch; got != want {
		t.Errorf("remainder = %d, want %d — it stays queued for the next flush", got, want)
	}
}

func TestEmptyQueueYieldsNoBatch(t *testing.T) {
	setup(t)
	if _, _, ok := takeBatch(); ok {
		t.Error("nothing to say must produce no wire traffic")
	}
}

func TestDropCountAloneIsStillWorthABatch(t *testing.T) {
	setup(t)
	for i := 0; i < QueueCapacity+3; i++ {
		Emit(LevelInfo, "tick", fmt.Sprintf("%d", i))
	}
	// Drain the events, keeping the accumulated drop count for the next take.
	discardEventsKeepingDrops()
	batch, dropped, ok := takeBatch()
	if !ok {
		t.Fatal("a drop-only batch must still report")
	}
	if len(batch) != 0 {
		t.Errorf("events = %d, want 0", len(batch))
	}
	if dropped != 3 {
		t.Errorf("dropped = %d, want 3", dropped)
	}
}

func TestEmitIsANoOpOnceDisabled(t *testing.T) {
	setup(t)
	setDisabled(true)
	Emit(LevelError, "boom", "x")
	if QueuedLen() != 0 {
		t.Error("a refusing nest must cost the bridge nothing")
	}
}

// An error *reply* is a permanent server refusal (a same-image retry would fail
// identically), so the source disables rather than error-loops.
func TestAnErrorReplyDisablesReportingForTheSession(t *testing.T) {
	setup(t)
	caller := &fakeCaller{err: errors.New("unknown kind")}
	Emit(LevelInfo, "ready", "up")
	Flush(context.Background(), caller)
	if !IsDisabled() {
		t.Fatal("an error reply must disable the plane for this process")
	}
	Emit(LevelInfo, "ready", "up again")
	if QueuedLen() != 0 {
		t.Error("emit after disable must not queue")
	}
	Flush(context.Background(), caller)
	if caller.count() != 1 {
		t.Errorf("calls = %d, want 1 — a disabled plane must not keep dialling", caller.count())
	}
}

func TestFlushOnAnEmptyQueueMakesNoCall(t *testing.T) {
	setup(t)
	caller := &fakeCaller{}
	Flush(context.Background(), caller)
	if caller.count() != 0 {
		t.Errorf("calls = %d, want 0 — an idle bridge must be silent on the wire", caller.count())
	}
}

// A transport blip (nest restarting, reconnect gap) drops the batch but must
// leave the plane enabled — otherwise one blip silences the bridge forever.
func TestATransportErrorDropsTheBatchButStaysEnabled(t *testing.T) {
	setup(t)
	caller := &fakeCaller{err: wsrpc.ErrReconnecting}
	Emit(LevelInfo, "ready", "up")
	Flush(context.Background(), caller)
	if IsDisabled() {
		t.Fatal("a reconnect gap must not disable the plane permanently")
	}
	if QueuedLen() != 0 {
		t.Error("the batch is dropped, not requeued — a flapping channel must not grow the queue")
	}
}

// The events a failed flush threw away must still be COUNTED, or nest's ring
// shows an unbroken story with a silent hole in it. The events themselves stay
// dropped (re-queueing is what would grow unboundedly); only the integer
// survives, which is why this is safe.
func TestAFailedFlushFoldsItsLossBackIntoTheDropCount(t *testing.T) {
	setup(t)
	failing := &fakeCaller{err: wsrpc.ErrReconnecting}
	Emit(LevelInfo, "ready", "up")
	Emit(LevelWarn, "nest_reconnected", "again")
	Flush(context.Background(), failing)

	// Nothing queued, nothing sent — but the loss is remembered.
	if QueuedLen() != 0 {
		t.Fatalf("QueuedLen = %d, want 0", QueuedLen())
	}
	ok := &fakeCaller{}
	Flush(context.Background(), ok)
	batch, drops := ok.lastBatch()
	if len(batch) != 0 {
		t.Errorf("the failed batch's events must NOT be resent, got %d", len(batch))
	}
	if drops != 2 {
		t.Errorf("dropped = %d, want 2 (the two events the failed flush lost)", drops)
	}
}

// A drop count already in flight when a flush fails must not be lost either —
// it is folded back ALONGSIDE the batch's events, not replaced by them.
func TestAFailedFlushPreservesAnAlreadyReportedDropCount(t *testing.T) {
	setup(t)
	// Overflow the queue so evictions accrue a pending count, then fail the
	// flush that would have carried both the survivors and that count.
	for i := 0; i < QueueCapacity+3; i++ {
		Emit(LevelInfo, "tick", fmt.Sprintf("%d", i))
	}
	failing := &fakeCaller{err: wsrpc.ErrReconnecting}
	Flush(context.Background(), failing)

	ok := &fakeCaller{}
	// Drain whatever survived the first (capped) batch, accumulating the count.
	var total uint64
	for i := 0; i < 10; i++ {
		before := ok.count()
		Flush(context.Background(), ok)
		if ok.count() == before {
			break
		}
		_, d := ok.lastBatch()
		total += d
	}
	// 3 evicted by the overflow + MaxEventsPerBatch lost to the failed flush.
	want := uint64(3 + MaxEventsPerBatch)
	if total != want {
		t.Errorf("total reported drops = %d, want %d (3 evicted + %d lost to the failed flush)",
			total, want, MaxEventsPerBatch)
	}
}

// Run is the long-lived flush loop; a size trigger must flush well before the
// timer would.
func TestRunFlushesOnTheSizeTrigger(t *testing.T) {
	setup(t)
	caller := &fakeCaller{}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan struct{})
	go func() { Run(ctx, caller); close(done) }()

	for i := 0; i < FlushThreshold; i++ {
		Emit(LevelInfo, "tick", fmt.Sprintf("%d", i))
	}
	deadline := time.After(2 * time.Second)
	for caller.count() == 0 {
		select {
		case <-deadline:
			t.Fatal("size trigger did not flush within 2s (the timer interval is longer)")
		case <-time.After(5 * time.Millisecond):
		}
	}
	cancel()
	<-done
}

// Shutdown is when the most valuable events happen (revoked, forced close), so
// the loop owes a final flush after its context is cancelled.
func TestRunFlushesOnceMoreAfterCancel(t *testing.T) {
	setup(t)
	caller := &fakeCaller{}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() { Run(ctx, caller); close(done) }()
	Emit(LevelError, "enrollment_revoked", "revoked")
	cancel()
	<-done
	if caller.count() != 1 {
		t.Errorf("calls = %d, want 1 — the shutdown flush must ship the last events", caller.count())
	}
	events, dropped := caller.lastBatch()
	if len(events) != 1 || events[0].Event != "enrollment_revoked" || events[0].Level != "error" {
		t.Errorf("wire batch = %+v, want one error/enrollment_revoked event", events)
	}
	if dropped != 0 {
		t.Errorf("dropped = %d, want 0", dropped)
	}
}
