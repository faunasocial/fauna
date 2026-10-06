//go:build !fauna_e2e_fixtures

// The production twin of resolve_seam.go: the fake-DNS redirect seam does not
// exist in this build. Convention 15 — the tag is the boundary, so a release
// bridge carries neither the `FAUNA_ATPROTO_FAKE_DNS_URL` read nor the fake
// resolver behind it, and the production recipes grep the built artifact for
// the variable's name to prove it (`just atproto-bridge-build`).
//
// The signature below is the contract resolve_seam.go implements. Keep them in
// sync.
package atprotoid

import "net"

// TXTResolverFromEnv always returns the real system resolver: the environment
// is never consulted here, so setting the harness variable on a release bridge
// changes nothing.
func TXTResolverFromEnv() TXTResolver {
	return net.DefaultResolver
}
