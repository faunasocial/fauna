package main

// Production wiring for F4's permission-set resolution plane: the guarded
// proof fetcher and the resolver chain the nest's `/oauth/par` reaches an
// `include:` scope through, over the permission_set_requested push
// (docs/goal/behavior/atproto-pds-full.md § F4 detail → *Permission sets*, the
// `:332` chain bullet).
//
// The three seams `atprotolex.SetResolver` declares are filled here and nowhere
// else. Every host below is named by attacker-influenced data — a DNS TXT
// record the NSID's authority publishes, then a PDS endpoint that authority's
// DID document names — so the fetch arm is the SSRF-guarded one F3's proxy
// already uses. There is deliberately no
// unguarded fallback: a nil seam refuses (SetResolver's own rule), because a
// fallback is the bypass rather than the convenience.

import (
	"context"
	"fmt"
	"net/http"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotoid"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotolex"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/safefetch"
)

// proofMaxBytes caps a `sync.getRecord` proof CAR.
//
// Sized for what it actually is rather than reusing the document default: the
// response is a CAR carrying one record plus its MST path and signed commit —
// kilobytes for any real permission set — and the URL is chosen by a third
// party's DID document, so the response size is that party's choice. One MiB is
// far above any honest answer and far below safefetch's 5 MiB general default,
// which is sized for XRPC JSON and blob-ish payloads this path never wants.
const proofMaxBytes int64 = 1 << 20

// proofFetchTimeout bounds one document fetch. A PAR request waits on this (up
// to the fan-out cap's worth of them), so it is short for the same reason
// client-metadata resolution's is: an authority's PDS that cannot answer in ten
// seconds is one this flow should fail against rather than hold a request
// goroutine for.
const proofFetchTimeout = 10 * time.Second

// safeProofFetcher adapts safefetch's buffered GET to
// atprotolex.ProofFetcher — the guarded arm, the one that dials the PDS a
// third-party DID document named.
//
// Its own method name rather than a second FetchDoc implementation: this and
// safeDocFetcher fetch different things with different caps, and one type
// answering both would make the next reader wonder which cap applied.
type safeProofFetcher struct{ f *safefetch.Fetcher }

func (s safeProofFetcher) FetchProof(ctx context.Context, url string) ([]byte, error) {
	body, resp, err := s.f.Do(ctx, url, nil)
	if err != nil {
		return nil, err
	}
	if resp.StatusCode/100 != 2 {
		return nil, fmt.Errorf("permission-set record fetch returned %d", resp.StatusCode)
	}
	return body, nil
}

// permissionSetResolver assembles the production chain, behind its cache.
//
// The document cache is the spec's (24 h stale / 90 d expiry / stale-on-failure
// / short negative TTL) and the TXT cache is the lexicon spec's short one. Both
// serve only NEW ceremonies: a standing grant never re-resolves (`:329`), so the
// durable stale copy is the grant row and a bridge restart loses nothing that
// matters.
func permissionSetResolver() atprotolex.DocumentResolver {
	fetcher := safefetch.New(ffiFetchGuard{}).WithCaps(proofMaxBytes, proofFetchTimeout)
	chain := atprotolex.SetResolver{
		TXT: atprotolex.NewCachingTXTResolver(atprotoid.TXTResolverFromEnv(), nil),
		// The same identity resolver the proxy path builds, for the same
		// reason it is guarded there: a did:web authority is a host the
		// document's author chose.
		Identity: atprotoid.ServiceResolver{
			Guarded:          safeDocFetcher{f: safefetch.New(ffiFetchGuard{})},
			HTTP:             &http.Client{Timeout: 10 * time.Second},
			DirectoryBaseURL: atprotoid.PLCDirectoryBaseURL(),
		},
		// fixtures_seam.go (e2e flavor) layers the harness's fixture seam
		// over the guarded fetcher; fixtures_seam_absent.go (production)
		// returns it bare. Convention 15: the bypass is compiled out, not
		// switched off.
		Fetch: proofFetcherFromEnv(fetcher),
	}
	// fixtures_seam.go (e2e flavor) lays the harness's permission-set
	// documents over the cached chain, so a tier_3 can drive the nest→bridge
	// request call without publishing a real set; fixtures_seam_absent.go
	// (production) returns the chain bare. Convention 15, as above.
	return permissionSetFixtureWrap(atprotolex.NewCachingSetResolver(chain, nil))
}
