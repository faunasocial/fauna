package imap

import (
	"context"
	"encoding/hex"
	"fmt"
	"log/slog"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/byteplane"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// ⚠ **Naming, read this first.** "Index segment" means two unrelated things in
// this package during the S5 rollout:
//
//   - `wsrpc.IndexSegment` — the OLD per-message sealed *hint*, one per message,
//     which `bodySearch` opens and tokenizes one at a time. Nothing here.
//   - a **content-index segment** — an immutable sealed tantivy segment covering
//     many messages, published on the `__index` rail. That is what this file
//     moves, and what `contentIndexRail` below is the transport for.
//
// Do not merge the two, and do not name a new thing `IndexSegment`.
//
// # What this file is
//
// The Go implementation of `faunaFfi.FfiIndexRail`, the foreign trait the
// shared Rust builder is driven over (`libs/fauna-ffi/src/content_index_session.rs`).
// Every byte crossing it is already AEAD-sealed under a key no nest holds and
// this process never derives — the rail moves opaque bytes and holds no key
// material, which is why implementing it needs nothing from the MSEK.
//
// Index *orchestration* stays in shared Rust where the client builder leg
// proves it (priority #2). This file is transport and nothing else — and
// **read-only transport** since the 2026-08-10 carrier ruling
// (`content-index.md` § Where the index is built): the MDA's build half is
// retired, because it never held the mail kind's ratified content id (the RFC
// `Message-ID`) and every doc it staged duplicated a client-built one under a
// spelling no other leg could see. What remains is one WS-RPC kind
// (`fauna.bridges.index_list`) and the blob GET route; `SEARCH` coverage is
// decided Rust-side against each doc's stored *secondary* identity — the nest
// message id, exactly the spelling `indexDocID` below renders.

// indexRailRPCTimeout bounds one rail call. Generous relative to a healthy nest
// (a segment PUT is at most the one-blob ceiling) and far below any MUA-visible
// deadline, because a rail failure degrades this session to the old `SEARCH`
// path rather than failing anything the user asked for.
const indexRailRPCTimeout = 30 * time.Second

// contentIndexRail is one MUA session's `__index` transport.
//
// Constructed at AUTH and handed to the Rust session, which calls it
// synchronously from its own runtime — the trait is sync on purpose (a foreign
// impl blocks either way), so blocking here is expected and correct.
type contentIndexRail struct {
	client  wsrpc.Caller
	plane   *byteplane.Client
	actorID []byte
	logger  *slog.Logger
}

func newContentIndexRail(
	client wsrpc.Caller,
	plane *byteplane.Client,
	actorID []byte,
	logger *slog.Logger,
) *contentIndexRail {
	return &contentIndexRail{
		client:  client,
		plane:   plane,
		actorID: actorID,
		logger:  logger,
	}
}

// ListEntries implements faunaFfi.FfiIndexRail.
//
// The nest filters this to mail/calendar-class paths on its side, so a
// master-class segment is not merely unopenable here — it never arrives. That is
// deliberate: withholding it keeps the user's per-kind segment counts away from a
// MUA-credential-reachable position.
func (r *contentIndexRail) ListEntries() ([]faunaFfi.FfiIndexRailEntry, error) {
	ctx, cancel := context.WithTimeout(context.Background(), indexRailRPCTimeout)
	defer cancel()
	entries, err := wsrpc.BridgeIndexList(ctx, r.client, r.actorID)
	if err != nil {
		return nil, railError(err)
	}
	out := make([]faunaFfi.FfiIndexRailEntry, 0, len(entries))
	for _, e := range entries {
		out = append(out, faunaFfi.FfiIndexRailEntry{
			Path:      e.Path,
			BlobHash:  e.BlobHash,
			SizeBytes: uint64(e.SizeBytes),
		})
	}
	return out, nil
}

// FetchBlob implements faunaFfi.FfiIndexRail.
//
// Takes the **hex** digest the listing hands out (not the base32 CID the upload
// used — the same bytes are addressable both ways and callers never convert).
// The download route is open, so no token: the bytes are sealed under a key no
// nest holds, and there is no confidentiality resting on the request being
// authenticated.
func (r *contentIndexRail) FetchBlob(blobHash string) ([]byte, error) {
	if r.plane == nil {
		return nil, railError(fmt.Errorf("fetch index blob %s: no byte-plane client wired", blobHash))
	}
	ctx, cancel := context.WithTimeout(context.Background(), indexRailRPCTimeout)
	defer cancel()
	bytes, err := r.plane.DownloadBlob(ctx, blobHash)
	if err != nil {
		return nil, railError(fmt.Errorf("fetch index blob %s: %w", blobHash, err))
	}
	return bytes, nil
}

// railError is how a rail failure crosses back into Rust.
//
// The generated wrapper lowers only a *faunaFfi.FfiError. Any other Go error
// crosses as an unexpected-callback error with no payload, which the Rust side
// can only report as a General error with an empty reason — so a SEARCH that
// fell back to the hint scan logged nothing about why.
func railError(err error) error {
	return faunaFfi.NewFfiErrorGeneral(err.Error())
}

// resumeIndexSession mints this session's content-index handle, or returns nil
// with the reason logged.
//
// **Never fails AUTH.** Every failure mode here — a rail read that times
// out or a snapshot this build cannot parse — is a degradation to the pre-backend-2 `SEARCH` path, which is
// what `content-index.md` § Where the index is built prescribes and what the
// client leg does with a failed builder launch. Returning an error instead would
// let a transient nest blip lock a user out of their mailbox.
//
// `cap` is the session's MlsCapability: the session is constructed *from* it so
// the MSEK is derived Rust-side and never crosses the boundary.
func (s *Session) resumeIndexSession(
	cap *faunaFfi.MlsCapability,
	snapshotPlaintext []byte,
	actorID []byte,
) *faunaFfi.FfiMailIndexSession {
	if s.plane == nil {
		// No byte plane wired ⇒ segment blobs cannot be fetched, so the reader
		// could never open the slice.
		if s.logger != nil {
			s.logger.Debug("imap: content index disabled — no byte-plane client wired")
		}
		return nil
	}
	rail := newContentIndexRail(s.client, s.plane, actorID, s.logger)
	session, err := cap.ResumeMailIndexSession(snapshotPlaintext, rail)
	if err != nil {
		if s.logger != nil {
			s.logger.Info(
				"imap: content-index session unavailable; SEARCH falls back to the hint scan",
				"err", err,
			)
		}
		return nil
	}
	if s.logger != nil {
		s.logger.Debug("imap: content-index session resumed (query-only)")
	}
	return session
}

// indexDocID renders a nest message id as a coverage candidate.
//
// **Hex, and it must be, because the id is BINARY.** `wsrpc.IndexSegment`'s
// `MessageID` is a raw 32-byte nest message id, not an RFC `Message-ID` string
// — and every id-shaped field crossing the UniFFI boundary is a `String`, whose
// Go→Rust converter **panics** on invalid UTF-8 rather than returning an error.
// Passing `string(seg.MessageID)` therefore killed the IMAP connection on the
// first real message. Go unit tests never saw it (a Go string happily holds
// arbitrary bytes; only the FFI conversion objects), which is why the tier_3
// inbound test is what caught it.
//
// **Hex of the nest id is the sanctioned coverage spelling since the
// 2026-08-10 carrier ruling** (`content-index.md` § Where the index is built):
// the Rust side decides coverage against each doc's stored *secondary*
// identity — the same nest id, stamped by the client leg at ingest — so this
// is no longer compared against the RFC `Message-ID` content id at all. This
// process stages nothing (the MDA build half is retired there too), so the
// only consumer is the coverage ask below.
func indexDocID(messageID []byte) string {
	return hex.EncodeToString(messageID)
}

// indexAnswerFunc is what the content index can say about one `SEARCH`'s
// candidate messages: which of them the published slice covers, and which of
// those match the terms.
//
// Two sets, not one, because "did not match" and "cannot say" are different
// answers: a candidate absent from `covered` still owes its hint an open, while
// one in `covered` but not in `matched` has been answered *no* and must not be
// scanned again. Collapsing them would either lose mail or waste the leg.
type indexAnswerFunc func(terms []string, candidates []string) (covered map[string]bool, matched map[string]bool, err error)

// indexAnswerer adapts the FFI session to that seam, or returns nil when there
// is no index session (a normal state — see Session.indexSession).
//
// One FFI call per `SEARCH`, not one per message: coverage and matching must be
// decided against the *same* opened slice, and the shared side re-opens it only
// when the published manifest has moved.
//
// ⚠ Coverage is the **queryable** slice, decided Rust-side against each doc's
// stored secondary identity — never a staging guard's view (this process no
// longer stages anything): only what an opened reader can actually answer for
// may skip its hint's open, or a message could vanish from `SEARCH` with no
// error on any path.
func (s *Session) indexAnswerer(
	session *faunaFfi.FfiMailIndexSession,
) indexAnswerFunc {
	if session == nil {
		return nil
	}
	return func(terms []string, candidates []string) (map[string]bool, map[string]bool, error) {
		// The caller keys everything by the RAW message id (so does uidByMid);
		// only the FFI hop speaks hex. Encoding here rather than at the call
		// site keeps the whole encoding question inside this adapter — see
		// indexDocID for why it is not optional.
		rawByHex := make(map[string]string, len(candidates))
		encoded := make([]string, 0, len(candidates))
		for _, raw := range candidates {
			h := indexDocID([]byte(raw))
			rawByHex[h] = raw
			encoded = append(encoded, h)
		}
		reply, err := session.AnswerBodySearch(terms, encoded)
		if err != nil {
			if s.logger != nil {
				// Info, not debug: a persistent failure here means every SEARCH
				// is paying the full decrypt pass the index exists to avoid.
				s.logger.Info("imap: content-index search failed, scanning instead",
					"candidates", len(candidates), "err", err)
			}
			return nil, nil, err
		}
		toRaw := func(hexes []string) map[string]bool {
			out := make(map[string]bool, len(hexes))
			for _, h := range hexes {
				if raw, ok := rawByHex[h]; ok {
					out[raw] = true
				}
			}
			return out
		}
		return toRaw(reply.Covered), toRaw(reply.Matched), nil
	}
}
