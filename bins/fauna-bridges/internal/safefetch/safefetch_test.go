package safefetch

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

// fakeGuard stands in for the shared-Rust module. It deliberately does NOT
// reimplement the policy — that would be the second decision point the split
// exists to prevent. It records what Go assembled and returns a canned verdict,
// so these tests assert Go's half: assembly, pinning, caps, redirect refusal.
//
// Consequently these tests use plain-HTTP servers and the fake allows them.
// That is not a statement that http:// is permitted — the real module refuses
// any non-https scheme (fetch_guard.rs `only_https_is_allowed`); it is just
// that Go's half is scheme-agnostic and httptest servers are cheaper unencrypted.
type fakeGuard struct {
	verdict Verdict
	seen    []Target
}

func (g *fakeGuard) CheckFetchTarget(t Target) Verdict {
	g.seen = append(g.seen, t)
	return g.verdict
}

func allowGuard() *fakeGuard { return &fakeGuard{verdict: Verdict{Allow: true}} }
func denyGuard() *fakeGuard  { return &fakeGuard{verdict: Verdict{Reason: "test deny"}} }

// fetcherFor builds a Fetcher whose resolution is stubbed to ips.
func fetcherFor(g Guard, ips ...string) *Fetcher {
	f := New(g)
	f.lookup = func(context.Context, string) ([]string, error) { return ips, nil }
	return f
}

func TestGoAssemblesTheTargetItWillDial(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_, _ = w.Write([]byte("ok"))
	}))
	defer srv.Close()
	host, port, _ := net.SplitHostPort(strings.TrimPrefix(srv.URL, "http://"))

	g := allowGuard()
	f := fetcherFor(g, host)
	if _, _, err := f.Do(context.Background(), fmt.Sprintf("http://example.test:%s/x", port), nil); err != nil {
		t.Fatalf("fetch: %v", err)
	}
	if len(g.seen) != 1 {
		t.Fatalf("guard consulted %d times, want 1", len(g.seen))
	}
	got := g.seen[0]
	if got.Scheme != "http" || got.Host != "example.test" {
		t.Fatalf("assembled %+v, want scheme=http host=example.test", got)
	}
	if len(got.ResolvedIPs) != 1 || got.ResolvedIPs[0] != host {
		t.Fatalf("resolved ips %v, want [%s]", got.ResolvedIPs, host)
	}
}

func TestADeniedTargetIsNeverDialled(t *testing.T) {
	var hits int
	srv := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { hits++ }))
	defer srv.Close()
	host, port, _ := net.SplitHostPort(strings.TrimPrefix(srv.URL, "http://"))

	f := fetcherFor(denyGuard(), host)
	_, _, err := f.Do(context.Background(), fmt.Sprintf("http://evil.test:%s/", port), nil)
	var de *DenyError
	if !errors.As(err, &de) {
		t.Fatalf("err = %v, want *DenyError", err)
	}
	if de.Reason != "test deny" {
		t.Fatalf("reason %q, want the module's verbatim reason", de.Reason)
	}
	if hits != 0 {
		t.Fatalf("server was reached %d times — a denied target must never be dialled", hits)
	}
}

// The pinning property: the dial goes to the verified address, not to whatever
// the name would resolve to at connect time.
func TestTheDialGoesToTheVerifiedAddressNotTheName(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_, _ = w.Write([]byte("pinned"))
	}))
	defer srv.Close()
	host, port, _ := net.SplitHostPort(strings.TrimPrefix(srv.URL, "http://"))

	// The URL names a host that does not exist anywhere; only the pinned
	// address can satisfy the request.
	f := fetcherFor(allowGuard(), host)
	body, _, err := f.Do(context.Background(), fmt.Sprintf("http://nonexistent.invalid:%s/", port), nil)
	if err != nil {
		t.Fatalf("fetch: %v", err)
	}
	if string(body) != "pinned" {
		t.Fatalf("body %q, want the pinned server's response", body)
	}
}

func TestARedirectIsRefusedNotFollowed(t *testing.T) {
	var target int
	redirectTo := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { target++ }))
	defer redirectTo.Close()
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Redirect(w, r, redirectTo.URL+"/next", http.StatusFound)
	}))
	defer srv.Close()
	host, port, _ := net.SplitHostPort(strings.TrimPrefix(srv.URL, "http://"))

	f := fetcherFor(allowGuard(), host)
	_, resp, err := f.Do(context.Background(), fmt.Sprintf("http://example.test:%s/", port), nil)
	if err != nil {
		t.Fatalf("fetch: %v", err)
	}
	if resp.StatusCode != http.StatusFound {
		t.Fatalf("status %d, want the 302 handed back unfollowed", resp.StatusCode)
	}
	if target != 0 {
		t.Fatal("the redirect target was fetched — it never went through the guard")
	}
}

func TestAnOversizeBodyIsRefused(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_, _ = w.Write(make([]byte, 4096))
	}))
	defer srv.Close()
	host, port, _ := net.SplitHostPort(strings.TrimPrefix(srv.URL, "http://"))

	f := fetcherFor(allowGuard(), host).WithCaps(1024, DefaultTimeout)
	_, _, err := f.Do(context.Background(), fmt.Sprintf("http://example.test:%s/", port), nil)
	if !errors.Is(err, ErrBodyTooLarge) {
		t.Fatalf("err = %v, want ErrBodyTooLarge", err)
	}
}

func TestABodyExactlyAtTheCapIsAccepted(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_, _ = w.Write(make([]byte, 1024))
	}))
	defer srv.Close()
	host, port, _ := net.SplitHostPort(strings.TrimPrefix(srv.URL, "http://"))

	f := fetcherFor(allowGuard(), host).WithCaps(1024, DefaultTimeout)
	body, _, err := f.Do(context.Background(), fmt.Sprintf("http://example.test:%s/", port), nil)
	if err != nil {
		t.Fatalf("a body exactly at the cap must be accepted: %v", err)
	}
	if len(body) != 1024 {
		t.Fatalf("body %d bytes, want 1024", len(body))
	}
}

func TestASlowEndpointHitsTheTimeout(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		time.Sleep(2 * time.Second)
	}))
	defer srv.Close()
	host, port, _ := net.SplitHostPort(strings.TrimPrefix(srv.URL, "http://"))

	f := fetcherFor(allowGuard(), host).WithCaps(DefaultMaxBytes, 150*time.Millisecond)
	start := time.Now()
	_, _, err := f.Do(context.Background(), fmt.Sprintf("http://example.test:%s/", port), nil)
	if err == nil {
		t.Fatal("a slow endpoint must not hang the bridge")
	}
	if elapsed := time.Since(start); elapsed > time.Second {
		t.Fatalf("took %v — the timeout did not bound the request", elapsed)
	}
}

func TestAUrlWithNoHostIsRefusedBeforeResolution(t *testing.T) {
	g := allowGuard()
	f := fetcherFor(g)
	_, _, err := f.Do(context.Background(), "https:///nohost", nil)
	var de *DenyError
	if !errors.As(err, &de) {
		t.Fatalf("err = %v, want *DenyError", err)
	}
	if len(g.seen) != 0 {
		t.Fatal("the guard was consulted for a hostless URL")
	}
}

// ── DoStream (the F3 proxy-forward face) ─────────────────────────────────────

func TestDoStreamForwardsAPostBodyAndStreamsTheResponse(t *testing.T) {
	var gotMethod, gotBody string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		b, _ := io.ReadAll(r.Body)
		gotMethod, gotBody = r.Method, string(b)
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"streamed":true}`))
	}))
	defer srv.Close()
	_, port, _ := net.SplitHostPort(srv.Listener.Addr().String())

	f := fetcherFor(allowGuard(), "127.0.0.1")
	resp, err := f.DoStream(context.Background(), Request{
		Method: http.MethodPost,
		URL:    fmt.Sprintf("http://example.test:%s/xrpc/x", port),
		Header: http.Header{"Content-Type": {"application/json"}},
		Body:   strings.NewReader(`{"in":1}`),
	})
	if err != nil {
		t.Fatalf("DoStream: %v", err)
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		t.Fatalf("read streamed body: %v", err)
	}
	if gotMethod != http.MethodPost || gotBody != `{"in":1}` {
		t.Errorf("upstream saw %s %q", gotMethod, gotBody)
	}
	if string(body) != `{"streamed":true}` {
		t.Errorf("body = %q", body)
	}
}

// A streamed body past the cap must ERROR mid-read, never truncate into
// something a caller could mistake for a complete reply.
func TestDoStreamCapsTheResponseBodyWithAnError(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_, _ = w.Write(make([]byte, 4096))
	}))
	defer srv.Close()
	_, port, _ := net.SplitHostPort(srv.Listener.Addr().String())

	f := fetcherFor(allowGuard(), "127.0.0.1").WithCaps(1024, 5*time.Second)
	resp, err := f.DoStream(context.Background(), Request{
		Method: http.MethodGet,
		URL:    fmt.Sprintf("http://example.test:%s/", port),
	})
	if err != nil {
		t.Fatalf("DoStream: %v", err)
	}
	defer resp.Body.Close()
	_, err = io.ReadAll(resp.Body)
	if !errors.Is(err, ErrBodyTooLarge) {
		t.Fatalf("read err = %v, want ErrBodyTooLarge", err)
	}
}

func TestDoStreamRefusesADeniedTargetWithoutDialling(t *testing.T) {
	dialled := false
	srv := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { dialled = true }))
	defer srv.Close()
	_, port, _ := net.SplitHostPort(srv.Listener.Addr().String())

	f := fetcherFor(denyGuard(), "127.0.0.1")
	_, err := f.DoStream(context.Background(), Request{
		Method: http.MethodGet,
		URL:    fmt.Sprintf("http://evil.test:%s/", port),
	})
	var deny *DenyError
	if !errors.As(err, &deny) {
		t.Fatalf("err = %v, want a DenyError", err)
	}
	if dialled {
		t.Fatal("a denied target was dialled")
	}
}
