package imap

import (
	"context"
	"errors"
	"fmt"
	"sort"
	"strings"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/imapserver"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// idleSubscribeRPCTimeout caps the synchronous nest round-trip that
// registers the IDLE push subscription. Separate from the IDLE loop
// timeout itself — the registration happens before the MDA returns
// the `+ idling` continuation to the MUA, so latency here is bounded.
const idleSubscribeRPCTimeout = 10 * time.Second

// idleSeqLookupRPCTimeout caps each per-event `fetch_message_metadata`
// round-trip used to translate a UID into a sequence number for
// `WriteExpunge` / `WriteMessageFlags` / `WriteNumMessages`. Bounded
// short — an event-translation timeout drops the event (we log and
// continue) rather than wedging the IDLE loop.
const idleSeqLookupRPCTimeout = 15 * time.Second

// defaultIdleTimeout is the fallback IDLE timeout when neither the
// Backend nor a per-session override supplies one. Per RFC 2177 §3
// the recommendation is < 30 min; nest's `imap.idle_timeout_seconds`
// policy default is 1740s (29 min) per `mail-policy-config.md`. The
// fallback here is reached only when the Backend was constructed
// without plumbing the config snapshot — production always sets it
// explicitly.
const defaultIdleTimeout = 29 * time.Minute

// updateWriter is the seam Session.idle dispatches through.  Production
// wraps emersion's *imapserver.UpdateWriter; tests substitute a fake
// that records every Write* call.  `WriteMessageFlagsModSeq` is a
// FAUNA-FORK seams (vendored go-imap, see third_party/go-imap/FORK.md):
// `WriteMessageFlagsModSeq` appends `MODSEQ (<n>)` to an unsolicited
// flag-change FETCH response (RFC 7162 §3.1.7, once CONDSTORE is
// enabled); `WriteVanished` emits `* VANISHED <uid_set>` for the
// expunge path (RFC 7162 §3.2.10, once QRESYNC is enabled) in place of
// the per-UID `* <seq> EXPUNGE`.
type updateWriter interface {
	WriteNumMessages(n uint32) error
	WriteMessageFlags(seqNum uint32, uid imap.UID, flags []imap.Flag) error
	WriteMessageFlagsModSeq(seqNum uint32, uid imap.UID, flags []imap.Flag, modSeq uint64) error
	WriteExpunge(seqNum uint32) error
	WriteVanished(uids imap.UIDSet) error
}

// metadataFetcher is the seam Session.idle uses to translate UIDs to
// sequence numbers per event.  Two shapes:
//   - SeqInfo(uid): the F1 present-UID path — resolves ONE live UID's
//     sequence number plus the mailbox EXISTS total in a single RPC
//     (Append/Flags/Move-dst), without pulling the whole mailbox.
//   - FetchAllUIDs: the absent-UID rank fallback — returns ALL surviving
//     UIDs ascending, so the caller can rank a UID that is already gone
//     (the pre-QRESYNC expunge / move-source `* <seq> EXPUNGE` path, which
//     modern QRESYNC clients never reach).
//
// Tests substitute a fake that derives both from a canned mailbox snapshot.
type metadataFetcher interface {
	FetchAllUIDs(ctx context.Context, actorID []byte, mailbox string) ([]uint32, error)
	SeqInfo(ctx context.Context, actorID []byte, mailbox string, uid uint32) (seq, total uint32, present bool, err error)
}

// wsrpcMetadataFetcher is the production metadataFetcher.
type wsrpcMetadataFetcher struct{ client wsrpc.Caller }

// FetchAllUIDs wraps `wsrpc.FetchMessageMetadata` with an empty UID-filter
// (returns the full mailbox) and projects to the sorted UID list. Used only by
// the absent-UID rank fallback.
func (f *wsrpcMetadataFetcher) FetchAllUIDs(ctx context.Context, actorID []byte, mailbox string) ([]uint32, error) {
	rows, err := wsrpc.FetchMessageMetadata(ctx, f.client, actorID, mailbox, nil)
	if err != nil {
		return nil, err
	}
	uids := make([]uint32, len(rows))
	for i, r := range rows {
		uids[i] = r.UID
	}
	sort.Slice(uids, func(i, j int) bool { return uids[i] < uids[j] })
	return uids, nil
}

// SeqInfo resolves one live UID's nest-computed sequence number plus the
// mailbox's live-message total (EXISTS) in a single RPC — the F1 present-UID
// IDLE path. `present` is false when the UID isn't live (already expunged).
func (f *wsrpcMetadataFetcher) SeqInfo(ctx context.Context, actorID []byte, mailbox string, uid uint32) (uint32, uint32, bool, error) {
	rows, total, err := wsrpc.FetchMessageMetadataWithTotal(ctx, f.client, actorID, mailbox, []uint32{uid})
	if err != nil {
		return 0, 0, false, err
	}
	for _, r := range rows {
		if r.UID == uid {
			return r.SeqNum, total, true, nil
		}
	}
	return 0, total, false, nil
}

// updateWriterAdapter wraps emersion's *imapserver.UpdateWriter so the
// production code path satisfies the updateWriter seam without leaking
// the concrete type into the per-event translation methods.
type updateWriterAdapter struct{ w *imapserver.UpdateWriter }

func (a *updateWriterAdapter) WriteNumMessages(n uint32) error {
	return a.w.WriteNumMessages(n)
}
func (a *updateWriterAdapter) WriteMessageFlags(seqNum uint32, uid imap.UID, flags []imap.Flag) error {
	return a.w.WriteMessageFlags(seqNum, uid, flags)
}
func (a *updateWriterAdapter) WriteMessageFlagsModSeq(seqNum uint32, uid imap.UID, flags []imap.Flag, modSeq uint64) error {
	return a.w.WriteMessageFlagsModSeq(seqNum, uid, flags, modSeq)
}
func (a *updateWriterAdapter) WriteExpunge(seqNum uint32) error {
	return a.w.WriteExpunge(seqNum)
}
func (a *updateWriterAdapter) WriteVanished(uids imap.UIDSet) error {
	return a.w.WriteVanished(uids)
}

// Idle implements imapserver.SessionIMAP4rev2.Idle.  emersion's
// imapserver/idle.go invokes this on the IMAP server's reader goroutine
// after writing `+ idling` to the wire; the stop channel closes when
// the MUA sends `DONE` (or the IMAP connection drops).
//
// Wire flow per imap-server.md § IDLE:
//
//  1. Register a `fauna.bridges.subscribe_mailbox_state` subscription
//     with nest for the SELECTed mailbox.  The subscription_id nest
//     returns is per-WS-connection-lifetime; F.2's router demuxes
//     incoming `BridgeMailboxStatePush` frames by it.
//  2. Register an event channel with the Backend's notification router
//     under that subscription_id.
//  3. Enter a select loop on (a) the event channel, (b) the stop
//     channel (DONE / connection drop), (c) the per-server idle
//     timeout.
//  4. On every received event, translate to the IMAP wire shape per
//     the mapping below and write it via the
//     UpdateWriter:
//     - append   → EXISTS (new count) + FETCH (UID, FLAGS)
//     - flags    → FETCH (UID, FLAGS)
//     - expunge  → `* VANISHED <uid>` when the MUA ENABLEd QRESYNC,
//     else the per-UID `* <seq> EXPUNGE` fallback
//     - move     → see comment in handleEvent (SELECT'd one mailbox
//     at a time, so each Move push hits exactly one of
//     the two branches)
//  5. On stop close, return nil (clean DONE).  On timeout, return nil
//     (the library writes nothing further; the next command from the
//     MUA — typically `NOOP` or a fresh `IDLE` — resumes traffic).
//  6. If the event channel is closed (WS connection died), return an
//     error so the IMAP layer closes the connection with `BAD Internal
//     error`.  No polling fallback.
//
// QRESYNC expunge wire shape: T3.2-b landed the fork's
// `UpdateWriter.WriteVanished` seam (vendored go-imap, see
// third_party/go-imap/FORK.md), so once the MUA ENABLEs QRESYNC the
// `expunge` (and Move-source) push emits `* VANISHED <uid>` (RFC 7162
// §3.2.10). Before the seam, the implementation forced the
// per-UID `* <seq> EXPUNGE` fallback regardless of QRESYNC; that
// fallback now applies only when QRESYNC is *not* enabled.
//
// MODSEQ on `* <seq> FETCH (UID … FLAGS …)` ships when CONDSTORE is
// enabled for the session, via the FAUNA-FORK
// `UpdateWriter.WriteMessageFlagsModSeq` seam (vendored go-imap, see
// third_party/go-imap/FORK.md) — RFC 7162 §3.1.7. When CONDSTORE is not
// enabled the plain FLAGS form ships. The event carries the new modseq
// (`MailboxStateEvent::{Append,Flags}.modseq`).
//
// NOTIFY (RFC 5465) is upstream-blocked: emersion/go-imap v2's
// imapserver package has no NOTIFY command parser and no
// `Session.Notify` interface hook.  The MDA advertises NOTIFY in its
// CAPABILITY response (per capabilities.go) for future-compatibility,
// but the wire-level NOTIFY command is not parsed today.  When
// upstream lands a NOTIFY hook this file gains a `Notify` method that
// reuses `Register`/`Unregister`/`handleEvent` here; the protocol
// payload shape is identical (one `subscribe_mailbox_state` per
// SET-mailbox).
func (s *Session) Idle(w *imapserver.UpdateWriter, stop <-chan struct{}) error {
	return s.idle(context.Background(), &updateWriterAdapter{w: w}, stop)
}

// idle is the seam-friendly inner method.  Splits ctx / writer / fetcher
// out so tests can drive the loop without standing up an emersion
// UpdateWriter (no live TCP connection) and without a real wsrpc
// client (the metadataFetcher seam injects a canned mailbox view).
func (s *Session) idle(
	parentCtx context.Context,
	w updateWriter,
	stop <-chan struct{},
) error {
	s.mu.Lock()
	actorID := s.actorID
	mailbox := s.selectedMailbox
	client := s.client
	router := s.router
	timeout := s.idleTimeout
	s.mu.Unlock()
	if actorID == nil {
		return errors.New("imap: IDLE requires authenticated state")
	}
	if mailbox == "" {
		return errors.New("imap: IDLE requires a SELECTed mailbox")
	}
	if router == nil {
		// Backend wired without a router — production always supplies
		// one; this guard exists so a test that forgets the router
		// surfaces the gap explicitly rather than nil-panicking.
		return errors.New("imap: IDLE requires a notification router")
	}
	if timeout <= 0 {
		timeout = defaultIdleTimeout
	}

	subCtx, cancel := context.WithTimeout(parentCtx, idleSubscribeRPCTimeout)
	subscriptionID, err := wsrpc.SubscribeMailboxState(subCtx, client, actorID, mailbox)
	cancel()
	if err != nil {
		return fmt.Errorf("imap: IDLE subscribe failed: %w", err)
	}

	eventCh := router.Register(subscriptionID)
	defer router.Unregister(subscriptionID)

	fetcher := s.metadataFetcher(client)

	timer := time.NewTimer(timeout)
	defer timer.Stop()

	for {
		select {
		case ev, ok := <-eventCh:
			if !ok {
				// A closed event
				// channel means the router shut down (e.g. WS-drop on
				// the wsrpc Client). Close the IMAP connection with
				// `BAD Internal error`; the MUA reconnects + re-SELECTs.
				// In the current router design the channel never
				// closes on its own (only Unregister removes the
				// route, and that happens after this loop exits), so
				// this branch is a future-proofing seam.
				return errors.New("imap: IDLE push channel closed (WS drop?)")
			}
			if err := s.handleEvent(parentCtx, w, fetcher, actorID, mailbox, ev); err != nil {
				return err
			}
		case <-stop:
			// MUA sent DONE (or the IMAP connection dropped — emersion
			// closes `stop` either way). Clean return; emersion
			// writes the `<tag> OK IDLE terminated` itself.
			return nil
		case <-timer.C:
			// Per-server IDLE timeout fired. RFC 2177 §3 lets the
			// server end the IDLE; emersion handles the unilateral
			// teardown by returning from Session.Idle. The MUA
			// observes a connection close and reconnects.
			return nil
		case <-parentCtx.Done():
			return parentCtx.Err()
		}
	}
}

// metadataFetcher returns the per-Session metadataFetcher seam.  Tests
// substitute by overriding s.idleFetcher; production constructs a
// fresh wsrpcMetadataFetcher per call (stateless wrapper around the
// shared wsrpc.Caller).
func (s *Session) metadataFetcher(client wsrpc.Caller) metadataFetcher {
	s.mu.Lock()
	override := s.idleFetcher
	s.mu.Unlock()
	if override != nil {
		return override
	}
	return &wsrpcMetadataFetcher{client: client}
}

// handleEvent translates one `MailboxStateEvent` to wire-side unsolicited
// responses per the mapping below.  Returns
// an error only if a write to the UpdateWriter fails; UID-to-seq
// translation failures log and drop the event (the next mutation will
// re-trigger and the MUA's view re-converges).
func (s *Session) handleEvent(
	parentCtx context.Context,
	w updateWriter,
	fetcher metadataFetcher,
	actorID []byte,
	mailbox string,
	ev wsrpc.MailboxStateEvent,
) error {
	switch ev.Kind {
	case wsrpc.MailboxStateEventAppend:
		// Scored-before-visible serve gate (Phase-3 design D5; content-scoring.md
		// § Timing): a message delivered while the client IDLEs must be per-user
		// scored — and any spam re-filed to Junk — BEFORE it is announced, the
		// IDLE twin of the SELECT-time pass (select.go). Score FIRST, then take
		// the metadata snapshot below, so the EXISTS + append-FETCH already
		// reflect a move-to-Junk (the moved UID drops out of the snapshot; its own
		// Move push reaches the client on the source subscription separately).
		// INBOX only; best-effort — scoreSelectedInbox never fails the IDLE, and
		// the $FaunaSpamScored watermark makes it idempotent with the SELECT pass
		// (a message scored at either entry point is skipped at the other).
		if strings.EqualFold(mailbox, "INBOX") {
			s.scoreSelectedInbox(parentCtx, s.announceScoreOpener())
		}
		// Append: emit EXISTS (new total) + FETCH (UID, FLAGS).  The
		// appended UID is live, so a single-UID lookup gives its seqNum AND
		// the post-append surviving-UID count (EXISTS) in one round-trip —
		// no whole-mailbox fetch (F1). The SQLite write has already committed
		// by the time the push fires (F.1 emission-after-commit rule).
		seq, total, present, err := s.idleSeqInfo(parentCtx, fetcher, actorID, mailbox, ev.Uid)
		if err != nil {
			s.idleLogWarn("imap: IDLE append metadata fetch failed", "uid", ev.Uid, "err", err)
			return nil
		}
		// EXISTS always ships (reconverges the MUA even if the append raced an
		// expunge). The per-message FETCH ships only when the UID is still live.
		if err := w.WriteNumMessages(total); err != nil {
			return err
		}
		if !present {
			// The appended UID was expunged between commit and our lookup (or
			// the reply is stale) — skip the per-message FETCH; EXISTS suffices.
			return nil
		}
		return s.flagsUpdate(w, seq, ev.Uid, toIMAPFlags(ev.Flags), ev.Modseq)

	case wsrpc.MailboxStateEventFlags:
		// Flags-only: emit FETCH (UID, FLAGS).  No EXISTS — the message
		// count is unchanged.  The changed UID is live, so a single-UID
		// lookup gives its seqNum without a whole-mailbox fetch (F1).
		// MODSEQ rides along when CONDSTORE is enabled (see flagsUpdate +
		// Idle method docstring).
		seq, _, present, err := s.idleSeqInfo(parentCtx, fetcher, actorID, mailbox, ev.Uid)
		if err != nil {
			s.idleLogWarn("imap: IDLE flags metadata fetch failed", "uid", ev.Uid, "err", err)
			return nil
		}
		if !present {
			// UID gone — the flag-change applied to a row that's now
			// expunged.  Drop silently; the expunge push will reach
			// the MUA separately.
			return nil
		}
		return s.flagsUpdate(w, seq, ev.Uid, toIMAPFlags(ev.Flags), ev.Modseq)

	case wsrpc.MailboxStateEventExpunge:
		// QRESYNC (RFC 7162 §3.2.10): once the client has ENABLE
		// QRESYNC'd, report the expunged UID via `* VANISHED <uid>` — no
		// pre-expunge sequence-number derivation, no metadata round-trip
		// (VANISHED carries UIDs, which are stable). T3.2-b landed the
		// fork's WriteVanished seam; before it, the implementation
		// forced the per-UID EXPUNGE fallback below
		// regardless of QRESYNC because the library couldn't emit VANISHED.
		if s.qresyncActive() {
			return w.WriteVanished(imap.UIDSetNum(imap.UID(ev.Uid)))
		}
		// Per-UID `* <seq> EXPUNGE` fallback (pre-QRESYNC wire shape).
		// The seq is the *pre-expunge* sequence number — derived from the
		// post-expunge snapshot + this UID using the same arithmetic
		// expunge.go uses: pre_seq(r) = |{s ∈ surviving : s ≤ r}| + 1.
		// Here we have exactly one removed UID per event (the F.1
		// emitter fires per-UID), so the "+1" form is exact.
		uids, err := s.idleFetchUIDs(parentCtx, fetcher, actorID, mailbox)
		if err != nil {
			s.idleLogWarn("imap: IDLE expunge metadata fetch failed", "uid", ev.Uid, "err", err)
			return nil
		}
		// pre_seq for a single removed UID r: count surviving UIDs ≤
		// r (which excludes r itself, already gone post-expunge) + 1.
		sIdx := sort.Search(len(uids), func(j int) bool { return uids[j] > ev.Uid })
		seq := uint32(sIdx) + 1
		return w.WriteExpunge(seq)

	case wsrpc.MailboxStateEventMove:
		// Move: the nest emits one push to the source mailbox's
		// subscribers and one to the destination's, each naming its side
		// (`imap-server.md` § Push wiring). The side comes from the event
		// alone: IMAP UIDs are per-mailbox, so dst_uid routinely also names
		// an unrelated message in the source (a first move into Archive gets
		// UID 1, and INBOX has a UID 1) and no UID lookup can tell the two
		// ends apart.
		switch ev.Side {
		case wsrpc.MoveSideDestination:
			// dst_uid arrives: EXISTS + FETCH. A single-UID lookup gives its
			// seq and the new count; absent means a later expunge overtook
			// the move — drop silently.
			dstSeq, total, dstPresent, err := s.idleSeqInfo(parentCtx, fetcher, actorID, mailbox, ev.DstUid)
			if err != nil {
				s.idleLogWarn("imap: IDLE move metadata fetch failed",
					"src_uid", ev.SrcUid, "dst_uid", ev.DstUid, "err", err)
				return nil
			}
			if !dstPresent {
				return nil
			}
			if err := w.WriteNumMessages(total); err != nil {
				return err
			}
			// The Move event payload doesn't carry the destination's
			// FLAGS — the F.1 emitter doesn't enrich.  Emit a FETCH with no
			// flags; the MUA can issue a FETCH to refresh if it
			// needs them.  This is a sharper signal than dropping the
			// FETCH entirely (preserves the "new message at seq N"
			// notification for MUAs that decide whether to download
			// based on seq+UID alone).
			return w.WriteMessageFlags(dstSeq, imap.UID(ev.DstUid), nil)
		case wsrpc.MoveSideSource:
			// src_uid leaves this mailbox. Under QRESYNC, report it via
			// `* VANISHED <src_uid>` (RFC 7162 §3.2.10) — UID-stable, no fetch.
			if s.qresyncActive() {
				return w.WriteVanished(imap.UIDSetNum(imap.UID(ev.SrcUid)))
			}
			// Pre-QRESYNC fallback: rank src_uid with the full list, mirroring
			// expunge.go's pre_seq arithmetic. src still present (commit raced
			// our snapshot) ⇒ |{s < src_uid}| + 1; src already gone (steady
			// state) ⇒ |{s ≤ src_uid in surviving}| + 1.
			uids, err := s.idleFetchUIDs(parentCtx, fetcher, actorID, mailbox)
			if err != nil {
				s.idleLogWarn("imap: IDLE move metadata fetch failed",
					"src_uid", ev.SrcUid, "dst_uid", ev.DstUid, "err", err)
				return nil
			}
			if containsUID(uids, ev.SrcUid) {
				sIdx := sort.Search(len(uids), func(j int) bool { return uids[j] >= ev.SrcUid })
				return w.WriteExpunge(uint32(sIdx) + 1)
			}
			sIdx := sort.Search(len(uids), func(j int) bool { return uids[j] > ev.SrcUid })
			return w.WriteExpunge(uint32(sIdx) + 1)
		default:
			// Every nest emitter names the side; a Move without one is
			// malformed — log and drop, never guess.
			s.idleLogWarn("imap: IDLE move without a side",
				"src_uid", ev.SrcUid, "dst_uid", ev.DstUid, "side", string(ev.Side))
			return nil
		}
	default:
		// Unknown event kind — log and drop. The wire protocol is
		// open-ended (new kinds may land on nest before the MDA
		// learns about them); silent drop is the safe default.
		s.idleLogWarn("imap: IDLE unknown event kind", "kind", string(ev.Kind))
		return nil
	}
}

// flagsUpdate writes an unsolicited flag-change FETCH response, attaching
// `MODSEQ (<n>)` when CONDSTORE is active for the session (RFC 7162
// §3.1.7) via the FAUNA-FORK WriteMessageFlagsModSeq seam; otherwise the
// plain FLAGS form. Used by the append + flags IDLE event branches.
func (s *Session) flagsUpdate(w updateWriter, seq, uid uint32, flags []imap.Flag, modSeq int64) error {
	if s.condStoreActive() {
		return w.WriteMessageFlagsModSeq(seq, imap.UID(uid), flags, uint64(modSeq))
	}
	return w.WriteMessageFlags(seq, imap.UID(uid), flags)
}

// idleFetchUIDs runs the per-event metadata round-trip with the bounded
// `idleSeqLookupRPCTimeout`.  Returned UIDs are sorted ascending — the
// caller depends on that for `sort.Search` translations.
func (s *Session) idleFetchUIDs(
	parentCtx context.Context,
	fetcher metadataFetcher,
	actorID []byte,
	mailbox string,
) ([]uint32, error) {
	ctx, cancel := context.WithTimeout(parentCtx, idleSeqLookupRPCTimeout)
	defer cancel()
	return fetcher.FetchAllUIDs(ctx, actorID, mailbox)
}

// idleSeqInfo runs the per-event single-UID sequence lookup with the bounded
// `idleSeqLookupRPCTimeout` — the F1 present-UID path (Append/Flags/Move-dst).
// Returns the UID's nest-computed sequence number, the mailbox EXISTS total,
// and whether the UID is still live.
func (s *Session) idleSeqInfo(
	parentCtx context.Context,
	fetcher metadataFetcher,
	actorID []byte,
	mailbox string,
	uid uint32,
) (seq, total uint32, present bool, err error) {
	ctx, cancel := context.WithTimeout(parentCtx, idleSeqLookupRPCTimeout)
	defer cancel()
	return fetcher.SeqInfo(ctx, actorID, mailbox, uid)
}

// idleLogWarn is a small indirection so the seam tests (which set
// s.logger = slog.Default()) and the production path agree on the
// log shape without each call-site repeating the nil-guard.
func (s *Session) idleLogWarn(msg string, args ...any) {
	if s.logger != nil {
		s.logger.Warn(msg, args...)
	}
}

// seqOf returns the 1-based sequence number of uid in the ascending
// uids list, or 0 if uid is absent.
func seqOf(uids []uint32, uid uint32) uint32 {
	idx := sort.Search(len(uids), func(j int) bool { return uids[j] >= uid })
	if idx == len(uids) || uids[idx] != uid {
		return 0
	}
	return uint32(idx) + 1
}

// containsUID is a binary-search "is this UID in the ascending list"
// helper.  Folded into a function for readability at the Move
// switch-case site.
func containsUID(uids []uint32, uid uint32) bool {
	return seqOf(uids, uid) != 0
}

// (toIMAPFlags lives in fetch.go; reused here for the IDLE event
// translation path's FLAGS-write seam.)
