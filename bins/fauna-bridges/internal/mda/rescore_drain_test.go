package mda

import (
	"bytes"
	"context"
	"encoding/hex"
	"errors"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/capability"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/scan"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"

	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// drainFixture wires a rescoreDrain against seams: a holder Registry whose
// (faked) nest serves one content.read{mail} grant per listed owner, and
// recording fakes for the plane RPCs + scanners. Times are pinned inside the
// grant window [1000, 2000].
type drainFixture struct {
	drain      *rescoreDrain
	worklists  [][]wsrpc.RescoreUnit // one reply per rescore_worklist call
	worklistN  int
	fetched    [][]byte // messageIDs fetched
	opened     int      // openFn invocations
	clamdRuns  int
	rspamdRuns int
	submitted  [][]wsrpc.SubmitScoreRow // one entry per submit_scores call
	submitErr  map[string]error         // keyed by first row's owner byte-string

	// Labeler-branch seams (Slice 3b).
	inspected         [][]byte // labelerIDs passed to inspectLabelerFn
	labelerIDs        [][]byte // labelerIDs passed to labelerScoreFn (the expected-id pin)
	labelerScored     int      // labelerScoreFn invocations
	labelerPt         [][]byte // the unsealed plaintext passed to labelerScoreFn
	labelerScoreValue int64    // the per-mille score labelerScoreFn returns
	labelerScoreErr   error    // if set, labelerScoreFn returns it
}

const drainTestNow = 1500

// ownerGrantRegistry holds the COMPOSED "read and filter my mail" grant per
// owner: a factor-less `content.read{mail}` wrap, which licenses the built-in
// perimeter factors and no community labeler.
func ownerGrantRegistry(t *testing.T, owners ...byte) *capability.Registry {
	t.Helper()
	return grantRegistry(t, "", owners...)
}

// labelerGrantRegistry holds the PER-LABELER grant per owner: the same wrap
// with its AAD-bound license naming exactly `factor` (`labeler:<hex>`) — the
// shape a labeler subscription over sealed mail mints.
func labelerGrantRegistry(t *testing.T, factor string, owners ...byte) *capability.Registry {
	t.Helper()
	return grantRegistry(t, factor, owners...)
}

func grantRegistry(t *testing.T, factor string, owners ...byte) *capability.Registry {
	t.Helper()
	blobs := make([][]byte, len(owners))
	for i, o := range owners {
		blobs[i] = []byte{o}
	}
	var license *string
	if factor != "" {
		f := factor
		license = &f
	}
	reg, err := capability.New(capability.Config{
		X25519Secret: make([]byte, 32),
		FetchFn:      func(_ context.Context) ([][]byte, error) { return blobs, nil },
		UnsealFn: func(blob []byte, _ []byte, _ []byte) (*mailfauna.CapabilityGrant, error) {
			kind := "mail"
			return &mailfauna.CapabilityGrant{
				OwnerActorId: []byte{blob[0]},
				GrantId:      []byte{blob[0], 0x01},
				EpochStart:   1000,
				EpochEnd:     2000,
				Keys: []mailfauna.CapabilityScopeKey{
					{Class: "content.read", Kind: &kind, Factor: license, Key: bytes.Repeat([]byte{blob[0]}, 32)},
				},
			}, nil
		},
	})
	if err != nil {
		t.Fatalf("capability.New: %v", err)
	}
	return reg
}

func newDrainFixture(t *testing.T, reg *capability.Registry, worklists ...[]wsrpc.RescoreUnit) *drainFixture {
	t.Helper()
	f := &drainFixture{worklists: worklists, submitErr: map[string]error{}}
	// The default fetchFn returns a genuinely sealed MailRecordEnvelope, the
	// only shape a mail record rests in. Sealed to a throwaway keypair; openFn
	// below stays a fake.
	sealKp := faunaFfi.GenerateX25519Keypair()
	f.drain = newRescoreDrain(rescoreDrainDeps{
		registry: reg,
		scanCfg:  scan.Config{ClamdAddr: "test-clamd", RspamdURL: "http://test", Policy: scan.PolicyDefault()},
		logger:   capDiscardLogger(),
		worklistFn: func(_ context.Context, _ uint32) ([]wsrpc.RescoreUnit, error) {
			i := f.worklistN
			f.worklistN++
			if i >= len(f.worklists) {
				return nil, nil
			}
			return f.worklists[i], nil
		},
		fetchFn: func(_ context.Context, _, messageID []byte) (*wsrpc.FetchedCiphertext, error) {
			f.fetched = append(f.fetched, messageID)
			sealed, err := mailfauna.EncryptToRecipient(append([]byte("mail-body:"), messageID...), sealKp.Pubkey)
			if err != nil {
				return nil, err
			}
			return &wsrpc.FetchedCiphertext{EncryptedBody: sealed}, nil
		},
		openFn: func(envelope, key []byte) ([]byte, error) {
			f.opened++
			if len(key) != 32 {
				return nil, errors.New("bad key")
			}
			return []byte("plain:opened-mail"), nil
		},
		clamdFn: func(_ context.Context, _ string, _ []byte) (string, error) {
			f.clamdRuns++
			return "stream: OK\x00", nil
		},
		rspamdFn: func(_ context.Context, _ scan.Config, _ []byte) (mailfauna.RspamdScore, error) {
			f.rspamdRuns++
			return mailfauna.RspamdScore{ScaledMilli: 4200}, nil
		},
		submitFn: func(_ context.Context, rows []wsrpc.SubmitScoreRow) (uint32, error) {
			if len(rows) > 0 {
				if err := f.submitErr[string(rows[0].OwnerActorID)]; err != nil {
					return 0, err
				}
			}
			f.submitted = append(f.submitted, rows)
			var n uint32
			for _, r := range rows {
				n += uint32(len(r.Entries))
			}
			return n, nil
		},
		inspectLabelerFn: func(_ context.Context, labelerID []byte) ([]byte, []byte, error) {
			f.inspected = append(f.inspected, labelerID)
			return []byte("meta:"), []byte("wasm:"), nil
		},
		labelerScoreFn: func(labelerID, _, _, pt []byte) (int64, error) {
			f.labelerScored++
			f.labelerIDs = append(f.labelerIDs, append([]byte(nil), labelerID...))
			// Copy pt: the drain zeroizes the plaintext (defer) after rescoreUnit
			// returns, so record a snapshot for post-run assertions. Production
			// reads pt synchronously inside this call, before the wipe.
			f.labelerPt = append(f.labelerPt, append([]byte(nil), pt...))
			if f.labelerScoreErr != nil {
				return 0, f.labelerScoreErr
			}
			return f.labelerScoreValue, nil
		},
		nowFn:   func() time.Time { return time.Unix(drainTestNow, 0) },
		afterFn: neverFiresCap,
	})
	return f
}

func mailUnit(owner byte, content byte, factor string, to uint32) wsrpc.RescoreUnit {
	return wsrpc.RescoreUnit{
		ContentID:    bytes.Repeat([]byte{content}, 32),
		ContentKind:  "mail",
		OwnerActorID: []byte{owner},
		Factor:       factor,
		FromVersion:  1,
		ToVersion:    to,
	}
}

// The end-to-end happy path: a clamav + an rspamd unit for one owner drain in
// one run — fetch → unseal under the grant key → co-resident re-scan → one
// per-owner submit whose entries are stamped at ToVersion / TierAdmin.
func TestRescoreDrain_drainsClamavAndRspamdUnits(t *testing.T) {
	reg := ownerGrantRegistry(t, 0x0A)
	f := newDrainFixture(t, reg, []wsrpc.RescoreUnit{
		mailUnit(0x0A, 0x01, wsrpc.FactorClamav, 3),
		mailUnit(0x0A, 0x01, wsrpc.FactorRspamd, 5),
	})

	f.drain.runOnce(context.Background())

	if f.clamdRuns != 1 || f.rspamdRuns != 1 {
		t.Fatalf("scanner runs: clamd=%d rspamd=%d, want 1/1", f.clamdRuns, f.rspamdRuns)
	}
	if f.opened != 2 {
		t.Fatalf("openFn runs: %d, want 2 (one per unit)", f.opened)
	}
	if len(f.submitted) != 1 {
		t.Fatalf("submit calls: %d, want 1", len(f.submitted))
	}
	rows := f.submitted[0]
	if len(rows) != 1 {
		t.Fatalf("rows: %d, want 1 (same content_id groups)", len(rows))
	}
	row := rows[0]
	if row.ContentKind != "mail" || !bytes.Equal(row.OwnerActorID, []byte{0x0A}) {
		t.Fatalf("row identity mismatch: %+v", row)
	}
	if row.ScoredAt != drainTestNow {
		t.Fatalf("scored_at: %d, want %d", row.ScoredAt, drainTestNow)
	}
	if len(row.Entries) != 2 {
		t.Fatalf("entries: %d, want 2", len(row.Entries))
	}
	for _, e := range row.Entries {
		switch e.Factor {
		case wsrpc.FactorClamav:
			if e.Score != 0 || e.ScorerVersion != 3 || e.Tier != wsrpc.TierAdmin {
				t.Fatalf("clamav entry: %+v", e)
			}
		case wsrpc.FactorRspamd:
			if e.Score != 4200 || e.ScorerVersion != 5 || e.Tier != wsrpc.TierAdmin {
				t.Fatalf("rspamd entry: %+v", e)
			}
		default:
			t.Fatalf("unexpected factor %q", e.Factor)
		}
	}
}

// A tier-3 community-labeler unit (`labeler:<id>` factor) drains through the
// WASM path under the owner's PER-LABELER grant: fetch → unseal under the
// wrap licensed for this labeler → inspect the module → run label() via the
// FFI seam, handed the parsed id to pin the module against → one submit
// whose entry is stamped Score / ToVersion / TierCommunity. No enablement
// toggle gates a labeler; the scanner switch is skipped.
func TestRescoreDrain_drainsLabelerUnit(t *testing.T) {
	labelerID := bytes.Repeat([]byte{0xAB}, 32)
	factor := wsrpc.LabelerFactorPrefix + hex.EncodeToString(labelerID)
	reg := labelerGrantRegistry(t, factor, 0x0A)
	f := newDrainFixture(t, reg, []wsrpc.RescoreUnit{
		mailUnit(0x0A, 0x01, factor, 7),
	})
	f.labelerScoreValue = 750

	f.drain.runOnce(context.Background())

	if f.opened != 1 {
		t.Fatalf("openFn runs: %d, want 1 (unsealed for the labeler)", f.opened)
	}
	if len(f.inspected) != 1 || !bytes.Equal(f.inspected[0], labelerID) {
		t.Fatalf("inspected: %x, want one call for %x", f.inspected, labelerID)
	}
	if f.labelerScored != 1 {
		t.Fatalf("labelerScoreFn runs: %d, want 1", f.labelerScored)
	}
	// The score seam receives the id the factor names, so the shared FFI can
	// refuse a module signed under any other key.
	if len(f.labelerIDs) != 1 || !bytes.Equal(f.labelerIDs[0], labelerID) {
		t.Fatalf("labelerScoreFn ids: %x, want %x", f.labelerIDs, labelerID)
	}
	// The score fn receives the UNSEALED plaintext (the fake openFn's output).
	wantPt := []byte("plain:opened-mail")
	if len(f.labelerPt) != 1 || !bytes.Equal(f.labelerPt[0], wantPt) {
		t.Fatalf("labeler pt: %q, want %q", f.labelerPt, wantPt)
	}
	if f.clamdRuns != 0 || f.rspamdRuns != 0 {
		t.Fatalf("scanner runs: clamd=%d rspamd=%d, want 0/0 (labeler path)", f.clamdRuns, f.rspamdRuns)
	}
	if len(f.submitted) != 1 || len(f.submitted[0]) != 1 {
		t.Fatalf("submit shape: %+v", f.submitted)
	}
	e := f.submitted[0][0].Entries
	if len(e) != 1 {
		t.Fatalf("entries: %d, want 1", len(e))
	}
	if e[0].Factor != factor || e[0].Score != 750 || e[0].ScorerVersion != 7 || e[0].Tier != wsrpc.TierCommunity {
		t.Fatalf("labeler entry: %+v, want {factor:%s score:750 ver:7 tier:%d}", e[0], factor, wsrpc.TierCommunity)
	}
}

// The holder-side pin: a worklist naming a labeler the
// owner never granted — here the owner holds only the composed "read and
// filter my mail" grant — unseals nothing, inspects nothing, runs nothing and
// submits nothing, whatever the nest claims. Before the per-labeler license
// the composed grant's key opened the mail for ANY `labeler:` factor, and a
// compromised nest could run a module of its own over the plaintext.
func TestRescoreDrain_labelerUnitUnderComposedGrantRunsNothing(t *testing.T) {
	reg := ownerGrantRegistry(t, 0x0A)
	labelerID := bytes.Repeat([]byte{0xAB}, 32)
	factor := wsrpc.LabelerFactorPrefix + hex.EncodeToString(labelerID)
	f := newDrainFixture(t, reg, []wsrpc.RescoreUnit{
		mailUnit(0x0A, 0x01, factor, 7),
	})
	f.labelerScoreValue = 750

	f.drain.runOnce(context.Background())

	if f.opened != 0 || len(f.inspected) != 0 || f.labelerScored != 0 {
		t.Fatalf("a labeler the grant does not license must run nothing: opened=%d inspected=%d scored=%d",
			f.opened, len(f.inspected), f.labelerScored)
	}
	if len(f.submitted) != 0 {
		t.Fatalf("submit calls: %d, want 0 (the obligation stays owed)", len(f.submitted))
	}
}

// The license is exact in both directions: labeler A's grant opens nothing
// for labeler B, and nothing for a built-in scanner factor either.
func TestRescoreDrain_labelerGrantLicensesOnlyItsOwnFactor(t *testing.T) {
	labelerA := bytes.Repeat([]byte{0xAA}, 32)
	labelerB := bytes.Repeat([]byte{0xBB}, 32)
	factorA := wsrpc.LabelerFactorPrefix + hex.EncodeToString(labelerA)
	factorB := wsrpc.LabelerFactorPrefix + hex.EncodeToString(labelerB)
	reg := labelerGrantRegistry(t, factorA, 0x0A)
	f := newDrainFixture(t, reg, []wsrpc.RescoreUnit{
		mailUnit(0x0A, 0x01, factorB, 7),
		mailUnit(0x0A, 0x02, wsrpc.FactorClamav, 3),
	})

	f.drain.runOnce(context.Background())

	if f.opened != 0 || len(f.inspected) != 0 || f.labelerScored != 0 || f.clamdRuns != 0 {
		t.Fatalf("labeler A's grant must serve neither labeler B nor a built-in factor: opened=%d inspected=%d scored=%d clamd=%d",
			f.opened, len(f.inspected), f.labelerScored, f.clamdRuns)
	}
	if len(f.submitted) != 0 {
		t.Fatalf("submit calls: %d, want 0", len(f.submitted))
	}
}

// A `labeler:` factor whose suffix isn't 32-byte hex is a malformed obligation
// (the nest computes the factor, so this can't happen in practice): skip it —
// no inspect, no score, no submit — leaving it owed rather than crashing.
func TestRescoreDrain_skipsMalformedLabelerFactor(t *testing.T) {
	reg := ownerGrantRegistry(t, 0x0A)
	f := newDrainFixture(t, reg, []wsrpc.RescoreUnit{
		mailUnit(0x0A, 0x01, "labeler:not-hex", 7),
	})

	f.drain.runOnce(context.Background())

	if len(f.inspected) != 0 || f.labelerScored != 0 {
		t.Fatalf("malformed labeler factor should not inspect/score: inspected=%d scored=%d",
			len(f.inspected), f.labelerScored)
	}
	if len(f.submitted) != 0 {
		t.Fatalf("submit calls: %d, want 0", len(f.submitted))
	}
}

// A labeler whose execution fails (verify/compile/run error surfaced by the FFI)
// leaves the obligation owed — no bogus row is submitted.
func TestRescoreDrain_labelerExecuteErrorLeavesOwed(t *testing.T) {
	factor := wsrpc.LabelerFactorPrefix + hex.EncodeToString(bytes.Repeat([]byte{0xCD}, 32))
	reg := labelerGrantRegistry(t, factor, 0x0A)
	f := newDrainFixture(t, reg, []wsrpc.RescoreUnit{
		mailUnit(0x0A, 0x01, factor, 4),
	})
	f.labelerScoreErr = errors.New("labeler verify: labeler wasm_hash does not match wasm_bytes")

	f.drain.runOnce(context.Background())

	if f.labelerScored != 1 {
		t.Fatalf("labelerScoreFn runs: %d, want 1 (attempted)", f.labelerScored)
	}
	if len(f.submitted) != 0 {
		t.Fatalf("submit calls: %d, want 0 (execution failed → stays owed)", len(f.submitted))
	}
}

// Units the MDA holder cannot service are skipped, their obligation staying
// owed on the nest. A non-recomputable factor (auth_spf) and a foreign content
// kind skip BEFORE any fetch. An owner with no in-window grant now costs one
// fetch — the fetched record carries its seal instant
// (`ct.SealEpochBasisUnix()`), which selects the candidate epoch keys the
// grant check weighs — but a sealed record without a grant is then skipped:
// no open, no submit.
func TestRescoreDrain_skipsUnservicableUnits(t *testing.T) {
	reg := ownerGrantRegistry(t, 0x0A)
	post := mailUnit(0x0A, 0x03, wsrpc.FactorClamav, 3)
	post.ContentKind = "post"
	f := newDrainFixture(t, reg, []wsrpc.RescoreUnit{
		mailUnit(0x0A, 0x01, "auth_spf", 3), // envelope-dependent factor
		post,                                // kind with no wired store
		mailUnit(0x0B, 0x02, wsrpc.FactorClamav, 3), // no grant for owner 0x0B
	})

	f.drain.runOnce(context.Background())

	if len(f.fetched) != 1 {
		t.Fatalf("fetched %d ciphertexts, want 1 (only the no-grant sealed unit reaches the fetch)", len(f.fetched))
	}
	if f.opened != 0 {
		t.Fatalf("openFn runs: %d, want 0 (sealed unit without a grant is skipped)", f.opened)
	}
	if len(f.submitted) != 0 {
		t.Fatalf("submit calls: %d, want 0", len(f.submitted))
	}
}

// A grant whose window has expired must not be wielded even if the nest still
// serves it (independent window honoring). The window
// check now sits AFTER the fetch (the fetched record carries its seal instant,
// `ct.SealEpochBasisUnix()`, which selects the candidate epoch keys), so the
// unit costs a fetch but the sealed record is never opened or submitted.
func TestRescoreDrain_honorsGrantWindow(t *testing.T) {
	reg := ownerGrantRegistry(t, 0x0A)
	f := newDrainFixture(t, reg, []wsrpc.RescoreUnit{
		mailUnit(0x0A, 0x01, wsrpc.FactorClamav, 3),
	})
	// Pin "now" past epoch_end (2000).
	f.drain.deps.nowFn = func() time.Time { return time.Unix(2001, 0) }

	f.drain.runOnce(context.Background())

	if f.opened != 0 || len(f.submitted) != 0 {
		t.Fatalf("an out-of-window grant was wielded: opened=%d submitted=%d",
			f.opened, len(f.submitted))
	}
}

// The content-sealing-epochs design § 4/§ 8 tier_3 bar, proved at the drain
// layer with REAL HPKE seal/open (no fake openFn): a bounded grant holding
// epochs {100,101,102} opens a record genuinely sealed under epoch 101's key,
// but a record sealed under epoch 103's key — one epoch past the held set —
// stays permanently dark to this holder EVEN THOUGH it still tries every key
// it holds (100, 101, 102, nearest-first per KeyForMailEpoch). The AEAD-fail
// is skip-not-fatal (INFO-A): the run does not error, the unit simply stays
// owed (unsubmitted) rather than crashing the drain.
func TestRescoreDrain_epochBoundedGrant_opensInWindowDarkPastHeldEpochs(t *testing.T) {
	const week = 7 * 24 * 60 * 60
	owner := byte(0x0A)

	// One real X25519 keypair per held epoch — standing in for the per-epoch
	// keys a real bounded mail grant wraps (MSEK-derived Rust-side; the exact
	// derivation is fauna-mls/fauna-capability-holder's job, already tested
	// there — this test only needs distinct, real HPKE-valid keypairs).
	epoch100Kp := faunaFfi.GenerateX25519Keypair()
	epoch101Kp := faunaFfi.GenerateX25519Keypair()
	epoch102Kp := faunaFfi.GenerateX25519Keypair()
	// Epoch 103's keypair is NEVER held by this grant — the record sealed to
	// it is the "post-t2 ingest" case.
	epoch103Kp := faunaFfi.GenerateX25519Keypair()

	reg, err := capability.New(capability.Config{
		X25519Secret: make([]byte, 32),
		FetchFn:      func(_ context.Context) ([][]byte, error) { return [][]byte{{owner}}, nil },
		UnsealFn: func(_ []byte, _ []byte, _ []byte) (*mailfauna.CapabilityGrant, error) {
			kind := "mail"
			e100, e101, e102 := uint64(100), uint64(101), uint64(102)
			return &mailfauna.CapabilityGrant{
				OwnerActorId: []byte{owner},
				GrantId:      []byte{owner, 0x01},
				EpochStart:   0,
				EpochEnd:     ^uint64(0),
				Keys: []mailfauna.CapabilityScopeKey{
					{Class: "content.read", Kind: &kind, Epoch: &e100, Key: epoch100Kp.Secret},
					{Class: "content.read", Kind: &kind, Epoch: &e101, Key: epoch101Kp.Secret},
					{Class: "content.read", Kind: &kind, Epoch: &e102, Key: epoch102Kp.Secret},
				},
			}, nil
		},
	})
	if err != nil {
		t.Fatalf("capability.New: %v", err)
	}

	// content byte 0x01 = sealed under epoch 101 (held, in-window); 0x02 =
	// sealed under epoch 103 (never held — the post-window case).
	sealPubkey := map[byte][]byte{
		0x01: epoch101Kp.Pubkey,
		0x02: epoch103Kp.Pubkey,
	}
	storedAt := map[byte]int64{ // the seal instant each record classifies from
		0x01: int64(101*week + 10),
		0x02: int64(103*week + 10),
	}

	f := &drainFixture{submitErr: map[string]error{}}
	f.drain = newRescoreDrain(rescoreDrainDeps{
		registry: reg,
		scanCfg:  scan.Config{ClamdAddr: "test-clamd", RspamdURL: "http://test", Policy: scan.PolicyDefault()},
		logger:   capDiscardLogger(),
		worklistFn: func(_ context.Context, _ uint32) ([]wsrpc.RescoreUnit, error) {
			i := f.worklistN
			f.worklistN++
			if i >= len(f.worklists) {
				return nil, nil
			}
			return f.worklists[i], nil
		},
		fetchFn: func(_ context.Context, _, messageID []byte) (*wsrpc.FetchedCiphertext, error) {
			f.fetched = append(f.fetched, messageID)
			content := messageID[0]
			sealed, err := mailfauna.EncryptToRecipient(append([]byte("mail-body:"), messageID...), sealPubkey[content])
			if err != nil {
				return nil, err
			}
			return &wsrpc.FetchedCiphertext{EncryptedBody: sealed, InternalDate: storedAt[content], StoredAt: storedAt[content]}, nil
		},
		// The REAL FFI opener — proves the cryptographic bound, not a fake.
		openFn: func(envelope, key []byte) ([]byte, error) {
			f.opened++
			return mailfauna.OpenMailRecordWithKey(envelope, key)
		},
		clamdFn: func(_ context.Context, _ string, _ []byte) (string, error) {
			f.clamdRuns++
			return "stream: OK\x00", nil
		},
		submitFn: func(_ context.Context, rows []wsrpc.SubmitScoreRow) (uint32, error) {
			f.submitted = append(f.submitted, rows)
			var n uint32
			for _, r := range rows {
				n += uint32(len(r.Entries))
			}
			return n, nil
		},
		nowFn:   func() time.Time { return time.Unix(drainTestNow, 0) },
		afterFn: neverFiresCap,
	})
	f.worklists = [][]wsrpc.RescoreUnit{{
		mailUnit(owner, 0x01, wsrpc.FactorClamav, 3), // in-window: epoch 101, held
		mailUnit(owner, 0x02, wsrpc.FactorClamav, 3), // post-window: epoch 103, never held
	}}

	f.drain.runOnce(context.Background())

	if len(f.submitted) != 1 {
		t.Fatalf("submit calls: %d, want 1 (only the in-window unit scores)", len(f.submitted))
	}
	if len(f.submitted[0]) != 1 || !bytes.Equal(f.submitted[0][0].ContentID, bytes.Repeat([]byte{0x01}, 32)) {
		t.Fatalf("submitted rows: %+v, want exactly the epoch-101 content", f.submitted[0])
	}
	// The post-window unit costs real AEAD-fail attempts (3, one per held
	// epoch — 102, 101, 100 nearest-first) plus the in-window unit's single
	// successful open: 4 total, never a crash or short-circuit.
	if f.opened != 4 {
		t.Fatalf("openFn invocations: %d, want 4 (1 success + 3 exhausted candidates)", f.opened)
	}
}

// Regression: the drain classifies a record's sealed epoch
// from the SEAL instant (the reply's stored_at), never from InternalDate.
// For imported mail the two diverge by design — the import stores the
// message's own historical timestamp as internal_date while the seal keys
// off import-time now. Under the old InternalDate basis this record
// classified to epoch_of(historical)=95, KeyForMailEpoch returned no
// candidate ≤95 from the held {100,101,102}, and the imported record went
// permanently dark to spam training (silent skip). Classifying from
// stored_at (epoch 102, held) opens it — with REAL HPKE seal/open.
func TestRescoreDrain_importedMailClassifiesFromStoredAtNotInternalDate(t *testing.T) {
	const week = 7 * 24 * 60 * 60
	owner := byte(0x0A)

	epoch102Kp := faunaFfi.GenerateX25519Keypair()
	kind := "mail"
	e100, e101, e102 := uint64(100), uint64(101), uint64(102)
	reg, err := capability.New(capability.Config{
		X25519Secret: make([]byte, 32),
		FetchFn:      func(_ context.Context) ([][]byte, error) { return [][]byte{{owner}}, nil },
		UnsealFn: func(_ []byte, _ []byte, _ []byte) (*mailfauna.CapabilityGrant, error) {
			return &mailfauna.CapabilityGrant{
				OwnerActorId: []byte{owner},
				GrantId:      []byte{owner, 0x01},
				EpochStart:   0,
				EpochEnd:     ^uint64(0),
				Keys: []mailfauna.CapabilityScopeKey{
					// Only epoch 102's key needs to be real for the open; the
					// grant also holds 100/101 so the old-basis failure mode
					// (no candidate ≤ 95) is what the assertion below rules out.
					{Class: "content.read", Kind: &kind, Epoch: &e100, Key: make([]byte, 32)},
					{Class: "content.read", Kind: &kind, Epoch: &e101, Key: make([]byte, 32)},
					{Class: "content.read", Kind: &kind, Epoch: &e102, Key: epoch102Kp.Secret},
				},
			}, nil
		},
	})
	if err != nil {
		t.Fatalf("capability.New: %v", err)
	}

	f := &drainFixture{submitErr: map[string]error{}}
	f.drain = newRescoreDrain(rescoreDrainDeps{
		registry: reg,
		scanCfg:  scan.Config{ClamdAddr: "test-clamd", RspamdURL: "http://test", Policy: scan.PolicyDefault()},
		logger:   capDiscardLogger(),
		worklistFn: func(_ context.Context, _ uint32) ([]wsrpc.RescoreUnit, error) {
			i := f.worklistN
			f.worklistN++
			if i >= len(f.worklists) {
				return nil, nil
			}
			return f.worklists[i], nil
		},
		fetchFn: func(_ context.Context, _, messageID []byte) (*wsrpc.FetchedCiphertext, error) {
			sealed, err := mailfauna.EncryptToRecipient(append([]byte("imported:"), messageID...), epoch102Kp.Pubkey)
			if err != nil {
				return nil, err
			}
			return &wsrpc.FetchedCiphertext{
				EncryptedBody: sealed,
				// The imported message's own historical timestamp — epoch 95,
				// far before any held epoch.
				InternalDate: int64(95*week + 10),
				// The seal instant: import-time now, epoch 102.
				StoredAt: int64(102*week + 20),
			}, nil
		},
		openFn: func(envelope, key []byte) ([]byte, error) {
			f.opened++
			return mailfauna.OpenMailRecordWithKey(envelope, key)
		},
		clamdFn: func(_ context.Context, _ string, _ []byte) (string, error) {
			return "stream: OK\x00", nil
		},
		submitFn: func(_ context.Context, rows []wsrpc.SubmitScoreRow) (uint32, error) {
			f.submitted = append(f.submitted, rows)
			var n uint32
			for _, r := range rows {
				n += uint32(len(r.Entries))
			}
			return n, nil
		},
		nowFn:   func() time.Time { return time.Unix(drainTestNow, 0) },
		afterFn: neverFiresCap,
	})
	f.worklists = [][]wsrpc.RescoreUnit{{
		mailUnit(owner, 0x01, wsrpc.FactorClamav, 3),
	}}

	f.drain.runOnce(context.Background())

	if len(f.submitted) != 1 || len(f.submitted[0]) != 1 {
		t.Fatalf("imported record did not score: submitted=%+v — the drain classified "+
			"from InternalDate (historical) instead of stored_at (seal instant)", f.submitted)
	}
	// Nearest-target-first from epoch 102: the real key is first, one open.
	if f.opened != 1 {
		t.Fatalf("openFn invocations: %d, want 1 (epoch 102 tried first)", f.opened)
	}
}

// Submits batch per owner: one owner's fail-closed rejection (e.g. no
// content.label-write grant) must not sink another owner's write-back.
func TestRescoreDrain_submitsPerOwnerAndIsolatesDenial(t *testing.T) {
	reg := ownerGrantRegistry(t, 0x0A, 0x0B)
	f := newDrainFixture(t, reg, []wsrpc.RescoreUnit{
		mailUnit(0x0A, 0x01, wsrpc.FactorClamav, 3),
		mailUnit(0x0B, 0x02, wsrpc.FactorClamav, 3),
	})
	f.submitErr[string([]byte{0x0A})] = errors.New("permission_denied: no label-write grant")

	f.drain.runOnce(context.Background())

	if len(f.submitted) != 1 {
		t.Fatalf("successful submit calls: %d, want 1 (owner B)", len(f.submitted))
	}
	if !bytes.Equal(f.submitted[0][0].OwnerActorID, []byte{0x0B}) {
		t.Fatalf("surviving submit is for owner %x, want 0B", f.submitted[0][0].OwnerActorID)
	}
}

// An unsealed record is never scored verbatim: with no grant key there is
// nothing to open it with, so the unit is skipped like any other sealed unit
// the drain holds no key for.
func TestRescoreDrain_rawRecordIsNotScored(t *testing.T) {
	reg := ownerGrantRegistry(t) // NO grants at all
	f := newDrainFixture(t, reg, []wsrpc.RescoreUnit{
		mailUnit(0x0A, 0x01, wsrpc.FactorClamav, 3),
	})
	// Serve a raw RFC 5322 body instead of the fixture's sealed envelope.
	f.drain.deps.fetchFn = func(_ context.Context, _, messageID []byte) (*wsrpc.FetchedCiphertext, error) {
		f.fetched = append(f.fetched, messageID)
		return &wsrpc.FetchedCiphertext{EncryptedBody: []byte("Subject: raw mail\r\n\r\nbody")}, nil
	}

	f.drain.runOnce(context.Background())

	if f.clamdRuns != 0 || len(f.submitted) != 0 {
		t.Fatalf("raw-record drain: clamd=%d submits=%d, want 0/0 (never scored verbatim)",
			f.clamdRuns, len(f.submitted))
	}
}

// The drain loop lifecycle: an immediate run at start, another per poke
// (config_changed), and a prompt exit (done closed) on ctx cancel — the
// teardown ordering mda.Run relies on before Close()ing the holder.
func TestRescoreDrain_startRunsPokesAndStops(t *testing.T) {
	reg := ownerGrantRegistry(t, 0x0A)
	ran := make(chan struct{}, 8)
	f := newDrainFixture(t, reg)
	inner := f.drain.deps.worklistFn
	f.drain.deps.worklistFn = func(ctx context.Context, limit uint32) ([]wsrpc.RescoreUnit, error) {
		ran <- struct{}{}
		return inner(ctx, limit)
	}

	ctx, cancel := context.WithCancel(context.Background())
	f.drain.start(ctx)

	select {
	case <-ran:
	case <-time.After(3 * time.Second):
		t.Fatal("no initial drain run at start")
	}

	f.drain.poke()
	select {
	case <-ran:
	case <-time.After(3 * time.Second):
		t.Fatal("poke did not trigger a drain run")
	}

	cancel()
	select {
	case <-f.drain.done:
	case <-time.After(3 * time.Second):
		t.Fatal("drain loop did not exit on ctx cancel")
	}
}

// A worklist that keeps serving the same unservicable units must not spin the
// run forever: the per-run seen-set stops on a no-progress batch.
func TestRescoreDrain_terminatesOnNoProgress(t *testing.T) {
	reg := ownerGrantRegistry(t, 0x0A)
	stuck := []wsrpc.RescoreUnit{mailUnit(0x0A, 0x01, "auth_spf", 3)}
	// Same stuck unit forever — pad the script far past drainMaxBatchesPerRun.
	lists := make([][]wsrpc.RescoreUnit, 32)
	for i := range lists {
		lists[i] = stuck
	}
	f := newDrainFixture(t, reg, lists...)

	done := make(chan struct{})
	go func() { f.drain.runOnce(context.Background()); close(done) }()
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("runOnce did not terminate on a no-progress worklist")
	}
	if f.worklistN > 2 {
		t.Fatalf("worklist fetched %d times for a stuck unit, want ≤ 2", f.worklistN)
	}
}
