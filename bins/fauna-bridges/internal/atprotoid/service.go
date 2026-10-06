package atprotoid

// Service-endpoint resolution: `did#fragment` → the URL that DID's document
// publishes for that service (docs/goal/behavior/atproto-pds-full.md § F3
// detail, *Service proxying* — "resolve the target DID's service endpoint").
//
// This is the READ half of the DID-document story; didweb.go builds the
// documents this box serves. The two are deliberately separate: what we
// publish about ourselves is ours, what we resolve about a third party is
// attacker-influenced input and is treated as such throughout.
//
// SECURITY — where the untrusted input is, and who guards it:
//
//   - `did:web:<host>` names a HOST. Resolving it means fetching
//     `https://<host>/.well-known/did.json`, so `did:web:169.254.169.254`
//     would aim the bridge at cloud IMDS. That fetch MUST go through the
//     guarded fetcher (internal/safefetch → the shared-Rust policy module).
//     The Guarded seam is not optional and there is no unguarded fallback.
//   - `did:plc:<id>` names nothing: the directory is a hard-coded constant and
//     the DID is only a path component, so that fetch is an ordinary request
//     to a known host — the same treatment FetchLastOp/SubmitOperation give it.
//   - The RESOLVED ENDPOINT is attacker-controlled too: a hostile DID document
//     may publish any URL at all. Nothing here dials it, and the caller that
//     does must put it through the same guard. Resolution returning a URL is
//     not a statement that the URL is safe to reach.
//
// No policy lives in this file — well-formedness only. Whether a target is
// reachable is the guard module's decision, and a second opinion here would be
// the duplicate decision point D8 and the fetch guard both exist to prevent.

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"

	"github.com/bluesky-social/indigo/atproto/atcrypto"
)

// maxDIDDocBytes bounds a DID-document read. A DID document is a small JSON
// object; anything vastly larger is a misbehaving or hostile host, and this
// response is parsed before anything has validated it. (The guarded fetcher
// applies its own cap as well — this covers the did:plc arm, which does not
// go through it.)
const maxDIDDocBytes = 1 << 20

// DocFetcher fetches a DID document by URL and returns its body.
//
// The did:web arm is wired to the SSRF-guarded fetcher in production; the seam
// keeps this package free of that dependency (and cgo-free) and lets tests
// drive both the allow and the refuse path.
type DocFetcher interface {
	FetchDoc(ctx context.Context, url string) ([]byte, error)
}

// ServiceResolver resolves service endpoints from DID documents.
type ServiceResolver struct {
	// Guarded fetches attacker-named URLs — the did:web arm. Required; a nil
	// Guarded refuses did:web rather than falling back to an unguarded fetch.
	Guarded DocFetcher
	// HTTP fetches from the hard-coded PLC directory — a known host, so no
	// guard applies.
	HTTP *http.Client
	// DirectoryBaseURL is the PLC directory (PLCDirectoryBaseURL()).
	DirectoryBaseURL string
}

// ParseServiceRef splits an `atproto-proxy` header value — `did#fragment` —
// into its two halves.
//
// The fragment is REQUIRED. A bare DID names an identity, not a service, and a
// document may publish several services; picking one for the caller would be
// this code inventing an intent the caller did not express.
func ParseServiceRef(v string) (did, fragment string, err error) {
	v = strings.TrimSpace(v)
	if v == "" {
		return "", "", fmt.Errorf("empty service reference")
	}
	did, fragment, found := strings.Cut(v, "#")
	if !found {
		return "", "", fmt.Errorf("service reference %q has no #fragment", v)
	}
	if did == "" || fragment == "" {
		return "", "", fmt.Errorf("service reference %q must be did#fragment", v)
	}
	if !strings.HasPrefix(did, "did:") {
		return "", "", fmt.Errorf("service reference %q does not start with a DID", v)
	}
	// One `#` only: a second would make "which fragment" ambiguous, and
	// ambiguity in a security-relevant selector is a bug waiting for a parser
	// disagreement.
	if strings.Contains(fragment, "#") {
		return "", "", fmt.Errorf("service reference %q has more than one #", v)
	}
	return did, fragment, nil
}

// didDocument models the fields resolution needs. Unknown fields are ignored
// (a DID document carries much more); `serviceEndpoint` is typed as a string
// because that is what ATProto publishes — a document using DID-core's object
// or array form fails to decode, which is the correct fail-closed outcome
// rather than a guess about which entry was meant.
type didDocument struct {
	ID      string `json:"id"`
	Service []struct {
		ID              string `json:"id"`
		Type            string `json:"type"`
		ServiceEndpoint string `json:"serviceEndpoint"`
	} `json:"service"`
	VerificationMethod []struct {
		ID                 string `json:"id"`
		Type               string `json:"type"`
		Controller         string `json:"controller"`
		PublicKeyMultibase string `json:"publicKeyMultibase"`
	} `json:"verificationMethod"`
}

// The two fragments an ATProto identity publishes: where the repo lives, and
// which key signs it. Named here rather than at each call site because a
// caller passing the wrong one gets an ordinary "no such service" error, which
// reads like a missing document rather than a typo.
const (
	atprotoPDSFragment     = "atproto_pds"
	atprotoSigningFragment = "atproto"
)

// AtprotoIdentity is what a DID document says about an ATProto identity: the
// PDS that serves its repo, and the key whose signature makes that repo's
// commits believable.
//
// The two travel together because they are only meaningful together. Asking
// one PDS for a record and checking it against a key read from a *different*
// fetch of a mutable document is the substituted-document hole one fetch
// closes by construction — see ResolveAtprotoIdentity.
type AtprotoIdentity struct {
	DID         string
	PDSEndpoint string
	// SigningKey is the `#atproto` verification method. Never nil on a
	// successful return: a document whose key does not parse is a failed
	// resolution, because continuing with a nil key would make every
	// downstream signature check vacuously pass.
	SigningKey atcrypto.PublicKey
}

// ResolveAtprotoIdentity fetches `did`'s document ONCE and reads both halves an
// ATProto identity publishes.
//
// # Why one fetch, and why the two halves are not separate calls
//
// The permission-set resolution chain (atproto-pds-full.md § F4 detail →
// *Permission sets*) asks a PDS for a record and then verifies that record
// against the authority's signing key. Both facts come from the DID document,
// and a DID document is mutable and third-party-hosted. Reading it twice would
// let the endpoint come from one version and the key from another — so a host
// that answers differently on the second read could serve a record signed by a
// key the *first* document never named, and the verification would pass. One
// read, both halves, is what makes "the key that vouches for this answer is the
// key that named the answerer" true rather than merely likely.
//
// The PDS endpoint is returned UNVALIDATED as a target, exactly as
// ResolveEndpoint's is: a hostile document may publish any URL, and the caller
// dials it through the guard.
func (r ServiceResolver) ResolveAtprotoIdentity(ctx context.Context, did string) (*AtprotoIdentity, error) {
	doc, err := r.fetchAndDecode(ctx, did)
	if err != nil {
		return nil, err
	}
	endpoint, err := doc.serviceEndpoint(did, atprotoPDSFragment)
	if err != nil {
		return nil, err
	}
	key, err := doc.signingKey(did)
	if err != nil {
		return nil, err
	}
	return &AtprotoIdentity{DID: did, PDSEndpoint: endpoint, SigningKey: key}, nil
}

// signingKey reads the `#atproto` verification method as a public key.
//
// `publicKeyMultibase` carries the bare multibase (`z…`) that a `did:key:` is
// the prefixed spelling of — the exact inverse of MultibaseFromDIDKey, which
// is how this box publishes its own (BuildDIDWebDoc). Reusing ParsePublicDIDKey
// rather than adding a second multibase parser keeps one owner for the
// key-parsing rules.
func (d *didDocument) signingKey(did string) (atcrypto.PublicKey, error) {
	want := "#" + atprotoSigningFragment
	for _, vm := range d.VerificationMethod {
		// Relative (`#atproto`) and absolute (`did:…#atproto`) name the same
		// method, the same as a service id.
		if vm.ID != want && vm.ID != did+want {
			continue
		}
		mb := strings.TrimSpace(vm.PublicKeyMultibase)
		if mb == "" {
			return nil, fmt.Errorf("DID document for %s has an empty %s signing key", did, want)
		}
		key, err := ParsePublicDIDKey("did:key:" + mb)
		if err != nil {
			return nil, fmt.Errorf("DID document for %s publishes an unusable %s signing key: %w", did, want, err)
		}
		return key, nil
	}
	return nil, fmt.Errorf("DID document for %s publishes no %s verification method", did, want)
}

// ResolveEndpoint returns the `serviceEndpoint` URL that `did`'s document
// publishes under `#fragment`.
//
// The returned URL is UNVALIDATED as a target — see the security note at the
// top of this file. The caller dials it through the guard.
func (r ServiceResolver) ResolveEndpoint(ctx context.Context, did, fragment string) (string, error) {
	doc, err := r.fetchAndDecode(ctx, did)
	if err != nil {
		return "", err
	}
	return doc.serviceEndpoint(did, fragment)
}

// fetchAndDecode retrieves and decodes `did`'s document, refusing a substituted
// one. One owner for the substitution check: every reader of a DID document
// gets it, so a new reader cannot forget it.
//
// Substitution check: a directory or host that answers with a document for a
// DIFFERENT DID has either misrouted us or is trying to. Either way what it
// lists is not what we asked about.
func (r ServiceResolver) fetchAndDecode(ctx context.Context, did string) (*didDocument, error) {
	body, err := r.fetchDocument(ctx, did)
	if err != nil {
		return nil, err
	}
	var doc didDocument
	if err := json.Unmarshal(body, &doc); err != nil {
		return nil, fmt.Errorf("decode DID document for %s: %w", did, err)
	}
	if doc.ID != did {
		return nil, fmt.Errorf("DID document for %s declares id %q — refusing a substituted document", did, doc.ID)
	}
	return &doc, nil
}

// serviceEndpoint returns the `serviceEndpoint` URL published under #fragment.
func (d *didDocument) serviceEndpoint(did, fragment string) (string, error) {
	doc := d
	want := "#" + fragment
	for _, svc := range doc.Service {
		// The `id` may be relative (`#atproto_pds`) or absolute
		// (`did:web:x#atproto_pds`); both name the same service.
		if svc.ID == want || svc.ID == did+want {
			if strings.TrimSpace(svc.ServiceEndpoint) == "" {
				return "", fmt.Errorf("service %s%s has an empty serviceEndpoint", did, want)
			}
			// Well-formedness only — not policy. A URL we cannot parse cannot
			// be dialed at all, so this is the one thing worth failing on here;
			// whether the target is *permitted* is the guard's call.
			u, err := url.Parse(svc.ServiceEndpoint)
			if err != nil {
				return "", fmt.Errorf("service %s%s has an unparseable endpoint %q: %w", did, want, svc.ServiceEndpoint, err)
			}
			if u.Hostname() == "" {
				return "", fmt.Errorf("service %s%s endpoint %q has no host", did, want, svc.ServiceEndpoint)
			}
			return svc.ServiceEndpoint, nil
		}
	}
	return "", fmt.Errorf("DID document for %s publishes no service %q", did, want)
}

// fetchDocument retrieves the DID document, routing each method to the fetcher
// its trust level calls for (see the security note above).
func (r ServiceResolver) fetchDocument(ctx context.Context, did string) ([]byte, error) {
	switch {
	case strings.HasPrefix(did, "did:plc:"):
		if r.HTTP == nil {
			return nil, fmt.Errorf("no HTTP client for the PLC directory")
		}
		return fetchTrusted(ctx, r.HTTP, strings.TrimRight(r.DirectoryBaseURL, "/")+"/"+did)

	case strings.HasPrefix(did, "did:web:"):
		host := strings.TrimPrefix(did, "did:web:")
		// The did:web path form (`did:web:host:a:b`) maps colons to path
		// segments. Deliberately unsupported rather than approximated: the
		// mapping needs percent-decoding and traversal rejection per segment,
		// and no ATProto *service* uses it. A DEFERRED gap, never a policy —
		// refusing is what keeps a half-right decoder from becoming the bypass.
		if strings.Contains(host, ":") {
			return nil, fmt.Errorf("did:web path form (%s) is not supported", did)
		}
		if host == "" {
			return nil, fmt.Errorf("did:web with no host: %q", did)
		}
		if r.Guarded == nil {
			// No unguarded fallback, ever: the host below comes from the DID.
			return nil, fmt.Errorf("refusing to resolve %s: no guarded fetcher wired", did)
		}
		return r.Guarded.FetchDoc(ctx, "https://"+host+"/.well-known/did.json")

	default:
		return nil, fmt.Errorf("unsupported DID method in %q", did)
	}
}

// fetchTrusted GETs a document from a known host (the hard-coded directory).
func fetchTrusted(ctx context.Context, client *http.Client, url string) ([]byte, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return nil, fmt.Errorf("build DID document request: %w", err)
	}
	resp, err := client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("fetch DID document: %w", err)
	}
	defer func() { _ = resp.Body.Close() }()
	if resp.StatusCode/100 != 2 {
		return nil, fmt.Errorf("DID document fetch returned %d", resp.StatusCode)
	}
	return io.ReadAll(io.LimitReader(resp.Body, maxDIDDocBytes))
}
