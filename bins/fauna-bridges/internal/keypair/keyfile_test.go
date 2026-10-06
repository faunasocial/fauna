// Regenerating testdata/rust-keyfile.cbor: see the
// libs/fauna-mls/src/wrapped_blob/service_user.rs::tests module.
// Add a temporary #[test] #[ignore] write_test_fixture_for_go that
// constructs a deterministic ServiceUserKeyfile (role="mta",
// bridge_id="test-bridge", version=1, ed25519_seed=[0u8;32],
// x25519_priv=[0xFFu8;32], created_at=1_700_000_000), writes
// kf.to_bytes() to this directory, then run:
//
//	cargo test -p fauna-mls --lib write_test_fixture_for_go -- --ignored
//
// Remove the temporary Rust test after the fixture lands. The fixture
// is small + deterministic + checked into git; do NOT regenerate on
// every run.

package keypair

import (
	"bytes"
	"crypto/ed25519"
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
)

// TestGenerateProducesValidKeyfile checks the basic shape contract:
// version == 1, role / bridge_id propagated, both seeds 32 bytes, and
// two consecutive generations produce distinct seeds (a stuck RNG
// would surface as a duplicate here).
func TestGenerateProducesValidKeyfile(t *testing.T) {
	t.Parallel()
	kf, err := Generate("mta", "bridge-1", 1_700_000_000)
	if err != nil {
		t.Fatalf("Generate: %v", err)
	}
	if kf.Version != 1 {
		t.Fatalf("Version = %d, want 1", kf.Version)
	}
	if kf.Role != "mta" {
		t.Fatalf("Role = %q, want %q", kf.Role, "mta")
	}
	if kf.BridgeID != "bridge-1" {
		t.Fatalf("BridgeID = %q, want %q", kf.BridgeID, "bridge-1")
	}
	if got := len(kf.Ed25519Seed); got != SeedLen {
		t.Fatalf("Ed25519Seed length = %d, want %d", got, SeedLen)
	}
	if got := len(kf.X25519Priv); got != SeedLen {
		t.Fatalf("X25519Priv length = %d, want %d", got, SeedLen)
	}
	if kf.CreatedAt != 1_700_000_000 {
		t.Fatalf("CreatedAt = %d, want 1_700_000_000", kf.CreatedAt)
	}

	kf2, err := Generate("mta", "bridge-1", 1_700_000_000)
	if err != nil {
		t.Fatalf("Generate (second): %v", err)
	}
	if bytes.Equal(kf.Ed25519Seed, kf2.Ed25519Seed) {
		t.Fatalf("two Generate calls produced identical Ed25519Seed; RNG is stuck")
	}
	if bytes.Equal(kf.X25519Priv, kf2.X25519Priv) {
		t.Fatalf("two Generate calls produced identical X25519Priv; RNG is stuck")
	}
}

// TestLoadOrCreateGeneratesOnFirstRun exercises the cold-start path:
// fresh tmpdir, no keyfile, LoadOrCreate must generate + persist with
// mode 0600. A second LoadOrCreate must read back the same secrets.
func TestLoadOrCreateGeneratesOnFirstRun(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	path := filepath.Join(dir, "mta.key")

	kf, err := LoadOrCreate(path, "mta", "first-run-bridge")
	if err != nil {
		t.Fatalf("LoadOrCreate (cold): %v", err)
	}
	if kf.Role != "mta" || kf.BridgeID != "first-run-bridge" {
		t.Fatalf("LoadOrCreate produced wrong identity: role=%q bridge_id=%q", kf.Role, kf.BridgeID)
	}

	// The file must exist, and must not be group/other-readable. The exact
	// 0o600 is the POSIX spelling of that; Windows has no such spelling —
	// os.Stat there synthesizes Mode() from the single read-only attribute
	// (0666 writable / 0444 read-only), which is why LoadOrCreate skips its
	// own 0o077 gate on Windows and relies on the ACL of the per-user data
	// dir instead. Assert what the platform can actually express.
	info, err := os.Stat(path)
	if err != nil {
		t.Fatalf("stat keyfile: %v", err)
	}
	if runtime.GOOS == "windows" {
		if perm := info.Mode().Perm(); perm&0o400 == 0 {
			t.Fatalf("keyfile mode = %o, want at least owner-readable", perm)
		}
	} else if perm := info.Mode().Perm(); perm != 0o600 {
		t.Fatalf("keyfile mode = %o, want 0o600", perm)
	}

	// SigningKey and X25519PublicKey must produce well-formed values.
	sk := kf.SigningKey()
	if len(sk) != ed25519.PrivateKeySize {
		t.Fatalf("SigningKey size = %d, want %d", len(sk), ed25519.PrivateKeySize)
	}
	pk := kf.Ed25519PublicKey()
	if len(pk) != ed25519.PublicKeySize {
		t.Fatalf("Ed25519PublicKey size = %d, want %d", len(pk), ed25519.PublicKeySize)
	}
	xpub := kf.X25519PublicKey()
	if xpub == ([SeedLen]byte{}) {
		t.Fatalf("X25519PublicKey is all-zero, suggests derivation failed silently")
	}

	// Second LoadOrCreate must return the same pubkeys.
	kf2, err := LoadOrCreate(path, "mta", "first-run-bridge")
	if err != nil {
		t.Fatalf("LoadOrCreate (warm): %v", err)
	}
	if !bytes.Equal(kf.Ed25519PublicKey(), kf2.Ed25519PublicKey()) {
		t.Fatalf("Ed25519 pubkey changed across LoadOrCreate round-trip")
	}
	if kf.X25519PublicKey() != kf2.X25519PublicKey() {
		t.Fatalf("X25519 pubkey changed across LoadOrCreate round-trip")
	}
}

// TestLoadOrCreateRoundtrip checks that every keyfile field survives a
// write-then-read cycle. Bypasses the auto-generate path by writing
// the keyfile explicitly first.
func TestLoadOrCreateRoundtrip(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	path := filepath.Join(dir, "mta.key")

	original, err := Generate("mda", "round-trip-bridge", 1_700_000_123)
	if err != nil {
		t.Fatalf("Generate: %v", err)
	}
	if err := writeAtomic(path, original); err != nil {
		t.Fatalf("writeAtomic: %v", err)
	}

	loaded, err := LoadOrCreate(path, "ignored", "ignored")
	if err != nil {
		t.Fatalf("LoadOrCreate: %v", err)
	}
	if loaded.Version != original.Version {
		t.Fatalf("Version: got %d want %d", loaded.Version, original.Version)
	}
	if loaded.Role != original.Role {
		t.Fatalf("Role: got %q want %q", loaded.Role, original.Role)
	}
	if loaded.BridgeID != original.BridgeID {
		t.Fatalf("BridgeID: got %q want %q", loaded.BridgeID, original.BridgeID)
	}
	if !bytes.Equal(loaded.Ed25519Seed, original.Ed25519Seed) {
		t.Fatalf("Ed25519Seed differs across round-trip")
	}
	if !bytes.Equal(loaded.X25519Priv, original.X25519Priv) {
		t.Fatalf("X25519Priv differs across round-trip")
	}
	if loaded.CreatedAt != original.CreatedAt {
		t.Fatalf("CreatedAt: got %d want %d", loaded.CreatedAt, original.CreatedAt)
	}
}

// TestRefusesWorldReadable confirms LoadOrCreate refuses any keyfile
// whose mode allows group or other access. The keyfile holds two
// long-lived secrets; a loose-perms file is a configuration error
// worth surfacing loudly.
func TestRefusesWorldReadable(t *testing.T) {
	t.Parallel()
	if runtime.GOOS == "windows" {
		// The gate this asserts does not exist here, deliberately:
		// LoadOrCreate skips the 0o077 check on Windows because NTFS security
		// is ACL-based and os.Stat can only ever report 0666 or 0444, so the
		// check would reject every keyfile and strand the desktop-native MDA.
		// The protection is the ACL of the per-user %LOCALAPPDATA% / service
		// data dir. Asserting a refusal here would be asserting a bug.
		t.Skip("mode-based permission gate is POSIX-only; Windows relies on the data dir's ACL (see LoadOrCreate)")
	}
	dir := t.TempDir()
	path := filepath.Join(dir, "mta.key")

	if _, err := LoadOrCreate(path, "mta", "perms-test"); err != nil {
		t.Fatalf("LoadOrCreate (setup): %v", err)
	}
	if err := os.Chmod(path, 0o644); err != nil {
		t.Fatalf("chmod 0644: %v", err)
	}

	_, err := LoadOrCreate(path, "mta", "perms-test")
	if err == nil {
		t.Fatalf("LoadOrCreate of mode-0644 keyfile succeeded; expected an error")
	}
	msg := err.Error()
	if !strings.Contains(msg, "mode") && !strings.Contains(msg, "permissive") {
		t.Fatalf("error message does not mention mode/permissions: %v", err)
	}
}

// TestRefusesWrongVersion confirms Unmarshal / LoadOrCreate rejects any
// keyfile whose version != 1. Mirrors the Rust side's behavior at
// libs/fauna-mls/src/wrapped_blob/service_user.rs:77-82.
func TestRefusesWrongVersion(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	path := filepath.Join(dir, "mta.key")

	// Hand-encode a keyfile with v=2 via the same codec — the version
	// check is on the decoded value, not on the wire bytes.
	bad := Keyfile{
		Version:     2,
		Role:        "mta",
		BridgeID:    "version-test",
		Ed25519Seed: make([]byte, SeedLen),
		X25519Priv:  make([]byte, SeedLen),
		CreatedAt:   1,
	}
	rawBytes, err := dagcbor.Marshal(&bad)
	if err != nil {
		t.Fatalf("dagcbor.Marshal: %v", err)
	}
	if err := os.WriteFile(path, rawBytes, 0o600); err != nil {
		t.Fatalf("write bad keyfile: %v", err)
	}

	_, err = LoadOrCreate(path, "mta", "version-test")
	if err == nil {
		t.Fatalf("LoadOrCreate accepted v=2 keyfile; want error")
	}
	if !strings.Contains(err.Error(), "unsupported version") {
		t.Fatalf("error does not mention unsupported version: %v", err)
	}
}

// TestRefusesWrongSeedLength confirms Unmarshal rejects a keyfile whose
// ed25519_seed is the wrong size. Mirrors the Rust side's check at
// service_user.rs:83-91.
func TestRefusesWrongSeedLength(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	path := filepath.Join(dir, "mta.key")

	bad := Keyfile{
		Version:     1,
		Role:        "mta",
		BridgeID:    "len-test",
		Ed25519Seed: make([]byte, 16), // wrong: want 32
		X25519Priv:  make([]byte, SeedLen),
		CreatedAt:   1,
	}
	// Marshal directly via the codec — Keyfile.Marshal() would refuse
	// this in-memory keyfile before encoding (its validate() catches
	// the same condition), and we need actual bytes on disk to test
	// the load path.
	rawBytes, err := dagcbor.Marshal(&bad)
	if err != nil {
		t.Fatalf("dagcbor.Marshal: %v", err)
	}
	if err := os.WriteFile(path, rawBytes, 0o600); err != nil {
		t.Fatalf("write bad keyfile: %v", err)
	}

	_, err = LoadOrCreate(path, "mta", "len-test")
	if err == nil {
		t.Fatalf("LoadOrCreate accepted 16-byte ed25519_seed; want error")
	}
	if !strings.Contains(err.Error(), "ed25519_seed") || !strings.Contains(err.Error(), "32") {
		t.Fatalf("error does not name the wrong-length field: %v", err)
	}
}

// TestCrossLanguageFixtureRoundtrip decodes a fixture produced by the
// Rust side (libs/fauna-mls::ServiceUserKeyfile::to_bytes on a known
// input), asserts every field matches the documented values, and
// confirms re-encoding + decoding produces the same struct. This is
// the load-bearing test for "Rust-written keyfile loads in Go."
func TestCrossLanguageFixtureRoundtrip(t *testing.T) {
	t.Parallel()
	raw, err := os.ReadFile("testdata/rust-keyfile.cbor")
	if err != nil {
		t.Fatalf("read fixture: %v (regenerate per the comment at the top of this file)", err)
	}

	kf, err := Unmarshal(raw)
	if err != nil {
		t.Fatalf("Unmarshal Rust-written fixture: %v", err)
	}
	if kf.Version != 1 {
		t.Fatalf("fixture Version = %d, want 1", kf.Version)
	}
	if kf.Role != "mta" {
		t.Fatalf("fixture Role = %q, want %q", kf.Role, "mta")
	}
	if kf.BridgeID != "test-bridge" {
		t.Fatalf("fixture BridgeID = %q, want %q", kf.BridgeID, "test-bridge")
	}
	wantSeed := bytes.Repeat([]byte{0x00}, 32)
	if !bytes.Equal(kf.Ed25519Seed, wantSeed) {
		t.Fatalf("fixture Ed25519Seed = %x, want %x", kf.Ed25519Seed, wantSeed)
	}
	wantX := bytes.Repeat([]byte{0xFF}, 32)
	if !bytes.Equal(kf.X25519Priv, wantX) {
		t.Fatalf("fixture X25519Priv = %x, want %x", kf.X25519Priv, wantX)
	}
	if kf.CreatedAt != 1_700_000_000 {
		t.Fatalf("fixture CreatedAt = %d, want 1_700_000_000", kf.CreatedAt)
	}

	// Re-encode (Go-canonical, length-first key sort) and decode again.
	// Wire bytes will NOT match the Rust fixture (the package doc
	// comment explains why); only the decoded fields need to match.
	reEncoded, err := kf.Marshal()
	if err != nil {
		t.Fatalf("Re-marshal: %v", err)
	}
	kf2, err := Unmarshal(reEncoded)
	if err != nil {
		t.Fatalf("Re-Unmarshal: %v", err)
	}
	if kf2.Role != kf.Role || kf2.BridgeID != kf.BridgeID || !bytes.Equal(kf2.Ed25519Seed, kf.Ed25519Seed) || !bytes.Equal(kf2.X25519Priv, kf.X25519Priv) || kf2.CreatedAt != kf.CreatedAt {
		t.Fatalf("Re-encode + decode lost data:\n got %+v\n want %+v", kf2, kf)
	}
}

// TestX25519PublicDerivation pins X25519PublicKey() against a hand-derived
// known answer. With a fixed all-zero private the clamped scalar is
// `0x40` followed by 30 zeros and `0x40`, but rather than hand-derive
// we just check internal consistency: deriving the pubkey from a
// freshly-generated keyfile and re-deriving from the same bytes
// produces the same value. (A full known-answer test would belong in
// a crypto crate; this layer just needs to confirm we're calling the
// right function on the right bytes.)
func TestX25519PublicDerivation(t *testing.T) {
	t.Parallel()
	kf, err := Generate("mta", "derivation-test", 1)
	if err != nil {
		t.Fatalf("Generate: %v", err)
	}
	pk1 := kf.X25519PublicKey()
	// Mutate kf.X25519Priv on a copy to confirm a different private
	// produces a different pubkey (sanity). Touch a middle byte — X25519
	// clamps the low three bits of byte 0 and the high bit of byte 31
	// (RFC 7748), so flipping those would be erased by clamping and
	// could collapse the pubkey to the original. Byte 15 is safely in
	// the un-clamped middle.
	kf2 := *kf
	kf2.X25519Priv = make([]byte, SeedLen)
	copy(kf2.X25519Priv, kf.X25519Priv)
	kf2.X25519Priv[15] ^= 0x80
	pk2 := kf2.X25519PublicKey()
	if pk1 == pk2 {
		t.Fatalf("X25519PublicKey collapsed across distinct private keys")
	}

	// Round-trip the same private through Marshal/Unmarshal and confirm
	// the pubkey is stable.
	raw, err := kf.Marshal()
	if err != nil {
		t.Fatalf("Marshal: %v", err)
	}
	kf3, err := Unmarshal(raw)
	if err != nil {
		t.Fatalf("Unmarshal: %v", err)
	}
	if pk1 != kf3.X25519PublicKey() {
		t.Fatalf("X25519PublicKey changed across Marshal/Unmarshal")
	}
}

// TestUpdateEnrollmentRewritesRoleAndBridgeID exercises the post-
// Whoami code path: a keyfile was generated with placeholder
// "unresolved" values before nest resolved the role; after Whoami,
// main.go calls UpdateEnrollment to persist the resolved values so
// a future admin inspecting the file sees the right role.
func TestUpdateEnrollmentRewritesRoleAndBridgeID(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	path := filepath.Join(dir, "keyfile.cbor")

	// Cold-start path with placeholder identity.
	kf, err := LoadOrCreate(path, "unresolved", "unresolved-bridge")
	if err != nil {
		t.Fatalf("LoadOrCreate: %v", err)
	}
	if kf.Role != "unresolved" || kf.BridgeID != "unresolved-bridge" {
		t.Fatalf("initial: role=%q bridge_id=%q", kf.Role, kf.BridgeID)
	}
	origSeed := append([]byte(nil), kf.Ed25519Seed...)
	origX := append([]byte(nil), kf.X25519Priv...)
	origCreated := kf.CreatedAt

	// Post-Whoami: persist the resolved identity.
	if err := kf.UpdateEnrollment(path, "mta", "mta-eu-1"); err != nil {
		t.Fatalf("UpdateEnrollment: %v", err)
	}
	if kf.Role != "mta" || kf.BridgeID != "mta-eu-1" {
		t.Fatalf("after update: role=%q bridge_id=%q", kf.Role, kf.BridgeID)
	}

	// Read back from disk and confirm all fields round-trip.
	reloaded, err := LoadOrCreate(path, "ignored", "ignored")
	if err != nil {
		t.Fatalf("reload: %v", err)
	}
	if reloaded.Role != "mta" || reloaded.BridgeID != "mta-eu-1" {
		t.Fatalf("reload: role=%q bridge_id=%q", reloaded.Role, reloaded.BridgeID)
	}
	if !bytes.Equal(reloaded.Ed25519Seed, origSeed) {
		t.Fatalf("Ed25519Seed changed across UpdateEnrollment")
	}
	if !bytes.Equal(reloaded.X25519Priv, origX) {
		t.Fatalf("X25519Priv changed across UpdateEnrollment")
	}
	if reloaded.CreatedAt != origCreated {
		t.Fatalf("CreatedAt changed: got %d, want %d", reloaded.CreatedAt, origCreated)
	}

	// Idempotent no-op when role + bridge_id already match.
	info1, err := os.Stat(path)
	if err != nil {
		t.Fatalf("stat: %v", err)
	}
	if err := kf.UpdateEnrollment(path, "mta", "mta-eu-1"); err != nil {
		t.Fatalf("UpdateEnrollment (no-op): %v", err)
	}
	info2, err := os.Stat(path)
	if err != nil {
		t.Fatalf("stat (post): %v", err)
	}
	if !info1.ModTime().Equal(info2.ModTime()) {
		t.Fatalf("UpdateEnrollment touched mtime on a no-op (orig %v post %v)", info1.ModTime(), info2.ModTime())
	}
}

// TestArchiveAndRegenerate exercises the service-user re-keying
// auto-regenerate leg (mail-bridge-lifecycle.md § Service-user re-keying
// steps 6–7): the revoked keyfile is archived read-only at
// `<path>.revoked.<unix-time>` and a fresh keypair lands at the original
// path.
func TestArchiveAndRegenerate(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	path := filepath.Join(dir, "mta.key")

	old, err := LoadOrCreate(path, "mta", "mta-1")
	if err != nil {
		t.Fatalf("LoadOrCreate: %v", err)
	}

	fresh, archived, err := ArchiveAndRegenerate(path, "mta", "mta-1")
	if err != nil {
		t.Fatalf("ArchiveAndRegenerate: %v", err)
	}

	// The archive carries the OLD key, read-only 0400, beside the original.
	if filepath.Dir(archived) != dir {
		t.Fatalf("archive %q not beside the keyfile", archived)
	}
	info, err := os.Stat(archived)
	if err != nil {
		t.Fatalf("stat archive: %v", err)
	}
	// Read-only, which is what the archive's 0400 is for (the revoked key is
	// preserved for forensics/audit and never auto-deleted). On Windows that
	// is the read-only attribute, surfacing as 0444 — same guarantee, the only
	// spelling the platform has.
	if runtime.GOOS == "windows" {
		if perm := info.Mode().Perm(); perm&0o222 != 0 {
			t.Fatalf("archive mode = %o, want read-only (no write bits)", perm)
		}
	} else if info.Mode().Perm() != 0o400 {
		t.Fatalf("archive mode = %o, want 0400", info.Mode().Perm())
	}
	raw, err := os.ReadFile(archived)
	if err != nil {
		t.Fatalf("read archive: %v", err)
	}
	archivedKf, err := Unmarshal(raw)
	if err != nil {
		t.Fatalf("archive must stay a valid keyfile: %v", err)
	}
	if !bytes.Equal(archivedKf.Ed25519Seed, old.Ed25519Seed) {
		t.Fatalf("archive does not carry the old Ed25519 seed")
	}

	// The original path holds a FRESH keypair (different from the old one).
	reloaded, err := LoadOrCreate(path, "ignored", "ignored")
	if err != nil {
		t.Fatalf("reload fresh keyfile: %v", err)
	}
	if bytes.Equal(reloaded.Ed25519Seed, old.Ed25519Seed) {
		t.Fatalf("fresh keyfile still carries the revoked Ed25519 seed")
	}
	if !bytes.Equal(reloaded.Ed25519Seed, fresh.Ed25519Seed) {
		t.Fatalf("on-disk fresh keyfile differs from the returned one")
	}

	// A second same-second rotation must not clobber the first archive.
	_, archived2, err := ArchiveAndRegenerate(path, "mta", "mta-1")
	if err != nil {
		t.Fatalf("second ArchiveAndRegenerate: %v", err)
	}
	if archived2 == archived {
		t.Fatalf("second archive path collided with the first: %q", archived)
	}
	if _, err := os.Stat(archived); err != nil {
		t.Fatalf("first archive vanished after second rotation: %v", err)
	}
}

// Sentinel check: ensure errors.Is wires through correctly for any
// future caller that wants to distinguish "file missing" from other
// errors. (LoadOrCreate hides ErrNotExist by generating; this exists
// purely to lock the package's import graph from drifting.)
func TestErrorsHelperImportPresent(t *testing.T) {
	t.Parallel()
	if !errors.Is(os.ErrNotExist, os.ErrNotExist) {
		t.Fatalf("errors.Is sanity check failed")
	}
}
