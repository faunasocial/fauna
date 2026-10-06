package capability

import (
	"bytes"
	"context"
	"errors"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
)

// freshGrant builds a NEW CapabilityGrant with freshly-allocated key backing
// arrays on every call — matching production, where each UnsealCapabilityGrant
// mints fresh Key bytes, so retiring/zeroizing one refresh's copy never touches
// another's.
func freshGrant(owner, id byte, keyFill byte) *mailfauna.CapabilityGrant {
	class := "content.read"
	kind := "mail"
	key := bytes.Repeat([]byte{keyFill}, 32)
	return &mailfauna.CapabilityGrant{
		OwnerActorId: []byte{owner},
		GrantId:      []byte{id},
		EpochStart:   1,
		EpochEnd:     2,
		Keys: []mailfauna.CapabilityScopeKey{
			{Class: class, Kind: &kind, Key: key},
		},
	}
}

// unsealByBlob maps a canned blob byte-string to a fresh grant (or an error for
// "BAD"), standing in for the FFI HPKE-Open.
func unsealByBlob(blob []byte, _ []byte, _ []byte) (*mailfauna.CapabilityGrant, error) {
	switch string(blob) {
	case "A":
		return freshGrant(0x0A, 0xA1, 0xAA), nil
	case "B":
		return freshGrant(0x0B, 0xB1, 0xBB), nil
	case "BAD":
		return nil, errors.New("hpke open failed")
	default:
		return nil, errors.New("unknown blob")
	}
}

// scriptedFetch returns a queued (blobs, err) per Refresh; the last entry
// repeats for any further calls.
type scriptedFetch struct {
	calls   int
	results []fetchResult
}

type fetchResult struct {
	blobs [][]byte
	err   error
}

func (s *scriptedFetch) next(_ context.Context) ([][]byte, error) {
	i := s.calls
	s.calls++
	if i >= len(s.results) {
		i = len(s.results) - 1
	}
	return s.results[i].blobs, s.results[i].err
}

func newTestRegistry(t *testing.T, fetch func(context.Context) ([][]byte, error)) *Registry {
	t.Helper()
	reg, err := New(Config{
		X25519Secret: make([]byte, 32),
		FetchFn:      fetch,
		UnsealFn:     unsealByBlob,
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	return reg
}

func blob(s string) []byte { return []byte(s) }

func TestRefresh_cachesUnwrappedGrants(t *testing.T) {
	f := &scriptedFetch{results: []fetchResult{{blobs: [][]byte{blob("A"), blob("B")}}}}
	reg := newTestRegistry(t, f.next)

	if err := reg.Refresh(context.Background()); err != nil {
		t.Fatalf("Refresh: %v", err)
	}
	set := reg.Current()
	if set.Len() != 2 {
		t.Fatalf("Current().Len(): got %d, want 2", set.Len())
	}
	// The unwrapped keys are present and correct.
	for _, g := range set.Grants() {
		switch g.OwnerActorId[0] {
		case 0x0A:
			if !bytes.Equal(g.Keys[0].Key, bytes.Repeat([]byte{0xAA}, 32)) {
				t.Errorf("grant A key mismatch")
			}
		case 0x0B:
			if !bytes.Equal(g.Keys[0].Key, bytes.Repeat([]byte{0xBB}, 32)) {
				t.Errorf("grant B key mismatch")
			}
		default:
			t.Errorf("unexpected grant owner %x", g.OwnerActorId)
		}
	}
}

// The load-bearing divergence: an AUTHORITATIVE fetch that omits a previously
// held grant goes dark IMMEDIATELY (absent from Current()) and zeroizes the
// unwrapped keys (one cycle later, via the retired slot). Design § 2.3.
func TestRefresh_authoritativeEmptyRevokesAndGoesDark(t *testing.T) {
	f := &scriptedFetch{results: []fetchResult{
		{blobs: [][]byte{blob("A")}}, // cycle 1: grant A present
		{blobs: nil},                 // cycle 2: nest omits A → revoked
		{blobs: nil},                 // cycle 3: still empty (drives the deferred wipe)
	}}
	reg := newTestRegistry(t, f.next)
	ctx := context.Background()

	// Cycle 1 — grant A cached; capture the live key slice the registry holds.
	if err := reg.Refresh(ctx); err != nil {
		t.Fatalf("Refresh 1: %v", err)
	}
	if reg.Current().Len() != 1 {
		t.Fatalf("after cycle 1: Len() = %d, want 1", reg.Current().Len())
	}
	keyRef := reg.Current().Grants()[0].Keys[0].Key
	if !bytes.Equal(keyRef, bytes.Repeat([]byte{0xAA}, 32)) {
		t.Fatalf("cycle 1 key not populated")
	}

	// Cycle 2 — authoritative empty fetch: go dark immediately, but the memory
	// wipe of A's bytes is deferred one cycle (A now sits in `retired`).
	if err := reg.Refresh(ctx); err != nil {
		t.Fatalf("Refresh 2: %v", err)
	}
	if reg.Current().Len() != 0 {
		t.Fatalf("after cycle 2 (revoke): Len() = %d, want 0 (dark)", reg.Current().Len())
	}
	if bytes.Equal(keyRef, make([]byte, 32)) {
		t.Fatalf("cycle 2: key wiped too early (should defer one cycle)")
	}

	// Cycle 3 — the next swap retires-and-wipes cycle 2's parked set (grant A).
	if err := reg.Refresh(ctx); err != nil {
		t.Fatalf("Refresh 3: %v", err)
	}
	if !bytes.Equal(keyRef, make([]byte, 32)) {
		t.Fatalf("cycle 3: revoked key not zeroized: %x", keyRef)
	}
}

// Close zeroizes the parked-retired set promptly (no need to wait for the next
// refresh) — so a revoke followed by shutdown still wipes the revoked key.
func TestClose_zeroizesRetiredAfterRevoke(t *testing.T) {
	f := &scriptedFetch{results: []fetchResult{
		{blobs: [][]byte{blob("A")}},
		{blobs: nil},
	}}
	reg := newTestRegistry(t, f.next)
	ctx := context.Background()

	_ = reg.Refresh(ctx)
	keyRef := reg.Current().Grants()[0].Keys[0].Key
	_ = reg.Refresh(ctx) // A retired
	if bytes.Equal(keyRef, make([]byte, 32)) {
		t.Fatalf("key wiped before Close")
	}

	reg.Close()
	if !bytes.Equal(keyRef, make([]byte, 32)) {
		t.Fatalf("Close did not zeroize retired revoked key: %x", keyRef)
	}
	// The holder secret is also wiped.
	if !bytes.Equal(reg.x25519Sec, make([]byte, 32)) {
		t.Fatalf("Close did not zeroize the holder secret")
	}
}

// A TRANSIENT transport error must KEEP the cached set — revocation
// does NOT bite on a network/timeout error. This is the contrast to the
// authoritative-empty case above.
func TestRefresh_transportErrorKeepsCachedSet(t *testing.T) {
	f := &scriptedFetch{results: []fetchResult{
		{blobs: [][]byte{blob("A")}},         // cycle 1: A cached
		{err: errors.New("network timeout")}, // cycle 2: transport error
	}}
	reg := newTestRegistry(t, f.next)
	ctx := context.Background()

	if err := reg.Refresh(ctx); err != nil {
		t.Fatalf("Refresh 1: %v", err)
	}
	keyRef := reg.Current().Grants()[0].Keys[0].Key

	if err := reg.Refresh(ctx); err == nil {
		t.Fatal("Refresh 2: expected transport error to propagate")
	}
	// Cache unchanged — still holding A, key not wiped.
	if reg.Current().Len() != 1 {
		t.Fatalf("after transport error: Len() = %d, want 1 (kept)", reg.Current().Len())
	}
	if bytes.Equal(keyRef, make([]byte, 32)) {
		t.Fatalf("transport error wrongly zeroized the cached key")
	}
}

// A revoked grant is dropped while OTHER still-served grants survive.
func TestRefresh_dropsRevokedKeepsOthers(t *testing.T) {
	f := &scriptedFetch{results: []fetchResult{
		{blobs: [][]byte{blob("A"), blob("B")}}, // both present
		{blobs: [][]byte{blob("B")}},            // A revoked, B stays
	}}
	reg := newTestRegistry(t, f.next)
	ctx := context.Background()

	_ = reg.Refresh(ctx)
	if reg.Current().Len() != 2 {
		t.Fatalf("cycle 1: Len() = %d, want 2", reg.Current().Len())
	}

	_ = reg.Refresh(ctx)
	set := reg.Current()
	if set.Len() != 1 {
		t.Fatalf("cycle 2: Len() = %d, want 1", set.Len())
	}
	if set.Grants()[0].OwnerActorId[0] != 0x0B {
		t.Fatalf("cycle 2: kept the wrong grant: owner %x", set.Grants()[0].OwnerActorId)
	}
}

// A present-but-unsealable grant is omitted (logged) but does NOT abort the
// refresh or trigger keep-old — one bad grant must not block others' revocation.
func TestRefresh_skipsUnsealableGrantWithoutAborting(t *testing.T) {
	f := &scriptedFetch{results: []fetchResult{
		{blobs: [][]byte{blob("BAD"), blob("A")}},
	}}
	reg := newTestRegistry(t, f.next)

	if err := reg.Refresh(context.Background()); err != nil {
		t.Fatalf("Refresh must succeed despite one bad grant: %v", err)
	}
	set := reg.Current()
	if set.Len() != 1 {
		t.Fatalf("Len() = %d, want 1 (bad grant omitted, good grant kept)", set.Len())
	}
	if set.Grants()[0].OwnerActorId[0] != 0x0A {
		t.Fatalf("kept the wrong grant")
	}
}

func TestNew_validatesInput(t *testing.T) {
	if _, err := New(Config{X25519Secret: make([]byte, 31), FetchFn: (&scriptedFetch{}).next}); err == nil {
		t.Error("expected 31-byte secret to reject")
	}
	// No Caller and no FetchFn → error (nothing to fetch with).
	if _, err := New(Config{X25519Secret: make([]byte, 32)}); err == nil {
		t.Error("expected missing Caller+FetchFn to reject")
	}
}

// The background loop refreshes on a SIGHUP/TriggerRefresh poke even when the
// scheduled timer never fires.
func TestRefreshLoop_hupTriggersRefresh(t *testing.T) {
	fetched := make(chan struct{}, 4)
	neverFire := func(time.Duration) <-chan time.Time { return make(chan time.Time) }
	reg, err := New(Config{
		X25519Secret: make([]byte, 32),
		AfterFn:      neverFire,
		FetchFn: func(_ context.Context) ([][]byte, error) {
			fetched <- struct{}{}
			return nil, nil
		},
		UnsealFn: unsealByBlob,
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	reg.Start(ctx)

	reg.TriggerRefresh()
	select {
	case <-fetched:
	case <-time.After(3 * time.Second):
		t.Fatal("hup did not trigger a refresh")
	}
}

// Close ENFORCES its "loop has exited" precondition: it blocks until the
// Start goroutines are gone (after ctx cancel), then wipes under the same
// mutex Refresh holds — so a teardown can never zeroize the secret out from
// under an in-flight Refresh. Run with
// -race, which is what would flag the unsynchronized pair.
func TestClose_waitsForLoopAndBlocksLateRefresh(t *testing.T) {
	inFetch := make(chan struct{})
	releaseFetch := make(chan struct{})
	reg, err := New(Config{
		X25519Secret: bytes.Repeat([]byte{0x5E}, 32),
		AfterFn:      func(time.Duration) <-chan time.Time { return make(chan time.Time) },
		FetchFn: func(_ context.Context) ([][]byte, error) {
			inFetch <- struct{}{}
			<-releaseFetch
			return [][]byte{blob("A")}, nil
		},
		UnsealFn: unsealByBlob,
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	reg.Start(ctx)
	reg.TriggerRefresh() // loop enters the (blocked) fetch

	<-inFetch // Refresh now in flight inside the loop

	closed := make(chan struct{})
	go func() {
		cancel()    // stop the loop (it still finishes the in-flight Refresh)
		reg.Close() // must WAIT for the loop + the in-flight Refresh
		close(closed)
	}()

	select {
	case <-closed:
		t.Fatal("Close returned while a Refresh was still in flight")
	case <-time.After(50 * time.Millisecond):
	}

	close(releaseFetch) // let the in-flight Refresh finish
	select {
	case <-closed:
	case <-time.After(3 * time.Second):
		t.Fatal("Close did not return after the loop exited")
	}

	// Post-Close, Refresh is a typed error — never an HPKE run against the
	// zeroized secret.
	if err := reg.Refresh(context.Background()); err == nil {
		t.Fatal("Refresh after Close must error")
	}
	for _, b := range reg.x25519Sec {
		if b != 0 {
			t.Fatal("holder secret not zeroized by Close")
		}
	}
}

// Refresh is single-flight: overlapping calls serialize on refreshMu, so the
// one-cycle-deferred retired zeroize can never wipe a set an intervening
// Current() reader still holds.
func TestRefresh_isSingleFlight(t *testing.T) {
	inFetch := make(chan struct{}, 2)
	releaseFetch := make(chan struct{})
	var concurrent, maxConcurrent int
	var mu = make(chan struct{}, 1) // tiny hand-rolled lock usable in the seam
	mu <- struct{}{}
	reg, err := New(Config{
		X25519Secret: make([]byte, 32),
		FetchFn: func(_ context.Context) ([][]byte, error) {
			<-mu
			concurrent++
			if concurrent > maxConcurrent {
				maxConcurrent = concurrent
			}
			mu <- struct{}{}
			inFetch <- struct{}{}
			<-releaseFetch
			<-mu
			concurrent--
			mu <- struct{}{}
			return nil, nil
		},
		UnsealFn: unsealByBlob,
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	done := make(chan struct{}, 2)
	for range 2 {
		go func() {
			_ = reg.Refresh(context.Background())
			done <- struct{}{}
		}()
	}

	<-inFetch // first Refresh inside the fetch; second must be queued
	select {
	case <-inFetch:
		t.Fatal("second Refresh entered fetch concurrently — not single-flight")
	case <-time.After(50 * time.Millisecond):
	}

	close(releaseFetch)
	<-done
	<-done
	<-inFetch // the queued second Refresh ran after the first
	<-mu
	if maxConcurrent != 1 {
		t.Fatalf("max concurrent fetches: got %d, want 1", maxConcurrent)
	}
	mu <- struct{}{}
}

// KeyFor scopes by owner + class/kind AND independently honors the grant's
// advisory window (the drain must not wield a key past
// the honest bound even if the nest served it).
func TestGrantSet_KeyFor(t *testing.T) {
	f := &scriptedFetch{results: []fetchResult{{blobs: [][]byte{blob("A")}}}}
	reg := newTestRegistry(t, f.next)
	if err := reg.Refresh(context.Background()); err != nil {
		t.Fatalf("Refresh: %v", err)
	}
	set := reg.Current()

	// freshGrant's window is [1, 2]; owner 0x0A; content.read{mail} key 0xAA…
	if got := set.KeyFor([]byte{0x0A}, "content.read", "mail", 1); !bytes.Equal(got, bytes.Repeat([]byte{0xAA}, 32)) {
		t.Fatalf("in-window key: got %x", got)
	}
	if set.KeyFor([]byte{0x0A}, "content.read", "mail", 3) != nil {
		t.Fatal("a key past epoch_end must not be wielded")
	}
	if set.KeyFor([]byte{0x0A}, "content.read", "mail", 0) != nil {
		t.Fatal("a key before epoch_start must not be wielded")
	}
	if set.KeyFor([]byte{0x0B}, "content.read", "mail", 1) != nil {
		t.Fatal("an un-granted owner must yield no key")
	}
	if set.KeyFor([]byte{0x0A}, "content.read", "calendar", 1) != nil {
		t.Fatal("a different kind must yield no key")
	}
	if set.KeyFor([]byte{0x0A}, "content.label-write", "mail", 1) != nil {
		t.Fatal("a different class must yield no key")
	}
}

// epochGrant builds a bounded mail grant carrying one wrapped key per given
// epoch (content-sealing-epochs design § 2's mint policy: every wrap here
// carries Epoch != nil — never mixed with a standing wrap in the same grant).
func epochGrant(owner, id byte, epochs []uint64) *mailfauna.CapabilityGrant {
	class := "content.read"
	kind := "mail"
	keys := make([]mailfauna.CapabilityScopeKey, len(epochs))
	for i, e := range epochs {
		ep := e
		keys[i] = mailfauna.CapabilityScopeKey{
			Class: class,
			Kind:  &kind,
			Epoch: &ep,
			Key:   bytes.Repeat([]byte{byte(e)}, 32),
		}
	}
	return &mailfauna.CapabilityGrant{
		OwnerActorId: []byte{owner},
		GrantId:      []byte{id},
		EpochStart:   0,
		EpochEnd:     ^uint64(0),
		Keys:         keys,
	}
}

// masterKeyGrant builds a standing (pre-epoch-adoption) mail grant — one wrap
// with Epoch == nil, the master-key regime.
func masterKeyGrant(owner, id byte, keyFill byte) *mailfauna.CapabilityGrant {
	class := "content.read"
	kind := "mail"
	return &mailfauna.CapabilityGrant{
		OwnerActorId: []byte{owner},
		GrantId:      []byte{id},
		EpochStart:   0,
		EpochEnd:     ^uint64(0),
		Keys: []mailfauna.CapabilityScopeKey{
			{Class: class, Kind: &kind, Key: bytes.Repeat([]byte{keyFill}, 32)},
		},
	}
}

func tsForEpoch(e uint64) uint64 {
	return e*7*24*60*60 + 10
}

// KeyForMailEpoch's bounded-regime chain: exact epoch first, then every OLDER
// held epoch as a fallback candidate (nearest first) — never a held epoch
// that postdates the target (design § 4 windowed-holder chain).
func TestGrantSet_KeyForMailEpoch_bounded(t *testing.T) {
	owner := byte(0x0A)
	grant := epochGrant(owner, 0xA1, []uint64{100, 101, 102})
	unseal := func(blobBytes, _, _ []byte) (*mailfauna.CapabilityGrant, error) {
		if string(blobBytes) == "EPOCHS" {
			return grant, nil
		}
		return nil, errors.New("unknown blob")
	}
	reg, err := New(Config{
		X25519Secret: make([]byte, 32),
		FetchFn:      (&scriptedFetch{results: []fetchResult{{blobs: [][]byte{blob("EPOCHS")}}}}).next,
		UnsealFn:     unseal,
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	if err := reg.Refresh(context.Background()); err != nil {
		t.Fatalf("Refresh: %v", err)
	}
	set := reg.Current()

	k := func(e uint64) []byte { return bytes.Repeat([]byte{byte(e)}, 32) }

	got := set.KeyForMailEpoch([]byte{owner}, "", tsForEpoch(101), 1)
	want := [][]byte{k(101), k(100)}
	if !equalByteSlices(got, want) {
		t.Fatalf("exact epoch first + older fallback: got %x, want %x", got, want)
	}

	got = set.KeyForMailEpoch([]byte{owner}, "", tsForEpoch(103), 1)
	want = [][]byte{k(102), k(101), k(100)}
	if !equalByteSlices(got, want) {
		t.Fatalf("epoch 103 not held: got %x, want %x nearest-first", got, want)
	}

	if got := set.KeyForMailEpoch([]byte{owner}, "", tsForEpoch(50), 1); len(got) != 0 {
		t.Fatalf("before every held epoch must be dark, got %x", got)
	}
	if got := set.KeyForMailEpoch([]byte{0x0B}, "", tsForEpoch(101), 1); len(got) != 0 {
		t.Fatalf("a grant from one owner must never open another's mail, got %x", got)
	}
}

// A master-key (standing) mail grant opens any timestamp — the mint policy's
// XOR means this never mixes with per-epoch wraps in the same grant.
func TestGrantSet_KeyForMailEpoch_masterKey(t *testing.T) {
	owner := byte(0x0A)
	grant := masterKeyGrant(owner, 0xA1, 0x42)
	unseal := func(blobBytes, _, _ []byte) (*mailfauna.CapabilityGrant, error) {
		if string(blobBytes) == "MASTER" {
			return grant, nil
		}
		return nil, errors.New("unknown blob")
	}
	reg, err := New(Config{
		X25519Secret: make([]byte, 32),
		FetchFn:      (&scriptedFetch{results: []fetchResult{{blobs: [][]byte{blob("MASTER")}}}}).next,
		UnsealFn:     unseal,
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	if err := reg.Refresh(context.Background()); err != nil {
		t.Fatalf("Refresh: %v", err)
	}
	set := reg.Current()

	standing := bytes.Repeat([]byte{0x42}, 32)
	if got := set.KeyForMailEpoch([]byte{owner}, "", 0, 1); !equalByteSlices(got, [][]byte{standing}) {
		t.Fatalf("standing key at ts=0: got %x", got)
	}
	if got := set.KeyForMailEpoch([]byte{owner}, "", tsForEpoch(999), 1); !equalByteSlices(got, [][]byte{standing}) {
		t.Fatalf("standing key opens any epoch: got %x", got)
	}
}

// labelerGrant builds a per-labeler mail grant: one epoch wrap whose license
// (`Factor`) names exactly `factor` — the shape a labeler subscription mints.
func labelerGrant(owner, id byte, factor string, epoch uint64, keyFill byte) *mailfauna.CapabilityGrant {
	kind := "mail"
	ep := epoch
	f := factor
	return &mailfauna.CapabilityGrant{
		OwnerActorId: []byte{owner},
		GrantId:      []byte{id},
		EpochStart:   0,
		EpochEnd:     ^uint64(0),
		Keys: []mailfauna.CapabilityScopeKey{
			{Class: "content.read", Kind: &kind, Epoch: &ep, Factor: &f, Key: bytes.Repeat([]byte{keyFill}, 32)},
		},
	}
}

// The per-labeler license: a wrap serves exactly the
// factor it was minted for. The composed (factor-less) grant yields no key for
// a community labeler however the worklist names it; labeler A's grant yields
// none for labeler B nor for a built-in factor; each opens only its own.
func TestGrantSet_KeyForMailEpoch_confinedToTheWrapsFactor(t *testing.T) {
	owner := byte(0x0A)
	composed := epochGrant(owner, 0xA1, []uint64{101})
	labelerA := labelerGrant(owner, 0xA2, "labeler:aa", 101, 0xAA)
	unseal := func(blobBytes, _, _ []byte) (*mailfauna.CapabilityGrant, error) {
		switch string(blobBytes) {
		case "COMPOSED":
			return composed, nil
		case "LABELER-A":
			return labelerA, nil
		}
		return nil, errors.New("unknown blob")
	}
	reg, err := New(Config{
		X25519Secret: make([]byte, 32),
		FetchFn:      (&scriptedFetch{results: []fetchResult{{blobs: [][]byte{blob("COMPOSED"), blob("LABELER-A")}}}}).next,
		UnsealFn:     unseal,
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	if err := reg.Refresh(context.Background()); err != nil {
		t.Fatalf("Refresh: %v", err)
	}
	set := reg.Current()

	if got := set.KeyForMailEpoch([]byte{owner}, "", tsForEpoch(101), 1); !equalByteSlices(got, [][]byte{bytes.Repeat([]byte{101}, 32)}) {
		t.Fatalf("a built-in factor opens under the composed grant only: got %x", got)
	}
	if got := set.KeyForMailEpoch([]byte{owner}, "labeler:aa", tsForEpoch(101), 1); !equalByteSlices(got, [][]byte{bytes.Repeat([]byte{0xAA}, 32)}) {
		t.Fatalf("labeler A opens under its own grant only: got %x", got)
	}
	if got := set.KeyForMailEpoch([]byte{owner}, "labeler:bb", tsForEpoch(101), 1); len(got) != 0 {
		t.Fatalf("a labeler the owner never granted must stay dark, got %x", got)
	}
}

func equalByteSlices(a, b [][]byte) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if !bytes.Equal(a[i], b[i]) {
			return false
		}
	}
	return true
}

// A transport error keeps the LIVE set but wipes the ≥1-cycle-old RETIRED set,
// so a superseded (possibly revoked) key does not linger un-zeroized through a
// persistent fetch outage.
func TestRefresh_transportErrorWipesRetired(t *testing.T) {
	f := &scriptedFetch{results: []fetchResult{
		{blobs: [][]byte{blob("A"), blob("B")}}, // 1: A+B live
		{blobs: [][]byte{blob("A")}},            // 2: B revoked → B's set parked in retired
		{err: errors.New("network down")},       // 3: transport error
	}}
	reg := newTestRegistry(t, f.next)
	ctx := context.Background()

	if err := reg.Refresh(ctx); err != nil {
		t.Fatalf("refresh 1: %v", err)
	}
	firstSet := reg.Current()
	if err := reg.Refresh(ctx); err != nil {
		t.Fatalf("refresh 2: %v", err)
	}
	if reg.retired.Load() != firstSet {
		t.Fatal("test setup: refresh 2 should park the first set in retired")
	}

	if err := reg.Refresh(ctx); err == nil {
		t.Fatal("refresh 3 should surface the transport error")
	}
	// Live set intact (revocation must not bite on a blip)…
	if reg.Current().Len() != 1 {
		t.Fatalf("live set: got %d grants, want 1", reg.Current().Len())
	}
	// …but the retired set is wiped and unparked.
	if reg.retired.Load() != nil {
		t.Fatal("retired set not unparked on transport error")
	}
	for _, g := range firstSet.Grants() {
		for _, k := range g.Keys {
			for _, b := range k.Key {
				if b != 0 {
					t.Fatal("retired key bytes not zeroized on transport error")
				}
			}
		}
	}
}
