package wsrpc

import (
	"context"
	"crypto/tls"
	"log/slog"
	"net"
	"net/http"
	"net/url"
	"strings"
)

// NestHTTPClient returns the HTTP client a bridge uses for its WS-RPC dial to
// the nest. For a loopback nest endpoint (the production in-container case,
// where the nest serves TLS with a cert for its public domain — possibly
// self-signed) it returns a client that skips TLS verification: the dial
// mismatches the hostname / may not chain to a trusted root, and the connection
// never leaves the host so there is nothing to MITM. For any non-loopback
// endpoint it returns the default verifying client.
//
// A localhost NAME (`localhost`, `x.localhost`) is admitted by its name, so the
// loopback client dials `127.0.0.1` for it and never resolves it: a resolver
// that forwards `x.localhost` would otherwise hand the unverified dial to
// whoever answers (domains-and-tls-bootstrap.md § Claim → Which host names mean
// this machine; RFC 6761 § 6.3).
//
// This is the SINGLE SOURCE of the loopback-only InsecureSkipVerify decision,
// shared by every sibling bridge binary in this module (mail MTA/MDA, the
// atproto PDS bridge). A drifted copy that skipped verification for a
// non-loopback host would be a real MITM hole, so it lives here once and the
// cmd binaries call it (mail-bridge-lifecycle.md § Cold boot — loopback dial).
func NestHTTPClient(endpoint string, logger *slog.Logger) *http.Client {
	u, err := url.Parse(endpoint)
	if err != nil || !isLoopbackHost(u.Hostname()) {
		return http.DefaultClient
	}
	logger.Info("nest endpoint is loopback; skipping TLS verification for the in-host nest dial",
		"endpoint", endpoint)
	dialer := &net.Dialer{}
	return &http.Client{
		Transport: &http.Transport{
			// The admission was by name; the dial goes to loopback by address.
			DialContext: func(ctx context.Context, network, addr string) (net.Conn, error) {
				return dialer.DialContext(ctx, network, loopbackDialAddr(addr))
			},
			// G402: loopback-only; the dial stays within the host. See doc above.
			TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, //nolint:gosec
		},
	}
}

// loopbackDialAddr maps a `host:port` whose host is a localhost name to
// `127.0.0.1:port` — the Rust twin is `fauna_core::resolve::loopback_dial_addr`
// — and returns any other address (a loopback IP literal) unchanged.
func loopbackDialAddr(addr string) string {
	host, port, err := net.SplitHostPort(addr)
	if err != nil || net.ParseIP(host) != nil || !isLoopbackHost(host) {
		return addr
	}
	return net.JoinHostPort("127.0.0.1", port)
}

const localhostLabel = `localhost`

// isLoopbackHost reports whether host names this machine: `localhost` and every
// name under it (`x.localhost`, RFC 6761), case-insensitive, one trailing root
// dot allowed, or a loopback IP literal. The authority for the name rule is the
// Rust owner `fauna_core::resolve::is_localhost_name` (goal doc
// `docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Claim → Which
// host names mean this machine); this copy must agree with it.
func isLoopbackHost(host string) bool {
	name := strings.ToLower(strings.TrimSuffix(host, "."))
	if name == localhostLabel || strings.HasSuffix(name, "."+localhostLabel) {
		return true
	}
	ip := net.ParseIP(host)
	return ip != nil && ip.IsLoopback()
}
