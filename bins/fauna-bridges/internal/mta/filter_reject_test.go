package mta

import (
	"strings"
	"testing"
)

// TestSanitizeRejectReason pins the SMTP-safety contract for a user-authored
// filter `Reject` reason placed on a 550 response line (smtp-server.md § Email
// filter rules): no control bytes survive (CRLF-injection guard), runs of space
// collapse to one, the result is length-capped, and a blank reason falls back to
// a generic message.
func TestSanitizeRejectReason(t *testing.T) {
	const fallback = "Message rejected by recipient mail policy"
	cases := []struct {
		name string
		in   string
		want string
	}{
		{"plain", "Not accepting mail from you", "Not accepting mail from you"},
		{"trims", "  spam, go away  ", "spam, go away"},
		{"empty", "", fallback},
		{"blankOnly", "   \t\r\n  ", fallback},
		// CRLF-injection attempt: the injected SMTP verb must not survive as a
		// new line — CR/LF become spaces and collapse.
		{
			"crlfInjection",
			"rejected\r\n250 OK\r\nMAIL FROM:<evil@x>",
			"rejected 250 OK MAIL FROM:<evil@x>",
		},
		{"controlChars", "a\x00b\x07c\x1fd\x7fe", "a b c d e"},
		{"collapseSpaces", "too     many    spaces", "too many spaces"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := sanitizeRejectReason(tc.in)
			if got != tc.want {
				t.Fatalf("sanitizeRejectReason(%q) = %q, want %q", tc.in, got, tc.want)
			}
			if strings.ContainsAny(got, "\r\n") {
				t.Fatalf("sanitizeRejectReason(%q) leaked CR/LF: %q", tc.in, got)
			}
		})
	}
}

// TestSanitizeRejectReasonLengthCap caps the wire length and never splits a
// multi-byte rune (rune-aware truncation).
func TestSanitizeRejectReasonLengthCap(t *testing.T) {
	const maxRunes = 200
	got := sanitizeRejectReason(strings.Repeat("x", 500))
	if n := len([]rune(got)); n > maxRunes {
		t.Fatalf("ASCII reason not capped: %d runes (max %d)", n, maxRunes)
	}

	// 300 two-byte runes (é). The cap must land on a rune boundary, so the
	// output must still be valid UTF-8 with no replacement char.
	multibyte := strings.Repeat("é", 300)
	out := sanitizeRejectReason(multibyte)
	if n := len([]rune(out)); n > maxRunes {
		t.Fatalf("multibyte reason not capped: %d runes (max %d)", n, maxRunes)
	}
	if strings.ContainsRune(out, '�') {
		t.Fatalf("truncation split a multi-byte rune: %q", out)
	}
}
