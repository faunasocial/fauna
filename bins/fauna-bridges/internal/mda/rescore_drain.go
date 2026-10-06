package mda

import (
	"context"
	"encoding/hex"
	"errors"
	"log/slog"
	"strings"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/byteplane"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/capability"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/imap"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/refreshsched"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/scan"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// The re-score drain WORKER (capability-mediated-content-processing design
// § 2.5 step 4; content-scoring.md § The scoring-metadata bus): when a model
// version bumps, content scored under the old version owes a re-score — the
// obligation is the version gap in the nest's content-free `content_scores`
// metadata. This worker drains it at the capability rendezvous: the MDA (the
// first content-processing-bridge instance) asks `rescore_worklist` what its
// granted owners owe, fetches each sealed record, transiently unseals it with
// the user-minted `content.read{mail}` grant key (NO AUTH session — the
// background case), re-runs the co-resident clamd/rspamd, and writes the
// re-computed rows back via `submit_scores` stamped at the current version.
// The nest never reads content (`encryption-at-rest.md` § Don't do these /
// KMH rule #4); the plaintext exists only transiently in this process and is
// wiped after scoring.
//
// Factor scope is clamav, rspamd, and the tier-3 community labelers
// (`labeler:<64-hex>`, serviced by rescoreUnit's TierCommunity branch): the
// auth_* factors need DNS + connection-time envelope state (client IP, HELO)
// that is not in the stored body, and the tier-1 spam factor is self-servicing
// at the AUTH'd session / client (imap/spam_score.go). A unit for any other
// factor is skipped — its obligation stays open for whichever holder can
// service it.

const (
	// drainInterval is the scheduled backstop cadence. The real triggers are
	// startup and every config_changed push (a nest-side mint/revoke — and a
	// reconnect after a nest deploy, which is exactly when a built-in model
	// version bumps — both poke the drain), so the backstop only catches a
	// missed push. Matches the holder's refresh backstop. Internal latency
	// tuning, not a human choice.
	drainInterval = 12 * time.Hour

	// drainWorklistLimit is the per-batch worklist size (≤ the server's 512
	// cap). A full batch means more remain; the run loops.
	drainWorklistLimit = 256

	// drainMaxBatchesPerRun bounds one run's fan-out (batches × limit units).
	// A larger backlog resumes on the next trigger/backstop — eventual
	// consistency, same model as the SELECT-time spam pass cap.
	drainMaxBatchesPerRun = 8

	// drainRPCTimeout bounds each nest RPC in a run (the scan dialers carry
	// their own scan.ScanTimeout()).
	drainRPCTimeout = 30 * time.Second
)

// rescoreDrainDeps carries what the drain worker needs. registry + caller +
// scanCfg + logger are the production inputs; the *Fn fields are test seams
// (production leaves them nil → the real wsrpc / FFI / scan implementations),
// mirroring capHolderDeps.
type rescoreDrainDeps struct {
	// registry is the MDA's capability holder (5B1). The drain Refreshes it
	// on-demand at the start of every run so an honest-box revoke bites at
	// use-time, then wields Current() grant keys per work-unit.
	registry *capability.Registry
	caller   wsrpc.Caller
	// plane is the bulk-byte-plane client a by-reference body is fetched
	// through — the production fetchFn resolves the reference before the
	// unit ever reaches rescoreUnit. nil where none is wired.
	plane *byteplane.Client
	// scanCfg carries the co-resident clamd/rspamd addresses + policy —
	// deployment topology main.go resolves for BOTH roles (the drain re-runs
	// the same scanners the MTA gates with).
	scanCfg scan.Config
	logger  *slog.Logger

	// Test seams — production leaves these nil.
	worklistFn func(ctx context.Context, limit uint32) ([]wsrpc.RescoreUnit, error)
	fetchFn    func(ctx context.Context, actorID, messageID []byte) (*wsrpc.FetchedCiphertext, error)
	openFn     func(envelope, key []byte) ([]byte, error)
	clamdFn    func(ctx context.Context, addr string, raw []byte) (string, error)
	rspamdFn   func(ctx context.Context, cfg scan.Config, raw []byte) (mailfauna.RspamdScore, error)
	submitFn   func(ctx context.Context, rows []wsrpc.SubmitScoreRow) (uint32, error)
	// inspectLabelerFn fetches a community-labeler's signed metadata + WASM
	// bytes (fauna.labelers.inspect) so the `labeler:`-factor branch can run it.
	inspectLabelerFn func(ctx context.Context, labelerID []byte) (metadataBlob, wasmBytes []byte, err error)
	// labelerScoreFn maps the unsealed mail to the BARE `label()` input and runs
	// the module over it, returning the per-mille tier-3 score. Bundles the
	// mail→input mapping + the verify(B1)/expected-id/clamp(F4)/run/decode FFI
	// into one seam; all crypto + limit-clamping is in shared Rust, so Go only
	// shuttles bytes. labelerID is the 32-byte id parsed from the unit's
	// factor — the labeler the owner's grant licenses — and the FFI refuses a
	// module signed under any other id.
	labelerScoreFn func(labelerID, metadataBlob, wasmBytes, rawRFC5322 []byte) (int64, error)
	nowFn          func() time.Time
	afterFn        func(time.Duration) <-chan time.Time
}

// rescoreDrain is the running worker: a poke-able background loop (startup +
// config_changed + the scheduled backstop) whose every wake runs runOnce.
type rescoreDrain struct {
	deps   rescoreDrainDeps
	pokeCh chan struct{}
	done   chan struct{}
}

// newRescoreDrain fills the production defaults for nil seams.
func newRescoreDrain(deps rescoreDrainDeps) *rescoreDrain {
	if deps.logger == nil {
		deps.logger = slog.Default()
	}
	if deps.worklistFn == nil {
		deps.worklistFn = func(ctx context.Context, limit uint32) ([]wsrpc.RescoreUnit, error) {
			return wsrpc.RescoreWorklist(ctx, deps.caller, limit)
		}
	}
	if deps.fetchFn == nil {
		deps.fetchFn = func(ctx context.Context, actorID, messageID []byte) (*wsrpc.FetchedCiphertext, error) {
			ct, err := wsrpc.FetchMessageCiphertext(ctx, deps.caller, actorID, messageID)
			if err != nil {
				return nil, err
			}
			// An oversized body arrives by reference off the bulk-byte plane
			// rather than inline. Resolve it HERE, at the fetch seam, so
			// everything downstream — the grant-key candidate selection and the
			// openFn unseal in rescoreUnit — reads ONE shape: past this point
			// `EncryptedBody` is the sealed bytes, already resolved, and
			// BodyRef is spent.
			sealed, err := imap.SealedBodyOf(ctx, deps.plane, ct)
			if err != nil {
				return nil, err
			}
			ct.EncryptedBody = sealed
			ct.BodyRef = nil
			return ct, nil
		}
	}
	if deps.openFn == nil {
		deps.openFn = mailfauna.OpenMailRecordWithKey
	}
	if deps.clamdFn == nil {
		deps.clamdFn = scan.Clamd
	}
	if deps.rspamdFn == nil {
		deps.rspamdFn = func(ctx context.Context, cfg scan.Config, raw []byte) (mailfauna.RspamdScore, error) {
			// Background re-score: no connection-time envelope (IP/From/Rcpt)
			// survives in the stored body — rspamd scores content rules only.
			return scan.Rspamd(ctx, cfg, raw, "", "", nil)
		}
	}
	if deps.submitFn == nil {
		deps.submitFn = func(ctx context.Context, rows []wsrpc.SubmitScoreRow) (uint32, error) {
			return wsrpc.SubmitScores(ctx, deps.caller, rows)
		}
	}
	if deps.inspectLabelerFn == nil {
		deps.inspectLabelerFn = func(ctx context.Context, labelerID []byte) ([]byte, []byte, error) {
			return wsrpc.InspectLabeler(ctx, deps.caller, labelerID)
		}
	}
	if deps.labelerScoreFn == nil {
		deps.labelerScoreFn = func(labelerID, metadataBlob, wasmBytes, rawRFC5322 []byte) (int64, error) {
			// mail → BARE `LabelerPostInput`, then verify(B1)+expected-id+
			// clamp(F4)+run+score in the shared FFI. Two calls, but Go does no
			// crypto/decode itself.
			inputBare, err := mailfauna.MailToLabelerInputBare(rawRFC5322)
			if err != nil {
				return 0, err
			}
			return mailfauna.RunWasmLabelerScore(metadataBlob, wasmBytes, labelerID, inputBare)
		}
	}
	if deps.nowFn == nil {
		deps.nowFn = time.Now
	}
	if deps.afterFn == nil {
		deps.afterFn = time.After
	}
	return &rescoreDrain{
		deps:   deps,
		pokeCh: make(chan struct{}, 1),
		done:   make(chan struct{}),
	}
}

// start spawns the drain loop: an immediate first run, then a run per poke
// (config_changed) or backstop tick. The loop exits on ctx cancel and closes
// done — mda.Run's teardown waits on it BEFORE Close()ing the holder, so no
// drain Refresh/KeyFor is in flight when the registry zeroizes.
func (d *rescoreDrain) start(ctx context.Context) {
	go func() {
		defer close(d.done)
		d.runOnce(ctx)
		retry := 0
		for {
			wait := refreshsched.NextDelay(retry, drainInterval, nil)
			select {
			case <-ctx.Done():
				return
			case <-d.afterOrBackstop(wait):
			case <-d.pokeCh:
			}
			if ctx.Err() != nil {
				return
			}
			d.runOnce(ctx)
			retry = 0
		}
	}()
}

func (d *rescoreDrain) afterOrBackstop(wait time.Duration) <-chan time.Time {
	return d.deps.afterFn(wait)
}

// poke asks the loop to drain at its next scheduling opportunity (coalesced).
func (d *rescoreDrain) poke() {
	select {
	case d.pokeCh <- struct{}{}:
	default:
	}
}

// unitKey dedupes work-units within one run so a unit that failed (scanner
// down, unsealable, no grant) is not re-fetched batch after batch — it stays
// owed on the nest and is retried on the NEXT run instead.
type unitKey struct {
	content string
	factor  string
}

// runOnce drains the holder's re-score obligation: refresh grants (use-time
// revoke), then loop worklist → per-unit fetch → unseal → re-scan → per-owner
// submit until the worklist is empty or every remaining unit was skipped.
// Best-effort throughout: any per-unit failure logs and skips that unit; a
// per-owner submit failure (e.g. no `content.label-write` grant — the nest is
// fail-closed per batch) skips that owner for the rest of the run.
func (d *rescoreDrain) runOnce(ctx context.Context) {
	// Use-time refresh: an honest-box revoke bites NOW, not at the 12 h
	// backstop (design § 2.3). A transport error keeps the cached
	// authoritative set — proceed with it, like the serve path does.
	refreshCtx, cancel := context.WithTimeout(ctx, drainRPCTimeout)
	if err := d.deps.registry.Refresh(refreshCtx); err != nil {
		d.deps.logger.Warn("rescore-drain: use-time grant refresh failed; draining on cached grants", "err", err)
	}
	cancel()

	seen := make(map[unitKey]bool)
	deniedOwners := make(map[string]bool)
	totalScored := 0

	for batch := 0; batch < drainMaxBatchesPerRun; batch++ {
		wlCtx, cancel := context.WithTimeout(ctx, drainRPCTimeout)
		units, err := d.deps.worklistFn(wlCtx, drainWorklistLimit)
		cancel()
		if err != nil {
			d.deps.logger.Warn("rescore-drain: worklist fetch failed", "err", err)
			return
		}
		if len(units) == 0 {
			break
		}

		// Per-owner rows: the nest rejects a submit batch fail-closed on any
		// row its holder may not label-write, so one owner's missing grant
		// must not sink another's write-back.
		type ownerBatch struct {
			owner []byte
			rows  map[string]*wsrpc.SubmitScoreRow // by content_id
			order []string
		}
		batches := make(map[string]*ownerBatch)
		var ownerOrder []string
		progressed := false

		for i := range units {
			u := &units[i]
			k := unitKey{content: string(u.ContentID), factor: u.Factor}
			if seen[k] || deniedOwners[string(u.OwnerActorID)] {
				continue
			}
			seen[k] = true
			progressed = true

			entry, ok := d.rescoreUnit(ctx, u)
			if !ok {
				continue
			}
			ownerStr := string(u.OwnerActorID)
			ob := batches[ownerStr]
			if ob == nil {
				ob = &ownerBatch{owner: u.OwnerActorID, rows: map[string]*wsrpc.SubmitScoreRow{}}
				batches[ownerStr] = ob
				ownerOrder = append(ownerOrder, ownerStr)
			}
			row := ob.rows[string(u.ContentID)]
			if row == nil {
				row = &wsrpc.SubmitScoreRow{
					ContentID:    u.ContentID,
					ContentKind:  u.ContentKind,
					OwnerActorID: u.OwnerActorID,
					ScoredAt:     uint64(d.deps.nowFn().Unix()),
				}
				ob.rows[string(u.ContentID)] = row
				ob.order = append(ob.order, string(u.ContentID))
			}
			row.Entries = append(row.Entries, entry)
		}

		for _, ownerStr := range ownerOrder {
			ob := batches[ownerStr]
			rows := make([]wsrpc.SubmitScoreRow, 0, len(ob.order))
			for _, cid := range ob.order {
				rows = append(rows, *ob.rows[cid])
			}
			subCtx, cancel := context.WithTimeout(ctx, drainRPCTimeout)
			written, err := d.deps.submitFn(subCtx, rows)
			cancel()
			if err != nil {
				// Fail-closed batch: most likely the owner granted content.read
				// but no content.label-write. The obligation stays owed; skip
				// this owner for the rest of the run.
				d.deps.logger.Warn("rescore-drain: submit_scores rejected; skipping owner this run",
					"rows", len(rows), "err", err)
				deniedOwners[ownerStr] = true
				continue
			}
			totalScored += int(written)
		}

		if ctx.Err() != nil {
			return
		}
		// A batch of only already-seen/denied units means the remainder is
		// undrainable by this holder right now (unsupported factors, missing
		// grants) — stop rather than spin on the same worklist.
		if !progressed || len(units) < drainWorklistLimit {
			break
		}
	}

	if totalScored > 0 {
		d.deps.logger.Info("rescore-drain: obligation drained", "rows_written", totalScored)
	}
}

// rescoreUnit services one work-unit: grant lookup → fetch → unseal → re-run
// the unit's factor scorer. Returns (entry, true) on success; (zero, false) to
// skip (logged) — the obligation stays owed on the nest. Two scorer families:
// the built-in perimeter scanners (clamav/rspamd, tier-2) and subscribed
// community labelers (`labeler:<id>` factor, tier-3, run as sandboxed WASM —
// Slice 3b).
func (d *rescoreDrain) rescoreUnit(ctx context.Context, u *wsrpc.RescoreUnit) (wsrpc.ScoreEntry, bool) {
	log := d.deps.logger
	if u.ContentKind != "mail" {
		// Only the mail store is wired today (calendar/post drains arrive with
		// their holders); the unit stays owed for a future holder.
		return wsrpc.ScoreEntry{}, false
	}
	isLabeler := strings.HasPrefix(u.Factor, wsrpc.LabelerFactorPrefix)
	// The license this unit needs from the owner's grant set: a community
	// labeler's own `labeler:<hex>` factor, or "" — the built-in perimeter
	// factors the composed "read and filter my mail" grant covers. Parsed
	// BEFORE any fetch so a malformed factor never pulls a record, and matched
	// exactly against each wrap's AAD-bound `factor` at key selection below —
	// so a worklist naming a labeler the owner never granted unseals nothing,
	// whatever the nest claims.
	var licenseFactor string
	var labelerID []byte
	if isLabeler {
		id, err := hex.DecodeString(strings.TrimPrefix(u.Factor, wsrpc.LabelerFactorPrefix))
		if err != nil || len(id) != 32 {
			log.Warn("rescore-drain: malformed labeler factor; skipping", "factor", u.Factor)
			return wsrpc.ScoreEntry{}, false
		}
		labelerID = id
		licenseFactor = u.Factor
	}
	if !isLabeler {
		// A built-in scanner factor: gate on its enablement before any fetch. A
		// labeler factor has no such deployment toggle — a subscribed labeler is
		// drainable whenever the holder has the grant + can fetch the module.
		switch u.Factor {
		case wsrpc.FactorClamav:
			if !d.deps.scanCfg.Policy.ClamavEnabled {
				return wsrpc.ScoreEntry{}, false
			}
		case wsrpc.FactorRspamd:
			if !d.deps.scanCfg.Policy.RspamdEnabled {
				return wsrpc.ScoreEntry{}, false
			}
		default:
			// auth_* need connection-time envelope state not in the stored body;
			// spam (tier 1) is self-servicing at the AUTH'd session / client.
			return wsrpc.ScoreEntry{}, false
		}
	}

	fetchCtx, cancel := context.WithTimeout(ctx, drainRPCTimeout)
	ct, err := d.deps.fetchFn(fetchCtx, u.OwnerActorID, u.ContentID)
	cancel()
	if err != nil {
		if !errors.Is(err, wsrpc.ErrMessageNotFound) {
			log.Warn("rescore-drain: ciphertext fetch failed", "err", err)
		}
		return wsrpc.ScoreEntry{}, false
	}

	// Every mail record rests sealed, so the unit transiently unseals under the
	// grant key: content-sealing-epochs design § 4's windowed-holder chain —
	// candidates are keyed off the RECORD's seal instant
	// (ct.SealEpochBasisUnix: the nest's stored_at, which is when the seal
	// key was selected — NOT InternalDate, which for imported mail is the
	// message's own historical timestamp and would classify to an epoch
	// older than the seal's, darking imported mail to the drain), never "now" (a record sealed weeks ago must classify to ITS
	// epoch, not today's), ordered nearest-target-first, and the holder's
	// own advisory window is still checked against "now" (window-honored,
	// same defense-in-depth KeyFor always did). Every candidate is tried in
	// order; an AEAD-fail is never fatal (INFO-A) — a record sealed after the
	// grant's held epoch set is *meant* to stay dark to this holder, exactly
	// the bound design § 8's tier_3 bar proves. The fetch happens before the
	// grant check because only the fetched record carries its seal instant.
	candidates := d.deps.registry.Current().KeyForMailEpoch(
		u.OwnerActorID, licenseFactor, ct.SealEpochBasisUnix(), uint64(d.deps.nowFn().Unix()))
	if len(candidates) == 0 {
		log.Warn("rescore-drain: no in-window content.read grant licensing this factor for sealed unit; skipping",
			"factor", u.Factor)
		return wsrpc.ScoreEntry{}, false
	}
	var pt []byte
	var openErr error
	for _, key := range candidates {
		pt, openErr = d.deps.openFn(ct.EncryptedBody, key)
		if openErr == nil {
			break
		}
	}
	if openErr != nil {
		log.Warn("rescore-drain: record unseal failed under every candidate grant key", "err", openErr)
		return wsrpc.ScoreEntry{}, false
	}
	defer zeroizeBytes(pt)

	var score int64
	tier := wsrpc.TierAdmin
	if isLabeler {
		// Tier-3 community labeler: the record opened under a wrap the owner
		// minted for THIS labeler (above); fetch the exact signed module by
		// the parsed id and run `label()` over the unsealed mail. The shared
		// FFI re-verifies sig+hash (B1), refuses a module whose signed id is
		// not the one asked for, and clamps the self-signed limits to the host
		// ceiling (F4) before running, so a compromised store / malicious
		// publisher can't swap the module or widen the sandbox.
		tier = wsrpc.TierCommunity
		inspectCtx, cancelInspect := context.WithTimeout(ctx, drainRPCTimeout)
		metadataBlob, wasmBytes, err := d.deps.inspectLabelerFn(inspectCtx, labelerID)
		cancelInspect()
		if err != nil {
			log.Warn("rescore-drain: labeler inspect failed", "factor", u.Factor, "err", err)
			return wsrpc.ScoreEntry{}, false
		}
		score, err = d.deps.labelerScoreFn(labelerID, metadataBlob, wasmBytes, pt)
		if err != nil {
			// A verify/compile/run failure leaves the obligation owed (the nest
			// keeps the version gap) rather than stamping a bogus score.
			log.Warn("rescore-drain: labeler execute failed", "factor", u.Factor, "err", err)
			return wsrpc.ScoreEntry{}, false
		}
	} else {
		scanCtx, cancel := context.WithTimeout(ctx, scan.ScanTimeout())
		defer cancel()
		switch u.Factor {
		case wsrpc.FactorClamav:
			reply, err := d.deps.clamdFn(scanCtx, d.deps.scanCfg.ClamdAddr, pt)
			if err != nil {
				log.Warn("rescore-drain: clamd re-scan failed", "err", err)
				return wsrpc.ScoreEntry{}, false
			}
			switch mailfauna.ClamdParseReply(reply).(type) {
			case mailfauna.ClamavVerdictClean:
				score = 0
			case mailfauna.ClamavVerdictInfected:
				// The uniform bus summary, the same value the ingest-time mapping
				// writes (fauna_core::scoring::perimeter_mail_score_rows: Infected
				// → 1000). The drain records the verdict; disposition of
				// already-delivered mail is a policy surface, not the drain's (it
				// writes SQL rows only).
				score = 1000
			default:
				// Error verdicts are not a score — leave the obligation owed.
				return wsrpc.ScoreEntry{}, false
			}
		case wsrpc.FactorRspamd:
			rs, err := d.deps.rspamdFn(scanCtx, d.deps.scanCfg, pt)
			if err != nil {
				log.Warn("rescore-drain: rspamd re-scan failed", "err", err)
				return wsrpc.ScoreEntry{}, false
			}
			score = int64(rs.ScaledMilli)
		}
	}

	return wsrpc.ScoreEntry{
		Factor: u.Factor,
		Score:  score,
		// clamav/rspamd are admin-tier (2); a `labeler:` factor is community (3).
		Tier: tier,
		// The re-scored row is stamped at the CURRENT model version — the row
		// is the durable watermark, so this write closes the obligation gap.
		ScorerVersion: u.ToVersion,
	}, true
}

// zeroizeBytes best-effort wipes a transient plaintext buffer.
func zeroizeBytes(b []byte) {
	for i := range b {
		b[i] = 0
	}
}
