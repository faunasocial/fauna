// Package capability — the content-processing capability holder loop.
//
// A capability is a user-minted, scope-limited, revocable grant that lets its
// holder (an enrolled bridge service-user — the MDA, a spam scorer, an
// FTS-indexer) unseal a specific slice of a user's content, HPKE-wrapped to the
// bridge's own X25519 pubkey (the TLS-blob shape). This Registry is the
// re-acquire transport for those grants: it periodically calls
// `fauna.capabilities.fetch`, HPKE-Opens every grant the nest sealed to this
// bridge, and swaps the unwrapped set into an atomic.Pointer cache the drain
// worker reads (design tracked internally).
//
// It follows the TLS-cert refresh loop (internal/tls) — same atomic.Pointer +
// busy-spin-proof refreshsched cadence + SIGHUP poke — with ONE deliberate
// divergence for revocation to work:
//
//   - A cached-key provider that keeps the last-good value on ANY fetch error
//     puts availability above freshness.
//   - A capability MUST distinguish a TRANSIENT transport failure (network /
//     timeout → keep the cached set) from an AUTHORITATIVE fetch that
//     no longer lists a grant (a successful `fetch` omitting it → drop it + go
//     dark + zeroize its unwrapped keys). Only the second arm makes revocation
//     bite on an honest box; conflating them (a blanket keep-old) would make
//     revoke a no-op even against an honest box (design § 2.3).
//
// So `Refresh` REPLACES the whole cache from exactly what a successful fetch
// serves (which may be fewer, or zero, grants than before) and returns the old
// set's error-free-path only on a transport error.
package capability

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"os/signal"
	"sort"
	"sync"
	"sync/atomic"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/refreshsched"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/reloadsignal"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// DefaultRefreshInterval is the steady-state cadence for scheduled re-fetches.
// 12h is the cadence the design names (§ 2.3); it is the
// FRESHNESS BACKSTOP, not the revocation-latency bound the drain relies on. A
// grant revoked between ticks stays cached until the next fetch, so the drain
// worker (Slice 5) is expected to call Refresh on-demand before wielding a
// capability — that makes an honest-box revoke bite at use time regardless of
// this cadence. A future `capability_changed` push (the config_changed shape)
// can shorten it further.
const DefaultRefreshInterval = 12 * time.Hour

// backoffSchedule names the per-failure delay before the next retry after a
// Refresh error — same short tail as the TLS provider (the fetch is cheap and
// the scheduled tick is long). refreshsched.NextDelay guarantees a > 0 delay so
// a persistently-failing refresh retries at the 15m cap forever rather than
// busy-spinning (the bug that filled example.com's disk on 2026-06-01).
var backoffSchedule = []time.Duration{
	1 * time.Minute,
	5 * time.Minute,
	15 * time.Minute,
}

// grantKey identifies a grant for cache diffing: (owner_actor_id, grant_id) —
// the same storage key + revocation handle the nest uses.
type grantKey struct {
	owner   string
	grantID string
}

func keyOf(g *mailfauna.CapabilityGrant) grantKey {
	return grantKey{owner: string(g.OwnerActorId), grantID: string(g.GrantId)}
}

// GrantSet is the immutable, atomic-swapped cache: the unwrapped grants a
// successful fetch last produced, keyed by (owner, grant_id). Never mutated
// after construction — Refresh builds a fresh set and swaps the pointer, so
// Current() readers hold a stable snapshot.
type GrantSet struct {
	byKey map[grantKey]*mailfauna.CapabilityGrant
}

// Len reports how many grants the holder currently holds.
func (s *GrantSet) Len() int {
	if s == nil {
		return 0
	}
	return len(s.byKey)
}

// Grants returns a snapshot slice of the held grants (unordered). The drain
// worker (Slice 5) walks these to find a capability for a content kind. The
// returned pointers share the cache's key bytes — treat them as read-only and
// short-lived (a later Refresh may zeroize a superseded copy, § retired).
func (s *GrantSet) Grants() []*mailfauna.CapabilityGrant {
	if s == nil {
		return nil
	}
	out := make([]*mailfauna.CapabilityGrant, 0, len(s.byKey))
	for _, g := range s.byKey {
		out = append(out, g)
	}
	return out
}

// KeyFor returns the unwrapped content key for `(owner, class, kind)` from a
// grant whose advisory window contains nowUnix, or nil when this holder wields
// no such capability. The drain calls it per work-unit; the window check
// independently honors the grant's `[epoch_start, epoch_end]` even though the
// nest already expiry-filters at fetch (defense-in-depth — the holder must not
// wield a key past the honest bound the user set). The returned slice aliases the cache's key bytes — treat as
// read-only and short-lived (a later Refresh may retire + zeroize it).
func (s *GrantSet) KeyFor(owner []byte, class, kind string, nowUnix uint64) []byte {
	if s == nil {
		return nil
	}
	for _, g := range s.byKey {
		if string(g.OwnerActorId) != string(owner) {
			continue
		}
		if nowUnix < g.EpochStart || nowUnix > g.EpochEnd {
			continue
		}
		for i := range g.Keys {
			k := &g.Keys[i]
			if k.Class == class && k.Kind != nil && *k.Kind == kind {
				return k.Key
			}
		}
	}
	return nil
}

// mailEpochCandidate pairs a bounded mail wrap's held epoch with its key, so
// KeyForMailEpoch can sort candidates nearest-target-first before shedding
// the epoch (the caller only wants the ordered keys).
type mailEpochCandidate struct {
	epoch uint64
	key   []byte
}

// LicensesFactor reports whether a wrap's per-factor license
// (`ScopeTuple.factor`, AAD-bound at mint) covers the bus factor the holder
// is about to compute. `factor == ""` is the built-in perimeter factors (the
// composed "read and filter my mail" role, whose wraps carry no factor); a
// community labeler's `labeler:<hex>` factor matches only a wrap the owner
// minted for exactly that labeler. Exact in both directions — a labeler's
// wrap never serves a built-in factor either. The one predicate the Rust
// holder (`fauna_capability_holder::GrantSet`) and this port share.
func LicensesFactor(k *mailfauna.CapabilityScopeKey, factor string) bool {
	if k.Factor == nil {
		return factor == ""
	}
	return *k.Factor == factor
}

// KeyForMailEpoch returns candidate `content.read{mail}` keys for a record
// sealed at the wall-clock epoch its own ingest timestamp implies — the
// content-sealing-epochs design § 4 windowed-holder opener chain.
// recordTimestamp is the record's stored ingest instant
// (wsrpc.FetchedCiphertext.InternalDate); the target epoch is
// mailfauna.MailSealingEpochOf(recordTimestamp). The grant's own advisory
// `[epoch_start, epoch_end]` window is honored against nowUnix exactly like
// KeyFor (defense-in-depth — independent of the nest's own fetch-time filter).
//
// factor is the bus factor the caller will compute over the opened record,
// matched exactly against each wrap's license (LicensesFactor): "" for a
// built-in perimeter factor, `labeler:<hex>` for a community labeler. A grant
// that licenses no labeler yields NO key for one, whatever the worklist
// names — the nest's word never widens a wrap.
//
// Two regimes, mutually exclusive within one grant (the mint policy's XOR,
// design § 2 — a mail grant never mixes a standing wrap with epoch wraps):
//
//   - Master-key (a wrap with Epoch == nil): matches any timestamp, exactly
//     KeyFor's shape — returned alone (a superset of any bounded key this
//     holder might also wield from a different grant for the same owner).
//   - Bounded (every wrap Epoch != nil): every held epoch <= target,
//     **nearest first** (target, then target-1 — the boundary/clock-skew
//     tolerance — then any older held epoch, the stale-published-schedule
//     case, design § 3 step 2). A held epoch > target is never a candidate —
//     the record cannot have been sealed under a key that didn't exist yet.
//
// The caller tries each returned key in order and lets AEAD/HPKE-open
// disambiguate — each miss is one cheap failed open. A nil/empty result is
// the fail-closed verdict: this holder cannot open a record from that
// epoch (never granted, revoked, or the record postdates the grant's held
// epoch set) — for mail that permanent darkness is the whole point of the
// bound (design § 8's tier_3 success bar).
func (s *GrantSet) KeyForMailEpoch(owner []byte, factor string, recordTimestamp, nowUnix uint64) [][]byte {
	if s == nil {
		return nil
	}
	target := mailfauna.MailSealingEpochOf(recordTimestamp)
	var master []byte
	var bounded []mailEpochCandidate
	for _, g := range s.byKey {
		if string(g.OwnerActorId) != string(owner) {
			continue
		}
		if nowUnix < g.EpochStart || nowUnix > g.EpochEnd {
			continue
		}
		for i := range g.Keys {
			k := &g.Keys[i]
			if k.Class != "content.read" || k.Kind == nil || *k.Kind != "mail" || !LicensesFactor(k, factor) {
				continue
			}
			switch {
			case k.Epoch == nil:
				master = k.Key
			case *k.Epoch <= target:
				bounded = append(bounded, mailEpochCandidate{epoch: *k.Epoch, key: k.Key})
			}
		}
	}
	if master != nil {
		return [][]byte{master}
	}
	sort.Slice(bounded, func(i, j int) bool { return bounded[i].epoch > bounded[j].epoch })
	out := make([][]byte, len(bounded))
	for i, c := range bounded {
		out[i] = c.key
	}
	return out
}

// Registry holds the bridge's current set of unwrapped capability grants and
// re-fetches them on a background cadence. Read lock-free via Current().
type Registry struct {
	x25519Sec []byte // 32-byte holder X25519 secret (the enrolled service-user key)
	// mlkemDk is the holder's 2400-byte ML-KEM-768 decapsulation key (PQ-CAP-2),
	// derived from the bridge's Ed25519 keyfile seed. Production always derives
	// it; nil only in tests (classical-only holder). Passed to the hybrid unseal so
	// the holder opens BOTH classical and X-Wing grant wraps with one path.
	// Zeroized at Close alongside x25519Sec.
	mlkemDk []byte
	caller  wsrpc.Caller
	logger  *slog.Logger

	// grants is the atomic-swapped live GrantSet. nil means "no successful fetch
	// yet". Current() reads it without a lock (the drain hot path).
	grants atomic.Pointer[GrantSet]

	// retired holds the GrantSet swapped OUT by the previous Refresh, so its
	// plaintext content keys are zeroized one refresh cycle later — on the next
	// Refresh (successful or transport-error) or at Close, whichever first —
	// once no Current() reader from before the previous swap can still
	// reference them (§ F12). Wiping on
	// the swap itself would race a drain that captured the old snapshot;
	// deferring one cycle bounds un-zeroized superseded key copies to one set.
	// A grant revoked this cycle still goes dark IMMEDIATELY (absent from the
	// new Current()); only the memory-wipe of its bytes is deferred one cycle.
	retired atomic.Pointer[GrantSet]

	refreshInterval time.Duration

	// afterFn is a test seam; production uses time.After.
	afterFn func(time.Duration) <-chan time.Time

	// fetchFn is the test seam for the `fauna.capabilities.fetch` RPC. Production
	// defaults to wsrpc.FetchCapabilityGrants over r.caller. Returning an error
	// is the TRANSIENT verdict (keep cached); returning a (possibly empty) blob
	// list is the AUTHORITATIVE verdict (replace the set).
	fetchFn func(ctx context.Context) ([][]byte, error)

	// unsealFn is the test seam for the FFI HPKE-Open step. Production uses
	// mailfauna.UnsealCapabilityGrant; tests substitute a stub mapping blob bytes
	// to a canned grant, so the divergent-revoke logic is verified without real
	// wrapped blobs + a matching secret. `mlkemDk` is the holder's ML-KEM decaps
	// key (nil for a classical-only bridge — the unseal then opens classical only).
	unsealFn func(blob []byte, secret []byte, mlkemDk []byte) (*mailfauna.CapabilityGrant, error)

	// hupCh receives SIGHUP (and TriggerRefresh nudges from tests). Coalesces
	// multiple HUPs into a single follow-up refresh.
	hupCh chan struct{}

	// refreshMu single-flights Refresh (and orders Close after any in-flight
	// Refresh). The one-cycle-deferred `retired` zeroize is sound only under
	// SERIALIZED swaps — two concurrent Refreshes could zeroize a set an
	// intervening Current() reader still holds — and Close's secret wipe must not race
	// a Refresh reading x25519Sec.
	refreshMu sync.Mutex
	// closed (under refreshMu) makes a post-Close Refresh a typed error
	// instead of an HPKE run against the zeroized secret.
	closed bool
	// loopWG tracks the Start goroutines so Close can WAIT for the loop to
	// exit — enforcing (not merely documenting) its "loop has exited"
	// precondition. Cancel Start's ctx before Close or Close blocks.
	loopWG sync.WaitGroup
}

// Config parameterises New.
type Config struct {
	// X25519Secret is the bridge's 32-byte holder secret (the enrolled
	// service-user X25519 private key — NOT the actor identity).
	X25519Secret []byte
	// MlkemDk is the holder's 2400-byte ML-KEM-768 decapsulation key (PQ-CAP-2),
	// derived from the bridge's Ed25519 keyfile seed via
	// faunaFfi.DeriveBridgeServiceUserMlkem768. nil/empty for a classical-only
	// bridge → grants open classical only. When set it MUST be 2400 bytes.
	MlkemDk []byte
	Caller  wsrpc.Caller
	Logger  *slog.Logger

	// RefreshInterval defaults to DefaultRefreshInterval when zero.
	RefreshInterval time.Duration

	// AfterFn is a test seam — production leaves it nil (time.After).
	AfterFn func(time.Duration) <-chan time.Time

	// FetchFn is a test seam — production leaves it nil and gets
	// wsrpc.FetchCapabilityGrants over Caller.
	FetchFn func(ctx context.Context) ([][]byte, error)

	// UnsealFn is a test seam — production leaves it nil and gets
	// mailfauna.UnsealCapabilityGrant.
	UnsealFn func(blob []byte, secret []byte, mlkemDk []byte) (*mailfauna.CapabilityGrant, error)
}

// New constructs a Registry. Does not perform I/O; the caller drives the first
// Refresh (synchronously before the consumer needs a capability) or lets the
// background loop do it.
func New(cfg Config) (*Registry, error) {
	if len(cfg.X25519Secret) != 32 {
		return nil, fmt.Errorf(
			"capability.New: X25519Secret must be 32 bytes, got %d",
			len(cfg.X25519Secret),
		)
	}
	if n := len(cfg.MlkemDk); n != 0 && n != 2400 {
		return nil, fmt.Errorf(
			"capability.New: MlkemDk must be 2400 bytes (ML-KEM-768 decaps key) or empty, got %d",
			n,
		)
	}
	logger := cfg.Logger
	if logger == nil {
		logger = slog.Default()
	}
	interval := cfg.RefreshInterval
	if interval <= 0 {
		interval = DefaultRefreshInterval
	}
	afterFn := cfg.AfterFn
	if afterFn == nil {
		afterFn = time.After
	}
	unsealFn := cfg.UnsealFn
	if unsealFn == nil {
		unsealFn = mailfauna.UnsealCapabilityGrant
	}

	// Copy the secret so the caller can zero its slice independently.
	secret := make([]byte, 32)
	copy(secret, cfg.X25519Secret)

	// Copy the ML-KEM dk too (nil stays nil → classical-only holder).
	var mlkemDk []byte
	if len(cfg.MlkemDk) == 2400 {
		mlkemDk = make([]byte, 2400)
		copy(mlkemDk, cfg.MlkemDk)
	}

	r := &Registry{
		x25519Sec:       secret,
		mlkemDk:         mlkemDk,
		caller:          cfg.Caller,
		logger:          logger,
		refreshInterval: interval,
		afterFn:         afterFn,
		unsealFn:        unsealFn,
		hupCh:           make(chan struct{}, 1),
	}

	fetchFn := cfg.FetchFn
	if fetchFn == nil {
		if cfg.Caller == nil {
			return nil, errors.New("capability.New: Caller is required when FetchFn is not supplied")
		}
		fetchFn = func(ctx context.Context) ([][]byte, error) {
			return wsrpc.FetchCapabilityGrants(ctx, r.caller)
		}
	}
	r.fetchFn = fetchFn
	return r, nil
}

// Refresh re-fetches the grants sealed to this holder and rebuilds the cache.
//
//   - TRANSPORT ERROR (fetchFn returns err): the cached set is left INTACT and
//     the error is returned. Revocation does NOT bite on a transport error —
//     keep operating on the last authoritative set.
//   - AUTHORITATIVE (fetchFn returns a blob list): the cache is REPLACED with
//     exactly the grants the nest served. A grant held before but absent now is
//     dropped (revoked/expired) and its keys are zeroized (one cycle later, via
//     retired). A present-but-unsealable grant is omitted (logged) but does NOT
//     abort the refresh or trigger keep-old — one bad grant must not block the
//     revocation of others.
//
// Safe for concurrent use: calls are SERIALIZED on refreshMu (single-flight),
// which is what keeps the one-cycle-deferred `retired` zeroize sound — the
// callers are the single background loop plus the drain's on-demand
// use-time refresh, and an unserialized pair could wipe a set a Current()
// reader still holds. After Close, Refresh
// returns a typed error rather than running against the zeroized secret.
func (r *Registry) Refresh(ctx context.Context) error {
	r.refreshMu.Lock()
	defer r.refreshMu.Unlock()
	if r.closed {
		return errors.New("capability.Refresh: registry closed")
	}
	blobs, err := r.fetchFn(ctx)
	if err != nil {
		// Transport error: keep the LIVE set (revocation must not bite on a
		// network blip) — but the RETIRED set is already ≥1 cycle old with no
		// possible reader, so wipe it now rather than letting a superseded
		// (possibly revoked) key linger un-zeroized through a persistent fetch
		// outage.
		zeroizeSet(r.retired.Swap(nil))
		return fmt.Errorf("fetch capability grants: %w", err)
	}

	newSet := &GrantSet{byKey: make(map[grantKey]*mailfauna.CapabilityGrant, len(blobs))}
	for _, blob := range blobs {
		grant, uerr := r.unsealFn(blob, r.x25519Sec, r.mlkemDk)
		if uerr != nil {
			// Present-but-unsealable: not a revoke, not a transport error — a
			// malformed/foreign grant. Omit it (go dark for its scope) without
			// aborting the refresh or keeping the old set.
			r.logger.Warn("capability.Refresh: skipping ungrantable grant", "err", uerr)
			continue
		}
		if grant == nil {
			continue
		}
		newSet.byKey[keyOf(grant)] = grant
	}

	old := r.grants.Swap(newSet)
	// Zeroize the set retired one cycle earlier (now provably free of any reader
	// that read Current() before the previous swap), then park THIS just-retired
	// set for the next swap to wipe. old / the parked set are nil on the first
	// refreshes; zeroizeSet no-ops on nil.
	zeroizeSet(r.retired.Swap(old))

	r.logger.Info(
		"capability.Refresh: grant set updated",
		"grants", newSet.Len(),
		"fetched_blobs", len(blobs),
	)
	return nil
}

// Current returns the live grant set. nil-safe to call and its methods are
// nil-safe, so a first-refresh-not-yet-succeeded holder reads as an empty set.
func (r *Registry) Current() *GrantSet {
	return r.grants.Load()
}

// Start spawns the background refresh loop. Returns immediately; the loop runs
// until ctx is cancelled. SIGHUP triggers an immediate refresh (tests use
// TriggerRefresh).
func (r *Registry) Start(ctx context.Context) {
	sigCh := make(chan os.Signal, 1)
	reloadsignal.Notify(sigCh)
	r.loopWG.Add(2)
	go func() {
		defer r.loopWG.Done()
		defer signal.Stop(sigCh)
		for {
			select {
			case <-ctx.Done():
				return
			case <-sigCh:
				r.signalHup()
			}
		}
	}()

	go func() {
		defer r.loopWG.Done()
		r.refreshLoop(ctx)
	}()
}

// signalHup pokes the hupCh non-blockingly; coalesces bursts.
func (r *Registry) signalHup() {
	select {
	case r.hupCh <- struct{}{}:
	default:
	}
}

// TriggerRefresh asks the loop to refresh on its next scheduling opportunity.
// Used by tests; production relies on SIGHUP delivery.
func (r *Registry) TriggerRefresh() {
	r.signalHup()
}

// refreshLoop is the long-running refresh goroutine. Exits on ctx cancel. Same
// busy-spin-proof shape as tls.Provider.refreshLoop — refreshsched.NextDelay
// never returns a zero delay, so a persistently-failing refresh retries at the
// backoff cap forever rather than spinning.
func (r *Registry) refreshLoop(ctx context.Context) {
	retryAttempt := 0

	for {
		wait := refreshsched.NextDelay(retryAttempt, r.refreshInterval, backoffSchedule)

		select {
		case <-ctx.Done():
			return
		case <-r.afterFn(wait):
		case <-r.hupCh:
			// SIGHUP — refresh now, resetting the failure streak so the
			// post-refresh cadence returns to the scheduled interval.
			retryAttempt = 0
		}

		refreshCtx, cancel := context.WithTimeout(ctx, 30*time.Second)
		err := r.Refresh(refreshCtx)
		cancel()
		if err != nil {
			retryAttempt++
			r.logger.Warn(
				"capability.refreshLoop: refresh failed, will retry",
				"attempt", retryAttempt,
				"err", err,
			)
			continue
		}
		retryAttempt = 0
	}
}

// Close zeroizes the cached X25519 secret and the retired grant set's content
// keys. It ENFORCES its ordering contract: it first WAITS for the Start
// goroutines to exit — so cancel Start's context before calling Close, or
// Close blocks — then takes refreshMu so no in-flight Refresh (e.g. a drain's
// use-time refresh) can race the wipe. A Refresh after Close returns a typed
// error. It deliberately does NOT wipe the LIVE set: a drain that captured
// the current snapshot may still be mid-use, and the live set is reclaimed by
// GC (or wiped when a later Refresh retires it). The retired set is at least
// one cycle old, so no reader references it.
func (r *Registry) Close() {
	r.loopWG.Wait()
	r.refreshMu.Lock()
	defer r.refreshMu.Unlock()
	r.closed = true
	for i := range r.x25519Sec {
		r.x25519Sec[i] = 0
	}
	for i := range r.mlkemDk {
		r.mlkemDk[i] = 0
	}
	zeroizeSet(r.retired.Swap(nil))
}

// zeroizeSet best-effort wipes every content key in a retired grant set. Only
// the per-scope Key bytes are secret — class/kind/tier/epoch/owner/grant_id are
// public audit metadata. nil-safe. Each UnsealCapabilityGrant mints fresh Key
// backing arrays, so wiping a retired set never touches a live one.
func zeroizeSet(s *GrantSet) {
	if s == nil {
		return
	}
	for _, g := range s.byKey {
		for i := range g.Keys {
			for j := range g.Keys[i].Key {
				g.Keys[i].Key[j] = 0
			}
		}
	}
}
