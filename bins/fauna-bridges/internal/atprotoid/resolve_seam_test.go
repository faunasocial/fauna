//go:build fauna_e2e_fixtures

// The e2e-flavor half of the fake-DNS seam's tests: these exercise the seam
// itself, so they compile only where the seam does. Run them with
// `go test -tags fauna_e2e_fixtures ./internal/atprotoid/` (`just
// atproto-bridge-test` does). Their production twin is
// resolve_seam_absent_test.go.
package atprotoid

import (
	"context"
	"fmt"
	"net"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestTXTResolverFromEnvDefaultsToSystemResolver(t *testing.T) {
	t.Setenv(fakeTXTEnv, "")
	if got := TXTResolverFromEnv(); got != TXTResolver(net.DefaultResolver) {
		t.Fatalf("unset env must yield the real system resolver, got %T", got)
	}
}

func TestTXTResolverFromEnvServesRecordsAndNXDOMAIN(t *testing.T) {
	srv := fakeDNSServer(t, map[string][]string{
		"_atproto.alice.example.com": {"did=did:plc:abc"},
		"_atproto.bob.example.com":   {},
	})
	t.Setenv(fakeTXTEnv, srv.URL+"/")
	r := TXTResolverFromEnv()

	got, err := r.LookupTXT(context.Background(), "_atproto.alice.example.com")
	if err != nil {
		t.Fatalf("canned lookup: %v", err)
	}
	if len(got) != 1 || got[0] != "did=did:plc:abc" {
		t.Fatalf("records = %#v, want [did=did:plc:abc]", got)
	}

	// A name the fake does not carry, and a name with an empty answer, must
	// both look like NXDOMAIN — a lookup ERROR, not an empty success.
	if _, err := r.LookupTXT(context.Background(), "_atproto.nobody.example.com"); err == nil {
		t.Fatal("unknown name must return an error, got nil")
	}
	if _, err := r.LookupTXT(context.Background(), "_atproto.bob.example.com"); err == nil {
		t.Fatal("explicitly-empty name must return an error, got nil")
	}
}

// The seam must satisfy the REAL gate end to end, and must still BLOCK the gate
// when the record is absent or names another DID. This is the property the
// tier_3 firehose test leans on, including its deferred-then-resolvable retry.
func TestTXTResolverFromEnvGatesRatherThanBypasses(t *testing.T) {
	const did = "did:plc:aaaabbbbccccddddeeeeffff"
	const handle = "alice.example.com"
	plc := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/"+did {
			w.WriteHeader(http.StatusNotFound)
			return
		}
		fmt.Fprintln(w, `{"id":"`+did+`"}`)
	}))
	defer plc.Close()

	table := map[string][]string{}
	dns := fakeDNSServer(t, table)
	t.Setenv(fakeTXTEnv, dns.URL)

	gate := func() error {
		return VerifyIdentityResolvable(
			context.Background(), TXTResolverFromEnv(), plc.Client(), plc.URL, did, handle,
		)
	}

	// 1. No record published yet → gated (the state right after a mint).
	if err := gate(); err == nil {
		t.Fatal("gate must BLOCK while the _atproto TXT is unpublished")
	}

	// 2. Record published, but naming a different DID → still gated.
	table["_atproto."+handle] = []string{"did=did:plc:someoneelse"}
	if err := gate(); err == nil {
		t.Fatal("gate must BLOCK when the TXT names a different DID")
	}

	// 3. The right record appears → the gate opens, WITHOUT restarting anything.
	table["_atproto."+handle] = []string{"did=" + did}
	if err := gate(); err != nil {
		t.Fatalf("gate must open once the TXT names this DID: %v", err)
	}
}
