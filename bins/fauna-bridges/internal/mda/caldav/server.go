package caldav

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"sync/atomic"
	"time"

	"github.com/emersion/go-webdav/caldav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/authlock"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/dav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// caldavRealm is the WWW-Authenticate realm this CalDAV terminator advertises on
// 401 responses (RFC 7617 requires one; "fauna-caldav" scopes the credentials to
// this service). It is threaded into davauth.NewMiddleware and reused by the
// interceptors' defensive no-session 401s (props/sync-collection/mkcalendar) so
// every CalDAV challenge is byte-identical. The coming CardDAV terminator passes
// "fauna-carddav" to the same davauth machinery.
const caldavRealm = "fauna-caldav"

// readHeaderTimeout is the per-request header-read deadline. CalDAV
// PROPFIND headers from well-behaved MUAs land in well under a
// second; a stuck client should not be able to occupy a server
// goroutine indefinitely.
const readHeaderTimeout = 30 * time.Second

// readTimeout / writeTimeout / idleTimeout time-bound the *whole* request, not
// just the header (§ B6 — 2026-06-24 email-component review). Without them a
// slow-body PUT/REPORT holds a goroutine + buffer (the body is size-bounded at
// maxRequestBytes = 16 MiB, but was not *time*-bounded), and an idle keep-alive
// connection sits open indefinitely.
//
//   - readTimeout bounds reading the entire request body. Generous (5 min) so a
//     genuinely slow but legitimate MUA uploading a near-16-MiB calendar over a
//     poor link still completes, while a slow-loris drip is cut off. Matches the
//     SMTP submission listener's ReadTimeout posture (submission.go).
//   - writeTimeout bounds generating + writing the response — a full-calendar
//     REPORT/multiget can be large but is event-count-bounded (report.go's
//     maxReportEvents); 5 min covers it.
//   - idleTimeout bounds a kept-alive connection between requests.
const (
	readTimeout  = 5 * time.Minute
	writeTimeout = 5 * time.Minute
	idleTimeout  = 2 * time.Minute
)

// Server wraps emersion/go-webdav/caldav.Handler with the Fauna-
// specific Backend + auth middleware + W1 (account-data-plane.md § Workstreams) mitigation middleware.
// One instance per listening address; the same Server is used by
// both implicit-TLS (`:443`) listeners.
type Server struct {
	inner  *http.Server
	logger *slog.Logger
	// lockout is the per-(credential, source-IP) AUTH-failure brake shared
	// with the auth middleware. ApplyConfig hot-swaps the pointed-to
	// instance on a `config_changed` re-fetch; the middleware Loads it per
	// request. Mirrors the IMAP backend's atomic authLockout.
	lockout *atomic.Pointer[authlock.Lockout]
	// primaryDomain is the box's PrimaryDomain, shared with the auth
	// middleware so a bare Basic-auth username (no `@domain`, what macOS
	// Calendar.app sends) resolves under it. ApplyConfig hot-swaps it on a
	// `config_changed` re-fetch; the middleware Loads it per request.
	primaryDomain *atomic.Pointer[string]
	// localDomains is the box's hosted-domain set, shared with the Backend's
	// auto-schedule classifier (it Loads it per organizer PUT). ApplyConfig
	// hot-swaps it on a `config_changed` re-fetch. Same pointer the Backend
	// holds, so a swap takes effect without rebuilding the Backend.
	localDomains *atomic.Pointer[[]string]
	// mailEnabled mirrors the deployment's `ConfigSnapshot.MailEnabled` toggle,
	// shared with the Backend's auto-schedule classifier (same pointer, so a
	// config_changed swap reaches it without a rebuild). On an email-disabled
	// nest the classifier routes local Fauna attendees onto the sealed scheduling
	// rail rather than the email rail (caldav-server.md § Server-side
	// auto-schedule — email-disabled nest → WS-RPC sealed delivery).
	mailEnabled *atomic.Bool
}

// ServerConfig collects the per-server settings the MDA wires when
// constructing a CalDAV listener.
type ServerConfig struct {
	// TLSConfig is the live TLS config (typically wired from
	// `internal/tls.Provider.GetCertificate`). Implicit-TLS only —
	// the goal doc § Process topology lists no STARTTLS path for
	// CalDAV; HTTPS handles the upgrade implicitly.
	TLSConfig *tls.Config
	// Logger is the per-server structured logger.
	Logger *slog.Logger
	// MaxAuthFailuresPerMinute seeds the per-(credential, source-IP)
	// AUTH-failure lockout from `Snapshot.Auth.MaxAuthFailuresPerMinute`
	// (catalog default 30; `0` disables — the value tests leave unset).
	// ApplyConfig rebuilds it on a `config_changed` re-fetch.
	MaxAuthFailuresPerMinute uint32
	// PrimaryDomain seeds the box's primary domain (ConfigSnapshot.PrimaryDomain)
	// — the domain a bare Basic-auth username (no `@domain`) resolves under,
	// so macOS Calendar.app (which sends only the local part) authenticates.
	// ApplyConfig hot-swaps it on a `config_changed` re-fetch. Empty ⇒ strict
	// `user@domain` only (the value tests leave unset).
	PrimaryDomain string
	// LocalDomains seeds the box's hosted mail/CalDAV domains
	// (ConfigSnapshot.LocalDomains) — the locality split the server-side
	// auto-schedule gateway uses: a LOCAL-domain attendee is classified by the
	// same-nest reads (resolve_recipient → actor.by_handle → keypackage.fetch),
	// while an OFF-box-domain attendee is classified by the shared resolver's
	// anon CROSS-NEST discovery (a mailbox-less Fauna attendee on a different,
	// email-disabled nest → the cross-nest sealed rail) — caldav-server.md
	// § Server-side auto-schedule. ApplyConfig hot-swaps it on a `config_changed`
	// re-fetch. Empty ⇒ no domain is local, so every attendee takes the off-box
	// (cross-nest discovery) path.
	LocalDomains []string
	// MailEnabled seeds the deployment's email-enabled toggle
	// (ConfigSnapshot.MailEnabled) — the signal the auto-schedule classifier uses
	// to decide whether a local attendee's canonical alias means email-reachable.
	// On an email-disabled (CalDAV-only) nest a local Fauna attendee rides the
	// sealed scheduling rail instead. ApplyConfig hot-swaps it on a
	// `config_changed` re-fetch. The zero value (false) is safe: NewServer always
	// seeds it from the live snapshot, and a Backend built without it wired treats
	// nil as email-enabled (the established default).
	MailEnabled bool
	// ClassifyTransport, when non-nil, overrides the off-box (cross-nest) attendee
	// discovery the auto-schedule classifier uses (production:
	// mailfauna.ClassifyAttendeeTransport — anon TLS discovery to the peer nest).
	// A dependency-injection seam, like the wsrpc.Caller NewServer takes: tests set
	// it to a stub so the classifier's off-box branch runs without real network.
	// nil ⇒ the production resolver.
	ClassifyTransport func(addr string) (mailfauna.AttendeeTransport, error)
	// CardDAVEnabled makes this CalDAV chain's shared root principal a UNIFIED
	// principal — it also advertises `addressbook-home-set` (→ `/carddav/{user}/`).
	// mda.go seeds it from the live `carddav_enabled` gate, but ONLY when it also
	// mounts this CalDAV chain at the mux catch-all `/` (the both-protocols-on
	// deployment; davMounts). In that shape a CardDAV client's principal PROPFIND
	// of `/{user}/` lands on this chain (not the `/carddav/`-mounted CardDAV
	// chain), so this is the one place the address-book home set can be advertised
	// for host-only autodiscovery. Contacts-only (CardDAV at `/`, no CalDAV) needs
	// no flag: emersion's own carddav principal serves addressbook-home-set. A
	// runtime toggle triggers a full 443-rebind, so this construction-time value is
	// always live — no atomic. See schedule.go principalServedProps.
	CardDAVEnabled bool
}

// NewServer constructs a CalDAV server bound to `client` (the
// MDA-process WS-RPC caller) and the configured TLS surface. The
// returned Server is not started; the caller invokes `Serve(ln)`
// with the TLS listener.
//
// Per `caldav-server.md` § Authentication: HTTPS-only. The auth
// middleware wraps the caldav.Handler so every request lands with
// a validated session in context (or 401 on the wire).
func NewServer(cfg ServerConfig, client wsrpc.Caller) *Server {
	if client == nil {
		panic("caldav: NewServer: client must not be nil")
	}
	logger := cfg.Logger
	if logger == nil {
		logger = slog.Default()
	}

	// Hosted-domain set, hot-swappable via ApplyConfig (same pattern as
	// primaryDomain). Shared with the Backend so the auto-schedule classifier
	// reads the live set; a config_changed swap reaches it without a rebuild.
	localDomains := &atomic.Pointer[[]string]{}
	ld := normalizeDomains(cfg.LocalDomains)
	localDomains.Store(&ld)

	// Deployment email-enabled flag, hot-swappable via ApplyConfig (same pattern
	// as localDomains). Shared with the Backend so the auto-schedule classifier
	// reads the live value; on an email-disabled nest it routes local Fauna
	// attendees onto the sealed scheduling rail.
	mailEnabled := &atomic.Bool{}
	mailEnabled.Store(cfg.MailEnabled)

	backend := NewBackend(logger, localDomains)
	backend.mailEnabled = mailEnabled
	// Off-box cross-nest discovery dependency (nil ⇒ the production resolver;
	// see Backend.classifyOffBox). Injected so tests stub the anon-TLS probe.
	backend.classifyTransport = cfg.ClassifyTransport
	handler := &caldav.Handler{Backend: backend}

	// Shared AUTH-failure lockout, hot-swappable via ApplyConfig. The
	// middleware Loads it per request; the Server keeps the same pointer so
	// a config_changed rebuild swaps the ceiling without a restart.
	lockout := &atomic.Pointer[authlock.Lockout]{}
	lockout.Store(authlock.New(cfg.MaxAuthFailuresPerMinute, time.Minute, nil))

	// Shared primary domain, hot-swappable via ApplyConfig (same pattern as
	// lockout). Lets a bare Basic-auth username resolve under it.
	primaryDomain := &atomic.Pointer[string]{}
	pd := cfg.PrimaryDomain
	primaryDomain.Store(&pd)

	// Mux owns the well-known redirect (`/.well-known/caldav`) and
	// the catch-all CalDAV path. emersion/go-webdav v0.7.0's
	// `caldav.Handler` already serves the well-known redirect, but
	// only after the request hits the handler — which requires
	// passing the auth middleware first. Per goal doc § Authentication
	// the redirect requires auth too, so the explicit well-known
	// mount here just defers to the handler.
	mux := http.NewServeMux()
	// Middleware chain (outermost → innermost):
	//   accessLogMiddleware — one structured line per request (method / path /
	//                       status / duration); Info for rare mutation verbs,
	//                       Debug for high-volume reads. Outermost so it also
	//                       logs auth failures and w1 rejections. Reconstructs
	//                       the macOS create-then-rename verb sequence in box
	//                       logs for the live hand-proof.
	//   w1Mitigations    — body cap / XML depth / PROPFIND depth gates
	//   davauth.NewMiddleware — HTTP Basic → AEAD-unwrap → Session in ctx
	//   ifMatchMiddleware — stash If-Match header for DELETE handler
	//   newPropPatchInterceptor    — handle PROPPATCH (emersion 501s
	//                       it at caldav/server.go:664); collection
	//                       path → unseal/mutate/re-seal/provision_calendar
	//                       (update_metadata=true); event path → 403
	//                       cannot-modify-protected-property.
	//   newMkcalendarInterceptor — handle MKCALENDAR (emersion 405s it —
	//                       it routes only MKCOL); collection path →
	//                       provision_calendar(update_metadata=false). macOS
	//                       Calendar.app creates calendars via MKCALENDAR.
	//   newSyncCollectionInterceptor — peek REPORT body; route
	//                       {DAV:}sync-collection to nest's
	//                       sync_calendar_since RPC; pass everything
	//                       else through to emersion.
	//   newSchedulingInterceptor — RFC 6638 advertisement: OPTIONS DAV
	//                       header `calendar-auto-schedule`; PROPFIND of the
	//                       principal serves the scheduling props; PROPFIND of
	//                       the schedule Inbox/Outbox serves a minimal typed
	//                       collection. Everything else → emersion (schedule.go).
	//   caldav.Handler   — emersion's PROPFIND/PUT/DELETE/REPORT-query
	//                       /REPORT-multiget/MKCOL handler.
	//
	// PUT continues to receive If-Match through `opts.IfMatch` per
	// emersion's existing plumbing.
	mux.Handle("/", accessLogMiddleware(w1Mitigations(davauth.NewMiddleware(caldavRealm, ifMatchMiddleware(newPropPatchInterceptor(newMkcalendarInterceptor(newMoveCopyInterceptor(newSyncCollectionInterceptor(newSchedulingInterceptor(dav.QuotaBody(handler), logger, cfg.CardDAVEnabled), logger), logger), logger), logger)), client, logger, lockout, primaryDomain)), logger))

	inner := &http.Server{
		Handler:           mux,
		ReadHeaderTimeout: readHeaderTimeout,
		ReadTimeout:       readTimeout,
		WriteTimeout:      writeTimeout,
		IdleTimeout:       idleTimeout,
		TLSConfig:         cfg.TLSConfig,
		ErrorLog:          nil, // emersion/go-webdav logs to our slog via the backend
	}
	return &Server{
		inner:         inner,
		logger:        logger,
		lockout:       lockout,
		primaryDomain: primaryDomain,
		localDomains:  localDomains,
		mailEnabled:   mailEnabled,
	}
}

// Handler returns the full CalDAV middleware+backend chain (accessLog → w1 →
// auth → ifMatch → propPatch → mkcalendar → sync-collection → scheduling →
// emersion caldav.Handler), mounted at the catch-all path, as a single
// http.Handler. Slice 2d's shared-443 mux mounts this at `/` (the CalDAV
// principal lives at root `/{user}/`) alongside the CardDAV chain under
// `/carddav/` on ONE 443 listener, so the two DAV protocols share a TLS
// termination and port (their SRV records both target 443). For standalone use
// call Serve instead — it wraps this handler in the Server's own http.Server.
//
// Minimal-disturbance: this only EXPOSES the existing internal mux/chain — the
// middleware chain and ApplyConfig are unchanged, so CalDAV serving is
// byte-identical whether reached standalone or under the shared mux.
func (s *Server) Handler() http.Handler {
	return s.inner.Handler
}

// ApplyConfig hot-swaps the AUTH-failure ceiling from a freshly fetched
// snapshot, so a `fauna.bridges.config_changed` re-fetch (or a reconnect
// re-fetch) takes effect on the next request without a restart. Registered
// with the wsrpc.ConfigReloader in mda.Run. A new lockout resets the
// in-flight counters (mirrors the MTA / IMAP backend); acceptable on a rare
// admin config edit. Concurrency-safe: the swap is a single atomic store
// the middleware Loads per request.
func (s *Server) ApplyConfig(snap wsrpc.ConfigSnapshot) {
	s.lockout.Store(authlock.New(snap.Auth.MaxAuthFailuresPerMinute, time.Minute, nil))
	pd := snap.PrimaryDomain
	s.primaryDomain.Store(&pd)
	ld := normalizeDomains(snap.LocalDomains)
	s.localDomains.Store(&ld)
	s.mailEnabled.Store(snap.MailEnabled)
	s.logger.Info("caldav server hot-applied config",
		"max_auth_failures_per_minute", snap.Auth.MaxAuthFailuresPerMinute,
		"primary_domain", snap.PrimaryDomain,
		"local_domains_count", len(ld),
		"mail_enabled", snap.MailEnabled,
	)
}

// Serve accepts connections on `ln` and processes them until `ln`
// is closed or Close is called. Standard io.EOF-on-close semantics;
// returns http.ErrServerClosed on a clean shutdown.
func (s *Server) Serve(ln net.Listener) error {
	err := s.inner.Serve(ln)
	if errors.Is(err, http.ErrServerClosed) {
		return nil
	}
	return err
}

// Close stops accepting new connections and waits up to 5s for
// in-flight requests to finish. The auth middleware's deferred
// `sess.Close()` zeroizes any active capabilities on the way out.
func (s *Server) Close() error {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := s.inner.Shutdown(ctx); err != nil {
		return fmt.Errorf("caldav: shutdown: %w", err)
	}
	return nil
}

// Shutdown gracefully drains in-flight CalDAV requests within ctx (T2.6,
// mail-bridge-lifecycle.md § Shutting down). Stdlib gives the graceful drain
// for free: http.Server.Shutdown closes the listener, lets active requests
// finish, and returns ctx.Err() if the deadline hits first. On a deadline
// expiry we force-close the stragglers (http.Server.Close) and report
// forced=true so the MDA role can map it to bridgeshutdown.ErrShutdownForced.
// (HTTP doesn't expose an in-flight count, so unlike the IMAP path there is no
// pending_count to surface.)
func (s *Server) Shutdown(ctx context.Context) (forced bool) {
	err := s.inner.Shutdown(ctx)
	if err == nil {
		return false
	}
	// Deadline (or cancellation) hit with requests still draining: force-close.
	_ = s.inner.Close()
	return true
}
