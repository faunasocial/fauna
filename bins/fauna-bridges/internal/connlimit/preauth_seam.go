//go:build fauna_e2e_fixtures

// The pre-auth deadline seam: e2e builds only. Convention 15
// (`docs/goal/architecture/e2e-automation-surface-gating.md` § Implementation
// status today → the Go bridges' leg): a harness seam is compiled out of the
// release bridge under the `fauna_e2e_fixtures` tag, with
// `preauth_seam_absent.go` as the same-signature production twin.
//
// Why it exists: the tier_3 witnesses of the pre-auth connection deadline
// (`docs/features/mail-abuse-refused.md` outcome 26) must watch a real bridge
// close a connection that never authenticates, on 993, 143, 465 and 587. At the
// shipped 60 s that is a minute of wall clock per port; with this seam the
// harness boots the e2e flavor with a deadline of a few seconds, short beside
// the 30 s per-command read timeout, so a close inside the budget can only be
// the bound fixed at accept. The seam shortens the bound and never removes it:
// an unset, unparsable, zero or negative value, or one past PreAuthDeadline,
// leaves the shipped constant.
package connlimit

import (
	"os"
	"strconv"
	"strings"
	"time"
)

// preAuthDeadlineEnv is TEST-HARNESS IPC, never operator configuration: only
// the e2e flavor reads it, so a production deployment cannot set it.
const preAuthDeadlineEnv = "FAUNA_MAIL_E2E_PREAUTH_DEADLINE_MS"

// preAuthDeadline returns the pre-auth bound new listeners apply: the
// harness's shorter value when the seam variable names one, else
// PreAuthDeadline.
func preAuthDeadline() time.Duration {
	raw := strings.TrimSpace(os.Getenv(preAuthDeadlineEnv))
	if raw == "" {
		return PreAuthDeadline
	}
	ms, err := strconv.ParseInt(raw, 10, 64)
	if err != nil || ms <= 0 {
		return PreAuthDeadline
	}
	d := time.Duration(ms) * time.Millisecond
	if d > PreAuthDeadline {
		return PreAuthDeadline
	}
	return d
}
