package main

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"encoding/hex"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/keypair"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// TestDryRunPrintsBanner locks the atproto-specific difference from the mail
// banner: this binary serves exactly one role, so the banner prints the fixed
// role=atproto.pds (not the mail bridge's role=<from-nest>). The authoritative
// role still comes from whoami at cold boot — the banner just names the role
// this binary requests.
func TestDryRunPrintsBanner(t *testing.T) {
	var buf bytes.Buffer
	err := run(context.Background(), []string{"fauna-atproto-bridge", "--keypair-file=/tmp/atproto.pds.key", "--dry-run"}, &buf)
	if err != nil {
		t.Fatalf("run: %v", err)
	}
	got := buf.String()
	if !strings.Contains(got, "keypair_file=/tmp/atproto.pds.key") {
		t.Fatalf("want keypair_file in output, got %q", got)
	}
	if !strings.Contains(got, "role=atproto.pds") {
		t.Fatalf("want role=atproto.pds in the banner, got %q", got)
	}
}

// TestPrintPubkeyMintsAndIsStable mirrors the mail bridge: --print-pubkey mints
// the keyfile if absent and prints ONLY its hex Ed25519 pubkey + newline (the
// artifact pipes it to the blessed registry), must not require --nest-endpoint,
// and is idempotent so the enrolled identity is stable across reboots.
func TestPrintPubkeyMintsAndIsStable(t *testing.T) {
	dir := t.TempDir()
	keyPath := filepath.Join(dir, "atproto.pds.key")

	var buf1 bytes.Buffer
	if err := run(context.Background(), []string{"fauna-atproto-bridge", "--keypair-file=" + keyPath, "--print-pubkey"}, &buf1); err != nil {
		t.Fatalf("run --print-pubkey (mint): %v", err)
	}
	out1 := strings.TrimSpace(buf1.String())
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
	kf, err := keypair.LoadOrCreate(keyPath, roleAtprotoPds, "unresolved-bridge")
	if err != nil {
		t.Fatalf("load minted keyfile: %v", err)
	}
	if got := hex.EncodeToString(kf.Ed25519PublicKey()); got != out1 {
		t.Errorf("printed pubkey %q != keyfile pubkey %q", out1, got)
	}

	// Idempotent: a second --print-pubkey loads (not re-mints) and prints the
	// same key — the stability the blessed registry depends on.
	var buf2 bytes.Buffer
	if err := run(context.Background(), []string{"fauna-atproto-bridge", "--keypair-file=" + keyPath, "--print-pubkey"}, &buf2); err != nil {
		t.Fatalf("run --print-pubkey (reload): %v", err)
	}
	if out2 := strings.TrimSpace(buf2.String()); out2 != out1 {
		t.Errorf("second --print-pubkey gave %q, want stable %q", out2, out1)
	}
}

func TestKeypairFileRequired(t *testing.T) {
	var buf bytes.Buffer
	err := run(context.Background(), []string{"fauna-atproto-bridge", "--dry-run"}, &buf)
	if err == nil {
		t.Fatalf("want error for missing --keypair-file, got nil")
	}
}

func TestNestEndpointRequiredOutsideDryRun(t *testing.T) {
	var buf bytes.Buffer
	err := run(context.Background(), []string{"fauna-atproto-bridge", "--keypair-file=/tmp/atproto.pds.key"}, &buf)
	if err == nil {
		t.Fatalf("want error for missing --nest-endpoint outside --dry-run, got nil")
	}
	if !strings.Contains(err.Error(), "nest-endpoint") {
		t.Fatalf("want error about --nest-endpoint, got %v", err)
	}
}

// TestEnrollmentSignsUnderAtprotoRole is the atproto-specific enrollment pin:
// the bridge enrolls with role_hint="atproto.pds" (the nest-side
// BridgeRole::parse target), and the proof-of-possession signature binds that
// exact role. A signature minted under a different role must NOT verify against
// the atproto enrollment message — so a mail keyfile can never masquerade as an
// atproto bridge and vice versa. This exercises the same
// SignEnrollment/EnrollmentSignedMessage primitives the enroll loop uses, under
// the atproto role.
func TestEnrollmentSignsUnderAtprotoRole(t *testing.T) {
	if roleAtprotoPds != "atproto.pds" {
		t.Fatalf("roleAtprotoPds = %q, want atproto.pds (nest BridgeRole::parse target)", roleAtprotoPds)
	}
	dir := t.TempDir()
	kf, err := keypair.LoadOrCreate(filepath.Join(dir, "atproto.pds.key"), roleAtprotoPds, "unresolved-bridge")
	if err != nil {
		t.Fatalf("mint keyfile: %v", err)
	}
	ed := kf.Ed25519PublicKey()
	x := kf.X25519PublicKey()

	sig := wsrpc.SignEnrollment(kf.SigningKey(), roleAtprotoPds, ed, x[:])
	if !ed25519.Verify(ed, wsrpc.EnrollmentSignedMessage(roleAtprotoPds, ed, x[:]), sig) {
		t.Errorf("enrollment signature under role %q does not verify against the single-source message", roleAtprotoPds)
	}
	// The role is bound into the signed message, so a signature under a
	// different role must not verify against the atproto message.
	wrong := wsrpc.SignEnrollment(kf.SigningKey(), "mta", ed, x[:])
	if ed25519.Verify(ed, wsrpc.EnrollmentSignedMessage(roleAtprotoPds, ed, x[:]), wrong) {
		t.Errorf("a signature under role \"mta\" must not verify against the atproto.pds enrollment message")
	}
}

// TestModeFlagRetired is a regression guard for the product invariant a bridge
// discovers its role from its nest service-user enrollment, not argv. The
// atproto bridge never had a --mode flag; reintroducing one is drift back toward
// operator-side configuration and must fail here.
func TestModeFlagRetired(t *testing.T) {
	var buf bytes.Buffer
	err := run(context.Background(), []string{"fauna-atproto-bridge", "--keypair-file=/tmp/atproto.pds.key", "--mode=pds", "--dry-run"}, &buf)
	if err == nil {
		t.Fatalf("want error for unknown --mode flag, got nil")
	}
}

// TestOperatorHatchFlagAbsent locks the slim-clone decision: the mail bridge's
// operator-hatch (MTA/MDA bind addresses, metrics, scan endpoints) is
// deliberately dropped — the atproto S1 skeleton opens no listeners, so it has
// no deployment-topology overrides to accept. An --operator-hatch flag is
// therefore unknown and must error.
func TestOperatorHatchFlagAbsent(t *testing.T) {
	var buf bytes.Buffer
	err := run(context.Background(), []string{"fauna-atproto-bridge", "--keypair-file=/tmp/atproto.pds.key", "--operator-hatch=/tmp/h.toml", "--dry-run"}, &buf)
	if err == nil {
		t.Fatalf("want error for absent --operator-hatch flag, got nil")
	}
}
