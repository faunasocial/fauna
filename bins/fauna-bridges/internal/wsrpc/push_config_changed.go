package wsrpc

import (
	"context"
	"log/slog"
	"sync"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
)

// PushKindConfigChanged is the nest→bridge push kind that nudges the
// bridge to re-fetch its config snapshot. Mirrors fauna-protocol's
// PUSH_KIND_BRIDGE_CONFIG_CHANGED (bridge_routing.rs) and the
// mail-bridge-lifecycle.md § Wire shapes name.
const PushKindConfigChanged = "fauna.bridges.config_changed"

// config_change_reason tokens, mirroring fauna-protocol's
// config_change_reason module. Advisory only — the bridge re-fetches the
// whole snapshot regardless — kept for observability + to let a future
// consumer act on a specific reason without a wire change.
const (
	configChangeReasonMailEnabled      = "mail_enabled"
	configChangeReasonLocalDomains     = "local_domains"
	configChangeReasonSpamPolicy       = "spam_policy"
	configChangeReasonAuthPolicy       = "auth_policy"
	configChangeReasonSubmissionPolicy = "submission_policy"
	configChangeReasonIMAPPolicy       = "imap_policy"
	configChangeReasonOutboundPolicy   = "outbound_policy"
)

// BridgeConfigChangedPush mirrors fauna-protocol's BridgeConfigChangedPush.
// The push carries no config — just an advisory `reason`; the bridge
// re-fetches the whole `fetch_config` snapshot. Reason is a plain string
// (not an enum) so an unknown future reason can never break the decode.
type BridgeConfigChangedPush struct {
	Reason string `cbor:"reason"`
}

// ConfigReloader consumes `fauna.bridges.config_changed` pushes and the
// reconnect re-fetch, re-fetches the whole `fetch_config` snapshot, and
// fans it to every registered applier so config changes apply at the next
// request boundary without a bridge restart (mail-bridge-lifecycle.md
// § Running / § Reconnecting / § Architectural rules — hot-reload mandatory).
//
// Why a worker goroutine, not a synchronous fetch in Handle: Handle runs
// on the wsrpc reader goroutine, and FetchConfig's reply is read by that
// same goroutine — a synchronous fetch would deadlock. Handle instead does
// a non-blocking, coalescing trigger; Run (a dedicated goroutine started by
// the role's main loop) drains it and does the fetch + apply. A burst of
// edits coalesces to as few re-fetches as the worker can keep up with
// (the push is already low-rate/debounced nest-side).
type ConfigReloader struct {
	caller  Caller
	timeout time.Duration
	logger  *slog.Logger

	// trigger is size-1 so a push that arrives while a reload is in
	// flight (or already pending) coalesces into the single pending slot.
	trigger chan struct{}

	// onFetchFailure, if set, runs after reloadOnce logs and drops a failed
	// fetch — every OTHER caller's default (nil) keeps the current
	// drop-and-keep-last-good behavior exactly, which is right for a SERVING
	// role (its listeners have a last-good config to keep). IdleUntilGatesOpen
	// is the one caller that sets it: an idling role has no last-good SERVING
	// state, so a dropped fetch there means the admin's enable is invisible
	// until an unrelated second push or a process restart.
	onFetchFailure func()

	mu       sync.RWMutex
	appliers []func(ConfigSnapshot)
}

// NewConfigReloader returns a reloader that issues fetch_config through
// caller with the given per-fetch timeout. A nil logger defaults to
// slog.Default(). Call Run in a goroutine to start the worker.
func NewConfigReloader(caller Caller, timeout time.Duration, logger *slog.Logger) *ConfigReloader {
	if logger == nil {
		logger = slog.Default()
	}
	if timeout <= 0 {
		timeout = 30 * time.Second
	}
	return &ConfigReloader{
		caller:  caller,
		timeout: timeout,
		logger:  logger,
		trigger: make(chan struct{}, 1),
	}
}

// Register adds an applier invoked with each freshly-fetched (or
// Apply'd) snapshot. Appliers run on the reloader's worker goroutine (for
// pushes) or the caller's goroutine (for Apply); each must be safe to call
// concurrently with the live listeners reading the same config (the
// listener-side holder is the atomic seam).
func (r *ConfigReloader) Register(apply func(ConfigSnapshot)) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.appliers = append(r.appliers, apply)
}

// Handle is the [PushHandler] for PushKindConfigChanged, installed on the
// dispatcher. It guards the kind (so it is also safe installed directly),
// logs the advisory reason, and coalescing-triggers a reload. Non-blocking
// — safe to run on the reader goroutine.
func (r *ConfigReloader) Handle(kind string, payload []byte, seq uint64) {
	if kind != PushKindConfigChanged {
		return
	}
	reason := "?"
	if push, err := dagcbor.Unmarshal[BridgeConfigChangedPush](payload); err == nil {
		reason = push.Reason
	}
	r.logger.Info("config_changed push received; scheduling re-fetch", "reason", reason, "seq", seq)
	select {
	case r.trigger <- struct{}{}:
	default:
		// A reload is already pending/in-flight; coalesce. The pending
		// reload re-fetches the whole snapshot, so it subsumes this edit.
	}
}

// Run drains reload triggers until ctx is cancelled, re-fetching + applying
// one snapshot per drained trigger. Start it in a goroutine from the role's
// main loop (mta.Run / mda.Run) with the process ctx.
func (r *ConfigReloader) Run(ctx context.Context) {
	for {
		select {
		case <-ctx.Done():
			return
		case <-r.trigger:
			r.reloadOnce(ctx)
		}
	}
}

// reloadOnce re-fetches the whole snapshot and fans it to the appliers. A
// fetch failure is logged and dropped — the next reconnect re-fetch (or the
// next push) recovers, and the listeners keep their last-good config. A
// caller with no last-good state to fall back on (IdleUntilGatesOpen) hooks
// onFetchFailure to re-arm the retry itself; every other caller leaves it
// nil and gets exactly the drop-and-keep-last-good behavior above.
func (r *ConfigReloader) reloadOnce(ctx context.Context) {
	fctx, cancel := context.WithTimeout(ctx, r.timeout)
	snap, err := FetchConfig(fctx, r.caller, "all")
	cancel()
	if err != nil {
		r.logger.Warn("config_changed re-fetch failed; keeping last-good config", "err", err)
		if r.onFetchFailure != nil {
			r.onFetchFailure()
		}
		return
	}
	r.logger.Info("config_changed re-fetch applied",
		"local_domains_count", len(snap.LocalDomains),
		"primary_domain", snap.PrimaryDomain,
		"imap_idle_timeout_secs", snap.IMAP.IdleTimeoutSecs,
	)
	r.Apply(snap)
}

// Apply fans an already-fetched snapshot to every registered applier. The
// reconnect path (main.go OnConnect) calls this with its re-fetched
// snapshot so push-prompted and reconnect-prompted hot-apply share one
// applier list — no second apply path.
func (r *ConfigReloader) Apply(snap ConfigSnapshot) {
	r.mu.RLock()
	appliers := make([]func(ConfigSnapshot), len(r.appliers))
	copy(appliers, r.appliers)
	r.mu.RUnlock()
	for _, a := range appliers {
		a(snap)
	}
}
