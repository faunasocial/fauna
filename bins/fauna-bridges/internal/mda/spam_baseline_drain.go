package mda

import (
	"context"
	"log/slog"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// The spam-baseline publish drain WORKER (mail-spam.md § Encrypted-mode
// interaction, ratified 2026-07-13) — the third holder-pull drain instance
// beside the re-score plane (rescore_drain.go). When an admin runs
// `publish_spam_baseline`, the nest registers a pending run, pokes the connected
// aggregation holder with the run_id (PushKindSpamBaselinePublish), and awaits
// the holder's submit for a bounded window (SPAM_BASELINE_HOLDER_WAIT = 10s).
// This worker services one such run per poke: it pulls the run's grant-gated
// sealed-copy worklist, unseal-merges it OFF-BOX in shared Rust with the
// holder's OWN service-user key halves (never any key of a user's — the nest
// core holds no in-process decryption authority, encryption-at-rest.md § Don't
// do these), and submits the merged plaintext half + its contributor count back
// against the run, naming the contributors it merged so the nest records exactly
// those as summed (mail-spam.md § Cold start Path 2 → A contributor's departure
// withdraws the baseline).
//
// Simpler than the re-score drain: no loop-until-empty and no scheduled
// backstop. A publish run exists only inside its ~10s window nest-side, so there
// is no standing obligation to periodically sweep — the worker is purely
// push-driven and does exactly one worklist pull per run.

const (
	// spamBaselineRPCTimeout bounds each nest RPC leg of a run. Both legs
	// (worklist pull + submit) plus the CPU-fast off-box merge between them must
	// finish inside the nest's SPAM_BASELINE_HOLDER_WAIT (10s) or the run's
	// bounded await closes and the submit reaches no pending run (ok=false — a
	// graceful, honest-skip outcome, not an error). 4s per leg leaves ample
	// margin on the sub-second loopback/LAN reality.
	spamBaselineRPCTimeout = 4 * time.Second

	// spamBaselineRunQueue bounds pending publish runs the poke queue holds. A
	// publish is a rare, synchronous admin action bounded to ~10s nest-side, so
	// one in flight is the norm; a small buffer absorbs a burst. A full queue
	// drops the run (it times out nest-side → the publish proceeds from its
	// readable half with an honest skipped count), never blocks the wsrpc reader
	// goroutine the poke runs on.
	spamBaselineRunQueue = 8
)

// spamBaselineDrainDeps carries what the worker needs. caller + the holder key
// halves + logger are the production inputs; the *Fn fields are test seams
// (production leaves them nil → the real wsrpc / FFI implementations), mirroring
// rescoreDrainDeps.
type spamBaselineDrainDeps struct {
	caller wsrpc.Caller
	// holderX25519Secret / holderMlkemDk are the enrolled service-user key
	// halves the off-box merge opens contributor copies with — the same halves
	// the re-score drain feeds unseal_capability_grant. holderMlkemDk is empty
	// for a classical-only holder.
	holderX25519Secret []byte
	holderMlkemDk      []byte
	logger             *slog.Logger

	// Test seams — production leaves these nil.
	worklistFn  func(ctx context.Context, runID []byte) ([]wsrpc.SpamBaselineCopy, error)
	aggregateFn func(copies []mailfauna.SpamBaselineCopyInput) (mailfauna.SpamBaselineAggregate, error)
	submitFn    func(ctx context.Context, runID, merged []byte, contributors, unreadable uint32, mergedContributors [][]byte) (bool, error)
}

// spamBaselineDrain is the running worker: a poke-able background loop whose
// every wake services exactly one publish run (identified by the poked run_id).
type spamBaselineDrain struct {
	deps  spamBaselineDrainDeps
	runCh chan []byte
	done  chan struct{}
}

// newSpamBaselineDrain fills the production defaults for nil seams.
func newSpamBaselineDrain(deps spamBaselineDrainDeps) *spamBaselineDrain {
	if deps.logger == nil {
		deps.logger = slog.Default()
	}
	if deps.worklistFn == nil {
		deps.worklistFn = func(ctx context.Context, runID []byte) ([]wsrpc.SpamBaselineCopy, error) {
			return wsrpc.SpamBaselineWorklist(ctx, deps.caller, runID)
		}
	}
	if deps.aggregateFn == nil {
		deps.aggregateFn = func(copies []mailfauna.SpamBaselineCopyInput) (mailfauna.SpamBaselineAggregate, error) {
			return mailfauna.AggregateSpamModelCopies(copies, deps.holderX25519Secret, deps.holderMlkemDk)
		}
	}
	if deps.submitFn == nil {
		deps.submitFn = func(ctx context.Context, runID, merged []byte, contributors, unreadable uint32, mergedContributors [][]byte) (bool, error) {
			return wsrpc.SubmitSpamBaseline(ctx, deps.caller, runID, merged, contributors, unreadable, mergedContributors)
		}
	}
	return &spamBaselineDrain{
		deps:  deps,
		runCh: make(chan []byte, spamBaselineRunQueue),
		done:  make(chan struct{}),
	}
}

// start spawns the drain loop: one runOnce per poked run_id, until ctx is
// cancelled. Closes done on exit — mda.Run's teardown waits on it BEFORE
// Close()ing the holder, so no drain merge is in flight when the registry
// zeroizes.
func (d *spamBaselineDrain) start(ctx context.Context) {
	go func() {
		defer close(d.done)
		for {
			select {
			case <-ctx.Done():
				return
			case runID := <-d.runCh:
				d.runOnce(ctx, runID)
			}
		}
	}()
}

// poke enqueues one publish run for the worker to service. Non-blocking: a full
// queue drops the run (it times out nest-side → honest skip), so this is safe to
// call on the wsrpc reader goroutine (the PushHandler "fast and non-blocking"
// contract). The run_id is copied because the push payload buffer may be reused.
func (d *spamBaselineDrain) poke(runID []byte) {
	r := make([]byte, len(runID))
	copy(r, runID)
	select {
	case d.runCh <- r:
	default:
		d.deps.logger.Warn("spam-baseline drain queue full; dropping publish run (will time out nest-side)")
	}
}

// runOnce services one publish run: pull the run's sealed-copy worklist →
// off-box unseal-merge with the holder's own key halves → submit the merged half
// back. Any leg failure logs and returns (the nest's bounded await proceeds from
// its plaintext half with an honest skipped count — no obligation is lost).
func (d *spamBaselineDrain) runOnce(ctx context.Context, runID []byte) {
	log := d.deps.logger

	// 1. Pull the run's grant-gated sealed-copy worklist. An expired run (the
	//    admin's bounded await already closed) is a typed error here — nothing
	//    to service; the nest proceeded from its plaintext half.
	wlCtx, cancel := context.WithTimeout(ctx, spamBaselineRPCTimeout)
	copies, err := d.deps.worklistFn(wlCtx, runID)
	cancel()
	if err != nil {
		log.Warn("spam-baseline drain: worklist fetch failed; run not serviced", "err", err)
		return
	}

	// 2. Merge OFF-BOX with the holder's own key halves. Per-copy failures are
	//    counted `unreadable` inside the shared merge, never fatal — only
	//    malformed holder key material errors, and then there is nothing honest
	//    to submit. An empty worklist still submits (empty half + zero counts)
	//    so the nest's await resolves promptly rather than timing out.
	inputs := make([]mailfauna.SpamBaselineCopyInput, len(copies))
	for i := range copies {
		inputs[i] = mailfauna.SpamBaselineCopyInput{
			OwnerActorId: copies[i].OwnerActorID,
			SealedCopy:   copies[i].SealedCopy,
		}
	}
	agg, err := d.deps.aggregateFn(inputs)
	if err != nil {
		log.Error("spam-baseline drain: aggregate failed (holder key material?); run not serviced", "err", err)
		return
	}

	// 3. Submit the merged half back against the pending run, naming the
	//    contributors it holds so the nest's inclusion record is exact.
	subCtx, cancel := context.WithTimeout(ctx, spamBaselineRPCTimeout)
	ok, err := d.deps.submitFn(subCtx, runID, agg.MergedModel, agg.Contributors, agg.Unreadable, agg.MergedContributors)
	cancel()
	if err != nil {
		log.Warn("spam-baseline drain: submit failed", "err", err)
		return
	}
	if !ok {
		// The publish's bounded await already closed the run (holder too slow);
		// the nest proceeded from its readable half. Not an error.
		log.Info("spam-baseline drain: submit reached no pending run (publish already resolved)",
			"contributors", agg.Contributors, "unreadable", agg.Unreadable)
		return
	}
	log.Info("spam-baseline drain: submitted merged half",
		"contributors", agg.Contributors, "unreadable", agg.Unreadable, "merged_bytes", len(agg.MergedModel))
}
