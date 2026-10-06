package wsrpc

import (
	"context"
	"errors"
	"log/slog"
	"net"
	"sync"
	"sync/atomic"
	"time"
)

// ReconnectingClient keeps a live WS-RPC connection to nest across drops —
// nest restarts, network blips, NAT timeouts, idle closes — without
// bouncing the bridge process or its protocol listeners. It owns a single
// inner connection at a time; when that connection drops it re-dials on the
// [BackoffSchedule] curve, re-runs the per-connect application step
// (whoami + fetch_config, via the configured hook), re-installs the push
// handler, and swaps the new connection in. The SMTP / IMAP / CalDAV
// listeners hold this value as their [Caller]; verdict RPCs issued while
// there is no live connection return [ErrReconnecting] (which the role
// code maps to a transient 451), and succeed again once the reconnect
// lands.
//
// This is the implementation of mail-bridge-lifecycle.md § Reconnecting —
// reconnect-exhaustion is *not* a shutdown trigger, so the loop retries
// indefinitely (30 s-max interval). The only things that stop it are an
// explicit Close (the supervisor sent SIGTERM) or the connect hook
// reporting [ErrBridgeRevoked] (the admin revoked the service user) — both
// surface via [ReconnectingClient.Done].
type ReconnectingClient struct {
	deps reconnectDeps

	mu  sync.Mutex
	cur rpcConn // the live connection; nil while a reconnect is in flight

	pushH  atomic.Pointer[PushHandler]
	logger atomic.Pointer[slog.Logger]

	ctx    context.Context
	cancel context.CancelFunc

	loopDone chan struct{} // closed when run() returns

	doneOnce sync.Once
	done     chan struct{} // closed when the loop stops permanently
	doneErr  error         // reason the loop stopped (nil = Close, else ErrBridgeRevoked)

	probing atomic.Bool // a standing probe (probeStanding) is in flight
}

// rpcConn is the minimal live-connection surface the reconnect loop drives.
// *Client satisfies it; tests fake it.
type rpcConn interface {
	Call(ctx context.Context, method string, body any, reply any) error
	SetOnPush(h PushHandler)
	SetLogger(l *slog.Logger)
	Done() <-chan struct{}
	Close() error
}

// reconnectDeps is the internal (interface-typed) dependency set the loop
// runs on, so the loop logic is unit-testable with fake conns.
type reconnectDeps struct {
	dial      func(context.Context) (rpcConn, error)
	onConnect func(context.Context, rpcConn) error
	backoff   func(int) time.Duration
	logger    *slog.Logger
}

// ReconnectConfig is the public (concrete-typed) configuration main.go
// supplies. Dial re-establishes the WS + bearer auth (a fresh [Dial]);
// OnConnect runs the per-connect application step on the new connection —
// re-`whoami` (return [ErrBridgeRevoked] if nest reports the service user
// was revoked) and re-`fetch_config`. Backoff defaults to [BackoffSchedule]
// when nil.
type ReconnectConfig struct {
	Dial      func(context.Context) (*Client, error)
	OnConnect func(context.Context, Caller) error
	Backoff   func(int) time.Duration
	Logger    *slog.Logger
}

// ErrReconnecting is returned by [ReconnectingClient.Call] while there is no
// live connection (a reconnect is in progress). It is deliberately distinct
// from [ErrClosed] (a permanent close): callers map a reconnecting gap to a
// transient 451 ("retry later"), not to a fatal teardown.
var ErrReconnecting = errors.New("wsrpc: reconnecting to nest (transient)")

// ErrBridgeRevoked is the sentinel the OnConnect hook returns when a
// reconnect's `whoami` reports the bridge service user was revoked. The
// loop stops and [ReconnectingClient.Done] fires so the orchestrator can
// shut the bridge down — a revoke is a shutdown trigger, not a reconnect
// (mail-bridge-lifecycle.md § Shutting down).
var ErrBridgeRevoked = errors.New("wsrpc: bridge service user revoked")

// NewReconnectingClient wraps an already-live initial connection (cold-boot
// ran whoami/fetch_config on it) and starts the reconnect loop. ctx
// cancellation — or [ReconnectingClient.Close] — stops the loop and closes
// the current connection.
func NewReconnectingClient(ctx context.Context, initial *Client, cfg ReconnectConfig) *ReconnectingClient {
	deps := reconnectDeps{
		dial: func(c context.Context) (rpcConn, error) {
			cl, err := cfg.Dial(c)
			if err != nil {
				return nil, err
			}
			return cl, nil
		},
		onConnect: func(c context.Context, conn rpcConn) error {
			if cfg.OnConnect == nil {
				return nil
			}
			// conn (rpcConn) satisfies the narrower Caller the hook wants.
			return cfg.OnConnect(c, conn)
		},
		backoff: cfg.Backoff,
		logger:  cfg.Logger,
	}
	return newReconnectingClient(ctx, initial, deps)
}

func newReconnectingClient(ctx context.Context, initial rpcConn, d reconnectDeps) *ReconnectingClient {
	if d.backoff == nil {
		d.backoff = BackoffSchedule
	}
	cctx, cancel := context.WithCancel(ctx)
	r := &ReconnectingClient{
		deps:     d,
		cur:      initial, // seed synchronously so an immediate Call sees it
		ctx:      cctx,
		cancel:   cancel,
		loopDone: make(chan struct{}),
		done:     make(chan struct{}),
	}
	if d.logger != nil {
		r.logger.Store(d.logger)
	}
	go r.run()
	return r
}

// Call delegates to the live connection. With no live connection (a
// reconnect in flight) it returns [ErrReconnecting]; a connection that drops
// mid-call surfaces [ErrClosed] from the inner client, which Call also
// re-maps to [ErrReconnecting] so the caller treats it as transient.
func (r *ReconnectingClient) Call(ctx context.Context, method string, body any, reply any) error {
	c := r.getCur()
	if c == nil {
		return ErrReconnecting
	}
	err := c.Call(ctx, method, body, reply)
	if err == nil {
		return nil
	}
	// Application-level (ok=false) and caller-context errors are real
	// outcomes on a working connection — surface them unchanged.
	var se *ServerError
	if errors.As(err, &se) || errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
		// Mid-connection revocation detection (mail-bridge-lifecycle.md
		// § Service-user re-keying step 4 / § Shutting down): when an admin
		// revokes this bridge, nest's central capability gate starts refusing
		// EVERY kind on the still-open connection with
		// `fauna.bridges.permission_denied` — but nothing closes the WS, so
		// the reconnect-time whoami that normally detects a revoke never
		// runs and the bridge would zombie forever, serving with a dead
		// identity. A permission_denied reply is therefore probed (once at a
		// time, off this call's hot path): whoami on the same connection
		// distinguishes an identity-level revocation (whoami is allowlisted
		// for every approved bridge, so it too being denied — or reporting
		// status=revoked — means the actor itself is gone) from a mere
		// kind-level authz miss (whoami fine → pass the error through).
		if code, ok := RpcErrorCode(err); ok && code == codeBridgePermissionDenied {
			go r.probeStanding(c)
		}
		return err
	}
	// Otherwise a transport failure means the connection dropped (or is
	// dropping): ErrClosed from the read side, a "use of closed network
	// connection" from the write side, or Done already observed. Surface a
	// transient ErrReconnecting so callers retry (451) instead of treating
	// it as a hard failure. Genuine non-transport errors (e.g. an encode
	// bug) on a still-live connection pass through.
	if errors.Is(err, ErrClosed) || errors.Is(err, net.ErrClosed) || connDropped(c) {
		return ErrReconnecting
	}
	return err
}

// codeBridgePermissionDenied is the RpcError.code nest's central capability
// gate returns for EVERY kind once an actor is unknown/revoked
// (`routes.rs` "central capability gate: unknown or revoked actor"), and
// which the bridge-kind allowlist also uses for kind-level denials — the
// ambiguity probeStanding resolves.
const codeBridgePermissionDenied = "fauna.bridges.permission_denied"

// WhoamiIndicatesRevoked classifies a Whoami outcome as "nest no longer
// recognizes this bridge's identity": an explicit status=revoked reply, or a
// fauna.bridges.permission_denied refusal. whoami is allowlisted for every
// approved bridge, so the central capability gate denying whoami itself means
// the actor is unknown/revoked (`caller_class_for_actor` resolves a
// non-Approved bridge to no class) — a revoked bridge can never receive a
// status=revoked *reply* mid-run, because the gate refuses the call before
// the handler runs. Shared by the in-band probe ([probeStanding]) and the
// orchestrator's reconnect-time whoami (its OnConnect hook) — the reconnect
// is where a revoke lands once nest's revocation teardown force-closes the
// old socket with 4401.
func WhoamiIndicatesRevoked(wh WhoamiReply, err error) bool {
	if err == nil {
		return wh.Status == StatusRevoked
	}
	code, ok := RpcErrorCode(err)
	return ok && code == codeBridgePermissionDenied
}

// probeStanding resolves a permission_denied reply into "kind-level authz
// miss" (bridge fine — return quietly) or "identity-level revocation" (the
// § Shutting down revoke trigger): one whoami on the same live connection.
// Deduped (one probe in flight), bounded by its own timeout on the loop ctx
// (the triggering call's ctx may be expiring). On a confirmed revocation it
// finishes the loop with [ErrBridgeRevoked] and cancels the loop ctx — run()
// then closes the connection and [ReconnectingClient.Done] fires, exactly as
// the reconnect-time detection path; the orchestrator shuts the bridge down
// and the supervisor restart enters the § Service-user re-keying flow.
func (r *ReconnectingClient) probeStanding(c rpcConn) {
	if !r.probing.CompareAndSwap(false, true) {
		return
	}
	defer r.probing.Store(false)
	pctx, cancel := context.WithTimeout(r.ctx, 15*time.Second)
	defer cancel()
	wh, err := Whoami(pctx, c)
	if !WhoamiIndicatesRevoked(wh, err) {
		return
	}
	r.log().Warn("wsrpc: nest refuses this bridge's identity on a live connection (service user revoked); shutting down")
	r.finish(ErrBridgeRevoked)
	r.cancel()
}

// connDropped reports whether the connection's drop signal has fired.
func connDropped(c rpcConn) bool {
	select {
	case <-c.Done():
		return true
	default:
		return false
	}
}

// SetOnPush stores the push handler and (re-)installs it on the current
// connection; the loop re-installs it on every subsequent reconnect.
func (r *ReconnectingClient) SetOnPush(h PushHandler) {
	if h == nil {
		r.pushH.Store(nil)
	} else {
		r.pushH.Store(&h)
	}
	if c := r.getCur(); c != nil {
		c.SetOnPush(h)
	}
}

// SetLogger stores the logger and applies it to the current connection;
// reconnects re-apply it to the new connection.
func (r *ReconnectingClient) SetLogger(l *slog.Logger) {
	if l != nil {
		r.logger.Store(l)
	}
	if c := r.getCur(); c != nil {
		c.SetLogger(l)
	}
}

// Done is closed when the reconnect loop stops permanently — an explicit
// Close, or a revoke ([ErrBridgeRevoked] from the connect hook). The
// orchestrator selects on it to begin graceful shutdown.
func (r *ReconnectingClient) Done() <-chan struct{} { return r.done }

// Err returns the reason the loop stopped: nil for an explicit Close,
// [ErrBridgeRevoked] for an admin revoke. Read after Done fires.
func (r *ReconnectingClient) Err() error {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.doneErr
}

// Close stops the reconnect loop and closes the current connection.
// Idempotent; blocks until the loop goroutine has exited.
func (r *ReconnectingClient) Close() error {
	r.cancel()
	<-r.loopDone
	return nil
}

func (r *ReconnectingClient) run() {
	defer close(r.loopDone)
	for {
		cur := r.getCur()
		if cur == nil {
			// Only reachable after finish() set cur=nil right before a
			// return; defensively stop rather than select on a nil channel.
			return
		}
		select {
		case <-r.ctx.Done():
			_ = cur.Close()
			r.finish(nil)
			return
		case <-cur.Done():
			// A drop — unless we're shutting down, in which case the
			// current connection's Done fired because Close closed it.
			if r.ctx.Err() != nil {
				r.finish(nil)
				return
			}
			r.setCur(nil) // Calls now return ErrReconnecting
			if !r.reconnect() {
				return // finish() already called (revoke or ctx-cancel)
			}
		}
	}
}

// reconnect re-dials on the backoff curve until a connect+onConnect
// succeeds (returns true, with the new connection swapped in) or the loop
// must stop (returns false, after calling finish). It retries dial /
// onConnect failures indefinitely — reconnect-exhaustion is not a shutdown
// trigger.
func (r *ReconnectingClient) reconnect() bool {
	attempt := 0
	for {
		select {
		case <-r.ctx.Done():
			r.finish(nil)
			return false
		case <-time.After(r.deps.backoff(attempt)):
		}
		nc, err := r.deps.dial(r.ctx)
		if err != nil {
			r.log().Warn("wsrpc reconnect: dial failed; will retry", "attempt", attempt, "err", err)
			attempt++
			continue
		}
		if err := r.deps.onConnect(r.ctx, nc); err != nil {
			_ = nc.Close()
			if errors.Is(err, ErrBridgeRevoked) {
				r.log().Warn("wsrpc reconnect: nest reports service user revoked; shutting down")
				r.finish(ErrBridgeRevoked)
				return false
			}
			r.log().Warn("wsrpc reconnect: post-dial handshake failed; will retry", "attempt", attempt, "err", err)
			attempt++
			continue
		}
		r.install(nc)
		r.setCur(nc)
		r.log().Info("wsrpc reconnect: reconnected to nest", "attempts", attempt+1)
		return true
	}
}

// install applies the stored push handler + logger to a freshly connected
// conn before it is swapped in as current.
func (r *ReconnectingClient) install(c rpcConn) {
	if hp := r.pushH.Load(); hp != nil {
		c.SetOnPush(*hp)
	}
	if l := r.logger.Load(); l != nil {
		c.SetLogger(l)
	}
}

func (r *ReconnectingClient) finish(err error) {
	r.mu.Lock()
	if r.doneErr == nil {
		r.doneErr = err
	}
	r.cur = nil
	r.mu.Unlock()
	r.doneOnce.Do(func() { close(r.done) })
}

func (r *ReconnectingClient) getCur() rpcConn {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.cur
}

func (r *ReconnectingClient) setCur(c rpcConn) {
	r.mu.Lock()
	r.cur = c
	r.mu.Unlock()
}

func (r *ReconnectingClient) log() *slog.Logger {
	if l := r.logger.Load(); l != nil {
		return l
	}
	return slog.Default()
}
