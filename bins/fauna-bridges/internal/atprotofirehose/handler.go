package atprotofirehose

import (
	"bytes"
	"context"
	"log/slog"
	"net"
	"net/http"
	"strconv"
	"time"

	comatproto "github.com/bluesky-social/indigo/api/atproto"
	"github.com/bluesky-social/indigo/events"
	"nhooyr.io/websocket"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/xrpc"
)

// writeTimeout bounds a single frame write, so one wedged TCP peer cannot pin a
// broadcast goroutine (the harness-self-termination discipline applied to the
// serving path: every wait is bounded).
const writeTimeout = 30 * time.Second

// pingInterval / pongTimeout bound how long a dead-but-not-closed connection
// can hold a subscriber slot. Hard-coded, not configurable (§ Product
// invariants) — a test injects its own cadence via registerWithKeepalive.
// Deliberately generous: a dead connection is evicted within one minute, while
// a real consumer — always sitting in its read loop, which is what answers a
// pong — has a full interval to reply. Availability hardening must not itself
// become an availability bug.
const (
	pingInterval = 30 * time.Second
	pongTimeout  = 30 * time.Second
)

// clientIP extracts the peer IP for the per-IP subscriber cap. The listener sits
// behind the SNI router with PROXY protocol v2, so RemoteAddr is the real
// client; forwarding headers are deliberately ignored (the anon-surface
// convention the route's own rate limiter follows — key on the transport peer,
// never on spoofable header text).
func clientIP(r *http.Request) string {
	host, _, err := net.SplitHostPort(r.RemoteAddr)
	if err != nil {
		return r.RemoteAddr
	}
	return host
}

// Register wires com.atproto.sync.subscribeRepos onto F1's route table.
func Register(srv *xrpc.Server, b *Broadcaster, logger *slog.Logger) {
	registerWithKeepalive(srv, b, logger, pingInterval, pongTimeout)
}

// registerWithKeepalive is Register with the keepalive cadence injected, so a
// test does not have to wait out the production interval. The cadence lives on
// the HANDLER rather than in package state: a package-level knob a test mutates
// is read concurrently by every other test's in-flight handler goroutine, which
// is a data race (caught by -race) and a flake waiting to happen.
func registerWithKeepalive(srv *xrpc.Server, b *Broadcaster, logger *slog.Logger, ping, pong time.Duration) {
	if logger == nil {
		logger = slog.Default()
	}
	h := &handler{b: b, logger: logger, pingInterval: ping, pongTimeout: pong}
	srv.Register(xrpc.Route{
		NSID:   "com.atproto.sync.subscribeRepos",
		Method: http.MethodGet,
		Auth:   xrpc.Public,
		Class:  xrpc.ClassPublicRead,
		// The one stream on the table: the frame must NOT arm its
		// response-stall deadline here, because a healthy relay legitimately
		// receives nothing for hours (the same reason writeTimeout above is
		// per-frame and liveness is proved by ping/pong rather than by a read
		// deadline). This route bounds its own writes; see writeFrame.
		LongLived: true,
		Handle:    h.subscribeRepos,
	})
}

type handler struct {
	b            *Broadcaster
	logger       *slog.Logger
	pingInterval time.Duration
	pongTimeout  time.Duration
}

// subscribeRepos upgrades to a WebSocket and streams frames. The upgrade happens
// here, inside the handler, so the route frame's per-IP rate limit and auth-class
// declaration run first (C5: one route table, middleware before the handler).
func (h *handler) subscribeRepos(w http.ResponseWriter, r *http.Request, _ *xrpc.Caller) {
	var cursor int64
	hasCursor := false
	if raw := r.URL.Query().Get("cursor"); raw != "" {
		v, err := strconv.ParseInt(raw, 10, 64)
		if err != nil || v < 0 {
			xrpc.WriteError(w, xrpc.InvalidRequest("cursor must be a non-negative integer"))
			return
		}
		cursor, hasCursor = v, true
	}

	// Admit BEFORE upgrading. The route's rate limiter bounds how fast
	// connections are established; this bounds how many are HELD, which is the
	// quantity that actually exhausts goroutines and fds on a stream designed to
	// stay open forever. Refusing here costs one cheap HTTP 429 instead of a
	// completed upgrade.
	//
	// Attaching before the upgrade also strengthens the gapless handover: frames
	// landing during the handshake buffer up in the subscriber, and liveFrom
	// marks exactly where the replay must stop, so there is no gap and no
	// duplicate.
	sub, liveFrom, err := h.b.subscribe(clientIP(r))
	if err != nil {
		h.logger.Warn("firehose: refusing subscriber at the concurrency cap",
			"ip", clientIP(r), "live", h.b.SubscriberCount())
		xrpc.WriteError(w, xrpc.RateLimited())
		return
	}
	defer h.b.unsubscribe(sub)

	// InsecureSkipVerify disables Origin checking. Safe here and nowhere else on
	// this bridge: the stream is Public (no token), carries no cookies or other
	// ambient authority, and serves only already-public repo data — so there is
	// no cross-origin request forgery to prevent, while real consumers (relays,
	// mirrors, debugging clients) send no Origin header or an arbitrary one.
	conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{InsecureSkipVerify: true})
	if err != nil {
		h.logger.Warn("firehose: websocket upgrade failed", "err", err)
		return
	}
	defer conn.CloseNow()

	// CloseRead drains (and discards) anything the consumer sends and gives back
	// a context cancelled the moment the peer goes away — the read half of the
	// connection is pure liveness signal on this stream. It only fires on a
	// GRACEFUL peer close, though, so a half-open or wedged socket needs the
	// keepalive probe below to be evicted.
	ctx := conn.CloseRead(r.Context())

	replayedTo := liveFrom
	if hasCursor {
		replayedTo, err = h.replay(ctx, conn, cursor, liveFrom)
		if err != nil {
			h.logger.Warn("firehose: replay ended", "cursor", cursor, "err", err)
			return
		}
	}

	// Keepalive probe. A firehose consumer legitimately sends NOTHING and a quiet
	// PDS writes nothing, so neither a read deadline nor the write timeout can
	// tell a healthy idle relay from a dead socket — a raw idle-read deadline
	// would evict exactly the well-behaved consumers we want, which is the same
	// availability bug inverted. A ping demands proof of life instead: a healthy
	// consumer's WS stack answers the pong regardless of application silence, and
	// anything that does not answer within pongTimeout is gone and gets closed.
	keepalive := time.NewTicker(h.pingInterval)
	defer keepalive.Stop()

	for {
		select {
		case <-ctx.Done():
			return
		case <-keepalive.C:
			pctx, cancel := context.WithTimeout(ctx, h.pongTimeout)
			err := conn.Ping(pctx)
			cancel()
			if err != nil {
				h.logger.Info("firehose: closing unresponsive subscriber (no pong)", "err", err)
				// Return straight into `defer conn.CloseNow()` — do NOT attempt a
				// graceful Close here. A graceful close writes a close frame and
				// WAITS for the peer to echo it, and this peer is unresponsive by
				// definition, so it would hold the subscriber slot for the whole
				// close-handshake timeout: the eviction path slow-lorised by the
				// very connection it is evicting. (Measured: the handler logged
				// this line and the slot stayed taken for seconds afterwards.)
				return
			}
		case <-sub.dropped:
			_ = conn.Close(websocket.StatusPolicyViolation, "ConsumerTooSlow")
			return
		case e := <-sub.ch:
			if e.Seq <= replayedTo {
				continue // already sent during replay
			}
			if err := writeFrame(ctx, conn, e.Payload); err != nil {
				return
			}
			replayedTo = e.Seq
		}
	}
}

// replay serves the consumer's requested history and returns the highest seq it
// sent, so the live loop knows where to pick up.
//
// A cursor ahead of our stream is a consumer error (FutureCursor). A cursor
// older than the retained window takes the sanctioned degraded path: one #sync
// per repo announcing the authoritative head, after which the consumer refetches
// history with getRepo (atproto-pds-bridge.md § Projection & backfill).
func (h *handler) replay(ctx context.Context, conn *websocket.Conn, cursor, liveFrom int64) (int64, error) {
	minSeq, maxSeq, ok, err := h.b.store.SeqRange(ctx)
	if err != nil {
		return 0, err
	}
	if ok && cursor > maxSeq {
		if err := h.writeError(ctx, conn, "FutureCursor", "cursor is ahead of this PDS's stream"); err != nil {
			return 0, err
		}
		_ = conn.Close(websocket.StatusPolicyViolation, "FutureCursor")
		return 0, errCursorRejected
	}
	// cursor N means "I have seen through N" — replayable only while N+1 is
	// still retained.
	if !ok || cursor+1 < minSeq {
		h.logger.Info("firehose: cursor outside retention; degrading to #sync",
			"cursor", cursor, "min_retained", minSeq)
		return h.emitSync(ctx, conn, liveFrom)
	}

	sent := cursor
	for sent < liveFrom {
		evs, err := h.b.store.EventsSince(ctx, sent, drainBatch)
		if err != nil {
			return sent, err
		}
		if len(evs) == 0 {
			// The outbox ran dry before reaching the live handover point: a
			// prune swept the rest of the window out from under this replay.
			// Degrade rather than jump the gap silently — a consumer that skips
			// commits it was never told about is exactly the corruption #sync
			// exists to prevent.
			h.logger.Info("firehose: replay window vanished mid-replay; degrading to #sync",
				"sent_through", sent, "live_from", liveFrom)
			return h.emitSync(ctx, conn, liveFrom)
		}
		for _, e := range evs {
			if e.Seq > liveFrom {
				return sent, nil
			}
			if err := writeFrame(ctx, conn, e.Payload); err != nil {
				return sent, err
			}
			sent = e.Seq
		}
	}
	return sent, nil
}

// emitSync writes one #sync frame per repo — the Sync v1.1 authoritative-head
// announcement. All of them carry the current head seq, so a consumer that
// records it and reconnects resumes cleanly from the live tail.
func (h *handler) emitSync(ctx context.Context, conn *websocket.Conn, seq int64) (int64, error) {
	repos, err := h.b.store.ListRepos(ctx)
	if err != nil {
		return seq, err
	}
	now := time.Now().UTC().Format(time.RFC3339)
	for _, repo := range repos {
		car, rev, ok, err := h.b.store.CommitCAR(ctx, repo.DID)
		if err != nil || !ok {
			h.logger.Warn("firehose: skipping #sync for repo without a readable head",
				"did", repo.DID, "err", err)
			continue
		}
		var buf bytes.Buffer
		evt := &events.XRPCStreamEvent{RepoSync: &comatproto.SyncSubscribeRepos_Sync{
			Seq:    seq,
			Did:    repo.DID,
			Rev:    rev,
			Blocks: car,
			Time:   now,
		}}
		if err := evt.Serialize(&buf); err != nil {
			return seq, err
		}
		if err := writeFrame(ctx, conn, buf.Bytes()); err != nil {
			return seq, err
		}
	}
	return seq, nil
}

func (h *handler) writeError(ctx context.Context, conn *websocket.Conn, name, msg string) error {
	var buf bytes.Buffer
	evt := &events.XRPCStreamEvent{Error: &events.ErrorFrame{Error: name, Message: msg}}
	if err := evt.Serialize(&buf); err != nil {
		return err
	}
	return writeFrame(ctx, conn, buf.Bytes())
}

func writeFrame(ctx context.Context, conn *websocket.Conn, payload []byte) error {
	wctx, cancel := context.WithTimeout(ctx, writeTimeout)
	defer cancel()
	return conn.Write(wctx, websocket.MessageBinary, payload)
}

// errCursorRejected ends the handler after an error frame was already sent.
var errCursorRejected = errRejected("cursor rejected")

type errRejected string

func (e errRejected) Error() string { return string(e) }
