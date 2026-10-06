package atprotoid

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"strings"
)

// TXTResolver resolves DNS TXT records. *net.Resolver satisfies it; tests
// substitute a fake.
//
// TXTResolverFromEnv — the constructor the bridge's two production call sites
// use (the first-emit gate in cmd/fauna-atproto-bridge/projection.go and the
// permission-set chain in permset_wiring.go) — lives in resolve_seam.go (e2e
// flavor, honours the FAUNA_ATPROTO_FAKE_DNS_URL harness seam) and
// resolve_seam_absent.go (production, always the system resolver). Convention
// 15: the seam is compiled out of the release bridge, not switched off at run
// time.
type TXTResolver interface {
	LookupTXT(ctx context.Context, name string) ([]string, error)
}

// VerifyIdentityResolvable is the boot-time first-impression self-check
// primitive (bluesky-pds-bridge design: an account whose DID fails to resolve
// at first AppView index can permanently 404, so verify resolvability before
// the first firehose emit). It checks:
//
//   - did:plc — GET {directoryBaseURL}/{did} answers 200 (the directory
//     serves the DID document);
//   - did:web — GET https://{handle}/.well-known/did.json answers 200;
//   - both — a `_atproto.{handle}` DNS TXT record contains `did={did}`.
//
// All failures are collected (errors.Join) so one call reports the whole
// picture. S2's caller is WARN-ONLY — the mint is already recorded; the check
// exists to surface a broken deployment loudly, never to fail it. S3/S5 wire
// it as the pre-emit gate.
func VerifyIdentityResolvable(ctx context.Context, resolver TXTResolver, httpClient *http.Client, directoryBaseURL, did, handle string) error {
	var errs []error

	var docURL string
	switch {
	case strings.HasPrefix(did, "did:plc:"):
		docURL = strings.TrimRight(directoryBaseURL, "/") + "/" + did
	case strings.HasPrefix(did, "did:web:"):
		docURL = "https://" + handle + "/.well-known/did.json"
	default:
		errs = append(errs, fmt.Errorf("unknown DID method in %q", did))
	}
	if docURL != "" {
		if err := checkHTTP200(ctx, httpClient, docURL); err != nil {
			errs = append(errs, fmt.Errorf("DID document not resolvable: %w", err))
		}
	}

	txtName := "_atproto." + handle
	records, err := resolver.LookupTXT(ctx, txtName)
	if err != nil {
		errs = append(errs, fmt.Errorf("TXT lookup %s: %w", txtName, err))
	} else {
		want := "did=" + did
		found := false
		for _, r := range records {
			if strings.TrimSpace(r) == want {
				found = true
				break
			}
		}
		if !found {
			errs = append(errs, fmt.Errorf("TXT %s has no record %q (got %d records)", txtName, want, len(records)))
		}
	}

	return errors.Join(errs...)
}

func checkHTTP200(ctx context.Context, httpClient *http.Client, url string) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return err
	}
	resp, err := httpClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("GET %s returned %d", url, resp.StatusCode)
	}
	return nil
}
