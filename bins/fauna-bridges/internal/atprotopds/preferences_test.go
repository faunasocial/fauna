package atprotopds

import (
	"bytes"
	"encoding/json"
	"net/http"
	"strings"
	"testing"
)

// postBody POSTs a JSON body with an optional bearer (f.post sends none).
func (f *fixture) postBody(t *testing.T, nsid, bearer string, body []byte) *http.Response {
	t.Helper()
	req, _ := http.NewRequest(http.MethodPost, f.http.URL+"/xrpc/"+nsid, bytes.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	if bearer != "" {
		req.Header.Set("Authorization", "Bearer "+bearer)
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { resp.Body.Close() })
	return resp
}

// The definition-of-success for F3 phase 4: a session-authed client round-trips
// app.bsky.actor.{get,put}Preferences against the bridge, the payload stored in
// nest state, opaque passthrough (the bridge invents nothing).
func TestPreferencesRoundTrip(t *testing.T) {
	f := newFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")

	// Unset → the ecosystem default is an empty array, never null.
	resp := f.get(t, "app.bsky.actor.getPreferences", sess.AccessJwt)
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("getPreferences (unset): %d", resp.StatusCode)
	}
	var got struct {
		Preferences json.RawMessage `json:"preferences"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&got); err != nil {
		t.Fatalf("decode getPreferences: %v", err)
	}
	if strings.TrimSpace(string(got.Preferences)) != "[]" {
		t.Fatalf("unset preferences not an empty array: %s", got.Preferences)
	}

	// Store a payload, then read the EXACT bytes back (opaque passthrough).
	payload := `[{"$type":"app.bsky.actor.defs#savedFeedsPrefV2","items":[{"id":"a"}]}]`
	put := f.postBody(t, "app.bsky.actor.putPreferences", sess.AccessJwt,
		[]byte(`{"preferences":`+payload+`}`))
	if put.StatusCode != http.StatusOK {
		t.Fatalf("putPreferences: %d", put.StatusCode)
	}
	if f.nest.storePrefCalls != 1 {
		t.Fatalf("store_preferences calls: %d", f.nest.storePrefCalls)
	}
	resp = f.get(t, "app.bsky.actor.getPreferences", sess.AccessJwt)
	if err := json.NewDecoder(resp.Body).Decode(&got); err != nil {
		t.Fatalf("decode getPreferences: %v", err)
	}
	if !bytes.Equal(bytes.TrimSpace(got.Preferences), []byte(payload)) {
		t.Fatalf("round-trip mismatch:\n got %s\nwant %s", got.Preferences, payload)
	}
}

func TestPreferencesRequireAuth(t *testing.T) {
	f := newFixture(t)
	// Both routes are authenticated: no token refuses uniformly, and nothing
	// reaches the nest.
	if s := f.get(t, "app.bsky.actor.getPreferences", "").StatusCode; s != http.StatusUnauthorized {
		t.Fatalf("getPreferences without token: %d", s)
	}
	if s := f.postBody(t, "app.bsky.actor.putPreferences", "", []byte(`{"preferences":[]}`)).StatusCode; s != http.StatusUnauthorized {
		t.Fatalf("putPreferences without token: %d", s)
	}
	if f.nest.storePrefCalls != 0 {
		t.Fatal("an unauthenticated putPreferences reached the nest")
	}
}

func TestPutPreferencesRejectsNonArrayAndOversize(t *testing.T) {
	f := newFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")

	// Opaque, but the lexicon shape is an array: a scalar/object is refused.
	bad := f.postBody(t, "app.bsky.actor.putPreferences", sess.AccessJwt,
		[]byte(`{"preferences":{"not":"an array"}}`))
	if bad.StatusCode != http.StatusBadRequest {
		t.Fatalf("non-array accepted: %d", bad.StatusCode)
	}

	// Oversize is refused at the bridge, before the nest.
	spaces := bytes.Repeat([]byte{' '}, maxPreferencesBytes+1)
	body := append([]byte(`{"preferences":[`), spaces...)
	body = append(body, ']', '}')
	over := f.postBody(t, "app.bsky.actor.putPreferences", sess.AccessJwt, body)
	if over.StatusCode != http.StatusBadRequest {
		t.Fatalf("oversize accepted: %d", over.StatusCode)
	}
	if f.nest.storePrefCalls != 0 {
		t.Fatal("a rejected putPreferences still reached the nest")
	}
}
