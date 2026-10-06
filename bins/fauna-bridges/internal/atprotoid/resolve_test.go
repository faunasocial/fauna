package atprotoid

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"sync"
	"testing"
)

// fakeTXT is a canned TXTResolver.
type fakeTXT struct {
	records map[string][]string
	err     error
}

func (f *fakeTXT) LookupTXT(_ context.Context, name string) ([]string, error) {
	if f.err != nil {
		return nil, f.err
	}
	return f.records[name], nil
}

// rewriteTransport routes every request (any scheme/host) to the one test
// server, so the "https://<handle>/.well-known/did.json" URL is exercisable
// against httptest.
type rewriteTransport struct{ target *url.URL }

func (rt rewriteTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	r2 := req.Clone(req.Context())
	r2.URL.Scheme = rt.target.Scheme
	r2.URL.Host = rt.target.Host
	return http.DefaultTransport.RoundTrip(r2)
}

func TestVerifyIdentityResolvablePlc(t *testing.T) {
	const did = "did:plc:aaaabbbbccccddddeeeeffff"
	const handle = "alice.example.com"

	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/"+did {
			fmt.Fprint(w, `{"id":"`+did+`"}`)
			return
		}
		http.NotFound(w, r)
	}))
	defer srv.Close()

	resolver := &fakeTXT{records: map[string][]string{
		"_atproto." + handle: {"did=" + did},
	}}

	if err := VerifyIdentityResolvable(context.Background(), resolver, srv.Client(), srv.URL, did, handle); err != nil {
		t.Errorf("healthy plc identity reported unresolvable: %v", err)
	}

	// Directory 404 → the DID-doc failure is reported.
	err := VerifyIdentityResolvable(context.Background(), resolver, srv.Client(), srv.URL, "did:plc:gggghhhhiiiijjjjkkkkllll", handle)
	if err == nil || !strings.Contains(err.Error(), "DID document") {
		t.Errorf("directory 404 not reported, got %v", err)
	}

	// Missing TXT → reported, and the doc success does not mask it.
	if err := VerifyIdentityResolvable(context.Background(), &fakeTXT{}, srv.Client(), srv.URL, did, handle); err == nil || !strings.Contains(err.Error(), "TXT") {
		t.Errorf("missing TXT not reported, got %v", err)
	}
	// Wrong TXT value counts as absent.
	wrong := &fakeTXT{records: map[string][]string{"_atproto." + handle: {"did=did:plc:zzzz"}}}
	if err := VerifyIdentityResolvable(context.Background(), wrong, srv.Client(), srv.URL, did, handle); err == nil || !strings.Contains(err.Error(), "TXT") {
		t.Errorf("wrong TXT value not reported, got %v", err)
	}
}

func TestVerifyIdentityResolvableWeb(t *testing.T) {
	const handle = "alice.example.com"
	did := DIDWeb(handle)

	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/.well-known/did.json" {
			fmt.Fprint(w, `{"id":"`+did+`"}`)
			return
		}
		http.NotFound(w, r)
	}))
	defer srv.Close()
	target, err := url.Parse(srv.URL)
	if err != nil {
		t.Fatalf("parse server url: %v", err)
	}
	client := &http.Client{Transport: rewriteTransport{target: target}}

	resolver := &fakeTXT{records: map[string][]string{
		"_atproto." + handle: {"did=" + did},
	}}
	if err := VerifyIdentityResolvable(context.Background(), resolver, client, DefaultPLCDirectoryURL, did, handle); err != nil {
		t.Errorf("healthy web identity reported unresolvable: %v", err)
	}

	// A 404 well-known doc fails.
	srv404 := httptest.NewServer(http.NotFoundHandler())
	defer srv404.Close()
	target404, err := url.Parse(srv404.URL)
	if err != nil {
		t.Fatalf("parse 404 server url: %v", err)
	}
	notFound := &http.Client{Transport: rewriteTransport{target: target404}}
	if err := VerifyIdentityResolvable(context.Background(), resolver, notFound, DefaultPLCDirectoryURL, did, handle); err == nil || !strings.Contains(err.Error(), "DID document") {
		t.Errorf("missing well-known doc not reported, got %v", err)
	}
}

func TestVerifyIdentityResolvableUnknownMethod(t *testing.T) {
	resolver := &fakeTXT{records: map[string][]string{"_atproto.h": {"did=did:key:z"}}}
	err := VerifyIdentityResolvable(context.Background(), resolver, http.DefaultClient, DefaultPLCDirectoryURL, "did:key:zabc", "h")
	if err == nil || !strings.Contains(err.Error(), "unknown DID method") {
		t.Errorf("unknown method not reported, got %v", err)
	}
}

// fakeDNSServer is the harness shape: a TXT server whose answers CHANGE over
// time, exactly as the e2e's does once the mint publishes the record.
func fakeDNSServer(t *testing.T, table map[string][]string) *httptest.Server {
	t.Helper()
	var mu sync.Mutex
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		name := strings.TrimPrefix(r.URL.Path, "/txt/")
		mu.Lock()
		records, ok := table[name]
		mu.Unlock()
		if !ok {
			w.WriteHeader(http.StatusNotFound)
			return
		}
		_ = json.NewEncoder(w).Encode(records)
	}))
	t.Cleanup(srv.Close)
	return srv
}
