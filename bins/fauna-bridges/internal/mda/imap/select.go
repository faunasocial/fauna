package imap

import (
	"context"
	"errors"
	"sort"
	"strings"
	"time"

	"github.com/emersion/go-imap/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

const selectRPCTimeout = 30 * time.Second

// standardFlags is the per-mailbox FLAGS list returned in SELECT /
// EXAMINE responses. RFC 9051 § 2.3.2 system flags plus \Junk (per
// `imap-server.md` § Standard mailboxes — the spam-training contract
// uses \Junk as a system-flag-with-special-semantics).
var standardFlags = []imap.Flag{
	imap.FlagSeen,
	imap.FlagAnswered,
	imap.FlagFlagged,
	imap.FlagDeleted,
	imap.FlagDraft,
	imap.FlagJunk,
}

// permanentFlags advertises which flags the user can change on
// messages permanently. RFC 9051 § 7.5.1 — `\*` indicates the server
// accepts arbitrary keywords as well; we mirror Dovecot's default of
// "all system flags + arbitrary keywords".
var permanentFlags = []imap.Flag{
	imap.FlagSeen,
	imap.FlagAnswered,
	imap.FlagFlagged,
	imap.FlagDeleted,
	imap.FlagDraft,
	imap.FlagJunk,
	"\\*",
}

// Select implements imapserver.Session.Select for both SELECT and
// EXAMINE. emersion's handleSelect calls this with options.ReadOnly
// flipped per the verb; we honor neither distinction at the nest
// layer (read-vs-write enforcement is per-RPC server-side) and
// simply stash the SelectedMailbox state for downstream FETCH /
// SEARCH / STORE / EXPUNGE calls.
//
// Per imap-server.md § Read surface: SELECT translates 1-for-1 to
// `fauna.bridges.select_mailbox`; the reply carries every value the
// SELECT response needs except FLAGS / PERMANENTFLAGS, which are
// the standard set defined above.
func (s *Session) Select(mailbox string, options *imap.SelectOptions) (*imap.SelectData, error) {
	s.mu.Lock()
	actorID := s.actorID
	client := s.client
	var opener mailfauna.RecordOpener
	if s.recordOpener != nil {
		opener = s.recordOpener
	}
	s.mu.Unlock()
	if actorID == nil {
		return nil, errors.New("imap: SELECT requires authenticated state")
	}
	if client == nil {
		return nil, errors.New("imap: SELECT has no wsrpc client")
	}

	ctx, cancel := context.WithTimeout(context.Background(), selectRPCTimeout)
	defer cancel()

	// Post-delivery per-user spam scoring (mail-spam.md § Scoring placement
	// — the AUTH'd-MDA-session position). On a read-write SELECT of INBOX,
	// score the un-scored messages and re-file spam to Junk BEFORE the
	// select_mailbox snapshot below, so the SELECT response's EXISTS / UID
	// state already reflects the moves (a re-filed message looks as if it
	// was filed at delivery). Skipped on EXAMINE (read-only must not
	// mutate) and on every non-INBOX mailbox. Best-effort — never fails the
	// SELECT (see scoreSelectedInbox).
	if (options == nil || !options.ReadOnly) && strings.EqualFold(mailbox, "INBOX") {
		s.scoreSelectedInbox(ctx, opener)
	}

	// QRESYNC SELECT parameter (RFC 7162 §3.2.5): forward
	// (last_uid_validity, last_modseq) to nest so the restore-divergence
	// detection seam (γ) can compare last_modseq against the mailbox's
	// highestmodseq (imap-server.md § Restore divergence detection). The
	// fork's `(QRESYNC ...)` SELECT-param parser populates options.QResync;
	// nil when the client didn't supply it (bridge passed nil pre-T3.2-b).
	var qr *wsrpc.QResyncHint
	if options != nil && options.QResync != nil {
		qr = &wsrpc.QResyncHint{
			LastUIDValidity: options.QResync.UIDValidity,
			LastModseq:      int64(options.QResync.ModSeq),
		}
	}

	picked, err := wsrpc.SelectMailbox(ctx, client, actorID, mailbox, qr)
	if err != nil {
		// Either nest reports no_such_mailbox (sentinel) or a
		// transport / decode failure. In both cases the client must
		// be in the authenticated (not selected) state, and any
		// stashed SELECT state from a prior mailbox is invalidated.
		s.mu.Lock()
		s.selectedMailbox = ""
		s.selectedUIDValidity = 0
		s.lastKnownModseq = 0
		s.mu.Unlock()
		return nil, err
	}

	s.mu.Lock()
	s.selectedMailbox = mailbox
	s.selectedUIDValidity = picked.UIDValidity
	s.lastKnownModseq = picked.HighestModseq
	if options != nil && options.CondStore {
		s.condStoreEnabled = true
	}
	if options != nil && options.QResync != nil {
		s.qresyncEnabled = true
	}
	s.mu.Unlock()

	data := &imap.SelectData{
		Flags:          standardFlags,
		PermanentFlags: permanentFlags,
		NumMessages:    picked.Exists,
		NumRecent:      picked.Recent,
		UIDNext:        imap.UID(picked.UIDNext),
		UIDValidity:    picked.UIDValidity,
		HighestModSeq:  uint64(picked.HighestModseq),
	}
	// IMAP4rev1 OK [UNSEEN <seq>] response — emersion only writes it
	// when FirstUnseenSeqNum != 0 AND the client hasn't ENABLE'd
	// IMAP4rev2. nest returns first_unseen_uid (a UID, not a seq);
	// Phase C maps it onto the unseen-count `unseen` field as the
	// IMAP4rev1 response is best-effort and Phase D will refine the
	// UID→seq translation when STORE/MOVE actually depend on it.
	data.FirstUnseenSeqNum = picked.Unseen

	// Inline SELECT (QRESYNC ...) fast-path (RFC 7162 §3.2.5,
	// imap-server.md § QRESYNC SELECT). When the client supplied the
	// QRESYNC SELECT parameter, the common case is `last_uid_validity`
	// unchanged AND `last_modseq <= highestmodseq`: synthesize the
	// VANISHED (EARLIER) set + changed-message FETCH responses inline (the
	// fork's handleSelect emits them from data.QResync). We skip it when:
	//   - UIDVALIDITY changed (mailbox deleted-and-recreated → full resync), or
	//   - last_modseq > highestmodseq (the restore-divergence (γ) case —
	//     nest writes the divergence row server-side and the client falls
	//     through to a full resync; synthesizing VANISHED here would lie).
	// In those cases data.QResync stays nil and the client reconciles via
	// the standard SELECT responses (UIDVALIDITY/HIGHESTMODSEQ).
	if options != nil && options.QResync != nil &&
		options.QResync.UIDValidity == picked.UIDValidity &&
		int64(options.QResync.ModSeq) <= picked.HighestModseq {
		qr, err := buildInlineQResyncSelectData(ctx, client, actorID, mailbox, int64(options.QResync.ModSeq))
		if err != nil {
			// The inline fast-path is an optimization, not a correctness
			// requirement: a reconnecting QRESYNC client reconciles just
			// as correctly via a follow-up `UID FETCH … (CHANGEDSINCE n
			// VANISHED)` (imap-server.md § QRESYNC SELECT —
			// honest-advertisement). So a delta-build RPC failure degrades
			// to that fallback rather than failing the whole SELECT.
			if s.logger != nil {
				s.logger.Warn("imap: inline QRESYNC SELECT delta build failed; client reconciles via UID FETCH",
					"err", err, "mailbox", mailbox)
			}
		} else {
			data.QResync = qr
		}
	}
	return data, nil
}

// buildInlineQResyncSelectData assembles the inline SELECT (QRESYNC ...)
// fast-path output (RFC 7162 §3.2.5): the VANISHED (EARLIER) UID set and
// the changed-message FETCH list the fork's handleSelect emits within the
// SELECT response. The vanished set is the expunge-log tombstones since
// lastModseq (from list_messages); the changed list is every current
// message whose modseq advanced past lastModseq, paired with its 1-based
// sequence number (its position in the full UID-ordered mailbox).
//
// Mirrors the `UID FETCH (CHANGEDSINCE n VANISHED)` path in fetch.go: one
// list_messages(since_modseq) call for the ExpungedUIDs + one
// fetch_message_metadata call for the full ordered row set (needed to map
// each changed UID to its sequence number).
func buildInlineQResyncSelectData(ctx context.Context, client wsrpc.Caller, actorID []byte, mailbox string, lastModseq int64) (*imap.SelectQResyncData, error) {
	lm, err := wsrpc.ListMessages(ctx, client, wsrpc.ListMessagesParams{
		ActorID:     actorID,
		Mailbox:     mailbox,
		SinceModseq: &lastModseq,
	})
	if err != nil {
		return nil, err
	}
	rows, err := wsrpc.FetchMessageMetadata(ctx, client, actorID, mailbox, nil)
	if err != nil {
		return nil, err
	}
	sort.Slice(rows, func(i, j int) bool { return rows[i].UID < rows[j].UID })

	out := &imap.SelectQResyncData{}
	if len(lm.ExpungedUIDs) > 0 {
		out.Vanished = uidSetFromUint32s(lm.ExpungedUIDs)
	}
	for i, m := range rows {
		// CHANGEDSINCE semantics (RFC 7162 §3.1.4): only messages whose
		// modseq advanced past the client's last_modseq are reported.
		if m.Modseq <= lastModseq {
			continue
		}
		out.Changed = append(out.Changed, imap.SelectQResyncChange{
			SeqNum: uint32(i + 1),
			UID:    imap.UID(m.UID),
			Flags:  toIMAPFlags(m.Flags),
			ModSeq: uint64(m.Modseq),
		})
	}
	return out, nil
}

// Unselect implements imapserver.Session.Unselect for the UNSELECT
// command and emersion's CLOSE-after-Expunge sequence. Clears every
// SELECT-state field; subsequent commands must re-SELECT.
//
// Per imap-server.md § Read surface: no nest RPC fires on UNSELECT
// (the selection is a per-session in-memory state).
func (s *Session) Unselect() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.selectedMailbox = ""
	s.selectedUIDValidity = 0
	s.lastKnownModseq = 0
	return nil
}

// Expunge lives in expunge.go (Phase D.3). CLOSE in IMAP is "Expunge
// then Unselect"; emersion's handleUnselect calls Session.Expunge
// followed by Session.Unselect — the real D.3 implementation handles
// both that CLOSE path and a standalone EXPUNGE / UID EXPUNGE command.
