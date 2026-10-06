package caldav

import (
	"encoding/hex"
	"io"
	"net/http"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"lukechampine.com/blake3"
)

// TestPutIfNoneMatchStarRefusesAnExistingEvent: a calendar app's create-only
// PUT (`If-None-Match: *`) onto an event that already exists is refused 412
// and nothing is written; onto a new event it creates — the CardDAV PUT's twin.
func TestPutIfNoneMatchStarRefusesAnExistingEvent(t *testing.T) {
	existingHash := blake3.Sum256([]byte("put-test-uid-001"))
	for _, tc := range []struct {
		name     string
		existing []wsrpc.EventEntry
		want     int
		puts     int
	}{
		{"existing event", []wsrpc.EventEntry{{UIDHash: existingHash[:], ETag: "e1"}}, http.StatusPreconditionFailed, 0},
		{"new event", []wsrpc.EventEntry{}, http.StatusCreated, 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			caller := putValidEventOK(t)
			caller.queryEventsByCalendar = map[string][]wsrpc.EventEntry{
				hex.EncodeToString(testCalendarID): tc.existing,
			}
			baseURL, stop := startServer(t, caller)
			defer stop()

			req := putRequest(t, eventURL(baseURL, testCalendarID, "any"), testPutValidEvent, "")
			req.Header.Set("If-None-Match", "*")
			resp, err := httpsClient().Do(req)
			if err != nil {
				t.Fatalf("Do: %v", err)
			}
			io.Copy(io.Discard, resp.Body)
			resp.Body.Close()
			if resp.StatusCode != tc.want {
				t.Fatalf("status = %d, want %d", resp.StatusCode, tc.want)
			}
			if n := len(caller.callsOf(wsrpc.MethodPutEventCiphertext)); n != tc.puts {
				t.Errorf("put_event_ciphertext fired %d times, want %d", n, tc.puts)
			}
		})
	}
}
