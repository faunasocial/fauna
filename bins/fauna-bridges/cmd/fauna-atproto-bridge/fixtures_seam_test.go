//go:build fauna_e2e_fixtures

// The e2e-flavor half of the proxy-fixtures seam's tests (`go test -tags
// fauna_e2e_fixtures ./cmd/fauna-atproto-bridge/`); production twin in
// fixtures_seam_absent_test.go.
package main

import (
	"context"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotolex"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/safefetch"
)

func TestProxyFixturesFromEnvParsesTheRefMapAndRefusesGarbage(t *testing.T) {
	t.Setenv(proxyFixturesEnv, "")
	if refs := proxyFixturesFromEnv(); refs != nil {
		t.Fatalf("unset seam must yield nil, got %#v", refs)
	}
	t.Setenv(proxyFixturesEnv, `{"did:web:appview.test#bsky_appview": "http://127.0.0.1:4321"}`)
	refs := proxyFixturesFromEnv()
	if got := refs["did:web:appview.test#bsky_appview"]; got != "http://127.0.0.1:4321" {
		t.Fatalf("ref map = %#v", refs)
	}
	// A malformed value is a harness bug; it must not half-apply.
	t.Setenv(proxyFixturesEnv, `{not json`)
	if refs := proxyFixturesFromEnv(); refs != nil {
		t.Fatalf("malformed seam must yield nil, got %#v", refs)
	}
}

func TestFixtureSeamWrapsOnlyWhenSet(t *testing.T) {
	real := safefetch.New(ffiFetchGuard{}).WithCaps(proofMaxBytes, proofFetchTimeout)

	t.Setenv(proxyFixturesEnv, "")
	if _, ok := proofFetcherFromEnv(real).(safeProofFetcher); !ok {
		t.Fatal("unset seam must leave the guarded proof fetcher bare")
	}
	baseResolver, baseStream := proxyFixtureWrap(nil, nil)
	if baseResolver != nil || baseStream != nil {
		t.Fatal("unset seam must return the production resolver/stream unchanged")
	}

	t.Setenv(proxyFixturesEnv, `{"did:web:appview.test#bsky_appview": "http://127.0.0.1:4321"}`)
	fp, ok := proofFetcherFromEnv(real).(fixtureProofFetcher)
	if !ok {
		t.Fatalf("set seam must layer the fixture proof fetcher, got %T", proofFetcherFromEnv(real))
	}
	if _, ok := fp.real.(safeProofFetcher); !ok {
		t.Fatal("the fixture arm must still fall through to the guarded fetcher for non-fixture URLs")
	}
	resolver, _ := proxyFixtureWrap(nil, nil)
	fr, ok := resolver.(fixtureResolver)
	if !ok {
		t.Fatalf("set seam must layer the fixture resolver, got %T", resolver)
	}
	ep, err := fr.ResolveEndpoint(context.Background(), "did:web:appview.test", "bsky_appview")
	if err != nil || ep != "http://127.0.0.1:4321" {
		t.Fatalf("fixture ref must resolve to the mapped endpoint: %q, %v", ep, err)
	}
}

// seamProbeResolver stands in for the production chain: it records what fell
// through to it and answers a marker.
type seamProbeResolver struct{ asked []string }

func (r *seamProbeResolver) ResolveSetDocument(_ context.Context, nsid string) ([]byte, error) {
	r.asked = append(r.asked, nsid)
	return []byte("from the chain"), nil
}

func TestPermissionSetFixtureSeamServesOnlyTheMappedSets(t *testing.T) {
	real := &seamProbeResolver{}

	t.Setenv(permissionSetFixturesEnv, "")
	if got := permissionSetFixtureWrap(real); got != atprotolexResolver(real) {
		t.Fatalf("unset seam must return the production resolver unchanged, got %T", got)
	}
	t.Setenv(permissionSetFixturesEnv, `{not json`)
	if got := permissionSetFixtureWrap(real); got != atprotolexResolver(real) {
		t.Fatalf("a malformed seam value must leave the production resolver alone, got %T", got)
	}
	t.Setenv(permissionSetFixturesEnv, `{"com.example.calendar.appPerms": "not base64!"}`)
	if got := permissionSetFixtureWrap(real); got != atprotolexResolver(real) {
		t.Fatalf("an undecodable document must leave the production resolver alone, got %T", got)
	}

	// "oQ==" is one dag-cbor byte: an empty map header. The seam serves bytes;
	// what they mean is the expander's business.
	t.Setenv(permissionSetFixturesEnv, `{"com.example.calendar.appPerms": "oQ=="}`)
	wrapped := permissionSetFixtureWrap(real)
	doc, err := wrapped.ResolveSetDocument(context.Background(), "com.example.calendar.appPerms")
	if err != nil || string(doc) != "\xa1" {
		t.Fatalf("a mapped NSID must be served from the fixture: %q, %v", doc, err)
	}
	if len(real.asked) != 0 {
		t.Fatalf("a mapped NSID must not reach the chain, but it asked for %v", real.asked)
	}
	doc, err = wrapped.ResolveSetDocument(context.Background(), "com.example.other.set")
	if err != nil || string(doc) != "from the chain" {
		t.Fatalf("an unmapped NSID must fall through to the production chain: %q, %v", doc, err)
	}
	if len(real.asked) != 1 || real.asked[0] != "com.example.other.set" {
		t.Fatalf("the chain must have been asked exactly for the unmapped set, got %v", real.asked)
	}
}

func atprotolexResolver(r *seamProbeResolver) atprotolex.DocumentResolver { return r }
