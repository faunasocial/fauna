// Package tls owns the bridge's runtime TLS-certificate state: fetching
// the wrapped TLS-cert blob from nest via WS-RPC, HPKE-Opening it with
// the bridge's X25519 secret via the fauna-ffi UniFFI surface, parsing
// the resulting PEM into a *crypto/tls.Certificate, caching it, and
// refreshing on a 12h timer (plus on SIGHUP).
//
// The exposed `*Provider.GetCertificate` callback is meant for
// crypto/tls.Config.GetCertificate so the SMTP listener (Phase C) plugs
// in directly: every TLS handshake reads the cached cert via an
// atomic.Pointer, never blocking on the refresh path.
//
// Refresh policy:
//
//   - First call to Refresh() is the caller's responsibility (main.go
//     does this synchronously before starting the listener so the first
//     handshake doesn't see a nil cert).
//   - Start() spawns a goroutine that re-Refreshes every refreshInterval
//     (default 12h — well below Let's Encrypt's 60-day rotation cadence).
//   - SIGHUP triggers an immediate Refresh.
//   - On Refresh error, the cached cert is NOT invalidated — the bridge
//     keeps serving the old cert until the next successful refresh.
//   - Failed refreshes retry on the capped backoff schedule below
//     (refreshsched.NextDelay); the streak resets once a refresh
//     succeeds. A persistent failure retries at the 15m cap forever — it
//     never busy-spins.
package tls

import (
	"context"
	crypto_tls "crypto/tls"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"os/signal"
	"sync/atomic"
	"time"

	"github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/refreshsched"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/reloadsignal"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// DefaultRefreshInterval is the steady-state cadence for scheduled
// refreshes. 12h is comfortably below Let's Encrypt's 60-day rotation
// cadence and gives a fresh cert plenty of time to propagate.
const DefaultRefreshInterval = 12 * time.Hour

// backoffSchedule names the per-failure delay before the next retry
// attempt after a Refresh error. Index by attempt-since-last-success
// (0 = first retry after a failure); index past len(backoffSchedule)-1
// uses the last entry until the next scheduled tick fires.
//
// Intentionally short tail (15m max): TLS-cert fetch is cheap and the
// scheduled-tick cadence (12h) means we won't dogpile nest. After 15m
// of failed retries, just wait for the next scheduled tick.
var backoffSchedule = []time.Duration{
	1 * time.Minute,
	5 * time.Minute,
	15 * time.Minute,
}

// Provider holds the bridge's runtime TLS state for one domain and one
// (role, bridge_id). A bridge serving multiple domains needs multiple
// Providers (one per domain, sharing the same wsrpc.Caller and
// x25519Secret). Phase C is single-domain-per-bridge; multi-domain
// support is a Phase E concern (Operator UX § Domain provisioning).
type Provider struct {
	domain    string
	role      string
	bridgeID  string
	x25519Sec []byte // 32-byte X25519 secret
	caller    wsrpc.Caller
	logger    *slog.Logger

	// cert is the atomic-swapped current *crypto_tls.Certificate. nil
	// means "not yet refreshed". GetCertificate reads this without a
	// lock — atomic.Pointer is concurrency-safe for the cert getter
	// path, which fires on every TLS handshake.
	cert atomic.Pointer[crypto_tls.Certificate]

	// refreshInterval is the scheduled cadence (defaults to
	// DefaultRefreshInterval; tests can override via the Config).
	refreshInterval time.Duration

	// afterFn is a test seam — production uses time.After; tests inject a
	// fake timer via Config.AfterFn.
	afterFn func(time.Duration) <-chan time.Time

	// hupCh is closed on Start; SIGHUP delivery is wired in Start so
	// the test can inject a synthetic signal via TriggerRefresh.
	hupCh chan struct{}
}

// Config parameterises New. Required: Domain, Role, BridgeID,
// X25519Secret, Caller. Logger defaults to slog.Default();
// RefreshInterval defaults to DefaultRefreshInterval.
//
// AfterFn is a test seam — production code leaves it nil and gets the
// real wall-clock timer (time.After).
type Config struct {
	Domain          string
	Role            string
	BridgeID        string
	X25519Secret    []byte // 32-byte X25519 private key
	Caller          wsrpc.Caller
	Logger          *slog.Logger
	RefreshInterval time.Duration

	AfterFn func(time.Duration) <-chan time.Time
}

// New constructs a Provider. Does not perform any I/O; the first
// Refresh() is the caller's responsibility (main.go does it
// synchronously at startup before opening the SMTP listener).
func New(cfg Config) (*Provider, error) {
	if cfg.Domain == "" {
		return nil, errors.New("tls.New: Domain is required")
	}
	if cfg.Role == "" {
		return nil, errors.New("tls.New: Role is required")
	}
	if cfg.BridgeID == "" {
		return nil, errors.New("tls.New: BridgeID is required")
	}
	if len(cfg.X25519Secret) != 32 {
		return nil, fmt.Errorf("tls.New: X25519Secret must be 32 bytes, got %d", len(cfg.X25519Secret))
	}
	if cfg.Caller == nil {
		return nil, errors.New("tls.New: Caller is required")
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

	// Copy the secret so the caller can zero its slice without
	// affecting our cached copy. We zeroize on Close (best-effort).
	secret := make([]byte, 32)
	copy(secret, cfg.X25519Secret)

	return &Provider{
		domain:          cfg.Domain,
		role:            cfg.Role,
		bridgeID:        cfg.BridgeID,
		x25519Sec:       secret,
		caller:          cfg.Caller,
		logger:          logger,
		refreshInterval: interval,
		afterFn:         afterFn,
		hupCh:           make(chan struct{}, 1),
	}, nil
}

// Refresh fetches the wrapped TLS-cert blob from nest and HPKE-Opens
// it via the FFI. On success, the cached cert is atomically swapped to
// the new value. On failure, the cached cert is left untouched.
//
// Safe for concurrent use; the cached cert is updated via
// atomic.Pointer.Store so the read path (GetCertificate) is never
// blocked. Multiple concurrent Refresh calls may all do the work,
// which is wasteful but not incorrect — the typical caller is the
// single refresh goroutine plus an occasional SIGHUP-driven manual
// refresh, so contention is rare.
func (p *Provider) Refresh(ctx context.Context) error {
	blob, err := wsrpc.FetchTLSCertBlob(ctx, p.caller, p.role, p.bridgeID, p.domain)
	if err != nil {
		return fmt.Errorf("fetch tls cert blob: %w", err)
	}
	if blob == nil {
		return fmt.Errorf("nest has no tls cert blob for (role=%s, bridge=%s, domain=%s)", p.role, p.bridgeID, p.domain)
	}

	bundle, err := fauna_ffi.UnsealTlsCertBlob(blob, p.x25519Sec)
	if err != nil {
		return fmt.Errorf("unseal tls cert blob: %w", err)
	}

	cert, err := crypto_tls.X509KeyPair(bundle.CertChain, bundle.PrivKey)
	if err != nil {
		return fmt.Errorf("parse tls cert PEM: %w", err)
	}

	p.cert.Store(&cert)
	p.logger.Info(
		"tls.Refresh: cert updated",
		"domain", p.domain,
		"role", p.role,
		"bridge_id", p.bridgeID,
		"issued_at", bundle.IssuedAt,
		"expires_at", bundle.ExpiresAt,
	)
	return nil
}

// Certificate returns the current cached cert. nil means "not yet
// refreshed". Safe for concurrent use.
func (p *Provider) Certificate() *crypto_tls.Certificate {
	return p.cert.Load()
}

// GetCertificate is suitable for crypto/tls.Config.GetCertificate. The
// SMTP listener (Phase C) hands this method as the callback so every
// handshake reads the cached cert.
//
// Returns an error if no cert is cached yet — the listener should
// arrange to call Refresh once synchronously before opening the
// socket, so this error path is only hit on the truly-degenerate
// "listener accepted before first refresh" race.
func (p *Provider) GetCertificate(_ *crypto_tls.ClientHelloInfo) (*crypto_tls.Certificate, error) {
	c := p.cert.Load()
	if c == nil {
		return nil, errors.New("tls.Provider: no cert cached (call Refresh first)")
	}
	return c, nil
}

// Start spawns the background refresh loop. Returns immediately; the
// loop runs until ctx is cancelled or Stop is called.
//
// The loop does NOT call Refresh on entry — main.go is expected to do
// the first refresh synchronously, before starting the listener, so a
// nil cert never surfaces on the handshake path. After ctx is
// cancelled the loop exits cleanly within one timer tick.
//
// SIGHUP delivery is wired here; sending SIGHUP to the process
// triggers an immediate Refresh. Tests can synthesize the same effect
// via TriggerRefresh.
func (p *Provider) Start(ctx context.Context) {
	// Wire SIGHUP — note that os/signal.Notify owns the channel, so
	// we attach a separate sighup channel that fans into hupCh.
	sigCh := make(chan os.Signal, 1)
	reloadsignal.Notify(sigCh)
	go func() {
		defer signal.Stop(sigCh)
		for {
			select {
			case <-ctx.Done():
				return
			case <-sigCh:
				p.signalHup()
			}
		}
	}()

	go p.refreshLoop(ctx)
}

// signalHup pokes the hupCh non-blockingly. Multiple HUPs arriving
// while a refresh is in flight coalesce into a single follow-up
// refresh, which is what we want — there's nothing to gain from
// re-refreshing twice back-to-back.
func (p *Provider) signalHup() {
	select {
	case p.hupCh <- struct{}{}:
	default:
		// Channel already has a pending signal — coalesce.
	}
}

// TriggerRefresh asks the refresh loop to do a Refresh on the next
// scheduling opportunity. Used by tests; production code relies on
// SIGHUP delivery instead.
func (p *Provider) TriggerRefresh() {
	p.signalHup()
}

// refreshLoop is the long-running refresh goroutine. Exits when ctx
// is cancelled.
//
// retryAttempt counts consecutive failures since the last success. The
// sleep before each attempt is a pure function of that count
// (refreshsched.NextDelay): the steady-state interval after a success,
// or a capped backoff (1m/5m/15m) on failure. It is never zero, so a
// persistently-failing refresh retries at the 15m cadence forever rather
// than busy-spinning — the bug that flooded a 69 GB json log and filled
// example.com's disk on 2026-06-01.
func (p *Provider) refreshLoop(ctx context.Context) {
	retryAttempt := 0

	// If the synchronous boot refresh (main.go) did not land a cert, the
	// listeners are already bound (465/587/993) but GetCertificate errors on
	// every handshake (TLSV1_ALERT_INTERNAL_ERROR) until one is cached. This is
	// the expected fresh-box race: the bridge is still pending approval, or its
	// x25519 isn't yet on the nest row (main.go's soft first-refresh failure).
	// Start in the failure-backoff cadence (1m) rather than the 12h steady-state
	// interval so the gap self-heals within a minute — exactly what main.go's
	// "will retry on backoff schedule" warning promises — instead of stranding
	// SMTPS/IMAPS TLS for up to 12h waiting on a config_changed / SIGHUP poke.
	// Backoff is >0 and capped at 15m (NextDelay), so no busy-spin: the 69 GB
	// disk-fill class stays impossible.
	if p.cert.Load() == nil {
		retryAttempt = 1
	}

	for {
		wait := refreshsched.NextDelay(retryAttempt, p.refreshInterval, backoffSchedule)

		select {
		case <-ctx.Done():
			return
		case <-p.afterFn(wait):
			// fall through to refresh
		case <-p.hupCh:
			// SIGHUP — refresh now, resetting the failure streak so the
			// post-refresh cadence returns to the scheduled interval.
			retryAttempt = 0
		}

		refreshCtx, cancel := context.WithTimeout(ctx, 30*time.Second)
		err := p.Refresh(refreshCtx)
		cancel()
		if err != nil {
			retryAttempt++
			p.logger.Warn(
				"tls.refreshLoop: refresh failed, will retry",
				"domain", p.domain,
				"attempt", retryAttempt,
				"err", err,
			)
			continue
		}
		retryAttempt = 0
	}
}

// Close zeroes the cached X25519 secret. After Close, Refresh calls
// will produce garbage results (the FFI's secret_32 length check will
// pass but HPKE-Open will reject the wrong key) — callers should only
// call Close after the refresh loop has exited (cancel its context
// first).
func (p *Provider) Close() {
	for i := range p.x25519Sec {
		p.x25519Sec[i] = 0
	}
}
