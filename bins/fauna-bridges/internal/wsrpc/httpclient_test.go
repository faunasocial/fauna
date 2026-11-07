package wsrpc

import (
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/http/httptest"
	"testing"
)

// The cases mirror the Rust owner's test (`fauna_core::resolve::is_localhost_name`).
func TestIsLoopbackHost(t *testing.T) {
	cases := []struct {
		host string
		want bool
	}{
		{"localhost", true},
		{"LOCALHOST.", true},
		{"a.localhost", true},
		{"A.B.localhost.", true},
		{"localhost.example.com", false},
		{"notlocalhost", false},
		{"example.com", false},
		{"127.0.0.1", true},
		{"::1", true},
		{"10.0.0.1", false},
		{"", false},
	}
	for _, c := range cases {
		if got := isLoopbackHost(c.host); got != c.want {
			t.Errorf("isLoopbackHost(%q) = %v, want %v", c.host, got, c.want)
		}
	}
}

// A localhost name dials loopback by address and is never resolved: the
// admission to the unverified client was by name, so the destination must not
// be a forwarding resolver's answer (domains-and-tls-bootstrap.md § Claim →
// Which host names mean this machine).
func TestLoopbackDialAddr(t *testing.T) {
	cases := []struct{ in, want string }{
		{"localhost:8443", "127.0.0.1:8443"},
		{"x.localhost:443", "127.0.0.1:443"},
		{"A.LOCALHOST.:80", "127.0.0.1:80"},
		{"127.0.0.1:443", "127.0.0.1:443"},
		{"[::1]:443", "[::1]:443"},
		{"nest.example.com:443", "nest.example.com:443"},
		{"garbage", "garbage"},
	}
	for _, c := range cases {
		if got := loopbackDialAddr(c.in); got != c.want {
			t.Errorf("loopbackDialAddr(%q) = %q, want %q", c.in, got, c.want)
		}
	}
}

// End to end: the loopback client reaches a TLS server on 127.0.0.1 through a
// name under `localhost`, the dial going to the address and not the name.
func TestNestHTTPClientDialsLoopbackForALocalhostName(t *testing.T) {
	srv := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_, _ = io.WriteString(w, "ok")
	}))
	defer srv.Close()
	_, port, err := net.SplitHostPort(srv.Listener.Addr().String())
	if err != nil {
		t.Fatal(err)
	}
	endpoint := "https://fauna-row-3951-probe.localhost:" + port
	client := NestHTTPClient(endpoint, slog.New(slog.NewTextHandler(io.Discard, nil)))
	resp, err := client.Get(endpoint + "/")
	if err != nil {
		t.Fatalf("GET %s: %v", endpoint, err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)
	if string(body) != "ok" {
		t.Fatalf("body = %q, want ok", body)
	}
}
