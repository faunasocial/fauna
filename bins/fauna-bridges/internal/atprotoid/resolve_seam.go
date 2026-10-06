//go:build fauna_e2e_fixtures

// The fake-DNS redirect seam: e2e builds only. Convention 15
// (`docs/goal/architecture/e2e-automation-surface-gating.md` § Implementation
// status today → the Go bridges' leg): the harness's redirect seams are
// compiled out of the release bridge under the `fauna_e2e_fixtures` tag, with
// `resolve_seam_absent.go` as the same-signature production twin. Until
// 2026-09-13 this seam was an unconditional `os.Getenv` in every shipped bridge,
// ruled acceptable because it "redirects without adding a capability" — a
// sentence written the day before the permission-set chain made this resolver
// its root of trust (`cmd/fauna-atproto-bridge/permset_wiring.go`: the
// `_lexicon.<authority>` TXT lookup decides which DID an `include:<nsid>` scope
// resolves to, and every later leg verifies the document THAT DID published).
// A redirect's severity is its payload's, not its mechanism's.
package atprotoid

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"os"
	"strings"
	"time"
)

// fakeTXTEnv is a TEST-ONLY seam, the exact sibling of plcDirectoryURLEnv
// (directory_seam.go): the e2e harness cannot publish real `_atproto.<handle>`
// TXT records for a throwaway test domain, so it points the bridge at a
// test-controlled HTTP server that answers TXT lookups —
//
//	GET {base}/txt/{name}  →  200 + JSON array of strings, or 404 for NXDOMAIN
//
// This is test-harness IPC, never operator configuration — and since it is
// compiled only into the e2e flavor, a production deployment CANNOT set it:
// the release bridge always resolves through the real system resolver.
//
// Why a URL and not a canned JSON map: the record this gate looks for carries
// the DID, which does not exist until the mint completes, and a real
// deployment publishes the TXT *after* minting. A map fixed at exec time
// cannot express "the record appeared later", so it could never exercise the
// gate's retry — the deferred-then-resolvable path is exactly what the
// first-impression trap makes load-bearing. The fake PLC directory next door
// answers `GET /{did}` dynamically for the same reason.
//
// Note what the seam does NOT do: it supplies an *answer* to the resolvability
// gate, it never bypasses the gate. A record naming the wrong DID still fails
// VerifyIdentityResolvable, which keeps the trap (atproto-pds-full.md
// § Ecosystem reality: "DID doc + handle fully resolvable before the first
// event hits the firehose") a real assertion under e2e rather than a disabled
// one.
const fakeTXTEnv = "FAUNA_ATPROTO_FAKE_DNS_URL"

// fakeTXTResolver resolves TXT records against the harness's HTTP server.
// A 404 is an error, not an empty success: that is what the real resolver does
// for NXDOMAIN, and a test asserting the gate BLOCKS needs the honest failure.
type fakeTXTResolver struct {
	baseURL string
	client  *http.Client
}

func (f fakeTXTResolver) LookupTXT(ctx context.Context, name string) ([]string, error) {
	url := f.baseURL + "/txt/" + name
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return nil, err
	}
	resp, err := f.client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("fake DNS (%s) lookup %s: %w", fakeTXTEnv, name, err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("fake DNS (%s): no TXT records for %q (HTTP %d)", fakeTXTEnv, name, resp.StatusCode)
	}
	var records []string
	if err := json.NewDecoder(resp.Body).Decode(&records); err != nil {
		return nil, fmt.Errorf("fake DNS (%s): bad answer for %q: %w", fakeTXTEnv, name, err)
	}
	if len(records) == 0 {
		return nil, fmt.Errorf("fake DNS (%s): no TXT records for %q", fakeTXTEnv, name)
	}
	return records, nil
}

// TXTResolverFromEnv returns the TXT resolver the first-emit gate and the
// permission-set chain resolve through: the real system resolver, unless the
// test-only seam above is set. The e2e flavor's arm — the production twin in
// resolve_seam_absent.go never reads the environment.
func TXTResolverFromEnv() TXTResolver {
	base := strings.TrimSpace(os.Getenv(fakeTXTEnv))
	if base == "" {
		return net.DefaultResolver
	}
	return fakeTXTResolver{
		baseURL: strings.TrimRight(base, "/"),
		client:  &http.Client{Timeout: 10 * time.Second},
	}
}
