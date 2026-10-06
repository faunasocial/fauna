package webdav

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

// W1 (account-data-plane.md § Workstreams) mitigations for the WebDAV files terminator. Same shape as the CalDAV /
// CardDAV `w1Mitigations`, with ONE deliberate deviation (webdav-server.md
// § Process topology): the 16 MiB DAV body cap is calendar/card-specific — a
// WebDAV PUT carries a *file*, bounded by `maxPutBytes` (the set's tier
// max-blob ceiling), and is NOT pre-read into memory (the XML methods are; a
// PUT streams to the backend, which chunks it).
//
// TODO(webdav): the pure helpers here (checkXMLDepth, isXMLContentType,
// statusRecorder, the PROPFIND-depth gate) are byte-identical across the
// caldav/carddav/webdav terminators — lift them to internal/mda/dav and migrate
// all three (priority #4; a mechanical refactor best done as its own commit so
// it doesn't destabilize the landed CalDAV/CardDAV terminators mid-slice).
const (
	maxPropfindDepth = 3
	maxXMLBodyBytes  = 16 << 20 // XML request bodies (PROPFIND/PROPPATCH/MKCOL).
	// maxPutBytes bounds a WebDAV PUT. The MDA buffers the file to chunk+seal it
	// (the FFI chunker is buffer-based), so this is the in-memory ceiling; nest's
	// per-actor storage metering (webdav_record_change → storage_quota_exceeded)
	// is the real quota. True streaming + precise tier-max-blob coupling is a
	// follow-on (webdav-server.md § Bulk-byte plane deviation).
	maxPutBytes = 512 << 20 // 512 MiB
	maxXMLDepth = 256
)

var errBodyTooLarge = errors.New("webdav: request body exceeds cap")

// w1Mitigations wraps `next` with the PROPFIND-depth gate, an XML-body-method
// pre-read + XML-depth check, and a streaming byte cap on PUT.
func w1Mitigations(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == "PROPFIND" {
			if code, msg, ok := checkPropfindDepth(r.Header.Get("Depth")); !ok {
				http.Error(w, msg, code)
				return
			}
		}

		switch {
		case r.Method == http.MethodPut && r.Body != nil:
			// Stream: cap the body without buffering it — the backend reads it to
			// chunk. MaxBytesReader trips lazily in the backend's io.ReadAll, which
			// surfaces as a 500 there (the file exceeds the box's PUT ceiling).
			r.Body = http.MaxBytesReader(w, r.Body, maxPutBytes)
		case isXMLBodyMethod(r.Method) && r.Body != nil:
			body, err := readCapped(w, r.Body, maxXMLBodyBytes)
			if err != nil {
				var mbErr *http.MaxBytesError
				if errors.As(err, &mbErr) {
					http.Error(w, errBodyTooLarge.Error(), http.StatusRequestEntityTooLarge)
					return
				}
				http.Error(w, fmt.Sprintf("webdav: read request body: %v", err), http.StatusBadRequest)
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

// checkPropfindDepth rejects `Depth: infinity` and any numeric depth over the
// cap (RFC 4918 §10.2). Returns (status, message, ok=false) on rejection.
func checkPropfindDepth(depthStr string) (int, string, bool) {
	if depthStr == "" || depthStr == "0" || depthStr == "1" {
		return 0, "", true
	}
	if depthStr == "infinity" {
		return http.StatusForbidden, fmt.Sprintf("webdav: PROPFIND Depth: infinity not supported (cap=%d)", maxPropfindDepth), false
	}
	depthN, err := strconv.Atoi(depthStr)
	if err != nil || depthN < 0 {
		return http.StatusBadRequest, fmt.Sprintf("webdav: malformed PROPFIND Depth header %q", depthStr), false
	}
	if depthN > maxPropfindDepth {
		return http.StatusForbidden, fmt.Sprintf("webdav: PROPFIND Depth=%d exceeds cap=%d", depthN, maxPropfindDepth), false
	}
	return 0, "", true
}

func readCapped(w http.ResponseWriter, body io.ReadCloser, limit int64) ([]byte, error) {
	return io.ReadAll(http.MaxBytesReader(w, body, limit))
}

// isXMLBodyMethod reports whether `method` carries a (small, XML) request body
// worth pre-reading + depth-checking. PUT is excluded — it carries a file and
// streams (handled above).
func isXMLBodyMethod(method string) bool {
	switch method {
	case "PROPFIND", "PROPPATCH", "MKCOL":
		return true
	default:
		return false
	}
}

func isXMLContentType(ct string) bool {
	if ct == "" {
		return false
	}
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

func checkXMLDepth(body []byte, max int) error {
	dec := xml.NewDecoder(bytes.NewReader(body))
	depth := 0
	for {
		tok, err := dec.Token()
		if err == io.EOF {
			return nil
		}
		if err != nil {
			return fmt.Errorf("webdav: malformed XML body: %w", err)
		}
		switch tok.(type) {
		case xml.StartElement:
			depth++
			if depth > max {
				return fmt.Errorf("webdav: XML nesting depth %d exceeds cap=%d", depth, max)
			}
		case xml.EndElement:
			depth--
		}
	}
}

// accessLogMiddleware emits one structured line per request. Low-volume
// mutations (MKCOL/DELETE/MOVE/COPY/PROPPATCH) log at Info; high-volume
// read/browse verbs (PROPFIND/GET/HEAD/OPTIONS) and PUT log at Debug so steady
// client polling can't flood the log (the example.com 69 GB log lesson).
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
		logger.LogAttrs(r.Context(), level, "webdav: access",
			slog.String("method", r.Method),
			slog.String("path", r.URL.Path),
			slog.Int("status", rec.status),
			slog.Int64("duration_ms", time.Since(start).Milliseconds()),
		)
	})
}

func isLowVolumeMutation(method string) bool {
	switch method {
	case "MKCOL", "PROPPATCH", "MOVE", "COPY", http.MethodDelete:
		return true
	default:
		return false
	}
}

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
