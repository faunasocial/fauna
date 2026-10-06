package mailfauna

import (
	"testing"

	faunaCore "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_core"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// CombinedSpamScoreMilli returns max(rspamd, weighted_bayesian) floored at 0.
// Cold-start (weighted_bayesian = 0) → the combined score is just rspamd's
// scaled score; a negative rspamd score floors to 0.
func TestCombinedSpamScoreMilli(t *testing.T) {
	if got := CombinedSpamScoreMilli(3000, 7500); got != 7500 {
		t.Errorf("max(3000,7500): got %d, want 7500", got)
	}
	if got := CombinedSpamScoreMilli(4200, 0); got != 4200 {
		t.Errorf("cold-start max(4200,0): got %d, want 4200", got)
	}
	if got := CombinedSpamScoreMilli(-1500, 0); got != 0 {
		t.Errorf("negative rspamd floors to 0: got %d, want 0", got)
	}
}

// defaultPolicy mirrors the permissive auto-Junk catalog default
// (SpamPolicyThresholds::default): spam_folder=5,
// reject=0 (off).
func defaultPolicy() SpamPolicy {
	return SpamPolicy{
		SpamFolderThreshold:  5,
		RejectThreshold:      0,
		HonorDmarcQuarantine: true,
	}
}

func cleanVerdicts() AuthVerdicts {
	return AuthVerdicts{
		Dkim:  faunaCore.DkimVerdictPass{},
		Spf:   faunaCore.SpfVerdictPass,
		Dmarc: faunaCore.DmarcVerdictPass{},
		Arc:   faunaCore.ArcVerdictNone,
	}
}

// The permissive default auto-files high-scoring mail to Junk but never
// 550-rejects on the content score.
func TestDecideDefaultAutoJunksNeverRejects(t *testing.T) {
	p := defaultPolicy()
	// Below spam_folder (5 points = 5000 milli) → INBOX.
	if d := DecideSpamDisposition(4999, cleanVerdicts(), p); d != SpamDispositionAccept {
		t.Errorf("below threshold: got %d, want Accept", d)
	}
	// At/above spam_folder → Junk.
	if d := DecideSpamDisposition(5000, cleanVerdicts(), p); d != SpamDispositionAcceptToSpamFolder {
		t.Errorf("at threshold: got %d, want AcceptToSpamFolder", d)
	}
	// Very high score still only Junk — no default reject.
	if d := DecideSpamDisposition(30000, cleanVerdicts(), p); d != SpamDispositionAcceptToSpamFolder {
		t.Errorf("high score: got %d, want AcceptToSpamFolder (no default reject)", d)
	}
}

// An admin opts into the 550-reject by setting reject_threshold != 0.
func TestDecideAdminOptInReject(t *testing.T) {
	p := SpamPolicy{
		SpamFolderThreshold:  5,
		RejectThreshold:      15,
		HonorDmarcQuarantine: true,
	}
	if d := DecideSpamDisposition(15000, cleanVerdicts(), p); d != SpamDispositionReject {
		t.Errorf("at reject threshold: got %d, want Reject", d)
	}
	if d := DecideSpamDisposition(14999, cleanVerdicts(), p); d != SpamDispositionAcceptToSpamFolder {
		t.Errorf("below reject: got %d, want AcceptToSpamFolder", d)
	}
}

// A DMARC Reject-policy fail does NOT short-circuit to Reject (T1.5):
// p=reject enforcement moved to the auth-enforce gate (550 5.7.1).
func TestDecideDMARCRejectIsNotAShortCircuit(t *testing.T) {
	verdicts := cleanVerdicts()
	verdicts.Dmarc = faunaCore.DmarcVerdictFail{Policy: faunaCore.DmarcPolicyReject}
	if d := DecideSpamDisposition(0, verdicts, defaultPolicy()); d != SpamDispositionAccept {
		t.Errorf("DMARC reject + zero score: got %d, want Accept", d)
	}
}

// A DMARC Quarantine-policy fail short-circuits to PolicyJunk (the
// recipient's Junk) regardless of the score, honoring the sender's published
// p=quarantine policy.
func TestDecideDMARCQuarantineFilesToJunk(t *testing.T) {
	verdicts := cleanVerdicts()
	verdicts.Dmarc = faunaCore.DmarcVerdictFail{Policy: faunaCore.DmarcPolicyQuarantine}
	if d := DecideSpamDisposition(0, verdicts, defaultPolicy()); d != SpamDispositionPolicyJunk {
		t.Errorf("DMARC quarantine: got %d, want PolicyJunk", d)
	}
}

// FoldSpamModelBaseline reaches the shared Rust fade through the FFI: a
// below-confidence own model gains the baseline's n-grams (a distinctive
// baseline token becomes scoreable), and the tolerance arms return the model
// bytes unchanged. The fade math itself is covered by the Rust unit tests
// (SpamModel::fold_baseline_faded); this proves the Go symbol end-to-end.
func TestFoldSpamModelBaselineFolds(t *testing.T) {
	train := func(model []byte, text string, isSpam bool) []byte {
		return ApplySpamTraining(model, text, isSpam).NewModelBytes
	}
	var baseline []byte
	for i := 0; i < 3; i++ {
		baseline = train(baseline, "qzbasetokwx urgent offer", true)
		baseline = train(baseline, "ham note", false)
	}
	var own []byte
	own = train(own, "cheap pills", true)
	own = train(own, "team lunch", false)

	knobs := BayesianKnobs{BayesianWeightMilli: 700, MinSamples: 1, FullConfidenceSamples: 4}
	spamText := "qzbasetokwx urgent offer"
	before := WeightedBayesianMilliForModel(own, spamText, knobs)

	folded := FoldSpamModelBaseline(own, baseline, 4)
	if string(folded) == string(own) {
		t.Fatalf("fold must change a below-confidence model")
	}
	after := WeightedBayesianMilliForModel(folded, spamText, knobs)
	if after <= before {
		t.Errorf(
			"folding the spam-trained baseline must raise the baseline-token score: %d -> %d",
			before, after,
		)
	}

	// Tolerance: empty baseline and a 0 horizon return the input unchanged.
	if got := FoldSpamModelBaseline(own, nil, 4); string(got) != string(own) {
		t.Errorf("empty baseline must be a verbatim no-op")
	}
	if got := FoldSpamModelBaseline(own, baseline, 0); string(got) != string(own) {
		t.Errorf("0 horizon must be a verbatim no-op")
	}
}

// SpamPolicyFromSnapshot maps the wire SpamPolicyThresholds + AuthPolicy
// onto fauna_mail::SpamPolicy. The two threshold fields ride straight
// through (0 = disabled is preserved); HonorDmarcQuarantine comes from
// AuthPolicy.EnforceDmarcQuarantine. DMARC *reject* (AuthPolicy.EnforceDmarc)
// is consumed by the auth-enforce gate, not the scorer.
func TestSpamPolicyFromSnapshot(t *testing.T) {
	spam := wsrpc.SpamPolicyThresholds{
		MaxScoreBeforeSpamFolder: 6,
		MaxScoreBeforeReject:     20,
	}
	auth := wsrpc.AuthPolicy{
		EnforceDmarc:           true,
		EnforceDmarcQuarantine: true,
	}
	got := SpamPolicyFromSnapshot(spam, auth)
	if got.SpamFolderThreshold != 6 {
		t.Errorf("SpamFolderThreshold: got %d, want 6", got.SpamFolderThreshold)
	}
	if got.RejectThreshold != 20 {
		t.Errorf("RejectThreshold: got %d, want 20", got.RejectThreshold)
	}
	if !got.HonorDmarcQuarantine {
		t.Errorf("HonorDmarcQuarantine: got false, want true (EnforceDmarcQuarantine=true)")
	}
}

// SpamDispositionToWire maps each Rust SpamDisposition variant onto
// the IngestInboundMailParams.SpamDisposition wire string. Reject
// BayesianKnobsFromSnapshot maps the Tier-2 per-user `mail.spam.bayesian_*`
// wire fields onto the BayesianKnobs the shared scorer consumes (weight /
// cold-start floor / confidence-ramp horizon). TrainingHistoryRetentionDays is
// nest-consumed only and not mapped.
func TestBayesianKnobsFromSnapshot(t *testing.T) {
	spam := wsrpc.SpamPolicyThresholds{
		BayesianWeightMilli:           850,
		BayesianMinSamples:            40,
		BayesianFullConfidenceSamples: 300,
		TrainingHistoryRetentionDays:  14, // ignored by the bridge
	}
	got := BayesianKnobsFromSnapshot(spam)
	if got.BayesianWeightMilli != 850 {
		t.Errorf("BayesianWeightMilli: got %d, want 850", got.BayesianWeightMilli)
	}
	if got.MinSamples != 40 {
		t.Errorf("MinSamples: got %d, want 40", got.MinSamples)
	}
	if got.FullConfidenceSamples != 300 {
		t.Errorf("FullConfidenceSamples: got %d, want 300", got.FullConfidenceSamples)
	}
	// The catalog defaults round-trip too (the unseeded path).
	def := BayesianKnobsFromSnapshot(wsrpc.DefaultSpamPolicyThresholds())
	if def != DefaultBayesianKnobs() {
		t.Errorf("default snapshot knobs = %+v, want DefaultBayesianKnobs %+v", def, DefaultBayesianKnobs())
	}
}

// returns ("", true) — the bridge 5xx's instead of ingesting; the
// other three return their canonical snake_case wire token + false.
func TestSpamDispositionToWire(t *testing.T) {
	cases := []struct {
		in      SpamDisposition
		wantStr string
		wantRej bool
	}{
		{SpamDispositionAccept, "accept", false},
		{SpamDispositionAcceptToSpamFolder, "accept_to_spam_folder", false},
		{SpamDispositionPolicyJunk, "policy_junk", false},
		{SpamDispositionReject, "", true},
	}
	for _, tc := range cases {
		gotStr, gotRej := SpamDispositionToWire(tc.in)
		if gotStr != tc.wantStr || gotRej != tc.wantRej {
			t.Errorf("disposition=%d: got (%q,%v), want (%q,%v)",
				tc.in, gotStr, gotRej, tc.wantStr, tc.wantRej)
		}
	}
}
