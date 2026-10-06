// T1.4 — content-scan gate (ClamAV malware scan + rspamd content score).
//
// Sibling of the C.7 spam gate (the combined-score computation in server.go —
// mailfauna.CombinedSpamScoreMilli; the former spam_gate.go was folded away):
// a perimeter pre-classifier wired into Session.Data after the spam gate and
// before sealing, on the
// plaintext `raw` the MTA already holds. Per docs/goal/architecture/
// content-scoring.md the scorer is **pure shared Rust** (libs/fauna-mail/src/
// scan/) and the **network I/O is Go-side here** — exactly the spam-gate
// split. Go dials clamd (unix socket / TCP loopback) and POSTs to rspamd
// /checkv2; the Rust side parses the replies and decides the delivery action.
//
// Only the verdict metadata crosses to the nest (it rides the ingest call, or
// the report_rejected_scan forensic path for a reject); the scanned bytes
// never leave the bridge (content-scoring.md § The scoring-metadata bus).
//
// Fail-closed: clamd/rspamd unavailable (dial / connect / timeout / malformed
// reply) ⇒ Tempfail ⇒ the caller 451s and the sender's MTA retries. Never
// allow-without-scan (mail-content-scanning.md § Don't do these).
package mta

import (
	"bytes"
	"context"
	"fmt"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/metrics"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/scan"
)

// The scanner Config/dialers/policy-default live in internal/scan — lifted
// there when the MDA's capability drain gained a second caller
// (capability-mediated-content-processing design § 2.5). The perimeter-only
// concerns (circuit breakers, in-flight cap, delivery-action mapping) remain
// in this file.

// ── D7: scan-gate runtime guards (reply bounds, circuit breaker, in-flight
// cap). These protect the bridge against a wedged/compromised clamd or rspamd
// turning every inbound message into an unbounded read or a 30s-blocked
// goroutine. All guards
// FAIL CLOSED — a tripped guard returns Tempfail, never allow-without-scan
// (mail-content-scanning.md § Don't do these). ──

const (
	// maxInFlightScans caps concurrent content scans across the MTA process.
	// Each scan buffers the full message `raw` and holds a clamd/rspamd
	// round-trip; without a cap a flood of large DATA bodies under a slow
	// scanner exhausts memory + FDs. At capacity the gate fails closed
	// (Tempfail) so the sender's MTA retries — never queue unboundedly. Well
	// above a small VPS's steady-state inbound concurrency.
	maxInFlightScans = 64

	// scanBreakerThreshold consecutive failures open a scanner's circuit
	// breaker; scanBreakerCooldown is how long it stays open (fast-tempfailing
	// without a dial) before admitting one half-open trial. Opening the breaker
	// turns a daemon outage from "every inbound message blocks scan.ScanTimeout()
	// on a dead socket" into an instant Tempfail — the DoS amplification the
	// review flagged.
	scanBreakerThreshold = 5
	scanBreakerCooldown  = 30 * time.Second

	// maxClamdReplyBytes / maxRspamdReplyBytes bound the scanner reply readers
	// (previously unbounded io.ReadAll). clamd answers a single short status
	// line; rspamd a /checkv2 JSON object. A wedged/compromised daemon could
	// otherwise stream unbounded bytes → OOM. Generous caps that never clip a
	// legitimate reply; over-cap ⇒ a read error ⇒ Tempfail (fail-closed).
	maxClamdReplyBytes  = 4 << 10 // 4 KiB
	maxRspamdReplyBytes = 1 << 20 // 1 MiB
)

// scanBreaker is a per-scanner circuit breaker. After scanBreakerThreshold
// consecutive failures it opens for scanBreakerCooldown; while open, allow()
// returns false so the gate fast-tempfails without dialing. Once the cooldown
// elapses one half-open trial is admitted (re-arming the window so concurrent
// callers still see it open); the trial's result (record) closes or re-opens
// the breaker. All methods are nil-safe (a nil breaker always allows / no-ops).
type scanBreaker struct {
	mu          sync.Mutex
	consecutive int
	openUntil   time.Time
	// clock is injectable for tests; nil ⇒ time.Now.
	clock func() time.Time
}

func (b *scanBreaker) now() time.Time {
	if b.clock != nil {
		return b.clock()
	}
	return time.Now()
}

func (b *scanBreaker) allow() bool {
	if b == nil {
		return true
	}
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.openUntil.IsZero() {
		return true // closed
	}
	if b.now().Before(b.openUntil) {
		return false // open — fast-fail without a dial
	}
	// Half-open: admit one trial, re-arming the window so a concurrent caller
	// keeps seeing the breaker open until record() resolves this trial.
	b.openUntil = b.now().Add(scanBreakerCooldown)
	return true
}

func (b *scanBreaker) record(ok bool) {
	if b == nil {
		return
	}
	b.mu.Lock()
	defer b.mu.Unlock()
	if ok {
		b.consecutive = 0
		b.openUntil = time.Time{}
		return
	}
	b.consecutive++
	if b.consecutive >= scanBreakerThreshold {
		b.openUntil = b.now().Add(scanBreakerCooldown)
	}
}

// scanGate holds the MTA-process-wide runtime state for the content-scan gate:
// a concurrency limiter and per-scanner circuit breakers. Constructed once
// (newScanGate) on inboundBackend and shared by pointer into every session —
// distinct from the value-copied (hot-reloadable) scan.Config. A nil *scanGate
// is valid and means "ungated" (the legacy behavior); unit tests of the pure
// scan orchestration pass nil.
type scanGate struct {
	inflight chan struct{} // cap maxInFlightScans; nil ⇒ uncapped
	clamd    *scanBreaker
	rspamd   *scanBreaker
}

func newScanGate() *scanGate {
	return &scanGate{
		inflight: make(chan struct{}, maxInFlightScans),
		clamd:    &scanBreaker{},
		rspamd:   &scanBreaker{},
	}
}

// tryAcquire reserves an in-flight scan slot, returning false at capacity (the
// caller fails closed). A nil/uncapped gate always succeeds without reserving.
func (g *scanGate) tryAcquire() bool {
	if g == nil || g.inflight == nil {
		return true
	}
	select {
	case g.inflight <- struct{}{}:
		return true
	default:
		return false
	}
}

// release returns a slot reserved by a successful tryAcquire (paired: only
// called when tryAcquire reserved a real slot).
func (g *scanGate) release() {
	if g == nil || g.inflight == nil {
		return
	}
	<-g.inflight
}

func (g *scanGate) clamdAllow() bool {
	if g == nil {
		return true
	}
	return g.clamd.allow()
}

func (g *scanGate) clamdRecord(ok bool) {
	if g != nil {
		g.clamd.record(ok)
	}
}

func (g *scanGate) rspamdAllow() bool {
	if g == nil {
		return true
	}
	return g.rspamd.allow()
}

func (g *scanGate) rspamdRecord(ok bool) {
	if g != nil {
		g.rspamd.record(ok)
	}
}

// applyScanGate runs the perimeter content scan on `raw` and returns the
// ClamAV verdict, the rspamd score (nil when rspamd is disabled / not run),
// and the delivery action. The caller (Session.Data) maps the action onto the
// SMTP wire: Tempfail → 451, RejectMalware → 554 (+ forensic report),
// Junk/Tag/Deliver → continue with the verdict attached.
//
// `g` carries the process-wide runtime guards (in-flight cap + per-scanner
// circuit breakers); a nil `g` runs ungated (pure-orchestration unit tests).
//
// rspamd's score does NOT gate delivery in T1.4 (only ClamAV does); it is
// stored + header-stamped. A clamav reject/tempfail short-circuits before
// rspamd runs (the message won't be delivered anyway).
func applyScanGate(
	g *scanGate,
	ctx context.Context,
	raw []byte,
	cfg scan.Config,
	maxMessageBytes uint32,
	clientIP, from string,
	rcpts []string,
) (mailfauna.ClamavVerdict, *mailfauna.RspamdScore, mailfauna.ScanAction) {
	// The scan cap IS the product ceiling — never a separately-chosen number
	// (mail-content-scanning.md § Oversize messages, ruled 2026-08-26). The
	// caller hands in the same live `max_message_bytes` its 552 5.3.4 door
	// enforced for *this* message, so by construction nothing the perimeter
	// accepts is above the cap. EffectiveMaxRawMessageBytes is the one shared
	// rule (fauna_mail::transport_limits): it maps an absent knob to the shipped
	// product default. The nest refuses a knob above the product ceiling at the
	// write, so the gate is never handed a cap the scan sidecar was not
	// configured to cover.
	maxFilesize := int(mailfauna.EffectiveMaxRawMessageBytes(maxMessageBytes))

	// Reserve an in-flight scan slot whenever a scanner will touch the network
	// (at least one enabled). Fail closed at capacity. Both-disabled skips the
	// slot — no I/O happens, so there is nothing to bound.
	if cfg.Policy.ClamavEnabled || cfg.Policy.RspamdEnabled {
		if !g.tryAcquire() {
			return mailfauna.ClamavVerdictError{Detail: "scan concurrency limit reached"},
				nil,
				mailfauna.ScanActionTempfail{Reason: "scan concurrency limit reached"}
		}
		defer g.release()
	}

	// ── ClamAV (gates delivery) ──
	var clamav mailfauna.ClamavVerdict
	var action mailfauna.ScanAction
	switch {
	case !cfg.Policy.ClamavEnabled:
		// Disabled by admin policy ⇒ no verdict to decide on; don't gate.
		// We must NOT call DecideScanAction here: a disabled-scanner policy can
		// carry the zero-value ClamavAction, which the UniFFI enum rejects.
		// And "scanner off ⇒ deliver" is the admin's explicit opt-in (the
		// toggle warns it weakens inbound defense) — distinct from the
		// never-allow-without-scan rule, which only governs a scanner that ran.
		// The verdict says so: NotScanned, never an affirmative Clean the nest
		// would record as a scan that happened (no header, no bus row; the
		// message_scan_results row survives only for rspamd's detail record).
		clamav = mailfauna.ClamavVerdictNotScanned{}
		action = mailfauna.ScanActionDeliver{}
	case len(raw) > maxFilesize:
		// DEFENSIVE ONLY. Since the cap is derived from the same ceiling the
		// perimeter door enforces, production cannot reach this arm: a message
		// this large was already refused with 552 5.3.4 before the gate ran. It
		// survives for the mis-wired case the ruling names — a gate handed no
		// ceiling — and for tests that inject a tiny cap. The counter is that
		// shape's tripwire: any non-zero value is a bug, not a statistic
		// (mail-content-scanning.md § Oversize messages).
		metrics.MailScanningClamavOversize.Inc()
		clamav = mailfauna.ClamavVerdictBypassedOversize{}
		action = mailfauna.DecideScanAction(clamav, cfg.Policy)
	case !g.clamdAllow():
		// Breaker open ⇒ skip the dial and fail closed: the daemon is presumed
		// down, so do not block scan.ScanTimeout() on a dead socket per inbound message.
		return mailfauna.ClamavVerdictError{Detail: "clamd circuit breaker open"},
			nil,
			mailfauna.ScanActionTempfail{Reason: "clamd circuit breaker open"}
	default:
		reply, err := scan.Clamd(ctx, cfg.ClamdAddr, raw)
		g.clamdRecord(err == nil)
		if err != nil {
			// Connectivity / timeout ⇒ fail-closed tempfail (never
			// allow-without-scan). Not a stored verdict — the message
			// tempfails and is retried, nothing is ingested.
			return mailfauna.ClamavVerdictError{Detail: err.Error()},
				nil,
				mailfauna.ScanActionTempfail{
					Reason: fmt.Sprintf("clamd unavailable: %v", err),
				}
		}
		clamav = mailfauna.ClamdParseReply(reply)
		action = mailfauna.DecideScanAction(clamav, cfg.Policy)
	}

	// A reject/tempfail message is never delivered, so skip rspamd — it would
	// only add another failure point + wasted work. (The forensic row for a
	// reject carries no rspamd score in T1.4; that's acceptable per the
	// metadata-only report contract.)
	switch action.(type) {
	case mailfauna.ScanActionRejectMalware, mailfauna.ScanActionTempfail:
		return clamav, nil, action
	}

	// ── rspamd (stored + header-stamped; does not gate in T1.4) ──
	if !cfg.Policy.RspamdEnabled {
		return clamav, nil, action
	}
	if !g.rspamdAllow() {
		// Breaker open ⇒ skip the POST and fail closed, same rationale as clamd.
		return clamav, nil, mailfauna.ScanActionTempfail{Reason: "rspamd circuit breaker open"}
	}
	score, err := scan.Rspamd(ctx, cfg, raw, clientIP, from, rcpts)
	g.rspamdRecord(err == nil)
	if err != nil {
		// rspamd unavailable ⇒ fail-closed tempfail, same as clamd.
		return clamav, nil, mailfauna.ScanActionTempfail{
			Reason: fmt.Sprintf("rspamd unavailable: %v", err),
		}
	}
	return clamav, &score, action
}

// Scan header names stamped on a delivered/junked/tagged message before
// sealing (mail-content-scanning.md; not knobs).
const (
	headerScanClamav      = "X-Fauna-Scan-Clamav"
	headerScanRspamdScore = "X-Fauna-Scan-Rspamd-Score"
	headerScanRspamdRules = "X-Fauna-Scan-Rspamd-Rules"
)

// scanHeaders returns the X-Fauna-Scan-* header lines (each "Key: Value", no
// CRLF) to prepend to a delivered message, given the verdict + optional score.
// The ClamAV header is suppressed when the scanner was disabled (clamavEnabled
// false) — "no header for a disabled scanner". rspamd headers appear only when
// a score is present (rspamd ran). Pure + table-tested.
func scanHeaders(clamav mailfauna.ClamavVerdict, score *mailfauna.RspamdScore, clamavEnabled bool) []string {
	var headers []string
	if clamavEnabled {
		if v := clamavHeaderValue(clamav); v != "" {
			headers = append(headers, headerScanClamav+": "+v)
		}
	}
	if score != nil {
		headers = append(headers,
			headerScanRspamdScore+": "+formatScaledScore(score.ScaledMilli),
			headerScanRspamdRules+": "+strings.Join(score.FlaggedRules, ","),
		)
	}
	return headers
}

// clamavHeaderValue maps the verdict onto its header token (the matchable
// value users' filter rules act on). Error has no delivered message, and
// NotScanned has no verdict to stamp, so both return "" (no header).
func clamavHeaderValue(clamav mailfauna.ClamavVerdict) string {
	switch clamav.(type) {
	case mailfauna.ClamavVerdictClean:
		return "clean"
	case mailfauna.ClamavVerdictInfected:
		return "infected"
	case mailfauna.ClamavVerdictBypassedOversize:
		return "bypassed_oversize"
	default:
		return ""
	}
}

// formatScaledScore renders a scaled milli-int as a decimal for the header
// (presentation only — no float crosses the wire). 1200 → "1.2".
func formatScaledScore(scaledMilli int32) string {
	return strconv.FormatFloat(float64(scaledMilli)/1000.0, 'f', -1, 64)
}

// prependHeaders inserts the given header lines at the top of an RFC 5322
// message (before its existing header block), CRLF-terminated. Header order is
// not significant in RFC 5322, so prepending — what a Received trace does — is
// safe and avoids parsing the header/body split.
func prependHeaders(raw []byte, headers []string) []byte {
	if len(headers) == 0 {
		return raw
	}
	var buf bytes.Buffer
	for _, h := range headers {
		buf.WriteString(h)
		buf.WriteString("\r\n")
	}
	buf.Write(raw)
	return buf.Bytes()
}
