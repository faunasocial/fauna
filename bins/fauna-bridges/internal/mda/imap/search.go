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

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

const searchRPCTimeout = 30 * time.Second

// partitionedCriteria is the output of partitionSearchCriteria: the
// flag/header/date/size axes become wire `SearchTerm`s (handled
// server-side by `fauna.bridges.search_messages`), and the Body / Text
// axes become local `bodyTerms` (handled by `bodySearch` after
// decrypting the index-hint segments).
type partitionedCriteria struct {
	flagTerms []wsrpc.SearchTerm
	bodyTerms []string
}

// partitionSearchCriteria walks an emersion `*imap.SearchCriteria` and
// splits it into:
//   - server-side `flagTerms`: one wire `SearchTerm` per Flag,
//     NotFlag, Header (From/To/Cc/Subject), Since, Before, Larger,
//     Smaller predicate.
//   - local `bodyTerms`: every Body and Text string, lowercased
//     (matches the tokenizer's lowercase fold so the local match is
//     case-insensitive).
//
// Or / Not / SentSince / SentBefore are out of scope for Phase C and
// return an error — the IMAP layer maps that to `BAD search criterion
// not supported`. Unsupported Header keys (anything other than
// From/To/Cc/Subject) also error.
func partitionSearchCriteria(c *imap.SearchCriteria) (partitionedCriteria, error) {
	if c == nil {
		return partitionedCriteria{}, nil
	}
	if len(c.Or) > 0 {
		return partitionedCriteria{}, errors.New("imap: SEARCH OR not supported in Phase C")
	}
	if len(c.Not) > 0 {
		return partitionedCriteria{}, errors.New("imap: SEARCH NOT not supported in Phase C")
	}
	if !c.SentSince.IsZero() {
		return partitionedCriteria{}, errors.New("imap: SEARCH SENTSINCE not supported (header Date column not indexed)")
	}
	if !c.SentBefore.IsZero() {
		return partitionedCriteria{}, errors.New("imap: SEARCH SENTBEFORE not supported (header Date column not indexed)")
	}

	var out partitionedCriteria

	for _, f := range c.Flag {
		out.flagTerms = append(out.flagTerms, wsrpc.NewSearchTermHasFlag(string(f)))
	}
	for _, f := range c.NotFlag {
		out.flagTerms = append(out.flagTerms, wsrpc.NewSearchTermLacksFlag(string(f)))
	}
	for _, h := range c.Header {
		field, ok := headerFieldFromKey(h.Key)
		if !ok {
			return partitionedCriteria{}, fmt.Errorf("imap: SEARCH HEADER %q not indexed (only From/To/Cc/Subject)", h.Key)
		}
		out.flagTerms = append(out.flagTerms,
			wsrpc.NewSearchTermHeaderContains(field, strings.ToLower(h.Value)))
	}
	if !c.Since.IsZero() {
		out.flagTerms = append(out.flagTerms, wsrpc.NewSearchTermSinceInternalDate(c.Since.Unix()))
	}
	if !c.Before.IsZero() {
		out.flagTerms = append(out.flagTerms, wsrpc.NewSearchTermBeforeInternalDate(c.Before.Unix()))
	}
	if c.Larger > 0 {
		out.flagTerms = append(out.flagTerms, wsrpc.NewSearchTermLarger(uint32(c.Larger)))
	}
	if c.Smaller > 0 {
		out.flagTerms = append(out.flagTerms, wsrpc.NewSearchTermSmaller(uint32(c.Smaller)))
	}

	for _, b := range c.Body {
		out.bodyTerms = append(out.bodyTerms, strings.ToLower(b))
	}
	for _, t := range c.Text {
		out.bodyTerms = append(out.bodyTerms, strings.ToLower(t))
	}

	return out, nil
}

// headerFieldFromKey maps an IMAP `SearchCriteriaHeaderField.Key` to the
// wire `HeaderField`, or reports `ok=false` for unsupported keys. The
// SEARCH HEADER axis is fixed to From/To/Cc/Subject per
// `imap-server.md` § SEARCH (the four `*_norm` columns on
// `bridge_imap_messages`); other headers (Date, Message-Id, …) would
// need new norm columns.
func headerFieldFromKey(key string) (wsrpc.HeaderField, bool) {
	switch strings.ToLower(key) {
	case "from":
		return wsrpc.HeaderFieldFrom, true
	case "to":
		return wsrpc.HeaderFieldTo, true
	case "cc":
		return wsrpc.HeaderFieldCc, true
	case "subject":
		return wsrpc.HeaderFieldSubject, true
	default:
		return "", false
	}
}

// bodySearch opens each segment's index hint through the ONE uniform
// serve rule (`mailfauna.OpenStoredRecord`, Phase-3 D1): the sealed hint
// (an `EncryptToRecipient` `MailRecordEnvelope`, not an MSEK-AEAD blob)
// HPKE-opens via the per-connection opener; an unsealed hint is refused
// and a nil opener refuses every hint — no storage-mode branch. It then
// tokenizes the plaintext and reports the UIDs whose token set contains
// every query token (every term tokenized to at least one query token, and every
// query token must appear in the segment's set). The UID resolution
// comes from `uidByMid` (a snapshot of the placement layer's
// `(message_id → uid)` mapping for the selected mailbox); segments
// whose message_id is not in the map are silently skipped
// (expunged-between-RPCs case, same convention as FETCH).
//
// The match is an intersection over terms: missing any required
// token drops the UID. An empty `terms` slice returns every UID in
// the segment list whose message_id resolves.
//
// `answer`, when non-nil, is the content index speaking for the messages the
// client leg has published (`indexAnswerer` — this process itself stages
// nothing since the 2026-08-10 carrier ruling). It is consulted ONCE, before
// any hint is opened, and it splits the mailbox in two: a message the index
// covers takes its verdict from there and **skips the HPKE open entirely** —
// the actual payoff of the whole leg — while everything else is scanned
// exactly as before.
//
// The split is what makes the swap correct rather than merely faster. The slice
// is built opportunistically (it holds what earlier searches happened to
// decrypt), so it is never "the mailbox": answering purely from it would
// silently under-report, and scanning everything anyway would make it pointless.
// Any failure on the index side collapses the covered set to empty, which
// degrades this function to the pre-index scan bit for bit.
func bodySearch(
	opener mailfauna.RecordOpener,
	segments []wsrpc.IndexSegment,
	terms []string,
	uidByMid map[string]uint32,
	answer indexAnswerFunc,
) ([]uint32, error) {
	var queryTokens [][]string
	for _, term := range terms {
		toks := mailfauna.Tokenize(term).Tokens
		if len(toks) > 0 {
			queryTokens = append(queryTokens, toks)
		}
	}

	// Ask the index before opening anything. Deliberately skipped when the terms
	// carry no tokens: the scan then matches every message, so there is nothing
	// for the index to narrow and no reason to pay a rail listing for it.
	covered, indexMatched := map[string]bool{}, map[string]bool{}
	if answer != nil && len(queryTokens) > 0 {
		candidates := make([]string, 0, len(segments))
		offered := make(map[string]bool, len(segments))
		for _, seg := range segments {
			mid := string(seg.MessageID)
			// Only messages this mailbox can resolve, and each asked about once
			// — two placements of one Message-ID must get one verdict, never a
			// half-scanned/half-indexed split.
			if mid == "" || offered[mid] {
				continue
			}
			if _, ok := uidByMid[mid]; !ok {
				continue
			}
			offered[mid] = true
			candidates = append(candidates, mid)
		}
		if len(candidates) > 0 {
			c, m, err := answer(terms, candidates)
			if err == nil {
				covered, indexMatched = c, m
			}
			// On error the sets stay empty, so every message is scanned. A rail
			// outage costs the user latency, never an answer.
		}
	}

	var matched []uint32
	for _, seg := range segments {
		mid := string(seg.MessageID)
		uid, ok := uidByMid[mid]
		if !ok {
			continue
		}
		if covered[mid] {
			// Answered from the sealed slice — no open, no tokenize, no stage
			// (it is already indexed, so staging would drop it anyway).
			if indexMatched[mid] {
				matched = append(matched, uid)
			}
			continue
		}
		// OpenStoredRecordAt so the epoch-aware session opener can try the
		// hint's candidate epoch keys (content-sealing-epochs design § 4,
		// Track 1b). The hint shares the body's key schedule on every
		// producer path: MTA ingest and in-domain delivery seal hint + body
		// to the SAME epoch-gated recipient key in one transaction
		// (`seal_and_persist_local` — the "dedicated index key" is Phase E,
		// not yet in play), and APPEND's hint seals to the session-cached
		// STANDING pubkey (actorIndexKey is nil pre-Phase-E, falling back
		// to the standing MLS pubkey), which the chain's standing arm
		// opens. seg.StoredAt = the seal instant; 0 (unknown — the nest's
		// append-time clock read failed) means the hint is standing-sealed
		// and the basis is unused past the standing arm.
		var sealBasis uint64
		if seg.StoredAt > 0 {
			sealBasis = uint64(seg.StoredAt)
		}
		plaintext, err := mailfauna.OpenStoredRecordAt(opener, seg.EncryptedIndexHint, sealBasis)
		if err != nil {
			return nil, fmt.Errorf("decrypt segment for UID %d: %w", uid, err)
		}
		segTokens := mailfauna.Tokenize(string(plaintext)).Tokens
		segSet := make(map[string]struct{}, len(segTokens))
		for _, tok := range segTokens {
			segSet[tok] = struct{}{}
		}
		// Best-effort wipe of the decrypted hint after tokenization.
		zeroize(plaintext)

		allMatch := true
		for _, qt := range queryTokens {
			for _, tok := range qt {
				if _, found := segSet[tok]; !found {
					allMatch = false
					break
				}
			}
			if !allMatch {
				break
			}
		}
		if allMatch {
			matched = append(matched, uid)
		}
	}
	sort.Slice(matched, func(i, j int) bool { return matched[i] < matched[j] })
	return matched, nil
}

// Search implements imapserver.Session.Search. Phase C.7: flag /
// header / date / size axes go to nest via search_messages; body / text
// axes are answered from the session's sealed content-index slice where it
// covers the message, and matched locally against decrypted index-hint
// segments where it does not (see bodySearch).
// OR / NOT / SentSince / SentBefore return BAD per the Phase C
// scope-reduce (see partitionSearchCriteria).
func (s *Session) Search(numKind imapserver.NumKind, criteria *imap.SearchCriteria, options *imap.SearchOptions) (*imap.SearchData, error) {
	s.mu.Lock()
	var opener mailfauna.RecordOpener
	if s.recordOpener != nil {
		opener = s.recordOpener
	}
	s.mu.Unlock()
	return s.searchWithDecryptor(numKind, criteria, options, opener)
}

// searchWithDecryptor is the seam-friendly implementation. Tests
// inject a stub opener here without standing up a real MLS
// capability. NumKind selects whether to emit UIDs (RFC 9051 § 6.4.8
// UID SEARCH) or sequence numbers (plain SEARCH); Phase C always
// returns UIDs on the wire — sequence-number translation falls back to
// "use uid as seq" today (acceptable for the Dovecot-parity baseline;
// a proper translation lands with the per-mailbox snapshot model in
// Phase D).
//
// The body axis opens each index-hint segment through the per-record
// serve rule (`mailfauna.OpenStoredRecord` via the `RecordOpener` seam,
// same as body-FETCH) — *not* the MSEK-AEAD `Decrypt`: a sealed index
// hint is an `EncryptToRecipient` `MailRecordEnvelope`, sealed to the
// actor's leaf init pubkey, openable only with the snapshot's leaf
// secret (mirrors fetch.go's `Decrypt`→record-opener migration).
func (s *Session) searchWithDecryptor(
	numKind imapserver.NumKind,
	criteria *imap.SearchCriteria,
	_ *imap.SearchOptions,
	opener mailfauna.RecordOpener,
) (*imap.SearchData, error) {
	s.mu.Lock()
	actorID := s.actorID
	mailbox := s.selectedMailbox
	client := s.client
	s.mu.Unlock()
	if actorID == nil {
		return nil, errors.New("imap: SEARCH requires authenticated state")
	}
	if mailbox == "" {
		return nil, errors.New("imap: SEARCH requires a SELECTed mailbox")
	}
	if criteria == nil {
		criteria = &imap.SearchCriteria{}
	}

	part, err := partitionSearchCriteria(criteria)
	if err != nil {
		return nil, err
	}
	// No upfront opener/snapshot guard for the body axis: each index hint
	// opens in bodySearch (`mailfauna.OpenStoredRecord`, Phase-3 D1). A
	// hint with no opener (no MLS snapshot provisioned — AUTH succeeds
	// regardless) surfaces the missing-snapshot error at open time, per
	// hint. Same posture as the body-axis FETCH path (fetch.go).

	ctx, cancel := context.WithTimeout(context.Background(), searchRPCTimeout)
	defer cancel()

	// Decide whether each axis runs.
	wantsFlag := len(part.flagTerms) > 0
	wantsBody := len(part.bodyTerms) > 0
	// When both axes are empty the wire RPC still works (server-side
	// "no filter" returns every UID in the mailbox).
	wantsFlagRPC := wantsFlag || !wantsBody

	var flagUIDs []uint32
	if wantsFlagRPC {
		got, err := wsrpc.SearchMessages(ctx, client, actorID, mailbox, part.flagTerms)
		if err != nil {
			return nil, err
		}
		flagUIDs = got
	}

	var finalUIDs []uint32
	if wantsBody {
		// Snapshot the placement layer's (uid → message_id) mapping for
		// the selected mailbox. The body-axis match resolves matched
		// segments back to UIDs via this snapshot.
		metaRows, err := wsrpc.FetchMessageMetadata(ctx, client, actorID, mailbox, nil)
		if err != nil {
			return nil, err
		}
		uidByMid := make(map[string]uint32, len(metaRows))
		for _, m := range metaRows {
			uidByMid[string(m.MessageID)] = m.UID
		}

		// Pull every index-hint segment for the actor scoped to this
		// mailbox.  `limit=0` = no limit; the body-axis cap on
		// encrypted-mode SEARCH is the deployment's segment count, not
		// a synthetic page size.
		mailboxPtr := mailbox
		segReply, err := wsrpc.FetchIndexSegmentsSince(ctx, client, wsrpc.FetchIndexSegmentsSinceParams{
			ActorID:     actorID,
			Mailbox:     &mailboxPtr,
			SinceModseq: 0,
			Limit:       0,
		})
		if err != nil {
			return nil, err
		}

		// The client-built slice answers for what it covers; only the rest is
		// decrypted (see bodySearch). Nil session — no snapshot,
		// a rail that failed at AUTH — simply means no index verdicts,
		// and the search below is bit-for-bit what it was before this leg
		// existed. This process never stages or publishes: the MDA build half
		// is retired (`content-index.md` § Where the index is built, the
		// 2026-08-10 carrier ruling).
		s.mu.Lock()
		indexSession := s.indexSession
		s.mu.Unlock()
		answer := s.indexAnswerer(indexSession)

		bodyUIDs, err := bodySearch(opener, segReply.Segments, part.bodyTerms, uidByMid, answer)
		if err != nil {
			return nil, err
		}

		if wantsFlag {
			// Intersect flagUIDs ∩ bodyUIDs (AND across axes).
			flagSet := make(map[uint32]struct{}, len(flagUIDs))
			for _, u := range flagUIDs {
				flagSet[u] = struct{}{}
			}
			for _, u := range bodyUIDs {
				if _, in := flagSet[u]; in {
					finalUIDs = append(finalUIDs, u)
				}
			}
		} else {
			finalUIDs = bodyUIDs
		}
	} else {
		finalUIDs = flagUIDs
	}

	sort.Slice(finalUIDs, func(i, j int) bool { return finalUIDs[i] < finalUIDs[j] })

	switch numKind {
	case imapserver.NumKindUID:
		set := imap.UIDSet{}
		for _, u := range finalUIDs {
			set.AddNum(imap.UID(u))
		}
		return &imap.SearchData{All: set}, nil
	default:
		// SeqNum path — Phase C maps each UID to its 1-indexed position
		// in the ascending UID list for the selected mailbox. Cheap +
		// matches Dovecot parity until the per-mailbox snapshot model
		// arrives in Phase D.
		metaRows, err := wsrpc.FetchMessageMetadata(ctx, client, actorID, mailbox, nil)
		if err != nil {
			return nil, err
		}
		seqByUID := make(map[uint32]uint32, len(metaRows))
		// metaRows are returned in ascending UID order by the handler.
		for i, m := range metaRows {
			seqByUID[m.UID] = uint32(i + 1)
		}
		set := imap.SeqSet{}
		for _, u := range finalUIDs {
			if seq, ok := seqByUID[u]; ok {
				set.AddNum(seq)
			}
		}
		return &imap.SearchData{All: set}, nil
	}
}
