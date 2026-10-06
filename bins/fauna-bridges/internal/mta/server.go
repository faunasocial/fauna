// Phase C.1: SMTP MX server skeleton.
//
// This file owns the go-smtp wiring that turns mta.Run from an idle
// stub into a working external-MX listener. Phase C.1 ships only the
// skeleton: every connection accepts, DATA is buffered into a []byte,
// and the backend records receipt (the production inboundBackend logs;
// tests inject a recording backend that pushes raw bytes onto a
// channel — see server_test.go).
//
// Phases C.2-C.9 wrap policy / verify / score / tokenize / encrypt /
// ingest around this skeleton:
//
//   - C.2 plugs in `policy.go` (rate limit + DNSBL + greylist + FCrDNS
//   - HELO identity) on the NewSession / Mail / Rcpt hooks.
//   - C.3 wires `validate_recipient` on Rcpt.
//   - C.4 wires `mailfauna.ParseRFC5322` on Data and stores the
//     parsed message on the session.
//   - C.5 wires `verify_inbound` (SPF/DKIM/DMARC/ARC).
//   - C.6 adds the DKIM enforce-on-fail policy gate (verdict-acting
//     sibling of C.5's verify).
//   - C.7-C.9 score → tokenize → encrypt-to-recipient → ingest.
//
// Legacy reference is `bins/fauna-bridge-imap/internal/smtp/inbound.go`
// (retired; see git history) for go-smtp
// wiring patterns; we lift the *shape* (NewSession / Mail / Rcpt / Data)
// and explicitly reject the env-var config reads that surround it — a
// product invariant: everything non-deployment-topology comes
// from `Deps.Snapshot` via WS-RPC.
// `go-message` is forbidden (Phase A forbidigo rule); message parsing
// is shared Rust via UniFFI from C.4 onward.
package mta

import (
	"bytes"
	"context"
	crand "crypto/rand"
	"crypto/tls"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/mail"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/emersion/go-sasl"
	gosmtp "github.com/emersion/go-smtp"

	faunaCore "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_core"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/bridgeshutdown"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/byteplane"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/connlimit"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailstage"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/metrics"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/scan"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// Connection-time limits for the public-internet inbound listener (port
// 25). The global connection cap (maxInboundConns) is now shared by every
// bridge listener via internal/connlimit — submission (465/587), IMAP
// (993/143) and CalDAV all wrap their listener in the same semaphore (see
// maxSubmissionConns / mda.maxMDAConns) — but port 25 keeps a deliberately
// tighter cap than the authenticated surfaces (smtp-server.md
// § Connection-time limits). All these values are compile-time constants,
// NOT tunable: mail-policy-config.md § Compile-time decisions pins
// the header caps as parser-bomb defense the admin cannot raise, and the
// global-concurrency / tarpit values are absent from the policy catalog by
// design. The per-IP / per-subnet *rate* limits that ARE catalog knobs live
// elsewhere (policy.go's RateLimiter at NewSession); these are the global
// cap, the error tarpit, and the header-section cap.
const (
	// maxInboundConns caps simultaneously-served inbound (port 25)
	// connections to back-pressure botnet bursts. Accept() blocks once the
	// cap is hit; excess connections wait in the kernel backlog rather than
	// spawning unbounded goroutines. Deliberately low: port 25 is
	// unauthenticated MX, where 100 concurrent deliveries is ample for
	// legitimate senders and tightly bounds a flood. The authenticated
	// submission / IMAP / CalDAV surfaces use a far higher backstop
	// (maxSubmissionConns / mda.maxMDAConns) since they hold many
	// long-lived, legitimately-concurrent client sessions.
	maxInboundConns = 100
	// maxInboundConnsPerIP bounds how many of the maxInboundConns global
	// port-25 slots a single source IP may hold concurrently — the per-IP
	// *concurrent* complement to port 25's per-IP *rate* cap (10/min) and the
	// 30 s per-command read timeout. Defense-in-depth against a patient
	// trickle-slowloris: even paced under the rate cap and trickling a command
	// every < 30 s to dodge the read timeout, one source can no longer
	// accumulate toward all 100 global slots. Set to half the global cap
	// (matching postfix's `smtpd_client_connection_count_limit` default of 50) —
	// generous enough that a legitimate sending MX's shared-egress-pool fan-in is
	// never refused, while still forcing multiple sources to saturate the
	// listener. Loopback is exempt (the in-container router/bridge dials).
	// Hard-coded: internal anti-abuse tuning, not a human-chosen knob;
	// smtp-server.md § Connection-time limits.
	maxInboundConnsPerIP = 50
	// maxSubmissionConns caps simultaneously-served submission (465/587)
	// connections. Authenticated clients, so the cap is a generous OS-FD /
	// goroutine backstop rather than a flood brake — matched to the Rust
	// nest listener loop's global connection cap (4096), not port
	// 25's deliberate 100 (mda.maxMDAConns is the IMAP/CalDAV twin).
	maxSubmissionConns = 4096
	// maxHeaderLines / maxHeaderBytes bound the RFC-5322 header section a
	// peer may send before the body separator. Exceeding either — or
	// sending no `\r\n\r\n` within the byte cap — is a parser bomb and
	// rejects with 554 5.6.0 before the UniFFI parser ever runs.
	maxHeaderLines = 256
	maxHeaderBytes = 1 << 20 // 1 MiB
	// tarpitBase / tarpitCap define the escalating per-session delay
	// applied before each policy rejection: min(tarpitBase·n, tarpitCap)
	// where n is the running count of policy rejections on the session.
	tarpitBase = 250 * time.Millisecond
	tarpitCap  = 5 * time.Second
)

// runListenerWithBackend runs the go-smtp server on l with the given
// backend until ctx is cancelled. Exposed for tests so they can dial
// an ephemeral listener with their own recording backend; Run wires
// the production inboundBackend.
//
// When tlsConfig != nil the port-25 listener advertises STARTTLS in
// EHLO and supports the in-protocol upgrade (per smtp-server.md § TLS
// posture per port — all listeners share one hardened *tls.Config).
// The inboundBackend independently enforces `InboundTLSMode=required`
// by rejecting MAIL FROM before STARTTLS (see inboundSession.Mail);
// go-smtp's `AllowInsecureAuth=false` only gates AUTH, which port 25
// doesn't offer. tlsConfig == nil binds plaintext-only and does not
// advertise STARTTLS (the bridge can still serve inbound MX before TLS
// is provisioned).
//
// Returns nil on clean shutdown (ctx.Done() drained), or an error if
// Serve fails for a reason other than the listener being closed.
func runListenerWithBackend(ctx context.Context, l net.Listener, backend gosmtp.Backend, tlsConfig *tls.Config, domain string, maxMessageBytes uint32, grace time.Duration, drain *drainTracker, logger *slog.Logger) error {
	srv := gosmtp.NewServer(backend)
	srv.Domain = domain
	if tlsConfig != nil {
		srv.TLSConfig = tlsConfig
	}
	// Sane bounds — finer-grained policy lands in C.2 (per-IP rate
	// limit, DNSBL, greylist, FCrDNS, HELO identity). Until then the
	// listener still rejects pathological inputs at the protocol
	// layer.
	srv.MaxRecipients = 100
	// go-smtp advertises `SIZE` in EHLO straight from this field, so it must be
	// the *effective* ceiling — the admin knob, a 0 knob mapped to the product
	// default (smtp-server.md § Message size limits; the at-rest ceiling that used to
	// bound this is retired — no body is too large to rest at any size since
	// ceiling retirement, 2026-07-18). It used to be a 1 GiB backstop,
	// which meant every EHLO told senders a number ~120× the truth; a sender that
	// believed it transferred a message we were always going to refuse.
	//
	// This is the BOOT snapshot's value, deliberately: go-smtp reads the field
	// live per-EHLO off a non-atomic int64 it owns, so hot-applying a later admin
	// change here would be a data race. Both staleness directions are safe — a
	// stale-high SIZE costs the sender one wasted transfer and then the permanent
	// 552 the live `Data` guard raises; a stale-low SIZE just makes the sender
	// self-limit — so we advertise boot and let enforcement stay live rather than
	// race it (decision D4).
	srv.MaxMessageBytes = int64(mailfauna.EffectiveMaxRawMessageBytes(maxMessageBytes))
	// Per-command read timeout — the inbound-MX slowloris defense
	// (smtp-server.md § Connection-time limits, "Per-command read timeout | 30s",
	// the `mail.inbound.read_timeout_s` target). go-smtp resets this deadline
	// before every line read, so 30 s is per-command, not per-connection: a
	// healthy sender's lines (incl. DATA-body lines) arrive back-to-back well
	// under it, while a connection that stalls > 30 s between commands — the
	// slowloris pattern that would otherwise pin a global concurrency slot for up
	// to 5 min — is dropped. Tighter than postfix's 300 s `smtpd_timeout` default
	// (the doc's "tighter defaults than postfix" bar). A stall mid-transaction is
	// a retryable disconnect, not mail loss. This is the unauthenticated-MX
	// perimeter's concurrency-pressure relief, working with the per-IP *rate* cap
	// (10/min) and the per-IP *concurrent* cap (maxInboundConnsPerIP, wired below
	// via perIPInbound) to bound a patient trickle-slowloris that paces under the
	// rate cap and trickles a command every < 30 s to dodge this timeout.
	// Submission (465/587, runSubmissionListener) keeps 5 min: authenticated +
	// AUTH-gated + per-IP concurrent-capped. Compile-time for now (the per-surface
	// knob is pending with the nest-config rate-limiter buckets, like the other
	// connection-time constants).
	srv.ReadTimeout = 30 * time.Second
	// WriteTimeout stays generous: SMTP responses are tiny, so our writes are
	// never the slowloris vector, and a slow-reading client must not turn a
	// delivered message into a failure.
	srv.WriteTimeout = 5 * time.Minute
	srv.AllowInsecureAuth = false

	// Global concurrency cap (smtp-server.md § Connection-time limits):
	// back-pressure connection floods before they spawn unbounded
	// goroutines. The accepted/capped counter is this listener's only
	// metric emission (the per-message `verdict` labels remain a separate
	// doc-wide gap — smtp-server.md § Implementation status today).
	port := portLabel(l)
	// Per-IP concurrent cap (inner) under the global cap (outer): bound any single
	// source IP to maxInboundConnsPerIP of the global slots, so one (or looping)
	// source can't accumulate toward all of them — defense-in-depth beside the
	// per-IP rate cap + 30 s read timeout (smtp-server.md § Connection-time limits).
	perIP := perIPInbound(l, connlimit.NewPerIPLimiter(maxInboundConnsPerIP), port)
	limited := connlimit.New(perIP, maxInboundConns,
		func() { metrics.SMTPConnectionsTotal.WithLabelValues(port, "accepted").Inc() },
		func() { metrics.SMTPConnectionsTotal.WithLabelValues(port, "capped").Inc() },
	)

	logger.Info("smtp listener serving", "addr", l.Addr().String(), "domain", domain, "max_conns", maxInboundConns)

	serveErrCh := make(chan error, 1)
	go func() {
		serveErrCh <- srv.Serve(limited)
	}()

	select {
	case <-ctx.Done():
		// Graceful drain (mail-bridge-lifecycle.md § Shutting down): stop
		// accepting, answer 421 on new MAIL FROM, drain up to grace, then
		// force-close. serveLn is `limited` — the listener Serve accepts on.
		if gracefulDrain(srv, limited, drain, grace, serveErrCh, logger) {
			return bridgeshutdown.ErrShutdownForced
		}
		return nil
	case err := <-serveErrCh:
		if err != nil && !errors.Is(err, net.ErrClosed) && !errors.Is(err, gosmtp.ErrServerClosed) {
			return fmt.Errorf("smtp serve: %w", err)
		}
		return nil
	}
}

// portLabel extracts the port from a listener's bound address for the
// SMTPConnectionsTotal{port,…} metric. Falls back to "unknown" for an
// unparseable / nil address.
func portLabel(l net.Listener) string {
	if l == nil || l.Addr() == nil {
		return "unknown"
	}
	if _, p, err := net.SplitHostPort(l.Addr().String()); err == nil {
		return p
	}
	return "unknown"
}

// capSubmission wraps a raw submission listener (465/587) in the shared
// global connection cap (internal/connlimit, maxSubmissionConns), labelling
// accepted/capped saturation on SMTPConnectionsTotal{port}. ⚠ Wrap the *raw*
// socket below tls.NewListener for the implicit-TLS port (465): the
// wrapper's slotConn is not a *tls.Conn, and go-smtp only allows AUTH once
// it detects the connection is TLS. `port` is the bare port string for the
// metric label. Port 25 keeps its own (tighter) cap inside
// runListenerWithBackend; this is the authenticated-submission twin
// (mda.capListener is the IMAP/CalDAV twin).
func capSubmission(ln net.Listener, port string) net.Listener {
	return connlimit.New(ln, maxSubmissionConns,
		func() { metrics.SMTPConnectionsTotal.WithLabelValues(port, "accepted").Inc() },
		func() { metrics.SMTPConnectionsTotal.WithLabelValues(port, "capped").Inc() },
	)
}

// perIPSubmission wraps a raw submission listener (465/587) in the shared
// per-IP concurrent-connection limiter (connlimit.PerIPLimiter, fed from
// AuthPolicy.max_conn_per_ip), labelling shed connections on
// SMTPConnectionsTotal{port,"per_ip_shed"}. Wrap it BELOW capSubmission (so the
// global cap stays outermost, matching nest serve_tls's global-then-per-IP
// order) and directly on the raw socket (submission is published directly, so
// RemoteAddr() is already the real client IP — no PROXY-v2 peel). The CalDAV
// twin (mda.go) wraps proxyproto instead. `limiter` is shared across both
// submission ports.
func perIPSubmission(ln net.Listener, limiter *connlimit.PerIPLimiter, port string) net.Listener {
	return connlimit.NewPerIPListener(ln, limiter,
		func() { metrics.SMTPConnectionsTotal.WithLabelValues(port, "per_ip_shed").Inc() },
	)
}

// perIPInbound wraps the raw port-25 listener in the per-IP concurrent-connection
// limiter (connlimit.PerIPLimiter, fixed at maxInboundConnsPerIP). Unlike the
// authenticated submission/MDA surfaces — fed by AuthPolicy.max_conn_per_ip and
// hot-reloaded — port 25 is unauthenticated MX, so its ceiling is a compile-time
// constant with no config to track. Wrap it BELOW the global cap (connlimit.New)
// so the global cap stays outermost, matching the submission/MDA order. Port 25 is
// published directly (the MX A record), so RemoteAddr() is already the real client
// IP — no PROXY-v2 peel (the SNI router only fronts :443). Shed connections
// increment SMTPConnectionsTotal{port,"per_ip_shed"}, the same label as submission.
func perIPInbound(ln net.Listener, limiter *connlimit.PerIPLimiter, port string) net.Listener {
	return connlimit.NewPerIPListener(ln, limiter,
		func() { metrics.SMTPConnectionsTotal.WithLabelValues(port, "per_ip_shed").Inc() },
	)
}

// inboundBackend is the production go-smtp Backend for Phase C.1's
// skeleton + Phase C.2's connection-time policy + Phase C.3's
// recipient validation. NewSession runs the rate-limit / DNSBL /
// FCrDNS gates; Session.Mail runs the HELO syntactic + identity
// checks + FCrDNS-enforce gate; Session.Rcpt runs the domain filter
// + validate_recipient nest call + greylist gate. Phases C.4-C.9
// wrap parse / verify / score / tokenize / encrypt / ingest around
// this.
//
// `policy` may be nil — when it is, every connection-time gate no-ops
// and the backend behaves like the Phase C.1 skeleton; `caller` may
// also be nil — when it is, Session.Rcpt skips the
// validate_recipient call (Phase C.1 skeleton path; tests that don't
// care about recipient validation can leave it nil). Production
// wiring sets both from `Run` via deps.Snapshot and deps.Client.
//
// `maxMessageBytes` is the snapshot-driven pre-parser size cap (Phase
// C.4); zero disables the cap and is intended for tests that don't
// exercise the size guard. The production path always populates a
// non-zero value (default 50_000_000 from
// `default_config_reply` in bins/fauna-nest/src/bridge_routing_handlers.rs).
type inboundBackend struct {
	logger *slog.Logger
	caller wsrpc.Caller // nil → skip resolve_recipient (skeleton path)
	// bytePlane stages an oversized sealed body on the nest's bulk-byte plane, so
	// a message the 2 MiB WS-RPC frame cannot carry still delivers (smtp-server.md
	// § Message size limits). nil in unit tests that never seal an over-budget
	// body → stageSealedBody surfaces a transient error rather than panicking.
	bytePlane *byteplane.Client
	// cfg is the hot-swappable MTA config holder, shared with the submission
	// backend in this process. NewSession reads the current bundle once at the
	// request boundary (b.cfg.current()) and value-copies its connection-time
	// Policy, the active local-domains list (RCPT TO for any domain not in it
	// is rejected as relay 550 5.7.1; empty ⇒ no RCPT accepted, mta.Run idles
	// first), the pre-parser size cap, and the auth + spam policies into the
	// inboundSession. A `fauna.bridges.config_changed` hot-apply therefore
	// takes effect on the next connection without disturbing in-flight ones
	// (mail-bridge-lifecycle.md § Running — hot-reload mandatory). nil in
	// drain-only unit tests → current() yields an empty bundle (no rate gate,
	// no local domains), the same lenient shape the old zero-value fields gave.
	cfg *mtaConfigHolder

	// scanConfig carries the T1.4 content-scan deployment topology (clamd /
	// rspamd addresses) + the scan policy. The clamd/rspamd addresses are
	// operator-hatch deployment topology; the policy is scan.PolicyDefault in
	// T1.4 (snapshot projection deferred).
	// Zero-value Policy (ClamavEnabled=false, RspamdEnabled=false) makes the
	// gate a no-op, so Phase C unit tests that leave it unset reach the
	// post-gate path with a NotScanned verdict (never Clean: nothing ran).
	scanConfig scan.Config

	// scanGate holds the process-wide content-scan runtime guards (in-flight
	// cap + per-scanner circuit breakers; D7). Unlike the value-copied
	// scanConfig, this is shared by pointer into every session so the breaker
	// state and concurrency budget are global to the MTA. nil in unit-test
	// backends that don't construct it ⇒ applyScanGate runs ungated.
	scanGate *scanGate

	// requireStartTLS enforces InboundTLSMode=required (smtp-server.md
	// § TLS posture per port): when true, inboundSession.Mail rejects
	// MAIL FROM on a not-yet-STARTTLS'd connection with 530 5.7.10. Set
	// true by mta.Run iff a TLS config is wired (TLSProvider != nil) —
	// the same condition under which the listener advertises STARTTLS.
	// False on a TLS-unprovisioned bridge (plaintext-only inbound).
	requireStartTLS bool

	// drain coordinates graceful shutdown (T2.6): NewSession registers each
	// connection, Logout deregisters it, and once draining is flipped on
	// SIGTERM, Session.Mail answers 421 4.3.2 on any new MAIL FROM. nil in
	// unit-test backends that don't exercise shutdown — the gate then no-ops.
	drain *drainTracker

	// outboundTrigger pokes the outbound worker after a forward-all stage
	// enqueues a forward row, so the forward delivers on the next poll cycle
	// instead of waiting the full PollInterval (mirrors the submission Data
	// hook — mail-forwarding N4 latency polish). nil on a TLS-unprovisioned
	// bridge (no outbound worker) and in unit-test backends — the nudge no-ops.
	outboundTrigger func()
}

func (b *inboundBackend) NewSession(c *gosmtp.Conn) (gosmtp.Session, error) {
	ip, _, _ := net.SplitHostPort(c.Conn().RemoteAddr().String())
	if b.drain != nil {
		b.drain.enter()
	}
	// Read the hot-swappable config once at the request boundary and value-copy
	// it into the session, so a `config_changed` swap that lands mid-connection
	// never tears this session's view (hot-reload applies to the NEXT
	// connection). lc is never nil (current() is nil-safe).
	lc := b.cfg.current()
	sess := &inboundSession{
		conn:                     c,
		logger:                   b.logger,
		clientIP:                 ip,
		policy:                   lc.policy,
		caller:                   b.caller,
		bytePlane:                b.bytePlane,
		localDomains:             lc.localDomains,
		outboundTrigger:          b.outboundTrigger,
		maxMessageBytes:          lc.maxMessageBytes,
		authPolicy:               lc.authPolicy,
		spamPolicy:               lc.spamPolicy,
		unlistedRecipientPenalty: lc.unlistedRecipientPenalty,
		scanConfig:               b.scanConfig,
		scanGate:                 b.scanGate,
		requireStartTLS:          b.requireStartTLS,
		drain:                    b.drain,
	}
	if lc.policy != nil {
		// Per-IP rate limit. Reject at NewSession so botnet floods
		// don't even reach the HELO banner exchange.
		if !lc.policy.RateLimiter.Allow(ip) {
			b.logger.Warn("smtp: rate-limited",
				"client_ip", ip,
				"limit", lc.policy.RateLimiter.limit,
				"window", lc.policy.RateLimiter.window.String(),
			)
			return nil, &gosmtp.SMTPError{
				Code:         421,
				EnhancedCode: gosmtp.EnhancedCode{4, 7, 0},
				Message:      "Connection rate limit exceeded; try again later",
			}
		}
		// DNSBL — reject-class hits short-circuit at NewSession (a
		// perimeter hard-gate, independent of the content score). Soft
		// score-class hits no longer feed a Fauna-side heuristic: rspamd
		// is the sole deployment-wide content scorer (T3.1), and its RBL
		// module covers score-class signals.
		dr := lc.policy.DNSBL.Check(ip)
		if dr.Rejected {
			b.logger.Info("smtp: DNSBL reject",
				"client_ip", ip, "reason", dr.RejectReason)
			return nil, &gosmtp.SMTPError{
				Code:         554,
				EnhancedCode: gosmtp.EnhancedCode{5, 7, 1},
				Message:      "Connection refused: " + dr.RejectReason,
			}
		}
		// FCrDNS — captured here for the once-per-session enforce
		// gate at Session.Mail (where we have HELO and the policy's
		// enforce-mode + reject_fcrdns_fail intersection can fire).
		if lc.policy.FCrDNSMode != FCrDNSModeOff {
			sess.fcrdns = lc.policy.FCrDNS.Check(ip)
			sess.fcrdnsResolved = true
		}
	}
	return sess, nil
}

// resolvedInboundRcpt is a RCPT TO that passed inbound recipient
// resolution: `resolve_recipient` resolved it to `actorID`, and
// `isRoleAddress` records whether it resolved via a role-address route
// (postmaster@/abuse@/security@/…). Role-address deliveries bypass the
// recipient's per-mailbox quota on ingest (smtp-server.md :204), carried to
// `ingest_inbound_mail` so an over-quota admin mailbox still receives
// postmaster mail.
type resolvedInboundRcpt struct {
	actorID       []byte
	isRoleAddress bool

	// rcptAddr is the RCPT TO address as the sender wrote it (the same value
	// pushed onto the parallel `inboundSession.rcpts` slice). Carried on the
	// struct so per-recipient DATA-stage logic (e.g. the AutoReply `From:`
	// header) doesn't depend on the rcpts/inboundRcpts index alignment holding.
	rcptAddr string

	// srsBounce marks a recipient that resolved via the inbound SRS-bounce
	// path (mail-forwarding N4): the RCPT was an `SRS0=`/`SRS1=` address that
	// nest decoded + verified, and `actorID` is the *forwarding-config owner*
	// the bounce routes to (NOT the original sender — mail-forwarding.md
	// § Bounce decode / § NDR routing). The DATA stage delivers the bounce to
	// this actor's mailbox and skips the forward-all stage (a bounce is never
	// re-forwarded, :255).
	srsBounce bool
	// srsBounceOrphan marks a verified-ours SRS bounce whose forwarding row is
	// gone (account deleted / row pruned). The RCPT is accepted, but the DATA
	// stage drops it (it carries the original sender's PII and there is no
	// mailbox to land it in — never the admin's, :104,:117). `actorID` is nil.
	srsBounceOrphan bool

	// forward marks an admin external forwarder match (mail-aliases.md § Kind 7):
	// the RCPT resolved to a `kind='forwarder'` alias with no local mailbox. The
	// DATA stage redirects the message to `forwardTarget` (attributed to the
	// managing-admin `forwarderActorID`) through the shared forward dispatch and
	// writes NO local copy (redirect-shaped, mail-forwarding.md:59). `actorID` is
	// nil; `headersToStamp`/`controlOverrides` are unused.
	forward          bool
	forwardTarget    string
	forwarderActorID []byte

	// headersToStamp carries the resolver's `X-Fauna-Address-*` headers for an
	// alias-kind match (subaddress / wildcard / disposable / catch-all —
	// aliases/mod.rs:55). The DATA stage stamps them onto this recipient's sealed
	// copy AND into this recipient's filter-rule context so a rule can match on
	// the matched-alias metadata (mail-aliases.md § Resolution order). Empty for
	// exact / role-address (and for the forwarder/SRS-bounce paths).
	headersToStamp []wsrpc.StampedHeader

	// controlOverrides carries the resolver's per-alias spam-threshold / rate-cap
	// overrides. Plumbed through the cutover but NOT yet enforced.
	//
	// ⚠ The producer HAS arrived (corrected 2026-08-17): the alias-detail control
	// UI is built and sends non-None spam_threshold_override /
	// rate_limit_per_hour — tui's add-sheet parses both through the shared
	// validators (mail-aliases.md § Where the override-input parse lives), so a
	// user can set a cap today and nothing honours it. This comment previously
	// said no producer existed, which read as "unreachable, ignore it"; the gap is
	// now the enforcement gate alone. Owner + declared status:
	// mail-aliases.md § Per-alias rate-cap and its § Implementation status today.
	// Carried so the consumer wiring lands in one place.
	controlOverrides wsrpc.AliasControls
}

// inboundSession is the per-connection state. Phase C.1's skeleton
// fields (clientIP, from, rcpts) survive; C.2 adds the policy-related
// fields (policy, fcrdns, mailChecked); C.3 adds the
// recipient-validation surface (caller, domain, inboundRcpts).
//
// `inboundRcpts` is the parallel slice to `rcpts` — for the i-th
// successful Rcpt call, `inboundRcpts[i]` carries the 32-byte nest-resolved
// actor identifier the C.9 ingest call keys on (plus its role-address
// bit). RCPTs rejected by the domain filter or resolve_recipient don't
// make it into either slice (go-smtp drops the recipient on error return).
type inboundSession struct {
	conn         *gosmtp.Conn
	logger       *slog.Logger
	clientIP     string
	from         string
	rcpts        []string
	inboundRcpts []resolvedInboundRcpt
	// hasDiscardRcpt is set when this transaction accepted a discard-only
	// envelope-command RCPT (RFC 8058 mailto unsubscribe — resolve_recipient
	// returned ResolveDiscard). Transaction-scoped, reset with `rcpts` at MAIL
	// FROM / RSET. At DATA, a discard-only envelope (this set, no deliverable
	// inboundRcpts) is accepted (250) and its body dropped before the
	// parse/auth/scan pipeline.
	hasDiscardRcpt bool

	policy         *Policy
	fcrdns         FCrDNSResult
	fcrdnsResolved bool
	mailChecked    bool

	// errCount is the running count of policy rejections on this session.
	// It drives the escalating tarpit (tarpitDelay) and persists across
	// Reset() — a RSET'd session keeps accumulating. sleep is the tarpit
	// sleeper, injectable so unit tests stay fast and deterministic; nil ⇒
	// time.Sleep (the production path).
	errCount int
	sleep    func(time.Duration)

	// requireStartTLS mirrors the backend flag: when true, Mail rejects
	// the envelope on a pre-STARTTLS connection (InboundTLSMode=required).
	requireStartTLS bool

	// drain is the backend's shutdown coordinator (T2.6); Mail consults its
	// draining flag and Logout deregisters the session. loggedOut guards the
	// single leave() — go-smtp may invoke Logout from more than one path.
	drain     *drainTracker
	loggedOut sync.Once

	caller       wsrpc.Caller
	localDomains []string
	// bytePlane stages an oversized sealed body on the bulk-byte plane. Copied
	// from the backend at NewSession, like every other session-scoped dep.
	bytePlane *byteplane.Client

	// outboundTrigger pokes the outbound worker after the forward-all stage
	// enqueues a forward (mail-forwarding N4); nil ⇒ no nudge (the worker still
	// drains on its poll cadence). Copied from the backend at NewSession.
	outboundTrigger func()

	// maxMessageBytes is the per-session pre-parser size cap copied
	// from the backend (Phase C.4). Zero disables the cap (test-only).
	maxMessageBytes uint32

	// authPolicy carries the AuthPolicy snapshot at session start
	// (Phase C.6+). Read by Session.Data to drive the DKIM
	// enforce-on-fail gate after verify_inbound returns.
	authPolicy wsrpc.AuthPolicy

	// parsed is the C.4 parse result, populated by Session.Data on
	// successful ParseRFC5322 and left nil if Data returns early
	// (size guard tripped or ParseRFC5322 errored). Phases C.5-C.9
	// (verify/score/tokenize/encrypt/ingest) read from this field.
	parsed *mailfauna.ParsedMessage

	// verdicts is the C.5 verify_inbound result, populated by
	// Session.Data after a successful ParseRFC5322. Phase C.6's DKIM
	// enforce-on-fail gate and C.7's spam scorer read from this field;
	// C.9 passes them to ingest_inbound_mail via the wsrpc.AuthVerdicts
	// wire-shape mirror. Nil when Data returned before the verify call
	// (size guard / parse failure) or when verify itself errored.
	verdicts *mailfauna.AuthVerdicts

	// spamPolicy is the snapshot-derived scorer config (the combined-score
	// tier thresholds); fed into mailfauna.DecideSpamDisposition at Data
	// time. Constructed once at NewSession time from the backend's
	// spamPolicy field; per-session copy survives mid-session
	// config-changed pushes (which land on the backend; the session
	// continues with its starting policy).
	spamPolicy mailfauna.SpamPolicy
	// unlistedRecipientPenalty (points, 0 = off) is the deployment-wide
	// recipient-whitelist penalty added to a catch-all recipient's combined
	// score in the per-recipient delivery loop (`mail-spam.md`
	// § Unlisted-recipient penalty). Threaded separately from spamPolicy (the
	// UniFFI scorer Record stays unchanged). Same per-session-copy lifetime.
	unlistedRecipientPenalty uint32

	// scanConfig is the T1.4 content-scan config copied from the backend at
	// NewSession (clamd/rspamd addresses + scan policy). Read by Session.Data
	// to run applyScanGate, which produces the rspamd score that drives the
	// T3.1 combined-score disposition.
	scanConfig scan.Config

	// scanGate is the process-wide content-scan runtime guard (D7), shared by
	// pointer from the backend (NOT value-copied like scanConfig) so its
	// breaker state + in-flight budget are global. nil ⇒ ungated.
	scanGate *scanGate

	// indexHint holds the C.8 tokenize result on the message body +
	// subject; populated by Session.Data once the C.7 spam gate has
	// accepted the message. Phase C.9 encrypts
	// indexHint.CanonicalBytes to the recipient's index key as the
	// encrypted_index_hint argument of
	// fauna.bridges.ingest_inbound_mail. Nil when Data hasn't reached
	// C.8 yet (size guard / parse / verify / spam reject paths).
	indexHint *mailfauna.CanonicalTokenSet
}

// AuthMechanisms returns nil — inbound MX SMTP doesn't advertise
// AUTH. Phase D's submission listeners (465/587) will, in their own
// backend type.
func (s *inboundSession) AuthMechanisms() []string { return nil }

// Auth rejects every attempt — inbound MX SMTP doesn't support AUTH.
func (s *inboundSession) Auth(string) (sasl.Server, error) {
	return nil, &gosmtp.SMTPError{
		Code:         503,
		EnhancedCode: gosmtp.EnhancedCode{5, 5, 1},
		Message:      "Authentication not supported",
	}
}

// tarpitDelay returns the per-session tarpit sleep for the n-th policy
// rejection: tarpitBase·n clamped to tarpitCap (smtp-server.md
// § Connection-time limits — 250ms × n_errors, capped at 5s).
func tarpitDelay(n int) time.Duration {
	d := time.Duration(n) * tarpitBase
	if d > tarpitCap {
		return tarpitCap
	}
	return d
}

// reject applies the escalating connection tarpit and returns err. Call
// it for every *policy* rejection — every 5xx permanent reject plus the
// deliberate greylist 451: it bumps the session's rejection count and
// sleeps tarpitDelay(count) before the reply is written, so an abuser
// probing for valid addresses pays a growing cost. Infra-class tempfails
// (the 451 "nest unavailable" paths: validate_recipient transport,
// verify_inbound transient, ingest tempfail) deliberately do NOT route
// through reject — we never slow a legitimate sender's retry because our
// own backend blipped.
func (s *inboundSession) reject(err *gosmtp.SMTPError) *gosmtp.SMTPError {
	s.errCount++
	sleep := s.sleep
	if sleep == nil {
		sleep = time.Sleep
	}
	sleep(tarpitDelay(s.errCount))
	return err
}

// checkHeaderSection is the parser-bomb defense (smtp-server.md
// § Connection-time limits and the 554 5.6.0 row in § Error / tempfail):
// it scans the RFC-5322 header section — the bytes before the first
// `\r\n\r\n` — and rejects with 554 5.6.0 when the header exceeds
// maxHeaderLines physical lines, exceeds maxHeaderBytes, or has no body
// separator within the byte cap (an unterminated header is a bomb). A
// short message that simply ends before any separator is not the cap's
// concern — it's left to the RFC-5322 parser, which 554s it too. Runs
// before mailfauna.ParseRFC5322 so a bomb never reaches the UniFFI parser.
func checkHeaderSection(raw []byte) *gosmtp.SMTPError {
	sep := []byte("\r\n\r\n")
	window := raw
	capped := false
	if len(window) > maxHeaderBytes {
		window = window[:maxHeaderBytes]
		capped = true
	}
	idx := bytes.Index(window, sep)
	if idx < 0 {
		if capped {
			// Scanned the full byte cap without a header/body separator.
			return headerCapErr("header section exceeds byte cap with no body separator")
		}
		// EOF before the cap and before a separator — defer to the parser.
		return nil
	}
	// Header section is raw[:idx]; count its physical (CRLF-terminated)
	// field lines (folded continuations count as separate physical lines,
	// which is correct for a line-cap defense).
	if lines := bytes.Count(raw[:idx], []byte("\r\n")) + 1; lines > maxHeaderLines {
		return headerCapErr("header section exceeds line cap")
	}
	return nil
}

func headerCapErr(reason string) *gosmtp.SMTPError {
	return &gosmtp.SMTPError{
		Code:         554,
		EnhancedCode: gosmtp.EnhancedCode{5, 6, 0},
		Message:      "Header section rejected: " + reason,
	}
}

// checkFromFieldCount refuses a message whose header section carries other
// than exactly one From field with 554 5.6.0, returning the count for the log
// line. Shared by the inbound MX and submission DATA stages; the count itself is
// shared Rust (mailfauna.FromFieldCount) so every door counts the same way.
func checkFromFieldCount(raw []byte) (*gosmtp.SMTPError, uint32) {
	n := mailfauna.FromFieldCount(raw)
	if n == 1 {
		return nil, n
	}
	return &gosmtp.SMTPError{
		Code:         554,
		EnhancedCode: gosmtp.EnhancedCode{5, 6, 0},
		Message:      "Message must carry exactly one From header field",
	}, n
}

// tlsParamsForReceived returns the TLS version + cipher labels for the
// `Received:` header's `with ESMTPS (<ver> <cipher>)` clause. Both empty on a
// cleartext (or nil-conn unit-test) session → the builder emits `with ESMTP`
// with no cipher parenthetical. Mirrors the RFC 8314 §4.1 TLS-extension fields
// the retired terminator stamped.
func tlsParamsForReceived(conn *gosmtp.Conn) (version, cipher string) {
	if conn == nil {
		return "", ""
	}
	state, ok := conn.TLSConnectionState()
	if !ok {
		return "", ""
	}
	switch state.Version {
	case tls.VersionTLS13:
		version = "TLS1.3"
	case tls.VersionTLS12:
		version = "TLS1.2"
	case tls.VersionTLS11:
		version = "TLS1.1"
	case tls.VersionTLS10:
		version = "TLS1.0"
	default:
		version = "TLS"
	}
	return version, tls.CipherSuiteName(state.CipherSuite)
}

func (s *inboundSession) Mail(from string, _ *gosmtp.MailOptions) error {
	// Graceful shutdown (mail-bridge-lifecycle.md § Shutting down step 2):
	// once the bridge is draining, refuse new transactions with 421 4.3.2 so
	// the sending MTA retries against a healthy instance. Fires before every
	// other gate — a shutting-down bridge must not begin new work. Returned
	// directly (not via reject): this is our own lifecycle, not a sender
	// fault, so we don't tarpit a legitimate retry — same rationale as the
	// 451 infra-tempfail paths (see reject's doc comment).
	if s.drain != nil && s.drain.isDraining() {
		return &gosmtp.SMTPError{
			Code:         421,
			EnhancedCode: gosmtp.EnhancedCode{4, 3, 2},
			Message:      "Service shutting down",
		}
	}
	// InboundTLSMode=required (smtp-server.md § TLS posture per port):
	// reject the envelope until the connection has upgraded via STARTTLS.
	// Fires before any policy gate — and regardless of whether a spam
	// policy is wired — so cleartext senders on port 25 never transmit.
	// Verdict: rejected_no_starttls. (No-op when requireStartTLS is
	// false, i.e. the bridge has no TLS provisioned and binds 25
	// plaintext-only.)
	if s.requireStartTLS {
		if _, isTLS := s.conn.TLSConnectionState(); !isTLS {
			if s.logger != nil {
				s.logger.Info("smtp: STARTTLS required, rejecting cleartext MAIL FROM",
					"client_ip", s.clientIP)
			}
			return s.reject(&gosmtp.SMTPError{
				Code:         530,
				EnhancedCode: gosmtp.EnhancedCode{5, 7, 10},
				Message:      "STARTTLS required (RFC 3207)",
			})
		}
	}
	s.from = from
	s.rcpts = nil
	s.hasDiscardRcpt = false
	if s.policy == nil {
		return nil
	}
	if !s.mailChecked {
		s.mailChecked = true
		// HELO/EHLO syntactic check — fires once per session at the
		// first MAIL FROM (go-smtp doesn't expose a HELO callback;
		// reading c.Hostname() here is the equivalent hook).
		helo := strings.TrimSpace(s.conn.Hostname())
		if err := validateHELOSyntax(helo, s.clientIP); err != nil {
			s.logger.Info("smtp: invalid HELO", "client_ip", s.clientIP, "helo", helo, "err", err)
			return s.reject(&gosmtp.SMTPError{
				Code:         554,
				EnhancedCode: gosmtp.EnhancedCode{5, 7, 0},
				Message:      "Invalid HELO/EHLO: " + err.Error(),
			})
		}
		// HELO identity (DNS-based) — production default rejects when
		// the HELO domain doesn't A-resolve to a set including the
		// peer IP. Loopback exemption fires inside ValidateHELOIdentity
		// unless SkipHELOLoopback is set (tests pass true).
		if s.policy.HELOIdentityRequired {
			ok, _, reason := ValidateHELOIdentity(
				helo, s.clientIP,
				s.policy.HELOResolver,
				s.policy.SkipHELOLoopback,
				s.policy.HELOLookupTimeout,
			)
			if !ok {
				s.logger.Info("smtp: HELO identity reject",
					"client_ip", s.clientIP, "helo", helo, "reason", reason)
				return s.reject(&gosmtp.SMTPError{
					Code:         554,
					EnhancedCode: gosmtp.EnhancedCode{5, 7, 0},
					Message:      "HELO identity check failed: " + reason,
				})
			}
		}
		// FCrDNS enforce — only fires when mode=enforce AND
		// reject_fcrdns_fail AND the check was a definite fail (not
		// fail-open).
		if s.policy.FCrDNSMode == FCrDNSModeEnforce &&
			s.policy.RejectFCrDNSFail &&
			s.fcrdnsResolved &&
			!s.fcrdns.Passed &&
			!s.fcrdns.FailOpen {
			s.logger.Info("smtp: FCrDNS enforce reject",
				"client_ip", s.clientIP, "reason", s.fcrdns.Reason)
			return s.reject(&gosmtp.SMTPError{
				Code:         550,
				EnhancedCode: gosmtp.EnhancedCode{5, 7, 25},
				Message:      "FCrDNS check failed: " + s.fcrdns.Reason,
			})
		}
	}
	// Sender-domain MX/A check (smtp-server.md § Sender-domain). Runs on
	// every MAIL FROM — the envelope sender can differ across an RSET, so
	// this is NOT once-per-session like the HELO gates above. Two bypasses:
	// the null sender `<>` (from == "", RFC 5321 §4.5.5 bounce traffic),
	// and loopback peers — local relays / cron legitimately send from
	// non-resolving sender domains, mirroring postfix's permit_mynetworks-
	// before-reject_unknown_sender_domain ordering and the HELO-identity
	// loopback exemption (SkipSenderDomainLoopback forces the check on for
	// wire-level tests).
	if from != "" && s.policy.SenderDomain != nil &&
		(s.policy.SkipSenderDomainLoopback || !isLoopbackIP(s.clientIP)) {
		domain := senderEnvelopeDomain(from)
		if domain == "" {
			s.logger.Info("smtp: malformed sender (no domain)", "client_ip", s.clientIP)
			return s.reject(&gosmtp.SMTPError{
				Code:         554,
				EnhancedCode: gosmtp.EnhancedCode{5, 1, 7},
				Message:      "Invalid sender address",
			})
		}
		if ok, reason := s.policy.SenderDomain.CheckSenderDomain(domain); !ok {
			s.logger.Info("smtp: sender-domain reject",
				"client_ip", s.clientIP, "sender_domain", domain, "reason", reason)
			return s.reject(&gosmtp.SMTPError{
				Code:         550,
				EnhancedCode: gosmtp.EnhancedCode{5, 7, 1},
				Message:      "Sender domain has no DNS records",
			})
		}
	}
	return nil
}

// senderEnvelopeDomain extracts the lower-cased domain from a MAIL FROM
// envelope address, stripping any angle brackets go-smtp left in place.
// Returns "" when the address has no `@` or an empty domain part — the
// caller maps that to 554 5.1.7.
func senderEnvelopeDomain(from string) string {
	at := strings.LastIndex(from, "@")
	if at < 0 {
		return ""
	}
	domain := from[at+1:]
	domain = strings.TrimSuffix(strings.TrimPrefix(domain, "<"), ">")
	return strings.ToLower(strings.TrimSpace(domain))
}

func (s *inboundSession) Rcpt(to string, _ *gosmtp.RcptOptions) error {
	// Domain filter — RCPT TO for any domain not in the deployment's
	// active mail_domains list is relay (550 5.7.1). The list arrives
	// via fauna.bridges.fetch_config's `local_domains` projection
	// (docs/goal/behavior/mail-multidomain.md § Architectural rules
	// → "The `local_domains` list is a derived projection"). Cheap,
	// no nest call.
	localPart, rcptDomain, err := splitRcptAddress(to)
	if err != nil {
		return s.reject(&gosmtp.SMTPError{
			Code:         501,
			EnhancedCode: gosmtp.EnhancedCode{5, 5, 2},
			Message:      "RCPT TO syntax error: " + err.Error(),
		})
	}
	if !containsFoldDomain(s.localDomains, rcptDomain) {
		s.logger.Info("smtp: relay rejected (foreign domain)",
			"client_ip", s.clientIP,
			"rcpt_domain", rcptDomain,
			"served_domains_count", len(s.localDomains),
		)
		return s.reject(&gosmtp.SMTPError{
			Code:         550,
			EnhancedCode: gosmtp.EnhancedCode{5, 7, 1},
			Message:      "Relay access denied",
		})
	}
	// mail-forwarding N4 — inbound SRS bounce recognition. A permanent-failure
	// bounce of one of our forwards comes back addressed to
	// `SRS0=…`/`SRS1=…@<our-domain>` — the envelope MAIL FROM we rewrote at
	// queue-out (N3). Recognize the SRS local-part here and have nest
	// decode+verify it against the deployment SRS secret(s); on a verified
	// bounce the DATA stage delivers it to the *forwarder*, not the original
	// sender (mail-forwarding.md § Bounce decode / § NDR routing). `not_srs`
	// (the prefix matched our cheap check but nest disagrees) falls through to
	// the normal recipient path. Skipped when caller is nil (skeleton tests).
	if s.caller != nil && isSrsBounceLocalPart(localPart) {
		if done, err := s.tryRcptSrsBounce(to, localPart); done {
			return err
		}
		// not_srs → fall through to resolve_recipient + greylist below.
	}
	// resolve_recipient — the fixed-order RCPT-TO resolver (mail-aliases.md
	// § Resolution order: exact → forwarder → +suffix → disposable → wildcard →
	// role-address → catch-all → 550), the superset of validate_recipient. It
	// yields one of three outcomes: a local actor (Resolved), an admin external
	// forwarder redirect (Forward), or a typed Reject. The MAIL FROM domain rides
	// along as `sender_domain` for the resolver's alias-hit audit log. Skipped
	// when caller is nil (Phase C.1 skeleton tests).
	if s.caller != nil {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		// `s.from` (the full envelope MAIL FROM) rides alongside its domain: the
		// nest's guardian mail gate keys on the whole address, and answers a
		// typed `550 5.7.1` Reject for a ward whose guardian set
		// `unknown_sender_mail = reject` (`family-safety.md` § The mail gate).
		// This is the only per-recipient stage — a refusal decided at DATA would
		// bounce the message for the ward's co-recipients too.
		res, rErr := wsrpc.ResolveRecipient(ctx, s.caller, localPart, rcptDomain, senderDomainOf(s.from), s.from)
		cancel()
		if rErr != nil {
			// Transport / decode / unknown-outcome / malformed payload — tempfail
			// so a transient nest blip doesn't drop legitimate mail.
			s.logger.Warn("smtp: resolve_recipient transport error",
				"client_ip", s.clientIP, "local_part", localPart, "domain", rcptDomain, "err", rErr)
			return &gosmtp.SMTPError{
				Code:         451,
				EnhancedCode: gosmtp.EnhancedCode{4, 7, 0},
				Message:      "Recipient validation temporarily unavailable; try again later",
			}
		}
		switch res.Outcome {
		case wsrpc.ResolveResolved:
			// RCPT-time key check (security-review), mirroring
			// submission's resolveLocalRecipient: confirm the recipient has an
			// MLS pubkey on file before accepting the RCPT, so a key-less
			// recipient is dropped from the envelope here — the others
			// unaffected — instead of failing the whole DATA transaction after
			// earlier recipients already committed (smtp-server.md § Recipient
			// handling on submission's partial-failure guarantee, extended to
			// the inbound MX arm; § Error / tempfail strategy rows :288-289).
			// Role addresses are exempt: they never reject at recipient-validate
			// time (smtp-server.md § abuse@ / postmaster@ routing, :234) — the
			// DATA-time preflight (below) remains their sole key check, same as
			// before this change. mailNewIngest=false: this is validation only,
			// not the genuine per-delivery resolution (that's
			// ingestForRecipient's ResolveRecipientSealKeys call, at DATA).
			if !res.IsRoleAddress {
				ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
				_, _, keyErr := checkRecipientMLSPubkeyAtRcpt(ctx, s.caller, res.ActorID, to, false, s.logger)
				cancel()
				if keyErr != nil {
					s.logger.Info("smtp: rcpt-time mls pubkey check failed",
						"client_ip", s.clientIP, "local_part", localPart, "domain", rcptDomain,
						"smtp_code", keyErr.Code, "verdict", "rcpt_no_key")
					return keyErr
				}
			}
			s.inboundRcpts = append(s.inboundRcpts, resolvedInboundRcpt{
				actorID:          res.ActorID,
				isRoleAddress:    res.IsRoleAddress,
				rcptAddr:         to,
				headersToStamp:   res.HeadersToStamp,
				controlOverrides: res.ControlOverrides,
			})
		case wsrpc.ResolveForward:
			// Admin external forwarder (mail-aliases.md § Kind 7): no local
			// mailbox. Recorded as a forward recipient so it flows through the
			// shared greylist + DATA pipeline; the DATA stage redirects it to the
			// external target (copy_mode=redirect) and writes no local copy.
			s.inboundRcpts = append(s.inboundRcpts, resolvedInboundRcpt{
				forward:          true,
				forwardTarget:    res.ForwardTarget,
				forwarderActorID: res.ForwarderActorID,
				rcptAddr:         to,
			})
		case wsrpc.ResolveReject:
			s.logger.Info("smtp: resolve_recipient reject",
				"client_ip", s.clientIP, "local_part", localPart, "domain", rcptDomain,
				"smtp_code", res.SMTPCode, "reason", res.Reason)
			return s.reject(rejectFromResolver(res.SMTPCode, res.Reason))
		case wsrpc.ResolveDiscard:
			// RFC 8058 mailto one-click unsubscribe (mail-mass-mailing.md
			// § The mailto handler): the nest already performed the unsubscribe
			// during resolution (fire-and-forget, idempotent). Accept the RCPT
			// with 250 but record it as discard-only so DATA drops the body
			// without delivering to a mailbox. Not appended to `rcpts` /
			// `inboundRcpts` (no delivery), and greylisting is skipped — a
			// fire-and-forget command is never deferred.
			s.hasDiscardRcpt = true
			s.logger.Info("smtp: resolve_recipient discard (mailto unsubscribe)",
				"client_ip", s.clientIP, "local_part", localPart, "domain", rcptDomain)
			return nil
		}
	}
	// Greylist (post-validate) — state lives **nest-side**
	// (`smtp-server.md` § Greylisting): we forward the envelope and nest
	// derives the tuple + applies the policy (incl. the enabled toggle and the
	// :205 role-address bypass), so behavior is uniform across bridge restart
	// (the in-process map this replaced was wiped on every supervisor-restart).
	// Only fires for recipients we'd otherwise accept, so we don't tempfail
	// mail we'd anyway 5xx. Skipped when caller is nil (Phase C.1 skeleton).
	if s.caller != nil {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		pass, gErr := wsrpc.CheckGreylist(ctx, s.caller, s.from, to, s.clientIP)
		cancel()
		switch {
		case gErr != nil:
			// Fail OPEN — never tempfail legitimate mail on our own backend
			// blip. Greylisting is itself a deferral, so the safe direction
			// is accept (unlike validate_recipient, which fails closed to
			// 451). Logged + metered so admins can spot a sick nest
			// silently bypassing greylisting.
			metrics.SMTPGreylistCheckFailOpen.Inc()
			s.logger.Warn("smtp: greylist check failed open",
				"client_ip", s.clientIP, "from", s.from, "to", to, "err", gErr)
		case !pass:
			// Pop the just-recorded recipient so a retry doesn't
			// double-count.
			if len(s.inboundRcpts) > 0 {
				s.inboundRcpts = s.inboundRcpts[:len(s.inboundRcpts)-1]
			}
			metrics.SMTPInboundMessagesTotal.WithLabelValues("rejected_greylist").Inc()
			s.logger.Info("smtp: greylist tempfail",
				"client_ip", s.clientIP, "from", s.from, "to", to)
			return s.reject(&gosmtp.SMTPError{
				Code:         451,
				EnhancedCode: gosmtp.EnhancedCode{4, 7, 1},
				Message:      "Greylisted; try again later",
			})
		}
	}
	s.rcpts = append(s.rcpts, to)
	return nil
}

// containsFoldDomain returns true when `needle` matches any entry in
// `haystack` under ASCII case-folding (RFC 1035 § 2.3.3 — domain
// labels compare case-insensitively). Empty haystack ⇒ false (no
// local domain accepts mail). Used by Rcpt to decide whether RCPT
// TO is for one of our local_domains.
func containsFoldDomain(haystack []string, needle string) bool {
	for _, d := range haystack {
		if strings.EqualFold(d, needle) {
			return true
		}
	}
	return false
}

// senderDomainOf extracts the domain of an envelope MAIL FROM for the
// resolve_recipient `sender_domain` audit field (the resolver logs it on an
// alias hit, bridge_routing_handlers.rs:750). A null sender (`MAIL FROM: <>`)
// or an address with no `@` yields the empty string (`#[serde(default)]`
// nest-side keeps it optional).
func senderDomainOf(from string) string {
	if at := strings.LastIndexByte(from, '@'); at >= 0 && at < len(from)-1 {
		return from[at+1:]
	}
	return ""
}

// rejectFromResolver maps a resolve_recipient Reject outcome (smtp_code +
// nest-supplied reason — e.g. "User unknown" / "Address expired" /
// "Address disabled", aliases/mod.rs) to the SMTP wire. The enhanced code is
// derived from the basic code's class (5xx → 5.1.1 permanent user-unknown-ish;
// 4xx → 4.7.0 transient); a zero/blank reply defaults to the conventional
// 550 5.1.1. The reason is nest-authored (not sender-controlled), so it rides
// to the wire as-is.
func rejectFromResolver(code uint16, reason string) *gosmtp.SMTPError {
	if code == 0 {
		code = 550
	}
	if strings.TrimSpace(reason) == "" {
		reason = "Recipient rejected"
	}
	enh := gosmtp.EnhancedCode{5, 1, 1}
	if code >= 400 && code < 500 {
		enh = gosmtp.EnhancedCode{4, 7, 0}
	}
	return &gosmtp.SMTPError{Code: int(code), EnhancedCode: enh, Message: reason}
}

// sanitizeRejectReason makes a user-authored filter `Reject` reason safe to put
// on an SMTP `550` response line (smtp-server.md § Email filter rules): it
// collapses to a single line — every ASCII control byte (CR/LF/TAB included)
// becomes a space and runs of spaces collapse, so a reason can never smuggle
// SMTP protocol bytes (CRLF injection) — trims the ends, and rune-caps the
// length so one rule can't bloat the wire. A blank reason falls back to a
// generic message.
func sanitizeRejectReason(reason string) string {
	var b strings.Builder
	prevSpace := false
	for _, r := range reason {
		if r < 0x20 || r == 0x7f {
			r = ' '
		}
		if r == ' ' {
			if prevSpace {
				continue
			}
			prevSpace = true
		} else {
			prevSpace = false
		}
		b.WriteRune(r)
	}
	out := strings.TrimSpace(b.String())
	const maxRunes = 200
	if rs := []rune(out); len(rs) > maxRunes {
		out = strings.TrimSpace(string(rs[:maxRunes]))
	}
	if out == "" {
		return "Message rejected by recipient mail policy"
	}
	return out
}

// isSrsBounceLocalPart is the cheap, no-RPC pre-check that a RCPT local-part
// looks like an SRS-rewritten bounce address (`SRS0=…` / `SRS1=…`,
// case-insensitive per the codec). A match routes the RCPT through
// `decode_srs_bounce`; nest is authoritative on whether it actually verifies
// (mail-forwarding.md § Bounce decode).
func isSrsBounceLocalPart(localPart string) bool {
	return len(localPart) >= 5 &&
		(strings.EqualFold(localPart[:5], "SRS0=") || strings.EqualFold(localPart[:5], "SRS1="))
}

// tryRcptSrsBounce handles a RCPT whose local-part looked like an SRS bounce
// (isSrsBounceLocalPart). It asks nest to decode+verify the address and maps
// the outcome to the SMTP wire (mail-forwarding.md § Bounce decode):
//
//   - ok      → accept; the DATA stage delivers the bounce to the forwarder
//     (`forwarder_actor_id`), not the original sender (§ NDR routing).
//   - orphan  → accept; the DATA stage drops it (verified ours, but the
//     forwarding row is gone — no mailbox, never the admin's, :104,:117).
//   - mac_fail → 550 5.1.1 (forged/corrupt; hard-reject, no retry, :100).
//   - expired  → 550 5.4.4 (bounce older than the max age, :101).
//   - malformed → 550 5.1.1 (looks like SRS but structurally invalid).
//   - not_srs   → fall through to the normal recipient path (returns done=false).
//
// Returns (done, err): done=true means the RCPT is fully handled and the
// caller returns err (nil to accept, or the SMTP reply to reject); done=false
// means fall through to validate_recipient. A transport / decode failure
// tempfails (451) — a transient nest blip must not drop a legitimate bounce.
func (s *inboundSession) tryRcptSrsBounce(to, localPart string) (done bool, err error) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	decoded, dErr := wsrpc.DecodeSrsBounce(ctx, s.caller, localPart)
	cancel()
	if dErr != nil {
		s.logger.Warn("smtp: decode_srs_bounce transport error",
			"client_ip", s.clientIP, "local_part", localPart, "err", dErr)
		return true, &gosmtp.SMTPError{
			Code:         451,
			EnhancedCode: gosmtp.EnhancedCode{4, 7, 0},
			Message:      "Bounce recognition temporarily unavailable; try again later",
		}
	}
	metrics.SMTPInboundSrsDecode.WithLabelValues(string(decoded.Outcome)).Inc()
	switch decoded.Outcome {
	case wsrpc.SrsBounceOutcomeOk:
		s.inboundRcpts = append(s.inboundRcpts, resolvedInboundRcpt{
			actorID:   decoded.ForwarderActorID,
			srsBounce: true,
			rcptAddr:  to,
		})
		s.rcpts = append(s.rcpts, to)
		s.logger.Info("smtp: srs bounce accepted",
			"client_ip", s.clientIP, "forwarder", hex.EncodeToString(decoded.ForwarderActorID),
			"original_destination", decoded.OriginalDestination, "verdict", "srs_bounce_to_forwarder")
		return true, nil
	case wsrpc.SrsBounceOutcomeOrphan:
		// Accept the RCPT so the sending MX doesn't retry, but mark it for a
		// silent DATA-stage drop — the forwarding row is gone and the decoded
		// payload carries the original sender's PII (never the admin mailbox).
		s.inboundRcpts = append(s.inboundRcpts, resolvedInboundRcpt{srsBounceOrphan: true, rcptAddr: to})
		s.rcpts = append(s.rcpts, to)
		s.logger.Info("smtp: srs bounce orphan accepted (will drop at data)",
			"client_ip", s.clientIP, "local_part", localPart, "verdict", "srs_bounce_orphan")
		return true, nil
	case wsrpc.SrsBounceOutcomeMacFail:
		s.logger.Info("smtp: srs bounce mac fail",
			"client_ip", s.clientIP, "local_part", localPart)
		return true, s.reject(&gosmtp.SMTPError{
			Code:         550,
			EnhancedCode: gosmtp.EnhancedCode{5, 1, 1},
			Message:      "SRS verification failed",
		})
	case wsrpc.SrsBounceOutcomeExpired:
		s.logger.Info("smtp: srs bounce expired",
			"client_ip", s.clientIP, "local_part", localPart)
		return true, s.reject(&gosmtp.SMTPError{
			Code:         550,
			EnhancedCode: gosmtp.EnhancedCode{5, 4, 4},
			Message:      "SRS bounce expired",
		})
	case wsrpc.SrsBounceOutcomeMalformed:
		s.logger.Info("smtp: srs bounce malformed",
			"client_ip", s.clientIP, "local_part", localPart)
		return true, s.reject(&gosmtp.SMTPError{
			Code:         550,
			EnhancedCode: gosmtp.EnhancedCode{5, 1, 1},
			Message:      "Invalid SRS bounce address",
		})
	case wsrpc.SrsBounceOutcomeNotSrs:
		// nest disagrees with our cheap prefix check — treat as a normal RCPT.
		return false, nil
	default:
		// Unknown outcome from a newer nest — tempfail rather than guess.
		s.logger.Warn("smtp: decode_srs_bounce unknown outcome",
			"client_ip", s.clientIP, "local_part", localPart, "outcome", string(decoded.Outcome))
		return true, &gosmtp.SMTPError{
			Code:         451,
			EnhancedCode: gosmtp.EnhancedCode{4, 7, 0},
			Message:      "Bounce recognition temporarily unavailable; try again later",
		}
	}
}

// splitRcptAddress parses an RCPT TO address into (localPart, domain).
// Uses `net/mail.ParseAddress` for the syntactic check (RFC 5322 § 3.4),
// then splits on the last `@` — the spec allows quoted-string local
// parts which may legitimately contain a literal `@` (rare but valid).
//
// Returns an error when the address is unparseable; the caller maps
// that to 501 5.5.2 on the SMTP wire.
func splitRcptAddress(to string) (localPart, domain string, err error) {
	to = strings.TrimSpace(to)
	if to == "" {
		return "", "", fmt.Errorf("empty address")
	}
	addr, err := mail.ParseAddress(to)
	if err != nil {
		return "", "", err
	}
	// addr.Address is the cleaned `local@domain` form.
	at := strings.LastIndex(addr.Address, "@")
	if at < 1 || at == len(addr.Address)-1 {
		return "", "", fmt.Errorf("address %q lacks both a local part and a domain", addr.Address)
	}
	return addr.Address[:at], addr.Address[at+1:], nil
}

// headerAddressCatchall is the resolver's per-recipient stamp marking a
// catch-all match — the delivered address is NOT on the recipient user's
// exact-alias whitelist (never-registered or dropped). Mirrors
// fauna_mail::aliases::HEADER_CATCHALL (aliases/mod.rs); the two must stay in
// lockstep. The MTA reads it to apply the unlisted-recipient penalty
// (mail-spam.md § Unlisted-recipient penalty).
const headerAddressCatchall = "X-Fauna-Address-Catchall"

// rcptStampedCatchall reports whether a resolved recipient's stamped headers
// carry the catch-all marker, i.e. this recipient is unlisted (fell through to
// the catch-all rather than matching an exact/wildcard/disposable alias).
func rcptStampedCatchall(stamped []wsrpc.StampedHeader) bool {
	for _, sh := range stamped {
		if sh.Name == headerAddressCatchall {
			return true
		}
	}
	return false
}

// verifyInbound is the SPF/DKIM/DMARC/ARC verifier Data calls at C.5 — always
// mailfauna.VerifyInbound in production. It is a package variable so this
// package's tests can install a canned verifier once, in TestMain
// (main_test.go): the real one resolves SPF and DMARC over live DNS, which made
// every inbound Data test's verdict depend on the box reaching a resolver
// (convention 14, docs/goal/architecture/e2e-latency-independent-assertions.md)
// and, on Windows, raised a firewall prompt per run from the resolver's
// wildcard-bound socket. Nothing outside TestMain assigns it.
var verifyInbound = mailfauna.VerifyInbound

func (s *inboundSession) Data(r io.Reader) error {
	// This MTA's own accept instant for the SMTP transaction — read ONCE,
	// here, and used for every "when did we receive this" fact the delivery
	// produces: the Received: header we prepend and the forensic
	// report_rejected_scan row. It is deliberately NOT the message's `Date:`
	// header, which the sender writes and no RFC obliges them to fill in
	// truthfully (imap-server.md § SEARCH -> INTERNALDATE is the nest's own
	// receipt time). Taken at the top of DATA rather than per-use so the two
	// facts can never disagree about one delivery, and so a transport-level
	// retry of the report reuses the same value -- the nest derives the
	// forensic row's synthetic message_id from it, which is what makes that
	// insert idempotent.
	acceptedAtUnix := time.Now().Unix()
	// Phase C.4 pre-parser size guard: bound io.ReadAll with a
	// LimitReader so a 1 GiB header bomb can't tie up the goroutine
	// before our explicit cap check. When maxMessageBytes is 0 (tests
	// that don't exercise the cap) we read without a cap; production
	// always carries a non-zero value from the snapshot.
	var raw []byte
	var err error
	if s.maxMessageBytes > 0 {
		raw, err = io.ReadAll(io.LimitReader(r, int64(s.maxMessageBytes)+1))
	} else {
		raw, err = io.ReadAll(r)
	}
	if err != nil {
		return err
	}
	if s.maxMessageBytes > 0 && uint32(len(raw)) > s.maxMessageBytes {
		s.logger.Info("smtp: message exceeds size limit",
			"client_ip", s.clientIP,
			"from", s.from,
			"bytes", len(raw),
			"cap", s.maxMessageBytes,
		)
		return s.reject(&gosmtp.SMTPError{
			Code:         552,
			EnhancedCode: gosmtp.EnhancedCode{5, 3, 4},
			Message:      "Message exceeds fixed size limit",
		})
	}
	// RFC 8058 mailto one-click unsubscribe (mail-mass-mailing.md § The mailto
	// handler): a discard-only envelope — every accepted RCPT was an
	// `unsubscribe+<token>@` whose unsubscribe the nest already applied at RCPT
	// time, and there is no deliverable recipient — is accepted (250) and its
	// body dropped here, BEFORE parse/auth/scan/ingest. Short-circuiting ahead
	// of the auth-enforce + content-scan gates keeps a sender's SPF/DMARC
	// posture (or a scanner blip) from turning the spec-mandated fire-and-forget
	// 250 into a 5xx/451. A mixed envelope (a real recipient alongside the
	// unsubscribe RCPT) has inboundRcpts != 0 and flows the normal path.
	if s.hasDiscardRcpt && len(s.inboundRcpts) == 0 {
		s.logger.Info("smtp: discard-only envelope accepted (mailto unsubscribe)",
			"client_ip", s.clientIP, "from", s.from, "bytes", len(raw))
		return nil
	}
	// T2.2 header-section cap (parser-bomb defense, smtp-server.md
	// § Connection-time limits): reject a header bomb with 554 5.6.0
	// before the UniFFI RFC-5322 parser ever sees the bytes.
	if smtpErr := checkHeaderSection(raw); smtpErr != nil {
		s.logger.Info("smtp: header section rejected",
			"client_ip", s.clientIP,
			"from", s.from,
			"bytes", len(raw),
			"reason", smtpErr.Message,
		)
		return s.reject(smtpErr)
	}
	// RFC 5322 §3.6: exactly one From field, counted before any parser reads
	// one (smtp-server.md § Architectural rules → Exactly one From field).
	// Given two, VerifyInbound's DMARC aligns against the first while every app
	// and SenderDomainWithEnvelopeFallback read the last, so a DMARC-passing
	// message would render as any p=reject domain. Tarpitted like the caps.
	if smtpErr, n := checkFromFieldCount(raw); smtpErr != nil {
		s.logger.Info("smtp: from field count rejected",
			"client_ip", s.clientIP,
			"from", s.from,
			"from_fields", n,
		)
		return s.reject(smtpErr)
	}
	// EF-2: strip any sender-forged reserved
	// `X-Fauna-*` delivery-stamp header before we parse, verify, or seal.
	// The namespace is OURS — the bridge stamps the genuine `X-Fauna-Scan-*` /
	// `X-Fauna-Address-*` (+ canonical `Received:`) downstream — so any
	// `X-Fauna-*` already present is forged (a Fauna nest only stamps these at
	// delivery, never on the wire). Doing it here, before ParseRFC5322, cleans
	// BOTH the per-recipient filter context (built from parsed.Headers) AND the
	// sealed copy in one place, so a forged `X-Fauna-Address-Matched` can't
	// trigger a recipient's alias-metadata filter rule (smtp-server.md
	// § Architectural rules). `X-Fauna-Forwarded-By` is preserved — the
	// forward-loop floor reads it inbound (mail-forwarding.md § Loop detection).
	// Inbound sibling of the outbound StripReceivedHeaders. Runs after the
	// header-section cap so a header bomb is still measured on the sender's bytes.
	raw = mailfauna.StripFaunaHeaders(raw)
	// Phase C.4 parse via UniFFI. Errors short-circuit with 554 5.6.0
	// (unparseable inbound mail); successful parses are stashed on the
	// session for Phases C.5-C.9 to consume.
	parsed, err := mailfauna.ParseRFC5322(raw)
	if err != nil {
		s.logger.Info("smtp: parse_rfc5322 failed",
			"client_ip", s.clientIP,
			"from", s.from,
			"bytes", len(raw),
			"err", err,
		)
		return s.reject(&gosmtp.SMTPError{
			Code:         554,
			EnhancedCode: gosmtp.EnhancedCode{5, 6, 0},
			Message:      "Message could not be parsed",
		})
	}
	s.parsed = &parsed
	// Phase C.5 verify_inbound (SPF/DKIM/DMARC/ARC). The shared Rust
	// impl is async-tokio internally; the UniFFI Go binding makes it
	// look like a sync call. AuthError::Unparseable → 554 5.6.0 (same
	// disposition as the ParseRFC5322 path above, since the auth-parser
	// is a separate library and could reject something mail-parser
	// accepted). AuthError::ResolverInit → 451 4.7.0 (transient
	// system-level DNS init failure; legitimate retry should succeed).
	//
	// `s.conn` is set by `NewSession` in production; the nil-guard
	// keeps the helo empty for unit tests that drive `Data` directly
	// without spinning up a real go-smtp connection (verify_inbound
	// accepts an empty client_helo).
	var helo string
	if s.conn != nil {
		helo = strings.TrimSpace(s.conn.Hostname())
	}
	verdicts, err := verifyInbound(raw, s.from, s.clientIP, helo)
	if err != nil {
		if errors.Is(err, mailfauna.ErrAuthErrorUnparseable) {
			s.logger.Info("smtp: verify_inbound unparseable",
				"client_ip", s.clientIP,
				"from", s.from,
				"bytes", len(raw),
				"err", err,
			)
			return s.reject(&gosmtp.SMTPError{
				Code:         554,
				EnhancedCode: gosmtp.EnhancedCode{5, 6, 0},
				Message:      "Message could not be parsed",
			})
		}
		s.logger.Warn("smtp: verify_inbound transient error",
			"client_ip", s.clientIP,
			"from", s.from,
			"err", err,
		)
		return &gosmtp.SMTPError{
			Code:         451,
			EnhancedCode: gosmtp.EnhancedCode{4, 7, 0},
			Message:      "Authentication verification temporarily unavailable; try again later",
		}
	}
	s.verdicts = &verdicts
	// T1.5 inbound auth-enforcement stage (DMARC-reject → SPF-hardfail →
	// DKIM, behind !LogOnly). Acts on the verdicts C.5 just stashed,
	// emitting the spec'd 5xx per gate (DMARC 550 5.7.1, SPF 550 5.7.23,
	// DKIM 550 5.7.20). DMARC quarantine is not rejected here — it routes
	// to the C.7 spam gate's PolicyJunk disposition (Junk) below.
	if smtpErr := applyAuthEnforceGates(verdicts, s.authPolicy); smtpErr != nil {
		s.logger.Info("smtp: auth enforce reject",
			"client_ip", s.clientIP,
			"from", s.from,
			"bytes", len(raw),
			"enhanced_code", smtpErr.EnhancedCode,
		)
		return s.reject(smtpErr)
	}
	// T1.4 content-scan gate (ClamAV malware + rspamd content score).
	// Runs on the plaintext `raw` after auth-enforce, before
	// tokenize/seal (mail-content-scanning.md § Where the pipeline runs).
	// ClamAV gates delivery; rspamd produces the content score that drives
	// the T3.1 combined-score disposition below. Scanner unavailable ⇒ 451
	// (fail-closed, never allow-without-scan). A zero-value scanConfig
	// (Phase C unit tests) disables both scanners, so the gate is a no-op
	// Clean/Deliver and the combined score is 0 (→ INBOX).
	// s.maxMessageBytes is the identical value the 552 5.3.4 door above enforced
	// for THIS message: the per-session copy taken at connection open, so a
	// mid-DATA `config_changed` reload cannot skew the door and the scan cap
	// apart (mail-content-scanning.md § Oversize messages).
	clamavVerdict, rspamdScore, scanAction := applyScanGate(
		s.scanGate, context.Background(), raw, s.scanConfig, s.maxMessageBytes,
		s.clientIP, s.from, s.rcpts,
	)
	switch a := scanAction.(type) {
	case mailfauna.ScanActionTempfail:
		s.logger.Warn("smtp: content-scan tempfail (scanner unavailable)",
			"client_ip", s.clientIP, "from", s.from, "reason", a.Reason)
		return &gosmtp.SMTPError{
			Code:         451,
			EnhancedCode: gosmtp.EnhancedCode{4, 7, 0},
			Message:      "Spam scanner unavailable, retry later",
		}
	case mailfauna.ScanActionRejectMalware:
		s.logger.Info("smtp: malware reject",
			"client_ip", s.clientIP, "from", s.from,
			"bytes", len(raw), "signature", a.Signature)
		// Forensic audit row ("we rejected this"); the 554 stands regardless
		// of whether the report lands. Skeleton-path tests have no nest peer
		// (caller nil) — skip the report there. rspamd is not run on a reject,
		// so the forensic row carries no rspamd score.
		if s.caller != nil {
			senderDomain := mailfauna.SenderDomainWithEnvelopeFallback(raw, s.from)
			reportCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			_, rerr := wsrpc.ReportRejectedScan(
				reportCtx, s.caller, a.Signature, nil, acceptedAtUnix, senderDomain,
			)
			cancel()
			if rerr != nil {
				s.logger.Warn("smtp: report_rejected_scan failed (continuing with 554)",
					"client_ip", s.clientIP, "err", rerr)
			}
		}
		return s.reject(&gosmtp.SMTPError{
			Code:         554,
			EnhancedCode: gosmtp.EnhancedCode{5, 7, 1},
			Message:      "Message contains malware: " + a.Signature,
		})
	}

	// T3.1 combined-score disposition. rspamd is the sole deployment-wide
	// content scorer; combined = max(rspamd_scaled, weighted_bayesian),
	// floored at 0, in milli-units of the 0–15 scale (dag-cbor forbids
	// floats). The per-user Bayesian model is a deferred track → cold-start
	// contributes 0, so combined == rspamd today. rspamd disabled/absent ⇒
	// rspamdScore nil ⇒ score 0 ⇒ Accept (no content scoring; mirrors the
	// admin's ClamAV-off opt-out). The permissive default (spam_folder=5,
	// reject=0; `0 = disabled`) auto-files above 5 to Junk and never
	// 550-rejects unless an admin sets a non-zero reject tier
	// (mail-spam.md § Combined-score formula + § Routing).
	var rspamdScaledMilli int32
	if rspamdScore != nil {
		rspamdScaledMilli = rspamdScore.ScaledMilli
	}
	combinedMilli := mailfauna.CombinedSpamScoreMilli(rspamdScaledMilli, 0)
	disposition := mailfauna.DecideSpamDisposition(combinedMilli, verdicts, s.spamPolicy)
	if _, isJunk := scanAction.(mailfauna.ScanActionJunk); isJunk {
		// ClamAV infected + junk action: the admin chose to file malware
		// to the recipient's Junk rather than reject it; that choice wins
		// over a content-score reject so we never 554 a message the admin
		// opted to keep. The nest derives action_taken=junked from
		// (Infected, PolicyJunk).
		disposition = mailfauna.SpamDispositionPolicyJunk
	} else if disposition == mailfauna.SpamDispositionReject {
		// Admin opted into a content-score reject (reject_threshold != 0).
		s.logger.Info("smtp: spam reject",
			"client_ip", s.clientIP,
			"from", s.from,
			"bytes", len(raw),
			"combined_score_milli", combinedMilli,
		)
		return s.reject(&gosmtp.SMTPError{
			Code:         554,
			EnhancedCode: gosmtp.EnhancedCode{5, 7, 1},
			Message:      "Message rejected by spam policy",
		})
	}
	// Floor the combined milli-score to 0–15 points for the ingest wire
	// `spam_score` (what filter rules match on; T3.3).
	spamScorePoints := uint32(combinedMilli / 1000)

	// Deliver / Junk / Tag: stamp our headers on `raw` before seal so
	// they ride into the recipient's mailbox. (Tokenize below reads
	// parsed.Subject/BodyText, not `raw`, so it is unaffected.)
	//
	// The canonical Fauna `Received:` trace header goes topmost — above the
	// X-Fauna-Scan-* headers — so every sealed copy carries exactly one Fauna
	// receipt trace (smtp-server.md § Architectural rules; the inbound sibling
	// of the outbound StripReceivedHeaders). Sender-controlled HELO/IP are
	// sanitized inside the shared Rust builder (CR/LF/non-printable → `unknown`),
	// defeating header-injection forgery. The `for <addr>` clause is emitted
	// only for a single-recipient envelope: the one header is sealed to every
	// recipient, so emitting `for` on a multi-RCPT delivery would leak
	// cross-recipient correlation. `helo` was resolved for verify_inbound above.
	tlsVersion, tlsCipher := tlsParamsForReceived(s.conn)
	var serverHostname string
	if len(s.localDomains) > 0 {
		serverHostname = s.localDomains[0]
	}
	var forRecipient string
	if len(s.rcpts) == 1 {
		forRecipient = s.rcpts[0]
	}
	receivedHeader := mailfauna.BuildReceivedHeader(acceptedAtUnix, mailfauna.ReceivedHeaderOpts{
		ServerHostname: serverHostname,
		HeloDomain:     helo,
		ClientIp:       s.clientIP,
		TlsVersion:     tlsVersion,
		TlsCipher:      tlsCipher,
		QueueId:        mailfauna.NewQueueID(),
		Recipient:      forRecipient,
	})
	raw = prependHeaders(
		raw,
		append(
			[]string{receivedHeader},
			scanHeaders(clamavVerdict, rspamdScore, s.scanConfig.Policy.ClamavEnabled)...,
		),
	)
	// The authenticated-sender stamp (smtp-server.md § Architectural rules →
	// The `X-Fauna-*` namespace): the `From:` addr-spec, only under a DMARC
	// pass. Deliberately NOT prepended to `raw` here with the trace + scan
	// lines above: `raw` is also what every forward leg ships to an external
	// destination (dispatchForward), and this is internal delivery metadata —
	// it rides only the per-recipient sealed copy, beside the resolver's
	// X-Fauna-Address-* stamps (the rcptRaw prepend below).
	authSenderStamp := mxAuthenticatedSenderStamp(s.verdicts, parsed.From)

	// Phase C.8 tokenize. The deterministic Unicode tokenizer runs over
	// Subject + " " + BodyText — the encrypted-index-hint input per
	// docs/goal/architecture/encryption-at-rest.md § Index shape (body +
	// subject is the body axis the MDA matches BODY/TEXT searches
	// against; participants ride server-side via search_messages).
	// CanonicalBytes is what C.9 encrypts to the recipient's index key.
	hint := mailfauna.Tokenize(parsed.Subject + " " + parsed.BodyText)
	s.indexHint = &hint
	// Canonical report-hash, once per message over the same parsed fields the
	// tokenizer consumes — identical for every recipient of the same message
	// on every nest (report-sharing.md § Content identity). Computed pre-seal
	// at DATA; only the hash crosses to the nest.
	reportHash := mailfauna.ReportHash(parsed.Subject, parsed.BodyText)
	// Canonical dedup keys, likewise once per message at this same pre-seal
	// position, over the raw RFC 5322 bytes (mailbox-migration.md § Key format).
	// Computed from `raw` — before the per-recipient `X-Fauna-*` stamps are
	// prepended — so every recipient records the identical pair, and so it
	// matches what a later import computes from the source server's unstamped
	// copy of the same message. The envelope key is what stops this sender's
	// chosen Message-ID from pre-empting a later import of a different message
	// (§ The envelope key confirms a Message-ID hit).
	dedupKeys := mailfauna.MailDedupKeys(raw)
	// Phase C.9 encrypt-to-recipient + ingest_inbound_mail. One ingest
	// call per resolved RCPT; each recipient gets their own HPKE-Seal
	// envelope under their own MLS pubkey (body) + index pubkey (hint).
	// Skipped when caller is nil (unit tests that drive Data without a
	// nest peer) or when no RCPTs resolved (defensive — Rcpt rejects
	// before Data on a fully-RCPT-rejected envelope).
	if s.caller == nil || len(s.inboundRcpts) == 0 {
		s.logger.Info("smtp message accepted (no ingest — skeleton path)",
			"client_ip", s.clientIP,
			"from", s.from,
			"rcpt_count", len(s.rcpts),
			"bytes", len(raw),
			"mime_parts", len(parsed.MimeParts),
			"spam_score", spamScorePoints,
			"spam_disposition", disposition,
			"index_hint_tokens", len(hint.Tokens),
		)
		return nil
	}
	wireDisposition, isReject := mailfauna.SpamDispositionToWire(disposition)
	if isReject {
		// Defensive: C.7's gate already 5xx'd at SMTP DATA on Reject; if
		// the bridge somehow reaches the ingest path with a Reject
		// disposition, log loudly and 5xx rather than smuggling a
		// rejected message through. Unreachable for any C.7 disposition
		// except a future variant the converter hasn't been updated for.
		s.logger.Error("smtp: C.9 reached ingest path with reject disposition",
			"client_ip", s.clientIP, "from", s.from, "disposition", disposition)
		return s.reject(&gosmtp.SMTPError{
			Code:         554,
			EnhancedCode: gosmtp.EnhancedCode{5, 7, 1},
			Message:      "Message rejected by spam policy",
		})
	}
	wireVerdicts := mailfauna.AuthVerdictsToWire(*s.verdicts)
	senderDomain := mailfauna.SenderDomainWithEnvelopeFallback(raw, s.from)
	// T1.4 scan verdict → wire (rides every per-recipient ingest call). rspamd
	// score is nil when rspamd is disabled or didn't run.
	clamavWire := mailfauna.ClamavVerdictToWire(clamavVerdict)
	var rspamdWire *wsrpc.RspamdScore
	if rspamdScore != nil {
		w := mailfauna.RspamdScoreToWire(*rspamdScore)
		rspamdWire = &w
	}
	// T3.3 — the per-recipient filter-rule evaluation context. Message-level
	// (envelope sender, Subject, headers, combined spam score in milli-units,
	// decoded body), identical for every recipient; only each recipient's stored
	// rules differ. Built once here on the plaintext the MTA holds pre-seal (the
	// storage-mode invariant — nest never evaluates filters, smtp-server.md
	// § Email filter rules). The body axis (`BodyContains`, S4b) reuses the
	// already-decoded text parts (no re-MIME-walk) and is size-capped by
	// FilterBodyFromParts.
	filterHeaders := make([]mailfauna.FilterHeader, 0, len(parsed.Headers))
	for _, h := range parsed.Headers {
		filterHeaders = append(filterHeaders, mailfauna.FilterHeader{Name: h.Name, Value: h.Value})
	}
	filterCtx := mailfauna.FilterContext{
		From:           s.from,
		Subject:        parsed.Subject,
		Headers:        filterHeaders,
		SpamScoreMilli: combinedMilli,
		Body:           mailfauna.FilterBodyFromParts(parsed.BodyText, parsed.BodyHTML),
	}
	// Preflight backstop (security-review): re-confirm every
	// deliverable recipient's seal keys BEFORE the first ingestForRecipient
	// write. The common case — a recipient with no key at all — is already
	// caught per-recipient at RCPT TO, above; this narrows the rare residual
	// race (a key revoked between RCPT and DATA) from the sender-controlled
	// RCPT→DATA gap (arbitrarily long — the sender picks when to send DATA)
	// down to this one sub-second preflight-to-ingest-loop gap. It does not
	// reach zero: a key revoked inside THAT narrower window is re-caught by
	// ingestForRecipient's own resolve below, which still tempfails/rejects
	// the whole transaction — but only after any earlier recipients in this
	// same DATA were already ingested, so a sender retry re-delivers to them
	// (; per-recipient ingest idempotency —
	// smtp-server.md:330's named follow-up — is what would close it).
	if keyErr := s.preflightRecipientSealKeys(); keyErr != nil {
		s.logger.Warn("smtp: preflight recipient seal-key check failed",
			"client_ip", s.clientIP, "from", s.from, "smtp_code", keyErr.Code,
			"verdict", "preflight_no_key")
		return keyErr
	}
	// Admin external forwarders (mail-aliases.md § Kind 7) dispatch FIRST, before
	// any local recipient commits. A forwarder keeps no local copy, so a forward
	// nest did not durably enqueue — refused at the queue ceiling because parking
	// it would evict the only copy of other accepted mail, or nest unreachable
	// (mail-forwarding.md § Queue ceiling) — must tempfail rather than 250 a
	// message that then exists nowhere. DATA carries one reply for the whole
	// envelope, so deciding that 451 before any local ingest keeps the sender's
	// retry from re-delivering to this envelope's local recipients. (A second
	// forwarder in the same envelope whose predecessor already enqueued is
	// re-forwarded on the retry — a duplicate at the external destination,
	// never a loss.)
	for _, rcpt := range s.inboundRcpts {
		if !rcpt.forward || rcpt.srsBounceOrphan {
			continue
		}
		if tempErr := s.dispatchForwarderRedirect(rcpt, raw); tempErr != nil {
			return tempErr
		}
	}
	for i, rcpt := range s.inboundRcpts {
		// mail-forwarding N4 — a verified-ours SRS bounce whose forwarding row
		// is gone (orphan). It was accepted at RCPT so the sending MX doesn't
		// retry, but there is no mailbox to land it in and its payload carries
		// the original sender's PII — drop it silently (never the admin's,
		// :104,:117). Countered at decode time (smtp_inbound_srs_decode_total).
		if rcpt.srsBounceOrphan {
			s.logger.Info("smtp: srs bounce orphan dropped at data",
				"client_ip", s.clientIP, "rcpt_index", i, "verdict", "srs_bounce_orphan_dropped")
			continue
		}
		// Admin external forwarder (mail-aliases.md § Kind 7) — already
		// redirected, with NO local copy, in the forwarders-first pass above. No
		// filter rules / ingest / auto-reply run (there is no local mailbox whose
		// rules apply).
		if rcpt.forward {
			continue
		}
		// control_overrides (rcpt.controlOverrides): per-alias spam-threshold /
		// rate-cap overrides are plumbed from the resolver but NOT yet enforced.
		// ⚠ Corrected 2026-08-17 — the alias-detail control UI IS built and is a
		// real producer of non-None spam_threshold_override / rate_limit_per_hour,
		// so a user-set cap is silently ignored rather than merely unreachable;
		// the disposable rate_limit_per_day still has no inbound enforcement gate.
		// TODO(mail-forwarding):
		// when the producer lands, re-derive this recipient's spam disposition with
		// SpamThresholdOverride and gate inbound on the per-alias rate cap here.
		// T3.3 — evaluate this recipient's filter rules and compose the matched
		// actions into a placement decision. Sieve `continue` means several rules
		// can fire in order; the composition precedence (smtp-server.md § Email
		// filter rules): FileInto/Allow set the target mailbox (last wins),
		// AddLabel accumulates keyword flags, and Discard is terminal (drops the
		// recipient — later actions in the chain are not applied).
		// `Reject` is terminal like `Discard` (smtp-server.md § Email filter
		// rules): it records a reason and stops composing — applied after the loop
		// as a single-recipient 550 or a silent multi-recipient/null-sender drop.
		// `AutoReply` is non-placement + non-terminal: it records a pending reply
		// that fires AFTER this recipient's delivery commits (O5 — a `Discard`
		// earlier in the chain suppresses it via the drop short-circuit).
		// `Forward` is non-placement + non-terminal too: each fired Forward is
		// stashed and dispatched after delivery commits (mail-forwarding.md
		// § Per-rule "forward to"; a Discard/Reject in the chain suppresses it).
		// A fetch/decode error never fails the accepted SMTP txn — it falls
		// through to spam-disposition placement (logged inside the helper).
		var filterTargetMailbox *string
		// allowPlacement: the placement that won (last FileInto/Allow) is an
		// Allow, whose spam-disposition override must outlive delivery — the
		// sealed copy carries a 0 threshold stamp (recipientSealedCopy).
		allowPlacement := false
		var filterExtraFlags []string
		dropRecipient := false
		var rejectReason *string
		var pendingAutoReply *autoReplyPlan
		var pendingForwards []filterForwardPlan
		actor := hex.EncodeToString(rcpt.actorID)
		// Per-recipient filter context: the message-level context plus this
		// recipient's resolver-stamped X-Fauna-Address-* headers, so a filter rule
		// can match on the matched-alias metadata (subaddress / wildcard /
		// disposable / catch-all — aliases/mod.rs:55). Exact / role-address stamp
		// none, so the base context is reused unchanged (no per-recipient alloc).
		rcptFilterCtx := filterCtx
		if len(rcpt.headersToStamp) > 0 {
			hdrs := make([]mailfauna.FilterHeader, len(filterHeaders), len(filterHeaders)+len(rcpt.headersToStamp))
			copy(hdrs, filterHeaders)
			for _, sh := range rcpt.headersToStamp {
				hdrs = append(hdrs, mailfauna.FilterHeader{Name: sh.Name, Value: sh.Value})
			}
			rcptFilterCtx.Headers = hdrs
		}
		for _, match := range s.maybeFilterPlacement(rcpt.actorID, rcptFilterCtx) {
			switch a := match.Action.(type) {
			case mailfauna.FilterActionFileInto:
				mb := a.Mailbox
				filterTargetMailbox = &mb
				allowPlacement = false
				s.logger.Info("smtp: filter FileInto",
					"client_ip", s.clientIP, "actor", actor, "filter_id", match.FilterId,
					"mailbox", a.Mailbox, "verdict", "filter_file_into")
			case mailfauna.FilterActionAllow:
				inbox := "INBOX"
				filterTargetMailbox = &inbox
				allowPlacement = true
				s.logger.Info("smtp: filter Allow (→ INBOX, spam disposition overridden)",
					"client_ip", s.clientIP, "actor", actor, "filter_id", match.FilterId,
					"verdict", "filter_allow")
			case mailfauna.FilterActionAddLabel:
				filterExtraFlags = append(filterExtraFlags, a.Label)
				s.logger.Info("smtp: filter AddLabel",
					"client_ip", s.clientIP, "actor", actor, "filter_id", match.FilterId,
					"label", a.Label, "verdict", "filter_add_label")
			case mailfauna.FilterActionDiscard:
				s.logger.Info("smtp: filter Discard (recipient delivery dropped)",
					"client_ip", s.clientIP, "actor", actor, "filter_id", match.FilterId,
					"verdict", "filter_discard")
				dropRecipient = true
			case mailfauna.FilterActionReject:
				// Terminal (O3). Record the sanitized reason; the application
				// decision (550 vs. silent drop) is made after the loop, where
				// the transaction's recipient count + sender are known.
				reason := sanitizeRejectReason(a.Reason)
				rejectReason = &reason
				s.logger.Info("smtp: filter Reject matched (terminal)",
					"client_ip", s.clientIP, "actor", actor, "filter_id", match.FilterId,
					"verdict", "filter_reject")
			case mailfauna.FilterActionAutoReply:
				// Non-terminal + non-placement: stash the reply and keep composing.
				// It fires after a successful local delivery (O5); the last
				// AutoReply in a continue chain wins.
				plan := autoReplyPlan{subject: a.Subject, body: a.Body, intervalHours: a.IntervalHours}
				pendingAutoReply = &plan
				s.logger.Info("smtp: filter AutoReply matched (deferred to post-delivery)",
					"client_ip", s.clientIP, "actor", actor, "filter_id", match.FilterId,
					"verdict", "filter_auto_reply")
			case mailfauna.FilterActionForward:
				// Non-terminal, like AutoReply: stash the forward and keep
				// composing. Every fired Forward in a continue chain forwards
				// once (mail-forwarding.md § Per-rule "forward to"), through
				// the shared dispatchForward. In `copy` mode it is also
				// non-placement (fires after the local delivery commits); a
				// `redirect` Forward is the one filter action that suppresses
				// the recipient's local placement — applied after the loop,
				// once the chain is known not to end in Discard/Reject.
				pendingForwards = append(pendingForwards, filterForwardPlan{
					ruleID:      strconv.FormatInt(match.FilterId, 10),
					destination: a.Address,
					redirect:    a.Redirect,
				})
				s.logger.Info("smtp: filter Forward matched (deferred to post-delivery)",
					"client_ip", s.clientIP, "actor", actor, "filter_id", match.FilterId,
					"redirect", a.Redirect, "verdict", "filter_forward")
			}
			if dropRecipient || rejectReason != nil {
				break // Discard / Reject are terminal — stop composing this recipient
			}
		}
		if rejectReason != nil {
			// O2/O3: a fired Reject. For a single-recipient, non-null-sender
			// transaction we can cleanly refuse the whole message at end-of-DATA
			// (550), so the sender's own MX bounces it — no backscatter from us.
			// For a multi-recipient transaction we have already committed 250 for
			// the others (the same partial-failure principle submission states at
			// smtp-server.md § Recipient handling on submission's "Partial-failure
			// guarantee" bullet), and for a null-sender message a bounce would be
			// a bounce-of-a-bounce; in both cases we drop this recipient silently
			// and count, never synthesizing a DSN.
			if s.from != "" && len(s.inboundRcpts) == 1 {
				metrics.SMTPInboundFilterReject.WithLabelValues("refuse_5xx").Inc()
				s.logger.Info("smtp: filter Reject → 550 (single-recipient refuse)",
					"client_ip", s.clientIP, "actor", actor, "verdict", "filter_reject_refuse")
				return s.reject(&gosmtp.SMTPError{
					Code:         550,
					EnhancedCode: gosmtp.EnhancedCode{5, 7, 1},
					Message:      *rejectReason,
				})
			}
			mode := "drop_multi"
			if s.from == "" {
				mode = "drop_null_sender"
			}
			metrics.SMTPInboundFilterReject.WithLabelValues(mode).Inc()
			s.logger.Info("smtp: filter Reject → silent drop (no DSN)",
				"client_ip", s.clientIP, "actor", actor, "mode", mode,
				"verdict", "filter_reject_drop")
			continue
		}
		if dropRecipient {
			continue
		}
		// `redirect` copy mode (mail-forwarding.md § Per-rule "forward to"): a
		// per-recipient placement decision — any fired redirect Forward means
		// this recipient keeps NO local copy, and every forward dispatched for
		// the message (the chain's copy rules and forward-all included) is
		// truthfully copy_mode=redirect. With no local commit to order after,
		// the forwards' durable enqueue in nest's outbound queue IS the
		// delivery: they dispatch first, and only if none could be enqueued
		// (null sender, a loop floor, the size guard, a nest error) does the
		// message fall back to local delivery below — suppression costs the
		// redirect, never the mail (§ Loop suppression vs. delivery). An SRS
		// bounce is never forwarded, so it takes the local path directly.
		if redirectRecipient(pendingForwards) && !rcpt.srsBounce {
			enqueued := s.dispatchFilterForwards(rcpt.actorID, raw, nil, pendingForwards,
				wsrpc.ForwardCopyModeRedirect)
			if enqueued > 0 {
				s.logger.Info("smtp: filter Forward redirect (forwarded, no local copy)",
					"client_ip", s.clientIP, "actor", actor, "forwards", enqueued,
					"verdict", "filter_redirect")
				s.maybeForwardForRecipient(rcpt.actorID, raw, nil, wsrpc.ForwardCopyModeRedirect)
				// The forward enqueue is this recipient's delivery commit, so
				// the vacation reply fires exactly as after a local one (O5).
				if pendingAutoReply != nil {
					s.maybeAutoReply(rcpt.actorID, rcpt.rcptAddr, filterHeaders, pendingAutoReply)
				}
				continue
			}
			s.logger.Info("smtp: filter Forward redirect fell back to local delivery (no forward enqueued)",
				"client_ip", s.clientIP, "actor", actor, "verdict", "filter_redirect_fallback_local")
			// Every plan was just attempted; the post-delivery stage below
			// must not try them a second time.
			pendingForwards = nil
		}
		// Stamp this recipient's X-Fauna-Address-* headers onto their sealed copy
		// (the MDA / client reads which alias matched; the filter context above
		// already sees them). prependHeaders returns `raw` unchanged when there
		// are none, so exact / role-address matches pay nothing. The forward-all
		// copy below intentionally keeps the *unstamped* `raw` — internal alias
		// routing metadata must not leak to an external forward destination.
		// Recipient-whitelist unlisted-recipient penalty (mail-spam.md
		// § Unlisted-recipient penalty). A catch-all stamp means this address is
		// NOT on the user's exact-alias whitelist (never-registered or dropped →
		// fell through to the catch-all), so add the deployment-wide penalty to
		// the combined score and re-derive THIS recipient's disposition. Listed
		// recipients (no catch-all stamp) — and every recipient when the penalty
		// is 0 (the opt-in-off default) — keep the message-global values. The
		// message-global reject tier already 554'd at DATA; a per-recipient
		// penalty has no hard-reject path here (550-ing one RCPT of a multi-RCPT
		// txn would be backscatter), so a penalty reaching the reject tier is
		// filed to this recipient's Junk instead (PolicyJunk) — the tightest
		// deliverable disposition. With the permissive default (reject=0) the
		// penalty only ever reaches the spam-folder tier → Junk as well.
		rcptScorePoints := spamScorePoints
		rcptScoreMilli := combinedMilli
		rcptDisposition := wireDisposition
		if s.unlistedRecipientPenalty > 0 && rcptStampedCatchall(rcpt.headersToStamp) {
			penalizedMilli := mailfauna.ApplyUnlistedRecipientPenaltyMilli(combinedMilli, s.unlistedRecipientPenalty)
			disp := mailfauna.DecideSpamDisposition(penalizedMilli, verdicts, s.spamPolicy)
			if disp == mailfauna.SpamDispositionReject {
				disp = mailfauna.SpamDispositionPolicyJunk
			}
			dispWire, _ := mailfauna.SpamDispositionToWire(disp)
			rcptScorePoints = uint32(penalizedMilli / 1000)
			rcptScoreMilli = penalizedMilli
			rcptDisposition = dispWire
			s.logger.Info("smtp: unlisted-recipient penalty applied",
				"client_ip", s.clientIP, "actor", actor,
				"penalty_points", s.unlistedRecipientPenalty,
				"combined_milli", combinedMilli, "penalized_milli", penalizedMilli,
				"verdict", "unlisted_recipient_penalty")
		}
		rcptRaw := recipientSealedCopy(raw, authSenderStamp, rcpt.headersToStamp, allowPlacement)
		// This recipient's scoring-metadata bus rows — the perimeter's own,
		// minted by the ONE shared-Rust mapping from the verdicts the per-kind
		// fields carry (content-scoring.md § The scoring-metadata bus, the
		// contract phase); the nest stores them as-is and derives nothing.
		// Per recipient because the spam score is (the penalty above).
		rcptScores := mailfauna.PerimeterMailScoreRows(rcptScoreMilli, clamavVerdict, rspamdScore, verdicts)
		ingestCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		messageID, err := s.ingestForRecipient(
			ingestCtx, rcpt.actorID, rcpt.isRoleAddress, rcptRaw, hint, reportHash, dedupKeys, wireVerdicts, rcptScorePoints,
			rcptDisposition, senderDomain, parsed.DateUnixSeconds,
			clamavWire, rspamdWire, rcptScores, filterTargetMailbox, filterExtraFlags,
		)
		cancel()
		if err != nil {
			s.logger.Warn("smtp: ingest_inbound_mail failed",
				"client_ip", s.clientIP,
				"from", s.from,
				"rcpt_index", i,
				"err", err,
			)
			// The nest handler distinguishes malformed (5xx-class) from
			// internal (4xx-class) via the RpcError code; surfacing
			// that distinction back to SMTP is a Phase E concern.
			// Over-quota is the recipient's mailbox being full → 552 5.2.2
			// permanent (imap-server.md § Quota enforcement points); the
			// sender bounces. Role-address recipients never reach here
			// (nest skips the pre-check for them). Missing-MLS-pubkey is
			// permanent UNLESS the recipient is a succession's successor
			// still inside the bounded re-provisioning window, which
			// tempfails instead (smtp-server.md § Error / tempfail
			// strategy); everything else is transient.
			if code, ok := wsrpc.RpcErrorCode(err); ok && code == wsrpc.CodeOverQuota {
				return s.reject(mailboxFullError())
			}
			var noKeyErr *recipientNoKeyError
			if errors.As(err, &noKeyErr) {
				if noKeyErr.SuccessionPending {
					return s.reject(&gosmtp.SMTPError{
						Code:         451,
						EnhancedCode: gosmtp.EnhancedCode{4, 7, 1},
						Message:      "Recipient's mailbox is being restored; try again later",
					})
				}
				return s.reject(&gosmtp.SMTPError{
					Code:         550,
					EnhancedCode: gosmtp.EnhancedCode{5, 1, 1},
					Message:      "Recipient has no encryption key on file",
				})
			}
			// A size failure is a property of the message, not of nest's
			// health: it can never succeed on retry, so an honest immediate
			// bounce beats days of doomed retries (smtp-server.md § Error /
			// tempfail strategy; § Message size limits). Both causes — a
			// sealed body too large to *rest*, and the pathological
			// hint-alone-over-frame case — are permanent.
			if errors.Is(err, mailstage.ErrMessageTooLarge) {
				return s.reject(&gosmtp.SMTPError{
					Code:         552,
					EnhancedCode: gosmtp.EnhancedCode{5, 3, 4},
					Message:      "Message exceeds fixed size limit",
				})
			}
			return &gosmtp.SMTPError{
				Code:         451,
				EnhancedCode: gosmtp.EnhancedCode{4, 7, 0},
				Message:      "Mail ingestion temporarily unavailable; try again later",
			}
		}
		s.logger.Info("smtp message ingested",
			"client_ip", s.clientIP,
			"from", s.from,
			"rcpt_index", i,
			"bytes", len(raw),
			"mime_parts", len(parsed.MimeParts),
			"spam_score", spamScorePoints,
			"spam_disposition", disposition,
			"index_hint_tokens", len(hint.Tokens),
			"message_id", hex.EncodeToString(messageID),
		)
		// An emailed invitation lands on the recipient's calendar too — only
		// when its mail went to the inbox (never from Junk), and
		// strictly after that copy committed, so it can never cost the mail.
		if rcptDisposition == "accept" {
			inviteCtx, cancelInvite := context.WithTimeout(context.Background(), 10*time.Second)
			placeInboundInvite(inviteCtx, s.caller, s.logger, rcpt.actorID, raw, s.from, parsed.DateUnixSeconds)
			cancelInvite()
		}
		// mail-forwarding N2 — per-account forward-all, fired strictly AFTER
		// the local mailbox write committed (:245,:269). A forward failure
		// never costs the local copy and never fails the SMTP transaction
		// (the inbound is already accepted), so this returns no error.
		//
		// N4: an inbound SRS bounce we just delivered to the forwarder is
		// itself a bounce — never re-forward it (:255). A real bounce is
		// null-sender, which maybeForwardForRecipient already skips, but the
		// explicit guard also covers a non-null-sender message addressed to a
		// (still MAC-verified) SRS recipient.
		if !rcpt.srsBounce {
			s.maybeForwardForRecipient(rcpt.actorID, raw, messageID, wsrpc.ForwardCopyModeCopy)
			s.dispatchFilterForwards(rcpt.actorID, raw, messageID, pendingForwards,
				wsrpc.ForwardCopyModeCopy)
		}
		// AutoReply (Sieve vacation) — fired strictly AFTER the local delivery
		// committed (O5: a `Discard`/`Reject` earlier in the chain `continue`d
		// before reaching here, so no delivery ⇒ no reply). Never for an
		// SRS-bounce delivery (the "message" is itself a bounce, and an
		// auto-reply must never bounce a bounce). Like forwarding, a reply
		// failure never costs the local copy or fails the accepted SMTP txn.
		if pendingAutoReply != nil && !rcpt.srsBounce {
			s.maybeAutoReply(rcpt.actorID, rcpt.rcptAddr, filterHeaders, pendingAutoReply)
		}
	}
	return nil
}

// maybeForwardForRecipient runs the per-account "forward all" stage for one
// just-delivered recipient (mail-forwarding.md § Trigger point). It is the
// MTA-perimeter forward decision: the nest holds only the recipient-sealed
// ciphertext, so only the bridge — which still has the plaintext `raw` in
// scope here — can produce the copy a downstream MX needs.
//
// It reads the recipient's forward-all target from
// fauna.bridges.fetch_recipient_forward_config and, when set, hands the live
// forward to the shared dispatchForward. `copyMode` is the recipient's — Copy
// after a local delivery (forward-all itself never drops the copy, :32), or
// Redirect when a fired redirect rule already suppressed this recipient's
// local copy and forward-all rides along truthfully labelled. A null-sender
// pre-check skips the fetch RPC (forwarding a bounce is backscatter, :255).
// Every skip drops only the forward.
func (s *inboundSession) maybeForwardForRecipient(actorID, raw, localMessageID []byte, copyMode wsrpc.ForwardCopyMode) {
	// Null-sender forwards are never attempted (:255) — cheap pre-check before
	// any RPC (dispatchForward re-checks as a backstop).
	if s.from == "" {
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	target, err := wsrpc.FetchRecipientForwardConfig(ctx, s.caller, actorID)
	if err != nil {
		s.logger.Warn("smtp: fetch_recipient_forward_config failed (forward skipped)",
			"client_ip", s.clientIP, "actor", hex.EncodeToString(actorID), "err", err)
		return
	}
	if target == "" {
		return // forward-all disabled for this recipient
	}
	dispatchForward(ctx, s.caller, s.logger, s.outboundTrigger, s.clientIP, s.from,
		s.parsed.Headers, raw, actorID, target, s.forwardOrigMsgID(localMessageID), "forward-all", copyMode)
}

// forwardOrigMsgID is the `original_msgid` a forward of this message carries:
// the source RFC 5322 Message-ID when present; else the nest-assigned
// per-recipient id of the local copy; else — a redirect, which has no local
// copy — a fresh queue id (as the admin forwarder does). Always non-empty, so
// nest's non-empty-msgid check passes and N4's NDR dedup keys on a stable id.
func (s *inboundSession) forwardOrigMsgID(localMessageID []byte) string {
	if id := strings.TrimSpace(s.parsed.MessageID); id != "" {
		return id
	}
	if len(localMessageID) > 0 {
		return hex.EncodeToString(localMessageID)
	}
	return mailfauna.NewQueueID()
}

// filterForwardPlan is a fired per-rule `Forward` action stashed during filter
// composition and dispatched once the recipient's delivery decision is durable
// — after the local commit in copy mode, in place of it in redirect mode.
type filterForwardPlan struct {
	ruleID      string
	destination string
	// redirect is the rule's copy mode: true ⇒ forward with no local copy.
	redirect bool
}

// redirectRecipient reports whether any fired Forward asks for `redirect` —
// the per-recipient placement decision (mail-forwarding.md § Per-rule "forward
// to"): one redirect rule in the chain suppresses the local copy for the whole
// recipient.
func redirectRecipient(plans []filterForwardPlan) bool {
	for _, p := range plans {
		if p.redirect {
			return true
		}
	}
	return false
}

// dispatchFilterForwards runs the per-rule "forward to" stage for one
// recipient (mail-forwarding.md § Per-rule "forward to"): each fired Forward
// rule becomes one forward through the shared dispatchForward, attributed to
// the rule's owner (the recipient) and stamped `rule=<filter-id>` in
// X-Fauna-Forwarded-By (§ Loop detection). `copyMode` is the recipient's
// (Copy after a local delivery, Redirect in place of one — every forward of a
// redirect recipient carries Redirect, whatever its own rule said, because no
// local copy exists). Returns how many forwards were durably enqueued; the
// redirect caller falls back to local delivery on zero. Like forward-all it
// keeps the unstamped `raw` (internal alias metadata never leaks outward),
// skips a null sender, and never fails the accepted SMTP transaction.
func (s *inboundSession) dispatchFilterForwards(actorID, raw, localMessageID []byte, plans []filterForwardPlan, copyMode wsrpc.ForwardCopyMode) int {
	if len(plans) == 0 {
		return 0
	}
	if s.from == "" {
		s.logger.Info("smtp: filter Forward skipped for null sender (delivered locally)",
			"client_ip", s.clientIP, "actor", hex.EncodeToString(actorID),
			"verdict", "forward_skipped_null_sender")
		return 0
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	origMsgID := s.forwardOrigMsgID(localMessageID)
	enqueued := 0
	for _, p := range plans {
		if dispatchForward(ctx, s.caller, s.logger, s.outboundTrigger, s.clientIP, s.from,
			s.parsed.Headers, raw, actorID, p.destination, origMsgID, p.ruleID, copyMode) == forwardEnqueued {
			enqueued++
		}
	}
	return enqueued
}

// dispatchForwarderRedirect runs the admin external-forwarder dispatch for one
// resolved Forward recipient (mail-aliases.md § Kind 7). Unlike forward-all it
// keeps NO local copy (copy_mode=redirect, :59) and reads the destination +
// attributed actor straight off the resolver outcome rather than
// fetch_recipient_forward_config. There is no local ingest (no message id), so
// the source RFC 5322 Message-ID is used, falling back to a fresh queue id so
// nest's non-empty-msgid check passes. A null-sender (bounce) to a forward-only
// address has nowhere to go — no forward (:255) and no local mailbox — so it is
// accepted at RCPT and dropped here. Returns the transaction's 451 when nest did
// not take the forward (forwardFailed — the one outcome that would otherwise
// lose accepted mail); a floor's deliberate suppression returns nil.
func (s *inboundSession) dispatchForwarderRedirect(rcpt resolvedInboundRcpt, raw []byte) *gosmtp.SMTPError {
	if s.from == "" {
		s.logger.Info("smtp: forwarder null-sender dropped (no forward, no local copy)",
			"client_ip", s.clientIP, "destination", rcpt.forwardTarget,
			"verdict", "forwarder_null_sender_drop")
		return nil
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	origMsgID := strings.TrimSpace(s.parsed.MessageID)
	if origMsgID == "" {
		origMsgID = mailfauna.NewQueueID()
	}
	if dispatchForward(ctx, s.caller, s.logger, s.outboundTrigger, s.clientIP, s.from,
		s.parsed.Headers, raw, rcpt.forwarderActorID, rcpt.forwardTarget, origMsgID,
		"forwarder", wsrpc.ForwardCopyModeRedirect) == forwardFailed {
		// Infra-class tempfail: never routed through reject's tarpit.
		return forwarderTempfailError()
	}
	return nil
}

// forwarderTempfailError is the 451 an admin external forwarder answers when
// nest did not durably enqueue its forward (mail-forwarding.md § Queue
// ceiling): with no local copy, a 250 would accept mail that then exists
// nowhere, so the sending MTA keeps it and retries.
func forwarderTempfailError() *gosmtp.SMTPError {
	return &gosmtp.SMTPError{
		Code:         451,
		EnhancedCode: gosmtp.EnhancedCode{4, 3, 0},
		Message:      "Forwarding temporarily unavailable; try again later",
	}
}

// forwardOutcome is what dispatchForward did with one forward. Only
// forwardEnqueued means nest holds a durable copy; the other two differ in
// whether leaving the message un-forwarded was a decision.
type forwardOutcome int

const (
	// forwardSuppressed: a floor deliberately skipped the forward (null
	// sender, a loop floor, the size guard) — retrying cannot change that.
	forwardSuppressed forwardOutcome = iota
	// forwardEnqueued: nest durably enqueued the forward row.
	forwardEnqueued
	// forwardFailed: forward_message errored — nest refused the enqueue (a
	// `redirect` over the queue ceiling, mail-forwarding.md § Queue ceiling)
	// or was unreachable. Nothing was enqueued, and a retry may succeed.
	forwardFailed
)

// dispatchForward is the shared forward emit used by every forward path: the
// inbound per-account forward-all stage (maybeForwardForRecipient, copy_mode=Copy),
// the inbound per-rule Forward stage (dispatchFilterForwards, copy_mode=Copy),
// the inbound admin external-forwarder redirect (inboundSession.dispatchForwarderRedirect,
// copy_mode=Redirect), AND the submission admin external-forwarder redirect
// (submissionSession.dispatchForwarderRedirect — a local user emailing a forwarder
// address). `forwarderActor` is the attributed principal (SRS forwarder-actor,
// rate-cap + NDR owner) — the account holder for forward-all, the managing admin
// for an external forwarder. `headers` are the inbound/submitted message's headers
// as they arrived (the loop floors inspect them, predating our own prepends).
//
// It applies the two per-forward loop floors (Received: chain > 10 hops, or our
// own X-Fauna-Forwarded-By already present — :134,:146; each suppresses only the
// forward), stamps a fresh X-Fauna-Forwarded-By onto a private copy (the caller's
// `raw` is untouched), and enqueues one outbound_mail_queue forward row via
// fauna.bridges.forward_message. The SRS envelope rewrite is applied nest-side at
// queue-out (N3). A suppressed/failed forward is logged and never fails the
// accepted SMTP transaction. A nil-`from` (null sender) is dropped (forwarding a
// bounce is backscatter, :255) — both inbound callers pre-check, submission's
// Mail() rejects MAIL FROM:<> so it never reaches here.
//
// Returns forwardEnqueued only when the forward row was durably enqueued in
// nest — the signal the per-rule `redirect` stage keys its local-delivery
// fallback on (mail-forwarding.md § Per-rule "forward to"); a floor's
// suppression returns forwardSuppressed and a forward_message error
// forwardFailed, which the admin forwarder (no local copy to fall back to)
// answers with a 451.
func dispatchForward(
	ctx context.Context,
	caller wsrpc.Caller,
	logger *slog.Logger,
	outboundTrigger func(),
	clientIP, from string,
	headers []mailfauna.ParsedHeader,
	raw, forwarderActor []byte,
	target, origMsgID, ruleID string,
	copyMode wsrpc.ForwardCopyMode,
) forwardOutcome {
	if logger == nil {
		logger = slog.New(slog.NewTextHandler(io.Discard, nil))
	}
	// Backstop null-sender guard (callers pre-check, but a forward of a bounce is
	// backscatter — never emit one, :255).
	if from == "" {
		return forwardSuppressed
	}
	ourActor := hex.EncodeToString(forwarderActor)

	// Stamp X-Fauna-Forwarded-By on the copy to forward — prependHeaders returns
	// a fresh buffer, so the caller's `raw` is untouched. Built BEFORE the size
	// guard because `stamped` (not `raw`) is what rides `forward_message` inline,
	// so the guard must measure the bytes actually shipped. The `rule=` token is
	// "forward-all" / "forwarder" / a rule id.
	stampValue := mailfauna.ForwardStampValue(ourActor, time.Now().Unix(), ruleID)
	stamped := prependHeaders(raw, []string{mailfauna.HeaderForwardedBy + ": " + stampValue})

	// Interim size guard (smtp-server.md § Message size limits, the
	// staged-envelope rule): the forward rides `forward_message` with the full
	// body inline, so over the inline budget the RPC can only be refused at
	// the 2 MiB frame — suppress the doomed attempt with an explicit verdict
	// instead of a generic enqueue failure. The forward is the only casualty;
	// the local copy is already delivered. (The forward leg is a separate leg —
	// its inline-budget backstop stays; only the size measured is corrected to
	// the stamped bytes.)
	if mailfauna.MailBodyNeedsReference(uint64(len(stamped)), 0) {
		logger.Info("smtp: forward suppressed",
			"client_ip", clientIP, "actor", ourActor, "rule", ruleID,
			"stamped_bytes", len(stamped), "verdict", "forward_suppressed_too_large")
		return forwardSuppressed
	}

	// Loop-detection floors (R2 (account-data-plane.md § The ratified decisions)), read off the message's headers as it arrived
	// (predating our scan-header / forwarded-by prepends, which is exactly the
	// chain loop detection inspects).
	var receivedCount uint64
	var forwardedBy []string
	for _, h := range headers {
		switch {
		case strings.EqualFold(h.Name, "Received"):
			receivedCount++
		case strings.EqualFold(h.Name, mailfauna.HeaderForwardedBy):
			forwardedBy = append(forwardedBy, h.Value)
		}
	}
	if mailfauna.ForwardReceivedChainExceeded(receivedCount) {
		logger.Info("smtp: forward suppressed",
			"client_ip", clientIP, "actor", ourActor, "rule", ruleID,
			"received_hops", receivedCount, "verdict", "forward_suppressed_received_chain")
		return forwardSuppressed
	}
	if mailfauna.ForwardSelfAlreadyForwarded(forwardedBy, ourActor) {
		logger.Info("smtp: forward suppressed",
			"client_ip", clientIP, "actor", ourActor, "rule", ruleID,
			"verdict", "forward_suppressed_self_seen")
		return forwardSuppressed
	}

	rowID, err := wsrpc.ForwardMessage(
		ctx, caller, forwarderActor, origMsgID, from, target, stamped,
		ruleID, copyMode,
	)
	if err != nil {
		logger.Warn("smtp: forward_message enqueue failed (forward dropped)",
			"client_ip", clientIP, "actor", ourActor, "destination", target,
			"rule", ruleID, "err", err)
		return forwardFailed
	}
	logger.Info("smtp: forward enqueued",
		"client_ip", clientIP, "actor", ourActor, "destination", target,
		"rule", ruleID, "copy_mode", string(copyMode), "outbound_row_id", rowID, "verdict", "forwarded")
	// Nudge the outbound worker so the forward delivers on the next poll instead
	// of waiting the full PollInterval (mirrors the submission Data hook —
	// mail-forwarding N4 latency polish). nil on a bridge with no outbound worker
	// / in unit tests.
	if outboundTrigger != nil {
		outboundTrigger()
	}
	return forwardEnqueued
}

// stampHeaderLines renders the resolver's per-recipient X-Fauna-Address-*
// headers (mail-aliases.md § Resolution order) into "Name: Value" lines for
// prependHeaders. Nil/empty → nil, so prependHeaders returns the buffer
// unchanged (exact / role-address matches stamp nothing).
func stampHeaderLines(hs []wsrpc.StampedHeader) []string {
	if len(hs) == 0 {
		return nil
	}
	lines := make([]string, len(hs))
	for i, h := range hs {
		lines[i] = h.Name + ": " + h.Value
	}
	return lines
}

// sealedCopyStampLines is the header block a door prepends to ONE recipient's
// sealed copy: the door's authenticated-sender stamp line (omitted when "" —
// the door authenticated no sender) followed by that recipient's resolver
// stamps. Shared by the MX door (Data) and the submission door
// (dispatchFaunaRecipients) so both compose the copy the same way.
func sealedCopyStampLines(authSenderStamp string, hs []wsrpc.StampedHeader) []string {
	lines := stampHeaderLines(hs)
	if authSenderStamp == "" {
		return lines
	}
	return append([]string{authSenderStamp}, lines...)
}

// recipientSealedCopy is the bytes sealed for one inbound recipient: the
// message with that recipient's delivery stamps prepended
// (sealedCopyStampLines) and, when a fired `Allow` filter rule won placement,
// the spam-threshold stamp replaced by the disabled tier — so the Allow holds
// through every post-delivery scoring pass, not only the delivery-time
// disposition (email-filters.md § Multi-action composition). Nest folds the
// threshold at RCPT, but Allow is decided here after DATA, so the MTA owns the
// override; mailfauna.StampFilterAllow leaves it as the FIRST (and only)
// threshold line, which is the one every reader takes.
func recipientSealedCopy(raw []byte, authSenderStamp string, hs []wsrpc.StampedHeader, allowPlacement bool) []byte {
	stamped := prependHeaders(raw, sealedCopyStampLines(authSenderStamp, hs))
	if !allowPlacement {
		return stamped
	}
	return mailfauna.StampFilterAllow(stamped)
}

// mxAuthenticatedSenderStamp is the MX door's `X-Fauna-Authenticated-Sender`
// line: the `From:` addr-spec, lower-cased, ONLY when the message's DMARC
// verdict is pass — DMARC aligns the RFC5322.From domain to the domain SPF or
// DKIM authenticated, so the domain owner vouches for the mailbox. DMARC
// none / fail / temperror / permerror (and an ARC-only forward, which DMARC
// does not pass) stamp nothing (smtp-server.md § Architectural rules → The
// `X-Fauna-*` namespace). `from` is the parsed `From:` value, which may carry a
// display name: only a single parseable mailbox is stamped; anything else
// yields "" (the refusing direction — an unstamped copy reads as
// unauthenticated downstream).
func mxAuthenticatedSenderStamp(v *mailfauna.AuthVerdicts, from string) string {
	if v == nil {
		return ""
	}
	if _, pass := v.Dmarc.(faunaCore.DmarcVerdictPass); !pass {
		return ""
	}
	addr, err := mail.ParseAddress(from)
	if err != nil {
		return ""
	}
	return mailfauna.BuildAuthenticatedSenderStamp(addr.Address)
}

// autoReplyPlan is a matched `AutoReply` action stashed during filter
// composition and fired after the recipient's local delivery commits (O5).
type autoReplyPlan struct {
	subject       string
	body          string
	intervalHours uint32
}

// maybeAutoReply runs the per-recipient vacation auto-reply stage AFTER a
// successful local delivery (smtp-server.md § Email filter rules — O4/O5/O6). It
// (1) applies the perimeter loop guard (`fauna_mail::filter::auto_reply_decision`),
// (2) composes the RFC 5322 reply in shared Rust, and (3) hands it to nest's
// `send_auto_reply`, which atomically rate-limits + enqueues it with a null
// envelope-from. The reply leaves here unsigned: the nest signs it at the
// outbound hand-out, like every other queued message (O6). Every skip leaves
// the local copy delivered and never fails the accepted SMTP txn (mirrors
// maybeForwardForRecipient).
func (s *inboundSession) maybeAutoReply(actorID []byte, rcptAddr string, headers []mailfauna.FilterHeader, plan *autoReplyPlan) {
	actor := hex.EncodeToString(actorID)
	// Defense-in-depth: this stage runs POST-delivery and, per the contract above,
	// must never fail the already-accepted SMTP txn. An unexpected panic here —
	// e.g. one crossing the UniFFI boundary from shared Rust on a hostile header —
	// would otherwise unwind into go-smtp's connection-level recover, turn the
	// delivered message into a 421, and make the sender RETRY → duplicate delivery
	// (and a per-retry panic-stack log flood). Swallow it: the local copy is
	// already delivered; we lose only this one auto-reply. (The known trigger,
	// fauna_mail::filter::auto_reply_decision's `List-*` slice, is fixed at the
	// source; this guards the whole post-delivery FFI path against future ones.)
	defer func() {
		if r := recover(); r != nil {
			metrics.SMTPInboundAutoReplySuppressed.WithLabelValues("panic").Inc()
			s.logger.Error("smtp: auto-reply stage panicked (local copy delivered; reply skipped)",
				"client_ip", s.clientIP, "actor", actor, "panic", fmt.Sprintf("%v", r),
				"verdict", "auto_reply_panic")
		}
	}()
	// 1. Loop guard — pure, perimeter-evaluated on the plaintext-floor headers.
	gate := mailfauna.AutoReplyDecision(s.from, headers, rcptAddr, s.localDomains)
	if gate != mailfauna.AutoReplyGateSend {
		reason := autoReplySuppressLabel(gate)
		metrics.SMTPInboundAutoReplySuppressed.WithLabelValues(reason).Inc()
		s.logger.Info("smtp: auto-reply suppressed (loop guard)",
			"client_ip", s.clientIP, "actor", actor, "reason", reason,
			"verdict", "auto_reply_suppressed")
		return
	}
	// 2. Compose the reply (shared Rust). From = the delivery recipient; To = the
	// envelope sender; In-Reply-To = the triggering Message-ID (if any).
	msgID := newMessageID(autoReplyDomain(rcptAddr))
	raw := mailfauna.ComposeAutoReply(mailfauna.AutoReplyMessage{
		FromAddr:  rcptAddr,
		ToAddr:    s.from,
		Subject:   plan.subject,
		Body:      plan.body,
		InReplyTo: filterHeaderValue(headers, "Message-ID"),
		MessageId: msgID,
		Date:      time.Now().UTC().Format(time.RFC1123Z),
	})
	// 3. Rate-limit claim + null-sender enqueue (nest, atomic).
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	sent, err := wsrpc.SendAutoReply(ctx, s.caller, actorID, s.from, plan.intervalHours, msgID, raw)
	cancel()
	if err != nil {
		s.logger.Warn("smtp: send_auto_reply failed (local copy delivered)",
			"client_ip", s.clientIP, "actor", actor, "err", err)
		return
	}
	if !sent {
		metrics.SMTPInboundAutoReplySuppressed.WithLabelValues("rate").Inc()
		s.logger.Info("smtp: auto-reply suppressed (rate limit)",
			"client_ip", s.clientIP, "actor", actor, "verdict", "auto_reply_rate_limited")
		return
	}
	metrics.SMTPInboundAutoReplySent.Inc()
	s.logger.Info("smtp: auto-reply sent",
		"client_ip", s.clientIP, "actor", actor, "to", s.from, "verdict", "auto_reply_sent")
	// Nudge the outbound worker so the reply delivers on the next poll instead of
	// waiting the full PollInterval (mirrors the forward stage).
	if s.outboundTrigger != nil {
		s.outboundTrigger()
	}
}

// autoReplySuppressLabel maps a non-Send AutoReplyGate to its metric label
// (smtp_inbound_autoreply_suppressed_total{reason}).
func autoReplySuppressLabel(gate mailfauna.AutoReplyGate) string {
	switch gate {
	case mailfauna.AutoReplyGateSuppressNullSender:
		return "null_sender"
	case mailfauna.AutoReplyGateSuppressAutoSubmitted:
		return "auto_submitted"
	case mailfauna.AutoReplyGateSuppressBulk:
		return "list"
	case mailfauna.AutoReplyGateSuppressOwnDomain:
		return "self"
	case mailfauna.AutoReplyGateSuppressNotInRecipients:
		return "not_in_recipients"
	default:
		return "unknown"
	}
}

// filterHeaderValue returns the first header value whose name case-insensitively
// equals `name`, or "" if absent.
func filterHeaderValue(headers []mailfauna.FilterHeader, name string) string {
	for _, h := range headers {
		if strings.EqualFold(h.Name, name) {
			return h.Value
		}
	}
	return ""
}

// autoReplyDomain extracts the domain of an `user@domain` address for the
// auto-reply Message-ID; falls back to "localhost" if the address has no '@'.
func autoReplyDomain(addr string) string {
	if at := strings.LastIndexByte(addr, '@'); at >= 0 && at+1 < len(addr) {
		return addr[at+1:]
	}
	return "localhost"
}

// newMessageID builds a fresh RFC 5322 Message-ID `<random@domain>`. Uniqueness
// (not secrecy) is the goal; the id is only queue/correlation metadata.
func newMessageID(domain string) string {
	var b [16]byte
	if _, err := crand.Read(b[:]); err != nil {
		// crypto/rand essentially never fails; fall back to a time-based id.
		return fmt.Sprintf("<%d@%s>", time.Now().UnixNano(), domain)
	}
	return fmt.Sprintf("<%s@%s>", hex.EncodeToString(b[:]), domain)
}

// maybeFilterPlacement evaluates one recipient's stored filter rules (T3.3,
// smtp-server.md § Email filter rules) against the just-received message and
// returns the **ordered** matched verdicts (Sieve `continue` — a non-`continue`
// match is terminal; empty when no rule matches / the fetch or decode fails).
// This is the MTA-perimeter filter decision on plaintext: the rules are stored
// in nest but evaluated here, because in encrypted mode nest never sees the
// subject/headers (the storage-mode invariant, mail-forwarding.md § Storage-mode
// interaction). A fetch or decode failure never fails the already-accepted SMTP
// transaction — it logs and returns an empty slice, so the caller falls through
// to spam-disposition placement.
func (s *inboundSession) maybeFilterPlacement(actorID []byte, ctx mailfauna.FilterContext) []mailfauna.FilterMatch {
	fctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	wireFilters, err := wsrpc.FetchRecipientFilters(fctx, s.caller, actorID)
	if err != nil {
		s.logger.Warn("smtp: fetch_recipient_filters failed (filters skipped, disposition placement)",
			"client_ip", s.clientIP, "actor", hex.EncodeToString(actorID), "err", err)
		return nil
	}
	if len(wireFilters) == 0 {
		return nil // common case: recipient has no filter rules
	}
	stored, err := mailfauna.StoredFiltersFromWire(wireFilters)
	if err != nil {
		s.logger.Warn("smtp: filter rule decode failed (filters skipped, disposition placement)",
			"client_ip", s.clientIP, "actor", hex.EncodeToString(actorID), "err", err)
		return nil
	}
	return mailfauna.Evaluate(stored, ctx)
}

// preflightRecipientSealKeys re-resolves every deliverable recipient's seal
// keys before Data's ingest loop writes any of them (security-review turn
// 418 § 4). It skips forward and srsBounceOrphan recipients — neither calls
// ingestForRecipient — and role addresses are included (they are exempt
// from the RCPT-time check in Rcpt, so this preflight remains their sole
// key check, unchanged from before this fix). Returns the first failing
// recipient's SMTP error (451 succession-pending / transport, or 550
// permanent no-key), or nil when every recipient still has a key. A
// zero-length s.inboundRcpts (or a nil s.caller — which implies an empty
// s.inboundRcpts, since Rcpt only appends when s.caller != nil) is a no-op.
func (s *inboundSession) preflightRecipientSealKeys() *gosmtp.SMTPError {
	for _, rcpt := range s.inboundRcpts {
		if rcpt.forward || rcpt.srsBounceOrphan {
			continue
		}
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		keys, err := wsrpc.ResolveRecipientSealKeys(ctx, s.caller, rcpt.actorID)
		cancel()
		if err != nil {
			return &gosmtp.SMTPError{
				Code:         451,
				EnhancedCode: gosmtp.EnhancedCode{4, 7, 0},
				Message:      "Mail ingestion temporarily unavailable; try again later",
			}
		}
		if keys.MLSPubkey == nil {
			if keys.SuccessionPending {
				return &gosmtp.SMTPError{
					Code:         451,
					EnhancedCode: gosmtp.EnhancedCode{4, 7, 1},
					Message:      "Recipient's mailbox is being restored; try again later",
				}
			}
			return &gosmtp.SMTPError{
				Code:         550,
				EnhancedCode: gosmtp.EnhancedCode{5, 1, 1},
				Message:      "Recipient has no encryption key on file",
			}
		}
	}
	return nil
}

// ingestForRecipient encrypts the message body + index hint to a single
// recipient and submits the encrypted envelope to nest. Returns the
// 32-byte server-assigned message id (or an error mapped to an SMTP
// reply code by the caller).
//
// Index pubkey fallback: when `fetch_recipient_index_key` returns nil
// (Phase E will add the provisioning RPC; until then it's always nil),
// the index hint is sealed to the *MLS* pubkey instead. This reuses one
// secret for two purposes — but only at the bridge's seal call: the
// HPKE info/AAD binding is the same `for_mail_record()` for both, so
// the recipient's MDA always opens with the same shape. Phase E will
// switch to the real index pubkey without changing the wire format.
// Flagged here so a future reader can find the seam.
func (s *inboundSession) ingestForRecipient(
	ctx context.Context,
	actorID []byte,
	isRoleAddress bool,
	raw []byte,
	hint mailfauna.CanonicalTokenSet,
	reportHash []byte,
	dedupKeys mailfauna.MailDedupKeyPair,
	wireVerdicts wsrpc.AuthVerdicts,
	spamScore uint32,
	wireDisposition string,
	senderDomain string,
	timestampSec int64,
	clamavVerdict wsrpc.ClamavVerdict,
	rspamdScore *wsrpc.RspamdScore,
	scores []wsrpc.ScoreEntry,
	targetMailbox *string,
	extraFlags []string,
) ([]byte, error) {
	// Phase-3 D2: the recipient's seal keys resolve through the ONE
	// epoch-indexable resolver (index-key Phase-E fallback applied inside).
	keys, err := wsrpc.ResolveRecipientSealKeys(ctx, s.caller, actorID)
	if err != nil {
		return nil, fmt.Errorf("resolve recipient seal keys: %w", err)
	}
	if keys.MLSPubkey == nil {
		return nil, &recipientNoKeyError{SuccessionPending: keys.SuccessionPending}
	}
	// Phase-3 D1: sealed at rest in BOTH storage modes — the design-(b)
	// plaintext-mode no-seal branch is deleted; one at-rest byte shape, the
	// nest core never holds a content key. The body seals post-quantum
	// X-Wing to the recipient's two key halves (the resolver refuses a key
	// missing either). PQ-6
	// seals the hint hybrid too (no longer leaking the plaintext body
	// word-set under HNDL); IndexHintMlkemEk passes the ek only while the
	// index key is the MLS-pubkey fallback.
	encryptedBody, err := mailfauna.EncryptToRecipientHybrid(raw, keys.MLSPubkey, keys.MlkemEk)
	if err != nil {
		return nil, fmt.Errorf("encrypt body: %w", err)
	}
	encryptedIndexHint, err := mailfauna.EncryptToRecipientHybrid(
		hint.CanonicalBytes, keys.IndexPubkey,
		mailfauna.IndexHintMlkemEk(keys.IndexPubkey, keys.MLSPubkey, keys.MlkemEk))
	if err != nil {
		return nil, fmt.Errorf("encrypt index hint: %w", err)
	}
	// The sealed size is the RFC822.SIZE floor the recipient sees, so capture it
	// before the body is (possibly) moved off the request and onto the byte plane.
	sealedBodyLen := uint32(len(encryptedBody))
	// Post-seal, pre-RPC: a body the 2 MiB frame cannot carry is staged on the
	// bulk-byte plane and the request carries only its chunk hashes; a body that
	// fits rides inline exactly as before. There is no upper size ceiling here
	// since ceiling retirement (2026-07-18) — a sealed body of any size stages;
	// the only permanent failure this can return is an over-budget sealed index
	// hint, which always rides inline and cannot be staged
	// (mailstage.ErrOverInlineBudget; smtp-server.md § Message size limits).
	inlineBody, bodyRef, err := mailstage.StageSealedBody(
		ctx, s.caller, s.bytePlane, actorID, encryptedBody, encryptedIndexHint)
	if err != nil {
		return nil, err
	}
	publicMetadata := wsrpc.PublicMailMetadata{
		Timestamp:      timestampSec,
		CiphertextSize: sealedBodyLen,
		SenderDomain:   senderDomain,
	}
	// A null-reverse-path delivery (`MAIL FROM:<>`) carries no sender for the
	// guardian mail gate to key on; extract the RFC 3464 correlation so the nest
	// can tell a genuine bounce of the ward's own mail from a stranger claiming
	// `<>` (family-safety.md § The mail gate). The nest authorizes on the
	// reported original Message-ID — unguessable — not on the address, which is
	// public and so forgeable by anyone.
	var dsn mailfauna.DsnFacts
	if s.from == "" {
		dsn = mailfauna.DsnCorrelation(raw)
	}
	params := wsrpc.IngestInboundMailParams{
		ActorID:            actorID,
		EncryptedBody:      inlineBody,
		BodyRef:            bodyRef,
		EncryptedIndexHint: encryptedIndexHint,
		PublicMetadata:     publicMetadata,
		Verdicts:           wireVerdicts,
		// The envelope MAIL FROM. The nest recomputes the guardian mail-gate
		// verdict from it (never from a flag we set) and may place the message
		// in the recipient's held mailbox.
		SenderAddress:      s.from,
		DsnOriginalMsgID:   dsn.OriginalMsgID,
		DsnReportAddresses: dsn.ReplyAddresses,
		SpamScore:          spamScore,
		SpamDisposition:    wireDisposition,
		ClamavVerdict:      clamavVerdict,
		RspamdScore:        rspamdScore,
		Scores:             scores,
		IsRoleAddress:      isRoleAddress,
		TargetMailbox:      targetMailbox,
		ExtraFlags:         extraFlags,
		ReportHash:         reportHash,
		DedupKey:           dedupKeys.DedupKey,
		EnvelopeKey:        dedupKeys.EnvelopeKey,
	}
	return wsrpc.IngestInboundMail(ctx, s.caller, params)
}

// mailboxFullError is the 552 5.2.2 permanent reply for an inbound delivery
// the recipient's nest rejected as over-quota (wsrpc.CodeOverQuota →
// imap-server.md § Quota enforcement points). The sender bounces. Shared by
// the inbound (server.go) and submission (fauna_recipient.go) delivery
// paths. Role-address recipients never reach here — nest skips the
// per-mailbox quota pre-check for them (smtp-server.md :204).
func mailboxFullError() *gosmtp.SMTPError {
	return &gosmtp.SMTPError{
		Code:         552,
		EnhancedCode: gosmtp.EnhancedCode{5, 2, 2},
		Message:      "Mailbox full",
	}
}

func (s *inboundSession) Reset() { s.from = ""; s.rcpts = nil; s.hasDiscardRcpt = false }

// Logout deregisters the session from the shutdown drain tracker exactly
// once (go-smtp may call Logout from both the normal QUIT path and the
// connection-teardown path).
func (s *inboundSession) Logout() error {
	if s.drain != nil {
		s.loggedOut.Do(s.drain.leave)
	}
	return nil
}
