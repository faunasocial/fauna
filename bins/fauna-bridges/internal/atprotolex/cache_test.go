package atprotolex

import (
	"context"
	"errors"
	"testing"
	"time"
)

// fakeClock is the convention-14 seam: every horizon in this cache is measured
// in hours and days, so a test that waited for one would be untestable, not
// merely slow. Time advances because the test says so.
type fakeClock struct{ t time.Time }

func (c *fakeClock) now() time.Time          { return c.t }
func (c *fakeClock) advance(d time.Duration) { c.t = c.t.Add(d) }
func newFakeClock() *fakeClock               { return &fakeClock{t: time.Unix(1_800_000_000, 0).UTC()} }

// countingResolver is the inner resolver, scripted per call.
type countingResolver struct {
	record []byte
	err    error
	calls  int
}

func (r *countingResolver) ResolveSetDocument(_ context.Context, _ string) ([]byte, error) {
	r.calls++
	if r.err != nil {
		return nil, r.err
	}
	return r.record, nil
}

func newCacheFixture(t *testing.T) (*CachingSetResolver, *countingResolver, *fakeClock) {
	t.Helper()
	inner := &countingResolver{record: []byte("document-v1")}
	clock := newFakeClock()
	return NewCachingSetResolver(inner, clock.now), inner, clock
}

// TestSetCacheServesAFreshEntryWithoutRefetching — the cache's whole job. A
// cold-cache PAR pays one resolution chain per include; a warm one pays none.
func TestSetCacheServesAFreshEntryWithoutRefetching(t *testing.T) {
	c, inner, clock := newCacheFixture(t)
	ctx := context.Background()

	for i := 0; i < 3; i++ {
		got, err := c.ResolveSetDocument(ctx, testSetNSID)
		if err != nil {
			t.Fatalf("call %d: %v", i, err)
		}
		if string(got) != "document-v1" {
			t.Errorf("call %d: got %q", i, got)
		}
		clock.advance(time.Hour) // still well inside the stale horizon
	}
	if inner.calls != 1 {
		t.Errorf("want exactly one resolution, got %d", inner.calls)
	}
}

// TestSetCacheRevalidatesOnceStale — 24 h is the spec's stale lifetime, and
// past it the authority gets asked again.
func TestSetCacheRevalidatesOnceStale(t *testing.T) {
	c, inner, clock := newCacheFixture(t)
	ctx := context.Background()

	if _, err := c.ResolveSetDocument(ctx, testSetNSID); err != nil {
		t.Fatal(err)
	}
	clock.advance(SetCacheStaleAfter + time.Minute)
	inner.record = []byte("document-v2")

	got, err := c.ResolveSetDocument(ctx, testSetNSID)
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != "document-v2" {
		t.Errorf("a stale entry must be revalidated, got %q", got)
	}
	if inner.calls != 2 {
		t.Errorf("want a second resolution, got %d", inner.calls)
	}
}

// TestSetCacheServesStaleOnFailure — the spec explicitly allows it, and it is
// what stops a third party's DNS outage taking down NEW authorization
// ceremonies for a set this box already verified once.
func TestSetCacheServesStaleOnFailure(t *testing.T) {
	c, inner, clock := newCacheFixture(t)
	ctx := context.Background()

	if _, err := c.ResolveSetDocument(ctx, testSetNSID); err != nil {
		t.Fatal(err)
	}
	clock.advance(SetCacheStaleAfter + time.Minute)
	inner.err = errors.New("authority unreachable")

	got, err := c.ResolveSetDocument(ctx, testSetNSID)
	if err != nil {
		t.Fatalf("a stale copy should have been served: %v", err)
	}
	if string(got) != "document-v1" {
		t.Errorf("got %q, want the stale copy", got)
	}
}

// TestSetCacheRefusesPastExpiry — stale-on-failure has a floor. Past 90 days a
// copy is not evidence of anything current, and serving it would let a set
// that was deleted or narrowed years ago keep widening fresh grants.
func TestSetCacheRefusesPastExpiry(t *testing.T) {
	c, inner, clock := newCacheFixture(t)
	ctx := context.Background()

	if _, err := c.ResolveSetDocument(ctx, testSetNSID); err != nil {
		t.Fatal(err)
	}
	clock.advance(SetCacheExpireAfter + time.Hour)
	inner.err = errors.New("authority unreachable")

	if _, err := c.ResolveSetDocument(ctx, testSetNSID); err == nil {
		t.Fatal("want a refusal: an expired copy must not be served, even on failure")
	}
}

// TestSetCacheCachesFailuresBriefly — a negative entry is a real answer. Not
// caching it would make an unresolvable NSID a fetch amplifier, the exact
// reasoning the client-metadata cache records; and the TTL is short because a
// broken authority coming back should be noticed in about a minute.
func TestSetCacheCachesFailuresBriefly(t *testing.T) {
	c, inner, clock := newCacheFixture(t)
	ctx := context.Background()
	inner.err = errors.New("no such authority")

	for i := 0; i < 3; i++ {
		if _, err := c.ResolveSetDocument(ctx, testSetNSID); err == nil {
			t.Fatalf("call %d: want the failure", i)
		}
	}
	if inner.calls != 1 {
		t.Errorf("a repeated failure should be answered from cache, got %d resolutions", inner.calls)
	}

	clock.advance(SetCacheNegativeTTL + time.Second)
	inner.err = nil
	got, err := c.ResolveSetDocument(ctx, testSetNSID)
	if err != nil {
		t.Fatalf("after the negative TTL the authority must be retried: %v", err)
	}
	if string(got) != "document-v1" {
		t.Errorf("got %q", got)
	}
}

// TestSetCacheNegativeTTLIsShorterThanTheStaleHorizon — the sanity pin the
// client-metadata cache carries: a failure must never be stickier than a
// success, or one outage would outlast the document it failed to fetch.
func TestSetCacheNegativeTTLIsShorterThanTheStaleHorizon(t *testing.T) {
	if SetCacheNegativeTTL >= SetCacheStaleAfter {
		t.Fatalf("negative TTL %v must be shorter than the stale horizon %v",
			SetCacheNegativeTTL, SetCacheStaleAfter)
	}
	if SetCacheStaleAfter >= SetCacheExpireAfter {
		t.Fatalf("stale horizon %v must be shorter than expiry %v",
			SetCacheStaleAfter, SetCacheExpireAfter)
	}
}

// TestSetCacheKeysPerNSID — two sets are two documents; a cache that collapsed
// them would serve one authority's permissions under another's name.
func TestSetCacheKeysPerNSID(t *testing.T) {
	c, inner, _ := newCacheFixture(t)
	ctx := context.Background()

	if _, err := c.ResolveSetDocument(ctx, "com.example.calendar.appPerms"); err != nil {
		t.Fatal(err)
	}
	if _, err := c.ResolveSetDocument(ctx, "com.example.calendar.otherPerms"); err != nil {
		t.Fatal(err)
	}
	if inner.calls != 2 {
		t.Errorf("each NSID resolves on its own, got %d resolutions", inner.calls)
	}
}

// TestSetCacheIsBounded — an attacker can name unboundedly many NSIDs in
// successive authorization requests, so the map must not grow with them.
func TestSetCacheIsBounded(t *testing.T) {
	c, _, clock := newCacheFixture(t)
	ctx := context.Background()

	for i := 0; i < setCacheCapacity*2; i++ {
		nsid := "com.example.flood.set" + string(rune('a'+i%26)) + string(rune('a'+i/26%26)) + string(rune('a'+i/676%26))
		if _, err := c.ResolveSetDocument(ctx, nsid); err != nil {
			t.Fatalf("resolve %s: %v", nsid, err)
		}
		clock.advance(time.Second)
	}
	if n := c.len(); n > setCacheCapacity {
		t.Errorf("cache holds %d entries, past the %d bound", n, setCacheCapacity)
	}
}

// countingTXT counts lookups per name.
type countingTXT struct {
	records map[string][]string
	err     error
	calls   int
}

func (r *countingTXT) LookupTXT(_ context.Context, name string) ([]string, error) {
	r.calls++
	if r.err != nil {
		return nil, r.err
	}
	v, ok := r.records[name]
	if !ok {
		return nil, errors.New("NXDOMAIN")
	}
	return v, nil
}

// TestTXTCacheCollapsesRepeatLookups — the case the document cache cannot
// reach: one authorization request naming several sets from the SAME authority
// resolves that authority's TXT record once, not once per set.
func TestTXTCacheCollapsesRepeatLookups(t *testing.T) {
	inner := &countingTXT{records: map[string][]string{
		"_lexicon.calendar.example.com": {"did=did:plc:auth0000000000000000000"},
	}}
	clock := newFakeClock()
	c := NewCachingTXTResolver(inner, clock.now)
	ctx := context.Background()

	for i := 0; i < 4; i++ {
		got, err := c.LookupTXT(ctx, "_lexicon.calendar.example.com")
		if err != nil {
			t.Fatalf("lookup %d: %v", i, err)
		}
		if len(got) != 1 || got[0] != "did=did:plc:auth0000000000000000000" {
			t.Errorf("lookup %d: got %v", i, got)
		}
	}
	if inner.calls != 1 {
		t.Errorf("want one DNS lookup, got %d", inner.calls)
	}
}

// TestTXTCacheKeepsItShort — the Lexicon spec cautions specifically against
// caching DNS answers for long periods: an authority re-pointing its NSIDs at
// a new DID must take effect in minutes, not days.
func TestTXTCacheKeepsItShort(t *testing.T) {
	inner := &countingTXT{records: map[string][]string{
		"_lexicon.calendar.example.com": {"did=did:plc:first000000000000000000"},
	}}
	clock := newFakeClock()
	c := NewCachingTXTResolver(inner, clock.now)
	ctx := context.Background()

	if _, err := c.LookupTXT(ctx, "_lexicon.calendar.example.com"); err != nil {
		t.Fatal(err)
	}
	clock.advance(TXTCacheTTL + time.Second)
	inner.records["_lexicon.calendar.example.com"] = []string{"did=did:plc:second00000000000000000"}

	got, err := c.LookupTXT(ctx, "_lexicon.calendar.example.com")
	if err != nil {
		t.Fatal(err)
	}
	if got[0] != "did=did:plc:second00000000000000000" {
		t.Errorf("past the TTL the record must be re-read, got %v", got)
	}
	if TXTCacheTTL > 15*time.Minute {
		t.Errorf("TXT cache TTL %v is not 'short'", TXTCacheTTL)
	}
}

// TestTXTCacheDoesNotCacheFailures — a DNS failure is transient far more often
// than a document-resolution failure is, and the negative answer that matters
// (an authority that publishes nothing) is already held by the document cache
// one leg up, where it is keyed by the NSID an attacker actually chose.
func TestTXTCacheDoesNotCacheFailures(t *testing.T) {
	inner := &countingTXT{err: errors.New("SERVFAIL")}
	clock := newFakeClock()
	c := NewCachingTXTResolver(inner, clock.now)
	ctx := context.Background()

	for i := 0; i < 3; i++ {
		if _, err := c.LookupTXT(ctx, "_lexicon.calendar.example.com"); err == nil {
			t.Fatalf("lookup %d: want the failure", i)
		}
	}
	if inner.calls != 3 {
		t.Errorf("a DNS failure must not be cached, got %d lookups", inner.calls)
	}
}

// TestTXTCacheIsBounded — the names come from attacker-chosen NSIDs.
func TestTXTCacheIsBounded(t *testing.T) {
	inner := &countingTXT{records: map[string][]string{}}
	clock := newFakeClock()
	c := NewCachingTXTResolver(inner, clock.now)
	ctx := context.Background()

	for i := 0; i < txtCacheCapacity*2; i++ {
		name := "_lexicon.a" + string(rune('a'+i%26)) + string(rune('a'+i/26%26)) + string(rune('a'+i/676%26)) + ".example.com"
		inner.records[name] = []string{"did=did:plc:x"}
		if _, err := c.LookupTXT(ctx, name); err != nil {
			t.Fatalf("lookup %s: %v", name, err)
		}
		clock.advance(time.Second)
	}
	if n := c.len(); n > txtCacheCapacity {
		t.Errorf("TXT cache holds %d entries, past the %d bound", n, txtCacheCapacity)
	}
}
