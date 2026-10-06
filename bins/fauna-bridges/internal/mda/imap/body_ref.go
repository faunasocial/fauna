package imap

import (
	"context"
	"encoding/hex"
	"fmt"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/byteplane"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// SealedBodyOf returns the sealed bytes of a fetched message, whichever way they
// arrived: inline in the reply, or by reference off the bulk-byte plane when the
// body was too large for the 2 MiB WS-RPC frame
// (smtp-server.md § Message size limits).
//
// Every consumer of a fetch reply must go through this rather than reading
// `ct.EncryptedBody` directly — that field is empty exactly when the body rode by
// reference, and reading it raw would hand the opener zero bytes and surface as a
// decrypt failure on a message that is perfectly intact.
//
// The download route is OPEN (ciphertext-by-hash, no token): confidentiality here
// is cryptographic, not transport-scoped — the bytes are HPKE-sealed to the
// recipient and the key never leaves their capability. So the read leg needs no
// mint, unlike the staging leg that produced it.
//
// Fails closed. The chunk *contents* are self-verifying — each is fetched by its
// own blake3 hash from a content-addressed store, so the bytes cannot be other
// than the bytes that hash names — but the chunk *list* is not: a reference that
// named the wrong chunks, named them out of order, or dropped one would otherwise
// yield a body that unseals to the wrong message, or to nothing. The declared
// total is the cheap end-to-end check that catches all three, and it is the same
// check the nest applies on the way up.
func SealedBodyOf(
	ctx context.Context,
	plane *byteplane.Client,
	ct *wsrpc.FetchedCiphertext,
) ([]byte, error) {
	if ct.BodyRef == nil {
		return ct.EncryptedBody, nil
	}
	if plane == nil {
		return nil, fmt.Errorf(
			"message body rides the bulk-byte plane but no byte-plane client is wired")
	}
	ref := ct.BodyRef
	chunks := make([][]byte, 0, len(ref.ChunkHashes))
	for i, h := range ref.ChunkHashes {
		b, err := plane.DownloadChunk(ctx, hex.EncodeToString(h))
		if err != nil {
			return nil, fmt.Errorf("fetch sealed body chunk %d of %d: %w", i+1, len(ref.ChunkHashes), err)
		}
		chunks = append(chunks, b)
	}
	// The join is shared Rust — the same one the MTA split with and the nest
	// rejoins with. Never re-derive it here (priority #2).
	joined := mailfauna.JoinSealedMailBody(chunks)
	if uint64(len(joined)) != ref.TotalBytes {
		return nil, fmt.Errorf(
			"body reference declared %d bytes but its chunks joined to %d",
			ref.TotalBytes, len(joined))
	}
	return joined, nil
}
