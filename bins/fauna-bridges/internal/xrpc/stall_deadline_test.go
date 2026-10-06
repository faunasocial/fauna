package xrpc

import (
	"bytes"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

// deadlineRecorder is an http.ResponseWriter that carries a write deadline, which
// is what http.ResponseController looks for when it unwraps. Recording the calls
// is the only way to observe the frame's hardening from inside a handler — the
// deadline itself lives on the connection.
type deadlineRecorder struct {
	http.ResponseWriter
	deadlines []time.Time
}

func (d *deadlineRecorder) SetWriteDeadline(t time.Time) error {
	d.deadlines = append(d.deadlines, t)
	return nil
}

// TestTheFrameArmsAResponseStallDeadlineOnEveryRouteExceptAStream pins both
// halves of the bound, and the second half is the one that matters most.
//
// Arming it: an unauthenticated peer that stops reading must stop holding the
// goroutine, the socket, and whatever the handler is mid-way through producing.
// The frame does it for every route so a new one inherits the bound rather than
// having to remember it — including the F3 proxy fallback's synthetic routes.
//
// NOT arming it on a stream: the subscribeRepos firehose is held open for hours
// and is legitimately silent for all of them, so a write deadline would evict
// exactly the healthy relays it exists to feed. Route.LongLived is the
// declaration; if the frame ever armed the deadline unconditionally, the firehose
// would break in production and nothing else would notice.
func TestTheFrameArmsAResponseStallDeadlineOnEveryRouteExceptAStream(t *testing.T) {
	srv := NewServer(nil, nil, nil, nil, nil)
	srv.Register(Route{
		NSID: "com.atproto.sync.getRepo", Method: http.MethodGet,
		Auth: Public, Class: ClassPublicRead,
		Handle: func(w http.ResponseWriter, _ *http.Request, _ *Caller) { WriteJSON(w, map[string]any{"ok": true}) },
	})
	srv.Register(Route{
		NSID: "com.atproto.sync.subscribeRepos", Method: http.MethodGet,
		Auth: Public, Class: ClassPublicRead, LongLived: true,
		Handle: func(w http.ResponseWriter, _ *http.Request, _ *Caller) { WriteJSON(w, map[string]any{"ok": true}) },
	})

	call := func(nsid string) *deadlineRecorder {
		rec := &deadlineRecorder{ResponseWriter: httptest.NewRecorder()}
		srv.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/xrpc/"+nsid, nil))
		return rec
	}

	before := time.Now()
	ordinary := call("com.atproto.sync.getRepo")
	after := time.Now()
	// Pre-handler arm + the frame writer's per-chunk arm(s) + the final-flush
	// arm — the exact interleaving is pinned by the chunking tests below; here
	// the point is only that an ordinary route is armed at all, with the right
	// window. An unauthenticated reply with no stall bound is a resource one
	// peer holds for free.
	if len(ordinary.deadlines) == 0 {
		t.Fatal("ordinary route saw no write deadline")
	}
	// Every arm happened inside [before, after], so its deadline lies in
	// [before, after] + responseStallTimeout — exactly, however long the call
	// took. A fixed slack past `before` instead reds whenever a loaded box stalls
	// the call by more than the slack (measured 2026-09-14 on Windows: 32.5s).
	for _, d := range ordinary.deadlines {
		if d.Before(before.Add(responseStallTimeout)) || d.After(after.Add(responseStallTimeout)) {
			t.Errorf("armed deadline is %v past the call's start (the call took %v), want responseStallTimeout (%v) past the arm",
				d.Sub(before), after.Sub(before), responseStallTimeout)
		}
	}

	if stream := call("com.atproto.sync.subscribeRepos"); len(stream.deadlines) != 0 {
		t.Errorf("LongLived route saw %d write deadlines, want 0 — a deadline on the firehose evicts healthy, quiet relays", len(stream.deadlines))
	}
}

// TestSetResponseStallDeadlineToleratesAWriterWithoutOne: the deadline is
// hardening on the real serving path, never a correctness precondition. A
// ResponseWriter that cannot carry one (httptest's recorder, and any wrapper that
// does not unwrap) must not turn into a handler failure.
func TestSetResponseStallDeadlineToleratesAWriterWithoutOne(t *testing.T) {
	rec := httptest.NewRecorder()
	SetResponseStallDeadline(rec) // must not panic
	WriteJSON(rec, map[string]any{"ok": true})
	if rec.Code != http.StatusOK {
		t.Errorf("status = %d, want 200", rec.Code)
	}
}

// eventRecorder records, in ONE ordered stream, every deadline arm and every
// write that reaches the connection. Order is the point: a count of arms
// alone cannot distinguish "armed N times up front" from the structural
// property the stall bound depends on — that every connection-level write is
// bounded in size and runs under a deadline armed immediately before it.
type eventRecorder struct {
	*httptest.ResponseRecorder
	events []string
}

func (e *eventRecorder) SetWriteDeadline(time.Time) error {
	e.events = append(e.events, "arm")
	return nil
}

func (e *eventRecorder) Write(b []byte) (int, error) {
	e.events = append(e.events, fmt.Sprintf("write:%d", len(b)))
	return e.ResponseRecorder.Write(b)
}

// TestOneLargeWriteIsChunkedUnderAFreshDeadlinePerChunk is the
// regression pin. sync.getBlob hands its whole
// payload — up to the 50 MiB video ceiling — to ONE unwrapped w.Write on an
// anonymous route. With the deadline armed once per response (or even once
// per Write call), it is a TOTAL transfer budget in disguise: 50 MiB in 30 s
// needs ~14 Mbit/s sustained, so a slower-but-attentive consumer is cut off
// mid-body — invisibly on both ends (the handler's Write returns nil error
// into net/http's buffer; the client sees a bare EOF). Proven by execution
// in that review, with a ProgressWriter control arm.
//
// The structural fix this pins: the frame's writer splits every write into
// chunks of at most stallWriteChunkBytes and re-arms the stall deadline
// immediately before each chunk, so one arming only ever covers one chunk's
// progress — a peer that keeps reading is never cut off, however large the
// body and however slow the link, no matter how the handler writes.
func TestOneLargeWriteIsChunkedUnderAFreshDeadlinePerChunk(t *testing.T) {
	payload := make([]byte, 2*stallWriteChunkBytes+stallWriteChunkBytes/2)
	for i := range payload {
		payload[i] = byte(i)
	}
	srv := NewServer(nil, nil, nil, nil, nil)
	srv.Register(Route{
		NSID: "com.atproto.sync.getBlob", Method: http.MethodGet,
		Auth: Public, Class: ClassPublicRead,
		Handle: func(w http.ResponseWriter, _ *http.Request, _ *Caller) {
			n, err := w.Write(payload) // ONE Write, exactly like getBlob
			if err != nil || n != len(payload) {
				t.Errorf("handler write = (%d, %v), want (%d, nil)", n, err, len(payload))
			}
		},
	})
	rec := &eventRecorder{ResponseRecorder: httptest.NewRecorder()}
	srv.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/xrpc/com.atproto.sync.getBlob", nil))

	if got := rec.Body.Bytes(); !bytes.Equal(got, payload) {
		t.Fatalf("delivered body differs: got %d bytes, want %d — chunking must be invisible to the consumer", len(got), len(payload))
	}
	writes := 0
	for i, ev := range rec.events {
		if !strings.HasPrefix(ev, "write:") {
			continue
		}
		writes++
		var n int
		if _, err := fmt.Sscanf(ev, "write:%d", &n); err != nil {
			t.Fatalf("unparseable event %q", ev)
		}
		if n > stallWriteChunkBytes {
			t.Errorf("event %d: a %d-byte write reached the connection — larger than one chunk (%d), so its deadline covers more than one chunk's progress and is a partial transfer budget", i, n, stallWriteChunkBytes)
		}
		if i == 0 || rec.events[i-1] != "arm" {
			t.Errorf("event %d (%s) is not immediately preceded by a deadline arm — that write runs on whatever is left of a stale window", i, ev)
		}
	}
	if want := 3; writes != want {
		t.Errorf("payload reached the connection in %d writes, want %d chunks (full, full, half)", writes, want)
	}
}

// TestALongLivedRouteIsNeverWrappedOrArmed: the subscribeRepos firehose is
// held open for hours, upgrades to a WebSocket inside its handler, and bounds
// its own writes (per-frame timeout + ping/pong). The frame must hand it the
// RAW ResponseWriter — no deadline, no chunking wrapper — both because a
// deadline would evict healthy, quiet relays and because the upgrade path
// negotiates against the connection's real writer.
func TestALongLivedRouteIsNeverWrappedOrArmed(t *testing.T) {
	payload := make([]byte, 2*stallWriteChunkBytes)
	rec := &eventRecorder{ResponseRecorder: httptest.NewRecorder()}
	var sawRaw bool
	srv := NewServer(nil, nil, nil, nil, nil)
	srv.Register(Route{
		NSID: "com.atproto.sync.subscribeRepos", Method: http.MethodGet,
		Auth: Public, Class: ClassPublicRead, LongLived: true,
		Handle: func(w http.ResponseWriter, _ *http.Request, _ *Caller) {
			sawRaw = w == http.ResponseWriter(rec)
			_, _ = w.Write(payload)
		},
	})
	srv.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/xrpc/com.atproto.sync.subscribeRepos", nil))
	if !sawRaw {
		t.Error("LongLived handler did not receive the raw ResponseWriter")
	}
	if len(rec.events) != 1 || rec.events[0] != fmt.Sprintf("write:%d", len(payload)) {
		t.Errorf("events = %v, want exactly one unchunked write and no arms — the stream owns its own write bounds", rec.events)
	}
}

// TestProgressWriterOverTheFramesWriterStillArmsTheConnection: getRepo and
// the proxy relay wrap the frame-provided writer in their own ProgressWriter
// (they need Written() to know whether the status code is spent). Its re-arm
// reaches the connection through http.ResponseController, which unwraps via
// Unwrap — this pins that the frame's wrapper participates in that chain
// rather than silently swallowing the deadline.
func TestProgressWriterOverTheFramesWriterStillArmsTheConnection(t *testing.T) {
	srv := NewServer(nil, nil, nil, nil, nil)
	srv.Register(Route{
		NSID: "com.atproto.sync.getRepo", Method: http.MethodGet,
		Auth: Public, Class: ClassPublicRead,
		Handle: func(w http.ResponseWriter, _ *http.Request, _ *Caller) {
			pw := NewProgressWriter(w)
			if _, err := pw.Write([]byte("hello")); err != nil {
				t.Errorf("write through ProgressWriter: %v", err)
			}
			if pw.Written() != 5 {
				t.Errorf("Written() = %d, want 5", pw.Written())
			}
		},
	})
	rec := &eventRecorder{ResponseRecorder: httptest.NewRecorder()}
	srv.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/xrpc/com.atproto.sync.getRepo", nil))
	arms := 0
	for _, ev := range rec.events {
		if ev == "arm" {
			arms++
		}
	}
	// Frame pre-arm + ProgressWriter's own + the wrapper's chunk arm +
	// the frame's final-flush arm. The load-bearing half is that the arms
	// reached THIS recorder at all — through Unwrap, not dying at a wrapper
	// with no deadline of its own.
	if arms < 3 {
		t.Errorf("saw %d deadline arms, want ≥3 — the Unwrap chain from ProgressWriter through the frame's writer to the connection is broken", arms)
	}
}
