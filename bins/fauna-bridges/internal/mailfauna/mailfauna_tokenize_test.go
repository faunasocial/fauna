package mailfauna

import (
	"bytes"
	"encoding/binary"
	"testing"
)

// Tokenize is the Go-side wrapper over libs/fauna-mail/src/tokenizer.rs::
// tokenize. Determinism is a hard correctness property — the same input
// produces a byte-identical CanonicalBytes everywhere. These tests pin
// the deterministic shape from the Go consumer's perspective so a
// regen-induced wire-shape drift can't slip in silently.

// Tokenize deduplicates repeated words, drops single-char tokens
// ("a" filtered as `< 2` chars), and emits tokens in alphabetical
// order.
func TestTokenizeDeduplicatesAndSorts(t *testing.T) {
	got := Tokenize("hello hello fauna fauna a x")
	want := []string{"fauna", "hello"}
	if len(got.Tokens) != len(want) {
		t.Fatalf("Tokens: got %d (%v), want %d (%v)", len(got.Tokens), got.Tokens, len(want), want)
	}
	for i, w := range want {
		if got.Tokens[i] != w {
			t.Errorf("Tokens[%d]: got %q, want %q", i, got.Tokens[i], w)
		}
	}
}

// CanonicalBytes is a length-prefixed concatenation: per token,
// `<u32 BE length><utf8 bytes>`. Pin the byte shape from Go so a
// regen-induced layout drift surfaces here.
func TestTokenizeCanonicalBytesShape(t *testing.T) {
	got := Tokenize("hello fauna")
	// Expected tokens (sorted): "fauna", "hello"
	// Bytes: <00 00 00 05>"fauna"<00 00 00 05>"hello"
	var want bytes.Buffer
	for _, tok := range []string{"fauna", "hello"} {
		var lenBuf [4]byte
		binary.BigEndian.PutUint32(lenBuf[:], uint32(len(tok)))
		want.Write(lenBuf[:])
		want.WriteString(tok)
	}
	if !bytes.Equal(got.CanonicalBytes, want.Bytes()) {
		t.Errorf("CanonicalBytes: got %x, want %x", got.CanonicalBytes, want.Bytes())
	}
}

// NFKC normalization: composed and decomposed forms of the same
// Unicode string must produce the same CanonicalBytes. "café" written
// with U+00E9 (composed) vs. "café" with U+0065 + U+0301 (decomposed)
// must collapse onto the same token.
func TestTokenizeNFKCNormalizes(t *testing.T) {
	composed := Tokenize("café")
	decomposed := Tokenize("café")
	if !bytes.Equal(composed.CanonicalBytes, decomposed.CanonicalBytes) {
		t.Errorf("NFKC mismatch: composed=%x decomposed=%x",
			composed.CanonicalBytes, decomposed.CanonicalBytes)
	}
}

// Empty / single-character / non-alphanumeric inputs produce an empty
// token set. Pins the filter rules: < 2 chars and no-alphanumerics
// both drop.
func TestTokenizeFiltersOutShortAndPunctuation(t *testing.T) {
	cases := []string{"", "a", "!!", "1", "a b c"}
	for _, in := range cases {
		got := Tokenize(in)
		if len(got.Tokens) != 0 {
			t.Errorf("input %q: expected empty token set, got %v", in, got.Tokens)
		}
		if len(got.CanonicalBytes) != 0 {
			t.Errorf("input %q: expected empty CanonicalBytes, got %x", in, got.CanonicalBytes)
		}
	}
}
