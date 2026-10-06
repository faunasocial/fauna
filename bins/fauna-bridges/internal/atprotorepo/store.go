// Package atprotorepo is the bridge-local, per-user ATProto repo store and the
// C1 commit funnel (docs/goal/behavior/atproto-pds-full.md § Constraints C1;
// docs/goal/behavior/atproto-pds-bridge.md § Where logic lives). Every user the
// nest projects gets one MST repo here, mutated only through Funnel.ApplyBatch:
// a per-user serialized apply -> sign -> persist -> emit funnel that the
// projection loop (and, later, F2's authed write path) is the sole caller of.
//
// Everything the store persists is re-derivable from nest state (deterministic
// rkeys, C6): blocks are content-addressed, the MST is a pure function of the
// current record set, and the firehose event log is a bounded replay window.
// Only the DID signing key is precious, and it never lives here — it is held
// sealed by the bridge and passed in per commit. A lost or corrupt store is
// rebuilt by re-projecting; it is a cache, not a source of truth.
package atprotorepo

import (
	"bytes"
	"context"
	"database/sql"
	"fmt"
	"io"
	"strings"

	"github.com/bluesky-social/indigo/atproto/repo"
	"github.com/bluesky-social/indigo/atproto/repo/mst"
	"github.com/ipfs/go-cid"
	_ "modernc.org/sqlite" // pure-Go sqlite driver, registered as "sqlite"
)

// schema is applied idempotently at Open. Table roles:
//   - repo_heads:       the current signed head (rev + commit CID) per DID.
//   - repo_blocks:      the content-addressed block cache, EXACT-CID keyed
//     (full codec-bearing cid.KeyString bytes); serves getRepo/getRecord and
//     the inductive #commit diff. Append-only — orphaned MST nodes are
//     harmless and reclaimed by a future rebuild(sources) pass (C3/S5).
//   - records:          the authoritative current record set (path -> cid +
//     bytes) the MST is rebuilt from; also backs repo.getRecord/listRecords.
//   - post_map:         Fauna post id -> AT-URI + record CID, for delete and
//     reply/quote resolution (C9-safe: external rkeys are never assumed).
//   - firehose_events:  the outbox — one row per emitted frame, PDS-global
//     monotonic seq, committed in the SAME txn as the repo head so a crash
//     never leaves a head without its frame or vice versa.
//   - projection_state: the projection loop's per-DID watermark + first-emit
//     gate (written by the loop, not the funnel).
const schema = `
CREATE TABLE IF NOT EXISTS repo_heads (
    did         TEXT PRIMARY KEY,
    rev         TEXT NOT NULL,
    commit_cid  TEXT NOT NULL,
    updated_at  INTEGER NOT NULL,
    active      INTEGER NOT NULL DEFAULT 1,
    sync_owed   INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS repo_blocks (
    did            TEXT NOT NULL,
    cid_keystring  BLOB NOT NULL,
    block          BLOB NOT NULL,
    PRIMARY KEY (did, cid_keystring)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS records (
    did          TEXT NOT NULL,
    path         TEXT NOT NULL,
    record_cid   TEXT NOT NULL,
    record_bytes BLOB NOT NULL,
    PRIMARY KEY (did, path)
);
CREATE TABLE IF NOT EXISTS post_map (
    did           TEXT NOT NULL,
    fauna_post_id TEXT NOT NULL,
    at_uri        TEXT NOT NULL,
    record_cid    TEXT NOT NULL,
    root_uri      TEXT NOT NULL DEFAULT '',
    root_cid      TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (did, fauna_post_id)
);
CREATE TABLE IF NOT EXISTS firehose_events (
    seq        INTEGER PRIMARY KEY,
    did        TEXT NOT NULL,
    frame_type TEXT NOT NULL,
    payload    BLOB NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS projection_state (
    did                    TEXT PRIMARY KEY,
    last_created_at_micros INTEGER NOT NULL DEFAULT 0,
    last_post_id           TEXT NOT NULL DEFAULT '',
    first_emit_gated       INTEGER NOT NULL DEFAULT 1,
    published_handle       TEXT NOT NULL DEFAULT ''
);
`

// additiveColumns are ADD COLUMN migrations for a store created by an earlier
// build: each is applied on open and its "duplicate column name" error ignored,
// so the statement list is the whole migration mechanism — additive-only by
// construction, which is all this store ever needs (its content is derived and
// re-derivable, C6; only the schema has to survive an upgrade in place). A new
// column is declared in the CREATE block above AND appended here. Empty since
// the compat-remnant sweep folded every earlier addition into the block
// (docs/goal/architecture/version-compatibility.md § Dimension 2).
//
// Column notes the block cannot carry: repo_heads.active is the layer-2
// served-status flag (S4-D); repo_heads.sync_owed is "this repo's head is ahead
// of what the firehose has announced" (S5 slice 4, the #sync collapse) — set
// inside the first deferred commit's txn, cleared inside the #sync's, so a
// crash in between leaves it set and the announcement cannot be lost;
// post_map.root_uri/root_cid are the thread root a projected reply anchors to
// (S5 slice 3), empty meaning "this post is itself a thread root" — storing it
// makes the ATProto root rule inductive (root = parent's root, or the parent
// itself), one O(1) lookup instead of walking the chain.
var additiveColumns = []string{}

// postMapByFaunaID indexes the reply/quote lookup, which is NOT did-scoped (the
// PK's leading `did` column cannot serve it): a reply to another bridged actor
// must resolve into that actor's repo. Created after the additive columns so a
// pre-existing store gains it on open.
const postMapFaunaIDIndex = `CREATE INDEX IF NOT EXISTS post_map_fauna_post_id ON post_map(fauna_post_id)`

// postMapByATURI indexes the REVERSE lookup (Store.FaunaPostIDForRecord): an
// external `deleteRecord` names a record by rkey, and the write path needs the
// Fauna post id behind it. The PK's leading `did` narrows to one repo but not
// to one row, so without this a delete scans every record that repo has ever
// projected.
const postMapATURIIndex = `CREATE INDEX IF NOT EXISTS post_map_did_at_uri ON post_map(did, at_uri)`

// Store is the sqlite-backed persistence layer for the bridge's repos.
type Store struct {
	db *sql.DB
}

// querier is the read/write surface shared by *sql.DB and *sql.Tx, so the same
// helpers run either standalone or inside the funnel's commit transaction.
type querier interface {
	ExecContext(ctx context.Context, query string, args ...any) (sql.Result, error)
	QueryContext(ctx context.Context, query string, args ...any) (*sql.Rows, error)
	QueryRowContext(ctx context.Context, query string, args ...any) *sql.Row
}

// Open opens (creating if absent) the sqlite DB at path and applies the schema.
// Pass ":memory:" for tests. WAL keeps the read surface unblocked while the
// funnel commits; busy_timeout absorbs the brief single-writer contention.
func Open(path string) (*Store, error) {
	dsn := fmt.Sprintf("file:%s?_pragma=journal_mode(WAL)&_pragma=busy_timeout(5000)&_pragma=foreign_keys(on)", path)
	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		return nil, fmt.Errorf("open sqlite %q: %w", path, err)
	}
	// A single connection avoids WAL/:memory: surprises and makes the
	// PDS-global seq allocation trivially race-free; the bridge's repo write
	// path is per-DID serial anyway.
	db.SetMaxOpenConns(1)
	for _, ddl := range []string{schema, blobsSchema} {
		if _, err := db.ExecContext(context.Background(), ddl); err != nil {
			db.Close()
			return nil, fmt.Errorf("apply schema: %w", err)
		}
	}
	for _, stmt := range additiveColumns {
		if _, err := db.ExecContext(context.Background(), stmt); err != nil &&
			!strings.Contains(err.Error(), "duplicate column name") {
			db.Close()
			return nil, fmt.Errorf("apply additive migration %q: %w", stmt, err)
		}
	}
	for _, stmt := range []string{postMapFaunaIDIndex, postMapATURIIndex} {
		if _, err := db.ExecContext(context.Background(), stmt); err != nil {
			db.Close()
			return nil, fmt.Errorf("apply post_map index %q: %w", stmt, err)
		}
	}
	return &Store{db: db}, nil
}

// DB exposes the underlying handle for the read surface (task 7) and tests.
func (s *Store) DB() *sql.DB { return s.db }

// Close closes the database.
func (s *Store) Close() error { return s.db.Close() }

// recordRow is one live record: its repo path ("collection/rkey"), the CID of
// its dag-cbor bytes, and the bytes themselves.
type recordRow struct {
	path       string
	recordCID  string
	recordCBOR []byte
}

// headRow is a repo's current signed head.
type headRow struct {
	rev       string
	commitCID string
}

// loadRecords returns every live record for did, so the MST can be rebuilt from
// the full set (the MST is a pure function of it — order-independent).
func loadRecords(ctx context.Context, q querier, did string) ([]recordRow, error) {
	rows, err := q.QueryContext(ctx,
		`SELECT path, record_cid, record_bytes FROM records WHERE did = ? ORDER BY path`, did)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []recordRow
	for rows.Next() {
		var r recordRow
		if err := rows.Scan(&r.path, &r.recordCID, &r.recordCBOR); err != nil {
			return nil, err
		}
		out = append(out, r)
	}
	return out, rows.Err()
}

// loadHead returns the current head for did; ok is false for a repo with no
// commit yet (the next ApplyBatch is its genesis).
func loadHead(ctx context.Context, q querier, did string) (headRow, bool, error) {
	var h headRow
	err := q.QueryRowContext(ctx,
		`SELECT rev, commit_cid FROM repo_heads WHERE did = ?`, did).Scan(&h.rev, &h.commitCID)
	if err == sql.ErrNoRows {
		return headRow{}, false, nil
	}
	if err != nil {
		return headRow{}, false, err
	}
	return h, true, nil
}

// blockKeyStrings returns the exact cid.KeyString of every block already
// persisted for did, so the funnel can diff the post-commit set down to just
// the new blocks the inductive #commit carries.
func blockKeyStrings(ctx context.Context, q querier, did string) (map[string]struct{}, error) {
	rows, err := q.QueryContext(ctx, `SELECT cid_keystring FROM repo_blocks WHERE did = ?`, did)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := make(map[string]struct{})
	for rows.Next() {
		var k []byte
		if err := rows.Scan(&k); err != nil {
			return nil, err
		}
		out[string(k)] = struct{}{}
	}
	return out, rows.Err()
}

// maxSeq returns the highest firehose seq ever allocated (0 if none), used to
// seed the funnel's in-memory PDS-global counter at startup.
func maxSeq(ctx context.Context, q querier) (int64, error) {
	var seq sql.NullInt64
	if err := q.QueryRowContext(ctx, `SELECT MAX(seq) FROM firehose_events`).Scan(&seq); err != nil {
		return 0, err
	}
	if !seq.Valid {
		return 0, nil
	}
	return seq.Int64, nil
}

// GetBlock returns the persisted block bytes for the exact cid keystring, or
// (nil, false) if absent. Used by the read surface (getRepo graph walk).
func (s *Store) GetBlock(ctx context.Context, did string, cidKeyString []byte) ([]byte, bool, error) {
	var b []byte
	err := s.db.QueryRowContext(ctx,
		`SELECT block FROM repo_blocks WHERE did = ? AND cid_keystring = ?`, did, cidKeyString).Scan(&b)
	if err == sql.ErrNoRows {
		return nil, false, nil
	}
	if err != nil {
		return nil, false, err
	}
	return b, true, nil
}

// Head returns the current head for did (rev, commit CID) or ok=false.
func (s *Store) Head(ctx context.Context, did string) (rev, commitCID string, ok bool, err error) {
	h, found, err := loadHead(ctx, s.db, did)
	if err != nil || !found {
		return "", "", false, err
	}
	return h.rev, h.commitCID, true, nil
}

// RepoActive reports did's layer-2 served status (S4-D): active=true means the
// read surface serves it and the firehose has announced it active; active=false
// means it is deactivated (repo retained, no longer served). exists=false when
// no repo head exists yet — the reconcile treats that as "nothing to unserve or
// announce" (atproto-pds-bridge.md § Disable & revocation, layer 2).
func (s *Store) RepoActive(ctx context.Context, did string) (active, exists bool, err error) {
	var flag int
	err = s.db.QueryRowContext(ctx, `SELECT active FROM repo_heads WHERE did = ?`, did).Scan(&flag)
	if err == sql.ErrNoRows {
		return false, false, nil
	}
	if err != nil {
		return false, false, err
	}
	return flag != 0, true, nil
}

// SyncOwed reports whether did's repo head is ahead of what the firehose has
// announced — i.e. a catch-up applied commits with their #commit frames deferred
// and the single #sync that stands in for them has not been emitted yet (S5
// slice 4, atproto-pds-bridge.md § Projection & backfill, watermark row 3).
//
// A repo with no head owes nothing: there is no head to announce.
func (s *Store) SyncOwed(ctx context.Context, did string) (bool, error) {
	var flag int
	err := s.db.QueryRowContext(ctx, `SELECT sync_owed FROM repo_heads WHERE did = ?`, did).Scan(&flag)
	if err == sql.ErrNoRows {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	return flag != 0, nil
}

// SetRepoActive flips did's served-status flag. Only ever called for a DID that
// already has a head (the reconcile guards on RepoActive.exists), so a missing
// row is a caller bug rather than a no-op to swallow.
func (s *Store) SetRepoActive(ctx context.Context, did string, active bool) error {
	flag := 0
	if active {
		flag = 1
	}
	res, err := s.db.ExecContext(ctx,
		`UPDATE repo_heads SET active = ? WHERE did = ?`, flag, did)
	if err != nil {
		return err
	}
	if n, _ := res.RowsAffected(); n == 0 {
		return fmt.Errorf("SetRepoActive: no repo head for did %s", did)
	}
	return nil
}

// PurgeRepo erases every trace of did's repo from this store in one transaction:
// records, blocks, the PostId↔AT-URI map, the blobs, the projection watermark
// and finally the head row itself. It is the last step of the delete-presence
// sweep (atproto-pds-bridge.md § Disable & revocation layer 2) and is a no-op on
// a DID with no repo, so the sweep converges when retried after a crash.
//
// Purging is not data loss: every table here is DERIVED and re-derivable from
// Fauna state (§ Projection & backfill — "the entire repo/carstore is
// re-derivable … only the DID key material … is precious"), and this call
// deliberately touches NEITHER the identity/key material (nest-side, retained so
// the delete stays "reversible in identity terms") NOR firehose_events, whose
// rows are the network's record that the deletion happened and which the
// ordinary retention window prunes on its own schedule.
//
// Dropping projection_state with the rest is load-bearing rather than tidiness:
// a surviving watermark would make a later re-enable project only posts NEWER
// than the sweep, leaving the rebuilt repo permanently missing history that sits
// inside the user's own consent floor — silently withholding posts they did
// consent to publish, and contradicting re-derivability. Gone, the DID simply
// reads as never-projected and rebuilds from its floor.
func (s *Store) PurgeRepo(ctx context.Context, did string) error {
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("begin purge txn: %w", err)
	}
	defer tx.Rollback()
	for _, stmt := range []string{
		`DELETE FROM records WHERE did = ?`,
		`DELETE FROM repo_blocks WHERE did = ?`,
		`DELETE FROM post_map WHERE did = ?`,
		`DELETE FROM blobs WHERE did = ?`,
		`DELETE FROM projection_state WHERE did = ?`,
		`DELETE FROM repo_heads WHERE did = ?`,
	} {
		if _, err := tx.ExecContext(ctx, stmt, did); err != nil {
			return fmt.Errorf("purge repo (%s): %w", stmt, err)
		}
	}
	if err := tx.Commit(); err != nil {
		return fmt.Errorf("commit purge txn: %w", err)
	}
	return nil
}

// RepoStatus returns did's head rev plus its served-status flag, or exists=false
// when there is no repo yet — the com.atproto.sync.getRepoStatus payload, and
// the served-status guard the content reads consult before serving.
func (s *Store) RepoStatus(ctx context.Context, did string) (rev string, active, exists bool, err error) {
	var flag int
	err = s.db.QueryRowContext(ctx,
		`SELECT rev, active FROM repo_heads WHERE did = ?`, did).Scan(&rev, &flag)
	if err == sql.ErrNoRows {
		return "", false, false, nil
	}
	if err != nil {
		return "", false, false, err
	}
	return rev, flag != 0, true, nil
}

// ExportRepo serializes did's CURRENT repo state as a CARv1 rooted at the head
// commit — the com.atproto.sync.getRepo payload, which the atproto sync spec
// defines as "all repo records, MST nodes, and the current signed commit
// object, all in a single CAR file" (the repository spec restates a full
// export as "the full repo structure … for the indicated commit").
//
// Superseded commits, MST nodes and record blocks stay persisted (append-only,
// C6) but are NOT payload. Nothing on the network needs them here: chain
// validation is inductive (`prevData` on #commit frames — no consumer
// dereferences the head commit's `prev` out of a getRepo response), and the
// lexicon's optional `since` diff — head-reachable blocks new since a given
// rev — contains no superseded block by construction. Serving them was
// conformant (consumers "should … ignore any unnecessary or unlinked blocks")
// but scaled the response with COMMIT count instead of repo size: measured
// ~4x blocks / ~8x bytes for append-only repos, unbounded for update-heavy
// ones (the profile singleton's exact shape).
//
// The walk is lazy — one GetBlock per reachable block — so the work is
// proportional to the live repo, never to the persisted table (the
// ExportRecordProof rule: narrowing the wire must not move the amplification
// to the disk). Node decoding is indigo's own (mst.NodeDataFromCBOR /
// NodeData.Node); ours is only the traversal and the CAR framing.
//
// This buffered form is for callers that genuinely need the bytes in hand — the
// boot self-check reloads them through indigo's loader, and tests compare block
// sets. The SERVING path streams instead ([Store.ExportRepoTo]): a payload whose
// size is the user's whole live repo must not also be materialised in memory on
// an anonymous surface.
func (s *Store) ExportRepo(ctx context.Context, did string) ([]byte, error) {
	var buf bytes.Buffer
	if err := s.ExportRepoTo(ctx, did, &buf); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

// ExportRepoTo is [Store.ExportRepo] streamed: it writes the CARv1 header, then
// each reachable block as the walk reaches it, so peak resident memory is one
// block plus the visited-CID set instead of the whole repo twice over.
//
// ⚠ The error contract is what streaming costs, and callers must handle it: any
// error AFTER the header is written leaves a TRUNCATED CAR already on w. The
// head commit is therefore loaded and decoded BEFORE anything is written, which
// is where a "this repo cannot be served at all" failure surfaces cleanly; every
// later error is repo corruption, and a serving caller must tear the response
// down rather than let a short CAR read as a complete one (see the getRepo
// handler). A buffered caller ([Store.ExportRepo]) simply discards the buffer.
func (s *Store) ExportRepoTo(ctx context.Context, did string, w io.Writer) error {
	h, ok, err := loadHead(ctx, s.db, did)
	if err != nil {
		return err
	}
	if !ok {
		return fmt.Errorf("no repo for did %s", did)
	}
	commitCID, err := cid.Decode(h.commitCID)
	if err != nil {
		return fmt.Errorf("decode head commit cid: %w", err)
	}

	load := func(c cid.Cid, kind string) ([]byte, error) {
		raw, found, err := s.GetBlock(ctx, did, []byte(c.KeyString()))
		if err != nil {
			return nil, err
		}
		if !found {
			return nil, fmt.Errorf("%s block %s missing from repo %s", kind, c, did)
		}
		return raw, nil
	}

	// The head commit is read and decoded before a single byte is written: it
	// supplies the CAR root and the MST entry point, so a repo that cannot be
	// served at all fails here, with nothing on the wire to truncate.
	commitRaw, err := load(commitCID, "head commit")
	if err != nil {
		return err
	}
	var commit repo.Commit
	if err := commit.UnmarshalCBOR(bytes.NewReader(commitRaw)); err != nil {
		return fmt.Errorf("decode head commit: %w", err)
	}

	cw, err := newCARWriter(w, commitCID)
	if err != nil {
		return err
	}
	if err := cw.writeBlock(commitCID, commitRaw); err != nil { // commit root first, per the repo spec
		return err
	}

	emit := func(c cid.Cid, kind string) ([]byte, error) {
		raw, err := load(c, kind)
		if err != nil {
			return nil, err
		}
		if err := cw.writeBlock(c, raw); err != nil {
			return nil, err
		}
		return raw, nil
	}

	// Depth-first over the MST from the commit's data root, emitting every node
	// and record block reachable from the head. The visited set makes a corrupt
	// (cyclic or block-sharing) graph terminate; the depth bound keeps one
	// anonymous request's walk bounded, exactly as in ExportRecordProof.
	visited := map[string]bool{}
	var walk func(nodeCID cid.Cid, depth int) error
	walk = func(nodeCID cid.Cid, depth int) error {
		if depth > maxProofDepth {
			return fmt.Errorf("mst walk exceeded %d levels in repo %s (corrupt tree)", maxProofDepth, did)
		}
		if visited[nodeCID.KeyString()] {
			return nil
		}
		visited[nodeCID.KeyString()] = true
		raw, err := emit(nodeCID, "mst node")
		if err != nil {
			return err
		}
		nd, err := mst.NodeDataFromCBOR(bytes.NewReader(raw))
		if err != nil {
			return fmt.Errorf("decode mst node %s: %w", nodeCID, err)
		}
		node := nd.Node(&nodeCID)
		// The two entry predicates are tested INDEPENDENTLY, never as an
		// ordered either/or. Today's decode path splits a wire entry's value
		// and subtree pointer into separate unfolded entries (pinned by
		// TestRealRepoMSTNodesSplitChildAndValueEntriesExclusively), but the
		// export's completeness must not hang on that representation detail:
		// a both-set entry must walk its child AND emit its record, or the
		// export serves a short CAR that parses as a complete repo. An entry
		// naming neither is a corrupt node, and the only safe answer is to
		// FAIL — silently skipping it drops a whole subtree the same way.
		for i := range node.Entries {
			e := &node.Entries[i]
			if !e.IsChild() && !e.IsValue() {
				return fmt.Errorf("mst node %s in repo %s: entry %d is neither child nor value (corrupt tree)", nodeCID, did, i)
			}
			if e.IsChild() {
				// Belt-and-braces, not load-bearing: IsChild() ≡ (ChildCID !=
				// nil) today, by the executed-equivalence pin — the guard just
				// turns a future representation change into a clean
				// corrupt-tree error instead of a nil dereference. Same below.
				if e.ChildCID == nil {
					return fmt.Errorf("mst node %s in repo %s: child entry with no CID (corrupt tree)", nodeCID, did)
				}
				if err := walk(*e.ChildCID, depth+1); err != nil {
					return err
				}
			}
			if e.IsValue() {
				if e.Value == nil {
					return fmt.Errorf("mst node %s in repo %s: value entry with no CID (corrupt tree)", nodeCID, did)
				}
				// Two rkeys holding byte-identical records share one block —
				// the visited set keeps the CAR de-duplicated.
				if !visited[e.Value.KeyString()] {
					visited[e.Value.KeyString()] = true
					if _, err := emit(*e.Value, "record"); err != nil {
						return err
					}
				}
			}
		}
		return nil
	}
	return walk(commit.Data, 0)
}

// RepoInfo is one repo's head, for com.atproto.sync.listRepos.
type RepoInfo struct {
	DID       string
	CommitCID string
	Rev       string
	// Active is the layer-2 served-status flag (S4-D). A deactivated repo stays
	// listed — with active:false + status:"deactivated" — because the Sync v1.1
	// listRepos lexicon carries exactly those fields so a relay learns the
	// account is known-but-inactive rather than assuming it vanished.
	Active bool
}

// ListRepos returns EVERY repo with a committed head, DID-sorted — INCLUDING
// deactivated ones (each carries its Active flag). It is the internal,
// complete listing: the firehose's #sync degrade must announce every repo, and
// the boot self-check must reload every repo, so neither may be paginated.
// The public com.atproto.sync.listRepos handler uses [Store.ListReposPage].
func (s *Store) ListRepos(ctx context.Context) ([]RepoInfo, error) {
	return s.ListReposPage(ctx, "", 0)
}

// ListReposPage is ListRepos over a keyset window: the repos whose DID sorts
// strictly after afterDID (empty = from the start), at most limit of them.
// A limit <= 0 means unbounded, which is how ListRepos reuses this body.
//
// The keyset is the DID itself rather than an offset, so a repo minted or
// removed mid-walk can never make the page after it skip or repeat an entry.
func (s *Store) ListReposPage(ctx context.Context, afterDID string, limit int) ([]RepoInfo, error) {
	if limit <= 0 {
		limit = -1 // SQLite: a negative LIMIT is no upper bound
	}
	rows, err := s.db.QueryContext(ctx,
		`SELECT did, commit_cid, rev, active FROM repo_heads WHERE did > ? ORDER BY did LIMIT ?`,
		afterDID, limit)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []RepoInfo
	for rows.Next() {
		var r RepoInfo
		var flag int
		if err := rows.Scan(&r.DID, &r.CommitCID, &r.Rev, &flag); err != nil {
			return nil, err
		}
		r.Active = flag != 0
		out = append(out, r)
	}
	return out, rows.Err()
}

// GetRecord returns one record's CID + dag-cbor bytes at (collection, rkey), or
// ok=false if absent — backs com.atproto.repo.getRecord / sync.getRecord.
func (s *Store) GetRecord(ctx context.Context, did, collection, rkey string) (recordCID string, recordCBOR []byte, ok bool, err error) {
	path := collection + "/" + rkey
	err = s.db.QueryRowContext(ctx,
		`SELECT record_cid, record_bytes FROM records WHERE did = ? AND path = ?`, did, path).
		Scan(&recordCID, &recordCBOR)
	if err == sql.ErrNoRows {
		return "", nil, false, nil
	}
	if err != nil {
		return "", nil, false, err
	}
	return recordCID, recordCBOR, true, nil
}

// RecordInfo is one record of a collection listing (com.atproto.repo.listRecords).
type RecordInfo struct {
	Rkey       string
	RecordCID  string
	RecordCBOR []byte
}

// ListRecords returns every record in did's collection, rkey-sorted — the
// complete listing, for internal callers. The public
// com.atproto.repo.listRecords handler uses [Store.ListRecordsPage].
func (s *Store) ListRecords(ctx context.Context, did, collection string) ([]RecordInfo, error) {
	return s.ListRecordsPage(ctx, did, collection, "", 0)
}

// ListRecordsPage is ListRecords over a keyset window: the records whose rkey
// sorts strictly after afterRkey (empty = from the start), at most limit of
// them. A limit <= 0 means unbounded, which is how ListRecords reuses this body.
//
// The comparison runs on the stored `path` (collection + "/" + rkey) so it uses
// the records PK's own ordering rather than a computed column, and the keyset —
// not an offset — is what keeps a record created or deleted mid-walk from
// making the next page skip or repeat one.
func (s *Store) ListRecordsPage(ctx context.Context, did, collection, afterRkey string, limit int) ([]RecordInfo, error) {
	prefix := collection + "/"
	if limit <= 0 {
		limit = -1 // SQLite: a negative LIMIT is no upper bound
	}
	rows, err := s.db.QueryContext(ctx,
		`SELECT path, record_cid, record_bytes FROM records
		 WHERE did = ? AND path LIKE ? || '/%' AND path > ? ORDER BY path LIMIT ?`,
		did, collection, prefix+afterRkey, limit)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []RecordInfo
	for rows.Next() {
		var path, recordCID string
		var recordCBOR []byte
		if err := rows.Scan(&path, &recordCID, &recordCBOR); err != nil {
			return nil, err
		}
		out = append(out, RecordInfo{Rkey: strings.TrimPrefix(path, prefix), RecordCID: recordCID, RecordCBOR: recordCBOR})
	}
	return out, rows.Err()
}

// RecordPath is one record's address inside a repo, collection and rkey split
// back out of the stored `path`.
type RecordPath struct {
	Collection string
	Rkey       string
}

// RecordPathsPage returns did's record addresses across EVERY collection, in
// `path` order, starting strictly after afterPath (empty = from the start) and
// at most limit of them — the delete-presence sweep's enumerator.
//
// It deliberately selects no record bytes: a delete op needs only the address,
// and a sweep over a large repo would otherwise pull the whole repo into memory
// to throw it away. Like [Store.ListRecordsPage] the window is a keyset on the
// records PK's own ordering, so records vanishing under the walk — which is
// exactly what the sweep itself does — can never make the next page skip one.
func (s *Store) RecordPathsPage(ctx context.Context, did, afterPath string, limit int) (paths []RecordPath, nextAfter string, err error) {
	if limit <= 0 {
		limit = -1 // SQLite: a negative LIMIT is no upper bound
	}
	rows, err := s.db.QueryContext(ctx,
		`SELECT path FROM records WHERE did = ? AND path > ? ORDER BY path LIMIT ?`,
		did, afterPath, limit)
	if err != nil {
		return nil, "", err
	}
	defer rows.Close()
	for rows.Next() {
		var path string
		if err := rows.Scan(&path); err != nil {
			return nil, "", err
		}
		collection, rkey, ok := strings.Cut(path, "/")
		if !ok {
			// Unreachable through the funnel, which always writes
			// collection + "/" + rkey; refuse rather than silently sweep
			// something whose address we cannot name back.
			return nil, "", fmt.Errorf("record path %q in repo %s has no collection separator", path, did)
		}
		paths = append(paths, RecordPath{Collection: collection, Rkey: rkey})
		nextAfter = path
	}
	return paths, nextAfter, rows.Err()
}

// Collections returns the distinct NSIDs did has records in, sorted — the
// com.atproto.repo.describeRepo `collections` list.
func (s *Store) Collections(ctx context.Context, did string) ([]string, error) {
	rows, err := s.db.QueryContext(ctx, `SELECT path FROM records WHERE did = ? ORDER BY path`, did)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	seen := map[string]struct{}{}
	var out []string
	for rows.Next() {
		var path string
		if err := rows.Scan(&path); err != nil {
			return nil, err
		}
		if i := strings.IndexByte(path, '/'); i > 0 {
			col := path[:i]
			if _, dup := seen[col]; !dup {
				seen[col] = struct{}{}
				out = append(out, col)
			}
		}
	}
	return out, rows.Err()
}

// ProjectionCursor is the projection loop's per-DID watermark + first-emit gate,
// read at the top of each pass and advanced after it. LastCreatedAtMicros +
// LastPostID form the exclusive resume point handed to fetch_public_posts (the
// (created_at, post_id) ordering the nest pages by). FirstEmitGated is true
// until the user's first firehose frame has actually emitted — the pre-firehose
// resolvability gate (atproto-pds-full.md § Ecosystem reality: first-impression
// trap). A DID with no row yet reads as the zero cursor with the gate ON (the
// column default), so a fresh user starts from the beginning, gated.
// PublishedHandle is the ATProto handle this DID's identity was last PUBLISHED
// under — the value in the DID document's alsoKnownAs, and the one the last
// #identity frame announced. The projection loop compares it against the
// handle nest derives at read time to detect a rename (D-s3-6). Empty means
// "never observed" (a freshly minted identity, or a wiped store), which is the
// adopt-silently case: converge without announcing a rename we have no
// evidence happened. A cache, never a source of truth — the PLC log is
// authoritative and this column is re-learned from it (C6).
type ProjectionCursor struct {
	LastCreatedAtMicros int64
	LastPostID          string
	FirstEmitGated      bool
	PublishedHandle     string
}

// ProjectionState returns did's projection cursor, or the zero cursor with
// FirstEmitGated=true when the DID has no row yet (matching the column
// defaults) — so the caller never special-cases "first sight".
func (s *Store) ProjectionState(ctx context.Context, did string) (ProjectionCursor, error) {
	var (
		micros    int64
		postID    string
		gated     int
		published string
	)
	err := s.db.QueryRowContext(ctx,
		`SELECT last_created_at_micros, last_post_id, first_emit_gated, published_handle
		   FROM projection_state WHERE did = ?`,
		did).Scan(&micros, &postID, &gated, &published)
	if err == sql.ErrNoRows {
		return ProjectionCursor{FirstEmitGated: true}, nil
	}
	if err != nil {
		return ProjectionCursor{}, err
	}
	return ProjectionCursor{
		LastCreatedAtMicros: micros,
		LastPostID:          postID,
		FirstEmitGated:      gated != 0,
		PublishedHandle:     published,
	}, nil
}

// FirstEmitGated reports whether did's pre-firehose resolvability gate is still
// closed. It is ProjectionState's gate column on its own, for the callers that
// need the verdict and nothing else: the funnel's emit chokepoint and the write
// path's pre-check. A DID with no row reads GATED, exactly as ProjectionState
// reads it — never projected is never announced.
func (s *Store) FirstEmitGated(ctx context.Context, did string) (bool, error) {
	return firstEmitGated(ctx, s.db, did)
}

func firstEmitGated(ctx context.Context, q querier, did string) (bool, error) {
	var gated int
	err := q.QueryRowContext(ctx,
		`SELECT first_emit_gated FROM projection_state WHERE did = ?`, did).Scan(&gated)
	if err == sql.ErrNoRows {
		return true, nil
	}
	if err != nil {
		return false, err
	}
	return gated != 0, nil
}

// SetPublishedHandle records the handle did's identity is now published under,
// after the PLC update op has been accepted and the #identity frame emitted.
// Written last in the rename sequence on purpose: a crash before it leaves the
// STALE handle in place, which is exactly the evidence the next pass needs to
// know it still owes the frame (D-s3-6).
func (s *Store) SetPublishedHandle(ctx context.Context, did, handle string) error {
	_, err := s.db.ExecContext(ctx,
		`INSERT INTO projection_state(did, published_handle) VALUES(?,?)
		 ON CONFLICT(did) DO UPDATE SET published_handle=excluded.published_handle`,
		did, handle)
	return err
}

// SetProjectionWatermark advances (upserts) did's resume point after a pass.
// The first-emit gate is left untouched — SetFirstEmitGated owns it — so a
// gated user whose watermark advances (e.g. skipped, unmapped tombstones) keeps
// its gate until a real emit clears it.
func (s *Store) SetProjectionWatermark(ctx context.Context, did string, lastMicros int64, lastPostID string) error {
	_, err := s.db.ExecContext(ctx,
		`INSERT INTO projection_state(did, last_created_at_micros, last_post_id) VALUES(?,?,?)
		 ON CONFLICT(did) DO UPDATE SET last_created_at_micros=excluded.last_created_at_micros, last_post_id=excluded.last_post_id`,
		did, lastMicros, lastPostID)
	return err
}

// SetFirstEmitGated flips did's first-emit gate. The projection loop clears it
// (gated=false) once the user's first firehose frame has actually emitted; it is
// never re-raised (resolvability is monotonic — a DID that resolved stays
// resolved). Creates the row if absent so a not-yet-projected DID can be gated.
func (s *Store) SetFirstEmitGated(ctx context.Context, did string, gated bool) error {
	g := 0
	if gated {
		g = 1
	}
	_, err := s.db.ExecContext(ctx,
		`INSERT INTO projection_state(did, first_emit_gated) VALUES(?,?)
		 ON CONFLICT(did) DO UPDATE SET first_emit_gated=excluded.first_emit_gated`,
		did, g)
	return err
}
