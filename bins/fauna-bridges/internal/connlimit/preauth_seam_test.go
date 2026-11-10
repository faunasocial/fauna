//go:build fauna_e2e_fixtures

// The e2e-flavor half of the pre-auth deadline seam's tests: they compile only
// where the seam does (`go test -tags fauna_e2e_fixtures ./internal/connlimit/`,
// which `just mail-bridge-test` runs). Their production twin is
// preauth_seam_absent_test.go.
package connlimit

import (
	"testing"
	"time"
)

func TestPreAuthDeadlineSeamShortensTheBound(t *testing.T) {
	t.Setenv(preAuthDeadlineEnv, "3000")
	if got := preAuthDeadline(); got != 3*time.Second {
		t.Fatalf("seam deadline = %v, want 3s", got)
	}
	// And a listener built while it is set applies it at accept.
	now := time.Unix(1_700_000_000, 0)
	inner := &recordingConn{}
	l := NewPreAuthListener(&oneShotListener{conn: inner}, nil)
	l.SetClockForTest(func() time.Time { return now })
	if _, err := l.Accept(); err != nil {
		t.Fatalf("accept: %v", err)
	}
	if got, want := inner.lastRead(), now.Add(3*time.Second); !got.Equal(want) {
		t.Fatalf("read bound = %v, want %v", got, want)
	}
	if got, want := inner.lastWrite(), now.Add(3*time.Second); !got.Equal(want) {
		t.Fatalf("write bound = %v, want %v", got, want)
	}
}

// The seam shortens the bound and never removes or lengthens it.
func TestPreAuthDeadlineSeamFallsBackToTheConstant(t *testing.T) {
	for _, v := range []string{"", "  ", "abc", "0", "-5", "600000"} {
		t.Setenv(preAuthDeadlineEnv, v)
		if got := preAuthDeadline(); got != PreAuthDeadline {
			t.Fatalf("%q: deadline = %v, want the shipped %v", v, got, PreAuthDeadline)
		}
	}
}
