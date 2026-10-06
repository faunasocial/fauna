package atprotolex

import (
	"strings"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotorepo"
)

// recordValue puts a record through the PRODUCTION encode path — the same
// JSONRecordToDagCBOR the write path and the projection loop both call — and
// hands back the decoded data-model value. Tests validate what we would
// COMMIT, never a hand-built map, which is finding 48/49's rule: a fixture
// each side builds for itself can agree with nothing real.
func recordValue(t *testing.T, recordJSON string) any {
	t.Helper()
	cbor, err := atprotorepo.JSONRecordToDagCBOR(recordJSON)
	if err != nil {
		t.Fatalf("encode fixture: %v", err)
	}
	v, err := atprotorepo.DagCBORRecordToJSONValue(cbor)
	if err != nil {
		t.Fatalf("decode fixture: %v", err)
	}
	return v
}

func testCatalog(t *testing.T) *Catalog {
	t.Helper()
	c, err := New()
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	return c
}

// The load must actually find the vendored tree. Asserting a floor rather than
// an exact count keeps a catalog refresh from being a test edit, while still
// failing loudly if the embed ever resolves to nothing.
//
// The floor is far below the count at the pinned commit (25 record schemas out
// of 396 lexicon files — most of the catalog is queries, procedures and
// subscriptions, which hold no records) so that upstream retiring a record type
// is not a red build, while an embed that resolved to an empty tree still is.
func TestCatalogLoadsTheVendoredRecordSchemas(t *testing.T) {
	c := testCatalog(t)
	if c.Len() < 15 {
		t.Fatalf("catalog holds %d record schemas, expected the vendored tree's ~25", c.Len())
	}
}

// The two collections D2 round-trips and a representative journaled one must
// be validatable, or the "where the lexicon is known" half of the table is
// empty in exactly the cases that matter.
func TestKnownRecordCoversTheCollectionsD2Names(t *testing.T) {
	c := testCatalog(t)
	for _, nsid := range []string{
		"app.bsky.feed.post",
		"app.bsky.actor.profile",
		"app.bsky.feed.like",
		"app.bsky.feed.repost",
		"app.bsky.graph.follow",
		"app.bsky.graph.block",
		"app.bsky.graph.list",
		"app.bsky.feed.threadgate",
	} {
		if !c.KnownRecord(nsid) {
			t.Errorf("KnownRecord(%q) = false, want true", nsid)
		}
	}
}

// A query NSID is in the catalog as a schema but is NOT record-shaped. If
// KnownRecord answered true for it, a write to that collection would be
// refused as an invalid record instead of journaled as an unknown one — the
// wrong disposition, reached by the wrong reasoning.
func TestKnownRecordIsFalseForQueriesAndStrangers(t *testing.T) {
	c := testCatalog(t)
	for _, nsid := range []string{
		"app.bsky.feed.getTimeline",
		"com.atproto.repo.createRecord",
		"com.example.someone.elses.lexicon",
	} {
		if c.KnownRecord(nsid) {
			t.Errorf("KnownRecord(%q) = true, want false", nsid)
		}
	}
}

func TestValidateAcceptsAWellFormedPost(t *testing.T) {
	c := testCatalog(t)
	v := recordValue(t, `{"$type":"app.bsky.feed.post","text":"hello","createdAt":"2026-08-02T12:00:00Z"}`)
	if err := c.Validate("app.bsky.feed.post", v); err != nil {
		t.Fatalf("Validate: %v", err)
	}
}

// The whole point of the slice: a record that does not satisfy its schema is
// caught. Without this the catalog could load, answer "valid" to everything,
// and no test would notice.
func TestValidateRefusesAPostMissingARequiredField(t *testing.T) {
	c := testCatalog(t)
	v := recordValue(t, `{"$type":"app.bsky.feed.post","text":"no createdAt here"}`)
	if err := c.Validate("app.bsky.feed.post", v); err == nil {
		t.Fatal("Validate accepted a post with no createdAt")
	}
}

func TestValidateRefusesAWronglyTypedField(t *testing.T) {
	c := testCatalog(t)
	v := recordValue(t, `{"$type":"app.bsky.feed.post","text":42,"createdAt":"2026-08-02T12:00:00Z"}`)
	if err := c.Validate("app.bsky.feed.post", v); err == nil {
		t.Fatal("Validate accepted a post whose text is a number")
	}
}

// AllowLenientDatetime is set deliberately (see validateFlags). This pins the
// ruling: a datetime the network accepts must not be refused here, or Fauna
// diverges from every other PDS on records it had no reason to reject.
func TestValidateAcceptsALenientDatetime(t *testing.T) {
	c := testCatalog(t)
	v := recordValue(t, `{"$type":"app.bsky.feed.post","text":"hi","createdAt":"2026-08-02T12:00:00.0000Z"}`)
	if err := c.Validate("app.bsky.feed.post", v); err != nil {
		t.Fatalf("Validate refused a lenient-but-real datetime: %v", err)
	}
}

// StrictRecursiveValidation is deliberately NOT set: atproto unions are open,
// so a record carrying an embed this catalog never heard of is legitimate and
// must not be refused. Refusing it would re-impose the schema gate on unknown
// NSIDs that D2's table rules out.
func TestValidateAcceptsAnOpenUnionMemberItDoesNotKnow(t *testing.T) {
	c := testCatalog(t)
	v := recordValue(t, `{"$type":"app.bsky.feed.post","text":"hi",`+
		`"createdAt":"2026-08-02T12:00:00Z",`+
		`"embed":{"$type":"com.example.future.embed","whatever":"yes"}}`)
	if err := c.Validate("app.bsky.feed.post", v); err != nil {
		t.Fatalf("Validate refused an unknown open-union embed: %v", err)
	}
}

// Validate is mechanism, and asking it about an NSID it holds no record schema
// for is the caller's bug. It must say so rather than pass the ref to indigo
// and surface whatever that produces as if it were a verdict about the record.
func TestValidateRefusesAnUnknownNSIDAsACallerBug(t *testing.T) {
	c := testCatalog(t)
	v := recordValue(t, `{"$type":"com.example.thing","a":"b"}`)
	err := c.Validate("com.example.thing", v)
	if err == nil {
		t.Fatal("Validate accepted an NSID with no record schema")
	}
	if !strings.Contains(err.Error(), "no record schema") {
		t.Fatalf("want the programming-error message, got %v", err)
	}
}
