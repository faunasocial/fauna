//go:build fauna_e2e_fixtures

// The e2e-flavor half of the fake-directory seam's tests (`go test -tags
// fauna_e2e_fixtures ./internal/atprotoid/`); production twin in
// directory_seam_absent_test.go.
package atprotoid

import "testing"

// TestPLCDirectoryBaseURLOverride locks the test-only env seam's parse: set →
// used, trailing slash trimmed. The production default is pinned untagged in
// directory_test.go, because both flavors share it.
func TestPLCDirectoryBaseURLOverride(t *testing.T) {
	t.Setenv(plcDirectoryURLEnv, "")
	if got := PLCDirectoryBaseURL(); got != DefaultPLCDirectoryURL {
		t.Errorf("empty override = %q, want the default %q", got, DefaultPLCDirectoryURL)
	}
	t.Setenv(plcDirectoryURLEnv, "http://127.0.0.1:9999/")
	if got := PLCDirectoryBaseURL(); got != "http://127.0.0.1:9999" {
		t.Errorf("override = %q, want trailing slash trimmed", got)
	}
}
