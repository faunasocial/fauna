// Package mta is the MTA role's main loop.
//
// This is the role the bridge runs when `fauna.bridges.whoami` resolves
// its role to `"mta"`: external-MX inbound (port 25, Phase C) plus
// authenticated submission (ports 465/587, Phase D). Both listeners
// terminate TLS using the bridge's `*tls.Provider` and route message
// disposition through nest's WS-RPC surface (`validate_recipient`,
// `ingest_inbound_mail`, `submit_inbound_mail`, …).
//
// The role-dispatch in main.go calls one of mta.Run / mda.Run after
// Whoami; Run idles (blocking on `<-ctx.Done()`) while mail is disabled or
// no domain exists, and otherwise brings up the SMTP listener. The Deps
// struct names every dependency the listener needs.
package mta

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"sync"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/bridgeshutdown"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/byteplane"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/connlimit"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/logplane"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/scan"
	bridgetls "github.com/faunasocial/fauna/bins/fauna-bridges/internal/tls"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// configReloadFetchTimeout bounds the fetch_config round-trip the config
// reloader issues when a `fauna.bridges.config_changed` push (or a reconnect)
// prompts a re-fetch. Generous — a slow re-fetch keeps the last-good config
// until it lands. Mirrors mda.go's same-named const.
const configReloadFetchTimeout = 30 * time.Second

// Deps is the dependency-injection bundle main passes to Run. Names
// every dependency the future MTA listener will need (Phases C/D)
// even though Phase B.8's Run only consumes Logger; this stabilises
// the surface so main.go's wire-up doesn't churn when the real
// listener lands.
type Deps struct {
	// Snapshot is the bridge's current config snapshot (from
	// `fauna.bridges.fetch_config`). Carries Spam thresholds,
	// auth-policy flags, the active LocalDomains list, and the
	// PrimaryDomain anchor. The MailEnabled field gates listener
	// startup — see MailEnabled() below.
	Snapshot wsrpc.ConfigSnapshot
	// TLSProvider serves the live TLS cert via GetCertificate. May
	// be nil when the deployment hasn't yet provisioned a TLS cert
	// for snapshot.PrimaryDomain; Run handles nil gracefully by
	// deferring listener startup until a cert is available.
	TLSProvider *bridgetls.Provider
	// Client is the WS-RPC caller to nest. Phase C/D call
	// validate_recipient / ingest_inbound_mail / submit_inbound_mail
	// through it. Typed as the Caller interface so the production process
	// passes a *wsrpc.ReconnectingClient (survives nest blips,
	// mail-bridge-lifecycle.md § Reconnecting); a plain *wsrpc.Client also
	// satisfies it (tests / non-reconnecting callers).
	Client wsrpc.Caller
	// Logger is the per-role structured logger (role="mta" attribute
	// already attached). Subsystems may add per-listener / per-RCPT
	// attributes via Logger.With.
	Logger *slog.Logger
	// BridgeID is the deployment-time bridge identity nest resolved
	// via Whoami. The submission-token fetch uses this as a key.
	BridgeID string
	// MTABindAddr is the host:port the SMTP MX listener binds to.
	// Defaulted to ":25" in main.go; tests use "127.0.0.1:0" for an
	// ephemeral local port. Reserved for deployment-topology values
	// only (a product invariant: nest holds policy, the
	// CLI/operator holds topology) — see also `MetricsBindAddr` on
	// the operator-hatch for the equivalent metrics-listener knob.
	MTABindAddr string
	// MTABindAddr465 is the host:port for the implicit-TLS submission
	// listener (Phase D.1). Defaulted to ":465" in main.go; tests use
	// "127.0.0.1:0" for an ephemeral local port. Same topology-only
	// invariant as MTABindAddr.
	MTABindAddr465 string
	// MTABindAddr587 is the host:port for the STARTTLS-required
	// submission listener (Phase D.1). Defaulted to ":587" in main.go;
	// tests use "127.0.0.1:0" for an ephemeral local port.
	MTABindAddr587 string
	// OutboundWorkerConfig controls the per-bridge outbound delivery
	// worker spawned by Run. Zero-value fields default to production
	// values (see OutboundWorkerConfig.fillDefaults). Tests can
	// override PollInterval / BatchSize / MaxAttempts.
	OutboundWorkerConfig OutboundWorkerConfig
	// MXResolver overrides the outbound worker's MX resolution. Nil
	// (the production default unless an operator-hatch `mta_mx_override`
	// is set) means LiveMXResolver — DNS MX lookups. main.go supplies an
	// OverrideMXResolver when the operator-hatch carries a static
	// transport route (split-horizon / air-gapped relay; test loopback
	// stub MX). See internal/config.OperatorHatch.MTAMXOverride.
	MXResolver MXResolver
	// ScanConfig is the T1.4 content-scan config (clamd / rspamd addresses
	// from the operator-hatch + the scan policy). Zero-value Policy disables
	// both scanners (the gate no-ops); main.go fills the production defaults
	// (scan.PolicyDefault + the co-located daemon addresses).
	ScanConfig scan.Config
	// ShutdownGrace is the graceful-shutdown drain budget (T2.6), sourced
	// from snapshot.Bridge.ShutdownGraceSeconds (catalog
	// mail.bridge.shutdown_grace_seconds, default 30 s). On ctx cancel each
	// listener stops accepting, answers 421 4.3.2 on new MAIL FROM, drains
	// in-flight transactions up to this budget, then force-closes. Zero ⇒
	// immediate force-close (an admin who set the knob to 0); a zero-value
	// Deps in a test that doesn't exercise shutdown gets immediate close too.
	ShutdownGrace time.Duration

	// PublishConfigReloader, if non-nil, is called once with the
	// wsrpc.ConfigReloader Run builds, so main.go can route its reconnect
	// re-fetch through the same Apply seam as the config_changed push (a
	// config changed during a WS gap then hot-applies on reconnect, not only on
	// the next push). Optional — nil in tests / standalone. The reloader is
	// goroutine-safe (Apply takes the RWMutex), so the cross-goroutine handoff
	// is sound; main.go holds it in an atomic.Pointer. Mirrors mda.Deps.
	PublishConfigReloader func(*wsrpc.ConfigReloader)

	// NestBaseURL / NestHTTPClient reach the nest's bulk-byte plane
	// (`POST /api/v1/chunks`), where the MTA stages a sealed body too large for
	// the 2 MiB WS-RPC frame; the RPC then carries only the chunk hashes
	// (smtp-server.md § Message size limits). A `ws(s)://` endpoint is accepted
	// verbatim — byteplane.New normalizes the scheme.
	//
	// Both empty/nil (a unit test, or a bridge with no HTTP reach) ⇒ no byte-plane
	// client is built and an over-frame message tempfails rather than delivering.
	// Every message that fits the frame is unaffected. Mirrors mda.Deps.
	NestBaseURL    string
	NestHTTPClient *http.Client
}

// Run is the MTA role's main loop.
//
// Phase C.1 binds an SMTP MX listener on deps.MTABindAddr when both
// gates are open: mail is enabled in the snapshot and nest has
// ratified a domain. With either gate closed, Run idles — this matches
// the product invariant of working out-of-the-box: admin disables a
// feature, the bridge gracefully no-ops — and lets the supervisor
// (s6 / systemd) gate process lifetime on the same admin signal
// without races between the snapshot change and the process tear-down.
//
// While idling it stays SUBSCRIBED (wsrpc.IdleUntilGatesOpen): a
// `config_changed` push (or a reconnect re-fetch) that opens both gates
// ends the idle and Run returns nil → exit 0 → the supervisor rebinds a
// process that cold-boots against the new snapshot and binds. Both gates
// share the one watcher, so an admin's `add_local_domain` on an
// enabled-but-domainless box brings the MTA live the same way an enable
// does — the case mail-bridge-lifecycle.md § Default-off called out as
// "invisible until they restart" (its nest-boot safety net stays: this
// closes the window, it does not replace the belt).
//
// Phase D will add the submission listeners on 465 + 587 alongside
// this one. Phase C/D both call Run on the supplied context, so a
// SIGTERM-driven cancel from main.go propagates through the
// listener goroutines.
//
// Returns nil on clean shutdown (ctx.Done() observed) or when an
// idle-gate kept the listener un-bound. Returns an error if the
// listener fails to bind; main.go propagates that to its exit code.
func Run(ctx context.Context, deps Deps) error {
	logger := deps.Logger
	if logger == nil {
		logger = slog.Default()
	}
	if !MailEnabled(deps.Snapshot) {
		logger.Info("mta.Run idling: mail disabled by admin config",
			"local_domains_count", len(deps.Snapshot.LocalDomains),
			"primary_domain", deps.Snapshot.PrimaryDomain,
			"bridge_id", deps.BridgeID,
		)
		return idleUntilBindable(ctx, deps, logger)
	}
	if len(deps.Snapshot.LocalDomains) == 0 {
		logger.Info("mta.Run idling: no mail_domains rows yet",
			"bridge_id", deps.BridgeID,
			"hint", "admin must add a primary domain from the Fauna app (fauna.bridges.add_local_domain)",
		)
		return idleUntilBindable(ctx, deps, logger)
	}
	if deps.MTABindAddr == "" {
		return fmt.Errorf("mta.Run: MTABindAddr is empty (main.go should default to :25)")
	}
	// Per docs/goal/behavior/smtp-server.md § Implementation status today:
	// port 25 binds regardless of TLS state (target shape — opportunistic
	// STARTTLS when TLS is available, plaintext-only when it isn't);
	// submission listeners 465/587 genuinely need TLS at bind. When the
	// admin hasn't yet provisioned an admin-uploaded TLS cert (or ACME
	// hasn't run), the bridge still accepts inbound MX traffic on 25 and
	// keeps submission unavailable until TLS arrives. The 465/587 bind
	// args may be empty in that case — they're unused until TLS lands.
	tlsAvailable := deps.TLSProvider != nil
	if tlsAvailable {
		if deps.MTABindAddr465 == "" {
			return fmt.Errorf("mta.Run: MTABindAddr465 is empty (main.go should default to :465)")
		}
		if deps.MTABindAddr587 == "" {
			return fmt.Errorf("mta.Run: MTABindAddr587 is empty (main.go should default to :587)")
		}
	}
	var tlsConfig *tls.Config
	if tlsAvailable {
		tlsConfig = &tls.Config{
			GetCertificate: deps.TLSProvider.GetCertificate,
			MinVersion:     tls.VersionTLS12,
		}
	}

	// Bind port 25 unconditionally. When TLS is available, also bind the
	// two submission listeners; if any submission bind fails, close the
	// ones already grabbed (including port 25) so we don't leak fds. The
	// order matches port number for log readability.
	port25, err := net.Listen("tcp", deps.MTABindAddr)
	if err != nil {
		// The most admin-meaningful failure this binary has: no port 25 means
		// no inbound mail at all. Only the port rides the plane — the bind host
		// and the error text stay in the returned error, which main logs.
		logplane.ListenerBindFailed("smtp", logplane.PortOf(deps.MTABindAddr))
		return fmt.Errorf("mta listen %s: %w", deps.MTABindAddr, err)
	}
	// One per-IP concurrent-connection limiter shared by both submission
	// listeners (so a source's total submission connections across 465 + 587
	// are counted together), hot-reloaded from AuthPolicy.max_conn_per_ip on
	// config_changed below. The Go analogue of the nest TLS loop's
	// fauna_conn_limit::PerIpConnLimit (smtp-server.md § Connection-time
	// limits; 0 = disabled, loopback exempt). Wrapped BELOW the global cap and
	// directly on the raw socket — submission has no PROXY-v2 front (465/587 are
	// published directly), so RemoteAddr() is the real client IP immediately.
	perIPLimiter := connlimit.NewPerIPLimiter(deps.Snapshot.Auth.MaxConnPerIP)
	var port465, port587 net.Listener
	if tlsAvailable {
		port465Raw, err := net.Listen("tcp", deps.MTABindAddr465)
		if err != nil {
			_ = port25.Close()
			logplane.ListenerBindFailed("submission", logplane.PortOf(deps.MTABindAddr465))
			return fmt.Errorf("submission(465) listen %s: %w", deps.MTABindAddr465, err)
		}
		// Cap connections on the raw socket BELOW tls.NewListener so go-smtp
		// still sees a *tls.Conn (AUTH-requires-TLS) — capSubmission. The per-IP
		// shed sits below the global cap (mirroring nest serve_tls: global
		// semaphore first, then per-IP), so a per-IP-shed conn never consumes a
		// global slot.
		port465 = tls.NewListener(capSubmission(perIPSubmission(port465Raw, perIPLimiter, "465"), "465"), tlsConfig)
		port587Raw, err := net.Listen("tcp", deps.MTABindAddr587)
		if err != nil {
			_ = port465.Close()
			_ = port25.Close()
			logplane.ListenerBindFailed("submission", logplane.PortOf(deps.MTABindAddr587))
			return fmt.Errorf("submission(587) listen %s: %w", deps.MTABindAddr587, err)
		}
		// 587 is STARTTLS, so the cap wraps the raw listener directly.
		port587 = capSubmission(perIPSubmission(port587Raw, perIPLimiter, "587"), "587")
	}

	// Per-listener drain trackers (T2.6). Port 25 (inbound) and 465/587
	// (submission) have independent in-flight sets, so each gets its own
	// tracker; mta.Run OR's their force/clean results for the exit code.
	inboundDrain := newDrainTracker()
	submissionDrain := newDrainTracker()

	// The shared, hot-swappable config holder — read by both the inbound and
	// submission backends at each request boundary, and re-applied in place by
	// the config_changed reloader below (no restart). It derives the
	// connection-time Policy, local-domains list, size cap, auth/spam policies,
	// primary domain, and the per-credential AUTH lockout from the snapshot.
	cfg := newMTAConfigHolder(deps.Snapshot, net.DefaultResolver, realClock{}, logger)
	// One byte-plane client for both mail-ingesting legs (inbound MX and
	// authenticated submission) — they stage identically, so they share it. Nil
	// when the deployment gave us no HTTP reach to nest; stageSealedBody then
	// tempfails an over-frame message rather than panicking.
	var bytePlane *byteplane.Client
	if deps.NestBaseURL != "" {
		bytePlane = byteplane.New(deps.NestBaseURL, deps.NestHTTPClient)
	}
	inbound := &inboundBackend{
		logger:     logger,
		cfg:        cfg,
		caller:     deps.Client,
		bytePlane:  bytePlane,
		scanConfig: deps.ScanConfig,
		// Process-wide content-scan runtime guards (D7): in-flight cap +
		// per-scanner circuit breakers, shared across all inbound sessions.
		scanGate: newScanGate(),
		// InboundTLSMode=required: when TLS is wired, port 25 advertises
		// STARTTLS and Mail rejects pre-STARTTLS envelopes (530 5.7.10).
		// Same condition as the listener's tlsConfig below.
		requireStartTLS: tlsAvailable,
		drain:           inboundDrain,
	}
	// Spawn the outbound worker + submission backend only when submission
	// listeners are wired (TLS present). Without 465/587, this bridge has
	// no submission source; the outbound queue waits for TLS to land
	// before draining. (Outbound rows queued by other paths — e.g. nest's
	// legacy in-process plaintext SMTP — sit in nest's outbound_mail_queue
	// and a later bridge instance with TLS picks them up.)
	//
	// Outbound EHLO host = snapshot.PrimaryDomain (the deployment uses the
	// primary as the canonical sender identity).
	var (
		outboundWorker *OutboundWorker
		submission     *submissionBackend
	)
	if tlsAvailable {
		// The worker reads back from nest's outbound_mail_queue via
		// FetchOutboundDue and drains the rows against external MX
		// hosts (no local-disk spool). Spawned before the submission
		// backend so the backend's Data hook can Trigger() the worker
		// as soon as the first row lands.
		// Build the outbound sender here (rather than letting NewOutboundWorker
		// default it) so its IPv6-egress + 5xx-allowlist knobs read live from
		// the shared config holder — a config_changed swap then hot-applies to
		// outbound delivery at the next attempt, with no restart.
		outboundSender := NewDefaultSMTPSender(deps.Snapshot.PrimaryDomain)
		outboundSender.policy = cfg.outboundPolicy
		var werr error
		outboundWorker, werr = NewOutboundWorker(
			deps.Client,
			deps.MXResolver, // nil ⇒ LiveMXResolver; non-nil ⇒ operator-hatch transport override
			outboundSender,  // EHLO = snapshot.PrimaryDomain; policy from the holder
			deps.Snapshot.PrimaryDomain,
			deps.OutboundWorkerConfig,
			logger.With("worker", "outbound"),
		)
		if werr != nil {
			if port587 != nil {
				_ = port587.Close()
			}
			if port465 != nil {
				_ = port465.Close()
			}
			_ = port25.Close()
			return fmt.Errorf("outbound worker: %w", werr)
		}
		// Share the one byte-plane client with the outbound worker so it can
		// resolve a unit whose body nest staged on the bulk-byte plane (the
		// staged-envelope rule — an over-inline-budget outbound body rides by
		// reference; the worker fetches + AEAD-opens it in bodyFor). nil when the
		// deployment gave us no HTTP reach to nest, exactly as the ingesting legs.
		outboundWorker.bytePlane = bytePlane

		// The submission backend reads its local-domains list, primary-domain
		// anchor, and the D.7 per-(credential, source-IP) AUTH-failure lockout
		// from the same shared, hot-swappable holder as the inbound backend
		// (the lockout cap mirrors the catalog
		// `mail.auth.max_auth_failures_per_minute`, default 30; zero disables).
		submission = &submissionBackend{
			cfg:             cfg,
			logger:          logger.With("listener", "submission"),
			client:          deps.Client,
			bytePlane:       bytePlane,
			outboundTrigger: outboundWorker.Trigger,
			drain:           submissionDrain,
		}
		// The inbound forward-all stage (mail-forwarding N4) nudges the same
		// worker so an inbound forward delivers promptly, like a submission.
		inbound.outboundTrigger = outboundWorker.Trigger
	}

	// config_changed hot-reload (mail-policy-config.md § Architectural rules —
	// "the bridge MUST apply changes at the next request boundary without a
	// restart"). A `fauna.bridges.config_changed` push (or a reconnect
	// re-fetch) re-fetches the whole snapshot off the reader goroutine — a
	// synchronous fetch there would deadlock — and applies it to the shared
	// `cfg` holder, so LocalDomains / spam / auth / submission knobs take
	// effect on the next inbound/submission connection. The MTA consumes only
	// this one push kind (no IMAP mailbox-state route), so the dispatcher
	// carries a single registration; the production *wsrpc.ReconnectingClient
	// re-installs it on every reconnect. mda.Run has the symmetric wiring.
	pushDispatcher := wsrpc.NewPushDispatcher(logger.With("component", "push_dispatcher"))
	configReloader := wsrpc.NewConfigReloader(deps.Client, configReloadFetchTimeout, logger.With("component", "config_reloader"))
	configReloader.Register(cfg.ApplyConfig)
	// Prompt-refresh-on-provision (mail-bridge-lifecycle.md § TLS provisioning):
	// re-fetch the submission (465/587) TLS cert on any config_changed push so a
	// cert that lands while this MTA is running (a domain added post-claim → ACME
	// issues; an admin self-signed provision) is served without waiting out the
	// provider's 12 h timer. Nest tags the push reason "tls"; refresh on any
	// reason (no token parsing needed). Symmetric with mda.Run. Nil when the
	// MTA has no submission listeners (no TLS provider) — then there is nothing
	// to refresh.
	if deps.TLSProvider != nil {
		configReloader.Register(func(_ wsrpc.ConfigSnapshot) {
			deps.TLSProvider.TriggerRefresh()
		})
	}
	// Hot-reload the per-IP concurrent-connection cap from AuthPolicy.
	// max_conn_per_ip on a config_changed re-fetch (same seam as the AUTH
	// lockout the holder's ApplyConfig reads). Already-open connections keep
	// their slots; the new ceiling applies at the next accept.
	configReloader.Register(func(snap wsrpc.ConfigSnapshot) {
		perIPLimiter.SetMax(snap.Auth.MaxConnPerIP)
	})
	pushDispatcher.Register(wsrpc.PushKindConfigChanged, configReloader.Handle)
	// outbound_ready hot-drain (smtp-server.md § Outbound delivery): a
	// nest-side remote enqueue (a client `fauna.email.send`) pushes
	// `fauna.bridges.outbound_ready`; the handler nudges the outbound
	// worker's coalescing Trigger so the relay happens promptly instead of
	// on the next fetch_outbound_due poll (the backstop). Only when the
	// worker exists — without TLS/submission listeners there is no
	// outboundWorker (nil), and the queue waits for a TLS-capable instance.
	if outboundWorker != nil {
		outboundReady := wsrpc.NewOutboundReadyHandler(
			outboundWorker.Trigger,
			logger.With("component", "outbound_ready"),
		)
		pushDispatcher.Register(wsrpc.PushKindOutboundReady, outboundReady.Handle)
	}
	// Publish the reloader so main.go's reconnect OnConnect re-applies the
	// re-fetched snapshot through the same Apply seam as the push (§ Reconnecting):
	// a config changed during a WS gap then hot-applies on reconnect, not only
	// on the next push. Held in an atomic.Pointer main-side; the reloader is
	// goroutine-safe.
	if deps.PublishConfigReloader != nil {
		deps.PublishConfigReloader(configReloader)
	}
	// Install the dispatcher as the process's single push handler. The
	// production deps.Client (a *wsrpc.ReconnectingClient) re-installs it on
	// every reconnect, so the config_changed subscription survives nest blips.
	if inst, ok := deps.Client.(interface {
		SetOnPush(wsrpc.PushHandler)
	}); ok {
		inst.SetOnPush(pushDispatcher.Handle)
	}
	go configReloader.Run(ctx)

	startArgs := []any{
		"inbound_addr", deps.MTABindAddr,
		"local_domains_count", len(deps.Snapshot.LocalDomains),
		"primary_domain", deps.Snapshot.PrimaryDomain,
		"bridge_id", deps.BridgeID,
		"tls_available", tlsAvailable,
		// Effective per-IP connection rate limit (snapshot-fed; catalog
		// default 10). Logged so a `421 Connection rate limit exceeded`
		// flake is diagnosable from the bridge log — e.g. the e2e harness
		// raises this via put_spam_policy because every test connects from
		// 127.0.0.1 sharing one budget; a stale/misprojected value shows
		// here as 10 instead of the override.
		"max_conn_per_min", deps.Snapshot.Spam.MaxConnPerMin,
	}
	if tlsAvailable {
		startArgs = append(startArgs,
			"submission_465_addr", deps.MTABindAddr465,
			"submission_587_addr", deps.MTABindAddr587,
		)
	}
	logger.Info("mta.Run starting listeners", startArgs...)

	// One goroutine per listener, errors collected on a buffered chan
	// so an early return doesn't block siblings. The shared listenerCtx
	// is cancelled on first error so all listeners shut down together;
	// each listener's runner respects ctx.Done() via its own graceful-
	// close path (see runListenerWithBackend / runSubmissionListener).
	listenerCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	// Start the outbound worker on the shared listener context so a
	// listener tear-down also tears down the worker. wg waits for the
	// worker on the way out. When TLS is absent the worker is nil (no
	// submission listeners to source rows from — see the worker
	// construction above).
	if outboundWorker != nil {
		workerWG := outboundWorker.Start(listenerCtx)
		defer workerWG.Wait()
	}
	var wg sync.WaitGroup
	listenerCount := 1
	if tlsAvailable {
		listenerCount = 3
	}
	errCh := make(chan error, listenerCount)

	wg.Add(listenerCount)
	go func() {
		defer wg.Done()
		errCh <- runListenerWithBackend(listenerCtx, port25, inbound, tlsConfig, deps.Snapshot.PrimaryDomain, deps.Snapshot.Spam.MaxMessageBytes, deps.ShutdownGrace, inboundDrain, logger.With("listener", "inbound-25"))
	}()
	if tlsAvailable {
		go func() {
			defer wg.Done()
			errCh <- runSubmissionListener(listenerCtx, port465, submission, submissionImplicitTLS, tlsConfig, deps.Snapshot.PrimaryDomain, deps.Snapshot.Spam.MaxMessageBytes, deps.ShutdownGrace, submissionDrain, logger.With("listener", "submission-465"))
		}()
		go func() {
			defer wg.Done()
			errCh <- runSubmissionListener(listenerCtx, port587, submission, submissionStartTLS, tlsConfig, deps.Snapshot.PrimaryDomain, deps.Snapshot.Spam.MaxMessageBytes, deps.ShutdownGrace, submissionDrain, logger.With("listener", "submission-587"))
		}()
	}

	// Collect every listener's result. A real serve error (failed bind /
	// unexpected Serve failure) cancels the siblings and is surfaced as
	// firstErr. bridgeshutdown.ErrShutdownForced is not a failure — it's the
	// expected outcome of a graceful shutdown whose grace window expired — but
	// it must still reach main as the process exit signal, so we remember it
	// and return it only if no real error preempted it.
	var firstErr error
	forced := false
	for i := 0; i < listenerCount; i++ {
		err := <-errCh
		switch {
		case err == nil:
		case errors.Is(err, bridgeshutdown.ErrShutdownForced):
			forced = true
		case firstErr == nil:
			firstErr = err
			cancel()
		}
	}
	wg.Wait()
	if firstErr != nil {
		return firstErr
	}
	if forced {
		return bridgeshutdown.ErrShutdownForced
	}
	return nil
}

// newPolicyFromSnapshot constructs the Phase C.2 connection-time
// policy from the bridge's nest-fetched config snapshot. Resolver +
// clock are injected so tests can use a fakeResolver / fakeClock; the
// production caller passes net.DefaultResolver + realClock{}.
//
// Every knob lives on the snapshot
// (a product invariant: nest holds policy, the CLI/operator holds topology). No env-var
// reads here.
func newPolicyFromSnapshot(snap wsrpc.ConfigSnapshot, resolver DNSResolver, clock Clock) *Policy {
	if resolver == nil {
		resolver = net.DefaultResolver
	}
	if clock == nil {
		clock = realClock{}
	}
	p := &Policy{
		RateLimiter:          NewRateLimiter(snap.Spam.MaxConnPerMin, time.Minute, clock),
		DNSBL:                NewDNSBLChecker(snap.Spam.DNSBLServers, resolver),
		FCrDNS:               NewFCrDNSChecker(resolver),
		FCrDNSMode:           ParseFCrDNSMode(snap.Spam.FCrDNSMode),
		RejectFCrDNSFail:     snap.Spam.RejectFCrDNSFail,
		HELOIdentityRequired: snap.Spam.HELOIdentityRequired,
		HELOResolver:         resolver,
		HELOLookupTimeout:    3 * time.Second,
		// SenderDomain is always-on (no catalog knob; smtp-server.md
		// § Sender-domain): MAIL FROM is rejected 550 5.7.1 when the
		// sender domain has neither MX nor A/AAAA. Resolver errors fail
		// open with a counter.
		SenderDomain: NewSenderDomainChecker(resolver),
		// SkipHELOLoopback stays false: production keeps the loopback
		// exemption ON so local test harnesses keep working. The e2e
		// harness that runs against a real bridge will set this via a
		// follow-up that exposes it to integration tests; for the
		// hermetic unit tests above we set it directly on the Policy.
	}
	// Greylisting is enforced **nest-side** (`fauna.bridges.check_greylist`,
	// smtp-server.md § Greylisting) so its state is uniform across bridge
	// restart — there is no per-process greylist map to construct here.
	// `snap.Spam.Greylist*` is still projected (the admin pane reads it); nest
	// applies it.
	return p
}

// MailEnabled reports whether the supplied ConfigSnapshot represents
// an "admin has enabled mail" configuration.
//
// The bridge's protocol with nest is "if you get a snapshot, mail is
// configured to your role"; the supervisor (s6 / systemd) is what
// gates whether the bridge process runs at all when the admin
// toggles mail off. Phase C's SMTP listener gates on this flag —
// when false it idles instead of binding port 25 — so the bridge
// handles a mid-transition snapshot (admin toggled mail off, but the
// supervisor hasn't yet torn the process down) without crashing.
//
// Phase C.0 replaced the Phase B.8 heuristic (
// `max_score_before_reject > 0 || len(dnsbl_servers) > 0`) with the
// explicit `mail_enabled` field on `ConfigSnapshot` (mirrors
// `FetchConfigReply::mail_enabled` in the protocol crate).
func MailEnabled(s wsrpc.ConfigSnapshot) bool {
	return s.MailEnabled
}

// Bindable reports whether a snapshot opens BOTH of Run's startup gates —
// mail enabled and at least one ratified mail domain — i.e. whether an MTA
// cold-booting on it would bind its listeners rather than idle.
//
// It is the single source of truth for that question: Run's own two idle
// branches and the idle watcher's "have my gates opened?" applier are the same
// predicate read from opposite sides, so an MTA can never idle on a snapshot it
// would also refuse to wake for (or wake for one it would then idle on, which
// is a restart loop).
func Bindable(s wsrpc.ConfigSnapshot) bool {
	return MailEnabled(s) && len(s.LocalDomains) > 0
}

// idleUntilBindable is the shared tail of Run's two idle branches: stay
// subscribed to `config_changed` and return once either the process context
// ends or a snapshot arrives that Bindable accepts.
//
// Both outcomes return nil — a clean shutdown either way, which main.go turns
// into exit 0. The difference is only what the supervisor does next: after a
// SIGTERM it stays down; after a gate-open it restarts the service, and the
// fresh process cold-boots against the snapshot that opened the gate. See
// wsrpc.IdleUntilGatesOpen for why exit-for-rebind rather than binding in
// place.
func idleUntilBindable(ctx context.Context, deps Deps, logger *slog.Logger) error {
	opened := wsrpc.IdleUntilGatesOpen(
		ctx,
		deps.Client,
		deps.PublishConfigReloader,
		configReloadFetchTimeout,
		logger.With("component", "idle_gate_watch"),
		Bindable,
	)
	if opened {
		logger.Info("mta.Run shutting down", "reason", "mail gates opened while idling; exiting for supervisor rebind")
		return nil
	}
	logger.Info("mta.Run shutting down", "reason", ctx.Err())
	return nil
}
