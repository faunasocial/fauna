package imap

import (
	"context"
	"errors"
	"sort"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/imapserver"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

const expungeRPCTimeout = 30 * time.Second

// expungeWriter is the per-EXPUNGE-command seam Session.expunge writes
// through. Production wraps emersion's *imapserver.ExpungeWriter; tests
// substitute a fake that records every WriteExpunge call.
//
// emersion v2.0.0-beta.8 exposed only `WriteExpunge(seqNum uint32)` —
// the per-UID `* <seq> EXPUNGE` wire shape. The vendored go-imap fork
// (T3.2-b, FORK.md) adds `WriteVanished(uid_set)` for the QRESYNC
// `* VANISHED <uid_set>` shape (RFC 7162 §3.2.10). Once the client has
// ENABLE QRESYNC'd, expunge notifications use VANISHED; otherwise the
// per-UID EXPUNGE fallback (RFC 7162-compliant, pre-QRESYNC wire shape).
type expungeWriter interface {
	WriteExpunge(seqNum uint32) error
	WriteVanished(uids imap.UIDSet) error
}

// Expunge implements imapserver.SessionIMAP4rev2.Expunge.
//
// Wire flow:
//
//  1. Call `fauna.bridges.expunge(actor_id, mailbox, uids)` with the
//     requested UID set. nest performs the \Deleted intersection
//     server-side (RFC 4315 §2.1 for UID EXPUNGE; full-mailbox \Deleted
//     scan for plain EXPUNGE). The reply carries the UIDs actually
//     removed.
//  2. If the reply is empty, return nil — no metadata round-trip, no
//     responses to emit.
//  3. Otherwise fetch the *post-expunge* per-UID metadata (the
//     surviving rows) and derive each removed UID's *pre-expunge*
//     sequence number arithmetically. Emit one `* <seq> EXPUNGE` per
//     removed UID, in descending sequence-number order so each
//     response is stable against the pre-expunge mailbox state (RFC
//     9051 §6.4.3 — the client's local renumbering is rolling-from-
//     the-top, so descending lets each report stand alone without the
//     "next response references the new sequence" bookkeeping).
//
// `uids == nil` corresponds to the plain `EXPUNGE` command. A non-nil
// pointer holds the UID set from a `UID EXPUNGE <uidset>` invocation.
//
// Per `imap-server.md` § QRESYNC: when the client has ENABLE QRESYNC'd
// (qresyncActive), the expunged set is reported as a single
// `* VANISHED <uid_set>` (RFC 7162 §3.2.10) — see the `expunge` inner
// method. Otherwise the per-UID `* <seq> EXPUNGE` fallback applies.
func (s *Session) Expunge(w *imapserver.ExpungeWriter, uids *imap.UIDSet) error {
	return s.expunge(context.Background(), &expungeWriterAdapter{w: w}, uidSetToList(uids))
}

// expunge is the seam-friendly inner method tested in expunge_test.go.
// `uids == nil` → plain EXPUNGE; non-nil → UID EXPUNGE bounded to the
// given list.
func (s *Session) expunge(
	ctx context.Context,
	w expungeWriter,
	uids *[]uint32,
) error {
	s.mu.Lock()
	actorID := s.actorID
	mailbox := s.selectedMailbox
	client := s.client
	s.mu.Unlock()
	if actorID == nil {
		return errors.New("imap: EXPUNGE requires authenticated state")
	}
	if mailbox == "" {
		return errors.New("imap: EXPUNGE requires a SELECTed mailbox")
	}

	rpcCtx, cancel := context.WithTimeout(ctx, expungeRPCTimeout)
	defer cancel()

	params := wsrpc.ExpungeParams{
		ActorID: actorID,
		Mailbox: mailbox,
	}
	if uids != nil {
		params.UIDs = *uids
	}
	reply, err := wsrpc.Expunge(rpcCtx, client, params)
	if err != nil {
		return err
	}
	if len(reply.ExpungedUIDs) == 0 {
		return nil
	}

	// QRESYNC fast path (RFC 7162 §3.2.10): once the client has ENABLE
	// QRESYNC'd, a single `* VANISHED <uid_set>` reports every expunged
	// UID. No per-UID sequence-number derivation and no post-expunge
	// survivor round-trip are needed — VANISHED carries UIDs directly.
	if s.qresyncActive() {
		return w.WriteVanished(uidSetFromUint32s(reply.ExpungedUIDs))
	}

	// Post-expunge survivor list (the metadata RPC sees the current
	// mailbox state — the removed rows are already gone). The pre-
	// expunge seqNum of a removed UID r is computed from the post-
	// expunge snapshot + the reply's removed-UID list:
	//
	//   pre_seq(r) = |{s ∈ surviving : s ≤ r}| + |{x ∈ removed : x ≤ r}|
	//
	// (every UID that was ≤ r in the pre-expunge mailbox, regardless
	// of whether it survived this EXPUNGE.)
	rows, err := wsrpc.FetchMessageMetadata(rpcCtx, client, actorID, mailbox, nil)
	if err != nil {
		return err
	}
	surviving := make([]uint32, len(rows))
	for i, r := range rows {
		surviving[i] = r.UID
	}
	sort.Slice(surviving, func(i, j int) bool { return surviving[i] < surviving[j] })

	removed := make([]uint32, len(reply.ExpungedUIDs))
	copy(removed, reply.ExpungedUIDs)
	sort.Slice(removed, func(i, j int) bool { return removed[i] < removed[j] })

	// For each removed UID, compute its pre-expunge seqNum.
	// Both lists are ascending, so the cumulative-count queries are
	// O(|surviving| + |removed|) via sort.Search (binary search).
	seqs := make([]uint32, len(removed))
	for i, r := range removed {
		sIdx := sort.Search(len(surviving), func(j int) bool { return surviving[j] > r })
		rIdx := sort.Search(len(removed), func(j int) bool { return removed[j] > r })
		seqs[i] = uint32(sIdx + rIdx)
	}

	// Emit in descending seqNum order so each response references the
	// pre-expunge sequence directly and the client's rolling-decrement
	// applies to higher-numbered messages that have already been
	// reported.
	sort.Slice(seqs, func(i, j int) bool { return seqs[i] > seqs[j] })
	for _, seq := range seqs {
		if err := w.WriteExpunge(seq); err != nil {
			return err
		}
	}
	return nil
}

// uidSetToList materializes an emersion-parsed UID set into a flat
// `[]uint32` for the wsrpc shape. Returns nil when `set == nil`
// (plain EXPUNGE — no UID filter). The dynamic-form `*` is rejected
// upstream by emersion before reaching the session method (UID EXPUNGE
// takes a finite set per RFC 4315 §2.1).
func uidSetToList(set *imap.UIDSet) *[]uint32 {
	if set == nil {
		return nil
	}
	nums, ok := set.Nums()
	if !ok {
		// Dynamic-range UID sets aren't legal in UID EXPUNGE; defensive
		// fallback to "expunge nothing" rather than full-mailbox.
		empty := []uint32{}
		return &empty
	}
	out := make([]uint32, len(nums))
	for i, n := range nums {
		out[i] = uint32(n)
	}
	return &out
}

// expungeWriterAdapter wraps emersion's *imapserver.ExpungeWriter to
// satisfy the expungeWriter seam.
type expungeWriterAdapter struct {
	w *imapserver.ExpungeWriter
}

func (a *expungeWriterAdapter) WriteExpunge(seqNum uint32) error {
	return a.w.WriteExpunge(seqNum)
}

func (a *expungeWriterAdapter) WriteVanished(uids imap.UIDSet) error {
	return a.w.WriteVanished(uids)
}

// uidSetFromUint32s builds an imap.UIDSet from a flat UID list,
// coalescing consecutive UIDs into ranges (emersion's AddNum delegates
// to imapnum.Set). Used to render `* VANISHED <uid_set>` responses.
func uidSetFromUint32s(uids []uint32) imap.UIDSet {
	var set imap.UIDSet
	for _, u := range uids {
		set.AddNum(imap.UID(u))
	}
	return set
}
