package wsrpc

import (
	"bytes"
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/fxamacker/cbor/v2"
	"nhooyr.io/websocket"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
)

// mockNest serves the authenticated /api/v1/ws/{actor_id} endpoint against an
// httptest.Server. The WS path upgrades via nhooyr.io/websocket.Accept and runs
// a caller-supplied handler. Bearer acquisition is short-circuited in the tests
// via mockAuth (the challenge/verify ceremony is covered in auth_test.go over a
// fake Caller), so the mock no longer serves an auth endpoint.
type mockNest struct {
	srv         *httptest.Server
	issuedToken string

	// wsHandler is invoked once per accepted connection.
	wsHandler func(t *testing.T, conn *websocket.Conn)
}

func newMockNest(t *testing.T, pub []byte, wsHandler func(t *testing.T, conn *websocket.Conn)) *mockNest {
	t.Helper()
	m := &mockNest{
		issuedToken: "mock-bearer-token",
		wsHandler:   wsHandler,
	}
	mux := http.NewServeMux()
	// /api/v1/ws/<actor_id_hex>
	mux.HandleFunc("/api/v1/ws/", func(w http.ResponseWriter, r *http.Request) {
		// Verify the subprotocol header carries fauna.v1 + bearer.<token>.
		proto := r.Header.Get("Sec-WebSocket-Protocol")
		hasFauna := false
		hasBearer := false
		for _, tok := range strings.Split(proto, ",") {
			tok = strings.TrimSpace(tok)
			if tok == "fauna.v1" {
				hasFauna = true
			}
			if strings.HasPrefix(tok, "bearer.") {
				hasBearer = true
			}
		}
		if !hasFauna || !hasBearer {
			http.Error(w, "missing fauna.v1 / bearer subprotocol", http.StatusUnauthorized)
			return
		}
		conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{
			Subprotocols: []string{"fauna.v1"},
		})
		if err != nil {
			t.Logf("accept: %v", err)
			return
		}
		defer conn.Close(websocket.StatusNormalClosure, "")
		conn.SetReadLimit(16 << 20)
		if m.wsHandler != nil {
			m.wsHandler(t, conn)
		}
	})
	m.srv = httptest.NewServer(mux)
	t.Cleanup(m.srv.Close)
	return m
}

// staticAcquire returns an acquireToken seam yielding a fixed bearer with a
// far-future expiry — for transport/reconnect tests that exercise the Client
// machinery, not the challenge/verify ceremony (auth_test.go covers that over a
// fake Caller). It lets those tests' mock servers serve only the authenticated
// /api/v1/ws endpoint, not the anonymous-WS auth handshake.
func staticAcquire(token string) func(context.Context) (string, time.Time, error) {
	return func(context.Context) (string, time.Time, error) {
		return token, time.Now().Add(time.Hour), nil
	}
}

// mockAuth builds an AuthClient whose token acquisition is short-circuited to
// the mock's issuedToken. Always uses a fresh Ed25519 keypair (no real verify
// is done — the mock WS handler only checks the bearer subprotocol is present).
func (m *mockNest) mockAuth(t *testing.T) *AuthClient {
	t.Helper()
	pub, priv := newTestKey(t)
	auth := NewAuthClient(http.DefaultClient, m.srv.URL, pub, priv)
	auth.acquireToken = staticAcquire(m.issuedToken)
	return auth
}

// dialAgainst is a helper that wires together the AuthClient + Dial for
// a mock nest.
func dialAgainst(t *testing.T, ctx context.Context, m *mockNest, onPush PushHandler) *Client {
	t.Helper()
	auth := m.mockAuth(t)
	c, err := Dial(ctx, ClientConfig{
		NestEndpoint: m.srv.URL,
		AuthClient:   auth,
		OnPush:       onPush,
	})
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })
	return c
}

// echoReplyHandler reads one binary frame, decodes it as a
// RequestFrame, and echoes a synthetic Reply with `body` as the payload.
func echoReplyHandler(body any, ok bool) func(t *testing.T, conn *websocket.Conn) {
	return func(t *testing.T, conn *websocket.Conn) {
		t.Helper()
		for {
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			typ, data, err := conn.Read(ctx)
			cancel()
			if err != nil {
				return
			}
			if typ != websocket.MessageBinary {
				continue
			}
			any, err := DecodeFrame(data)
			if err != nil {
				t.Logf("server decode frame: %v", err)
				return
			}
			req, ok2 := any.(*RequestFrame)
			if !ok2 {
				continue
			}
			rep, err := EncodeReplyForTest(req.CorrelationID, body, ok)
			if err != nil {
				t.Logf("server encode reply: %v", err)
				return
			}
			writeCtx, wcancel := context.WithTimeout(context.Background(), 5*time.Second)
			if err := conn.Write(writeCtx, websocket.MessageBinary, rep); err != nil {
				wcancel()
				return
			}
			wcancel()
		}
	}
}

// TestCallRoundTrips — happy path: a single Call sends a Request and
// gets back the decoded Reply.
func TestCallRoundTrips(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	type result struct {
		Role string `cbor:"role"`
	}
	m := newMockNest(t, nil, echoReplyHandler(map[string]string{"role": "mta"}, true))
	c := dialAgainst(t, ctx, m, nil)

	var got result
	if err := c.Call(ctx, "fauna.bridges.whoami", map[string]string{}, &got); err != nil {
		t.Fatalf("Call: %v", err)
	}
	if got.Role != "mta" {
		t.Errorf("Role = %q, want mta", got.Role)
	}
}

// TestCallContextCancellation — the server stalls; we cancel; Call
// returns ctx.Err within a tight deadline.
func TestCallContextCancellation(t *testing.T) {
	t.Parallel()
	stall := func(t *testing.T, conn *websocket.Conn) {
		// Read forever (or until close); don't reply.
		for {
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			_, _, err := conn.Read(ctx)
			cancel()
			if err != nil {
				return
			}
		}
	}
	m := newMockNest(t, nil, stall)
	dialCtx, dialCancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer dialCancel()
	c := dialAgainst(t, dialCtx, m, nil)

	callCtx, callCancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer callCancel()
	start := time.Now()
	err := c.Call(callCtx, "fauna.bridges.stall", map[string]string{}, nil)
	elapsed := time.Since(start)
	if err == nil {
		t.Fatal("Call returned nil; want context.DeadlineExceeded")
	}
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Errorf("Call err = %v, want context.DeadlineExceeded", err)
	}
	if elapsed > 500*time.Millisecond {
		t.Errorf("Call elapsed %v, want under 500ms", elapsed)
	}
}

// TestCallServerErrorReturnsServerError — Reply.OK=false → *ServerError.
func TestCallServerErrorReturnsServerError(t *testing.T) {
	t.Parallel()
	m := newMockNest(t, nil, echoReplyHandler(map[string]string{"err": "boom"}, false))
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	c := dialAgainst(t, ctx, m, nil)

	err := c.Call(ctx, "fauna.bridges.error", map[string]string{}, nil)
	if err == nil {
		t.Fatal("Call returned nil; want ServerError")
	}
	var se *ServerError
	if !errors.As(err, &se) {
		t.Fatalf("err = %v, want *ServerError", err)
	}
	if !errors.Is(err, ErrServerError) {
		t.Errorf("errors.Is(err, ErrServerError) = false")
	}
	// Payload should be non-empty (the mock encodes a one-key map).
	if len(se.Payload) == 0 {
		t.Errorf("ServerError.Payload is empty")
	}
}

// TestCallAfterCloseReturnsErrClosed.
func TestCallAfterCloseReturnsErrClosed(t *testing.T) {
	t.Parallel()
	m := newMockNest(t, nil, func(t *testing.T, conn *websocket.Conn) {
		// Read once, then close to keep the test fast.
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_, _, _ = conn.Read(ctx)
	})
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	c := dialAgainst(t, ctx, m, nil)
	_ = c.Close()
	err := c.Call(ctx, "fauna.bridges.after_close", map[string]string{}, nil)
	if !errors.Is(err, ErrClosed) {
		t.Errorf("err = %v, want ErrClosed", err)
	}
}

// TestCloseSendsNormalClosure pins graceful-shutdown step 5
// (mail-bridge-lifecycle.md § Shutting down): the bridge closes the WS
// connection to nest with a clean close-frame, status code 1000
// (StatusNormalClosure). The server-side handler blocks on a Read and
// observes the status code the client's Close() sends.
func TestCloseSendsNormalClosure(t *testing.T) {
	t.Parallel()
	gotStatus := make(chan websocket.StatusCode, 1)
	m := newMockNest(t, nil, func(t *testing.T, conn *websocket.Conn) {
		// Blocks until the client closes; Read then returns a CloseError
		// carrying the status code the client sent.
		_, _, err := conn.Read(context.Background())
		gotStatus <- websocket.CloseStatus(err)
	})
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	// Dial directly (not dialAgainst, whose t.Cleanup also calls Close) so
	// the single explicit Close below is the frame the server observes.
	auth := m.mockAuth(t)
	c, err := Dial(ctx, ClientConfig{NestEndpoint: m.srv.URL, AuthClient: auth})
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	if err := c.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	select {
	case st := <-gotStatus:
		if st != websocket.StatusNormalClosure {
			t.Fatalf("close status = %d, want %d (StatusNormalClosure)", st, websocket.StatusNormalClosure)
		}
	case <-time.After(3 * time.Second):
		t.Fatal("server did not observe a close frame")
	}
}

// TestBuildWSURL pins the http(s)→ws(s) transformation.
func TestBuildWSURL(t *testing.T) {
	t.Parallel()
	cases := []struct {
		in, out string
	}{
		{"https://nest.example.com", "wss://nest.example.com/api/v1/ws/00"},
		{"http://127.0.0.1:9100", "ws://127.0.0.1:9100/api/v1/ws/00"},
		{"https://nest.example.com/", "wss://nest.example.com/api/v1/ws/00"},
	}
	for _, tc := range cases {
		if got := buildWSURL(tc.in, "00"); got != tc.out {
			t.Errorf("buildWSURL(%q) = %q, want %q", tc.in, got, tc.out)
		}
	}
}

// TestBuildAnonymousWSURL pins the anonymous (no-actor-id) endpoint URL — the
// pre-identity self-enrollment connection (DialAnonymous).
func TestBuildAnonymousWSURL(t *testing.T) {
	t.Parallel()
	cases := []struct {
		in, out string
	}{
		{"https://nest.example.com", "wss://nest.example.com/api/v1/ws"},
		{"http://127.0.0.1:9100", "ws://127.0.0.1:9100/api/v1/ws"},
		{"https://nest.example.com/", "wss://nest.example.com/api/v1/ws"},
	}
	for _, tc := range cases {
		if got := buildAnonymousWSURL(tc.in); got != tc.out {
			t.Errorf("buildAnonymousWSURL(%q) = %q, want %q", tc.in, got, tc.out)
		}
	}
}

// TestBackoffSchedule pins the exponential schedule and 30s cap.
func TestBackoffSchedule(t *testing.T) {
	t.Parallel()
	cases := []struct {
		n         int
		lo, hi    time.Duration
		wantClose time.Duration // base before jitter
	}{
		{0, time.Second, time.Second + 250*time.Millisecond, time.Second},
		{1, 2 * time.Second, 2*time.Second + 500*time.Millisecond, 2 * time.Second},
		{2, 4 * time.Second, 4*time.Second + time.Second, 4 * time.Second},
		{3, 8 * time.Second, 8*time.Second + 2*time.Second, 8 * time.Second},
		{4, 16 * time.Second, 16*time.Second + 4*time.Second, 16 * time.Second},
		// 32s > cap → 30s (+ up to 25% jitter on the cap itself).
		{5, 30 * time.Second, 30*time.Second + 8*time.Second, 30 * time.Second},
		{10, 30 * time.Second, 30*time.Second + 8*time.Second, 30 * time.Second},
		// Regression: large attempt indices must stay clamped at the cap,
		// never overflow to 0/garbage. `base * 2^n` overflows int64 for
		// n≳34 and wraps to *exactly 0* for n≥55 → `next_poll_in: 0s` → a
		// hot busy-loop (observed on a long-pending bridge: 35M poll
		// attempts in minutes, pinning a CPU). The `d < 0` guard missed the
		// zero/small-positive wraps.
		{34, 30 * time.Second, 30*time.Second + 8*time.Second, 30 * time.Second},
		{55, 30 * time.Second, 30*time.Second + 8*time.Second, 30 * time.Second},
		{62, 30 * time.Second, 30*time.Second + 8*time.Second, 30 * time.Second},
		{1000, 30 * time.Second, 30*time.Second + 8*time.Second, 30 * time.Second},
	}
	// Run a few iterations per case to catch the jitter range.
	for _, tc := range cases {
		for i := 0; i < 5; i++ {
			got := BackoffSchedule(tc.n)
			if got < tc.lo {
				t.Errorf("BackoffSchedule(%d) = %v, want >= %v", tc.n, got, tc.lo)
			}
			if got > tc.hi {
				t.Errorf("BackoffSchedule(%d) = %v, want <= %v", tc.n, got, tc.hi)
			}
		}
	}
}

// TestPushHandler — server emits an unsolicited PushFrame; the
// client's OnPush callback fires.
func TestPushHandler(t *testing.T) {
	t.Parallel()
	gotKind := make(chan string, 1)
	pushHandler := func(kind string, payload []byte, seq uint64) {
		select {
		case gotKind <- kind:
		default:
		}
	}
	push := func(t *testing.T, conn *websocket.Conn) {
		payload, _ := cbor.Marshal(map[string]string{"event": "config-changed"})
		wire, err := EncodePush(&PushFrame{
			Type: TypePush, Kind: "fauna.sync.changed", Payload: payload, Seq: 1,
		})
		if err != nil {
			t.Logf("encode push: %v", err)
			return
		}
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer cancel()
		_ = conn.Write(ctx, websocket.MessageBinary, wire)
		// Hold the connection open until the client closes (cleanup
		// happens fast — the client's t.Cleanup fires before the
		// httptest server.Close that would unblock this Read).
		_, _, _ = conn.Read(ctx)
	}
	m := newMockNest(t, nil, push)
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	_ = dialAgainst(t, ctx, m, pushHandler)
	select {
	case k := <-gotKind:
		if k != "fauna.sync.changed" {
			t.Errorf("push kind = %q, want fauna.sync.changed", k)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("push handler not invoked within 2s")
	}
}

// TestPushHandler_BridgeMailboxState — server emits a
// `fauna.bridges.push.mailbox_state` PushFrame carrying a canonical-
// CBOR `BridgeMailboxStatePush`; the OnPush callback fires with the
// expected Kind, the typed payload decodes via dagcbor, and the
// MailboxStateEvent fields demux per the Kind discriminator. This
// asserts Phase F.1 Step 9's wire-decode contract — the type the
// MDA's notification router (F.2) will consume.
func TestPushHandler_BridgeMailboxState(t *testing.T) {
	t.Parallel()
	type captured struct {
		kind    string
		payload []byte
		seq     uint64
	}
	got := make(chan captured, 1)
	pushHandler := func(kind string, payload []byte, seq uint64) {
		// Copy bytes — wsrpc may reuse the buffer.
		buf := make([]byte, len(payload))
		copy(buf, payload)
		select {
		case got <- captured{kind: kind, payload: buf, seq: seq}:
		default:
		}
	}
	servedActor := bytes.Repeat([]byte{0xaa}, 32)
	wirePush := BridgeMailboxStatePush{
		SubscriptionID: 42,
		ActorID:        servedActor,
		Mailbox:        "INBOX",
		Event: MailboxStateEvent{
			Kind:   MailboxStateEventAppend,
			Uid:    7,
			Flags:  []string{"\\Recent"},
			Modseq: 100,
		},
	}
	payloadBytes, err := dagcbor.Marshal(wirePush)
	if err != nil {
		t.Fatalf("marshal BridgeMailboxStatePush: %v", err)
	}
	emitter := func(t *testing.T, conn *websocket.Conn) {
		wire, err := EncodePush(&PushFrame{
			Type:    TypePush,
			Kind:    BridgeMailboxStatePushKind,
			Payload: payloadBytes,
			Seq:     17,
		})
		if err != nil {
			t.Logf("encode push: %v", err)
			return
		}
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer cancel()
		_ = conn.Write(ctx, websocket.MessageBinary, wire)
		// Hold the connection open until the client closes (matches
		// the TestPushHandler pattern).
		_, _, _ = conn.Read(ctx)
	}
	m := newMockNest(t, nil, emitter)
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	_ = dialAgainst(t, ctx, m, pushHandler)
	select {
	case c := <-got:
		if c.kind != BridgeMailboxStatePushKind {
			t.Errorf("push kind = %q, want %q", c.kind, BridgeMailboxStatePushKind)
		}
		if c.seq != 17 {
			t.Errorf("push seq = %d, want 17", c.seq)
		}
		decoded, err := dagcbor.Unmarshal[BridgeMailboxStatePush](c.payload)
		if err != nil {
			t.Fatalf("unmarshal BridgeMailboxStatePush: %v", err)
		}
		if decoded.SubscriptionID != 42 {
			t.Errorf("subscription_id = %d, want 42", decoded.SubscriptionID)
		}
		if !bytes.Equal(decoded.ActorID, servedActor) {
			t.Errorf("actor_id mismatch")
		}
		if decoded.Mailbox != "INBOX" {
			t.Errorf("mailbox = %q, want INBOX", decoded.Mailbox)
		}
		if decoded.Event.Kind != MailboxStateEventAppend {
			t.Errorf("event.kind = %q, want %q", decoded.Event.Kind, MailboxStateEventAppend)
		}
		if decoded.Event.Uid != 7 {
			t.Errorf("event.uid = %d, want 7", decoded.Event.Uid)
		}
		if decoded.Event.Modseq != 100 {
			t.Errorf("event.modseq = %d, want 100", decoded.Event.Modseq)
		}
		if len(decoded.Event.Flags) != 1 || decoded.Event.Flags[0] != "\\Recent" {
			t.Errorf("event.flags = %v, want [\\Recent]", decoded.Event.Flags)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("push handler not invoked within 2s")
	}
}

// TestIntegrationAgainstHttptestNest exercises the full handshake +
// one Call against the in-process mock nest, asserting the
// auth-challenge / auth-verify / WS upgrade path lines up end-to-end.
func TestIntegrationAgainstHttptestNest(t *testing.T) {
	t.Parallel()
	// A more realistic mock: the server validates the actor_id in the
	// URL matches what the client signed for, and the synthetic
	// fetch_config reply round-trips through dagcbor.
	type cfgResp struct {
		Domain string `cbor:"domain"`
		Mode   string `cbor:"mode"`
	}
	wsHandler := func(t *testing.T, conn *websocket.Conn) {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_, data, err := conn.Read(ctx)
		if err != nil {
			return
		}
		any, err := DecodeFrame(data)
		if err != nil {
			t.Logf("decode: %v", err)
			return
		}
		req, ok := any.(*RequestFrame)
		if !ok {
			return
		}
		if req.Kind != "fauna.bridges.fetch_config" {
			t.Logf("unexpected kind %q", req.Kind)
		}
		rep, err := EncodeReplyForTest(req.CorrelationID, cfgResp{Domain: "example.com", Mode: "mta"}, true)
		if err != nil {
			t.Logf("encode reply: %v", err)
			return
		}
		_ = conn.Write(ctx, websocket.MessageBinary, rep)
	}
	m := newMockNest(t, nil, wsHandler)

	// Confirm the URL the test server hands back is HTTP — buildWSURL
	// turns it into ws://.
	parsed, _ := url.Parse(m.srv.URL)
	if parsed.Scheme != "http" {
		t.Fatalf("test server URL = %s, want http", m.srv.URL)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	c := dialAgainst(t, ctx, m, nil)

	var got cfgResp
	if err := c.Call(ctx, "fauna.bridges.fetch_config", map[string]string{"scope": "mta"}, &got); err != nil {
		t.Fatalf("Call fetch_config: %v", err)
	}
	if got.Domain != "example.com" || got.Mode != "mta" {
		t.Errorf("got = %+v, want {Domain:example.com Mode:mta}", got)
	}
}

// TestMultipleConcurrentCalls — multiple Call()s on the same Client
// can be in flight concurrently and each gets its own correct reply.
func TestMultipleConcurrentCalls(t *testing.T) {
	t.Parallel()
	type reply struct {
		Echo uint64 `cbor:"echo"`
	}
	// Server echoes back the request's correlation_id in the payload
	// so each caller can verify it got its own reply.
	wsHandler := func(t *testing.T, conn *websocket.Conn) {
		for {
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			_, data, err := conn.Read(ctx)
			cancel()
			if err != nil {
				return
			}
			any, err := DecodeFrame(data)
			if err != nil {
				return
			}
			req, ok := any.(*RequestFrame)
			if !ok {
				continue
			}
			rep, err := EncodeReplyForTest(req.CorrelationID, reply{Echo: req.CorrelationID}, true)
			if err != nil {
				return
			}
			wctx, wcancel := context.WithTimeout(context.Background(), 5*time.Second)
			_ = conn.Write(wctx, websocket.MessageBinary, rep)
			wcancel()
		}
	}
	m := newMockNest(t, nil, wsHandler)
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	c := dialAgainst(t, ctx, m, nil)

	const N = 20
	var wg sync.WaitGroup
	errs := make(chan error, N)
	for i := 0; i < N; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			var got reply
			if err := c.Call(ctx, "fauna.bridges.echo", map[string]uint64{}, &got); err != nil {
				errs <- err
				return
			}
			if got.Echo == 0 {
				errs <- errors.New("zero echo")
			}
		}()
	}
	wg.Wait()
	close(errs)
	for err := range errs {
		t.Errorf("concurrent Call: %v", err)
	}
}
