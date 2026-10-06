// Phase C.6 — DKIM enforce-on-fail policy gate.
//
// The gate is the canonical example of the spec's "verify" (C.5, emits
// verdicts) vs "enforce" (this file, acts on verdicts) split: a thin
// policy decision wired into Session.Data after verify_inbound returns.
// Kept as a pure function on (verdicts, enforce flag) so the policy
// table from docs/goal/behavior/smtp-server.md § DKIM verdict pipeline
// is unit-tested without needing to drive verify_inbound to specific
// outcomes via crafted DKIM signatures.
//
// The
// new shape preserves the legacy behaviour for Fail{policy=None}
// (treated as "DMARC did not decide" → gate is allowed to fire) while
// adopting the C.5 nested-enum verdicts.
package mta

import (
	gosmtp "github.com/emersion/go-smtp"

	faunaCore "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_core"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
)

// applyDKIMEnforceGate returns a non-nil *gosmtp.SMTPError when the
// DKIM enforce-on-fail policy gate fires:
//
//  1. AuthPolicy.EnforceDkim is true (admin opted in), AND
//  2. the DKIM verdict is Fail, AND
//  3. DMARC did not decide (see dmarcDecided).
//
// Returns nil otherwise — the message continues down the inbound
// pipeline.
func applyDKIMEnforceGate(v mailfauna.AuthVerdicts, enforceDKIM bool) *gosmtp.SMTPError {
	if !enforceDKIM {
		return nil
	}
	if _, isFail := v.Dkim.(faunaCore.DkimVerdictFail); !isFail {
		return nil
	}
	if dmarcDecided(v.Dmarc) {
		return nil
	}
	return &gosmtp.SMTPError{
		Code:         550,
		EnhancedCode: gosmtp.EnhancedCode{5, 7, 20},
		Message:      "DKIM signature missing or invalid",
	}
}

// dmarcDecided reports whether DMARC has an authoritative opinion the
// downstream pipeline will act on:
//
//   - Pass — DMARC vouched for the message (alignment succeeded).
//   - Fail{Quarantine | Reject} — the DMARC handler will quarantine
//     or reject; the DKIM gate must defer.
//
// All other verdicts (None, Fail{policy=None}, PermError, TempError)
// leave room for the DKIM enforce gate to fire when configured.
// Fail{policy=None} in particular is the publisher's explicit "I'm in
// observation mode" — the DMARC handler will not reject — matching the
// legacy `dmarc_policy == Policy::None` behaviour.
func dmarcDecided(v mailfauna.DmarcVerdict) bool {
	switch d := v.(type) {
	case faunaCore.DmarcVerdictPass:
		return true
	case faunaCore.DmarcVerdictFail:
		return d.Policy == faunaCore.DmarcPolicyQuarantine ||
			d.Policy == faunaCore.DmarcPolicyReject
	default:
		return false
	}
}
