package wsrpc

import (
	"context"
	"testing"
	"time"

	"nhooyr.io/websocket"
)

// TestBridgeAnswersAServerPing pins the one fact the nest's server-side WS
// heartbeat rests on for this peer.
//
// The nest pings every connection it serves and closes the link when a full
// liveness window passes with no inbound frame (transport.md § Connection
// lifecycle). On the sidecar channel the peer is this Go bridge, and whether it
// answers a Ping is a property of its WebSocket library — not something the nest
// may assume from "RFC 6455 requires a Pong". Assuming exactly that is what
// produced the UA-less-ActivityPub interop outage; and this project does not
// read third-party dependency source. So it is pinned here instead, the same
// way the Go connection limiter's keepalive defaults were: by test.
//
// If this test ever goes red, the nest's heartbeat would start reaping healthy
// bridge connections — so it is a load-bearing regression lock on a dependency
// bump, not a formality.
func TestBridgeAnswersAServerPing(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()

	pinged := make(chan error, 1)
	m := newMockNest(t, nil, func(t *testing.T, conn *websocket.Conn) {
		// Read continuously for as long as the connection lives, exactly as the
		// nest's own listener does — the Pong is an inbound frame like any other.
		done := make(chan struct{})
		go func() {
			defer close(done)
			for {
				if _, _, err := conn.Read(context.Background()); err != nil {
					return
				}
			}
		}()

		// Ping blocks until the peer's Pong arrives (or the budget expires). The
		// budget is generous rather than tuned: a green run returns as soon as
		// the Pong lands and pays nothing for the headroom.
		pctx, pcancel := context.WithTimeout(context.Background(), 15*time.Second)
		pinged <- conn.Ping(pctx)
		pcancel()

		// Hold the connection open briefly so the teardown races nothing.
		select {
		case <-done:
		case <-time.After(time.Second):
		}
	})

	c := dialAgainst(t, ctx, m, nil)
	defer func() { _ = c.Close() }()

	select {
	case err := <-pinged:
		if err != nil {
			t.Fatalf("the bridge did not answer a server-initiated WS Ping: %v\n"+
				"The nest's server-side heartbeat reaps a connection that answers no Ping "+
				"within a liveness window, so a bridge that cannot answer would be "+
				"disconnected every window. Fix the bridge (or exempt the sidecar channel "+
				"in transport.md § Connection lifecycle) before this ships.", err)
		}
	case <-ctx.Done():
		t.Fatal("timed out waiting for the bridge to answer a server-initiated WS Ping")
	}
}
