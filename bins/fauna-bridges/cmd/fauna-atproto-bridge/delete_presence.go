package main

// The "Delete my Bluesky presence" sweep — S5 slice 5 of
// `docs/goal/behavior/atproto-pds-bridge.md` § Disable & revocation, layer 2:
// the "separate, stronger action" beside the reversible step-down that S4-D
// built. A user who confirms it has their projected records swept with real
// deleteRecord commits, the network told once, and the repo erased from this
// bridge — while the DID and its sealed keys are RETAINED nest-side, which is
// what makes the section's "still reversible in identity terms" literally true.
//
// The sweep is a CONVERGENT RECONCILE, not an event: nest writes the identity's
// status as `deleted` when the user confirms, and this pass treats "the roster
// says deleted AND a repo still exists" as the work to do. That shape — the
// rename hook's, ratified in S3 — is why there is no `deleting` intermediate
// status, no bridge→nest completion kind, and no crash window that needs one:
// every step below is idempotent, so a pass interrupted anywhere is finished by
// the next one.

import (
	"context"
	"fmt"
	"log/slog"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotorepo"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// sweepChunk bounds how many records one delete commit carries. A hard-coded
// constant, never configuration (§ Product invariants — nobody would ever want
// to choose this), sized to the projection page ceiling so a sweep and a
// projection move the same amount of repo per commit.
//
// Batching deletes is safe even though slice 4's residual says batching CREATES
// is not: the objection there was in-batch reply resolution, and a delete op
// resolves nothing.
const sweepChunk = 200

// sweepDeletedIdentity erases did's projected presence, in the one order that
// keeps every intermediate state coherent:
//
//  1. Delete every record through the funnel, in chunked commits, while the repo
//     is STILL SERVED. Each commit is an ordinary signed #commit carrying delete
//     ops, so what the network receives is the tombstones — which is the entire
//     point of a delete, the only part that propagates removal to relays and
//     AppViews, and the thing that unserving first would suppress.
//  2. Announce #account(active=false, status="deleted") — durable in the outbox
//     before anything stops us re-announcing it.
//  3. Purge the repo from this store.
//
// The tempting inverse — stop serving first, "so a crash never serves a
// half-destroyed repo" — is wrong on its own terms: a sweep is a SEQUENCE OF
// VALID COMMITS, so a half-swept repo is a perfectly coherent repo that happens
// to have fewer records, indistinguishable from a user deleting records one at a
// time. Deactivating first is what would create incoherence (announcing commits
// on a repo we just told the network is unserved) and would leave a crash with
// records present-but-unreachable and the network never told they were deleted.
//
// Retrying after a crash at any point converges: step 1 finds fewer or no
// records, step 2 re-announces (a duplicate #account is harmless to a relay —
// the same argument S4-D made for its own frame-first ordering), step 3 is a
// no-op on an already-purged DID.
func sweepDeletedIdentity(ctx context.Context, c wsrpc.Caller, deps *projDeps, id wsrpc.AtprotoIdentityView, logger *slog.Logger) error {
	did := *id.DID

	_, exists, err := deps.store.RepoActive(ctx, did)
	if err != nil {
		return fmt.Errorf("read repo active flag: %w", err)
	}
	if !exists {
		// Already swept (or never projected). The nest row remains the durable
		// tombstone; this store keeps none, because its content is re-derivable
		// and a tombstone here would be the one row that is not.
		return nil
	}

	// The signer is needed only because a delete is a real commit that must be
	// signed like any other. Unsealing it here, rather than reusing the
	// projection path's, keeps the sweep independent of whether this identity is
	// still projectable at all.
	signer, err := unsealRepoSigner(ctx, c, deps, id)
	if err != nil {
		return fmt.Errorf("unseal repo signer: %w", err)
	}

	deleted, err := sweepRecords(ctx, deps, did, signer, logger)
	if err != nil {
		return err
	}

	seq, err := deps.funnel.EmitAccount(ctx, did, false, atprotorepo.AccountStatusDeleted)
	if err != nil {
		return fmt.Errorf("emit #account(deleted): %w", err)
	}

	if err := deps.store.PurgeRepo(ctx, did); err != nil {
		return fmt.Errorf("purge repo: %w", err)
	}

	logger.Info("atproto presence deleted: records swept, #account(deleted) announced, repo purged",
		"handle", id.Handle, "did", did, "records_deleted", deleted, "account_seq", seq)
	return nil
}

// sweepRecords deletes every record in did's repo through the funnel, chunked,
// and returns how many were deleted. It re-reads the first page each round
// rather than walking a cursor forward: the previous round has just removed the
// records it read, so "the first sweepChunk records that are still there" is
// always the right next batch, and a keyset cursor would instead walk PAST the
// records a failed chunk left behind.
func sweepRecords(ctx context.Context, deps *projDeps, did string, signer atprotorepo.Signer, logger *slog.Logger) (int, error) {
	total := 0
	for {
		paths, _, err := deps.store.RecordPathsPage(ctx, did, "", sweepChunk)
		if err != nil {
			return total, fmt.Errorf("enumerate records to sweep: %w", err)
		}
		if len(paths) == 0 {
			return total, nil
		}
		ops := make([]atprotorepo.RepoOp, 0, len(paths))
		for _, p := range paths {
			ops = append(ops, atprotorepo.RepoOp{
				Action:     atprotorepo.ActionDelete,
				Collection: p.Collection,
				Rkey:       p.Rkey,
			})
		}
		res, err := deps.funnel.ApplyBatch(ctx, did, signer, ops)
		if err != nil {
			return total, fmt.Errorf("commit delete sweep chunk: %w", err)
		}
		if res.NoChange || res.Ops == 0 {
			// The batch reduced to nothing while records are still listed —
			// impossible through the funnel, and looping would spin forever.
			return total, fmt.Errorf("delete sweep made no progress on %d listed records in repo %s", len(paths), did)
		}
		total += res.Ops
		logger.Info("atproto delete sweep chunk committed",
			"did", did, "ops", res.Ops, "seq", res.Seq, "rev", res.Rev)
	}
}
