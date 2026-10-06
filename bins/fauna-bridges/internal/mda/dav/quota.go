package dav

import (
	"fmt"
	"net/http"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// The storage-full answer shared by the CalDAV and CardDAV writes
// (caldav-server.md § QUOTA → § Enforcement points; carddav-server.md § Quota
// delegates there): the nest refuses a write past the account's one storage
// allowance with the typed `fauna.bridges.over_quota` before anything is
// stored, and the MDA answers `507 Insufficient Storage` carrying the RFC 4918
// §15 `DAV:quota-not-exceeded` precondition.

// IsOverQuota reports whether err is the nest's typed over-quota refusal.
func IsOverQuota(err error) bool {
	code, ok := wsrpc.RpcErrorCode(err)
	return ok && code == wsrpc.CodeOverQuota
}

// WriteQuotaNotExceeded answers 507 with the DAV:quota-not-exceeded
// precondition body — for a handler that owns its response (MOVE/COPY).
func WriteQuotaNotExceeded(w http.ResponseWriter) {
	w.Header().Del("Content-Length")
	w.Header().Set("Content-Type", "application/xml; charset=utf-8")
	w.WriteHeader(http.StatusInsufficientStorage)
	_, _ = fmt.Fprint(w,
		`<?xml version="1.0" encoding="utf-8"?>`+"\n"+
			`<D:error xmlns:D="DAV:">`+"\n"+
			`  <D:quota-not-exceeded/>`+"\n"+
			`</D:error>`+"\n",
	)
}

// QuotaBody wraps a DAV library handler so the 507 it answers — for a backend
// write that returned `webdav.NewHTTPError(http.StatusInsufficientStorage, …)`
// on IsOverQuota — carries the DAV:quota-not-exceeded body in place of the
// library's own error text. Over-quota is the only 507 the CalDAV and CardDAV
// backends produce, so every 507 through this wrapper is that precondition.
// Every other answer passes through untouched.
func QuotaBody(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		next.ServeHTTP(&quotaBodyWriter{ResponseWriter: w}, r)
	})
}

type quotaBodyWriter struct {
	http.ResponseWriter
	// replaced is set once the 507 precondition has been written; the
	// wrapped handler's own body for it is then dropped.
	replaced bool
}

func (q *quotaBodyWriter) WriteHeader(code int) {
	if code == http.StatusInsufficientStorage {
		q.replaced = true
		WriteQuotaNotExceeded(q.ResponseWriter)
		return
	}
	q.ResponseWriter.WriteHeader(code)
}

func (q *quotaBodyWriter) Write(b []byte) (int, error) {
	if q.replaced {
		return len(b), nil
	}
	return q.ResponseWriter.Write(b)
}

// Unwrap lets http.ResponseController reach the underlying writer.
func (q *quotaBodyWriter) Unwrap() http.ResponseWriter { return q.ResponseWriter }
