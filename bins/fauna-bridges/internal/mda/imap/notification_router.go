package imap

import (
	"log/slog"
	"sync"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// notificationRouter demuxes `BridgeMailboxStatePush` frames (received
// via `wsrpc.Client.OnPush` on the reader goroutine) by
// `subscription_id` to per-IDLE-call channels. One router per Backend
// — the wsrpc.Client carries a single OnPush callback slot, so any
// scheme above one router would need a chain-of-handlers shim that
// would not add value for the MDA's one-WS-per-process topology.
//
// Concurrency contract:
//
//   - `Handle` runs on the wsrpc reader goroutine. It MUST be fast and
//     non-blocking: take the lock just long enough to look up the
//     route, then non-blocking-send onto the per-subscription channel
//     (drop on full — F.2 considers the IDLE consumer the slow path
//     and chooses event loss over reader-goroutine head-of-line block
//     because losing one push triggers no semantic break for
//     RFC-2177-compliant MUAs: the next mailbox-affecting RPC re-derives
//     the latest state, and `imap-server.md` § IDLE only requires
//     "eventually consistent" emission). Per-channel buffer size is
//     sized at construction so realistic burst rates don't tip into the
//     drop path (default 64 — one per APPEND-storm).
//   - `Register` / `Unregister` are called from the per-session Idle
//     goroutine. They take the router lock to mutate `routes`.
//   - A channel returned by Register is never closed by the router
//     itself — only by the caller via Unregister. The caller may
//     receive on it for as long as its goroutine lives; after
//     Unregister, no further events arrive and pending sends are
//     impossible (the route is gone).
//
// Per imap-server.md § IDLE — push registrations are per-IMAP-session;
// Session.Close calls Unregister
// for every subscription_id it ever Registered. The Router does not
// track per-session ownership directly — that bookkeeping lives in
// the Session struct.
type notificationRouter struct {
	logger *slog.Logger

	mu     sync.Mutex
	routes map[uint64]chan wsrpc.MailboxStateEvent
}

// defaultRouterChanBuf is the per-subscription channel buffer.  Big
// enough to absorb a small burst of mail-delivery push events (the
// MDA's IDLE loop typically drains one push per Read cycle, but a
// bulk APPEND from a sister-session can produce N consecutive
// pushes back-to-back).  Drop-on-full lives in `Handle`.
const defaultRouterChanBuf = 64

// newNotificationRouter constructs a router with an empty route map.
// Logger is optional; nil falls back to slog.Default().
func newNotificationRouter(logger *slog.Logger) *notificationRouter {
	if logger == nil {
		logger = slog.Default()
	}
	return &notificationRouter{
		logger: logger,
		routes: make(map[uint64]chan wsrpc.MailboxStateEvent),
	}
}

// Register attaches a per-subscription channel and returns its receive
// end.  The same subscription_id MUST NOT be registered twice — the
// second call replaces the first registration's channel (callers
// should treat a duplicate-Register as a logic bug; we log a warning).
// Returns a receive-only channel so the caller cannot accidentally
// close it (the router never closes channels; Unregister is the only
// teardown seam).
func (r *notificationRouter) Register(subscriptionID uint64) <-chan wsrpc.MailboxStateEvent {
	ch := make(chan wsrpc.MailboxStateEvent, defaultRouterChanBuf)
	r.mu.Lock()
	if _, exists := r.routes[subscriptionID]; exists {
		r.logger.Warn("imap: notificationRouter.Register replacing existing route",
			"subscription_id", subscriptionID)
	}
	r.routes[subscriptionID] = ch
	r.mu.Unlock()
	return ch
}

// Unregister drops the route for the given subscription_id.  Idempotent
// — unregistering an unknown id is a no-op.  After Unregister returns,
// the router will discard any further pushes for this subscription
// (the next `Handle` lookup misses and drops the event).
func (r *notificationRouter) Unregister(subscriptionID uint64) {
	r.mu.Lock()
	delete(r.routes, subscriptionID)
	r.mu.Unlock()
}

// Handle is the wsrpc.PushHandler-shaped callback the Backend installs
// on Client.OnPush.  It runs on the wsrpc reader goroutine; the
// contract is "fast and non-blocking".
//
// For non-matching kinds we return immediately — other push kinds
// (config_changed, etc.) currently have no MDA consumer; future
// surfaces can compose by wrapping or chaining handlers above this
// one.  `payload` is owned by the wsrpc client and may be reused by
// the next frame; the router decodes it into typed values on the
// stack/heap before returning, so the caller's buffer reuse is safe.
func (r *notificationRouter) Handle(kind string, payload []byte, seq uint64) {
	if kind != wsrpc.BridgeMailboxStatePushKind {
		return
	}
	push, err := dagcbor.Unmarshal[wsrpc.BridgeMailboxStatePush](payload)
	if err != nil {
		r.logger.Warn("imap: notificationRouter decode failed",
			"kind", kind, "seq", seq, "err", err)
		return
	}
	r.mu.Lock()
	ch, ok := r.routes[push.SubscriptionID]
	r.mu.Unlock()
	if !ok {
		// Unknown subscription_id — typical when the IMAP session has
		// already torn down its IDLE but nest hasn't observed the WS
		// close yet, or when a stale-after-reconnect push slips in.
		// Drop silently; debug-log so an observer can confirm the
		// drop happened on demand.
		r.logger.Debug("imap: notificationRouter dropping push for unknown subscription",
			"subscription_id", push.SubscriptionID, "seq", seq)
		return
	}
	select {
	case ch <- push.Event:
	default:
		// Per-subscription channel full — the IDLE consumer is slow.
		// Drop the event; see the package-level Concurrency contract.
		r.logger.Warn("imap: notificationRouter dropping push (channel full)",
			"subscription_id", push.SubscriptionID, "seq", seq)
	}
}
