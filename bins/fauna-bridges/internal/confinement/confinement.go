// Package confinement lets the bridge probe **its own** sandbox at startup and
// report what it found to nest, so a deployed box's isolation facts are
// readable over the wire instead of over SSH.
//
// # Why this exists
//
// Slices 1 and 4 of the co-resident trust boundary (security.md § Co-resident
// process trust boundary) give each network-facing service its own UID and wrap
// the hostile-MIME parsers in `fauna-sandbox`'s Landlock + seccomp profiles. The
// tier_4 suite proves that on an image it built. But on a *deployed* box those
// same facts were provable only by a human with an SSH key and a production-shell
// approval — and a provisioned box carries no ssh key at all (testing.md § Gap 3),
// so the check does not scale past the one hand-managed host. This package is the
// no-SSH replacement, mirroring what `enrollment_strict` already does for the
// blessed-registry half of the same boundary.
//
// # What a report is, and is NOT
//
// It is a **provisioning diagnostic**: it catches the honest misconfiguration —
// a compose that bypasses the sandbox wrapper, a kernel without Landlock, a
// docker seccomp policy that blocks the landlock syscalls, an image that lost
// its per-role UIDs — on the box actually running.
//
// It is **NOT a security attestation.** A compromised bridge can report whatever
// it likes; a self-report is exactly as trustworthy as the process making it.
// Nothing may gate a security decision on it. The trust-bearing proof stays
// tier_4, where the probes run against the image from outside the sandboxed
// process. (Same framing as the sidecar log plane's trust posture —
// observability.md § Trust posture.)
//
// # Measured vs relayed
//
// [Probe] returns two kinds of fact and the distinction is load-bearing:
//
//   - UID and SealedStore are **measured first-hand**, inside the sandbox, by
//     asking the kernel. SealedStore is the fact that actually bounds the blast
//     radius ("this process cannot read the sealed store"), but it attributes
//     nothing — the DAC UID alone produces the same EACCES, so a denial here is
//     not evidence that Landlock is doing anything.
//   - Landlock is **relayed** by the `fauna-sandbox` wrapper through the
//     environment, because `RulesetStatus` is knowable only at `restrict_self`
//     and a restricted process cannot ask the kernel about its own domain. It is
//     the only field that attributes a denial to the kernel LSM.
//
// Reading them together is what makes a report meaningful: `denied` + `partial`
// is the healthy production shape; `denied` + `unknown` says the confinement
// holds but the wrapper never ran; `readable` says it does not hold at all.
package confinement

import (
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"syscall"
)

// landlockStatusEnv is the variable `fauna-sandbox` exports across execvp to
// hand this process its Landlock enforcement status. **Mirror of
// `fauna_sandbox::LANDLOCK_STATUS_ENV`** (bins/fauna-sandbox/src/main.rs) —
// the cross-language pin lives in confinement_test.go.
//
// It is artifact-set IPC, never a configuration knob: one process telling the
// next what the kernel just did, with no human in the loop
// (principles.md § One configuration surface, bucket 1).
const landlockStatusEnv = "FAUNA_SANDBOX_LANDLOCK"

// Status tokens. These go on the wire verbatim and render on the admin page, so
// they are a small stable vocabulary — never free-form text, and never an error
// string (which could carry attacker-influenced bytes into an admin surface,
// the same rule the log-plane catalogue enforces).
const (
	// Unknown — the honest answer when a fact could not be established, always
	// preferred over guessing a reassuring one.
	Unknown = "unknown"

	// SealedStoreDenied — the kernel refused the read (EACCES/EPERM). The
	// expected, healthy answer on a deployed box.
	SealedStoreDenied = "denied"
	// SealedStoreReadable — the read SUCCEEDED. The confinement is broken: this
	// process can read nest's sealed store.
	SealedStoreReadable = "readable"
	// SealedStoreAbsent — no such file (ENOENT). Inconclusive rather than bad:
	// a dev or binary-only run legitimately has no nest database at that path.
	// Landlock denies with EACCES on a path that exists, so on a real deployment
	// this token means the store genuinely is not there.
	SealedStoreAbsent = "absent"

	SeccompFilter = "filter" // /proc/self/status Seccomp: 2
	SeccompStrict = "strict" // Seccomp: 1
	SeccompOff    = "off"    // Seccomp: 0

	// The Landlock tokens the `fauna-sandbox` wrapper hands across execvp.
	// Named here as of 2026-08-23 — they were bare literals in `Confined` and
	// `landlockStatus`, i.e. the two places that decide whether a box counts as
	// sandboxed and which strings are even admissible. Their Rust counterparts
	// (`fauna_sandbox::LANDLOCK_STATUS_*`) are pinned against these by
	// TestLandlockTokensMatchTheRustWrapper.
	//
	// LandlockFully — the ruleset applied in full.
	LandlockFully = "fully"
	// LandlockPartial — partially enforced: an ABI-negotiation artifact on
	// current kernels, with the denials provably working. Counts as enforcing
	// (see Confined) — collapsing it into "not enforced" would cry wolf on
	// every healthy box.
	LandlockPartial = "partial"
	// LandlockOff — the kernel does not support Landlock. Distinct from
	// Unknown, which means the wrapper never ran at all.
	LandlockOff = "off"
)

// ConfinementStates is the closed set of tokens any field of a Report can hold
// — the union of the three vocabularies above plus Unknown.
//
// Exported so a consumer that needs to bound an incoming token can ask this
// package rather than restate the list: `internal/logplane` held its own copy
// under a comment saying the tokens "originate in internal/confinement", which
// is precisely the shape that goes stale when a fourth state lands here.
//
// Note nest deliberately does NOT mirror this set: `bound_confinement_token`
// (bins/fauna-nest) applies a charset bound instead of an allowlist, on purpose
// — a newer bridge may legitimately report a state that nest build predates,
// and additive-everywhere says that must survive. That asymmetry is correct and
// is why this set has no Rust twin.
func ConfinementStates() []string {
	return []string{
		Unknown,
		SealedStoreDenied, SealedStoreReadable, SealedStoreAbsent,
		LandlockFully, LandlockPartial, LandlockOff,
		SeccompFilter, SeccompStrict, SeccompOff,
	}
}

// Report is what the bridge observed about its own confinement. Mirrors
// libs/fauna-protocol/src/wrapped_blob.rs:BridgeConfinement.
type Report struct {
	UID         uint32 `cbor:"uid"`
	SealedStore string `cbor:"sealed_store"`
	Landlock    string `cbor:"landlock"`
	Seccomp     string `cbor:"seccomp"`
}

// Confined reports whether this looks like a correctly sandboxed process: the
// sealed store is unreachable AND Landlock is enforcing to some degree. Used
// only to decide whether to raise the log-plane warning — never to gate
// behavior, per the "not an attestation" rule above.
//
// `partial` counts as enforcing: current kernels report partial enforcement (an
// ABI-negotiation artifact) with the denials provably working, so treating it as
// a failure would cry wolf on every healthy box.
func (r Report) Confined() bool {
	return r.SealedStore == SealedStoreDenied &&
		(r.Landlock == LandlockFully || r.Landlock == LandlockPartial)
}

// Probe measures this process's confinement. `dataDir` is the bridge's
// --data-dir (the directory holding nest's sealed store).
//
// It must run **inside** the sandbox — i.e. in the bridge binary proper, after
// `fauna-sandbox` has applied its profile and exec'd us — which is automatic
// here: everything in this process is already post-exec. Probing from the
// wrapper instead would measure the wrapper's own (unrestricted) view and report
// a comforting lie.
//
// Never fails: every unobtainable fact degrades to a token, because a bridge
// must not refuse to start over a diagnostic.
func Probe(dataDir string) Report {
	return Report{
		UID:         uint32(os.Getuid()),
		SealedStore: probeSealedStore(filepath.Join(dataDir, "nest.db")),
		Landlock:    landlockStatus(),
		Seccomp:     seccompStatus(),
	}
}

// probeSealedStore attempts the read the sandbox exists to prevent. A successful
// open is the failure case — hence the immediate Close and the deliberate
// absence of any read of the contents: we are testing reachability, and must not
// pull sealed bytes into the address space of the process we are checking is not
// supposed to have them.
func probeSealedStore(path string) string {
	f, err := os.Open(path)
	if err == nil {
		_ = f.Close()
		return SealedStoreReadable
	}
	switch {
	case errors.Is(err, fs.ErrPermission), errors.Is(err, syscall.EPERM):
		return SealedStoreDenied
	case errors.Is(err, fs.ErrNotExist):
		return SealedStoreAbsent
	default:
		return Unknown
	}
}

// landlockStatus reads the wrapper's hand-off. An absent variable means the
// wrapper never ran (a dev/binary launch, or a compose that bypassed it) — a
// materially different situation from "Landlock is off", so it gets its own
// token.
//
// The value is bounded to the tokens the wrapper actually emits: nest bounds it
// again on admission, but a source that can only produce known tokens is the
// cheaper guarantee, and it means an environment variable an attacker managed to
// set cannot put arbitrary text on an admin page.
func landlockStatus() string {
	switch v := strings.TrimSpace(os.Getenv(landlockStatusEnv)); v {
	case LandlockFully, LandlockPartial, LandlockOff:
		return v
	default:
		return Unknown
	}
}

// seccompStatus reads the `Seccomp:` line of /proc/self/status. The bridge
// profiles grant /proc read access, so this is available inside the sandbox.
func seccompStatus() string {
	b, err := os.ReadFile("/proc/self/status")
	if err != nil {
		return Unknown
	}
	for _, line := range strings.Split(string(b), "\n") {
		rest, ok := strings.CutPrefix(line, "Seccomp:")
		if !ok {
			continue
		}
		switch strings.TrimSpace(rest) {
		case "0":
			return SeccompOff
		case "1":
			return SeccompStrict
		case "2":
			return SeccompFilter
		default:
			return Unknown
		}
	}
	return Unknown
}
