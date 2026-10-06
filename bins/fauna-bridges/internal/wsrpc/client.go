// client.go — single-connection WS-RPC client over nhooyr.io/websocket.
//
// Dial flow:
//
//  1. AuthClient.Token(ctx) — HTTP challenge/verify to get a bearer.
//  2. websocket.Dial(ctx, "wss://<nest>/api/v1/ws/<actor_id_hex>", &DialOptions{
//     Subprotocols: []string{"fauna.v1", "bearer." + token},
//     }) — nhooyr serialises Subprotocols as a comma-joined
//     `Sec-WebSocket-Protocol` header, matching what
//     libs/fauna-client/src/ws_adapter.rs:79 produces and what
//     bins/fauna-nest/src/routes.rs:696 (`parse_subprotocol`) parses.
//  3. Spawn a single reader goroutine that decodes incoming frames and
//     dispatches Replies to pending Call() callers by correlation_id.
//
// Call flow:
//
//  1. Allocate a fresh u64 correlation_id (per-connection ascending)
//     and a random 16-byte idempotency_key (crypto/rand).
//  2. Marshal the body to canonical DAG-CBOR (via internal/dagcbor).
//  3. Send the RequestFrame as a Binary WS frame.
//  4. Wait on a per-call result channel; on ctx cancellation send a
//     CancelFrame, return ctx.Err().
//  5. On Reply: dagcbor.Unmarshal the payload into the caller's reply
//     and observe the wsrpc_call_seconds histogram + wsrpc_calls_total
//     counter with result="ok" / "error" / "timeout".
//
// Reconnect-with-backoff: a single Client owns one WS connection and does
// NOT re-dial itself. In-process reconnect across nest blips is the job of
// [ReconnectingClient] (reconnect.go), which wraps a Client + the
// [BackoffSchedule] curve and is what main.go hands the listeners — per
// mail-bridge-lifecycle.md § Reconnecting. The process supervisor
// (s6/systemd) is the fallback for crash / fatal-error exits, not for
// transient WS drops (those are handled in-process now).
package wsrpc

import (
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"log/slog"
	mathrand "math/rand/v2"
	"net/http"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/fxamacker/cbor/v2"
	"nhooyr.io/websocket"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/metrics"
)

// PushHandler is the caller-supplied callback invoked when an
// unsolicited push frame arrives. The handler runs on the client's
// reader goroutine — keep it fast (queue + return); the next frame is
// blocked until the handler returns.
type PushHandler func(kind string, payload []byte, seq uint64)

// ClientConfig parameterises Dial.
type ClientConfig struct {
	// NestEndpoint is the nest's external base URL (https://nest.example).
	// The bridge appends "/api/v1/ws/<actor_id_hex>" internally — passing
	// a URL that already ends with that path is rejected.
	NestEndpoint string
	// AuthClient supplies the bearer token used in the subprotocol
	// handshake. Re-dials acquire a fresh token via this client.
	AuthClient *AuthClient
	// Logger is the per-client logger; defaults to slog.Default().
	Logger *slog.Logger
	// HTTPClient is forwarded to websocket.DialOptions.HTTPClient. Nil
	// uses http.DefaultClient with no timeout, which is fine for tests
	// against a local httptest server — production passes a client
	// with a TLS config + sensible timeouts.
	HTTPClient *http.Client
	// OnPush, if non-nil, is invoked synchronously on the reader
	// goroutine for every PushFrame. Leave nil to drop pushes.
	OnPush PushHandler
}

// Client is a single open WS-RPC connection; it does not re-dial. Spanning
// reconnects across nest restarts / blips is [ReconnectingClient]'s job
// (reconnect.go) — it wraps a Client and re-dials on the [BackoffSchedule]
// curve, keeping the bridge's listeners up (mail-bridge-lifecycle.md
// § Reconnecting). [Client.Done] is the drop signal it watches.
type Client struct {
	cfg  ClientConfig
	conn *websocket.Conn
	// logger is swapped after the resolved role is known (see SetLogger).
	// Atomic so the reader goroutine's reads don't race with main.go's
	// post-Whoami swap.
	logger atomic.Pointer[slog.Logger]
	// onPush is the active push handler. Initially seeded from
	// `cfg.OnPush` at Dial-time; main.go (or any post-Dial wiring,
	// e.g. mda.Run installing the notification router) swaps it via
	// SetOnPush so the reader goroutine's dispatch picks up the new
	// handler atomically. nil → pushes drop silently.
	onPush atomic.Pointer[PushHandler]

	// Per-connection ascending correlation_id.
	nextCorr atomic.Uint64

	// Pending Call() callers, keyed by correlation_id.
	mu      sync.Mutex
	pending map[uint64]chan *ReplyFrame

	// closed is set when Close is called or the reader goroutine
	// terminates; further Call()s return ErrClosed.
	closed atomic.Bool
	// closeErr captures the reason the reader goroutine terminated
	// (nil = clean close). Read after closed.Load() observes true.
	closeErrMu sync.Mutex
	closeErr   error

	// readerDone is closed when the reader goroutine exits.
	readerDone chan struct{}

	// writeMu serialises websocket.Conn.Write calls — nhooyr's Conn
	// doesn't allow concurrent writes per its docs.
	writeMu sync.Mutex
}

// Caller is the minimal surface the typed method wrappers (methods.go)
// consume. *Client satisfies it; tests substitute a fake Caller so
// wrappers can be exercised without spinning up a WS server. Keeping
// the surface narrow keeps the test substitution trivial and makes the
// wrapper contracts ("encode this body, decode this reply") the
// load-bearing thing rather than the transport.
type Caller interface {
	Call(ctx context.Context, method string, body any, reply any) error
}

// ErrClosed is returned by Call when the connection has been closed
// (either via Close() or by the server tearing the WS down).
var ErrClosed = errors.New("wsrpc: client closed")

// ErrServerError is wrapped by Call when the Reply.OK field is false.
// The wrapped error carries the raw CBOR payload bytes (the typed
// RpcError shape is Phase B.5's responsibility).
var ErrServerError = errors.New("wsrpc: server returned ok=false")

// ServerError carries an OK=false reply's raw payload so callers can
// decode it into the typed RpcError shape if they want to.
type ServerError struct {
	Payload []byte
}

func (e *ServerError) Error() string {
	// Self-describing on a best-effort basis: an opaque "(payload N bytes)"
	// forces whoever reads the log to re-run the failure with instrumentation
	// just to learn the nest's stated reason (it cost a tier_3 round on
	// 2026-07-29) — failures must diagnose themselves. The typed decode is
	// what RpcErrorDetail does; falling back to the byte count only when the
	// payload is not an RpcError at all.
	var body struct {
		Code    string `cbor:"code"`
		Details string `cbor:"details"`
	}
	if cbor.Unmarshal(e.Payload, &body) == nil && body.Code != "" {
		if body.Details != "" {
			return fmt.Sprintf("wsrpc: server returned ok=false (code=%s: %s)", body.Code, body.Details)
		}
		return fmt.Sprintf("wsrpc: server returned ok=false (code=%s)", body.Code)
	}
	return fmt.Sprintf("wsrpc: server returned ok=false (payload %d bytes)", len(e.Payload))
}

// Unwrap satisfies errors.Is(..., ErrServerError).
func (e *ServerError) Unwrap() error { return ErrServerError }

// CodeOverQuota is the `RpcError.code` the nest mailbox-write handlers
// (append/copy/move/ingest_inbound_mail) return when a write would exceed
// the actor's RFC 9208 quota root. The IMAP MDA maps it to a
// `NO [OVERQUOTA]` status response; the SMTP MTA maps it to `552 5.2.2
// Mailbox full`. See `docs/goal/behavior/imap-server.md` § Quota
// enforcement points.
const CodeOverQuota = "fauna.bridges.over_quota"

// CodeHeldForReview is the `RpcError.code` the nest move handler returns
// when a message in the guardian held mailbox still has a live hold — a
// supervised account's mail app must not relocate it; the guardian's
// approve/deny is the only way out (`docs/goal/behavior/family-safety.md`
// § The mail gate). The IMAP MDA maps it to a tagged `NO`.
const CodeHeldForReview = "fauna.bridges.held_for_review"

// CodeMessageTooLarge is the `RpcError.code` (shared
// `fauna_protocol::email::MESSAGE_TOO_LARGE_CODE`) the nest returns when a
// message exceeds the product ceiling `max_message_bytes` — `fauna.email.send`
// and `import_message` on their own paths, and `append` since ceiling
// retirement (IMAP APPEND has no SMTP perimeter clamp, so nest is its
// authoritative size gate). The IMAP MDA maps it to an IMAP `BAD`: a permanent
// size failure the MUA must not retry (`docs/goal/behavior/smtp-server.md`
// § Message size limits).
const CodeMessageTooLarge = "fauna.email.too_large"

// RpcErrorCode decodes the typed RpcError carried by an `ok=false` reply
// (a *ServerError) and returns its `code`. ok is false when err is not a
// ServerError, or its payload does not decode as an RpcError. This is the
// minimal "decode it into the typed RpcError shape" the ServerError doc
// refers to — callers branch on the returned code (e.g. CodeOverQuota).
func RpcErrorCode(err error) (code string, ok bool) {
	var se *ServerError
	if !errors.As(err, &se) {
		return "", false
	}
	var body struct {
		Code string `cbor:"code"`
	}
	if cbor.Unmarshal(se.Payload, &body) != nil {
		return "", false
	}
	return body.Code, true
}

// RpcErrorDetail decodes the typed RpcError (code + free-form details
// string) carried by an `ok=false` reply (a *ServerError). ok is false
// when err is not a ServerError or its payload does not decode as an
// RpcError. The `message` field is a LocalizedText (i18n key + args); the
// human-readable cause lives in `details` (the nest's `malformed`/`internal`
// helpers stash the Rust error Display there), so that's what callers log to
// turn an opaque "ok=false (payload N bytes)" into the real rejection reason.
func RpcErrorDetail(err error) (code, details string, ok bool) {
	var se *ServerError
	if !errors.As(err, &se) {
		return "", "", false
	}
	var body struct {
		Code    string `cbor:"code"`
		Details string `cbor:"details"`
	}
	if cbor.Unmarshal(se.Payload, &body) != nil {
		return "", "", false
	}
	return body.Code, body.Details, true
}

// Dial opens a WS-RPC connection to the nest.
//
// On success the returned Client owns the WebSocket and a background
// reader goroutine. Close() shuts both down.
func Dial(ctx context.Context, cfg ClientConfig) (*Client, error) {
	if cfg.AuthClient == nil {
		return nil, errors.New("wsrpc: ClientConfig.AuthClient is required")
	}
	if cfg.NestEndpoint == "" {
		return nil, errors.New("wsrpc: ClientConfig.NestEndpoint is required")
	}
	if cfg.Logger == nil {
		cfg.Logger = slog.Default()
	}

	token, err := cfg.AuthClient.Token(ctx)
	if err != nil {
		return nil, fmt.Errorf("wsrpc dial: acquire bearer: %w", err)
	}
	wsURL := buildWSURL(cfg.NestEndpoint, cfg.AuthClient.ActorIDHex())

	httpClient := cfg.HTTPClient
	if httpClient == nil {
		httpClient = http.DefaultClient
	}
	dial := func(bearer string) (*websocket.Conn, *http.Response, error) {
		return websocket.Dial(ctx, wsURL, &websocket.DialOptions{
			HTTPClient: httpClient,
			// nhooyr serialises Subprotocols as a comma-joined header.
			// Nest's parse_subprotocol splits on ',' and trims whitespace
			// so "fauna.v1,bearer.<token>" is accepted.
			Subprotocols: []string{"fauna.v1", "bearer." + bearer},
		})
	}

	conn, resp, err := dial(token)
	if err != nil && resp != nil && resp.StatusCode == http.StatusUnauthorized {
		// The nest refused the cached bearer at the upgrade: a restarted nest
		// has forgotten it (see the AuthClient note). Re-mint once and retry
		// once; a fresh bearer still refused goes to the caller's backoff.
		cfg.AuthClient.Invalidate()
		fresh, merr := cfg.AuthClient.Token(ctx)
		if merr != nil {
			return nil, fmt.Errorf("wsrpc dial: re-mint after an upgrade 401: %w", merr)
		}
		conn, _, err = dial(fresh)
	}
	if err != nil {
		return nil, fmt.Errorf("wsrpc dial: ws connect %s: %w", wsURL, err)
	}
	return newClientFromConn(cfg, conn), nil
}

// AnonymousClientConfig parameterises [DialAnonymous] — the token-less
// pre-identity WS used for bridge self-enrollment (fauna.bridges.request_enrollment)
// before the bridge has an enrollment row and can authenticate. Mirrors
// [ClientConfig] minus AuthClient/OnPush (the anonymous connection binds no
// actor and receives no Push frames).
type AnonymousClientConfig struct {
	// NestEndpoint is the nest's base URL (e.g. http://127.0.0.1:<port>);
	// DialAnonymous appends /api/v1/ws (no actor_id) itself.
	NestEndpoint string
	// Logger is the per-client logger; defaults to slog.Default().
	Logger *slog.Logger
	// HTTPClient is forwarded to websocket.DialOptions.HTTPClient. Nil uses
	// http.DefaultClient.
	HTTPClient *http.Client
}

// DialAnonymous opens the anonymous (pre-identity) WS-RPC connection to nest's
// GET /api/v1/ws endpoint — no bearer token, no /<actor_id> path segment. The
// connection binds no actor and the dispatcher routes only the pre-identity
// allowlist kinds (here: fauna.bridges.request_enrollment). Used at cold boot
// for zero-touch self-enrollment, before the bridge has an enrollment row and
// can run the authenticated challenge/verify flow ([Dial]).
//
// On success the returned Client owns the WebSocket and a reader goroutine;
// Close() shuts both down.
func DialAnonymous(ctx context.Context, cfg AnonymousClientConfig) (*Client, error) {
	if cfg.NestEndpoint == "" {
		return nil, errors.New("wsrpc: AnonymousClientConfig.NestEndpoint is required")
	}
	if cfg.Logger == nil {
		cfg.Logger = slog.Default()
	}
	wsURL := buildAnonymousWSURL(cfg.NestEndpoint)
	httpClient := cfg.HTTPClient
	if httpClient == nil {
		httpClient = http.DefaultClient
	}
	conn, _, err := websocket.Dial(ctx, wsURL, &websocket.DialOptions{
		HTTPClient: httpClient,
		// Anonymous endpoint: offer only "fauna.v1" — no "bearer.<token>"
		// element. Nest's subprotocol_offers_fauna_v1 (routes.rs) accepts it.
		Subprotocols: []string{"fauna.v1"},
	})
	if err != nil {
		return nil, fmt.Errorf("wsrpc dial anonymous: ws connect %s: %w", wsURL, err)
	}
	return newClientFromConn(ClientConfig{NestEndpoint: cfg.NestEndpoint, Logger: cfg.Logger}, conn), nil
}

// newClientFromConn wraps an already-dialed WS connection in a Client and
// starts its reader goroutine. Shared by [Dial] (authed) and [DialAnonymous]
// (pre-identity) so the per-connection bookkeeping has one home.
func newClientFromConn(cfg ClientConfig, conn *websocket.Conn) *Client {
	// Disable nhooyr's per-message read limit (default 32 KiB). The WS-RPC
	// payloads include wrapped TLS certs and mail bodies that can be several
	// MiB. 16 MiB is a generous upper bound that still protects against
	// accidental unbounded growth.
	conn.SetReadLimit(16 << 20)
	c := &Client{
		cfg:        cfg,
		conn:       conn,
		pending:    make(map[uint64]chan *ReplyFrame),
		readerDone: make(chan struct{}),
	}
	c.logger.Store(cfg.Logger)
	if cfg.OnPush != nil {
		h := cfg.OnPush
		c.onPush.Store(&h)
	}
	go c.readLoop()
	return c
}

// SetOnPush atomically swaps the push-frame handler. Used by mda.Run
// to install the IDLE/NOTIFY notification router after Dial — the
// router lives on the Backend, which is constructed only after the
// Client exists (and after Whoami / FetchConfig). Pass nil to drop
// pushes silently.
//
// Safe to call concurrently with the reader goroutine — the handler
// pointer is atomic.
func (c *Client) SetOnPush(h PushHandler) {
	if h == nil {
		c.onPush.Store(nil)
		return
	}
	c.onPush.Store(&h)
}

// Done returns a channel closed when this connection's reader goroutine
// exits — i.e. the WS dropped (read error) or Close was called. The
// [ReconnectingClient] watches it to trigger an in-process reconnect
// (mail-bridge-lifecycle.md § Reconnecting).
func (c *Client) Done() <-chan struct{} { return c.readerDone }

// SetLogger swaps the client's logger. Used by main.go to install a
// role-attributed logger after Whoami resolves the bridge's role, so
// subsequent WS-RPC log records carry the resolved role rather than
// the "unresolved" placeholder the Dial-time logger had.
//
// Safe to call concurrently with the reader goroutine — the logger
// pointer is atomic.
func (c *Client) SetLogger(l *slog.Logger) {
	if l == nil {
		l = slog.Default()
	}
	c.logger.Store(l)
}

// log returns the current logger, used internally by the reader
// goroutine and by Call().
func (c *Client) log() *slog.Logger {
	if l := c.logger.Load(); l != nil {
		return l
	}
	return slog.Default()
}

// buildWSURL maps an http(s):// base URL to its ws(s):// counterpart and
// appends /api/v1/ws/<actor_id_hex>. Mirrors
// libs/fauna-client/src/ws_adapter.rs::build_ws_url.
func buildWSURL(nestBaseURL, actorIDHex string) string {
	base := strings.TrimRight(nestBaseURL, "/")
	switch {
	case strings.HasPrefix(base, "https://"):
		base = "wss://" + base[len("https://"):]
	case strings.HasPrefix(base, "http://"):
		base = "ws://" + base[len("http://"):]
	}
	return base + "/api/v1/ws/" + actorIDHex
}

// buildAnonymousWSURL maps an http(s):// base URL to its ws(s):// counterpart
// and appends /api/v1/ws — the anonymous (pre-identity) endpoint, with no
// /<actor_id> segment (the connection binds no actor). Mirrors buildWSURL
// minus the actor id.
func buildAnonymousWSURL(nestBaseURL string) string {
	base := strings.TrimRight(nestBaseURL, "/")
	switch {
	case strings.HasPrefix(base, "https://"):
		base = "wss://" + base[len("https://"):]
	case strings.HasPrefix(base, "http://"):
		base = "ws://" + base[len("http://"):]
	}
	return base + "/api/v1/ws"
}

// Call sends a Request and waits for the matching Reply.
//
// `body` is canonical-CBOR-encoded via internal/dagcbor.
//
// On Reply.OK=true the wire payload is decoded into `reply` (must be a
// pointer); on Reply.OK=false a *ServerError carrying the raw payload
// is returned. On context cancellation a CancelFrame is sent and
// ctx.Err() returned. On WS teardown ErrClosed is returned.
//
// Increments the wsrpc_calls_total / wsrpc_call_seconds metrics.
func (c *Client) Call(ctx context.Context, method string, body any, reply any) (err error) {
	if c.closed.Load() {
		return ErrClosed
	}
	start := time.Now()
	result := "ok"
	defer func() {
		metrics.WSRPCCallSeconds.WithLabelValues(method).Observe(time.Since(start).Seconds())
		if err != nil {
			switch {
			case errors.Is(err, context.DeadlineExceeded), errors.Is(err, context.Canceled):
				result = "timeout"
			default:
				result = "error"
			}
		}
		metrics.WSRPCCallsTotal.WithLabelValues(method, result).Inc()
	}()

	payload, err := dagcbor.Marshal(body)
	if err != nil {
		return fmt.Errorf("wsrpc Call: encode body for %q: %w", method, err)
	}
	var idemKey IdempotencyKey
	if _, err := rand.Read(idemKey[:]); err != nil {
		return fmt.Errorf("wsrpc Call: random idempotency_key: %w", err)
	}
	corr := c.nextCorr.Add(1)
	req := &RequestFrame{
		Type:           TypeRequest,
		CorrelationID:  corr,
		Kind:           method,
		IdempotencyKey: idemKey,
		Payload:        payload,
	}
	wire, err := EncodeRequest(req)
	if err != nil {
		return fmt.Errorf("wsrpc Call: encode envelope for %q: %w", method, err)
	}

	// Register the result channel before sending so a fast reply
	// doesn't race with the writer.
	ch := make(chan *ReplyFrame, 1)
	c.mu.Lock()
	c.pending[corr] = ch
	c.mu.Unlock()
	// Always clean up the pending entry on any exit path.
	defer func() {
		c.mu.Lock()
		delete(c.pending, corr)
		c.mu.Unlock()
	}()

	if err := c.writeBinary(ctx, wire); err != nil {
		return fmt.Errorf("wsrpc Call: send request for %q: %w", method, err)
	}

	select {
	case rep := <-ch:
		if rep == nil {
			// reader signalled close via nil
			return ErrClosed
		}
		if !rep.OK {
			return &ServerError{Payload: rep.Payload}
		}
		if reply != nil && len(rep.Payload) > 0 {
			if err := cbor.Unmarshal(rep.Payload, reply); err != nil {
				return fmt.Errorf("wsrpc Call: decode reply for %q: %w", method, err)
			}
		}
		return nil
	case <-ctx.Done():
		// Best-effort cancel — fire and forget; the wsrpc spec lets
		// nest finish the work in progress, the cancel is a hint not a
		// guarantee. We use a fresh non-cancelled context for the
		// cancel write so the cancel frame actually goes out.
		go c.sendCancel(corr)
		return ctx.Err()
	case <-c.readerDone:
		return ErrClosed
	}
}

// sendCancel writes a CancelFrame for the given correlation_id. Fire
// and forget; we never block Call's caller on it.
func (c *Client) sendCancel(corr uint64) {
	if c.closed.Load() {
		return
	}
	wire, err := EncodeCancel(&CancelFrame{Type: TypeCancel, CorrelationID: corr})
	if err != nil {
		c.log().Warn("wsrpc: encode cancel", "corr", corr, "err", err)
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := c.writeBinary(ctx, wire); err != nil {
		// Demote to debug — by the time we cancel, the server may
		// already be closing the connection.
		c.log().Debug("wsrpc: send cancel", "corr", corr, "err", err)
	}
}

// writeBinary sends a binary WS frame; serialises concurrent callers.
func (c *Client) writeBinary(ctx context.Context, b []byte) error {
	c.writeMu.Lock()
	defer c.writeMu.Unlock()
	return c.conn.Write(ctx, websocket.MessageBinary, b)
}

// readLoop is the single reader goroutine. Exits when the WS read
// returns an error or the connection is closed.
func (c *Client) readLoop() {
	defer close(c.readerDone)
	defer c.markClosed(nil)
	for {
		typ, data, err := c.conn.Read(context.Background())
		if err != nil {
			c.markClosed(err)
			return
		}
		if typ != websocket.MessageBinary {
			c.log().Debug("wsrpc: dropping non-binary frame", "type", typ.String())
			continue
		}
		frame, err := DecodeFrame(data)
		if err != nil {
			c.log().Warn("wsrpc: decode frame", "err", err)
			continue
		}
		switch f := frame.(type) {
		case *ReplyFrame:
			c.mu.Lock()
			ch, ok := c.pending[f.CorrelationID]
			c.mu.Unlock()
			if !ok {
				c.log().Debug("wsrpc: reply for unknown correlation_id (likely after cancel)", "corr", f.CorrelationID)
				continue
			}
			// Buffered channel size 1 — non-blocking send.
			select {
			case ch <- f:
			default:
				// Should be unreachable (Call drains the channel once),
				// but don't deadlock the reader if it ever happens.
				c.log().Warn("wsrpc: reply channel full", "corr", f.CorrelationID)
			}
		case *PushFrame:
			if hp := c.onPush.Load(); hp != nil {
				(*hp)(f.Kind, f.Payload, f.Seq)
			}
		case *RequestFrame, *CancelFrame:
			c.log().Debug("wsrpc: ignoring server-initiated request/cancel", "frame", fmt.Sprintf("%T", frame))
		}
	}
}

// markClosed flips the closed flag, records the reason, and unblocks
// every pending Call. Idempotent.
func (c *Client) markClosed(reason error) {
	if !c.closed.CompareAndSwap(false, true) {
		return
	}
	c.closeErrMu.Lock()
	c.closeErr = reason
	c.closeErrMu.Unlock()
	c.mu.Lock()
	pending := c.pending
	c.pending = make(map[uint64]chan *ReplyFrame)
	c.mu.Unlock()
	for _, ch := range pending {
		select {
		case ch <- nil:
		default:
		}
	}
}

// Close shuts the WS down (StatusNormalClosure) and unblocks every
// in-flight Call. Idempotent.
func (c *Client) Close() error {
	if c.closed.Load() {
		// Already closing; wait for the reader to exit.
		<-c.readerDone
		return nil
	}
	err := c.conn.Close(websocket.StatusNormalClosure, "client closing")
	<-c.readerDone
	c.closeErrMu.Lock()
	cerr := c.closeErr
	c.closeErrMu.Unlock()
	if err != nil {
		return err
	}
	// Filter the reader-loop's expected close error (normal closure
	// surfaces as a CloseError from nhooyr).
	if cerr != nil && websocket.CloseStatus(cerr) == websocket.StatusNormalClosure {
		return nil
	}
	return nil
}

// BackoffSchedule returns the n-th retry delay for the WS-RPC reconnect
// policy: exponential 1s, 2s, 4s, 8s, 16s, capped at 30s, with up to
// 25% jitter added on top. The 30s cap is the ratified value
// (mail-bridge-lifecycle.md § Reconnecting — "a compromise that the
// marathon test ratifies") and keeps a flapping server from getting
// hammered.
//
// `n` is the zero-based attempt index: 0 = first retry. This is the curve
// [ReconnectingClient] drives in production (its default Backoff); it stays
// exported for tests + alternate reconnect policies.
func BackoffSchedule(n int) time.Duration {
	const (
		base     = time.Second
		cap_     = 30 * time.Second
		capShift = 5 // 2^5 s = 32s already exceeds the 30s cap
	)
	if n < 0 {
		n = 0
	}
	// 1s, 2s, 4s, 8s, 16s, then clamp at 30s. Clamp the exponent *before*
	// shifting: the old `base * 2^n` (base ≈ 2^30 ns) overflows int64 for
	// n≳34 and wraps to small/zero/negative values — at n≥55 it wrapped to
	// *exactly 0*, so a long-pending bridge (or a long-flapping reconnect)
	// busy-looped at `next_poll_in: 0s`. Clamping n first makes overflow
	// impossible and keeps the schedule monotonic for every n.
	d := cap_
	if n < capShift {
		d = base << uint(n) // 1,2,4,8,16 s — cannot overflow
	}
	// 0..25% additive jitter.
	jitter := time.Duration(mathrand.Float64() * float64(d) * 0.25) //nolint:gosec // jitter, not cryptographic
	return d + jitter
}

// EncodeReplyForTest is a test-only helper exposing the test mock's
// ability to encode a synthetic reply. Kept exported so the
// client_test.go file can use it without reaching into envelope.go's
// internals.
func EncodeReplyForTest(corr uint64, body any, ok bool) ([]byte, error) {
	payload, err := dagcbor.Marshal(body)
	if err != nil {
		return nil, err
	}
	return EncodeReply(&ReplyFrame{
		Type:          TypeReply,
		CorrelationID: corr,
		Payload:       payload,
		OK:            ok,
	})
}
