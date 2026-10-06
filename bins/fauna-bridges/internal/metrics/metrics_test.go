package metrics

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// NOTE: metrics tests do NOT call t.Parallel(). All tests share the
// package-level registry; concurrent tests touching the same counters
// would race on observed values. Sequential is fine — the suite is tiny.

// TestHandlerServes200 asserts the /metrics endpoint returns 200 and
// includes a known counter's name once it has at least one observed
// label tuple. Prometheus CounterVec / HistogramVec collectors omit
// HELP / TYPE lines until a label combination is observed (this is a
// client_golang invariant, not a bug to work around), so we increment
// once before scraping. Tests downstream of B.4 that exercise real call
// sites will already have non-empty series.
func TestHandlerServes200(t *testing.T) {
	WSRPCCallsTotal.WithLabelValues("Whoami", "ok").Inc()

	req := httptest.NewRequest(http.MethodGet, "/metrics", nil)
	w := httptest.NewRecorder()
	Handler().ServeHTTP(w, req)
	if w.Code != http.StatusOK {
		t.Fatalf("status = %d, want %d", w.Code, http.StatusOK)
	}
	body := w.Body.String()
	if !strings.Contains(body, "wsrpc_calls_total") {
		t.Errorf("/metrics body missing wsrpc_calls_total; got:\n%s", body)
	}
}

// TestCountersRegistered increments each counter once and asserts the
// exposed /metrics body names every counter. This catches the
// pathological case where a counter is declared but not actually
// registered against the package registry.
func TestCountersRegistered(t *testing.T) {
	WSRPCCallsTotal.WithLabelValues("Whoami", "ok").Inc()
	SMTPConnectionsTotal.WithLabelValues("25", "accepted").Inc()
	SMTPInboundMessagesTotal.WithLabelValues("accepted").Inc()
	CgoCallsTotal.WithLabelValues("fauna_mail_open_wrapped_blob", "ok").Inc()
	SMTPSenderDomainFailOpen.WithLabelValues("resolver_error").Inc()
	SMTPGreylistCheckFailOpen.Inc()
	BridgeRole.WithLabelValues("mda").Set(1)
	BridgeRole.WithLabelValues("mta").Set(0)

	body := scrape(t)
	for _, name := range []string{
		"wsrpc_calls_total",
		"smtp_connections_total",
		"smtp_inbound_messages_total",
		"cgo_calls_total",
		"smtp_inbound_sender_domain_fail_open_total",
		"smtp_inbound_greylist_check_fail_open_total",
		"bridge_role",
	} {
		if !strings.Contains(body, name) {
			t.Errorf("scrape missing counter %q; got:\n%s", name, body)
		}
	}
}

// TestBridgeRoleGauge pins the "1 for the role this process took,
// 0 for the other" shape main.go relies on after Whoami. Two label
// values for one Set: a downstream sum-by-role gives the fleet count
// of MTAs vs. MDAs cleanly.
func TestBridgeRoleGauge(t *testing.T) {
	BridgeRole.WithLabelValues("mda").Set(1)
	BridgeRole.WithLabelValues("mta").Set(0)
	body := scrape(t)
	if !strings.Contains(body, `bridge_role{role="mda"} 1`) {
		t.Errorf("scrape missing bridge_role{role=\"mda\"} 1; got:\n%s", body)
	}
	if !strings.Contains(body, `bridge_role{role="mta"} 0`) {
		t.Errorf("scrape missing bridge_role{role=\"mta\"} 0; got:\n%s", body)
	}
}

// TestHistogramBuckets observes one sample on WSRPCCallSeconds and
// asserts the bucket-boundary lines we tuned for low-ms RTT are present
// in the exposed output. The bucket values come straight from
// prometheus.ExponentialBuckets(0.001, 2, 12).
func TestHistogramBuckets(t *testing.T) {
	WSRPCCallSeconds.WithLabelValues("Whoami").Observe(0.05)
	body := scrape(t)
	if !strings.Contains(body, "wsrpc_call_seconds_bucket") {
		t.Fatalf("scrape missing wsrpc_call_seconds_bucket lines; got:\n%s", body)
	}
	// Spot-check a few of the boundaries. ExponentialBuckets(0.001, 2, 12)
	// emits 0.001, 0.002, 0.004, 0.008, 0.016, 0.032, 0.064, 0.128, 0.256,
	// 0.512, 1.024, 2.048; Prometheus serializes them as `le="..."` strings.
	for _, le := range []string{`le="0.001"`, `le="0.064"`, `le="2.048"`, `le="+Inf"`} {
		if !strings.Contains(body, le) {
			t.Errorf("scrape missing bucket boundary %s; got:\n%s", le, body)
		}
	}
}

// TestRegistryReturnsPackageRegistry pins that Registry() returns the
// same *prometheus.Registry the package uses internally — call sites
// (and tests outside this file) need a way to get at it.
func TestRegistryReturnsPackageRegistry(t *testing.T) {
	if Registry() == nil {
		t.Fatal("Registry() returned nil")
	}
	if Registry() != registry {
		t.Fatal("Registry() returned a different *prometheus.Registry than the package-level one")
	}
}

// scrape invokes the /metrics handler and returns the body. Test helper.
func scrape(t *testing.T) string {
	t.Helper()
	req := httptest.NewRequest(http.MethodGet, "/metrics", nil)
	w := httptest.NewRecorder()
	Handler().ServeHTTP(w, req)
	if w.Code != http.StatusOK {
		t.Fatalf("scrape status = %d, want %d", w.Code, http.StatusOK)
	}
	return w.Body.String()
}
