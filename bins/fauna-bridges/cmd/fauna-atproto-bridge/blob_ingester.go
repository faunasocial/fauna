package main

// The production atprotopds.BlobIngester: land an inbound uploadBlob on both
// sides of the CID divide — the Fauna media path (BLAKE3-addressed) and the
// bridge's own ATProto blob store (sha256-addressed).
//
// Same placement rationale as funnelRepoWriter / sealedRepoSigners: the sidecar
// encoding is a cgo FFI call and the nest HTTP custody is main's business, so
// internal/atprotopds stays cgo-free behind the seam.
//
// The byte leg deliberately reuses the SAME nest HTTP client the enroll/auth path
// and the outbound blob source already use (loopback-aware TLS, single-sourced in
// wsrpc.NestHTTPClient) — a second client here would be a second place for that
// decision to drift.

import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"mime/multipart"
	"net/http"
	"strings"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotorepo"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

type nestBlobIngester struct {
	client   *http.Client
	endpoint string
	// token mints the bearer for POST /api/v1/blob. Unlike the GET byte route
	// (unauthenticated and CID-verified), the upload direction is authed — as
	// the BRIDGE's service user, which is exactly why the account attribution
	// cannot ride on this leg. See RecordBlobMapping.
	token   func(context.Context) (string, error)
	nest    wsrpc.Caller
	store   *atprotorepo.Store
	sidecar func(mime string) []byte
}

func newNestBlobIngester(
	client *http.Client,
	endpoint string,
	token func(context.Context) (string, error),
	nest wsrpc.Caller,
	store *atprotorepo.Store,
) *nestBlobIngester {
	return &nestBlobIngester{
		client:   client,
		endpoint: endpoint,
		token:    token,
		nest:     nest,
		store:    store,
		// The sidecar bytes come from shared Rust, never hand-rolled here: they
		// are strict-decoded nest-side and then cross-checked against the body by
		// the per-class envelope verifier, so a Go copy drifting on canonical map
		// ordering, the AudienceClass spelling, or how an absent thumbnail_hash
		// encodes would fail with an opaque 400. Same reason
		// appview_service_did() crosses the FFI rather than being copied.
		sidecar: faunaFfi.AtprotoPublicPostUploadSidecar,
	}
}

// blobUploadReply is the nest's answer to POST /api/v1/blob: the blake3 digest,
// hex — the Fauna ContentHash those bytes now live under.
type blobUploadReply struct {
	Hash string `json:"hash"`
}

func (n *nestBlobIngester) StoreInFaunaMedia(ctx context.Context, mime string, data []byte) ([]byte, error) {
	if n.endpoint == "" {
		return nil, fmt.Errorf("no nest endpoint configured for blob upload")
	}

	var body bytes.Buffer
	mw := multipart.NewWriter(&body)
	// Two parts, named exactly as the nest's parse_multipart_upload expects, in
	// either order: `sidecar` (canonical dag-cbor UploadSidecar) and `bytes`.
	part, err := mw.CreateFormField("sidecar")
	if err != nil {
		return nil, fmt.Errorf("build sidecar part: %w", err)
	}
	if _, err := part.Write(n.sidecar(mime)); err != nil {
		return nil, fmt.Errorf("write sidecar part: %w", err)
	}
	part, err = mw.CreateFormField("bytes")
	if err != nil {
		return nil, fmt.Errorf("build bytes part: %w", err)
	}
	if _, err := part.Write(data); err != nil {
		return nil, fmt.Errorf("write bytes part: %w", err)
	}
	if err := mw.Close(); err != nil {
		return nil, fmt.Errorf("close multipart: %w", err)
	}

	url := strings.TrimSuffix(n.endpoint, "/") + "/api/v1/blob"
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(body.Bytes()))
	if err != nil {
		return nil, fmt.Errorf("build blob upload request: %w", err)
	}
	req.Header.Set("Content-Type", mw.FormDataContentType())
	token, err := n.token(ctx)
	if err != nil {
		return nil, fmt.Errorf("bearer for blob upload: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+token)

	resp, err := n.client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("upload blob to nest: %w", err)
	}
	defer resp.Body.Close()
	// The nest answers 201 Created (blob_routes.rs) — a demand for exactly 200
	// here once turned every successful upload into a 500, invisible to both
	// sides' unit tests until the first real-nest tier_3 run (2026-07-30; the
	// cross-binary-agreement class of F2.2 4d). Pinned against 201 by
	// TestStoreInFaunaMediaAcceptsTheNests201.
	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusCreated {
		// Bounded: an error body is small, and a hostile one must not be
		// buffered unbounded just to be logged.
		detail, _ := io.ReadAll(io.LimitReader(resp.Body, 512))
		return nil, fmt.Errorf("upload blob to nest: %s: %s",
			resp.Status, strings.TrimSpace(string(detail)))
	}
	var reply blobUploadReply
	if err := json.NewDecoder(io.LimitReader(resp.Body, 4096)).Decode(&reply); err != nil {
		return nil, fmt.Errorf("decode blob upload reply: %w", err)
	}
	mediaRef, err := hex.DecodeString(reply.Hash)
	if err != nil {
		return nil, fmt.Errorf("blob upload reply hash is not hex: %w", err)
	}
	if len(mediaRef) != 32 {
		return nil, fmt.Errorf("blob upload reply hash is %d bytes, want a 32-byte blake3 digest",
			len(mediaRef))
	}
	return mediaRef, nil
}

func (n *nestBlobIngester) RecordBlobMapping(
	ctx context.Context, actorID []byte, cid string, mediaRef []byte,
) (string, error) {
	return wsrpc.RecordBlob(ctx, n.nest, actorID, cid, mediaRef)
}

func (n *nestBlobIngester) ServeBlobBytes(
	ctx context.Context, did, cid, faunaCID, mime string, data []byte,
) error {
	return n.store.PutBlob(ctx, did, cid, faunaCID, mime, data)
}
