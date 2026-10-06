package webdav

import (
	"bytes"
	"context"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"mime"
	"net/http"
	"path"
	"sort"
	"strings"
	"time"

	"github.com/emersion/go-webdav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/byteplane"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// Backend implements emersion/go-webdav's FileSystem over the user's folder
// substrate (webdav-server.md § What the WebDAV namespace is). It is stateless —
// all per-request state (the AUTH'd actor, the WS-RPC caller, the unsealed
// served-set content keys) rides the davauth.Session in the request context.
//
// Chunk crypto — and chunk FRAMING — stay in shared Rust (the FFI), as the
// sync engine's own pipeline rather than a per-chunk primitive: GET fetches a
// manifest + its ciphertext chunks by store key and opens them through the
// apps' shared walk (`WebdavOpenFile`) under the served set's M2 content keys;
// PUT chunks+seals through the engine's own seal (`WebdavSealFile`), uploads,
// and records an ordinary folder change. Nothing here decides what bytes the
// AEAD sees: until 2026-09-03 a Go loop sealed each RAW chunk through a bare
// `EncryptChunk` while every Rust writer sealed the FRAMED body — two
// plaintexts under one deterministic (key, nonce), a two-time pad. There is NO
// mirror store (§ Architectural rules).
type Backend struct {
	logger *slog.Logger
	client wsrpc.Caller
	bytes  *byteplane.Client
}

// NewBackend constructs a stateless WebDAV backend. `client` is the MDA-process
// WS-RPC caller (also reachable per-request via the session); `bc` is the
// byte-route HTTP client.
func NewBackend(logger *slog.Logger, client wsrpc.Caller, bc *byteplane.Client) *Backend {
	if logger == nil {
		logger = slog.Default()
	}
	return &Backend{logger: logger, client: client, bytes: bc}
}

var _ webdav.FileSystem = (*Backend)(nil)

var errAuthRequired = webdav.NewHTTPError(http.StatusUnauthorized, errors.New("webdav: authentication required"))

// session extracts the AUTH'd Session the davauth middleware injected. A nil
// session is a 401 (the middleware should have rejected first — defensive).
func (b *Backend) session(ctx context.Context) (*davauth.Session, error) {
	sess := davauth.SessionFromContext(ctx)
	if sess == nil {
		return nil, errAuthRequired
	}
	return sess, nil
}

// davPath is a parsed WebDAV URL path: `/webdav/{user}/{set}/{rel...}`. The
// {user} segment is cosmetic — every nest call is scoped to the AUTH'd actor, so
// a user can only ever reach their own sets regardless of the path. `set` is ""
// at the root collection; `rel` is "" at a set's root, else the normalized
// forward-slash relative path (no leading/trailing slash).
type davPath struct {
	set string
	rel string
}

// parsePath splits an emersion `name` (the raw r.URL.Path) into its DAV parts.
func parsePath(name string) davPath {
	// Trim the /webdav mount prefix and any leading/trailing slashes.
	trimmed := strings.Trim(name, "/")
	trimmed = strings.TrimPrefix(trimmed, "webdav")
	trimmed = strings.Trim(trimmed, "/")
	if trimmed == "" {
		return davPath{}
	}
	segs := strings.SplitN(trimmed, "/", 3) // [user, set, rel...]
	if len(segs) < 2 {
		return davPath{} // /webdav/{user} → root collection
	}
	dp := davPath{set: segs[1]}
	if len(segs) == 3 {
		dp.rel = strings.Trim(segs[2], "/")
	}
	return dp
}

// urlJoin joins a request path with a child name under a single slash.
func urlJoin(base, child string) string {
	return strings.TrimRight(base, "/") + "/" + child
}

// mapErr turns a nest RpcError into the right WebDAV HTTP status. A conflict
// (lost If-Match/If-None-Match race, enforced authoritatively at the nest) is a
// 412; an unserved set is 404; a storage-quota rejection is 507. Anything else
// stays a 500.
func mapErr(err error) error {
	if err == nil {
		return nil
	}
	if code, ok := wsrpc.RpcErrorCode(err); ok {
		switch code {
		case "fauna.bridges.conflict":
			return webdav.NewHTTPError(http.StatusPreconditionFailed, err)
		case "fauna.bridges.set_not_served":
			return webdav.NewHTTPError(http.StatusNotFound, err)
		case "fauna.sync.storage_quota_exceeded":
			return webdav.NewHTTPError(http.StatusInsufficientStorage, err)
		}
	}
	return err
}

// setHash is a served set's hash address — the shared-Rust `set_name_hash`, never
// a Go re-derivation. Every set-scoped request the MDA sends carries it, and the
// nest resolves by it first, so the set stays addressable once the nest's
// plaintext name blanks (`path-sealing.md` § the set-name plane).
func setHash(set string) []byte {
	return faunaFfi.WebdavSetNameHash(set)
}

// setSource is what one request reaches: the root collection's children, the
// served gate, the hash address the nest resolves a set by, and the content
// keys the set opens under. The Basic arm answers from the nest's served list
// and the session's MSEK-unsealed WebdavKeysBlob (basicSource); the bearer door
// answers from the principal's admission and its keys header (principalView,
// bearer.go). Every read path asks the request's source, so the two arms share
// one listing, one render and one open.
type setSource interface {
	rootNames(ctx context.Context) ([]string, error)
	served(ctx context.Context, set string) (bool, error)
	nameHash(set string) []byte
	keys(ctx context.Context, set string) (faunaFfi.FfiServedSetKeys, bool, error)
}

// basicSource is the Basic arm's setSource: the AUTH'd owner's served sets.
type basicSource struct {
	b    *Backend
	sess *davauth.Session
}

func (s basicSource) rootNames(ctx context.Context) ([]string, error) {
	return s.b.servedSetNames(ctx, s.sess)
}

func (s basicSource) served(ctx context.Context, set string) (bool, error) {
	return s.b.isServed(ctx, s.sess, set)
}

func (s basicSource) nameHash(set string) []byte { return setHash(set) }

func (s basicSource) keys(ctx context.Context, set string) (faunaFfi.FfiServedSetKeys, bool, error) {
	return s.b.servedSet(ctx, s.sess, set)
}

// source is the request's setSource: the bearer door's admitted principal when
// one rides the context, else the Basic arm's AUTH'd owner.
func (b *Backend) source(ctx context.Context, sess *davauth.Session) setSource {
	if p := principalFromContext(ctx); p != nil {
		return p
	}
	return basicSource{b: b, sess: sess}
}

// servedSet resolves a served set's unsealed keys (read_only + content-key
// generations) from the session's WebdavKeysBlob — fetched from nest and
// unsealed under the session's MLS capability (the MSEK stays inside the
// capability). Fail-closed: a set absent from the blob (unprovisioned / withheld
// / not served) yields ok=false, and callers reject rather than serve plaintext
// (FS-BIND-5). Needed only by GET + the write paths; PROPFIND does not consume
// keys.
func (b *Backend) servedSet(ctx context.Context, sess *davauth.Session, set string) (faunaFfi.FfiServedSetKeys, bool, error) {
	sets, err := b.unsealedServedSets(ctx, sess)
	if err != nil {
		return faunaFfi.FfiServedSetKeys{}, false, err
	}
	for _, s := range sets {
		if s.SetName == set {
			return s, true, nil
		}
	}
	return faunaFfi.FfiServedSetKeys{}, false, nil
}

// unsealedServedSets fetches the session's WebdavKeysBlob and unseals its
// served-set entries; an absent blob is no entries.
func (b *Backend) unsealedServedSets(ctx context.Context, sess *davauth.Session) ([]faunaFfi.FfiServedSetKeys, error) {
	blob, err := wsrpc.FetchWebdavKeysBlob(ctx, sess.Client(), sess.ActorID())
	if err != nil {
		return nil, err
	}
	if blob == nil {
		return nil, nil
	}
	pt, err := sess.MLSUnwrap().UnsealWebdavKeysBlob(blob)
	if err != nil {
		return nil, fmt.Errorf("unseal webdav keys: %w", err)
	}
	return pt.ServedSets, nil
}

// blobSetNames is the display names the session's WebdavKeysBlob carries.
func (b *Backend) blobSetNames(ctx context.Context, sess *davauth.Session) ([]string, error) {
	sets, err := b.unsealedServedSets(ctx, sess)
	if err != nil {
		return nil, err
	}
	names := make([]string, 0, len(sets))
	for _, s := range sets {
		names = append(names, s.SetName)
	}
	return names, nil
}

// isServed reports whether `set` is one of the actor's WebDAV-served sets
// (nest-authoritative; no keys needed — drives PROPFIND).
func (b *Backend) isServed(ctx context.Context, sess *davauth.Session, set string) (bool, error) {
	sets, err := wsrpc.WebdavListFolders(ctx, sess.Client(), sess.ActorID())
	if err != nil {
		return false, err
	}
	return servesSet(sets, set, setHash(set)), nil
}

// servesSet matches a display name against the nest's served sets by its
// set_name_hash, the address a sealed set keeps once its row holds no plaintext
// name (`path-sealing.md` § the set-name plane). An entry without a hash (a set
// whose name rests plaintext, unstamped) falls back to the name.
func servesSet(sets []wsrpc.WebdavServedSet, set string, hash []byte) bool {
	for _, s := range sets {
		if len(s.NameHash) > 0 {
			if bytes.Equal(s.NameHash, hash) {
				return true
			}
		} else if s.Name != "" && s.Name == set {
			return true
		}
	}
	return false
}

// rootSetNames is the root collection's children: each served set's display
// name — the nest's plaintext where the row still holds one (a public or
// pre-sealing set), else the name from the session's WebdavKeysBlob whose hash
// matches. A sealed set the blob does not name is dropped, the ratified degrade
// for an entry this session cannot name (see listFiles).
func rootSetNames(sets []wsrpc.WebdavServedSet, blobNames []string, hashOf func(string) []byte) []string {
	byHash := make(map[string]string, len(blobNames))
	for _, n := range blobNames {
		byHash[string(hashOf(n))] = n
	}
	out := make([]string, 0, len(sets))
	for _, s := range sets {
		if s.Name != "" {
			out = append(out, s.Name)
		} else if n, ok := byHash[string(s.NameHash)]; ok && len(s.NameHash) > 0 {
			out = append(out, n)
		}
	}
	return out
}

// servedSetNames resolves the root collection's children, fetching the
// WebdavKeysBlob only when a served set's row holds no plaintext name.
func (b *Backend) servedSetNames(ctx context.Context, sess *davauth.Session) ([]string, error) {
	sets, err := wsrpc.WebdavListFolders(ctx, sess.Client(), sess.ActorID())
	if err != nil {
		return nil, err
	}
	var blobNames []string
	for _, s := range sets {
		if s.Name != "" {
			continue
		}
		blobNames, err = b.blobSetNames(ctx, sess)
		if err != nil {
			return nil, err
		}
		break
	}
	return rootSetNames(sets, blobNames, setHash), nil
}

// listFiles fetches a served set's listing and renders every row's path
// sealed-first, so the whole DAV hierarchy synthesis below keeps operating on
// session-memory plaintext exactly as it did before sealing existed
// (`webdav-server.md` § Key model). Every read path goes through here — there is
// no second listing seam that could render differently.
//
// A row this session's keys cannot open is DROPPED, which is the ratified
// degrade: *omit the entry from the listing, and let it re-enter on re-record*
// (`file-sync.md` § Sealed names & paths → Migration). Never a blank href — a
// DAV client shown one would corrupt its own view of the collection — and never
// a failed PROPFIND, so one unopenable row cannot take the page down.
//
// Dropping is also the consistent answer for GET: a row the MDA cannot name is
// one whose chunks it cannot decrypt either (the label's generation and the
// chunk stamp come from the same root), so `readFileBytes` would fail closed on
// it regardless.
func (b *Backend) listFiles(ctx context.Context, sess *davauth.Session, set string) ([]wsrpc.WebdavFile, error) {
	files, err := wsrpc.WebdavListFiles(ctx, sess.Client(), sess.ActorID(), b.source(ctx, sess).nameHash(set))
	if err != nil {
		return nil, err
	}

	// The ONE safe skip: nothing on this page is sealed, so every name is the
	// plaintext (a public-audience or plaintext-plane row) and a keyless
	// session lists it directly.
	//
	// ⚠ Do NOT widen this to "this session holds no keys → skip the render".
	// That is the S3 bug: a keyless reader meeting a sealed-only row would then
	// render its BLANK plaintext as the name, the one outcome the degrade
	// exists to forbid.
	sealed := false
	for i := range files {
		if len(files[i].PathSealed) > 0 {
			sealed = true
			break
		}
	}
	if !sealed {
		return files, nil
	}

	served, ok, err := b.source(ctx, sess).keys(ctx, set)
	if err != nil {
		return nil, err
	}
	if !ok {
		// No content keys for this set in the blob. Apply the SAME policy the
		// FFI render applies, not a stricter one: seal (impossible here) →
		// plaintext → omit. So a public-audience / plaintext-plane row lists
		// by its plaintext, and only a scrubbed one omits. Dropping every sealed
		// row here instead would make a keyless session see a *smaller*
		// listing than one holding a merely wrong key — two rules for one
		// policy, and the divergence would be invisible until the flip.
		out := make([]wsrpc.WebdavFile, 0, len(files))
		for _, f := range files {
			if f.Path != "" {
				out = append(out, f)
			}
		}
		return out, nil
	}

	rows := make([]faunaFfi.FfiWebdavPathRow, len(files))
	for i, f := range files {
		rows[i] = faunaFfi.FfiWebdavPathRow{
			Path:       f.Path,
			PathSealed: optBytes(f.PathSealed),
			PathHash:   optBytes(f.PathHash),
		}
	}
	rendered, err := faunaFfi.WebdavRenderPaths(served.Keys, rows)
	if err != nil {
		return nil, fmt.Errorf("webdav: render sealed paths: %w", err)
	}
	if len(rendered) != len(files) {
		// Index alignment is the contract; a mismatch would silently rename
		// rows, so fail loud rather than serve a scrambled collection.
		return nil, fmt.Errorf("webdav: render returned %d names for %d files", len(rendered), len(files))
	}

	out := make([]wsrpc.WebdavFile, 0, len(files))
	for i, name := range rendered {
		if name == nil {
			continue // unopenable — omit, per the ratified degrade
		}
		f := files[i]
		f.Path = *name
		out = append(out, f)
	}
	return out, nil
}

// sealRecordedPath seals `rel` under the served set's CURRENT content-key
// generation — the bridge-side half of the keyed-writer contract
// (`webdav-server.md` § Key model: the nest holds no key that could seal it).
// Every WebDAV write records through here, so a DAV-written row rests
// indistinguishable from one an ordinary client's engine recorded.
//
// Sealing under `current` is right even when the row's chunks stay at an older
// generation (the COPY/MOVE case, which preserves the source stamp): a label is
// discriminated by its own envelope `gen`, never by the chunk's
// `content_key_version`, so the two are independently resolvable.
func sealRecordedPath(served faunaFfi.FfiServedSetKeys, rel string) ([]byte, error) {
	sealed, err := faunaFfi.WebdavSealPath(served.Keys, rel)
	if err != nil {
		return nil, fmt.Errorf("webdav: seal recorded path: %w", err)
	}
	return sealed, nil
}

// optBytes maps an empty/absent byte slice to a nil optional, matching the
// wire's `skip_serializing_if = "Option::is_none"` discipline.
func optBytes(b []byte) *[]byte {
	if len(b) == 0 {
		return nil
	}
	return &b
}

// ── FileSystem: read side (PROPFIND, GET/HEAD) ──────────────────────────

func (b *Backend) Stat(ctx context.Context, name string) (*webdav.FileInfo, error) {
	sess, err := b.session(ctx)
	if err != nil {
		return nil, err
	}
	dp := parsePath(name)

	// Root collection.
	if dp.set == "" {
		return &webdav.FileInfo{Path: name, IsDir: true}, nil
	}

	served, err := b.source(ctx, sess).served(ctx, dp.set)
	if err != nil {
		return nil, mapErr(err)
	}
	if !served {
		return nil, webdav.NewHTTPError(http.StatusNotFound, fmt.Errorf("webdav: set %q not served", dp.set))
	}

	// Set root collection.
	if dp.rel == "" {
		return &webdav.FileInfo{Path: name, IsDir: true}, nil
	}

	files, err := b.listFiles(ctx, sess, dp.set)
	if err != nil {
		return nil, mapErr(err)
	}
	if f, ok := findFile(files, dp.rel); ok {
		return fileInfo(name, f), nil
	}
	if isDirPrefix(files, dp.rel) {
		return &webdav.FileInfo{Path: name, IsDir: true}, nil
	}
	return nil, webdav.NewHTTPError(http.StatusNotFound, fmt.Errorf("webdav: %q not found", dp.rel))
}

func (b *Backend) ReadDir(ctx context.Context, name string, recursive bool) ([]webdav.FileInfo, error) {
	sess, err := b.session(ctx)
	if err != nil {
		return nil, err
	}
	dp := parsePath(name)

	// Root: self + one child collection per served set.
	if dp.set == "" {
		sets, err := b.source(ctx, sess).rootNames(ctx)
		if err != nil {
			return nil, mapErr(err)
		}
		out := []webdav.FileInfo{{Path: name, IsDir: true}}
		for _, s := range sets {
			out = append(out, webdav.FileInfo{Path: urlJoin(name, s), IsDir: true})
		}
		return out, nil
	}

	served, err := b.source(ctx, sess).served(ctx, dp.set)
	if err != nil {
		return nil, mapErr(err)
	}
	if !served {
		return nil, webdav.NewHTTPError(http.StatusNotFound, fmt.Errorf("webdav: set %q not served", dp.set))
	}

	files, err := b.listFiles(ctx, sess, dp.set)
	if err != nil {
		return nil, mapErr(err)
	}
	// Self + immediate children under dp.rel. Depth:infinity is rejected upstream
	// (w1Mitigations), so `recursive` is never true in practice; immediate
	// children satisfy the Depth:0/1 browse.
	out := []webdav.FileInfo{{Path: name, IsDir: true}}
	for _, c := range immediateChildren(files, dp.rel) {
		if c.isDir {
			out = append(out, webdav.FileInfo{Path: urlJoin(name, c.name), IsDir: true})
		} else {
			out = append(out, *fileInfo(urlJoin(name, c.name), c.file))
		}
	}
	return out, nil
}

func (b *Backend) Open(ctx context.Context, name string) (io.ReadCloser, error) {
	sess, err := b.session(ctx)
	if err != nil {
		return nil, err
	}
	dp := parsePath(name)
	if dp.set == "" || dp.rel == "" {
		return nil, webdav.NewHTTPError(http.StatusMethodNotAllowed, errors.New("webdav: cannot GET a collection"))
	}

	files, err := b.listFiles(ctx, sess, dp.set)
	if err != nil {
		return nil, mapErr(err)
	}
	file, ok := findFile(files, dp.rel)
	if !ok {
		return nil, webdav.NewHTTPError(http.StatusNotFound, fmt.Errorf("webdav: %q not found", dp.rel))
	}

	served, ok, err := b.source(ctx, sess).keys(ctx, dp.set)
	if err != nil {
		return nil, mapErr(err)
	}
	if !ok {
		// Set is listed but its keys are not in the blob — fail closed rather
		// than serve ciphertext we cannot decrypt (FS-BIND-5).
		return nil, webdav.NewHTTPError(http.StatusServiceUnavailable, fmt.Errorf("webdav: no content keys for set %q", dp.set))
	}

	data, err := b.readFileBytes(ctx, served, file)
	if err != nil {
		return nil, err
	}
	return readSeekCloser{bytes.NewReader(data)}, nil
}

// readFileBytes fetches a file's manifest + ciphertext chunks by store key and
// opens them through the apps' SHARED walk (`WebdavOpenFile` →
// `fauna_core::file_download::download_file_bytes_by_manifest`): generation
// selection for the file's stamp (every same-version candidate, current-first;
// a generation this session lacks fails closed), the AEAD open, the unframe decided by
// the manifest's plaintext hash, and the whole-file verify. No read policy
// lives in Go: the per-chunk `DecryptChunk` loop that stood here until
// 2026-09-03 never unframed, so an app-written file came back over DAV with
// its compression frame bytes.
func (b *Backend) readFileBytes(ctx context.Context, served faunaFfi.FfiServedSetKeys, file *wsrpc.WebdavFile) ([]byte, error) {
	if file.ContentKeyVersion == nil {
		return nil, webdav.NewHTTPError(http.StatusServiceUnavailable, fmt.Errorf("webdav: file %q has no content-key generation", file.Path))
	}

	manifestBytes, err := b.bytes.DownloadManifest(ctx, file.ManifestHash)
	if err != nil {
		return nil, fmt.Errorf("download manifest %s: %w", file.ManifestHash, err)
	}
	// Decoded here only to learn the store keys to fetch; the walk decodes
	// (and verifies) it again on its own terms. A sealed manifest names no
	// plaintext chunk hash (they ride sealed — `mls-group-key-material.md`
	// § M2 *Sealed manifest hashes*), so the store keys are the whole list;
	// the walk opens the sealed hashes under the served set's keys.
	manifest, err := faunaFfi.DeserializeManifest(manifestBytes)
	if err != nil {
		return nil, fmt.Errorf("decode manifest: %w", err)
	}
	if manifest.StoredHashes == nil {
		return nil, webdav.NewHTTPError(http.StatusServiceUnavailable, errors.New("webdav: manifest missing ciphertext store keys (not content-key sealed)"))
	}
	stored := *manifest.StoredHashes

	bodies := make([][]byte, 0, len(stored))
	for _, storeKey := range stored {
		ct, err := b.bytes.DownloadChunk(ctx, hex.EncodeToString(storeKey))
		if err != nil {
			return nil, fmt.Errorf("download chunk %x: %w", storeKey, err)
		}
		bodies = append(bodies, ct)
	}

	// The walk names the missing generation, the failing chunk, or the
	// whole-file mismatch itself; all three are a 500 here (the served set's
	// keys ARE in the blob — this is not the keyless 503 above).
	pt, err := faunaFfi.WebdavOpenFile(served.Keys, *file.ContentKeyVersion, file.Path, manifestBytes, bodies)
	if err != nil {
		return nil, fmt.Errorf("open %q: %w", file.Path, err)
	}
	return pt, nil
}

// ── FileSystem: write side (PUT, DELETE, MKCOL, MOVE, COPY) ──────────────

func (b *Backend) Create(ctx context.Context, name string, body io.ReadCloser, opts *webdav.CreateOptions) (*webdav.FileInfo, bool, error) {
	sess, err := b.session(ctx)
	if err != nil {
		return nil, false, err
	}
	dp := parsePath(name)
	if dp.set == "" || dp.rel == "" {
		return nil, false, webdav.NewHTTPError(http.StatusMethodNotAllowed, errors.New("webdav: cannot PUT a collection"))
	}

	served, ok, err := b.writableSet(ctx, sess, dp.set)
	if err != nil {
		return nil, false, err
	}
	if !ok {
		return nil, false, webdav.NewHTTPError(http.StatusForbidden, fmt.Errorf("webdav: set %q is read-only", dp.set))
	}

	data, err := io.ReadAll(body)
	if err != nil {
		var mbErr *http.MaxBytesError
		if errors.As(err, &mbErr) {
			return nil, false, webdav.NewHTTPError(http.StatusRequestEntityTooLarge, errBodyTooLarge)
		}
		return nil, false, fmt.Errorf("webdav: read PUT body: %w", err)
	}

	// created vs replaced (201 vs 204) + change_type: look up the current head.
	existing, err := b.listFiles(ctx, sess, dp.set)
	if err != nil {
		return nil, false, mapErr(err)
	}
	_, existed := findFile(existing, dp.rel)
	changeType := "create"
	if existed {
		changeType = "modify"
	}

	manifestHash, version, err := b.chunkSealUpload(ctx, sess, served, data)
	if err != nil {
		return nil, false, err
	}

	// `version` is the generation the bytes were ACTUALLY sealed under
	// (`WebdavSealFile` seals under `served.Keys.Current` and reports it back),
	// and the label below seals under `served.Keys.Current` too — one
	// generation for the record, its label and its chunks, by construction.
	pathSealed, err := sealRecordedPath(served, dp.rel)
	if err != nil {
		return nil, false, err
	}
	ifMatch, ifNoneMatch := conditionals(opts.IfMatch, opts.IfNoneMatch)
	if _, err := wsrpc.WebdavRecordChange(
		ctx, sess.Client(), sess.ActorID(), setHash(dp.set), dp.rel,
		&manifestHash, int64(len(data)), changeType, &version, ifMatch, ifNoneMatch,
		pathSealed,
	); err != nil {
		return nil, false, mapErr(err)
	}

	return &webdav.FileInfo{
		Path:    name,
		Size:    int64(len(data)),
		ModTime: time.Now(),
		ETag:    manifestHash,
	}, !existed, nil
}

// chunkSealUpload chunks + seals `data` under the set's CURRENT content key
// through the sync engine's own seal (`WebdavSealFile` → `seal_blob`: FastCDC,
// the ONE per-chunk frame→AEAD→re-key pipeline, the canonical manifest
// carrying `stored_hashes`), uploads the missing ciphertext chunks + the
// manifest, and returns the manifest hash (the ETag) and the generation the
// bytes were sealed under. Byte-identical to what an app's engine uploads for
// the same bytes — which is the whole point: the `EncryptChunk` loop that
// stood here until 2026-09-03 sealed each chunk RAW while the engine sealed it
// FRAMED, resting a second plaintext under the engine's (key, nonce).
func (b *Backend) chunkSealUpload(ctx context.Context, sess *davauth.Session, served faunaFfi.FfiServedSetKeys, data []byte) (string, uint64, error) {
	sealed, err := faunaFfi.WebdavSealFile(served.Keys, data)
	if err != nil {
		return "", 0, fmt.Errorf("webdav: seal file: %w", err)
	}
	storeKeysHex := make([]string, 0, len(sealed.Chunks))
	for _, c := range sealed.Chunks {
		storeKeysHex = append(storeKeysHex, hex.EncodeToString(c.StoreKey))
	}

	token, _, err := wsrpc.MintBulkByteToken(ctx, sess.Client(), sess.ActorID(), setHash(served.SetName), wsrpc.BulkByteAccessWrite)
	if err != nil {
		return "", 0, mapErr(err)
	}

	missing, err := b.bytes.CheckChunks(ctx, token, storeKeysHex)
	if err != nil {
		return "", 0, err
	}
	for i, keyHex := range storeKeysHex {
		if missing[keyHex] {
			if err := b.bytes.UploadChunk(ctx, token, sealed.Chunks[i].Body, keyHex); err != nil {
				return "", 0, err
			}
		}
	}

	manifestHash, err := b.bytes.UploadManifest(ctx, token, sealed.ManifestBytes)
	if err != nil {
		return "", 0, err
	}
	// The nest addresses the manifest by blake3(bytes); the seal computed the
	// same value. A disagreement would mean the record points at bytes the
	// devices cannot find — fail loud rather than record it.
	if want := hex.EncodeToString(sealed.ManifestHash); !strings.EqualFold(manifestHash, want) {
		return "", 0, fmt.Errorf("webdav: nest stored the manifest as %s, the seal computed %s", manifestHash, want)
	}
	return manifestHash, sealed.ContentKeyVersion, nil
}

func (b *Backend) RemoveAll(ctx context.Context, name string, opts *webdav.RemoveAllOptions) error {
	sess, err := b.session(ctx)
	if err != nil {
		return err
	}
	dp := parsePath(name)
	if dp.set == "" {
		return webdav.NewHTTPError(http.StatusForbidden, errors.New("webdav: cannot delete a served set over WebDAV (use the client)"))
	}

	served, ok, err := b.writableSet(ctx, sess, dp.set)
	if err != nil {
		return err
	}
	if !ok {
		return webdav.NewHTTPError(http.StatusForbidden, fmt.Errorf("webdav: set %q is read-only", dp.set))
	}
	files, err := b.listFiles(ctx, sess, dp.set)
	if err != nil {
		return mapErr(err)
	}

	if dp.rel == "" {
		// Deleting the whole set collection: tombstone every file.
		return b.tombstoneAll(ctx, sess, served, dp.set, files, "", nil, nil)
	}
	if file, ok := findFile(files, dp.rel); ok {
		ifMatch, ifNoneMatch := conditionals(opts.IfMatch, opts.IfNoneMatch)
		_ = file
		return b.tombstone(ctx, sess, served, dp.set, dp.rel, ifMatch, ifNoneMatch)
	}
	if isDirPrefix(files, dp.rel) {
		return b.tombstoneAll(ctx, sess, served, dp.set, files, dp.rel, nil, nil)
	}
	return webdav.NewHTTPError(http.StatusNotFound, fmt.Errorf("webdav: %q not found", dp.rel))
}

// tombstone records a delete change for one path. The tombstone row carries the
// path too, so it seals like every other write — a delete must not be the one
// DAV row that leaves a filename resting plaintext.
func (b *Backend) tombstone(ctx context.Context, sess *davauth.Session, served faunaFfi.FfiServedSetKeys, set, rel string, ifMatch, ifNoneMatch *string) error {
	pathSealed, err := sealRecordedPath(served, rel)
	if err != nil {
		return err
	}
	_, err = wsrpc.WebdavRecordChange(
		ctx, sess.Client(), sess.ActorID(), setHash(set), rel,
		nil, 0, "delete", nil, ifMatch, ifNoneMatch,
		pathSealed,
	)
	return mapErr(err)
}

// tombstoneAll records a delete for every file at-or-under `prefix` ("" = the
// whole set). Conditionals are not applied to a bulk collection delete.
func (b *Backend) tombstoneAll(ctx context.Context, sess *davauth.Session, served faunaFfi.FfiServedSetKeys, set string, files []wsrpc.WebdavFile, prefix string, _, _ *string) error {
	pfx := ""
	if prefix != "" {
		pfx = prefix + "/"
	}
	for _, f := range files {
		if pfx == "" || strings.HasPrefix(f.Path, pfx) || f.Path == prefix {
			if err := b.tombstone(ctx, sess, served, set, f.Path, nil, nil); err != nil {
				return err
			}
		}
	}
	return nil
}

func (b *Backend) Mkdir(ctx context.Context, name string) error {
	sess, err := b.session(ctx)
	if err != nil {
		return err
	}
	dp := parsePath(name)
	if dp.set == "" || dp.rel == "" {
		// MKCOL at the set level would create a new folder — that is a
		// client-side, content-key-genesis + serve-enable operation, not a WebDAV
		// one (webdav-server.md § Enablement).
		return webdav.NewHTTPError(http.StatusForbidden, errors.New("webdav: cannot create a folder over WebDAV (use the client)"))
	}
	// Folders are flat path→content maps: a directory is implicit in its files'
	// paths. MKCOL on a writable served set is accepted as a no-op — the
	// collection materializes when the first file is PUT under it. (An empty
	// directory has no representation; that persistence gap is a documented v1
	// limitation, like Class-2 LOCK.) Guard 2 still applies.
	_, ok, err := b.writableSet(ctx, sess, dp.set)
	if err != nil {
		return err
	}
	if !ok {
		return webdav.NewHTTPError(http.StatusForbidden, fmt.Errorf("webdav: set %q is read-only", dp.set))
	}
	return nil
}

func (b *Backend) Copy(ctx context.Context, name, dest string, options *webdav.CopyOptions) (bool, error) {
	return b.copyOrMove(ctx, name, dest, false)
}

func (b *Backend) Move(ctx context.Context, name, dest string, options *webdav.MoveOptions) (bool, error) {
	return b.copyOrMove(ctx, name, dest, true)
}

// copyOrMove re-records a file at `dest` (chunks never move — content-addressed,
// so COPY is free and MOVE re-uses the manifest), then for a MOVE tombstones the
// source. v1 supports same-set copy/move only (a cross-set move would re-seal
// under the destination set's content key — a follow-on).
func (b *Backend) copyOrMove(ctx context.Context, name, dest string, isMove bool) (bool, error) {
	sess, err := b.session(ctx)
	if err != nil {
		return false, err
	}
	src := parsePath(name)
	dst := parsePath(dest)
	if src.set == "" || src.rel == "" || dst.set == "" || dst.rel == "" {
		return false, webdav.NewHTTPError(http.StatusForbidden, errors.New("webdav: COPY/MOVE requires file paths within a served set"))
	}
	if src.set != dst.set {
		return false, webdav.NewHTTPError(http.StatusForbidden, errors.New("webdav: cross-set COPY/MOVE is not supported (v1)"))
	}

	served, ok, err := b.writableSet(ctx, sess, dst.set)
	if err != nil {
		return false, err
	}
	if !ok {
		return false, webdav.NewHTTPError(http.StatusForbidden, fmt.Errorf("webdav: set %q is read-only", dst.set))
	}

	files, err := b.listFiles(ctx, sess, src.set)
	if err != nil {
		return false, mapErr(err)
	}
	file, ok := findFile(files, src.rel)
	if !ok {
		return false, webdav.NewHTTPError(http.StatusNotFound, fmt.Errorf("webdav: %q not found", src.rel))
	}
	_, destExisted := findFile(files, dst.rel)

	changeType := "create"
	if destExisted {
		changeType = "modify"
	}
	version := served.Keys.Current.Version
	mh := file.ManifestHash
	var cv *uint64
	if file.ContentKeyVersion != nil {
		cv = file.ContentKeyVersion // preserve the source generation for the copy
	} else {
		cv = &version
	}
	// Sealed under `current` even though `cv` may preserve the source
	// generation — see sealRecordedPath on why the two are independent.
	pathSealed, err := sealRecordedPath(served, dst.rel)
	if err != nil {
		return false, err
	}
	if _, err := wsrpc.WebdavRecordChange(
		ctx, sess.Client(), sess.ActorID(), setHash(dst.set), dst.rel,
		&mh, file.SizeBytes, changeType, cv, nil, nil,
		pathSealed,
	); err != nil {
		return false, mapErr(err)
	}

	if isMove {
		if err := b.tombstone(ctx, sess, served, src.set, src.rel, nil, nil); err != nil {
			return false, err
		}
	}
	return !destExisted, nil
}

// writableSet resolves a served set and enforces Guard 2 (the user's read-only
// preference) as a HARD gate at the DAV write boundary — read_only is advisory
// metadata from the blob (chunk_crypto is symmetric), so the MDA is the only
// authoritative enforcement point. Returns ok=false when the set is read-only OR
// absent from the blob (fail-closed). A bearer-admitted principal never writes:
// the door refuses every write method before admission (bearer.go), and this
// refuses again here, so no write path can run under a principal's session.
func (b *Backend) writableSet(ctx context.Context, sess *davauth.Session, set string) (faunaFfi.FfiServedSetKeys, bool, error) {
	if principalFromContext(ctx) != nil {
		return faunaFfi.FfiServedSetKeys{}, false, nil
	}
	served, ok, err := b.servedSet(ctx, sess, set)
	if err != nil {
		return faunaFfi.FfiServedSetKeys{}, false, mapErr(err)
	}
	if !ok || served.ReadOnly {
		return faunaFfi.FfiServedSetKeys{}, false, nil
	}
	return served, true, nil
}

// ── helpers ──────────────────────────────────────────────────────────────

// conditionals maps emersion ConditionalMatch values to the wire strings the
// nest enforces: "*" (wildcard) passes through; an ETag passes through; unset is
// nil (absent).
func conditionals(ifMatch, ifNoneMatch webdav.ConditionalMatch) (*string, *string) {
	return condStr(ifMatch), condStr(ifNoneMatch)
}

func condStr(c webdav.ConditionalMatch) *string {
	if !c.IsSet() {
		return nil
	}
	if c.IsWildcard() {
		s := "*"
		return &s
	}
	etag, err := c.ETag()
	if err != nil {
		// A malformed ETag: pass the raw value; nest treats a non-match as a
		// conflict (412), which is the correct precondition outcome.
		s := string(c)
		return &s
	}
	return &etag
}

func fileInfo(urlPath string, f *wsrpc.WebdavFile) *webdav.FileInfo {
	ct := mime.TypeByExtension(path.Ext(f.Path))
	if ct == "" {
		ct = "application/octet-stream"
	}
	return &webdav.FileInfo{
		Path: urlPath,
		Size: f.SizeBytes,
		// `updated_at` is the change's `created_at`, epoch MILLISECONDS on the
		// nest; read as seconds it served a `getlastmodified` ~56,000 years
		// out, which file managers reject or show as garbage.
		ModTime:  time.UnixMilli(f.UpdatedAt),
		IsDir:    false,
		MIMEType: ct,
		ETag:     f.ManifestHash,
	}
}

func findFile(files []wsrpc.WebdavFile, rel string) (*wsrpc.WebdavFile, bool) {
	for i := range files {
		if files[i].Path == rel {
			return &files[i], true
		}
	}
	return nil, false
}

// isDirPrefix reports whether any file lives under `rel`/ (so `rel` is an
// implicit collection).
func isDirPrefix(files []wsrpc.WebdavFile, rel string) bool {
	pfx := rel + "/"
	for _, f := range files {
		if strings.HasPrefix(f.Path, pfx) {
			return true
		}
	}
	return false
}

type childEntry struct {
	name  string
	isDir bool
	file  *wsrpc.WebdavFile
}

// immediateChildren computes the direct children (files + sub-collections) under
// `dirRel` ("" = set root) from the flat file list, sorted by name.
func immediateChildren(files []wsrpc.WebdavFile, dirRel string) []childEntry {
	pfx := ""
	if dirRel != "" {
		pfx = dirRel + "/"
	}
	seen := map[string]childEntry{}
	for i := range files {
		f := &files[i]
		if !strings.HasPrefix(f.Path, pfx) {
			continue
		}
		rest := f.Path[len(pfx):]
		if rest == "" {
			continue
		}
		if slash := strings.IndexByte(rest, '/'); slash >= 0 {
			name := rest[:slash]
			if _, ok := seen[name]; !ok {
				seen[name] = childEntry{name: name, isDir: true}
			}
		} else {
			seen[rest] = childEntry{name: rest, isDir: false, file: f}
		}
	}
	out := make([]childEntry, 0, len(seen))
	for _, c := range seen {
		out = append(out, c)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].name < out[j].name })
	return out
}

// readSeekCloser adapts a *bytes.Reader to io.ReadCloser while preserving
// io.Seeker, so emersion upgrades GET to http.ServeContent (Range support).
type readSeekCloser struct {
	*bytes.Reader
}

func (readSeekCloser) Close() error { return nil }
