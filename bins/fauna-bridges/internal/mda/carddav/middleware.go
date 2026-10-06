package carddav

import (
	"bytes"
	"encoding/xml"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"strconv"
	"time"
)

// W1 (account-data-plane.md § Workstreams) mitigations:
//
//   - PROPFIND `Depth: <n>` header capped at 3 (RFC 4918 allows `infinity`;
//     capping defends against worst-case XML-tree traversal on a deeply-nested
//     collection home set).
//   - Request body capped at 16 MiB (cards larger than this are rejected at the
//     wire before reaching the library's parser).
//   - XML nesting depth capped at 256 (`xml.Decoder` happily descends
//     arbitrarily deep; a malicious actor could otherwise send a report request
//     with millions of nested elements and exhaust the parser stack).
//
// emersion/go-webdav v0.7.0 exposes no Options struct on `carddav.Handler` for
// these — they enforce here in a wrapping middleware that runs before the
// library's XML parser ever sees the payload. Twin of the CalDAV terminator's
// w1Mitigations.
const (
	maxPropfindDepth = 3
	maxRequestBytes  = 16 << 20 // 16 MiB
	maxXMLDepth      = 256
)

// errBodyTooLarge is the canonical body-cap error. Wrapped with
// `http.StatusRequestEntityTooLarge` (413).
var errBodyTooLarge = errors.New("carddav: request body exceeds 16 MiB cap")

// w1Mitigations wraps `next` with the body cap + PROPFIND depth + XML-depth
// checks. Validation happens before the request reaches the CardDAV handler.
//
// Body cap: `http.MaxBytesReader` returns the body cap error lazily — readers
// get an `http.MaxBytesError` from `io.ReadAll` when they exceed the cap. To
// avoid handing a half-read body to the carddav.Handler when the limit trips,
// the middleware pre-reads the body for write methods (PUT / PROPPATCH / REPORT
// / MKCOL / PROPFIND with body), validates XML where applicable, then resets
// the body with the buffered bytes so downstream parsing sees the same wire
// payload.
func w1Mitigations(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		// PROPFIND `Depth:` header check.
		if r.Method == "PROPFIND" {
			depthStr := r.Header.Get("Depth")
			if depthStr != "" && depthStr != "0" && depthStr != "1" {
				// RFC 4918 §10.2 — Depth: infinity is the only non-numeric
				// value. We reject infinity AND any numeric value > 3.
				if depthStr == "infinity" {
					http.Error(w, fmt.Sprintf("carddav: PROPFIND Depth: infinity not supported (cap=%d)", maxPropfindDepth), http.StatusForbidden)
					return
				}
				depthN, err := strconv.Atoi(depthStr)
				if err != nil || depthN < 0 {
					http.Error(w, fmt.Sprintf("carddav: malformed PROPFIND Depth header %q", depthStr), http.StatusBadRequest)
					return
				}
				if depthN > maxPropfindDepth {
					http.Error(w, fmt.Sprintf("carddav: PROPFIND Depth=%d exceeds cap=%d", depthN, maxPropfindDepth), http.StatusForbidden)
					return
				}
			}
		}

		// Body cap + XML-depth check on methods that carry bodies.
		if hasBody(r.Method) && r.Body != nil {
			limited := http.MaxBytesReader(w, r.Body, maxRequestBytes)
			body, err := io.ReadAll(limited)
			if err != nil {
				var mbErr *http.MaxBytesError
				if errors.As(err, &mbErr) {
					http.Error(w, errBodyTooLarge.Error(), http.StatusRequestEntityTooLarge)
					return
				}
				http.Error(w, fmt.Sprintf("carddav: read request body: %v", err), http.StatusBadRequest)
				return
			}
			if isXMLContentType(r.Header.Get("Content-Type")) && len(body) > 0 {
				if err := checkXMLDepth(body, maxXMLDepth); err != nil {
					http.Error(w, err.Error(), http.StatusBadRequest)
					return
				}
			}
			r.Body = io.NopCloser(bytes.NewReader(body))
			r.ContentLength = int64(len(body))
		}

		next.ServeHTTP(w, r)
	})
}

// accessLogMiddleware emits one structured line per CardDAV request so a
// post-deploy hand-proof can reconstruct the exact verb sequence a client
// drove. It sits outermost so it captures auth failures and w1 rejections too,
// and records the final response status via statusRecorder.
//
// Volume discipline (memory: a busy-spun refresh loop once filled example.com's
// disk with a 69 GB JSON log): the rare collection-mutation verbs (MKCOL /
// PROPPATCH / DELETE) log at Info so they surface on a default-Info box without
// any config flip, while the high-volume read/sync verbs (PROPFIND / REPORT /
// GET / OPTIONS / …) log at Debug so steady client polling cannot flood the log
// unless someone deliberately raises the level.
func accessLogMiddleware(next http.Handler, logger *slog.Logger) http.Handler {
	if logger == nil {
		logger = slog.Default()
	}
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		start := time.Now()
		rec := &statusRecorder{ResponseWriter: w, status: http.StatusOK}
		next.ServeHTTP(rec, r)
		level := slog.LevelDebug
		if isLowVolumeMutation(r.Method) {
			level = slog.LevelInfo
		}
		logger.LogAttrs(r.Context(), level, "carddav: access",
			slog.String("method", r.Method),
			slog.String("path", r.URL.Path),
			slog.Int("status", rec.status),
			slog.Int64("duration_ms", time.Since(start).Milliseconds()),
		)
	})
}

// isLowVolumeMutation reports whether `method` is a rare collection-mutation
// verb worth logging at Info. PUT is deliberately excluded — a card upload
// fires on every card create/edit and during sync catch-up, so it belongs with
// the high-volume Debug verbs.
func isLowVolumeMutation(method string) bool {
	switch method {
	case "MKCOL", "PROPPATCH", http.MethodDelete:
		return true
	default:
		return false
	}
}

// statusRecorder wraps a http.ResponseWriter to capture the response status
// code for accessLogMiddleware. It defaults to 200 (the implicit status when a
// handler writes a body without calling WriteHeader) and records the first
// explicit WriteHeader, mirroring net/http's own once-only semantics.
type statusRecorder struct {
	http.ResponseWriter
	status      int
	wroteHeader bool
}

func (s *statusRecorder) WriteHeader(code int) {
	if !s.wroteHeader {
		s.status = code
		s.wroteHeader = true
	}
	s.ResponseWriter.WriteHeader(code)
}

func (s *statusRecorder) Write(b []byte) (int, error) {
	s.wroteHeader = true
	return s.ResponseWriter.Write(b)
}

// hasBody reports whether `method` typically carries a request body. PROPFIND
// carries an optional XML body (the prop filter); PROPPATCH/REPORT/PUT/MKCOL
// always carry bodies (MKCOL carries an optional extended-MKCOL body of initial
// properties); the rest don't. Unlike CalDAV there is no MKCALENDAR verb —
// carddav uses extended-MKCOL for address-book creation.
func hasBody(method string) bool {
	switch method {
	case http.MethodPut, "PROPFIND", "PROPPATCH", "REPORT", "MKCOL", http.MethodPost:
		return true
	default:
		return false
	}
}

// isXMLContentType reports whether the Content-Type header suggests XML. WebDAV
// methods send `application/xml` and `text/xml`; some clients append
// `; charset=utf-8`.
func isXMLContentType(ct string) bool {
	if ct == "" {
		return false
	}
	// Strip parameters (charset, boundary, …).
	for i := 0; i < len(ct); i++ {
		if ct[i] == ';' {
			ct = ct[:i]
			break
		}
	}
	switch ct {
	case "application/xml", "text/xml":
		return true
	default:
		return false
	}
}

// checkXMLDepth scans `body` with `encoding/xml` and rejects when the nesting
// depth exceeds `max`. Returns nil for well-formed XML within the cap; returns
// a wrapped error otherwise. Malformed XML also returns an error, surfacing as
// 400 from the middleware.
func checkXMLDepth(body []byte, max int) error {
	dec := xml.NewDecoder(bytes.NewReader(body))
	depth := 0
	for {
		tok, err := dec.Token()
		if err == io.EOF {
			return nil
		}
		if err != nil {
			return fmt.Errorf("carddav: malformed XML body: %w", err)
		}
		switch tok.(type) {
		case xml.StartElement:
			depth++
			if depth > max {
				return fmt.Errorf("carddav: XML nesting depth %d exceeds cap=%d", depth, max)
			}
		case xml.EndElement:
			depth--
		}
	}
}
