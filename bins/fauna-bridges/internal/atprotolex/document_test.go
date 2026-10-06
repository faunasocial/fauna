package atprotolex

import (
	"context"
	"errors"
	"net/url"
	"strings"
	"testing"

	"github.com/bluesky-social/indigo/atproto/atcrypto"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotoid"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotorepo"
)

// --- seams -------------------------------------------------------------

// fakeTXT answers `_lexicon.<authority>` lookups from a map.
type fakeTXT struct {
	records map[string][]string
	err     error
}

func (f fakeTXT) LookupTXT(_ context.Context, name string) ([]string, error) {
	if f.err != nil {
		return nil, f.err
	}
	r, ok := f.records[name]
	if !ok {
		return nil, errors.New("NXDOMAIN")
	}
	return r, nil
}

// fakeIdentity answers DID-document resolution from a map.
type fakeIdentity struct {
	ids map[string]*atprotoid.AtprotoIdentity
	err error
}

func (f fakeIdentity) ResolveAtprotoIdentity(_ context.Context, did string) (*atprotoid.AtprotoIdentity, error) {
	if f.err != nil {
		return nil, f.err
	}
	id, ok := f.ids[did]
	if !ok {
		return nil, errors.New("no such DID")
	}
	return id, nil
}

// fakeFetch stands in for the SSRF-guarded fetcher, recording every URL.
type fakeFetch struct {
	body  []byte
	err   error
	asked []string
}

func (f *fakeFetch) FetchProof(_ context.Context, rawURL string) ([]byte, error) {
	f.asked = append(f.asked, rawURL)
	if f.err != nil {
		return nil, f.err
	}
	return f.body, nil
}

// --- fixture -----------------------------------------------------------

// publishSet builds a REAL repo holding a com.atproto.lexicon.schema record at
// rkey == nsid, and returns its getRecord proof CAR plus the signing key's
// public half and the record's stored bytes.
//
// A real repo rather than a canned blob: the whole slice's claim is that an
// honest publication verifies and everything else does not, and a hand-written
// CAR would prove only that the verifier accepts what the test author expected.
func publishSet(t *testing.T, did, nsid string, recordJSON string) (proof []byte, pub atcrypto.PublicKey, record []byte) {
	t.Helper()
	ctx := context.Background()

	st, err := atprotorepo.Open(":memory:")
	if err != nil {
		t.Fatalf("open store: %v", err)
	}
	t.Cleanup(func() { st.Close() })
	f, err := atprotorepo.NewFunnel(ctx, st, nil)
	if err != nil {
		t.Fatalf("new funnel: %v", err)
	}
	key, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	pub, _ = key.PublicKey()

	if err := st.SetFirstEmitGated(ctx, did, false); err != nil {
		t.Fatalf("open first-emit gate: %v", err)
	}
	cbor, err := atprotorepo.JSONRecordToDagCBOR(recordJSON)
	if err != nil {
		t.Fatalf("encode set record: %v", err)
	}
	if _, err := f.ApplyBatch(ctx, did, key, []atprotorepo.RepoOp{{
		Action:     atprotorepo.ActionCreate,
		Collection: LexiconSchemaCollection,
		Rkey:       nsid,
		RecordCBOR: cbor,
	}}); err != nil {
		t.Fatalf("publish set record: %v", err)
	}

	_, record, ok, err := st.GetRecord(ctx, did, LexiconSchemaCollection, nsid)
	if err != nil || !ok {
		t.Fatalf("published record missing: ok=%v err=%v", ok, err)
	}
	proof, err = st.ExportRecordProof(ctx, did, LexiconSchemaCollection, nsid)
	if err != nil {
		t.Fatalf("ExportRecordProof: %v", err)
	}
	return proof, pub, record
}

const testSetNSID = "com.example.calendar.appPerms"
const testSetJSON = `{"$type":"com.atproto.lexicon.schema","lexicon":1,` +
	`"id":"com.example.calendar.appPerms","title":"Calendar access"}`

// honestChain wires a resolver whose every leg answers correctly.
func honestChain(t *testing.T) (SetResolver, *fakeFetch, []byte) {
	t.Helper()
	const did = "did:plc:lexauthority00000000000"
	proof, pub, record := publishSet(t, did, testSetNSID, testSetJSON)
	fetch := &fakeFetch{body: proof}
	return SetResolver{
		TXT: fakeTXT{records: map[string][]string{"_lexicon.calendar.example.com": {"did=" + did}}},
		Identity: fakeIdentity{ids: map[string]*atprotoid.AtprotoIdentity{
			did: {DID: did, PDSEndpoint: "https://pds.example.com", SigningKey: pub},
		}},
		Fetch: fetch,
	}, fetch, record
}

// --- tests -------------------------------------------------------------

// TestResolveSetDocumentHonestChain walks the whole chain: NSID → DNS TXT →
// DID → the DID document's PDS + signing key → the authenticated record, and
// hands back the VERIFIED dag-cbor bytes verbatim (what the pure expander is
// contractually owed — a JSON round-trip in between would reopen the gap the
// verification closes).
func TestResolveSetDocumentHonestChain(t *testing.T) {
	r, fetch, want := honestChain(t)

	got, err := r.ResolveSetDocument(context.Background(), testSetNSID)
	if err != nil {
		t.Fatalf("resolve: %v", err)
	}
	if string(got) != string(want) {
		t.Errorf("record bytes differ from the published record (%d vs %d bytes)", len(got), len(want))
	}

	// The fetch must name the record precisely: this DID's repo, the lexicon
	// schema collection, rkey = the FULL NSID.
	if len(fetch.asked) != 1 {
		t.Fatalf("want exactly one fetch, got %v", fetch.asked)
	}
	u, err := url.Parse(fetch.asked[0])
	if err != nil {
		t.Fatalf("the chain built an unparseable URL %q: %v", fetch.asked[0], err)
	}
	if u.Host != "pds.example.com" {
		t.Errorf("fetched from %q, want the PDS the DID document named", u.Host)
	}
	if !strings.HasSuffix(u.Path, "/xrpc/com.atproto.sync.getRecord") {
		t.Errorf("path %q is not the getRecord endpoint", u.Path)
	}
	q := u.Query()
	if q.Get("collection") != LexiconSchemaCollection {
		t.Errorf("collection: got %q", q.Get("collection"))
	}
	if q.Get("rkey") != testSetNSID {
		t.Errorf("rkey: got %q, want the full NSID", q.Get("rkey"))
	}
	if !strings.HasPrefix(q.Get("did"), "did:plc:") {
		t.Errorf("did: got %q", q.Get("did"))
	}
}

// TestResolveSetDocumentRefusalArms — a permission set widens an OAuth grant,
// so every leg that cannot be established end-to-end is a failed resolution.
// § F4 detail: failure at PAR fails the whole request, before anything is
// stored; there is no partial answer to degrade to.
func TestResolveSetDocumentRefusalArms(t *testing.T) {
	ctx := context.Background()

	t.Run("DNS does not answer", func(t *testing.T) {
		r, fetch, _ := honestChain(t)
		r.TXT = fakeTXT{err: errors.New("SERVFAIL")}
		if _, err := r.ResolveSetDocument(ctx, testSetNSID); err == nil {
			t.Fatal("want a refusal when the authority does not resolve")
		}
		if len(fetch.asked) != 0 {
			t.Errorf("nothing should be fetched once the chain has already failed: %v", fetch.asked)
		}
	})

	t.Run("the DID does not resolve", func(t *testing.T) {
		r, fetch, _ := honestChain(t)
		r.Identity = fakeIdentity{err: errors.New("directory down")}
		if _, err := r.ResolveSetDocument(ctx, testSetNSID); err == nil {
			t.Fatal("want a refusal when the DID document does not resolve")
		}
		if len(fetch.asked) != 0 {
			t.Errorf("the record must not be fetched before its authority is known: %v", fetch.asked)
		}
	})

	t.Run("the guard refuses the PDS", func(t *testing.T) {
		// The PDS endpoint comes from an attacker-influenced document, so the
		// guard's no is the answer — never a retry by another route.
		r, _, _ := honestChain(t)
		r.Fetch = &fakeFetch{err: errors.New("safefetch: denied")}
		if _, err := r.ResolveSetDocument(ctx, testSetNSID); err == nil {
			t.Fatal("want the guard's refusal to propagate")
		}
	})

	t.Run("a proof for a DIFFERENT set", func(t *testing.T) {
		// The load-bearing one. A hostile PDS answers with a real, correctly
		// signed proof — for another record in the same repo. Only the rkey
		// check distinguishes it, and without it an authority could serve any
		// set's permissions under any set's name.
		const did = "did:plc:lexauthority00000000000"
		proof, pub, _ := publishSet(t, did, "com.example.calendar.otherPerms", testSetJSON)
		r := SetResolver{
			TXT: fakeTXT{records: map[string][]string{"_lexicon.calendar.example.com": {"did=" + did}}},
			Identity: fakeIdentity{ids: map[string]*atprotoid.AtprotoIdentity{
				did: {DID: did, PDSEndpoint: "https://pds.example.com", SigningKey: pub},
			}},
			Fetch: &fakeFetch{body: proof},
		}
		if _, err := r.ResolveSetDocument(ctx, testSetNSID); err == nil {
			t.Fatal("want a refusal when the proof establishes a different rkey")
		}
	})

	t.Run("a proof signed by another key", func(t *testing.T) {
		// The DID document's key is the authority; a repo signed by anything
		// else is not this authority speaking.
		r, _, _ := honestChain(t)
		other, err := atcrypto.GeneratePrivateKeyK256()
		if err != nil {
			t.Fatal(err)
		}
		otherPub, _ := other.PublicKey()
		const did = "did:plc:lexauthority00000000000"
		r.Identity = fakeIdentity{ids: map[string]*atprotoid.AtprotoIdentity{
			did: {DID: did, PDSEndpoint: "https://pds.example.com", SigningKey: otherPub},
		}}
		if _, err := r.ResolveSetDocument(ctx, testSetNSID); err == nil {
			t.Fatal("want a refusal when the commit is signed by a key the DID document does not name")
		}
	})

	t.Run("an unwired seam refuses rather than falling back", func(t *testing.T) {
		// No unguarded fallback, ever — the atprotoid precedent. A missing seam
		// is a wiring bug that must be loud, not a quiet plain-HTTP fetch.
		r, _, _ := honestChain(t)
		r.Fetch = nil
		if _, err := r.ResolveSetDocument(ctx, testSetNSID); err == nil {
			t.Fatal("want a refusal when no guarded fetcher is wired")
		}
	})

	t.Run("a syntactically impossible authority", func(t *testing.T) {
		r, fetch, _ := honestChain(t)
		if _, err := r.ResolveSetDocument(ctx, "tooshort"); err == nil {
			t.Fatal("want a refusal for an NSID with no derivable authority")
		}
		if len(fetch.asked) != 0 {
			t.Errorf("nothing should be fetched for an underivable authority: %v", fetch.asked)
		}
	})
}
