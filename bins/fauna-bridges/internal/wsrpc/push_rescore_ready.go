package wsrpc

import "log/slog"

// PushKindRescoreReady is the nest→bridge push kind that nudges the MDA /
// content-processor re-score drain worker to run promptly. Mirrors
// fauna-protocol's PUSH_KIND_BRIDGE_RESCORE_READY (bridge_routing.rs) and the
// content-scoring.md § Timing "delivery-time fast path". The re-score twin of
// the inbound `fauna.mail.received` push.
const PushKindRescoreReady = "fauna.bridges.rescore_ready"

// RescoreReadyHandler is the [PushHandler] for PushKindRescoreReady, installed
// on the MDA's PushDispatcher. On the push it calls the re-score drain's
// coalescing poke so a freshly-delivered mail's per-user obligation drains
// moments after delivery instead of waiting for the next startup /
// config_changed / 12 h trigger. Best-effort: a missed push just means the
// obligation drains on the next trigger (the correctness backstop). Only a
// process with a live capability holder (→ a drain) registers this.
//
// The push carries no payload — a pure "drain now" nudge — so the handler
// ignores payload entirely. poke is rescoreDrain.poke, a non-blocking size-1
// coalescing send, safe to run on the wsrpc reader goroutine (the PushHandler
// "fast and non-blocking" contract).
type RescoreReadyHandler struct {
	poke   func()
	logger *slog.Logger
}

// NewRescoreReadyHandler wraps the drain's poke. A nil logger defaults to
// slog.Default().
func NewRescoreReadyHandler(poke func(), logger *slog.Logger) *RescoreReadyHandler {
	if logger == nil {
		logger = slog.Default()
	}
	return &RescoreReadyHandler{poke: poke, logger: logger}
}

// Handle guards the kind (so it is safe installed directly), logs the nudge,
// and fires the coalescing poke. Non-blocking.
func (h *RescoreReadyHandler) Handle(kind string, _ []byte, seq uint64) {
	if kind != PushKindRescoreReady {
		return
	}
	h.logger.Info("rescore_ready push received; poking re-score drain", "seq", seq)
	if h.poke != nil {
		h.poke()
	}
}
