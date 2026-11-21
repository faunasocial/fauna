package webdav

import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/byteplane"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
	"lukechampine.com/blake3"
)

// pagedFixture is testdata/paged_manifest.json: a file sealed by the MDA's
// own seal door (`webdav_seal_file`) under a small paging rule, written by the
// Rust test `webdav_paged_fixture_for_the_go_bridge_is_current`, which reds
// when the seal drifts from it. The shipped library pages only past 32,768
// chunks (about 64 GB), so a real root and real pages reach these tests only
// this way (`file-sync.md` § Paged manifests).
type pagedFixture struct {
	KeyVersion uint64 `json:"key_version"`
	KeyByte    byte   `json:"key_byte"`
	Segments   []struct {
		Byte byte `json:"byte"`
		Len  int  `json:"len"`
	} `json:"segments"`
	ManifestHex     string   `json:"manifest_hex"`
	ManifestHashHex string   `json:"manifest_hash_hex"`
	PagesHex        []string `json:"pages_hex"`
	PageByteLens    []uint64 `json:"page_byte_lens"`
	Chunks          []struct {
		AddressHex string `json:"address_hex"`
		BodyHex    string `json:"body_hex"`
	} `json:"chunks"`
}

type loadedFixture struct {
	keys     faunaFfi.FfiFolderContentKeys
	version  uint64
	body     []byte
	manifest []byte
	hashHex  string
	pages    [][]byte
	pageLens []uint64
	chunks   map[string][]byte
}

func mustHex(t *testing.T, s string) []byte {
	t.Helper()
	b, err := hex.DecodeString(s)
	if err != nil {
		t.Fatalf("fixture hex: %v", err)
	}
	return b
}

func loadPagedFixture(t *testing.T) loadedFixture {
	t.Helper()
	raw, err := os.ReadFile("testdata/paged_manifest.json")
	if err != nil {
		t.Fatalf("read fixture: %v", err)
	}
	var f pagedFixture
	if err := json.Unmarshal(raw, &f); err != nil {
		t.Fatalf("decode fixture: %v", err)
	}
	var body []byte
	for _, s := range f.Segments {
		body = append(body, bytes.Repeat([]byte{s.Byte}, s.Len)...)
	}
	out := loadedFixture{
		keys: faunaFfi.FfiFolderContentKeys{
			Current: faunaFfi.FfiContentKeyGeneration{Version: f.KeyVersion, Key: bytes.Repeat([]byte{f.KeyByte}, 32), RotatedAt: 1},
		},
		version:  f.KeyVersion,
		body:     body,
		manifest: mustHex(t, f.ManifestHex),
		hashHex:  f.ManifestHashHex,
		pageLens: f.PageByteLens,
		chunks:   map[string][]byte{},
	}
	for _, p := range f.PagesHex {
		out.pages = append(out.pages, mustHex(t, p))
	}
	for _, c := range f.Chunks {
		out.chunks[c.AddressHex] = mustHex(t, c.BodyHex)
	}
	if len(out.pages) < 3 {
		t.Fatalf("fixture has %d pages, want at least 3", len(out.pages))
	}
	return out
}

// fakeBytePlane is the nest's bulk-byte plane, enough of it for a paged PUT and
// GET: the chunk existence check, the page door and the manifest door (each
// answering the blob's blake3), and the open download routes over one store.
type fakeBytePlane struct {
	mu     sync.Mutex
	blobs  map[string][]byte // hex hash → bytes (chunks, pages and roots alike)
	posted []string          // "page:<hash>" / "manifest:<hash>", in order
}

func newFakeBytePlane() *fakeBytePlane { return &fakeBytePlane{blobs: map[string][]byte{}} }

func (n *fakeBytePlane) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	n.mu.Lock()
	defer n.mu.Unlock()
	store := func(kind string) {
		body, _ := io.ReadAll(r.Body)
		sum := blake3.Sum256(body)
		h := hex.EncodeToString(sum[:])
		n.blobs[h] = body
		n.posted = append(n.posted, kind+":"+h)
		w.WriteHeader(http.StatusCreated)
		_ = json.NewEncoder(w).Encode(map[string]string{"hash": h})
	}
	switch {
	case r.Method == http.MethodPost && r.URL.Path == "/api/v1/chunks/check":
		var req struct {
			Hashes []string `json:"hashes"`
		}
		_ = json.NewDecoder(r.Body).Decode(&req)
		missing := []string{}
		for _, h := range req.Hashes {
			if _, ok := n.blobs[h]; !ok {
				missing = append(missing, h)
			}
		}
		_ = json.NewEncoder(w).Encode(map[string][]string{"missing": missing})
	case r.Method == http.MethodPost && r.URL.Path == "/api/v1/manifest-pages":
		store("page")
	case r.Method == http.MethodPost && r.URL.Path == "/api/v1/manifests":
		store("manifest")
	case r.Method == http.MethodGet && (strings.HasPrefix(r.URL.Path, "/api/v1/manifests/") || strings.HasPrefix(r.URL.Path, "/api/v1/chunks/")):
		h := r.URL.Path[strings.LastIndex(r.URL.Path, "/")+1:]
		blob, ok := n.blobs[h]
		if !ok {
			http.NotFound(w, r)
			return
		}
		_, _ = w.Write(blob)
	default:
		http.Error(w, "unexpected "+r.Method+" "+r.URL.Path, http.StatusTeapot)
	}
}

// A paged PUT: the pages the nest lacks go up through the page door — each
// distinct page once, every one before the root — and the root then reads
// back through the GET leg's walk to the bytes written.
func TestPagedPutUploadsPagesBeforeTheRootAndReadsBack(t *testing.T) {
	fx := loadPagedFixture(t)
	nest := newFakeBytePlane()
	for k, body := range fx.chunks {
		nest.blobs[k] = body // the chunks went up first (chunkSealUpload's loop)
	}
	// One page already held: the existence check skips it.
	held := blake3.Sum256(fx.pages[0])
	heldHex := hex.EncodeToString(held[:])
	nest.blobs[heldHex] = fx.pages[0]

	srv := httptest.NewServer(nest)
	defer srv.Close()
	b := NewBackend(nil, nil, byteplane.New(srv.URL, srv.Client()))
	ctx := context.Background()

	if err := b.uploadManifestPages(ctx, "tok", fx.pages); err != nil {
		t.Fatalf("upload pages: %v", err)
	}
	root, err := b.bytes.UploadManifest(ctx, "tok", fx.manifest)
	if err != nil {
		t.Fatalf("upload root: %v", err)
	}
	if root != fx.hashHex {
		t.Fatalf("root stored as %s, the seal computed %s", root, fx.hashHex)
	}

	want := map[string]bool{}
	for _, p := range fx.pages {
		sum := blake3.Sum256(p)
		if h := hex.EncodeToString(sum[:]); h != heldHex {
			want[h] = true
		}
	}
	seen := map[string]bool{}
	for i, entry := range nest.posted {
		kind, h, _ := strings.Cut(entry, ":")
		switch kind {
		case "page":
			if seen[h] {
				t.Fatalf("page %s posted twice", h)
			}
			if h == heldHex {
				t.Fatalf("page %s the nest already held was posted", h)
			}
			seen[h] = true
		case "manifest":
			if i != len(nest.posted)-1 {
				t.Fatalf("the root went up before a page: %v", nest.posted)
			}
		}
	}
	if len(seen) != len(want) {
		t.Fatalf("posted %d pages, want the %d the nest lacked", len(seen), len(want))
	}

	version := fx.version
	file := &wsrpc.WebdavFile{Path: "docs/large.bin", ManifestHash: fx.hashHex, ContentKeyVersion: &version}
	served := faunaFfi.FfiServedSetKeys{SetName: "docs", Keys: fx.keys}
	got, err := b.readFileBytes(ctx, served, file)
	if err != nil {
		t.Fatalf("read the paged file back: %v", err)
	}
	if !bytes.Equal(got, fx.body) {
		t.Fatalf("read %d bytes, want the %d written", len(got), len(fx.body))
	}
}

// The nest answering a page under another hash than the seal computed is
// refused — the root would name a page the devices cannot find.
func TestPagedPutRefusesAPageStoredUnderAnotherHash(t *testing.T) {
	fx := loadPagedFixture(t)
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/api/v1/chunks/check":
			var req struct {
				Hashes []string `json:"hashes"`
			}
			_ = json.NewDecoder(r.Body).Decode(&req)
			_ = json.NewEncoder(w).Encode(map[string][]string{"missing": req.Hashes})
		case "/api/v1/manifest-pages":
			w.WriteHeader(http.StatusCreated)
			_ = json.NewEncoder(w).Encode(map[string]string{"hash": strings.Repeat("00", 32)})
		default:
			http.NotFound(w, r)
		}
	}))
	defer srv.Close()
	b := NewBackend(nil, nil, byteplane.New(srv.URL, srv.Client()))
	err := b.uploadManifestPages(context.Background(), "tok", fx.pages)
	if err == nil || !strings.Contains(err.Error(), "the seal computed") {
		t.Fatalf("want a hash-disagreement refusal, got %v", err)
	}
}

// A paged GET walks the pages through the FFI; a ranged GET across a page
// boundary — what emersion serves through http.ServeContent over the opened
// file — returns the right bytes; a page the nest serves wrong is refused.
func TestPagedGetWalksPagesAndServesARangeAcrossABoundary(t *testing.T) {
	fx := loadPagedFixture(t)
	nest := newFakeBytePlane()
	nest.blobs[fx.hashHex] = fx.manifest
	for _, p := range fx.pages {
		sum := blake3.Sum256(p)
		nest.blobs[hex.EncodeToString(sum[:])] = p
	}
	for k, body := range fx.chunks {
		nest.blobs[k] = body
	}
	srv := httptest.NewServer(nest)
	defer srv.Close()
	b := NewBackend(nil, nil, byteplane.New(srv.URL, srv.Client()))

	version := fx.version
	file := &wsrpc.WebdavFile{Path: "docs/large.bin", ManifestHash: fx.hashHex, ContentKeyVersion: &version}
	served := faunaFfi.FfiServedSetKeys{SetName: "docs", Keys: fx.keys}
	data, err := b.readFileBytes(context.Background(), served, file)
	if err != nil {
		t.Fatalf("read paged file: %v", err)
	}
	if !bytes.Equal(data, fx.body) {
		t.Fatalf("read %d bytes, want %d", len(data), len(fx.body))
	}

	// A range straddling the end of the first page.
	boundary := int(fx.pageLens[0])
	start, end := boundary-1000, boundary+1000
	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodGet, "/docs/large.bin", nil)
	req.Header.Set("Range", fmt.Sprintf("bytes=%d-%d", start, end-1))
	http.ServeContent(rec, req, "large.bin", time.Time{}, readSeekCloser{bytes.NewReader(data)})
	if rec.Code != http.StatusPartialContent {
		t.Fatalf("ranged GET answered %d", rec.Code)
	}
	if got := rec.Body.Bytes(); !bytes.Equal(got, fx.body[start:end]) {
		t.Fatalf("ranged GET returned %d bytes not matching the file's [%d,%d)", len(got), start, end)
	}

	// A page served with another page's bytes fails the walk's check against
	// its root.
	last := len(fx.pages) - 1
	if bytes.Equal(fx.pages[0], fx.pages[last]) {
		t.Fatal("the fixture's first and last pages are equal; the swap below would prove nothing")
	}
	sum := blake3.Sum256(fx.pages[last])
	nest.mu.Lock()
	nest.blobs[hex.EncodeToString(sum[:])] = fx.pages[0]
	nest.mu.Unlock()
	if _, err := b.readFileBytes(context.Background(), served, file); err == nil {
		t.Fatal("a page served wrong was accepted")
	} else if !strings.Contains(err.Error(), "its root names") {
		t.Fatalf("want the walk's page-against-root refusal, got %v", err)
	}
}
