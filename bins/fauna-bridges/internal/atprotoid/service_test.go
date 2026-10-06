package atprotoid

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/bluesky-social/indigo/atproto/atcrypto"
)

func TestParseServiceRef(t *testing.T) {
	for _, tc := range []struct {
		name, in      string
		did, fragment string
		wantErr       bool
	}{
		{name: "appview", in: "did:web:api.bsky.app#bsky_appview", did: "did:web:api.bsky.app", fragment: "bsky_appview"},
		{name: "plc pds", in: "did:plc:abc123#atproto_pds", did: "did:plc:abc123", fragment: "atproto_pds"},
		{name: "surrounding space", in: "  did:web:x.example#svc  ", did: "did:web:x.example", fragment: "svc"},
		// A bare DID names an identity, not a service — picking one of its
		// services would be inventing an intent the caller never expressed.
		{name: "no fragment", in: "did:web:api.bsky.app", wantErr: true},
		{name: "empty fragment", in: "did:web:api.bsky.app#", wantErr: true},
		{name: "empty did", in: "#bsky_appview", wantErr: true},
		{name: "not a did", in: "https://evil.example#svc", wantErr: true},
		{name: "empty", in: "", wantErr: true},
		{name: "blank", in: "   ", wantErr: true},
		// Ambiguity in a security-relevant selector is a parser disagreement
		// waiting to happen.
		{name: "two fragments", in: "did:web:x.example#a#b", wantErr: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			did, fragment, err := ParseServiceRef(tc.in)
			if tc.wantErr {
				if err == nil {
					t.Fatalf("want an error, got did=%q fragment=%q", did, fragment)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if did != tc.did || fragment != tc.fragment {
				t.Errorf("got (%q, %q), want (%q, %q)", did, fragment, tc.did, tc.fragment)
			}
		})
	}
}

// fakeGuarded records what it was asked to fetch and answers from a map. It
// stands in for the SSRF-guarded fetcher; `refuse` models the guard saying no.
type fakeGuarded struct {
	docs   map[string]string
	asked  []string
	refuse error
}

func (f *fakeGuarded) FetchDoc(_ context.Context, url string) ([]byte, error) {
	f.asked = append(f.asked, url)
	if f.refuse != nil {
		return nil, f.refuse
	}
	body, ok := f.docs[url]
	if !ok {
		return nil, http.ErrMissingFile
	}
	return []byte(body), nil
}

func didDocJSON(t *testing.T, id string, services ...map[string]string) string {
	t.Helper()
	svc := make([]any, 0, len(services))
	for _, s := range services {
		svc = append(svc, map[string]any{
			"id":              s["id"],
			"type":            s["type"],
			"serviceEndpoint": s["serviceEndpoint"],
		})
	}
	b, err := json.Marshal(map[string]any{"id": id, "service": svc})
	if err != nil {
		t.Fatalf("marshal doc: %v", err)
	}
	return string(b)
}

// TestResolveEndpointDidWebGoesThroughTheGuard is the load-bearing one: a
// did:web NAMES A HOST, so `did:web:169.254.169.254` would aim the bridge at
// cloud IMDS. The fetch must go through the guarded fetcher — and there must be
// no unguarded fallback when none is wired.
func TestResolveEndpointDidWebGoesThroughTheGuard(t *testing.T) {
	const did = "did:web:api.bsky.app"
	guarded := &fakeGuarded{docs: map[string]string{
		"https://api.bsky.app/.well-known/did.json": didDocJSON(t, did, map[string]string{
			"id": "#bsky_appview", "type": "BskyAppView", "serviceEndpoint": "https://api.bsky.app",
		}),
	}}
	r := ServiceResolver{Guarded: guarded}

	got, err := r.ResolveEndpoint(context.Background(), did, "bsky_appview")
	if err != nil {
		t.Fatalf("resolve: %v", err)
	}
	if got != "https://api.bsky.app" {
		t.Errorf("endpoint: got %q", got)
	}
	if len(guarded.asked) != 1 || guarded.asked[0] != "https://api.bsky.app/.well-known/did.json" {
		t.Errorf("the did:web document was not fetched through the guard: %v", guarded.asked)
	}
}

// TestResolveEndpointRefusesDidWebWithNoGuard — the seam is not optional. With
// no guarded fetcher wired, resolution must refuse rather than reach for a
// plain client, because the host below comes from the DID.
func TestResolveEndpointRefusesDidWebWithNoGuard(t *testing.T) {
	r := ServiceResolver{HTTP: http.DefaultClient} // plain client present, guard absent
	_, err := r.ResolveEndpoint(context.Background(), "did:web:169.254.169.254", "svc")
	if err == nil {
		t.Fatal("want a refusal when no guarded fetcher is wired")
	}
	if !strings.Contains(err.Error(), "guarded") {
		t.Errorf("the refusal should name the missing guard, got %v", err)
	}
}

// TestResolveEndpointPropagatesAGuardRefusal — when the guard says no, that is
// the answer; resolution must not retry by another route.
func TestResolveEndpointPropagatesAGuardRefusal(t *testing.T) {
	guarded := &fakeGuarded{refuse: http.ErrNotSupported}
	r := ServiceResolver{Guarded: guarded, HTTP: http.DefaultClient}
	if _, err := r.ResolveEndpoint(context.Background(), "did:web:internal.example", "svc"); err == nil {
		t.Fatal("want the guard's refusal to propagate")
	}
	if len(guarded.asked) != 1 {
		t.Errorf("want exactly one guarded attempt, got %d", len(guarded.asked))
	}
}

// TestResolveEndpointDidPlcUsesTheHardCodedDirectory — a did:plc names nothing:
// the host is a hard-coded constant and the DID is only a path component, so
// this arm is an ordinary request to a known host (the treatment FetchLastOp
// and SubmitOperation already give it) and must NOT consume the guard.
func TestResolveEndpointDidPlcUsesTheHardCodedDirectory(t *testing.T) {
	const did = "did:plc:abc123"
	var gotPath string
	dir := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		gotPath = req.URL.Path
		_, _ = w.Write([]byte(didDocJSON(t, did, map[string]string{
			"id": did + "#atproto_pds", "type": "AtprotoPersonalDataServer", "serviceEndpoint": "https://pds.example.com",
		})))
	}))
	defer dir.Close()

	guarded := &fakeGuarded{}
	r := ServiceResolver{Guarded: guarded, HTTP: dir.Client(), DirectoryBaseURL: dir.URL}

	got, err := r.ResolveEndpoint(context.Background(), did, "atproto_pds")
	if err != nil {
		t.Fatalf("resolve: %v", err)
	}
	if got != "https://pds.example.com" {
		t.Errorf("endpoint: got %q", got)
	}
	if gotPath != "/"+did {
		t.Errorf("directory path: got %q, want %q", gotPath, "/"+did)
	}
	if len(guarded.asked) != 0 {
		t.Errorf("the did:plc arm must not consume the guarded fetcher, asked: %v", guarded.asked)
	}
}

// TestResolveEndpointRefusesASubstitutedDocument — a directory or host that
// answers with a document for a DIFFERENT DID has misrouted us or is trying to;
// either way the services it lists are not the ones we asked about.
func TestResolveEndpointRefusesASubstitutedDocument(t *testing.T) {
	guarded := &fakeGuarded{docs: map[string]string{
		"https://victim.example/.well-known/did.json": didDocJSON(t, "did:web:attacker.example", map[string]string{
			"id": "#svc", "type": "X", "serviceEndpoint": "https://attacker.example",
		}),
	}}
	r := ServiceResolver{Guarded: guarded}
	_, err := r.ResolveEndpoint(context.Background(), "did:web:victim.example", "svc")
	if err == nil {
		t.Fatal("want a refusal for a document declaring a different id")
	}
	if !strings.Contains(err.Error(), "substituted") {
		t.Errorf("error should name the substitution, got %v", err)
	}
}

func TestResolveEndpointServiceSelection(t *testing.T) {
	const did = "did:web:x.example"
	docURL := "https://x.example/.well-known/did.json"

	for _, tc := range []struct {
		name     string
		doc      string
		fragment string
		want     string
		wantErr  string
	}{
		{
			name:     "absolute service id",
			doc:      didDocJSON(t, did, map[string]string{"id": did + "#atproto_pds", "serviceEndpoint": "https://a.example"}),
			fragment: "atproto_pds",
			want:     "https://a.example",
		},
		{
			name:     "relative service id",
			doc:      didDocJSON(t, did, map[string]string{"id": "#atproto_pds", "serviceEndpoint": "https://b.example"}),
			fragment: "atproto_pds",
			want:     "https://b.example",
		},
		{
			name: "picks the requested one among several",
			doc: didDocJSON(t, did,
				map[string]string{"id": "#other", "serviceEndpoint": "https://wrong.example"},
				map[string]string{"id": "#wanted", "serviceEndpoint": "https://right.example"}),
			fragment: "wanted",
			want:     "https://right.example",
		},
		{
			name:     "fragment not published",
			doc:      didDocJSON(t, did, map[string]string{"id": "#other", "serviceEndpoint": "https://x.example"}),
			fragment: "missing",
			wantErr:  "publishes no service",
		},
		{
			name:     "empty endpoint",
			doc:      didDocJSON(t, did, map[string]string{"id": "#svc", "serviceEndpoint": ""}),
			fragment: "svc",
			wantErr:  "empty serviceEndpoint",
		},
		{
			name:     "endpoint with no host",
			doc:      didDocJSON(t, did, map[string]string{"id": "#svc", "serviceEndpoint": "not-a-url"}),
			fragment: "svc",
			wantErr:  "no host",
		},
		{
			// DID-core allows an object/array serviceEndpoint; ATProto does
			// not. Failing to decode is the correct fail-closed outcome rather
			// than a guess about which entry was meant.
			name:     "object-form serviceEndpoint fails closed",
			doc:      `{"id":"` + did + `","service":[{"id":"#svc","serviceEndpoint":{"uri":"https://x.example"}}]}`,
			fragment: "svc",
			wantErr:  "decode DID document",
		},
		{
			name:     "no service array at all",
			doc:      `{"id":"` + did + `"}`,
			fragment: "svc",
			wantErr:  "publishes no service",
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			r := ServiceResolver{Guarded: &fakeGuarded{docs: map[string]string{docURL: tc.doc}}}
			got, err := r.ResolveEndpoint(context.Background(), did, tc.fragment)
			if tc.wantErr != "" {
				if err == nil {
					t.Fatalf("want an error containing %q, got %q", tc.wantErr, got)
				}
				if !strings.Contains(err.Error(), tc.wantErr) {
					t.Errorf("error %q does not contain %q", err, tc.wantErr)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if got != tc.want {
				t.Errorf("got %q, want %q", got, tc.want)
			}
		})
	}
}

// TestResolveEndpointRefusesUnsupportedDIDForms — fail closed on anything the
// resolver does not actually implement, rather than approximating it.
func TestResolveEndpointRefusesUnsupportedDIDForms(t *testing.T) {
	r := ServiceResolver{Guarded: &fakeGuarded{}, HTTP: http.DefaultClient, DirectoryBaseURL: "https://plc.directory"}
	for _, tc := range []struct{ name, did, want string }{
		{"unknown method", "did:key:z6Mk", "unsupported DID method"},
		// The path form needs per-segment percent-decoding and traversal
		// rejection; a half-right decoder is exactly how a bypass gets in.
		{"did:web path form", "did:web:example.com:user:alice", "path form"},
		{"did:web with no host", "did:web:", "no host"},
		{"not a did", "https://evil.example", "unsupported DID method"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			_, err := r.ResolveEndpoint(context.Background(), tc.did, "svc")
			if err == nil {
				t.Fatal("want a refusal")
			}
			if !strings.Contains(err.Error(), tc.want) {
				t.Errorf("error %q does not contain %q", err, tc.want)
			}
		})
	}
}

// identityDocJSON renders a DID document carrying both halves an ATProto
// identity publishes: the `#atproto` Multikey verification method and the
// `#atproto_pds` service. Mirrors BuildDIDWebDoc's shape, which is what this
// box itself serves.
func identityDocJSON(t *testing.T, id, signingMultibase, pdsEndpoint string) string {
	t.Helper()
	b, err := json.Marshal(map[string]any{
		"id": id,
		"verificationMethod": []any{map[string]any{
			"id": id + "#atproto", "type": "Multikey",
			"controller": id, "publicKeyMultibase": signingMultibase,
		}},
		"service": []any{map[string]any{
			"id": "#atproto_pds", "type": "AtprotoPersonalDataServer",
			"serviceEndpoint": pdsEndpoint,
		}},
	})
	if err != nil {
		t.Fatalf("marshal identity doc: %v", err)
	}
	return string(b)
}

// testSigningKey generates a k256 keypair and returns it with the bare public
// multibase a DID document's publicKeyMultibase field carries.
func testSigningKey(t *testing.T) (atcrypto.PrivateKeyExportable, string) {
	t.Helper()
	priv, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatalf("GeneratePrivateKeyK256: %v", err)
	}
	didKey, err := DIDKeyForPrivate(priv)
	if err != nil {
		t.Fatalf("DIDKeyForPrivate: %v", err)
	}
	mb, err := MultibaseFromDIDKey(didKey)
	if err != nil {
		t.Fatalf("MultibaseFromDIDKey: %v", err)
	}
	return priv, mb
}

// TestResolveAtprotoIdentityReadsBothHalves — the permission-set resolution
// chain needs the PDS endpoint (where to ask) and the signing key (whose
// answer to believe), and it needs them to describe the SAME document.
func TestResolveAtprotoIdentityReadsBothHalves(t *testing.T) {
	const did = "did:web:lex.example.com"
	priv, mb := testSigningKey(t)
	guarded := &fakeGuarded{docs: map[string]string{
		"https://lex.example.com/.well-known/did.json": identityDocJSON(t, did, mb, "https://pds.example.com"),
	}}
	r := ServiceResolver{Guarded: guarded}

	got, err := r.ResolveAtprotoIdentity(context.Background(), did)
	if err != nil {
		t.Fatalf("resolve identity: %v", err)
	}
	if got.DID != did {
		t.Errorf("DID: got %q want %q", got.DID, did)
	}
	if got.PDSEndpoint != "https://pds.example.com" {
		t.Errorf("PDS endpoint: got %q", got.PDSEndpoint)
	}
	wantPub, err := priv.PublicKey()
	if err != nil {
		t.Fatalf("PublicKey: %v", err)
	}
	if got.SigningKey == nil || got.SigningKey.DIDKey() != wantPub.DIDKey() {
		t.Errorf("signing key: got %v want %s", got.SigningKey, wantPub.DIDKey())
	}
	// ONE fetch. Two would let the key and the endpoint come from two
	// different reads of a mutable document — precisely the substitution the
	// verification exists to stop.
	if len(guarded.asked) != 1 {
		t.Errorf("want exactly one document fetch, got %d: %v", len(guarded.asked), guarded.asked)
	}
}

// TestResolveAtprotoIdentityRefusalArms — every way a document can fail to
// name a usable ATProto identity is a refusal, never a partial answer.
func TestResolveAtprotoIdentityRefusalArms(t *testing.T) {
	const did = "did:web:lex.example.com"
	const docURL = "https://lex.example.com/.well-known/did.json"
	_, mb := testSigningKey(t)

	for _, tc := range []struct{ name, doc, want string }{
		{
			// A document for a different DID has misrouted us or is trying to.
			name: "substituted document",
			doc:  identityDocJSON(t, "did:web:evil.example", mb, "https://pds.example.com"),
			want: "substituted",
		},
		{
			name: "no atproto verification method",
			doc: `{"id":"` + did + `","verificationMethod":[{"id":"` + did + `#other",` +
				`"type":"Multikey","publicKeyMultibase":"` + mb + `"}],` +
				`"service":[{"id":"#atproto_pds","type":"AtprotoPersonalDataServer",` +
				`"serviceEndpoint":"https://pds.example.com"}]}`,
			want: "no #atproto verification method",
		},
		{
			// An unparseable key is not a key. Continuing with a nil one would
			// make the signature check vacuous.
			name: "unparseable signing key",
			doc:  identityDocJSON(t, did, "znot-a-real-multibase", "https://pds.example.com"),
			want: "signing key",
		},
		{
			name: "no pds service",
			doc: `{"id":"` + did + `","verificationMethod":[{"id":"` + did + `#atproto",` +
				`"type":"Multikey","publicKeyMultibase":"` + mb + `"}],"service":[]}`,
			want: "atproto_pds",
		},
		{
			name: "empty pds endpoint",
			doc:  identityDocJSON(t, did, mb, ""),
			want: "atproto_pds",
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			r := ServiceResolver{Guarded: &fakeGuarded{docs: map[string]string{docURL: tc.doc}}}
			got, err := r.ResolveAtprotoIdentity(context.Background(), did)
			if err == nil {
				t.Fatalf("want a refusal, got %+v", got)
			}
			if !strings.Contains(err.Error(), tc.want) {
				t.Errorf("error %q does not contain %q", err, tc.want)
			}
		})
	}
}
