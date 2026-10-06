package mda

import (
	"context"
	"fmt"
	"log/slog"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/capability"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// capabilityInitialRefreshTimeout bounds the best-effort synchronous initial
// grant fetch at startup (mirrors the MDA's TLS-provider initial Refresh
// timeout). A failure is soft — the background loop retries on backoff.
const capabilityInitialRefreshTimeout = 30 * time.Second

// capHolderDeps carries what starting the MDA's capability holder loop needs.
// caller + secret + logger + reloader are the production inputs; fetchFn /
// unsealFn / afterFn are test seams (production leaves them nil → the real
// wsrpc fetch transport, FFI HPKE-Open, and 12 h backstop timer).
type capHolderDeps struct {
	// caller is the authenticated WS-RPC caller to nest (the MDA's
	// reconnector). fetch is holder = the authenticated caller, so there is
	// nothing to spoof.
	caller wsrpc.Caller
	// secret is the bridge's enrolled service-user X25519 private key (the same
	// 32-byte secret the TLS blob uses, NOT the actor identity). Absent/short → the
	// holder is a no-op.
	secret []byte
	// mlkemDk is the holder's 2400-byte ML-KEM-768 decapsulation key (PQ-CAP-2),
	// nil for a classical-only bridge. Threaded into capability.Config so the
	// holder opens hybrid (X-Wing) grant wraps in addition to classical ones.
	mlkemDk []byte
	logger  *slog.Logger
	// reloader, when non-nil, is the MDA's config-changed reloader; the holder
	// registers a refresh on it so a nest-side mint/revoke bites at use-time.
	reloader *wsrpc.ConfigReloader

	// Test seams — production leaves these nil.
	fetchFn  func(ctx context.Context) ([][]byte, error)
	unsealFn func(blob []byte, secret []byte, mlkemDk []byte) (*mailfauna.CapabilityGrant, error)
	afterFn  func(time.Duration) <-chan time.Time
}

// startCapabilityHolder wires + starts the MDA's capability holder loop
// (capability-mediated-content-processing design § 2.4: the MDA keeps its
// legacy mail-serve role AND becomes a capability holder). It fetches +
// HPKE-Opens the content grants the user minted to this bridge's enrolled
// x25519 pubkey, so a re-score / re-index drain (Slice 5A) can transiently
// wield a scoped content key on an untrusted box. The wrapped keys are the
// minimal derived per-kind subset — never MSEK / identity (key-material-
// hierarchy.md rule #7); scope is crypto-self-enforcing (design § 2.2), so the
// grant, not the coarse fetch gate, is the confidentiality boundary.
//
// Returns (nil, nil) — a no-op — when the secret is absent/short (localhost /
// pre-enrollment); a later restart
// post-enrollment wires the holder. On success the returned Registry's refresh
// loop runs under ctx; the holder is refreshed best-effort once synchronously
// (soft failure — the loop retries on backoff, like the TLS provider's initial
// Refresh) and again on every config_changed push, so a nest-side mint/revoke
// takes effect without waiting for the 12 h backstop — a revoke bites at
// use-time (design § 2.3, honest-box revocation).
//
// Teardown is the caller's: cancel ctx (stopping the loop) THEN call Close()
// (zeroize) — Close's contract requires the loop to have exited first.
func startCapabilityHolder(ctx context.Context, deps capHolderDeps) (*capability.Registry, error) {
	if len(deps.secret) != 32 {
		// No enrolled holder secret yet (localhost / pre-enrollment): serve
		// mail without holding capabilities.
		return nil, nil
	}
	logger := deps.logger
	if logger == nil {
		logger = slog.Default()
	}

	reg, err := capability.New(capability.Config{
		X25519Secret: deps.secret,
		MlkemDk:      deps.mlkemDk,
		Caller:       deps.caller,
		Logger:       logger,
		FetchFn:      deps.fetchFn,
		UnsealFn:     deps.unsealFn,
		AfterFn:      deps.afterFn,
	})
	if err != nil {
		return nil, fmt.Errorf("capability.New: %w", err)
	}

	// Best-effort initial fetch so grants are available promptly (the drain
	// still refreshes on-demand before wielding one). Soft failure — the
	// background loop retries on backoff (mirrors the TLS provider's soft
	// initial Refresh); a pre-approval bridge whose x25519 isn't on the nest
	// row yet simply fetches nothing until it is.
	refreshCtx, cancel := context.WithTimeout(ctx, capabilityInitialRefreshTimeout)
	if err := reg.Refresh(refreshCtx); err != nil {
		logger.Warn("initial capability-grant refresh failed (will retry on backoff schedule)", "err", err)
	}
	cancel()

	reg.Start(ctx)

	// Refresh on any config_changed so a nest-side mint/revoke takes effect
	// without waiting for the 12 h backstop — a revoke bites at use-time
	// (design § 2.3). TriggerRefresh is a cheap coalesced async poke; re-fetch
	// on ANY config_changed (not a reason token) — a coalesced poke that
	// needs no reason parsing — mirroring the MDA's TLS TriggerRefresh registration.
	if deps.reloader != nil {
		deps.reloader.Register(func(_ wsrpc.ConfigSnapshot) {
			reg.TriggerRefresh()
		})
	}

	return reg, nil
}
