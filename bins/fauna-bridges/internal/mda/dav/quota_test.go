package dav

import (
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"github.com/fxamacker/cbor/v2"
)

func nestError(t *testing.T, code string) error {
	t.Helper()
	payload, err := cbor.Marshal(map[string]any{"code": code})
	if err != nil {
		t.Fatal(err)
	}
	return fmt.Errorf("put_event_ciphertext: %w", &wsrpc.ServerError{Payload: payload})
}

// TestIsOverQuota: only the nest's typed over-quota refusal counts — any other
// nest error, or a transport error, is not a storage-full answer.
func TestIsOverQuota(t *testing.T) {
	if !IsOverQuota(nestError(t, wsrpc.CodeOverQuota)) {
		t.Error("the typed over_quota refusal must read as over quota")
	}
	if IsOverQuota(nestError(t, "fauna.bridges.permission_denied")) {
		t.Error("another nest refusal must not read as over quota")
	}
	if IsOverQuota(errors.New("connection reset")) {
		t.Error("a transport error must not read as over quota")
	}
}

// TestQuotaBodyAnswers507WithThePrecondition pins caldav-server.md § QUOTA →
// § Enforcement points: whatever body the wrapped DAV handler writes with its
// 507, the client receives the RFC 4918 §15 DAV:quota-not-exceeded
// precondition, as XML, and nothing of the handler's own text.
func TestQuotaBodyAnswers507WithThePrecondition(t *testing.T) {
	inner := http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Length", "11")
		http.Error(w, "inner text", http.StatusInsufficientStorage)
	})
	rec := httptest.NewRecorder()
	QuotaBody(inner).ServeHTTP(rec, httptest.NewRequest(http.MethodPut, "/x.ics", nil))

	resp := rec.Result()
	body, _ := io.ReadAll(resp.Body)
	if resp.StatusCode != http.StatusInsufficientStorage {
		t.Fatalf("status = %d, want 507", resp.StatusCode)
	}
	if !strings.Contains(string(body), "<D:quota-not-exceeded/>") || strings.Contains(string(body), "inner text") {
		t.Errorf("body = %q, want only the DAV:quota-not-exceeded precondition", body)
	}
	if ct := resp.Header.Get("Content-Type"); !strings.HasPrefix(ct, "application/xml") {
		t.Errorf("Content-Type = %q, want application/xml", ct)
	}
	if cl := resp.Header.Get("Content-Length"); cl != "" {
		t.Errorf("a stale Content-Length %q survived the body swap", cl)
	}
}

// TestQuotaBodyPassesOtherAnswersThrough: every non-507 answer reaches the
// client untouched.
func TestQuotaBodyPassesOtherAnswersThrough(t *testing.T) {
	inner := http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusCreated)
		_, _ = io.WriteString(w, "made")
	})
	rec := httptest.NewRecorder()
	QuotaBody(inner).ServeHTTP(rec, httptest.NewRequest(http.MethodPut, "/x.ics", nil))
	if rec.Code != http.StatusCreated || rec.Body.String() != "made" {
		t.Errorf("got %d %q, want 201 \"made\"", rec.Code, rec.Body.String())
	}
}
