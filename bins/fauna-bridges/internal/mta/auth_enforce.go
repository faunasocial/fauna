// T1.5 — inbound auth-enforcement stage.
//
// The single place inbound SPF/DKIM/DMARC enforcement runs, wired into
// Session.Data after verify_inbound (C.5) returns the verdicts. Mirrors the
// legacy enforcement block bins/fauna-bridge-daemon/src/handler.rs:1260-1297
// (retired daemon; resolved via git history): one `if !log_only { … }` guard,
// stage order DMARC-reject → SPF-hardfail → DKIM.
//
// Each stage is a pure function on (verdicts, enforce flag) returning a
// non-nil *gosmtp.SMTPError when it rejects — same shape as the C.6 DKIM gate
// (dkim_gate.go), so the policy table from docs/goal/behavior/smtp-server.md
// § Error / tempfail strategy is unit-tested without driving verify_inbound to
// crafted outcomes.
//
// Verification vs enforcement: verify_inbound (libs/fauna-mail/src/auth.rs)
// computes the verdicts; this file acts on them. The Go bridge owns
// enforcement now — the goal doc's "enforcement runs in the Rust daemon" prose
// describes the retired transport.
package mta

import (
	gosmtp "github.com/emersion/go-smtp"

	faunaCore "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_core"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// applyAuthEnforceGates runs the inbound auth-enforcement stage and returns a
// non-nil *gosmtp.SMTPError when any gate rejects the message at DATA. Stage
// order matches the legacy block:
//
//  1. DMARC reject  → 550 5.7.1   (the authoritative anti-spoofing policy;
//     runs first, so SPF/DKIM never double-reject a DMARC-decided message)
//  2. SPF hardfail  → 550 5.7.23  (defers to DMARC via dmarcDecided)
//  3. DKIM fail     → 550 5.7.20  (defers to DMARC via dmarcDecided)
//
// LogOnly is observe mode: verdicts are still computed and logged upstream,
// but no gate rejects. It gates the whole stage (the C.6 DKIM gate ignored
// LogOnly before this stage existed; routing it through here fixes that
// uniformly). DMARC *quarantine* is not handled here — it is a disposition
// the C.7 spam gate routes to the recipient's Junk, and (like the legacy)
// stays outside the LogOnly-gated reject block.
func applyAuthEnforceGates(v mailfauna.AuthVerdicts, policy wsrpc.AuthPolicy) *gosmtp.SMTPError {
	if policy.LogOnly {
		return nil
	}
	if err := applyDMARCRejectGate(v, policy.EnforceDmarc); err != nil {
		return err
	}
	if err := applySPFHardfailGate(v, policy.EnforceSpfHardfail); err != nil {
		return err
	}
	return applyDKIMEnforceGate(v, policy.EnforceDkim)
}

// applyDMARCRejectGate fires when the admin enforces DMARC and the message
// failed DMARC with a published `p=reject` policy → 550 5.7.1. Quarantine
// (`p=quarantine`) is intentionally not a reject here; it falls through to the
// spam gate's PolicyJunk disposition (the recipient's Junk).
func applyDMARCRejectGate(v mailfauna.AuthVerdicts, enforceDMARC bool) *gosmtp.SMTPError {
	if !enforceDMARC {
		return nil
	}
	d, isFail := v.Dmarc.(faunaCore.DmarcVerdictFail)
	if !isFail || d.Policy != faunaCore.DmarcPolicyReject {
		return nil
	}
	return &gosmtp.SMTPError{
		Code:         550,
		EnhancedCode: gosmtp.EnhancedCode{5, 7, 1},
		Message:      "DMARC reject",
	}
}

// applySPFHardfailGate fires when:
//
//  1. EnforceSpfHardfail is true (admin opted in; default on), AND
//  2. SPF returned a hard Fail (`-all` disowned the source IP — not SoftFail
//     `~all`, Neutral, or a resolver error), AND
//  3. DMARC did not decide (dmarcDecided — same deference as the DKIM gate).
//
// Returns 550 5.7.23 on fire, nil otherwise. Deferring to DMARC keeps
// DMARC-aligned mail that legitimately breaks envelope SPF (forwarders,
// mailing lists) deliverable — matching the legacy `dmarc_policy == None`
// guard.
func applySPFHardfailGate(v mailfauna.AuthVerdicts, enforceSPFHardfail bool) *gosmtp.SMTPError {
	if !enforceSPFHardfail {
		return nil
	}
	if v.Spf != faunaCore.SpfVerdictFail {
		return nil
	}
	if dmarcDecided(v.Dmarc) {
		return nil
	}
	return &gosmtp.SMTPError{
		Code:         550,
		EnhancedCode: gosmtp.EnhancedCode{5, 7, 23},
		Message:      "SPF reject",
	}
}
