package caldav

import (
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"testing"
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
	for i := 0; i < n; i++ {
		p[i] = 0
	}
	b.remaining -= n
	b.consumed += n
	return n, nil
}

func (b *countingBody) Close() error { return nil }

// TestServerDoesNotBufferBodyBeforeAuth pins: an anonymous peer must
// not make the chain buffer the 16 MiB body cap before the Authorization header
// is checked. An unauthenticated large PUT is refused 401 having consumed (far)
// less than the pre-auth allowance.
func TestServerDoesNotBufferBodyBeforeAuth(t *testing.T) {
	srv := NewServer(ServerConfig{Logger: slog.Default(), MailEnabled: true}, &mockCaller{})
	defer func() { _ = srv.Close() }()
	body := &countingBody{remaining: maxRequestBytes}
	req := httptest.NewRequest(http.MethodPut, "/caldav/"+fixtureLocalPart+"@"+fixtureDomain+"/x/y.ics", body)
	req.Header.Set("Content-Type", "text/calendar")
	rec := httptest.NewRecorder()
	srv.Handler().ServeHTTP(rec, req)
	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("anonymous PUT status = %d, want 401", rec.Code)
	}
	const preAuthAllowance = 64 << 10
	if body.consumed > preAuthAllowance {
		t.Fatalf("server consumed %d body bytes before authenticating, want <= %d", body.consumed, preAuthAllowance)
	}
}
