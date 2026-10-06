package dav

import (
	"context"
	"io"
	"net"
	"net/http"
	"testing"
	"time"
)

// markerHandler writes a fixed body so a test can assert which mount served a
// request. It also echoes the received URL path so the routing assertions can
// confirm the handler sees the FULL request path (the shared mux does not strip
// the prefix — emersion's depth routing needs the whole path).
func markerHandler(marker string) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("X-DAV-Mount", marker)
		w.Header().Set("X-DAV-Path", r.URL.Path)
		w.WriteHeader(http.StatusOK)
		_, _ = io.WriteString(w, marker)
	})
}

// serveOnLoopback binds a plain (non-TLS) loopback listener and serves srv on
// it, returning the base URL and a cleanup that gracefully shuts the server
// down. TLSConfig is left nil so the test speaks plain HTTP — this package's
// job is path routing + drain, not TLS termination (that is the caller's
// tls.NewListener wrap in mda.go).
func serveOnLoopback(t *testing.T, srv *Server) (baseURL string, cleanup func()) {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	serveErr := make(chan error, 1)
	go func() { serveErr <- srv.Serve(ln) }()
	return "http://" + ln.Addr().String(), func() {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		srv.Shutdown(ctx)
		if err := <-serveErr; err != nil {
			t.Errorf("Serve returned error: %v", err)
		}
	}
}

func get(t *testing.T, url string) (status int, mount, path string) {
	t.Helper()
	resp, err := http.Get(url)
	if err != nil {
		t.Fatalf("GET %s: %v", url, err)
	}
	defer resp.Body.Close()
	_, _ = io.Copy(io.Discard, resp.Body)
	return resp.StatusCode, resp.Header.Get("X-DAV-Mount"), resp.Header.Get("X-DAV-Path")
}

// TestRoutesByLongestPrefix — the load-bearing shared-443 guarantee: with a
// `/carddav/` mount beside a `/` catch-all, `/carddav/…` reaches the CardDAV
// handler and every other path (the root principal, `/caldav/…`, well-known)
// reaches the CalDAV handler. Proves Go ServeMux longest-prefix routing and
// that the mounted handler sees the FULL, un-stripped request path.
func TestRoutesByLongestPrefix(t *testing.T) {
	t.Parallel()
	srv := NewServer(ServerConfig{},
		Mount{Pattern: "/", Handler: markerHandler("caldav")},
		Mount{Pattern: "/carddav/", Handler: markerHandler("carddav")},
	)
	base, cleanup := serveOnLoopback(t, srv)
	defer cleanup()

	cases := []struct {
		path      string
		wantMount string
	}{
		{"/carddav/alice@example.com/", "carddav"},                  // CardDAV home set
		{"/carddav/alice@example.com/deadbeef/card.vcf", "carddav"}, // CardDAV object
		{"/alice@example.com/", "caldav"},                           // root principal (shared shape)
		{"/caldav/alice@example.com/", "caldav"},                    // CalDAV home set
		{"/.well-known/caldav", "caldav"},                           // CalDAV discovery
		{"/", "caldav"},                                             // root
	}
	for _, tc := range cases {
		tc := tc
		t.Run(tc.path, func(t *testing.T) {
			status, mount, gotPath := get(t, base+tc.path)
			if status != http.StatusOK {
				t.Fatalf("GET %s: status = %d, want 200", tc.path, status)
			}
			if mount != tc.wantMount {
				t.Errorf("GET %s: mount = %q, want %q", tc.path, mount, tc.wantMount)
			}
			// The handler must see the full path (no prefix stripping) — emersion's
			// carddav backend routes resources by path-segment depth on r.URL.Path.
			if gotPath != tc.path {
				t.Errorf("GET %s: handler saw path %q, want the full un-stripped path", tc.path, gotPath)
			}
		})
	}
}

// TestCardDAVOnlyMountsServeRoot — the contacts-only deployment (carddav
// enabled, caldav disabled) mounts the CardDAV handler at BOTH `/carddav/` and
// `/` so its root-level principal (`/{user}/`) and `propFindRoot` discovery are
// reachable with no CalDAV catch-all present. mda.davMounts builds this mount
// set; here we assert the substrate routes it as intended.
func TestCardDAVOnlyMountsServeRoot(t *testing.T) {
	t.Parallel()
	h := markerHandler("carddav")
	srv := NewServer(ServerConfig{},
		Mount{Pattern: "/carddav/", Handler: h},
		Mount{Pattern: "/", Handler: h},
	)
	base, cleanup := serveOnLoopback(t, srv)
	defer cleanup()

	for _, path := range []string{"/", "/alice@example.com/", "/carddav/alice@example.com/"} {
		status, mount, _ := get(t, base+path)
		if status != http.StatusOK || mount != "carddav" {
			t.Errorf("GET %s: status=%d mount=%q, want 200/carddav", path, status, mount)
		}
	}
}

// TestNoCatchAll404s — when only `/carddav/` is mounted (no `/` catch-all),
// a request outside `/carddav/` gets the stdlib mux 404. Documents that the
// substrate does not invent a catch-all; the mda.davMounts logic is what
// decides whether `/` is served.
func TestNoCatchAll404s(t *testing.T) {
	t.Parallel()
	srv := NewServer(ServerConfig{},
		Mount{Pattern: "/carddav/", Handler: markerHandler("carddav")},
	)
	base, cleanup := serveOnLoopback(t, srv)
	defer cleanup()

	if status, _, _ := get(t, base+"/alice@example.com/"); status != http.StatusNotFound {
		t.Errorf("GET /alice@example.com/ with no catch-all: status = %d, want 404", status)
	}
	if status, mount, _ := get(t, base+"/carddav/x/"); status != http.StatusOK || mount != "carddav" {
		t.Errorf("GET /carddav/x/: status=%d mount=%q, want 200/carddav", status, mount)
	}
}

// TestShutdownDrainsCleanly — Serve returns nil (not http.ErrServerClosed) once
// Shutdown closes the listener within the grace window, and Shutdown reports
// forced=false on a clean drain. This is the graceful-drain contract the MDA's
// runDAVListener relies on.
func TestShutdownDrainsCleanly(t *testing.T) {
	t.Parallel()
	srv := NewServer(ServerConfig{}, Mount{Pattern: "/", Handler: markerHandler("caldav")})
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	serveErr := make(chan error, 1)
	go func() { serveErr <- srv.Serve(ln) }()

	// One successful request so the server is definitely up.
	if status, _, _ := get(t, "http://"+ln.Addr().String()+"/"); status != http.StatusOK {
		t.Fatalf("warm-up GET: status = %d, want 200", status)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if forced := srv.Shutdown(ctx); forced {
		t.Errorf("Shutdown reported forced=true on a clean drain")
	}
	if err := <-serveErr; err != nil {
		t.Errorf("Serve returned %v after clean Shutdown, want nil", err)
	}
}
