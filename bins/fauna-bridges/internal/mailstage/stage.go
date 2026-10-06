// Package mailstage holds the one decision — shared by every mail-bridge leg
// that ships an already-*sealed* body to nest — of whether that body rides the
// WS-RPC frame inline or crosses on the bulk-byte plane by reference.
//
// It is deliberately its own package so both the MTA (inbound MX + authenticated
// submission, internal/mta) and the MDA (IMAP APPEND, internal/mda/imap) call
// the *same* StageSealedBody rather than each keeping its own copy: the
// switchover predicate and the chunk boundaries must be bit-for-bit identical
// across every producer, or mail corrupts silently (smtp-server.md § Message
// size limits). The domain-critical pieces (MailBodyNeedsReference,
// SplitSealedMailBody) are shared Rust reached through mailfauna; this package
// is the thin Go orchestration that mints, splits, and uploads around them.
//
// There is no upper size ceiling here since ceiling retirement (2026-07-18): a
// sealed body of any size stages, and nest rests it as frame-sized continuation
// records. The product ceiling `max_message_bytes` is enforced at each leg's own
// perimeter (the SMTP `Data` io.LimitReader clamp; nest's APPEND admission).
//
// The plaintext-derived staging (client import / the outbound queue pair, which
// AEAD-envelope raw bytes under a one-shot key) is a *different* rule and stays
// with its own producers — this package is sealed-bytes only.
package mailstage

import (
	"context"
	"encoding/hex"
	"errors"
	"fmt"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/byteplane"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// ErrMessageTooLarge is the permanent "this message can never be delivered, at
// any retry, because of its size" class. The SMTP `Data` paths map it — and only
// it — to a permanent `552 5.3.4`, never the transient `451`
// (smtp-server.md § Message size limits). The IMAP APPEND path maps it to a
// `BAD` (a client upload the MUA must not retry).
//
// The distinction matters: before 2026-07-12 an oversized message surfaced as a
// generic `451`, so senders retried a doomed delivery for days. A size failure is
// a property of the message, not of the nest's health.
var ErrMessageTooLarge = errors.New("message exceeds a permanent size limit")

// ErrOverInlineBudget: the sealed *index hint* alone exceeds the inline WS-RPC
// request budget. The hint always rides inline — only the body can go by
// reference — so no amount of staging saves this request from the 2 MiB frame.
//
// The hint is input-dependent (a unique-word-dense body grows it toward the
// body's own size), which is why this is reachable at all. It is now the only
// residual inline-budget failure: an over-budget *body* is staged, not refused.
var ErrOverInlineBudget = fmt.Errorf(
	"%w: sealed index hint alone exceeds the inline WS-RPC request budget", ErrMessageTooLarge)

// StageSealedBody decides how a sealed mail body crosses to nest, staging it on
// the bulk-byte plane when it cannot ride the RPC inline, and returns the two
// mutually-exclusive request fields:
//
//   - (body, nil)  → the body fits the frame; it rides inline, byte-for-byte as
//     it always has, and the request encodes identically to its pre-reference
//     shape.
//   - (nil, ref)   → the sealed bytes are staged as content-addressed chunks and
//     the request carries only their hashes.
//
// Nest binds the staged bytes into the mail record atomically with the RPC, and
// resolves them from its own local blob store — so the at-rest shape, the seal,
// and the quota charge are bit-for-bit identical either way. Only the transport
// moves (smtp-server.md § Message size limits).
//
// Staging needs no lifecycle of its own: a staged-but-unconsumed chunk is an
// orphan blob, and the existing blob GC already reaps orphans past its grace
// window while skipping fresh ones. That *is* the "unconsumed staging is TTL-GC'd"
// the design calls for — for free, with no staging table and nothing to leak.
//
// Every leg that ships already-sealed bytes shares this: the inbound MX path and
// the authenticated submission / Sent-copy path (both internal/mta), and the
// IMAP APPEND path (internal/mda/imap). They must, because the perimeter now
// admits messages far larger than the frame — a leg without a reference path
// would seal a multi-megabyte message and then have the transport sever the
// connection under it.
func StageSealedBody(
	ctx context.Context,
	caller wsrpc.Caller,
	plane *byteplane.Client,
	actorID []byte,
	sealedBody []byte,
	sealedHint []byte,
) (inlineBody []byte, ref *wsrpc.MailBodyRef, err error) {
	// The single switchover predicate, shared with nest and every other leg. It
	// counts the hint too — a body comfortably under the budget can still
	// assemble a request the frame refuses. There is no upper size ceiling: a
	// body of any size stages, and nest rests it as continuation records (the
	// product ceiling is enforced at each leg's own perimeter, not here — see
	// the package doc).
	if !mailfauna.MailBodyNeedsReference(uint64(len(sealedBody)), uint64(len(sealedHint))) {
		return sealedBody, nil, nil
	}
	// Only the body can leave the frame. A hint that alone overflows the budget
	// is unsendable no matter what we stage.
	if uint64(len(sealedHint)) > uint64(mailfauna.InlineMailRequestBudgetBytes()) {
		return nil, nil, ErrOverInlineBudget
	}
	if plane == nil {
		// Unwired byte plane ⇒ transient, not permanent: the message is fine, our
		// deployment is not. A 451 lets the sender retry once we are wired.
		return nil, nil, errors.New("no byte-plane client wired; cannot stage an oversized sealed body")
	}

	// A `MailBody` token, not a folder one: mail belongs to no folder, and an
	// MTA may not mint a folder token (admitting it to the byte plane for mail
	// did not open the WebDAV path to it). Nest gates this on `actorID` being a
	// mail recipient here. The MDA is admitted the same way (BridgeMta | BridgeMda).
	token, _, err := wsrpc.MintMailBodyByteToken(ctx, caller, actorID)
	if err != nil {
		return nil, nil, fmt.Errorf("mint mail-body bulk-byte token: %w", err)
	}

	// The chunk split is shared Rust (`fauna_mail::body_ref`), never re-derived
	// here: the producer that stages, the nest that rejoins, and the MDA that
	// re-fetches must agree byte-for-byte on where a boundary falls and which
	// hash keys a chunk. A disagreement would corrupt mail silently.
	chunks := mailfauna.SplitSealedMailBody(sealedBody)
	hashes := make([][]byte, 0, len(chunks))
	for i, c := range chunks {
		// Content-addressed ⇒ the upload is idempotent, and a chunk already
		// present (a retried delivery, a duplicate recipient) costs nothing.
		if err := plane.UploadChunk(ctx, token, c.Bytes, hex.EncodeToString(c.Hash)); err != nil {
			return nil, nil, fmt.Errorf("stage sealed body chunk %d of %d: %w", i+1, len(chunks), err)
		}
		hashes = append(hashes, c.Hash)
	}
	return nil, &wsrpc.MailBodyRef{
		ChunkHashes: hashes,
		TotalBytes:  uint64(len(sealedBody)),
	}, nil
}
