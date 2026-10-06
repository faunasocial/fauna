package mta

import (
	"context"
	"crypto/x509"
	"errors"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// ── Pure classifier coverage (the genuinely Go-side TLSRPT bits) ──────

// TestClassifyTLSHandshakeError: a failed STARTTLS handshake maps to the
// right RFC 8460 §4.3 token per the TLS posture (smtp-server.md:474).
func TestClassifyTLSHandshakeError(t *testing.T) {
	generic := errors.New("handshake failed: connection reset by peer")
	hostMismatch := x509.HostnameError{Host: "mx.dest.test"}
	cases := []struct {
		name string
		err  error
		mode TLSMode
		want string
	}{
		{"dane any failure is tlsa-invalid", generic, TLSDanePinned, "tlsa-invalid"},
		{"dane outranks hostname classification", hostMismatch, TLSDanePinned, "tlsa-invalid"},
		{"enforce hostname mismatch", hostMismatch, TLSRequired, "certificate-host-mismatch"},
		{"enforce other webpki failure", generic, TLSRequired, "sts-webpki-invalid"},
		{"opportunistic protocol failure", generic, TLSOpportunistic, "validation-failure"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := classifyTLSHandshakeError(tc.err, tc.mode); got != tc.want {
				t.Fatalf("classifyTLSHandshakeError(%v, %v) = %q want %q", tc.err, tc.mode, got, tc.want)
			}
		})
	}
}

// TestPreTLSStsSignal: the pre-TLS STS signal (recorded independent of the
// handshake) maps the retained 4-way outcome + the testing-mismatch flag to
// its RFC 8460 §4.3 token, or "" for no signal.
func TestPreTLSStsSignal(t *testing.T) {
	cases := []struct {
		name            string
		outcome         wsrpc.MtaStsOutcome
		testingMismatch bool
		want            string
	}{
		{"fetch_error", wsrpc.MtaStsOutcomeFetchError, false, "sts-policy-fetch-error"},
		{"invalid", wsrpc.MtaStsOutcomeInvalid, false, "sts-policy-invalid"},
		{"testing mismatch", wsrpc.MtaStsOutcomeFound, true, "sts-policy-mismatch"},
		{"found and matches has no signal", wsrpc.MtaStsOutcomeFound, false, ""},
		{"not_published has no signal", wsrpc.MtaStsOutcomeNotPublished, false, ""},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := preTLSStsSignal(tc.outcome, tc.testingMismatch); got != tc.want {
				t.Fatalf("preTLSStsSignal(%q, %v) = %q want %q", tc.outcome, tc.testingMismatch, got, tc.want)
			}
		})
	}
}

// ── deliverOne → report_tls_attempt wiring (the G1 production flow) ───

func assertReport(t *testing.T, got, want tlsReportCall) {
	t.Helper()
	if got != want {
		t.Fatalf("report = %+v want %+v", got, want)
	}
}

// runStsReports drives one outbound unit through deliverOne against q +
// twoHostMX, waits for `until`, then returns the recorded TLSRPT reports.
func runStsReports(t *testing.T, q *mtaStsCaller, until func(*mtaStsCaller) bool) []tlsReportCall {
	t.Helper()
	w := newStsWorker(t, q, twoHostMX(), &stsSender{})
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	wg := w.Start(ctx)
	waitUntil(t, func() bool {
		q.mu.Lock()
		defer q.mu.Unlock()
		return until(q)
	}, "terminal")
	cancel()
	wg.Wait()
	return append([]tlsReportCall(nil), q.tlsReports...)
}

// TestTlsrptReportEnforceMismatchThenSuccess: an enforce policy matching only
// mx-b → mx-a is refused with a pre-TLS sts-policy-mismatch report (no DANE
// strings, since a refused host runs no TLSA lookup), then mx-b delivers and
// reports a successful TLS session (result_type=None). smtp-server.md:474.
func TestTlsrptReportEnforceMismatchThenSuccess(t *testing.T) {
	q := &mtaStsCaller{
		queueCaller: queueCaller{batches: [][]wsrpc.OutboundUnit{{twoHostUnit()}, nil}},
		stsOutcome:  "found",
		stsPolicy:   foundPolicy("enforce", "mx-b.dest.test"),
	}
	reports := runStsReports(t, q, func(q *mtaStsCaller) bool { return len(q.delivered) == 1 })
	if len(reports) != 2 {
		t.Fatalf("reports = %+v want 2 (mx-a enforce-mismatch + mx-b success)", reports)
	}
	assertReport(t, reports[0], tlsReportCall{
		RecipientDomain: "dest.test", MxHost: "mx-a.dest.test",
		ResultType: "sts-policy-mismatch", MtaStsOutcome: "found", HasPolicy: true, TlsaRecordCount: 0,
	})
	assertReport(t, reports[1], tlsReportCall{
		RecipientDomain: "dest.test", MxHost: "mx-b.dest.test",
		ResultType: "", MtaStsOutcome: "found", HasPolicy: true, TlsaRecordCount: 0,
	})
}

// TestTlsrptReportEnforceAllMismatch: enforce policy matching no host → both
// hosts report sts-policy-mismatch and the unit is reported failed; no
// success report (the sender is never called). smtp-server.md:452,474.
func TestTlsrptReportEnforceAllMismatch(t *testing.T) {
	q := &mtaStsCaller{
		queueCaller: queueCaller{batches: [][]wsrpc.OutboundUnit{{twoHostUnit()}, nil}},
		stsOutcome:  "found",
		stsPolicy:   foundPolicy("enforce", "other.test"),
	}
	reports := runStsReports(t, q, func(q *mtaStsCaller) bool { return len(q.failed) == 1 })
	if len(reports) != 2 {
		t.Fatalf("reports = %+v want 2 (both hosts enforce-mismatch)", reports)
	}
	for _, r := range reports {
		if r.ResultType != "sts-policy-mismatch" {
			t.Fatalf("report %+v want ResultType sts-policy-mismatch", r)
		}
	}
}

// TestTlsrptReportTestingMismatchThenSuccess: testing-mode mismatch proceeds
// opportunistically → mx-a reports a pre-TLS sts-policy-mismatch then a
// successful TLS session. smtp-server.md:453,474.
func TestTlsrptReportTestingMismatchThenSuccess(t *testing.T) {
	q := &mtaStsCaller{
		queueCaller: queueCaller{batches: [][]wsrpc.OutboundUnit{{twoHostUnit()}, nil}},
		stsOutcome:  "found",
		stsPolicy:   foundPolicy("testing", "other.test"),
	}
	reports := runStsReports(t, q, func(q *mtaStsCaller) bool { return len(q.delivered) == 1 })
	if len(reports) != 2 {
		t.Fatalf("reports = %+v want 2 (mismatch signal + success at mx-a)", reports)
	}
	assertReport(t, reports[0], tlsReportCall{
		RecipientDomain: "dest.test", MxHost: "mx-a.dest.test",
		ResultType: "sts-policy-mismatch", MtaStsOutcome: "found", HasPolicy: true, TlsaRecordCount: 0,
	})
	assertReport(t, reports[1], tlsReportCall{
		RecipientDomain: "dest.test", MxHost: "mx-a.dest.test",
		ResultType: "", MtaStsOutcome: "found", HasPolicy: true, TlsaRecordCount: 0,
	})
}

// TestTlsrptReportNoPolicySuccessIsNone: not_published → a single successful
// TLS-session report (result_type=None), no pre-TLS signal.
func TestTlsrptReportNoPolicySuccessIsNone(t *testing.T) {
	q := &mtaStsCaller{
		queueCaller: queueCaller{batches: [][]wsrpc.OutboundUnit{{twoHostUnit()}, nil}},
		stsOutcome:  "not_published",
	}
	reports := runStsReports(t, q, func(q *mtaStsCaller) bool { return len(q.delivered) == 1 })
	if len(reports) != 1 {
		t.Fatalf("reports = %+v want 1 (single success report)", reports)
	}
	assertReport(t, reports[0], tlsReportCall{
		RecipientDomain: "dest.test", MxHost: "mx-a.dest.test",
		ResultType: "", MtaStsOutcome: "not_published", HasPolicy: false, TlsaRecordCount: 0,
	})
}

// TestTlsrptReportFetchErrorSignalThenSuccess: a published-but-broken STS
// policy (fetch_error) is no-policy-for-delivery (RFC 8461 §5) but still
// reports the sts-policy-fetch-error signal, then the opportunistic delivery
// reports its successful TLS session. smtp-server.md:474.
func TestTlsrptReportFetchErrorSignalThenSuccess(t *testing.T) {
	q := &mtaStsCaller{
		queueCaller: queueCaller{batches: [][]wsrpc.OutboundUnit{{twoHostUnit()}, nil}},
		stsOutcome:  "fetch_error",
	}
	reports := runStsReports(t, q, func(q *mtaStsCaller) bool { return len(q.delivered) == 1 })
	if len(reports) != 2 {
		t.Fatalf("reports = %+v want 2 (fetch-error signal + success)", reports)
	}
	assertReport(t, reports[0], tlsReportCall{
		RecipientDomain: "dest.test", MxHost: "mx-a.dest.test",
		ResultType: "sts-policy-fetch-error", MtaStsOutcome: "fetch_error", HasPolicy: false, TlsaRecordCount: 0,
	})
	assertReport(t, reports[1], tlsReportCall{
		RecipientDomain: "dest.test", MxHost: "mx-a.dest.test",
		ResultType: "", MtaStsOutcome: "fetch_error", HasPolicy: false, TlsaRecordCount: 0,
	})
}

// TestTlsrptReportDaneSuccessCarriesRecords: a DANE-pinned attempt reports a
// successful TLS session and carries the TLSA record count so nest buckets it
// under the `tlsa` policy type (DANE > MTA-STS). smtp-server.md:449,474.
func TestTlsrptReportDaneSuccessCarriesRecords(t *testing.T) {
	c := &daneCaller{
		mtaStsCaller: mtaStsCaller{
			queueCaller: queueCaller{batches: [][]wsrpc.OutboundUnit{{twoHostUnit()}, nil}},
			stsOutcome:  "not_published",
		},
		tlsaRecords: []wsrpc.TlsaRecordWire{daneEERecord()},
	}
	w := newDaneWorker(t, c, twoHostMX(), &stsSender{})
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	wg := w.Start(ctx)
	waitUntil(t, func() bool {
		c.mu.Lock()
		defer c.mu.Unlock()
		return len(c.delivered) == 1
	}, "delivered")
	cancel()
	wg.Wait()
	if len(c.tlsReports) != 1 {
		t.Fatalf("reports = %+v want 1 DANE success report", c.tlsReports)
	}
	assertReport(t, c.tlsReports[0], tlsReportCall{
		RecipientDomain: "dest.test", MxHost: "mx-a.dest.test",
		ResultType: "", MtaStsOutcome: "not_published", HasPolicy: false, TlsaRecordCount: 1,
	})
}
