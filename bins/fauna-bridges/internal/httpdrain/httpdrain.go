// Package httpdrain reads what a handler left of a request body before the
// server closes the connection, so the close reaches the client as a clean end
// and not as a reset that throws away the reply.
//
// The fault it closes. net/http does not read the rest of a request body when
// the request says `Connection: close` (Python's urllib always does, as does
// HTTP/1.0 — the server is closing anyway, so it sees no point in reading to
// EOF). Any route
// that answers without reading its body — an XRPC procedure that takes no input
// (refreshSession, deleteSession), and every refusal written before a body is
// parsed (a 401, a 404, a 429) — then closes a socket with the client's bytes
// still unread in the kernel. A TCP close with unread data sends RST, and on
// Windows an RST makes the client's next receive fail with WSAECONNRESET (10054)
// and discards whatever reply had already arrived. RFC 9112 §9.6 asks the
// server to keep reading until the client is done for exactly this reason.
//
// Measured on Windows 11, 2026-11-07, with a stdlib-only Go TLS server and the e2e
// suite's own urllib client: 26 of 200 POSTs of `{}` to a handler that left the
// body unread failed with ConnectionResetError 10054; 0 of 200 failed once the
// handler drained it. The same error turned the windows background tester's
// connected-apps `refreshSession` call red.
//
// The drain is bounded both ways, because these listeners face anonymous
// peers: at most [MaxBytes], and at most [Timeout] of waiting. A body past the bound is left
// as before; a client that sent `Expect: 100-continue` is not waited on, since
// it holds its body until it hears 100 Continue, which net/http stops offering
// once a reply is written.
package httpdrain

import (
	"io"
	"net/http"
	"strings"
	"time"
)

// MaxBytes is the most of a leftover body the drain reads: the pre-auth body
// allowance the DAV listeners already grant an anonymous peer (their
// TestServerDoesNotBufferBodyBeforeAuth), so the drain never widens what an
// unauthenticated request can make a bridge read. Far above any body a
// refused or no-input request carries in practice (refreshSession's is `{}`).
const MaxBytes = 64 << 10

// Timeout bounds how long the drain waits on a peer that declared a body and
// is slow to send it. The read deadline it sets is left in place afterwards, so
// whatever net/http reads after the handler is bounded too.
const Timeout = 5 * time.Second

// Handler wraps h so that whatever h leaves of a request body (up to
// [MaxBytes], within [Timeout]) is read and discarded before the server
// finishes the request. Every bridge http.Server wraps its Handler in it
// (listeners_test.go sweeps them).
func Handler(h http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		h.ServeHTTP(w, r)
		drain(w, r)
	})
}

func drain(w http.ResponseWriter, r *http.Request) {
	if r.Body == nil || r.Body == http.NoBody || r.ContentLength == 0 {
		return
	}
	if strings.EqualFold(strings.TrimSpace(r.Header.Get("Expect")), "100-continue") {
		return
	}
	// A writer that cannot carry a deadline (a test recorder, an HTTP/2 stream
	// that refuses it) reports an error, deliberately ignored: the byte bound
	// still holds.
	rc := http.NewResponseController(w)
	_ = rc.SetReadDeadline(time.Now().Add(Timeout))
	_, _ = io.CopyN(io.Discard, r.Body, MaxBytes)
	// Leave the bounded deadline in place; never clear it. The
	// deadline the drain borrowed was the server's whole-request ReadTimeout, and
	// net/http still reads after the handler returns (its pre-reply discard of a
	// keep-alive body), so clearing it would let a stalled peer hold the
	// connection for ever. net/http re-arms the deadline itself before the next
	// request on a keep-alive connection.
}
