package mailfauna

import (
	"errors"
	"testing"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	faunaCore "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_core"
)

// No test here runs VerifyInbound over a PARSEABLE message: past the parse it
// resolves SPF and DMARC over live DNS, so its verdicts would depend on the box
// reaching a resolver (convention 14,
// docs/goal/architecture/e2e-latency-independent-assertions.md) — and on
// Windows its wildcard-bound resolver socket raised a firewall prompt per run.
// The verdicts themselves are pinned hermetically in Rust
// (libs/fauna-mail/tests/auth_tests.rs, over `verify_inbound_with` and an
// in-process DNS responder); the wire conversion is pinned below.

// VerifyInbound on bytes that mail-auth can't parse returns an error
// that errors.Is recognizes as ErrAuthErrorUnparseable. Session.Data
// maps that sentinel to 554 5.6.0; the test pins the sentinel
// detection so the SMTP-layer mapping stays correct. Hermetic: the parse
// fails before any resolver is built.
func TestVerifyInboundUnparseableReturnsSentinel(t *testing.T) {
	// Garbage bytes — no header section, no CRLF separator. mail-auth's
	// AuthenticatedMessage::parse refuses this with None.
	_, err := VerifyInbound(
		[]byte("not an rfc 5322 message"),
		"alice@example.com",
		"192.0.2.1",
		"mail.example.com",
	)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
	if !errors.Is(err, ErrAuthErrorUnparseable) {
		t.Fatalf("expected ErrAuthErrorUnparseable, got %v", err)
	}
}

// AuthVerdictsToWire flattens the UniFFI nested-enum AuthVerdicts
// into the wsrpc wire-shape mirror. The wire shape uses adjacently-
// tagged maps; the converter must produce the right Kind string for
// each variant and populate Data only for the struct-like variants
// (DkimVerdict::Fail, DmarcVerdict::Fail).
func TestAuthVerdictsToWire_UnitVariants(t *testing.T) {
	v := AuthVerdicts{
		Dkim:  faunaCore.DkimVerdictPass{},
		Spf:   faunaCore.SpfVerdictSoftFail,
		Dmarc: faunaCore.DmarcVerdictPass{},
		Arc:   faunaCore.ArcVerdictNone,
	}
	w := AuthVerdictsToWire(v)
	if w.Dkim.Kind != "pass" || w.Dkim.Data != nil {
		t.Errorf("Dkim: %+v", w.Dkim)
	}
	if w.Spf.Kind != "soft_fail" {
		t.Errorf("Spf.Kind: got %q, want soft_fail", w.Spf.Kind)
	}
	if w.Dmarc.Kind != "pass" || w.Dmarc.Data != nil {
		t.Errorf("Dmarc: %+v", w.Dmarc)
	}
	if w.Arc.Kind != "none" {
		t.Errorf("Arc.Kind: got %q, want none", w.Arc.Kind)
	}
}

func TestAuthVerdictsToWire_StructVariants(t *testing.T) {
	v := AuthVerdicts{
		Dkim: faunaCore.DkimVerdictFail{Reason: "bad signature"},
		Spf:  faunaCore.SpfVerdictPass,
		Dmarc: faunaCore.DmarcVerdictFail{
			Policy: faunaCore.DmarcPolicyQuarantine,
		},
		Arc: faunaCore.ArcVerdictNone,
	}
	w := AuthVerdictsToWire(v)
	if w.Dkim.Kind != "fail" {
		t.Errorf("Dkim.Kind: got %q", w.Dkim.Kind)
	}
	if w.Dkim.Data == nil || w.Dkim.Data.Reason != "bad signature" {
		t.Errorf("Dkim.Data: %+v", w.Dkim.Data)
	}
	if w.Dmarc.Kind != "fail" {
		t.Errorf("Dmarc.Kind: got %q", w.Dmarc.Kind)
	}
	if w.Dmarc.Data == nil || w.Dmarc.Data.Policy != "quarantine" {
		t.Errorf("Dmarc.Data: %+v", w.Dmarc.Data)
	}
}

// CBOR encode → decode round-trip for the wire-shape AuthVerdicts.
// Pins agreement between encoder (DAG-CBOR canonical) and decoder for
// every variant family. The Rust-side mirror in
// libs/fauna-protocol/src/bridge_routing.rs has equivalent
// round-trip tests; together they pin Go↔Rust wire parity.
func TestAuthVerdictsCBORRoundTrip(t *testing.T) {
	cases := []struct {
		name string
		in   wsrpc.AuthVerdicts
	}{
		{
			name: "all-units",
			in: wsrpc.AuthVerdicts{
				Dkim:  wsrpc.DkimVerdict{Kind: "pass"},
				Spf:   wsrpc.SpfVerdict{Kind: "pass"},
				Dmarc: wsrpc.DmarcVerdict{Kind: "pass"},
				Arc:   wsrpc.ArcVerdict{Kind: "none"},
			},
		},
		{
			name: "dkim-fail-dmarc-fail",
			in: wsrpc.AuthVerdicts{
				Dkim: wsrpc.DkimVerdict{
					Kind: "fail",
					Data: &wsrpc.DkimVerdictFail{Reason: "bad sig"},
				},
				Spf: wsrpc.SpfVerdict{Kind: "fail"},
				Dmarc: wsrpc.DmarcVerdict{
					Kind: "fail",
					Data: &wsrpc.DmarcVerdictFail{Policy: "reject"},
				},
				Arc: wsrpc.ArcVerdict{Kind: "perm_error"},
			},
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			b, err := dagcbor.Marshal(tc.in)
			if err != nil {
				t.Fatalf("dagcbor.Marshal: %v", err)
			}
			var got wsrpc.AuthVerdicts
			if err := cbor.Unmarshal(b, &got); err != nil {
				t.Fatalf("cbor.Unmarshal: %v", err)
			}
			if got.Dkim.Kind != tc.in.Dkim.Kind ||
				got.Spf.Kind != tc.in.Spf.Kind ||
				got.Dmarc.Kind != tc.in.Dmarc.Kind ||
				got.Arc.Kind != tc.in.Arc.Kind {
				t.Errorf("kind drift: got=%+v in=%+v", got, tc.in)
			}
			if (got.Dkim.Data == nil) != (tc.in.Dkim.Data == nil) {
				t.Errorf("dkim Data nilness drift: got=%v in=%v",
					got.Dkim.Data, tc.in.Dkim.Data)
			}
			if got.Dkim.Data != nil && got.Dkim.Data.Reason != tc.in.Dkim.Data.Reason {
				t.Errorf("dkim reason drift: got=%q in=%q",
					got.Dkim.Data.Reason, tc.in.Dkim.Data.Reason)
			}
			if (got.Dmarc.Data == nil) != (tc.in.Dmarc.Data == nil) {
				t.Errorf("dmarc Data nilness drift: got=%v in=%v",
					got.Dmarc.Data, tc.in.Dmarc.Data)
			}
			if got.Dmarc.Data != nil && got.Dmarc.Data.Policy != tc.in.Dmarc.Data.Policy {
				t.Errorf("dmarc policy drift: got=%q in=%q",
					got.Dmarc.Data.Policy, tc.in.Dmarc.Data.Policy)
			}
		})
	}
}
