package wsrpc

import (
	"log/slog"
	"sync"
)

// PushDispatcher is a kind-routed fan-out for server→bridge push frames.
//
// A bridge has exactly one installed [PushHandler] slot per connection
// (Client.SetOnPush / ReconnectingClient.SetOnPush — the latter
// re-installs the same handler on every reconnect). When a process must
// consume more than one push kind — the MDA serves both
// `fauna.bridges.push.mailbox_state` (→ the IMAP notification router) and
// `fauna.bridges.config_changed` (→ hot-reload) — the dispatcher is that
// single installed handler, and each consumer registers its kind on it.
// This is the composition the notification_router doc anticipated ("future
// surfaces can compose by wrapping or chaining handlers above this one").
//
// Concurrency: Register runs on per-session goroutines; Handle runs on the
// wsrpc reader goroutine (one frame at a time). The RWMutex guards the
// route map against that race — mirroring the notificationRouter contract.
// A registered handler must itself be "fast and non-blocking" since it runs
// on the reader goroutine.
type PushDispatcher struct {
	logger *slog.Logger
	mu     sync.RWMutex
	routes map[string]PushHandler
}

// NewPushDispatcher returns an empty dispatcher. A nil logger defaults to
// slog.Default().
func NewPushDispatcher(logger *slog.Logger) *PushDispatcher {
	if logger == nil {
		logger = slog.Default()
	}
	return &PushDispatcher{logger: logger, routes: make(map[string]PushHandler)}
}

// Register routes frames of the given kind to h. Re-registering a kind
// replaces the prior handler (last-writer-wins); in practice each kind has
// a single owner.
func (d *PushDispatcher) Register(kind string, h PushHandler) {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.routes[kind] = h
}

// Handle is the [PushHandler] installed via SetOnPush. It routes a frame to
// the handler registered for its kind; an unregistered kind is dropped
// (debug-logged), which is the safe default since nest may emit push kinds
// a given bridge build doesn't yet consume.
func (d *PushDispatcher) Handle(kind string, payload []byte, seq uint64) {
	d.mu.RLock()
	h, ok := d.routes[kind]
	d.mu.RUnlock()
	if !ok {
		d.logger.Debug("push dispatcher: no handler for kind, dropping", "kind", kind, "seq", seq)
		return
	}
	h(kind, payload, seq)
}
