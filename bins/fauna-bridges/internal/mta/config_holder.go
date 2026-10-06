package mta

import (
	"log/slog"
	"net"
	"sync/atomic"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// mtaLiveConfig is the immutable bundle of snapshot-derived config the MTA
// listeners read at the request boundary. It holds every knob that
// `fauna.bridges.config_changed` can mutate for the MTA role: the
// connection-time `Policy` (rate-limit / DNSBL / FCrDNS / HELO /
// sender-domain), the local-domains list (RCPT 250/550 + submission MAIL
// FROM), the inbound size cap, the auth + spam policies, the primary domain
// (submission EHLO / sender identity), and the per-credential AUTH lockout.
//
// It is treated as immutable once stored: a config change builds a fresh
// bundle and atomically swaps the holder's pointer (mtaConfigHolder), so a
// reader that loaded the old pointer keeps a consistent view for the life of
// its read — no field-tearing, no locking on the hot path.
//
// Fields NOT here are intentionally process-static (not in `fetch_config`):
// the content-scan topology (`scanConfig`, operator-hatch clamd/rspamd
// addresses) and `requireStartTLS` (driven by TLS-provisioning state, refreshed
// on the separate fetch_tls_cert_blob cycle). Those stay direct fields on the
// backends.
type mtaLiveConfig struct {
	policy          *Policy
	localDomains    []string
	maxMessageBytes uint32
	authPolicy      wsrpc.AuthPolicy
	spamPolicy      mailfauna.SpamPolicy
	// unlistedRecipientPenalty (points, 0 = off) is threaded separately from
	// spamPolicy (the UniFFI scorer Record, kept unchanged so the 6 clients'
	// generated ctors don't break) — the MTA per-recipient loop applies it to
	// a catch-all recipient's combined score (`mail-spam.md`
	// § Unlisted-recipient penalty).
	unlistedRecipientPenalty uint32
	primaryDomain            string
	// signsOutbound reports whether the nest signs outbound mail for at least
	// one local domain (the snapshot projects a non-empty `dkim_selectors`).
	// The bridge holds no DKIM key and signs nothing — the nest signs at the
	// outbound hand-out — but the submission door refuses a From: header on a
	// domain the deployment does not host only when the deployment signs at
	// all (`550 5.7.7 From: domain not local`, submissionSession.checkFromLocal).
	signsOutbound bool
	authLockout   *AuthLockout
	// outbound is the bridge-side subset of OutboundPolicy the outbound
	// delivery worker applies (IPv6 egress gating + the 5xx→transient
	// allowlist). The other OutboundPolicy fields — retry curve, NDR rate
	// limit, backscatter suppression, TLSRPT opt-out — are owned nest-side
	// (the worker reports attempt outcomes; nest owns the queue + bounce
	// logic), so they are deliberately absent here.
	outbound outboundDeliveryPolicy
}

// mtaConfigHolder is the single hot-reload seam shared by the inbound and
// submission backends within one MTA process. Both backends hold the same
// *mtaConfigHolder; a `config_changed` push (or a reconnect re-fetch) calls
// ApplyConfig once and every listener sees the new bundle at its next request
// boundary, with no restart (mail-policy-config.md § Architectural rules —
// "the bridge MUST apply changes at the next request boundary without a
// restart"; mail-bridge-lifecycle.md § Running — hot-reload mandatory).
//
// The holder retains the injected resolver + clock so ApplyConfig can rebuild
// the derived Policy / AuthLockout from a fresh snapshot using the same
// dependencies the process started with. The rebuild resets the two
// 1-minute ephemeral counters (the per-IP connection RateLimiter and the
// per-credential AuthLockout); that is acceptable for a rare admin-driven
// config change — the windows self-reset every minute anyway and an admin
// cannot be coerced into changing config mid-attack.
type mtaConfigHolder struct {
	live atomic.Pointer[mtaLiveConfig]

	resolver DNSResolver
	clock    Clock
	logger   *slog.Logger
}

// newMTAConfigHolder builds a holder seeded from the startup snapshot. A nil
// resolver defaults to net.DefaultResolver and a nil clock to realClock{} —
// the same defaults newPolicyFromSnapshot / NewAuthLockout apply — so the
// rebuilt config matches the one mta.Run would build directly for the same
// snapshot. A nil logger defaults to slog.Default().
func newMTAConfigHolder(snap wsrpc.ConfigSnapshot, resolver DNSResolver, clock Clock, logger *slog.Logger) *mtaConfigHolder {
	if resolver == nil {
		resolver = net.DefaultResolver
	}
	if clock == nil {
		clock = realClock{}
	}
	if logger == nil {
		logger = slog.Default()
	}
	h := &mtaConfigHolder{resolver: resolver, clock: clock, logger: logger}
	h.live.Store(buildMTALiveConfig(snap, resolver, clock))
	return h
}

// staticMTAConfig wraps a pre-built bundle in a holder that never rebuilds —
// the seam tests use to inject a hand-built Policy / domain list without going
// through a snapshot. ApplyConfig on such a holder still works (it rebuilds
// from a snapshot using the default resolver/clock), but tests that only set
// a static bundle never call it.
func staticMTAConfig(lc *mtaLiveConfig) *mtaConfigHolder {
	h := &mtaConfigHolder{logger: slog.Default()}
	if lc == nil {
		lc = &mtaLiveConfig{}
	}
	h.live.Store(lc)
	return h
}

// current returns the live config bundle. Nil-safe on both the receiver and a
// not-yet-stored pointer so a backend constructed without a holder (drain-only
// unit tests) reads an empty bundle (no rate gate, no local domains) rather
// than panicking — the same lenient shape those tests relied on with the old
// zero-value fields.
func (h *mtaConfigHolder) current() *mtaLiveConfig {
	if h == nil {
		return &mtaLiveConfig{}
	}
	if lc := h.live.Load(); lc != nil {
		return lc
	}
	return &mtaLiveConfig{}
}

// ApplyConfig rebuilds the derived bundle from a freshly-fetched snapshot and
// atomically swaps it in. Registered with the wsrpc.ConfigReloader so both a
// `config_changed` push and a reconnect re-fetch flow through this one seam
// (mta.Run). Safe to call concurrently with listeners reading current().
func (h *mtaConfigHolder) ApplyConfig(snap wsrpc.ConfigSnapshot) {
	h.live.Store(buildMTALiveConfig(snap, h.resolver, h.clock))
	h.logger.Info("mta config hot-applied",
		"local_domains_count", len(snap.LocalDomains),
		"primary_domain", snap.PrimaryDomain,
		"max_conn_per_min", snap.Spam.MaxConnPerMin,
		"max_message_bytes", snap.Spam.MaxMessageBytes,
		"dkim_domains", len(snap.DkimSelectors),
	)
}

// effectiveMaxMessageBytes is the honest perimeter ceiling: the admin's
// `max_message_bytes` knob, and nothing else, since ceiling retirement
// (smtp-server.md § Message size limits).
//
// Crossing the wire is no longer the constraint — a body above the inline
// ceiling rides the bulk-byte plane by reference — and *resting* is no longer a
// constraint either: continuation records rest a sealed body of any size, so
// there is no at-rest limit to clamp against. The product ceiling
// `max_message_bytes` alone governs the perimeter.
//
// The rule itself lives in shared Rust (`fauna_mail::transport_limits`), so the
// value the perimeter enforces and the value EHLO advertises can never drift
// apart. A `0` knob does not mean uncapped — see EffectiveMaxRawMessageBytes.
func effectiveMaxMessageBytes(maxMessageBytes uint32) uint32 {
	return mailfauna.EffectiveMaxRawMessageBytes(maxMessageBytes)
}

// buildMTALiveConfig derives the immutable bundle from a snapshot, mirroring
// the field-by-field wiring mta.Run did inline before hot-reload. The Policy
// and AuthLockout are rebuilt fresh (the connection / credential counters
// reset — see mtaConfigHolder).
func buildMTALiveConfig(snap wsrpc.ConfigSnapshot, resolver DNSResolver, clock Clock) *mtaLiveConfig {
	return &mtaLiveConfig{
		policy:                   newPolicyFromSnapshot(snap, resolver, clock),
		localDomains:             snap.LocalDomains,
		maxMessageBytes:          effectiveMaxMessageBytes(snap.Spam.MaxMessageBytes),
		authPolicy:               snap.Auth,
		spamPolicy:               mailfauna.SpamPolicyFromSnapshot(snap.Spam, snap.Auth),
		unlistedRecipientPenalty: snap.Spam.UnlistedRecipientPenalty,
		primaryDomain:            snap.PrimaryDomain,
		signsOutbound:            len(snap.DkimSelectors) > 0,
		// D.7 per-(credential, source-IP) lockout, keyed off the auth policy's
		// failure cap; zero disables (see NewAuthLockout). The fixed window
		// matches mta.Run's original construction (time.Minute).
		authLockout: NewAuthLockout(snap.Auth.MaxAuthFailuresPerMinute, time.Minute, clock),
		outbound: outboundDeliveryPolicy{
			ipv6Enabled:         snap.Outbound.IPv6Enabled,
			treat5xxAsTransient: snap.Outbound.Treat5xxAsTransient,
		},
	}
}

// outboundPolicy returns the current bridge-side outbound delivery knobs. It
// is a method value (`cfg.outboundPolicy`) handed to the OutboundWorker's
// SMTP sender so each delivery attempt reads the live config — a
// `config_changed` swap hot-applies at the next attempt with no restart.
func (h *mtaConfigHolder) outboundPolicy() outboundDeliveryPolicy {
	return h.current().outbound
}
