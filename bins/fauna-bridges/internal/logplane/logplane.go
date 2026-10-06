// Package logplane is the mail bridge's source-side half of the **sidecar log
// plane** — the queue, the flush loop, and the disable-on-refusal rule (a futile-retry stop), in one
// audited copy.
//
// Authority: `docs/goal/architecture/apps/observability.md` § The sidecar
// log plane. The Rust sidecars' equivalent is
// `libs/fauna-sidecar-client/src/log_plane.rs`; this package deliberately
// mirrors its semantics (bounded drop-oldest queue with a counted `dropped`,
// batch drain at the wire cap, disable on an error reply) so the two sources
// behave identically from nest's side. The catalogue of events this binary may
// report lives beside it in catalogue.go.
//
// # Why a plane at all
//
// The production container runs nine s6-supervised services but only
// `fauna-nest` installs a ring layer, so a bridge that cannot enroll or fetch
// a TLS cert is invisible on the admin Logs page — precisely what an admin
// needs in an incident. This package reports a small set of deliberate,
// admin-meaningful events over the authenticated WS the bridge already holds.
// It is emphatically **not** a tee, subscriber, or filter over the bridge's
// slog stream: that stream correctly interpolates recipient addresses and
// actor IDs for journald, and parts of it are remote-controlled (HELO strings,
// header values). Forwarding it would hand remote peers a log-injection
// surface into an admin page.
//
// # Strictly best-effort, by construction
//
// [Emit] never blocks, never fails, and never allocates without bound: the
// queue is capped at [QueueCapacity] and drops *oldest*-first, counting each
// drop so nest can surface the loss. The bridge's real work — accepting mail,
// serving IMAP — must never wait on, or fail because of, its logging.
//
// # The authoring contract this package cannot enforce
//
// An event's message must be a compile-time-constant template whose
// interpolations are limited to the bounded value classes: counts, sizes,
// durations, ports, protocol/error codes, DNS domain names, and Fauna
// service/component names. **Never** free-form remote-controlled text,
// upstream error strings passed through verbatim, mail addresses or local
// parts, actor IDs, message subjects, or secrets. Nest sanitizes as
// defense-in-depth, but it cannot un-leak an address you interpolated — the
// catalogue is where this is enforced, by review.
package logplane

import (
	"context"
	"errors"
	"log/slog"
	"math"
	"sync"
	"sync/atomic"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// Wire and admission bounds, mirroring libs/fauna-protocol/src/log_plane.rs.
// Go cannot import the Rust constants, so these are a hand-mirror in the same
// spirit as every other shape in internal/wsrpc — catalogue_test.go asserts the
// catalogue stays inside them, which is what keeps a silently-dropped event
// from being the way we find out they drifted.
const (
	// MaxEventsPerBatch mirrors fauna_protocol::log_plane::MAX_EVENTS_PER_BATCH.
	// Nest truncates a longer batch at admission, so the source splits instead.
	MaxEventsPerBatch = 128
	// MaxMessageBytes mirrors MAX_MESSAGE_BYTES. Nest truncates rather than
	// rejects, so exceeding it costs an admin the tail of a message.
	MaxMessageBytes = 512
	// MaxEventBytes mirrors MAX_EVENT_BYTES. Nest *rejects* an over-length or
	// off-charset event id — a catalogue constant cannot be malformed, so a
	// violation is a silently missing event.
	MaxEventBytes = 64
)

const (
	// QueueCapacity is the most events held before a flush. Older events are
	// dropped first — a recent failure is worth more to an admin than a stale
	// one — and counted into the reported drop total.
	QueueCapacity = 256

	// FlushThreshold is the size trigger: this many queued events flush without
	// waiting for the timer, so a burst (a failing listener retrying) reaches
	// the admin promptly instead of sitting for a full interval.
	FlushThreshold = 32

	// FlushInterval is the timer trigger — the worst-case delay between a quiet
	// bridge emitting one event and nest seeing it.
	FlushInterval = 2 * time.Second

	// flushTimeout bounds one flush RPC. The plane must never hold the loop
	// against a wedged connection; a blip drops the batch, which is already
	// counted.
	flushTimeout = 10 * time.Second

	// shutdownFlushTimeout bounds the final post-cancel flush. Shorter than
	// flushTimeout: shutdown is on the supervisor's clock, and a nest that
	// cannot answer promptly is one we are about to stop talking to anyway.
	shutdownFlushTimeout = 3 * time.Second
)

// Level is a plane event's severity. Deliberately narrower than slog's set —
// the plane carries only what an admin acts on; debug/trace detail stays in the
// bridge's own stderr, which the container log stream already captures.
type Level string

const (
	LevelError Level = "error"
	LevelWarn  Level = "warn"
	LevelInfo  Level = "info"
)

var (
	mu      sync.Mutex
	events  []wsrpc.LogEvent
	dropped uint64

	// nudge carries the size trigger from Emit to Run. Buffered depth 1 and
	// never blocking: a full channel already means "a flush is pending", which
	// is exactly what a second nudge would ask for.
	nudge = make(chan struct{}, 1)

	// disabled is set once nest answers with an error: the server refuses the
	// kind, and a same-image retry would fail identically, so reporting stops
	// for this process's life (the source silently disables rather than
	// error-looping).
	disabled atomic.Bool
)

// IsDisabled reports whether a server refusal has turned reporting off. Callers may
// check it to skip building a message; calling [Emit] while disabled is
// harmless.
func IsDisabled() bool { return disabled.Load() }

// Emit queues one catalogued event. Never blocks, never fails.
//
// Application code should call the named catalogue functions in catalogue.go
// rather than this directly — that is what keeps the full set of reportable
// events auditable in one read.
func Emit(level Level, event string, message string) {
	if IsDisabled() {
		return
	}
	mu.Lock()
	for len(events) >= QueueCapacity {
		events = events[1:]
		dropped++
	}
	events = append(events, wsrpc.LogEvent{
		TimestampMs: uint64(time.Now().UnixMilli()),
		Level:       string(level),
		Event:       event,
		Message:     message,
	})
	n := len(events)
	mu.Unlock()

	if n >= FlushThreshold {
		select {
		case nudge <- struct{}{}:
		default: // a flush is already pending; nothing to add
		}
	}
}

// QueuedLen is how many events are queued right now (tests / diagnostics).
func QueuedLen() int {
	mu.Lock()
	defer mu.Unlock()
	return len(events)
}

// Run is the flush loop: it ships queued events on the size trigger, on the
// [FlushInterval] timer, and once more after ctx is cancelled.
//
// That last flush is the load-bearing one: `shutdown_forced` is emitted after
// the role listeners have drained, so without it the one event that says "live
// mail sessions were cut" would die in the queue. It runs on a fresh context
// because ctx is, by then, already cancelled.
//
// Run blocks until ctx is done; start it in a goroutine once the bridge holds
// an authenticated caller.
func Run(ctx context.Context, c wsrpc.Caller) {
	ticker := time.NewTicker(FlushInterval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			sctx, cancel := context.WithTimeout(context.WithoutCancel(ctx), shutdownFlushTimeout)
			Flush(sctx, c)
			cancel()
			return
		case <-ticker.C:
			Flush(ctx, c)
		case <-nudge:
			Flush(ctx, c)
		}
	}
}

// Flush drains up to one batch and ships it. It never returns an error: a
// failed flush drops the batch (already counted) and leaves the bridge's real
// work untouched. An *error reply* is read as "this nest does not know the
// kind" and disables reporting for the process.
func Flush(ctx context.Context, c wsrpc.Caller) {
	if IsDisabled() {
		return
	}
	batch, drops, ok := takeBatch()
	if !ok {
		return // nothing to say ⇒ no wire traffic
	}
	cctx, cancel := context.WithTimeout(ctx, flushTimeout)
	defer cancel()
	if err := wsrpc.ReportLogEvents(cctx, c, batch, drops); err != nil {
		if isUnknownKind(err) {
			disabled.Store(true)
			// This one belongs on the bridge's own stderr by definition — it is
			// the plane announcing it cannot speak.
			slog.Warn("nest rejected report_log_events; disabling log-plane reporting", "err", err)
			return
		}
		// Transport hiccup (reconnect gap, nest restart): drop the batch's
		// EVENTS and stay enabled. Re-queueing the events would let a flapping
		// channel grow the queue without bound — that trade is deliberate.
		//
		// But the *count* survives: takeBatch already zeroed the pending
		// `dropped`, so without this fold-back a failed flush loses both the
		// events and the fact that they existed, and nest's ring shows an
		// unbroken story with a silent hole in it. The fold-back is bounded by
		// construction (one integer, no queue growth).
		//
		// Landed 2026-07-24 in BOTH halves in one change, as the previous note
		// here required — the Rust twin is
		// `fauna_sidecar_client::log_plane::fold_back_dropped`. Keep them in
		// step: two sources with different drop accounting is a worse trap than
		// one shared shortcoming.
		foldBackDropped(drops, len(batch))
	}
}

// foldBackDropped returns a failed flush's loss to the pending drop counter (see
// the transport-hiccup branch above). Saturating rather than wrapping: a wrapped
// counter would report a *small* number after a very long outage, which reads as
// healthy — the one outcome worse than an unbounded-looking one.
func foldBackDropped(reported uint64, eventCount int) {
	lost := reported
	if eventCount > 0 {
		if add := uint64(eventCount); lost > math.MaxUint64-add {
			lost = math.MaxUint64
		} else {
			lost += add
		}
	}
	if lost == 0 {
		return
	}
	mu.Lock()
	defer mu.Unlock()
	if dropped > math.MaxUint64-lost {
		dropped = math.MaxUint64
		return
	}
	dropped += lost
}

// isUnknownKind distinguishes a permanent server refusal of the plane (the
// bridge ships in the same image as its nest, so this is never version skew —
// it stops a retry loop no retry can clear) from "the connection is having a
// moment". Only the former is permanent, and getting it
// wrong in that direction would silence the bridge for the rest of its life
// over a single blip — so anything the reconnector owns is explicitly transient
// and the classification errs toward staying enabled.
func isUnknownKind(err error) bool {
	switch {
	case err == nil:
		return false
	case errors.Is(err, wsrpc.ErrReconnecting),
		errors.Is(err, wsrpc.ErrClosed),
		errors.Is(err, context.Canceled),
		errors.Is(err, context.DeadlineExceeded):
		return false
	default:
		// A server-side rejection (including ErrServerError, which wraps an
		// ok=false reply) is a permanent refusal: retrying the same image's kind would fail identically.
		return true
	}
}

// takeBatch drains up to one wire-capped batch. ok is false when there is
// nothing to report at all — note that a drop count alone still counts as
// something worth reporting, so source-side loss never goes unseen.
func takeBatch() ([]wsrpc.LogEvent, uint64, bool) {
	mu.Lock()
	defer mu.Unlock()
	if len(events) == 0 && dropped == 0 {
		return nil, 0, false
	}
	n := min(len(events), MaxEventsPerBatch)
	batch := make([]wsrpc.LogEvent, n)
	copy(batch, events[:n])
	events = events[n:]
	drops := dropped
	dropped = 0
	return batch, drops, true
}
