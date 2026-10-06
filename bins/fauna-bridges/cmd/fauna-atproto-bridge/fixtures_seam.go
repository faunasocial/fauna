//go:build fauna_e2e_fixtures

// The proxy-fixtures redirect seam: e2e builds only. Convention 15
// (`docs/goal/architecture/e2e-automation-surface-gating.md` § Implementation
// status today → the Go bridges' leg): compiled out of the release bridge under
// the `fauna_e2e_fixtures` tag, with `fixtures_seam_absent.go` as the
// same-signature production twin. This seam is the one whose payload made the
// old "redirects without adding a capability" reading untenable: a service ref
// or proof URL under a fixture base is dialled DIRECTLY, skipping safefetch's
// SSRF verdict for that host — the harness's fake AppView and fixture authority
// live on loopback, which the guard exists to refuse. Under e2e that is exactly
// right; in a shipped bridge it would be a guard bypass selectable by whoever
// controls the launch environment, so the shipped bridge does not carry it.
package main

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"strings"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotolex"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotopds"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/safefetch"
)

// permissionSetFixturesEnv is a TEST-ONLY seam, the fourth under this tag: a
// JSON object mapping a permission set's NSID to the standard-base64 dag-cbor
// of its `com.atproto.lexicon.schema` record, served by the resolver AS IF the
// chain had verified it. It exists so a tier_3 can drive the nest→bridge
// permission-set request call (atproto-oauth-provider.md § Implementation
// status today, the 2026-09-25 bullet) end to end without publishing a TXT
// record, a DID document and an MST proof; the honest chain stays proven by
// atprotolex's own tests and the cross-binary PAR test. Every NSID NOT in the
// map still runs the full guarded chain, so a refusal in a tier_3 is genuine.
//
// Its payload is exactly the bypass this leg exists to keep out of the shipped
// bridge — a document taken as verified widens an OAuth grant — which is why
// the production twin returns the resolver unchanged and never spells this
// name.
const permissionSetFixturesEnv = "FAUNA_ATPROTO_PERMISSION_SET_FIXTURES"

func permissionSetFixturesFromEnv() map[string][]byte {
	raw := strings.TrimSpace(os.Getenv(permissionSetFixturesEnv))
	if raw == "" {
		return nil
	}
	encoded := map[string]string{}
	if err := json.Unmarshal([]byte(raw), &encoded); err != nil {
		// A malformed seam value is a harness bug; refusing to half-apply it
		// keeps the production path the only one running.
		return nil
	}
	docs := make(map[string][]byte, len(encoded))
	for nsid, b64 := range encoded {
		doc, err := base64.StdEncoding.DecodeString(b64)
		if err != nil {
			return nil
		}
		docs[nsid] = doc
	}
	return docs
}

// permissionSetFixtureWrap layers the fixture documents over the production
// resolver when the harness set them, and returns it unchanged otherwise. The
// production twin returns it unchanged unconditionally.
func permissionSetFixtureWrap(real atprotolex.DocumentResolver) atprotolex.DocumentResolver {
	docs := permissionSetFixturesFromEnv()
	if len(docs) == 0 {
		return real
	}
	return fixtureSetResolver{docs: docs, real: real}
}

type fixtureSetResolver struct {
	docs map[string][]byte
	real atprotolex.DocumentResolver
}

func (f fixtureSetResolver) ResolveSetDocument(ctx context.Context, nsid string) ([]byte, error) {
	if doc, ok := f.docs[nsid]; ok {
		return doc, nil
	}
	return f.real.ResolveSetDocument(ctx, nsid)
}

// proxyFixturesEnv is a TEST-ONLY seam (the FAUNA_ATPROTO_PLC_DIRECTORY_URL /
// link-preview-fixture precedent): a JSON object mapping service refs
// ("did#fragment") to plain-HTTP endpoint URLs. A ref in the map skips DID
// resolution and the SSRF-guarded dial — the harness endpoint is loopback by
// design, which the guard exists to refuse — while every OTHER ref still runs
// the full guarded production path, so guard rejections in a tier_3 stay
// genuine. Test-harness IPC, never operator configuration — and since it is
// compiled only into the e2e flavor, a production deployment CANNOT set it.
const proxyFixturesEnv = "FAUNA_ATPROTO_PROXY_FIXTURES"

func proxyFixturesFromEnv() map[string]string {
	raw := strings.TrimSpace(os.Getenv(proxyFixturesEnv))
	if raw == "" {
		return nil
	}
	refs := map[string]string{}
	if err := json.Unmarshal([]byte(raw), &refs); err != nil {
		// A malformed seam value is a harness bug; refusing to half-apply it
		// keeps the production path the only one running.
		return nil
	}
	return refs
}

// proxyFixtureWrap layers the fixture seam over the production proxy resolver
// and stream fetcher when the harness set it, and returns both unchanged
// otherwise. The production twin returns them unchanged unconditionally.
func proxyFixtureWrap(resolver atprotopds.ServiceEndpointResolver, stream atprotopds.StreamFetcher) (atprotopds.ServiceEndpointResolver, atprotopds.StreamFetcher) {
	refs := proxyFixturesFromEnv()
	if len(refs) == 0 {
		return resolver, stream
	}
	return fixtureResolver{refs: refs, real: resolver}, fixtureFetcher{refs: refs, real: stream}
}

type fixtureResolver struct {
	refs map[string]string
	real atprotopds.ServiceEndpointResolver
}

func (f fixtureResolver) ResolveEndpoint(ctx context.Context, did, fragment string) (string, error) {
	if ep, ok := f.refs[did+"#"+fragment]; ok {
		return ep, nil
	}
	return f.real.ResolveEndpoint(ctx, did, fragment)
}

type fixtureFetcher struct {
	refs map[string]string
	real atprotopds.StreamFetcher
}

func (f fixtureFetcher) DoStream(ctx context.Context, req safefetch.Request) (*http.Response, error) {
	for _, base := range f.refs {
		if strings.HasPrefix(req.URL, strings.TrimRight(base, "/")+"/") {
			hreq, err := http.NewRequestWithContext(ctx, req.Method, req.URL, req.Body)
			if err != nil {
				return nil, err
			}
			for k, vs := range req.Header {
				for _, v := range vs {
					hreq.Header.Add(k, v)
				}
			}
			return (&http.Client{Timeout: safefetch.DefaultTimeout}).Do(hreq)
		}
	}
	return f.real.DoStream(ctx, req)
}

// proofFetcherFromEnv wraps the guarded fetcher with the SAME test-only fixture
// seam the proxy path uses (FAUNA_ATPROTO_PROXY_FIXTURES), rather than
// inventing a second one.
//
// A tier_3 run publishes its fixture authority on loopback, which the guard
// exists to refuse — so a URL under a fixture base is dialled directly, and
// every OTHER URL still runs the full guarded production path. That is what
// keeps a guard rejection in a tier_3 genuine: the fixture arm is reachable
// only for hosts the harness itself put in the map.
func proofFetcherFromEnv(real *safefetch.Fetcher) atprotolex.ProofFetcher {
	refs := proxyFixturesFromEnv()
	if len(refs) == 0 {
		return safeProofFetcher{f: real}
	}
	return fixtureProofFetcher{refs: refs, real: safeProofFetcher{f: real}}
}

type fixtureProofFetcher struct {
	refs map[string]string
	real atprotolex.ProofFetcher
}

func (f fixtureProofFetcher) FetchProof(ctx context.Context, url string) ([]byte, error) {
	for _, base := range f.refs {
		if !strings.HasPrefix(url, strings.TrimRight(base, "/")+"/") {
			continue
		}
		req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
		if err != nil {
			return nil, err
		}
		resp, err := (&http.Client{Timeout: proofFetchTimeout}).Do(req)
		if err != nil {
			return nil, err
		}
		defer func() { _ = resp.Body.Close() }()
		if resp.StatusCode/100 != 2 {
			return nil, fmt.Errorf("permission-set record fetch returned %d", resp.StatusCode)
		}
		// The cap applies on the fixture arm too. A harness is not a reason to
		// read an unbounded body, and a fixture that outgrew the production cap
		// should fail here rather than pass a test the real path would refuse.
		body, err := io.ReadAll(io.LimitReader(resp.Body, proofMaxBytes+1))
		if err != nil {
			return nil, err
		}
		if int64(len(body)) > proofMaxBytes {
			return nil, fmt.Errorf("permission-set record exceeds %d bytes", proofMaxBytes)
		}
		return body, nil
	}
	return f.real.FetchProof(ctx, url)
}
