// Package safefetch performs SSRF-guarded outbound HTTP fetches to
// attacker-directed targets (docs/goal/behavior/atproto-pds-full.md § F3
// detail, *Service proxying*; § F4 detail, *Client metadata resolution*).
//
// Two callers share one guard, deliberately: F3 dials the service endpoint
// named by an `atproto-proxy: did#fragment` header, and F4 fetches the OAuth
// `client_id` URL. Both URLs are chosen by whoever is making the request, so
// both can aim the bridge at cloud IMDS (169.254.169.254) or an internal
// service unless something stops them.
//
// There is deliberately NO policy in this file — the same split as D8
// (internal/atprotopds/authz.go). Deciding whether a target is safe lives in
// shared Rust (`fauna_bridge_atproto::fetch_guard`); Go's job is the three
// things a pure function cannot do: resolve the name, pin the verified
// addresses for the dial, and cap the transfer. A new range or scheme rule is
// a Rust change with Rust tests, never a Go change.
//
// This package stays cgo-free via the Guard seam (the ffiTranslator /
// Authorizer pattern): production wires the FFI adapter from
// cmd/fauna-atproto-bridge, tests wire a fake.
package safefetch

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"time"
)

// Target mirrors `fauna_bridge_atproto::fetch_guard::FetchTarget`.
//
// It carries parsed components rather than a URL string on purpose: this
// package parses the URL once and dials with those same components, so there
// is no second parser for an attacker to disagree with (the classic SSRF
// bypass where the check inspects host A and the connection reaches host B).
type Target struct {
	Scheme      string
	Host        string
	ResolvedIPs []string
}

// Verdict mirrors `fauna_bridge_atproto::fetch_guard::FetchTargetVerdict`.
type Verdict struct {
	Allow bool
	// Reason is the module's stable deny string when !Allow.
	Reason string
}

// Guard is the SSRF-policy seam — the shared-Rust module.
type Guard interface {
	CheckFetchTarget(Target) Verdict
}

// Defaults. Hard-coded constants, never configuration: nobody would ever want
// to choose these (docs/goal/principles.md — the only configuration surface is
// the apps).
const (
	// DefaultMaxBytes caps a guarded response body. Generous for an XRPC JSON
	// reply or an OAuth client-metadata document, small enough that a hostile
	// endpoint cannot stream the bridge out of memory.
	DefaultMaxBytes int64 = 5 << 20 // 5 MiB
	// DefaultTimeout bounds the whole request, connection included.
	DefaultTimeout = 10 * time.Second
)

// ErrBodyTooLarge is returned when a response body exceeds the cap. It is a
// distinct error because a truncated body must never be mistaken for a short
// one by a caller that parses whatever it got.
var ErrBodyTooLarge = errors.New("safefetch: response body exceeded cap")

// Fetcher performs guarded fetches. The zero value is not usable — use New.
type Fetcher struct {
	guard Guard
	// lookup resolves a host to its addresses. A seam rather than a
	// *net.Resolver so tests can drive the pinning path without real DNS.
	lookup   func(context.Context, string) ([]string, error)
	maxBytes int64
	timeout  time.Duration
}

// New returns a Fetcher with the default caps and the system resolver.
func New(guard Guard) *Fetcher {
	return &Fetcher{
		guard:    guard,
		lookup:   systemLookup,
		maxBytes: DefaultMaxBytes,
		timeout:  DefaultTimeout,
	}
}

func systemLookup(ctx context.Context, host string) ([]string, error) {
	addrs, err := net.DefaultResolver.LookupIPAddr(ctx, host)
	if err != nil {
		return nil, err
	}
	ips := make([]string, 0, len(addrs))
	for _, a := range addrs {
		ips = append(ips, a.IP.String())
	}
	return ips, nil
}

// WithCaps overrides the transfer caps (tests use it; production takes the
// defaults).
func (f *Fetcher) WithCaps(maxBytes int64, timeout time.Duration) *Fetcher {
	f.maxBytes = maxBytes
	f.timeout = timeout
	return f
}

// DenyError reports a target the guard refused. It carries the module's reason
// verbatim so a caller can log precisely why without re-deriving policy.
type DenyError struct {
	Reason string
	Host   string
}

func (e *DenyError) Error() string {
	return fmt.Sprintf("safefetch: refused %q: %s", e.Host, e.Reason)
}

// Request describes one guarded forward — the streaming face F3's service
// proxying uses (Do below is the buffered-GET face for documents).
type Request struct {
	// Method is http.MethodGet or http.MethodPost (the two XRPC verbs).
	Method string
	URL    string
	Header http.Header
	// Body of a POST; nil for GET. The caller bounds it — this package caps
	// only what comes BACK.
	Body io.Reader
}

// Do performs an SSRF-guarded GET of rawURL and returns the capped body.
//
// Redirects are refused rather than followed: a redirect names a fresh target
// that has not been through the guard, and silently following it would hand an
// attacker exactly the bypass this package prevents. A caller that must follow
// one can call Do again on the new URL, which puts it through the guard.
func (f *Fetcher) Do(ctx context.Context, rawURL string, header http.Header) ([]byte, *http.Response, error) {
	resp, err := f.DoStream(ctx, Request{Method: http.MethodGet, URL: rawURL, Header: header})
	if err != nil {
		return nil, nil, err
	}
	defer func() { _ = resp.Body.Close() }()

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		if errors.Is(err, ErrBodyTooLarge) {
			return nil, nil, ErrBodyTooLarge
		}
		return nil, nil, fmt.Errorf("safefetch: read body: %w", err)
	}
	return body, resp, nil
}

// DoStream performs an SSRF-guarded request and returns the response with its
// body still OPEN — the caller must Close it. Closing releases the request's
// timeout and connection. Reads are capped like Do's: a body that exceeds the
// cap fails the read with ErrBodyTooLarge rather than silently truncating.
//
// The sequence is load-bearing and must not be reordered: parse once, resolve,
// let the module decide, then dial the *verified* addresses. Re-resolving after
// the verdict would re-open the DNS-rebinding window the guard exists to close.
func (f *Fetcher) DoStream(ctx context.Context, freq Request) (*http.Response, error) {
	ctx, cancel := context.WithTimeout(ctx, f.timeout)
	// cancel is carried by the response body's Close — not deferred here,
	// because the body outlives this call by design.

	// One parse. The components below are what both the check and the dial use.
	u, err := url.Parse(freq.URL)
	if err != nil {
		cancel()
		return nil, fmt.Errorf("safefetch: parse target: %w", err)
	}
	host := u.Hostname()
	if host == "" {
		cancel()
		return nil, &DenyError{Reason: "target has no host", Host: freq.URL}
	}

	ips, err := f.lookup(ctx, host)
	if err != nil {
		cancel()
		return nil, fmt.Errorf("safefetch: resolve %q: %w", host, err)
	}

	if v := f.guard.CheckFetchTarget(Target{
		Scheme:      u.Scheme,
		Host:        host,
		ResolvedIPs: ips,
	}); !v.Allow {
		cancel()
		return nil, &DenyError{Reason: v.Reason, Host: host}
	}

	// Pin. The dialer ignores the address the transport derived from the URL
	// and connects to an address the module verified, so a name that answers
	// differently a moment later cannot change where we land.
	port := u.Port()
	if port == "" {
		port = "443"
	}
	pinned := make([]string, 0, len(ips))
	for _, ip := range ips {
		pinned = append(pinned, net.JoinHostPort(ip, port))
	}

	// TLS still verifies the certificate against the URL's *hostname*, not the
	// pinned address: http.Transport derives the SNI/verification name from the
	// request URL and only uses DialContext for the raw connection. So we get
	// both properties — we land on an address the module verified, and we still
	// prove we are talking to the named host.
	transport := &http.Transport{
		DialContext: pinnedDialer(pinned),
		// Proxy deliberately unset: an ambient HTTP_PROXY would defeat the
		// pinning by routing the dial through a third party.
		Proxy: nil,
		// The transport is per-request, so a pooled connection could never be
		// reused anyway — keeping one alive would just leak a socket.
		DisableKeepAlives: true,
	}
	client := &http.Client{
		CheckRedirect: func(*http.Request, []*http.Request) error {
			return http.ErrUseLastResponse
		},
		Transport: transport,
	}

	req, err := http.NewRequestWithContext(ctx, freq.Method, freq.URL, freq.Body)
	if err != nil {
		cancel()
		transport.CloseIdleConnections()
		return nil, fmt.Errorf("safefetch: build request: %w", err)
	}
	for k, vs := range freq.Header {
		for _, v := range vs {
			req.Header.Add(k, v)
		}
	}

	resp, err := client.Do(req)
	if err != nil {
		cancel()
		transport.CloseIdleConnections()
		return nil, fmt.Errorf("safefetch: fetch %q: %w", host, err)
	}
	resp.Body = &cappedBody{
		rc:     resp.Body,
		remain: f.maxBytes,
		release: func() {
			cancel()
			transport.CloseIdleConnections()
		},
	}
	return resp, nil
}

// cappedBody enforces the transfer cap on a streamed response. It ERRORS past
// the cap rather than truncating: a caller mid-copy must see the overrun, not
// mistake a cut-off body for a complete one.
type cappedBody struct {
	rc      io.ReadCloser
	remain  int64
	release func()
}

func (c *cappedBody) Read(p []byte) (int, error) {
	if c.remain < 0 {
		return 0, ErrBodyTooLarge
	}
	n, err := c.rc.Read(p)
	c.remain -= int64(n)
	if c.remain < 0 {
		return 0, ErrBodyTooLarge
	}
	return n, err
}

func (c *cappedBody) Close() error {
	err := c.rc.Close()
	c.release()
	return err
}

// pinnedDialer returns a DialContext that only ever connects to one of addrs,
// trying them in order. The addr argument is discarded by design — that is the
// pinning.
func pinnedDialer(addrs []string) func(context.Context, string, string) (net.Conn, error) {
	d := &net.Dialer{Timeout: 5 * time.Second, KeepAlive: 30 * time.Second}
	return func(ctx context.Context, network, _ string) (net.Conn, error) {
		var lastErr error
		for _, a := range addrs {
			conn, err := d.DialContext(ctx, network, a)
			if err == nil {
				return conn, nil
			}
			lastErr = err
		}
		if lastErr == nil {
			lastErr = errors.New("no verified addresses")
		}
		return nil, lastErr
	}
}
