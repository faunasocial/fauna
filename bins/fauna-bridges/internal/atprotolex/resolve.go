package atprotolex

import (
	"context"
	"fmt"
	"strings"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotoid"
)

// Resolving a Lexicon NSID to the DID that publishes it — the first leg of the
// permission-set resolution chain (`atproto-pds-full.md` § F4 detail's
// *Permission sets* → *The chain is authenticated end-to-end*).
//
// The chain is DNS TXT → DID → the record at that DID's PDS. This file is the
// DNS leg only: it answers "which DID speaks for this NSID's authority", and
// nothing here fetches a record or believes one.
//
// # Why DNS is the root of trust here, named rather than fixed
//
// The Lexicon publication system anchors an NSID's authority in DNS, the same
// class of root the ecosystem's handle resolution already uses. § F4 detail
// names that rather than pretending otherwise: a DNS answer is only as good as
// the resolver path, which is exactly why the *record* this DID leads to is
// verified (MST inclusion proof plus commit signature) instead of trusted for
// having come from the right host. Compromise the DNS and you can point an NSID
// at a DID you control; you still cannot make that DID's PDS produce a record
// the authority did not sign.

// lexiconTXTPrefix is the subdomain the Lexicon publication system reserves.
const lexiconTXTPrefix = "_lexicon."

// txtDIDPrefix is the only key a `_lexicon` TXT record carries.
const txtDIDPrefix = "did="

// AuthorityDomain derives the DNS authority that publishes an NSID.
//
// The authority is every segment of the NSID except the last, reversed:
//
//	com.example.calendar.appPerms  →  calendar.example.com
//
// The last segment is the *name* and belongs to the record key, not to DNS.
//
// Note this is the same split the pure expander's hierarchy constraint uses:
// the group it confines members to IS the authority domain that published the
// set. That is not a coincidence worth abstracting away — it is the constraint
// restated. A set may only widen within the namespace its own publisher owns,
// and "its own publisher" is precisely who answered this DNS query.
func AuthorityDomain(nsid string) (string, error) {
	segments := strings.Split(nsid, ".")
	if len(segments) < 3 {
		return "", fmt.Errorf("NSID %q has fewer than three segments", nsid)
	}
	authority := segments[:len(segments)-1]
	// Reverse in place on our own copy.
	reversed := make([]string, 0, len(authority))
	for i := len(authority) - 1; i >= 0; i-- {
		if authority[i] == "" {
			return "", fmt.Errorf("NSID %q has an empty segment", nsid)
		}
		reversed = append(reversed, authority[i])
	}
	return strings.Join(reversed, "."), nil
}

// ResolveAuthorityDID answers which DID publishes this NSID, by looking up the
// `_lexicon.<authority>` TXT record.
//
// # What this deliberately does NOT do
//
// It does not validate the NSID's syntax. The caller has already done that in
// the pure expander (`permission_set::parse_include_scope`), before any I/O, so
// that Go only ever fetches what that parser emitted — re-deriving the rule
// here would be a second owner for it, and the two would eventually disagree
// about which strings are safe to put in a DNS query. `AuthorityDomain` still
// refuses the shapes that would make a *query* meaningless (too few segments,
// an empty label), because building `_lexicon..example.com` is this function's
// own error, not the parser's.
//
// It also does not cache. § F4 detail puts the document cache one leg further
// in and the lexicon spec cautions specifically against caching DNS answers for
// long periods; the caller owns both lifetimes, so neither is invented here.
//
// # Exactly one answer, or none
//
// A `_lexicon` subdomain carrying two different `did=` records is not a
// resolution with a tie to break — it is an authority in an inconsistent state,
// and picking either one would mean this server silently chose which of two
// publishers speaks for the namespace. Both are refused. Records that are not
// `did=` at all are ignored rather than refused: TXT is a shared namespace and
// an unrelated record at the same name is ordinary, not hostile.
func ResolveAuthorityDID(ctx context.Context, resolver atprotoid.TXTResolver, nsid string) (string, error) {
	authority, err := AuthorityDomain(nsid)
	if err != nil {
		return "", err
	}
	name := lexiconTXTPrefix + authority

	records, err := resolver.LookupTXT(ctx, name)
	if err != nil {
		return "", fmt.Errorf("lexicon authority %s: TXT lookup failed: %w", name, err)
	}

	var found string
	for _, record := range records {
		// A long TXT record arrives as several strings the resolver has
		// already joined per-record; what it never does is join across
		// records, so each entry is considered whole.
		value, ok := strings.CutPrefix(strings.TrimSpace(record), txtDIDPrefix)
		if !ok {
			continue
		}
		value = strings.TrimSpace(value)
		if value == "" {
			continue
		}
		if found != "" && found != value {
			return "", fmt.Errorf(
				"lexicon authority %s publishes conflicting DIDs (%q and %q)",
				name, found, value)
		}
		found = value
	}

	if found == "" {
		return "", fmt.Errorf("lexicon authority %s publishes no did= TXT record", name)
	}
	if !strings.HasPrefix(found, "did:") {
		// Refused rather than passed on: the next leg would hand this string
		// to a DID resolver, and a value that is not a DID at all is a
		// misconfiguration worth naming here, where the record it came from is
		// still in hand.
		return "", fmt.Errorf("lexicon authority %s publishes %q, which is not a DID", name, found)
	}
	return found, nil
}
