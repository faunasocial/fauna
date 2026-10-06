//go:build !fauna_e2e_fixtures

// The production twin of fixtures_seam.go: the proxy-fixtures redirect seam
// does not exist in this build. Convention 15 — the tag is the boundary, so a
// release bridge carries neither the `FAUNA_ATPROTO_PROXY_FIXTURES` read nor
// the direct-dial fixture types that skip safefetch's SSRF verdict, and the
// production recipes grep the built artifact for the variable's name to prove
// it (`just atproto-bridge-build`). Every service ref and every proof URL runs
// the guarded production path; there is no other path to run.
//
// The signatures below are the contract fixtures_seam.go implements. Keep them
// in sync.
package main

import (
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotolex"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotopds"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/safefetch"
)

// proxyFixtureWrap returns the production resolver and stream fetcher
// unchanged: the environment is never consulted here.
func proxyFixtureWrap(resolver atprotopds.ServiceEndpointResolver, stream atprotopds.StreamFetcher) (atprotopds.ServiceEndpointResolver, atprotopds.StreamFetcher) {
	return resolver, stream
}

// proofFetcherFromEnv always returns the guarded fetcher: the environment is
// never consulted here.
func proofFetcherFromEnv(real *safefetch.Fetcher) atprotolex.ProofFetcher {
	return safeProofFetcher{f: real}
}

func permissionSetFixtureWrap(real atprotolex.DocumentResolver) atprotolex.DocumentResolver {
	return real
}
