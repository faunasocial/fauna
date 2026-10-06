package wsrpc

import (
	"context"
	"errors"
	"log/slog"
	"net/http"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"nhooyr.io/websocket"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
)

// fakeConn is a deterministic rpcConn substitute for the reconnect-loop
// tests — no real WebSocket. A "server-side drop" is modelled by closing
// the done channel (mirrors *Client.readerDone closing when readLoop
// errors); Close() does the same, since the loop distinguishes an
// intentional shutdown from a drop via its own context, not the conn.
type fakeConn struct {
	id       int
	mu       sync.Mutex
	done     chan struct{}
	closedCh bool
	callErr  error
	calls    int
	pushH    PushHandler
}

func newFakeConn(id int) *fakeConn { return &fakeConn{id: id, done: make(chan struct{})} }

func (f *fakeConn) Call(_ context.Context, _ string, _ any, _ any) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	// Mirror *Client: once the connection has dropped/closed, Call returns
	// ErrClosed (the ReconnectingClient remaps that to ErrReconnecting).
	if f.closedCh {
		return ErrClosed
	}
	f.calls++
	return f.callErr
}
func (f *fakeConn) SetOnPush(h PushHandler)  { f.mu.Lock(); f.pushH = h; f.mu.Unlock() }
func (f *fakeConn) SetLogger(_ *slog.Logger) {}
func (f *fakeConn) Done() <-chan struct{}    { return f.done }
func (f *fakeConn) Close() error {
	f.mu.Lock()
	defer f.mu.Unlock()
	if !f.closedCh {
		close(f.done)
		f.closedCh = true
	}
	return nil
}
func (f *fakeConn) callCount() int           { f.mu.Lock(); defer f.mu.Unlock(); return f.calls }
func (f *fakeConn) pushHandler() PushHandler { f.mu.Lock(); defer f.mu.Unlock(); return f.pushH }

func fastBackoff(int) time.Duration { return time.Millisecond }

// serveOneThenDrop replies OK to a single request, then returns so the
// mockNest closes the connection — modelling a nest that drops the WS
// (restart / blip) after one roundtrip.
func serveOneThenDrop(t *testing.T, conn *websocket.Conn) {
	t.Helper()
	rctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	typ, data, err := conn.Read(rctx)
	if err != nil || typ != websocket.MessageBinary {
		return
	}
	frame, err := DecodeFrame(data)
	if err != nil {
		return
	}
	req, ok := frame.(*RequestFrame)
	if !ok {
		return
	}
	rep, err := EncodeReplyForTest(req.CorrelationID, map[string]string{"ok": "1"}, true)
	if err != nil {
		return
	}
	wctx, wcancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer wcancel()
	_ = conn.Write(wctx, websocket.MessageBinary, rep)
	// return → mockNest's deferred conn.Close drops the WS.
}

// Integration: the exported NewReconnectingClient, driving a *real* *Client
// (real readLoop / WS frames) against an httptest nest that drops the first
// connection. Exercises the actual Done()→re-dial→recover path the fakes
// can't — the readLoop's markClosed surfacing the drop, then a fresh Dial.
func TestReconnectingClient_IntegrationRecoversAgainstRealWS(t *testing.T) {
	pub, priv := newTestKey(t)
	var connCount atomic.Int32
	m := newMockNest(t, pub, func(t *testing.T, conn *websocket.Conn) {
		if connCount.Add(1) == 1 {
			serveOneThenDrop(t, conn) // first connection: one reply, then drop
			return
		}
		echoReplyHandler(map[string]string{"ok": "1"}, true)(t, conn) // reconnects: normal
	})

	auth := NewAuthClient(http.DefaultClient, m.srv.URL, pub, priv)
	auth.acquireToken = staticAcquire(m.issuedToken)
	dial := func(ctx context.Context) (*Client, error) {
		return Dial(ctx, ClientConfig{NestEndpoint: m.srv.URL, AuthClient: auth})
	}

	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	initial, err := dial(ctx)
	if err != nil {
		t.Fatalf("initial dial: %v", err)
	}
	r := NewReconnectingClient(ctx, initial, ReconnectConfig{
		Dial:    dial,
		Backoff: func(int) time.Duration { return 5 * time.Millisecond },
	})
	defer func() { _ = r.Close() }()

	// First Call lands on conn1 (which then drops).
	var reply map[string]string
	if err := r.Call(ctx, "fauna.bridges.whoami", struct{}{}, &reply); err != nil {
		t.Fatalf("first Call: %v", err)
	}

	// Subsequent Calls 451 transiently while the reconnect runs, then recover
	// against conn2 — proving the listeners' RPCs survive a real WS drop.
	deadline := time.After(10 * time.Second)
	for {
		err := r.Call(ctx, "fauna.bridges.whoami", struct{}{}, &reply)
		if err == nil {
			break
		}
		if !errors.Is(err, ErrReconnecting) {
			t.Fatalf("during gap want ErrReconnecting, got %v", err)
		}
		select {
		case <-deadline:
			t.Fatal("never recovered after the real WS drop")
		case <-time.After(5 * time.Millisecond):
		}
	}
	if connCount.Load() < 2 {
		t.Fatalf("expected a re-dial (>=2 connections), got %d", connCount.Load())
	}
}

// (a) initial Call delegates to the seeded conn; (b) a drop makes Call
// transiently return ErrReconnecting, the loop re-dials, and Call then
// succeeds against the new conn; (d) a push handler set before the drop
// is re-installed on the reconnected conn.
func TestReconnectingClient_ReconnectsAfterDrop(t *testing.T) {
	conn1 := newFakeConn(1)
	conn2 := newFakeConn(2)
	var dialCount atomic.Int32
	deps := reconnectDeps{
		dial: func(context.Context) (rpcConn, error) {
			dialCount.Add(1)
			return conn2, nil
		},
		onConnect: func(context.Context, rpcConn) error { return nil },
		backoff:   fastBackoff,
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	r := newReconnectingClient(ctx, conn1, deps)
	defer func() { _ = r.Close() }()

	// (a) initial Call works on conn1.
	if err := r.Call(ctx, "x", nil, nil); err != nil {
		t.Fatalf("initial Call: %v", err)
	}

	// A push handler that must survive the reconnect (d).
	h := func(string, []byte, uint64) {}
	r.SetOnPush(h)

	// (b) the server drops conn1.
	conn1.Close()

	// Call must transiently 451 (ErrReconnecting) then recover on conn2.
	deadline := time.After(2 * time.Second)
	for {
		err := r.Call(ctx, "x", nil, nil)
		if err == nil {
			break
		}
		if !errors.Is(err, ErrReconnecting) {
			t.Fatalf("during gap want ErrReconnecting, got %v", err)
		}
		select {
		case <-deadline:
			t.Fatal("never reconnected")
		case <-time.After(2 * time.Millisecond):
		}
	}
	if dialCount.Load() == 0 {
		t.Fatal("expected at least one re-dial after the drop")
	}
	if conn2.callCount() == 0 {
		t.Fatal("post-reconnect Call did not delegate to conn2")
	}
	if conn2.pushHandler() == nil {
		t.Fatal("(d) push handler was not re-installed on the reconnected conn")
	}
}

// (c) when OnConnect reports the service user was revoked, the loop stops,
// Done() fires, and Err() is ErrBridgeRevoked — the orchestrator's signal
// to shut the bridge down (a revoke is a shutdown trigger, not a reconnect).
func TestReconnectingClient_RevokedStopsAndSignalsDone(t *testing.T) {
	conn1 := newFakeConn(1)
	conn2 := newFakeConn(2)
	deps := reconnectDeps{
		dial:      func(context.Context) (rpcConn, error) { return conn2, nil },
		onConnect: func(context.Context, rpcConn) error { return ErrBridgeRevoked },
		backoff:   fastBackoff,
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	r := newReconnectingClient(ctx, conn1, deps)
	defer func() { _ = r.Close() }()

	conn1.Close() // drop → loop re-dials → onConnect says revoked

	select {
	case <-r.Done():
	case <-time.After(2 * time.Second):
		t.Fatal("Done() never fired after a revoke")
	}
	if !errors.Is(r.Err(), ErrBridgeRevoked) {
		t.Fatalf("Err() = %v, want ErrBridgeRevoked", r.Err())
	}
	// Calls after a revoke must fail fast, never hang.
	if err := r.Call(ctx, "x", nil, nil); err == nil {
		t.Fatal("Call after revoke unexpectedly succeeded")
	}
}

// (e) Close stops the loop and closes the current conn; no re-dial happens
// when the current conn's done channel closes as a *result* of Close.
func TestReconnectingClient_CloseStopsLoop(t *testing.T) {
	conn1 := newFakeConn(1)
	var redialed atomic.Bool
	deps := reconnectDeps{
		dial: func(context.Context) (rpcConn, error) {
			redialed.Store(true)
			return newFakeConn(99), nil
		},
		onConnect: func(context.Context, rpcConn) error { return nil },
		backoff:   fastBackoff,
	}
	ctx := context.Background()
	r := newReconnectingClient(ctx, conn1, deps)

	if err := r.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	// The current conn must have been closed by Close().
	select {
	case <-conn1.done:
	default:
		t.Fatal("Close did not close the current conn")
	}
	// Give any errant loop iteration a chance to (wrongly) re-dial.
	time.Sleep(20 * time.Millisecond)
	if redialed.Load() {
		t.Fatal("loop re-dialled after Close (should have stopped)")
	}
}

// methodFakeConn is an rpcConn whose Call dispatches per method — the probe
// tests need whoami to behave differently from the periodic call that
// triggered it.
type methodFakeConn struct {
	fakeConn
	call func(method string, reply any) error
}

func (f *methodFakeConn) Call(_ context.Context, method string, _ any, reply any) error {
	return f.call(method, reply)
}

// permissionDeniedServerError builds the ok=false reply nest's central
// capability gate sends a revoked actor (RpcError.code =
// fauna.bridges.permission_denied), as the Go client surfaces it.
func permissionDeniedServerError(t *testing.T) error {
	t.Helper()
	payload, err := dagcbor.Marshal(struct {
		Code string `cbor:"code"`
	}{Code: codeBridgePermissionDenied})
	if err != nil {
		t.Fatalf("marshal payload: %v", err)
	}
	return &ServerError{Payload: payload}
}

// TestReconnectingClient_RevokedMidConnectionShutsDown pins the § Service-user
// re-keying step-4 detection on a LIVE connection: after an admin revoke, nest
// refuses every kind (whoami included) with permission_denied but never closes
// the WS — the standing probe must turn that into ErrBridgeRevoked/Done so the
// orchestrator shuts down (and the supervisor restart re-keys). Pre-fix the
// bridge zombied forever.
func TestReconnectingClient_RevokedMidConnectionShutsDown(t *testing.T) {
	conn := &methodFakeConn{fakeConn: *newFakeConn(1)}
	conn.call = func(method string, _ any) error {
		// EVERY kind denied — the revoked-actor central-gate behavior.
		return permissionDeniedServerError(t)
	}
	deps := reconnectDeps{
		dial:      func(context.Context) (rpcConn, error) { t.Fatal("must not re-dial"); return nil, nil },
		onConnect: func(context.Context, rpcConn) error { return nil },
		backoff:   fastBackoff,
	}
	r := newReconnectingClient(context.Background(), conn, deps)
	defer func() { _ = r.Close() }()

	// A periodic RPC gets the denial — surfaced unchanged to the caller...
	err := r.Call(context.Background(), "fauna.bridges.fetch_outbound_due", struct{}{}, &struct{}{})
	if code, ok := RpcErrorCode(err); !ok || code != codeBridgePermissionDenied {
		t.Fatalf("periodic call should surface the permission_denied error; got %v", err)
	}
	// ...and the probe (whoami also denied) shuts the loop down as revoked.
	select {
	case <-r.Done():
	case <-time.After(5 * time.Second):
		t.Fatal("Done did not fire after an identity-level permission_denied")
	}
	if !errors.Is(r.Err(), ErrBridgeRevoked) {
		t.Fatalf("Err() = %v, want ErrBridgeRevoked", r.Err())
	}
}

// TestWhoamiIndicatesRevoked pins the shared classification rule (used by the
// in-band probe AND the orchestrator's reconnect-time whoami): revoked means
// either an explicit status=revoked reply or a permission_denied refusal — a
// revoked bridge can never receive the former mid-run (nest's capability gate
// denies it whoami before the handler runs), so the denial IS the signal. Any
// other error stays transient.
func TestWhoamiIndicatesRevoked(t *testing.T) {
	cases := []struct {
		name string
		wh   WhoamiReply
		err  error
		want bool
	}{
		{"explicit revoked reply", WhoamiReply{Status: StatusRevoked}, nil, true},
		{"approved reply", WhoamiReply{Status: StatusApproved}, nil, false},
		{"permission_denied refusal", WhoamiReply{}, permissionDeniedServerError(t), true},
		{"transient transport error", WhoamiReply{}, errors.New("dial tcp: connection refused"), false},
		{"context deadline", WhoamiReply{}, context.DeadlineExceeded, false},
	}
	for _, tc := range cases {
		if got := WhoamiIndicatesRevoked(tc.wh, tc.err); got != tc.want {
			t.Errorf("%s: WhoamiIndicatesRevoked = %v, want %v", tc.name, got, tc.want)
		}
	}
}

// TestReconnectingClient_KindLevelDenialDoesNotShutDown is the probe's
// negative control: a single kind-level authz miss (whoami still fine,
// status approved) must NOT be treated as a revocation.
func TestReconnectingClient_KindLevelDenialDoesNotShutDown(t *testing.T) {
	conn := &methodFakeConn{fakeConn: *newFakeConn(1)}
	conn.call = func(method string, reply any) error {
		if method == MethodWhoami {
			if wr, ok := reply.(*WhoamiReply); ok {
				wr.Status = StatusApproved
				wr.Role = "mta"
			}
			return nil
		}
		return permissionDeniedServerError(t)
	}
	deps := reconnectDeps{
		dial:      func(context.Context) (rpcConn, error) { return newFakeConn(2), nil },
		onConnect: func(context.Context, rpcConn) error { return nil },
		backoff:   fastBackoff,
	}
	r := newReconnectingClient(context.Background(), conn, deps)
	defer func() { _ = r.Close() }()

	_ = r.Call(context.Background(), MethodFetchTLSCertBlob, struct{}{}, &struct{}{})
	select {
	case <-r.Done():
		t.Fatal("a kind-level denial with a healthy whoami must not shut the bridge down")
	case <-time.After(200 * time.Millisecond):
	}
}
