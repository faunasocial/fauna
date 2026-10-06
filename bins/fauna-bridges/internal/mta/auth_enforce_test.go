package mta

import (
	"testing"

	gosmtp "github.com/emersion/go-smtp"

	faunaCore "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_core"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// T1.5 — inbound auth-enforcement stage (`applyAuthEnforceGates`).
//
// Spec: docs/goal/behavior/smtp-server.md §§ Inbound policy stack /
// Error-tempfail strategy. The single stage runs DMARC-reject →
// SPF-hardfail → DKIM in spec order, all behind `!LogOnly`, mirroring the
// legacy enforcement block bins/fauna-bridge-daemon/src/handler.rs:1260-1297
// (retired daemon; resolved via git history).
//
//   - DMARC reject  → 550 5.7.1   (EnforceDmarc + Dmarc==Fail{Reject})
//   - SPF hardfail  → 550 5.7.23  (EnforceSpfHardfail + Spf==Fail, deferring
//                                  to DMARC via dmarcDecided)
//   - DKIM fail     → 550 5.7.20  (delegates to applyDKIMEnforceGate — the C.6
//                                  matrix is exhaustively covered in
//                                  dkim_gate_test.go; one delegation smoke
//                                  test here)
//
// DMARC *quarantine* is deliberately NOT an enforce reject — it routes to the
// PolicyJunk disposition (Junk) in the C.7 spam gate (legacy left it downstream of
// the log_only block). These tests assert quarantine continues here.

// verdicts3 builds AuthVerdicts with explicit SPF / DKIM / DMARC verdicts
// (ARC fixed to None — orthogonal to the enforce gates).
func verdicts3(spf faunaCore.SpfVerdict, dkim faunaCore.DkimVerdict, dmarc faunaCore.DmarcVerdict) mailfauna.AuthVerdicts {
	return mailfauna.AuthVerdicts{Dkim: dkim, Spf: spf, Dmarc: dmarc, Arc: faunaCore.ArcVerdictNone}
}

// enforceAllPolicy turns every auth-enforce knob on (LogOnly off) — the
// strictest posture, so a test that expects "continue" proves the verdict
// itself (not a disabled knob) drove the decision.
func enforceAllPolicy() wsrpc.AuthPolicy {
	return wsrpc.AuthPolicy{
		EnforceDmarc:           true,
		EnforceDmarcQuarantine: true,
		EnforceSpfHardfail:     true,
		EnforceDkim:            true,
		LogOnly:                false,
	}
}

// ── SPF hardfail ────────────────────────────────────────────────────────────

func TestAuthEnforce_SPFHardfail_FiresOnFailDMARCNone(t *testing.T) {
	t.Parallel()
	got := applyAuthEnforceGates(verdicts3(faunaCore.SpfVerdictFail, dkimPass(), dmarcNone()), enforceAllPolicy())
	if got == nil {
		t.Fatal("SPF hardfail gate must fire on SPF fail + DMARC none + enforce on")
	}
	if got.Code != 550 || got.EnhancedCode != (gosmtp.EnhancedCode{5, 7, 23}) {
		t.Errorf("wire: got %d %v, want 550 5.7.23", got.Code, got.EnhancedCode)
	}
}

// p=none publisher (DMARC Fail{None}) is observation mode → DMARC won't act,
// so the SPF hardfail gate is allowed to fire. Mirrors the legacy
// `dmarc_policy == None` guard and the DKIM gate's Fail{None} case.
func TestAuthEnforce_SPFHardfail_FiresOnDMARCFailPolicyNone(t *testing.T) {
	t.Parallel()
	got := applyAuthEnforceGates(verdicts3(faunaCore.SpfVerdictFail, dkimPass(), dmarcFailNone()), enforceAllPolicy())
	if got == nil {
		t.Fatal("SPF hardfail gate must fire when DMARC published p=none")
	}
	if got.EnhancedCode != (gosmtp.EnhancedCode{5, 7, 23}) {
		t.Errorf("enhanced code: got %v, want 5.7.23", got.EnhancedCode)
	}
}

// DMARC passed (alignment succeeded, e.g. via DKIM on forwarded mail) → the
// SPF hardfail gate must defer; rejecting here would drop DMARC-legit mail.
func TestAuthEnforce_SPFHardfail_DefersToDMARCPass(t *testing.T) {
	t.Parallel()
	policy := enforceAllPolicy()
	policy.EnforceDkim = false // isolate: only SPF/DMARC knobs in play
	if got := applyAuthEnforceGates(verdicts3(faunaCore.SpfVerdictFail, dkimNone(), dmarcPass()), policy); got != nil {
		t.Errorf("SPF hardfail gate must defer to DMARC pass; got %v", got)
	}
}

func TestAuthEnforce_SPFHardfail_SkipsOnSoftFail(t *testing.T) {
	t.Parallel()
	if got := applyAuthEnforceGates(verdicts3(faunaCore.SpfVerdictSoftFail, dkimPass(), dmarcNone()), enforceAllPolicy()); got != nil {
		t.Errorf("SPF softfail must not reject (score-only); got %v", got)
	}
}

func TestAuthEnforce_SPFHardfail_SkipsOnNeutral(t *testing.T) {
	t.Parallel()
	if got := applyAuthEnforceGates(verdicts3(faunaCore.SpfVerdictNeutral, dkimPass(), dmarcNone()), enforceAllPolicy()); got != nil {
		t.Errorf("SPF neutral must not reject; got %v", got)
	}
}

func TestAuthEnforce_SPFHardfail_SkipsWhenPolicyOff(t *testing.T) {
	t.Parallel()
	policy := enforceAllPolicy()
	policy.EnforceSpfHardfail = false
	if got := applyAuthEnforceGates(verdicts3(faunaCore.SpfVerdictFail, dkimPass(), dmarcNone()), policy); got != nil {
		t.Errorf("SPF hardfail gate must not fire when EnforceSpfHardfail=false; got %v", got)
	}
}

// ── DMARC reject ──────────────────────────────────────────────────────────

func TestAuthEnforce_DMARCReject_FiresOnFailReject(t *testing.T) {
	t.Parallel()
	got := applyAuthEnforceGates(verdicts3(faunaCore.SpfVerdictPass, dkimPass(), dmarcFailReject()), enforceAllPolicy())
	if got == nil {
		t.Fatal("DMARC reject gate must fire on DMARC Fail{Reject} + EnforceDmarc")
	}
	if got.Code != 550 || got.EnhancedCode != (gosmtp.EnhancedCode{5, 7, 1}) {
		t.Errorf("wire: got %d %v, want 550 5.7.1", got.Code, got.EnhancedCode)
	}
}

func TestAuthEnforce_DMARCReject_SkipsWhenPolicyOff(t *testing.T) {
	t.Parallel()
	policy := enforceAllPolicy()
	policy.EnforceDmarc = false
	if got := applyAuthEnforceGates(verdicts3(faunaCore.SpfVerdictPass, dkimPass(), dmarcFailReject()), policy); got != nil {
		t.Errorf("DMARC reject gate must not fire when EnforceDmarc=false; got %v", got)
	}
}

// DMARC quarantine is a disposition (spam gate), never a 5xx reject here.
func TestAuthEnforce_DMARCReject_SkipsOnQuarantine(t *testing.T) {
	t.Parallel()
	policy := enforceAllPolicy()
	policy.EnforceDkim = false // isolate DMARC; DKIM pass anyway
	if got := applyAuthEnforceGates(verdicts3(faunaCore.SpfVerdictPass, dkimPass(), dmarcFailQuarantine()), policy); got != nil {
		t.Errorf("DMARC quarantine must not reject at the auth stage; got %v", got)
	}
}

func TestAuthEnforce_DMARCReject_SkipsOnPass(t *testing.T) {
	t.Parallel()
	policy := enforceAllPolicy()
	policy.EnforceDkim = false
	policy.EnforceSpfHardfail = false
	if got := applyAuthEnforceGates(verdicts3(faunaCore.SpfVerdictPass, dkimPass(), dmarcPass()), policy); got != nil {
		t.Errorf("DMARC pass must not reject; got %v", got)
	}
}

// ── DKIM delegation ───────────────────────────────────────────────────────

func TestAuthEnforce_DKIM_DelegatesToDKIMGate(t *testing.T) {
	t.Parallel()
	got := applyAuthEnforceGates(verdicts3(faunaCore.SpfVerdictPass, dkimFail(), dmarcNone()), enforceAllPolicy())
	if got == nil {
		t.Fatal("DKIM enforce must fire (delegated) on DKIM fail + DMARC none")
	}
	if got.Code != 550 || got.EnhancedCode != (gosmtp.EnhancedCode{5, 7, 20}) {
		t.Errorf("wire: got %d %v, want 550 5.7.20", got.Code, got.EnhancedCode)
	}
}

// ── LogOnly (observe mode suppresses ALL enforcement) ─────────────────────

func TestAuthEnforce_LogOnly_SuppressesDMARCReject(t *testing.T) {
	t.Parallel()
	policy := enforceAllPolicy()
	policy.LogOnly = true
	if got := applyAuthEnforceGates(verdicts3(faunaCore.SpfVerdictPass, dkimPass(), dmarcFailReject()), policy); got != nil {
		t.Errorf("LogOnly must suppress DMARC reject; got %v", got)
	}
}

func TestAuthEnforce_LogOnly_SuppressesSPFHardfail(t *testing.T) {
	t.Parallel()
	policy := enforceAllPolicy()
	policy.LogOnly = true
	if got := applyAuthEnforceGates(verdicts3(faunaCore.SpfVerdictFail, dkimPass(), dmarcNone()), policy); got != nil {
		t.Errorf("LogOnly must suppress SPF hardfail; got %v", got)
	}
}

func TestAuthEnforce_LogOnly_SuppressesDKIM(t *testing.T) {
	t.Parallel()
	policy := enforceAllPolicy()
	policy.LogOnly = true
	if got := applyAuthEnforceGates(verdicts3(faunaCore.SpfVerdictPass, dkimFail(), dmarcNone()), policy); got != nil {
		t.Errorf("LogOnly must suppress DKIM enforce; got %v", got)
	}
}

// ── Ordering / precedence ──────────────────────────────────────────────────

// DMARC reject takes precedence: when SPF hardfails AND DKIM fails AND DMARC
// is Fail{Reject}, the wire code is the DMARC one (550 5.7.1), proving DMARC
// runs first in the stage order.
func TestAuthEnforce_DMARCRejectTakesPrecedence(t *testing.T) {
	t.Parallel()
	got := applyAuthEnforceGates(verdicts3(faunaCore.SpfVerdictFail, dkimFail(), dmarcFailReject()), enforceAllPolicy())
	if got == nil {
		t.Fatal("expected a reject")
	}
	if got.EnhancedCode != (gosmtp.EnhancedCode{5, 7, 1}) {
		t.Errorf("DMARC reject must win the ordering; got %v, want 5.7.1", got.EnhancedCode)
	}
}

func TestAuthEnforce_AllPass_Continues(t *testing.T) {
	t.Parallel()
	v := verdicts3(faunaCore.SpfVerdictPass, dkimPass(), dmarcPass())
	if got := applyAuthEnforceGates(v, enforceAllPolicy()); got != nil {
		t.Errorf("clean message must continue; got %v", got)
	}
}
