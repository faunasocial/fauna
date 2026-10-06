package confinement

import (
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

// ⚠ BOTH pins in this file read a file OUTSIDE the Go module, and Go's test
// cache does not track it. Measured 2026-08-23: rename a token in
// bins/fauna-sandbox/src/main.rs, run `go test ./internal/confinement/` with no
// other Go file touched, and the result comes back `(cached)` — green, while
// wrong. That is the failure mode these tests exist to prevent, reproduced in
// the tests themselves.
//
// So `_mail-bridge-go-test-legs` (the legs `mail-bridge-test` and the Linux
// merge-gate check both run; the Windows portable half mirrors the line) runs them
// with `-count=1`, and both machines' Go gate scopes name bins/fauna-sandbox/
// so a sandbox-only change fires them. Do not "simplify" that line away:
// without it a cross-language rename lands green, and the only signal is a
// deployed bridge reporting `unknown` forever. Locally, always `-count=1` when
// touching fauna-sandbox.
//
// The env-var name is a contract with a Rust binary in another language that
// cannot break the build if it drifts: `fauna-sandbox` sets it, this package
// reads it, and a rename on either side silently degrades every deployed
// bridge's report to "unknown" — a failure that looks exactly like a
// Landlock-less kernel. Pin the name against the Rust source itself.
func TestLandlockStatusEnvMatchesTheRustWrapper(t *testing.T) {
	// internal/confinement → bins/fauna-bridges → bins → repo root
	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("runtime.Caller failed")
	}
	root := filepath.Clean(filepath.Join(filepath.Dir(thisFile), "..", "..", "..", ".."))
	src := filepath.Join(root, "bins", "fauna-sandbox", "src", "main.rs")
	b, err := os.ReadFile(src)
	if err != nil {
		t.Fatalf("read %s: %v", src, err)
	}
	want := `pub const LANDLOCK_STATUS_ENV: &str = "` + landlockStatusEnv + `";`
	if !strings.Contains(string(b), want) {
		t.Fatalf("fauna-sandbox does not declare %s\n"+
			"expected to find: %s\n"+
			"The wrapper and this reader must agree on the variable name, or every "+
			"deployed bridge reports landlock=%q and the drift is invisible.",
			landlockStatusEnv, want, Unknown)
	}
}

// The env-var pin above covers the variable's NAME and structurally cannot
// cover its VALUES — which is the same blind spot the reply-*.cbor fixtures
// have against the WS-RPC wire, one boundary over. A rename of `partial` on the
// Rust side would leave the name pin green, `landlockStatus` would fall to
// `unknown` for every deployed bridge, and `Confined()` would then report every
// correctly-sandboxed box as degraded — a false alarm on the one signal that is
// supposed to mean the sandbox broke.
//
// So pin the values the same way, against the Rust source that emits them. The
// wrapper declares them as named constants precisely so this test has something
// stable to match; matching the `match` arms of `landlock::apply` instead would
// break on a refactor that changed nothing observable.
func TestLandlockTokensMatchTheRustWrapper(t *testing.T) {
	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("runtime.Caller failed")
	}
	root := filepath.Clean(filepath.Join(filepath.Dir(thisFile), "..", "..", "..", ".."))
	src := filepath.Join(root, "bins", "fauna-sandbox", "src", "main.rs")
	b, err := os.ReadFile(src)
	if err != nil {
		t.Fatalf("read %s: %v", src, err)
	}
	// Every token this package can read must be one the wrapper can emit. The
	// map is over the Go constants, so adding one here without the Rust side
	// reds — the direction that matters, since this reader is downstream.
	for name, token := range map[string]string{
		"LandlockFully":   LandlockFully,
		"LandlockPartial": LandlockPartial,
		"LandlockOff":     LandlockOff,
	} {
		want := `pub const LANDLOCK_STATUS_` + strings.ToUpper(strings.TrimPrefix(name, "Landlock")) +
			`: &str = "` + token + `";`
		if !strings.Contains(string(b), want) {
			t.Errorf("fauna-sandbox does not declare %s = %q\n"+
				"expected to find: %s\n"+
				"The wrapper emits these tokens and this package switches on them; a "+
				"rename on either side degrades every deployed bridge's report to %q, "+
				"which then reads as a broken sandbox rather than a broken contract.",
				name, token, want, Unknown)
		}
	}
}

func TestProbeSealedStore(t *testing.T) {
	dir := t.TempDir()

	t.Run("absent", func(t *testing.T) {
		if got := probeSealedStore(filepath.Join(dir, "nope.db")); got != SealedStoreAbsent {
			t.Errorf("got %q, want %q", got, SealedStoreAbsent)
		}
	})

	t.Run("readable is the FAILURE case", func(t *testing.T) {
		p := filepath.Join(dir, "nest.db")
		if err := os.WriteFile(p, []byte("SQLite format 3"), 0o600); err != nil {
			t.Fatal(err)
		}
		if got := probeSealedStore(p); got != SealedStoreReadable {
			t.Errorf("got %q, want %q — a store this process CAN read must report "+
				"readable, since that is the whole misconfiguration we are looking for",
				got, SealedStoreReadable)
		}
	})

	t.Run("denied", func(t *testing.T) {
		if os.Getuid() == 0 {
			t.Skip("root bypasses DAC (CAP_DAC_OVERRIDE), so a 0000 file is still readable")
		}
		if runtime.GOOS == "windows" {
			// Windows has no spelling of 0o000: os.WriteFile's mode maps only
			// to the read-only attribute (write-deny), never read-deny, so
			// os.Open still succeeds and probeSealedStore correctly reports
			// "readable" — the same platform reality keypair_test.go's mode
			// assertions carry. The real protection on Windows is the ACL of
			// the data dir, not a POSIX permission bit this probe can trigger.
			t.Skip("Windows mode bits cannot deny read access; the ACL of the data dir is the real gate here")
		}
		p := filepath.Join(dir, "locked.db")
		if err := os.WriteFile(p, []byte("x"), 0o000); err != nil {
			t.Fatal(err)
		}
		if got := probeSealedStore(p); got != SealedStoreDenied {
			t.Errorf("got %q, want %q", got, SealedStoreDenied)
		}
	})
}

func TestLandlockStatusBoundsTheWrapperHandoff(t *testing.T) {
	for _, tc := range []struct {
		name, set, want string
	}{
		{"fully", "fully", "fully"},
		{"partial", "partial", "partial"},
		{"off", "off", "off"},
		{"whitespace tolerated", " partial\n", "partial"},
		// The wrapper never ran — materially different from "Landlock is off",
		// which is why it is not collapsed into "off".
		{"absent means unknown, not off", "", Unknown},
		// A value that is not one of the wrapper's tokens must never reach the
		// wire: nest renders this string on an admin page.
		{"arbitrary text is refused", "fully<script>", Unknown},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if tc.set == "" {
				t.Setenv(landlockStatusEnv, "")
				os.Unsetenv(landlockStatusEnv)
			} else {
				t.Setenv(landlockStatusEnv, tc.set)
			}
			if got := landlockStatus(); got != tc.want {
				t.Errorf("got %q, want %q", got, tc.want)
			}
		})
	}
}

func TestSeccompStatusIsOneOfTheKnownTokens(t *testing.T) {
	got := seccompStatus()
	switch got {
	case SeccompOff, SeccompStrict, SeccompFilter, Unknown:
	default:
		t.Fatalf("seccompStatus returned %q, which is not a wire token", got)
	}
}

// Probe must never fail the bridge's startup, whatever it cannot determine.
func TestProbeAlwaysProducesAWellFormedReport(t *testing.T) {
	r := Probe(filepath.Join(t.TempDir(), "no-such-dir"))
	if r.SealedStore == "" || r.Landlock == "" || r.Seccomp == "" {
		t.Fatalf("Probe left a field empty: %+v", r)
	}
	if r.UID != uint32(os.Getuid()) {
		t.Errorf("uid = %d, want %d", r.UID, os.Getuid())
	}
}

func TestConfinedRequiresBothHalves(t *testing.T) {
	for _, tc := range []struct {
		name  string
		r     Report
		want  bool
		wants string
	}{
		{"healthy production shape", Report{SealedStore: SealedStoreDenied, Landlock: "partial"}, true,
			"partial enforcement with working denials is the shape every current kernel reports"},
		{"fully enforced", Report{SealedStore: SealedStoreDenied, Landlock: "fully"}, true, ""},
		{"denied but wrapper never ran", Report{SealedStore: SealedStoreDenied, Landlock: Unknown}, false,
			"the store is unreachable, but nothing attributes that to the kernel LSM"},
		{"store readable", Report{SealedStore: SealedStoreReadable, Landlock: "fully"}, false,
			"a readable sealed store is broken confinement regardless of Landlock"},
		{"absent store is not proof", Report{SealedStore: SealedStoreAbsent, Landlock: "fully"}, false,
			"ENOENT is inconclusive, never evidence of confinement"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := tc.r.Confined(); got != tc.want {
				t.Errorf("Confined() = %v, want %v (%s)", got, tc.want, tc.wants)
			}
		})
	}
}
