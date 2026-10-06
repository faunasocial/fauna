package wsrpc

import (
	"context"
	"log/slog"
	"time"
)

// configReloadIdleRetryDelay bounds the idle path's re-arm after a failed
// gate-opening fetch. Fixed rather than exponential-backed-
// off: the idle role has nothing else to do while waiting, a transient
// failure (timeout, busy nest, a blip that doesn't kill the WS) clears on the
// timescale of one more attempt, and a real prolonged outage is still bounded
// by the reconnect path's own re-fetch the moment the WS comes back — this
// delay only needs to survive a single blip, not replace that recovery.
const configReloadIdleRetryDelay = 3 * time.Second

// IdleUntilGatesOpen blocks an *idling* bridge role until either its process
// context is cancelled or its startup gates open, and reports which happened.
//
// # Why this exists
//
// Both roles decide their listener set ONCE, from the `fetch_config` snapshot
// they cold-boot with, and a role whose gates are all shut binds nothing and
// idles. Before this helper the idle branches simply blocked on `<-ctx.Done()`,
// which returned BEFORE the role built its `ConfigReloader` — so an idling
// bridge had no `config_changed` subscription at all, and an admin enabling the
// subsystem was invisible to it until something else restarted the process.
// That contradicted the ratified contract in
// `docs/goal/behavior/mail-bridge-lifecycle.md` § Running ("subscribe-and-
// hot-reload is mandatory ... no restart-to-pickup-config") and § Architectural
// rules ("The bridge subscribes to `config_changed` push events"): a bridge is
// supposed to be subscribed in EVERY state, not only when it happens to be
// serving.
//
// # The shape it restores
//
// This installs the same minimal reload path a serving role installs — push
// dispatcher → `ConfigReloader` → appliers, plus the reconnect re-fetch seam via
// publish — with exactly one applier: "did my gates open?" When they have, the
// helper returns true and the role returns nil, which `main.go` treats as a
// clean shutdown → **exit 0 → the supervisor rebinds** the process, which then
// cold-boots against the new snapshot and binds its listeners. That is the SAME
// exit-for-rebind mechanism a serving MDA already uses when its gating tuple
// flips (`mda.Run`'s gating watcher) — an in-process listener set cannot be
// re-bound live, so a fresh process is the simplest correct rebind. The idle
// state now plays by that one rule too, rather than being the one state from
// which no config change was ever observable.
//
// gatesOpen is the role's own startup predicate, evaluated against each freshly
// fetched snapshot; it must report whether the role would NOW bind something.
// publish may be nil (tests); when non-nil it receives the reloader so the
// reconnect path's re-fetched snapshot flows through the same applier — a gate
// that opened during a WS gap is then caught on the reconnect itself, not only
// on the next push.
//
// # A failed gate-opening fetch retries — unlike every other reloader caller
//
// reloadOnce's default (log-and-drop, keeping last-good config) is right for a
// SERVING role: the listeners have a config to keep. An idling role has no
// last-good state — nothing is bound — so a transient fetch failure on the ONE
// push an admin's enable produces used to leave the bridge idling forever,
// recoverable only by an unrelated second config change or a process restart. This wires ConfigReloader's onFetchFailure to re-arm the
// same trigger after configReloadIdleRetryDelay, so the retry drains through
// the ordinary Run loop and gets the ordinary applier — no second code path.
func IdleUntilGatesOpen(
	ctx context.Context,
	client Caller,
	publish func(*ConfigReloader),
	fetchTimeout time.Duration,
	logger *slog.Logger,
	gatesOpen func(ConfigSnapshot) bool,
) (opened bool) {
	if logger == nil {
		logger = slog.Default()
	}
	// A role with no client to re-fetch through (a unit-test Deps, a wiring
	// bug) can only do what the old code did: block until the process ends.
	// Degrade to that rather than nil-panicking on the reloader's Caller.
	if client == nil {
		<-ctx.Done()
		return false
	}

	watchCtx, cancel := context.WithCancel(ctx)
	defer cancel()

	dispatcher := NewPushDispatcher(logger.With("component", "push_dispatcher"))
	reloader := NewConfigReloader(client, fetchTimeout, logger.With("component", "config_reloader"))
	reloader.Register(func(snap ConfigSnapshot) {
		if gatesOpen(snap) {
			logger.Info("idle gates opened; exiting for supervisor rebind")
			cancel()
		}
	})
	reloader.onFetchFailure = func() {
		if watchCtx.Err() != nil {
			return // already ending; nothing left to retry for
		}
		logger.Info("idle gate-opening re-fetch failed; retrying", "retry_delay", configReloadIdleRetryDelay)
		time.AfterFunc(configReloadIdleRetryDelay, func() {
			select {
			case reloader.trigger <- struct{}{}:
			default:
				// A push (or a previous retry) already re-armed the trigger;
				// that pending reload subsumes this one.
			}
		})
	}
	dispatcher.Register(PushKindConfigChanged, reloader.Handle)
	if publish != nil {
		publish(reloader)
	}
	// Same install seam the serving path uses: the production client is a
	// *ReconnectingClient, which re-installs the stored handler on every
	// reconnect, so the subscription survives a nest blip while idling.
	if inst, ok := client.(interface{ SetOnPush(PushHandler) }); ok {
		inst.SetOnPush(dispatcher.Handle)
	}
	go reloader.Run(watchCtx)

	<-watchCtx.Done()
	// ctx.Err() is non-nil only when the PROCESS context ended (SIGTERM); a
	// gate-open cancels the derived ctx alone. Distinguishing them is what
	// lets the caller return "clean shutdown" for both while logging the
	// reason honestly.
	return ctx.Err() == nil
}
