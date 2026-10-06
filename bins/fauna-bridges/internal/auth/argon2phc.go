// Argon2id PHC-string verification — the bridge-side half of the ATProto
// app-credential custody split (docs/goal/behavior/atproto-pds-full.md
// § Detailed design → F1 detail). The minting Fauna app computes a
// PHC-serialized Argon2id verifier (libs/fauna-client-bridges/src/
// atproto_credential.rs — the Rust twin whose output this must verify);
// the nest stores it; the atproto bridge fetches verifier rows at
// createSession and verifies the presented secret HERE, keeping both the
// secret and the Argon2id CPU cost off the nest.
//
// The PHC string is self-describing ($argon2id$v=19$m=…,t=…,p=…$salt$hash),
// so old credentials verify under their own parameters forever, exactly as
// the Rust side promises. The cross-language fixture test
// (argon2phc_fixture_test.go) pins the two implementations to one format.
//
// Pure Go (x/crypto/argon2) — the atproto bridge is CGO-free by design
// (cmd/fauna-atproto-bridge doc comment), so this must never grow an FFI
// dependency.
package auth

import (
	"crypto/subtle"
	"encoding/base64"
	"fmt"
	"strconv"
	"strings"

	"golang.org/x/crypto/argon2"
)

// Verify-side resource ceilings: a stored verifier is trusted data (the
// account's own client minted it), but a hostile or corrupted row must not
// be able to turn one createSession attempt into an OOM. Production mint
// parameters are m=65536 KiB, t=2, p=1 (mail-credentials.md § KDF choice);
// the caps leave generous headroom for future cost bumps without admitting
// gigabyte-scale requests.
const (
	maxArgon2MemoryKiB = 1 << 20 // 1 GiB
	maxArgon2Time      = 16
	maxArgon2Threads   = 8
	maxArgon2OutputLen = 128
)

// VerifyArgon2PHC reports whether candidate matches the PHC-serialized
// Argon2id verifier. Malformed strings, non-argon2id variants, and
// out-of-cap parameters all return false — never an error a caller could
// accidentally turn into a user-visible distinction (createSession failures
// must be uniform, no enumeration signal).
func VerifyArgon2PHC(candidate, phc string) bool {
	params, salt, want, ok := parseArgon2PHC(phc)
	if !ok {
		return false
	}
	got := argon2.IDKey([]byte(candidate), salt, params.time, params.memoryKiB, params.threads, uint32(len(want)))
	return subtle.ConstantTimeCompare(got, want) == 1
}

type argon2Params struct {
	memoryKiB uint32
	time      uint32
	threads   uint8
}

// parseArgon2PHC parses `$argon2id$v=19$m=<KiB>,t=<n>,p=<n>$<salt-b64>$<hash-b64>`.
// Only the argon2id variant at version 19 is accepted (the only shape the
// Rust minter emits); salts and hashes are unpadded standard base64 per the
// PHC spec.
func parseArgon2PHC(phc string) (argon2Params, []byte, []byte, bool) {
	fields := strings.Split(phc, "$")
	// Leading '$' yields an empty fields[0].
	if len(fields) != 6 || fields[0] != "" || fields[1] != "argon2id" {
		return argon2Params{}, nil, nil, false
	}
	if fields[2] != "v="+strconv.Itoa(argon2.Version) {
		return argon2Params{}, nil, nil, false
	}
	params, ok := parseArgon2ParamField(fields[3])
	if !ok {
		return argon2Params{}, nil, nil, false
	}
	salt, err := base64.RawStdEncoding.Strict().DecodeString(fields[4])
	if err != nil || len(salt) == 0 {
		return argon2Params{}, nil, nil, false
	}
	hash, err := base64.RawStdEncoding.Strict().DecodeString(fields[5])
	if err != nil || len(hash) == 0 || len(hash) > maxArgon2OutputLen {
		return argon2Params{}, nil, nil, false
	}
	return params, salt, hash, true
}

// parseArgon2ParamField parses the `m=…,t=…,p=…` field. The three keys must
// all be present exactly once, in any order (the PHC spec fixes the order the
// Rust argon2 crate emits, but accepting any order costs nothing).
func parseArgon2ParamField(field string) (argon2Params, bool) {
	var p argon2Params
	seen := map[string]bool{}
	for _, kv := range strings.Split(field, ",") {
		k, v, found := strings.Cut(kv, "=")
		if !found || seen[k] {
			return argon2Params{}, false
		}
		seen[k] = true
		n, err := strconv.ParseUint(v, 10, 32)
		if err != nil {
			return argon2Params{}, false
		}
		switch k {
		case "m":
			if n == 0 || n > maxArgon2MemoryKiB {
				return argon2Params{}, false
			}
			p.memoryKiB = uint32(n)
		case "t":
			if n == 0 || n > maxArgon2Time {
				return argon2Params{}, false
			}
			p.time = uint32(n)
		case "p":
			if n == 0 || n > maxArgon2Threads {
				return argon2Params{}, false
			}
			p.threads = uint8(n)
		default:
			return argon2Params{}, false
		}
	}
	if !seen["m"] || !seen["t"] || !seen["p"] {
		return argon2Params{}, false
	}
	return p, true
}

// RedactPHC returns a loggable form of a PHC string (parameters only, no
// salt/hash) for diagnostics that must never leak verifier material.
func RedactPHC(phc string) string {
	fields := strings.Split(phc, "$")
	if len(fields) < 4 {
		return "<malformed-phc>"
	}
	return fmt.Sprintf("$%s$%s$%s$<redacted>", fields[1], fields[2], fields[3])
}
