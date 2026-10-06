package davauth

import (
	"sync"
	"time"
)

// authMaterialTTL bounds how long the auth middleware reuses a cached
// set of nest-fetched auth material for one (actor, credential) before
// re-fetching.
//
// CalDAV is stateless HTTP Basic auth, so every request re-runs the AUTH
// flow. Without this cache the MDA re-fetches the wrapped MLS blob — and
// the MLS snapshot — from nest on EVERY request (PROPFIND/REPORT/PUT). A
// burst (two MUAs syncing plus the IMAP poller, all sharing the actor's
// "default" credential bucket) then blows past nest's
// per-(bridge,actor,credential) `bridge_rate_limit` (30 events / 60 s,
// `bins/fauna-nest/src/bridge_rate_limit.rs`), nest returns rate-limited
// on `fetch_wrapped_mls_blob`, and the MDA 401s a legitimate request.
//
// Caching the (password-independent) fetched material for a short window
// collapses a request burst for one (actor, credential) to a single nest
// fetch while keeping the per-request AEAD-unwrap intact (see resolve()),
// so it does NOT weaken "AEAD-success = AUTH-success". The window is
// deliberately short so a credential rotation propagates quickly — the
// worst case is the rotated-away password staying valid for at most this
// long. `caldav-server.md` § Goal sanctions the MDA holding "an in-memory
// cache ... for the AUTH'd session lifetime"; for stateless HTTP this TTL
// window is that "session".
const authMaterialTTL = 30 * time.Second

// authMaterial is the password-independent material the CalDAV auth flow
// fetches from nest to build a Session: the AUTH'd actor id, the wrapped
// MLS blob (ciphertext), the actor's MLS + index pubkeys, and the
// encrypted MLS-snapshot blob. Every field is either ciphertext or a
// public key, so caching it in MDA memory is no more sensitive than
// nest's at-rest copy. The user's password is NEVER stored here; the
// per-request AEAD-unwrap of wrappedBlob (resolve()) remains the AUTH
// signal, so a cache hit still verifies the password.
type authMaterial struct {
	actorID      []byte
	wrappedBlob  []byte
	pubkey       []byte
	mlkemEk      []byte // the ML-KEM half of pubkey; set whenever pubkey is
	indexKey     []byte // nil when nest has no index key on file
	snapshotBlob []byte // nil when nest has no snapshot on file
}

type authMaterialEntry struct {
	material *authMaterial
	expires  time.Time
}

// authMaterialCache is a small TTL cache of authMaterial keyed by
// (local, domain, credential). Safe for concurrent use. Entries expire
// after authMaterialTTL; expired entries are evicted opportunistically on
// access so the map tracks at most the actors that authenticated within
// the last TTL.
type authMaterialCache struct {
	mu      sync.Mutex
	entries map[string]authMaterialEntry
}

func newAuthMaterialCache() *authMaterialCache {
	return &authMaterialCache{entries: make(map[string]authMaterialEntry)}
}

// materialKey builds the cache key. NUL separators are unambiguous (none
// of the three parts can contain a NUL). A bare username and its explicit
// "+default" form both resolve to credentialID="default" → same key →
// one cache entry (a feature: the shared blob is fetched once).
func materialKey(local, domain, credentialID string) string {
	return local + "\x00" + domain + "\x00" + credentialID
}

// get returns the cached material for key if present and unexpired at
// `now`. A miss (absent or expired) returns (nil, false); an expired
// entry is evicted on the way out.
func (c *authMaterialCache) get(key string, now time.Time) (*authMaterial, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	entry, ok := c.entries[key]
	if !ok {
		return nil, false
	}
	if !now.Before(entry.expires) {
		delete(c.entries, key)
		return nil, false
	}
	return entry.material, true
}

// put stores material for key, expiring authMaterialTTL after `now`, and
// opportunistically evicts other already-expired entries so the map stays
// bounded by the set of actors active within the last TTL.
func (c *authMaterialCache) put(key string, material *authMaterial, now time.Time) {
	c.mu.Lock()
	defer c.mu.Unlock()
	for k, e := range c.entries {
		if !now.Before(e.expires) {
			delete(c.entries, k)
		}
	}
	c.entries[key] = authMaterialEntry{material: material, expires: now.Add(authMaterialTTL)}
}
