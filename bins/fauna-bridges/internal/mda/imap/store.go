package imap

import (
	"context"
	"errors"
	"fmt"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/imapserver"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

const storeRPCTimeout = 30 * time.Second

// sealSpamModelCopy is the package-level seal function trainJunkAgentSide
// uses to attach a deployment-baseline holder copy (piece b4). Indirected
// through a var — the same seam `sealToRecipient` (append.go) uses — so
// tests can stub it without exercising the real FFI/X25519 material.
var sealSpamModelCopy = mailfauna.SealSpamModelCopy

// Store implements imapserver.SessionIMAP4rev2.Store. The wire flow:
//
//  1. Resolve `numSet` to an explicit UID list. UIDSet without dynamic
//     ranges is taken at face value; SeqSet (and dynamic UIDSet "*")
//     goes through a fetch_message_metadata round-trip so the bridge
//     can enumerate UIDs in sequence order.
//  2. Call `fauna.bridges.store_flags` with the resolved UIDs +
//     translated Op + flags + optional `unchanged_since`.
//  3. For every entry in `reply.Updated`, emit a FETCH response via the
//     FetchWriter (UID + new FLAGS). Silent STORE skips the response.
//  4. For STORE +FLAGS (\Junk) / STORE -FLAGS (\Junk), fire the
//     per-UID agent-side training signal (fireJunkTrainSignals →
//     sealed `put_spam_model`; best-effort: body-fetch
//     or decrypt failures log and skip, per mail-spam.md
//     § Training signal sources).
//  5. If `reply.Modified` is non-empty (RFC 7162 §3.1.3 CONDSTORE
//     precondition rejected some UIDs), return a typed `imap.Error`
//     with Type=OK and Code="MODIFIED" so emersion writes the OK
//     status response with the MODIFIED code per the goal-doc contract
//     (`imap-server.md` § CONDSTORE UNCHANGEDSINCE).
//
// Emersion's beta.8 STORE dispatcher does not yet parse the
// `UNCHANGEDSINCE` IMAP modifier on the wire (handleStore always
// passes a zero-valued StoreOptions). That's an upstream gap
// (`imap-server.md` § Upstream-blocked gaps) — the MDA-side handling
// is ready for the day it lands.
func (s *Session) Store(w *imapserver.FetchWriter, numSet imap.NumSet,
	flags *imap.StoreFlags, options *imap.StoreOptions,
) error {
	s.mu.Lock()
	var opener mailfauna.RecordOpener
	if s.recordOpener != nil {
		opener = s.recordOpener
	}
	s.mu.Unlock()
	return s.store(context.Background(), &fetchWriterAdapter{w: w}, numSet, flags, options, opener)
}

// store is the seam-friendly inner method tested in store_test.go.
// All FetchWriter contact happens through the fetchWriter interface
// (declared in fetch.go); the RecordOpener seam is the same one Fetch
// uses, so the \Junk train hook can swap in a stub.
func (s *Session) store(
	ctx context.Context,
	w fetchWriter,
	numSet imap.NumSet,
	flags *imap.StoreFlags,
	options *imap.StoreOptions,
	opener mailfauna.RecordOpener,
) error {
	s.mu.Lock()
	actorID := s.actorID
	mailbox := s.selectedMailbox
	client := s.client
	s.mu.Unlock()
	if actorID == nil {
		return errors.New("imap: STORE requires authenticated state")
	}
	if mailbox == "" {
		return errors.New("imap: STORE requires a SELECTed mailbox")
	}
	if flags == nil {
		return errors.New("imap: STORE requires *imap.StoreFlags")
	}

	rpcCtx, cancel := context.WithTimeout(ctx, storeRPCTimeout)
	defer cancel()

	// 1. Resolve numSet → ordered (seqNum, UID) pairs.
	pairs, err := resolveStoreNumSet(rpcCtx, client, actorID, mailbox, numSet)
	if err != nil {
		return err
	}
	if len(pairs) == 0 {
		return nil // nothing matches — silently succeed.
	}
	uids := make([]uint32, len(pairs))
	for i, p := range pairs {
		uids[i] = p.uid
	}

	// 2. Translate flags + op + UnchangedSince to the wsrpc shape.
	op, err := translateStoreOp(flags.Op)
	if err != nil {
		return err
	}
	flagStrs := make([]string, len(flags.Flags))
	for i, f := range flags.Flags {
		flagStrs[i] = string(f)
	}
	params := wsrpc.StoreFlagsParams{
		ActorID: actorID,
		Mailbox: mailbox,
		UIDs:    uids,
		Op:      op,
		Flags:   flagStrs,
	}
	if options != nil && options.UnchangedSince != 0 {
		n := int64(options.UnchangedSince)
		params.UnchangedSince = &n
	}

	reply, err := wsrpc.StoreFlags(rpcCtx, client, params)
	if err != nil {
		return err
	}

	// 3. Per-UID FETCH responses (skipped under STORE.SILENT).
	if !flags.Silent {
		// CONDSTORE: STORE FLAGS responses carry the new per-UID MODSEQ
		// when CONDSTORE is active (RFC 7162 §3.1.4; imap-server.md:152).
		condStore := s.condStoreActive()
		seqByUID := make(map[uint32]uint32, len(pairs))
		for _, p := range pairs {
			seqByUID[p.uid] = p.seqNum
		}
		for _, e := range reply.Updated {
			seqNum, ok := seqByUID[e.UID]
			if !ok {
				// Race window — the row was expunged between metadata
				// fetch and STORE. Use the UID as a best-effort seqNum
				// (RFC 9051 §6.4.6 allows silent skipping but we still
				// want to surface the FETCH for clients that asked).
				seqNum = e.UID
			}
			mw := w.CreateMessage(seqNum)
			mw.WriteUID(imap.UID(e.UID))
			mw.WriteFlags(toIMAPFlags(e.Flags))
			if condStore {
				mw.WriteModSeq(uint64(e.ModSeq))
			}
			if err := mw.Close(); err != nil {
				return err
			}
		}
	}

	// 4. \Junk training signal: only on +\Junk / -\Junk, never on Set.
	if shouldFireJunkSignal(flags.Op, flags.Flags) {
		label := wsrpc.SpamLabelSpam
		if flags.Op == imap.StoreFlagsDel {
			label = wsrpc.SpamLabelHam
		}
		uids := make([]uint32, len(reply.Updated))
		for i, e := range reply.Updated {
			uids[i] = e.UID
		}
		s.fireJunkTrainSignals(rpcCtx, client, opener, actorID, mailbox, uids,
			label, wsrpc.TrainingSourceImapJunkFlag)
	}

	// 5. CONDSTORE MODIFIED partition (RFC 7162 §3.1.3). The stale UID set
	// MUST ride *inside* the response code — `[MODIFIED <uid-set>]` — so a
	// CONDSTORE-aware MUA can parse which messages failed the UNCHANGEDSINCE
	// precondition and refetch+retry them. Putting it only in the free text
	// is non-compliant (the client can't recover the set). writeStatusResp
	// renders `[%v]` over the Code verbatim, so embedding the set in the
	// Code string yields `OK [MODIFIED 7,9] Conditional STORE failed`.
	if len(reply.Modified) > 0 {
		return &imap.Error{
			Type: imap.StatusResponseTypeOK,
			Code: imap.ResponseCode("MODIFIED " + formatUIDSetString(reply.Modified)),
			Text: "Conditional STORE failed",
		}
	}
	return nil
}

// uidPair is an (seqNum, UID) tuple in sequence order.
type uidPair struct {
	seqNum uint32
	uid    uint32
}

// resolveStoreNumSet returns the (seqNum, UID) pairs that match the
// requested NumSet, in ascending seqNum order.
//
// For non-dynamic UIDSet the bridge can take the UIDs at face value
// and synthesize seqNums lazily (set seqNum = UID — clients usually
// don't care since UID STORE already carries the UID in the FETCH
// response). For SeqSet or dynamic UIDSet the bridge issues one
// fetch_message_metadata round-trip and filters locally — same shape
// as fetch.go.
func resolveStoreNumSet(
	ctx context.Context,
	client wsrpc.Caller,
	actorID []byte,
	mailbox string,
	numSet imap.NumSet,
) ([]uidPair, error) {
	if u, ok := numSet.(imap.UIDSet); ok && !u.Dynamic() {
		if nums, ok := u.Nums(); ok {
			out := make([]uidPair, 0, len(nums))
			for _, n := range nums {
				out = append(out, uidPair{seqNum: uint32(n), uid: uint32(n)})
			}
			return out, nil
		}
	}

	rows, err := wsrpc.FetchMessageMetadata(ctx, client, actorID, mailbox, nil)
	if err != nil {
		return nil, err
	}
	sort.Slice(rows, func(i, j int) bool { return rows[i].UID < rows[j].UID })
	out := make([]uidPair, 0, len(rows))
	for i, m := range rows {
		seqNum := uint32(i + 1)
		if !numSetMatches(numSet, seqNum, imap.UID(m.UID)) {
			continue
		}
		out = append(out, uidPair{seqNum: seqNum, uid: m.UID})
	}
	return out, nil
}

// translateStoreOp maps emersion's StoreFlagsOp constant to the
// wsrpc-layer enum string.
func translateStoreOp(op imap.StoreFlagsOp) (wsrpc.StoreFlagsOp, error) {
	switch op {
	case imap.StoreFlagsSet:
		return wsrpc.StoreFlagsOpSet, nil
	case imap.StoreFlagsAdd:
		return wsrpc.StoreFlagsOpAdd, nil
	case imap.StoreFlagsDel:
		return wsrpc.StoreFlagsOpRemove, nil
	default:
		return "", fmt.Errorf("imap: unknown StoreFlagsOp %d", int(op))
	}
}

// shouldFireJunkSignal reports whether a STORE op should trigger a
// training signal (fireJunkTrainSignals) per `mail-spam.md` § Training signal
// sources. Only +\Junk (Add) and -\Junk (Del) are explicit triggers;
// SET is not in the contract (and would require a pre-state read to
// know which way the flag flipped).
func shouldFireJunkSignal(op imap.StoreFlagsOp, flags []imap.Flag) bool {
	if op != imap.StoreFlagsAdd && op != imap.StoreFlagsDel {
		return false
	}
	for _, f := range flags {
		if string(f) == "\\Junk" {
			return true
		}
	}
	return false
}

// fireJunkTrainSignals turns a \Junk flag/move user-action into
// per-user model training, one event per UID in `uids` (message_id
// looked up via a per-UID metadata fetch from `mailbox`). There is ONE
// path: the model rests sealed to the actor's own recipient key and the
// nest can neither read nor train it, so the MDA — a capability holder on
// the actor's AUTH'd session — trains agent-side (trainJunkAgentSide;
// mail-spam.md § Encrypted-mode interaction). The starting model is
// picked off `fetch_spam_model`:
//
//   - fetch error → log and skip the whole signal (best-effort; there is
//     no server-side train to fall back to).
//   - `stored_sealed` with a blob → open that stored model and train it.
//   - otherwise — no stored model, or the cold-start seed the nest
//     seals-on-read from the deployment baseline (`stored_sealed == false`)
//     → train from an EMPTY model. The seed is never trained on: the
//     baseline fold is read-time only and must never be persisted into the
//     user's model (mail-spam.md § Cold start Path 2).
//
// The baseline return is deliberately DISCARDED here for the same reason.
// Body fetch + decrypt failures log and skip (best-effort signal — the
// user-action already fired on the IMAP wire and waiting on a body fetch
// would block the IMAP response).
//
// Used by STORE (+/-\Junk → source=imap_junk_flag, mailbox = SELECTed)
// and COPY/MOVE in or out of a \Junk-marked mailbox
// (source=imap_junk_move, mailbox = dest because that's where the row
// lives after the operation).
func (s *Session) fireJunkTrainSignals(
	ctx context.Context,
	client wsrpc.Caller,
	opener mailfauna.RecordOpener,
	actorID []byte,
	mailbox string,
	uids []uint32,
	label wsrpc.SpamLabel,
	source wsrpc.TrainingSource,
) {
	// contributeBaseline + holderSealTarget are piece (b4): when both are
	// present, every re-seal in trainJunkAgentSide also attaches a fresh
	// holder copy (mail-spam.md § Encrypted-mode interaction, the
	// re-seal-on-every-write rule).
	fetched, storedSealed, _, contributeBaseline, holderSealTarget, err := wsrpc.FetchSpamModel(ctx, client, actorID)
	if err != nil {
		if s.logger != nil {
			s.logger.Warn("imap: \\Junk train: fetch_spam_model failed — signal skipped",
				"label", string(label), "uid_count", len(uids), "err", err)
		}
		return
	}
	var sealedModel []byte
	if storedSealed && len(fetched) > 0 {
		sealedModel = fetched
	}
	s.trainJunkAgentSide(ctx, client, opener, actorID,
		mailbox, uids, label, source, sealedModel, contributeBaseline, holderSealTarget)
}

// trainJunkAgentSide runs the whole open → mutate → re-seal → write-back
// loop for fireJunkTrainSignals. `sealedModel` is the actor's stored
// sealed model, or nil to train from an empty model (an untrained actor,
// or one the nest only serves the cold-start seed for). Per UID: fetch +
// open the body under the session opener, apply the training delta via
// the shared `apply_spam_training` FFI (one shared `SpamModel`, byte-
// identical with the client train; empty model bytes train a fresh
// model), re-seal the mutated model to the actor's OWN recipient key
// (`sealToRecipient`, the same suite-deciding hybrid seal APPEND uses),
// seal the subject + forward delta for the audit row, and ship model +
// row in ONE atomic `put_spam_model`. The mutated model carries forward
// across UIDs so a multi-message \Junk move trains cumulatively; a failed
// event skips (best-effort, the model stays at the last successful
// write).
func (s *Session) trainJunkAgentSide(
	ctx context.Context,
	client wsrpc.Caller,
	opener mailfauna.RecordOpener,
	actorID []byte,
	mailbox string,
	uids []uint32,
	label wsrpc.SpamLabel,
	source wsrpc.TrainingSource,
	sealedModel []byte,
	contributeBaseline bool,
	holderSealTarget *wsrpc.HolderSealTarget,
) {
	// The session's record opener opens every body (and the stored model,
	// when there is one) — no opener, nothing can be trained.
	if opener == nil {
		if s.logger != nil {
			s.logger.Warn("imap: \\Junk train: no MLS snapshot is open — skipping",
				"label", string(label), "uid_count", len(uids))
		}
		return
	}
	s.mu.Lock()
	mlsPubkey := s.actorMLSPubkey
	mlkemEk := s.actorMlkemEk
	s.mu.Unlock()
	if mlsPubkey == nil {
		if s.logger != nil {
			s.logger.Warn("imap: \\Junk train: no recipient pubkey cached for the re-seal — skipping")
		}
		return
	}
	// nil ⇒ empty model bytes ⇒ ApplySpamTraining trains a fresh model.
	var modelBytes []byte
	if sealedModel != nil {
		// STRICT open — the same strict open every mail record gets: a raw
		// blob where a sealed model is stored means corruption.
		opened, err := opener.Open(sealedModel)
		if err != nil {
			if s.logger != nil {
				s.logger.Warn("imap: \\Junk train: sealed model unwrap failed — skipping", "err", err)
			}
			return
		}
		modelBytes = opened
	}
	defer func() { zeroize(modelBytes) }()
	isSpam := label == wsrpc.SpamLabelSpam
	for _, uid := range uids {
		meta, err := wsrpc.FetchMessageMetadata(ctx, client, actorID, mailbox, []uint32{uid})
		if err != nil || len(meta) == 0 {
			if s.logger != nil {
				s.logger.Warn("imap: \\Junk train: metadata fetch failed", "uid", uid, "err", err)
			}
			continue
		}
		ct, err := wsrpc.FetchMessageCiphertext(ctx, client, actorID, meta[0].MessageID)
		if err != nil {
			if s.logger != nil {
				s.logger.Warn("imap: \\Junk train: ciphertext fetch failed", "uid", uid, "err", err)
			}
			continue
		}
		// An oversized body arrives by reference off the bulk-byte plane
		// (body_ref.go), not inline in the fetch reply.
		sealed, err := SealedBodyOf(ctx, s.plane, ct)
		if err != nil {
			if s.logger != nil {
				s.logger.Warn("imap: \\Junk train: body-reference fetch failed", "uid", uid, "err", err)
			}
			continue
		}
		// Uniform serve rule (Phase-3 D1): the sealed body opens via the
		// session opener; an unsealed body is refused, never trained.
		// OpenStoredRecordAt so the epoch-aware session opener can try
		// the record's own candidate epoch keys (content-sealing-epochs
		// design § 4, Track 1b) — classified off the record's seal
		// instant, same basis as FETCH and the rescore drain.
		pt, err := mailfauna.OpenStoredRecordAt(opener, sealed, ct.SealEpochBasisUnix())
		if err != nil {
			if s.logger != nil {
				s.logger.Warn("imap: \\Junk train: body decrypt failed", "uid", uid, "err", err)
			}
			continue
		}
		// The full message is the training text (the shared canonical
		// tokenizer is identical at every position — mail-spam.md
		// § "byte-identical scores").
		mutation := mailfauna.ApplySpamTraining(modelBytes, string(pt), isSpam)
		// The audit row's subject, RFC-2047-decoded; a parse failure
		// degrades to the empty subject (the row still renders its
		// mailbox). Sealed even when empty — the ciphertext is never
		// empty, and the nest refuses a history insert whose
		// `sealed_subject` is empty.
		subject := ""
		if parsed, perr := mailfauna.ParseRFC5322(pt); perr == nil {
			subject = parsed.Subject
		}
		zeroize(pt)
		resealedModel, errM := sealToRecipient(mutation.NewModelBytes, mlsPubkey, mlkemEk)
		sealedSubject, errS := sealToRecipient([]byte(subject), mlsPubkey, mlkemEk)
		sealedDelta, errD := sealToRecipient(mutation.DeltaJson, mlsPubkey, mlkemEk)
		// The plaintext forward-delta n-grams are fully consumed by the seal
		// above; wipe them here so they don't linger to GC on any exit path
		// . The
		// decrypted body copies into immutable Go strings the runtime won't let
		// us zeroize — `string(pt)` into the ApplySpamTraining FFI and the parsed
		// `subject` — an accepted FFI-string limitation (the []byte `pt` itself
		// was already zeroized above; NewModelBytes is wiped per exit path below,
		// or carried forward + wiped on the next iteration).
		zeroize(mutation.DeltaJson)
		if errM != nil || errS != nil || errD != nil {
			if s.logger != nil {
				s.logger.Warn("imap: \\Junk train: re-seal failed — event skipped",
					"uid", uid, "model_err", errM, "subject_err", errS, "delta_err", errD)
			}
			zeroize(mutation.NewModelBytes)
			continue
		}
		// Piece (b4): opted-in + a holder enrolled ⇒ attach a fresh
		// holder copy sealed from the SAME post-mutation plaintext (the
		// re-seal-on-every-write rule, mail-spam.md § Encrypted-mode
		// interaction). A seal failure is non-fatal — it just means this
		// write leaves the previously-stored copy (if any) stale for the
		// next publish, never drops the contributor or the model write
		// itself (freshness ground 5).
		var holderCopy *wsrpc.SpamModelHolderCopy
		if contributeBaseline && holderSealTarget != nil {
			copyBytes, errC := sealSpamModelCopy(mutation.NewModelBytes, actorID,
				holderSealTarget.X25519Pubkey, holderSealTarget.MLKemEk)
			if errC != nil {
				if s.logger != nil {
					s.logger.Warn("imap: \\Junk train: holder-copy seal failed — attaching none this write",
						"uid", uid, "err", errC)
				}
			} else {
				holderCopy = &wsrpc.SpamModelHolderCopy{
					HolderPubkey: holderSealTarget.X25519Pubkey,
					SealedCopy:   copyBytes,
				}
			}
		}
		// SampleCount 0: advisory display-only and the mutation carries no
		// count; the nest never trusts it against the opaque blob anyway.
		outcome, err := wsrpc.PutSpamModel(ctx, client, actorID, resealedModel, 0,
			&wsrpc.SpamHistoryInsert{
				MessageID:     meta[0].MessageID,
				Mailbox:       mailbox,
				SealedSubject: sealedSubject,
				SealedDelta:   sealedDelta,
				Label:         label,
				Source:        source,
			}, holderCopy)
		if err != nil {
			if s.logger != nil {
				s.logger.Warn("imap: \\Junk train: put_spam_model failed — event skipped",
					"uid", uid, "err", err)
			}
			zeroize(mutation.NewModelBytes)
			continue
		}
		if outcome == wsrpc.PutOutcomeDuplicateSignal {
			// The one-lesson rule (mail-spam.md § 3): the nest kept the
			// stored model byte-identical to the last accepted write, so this
			// mutation must not carry into the next UID either — the batch
			// keeps training from the last ACCEPTED model.
			if s.logger != nil {
				s.logger.Info("imap: \\Junk train: duplicate of the newest lesson — collapsed",
					"uid", uid)
			}
			zeroize(mutation.NewModelBytes)
			continue
		}
		// Carry the mutated model into the next event so a batch trains
		// cumulatively; wipe the superseded plaintext first.
		zeroize(modelBytes)
		modelBytes = mutation.NewModelBytes
	}
}

// formatUIDSetString renders a UID list as an IMAP set string ("1:3,5,7")
// suitable for the MODIFIED response code. Folds adjacent UIDs into
// ranges so an MUA can parse it like any other UID-set.
func formatUIDSetString(uids []uint32) string {
	if len(uids) == 0 {
		return ""
	}
	sorted := make([]uint32, len(uids))
	copy(sorted, uids)
	sort.Slice(sorted, func(i, j int) bool { return sorted[i] < sorted[j] })

	var parts []string
	start := sorted[0]
	prev := sorted[0]
	for i := 1; i < len(sorted); i++ {
		if sorted[i] == prev+1 {
			prev = sorted[i]
			continue
		}
		parts = append(parts, rangeString(start, prev))
		start = sorted[i]
		prev = sorted[i]
	}
	parts = append(parts, rangeString(start, prev))
	return strings.Join(parts, ",")
}

func rangeString(start, stop uint32) string {
	if start == stop {
		return strconv.FormatUint(uint64(start), 10)
	}
	return strconv.FormatUint(uint64(start), 10) + ":" + strconv.FormatUint(uint64(stop), 10)
}
