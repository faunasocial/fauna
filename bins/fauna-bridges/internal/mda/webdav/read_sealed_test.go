package webdav

import (
	"bytes"
	"context"
	"encoding/hex"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/byteplane"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// A DAV GET opens a sealed-only manifest — the shape every Rust writer emits
// (`mls-group-key-material.md` § M2 *Sealed manifest hashes*): the plaintext
// chunk hashes ride only sealed, so the Go side fetches by store key alone and
// the shared walk opens the hashes under the served set's keys. The fixture
// is the Rust writer's own output (`WebdavSealFile`, the engine's
// `seal_blob`), served over a fake byte plane.
func TestReadFileBytesOpensASealedOnlyManifest(t *testing.T) {
	keys := faunaFfi.FfiFolderContentKeys{
		Current: faunaFfi.FfiContentKeyGeneration{Version: 3, Key: bytes.Repeat([]byte{0x42}, 32), RotatedAt: 1},
	}
	// Large enough for several FastCDC chunks, so the walk is over a list.
	body := bytes.Repeat([]byte("a DAV file sealed by the engine's own writer. "), 600_000)
	sealed, err := faunaFfi.WebdavSealFile(keys, body)
	if err != nil {
		t.Fatalf("seal: %v", err)
	}

	manifest, err := faunaFfi.DeserializeManifest(sealed.ManifestBytes)
	if err != nil {
		t.Fatalf("decode manifest: %v", err)
	}
	if len(manifest.ChunkHashes) != 0 {
		t.Fatalf("the writer left %d plaintext chunk hashes on the wire", len(manifest.ChunkHashes))
	}
	if manifest.StoredHashes == nil || len(*manifest.StoredHashes) < 2 {
		t.Fatalf("want a multi-chunk sealed manifest, got store keys %v", manifest.StoredHashes)
	}

	blobs := map[string][]byte{"/api/v1/manifests/" + hex.EncodeToString(sealed.ManifestHash): sealed.ManifestBytes}
	for _, c := range sealed.Chunks {
		blobs["/api/v1/chunks/"+hex.EncodeToString(c.StoreKey)] = c.Body
	}
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		blob, ok := blobs[r.URL.Path]
		if !ok || r.Method != http.MethodGet {
			http.NotFound(w, r)
			return
		}
		_, _ = w.Write(blob)
	}))
	defer srv.Close()

	b := NewBackend(nil, nil, byteplane.New(srv.URL, srv.Client()))
	version := sealed.ContentKeyVersion
	file := &wsrpc.WebdavFile{
		Path:              "docs/sealed.txt",
		ManifestHash:      hex.EncodeToString(sealed.ManifestHash),
		ContentKeyVersion: &version,
	}
	served := faunaFfi.FfiServedSetKeys{SetName: "docs", Keys: keys}

	got, err := b.readFileBytes(context.Background(), served, file)
	if err != nil {
		t.Fatalf("read a sealed-only manifest: %v", err)
	}
	if !bytes.Equal(got, body) {
		t.Fatalf("read %d bytes, want the %d bytes written", len(got), len(body))
	}

	// A holder of another key cannot open the hashes: fail closed, never
	// fall through to the blanked fields.
	other := faunaFfi.FfiServedSetKeys{SetName: "docs", Keys: faunaFfi.FfiFolderContentKeys{
		Current: faunaFfi.FfiContentKeyGeneration{Version: 3, Key: bytes.Repeat([]byte{0x07}, 32), RotatedAt: 1},
	}}
	if _, err := b.readFileBytes(context.Background(), other, file); err == nil {
		t.Fatal("a wrong key read the sealed file")
	} else if !strings.Contains(err.Error(), "docs/sealed.txt") {
		t.Fatalf("the error should name the file: %v", err)
	}
}
