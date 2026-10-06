package carddav

import (
	"net/http"
	"testing"
)

// TestWellKnownCardDAVRedirectsASignedInClientToItsPrincipal pins
// carddav-server.md § Process topology: `/.well-known/carddav` is RFC 6764
// service discovery, redirecting a signed-in request to the user's principal
// `/{user}/` (whose unified PROPFIND advertises `addressbook-home-set`). The
// apex sends a contacts app to the mail host's own well-known; this is the hop
// that must lead on from there.
func TestWellKnownCardDAVRedirectsASignedInClientToItsPrincipal(t *testing.T) {
	baseURL, stop := startServer(t, putAuthedCaller(t))
	defer stop()

	for _, method := range []string{http.MethodGet, "PROPFIND"} {
		req, err := http.NewRequest(method, baseURL+"/.well-known/carddav", nil)
		if err != nil {
			t.Fatal(err)
		}
		req.SetBasicAuth(fixtureLocalPart+"@"+fixtureDomain, string(fixturePlainPassword))
		client := httpsClient()
		client.CheckRedirect = func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }
		resp, err := client.Do(req)
		if err != nil {
			t.Fatalf("%s: %v", method, err)
		}
		resp.Body.Close()
		if resp.StatusCode < 300 || resp.StatusCode > 399 {
			t.Errorf("%s /.well-known/carddav = %d, want a redirect", method, resp.StatusCode)
		}
		want := "/" + fixtureLocalPart + "@" + fixtureDomain + "/"
		if got := resp.Header.Get("Location"); got != want {
			t.Errorf("%s Location = %q, want %q", method, got, want)
		}
	}
}
