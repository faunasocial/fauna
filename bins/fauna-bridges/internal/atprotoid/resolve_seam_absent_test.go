//go:build !fauna_e2e_fixtures

// The production-flavor half of the fake-DNS seam's tests: with the tag absent
// the harness variable must be INERT — not "unset", inert — so this sets it to
// a live server and asserts the twin never looks. A plain `go test` (the
// untagged run every Go gate makes) is the production flavor, which is what
// makes this the witness that matters: it runs where the seam must not.
package atprotoid

import (
	"net"
	"testing"
)

// The variable's name is spelled out here rather than shared with the tagged
// file on purpose: the production package must not carry the constant, so the
// test cannot import it — and a rename of the seam that forgot this test would
// surface as a stale name here, which is the right failure.
const productionInertFakeDNSEnv = "FAUNA_ATPROTO_FAKE_DNS_URL"

func TestTXTResolverFromEnvIgnoresTheHarnessVariableInProduction(t *testing.T) {
	srv := fakeDNSServer(t, map[string][]string{
		"_atproto.alice.example.com": {"did=did:plc:abc"},
	})
	t.Setenv(productionInertFakeDNSEnv, srv.URL)
	if got := TXTResolverFromEnv(); got != TXTResolver(net.DefaultResolver) {
		t.Fatalf("production build must return the real system resolver whatever the environment says, got %T", got)
	}
}
