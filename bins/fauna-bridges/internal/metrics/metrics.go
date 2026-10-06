// Package metrics owns the bridge's Prometheus counter / histogram
// definitions and the /metrics HTTP handler.
//
// All metrics are registered against a package-level *prometheus.Registry
// at init() time (eager, fail-loud — a duplicate registration panics here
// so it cannot mask a counter aliasing bug). Call sites — wsrpc client,
// SMTP listeners, UniFFI/cgo call boundary — increment the
// exported vars directly; prometheus/client_golang's CounterVec /
// HistogramVec are already concurrency-safe, so no extra locking is
// needed in the call sites.
//
// The /metrics endpoint itself is mounted on 127.0.0.1:9090 by main.go
// in Phase B.8; here we only expose the http.Handler.
package metrics

import (
	"net/http"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promhttp"
)

// registry is the package-level Prometheus registry. We deliberately do
// NOT use prometheus.DefaultRegisterer — that registry collects Go
// runtime + process collectors by default, which inflates the /metrics
// payload without giving admins much actionable signal. The
// fauna-mail-bridge surface is the six metrics declared below; if we
// want runtime metrics we'll add the collectors here explicitly.
var registry = prometheus.NewRegistry()

// Registry returns the package-level Prometheus registry. Tests pass it
// to promhttp.HandlerFor; production passes it to Handler() below.
func Registry() *prometheus.Registry { return registry }

// Handler returns an http.Handler that serves the /metrics endpoint from
// the package registry. main.go binds it on 127.0.0.1:9090 in B.8.
//
// We deliberately do NOT pass HandlerOpts.Registry — that option asks
// promhttp to register its own internal handler-error counters into the
// supplied registry, which inflates the exposition with handler meta-
// metrics that aren't part of the bridge's surface.
func Handler() http.Handler {
	return promhttp.HandlerFor(registry, promhttp.HandlerOpts{})
}

// rttBuckets are the histogram boundaries for WS-RPC call latency.
// 1ms..~4s in 12 exponentially-spaced bins covers the expected envelope:
// nest is loopback / same-network in every deployment, so normal-path
// RTT lives in the low single-digit milliseconds; outliers (TLS handshake
// on a fresh dial, nest under load) tail into the seconds. SMTP-stage
// latency at the listener boundary is recorded separately in
// SMTPInboundMessagesTotal's verdict label — not as a histogram on the inbound
// path, because the SMTP RFC budget is per-stage and mostly bounded by
// the peer, not by us.
var rttBuckets = prometheus.ExponentialBuckets(0.001, 2, 12)

// WSRPCCallsTotal counts WS-RPC method invocations from the bridge to
// nest. The "result" label is one of "ok", "error", "timeout" — the
// caller decides which (timeout is distinct from error because admins
// want a separate alert threshold on it).
var WSRPCCallsTotal = prometheus.NewCounterVec(
	prometheus.CounterOpts{
		Name: "wsrpc_calls_total",
		Help: "Count of WS-RPC method invocations from the bridge to nest, by method and result.",
	},
	[]string{"method", "result"},
)

// WSRPCCallSeconds records the per-method round-trip time for WS-RPC
// calls. The bucket boundaries are tuned for low-ms loopback latency
// (see rttBuckets above).
var WSRPCCallSeconds = prometheus.NewHistogramVec(
	prometheus.HistogramOpts{
		Name:    "wsrpc_call_seconds",
		Help:    "Round-trip duration of WS-RPC calls from the bridge to nest, in seconds, by method.",
		Buckets: rttBuckets,
	},
	[]string{"method"},
)

// SMTPConnectionsTotal counts SMTP listener accepts, by listener port
// (25 / 465 / 587) and outcome ("accepted", "capped", and on the submission
// ports "per_ip_shed" when a connection is shed by the per-IP concurrent cap).
var SMTPConnectionsTotal = prometheus.NewCounterVec(
	prometheus.CounterOpts{
		Name: "smtp_connections_total",
		Help: "Count of SMTP connections handled by the bridge, by listener port and outcome.",
	},
	[]string{"port", "result"},
)

// MDAConnectionsTotal counts MDA listener accepts, by listener
// ("imap-993" / "imap-143" / "dav-443" — the last is the shared CalDAV+CardDAV
// 443 listener, matching the slog `listener` label) and outcome ("accepted" /
// "capped" / "per_ip_shed" — the last when a connection is shed by the per-IP
// concurrent cap). The IMAP/DAV analog of
// SMTPConnectionsTotal: both surface the per-listener global-connection-cap
// (internal/connlimit) saturation, plus the per-IP cap's sheds, so a connection
// flood is visible without a disk-filling log.
var MDAConnectionsTotal = prometheus.NewCounterVec(
	prometheus.CounterOpts{
		Name: "mda_connections_total",
		Help: "Count of MDA (IMAP/CalDAV) connections handled by the bridge, by listener and outcome.",
	},
	[]string{"listener", "result"},
)

// SMTPInboundMessagesTotal counts inbound SMTP transactions by verdict,
// the SmtpVerdict string (`docs/goal/behavior/smtp-server.md` § Log
// shape, e.g. "rejected_greylist"). The target is one increment per
// transaction; today only the greylist tempfail increments it (§ Metrics
// surface).
var SMTPInboundMessagesTotal = prometheus.NewCounterVec(
	prometheus.CounterOpts{
		Name: "smtp_inbound_messages_total",
		Help: "Count of inbound SMTP transactions handled by the bridge, by verdict.",
	},
	[]string{"verdict"},
)

// CgoCallsTotal counts crossings of the UniFFI / cgo boundary, by the
// foreign symbol name and result ("ok", "error"). Used to monitor cgo
// health — a high error rate at the boundary is usually a sign of a
// UniFFI surface drift between the Rust and Go sides.
var CgoCallsTotal = prometheus.NewCounterVec(
	prometheus.CounterOpts{
		Name: "cgo_calls_total",
		Help: "Count of UniFFI / cgo boundary crossings, by foreign symbol and result.",
	},
	[]string{"symbol", "result"},
)

// SMTPSenderDomainFailOpen counts envelope-sender-domain MX/A checks that
// failed OPEN because the DNS resolver errored (rather than authoritatively
// answering). A non-zero rate means the bridge is accepting mail it could
// not verify the sender domain for — admins watch this to spot a sick
// resolver. The "reason" label is bounded by error class ("timeout" /
// "resolver_error"); see smtp-server.md § Sender-domain + § Metrics.
var SMTPSenderDomainFailOpen = prometheus.NewCounterVec(
	prometheus.CounterOpts{
		Name: "smtp_inbound_sender_domain_fail_open_total",
		Help: "Count of inbound sender-domain MX/A checks that failed open on a resolver error, by error class.",
	},
	[]string{"reason"},
)

// SMTPGreylistCheckFailOpen counts inbound greylist checks
// (`fauna.bridges.check_greylist`) that failed OPEN because the nest call
// errored (transport / decode), so the bridge accepted the envelope rather
// than deferring it. Greylisting is a deferral, so failing open (accept) is
// the safe direction — but a non-zero rate means greylisting is silently
// bypassed because nest is sick; admins watch this. See smtp-server.md
// § Greylisting + § Metrics.
var SMTPGreylistCheckFailOpen = prometheus.NewCounter(
	prometheus.CounterOpts{
		Name: "smtp_inbound_greylist_check_fail_open_total",
		Help: "Count of inbound greylist checks that failed open (accepted) on a nest call error.",
	},
)

// SMTPInboundSrsDecode counts inbound RCPT-TO addresses recognized as SRS
// bounces (`SRS0=`/`SRS1=`) and decoded by nest (`fauna.bridges.decode_srs_bounce`),
// by outcome ("ok" / "not_srs" / "malformed" / "mac_fail" / "expired" /
// "orphan"). `ok` delivers the bounce to the forwarder; `mac_fail` (forged) and
// `expired` are hard-rejected; `orphan` (forwarding row gone) is accepted then
// dropped. Admins watch `mac_fail` (forgery attempts) and `orphan` (deleted
// forwarders still receiving bounces). See mail-forwarding.md § Bounce decode.
var SMTPInboundSrsDecode = prometheus.NewCounterVec(
	prometheus.CounterOpts{
		Name: "smtp_inbound_srs_decode_total",
		Help: "Count of inbound SRS-bounce RCPT recipients decoded by nest, by outcome.",
	},
	[]string{"outcome"},
)

// BridgeRole is a gauge set after Whoami resolves the process's role.
// The resolved role's label gets `1`; the other gets `0`. This makes
// "how many MTA vs. MDA processes are up across the fleet" a single
// sum query in Prometheus and gives dashboards a stable signal that
// the binary did finish role discovery (vs. being stuck pre-Whoami).
//
// Set once at startup from cmd/fauna-mail-bridge/main.go; never
// updated thereafter — role is immutable for a process lifetime.
var BridgeRole = prometheus.NewGaugeVec(
	prometheus.GaugeOpts{
		Name: "bridge_role",
		Help: "Resolved bridge role: 1 for the role this process took (from fauna.bridges.whoami), 0 for the other.",
	},
	[]string{"role"},
)

// SMTPInboundFilterReject counts inbound recipients whose stored filter rules
// fired a `Reject` action (Sieve `reject`), by how the perimeter applied it
// (smtp-server.md § Email filter rules):
//   - "refuse_5xx"       — single-recipient, non-null-sender txn refused
//     `550 5.7.1` at end-of-DATA (the sender's MX bounces it).
//   - "drop_multi"       — multi-recipient txn (we already committed 250 for the
//     others) → the rejected recipient is dropped silently, no DSN.
//   - "drop_null_sender" — null-sender (`<>`) message → dropped silently (we
//     never bounce a bounce).
//
// `drop_*` modes are placement-identical to `Discard`; the counter is what lets
// admins distinguish a Reject-drop from a Discard-drop.
var SMTPInboundFilterReject = prometheus.NewCounterVec(
	prometheus.CounterOpts{
		Name: "smtp_inbound_filter_reject_total",
		Help: "Count of inbound recipients whose filter rules fired Reject, by application mode.",
	},
	[]string{"mode"},
)

// SMTPInboundAutoReplySent counts vacation auto-replies the MTA composed, signed,
// and enqueued after a delivered inbound message matched an `AutoReply` filter
// rule and passed both the loop guard and the rate-limit claim.
var SMTPInboundAutoReplySent = prometheus.NewCounter(
	prometheus.CounterOpts{
		Name: "smtp_inbound_autoreply_sent_total",
		Help: "Count of vacation auto-replies sent.",
	},
)

// SMTPInboundAutoReplySuppressed counts AutoReply matches that did NOT send, by
// reason: "rate" (a reply was already sent within interval_hours) plus the
// loop-guard reasons from fauna_mail::filter::auto_reply_decision —
// "null_sender" / "auto_submitted" / "list" / "self" / "not_in_recipients" —
// plus "panic" (the post-delivery auto-reply stage recovered an unexpected
// panic; the local copy was still delivered). Admins watch these to spot a
// misfiring vacation rule, a loop attempt, or a hostile-header-triggered panic.
var SMTPInboundAutoReplySuppressed = prometheus.NewCounterVec(
	prometheus.CounterOpts{
		Name: "smtp_inbound_autoreply_suppressed_total",
		Help: "Count of AutoReply matches suppressed before sending, by reason.",
	},
	[]string{"reason"},
)

// MailScanningClamavOversize counts inbound messages the ClamAV gate delivered
// **unscanned** because they exceeded its size cap (the `bypassed_oversize`
// verdict).
//
// This is a TRIPWIRE, not a statistic: a non-zero value is a bug
// (mail-content-scanning.md § Oversize messages). Since the 2026-08-26 ruling
// the gate's cap is *derived* from the same live `max_message_bytes` ceiling the
// perimeter's 552 5.3.4 door enforces, so a message large enough to bypass the
// scan is a message the door should already have refused — the two can only
// disagree if the gate was handed no ceiling at all (a mis-wired deployment).
// Admins alert on any increase.
var MailScanningClamavOversize = prometheus.NewCounter(
	prometheus.CounterOpts{
		Name: "mail_scanning_clamav_oversize_total",
		Help: "Tripwire: inbound messages delivered unscanned because they exceeded the ClamAV gate's derived size cap. Any non-zero value is a bug.",
	},
)

func init() {
	// MustRegister panics on duplicate name — that's the desired loud
	// failure if a future maintainer accidentally aliases a metric.
	registry.MustRegister(
		WSRPCCallsTotal,
		WSRPCCallSeconds,
		SMTPConnectionsTotal,
		MDAConnectionsTotal,
		SMTPInboundMessagesTotal,
		CgoCallsTotal,
		SMTPSenderDomainFailOpen,
		SMTPGreylistCheckFailOpen,
		SMTPInboundSrsDecode,
		SMTPInboundFilterReject,
		SMTPInboundAutoReplySent,
		SMTPInboundAutoReplySuppressed,
		MailScanningClamavOversize,
		BridgeRole,
	)
}
