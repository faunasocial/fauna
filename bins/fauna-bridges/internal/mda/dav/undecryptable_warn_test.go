package dav

import (
	"bytes"
	"log/slog"
	"strings"
	"sync"
	"testing"
)

// newCountingLogger returns a logger writing to buf at DEBUG level (so both
// tiers are captured) and the buffer to inspect.
func newCountingLogger() (*slog.Logger, *bytes.Buffer) {
	var buf bytes.Buffer
	h := slog.NewTextHandler(&buf, &slog.HandlerOptions{Level: slog.LevelDebug})
	return slog.New(h), &buf
}

func countLevel(buf *bytes.Buffer, level string) int {
	return strings.Count(buf.String(), "level="+level)
}

// The flood this exists to stop: the same collection reported on every
// PROPFIND must alert once, then fall to DEBUG.
func TestRepeatedCollectionWarnsOnceThenDebugs(t *testing.T) {
	logger, buf := newCountingLogger()
	var d UndecryptableWarnDedup

	for range 50 {
		d.Log(logger, "cal-a", "caldav: skipping calendar with undecryptable metadata")
	}

	if got := countLevel(buf, "WARN"); got != 1 {
		t.Fatalf("WARN count = %d, want exactly 1 across 50 reports of one calendar", got)
	}
	if got := countLevel(buf, "DEBUG"); got != 49 {
		t.Fatalf("DEBUG count = %d, want the other 49 reports", got)
	}
}

// Dedup is per collection — a second bad calendar is its own alert, never
// swallowed by the first one's.
func TestEachCollectionGetsItsOwnWarn(t *testing.T) {
	logger, buf := newCountingLogger()
	var d UndecryptableWarnDedup

	for range 10 {
		d.Log(logger, "cal-a", "skipping")
		d.Log(logger, "cal-b", "skipping")
		d.Log(logger, "cal-c", "skipping")
	}

	if got := countLevel(buf, "WARN"); got != 3 {
		t.Fatalf("WARN count = %d, want one per distinct collection (3)", got)
	}
}

// Past the cap the dedup degrades to NOISY, never to silent — an unbounded
// client-chosen key space must not be able to suppress the alert for a
// collection the box has never warned about.
func TestPastTheCapEveryCollectionStillWarns(t *testing.T) {
	logger, buf := newCountingLogger()
	var d UndecryptableWarnDedup

	for i := range maxTrackedUndecryptableCollections {
		d.Log(logger, string(rune(i))+"-filler", "skipping")
	}
	warnsAfterFill := countLevel(buf, "WARN")

	// A fresh id past the cap: warns every time rather than being silently
	// deduped against an entry that was never recorded.
	for range 5 {
		d.Log(logger, "past-the-cap", "skipping")
	}

	if got := countLevel(buf, "WARN") - warnsAfterFill; got != 5 {
		t.Fatalf("post-cap WARN count = %d, want 5 (degrade to noisy, never silent)", got)
	}
}

// Two goroutines racing on the same unseen id must not both claim the first
// sighting — that is the double-WARN the re-check under the lock prevents.
func TestConcurrentFirstSightingWarnsOnce(t *testing.T) {
	logger, buf := newCountingLogger()
	var d UndecryptableWarnDedup

	var wg sync.WaitGroup
	start := make(chan struct{})
	for range 64 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			<-start
			d.Log(logger, "contended", "skipping")
		}()
	}
	close(start)
	wg.Wait()

	if got := countLevel(buf, "WARN"); got != 1 {
		t.Fatalf("WARN count = %d under 64 racing callers, want exactly 1", got)
	}
}

// A nil logger is a no-op rather than a panic (a Backend built without one).
func TestNilLoggerIsANoOp(t *testing.T) {
	var d UndecryptableWarnDedup
	d.Log(nil, "cal-a", "skipping")
}
