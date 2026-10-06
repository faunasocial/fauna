package imap

import (
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
)

// makeBS returns a tiny BodyStructure stub for cache tests — content
// doesn't matter, only identity (via Type/Subtype tag).
func makeBS(tag string) mailfauna.BodyStructure {
	return mailfauna.BodyStructure{Type: tag, Subtype: "PLAIN"}
}

func makeEnv(msgID string) mailfauna.Envelope {
	id := msgID
	return mailfauna.Envelope{MessageId: &id}
}

func TestBodyStructureCacheHitMiss(t *testing.T) {
	c := newBodyStructureCache(4)
	actor := []byte("\x01\x02actor-32-bytes-padding-padding")
	k := bodyStructureCacheKey{
		actorID:     string(actor),
		mailbox:     "INBOX",
		uidValidity: 100,
		uid:         5,
	}
	if _, _, ok := c.Get(k); ok {
		t.Fatalf("Get on empty cache: want miss, got hit")
	}
	c.Put(k, makeBS("TEXT"), makeEnv("<a@x>"))
	bs, env, ok := c.Get(k)
	if !ok {
		t.Fatalf("Get after Put: want hit, got miss")
	}
	if bs.Type != "TEXT" {
		t.Errorf("BS type: got %q", bs.Type)
	}
	if env.MessageId == nil || *env.MessageId != "<a@x>" {
		t.Errorf("Envelope MessageId: %+v", env.MessageId)
	}
}

func TestBodyStructureCacheKeyIncludesUIDValidity(t *testing.T) {
	c := newBodyStructureCache(4)
	actor := []byte("aa")
	k1 := bodyStructureCacheKey{actorID: string(actor), mailbox: "INBOX", uidValidity: 100, uid: 5}
	k2 := bodyStructureCacheKey{actorID: string(actor), mailbox: "INBOX", uidValidity: 200, uid: 5}
	c.Put(k1, makeBS("OLD"), makeEnv("<1>"))
	if _, _, ok := c.Get(k2); ok {
		t.Fatalf("entry under uid_validity=100 must not satisfy a Get under uid_validity=200")
	}
	c.Put(k2, makeBS("NEW"), makeEnv("<2>"))
	bs, _, ok := c.Get(k2)
	if !ok || bs.Type != "NEW" {
		t.Errorf("k2 lookup: ok=%v bs.Type=%q", ok, bs.Type)
	}
	// k1 must still resolve to the OLD entry.
	bs, _, ok = c.Get(k1)
	if !ok || bs.Type != "OLD" {
		t.Errorf("k1 lookup post-k2 insert: ok=%v bs.Type=%q", ok, bs.Type)
	}
}

func TestBodyStructureCacheLRUEvictionOnOverflow(t *testing.T) {
	c := newBodyStructureCache(2)
	k1 := bodyStructureCacheKey{actorID: "a", mailbox: "INBOX", uidValidity: 1, uid: 1}
	k2 := bodyStructureCacheKey{actorID: "a", mailbox: "INBOX", uidValidity: 1, uid: 2}
	k3 := bodyStructureCacheKey{actorID: "a", mailbox: "INBOX", uidValidity: 1, uid: 3}
	c.Put(k1, makeBS("k1"), makeEnv("<1>"))
	c.Put(k2, makeBS("k2"), makeEnv("<2>"))
	c.Put(k3, makeBS("k3"), makeEnv("<3>")) // evicts k1
	if _, _, ok := c.Get(k1); ok {
		t.Fatalf("k1 must be evicted (cap=2, inserted k1->k2->k3)")
	}
	if _, _, ok := c.Get(k2); !ok {
		t.Errorf("k2 must remain")
	}
	if _, _, ok := c.Get(k3); !ok {
		t.Errorf("k3 must remain")
	}
}

func TestBodyStructureCacheTouchPromotesRecency(t *testing.T) {
	// Touching k1 via Get must promote it; subsequent Put of k3 must
	// then evict k2 (the LRU after the touch), not k1.
	c := newBodyStructureCache(2)
	k1 := bodyStructureCacheKey{actorID: "a", mailbox: "INBOX", uidValidity: 1, uid: 1}
	k2 := bodyStructureCacheKey{actorID: "a", mailbox: "INBOX", uidValidity: 1, uid: 2}
	k3 := bodyStructureCacheKey{actorID: "a", mailbox: "INBOX", uidValidity: 1, uid: 3}
	c.Put(k1, makeBS("k1"), makeEnv("<1>"))
	c.Put(k2, makeBS("k2"), makeEnv("<2>"))
	if _, _, ok := c.Get(k1); !ok {
		t.Fatalf("k1 hit before eviction")
	}
	c.Put(k3, makeBS("k3"), makeEnv("<3>"))
	if _, _, ok := c.Get(k1); !ok {
		t.Errorf("k1 must be retained after Get-touch + k3 insert")
	}
	if _, _, ok := c.Get(k2); ok {
		t.Errorf("k2 must be evicted (LRU after touching k1)")
	}
}

func TestBodyStructureCacheRespectsMaxZero(t *testing.T) {
	// max=0 disables caching: every Get returns miss; Put no-ops.
	c := newBodyStructureCache(0)
	k := bodyStructureCacheKey{actorID: "a", mailbox: "INBOX", uidValidity: 1, uid: 1}
	c.Put(k, makeBS("never"), makeEnv("<1>"))
	if _, _, ok := c.Get(k); ok {
		t.Fatalf("max=0 must disable the cache")
	}
}

func TestBodyStructureCacheResizeShrinkEvictsLRUTail(t *testing.T) {
	// Cap 4, fill with k1..k4 (k1 = LRU), then Resize down to 2: the two
	// LRU-tail entries (k1, k2) must be evicted immediately, k3/k4 retained.
	c := newBodyStructureCache(4)
	ks := make([]bodyStructureCacheKey, 4)
	for i := range ks {
		ks[i] = bodyStructureCacheKey{actorID: "a", mailbox: "INBOX", uidValidity: 1, uid: uint32(i + 1)}
		c.Put(ks[i], makeBS("k"), makeEnv("<x>"))
	}
	c.Resize(2)
	if _, _, ok := c.Get(ks[0]); ok {
		t.Errorf("k1 (LRU) must be evicted by Resize(2)")
	}
	if _, _, ok := c.Get(ks[1]); ok {
		t.Errorf("k2 must be evicted by Resize(2)")
	}
	if _, _, ok := c.Get(ks[2]); !ok {
		t.Errorf("k3 must be retained after Resize(2)")
	}
	if _, _, ok := c.Get(ks[3]); !ok {
		t.Errorf("k4 (MRU) must be retained after Resize(2)")
	}
}

func TestBodyStructureCacheResizeGrowAdmitsMore(t *testing.T) {
	// Cap 2 then Resize up to 4: subsequent inserts beyond the old cap are
	// admitted without evicting the earlier survivors.
	c := newBodyStructureCache(2)
	k1 := bodyStructureCacheKey{actorID: "a", mailbox: "INBOX", uidValidity: 1, uid: 1}
	k2 := bodyStructureCacheKey{actorID: "a", mailbox: "INBOX", uidValidity: 1, uid: 2}
	c.Put(k1, makeBS("k1"), makeEnv("<1>"))
	c.Put(k2, makeBS("k2"), makeEnv("<2>"))
	c.Resize(4)
	k3 := bodyStructureCacheKey{actorID: "a", mailbox: "INBOX", uidValidity: 1, uid: 3}
	k4 := bodyStructureCacheKey{actorID: "a", mailbox: "INBOX", uidValidity: 1, uid: 4}
	c.Put(k3, makeBS("k3"), makeEnv("<3>"))
	c.Put(k4, makeBS("k4"), makeEnv("<4>"))
	for _, k := range []bodyStructureCacheKey{k1, k2, k3, k4} {
		if _, _, ok := c.Get(k); !ok {
			t.Errorf("all four entries must fit after Resize(4); missing uid=%d", k.uid)
		}
	}
}

func TestBodyStructureCacheResizeToZeroClearsAndDisables(t *testing.T) {
	c := newBodyStructureCache(4)
	k := bodyStructureCacheKey{actorID: "a", mailbox: "INBOX", uidValidity: 1, uid: 1}
	c.Put(k, makeBS("k"), makeEnv("<1>"))
	c.Resize(0)
	if _, _, ok := c.Get(k); ok {
		t.Errorf("Resize(0) must clear the existing entry")
	}
	c.Put(k, makeBS("k"), makeEnv("<1>"))
	if _, _, ok := c.Get(k); ok {
		t.Errorf("Resize(0) must disable the cache (Put no-ops)")
	}
}

func TestBodyStructureCachePutOverwritesEntry(t *testing.T) {
	c := newBodyStructureCache(4)
	k := bodyStructureCacheKey{actorID: "a", mailbox: "INBOX", uidValidity: 1, uid: 5}
	c.Put(k, makeBS("first"), makeEnv("<a>"))
	c.Put(k, makeBS("second"), makeEnv("<b>"))
	bs, env, ok := c.Get(k)
	if !ok {
		t.Fatalf("Get after overwrite: miss")
	}
	if bs.Type != "second" {
		t.Errorf("BS type after overwrite: got %q want second", bs.Type)
	}
	if env.MessageId == nil || *env.MessageId != "<b>" {
		t.Errorf("Envelope after overwrite: %+v", env.MessageId)
	}
}
