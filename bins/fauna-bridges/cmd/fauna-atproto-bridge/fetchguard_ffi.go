package main

import (
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/safefetch"
	faunaAtproto "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_bridge_atproto"
)

// ffiFetchGuard is the production safefetch.Guard seam: the SSRF policy for
// every attacker-directed outbound fetch comes from the shared-Rust module
// over the tracked binding. The adapter is pure carriage — see
// internal/safefetch/safefetch.go for why no policy may live on this side of
// it, and libs/fauna-bridge-atproto/src/fetch_guard.rs for the policy itself.
//
// Same binding-package story as ffiAuthorizer: uniffi emits one Go package per
// namespace, so `check_fetch_target` is exported from fauna-bridge-atproto
// (where FetchTarget/FetchTargetVerdict are declared) rather than wrapped in
// fauna-ffi.
type ffiFetchGuard struct{}

func (ffiFetchGuard) CheckFetchTarget(t safefetch.Target) safefetch.Verdict {
	v := faunaAtproto.CheckFetchTarget(faunaAtproto.FetchTarget{
		Scheme:      t.Scheme,
		Host:        t.Host,
		ResolvedIps: t.ResolvedIPs,
	})
	switch d := v.(type) {
	case faunaAtproto.FetchTargetVerdictAllow:
		return safefetch.Verdict{Allow: true}
	case faunaAtproto.FetchTargetVerdictDeny:
		return safefetch.Verdict{Reason: d.Reason}
	default:
		// Unreachable unless the module grows a verdict this build predates —
		// closed world: refuse rather than assume.
		return safefetch.Verdict{Reason: "unrecognized guard verdict"}
	}
}
