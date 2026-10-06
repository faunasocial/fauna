package wsrpc

import (
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"nhooyr.io/websocket"
)

// A nest keeps its bearers in memory, so a nest restart rejects every
// outstanding token at the next WS upgrade with HTTP 401 — no close code. The
// bridges redial in-process (NewReconnectingClient), so without a re-mint the
// cached bearer is re-presented until it nears expiry, locking the bridge out
// for up to a token lifetime (transport-connection.md § Connection lifecycle →
// Upgrade-time auth rejection: clear, re-mint once, retry).

// upgradeServer accepts the WS upgrade only for `bearer.<accept>`, answering
// every other bearer with the 401 a restarted nest gives a token it forgot.
func upgradeServer(t *testing.T, accept string, accepted *atomic.Int32) *httptest.Server {
	t.Helper()
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !strings.Contains(r.Header.Get("Sec-WebSocket-Protocol"), "bearer."+accept) {
			http.Error(w, "invalid token", http.StatusUnauthorized)
			return
		}
		conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{Subprotocols: []string{"fauna.v1"}})
		if err != nil {
			return
		}
		accepted.Add(1)
		_ = conn.Close(websocket.StatusNormalClosure, "")
	}))
	t.Cleanup(srv.Close)
	return srv
}

// authMinting returns an AuthClient whose mints hand out `tokens` in order,
// counting every mint.
func authMinting(t *testing.T, baseURL string, mints *atomic.Int32, tokens ...string) *AuthClient {
	t.Helper()
	pub, priv := newTestKey(t)
	auth := NewAuthClient(http.DefaultClient, baseURL, pub, priv)
	auth.acquireToken = func(context.Context) (string, time.Time, error) {
		n := int(mints.Add(1))
		if n > len(tokens) {
			n = len(tokens)
		}
		return tokens[n-1], time.Now().Add(time.Hour), nil
	}
	return auth
}

func TestDial_RemintsOnceAfterUpgrade401(t *testing.T) {
	var accepted, mints atomic.Int32
	srv := upgradeServer(t, "fresh-token", &accepted)
	auth := authMinting(t, srv.URL, &mints, "forgotten-token", "fresh-token")

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	c, err := Dial(ctx, ClientConfig{NestEndpoint: srv.URL, AuthClient: auth})
	if err != nil {
		t.Fatalf("Dial after a restarted nest refused the cached bearer: %v — the bearer was re-presented instead of re-minted", err)
	}
	_ = c.Close()

	if got := mints.Load(); got != 2 {
		t.Errorf("mints = %d, want 2: the cached bearer, then exactly one re-mint after the 401", got)
	}
	if got := accepted.Load(); got != 1 {
		t.Errorf("accepted upgrades = %d, want 1", got)
	}
}

func TestDial_StillRefusedFreshBearerFailsWithoutLooping(t *testing.T) {
	var accepted, mints atomic.Int32
	srv := upgradeServer(t, "never-issued", &accepted)
	auth := authMinting(t, srv.URL, &mints, "forgotten-token", "also-refused")

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if c, err := Dial(ctx, ClientConfig{NestEndpoint: srv.URL, AuthClient: auth}); err == nil {
		_ = c.Close()
		t.Fatal("Dial succeeded against a nest that refuses every bearer")
	}

	if got := mints.Load(); got != 2 {
		t.Errorf("mints = %d, want 2: one re-mint, then the error goes to the caller's backoff — never a refresh→401 busy loop", got)
	}
}
