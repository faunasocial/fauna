// Microbenchmarks for the WS-RPC client.
//
// BenchmarkValidateRecipientCall closes the RTT-microbench gap flagged
// in the Phase B.4 self-review: measures the Call() round-trip against
// an in-process httptest WebSocket server, with a synchronous
// fast-path reply handler. The number we care about is "what's the
// floor under SMTP-stage latencies?" — if a localhost loopback Call()
// is bounded in the low hundreds of microseconds, the spec's "extra
// round trips are invisible at MTA latencies" claim holds: SMTP per-
// frame latencies are dominated by the peer's response time (typically
// tens of ms or more), so a ~100µs internal hop is invisible.
//
// `just mail-bridge-bench` runs this with `-benchmem` and reports
// ns/op. The benchmark intentionally does NOT include the dial cost
// (one-shot, amortised over the connection's lifetime) — only the
// per-call Round-Trip Time of an established connection is interesting
// here.
package wsrpc

import (
	"context"
	"crypto/ed25519"
	"encoding/hex"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"nhooyr.io/websocket"
)

// BenchmarkValidateRecipientCall measures a fast-path Call() round-trip
// against a local httptest WebSocket server. The server reads each
// Request, echoes back a synthetic Reply with the same correlation_id.
//
// Reports ns/op + allocs/op. Run via `just mail-bridge-bench` or
// `go test -bench=. -benchmem -run=^$ ./internal/wsrpc/`.
func BenchmarkValidateRecipientCall(b *testing.B) {
	type validateRecipientReply struct {
		Accept bool   `cbor:"accept"`
		Reason string `cbor:"reason,omitempty"`
	}
	type validateRecipientReq struct {
		LocalPart string `cbor:"local_part"`
		Domain    string `cbor:"domain"`
	}
	mux := http.NewServeMux()
	mux.HandleFunc("/api/v1/auth/challenge", func(w http.ResponseWriter, r *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"nonce":      hex.EncodeToString(make([]byte, 32)),
			"expires_in": 300,
			"expires_at": time.Now().Unix() + 300,
		})
	})
	mux.HandleFunc("/api/v1/auth/verify", func(w http.ResponseWriter, r *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"token":      "bench-token",
			"expires_in": 3600,
			"expires_at": time.Now().Unix() + 3600,
		})
	})
	mux.HandleFunc("/api/v1/ws/", func(w http.ResponseWriter, r *http.Request) {
		// Reject non-fauna.v1 subprotocols (mirrors the real nest).
		proto := r.Header.Get("Sec-WebSocket-Protocol")
		if !strings.Contains(proto, "fauna.v1") {
			http.Error(w, "subprotocol", http.StatusUnauthorized)
			return
		}
		conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{
			Subprotocols: []string{"fauna.v1"},
		})
		if err != nil {
			return
		}
		defer conn.Close(websocket.StatusNormalClosure, "")
		conn.SetReadLimit(16 << 20)
		for {
			ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
			_, data, err := conn.Read(ctx)
			cancel()
			if err != nil {
				return
			}
			frame, err := DecodeFrame(data)
			if err != nil {
				return
			}
			req, ok := frame.(*RequestFrame)
			if !ok {
				continue
			}
			rep, err := EncodeReplyForTest(req.CorrelationID, validateRecipientReply{Accept: true}, true)
			if err != nil {
				return
			}
			wctx, wcancel := context.WithTimeout(context.Background(), 30*time.Second)
			_ = conn.Write(wctx, websocket.MessageBinary, rep)
			wcancel()
		}
	})
	srv := httptest.NewServer(mux)
	defer srv.Close()

	pub, priv, err := ed25519.GenerateKey(nil)
	if err != nil {
		b.Fatalf("gen key: %v", err)
	}
	auth := NewAuthClient(http.DefaultClient, srv.URL, []byte(pub), priv)
	auth.acquireToken = staticAcquire("bench-bearer-token")
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	c, err := Dial(ctx, ClientConfig{
		NestEndpoint: srv.URL,
		AuthClient:   auth,
	})
	if err != nil {
		b.Fatalf("Dial: %v", err)
	}
	defer c.Close()

	req := validateRecipientReq{LocalPart: "alice", Domain: "example.com"}

	b.ReportAllocs()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		var got validateRecipientReply
		if err := c.Call(ctx, MethodValidateRecipient, req, &got); err != nil {
			b.Fatalf("Call: %v", err)
		}
		if !got.Accept {
			b.Fatalf("Accept = false")
		}
	}
}
