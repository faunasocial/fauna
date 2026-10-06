package carddav

import (
	"bytes"
	"context"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// captureHandler is a minimal slog.Handler that records every emitted record
// regardless of level, so a test can assert both the chosen level and the
// attributes accessLogMiddleware writes. Twin of the CalDAV terminator's
// captureHandler.
type captureHandler struct {
	records []slog.Record
}

func (h *captureHandler) Enabled(context.Context, slog.Level) bool { return true }
func (h *captureHandler) Handle(_ context.Context, r slog.Record) error {
	h.records = append(h.records, r)
	return nil
}
func (h *captureHandler) WithAttrs([]slog.Attr) slog.Handler { return h }
func (h *captureHandler) WithGroup(string) slog.Handler      { return h }

func recordAttrs(r slog.Record) map[string]slog.Value {
	out := make(map[string]slog.Value, r.NumAttrs())
	r.Attrs(func(a slog.Attr) bool {
		out[a.Key] = a.Value
		return true
	})
	return out
}

// TestAccessLogRecordsMutationVerbAtInfo asserts that a rare collection-mutation
// verb (PROPPATCH — the rename half of the create-then-rename gesture) is logged
// at Info with the response status the inner handler chose, so it surfaces on a
// default-Info box during a live hand-proof.
func TestAccessLogRecordsMutationVerbAtInfo(t *testing.T) {
	cap := &captureHandler{}
	logger := slog.New(cap)

	inner := http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusMultiStatus)
	})
	mw := accessLogMiddleware(inner, logger)

	req := httptest.NewRequest("PROPPATCH", "/carddav/user/abcd/", nil)
	mw.ServeHTTP(httptest.NewRecorder(), req)

	if len(cap.records) != 1 {
		t.Fatalf("expected 1 access-log record, got %d", len(cap.records))
	}
	rec := cap.records[0]
	if rec.Level != slog.LevelInfo {
		t.Errorf("PROPPATCH should log at Info, got %v", rec.Level)
	}
	attrs := recordAttrs(rec)
	if got := attrs["method"].String(); got != "PROPPATCH" {
		t.Errorf("method attr = %q, want PROPPATCH", got)
	}
	if got := attrs["path"].String(); got != "/carddav/user/abcd/" {
		t.Errorf("path attr = %q", got)
	}
	if got := attrs["status"].Int64(); got != int64(http.StatusMultiStatus) {
		t.Errorf("status attr = %d, want %d", got, http.StatusMultiStatus)
	}
}

// TestAccessLogRecordsReadVerbAtDebug asserts a high-volume read verb (PROPFIND)
// is logged at Debug, so steady client polling cannot flood a default-Info box.
func TestAccessLogRecordsReadVerbAtDebug(t *testing.T) {
	cap := &captureHandler{}
	logger := slog.New(cap)

	inner := http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusMultiStatus)
	})
	mw := accessLogMiddleware(inner, logger)

	req := httptest.NewRequest("PROPFIND", "/carddav/user/abcd/", nil)
	mw.ServeHTTP(httptest.NewRecorder(), req)

	if len(cap.records) != 1 {
		t.Fatalf("expected 1 access-log record, got %d", len(cap.records))
	}
	if cap.records[0].Level != slog.LevelDebug {
		t.Errorf("PROPFIND should log at Debug, got %v", cap.records[0].Level)
	}
}

// TestAccessLogDefaultsStatusTo200 asserts statusRecorder reports 200 when the
// inner handler writes a body without an explicit WriteHeader (net/http's
// implicit-200 behavior), so the access log never reports a bogus 0.
func TestAccessLogDefaultsStatusTo200(t *testing.T) {
	cap := &captureHandler{}
	logger := slog.New(cap)

	inner := http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_, _ = w.Write([]byte("ok"))
	})
	mw := accessLogMiddleware(inner, logger)

	req := httptest.NewRequest("GET", "/carddav/user/abcd/", nil)
	mw.ServeHTTP(httptest.NewRecorder(), req)

	if len(cap.records) != 1 {
		t.Fatalf("expected 1 access-log record, got %d", len(cap.records))
	}
	attrs := recordAttrs(cap.records[0])
	if got := attrs["status"].Int64(); got != http.StatusOK {
		t.Errorf("implicit status = %d, want 200", got)
	}
}

// TestAccessLogPropagatesStatusToClient asserts the statusRecorder wrapper does
// not swallow the inner handler's status — the client still sees the real code.
func TestAccessLogPropagatesStatusToClient(t *testing.T) {
	logger := slog.New(&captureHandler{})
	inner := http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		http.Error(w, "nope", http.StatusNotFound)
	})
	mw := accessLogMiddleware(inner, logger)

	rec := httptest.NewRecorder()
	mw.ServeHTTP(rec, httptest.NewRequest("PROPPATCH", "/carddav/user/abcd/", nil))

	if rec.Code != http.StatusNotFound {
		t.Errorf("client saw status %d, want 404 — wrapper must not swallow it", rec.Code)
	}
}

// w1SentinelInner records whether the wrapped handler was reached, so a w1 test
// can assert both the reject status AND that a rejected request never reaches
// the CardDAV handler (the whole point of validating before the library's XML
// parser ever sees the payload).
func w1SentinelInner(reached *bool) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		*reached = true
		w.WriteHeader(http.StatusMultiStatus)
	})
}

// TestW1RejectsOversizedBody asserts a PUT body above the 16 MiB cap is rejected
// with 413 before it reaches the handler (W1 (account-data-plane.md § Workstreams) body cap; goal doc caldav-server.md
// § Size cap — the CardDAV surface shares the constant via the twin middleware).
func TestW1RejectsOversizedBody(t *testing.T) {
	reached := false
	mw := w1Mitigations(w1SentinelInner(&reached))

	body := bytes.Repeat([]byte("x"), maxRequestBytes+1)
	req := httptest.NewRequest(http.MethodPut, "/carddav/user/abcd/card.vcf", bytes.NewReader(body))
	rec := httptest.NewRecorder()
	mw.ServeHTTP(rec, req)

	if rec.Code != http.StatusRequestEntityTooLarge {
		t.Errorf("oversized body: status = %d, want 413", rec.Code)
	}
	if reached {
		t.Error("oversized body must be rejected before reaching the CardDAV handler")
	}
}

// TestW1RejectsBadPropfindDepth asserts the PROPFIND Depth cap: infinity and any
// value above the cap are 403; a malformed/negative Depth is 400. None reach the
// handler (W1 PROPFIND depth cap defends against deeply-nested-tree traversal).
func TestW1RejectsBadPropfindDepth(t *testing.T) {
	cases := []struct {
		name  string
		depth string
		want  int
	}{
		{"infinity", "infinity", http.StatusForbidden},
		{"over_cap", "4", http.StatusForbidden},
		{"malformed", "banana", http.StatusBadRequest},
		{"negative", "-1", http.StatusBadRequest},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			reached := false
			mw := w1Mitigations(w1SentinelInner(&reached))
			req := httptest.NewRequest("PROPFIND", "/carddav/user/abcd/", nil)
			req.Header.Set("Depth", tc.depth)
			rec := httptest.NewRecorder()
			mw.ServeHTTP(rec, req)

			if rec.Code != tc.want {
				t.Errorf("Depth: %s: status = %d, want %d", tc.depth, rec.Code, tc.want)
			}
			if reached {
				t.Errorf("Depth: %s must be rejected before the handler", tc.depth)
			}
		})
	}
}

// TestW1AllowsPropfindDepthWithinCap asserts a Depth within the cap (1) is passed
// straight through to the handler — the cap rejects only the abusive tail.
func TestW1AllowsPropfindDepthWithinCap(t *testing.T) {
	reached := false
	mw := w1Mitigations(w1SentinelInner(&reached))
	req := httptest.NewRequest("PROPFIND", "/carddav/user/abcd/", nil)
	req.Header.Set("Depth", "1")
	rec := httptest.NewRecorder()
	mw.ServeHTTP(rec, req)

	if !reached {
		t.Error("Depth: 1 is within the cap and must reach the handler")
	}
	if rec.Code != http.StatusMultiStatus {
		t.Errorf("Depth: 1: status = %d, want 207", rec.Code)
	}
}

// TestW1RejectsDeeplyNestedXML asserts an XML body nested beyond the cap is
// rejected with 400 before the library's parser descends it (W1 XML-depth cap
// defends against parser-stack exhaustion from millions of nested elements).
func TestW1RejectsDeeplyNestedXML(t *testing.T) {
	reached := false
	mw := w1Mitigations(w1SentinelInner(&reached))

	depth := maxXMLDepth + 5
	body := strings.Repeat("<a>", depth) + strings.Repeat("</a>", depth)
	req := httptest.NewRequest("REPORT", "/carddav/user/abcd/book/", strings.NewReader(body))
	req.Header.Set("Content-Type", "application/xml")
	rec := httptest.NewRecorder()
	mw.ServeHTTP(rec, req)

	if rec.Code != http.StatusBadRequest {
		t.Errorf("deeply-nested XML: status = %d, want 400", rec.Code)
	}
	if reached {
		t.Error("deeply-nested XML must be rejected before the handler")
	}
}

// TestW1PassesValidRequestAndPreservesBody asserts the happy path: a valid,
// within-cap XML body reaches the handler unchanged. This is the load-bearing
// guarantee that the middleware's buffer-then-reset (needed so MaxBytesReader
// never hands the handler a half-read body) does not corrupt the wire payload —
// the downstream handler must see the same bytes and a corrected ContentLength.
func TestW1PassesValidRequestAndPreservesBody(t *testing.T) {
	const body = `<propfind xmlns="DAV:"><prop><displayname/></prop></propfind>`

	var gotBody string
	var gotLen int64
	inner := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		b, _ := io.ReadAll(r.Body)
		gotBody = string(b)
		gotLen = r.ContentLength
		w.WriteHeader(http.StatusMultiStatus)
	})
	mw := w1Mitigations(inner)

	req := httptest.NewRequest("REPORT", "/carddav/user/abcd/book/", strings.NewReader(body))
	req.Header.Set("Content-Type", "text/xml")
	rec := httptest.NewRecorder()
	mw.ServeHTTP(rec, req)

	if rec.Code != http.StatusMultiStatus {
		t.Errorf("valid request: status = %d, want 207", rec.Code)
	}
	if gotBody != body {
		t.Errorf("downstream body = %q, want the original wire bytes intact", gotBody)
	}
	if gotLen != int64(len(body)) {
		t.Errorf("downstream ContentLength = %d, want %d (middleware must reset it after buffering)", gotLen, len(body))
	}
}
