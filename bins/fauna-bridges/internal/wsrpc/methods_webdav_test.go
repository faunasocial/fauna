// Tests for the WebDAV config-plane Go mirror (the `WebDAVEnabled` field of
// ConfigSnapshot in methods.go). The files twin of TestConfigSnapshotCardDAVEnabled
// in methods_carddav_test.go — same package, same fixture. WebDAV rides the same
// FetchConfigReply / ConfigSnapshot the CalDAV/CardDAV toggles do
// (docs/goal/behavior/webdav-server.md § Independent enablement).
package wsrpc

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
)

// TestConfigSnapshotWebDAVEnabled decodes the committed reply-fetch-config.cbor
// fixture (Rust-produced, exhaustively populated) through the ConfigSnapshot Go
// mirror and asserts the WebDAV toggle round-trips. The fixture pins
// webdav_enabled=true (non-zero, distinct from the decode default false) so a
// mistyped `cbor:"webdav_enabled"` tag / a missed ConfigSnapshot.WebDAVEnabled
// mapping is caught here (a wrong tag would decode to false).
func TestConfigSnapshotWebDAVEnabled(t *testing.T) {
	b, err := os.ReadFile(filepath.Join("testdata", "reply-fetch-config.cbor"))
	if err != nil {
		t.Fatalf("read fixture: %v (run `cargo run -p fauna-protocol "+
			"--example regen_go_wsrpc_reply_fixtures`)", err)
	}
	snap, err := dagcbor.Unmarshal[ConfigSnapshot](b)
	if err != nil {
		t.Fatalf("decode ConfigSnapshot: %v", err)
	}
	if !snap.WebDAVEnabled {
		t.Errorf("WebDAVEnabled: got false, want true (fixture pins webdav_enabled=true)")
	}
	// Guard against a field swap with the neighbouring CalDAV toggle (pinned
	// false in the fixture) — a positional/tag mix-up would surface here.
	if snap.CalDAVEnabled {
		t.Errorf("CalDAVEnabled: got true, want false (fixture pins caldav_enabled=false)")
	}
}
