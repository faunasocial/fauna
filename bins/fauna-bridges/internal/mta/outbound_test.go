package mta

import (
	"context"
	"errors"
	"io"
	"log/slog"
	"net"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// ── In-memory queue caller — scriptable wsrpc.Caller ─────────────

// queueCaller is a fake wsrpc.Caller that simulates nest's outbound
// queue. It hands out scripted units on fetch_outbound_due and
// records the mark_outbound_* calls so tests can assert the
// reporting path.
type queueCaller struct {
	mu sync.Mutex
	// scripted batches; pop one per FetchOutboundDue call.
	batches [][]wsrpc.OutboundUnit

	delivered  []int64
	failed     []failedCall
	bounced    []bouncedCall
	tlsReports []tlsReportCall

	// fetchCalls counts polls so tests can observe the worker's
	// trigger behaviour.
	fetchCalls atomic.Uint32
}

// tlsReportCall captures one fauna.bridges.report_tls_attempt the worker
// emitted, so the MTA-STS / DANE tests can assert the TLSRPT signal(s) per
// attempt. ResultType is "" when the report carried a nil result_type (a
// successful TLS session → RFC 8460 result_type=None).
type tlsReportCall struct {
	RecipientDomain string
	MxHost          string
	ResultType      string
	MtaStsOutcome   string
	HasPolicy       bool
	TlsaRecordCount int
}

type failedCall struct {
	ID                int64
	RetryAfterSeconds uint32
	LastError         string
}

type bouncedCall struct {
	ID     int64
	Reason string
}

func (q *queueCaller) Call(_ context.Context, method string, body, reply any) error {
	q.mu.Lock()
	defer q.mu.Unlock()
	bodyBytes, err := dagcbor.Marshal(body)
	if err != nil {
		return err
	}
	switch method {
	case wsrpc.MethodFetchMtaStsPolicy:
		// Default: no STS policy published → no enforcement. The MTA-STS
		// table tests (outbound_mta_sts_test.go) override Call to script
		// the other outcomes; existing outbound tests get this no-op.
		out := struct {
			Outcome string                  `cbor:"outcome"`
			Policy  *wsrpc.MtaStsPolicyWire `cbor:"policy"`
		}{Outcome: "not_published", Policy: nil}
		repBytes, _ := dagcbor.Marshal(out)
		return cbor.Unmarshal(repBytes, reply)
	case wsrpc.MethodFetchTlsa:
		// Default: no DANE/TLSA records → no pinning. The DANE table tests
		// (outbound_dane_test.go) override Call to script records / errors;
		// existing outbound tests get this empty no-op so deliverOne falls
		// through to the MTA-STS / opportunistic posture.
		out := struct {
			Records []wsrpc.TlsaRecordWire `cbor:"records"`
		}{Records: nil}
		repBytes, _ := dagcbor.Marshal(out)
		return cbor.Unmarshal(repBytes, reply)
	case wsrpc.MethodFetchOutboundDue:
		q.fetchCalls.Add(1)
		var batch []wsrpc.OutboundUnit
		if len(q.batches) > 0 {
			batch = q.batches[0]
			q.batches = q.batches[1:]
		}
		out := struct {
			Units []wsrpc.OutboundUnit `cbor:"units"`
		}{Units: batch}
		repBytes, _ := dagcbor.Marshal(out)
		return cbor.Unmarshal(repBytes, reply)
	case wsrpc.MethodMarkOutboundDelivered:
		var req struct {
			ID int64 `cbor:"id"`
		}
		if err := cbor.Unmarshal(bodyBytes, &req); err != nil {
			return err
		}
		q.delivered = append(q.delivered, req.ID)
		repBytes, _ := dagcbor.Marshal(struct {
			OK bool `cbor:"ok"`
		}{OK: true})
		return cbor.Unmarshal(repBytes, reply)
	case wsrpc.MethodMarkOutboundFailed:
		var req struct {
			ID                int64  `cbor:"id"`
			RetryAfterSeconds uint32 `cbor:"retry_after_seconds"`
			LastError         string `cbor:"last_error"`
		}
		if err := cbor.Unmarshal(bodyBytes, &req); err != nil {
			return err
		}
		q.failed = append(q.failed, failedCall{ID: req.ID, RetryAfterSeconds: req.RetryAfterSeconds, LastError: req.LastError})
		repBytes, _ := dagcbor.Marshal(struct {
			OK bool `cbor:"ok"`
		}{OK: true})
		return cbor.Unmarshal(repBytes, reply)
	case wsrpc.MethodMarkOutboundBounced:
		var req struct {
			ID     int64  `cbor:"id"`
			Reason string `cbor:"reason"`
		}
		if err := cbor.Unmarshal(bodyBytes, &req); err != nil {
			return err
		}
		q.bounced = append(q.bounced, bouncedCall{ID: req.ID, Reason: req.Reason})
		repBytes, _ := dagcbor.Marshal(struct {
			OK bool `cbor:"ok"`
		}{OK: true})
		return cbor.Unmarshal(repBytes, reply)
	case wsrpc.MethodReportTlsAttempt:
		var req struct {
			RecipientDomain string                  `cbor:"recipient_domain"`
			MxHost          string                  `cbor:"mx_host"`
			ResultType      *string                 `cbor:"result_type"`
			MtaStsOutcome   string                  `cbor:"mta_sts_outcome"`
			MtaStsPolicy    *wsrpc.MtaStsPolicyWire `cbor:"mta_sts_policy"`
			TlsaRecords     []wsrpc.TlsaRecordWire  `cbor:"tlsa_records"`
		}
		if err := cbor.Unmarshal(bodyBytes, &req); err != nil {
			return err
		}
		rt := ""
		if req.ResultType != nil {
			rt = *req.ResultType
		}
		q.tlsReports = append(q.tlsReports, tlsReportCall{
			RecipientDomain: req.RecipientDomain,
			MxHost:          req.MxHost,
			ResultType:      rt,
			MtaStsOutcome:   req.MtaStsOutcome,
			HasPolicy:       req.MtaStsPolicy != nil,
			TlsaRecordCount: len(req.TlsaRecords),
		})
		repBytes, _ := dagcbor.Marshal(struct {
			OK bool `cbor:"ok"`
		}{OK: true})
		return cbor.Unmarshal(repBytes, reply)
	default:
		return errors.New("queueCaller: unexpected method " + method)
	}
}

// ── Fake MX resolver ──────────────────────────────────────────────

type fakeMX struct {
	hosts map[string][]MXHost
	err   error
	// insecure scripts an MX RRset the resolver could NOT DNSSEC-validate.
	// The field is negative so the zero value stays the ordinary
	// secure-RRset case every pre-existing fixture already means; the
	// DNSSEC-provenance tests set it explicitly.
	insecure bool
}

func (f fakeMX) LookupMX(_ context.Context, domain string) (MXAnswer, error) {
	if f.err != nil {
		return MXAnswer{}, f.err
	}
	hosts, ok := f.hosts[domain]
	if !ok {
		return MXAnswer{}, &net.DNSError{Err: "no such host", Name: domain, IsNotFound: true}
	}
	return MXAnswer{Hosts: hosts, Secure: !f.insecure}, nil
}

// ── Fake SMTP sender ──────────────────────────────────────────────

type fakeSMTP struct {
	mu      sync.Mutex
	sent    []sentRecord
	respond func(host, from, recipient string, body []byte) error
}

type sentRecord struct {
	Host      string
	From      string
	Recipient string
	Body      []byte
	TLSPolicy TLSPolicy
}

func (f *fakeSMTP) Send(_ context.Context, host string, from string, recipient string, body []byte, tlsPolicy TLSPolicy) (TLSAttemptOutcome, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.respond != nil {
		if err := f.respond(host, from, recipient, body); err != nil {
			// A scripted SMTP-level failure simulates a post-TLS protocol
			// response (4xx/5xx), which carries no TLS verdict.
			return TLSAttemptOutcome{}, err
		}
	}
	f.sent = append(f.sent, sentRecord{Host: host, From: from, Recipient: recipient, Body: append([]byte(nil), body...), TLSPolicy: tlsPolicy})
	// A delivered envelope implies a successful TLS session (the fake never
	// downgrades to plaintext) → reported as result_type=None.
	return TLSAttemptOutcome{Reportable: true}, nil
}

// ── Test helpers ──────────────────────────────────────────────────

func discardLogger() *slog.Logger {
	return slog.New(slog.NewTextHandler(io.Discard, &slog.HandlerOptions{Level: slog.LevelInfo}))
}

func newTestWorker(t *testing.T, q *queueCaller, mx MXResolver, sender SMTPSender) *OutboundWorker {
	t.Helper()
	cfg := OutboundWorkerConfig{
		PollInterval:   10 * time.Millisecond,
		BatchSize:      4,
		LeaseSeconds:   60,
		AttemptTimeout: time.Second,
	}
	w, err := NewOutboundWorker(q, mx, sender, "mta.example.com", cfg, discardLogger())
	if err != nil {
		t.Fatalf("NewOutboundWorker: %v", err)
	}
	return w
}

func waitUntil(t *testing.T, cond func() bool, msg string) {
	t.Helper()
	deadline := time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		if cond() {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("waitUntil timeout: %s", msg)
}

// ── Round-trip integration tests ──────────────────────────────────

func TestOutboundWorkerDeliversAndMarksDelivered(t *testing.T) {
	q := &queueCaller{
		batches: [][]wsrpc.OutboundUnit{
			{
				{
					ID:             7,
					MessageID:      "msg-1@example.com",
					OriginalSender: "alice@example.com",
					Recipient:      "bob@dest.test",
					RawMessage:     []byte("From: alice\r\nTo: bob\r\n\r\nhi\r\n"),
					AttemptCount:   0,
				},
			},
			nil,
		},
	}
	mx := fakeMX{hosts: map[string][]MXHost{
		"dest.test": {{Hostname: "mx1.dest.test", Pref: 10}},
	}}
	sender := &fakeSMTP{}
	w := newTestWorker(t, q, mx, sender)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	wg := w.Start(ctx)

	waitUntil(t, func() bool {
		q.mu.Lock()
		defer q.mu.Unlock()
		return len(q.delivered) == 1
	}, "delivered count")
	cancel()
	wg.Wait()

	if len(q.delivered) != 1 || q.delivered[0] != 7 {
		t.Fatalf("delivered=%v want [7]", q.delivered)
	}
	if len(q.failed) != 0 || len(q.bounced) != 0 {
		t.Fatalf("unexpected failed/bounced: %+v %+v", q.failed, q.bounced)
	}
	sender.mu.Lock()
	defer sender.mu.Unlock()
	if len(sender.sent) != 1 {
		t.Fatalf("sent=%d want 1", len(sender.sent))
	}
	got := sender.sent[0]
	if got.Host != "mx1.dest.test" || got.From != "alice@example.com" || got.Recipient != "bob@dest.test" {
		t.Fatalf("sent=%+v", got)
	}
	if !strings.Contains(string(got.Body), "hi") {
		t.Fatalf("body=%q", got.Body)
	}
}

// The worker signs nothing: the nest signs each message as it hands it out, so
// the bytes that reach the remote MX are exactly the unit's RawMessage — an
// existing DKIM-Signature is neither touched nor joined by a second one.
func TestOutboundWorkerRelaysBodyVerbatim(t *testing.T) {
	raw := "DKIM-Signature: v=1; a=ed25519-sha256; d=list.test; stub\r\nFrom: news@list.test\r\nList-Unsubscribe: <https://list.test/u?t=tok>\r\n\r\nhi\r\n"
	q := &queueCaller{
		batches: [][]wsrpc.OutboundUnit{
			{
				{
					ID:             11,
					MessageID:      "issue-1@list.test",
					OriginalSender: "news@list.test",
					Recipient:      "sub@dest.test",
					RawMessage:     []byte(raw),
				},
			},
			nil,
		},
	}
	mx := fakeMX{hosts: map[string][]MXHost{
		"dest.test": {{Hostname: "mx1.dest.test", Pref: 10}},
	}}
	sender := &fakeSMTP{}
	w := newTestWorker(t, q, mx, sender)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	wg := w.Start(ctx)
	waitUntil(t, func() bool {
		q.mu.Lock()
		defer q.mu.Unlock()
		return len(q.delivered) == 1
	}, "delivered count")
	cancel()
	wg.Wait()

	sender.mu.Lock()
	defer sender.mu.Unlock()
	if len(sender.sent) != 1 {
		t.Fatalf("sent=%d want 1", len(sender.sent))
	}
	if body := string(sender.sent[0].Body); body != raw {
		t.Fatalf("worker must relay the body exactly as handed out:\n got %q\nwant %q", body, raw)
	}
}

func TestOutboundWorkerMarksFailedOnTemporaryError(t *testing.T) {
	q := &queueCaller{
		batches: [][]wsrpc.OutboundUnit{
			{
				{
					ID:             8,
					MessageID:      "msg-2@example.com",
					OriginalSender: "alice@example.com",
					Recipient:      "bob@dest.test",
					RawMessage:     []byte("body\r\n"),
					AttemptCount:   0,
				},
			},
			nil,
		},
	}
	mx := fakeMX{hosts: map[string][]MXHost{
		"dest.test": {{Hostname: "mx1.dest.test", Pref: 10}},
	}}
	sender := &fakeSMTP{respond: func(_, _, _ string, _ []byte) error {
		return NewTemporaryError("421 4.7.0 try again later")
	}}
	w := newTestWorker(t, q, mx, sender)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	wg := w.Start(ctx)

	waitUntil(t, func() bool {
		q.mu.Lock()
		defer q.mu.Unlock()
		return len(q.failed) == 1
	}, "failed count")
	cancel()
	wg.Wait()

	if len(q.failed) != 1 || q.failed[0].ID != 8 {
		t.Fatalf("failed=%+v", q.failed)
	}
	// nest owns the retry curve now; the worker reports no server hint.
	if q.failed[0].RetryAfterSeconds != 0 {
		t.Fatalf("retry_after=%d want 0 (nest owns the curve)", q.failed[0].RetryAfterSeconds)
	}
	if !strings.Contains(q.failed[0].LastError, "421") {
		t.Fatalf("last_error=%q", q.failed[0].LastError)
	}
	if len(q.delivered) != 0 || len(q.bounced) != 0 {
		t.Fatalf("unexpected delivered/bounced")
	}
}

func TestOutboundWorkerMarksBouncedOnPermanentError(t *testing.T) {
	q := &queueCaller{
		batches: [][]wsrpc.OutboundUnit{
			{
				{
					ID:             9,
					MessageID:      "msg-3@example.com",
					OriginalSender: "alice@example.com",
					Recipient:      "ghost@dest.test",
					RawMessage:     []byte("body\r\n"),
					AttemptCount:   0,
				},
			},
			nil,
		},
	}
	mx := fakeMX{hosts: map[string][]MXHost{
		"dest.test": {{Hostname: "mx1.dest.test", Pref: 10}},
	}}
	sender := &fakeSMTP{respond: func(_, _, _ string, _ []byte) error {
		return NewPermanentError("550 5.1.1 mailbox unknown")
	}}
	w := newTestWorker(t, q, mx, sender)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	wg := w.Start(ctx)

	waitUntil(t, func() bool {
		q.mu.Lock()
		defer q.mu.Unlock()
		return len(q.bounced) == 1
	}, "bounced count")
	cancel()
	wg.Wait()

	if len(q.bounced) != 1 || q.bounced[0].ID != 9 {
		t.Fatalf("bounced=%+v", q.bounced)
	}
	if !strings.Contains(q.bounced[0].Reason, "550") {
		t.Fatalf("reason=%q", q.bounced[0].Reason)
	}
}

func TestOutboundWorkerReportsFailedWhenMXLookupTempfails(t *testing.T) {
	q := &queueCaller{
		batches: [][]wsrpc.OutboundUnit{
			{
				{
					ID:             10,
					MessageID:      "msg-4@example.com",
					OriginalSender: "alice@example.com",
					Recipient:      "bob@nowhere.invalid",
					RawMessage:     []byte("body\r\n"),
					AttemptCount:   0,
				},
			},
			nil,
		},
	}
	// fakeMX with no entries → DNSError IsNotFound → no implicit-MX
	// (the fake doesn't recurse to A/AAAA). The worker classifies a
	// resolver error as temporary and reports mark_outbound_failed;
	// nest (not the worker) decides whether to reschedule or bounce.
	mx := fakeMX{hosts: map[string][]MXHost{}}
	sender := &fakeSMTP{}
	w := newTestWorker(t, q, mx, sender)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	wg := w.Start(ctx)

	waitUntil(t, func() bool {
		q.mu.Lock()
		defer q.mu.Unlock()
		return len(q.failed) == 1
	}, "failed count")
	cancel()
	wg.Wait()

	if len(q.failed) != 1 || len(q.bounced) != 0 {
		t.Fatalf("failed=%+v bounced=%+v (a temp MX failure reports failed, never bounces)", q.failed, q.bounced)
	}
}

func TestOutboundWorkerTemporaryFailureAlwaysReportsFailedNotBounced(t *testing.T) {
	// Even a high attempt_count must NOT make the worker bounce on a
	// temporary failure: nest owns the retry budget / give-up decision
	// (smtp-server.md § Outbound delivery). The worker reports every
	// temporary failure as mark_outbound_failed with no server hint.
	q := &queueCaller{
		batches: [][]wsrpc.OutboundUnit{
			{
				{
					ID:             11,
					MessageID:      "msg-5@example.com",
					OriginalSender: "alice@example.com",
					Recipient:      "bob@dest.test",
					RawMessage:     []byte("body\r\n"),
					AttemptCount:   42, // far past any old bridge-side cap
				},
			},
			nil,
		},
	}
	mx := fakeMX{hosts: map[string][]MXHost{
		"dest.test": {{Hostname: "mx1.dest.test", Pref: 10}},
	}}
	sender := &fakeSMTP{respond: func(_, _, _ string, _ []byte) error {
		return NewTemporaryError("450 transient")
	}}
	w := newTestWorker(t, q, mx, sender)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	wg := w.Start(ctx)

	waitUntil(t, func() bool {
		q.mu.Lock()
		defer q.mu.Unlock()
		return len(q.failed) == 1
	}, "failed count")
	cancel()
	wg.Wait()

	if len(q.failed) != 1 || q.failed[0].ID != 11 {
		t.Fatalf("failed=%+v", q.failed)
	}
	if q.failed[0].RetryAfterSeconds != 0 {
		t.Fatalf("retry_after=%d want 0 (nest owns the curve)", q.failed[0].RetryAfterSeconds)
	}
	if len(q.bounced) != 0 {
		t.Fatalf("unexpected bounced=%+v (worker must not give up on a temporary failure)", q.bounced)
	}
}

func TestOutboundWorkerTriggerSkipsPollSleep(t *testing.T) {
	// Empty queue first; verify the worker is idle (one fetch, then
	// sleeping on PollInterval). Trigger should immediately wake it
	// and the second fetch lands the scripted unit.
	q := &queueCaller{
		batches: [][]wsrpc.OutboundUnit{
			nil, // empty first poll
			{
				{
					ID:             12,
					MessageID:      "msg-6@example.com",
					OriginalSender: "alice@example.com",
					Recipient:      "bob@dest.test",
					RawMessage:     []byte("body\r\n"),
					AttemptCount:   0,
				},
			},
		},
	}
	mx := fakeMX{hosts: map[string][]MXHost{
		"dest.test": {{Hostname: "mx1.dest.test", Pref: 10}},
	}}
	sender := &fakeSMTP{}
	cfg := OutboundWorkerConfig{
		// Long PollInterval to make the Trigger->wake path observable.
		PollInterval:   2 * time.Second,
		BatchSize:      4,
		LeaseSeconds:   60,
		AttemptTimeout: time.Second,
	}
	w, err := NewOutboundWorker(q, mx, sender, "mta.example.com", cfg, discardLogger())
	if err != nil {
		t.Fatalf("NewOutboundWorker: %v", err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	wg := w.Start(ctx)

	// Wait until the worker has polled the empty batch and is asleep.
	waitUntil(t, func() bool { return q.fetchCalls.Load() >= 1 }, "first fetch")
	w.Trigger()
	waitUntil(t, func() bool {
		q.mu.Lock()
		defer q.mu.Unlock()
		return len(q.delivered) == 1
	}, "delivered after trigger")
	cancel()
	wg.Wait()
}

// ── LiveMXResolver shape tests ────────────────────────────────────

func TestDomainOfExtractsTail(t *testing.T) {
	cases := map[string]string{
		"a@b.c":     "b.c",
		"a@b":       "b",
		"a@b@c.d":   "c.d", // last @ wins
		"no-at":     "",
		"trailing@": "",
	}
	for input, want := range cases {
		if got := domainOf(input); got != want {
			t.Errorf("domainOf(%q): got %q want %q", input, got, want)
		}
	}
}

// The retry-schedule curve moved nest-side (smtp-server.md § Outbound
// delivery: "nest reschedules per the retry schedule below"); it is now
// exercised in Rust by `fauna-nest`'s `outbound_retry` unit tests
// (`policy_conversion_drops_attempt1_delay_and_scales_hours`,
// `first_failure_schedules_about_five_minutes_no_warn`, …). The worker no
// longer computes a backoff, so there is no `backoffSeconds` to test here.

// Real-loopback end-to-end coverage for DefaultSMTPSender rides on
// the Phase E e2e fixture — for a bare-hostname MX target it dials the
// relay-MX port 25 (binding 25 on a dev machine requires root), but a
// `host:port` target from an operator transport override
// (OverrideMXResolver) dials that port verbatim, which is how the E.3
// submission round-trip points `external.test` at a loopback stub MX on
// an ephemeral port. The worker→sender→nest contract is covered by the
// fake-MX integration tests above.

// TestOverrideMXResolver covers the static-transport-route resolver:
// mapped domains resolve to the verbatim target (case-insensitively);
// unmapped domains fall through to Fallback; an unmapped domain with no
// fallback is an error.
func TestOverrideMXResolver(t *testing.T) {
	fallback := fakeMX{hosts: map[string][]MXHost{
		"fallback.test": {{Hostname: "mx.fallback.test", Pref: 10}},
	}}
	r := OverrideMXResolver{
		Override: map[string]string{"external.test": "127.0.0.1:2526"},
		Fallback: fallback,
	}

	got, err := r.LookupMX(context.Background(), "External.Test") // case-insensitive
	if err != nil {
		t.Fatalf("override hit: %v", err)
	}
	if len(got.Hosts) != 1 || got.Hosts[0].Hostname != "127.0.0.1:2526" {
		t.Fatalf("override hit = %+v, want one MXHost{Hostname:127.0.0.1:2526}", got)
	}
	// A static route is configuration, not a DNS answer, so no spoofing
	// attacker chose the name — the override target stays DANE-eligible
	// (RFC 7672 §2.2 is about attacker-selectable names). Were this false,
	// DANE would be off for every split-horizon relay deployment.
	if !got.Secure {
		t.Fatal("override hit: want Secure — a configured static route is not an attacker-chosen name")
	}

	got, err = r.LookupMX(context.Background(), "fallback.test")
	if err != nil {
		t.Fatalf("fallback: %v", err)
	}
	if len(got.Hosts) != 1 || got.Hosts[0].Hostname != "mx.fallback.test" {
		t.Fatalf("fallback = %+v, want mx.fallback.test", got)
	}

	rNoFallback := OverrideMXResolver{Override: map[string]string{"x.test": "h"}}
	if _, err := rNoFallback.LookupMX(context.Background(), "other.test"); err == nil {
		t.Fatal("expected error for unmapped domain with nil fallback")
	}
}

// TestMXDialTarget pins the bare-host vs host:port split that lets an
// operator transport override carry a non-25 port.
func TestMXDialTarget(t *testing.T) {
	cases := []struct{ host, wantAddr, wantServer string }{
		{"mx.example.com", "mx.example.com:25", "mx.example.com"},
		{"127.0.0.1:2526", "127.0.0.1:2526", "127.0.0.1"},
		{"[::1]:587", "[::1]:587", "::1"},
	}
	for _, c := range cases {
		addr, server := mxDialTarget(c.host)
		if addr != c.wantAddr || server != c.wantServer {
			t.Errorf("mxDialTarget(%q) = (%q,%q), want (%q,%q)", c.host, addr, server, c.wantAddr, c.wantServer)
		}
	}
}

// ── OutboundPolicy bridge-side knobs ──

func TestDialNetworkGatesIPv6(t *testing.T) {
	t.Parallel()
	if got := dialNetwork(true); got != "tcp" {
		t.Errorf("ipv6_enabled=true must dial dual-stack tcp; got %q", got)
	}
	if got := dialNetwork(false); got != "tcp4" {
		t.Errorf("ipv6_enabled=false must dial IPv4-only tcp4; got %q", got)
	}
}

func TestExtractEnhancedStatus(t *testing.T) {
	t.Parallel()
	cases := []struct {
		msg  string
		want string
	}{
		{"550 5.7.1 Relay access denied", "5.7.1"},
		{"450 4.2.0 try again", "4.2.0"},
		{"250 2.1.5 ok", "2.1.5"},
		{"550 No such user here", ""}, // no enhanced triple present
		{"503 5.5.1 Bad sequence", "5.5.1"},
		{"421 service unavailable", ""},
		{"550", ""},                // bare reply code is not enhanced
		{"550 5.7 incomplete", ""}, // two-part is not a valid enhanced code
	}
	for _, c := range cases {
		if got := extractEnhancedStatus(c.msg); got != c.want {
			t.Errorf("extractEnhancedStatus(%q) = %q, want %q", c.msg, got, c.want)
		}
	}
}

func TestClassifyTreat5xxAsTransient(t *testing.T) {
	t.Parallel()
	// Mirrors fauna_mail::outbound::classifier::DefaultBouncePolicy: a 5xx
	// whose enhanced status is in the admin allowlist demotes to transient;
	// otherwise it stays permanent. 4xx is always transient; a 5xx with no
	// enhanced status defaults to "5.0.0" for the allowlist check.
	allow := []string{"5.7.1"}

	// 5xx in the allowlist → transient.
	if err := classify(errors.New("550 5.7.1 greylisted-but-mislabeled"), allow); !isTemporary(err) {
		t.Errorf("5.7.1 in allowlist must be temporary; got %T %v", err, err)
	}
	// 5xx NOT in the allowlist → permanent.
	if err := classify(errors.New("550 5.1.1 no such user"), allow); !isPermanent(err) {
		t.Errorf("5.1.1 not in allowlist must be permanent; got %T %v", err, err)
	}
	// Empty allowlist → all 5xx permanent (prior behaviour preserved).
	if err := classify(errors.New("550 5.7.1 denied"), nil); !isPermanent(err) {
		t.Errorf("empty allowlist must keep 5xx permanent; got %T %v", err, err)
	}
	// 4xx → temporary regardless of allowlist.
	if err := classify(errors.New("450 4.2.0 try later"), allow); !isTemporary(err) {
		t.Errorf("4xx must be temporary; got %T %v", err, err)
	}
	// 5xx with no enhanced code defaults to 5.0.0; allowlisting 5.0.0 demotes it.
	if err := classify(errors.New("550 no enhanced code here"), []string{"5.0.0"}); !isTemporary(err) {
		t.Errorf("5xx w/o enhanced code must match allowlisted 5.0.0 → temporary; got %T %v", err, err)
	}
	if err := classify(errors.New("550 no enhanced code here"), allow); !isPermanent(err) {
		t.Errorf("5xx w/o enhanced code, 5.0.0 not allowlisted → permanent; got %T %v", err, err)
	}
}

// isTemporary is the TemporaryError counterpart to outbound.go's isPermanent.
func isTemporary(err error) bool {
	var tmp *TemporaryError
	return errors.As(err, &tmp)
}
