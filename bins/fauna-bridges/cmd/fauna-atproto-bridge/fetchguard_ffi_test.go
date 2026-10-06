package main

import (
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/safefetch"
)

// The SSRF policy lives in Rust and is exhaustively tested there
// (libs/fauna-bridge-atproto/src/fetch_guard.rs). What CANNOT be tested there
// is the binding: that the target really crosses the FFI intact and the verdict
// really comes back. This suite is that proof, and — like authz_ffi_test.go —
// it is deliberately the only place a Go test asserts a policy outcome. Every
// other Go test stubs the Guard seam, so no second copy of the policy can grow
// on this side.
//
// It drives the production adapter (ffiFetchGuard), so a regression in the
// generated binding, the record field order, or the enum-variant mapping fails
// here rather than in production.

func TestFfiFetchGuardCarriesTheRealPolicy(t *testing.T) {
	g := ffiFetchGuard{}
	const publicIP = "93.184.216.34"

	for _, tc := range []struct {
		name      string
		target    safefetch.Target
		wantAllow bool
	}{
		{
			"a public https target is allowed",
			safefetch.Target{Scheme: "https", Host: "api.bsky.app", ResolvedIPs: []string{publicIP}},
			true,
		},
		{
			"plaintext http is refused",
			safefetch.Target{Scheme: "http", Host: "api.bsky.app", ResolvedIPs: []string{publicIP}},
			false,
		},
		{
			"an IP-literal host is refused",
			safefetch.Target{Scheme: "https", Host: publicIP, ResolvedIPs: []string{publicIP}},
			false,
		},
		{
			"a public name resolving to cloud IMDS is refused",
			safefetch.Target{Scheme: "https", Host: "evil.example", ResolvedIPs: []string{"169.254.169.254"}},
			false,
		},
		{
			"a public name resolving to loopback is refused",
			safefetch.Target{Scheme: "https", Host: "evil.example", ResolvedIPs: []string{"127.0.0.1"}},
			false,
		},
		{
			// The Vec<String> crossing is what makes this worth asserting over
			// the FFI: losing an element would silently disable the check for it.
			"one internal address among public ones refuses the whole target",
			safefetch.Target{
				Scheme:      "https",
				Host:        "split.example",
				ResolvedIPs: []string{publicIP, "10.0.0.5"},
			},
			false,
		},
		{
			"an empty address set is refused",
			safefetch.Target{Scheme: "https", Host: "api.bsky.app", ResolvedIPs: nil},
			false,
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got := g.CheckFetchTarget(tc.target)
			if got.Allow != tc.wantAllow {
				t.Fatalf("Allow = %v, want %v (reason %q)", got.Allow, tc.wantAllow, got.Reason)
			}
			if !tc.wantAllow && got.Reason == "" {
				t.Fatal("a deny crossed the binding with an empty reason")
			}
		})
	}
}
