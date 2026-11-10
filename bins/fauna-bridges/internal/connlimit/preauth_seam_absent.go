//go:build !fauna_e2e_fixtures

// The production twin of preauth_seam.go: the harness's shorter pre-auth
// deadline does not exist in this build. Convention 15 — the tag is the
// boundary, so a release bridge carries neither the environment read nor the
// variable's name, and the production recipes grep the built artifact for that
// name to prove it (`just mail-bridge-build`).
//
// The signature below is the contract preauth_seam.go implements. Keep them in
// sync.
package connlimit

import "time"

// preAuthDeadline always returns the shipped constant: the environment is
// never consulted here, so setting the harness variable on a release bridge
// changes nothing.
func preAuthDeadline() time.Duration {
	return PreAuthDeadline
}
