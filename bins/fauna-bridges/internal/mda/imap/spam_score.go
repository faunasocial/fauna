package imap

import (
	"context"
	"sort"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// spamScoredKeyword is the internal IMAP keyword the SELECT-time scoring
// pass stamps on each INBOX message it has scored, so a later SELECT
// skips it instead of re-fetching + re-decrypting + re-scoring every
// message on every poll (mail-spam.md § Re-file timing — the per-message
// "scored" watermark). It is nest-persisted in the `bridge_imap_messages`
// flags column via store_flags (the same channel \Seen / \Junk ride) and
// carries verbatim on COPY / MOVE (RFC 9051 §6.4.7), so a message a user
// moves back out of Junk into INBOX stays scored and is not re-Junked.
// The `$`-prefix marks it a system keyword (RFC 9051 §2.3.2). It is
// visible to MUAs like any keyword; should that visibility ever matter a
// future track MAY migrate the watermark to an invisible per-message nest
// column — the scoring logic is unchanged and a keyword is not
// user-irrecoverable data.
const spamScoredKeyword = "$FaunaSpamScored"

// spamScoreMilliScale converts a 0–15 spam-tier threshold to the
// milli-units the combined score is carried in (dag-cbor forbids floats).
// Mirrors `milli()` in libs/fauna-mail/src/spam/mod.rs — the scale
// `decide_spam_disposition`'s `reached()` compares the combined score
// against.
const spamScoreMilliScale = 1000

// spamScoreRPCTimeout bounds the whole SELECT-time scoring pass. The pass
// is best-effort: on timeout it logs and degrades to "not scored this
// SELECT", never failing the SELECT — the message is scored on a later
// SELECT instead.
const spamScoreRPCTimeout = 30 * time.Second

// spamScorePerPassCap bounds how many un-watermarked INBOX messages a
// single SELECT-time pass decrypts + scores, so a cold-start SELECT of a
// large un-scored INBOX (e.g. a just-migrated mailbox) can't blow the
// Session.Select budget (spamScoreRPCTimeout is only the backstop). The
// most-recent (highest-UID) cap'd messages are scored this pass — the user
// cares most about their newest mail being filed correctly — and the rest
// are deferred to the next read-write SELECT INBOX. The watermark makes the
// scan resumable, so this is just eventual consistency: it is already the
// model (each SELECT picks up where the last left off), the cap only bounds
// the one-time cold-start backlog drain. Steady-state it never bites (only
// the few newly-delivered un-watermarked messages are scored per SELECT).
//
// Not a user/admin choice — it is pure internal latency tuning, never a
// preference anyone would express — so it
// is a hard-coded constant, not a client knob. The exact value (and the
// cap-vs-async tradeoff) is a pre-GA tuning question; this conservative
// default keeps a cold-start SELECT snappy while draining in a handful of
// SELECTs.
const spamScorePerPassCap = 100

// scoreSelectedInbox is the post-delivery, AUTH'd-MDA-session per-user
// spam scoring pass (mail-spam.md § Scoring placement, § Encrypted-mode
// interaction). Run from Session.Select on a read-write SELECT of INBOX,
// BEFORE the select_mailbox snapshot, so a message it re-files to Junk is
// already gone from the SELECT response's EXISTS / UID state — exactly as
// if it had been filed there at delivery.
//
// It scores at the search-equivalent position: it fetches the actor's
// per-user model **sealed to them** (fetch_spam_model), unwraps it under
// the session's per-connection record opener (`opener.Open` — a STRICT
// open: the model is sealed-on-read by nest in BOTH storage modes, never
// raw; the sealed model is the identical `MailRecordEnvelope` shape as a
// mail body), then for each un-scored INBOX message opens the body
// through the uniform serve rule (`mailfauna.OpenStoredRecord` — sealed
// opens, an unsealed payload is refused), runs the shared scorer, and
// moves the message to Junk iff its combined score crosses the
// admin-effective `spam_folder` threshold. Per-user scoring only ever
// routes INBOX↔Junk (mail-spam.md § Scoring placement); reject is a
// deployment-wide rspamd decision taken once at ingest, never re-decided
// here post-delivery.
//
// Works identically in BOTH storage modes: no mode branch anywhere —
// both the body rail and the model rail are strict-sealed opens
// (Phase-3 D1/D5).
//
// Best-effort throughout: any RPC / decrypt failure logs and skips that
// message (or returns), never failing the SELECT. The auto-move calls
// wsrpc.Move directly (not Session.move), so it deliberately does NOT fire
// the \Junk training signal — the scorer's own disposition must not feed
// back into the model it scored with.
func (s *Session) scoreSelectedInbox(ctx context.Context, opener mailfauna.RecordOpener) {
	s.mu.Lock()
	actorID := s.actorID
	client := s.client
	policy := s.spamPolicy
	s.mu.Unlock()
	if actorID == nil || client == nil {
		return
	}
	// No record opener (no MLS snapshot provisioned yet) ⇒ we can open
	// neither the sealed model nor any sealed bodies; nothing to score.
	if opener == nil {
		return
	}
	// NOTE — there is deliberately NO pass-level `SpamFolderThreshold == 0`
	// exit here any more. It was correct only while the threshold was a
	// session-wide number; under the delivery-time fold (mail-aliases.md
	// § Spam-threshold override) a message carries its OWN threshold, so a
	// session-level exit would skip exactly the messages a user set an
	// override for — the goal doc names this trap explicitly. The per-message
	// value is the one consulted, with the session policy as its fallback;
	// the disabled case is decided per message, below. The cheap cold-start
	// exit that actually bounds this pass is the untrained-model check in
	// step 1 — an actor with no model is still one RPC, not an INBOX scan.

	rpcCtx, cancel := context.WithTimeout(ctx, spamScoreRPCTimeout)
	defer cancel()

	// 1. Fetch + unwrap the per-user model first — the cheap exit for the
	//    cold-start / untrained actor (one DB lookup returning None, no
	//    INBOX scan or body decrypts).
	// stored_sealed is the TRAIN path's dispatch signal; scoring opens
	// the returned envelope identically either way — discard it here.
	// contribute_baseline/holder_seal_target are the piece-(b4) TRAIN-path
	// write signal; scoring ignores both.
	sealed, _, baseline, _, _, err := wsrpc.FetchSpamModel(rpcCtx, client, actorID)
	if err != nil {
		if s.logger != nil {
			s.logger.Warn("imap: spam-score: fetch_spam_model failed; INBOX not scored this SELECT", "err", err)
		}
		return
	}
	if len(sealed) == 0 {
		return // untrained ⇒ cold start ⇒ nothing to score
	}
	// STRICT open — the model is sealed-on-read by nest in BOTH modes,
	// never raw (a raw model here means corruption); the same strict open
	// every mail record gets.
	modelBytes, err := opener.Open(sealed)
	if err != nil {
		if s.logger != nil {
			s.logger.Warn("imap: spam-score: model unwrap failed; INBOX not scored this SELECT", "err", err)
		}
		return
	}
	defer zeroize(modelBytes)
	// The published deployment baseline rides the reply only for a
	// stored (sealed) model — fold it into the just-unwrapped model
	// HERE, the agent leg of the read-time faded prior (the cold-start
	// seed was folded nest-side on read and carries no baseline — the
	// no-double-fold rule, mail-spam.md § Encrypted-mode interaction).
	// Score-time only: the train path (store.go) never folds, so the fold
	// is never persisted into the user's model. The FFI returns a fresh
	// slice either way; a 0 confidence horizon (uninitialized knobs under
	// an injected test scoreFn) is a guarded no-op inside the fold.
	if len(baseline) > 0 {
		folded := mailfauna.FoldSpamModelBaseline(modelBytes, baseline,
			s.bayesianKnobs.FullConfidenceSamples)
		defer zeroize(folded)
		modelBytes = folded
	}

	// 2. List INBOX; keep the messages not yet watermarked.
	rows, err := wsrpc.FetchMessageMetadata(rpcCtx, client, actorID, "INBOX", nil)
	if err != nil {
		if s.logger != nil {
			s.logger.Warn("imap: spam-score: INBOX metadata fetch failed", "err", err)
		}
		return
	}

	// The per-message scorer. Production uses the shared cgo FFI scorer
	// with the admin-effective knobs; tests inject a deterministic seam
	// (scoreFn) so the pass logic (watermark + re-file) is exercised without
	// the FFI.
	score := s.scoreFn
	if score == nil {
		// The Tier-2 `mail.spam.bayesian_*` knobs snapshotted from the Backend
		// at NewSession (mail-policy-config.md § Spam), not the hard-coded
		// defaults — so an admin's `put_spam_policy` weight / confidence-ramp
		// override reaches the per-user scorer.
		knobs := s.bayesianKnobs
		score = func(model []byte, text string) int32 {
			return mailfauna.WeightedBayesianMilliForModel(model, text, knobs)
		}
	}
	// The session-policy fallback: what a message that carries no delivery
	// stamp is measured against (unstamped messages arrive live via APPEND and
	// import). A stamped message overrides it per message, below.
	sessionSpamFolderMilli := int32(policy.SpamFolderThreshold) * spamScoreMilliScale

	// Collect the un-watermarked messages, then cap the per-pass count so a
	// cold-start large INBOX can't blow the Select budget.
	// The most-recent (highest-UID) messages are scored this pass; the rest
	// are deferred to the next read-write SELECT, where the watermark lets the
	// scan resume — eventual consistency, already the model.
	pending := make([]wsrpc.MessageMeta, 0, len(rows))
	for _, m := range rows {
		if hasFlag(m.Flags, spamScoredKeyword) {
			continue
		}
		pending = append(pending, m)
	}
	deferred := 0
	if len(pending) > spamScorePerPassCap {
		sort.Slice(pending, func(i, j int) bool { return pending[i].UID > pending[j].UID })
		deferred = len(pending) - spamScorePerPassCap
		pending = pending[:spamScorePerPassCap]
	}

	var scoredUIDs, spamUIDs []uint32
	for _, m := range pending {
		ct, err := wsrpc.FetchMessageCiphertext(rpcCtx, client, actorID, m.MessageID)
		if err != nil {
			if s.logger != nil {
				s.logger.Warn("imap: spam-score: ciphertext fetch failed", "uid", m.UID, "err", err)
			}
			continue
		}
		// An oversized body arrives by reference off the bulk-byte plane
		// (body_ref.go), not inline in the fetch reply.
		sealed, err := SealedBodyOf(rpcCtx, s.plane, ct)
		if err != nil {
			if s.logger != nil {
				s.logger.Warn("imap: spam-score: body-reference fetch failed", "uid", m.UID, "err", err)
			}
			continue
		}
		// Uniform serve rule (Phase-3 D1): the sealed body opens via the
		// session opener; an unsealed body is refused, never scored.
		// OpenStoredRecordAt so the epoch-aware session opener can try
		// the record's own candidate epoch keys (content-sealing-epochs
		// design § 4, Track 1b) — classified off the record's seal
		// instant, same basis as FETCH and the rescore drain.
		pt, err := mailfauna.OpenStoredRecordAt(opener, sealed, ct.SealEpochBasisUnix())
		if err != nil {
			if s.logger != nil {
				s.logger.Warn("imap: spam-score: body decrypt failed", "uid", m.UID, "err", err)
			}
			continue
		}
		// The same text shape the \Junk-train path ships (the full
		// decrypted message); the shared canonical tokenizer is identical
		// at train + score, so the per-user score agrees across positions
		// (mail-spam.md § Scoring placement — "byte-identical scores at
		// every position").
		weighted := score(modelBytes, string(pt))
		// THIS message's spam-folder tier: the delivery stamp nest folded from
		// per-alias > per-account > admin default at RCPT (mail-aliases.md
		// § Spam-threshold override), or the session policy when the message
		// carries no stamp. Read before `pt` is zeroized.
		spamFolderMilli := sessionSpamFolderMilli
		if stamped := mailfauna.ReadSpamThresholdStamp(pt); stamped != nil {
			spamFolderMilli = int32(*stamped) * spamScoreMilliScale
		}
		zeroize(pt)
		// rspamd term is 0 here: rspamd already scored at ingest and routed
		// to Junk anything that crossed a tier (so it isn't in INBOX). The
		// MDA pass adds the per-user Bayesian term the ingest path
		// hard-codes to 0 (mta/server.go `CombinedSpamScoreMilli(rspamd, 0)`).
		combined := mailfauna.CombinedSpamScoreMilli(0, weighted)
		scoredUIDs = append(scoredUIDs, m.UID)
		// A 0 threshold means auto-Junk routing is off for this message — the
		// same `reached()` semantics the shared scorer uses, now decided per
		// message. It still counts as scored (watermarked once, never
		// re-decrypted); it is simply never re-filed.
		if spamFolderMilli > 0 && combined >= spamFolderMilli {
			spamUIDs = append(spamUIDs, m.UID)
		}
	}

	// Surface the truncation so a deferred backlog isn't silent (the tail is
	// scored on a subsequent SELECT). Steady-state deferred is always 0.
	if deferred > 0 && s.logger != nil {
		s.logger.Info("imap: spam-score: per-pass cap reached; deferred the older tail to the next SELECT",
			"scored", len(scoredUIDs), "deferred", deferred, "cap", spamScorePerPassCap)
	}

	if len(scoredUIDs) == 0 {
		return
	}

	// 3. Watermark every scored message BEFORE moving the spam ones, so a
	//    failed move leaves a scored-but-still-INBOX message (no infinite
	//    re-score loop) rather than an unmarked one. The keyword carries
	//    onto the Junk copy on MOVE, which is harmless.
	if _, err := wsrpc.StoreFlags(rpcCtx, client, wsrpc.StoreFlagsParams{
		ActorID: actorID,
		Mailbox: "INBOX",
		UIDs:    scoredUIDs,
		Op:      wsrpc.StoreFlagsOpAdd,
		Flags:   []string{spamScoredKeyword},
	}); err != nil {
		if s.logger != nil {
			s.logger.Warn("imap: spam-score: watermark store_flags failed", "err", err)
		}
	}

	// 4. Re-file the spam to Junk. Direct wsrpc.Move (not Session.move) —
	//    no MUA response writer, and deliberately no \Junk-train signal.
	if len(spamUIDs) > 0 {
		if _, err := wsrpc.Move(rpcCtx, client, wsrpc.MoveParams{
			ActorID:       actorID,
			SourceMailbox: "INBOX",
			UIDs:          spamUIDs,
			DestMailbox:   "Junk",
		}); err != nil {
			if s.logger != nil {
				s.logger.Warn("imap: spam-score: move to Junk failed", "spam_count", len(spamUIDs), "err", err)
			}
		} else if s.logger != nil {
			s.logger.Info("imap: spam-score: re-filed messages to Junk",
				"spam_count", len(spamUIDs), "scored_count", len(scoredUIDs))
		}
	}
}
