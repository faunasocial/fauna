//go:build fauna_e2e_fixtures

// The fake-PLC-directory redirect seam: e2e builds only. Convention 15
// (`docs/goal/architecture/e2e-automation-surface-gating.md` § Implementation
// status today → the Go bridges' leg): compiled out of the release bridge under
// the `fauna_e2e_fixtures` tag, `directory_seam_absent.go` the same-signature
// production twin. The Rust client's reader of the SAME variable
// (`libs/fauna-client-atproto/src/genesis_verify.rs`) has been compile-gated
// since 2026-08-02; until 2026-09-13 the Go bridge honoured it unconditionally,
// so the two halves of one deployment disagreed about whether a shipped binary
// may be pointed at a directory of the launcher's choosing. Which directory is
// trusted decides which DID document — and so which PDS endpoint and which
// `#atproto` signing key — a third-party authority resolves to
// (`internal/atprotolex/document.go`), so this redirect's payload is a forged
// signing key, not "just a URL".
package atprotoid

import (
	"os"
	"strings"
)

// plcDirectoryURLEnv is a TEST-ONLY seam (the FAUNA_BRIDGE_FAKE_DNS_JSON
// precedent): the e2e harness points the bridge at a fake directory. This is
// test-harness IPC, never operator configuration — and since it is compiled
// only into the e2e flavor, a production deployment CANNOT set it: the release
// bridge always uses DefaultPLCDirectoryURL.
const plcDirectoryURLEnv = "FAUNA_ATPROTO_PLC_DIRECTORY_URL"

// PLCDirectoryBaseURL resolves the directory base URL: the hard-coded
// production default, unless the test-only env seam overrides it. The e2e
// flavor's arm — the production twin never reads the environment.
func PLCDirectoryBaseURL() string {
	if v := strings.TrimSpace(os.Getenv(plcDirectoryURLEnv)); v != "" {
		return strings.TrimRight(v, "/")
	}
	return DefaultPLCDirectoryURL
}
