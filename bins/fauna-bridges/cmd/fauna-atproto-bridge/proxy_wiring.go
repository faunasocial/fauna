package main

// Production wiring for F3 service proxying: the guarded resolver + fetcher
// the atprotopds proxy path composes. The test-only fixture seam the tier_3
// e2e drives a local fake AppView through is fixtures_seam.go — e2e flavor
// only (`-tags fauna_e2e_fixtures`), with fixtures_seam_absent.go the
// production twin whose `proxyFixtureWrap` returns its inputs unchanged.

import (
	"context"
	"fmt"
	"net/http"
	"time"

	faunaAtproto "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_bridge_atproto"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotoid"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotopds"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/safefetch"
)

// safeDocFetcher adapts safefetch's buffered GET to the resolver's
// did:web DocFetcher seam — the guarded arm, the one that dials
// attacker-named hosts.
type safeDocFetcher struct{ f *safefetch.Fetcher }

func (s safeDocFetcher) FetchDoc(ctx context.Context, url string) ([]byte, error) {
	body, resp, err := s.f.Do(ctx, url, nil)
	if err != nil {
		return nil, err
	}
	if resp.StatusCode/100 != 2 {
		return nil, fmt.Errorf("DID document fetch returned %d", resp.StatusCode)
	}
	return body, nil
}

// proxyConfig assembles the production ProxyConfig: the AppView default
// carried across the FFI (never a Go copy of the Rust constant), resolution
// through the guarded fetcher, forwards through the same guard.
func proxyConfig() atprotopds.ProxyConfig {
	fetcher := safefetch.New(ffiFetchGuard{})
	var resolver atprotopds.ServiceEndpointResolver = atprotoid.ServiceResolver{
		Guarded:          safeDocFetcher{f: fetcher},
		HTTP:             &http.Client{Timeout: 10 * time.Second},
		DirectoryBaseURL: atprotoid.PLCDirectoryBaseURL(),
	}
	var stream atprotopds.StreamFetcher = fetcher
	// The e2e flavor layers the harness's fixture seam here; the production
	// twin is the identity, so a shipped bridge has no path but the guarded one.
	resolver, stream = proxyFixtureWrap(resolver, stream)
	return atprotopds.ProxyConfig{
		AppViewDID: faunaAtproto.AppviewServiceDid(),
		Resolver:   resolver,
		Fetch:      stream,
	}
}
