package webdav

import (
	"bytes"
	"context"
	"encoding/hex"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// The bearer door (`webdav-server.md` § Key model → *A principal's read*)
// driven end to end through Server.Handler() over a fake nest.

// bearerKeysHeader is the header a principal sends for {current: v3, key
// 0x42…, rotated_at 1} — pinned on the Rust side by fauna-ffi's
// `webdav_decode_folder_keys_header_matches_the_principals_encoding`. A
// fixture, not a secret: its key is the byte 0x42 thirty-two times.
const bearerKeysHeader = "omVwcmlvcoBnY3VycmVudKNja2V5WCBCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQmd2ZXJzaW9uA2pyb3RhdGVkX2F0AQ" // gitleaks:allow

// notesPathHashHex is fauna_core::sync::path_hash("notes.txt"), pinned beside it.
const notesPathHashHex = "a859c1198d69e635a868b337216913df319113c1315cdd69ff8eb460ee5f62de"

var bearerKeys = faunaFfi.FfiFolderContentKeys{
	Current: faunaFfi.FfiContentKeyGeneration{Version: 3, Key: bytes.Repeat([]byte{0x42}, 32), RotatedAt: 1},
}

var folderHash = bytes.Repeat([]byte{0xAB}, 32)

type admitCall struct {
	Token  string   `cbor:"token"`
	Proofs []string `cbor:"dpop_proofs"`
	HTM    string   `cbor:"htm"`
	HTU    string   `cbor:"htu"`
}

// fakeNest answers the two kinds the bearer door's read path sends.
type fakeNest struct {
	mu     sync.Mutex
	admit  wsrpc.WebdavAdmitPrincipalReply
	files  []wsrpc.WebdavFile
	admits []admitCall
	calls  []string
}

func (f *fakeNest) Call(_ context.Context, method string, body any, reply any) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls = append(f.calls, method)
	var out any
	switch method {
	case wsrpc.MethodWebdavAdmitPrincipal:
		var req admitCall
		if err := roundTrip(body, &req); err != nil {
			return err
		}
		f.admits = append(f.admits, req)
		out = f.admit
	case wsrpc.MethodWebdavListFiles:
		var req struct {
			NameHash []byte `cbor:"name_hash"`
		}
		if err := roundTrip(body, &req); err != nil {
			return err
		}
		if !bytes.Equal(req.NameHash, folderHash) {
			return fmt.Errorf("listed by the wrong hash %x", req.NameHash)
		}
		out = map[string]any{"files": f.files}
	default:
		return fmt.Errorf("the bearer door sent %s", method)
	}
	return roundTrip(out, reply)
}

func roundTrip(in, out any) error {
	b, err := cbor.Marshal(in)
	if err != nil {
		return err
	}
	return cbor.Unmarshal(b, out)
}

func (f *fakeNest) set(admit wsrpc.WebdavAdmitPrincipalReply) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.admit = admit
}

func admitted(grantLive, served bool) wsrpc.WebdavAdmitPrincipalReply {
	return wsrpc.WebdavAdmitPrincipalReply{Admitted: &wsrpc.WebdavAdmittedPrincipal{
		ActorID: bytes.Repeat([]byte{0x01}, 32),
		Scopes:  []string{"fauna:folder:read:7"},
		Exp:     4_000_000_000,
		Folders: []wsrpc.WebdavAdmittedFolder{{FolderID: 7, NameHash: folderHash, GrantLive: grantLive, Served: served}},
	}}
}

// bearerFixture is a served folder (id 7) holding one sealed file, notes.txt,
// whose manifest and chunks a fake byte plane serves.
func bearerFixture(t *testing.T) (*fakeNest, http.Handler, []byte) {
	t.Helper()
	body := []byte("a principal reads this over the bearer door")
	sealed, err := faunaFfi.WebdavSealFile(bearerKeys, body)
	if err != nil {
		t.Fatalf("seal: %v", err)
	}
	pathSealed, err := faunaFfi.WebdavSealPath(bearerKeys, "notes.txt")
	if err != nil {
		t.Fatalf("seal path: %v", err)
	}
	pathHash, _ := hex.DecodeString(notesPathHashHex)

	blobs := map[string][]byte{"/api/v1/manifests/" + hex.EncodeToString(sealed.ManifestHash): sealed.ManifestBytes}
	for _, c := range sealed.Chunks {
		blobs["/api/v1/chunks/"+hex.EncodeToString(c.StoreKey)] = c.Body
	}
	plane := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		blob, ok := blobs[r.URL.Path]
		if !ok {
			http.NotFound(w, r)
			return
		}
		_, _ = w.Write(blob)
	}))
	t.Cleanup(plane.Close)

	version := sealed.ContentKeyVersion
	nest := &fakeNest{
		admit: admitted(true, true),
		files: []wsrpc.WebdavFile{{
			ManifestHash:      hex.EncodeToString(sealed.ManifestHash),
			SizeBytes:         int64(len(body)),
			UpdatedAt:         1_700_000_000_000,
			ContentKeyVersion: &version,
			PathSealed:        pathSealed,
			PathHash:          pathHash,
		}},
	}
	srv := NewServer(ServerConfig{NestBaseURL: plane.URL, NestHTTPClient: plane.Client()}, nest)
	return nest, srv.Handler(), body
}

func bearerRequest(method, path string, keys bool) *http.Request {
	r := httptest.NewRequest(method, path, nil)
	r.Host = "mda.example"
	r.Header.Set("Authorization", "DPoP the-token")
	r.Header.Set("DPoP", "the-proof")
	if keys {
		r.Header.Set(folderKeysHeader, bearerKeysHeader)
	}
	if method == "PROPFIND" {
		r.Header.Set("Depth", "1")
	}
	return r
}

func serve(h http.Handler, r *http.Request) *httptest.ResponseRecorder {
	w := httptest.NewRecorder()
	h.ServeHTTP(w, r)
	return w
}

func TestBearerAdmittedReadListsAndOpens(t *testing.T) {
	nest, h, body := bearerFixture(t)

	root := serve(h, bearerRequest("PROPFIND", "/webdav/u/", false))
	if root.Code != http.StatusMultiStatus || !strings.Contains(root.Body.String(), "<href>/webdav/u/7</href>") {
		t.Fatalf("root PROPFIND = %d %s, want the folder by its id", root.Code, root.Body)
	}

	list := serve(h, bearerRequest("PROPFIND", "/webdav/u/7/", true))
	if list.Code != http.StatusMultiStatus || !strings.Contains(list.Body.String(), "/webdav/u/7/notes.txt") {
		t.Fatalf("folder PROPFIND = %d %s, want notes.txt", list.Code, list.Body)
	}

	get := serve(h, bearerRequest(http.MethodGet, "/webdav/u/7/notes.txt", true))
	if get.Code != http.StatusOK {
		t.Fatalf("GET = %d %s", get.Code, get.Body)
	}
	if got, _ := io.ReadAll(get.Body); !bytes.Equal(got, body) {
		t.Fatalf("GET served %q, want %q", got, body)
	}

	last := nest.admits[len(nest.admits)-1]
	want := admitCall{Token: "the-token", Proofs: []string{"the-proof"}, HTM: "GET", HTU: "https://mda.example/webdav/u/7/notes.txt"}
	if fmt.Sprint(last) != fmt.Sprint(want) {
		t.Fatalf("relayed %+v, want %+v", last, want)
	}
	if len(nest.admits) != 3 {
		t.Fatalf("%d admissions for 3 requests: every request is admitted by the nest", len(nest.admits))
	}
}

func TestBearerRefusalRelaysTheChallenge(t *testing.T) {
	nest, h, _ := bearerFixture(t)
	challenge, nonce := `DPoP error="use_dpop_nonce"`, "nonce-1"
	nest.set(wsrpc.WebdavAdmitPrincipalReply{Status: 401, WWWAuthenticate: &challenge, DPoPNonce: &nonce})

	w := serve(h, bearerRequest("PROPFIND", "/webdav/u/7/", true))
	if w.Code != http.StatusUnauthorized {
		t.Fatalf("status %d, want 401", w.Code)
	}
	if got := w.Header().Get("WWW-Authenticate"); got != challenge {
		t.Fatalf("WWW-Authenticate %q, want %q", got, challenge)
	}
	if got := w.Header().Get("DPoP-Nonce"); got != nonce {
		t.Fatalf("DPoP-Nonce %q, want %q", got, nonce)
	}
}

// A revoke reaches the very next request: the door caches no admission.
func TestBearerRevokeEndsTheNextRead(t *testing.T) {
	nest, h, _ := bearerFixture(t)
	if w := serve(h, bearerRequest("PROPFIND", "/webdav/u/7/", true)); w.Code != http.StatusMultiStatus {
		t.Fatalf("admitted read = %d", w.Code)
	}

	nest.set(admitted(false, true)) // the folder grant revoked
	if w := serve(h, bearerRequest("PROPFIND", "/webdav/u/7/", true)); w.Code != http.StatusNotFound {
		t.Fatalf("read after the grant's revoke = %d, want 404", w.Code)
	}
	nest.set(admitted(true, false)) // the folder unserved
	if w := serve(h, bearerRequest(http.MethodGet, "/webdav/u/7/notes.txt", true)); w.Code != http.StatusNotFound {
		t.Fatalf("read after the unserve = %d, want 404", w.Code)
	}

	invalid := `DPoP error="invalid_token"`
	nest.set(wsrpc.WebdavAdmitPrincipalReply{Status: 401, WWWAuthenticate: &invalid})
	if w := serve(h, bearerRequest("PROPFIND", "/webdav/u/7/", true)); w.Code != http.StatusUnauthorized {
		t.Fatalf("read after the principal's revoke = %d, want 401", w.Code)
	}
}

func TestBearerWritesAreForbiddenBeforeAdmission(t *testing.T) {
	nest, h, _ := bearerFixture(t)
	for _, m := range []string{http.MethodPut, http.MethodDelete, "MKCOL", "MOVE", "COPY", "PROPPATCH", http.MethodPost} {
		if w := serve(h, bearerRequest(m, "/webdav/u/7/notes.txt", true)); w.Code != http.StatusForbidden {
			t.Fatalf("%s = %d, want 403", m, w.Code)
		}
	}
	if len(nest.calls) != 0 {
		t.Fatalf("a write reached the nest: %v", nest.calls)
	}
}

func TestBearerMissingOrBadKeysListNothingAndOpenNothing(t *testing.T) {
	_, h, _ := bearerFixture(t)
	for name, mutate := range map[string]func(*http.Request){
		"absent":      func(*http.Request) {},
		"unparseable": func(r *http.Request) { r.Header.Set(folderKeysHeader, "bm90IGtleXM") },
	} {
		list := bearerRequest("PROPFIND", "/webdav/u/7/", false)
		mutate(list)
		w := serve(h, list)
		if w.Code != http.StatusMultiStatus || strings.Contains(w.Body.String(), "notes.txt") {
			t.Fatalf("%s keys: PROPFIND = %d %s, want an empty listing", name, w.Code, w.Body)
		}
		get := bearerRequest(http.MethodGet, "/webdav/u/7/notes.txt", false)
		mutate(get)
		if w := serve(h, get); w.Code != http.StatusNotFound {
			t.Fatalf("%s keys: GET = %d, want 404", name, w.Code)
		}
	}
}

func TestBearerFolderOutsideTheScopesIs404(t *testing.T) {
	_, h, _ := bearerFixture(t)
	if w := serve(h, bearerRequest("PROPFIND", "/webdav/u/8/", true)); w.Code != http.StatusNotFound {
		t.Fatalf("PROPFIND outside the scopes = %d, want 404", w.Code)
	}
	if w := serve(h, bearerRequest(http.MethodGet, "/webdav/u/8/notes.txt", true)); w.Code != http.StatusNotFound {
		t.Fatalf("GET outside the scopes = %d, want 404", w.Code)
	}
}

// A request without a DPoP credential stays on the Basic arm.
func TestBasicRequestsKeepTheBasicArm(t *testing.T) {
	nest, h, _ := bearerFixture(t)
	r := httptest.NewRequest("PROPFIND", "/webdav/u/", nil)
	w := serve(h, r)
	if w.Code != http.StatusUnauthorized || !strings.HasPrefix(w.Header().Get("WWW-Authenticate"), "Basic") {
		t.Fatalf("no credential = %d %q, want the Basic challenge", w.Code, w.Header().Get("WWW-Authenticate"))
	}
	if len(nest.admits) != 0 {
		t.Fatal("a Basic request reached the bearer admission")
	}
}
