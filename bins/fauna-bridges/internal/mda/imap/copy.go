package imap

import (
	"context"
	"errors"
	"time"

	"github.com/emersion/go-imap/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

const copyRPCTimeout = 30 * time.Second

// Copy implements imapserver.Session.Copy (RFC 9051 §6.4.7 — also
// covers UID COPY via emersion's numKind dispatch).
//
// Wire flow:
//
//  1. Resolve the numSet → ordered UID list (same shape as
//     resolveStoreNumSet — UIDSet without dynamic ranges taken at
//     face value, otherwise a fetch_message_metadata round-trip).
//  2. Call `fauna.bridges.copy` with the source/dest mailbox pair +
//     UIDs.
//  3. Build the `*imap.CopyData` from the reply's `Copied` pairs.
//     emersion writes the tagged-OK `[COPYUID <uid_validity>
//     <source-set> <dest-set>]` response from this payload (RFC 4315).
//  4. If the source or dest mailbox is the canonical `\Junk`
//     SPECIAL-USE mailbox, fire the per-UID agent-side training
//     signal (fireJunkTrainSignals) with source=imap_junk_move and
//     the label inferred from whether the move is *into* or *out of*
//     `\Junk` (per `mail-spam.md` § Training signal sources).
func (s *Session) Copy(numSet imap.NumSet, dest string) (*imap.CopyData, error) {
	s.mu.Lock()
	var opener mailfauna.RecordOpener
	if s.recordOpener != nil {
		opener = s.recordOpener
	}
	s.mu.Unlock()
	return s.copy(context.Background(), numSet, dest, opener)
}

// copy is the seam-friendly inner method tested in copy_test.go.
func (s *Session) copy(
	ctx context.Context,
	numSet imap.NumSet,
	dest string,
	opener mailfauna.RecordOpener,
) (*imap.CopyData, error) {
	s.mu.Lock()
	actorID := s.actorID
	source := s.selectedMailbox
	client := s.client
	s.mu.Unlock()
	if actorID == nil {
		return nil, errors.New("imap: COPY requires authenticated state")
	}
	if source == "" {
		return nil, errors.New("imap: COPY requires a SELECTed source mailbox")
	}
	if dest == "" {
		return nil, errors.New("imap: COPY requires a destination mailbox")
	}

	rpcCtx, cancel := context.WithTimeout(ctx, copyRPCTimeout)
	defer cancel()

	pairs, err := resolveStoreNumSet(rpcCtx, client, actorID, source, numSet)
	if err != nil {
		return nil, err
	}
	if len(pairs) == 0 {
		return nil, nil
	}
	uids := make([]uint32, len(pairs))
	for i, p := range pairs {
		uids[i] = p.uid
	}

	reply, err := wsrpc.Copy(rpcCtx, client, wsrpc.CopyParams{
		ActorID:       actorID,
		SourceMailbox: source,
		UIDs:          uids,
		DestMailbox:   dest,
	})
	if err != nil {
		// RFC 9208: over-quota destination → NO [OVERQUOTA] (the nest
		// pre-check rolls back, source untouched). imap-server.md
		// § Quota enforcement points.
		return nil, mapOverQuota(err)
	}
	if len(reply.Copied) == 0 {
		return nil, nil
	}

	srcUIDs := make(imap.UIDSet, 0, len(reply.Copied))
	destUIDs := make(imap.UIDSet, 0, len(reply.Copied))
	destUIDList := make([]uint32, 0, len(reply.Copied))
	for _, pair := range reply.Copied {
		srcUIDs.AddNum(imap.UID(pair.SourceUID))
		destUIDs.AddNum(imap.UID(pair.DestUID))
		destUIDList = append(destUIDList, pair.DestUID)
	}

	data := &imap.CopyData{
		UIDValidity: reply.DestUIDValidity,
		SourceUIDs:  srcUIDs,
		DestUIDs:    destUIDs,
	}

	// \Junk SPECIAL-USE check: fire the training signal when either
	// the source or the destination is the canonical "Junk" mailbox.
	// Label is "spam" when moving *into* Junk, "ham" when moving
	// *out of* Junk; the train metadata lookup happens against the
	// dest mailbox (that's where the row exists post-COPY — the
	// source row stays too, but dest UIDs are easier to address).
	if label, ok := junkMoveLabel(source, dest); ok {
		s.fireJunkTrainSignals(rpcCtx, client, opener, actorID, dest, destUIDList,
			label, wsrpc.TrainingSourceImapJunkMove)
	}

	return data, nil
}

// junkMoveLabel detects whether a COPY/MOVE source/dest pair triggers
// a `\Junk` training signal, and which label fires.
//
// Per `mail-spam.md` § Training signal sources: relocating *into*
// the canonical `\Junk` mailbox implies "user thinks this is spam"
// (label=spam); relocating *out of* `\Junk` implies "user thinks this
// is legitimate" (label=ham). The detection currently keys on the
// canonical mailbox name ("Junk") via `canonicalMailboxAttr`; once
// Phase D.6 lands CREATE-with-SPECIAL-USE, this helper extends to
// also honor user-marked \Junk mailboxes via a nest-side metadata
// lookup.
func junkMoveLabel(source, dest string) (wsrpc.SpamLabel, bool) {
	_, srcIsJunk := canonicalMailboxAttr(source)
	srcIsJunk = srcIsJunk && source == "Junk"
	_, dstIsJunk := canonicalMailboxAttr(dest)
	dstIsJunk = dstIsJunk && dest == "Junk"
	switch {
	case dstIsJunk && !srcIsJunk:
		return wsrpc.SpamLabelSpam, true
	case srcIsJunk && !dstIsJunk:
		return wsrpc.SpamLabelHam, true
	}
	return "", false
}
