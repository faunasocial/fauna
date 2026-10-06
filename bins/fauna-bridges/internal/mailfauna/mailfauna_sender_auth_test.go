package mailfauna

import "testing"

// BuildAuthenticatedSenderStamp crosses the real cgo boundary to the shared
// Rust builder (libs/fauna-mail/src/sender_auth.rs, where the value grammar is
// pinned in full). This pins only the Go-side contract the MTA doors rely on:
// a stampable address yields the one lower-cased header line with no trailing
// CRLF, and anything else yields "" — which the doors read as "prepend
// nothing".
func TestBuildAuthenticatedSenderStamp(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name, addr, want string
	}{
		{"valid addr-spec", "alice@example.org", "X-Fauna-Authenticated-Sender: alice@example.org"},
		{"lower-cased and trimmed", "  Alice@Example.ORG ", "X-Fauna-Authenticated-Sender: alice@example.org"},
		{"empty", "", ""},
		{"no at-sign", "alice", ""},
		{"display name form", "Alice <alice@example.org>", ""},
		{"header injection", "alice@example.org\r\nX-Evil: 1", ""},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			if got := BuildAuthenticatedSenderStamp(tc.addr); got != tc.want {
				t.Errorf("BuildAuthenticatedSenderStamp(%q) = %q, want %q", tc.addr, got, tc.want)
			}
		})
	}
}
