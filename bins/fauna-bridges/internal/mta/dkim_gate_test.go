package mta

import (
	"testing"

	gosmtp "github.com/emersion/go-smtp"

	faunaCore "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_core"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
)

// Phase C.6 — DKIM enforce-on-fail policy gate.
//
// Spec: docs/goal/behavior/smtp-server.md § DKIM verdict pipeline + § Error
// / tempfail strategy ("DKIM-fail with `enforce_dkim` enabled (and no DMARC
// `p=` decision) → 550 5.7.20").
//
// The gate operates on the C.5 verdicts post-verify_inbound. "DMARC decided"
// means DMARC has an authoritative opinion the downstream pipeline acts on:
// Pass (DMARC vouched for the message) or Fail{Quarantine|Reject} (DMARC's
// handler will quarantine/reject). Fail{None} and the error/none verdicts
// leave room for the DKIM gate to act.

func dkimFail() faunaCore.DkimVerdict { return faunaCore.DkimVerdictFail{Reason: "bad signature"} }
func dkimPass() faunaCore.DkimVerdict { return faunaCore.DkimVerdictPass{} }
func dkimNone() faunaCore.DkimVerdict { return faunaCore.DkimVerdictNone{} }

func dmarcNone() faunaCore.DmarcVerdict { return faunaCore.DmarcVerdictNone{} }
func dmarcPass() faunaCore.DmarcVerdict { return faunaCore.DmarcVerdictPass{} }
func dmarcFailReject() faunaCore.DmarcVerdict {
	return faunaCore.DmarcVerdictFail{Policy: faunaCore.DmarcPolicyReject}
}
func dmarcFailQuarantine() faunaCore.DmarcVerdict {
	return faunaCore.DmarcVerdictFail{Policy: faunaCore.DmarcPolicyQuarantine}
}
func dmarcFailNone() faunaCore.DmarcVerdict {
	return faunaCore.DmarcVerdictFail{Policy: faunaCore.DmarcPolicyNone}
}

func verdictsWith(dkim faunaCore.DkimVerdict, dmarc faunaCore.DmarcVerdict) mailfauna.AuthVerdicts {
	return mailfauna.AuthVerdicts{
		Dkim:  dkim,
		Spf:   faunaCore.SpfVerdictNone,
		Dmarc: dmarc,
		Arc:   faunaCore.ArcVerdictNone,
	}
}

// (a) enforce_dkim=true + DKIM fail + DMARC none → 550 5.7.20.
func TestDKIMEnforceGate_FiresOnDKIMFailDMARCNone(t *testing.T) {
	t.Parallel()
	got := applyDKIMEnforceGate(verdictsWith(dkimFail(), dmarcNone()), true)
	if got == nil {
		t.Fatal("gate must fire on DKIM fail + DMARC none + enforce_dkim=true")
	}
	if got.Code != 550 {
		t.Errorf("reject code: got %d, want 550", got.Code)
	}
	if got.EnhancedCode != (gosmtp.EnhancedCode{5, 7, 20}) {
		t.Errorf("enhanced code: got %v, want {5,7,20}", got.EnhancedCode)
	}
}

// (b) enforce_dkim=true + DKIM fail + DMARC pass → continue (DMARC vouched).
func TestDKIMEnforceGate_SkipsOnDMARCPass(t *testing.T) {
	t.Parallel()
	if got := applyDKIMEnforceGate(verdictsWith(dkimFail(), dmarcPass()), true); got != nil {
		t.Errorf("gate must not fire when DMARC passed; got %v", got)
	}
}

// (c) enforce_dkim=true + DKIM fail + DMARC reject → continue (DMARC will
// reject downstream).
func TestDKIMEnforceGate_SkipsOnDMARCReject(t *testing.T) {
	t.Parallel()
	if got := applyDKIMEnforceGate(verdictsWith(dkimFail(), dmarcFailReject()), true); got != nil {
		t.Errorf("gate must not fire when DMARC will reject; got %v", got)
	}
}

// DMARC Fail{Quarantine} is also a decision the downstream pipeline acts on;
// the gate must defer to it.
func TestDKIMEnforceGate_SkipsOnDMARCQuarantine(t *testing.T) {
	t.Parallel()
	if got := applyDKIMEnforceGate(verdictsWith(dkimFail(), dmarcFailQuarantine()), true); got != nil {
		t.Errorf("gate must not fire when DMARC quarantines; got %v", got)
	}
}

// DMARC Fail{None} means the publisher published p=none ("observation
// mode"); the DMARC handler won't act, so the DKIM enforce gate is allowed
// to fire — matches legacy `dmarc_policy == Policy::None` behaviour.
func TestDKIMEnforceGate_FiresOnDMARCFailPolicyNone(t *testing.T) {
	t.Parallel()
	got := applyDKIMEnforceGate(verdictsWith(dkimFail(), dmarcFailNone()), true)
	if got == nil {
		t.Fatal("gate must fire when DMARC fail policy=none (publisher opted out)")
	}
	if got.Code != 550 {
		t.Errorf("reject code: got %d, want 550", got.Code)
	}
}

// (d) enforce_dkim=false + DKIM fail → continue (the policy is off-by-default
// per docs/goal/behavior/mail-policy-config.md § Inbound hardening).
func TestDKIMEnforceGate_SkipsWhenPolicyOff(t *testing.T) {
	t.Parallel()
	if got := applyDKIMEnforceGate(verdictsWith(dkimFail(), dmarcNone()), false); got != nil {
		t.Errorf("gate must not fire when enforce_dkim=false; got %v", got)
	}
}

// DKIM pass — gate must never fire regardless of DMARC.
func TestDKIMEnforceGate_SkipsOnDKIMPass(t *testing.T) {
	t.Parallel()
	if got := applyDKIMEnforceGate(verdictsWith(dkimPass(), dmarcNone()), true); got != nil {
		t.Errorf("gate must not fire on DKIM pass; got %v", got)
	}
}

// DKIM none — the absence of any signature is treated as DkimVerdictNone
// (not Fail). The gate only fires on Fail; absent signatures are an
// orthogonal concern that DMARC or other layers handle.
func TestDKIMEnforceGate_SkipsOnDKIMNone(t *testing.T) {
	t.Parallel()
	if got := applyDKIMEnforceGate(verdictsWith(dkimNone(), dmarcNone()), true); got != nil {
		t.Errorf("gate must not fire on DKIM none; got %v", got)
	}
}
