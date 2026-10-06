package atprotolex

// The permission-set document cache (atproto-pds-full.md § F4 detail →
// *Permission sets*, "The document cache carries the spec's constants").
//
// # What this cache is, and what it is emphatically not
//
// It serves only NEW ceremonies. A standing grant never re-resolves anything —
// the expansion is frozen at the consent card for the grant's whole life — so
// the durable "stale copy" that matters is the GRANT ROW, not this map. That is
// what makes a bridge restart lose nothing that matters: an empty cache costs
// the next authorization request one resolution chain and costs existing
// sessions nothing at all.
//
// It is therefore never authoritative. Every entry can be recomputed by
// resolving again, so an eviction, a race or a cold start costs latency and
// never correctness — the same license the client-metadata cache records, and
// what permits the crude eviction policy below.

import (
	"context"
	"sync"
	"time"
)

const (
	// SetCacheStaleAfter / SetCacheExpireAfter are the Lexicon spec's own
	// document lifetimes (24 h stale, 90 d expiration). Below the stale
	// horizon a cached document is served outright; between the two it is
	// revalidated, and kept if revalidation fails; past expiry it is gone.
	SetCacheStaleAfter  = 24 * time.Hour
	SetCacheExpireAfter = 90 * 24 * time.Hour

	// SetCacheNegativeTTL is how long a FAILED resolution stays cached — the
	// client-metadata cache's constant and its reasoning, unchanged: caching
	// only the positive half is what would make an unresolvable NSID a fetch
	// amplifier, since an attacker picks the NSID and every miss is a fresh
	// DNS+DID+HTTPS chain. Deliberately short, and deliberately far shorter
	// than the stale horizon: a failure must never be stickier than a success.
	SetCacheNegativeTTL = time.Minute

	// setCacheCapacity bounds the map. An authorization request names the
	// NSIDs, so the key space is attacker-chosen and unbounded; eviction can
	// only ever cost a re-resolution.
	setCacheCapacity = 512
)

// DocumentResolver is what the cache wraps. SetResolver satisfies it.
type DocumentResolver interface {
	ResolveSetDocument(ctx context.Context, nsid string) ([]byte, error)
}

type setCacheEntry struct {
	// record is the verified document on a positive entry, nil on a negative
	// one; err carries the refusal on a negative entry. Exactly one is set —
	// both a success and a refusal are real answers worth remembering.
	record []byte
	err    error
	// at is when the answer was obtained, not when it expires: this cache has
	// three horizons off the same instant, so storing the instant keeps one
	// source of truth for all three.
	at time.Time
}

// CachingSetResolver wraps a DocumentResolver with the spec's document cache.
// It satisfies DocumentResolver itself, so callers hold one type either way.
type CachingSetResolver struct {
	inner DocumentResolver

	mu      sync.Mutex
	entries map[string]setCacheEntry
	now     func() time.Time
}

// NewCachingSetResolver wraps inner. now is the clock seam — nil means
// time.Now; tests drive the horizons with a fake, because a cache measured in
// days cannot be tested by waiting.
func NewCachingSetResolver(inner DocumentResolver, now func() time.Time) *CachingSetResolver {
	if now == nil {
		now = time.Now
	}
	return &CachingSetResolver{inner: inner, entries: map[string]setCacheEntry{}, now: now}
}

// ResolveSetDocument answers from cache where the spec's lifetimes allow, and
// otherwise resolves through.
//
// The four cases, in the order they are decided:
//
//   - a cached FAILURE inside the negative TTL — answered as-is;
//   - a FRESH document (younger than the stale horizon) — answered as-is;
//   - a STALE document (past stale, inside expiry) — revalidated, and kept if
//     revalidation fails (stale-on-failure);
//   - anything else — resolved, and the outcome cached either way.
//
// Note what is deliberately absent: single-flight. Two concurrent ceremonies
// naming the same cold set perform two resolution chains, which is bounded by
// the same per-request fan-out cap that bounds one of them, and collapsing them
// would introduce a shared failure the frozen-at-ceremony rule exists to avoid.
func (c *CachingSetResolver) ResolveSetDocument(ctx context.Context, nsid string) ([]byte, error) {
	if cached, ok, stale := c.lookup(nsid); ok && !stale {
		return cached.record, cached.err
	} else if ok && stale {
		// Revalidate; on failure the stale copy still stands.
		record, err := c.inner.ResolveSetDocument(ctx, nsid)
		if err != nil {
			return cached.record, nil
		}
		c.store(nsid, setCacheEntry{record: record, at: c.now()})
		return record, nil
	}

	record, err := c.inner.ResolveSetDocument(ctx, nsid)
	c.store(nsid, setCacheEntry{record: record, err: err, at: c.now()})
	return record, err
}

// lookup classifies the cached entry: (entry, usable, needsRevalidation).
//
// `usable && stale` is reachable only for a positive entry — a failure is
// either fresh enough to answer with or gone, because "revalidate but keep the
// old failure on failure" would just be a longer negative TTL wearing a
// disguise.
func (c *CachingSetResolver) lookup(nsid string) (setCacheEntry, bool, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	e, ok := c.entries[nsid]
	if !ok {
		return setCacheEntry{}, false, false
	}
	age := c.now().Sub(e.at)
	if e.err != nil {
		if age < SetCacheNegativeTTL {
			return e, true, false
		}
		delete(c.entries, nsid)
		return setCacheEntry{}, false, false
	}
	switch {
	case age < SetCacheStaleAfter:
		return e, true, false
	case age < SetCacheExpireAfter:
		return e, true, true
	default:
		// Past expiry a copy is not evidence of anything current: a set deleted
		// or narrowed months ago must not keep widening fresh grants.
		delete(c.entries, nsid)
		return setCacheEntry{}, false, false
	}
}

func (c *CachingSetResolver) store(nsid string, e setCacheEntry) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if len(c.entries) >= setCacheCapacity {
		c.evictLocked()
	}
	c.entries[nsid] = e
}

// evictLocked drops entries that are past every horizon, and if that freed
// nothing, the oldest. Oldest rather than least-recently-used because the cache
// keeps no access record and adding one would buy nothing: evicting the wrong
// entry costs one re-resolution.
func (c *CachingSetResolver) evictLocked() {
	now := c.now()
	freed := false
	for k, e := range c.entries {
		dead := e.err != nil && now.Sub(e.at) >= SetCacheNegativeTTL
		if dead || now.Sub(e.at) >= SetCacheExpireAfter {
			delete(c.entries, k)
			freed = true
		}
	}
	if freed {
		return
	}
	var oldestKey string
	var oldest time.Time
	for k, e := range c.entries {
		if oldestKey == "" || e.at.Before(oldest) {
			oldestKey, oldest = k, e.at
		}
	}
	if oldestKey != "" {
		delete(c.entries, oldestKey)
	}
}

// len reports the entry count. Test-facing: the bound is a property worth
// asserting, and asserting it through the mutex is better than exporting the
// map.
func (c *CachingSetResolver) len() int {
	c.mu.Lock()
	defer c.mu.Unlock()
	return len(c.entries)
}

// ── The TXT cache ────────────────────────────────────────────────────────────

const (
	// TXTCacheTTL is how long a `_lexicon` TXT answer is reused. Short on
	// purpose: the Lexicon spec cautions specifically against caching DNS
	// answers for long periods, and DNS is the ONE leg of this chain that is
	// trusted rather than verified — an authority that re-points its NSIDs at a
	// new DID (or recovers from a hijacked record) must take effect in minutes.
	// The document cache above absorbs the repeat-ceremony traffic, so this
	// layer exists for a different case entirely: several sets from the same
	// authority inside one authorization request.
	TXTCacheTTL = 5 * time.Minute

	// txtCacheCapacity bounds the map; the names derive from attacker-chosen
	// NSIDs, and eviction costs one lookup.
	txtCacheCapacity = 512
)

type txtCacheEntry struct {
	records []string
	at      time.Time
}

// CachingTXTResolver is a short-TTL cache over TXT lookups. It satisfies
// atprotoid.TXTResolver, so it composes into SetResolver.TXT without anything
// downstream knowing it is there.
//
// Failures are deliberately NOT cached. A DNS failure is transient far more
// often than a document-resolution failure is, and the negative answer worth
// remembering — this NSID does not resolve — is already held by the document
// cache one leg up, keyed by the NSID an attacker actually chose rather than by
// the authority it happened to derive to.
type CachingTXTResolver struct {
	inner interface {
		LookupTXT(ctx context.Context, name string) ([]string, error)
	}

	mu      sync.Mutex
	entries map[string]txtCacheEntry
	now     func() time.Time
}

// NewCachingTXTResolver wraps inner. now is the clock seam; nil means time.Now.
func NewCachingTXTResolver(inner interface {
	LookupTXT(ctx context.Context, name string) ([]string, error)
}, now func() time.Time) *CachingTXTResolver {
	if now == nil {
		now = time.Now
	}
	return &CachingTXTResolver{inner: inner, entries: map[string]txtCacheEntry{}, now: now}
}

func (c *CachingTXTResolver) LookupTXT(ctx context.Context, name string) ([]string, error) {
	if records, ok := c.get(name); ok {
		return records, nil
	}
	records, err := c.inner.LookupTXT(ctx, name)
	if err != nil {
		return nil, err
	}
	c.put(name, records)
	return records, nil
}

func (c *CachingTXTResolver) get(name string) ([]string, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	e, ok := c.entries[name]
	if !ok {
		return nil, false
	}
	if c.now().Sub(e.at) >= TXTCacheTTL {
		delete(c.entries, name)
		return nil, false
	}
	// A copy: the caller iterates these and a shared slice would let one
	// caller's handling mutate what the next one reads.
	out := make([]string, len(e.records))
	copy(out, e.records)
	return out, true
}

func (c *CachingTXTResolver) put(name string, records []string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if len(c.entries) >= txtCacheCapacity {
		now := c.now()
		freed := false
		for k, e := range c.entries {
			if now.Sub(e.at) >= TXTCacheTTL {
				delete(c.entries, k)
				freed = true
			}
		}
		if !freed {
			var oldestKey string
			var oldest time.Time
			for k, e := range c.entries {
				if oldestKey == "" || e.at.Before(oldest) {
					oldestKey, oldest = k, e.at
				}
			}
			if oldestKey != "" {
				delete(c.entries, oldestKey)
			}
		}
	}
	stored := make([]string, len(records))
	copy(stored, records)
	c.entries[name] = txtCacheEntry{records: stored, at: c.now()}
}

func (c *CachingTXTResolver) len() int {
	c.mu.Lock()
	defer c.mu.Unlock()
	return len(c.entries)
}
