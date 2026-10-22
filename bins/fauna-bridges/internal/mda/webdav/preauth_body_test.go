package webdav

import (
	"io"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// countingBody yields n zero bytes and counts how many the server consumed.
type countingBody struct {
	remaining int
	consumed  int
}

func (b *countingBody) Read(p []byte) (int, error) {
	if b.remaining == 0 {
		return 0, io.EOF
	}
	n := len(p)
	if n > b.remaining {
		n = b.remaining
	}
	clear(p[:n])
	b.remaining -= n
	b.consumed += n
	return n, nil
}

func (b *countingBody) Close() error { return nil }

// TestChainReadsNoBodyBeforeAuth pins the goal doc's "authenticated
// before buffered" bar on the WebDAV chain: an anonymous peer, and a DPoP
// request the nest refuses, are answered 401 having consumed zero body bytes,
// on every body-carrying method.
func TestChainReadsNoBodyBeforeAuth(t *testing.T) {
	challenge := `DPoP error="invalid_token"`
	cases := []struct {
		name  string
		setup func(r *http.Request)
	}{
		{"anonymous", func(*http.Request) {}},
		{"dpop-refused", func(r *http.Request) {
			r.Header.Set("Authorization", "DPoP the-token")
			r.Header.Set("DPoP", "the-proof")
		}},
	}
	for _, c := range cases {
		for _, method := range []string{http.MethodPut, "PROPFIND", "PROPPATCH", "MKCOL"} {
			t.Run(c.name+"/"+method, func(t *testing.T) {
				nest := &fakeNest{admit: wsrpc.WebdavAdmitPrincipalReply{Status: 401, WWWAuthenticate: &challenge}}
				srv := NewServer(ServerConfig{}, nest)
				body := &countingBody{remaining: maxXMLBodyBytes}
				req := httptest.NewRequest(method, "/webdav/u/7/x", body)
				req.Host = "mda.example"
				req.Header.Set("Content-Type", "application/xml")
				req.Header.Set("Depth", "1")
				c.setup(req)
				w := serve(srv.Handler(), req)
				want := http.StatusUnauthorized
				if c.name == "dpop-refused" && !bearerReadMethod(method) {
					want = http.StatusForbidden // the bearer door is read-only; refused before admission
				}
				if w.Code != want {
					t.Fatalf("%s %s status = %d, want %d", c.name, method, w.Code, want)
				}
				if body.consumed != 0 {
					t.Fatalf("%s %s consumed %d body bytes before authenticating, want 0", c.name, method, body.consumed)
				}
			})
		}
	}
}
