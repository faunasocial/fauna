//go:build !fauna_e2e_fixtures

// The production-flavor half of the proxy-fixtures seam's tests: the harness
// variable must be INERT with the tag absent — set it to a real-looking map and
// assert the twins never look. A plain `go test` is the production flavor.
package main

import (
	"context"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotolex"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/safefetch"
)

// Spelled out rather than shared with the tagged file on purpose — the
// production package must not carry the constant.
const productionInertProxyFixturesEnv = "FAUNA_ATPROTO_PROXY_FIXTURES"

func TestFixtureSeamIsInertInProduction(t *testing.T) {
	t.Setenv(productionInertProxyFixturesEnv, `{"did:web:appview.test#bsky_appview": "http://127.0.0.1:4321"}`)
	real := safefetch.New(ffiFetchGuard{}).WithCaps(proofMaxBytes, proofFetchTimeout)
	if _, ok := proofFetcherFromEnv(real).(safeProofFetcher); !ok {
		t.Fatalf("production build must return the bare guarded proof fetcher whatever the environment says, got %T", proofFetcherFromEnv(real))
	}
	resolver, stream := proxyFixtureWrap(nil, nil)
	if resolver != nil || stream != nil {
		t.Fatal("production build must return the resolver/stream it was given, unchanged")
	}
}

// Spelled out for the same reason as its sibling above.
const productionInertPermissionSetFixturesEnv = "FAUNA_ATPROTO_PERMISSION_SET_FIXTURES"

func TestPermissionSetFixtureSeamIsInertInProduction(t *testing.T) {
	t.Setenv(productionInertPermissionSetFixturesEnv, `{"com.example.calendar.appPerms": "oQ=="}`)
	real := &inertProbeResolver{}
	if got := permissionSetFixtureWrap(real); got != atprotolex.DocumentResolver(real) {
		t.Fatalf("production build must return the resolver it was given whatever the environment says, got %T", got)
	}
}

type inertProbeResolver struct{}

func (*inertProbeResolver) ResolveSetDocument(context.Context, string) ([]byte, error) {
	return nil, nil
}
