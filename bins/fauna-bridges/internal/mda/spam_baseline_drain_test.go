package mda

import (
	"bytes"
	"context"
	"errors"
	"sync"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// spamBaselineFixture wires a spamBaselineDrain against pure seams and records
// what each leg saw, so a test asserts the run_id threads end-to-end and the
// aggregate's counts reach the submit.
type spamBaselineFixture struct {
	drain *spamBaselineDrain

	mu             sync.Mutex
	worklistRunIDs [][]byte                            // run_id per worklist call
	worklistReply  []wsrpc.SpamBaselineCopy            // what worklistFn returns
	worklistErr    error                               // if set, worklistFn returns it
	aggInputs      [][]mailfauna.SpamBaselineCopyInput // inputs per aggregate call
	aggReply       mailfauna.SpamBaselineAggregate     // what aggregateFn returns
	aggErr         error                               // if set, aggregateFn returns it
	submits        []submitCall                        // one per submit call
	submitOK       bool                                // what submitFn returns for ok
	submitErr      error                               // if set, submitFn returns it
}

type submitCall struct {
	runID              []byte
	merged             []byte
	contributors       uint32
	unreadable         uint32
	mergedContributors [][]byte
}

func newSpamBaselineFixture(t *testing.T) *spamBaselineFixture {
	t.Helper()
	f := &spamBaselineFixture{submitOK: true}
	f.drain = newSpamBaselineDrain(spamBaselineDrainDeps{
		worklistFn: func(_ context.Context, runID []byte) ([]wsrpc.SpamBaselineCopy, error) {
			f.mu.Lock()
			defer f.mu.Unlock()
			f.worklistRunIDs = append(f.worklistRunIDs, append([]byte(nil), runID...))
			if f.worklistErr != nil {
				return nil, f.worklistErr
			}
			return f.worklistReply, nil
		},
		aggregateFn: func(copies []mailfauna.SpamBaselineCopyInput) (mailfauna.SpamBaselineAggregate, error) {
			f.mu.Lock()
			defer f.mu.Unlock()
			f.aggInputs = append(f.aggInputs, copies)
			if f.aggErr != nil {
				return mailfauna.SpamBaselineAggregate{}, f.aggErr
			}
			return f.aggReply, nil
		},
		submitFn: func(_ context.Context, runID, merged []byte, contributors, unreadable uint32, mergedContributors [][]byte) (bool, error) {
			f.mu.Lock()
			defer f.mu.Unlock()
			f.submits = append(f.submits, submitCall{
				runID:              append([]byte(nil), runID...),
				merged:             append([]byte(nil), merged...),
				contributors:       contributors,
				unreadable:         unreadable,
				mergedContributors: mergedContributors,
			})
			if f.submitErr != nil {
				return false, f.submitErr
			}
			return f.submitOK, nil
		},
	})
	return f
}

// TestSpamBaselineDrain_happyPathThreadsRunIDAndCounts — the run_id from the
// poke reaches the worklist pull, the pulled copies reach the off-box merge, and
// the merge's counts + merged bytes reach the submit against the same run_id.
func TestSpamBaselineDrain_happyPathThreadsRunIDAndCounts(t *testing.T) {
	f := newSpamBaselineFixture(t)
	runID := []byte{1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16}
	f.worklistReply = []wsrpc.SpamBaselineCopy{
		{OwnerActorID: []byte{0xAA}, SealedCopy: []byte("copy-a")},
		{OwnerActorID: []byte{0xBB}, SealedCopy: []byte("copy-b")},
	}
	f.aggReply = mailfauna.SpamBaselineAggregate{
		MergedModel:        []byte("merged"),
		Contributors:       2,
		Unreadable:         0,
		MergedContributors: [][]byte{{0xAA}, {0xBB}},
	}

	f.drain.runOnce(context.Background(), runID)

	if len(f.worklistRunIDs) != 1 || !bytes.Equal(f.worklistRunIDs[0], runID) {
		t.Fatalf("worklist run_ids = %x, want one call with %x", f.worklistRunIDs, runID)
	}
	if len(f.aggInputs) != 1 || len(f.aggInputs[0]) != 2 {
		t.Fatalf("aggregate got %d calls / inputs, want 1 call of 2 copies", len(f.aggInputs))
	}
	if !bytes.Equal(f.aggInputs[0][0].SealedCopy, []byte("copy-a")) ||
		!bytes.Equal(f.aggInputs[0][1].OwnerActorId, []byte{0xBB}) {
		t.Fatalf("aggregate inputs not threaded from worklist: %+v", f.aggInputs[0])
	}
	if len(f.submits) != 1 {
		t.Fatalf("submit called %d times, want 1", len(f.submits))
	}
	s := f.submits[0]
	if !bytes.Equal(s.runID, runID) || !bytes.Equal(s.merged, []byte("merged")) || s.contributors != 2 || s.unreadable != 0 {
		t.Fatalf("submit = %+v, want run_id %x merged 'merged' contributors 2 unreadable 0", s, runID)
	}
	// The names the merge produced reach the submit untouched — the nest
	// records exactly these as summed (mail-spam.md § Cold start Path 2).
	if len(s.mergedContributors) != 2 ||
		!bytes.Equal(s.mergedContributors[0], []byte{0xAA}) ||
		!bytes.Equal(s.mergedContributors[1], []byte{0xBB}) {
		t.Fatalf("submit merged_contributors = %x, want [AA BB]", s.mergedContributors)
	}
}

// TestSpamBaselineDrain_emptyWorklistStillSubmits — a holder with no reaching
// copies still submits an empty half (zero counts) so the nest's bounded await
// resolves promptly instead of timing out.
func TestSpamBaselineDrain_emptyWorklistStillSubmits(t *testing.T) {
	f := newSpamBaselineFixture(t)
	f.worklistReply = nil // empty
	f.aggReply = mailfauna.SpamBaselineAggregate{MergedModel: nil, Contributors: 0, Unreadable: 0}

	f.drain.runOnce(context.Background(), []byte{0x01})

	if len(f.submits) != 1 {
		t.Fatalf("submit called %d times, want 1 (empty half)", len(f.submits))
	}
	if f.submits[0].contributors != 0 || len(f.submits[0].merged) != 0 {
		t.Fatalf("empty submit = %+v, want zero contributors + empty merged", f.submits[0])
	}
}

// TestSpamBaselineDrain_worklistErrorSkipsSubmit — an expired/failed worklist
// pull services nothing; the nest proceeds from its plaintext half.
func TestSpamBaselineDrain_worklistErrorSkipsSubmit(t *testing.T) {
	f := newSpamBaselineFixture(t)
	f.worklistErr = errors.New("no pending spam-baseline publish run")

	f.drain.runOnce(context.Background(), []byte{0x01})

	if len(f.aggInputs) != 0 {
		t.Fatalf("aggregate called %d times after worklist error, want 0", len(f.aggInputs))
	}
	if len(f.submits) != 0 {
		t.Fatalf("submit called %d times after worklist error, want 0", len(f.submits))
	}
}

// TestSpamBaselineDrain_aggregateErrorSkipsSubmit — malformed holder key
// material errors the merge; without a merge there is nothing honest to submit.
func TestSpamBaselineDrain_aggregateErrorSkipsSubmit(t *testing.T) {
	f := newSpamBaselineFixture(t)
	f.worklistReply = []wsrpc.SpamBaselineCopy{{OwnerActorID: []byte{0xAA}, SealedCopy: []byte("c")}}
	f.aggErr = errors.New("holder key material bad")

	f.drain.runOnce(context.Background(), []byte{0x01})

	if len(f.submits) != 0 {
		t.Fatalf("submit called %d times after aggregate error, want 0", len(f.submits))
	}
}

// TestSpamBaselineDrain_submitOKFalseIsGraceful — a run that already closed
// (submit ok=false) is not fatal: the run just isn't serviced (no panic, no
// retry). The holder-too-slow outcome the nest's bounded await priced in.
func TestSpamBaselineDrain_submitOKFalseIsGraceful(t *testing.T) {
	f := newSpamBaselineFixture(t)
	f.worklistReply = []wsrpc.SpamBaselineCopy{{OwnerActorID: []byte{0xAA}, SealedCopy: []byte("c")}}
	f.aggReply = mailfauna.SpamBaselineAggregate{MergedModel: []byte("m"), Contributors: 1}
	f.submitOK = false

	// Must not panic or block.
	f.drain.runOnce(context.Background(), []byte{0x01})

	if len(f.submits) != 1 {
		t.Fatalf("submit called %d times, want 1", len(f.submits))
	}
}

// TestSpamBaselineDrain_startPokeServiceStop — start spawns the loop, a poke
// with a run_id services exactly that run, and ctx cancel exits the loop.
func TestSpamBaselineDrain_startPokeServiceStop(t *testing.T) {
	f := newSpamBaselineFixture(t)
	ran := make(chan []byte, 8)
	inner := f.drain.deps.worklistFn
	f.drain.deps.worklistFn = func(ctx context.Context, runID []byte) ([]wsrpc.SpamBaselineCopy, error) {
		ran <- append([]byte(nil), runID...)
		return inner(ctx, runID)
	}

	ctx, cancel := context.WithCancel(context.Background())
	f.drain.start(ctx)

	runID := []byte{0xDE, 0xAD, 0xBE, 0xEF}
	f.drain.poke(runID)
	select {
	case got := <-ran:
		if !bytes.Equal(got, runID) {
			t.Fatalf("serviced run_id %x, want %x", got, runID)
		}
	case <-time.After(3 * time.Second):
		t.Fatal("poke did not trigger a drain run")
	}

	cancel()
	select {
	case <-f.drain.done:
	case <-time.After(3 * time.Second):
		t.Fatal("drain loop did not exit on ctx cancel")
	}
}

// TestSpamBaselineDrain_pokeIsNonBlockingWhenFull — poke never blocks even when
// the run queue is saturated (the reader-goroutine safety contract): a burst
// past the buffer drops the overflow rather than stalling.
func TestSpamBaselineDrain_pokeIsNonBlockingWhenFull(t *testing.T) {
	f := newSpamBaselineFixture(t)
	// Never start()ed, so nothing drains runCh — every poke past the buffer
	// must drop, not block.
	done := make(chan struct{})
	go func() {
		for i := 0; i < spamBaselineRunQueue+16; i++ {
			f.drain.poke([]byte{byte(i)})
		}
		close(done)
	}()
	select {
	case <-done:
	case <-time.After(3 * time.Second):
		t.Fatal("poke blocked on a full queue")
	}
}
