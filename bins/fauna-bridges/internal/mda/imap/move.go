package imap

import (
	"context"
	"errors"
	"sort"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/imapserver"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

const moveRPCTimeout = 30 * time.Second

// moveWriter is the per-MOVE-command seam Session.move writes through.
// Production wraps emersion's *imapserver.MoveWriter; tests substitute
// a fake. The contract (per emersion's MoveWriter doc): WriteCopyData
// once, then WriteExpunge any number of times.
//
// emersion's MoveWriter has no VANISHED counterpart for QRESYNC — same
// upstream-blocked story as Expunge. See `imap-server.md`
// § Upstream-blocked gaps.
type moveWriter interface {
	WriteCopyData(data *imap.CopyData) error
	WriteExpunge(seqNum uint32) error
}

// Move implements imapserver.SessionIMAP4rev2.Move (RFC 6851 / 9051
// §6.6 — atomic copy-then-expunge).
//
// Wire flow:
//
//  1. Resolve numSet → ordered (seqNum, UID) pairs for the pre-move
//     source mailbox.
//  2. Call `fauna.bridges.move` — nest performs copy + expunge in
//     a single transaction (atomicity per `imap-server.md`
//     § Architectural rules).
//  3. Write `imap.CopyData` via `MoveWriter.WriteCopyData` so emersion
//     emits the untagged `OK [COPYUID …]` response (RFC 4315 UIDPLUS).
//  4. For each moved source UID, emit `* <seq> EXPUNGE` via
//     `MoveWriter.WriteExpunge` in descending pre-move-seqNum order
//     (same arithmetic as Session.expunge: pre_seq(r) = |survivors≤r|
//     + |removed≤r|).
//  5. \Junk SPECIAL-USE training-signal hook (same shape as COPY).
//
// QRESYNC `VANISHED` for the expunge half is upstream-blocked
// (see `imap-server.md` § Upstream-blocked gaps) — until upstream
// lands ENABLE-QRESYNC + a VANISHED writer, MOVE always emits the
// per-source `* <seq> EXPUNGE` fallback.
func (s *Session) Move(w *imapserver.MoveWriter, numSet imap.NumSet, dest string) error {
	s.mu.Lock()
	var opener mailfauna.RecordOpener
	if s.recordOpener != nil {
		opener = s.recordOpener
	}
	s.mu.Unlock()
	return s.move(context.Background(), &moveWriterAdapter{w: w}, numSet, dest, opener)
}

// move is the seam-friendly inner method tested in move_test.go.
func (s *Session) move(
	ctx context.Context,
	w moveWriter,
	numSet imap.NumSet,
	dest string,
	opener mailfauna.RecordOpener,
) error {
	s.mu.Lock()
	actorID := s.actorID
	source := s.selectedMailbox
	client := s.client
	s.mu.Unlock()
	if actorID == nil {
		return errors.New("imap: MOVE requires authenticated state")
	}
	if source == "" {
		return errors.New("imap: MOVE requires a SELECTed source mailbox")
	}
	if dest == "" {
		return errors.New("imap: MOVE requires a destination mailbox")
	}

	rpcCtx, cancel := context.WithTimeout(ctx, moveRPCTimeout)
	defer cancel()

	pairs, err := resolveStoreNumSet(rpcCtx, client, actorID, source, numSet)
	if err != nil {
		return err
	}
	if len(pairs) == 0 {
		return nil
	}
	uids := make([]uint32, len(pairs))
	for i, p := range pairs {
		uids[i] = p.uid
	}

	reply, err := wsrpc.Move(rpcCtx, client, wsrpc.MoveParams{
		ActorID:       actorID,
		SourceMailbox: source,
		UIDs:          uids,
		DestMailbox:   dest,
	})
	if err != nil {
		// RFC 9208: over-quota destination → NO [OVERQUOTA] (the nest
		// pre-check rolls back, source stays). imap-server.md § Quota
		// enforcement points. A live guardian hold → tagged NO (the
		// nest refuses to relocate a held message out of the guardian
		// held mailbox; family-safety.md § The mail gate).
		return mapHeldForReview(mapOverQuota(err))
	}
	if len(reply.Moved) == 0 {
		return nil
	}

	srcUIDs := make(imap.UIDSet, 0, len(reply.Moved))
	destUIDs := make(imap.UIDSet, 0, len(reply.Moved))
	movedSourceList := make([]uint32, 0, len(reply.Moved))
	destUIDList := make([]uint32, 0, len(reply.Moved))
	for _, pair := range reply.Moved {
		srcUIDs.AddNum(imap.UID(pair.SourceUID))
		destUIDs.AddNum(imap.UID(pair.DestUID))
		movedSourceList = append(movedSourceList, pair.SourceUID)
		destUIDList = append(destUIDList, pair.DestUID)
	}

	if err := w.WriteCopyData(&imap.CopyData{
		UIDValidity: reply.DestUIDValidity,
		SourceUIDs:  srcUIDs,
		DestUIDs:    destUIDs,
	}); err != nil {
		return err
	}

	// Per-source EXPUNGE. The source rows are gone; fetch the post-
	// move survivor list to derive each removed UID's pre-move seqNum
	// via the same arithmetic Session.expunge uses.
	survivors, err := wsrpc.FetchMessageMetadata(rpcCtx, client, actorID, source, nil)
	if err != nil {
		return err
	}
	survivingUIDs := make([]uint32, len(survivors))
	for i, r := range survivors {
		survivingUIDs[i] = r.UID
	}
	sort.Slice(survivingUIDs, func(i, j int) bool { return survivingUIDs[i] < survivingUIDs[j] })

	removed := make([]uint32, len(movedSourceList))
	copy(removed, movedSourceList)
	sort.Slice(removed, func(i, j int) bool { return removed[i] < removed[j] })

	seqs := make([]uint32, len(removed))
	for i, r := range removed {
		sIdx := sort.Search(len(survivingUIDs), func(j int) bool { return survivingUIDs[j] > r })
		rIdx := sort.Search(len(removed), func(j int) bool { return removed[j] > r })
		seqs[i] = uint32(sIdx + rIdx)
	}
	sort.Slice(seqs, func(i, j int) bool { return seqs[i] > seqs[j] })
	for _, seq := range seqs {
		if err := w.WriteExpunge(seq); err != nil {
			return err
		}
	}

	// \Junk training-signal hook — same shape as COPY. Dest-side
	// metadata fetch (the row is in dest after the move).
	if label, ok := junkMoveLabel(source, dest); ok {
		s.fireJunkTrainSignals(rpcCtx, client, opener, actorID, dest, destUIDList,
			label, wsrpc.TrainingSourceImapJunkMove)
	}

	return nil
}

// mapHeldForReview translates a nest `fauna.bridges.held_for_review`
// RpcError into a tagged `NO`: the message sits in the guardian held
// mailbox with a live hold, and only the guardian's approve/deny moves it
// (family-safety.md § The mail gate). No RFC 5530 response code fits, so
// the text carries the explanation the MUA shows the user. Any other error
// passes through unchanged.
func mapHeldForReview(err error) error {
	if code, ok := wsrpc.RpcErrorCode(err); ok && code == wsrpc.CodeHeldForReview {
		return &imap.Error{
			Type: imap.StatusResponseTypeNo,
			Text: "Message is held for guardian review",
		}
	}
	return err
}

// moveWriterAdapter wraps emersion's *imapserver.MoveWriter to satisfy
// the moveWriter seam.
type moveWriterAdapter struct {
	w *imapserver.MoveWriter
}

func (a *moveWriterAdapter) WriteCopyData(data *imap.CopyData) error {
	return a.w.WriteCopyData(data)
}

func (a *moveWriterAdapter) WriteExpunge(seqNum uint32) error {
	return a.w.WriteExpunge(seqNum)
}
