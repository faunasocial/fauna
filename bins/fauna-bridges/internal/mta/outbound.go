// Phase D.5: outbound MX delivery worker.
//
// The Go MTA bridge polls nest's `outbound_mail_queue` via
// `fauna.bridges.fetch_outbound_due`, attempts MX delivery for each
// returned row, and reports the result back via one of
// `mark_outbound_delivered` / `mark_outbound_failed` /
// `mark_outbound_bounced`. Restart resumes from nest's
// `next_attempt_at` — no local-disk spool on the bridge side.
//
// MTA-STS enforcement (RFC 8461 §5) is wired: deliverOne fetches the
// recipient domain's policy once via fauna.bridges.fetch_mta_sts_policy
// and applies the per-host enforce/testing decision (matching the
// connected MX against the policy's mx: list with fauna_ffi.MtaStsMxMatches)
// before each Send, requiring WebPKI-verified TLS under enforce. DANE/TLSA
// hardening remains a follow-up; STARTTLS is otherwise opportunistic
// (RFC 7435: upgrade-if-offered, accept any cert, plaintext fallback).
package mta

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/hex"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/smtp"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/byteplane"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	fauna_ffi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// ── Interfaces (test seams) ──────────────────────────────────────

// MXResolver resolves the MX (or implicit A/AAAA) hosts for an SMTP
// recipient domain. Production code uses NestMXResolver (nest does the
// DNSSEC-validating lookup); tests use a scripted map.
type MXResolver interface {
	LookupMX(ctx context.Context, domain string) (MXAnswer, error)
}

// MXAnswer is one recipient domain's ranked SMTP targets plus the DNSSEC
// provenance of the RRset they came from.
//
// Secure says the MX RRset was DNSSEC-validated. Outbound DANE MUST be
// gated on it (RFC 7672 §2.2: an SMTP client whose MX RRset is not
// DNSSEC-validated must not treat the destination as DANE-capable).
// Validating only the TLSA leg is not enough: an attacker who can spoof DNS
// — exactly the attacker DANE exists to stop — forges
// `MX victim.test → mx.attacker.test`, publishes a genuine DNSSEC-signed
// TLSA for their OWN name, and the pin then succeeds honestly against the
// wrong host. The validation that ran was validating the name the attacker
// chose.
//
// The zero value is the fail-safe one: an answer whose provenance nobody
// established is not DANE-capable. Secure=false narrows the posture to
// MTA-STS/opportunistic — it never fails delivery, exactly as an Insecure
// *TLSA* answer already behaves.
type MXAnswer struct {
	Hosts  []MXHost
	Secure bool
}

// MXHost is one ranked SMTP target. `Pref` lowest-numbered first
// (RFC 5321 §5.1); same-Pref hosts are reshuffled to a deterministic
// order at LookupMX time (the caller iterates them in order).
//
// Hostname is normally a bare hostname (implicit SMTP port 25), as
// emitted by LiveMXResolver. It MAY carry an explicit `host:port` suffix
// when sourced from an operator transport override (OverrideMXResolver /
// operator-hatch `mta_mx_override`); DefaultSMTPSender dials such a value
// verbatim and uses the bare host for TLS ServerName.
type MXHost struct {
	Hostname string
	Pref     uint16
}

// TLSMode is the cert-verification posture for one outbound delivery
// attempt.
//
//   - TLSOpportunistic (RFC 7435 opportunistic security): upgrade to TLS
//     if the MX advertises STARTTLS, but accept ANY server certificate —
//     the alternative is cleartext, so an unauthenticated TLS channel is
//     strictly better. A self-signed / mismatched MX cert must not
//     downgrade to plaintext or fail the attempt. If STARTTLS is not
//     advertised, deliver in the clear. This is the default for `mode=none`,
//     `not_published`, `fetch_error`, `invalid`, and (testing-mode)
//     mismatches.
//   - TLSRequired (RFC 8461 MTA-STS enforce): STARTTLS is mandatory AND the
//     server certificate must WebPKI-verify against the connected MX
//     hostname. A missing STARTTLS extension, a failed handshake, or a
//     verification failure is a (temporary) attempt failure — never a
//     plaintext downgrade. Set only when `mode=enforce` AND the connected MX
//     matched an `mx:` pattern.
//   - TLSDanePinned (RFC 7672 DANE): STARTTLS is mandatory AND the presented
//     cert chain must match one of the published DNSSEC-secure TLSA records
//     (via fauna_ffi.DaneChainMatches) instead of WebPKI. DANE > MTA-STS:
//     when a surviving host publishes usable TLSA records the handshake is
//     pinned to them even under an MTA-STS enforce policy. A pin mismatch /
//     missing STARTTLS is a (temporary) attempt failure — never plaintext.
type TLSMode int

const (
	// TLSOpportunistic upgrades if offered, accepts any cert, falls back
	// to plaintext if STARTTLS is absent.
	TLSOpportunistic TLSMode = iota
	// TLSRequired demands STARTTLS + a WebPKI-verified server cert.
	TLSRequired
	// TLSDanePinned demands STARTTLS + a chain matching a published TLSA
	// record.
	TLSDanePinned
)

// TLSPolicy is the full TLS posture for one delivery attempt: the
// verification Mode plus, for TLSDanePinned, the DANE/TLSA records the
// presented chain must match. Carried by value to SMTPSender.Send.
type TLSPolicy struct {
	Mode TLSMode
	// DaneRecords is non-empty iff Mode == TLSDanePinned; the server's cert
	// chain must match one of them (fauna_ffi.DaneChainMatches).
	DaneRecords []wsrpc.TlsaRecordWire
}

// TLSAttemptOutcome is the TLSRPT-relevant verdict of one Send attempt,
// returned alongside the delivery error so deliverOne can report it to
// nest's TLSRPT aggregator (RFC 8460). It is independent of the delivery
// error: a successful TLS session that then 5xx's still reports its TLS
// outcome, and a plaintext fallback that delivers fine still reports
// `starttls-not-supported`.
//
//   - Reportable=false — no TLSRPT record (a failure with no TLS verdict:
//     dial / EHLO / pre-handshake error). The zero value, so an early
//     return reports nothing.
//   - Reportable=true, ResultType=="" — a successful TLS session; reported
//     as RFC 8460 result_type=None (a counted successful-session).
//   - Reportable=true, ResultType=="<token>" — an RFC 8460 §4.3 failure
//     token (e.g. "starttls-not-supported", "sts-webpki-invalid").
type TLSAttemptOutcome struct {
	Reportable bool
	ResultType string
}

// SMTPSender sends one RFC 5321 envelope (MAIL FROM → RCPT TO → DATA)
// against one MX host. Returns nil on a 2xx end-of-DATA response,
// PermanentError for a 5xx, TemporaryError for a 4xx / network /
// TLS / connection error.
//
// The first return is the TLS-handshake verdict for TLSRPT (see
// TLSAttemptOutcome) — surfaced separately from the delivery error so a
// successful TLS session that later fails the SMTP transaction still
// reports its TLS outcome, and an opportunistic plaintext fallback still
// reports `starttls-not-supported`.
//
// tlsPolicy carries the MTA-STS/DANE-derived posture (see TLSPolicy):
// TLSOpportunistic upgrades-if-offered accepting any cert (RFC 7435);
// TLSRequired demands STARTTLS + WebPKI verification (RFC 8461 enforce);
// TLSDanePinned demands STARTTLS + a TLSA-matching chain (RFC 7672).
type SMTPSender interface {
	Send(ctx context.Context, host string, from string, recipient string, body []byte, tlsPolicy TLSPolicy) (TLSAttemptOutcome, error)
}

// PermanentError signals a 5xx-class final failure; the worker maps
// this to `mark_outbound_bounced`.
type PermanentError struct{ msg string }

func (e *PermanentError) Error() string { return e.msg }

// NewPermanentError wraps a final-failure message. Exported so the
// fake-MX integration test can simulate a 5xx outcome.
func NewPermanentError(msg string) error { return &PermanentError{msg: msg} }

// TemporaryError signals a 4xx-class / network / TLS / connection
// failure; the worker maps this to `mark_outbound_failed` (retry).
type TemporaryError struct{ msg string }

func (e *TemporaryError) Error() string { return e.msg }

// NewTemporaryError wraps a soft-failure message.
func NewTemporaryError(msg string) error { return &TemporaryError{msg: msg} }

// ── Worker config + struct ────────────────────────────────────────

// OutboundWorkerConfig tunes the polling cadence and per-poll batch
// size. Sensible defaults are filled in via fillDefaults so call
// sites can pass a zero value.
type OutboundWorkerConfig struct {
	// PollInterval is the wallclock period between successive polls
	// when nest returned an empty batch. After a non-empty batch the
	// worker re-polls immediately so a backlog drains quickly.
	PollInterval time.Duration
	// BatchSize is the `max` field on FetchOutboundDueRequest.
	BatchSize uint32
	// LeaseSeconds is the `lease_seconds` advisory field on
	// FetchOutboundDueRequest.
	LeaseSeconds uint32
	// AttemptTimeout caps the wallclock time for one MX dial + SMTP
	// send. Defends against a slow remote tying up a worker slot.
	AttemptTimeout time.Duration
}

func (c *OutboundWorkerConfig) fillDefaults() {
	if c.PollInterval <= 0 {
		c.PollInterval = 30 * time.Second
	}
	if c.BatchSize == 0 {
		c.BatchSize = 16
	}
	if c.LeaseSeconds == 0 {
		c.LeaseSeconds = 60
	}
	if c.AttemptTimeout <= 0 {
		c.AttemptTimeout = 60 * time.Second
	}
}

// OutboundWorker drains nest's outbound queue against external MX
// hosts. One worker goroutine per MTA process; spawn via Start.
type OutboundWorker struct {
	client wsrpc.Caller
	mx     MXResolver
	smtp   SMTPSender
	cfg    OutboundWorkerConfig
	logger *slog.Logger
	// helo is the EHLO host the SMTP sender announces to remote MX
	// peers; mirrors the bridge's nest-ratified domain.
	helo string

	// trigger is a unit-buffered channel an external caller can poke
	// to short-circuit the inter-poll sleep — e.g. submission Data
	// just enqueued a row, so the next poll should happen now.
	trigger chan struct{}

	// bytePlane resolves a unit whose body was staged on the bulk-byte plane
	// (u.StagedBody set: the plaintext body was over the inline reply budget, so
	// nest sealed it under a one-shot AEAD key and staged the ciphertext — the
	// staged-envelope rule, smtp-server.md § Message size limits). Set from
	// mta.go, sharing the one byte-plane client the inbound + submission legs use.
	// nil ⇒ no staged bodies can be resolved (a test seam: tests that don't
	// exercise staging leave it nil).
	bytePlane *byteplane.Client
}

// bodyFor returns the wire bytes to deliver for u: the unit's RawMessage
// verbatim. The nest signs each message as it hands it out, so the bytes are
// relayed exactly as received — the worker signs nothing.
//
// When the unit's body was staged on the bulk-byte plane (u.StagedBody set,
// RawMessage empty — the staged-envelope rule), it is resolved instead: the
// chunks are fetched, joined fail-closed on the declared total, and AEAD-opened
// to recover the identical plaintext.
// A resolution failure returns an error so deliverOne fails closed (never ships
// a partial/empty body); nest re-stages statelessly, so a retry gets a fresh
// reference.
func (w *OutboundWorker) bodyFor(ctx context.Context, u wsrpc.OutboundUnit) ([]byte, error) {
	raw := u.RawMessage
	if u.StagedBody != nil {
		resolved, err := resolveStagedOutboundBody(ctx, w.bytePlane, u.StagedBody)
		if err != nil {
			return nil, err
		}
		raw = resolved
	}
	return raw, nil
}

// resolveStagedOutboundBody recovers the plaintext body a unit staged on the
// bulk-byte plane under a one-shot AEAD envelope (the staged-envelope rule,
// smtp-server.md § Message size limits). Mirrors the inbound-fetch resolver
// (mda/imap.SealedBodyOf) plus the AEAD open: download each ciphertext chunk over
// the OPEN download route (no token — the bytes are AEAD-sealed and useless
// without the key), join with the shared body_ref rejoin, pin the join against
// the declared total, then AEAD-open with the reference's one-shot key.
//
// Fails closed at every step. The declared total catches a reference that named
// the wrong chunks, reordered them, or dropped one (the chunk *contents* are
// self-verifying but the chunk *list* is not); the AEAD tag then authenticates
// the entire join, so a corrupt key or tampered ciphertext surfaces as an error,
// never a silent partial/empty body.
func resolveStagedOutboundBody(ctx context.Context, plane *byteplane.Client, ref *wsrpc.StagedBodyRef) ([]byte, error) {
	if plane == nil {
		return nil, fmt.Errorf(
			"outbound unit body rides the bulk-byte plane but no byte-plane client is wired")
	}
	chunks := make([][]byte, 0, len(ref.ChunkHashes))
	for i, h := range ref.ChunkHashes {
		b, err := plane.DownloadChunk(ctx, hex.EncodeToString(h))
		if err != nil {
			return nil, fmt.Errorf("fetch staged outbound body chunk %d of %d: %w", i+1, len(ref.ChunkHashes), err)
		}
		chunks = append(chunks, b)
	}
	// Shared Rust rejoin — the same one the MTA/nest split with. Never re-derive.
	joined := mailfauna.JoinSealedMailBody(chunks)
	if uint64(len(joined)) != ref.TotalBytes {
		return nil, fmt.Errorf(
			"staged outbound body reference declared %d bytes but its chunks joined to %d",
			ref.TotalBytes, len(joined))
	}
	raw, err := mailfauna.OpenStagedBody(joined, ref.Key)
	if err != nil {
		return nil, fmt.Errorf("open staged outbound body envelope: %w", err)
	}
	return raw, nil
}

// NewOutboundWorker constructs a worker; nil arguments fall back to
// the production defaults (LiveMXResolver + DefaultSMTPSender + a
// freshly-constructed config). `client` is required and may not be
// nil (the worker can't function without nest).
func NewOutboundWorker(
	client wsrpc.Caller,
	mx MXResolver,
	smtpSender SMTPSender,
	helo string,
	cfg OutboundWorkerConfig,
	logger *slog.Logger,
) (*OutboundWorker, error) {
	if client == nil {
		return nil, errors.New("OutboundWorker: client is required")
	}
	cfg.fillDefaults()
	if mx == nil {
		// nest's DNSSEC-validating resolver, not the Go stdlib one: outbound
		// DANE may only bind to a name that came out of a validated MX RRset
		// (RFC 7672 §2.2), and only nest can establish that.
		mx = NestMXResolver{Client: client}
	}
	if smtpSender == nil {
		smtpSender = NewDefaultSMTPSender(helo)
	}
	if logger == nil {
		logger = slog.Default()
	}
	return &OutboundWorker{
		client:  client,
		mx:      mx,
		smtp:    smtpSender,
		cfg:     cfg,
		logger:  logger,
		helo:    helo,
		trigger: make(chan struct{}, 1),
	}, nil
}

// Trigger nudges the worker to re-poll without waiting for the
// PollInterval. Safe to call from any goroutine; coalesces (a flood
// of triggers becomes one extra poll). No-op if the worker has not
// been Start'd.
func (w *OutboundWorker) Trigger() {
	select {
	case w.trigger <- struct{}{}:
	default:
	}
}

// Start spawns the poll-loop goroutine. Returns immediately. Stop the
// worker by cancelling ctx; the goroutine drains its in-flight
// attempt and exits cleanly.
func (w *OutboundWorker) Start(ctx context.Context) *sync.WaitGroup {
	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		w.run(ctx)
	}()
	return &wg
}

// run is the worker's main loop. Polls in a tight cycle while units
// are returned; sleeps PollInterval (or until Trigger fires / ctx
// cancels) when the queue is empty.
func (w *OutboundWorker) run(ctx context.Context) {
	w.logger.Info("outbound worker starting",
		"poll_interval", w.cfg.PollInterval,
		"batch_size", w.cfg.BatchSize,
	)
	for {
		select {
		case <-ctx.Done():
			w.logger.Info("outbound worker stopping", "reason", ctx.Err())
			return
		default:
		}

		drained, err := w.pollOnce(ctx)
		if err != nil {
			// Transport errors during the poll itself: log and sleep
			// the configured PollInterval. A persistent nest outage
			// otherwise would tight-loop the bridge.
			w.logger.Warn("outbound poll failed", "err", err)
			if !sleepOrCancel(ctx, w.cfg.PollInterval, w.trigger) {
				return
			}
			continue
		}
		if !drained {
			// Empty batch: sleep the poll interval. Any newly-enqueued
			// row Trigger()s us awake immediately.
			if !sleepOrCancel(ctx, w.cfg.PollInterval, w.trigger) {
				return
			}
		}
	}
}

// sleepOrCancel sleeps `d`, returning false if ctx was cancelled or
// the trigger channel fired. Used to fast-drain the queue on
// submission while still respecting the poll cadence when idle.
func sleepOrCancel(ctx context.Context, d time.Duration, trigger <-chan struct{}) bool {
	t := time.NewTimer(d)
	defer t.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-trigger:
		return true
	case <-t.C:
		return true
	}
}

// pollOnce drains one batch from nest. Returns (true, nil) when at
// least one unit was processed (caller should re-poll immediately to
// keep draining); (false, nil) on an empty batch.
func (w *OutboundWorker) pollOnce(ctx context.Context) (bool, error) {
	pollCtx, cancel := context.WithTimeout(ctx, 30*time.Second)
	units, err := wsrpc.FetchOutboundDue(pollCtx, w.client, w.cfg.BatchSize, w.cfg.LeaseSeconds)
	cancel()
	if err != nil {
		return false, fmt.Errorf("fetch_outbound_due: %w", err)
	}
	if len(units) == 0 {
		return false, nil
	}
	for _, u := range units {
		// Respect ctx between units so a shutdown signal doesn't have
		// to wait for the whole batch.
		select {
		case <-ctx.Done():
			return true, nil
		default:
		}
		w.handleUnit(ctx, u)
	}
	return true, nil
}

// handleUnit attempts delivery for one row and reports the result
// back to nest. Never returns an error — every outcome is reported
// to nest via mark_outbound_*; a failed report is logged and the row
// stays pending for the next poll (nest will re-emit it).
func (w *OutboundWorker) handleUnit(ctx context.Context, u wsrpc.OutboundUnit) {
	logger := w.logger.With(
		"id", u.ID,
		"recipient", u.Recipient,
		"attempt", u.AttemptCount+1,
	)
	deliverCtx, cancel := context.WithTimeout(ctx, w.cfg.AttemptTimeout)
	defer cancel()
	err := w.deliverOne(deliverCtx, u)
	switch {
	case err == nil:
		logger.Info("outbound delivered")
		w.reportDelivered(ctx, u.ID, logger)
	case isPermanent(err):
		logger.Warn("outbound bounced", "err", err)
		w.reportBounced(ctx, u.ID, err.Error(), logger)
	default:
		// Temporary failure (4xx / network / TLS). nest owns the retry
		// curve AND the give-up decision (smtp-server.md § Outbound
		// delivery: "nest reschedules per the retry schedule below"); the
		// worker just reports the failure and lets nest reschedule, emit
		// the 4 h delay-warning, or bounce on budget/timeout exhaustion.
		// retry_after_seconds is reserved for a parsed server Retry-After
		// hint (none surfaced today → 0); nest honours it as a floor under
		// its curve, never a ceiling.
		logger.Info("outbound failed; reported to nest for reschedule", "err", err)
		w.reportFailed(ctx, u.ID, 0, err.Error(), logger)
	}
}

// deliverOne resolves MX hosts for the recipient's domain and walks
// them in order, returning nil on the first successful send.
func (w *OutboundWorker) deliverOne(ctx context.Context, u wsrpc.OutboundUnit) error {
	domain := domainOf(u.Recipient)
	if domain == "" {
		return &PermanentError{msg: fmt.Sprintf("recipient lacks @domain: %q", u.Recipient)}
	}
	mxAnswer, err := w.mx.LookupMX(ctx, domain)
	if err != nil {
		// DNS resolution issues (NXDOMAIN, SERVFAIL) are split between
		// permanent ("recipient domain doesn't exist") and temporary
		// ("DNS server unreachable"). Without DNSSEC plumbing we can't
		// reliably distinguish on the wire — treat any resolution
		// failure as temporary; a domain that doesn't exist will
		// continue failing and eventually hit the retry budget bounce.
		return &TemporaryError{msg: fmt.Sprintf("mx lookup %s: %v", domain, err)}
	}
	hosts := mxAnswer.Hosts
	if len(hosts) == 0 {
		return &PermanentError{msg: fmt.Sprintf("no MX or A record for %s", domain)}
	}

	// MTA-STS policy fetch (RFC 8461), once per delivery. nest owns the
	// TXT + well-known fetch and the per-max_age cache; we apply the
	// per-host enforce/testing decision locally. An RPC error must NOT
	// block delivery — log and proceed opportunistically for every host
	// (smtp-server.md § MX resolution, item 3 + §5).
	//
	// stsOutcome retains the raw 4-way outcome so each per-host TLSRPT report
	// (report_tls_attempt) lets nest reconstruct the same RFC 8460 §4.4
	// policy bucket without a second fetch. An RPC error maps to
	// NotPublished (no policy applies to the bucket). The token vocabulary is
	// owned by fauna_mail::outbound::mta_sts::MtaStsOutcome and pinned by
	// internal/wsrpc/go_wire_outcome_contract_test.go, which is also why no
	// token in this file may be spelled as a literal.
	stsOutcome := wsrpc.MtaStsOutcomeNotPublished
	var stsPolicy *wsrpc.MtaStsPolicyWire
	stsReply, stsErr := wsrpc.FetchMtaStsPolicy(ctx, w.client, domain)
	switch {
	case stsErr != nil:
		w.logger.Warn("mta-sts policy fetch failed; proceeding without enforcement",
			"domain", domain, "err", stsErr)
	case wsrpc.MtaStsOutcome(stsReply.Outcome) == wsrpc.MtaStsOutcomeFound && stsReply.Policy != nil:
		stsOutcome = wsrpc.MtaStsOutcomeFound
		stsPolicy = stsReply.Policy
	case wsrpc.MtaStsOutcome(stsReply.Outcome) == wsrpc.MtaStsOutcomeFound:
		// found-without-a-policy-body is malformed; treat as no usable policy
		// (and don't report it as found — nest rejects found-without-policy).
		w.logger.Warn("mta-sts outcome=found without policy; proceeding opportunistically",
			"domain", domain)
	default:
		// not-published / fetch-error / invalid → no enforcement (RFC 8461
		// §5: a published-but-broken policy never forces plaintext or
		// refusal). The fetch-error / invalid signals are reported per-host
		// below for TLSRPT.
		stsOutcome = wsrpc.MtaStsOutcome(stsReply.Outcome)
	}

	from := u.OriginalSender
	// Resolve the deliverable body once, reused across the per-host retry fan-out
	// below: staged bodies are fetched + AEAD-opened from the byte plane. A
	// staged-resolve failure is transient — nest re-stages statelessly per
	// serve, so a re-fetch yields a fresh reference; fail closed rather than
	// ship a partial/empty body.
	outboundBody, err := w.bodyFor(ctx, u)
	if err != nil {
		return &TemporaryError{msg: fmt.Sprintf("resolve staged outbound body: %v", err)}
	}
	var lastErr error
	var enforceSkipped bool
	for _, h := range hosts {
		_, bareHost := mxDialTarget(h.Hostname)
		tlsPolicy := TLSPolicy{Mode: TLSOpportunistic}
		// stsTestingMismatch records that this host failed the policy `mx:`
		// match under a *testing*-mode policy → a `sts-policy-mismatch`
		// pre-TLS TLSRPT signal (emitted under enforce AND testing per
		// smtp-server.md:474; the enforce case reports + refuses below).
		stsTestingMismatch := false
		if stsPolicy != nil {
			matches := fauna_ffi.MtaStsMxMatches(stsPolicy.Mx, bareHost)
			switch wsrpc.MtaStsMode(stsPolicy.Mode) {
			case wsrpc.MtaStsModeEnforce:
				if !matches {
					// Refuse this host (RFC 8461 §5; smtp-server.md:452):
					// fall through to the next MX. NOT a permanent bounce —
					// refusal is per-host. Only after every host is
					// exhausted does the temporary "no host matched" result
					// reach nest for reschedule. Report the TLSRPT pre-TLS
					// `sts-policy-mismatch` signal first (smtp-server.md:474:
					// emitted under enforce too); no TLSA lookup happened for a
					// refused host, so the bucket carries no DANE strings.
					enforceSkipped = true
					w.logger.Info("mta-sts enforce: MX does not match policy; refusing host",
						"mx", bareHost, "policy_mx", stsPolicy.Mx)
					w.reportTLSAttempt(ctx, domain, bareHost, strptr("sts-policy-mismatch"), stsOutcome, stsPolicy, nil)
					continue
				}
				tlsPolicy.Mode = TLSRequired
			case wsrpc.MtaStsModeTesting:
				if !matches {
					// Record the mismatch but proceed opportunistically
					// (smtp-server.md:453).
					stsTestingMismatch = true
					w.logger.Info("mta-sts testing: MX does not match policy; proceeding opportunistically",
						"mx", bareHost, "policy_mx", stsPolicy.Mx)
				}
				// testing never requires TLS; leave tlsPolicy opportunistic.
			default:
				// mode=none (or any unrecognised mode) → no enforcement.
			}
		}

		// DANE/TLSA pinning (RFC 7672), per surviving host. nest owns the
		// `_25._tcp.<host>` DNSSEC lookup (the Go stdlib can't do DNSSEC);
		// we pin the handshake against the returned records. Applied AFTER
		// the MTA-STS refusal gate (smtp-server.md:451) so a refused host
		// costs no TLSA fetch. DANE > MTA-STS (smtp-server.md:449): secure
		// TLSA records override the WebPKI-required posture. A fetch RPC
		// error must NOT block delivery — log and proceed with the
		// MTA-STS/opportunistic posture (a TLSA fetch failure is not a
		// delivery failure).
		//
		// ⚠ Gated on the MX RRset's DNSSEC provenance (RFC 7672 §2.2): an
		// SMTP client whose MX RRset was not validated MUST NOT treat the
		// destination as DANE-capable. Validating the TLSA leg alone
		// authenticates whatever name the MX answer carried — a DNS-spoofing
		// attacker forges `MX victim → mx.attacker`, publishes a genuine
		// signed TLSA for their own name, and the pin succeeds honestly
		// against the wrong host. Skipping (not failing) is the required
		// direction, and skipping BEFORE the fetch mirrors the MTA-STS
		// refusal gate above: a host that can never be pinned costs no RPC.
		var tlsaRecords []wsrpc.TlsaRecordWire
		if !mxAnswer.Secure {
			w.logger.Info("mx rrset not DNSSEC-validated; delivering without DANE",
				"domain", domain, "mx", bareHost)
		} else if tlsaReply, tlsaErr := wsrpc.FetchTlsa(ctx, w.client, bareHost); tlsaErr != nil {
			w.logger.Warn("tlsa fetch failed; proceeding without DANE pinning",
				"mx", bareHost, "err", tlsaErr)
		} else if len(tlsaReply.Records) > 0 {
			tlsaRecords = tlsaReply.Records
			tlsPolicy = TLSPolicy{Mode: TLSDanePinned, DaneRecords: tlsaReply.Records}
			w.logger.Info("dane: pinning handshake to published TLSA records",
				"mx", bareHost, "records", len(tlsaReply.Records))
		}

		// Pre-TLS STS signal for TLSRPT (smtp-server.md:474), independent of
		// the handshake. fetch_error / invalid are no-policy-for-delivery
		// (RFC 8461 §5) but still reported; testing-mode mismatch is reported
		// here (enforce-mode mismatch already reported + refused above). nest
		// reconstructs the same bucket from (stsOutcome, stsPolicy, tlsaRecords).
		if signal := preTLSStsSignal(stsOutcome, stsTestingMismatch); signal != "" {
			w.reportTLSAttempt(ctx, domain, bareHost, &signal, stsOutcome, stsPolicy, tlsaRecords)
		}

		tlsOutcome, err := w.smtp.Send(ctx, h.Hostname, from, u.Recipient, outboundBody, tlsPolicy)
		// TLS-handshake outcome report (RFC 8460), best-effort. A successful
		// TLS session reports result_type=None; a TLS failure reports its
		// §4.3 token; a non-TLS failure (dial / EHLO) reports nothing.
		if tlsOutcome.Reportable {
			var resultType *string
			if tlsOutcome.ResultType != "" {
				resultType = &tlsOutcome.ResultType
			}
			w.reportTLSAttempt(ctx, domain, bareHost, resultType, stsOutcome, stsPolicy, tlsaRecords)
		}
		if err == nil {
			return nil
		}
		if isPermanent(err) {
			// A 5xx from any MX is treated as authoritative for the
			// recipient: walking further MX hosts wouldn't recover a
			// "mailbox does not exist" verdict.
			return err
		}
		lastErr = err
		w.logger.Info("outbound MX attempt failed; trying next", "mx", h.Hostname, "err", err)
	}
	if lastErr == nil {
		if enforceSkipped {
			// Every candidate host was enforce-refused; none was attempted.
			// Temporary so nest reschedules (the policy or MX set may
			// change before the next attempt) — never a permanent bounce.
			return &TemporaryError{msg: fmt.Sprintf("mta-sts enforce: no MX host matched policy for %s", domain)}
		}
		return &TemporaryError{msg: "no MX hosts attempted"}
	}
	return lastErr
}

// ── Reporting helpers ─────────────────────────────────────────────

func (w *OutboundWorker) reportDelivered(ctx context.Context, id int64, logger *slog.Logger) {
	rctx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	if err := wsrpc.MarkOutboundDelivered(rctx, w.client, id); err != nil {
		logger.Warn("mark_outbound_delivered failed", "err", err)
	}
}

func (w *OutboundWorker) reportFailed(ctx context.Context, id int64, retryAfter uint32, reason string, logger *slog.Logger) {
	rctx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	if err := wsrpc.MarkOutboundFailed(rctx, w.client, id, retryAfter, reason); err != nil {
		logger.Warn("mark_outbound_failed failed", "err", err)
	}
}

func (w *OutboundWorker) reportBounced(ctx context.Context, id int64, reason string, logger *slog.Logger) {
	rctx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	if err := wsrpc.MarkOutboundBounced(rctx, w.client, id, reason); err != nil {
		logger.Warn("mark_outbound_bounced failed", "err", err)
	}
}

// reportTLSAttempt reports one per-host outbound TLS attempt outcome to
// nest's TLSRPT aggregator (RFC 8460) via fauna.bridges.report_tls_attempt.
// nest reconstructs the RFC 8460 §4.4 policy bucket from (stsOutcome,
// stsPolicy, tlsaRecords) via the shared pure policy_for_attempt; this
// passes the raw per-attempt facts plus the result-type token (nil =
// successful TLS session). Best-effort: a reporting failure is logged and
// never blocks or fails delivery — TLSRPT is cooperative, not load-bearing.
func (w *OutboundWorker) reportTLSAttempt(
	ctx context.Context,
	recipientDomain, mxHost string,
	resultType *string,
	stsOutcome wsrpc.MtaStsOutcome,
	stsPolicy *wsrpc.MtaStsPolicyWire,
	tlsaRecords []wsrpc.TlsaRecordWire,
) {
	rctx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	if err := wsrpc.ReportTlsAttempt(rctx, w.client, recipientDomain, mxHost, resultType, stsOutcome, stsPolicy, tlsaRecords); err != nil {
		w.logger.Warn("report_tls_attempt failed; TLSRPT record dropped",
			"domain", recipientDomain, "mx", mxHost, "err", err)
	}
}

// ── Error helpers ─────────────────────────────────────────────────

func isPermanent(err error) bool {
	var p *PermanentError
	return errors.As(err, &p)
}

// strptr returns a pointer to s — for the result_type Option<String> the
// report wire carries (nil = successful TLS session).
func strptr(s string) *string { return &s }

// preTLSStsSignal returns the RFC 8460 §4.3 pre-TLS STS result-type token
// for an attempt that *reaches* the handshake, or "" for no pre-TLS signal.
// The enforce-mode mismatch (which refuses the host) is reported separately
// by the caller before the refusal. `testingMismatch` is true iff a
// testing-mode policy's `mx:` list did not match this host.
func preTLSStsSignal(stsOutcome wsrpc.MtaStsOutcome, testingMismatch bool) string {
	switch {
	case stsOutcome == wsrpc.MtaStsOutcomeFetchError:
		return "sts-policy-fetch-error"
	case stsOutcome == wsrpc.MtaStsOutcomeInvalid:
		return "sts-policy-invalid"
	case testingMismatch:
		return "sts-policy-mismatch"
	default:
		return ""
	}
}

// classifyTLSHandshakeError maps a failed STARTTLS handshake `err` to its
// RFC 8460 §4.3 `result-type` token, given the TLS `mode` that was in
// effect. This is the genuinely Go-side half of TLSRPT attribution: the
// handshake runs in crypto/tls + net/smtp, so the legacy in-nest
// `tls_failure_type` (which maps the Rust `mail_send` client's error enum)
// can't be shared — only the RFC 8460 §4.3 token set is the common
// contract. The `starttls-not-supported` token (STARTTLS not advertised)
// is handled by the caller, not here — this classifies only an *attempted*
// handshake that failed.
//
// Precedence mirrors the legacy: DANE-active failures are `tlsa-invalid`
// (DANE > WebPKI per RFC 8460 §4.3); under an enforce-mode MTA-STS policy a
// hostname mismatch is `certificate-host-mismatch` and any other WebPKI
// failure is `sts-webpki-invalid`; opportunistic StartTLS failures (cert is
// not verified — InsecureSkipVerify) are protocol-level `validation-failure`.
func classifyTLSHandshakeError(err error, mode TLSMode) string {
	switch mode {
	case TLSDanePinned:
		// The handshake pinned to a published TLSA record (VerifyPeerCertificate);
		// any failure is a DANE-pin/validation failure.
		return "tlsa-invalid"
	case TLSRequired:
		// MTA-STS enforce: full WebPKI verification ran. A hostname mismatch is
		// its own token; everything else (untrusted chain, expiry, protocol) is
		// a generic WebPKI failure under STS.
		var hostErr x509.HostnameError
		if errors.As(err, &hostErr) {
			return "certificate-host-mismatch"
		}
		return "sts-webpki-invalid"
	default: // TLSOpportunistic
		// Cert is not verified (InsecureSkipVerify), so this is a protocol-level
		// STARTTLS failure, not a verification verdict.
		return "validation-failure"
	}
}

// daneChainMatches maps the wire TLSA records to the FFI record type and
// asks the shared-Rust decision (fauna_ffi.DaneChainMatches) whether the
// presented chain satisfies any of them. `rawCerts` is the raw DER chain
// Go's tls.Config.VerifyPeerCertificate hands us (leaf-first), which is the
// order fauna_mail::outbound::dane::dane_chain_matches expects. `mxHost` is
// the host this connection was dialled as, i.e. the TLSA base domain: a
// DANE-TA (usage 2) record names a trust ANCHOR, so satisfying it means the
// leaf chains to the matched cert AND carries this name -- not merely that
// some cert in the chain hashes right (RFC 7672 3.1.1). The whole decision
// stays Rust-side because the path validation has to run against the cert
// the record matched, which only the matcher knows.
func daneChainMatches(records []wsrpc.TlsaRecordWire, rawCerts [][]byte, mxHost string) bool {
	ffiRecords := make([]fauna_ffi.DaneTlsaRecord, len(records))
	for i, r := range records {
		ffiRecords[i] = fauna_ffi.DaneTlsaRecord{
			Usage:    r.Usage,
			Selector: r.Selector,
			Matching: r.Matching,
			Data:     r.Data,
		}
	}
	return fauna_ffi.DaneChainMatches(ffiRecords, rawCerts, mxHost)
}

func domainOf(rcpt string) string {
	at := strings.LastIndex(rcpt, "@")
	if at < 0 || at == len(rcpt)-1 {
		return ""
	}
	return rcpt[at+1:]
}

// mxDialTarget splits an MX target into the TCP dial address (always
// host:port) and the bare host for the SMTP/TLS server name. A target
// that already carries a port (operator transport override) is dialed
// verbatim; a bare hostname (the DNS-MX common case) gets the implicit
// SMTP port 25.
func mxDialTarget(host string) (addr, serverName string) {
	if h, _, err := net.SplitHostPort(host); err == nil {
		return host, h
	}
	return net.JoinHostPort(host, "25"), host
}

// ── Production MX resolver ────────────────────────────────────────

// LiveMXResolver wraps `net.DefaultResolver.LookupMX`. Falls back to
// the implicit-MX rule (RFC 5321 §5.1 step 2): if no MX records, the
// recipient domain itself is the implicit MX host.
//
// ⚠ Every answer it returns is Secure=false, and that is not a placeholder:
// the Go stdlib resolver cannot do DNSSEC at all (the same limitation that
// put the TLSA lookup nest-side — smtp-server.md:656). A deployment
// resolving MX through this type therefore never pins DANE. Production uses
// NestMXResolver; this remains for the operator-hatch override's
// un-overridden domains only when no nest client is in hand.
type LiveMXResolver struct {
	Resolver *net.Resolver
}

func (l LiveMXResolver) LookupMX(ctx context.Context, domain string) (MXAnswer, error) {
	hosts, err := l.lookupHosts(ctx, domain)
	if err != nil {
		return MXAnswer{}, err
	}
	// Secure is false unconditionally — see the type's doc comment.
	return MXAnswer{Hosts: hosts}, nil
}

func (l LiveMXResolver) lookupHosts(ctx context.Context, domain string) ([]MXHost, error) {
	resolver := l.Resolver
	if resolver == nil {
		resolver = net.DefaultResolver
	}
	mxs, err := resolver.LookupMX(ctx, domain)
	if err != nil {
		// Implicit MX (RFC 5321 §5.1 step 2): if MX lookup itself
		// failed, try an A/AAAA against the domain — but only when the
		// resolver indicated "no MX records", not on transient errors.
		// `net.LookupMX` doesn't distinguish; surface the error to the
		// caller and let it decide (PermanentError vs TemporaryError).
		var dnsErr *net.DNSError
		if errors.As(err, &dnsErr) && dnsErr.IsNotFound {
			// Try implicit MX.
			ips, err2 := resolver.LookupIPAddr(ctx, domain)
			if err2 == nil && len(ips) > 0 {
				return []MXHost{{Hostname: domain, Pref: 0}}, nil
			}
		}
		return nil, err
	}
	if len(mxs) == 0 {
		return nil, nil
	}
	out := make([]MXHost, 0, len(mxs))
	for _, m := range mxs {
		out = append(out, MXHost{
			Hostname: strings.TrimSuffix(m.Host, "."),
			Pref:     m.Pref,
		})
	}
	sort.SliceStable(out, func(i, j int) bool { return out[i].Pref < out[j].Pref })
	return out, nil
}

// ── Static transport-override MX resolver ─────────────────────────

// OverrideMXResolver implements a static outbound transport route: a
// recipient domain present in Override resolves straight to the mapped
// SMTP target (a bare `host` ⇒ implicit port 25, or `host:port`),
// bypassing DNS; every other domain delegates to Fallback (LiveMXResolver
// in production). Constructed by main.go from the operator-hatch
// `mta_mx_override` table — see internal/config.OperatorHatch.MTAMXOverride
// for the deployment-topology rationale (split-horizon / air-gapped relay;
// test loopback stub MX). The target rides on MXHost.Hostname; the
// `host:port` form is what lets a test point `external.test` at a
// loopback stub on an ephemeral port.
type OverrideMXResolver struct {
	// Override maps recipient domain (lowercased) → `host` or `host:port`.
	Override map[string]string
	// Fallback resolves any domain not in Override. nil is treated as a
	// hard error for un-mapped domains (an override-only deployment that
	// expects every reachable domain to be listed).
	Fallback MXResolver
}

func (r OverrideMXResolver) LookupMX(ctx context.Context, domain string) (MXAnswer, error) {
	if target, ok := r.Override[strings.ToLower(domain)]; ok {
		// Secure=true, and deliberately so: an override target is a static
		// route this deployment configured, so no DNS answer — and therefore
		// no DNS-spoofing attacker — chose the name. RFC 7672 §2.2's
		// requirement is that the name DANE binds to not be attacker-
		// selectable; local configuration satisfies that at least as well as
		// a validated RRset does. Returning false here would instead disable
		// DANE for every split-horizon / air-gapped relay deployment.
		return MXAnswer{Hosts: []MXHost{{Hostname: target, Pref: 0}}, Secure: true}, nil
	}
	if r.Fallback == nil {
		return MXAnswer{}, fmt.Errorf("no mta_mx_override entry for %q and no fallback resolver", domain)
	}
	return r.Fallback.LookupMX(ctx, domain)
}

// NestMXResolver resolves MX through nest's DNSSEC-validating resolver
// (`fauna.bridges.resolve_mx`), which is the only resolver in the system
// that can report whether the RRset was validated. This is the MX leg of
// the same split that put the TLSA lookup nest-side: Go owns the outbound
// TLS handshake, nest does the DNS work that needs a Rust capability
// (smtp-server.md § Architectural rules).
type NestMXResolver struct {
	Client wsrpc.Caller
}

func (n NestMXResolver) LookupMX(ctx context.Context, domain string) (MXAnswer, error) {
	reply, err := wsrpc.ResolveMx(ctx, n.Client, domain)
	if err != nil {
		return MXAnswer{}, err
	}
	hosts := make([]MXHost, 0, len(reply.Hosts))
	for _, h := range reply.Hosts {
		hosts = append(hosts, MXHost{Hostname: h.Hostname, Pref: h.Priority})
	}
	sort.SliceStable(hosts, func(i, j int) bool { return hosts[i].Pref < hosts[j].Pref })
	return MXAnswer{Hosts: hosts, Secure: reply.Secure}, nil
}

// ── Production SMTP sender ────────────────────────────────────────

// DefaultSMTPSender uses Go's stdlib net/smtp client. Negotiates
// STARTTLS opportunistically; falls back to plaintext if the remote
// MX doesn't advertise it. STARTTLS-required + DANE/MTA-STS are the
// D.5b follow-up.
// outboundDeliveryPolicy is the per-attempt subset of OutboundPolicy the Go
// delivery I/O owner applies. The bridge owns outbound network I/O (live MX
// dial + STARTTLS + result classification), so these two knobs are bridge-side
// (the rest of OutboundPolicy is nest-side — see mtaLiveConfig.outbound):
//
//   - ipv6Enabled: when false, dial IPv4-only (network "tcp4") so a box
//     on an IPv4-only egress isn't black-holed on AAAA. Defaults true
//     (dual-stack) — both when unset and per the wire default.
//   - treat5xxAsTransient: RFC 3463 enhanced status codes (e.g. "5.7.1") to
//     demote from permanent to transient, mirroring the shared
//     fauna_mail::outbound::classifier::DefaultBouncePolicy allowlist.
type outboundDeliveryPolicy struct {
	ipv6Enabled         bool
	treat5xxAsTransient []string
}

type DefaultSMTPSender struct {
	helo string
	// policy, if non-nil, is read once per Send for the live IPv6-egress +
	// 5xx-allowlist knobs (mta.Run wires it to the shared mtaConfigHolder so a
	// config_changed hot-applies at the next attempt). nil → dual-stack dial,
	// empty allowlist (the prior hard-coded behaviour; used by test senders).
	policy func() outboundDeliveryPolicy
}

// NewDefaultSMTPSender constructs a sender that announces `helo` as
// the EHLO host. Empty helo falls back to the local hostname (which
// is usually wrong for a deliverable MX; production callers must
// supply the bridge's mail domain).
func NewDefaultSMTPSender(helo string) *DefaultSMTPSender {
	return &DefaultSMTPSender{helo: helo}
}

// Send dials port 25 on the emulator host, negotiates STARTTLS per tlsPolicy, and
// transmits one envelope. Returned errors are classified as
// PermanentError (5xx) or TemporaryError (4xx, network, TLS) so the
// worker can route them to mark_outbound_bounced vs mark_outbound_
// failed.
//
// TLS posture (RFC 7435 vs RFC 8461 §5):
//   - TLSOpportunistic: if the MX advertises STARTTLS, upgrade with
//     InsecureSkipVerify=true — opportunistic security accepts an
//     unauthenticated cert because the alternative is cleartext, so a
//     self-signed MX cert must not spuriously fail (only the STARTTLS
//     command itself failing is a TemporaryError). No STARTTLS advertised
//     → deliver in the clear.
//   - TLSRequired (MTA-STS enforce): STARTTLS is mandatory — its absence
//     is a TemporaryError — and the cert must WebPKI-verify
//     (InsecureSkipVerify=false) against the connected MX hostname; a
//     verification/handshake failure is a TemporaryError. Enforce never
//     downgrades to plaintext.
func (s *DefaultSMTPSender) Send(ctx context.Context, host string, from string, recipient string, body []byte, tlsPolicy TLSPolicy) (TLSAttemptOutcome, error) {
	// host is normally a bare MX hostname (→ port 25), but an operator
	// transport override (OverrideMXResolver) may hand us `host:port`.
	// Dial the explicit port when present; use the bare host for the
	// SMTP/TLS server name either way.
	addr, serverName := mxDialTarget(host)
	// Read the live outbound knobs once for this attempt (hot-applied via the
	// shared holder). Default: dual-stack dial, empty 5xx allowlist.
	pol := outboundDeliveryPolicy{ipv6Enabled: true}
	if s.policy != nil {
		pol = s.policy()
	}
	dialer := &net.Dialer{}
	conn, err := dialer.DialContext(ctx, dialNetwork(pol.ipv6Enabled), addr)
	if err != nil {
		// No TLS handshake reached → no TLSRPT verdict.
		return TLSAttemptOutcome{}, &TemporaryError{msg: fmt.Sprintf("dial %s: %v", addr, err)}
	}
	deadline, ok := ctx.Deadline()
	if ok {
		_ = conn.SetDeadline(deadline)
	}
	host = serverName
	c, err := smtp.NewClient(conn, host)
	if err != nil {
		_ = conn.Close()
		return TLSAttemptOutcome{}, &TemporaryError{msg: fmt.Sprintf("smtp.NewClient %s: %v", host, err)}
	}
	defer func() {
		_ = c.Close()
	}()

	if s.helo != "" {
		if err := c.Hello(s.helo); err != nil {
			// EHLO failed before any STARTTLS decision → no TLS verdict.
			return TLSAttemptOutcome{}, classify(err, pol.treat5xxAsTransient)
		}
	}
	starttlsOffered, _ := c.Extension("STARTTLS")

	// tlsOutcome is the TLSRPT verdict for this attempt, set once the
	// STARTTLS decision/handshake resolves. A later MAIL/RCPT/DATA failure
	// does not change it: a successful TLS session that then 5xx's still
	// reports its TLS outcome (RFC 8460), and an opportunistic plaintext
	// fallback still reports `starttls-not-supported`.
	var tlsOutcome TLSAttemptOutcome
	switch tlsPolicy.Mode {
	case TLSRequired:
		// MTA-STS enforce: STARTTLS is mandatory and the cert must
		// WebPKI-verify. A missing extension or a verification failure is
		// a temporary failure — never a plaintext downgrade.
		if !starttlsOffered {
			return TLSAttemptOutcome{Reportable: true, ResultType: "starttls-not-supported"},
				&TemporaryError{msg: fmt.Sprintf("mta-sts enforce: remote MX %s does not offer STARTTLS", host)}
		}
		tlsCfg := &tls.Config{
			ServerName:         host,
			MinVersion:         tls.VersionTLS12,
			InsecureSkipVerify: false,
		}
		if err := c.StartTLS(tlsCfg); err != nil {
			return TLSAttemptOutcome{Reportable: true, ResultType: classifyTLSHandshakeError(err, TLSRequired)},
				&TemporaryError{msg: fmt.Sprintf("mta-sts enforce starttls %s: %v", host, err)}
		}
		tlsOutcome = TLSAttemptOutcome{Reportable: true} // successful TLS session
	case TLSDanePinned:
		// DANE (RFC 7672): STARTTLS is mandatory and the presented chain
		// must satisfy a published DNSSEC-secure TLSA record. We bypass
		// WebPKI (InsecureSkipVerify=true) and verify via the TLSA pin in
		// VerifyPeerCertificate instead — a DANE-pinned chain need not chain
		// to a public CA. A missing STARTTLS extension or a pin mismatch is a
		// TemporaryError (no plaintext fallback; nest reschedules).
		//
		// What the pin actually buys, stated precisely because it is not the
		// same for both usages: DANE-EE (3) names the peer's own key, so a CA
		// compromise buys an attacker nothing at all. DANE-TA (2) names a
		// trust anchor, and an anchor is a PUBLIC certificate — so it
		// authenticates only because the matcher additionally requires the
		// presented leaf to chain to that anchor and to carry this host's
		// name. That is why `host` is passed down.
		if !starttlsOffered {
			return TLSAttemptOutcome{Reportable: true, ResultType: "starttls-not-supported"},
				&TemporaryError{msg: fmt.Sprintf("dane: remote MX %s does not offer STARTTLS", host)}
		}
		records := tlsPolicy.DaneRecords
		tlsCfg := &tls.Config{
			ServerName:         host,
			MinVersion:         tls.VersionTLS12,
			InsecureSkipVerify: true, // DANE replaces WebPKI; pinned below.
			VerifyPeerCertificate: func(rawCerts [][]byte, _ [][]*x509.Certificate) error {
				// verifiedChains is deliberately ignored: it is empty under
				// InsecureSkipVerify, and DANE's trust root is the TLSA
				// record rather than the WebPKI store it would be built from.
				if daneChainMatches(records, rawCerts, host) {
					return nil
				}
				return fmt.Errorf("dane: presented cert chain satisfies no published TLSA record")
			},
		}
		if err := c.StartTLS(tlsCfg); err != nil {
			return TLSAttemptOutcome{Reportable: true, ResultType: classifyTLSHandshakeError(err, TLSDanePinned)},
				&TemporaryError{msg: fmt.Sprintf("dane starttls %s: %v", host, err)}
		}
		tlsOutcome = TLSAttemptOutcome{Reportable: true} // successful TLS session
	default: // TLSOpportunistic
		if starttlsOffered {
			// Opportunistic (RFC 7435): upgrade if offered but accept any
			// cert — an unauthenticated TLS channel beats cleartext, so a
			// self-signed MX cert must not spuriously fail. Only a failed
			// STARTTLS command is a TemporaryError. No STARTTLS → plaintext.
			tlsCfg := &tls.Config{
				ServerName:         host,
				MinVersion:         tls.VersionTLS12,
				InsecureSkipVerify: true,
			}
			if err := c.StartTLS(tlsCfg); err != nil {
				return TLSAttemptOutcome{Reportable: true, ResultType: classifyTLSHandshakeError(err, TLSOpportunistic)},
					&TemporaryError{msg: fmt.Sprintf("starttls %s: %v", host, err)}
			}
			tlsOutcome = TLSAttemptOutcome{Reportable: true} // successful TLS session
		} else {
			// No STARTTLS advertised → deliver in the clear. RFC 8460 §4.3:
			// record `starttls-not-supported` regardless of the plaintext
			// fallback (matches the legacy in-nest path, which always tried
			// STARTTLS first and reported the missing extension before falling
			// back). The fallback is policy, not a separate TLS attempt.
			tlsOutcome = TLSAttemptOutcome{Reportable: true, ResultType: "starttls-not-supported"}
		}
	}
	if err := c.Mail(from); err != nil {
		return tlsOutcome, classify(err, pol.treat5xxAsTransient)
	}
	if err := c.Rcpt(recipient); err != nil {
		return tlsOutcome, classify(err, pol.treat5xxAsTransient)
	}
	w, err := c.Data()
	if err != nil {
		return tlsOutcome, classify(err, pol.treat5xxAsTransient)
	}
	if _, err := w.Write(body); err != nil {
		return tlsOutcome, &TemporaryError{msg: fmt.Sprintf("write body %s: %v", host, err)}
	}
	if err := w.Close(); err != nil {
		return tlsOutcome, classify(err, pol.treat5xxAsTransient)
	}
	if err := c.Quit(); err != nil {
		// QUIT failure post-DATA is non-fatal — the message is already
		// committed at the remote MTA. Log via a wrapped Temporary so
		// the caller has visibility, but classification doesn't drive
		// a bounce here. Treat as success.
		_ = err
	}
	return tlsOutcome, nil
}

// classify maps an SMTP/textproto error to PermanentError vs
// TemporaryError using the first-digit rule (RFC 5321 §4.2.1: 5xx is
// permanent, 4xx is temporary; anything else we conservatively call
// temporary so a single weird response doesn't generate a spurious
// NDR).
//
// treat5xxAsTransient is the admin allowlist (OutboundPolicy.
// treat_5xx_as_transient): RFC 3463 enhanced status codes that demote a 5xx
// to transient — typically a downstream MX misconfigured to return 5.7.1 for
// greylist holds. Mirrors fauna_mail::outbound::classifier::DefaultBouncePolicy
// (the enhanced status defaults to "5.0.0" when the response carries none).
func classify(err error, treat5xxAsTransient []string) error {
	msg := err.Error()
	// net/textproto.Error stringifies as "<code> <message>". Pluck the
	// code off the front and use the digit.
	if len(msg) >= 3 {
		switch msg[0] {
		case '5':
			enhanced := extractEnhancedStatus(msg)
			if enhanced == "" {
				enhanced = "5.0.0"
			}
			for _, c := range treat5xxAsTransient {
				if c == enhanced {
					return &TemporaryError{msg: msg}
				}
			}
			return &PermanentError{msg: msg}
		case '4':
			return &TemporaryError{msg: msg}
		}
	}
	return &TemporaryError{msg: msg}
}

// dialNetwork selects the dial address family from OutboundPolicy.ipv6_enabled.
// "tcp" is dual-stack (the OS picks A/AAAA, Happy Eyeballs); "tcp4" forces
// IPv4-only so a box on an IPv4-only egress isn't black-holed dialing
// AAAA targets.
func dialNetwork(ipv6Enabled bool) string {
	if ipv6Enabled {
		return "tcp"
	}
	return "tcp4"
}

// extractEnhancedStatus plucks the RFC 3463 enhanced status code (e.g.
// "5.7.1") out of a textproto error string like "550 5.7.1 message". Returns
// "" when no enhanced code is present (the bare 3-digit reply code "550" is
// not one — it has no dotted triple).
func extractEnhancedStatus(msg string) string {
	for _, tok := range strings.Fields(msg) {
		if isEnhancedStatusCode(tok) {
			return tok
		}
	}
	return ""
}

// isEnhancedStatusCode reports whether tok is an RFC 3463 enhanced status
// code: class.subject.detail, class ∈ {2,4,5}, subject/detail 1–3 digits.
func isEnhancedStatusCode(tok string) bool {
	parts := strings.Split(tok, ".")
	if len(parts) != 3 {
		return false
	}
	if parts[0] != "2" && parts[0] != "4" && parts[0] != "5" {
		return false
	}
	for _, p := range parts[1:] {
		if p == "" || len(p) > 3 {
			return false
		}
		for _, r := range p {
			if r < '0' || r > '9' {
				return false
			}
		}
	}
	return true
}
