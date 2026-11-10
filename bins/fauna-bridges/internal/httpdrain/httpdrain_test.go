package httpdrain

import (
	"bytes"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"
)

// answerWithoutReading is the shape that reset the connection: a handler that
// replies without touching the request body (the PDS's refreshSession, every
// refusal written before a body is parsed).
var answerWithoutReading = http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	_, _ = w.Write([]byte(`{"ok":true}`))
})

// post builds a request whose body is a bytes.Reader the test can inspect
// afterwards: Len() is what nobody read.
func post(body []byte) (*http.Request, *bytes.Reader) {
	src := bytes.NewReader(body)
	r := httptest.NewRequest(http.MethodPost, "/xrpc/com.atproto.server.refreshSession", io.NopCloser(src))
	r.ContentLength = int64(len(body))
	// What urllib (and many one-shot clients) send: the server will close the
	// connection after this reply, and net/http does not read the rest of the
	// body on that path.
	r.Close = true
	r.Header.Set("Connection", "close")
	return r, src
}

// TestAnUnreadBodyIsReadBeforeTheConnectionCloses pins the property the
// windows red needed: what a handler left of a small body is consumed before
// the server closes, so the close is a FIN and not a reset that discards the
// reply the client was already sent.
func TestAnUnreadBodyIsReadBeforeTheConnectionCloses(t *testing.T) {
	r, src := post([]byte(`{}`))
	rec := httptest.NewRecorder()

	Handler(answerWithoutReading).ServeHTTP(rec, r)

	if src.Len() != 0 {
		t.Fatalf("%d byte(s) of the request body were left unread when the handler returned — on Windows the "+
			"close that follows resets the connection and the client loses the reply", src.Len())
	}
	if rec.Code != http.StatusOK || rec.Body.String() != `{"ok":true}` {
		t.Fatalf("the drain changed the reply: %d %q", rec.Code, rec.Body.String())
	}
}

// TestTheDrainStopsAtItsBound pins that an anonymous peer cannot make the
// server read an arbitrary amount: past MaxBytes the drain gives up and the
// connection closes as before.
func TestTheDrainStopsAtItsBound(t *testing.T) {
	r, src := post(make([]byte, 3*MaxBytes))
	Handler(answerWithoutReading).ServeHTTP(httptest.NewRecorder(), r)

	read := 3*MaxBytes - src.Len()
	if read == 0 {
		t.Fatal("nothing was drained — the bound test cannot tell a bounded drain from no drain at all")
	}
	if read > MaxBytes {
		t.Fatalf("the drain read %d bytes, past its bound of %d", read, MaxBytes)
	}
}

// TestAnExpectContinueBodyIsNotWaitedOn: a client that sent `Expect:
// 100-continue` holds its body until it hears 100 Continue, which net/http no
// longer sends once the handler has written a reply. Reading then would wait
// on bytes that never come, so the drain leaves such a request alone.
func TestAnExpectContinueBodyIsNotWaitedOn(t *testing.T) {
	r, src := post([]byte(`{}`))
	r.Header.Set("Expect", "100-continue")
	Handler(answerWithoutReading).ServeHTTP(httptest.NewRecorder(), r)

	if src.Len() != 2 {
		t.Fatalf("an Expect: 100-continue body was read (%d of 2 bytes left)", src.Len())
	}
}

// TestABodyTheHandlerReadIsLeftAsItWas and a bodiless request are no-ops: the
// drain only ever reads what is still there.
func TestABodyTheHandlerReadIsLeftAsItWas(t *testing.T) {
	r, src := post([]byte(`{"identifier":"alice"}`))
	readAll := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = io.Copy(io.Discard, r.Body)
		w.WriteHeader(http.StatusNoContent)
	})
	rec := httptest.NewRecorder()
	Handler(readAll).ServeHTTP(rec, r)
	if src.Len() != 0 || rec.Code != http.StatusNoContent {
		t.Fatalf("left %d byte(s), code %d", src.Len(), rec.Code)
	}

	get := httptest.NewRequest(http.MethodGet, "/.well-known/did.json", http.NoBody)
	rec = httptest.NewRecorder()
	Handler(answerWithoutReading).ServeHTTP(rec, get)
	if rec.Code != http.StatusOK {
		t.Fatalf("a bodiless GET came back %d", rec.Code)
	}
}

// readTimeoutSlack is how long the test lets the server take to cut a stalled
// connection: its 2 s ReadTimeout plus the drain's Timeout, with margin.
const readTimeoutSlack = 2*time.Second + Timeout + 3*time.Second

// TestAStalledKeepAliveBodyIsStillCutAtTheServersReadTimeout pins: the
// drain borrows the connection's read deadline, and must leave a bounded one
// behind. A keep-alive request that declares a long body and stalls after the
// drain's 64 KiB used to hold the connection for ever, because the drain
// cleared the server's whole-request ReadTimeout and net/http's own body
// discard then ran with no deadline at all.
func TestAStalledKeepAliveBodyIsStillCutAtTheServersReadTimeout(t *testing.T) {
	refuse := http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusUnauthorized)
	})
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	srv := &http.Server{Handler: Handler(refuse), ReadTimeout: 2 * time.Second}
	go func() { _ = srv.Serve(ln) }()
	defer srv.Close()

	c, err := net.Dial("tcp", ln.Addr().String())
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()
	if _, err := fmt.Fprintf(c, "POST /x HTTP/1.1\r\nHost: t\r\nContent-Length: 200000\r\n\r\n"); err != nil {
		t.Fatal(err)
	}
	if _, err := c.Write(make([]byte, 70000)); err != nil {
		t.Fatal(err)
	}

	// The client now stalls. The server must release the connection soon after
	// ReadTimeout (plus the drain's own wait), not hold it for ever.
	_ = c.SetReadDeadline(time.Now().Add(readTimeoutSlack))
	if _, err := io.Copy(io.Discard, c); err != nil {
		t.Fatalf("the server still held the stalled connection %v after the request began: %v", readTimeoutSlack, err)
	}
}
