package atprotorepo

import (
	"context"
	"fmt"
	"time"

	blocks "github.com/ipfs/go-block-format"
	"github.com/ipfs/go-cid"
)

// The firehose outbox read side: everything the subscribeRepos broadcaster
// (internal/atprotofirehose) needs to replay a cursor and follow the live tail.
// The write side is the funnel's commit txn (funnel.go step 6/7) — one row per
// frame, committed with the head so a crash never leaves one without the other.

// EventRetention is how long an emitted frame stays replayable. A cursor older
// than the oldest retained seq gets the sanctioned degraded path — one #sync per
// repo, then the live tail, and the relay re-fetches history with getRepo
// (atproto-pds-bridge.md § Projection & backfill, "collapse huge gaps to one
// #sync + relay re-getRepo").
//
// Hard-coded, not configurable: nobody chooses this (§ Product invariants — the
// only configuration surface is the apps). 72h comfortably covers a relay
// restart or a network partition while keeping the outbox bounded; projected
// volume is one frame per public post, so a nest's outbox stays tiny.
const EventRetention = 72 * time.Hour

// FirehoseEvent is one persisted outbox frame, ready to write to a subscriber
// as a binary WS message — Payload is the already-serialized indigo frame
// (header + body), so the broadcaster never re-encodes.
type FirehoseEvent struct {
	Seq       int64
	DID       string
	FrameType string
	Payload   []byte
}

// EventsSince returns up to limit frames with seq > afterSeq, in seq order.
// Pass afterSeq=0 to start from the oldest retained frame.
func (s *Store) EventsSince(ctx context.Context, afterSeq int64, limit int) ([]FirehoseEvent, error) {
	if limit <= 0 {
		limit = 512
	}
	rows, err := s.db.QueryContext(ctx,
		`SELECT seq, did, frame_type, payload FROM firehose_events
		 WHERE seq > ? ORDER BY seq LIMIT ?`, afterSeq, limit)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []FirehoseEvent
	for rows.Next() {
		var e FirehoseEvent
		if err := rows.Scan(&e.Seq, &e.DID, &e.FrameType, &e.Payload); err != nil {
			return nil, err
		}
		out = append(out, e)
	}
	return out, rows.Err()
}

// SeqRange reports the lowest and highest seq still retained. ok is false when
// the outbox is empty (a PDS that has never committed, or one whose whole window
// aged out). A requested cursor below minSeq cannot be replayed.
func (s *Store) SeqRange(ctx context.Context) (minSeq, maxSeq int64, ok bool, err error) {
	var lo, hi *int64
	if err := s.db.QueryRowContext(ctx,
		`SELECT MIN(seq), MAX(seq) FROM firehose_events`).Scan(&lo, &hi); err != nil {
		return 0, 0, false, err
	}
	if lo == nil || hi == nil {
		return 0, 0, false, nil
	}
	return *lo, *hi, true, nil
}

// PruneEvents drops frames older than EventRetention, returning how many it
// removed. The most recent frame is always kept whatever its age, so SeqRange
// stays meaningful (and a cursor at the head still validates) on a quiet PDS
// whose last commit was days ago.
func (s *Store) PruneEvents(ctx context.Context, now time.Time) (int64, error) {
	cutoff := now.Add(-EventRetention).UnixMicro()
	res, err := s.db.ExecContext(ctx,
		`DELETE FROM firehose_events
		 WHERE created_at < ? AND seq < (SELECT MAX(seq) FROM firehose_events)`, cutoff)
	if err != nil {
		return 0, err
	}
	n, err := res.RowsAffected()
	if err != nil {
		return 0, err
	}
	return n, nil
}

// CommitCAR returns a CARv1 carrying only did's head commit block, plus the
// head rev — the #sync frame's authoritative-head payload (Sync v1.1: #sync
// announces the head; the consumer fetches the rest with getRepo).
func (s *Store) CommitCAR(ctx context.Context, did string) (car []byte, rev string, ok bool, err error) {
	h, found, err := loadHead(ctx, s.db, did)
	if err != nil || !found {
		return nil, "", false, err
	}
	commitCID, err := cid.Decode(h.commitCID)
	if err != nil {
		return nil, "", false, fmt.Errorf("decode head commit cid: %w", err)
	}
	raw, found, err := s.GetBlock(ctx, did, []byte(commitCID.KeyString()))
	if err != nil {
		return nil, "", false, err
	}
	if !found {
		return nil, "", false, fmt.Errorf("head commit block missing for did %s", did)
	}
	blk, err := blocks.NewBlockWithCid(raw, commitCID)
	if err != nil {
		return nil, "", false, err
	}
	bs := newMemBlockstore()
	if err := bs.Put(ctx, blk); err != nil {
		return nil, "", false, err
	}
	car, err = writeCAR(ctx, commitCID, bs, []cid.Cid{commitCID})
	if err != nil {
		return nil, "", false, err
	}
	return car, h.rev, true, nil
}
