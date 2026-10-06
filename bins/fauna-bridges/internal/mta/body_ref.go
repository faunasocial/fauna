package mta

import (
	"context"
	"encoding/hex"
	"errors"
	"fmt"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/byteplane"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// The sealed-body staging decision (StageSealedBody + its size-error sentinels)
// lives in internal/mailstage, shared with the MDA's IMAP APPEND leg: the
// switchover predicate and chunk boundaries must be identical across every
// producer of already-sealed bytes, or mail corrupts silently
// (smtp-server.md § Message size limits). This file keeps only the
// plaintext-derived outbound staging, which is MTA-only.

// stageOutboundEnvelope stages a plaintext outbound body on the bulk-byte
// plane under a one-shot AEAD envelope, returning the staged-envelope reference
// the enqueue RPC carries in place of the inline body (the staged-envelope rule,
// smtp-server.md § Message size limits; ratified 2026-07-18).
//
// It is the plaintext-derived sibling of mailstage.StageSealedBody, and MUST NOT
// reuse that path: the body here is PLAINTEXT, and MailBodyRef's open
// chunk-download route is safe only because everything in that store is
// ciphertext — staging raw plaintext would disclose it to anyone holding the
// hash and open a blake3(plaintext) correlation channel. So the producer
// AEAD-seals the whole plaintext under a FRESH one-shot key first
// (SealStagedBody), splits the CIPHERTEXT with the same body_ref chunk rule the
// nest rejoins with, uploads the chunks, and sends the reference WITH the key.
// The key rides inside the already-confidential authenticated WS-RPC — the very
// channel that otherwise carries the plaintext inline — so no party learns
// anything it could not already read, and the store never sees plaintext or a
// stable content hash (fresh key ⇒ unique ciphertext ⇒ no cross-staging
// correlation).
//
// The at-rest ceiling is NOT re-checked here: the outbound body already passed
// the perimeter's pre-parse max_message_bytes clamp at the DATA read (the
// product ceiling), so the 552 that ceiling raises fires before this leg is
// reached — this leg only decides inline-vs-staged transport for a body the
// perimeter already admitted. A nil plane is transient (our deployment lacks
// HTTP reach to nest, not the message's fault) → the caller answers 451.
func stageOutboundEnvelope(
	ctx context.Context,
	caller wsrpc.Caller,
	plane *byteplane.Client,
	actorID []byte,
	rawBody []byte,
) (*wsrpc.StagedBodyRef, error) {
	if plane == nil {
		return nil, errors.New("no byte-plane client wired; cannot stage an oversized outbound body")
	}

	// A `MailBody` token, minted for the AUTH'd submitter's actor (nest gates the
	// mint on actorID being a mail recipient), exactly as the inbound staging leg.
	token, _, err := wsrpc.MintMailBodyByteToken(ctx, caller, actorID)
	if err != nil {
		return nil, fmt.Errorf("mint mail-body bulk-byte token: %w", err)
	}

	// Seal the plaintext under a fresh one-shot key, THEN split the ciphertext
	// with the shared body_ref chunker (never re-derived in Go — nest rejoins
	// with the same implementation; priority #2).
	seal := mailfauna.SealStagedBody(rawBody)
	chunks := mailfauna.SplitSealedMailBody(seal.Sealed)
	hashes := make([][]byte, 0, len(chunks))
	for i, c := range chunks {
		// Content-addressed ⇒ the upload is idempotent (a retried submission or a
		// duplicated chunk costs nothing).
		if err := plane.UploadChunk(ctx, token, c.Bytes, hex.EncodeToString(c.Hash)); err != nil {
			return nil, fmt.Errorf("stage outbound body chunk %d of %d: %w", i+1, len(chunks), err)
		}
		hashes = append(hashes, c.Hash)
	}
	return &wsrpc.StagedBodyRef{
		ChunkHashes: hashes,
		TotalBytes:  uint64(len(seal.Sealed)),
		Key:         seal.Key,
	}, nil
}
