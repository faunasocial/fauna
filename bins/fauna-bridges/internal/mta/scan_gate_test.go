package mta

import (
	"bufio"
	"context"
	"encoding/binary"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/metrics"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/scan"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/testutil"
)

// fakeClamd starts a one-shot loopback clamd that consumes an INSTREAM upload
// (zINSTREAM\0, then uint32-BE-len frames terminated by a zero-length frame)
// and writes the canned reply. Returns its host:port and a cleanup.
func fakeClamd(t *testing.T, reply string) (string, func()) {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen fake clamd: %v", err)
	}
	go func() {
		conn, err := ln.Accept()
		if err != nil {
			return
		}
		defer func() { _ = conn.Close() }()
		r := bufio.NewReader(conn)
		// Command line "zINSTREAM\0".
		if _, err := r.ReadString(0); err != nil {
			return
		}
		for {
			var lb [4]byte
			if _, err := io.ReadFull(r, lb[:]); err != nil {
				return
			}
			n := binary.BigEndian.Uint32(lb[:])
			if n == 0 {
				break
			}
			if _, err := io.CopyN(io.Discard, r, int64(n)); err != nil {
				return
			}
		}
		_, _ = conn.Write([]byte(reply))
	}()
	return ln.Addr().String(), func() { _ = ln.Close() }
}

// fakeRspamd returns an httptest server answering /checkv2 with the given JSON
// (status 200) and a ScanConfig HTTPClient wired to it.
func fakeRspamd(t *testing.T, json string) (*httptest.Server, string) {
	t.Helper()
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_, _ = io.WriteString(w, json)
	}))
	t.Cleanup(srv.Close)
	return srv, srv.URL
}

func defaultTestPolicy() mailfauna.ScanPolicy {
	return scan.PolicyDefault()
}

func TestApplyScanGate_CleanDelivers(t *testing.T) {
	clamdAddr, stop := fakeClamd(t, "stream: OK\x00")
	defer stop()
	_, rspamdURL := fakeRspamd(t, `{"score": 2.5, "symbols": {"BAYES_SPAM": {"score": 4.0}}}`)
	cfg := scan.Config{ClamdAddr: clamdAddr, RspamdURL: rspamdURL, Policy: defaultTestPolicy()}

	clamav, score, action := applyScanGate(nil, context.Background(), []byte("hello"), cfg, 0, "1.2.3.4", "a@b.test", []string{"c@d.test"})
	if _, ok := clamav.(mailfauna.ClamavVerdictClean); !ok {
		t.Fatalf("clamav = %T, want Clean", clamav)
	}
	if _, ok := action.(mailfauna.ScanActionDeliver); !ok {
		t.Fatalf("action = %T, want Deliver", action)
	}
	if score == nil {
		t.Fatal("rspamd score = nil, want present")
	}
	// 2.5 raw × 0.5 scaling = 1.25 → 1250 milli.
	if score.ScaledMilli != 1250 {
		t.Errorf("ScaledMilli = %d, want 1250", score.ScaledMilli)
	}
}

func TestApplyScanGate_InfectedRejects(t *testing.T) {
	clamdAddr, stop := fakeClamd(t, "stream: Eicar-Test-Signature FOUND\x00")
	defer stop()
	cfg := scan.Config{ClamdAddr: clamdAddr, Policy: defaultTestPolicy()}
	cfg.Policy.RspamdEnabled = false // reject short-circuits before rspamd anyway

	clamav, score, action := applyScanGate(nil, context.Background(), []byte("x"), cfg, 0, "", "", nil)
	inf, ok := clamav.(mailfauna.ClamavVerdictInfected)
	if !ok {
		t.Fatalf("clamav = %T, want Infected", clamav)
	}
	if inf.Signature != "Eicar-Test-Signature" {
		t.Errorf("signature = %q", inf.Signature)
	}
	rej, ok := action.(mailfauna.ScanActionRejectMalware)
	if !ok {
		t.Fatalf("action = %T, want RejectMalware", action)
	}
	if rej.Signature != "Eicar-Test-Signature" {
		t.Errorf("reject signature = %q", rej.Signature)
	}
	if score != nil {
		t.Error("rspamd score should be nil on a reject (rspamd not run)")
	}
}

func TestApplyScanGate_InfectedJunkRunsRspamd(t *testing.T) {
	clamdAddr, stop := fakeClamd(t, "stream: Win.Test.EICAR FOUND\x00")
	defer stop()
	_, rspamdURL := fakeRspamd(t, `{"score": 10.0}`)
	cfg := scan.Config{ClamdAddr: clamdAddr, RspamdURL: rspamdURL, Policy: defaultTestPolicy()}
	cfg.Policy.ClamavActionOnInfected = mailfauna.ClamavActionJunk

	clamav, score, action := applyScanGate(nil, context.Background(), []byte("x"), cfg, 0, "", "", nil)
	if _, ok := clamav.(mailfauna.ClamavVerdictInfected); !ok {
		t.Fatalf("clamav = %T, want Infected", clamav)
	}
	if _, ok := action.(mailfauna.ScanActionJunk); !ok {
		t.Fatalf("action = %T, want Junk", action)
	}
	// The junk action still delivers (to Junk), so rspamd runs + the score rides.
	if score == nil {
		t.Fatal("rspamd score = nil, want present on the junk action")
	}
}

func TestApplyScanGate_ClamdDownTempfails(t *testing.T) {
	// Reserve a port then close it ⇒ a guaranteed-refused dial.
	ln, _ := net.Listen("tcp", "127.0.0.1:0")
	addr := ln.Addr().String()
	_ = ln.Close()
	cfg := scan.Config{ClamdAddr: addr, Policy: defaultTestPolicy()}

	_, _, action := applyScanGate(nil, context.Background(), []byte("x"), cfg, 0, "", "", nil)
	if _, ok := action.(mailfauna.ScanActionTempfail); !ok {
		t.Fatalf("action = %T, want Tempfail (fail-closed on clamd down)", action)
	}
}

func TestApplyScanGate_RspamdDownTempfails(t *testing.T) {
	clamdAddr, stop := fakeClamd(t, "stream: OK\x00")
	defer stop()
	ln, _ := net.Listen("tcp", "127.0.0.1:0")
	rspamdURL := "http://" + ln.Addr().String()
	_ = ln.Close()
	cfg := scan.Config{ClamdAddr: clamdAddr, RspamdURL: rspamdURL, Policy: defaultTestPolicy()}

	_, _, action := applyScanGate(nil, context.Background(), []byte("x"), cfg, 0, "", "", nil)
	if _, ok := action.(mailfauna.ScanActionTempfail); !ok {
		t.Fatalf("action = %T, want Tempfail (fail-closed on rspamd down)", action)
	}
}

func TestApplyScanGate_OversizeBypassesWithoutDialingClamd(t *testing.T) {
	// The DEFENSIVE arm (mail-content-scanning.md, Oversize messages). Since
	// the 2026-08-26 ruling the cap is the live product ceiling, so production
	// cannot reach this arm -- a body over the ceiling was already refused 552
	// at the door. It is reachable only with an injected tiny ceiling, which is
	// what this test does.
	//
	// Bogus clamd addr: if the gate dials it, the test fails on the verdict.
	cfg := scan.Config{ClamdAddr: "127.0.0.1:1", Policy: defaultTestPolicy()}
	cfg.Policy.RspamdEnabled = false

	before := counterValue(t, metrics.MailScanningClamavOversize)

	clamav, _, action := applyScanGate(nil, context.Background(), []byte("oversized body"), cfg, 4, "", "", nil)
	if _, ok := clamav.(mailfauna.ClamavVerdictBypassedOversize); !ok {
		t.Fatalf("clamav = %T, want BypassedOversize", clamav)
	}
	if _, ok := action.(mailfauna.ScanActionDeliver); !ok {
		t.Fatalf("action = %T, want Deliver", action)
	}
	// The tripwire fires. A non-zero value of this counter is a bug in a real
	// deployment; the point of the pin is that the bug is observable at all,
	// which it was not before (the counter had no emitter anywhere).
	if got := counterValue(t, metrics.MailScanningClamavOversize) - before; got != 1 {
		t.Fatalf("mail_scanning_clamav_oversize_total delta = %v, want 1", got)
	}
}

// counterValue reads a prometheus counter's current value. The gate's tripwire
// is a process-wide counter, so the pins below read a *delta* around the call
// rather than an absolute -- test order must not matter.
func counterValue(t *testing.T, c prometheus.Counter) float64 {
	t.Helper()
	return testutil.ToFloat64(c)
}

// TestApplyScanGate_RaisedCeilingStillScans is the regression pin for the defect
// the 2026-08-26 ruling closed: an admin raising max_message_bytes above the
// old compile-time 52,428,800 constant used to open a band delivered UNSCANNED
// and silently. The gate must now dial clamd for a body in that band rather
// than bypass it.
//
// The dial is proved by the failure mode: the clamd address is a closed port, so
// a gate that dials fails closed (Tempfail) and a gate that bypasses returns
// BypassedOversize/Deliver. Asserting "not BypassedOversize" is the load-bearing
// half -- Tempfail here means "it tried to scan", which is exactly the fix.
func TestApplyScanGate_RaisedCeilingStillScans(t *testing.T) {
	cfg := scan.Config{ClamdAddr: "127.0.0.1:1", Policy: defaultTestPolicy()}
	cfg.Policy.RspamdEnabled = false

	// 60 MB -- above the retired 52,428,800 constant, below a 100 MB ceiling.
	raw := make([]byte, 60_000_000)
	before := counterValue(t, metrics.MailScanningClamavOversize)

	clamav, _, action := applyScanGate(newScanGate(), context.Background(), raw, cfg, 100_000_000, "", "", nil)
	if _, ok := clamav.(mailfauna.ClamavVerdictBypassedOversize); ok {
		t.Fatal("clamav = BypassedOversize: a raised ceiling must NOT open an unscanned band")
	}
	if _, ok := action.(mailfauna.ScanActionDeliver); ok {
		t.Fatal("action = Deliver: an unscannable body must fail closed, never deliver unscanned")
	}
	if _, ok := action.(mailfauna.ScanActionTempfail); !ok {
		t.Fatalf("action = %T, want Tempfail (the gate dialled clamd and it was down)", action)
	}
	if got := counterValue(t, metrics.MailScanningClamavOversize) - before; got != 0 {
		t.Fatalf("oversize tripwire fired (delta %v) on a body inside the ceiling", got)
	}
}

func TestApplyScanGate_DisabledScannersNoOp(t *testing.T) {
	// Both disabled, bogus addresses: a no-op NotScanned/Deliver, no dials —
	// and no Clean, which would record a scan that never ran.
	cfg := scan.Config{
		ClamdAddr: "127.0.0.1:1",
		RspamdURL: "http://127.0.0.1:1",
		Policy: mailfauna.ScanPolicy{
			ClamavEnabled:              false,
			ClamavActionOnInfected:     mailfauna.ClamavActionReject,
			RspamdEnabled:              false,
			RspamdScoreScalingPerMille: 500,
		},
	}
	clamav, score, action := applyScanGate(nil, context.Background(), []byte("x"), cfg, 0, "", "", nil)
	if _, ok := clamav.(mailfauna.ClamavVerdictNotScanned); !ok {
		t.Fatalf("clamav = %T, want NotScanned (a disabled scanner computed no verdict)", clamav)
	}
	if _, ok := action.(mailfauna.ScanActionDeliver); !ok {
		t.Fatalf("action = %T, want Deliver", action)
	}
	if score != nil {
		t.Error("rspamd score should be nil when rspamd disabled")
	}
}

// ── D7 scan-gate runtime guards: reply bounds, circuit breaker, in-flight cap ──

func tempfailReason(t *testing.T, action mailfauna.ScanAction) string {
	t.Helper()
	tf, ok := action.(mailfauna.ScanActionTempfail)
	if !ok {
		t.Fatalf("action = %T, want Tempfail", action)
	}
	return tf.Reason
}

func TestScanGate_ClamdReplyReaderBounded(t *testing.T) {
	// A *valid* "OK" reply padded past the cap: unbounded, clamd_parse_reply
	// trims the trailing null + whitespace and sees the "OK" suffix ⇒
	// Clean→Deliver. Bounded, the over-cap read errors ⇒ Tempfail (fail-closed),
	// proving the reader is capped (no OOM on a wedged/compromised daemon).
	reply := strings.Repeat("A", maxClamdReplyBytes) + "stream: OK\x00"
	clamdAddr, stop := fakeClamd(t, reply)
	defer stop()
	cfg := scan.Config{ClamdAddr: clamdAddr, Policy: defaultTestPolicy()}
	cfg.Policy.RspamdEnabled = false

	_, _, action := applyScanGate(newScanGate(), context.Background(), []byte("x"), cfg, 0, "", "", nil)
	if reason := tempfailReason(t, action); !strings.Contains(reason, "exceeds") {
		t.Fatalf("tempfail reason = %q, want it to mention the byte cap (exceeds)", reason)
	}
}

func TestScanGate_RspamdReplyReaderBounded(t *testing.T) {
	clamdAddr, stop := fakeClamd(t, "stream: OK\x00")
	defer stop()
	// Valid JSON whose body exceeds the rspamd cap (the unknown "pad" field is
	// ignored by the parser, so unbounded this would parse to a score).
	huge := `{"score": 2.5, "pad": "` + strings.Repeat("x", maxRspamdReplyBytes) + `"}`
	_, rspamdURL := fakeRspamd(t, huge)
	cfg := scan.Config{ClamdAddr: clamdAddr, RspamdURL: rspamdURL, Policy: defaultTestPolicy()}

	_, _, action := applyScanGate(newScanGate(), context.Background(), []byte("x"), cfg, 0, "", "", nil)
	if reason := tempfailReason(t, action); !strings.Contains(reason, "exceeds") {
		t.Fatalf("tempfail reason = %q, want it to mention the byte cap (exceeds)", reason)
	}
}

func TestScanGate_CircuitBreakerOpensAfterFailures(t *testing.T) {
	// Reserve a port then close it ⇒ a guaranteed-refused (fast) dial.
	ln, _ := net.Listen("tcp", "127.0.0.1:0")
	addr := ln.Addr().String()
	_ = ln.Close()
	cfg := scan.Config{ClamdAddr: addr, Policy: defaultTestPolicy()}
	cfg.Policy.RspamdEnabled = false
	g := newScanGate()

	for i := 0; i < scanBreakerThreshold; i++ {
		_, _, action := applyScanGate(g, context.Background(), []byte("x"), cfg, 0, "", "", nil)
		if reason := tempfailReason(t, action); !strings.Contains(reason, "clamd unavailable") {
			t.Fatalf("attempt %d reason = %q, want 'clamd unavailable' (a real dial)", i, reason)
		}
	}
	// scanBreakerThreshold consecutive failures ⇒ breaker open ⇒ the next call
	// fast-fails WITHOUT a dial, with a distinct reason.
	_, _, action := applyScanGate(g, context.Background(), []byte("x"), cfg, 0, "", "", nil)
	if reason := tempfailReason(t, action); !strings.Contains(reason, "circuit breaker open") {
		t.Fatalf("post-threshold reason = %q, want 'circuit breaker open'", reason)
	}
}

func TestScanGate_CircuitBreakerRecoversAfterCooldown(t *testing.T) {
	now := time.Unix(1_700_000_000, 0)
	clk := func() time.Time { return now }
	g := &scanGate{
		inflight: make(chan struct{}, maxInFlightScans),
		clamd:    &scanBreaker{clock: clk},
		rspamd:   &scanBreaker{clock: clk},
	}
	// Drive the clamd breaker open with refused dials.
	ln, _ := net.Listen("tcp", "127.0.0.1:0")
	downAddr := ln.Addr().String()
	_ = ln.Close()
	downCfg := scan.Config{ClamdAddr: downAddr, Policy: defaultTestPolicy()}
	downCfg.Policy.RspamdEnabled = false
	for i := 0; i < scanBreakerThreshold; i++ {
		applyScanGate(g, context.Background(), []byte("x"), downCfg, 0, "", "", nil)
	}
	// Open now ⇒ fast-fail without dialing.
	_, _, action := applyScanGate(g, context.Background(), []byte("x"), downCfg, 0, "", "", nil)
	if reason := tempfailReason(t, action); !strings.Contains(reason, "circuit breaker open") {
		t.Fatalf("reason = %q, want breaker open before cooldown", reason)
	}
	// Advance past the cooldown; a now-working clamd's half-open trial succeeds
	// and closes the breaker (records reset).
	now = now.Add(scanBreakerCooldown + time.Second)
	upAddr, stop := fakeClamd(t, "stream: OK\x00")
	defer stop()
	upCfg := scan.Config{ClamdAddr: upAddr, Policy: defaultTestPolicy()}
	upCfg.Policy.RspamdEnabled = false
	clamav, _, action := applyScanGate(g, context.Background(), []byte("x"), upCfg, 0, "", "", nil)
	if _, ok := clamav.(mailfauna.ClamavVerdictClean); !ok {
		t.Fatalf("clamav = %T, want Clean after recovery", clamav)
	}
	if _, ok := action.(mailfauna.ScanActionDeliver); !ok {
		t.Fatalf("action = %T, want Deliver after recovery", action)
	}
}

func TestScanGate_InFlightCapTempfails(t *testing.T) {
	// A working clamd that must NEVER be dialed (the cap rejects first).
	clamdAddr, stop := fakeClamd(t, "stream: OK\x00")
	defer stop()
	cfg := scan.Config{ClamdAddr: clamdAddr, Policy: defaultTestPolicy()}
	cfg.Policy.RspamdEnabled = false
	// A gate whose single in-flight slot is already taken.
	g := &scanGate{
		inflight: make(chan struct{}, 1),
		clamd:    &scanBreaker{},
		rspamd:   &scanBreaker{},
	}
	g.inflight <- struct{}{} // occupy the only slot

	_, _, action := applyScanGate(g, context.Background(), []byte("x"), cfg, 0, "", "", nil)
	if reason := tempfailReason(t, action); !strings.Contains(reason, "concurrency limit") {
		t.Fatalf("reason = %q, want 'concurrency limit' (fail-closed at capacity)", reason)
	}
}

func TestScanHeaders(t *testing.T) {
	tests := []struct {
		name          string
		clamav        mailfauna.ClamavVerdict
		score         *mailfauna.RspamdScore
		clamavEnabled bool
		want          []string
	}{
		{
			name:          "clean with rspamd",
			clamav:        mailfauna.ClamavVerdictClean{},
			score:         &mailfauna.RspamdScore{ScaledMilli: 1200, FlaggedRules: []string{"BAYES_HAM", "MIME_GOOD"}},
			clamavEnabled: true,
			want: []string{
				"X-Fauna-Scan-Clamav: clean",
				"X-Fauna-Scan-Rspamd-Score: 1.2",
				"X-Fauna-Scan-Rspamd-Rules: BAYES_HAM,MIME_GOOD",
			},
		},
		{
			name:          "infected, no rspamd",
			clamav:        mailfauna.ClamavVerdictInfected{Signature: "X"},
			score:         nil,
			clamavEnabled: true,
			want:          []string{"X-Fauna-Scan-Clamav: infected"},
		},
		{
			name:          "oversize",
			clamav:        mailfauna.ClamavVerdictBypassedOversize{},
			score:         nil,
			clamavEnabled: true,
			want:          []string{"X-Fauna-Scan-Clamav: bypassed_oversize"},
		},
		{
			name:          "clamav disabled suppresses its header",
			clamav:        mailfauna.ClamavVerdictNotScanned{},
			score:         &mailfauna.RspamdScore{ScaledMilli: 0, FlaggedRules: nil},
			clamavEnabled: false,
			want: []string{
				"X-Fauna-Scan-Rspamd-Score: 0",
				"X-Fauna-Scan-Rspamd-Rules: ",
			},
		},
		{
			// A verdict nobody computed stamps nothing, whatever the policy
			// flag says — there is no value for a filter rule to match.
			name:          "not scanned stamps no clamav header even when enabled",
			clamav:        mailfauna.ClamavVerdictNotScanned{},
			score:         nil,
			clamavEnabled: true,
			want:          nil,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := scanHeaders(tc.clamav, tc.score, tc.clamavEnabled)
			if strings.Join(got, "\n") != strings.Join(tc.want, "\n") {
				t.Errorf("scanHeaders = %q, want %q", got, tc.want)
			}
		})
	}
}

func TestPrependHeaders(t *testing.T) {
	raw := []byte("Subject: hi\r\n\r\nbody")
	got := prependHeaders(raw, []string{"X-Fauna-Scan-Clamav: clean"})
	want := "X-Fauna-Scan-Clamav: clean\r\nSubject: hi\r\n\r\nbody"
	if string(got) != want {
		t.Errorf("prependHeaders = %q, want %q", got, want)
	}
	// No headers ⇒ unchanged.
	if string(prependHeaders(raw, nil)) != string(raw) {
		t.Error("prependHeaders with no headers should be a no-op")
	}
}
