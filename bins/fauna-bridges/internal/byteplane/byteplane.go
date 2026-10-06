// Package byteplane is the shared HTTP client for the nest's bulk-byte plane
// (`/api/v1/chunks`, `/api/v1/manifests`, `/api/v1/blob/{cid}`) — the carve-out
// bulk binary rides instead of the 2-MiB-capped WS-RPC plane (webdav-server.md
// § Bulk-byte plane). It is used by the MDA (WebDAV folders, mail body
// references, and the `__index` content-index rail) and the MTA (staging an
// oversized sealed mail body).
package byteplane

import (
	"bytes"
	"context"
	"encoding/base32"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"

	"lukechampine.com/blake3"
)

// ErrChunkNotFound is returned by the download routes on a 404 — a chunk or
// manifest hash the store does not hold. Callers map it to their own
// protocol's not-found status (e.g. the WebDAV backend maps it to a WebDAV
// 404, or, mid-GET, a 500 since a listed file's chunks should exist).
var ErrChunkNotFound = errors.New("byteplane: chunk or manifest not found")

// Chunks are ≤8 MB (webdav-server.md § Bulk-byte plane); bound the download
// read generously past the ciphertext+tag+compression envelope. Manifests match
// the nest CHUNK_BLOB_BODY_LIMIT (10 MiB).
const (
	maxChunkDownloadBytes = 16 << 20
	maxManifestBytes      = 10 << 20
	maxUploadReplyBytes   = 4 << 20
)

// Client talks to the nest's HTTP bulk-byte plane (`/api/v1/chunks`,
// `/api/v1/manifests`) — the carve-out chunks ride instead of the 2-MiB-capped
// WS-RPC plane (webdav-server.md § Bulk-byte plane). Download routes are open
// (ciphertext-by-hash, no confidentiality without the content key); the write
// routes take a short-TTL bulk token the caller mints per-session via
// `mint_bulk_byte_token`.
type Client struct {
	baseURL string
	http    *http.Client
}

// New constructs a Client against the nest's `nestBaseURL` (a ws(s):// or
// http(s):// endpoint — normalized via NormalizeHTTPBase). A nil httpClient
// defaults to http.DefaultClient.
func New(nestBaseURL string, httpClient *http.Client) *Client {
	if httpClient == nil {
		httpClient = http.DefaultClient
	}
	return &Client{baseURL: NormalizeHTTPBase(nestBaseURL), http: httpClient}
}

// NormalizeHTTPBase maps a ws(s):// endpoint to its http(s):// counterpart and
// trims a trailing slash — the byte routes are plain HTTP on the same nest host
// the WS-RPC connection dials (inverse of client.buildWSURL).
func NormalizeHTTPBase(base string) string {
	base = strings.TrimRight(base, "/")
	switch {
	case strings.HasPrefix(base, "wss://"):
		base = "https://" + base[len("wss://"):]
	case strings.HasPrefix(base, "ws://"):
		base = "http://" + base[len("ws://"):]
	}
	return base
}

// DownloadChunk fetches a ciphertext chunk by its store key (hex blake3 of the
// ciphertext). Open route — no token.
func (c *Client) DownloadChunk(ctx context.Context, storeKeyHex string) ([]byte, error) {
	return c.get(ctx, "/api/v1/chunks/"+storeKeyHex, maxChunkDownloadBytes)
}

// DownloadManifest fetches a manifest blob by its hex hash. Open route.
func (c *Client) DownloadManifest(ctx context.Context, manifestHashHex string) ([]byte, error) {
	return c.get(ctx, "/api/v1/manifests/"+manifestHashHex, maxManifestBytes)
}

func (c *Client) get(ctx context.Context, path string, limit int64) ([]byte, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, c.baseURL+path, nil)
	if err != nil {
		return nil, fmt.Errorf("build GET %s: %w", path, err)
	}
	resp, err := c.http.Do(req)
	if err != nil {
		return nil, fmt.Errorf("GET %s: %w", path, err)
	}
	defer func() { _ = resp.Body.Close() }()
	if resp.StatusCode == http.StatusNotFound {
		return nil, ErrChunkNotFound
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("GET %s: unexpected status %d", path, resp.StatusCode)
	}
	body, err := io.ReadAll(io.LimitReader(resp.Body, limit+1))
	if err != nil {
		return nil, fmt.Errorf("read GET %s: %w", path, err)
	}
	if int64(len(body)) > limit {
		return nil, fmt.Errorf("GET %s: body exceeds %d-byte cap", path, limit)
	}
	return body, nil
}

// hashReply is the nest's `201 CREATED` upload response for both chunk and
// manifest POSTs: `{"hash": "<hex>"}`.
type hashReply struct {
	Hash string `json:"hash"`
}

// UploadChunk POSTs a ciphertext chunk under a bulk `write` token, stamping
// `X-Content-Hash` = hex blake3(ciphertext) = the store key, which passes the
// route's F9 anti-poisoning check unchanged (webdav-server.md § Bulk-byte
// plane). Idempotent — content-addressed, so re-POSTing a present chunk is a
// no-op 201.
func (c *Client) UploadChunk(ctx context.Context, token string, ciphertext []byte, storeKeyHex string) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, c.baseURL+"/api/v1/chunks", bytes.NewReader(ciphertext))
	if err != nil {
		return fmt.Errorf("build chunk upload: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("X-Content-Hash", storeKeyHex)
	req.Header.Set("Content-Type", "application/octet-stream")
	_, err = c.doUpload(req, "chunk")
	return err
}

// UploadManifest POSTs a serialized ChunkManifest under a bulk `write` token
// and returns the nest-computed manifest hash (hex) — e.g. the WebDAV ETag.
func (c *Client) UploadManifest(ctx context.Context, token string, manifestBytes []byte) (string, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, c.baseURL+"/api/v1/manifests", bytes.NewReader(manifestBytes))
	if err != nil {
		return "", fmt.Errorf("build manifest upload: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("Content-Type", "application/octet-stream")
	return c.doUpload(req, "manifest")
}

func (c *Client) doUpload(req *http.Request, kind string) (string, error) {
	resp, err := c.http.Do(req)
	if err != nil {
		return "", fmt.Errorf("%s upload: %w", kind, err)
	}
	defer func() { _ = resp.Body.Close() }()
	body, _ := io.ReadAll(io.LimitReader(resp.Body, maxUploadReplyBytes))
	if resp.StatusCode != http.StatusCreated {
		return "", fmt.Errorf("%s upload: status %d: %s", kind, resp.StatusCode, strings.TrimSpace(string(body)))
	}
	var reply hashReply
	if err := json.Unmarshal(body, &reply); err != nil {
		return "", fmt.Errorf("%s upload: decode reply: %w", kind, err)
	}
	return reply.Hash, nil
}

// CheckChunks asks the nest which of `storeKeysHex` are absent, so the caller
// only uploads missing chunks (content-addressed dedup, mirroring the sync
// engine). Requires a `write` token like the upload routes.
func (c *Client) CheckChunks(ctx context.Context, token string, storeKeysHex []string) (map[string]bool, error) {
	reqBody, err := json.Marshal(struct {
		Hashes []string `json:"hashes"`
	}{Hashes: storeKeysHex})
	if err != nil {
		return nil, fmt.Errorf("encode check_chunks: %w", err)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, c.baseURL+"/api/v1/chunks/check", bytes.NewReader(reqBody))
	if err != nil {
		return nil, fmt.Errorf("build check_chunks: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("Content-Type", "application/json")
	resp, err := c.http.Do(req)
	if err != nil {
		return nil, fmt.Errorf("check_chunks: %w", err)
	}
	defer func() { _ = resp.Body.Close() }()
	body, _ := io.ReadAll(io.LimitReader(resp.Body, maxUploadReplyBytes))
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("check_chunks: status %d: %s", resp.StatusCode, strings.TrimSpace(string(body)))
	}
	var reply struct {
		Missing []string `json:"missing"`
	}
	if err := json.Unmarshal(body, &reply); err != nil {
		return nil, fmt.Errorf("check_chunks: decode reply: %w", err)
	}
	missing := make(map[string]bool, len(reply.Missing))
	for _, h := range reply.Missing {
		missing[h] = true
	}
	return missing, nil
}

// ── The `__index` blob route ────────────────────────────────────────────────
//
// `PUT|GET /api/v1/blob/{cid}` is a different route from the chunk plane above
// and the content-index rail is its canonical caller: one sealed index segment
// is ONE blob, because the `__index` rail has no chunk-manifest form
// (content-index.md § Ingest triggers, v1). Writers therefore bound their own
// segment size rather than spilling to `/api/v1/chunks`.
//
// Ordering is not a convention: the nest verifies it holds the blob before
// `fauna.bridges.index_record` will write the journal row, so the bytes go
// here FIRST and the reference second.

// Blob bodies are capped by the nest's BLOB_BODY_LIMIT (10 MiB); the shared
// builder's own ceiling (fauna-client-index MAX_SEGMENT_BYTES) is 8 MiB, so a
// well-formed segment always fits. Bound the download read at the nest's cap.
const maxBlobDownloadBytes = 10 << 20

// blobCID returns the Fauna content identifier of b — CIDv1, **raw** codec
// (0x55), BLAKE3-256 multihash, multibase base32-lower — the exact string the
// PUT route's path segment must carry.
//
// Assembled byte-for-byte as `fauna_cbor::Cid::of_raw(..).to_base32()` does it,
// because the nest recomputes `blake3(body)` and refuses a mismatch with a 400:
// a divergence between the two encoders is a hard failure, not a silent one, so
// the layout is written out explicitly here rather than routed through a
// multihash table whose constants could drift.
//
// Raw codec, never dag-cbor: a sealed segment is opaque bytes with no IPLD
// structure to link into.
func blobCID(b []byte) string {
	sum := blake3.Sum256(b)
	buf := make([]byte, 0, 36)
	buf = append(buf,
		0x01, // CIDv1
		0x55, // raw codec
		0x1e, // blake3-256 multihash code
		0x20, // 32-byte digest
	)
	buf = append(buf, sum[:]...)
	// Multibase 'b' = base32 lower, unpadded (RFC 4648 without padding).
	return "b" + strings.ToLower(base32.StdEncoding.WithPadding(base32.NoPadding).EncodeToString(buf))
}

// BlobCID exposes the content identifier a caller will publish under, so it can
// log or record the reference without re-deriving the encoding itself.
func BlobCID(b []byte) string { return blobCID(b) }

// BlobHashHex is the same 32 bytes as [BlobCID]'s digest, in the **hex** form the
// WS-RPC rails record and list blobs by (`fauna_core::hex32`).
//
// The two spellings coexist on purpose and are not interchangeable at a call
// site: the PUT is addressed by the base32 CID (the nest recomputes it from the
// body), while a rail reference and a download are keyed by the bare hex digest.
// Deriving both here from the one `blake3.Sum256` is what keeps a caller from
// converting between them by hand and getting it subtly wrong.
func BlobHashHex(b []byte) string {
	sum := blake3.Sum256(b)
	return hex.EncodeToString(sum[:])
}

// UploadBlob PUTs opaque bytes under their own CID and returns that CID.
//
// Bearer-authed (unlike the chunk *download* routes, which are open) and
// idempotent: the nest answers 200 for both a fresh store and a repeat PUT of
// bytes it already holds, so a retried publish is free rather than an error.
func (c *Client) UploadBlob(ctx context.Context, token string, blobBytes []byte) (string, error) {
	cid := blobCID(blobBytes)
	req, err := http.NewRequestWithContext(ctx, http.MethodPut, c.baseURL+"/api/v1/blob/"+cid, bytes.NewReader(blobBytes))
	if err != nil {
		return "", fmt.Errorf("build blob upload: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("Content-Type", "application/octet-stream")
	resp, err := c.http.Do(req)
	if err != nil {
		return "", fmt.Errorf("blob upload: %w", err)
	}
	defer func() { _ = resp.Body.Close() }()
	body, _ := io.ReadAll(io.LimitReader(resp.Body, maxUploadReplyBytes))
	// 200 for both "created" and "exists" — this route reports the outcome in
	// the body, not the status, so treating a repeat as success is correct.
	if resp.StatusCode != http.StatusOK {
		return "", fmt.Errorf("blob upload: status %d: %s", resp.StatusCode, strings.TrimSpace(string(body)))
	}
	return cid, nil
}

// DownloadBlob fetches a blob by the **hex** blake3 digest the rail's listing
// hands out.
//
// The same bytes are addressable two ways — by the base32 CID they were PUT
// under and by the bare hex digest — because the nest keys its store on the
// 32-byte digest either way. The rail lists hex, so that is what this takes;
// callers never convert between the two.
//
// Open route (no bearer), like the chunk downloads: the bytes are AEAD-sealed
// under a key no nest holds, so there is no confidentiality resting on the
// request being authenticated.
func (c *Client) DownloadBlob(ctx context.Context, blobHashHex string) ([]byte, error) {
	return c.get(ctx, "/api/v1/blob/"+blobHashHex, maxBlobDownloadBytes)
}
