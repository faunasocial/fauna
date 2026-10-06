package byteplane

import "testing"

func TestNormalizeHTTPBase(t *testing.T) {
	cases := map[string]string{
		"https://nest.example":  "https://nest.example",
		"https://nest.example/": "https://nest.example",
		"wss://nest.example":    "https://nest.example",
		"ws://127.0.0.1:8443/":  "http://127.0.0.1:8443",
		"http://localhost:9000": "http://localhost:9000",
	}
	for in, want := range cases {
		if got := NormalizeHTTPBase(in); got != want {
			t.Errorf("NormalizeHTTPBase(%q) = %q, want %q", in, got, want)
		}
	}
}

// The Go half of the cross-language content-address contract. The Rust half is
// `libs/fauna-cbor/tests/cid_golden_cross_language.rs`, which pins the same
// string for the same bytes.
//
// This is not a style preference: the nest recomputes blake3(body) on
// `PUT /api/v1/blob/{cid}` and answers 400 `cid_mismatch` when the path
// disagrees, so an encoder drift here would fail every content-index publish
// from the bridge at runtime. Pinned on both sides so it fails in CI instead.
func TestBlobCIDMatchesTheRustGoldenVector(t *testing.T) {
	const want = "bafkr4igwtls73dmy7tjwwwugc6homoskipdjpcdy3fzqbtyvi3pcnpq5zm"
	if got := BlobCID([]byte("fauna index segment golden vector")); got != want {
		t.Errorf("BlobCID = %q, want %q (must match libs/fauna-cbor/tests/cid_golden_cross_language.rs)", got, want)
	}
}

// Shape checks that would catch a hand-assembly slip the single golden vector
// could not localize: the multibase prefix, and the fact that distinct bytes
// get distinct addresses.
func TestBlobCIDShape(t *testing.T) {
	got := BlobCID([]byte("anything"))
	if got == "" || got[0] != 'b' {
		t.Errorf("BlobCID = %q, want a multibase base32-lower ('b') prefix", got)
	}
	// CIDv1 + codec + hash code + length + 32-byte digest = 36 bytes, which is
	// 58 unpadded base32 characters, plus the 1-char multibase prefix.
	if len(got) != 59 {
		t.Errorf("BlobCID = %q (len %d), want len 59 for a 36-byte CID", got, len(got))
	}
	if BlobCID([]byte("a")) == BlobCID([]byte("b")) {
		t.Error("distinct bytes must not share a content address")
	}
}
