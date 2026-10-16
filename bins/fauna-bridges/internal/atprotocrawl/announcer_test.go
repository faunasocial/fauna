package atprotocrawl

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"
	"time"
)

// fakeRelay records every requestCrawl it receives and answers with status.
type fakeRelay struct {
	mu        sync.Mutex
	hostnames []string
	status    int
	got       chan string
}

func newFakeRelay(t *testing.T, status int) (*fakeRelay, *httptest.Server) {
	t.Helper()
	fr := &fakeRelay{status: status, got: make(chan string, 16)}
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost || r.URL.Path != "/xrpc/com.atproto.sync.requestCrawl" {
			http.NotFound(w, r)
			return
		}
		raw, _ := io.ReadAll(r.Body)
		var body struct {
			Hostname string `json:"hostname"`
		}
		if err := json.Unmarshal(raw, &body); err != nil {
			http.Error(w, "bad body", http.StatusBadRequest)
			return
		}
		fr.mu.Lock()
		fr.hostnames = append(fr.hostnames, body.Hostname)
		st := fr.status
		fr.mu.Unlock()
		fr.got <- body.Hostname
		w.WriteHeader(st)
	}))
	t.Cleanup(srv.Close)
	return fr, srv
}

func (fr *fakeRelay) setStatus(s int) {
	fr.mu.Lock()
	fr.status = s
	fr.mu.Unlock()
}

func (fr *fakeRelay) count() int {
	fr.mu.Lock()
	defer fr.mu.Unlock()
	return len(fr.hostnames)
}

func expectAnnounce(t *testing.T, fr *fakeRelay, want string) {
	t.Helper()
	select {
	case got := <-fr.got:
		if got != want {
			t.Fatalf("requestCrawl hostname = %q, want %q", got, want)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("no requestCrawl reached the relay")
	}
}

func expectSilence(t *testing.T, fr *fakeRelay, d time.Duration) {
	t.Helper()
	select {
	case got := <-fr.got:
		t.Fatalf("unexpected requestCrawl for %q", got)
	case <-time.After(d):
	}
}

func TestNotifyAnnouncesTheHostnameToEveryRelay(t *testing.T) {
	a1, s1 := newFakeRelay(t, http.StatusOK)
	a2, s2 := newFakeRelay(t, http.StatusOK)
	ann := New("pds.example.com", []string{s1.URL + "/", " " + s2.URL}, nil, nil)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go ann.Run(ctx)

	ann.Notify()
	expectAnnounce(t, a1, "pds.example.com")
	expectAnnounce(t, a2, "pds.example.com")
}

func TestAnnouncesAreThrottledWithOneTrailingAnnounce(t *testing.T) {
	fr, srv := newFakeRelay(t, http.StatusOK)
	ann := New("pds.example.com", []string{srv.URL}, nil, nil)
	ann.minInterval = 300 * time.Millisecond
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go ann.Run(ctx)

	ann.Notify()
	expectAnnounce(t, fr, "pds.example.com")
	// A burst of commits inside the window collapses into ONE trailing announce
	// at the window's end, never one per commit.
	for i := 0; i < 5; i++ {
		ann.Notify()
		time.Sleep(10 * time.Millisecond)
	}
	expectSilence(t, fr, 150*time.Millisecond)
	expectAnnounce(t, fr, "pds.example.com")
	expectSilence(t, fr, 500*time.Millisecond)
	if n := fr.count(); n != 2 {
		t.Fatalf("relay saw %d announces, want 2", n)
	}
}

func TestAFailedAnnounceRetriesWithoutANewCommit(t *testing.T) {
	fr, srv := newFakeRelay(t, http.StatusServiceUnavailable)
	ann := New("pds.example.com", []string{srv.URL}, nil, nil)
	ann.retryInterval = 100 * time.Millisecond
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go ann.Run(ctx)

	ann.Notify()
	expectAnnounce(t, fr, "pds.example.com") // refused
	fr.setStatus(http.StatusOK)
	expectAnnounce(t, fr, "pds.example.com") // retried on its own, accepted
	expectSilence(t, fr, 300*time.Millisecond)
}

func TestWithNoRelaysTheAnnouncerIsInert(t *testing.T) {
	ann := New("pds.example.com", nil, nil, nil)
	done := make(chan struct{})
	go func() {
		ann.Run(context.Background())
		close(done)
	}()
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("Run with no relays must return at once")
	}
	ann.Notify() // never blocks
	ann.Notify()
}
