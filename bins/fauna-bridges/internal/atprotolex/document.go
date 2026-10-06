package atprotolex

// The permission-set resolution chain, end to end: an NSID a client named in an
// `include:` scope → the verified bytes of the Lexicon document that defines it
// (docs/goal/behavior/atproto-pds-full.md § F4 detail → *Permission sets*).
//
// resolve.go is the first leg (DNS TXT → DID). This file is the rest: the DID's
// document (which PDS, whose key), the record fetch through the guard, and the
// verification that makes the answer believable.
//
// # What this file is NOT allowed to do
//
// It does not interpret the document. Everything about what a permission set
// MEANS — the `include:` grammar, the member types, the same-NSID hierarchy
// constraint, `inheritAud`, the ignore rules — belongs to the pure expander
// (`fauna-bridge-atproto::permission_set`), and § F4 detail's split is explicit:
// expansion is the pure module's, Go only ever fetches. So the return value is
// the record's dag-cbor bytes VERBATIM, exactly as the proof established them.
// Decoding them here to "check they look right" would create a second opinion
// about document shape and hand the expander bytes it did not verify.

import (
	"context"
	"fmt"
	"net/url"
	"strings"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotoid"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotorepo"
)

// LexiconSchemaCollection is where a published Lexicon document lives; the rkey
// is the full NSID (`at://<did>/com.atproto.lexicon.schema/<nsid>`).
const LexiconSchemaCollection = "com.atproto.lexicon.schema"

// The wiring these seams exist for, asserted at compile time so a signature
// change on either side is a build error here rather than a surprise at the
// production call site.
var (
	_ IdentityResolver      = atprotoid.ServiceResolver{}
	_ DocumentResolver      = SetResolver{}
	_ DocumentResolver      = (*CachingSetResolver)(nil)
	_ atprotoid.TXTResolver = (*CachingTXTResolver)(nil)
)

// getRecordPath is the read this chain performs. sync.getRecord rather than
// repo.getRecord deliberately: repo.getRecord answers with the record alone,
// which is the host's unsupported word for it, while sync.getRecord answers
// with a proof CAR — the signed commit plus the MST path — which is the only
// form that can be verified against the authority's key.
const getRecordPath = "/xrpc/com.atproto.sync.getRecord"

// IdentityResolver resolves a DID to the PDS that serves its repo and the key
// that signs it. atprotoid.ServiceResolver satisfies it.
type IdentityResolver interface {
	ResolveAtprotoIdentity(ctx context.Context, did string) (*atprotoid.AtprotoIdentity, error)
}

// ProofFetcher fetches a getRecord proof CAR. The URL is derived from a
// third-party DID document, so the production wiring is the SSRF-guarded
// fetcher and the seam exists so tests can drive both the allow and refuse
// paths without a network.
type ProofFetcher interface {
	FetchProof(ctx context.Context, url string) ([]byte, error)
}

// SetResolver holds the three seams the chain crosses. Every one is required;
// a nil seam refuses rather than falling back, following atprotoid's rule —
// the hosts below all come from attacker-influenced data, so an unguarded
// fallback is the bypass, not the convenience.
type SetResolver struct {
	TXT      atprotoid.TXTResolver
	Identity IdentityResolver
	Fetch    ProofFetcher
}

// ResolveSetDocument answers with the verified bytes of the permission-set
// document published for nsid, or an error naming the leg that failed.
//
// # The chain, and why each leg refuses instead of degrading
//
//  1. DNS TXT `_lexicon.<reversed-authority>` → the publishing DID. DNS is the
//     ecosystem's root of trust for NSIDs, named rather than pretended away
//     (resolve.go's header).
//  2. That DID's document → the PDS endpoint and the `#atproto` signing key,
//     read in ONE fetch so the two describe the same document.
//  3. `com.atproto.sync.getRecord` at that PDS, through the guard.
//  4. VerifyRecordProof: the commit is signed by the key from step 2, the repo
//     is the DID from step 1, and the MST path establishes exactly the rkey we
//     asked for.
//
// There is no partial answer to fall back to. § F4 detail: a set that cannot be
// resolved fails the whole authorization request at PAR, before anything is
// stored — a partial grant would mean the user consents to a card missing the
// set the client asked for while the client believes it got it.
//
// # The caller's contract
//
// nsid MUST already have passed the pure parser (`parse_include_scope`), which
// validates NSID syntax before any I/O. This function does not re-derive that
// rule — a second owner for it would eventually disagree with the first about
// which strings are safe to put in a DNS query — but AuthorityDomain still
// refuses shapes that would make a *query* meaningless.
//
// Caching is the caller's: § F4 detail puts the document cache above this
// (24 h stale / 90 d expiry / stale-on-failure, short negative TTL), and it
// caches this function's verified output. Nothing here holds state.
func (r SetResolver) ResolveSetDocument(ctx context.Context, nsid string) ([]byte, error) {
	if r.TXT == nil {
		return nil, fmt.Errorf("refusing to resolve %s: no TXT resolver wired", nsid)
	}
	if r.Identity == nil {
		return nil, fmt.Errorf("refusing to resolve %s: no identity resolver wired", nsid)
	}
	if r.Fetch == nil {
		// No unguarded fallback, ever: the host below comes from a DID document.
		return nil, fmt.Errorf("refusing to resolve %s: no guarded fetcher wired", nsid)
	}

	did, err := ResolveAuthorityDID(ctx, r.TXT, nsid)
	if err != nil {
		return nil, fmt.Errorf("permission set %s: %w", nsid, err)
	}

	identity, err := r.Identity.ResolveAtprotoIdentity(ctx, did)
	if err != nil {
		return nil, fmt.Errorf("permission set %s: authority %s does not resolve: %w", nsid, did, err)
	}

	recordURL, err := getRecordURL(identity.PDSEndpoint, did, nsid)
	if err != nil {
		return nil, fmt.Errorf("permission set %s: %w", nsid, err)
	}

	proof, err := r.Fetch.FetchProof(ctx, recordURL)
	if err != nil {
		return nil, fmt.Errorf("permission set %s: fetching the document from %s failed: %w", nsid, did, err)
	}

	// The rkey asserted here is nsid, not whatever the payload happens to hold:
	// a hostile host answering with a real, correctly-signed proof for a
	// DIFFERENT record in the same repo is otherwise indistinguishable from an
	// honest answer, and would let one authority serve any set's permissions
	// under any set's name.
	record, err := atprotorepo.VerifyRecordProof(ctx, proof, did, LexiconSchemaCollection, nsid, identity.SigningKey)
	if err != nil {
		return nil, fmt.Errorf("permission set %s: %w", nsid, err)
	}
	return record, nil
}

// getRecordURL builds the sync.getRecord request against the PDS the DID
// document named.
//
// The endpoint is attacker-controlled, so it is parsed rather than concatenated
// into: a value like `https://pds.example/?x=` would otherwise let the document
// author decide where our query parameters land. Query values go through
// url.Values so an NSID or DID can never break out of its parameter either —
// belt and braces, since both were validated upstream.
func getRecordURL(endpoint, did, nsid string) (string, error) {
	u, err := url.Parse(strings.TrimSpace(endpoint))
	if err != nil {
		return "", fmt.Errorf("authority PDS endpoint %q is unparseable: %w", endpoint, err)
	}
	if u.Scheme != "https" {
		// The guard would refuse most of what this excludes, but the scheme is
		// this chain's own requirement: a published Lexicon authority reached
		// over plaintext is not an authenticated chain, whatever the guard
		// thinks of the host.
		return "", fmt.Errorf("authority PDS endpoint %q is not https", endpoint)
	}
	if u.Hostname() == "" {
		return "", fmt.Errorf("authority PDS endpoint %q has no host", endpoint)
	}
	u.Path = strings.TrimSuffix(u.Path, "/") + getRecordPath
	u.RawQuery = url.Values{
		"did":        {did},
		"collection": {LexiconSchemaCollection},
		"rkey":       {nsid},
	}.Encode()
	u.Fragment = ""
	return u.String(), nil
}
