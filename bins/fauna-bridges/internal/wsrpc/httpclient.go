package wsrpc

import (
	"crypto/tls"
	"log/slog"
	"net"
	"net/http"
	"net/url"
)

// NestHTTPClient returns the HTTP client a bridge uses for its WS-RPC dial to
// the nest. For a loopback nest endpoint (the production in-container case,
// where the nest serves TLS with a cert for its public domain — possibly
// self-signed) it returns a client that skips TLS verification: the dial
// mismatches the hostname / may not chain to a trusted root, and the connection
// never leaves the host so there is nothing to MITM. For any non-loopback
// endpoint it returns the default verifying client.
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
	return &http.Client{
		Transport: &http.Transport{
			// G402: loopback-only; the dial stays within the host. See doc above.
			TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, //nolint:gosec
		},
	}
}

// isLoopbackHost reports whether host is a loopback name/address.
func isLoopbackHost(host string) bool {
	if host == "localhost" {
		return true
	}
	ip := net.ParseIP(host)
	return ip != nil && ip.IsLoopback()
}
