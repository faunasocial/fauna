package mailfauna

import (
	"testing"

	faunaCore "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_core"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// PerimeterMailScoreRows is the Go MTA's only source of bus rows (the
// contract phase of content-scoring.md § The scoring-metadata bus): one row
// per factor that actually produced a verdict, minted by the shared-Rust
// mapping and handed back in the wire shape. This pins the seam end to end —
// the UniFFI call, the wire mapping and the units — so a Go-side rewrite of
// the mapping (the thing the contract phase forbids) or a regen that drops
// the export both red here.
func TestPerimeterMailScoreRowsMintsTheWireRows(t *testing.T) {
	rspamd := RspamdScore{RawMilli: 2400, ScaledMilli: 1200, FlaggedRules: []string{"URIBL_BLACK"}}
	verdicts := AuthVerdicts{
		Dkim:  faunaCore.DkimVerdictPass{},
		Spf:   faunaCore.SpfVerdictSoftFail,
		Dmarc: faunaCore.DmarcVerdictFail{Policy: faunaCore.DmarcPolicyQuarantine},
		Arc:   faunaCore.ArcVerdictNone,
	}
	rows := PerimeterMailScoreRows(1875, ClamavVerdictInfected{Signature: "Eicar-Test-Signature"}, &rspamd, verdicts)
	byFactor := map[string]wsrpc.ScoreEntry{}
	for _, r := range rows {
		byFactor[r.Factor] = r
	}
	if len(rows) != 6 {
		t.Fatalf("want 6 rows (arc None emits none), got %d: %+v", len(rows), rows)
	}
	want := map[string]struct {
		score int64
		tier  uint8
	}{
		"spam":       {1875, 1}, // milli-points, as passed — never floored to points
		"clamav":     {1000, wsrpc.TierAdmin},
		"rspamd":     {1200, wsrpc.TierAdmin},
		"auth_spf":   {500, wsrpc.TierAdmin},
		"auth_dkim":  {0, wsrpc.TierAdmin},
		"auth_dmarc": {1000, wsrpc.TierAdmin},
	}
	for factor, w := range want {
		got, ok := byFactor[factor]
		if !ok {
			t.Fatalf("missing factor %q in %+v", factor, rows)
		}
		if got.Score != w.score || got.Tier != w.tier {
			t.Errorf("%s: got (score %d, tier %d), want (%d, %d)", factor, got.Score, got.Tier, w.score, w.tier)
		}
		if got.ScorerVersion == 0 {
			t.Errorf("%s: scorer_version must be the registry's current watermark, got 0", factor)
		}
	}
	if _, has := byFactor["auth_arc"]; has {
		t.Errorf("arc None must emit no row: %+v", rows)
	}
}

// Nothing ran but the spam scorer: an oversize bypass, no rspamd, every auth
// verdict None → only the spam row survives.
func TestPerimeterMailScoreRowsSkipsWhatDidNotRun(t *testing.T) {
	rows := PerimeterMailScoreRows(0, ClamavVerdictBypassedOversize{}, nil, AuthVerdicts{
		Dkim:  faunaCore.DkimVerdictNone{},
		Spf:   faunaCore.SpfVerdictNone,
		Dmarc: faunaCore.DmarcVerdictNone{},
		Arc:   faunaCore.ArcVerdictNone,
	})
	if len(rows) != 1 || rows[0].Factor != "spam" || rows[0].Score != 0 {
		t.Fatalf("only the spam row survives an unscored delivery, got %+v", rows)
	}
}

// The submission twin's inputs (fauna_recipient.go): synthetic pass verdicts,
// zero spam, no rspamd, and NotScanned for ClamAV (submission never invokes
// the scan gate) → spam 0 and three pass rows, and NO clamav row — the twin's
// nest-side detail record (no message_scan_results row) agrees.
func TestPerimeterMailScoreRowsSubmissionTwinClaimsNoScan(t *testing.T) {
	rows := PerimeterMailScoreRows(0, ClamavVerdictNotScanned{}, nil, AuthVerdicts{
		Dkim:  faunaCore.DkimVerdictPass{},
		Spf:   faunaCore.SpfVerdictPass,
		Dmarc: faunaCore.DmarcVerdictPass{},
		Arc:   faunaCore.ArcVerdictNone,
	})
	byFactor := map[string]wsrpc.ScoreEntry{}
	for _, r := range rows {
		byFactor[r.Factor] = r
	}
	if _, has := byFactor["clamav"]; has {
		t.Fatalf("a ClamAV scan that never ran must emit no row: %+v", rows)
	}
	if len(rows) != 4 || byFactor["spam"].Score != 0 {
		t.Fatalf("want spam 0 + three pass rows, got %+v", rows)
	}
}
