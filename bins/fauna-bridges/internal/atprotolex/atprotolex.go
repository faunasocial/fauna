// Package atprotolex validates atproto records against the ATProto Lexicon
// catalog vendored at bins/fauna-bridges/internal/atprotolex/lexicons.
//
// It is MECHANISM only — the same split as internal/atprotopds/authz.go and
// internal/safefetch: this package answers "is this NSID one we hold a record
// schema for" and "does this record satisfy it", and holds no opinion about
// what a caller's `validate` flag means or which XRPC error a failure becomes.
// That policy lives with the write path (internal/atprotopds/repo_write.go),
// which is the one place that can weigh it against D2's membership rules.
//
// Why a vendored catalog and not runtime resolution
// (docs/goal/behavior/atproto-pds-full.md § F2 detail, *Lexicon-schema
// validation*): D2's own table already rules that unknown/third-party NSIDs get
// NO schema gate — "real PDSs accept arbitrary structurally-valid records; we
// do the same" — so resolving a stranger's Lexicon at write time would buy the
// write path nothing while adding an attacker-directed DNS+HTTP plane and
// making a record's validity change over time. What the catalog closes is the
// other half of the same table: records whose lexicon IS known are
// "lexicon-validated", and only those may answer `validationStatus: "valid"`.
//
// The tree is embedded rather than read from disk because this runs in
// production inside the shipped bridge binary; a dedicated dev-fleet sync
// tool refreshes it from the pinned commit in LEXICONS_COMMIT.
package atprotolex

import (
	"embed"
	"encoding/json"
	"fmt"
	"io/fs"
	"strings"
	"sync"

	"github.com/bluesky-social/indigo/atproto/lexicon"
)

// all: so a schema whose path component begins with `_` or `.` is still
// embedded — the vendored tree is a verbatim upstream copy and gets no say in
// which of its files we honour.
//
//go:embed all:lexicons
var lexiconFS embed.FS

// validateFlags is the strictness dial, and each bit is a deliberate ruling
// about what this PDS refuses.
//
//   - AllowLenientDatetime is SET. Real records across the network carry
//     datetimes that are valid RFC3339 but not in the normalized form the
//     stricter reading wants, and the ecosystem's own tooling accepts them.
//     Refusing those would refuse writes every other PDS takes — a divergence
//     the caller experiences as "Fauna is broken", not as rigor.
//   - AllowLegacyBlob is NOT set. The legacy pre-`blob` format predates the
//     ref shape our blob walk (ExtractBlobRefs) knows how to read, so a legacy
//     blob would commit a reference nothing walks — exactly the dangling ref
//     *store-then-reference* forbids (§ F2 detail, uploadBlob). Our own
//     uploadBlob only ever answers the modern ref, so nothing we serve needs
//     it.
//   - LenientMode and StrictRecursiveValidation are NOT set. The first would
//     gut the check; the second recurses into nested union members, and
//     atproto unions are open by design — a record carrying an embed type this
//     catalog has never heard of is legitimate, and refusing it would
//     re-impose the schema gate on unknown NSIDs that D2's table rules out.
const validateFlags = lexicon.AllowLenientDatetime

// Shared is the process-wide catalog. The vendored tree is compiled in and
// immutable, so there is nothing per-server about it and no seam to inject:
// one parse, reused by every request.
//
// It returns the error rather than panicking so a broken embed surfaces as a
// failed request instead of a bridge that will not boot — but the load test in
// this package is what is actually expected to catch that, long before a
// deployment does.
func Shared() (*Catalog, error) { return sharedCatalog() }

var sharedCatalog = sync.OnceValues(New)

// Catalog is a loaded, immutable view of the vendored schemas. Safe for
// concurrent use: nothing mutates it after New returns.
type Catalog struct {
	base *lexicon.BaseCatalog
	// records holds exactly the NSIDs that resolve to a schema this catalog can
	// validate a RECORD against. It is not "every NSID in the tree": most of
	// the catalog is queries and procedures, and a repo can hold no records of
	// `app.bsky.feed.getTimeline`.
	records map[string]struct{}
}

// New loads every vendored schema. It fails rather than degrading: a catalog
// that silently dropped a schema would answer "unknown" for a lexicon we do
// hold, quietly downgrading records that should have been validated.
func New() (*Catalog, error) {
	base := lexicon.NewBaseCatalog()
	c := &Catalog{base: base, records: make(map[string]struct{})}

	var ids []string
	err := fs.WalkDir(lexiconFS, "lexicons", func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.IsDir() || !strings.HasSuffix(path, ".json") {
			return nil
		}
		b, err := lexiconFS.ReadFile(path)
		if err != nil {
			return fmt.Errorf("read %s: %w", path, err)
		}
		var sf lexicon.SchemaFile
		if err := json.Unmarshal(b, &sf); err != nil {
			return fmt.Errorf("parse %s: %w", path, err)
		}
		if err := base.AddSchemaFile(sf); err != nil {
			return fmt.Errorf("load %s: %w", path, err)
		}
		ids = append(ids, sf.ID)
		return nil
	})
	if err != nil {
		return nil, err
	}
	if len(ids) == 0 {
		return nil, fmt.Errorf("atprotolex: the vendored catalog is empty")
	}

	// Second pass: a schema is record-shaped iff its `main` def is a record.
	// Resolving through the catalog rather than re-reading the file keeps one
	// owner for what "the main def" means.
	for _, id := range ids {
		schema, err := base.Resolve(id)
		if err != nil {
			continue
		}
		if _, ok := schema.Def.(lexicon.SchemaRecord); ok {
			c.records[id] = struct{}{}
		}
	}
	return c, nil
}

// KnownRecord reports whether this catalog holds a record schema for the
// collection NSID — the "where the lexicon is known" half of D2's table.
func (c *Catalog) KnownRecord(nsid string) bool {
	_, ok := c.records[nsid]
	return ok
}

// Len is the number of record schemas held, for the load test to assert
// against rather than trusting that WalkDir found anything.
func (c *Catalog) Len() int { return len(c.records) }

// Validate checks record — the atproto data-model value, as produced by
// decoding the dag-cbor this PDS encoded from the caller's JSON — against the
// collection's record schema.
//
// Validating the ENCODED form rather than the caller's raw JSON is deliberate:
// encoding coerces numbers and turns `{"/": …}` maps into CID links, so the
// raw JSON and the record are not the same value, and the record is the one
// every downstream consumer reads.
//
// ⚠ It is the CALLER'S record, not necessarily the COMMITTED one. For a
// round-tripped collection the nest answers a `record_override` and the repo
// commits the projection's rendering instead (§ F2 detail). That is the right
// input here — `validationStatus` is the lexicon's statement about what the
// caller submitted — but it means this check is NOT a guarantee that what we
// commit validates. That guarantee cannot live on the request path at all: a
// refusal there would arrive after the Fauna post exists, with nowhere to go.
// It lives in tests over the projection's own renderings instead —
// cmd/fauna-atproto-bridge/projection_lexicon_ffi_test.go, which is finding
// 48/49's split. NOT in atprotorepo's own tests, where an earlier draft of this
// comment pointed: that package is deliberately CGO-free and fakes the
// translator, so the only renderings it could validate are the fake's.
//
// Calling Validate for an NSID KnownRecord rejects is a programming error, not
// a caller error; it returns an error saying so rather than guessing.
func (c *Catalog) Validate(nsid string, record any) error {
	if !c.KnownRecord(nsid) {
		return fmt.Errorf("atprotolex: no record schema for %q", nsid)
	}
	return lexicon.ValidateRecord(c.base, record, nsid, validateFlags)
}
