package mta

import (
	"context"
	"errors"
	"sync"
	"testing"
	"time"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// ── MTA-STS fake caller ───────────────────────────────────────────

// mtaStsCaller wraps queueCaller's outbound-queue behaviour and adds a
// scripted fauna.bridges.fetch_mta_sts_policy reply. It lets the
// MTA-STS table tests drive each RFC 8461 §5 outcome (and an RPC
// error) through deliverOne while still recording the mark_outbound_*
// reporting path.
type mtaStsCaller struct {
	queueCaller
	// stsOutcome is the `outcome` field of the scripted reply.
	stsOutcome string
	// stsPolicy is returned iff stsOutcome == "found".
	stsPolicy *wsrpc.MtaStsPolicyWire
	// stsErr, when non-nil, is returned for the fetch_mta_sts_policy
	// call (simulating an RPC transport failure).
	stsErr error
}

func (c *mtaStsCaller) Call(ctx context.Context, method string, body, reply any) error {
	if method == wsrpc.MethodFetchMtaStsPolicy {
		if c.stsErr != nil {
			return c.stsErr
		}
		// Build the reply by its public cbor field tags so we don't need
		// to reach into the unexported fetchMtaStsPolicyReply struct.
		out := struct {
			Outcome string                  `cbor:"outcome"`
			Policy  *wsrpc.MtaStsPolicyWire `cbor:"policy"`
		}{Outcome: c.stsOutcome, Policy: c.stsPolicy}
		repBytes, err := dagcbor.Marshal(out)
		if err != nil {
			return err
		}
		return cbor.Unmarshal(repBytes, reply)
	}
	return c.queueCaller.Call(ctx, method, body, reply)
}

// ── TLS-policy-recording sender ───────────────────────────────────

// stsSender records, per call, the host it was asked to send to and the
// TLSPolicy the worker chose, so the MTA-STS tests can assert the
// enforce/testing/none decision reached the wire layer.
type stsSender struct {
	mu    sync.Mutex
	calls []stsCall
}

type stsCall struct {
	Host      string
	TLSPolicy TLSPolicy
}

func (s *stsSender) Send(_ context.Context, host, _, _ string, _ []byte, tlsPolicy TLSPolicy) (TLSAttemptOutcome, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.calls = append(s.calls, stsCall{Host: host, TLSPolicy: tlsPolicy})
	// Every recorded send simulates a successful TLS session → the worker
	// reports result_type=None for it.
	return TLSAttemptOutcome{Reportable: true}, nil
}

func (s *stsSender) snapshot() []stsCall {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]stsCall(nil), s.calls...)
}

func foundPolicy(mode string, mx ...string) *wsrpc.MtaStsPolicyWire {
	return &wsrpc.MtaStsPolicyWire{ID: "policy-1", Mode: mode, Mx: mx, MaxAgeSecs: 604800}
}

func twoHostUnit() wsrpc.OutboundUnit {
	return wsrpc.OutboundUnit{
		ID:             100,
		MessageID:      "msg@example.com",
		OriginalSender: "alice@example.com",
		Recipient:      "bob@dest.test",
		RawMessage:     []byte("body\r\n"),
	}
}

// twoHostMX returns dest.test → [mx-a.dest.test, mx-b.dest.test] so the
// per-host MX-walk + enforce-skip semantics are observable.
func twoHostMX() fakeMX {
	return fakeMX{hosts: map[string][]MXHost{
		"dest.test": {
			{Hostname: "mx-a.dest.test", Pref: 10},
			{Hostname: "mx-b.dest.test", Pref: 20},
		},
	}}
}

func newStsWorker(t *testing.T, q *mtaStsCaller, mx MXResolver, sender SMTPSender) *OutboundWorker {
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

// TestMtaStsEnforceSkipsMismatchTriesNext: enforce mode where the
// first MX does not match the policy's mx: list but the second does →
// the first host is refused (no Send), the second is sent with
// TLSRequired. (smtp-server.md § MX resolution, MTA-STS enforcement.)
func TestMtaStsEnforceSkipsMismatchTriesNext(t *testing.T) {
	q := &mtaStsCaller{
		queueCaller: queueCaller{batches: [][]wsrpc.OutboundUnit{{twoHostUnit()}, nil}},
		stsOutcome:  "found",
		// Only mx-b matches; mx-a must be skipped.
		stsPolicy: foundPolicy("enforce", "mx-b.dest.test"),
	}
	sender := &stsSender{}
	w := newStsWorker(t, q, twoHostMX(), sender)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	wg := w.Start(ctx)
	waitUntil(t, func() bool {
		q.mu.Lock()
		defer q.mu.Unlock()
		return len(q.delivered) == 1
	}, "delivered")
	cancel()
	wg.Wait()

	calls := sender.snapshot()
	if len(calls) != 1 {
		t.Fatalf("sent calls = %d want 1 (mx-a skipped, mx-b sent); calls=%+v", len(calls), calls)
	}
	if calls[0].Host != "mx-b.dest.test" {
		t.Fatalf("sent to %q want mx-b.dest.test (mx-a should be enforce-skipped)", calls[0].Host)
	}
	if calls[0].TLSPolicy.Mode != TLSRequired {
		t.Fatalf("tls policy = %v want TLSRequired", calls[0].TLSPolicy)
	}
}

// TestMtaStsEnforceAllMismatchReportsFailedNotBounced: enforce mode
// where NO host matches → the sender is never called and the unit is
// reported mark_outbound_failed (temporary, so nest reschedules), never
// bounced (refusal is per-host, not a permanent verdict —
// smtp-server.md:452).
func TestMtaStsEnforceAllMismatchReportsFailedNotBounced(t *testing.T) {
	q := &mtaStsCaller{
		queueCaller: queueCaller{batches: [][]wsrpc.OutboundUnit{{twoHostUnit()}, nil}},
		stsOutcome:  "found",
		stsPolicy:   foundPolicy("enforce", "other.test"), // matches neither MX
	}
	sender := &stsSender{}
	w := newStsWorker(t, q, twoHostMX(), sender)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	wg := w.Start(ctx)
	waitUntil(t, func() bool {
		q.mu.Lock()
		defer q.mu.Unlock()
		return len(q.failed) == 1
	}, "failed")
	cancel()
	wg.Wait()

	if len(sender.snapshot()) != 0 {
		t.Fatalf("sender called %d times; want 0 (all hosts enforce-skipped)", len(sender.snapshot()))
	}
	if len(q.failed) != 1 || q.failed[0].ID != 100 {
		t.Fatalf("failed=%+v want one entry id=100", q.failed)
	}
	if len(q.bounced) != 0 || len(q.delivered) != 0 {
		t.Fatalf("unexpected bounced=%+v delivered=%+v (all-mismatch is temporary, never a bounce)", q.bounced, q.delivered)
	}
}

// TestMtaStsEnforceMatchSendsTLSRequired: enforce + first host matches →
// sent immediately with TLSRequired.
func TestMtaStsEnforceMatchSendsTLSRequired(t *testing.T) {
	q := &mtaStsCaller{
		queueCaller: queueCaller{batches: [][]wsrpc.OutboundUnit{{twoHostUnit()}, nil}},
		stsOutcome:  "found",
		stsPolicy:   foundPolicy("enforce", "*.dest.test"), // wildcard matches both
	}
	sender := &stsSender{}
	w := newStsWorker(t, q, twoHostMX(), sender)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	wg := w.Start(ctx)
	waitUntil(t, func() bool {
		q.mu.Lock()
		defer q.mu.Unlock()
		return len(q.delivered) == 1
	}, "delivered")
	cancel()
	wg.Wait()

	calls := sender.snapshot()
	if len(calls) != 1 || calls[0].Host != "mx-a.dest.test" {
		t.Fatalf("calls=%+v want single send to mx-a.dest.test", calls)
	}
	if calls[0].TLSPolicy.Mode != TLSRequired {
		t.Fatalf("tls policy = %v want TLSRequired", calls[0].TLSPolicy)
	}
}

// TestMtaStsTestingMismatchSendsOpportunistic: testing mode +
// mismatch → log/record but proceed to the first host opportunistically
// (smtp-server.md:453).
func TestMtaStsTestingMismatchSendsOpportunistic(t *testing.T) {
	q := &mtaStsCaller{
		queueCaller: queueCaller{batches: [][]wsrpc.OutboundUnit{{twoHostUnit()}, nil}},
		stsOutcome:  "found",
		stsPolicy:   foundPolicy("testing", "other.test"), // matches neither
	}
	sender := &stsSender{}
	w := newStsWorker(t, q, twoHostMX(), sender)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	wg := w.Start(ctx)
	waitUntil(t, func() bool {
		q.mu.Lock()
		defer q.mu.Unlock()
		return len(q.delivered) == 1
	}, "delivered")
	cancel()
	wg.Wait()

	calls := sender.snapshot()
	if len(calls) != 1 || calls[0].Host != "mx-a.dest.test" {
		t.Fatalf("calls=%+v want single send to mx-a.dest.test (testing proceeds)", calls)
	}
	if calls[0].TLSPolicy.Mode != TLSOpportunistic {
		t.Fatalf("tls policy = %v want TLSOpportunistic (testing never requires TLS)", calls[0].TLSPolicy)
	}
}

// TestMtaStsNoPolicyOutcomesSendOpportunistic: not_published /
// fetch_error / invalid → no enforcement, opportunistic TLS, delivery
// proceeds (RFC 8461 §5: a published-but-broken policy never forces
// plaintext fallback or refusal — smtp-server.md:455).
func TestMtaStsNoPolicyOutcomesSendOpportunistic(t *testing.T) {
	for _, outcome := range []string{"not_published", "fetch_error", "invalid"} {
		t.Run(outcome, func(t *testing.T) {
			q := &mtaStsCaller{
				queueCaller: queueCaller{batches: [][]wsrpc.OutboundUnit{{twoHostUnit()}, nil}},
				stsOutcome:  outcome,
				stsPolicy:   nil,
			}
			sender := &stsSender{}
			w := newStsWorker(t, q, twoHostMX(), sender)
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			wg := w.Start(ctx)
			waitUntil(t, func() bool {
				q.mu.Lock()
				defer q.mu.Unlock()
				return len(q.delivered) == 1
			}, "delivered")
			cancel()
			wg.Wait()

			calls := sender.snapshot()
			if len(calls) != 1 || calls[0].Host != "mx-a.dest.test" {
				t.Fatalf("calls=%+v want single send to mx-a.dest.test", calls)
			}
			if calls[0].TLSPolicy.Mode != TLSOpportunistic {
				t.Fatalf("tls policy = %v want TLSOpportunistic for outcome %q", calls[0].TLSPolicy, outcome)
			}
		})
	}
}

// TestMtaStsRPCErrorSendsOpportunistic: an MTA-STS fetch RPC failure must
// NOT block delivery — the worker logs and proceeds opportunistically
// for every host (an MTA-STS fetch failure is not a delivery failure).
func TestMtaStsRPCErrorSendsOpportunistic(t *testing.T) {
	q := &mtaStsCaller{
		queueCaller: queueCaller{batches: [][]wsrpc.OutboundUnit{{twoHostUnit()}, nil}},
		stsErr:      errors.New("nest unreachable"),
	}
	sender := &stsSender{}
	w := newStsWorker(t, q, twoHostMX(), sender)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	wg := w.Start(ctx)
	waitUntil(t, func() bool {
		q.mu.Lock()
		defer q.mu.Unlock()
		return len(q.delivered) == 1
	}, "delivered")
	cancel()
	wg.Wait()

	calls := sender.snapshot()
	if len(calls) != 1 || calls[0].Host != "mx-a.dest.test" {
		t.Fatalf("calls=%+v want single send to mx-a.dest.test (RPC error must not block delivery)", calls)
	}
	if calls[0].TLSPolicy.Mode != TLSOpportunistic {
		t.Fatalf("tls policy = %v want TLSOpportunistic on RPC error", calls[0].TLSPolicy)
	}
}
