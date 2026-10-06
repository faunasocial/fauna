package atprotorepo

import (
	"bytes"
	"context"
	"database/sql"
	"errors"
	"fmt"
	"sort"
	"strings"
	"sync"
	"time"

	comatproto "github.com/bluesky-social/indigo/api/atproto"
	"github.com/bluesky-social/indigo/atproto/atcrypto"
	"github.com/bluesky-social/indigo/atproto/repo"
	"github.com/bluesky-social/indigo/atproto/repo/mst"
	"github.com/bluesky-social/indigo/atproto/syntax"
	"github.com/bluesky-social/indigo/events"
	lexutil "github.com/bluesky-social/indigo/lex/util"

	blocks "github.com/ipfs/go-block-format"
	"github.com/ipfs/go-cid"
)

// dagCBOR builds a CIDv1 with the dag-cbor codec (0x71) and sha2-256 (0x12),
// the content-addressing every atproto block uses.
var dagCBOR = cid.V1Builder{Codec: 0x71, MhType: 0x12, MhLength: 0}

// Frame type strings recorded in firehose_events.frame_type.
const (
	FrameCommit   = "#commit"
	FrameSync     = "#sync"
	FrameIdentity = "#identity"
	FrameAccount  = "#account"
)

// ErrFirstEmitGated refuses a firehose emission for an identity still behind
// the pre-firehose resolvability gate (atproto-pds-full.md § Ecosystem reality:
// the first-impression trap — a DID that does not resolve at first AppView index
// can permanently 404, because later events only UPDATE an actor row that was
// never created).
//
// The projection loop is the only thing that OPENS the gate, and it checks
// before it commits; this is the chokepoint that makes that discipline
// structural rather than a property of one caller. It exists because it was
// once only a property of one caller: the F2 write path committed straight
// through ApplyBatch without consulting the gate, so an external app writing
// before its handle resolved could put the account's first event on the network
// unresolvable — the exact trap the gate exists to prevent.
//
// A gated DID has never been announced, so there is nothing for a suppressed
// #identity/#sync/#account to correct: suppressing them is right, not lossy.
var ErrFirstEmitGated = errors.New("first-emit gated: identity is not yet resolvable ecosystem-side")

// refuseIfFirstEmitGated is the one decision point behind [ErrFirstEmitGated],
// called by every producer that appends to the firehose outbox.
func (f *Funnel) refuseIfFirstEmitGated(ctx context.Context, did string) error {
	gated, err := firstEmitGated(ctx, f.store.db, did)
	if err != nil {
		return fmt.Errorf("read first-emit gate: %w", err)
	}
	if gated {
		return fmt.Errorf("%w: %s", ErrFirstEmitGated, did)
	}
	return nil
}

// AccountStatusDeactivated is the #account frame's `status` value when
// active=false, and the com.atproto.sync.getRepoStatus / listRepos `status`
// string for a deactivated account (one of the Sync v1.1 known values). Shared
// by the firehose producer and the read surface so the two never drift on the
// spelling a relay keys off.
const AccountStatusDeactivated = "deactivated"

// AccountStatusDeleted is the #account frame's `status` value for the terminal
// announcement of a delete-presence sweep (atproto-pds-bridge.md § Disable &
// revocation layer 2 — the "separate, stronger action"). Unlike
// [AccountStatusDeactivated] it has no read-surface twin: the sweep drops the
// repo_heads row with the rest of the repo, so every subsequent read answers
// RepoNotFound and listRepos omits the DID. That is the spec-conformant shape —
// the sync lexicons define no `RepoDeleted` error, and this frame is the
// announcement.
const AccountStatusDeleted = "deleted"

// RepoOp actions.
const (
	ActionCreate = "create"
	ActionUpdate = "update"
	ActionDelete = "delete"
)

// Signer is the per-user repo signing key (K-256), unsealed by the bridge and
// passed into each commit. It never lives in the store.
type Signer = atcrypto.PrivateKey

// RepoOp is one mutation in a batch: create/update/delete a single record at
// (Collection, Rkey). The funnel is protocol-agnostic — the caller (the
// projection loop, or F2's authed write path) has already translated the Fauna
// object into the dag-cbor record bytes.
type RepoOp struct {
	Action     string // ActionCreate | ActionUpdate | ActionDelete
	Collection string // NSID, e.g. "app.bsky.feed.post"
	Rkey       string // record key: a deterministic TID for posts, "self" for profile
	RecordCBOR []byte // canonical dag-cbor record bytes; nil for a delete
	// FaunaPostID is the lowercase-hex 32-byte content-row id this op maps
	// from, recorded in post_map for delete + reply/quote resolution. Empty
	// for records with no Fauna post identity (e.g. the profile singleton).
	FaunaPostID string
	// RootURI/RootCID are the thread root this record anchors to, recorded in
	// post_map so a later reply resolves its own root in one lookup. Both empty
	// for a record that is itself a thread root (every non-reply post, and any
	// reply whose parent was not bridged). Written in the SAME txn as the
	// commit, so the root index can never outlive or lag the record it describes.
	RootURI string
	RootCID string
	// BlobCIDs are the ATProto blob CIDs this op's record references — what the
	// #commit frame's `blobs` field announces, so a consumer learns which blobs
	// to fetch without decoding the record. Empty for a record with no media,
	// which is every non-media op (the field is additive and its zero value
	// is the no-media case), so F1's own ApplyBatch call sites compile
	// untouched.
	BlobCIDs []string
}

// frameBlobLinks collects the blob CIDs a batch's records reference, for the
// #commit frame's `blobs` field, deduplicated and in op order.
//
// The field is REQUIRED in the lexicon and carries no deprecation marker (unlike
// the frame's `rebase`/`tooBig`), so emitting `[]` while the commit does
// reference blobs is a small lie about the commit — and the honest value is
// free, since the projection has just resolved exactly this set. A consumer can
// then learn which blobs to fetch without decoding the records.
func frameBlobLinks(ops []RepoOp) ([]lexutil.LexLink, error) {
	links := []lexutil.LexLink{}
	seen := make(map[string]struct{})
	for _, op := range ops {
		for _, c := range op.BlobCIDs {
			if _, dup := seen[c]; dup {
				continue
			}
			seen[c] = struct{}{}
			parsed, err := cid.Decode(c)
			if err != nil {
				return nil, fmt.Errorf("blob cid %q in commit frame: %w", c, err)
			}
			links = append(links, lexutil.LexLink(parsed))
		}
	}
	return links, nil
}

// CommitResult reports the outcome of one ApplyBatch. NoChange is set (and no
// commit is written) when the batch reduced to nothing — e.g. a delete of a
// record that was never projected.
//
// Seq and Frame are zero for a batch applied with [DeferFrameToSync]: that
// commit deliberately allocates no firehose seq and serializes no frame.
type CommitResult struct {
	Rev       string
	CommitCID string
	DataCID   string // the MST root CID
	Seq       int64  // the firehose seq of the emitted #commit (0 when deferred)
	Ops       int    // number of ops that actually changed the repo
	Frame     []byte // the serialized #commit frame (also persisted in the outbox)
	NoChange  bool
}

// BatchOption tunes one ApplyBatch call. The empty set is the ordinary live
// path: every commit announces itself with its own #commit frame.
type BatchOption func(*batchOpts)

type batchOpts struct {
	deferFrameToSync bool
	skipUnchanged    bool
	swapCommit       string
	swapRecords      map[string]RecordSwap
}

// ErrSwapMismatch is a compare-and-swap failure: the repo head or a record the
// caller pinned is no longer the value they named, so the batch was applied in
// no part. Callers map it onto the lexicon's `InvalidSwap`.
var ErrSwapMismatch = errors.New("compare-and-swap precondition failed")

// RecordSwap is one record-level CAS expectation: the record at a path must
// currently have CID [RecordSwap.CID], or — when [RecordSwap.MustNotExist] —
// must not exist at all.
//
// The two are genuinely distinct states, which is why this is a struct and not
// a string: `putRecord`'s `swapRecord` is nullable, and an explicit `null`
// asserts "this record does not exist yet". Collapsing that into an empty
// string would serve a write the caller asked us to guard.
type RecordSwap struct {
	CID          string
	MustNotExist bool
}

// ExpectCommit pins the batch to a repo head: the current commit CID must be
// `cid` or the batch fails with [ErrSwapMismatch], having applied nothing.
//
// This is the `swapCommit` GUARANTEE, not its error message. The write path
// pre-checks the same value before it calls the nest (so a doomed write never
// creates a Fauna post), but that read cannot be atomic with this commit — a
// concurrent writer for the same account can land in between. Only a compare
// taken INSIDE ApplyBatch's per-DID lock, against the same head the commit
// chains onto, can actually keep the caller's concurrency guarantee. Two-site
// enforcement, the same shape as the first-emit gate: the handler pre-check
// is the good error, this is the truth.
func ExpectCommit(cid string) BatchOption { return func(o *batchOpts) { o.swapCommit = cid } }

// ExpectRecords pins individual records by path (`collection/rkey`) — the
// `swapRecord` guarantee, the record-level twin of [ExpectCommit] and enforced
// at the same point for the same reason.
func ExpectRecords(want map[string]RecordSwap) BatchOption {
	return func(o *batchOpts) { o.swapRecords = want }
}

// CheckCommitSwap compares a `swapCommit` expectation against a repo head.
// Exported because the CAS is enforced at TWO sites — the write path's
// pre-check before it calls the nest, and [Funnel.ApplyBatch] under the per-DID
// lock — and both must reach the same verdict with the same wording. One owner
// for the comparison, two callers.
func CheckCommitSwap(want, headCID string, hasHead bool) error {
	if want == "" {
		return nil
	}
	// A repo with no commit yet has no head to match, so a swapCommit naming
	// one is a mismatch rather than a vacuous pass.
	if !hasHead {
		return fmt.Errorf("%w: swapCommit %s, but this repo has no commit yet", ErrSwapMismatch, want)
	}
	if headCID != want {
		return fmt.Errorf("%w: swapCommit %s, but the repo head is %s", ErrSwapMismatch, want, headCID)
	}
	return nil
}

// CheckRecordSwap compares a `swapRecord` expectation against the record
// currently at `path`. Exported for the same two-site reason as
// [CheckCommitSwap].
func CheckRecordSwap(want RecordSwap, path, gotCID string, exists bool) error {
	switch {
	case want.MustNotExist && exists:
		return fmt.Errorf("%w: swapRecord asserted %s does not exist, but it is at %s",
			ErrSwapMismatch, path, gotCID)
	case want.MustNotExist:
		// Asserted absent and absent — the assertion holds.
		return nil
	case !exists:
		return fmt.Errorf("%w: swapRecord %s for %s, but no such record exists",
			ErrSwapMismatch, want.CID, path)
	case gotCID != want.CID:
		return fmt.Errorf("%w: swapRecord %s for %s, but the record is %s",
			ErrSwapMismatch, want.CID, path, gotCID)
	}
	return nil
}

// checkSwap evaluates the batch's CAS expectations against the repo state just
// read under the per-DID lock. Every mismatch is [ErrSwapMismatch]; the message
// names what was expected and what is actually there, since a CAS failure is a
// retry-after-re-reading signal rather than a bug.
func checkSwap(opts batchOpts, head headRow, hasHead bool, recs map[string]recordRow) error {
	if err := CheckCommitSwap(opts.swapCommit, head.commitCID, hasHead); err != nil {
		return err
	}
	// Sorted so a multi-record batch reports the same path first every run —
	// a CAS failure the caller sees must not depend on map iteration order.
	paths := make([]string, 0, len(opts.swapRecords))
	for path := range opts.swapRecords {
		paths = append(paths, path)
	}
	sort.Strings(paths)
	for _, path := range paths {
		got, exists := recs[path]
		if err := CheckRecordSwap(opts.swapRecords[path], path, got.recordCID, exists); err != nil {
			return err
		}
	}
	return nil
}

// DeferFrameToSync applies the batch WITHOUT writing its #commit frame to the
// outbox, and records on the repo that it now owes a #sync
// ([Store.SyncOwed]). It is the primitive behind the downtime-catch-up collapse
// (atproto-pds-bridge.md § Projection & backfill, watermark row 3: "replay gap
// as #commit; collapse huge gaps to one #sync + relay re-getRepo").
//
// The commit itself is in no way weaker: blocks, MST, head, records and
// post_map are written exactly as on the live path, so the repo stays
// re-derivable and its rkeys deterministic. Only the *announcement* changes —
// instead of N frames the caller emits ONE [Funnel.EmitSync] once the gap is
// drained, and the relay refetches the result with getRepo.
//
// The owed flag is what makes that announcement impossible to lose. Without it
// a crash between the last deferred commit and the #sync would leave the
// network permanently behind: the next ordinary #commit would carry a
// Sync v1.1 `prevData` naming an MST root no consumer ever saw.
func DeferFrameToSync() BatchOption { return func(o *batchOpts) { o.deferFrameToSync = true } }

// SkipUnchangedRecords drops any create/update op whose record bytes are
// already byte-identical to the stored record, so a batch that would rewrite
// the repo with what it already holds reduces to [CommitResult.NoChange]
// instead of minting a rev and broadcasting a #commit.
//
// It exists for the PROJECTION, which re-derives a user's whole repo from
// Fauna state on every pass: the profile singleton is re-projected each time
// whether or not anybody edited it, and without this a byte-identical record
// would be re-announced to the network forever.
//
// Deliberately opt-in rather than the funnel's default. An external write
// (`com.atproto.repo.putRecord` / `applyWrites`) must still produce a commit
// and hand the caller back a rev and a record CID even when the bytes match
// what is already there — the lexicon promises a commit, and an app that
// re-puts identical content is entitled to one. Only a re-derivation knows
// that "identical" means "nothing happened".
//
// The comparison runs inside ApplyBatch, under the per-DID lock, so it cannot
// race a concurrent writer the way a read-then-write in the caller could.
func SkipUnchangedRecords() BatchOption { return func(o *batchOpts) { o.skipUnchanged = true } }

// RevClock allocates repo revisions (TIDs). Production uses a wall-clock TID
// clock (monotonic across restarts because time moves forward); tests inject a
// fixed sequence to make full commit CIDs reproducible.
type RevClock interface {
	Next() string
}

type revClockFunc func() string

func (f revClockFunc) Next() string { return f() }

// NewTIDRevClock returns the production RevClock: a mutex-guarded indigo TID
// clock. TIDs embed the wall-clock microsecond, so a fresh clock after a
// restart still produces revs greater than any previously persisted head.
func NewTIDRevClock() RevClock {
	clk := syntax.NewTIDClock(0)
	var mu sync.Mutex
	return revClockFunc(func() string {
		mu.Lock()
		defer mu.Unlock()
		return clk.Next().String()
	})
}

// Funnel is the single writer for every repo in the store. All mutation flows
// through ApplyBatch, serialized per DID, so a repo is never half-mutated and
// its firehose frame is always committed with its head (C1).
type Funnel struct {
	store *Store
	clock RevClock

	mu    sync.Mutex
	didMu map[string]*sync.Mutex

	// onCommit, when set, is called after each successful commit txn so the
	// firehose broadcaster can wake immediately instead of waiting for its poll
	// tick. Best-effort and non-blocking: the broadcaster re-reads the outbox
	// from its own cursor either way, so a dropped notification only delays.
	onCommit func()
}

// NewFunnel builds a funnel over store. Firehose seqs are allocated inside each
// commit txn (ApplyBatch step 6), so there is no in-memory counter to seed; the
// constructor still reads the outbox once, as a boot-time check that the schema
// is applied before the bridge claims to be serving. Pass a nil clock to use the
// production wall-clock TID clock.
func NewFunnel(ctx context.Context, store *Store, clock RevClock) (*Funnel, error) {
	if _, err := maxSeq(ctx, store.db); err != nil {
		return nil, fmt.Errorf("read firehose outbox: %w", err)
	}
	if clock == nil {
		clock = NewTIDRevClock()
	}
	return &Funnel{
		store: store,
		clock: clock,
		didMu: make(map[string]*sync.Mutex),
	}, nil
}

// SetOnCommit installs the post-commit notification hook (see Funnel.onCommit).
// Call it once at wiring time, before any commit.
func (f *Funnel) SetOnCommit(fn func()) { f.onCommit = fn }

func (f *Funnel) lockDID(did string) func() {
	f.mu.Lock()
	m, ok := f.didMu[did]
	if !ok {
		m = &sync.Mutex{}
		f.didMu[did] = m
	}
	f.mu.Unlock()
	m.Lock()
	return m.Unlock
}

// effOp is an op that actually changed the repo (a create/update, or a delete
// of a record that existed), carrying everything the persist step needs.
type effOp struct {
	action      string
	path        string
	recordCID   string
	recordCBOR  []byte
	faunaPostID string
	atURI       string
	rootURI     string
	rootCID     string
}

// ApplyBatch applies ops to did's repo under a per-DID lock: rebuild the MST
// from the resulting record set, sign a commit chained to the previous head,
// and persist blocks + head + records + post_map + the #commit outbox row in
// ONE transaction. The MST is rebuilt from scratch each time; because it is a
// pure function of the record set, the result is byte-identical to an
// incremental mutation and the whole carstore stays re-derivable (C6).
func (f *Funnel) ApplyBatch(ctx context.Context, did string, signer Signer, ops []RepoOp, options ...BatchOption) (CommitResult, error) {
	if did == "" {
		return CommitResult{}, fmt.Errorf("empty did")
	}
	var opts batchOpts
	for _, o := range options {
		o(&opts)
	}
	// The gate guards the FIREHOSE, not the repo: a DeferFrameToSync batch
	// appends no outbox row (it records a #sync debt instead), so it cannot
	// carry an unresolvable identity onto the network and is left alone. Every
	// other batch emits a #commit and must not.
	if !opts.deferFrameToSync {
		if err := f.refuseIfFirstEmitGated(ctx, did); err != nil {
			return CommitResult{}, err
		}
	}
	unlock := f.lockDID(did)
	defer unlock()

	// 1. Load the current record set + head. The per-DID lock guarantees this
	//    read is consistent with the write below.
	current, err := loadRecords(ctx, f.store.db, did)
	if err != nil {
		return CommitResult{}, fmt.Errorf("load records: %w", err)
	}
	head, hasHead, err := loadHead(ctx, f.store.db, did)
	if err != nil {
		return CommitResult{}, fmt.Errorf("load head: %w", err)
	}
	recs := make(map[string]recordRow, len(current))
	for _, r := range current {
		recs[r.path] = r
	}

	// 1b. Compare-and-swap, under the lock and against the very head this
	//     commit will chain onto — so no concurrent writer can slip between the
	//     check and the write. Before any op is applied: a failed CAS must
	//     leave the repo untouched.
	if err := checkSwap(opts, head, hasHead, recs); err != nil {
		return CommitResult{}, err
	}

	// 2. Apply ops in memory, keeping only the effective ones.
	var eff []effOp
	var frameOps []*comatproto.SyncSubscribeRepos_RepoOp
	for _, op := range ops {
		if op.Collection == "" || op.Rkey == "" {
			return CommitResult{}, fmt.Errorf("op missing collection/rkey")
		}
		path := op.Collection + "/" + op.Rkey
		atURI := "at://" + did + "/" + path
		switch op.Action {
		case ActionCreate, ActionUpdate:
			if len(op.RecordCBOR) == 0 {
				return CommitResult{}, fmt.Errorf("%s op %s has empty record", op.Action, path)
			}
			rc, err := dagCBOR.Sum(op.RecordCBOR)
			if err != nil {
				return CommitResult{}, err
			}
			// The firehose op action must reflect reality, not the caller's
			// label: a "create" of a path that already exists is an update
			// (the profile singleton at rkey "self" is re-projected as the
			// Fauna profile changes), and vice versa.
			prior, existed := recs[path]
			// A re-derivation that produced exactly what is already stored
			// changed nothing; under SkipUnchangedRecords it contributes no
			// effective op, so the batch can still reduce to NoChange.
			if opts.skipUnchanged && existed && bytes.Equal(prior.recordCBOR, op.RecordCBOR) {
				continue
			}
			frameAction := ActionCreate
			if existed {
				frameAction = ActionUpdate
			}
			recs[path] = recordRow{path: path, recordCID: rc.String(), recordCBOR: op.RecordCBOR}
			link := lexutil.LexLink(rc)
			frameOps = append(frameOps, &comatproto.SyncSubscribeRepos_RepoOp{Action: frameAction, Path: path, Cid: &link})
			eff = append(eff, effOp{action: op.Action, path: path, recordCID: rc.String(), recordCBOR: op.RecordCBOR, faunaPostID: op.FaunaPostID, atURI: atURI, rootURI: op.RootURI, rootCID: op.RootCID})
		case ActionDelete:
			if _, ok := recs[path]; !ok {
				continue // idempotent: nothing to delete
			}
			delete(recs, path)
			frameOps = append(frameOps, &comatproto.SyncSubscribeRepos_RepoOp{Action: ActionDelete, Path: path, Cid: nil})
			eff = append(eff, effOp{action: ActionDelete, path: path, faunaPostID: op.FaunaPostID})
		default:
			return CommitResult{}, fmt.Errorf("unknown op action %q", op.Action)
		}
	}
	if len(eff) == 0 {
		return CommitResult{NoChange: true}, nil
	}

	// 3. Rebuild the MST from the full current record set.
	bs := newMemBlockstore()
	tree := mst.NewEmptyTree()
	paths := make([]string, 0, len(recs))
	for p := range recs {
		paths = append(paths, p)
	}
	sort.Strings(paths)
	for _, p := range paths {
		rc, err := cid.Decode(recs[p].recordCID)
		if err != nil {
			return CommitResult{}, fmt.Errorf("decode record cid %s: %w", p, err)
		}
		if _, err := tree.Insert([]byte(p), rc); err != nil {
			return CommitResult{}, fmt.Errorf("mst insert %s: %w", p, err)
		}
	}
	rootCID, err := tree.WriteDiffBlocks(ctx, bs)
	if err != nil {
		return CommitResult{}, fmt.Errorf("write mst blocks: %w", err)
	}
	for _, p := range paths {
		rc, _ := cid.Decode(recs[p].recordCID)
		blk, err := blocks.NewBlockWithCid(recs[p].recordCBOR, rc)
		if err != nil {
			return CommitResult{}, err
		}
		if err := bs.Put(ctx, blk); err != nil {
			return CommitResult{}, err
		}
	}

	// 4. Build + sign the commit, chained to the previous head.
	rev := f.clock.Next()
	commit := repo.Commit{
		DID:     did,
		Version: repo.ATPROTO_REPO_VERSION,
		Data:    *rootCID,
		Rev:     rev,
	}
	var sincePtr *string
	var prevDataPtr *lexutil.LexLink
	if hasHead {
		if rev <= head.rev {
			return CommitResult{}, fmt.Errorf("rev clock regressed: %q <= head %q", rev, head.rev)
		}
		pc, err := cid.Decode(head.commitCID)
		if err != nil {
			return CommitResult{}, fmt.Errorf("decode prev commit cid: %w", err)
		}
		commit.Prev = &pc
		since := head.rev
		sincePtr = &since
		// Sync v1.1's inductive `prevData`: the MST root the previous commit
		// pointed at, so a consumer can validate this commit's diff by MST
		// inversion without holding the whole repo. Read it back off the
		// persisted prev commit block rather than caching it in repo_heads —
		// the decode costs microseconds on a path that already signs K-256, and
		// it keeps the schema unchanged.
		prevRaw, found, err := f.store.GetBlock(ctx, did, []byte(pc.KeyString()))
		if err != nil {
			return CommitResult{}, fmt.Errorf("load prev commit block: %w", err)
		}
		if !found {
			return CommitResult{}, fmt.Errorf("prev commit block %s missing for did %s", pc, did)
		}
		var prevCommit repo.Commit
		if err := prevCommit.UnmarshalCBOR(bytes.NewReader(prevRaw)); err != nil {
			return CommitResult{}, fmt.Errorf("decode prev commit: %w", err)
		}
		prevData := lexutil.LexLink(prevCommit.Data)
		prevDataPtr = &prevData
	}
	if err := commit.Sign(signer); err != nil {
		return CommitResult{}, fmt.Errorf("sign commit: %w", err)
	}
	var cbuf bytes.Buffer
	if err := commit.MarshalCBOR(&cbuf); err != nil {
		return CommitResult{}, fmt.Errorf("marshal commit: %w", err)
	}
	commitBytes := cbuf.Bytes()
	commitCID, err := dagCBOR.Sum(commitBytes)
	if err != nil {
		return CommitResult{}, err
	}
	commitBlk, err := blocks.NewBlockWithCid(commitBytes, commitCID)
	if err != nil {
		return CommitResult{}, err
	}
	if err := bs.Put(ctx, commitBlk); err != nil {
		return CommitResult{}, err
	}

	// 5. Diff the post-commit block set down to the blocks new since the last
	//    commit — the inductive #commit carries exactly these (changed MST
	//    path + new records + new commit), commit root first.
	oldKeys, err := blockKeyStrings(ctx, f.store.db, did)
	if err != nil {
		return CommitResult{}, fmt.Errorf("load block keys: %w", err)
	}
	var newBlks []blocks.Block
	diffCIDs := []cid.Cid{commitCID}
	for _, b := range bs.allBlocks() {
		if b.Cid().Equals(commitCID) {
			newBlks = append(newBlks, b)
			continue
		}
		if _, old := oldKeys[b.Cid().KeyString()]; old {
			continue
		}
		newBlks = append(newBlks, b)
		diffCIDs = append(diffCIDs, b.Cid())
	}
	diffCAR, err := writeCAR(ctx, commitCID, bs, diffCIDs)
	if err != nil {
		return CommitResult{}, fmt.Errorf("write diff car: %w", err)
	}

	// 6. Persist everything in ONE transaction: allocate the firehose seq,
	//    serialize the #commit frame, append the new blocks, move the head,
	//    apply the record + post_map changes, append the outbox row.
	//
	//    The PDS-global seq is allocated INSIDE the txn (MAX(seq)+1; the store
	//    holds a single connection, so a txn is exclusive) rather than from an
	//    in-memory counter. That makes the sequence gapless AND ordered with the
	//    commits themselves — a pre-txn counter can hand seq N to one DID while
	//    another takes N+1 and commits first, and a broadcaster following
	//    "seq > lastSent" would then skip N forever; a rolled-back txn would
	//    likewise burn its number and leave a permanent hole in the stream.
	now := time.Now().UnixMicro()
	tx, err := f.store.db.BeginTx(ctx, nil)
	if err != nil {
		return CommitResult{}, err
	}
	defer tx.Rollback()

	//    A DEFERRED batch skips this whole half: it allocates no seq and
	//    serializes no frame, so a collapsed catch-up leaves the stream's numbering
	//    exactly as the relay last saw it and the single #sync that follows takes
	//    the next number.
	var seq int64
	var frameBytes []byte
	if !opts.deferFrameToSync {
		prevSeq, err := maxSeq(ctx, tx)
		if err != nil {
			return CommitResult{}, fmt.Errorf("allocate firehose seq: %w", err)
		}
		seq = prevSeq + 1
		nowRFC := time.Now().UTC().Format(time.RFC3339)
		frameBlobs, err := frameBlobLinks(ops)
		if err != nil {
			return CommitResult{}, err
		}
		commitEvt := &comatproto.SyncSubscribeRepos_Commit{
			Seq:      seq,
			Repo:     did,
			Commit:   lexutil.LexLink(commitCID),
			Rev:      rev,
			Since:    sincePtr,
			PrevData: prevDataPtr,
			Blocks:   diffCAR,
			Ops:      frameOps,
			Blobs:    frameBlobs,
			Time:     nowRFC,
		}
		var fbuf bytes.Buffer
		if err := (&events.XRPCStreamEvent{RepoCommit: commitEvt}).Serialize(&fbuf); err != nil {
			return CommitResult{}, fmt.Errorf("serialize #commit: %w", err)
		}
		frameBytes = fbuf.Bytes()
	}

	for _, b := range newBlks {
		if _, err := tx.ExecContext(ctx,
			`INSERT OR IGNORE INTO repo_blocks(did, cid_keystring, block) VALUES(?,?,?)`,
			did, []byte(b.Cid().KeyString()), b.RawData()); err != nil {
			return CommitResult{}, fmt.Errorf("persist block: %w", err)
		}
	}
	if _, err := tx.ExecContext(ctx,
		`INSERT INTO repo_heads(did, rev, commit_cid, updated_at) VALUES(?,?,?,?)
		 ON CONFLICT(did) DO UPDATE SET rev=excluded.rev, commit_cid=excluded.commit_cid, updated_at=excluded.updated_at`,
		did, rev, commitCID.String(), now); err != nil {
		return CommitResult{}, fmt.Errorf("persist head: %w", err)
	}
	for _, e := range eff {
		switch e.action {
		case ActionCreate, ActionUpdate:
			if _, err := tx.ExecContext(ctx,
				`INSERT INTO records(did, path, record_cid, record_bytes) VALUES(?,?,?,?)
				 ON CONFLICT(did,path) DO UPDATE SET record_cid=excluded.record_cid, record_bytes=excluded.record_bytes`,
				did, e.path, e.recordCID, e.recordCBOR); err != nil {
				return CommitResult{}, fmt.Errorf("persist record: %w", err)
			}
			if e.faunaPostID != "" {
				if _, err := tx.ExecContext(ctx,
					`INSERT INTO post_map(did, fauna_post_id, at_uri, record_cid, root_uri, root_cid) VALUES(?,?,?,?,?,?)
					 ON CONFLICT(did,fauna_post_id) DO UPDATE SET at_uri=excluded.at_uri, record_cid=excluded.record_cid, root_uri=excluded.root_uri, root_cid=excluded.root_cid`,
					did, e.faunaPostID, e.atURI, e.recordCID, e.rootURI, e.rootCID); err != nil {
					return CommitResult{}, fmt.Errorf("persist post_map: %w", err)
				}
			}
		case ActionDelete:
			if _, err := tx.ExecContext(ctx, `DELETE FROM records WHERE did=? AND path=?`, did, e.path); err != nil {
				return CommitResult{}, fmt.Errorf("delete record: %w", err)
			}
			if e.faunaPostID != "" {
				if _, err := tx.ExecContext(ctx, `DELETE FROM post_map WHERE did=? AND fauna_post_id=?`, did, e.faunaPostID); err != nil {
					return CommitResult{}, fmt.Errorf("delete post_map: %w", err)
				}
			}
		}
	}
	if opts.deferFrameToSync {
		// No frame goes out, so the repo is now ahead of what the network has
		// been told. Record the debt in the SAME txn that moves the head: that
		// ordering is what survives a crash — the flag can never be set without
		// its commit, nor a commit go unannounced without the flag.
		if _, err := tx.ExecContext(ctx,
			`UPDATE repo_heads SET sync_owed = 1 WHERE did = ?`, did); err != nil {
			return CommitResult{}, fmt.Errorf("record owed #sync: %w", err)
		}
	} else if _, err := tx.ExecContext(ctx,
		`INSERT INTO firehose_events(seq, did, frame_type, payload, created_at) VALUES(?,?,?,?,?)`,
		seq, did, FrameCommit, frameBytes, now); err != nil {
		return CommitResult{}, fmt.Errorf("persist outbox: %w", err)
	}
	if err := tx.Commit(); err != nil {
		return CommitResult{}, fmt.Errorf("commit txn: %w", err)
	}
	// Nothing was appended to the outbox on the deferred path, so there is
	// nothing for the broadcaster to drain — waking it would be pure noise.
	if f.onCommit != nil && !opts.deferFrameToSync {
		f.onCommit()
	}

	return CommitResult{
		Rev:       rev,
		CommitCID: commitCID.String(),
		DataCID:   rootCID.String(),
		Seq:       seq,
		Ops:       len(frameOps),
		Frame:     frameBytes,
	}, nil
}

// RecordCID computes a record's CIDv1 from its canonical dag-cbor bytes.
//
// It is the SAME builder ApplyBatch uses, exported so the F2 write path can
// answer the XRPC caller `{uri, cid}` without a second encoder: there is
// deliberately no Rust dag-cbor/CID implementation kept byte-identical to
// indigo's (atproto-pds-full.md § F2 detail, *CID source*), and there must not
// be a second Go one either. Sharing the builder is what makes the CID the
// bridge answers with provably the CID the commit records.
func RecordCID(recordCBOR []byte) (string, error) {
	c, err := dagCBOR.Sum(recordCBOR)
	if err != nil {
		return "", fmt.Errorf("compute record cid: %w", err)
	}
	return c.String(), nil
}

// FaunaPostIDForRecord is post_map read in REVERSE: it resolves a record this
// repo serves back to the Fauna post it maps from, or ok=false when the record
// maps to no Fauna post (a journaled native record, or one never projected).
//
// The forward direction ([Store.PostAtURI]) serves the projection loop; this
// direction serves the F2 write path, where an external `deleteRecord` names a
// record by rkey and the nest needs the Fauna post id to tombstone. The rkey
// derivation is one-way, so this index is the only way back.
func (s *Store) FaunaPostIDForRecord(ctx context.Context, did, collection, rkey string) (faunaPostID string, ok bool, err error) {
	return s.FaunaPostIDForATURI(ctx, ATURI(did, collection, rkey))
}

// ATURI builds the canonical AT-URI of a record. One builder, because the same
// string is the post_map key, the nest's reply, and the resolution key on the
// write path — three spellings that must never drift apart.
func ATURI(did, collection, rkey string) string {
	return "at://" + did + "/" + collection + "/" + rkey
}

// FaunaPostIDForATURI is the CROSS-REPO form of [Store.FaunaPostIDForRecord]:
// it resolves any AT-URI, not just one in a repo we were handed the DID for.
//
// The write path needs this because replying to (or quoting) ANOTHER bridged
// actor is the ordinary case, not an edge one. The DID is read back out of the
// URI's own authority so the query still rides the (did, at_uri) index rather
// than forcing a scan.
//
// A URI whose authority is not a DID — a handle-authority AT-URI is legal in
// the wider ecosystem, though strongRefs carry DIDs in practice — resolves to
// ok=false rather than being resolved through an identity lookup. Degrading to
// "not a Fauna post" makes the write journal, which is the ratified fallback;
// guessing at a handle->DID mapping here would risk attributing a reply to the
// wrong account's post.
func (s *Store) FaunaPostIDForATURI(ctx context.Context, atURI string) (faunaPostID string, ok bool, err error) {
	const prefix = "at://"
	if !strings.HasPrefix(atURI, prefix) {
		return "", false, nil
	}
	authority, _, found := strings.Cut(atURI[len(prefix):], "/")
	if !found || !strings.HasPrefix(authority, "did:") {
		return "", false, nil
	}
	row := s.db.QueryRowContext(ctx,
		`SELECT fauna_post_id FROM post_map WHERE did=? AND at_uri=?`, authority, atURI)
	err = row.Scan(&faunaPostID)
	if errors.Is(err, sql.ErrNoRows) {
		return "", false, nil
	}
	if err != nil {
		return "", false, err
	}
	return faunaPostID, true, nil
}

// PostAtURI returns the AT-URI a Fauna post was projected to, or ok=false if it
// was never projected (or has been deleted). Used by the projection loop to
// resolve reply/quote parents and to find the record to delete.
func (s *Store) PostAtURI(ctx context.Context, did, faunaPostID string) (atURI, recordCID string, ok bool, err error) {
	row := s.db.QueryRowContext(ctx,
		`SELECT at_uri, record_cid FROM post_map WHERE did=? AND fauna_post_id=?`, did, faunaPostID)
	err = row.Scan(&atURI, &recordCID)
	if errors.Is(err, sql.ErrNoRows) {
		return "", "", false, nil
	}
	if err != nil {
		return "", "", false, err
	}
	return atURI, recordCID, true, nil
}

// ProjectedPost is a resolved post_map row: where a Fauna post was projected to,
// plus the thread root it anchors to. Root is empty for a post that is itself a
// thread root, which is what makes the ATProto reply rule inductive — see
// [Store.ProjectedPostAnyRepo].
type ProjectedPost struct {
	AtURI     string
	RecordCID string
	RootURI   string // "" when this post is itself the thread root
	RootCID   string
}

// ThreadRoot returns the (uri, cid) a *reply to this post* must carry as its
// `reply.root`: this post's own root when it has one, else this post itself.
// That is the ATProto rule, and evaluating it here — rather than at each call
// site — is what keeps a deep thread anchored to the true root instead of
// drifting to the parent.
func (p ProjectedPost) ThreadRoot() (uri, cid string) {
	if p.RootURI != "" {
		return p.RootURI, p.RootCID
	}
	return p.AtURI, p.RecordCID
}

// ProjectedPostAnyRepo resolves a Fauna post id to its projected record across
// EVERY served repo, or ok=false if no repo has projected it (or it has since
// been deleted).
//
// Deliberately not did-scoped: a reply to another bridged actor's post must
// resolve into THAT actor's repo, which is the ordinary case on the network. A
// Fauna post has exactly one author and is only ever projected into its author's
// repo, so the id identifies at most one row; LIMIT 1 makes that explicit rather
// than relying on it.
func (s *Store) ProjectedPostAnyRepo(ctx context.Context, faunaPostID string) (ProjectedPost, bool, error) {
	var p ProjectedPost
	row := s.db.QueryRowContext(ctx,
		`SELECT at_uri, record_cid, root_uri, root_cid FROM post_map WHERE fauna_post_id=? LIMIT 1`,
		faunaPostID)
	err := row.Scan(&p.AtURI, &p.RecordCID, &p.RootURI, &p.RootCID)
	if errors.Is(err, sql.ErrNoRows) {
		return ProjectedPost{}, false, nil
	}
	if err != nil {
		return ProjectedPost{}, false, err
	}
	return p, true, nil
}

// EmitIdentity writes an `#identity` frame to the outbox for did, announcing
// that its handle changed and the network must re-resolve the DID
// (atproto-pds-bridge.md § Identity — "A Fauna handle or domain change …
// updates the DID document's alsoKnownAs, and emits an #identity firehose
// event so the network re-resolves").
//
// It is the second producer beside ApplyBatch, and deliberately the same
// shape: allocate the PDS-global seq INSIDE the txn that writes the row (so no
// two frames can ever contend for a number, and a rolled-back txn burns none),
// then wake the broadcaster. The broadcaster itself is frame-type agnostic —
// it fans already-serialized payloads in seq order — so nothing downstream
// changes.
//
// Not a repo mutation: no commit, no MST, no blocks. It still takes the
// per-DID lock, so a rename frame can never interleave INTO the middle of a
// concurrent commit's seq allocation for the same repo, and consumers see the
// identity change in a stable position relative to that repo's commits.
func (f *Funnel) EmitIdentity(ctx context.Context, did, handle string) (int64, error) {
	if did == "" {
		return 0, errors.New("EmitIdentity: empty did")
	}
	if err := f.refuseIfFirstEmitGated(ctx, did); err != nil {
		return 0, err
	}
	unlock := f.lockDID(did)
	defer unlock()

	tx, err := f.store.db.BeginTx(ctx, nil)
	if err != nil {
		return 0, fmt.Errorf("begin identity txn: %w", err)
	}
	defer tx.Rollback()

	prevSeq, err := maxSeq(ctx, tx)
	if err != nil {
		return 0, fmt.Errorf("allocate firehose seq: %w", err)
	}
	seq := prevSeq + 1
	h := handle
	evt := &comatproto.SyncSubscribeRepos_Identity{
		Seq:    seq,
		Did:    did,
		Handle: &h,
		Time:   time.Now().UTC().Format(time.RFC3339),
	}
	var fbuf bytes.Buffer
	if err := (&events.XRPCStreamEvent{RepoIdentity: evt}).Serialize(&fbuf); err != nil {
		return 0, fmt.Errorf("serialize #identity: %w", err)
	}
	if _, err := tx.ExecContext(ctx,
		`INSERT INTO firehose_events(seq, did, frame_type, payload, created_at) VALUES(?,?,?,?,?)`,
		seq, did, FrameIdentity, fbuf.Bytes(), time.Now().UnixMicro()); err != nil {
		return 0, fmt.Errorf("persist identity outbox row: %w", err)
	}
	if err := tx.Commit(); err != nil {
		return 0, fmt.Errorf("commit identity txn: %w", err)
	}
	if f.onCommit != nil {
		f.onCommit()
	}
	return seq, nil
}

// EmitSync writes one `#sync` frame to the outbox for did, announcing its
// current authoritative head, and clears the repo's [Store.SyncOwed] debt in the
// same transaction. It is the second half of the downtime-catch-up collapse
// (atproto-pds-bridge.md § Projection & backfill, watermark row 3): a gap too
// large to replay as individual `#commit`s is applied with [DeferFrameToSync]
// and announced once here, after which the consumer refetches with getRepo.
//
// A repo with no head is a no-op returning seq 0 — the crash-recovery case
// where the debt outlived every commit it was owed for. Announcing a head that
// does not exist is the one thing worse than announcing nothing.
//
// Same shape and contract as EmitIdentity/EmitAccount — a NON-mutating producer
// (no commit, no MST, no blocks) allocating the PDS-global seq INSIDE the txn
// writing the row, under the per-DID lock. The head is read BEFORE the txn
// opens, deliberately: the store holds a single connection, so a read issued
// through *sql.DB while a txn is open on it would deadlock, and the per-DID lock
// already makes that read consistent with the write.
func (f *Funnel) EmitSync(ctx context.Context, did string) (int64, error) {
	if did == "" {
		return 0, errors.New("EmitSync: empty did")
	}
	if err := f.refuseIfFirstEmitGated(ctx, did); err != nil {
		return 0, err
	}
	unlock := f.lockDID(did)
	defer unlock()

	car, rev, ok, err := f.store.CommitCAR(ctx, did)
	if err != nil {
		return 0, fmt.Errorf("read head for #sync: %w", err)
	}
	if !ok {
		return 0, nil // nothing has ever been committed for this repo
	}

	tx, err := f.store.db.BeginTx(ctx, nil)
	if err != nil {
		return 0, fmt.Errorf("begin sync txn: %w", err)
	}
	defer tx.Rollback()

	prevSeq, err := maxSeq(ctx, tx)
	if err != nil {
		return 0, fmt.Errorf("allocate firehose seq: %w", err)
	}
	seq := prevSeq + 1
	evt := &comatproto.SyncSubscribeRepos_Sync{
		Seq:    seq,
		Did:    did,
		Rev:    rev,
		Blocks: car,
		Time:   time.Now().UTC().Format(time.RFC3339),
	}
	var fbuf bytes.Buffer
	if err := (&events.XRPCStreamEvent{RepoSync: evt}).Serialize(&fbuf); err != nil {
		return 0, fmt.Errorf("serialize #sync: %w", err)
	}
	if _, err := tx.ExecContext(ctx,
		`INSERT INTO firehose_events(seq, did, frame_type, payload, created_at) VALUES(?,?,?,?,?)`,
		seq, did, FrameSync, fbuf.Bytes(), time.Now().UnixMicro()); err != nil {
		return 0, fmt.Errorf("persist sync outbox row: %w", err)
	}
	if _, err := tx.ExecContext(ctx,
		`UPDATE repo_heads SET sync_owed = 0 WHERE did = ?`, did); err != nil {
		return 0, fmt.Errorf("clear owed #sync: %w", err)
	}
	if err := tx.Commit(); err != nil {
		return 0, fmt.Errorf("commit sync txn: %w", err)
	}
	if f.onCommit != nil {
		f.onCommit()
	}
	return seq, nil
}

// EmitAccount writes an `#account` frame to the outbox for did, announcing that
// its hosting status changed. Two callers, two meanings, one frame producer:
// layer-2 deactivation/re-entry passes [AccountStatusDeactivated] with
// active=false and "" with active=true (atproto-pds-bridge.md § Disable &
// revocation, layer 2 — "the repo is no longer served, an #account (deactivated)
// firehose event is emitted … re-enabling restores the same identity"), and the
// delete-presence sweep passes [AccountStatusDeleted] as its terminal
// announcement (layer 2's "separate, stronger action").
//
// The Sync v1.1 semantics put the reason in `status` only when active=false — a
// bare active=true reactivation needs no reason — so an active=true call with a
// non-empty status is a caller bug and is refused rather than serialized: a
// relay keys its account bookkeeping off exactly this pair, and a frame saying
// "active, and by the way deleted" has no honest reading.
//
// Same shape and contract as EmitIdentity/EmitSync — a NON-mutating producer (no
// commit, no MST, no blocks) that allocates the PDS-global seq INSIDE the txn
// writing the row, under the per-DID lock, so the status change can never
// interleave into a concurrent commit's seq allocation and consumers see it in a
// stable position relative to that repo's commits. The broadcaster is frame-type
// agnostic, so nothing downstream changes.
func (f *Funnel) EmitAccount(ctx context.Context, did string, active bool, status string) (int64, error) {
	if did == "" {
		return 0, errors.New("EmitAccount: empty did")
	}
	if active && status != "" {
		return 0, fmt.Errorf("EmitAccount: active=true carries no status, got %q", status)
	}
	if !active && status == "" {
		return 0, errors.New("EmitAccount: active=false needs a status")
	}
	if err := f.refuseIfFirstEmitGated(ctx, did); err != nil {
		return 0, err
	}
	unlock := f.lockDID(did)
	defer unlock()

	tx, err := f.store.db.BeginTx(ctx, nil)
	if err != nil {
		return 0, fmt.Errorf("begin account txn: %w", err)
	}
	defer tx.Rollback()

	prevSeq, err := maxSeq(ctx, tx)
	if err != nil {
		return 0, fmt.Errorf("allocate firehose seq: %w", err)
	}
	seq := prevSeq + 1
	evt := &comatproto.SyncSubscribeRepos_Account{
		Seq:    seq,
		Did:    did,
		Active: active,
		Time:   time.Now().UTC().Format(time.RFC3339),
	}
	if !active {
		evt.Status = &status
	}
	var fbuf bytes.Buffer
	if err := (&events.XRPCStreamEvent{RepoAccount: evt}).Serialize(&fbuf); err != nil {
		return 0, fmt.Errorf("serialize #account: %w", err)
	}
	if _, err := tx.ExecContext(ctx,
		`INSERT INTO firehose_events(seq, did, frame_type, payload, created_at) VALUES(?,?,?,?,?)`,
		seq, did, FrameAccount, fbuf.Bytes(), time.Now().UnixMicro()); err != nil {
		return 0, fmt.Errorf("persist account outbox row: %w", err)
	}
	if err := tx.Commit(); err != nil {
		return 0, fmt.Errorf("commit account txn: %w", err)
	}
	if f.onCommit != nil {
		f.onCommit()
	}
	return seq, nil
}
