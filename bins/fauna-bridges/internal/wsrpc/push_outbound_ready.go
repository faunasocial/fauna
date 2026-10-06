package wsrpc

import "log/slog"

// PushKindOutboundReady is the nest→bridge push kind that nudges the MTA's
// outbound worker to drain its queue promptly. Mirrors fauna-protocol's
// PUSH_KIND_BRIDGE_OUTBOUND_READY (bridge_routing.rs) and the
// smtp-server.md § Outbound delivery / mail-bridge-lifecycle.md § Wire
// shapes name. The outbound twin of the inbound `fauna.mail.received` push.
const PushKindOutboundReady = "fauna.bridges.outbound_ready"

// OutboundReadyHandler is the [PushHandler] for PushKindOutboundReady,
// installed on the MTA's PushDispatcher. On the push it calls the outbound
// worker's coalescing Trigger so a client `fauna.email.send` relays without
// waiting for the next `fetch_outbound_due` poll. Best-effort: a missed
// push just means the message drains on the next poll (the correctness
// backstop — see smtp-server.md § Outbound delivery). Only the MTA role
// registers this; the MDA has no outbound worker.
//
// The push carries no payload — it is a pure "drain now" nudge — so the
// handler ignores payload entirely. trigger is OutboundWorker.Trigger, a
// non-blocking size-1 coalescing send, so this is safe to run on the wsrpc
// reader goroutine (the PushHandler "fast and non-blocking" contract).
type OutboundReadyHandler struct {
	trigger func()
	logger  *slog.Logger
}

// NewOutboundReadyHandler wraps the outbound worker's Trigger. A nil logger
// defaults to slog.Default().
func NewOutboundReadyHandler(trigger func(), logger *slog.Logger) *OutboundReadyHandler {
	if logger == nil {
		logger = slog.Default()
	}
	return &OutboundReadyHandler{trigger: trigger, logger: logger}
}

// Handle guards the kind (so it is also safe installed directly), logs the
// nudge, and fires the coalescing trigger. Non-blocking.
func (h *OutboundReadyHandler) Handle(kind string, _ []byte, seq uint64) {
	if kind != PushKindOutboundReady {
		return
	}
	h.logger.Info("outbound_ready push received; nudging outbound worker", "seq", seq)
	if h.trigger != nil {
		h.trigger()
	}
}
