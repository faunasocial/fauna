package imap

import (
	"log/slog"
	"net"
	"sync/atomic"
	"time"

	"github.com/emersion/go-imap/v2/imapserver"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/authlock"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/byteplane"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/undecryptable"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// authFailureWindow is the fixed window the per-(credential, source-IP)
// AUTH-failure lockout counts within — 1 minute, matching the submission
// MTA (mta.config_holder) and the `mail.auth.max_auth_failures_per_minute`
// catalog row (mail-policy-config.md § Submission policy).
const authFailureWindow = time.Minute

// pushHandlerInstaller is the optional capability the Backend probes for
// on the wsrpc.Caller it receives. The production *wsrpc.Client
// satisfies it (via SetOnPush — added in Phase F.2 to swap the push
// handler atomically post-Dial); tests using a plain `recordingCaller`
// don't satisfy it and the Backend skips the install, which is fine
// because tests that need IDLE plumbing construct the router by hand
// and call `notificationRouter.Handle` directly.
type pushHandlerInstaller interface {
	SetOnPush(h wsrpc.PushHandler)
}

// backend is the production imap.Backend — one instance per MDA
// process, shared across both the 993 and 143 listeners. Holds the
// long-lived WS-RPC Caller + logger + cross-session BODYSTRUCTURE
// cache every Session shares, plus the F.2 notification router every
// Session.Idle registers against.
//
// Concrete type is unexported; consumers receive a Backend interface
// (defined in server.go). Tests construct fakeBackend directly.
type backend struct {
	client wsrpc.Caller
	logger *slog.Logger
	cache  *bodyStructureCache
	router *notificationRouter
	// undecryptableWarn dedups the FETCH path's "record could not be
	// opened" WARN to once per record across every Session.
	undecryptableWarn undecryptable.WarnDedup
	// plane is the nest's bulk-byte-plane client, off which an
	// oversized sealed body is fetched by reference (SealedBodyOf).
	// Snapshotted into every Session at NewSession. nil on a box with no
	// NestBaseURL wired (and in tests) — harmless until a fetch reply
	// actually carries a BodyRef, which then fails closed.
	plane *byteplane.Client
	// idleTimeoutNanos is the per-server IDLE timeout (RFC 2177 §3) as
	// nanoseconds, held atomically so a `fauna.bridges.config_changed`
	// re-fetch can hot-swap it (ApplyConfig) without a restart. NewSession
	// reads the current value, so a connection opened after an admin
	// `put_imap_policy` edit picks up the new timeout at that request
	// boundary; in-flight sessions keep their copy (mail-bridge-lifecycle.md
	// § Running — hot-reload mandatory, applies at the next request boundary).
	idleTimeoutNanos atomic.Int64
	// authLockout is the per-(credential, source-IP) AUTH-failure brake
	// (security review § D4/M1), held behind an atomic pointer so a
	// `config_changed` re-fetch can hot-swap the failure ceiling
	// (ApplyConfig) without a restart. NewSession snapshots the current
	// instance into each Session; a swap that lands mid-session takes
	// effect on the next connection. Mirrors the MTA's
	// config_holder.authLockout — a rebuild resets the in-flight counters,
	// acceptable on a rare admin-driven config edit.
	authLockout atomic.Pointer[authlock.Lockout]
	// primaryDomain is the box's PrimaryDomain (ConfigSnapshot.PrimaryDomain),
	// seeded at boot via SetPrimaryDomain and hot-swapped by ApplyConfig on a
	// `config_changed` re-fetch. NewSession snapshots it into each Session so a
	// bare username (no `@domain`) resolves under it — uniform with the CalDAV
	// + MTA AUTH surfaces. Empty/nil ⇒ strict `user@domain` (the value tests
	// leave unset, so their full-email auths are unaffected).
	primaryDomain atomic.Pointer[string]
	// spamPolicy is the snapshot-derived per-user-scorer config (the
	// `spam_folder` threshold the SELECT-time scoring pass routes
	// INBOX→Junk against — spam_score.go). Seeded at boot via
	// SetSpamPolicy and hot-swapped by ApplyConfig on a `config_changed`
	// re-fetch, mirroring the MTA's config_holder.spamPolicy so both
	// scoring positions read the same admin-effective thresholds.
	// NewSession snapshots it into each Session; nil ⇒ the permissive
	// catalog default (currentSpamPolicy).
	spamPolicy atomic.Pointer[mailfauna.SpamPolicy]
	// bayesianKnobs is the snapshot-derived per-user-scorer confidence-ramp /
	// weight config (the Tier-2 `mail.spam.bayesian_*` knobs the SELECT-time
	// scoring pass feeds to the shared scorer). Seeded at boot via
	// SetBayesianKnobs and hot-swapped by ApplyConfig on a `config_changed`
	// re-fetch, in lockstep with spamPolicy. NewSession snapshots it into each
	// Session; nil ⇒ the catalog default (currentBayesianKnobs).
	bayesianKnobs atomic.Pointer[mailfauna.BayesianKnobs]
}

// currentBayesianKnobs reads the hot-swappable per-user-scorer confidence /
// weight knobs, defaulting to the catalog values (700/50/200) when unset.
// Production seeds the admin-effective knobs via SetBayesianKnobs in mda.Run;
// the default only applies to tests that don't seed it.
func (b *backend) currentBayesianKnobs() mailfauna.BayesianKnobs {
	if k := b.bayesianKnobs.Load(); k != nil {
		return *k
	}
	return mailfauna.DefaultBayesianKnobs()
}

// SetBayesianKnobs seeds the per-user-scorer confidence / weight knobs from the
// boot snapshot (mda.Run). ApplyConfig keeps them current on a `config_changed`
// re-fetch, in lockstep with SetSpamPolicy.
func (b *backend) SetBayesianKnobs(k mailfauna.BayesianKnobs) {
	kk := k
	b.bayesianKnobs.Store(&kk)
}

// currentSpamPolicy reads the hot-swappable per-user-scorer policy,
// defaulting to the permissive catalog thresholds (spam_folder=5,
// reject=0) when unset. Production always seeds the real
// admin-effective policy via SetSpamPolicy in mda.Run before accepting
// connections; the default only applies to tests that don't seed it.
func (b *backend) currentSpamPolicy() mailfauna.SpamPolicy {
	if p := b.spamPolicy.Load(); p != nil {
		return *p
	}
	return mailfauna.SpamPolicyFromSnapshot(wsrpc.DefaultSpamPolicyThresholds(), wsrpc.DefaultAuthPolicy())
}

// SetSpamPolicy seeds the per-user-scorer policy from the boot snapshot
// (mda.Run). ApplyConfig keeps it current on a `config_changed` re-fetch.
func (b *backend) SetSpamPolicy(p mailfauna.SpamPolicy) {
	pp := p
	b.spamPolicy.Store(&pp)
}

// currentPrimaryDomain reads the hot-swappable primary domain, defaulting to
// "" (strict `user@domain`) when unset.
func (b *backend) currentPrimaryDomain() string {
	if p := b.primaryDomain.Load(); p != nil {
		return *p
	}
	return ""
}

// SetPrimaryDomain seeds the box's primary domain from the boot snapshot
// (mda.Run). ApplyConfig keeps it current on a `config_changed` re-fetch.
func (b *backend) SetPrimaryDomain(domain string) {
	d := domain
	b.primaryDomain.Store(&d)
}

// NewBackend returns a Backend that manufactures fresh Sessions
// sharing the given WS-RPC client + logger + BODYSTRUCTURE LRU cache
// + F.2 notification router. `cacheMax` is the cap on the cache (0
// disables it entirely; sized from `cfg.IMAP.BodyStructureCacheMax`
// in production via mda.Run). `idleTimeout` is the per-server IDLE
// timeout from `cfg.IMAP.IdleTimeoutSecs` (RFC 2177 §3; default 29
// min per imap-server.md § IDLE — caller converts seconds to
// time.Duration). Zero or negative falls back to a sane default
// inside Session.Idle.
//
// The client MUST be safe for concurrent use across sessions (the I4
// Phase B.4 *wsrpc.Client is). `dispatcher` is the seam through which
// IDLE/NOTIFY receive `BridgeMailboxState` pushes from nest: when non-nil
// (production, via mda.Run), the router's `Handle` is registered on it
// under `BridgeMailboxStatePushKind` — the dispatcher is the single push
// handler the process installs, so config_changed can be composed
// alongside. When nil, the router falls back to grabbing the client's push
// slot directly via `pushHandlerInstaller` (the standalone single-consumer
// path; tests that drive `router.Handle` by hand pass neither).
//
// `maxAuthFailuresPerMinute` seeds the per-(credential, source-IP)
// AUTH-failure lockout from `Snapshot.Auth.MaxAuthFailuresPerMinute`
// (catalog default 30; `0` disables the gate — the value tests pass).
// ApplyConfig rebuilds it on a `config_changed` re-fetch.
//
// `plane` is the bulk-byte-plane client every serve-path body read resolves a
// by-reference body through (SealedBodyOf). mda.Run builds it from the same
// NestBaseURL the WS-RPC dial uses; nil is legal (no byte plane wired / tests).
func NewBackend(client wsrpc.Caller, logger *slog.Logger, cacheMax int, idleTimeout time.Duration, maxAuthFailuresPerMinute uint32, dispatcher *wsrpc.PushDispatcher, plane *byteplane.Client) Backend {
	if client == nil {
		panic("imap.NewBackend: client must not be nil")
	}
	if logger == nil {
		logger = slog.Default()
	}
	router := newNotificationRouter(logger.With("component", "notification_router"))
	switch {
	case dispatcher != nil:
		// Composed install: the MDA process consumes more than one push kind
		// (mailbox_state here + config_changed for hot-reload), so a kind-routed
		// dispatcher is the single installed handler. Register the mailbox-state
		// route on it; mda.Run registers config_changed and installs the
		// dispatcher via SetOnPush.
		dispatcher.Register(wsrpc.BridgeMailboxStatePushKind, router.Handle)
	default:
		// Standalone install (no dispatcher → router is the only consumer):
		// grab the push slot directly when the client supports it. Tests that
		// drive router.Handle by hand pass a Caller that doesn't satisfy
		// pushHandlerInstaller, so the install is skipped.
		if installer, ok := client.(pushHandlerInstaller); ok {
			installer.SetOnPush(router.Handle)
		}
	}
	b := &backend{
		client: client,
		logger: logger,
		cache:  newBodyStructureCache(cacheMax),
		router: router,
		plane:  plane,
	}
	b.idleTimeoutNanos.Store(int64(idleTimeout))
	b.authLockout.Store(authlock.New(maxAuthFailuresPerMinute, authFailureWindow, nil))
	return b
}

func (b *backend) NewSession(c *imapserver.Conn) imapserver.Session {
	// Host part of the TCP RemoteAddr — the real client IP (993/143 are
	// published directly). Keys the AUTH lockout + the report_auth_event
	// audit row. A malformed/nil RemoteAddr leaves it empty; the lockout
	// degrades to a username-only key and report_auth_event would reject
	// the empty IP, but a real accepted TCP conn always has a RemoteAddr.
	var sourceIP string
	if c != nil {
		if nc := c.NetConn(); nc != nil {
			if host, _, err := net.SplitHostPort(nc.RemoteAddr().String()); err == nil {
				sourceIP = host
			}
		}
	}
	return &Session{
		conn:              c,
		client:            b.client,
		logger:            b.logger,
		sourceIP:          sourceIP,
		lockout:           b.authLockout.Load(),
		primaryDomain:     b.currentPrimaryDomain(),
		cache:             b.cache,
		undecryptableWarn: &b.undecryptableWarn,
		router:            b.router,
		plane:             b.plane,
		idleTimeout:       time.Duration(b.idleTimeoutNanos.Load()),
		spamPolicy:        b.currentSpamPolicy(),
		bayesianKnobs:     b.currentBayesianKnobs(),
	}
}

// ApplyConfig hot-swaps the backend's config-derived knobs from a freshly
// fetched snapshot, so a `fauna.bridges.config_changed` re-fetch (or a
// reconnect re-fetch) takes effect at the next request boundary without a
// restart. Registered with the wsrpc.ConfigReloader in mda.Run. Re-applies:
//   - the IMAP IDLE timeout (read per-IDLE by the fork's handleIdle via the
//     session's IdleTimeout(); see idle.go / FORK.md row 19);
//   - the BODYSTRUCTURE cache cap (Resize evicts the LRU tail down to the new
//     cap immediately).
//
// Concurrency-safe: idle timeout is an atomic store NewSession reads, and the
// cache cap is the cache's own atomic + mutexed LRU surgery.
func (b *backend) ApplyConfig(snap wsrpc.ConfigSnapshot) {
	b.idleTimeoutNanos.Store(int64(time.Duration(snap.IMAP.IdleTimeoutSecs) * time.Second))
	b.cache.Resize(int(snap.IMAP.BodyStructureCacheMax))
	// Rebuild the AUTH-failure lockout from the fresh ceiling. A new
	// instance resets the in-flight counters (mirrors the MTA); acceptable
	// on a rare admin config edit. NewSession picks it up on the next conn.
	b.authLockout.Store(authlock.New(snap.Auth.MaxAuthFailuresPerMinute, authFailureWindow, nil))
	b.SetPrimaryDomain(snap.PrimaryDomain)
	b.SetSpamPolicy(mailfauna.SpamPolicyFromSnapshot(snap.Spam, snap.Auth))
	b.SetBayesianKnobs(mailfauna.BayesianKnobsFromSnapshot(snap.Spam))
	b.logger.Info("imap backend hot-applied config",
		"idle_timeout_secs", snap.IMAP.IdleTimeoutSecs,
		"bodystructure_cache_max", snap.IMAP.BodyStructureCacheMax,
		"max_auth_failures_per_minute", snap.Auth.MaxAuthFailuresPerMinute,
		"primary_domain", snap.PrimaryDomain,
		"spam_folder_threshold", snap.Spam.MaxScoreBeforeSpamFolder,
		"bayesian_full_confidence_samples", snap.Spam.BayesianFullConfidenceSamples,
	)
}
