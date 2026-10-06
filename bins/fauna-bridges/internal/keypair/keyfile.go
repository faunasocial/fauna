// Package keypair holds the bridge's on-disk service-user keyfile.
//
// The keyfile carries TWO secrets, both required by the bridge runtime:
//
//   - A 32-byte Ed25519 signing seed. Used to sign WS-RPC handshake
//     challenges so nest can identify which approved service user this
//     bridge is. The derived Ed25519 public key is what the bridge
//     enrolls via fauna.bridges.request_enrollment and the admin approves.
//   - A 32-byte X25519 private key. Used to HPKE-Open the wrapped TLS
//     blob and capability grants nest hands the bridge after authentication (see
//     the wrapped-blob crypto design, tracked internally,
//     § Service-user keypair shape).
//
// Wire format mirrors libs/fauna-mls/src/wrapped_blob/service_user.rs::
// ServiceUserKeyfile byte-for-decoded-field. Both sides encode a CBOR
// map with the same six string keys:
//
//	v             : u8       (must be 1)
//	role          : string   ("mta" or "mda")
//	bridge_id     : string
//	ed25519_seed  : bstr 32B
//	x25519_priv   : bstr 32B
//	created_at    : u64
//
// CROSS-LANGUAGE BYTE PARITY: the keyfile is a per-deployment local file
// (mode 0600). We need decoded-value parity (Rust-written keyfile loads
// fine in Go and vice versa), NOT on-wire byte parity. The Rust side
// (ciborium) currently emits these mixed-length string keys in serde
// source order, not length-first; the Go side (internal/dagcbor)
// always emits length-first. This means Go-written and Rust-written
// keyfiles for the same struct differ in key order on disk. That is
// intentional and harmless here — the decoder on either side accepts
// any well-formed CBOR map. A future maintainer should NOT try to
// "fix" the divergence by reaching into fxamacker/cbor directly; the
// wire-envelope path (Phase B.4) is where byte parity matters, and
// the Rust side tracks the fix
// for the cases where it does.
package keypair

import (
	"crypto/ecdh"
	"crypto/ed25519"
	"crypto/rand"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
)

// Version is the only valid value for Keyfile.Version. The Rust side
// rejects anything else at decode time (libs/fauna-mls/src/wrapped_blob/
// service_user.rs:77-82); we mirror that behavior in LoadOrCreate.
const Version uint8 = 1

// SeedLen is the required length of both Ed25519Seed and X25519Priv.
const SeedLen = 32

// Keyfile is the on-disk representation. Field tags MUST match the Rust
// side's serde rename attributes (see service_user.rs lines 23-35).
type Keyfile struct {
	Version     uint8  `cbor:"v"`
	Role        string `cbor:"role"`
	BridgeID    string `cbor:"bridge_id"`
	Ed25519Seed []byte `cbor:"ed25519_seed"`
	X25519Priv  []byte `cbor:"x25519_priv"`
	CreatedAt   uint64 `cbor:"created_at"`
}

// Generate creates a fresh keyfile with random Ed25519 + X25519 secrets.
//
// Uses crypto/rand for both seeds. Both keys are RFC 7748 / RFC 8032
// conformant — the Ed25519 32-byte "seed" is the standard ed25519.PrivateKey
// seed (used by ed25519.NewKeyFromSeed), and the X25519 32-byte private
// is the standard scalar (the high-bit / low-three-bits clamping happens
// inside crypto/ecdh.X25519().NewPrivateKey, matching what the Rust hpke
// crate does on its side).
func Generate(role, bridgeID string, createdAt uint64) (*Keyfile, error) {
	edSeed := make([]byte, SeedLen)
	if _, err := io.ReadFull(rand.Reader, edSeed); err != nil {
		return nil, fmt.Errorf("keypair.Generate: read ed25519 seed: %w", err)
	}
	xPriv := make([]byte, SeedLen)
	if _, err := io.ReadFull(rand.Reader, xPriv); err != nil {
		return nil, fmt.Errorf("keypair.Generate: read x25519 priv: %w", err)
	}
	return &Keyfile{
		Version:     Version,
		Role:        role,
		BridgeID:    bridgeID,
		Ed25519Seed: edSeed,
		X25519Priv:  xPriv,
		CreatedAt:   createdAt,
	}, nil
}

// Marshal encodes the keyfile as DAG-CBOR.
//
// The lengths of Ed25519Seed and X25519Priv are validated before encoding
// so a malformed in-memory keyfile can't reach the disk.
func (k *Keyfile) Marshal() ([]byte, error) {
	if err := k.validate(); err != nil {
		return nil, err
	}
	return dagcbor.Marshal(k)
}

// Unmarshal decodes a DAG-CBOR keyfile and validates version + lengths.
//
// Returns an error for unsupported version, wrong seed lengths, or any
// underlying CBOR decode failure.
func Unmarshal(b []byte) (*Keyfile, error) {
	kf, err := dagcbor.Unmarshal[Keyfile](b)
	if err != nil {
		return nil, fmt.Errorf("keypair.Unmarshal: cbor decode: %w", err)
	}
	if kf.Version != Version {
		return nil, fmt.Errorf("keypair.Unmarshal: unsupported version %d (want %d)", kf.Version, Version)
	}
	if err := kf.validate(); err != nil {
		return nil, err
	}
	return &kf, nil
}

// validate checks the structural invariants enforced on both load and
// save. Version is intentionally NOT checked here — Generate writes the
// current version, and Unmarshal checks it explicitly with a clearer
// error message.
func (k *Keyfile) validate() error {
	if len(k.Ed25519Seed) != SeedLen {
		return fmt.Errorf("keypair: ed25519_seed must be %d bytes, got %d", SeedLen, len(k.Ed25519Seed))
	}
	if len(k.X25519Priv) != SeedLen {
		return fmt.Errorf("keypair: x25519_priv must be %d bytes, got %d", SeedLen, len(k.X25519Priv))
	}
	return nil
}

// LoadOrCreate reads the keyfile at path, or generates and writes a new
// one if the path does not exist.
//
// Refuses to load a keyfile whose mode allows group/other access
// (perm & 0o077 != 0); the keyfile holds two long-lived secrets, and a
// loose-perms file is a configuration error worth surfacing loudly.
//
// On create, writes atomically via a temp file in the same directory
// with mode 0o600, then renames. The rename is local-filesystem-only
// (same dir), so it's POSIX-atomic.
func LoadOrCreate(path, role, bridgeID string) (*Keyfile, error) {
	info, err := os.Stat(path)
	switch {
	case err == nil:
		// File exists; enforce 0o077-clean perms before reading. Skipped on
		// Windows: NTFS security is ACL-based, and Go's os.Stat synthesizes
		// Mode() from only the read-only attribute (0666 when writable, 0444
		// when read-only) — never 0o600 — so a 0o077-clean check is both
		// meaningless and unsatisfiable there, rejecting every keyfile and
		// stranding the desktop-native Windows MDA. On Windows the keyfile is
		// protected by filesystem ACLs (per-user %LOCALAPPDATA% / the service's
		// data dir) instead. The Unix gate stays exact (chmod 0600 required).
		if runtime.GOOS != "windows" {
			if perm := info.Mode().Perm(); perm&0o077 != 0 {
				return nil, fmt.Errorf("keypair: refusing to load %s: mode %o is too permissive (group/other bits set); chmod 0600", path, perm)
			}
		}
		bytes, err := os.ReadFile(path)
		if err != nil {
			return nil, fmt.Errorf("keypair: read %s: %w", path, err)
		}
		kf, err := Unmarshal(bytes)
		if err != nil {
			return nil, fmt.Errorf("keypair: decode %s: %w", path, err)
		}
		return kf, nil
	case errors.Is(err, os.ErrNotExist):
		// Fresh-run path: generate, write atomically with mode 0o600.
		kf, err := Generate(role, bridgeID, uint64(nowUnix()))
		if err != nil {
			return nil, err
		}
		if err := writeAtomic(path, kf); err != nil {
			return nil, err
		}
		return kf, nil
	default:
		return nil, fmt.Errorf("keypair: stat %s: %w", path, err)
	}
}

// writeAtomic encodes kf and writes it to path via a tempfile + rename.
// The tempfile is created in the same directory as path so the rename
// is on the same filesystem (POSIX-atomic).
func writeAtomic(path string, kf *Keyfile) error {
	bytes, err := kf.Marshal()
	if err != nil {
		return fmt.Errorf("keypair: encode: %w", err)
	}
	dir := filepath.Dir(path)
	// CreateTemp creates the file with mode 0600 by default, which is
	// exactly what we want — no separate chmod needed.
	tmp, err := os.CreateTemp(dir, ".fauna-keyfile-*")
	if err != nil {
		return fmt.Errorf("keypair: create temp in %s: %w", dir, err)
	}
	tmpPath := tmp.Name()
	// Best-effort cleanup if anything below fails; ignore the error
	// (the file may already have been renamed away).
	defer func() { _ = os.Remove(tmpPath) }()
	if _, err := tmp.Write(bytes); err != nil {
		_ = tmp.Close()
		return fmt.Errorf("keypair: write temp %s: %w", tmpPath, err)
	}
	if err := tmp.Sync(); err != nil {
		_ = tmp.Close()
		return fmt.Errorf("keypair: fsync temp %s: %w", tmpPath, err)
	}
	if err := tmp.Close(); err != nil {
		return fmt.Errorf("keypair: close temp %s: %w", tmpPath, err)
	}
	if err := os.Rename(tmpPath, path); err != nil {
		return fmt.Errorf("keypair: rename %s -> %s: %w", tmpPath, path, err)
	}
	return nil
}

// SigningKey returns the full 64-byte Ed25519 private key derived from
// the stored 32-byte seed. The crypto/ed25519 package's "private key"
// shape is seed || pubkey, derived via NewKeyFromSeed.
func (k *Keyfile) SigningKey() ed25519.PrivateKey {
	return ed25519.NewKeyFromSeed(k.Ed25519Seed)
}

// Ed25519PublicKey returns the 32-byte Ed25519 public key.
func (k *Keyfile) Ed25519PublicKey() ed25519.PublicKey {
	return k.SigningKey().Public().(ed25519.PublicKey)
}

// X25519Secret returns a copy of the 32-byte X25519 private scalar.
//
// Returning a fixed-size array (not a slice) prevents callers from
// holding an aliased reference to the secret bytes — every call
// produces an independent copy the caller is free to zero.
func (k *Keyfile) X25519Secret() [SeedLen]byte {
	var out [SeedLen]byte
	copy(out[:], k.X25519Priv)
	return out
}

// X25519PublicKey computes the X25519 public key from the stored
// private scalar. Uses crypto/ecdh (Go 1.20+), which performs the
// standard RFC 7748 clamping internally — matching the Rust hpke
// crate's behavior on its side.
//
// Panics if the stored x25519_priv is not 32 bytes — caller-side
// validation (validate / Unmarshal / LoadOrCreate) ensures this in
// every code path that builds a Keyfile, so a panic here is a
// programmer error, not a runtime condition.
func (k *Keyfile) X25519PublicKey() [SeedLen]byte {
	priv, err := ecdh.X25519().NewPrivateKey(k.X25519Priv)
	if err != nil {
		panic(fmt.Sprintf("keypair: X25519PublicKey on invalid private (len=%d): %v", len(k.X25519Priv), err))
	}
	var out [SeedLen]byte
	copy(out[:], priv.PublicKey().Bytes())
	return out
}

// ArchiveAndRegenerate implements the service-user re-keying auto-regenerate
// leg (mail-bridge-lifecycle.md § Service-user re-keying steps 6–7): the
// keyfile at path belongs to a REVOKED enrollment, so archive it at
// `<path>.revoked.<unix-time>` (mode 0400, read-only — kept indefinitely for
// audit/forensics) and write a freshly-generated keyfile to the original path.
//
// Returns the fresh keyfile plus the archive path. The archive rename is
// same-directory (POSIX-atomic); on the pathological same-second double
// rotation a numeric suffix is appended rather than clobbering the earlier
// archive. The fresh keyfile is written with the same atomic temp+rename as
// LoadOrCreate.
func ArchiveAndRegenerate(path, role, bridgeID string) (kf *Keyfile, archivePath string, err error) {
	archivePath = fmt.Sprintf("%s.revoked.%d", path, nowUnix())
	for i := 1; ; i++ {
		if _, err := os.Stat(archivePath); errors.Is(err, os.ErrNotExist) {
			break
		}
		archivePath = fmt.Sprintf("%s.revoked.%d.%d", path, nowUnix(), i)
	}
	if err := os.Rename(path, archivePath); err != nil {
		return nil, "", fmt.Errorf("keypair: archive revoked keyfile %s -> %s: %w", path, archivePath, err)
	}
	if err := os.Chmod(archivePath, 0o400); err != nil {
		return nil, "", fmt.Errorf("keypair: chmod 0400 archived keyfile %s: %w", archivePath, err)
	}
	kf, err = Generate(role, bridgeID, uint64(nowUnix()))
	if err != nil {
		return nil, "", err
	}
	if err := writeAtomic(path, kf); err != nil {
		return nil, "", err
	}
	return kf, archivePath, nil
}

// UpdateEnrollment rewrites the keyfile at path with the resolved role
// + bridgeID fields, preserving every other field (seeds, version,
// created_at). main.go calls this once after the first successful
// `fauna.bridges.whoami` so a future inspection of the keyfile names
// the role nest assigned this bridge.
//
// Useful when the keyfile was created with placeholder "unresolved"
// values before Whoami had resolved the role — the seeds are
// load-bearing for crypto, role + bridge_id are admin-visible
// metadata.
//
// Idempotent: if the keyfile's existing role + bridge_id already
// match, no rewrite happens (avoids touching mtime on a no-op).
// Uses the same atomic temp-file + rename path as LoadOrCreate.
func (k *Keyfile) UpdateEnrollment(path, role, bridgeID string) error {
	if k.Role == role && k.BridgeID == bridgeID {
		return nil
	}
	k.Role = role
	k.BridgeID = bridgeID
	return writeAtomic(path, k)
}

// nowUnix is a seam for tests; production calls time.Now().Unix(). The
// LoadOrCreate path uses it only for the fresh-generation CreatedAt
// field.
var nowUnix = func() int64 { return time.Now().Unix() }
