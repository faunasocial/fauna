package webdav

import (
	"errors"
	"net/http"
	"strings"
	"testing"

	"github.com/emersion/go-webdav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

func TestParsePath(t *testing.T) {
	cases := []struct {
		name    string
		wantSet string
		wantRel string
	}{
		{"/webdav/", "", ""},
		{"/webdav/alice", "", ""},
		{"/webdav/alice/", "", ""},
		{"/webdav/alice/docs", "docs", ""},
		{"/webdav/alice/docs/", "docs", ""},
		{"/webdav/alice/docs/report.txt", "docs", "report.txt"},
		{"/webdav/alice/docs/sub/a.txt", "docs", "sub/a.txt"},
		{"/webdav/alice/docs/sub/", "docs", "sub"},
	}
	for _, c := range cases {
		got := parsePath(c.name)
		if got.set != c.wantSet || got.rel != c.wantRel {
			t.Errorf("parsePath(%q) = {set:%q rel:%q}, want {set:%q rel:%q}",
				c.name, got.set, got.rel, c.wantSet, c.wantRel)
		}
	}
}

func TestImmediateChildren(t *testing.T) {
	files := []wsrpc.WebdavFile{
		{Path: "readme.txt"},
		{Path: "photos/a.jpg"},
		{Path: "photos/b.jpg"},
		{Path: "photos/2024/x.jpg"},
		{Path: "docs/notes.md"},
	}

	// Set root: readme.txt (file), photos (dir), docs (dir).
	root := immediateChildren(files, "")
	if len(root) != 3 {
		t.Fatalf("root children = %d, want 3 (%+v)", len(root), root)
	}
	// sorted: docs, photos, readme.txt
	if root[0].name != "docs" || !root[0].isDir {
		t.Errorf("root[0] = %+v, want dir docs", root[0])
	}
	if root[2].name != "readme.txt" || root[2].isDir {
		t.Errorf("root[2] = %+v, want file readme.txt", root[2])
	}

	// photos/: a.jpg (file), b.jpg (file), 2024 (dir).
	photos := immediateChildren(files, "photos")
	if len(photos) != 3 {
		t.Fatalf("photos children = %d, want 3 (%+v)", len(photos), photos)
	}
	byName := map[string]childEntry{}
	for _, c := range photos {
		byName[c.name] = c
	}
	if c, ok := byName["2024"]; !ok || !c.isDir {
		t.Errorf("photos/2024 must be a dir; got %+v", c)
	}
	if c, ok := byName["a.jpg"]; !ok || c.isDir {
		t.Errorf("photos/a.jpg must be a file; got %+v", c)
	}
}

func TestFindFileAndDirPrefix(t *testing.T) {
	files := []wsrpc.WebdavFile{
		{Path: "readme.txt", ManifestHash: "abc"},
		{Path: "photos/a.jpg"},
	}
	if f, ok := findFile(files, "readme.txt"); !ok || f.ManifestHash != "abc" {
		t.Errorf("findFile(readme.txt) = %v, %v; want the abc file", f, ok)
	}
	if _, ok := findFile(files, "photos"); ok {
		t.Error("findFile(photos) must be false — it is a dir prefix, not a file")
	}
	if !isDirPrefix(files, "photos") {
		t.Error("isDirPrefix(photos) must be true")
	}
	if isDirPrefix(files, "readme.txt") {
		t.Error("isDirPrefix(readme.txt) must be false — it is a leaf file")
	}
}

func TestCondStr(t *testing.T) {
	if got := condStr(webdav.ConditionalMatch("")); got != nil {
		t.Errorf("unset ConditionalMatch → %v, want nil", got)
	}
	if got := condStr(webdav.ConditionalMatch("*")); got == nil || *got != "*" {
		t.Errorf("wildcard → %v, want \"*\"", got)
	}
	// An ETag value (emersion stores the raw quoted form; ETag() unquotes it).
	if got := condStr(webdav.ConditionalMatch(`"deadbeef"`)); got == nil || *got != "deadbeef" {
		t.Errorf("etag → %v, want deadbeef", got)
	}
}

func TestUrlJoin(t *testing.T) {
	if got := urlJoin("/webdav/alice/docs", "a.txt"); got != "/webdav/alice/docs/a.txt" {
		t.Errorf("urlJoin no-slash = %q", got)
	}
	if got := urlJoin("/webdav/alice/docs/", "a.txt"); got != "/webdav/alice/docs/a.txt" {
		t.Errorf("urlJoin trailing-slash = %q", got)
	}
}

// serverErr builds a *wsrpc.ServerError carrying a typed RpcError code, the way
// nest's ok=false replies encode it, so mapErr's code branches are exercised.
func serverErr(t *testing.T, code string) error {
	t.Helper()
	payload, err := dagcbor.Marshal(struct {
		Code string `cbor:"code"`
	}{Code: code})
	if err != nil {
		t.Fatalf("encode server error: %v", err)
	}
	return &wsrpc.ServerError{Payload: payload}
}

// mappedStatus reconstructs the HTTP status mapErr encoded. emersion's
// NewHTTPError returns an unexported *internal.HTTPError whose Error() string is
// the status text, so we compare against the known status texts (the exact
// numeric mapping is additionally observed end-to-end by the tier_3 e2e via a
// real HTTP round-trip).
func mappedStatus(t *testing.T, err error) string {
	t.Helper()
	if err == nil {
		t.Fatal("expected a non-nil error")
	}
	return err.Error()
}

func TestMapErr(t *testing.T) {
	if mapErr(nil) != nil {
		t.Error("mapErr(nil) must be nil")
	}

	// A typed nest RpcError is wrapped into the matching HTTP status. emersion's
	// HTTPError.Error() renders as "<code> <status text>: <cause>".
	conflict := mappedStatus(t, mapErr(serverErr(t, "fauna.bridges.conflict")))
	if want := http.StatusText(http.StatusPreconditionFailed); !strings.Contains(conflict, want) {
		t.Errorf("conflict → %q, want it to contain %q (412)", conflict, want)
	}
	notServed := mappedStatus(t, mapErr(serverErr(t, "fauna.bridges.set_not_served")))
	if want := http.StatusText(http.StatusNotFound); !strings.Contains(notServed, want) {
		t.Errorf("set_not_served → %q, want it to contain %q (404)", notServed, want)
	}
	quota := mappedStatus(t, mapErr(serverErr(t, "fauna.sync.storage_quota_exceeded")))
	if want := http.StatusText(http.StatusInsufficientStorage); !strings.Contains(quota, want) {
		t.Errorf("quota → %q, want it to contain %q (507)", quota, want)
	}

	// An untyped error (or an unrecognized code) passes through unchanged so
	// emersion maps it to 500.
	plain := errors.New("boom")
	if mapErr(plain) != plain {
		t.Error("an untyped error must pass through unchanged (500)")
	}
	unknown := serverErr(t, "fauna.bridges.some_new_code")
	if mapErr(unknown) != unknown {
		t.Error("an unrecognized RpcError code must pass through unchanged (500)")
	}
}

// The listing's `updated_at` is the change's `created_at` in epoch
// milliseconds; getlastmodified must be that instant, not a date tens of
// thousands of years out.
func TestFileInfoReadsUpdatedAtAsMilliseconds(t *testing.T) {
	const ms = int64(1_790_000_000_123) // 2026-09-21T…Z
	fi := fileInfo("/webdav/u/docs/a.txt", &wsrpc.WebdavFile{Path: "a.txt", UpdatedAt: ms, SizeBytes: 3})
	if got := fi.ModTime.UnixMilli(); got != ms {
		t.Fatalf("ModTime = %v (%d ms), want %d ms", fi.ModTime, got, ms)
	}
	if y := fi.ModTime.UTC().Year(); y != 2026 {
		t.Fatalf("ModTime year = %d", y)
	}
}

// A served set is matched by its set_name_hash, so a sealed set whose row holds
// no plaintext name is still served; an entry without a hash (a set whose name
// rests plaintext, unstamped) falls back to the name.
func TestServesSetMatchesByHash(t *testing.T) {
	hashOf := func(n string) []byte { return []byte("h:" + n) }
	sets := []wsrpc.WebdavServedSet{
		{Name: "", NameHash: hashOf("photos")},
		{Name: "plain"},
	}
	if !servesSet(sets, "photos", hashOf("photos")) {
		t.Fatal("a blank-named set must be served by its hash")
	}
	if !servesSet(sets, "plain", hashOf("plain")) {
		t.Fatal("a hash-less entry must fall back to the name")
	}
	if servesSet(sets, "other", hashOf("other")) {
		t.Fatal("an unknown set must not be served")
	}
	if servesSet([]wsrpc.WebdavServedSet{{Name: "photos", NameHash: hashOf("x")}}, "photos", hashOf("photos")) {
		t.Fatal("a present hash is authoritative over the name")
	}
}

// The root collection names a blank-named set through the keys blob and drops
// one the blob does not name.
func TestRootSetNamesRendersSealedSetsFromTheBlob(t *testing.T) {
	hashOf := func(n string) []byte { return []byte("h:" + n) }
	sets := []wsrpc.WebdavServedSet{
		{Name: "public", NameHash: hashOf("public")},
		{Name: "", NameHash: hashOf("photos")},
		{Name: "", NameHash: hashOf("unknown")},
		{Name: ""},
	}
	got := rootSetNames(sets, []string{"photos", "public"}, hashOf)
	want := []string{"public", "photos"}
	if len(got) != len(want) || got[0] != want[0] || got[1] != want[1] {
		t.Fatalf("rootSetNames = %v, want %v", got, want)
	}
}
