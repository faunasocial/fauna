package atprotorepo

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"fmt"
	"time"

	"github.com/ipfs/go-cid"
	mh "github.com/multiformats/go-multihash"
)

// The bridge's blob half: the bytes an ATProto consumer fetches with
// com.atproto.sync.getBlob, keyed by the CID the record's blob ref names.
//
// These bytes are a DUPLICATE of what the nest already stores — the ratified
// cost in atproto-pds-bridge.md § Goal ("storage duplication (cheap) + blob
// re-hashing (BLAKE3/FastCDC → sha256-CID)"). The duplication is not laziness:
// ATProto addresses a blob by the sha256 of its bytes while Fauna addresses it
// by BLAKE3, so the two content-address spaces cannot share a store, and a
// consumer must be able to fetch by the CID our own record published.
//
// Like every other table here the content is DERIVED and re-derivable (C6): a
// wiped blob store re-fills from nest on the next projection pass, because the
// Fauna post still names the source bytes.

// blobsSchema is the blob table, applied beside the main schema. Kept separate
// from `schema` only for readability — Open applies both.
const blobsSchema = `
CREATE TABLE IF NOT EXISTS blobs (
    did        TEXT NOT NULL,
    cid        TEXT NOT NULL,
    fauna_cid  TEXT NOT NULL,
    mime       TEXT NOT NULL,
    size       INTEGER NOT NULL,
    bytes      BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (did, cid)
);
CREATE INDEX IF NOT EXISTS blobs_did_fauna_cid ON blobs(did, fauna_cid);
`

// BlobCIDForBytes returns the ATProto blob CID of b: CIDv1, raw codec (0x55),
// sha2-256 multihash — the identifier a record's blob `ref` carries and
// com.atproto.sync.getBlob is asked for.
//
// This is the "blob re-hashing" half of the projection's residual cost: the
// same bytes Fauna addresses by BLAKE3 get a second, sha256-based name for the
// target network. Deterministic, so a re-projection of an unchanged blob names
// the identical CID and the repo stays byte-reproducible.
func BlobCIDForBytes(b []byte) (cid.Cid, error) {
	sum := sha256.Sum256(b)
	digest, err := mh.Encode(sum[:], mh.SHA2_256)
	if err != nil {
		return cid.Undef, fmt.Errorf("encode sha256 multihash: %w", err)
	}
	return cid.NewCidV1(cid.Raw, digest), nil
}

// BlobRow is one stored blob's metadata plus its bytes.
type BlobRow struct {
	CID string
	// FaunaCID is the Fauna content CID the bytes were fetched by — the
	// provenance that lets a second sighting of the same attachment skip the
	// fetch AND the re-hash. Needed because the two content-address spaces do
	// not map onto each other: BLAKE3 → sha256 is only computable by holding
	// the bytes, so without this column "have I already published this
	// attachment?" would require re-fetching it to find out.
	FaunaCID string
	MIME     string
	Size     int64
	Bytes    []byte
}

// PutBlob stores bytes for did under blobCID, idempotently: re-storing an
// identical blob is a no-op, which is what makes a re-projection free.
//
// The caller MUST have derived blobCID from these exact bytes
// ([BlobCIDForBytes]); nothing here re-verifies it, because the read surface
// serves what it stored under the name the record published — a mismatch would
// be a bug in the caller, not an attacker-reachable input (the bytes come from
// the nest's own CID-verified blob route).
func (s *Store) PutBlob(ctx context.Context, did, blobCID, faunaCID, mime string, data []byte) error {
	_, err := s.db.ExecContext(ctx,
		`INSERT INTO blobs (did, cid, fauna_cid, mime, size, bytes, created_at)
		 VALUES (?, ?, ?, ?, ?, ?, ?)
		 ON CONFLICT (did, cid) DO NOTHING`,
		did, blobCID, faunaCID, mime, len(data), data, time.Now().UnixMicro())
	if err != nil {
		return fmt.Errorf("store blob %s for %s: %w", blobCID, did, err)
	}
	return nil
}

// BlobByFaunaCID returns did's already-published blob for a Fauna attachment,
// without its bytes — the dedup lookup ResolveMedia runs before fetching
// anything. ok is false when this repo has never published that attachment.
func (s *Store) BlobByFaunaCID(ctx context.Context, did, faunaCID string) (BlobRow, bool, error) {
	row := BlobRow{FaunaCID: faunaCID}
	err := s.db.QueryRowContext(ctx,
		`SELECT cid, mime, size FROM blobs WHERE did = ? AND fauna_cid = ? ORDER BY cid LIMIT 1`,
		did, faunaCID).Scan(&row.CID, &row.MIME, &row.Size)
	if err == sql.ErrNoRows {
		return BlobRow{}, false, nil
	}
	if err != nil {
		return BlobRow{}, false, fmt.Errorf("look up fauna blob %s for %s: %w", faunaCID, did, err)
	}
	return row, true, nil
}

// FaunaCIDForBlob is BlobByFaunaCID read backwards: given an ATProto blob CID a
// record references, it answers which Fauna content the bytes are — the
// bridge-side half of the inbound-picture resolution (atproto-pds-full.md § F2
// detail). ok is false when this repo has never published that blob, which is
// the nest's signal to fall back to its own upload ledger and, failing that, to
// refuse the write.
//
// Deliberately NOT GetBlob: that one selects `bytes` too, so using it as a
// reverse index would read a whole picture (up to MaxBlobBytes) into memory per
// ref, on the write path, to answer a question about a CID string.
func (s *Store) FaunaCIDForBlob(ctx context.Context, did, blobCID string) (string, bool, error) {
	var faunaCID string
	err := s.db.QueryRowContext(ctx,
		`SELECT fauna_cid FROM blobs WHERE did = ? AND cid = ?`, did, blobCID).Scan(&faunaCID)
	if err == sql.ErrNoRows {
		return "", false, nil
	}
	if err != nil {
		return "", false, fmt.Errorf("look up fauna cid for blob %s of %s: %w", blobCID, did, err)
	}
	return faunaCID, true, nil
}

// GetBlob returns did's blob under blobCID. ok is false when the repo does not
// serve it.
func (s *Store) GetBlob(ctx context.Context, did, blobCID string) (BlobRow, bool, error) {
	var row BlobRow
	row.CID = blobCID
	err := s.db.QueryRowContext(ctx,
		`SELECT fauna_cid, mime, size, bytes FROM blobs WHERE did = ? AND cid = ?`, did, blobCID).
		Scan(&row.FaunaCID, &row.MIME, &row.Size, &row.Bytes)
	if err == sql.ErrNoRows {
		return BlobRow{}, false, nil
	}
	if err != nil {
		return BlobRow{}, false, fmt.Errorf("read blob %s for %s: %w", blobCID, did, err)
	}
	return row, true, nil
}

// ListBlobCIDs returns up to limit of did's blob CIDs in CID order, starting
// strictly after `after` (empty = from the beginning), plus the cursor to
// resume from when more remain.
//
// Keyset, never an offset — the same rule listRepos/listRecords follow: a blob
// added or removed mid-walk must not be able to shift the window and make the
// next page skip or repeat an entry. CID order is arbitrary but stable, which
// is all a resumable walk needs.
func (s *Store) ListBlobCIDs(ctx context.Context, did, after string, limit int) (cids []string, next string, err error) {
	rows, err := s.db.QueryContext(ctx,
		`SELECT cid FROM blobs WHERE did = ? AND cid > ? ORDER BY cid LIMIT ?`,
		did, after, limit+1)
	if err != nil {
		return nil, "", fmt.Errorf("list blobs for %s: %w", did, err)
	}
	defer rows.Close()
	for rows.Next() {
		var c string
		if err := rows.Scan(&c); err != nil {
			return nil, "", err
		}
		cids = append(cids, c)
	}
	if err := rows.Err(); err != nil {
		return nil, "", err
	}
	// One extra row was requested purely to learn whether more remain; it is
	// never served, so the cursor is always the last SERVED cid.
	if len(cids) > limit {
		cids = cids[:limit]
		next = cids[len(cids)-1]
	}
	return cids, next, nil
}
