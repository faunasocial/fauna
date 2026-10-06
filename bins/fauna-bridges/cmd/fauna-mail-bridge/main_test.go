package main

import (
	"bytes"
	"context"
	"encoding/hex"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/config"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/keypair"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

func TestDryRunPrintsBanner(t *testing.T) {
	var buf bytes.Buffer
	err := run(context.Background(), []string{"fauna-mail-bridge", "--keypair-file=/tmp/k.pem", "--dry-run"}, &buf)
	if err != nil {
		t.Fatalf("run: %v", err)
	}
	got := buf.String()
	if !strings.Contains(got, "keypair_file=/tmp/k.pem") {
		t.Fatalf("want keypair_file in output, got %q", got)
	}
	if !strings.Contains(got, "role=<from-nest>") {
		t.Fatalf("want role=<from-nest> in output (role is nest-side), got %q", got)
	}
}

// --print-pubkey mints the keyfile if absent and prints ONLY its hex Ed25519
// pubkey + newline to stdout (the artifact pipes it to the blessed registry). It must not require --nest-endpoint and must be idempotent —
// a second call loads the same keyfile and prints the same pubkey, so the
// enrolled identity is stable across reboots.
func TestPrintPubkeyMintsAndIsStable(t *testing.T) {
	dir := t.TempDir()
	keyPath := filepath.Join(dir, "mta.key")

	var buf1 bytes.Buffer
	if err := run(context.Background(), []string{"fauna-mail-bridge", "--keypair-file=" + keyPath, "--print-pubkey"}, &buf1); err != nil {
		t.Fatalf("run --print-pubkey (mint): %v", err)
	}
	out1 := strings.TrimSpace(buf1.String())
	// Output is exactly the 32-byte pubkey as hex (64 chars), nothing else.
	pub, err := hex.DecodeString(out1)
	if err != nil {
		t.Fatalf("stdout %q is not hex: %v", out1, err)
	}
	if len(pub) != 32 {
		t.Fatalf("printed pubkey is %d bytes, want 32", len(pub))
	}
	if strings.Contains(out1, "keypair_file=") {
		t.Errorf("stdout leaked the startup banner: %q", out1)
	}

	// The keyfile must now exist, 0600, and its pubkey must match what we printed.
	info, err := os.Stat(keyPath)
	if err != nil {
		t.Fatalf("keyfile not minted: %v", err)
	}
	// Assert what the platform can express, as internal/keypair's own mint test
	// does: Windows synthesizes Mode() from the read-only attribute alone (0666
	// or 0444, never 0600) and protects the keyfile by the data dir's ACL.
	if runtime.GOOS == "windows" {
		if perm := info.Mode().Perm(); perm&0o400 == 0 {
			t.Errorf("keyfile mode = %o, want at least owner-readable", perm)
		}
	} else if perm := info.Mode().Perm(); perm != 0o600 {
		t.Errorf("keyfile mode = %o, want 0600", perm)
	}
	kf, err := keypair.LoadOrCreate(keyPath, "mta", "unresolved-bridge")
	if err != nil {
		t.Fatalf("load minted keyfile: %v", err)
	}
	if got := hex.EncodeToString(kf.Ed25519PublicKey()); got != out1 {
		t.Errorf("printed pubkey %q != keyfile pubkey %q", out1, got)
	}

	// Idempotent: a second --print-pubkey loads (not re-mints) and prints the
	// same key — the stability the blessed registry depends on.
	var buf2 bytes.Buffer
	if err := run(context.Background(), []string{"fauna-mail-bridge", "--keypair-file=" + keyPath, "--print-pubkey"}, &buf2); err != nil {
		t.Fatalf("run --print-pubkey (reload): %v", err)
	}
	if out2 := strings.TrimSpace(buf2.String()); out2 != out1 {
		t.Errorf("second --print-pubkey gave %q, want stable %q", out2, out1)
	}
}

func TestKeypairFileRequired(t *testing.T) {
	var buf bytes.Buffer
	err := run(context.Background(), []string{"fauna-mail-bridge", "--dry-run"}, &buf)
	if err == nil {
		t.Fatalf("want error for missing --keypair-file, got nil")
	}
}

func TestNestEndpointRequiredOutsideDryRun(t *testing.T) {
	var buf bytes.Buffer
	err := run(context.Background(), []string{"fauna-mail-bridge", "--keypair-file=/tmp/k.pem"}, &buf)
	if err == nil {
		t.Fatalf("want error for missing --nest-endpoint outside --dry-run, got nil")
	}
	if !strings.Contains(err.Error(), "nest-endpoint") {
		t.Fatalf("want error about --nest-endpoint, got %v", err)
	}
}

// The TLS-cert fetch-key logic moved to the shared internal/tls package
// (bridgetls.CertFetchDomain) and is tested there
// (internal/tls/fetchdomain_test.go), since the ATProto PDS bridge shares it.

// Regression guard: the --mode={mta,mda} flag was retired — a product
// invariant: a bridge discovers its role from
// its nest service-user enrollment, not argv. Any reintroduction of
// --mode is a drift back toward operator-side configuration and
// should fail this test.
func TestModeFlagRetired(t *testing.T) {
	var buf bytes.Buffer
	err := run(context.Background(), []string{"fauna-mail-bridge", "--keypair-file=/tmp/k.pem", "--mode=mta", "--dry-run"}, &buf)
	if err == nil {
		t.Fatalf("want error for retired --mode flag, got nil")
	}
}

// Regression guard: --config (a TOML config file holding nest-state
// fields) was retired per the same product-invariant rule. Admins
// who want to change a Tier 2/3 knob go through their Fauna app.
// The operator-hatch is a Tier 1 deployment-topology override, not a
// nest-config file.
func TestConfigFlagAbsent(t *testing.T) {
	var buf bytes.Buffer
	err := run(context.Background(), []string{"fauna-mail-bridge", "--keypair-file=/tmp/k.pem", "--config=/tmp/cfg.toml", "--dry-run"}, &buf)
	if err == nil {
		t.Fatalf("want error for --config flag, got nil")
	}
}

// TestDefaultMetricsBindAddrForRole locks the per-role /metrics default
// (Stage-5 Gap D): MTA and MDA run as separate processes on one host, so
// the binary derives a distinct metrics port per role rather than
// diverging the two s6 run-scripts. An unresolved role falls back to the
// MTA default.
func TestDefaultMetricsBindAddrForRole(t *testing.T) {
	cases := map[string]string{
		"mta": "127.0.0.1:9090",
		"mda": "127.0.0.1:9091",
		"":    "127.0.0.1:9090",
	}
	for role, want := range cases {
		if got := defaultMetricsBindAddrForRole(role); got != want {
			t.Errorf("defaultMetricsBindAddrForRole(%q) = %q, want %q", role, got, want)
		}
	}
}

// TestResolveMDAListenAddrsPrivateDefaultsToLoopback locks Slice 3's safe
// default (deployment-home-with-public-relay.md § Plaintext-mode behavior →
// "IMAP/CalDAV listener bind: LAN interface only"): when the nest reports the
// private NAT axis and the operator-hatch leaves the MDA binds empty, the MDA
// defaults to loopback rather than all-interfaces, so a fresh private-paired
// plaintext deployment never silently exposes IMAP/CalDAV on a public interface.
func TestResolveMDAListenAddrsPrivateDefaultsToLoopback(t *testing.T) {
	got, warns := resolveMDAListenAddrs(nil, "private", wsrpc.DefaultCalDAVPort)
	if got.IMAPImplicitTLS != "127.0.0.1:993" {
		t.Errorf("IMAPImplicitTLS = %q, want 127.0.0.1:993", got.IMAPImplicitTLS)
	}
	if got.IMAPStartTLS != "127.0.0.1:143" {
		t.Errorf("IMAPStartTLS = %q, want 127.0.0.1:143", got.IMAPStartTLS)
	}
	// CalDAV's no-hatch default is the admin-set port (default 8443), not :443 —
	// the loopback host default still applies on the private axis.
	if got.CalDAVHTTPS != "127.0.0.1:8443" {
		t.Errorf("CalDAVHTTPS = %q, want 127.0.0.1:8443", got.CalDAVHTTPS)
	}
	if len(warns) != 0 {
		t.Errorf("no explicit bind ⇒ no warning, got %v", warns)
	}
}

// TestResolveMDAListenAddrsPublicDefaultsToAllInterfaces locks that the public
// axis (and an empty node_mode, which is just "not private") keeps the canonical all-interfaces binds — the public nest serves
// IMAP/CalDAV publicly, gated per-actor by the Slice-2 serving toggle.
func TestResolveMDAListenAddrsPublicDefaultsToAllInterfaces(t *testing.T) {
	for _, mode := range []string{"public", ""} {
		got, warns := resolveMDAListenAddrs(nil, mode, wsrpc.DefaultCalDAVPort)
		// IMAP keeps its canonical :993/:143; CalDAV's no-hatch default is the
		// admin-set port (default 8443), bound all-interfaces on the public axis.
		if got.IMAPImplicitTLS != ":993" || got.IMAPStartTLS != ":143" || got.CalDAVHTTPS != ":8443" {
			t.Errorf("mode=%q: got %+v, want :993/:143/:8443", mode, got)
		}
		if len(warns) != 0 {
			t.Errorf("mode=%q: public axis ⇒ no warning, got %v", mode, warns)
		}
	}
}

// TestResolveMDAListenAddrsCalDAVUsesAdminPort locks the any-locator fix
// (caldav-server.md § Network exposure): the CalDAV no-hatch listener binds the
// admin-set caldav_port, not a fixed :443 — so a bare-IP / domainless box serves
// CalDAV directly at <host>:<admin-port> and the admin can move the port from a
// client. A 0 (a non-conforming snapshot) normalizes to the canonical
// default (8443), never an ephemeral :0.
func TestResolveMDAListenAddrsCalDAVUsesAdminPort(t *testing.T) {
	// A distinctive non-default admin port proves the bind uses the passed value.
	gotPub, _ := resolveMDAListenAddrs(nil, "public", 9443)
	if gotPub.CalDAVHTTPS != ":9443" {
		t.Errorf("public admin-port CalDAVHTTPS = %q, want :9443", gotPub.CalDAVHTTPS)
	}
	gotPriv, _ := resolveMDAListenAddrs(nil, "private", 9443)
	if gotPriv.CalDAVHTTPS != "127.0.0.1:9443" {
		t.Errorf("private admin-port CalDAVHTTPS = %q, want 127.0.0.1:9443", gotPriv.CalDAVHTTPS)
	}
	// 0 (no wire field) ⇒ canonical default, never :0.
	gotZero, _ := resolveMDAListenAddrs(nil, "public", 0)
	if gotZero.CalDAVHTTPS != ":8443" {
		t.Errorf("zero admin-port CalDAVHTTPS = %q, want :8443 (default, not :0)", gotZero.CalDAVHTTPS)
	}
}

// TestResolveMDAListenAddrsCalDAVHatchWinsOverAdminPort locks the precedence: an
// explicit operator-hatch caldav_listen_https (the loopback IPC port a domain
// box's SNI router targets, or a desktop supervisor's <iface>:<port>) wins over
// the admin port — the admin port drives only the no-hatch direct listener, so
// the change is purely additive and a router/desktop box is unaffected.
func TestResolveMDAListenAddrsCalDAVHatchWinsOverAdminPort(t *testing.T) {
	hatch := &config.OperatorHatch{CalDAVListenHTTPS: "127.0.0.1:8444"}
	got, warns := resolveMDAListenAddrs(hatch, "private", 9443)
	if got.CalDAVHTTPS != "127.0.0.1:8444" {
		t.Errorf("hatch should win over admin port: CalDAVHTTPS = %q, want 127.0.0.1:8444", got.CalDAVHTTPS)
	}
	if len(warns) != 0 {
		t.Errorf("specific loopback bind is not all-interfaces ⇒ no warning, got %v", warns)
	}
}

// TestResolveMDAListenAddrsCalDAVBindHostUsesAdminPort locks the home2 / home-
// relay fix (caldav-server.md § Network exposure): caldav_bind_host carries ONLY
// the LAN/bare-IP interface (from FAUNA_LAN_BIND_IP) while the admin-set
// caldav_port supplies the number, so CalDAV binds <LAN-IP>:<admin-port>
// directly — router-bypassing and reachable by bare IP — and an admin port
// change still rebinds (no pin). A specific LAN IP is not all-interfaces ⇒ no
// warning even on the private axis (the home box's real shape).
func TestResolveMDAListenAddrsCalDAVBindHostUsesAdminPort(t *testing.T) {
	hatch := &config.OperatorHatch{CalDAVBindHost: "192.168.1.57"}
	got, warns := resolveMDAListenAddrs(hatch, "private", 9443)
	if got.CalDAVHTTPS != "192.168.1.57:9443" {
		t.Errorf("caldav_bind_host + admin port: CalDAVHTTPS = %q, want 192.168.1.57:9443", got.CalDAVHTTPS)
	}
	if len(warns) != 0 {
		t.Errorf("specific LAN IP is not all-interfaces ⇒ no warning, got %v", warns)
	}
	// 0 (a non-conforming snapshot) ⇒ canonical default port, never :0.
	gotZero, _ := resolveMDAListenAddrs(&config.OperatorHatch{CalDAVBindHost: "192.168.1.57"}, "private", 0)
	if gotZero.CalDAVHTTPS != "192.168.1.57:8443" {
		t.Errorf("zero admin-port with bind host: CalDAVHTTPS = %q, want 192.168.1.57:8443", gotZero.CalDAVHTTPS)
	}
}

// TestResolveMDAListenAddrsCalDAVFullPinWinsOverBindHost locks the precedence: a
// full caldav_listen_https pin (a domain box's SNI-router loopback, or a desktop
// supervisor's <iface>:<port>) wins outright over caldav_bind_host — the two are
// never both set in this image, but the order must be deterministic.
func TestResolveMDAListenAddrsCalDAVFullPinWinsOverBindHost(t *testing.T) {
	hatch := &config.OperatorHatch{
		CalDAVListenHTTPS: "127.0.0.1:8444",
		CalDAVBindHost:    "192.168.1.57",
	}
	got, _ := resolveMDAListenAddrs(hatch, "private", 9443)
	if got.CalDAVHTTPS != "127.0.0.1:8444" {
		t.Errorf("full caldav_listen_https pin must win over caldav_bind_host: got %q, want 127.0.0.1:8444", got.CalDAVHTTPS)
	}
}

// TestResolveMDAListenAddrsCalDAVBindHostWarnsOnAllInterfaces locks that a
// caldav_bind_host of an all-interfaces wildcard on the private axis still
// surfaces the loud plaintext-exposure warning (the tier_4 bridge-network
// harness sets 0.0.0.0 to make the published port reachable, but a real private
// box should bind a specific LAN IP).
func TestResolveMDAListenAddrsCalDAVBindHostWarnsOnAllInterfaces(t *testing.T) {
	hatch := &config.OperatorHatch{CalDAVBindHost: "0.0.0.0"}
	got, warns := resolveMDAListenAddrs(hatch, "private", 9443)
	if got.CalDAVHTTPS != "0.0.0.0:9443" {
		t.Errorf("CalDAVHTTPS = %q, want 0.0.0.0:9443", got.CalDAVHTTPS)
	}
	if len(warns) == 0 {
		t.Errorf("0.0.0.0 bind host on private axis should warn")
	}
}

// TestResolveMDAListenAddrsExplicitHatchWins locks that an explicit
// operator-hatch bind (the sanctioned OS-deployment-topology opt-out — the
// installer's real LAN IP) always wins over the axis default, per-listener;
// unset listeners still fall back to the private loopback default. A specific
// LAN IP is not an all-interfaces wildcard, so it raises no warning.
func TestResolveMDAListenAddrsExplicitHatchWins(t *testing.T) {
	hatch := &config.OperatorHatch{IMAPListenImplicitTLS: "192.168.1.10:993"}
	got, warns := resolveMDAListenAddrs(hatch, "private", wsrpc.DefaultCalDAVPort)
	if got.IMAPImplicitTLS != "192.168.1.10:993" {
		t.Errorf("explicit IMAPImplicitTLS not honored: %q", got.IMAPImplicitTLS)
	}
	if got.IMAPStartTLS != "127.0.0.1:143" {
		t.Errorf("unset listener should keep loopback default, got %q", got.IMAPStartTLS)
	}
	if len(warns) != 0 {
		t.Errorf("explicit LAN bind is not all-interfaces ⇒ no warning, got %v", warns)
	}
}

// TestResolveMDAListenAddrsWarnsOnPrivateAllInterfaces locks the
// "Don't bind the private nest's IMAP/CalDAV to all interfaces" guard
// (§ Don't do these): an explicit all-interfaces wildcard on the private axis
// is allowed (deliberate opt-out) but must surface a loud warning.
func TestResolveMDAListenAddrsWarnsOnPrivateAllInterfaces(t *testing.T) {
	for _, bind := range []string{":993", "0.0.0.0:993", "[::]:993"} {
		hatch := &config.OperatorHatch{IMAPListenImplicitTLS: bind}
		got, warns := resolveMDAListenAddrs(hatch, "private", wsrpc.DefaultCalDAVPort)
		if got.IMAPImplicitTLS != bind {
			t.Errorf("explicit bind %q not honored: %q", bind, got.IMAPImplicitTLS)
		}
		if len(warns) == 0 {
			t.Errorf("bind %q on private axis should warn", bind)
		}
	}
}
