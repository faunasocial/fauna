package imap

import (
	"container/list"
	"sync"
	"sync/atomic"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
)

// bodyStructureCacheKey identifies a derived BODYSTRUCTURE+Envelope pair
// uniquely. (actor_id, mailbox, uid_validity, uid) is the full
// coordinates: a UIDVALIDITY change on a re-SELECT invalidates cached
// entries automatically because the key embeds it.
//
// actorID is stored as a `string` (Go's immutable byte view) so the
// cache key is comparable for use as a map key.
type bodyStructureCacheKey struct {
	actorID     string
	mailbox     string
	uidValidity uint32
	uid         uint32
}

type bodyStructureCacheEntry struct {
	key      bodyStructureCacheKey
	bs       mailfauna.BodyStructure
	envelope mailfauna.Envelope
}

// bodyStructureCache is a process-wide LRU cache of derived
// BODYSTRUCTURE + ENVELOPE pairs keyed by (actor_id, mailbox,
// uid_validity, uid). The cap is sized from `cfg.IMAP.BodyStructureCacheMax`
// at Backend construction. A cap of 0 disables caching entirely (every
// Get is a miss; every Put no-ops) — useful in tests and for admins
// who want to deopt the cache without redeploying.
//
// Thread-safe; backed by a doubly-linked list + map for O(1) LRU
// operations. The actor_id portion of the key is the 32-byte sender
// identity converted to string(bytes) — same comparable trick the
// stdlib uses for sets keyed on byte slices.
//
// `max` is an atomic so Resize (a config_changed hot-apply of
// `imap.bodystructure_cache_max`) can change the cap without taking the
// LRU mutex on the hot Get/Put fast path: the max==0 short-circuit and the
// eviction-threshold compare read it via Load(); Resize stores the new cap
// and evicts down to it under the mutex.
type bodyStructureCache struct {
	mu  sync.Mutex
	max atomic.Int64
	lru *list.List // front = MRU; back = LRU
	idx map[bodyStructureCacheKey]*list.Element
}

func newBodyStructureCache(max int) *bodyStructureCache {
	c := &bodyStructureCache{
		lru: list.New(),
		idx: make(map[bodyStructureCacheKey]*list.Element, max),
	}
	c.max.Store(int64(max))
	return c
}

// Get returns the cached (BodyStructure, Envelope) pair if present;
// the boolean third return value is true on hit, false on miss. A hit
// promotes the entry to MRU. Get on a max=0 cache is always a miss.
func (c *bodyStructureCache) Get(k bodyStructureCacheKey) (mailfauna.BodyStructure, mailfauna.Envelope, bool) {
	if c.max.Load() == 0 {
		return mailfauna.BodyStructure{}, mailfauna.Envelope{}, false
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	e, ok := c.idx[k]
	if !ok {
		return mailfauna.BodyStructure{}, mailfauna.Envelope{}, false
	}
	c.lru.MoveToFront(e)
	entry := e.Value.(*bodyStructureCacheEntry)
	return entry.bs, entry.envelope, true
}

// Put stores or refreshes the (BodyStructure, Envelope) pair for k.
// On insert into a full cache, the LRU entry is evicted. Put on a
// max=0 cache no-ops.
func (c *bodyStructureCache) Put(k bodyStructureCacheKey, bs mailfauna.BodyStructure, env mailfauna.Envelope) {
	if c.max.Load() == 0 {
		return
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if e, ok := c.idx[k]; ok {
		c.lru.MoveToFront(e)
		entry := e.Value.(*bodyStructureCacheEntry)
		entry.bs = bs
		entry.envelope = env
		return
	}
	entry := &bodyStructureCacheEntry{key: k, bs: bs, envelope: env}
	c.idx[k] = c.lru.PushFront(entry)
	if int64(c.lru.Len()) > c.max.Load() {
		oldest := c.lru.Back()
		if oldest != nil {
			c.lru.Remove(oldest)
			delete(c.idx, oldest.Value.(*bodyStructureCacheEntry).key)
		}
	}
}

// Resize changes the cache cap (a config_changed hot-apply of
// `imap.bodystructure_cache_max`) and immediately evicts the LRU tail down to
// the new cap — so lowering it frees memory at once rather than waiting for
// organic eviction, and a new cap of 0 clears + disables the cache. Raising it
// just admits more entries on subsequent Puts. Safe to call concurrently with
// Get/Put; the LRU surgery is under the mutex, the cap read on the hot path is
// atomic.
func (c *bodyStructureCache) Resize(newMax int) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.max.Store(int64(newMax))
	for int64(c.lru.Len()) > c.max.Load() {
		oldest := c.lru.Back()
		if oldest == nil {
			break
		}
		c.lru.Remove(oldest)
		delete(c.idx, oldest.Value.(*bodyStructureCacheEntry).key)
	}
}
