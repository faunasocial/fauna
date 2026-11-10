//go:build !fauna_e2e_fixtures

// The production-flavor half of the pre-auth deadline seam's tests: with the
// tag absent the harness variable must be INERT, so this sets it and asserts
// the shipped constant still applies. A plain `go test` (every untagged Go
// gate) is the production flavor, which is what makes this the witness that
// matters.
package connlimit

import "testing"

// Spelled out here rather than shared with the tagged file on purpose: the
// production package must not carry the constant.
const productionInertPreAuthEnv = "FAUNA_MAIL_E2E_PREAUTH_DEADLINE_MS"

func TestPreAuthDeadlineIgnoresTheHarnessVariableInProduction(t *testing.T) {
	t.Setenv(productionInertPreAuthEnv, "1000")
	if got := preAuthDeadline(); got != PreAuthDeadline {
		t.Fatalf("production build must apply the shipped %v whatever the environment says, got %v", PreAuthDeadline, got)
	}
}
