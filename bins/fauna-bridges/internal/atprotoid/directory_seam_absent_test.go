//go:build !fauna_e2e_fixtures

// The production-flavor half of the fake-directory seam's tests: the harness
// variable must be INERT with the tag absent, so this sets it and asserts the
// twin never looks. A plain `go test` is the production flavor.
package atprotoid

import "testing"

// Spelled out rather than shared with the tagged file on purpose — see
// resolve_seam_absent_test.go.
const productionInertDirectoryEnv = "FAUNA_ATPROTO_PLC_DIRECTORY_URL"

func TestPLCDirectoryBaseURLIgnoresTheHarnessVariableInProduction(t *testing.T) {
	t.Setenv(productionInertDirectoryEnv, "http://127.0.0.1:9999/")
	if got := PLCDirectoryBaseURL(); got != DefaultPLCDirectoryURL {
		t.Fatalf("production build must return %q whatever the environment says, got %q", DefaultPLCDirectoryURL, got)
	}
}
