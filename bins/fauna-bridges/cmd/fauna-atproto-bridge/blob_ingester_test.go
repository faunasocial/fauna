package main

import (
	"context"
	"encoding/hex"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// The production byte leg against the status the nest REALLY answers. The
// handler-level tests fake the whole BlobIngester seam, so nothing above this
// exercises the HTTP contract — and a fixture answering 200 here would repeat
// the exact both-sides-green-while-disagreeing miss this test exists for
// (found live 2026-07-30: the nest answers 201 Created, the ingester demanded
// 200, and every real upload 500'd).
func TestStoreInFaunaMediaAcceptsTheNests201(t *testing.T) {
	digest := strings.Repeat("ab", 32)
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "Bearer test-token" {
			t.Errorf("upload carried no bearer: %q", r.Header.Get("Authorization"))
		}
		if err := r.ParseMultipartForm(1 << 20); err != nil {
			t.Errorf("upload body is not multipart: %v", err)
		}
		// 201 Created — blob_routes.rs's spelling of success, verbatim.
		w.WriteHeader(http.StatusCreated)
		_, _ = w.Write([]byte(`{"hash":"` + digest + `"}`))
	}))
	defer srv.Close()

	n := &nestBlobIngester{
		client:   srv.Client(),
		endpoint: srv.URL,
		token:    func(context.Context) (string, error) { return "test-token", nil },
		sidecar:  func(string) []byte { return []byte("sidecar-bytes") },
	}
	mediaRef, err := n.StoreInFaunaMedia(context.Background(), "image/png", []byte("png-bytes"))
	if err != nil {
		t.Fatalf("a 201 Created upload must succeed: %v", err)
	}
	if hex.EncodeToString(mediaRef) != digest {
		t.Fatalf("mediaRef = %x, want the answered hash", mediaRef)
	}
}

// The error arm stays an error arm: a real refusal must not be swallowed by
// the widened success set.
func TestStoreInFaunaMediaStillRefusesANestError(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		http.Error(w, "no", http.StatusForbidden)
	}))
	defer srv.Close()

	n := &nestBlobIngester{
		client:   srv.Client(),
		endpoint: srv.URL,
		token:    func(context.Context) (string, error) { return "t", nil },
		sidecar:  func(string) []byte { return []byte("s") },
	}
	if _, err := n.StoreInFaunaMedia(context.Background(), "image/png", []byte("b")); err == nil {
		t.Fatal("a 403 upload must error")
	}
}
